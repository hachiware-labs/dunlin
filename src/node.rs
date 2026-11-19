use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use napi::bindgen_prelude::{
    AsyncTask, Buffer, Env, Error, Function, Object, Result, Status, Task, ToNapiValue, TypeName,
    Unknown, ValueType,
};
use napi::sys::{napi_env, napi_value};
use napi_derive::napi;
use serde::Serialize;
use serde_json::Value;

use crate::{
    Cancellation, CorpusBackend, DocumentRef, Evidence, ExploreOptions, KnowledgeBase,
    RetrievalMethod, engine::KeywordOptions, source::DocumentInput,
};

#[napi]
pub struct NativeCancellation(Cancellation);

#[napi]
impl NativeCancellation {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self(Cancellation::default())
    }

    #[napi]
    pub fn abort(&self) {
        self.0.abort();
    }
}

/// An open index, with the vector metadata its searches read once.
#[napi]
pub struct NativeDatabase(
    Option<Arc<KnowledgeBase>>,
    Arc<crate::search_stage::VectorCache>,
);

#[napi]
pub struct NativeExecutionLock(Option<File>);

#[napi]
impl NativeExecutionLock {
    #[napi]
    pub fn release(&mut self) {
        self.0.take();
    }
}

#[napi]
pub fn acquire_execution_lock(path: String) -> Result<NativeExecutionLock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| {
            native_error(crate::Error::Io(
                "Could not open the project execution lock",
            ))
        })?;
    file.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => native_error(crate::Error::Busy),
        TryLockError::Error(_) => native_error(crate::Error::Io(
            "Could not acquire the project execution lock",
        )),
    })?;
    Ok(NativeExecutionLock(Some(file)))
}

#[napi(object)]
pub struct JapaneseKeywordOptions {
    pub user_dictionary: Option<String>,
}

#[napi]
pub fn open_native(
    path: String,
    cancel: &NativeCancellation,
    backend: Option<String>,
    keyword_ngrams: Option<bool>,
    keyword_bigrams: Option<bool>,
    corpus_path: Option<String>,
    keyword_japanese: Option<JapaneseKeywordOptions>,
) -> AsyncTask<Job<NativeDatabase>> {
    let cancel = cancel.0.clone();
    task(move || {
        let backend = match backend.as_deref() {
            None | Some("sqlite") => CorpusBackend::Sqlite,
            Some("json") => CorpusBackend::Json,
            _ => {
                return Err(crate::Error::Invalid(
                    "Expected corpus backend sqlite or json",
                ));
            }
        };
        let keyword = KeywordOptions {
            ngrams: keyword_ngrams.unwrap_or(false),
            bigrams: keyword_bigrams.unwrap_or(false),
            japanese: keyword_japanese.is_some(),
            user_dictionary: keyword_japanese
                .as_ref()
                .and_then(|options| options.user_dictionary.as_deref())
                .map(Path::new),
        };
        Ok(NativeDatabase(
            Some(Arc::new(KnowledgeBase::open_with_options(
                Path::new(&path),
                backend,
                corpus_path.as_deref().map(Path::new),
                keyword,
                &cancel,
            )?)),
            Arc::default(),
        ))
    })
}

#[napi]
pub fn inspect_index(path: String, cancel: &NativeCancellation) -> AsyncTask<Job<Json>> {
    let cancel = cancel.0.clone();
    task(move || json(KnowledgeBase::inspect_index(Path::new(&path), &cancel)?))
}

#[napi]
pub fn analyze_japanese(
    texts: Vec<String>,
    dictionary: Option<String>,
    cancel: &NativeCancellation,
) -> AsyncTask<Job<Json>> {
    let cancel = cancel.0.clone();
    task(move || {
        cancel.check()?;
        let tokenizer =
            crate::japanese::JapaneseTokenizer::open(dictionary.as_deref().map(Path::new))?;
        let tokens = texts
            .iter()
            .map(|text| {
                cancel.check()?;
                tokenizer.analyze(text)
            })
            .collect::<crate::Result<Vec<_>>>()?;
        json(tokens)
    })
}

#[napi]
pub fn merge_user_dictionary(
    existing: Option<String>,
    selected: String,
    cancel: &NativeCancellation,
) -> AsyncTask<Job<String>> {
    let cancel = cancel.0.clone();
    task(move || {
        cancel.check()?;
        crate::japanese::merge_user_records(
            existing.as_deref().map(Path::new),
            Path::new(&selected),
        )
    })
}

#[napi(object)]
pub struct SourceFileRequest {
    pub path: String,
    /// Largest accepted size in bytes.
    pub limit: f64,
}

#[napi(object)]
pub struct SourceFileRead {
    pub bytes: Option<Buffer>,
    pub modified_at_ms: Option<f64>,
    /// `not_file`, `too_large`, `changed` or `io`.
    pub error: Option<String>,
    pub message: Option<String>,
}

/// Reads source files in parallel, stopping before the files that would exceed `max_bytes`.
/// Each result carries the bytes or the reason for failure; callers request the rest again.
#[napi]
pub fn read_source_files(
    files: Vec<SourceFileRequest>,
    max_bytes: f64,
    cancel: &NativeCancellation,
) -> AsyncTask<Job<Vec<SourceFileRead>>> {
    use rayon::prelude::*;
    let cancel = cancel.0.clone();
    task(move || {
        cancel.check()?;
        // Read the longest prefix whose current sizes fit the budget, and at least one file.
        let sizes: Vec<u64> = files
            .par_iter()
            .map(|request| std::fs::metadata(&request.path).map_or(0, |entry| entry.len()))
            .collect();
        let mut total = 0;
        let count = sizes
            .iter()
            .take_while(|&&size| {
                total += size;
                total as f64 <= max_bytes
            })
            .count()
            .max(1)
            .min(files.len());
        files
            .into_par_iter()
            .take(count)
            .map(|request| {
                cancel.check()?;
                let read = crate::source_files::read_source_file(
                    Path::new(&request.path),
                    request.limit.max(0.0) as u64,
                );
                Ok(match read {
                    Ok(file) => SourceFileRead {
                        bytes: Some(file.bytes.into()),
                        modified_at_ms: file.modified_at_ms.map(|value| value as f64),
                        error: None,
                        message: None,
                    },
                    Err(failure) => {
                        use crate::source_files::ReadFailure;
                        let (code, message) = match failure {
                            ReadFailure::NotFile => ("not_file", None),
                            ReadFailure::TooLarge => ("too_large", None),
                            ReadFailure::Changed => ("changed", None),
                            ReadFailure::Io(message) => ("io", Some(message)),
                        };
                        SourceFileRead {
                            bytes: None,
                            modified_at_ms: None,
                            error: Some(code.into()),
                            message,
                        }
                    }
                })
            })
            .collect()
    })
}

#[napi(object)]
pub struct OriginalFile {
    pub sha256: String,
    pub bytes: Buffer,
}

/// Stores originals as `<directory>/<sha256>.bin` in parallel. Returns an error message or
/// `null` for each file.
#[napi]
pub fn store_originals(
    directory: String,
    files: Vec<OriginalFile>,
    cancel: &NativeCancellation,
) -> AsyncTask<Job<Vec<Option<String>>>> {
    use rayon::prelude::*;
    // Copy out of JavaScript memory before leaving the main thread.
    let files: Vec<(String, Vec<u8>)> = files
        .into_iter()
        .map(|file| (file.sha256, file.bytes.to_vec()))
        .collect();
    let cancel = cancel.0.clone();
    task(move || {
        cancel.check()?;
        let directory = PathBuf::from(directory);
        if let Err(message) = crate::source_files::original_directory(&directory) {
            return Ok(vec![Some(message); files.len()]);
        }
        files
            .par_iter()
            .map(|(sha256, bytes)| {
                cancel.check()?;
                Ok(crate::source_files::store_original(&directory, sha256, bytes).err())
            })
            .collect()
    })
}

