//! The `transform` stage (spec 28.9 step 2b): `text.normalize`, `chunk.length`, `chunk.structure`
//! and `chunk.semantic`, the chunk cache and the stage record.
//!
//! Identifiers, cache keys and stored JSON are the ones the JavaScript implementation wrote, so
//! existing projects keep their stage IDs, cached chunks and build IDs.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Deserialize;

use crate::Cancellation;
use crate::chunk::{ChunkDocument, ChunkRequest, TextChunk, chunk_documents};
use crate::config_value::ConfigValue;
use crate::embed::{Charge, EmbedError, Embedder, EmbedderSettings};
use crate::source_load::sha256_hex;

/// Errors carry an optional code and exit code, as the JavaScript errors did.
pub type StageError = EmbedError;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformRequest {
    pub state: String,
    /// The recorded `source` stage to transform.
    pub source_id: String,
    #[serde(default)]
    pub force: bool,
    /// `moduleSettings(plan)` for the transform stage.
    pub modules: ConfigValue,
    /// The embedding model of `chunk.semantic`.
    #[serde(default)]
    pub embedder: Option<EmbedderSettings>,
}

pub struct TransformOutcome {
    pub output: ConfigValue,
    pub reused: bool,
}

/// Chunk documents in batches of about this many characters, checking for cancellation between them.
const BATCH_CHARS: usize = 4 * 1024 * 1024;
/// Texts per embedding request.
const EMBED_BATCH: usize = 32;

fn text(value: &str) -> ConfigValue {
    ConfigValue::String(value.to_owned())
}

fn number(value: usize) -> ConfigValue {
    ConfigValue::Number(value as f64)
}

/// `kind + '-' + sha256(JSON.stringify(value))`
fn digest(kind: &str, value: &ConfigValue) -> String {
    format!("{kind}-{}", sha256_hex(value.to_js_json().as_bytes()))
}

fn sql(error: rusqlite::Error) -> StageError {
    StageError::plain(error.to_string())
}

fn aborted(_: crate::Error) -> StageError {
    StageError::aborted()
}

/// One document of the source stage, as the transform stage reads it.
struct Document {
    content: String,
    /// `format`, or null.
    format: ConfigValue,
    /// Lines that start a page: `corpus.locations` with a page, first occurrence first.
    boundaries: Vec<usize>,
}

enum Chunker {
    Length {
        size: ConfigValue,
        overlap: ConfigValue,
        structure: bool,
    },
    Semantic {
        max_chars: ConfigValue,
        sentence_chars: ConfigValue,
        similarity: ConfigValue,
    },
}

