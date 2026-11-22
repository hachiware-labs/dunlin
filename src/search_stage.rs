//! Search execution (spec 28.10 step 3a): `query.normalize`, `retrieval.keyword`,
//! `retrieval.vector`, `hannoy.search`, `fusion.rrf`, `rerank.model`, `context.assemble`, the
//! search configuration ID and the returned format of spec 27.1.
//!
//! Results, identifiers, messages and the JSON key order are the ones the JavaScript
//! implementation produced. Evidence keeps the key order it had in JavaScript, where it came from
//! `serde_json::Value` with sorted keys. `rerank.model` still calls the JavaScript host.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::config_value::ConfigValue;
use crate::embed::{Charge, Embedder, EmbedderSettings};
use crate::index_stage::{assign, digest, field, items, object, string_field, text, truthy};
use crate::source_load::sha256_hex;
use crate::transform_stage::StageError;
use crate::{Cancellation, Evidence, KnowledgeBase, RetrievalMethod};

/// What the search configuration ID covers: the plan, the request overrides and the models.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSettings {
    /// `moduleSettings(plan)` of the search stage.
    pub plan: ConfigValue,
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default)]
    pub budget_chars: Option<f64>,
    #[serde(default)]
    pub source_ids: Option<Vec<String>>,
    /// The build manifest.
    pub build: ConfigValue,
    /// `modelSetting` of each `rerank.model`, in plan order.
    #[serde(default)]
    pub rerank_models: Vec<ConfigValue>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    #[serde(flatten)]
    pub settings: SearchSettings,
    pub query: String,
    /// The build's index directory.
    pub directory: String,
    /// Embedding models by name: resolved settings, or `{ error: { code, message, exitCode } }`.
    #[serde(default)]
    pub embedders: ConfigValue,
    /// The chat models of `rerank.model` by name, in the same form.
    #[serde(default)]
    pub rerankers: ConfigValue,
}

/// What search asks of JavaScript: trial charges and the index tool.
pub trait SearchHost: Charge {
    /// The verified path of the hannoy index tool.
    fn index_tool(&self) -> Result<String, String>;
}

/// A failure, with the module it happened in.
#[derive(Debug)]
pub struct SearchFailure {
    pub error: StageError,
    pub module: Option<String>,
}

pub struct SearchOutcome {
    /// The returned format of spec 27.1.
    pub output: ConfigValue,
    /// The evidence of each context excerpt, which evaluation matches.
    pub context: Vec<ConfigValue>,
    /// `exhausted`, `candidate_limit`, `budget` or `no_truncation`.
    pub context_stop: &'static str,
}

/// `vectors.json` of an open index, read once.
#[derive(Default)]
pub struct VectorCache(Mutex<Option<Arc<ConfigValue>>>);

impl VectorCache {
    fn load(&self, directory: &Path) -> Result<Arc<ConfigValue>, StageError> {
        let mut slot = self
            .0
            .lock()
            .map_err(|_| StageError::plain("Could not read the vector index."))?;
        if let Some(loaded) = slot.as_ref() {
            return Ok(loaded.clone());
        }
        let path = directory.join("vectors.json");
        let bytes = std::fs::read(&path).map_err(|error| {
            StageError::plain(format!("Could not read {}: {error}", path.display()))
        })?;
        let loaded: Arc<ConfigValue> = Arc::new(
            serde_json::from_slice(&bytes)
                .map_err(|_| StageError::plain("The vector index metadata is not valid JSON."))?,
        );
        *slot = Some(loaded.clone());
        Ok(loaded)
    }
}

#[derive(Clone, Debug)]
struct Hit {
    title: String,
    score: f64,
    evidence: ConfigValue,
}

#[derive(Clone, Debug)]
struct Found {
    hits: Vec<Hit>,
    total: usize,
}

const RETRIEVAL: [&str; 3] = ["retrieval.keyword", "retrieval.vector", "hannoy.search"];
const VECTOR: [&str; 2] = ["retrieval.vector", "hannoy.search"];

fn number(value: Option<&ConfigValue>) -> f64 {
    match value {
        Some(ConfigValue::Number(value)) => *value,
        _ => f64::NAN,
    }
}

fn js_number(value: f64) -> ConfigValue {
    ConfigValue::Number(value)
}

/// `String.prototype.trim`.
fn js_trim(text: &str) -> &str {
    text.trim_matches(crate::registry::js_space)
}

/// A `KnowledgeNestError('INVALID_ARGUMENT', message)` of the JavaScript API: a code without an exit code.
fn argument(message: impl Into<String>) -> StageError {
    StageError {
        code: Some("INVALID_ARGUMENT"),
        message: message.into(),
        exit_code: None,
        aborted: false,
        data: None,
    }
}

/// A code that JavaScript reported for a model it could not resolve.
fn static_code(code: &str) -> &'static str {
    match code {
        "VECTOR_MODEL_REQUIRED" => "VECTOR_MODEL_REQUIRED",
        "INVALID_VECTOR_MODEL" => "INVALID_VECTOR_MODEL",
        "INVALID_MODEL" => "INVALID_MODEL",
        other => Box::leak(other.to_owned().into_boxed_str()),
    }
}

/// Evidence as JavaScript received it from the native API: keys sorted by `serde_json::Value`.
fn evidence_value(evidence: &Evidence) -> Result<ConfigValue, StageError> {
    sorted_value(evidence)
}

fn sorted_value(value: &impl serde::Serialize) -> Result<ConfigValue, StageError> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| serde_json::to_string(&value).ok())
        .and_then(|json| serde_json::from_str(&json).ok())
        .ok_or_else(|| StageError::plain("Could not encode the evidence."))
}

fn evidence_of(value: &ConfigValue) -> Result<Evidence, StageError> {
    serde_json::from_str(&value.to_js_json())
        .map_err(|_| argument("Expected source evidence from search"))
}

fn module_use(module: &ConfigValue) -> &str {
    string_field(module, "use").unwrap_or_default()
}

fn module_args(module: &ConfigValue) -> ConfigValue {
    field(module, "with").cloned().unwrap_or(ConfigValue::Null)
}