/// Reads `Dunlinfile.yaml` text. Returns `{ syntaxError: { line, message } }`, or
/// `{ config, lines }` where `lines` lists `[path, line]` for every value.
#[napi]
pub fn parse_config(text: String) -> Result<String> {
    use crate::yaml_config::{PathPart, parse_config};
    use serde::ser::{SerializeMap, Serializer};

    struct Path<'a>(&'a [PathPart]);
    impl Serialize for Path<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
            for part in self.0 {
                match part {
                    PathPart::Key(key) => seq.serialize_element(key)?,
                    PathPart::Index(index) => seq.serialize_element(index)?,
                }
            }
            seq.end()
        }
    }
    struct Output(
        std::result::Result<crate::yaml_config::ParsedConfig, crate::yaml_config::SyntaxError>,
    );
    impl Serialize for Output {
        fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(None)?;
            match &self.0 {
                Ok(parsed) => {
                    map.serialize_entry("config", &parsed.value)?;
                    let lines: Vec<(Path<'_>, usize)> = parsed
                        .lines
                        .iter()
                        .map(|(path, line)| (Path(path), *line))
                        .collect();
                    map.serialize_entry("lines", &lines)?;
                }
                Err(error) => {
                    #[derive(Serialize)]
                    struct Syntax<'a> {
                        line: Option<usize>,
                        message: &'a str,
                    }
                    map.serialize_entry(
                        "syntaxError",
                        &Syntax {
                            line: error.line,
                            message: error.message,
                        },
                    )?;
                }
            }
            map.end()
        }
    }
    serde_json::to_string(&Output(parse_config(&text)))
        .map_err(|_| native_error(crate::Error::Index))
}

#[derive(serde::Deserialize)]
struct ValidateOptions {
    file: String,
    #[serde(default)]
    lines: Vec<(Vec<crate::config_value::ConfigValue>, usize)>,
    #[serde(default)]
    tools: Vec<(String, String)>,
    #[serde(default)]
    cwd: String,
}

fn config_input(text: &str) -> Result<crate::config_value::ConfigValue> {
    serde_json::from_str(text)
        .map_err(|_| native_error(crate::Error::Invalid("Invalid configuration")))
}

fn line_index(
    lines: Vec<(Vec<crate::config_value::ConfigValue>, usize)>,
) -> crate::config_check::Lines {
    use crate::config_value::ConfigValue;
    crate::config_check::Lines::new(
        lines
            .into_iter()
            .map(|(path, line)| {
                let text = path
                    .into_iter()
                    .map(|part| match part {
                        ConfigValue::String(text) => text,
                        ConfigValue::Number(number) => (number as u64).to_string(),
                        _ => String::new(),
                    })
                    .collect();
                (text, line)
            })
            .collect(),
    )
}

/// Diagnostics without a file, for callers that report them through their own helper.
struct Issues<'a>(&'a [crate::config_check::Diagnostic]);

impl Serialize for Issues<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        struct Item<'a>(&'a crate::config_check::Diagnostic);
        impl Serialize for Item<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("code", self.0.code)?;
                map.serialize_entry("path", &self.0.path)?;
                map.serialize_entry("expected", &self.0.expected)?;
                map.serialize_entry("hint", &self.0.hint)?;
                map.end()
            }
        }
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for item in self.0 {
            seq.serialize_element(&Item(item))?;
        }
        seq.end()
    }
}

/// Validates an MCP or RAG configuration (JSON). Returns `{ errors, warnings, plan, serveSettings,
/// evalSettings }`; the plan maps each stage to `{ use, name, args }` entries.
#[napi]
pub fn validate_config(config: String, options: String) -> Result<String> {
    use crate::config_check::{Environment, FileDiagnostic, Report, validate_rag};
    use serde::ser::{SerializeMap, Serializer};

    let config = config_input(&config)?;
    let options: ValidateOptions = serde_json::from_str(&options)
        .map_err(|_| native_error(crate::Error::Invalid("Invalid validation options")))?;
    let lines = line_index(options.lines);
    let mut report = Report::new(&lines);
    let environment = Environment {
        file: &options.file,
        tools: &options.tools,
        cwd: options.cwd,
    };
    let validation = validate_rag(&config, &environment, &mut report);

    struct Output<'a> {
        file: &'a str,
        report: &'a Report<'a>,
        validation: &'a crate::config_check::Validation,
    }
    impl Serialize for Output<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
            struct Diagnostics<'a>(&'a str, &'a [crate::config_check::Diagnostic]);
            impl Serialize for Diagnostics<'_> {
                fn serialize<S: Serializer>(
                    &self,
                    serializer: S,
                ) -> std::result::Result<S::Ok, S::Error> {
                    use serde::ser::SerializeSeq;
                    let mut seq = serializer.serialize_seq(Some(self.1.len()))?;
                    for diagnostic in self.1 {
                        seq.serialize_element(&FileDiagnostic {
                            file: self.0,
                            diagnostic,
                        })?;
                    }
                    seq.end()
                }
            }
            struct Plan<'a>(&'a [(&'static str, Vec<crate::config_check::PlanEntry>)]);
            impl Serialize for Plan<'_> {
                fn serialize<S: Serializer>(
                    &self,
                    serializer: S,
                ) -> std::result::Result<S::Ok, S::Error> {
                    let mut map = serializer.serialize_map(Some(self.0.len()))?;
                    for (stage, entries) in self.0 {
                        map.serialize_entry(stage, entries)?;
                    }
                    map.end()
                }
            }
            let mut map = serializer.serialize_map(Some(5))?;
            map.serialize_entry("errors", &Diagnostics(self.file, &self.report.errors))?;
            map.serialize_entry("warnings", &Diagnostics(self.file, &self.report.warnings))?;
            map.serialize_entry("plan", &Plan(&self.validation.plan))?;
            map.serialize_entry("serveSettings", &self.validation.serve)?;
            map.serialize_entry("evalSettings", &self.validation.eval)?;
            map.end()
        }
    }
    serde_json::to_string(&Output {
        file: &options.file,
        report: &report,
        validation: &validation,
    })
    .map_err(|_| native_error(crate::Error::Index))
}

/// Validates a serve section (JSON or `undefined` as an empty string). Returns
/// `{ settings, issues }` where each issue has `code`, `path`, `expected` and `hint`.
#[napi]
pub fn validate_serve(project_type: String, input: String) -> Result<String> {
    use crate::config_check::{Lines, Report};
    let input = if input.is_empty() {
        None
    } else {
        Some(config_input(&input)?)
    };
    let lines = Lines::new(Vec::new());
    let mut report = Report::new(&lines);
    let settings = crate::config_check::validate_serve(&project_type, input.as_ref(), &mut report);
    #[derive(Serialize)]
    struct Output<'a> {
        settings: Option<crate::config_value::ConfigValue>,
        issues: Issues<'a>,
    }
    serde_json::to_string(&Output {
        settings,
        issues: Issues(&report.errors),
    })
    .map_err(|_| native_error(crate::Error::Index))
}

/// Checks a `tools` list (JSON) against the bundled catalog of `[name, version]` pairs.
#[napi]
pub fn check_tools(tools: String, catalog: String) -> Result<String> {
    use crate::config_check::{Lines, Report};
    let tools = config_input(&tools)?;
    let catalog: Vec<(String, String)> = serde_json::from_str(&catalog)
        .map_err(|_| native_error(crate::Error::Invalid("Invalid tool catalog")))?;
    let lines = Lines::new(Vec::new());
    let mut report = Report::new(&lines);
    crate::config_check::check_tools(Some(&tools), &catalog, &mut report);
    serde_json::to_string(&Issues(&report.errors)).map_err(|_| native_error(crate::Error::Index))
}

/// Whether `trial_limits` (JSON) holds only nonnegative integer limits.
#[napi]
pub fn trial_limits_valid(limits: String) -> Result<bool> {
    Ok(crate::config_check::trial_limits_valid(&config_input(
        &limits,
    )?))
}

/// Arguments for host calls: call ID, kind (`parse` or `progress`), JSON arguments, bytes.
type HostArgs = (u32, String, String, Buffer);
type HostFunction = napi::threadsafe_function::ThreadsafeFunction<
    HostArgs,
    Unknown<'static>,
    HostArgs,
    Status,
    false,
>;

/// Senders for host calls waiting for JavaScript, by call ID.
type HostCalls = std::sync::Mutex<
    std::collections::HashMap<u32, std::sync::mpsc::Sender<std::result::Result<String, String>>>,
>;

fn host_calls() -> &'static HostCalls {
    static CALLS: std::sync::OnceLock<HostCalls> = std::sync::OnceLock::new();
    CALLS.get_or_init(Default::default)
}

static NEXT_HOST_CALL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Delivers the result of a host call. JavaScript calls this when its parser finishes.
#[napi]
pub fn complete_host_call(id: u32, result: Option<String>, error: Option<String>) {
    let sender = host_calls()
        .lock()
        .ok()
        .and_then(|mut calls| calls.remove(&id));
    if let Some(sender) = sender {
        let _ = sender.send(match (result, error) {
            (_, Some(error)) => Err(error),
            (Some(result), None) => Ok(result),
            (None, None) => Err("The parser returned no result".into()),
        });
    }
}

