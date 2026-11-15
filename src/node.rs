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

#[napi]
pub struct NativeDatabase(Option<Arc<KnowledgeBase>>);

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
        Ok(NativeDatabase(Some(Arc::new(
            KnowledgeBase::open_with_options(
                Path::new(&path),
                backend,
                corpus_path.as_deref().map(Path::new),
                keyword,
                &cancel,
            )?,
        ))))
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
}

impl crate::source_load::Host for JsHost {
    fn parse(
        &self,
        file: &crate::source_load::LoadFile,
        bytes: &[u8],
    ) -> std::result::Result<crate::config_value::ConfigValue, String> {
        use napi::threadsafe_function::ThreadsafeFunctionCallMode;
        let id = NEXT_HOST_CALL.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (sender, receiver) = std::sync::mpsc::channel();
        host_calls()
            .lock()
            .map_err(|_| "Could not register the parser call".to_owned())?
            .insert(id, sender);
        let name = std::path::Path::new(&file.file)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let args = serde_json::json!({
            "kind": file.kind,
            "name": name,
            "file": file.file,
            "id": file.id,
            "objectId": file.object_id,
            "route": file.route,
        })
        .to_string();
        let status = self.function.call(
            (id, "parse".into(), args, Buffer::from(bytes.to_vec())),
            ThreadsafeFunctionCallMode::Blocking,
        );
        if status != Status::Ok {
            if let Ok(mut calls) = host_calls().lock() {
                calls.remove(&id);
            }
            return Err("Could not call the JavaScript parser".into());
        }
        let text = receiver
            .recv()
            .map_err(|_| "The JavaScript parser stopped".to_owned())??;
        serde_json::from_str(&text)
            .map_err(|_| "The JavaScript parser returned invalid JSON".to_owned())
    }

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
        let host = JsHost { function };
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

/// Scans a source folder (see `source_scan`). Resolves to `{ id, files: [{ file, objectId }] }`
/// as JSON, unsorted, or `{ error }` with a message.
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
            json(database.sync_document_inputs(&id, documents, &cancel)?)
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

    #[napi]
    pub fn wiki_build(
        &self,
        options: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || {
            let options = serde_json::from_str(&options)
                .map_err(|_| crate::Error::Invalid("Invalid Wiki build options"))?;
            json(database.wiki_build(options, &cancel)?)
        }))
    }

    #[napi]
    pub fn wiki_get(
        &self,
        id: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || json(database.wiki_get(&id, &cancel)?)))
    }

    #[napi]
    pub fn wiki_affected(
        &self,
        source_id: Option<String>,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || {
            json(database.wiki_affected(source_id.as_deref(), &cancel)?)
        }))
    }

    #[napi]
    pub fn wiki_regenerate(
        &self,
        id: String,
        cancel: &NativeCancellation,
    ) -> Result<AsyncTask<Job<Json>>> {
        let database = self.database()?;
        let cancel = cancel.0.clone();
        Ok(task(move || json(database.wiki_regenerate(&id, &cancel)?)))
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