/// `assertSearchIndexes`: every retrieval module has its index in the build.
pub(crate) fn assert_indexes(build: &ConfigValue, plan: &[ConfigValue]) -> Result<(), StageError> {
    for module in plan {
        let name = module_use(module);
        let vector = field(build, "vector");
        if (VECTOR.contains(&name) && !truthy(vector))
            || (name == "retrieval.keyword" && !truthy(field(build, "keyword")))
        {
            return Err(StageError::coded(
                "INDEX_REQUIRED",
                format!("The selected build has no index for {name}."),
                3,
            ));
        }
        let required = match name {
            "retrieval.vector" => Some("exact"),
            "hannoy.search" => Some("hannoy"),
            _ => None,
        };
        if let Some(required) = required
            && vector
                .and_then(|vector| string_field(vector, "backend"))
                .unwrap_or("exact")
                != required
        {
            return Err(StageError::coded(
                "VECTOR_BACKEND_MISMATCH",
                "The selected build uses another vector index backend.",
                2,
            ));
        }
    }
    Ok(())
}

/// `[...a].sort()` on strings: UTF-16 code unit order.
fn utf16_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `searchConfigId(plan, options, build, project)`.
pub fn search_config_id(request: &SearchSettings, with_options: bool) -> String {
    let plan = items(Some(&request.plan));
    let mut options = Vec::new();
    if with_options {
        assign(&mut options, "limit", request.limit.map(js_number));
        assign(
            &mut options,
            "budgetChars",
            request.budget_chars.map(js_number),
        );
        if let Some(ids) = &request.source_ids {
            let mut unique: Vec<&String> = Vec::new();
            for id in ids {
                if !unique.contains(&id) {
                    unique.push(id);
                }
            }
            unique.sort_by(|a, b| utf16_order(a, b));
            options.push((
                "sourceIds".into(),
                ConfigValue::Array(unique.into_iter().map(|id| text(id)).collect()),
            ));
        }
    }
    let keyword = field(&request.build, "keyword");
    if keyword.and_then(|keyword| string_field(keyword, "tokenizer")) == Some("lindera")
        && plan
            .iter()
            .any(|module| module_use(module) == "retrieval.keyword")
    {
        options.push((
            "keyword".into(),
            ConfigValue::Array(vec![
                text("search.lindera-ipadic-v1"),
                keyword
                    .and_then(|keyword| field(keyword, "dictionary_hash"))
                    .cloned()
                    .unwrap_or(ConfigValue::Null),
            ]),
        ));
    }
    if plan
        .iter()
        .any(|module| VECTOR.contains(&module_use(module)))
    {
        assign(
            &mut options,
            "vector",
            field(&request.build, "vector")
                .and_then(|vector| field(vector, "identity"))
                .cloned(),
        );
    }
    if !request.rerank_models.is_empty() {
        options.push((
            "rerank".into(),
            ConfigValue::Array(request.rerank_models.clone()),
        ));
    }
    digest(
        "search",
        &ConfigValue::Array(vec![request.plan.clone(), ConfigValue::Object(options)]),
    )
}

/// Runs the search plan and assembles the returned format.
pub fn run(
    database: &KnowledgeBase,
    vectors: &VectorCache,
    request: &SearchRequest,
    host: &dyn SearchHost,
    cancel: &Cancellation,
) -> Result<SearchOutcome, SearchFailure> {
    let unattached = |error: StageError| SearchFailure {
        error,
        module: None,
    };
    let plan = items(Some(&request.settings.plan));
    assert_indexes(&request.settings.build, plan).map_err(unattached)?;
    let context_args = plan
        .iter()
        .find(|module| module_use(module) == "context.assemble")
        .map(module_args)
        .unwrap_or(ConfigValue::Null);
    let top = request.settings.limit.unwrap_or_else(|| {
        plan.iter()
            .filter(|module| RETRIEVAL.contains(&module_use(module)))
            .map(|module| number(field(&module_args(module), "top_k")))
            .fold(f64::NEG_INFINITY, f64::max)
    });
    let top_k_only = string_field(&context_args, "retrieval") == Some("top_k");
    let max = if top_k_only {
        top
    } else {
        top.max(number(field(&context_args, "max_candidates")))
    };
    let mut query = request.query.clone();
    let mut results: Vec<Found> = Vec::new();
    let mut current: Option<Found> = None;
    let mut assembled: Option<Assembled> = None;
    for module in plan {
        cancel
            .check()
            .map_err(|_| unattached(StageError::aborted()))?;
        let name = module_use(module);
        let args = module_args(module);
        // `limit ?? top_k`, or every candidate the context may use.
        let wanted = || {
            if top_k_only {
                request
                    .settings
                    .limit
                    .unwrap_or_else(|| number(field(&args, "top_k")))
            } else {
                max
            }
        };
        let step = (|| -> Result<(), StageError> {
            match name {
                "query.normalize" => {
                    let trimmed = js_trim(&query);
                    query = if string_field(&args, "unicode") == Some("nfkc") {
                        icu_normalizer::ComposingNormalizerBorrowed::new_nfkc()
                            .normalize(trimmed)
                            .into_owned()
                    } else {
                        trimmed.to_owned()
                    };
                }
                "retrieval.keyword" => {
                    let found = keyword(database, &query, wanted(), request, cancel)?;
                    results.push(found.clone());
                    current = Some(found);
                }
                "retrieval.vector" | "hannoy.search" => {
                    let backend = if name == "hannoy.search" {
                        "hannoy"
                    } else {
                        "exact"
                    };
                    let found = vector(
                        &VectorSearch {
                            vectors,
                            request,
                            query: &query,
                            args: &args,
                            backend,
                            max: wanted(),
                            host,
                        },
                        cancel,
                    )?;
                    results.push(found.clone());
                    current = Some(found);
                }
                "fusion.rrf" => current = Some(fuse(&results, number(field(&args, "k")), max)),
                "rerank.model" => {
                    let found = current.as_mut().ok_or_else(|| {
                        StageError::plain("rerank.model needs candidates from a retrieval module.")
                    })?;
                    rerank(database, found, &query, &args, request, host, cancel)?;
                }
                "context.assemble" => {
                    let found = current.as_ref().ok_or_else(|| {
                        StageError::plain(
                            "context.assemble needs candidates from a retrieval module.",
                        )
                    })?;
                    assembled = Some(assemble(
                        database,
                        found,
                        &args,
                        request.settings.budget_chars,
                        top,
                        max,
                        cancel,
                    )?);
                }
                other => {
                    return Err(StageError::plain(format!(
                        "The search stage cannot run {other}."
                    )));
                }
            }
            Ok(())
        })();
        step.map_err(|error| SearchFailure {
            error,
            module: Some(name.to_owned()),
        })?;
    }
    let assembled = assembled
        .ok_or_else(|| unattached(StageError::plain("The search stage has no context module.")))?;
    let output = output(request, &assembled).map_err(unattached)?;
    Ok(SearchOutcome {
        output,
        context: assembled
            .context
            .iter()
            .map(|item| item.evidence.clone())
            .collect(),
        context_stop: assembled.stop,
    })
}

