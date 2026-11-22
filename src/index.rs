//! The `index` stage on the new store: a keyword index over the chunks of a snapshot, and the
//! search that reads it (docs/data-management.md, 6.4).
//!
//! An `index` snapshot lists one information object per index: what was indexed and by which
//! modules. The directory of the index is a note on that object, so the index can be built again
//! without the snapshot changing.
//!
//! Not here yet: updating an index from the one before it, the vector index, and removing the
//! directories of versions that are no longer needed. The index keeps its own copy of the text.
//!
//! When the chunks ask for it, the text that is searched starts with the context of the chunk:
//! the name of the document and the headings above the chunk. The text a search returns does not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use icu_normalizer::ComposingNormalizerBorrowed;
use serde_json::{Value, json};

use crate::engine::KeywordOptions;
use crate::records::{InfoId, ItemId, ModuleIdentity, Process, info_id};
use crate::source::DocumentInput;
use crate::store::{Committed, SnapshotId, Store, StoreError, StoreResult, now_ms};
use crate::{Cancellation, CorpusBackend, KnowledgeBase};

pub struct IndexRequest<'a> {
    /// The `chunks` snapshot to index.
    pub input: SnapshotId,
    /// Where the directories of indexes are made.
    pub directory: &'a Path,
    pub tag: Option<&'a str>,
    /// The configuration file as it is now. It is kept with the run.
    pub config: Option<&'a [u8]>,
    /// The run that started this one, such as an optimization.
    pub cause: Option<&'a str>,
}

#[derive(Debug)]
pub struct IndexOutcome {
    pub index: Committed,
    /// An index directory was built in this run.
    pub built: bool,
    pub location: PathBuf,
}

/// A chunk that a search found.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub source: String,
    pub document: String,
    /// First and last line of the chunk, one-based.
    pub lines: (usize, usize),
    pub text: String,
}

/// What building the keyword index is to the store.
pub fn indexing() -> Process {
    Process {
        modules: vec![ModuleIdentity {
            name: "index.keyword".into(),
            compatibility: 1,
            args: json!({ "tokenizer": "lindera" }),
        }],
    }
}

fn failed(error: crate::Error) -> StoreError {
    StoreError::new(error.to_string())
}

fn open(location: &Path) -> StoreResult<KnowledgeBase> {
    KnowledgeBase::open_with_options(
        location,
        CorpusBackend::default(),
        None,
        KeywordOptions {
            japanese: true,
            ..KeywordOptions::default()
        },
        &Cancellation::default(),
    )
    .map_err(failed)
}

/// Puts the context of each chunk before the text that is searched for it: the name of the
/// document when it is given, then the headings the chunk keeps.
fn prepend(chunks: &mut Value, content: &str, document: Option<&str>) {
    let nfkc = ComposingNormalizerBorrowed::new_nfkc();
    for chunk in chunks.as_array_mut().into_iter().flatten() {
        let headings = chunk["context"]["headings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        let context: Vec<&str> = document.into_iter().chain(headings).collect();
        if context.is_empty() {
            continue;
        }
        let range = |name: &str| chunk["byteRange"][name].as_u64().map(|at| at as usize);
        let text = match chunk["searchText"].as_str() {
            Some(text) => Some(text.to_owned()),
            None => range("start")
                .zip(range("end"))
                .and_then(|(start, end)| content.get(start..end))
                .map(str::to_owned),
        };
        if let Some(text) = text {
            let header = nfkc.normalize(&context.join(" > ")).into_owned();
            chunk["searchText"] = json!(format!("{header}\n\n{text}"));
        }
    }
}

/// Builds the index of a `chunks` snapshot in a new directory.
fn build(store: &Store, input: SnapshotId, location: &Path) -> StoreResult<usize> {
    let mut sources: BTreeMap<String, Vec<DocumentInput>> = BTreeMap::new();
    let mut documents = 0;
    for (name, item) in store.snapshot(input)?.items {
        let [source, document] = name.as_slice() else {
            continue;
        };
        let Some(id) = ItemId::parse(&item) else {
            continue;
        };
        let parts = store.item(&id)?;
        let (Some(text), Some(chunks)) = (
            parts.iter().find(|part| part.kind == "text"),
            parts.iter().find(|part| part.kind == "chunks"),
        ) else {
            continue;
        };
        let content = String::from_utf8(store.bytes(&text.object)?)
            .map_err(|_| StoreError::new("A text part does not hold text."))?;
        let context = chunks.attrs.get("context").cloned();
        let mut chunks: Value = serde_json::from_slice(&store.bytes(&chunks.object)?)
            .map_err(|_| StoreError::new("A chunks part does not hold chunks."))?;
        if let Some(context) = context.filter(|context| context["prepend"] == true) {
            let named = (context["document"] == true).then_some(document.as_str());
            prepend(&mut chunks, &content, named);
        }
        let format = if text.media_type == "text/markdown" {
            "markdown"
        } else {
            "text"
        };
        let input = serde_json::from_value(json!({
            "objectId": document,
            "url": format!("knowledge-nest://{source}/{document}"),
            "format": format,
            "content": content,
            "chunks": chunks,
        }))
        .map_err(|error| StoreError::new(error.to_string()))?;
        sources.entry(source.clone()).or_default().push(input);
        documents += 1;
    }
    let index = open(location)?;
    for (source, inputs) in sources {
        index
            .replace_document_inputs(&source, inputs, &Cancellation::default())
            .map_err(failed)?;
    }
    Ok(documents)
}

/// Builds the next generation of the index an information object describes, and notes where it
/// is on the object.
/// The longest path below the directory of an index: the journal of its database.
const LONGEST_INSIDE: usize = "/.knowledge-nest-corpus-v1/corpus.sqlite-journal".len();
/// The longest path that is known to work on Windows, with some room left.
const LONGEST_PATH: usize = 256;

fn materialize(
    store: &Store,
    info: &InfoId,
    input: SnapshotId,
    directory: &Path,
) -> StoreResult<PathBuf> {
    let generation = store.object_notes(info.as_str())?["built"]["generation"]
        .as_u64()
        .unwrap_or(0)
        + 1;
    let name = format!("keyword-{}-g{generation}", &info.as_str()[4..20]);
    let location = directory.join(&name);
    // The database inside the index is opened by SQLite, which on Windows takes no path of
    // 260 characters or more. Said here, because what SQLite says does not name the cause.
    let longest = location.to_string_lossy().chars().count() + LONGEST_INSIDE;
    if cfg!(windows) && longest > LONGEST_PATH {
        return Err(StoreError::new(format!(
            "The index would be made at {}, and a file inside it would have a path of {longest} characters. Windows takes at most {LONGEST_PATH}. Move the project to a folder with a shorter path.",
            location.display()
        )));
    }
    // A directory of this name is what an attempt that failed left: no index is noted there.
    if location.exists() {
        std::fs::remove_dir_all(&location)?;
    }
    let documents = match build(store, input, &location) {
        Ok(documents) => documents,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&location);
            return Err(error);
        }
    };
    store.note_object(
        info.as_str(),
        "built",
        &json!({
            "location": name,
            "generation": generation,
            "built_at": now_ms(),
            "documents": documents,
        }),
    )?;
    Ok(location)
}

