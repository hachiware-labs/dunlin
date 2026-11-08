use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use napi::bindgen_prelude::{
    AsyncTask, Env, Error, Result, Status, Task, ToNapiValue, TypeName, ValueType,
};
use napi::sys::{napi_env, napi_value};
use napi_derive::napi;
use serde::Serialize;
use serde_json::Value;

use crate::{
    Cancellation, DocumentRef, Evidence, ExploreOptions, KnowledgeBase, RetrievalMethod,
    source::DocumentInput,
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
pub fn open_native(path: String, cancel: &NativeCancellation) -> AsyncTask<Job<NativeDatabase>> {
    let cancel = cancel.0.clone();
    task(move || {
        Ok(NativeDatabase(Some(Arc::new(KnowledgeBase::open(
            Path::new(&path),
            &cancel,
        )?))))
    })
}

#[napi]
pub fn inspect_index(path: String, cancel: &NativeCancellation) -> AsyncTask<Job<Json>> {
    let cancel = cancel.0.clone();
    task(move || json(KnowledgeBase::inspect_index(Path::new(&path), &cancel)?))
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