struct JsHost {
    function: HostFunction,
    /// The trial the call runs in, which counts model requests.
    trial: Option<Arc<crate::trial::Trial>>,
}

impl crate::source_load::Host for JsHost {
    fn progress(&self, completed: usize) {
        use napi::threadsafe_function::ThreadsafeFunctionCallMode;
        self.function.call(
            (
                0,
                "progress".into(),
                completed.to_string(),
                Buffer::from(Vec::new()),
            ),
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }
}

impl JsHost {
    fn new(function: HostFunction) -> Self {
        Self {
            function,
            trial: None,
        }
    }

    /// Tells the JavaScript host something without waiting for it.
    fn notify(&self, kind: &str, args: String) {
        use napi::threadsafe_function::ThreadsafeFunctionCallMode;
        self.function.call(
            (0, kind.into(), args, Buffer::from(Vec::new())),
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }

    /// Calls the JavaScript host and waits for its JSON answer.
    fn call(&self, kind: &str, args: String) -> std::result::Result<String, String> {
        use napi::threadsafe_function::ThreadsafeFunctionCallMode;
        let id = NEXT_HOST_CALL.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (sender, receiver) = std::sync::mpsc::channel();
        host_calls()
            .lock()
            .map_err(|_| "Could not register the host call".to_owned())?
            .insert(id, sender);
        let status = self.function.call(
            (id, kind.into(), args, Buffer::from(Vec::new())),
            ThreadsafeFunctionCallMode::Blocking,
        );
        if status != Status::Ok {
            if let Ok(mut calls) = host_calls().lock() {
                calls.remove(&id);
            }
            return Err("Could not call JavaScript".into());
        }
        receiver
            .recv()
            .map_err(|_| "JavaScript stopped".to_owned())?
    }
}

impl crate::embed::Charge for JsHost {
    /// Counts a model request against the limits of the trial, if the call runs in one.
    fn charge(&self, bytes: usize) -> std::result::Result<(), String> {
        match &self.trial {
            Some(trial) => trial.charge(bytes).map_err(|error| error.message),
            None => Ok(()),
        }
    }
}

impl crate::index_stage::IndexHost for JsHost {
    fn config_changed(&self) -> std::result::Result<bool, String> {
        let answer = self.call("config", String::new())?;
        serde_json::from_str(&answer).map_err(|_| "JavaScript returned an invalid answer".into())
    }

    fn index_tool(&self) -> std::result::Result<String, String> {
        let answer = self.call("tool", "hannoy".into())?;
        serde_json::from_str(&answer).map_err(|_| "JavaScript returned an invalid tool path".into())
    }
}

impl crate::adoption::AdoptHost for JsHost {
    fn write_config(&self) -> std::result::Result<(), String> {
        self.call("write_config", String::new()).map(|_| ())
    }

    fn restore_config(&self) -> std::result::Result<(), String> {
        self.call("restore_config", String::new()).map(|_| ())
    }
}

impl crate::wiki_serve::WikiServeHost for JsHost {
    fn listening(&self, port: u16) {
        self.notify("listening", format!("{{\"port\":{port}}}"));
    }
}

impl crate::search_stage::SearchHost for JsHost {
    fn index_tool(&self) -> std::result::Result<String, String> {
        crate::index_stage::IndexHost::index_tool(self)
    }
}

impl crate::serve::ServeHost for JsHost {
    fn search_models(
        &self,
        build: &crate::config_value::ConfigValue,
    ) -> std::result::Result<crate::config_value::ConfigValue, String> {
        let answer = self.call("search_models", build.to_js_json())?;
        serde_json::from_str(&answer).map_err(|_| "JavaScript returned invalid models".into())
    }

    fn listening(&self, address: &crate::config_value::ConfigValue) {
        self.notify("listening", address.to_js_json());
    }

    fn opened(&self, directory: &str) {
        self.notify(
            "opened",
            crate::config_value::ConfigValue::String(directory.into()).to_js_json(),
        );
    }
}

/// The search and read functions a caller passes to `Dunlin.serveMcp`.
impl crate::mcp::McpHost for JsHost {
    fn search(
        &self,
        arguments: &crate::config_value::ConfigValue,
        _cancel: &crate::Cancellation,
    ) -> std::result::Result<String, String> {
        self.call("mcp_search", arguments.to_js_json())
    }

    fn read(
        &self,
        evidence: &Evidence,
        _cancel: &crate::Cancellation,
    ) -> std::result::Result<String, String> {
        let evidence = serde_json::to_string(evidence).map_err(|error| error.to_string())?;
        self.call("mcp_read", evidence)
    }
}

impl crate::build_stage::TagHost for JsHost {
    fn search_config_id(
        &self,
        build: &crate::config_value::ConfigValue,
    ) -> std::result::Result<String, String> {
        let answer = self.call("search_config", build.to_js_json())?;
        serde_json::from_str(&answer)
            .map_err(|_| "JavaScript returned an invalid search configuration ID".into())
    }
}

/// `{ status: "failed", error: { code?, message, exitCode?, data? } }`
fn failed_stage(error: crate::embed::EmbedError) -> String {
    use crate::config_value::ConfigValue;
    let mut fields = Vec::new();
    if let Some(code) = error.code {
        fields.push(("code".into(), ConfigValue::String(code.into())));
    }
    fields.push(("message".into(), ConfigValue::String(error.message)));
    if let Some(exit) = error.exit_code {
        fields.push(("exitCode".into(), ConfigValue::Number(f64::from(exit))));
    }
    if let Some(data) = error.data {
        fields.push(("data".into(), data));
    }
    format!(
        "{{\"status\":\"failed\",\"error\":{}}}",
        ConfigValue::Object(fields).to_js_json()
    )
}

/// The `trial` of a request: `{ state, id, operation, limits }`, started before the run.
fn start_trial(
    request: &str,
    cancel: &crate::Cancellation,
) -> std::result::Result<Option<Arc<crate::trial::Trial>>, crate::embed::EmbedError> {
    #[derive(serde::Deserialize)]
    struct Field {
        #[serde(default)]
        trial: Option<crate::trial::TrialRequest>,
    }
    let field: Field = serde_json::from_str(request).map_err(|error| {
        crate::embed::EmbedError::plain(format!("Invalid trial request: {error}"))
    })?;
    field
        .trial
        .map(|trial| crate::trial::Trial::start(&trial, cancel))
        .transpose()
}

/// Ends a trial with the outcome of its run. A run stopped at a limit reports the limit.
fn finish_trial<T, E>(
    trial: Option<&crate::trial::Trial>,
    outcome: std::result::Result<T, E>,
    error: impl Fn(&E) -> &crate::embed::EmbedError,
    replace: impl FnOnce(E, crate::embed::EmbedError) -> E,
) -> std::result::Result<T, E> {
    let Some(trial) = trial else {
        return outcome;
    };
    match outcome {
        Ok(value) => match trial.finish(Ok(())) {
            None => Ok(value),
            // Recording the end failed after the run succeeded, as the JavaScript run did not.
            Some(_) => Ok(value),
        },
        Err(failure) => match trial.finish(Err(error(&failure))) {
            Some(limit) => Err(replace(failure, limit)),
            None => Err(failure),
        },
    }
}

/// Runs a build command (see `build_stage`): the selected stages with their inputs, or a dry run.
/// `host(id, kind, args, bytes)` answers `parse`, `progress`, `charge`, `config` and `tool`.
/// Resolves to `{ status: "done", output, warnings }` or a failed status as `runTransform` does.
#[napi]
pub fn run_build<'env>(
    env: &'env Env,
    request: String,
    host: Function<'env, HostArgs, Unknown<'static>>,
    cancel: &NativeCancellation,
) -> Result<Object<'env>> {
    use crate::config_value::ConfigValue;
    let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    let cancel = cancel.0.clone();
    std::thread::spawn(move || {
        let mut host = JsHost::new(function);
        let (trial, started) = match start_trial(&request, &cancel) {
            Ok(trial) => (trial, Ok(())),
            Err(error) => (None, Err(error)),
        };
        host.trial = trial.clone();
        let outcome = started
            .and_then(|()| {
                serde_json::from_str::<crate::build_stage::BuildRequest>(&request).map_err(
                    |error| {
                        crate::embed::EmbedError::plain(format!("Invalid build request: {error}"))
                    },
                )
            })
            .and_then(|request| crate::build_stage::run(request, &host, &cancel));
        let outcome = finish_trial(trial.as_deref(), outcome, |error| error, |_, limit| limit);
        let text = match outcome {
            Ok(outcome) => format!(
                "{{\"status\":\"done\",\"output\":{},\"warnings\":{}}}",
                outcome.output.to_js_json(),
                ConfigValue::Array(outcome.warnings).to_js_json()
            ),
            Err(error) if error.aborted => {
                deferred.reject(native_error(crate::Error::Aborted));
                return;
            }
            Err(error) => failed_stage(error),
        };
        deferred.resolve(move |_| Ok(text));
    });
    Ok(promise)
}