/// The directory noted on an information object, when it is there.
fn built(store: &Store, info: &InfoId, directory: &Path) -> StoreResult<Option<PathBuf>> {
    let notes = store.object_notes(info.as_str())?;
    Ok(notes["built"]["location"]
        .as_str()
        .map(|name| directory.join(name))
        .filter(|location| location.is_dir()))
}

/// Indexes the chunks of a snapshot.
pub fn index(store: &Store, request: &IndexRequest<'_>) -> StoreResult<IndexOutcome> {
    let process = indexing();
    let description = json!({
        "index": "keyword",
        "input": store.snapshot(request.input)?.digest().as_str(),
        "modules": process.to_value(),
    });
    let mut draft = store.begin("index", "index", &[request.input], &process)?;
    if let Some(config) = request.config {
        draft.set_config(config)?;
    }
    if let Some(cause) = request.cause {
        draft.set_cause(cause);
    }
    if let Some(existing) = draft.existing()? {
        // Found by the key. The directory may be gone all the same: then only it is made again.
        let info = info_id(&description);
        let (location, rebuilt) = match built(store, &info, request.directory)? {
            Some(location) => (location, false),
            None => (
                materialize(store, &info, request.input, request.directory)?,
                true,
            ),
        };
        return Ok(IndexOutcome {
            index: draft.finish_reused(existing, request.tag)?,
            built: rebuilt,
            location,
        });
    }
    let info = draft.save_info(&description)?;
    draft.list(vec!["keyword".into()], info.to_string());
    let location = materialize(store, &info, request.input, request.directory)?;
    draft.set_stats(json!({ "location": location.to_string_lossy() }));
    Ok(IndexOutcome {
        index: draft.commit(request.tag, None)?,
        built: true,
        location,
    })
}

/// Searches the keyword index of an `index` snapshot, given by a tag or its serial number.
pub fn search(
    store: &Store,
    directory: &Path,
    tag_or_id: &str,
    query: &str,
    limit: usize,
) -> StoreResult<Vec<Found>> {
    let id = store
        .resolve("index", tag_or_id)?
        .ok_or_else(|| StoreError::new(format!("There is no index named {tag_or_id}.")))?;
    let snapshot = store.snapshot(id)?;
    let info = snapshot
        .items
        .iter()
        .find(|(name, _)| name.as_slice() == ["keyword"])
        .and_then(|(_, id)| InfoId::parse(id))
        .filter(|_| snapshot.kind == "index")
        .ok_or_else(|| StoreError::new(format!("{tag_or_id} is not a keyword index.")))?;
    let location = built(store, &info, directory)?.ok_or_else(|| {
        StoreError::new(format!(
            "The index of {tag_or_id} is not built. Run a build."
        ))
    })?;
    let index = open(&location)?;
    let cancel = Cancellation::default();
    index
        .search(query, limit, &cancel)
        .map_err(failed)?
        .hits
        .into_iter()
        .map(|hit| {
            let read = index.read(&hit.evidence, &cancel).map_err(failed)?;
            Ok(Found {
                source: hit.evidence.source.source_id,
                document: hit.evidence.source.object_id,
                lines: (hit.evidence.range.start_line, hit.evidence.range.end_line),
                text: read.content,
            })
        })
        .collect()
}
