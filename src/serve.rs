//! Serving a project (spec 28.10 step 3c): the serving builds, the project search and answers, and
//! the HTTP server of spec 27.3 with `/v1/search`, `/v1/responses` and `/mcp`.
//!
//! The rules, status codes, headers and error bodies are the ones of the JavaScript server this
//! replaces (`listenHttp` and `serveRagHttp`). `/mcp` answers in the stateless mode the TypeScript
//! SDK used: one server per request and the replies as server-sent events.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response, StatusCode};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config_value::ConfigValue;
use crate::generate::{AnswerRequest, GeneratorSettings};
use crate::index_stage::{field, js_json, string_field, truthy};
use crate::mcp::{McpHost, McpServer};
use crate::search_stage::{SearchHost, SearchRequest, SearchSettings, VectorCache};
use crate::transform_stage::StageError;
use crate::{Cancellation, CorpusBackend, Evidence, KnowledgeBase};

#[derive(Debug, Default, Deserialize)]
pub struct Access {
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    #[serde(default)]
    pub allowed_clients: Vec<String>,
}

impl Access {
    /// The ranges of `allowed_clients`; none means any client.
    pub(crate) fn clients(&self) -> Vec<crate::config_check::Network> {
        self.allowed_clients
            .iter()
            .filter_map(|range| crate::config_check::Network::parse(range))
            .collect()
    }
}

#[derive(Debug, Deserialize)]
pub struct Limits {
    pub max_request_bytes: u64,
    pub request_timeout_seconds: u64,
    pub max_concurrent: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_request_bytes: 1_048_576,
            request_timeout_seconds: 120,
            max_concurrent: 8,
        }
    }
}

/// The validated `serve` section.
#[derive(Debug, Deserialize)]
pub struct ServeSettings {
    pub transport: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub access: Access,
    #[serde(default)]
    pub limits: Limits,
}

