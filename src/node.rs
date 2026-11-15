use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use napi::bindgen_prelude::{
    AsyncTask, Buffer, Env, Error, Result, Status, Task, ToNapiValue, TypeName, ValueType,
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
