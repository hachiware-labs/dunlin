//! The `extract` stage on the new store, for text files (docs/data-management.md).
//!
//! A run scans the folder and makes a `files` snapshot, then makes the `extracted` snapshot from
//! it. Files that did not change are neither opened nor read again.
//!
//! Not here yet: the other parsers, several sources in one run, the stop on a large drop in the
//! number of documents, and the command line.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::records::{ItemName, Location, ModuleIdentity, ObjectId, Part, Process, object_id};
use crate::store::{Committed, Counts, Store, StoreError, StoreResult, compare, now_ms};

pub struct ExtractRequest<'a> {
    /// The ID of the source the folder is.
    pub source: &'a str,
    pub root: &'a Path,
    /// The name that follows the result, as the section's `tag` gives it.
    pub tag: Option<&'a str>,
    /// The configuration file as it is now. It is kept with the runs.
    pub config: Option<&'a [u8]>,
}

/// What a run of `extract` did. Documents are named by their path below the root, with `/`.
#[derive(Debug)]
pub struct ExtractOutcome {
    /// The file list: which documents there were, with what content.
    pub files: Committed,
    pub extracted: Committed,
    /// Compared with the file list of the run that last moved the tag.
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub deleted: Vec<String>,
    pub unchanged: usize,
    /// Files whose bytes were read to hash them.
    pub hashed: usize,
    /// Documents that were parsed.
    pub read: usize,
    /// Files no parser takes. They are in the file list and not among the extracted documents.
    pub skipped: Vec<String>,
    /// Documents that failed, for which the earlier text stands in when there is one.
    pub substituted: Vec<String>,
}

/// The identity of the parser of text files. Arguments that only steer scanning, such as the
/// path, are not part of it: the file list already shows their result.
fn loader() -> Process {
    Process {
        modules: vec![ModuleIdentity {
            name: "loader.text".into(),
            compatibility: 1,
            args: json!({}),
        }],
    }
}

fn media_type(document: &str) -> Option<&'static str> {
    let extension = Path::new(document)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match extension.as_str() {
        "md" | "markdown" => Some("text/markdown"),
        "txt" => Some("text/plain"),
        _ => None,
    }
}

fn documents(names: Vec<ItemName>) -> Vec<String> {
    names
        .into_iter()
        .filter_map(|mut name| name.pop())
        .collect()
}

/// Size, modification time and whether to hash again, as a scan noted them on a file list.
fn noted(seen: Option<&Value>, document: &str) -> Option<(u64, u64, bool)> {
    match seen?.get(document)?.as_array()?.as_slice() {
        [size, modified_ms, recheck] => {
            Some((size.as_u64()?, modified_ms.as_u64()?, recheck.as_bool()?))
        }
        _ => None,
    }
}