#[derive(Debug, Deserialize)]
pub struct Paths {
    pub state: String,
    pub artifacts: String,
    pub corpus: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServeRequest {
    /// `mcp` or `rag`.
    pub project_type: String,
    pub settings: ServeSettings,
    /// The token of `auth: token`, read from its environment variable.
    #[serde(default)]
    pub token: Option<String>,
    pub paths: Paths,
    /// `moduleSettings(plan.search)`.
    pub search: ConfigValue,
    /// `moduleSettings(plan.ask)`.
    #[serde(default)]
    pub ask: ConfigValue,
    /// The answer model, when one is set.
    #[serde(default)]
    pub model: Option<GeneratorSettings>,
}

/// What serving asks of JavaScript: the models of a build, the index tool, and where it listens.
pub trait ServeHost: SearchHost + Send + Sync {
    /// `{ embedders, rerankers, rerankModels }` for a build, or `{ error: { code, message,
    /// exitCode } }`.
    fn search_models(&self, build: &ConfigValue) -> Result<ConfigValue, String>;
    /// Reports `{ port, buildId }` once the server listens.
    fn listening(&self, address: &ConfigValue);
    /// Reports the index directory of a build it opened.
    fn opened(&self, directory: &str);
}

struct OpenIndex {
    database: Arc<KnowledgeBase>,
    vectors: Arc<VectorCache>,
    users: usize,
}

struct Active {
    /// The build the search settings still apply to (spec 12).
    compatible: ConfigValue,
    indexes: HashMap<String, OpenIndex>,
}

/// The serving builds of a project: the build at startup and the builds that replace it in the
/// same serving generation, while their keyword and vector settings stay the same.
pub struct Serving {
    request: ServeRequest,
    host: Arc<dyn ServeHost>,
    initial: ConfigValue,
    uses_keyword: bool,
    uses_vector: bool,
    builds: Builds,
    active: Mutex<Active>,
    /// What the index served at startup holds, for the descriptions of the MCP tools.
    about: Option<String>,
}

/// Where the builds to serve come from.
enum Builds {
    /// The builds of `state.sqlite`, in the serving generation of startup.
    State {
        connection: Mutex<Connection>,
        generation: i64,
    },
    /// The index the versioned store serves under a name (`nest release`). Its directories are
    /// below `indexes`.
    Store {
        store: Mutex<crate::store::Store>,
        indexes: PathBuf,
        name: String,
    },
}

fn sql(error: rusqlite::Error) -> StageError {
    StageError::plain(error.to_string())
}

fn stored(error: crate::store::StoreError) -> StageError {
    StageError::plain(error.to_string())
}

fn index_of(build: &ConfigValue) -> String {
    string_field(build, "index").unwrap_or_default().to_owned()
}

/// What serving needs of an index of the versioned store, written as a saved build is: its
/// directory, and the keyword settings every index of the store is built with.
fn store_build(
    store: &crate::store::Store,
    indexes: &Path,
    id: crate::store::SnapshotId,
) -> Result<Option<ConfigValue>, StageError> {
    let Some(location) = crate::index::located(store, indexes, id).map_err(stored)? else {
        return Ok(None);
    };
    serde_json::from_value(serde_json::json!({
        "build_id": format!("index-{}", id.0),
        "index": location,
        "keyword": { "tokenizer": "lindera" },
    }))
    .map(Some)
    .map_err(|_| StageError::plain("Could not describe a served index."))
}

impl Builds {
    /// The build served now. `None` from `state.sqlite` when the startup generation has none.
    fn served(&self) -> Result<Option<ConfigValue>, StageError> {
        match self {
            Self::State {
                connection,
                generation,
            } => {
                let state = connection
                    .lock()
                    .map_err(|_| StageError::plain("Could not read the serving build."))?;
                let manifest: Option<String> = state
                    .query_row(
                        "SELECT manifest FROM serving_generations JOIN builds ON builds.id = serving_generations.build_id
                         WHERE serving_generations.id = ?",
                        [generation],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(sql)?;
                manifest
                    .map(|manifest| {
                        serde_json::from_str(&manifest)
                            .map_err(|_| StageError::plain("A saved build is not valid JSON."))
                    })
                    .transpose()
            }
            Self::Store {
                store,
                indexes,
                name,
            } => {
                let store = store
                    .lock()
                    .map_err(|_| StageError::plain("Could not read the served index."))?;
                match store.releases(name).map_err(stored)?.last() {
                    Some(id) => store_build(&store, indexes, *id),
                    None => Ok(None),
                }
            }
        }
    }

    /// The builds to read evidence from when the one served no longer has it, newest first.
    fn earlier(&self) -> Result<Vec<ConfigValue>, StageError> {
        match self {
            Self::State { connection, .. } => {
                let manifests: Vec<String> = {
                    let state = connection
                        .lock()
                        .map_err(|_| StageError::plain("Could not read the builds."))?;
                    let mut statement = state
                        .prepare("SELECT manifest FROM builds ORDER BY rowid DESC")
                        .map_err(sql)?;
                    statement
                        .query_map([], |row| row.get(0))
                        .map_err(sql)?
                        .collect::<Result<_, _>>()
                        .map_err(sql)?
                };
                Ok(manifests
                    .iter()
                    .filter_map(|manifest| serde_json::from_str(manifest).ok())
                    .collect())
            }
            Self::Store {
                store,
                indexes,
                name,
            } => {
                let store = store
                    .lock()
                    .map_err(|_| StageError::plain("Could not read the served indexes."))?;
                let mut builds = Vec::new();
                for id in store.releases(name).map_err(stored)?.iter().rev() {
                    if let Some(build) = store_build(&store, indexes, *id)? {
                        builds.push(build);
                    }
                }
                Ok(builds)
            }
        }
    }
}

impl Serving {
    pub fn open(request: ServeRequest, host: Arc<dyn ServeHost>) -> Result<Self, StageError> {
        let state = Connection::open_with_flags(
            &request.paths.state,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sql)?;
        state
            .busy_timeout(Duration::from_millis(500))
            .map_err(sql)?;
        let row: Option<(String, i64)> = state
            .query_row(
                "SELECT manifest, coalesce((SELECT max(id) FROM adoption_history), 0)
                 FROM project JOIN builds ON builds.id = project.build_id WHERE project.id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        let Some((manifest, generation)) = row else {
            return Err(StageError::coded(
                "BUILD_REQUIRED",
                "No searchable build. Run nest build first.",
                3,
            ));
        };
        let initial: ConfigValue = serde_json::from_str(&manifest)
            .map_err(|_| StageError::plain("A saved build is not valid JSON."))?;
        let builds = Builds::State {
            connection: Mutex::new(state),
            generation,
        };
        Self::start(request, host, initial, builds)
    }

    /// Serves the index the versioned store in `store` serves under `name`, and the index it
    /// serves next whenever `nest release` moves the name. `request.paths.artifacts` is the
    /// directory of the store's indexes.
    pub fn open_store(
        request: ServeRequest,
        host: Arc<dyn ServeHost>,
        store: &Path,
        name: &str,
    ) -> Result<Self, StageError> {
        let builds = Builds::Store {
            store: Mutex::new(crate::store::Store::open(store).map_err(stored)?),
            indexes: PathBuf::from(&request.paths.artifacts),
            name: name.to_owned(),
        };
        let Some(initial) = builds.served()? else {
            return Err(StageError::coded(
                "BUILD_REQUIRED",
                format!(
                    "No index is served as {name}. Run nest release <index>, or nest build --release."
                ),
                3,
            ));
        };
        Self::start(request, host, initial, builds)
    }

    fn start(
        request: ServeRequest,
        host: Arc<dyn ServeHost>,
        initial: ConfigValue,
        builds: Builds,
    ) -> Result<Self, StageError> {
        let plan = crate::index_stage::items(Some(&request.search));
        crate::search_stage::assert_indexes(&initial, plan)?;
        let uses = |names: &[&str]| {
            plan.iter()
                .any(|module| names.contains(&string_field(module, "use").unwrap_or_default()))
        };
        let uses_keyword = uses(&["retrieval.keyword"]);
        let uses_vector = uses(&["retrieval.vector", "hannoy.search"]);
        let mut serving = Self {
            request,
            host,
            initial: initial.clone(),
            uses_keyword,
            uses_vector,
            builds,
            about: None,
            active: Mutex::new(Active {
                compatible: initial.clone(),
                indexes: HashMap::new(),
            }),
        };
        let first = serving.open_build(&initial)?;
        serving.about =
            holdings(&Path::new(&serving.request.paths.artifacts).join(index_of(&initial)));
        serving
            .active
            .get_mut()
            .map_err(|_| StageError::plain("Could not open the serving build."))?
            .indexes
            .insert(index_of(&initial), first);
        Ok(serving)
    }

    pub fn initial_database(&self) -> Option<Arc<KnowledgeBase>> {
        self.active.lock().ok().and_then(|active| {
            active
                .indexes
                .get(&index_of(&self.initial))
                .map(|index| index.database.clone())
        })
    }

    /// `openBuild`: the index of a build with its keyword settings and the shared corpus.
    fn open_build(&self, build: &ConfigValue) -> Result<OpenIndex, StageError> {
        let artifacts = Path::new(&self.request.paths.artifacts);
        let directory = artifacts.join(index_of(build));
        let cancel = Cancellation::default();
        if KnowledgeBase::inspect_index(&directory, &cancel)
            .map_err(StageError::native)?
            .is_none()
        {
            return Err(StageError::coded(
                "BUILD_REQUIRED",
                "The build index is missing. Run nest build first.",
                3,
            ));
        }
        let keyword = field(build, "keyword");
        let snapshot: Option<PathBuf> = keyword
            .and_then(|keyword| string_field(keyword, "dictionary_hash"))
            .map(|hash| artifacts.join("dictionaries").join(format!("{hash}.csv")));
        let corpus = truthy(field(build, "shared_corpus"))
            .then(|| PathBuf::from(&self.request.paths.corpus));
        let database = KnowledgeBase::open_with_options(
            &directory,
            CorpusBackend::Sqlite,
            corpus.as_deref(),
            crate::index_stage::keyword_options(keyword, snapshot.as_deref()),
            &cancel,
        )
        .map_err(StageError::native)?;
        self.host.opened(&directory.to_string_lossy());
        Ok(OpenIndex {
            database: Arc::new(database),
            vectors: Arc::default(),
            users: 0,
        })
    }

    /// The build serving now, or the startup build.
    fn current(&self) -> Result<ConfigValue, StageError> {
        Ok(self
            .builds
            .served()?
            .unwrap_or_else(|| self.initial.clone()))
    }

    /// `withActive`: runs `work` on the index of the build to use, keeping it open meanwhile.
    fn with_active<T>(
        &self,
        work: impl FnOnce(&Arc<KnowledgeBase>, &Arc<VectorCache>, &ConfigValue) -> Result<T, StageError>,
    ) -> Result<T, StageError> {
        let current = self.current()?;
        let locked = || {
            self.active
                .lock()
                .map_err(|_| StageError::plain("Could not use the serving build."))
        };
        let vector = |build: &ConfigValue| {
            let vector = field(build, "vector");
            (
                js_json(vector.and_then(|vector| field(vector, "identity"))),
                vector
                    .and_then(|vector| string_field(vector, "backend"))
                    .unwrap_or("exact")
                    .to_owned(),
            )
        };
        let keyword_changed = self.uses_keyword
            && js_json(field(&current, "keyword")) != js_json(field(&self.initial, "keyword"));
        let vector_changed = self.uses_vector && vector(&current) != vector(&self.initial);
        let (previous, selected, database, vectors) = {
            let mut active = locked()?;
            let previous = index_of(&active.compatible);
            if !keyword_changed && !vector_changed {
                active.compatible = current;
            }
            let selected = active.compatible.clone();
            let index = index_of(&selected);
            if !active.indexes.contains_key(&index) {
                let opened = self.open_build(&selected)?;
                active.indexes.insert(index.clone(), opened);
            }
            let entry = active
                .indexes
                .get_mut(&index)
                .ok_or_else(|| StageError::plain("Could not open the serving build."))?;
            entry.users += 1;
            (
                previous,
                selected,
                entry.database.clone(),
                entry.vectors.clone(),
            )
        };
        let outcome = work(&database, &vectors, &selected);
        drop(database);
        if let Ok(mut active) = locked() {
            let index = index_of(&selected);
            if let Some(entry) = active.indexes.get_mut(&index) {
                entry.users -= 1;
            }
            let compatible = index_of(&active.compatible);
            if previous != index || index != compatible {
                for name in [previous, index] {
                    if name != compatible
                        && active
                            .indexes
                            .get(&name)
                            .is_some_and(|entry| entry.users == 0)
                    {
                        active.indexes.remove(&name);
                    }
                }
            }
        }
        outcome
    }

    /// The project search of a request on the build to use.
    pub fn search(
        &self,
        query: &str,
        limit: Option<f64>,
        budget_chars: Option<f64>,
        source_ids: Option<Vec<String>>,
        cancel: &Cancellation,
    ) -> Result<ConfigValue, StageError> {
        self.with_active(|database, vectors, build| {
            self.search_on(
                database,
                vectors,
                build,
                query,
                limit,
                budget_chars,
                source_ids,
                cancel,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn search_on(
        &self,
        database: &KnowledgeBase,
        vectors: &VectorCache,
        build: &ConfigValue,
        query: &str,
        limit: Option<f64>,
        budget_chars: Option<f64>,
        source_ids: Option<Vec<String>>,
        cancel: &Cancellation,
    ) -> Result<ConfigValue, StageError> {
        let models = self.host.search_models(build).map_err(StageError::plain)?;
        if let Some(error) = field(&models, "error") {
            let message = string_field(error, "message")
                .unwrap_or_default()
                .to_owned();
            return Err(
                match (string_field(error, "code"), field(error, "exitCode")) {
                    (Some(code), Some(ConfigValue::Number(exit))) => StageError::coded(
                        Box::leak(code.to_owned().into_boxed_str()),
                        message,
                        *exit as u8,
                    ),
                    _ => StageError::plain(message),
                },
            );
        }
        let request = SearchRequest {
            settings: SearchSettings {
                plan: self.request.search.clone(),
                limit,
                budget_chars,
                source_ids,
                build: build.clone(),
                rerank_models: crate::index_stage::items(field(&models, "rerankModels")).to_vec(),
            },
            query: query.to_owned(),
            directory: Path::new(&self.request.paths.artifacts)
                .join(index_of(build))
                .to_string_lossy()
                .into_owned(),
            embedders: field(&models, "embedders")
                .cloned()
                .unwrap_or(ConfigValue::Null),
            rerankers: field(&models, "rerankers")
                .cloned()
                .unwrap_or(ConfigValue::Null),
        };
        crate::search_stage::run(database, vectors, &request, self.host.as_ref(), cancel)
            .map(|outcome| outcome.output)
            .map_err(|failure| failure.error)
    }

    /// An answer: the project search of the question and the answer model on its context.
    pub fn answer(&self, query: &str, cancel: &Cancellation) -> Result<ConfigValue, StageError> {
        let Some(model) = &self.request.model else {
            return Err(StageError::coded(
                "MODEL_REQUIRED",
                "Set the answer model in the KnowledgeNestfile and restart serve.",
                3,
            ));
        };
        self.with_active(|database, vectors, build| {
            let search =
                self.search_on(database, vectors, build, query, None, None, None, cancel)?;
            crate::generate::answer(
                &AnswerRequest {
                    plan: self.request.ask.clone(),
                    search,
                    query: query.to_owned(),
                    model: model.clone(),
                },
                self.host.as_ref(),
                cancel,
            )
            .map_err(|failure| failure.error)
        })
    }

    /// Reads evidence from the build to use, or from the newest build that still has it.
    pub fn read(&self, evidence: &Evidence, cancel: &Cancellation) -> Result<String, StageError> {
        let text = |outcome: crate::ReadOutcome| {
            serde_json::to_value(outcome)
                .ok()
                .and_then(|value| serde_json::from_str::<ConfigValue>(&value.to_string()).ok())
                .map(|value| value.to_js_json())
                .ok_or_else(|| StageError::plain("Could not encode the read result."))
        };
        let stale = |error: &crate::Error| matches!(error.code(), "STALE_EVIDENCE" | "NOT_FOUND");
        let first = self.with_active(|database, _, _| Ok(database.read(evidence, cancel)))?;
        let error = match first {
            Ok(outcome) => return text(outcome),
            Err(error) if stale(&error) => error,
            Err(error) => return Err(StageError::native(error)),
        };
        for build in self.builds.earlier()? {
            let Ok(archived) = self.open_build(&build) else {
                continue;
            };
            match archived.database.read(evidence, cancel) {
                Ok(outcome) => return text(outcome),
                Err(failure) if stale(&failure) => {}
                Err(failure) => return Err(StageError::native(failure)),
            }
        }
        Err(StageError::native(error))
    }
}

/// What an index holds, in words: how many documents, in which sources.
fn holdings(directory: &Path) -> Option<String> {
    let status = KnowledgeBase::inspect_index(directory, &Cancellation::default())
        .ok()
        .flatten()?;
    let sources: Vec<String> = status
        .sources
        .iter()
        .map(|(source, documents)| format!("{source} ({documents})"))
        .collect();
    let documents = status.searchable_documents;
    let noun = if documents == 1 {
        "document"
    } else {
        "documents"
    };
    Some(format!(
        "It holds {documents} {noun}, by source: {}.",
        sources.join(", ")
    ))
}

impl McpHost for Serving {
    fn search(&self, arguments: &ConfigValue, cancel: &Cancellation) -> Result<String, String> {
        let number = |name: &str| match field(arguments, name) {
            Some(ConfigValue::Number(value)) => Some(*value),
            _ => None,
        };
        let ids = crate::index_stage::items(field(arguments, "sourceIds"))
            .iter()
            .filter_map(|id| id.as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        Serving::search(
            self,
            string_field(arguments, "query").unwrap_or_default(),
            number("limit"),
            number("budgetChars"),
            field(arguments, "sourceIds").map(|_| ids),
            cancel,
        )
        .map(|output| output.to_js_json())
        .map_err(|error| error.message)
    }

    fn read(&self, evidence: &Evidence, cancel: &Cancellation) -> Result<String, String> {
        Serving::read(self, evidence, cancel).map_err(|error| error.message)
    }

    fn document(
        &self,
        document: &crate::DocumentRef,
        cancel: &Cancellation,
    ) -> Option<Result<Option<crate::ReadOutcome>, String>> {
        Some(
            self.with_active(|database, _, _| {
                database
                    .get_document(document, cancel)
                    .map_err(StageError::native)
            })
            .map_err(|error| error.message),
        )
    }
}

/// Serves MCP on standard input and output for a project.
pub fn serve_stdio(
    serving: Arc<Serving>,
    search_only: bool,
    cancel: &Cancellation,
) -> Result<(), String> {
    let database = serving
        .initial_database()
        .ok_or("The serving build is not open.")?;
    serving.host.listening(&ConfigValue::Object(vec![(
        "buildId".into(),
        field(&serving.initial, "build_id")
            .cloned()
            .unwrap_or(ConfigValue::Null),
    )]));
    let host: Arc<dyn McpHost> = serving.clone();
    let about = serving.about.clone();
    crate::mcp::serve_stdio(
        McpServer::new(database, Some(host), search_only).with_about(about),
        cancel,
    )
}

// ---------------------------------------------------------------------------------------------
// HTTP

pub(crate) type Body = Full<Bytes>;

pub(crate) struct Reply {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
}

impl Reply {
    pub(crate) fn json(status: u16, body: String) -> Self {
        Self {
            status,
            headers: vec![("content-type", "application/json; charset=utf-8".into())],
            body,
        }
    }

    /// `reject`: `{ error: { message, type, param, code } }`.
    pub(crate) fn error(status: u16, code: &str, message: &str) -> Self {
        let kind = if status < 500 {
            "invalid_request_error"
        } else {
            "server_error"
        };
        let mut reply = Self::json(
            status,
            format!(
                r#"{{"error":{{"message":{},"type":"{kind}","param":null,"code":{}}}}}"#,
                ConfigValue::String(message.into()).to_js_json(),
                ConfigValue::String(code.into()).to_js_json()
            ),
        );
        if status == 408 || status == 413 {
            reply.headers.push(("connection", "close".into()));
        }
        reply
    }

    /// The reply for an error of the search or the answer.
    fn failure(error: &StageError) -> Self {
        let status = match error.exit_code {
            Some(2) => 400,
            Some(3) => 503,
            _ => 500,
        };
        let message = if error.exit_code.is_some() {
            error.message.as_str()
        } else {
            "The request could not be completed."
        };
        Self::error(status, error.code.unwrap_or("EXECUTION_FAILED"), message)
    }
}

/// `hostname`: a host name or address, lowercase, or `None`.
pub(crate) fn hostname(value: &str) -> Option<String> {
    if value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || "/@?#\\,%".contains(c))
    {
        return None;
    }
    let host = value
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(value)
        .to_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Some(host);
    }
    let bytes = host.as_bytes();
    let domain = host.len() <= 253
        && !bytes.is_empty()
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'.' || *byte == b'-');
    let same = url::Url::parse(&format!("http://{host}"))
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|parsed| parsed == host);
    (domain && same).then_some(host)
}

/// `requestTarget`: the host name of the Host header and the authority with its port.
pub(crate) fn request_target(value: &str, port: u16) -> Option<(String, String)> {
    if value
        .chars()
        .any(|c| c.is_whitespace() || "/@?#\\,%".contains(c))
    {
        return None;
    }
    let (name, given) = if value.starts_with('[') {
        let end = value.find(']')?;
        let rest = &value[end + 1..];
        let name = &value[..=end];
        if !name[1..name.len() - 1]
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
        {
            return None;
        }
        (name, rest)
    } else {
        let end = value.find(':').unwrap_or(value.len());
        (&value[..end], &value[end..])
    };
    if name.is_empty()
        || (!name.starts_with('[')
            && !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)))
    {
        return None;
    }
    let given = match given.strip_prefix(':') {
        Some(digits) if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) => {
            let number: u64 = digits.parse().unwrap_or(0);
            if !(1..=65535).contains(&number) {
                return None;
            }
            true
        }
        Some(_) => return None,
        None if given.is_empty() => false,
        None => return None,
    };
    let name = hostname(name)?;
    let authority = if given || port == 80 {
        value.to_owned()
    } else {
        format!("{value}:{port}")
    };
    Some((name, authority))
}

/// `authorized`: a bearer token whose SHA-256 matches, compared in constant time.
fn authorized(value: Option<&str>, digest: Option<&[u8; 32]>) -> bool {
    let Some(digest) = digest else { return true };
    let Some(value) = value else { return false };
    let Some(token) = value
        .get(..7)
        .filter(|prefix| prefix.eq_ignore_ascii_case("bearer "))
        .map(|_| &value[7..])
        .filter(|token| !token.is_empty() && !token.chars().any(char::is_whitespace))
    else {
        return false;
    };
    let actual: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    actual
        .iter()
        .zip(digest)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

struct Server {
    serving: Arc<Serving>,
    hosts: Vec<Option<String>>,
    origins: Vec<String>,
    clients: Vec<crate::config_check::Network>,
    digest: Option<[u8; 32]>,
    port: u16,
    running: AtomicUsize,
    mcp: McpServer,
}

/// Aborts the work of a request whose connection closed before it finished.
pub(crate) struct AbortOnDrop(pub Option<Cancellation>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel.abort();
        }
    }
}

