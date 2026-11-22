//! The `index` stage (spec 28.9 step 2c): the keyword index, the vector index (`exact` or the
//! hannoy tool), the embedding cache and the build record in `state.sqlite`, including the tag and
//! the serving build it may set.
//!
//! Identifiers, stored JSON and files are the ones the JavaScript implementation wrote, so existing
//! projects keep their build IDs and can reuse their indexes.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Deserialize;

use crate::config_value::ConfigValue;
use crate::embed::{Charge, Embedder, EmbedderSettings};
use crate::engine::KeywordOptions;
use crate::source::DocumentInput;
use crate::source_load::sha256_hex;
use crate::source_stage::iso_now;
use crate::transform_stage::{StageError, js_blank, transform_manifest};
use crate::{Cancellation, CorpusBackend, DocumentRef, KnowledgeBase};

/// What the stage asks of JavaScript while it runs.
pub trait IndexHost: Charge {
    /// Whether the configuration file no longer matches the one this build started from.
    fn config_changed(&self) -> Result<bool, String>;
    /// The path of the installed hannoy tool, after its integrity check.
    fn index_tool(&self) -> Result<String, String>;
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexRequest {
    pub state: String,
    /// `.knowledge-nest/artifacts`: build directories and dictionary snapshots.
    pub artifacts: String,
    /// The shared corpus directory.
    pub corpus: String,
    /// The project root. A failed build removes its directory only inside it.
    pub root: String,
    pub run_id: String,
    /// The recorded `transform` stage to index.
    pub transform_id: String,
    /// The `index.keyword` arguments.
    #[serde(default)]
    pub keyword: Option<ConfigValue>,
    /// `{ backend, settings }` of `index.vector` or `hannoy.index`.
    #[serde(default)]
    pub vector: Option<ConfigValue>,
    /// The embedding model of the vector index.
    #[serde(default)]
    pub embedder: Option<EmbedderSettings>,
    /// `moduleSettings(plan.search)`.
    pub search: ConfigValue,
    /// The model settings of the rerank modules, part of the configuration ID.
    pub rerank: ConfigValue,
    /// `moduleSettings(plan.index)`, recorded as `construction.index`.
    pub construction: ConfigValue,
    /// The configuration saved with the build, for `use` and `tag`.
    pub configuration: ConfigValue,
    #[serde(default)]
    pub force: bool,
    /// Make the build the serving build.
    #[serde(default)]
    pub activate: bool,
    #[serde(default)]
    pub tag: Option<String>,
    /// The adoption generation when the build started (`max(adoption_history.id)`).
    #[serde(default)]
    pub generation: Option<i64>,
}

pub struct IndexOutcome {
    /// The build manifest without `index`, with `config_id`, `serving_changed` and `reused`.
    pub summary: ConfigValue,
    /// `{ code, message }` objects.
    pub warnings: Vec<ConfigValue>,
}

/// Texts per embedding request.
const EMBED_BATCH: usize = 32;
/// The largest response read from the index tool.
const TOOL_OUTPUT_LIMIT: usize = 1024 * 1024;
const KEYWORD_DIR: &str = ".knowledge-nest-index-v1";

pub(crate) fn text(value: &str) -> ConfigValue {
    ConfigValue::String(value.to_owned())
}

pub(crate) fn number(value: usize) -> ConfigValue {
    ConfigValue::Number(value as f64)
}

pub(crate) fn object(entries: Vec<(&str, ConfigValue)>) -> ConfigValue {
    ConfigValue::Object(
        entries
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    )
}

/// `kind + '-' + sha256(JSON.stringify(value))`
pub(crate) fn digest(kind: &str, value: &ConfigValue) -> String {
    format!("{kind}-{}", sha256_hex(value.to_js_json().as_bytes()))
}

/// `object[name] = value` in JavaScript, where `None` is `undefined`, which JSON leaves out.
pub(crate) fn assign(
    entries: &mut Vec<(String, ConfigValue)>,
    name: &str,
    value: Option<ConfigValue>,
) {
    match (entries.iter().position(|(key, _)| key == name), value) {
        (Some(index), Some(value)) => entries[index].1 = value,
        (Some(index), None) => {
            entries.remove(index);
        }
        (None, Some(value)) => entries.push((name.to_owned(), value)),
        (None, None) => {}
    }
}

pub(crate) fn field<'a>(value: &'a ConfigValue, name: &str) -> Option<&'a ConfigValue> {
    value.get(name)
}

pub(crate) fn string_field<'a>(value: &'a ConfigValue, name: &str) -> Option<&'a str> {
    value.get(name).and_then(ConfigValue::as_str)
}

/// `JSON.stringify(value)`, which is `undefined` for `undefined`.
pub(crate) fn js_json(value: Option<&ConfigValue>) -> Option<String> {
    value.map(ConfigValue::to_js_json)
}

/// JavaScript truthiness of a JSON value.
pub(crate) fn truthy(value: Option<&ConfigValue>) -> bool {
    match value {
        None | Some(ConfigValue::Null) => false,
        Some(ConfigValue::Bool(value)) => *value,
        Some(ConfigValue::Number(value)) => *value != 0.0 && !value.is_nan(),
        Some(ConfigValue::NonFinite(name)) => name != "NaN",
        Some(ConfigValue::String(value)) => !value.is_empty(),
        Some(_) => true,
    }
}

pub(crate) fn items(value: Option<&ConfigValue>) -> &[ConfigValue] {
    match value {
        Some(ConfigValue::Array(items)) => items,
        _ => &[],
    }
}

pub(crate) fn sql(error: rusqlite::Error) -> StageError {
    StageError::plain(error.to_string())
}

fn io(context: &str, error: std::io::Error) -> StageError {
    StageError::plain(format!("{context}: {error}"))
}

fn json_error(context: &str) -> StageError {
    StageError::plain(format!("{context} is not valid JSON."))
}

fn tag_exists(message: &str) -> StageError {
    StageError::coded("TAG_EXISTS", message, 2)
}