/// Adopts a build with `use` (see `adoption`). `host` answers `write_config` and
/// `restore_config`. Resolves to `{ status: "done", output }` or a failed status.
#[napi]
pub fn adopt_build<'env>(
    env: &'env Env,
    request: String,
    host: Function<'env, HostArgs, Unknown<'static>>,
) -> Result<Object<'env>> {
    let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    std::thread::spawn(move || {
        let host = JsHost::new(function);
        let outcome = serde_json::from_str::<crate::adoption::AdoptRequest>(&request)
            .map_err(|error| {
                crate::embed::EmbedError::plain(format!("Invalid adoption request: {error}"))
            })
            .and_then(|request| crate::adoption::adopt(&request, &host));
        let text = match outcome {
            Ok(output) => format!("{{\"status\":\"done\",\"output\":{}}}", output.to_js_json()),
            Err(error) => failed_stage(error),
        };
        deferred.resolve(move |_| Ok(text));
    });
    Ok(promise)
}

/// Answers a question from a search result (see `generate::answer`). `host` answers `charge`.
/// Resolves to `{ status: "done", output }` or `{ status: "failed", error: { ..., module? } }`.
#[napi]
pub fn answer_question<'env>(
    env: &'env Env,
    request: String,
    host: Function<'env, HostArgs, Unknown<'static>>,
    cancel: &NativeCancellation,
) -> Result<Object<'env>> {
    let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    let cancel = cancel.0.clone();
    std::thread::spawn(move || {
        let host = JsHost::new(function);
        let outcome = serde_json::from_str::<crate::generate::AnswerRequest>(&request)
            .map_err(|error| crate::generate::AnswerFailure {
                error: crate::embed::EmbedError::plain(format!("Invalid answer request: {error}")),
                module: None,
            })
            .and_then(|request| crate::generate::answer(&request, &host, &cancel));
        let text = match outcome {
            Ok(output) => format!("{{\"status\":\"done\",\"output\":{}}}", output.to_js_json()),
            Err(failure) if failure.error.aborted => {
                deferred.reject(native_error(crate::Error::Aborted));
                return;
            }
            Err(failure) => with_module(failed_stage(failure.error), failure.module),
        };
        deferred.resolve(move |_| Ok(text));
    });
    Ok(promise)
}

/// Adds `module` inside the error of a failed status.
fn with_module(mut text: String, module: Option<String>) -> String {
    if let Some(module) = module {
        // `{"status":"failed","error":{...}}`
        text.truncate(text.len() - 2);
        text.push_str(&format!(
            ",\"module\":{}}}}}",
            crate::config_value::ConfigValue::String(module).to_js_json()
        ));
    }
    text
}

/// Serves a project (see `serve`) until the client closes standard input or `cancel` aborts.
/// `host` answers `search_models`, `tool` and `charge`, and hears `listening` once the server is
/// ready. Resolves to `{ status: "done" }` or a failed status.
#[napi]
pub fn serve_project<'env>(
    env: &'env Env,
    request: String,
    host: Function<'env, HostArgs, Unknown<'static>>,
    cancel: &NativeCancellation,
) -> Result<Object<'env>> {
    let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    let cancel = cancel.0.clone();
    std::thread::spawn(move || {
        let host = Arc::new(JsHost::new(function));
        let outcome = serde_json::from_str::<crate::serve::ServeRequest>(&request)
            .map_err(|error| {
                crate::embed::EmbedError::plain(format!("Invalid serve request: {error}"))
            })
            .and_then(|request| {
                let stdio = request.settings.transport == "stdio";
                let search_only = request.project_type == "rag";
                let serving = Arc::new(crate::serve::Serving::open(request, host)?);
                if stdio {
                    crate::serve::serve_stdio(serving, search_only, &cancel)
                } else {
                    crate::serve::serve_http(serving, &cancel)
                }
                .map_err(crate::embed::EmbedError::plain)
            });
        let text = match outcome {
            Ok(()) => "{\"status\":\"done\"}".to_owned(),
            Err(error) => failed_stage(error),
        };
        deferred.resolve(move |_| Ok(text));
    });
    Ok(promise)
}

/// Names a build with a tag (see `build_stage::tag`). `host` answers `search_config` with the
/// search configuration ID of a build. Resolves to `{ status: "done", output }` or a failed status.
#[napi]
pub fn tag_build<'env>(
    env: &'env Env,
    request: String,
    host: Function<'env, HostArgs, Unknown<'static>>,
) -> Result<Object<'env>> {
    let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    std::thread::spawn(move || {
        let host = JsHost::new(function);
        let outcome = serde_json::from_str::<crate::build_stage::TagRequest>(&request)
            .map_err(|error| {
                crate::embed::EmbedError::plain(format!("Invalid tag request: {error}"))
            })
            .and_then(|request| crate::build_stage::tag(&request, &host));
        let text = match outcome {
            Ok(output) => format!("{{\"status\":\"done\",\"output\":{}}}", output.to_js_json()),
            Err(error) => failed_stage(error),
        };
        deferred.resolve(move |_| Ok(text));
    });
    Ok(promise)
}

/// The result of an `evidence-map` command as `{ status, output }` or a failed status, or `None`
/// when it was aborted.
fn evidence_map_outcome(
    database: Option<&KnowledgeBase>,
    request: &str,
    cancel: &crate::Cancellation,
) -> Option<String> {
    let outcome = serde_json::from_str::<crate::eval_stage::EvidenceMapRequest>(request)
        .map_err(|error| {
            crate::embed::EmbedError::plain(format!("Invalid evidence map request: {error}"))
        })
        .and_then(|request| crate::eval_stage::evidence_maps(database, &request, cancel));
    match outcome {
        Ok(output) => Some(format!(
            "{{\"status\":\"done\",\"output\":{}}}",
            output.to_js_json()
        )),
        Err(error) if error.aborted => None,
        Err(error) => Some(failed_stage(error)),
    }
}

/// Runs `evidence-map list` or `show` on `state.sqlite` (see `eval_stage`). Resolves to
/// `{ status: "done", output }` or a failed status.
#[napi]
pub fn evidence_maps(env: &Env, request: String) -> Result<Object<'_>> {
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    std::thread::spawn(move || {
        let cancel = crate::Cancellation::default();
        let text = evidence_map_outcome(None, &request, &cancel).unwrap_or_default();
        deferred.resolve(move |_| Ok(text));
    });
    Ok(promise)
}

/// Runs the `index` stage on a recorded transform stage (see `index_stage`): builds the keyword
/// and vector indexes in a new build directory, or reuses a saved build, and records the build.
/// `host(id, kind, args, bytes)` answers `charge`, `config` (whether the configuration file
/// changed) and `tool` (the verified hannoy path). Resolves to `{ status: "done", output,
/// warnings }` or a failed status as `runTransform` does.
#[napi]
pub fn run_index<'env>(
    env: &'env Env,
    request: String,
    host: Function<'env, HostArgs, Unknown<'static>>,
    cancel: &NativeCancellation,
) -> Result<Object<'env>> {
    use crate::config_value::ConfigValue;
    let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    let cancel = cancel.0.clone();
    std::thread::spawn(move || {
        let host = JsHost::new(function);
        let outcome = serde_json::from_str::<crate::index_stage::IndexRequest>(&request)
            .map_err(|error| {
                crate::embed::EmbedError::plain(format!("Invalid index request: {error}"))
            })
            .and_then(|request| crate::index_stage::run(&request, &host, &cancel));
        let text = match outcome {
            Ok(outcome) => format!(
                "{{\"status\":\"done\",\"output\":{},\"warnings\":{}}}",
                outcome.summary.to_js_json(),
                ConfigValue::Array(outcome.warnings).to_js_json()
            ),
            Err(error) if error.aborted => {
                deferred.reject(native_error(crate::Error::Aborted));
                return;
            }
            Err(error) => failed_stage(error),
        };
        deferred.resolve(move |_| Ok(text));
    });
    Ok(promise)
}