/// Counts a request against `max_concurrent` until it ends.
pub(crate) struct Running<'a>(pub &'a AtomicUsize);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(crate) fn header<'a>(request: &'a Request<Incoming>, name: &str) -> Option<&'a str> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

impl Server {
    async fn handle(
        self: Arc<Self>,
        request: Request<Incoming>,
        peer: std::net::SocketAddr,
    ) -> Response<Body> {
        let mut headers: Vec<(&'static str, String)> = vec![
            ("cache-control", "no-store".into()),
            ("x-content-type-options", "nosniff".into()),
        ];
        let reply = self.route(request, peer, &mut headers).await;
        // Headers of the reply replace the ones set before, as setHeader then writeHead did.
        for (name, value) in reply.headers {
            headers.retain(|(existing, _)| *existing != name);
            headers.push((name, value));
        }
        let mut response = Response::builder().status(
            StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        );
        for (name, value) in headers {
            response = response.header(name, value);
        }
        response
            .body(Full::new(Bytes::from(reply.body)))
            .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
    }

    async fn route(
        self: &Arc<Self>,
        request: Request<Incoming>,
        peer: std::net::SocketAddr,
        headers: &mut Vec<(&'static str, String)>,
    ) -> Reply {
        if !crate::config_check::client_allowed(&self.clients, peer.ip()) {
            return Reply::error(
                403,
                "CLIENT_FORBIDDEN",
                "This client address is not allowed.",
            );
        }
        let target = header(&request, "host").and_then(|host| request_target(host, self.port));
        let Some((_, authority)) =
            target.filter(|(name, _)| self.hosts.iter().any(|host| host.as_deref() == Some(name)))
        else {
            return Reply::error(403, "HOST_FORBIDDEN", "This Host is not allowed.");
        };
        let source = header(&request, "origin").map(str::to_owned);
        if let Some(source) = &source
            && !self.origins.contains(source)
        {
            return Reply::error(403, "ORIGIN_FORBIDDEN", "This Origin is not allowed.");
        }
        if let Some(source) = source.as_ref().filter(|source| !source.is_empty()) {
            headers.push(("access-control-allow-origin", source.clone()));
            headers.push((
                "access-control-expose-headers",
                "Mcp-Session-Id, MCP-Protocol-Version".into(),
            ));
            headers.push(("vary", "Origin".into()));
        }
        if request.method() == hyper::Method::OPTIONS
            && source.as_ref().is_some_and(|source| !source.is_empty())
            && header(&request, "access-control-request-method").is_some()
        {
            return Reply {
                status: 204,
                headers: vec![
                    ("access-control-allow-methods", "GET, POST, DELETE, OPTIONS".into()),
                    (
                        "access-control-allow-headers",
                        "Authorization, Content-Type, MCP-Protocol-Version, Mcp-Method, Mcp-Name, Mcp-Session-Id, Last-Event-ID".into(),
                    ),
                ],
                body: String::new(),
            };
        }
        if !authorized(header(&request, "authorization"), self.digest.as_ref()) {
            let mut reply = Reply::error(401, "AUTH_REQUIRED", "Valid authentication is required.");
            reply.headers.push((
                "www-authenticate",
                r#"Bearer realm="Knowledge-Nest""#.into(),
            ));
            return reply;
        }
        let limits = &self.serving.request.settings.limits;
        if self.running.load(Ordering::SeqCst) >= limits.max_concurrent {
            return Reply::error(429, "REQUEST_LIMIT", "Too many concurrent requests.");
        }
        let path = request
            .uri()
            .path_and_query()
            .map_or("/", |path| path.as_str())
            .to_owned();
        if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
            return Reply::error(400, "INVALID_REQUEST", "Expected a local request path.");
        }
        let length: u64 = header(&request, "content-length")
            .and_then(|length| length.trim().parse().ok())
            .unwrap_or(0);
        if length > limits.max_request_bytes {
            return Reply::error(413, "BODY_LIMIT", "The request body is too large.");
        }
        let pathname = url::Url::parse(&format!("http://{authority}"))
            .and_then(|base| base.join(&path))
            .map(|url| url.path().to_owned())
            .unwrap_or(path);
        self.running.fetch_add(1, Ordering::SeqCst);
        let _running = Running(&self.running);
        let cancel = Cancellation::default();
        let mut guard = AbortOnDrop(Some(cancel.clone()));
        let timeout = Duration::from_secs(limits.request_timeout_seconds);
        let reply = tokio::time::timeout(
            timeout,
            self.clone().respond(request, pathname, cancel.clone()),
        )
        .await;
        match reply {
            Ok(reply) => {
                guard.0 = None;
                reply
            }
            Err(_) => Reply::error(
                408,
                "REQUEST_TIMEOUT",
                "The request exceeded its time limit.",
            ),
        }
    }

