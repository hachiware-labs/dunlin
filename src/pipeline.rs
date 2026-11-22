//! A build on the new store: `extract`, `transform` and `index` one after the other
//! (docs/data-management.md, 3.3).
//!
//! Each stage takes the snapshot the stage before it made. A stage whose input and settings are
//! the ones of an earlier run is not run again.

use std::path::Path;

use crate::extract::{Apply, ExtractOutcome, ExtractRequest, extract};
use crate::index::{IndexOutcome, IndexRequest, index};
use crate::store::{Store, StoreResult};
use crate::transform::{Chunking, TransformOutcome, TransformRequest, transform};

pub struct BuildRequest<'a> {
    /// The ID of the source the folder is.
    pub source: &'a str,
    /// The folder of documents.
    pub root: &'a Path,
    /// Where the directories of indexes are made.
    pub indexes: &'a Path,
    /// The configuration file as it is now. It is kept with the runs.
    pub config: Option<&'a [u8]>,
    /// Which changes of the folder are taken in.
    pub apply: Apply,
    /// The tags of the three sections.
    pub extract_tag: &'a str,
    pub transform_tag: &'a str,
    pub index_tag: &'a str,
    /// The name that is served. A build with the project's own configuration moves it to the
    /// index it made; a trial leaves it where it is.
    pub release: Option<&'a str>,
    /// Maximum characters per chunk.
    pub size: usize,
    /// Characters repeated from the end of the previous chunk.
    pub overlap: usize,
}

#[derive(Debug)]
pub struct BuildOutcome {
    pub extract: ExtractOutcome,
    pub transform: TransformOutcome,
    pub index: IndexOutcome,
}

/// Brings the documents, their chunks and the index up to date.
pub fn build(store: &Store, request: &BuildRequest<'_>) -> StoreResult<BuildOutcome> {
    let extract = extract(
        store,
        &ExtractRequest {
            source: request.source,
            root: request.root,
            tag: Some(request.extract_tag),
            config: request.config,
            apply: request.apply,
        },
    )?;
    let transform = transform(
        store,
        &TransformRequest {
            input: extract.extracted.snapshot,
            chunking: Chunking::Length {
                size: request.size,
                overlap: request.overlap,
            },
            tag: Some(request.transform_tag),
            config: request.config,
            cause: None,
        },
    )?;
    let index = index(
        store,
        &IndexRequest {
            input: transform.chunks.snapshot,
            directory: request.indexes,
            tag: Some(request.index_tag),
            config: request.config,
            cause: None,
        },
    )?;
    if let Some(release) = request.release {
        store.tag("release", release, index.index.snapshot)?;
    }
    Ok(BuildOutcome {
        extract,
        transform,
        index,
    })
}