/// Runs the `transform` stage on a recorded source stage (see `transform_stage`): chunks, calls the
/// embedding model for `chunk.semantic` and records the stage in `state.sqlite`. `host(id, kind,
/// args, bytes)` answers `charge` calls, which count model requests against the trial limits.
/// Resolves to `{ status: "done", output, reused }` or `{ status: "failed", error: { code?,
/// message, exitCode? } }`.
#[napi]
pub fn run_transform<'env>(
    env: &'env Env,
    request: String,
    host: Function<'env, HostArgs, Unknown<'static>>,
    cancel: &NativeCancellation,
) -> Result<Object<'env>> {
    let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    let cancel = cancel.0.clone();
    std::thread::spawn(move || {
        let host = JsHost::new(function);
        let outcome = serde_json::from_str::<crate::transform_stage::TransformRequest>(&request)
            .map_err(|error| {
                crate::embed::EmbedError::plain(format!("Invalid transform request: {error}"))
            })
            .and_then(|request| crate::transform_stage::run(&request, &host, &cancel));
        let text = match outcome {
            Ok(outcome) => format!(
                "{{\"status\":\"done\",\"output\":{},\"reused\":{}}}",
                serde_json::to_string(&outcome.output).unwrap_or_default(),
                outcome.reused
            ),
            Err(error) if error.aborted => {
                deferred.reject(native_error(crate::Error::Aborted));
                return;
            }
            Err(error) => failed_stage(error),
        };
        deferred.resolve(move |_| Ok(text));
    });
    Ok(promise)
}

/// Runs a command of a Wiki or PARA project (see `wiki_commands`). Resolves to
/// `{ status: "done", output, warnings }` or a failed status.
#[napi]
pub fn wiki<'env>(
    env: &'env Env,
    request: String,
    cancel: &NativeCancellation,
) -> Result<Object<'env>> {
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    let cancel = cancel.0.clone();
    std::thread::spawn(move || {
        let outcome = serde_json::from_str::<crate::wiki_project::WikiRequest>(&request)
            .map_err(|error| {
                crate::embed::EmbedError::plain(format!("Invalid Wiki request: {error}"))
            })
            .and_then(|request| crate::wiki_commands::run(&request, &cancel));
        let text = match outcome {
            Ok(outcome) => format!(
                "{{\"status\":\"done\",\"output\":{},\"warnings\":{}}}",
                outcome.output.to_js_json(),
                crate::config_value::ConfigValue::Array(outcome.warnings).to_js_json()
            ),
            Err(error) if error.aborted => {
                deferred.reject(native_error(crate::Error::Aborted));
                return;
            }
            Err(error) => failed_stage(error),
        };
        deferred.resolve(move |_| Ok(text));
    });
    Ok(promise)
}

/// Serves the screen of a Wiki or PARA project (see `wiki_serve`) until `cancel` aborts, then
/// finishes the queued work. `host` parses uploaded HTML and Office files and hears `listening`
/// with `{ port }`. Resolves to `{ status: "done" }` or a failed status.
#[napi]
pub fn wiki_serve<'env>(
    env: &'env Env,
    request: String,
    host: Function<'env, HostArgs, Unknown<'static>>,
    cancel: &NativeCancellation,
) -> Result<Object<'env>> {
    let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    let cancel = cancel.0.clone();
    std::thread::spawn(move || {
        let host = Arc::new(JsHost::new(function));
        let outcome = serde_json::from_str::<crate::wiki_serve::WikiServeRequest>(&request)
            .map_err(|error| format!("Invalid Wiki serve request: {error}"))
            .and_then(|request| crate::wiki_serve::serve(request, host, &cancel));
        let text = match outcome {
            Ok(()) => "{\"status\":\"done\"}".to_owned(),
            Err(message) => failed_stage(crate::embed::EmbedError::plain(message)),
        };
        deferred.resolve(move |_| Ok(text));
    });
    Ok(promise)
}

/// Creates the tables of a Wiki or PARA project in `state.sqlite`.
#[napi]
pub fn ensure_wiki(state: String) -> Result<()> {
    crate::eval_stage::open_state(&state, false)
        .and_then(|connection| crate::wiki_project::ensure(&connection))
        .map_err(|error| Error::new(Status::GenericFailure, error.message))
}

/// Loads source files for the `source` stage (see `source_load`). `host(id, kind, args, bytes)`
/// parses the kinds Rust does not read and answers with `completeHostCall`. Resolves to
/// `{ documents, warnings }` as JSON.
#[napi]
pub fn load_corpus<'env>(
    env: &'env Env,
    request: String,
    host: Function<'env, HostArgs, Unknown<'static>>,
    cancel: &NativeCancellation,
) -> Result<Object<'env>> {
    let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    let cancel = cancel.0.clone();
    std::thread::spawn(move || {
        let host = JsHost::new(function);
        let outcome = serde_json::from_str::<crate::source_load::LoadRequest>(&request)
            .map_err(|_| crate::source_load::LoadError {
                message: "Invalid load request".into(),
                aborted: false,
            })
            .and_then(|request| crate::source_load::load(&request, &host, &cancel));
        match outcome {
            Ok(loaded) => {
                // Built by hand: serde_json::Value would sort the keys of each document.
                let text = format!(
                    "{{\"documents\":{},\"warnings\":{}}}",
                    serde_json::to_string(&loaded.documents).unwrap_or_default(),
                    serde_json::to_string(&loaded.warnings).unwrap_or_default()
                );
                deferred.resolve(move |_| Ok(text));
            }
            Err(error) if error.aborted => deferred.reject(native_error(crate::Error::Aborted)),
            Err(error) => deferred.reject(Error::new(
                Status::GenericFailure,
                format!("LOAD_FAILED|{}", error.message),
            )),
        }
    });
    Ok(promise)
}

/// Runs the `source` stage on scanned sources (see `source_stage`): routes, loads, computes the
/// stage ID and records the stage in `state.sqlite`. The host works as for `loadCorpus`. Resolves to
/// `{ status: "done", output, reused, warnings }` or `{ status: "unsupported", files, skipped }`.
#[napi]
pub fn run_source<'env>(
    env: &'env Env,
    request: String,
    host: Function<'env, HostArgs, Unknown<'static>>,
    cancel: &NativeCancellation,
) -> Result<Object<'env>> {
    use crate::source_stage::SourceOutcome;
    let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    let cancel = cancel.0.clone();
    std::thread::spawn(move || {
        let host = JsHost::new(function);
        let outcome = serde_json::from_str::<crate::source_stage::SourceRequest>(&request)
            .map_err(|error| crate::source_load::LoadError {
                message: format!("Invalid source request: {error}"),
                aborted: false,
            })
            .and_then(|request| crate::source_stage::run(&request, &host, &cancel));
        let json = |value: &crate::config_value::ConfigValue| {
            serde_json::to_string(value).unwrap_or_default()
        };
        // Built by hand: serde_json::Value would sort the keys of each document.
        let outcome = outcome.map(|outcome| match outcome {
            SourceOutcome::Done {
                output,
                reused,
                warnings,
            } => format!(
                "{{\"status\":\"done\",\"output\":{},\"reused\":{reused},\"warnings\":{}}}",
                json(&output),
                serde_json::to_string(&warnings).unwrap_or_default()
            ),
            SourceOutcome::Unsupported(stats) => {
                format!("{{\"status\":\"unsupported\",\"stats\":{}}}", json(&stats))
            }
        });
        match outcome {
            Ok(text) => deferred.resolve(move |_| Ok(text)),
            Err(error) if error.aborted => deferred.reject(native_error(crate::Error::Aborted)),
            Err(error) => deferred.reject(Error::new(
                Status::GenericFailure,
                format!("LOAD_FAILED|{}", error.message),
            )),
        }
    });
    Ok(promise)
}

/// Scans a source folder (see `source_scan`). Resolves to `{ id, files: [{ file, objectId }] }`
/// as JSON, ordered by object ID, or `{ error }` with a message.
#[napi]
pub fn scan_source(request: String, cancel: &NativeCancellation) -> AsyncTask<Job<String>> {
    let cancel = cancel.0.clone();
    task(move || {
        let request: crate::source_scan::ScanRequest = serde_json::from_str(&request)
            .map_err(|_| crate::Error::Invalid("Invalid scan request"))?;
        match crate::source_scan::scan(&request, &cancel) {
            Ok(scanned) => serde_json::to_string(&scanned).map_err(|_| crate::Error::Index),
            Err(message) => Ok(serde_json::json!({ "error": message }).to_string()),
        }
    })
}

/// The search configuration ID of a plan, its request overrides and models (see `search_stage`).
#[napi]
pub fn search_config_id(request: String) -> Result<String> {
    let settings: crate::search_stage::SearchSettings = serde_json::from_str(&request)
        .map_err(|_| native_error(crate::Error::Invalid("Invalid search settings")))?;
    Ok(crate::search_stage::search_config_id(&settings, true))
}