/// Scans the folder and brings the extracted documents up to date.
pub fn extract(store: &Store, request: &ExtractRequest<'_>) -> StoreResult<ExtractOutcome> {
    let started = now_ms();
    let name = |document: &str| vec![request.source.to_owned(), document.to_owned()];
    // The file list of the run that last moved the tag, and the documents the tag holds now.
    let (before, current) = match request.tag {
        Some(tag) => (
            store.last_inputs(tag)?.first().copied(),
            store.resolve(tag)?,
        ),
        None => (None, None),
    };
    let before = before.map(|id| store.snapshot(id)).transpose()?;
    let listed_before: HashMap<&ItemName, &String> = before
        .iter()
        .flat_map(|snapshot| &snapshot.items)
        .map(|(name, id)| (name, id))
        .collect();
    let seen = before
        .as_ref()
        .and_then(|snapshot| snapshot.meta.get("seen"));

    // The file list is made on every run. A scan that fails leaves a failed run and no list, so
    // no deletion is concluded from it.
    let mut draft = store.begin_scan("extract", "files")?;
    if let Some(config) = request.config {
        draft.set_config(config)?;
    }
    let mut hashed = 0;
    let mut files: Vec<(String, String)> = Vec::new();
    let mut notes = serde_json::Map::new();
    for entry in walkdir::WalkDir::new(request.root).sort_by_file_name() {
        let entry = entry.map_err(|error| StoreError::new(error.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(request.root)
            .map_err(|error| StoreError::new(error.to_string()))?;
        let document = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let metadata = entry
            .metadata()
            .map_err(|error| StoreError::new(error.to_string()))?;
        let modified_ms = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        let size = metadata.len();
        let earlier = listed_before.get(&name(&document)).copied();
        let id = match (noted(seen, &document), earlier) {
            // The same size and time as last seen: the file is not opened.
            (Some((seen_size, seen_modified, false)), Some(earlier))
                if seen_size == size && seen_modified == modified_ms =>
            {
                earlier.clone()
            }
            _ => {
                hashed += 1;
                let bytes = std::fs::read(entry.path())?;
                if media_type(&document).is_some() {
                    // A file a parser takes is kept, so that it is read once.
                    draft.save(&bytes)?.to_string()
                } else {
                    object_id(&bytes).to_string()
                }
            }
        };
        // A write in the same tick as the hashing would keep this time: hash again next time.
        notes.insert(
            document.clone(),
            json!([size, modified_ms, modified_ms >= started]),
        );
        draft.list(name(&document), id.clone());
        files.push((document, id));
    }
    let listed: Vec<(ItemName, String)> = files
        .iter()
        .map(|(document, id)| (name(document), id.clone()))
        .collect();
    let changes = compare(
        before
            .as_ref()
            .map_or(&[][..], |snapshot| snapshot.items.as_slice()),
        &listed,
    );
    let counts = Counts {
        added: changes.added.len(),
        updated: changes.updated.len(),
        deleted: changes.removed.len(),
        unchanged: changes.unchanged,
    };
    draft.set_counts(counts);
    draft.set_stats(json!({
        "source": request.source,
        "root": request.root.to_string_lossy(),
        "hashed": hashed,
    }));
    let listing = draft.commit(None, before.as_ref().map(|snapshot| snapshot.id))?;
    store.note(listing.snapshot, "seen", &Value::Object(notes))?;

    let process = loader();
    let applied = process.id();
    let mut draft = store.begin(
        "extract",
        "extracted",
        std::slice::from_ref(&listing.snapshot),
        &process,
    )?;
    draft.set_counts(counts);
    if let Some(config) = request.config {
        draft.set_config(config)?;
    }
    let mut read = 0;
    let mut skipped = Vec::new();
    let mut substituted = Vec::new();
    let extracted = if let Some(existing) = draft.existing()? {
        draft.finish_reused(existing, request.tag)?
    } else {
        let current = current.map(|id| store.snapshot(id)).transpose()?;
        let held: HashMap<&ItemName, &String> = current
            .iter()
            .flat_map(|snapshot| &snapshot.items)
            .map(|(name, id)| (name, id))
            .collect();
        for (document, id) in &files {
            let Some(media_type) = media_type(document) else {
                skipped.push(document.clone());
                continue;
            };
            let object = ObjectId::parse(id)
                .ok_or_else(|| StoreError::new("The file list holds an ID that is not valid."))?;
            // Only the parser that takes this file is asked what it made of the content.
            if let Some(item) = draft.result(object.as_str(), &applied)? {
                draft.list(name(document), item.to_string());
                continue;
            }
            let bytes = store.bytes(&object)?;
            if std::str::from_utf8(&bytes).is_err() {
                substituted.push(document.clone());
                let earlier = held.get(&name(document)).map(|item| (*item).clone());
                draft.substitute(name(document), earlier);
                continue;
            }
            read += 1;
            let lines = bytes.split_inclusive(|byte| *byte == b'\n').count().max(1) as u64;
            let part = Part {
                // The text of a text file is its bytes: the same object as the file.
                object: object.clone(),
                kind: "text".into(),
                media_type: media_type.into(),
                location: Some(Location::TextRange {
                    lines: (1, lines),
                    bytes: (0, bytes.len() as u64),
                }),
                derived_from: None,
                attrs: json!({}),
            };
            draft.put(name(document), object.as_str(), &applied, &[part])?;
        }
        draft.set_stats(json!({
            "read": read,
            "skipped": skipped,
            "substituted": substituted,
        }));
        draft.commit(request.tag, None)?
    };

    Ok(ExtractOutcome {
        files: listing,
        extracted,
        added: documents(changes.added),
        updated: documents(changes.updated),
        deleted: documents(changes.removed),
        unchanged: changes.unchanged,
        hashed,
        read,
        skipped,
        substituted,
    })
}
