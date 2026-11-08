use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use url::Url;
use walkdir::{DirEntry, WalkDir};

use crate::corpus::CorpusMetadata;
use crate::{
    Cancellation, DocumentFormat, Error, Evidence, LineRange, Result, SourceDocument, SourceRef,
    SyncOutcome,
};

const MAX_BYTES: u64 = 1024 * 1024;
pub(crate) const WINDOW_LINES: usize = 32;

pub(crate) enum SourceInput<'a> {
    Filesystem(&'a Path),
    Documents(Vec<DocumentInput>),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DocumentInput {
    #[serde(flatten)]
    document: SourceDocument,
    #[serde(default)]
    corpus: CorpusMetadata,
    modified_at_ms: Option<u64>,
}

impl From<SourceDocument> for DocumentInput {
    fn from(document: SourceDocument) -> Self {
        Self {
            document,
            corpus: CorpusMetadata::default(),
            modified_at_ms: None,
        }
    }
}

impl SourceInput<'_> {
    pub(crate) fn ingest(
        self,
        id: &str,
        cancel: &Cancellation,
        add: impl FnMut(SourceObject) -> Result<usize>,
    ) -> Result<SyncOutcome> {
        match self {
            Self::Filesystem(path) => ingest_files(id, path, cancel, add),
            Self::Documents(documents) => ingest_documents(id, documents, cancel, add),
        }
    }
}

pub(crate) struct SourceObject {
    pub source: SourceRef,
    pub acquired_at_ms: u64,
    pub content: String,
    pub metadata: CorpusMetadata,
    pub parsed_version: String,
    markdown: bool,
}

impl SourceObject {
    fn new(id: &str, input: DocumentInput) -> Result<Self> {
        let DocumentInput {
            document,
            corpus: mut metadata,
            modified_at_ms,
        } = input;
        let SourceDocument {
            object_id,
            url,
            format,
            content,
        } = document;
        let original = metadata.original_sha256.take();
        let revision = serde_json::to_vec(&(format, &metadata))
            .map_err(|_| Error::Invalid("Invalid corpus metadata"))?;
        let parsed_version = blake3::Hasher::new_derive_key("Dunlin parsed revision v1")
            .update(blake3::hash(&revision).as_bytes())
            .update(content.as_bytes())
            .finalize()
            .to_hex()
            .to_string();
        let binding = serde_json::to_vec(&(&parsed_version, &url, &original))
            .map_err(|_| Error::Invalid("Invalid corpus metadata"))?;
        let version = blake3::hash(&binding).to_hex().to_string();
        metadata.original_sha256 = original;
        Ok(Self {
            source: SourceRef {
                source_id: id.to_owned(),
                object_id,
                url,
                version,
                modified_at_ms,
            },
            acquired_at_ms: epoch_ms(SystemTime::now()).unwrap_or_default(),
            content,
            metadata,
            parsed_version,
            markdown: matches!(format, DocumentFormat::Markdown),
        })
    }

    pub fn format(&self) -> DocumentFormat {
        if self.markdown {
            DocumentFormat::Markdown
        } else {
            DocumentFormat::Text
        }
    }

    pub fn units(&self) -> impl Iterator<Item = (&str, &str, LineRange)> {
        let mut lines = self.content.split_inclusive('\n').peekable();
        let mut line_number = 1;
        let mut offset = 0;
        let mut heading = "";
        std::iter::from_fn(move || {
            let start_line = line_number;
            let start_offset = offset;
            for index in 0..WINDOW_LINES {
                let Some(&line) = lines.peek() else { break };
                if self.markdown
                    && let Some(next_heading) = markdown_heading(line)
                {
                    if index > 0 {
                        break;
                    }
                    heading = next_heading;
                }
                lines.next();
                offset += line.len();
                line_number += 1;
            }
            (offset > start_offset).then(|| {
                (
                    heading,
                    &self.content[start_offset..offset],
                    LineRange {
                        start_line,
                        end_line: line_number - 1,
                    },
                )
            })
        })
    }

    pub fn evidence(&self, range: LineRange) -> Evidence {
        Evidence {
            source: self.source.clone(),
            acquired_at_ms: self.acquired_at_ms,
            range,
        }
    }
}

pub(crate) fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    {
        return Err(Error::Invalid(
            "Source ID must contain 1–64 ASCII letters, digits, dots, underscores, or hyphens",
        ));
    }
    Ok(())
}

pub(crate) fn validate_scope(ids: &[String]) -> Result<()> {
    if ids.len() > 100 {
        return Err(Error::Invalid("Select at most 100 source IDs"));
    }
    for id in ids {
        validate_id(id)?;
    }
    Ok(())
}

pub(crate) fn in_scope(source_id: &str, ids: &[String]) -> bool {
    ids.is_empty() || ids.iter().any(|id| id == source_id)
}

pub(crate) fn object_id(path: &Path) -> Result<String> {
    Ok(path
        .components()
        .map(|part| {
            part.as_os_str()
                .to_str()
                .ok_or(Error::Invalid("File names must be Unicode"))
        })
        .collect::<Result<Vec<_>>>()?
        .join("/"))
}

