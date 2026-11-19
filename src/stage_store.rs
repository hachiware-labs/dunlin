//! Where stage data lives (spec 28.3, step 2e). Extraction results are corpus revisions and chunk
//! sets live in `corpus.sqlite`; `state.sqlite` keeps only their identifiers. Projects built before
//! step 2e move once (spec 28.7): their cached extractions, stage payloads and chunk caches go to
//! the corpus without parsing again, and their stage records keep their IDs.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::Cancellation;
use crate::config_value::ConfigValue;
use crate::source::DocumentInput;
use crate::source_load::sha256_hex;
use crate::transform_stage::StageError;

const STATE_TABLES: &str = "CREATE TABLE IF NOT EXISTS extractions (
    key TEXT PRIMARY KEY, source_id TEXT NOT NULL, object_id TEXT NOT NULL, version TEXT NOT NULL);";
/// Chunk sets are a rowid table: rows of a few kilobytes read about 40% faster than from a
/// `WITHOUT ROWID` table, which spills them to overflow pages sooner.
const CORPUS_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS chunk_sets (id TEXT PRIMARY KEY, chunks TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS chunkings (key TEXT PRIMARY KEY, chunk_set TEXT NOT NULL) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS version_chunkings (version TEXT NOT NULL, settings TEXT NOT NULL,
        chunk_set TEXT NOT NULL, PRIMARY KEY (version, settings)) WITHOUT ROWID;";
/// The tables of the layout before step 2e.
const OLD_TABLES: [&str; 3] = ["loaded_files", "stage_payloads", "document_chunks"];

/// The keys of a document that the corpus version covers, with `modifiedAtMs`.
const DOCUMENT_KEYS: [&str; 6] = [
    "objectId",
    "url",
    "format",
    "content",
    "corpus",
    "modifiedAtMs",
];

pub(crate) struct Stores {
    pub state: Connection,
    pub corpus: Connection,
    /// The project moved from the layout before step 2e during this call.
    pub migrated: bool,
}

fn sql(error: rusqlite::Error) -> StageError {
    StageError::plain(error.to_string())
}

fn text(value: &str) -> ConfigValue {
    ConfigValue::String(value.to_owned())
}

fn items(value: Option<&ConfigValue>) -> &[ConfigValue] {
    match value {
        Some(ConfigValue::Array(items)) => items,
        _ => &[],
    }
}

/// Opens both databases and moves an older project once.
pub(crate) fn open(state: &str, corpus: &str, cancel: &Cancellation) -> Result<Stores, StageError> {
    let mut state = Connection::open(state).map_err(sql)?;
    state
        .busy_timeout(std::time::Duration::from_secs(10))
        .map_err(sql)?;
    state.execute_batch(STATE_TABLES).map_err(sql)?;
    let mut corpus =
        crate::corpus::open_connection(Path::new(corpus), cancel).map_err(StageError::native)?;
    corpus.execute_batch(CORPUS_TABLES).map_err(sql)?;
    let migrated = migrate(&mut state, &mut corpus, cancel)?;
    Ok(Stores {
        state,
        corpus,
        migrated,
    })
}

/// The corpus version and record of a document as the `source` stage produces it.
pub(crate) fn revision(
    source_id: &str,
    document: &ConfigValue,
) -> Result<(String, Vec<u8>), StageError> {
    let mut input = Vec::new();
    for name in DOCUMENT_KEYS {
        if let Some(value) = document
            .get(name)
            .filter(|value| **value != ConfigValue::Null)
        {
            input.push((name.to_owned(), value.clone()));
        }
    }
    let input: DocumentInput = serde_json::from_str(&ConfigValue::Object(input).to_js_json())
        .map_err(|_| StageError::plain("Invalid extraction result."))?;
    crate::source::revision(source_id, input).map_err(StageError::native)
}

pub(crate) const INSERT_REVISION: &str =
    "INSERT INTO revisions (source_id, object_id, version, record)
    VALUES (?1, ?2, ?3, ?4) ON CONFLICT (source_id, object_id, version) DO NOTHING";

/// The document of a corpus revision, in the shape the stages pass on: `objectId`, `url`,
/// `modifiedAtMs`, `content`, `format`, `corpus` (`locations`, `parser`, `originalSha256`) and
/// `version`.
pub(crate) fn read_document(
    corpus: &Connection,
    source_id: &str,
    object_id: &str,
    version: &str,
    modified_at_ms: Option<&ConfigValue>,
) -> Result<Option<ConfigValue>, StageError> {
    let mut statement = corpus.prepare(SELECT_REVISION).map_err(sql)?;
    read_with(
        &mut statement,
        source_id,
        object_id,
        version,
        modified_at_ms,
    )
}

const SELECT_REVISION: &str =
    "SELECT record FROM revisions WHERE source_id = ?1 AND object_id = ?2 AND version = ?3";

/// Reads documents of corpus revisions with one prepared statement.
pub(crate) struct DocumentReader<'a>(rusqlite::Statement<'a>);

