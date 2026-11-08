use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tantivy::directory::error::{LockError, OpenDirectoryError};
use tantivy::directory::{Directory, Lock, MmapDirectory};

use crate::engine::{KeywordIndex, KeywordSnapshot};
use crate::storage::{read_json, write_json};
use crate::{Cancellation, DocumentRef, Error, Evidence, LineRange, Result, SourceRef, source};

const SCHEMA_VERSION: u32 = 1;
const EXCERPT_LINES: usize = 12;
const EXCERPT_BYTES: usize = 4096;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiBuildOptions {
    pub id: String,
    pub title: String,
    pub documents: Vec<DocumentRef>,
}

impl WikiBuildOptions {
    fn validate(&self) -> Result<()> {
        validate_id(&self.id)?;
        if self.title.trim().is_empty()
            || self.title.len() > 512
            || self.title.chars().any(char::is_control)
        {
            return Err(Error::Invalid(
                "Wiki title must be a nonempty line of at most 512 UTF-8 bytes",
            ));
        }
        if !(2..=32).contains(&self.documents.len()) {
            return Err(Error::Invalid(
                "A Wiki page requires 2–32 distinct documents",
            ));
        }
        let mut seen = HashSet::new();
        for document in &self.documents {
            source::validate_id(&document.source_id)?;
            if document.object_id.is_empty()
                || document.object_id.len() > 128 * 1024
                || document.object_id.contains('\0')
                || !seen.insert(document)
            {
                return Err(Error::Invalid(
                    "Wiki documents must have valid object IDs and be distinct",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiPage {
    pub id: String,
    pub title: String,
    pub revision: u32,
    pub generated_at_ms: u64,
    pub status: WikiStatus,
    pub markdown: String,
    pub citations: Vec<WikiCitation>,
    pub changes: Vec<WikiChange>,
    pub unavailable: Vec<WikiUnavailable>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WikiStatus {
    Current,
    Stale,
    InsufficientEvidence,
}

#[derive(Debug, Serialize)]
pub struct WikiCitation {
    pub number: usize,
    pub evidence: Evidence,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct WikiChange {
    pub document: DocumentRef,
    pub reason: WikiChangeReason,
    pub previous: Option<SourceRef>,
    pub current: Option<SourceRef>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WikiChangeReason {
    Updated,
    Deleted,
    Restored,
}

#[derive(Debug, Serialize)]
pub struct WikiUnavailable {
    pub document: DocumentRef,
    pub reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct WikiImpact {
    pub id: String,
    pub title: String,
    pub revision: u32,
    pub changes: Vec<WikiChange>,
}

pub(crate) struct WikiStore {
    root: PathBuf,
}

impl WikiStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn build(
        &self,
        index: &KeywordIndex,
        options: WikiBuildOptions,
        cancel: &Cancellation,
    ) -> Result<WikiPage> {
        options.validate()?;
        let id = options.id.clone();
        self.replace(index, &id, Some(options), None, cancel)?
            .ok_or(Error::Wiki)
    }

    pub fn regenerate(
        &self,
        index: &KeywordIndex,
        id: &str,
        cancel: &Cancellation,
    ) -> Result<WikiPage> {
        validate_id(id)?;
        self.replace(index, id, None, None, cancel)?
            .ok_or(Error::Wiki)
    }

    pub fn regenerate_affected(
        &self,
        index: &KeywordIndex,
        id: &str,
        source_id: &str,
        cancel: &Cancellation,
    ) -> Result<Option<WikiPage>> {
        self.replace(index, id, None, Some(source_id), cancel)
    }

    pub fn get(
        &self,
        snapshot: &KeywordSnapshot<'_>,
        id: &str,
        cancel: &Cancellation,
    ) -> Result<Option<WikiPage>> {
        validate_id(id)?;
        cancel.check()?;
        let Some(directory) = self.open(false)? else {
            return Ok(None);
        };
        read_page(&directory, &page_path(id))?
            .map(|page| page.view(snapshot, cancel))
            .transpose()
    }

    pub fn affected(
        &self,
        snapshot: &KeywordSnapshot<'_>,
        source_id: Option<&str>,
        cancel: &Cancellation,
    ) -> Result<Vec<WikiImpact>> {
        if let Some(id) = source_id {
            source::validate_id(id)?;
        }
        cancel.check()?;
        let Some(directory) = self.open(false)? else {
            return Ok(Vec::new());
        };
        let mut impacts = Vec::new();
        let mut versions = HashMap::new();
        for entry in fs::read_dir(&self.root).map_err(|_| Error::Wiki)? {
            cancel.check()?;
            let entry = entry.map_err(|_| Error::Wiki)?;
            let name = entry.file_name();
            let path = Path::new(&name);
            if !is_page_file(path) || !entry.file_type().map_err(|_| Error::Wiki)?.is_file() {
                continue;
            }
            let Some(page) = read_page(&directory, path)? else {
                continue;
            };
            let mut changes = Vec::new();
            for dependency in &page.documents {
                cancel.check()?;
                let document = &dependency.document;
                if source_id.is_some_and(|id| id != document.source_id) {
                    continue;
                }
                let current = match versions.entry(document.clone()) {
                    std::collections::hash_map::Entry::Occupied(value) => value.into_mut(),
                    std::collections::hash_map::Entry::Vacant(value) => {
                        value.insert(snapshot.object_ref(&document.source_id, &document.object_id)?)
                    }
                };
                if let Some(change) = dependency.change(current.as_ref()) {
                    changes.push(change);
                }
            }
            if !changes.is_empty() {
                impacts.push(WikiImpact {
                    id: page.id,
                    title: page.title,
                    revision: page.revision,
                    changes,
                });
            }
        }
        impacts.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(impacts)
    }

    fn open(&self, create: bool) -> Result<Option<MmapDirectory>> {
        if create {
            fs::create_dir_all(&self.root).map_err(|_| Error::Wiki)?;
        }
        match MmapDirectory::open(&self.root) {
            Ok(directory) => Ok(Some(directory)),
            Err(OpenDirectoryError::DoesNotExist(_)) if !create => Ok(None),
            Err(_) => Err(Error::Wiki),
        }
    }

    fn replace(
        &self,
        index: &KeywordIndex,
        id: &str,
        options: Option<WikiBuildOptions>,
        affected_source: Option<&str>,
        cancel: &Cancellation,
    ) -> Result<Option<WikiPage>> {
        cancel.check()?;
        let directory = self.open(options.is_some())?.ok_or(Error::WikiNotFound)?;
        let path = page_path(id);
        let _lock = directory
            .acquire_lock(&Lock {
                filepath: path.with_extension("lock"),
                is_blocking: false,
            })
            .map_err(|error| match error {
                LockError::LockBusy => Error::Busy,
                _ => Error::Wiki,
            })?;
        let previous = read_page(&directory, &path)?;
        let snapshot = index.snapshot();
        if let Some(source_id) = affected_source {
            let page = previous.as_ref().ok_or(Error::WikiNotFound)?;
            if !page.is_affected_by(&snapshot, source_id, cancel)? {
                return Ok(None);
            }
        }
        let revision = previous
            .as_ref()
            .map_or(0, |page| page.revision)
            .checked_add(1)
            .ok_or(Error::Wiki)?;
        let (title, documents) = match options {
            Some(options) => (options.title, options.documents),
            None => {
                let page = previous.ok_or(Error::WikiNotFound)?;
                (
                    page.title,
                    page.documents
                        .into_iter()
                        .map(|record| record.document)
                        .collect(),
                )
            }
        };
        let mut records = Vec::with_capacity(documents.len());
        for document in documents {
            cancel.check()?;
            let reference = match snapshot.object(&document.source_id, &document.object_id)? {
                Some(object) => match excerpt_range(&object.content) {
                    Some(range) => RecordedSource::Available(Evidence {
                        range,
                        ..object.evidence
                    }),
                    None => RecordedSource::Empty(object.evidence.source),
                },
                None => RecordedSource::Missing,
            };
            records.push(Dependency {
                document,
                reference,
            });
        }
        let page = StoredPage {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            title,
            revision,
            generated_at_ms: source::epoch_ms(SystemTime::now()).unwrap_or_default(),
            documents: records,
        };
        let view = page.view(&snapshot, cancel)?;
        cancel.check()?;
        write_json(&directory, &path, &page).map_err(|_| Error::Wiki)?;
        Ok(Some(view))
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredPage {
    schema_version: u32,
    id: String,
    title: String,
    revision: u32,
    generated_at_ms: u64,
    documents: Vec<Dependency>,
}

#[derive(Deserialize, Serialize)]
struct Dependency {
    document: DocumentRef,
    reference: RecordedSource,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "state", content = "reference", rename_all = "snake_case")]
enum RecordedSource {
    Missing,
    Empty(SourceRef),
    Available(Evidence),
}

impl Dependency {
    fn change(&self, current: Option<&SourceRef>) -> Option<WikiChange> {
        let previous = match &self.reference {
            RecordedSource::Missing => None,
            RecordedSource::Empty(source) => Some(source),
            RecordedSource::Available(evidence) => Some(&evidence.source),
        };
        let reason = match (previous, current) {
            (Some(old), Some(new)) if old.version != new.version || old.url != new.url => {
                WikiChangeReason::Updated
            }
            (Some(_), None) => WikiChangeReason::Deleted,
            (None, Some(_)) => WikiChangeReason::Restored,
            _ => return None,
        };
        Some(WikiChange {
            document: self.document.clone(),
            reason,
            previous: previous.cloned(),
            current: current.cloned(),
        })
    }
}

impl StoredPage {
    fn is_affected_by(
        &self,
        snapshot: &KeywordSnapshot<'_>,
        source_id: &str,
        cancel: &Cancellation,
    ) -> Result<bool> {
        for dependency in self
            .documents
            .iter()
            .filter(|dep| dep.document.source_id == source_id)
        {
            cancel.check()?;
            let document = &dependency.document;
            let current = snapshot.object_ref(source_id, &document.object_id)?;
            if dependency.change(current.as_ref()).is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn view(&self, snapshot: &KeywordSnapshot<'_>, cancel: &Cancellation) -> Result<WikiPage> {
        let mut citations = Vec::new();
        let mut changes = Vec::new();
        let mut unavailable = Vec::new();
        let mut sections = String::new();
        for (index, dependency) in self.documents.iter().enumerate() {
            cancel.check()?;
            let document = &dependency.document;
            let current = snapshot.object(&document.source_id, &document.object_id)?;
            if let Some(change) =
                dependency.change(current.as_ref().map(|object| &object.evidence.source))
            {
                changes.push(change);
                continue;
            }
            let reason = match (&dependency.reference, current) {
                (RecordedSource::Available(evidence), Some(object)) => {
                    let range = evidence.range;
                    if range.start_line == 0
                        || range.end_line < range.start_line
                        || range.end_line > object.evidence.range.end_line
                    {
                        return Err(Error::Wiki);
                    }
                    let mut excerpt = source::select_lines(&object.content, range);
                    let truncated = excerpt.len() > EXCERPT_BYTES;
                    excerpt.truncate(excerpt.floor_char_boundary(EXCERPT_BYTES));
                    let number = index + 1;
                    append_excerpt(&mut sections, number, evidence, &excerpt, truncated);
                    citations.push(WikiCitation {
                        number,
                        evidence: evidence.clone(),
                        truncated,
                    });
                    continue;
                }
                (RecordedSource::Missing, None) => "missing",
                (RecordedSource::Empty(_), Some(_)) => "empty",
                _ => return Err(Error::Wiki),
            };
            unavailable.push(WikiUnavailable {
                document: document.clone(),
                reason,
            });
        }
        cancel.check()?;
        let (status, notice) = if !changes.is_empty() {
            (
                WikiStatus::Stale,
                "> Some source documents have changed. Affected excerpts are omitted.\n\n",
            )
        } else if !unavailable.is_empty() {
            (
                WikiStatus::InsufficientEvidence,
                "> Some source documents have no available evidence.\n\n",
            )
        } else {
            (WikiStatus::Current, "")
        };
        let markdown = format!("# {}\n\n{notice}{sections}", markdown_text(&self.title));
        Ok(WikiPage {
            id: self.id.clone(),
            title: self.title.clone(),
            revision: self.revision,
            generated_at_ms: self.generated_at_ms,
            status,
            markdown,
            citations,
            changes,
            unavailable,
        })
    }
}

fn excerpt_range(content: &str) -> Option<LineRange> {
    let mut lines = content.split_inclusive('\n').enumerate();
    let (start, first) = lines.find(|(_, line)| !line.trim().is_empty())?;
    let mut end = start;
    let mut bytes = first.len();
    for (number, line) in lines.take(EXCERPT_LINES - 1) {
        if bytes + line.len() > EXCERPT_BYTES {
            break;
        }
        bytes += line.len();
        end = number;
    }
    Some(LineRange {
        start_line: start + 1,
        end_line: end + 1,
    })
}

fn append_excerpt(
    markdown: &mut String,
    number: usize,
    evidence: &Evidence,
    excerpt: &str,
    truncated: bool,
) {
    let source = &evidence.source;
    let label = markdown_text(&format!("{}/{}", source.source_id, source.object_id));
    let url = markdown_text(&source.url);
    let version = markdown_text(&source.version);
    let range = evidence.range;
    markdown.push_str(&format!(
        "## [{number}] {label}\n\nSource {url}\n\nVersion {version}, lines {}–{}\n\n",
        range.start_line, range.end_line
    ));
    // A fence longer than any run in the excerpt keeps source markup literal.
    let longest = excerpt
        .as_bytes()
        .split(|&byte| byte != b'~')
        .map(<[u8]>::len)
        .max()
        .unwrap_or_default();
    let fence = "~".repeat((longest + 1).max(3));
    markdown.push_str(&format!("{fence}text\n"));
    markdown.push_str(excerpt);
    if !excerpt.ends_with('\n') {
        markdown.push('\n');
    }
    markdown.push_str(&format!("{fence}\n\n"));
    if truncated {
        markdown.push_str("Excerpt shortened within the last cited line.\n\n");
    }
}

fn markdown_text(text: &str) -> String {
    let mut escaped = String::new();
    for ch in text.chars() {
        if ch.is_control() {
            escaped.push(' ');
        } else {
            if ch.is_ascii_punctuation() {
                escaped.push('\\');
            }
            escaped.push(ch);
        }
    }
    escaped
}

fn validate_id(id: &str) -> Result<()> {
    if id.len() > 128
        || id
            .split('/')
            .any(|part| matches!(part, "" | "." | "..") || source::validate_id(part).is_err())
    {
        return Err(Error::Invalid(
            "Wiki ID must have slash-separated ASCII names of at most 128 bytes total",
        ));
    }
    Ok(())
}

fn page_path(id: &str) -> PathBuf {
    format!("page-{}.json", blake3::hash(id.as_bytes()).to_hex()).into()
}

fn is_page_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.strip_prefix("page-")
                .and_then(|name| name.strip_suffix(".json"))
                .is_some_and(|hash| {
                    hash.len() == 64
                        && hash
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
        })
}

fn read_page(directory: &MmapDirectory, path: &Path) -> Result<Option<StoredPage>> {
    let page: Option<StoredPage> = read_json(directory, path).map_err(|_| Error::Wiki)?;
    if page.as_ref().is_some_and(|page| {
        page.schema_version != SCHEMA_VERSION || page.revision == 0 || page_path(&page.id) != path
    }) {
        return Err(Error::Wiki);
    }
    Ok(page)
}