/// `{ status: "done", output }` or a failed status, for the evaluation reports.
fn report_outcome<T: serde::de::DeserializeOwned>(
    request: &str,
    run: impl FnOnce(
        &T,
    )
        -> std::result::Result<crate::config_value::ConfigValue, crate::embed::EmbedError>,
) -> String {
    let outcome = serde_json::from_str::<T>(request)
        .map_err(|error| {
            crate::embed::EmbedError::plain(format!("Invalid report request: {error}"))
        })
        .and_then(|request| run(&request));
    match outcome {
        Ok(output) => format!("{{\"status\":\"done\",\"output\":{}}}", output.to_js_json()),
        Err(error) => failed_stage(error),
    }
}

/// The result of an `eval-data` command: `{ status: "done", output, select? }` or a failed status,
/// or `None` when it was aborted.
fn eval_data_outcome(
    database: Option<&KnowledgeBase>,
    request: &str,
    cancel: &crate::Cancellation,
) -> Option<String> {
    let outcome = serde_json::from_str::<crate::eval_data::EvalDataRequest>(request)
        .map_err(|error| {
            crate::embed::EmbedError::plain(format!("Invalid eval-data request: {error}"))
        })
        .and_then(|request| crate::eval_data::eval_data(database, &request, cancel));
    match outcome {
        Ok(outcome) => Some(format!(
            "{{\"status\":\"done\",\"output\":{}{}}}",
            outcome.output.to_js_json(),
            outcome.select.map_or(String::new(), |id| format!(
                ",\"select\":{}",
                crate::config_value::ConfigValue::String(id).to_js_json()
            ))
        )),
        Err(error) if error.aborted => None,
        Err(error) => Some(failed_stage(error)),
    }
}

/// Runs an `eval-data` command on `state.sqlite` (see `eval_data`). A command that reads the
/// documents of the active build fails with the code `BUILD_NEEDED`; run it with
/// `Dunlin#evalData` on the open build instead.
#[napi]
pub fn eval_data(env: &Env, request: String) -> Result<Object<'_>> {
    let (deferred, promise) = env.create_deferred::<String, _>()?;
    std::thread::spawn(move || {
        let cancel = crate::Cancellation::default();
        let text = eval_data_outcome(None, &request, &cancel).unwrap_or_default();
        deferred.resolve(move |_| Ok(text));
    });
    Ok(promise)
}

/// The usage of all trial runs of a project: `{ runs, elapsed_ms, api_requests, api_input_bytes }`.
#[napi]
pub fn trial_usage(state: String) -> Result<String> {
    crate::trial::usage(&state)
        .map(|usage| usage.to_js_json())
        .map_err(|error| Error::new(Status::GenericFailure, error.message))
}

/// `report show`, `list` and `compare` on `state.sqlite` (see `eval_report`).
#[napi]
pub fn evaluation_reports(request: String) -> String {
    report_outcome(&request, crate::eval_report::reports)
}

/// The evaluation status of a build: `{ state, report }` (see `eval_report`).
#[napi]
pub fn evaluation_status(request: String) -> String {
    report_outcome(&request, crate::eval_report::evaluation_status)
}

/// The report and evaluation data that dictionary suggestions use: `{ report, data }`.
#[napi]
pub fn dictionary_evaluation(request: String) -> String {
    report_outcome(&request, crate::eval_report::dictionary_evaluation)
}

/// Routes files to parser modules. Returns `[{ route, kind }]` as JSON, in input order.
#[napi]
pub fn route_files(request: String) -> Result<String> {
    let request: crate::source_scan::RouteRequest = serde_json::from_str(&request)
        .map_err(|_| native_error(crate::Error::Invalid("Invalid routing request")))?;
    serde_json::to_string(&crate::source_scan::route(&request))
        .map_err(|_| native_error(crate::Error::Index))
}

/// `path.matchesGlob(path, pattern)` as Node.js computes it.
#[napi]
pub fn matches_glob(path: String, pattern: String) -> bool {
    crate::glob::matches(&path, &pattern)
}

/// JSON as YAML, as the `yaml` package's `stringify` writes it (spec 28.13 step 6b).
#[napi]
pub fn stringify_yaml(json: String) -> Result<String> {
    let value: crate::config_value::ConfigValue = serde_json::from_str(&json)
        .map_err(|_| native_error(crate::Error::Invalid("Invalid JSON")))?;
    Ok(crate::yaml_emit::stringify(&value))
}

/// Markdown as HTML, as markdown-it renders it with `{ html: false }` (spec 28.12 step 5a).
#[napi]
pub fn render_markdown(markdown: String) -> String {
    crate::markdown::render(&markdown)
}

/// Describes every registered module as JSON, in registration order.
#[napi]
pub fn describe_modules() -> Result<String> {
    serde_json::to_string(crate::registry::modules()).map_err(|_| native_error(crate::Error::Index))
}

/// Reads a module's arguments. Returns, for each defined argument in order, its value after
/// defaults and trimming, or the diagnostic code. `args` is a JSON object.
#[napi]
pub fn read_module_args(name: String, args: String) -> Result<String> {
    use crate::config_value::ConfigValue;
    use crate::registry::ArgResult;
    use serde::ser::{SerializeMap, SerializeSeq, Serializer};

    struct Results(Vec<(&'static crate::registry::Arg, ArgResult)>);
    impl Serialize for Results {
        fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
            let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
            for (arg, result) in &self.0 {
                struct Entry<'a>(&'a crate::registry::Arg, &'a ArgResult);
                impl Serialize for Entry<'_> {
                    fn serialize<S: Serializer>(
                        &self,
                        serializer: S,
                    ) -> std::result::Result<S::Ok, S::Error> {
                        let mut map = serializer.serialize_map(None)?;
                        map.serialize_entry("key", self.0.name)?;
                        match self.1 {
                            ArgResult::Value { value, defaulted } => {
                                if let Some(value) = value {
                                    map.serialize_entry("value", value)?;
                                }
                                map.serialize_entry("defaulted", defaulted)?;
                            }
                            ArgResult::Invalid(code) => map.serialize_entry("code", code)?,
                        }
                        map.serialize_entry(
                            "expected",
                            &crate::registry::ArgInfo {
                                arg: self.0,
                                speed: false,
                            },
                        )?;
                        map.end()
                    }
                }
                seq.serialize_element(&Entry(arg, result))?;
            }
            seq.end()
        }
    }

    let module = crate::registry::module(&name)
        .ok_or_else(|| native_error(crate::Error::Invalid("Unknown module")))?;
    let args: ConfigValue = serde_json::from_str(&args)
        .map_err(|_| native_error(crate::Error::Invalid("Invalid module arguments")))?;
    serde_json::to_string(&Results(crate::registry::read_args(module, &args)))
        .map_err(|_| native_error(crate::Error::Index))
}

/// Chunks documents synchronously. The request and result are JSON; see `chunk::ChunkRequest`.
#[napi]
pub fn chunk_documents(request: String) -> Result<String> {
    let request: crate::chunk::ChunkRequest = serde_json::from_str(&request)
        .map_err(|_| native_error(crate::Error::Invalid("Invalid chunk request")))?;
    let chunks =
        crate::chunk::chunk_documents(&request, &Cancellation::default()).map_err(native_error)?;
    serde_json::to_string(&chunks).map_err(|_| native_error(crate::Error::Index))
}

#[napi]
pub fn rank_vectors(
    path: String,
    request: String,
    cancel: &NativeCancellation,
) -> AsyncTask<Job<Json>> {
    let cancel = cancel.0.clone();
    task(move || {
        cancel.check()?;
        let search = serde_json::from_str(&request)
            .map_err(|_| crate::Error::Invalid("Invalid vector ranking options"))?;
        json(crate::vector::rank(Path::new(&path), search, &cancel)?)
    })
}