fn ingest_documents(
    id: &str,
    documents: Vec<DocumentInput>,
    cancel: &Cancellation,
    mut add: impl FnMut(SourceObject) -> Result<usize>,
) -> Result<SyncOutcome> {
    let mut ids = HashSet::new();
    let mut report = SyncOutcome::default();
    for input in documents {
        cancel.check()?;
        let document = &input.document;
        if document.object_id.is_empty() || document.object_id.contains('\0') {
            return Err(Error::Invalid(
                "Expected a nonempty document ID without NUL",
            ));
        }
        if !ids.insert(document.object_id.clone()) {
            return Err(Error::Invalid(
                "Document IDs must be unique within a source",
            ));
        }
        if document
            .url
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
            || Url::parse(&document.url).is_err()
        {
            return Err(Error::Invalid("Expected an absolute document URL"));
        }
        if document.content.len() as u64 > MAX_BYTES {
            return Err(Error::Invalid("A source document exceeds 1 MiB"));
        }
        let line_count = document.content.split_inclusive('\n').count();
        if input.corpus.locations.iter().any(|location| {
            let range = location.range;
            range.start_line == 0
                || range.end_line < range.start_line
                || range.end_line > line_count
                || location.page == Some(0)
                || location
                    .bbox
                    .is_some_and(|bbox| bbox.iter().any(|value| *value > 1000))
        }) {
            return Err(Error::Invalid("Invalid corpus location"));
        }
        if input.corpus.original_sha256.as_ref().is_some_and(|hash| {
            hash.len() != 64
                || !hash
                    .bytes()
                    .all(|value| matches!(value, b'0'..=b'9' | b'a'..=b'f'))
        }) {
            return Err(Error::Invalid("Invalid original SHA-256"));
        }
        report.units += add(SourceObject::new(id, input)?)?;
        report.files += 1;
    }
    Ok(report)
}

fn ingest_files(
    id: &str,
    path: &Path,
    cancel: &Cancellation,
    mut add: impl FnMut(SourceObject) -> Result<usize>,
) -> Result<SyncOutcome> {
    let root = dunce::canonicalize(path).map_err(|_| Error::Io("Could not open source path"))?;
    let single_file = root.is_file();
    if (single_file && !supported(&root)) || (!single_file && !root.is_dir()) {
        return Err(Error::Invalid(
            "Source must be a directory or supported UTF-8 file",
        ));
    }
    let relative_to = if single_file {
        root.parent().ok_or(Error::Invalid("Invalid file path"))?
    } else {
        &root
    };
    let mut report = SyncOutcome::default();
    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(visible)
    {
        cancel.check()?;
        let entry = entry.map_err(|_| Error::Io("Could not list source directory"))?;
        let path = entry.path();
        if !entry.file_type().is_file() || !supported(path) {
            continue;
        }
        let object_id = object_id(
            path.strip_prefix(relative_to)
                .map_err(|_| Error::Invalid("Invalid relative file path"))?,
        )?;
        let file = File::open(path).map_err(|_| Error::Io("Could not open source file"))?;
        let before = file
            .metadata()
            .map_err(|_| Error::Io("Could not stat source file"))?;
        let mut content = String::new();
        (&file)
            .take(MAX_BYTES + 1)
            .read_to_string(&mut content)
            .map_err(|_| Error::Io("Source files must be readable UTF-8"))?;
        if content.len() as u64 > MAX_BYTES {
            return Err(Error::Invalid("A source file exceeds 1 MiB"));
        }
        let after = fs::metadata(path).map_err(|_| Error::Io("Could not stat source file"))?;
        if before.modified().ok() != after.modified().ok() || before.len() != after.len() {
            return Err(Error::Io(
                "A source file changed while it was being indexed",
            ));
        }
        let url = Url::from_file_path(path).map_err(|_| Error::Invalid("Invalid file URL"))?;
        let format = filesystem_format(path);
        let document = SourceDocument {
            object_id,
            url: url.into(),
            format,
            content,
        };
        report.units += add(SourceObject::new(
            id,
            DocumentInput {
                modified_at_ms: before.modified().ok().and_then(epoch_ms),
                ..document.into()
            },
        )?)?;
        report.files += 1;
    }
    Ok(report)
}

pub(crate) fn filesystem_format(path: &Path) -> DocumentFormat {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
    {
        DocumentFormat::Markdown
    } else {
        DocumentFormat::Text
    }
}

pub(crate) fn epoch_ms(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}

pub(crate) fn select_lines(content: &str, range: LineRange) -> String {
    content
        .split_inclusive('\n')
        .skip(range.start_line - 1)
        .take(range.end_line - range.start_line + 1)
        .collect()
}

fn visible(entry: &DirEntry) -> bool {
    entry.depth() == 0
        || (!entry.file_name().to_string_lossy().starts_with('.')
            && entry.file_name() != "node_modules")
}

fn supported(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|part| part.to_str()) else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "md" | "txt"
            | "rs"
            | "js"
            | "mjs"
            | "cjs"
            | "ts"
            | "tsx"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "cs"
            | "json"
            | "toml"
            | "yaml"
            | "yml"
            | "sql"
    )
}

fn markdown_heading(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let rest = line.trim_start_matches('#');
    let hashes = line.len() - rest.len();
    (1..=6).contains(&hashes).then_some(())?;
    rest.strip_prefix(' ').map(str::trim)
}