pub fn run(
    request: &TransformRequest,
    charge: &dyn Charge,
    cancel: &Cancellation,
) -> Result<TransformOutcome, StageError> {
    let modules = match &request.modules {
        ConfigValue::Array(modules) => modules.as_slice(),
        _ => &[],
    };
    let mut unicode = ConfigValue::Null;
    let mut chunker = None;
    for module in modules {
        if chunker.is_some() {
            return Err(StageError::plain(
                "The transform stage has a module after its chunk module.",
            ));
        }
        let args = module.get("with").cloned().unwrap_or(ConfigValue::Null);
        let arg = |name: &str| args.get(name).cloned().unwrap_or(ConfigValue::Null);
        match module.get("use").and_then(ConfigValue::as_str) {
            Some("text.normalize") => unicode = arg("unicode"),
            Some("chunk.length") => {
                chunker = Some(Chunker::Length {
                    size: arg("size"),
                    overlap: arg("overlap"),
                    structure: false,
                })
            }
            Some("chunk.structure") => {
                chunker = Some(Chunker::Length {
                    size: arg("max_chars"),
                    overlap: number(0),
                    structure: true,
                })
            }
            Some("chunk.semantic") => {
                chunker = Some(Chunker::Semantic {
                    max_chars: arg("max_chars"),
                    sentence_chars: arg("sentence_chars"),
                    similarity: arg("similarity"),
                })
            }
            other => {
                return Err(StageError::plain(format!(
                    "The transform stage cannot run {}.",
                    other.unwrap_or("this module")
                )));
            }
        }
    }
    let Some(chunker) = chunker else {
        return Err(StageError::plain(
            "The transform stage has no chunk module.",
        ));
    };
    let semantic = matches!(chunker, Chunker::Semantic { .. });
    let embedder = match (&chunker, &request.embedder) {
        (Chunker::Semantic { .. }, Some(settings)) => Some(settings),
        (Chunker::Semantic { .. }, None) => {
            return Err(StageError::coded(
                "VECTOR_MODEL_REQUIRED",
                "Define an embedding model under models.",
                2,
            ));
        }
        _ => None,
    };
    let config_id = digest(
        "cfg",
        &match embedder {
            Some(settings) => {
                ConfigValue::Array(vec![request.modules.clone(), settings.identity()])
            }
            None => request.modules.clone(),
        },
    );
    let id = digest(
        "trn",
        &ConfigValue::Array(vec![text(&request.source_id), text(&config_id)]),
    );

    let mut connection = Connection::open(&request.state).map_err(sql)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(10))
        .map_err(sql)?;
    let latest = if semantic {
        connection
            .query_row(
                "SELECT output_id FROM transform_outputs WHERE key = ?",
                [&id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(sql)?
    } else {
        None
    };
    if !request.force
        && let Some(saved) = read_transform(&connection, latest.as_deref().unwrap_or(&id))?
    {
        save(&mut connection, &saved, true)?;
        return Ok(TransformOutcome {
            output: saved,
            reused: true,
        });
    }

    let groups = read_source(&connection, &request.source_id)?;
    let nfkc = unicode.as_str() == Some("nfkc");
    let cache = Cache {
        connection: &connection,
        force: request.force,
        replace: request.force && semantic,
    };
    let chunks = match &chunker {
        Chunker::Length {
            size,
            overlap,
            structure,
        } => length_chunks(
            &groups, &unicode, nfkc, size, overlap, *structure, &cache, cancel,
        )?,
        Chunker::Semantic {
            max_chars,
            sentence_chars,
            similarity,
        } => {
            let settings = embedder.expect("checked above");
            semantic_chunks(
                &groups,
                &unicode,
                nfkc,
                [max_chars, sentence_chars, similarity],
                &Embedder::new(settings.clone()),
                &settings.identity(),
                charge,
                &cache,
                cancel,
            )?
        }
    };
    let chunks = ConfigValue::Array(
        chunks
            .into_iter()
            .map(|group| ConfigValue::Array(group.into_iter().map(ConfigValue::Array).collect()))
            .collect(),
    );
    let output_id = if semantic {
        digest("trn", &ConfigValue::Array(vec![text(&id), chunks.clone()]))
    } else {
        id.clone()
    };
    let output = ConfigValue::Object(vec![
        ("id".into(), text(&output_id)),
        ("key".into(), text(&id)),
        ("config_id".into(), text(&config_id)),
        ("source_id".into(), text(&request.source_id)),
        ("modules".into(), request.modules.clone()),
        ("chunks".into(), chunks),
    ]);
    cancel.check().map_err(aborted)?;
    save(&mut connection, &output, false)?;
    Ok(TransformOutcome {
        output,
        reused: false,
    })
}

/// The documents of a recorded source stage, by group.
fn read_source(connection: &Connection, id: &str) -> Result<Vec<Vec<Document>>, StageError> {
    let manifest: Option<String> = connection
        .query_row(
            "SELECT manifest FROM stage_outputs WHERE stage = 'source' AND id = ?",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    let Some(manifest) = manifest else {
        return Err(StageError::coded(
            "STAGE_REQUIRED",
            format!("Missing saved source {id}. Run dunlin build."),
            3,
        ));
    };
    let manifest: ConfigValue = serde_json::from_str(&manifest)
        .map_err(|_| StageError::plain("A saved source stage is not valid JSON."))?;
    let payloads = manifest.get("payloads") == Some(&ConfigValue::Number(1.0));
    let mut find = connection
        .prepare("SELECT data FROM stage_payloads WHERE id = ?")
        .map_err(sql)?;
    let groups = match manifest.get("groups") {
        Some(ConfigValue::Array(groups)) => groups.as_slice(),
        _ => &[],
    };
    groups
        .iter()
        .map(|group| {
            let documents = match group {
                ConfigValue::Array(pair) => match pair.get(1) {
                    Some(ConfigValue::Array(documents)) => documents.as_slice(),
                    _ => &[],
                },
                _ => &[],
            };
            documents
                .iter()
                .map(|document| {
                    let content = if payloads {
                        let key = document
                            .get("text")
                            .and_then(ConfigValue::as_str)
                            .unwrap_or_default();
                        let data: Option<String> = find
                            .query_row([key], |row| row.get(0))
                            .optional()
                            .map_err(sql)?;
                        let data = data.ok_or_else(missing_payload)?;
                        serde_json::from_str::<String>(&data).map_err(|_| missing_payload())?
                    } else {
                        document
                            .get("content")
                            .and_then(ConfigValue::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    };
                    Ok(Document {
                        content,
                        format: document.get("format").cloned().unwrap_or(ConfigValue::Null),
                        boundaries: page_starts(document),
                    })
                })
                .collect()
        })
        .collect()
}

fn missing_payload() -> StageError {
    StageError::coded("STAGE_REQUIRED", "A saved stage payload is missing.", 3)
}

/// `[...new Set(locations.filter(({ page }) => page).map(({ range }) => range.startLine))]`
fn page_starts(document: &ConfigValue) -> Vec<usize> {
    let mut seen = HashSet::new();
    let locations = match document
        .get("corpus")
        .and_then(|corpus| corpus.get("locations"))
    {
        Some(ConfigValue::Array(locations)) => locations.as_slice(),
        _ => &[],
    };
    locations
        .iter()
        .filter(|location| truthy(location.get("page")))
        .filter_map(|location| match location.get("range")?.get("startLine")? {
            ConfigValue::Number(line) if *line >= 0.0 => Some(*line as usize),
            _ => None,
        })
        .filter(|line| seen.insert(*line))
        .collect()
}

fn truthy(value: Option<&ConfigValue>) -> bool {
    match value {
        None | Some(ConfigValue::Null) | Some(ConfigValue::Bool(false)) => false,
        Some(ConfigValue::Number(value)) => *value != 0.0,
        Some(ConfigValue::NonFinite(name)) => name != "NaN",
        Some(ConfigValue::String(value)) => !value.is_empty(),
        Some(_) => true,
    }
}

/// The `document_chunks` table: chunks by cache key.
struct Cache<'a> {
    connection: &'a Connection,
    /// Skip cached chunks.
    force: bool,
    /// Overwrite cached chunks (forced `chunk.semantic`, whose model output can change).
    replace: bool,
}

impl Cache<'_> {
    fn read(&self, key: &str) -> Result<Option<Vec<ConfigValue>>, StageError> {
        if self.force {
            return Ok(None);
        }
        let row: Option<String> = self
            .connection
            .query_row(
                "SELECT chunks FROM document_chunks WHERE id = ?",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?;
        Ok(match row.and_then(|row| serde_json::from_str(&row).ok()) {
            Some(ConfigValue::Array(chunks)) => Some(chunks),
            _ => None,
        })
    }

    /// Writes entries in one transaction.
    fn write(&self, entries: &[(String, &[ConfigValue])]) -> Result<(), StageError> {
        let statement = if self.replace {
            "INSERT OR REPLACE INTO document_chunks VALUES (?, ?)"
        } else {
            "INSERT OR IGNORE INTO document_chunks VALUES (?, ?)"
        };
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sql)?;
        let written = (|| {
            let mut insert = self.connection.prepare(statement).map_err(sql)?;
            for (key, chunks) in entries {
                let json = ConfigValue::Array(chunks.to_vec()).to_js_json();
                insert.execute(params![key, json]).map_err(sql)?;
            }
            Ok(())
        })();
        match written {
            Ok(()) => self.connection.execute_batch("COMMIT").map_err(sql),
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }
}

/// A chunk as JSON, with the keys in the order the JavaScript objects had.
fn chunk_value(chunk: &TextChunk) -> ConfigValue {
    let mut entries = vec![
        ("heading".into(), text(&chunk.heading)),
        (
            "range".into(),
            ConfigValue::Object(vec![
                ("startLine".into(), number(chunk.range.start_line)),
                ("endLine".into(), number(chunk.range.end_line)),
            ]),
        ),
        (
            "byteRange".into(),
            ConfigValue::Object(vec![
                ("start".into(), number(chunk.byte_range.start)),
                ("end".into(), number(chunk.byte_range.end)),
            ]),
        ),
    ];
    if let Some(search_text) = &chunk.search_text {
        entries.push(("searchText".into(), text(search_text)));
    }
    ConfigValue::Object(entries)
}

fn size_of(value: &ConfigValue) -> usize {
    match value {
        ConfigValue::Number(value) if *value >= 0.0 => *value as usize,
        _ => 0,
    }
}

/// `chunk.length` and `chunk.structure`, with the cache keyed by options and content hash.
#[allow(clippy::too_many_arguments)]
fn length_chunks(
    groups: &[Vec<Document>],
    unicode: &ConfigValue,
    nfkc: bool,
    size: &ConfigValue,
    overlap: &ConfigValue,
    structure: bool,
    cache: &Cache,
    cancel: &Cancellation,
) -> Result<Vec<Vec<Vec<ConfigValue>>>, StageError> {
    let mut output: Vec<Vec<Vec<ConfigValue>>> = groups
        .iter()
        .map(|documents| vec![Vec::new(); documents.len()])
        .collect();
    let mut pending = Vec::new();
    for (group, documents) in groups.iter().enumerate() {
        for (index, document) in documents.iter().enumerate() {
            cancel.check().map_err(aborted)?;
            let mut key = vec![
                text(if structure {
                    "chunk.structure-v1"
                } else {
                    "chunk.length-v1"
                }),
                unicode.clone(),
                size.clone(),
                overlap.clone(),
                document.format.clone(),
                text(&sha256_hex(document.content.as_bytes())),
            ];
            if structure {
                key.push(ConfigValue::Array(
                    document
                        .boundaries
                        .iter()
                        .map(|&line| number(line))
                        .collect(),
                ));
            }
            let key = ConfigValue::Array(key).to_js_json();
            match cache.read(&key)? {
                Some(saved) => output[group][index] = saved,
                None => pending.push((group, index, key)),
            }
        }
    }
    let mut start = 0;
    while start < pending.len() {
        cancel.check().map_err(aborted)?;
        let mut end = start;
        let mut chars = 0;
        while end < pending.len() && (end == start || chars < BATCH_CHARS) {
            let (group, index, _) = &pending[end];
            chars += groups[*group][*index].content.encode_utf16().count();
            end += 1;
        }
        let batch = &pending[start..end];
        let request = ChunkRequest {
            documents: batch
                .iter()
                .map(|(group, index, _)| {
                    let document = &groups[*group][*index];
                    ChunkDocument {
                        content: document.content.clone(),
                        markdown: document.format.as_str() == Some("markdown"),
                        boundaries: if structure {
                            document.boundaries.clone()
                        } else {
                            Vec::new()
                        },
                    }
                })
                .collect(),
            size: size_of(size),
            overlap: size_of(overlap),
            structure,
            nfkc,
        };
        let chunked = chunk_documents(&request, cancel).map_err(aborted)?;
        for ((group, index, _), chunks) in batch.iter().zip(chunked) {
            output[*group][*index] = chunks.iter().map(chunk_value).collect();
        }
        let entries: Vec<(String, &[ConfigValue])> = batch
            .iter()
            .map(|(group, index, key)| (key.clone(), output[*group][*index].as_slice()))
            .collect();
        cache.write(&entries)?;
        start = end;
    }
    Ok(output)
}

/// `String.prototype.trim()` leaves nothing: JavaScript white space and line terminators only.
pub(crate) fn js_blank(value: &str) -> bool {
    value.chars().all(|c| {
        matches!(
            c,
            '\u{9}'
                | '\u{a}'
                | '\u{b}'
                | '\u{c}'
                | '\u{d}'
                | ' '
                | '\u{a0}'
                | '\u{1680}'
                | '\u{2000}'
                ..='\u{200a}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202f}'
                    | '\u{205f}'
                    | '\u{3000}'
                    | '\u{feff}'
        )
    })
}

/// `chunk.semantic`: sentence-sized passages, joined while neighbours stay similar.
#[allow(clippy::too_many_arguments)]
fn semantic_chunks(
    groups: &[Vec<Document>],
    unicode: &ConfigValue,
    nfkc: bool,
    [max_chars, sentence_chars, similarity]: [&ConfigValue; 3],
    embedder: &Embedder,
    identity: &ConfigValue,
    charge: &dyn Charge,
    cache: &Cache,
    cancel: &Cancellation,
) -> Result<Vec<Vec<Vec<ConfigValue>>>, StageError> {
    let limit = size_of(max_chars);
    let threshold = match similarity {
        ConfigValue::Number(value) => *value,
        _ => f64::NAN,
    };
    let mut output = Vec::new();
    for documents in groups {
        let mut chunks = Vec::new();
        for document in documents {
            cancel.check().map_err(aborted)?;
            let key = sha256_hex(
                ConfigValue::Array(vec![
                    text("chunk.semantic-v1"),
                    identity.clone(),
                    max_chars.clone(),
                    sentence_chars.clone(),
                    similarity.clone(),
                    unicode.clone(),
                    document.format.clone(),
                    text(&sha256_hex(document.content.as_bytes())),
                ])
                .to_js_json()
                .as_bytes(),
            );
            if let Some(saved) = cache.read(&key)? {
                chunks.push(saved);
                continue;
            }
            let parts = chunk_documents(
                &ChunkRequest {
                    documents: vec![ChunkDocument {
                        content: document.content.clone(),
                        markdown: document.format.as_str() == Some("markdown"),
                        boundaries: Vec::new(),
                    }],
                    size: size_of(sentence_chars),
                    overlap: 0,
                    structure: false,
                    nfkc,
                },
                cancel,
            )
            .map_err(aborted)?
            .pop()
            .unwrap_or_default();
            let raw =
                |part: &TextChunk| &document.content[part.byte_range.start..part.byte_range.end];
            let texts: Vec<String> = parts
                .iter()
                .map(|part| {
                    part.search_text
                        .clone()
                        .unwrap_or_else(|| raw(part).to_owned())
                })
                .collect();
            let nonempty: Vec<usize> = (0..texts.len()).filter(|&i| !js_blank(&texts[i])).collect();
            let mut vectors: Vec<Option<Vec<f64>>> = vec![None; parts.len()];
            for batch in nonempty.chunks(EMBED_BATCH) {
                let input: Vec<String> = batch.iter().map(|&i| texts[i].clone()).collect();
                let response = embedder.embed(&input, charge, cancel)?;
                for (&i, vector) in batch.iter().zip(response) {
                    vectors[i] = Some(vector);
                }
            }
            let mut present = vectors.iter().flatten();
            if let Some(first) = present.next()
                && present.any(|vector| vector.len() != first.len())
            {
                return Err(StageError::coded(
                    "VECTOR_DIMENSIONS",
                    "Embedding dimensions differ within the document.",
                    2,
                ));
            }
            let mut merged: Vec<TextChunk> = Vec::new();
            let mut length = 0;
            for (j, part) in parts.iter().enumerate() {
                let width = raw(part).chars().count();
                let related = j > 0
                    && match (&vectors[j], &vectors[j - 1]) {
                        (Some(current), Some(previous)) => {
                            current
                                .iter()
                                .zip(previous)
                                .fold(0.0, |sum, (a, b)| sum + a * b)
                                >= threshold
                        }
                        _ => false,
                    };
                match merged.last_mut() {
                    Some(previous) if length + width <= limit && related => {
                        previous.range.end_line = part.range.end_line;
                        previous.byte_range.end = part.byte_range.end;
                        if nfkc {
                            previous
                                .search_text
                                .get_or_insert_with(String::new)
                                .push_str(&texts[j]);
                        }
                        length += width;
                    }
                    _ => {
                        merged.push(part.clone());
                        length = width;
                    }
                }
            }
            let merged: Vec<ConfigValue> = merged.iter().map(chunk_value).collect();
            cache.write(&[(key, merged.as_slice())])?;
            chunks.push(merged);
        }
        output.push(chunks);
    }
    Ok(output)
}

/// A recorded transform stage with its chunks, or `None`.
pub(crate) fn read_transform(
    connection: &Connection,
    id: &str,
) -> Result<Option<ConfigValue>, StageError> {
    let manifest: Option<String> = connection
        .query_row(
            "SELECT manifest FROM stage_outputs WHERE stage = 'transform' AND id = ?",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    let manifest: ConfigValue = serde_json::from_str(&manifest)
        .map_err(|_| StageError::plain("A saved transform stage is not valid JSON."))?;
    if manifest.get("payloads") != Some(&ConfigValue::Number(1.0)) {
        return Ok(Some(manifest));
    }
    let mut find = connection
        .prepare("SELECT data FROM stage_payloads WHERE id = ?")
        .map_err(sql)?;
    let ConfigValue::Object(mut entries) = manifest else {
        return Ok(None);
    };
    for (name, value) in entries.iter_mut() {
        if name != "chunks" {
            continue;
        }
        let ConfigValue::Array(groups) = value else {
            continue;
        };
        for group in groups.iter_mut() {
            let ConfigValue::Array(documents) = group else {
                continue;
            };
            for document in documents.iter_mut() {
                let key = document.as_str().unwrap_or_default().to_owned();
                let data: Option<String> = find
                    .query_row([&key], |row| row.get(0))
                    .optional()
                    .map_err(sql)?;
                let data = data.ok_or_else(missing_payload)?;
                *document = serde_json::from_str(&data).map_err(|_| missing_payload())?;
            }
        }
    }
    Ok(Some(ConfigValue::Object(entries)))
}

/// Records the stage: chunk payloads by content, the manifest, the latest stage and, when the output
/// ID differs from its key, the key's latest output.
fn save(connection: &mut Connection, output: &ConfigValue, reused: bool) -> Result<(), StageError> {
    let field = |name: &str| {
        output
            .get(name)
            .and_then(ConfigValue::as_str)
            .map(str::to_owned)
    };
    let id = field("id").unwrap_or_default();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    if !reused {
        let ConfigValue::Object(entries) = output else {
            return Err(StageError::plain("Invalid transform output."));
        };
        let mut insert = transaction
            .prepare("INSERT OR IGNORE INTO stage_payloads VALUES (?, ?)")
            .map_err(sql)?;
        let mut manifest = Vec::with_capacity(entries.len() + 1);
        for (name, value) in entries {
            let value = match (name.as_str(), value) {
                ("chunks", ConfigValue::Array(groups)) => {
                    let mut stored = Vec::with_capacity(groups.len());
                    for group in groups {
                        let ConfigValue::Array(documents) = group else {
                            stored.push(group.clone());
                            continue;
                        };
                        let mut keys = Vec::with_capacity(documents.len());
                        for chunks in documents {
                            let json = chunks.to_js_json();
                            let key = sha256_hex(json.as_bytes());
                            insert.execute(params![key, json]).map_err(sql)?;
                            keys.push(text(&key));
                        }
                        stored.push(ConfigValue::Array(keys));
                    }
                    ConfigValue::Array(stored)
                }
                _ => value.clone(),
            };
            manifest.push((name.clone(), value));
        }
        manifest.push(("payloads".into(), number(1)));
        drop(insert);
        transaction
            .execute(
                "INSERT OR IGNORE INTO stage_outputs VALUES (?, ?, ?)",
                params!["transform", id, ConfigValue::Object(manifest).to_js_json()],
            )
            .map_err(sql)?;
    }
    transaction
        .execute(
            "INSERT INTO latest_stage VALUES (?, ?) ON CONFLICT(stage) DO UPDATE SET id = excluded.id",
            params!["transform", id],
        )
        .map_err(sql)?;
    if let Some(key) = field("key").filter(|key| *key != id) {
        transaction
            .execute(
                "INSERT OR REPLACE INTO transform_outputs VALUES (?, ?)",
                params![key, id],
            )
            .map_err(sql)?;
    }
    transaction.commit().map_err(sql)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::tests::{Free, serve};

    const SCHEMA: &str = "CREATE TABLE stage_outputs (stage TEXT NOT NULL, id TEXT NOT NULL, manifest TEXT NOT NULL, PRIMARY KEY (stage, id));
        CREATE TABLE latest_stage (stage TEXT PRIMARY KEY, id TEXT NOT NULL);
        CREATE TABLE stage_payloads (id TEXT PRIMARY KEY, data TEXT NOT NULL);
        CREATE TABLE document_chunks (id TEXT PRIMARY KEY, chunks TEXT NOT NULL);
        CREATE TABLE transform_outputs (key TEXT PRIMARY KEY, output_id TEXT NOT NULL);";

    fn state(content: &str) -> (tempfile::TempDir, String) {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state.sqlite");
        let connection = Connection::open(&state).unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        let payload = serde_json::to_string(content).unwrap();
        let key = sha256_hex(payload.as_bytes());
        connection
            .execute(
                "INSERT INTO stage_payloads VALUES (?, ?)",
                params![key, payload],
            )
            .unwrap();
        let manifest = format!(
            "{{\"id\":\"src-1\",\"groups\":[[\"docs\",[{{\"objectId\":\"a.md\",\"format\":\"markdown\",\"text\":\"{key}\"}}]]],\"payloads\":1}}"
        );
        connection
            .execute(
                "INSERT INTO stage_outputs VALUES ('source', 'src-1', ?)",
                [manifest],
            )
            .unwrap();
        (root, state.to_string_lossy().into_owned())
    }

    fn modules(json: &str) -> ConfigValue {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn chunks_records_and_reuses_the_stage() {
        let (_root, state) = state("# A\nOne. Two. Three.\n");
        let request = TransformRequest {
            state: state.clone(),
            source_id: "src-1".into(),
            force: false,
            modules: modules(
                r#"[{"use":"text.normalize","version":1,"name":"text.normalize","with":{"unicode":"nfkc"}},
                    {"use":"chunk.length","version":1,"name":"chunk.length","with":{"unit":"characters","size":8,"overlap":0}}]"#,
            ),
            embedder: None,
        };
        let cancel = Cancellation::default();
        let first = run(&request, &Free, &cancel).unwrap();
        assert!(!first.reused);
        let id = first
            .output
            .get("id")
            .and_then(ConfigValue::as_str)
            .unwrap()
            .to_owned();
        assert!(id.starts_with("trn-"));
        assert_eq!(first.output.get("key"), Some(&text(&id)));
        let ConfigValue::Array(groups) = first.output.get("chunks").unwrap() else {
            panic!("expected chunks")
        };
        let ConfigValue::Array(documents) = &groups[0] else {
            panic!("expected documents")
        };
        let ConfigValue::Array(chunks) = &documents[0] else {
            panic!("expected chunks")
        };
        assert!(chunks.len() > 1);
        assert!(chunks[0].get("searchText").is_some());

        let again = run(&request, &Free, &cancel).unwrap();
        assert!(again.reused);
        assert_eq!(again.output.get("chunks"), first.output.get("chunks"));
        let connection = Connection::open(&state).unwrap();
        let cached: i64 = connection
            .query_row("SELECT count(*) FROM document_chunks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(cached, 1);
        let key: String = connection
            .query_row("SELECT id FROM document_chunks", [], |row| row.get(0))
            .unwrap();
        assert!(
            key.starts_with("[\"chunk.length-v1\",\"nfkc\",8,0,\"markdown\",\""),
            "{key}"
        );
        let latest: String = connection
            .query_row(
                "SELECT id FROM latest_stage WHERE stage = 'transform'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(latest, id);
    }

    #[test]
    fn joins_similar_passages_with_the_embedding_model() {
        let content = "Alpha one.\nAlpha two.\nBeta three.\n";
        let (_root, state) = state(content);
        // Passages about the same word get the same direction.
        let (base, bodies) = serve(|body| {
            let vectors: Vec<[f64; 2]> = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|text| {
                    if text.as_str().unwrap().contains("Alpha") {
                        [1.0, 0.0]
                    } else {
                        [0.0, 2.0]
                    }
                })
                .collect();
            (
                200,
                serde_json::json!({ "embeddings": vectors }).to_string(),
            )
        });
        let request = TransformRequest {
            state,
            source_id: "src-1".into(),
            force: false,
            modules: modules(
                r#"[{"use":"chunk.semantic","version":1,"name":"chunk.semantic","with":{"model":"e","max_chars":100,"sentence_chars":11,"similarity":0.5}}]"#,
            ),
            embedder: Some(EmbedderSettings {
                provider: "ollama".into(),
                model: "e".into(),
                url: format!("{base}/api/embed"),
                api_key: None,
                max_input_bytes: None,
            }),
        };
        let cancel = Cancellation::default();
        let first = run(&request, &Free, &cancel).unwrap();
        let ConfigValue::Array(groups) = first.output.get("chunks").unwrap() else {
            panic!()
        };
        let ConfigValue::Array(documents) = &groups[0] else {
            panic!()
        };
        let ConfigValue::Array(chunks) = &documents[0] else {
            panic!()
        };
        let texts: Vec<&str> = chunks
            .iter()
            .map(|chunk| {
                let range = chunk.get("byteRange").unwrap();
                let at = |name| size_of(range.get(name).unwrap());
                content[at("start")..at("end")].trim()
            })
            .collect();
        assert_eq!(texts, ["Alpha one.\nAlpha two.", "Beta three."]);
        assert_ne!(first.output.get("id"), first.output.get("key"));
        assert_eq!(bodies.lock().unwrap().len(), 1);
        let again = run(&request, &Free, &cancel).unwrap();
        assert!(again.reused);
        assert_eq!(again.output.get("id"), first.output.get("id"));
        assert_eq!(bodies.lock().unwrap().len(), 1);
    }

    #[test]
    fn treats_white_space_as_javascript_does() {
        assert!(js_blank(" \u{3000}\u{feff}\n"));
        assert!(!js_blank("\u{85}"));
        assert!(!js_blank("a"));
    }
}