    async fn respond(
        self: Arc<Self>,
        request: Request<Incoming>,
        pathname: String,
        cancel: Cancellation,
    ) -> Reply {
        let settings = &self.serving.request.settings;
        let route = match pathname.as_str() {
            "/v1/responses" => "responses",
            "/v1/search" => "search",
            "/mcp" => "mcp",
            _ => "",
        };
        let method = request.method().clone();
        let content_type = header(&request, "content-type")
            .unwrap_or_default()
            .to_owned();
        let accept = header(&request, "accept").unwrap_or_default().to_owned();
        let version = header(&request, "mcp-protocol-version").map(str::to_owned);
        let limit = settings.limits.max_request_bytes;
        let mut body = request.into_body();
        let mut bytes = Vec::new();
        loop {
            match body.frame().await {
                None => break,
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        if (bytes.len() + data.len()) as u64 > limit {
                            return Reply::error(
                                413,
                                "BODY_LIMIT",
                                "The request body is too large.",
                            );
                        }
                        bytes.extend_from_slice(&data);
                    }
                }
                Some(Err(_)) => {
                    return Reply::error(408, "REQUEST_CLOSED", "The request ended.");
                }
            }
        }
        if route.is_empty() || !settings.endpoints.iter().any(|endpoint| endpoint == route) {
            return Reply::error(404, "NOT_FOUND", "The endpoint is unavailable.");
        }
        if route == "mcp" {
            if !matches!(
                method,
                hyper::Method::GET | hyper::Method::POST | hyper::Method::DELETE
            ) {
                let mut reply = Reply::error(
                    405,
                    "METHOD_NOT_ALLOWED",
                    "Use GET, POST or DELETE for MCP.",
                );
                reply.headers.push(("allow", "GET, POST, DELETE".into()));
                return reply;
            }
            let server = self.clone();
            let post = method == hyper::Method::POST;
            return tokio::task::spawn_blocking(move || {
                server.mcp_http(
                    post,
                    &accept,
                    &content_type,
                    version.as_deref(),
                    &bytes,
                    &cancel,
                )
            })
            .await
            .unwrap_or_else(|_| {
                Reply::error(
                    500,
                    "EXECUTION_FAILED",
                    "The request could not be completed.",
                )
            });
        }
        if method != hyper::Method::POST {
            let mut reply = Reply::error(405, "METHOD_NOT_ALLOWED", "Use POST for this endpoint.");
            reply.headers.push(("allow", "POST".into()));
            return reply;
        }
        // `/^application\/json(?:\s*;|$)/iu`
        let json = content_type
            .get(..16)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("application/json"))
            && content_type
                .get(16..)
                .is_some_and(|rest| rest.is_empty() || rest.trim_start().starts_with(';'));
        if !json {
            return Reply::error(415, "CONTENT_TYPE", "Use application/json.");
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return Reply::error(
                400,
                "INVALID_JSON",
                "Send the request body as UTF-8 JSON. The body is not UTF-8; on Windows, save the JSON to a UTF-8 file and send it with curl --data-binary @file.",
            );
        };
        let body = match serde_json::from_str::<Value>(text) {
            Ok(body) => body,
            Err(error) => {
                return Reply::error(
                    400,
                    "INVALID_JSON",
                    &format!("Provide valid JSON: {error}."),
                );
            }
        };
        let invalid = || {
            Reply::error(
                400,
                "INVALID_REQUEST",
                "The request contains invalid or unsupported fields.",
            )
        };
        let serving = self.serving.clone();
        if route == "search" {
            let Some(search) = search_request(&body) else {
                return invalid();
            };
            return tokio::task::spawn_blocking(move || {
                match serving.search(&search.0, search.1, search.2, search.3, &cancel) {
                    Ok(output) => Reply::json(200, output.to_js_json()),
                    Err(error) => Reply::failure(&error),
                }
            })
            .await
            .unwrap_or_else(|_| {
                Reply::error(
                    500,
                    "EXECUTION_FAILED",
                    "The request could not be completed.",
                )
            });
        }
        let Some((input, requested)) = answer_request(&body) else {
            return invalid();
        };
        let Some(model) = serving.request.model.as_ref() else {
            return Reply::error(
                503,
                "MODEL_REQUIRED",
                "Set the answer model in the KnowledgeNestfile and restart serve.",
            );
        };
        if requested.is_some_and(|requested| requested != model.model) {
            return Reply::error(
                400,
                "MODEL_MISMATCH",
                &format!(
                    "This project answers with {}. Leave out model, or set it to {}.",
                    model.model, model.model
                ),
            );
        }
        let query = match question(&input) {
            Ok(query) => query,
            Err(reply) => return reply,
        };
        tokio::task::spawn_blocking(move || match serving.answer(&query, &cancel) {
            Ok(output) => Reply::json(200, output.to_js_json()),
            Err(error) => Reply::failure(&error),
        })
        .await
        .unwrap_or_else(|_| {
            Reply::error(
                500,
                "EXECUTION_FAILED",
                "The request could not be completed.",
            )
        })
    }

    /// `/mcp` in the stateless mode of the TypeScript SDK.
    fn mcp_http(
        &self,
        post: bool,
        accept: &str,
        content_type: &str,
        version: Option<&str>,
        body: &[u8],
        cancel: &Cancellation,
    ) -> Reply {
        let rpc_error = |status: u16, code: i64, message: &str| Reply {
            status,
            headers: vec![("content-type", "application/json".into())],
            body: format!(
                r#"{{"jsonrpc":"2.0","error":{{"code":{code},"message":{}}},"id":null}}"#,
                ConfigValue::String(message.into()).to_js_json()
            ),
        };
        if !post {
            let mut reply = rpc_error(405, -32000, "Method not allowed.");
            reply.headers.push(("allow", "POST".into()));
            return reply;
        }
        if !(accept.contains("application/json") && accept.contains("text/event-stream")) {
            return rpc_error(
                406,
                -32000,
                "Not Acceptable: Client must accept both application/json and text/event-stream",
            );
        }
        if !content_type.contains("application/json") {
            return rpc_error(
                415,
                -32000,
                "Unsupported Media Type: Content-Type must be application/json",
            );
        }
        let Some(parsed) = std::str::from_utf8(body)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
        else {
            return rpc_error(400, -32700, "Parse error: Invalid JSON");
        };
        let messages = match parsed {
            Value::Array(messages) if messages.is_empty() => {
                return rpc_error(400, -32600, "Bad Request: empty JSON-RPC batch");
            }
            Value::Array(messages) => messages,
            message => vec![message],
        };
        if !messages.iter().all(valid_message) {
            return rpc_error(
                400,
                -32600,
                "Bad Request: the request body is not a valid JSON-RPC message",
            );
        }
        let initializing = messages
            .iter()
            .any(|message| message.get("method").and_then(Value::as_str) == Some("initialize"));
        if let Some(version) = version
            && !initializing
            && !SUPPORTED.contains(&version)
        {
            return rpc_error(
                400,
                -32000,
                &format!(
                    "Bad Request: Unsupported protocol version: {version} (supported versions: {})",
                    SUPPORTED.join(", ")
                ),
            );
        }
        let requests: Vec<&Value> = messages
            .iter()
            .filter(|message| message.get("method").is_some() && message.get("id").is_some())
            .collect();
        if requests.is_empty() {
            return Reply {
                status: 202,
                headers: Vec::new(),
                body: String::new(),
            };
        }
        let mut events = String::new();
        for request in requests {
            let id = request["id"].to_string();
            let answer = match self.mcp_call(request, cancel) {
                Ok(result) => format!(r#"{{"result":{result},"jsonrpc":"2.0","id":{id}}}"#),
                Err((code, message)) => format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":{code},"message":{}}}}}"#,
                    ConfigValue::String(message).to_js_json()
                ),
            };
            events.push_str(&format!("event: message\ndata: {answer}\n\n"));
        }
        Reply {
            status: 200,
            headers: vec![
                ("content-type", "text/event-stream".into()),
                ("cache-control", "no-cache, no-transform".into()),
                ("x-accel-buffering", "no".into()),
            ],
            body: events,
        }
    }

    /// One JSON-RPC request: its result as JSON text, or an error code and message.
    fn mcp_call(&self, request: &Value, cancel: &Cancellation) -> Result<String, (i64, String)> {
        let params = request.get("params");
        match request["method"].as_str().unwrap_or_default() {
            "initialize" => {
                let requested = params
                    .and_then(|params| params.get("protocolVersion"))
                    .and_then(Value::as_str);
                let version = requested
                    .filter(|version| SUPPORTED.contains(version))
                    .unwrap_or(SUPPORTED[0]);
                Ok(format!(
                    r#"{{"protocolVersion":"{version}","capabilities":{{"tools":{{"listChanged":true}}}},"serverInfo":{{"name":"knowledge-nest","version":"{}"}}}}"#,
                    env!("CARGO_PKG_VERSION")
                ))
            }
            "ping" => Ok("{}".into()),
            "tools/list" => Ok(self.mcp.tools_json()),
            "tools/call" => {
                let issue = |expected: &str, path: &[&str], received: &str| {
                    let path: Vec<String> = path
                        .iter()
                        .map(|part| format!("      \"{part}\""))
                        .collect();
                    (
                        -32602,
                        format!(
                            "Invalid tools/call request: [\n  {{\n    \"expected\": \"{expected}\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n{}\n    ],\n    \"message\": \"Invalid input: expected {expected}, received {received}\"\n  }}\n]",
                            path.join(",\n")
                        ),
                    )
                };
                let Some(Value::Object(params)) = params else {
                    return Err(issue("object", &["params"], received_kind(params)));
                };
                let Some(Value::String(name)) = params.get("name") else {
                    return Err(issue(
                        "string",
                        &["params", "name"],
                        received_kind(params.get("name")),
                    ));
                };
                let arguments = match params.get("arguments") {
                    None => None,
                    Some(Value::Object(arguments)) => Some(arguments),
                    Some(other) => {
                        return Err(issue(
                            "record",
                            &["params", "arguments"],
                            received_kind(Some(other)),
                        ));
                    }
                };
                self.mcp
                    .call_json(name, arguments, cancel)
                    .map_err(|message| (-32602, message))
            }
            _ => Err((-32601, "Method not found".into())),
        }
    }
}