fn keyword(
    database: &KnowledgeBase,
    query: &str,
    limit: f64,
    request: &SearchRequest,
    cancel: &Cancellation,
) -> Result<Found, StageError> {
    if js_trim(query).is_empty() {
        return Err(StageError::coded(
            "INVALID_QUERY",
            "Enter a search query.",
            2,
        ));
    }
    // The checks of the JavaScript `search`, in its order.
    if query.len() > 4096 {
        return Err(argument("Expected a query of at most 4096 UTF-8 bytes"));
    }
    if let Some(ids) = &request.settings.source_ids {
        let valid = |id: &String| {
            (1..=64).contains(&id.len())
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        };
        if ids.is_empty() || ids.len() > 100 || !ids.iter().all(valid) {
            return Err(argument("Expected 1–100 valid source IDs"));
        }
    }
    if limit.fract() != 0.0 || !(1.0..=100.0).contains(&limit) {
        return Err(argument("limit must be an integer between 1 and 100"));
    }
    let outcome = database
        .search_with_sources(
            query,
            Some(limit as usize),
            Some(RetrievalMethod::Keyword),
            request.settings.source_ids.as_deref().unwrap_or_default(),
            cancel,
        )
        .map_err(StageError::native)?;
    let hits = outcome
        .hits
        .iter()
        .map(|hit| {
            Ok(Hit {
                title: hit.title.clone(),
                score: f64::from(hit.score),
                evidence: evidence_value(&hit.evidence)?,
            })
        })
        .collect::<Result<_, StageError>>()?;
    Ok(Found {
        hits,
        total: outcome.total_hits,
    })
}

struct VectorSearch<'a> {
    vectors: &'a VectorCache,
    request: &'a SearchRequest,
    query: &'a str,
    args: &'a ConfigValue,
    /// `exact` or `hannoy`.
    backend: &'static str,
    max: f64,
    host: &'a dyn SearchHost,
}

/// The embedding model of a vector module, as JavaScript resolved it.
fn embedder(request: &SearchRequest, name: &str) -> Result<EmbedderSettings, StageError> {
    resolved(
        &request.embedders,
        name,
        StageError::coded(
            "VECTOR_MODEL_REQUIRED",
            "Define an embedding model under models.",
            2,
        ),
    )
}

/// A model JavaScript resolved by name, or the error it reported.
fn resolved<T: serde::de::DeserializeOwned>(
    models: &ConfigValue,
    name: &str,
    missing: StageError,
) -> Result<T, StageError> {
    let Some(entry) = field(models, name) else {
        return Err(missing);
    };
    if let Some(error) = field(entry, "error") {
        let message = string_field(error, "message")
            .unwrap_or_default()
            .to_owned();
        return Err(match string_field(error, "code") {
            Some(code) => StageError::coded(
                static_code(code),
                message,
                number(field(error, "exitCode")) as u8,
            ),
            None => StageError::plain(message),
        });
    }
    serde_json::from_str(&entry.to_js_json())
        .map_err(|_| StageError::plain("Invalid model settings."))
}

fn vector(search: &VectorSearch<'_>, cancel: &Cancellation) -> Result<Found, StageError> {
    let VectorSearch {
        vectors,
        request,
        query,
        args,
        backend,
        max,
        host,
    } = *search;
    if js_trim(query).is_empty() {
        return Err(StageError::coded(
            "INVALID_QUERY",
            "Enter a search query.",
            2,
        ));
    }
    let settings = embedder(request, string_field(args, "model").unwrap_or_default())?;
    let built = field(&request.settings.build, "vector");
    if !truthy(built) {
        return Err(StageError::coded(
            "VECTOR_REQUIRED",
            "The selected build has no vector index. Run nest build with index.vector configured.",
            3,
        ));
    }
    let identity = sha256_hex(settings.identity().to_js_json().as_bytes());
    if built.and_then(|built| string_field(built, "identity")) != Some(identity.as_str()) {
        return Err(StageError::coded(
            "VECTOR_MODEL_MISMATCH",
            "The query embedding model differs from the model used for the selected build.",
            2,
        ));
    }
    if built
        .and_then(|built| string_field(built, "backend"))
        .unwrap_or("exact")
        != backend
    {
        return Err(StageError::coded(
            "VECTOR_BACKEND_MISMATCH",
            "The selected build uses another vector index backend.",
            2,
        ));
    }
    let directory = Path::new(&request.directory);
    let metadata = vectors.load(directory)?;
    let entries = items(field(&metadata, "entries"));
    let positions: Option<Vec<usize>> = request.settings.source_ids.as_ref().map(|ids| {
        let sources: HashSet<&str> = ids.iter().map(String::as_str).collect();
        entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                field(entry, "evidence")
                    .and_then(|evidence| field(evidence, "source"))
                    .and_then(|source| string_field(source, "sourceId"))
                    .is_some_and(|id| sources.contains(id))
            })
            .map(|(i, _)| i)
            .collect()
    });
    let total = positions.as_ref().map_or(entries.len(), Vec::len);
    if total == 0 {
        return Ok(Found {
            hits: Vec::new(),
            total,
        });
    }
    let needle = Embedder::new(settings)
        .embed(&[query.to_owned()], host, cancel)?
        .pop()
        .unwrap_or_default();
    cancel.check().map_err(|_| StageError::aborted())?;
    if needle.len() as f64 != number(field(&metadata, "dimensions")) {
        return Err(StageError::coded(
            "VECTOR_DIMENSIONS",
            "Query and index dimensions differ. Rebuild using the current embedding model.",
            2,
        ));
    }
    let ranked: Vec<(usize, f64)> = if backend == "exact" {
        crate::vector::rank(
            &directory.join("vectors.f32"),
            crate::vector::Search {
                needle,
                positions,
                rows: entries.len(),
                limit: max as usize,
            },
            cancel,
        )
        .map_err(StageError::native)?
        .into_iter()
        .map(|hit| (hit.i, hit.score))
        .collect()
    } else {
        hannoy(
            &HannoySearch {
                directory,
                entries,
                positions,
                needle,
                max,
                args,
                host,
            },
            cancel,
        )?
    };
    let hits = ranked
        .into_iter()
        .map(|(i, score)| Hit {
            title: field(&entries[i], "title")
                .and_then(ConfigValue::as_str)
                .unwrap_or_default()
                .to_owned(),
            evidence: field(&entries[i], "evidence")
                .cloned()
                .unwrap_or(ConfigValue::Null),
            score,
        })
        .collect();
    Ok(Found { hits, total })
}