/// A recorded source stage. Its entries name corpus versions; `expand_source` reads the documents.
fn read_source(connection: &Connection, id: &str) -> Result<Option<ConfigValue>, StageError> {
    let manifest: Option<String> = connection
        .query_row(
            "SELECT manifest FROM stage_outputs WHERE stage = 'source' AND id = ?",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    let manifest: ConfigValue =
        serde_json::from_str(&manifest).map_err(|_| json_error("A saved source stage"))?;
    Ok(Some(manifest))
}

/// The corpus version and chunk set of each document of a source and transform stage, by source
/// ID and object ID.
fn document_sets(
    source: &ConfigValue,
    transformed: &ConfigValue,
) -> HashMap<(String, String), (String, String)> {
    let chunks = items(field(transformed, "chunks"));
    let mut sets = HashMap::new();
    for (group, pair) in items(field(source, "groups")).iter().enumerate() {
        let pair = items(Some(pair));
        let id = pair
            .first()
            .and_then(ConfigValue::as_str)
            .unwrap_or_default();
        let ids = items(chunks.get(group));
        for (document, set) in items(pair.get(1)).iter().zip(ids) {
            if let (Some(object_id), Some(version), Some(set)) = (
                string_field(document, "objectId"),
                string_field(document, "version"),
                set.as_str(),
            ) {
                sets.insert(
                    (id.to_owned(), object_id.to_owned()),
                    (version.to_owned(), set.to_owned()),
                );
            }
        }
    }
    sets
}

/// For each source group, the object IDs whose corpus version and chunk set are the same as in
/// `prior`, the build whose keyword index was copied.
fn unchanged_documents(
    connection: &Connection,
    prior: Option<&ConfigValue>,
    source: &ConfigValue,
    transformed: &ConfigValue,
) -> Result<Vec<HashSet<String>>, StageError> {
    let Some((Some(source_id), Some(transform_id))) = prior.map(|prior| {
        (
            string_field(prior, "source_id"),
            string_field(prior, "transform_id"),
        )
    }) else {
        return Ok(Vec::new());
    };
    let (Some(before_source), Some(before_transform)) = (
        read_source(connection, source_id)?,
        transform_manifest(connection, transform_id)?,
    ) else {
        return Ok(Vec::new());
    };
    let before = document_sets(&before_source, &before_transform);
    let now = document_sets(source, transformed);
    Ok(items(field(source, "groups"))
        .iter()
        .map(|pair| {
            let pair = items(Some(pair));
            let id = pair
                .first()
                .and_then(ConfigValue::as_str)
                .unwrap_or_default();
            items(pair.get(1))
                .iter()
                .filter_map(|document| string_field(document, "objectId"))
                .filter(|object_id| {
                    let key = (id.to_owned(), (*object_id).to_owned());
                    now.get(&key)
                        .is_some_and(|set| before.get(&key) == Some(set))
                })
                .map(str::to_owned)
                .collect()
        })
        .collect())
}

/// The source and transform stages without the kept documents.
fn without_kept(
    source: &ConfigValue,
    transformed: &ConfigValue,
    kept: &[HashSet<String>],
) -> (ConfigValue, ConfigValue) {
    if kept.iter().all(HashSet::is_empty) {
        return (source.clone(), transformed.clone());
    }
    let chunks = items(field(transformed, "chunks"));
    let mut groups = Vec::new();
    let mut sets = Vec::new();
    for (group, pair) in items(field(source, "groups")).iter().enumerate() {
        let pair = items(Some(pair));
        let keep = kept.get(group);
        let ids = items(chunks.get(group));
        let mut documents = Vec::new();
        let mut chosen = Vec::new();
        for (i, document) in items(pair.get(1)).iter().enumerate() {
            let object_id = string_field(document, "objectId").unwrap_or_default();
            if keep.is_some_and(|keep| keep.contains(object_id)) {
                continue;
            }
            documents.push(document.clone());
            chosen.push(ids.get(i).cloned().unwrap_or(ConfigValue::Null));
        }
        groups.push(ConfigValue::Array(vec![
            pair.first().cloned().unwrap_or(ConfigValue::Null),
            ConfigValue::Array(documents),
        ]));
        sets.push(ConfigValue::Array(chosen));
    }
    let replace = |value: &ConfigValue, name: &str, items: Vec<ConfigValue>| {
        let mut entries = match value {
            ConfigValue::Object(entries) => entries.clone(),
            _ => Vec::new(),
        };
        assign(&mut entries, name, Some(ConfigValue::Array(items)));
        ConfigValue::Object(entries)
    };
    (
        replace(source, "groups", groups),
        replace(transformed, "chunks", sets),
    )
}

fn read_manifest(connection: &Connection, id: &str) -> Result<Option<ConfigValue>, StageError> {
    let manifest: Option<String> = connection
        .query_row("SELECT manifest FROM builds WHERE id = ?", [id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(sql)?;
    manifest
        .map(|manifest| serde_json::from_str(&manifest).map_err(|_| json_error("A saved build")))
        .transpose()
}

/// `KnowledgeNest.inspectIndex(path)` is not null.
fn index_exists(path: &Path, cancel: &Cancellation) -> Result<bool, StageError> {
    Ok(KnowledgeBase::inspect_index(path, cancel)
        .map_err(StageError::native)?
        .is_some())
}

/// The documents of one source group, with their chunks, as `documentsRequest` sends them.
fn document_inputs(
    documents: &[ConfigValue],
    chunks: Option<&ConfigValue>,
) -> Result<Vec<DocumentInput>, StageError> {
    let chunks = items(chunks);
    documents
        .iter()
        .enumerate()
        .map(|(i, document)| {
            let mut input = Vec::new();
            for name in [
                "objectId",
                "url",
                "format",
                "content",
                "corpus",
                "modifiedAtMs",
            ] {
                assign(&mut input, name, document.get(name).cloned());
            }
            assign(&mut input, "chunks", chunks.get(i).cloned());
            serde_json::from_str(&ConfigValue::Object(input).to_js_json())
                .map_err(|_| StageError::native(crate::Error::Invalid("Invalid source documents")))
        })
        .collect()
}

/// `options.keyword` for `KnowledgeNest.open`, from the recorded keyword settings (`keywordOptions`).
pub(crate) fn keyword_options<'a>(
    keyword: Option<&ConfigValue>,
    snapshot: Option<&'a Path>,
) -> KeywordOptions<'a> {
    let mut options = KeywordOptions {
        ngrams: false,
        bigrams: false,
        japanese: false,
        user_dictionary: None,
    };
    let Some(keyword) = keyword else {
        return options;
    };
    match string_field(keyword, "tokenizer") {
        Some("lindera") => {
            options.japanese = true;
            options.user_dictionary = snapshot;
        }
        Some("word") => {}
        _ => {
            options.ngrams = true;
            options.bigrams = string_field(keyword, "short_query") == Some("bigram");
        }
    }
    options
}

/// Writes through a temporary file next to `path`, as `atomicWrite` does.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StageError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_nanos())
        .unwrap_or_default();
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}-{nanos}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| std::fs::rename(&temporary, path));
    let _ = std::fs::remove_file(&temporary);
    result.map_err(|error| io("Could not write the user dictionary snapshot", error))
}

/// Copies the keyword index of a build for incremental updates: Tantivy's completed segment files
/// as hard links, everything else as copies, without vector files or the hannoy graph.
fn copy_index_snapshot(source: &Path, target: &Path) -> Result<(), StageError> {
    let from = source.join(KEYWORD_DIR);
    let to = target.join(KEYWORD_DIR);
    let keep = |path: &Path| {
        let name = path.to_string_lossy();
        !name.ends_with("-shm")
            && !name.ends_with("vectors.json")
            && !name.ends_with("vectors.f32")
            && path.file_name() != Some(std::ffi::OsStr::new("hannoy"))
    };
    copy_tree(source, target, &|path| path != from.as_path() && keep(path))?;
    std::fs::create_dir(&to).map_err(|error| io("Could not copy the keyword index", error))?;
    let entries =
        std::fs::read_dir(&from).map_err(|error| io("Could not copy the keyword index", error))?;
    for entry in entries {
        let entry = entry.map_err(|error| io("Could not copy the keyword index", error))?;
        let input = entry.path();
        let output = to.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|error| io("Could not copy the keyword index", error))?;
        // Tantivy writes these completed segment components once. Metadata, delete bitsets, and
        // locks get their own copies.
        if kind.is_file()
            && segment_file(&entry.file_name().to_string_lossy())
            && std::fs::hard_link(&input, &output).is_ok()
        {
            continue;
        }
        copy_entry(&input, &output, &keep)?;
    }
    let permissions = std::fs::metadata(&from)
        .map_err(|error| io("Could not copy the keyword index", error))?
        .permissions();
    std::fs::set_permissions(&to, permissions)
        .map_err(|error| io("Could not copy the keyword index", error))
}

/// `^[0-9a-f]{32}\.(?:idx|pos|term|store|fast|fieldnorm)$`
fn segment_file(name: &str) -> bool {
    let Some((stem, extension)) = name.split_once('.') else {
        return false;
    };
    stem.len() == 32
        && stem
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        && ["idx", "pos", "term", "store", "fast", "fieldnorm"].contains(&extension)
}

/// `cp(source, target, { recursive: true, filter })`
fn copy_entry(
    source: &Path,
    target: &Path,
    keep: &dyn Fn(&Path) -> bool,
) -> Result<(), StageError> {
    if !keep(source) {
        return Ok(());
    }
    let metadata =
        std::fs::metadata(source).map_err(|error| io("Could not copy the index", error))?;
    if metadata.is_dir() {
        copy_tree(source, target, keep)
    } else {
        std::fs::copy(source, target)
            .map(|_| ())
            .map_err(|error| io("Could not copy the index", error))
    }
}