const SUPPORTED: [&str; 5] = [
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];

fn received_kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

/// A JSON-RPC 2.0 request, notification or response.
fn valid_message(message: &Value) -> bool {
    let Some(object) = message.as_object() else {
        return false;
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return false;
    }
    let id_valid = object
        .get("id")
        .is_none_or(|id| id.is_string() || id.as_i64().is_some());
    match object.get("method") {
        Some(Value::String(_)) => id_valid && object.get("params").is_none_or(Value::is_object),
        Some(_) => false,
        None => {
            object.get("id").is_some()
                && id_valid
                && (object.get("result").is_some_and(Value::is_object)
                    || object.get("error").is_some_and(Value::is_object))
        }
    }
}

/// `String.prototype.trim()` leaves text.
fn nonblank(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.trim_matches(crate::registry::js_space).is_empty())
}

fn safe_integer(value: &Value, low: f64, high: f64) -> Option<f64> {
    value
        .as_f64()
        .filter(|number| number.fract() == 0.0 && *number >= low && *number <= high)
}

/// The query and options of a `/v1/search` body: `{ query, limit?, budget_chars?, source_ids? }`.
#[allow(clippy::type_complexity)]
fn search_request(body: &Value) -> Option<(String, Option<f64>, Option<f64>, Option<Vec<String>>)> {
    let object = body.as_object()?;
    if object
        .keys()
        .any(|key| !["query", "limit", "budget_chars", "source_ids"].contains(&key.as_str()))
    {
        return None;
    }
    let query = object.get("query").filter(|query| nonblank(query))?;
    let number = |name: &str, high: f64| -> Option<Option<f64>> {
        match object.get(name) {
            None => Some(None),
            Some(value) => safe_integer(value, 1.0, high).map(Some),
        }
    };
    let limit = number("limit", 100.0)?;
    let budget = number("budget_chars", 1_000_000.0)?;
    let ids = match object.get("source_ids") {
        None => None,
        Some(Value::Array(ids)) if (1..=100).contains(&ids.len()) => Some(
            ids.iter()
                .map(|id| {
                    id.as_str()
                        .filter(|id| crate::mcp::valid_source_id(id))
                        .map(str::to_owned)
                })
                .collect::<Option<Vec<_>>>()?,
        ),
        Some(_) => return None,
    };
    Some((query.as_str()?.to_owned(), limit, budget, ids))
}