struct HannoySearch<'a> {
    directory: &'a Path,
    entries: &'a [ConfigValue],
    positions: Option<Vec<usize>>,
    needle: Vec<f64>,
    max: f64,
    args: &'a ConfigValue,
    host: &'a dyn SearchHost,
}

/// Searches the hannoy graph through the index tool.
fn hannoy(
    search: &HannoySearch<'_>,
    cancel: &Cancellation,
) -> Result<Vec<(usize, f64)>, StageError> {
    let tool = search.host.index_tool().map_err(StageError::plain)?;
    // `new Map(positions.map((i) => [entries[i].id, i]))`
    let mut allowed: Vec<(ConfigValue, usize)> = Vec::new();
    let all: Vec<usize> = match &search.positions {
        Some(positions) => positions.clone(),
        None => (0..search.entries.len()).collect(),
    };
    for i in all {
        let id = field(&search.entries[i], "id")
            .cloned()
            .unwrap_or(ConfigValue::Null);
        match allowed.iter_mut().find(|(key, _)| *key == id) {
            Some(entry) => entry.1 = i,
            None => allowed.push((id, i)),
        }
    }
    let mut request = vec![
        ("operation".to_owned(), text("search")),
        (
            "directory".to_owned(),
            text(&search.directory.join("hannoy").to_string_lossy()),
        ),
        (
            "query".to_owned(),
            ConfigValue::Array(search.needle.iter().copied().map(js_number).collect()),
        ),
        ("limit".to_owned(), js_number(search.max)),
    ];
    assign(
        &mut request,
        "ef_search",
        field(search.args, "ef_search").cloned(),
    );
    request.push((
        "candidates".to_owned(),
        match &search.positions {
            Some(_) => ConfigValue::Array(allowed.iter().map(|(id, _)| id.clone()).collect()),
            None => ConfigValue::Null,
        },
    ));
    let response = crate::index_stage::call_tool(
        &tool,
        &ConfigValue::Object(request).to_js_json(),
        "search",
        cancel,
    )?;
    let invalid = || {
        StageError::coded(
            "VECTOR_BACKEND_MISMATCH",
            "The index tool returned invalid candidates.",
            2,
        )
    };
    let hits = response
        .get("hits")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(invalid)?;
    if hits.len() as f64 > search.max {
        return Err(invalid());
    }
    hits.iter()
        .map(|pair| {
            let pair = pair.as_array().ok_or_else(invalid)?;
            let id = pair.first().and_then(serde_json::Value::as_f64);
            let distance = pair
                .get(1)
                .and_then(serde_json::Value::as_f64)
                .filter(|distance| distance.is_finite())
                .ok_or_else(invalid)?;
            let i = allowed
                .iter()
                .find(|(key, _)| id.is_some_and(|id| *key == ConfigValue::Number(id)))
                .map(|(_, i)| *i)
                .ok_or_else(invalid)?;
            Ok((i, 1.0 - distance))
        })
        .collect()
}

/// `fuseResults`: reciprocal rank fusion of every retrieval so far.
fn fuse(results: &[Found], k: f64, max: f64) -> Found {
    let mut fused: Vec<Hit> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for result in results {
        for (i, hit) in result.hits.iter().enumerate() {
            let evidence = &hit.evidence;
            let source = field(evidence, "source");
            let part = |name: &str| {
                source
                    .and_then(|source| field(source, name))
                    .cloned()
                    .unwrap_or(ConfigValue::Null)
            };
            let span = field(evidence, "byteRange")
                .filter(|range| !matches!(range, ConfigValue::Null))
                .or_else(|| field(evidence, "range"))
                .cloned()
                .unwrap_or(ConfigValue::Null);
            let key = ConfigValue::Array(vec![
                part("sourceId"),
                part("objectId"),
                part("version"),
                span,
            ])
            .to_js_json();
            let index = match keys.iter().position(|existing| *existing == key) {
                Some(index) => index,
                None => {
                    keys.push(key);
                    fused.push(Hit {
                        score: 0.0,
                        ..hit.clone()
                    });
                    fused.len() - 1
                }
            };
            fused[index].score += 1.0 / (k + 1.0 + i as f64);
        }
    }
    let total = results
        .iter()
        .map(|result| result.total)
        .fold(fused.len(), usize::max);
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fused.truncate(if max.is_nan() {
        0
    } else {
        max.max(0.0) as usize
    });
    Found { hits: fused, total }
}

/// `rerank.model`: the chat model orders the first `candidates` hits by their first 800 UTF-16
/// code units.
fn rerank(
    database: &KnowledgeBase,
    found: &mut Found,
    query: &str,
    args: &ConfigValue,
    request: &SearchRequest,
    host: &dyn SearchHost,
    cancel: &Cancellation,
) -> Result<(), StageError> {
    let candidates = number(field(args, "candidates"));
    let count = (candidates.max(0.0) as usize).min(found.hits.len());
    if count < 2 {
        return Ok(());
    }
    let settings: crate::generate::GeneratorSettings = resolved(
        &request.rerankers,
        string_field(args, "model").unwrap_or_default(),
        StageError::coded("INVALID_MODEL", "Define the reranking model in models.", 2),
    )?;
    let mut data = Vec::with_capacity(count);
    for (i, hit) in found.hits[..count].iter().enumerate() {
        let content = database
            .read(&evidence_of(&hit.evidence)?, cancel)
            .map_err(StageError::native)?
            .content;
        data.push(format!(
            "{{\"id\":{},\"title\":{},\"text\":{}}}",
            i + 1,
            text(&hit.title).to_js_json(),
            crate::generate::js_prefix_json(&content, 800)
        ));
    }
    let input = format!(
        "{{\"query\":{},\"candidates\":[{}]}}",
        text(query).to_js_json(),
        data.join(",")
    );
    let generated = crate::generate::generate(
        &settings,
        crate::generate::RERANK_INSTRUCTIONS,
        &input,
        crate::generate::RERANK_SCHEMA,
        host,
        cancel,
    )?;
    let order =
        crate::generate::rerank_order(generated.text.as_deref(), count).ok_or_else(|| {
            StageError::coded(
                "INVALID_MODEL_OUTPUT",
                "The reranking model must return each candidate ID exactly once.",
                1,
            )
        })?;
    let mut hits: Vec<Hit> = order.iter().map(|&id| found.hits[id - 1].clone()).collect();
    hits.extend(found.hits[count..].iter().cloned());
    found.hits = hits;
    Ok(())
}