#[napi]
impl NativeDatabase {
    #[napi]
    pub fn add_source(
        &self,
        id: String,
        path: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || {
            json(database.add_source(&id, PathBuf::from(path), &cancel)?)
        }))
    }

    #[napi]
    pub fn replace_documents(
        &self,
        id: String,
        request: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || {
            let documents = parse_documents(request, &cancel)?;
            json(database.replace_document_inputs(&id, documents, &cancel)?)
        }))
    }

    #[napi]
    pub fn sync_source(
        &self,
        id: String,
        path: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || {
            json(database.sync_source(&id, PathBuf::from(path), &cancel)?)
        }))
    }

    #[napi]
    pub fn sync_documents(
        &self,
        id: String,
        request: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || {
            let documents = parse_documents(request, &cancel)?;
            json(database.sync_document_inputs(&id, documents, &Default::default(), &cancel)?)
        }))
    }

    #[napi]
    pub fn remove_source(
        &self,
        id: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<()>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || database.remove_source(&id, &cancel)))
    }

    #[napi]
    pub fn search(
        &self,
        query: String,
        limit: Option<u32>,
        budget_ms: Option<u32>,
        method: Option<String>,
        source_ids: Option<Vec<String>>,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let deadline = budget_ms.map(|ms| Instant::now() + Duration::from_millis(ms.into()));
        let cancel = cancel.0.clone().with_deadline(deadline);
        Ok(task(move || {
            let method = match method.as_deref() {
                None => None,
                Some("keyword") => Some(RetrievalMethod::Keyword),
                Some("coverage") => Some(RetrievalMethod::Coverage),
                _ => return Err(crate::Error::Invalid("Unknown retrieval method")),
            };
            json(database.search_with_sources(
                &query,
                limit.map(|value| value as usize),
                method,
                source_ids.as_deref().unwrap_or_default(),
                &cancel,
            )?)
        }))
    }

    #[napi]
    pub fn explore(
        &self,
        query: String,
        max_steps: u32,
        timeout_ms: u32,
        seed_limit: Option<u32>,
        source_ids: Option<Vec<String>>,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.into());
        let cancel = cancel.0.clone().with_deadline(Some(deadline));
        let options = ExploreOptions {
            max_steps: max_steps as usize,
            timeout_ms,
            seed_limit: seed_limit.map(|limit| limit as usize),
            source_ids: source_ids.unwrap_or_default(),
        };
        Ok(task(move || {
            json(database.explore(&query, options, &cancel)?)
        }))
    }

    #[napi]
    pub fn evaluate(
        &self,
        options: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || {
            json(database.evaluate(evaluation_options(&options)?, &cancel)?)
        }))
    }

    #[napi]
    pub fn optimize(
        &self,
        options: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || {
            json(database.optimize(evaluation_options(&options)?, &cancel)?)
        }))
    }

    #[napi]
    pub fn get_active_plan(&self, cancel: &NativeCancellation) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || json(database.get_active_plan(&cancel)?)))
    }

    #[napi]
    pub fn activate_plan(
        &self,
        id: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || json(database.activate_plan(&id, &cancel)?)))
    }

    #[napi]
    pub fn snapshot_documents(&self, cancel: &NativeCancellation) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || json(database.snapshot_documents(&cancel)?)))
    }

    #[napi]
    pub fn get_index_revision(
        &self,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<String>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || database.get_index_revision(&cancel)))
    }

    #[napi]
    pub fn get_corpus_document(
        &self,
        source_id: String,
        object_id: String,
        version: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || {
            let document = DocumentRef {
                source_id,
                object_id,
            };
            json(database.get_corpus_document(&document, &version, &cancel)?)
        }))
    }

    #[napi]
    pub fn get_document(
        &self,
        source_id: String,
        object_id: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || {
            json(database.get_document(
                &DocumentRef {
                    source_id,
                    object_id,
                },
                &cancel,
            )?)
        }))
    }

    #[napi]
    pub fn read(
        &self,
        evidence: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || {
            let evidence: Evidence = serde_json::from_str(&evidence)
                .map_err(|_| crate::Error::Invalid("Invalid evidence"))?;
            json(database.read(&evidence, &cancel)?)
        }))
    }

    #[napi]
    pub fn describe_evidence(
        &self,
        evidence: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || {
            let evidence: Vec<Evidence> = serde_json::from_str(&evidence)
                .map_err(|_| crate::Error::Invalid("Invalid evidence"))?;
            json(database.describe_evidence(&evidence, &cancel)?)
        }))
    }

    /// Runs a project search plan (see `search_stage`). `host(id, kind, args, bytes)` answers
    /// `charge` and `tool`. Resolves to `{ status: "done", output, context, contextStop }`
    /// or `{ status: "failed", error: { code?, message, exitCode?, module? } }`.
    #[napi]
    pub fn search_project<'env>(
        &self,
        env: &'env Env,
        request: String,
        host: Function<'env, HostArgs, Unknown<'static>>,
        cancel: &NativeCancellation,
    ) -> Result<Object<'env>> {
        use crate::config_value::ConfigValue;
        let database = self.database()?;
        let vectors = self.1.clone();
        let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
        let (deferred, promise) = env.create_deferred::<String, _>()?;
        let cancel = cancel.0.clone();
        std::thread::spawn(move || {
            let host = JsHost::new(function);
            let outcome = serde_json::from_str::<crate::search_stage::SearchRequest>(&request)
                .map_err(|error| crate::search_stage::SearchFailure {
                    error: crate::embed::EmbedError::plain(format!(
                        "Invalid search request: {error}"
                    )),
                    module: None,
                })
                .and_then(|request| {
                    crate::search_stage::run(&database, &vectors, &request, &host, &cancel)
                });
            let text = match outcome {
                Ok(outcome) => format!(
                    "{{\"status\":\"done\",\"output\":{},\"context\":{},\"contextStop\":\"{}\"}}",
                    outcome.output.to_js_json(),
                    ConfigValue::Array(outcome.context).to_js_json(),
                    outcome.context_stop
                ),
                Err(failure) if failure.error.aborted => {
                    deferred.reject(native_error(crate::Error::Aborted));
                    return;
                }
                Err(failure) => with_module(failed_stage(failure.error), failure.module),
            };
            deferred.resolve(move |_| Ok(text));
        });
        Ok(promise)
    }

    /// Scores the build of this index with the selected evaluation data and saves the report (see
    /// `eval_stage`). `host` answers `charge` and `tool` as for `searchProject`. Resolves to
    /// `{ status: "done", report }` or `{ status: "failed", error: { code?, message, exitCode?,
    /// module? } }`.
    #[napi]
    pub fn evaluate_project<'env>(
        &self,
        env: &'env Env,
        request: String,
        host: Function<'env, HostArgs, Unknown<'static>>,
        cancel: &NativeCancellation,
    ) -> Result<Object<'env>> {
        let database = self.database()?;
        let vectors = self.1.clone();
        let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
        let (deferred, promise) = env.create_deferred::<String, _>()?;
        let cancel = cancel.0.clone();
        std::thread::spawn(move || {
            let mut host = JsHost::new(function);
            let (trial, started) = match start_trial(&request, &cancel) {
                Ok(trial) => (trial, Ok(())),
                Err(error) => (
                    None,
                    Err(crate::search_stage::SearchFailure {
                        error,
                        module: None,
                    }),
                ),
            };
            host.trial = trial.clone();
            let outcome = started
                .and_then(|()| {
                    serde_json::from_str::<crate::eval_stage::EvaluateRequest>(&request).map_err(
                        |error| crate::search_stage::SearchFailure {
                            error: crate::embed::EmbedError::plain(format!(
                                "Invalid evaluation request: {error}"
                            )),
                            module: None,
                        },
                    )
                })
                .and_then(|request| {
                    crate::eval_stage::evaluate(&database, &vectors, request, &host, &cancel)
                });
            let outcome = finish_trial(
                trial.as_deref(),
                outcome,
                |failure| &failure.error,
                |_, limit| crate::search_stage::SearchFailure {
                    error: limit,
                    module: None,
                },
            );
            let text = match outcome {
                Ok(report) => format!("{{\"status\":\"done\",\"report\":{}}}", report.to_js_json()),
                Err(failure) if failure.error.aborted => {
                    deferred.reject(native_error(crate::Error::Aborted));
                    return;
                }
                Err(failure) => with_module(failed_stage(failure.error), failure.module),
            };
            deferred.resolve(move |_| Ok(text));
        });
        Ok(promise)
    }

    /// The evidence a development report missed, read from the documents of this index (see
    /// `eval_report`). Resolves to `{ status: "done", output }` or a failed status.
    #[napi]
    pub fn dictionary_misses<'env>(
        &self,
        env: &'env Env,
        request: String,
        cancel: &NativeCancellation,
    ) -> Result<Object<'env>> {
        let database = self.database()?;
        let (deferred, promise) = env.create_deferred::<String, _>()?;
        let cancel = cancel.0.clone();
        std::thread::spawn(move || {
            let outcome = serde_json::from_str::<crate::eval_report::MissesRequest>(&request)
                .map_err(|error| {
                    crate::embed::EmbedError::plain(format!("Invalid misses request: {error}"))
                })
                .and_then(|request| {
                    crate::eval_report::dictionary_misses(&database, &request, &cancel)
                });
            let text = match outcome {
                Ok(output) => format!("{{\"status\":\"done\",\"output\":{}}}", output.to_js_json()),
                Err(error) if error.aborted => {
                    deferred.reject(native_error(crate::Error::Aborted));
                    return;
                }
                Err(error) => failed_stage(error),
            };
            deferred.resolve(move |_| Ok(text));
        });
        Ok(promise)
    }

    /// Runs an `eval-data` command with the documents of this index (see `eval_data`).
    #[napi]
    pub fn eval_data<'env>(
        &self,
        env: &'env Env,
        request: String,
        cancel: &NativeCancellation,
    ) -> Result<Object<'env>> {
        let database = self.database()?;
        let (deferred, promise) = env.create_deferred::<String, _>()?;
        let cancel = cancel.0.clone();
        std::thread::spawn(
            move || match eval_data_outcome(Some(&database), &request, &cancel) {
                Some(text) => deferred.resolve(move |_| Ok(text)),
                None => deferred.reject(native_error(crate::Error::Aborted)),
            },
        );
        Ok(promise)
    }

    /// Runs `evidence-map create`, `import` or `confirm` on the documents of this index (see
    /// `eval_stage`). Resolves to `{ status: "done", output }` or a failed status.
    #[napi]
    pub fn evidence_map<'env>(
        &self,
        env: &'env Env,
        request: String,
        cancel: &NativeCancellation,
    ) -> Result<Object<'env>> {
        let database = self.database()?;
        let (deferred, promise) = env.create_deferred::<String, _>()?;
        let cancel = cancel.0.clone();
        std::thread::spawn(move || {
            let text = evidence_map_outcome(Some(&database), &request, &cancel);
            match text {
                Some(text) => deferred.resolve(move |_| Ok(text)),
                None => deferred.reject(native_error(crate::Error::Aborted)),
            }
        });
        Ok(promise)
    }

    /// Serves MCP on standard input and output (see `mcp`) until the client closes it or `cancel`
    /// aborts. With `project`, `host` answers `mcp_search` and `mcp_read`; `searchOnly` publishes
    /// only `knowledge.search`.
    #[napi]
    pub fn serve_mcp<'env>(
        &self,
        env: &'env Env,
        request: String,
        host: Function<'env, HostArgs, Unknown<'static>>,
        cancel: &NativeCancellation,
    ) -> Result<Object<'env>> {
        let options: Value = serde_json::from_str(&request)
            .map_err(|_| native_error(crate::Error::Invalid("Invalid MCP options")))?;
        let database = self.database()?;
        let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
        let (deferred, promise) = env.create_deferred::<(), _>()?;
        let cancel = cancel.0.clone();
        std::thread::spawn(move || {
            let host: Option<Arc<dyn crate::mcp::McpHost>> = options["project"]
                .as_bool()
                .unwrap_or(false)
                .then(|| Arc::new(JsHost::new(function)) as Arc<dyn crate::mcp::McpHost>);
            let server = crate::mcp::McpServer::new(
                database,
                host,
                options["searchOnly"].as_bool().unwrap_or(false),
            );
            match crate::mcp::serve_stdio(server, &cancel) {
                Ok(()) => deferred.resolve(|_| Ok(())),
                Err(message) => deferred.reject(Error::new(
                    Status::GenericFailure,
                    format!("IO_ERROR|{message}"),
                )),
            }
        });
        Ok(promise)
    }

    #[napi]
    pub fn close(&mut self) -> AsyncTask<Job<()>> {
        let database = self.0.take();
        task(move || {
            drop(database);
            Ok(())
        })
    }
}