impl<'a> DocumentReader<'a> {
    pub fn new(corpus: &'a Connection) -> Result<Self, StageError> {
        corpus.prepare(SELECT_REVISION).map(Self).map_err(sql)
    }

    /// The document of an entry of a `source` stage, or `STAGE_REQUIRED` when it is gone.
    pub fn entry(
        &mut self,
        source_id: &str,
        entry: &ConfigValue,
    ) -> Result<ConfigValue, StageError> {
        let object_id = entry
            .get("objectId")
            .and_then(ConfigValue::as_str)
            .unwrap_or_default();
        let version = entry
            .get("version")
            .and_then(ConfigValue::as_str)
            .unwrap_or_default();
        read_with(
            &mut self.0,
            source_id,
            object_id,
            version,
            entry.get("modifiedAtMs"),
        )?
        .ok_or_else(|| missing_document(source_id, object_id))
    }
}

/// `read_document` with a prepared `SELECT_REVISION`.
fn read_with(
    statement: &mut rusqlite::Statement<'_>,
    source_id: &str,
    object_id: &str,
    version: &str,
    modified_at_ms: Option<&ConfigValue>,
) -> Result<Option<ConfigValue>, StageError> {
    let record: Option<Vec<u8>> = statement
        .query_row(params![source_id, object_id, version], |row| row.get(0))
        .optional()
        .map_err(sql)?;
    let Some(record) = record else {
        return Ok(None);
    };
    let record: ConfigValue = serde_json::from_slice(&record)
        .map_err(|_| StageError::plain("A corpus revision is not valid JSON."))?;
    let url = record
        .get("evidence")
        .and_then(|evidence| evidence.get("source"))
        .and_then(|source| source.get("url"))
        .cloned()
        .unwrap_or(ConfigValue::Null);
    let mut metadata = Vec::new();
    for name in ["locations", "parser", "originalSha256"] {
        if let Some(value) = record.get(name) {
            metadata.push((name.to_owned(), value.clone()));
        }
    }
    let mut document = vec![
        ("objectId".to_owned(), text(object_id)),
        ("url".to_owned(), url),
    ];
    if let Some(modified) = modified_at_ms.filter(|value| **value != ConfigValue::Null) {
        document.push(("modifiedAtMs".to_owned(), modified.clone()));
    }
    for name in ["content", "format"] {
        document.push((
            name.to_owned(),
            record.get(name).cloned().unwrap_or(ConfigValue::Null),
        ));
    }
    document.push(("corpus".to_owned(), ConfigValue::Object(metadata)));
    document.push(("version".to_owned(), text(version)));
    Ok(Some(ConfigValue::Object(document)))
}

/// The entry a `source` stage keeps for a document: its object ID, corpus version, the SHA-256 of
/// the original and its modification time.
pub(crate) fn source_entry(document: &ConfigValue, version: &str) -> ConfigValue {
    let mut entry = vec![
        (
            "objectId".to_owned(),
            document
                .get("objectId")
                .cloned()
                .unwrap_or(ConfigValue::Null),
        ),
        ("version".to_owned(), text(version)),
    ];
    if let Some(hash) = document
        .get("corpus")
        .and_then(|corpus| corpus.get("originalSha256"))
    {
        entry.push(("sha256".to_owned(), hash.clone()));
    }
    if let Some(modified) = document
        .get("modifiedAtMs")
        .filter(|value| **value != ConfigValue::Null)
    {
        entry.push(("modifiedAtMs".to_owned(), modified.clone()));
    }
    ConfigValue::Object(entry)
}

