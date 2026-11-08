use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tantivy::directory::{Directory, MmapDirectory};

use crate::{
    DocumentFormat, DocumentRef, Error, Evidence, LineRange, Result, source::SourceObject, storage,
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorpusMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<SourceLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parser: Option<ParserInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLocation {
    pub range: LineRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[u16; 4]>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ParserInfo {
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorpusDocument {
    pub evidence: Evidence,
    pub content: String,
    pub format: DocumentFormat,
    pub parsed_version: String,
    #[serde(flatten)]
    pub metadata: CorpusMetadata,
}

pub(crate) struct Corpus(MmapDirectory);

impl Corpus {
    pub fn open(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path)
            .map_err(|_| Error::Io("Could not create the corpus directory"))?;
        Ok(Self(MmapDirectory::open(path).map_err(|_| {
            Error::Io("Could not open the corpus directory")
        })?))
    }

    pub fn save(&self, object: &SourceObject, end_line: usize) -> Result<()> {
        let source = &object.source;
        let path = record_path(&source.source_id, &source.object_id, &source.version);
        if self
            .0
            .exists(&path)
            .map_err(|_| Error::Io("Could not check the corpus revision"))?
        {
            return Ok(());
        }
        let record = CorpusDocument {
            evidence: object.evidence(LineRange {
                start_line: 1,
                end_line,
            }),
            content: object.content.clone(),
            format: object.format(),
            parsed_version: object.parsed_version.clone(),
            metadata: object.metadata.clone(),
        };
        storage::write_json(&self.0, &path, &record)
            .map_err(|_| Error::Io("Could not save the corpus revision"))
    }

    pub fn get(&self, document: &DocumentRef, version: &str) -> Result<Option<CorpusDocument>> {
        let path = record_path(&document.source_id, &document.object_id, version);
        storage::read_json(&self.0, &path)
            .map_err(|_| Error::Io("Could not read the corpus revision"))
    }
}

fn record_path(source_id: &str, object_id: &str, version: &str) -> PathBuf {
    let mut key = blake3::Hasher::new();
    key.update(source_id.as_bytes())
        .update(b"\0")
        .update(object_id.as_bytes())
        .update(b"\0")
        .update(version.as_bytes());
    PathBuf::from(format!("{}.json", key.finalize().to_hex()))
}
