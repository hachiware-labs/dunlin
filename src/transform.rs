//! The `transform` stage on the new store: the text of each document is split into chunks
//! (docs/data-management.md).
//!
//! The item of a document keeps its text and gains a `chunks` part that is derived from it. An
//! item remembers what a chunking made of it, so a document that did not change is not read.
//!
//! The chunking is `chunk.length` or `chunk.structure`.
//!
//! Not here yet: normalization as a module of its own, page boundaries and `chunk.semantic`.

use serde_json::json;

use crate::Cancellation;
use crate::chunk::{ChunkDocument, ChunkRequest, chunk_documents};
use crate::records::{Derived, ItemId, ItemName, ModuleIdentity, Part, Process, canonical};
use crate::store::{Committed, Counts, SnapshotId, Store, StoreError, StoreResult, compare};

pub struct TransformRequest<'a> {
    /// The `extracted` snapshot to chunk.
    pub input: SnapshotId,
    pub chunking: Chunking,
    pub tag: Option<&'a str>,
    /// The configuration file as it is now. It is kept with the run.
    pub config: Option<&'a [u8]>,
    /// The run that started this one, such as an optimization.
    pub cause: Option<&'a str>,
}

/// The module that splits the text, with its arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chunking {
    /// `chunk.length`: windows of at most `size` characters, each starting with `overlap`
    /// characters of the one before.
    Length { size: usize, overlap: usize },
    /// `chunk.structure`: at most `max_chars` characters, split where a section starts.
    Structure { max_chars: usize },
}

impl Chunking {
    /// What the chunking is to the store: a change of module is a change of this, as a change
    /// of an argument is.
    pub fn process(self) -> Process {
        let (name, args) = match self {
            Self::Length { size, overlap } => (
                "chunk.length",
                json!({ "unit": "characters", "size": size, "overlap": overlap, "nfkc": true }),
            ),
            Self::Structure { max_chars } => (
                "chunk.structure",
                json!({ "unit": "characters", "max_chars": max_chars, "nfkc": true }),
            ),
        };
        Process {
            modules: vec![ModuleIdentity {
                name: name.into(),
                compatibility: 1,
                args,
            }],
        }
    }

    fn size(self) -> usize {
        match self {
            Self::Length { size, .. } => size,
            Self::Structure { max_chars } => max_chars,
        }
    }

    fn overlap(self) -> usize {
        match self {
            Self::Length { overlap, .. } => overlap,
            Self::Structure { .. } => 0,
        }
    }
}

#[derive(Debug)]
pub struct TransformOutcome {
    pub chunks: Committed,
    /// Documents that were read and chunked in this run.
    pub chunked: usize,
    /// Compared with the snapshot the tag pointed at before.
    pub counts: Counts,
}

fn counts(before: &[(ItemName, String)], after: &[(ItemName, String)]) -> Counts {
    let changes = compare(before, after);
    Counts {
        added: changes.added.len(),
        updated: changes.updated.len(),
        deleted: changes.removed.len(),
        unchanged: changes.unchanged,
    }
}

/// Chunks the documents of an `extracted` snapshot.
pub fn transform(store: &Store, request: &TransformRequest<'_>) -> StoreResult<TransformOutcome> {
    let input = store.snapshot(request.input)?;
    let process = request.chunking.process();
    let applied = process.id();
    let before = match request.tag {
        Some(tag) => store.resolve(tag)?,
        None => None,
    };
    let before = before
        .map(|id| store.snapshot(id))
        .transpose()?
        .map(|snapshot| snapshot.items)
        .unwrap_or_default();

    let mut draft = store.begin("transform", "chunks", &[request.input], &process)?;
    if let Some(config) = request.config {
        draft.set_config(config)?;
    }
    if let Some(cause) = request.cause {
        draft.set_cause(cause);
    }
    if let Some(existing) = draft.existing()? {
        let counts = counts(&before, &store.snapshot(existing)?.items);
        draft.set_counts(counts);
        return Ok(TransformOutcome {
            chunks: draft.finish_reused(existing, request.tag)?,
            chunked: 0,
            counts,
        });
    }

    let mut chunked = 0;
    let mut listed = Vec::with_capacity(input.items.len());
    for (name, item) in &input.items {
        if let Some(made) = draft.result(item, &applied)? {
            draft.list(name.clone(), made.to_string());
            listed.push((name.clone(), made.to_string()));
            continue;
        }
        let parts = match ItemId::parse(item) {
            Some(id) => store.item(&id)?,
            None => Vec::new(),
        };
        let Some(text) = parts.iter().find(|part| part.kind == "text") else {
            // Nothing this stage handles: the item goes on as it is.
            draft.list(name.clone(), item.clone());
            listed.push((name.clone(), item.clone()));
            continue;
        };
        let content = String::from_utf8(store.bytes(&text.object)?)
            .map_err(|_| StoreError::new("A text part does not hold text."))?;
        let chunks = chunk_documents(
            &ChunkRequest {
                documents: vec![ChunkDocument {
                    content,
                    markdown: text.media_type == "text/markdown",
                    boundaries: Vec::new(),
                }],
                size: request.chunking.size(),
                overlap: request.chunking.overlap(),
                structure: matches!(request.chunking, Chunking::Structure { .. }),
                nfkc: true,
            },
            &Cancellation::default(),
        )
        .map_err(|error| StoreError::new(error.to_string()))?
        .pop()
        .unwrap_or_default();
        let saved =
            serde_json::to_value(&chunks).map_err(|error| StoreError::new(error.to_string()))?;
        let object = draft.save(canonical(&saved).as_bytes())?;
        let mut made = parts.clone();
        made.push(Part {
            object,
            kind: "chunks".into(),
            media_type: "application/json".into(),
            location: None,
            derived_from: Some(Derived {
                object: text.object.clone(),
                location: None,
            }),
            attrs: json!({ "count": chunks.len() }),
        });
        let made = draft.put(name.clone(), item, &applied, &made)?;
        listed.push((name.clone(), made.to_string()));
        chunked += 1;
    }
    let counts = counts(&before, &listed);
    draft.set_counts(counts);
    draft.set_stats(json!({ "chunked": chunked }));
    Ok(TransformOutcome {
        chunks: draft.commit(request.tag, None)?,
        chunked,
        counts,
    })
}
