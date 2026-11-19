use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::RetrievalMethod;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Io(&'static str),
    Busy,
    Aborted,
    Timeout,
    NotFound,
    Stale,
    Index,
    PlanNotFound,
    Plan,
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "INVALID_ARGUMENT",
            Self::Io(_) => "IO_ERROR",
            Self::Busy => "BUSY",
            Self::Aborted => "ABORTED",
            Self::Timeout => "TIMEOUT",
            Self::NotFound => "NOT_FOUND",
            Self::Stale => "STALE_EVIDENCE",
            Self::Index => "INDEX_ERROR",
            Self::PlanNotFound => "PLAN_NOT_FOUND",
            Self::Plan => "PLAN_ERROR",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid(message) | Self::Io(message) => message,
            Self::Busy => "Another operation is updating the knowledge base",
            Self::Aborted => "Operation aborted",
            Self::Timeout => "Search deadline exceeded",
            Self::NotFound => "Source object is not in the current index",
            Self::Stale => "Source object changed since this evidence was indexed",
            Self::Index => "Could not open, update, or read the index",
            Self::PlanNotFound => "Retrieval plan was not found",
            Self::Plan => "Could not load or save the retrieval plan",
        })
    }
}

impl std::error::Error for Error {}

impl From<tantivy::TantivyError> for Error {
    fn from(error: tantivy::TantivyError) -> Self {
        match error {
            tantivy::TantivyError::LockFailure(..) => Self::Busy,
            _ => Self::Index,
        }
    }
}

#[derive(Clone, Default)]
pub struct Cancellation {
    aborted: Arc<AtomicBool>,
    deadline: Option<Instant>,
}

impl Cancellation {
    pub fn with_deadline(mut self, deadline: Option<Instant>) -> Self {
        self.deadline = self.deadline.into_iter().chain(deadline).min();
        self
    }

    pub fn abort(&self) {
        self.aborted.store(true, Ordering::Relaxed);
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.aborted.load(Ordering::Relaxed) {
            return Err(Error::Aborted);
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(Error::Timeout);
        }
        Ok(())
    }
}

/// Text obtained and decoded by the caller before indexing.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDocument {
    pub object_id: String,
    pub url: String,
    pub format: DocumentFormat,
    pub content: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentFormat {
    Text,
    Markdown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    pub source_id: String,
    pub object_id: String,
    pub url: String,
    pub version: String,
    pub modified_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRef {
    pub source_id: String,
    pub object_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineRange {
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub source: SourceRef,
    pub range: LineRange,
    pub acquired_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_range: Option<ByteRange>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub title: String,
    pub score: f32,
    pub evidence: Evidence,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchOutcome {
    pub hits: Vec<SearchHit>,
    pub total_hits: usize,
    pub status: &'static str,
    pub engine: &'static str,
    pub method: RetrievalMethod,
    pub plan_id: String,
    pub elapsed_ms: f64,
}

#[derive(Debug, Serialize)]
pub struct ReadOutcome {
    pub evidence: Evidence,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct DocumentSnapshot {
    pub revision: String,
    pub documents: Vec<ReadOutcome>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub revision: String,
    pub sources: std::collections::BTreeMap<String, usize>,
    pub searchable_documents: usize,
}

#[derive(Default, Debug, Serialize)]
pub struct SyncOutcome {
    pub files: usize,
    pub units: usize,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexChanges {
    pub source_id: String,
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub deleted: Vec<String>,
    pub unchanged: usize,
    pub indexed_units: usize,
}