/// Two evidence ranges of the same revision that overlap.
fn overlaps(a: &ConfigValue, b: &ConfigValue) -> bool {
    let source = |evidence: &ConfigValue, name: &str| {
        field(evidence, "source")
            .and_then(|source| field(source, name))
            .cloned()
    };
    if ["sourceId", "objectId", "version"]
        .iter()
        .any(|name| source(a, name) != source(b, name))
    {
        return false;
    }
    let bound = |evidence: &ConfigValue, range: &str, name: &str| {
        number(field(evidence, range).and_then(|range| field(range, name)))
    };
    let bytes = |evidence: &ConfigValue| truthy(field(evidence, "byteRange"));
    if bytes(a) && bytes(b) {
        return bound(a, "byteRange", "start") < bound(b, "byteRange", "end")
            && bound(b, "byteRange", "start") < bound(a, "byteRange", "end");
    }
    bound(a, "range", "startLine") <= bound(b, "range", "endLine")
        && bound(b, "range", "startLine") <= bound(a, "range", "endLine")
}

fn set_number(value: &mut ConfigValue, path: &[&str], number: f64) {
    let mut target = value;
    for (depth, name) in path.iter().enumerate() {
        let ConfigValue::Object(entries) = target else {
            return;
        };
        if depth == path.len() - 1 {
            assign(entries, name, Some(js_number(number)));
            return;
        }
        let Some((_, next)) = entries.iter_mut().find(|(key, _)| key == name) else {
            return;
        };
        target = next;
    }
}

/// `mergedCandidates`: joins overlapping hits into the first of them.
fn merged(hits: &[Hit]) -> Vec<Hit> {
    let mut groups: Vec<Hit> = Vec::new();
    for hit in hits {
        let matching: Vec<usize> = groups
            .iter()
            .enumerate()
            .filter(|(_, group)| overlaps(&group.evidence, &hit.evidence))
            .map(|(i, _)| i)
            .collect();
        let Some(&first) = matching.first() else {
            groups.push(hit.clone());
            continue;
        };
        let all: Vec<ConfigValue> = std::iter::once(hit.evidence.clone())
            .chain(matching.iter().map(|&i| groups[i].evidence.clone()))
            .collect();
        let extreme = |range: &str, name: &str, low: bool| {
            all.iter()
                .map(|evidence| number(field(evidence, range).and_then(|range| field(range, name))))
                .fold(
                    if low {
                        f64::INFINITY
                    } else {
                        f64::NEG_INFINITY
                    },
                    |a, b| {
                        if low { a.min(b) } else { a.max(b) }
                    },
                )
        };
        let start_line = extreme("range", "startLine", true);
        let end_line = extreme("range", "endLine", false);
        let all_bytes = all
            .iter()
            .all(|evidence| truthy(field(evidence, "byteRange")));
        let (byte_start, byte_end) = (
            extreme("byteRange", "start", true),
            extreme("byteRange", "end", false),
        );
        let target = &mut groups[first].evidence;
        set_number(target, &["range", "startLine"], start_line);
        set_number(target, &["range", "endLine"], end_line);
        if all_bytes {
            set_number(target, &["byteRange", "start"], byte_start);
            set_number(target, &["byteRange", "end"], byte_end);
        } else if let ConfigValue::Object(entries) = target {
            assign(entries, "byteRange", None);
        }
        for &i in matching[1..].iter().rev() {
            groups.remove(i);
        }
    }
    groups
}