fn copy_tree(source: &Path, target: &Path, keep: &dyn Fn(&Path) -> bool) -> Result<(), StageError> {
    std::fs::create_dir_all(target).map_err(|error| io("Could not copy the index", error))?;
    let entries =
        std::fs::read_dir(source).map_err(|error| io("Could not copy the index", error))?;
    for entry in entries {
        let entry = entry.map_err(|error| io("Could not copy the index", error))?;
        copy_entry(&entry.path(), &target.join(entry.file_name()), keep)?;
    }
    Ok(())
}

/// Removes a build directory that was not recorded, only when it is this run's directory under the
/// project.
fn remove_unrecorded(root: &str, location: &Path, run_id: &str) {
    let (Ok(root), Ok(target)) = (std::fs::canonicalize(root), std::fs::canonicalize(location))
    else {
        return;
    };
    if target != root.join(".knowledge-nest").join("artifacts").join(run_id) {
        return;
    }
    for attempt in 0..4 {
        if std::fs::remove_dir_all(&target).is_ok() || !target.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
    }
}

pub fn run(
    request: &IndexRequest,
    host: &dyn IndexHost,
    cancel: &Cancellation,
) -> Result<IndexOutcome, StageError> {
    let stores = crate::stage_store::open(&request.state, &request.corpus, cancel)?;
    let mut connection = stores.state;
    let corpus = stores.corpus;
    // The documents and chunks are read from the corpus only when the build is not reused.
    let transformed = transform_manifest(&connection, &request.transform_id)?.ok_or_else(|| {
        StageError::coded(
            "STAGE_REQUIRED",
            format!(
                "Missing saved transform {}. Run nest build.",
                request.transform_id
            ),
            3,
        )
    })?;
    let source = string_field(&transformed, "source_id")
        .map(|id| read_source(&connection, id))
        .transpose()?
        .flatten()
        .ok_or_else(|| {
            StageError::coded(
                "STAGE_REQUIRED",
                "The source output referenced by transform is missing. Run nest build.",
                3,
            )
        })?;
    let embedder = match (&request.vector, &request.embedder) {
        (Some(_), Some(settings)) => Some(settings),
        (Some(_), None) => {
            return Err(StageError::coded(
                "VECTOR_MODEL_REQUIRED",
                "Define an embedding model under models.",
                2,
            ));
        }
        (None, _) => None,
    };
    let dictionary_path = request
        .keyword
        .as_ref()
        .and_then(|keyword| string_field(keyword, "user_dictionary"))
        .filter(|path| !path.is_empty());
    let dictionary = dictionary_path
        .map(|path| {
            std::fs::read(path)
                .map(|bytes| {
                    let hash = sha256_hex(&bytes);
                    (bytes, hash)
                })
                .map_err(|_| {
                    StageError::coded(
                        "USER_DICTIONARY_REQUIRED",
                        format!("Could not read the user dictionary CSV at {path}"),
                        2,
                    )
                })
        })
        .transpose()?;
    let japanese = request
        .keyword
        .as_ref()
        .and_then(|keyword| string_field(keyword, "tokenizer"))
        == Some("lindera");
    let keyword = if japanese {
        Some(object(vec![
            ("tokenizer", text("lindera")),
            (
                "dictionary_hash",
                dictionary
                    .as_ref()
                    .map_or(ConfigValue::Null, |(_, hash)| text(hash)),
            ),
        ]))
    } else {
        request.keyword.clone()
    };
    let mut parts = vec![
        text(if japanese {
            "index.lindera-ipadic-v1"
        } else {
            "index.keyword-v2"
        }),
        keyword.clone().unwrap_or(ConfigValue::Null),
    ];
    if let (Some(settings), Some(vector)) = (embedder, &request.vector) {
        parts.extend([text("index.vector-v1"), vector.clone(), settings.identity()]);
    }
    let index_config_id = digest("cfg", &ConfigValue::Array(parts));
    let config_id = digest(
        "cfg",
        &ConfigValue::Array(vec![
            field(&source, "config_id")
                .cloned()
                .unwrap_or(ConfigValue::Null),
            field(&transformed, "config_id")
                .cloned()
                .unwrap_or(ConfigValue::Null),
            text(&index_config_id),
            request.search.clone(),
            request.rerank.clone(),
        ]),
    );
    let build_key = digest(
        "bld",
        &ConfigValue::Array(vec![
            field(&transformed, "id")
                .cloned()
                .unwrap_or(ConfigValue::Null),
            text(&index_config_id),
        ]),
    );
    let cached: Option<String> = if embedder.is_some() {
        connection
            .query_row(
                "SELECT build_id FROM index_outputs WHERE key = ?",
                [&build_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?
    } else {
        None
    };
    let mut build_id = cached.unwrap_or_else(|| build_key.clone());
    let saved = read_manifest(&connection, &build_id)?;
    let backend = request
        .vector
        .as_ref()
        .map(|vector| match string_field(vector, "backend") {
            Some("exact") => Ok(Backend::Exact),
            Some("hannoy") => Ok(Backend::Hannoy),
            _ => Err(StageError::coded(
                "VECTOR_BACKEND_MISMATCH",
                "Unknown vector index backend.",
                2,
            )),
        })
        .transpose()?;
    let artifacts = Path::new(&request.artifacts);
    let reused = !request.force
        && match saved
            .as_ref()
            .and_then(|manifest| string_field(manifest, "index"))
        {
            Some(index) => {
                let directory = artifacts.join(index);
                index_exists(&directory, cancel)?
                    && (embedder.is_none()
                        || directory.join("vectors.json").exists()
                            && (saved
                                .as_ref()
                                .and_then(|manifest| field(manifest, "vector"))
                                .and_then(|vector| field(vector, "count"))
                                == Some(&ConfigValue::Number(0.0))
                                || backend.is_some_and(|backend| {
                                    directory.join(backend.file()).exists()
                                })))
            }
            None => false,
        };
    let previous: Option<String> = connection
        .query_row("SELECT build_id FROM project WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(sql)?
        .flatten();
    let search = request.search.to_js_json();
    let configuration = request.configuration.to_js_json();
    let tag = request.tag.as_deref().filter(|tag| !tag.is_empty());
    if let Some(tag) = tag {
        let existing: Option<String> = connection
            .query_row(
                "SELECT build_id FROM build_tags WHERE name = ?",
                [tag],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?;
        let settings: Option<String> = connection
            .query_row(
                "SELECT settings FROM tag_search WHERE name = ?",
                [tag],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?;
        if existing.is_some_and(|existing| {
            settings.as_deref() != Some(search.as_str()) || reused && existing != build_id
        }) {
            return Err(tag_exists(&format!(
                "Build tag already names a different configuration: {tag}"
            )));
        }
    }
    let location = artifacts.join(&request.run_id);
    let mut created = false;
    let mut warnings = Vec::new();
    let mut changed = false;
    let result = (|| -> Result<ConfigValue, StageError> {
        if let Some((bytes, hash)) = &dictionary {
            let snapshot = artifacts.join("dictionaries").join(format!("{hash}.csv"));
            std::fs::create_dir_all(artifacts.join("dictionaries"))
                .map_err(|error| io("Could not save the user dictionary snapshot", error))?;
            if !snapshot.exists() {
                atomic_write(&snapshot, bytes)?;
            }
        }
        let manifest = match &saved {
            Some(manifest) if reused => manifest.clone(),
            _ => {
                std::fs::create_dir(&location)
                    .map_err(|error| io("Could not create the build directory", error))?;
                created = true;
                let prior = previous
                    .as_deref()
                    .map(|id| read_manifest(&connection, id))
                    .transpose()?
                    .flatten();
                let prior_index = prior
                    .as_ref()
                    .and_then(|prior| string_field(prior, "index"))
                    .map(|index| artifacts.join(index));
                let baseline = match (&prior, &prior_index) {
                    (Some(prior), Some(directory))
                        if !request.force
                            && truthy(field(prior, "shared_corpus"))
                            && js_json(field(prior, "keyword")) == js_json(keyword.as_ref()) =>
                    {
                        KnowledgeBase::inspect_index(directory, cancel)
                            .map_err(StageError::native)?
                    }
                    _ => None,
                };
                if baseline.is_some() {
                    copy_index_snapshot(prior_index.as_deref().unwrap_or(artifacts), &location)?;
                }
                let snapshot = keyword
                    .as_ref()
                    .and_then(|keyword| string_field(keyword, "dictionary_hash"))
                    .map(|hash| artifacts.join("dictionaries").join(format!("{hash}.csv")));
                let vector = {
                    let database = KnowledgeBase::open_with_options(
                        &location,
                        CorpusBackend::Sqlite,
                        Some(Path::new(&request.corpus)),
                        keyword_options(keyword.as_ref(), snapshot.as_deref()),
                        cancel,
                    )
                    .map_err(|error| match (&error, dictionary_path) {
                        (crate::Error::Invalid(_), Some(path)) if dictionary.is_some() => {
                            StageError::coded(
                                "INVALID_USER_DICTIONARY",
                                format!("Could not load {path}. {error}"),
                                2,
                            )
                        }
                        _ => StageError::native(error),
                    })?;
                    // Documents whose version and chunk set match the build the index was copied
                    // from stay in it unread. Vectors are written for every chunk, so they read all.
                    let kept = if baseline.is_some() && embedder.is_none() {
                        unchanged_documents(&connection, prior.as_ref(), &source, &transformed)?
                    } else {
                        Vec::new()
                    };
                    let (source, transformed) = without_kept(&source, &transformed, &kept);
                    let source = crate::stage_store::expand_source(&corpus, source)?;
                    let mut transformed =
                        crate::stage_store::expand_transform(&corpus, transformed)?;
                    crate::stage_store::restore_transform(&source, &mut transformed)?;
                    let groups = items(field(&source, "groups"));
                    let chunks = items(field(&transformed, "chunks"));
                    let mut unused: Vec<String> = baseline
                        .as_ref()
                        .map(|status| status.sources.keys().cloned().collect())
                        .unwrap_or_default();
                    for (group, pair) in groups.iter().enumerate() {
                        cancel.check().map_err(StageError::native)?;
                        let pair = items(Some(pair));
                        let id = pair
                            .first()
                            .and_then(ConfigValue::as_str)
                            .unwrap_or_default();
                        let input = document_inputs(items(pair.get(1)), chunks.get(group))?;
                        if baseline.is_some() {
                            database
                                .sync_document_inputs(
                                    id,
                                    input,
                                    kept.get(group).unwrap_or(&HashSet::new()),
                                    cancel,
                                )
                                .map_err(StageError::native)?;
                        } else {
                            database
                                .replace_document_inputs(id, input, cancel)
                                .map_err(StageError::native)?;
                        }
                        unused.retain(|source| source != id);
                    }
                    for id in &unused {
                        database
                            .remove_source(id, cancel)
                            .map_err(StageError::native)?;
                    }
                    match (embedder, &request.vector, backend) {
                        (Some(settings), Some(vector), Some(backend)) => {
                            let args = field(vector, "settings");
                            let ef_construction =
                                args.and_then(|args| field(args, "ef_construction"));
                            let compatible = !request.force
                                && prior
                                    .as_ref()
                                    .and_then(|prior| field(prior, "vector"))
                                    .is_some_and(|built| {
                                        field(built, "backend") == field(vector, "backend")
                                            && field(built, "ef_construction") == ef_construction
                                    });
                            let mut output = build_vectors(&VectorBuild {
                                connection: &connection,
                                database: &database,
                                location: &location,
                                source: &source,
                                chunks,
                                embedder: settings,
                                force: request.force,
                                backend,
                                ef_construction,
                                baseline: if compatible {
                                    prior_index.as_deref()
                                } else {
                                    None
                                },
                                host,
                                cancel,
                            })?;
                            if let ConfigValue::Object(entries) = &mut output {
                                assign(entries, "ef_construction", ef_construction.cloned());
                            }
                            Some(output)
                        }
                        _ => None,
                    }
                };
                let documents = KnowledgeBase::inspect_index(&location, cancel)
                    .map_err(StageError::native)?
                    .map_or(0, |status| status.searchable_documents);
                if documents == 0 {
                    let mut data = Vec::new();
                    assign(&mut data, "files", field(&source, "files").cloned());
                    assign(&mut data, "documents", Some(number(0)));
                    assign(&mut data, "skipped", field(&source, "skipped").cloned());
                    let mut error = StageError::coded(
                        "NO_SEARCHABLE_DOCUMENTS",
                        "No searchable documents. Add UTF-8 text or Markdown files and run nest build.",
                        3,
                    );
                    error.data = Some(ConfigValue::Object(data));
                    return Err(error);
                }
                if let Some(output) = vector.as_ref().and_then(|vector| field(vector, "output")) {
                    build_id = digest(
                        "bld",
                        &ConfigValue::Array(vec![text(&build_key), output.clone()]),
                    );
                }
                let mut manifest = Vec::new();
                assign(&mut manifest, "build_id", Some(text(&build_id)));
                assign(&mut manifest, "index", Some(text(&request.run_id)));
                assign(&mut manifest, "files", field(&source, "files").cloned());
                assign(&mut manifest, "documents", Some(number(documents)));
                assign(&mut manifest, "skipped", field(&source, "skipped").cloned());
                assign(&mut manifest, "source_id", field(&source, "id").cloned());
                assign(
                    &mut manifest,
                    "transform_id",
                    field(&transformed, "id").cloned(),
                );
                assign(&mut manifest, "keyword", keyword.clone());
                assign(&mut manifest, "vector", vector);
                assign(&mut manifest, "config_id", Some(text(&config_id)));
                let mut construction = Vec::new();
                assign(
                    &mut construction,
                    "source",
                    field(&source, "modules").cloned(),
                );
                assign(
                    &mut construction,
                    "transform",
                    field(&transformed, "modules").cloned(),
                );
                assign(
                    &mut construction,
                    "index",
                    Some(request.construction.clone()),
                );
                assign(
                    &mut manifest,
                    "construction",
                    Some(ConfigValue::Object(construction)),
                );
                assign(
                    &mut manifest,
                    "shared_corpus",
                    Some(ConfigValue::Bool(true)),
                );
                ConfigValue::Object(manifest)
            }
        };
        cancel.check().map_err(StageError::native)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        let current_tag: Option<(String, String)> = match tag {
            Some(tag) => transaction
                .query_row(
                    "SELECT build_id, settings FROM build_tags JOIN tag_search USING (name) WHERE name = ?",
                    [tag],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(sql)?,
            None => None,
        };
        if let (Some(tag), Some((tagged, settings))) = (tag, &current_tag)
            && (*tagged != build_id || *settings != search)
        {
            return Err(tag_exists(&format!(
                "Build tag already names a different configuration: {tag}"
            )));
        }
        if !reused {
            transaction
                .execute(
                    "INSERT INTO builds VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET manifest = excluded.manifest",
                    params![build_id, manifest.to_js_json()],
                )
                .map_err(sql)?;
        }
        if embedder.is_some() {
            transaction
                .execute(
                    "INSERT OR REPLACE INTO index_outputs VALUES (?, ?)",
                    params![build_key, build_id],
                )
                .map_err(sql)?;
        }
        if request.activate {
            let generation: Option<i64> = transaction
                .query_row("SELECT max(id) FROM adoption_history", [], |row| row.get(0))
                .map_err(sql)?;
            if generation != request.generation
                || host.config_changed().map_err(StageError::plain)?
            {
                warnings.push(object(vec![
                    ("code", text("ADOPTION_CHANGED")),
                    (
                        "message",
                        text("The adopted configuration changed during the build. The serving build was kept."),
                    ),
                ]));
            } else {
                transaction
                    .execute("UPDATE project SET build_id = ? WHERE id = 1", [&build_id])
                    .map_err(sql)?;
                transaction
                    .execute(
                        "INSERT INTO serving_generations VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET build_id = excluded.build_id",
                        params![request.generation.unwrap_or(0), build_id],
                    )
                    .map_err(sql)?;
                changed = previous.as_deref() != Some(build_id.as_str()) || !reused;
            }
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO build_search VALUES (?, ?)",
                params![build_id, search],
            )
            .map_err(sql)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO build_config VALUES (?, ?)",
                params![build_id, configuration],
            )
            .map_err(sql)?;
        if let (Some(tag), None) = (tag, &current_tag) {
            transaction
                .execute(
                    "INSERT INTO build_tags VALUES (?, ?)",
                    params![tag, build_id],
                )
                .map_err(sql)?;
            transaction
                .execute("INSERT INTO tag_search VALUES (?, ?)", params![tag, search])
                .map_err(sql)?;
            transaction
                .execute(
                    "INSERT INTO build_config VALUES (?, ?)",
                    params![format!("tag:{tag}"), configuration],
                )
                .map_err(sql)?;
            let target = object(vec![
                ("build_id", text(&build_id)),
                ("search", request.search.clone()),
            ]);
            transaction
                .execute(
                    "INSERT INTO tag_history (name, previous, target) VALUES (?, ?, ?)",
                    params![tag, Option::<String>::None, target.to_js_json()],
                )
                .map_err(sql)?;
        }
        transaction
            .execute(
                "INSERT INTO build_runs VALUES (?, ?, ?, ?)",
                params![request.run_id, build_id, config_id, iso_now()],
            )
            .map_err(sql)?;
        transaction.commit().map_err(sql)?;
        Ok(manifest)
    })();
    let manifest = match result {
        Ok(manifest) => manifest,
        Err(error) => {
            if created {
                remove_unrecorded(&request.root, &location, &request.run_id);
            }
            return Err(error);
        }
    };
    let ConfigValue::Object(entries) = manifest else {
        return Err(StageError::plain("Invalid build manifest."));
    };
    let mut summary: Vec<_> = entries
        .into_iter()
        .filter(|(name, _)| name != "index")
        .collect();
    assign(&mut summary, "config_id", Some(text(&config_id)));
    assign(
        &mut summary,
        "serving_changed",
        Some(ConfigValue::Bool(changed)),
    );
    assign(&mut summary, "reused", Some(ConfigValue::Bool(reused)));
    Ok(IndexOutcome {
        summary: ConfigValue::Object(summary),
        warnings,
    })
}

#[derive(Clone, Copy)]
enum Backend {
    Exact,
    Hannoy,
}

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Hannoy => "hannoy",
        }
    }

    /// The file that holds the vectors of a completed build.
    fn file(self) -> PathBuf {
        match self {
            Self::Exact => PathBuf::from("vectors.f32"),
            Self::Hannoy => Path::new("hannoy").join("data.mdb"),
        }
    }
}

struct VectorBuild<'a> {
    connection: &'a Connection,
    database: &'a KnowledgeBase,
    location: &'a Path,
    source: &'a ConfigValue,
    chunks: &'a [ConfigValue],
    embedder: &'a EmbedderSettings,
    force: bool,
    backend: Backend,
    ef_construction: Option<&'a ConfigValue>,
    /// The previous build directory, when its hannoy graph may be updated in place.
    baseline: Option<&'a Path>,
    host: &'a dyn IndexHost,
    cancel: &'a Cancellation,
}

/// Embeds every chunk (through the `embeddings` cache), writes `vectors.json` and the backend's
/// files, and returns the `vector` field of the build manifest.
fn build_vectors(build: &VectorBuild<'_>) -> Result<ConfigValue, StageError> {
    let identity = sha256_hex(build.embedder.identity().to_js_json().as_bytes());
    let mut entries = Vec::new();
    let mut keys = Vec::new();
    let mut pending: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for (group, pair) in items(field(build.source, "groups")).iter().enumerate() {
        let pair = items(Some(pair));
        let source_id = pair
            .first()
            .and_then(ConfigValue::as_str)
            .unwrap_or_default();
        let chunks = items(build.chunks.get(group));
        for (i, document) in items(pair.get(1)).iter().enumerate() {
            build.cancel.check().map_err(StageError::native)?;
            let object_id = string_field(document, "objectId").unwrap_or_default();
            let current = build
                .database
                .get_document(
                    &DocumentRef {
                        source_id: source_id.to_owned(),
                        object_id: object_id.to_owned(),
                    },
                    build.cancel,
                )
                .map_err(StageError::native)?;
            let Some(current) = current else {
                continue;
            };
            // The evidence as the JavaScript API returned it: through serde_json's map order.
            let evidence = serde_json::to_value(&current.evidence)
                .ok()
                .and_then(|value| serde_json::to_string(&value).ok())
                .and_then(|json| serde_json::from_str::<ConfigValue>(&json).ok())
                .ok_or_else(|| StageError::plain("Could not read the document evidence."))?;
            let content = string_field(document, "content")
                .unwrap_or_default()
                .as_bytes();
            for chunk in items(chunks.get(i)) {
                let text = match string_field(chunk, "searchText") {
                    Some(text) => text.to_owned(),
                    None => {
                        let range = field(chunk, "byteRange");
                        let bound = |name: &str| match range.and_then(|range| field(range, name)) {
                            Some(ConfigValue::Number(value)) if *value > 0.0 => {
                                (*value as usize).min(content.len())
                            }
                            _ => 0,
                        };
                        let (start, end) = (bound("start"), bound("end"));
                        String::from_utf8_lossy(&content[start.min(end)..end]).into_owned()
                    }
                };
                if js_blank(&text) {
                    continue;
                }
                let key = sha256_hex(
                    ConfigValue::Array(vec![self::text(&identity), self::text(&text)])
                        .to_js_json()
                        .as_bytes(),
                );
                if seen.insert(key.clone()) {
                    pending.push((key.clone(), text));
                }
                let title = match field(chunk, "heading") {
                    heading @ Some(_) if truthy(heading) => heading.cloned(),
                    _ => Some(self::text(object_id)),
                };
                let mut evidence = evidence.clone();
                if let ConfigValue::Object(fields) = &mut evidence {
                    assign(fields, "range", field(chunk, "range").cloned());
                    assign(fields, "byteRange", field(chunk, "byteRange").cloned());
                }
                let mut entry = Vec::new();
                assign(&mut entry, "title", title);
                assign(&mut entry, "evidence", Some(evidence));
                assign(&mut entry, "key", Some(self::text(&key)));
                entries.push(entry);
                keys.push(key);
            }
        }
    }
    let mut vectors: HashMap<String, Vec<u8>> = HashMap::new();
    let mut missing = Vec::new();
    {
        let mut lookup = build
            .connection
            .prepare("SELECT data FROM embeddings WHERE id = ?")
            .map_err(sql)?;
        for (key, text) in &pending {
            let cached: Option<Vec<u8>> = if build.force {
                None
            } else {
                lookup
                    .query_row([key], |row| row.get(0))
                    .optional()
                    .map_err(sql)?
            };
            match cached {
                Some(bytes) => {
                    vectors.insert(key.clone(), bytes);
                }
                None => missing.push((key, text)),
            }
        }
    }
    let embedder = Embedder::new(build.embedder.clone());
    let save = if build.force {
        "INSERT OR REPLACE INTO embeddings VALUES (?, ?)"
    } else {
        "INSERT OR IGNORE INTO embeddings VALUES (?, ?)"
    };
    for batch in missing.chunks(EMBED_BATCH) {
        let input: Vec<String> = batch.iter().map(|(_, text)| (*text).clone()).collect();
        let values = embedder.embed(&input, build.host, build.cancel)?;
        let mut statement = build.connection.prepare(save).map_err(sql)?;
        for ((key, _), vector) in batch.iter().zip(values) {
            let bytes: Vec<u8> = vector
                .iter()
                .flat_map(|value| (*value as f32).to_le_bytes())
                .collect();
            statement.execute(params![key, bytes]).map_err(sql)?;
            vectors.insert((*key).clone(), bytes);
        }
    }
    let width = vectors.values().next().map_or(0, Vec::len);
    if vectors.values().any(|vector| vector.len() != width) {
        return Err(StageError::coded(
            "VECTOR_DIMENSIONS",
            "Embedding dimensions differ. Choose a fixed version of the model and rebuild.",
            2,
        ));
    }
    let mut bytes = Vec::with_capacity(keys.len() * width);
    for key in &keys {
        bytes.extend_from_slice(&vectors[key]);
    }
    let dimensions = ConfigValue::Number(width as f64 / 4.0);
    let mut metadata = vec![
        ("identity".to_owned(), text(&identity)),
        ("dimensions".to_owned(), dimensions.clone()),
    ];
    let stats = match build.backend {
        Backend::Exact => {
            std::fs::write(build.location.join("vectors.f32"), &bytes)
                .map_err(|error| io("Could not write the vector index", error))?;
            vec![("indexed", number(entries.len())), ("removed", number(0))]
        }
        Backend::Hannoy => hannoy_build(
            build,
            &identity,
            &dimensions,
            &mut entries,
            &mut metadata,
            &bytes,
        )?,
    };
    let count = entries.len();
    metadata.insert(
        2,
        (
            "entries".to_owned(),
            ConfigValue::Array(entries.into_iter().map(ConfigValue::Object).collect()),
        ),
    );
    std::fs::write(
        build.location.join("vectors.json"),
        ConfigValue::Object(metadata).to_js_json(),
    )
    .map_err(|error| io("Could not write the vector index", error))?;
    let mut output = vec![
        ("identity".to_owned(), text(&identity)),
        ("backend".to_owned(), text(build.backend.name())),
        ("count".to_owned(), number(count)),
        ("output".to_owned(), text(&sha256_hex(&bytes))),
    ];
    for (name, value) in stats {
        assign(&mut output, name, Some(value));
    }
    Ok(ConfigValue::Object(output))
}

/// Updates the hannoy graph of the baseline build, or builds a new one, with the installed tool.
/// Gives each entry its `slot` and graph `id`, and sets `next_id` in the metadata.
fn hannoy_build(
    build: &VectorBuild<'_>,
    identity: &str,
    dimensions: &ConfigValue,
    entries: &mut [Vec<(String, ConfigValue)>],
    metadata: &mut Vec<(String, ConfigValue)>,
    bytes: &[u8],
) -> Result<Vec<(&'static str, ConfigValue)>, StageError> {
    if entries.is_empty() {
        return Ok(vec![("indexed", number(0)), ("removed", number(0))]);
    }
    let location = build.location.join("hannoy");
    std::fs::create_dir(&location)
        .map_err(|error| io("Could not create the hannoy index directory", error))?;
    let tool = build.host.index_tool().map_err(StageError::plain)?;
    let old = compatible_graph(build.baseline, dimensions, identity)?;
    if let (Some(_), Some(baseline)) = (&old, build.baseline) {
        std::fs::copy(
            baseline.join("hannoy").join("data.mdb"),
            location.join("data.mdb"),
        )
        .map_err(|error| io("Could not copy the hannoy index", error))?;
    }
    // `new Map(old.entries.map(({ slot, id }) => [slot, id]))`
    let mut prior: Vec<(ConfigValue, ConfigValue)> = Vec::new();
    for entry in items(old.as_ref().and_then(|old| field(old, "entries"))) {
        let slot = field(entry, "slot").cloned().unwrap_or(ConfigValue::Null);
        let id = field(entry, "id").cloned().unwrap_or(ConfigValue::Null);
        match prior.iter_mut().find(|(key, _)| *key == slot) {
            Some(existing) => existing.1 = id,
            None => prior.push((slot, id)),
        }
    }
    let mut next = match old.as_ref().and_then(|old| field(old, "next_id")) {
        Some(ConfigValue::Number(next)) => *next,
        _ => 0.0,
    };
    let mut inserts = Vec::new();
    let mut occurrences: HashMap<String, usize> = HashMap::new();
    for (row, entry) in entries.iter_mut().enumerate() {
        let source = entry
            .iter()
            .find(|(name, _)| name == "evidence")
            .and_then(|(_, evidence)| field(evidence, "source"))
            .cloned();
        let part = |name: &str| {
            source
                .as_ref()
                .and_then(|source| field(source, name))
                .cloned()
                .unwrap_or(ConfigValue::Null)
        };
        let key = entry
            .iter()
            .find(|(name, _)| name == "key")
            .map_or(ConfigValue::Null, |(_, key)| key.clone());
        let group = ConfigValue::Array(vec![part("sourceId"), part("objectId"), key]).to_js_json();
        let occurrence = occurrences.entry(group.clone()).or_insert(0);
        let slot = sha256_hex(
            ConfigValue::Array(vec![text(&group), number(*occurrence)])
                .to_js_json()
                .as_bytes(),
        );
        *occurrence += 1;
        let slot = text(&slot);
        assign(entry, "slot", Some(slot.clone()));
        match prior.iter().position(|(key, _)| *key == slot) {
            Some(index) => {
                let (_, id) = prior.remove(index);
                assign(entry, "id", Some(id));
            }
            None => {
                assign(entry, "id", Some(ConfigValue::Number(next)));
                inserts.push(object(vec![
                    ("id", ConfigValue::Number(next)),
                    ("row", number(row)),
                ]));
                next += 1.0;
            }
        }
    }
    metadata.push(("next_id".to_owned(), ConfigValue::Number(next)));
    let file = build.location.join("vectors.f32");
    std::fs::write(&file, bytes).map_err(|error| io("Could not write the vector index", error))?;
    let mut request = vec![
        ("operation", text("build")),
        ("directory", text(&location.to_string_lossy())),
        ("vectors", text(&file.to_string_lossy())),
        ("dimensions", dimensions.clone()),
        ("rows", number(entries.len())),
        ("inserts", ConfigValue::Array(inserts.clone())),
        (
            "deletes",
            ConfigValue::Array(prior.iter().map(|(_, id)| id.clone()).collect()),
        ),
    ];
    if let Some(ef_construction) = build.ef_construction {
        request.push(("ef_construction", ef_construction.clone()));
    }
    let response = call_tool(&tool, &object(request).to_js_json(), "build", build.cancel);
    let _ = std::fs::remove_file(&file);
    let response = response?;
    let count = |name: &str| response.get(name).and_then(serde_json::Value::as_f64);
    let (inserted, deleted) = (count("inserted"), count("deleted"));
    if inserted != Some(inserts.len() as f64) || deleted != Some(prior.len() as f64) {
        return Err(StageError::coded(
            "VECTOR_BACKEND_MISMATCH",
            "The index tool reported an incomplete build.",
            2,
        ));
    }
    let inserted = inserted.unwrap_or_default();
    Ok(vec![
        ("indexed", ConfigValue::Number(inserted)),
        ("removed", ConfigValue::Number(deleted.unwrap_or_default())),
        (
            "reused",
            ConfigValue::Number(entries.len() as f64 - inserted),
        ),
        ("identity", text(identity)),
    ])
}

/// The baseline's `vectors.json` when its graph has the same model and dimensions.
fn compatible_graph(
    baseline: Option<&Path>,
    dimensions: &ConfigValue,
    identity: &str,
) -> Result<Option<ConfigValue>, StageError> {
    let Some(baseline) = baseline else {
        return Ok(None);
    };
    let json = std::fs::read_to_string(baseline.join("vectors.json"))
        .map_err(|error| io("Could not read the previous vector index", error))?;
    let old: ConfigValue =
        serde_json::from_str(&json).map_err(|_| json_error("The previous vector index"))?;
    let safe_integer = matches!(field(&old, "next_id"),
        Some(ConfigValue::Number(next)) if next.fract() == 0.0 && next.abs() <= 9_007_199_254_740_991.0);
    if field(&old, "dimensions") != Some(dimensions)
        || string_field(&old, "identity") != Some(identity)
        || !safe_integer
    {
        return Ok(None);
    }
    Ok(baseline
        .join("hannoy")
        .join("data.mdb")
        .is_file()
        .then_some(old))
}

/// Runs the index tool with a JSON request on standard input and reads its JSON response.
/// `operation` names the request in the failure message.
pub(crate) fn call_tool(
    path: &str,
    request: &str,
    operation: &str,
    cancel: &Cancellation,
) -> Result<serde_json::Value, StageError> {
    let failed = |code: &'static str, message: &str| StageError::coded(code, message, 1);
    cancel.check().map_err(StageError::native)?;
    let mut command = Command::new(path);
    command
        .env_clear()
        .envs(
            ["SystemRoot", "WINDIR", "TMP", "TEMP", "TMPDIR"]
                .into_iter()
                .filter_map(|key| std::env::var_os(key).map(|value| (key, value))),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .spawn()
        .map_err(|_| failed("TOOL_EXECUTION", "Could not start the index tool."))?;
    let writer = child.stdin.take().map(|mut pipe| {
        let request = request.as_bytes().to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&request);
        })
    });
    let too_large = Arc::new(AtomicBool::new(false));
    let reader = child.stdout.take().map(|mut pipe| {
        let too_large = too_large.clone();
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let mut buffer = [0; 65536];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) if output.len() + read > TOOL_OUTPUT_LIMIT => {
                        too_large.store(true, Ordering::SeqCst);
                        break;
                    }
                    Ok(read) => output.extend_from_slice(&buffer[..read]),
                }
            }
            output
        })
    });
    let mut stopped = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if cancel.check().is_err() {
            stopped = Some(StageError::aborted());
        } else if too_large.load(Ordering::SeqCst) {
            stopped = Some(failed(
                "TOOL_PROTOCOL",
                "The index tool response exceeded its size limit.",
            ));
        }
        if stopped.is_some() {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let output = reader
        .map(|reader| reader.join().unwrap_or_default())
        .unwrap_or_default();
    if let Some(error) = stopped {
        return Err(error);
    }
    if too_large.load(Ordering::SeqCst) {
        return Err(failed(
            "TOOL_PROTOCOL",
            "The index tool response exceeded its size limit.",
        ));
    }
    if !status.is_some_and(|status| status.success()) {
        return Err(failed(
            "TOOL_EXECUTION",
            &format!("The index tool failed during {operation}."),
        ));
    }
    serde_json::from_slice(&output)
        .map_err(|_| failed("TOOL_PROTOCOL", "The index tool returned invalid JSON."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::tests::serve;
    use crate::transform_stage::{TransformRequest, run as transform};

    const SCHEMA: &str = "CREATE TABLE stage_outputs (stage TEXT NOT NULL, id TEXT NOT NULL, manifest TEXT NOT NULL, PRIMARY KEY (stage, id));
        CREATE TABLE latest_stage (stage TEXT PRIMARY KEY, id TEXT NOT NULL);
        CREATE TABLE transform_outputs (key TEXT PRIMARY KEY, output_id TEXT NOT NULL);
        CREATE TABLE builds (id TEXT PRIMARY KEY, manifest TEXT NOT NULL);
        CREATE TABLE project (id INTEGER PRIMARY KEY CHECK (id = 1), build_id TEXT REFERENCES builds(id));
        INSERT INTO project VALUES (1, NULL);
        CREATE TABLE build_tags (name TEXT PRIMARY KEY, build_id TEXT NOT NULL);
        CREATE TABLE build_search (build_id TEXT PRIMARY KEY, settings TEXT NOT NULL);
        CREATE TABLE tag_search (name TEXT PRIMARY KEY, settings TEXT NOT NULL);
        CREATE TABLE build_config (ref TEXT PRIMARY KEY, config TEXT NOT NULL);
        CREATE TABLE adoption_history (id INTEGER PRIMARY KEY, previous TEXT NOT NULL, adopted TEXT NOT NULL);
        CREATE TABLE serving_generations (id INTEGER PRIMARY KEY, build_id TEXT NOT NULL);
        CREATE TABLE tag_history (id INTEGER PRIMARY KEY, name TEXT NOT NULL, previous TEXT, target TEXT NOT NULL);
        CREATE TABLE build_runs (id TEXT PRIMARY KEY, build_id TEXT NOT NULL, config_id TEXT NOT NULL, created TEXT NOT NULL);
        CREATE TABLE index_outputs (key TEXT PRIMARY KEY, build_id TEXT NOT NULL);
        CREATE TABLE embeddings (id TEXT PRIMARY KEY, data BLOB NOT NULL);";

    struct Host {
        changed: bool,
    }

    impl Charge for Host {
        fn charge(&self, _bytes: usize) -> Result<(), String> {
            Ok(())
        }
    }

    impl IndexHost for Host {
        fn config_changed(&self) -> Result<bool, String> {
            Ok(self.changed)
        }

        fn index_tool(&self) -> Result<String, String> {
            Err("No index tool in tests".into())
        }
    }

    /// A project with a recorded source and transform stage: returns its root and the transform ID.
    fn project(contents: &[&str]) -> (tempfile::TempDir, String) {
        let root = tempfile::tempdir().unwrap();
        let artifacts = root.path().join(".knowledge-nest").join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let state = root.path().join(".knowledge-nest").join("state.sqlite");
        let corpus = root.path().join(".knowledge-nest").join("corpus");
        Connection::open(&state)
            .unwrap()
            .execute_batch(SCHEMA)
            .unwrap();
        let stores = crate::stage_store::open(
            &state.to_string_lossy(),
            &corpus.to_string_lossy(),
            &Cancellation::default(),
        )
        .unwrap();
        let mut documents = Vec::new();
        for (i, content) in contents.iter().enumerate() {
            let document = object(vec![
                ("objectId", text(&format!("d{i}.md"))),
                ("url", text(&format!("file:///d{i}.md"))),
                ("content", text(content)),
                ("format", text("markdown")),
            ]);
            let (version, record) = crate::stage_store::revision("docs", &document).unwrap();
            stores
                .corpus
                .execute(
                    crate::stage_store::INSERT_REVISION,
                    params!["docs", format!("d{i}.md"), version, record],
                )
                .unwrap();
            documents.push(format!(
                "{{\"objectId\":\"d{i}.md\",\"version\":\"{version}\"}}"
            ));
        }
        let manifest = format!(
            "{{\"id\":\"src-1\",\"config_id\":\"cfg-s\",\"modules\":[],\"files\":{},\"skipped\":[],\"groups\":[[\"docs\",[{}]]]}}",
            contents.len(),
            documents.join(",")
        );
        stores
            .state
            .execute(
                "INSERT INTO stage_outputs VALUES ('source', 'src-1', ?)",
                [manifest],
            )
            .unwrap();
        let transformed = transform(
            &TransformRequest {
                state: state.to_string_lossy().into_owned(),
                corpus: corpus.to_string_lossy().into_owned(),
                source_id: "src-1".into(),
                force: false,
                modules: serde_json::from_str(
                    r#"[{"use":"chunk.length","version":1,"name":"chunk.length","with":{"size":40,"overlap":0}}]"#,
                )
                .unwrap(),
                embedder: None,
            },
            &Host { changed: false },
            &Cancellation::default(),
        )
        .unwrap();
        let id = transformed
            .output
            .get("id")
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        (root, id)
    }

    fn request(root: &Path, transform_id: &str, run_id: &str) -> IndexRequest {
        let data = root.join(".knowledge-nest");
        IndexRequest {
            state: data.join("state.sqlite").to_string_lossy().into_owned(),
            artifacts: data.join("artifacts").to_string_lossy().into_owned(),
            corpus: data.join("corpus").to_string_lossy().into_owned(),
            root: root.to_string_lossy().into_owned(),
            run_id: run_id.into(),
            transform_id: transform_id.into(),
            keyword: Some(serde_json::from_str(r#"{"tokenizer":"trigram"}"#).unwrap()),
            vector: None,
            embedder: None,
            search: serde_json::from_str(r#"[{"use":"retrieval.keyword"}]"#).unwrap(),
            rerank: ConfigValue::Array(Vec::new()),
            construction: ConfigValue::Array(Vec::new()),
            configuration: serde_json::from_str(r#"{"index":{}}"#).unwrap(),
            force: false,
            activate: true,
            tag: None,
            generation: None,
        }
    }

    fn summary_text(outcome: &IndexOutcome, name: &str) -> String {
        outcome.summary.get(name).unwrap().to_js_json()
    }

    #[test]
    fn builds_records_and_reuses_the_keyword_index() {
        let (root, transform_id) = project(&["# 点検\n電源を止めます。\n", "Calibration notes.\n"]);
        let cancel = Cancellation::default();
        let host = Host { changed: false };
        let first = run(
            &request(root.path(), &transform_id, "run-1"),
            &host,
            &cancel,
        )
        .unwrap();
        assert_eq!(summary_text(&first, "reused"), "false");
        assert_eq!(summary_text(&first, "serving_changed"), "true");
        assert_eq!(summary_text(&first, "documents"), "2");
        assert!(first.summary.get("index").is_none());
        let build_id = first
            .summary
            .get("build_id")
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        let artifacts = root.path().join(".knowledge-nest").join("artifacts");
        assert!(artifacts.join("run-1").join(KEYWORD_DIR).is_dir());
        let connection =
            Connection::open(root.path().join(".knowledge-nest").join("state.sqlite")).unwrap();
        let serving: String = connection
            .query_row("SELECT build_id FROM project", [], |row| row.get(0))
            .unwrap();
        assert_eq!(serving, build_id);

        // The same configuration reuses the build and leaves no directory for the run.
        let again = run(
            &request(root.path(), &transform_id, "run-2"),
            &host,
            &cancel,
        )
        .unwrap();
        assert_eq!(summary_text(&again, "reused"), "true");
        assert_eq!(summary_text(&again, "serving_changed"), "false");
        assert!(!artifacts.join("run-2").exists());
        let runs: i64 = connection
            .query_row("SELECT count(*) FROM build_runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(runs, 2);

        // A tag names the build and its search settings; other settings cannot reuse it.
        let mut tagged = request(root.path(), &transform_id, "run-3");
        tagged.tag = Some("v1".into());
        tagged.activate = false;
        run(&tagged, &host, &cancel).unwrap();
        let tag: String = connection
            .query_row(
                "SELECT build_id FROM build_tags WHERE name = 'v1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tag, build_id);
        let mut other = request(root.path(), &transform_id, "run-4");
        other.tag = Some("v1".into());
        other.search = ConfigValue::Array(Vec::new());
        let error = run(&other, &host, &cancel).err().unwrap();
        assert_eq!(error.code, Some("TAG_EXISTS"));

        // A changed configuration file keeps the serving build.
        let mut forced = request(root.path(), &transform_id, "run-5");
        forced.force = true;
        let kept = run(&forced, &Host { changed: true }, &cancel).unwrap();
        assert_eq!(summary_text(&kept, "serving_changed"), "false");
        assert_eq!(kept.warnings.len(), 1);
        assert!(artifacts.join("run-5").is_dir());
    }

    #[test]
    fn embeds_chunks_once_and_writes_the_exact_vector_index() {
        let text = "Alpha one.\nAlpha two.\nBeta three.\n";
        let (root, transform_id) = project(&[text, text]);
        let (base, bodies) = serve(|body| {
            let count = body["input"].as_array().unwrap().len();
            (
                200,
                serde_json::json!({ "embeddings": vec![[3.0, 4.0]; count] }).to_string(),
            )
        });
        let mut indexed = request(root.path(), &transform_id, "run-1");
        indexed.vector =
            Some(serde_json::from_str(r#"{"backend":"exact","settings":{"model":"e"}}"#).unwrap());
        indexed.embedder = Some(EmbedderSettings {
            provider: "ollama".into(),
            model: "e".into(),
            url: format!("{base}/api/embed"),
            api_key: None,
            max_input_bytes: None,
        });
        let cancel = Cancellation::default();
        let host = Host { changed: false };
        let first = run(&indexed, &host, &cancel).unwrap();
        let vector = first.summary.get("vector").unwrap();
        let count = match vector.get("count") {
            Some(ConfigValue::Number(count)) => *count as usize,
            _ => panic!("no count"),
        };
        // One chunk per document, with the same text.
        assert_eq!(count, 2);
        let directory = root
            .path()
            .join(".knowledge-nest")
            .join("artifacts")
            .join("run-1");
        assert_eq!(
            std::fs::read(directory.join("vectors.f32")).unwrap().len(),
            count * 2 * 4
        );
        let metadata: ConfigValue =
            serde_json::from_str(&std::fs::read_to_string(directory.join("vectors.json")).unwrap())
                .unwrap();
        assert_eq!(items(metadata.get("entries")).len(), count);
        assert_eq!(metadata.get("dimensions"), Some(&ConfigValue::Number(2.0)));
        // The two documents share their chunk texts, so one request embeds each text once.
        assert_eq!(bodies.lock().unwrap().len(), 1);

        // A forced build embeds again; the same vectors give the same build ID.
        indexed.run_id = "run-2".into();
        indexed.force = true;
        let again = run(&indexed, &host, &cancel).unwrap();
        assert_eq!(bodies.lock().unwrap().len(), 2);
        assert_eq!(again.summary.get("build_id"), first.summary.get("build_id"));
    }

    #[test]
    fn removes_the_build_directory_when_nothing_is_searchable() {
        let (root, transform_id) = project(&[]);
        let error = run(
            &request(root.path(), &transform_id, "run-1"),
            &Host { changed: false },
            &Cancellation::default(),
        )
        .err()
        .unwrap();
        assert_eq!(error.code, Some("NO_SEARCHABLE_DOCUMENTS"));
        assert_eq!(
            error.data.unwrap().to_js_json(),
            r#"{"files":0,"documents":0,"skipped":[]}"#
        );
        assert!(
            !root
                .path()
                .join(".knowledge-nest")
                .join("artifacts")
                .join("run-1")
                .exists()
        );
    }

    #[test]
    fn matches_tantivy_segment_files() {
        assert!(segment_file("0123456789abcdef0123456789abcdef.idx"));
        assert!(!segment_file("0123456789abcdef0123456789abcdef.del"));
        assert!(!segment_file("meta.json"));
    }
}