/// The input and requested model of a `/v1/responses` body.
fn answer_request(body: &Value) -> Option<(Value, Option<String>)> {
    let object = body.as_object()?;
    if object
        .keys()
        .any(|key| !["input", "model", "stream", "store", "background"].contains(&key.as_str()))
    {
        return None;
    }
    for flag in ["stream", "store", "background"] {
        if object
            .get(flag)
            .is_some_and(|value| *value != Value::Bool(false))
        {
            return None;
        }
    }
    let model = match object.get("model") {
        None => None,
        Some(model) if nonblank(model) => Some(model.as_str()?.to_owned()),
        Some(_) => return None,
    };
    let input = object.get("input")?;
    let valid = match input {
        Value::String(_) => nonblank(input),
        Value::Array(messages) => !messages.is_empty() && messages.iter().all(valid_input_message),
        _ => false,
    };
    valid.then(|| (input.clone(), model))
}

fn valid_input_message(message: &Value) -> bool {
    let Some(object) = message.as_object() else {
        return false;
    };
    if object
        .keys()
        .any(|key| !["type", "role", "content"].contains(&key.as_str()))
    {
        return false;
    }
    if object
        .get("type")
        .is_some_and(|kind| kind.as_str() != Some("message"))
    {
        return false;
    }
    if !object
        .get("role")
        .and_then(Value::as_str)
        .is_some_and(|role| ["user", "assistant", "system", "developer"].contains(&role))
    {
        return false;
    }
    match object.get("content") {
        Some(Value::String(_)) => true,
        Some(Value::Array(parts)) => parts.iter().all(|part| {
            part.as_object().is_some_and(|part| {
                part.keys().all(|key| key == "type" || key == "text")
                    && part
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| kind == "input_text" || kind == "output_text")
                    && part.get("text").is_some_and(Value::is_string)
            })
        }),
        _ => false,
    }
}