struct ContextItem {
    evidence: ConfigValue,
    text: String,
    decorations: Vec<(&'static str, usize, usize)>,
    body: (usize, usize),
}

struct Assembled {
    hits: Vec<Hit>,
    context: Vec<ContextItem>,
    /// The described excerpts of the context items, then of the hits.
    described: Vec<Option<ConfigValue>>,
    chars: usize,
    budget: usize,
    stop: &'static str,
}

/// `context.assemble`: excerpts of the candidates in order until the budget is filled.
fn assemble(
    database: &KnowledgeBase,
    found: &Found,
    args: &ConfigValue,
    budget_chars: Option<f64>,
    top: f64,
    max: f64,
    cancel: &Cancellation,
) -> Result<Assembled, StageError> {
    let budget = budget_chars.unwrap_or_else(|| number(field(args, "budget_chars"))) as usize;
    let flag = |name: &str| truthy(field(args, name));
    let mut remaining = budget;
    let mut stop = if found.total as f64 > max {
        "candidate_limit"
    } else {
        "exhausted"
    };
    let candidates = if flag("merge_overlaps") {
        merged(&found.hits)
    } else {
        found.hits.clone()
    };
    let mut context: Vec<ContextItem> = Vec::new();
    for hit in &candidates {
        cancel.check().map_err(|_| StageError::aborted())?;
        let evidence = evidence_of(&hit.evidence)?;
        let content = database
            .read(&evidence, cancel)
            .map_err(StageError::native)?
            .content;
        let heading = if flag("include_headings") {
            format!("{}\n", hit.title)
        } else {
            String::new()
        };
        let parts = [
            (
                "separator",
                if context.is_empty() { "" } else { "\n\n" }.to_owned(),
            ),
            ("heading", heading),
            (
                "source",
                format!("{}:{}\n", evidence.source.url, evidence.range.start_line),
            ),
        ];
        let header: String = parts.iter().map(|(_, text)| text.as_str()).collect();
        let used = budget - remaining;
        let mut start = used;
        let mut decorations = Vec::new();
        for (kind, text) in &parts {
            if text.is_empty() {
                continue;
            }
            let end = start + text.chars().count();
            decorations.push((*kind, start, end));
            start = end;
        }
        let heading_size = start - used;
        let length = content.chars().count();
        if heading_size >= remaining
            || (!flag("truncate_last") && heading_size + length > remaining)
        {
            stop = if flag("truncate_last") {
                "budget"
            } else {
                "no_truncation"
            };
            break;
        }
        let body: String = content.chars().take(remaining - heading_size).collect();
        if body.is_empty() {
            continue;
        }
        let lines = body.strip_suffix('\n').unwrap_or(&body).split('\n').count();
        let body_size = body.chars().count();
        let mut excerpt = hit.evidence.clone();
        set_number(
            &mut excerpt,
            &["range", "endLine"],
            (evidence.range.start_line + lines - 1) as f64,
        );
        if let (Some(bytes), ConfigValue::Object(entries)) = (evidence.byte_range, &mut excerpt) {
            assign(
                entries,
                "byteRange",
                Some(object(vec![
                    ("start", js_number(bytes.start as f64)),
                    ("end", js_number((bytes.start + body.len()) as f64)),
                ])),
            );
        }
        context.push(ContextItem {
            evidence: excerpt,
            text: header + &body,
            decorations,
            body: (start, start + body_size),
        });
        remaining -= heading_size + body_size;
        if remaining == 0 {
            stop = "budget";
            break;
        }
    }
    let returned: Vec<Hit> = found
        .hits
        .iter()
        .take(if top.is_nan() {
            0
        } else {
            top.max(0.0) as usize
        })
        .cloned()
        .collect();
    let evidence: Vec<Evidence> = context
        .iter()
        .map(|item| &item.evidence)
        .chain(returned.iter().map(|hit| &hit.evidence))
        .map(evidence_of)
        .collect::<Result<_, _>>()?;
    let described = if evidence.is_empty() {
        Vec::new()
    } else {
        database
            .describe_evidence(&evidence, cancel)
            .map_err(StageError::native)?
            .iter()
            .map(|excerpt| excerpt.as_ref().map(sorted_value).transpose())
            .collect::<Result<_, _>>()?
    };
    Ok(Assembled {
        hits: returned,
        context,
        described,
        chars: budget - remaining,
        budget,
        stop,
    })
}

fn document_id(evidence: &ConfigValue) -> String {
    let source = field(evidence, "source");
    let part = |name: &str| {
        source
            .and_then(|source| string_field(source, name))
            .unwrap_or_default()
    };
    format!("{}:{}", part("sourceId"), part("objectId"))
}

fn pointer(evidence: &ConfigValue) -> Vec<(String, ConfigValue)> {
    vec![
        ("document_id".into(), text(&document_id(evidence))),
        (
            "document_version".into(),
            field(evidence, "source")
                .and_then(|source| field(source, "version"))
                .cloned()
                .unwrap_or(ConfigValue::Null),
        ),
    ]
}

fn range(start: f64, end: f64) -> ConfigValue {
    ConfigValue::Array(vec![js_number(start), js_number(end)])
}

/// `searchOutput`: the returned format of spec 27.1.
fn output(request: &SearchRequest, assembled: &Assembled) -> Result<ConfigValue, StageError> {
    let count = assembled.context.len();
    // Hits first, then the context items, as `searchOutput` lists them.
    let excerpt = |index: usize, evidence: &ConfigValue| {
        let described = if index < assembled.hits.len() {
            assembled.described.get(count + index)
        } else {
            assembled.described.get(index - assembled.hits.len())
        };
        described.cloned().flatten().ok_or_else(|| {
            StageError::plain(format!(
                "The saved revision of {} is missing.",
                document_id(evidence)
            ))
        })
    };
    let mut results = Vec::new();
    for (i, hit) in assembled.hits.iter().enumerate() {
        let described = excerpt(i, &hit.evidence)?;
        let object_id = field(&hit.evidence, "source")
            .and_then(|source| string_field(source, "objectId"))
            .unwrap_or_default();
        let heading = match hit.title.strip_prefix(&format!("{object_id} — ")) {
            Some(heading) => text(heading),
            None if hit.title == object_id => ConfigValue::Null,
            None => text(&hit.title),
        };
        let mut pages: Vec<ConfigValue> = Vec::new();
        for location in items(field(&described, "locations")) {
            if let Some(page) = field(location, "page")
                && !pages.contains(page)
            {
                pages.push(page.clone());
            }
        }
        let mut entry = vec![
            ("rank".to_owned(), js_number((i + 1) as f64)),
            ("score".to_owned(), js_number(hit.score)),
        ];
        entry.extend(pointer(&hit.evidence));
        entry.extend([
            (
                "range".to_owned(),
                range(
                    number(field(&described, "start")),
                    number(field(&described, "end")),
                ),
            ),
            (
                "text".to_owned(),
                field(&described, "text")
                    .cloned()
                    .unwrap_or(ConfigValue::Null),
            ),
            ("heading".to_owned(), heading),
            (
                "page".to_owned(),
                if pages.len() == 1 {
                    pages.remove(0)
                } else {
                    ConfigValue::Null
                },
            ),
            ("evidence".to_owned(), hit.evidence.clone()),
        ]);
        results.push(ConfigValue::Object(entry));
    }
    let mut segments = Vec::new();
    for (j, item) in assembled.context.iter().enumerate() {
        for &(kind, start, end) in &item.decorations {
            let mut segment = vec![
                ("kind".to_owned(), text(kind)),
                ("output_range".to_owned(), range(start as f64, end as f64)),
            ];
            if kind != "separator" {
                segment.extend(pointer(&item.evidence));
            }
            segments.push(ConfigValue::Object(segment));
        }
        let (start, end) = item.body;
        let source_start = number(field(
            &excerpt(assembled.hits.len() + j, &item.evidence)?,
            "start",
        ));
        let mut segment = vec![
            ("kind".to_owned(), text("body")),
            ("output_range".to_owned(), range(start as f64, end as f64)),
        ];
        segment.extend(pointer(&item.evidence));
        segment.push((
            "source_range".to_owned(),
            range(source_start, source_start + (end - start) as f64),
        ));
        segments.push(ConfigValue::Object(segment));
    }
    let id = search_config_id(&request.settings, true);
    let base = search_config_id(&request.settings, false);
    let warnings = if base == id {
        Vec::new()
    } else {
        vec![object(vec![
            ("code", text("SEARCH_OVERRIDES")),
            (
                "message",
                text("Request overrides change the conditions used for evaluation."),
            ),
        ])]
    };
    let context_text: String = assembled
        .context
        .iter()
        .map(|item| item.text.as_str())
        .collect();
    Ok(object(vec![
        ("schema_version", js_number(1.0)),
        ("query", text(&request.query)),
        (
            "build_id",
            field(&request.settings.build, "build_id")
                .cloned()
                .unwrap_or(ConfigValue::Null),
        ),
        ("search_config_id", text(&id)),
        ("results", ConfigValue::Array(results)),
        (
            "context",
            object(vec![
                ("text", text(&context_text)),
                ("chars", js_number(assembled.chars as f64)),
                ("budget_chars", js_number(assembled.budget as f64)),
                (
                    "stop_reason",
                    text(if assembled.stop == "budget" {
                        "budget_filled"
                    } else {
                        assembled.stop
                    }),
                ),
                ("segments", ConfigValue::Array(segments)),
            ]),
        ),
        ("warnings", ConfigValue::Array(warnings)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Host;

    impl Charge for Host {
        fn charge(&self, _bytes: usize) -> Result<(), String> {
            Ok(())
        }
    }

    impl SearchHost for Host {
        fn index_tool(&self) -> Result<String, String> {
            Err("No index tool in tests".into())
        }
    }

    fn json(text: &str) -> ConfigValue {
        serde_json::from_str(text).unwrap()
    }

    fn request(plan: &str, values: &str) -> SearchRequest {
        serde_json::from_str(&format!(
            r#"{{"plan":{plan},"query":" pump  ","build":{{"build_id":"bld-1","index":"run-1","keyword":{{"tokenizer":"word"}}}},"directory":"unused"{values}}}"#
        ))
        .unwrap()
    }

    const PLAN: &str = r#"[{"use":"query.normalize","with":{"unicode":"nfkc"}},
        {"use":"retrieval.keyword","with":{"top_k":5}},
        {"use":"context.assemble","with":{"budget_chars":400,"retrieval":"fill_budget","max_candidates":10,
          "merge_overlaps":true,"truncate_last":true,"include_headings":true}}]"#;

    #[test]
    fn searches_assembles_context_and_reports_overrides() {
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("a.md"), "# Pump\nCheck the pump seal.\n").unwrap();
        std::fs::write(docs.join("b.md"), "# Valve\nThe pump valve opens.\n").unwrap();
        let cancel = Cancellation::default();
        let database = KnowledgeBase::open(&root.path().join("index"), &cancel).unwrap();
        database.add_source("docs", docs, &cancel).unwrap();
        let vectors = VectorCache::default();

        let outcome = run(&database, &vectors, &request(PLAN, ""), &Host, &cancel).unwrap();
        let output = &outcome.output;
        assert_eq!(output.get("query"), Some(&text(" pump  ")));
        let results = items(output.get("results"));
        assert_eq!(results.len(), 2);
        assert_eq!(field(&results[0], "rank"), Some(&ConfigValue::Number(1.0)));
        let context = output.get("context").unwrap();
        let context_text = string_field(context, "text").unwrap();
        assert!(context_text.contains("pump"), "{context_text}");
        assert_eq!(
            field(context, "budget_chars"),
            Some(&ConfigValue::Number(400.0))
        );
        assert_eq!(string_field(context, "stop_reason"), Some("exhausted"));
        assert_eq!(outcome.context_stop, "exhausted");
        // A smaller budget truncates the last excerpt.
        let used = number(field(context, "chars"));
        let small = run(
            &database,
            &vectors,
            &request(PLAN, &format!(r#","budgetChars":{}"#, used - 3.0)),
            &Host,
            &cancel,
        )
        .unwrap();
        assert_eq!(small.context_stop, "budget");
        let small_context = small.output.get("context").unwrap();
        assert_eq!(number(field(small_context, "chars")), used - 3.0);
        assert!(!outcome.context.is_empty());
        let id = string_field(output, "search_config_id").unwrap().to_owned();
        assert!(id.starts_with("search-"));
        assert_eq!(items(output.get("warnings")).len(), 0);

        // A request override changes the configuration ID and warns.
        let limited = run(
            &database,
            &vectors,
            &request(PLAN, r#","limit":1"#),
            &Host,
            &cancel,
        )
        .unwrap()
        .output;
        assert_eq!(items(limited.get("results")).len(), 1);
        assert_ne!(
            string_field(&limited, "search_config_id"),
            Some(id.as_str())
        );
        assert_eq!(
            string_field(&items(limited.get("warnings"))[0], "code"),
            Some("SEARCH_OVERRIDES")
        );

        // Errors name their module; a missing index is checked first.
        let blank = SearchRequest {
            query: " \u{3000}".into(),
            ..request(PLAN, "")
        };
        let failure = run(&database, &vectors, &blank, &Host, &cancel)
            .err()
            .unwrap();
        assert_eq!(failure.error.code, Some("INVALID_QUERY"));
        assert_eq!(failure.module.as_deref(), Some("retrieval.keyword"));
        let vector = request(
            r#"[{"use":"retrieval.vector","with":{"model":"e","top_k":5}},{"use":"context.assemble","with":{}}]"#,
            "",
        );
        let failure = run(&database, &vectors, &vector, &Host, &cancel)
            .err()
            .unwrap();
        assert_eq!(failure.error.code, Some("INDEX_REQUIRED"));
        assert_eq!(failure.module, None);

        // rerank.model asks the chat model to reorder the first candidates.
        let (base, bodies) = crate::embed::tests::serve(|_| {
            (
                200,
                serde_json::json!({ "done": true, "message": { "content": r#"{"order":[2,1]}"# } })
                    .to_string(),
            )
        });
        let plan = PLAN.replace(
            r#"{"use":"context.assemble""#,
            r#"{"use":"rerank.model","with":{"model":"m","candidates":2}},{"use":"context.assemble""#,
        );
        let missing = run(&database, &vectors, &request(&plan, ""), &Host, &cancel)
            .err()
            .unwrap();
        assert_eq!(missing.error.code, Some("INVALID_MODEL"));
        assert_eq!(missing.module.as_deref(), Some("rerank.model"));
        let reranked = request(
            &plan,
            &format!(
                r#","rerankModels":[null],"rerankers":{{"m":{{"provider":"ollama","model":"r","url":"{base}/api/chat"}}}}"#
            ),
        );
        let output = run(&database, &vectors, &reranked, &Host, &cancel)
            .unwrap()
            .output;
        let first =
            |output: &ConfigValue| field(&items(output.get("results"))[0], "document_id").cloned();
        assert_ne!(first(&output), first(&outcome.output));
        let sent: serde_json::Value = serde_json::from_str(&bodies.lock().unwrap()[0]).unwrap();
        let input: serde_json::Value =
            serde_json::from_str(sent["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["query"], "pump");
        assert_eq!(input["candidates"][1]["id"], 2);
    }

    #[test]
    fn reads_vector_metadata_once_per_index_and_filters_sources() {
        let root = tempfile::tempdir().unwrap();
        let (base, bodies) = crate::embed::tests::serve(|_| {
            (
                200,
                serde_json::json!({ "embeddings": [[2.0, 0.0]] }).to_string(),
            )
        });
        let settings = format!(r#"{{"provider":"ollama","model":"e","url":"{base}/api/embed"}}"#);
        let identity = sha256_hex(
            serde_json::from_str::<EmbedderSettings>(&settings)
                .unwrap()
                .identity()
                .to_js_json()
                .as_bytes(),
        );
        let write = |directory: &Path, label: &str| {
            std::fs::create_dir_all(directory).unwrap();
            let entries: Vec<String> = ["docs", "other", "docs"]
                .iter()
                .enumerate()
                .map(|(i, source)| {
                    format!(r#"{{"title":"{label}-{i}","evidence":{{"source":{{"sourceId":"{source}"}}}}}}"#)
                })
                .collect();
            let mut bytes = vec![0_u8; 3 * 8];
            for i in [0, 2] {
                bytes[i * 8..i * 8 + 4].copy_from_slice(&1.0_f32.to_le_bytes());
            }
            std::fs::write(directory.join("vectors.f32"), bytes).unwrap();
            std::fs::write(
                directory.join("vectors.json"),
                format!(
                    r#"{{"identity":"{identity}","dimensions":2,"entries":[{}]}}"#,
                    entries.join(",")
                ),
            )
            .unwrap();
        };
        let request = |directory: &Path, sources: &str| -> SearchRequest {
            serde_json::from_str(&format!(
                r#"{{"plan":[],"query":"q","build":{{"vector":{{"identity":"{identity}"}}}},"directory":{},"embedders":{{"e":{settings}}}{sources}}}"#,
                serde_json::to_string(&directory.to_string_lossy()).unwrap()
            ))
            .unwrap()
        };
        let args = json(r#"{"model":"e","top_k":3}"#);
        let cancel = Cancellation::default();
        let search = |cache: &VectorCache, request: &SearchRequest| {
            vector(
                &VectorSearch {
                    vectors: cache,
                    request,
                    query: "q",
                    args: &args,
                    backend: "exact",
                    max: 3.0,
                    host: &Host,
                },
                &cancel,
            )
        };
        let titles = |found: &Found| -> Vec<String> {
            found.hits.iter().map(|hit| hit.title.clone()).collect()
        };
        write(root.path(), "original");
        let cache = VectorCache::default();
        let all = search(&cache, &request(root.path(), "")).unwrap();
        assert_eq!(all.total, 3);
        assert_eq!(titles(&all), ["original-0", "original-2", "original-1"]);
        let filtered = search(&cache, &request(root.path(), r#","sourceIds":["docs"]"#)).unwrap();
        assert_eq!(filtered.total, 2);
        assert_eq!(titles(&filtered), ["original-0", "original-2"]);
        // The metadata stays loaded for the open index.
        std::fs::remove_file(root.path().join("vectors.json")).unwrap();
        assert_eq!(
            titles(&search(&cache, &request(root.path(), "")).unwrap()),
            titles(&all)
        );
        let none = search(&cache, &request(root.path(), r#","sourceIds":["missing"]"#)).unwrap();
        assert!(none.hits.is_empty());
        assert_eq!(bodies.lock().unwrap().len(), 3);
        // Another index has its own metadata.
        let next = root.path().join("next");
        assert!(search(&VectorCache::default(), &request(&next, "")).is_err());
        write(&next, "updated");
        let updated = search(&VectorCache::default(), &request(&next, "")).unwrap();
        assert_eq!(updated.hits[0].title, "updated-0");
    }

    fn hit(object: &str, start: u32, end: u32, score: f64) -> Hit {
        Hit {
            title: object.into(),
            score,
            evidence: json(&format!(
                r#"{{"acquiredAtMs":1,"byteRange":{{"end":{},"start":{}}},"range":{{"endLine":{end},"startLine":{start}}},"source":{{"modifiedAtMs":null,"objectId":"{object}","sourceId":"docs","url":"file:///{object}","version":"v"}}}}"#,
                end * 10,
                start * 10
            )),
        }
    }

    #[test]
    fn fuses_ranks_and_merges_overlaps() {
        let keyword = Found {
            hits: vec![hit("a", 1, 2, 3.0), hit("b", 1, 2, 2.0)],
            total: 2,
        };
        let vector = Found {
            hits: vec![hit("b", 1, 2, 0.9), hit("c", 1, 2, 0.8)],
            total: 5,
        };
        let fused = fuse(&[keyword, vector], 60.0, 10.0);
        let order: Vec<&str> = fused.hits.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(order, ["b", "a", "c"]);
        assert_eq!(fused.total, 5);
        assert!((fused.hits[0].score - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-15);

        let joined = merged(&[
            hit("a", 1, 3, 1.0),
            hit("a", 2, 5, 0.5),
            hit("a", 9, 9, 0.1),
        ]);
        assert_eq!(joined.len(), 2);
        assert_eq!(
            field(&joined[0].evidence, "range").unwrap().to_js_json(),
            r#"{"endLine":5,"startLine":1}"#
        );
        assert_eq!(
            field(&joined[0].evidence, "byteRange")
                .unwrap()
                .to_js_json(),
            r#"{"end":50,"start":10}"#
        );
    }
}