impl NativeDatabase {
    fn database(&self) -> Result<Arc<KnowledgeBase>> {
        self.0
            .clone()
            .ok_or_else(|| Error::new(Status::GenericFailure, "CLOSED|Dunlin is closed"))
    }
}

// napi provides the JSON conversion but Value does not implement Task's TypeName.
pub struct Json(Value);

impl TypeName for Json {
    fn type_name() -> &'static str {
        "object"
    }

    fn value_type() -> ValueType {
        ValueType::Object
    }
}

impl ToNapiValue for Json {
    unsafe fn to_napi_value(env: napi_env, value: Self) -> Result<napi_value> {
        // The task resolves on the JS thread with the environment supplied by napi.
        unsafe { Value::to_napi_value(env, value.0) }
    }
}

pub struct Job<T>(Option<Box<dyn FnOnce() -> crate::Result<T> + Send>>);

impl<T: Send + ToNapiValue + TypeName + 'static> Task for Job<T> {
    type Output = T;
    type JsValue = T;

    fn compute(&mut self) -> Result<T> {
        let run = self
            .0
            .take()
            .ok_or_else(|| native_error(crate::Error::Index))?;
        run().map_err(native_error)
    }

    fn resolve(&mut self, _: Env, output: T) -> Result<T> {
        Ok(output)
    }
}

fn task<T: Send + ToNapiValue + TypeName + 'static>(
    run: impl FnOnce() -> crate::Result<T> + Send + 'static,
) -> AsyncTask<Job<T>> {
    AsyncTask::new(Job(Some(Box::new(run))))
}

fn json(value: impl Serialize) -> crate::Result<Json> {
    serde_json::to_value(value)
        .map(Json)
        .map_err(|_| crate::Error::Index)
}

fn parse_documents(request: String, cancel: &Cancellation) -> crate::Result<Vec<DocumentInput>> {
    cancel.check()?;
    serde_json::from_str(&request).map_err(|_| crate::Error::Invalid("Invalid source documents"))
}

fn evaluation_options<T: serde::de::DeserializeOwned>(options: &str) -> crate::Result<T> {
    serde_json::from_str(options).map_err(|_| crate::Error::Invalid("Invalid evaluation options"))
}

fn native_error(error: crate::Error) -> Error {
    Error::new(Status::GenericFailure, format!("{}|{error}", error.code()))
}

/// Where the CLI runs, from the launcher (spec 28.13 step 6c).
#[napi(object)]
pub struct CliOptions {
    /// `process.execPath` and the launcher script, for `serve --print-config`.
    pub exec_path: String,
    pub cli: String,
    /// The package root, which holds `dist/tools` and `tools/rag/parsers`.
    pub package: String,
    /// The `tools` folder of the platform package, when it is installed.
    pub native_tools: Option<String>,
    /// `os.homedir()`.
    pub home: String,
}

struct CliHost(HostFunction);

impl crate::cli_run::CliEvents for CliHost {
    fn serving(&self, active: bool) {
        use napi::threadsafe_function::ThreadsafeFunctionCallMode;
        self.0.call(
            (
                0,
                "serving".into(),
                active.to_string(),
                Buffer::from(Vec::new()),
            ),
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }

    fn opened(&self, path: &str) {
        use napi::threadsafe_function::ThreadsafeFunctionCallMode;
        self.0.call(
            (
                0,
                "opened".into(),
                crate::config_value::ConfigValue::String(path.into()).to_js_json(),
                Buffer::from(Vec::new()),
            ),
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }
}

/// Runs the `dunlin` CLI with `process.argv.slice(2)` (see `cli_run`). It prints the result and
/// resolves to the exit code. `host(id, kind, args)` hears `serving` (`true` while a server runs,
/// when SIGINT and SIGTERM should abort `cancel`) and `opened` (an index a server opened).
#[napi]
pub fn run_cli<'env>(
    env: &'env Env,
    args: Vec<String>,
    options: CliOptions,
    host: Function<'env, HostArgs, Unknown<'static>>,
    cancel: &NativeCancellation,
) -> Result<Object<'env>> {
    let function: HostFunction = host.build_threadsafe_function::<HostArgs>().build()?;
    let (deferred, promise) = env.create_deferred::<u32, _>()?;
    let cancel = cancel.0.clone();
    std::thread::spawn(move || {
        let cwd = std::env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let environment = crate::cli_tools::CliEnv {
            cwd,
            home: options.home,
            package: options.package,
            native_tools: options.native_tools,
            exec_path: options.exec_path,
            cli: options.cli,
        };
        let events: Arc<dyn crate::cli_run::CliEvents> = Arc::new(CliHost(function));
        let code = crate::cli_run::run(args, &environment, &cancel, events);
        deferred.resolve(move |_| Ok(u32::from(code)));
    });
    Ok(promise)
}

/// The usage line of every CLI command, in order, as `[name, usage]`.
#[napi]
pub fn cli_commands() -> Vec<Vec<String>> {
    crate::cli_args::COMMANDS
        .iter()
        .map(|(name, usage)| vec![(*name).to_owned(), (*usage).to_owned()])
        .collect()
}