/// `question`: the text of the input, or the last user message.
fn question(input: &Value) -> Result<String, Reply> {
    if let Some(text) = input.as_str() {
        return Ok(text.to_owned());
    }
    let content = input.as_array().and_then(|messages| {
        messages
            .iter()
            .rev()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
            .and_then(|message| message.get("content"))
    });
    if let Some(Value::Array(parts)) = content
        && parts
            .iter()
            .any(|part| part.get("type").and_then(Value::as_str) != Some("input_text"))
    {
        return Err(Reply::error(
            400,
            "INVALID_INPUT",
            "The last user message must contain input text.",
        ));
    }
    let text = match content {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => Some(
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    };
    match text {
        Some(text) if !text.trim_matches(crate::registry::js_space).is_empty() => Ok(text),
        _ => Err(Reply::error(
            400,
            "INVALID_INPUT",
            "Provide a nonempty user message.",
        )),
    }
}

/// Serves HTTP until `cancel` aborts. Reports the port through `listening` once bound.
pub fn serve_http(serving: Arc<Serving>, cancel: &Cancellation) -> Result<(), String> {
    let settings = &serving.request.settings;
    let listener =
        std::net::TcpListener::bind((settings.host.as_str(), settings.port)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AddrInUse {
                format!(
                    "listen EADDRINUSE: address already in use {}:{}",
                    settings.host, settings.port
                )
            } else {
                format!("listen {}:{}: {error}", settings.host, settings.port)
            }
        })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let digest = serving
        .request
        .token
        .as_deref()
        .map(|token| Sha256::digest(token.as_bytes()).into());
    let database = serving
        .initial_database()
        .ok_or("The serving build is not open.")?;
    let host: Arc<dyn McpHost> = serving.clone();
    let server = Arc::new(Server {
        hosts: settings
            .access
            .allowed_hosts
            .iter()
            .map(|host| hostname(host))
            .collect(),
        origins: settings.access.allowed_origins.clone(),
        clients: settings.access.clients(),
        digest,
        port,
        running: AtomicUsize::new(0),
        mcp: McpServer::new(database, Some(host), true).with_about(serving.about.clone()),
        serving: serving.clone(),
    });
    let header_timeout = Duration::from_secs(settings.limits.request_timeout_seconds.min(60));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        let listener =
            tokio::net::TcpListener::from_std(listener).map_err(|error| error.to_string())?;
        serving.host.listening(&ConfigValue::Object(vec![
            ("port".into(), ConfigValue::Number(f64::from(port))),
            (
                "buildId".into(),
                field(&serving.initial, "build_id")
                    .cloned()
                    .unwrap_or(ConfigValue::Null),
            ),
        ]));
        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                () = tokio::time::sleep(Duration::from_millis(100)) => {
                    if cancel.check().is_err() {
                        return Ok(());
                    }
                    continue;
                }
            };
            let Ok((stream, peer)) = accepted else {
                continue;
            };
            let server = server.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |request| {
                    let server = server.clone();
                    async move { Ok::<_, Infallible>(server.handle(request, peer).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .timer(hyper_util::rt::TokioTimer::new())
                    .header_read_timeout(header_timeout)
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
            });
        }
    })
}