fn missing_document(source_id: &str, object_id: &str) -> StageError {
    StageError::coded(
        "STAGE_REQUIRED",
        format!(
            "The extraction result of {source_id}:{object_id} is missing from the corpus. Run dunlin build."
        ),
        3,
    )
}

/// A recorded `source` stage with each entry replaced by its document, as the later stages read it.
pub(crate) fn expand_source(
    corpus: &Connection,
    manifest: ConfigValue,
) -> Result<ConfigValue, StageError> {
    let ConfigValue::Object(mut entries) = manifest else {
        return Err(StageError::plain("A saved source stage is not valid."));
    };
    let mut statement = corpus.prepare(SELECT_REVISION).map_err(sql)?;
    for (name, value) in entries.iter_mut() {
        let (true, ConfigValue::Array(groups)) = (name == "groups", value) else {
            continue;
        };
        for group in groups.iter_mut() {
            let ConfigValue::Array(pair) = group else {
                continue;
            };
            let source_id = pair
                .first()
                .and_then(ConfigValue::as_str)
                .unwrap_or_default()
                .to_owned();
            let Some(ConfigValue::Array(documents)) = pair.get_mut(1) else {
                continue;
            };
            for document in documents.iter_mut() {
                let object_id = document
                    .get("objectId")
                    .and_then(ConfigValue::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let version = document
                    .get("version")
                    .and_then(ConfigValue::as_str)
                    .unwrap_or_default();
                *document = read_with(
                    &mut statement,
                    &source_id,
                    &object_id,
                    version,
                    document.get("modifiedAtMs"),
                )?
                .ok_or_else(|| missing_document(&source_id, &object_id))?;
            }
        }
    }
    Ok(ConfigValue::Object(entries))
}

/// The chunk set saved for a corpus version and chunk settings (spec 28.3).
pub(crate) fn read_version_chunking(
    statement: &mut rusqlite::Statement<'_>,
    version: &str,
    settings: &str,
) -> Result<Option<String>, StageError> {
    statement
        .query_row(params![version, settings], |row| row.get(0))
        .optional()
        .map_err(sql)
}

pub(crate) const SELECT_VERSION_CHUNKING: &str =
    "SELECT chunk_set FROM version_chunkings WHERE version = ? AND settings = ?";

/// Saves the chunk sets of corpus versions for chunk settings, in one transaction.
pub(crate) fn write_version_chunkings(
    corpus: &Connection,
    rows: &[(String, String)],
    settings: &str,
) -> Result<(), StageError> {
    corpus.execute_batch("BEGIN IMMEDIATE").map_err(sql)?;
    let written = (|| {
        let mut insert = corpus
            .prepare("INSERT OR REPLACE INTO version_chunkings VALUES (?, ?, ?)")
            .map_err(sql)?;
        for (version, chunk_set) in rows {
            insert
                .execute(params![version, settings, chunk_set])
                .map_err(sql)?;
        }
        Ok(())
    })();
    match written {
        Ok(()) => corpus.execute_batch("COMMIT").map_err(sql),
        Err(error) => {
            let _ = corpus.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// The cache key of a chunking in `chunkings`: the SHA-256 of a JSON key, or a key that is one.
pub(crate) fn chunk_key(key: &str) -> String {
    if key.starts_with('[') {
        sha256_hex(key.as_bytes())
    } else {
        key.to_owned()
    }
}

/// The ID of a chunk set: the SHA-256 of its JSON.
pub(crate) fn chunk_set_id(chunks: &[ConfigValue]) -> String {
    sha256_hex(ConfigValue::Array(chunks.to_vec()).to_js_json().as_bytes())
}

/// The text of a chunk's byte range in `content`.
fn chunk_text<'a>(chunk: &ConfigValue, content: &'a str) -> Option<&'a str> {
    let range = chunk.get("byteRange")?;
    let bound = |name: &str| match range.get(name) {
        Some(ConfigValue::Number(value)) if *value >= 0.0 && value.fract() == 0.0 => {
            Some(*value as usize)
        }
        _ => None,
    };
    content.get(bound("start")?..bound("end")?)
}

/// The chunks as `chunk_sets` stores them: a `searchText` that is the NFKC form of the chunk's
/// text becomes `true`, and reading rebuilds it (spec 28.14). The chunk set ID covers the text.
pub(crate) fn stored_chunks(chunks: &[ConfigValue], content: &str) -> Vec<ConfigValue> {
    chunks
        .iter()
        .map(|chunk| {
            let ConfigValue::Object(entries) = chunk else {
                return chunk.clone();
            };
            let derived = match (chunk.get("searchText"), chunk_text(chunk, content)) {
                (Some(ConfigValue::String(search)), Some(text)) => {
                    *search == crate::chunk::nfkc(text)
                }
                _ => false,
            };
            if !derived {
                return chunk.clone();
            }
            let mut entries = entries.clone();
            put(&mut entries, "searchText", Some(ConfigValue::Bool(true)));
            ConfigValue::Object(entries)
        })
        .collect()
}

/// Rebuilds each `searchText` that `stored_chunks` left out, from the document's content.
pub(crate) fn restore_chunks(chunks: &mut [ConfigValue], content: &str) -> Result<(), StageError> {
    for chunk in chunks {
        if chunk.get("searchText") != Some(&ConfigValue::Bool(true)) {
            continue;
        }
        let search = chunk_text(chunk, content)
            .map(crate::chunk::nfkc)
            .ok_or_else(|| StageError::plain("A saved chunk set does not match its document."))?;
        if let ConfigValue::Object(entries) = chunk {
            put(entries, "searchText", Some(ConfigValue::String(search)));
        }
    }
    Ok(())
}

/// Rebuilds the `searchText` of an expanded transform stage from the expanded source stage, whose
/// groups and documents are in the same order.
pub(crate) fn restore_transform(
    source: &ConfigValue,
    transform: &mut ConfigValue,
) -> Result<(), StageError> {
    let groups = match source.get("groups") {
        Some(ConfigValue::Array(groups)) => groups.as_slice(),
        _ => &[],
    };
    let ConfigValue::Object(entries) = transform else {
        return Ok(());
    };
    let Some((_, ConfigValue::Array(chunks))) =
        entries.iter_mut().find(|(name, _)| name == "chunks")
    else {
        return Ok(());
    };
    for (group, sets) in groups.iter().zip(chunks.iter_mut()) {
        let documents = match group {
            ConfigValue::Array(pair) => match pair.get(1) {
                Some(ConfigValue::Array(documents)) => documents.as_slice(),
                _ => &[],
            },
            _ => &[],
        };
        let ConfigValue::Array(sets) = sets else {
            continue;
        };
        for (document, set) in documents.iter().zip(sets.iter_mut()) {
            if let ConfigValue::Array(set) = set {
                let content = document
                    .get("content")
                    .and_then(ConfigValue::as_str)
                    .unwrap_or_default();
                restore_chunks(set, content)?;
            }
        }
    }
    Ok(())
}

/// The chunks cached for a document and chunk settings, as stored: `restore_chunks` rebuilds their
/// `searchText`.
pub(crate) fn read_chunking(
    corpus: &Connection,
    key: &str,
) -> Result<Option<Vec<ConfigValue>>, StageError> {
    let row: Option<String> = corpus
        .query_row(
            "SELECT chunks FROM chunkings JOIN chunk_sets ON chunk_sets.id = chunkings.chunk_set
             WHERE chunkings.key = ?",
            [chunk_key(key)],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    Ok(match row.and_then(|row| serde_json::from_str(&row).ok()) {
        Some(ConfigValue::Array(chunks)) => Some(chunks),
        _ => None,
    })
}

/// Saves chunk sets and the chunkings that lead to them, in one transaction. `replace` overwrites
/// a cached chunking (a forced `chunk.semantic`, whose model output can change). Each entry has
/// the document's content, which `stored_chunks` compares with.
pub(crate) fn write_chunkings(
    corpus: &Connection,
    entries: &[(String, &[ConfigValue], &str)],
    replace: bool,
) -> Result<(), StageError> {
    corpus.execute_batch("BEGIN IMMEDIATE").map_err(sql)?;
    let written = (|| {
        let mut set = corpus
            .prepare("INSERT OR IGNORE INTO chunk_sets VALUES (?, ?)")
            .map_err(sql)?;
        let mut chunking = corpus
            .prepare(if replace {
                "INSERT OR REPLACE INTO chunkings VALUES (?, ?)"
            } else {
                "INSERT OR IGNORE INTO chunkings VALUES (?, ?)"
            })
            .map_err(sql)?;
        for (key, chunks, content) in entries {
            let id = chunk_set_id(chunks);
            let stored = ConfigValue::Array(stored_chunks(chunks, content)).to_js_json();
            set.execute(params![id, stored]).map_err(sql)?;
            chunking.execute(params![chunk_key(key), id]).map_err(sql)?;
        }
        Ok(())
    })();
    match written {
        Ok(()) => corpus.execute_batch("COMMIT").map_err(sql),
        Err(error) => {
            let _ = corpus.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// A recorded `transform` stage with each chunk set ID replaced by its chunks.
pub(crate) fn expand_transform(
    corpus: &Connection,
    manifest: ConfigValue,
) -> Result<ConfigValue, StageError> {
    let ConfigValue::Object(mut entries) = manifest else {
        return Err(StageError::plain("A saved transform stage is not valid."));
    };
    let mut find = corpus
        .prepare("SELECT chunks FROM chunk_sets WHERE id = ?")
        .map_err(sql)?;
    for (name, value) in entries.iter_mut() {
        let (true, ConfigValue::Array(groups)) = (name == "chunks", value) else {
            continue;
        };
        for group in groups.iter_mut() {
            let ConfigValue::Array(documents) = group else {
                continue;
            };
            for document in documents.iter_mut() {
                let id = document.as_str().unwrap_or_default().to_owned();
                let data: Option<String> = find
                    .query_row([&id], |row| row.get(0))
                    .optional()
                    .map_err(sql)?;
                let data = data.ok_or_else(|| {
                    StageError::coded(
                        "STAGE_REQUIRED",
                        "A saved chunk set is missing from the corpus. Run dunlin build.",
                        3,
                    )
                })?;
                *document = serde_json::from_str(&data)
                    .map_err(|_| StageError::plain("A saved chunk set is not valid JSON."))?;
            }
        }
    }
    Ok(ConfigValue::Object(entries))
}

fn old_tables(state: &Connection) -> Result<Vec<&'static str>, StageError> {
    let mut found = Vec::new();
    for table in OLD_TABLES {
        let exists = state
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
                [table],
                |_| Ok(()),
            )
            .optional()
            .map_err(sql)?
            .is_some();
        if exists {
            found.push(table);
        }
    }
    Ok(found)
}

/// Replaces `key` in an object, keeping its position, or removes it for `None`.
fn put(entries: &mut Vec<(String, ConfigValue)>, key: &str, value: Option<ConfigValue>) {
    let position = entries.iter().position(|(name, _)| name == key);
    match (position, value) {
        (Some(index), Some(value)) => entries[index].1 = value,
        (None, Some(value)) => entries.push((key.to_owned(), value)),
        (Some(index), None) => {
            entries.remove(index);
        }
        (None, None) => {}
    }
}

/// Moves a project built before step 2e (spec 28.7). Returns whether there was anything to move.
///
/// Cached extractions become corpus revisions with an `extractions` row each. Source stages keep
/// their IDs and list corpus versions; transform stages keep their IDs and list chunk set IDs.
/// Cached chunks become chunk sets with their chunkings. The old tables go last, so a move that
/// stops partway runs again from the start.
pub(crate) fn migrate(
    state: &mut Connection,
    corpus: &mut Connection,
    cancel: &Cancellation,
) -> Result<bool, StageError> {
    let tables = old_tables(state)?;
    if tables.is_empty() {
        return Ok(false);
    }
    let has = |table: &str| tables.contains(&table);
    let check = || cancel.check().map_err(|_| StageError::aborted());
    let mut extractions = Vec::new();
    let mut manifests = Vec::new();
    let transaction = corpus
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    {
        let mut revision_insert = transaction.prepare(INSERT_REVISION).map_err(sql)?;
        let mut save = |source_id: &str, document: &ConfigValue| -> Result<String, StageError> {
            let (version, record) = revision(source_id, document)?;
            let object_id = document
                .get("objectId")
                .and_then(ConfigValue::as_str)
                .unwrap_or_default();
            revision_insert
                .execute(params![source_id, object_id, version, record])
                .map_err(sql)?;
            Ok(version)
        };
        if has("loaded_files") {
            let mut rows = state
                .prepare("SELECT id, document FROM loaded_files")
                .map_err(sql)?;
            let mut rows = rows.query([]).map_err(sql)?;
            while let Some(row) = rows.next().map_err(sql)? {
                check()?;
                let key: String = row.get(0).map_err(sql)?;
                let document: String = row.get(1).map_err(sql)?;
                let parts: ConfigValue = serde_json::from_str(&key).unwrap_or(ConfigValue::Null);
                let (Some(source_id), Ok(document)) = (
                    items(Some(&parts)).get(1).and_then(ConfigValue::as_str),
                    serde_json::from_str::<ConfigValue>(&document),
                ) else {
                    continue;
                };
                // A row the corpus cannot hold is parsed again on the next build.
                let Ok(version) = save(source_id, &document) else {
                    continue;
                };
                let object_id = document
                    .get("objectId")
                    .and_then(ConfigValue::as_str)
                    .unwrap_or_default()
                    .to_owned();
                extractions.push((key, source_id.to_owned(), object_id, version));
            }
        }
        let payload = |key: &str| -> Result<String, StageError> {
            if !has("stage_payloads") {
                return Err(StageError::coded(
                    "STAGE_REQUIRED",
                    "A saved stage payload is missing.",
                    3,
                ));
            }
            state
                .query_row(
                    "SELECT data FROM stage_payloads WHERE id = ?",
                    [key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql)?
                .ok_or_else(|| {
                    StageError::coded("STAGE_REQUIRED", "A saved stage payload is missing.", 3)
                })
        };
        let rows: Vec<(String, String, String)> = {
            let mut statement = state
                .prepare("SELECT stage, id, manifest FROM stage_outputs WHERE stage IN ('source', 'transform')")
                .map_err(sql)?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(sql)?
                .collect::<Result<_, _>>()
                .map_err(sql)?
        };
        let mut sets = transaction
            .prepare("INSERT OR IGNORE INTO chunk_sets VALUES (?, ?)")
            .map_err(sql)?;
        for (stage, id, manifest) in rows {
            check()?;
            let Ok(ConfigValue::Object(mut entries)) = serde_json::from_str(&manifest) else {
                continue;
            };
            if entries.iter().any(|(name, _)| name == "migrated") {
                continue;
            }
            let payloads = entries
                .iter()
                .any(|(name, value)| name == "payloads" && *value == ConfigValue::Number(1.0));
            // A stage whose payloads are gone cannot move; later builds make it again.
            let moved = (|| -> Result<(), StageError> {
                for (name, value) in entries.iter_mut() {
                    match (stage.as_str(), name.as_str(), value) {
                        ("source", "groups", ConfigValue::Array(groups)) => {
                            for group in groups.iter_mut() {
                                let ConfigValue::Array(pair) = group else {
                                    continue;
                                };
                                let source_id = pair
                                    .first()
                                    .and_then(ConfigValue::as_str)
                                    .unwrap_or_default()
                                    .to_owned();
                                let Some(ConfigValue::Array(documents)) = pair.get_mut(1) else {
                                    continue;
                                };
                                for document in documents.iter_mut() {
                                    let ConfigValue::Object(fields) = document else {
                                        continue;
                                    };
                                    if payloads {
                                        let key = fields
                                            .iter()
                                            .find(|(name, _)| name == "text")
                                            .and_then(|(_, value)| value.as_str())
                                            .unwrap_or_default()
                                            .to_owned();
                                        let content: ConfigValue = serde_json::from_str(&payload(
                                            &key,
                                        )?)
                                        .map_err(|_| StageError::plain("Invalid stage payload."))?;
                                        put(fields, "text", None);
                                        put(fields, "content", Some(content));
                                    }
                                    let full = ConfigValue::Object(fields.clone());
                                    let version = save(&source_id, &full)?;
                                    *document = source_entry(&full, &version);
                                }
                            }
                        }
                        ("transform", "chunks", ConfigValue::Array(groups)) => {
                            for group in groups.iter_mut() {
                                let ConfigValue::Array(documents) = group else {
                                    continue;
                                };
                                for document in documents.iter_mut() {
                                    let json = if payloads {
                                        payload(document.as_str().unwrap_or_default())?
                                    } else {
                                        document.to_js_json()
                                    };
                                    let id = sha256_hex(json.as_bytes());
                                    sets.execute(params![id, json]).map_err(sql)?;
                                    *document = text(&id);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(())
            })();
            if moved.is_err() {
                continue;
            }
            put(&mut entries, "payloads", None);
            put(&mut entries, "migrated", Some(ConfigValue::Bool(true)));
            manifests.push((stage, id, ConfigValue::Object(entries).to_js_json()));
        }
        if has("document_chunks") {
            let mut chunking = transaction
                .prepare("INSERT OR IGNORE INTO chunkings VALUES (?, ?)")
                .map_err(sql)?;
            let mut rows = state
                .prepare("SELECT id, chunks FROM document_chunks")
                .map_err(sql)?;
            let mut rows = rows.query([]).map_err(sql)?;
            while let Some(row) = rows.next().map_err(sql)? {
                check()?;
                let key: String = row.get(0).map_err(sql)?;
                let chunks: String = row.get(1).map_err(sql)?;
                let id = sha256_hex(chunks.as_bytes());
                sets.execute(params![id, chunks]).map_err(sql)?;
                chunking
                    .execute(params![chunk_key(&key), id])
                    .map_err(sql)?;
            }
        }
    }
    check()?;
    transaction.commit().map_err(sql)?;

    let transaction = state
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    {
        let mut insert = transaction
            .prepare("INSERT OR REPLACE INTO extractions VALUES (?, ?, ?, ?)")
            .map_err(sql)?;
        for (key, source_id, object_id, version) in &extractions {
            insert
                .execute(params![key, source_id, object_id, version])
                .map_err(sql)?;
        }
        let mut update = transaction
            .prepare("UPDATE stage_outputs SET manifest = ? WHERE stage = ? AND id = ?")
            .map_err(sql)?;
        for (stage, id, manifest) in &manifests {
            update.execute(params![manifest, stage, id]).map_err(sql)?;
        }
        for table in &tables {
            transaction
                .execute_batch(&format!("DROP TABLE {table}"))
                .map_err(sql)?;
        }
    }
    transaction.commit().map_err(sql)?;
    // Give the space of the removed copies back to the file system. Another reader can hold the
    // database open; the space then stays free inside the file.
    let _ = state.execute_batch("VACUUM");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> ConfigValue {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn moves_an_older_project_once() {
        let root = tempfile::tempdir().unwrap();
        let state_path = root.path().join("state.sqlite");
        let corpus_path = root.path().join("corpus");
        let document = json(
            r##"{"objectId":"a.md","url":"file:///a.md","modifiedAtMs":5,"content":"# A\nbody\n","format":"markdown","corpus":{"originalSha256":"abc"}}"##,
        );
        let content_key = sha256_hex(br##""# A\nbody\n""##);
        let chunks = r#"[{"heading":"A","range":{"startLine":1,"endLine":2},"byteRange":{"start":0,"end":9}}]"#;
        let chunk_id = sha256_hex(chunks.as_bytes());
        let length_key = r#"["chunk.length-v1",null,800,100,"markdown","h"]"#;
        let state = Connection::open(&state_path).unwrap();
        state
            .execute_batch(
                "CREATE TABLE stage_outputs (stage TEXT NOT NULL, id TEXT NOT NULL, manifest TEXT NOT NULL, PRIMARY KEY (stage, id));
                 CREATE TABLE loaded_files (id TEXT PRIMARY KEY, document TEXT NOT NULL);
                 CREATE TABLE stage_payloads (id TEXT PRIMARY KEY, data TEXT NOT NULL);
                 CREATE TABLE document_chunks (id TEXT PRIMARY KEY, chunks TEXT NOT NULL);",
            )
            .unwrap();
        state
            .execute(
                "INSERT INTO loaded_files VALUES (?, ?)",
                params![
                    r#"["loader.auto-v1","docs","a.md",".md","abc"]"#,
                    document.to_js_json()
                ],
            )
            .unwrap();
        state
            .execute(
                "INSERT INTO stage_payloads VALUES (?, ?), (?, ?)",
                params![content_key, r##""# A\nbody\n""##, chunk_id, chunks],
            )
            .unwrap();
        state
            .execute(
                "INSERT INTO stage_outputs VALUES ('source', 'src-old', ?), ('transform', 'trn-old', ?)",
                params![
                    format!(
                        r#"{{"id":"src-old","groups":[["docs",[{{"objectId":"a.md","url":"file:///a.md","modifiedAtMs":5,"format":"markdown","corpus":{{"originalSha256":"abc"}},"text":"{content_key}"}}]]],"payloads":1}}"#
                    ),
                    format!(
                        r#"{{"id":"trn-old","source_id":"src-old","chunks":[["{chunk_id}"]],"payloads":1}}"#
                    ),
                ],
            )
            .unwrap();
        state
            .execute(
                "INSERT INTO document_chunks VALUES (?, ?)",
                params![length_key, chunks],
            )
            .unwrap();
        drop(state);
        let cancel = Cancellation::default();
        let stores = open(
            &state_path.to_string_lossy(),
            &corpus_path.to_string_lossy(),
            &cancel,
        )
        .unwrap();
        assert!(stores.migrated);
        assert!(old_tables(&stores.state).unwrap().is_empty());
        let (version, _) = revision("docs", &document).unwrap();
        let (key, mapped): (String, String) = stores
            .state
            .query_row("SELECT key, version FROM extractions", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert!(key.starts_with("[\"loader.auto-v1\""));
        assert_eq!(mapped, version);
        let manifest = |stage: &str| -> ConfigValue {
            let text: String = stores
                .state
                .query_row(
                    "SELECT manifest FROM stage_outputs WHERE stage = ?",
                    [stage],
                    |row| row.get(0),
                )
                .unwrap();
            json(&text)
        };
        let source = manifest("source");
        assert_eq!(
            source.get("groups").unwrap().to_js_json(),
            format!(
                r#"[["docs",[{{"objectId":"a.md","version":"{version}","sha256":"abc","modifiedAtMs":5}}]]]"#
            )
        );
        assert_eq!(source.get("migrated"), Some(&ConfigValue::Bool(true)));
        let expanded = expand_source(&stores.corpus, source).unwrap();
        let group = items(expanded.get("groups"))[0].clone();
        let documents = items(Some(&group)).get(1).cloned();
        let documents = items(documents.as_ref()).to_vec();
        assert_eq!(documents[0].get("content"), document.get("content"));
        assert_eq!(
            documents[0].get("modifiedAtMs"),
            document.get("modifiedAtMs")
        );
        let transform = manifest("transform");
        assert_eq!(
            transform.get("chunks").unwrap().to_js_json(),
            format!(r#"[["{chunk_id}"]]"#)
        );
        let expanded = expand_transform(&stores.corpus, transform).unwrap();
        assert_eq!(
            expanded.get("chunks").unwrap().to_js_json(),
            format!("[[{chunks}]]")
        );
        assert_eq!(
            read_chunking(&stores.corpus, length_key)
                .unwrap()
                .map(|chunks| ConfigValue::Array(chunks).to_js_json()),
            Some(chunks.to_owned())
        );
        drop(stores);
        let again = open(
            &state_path.to_string_lossy(),
            &corpus_path.to_string_lossy(),
            &cancel,
        )
        .unwrap();
        assert!(!again.migrated);
    }
}
