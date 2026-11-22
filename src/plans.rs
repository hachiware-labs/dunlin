use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use tantivy::directory::error::LockError;
use tantivy::directory::{Directory, Lock};

use crate::storage::{read_json, write_json};
use crate::{Cancellation, Error, Result};

const PLAN_VERSION: u32 = 1;
const ACTIVE: &str = "active.json";
pub(crate) const DEFAULT_LIMIT: usize = 10;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetrievalMethod {
    Keyword,
    Coverage,
}

impl RetrievalMethod {
    pub(crate) const ALL: [Self; 2] = [Self::Keyword, Self::Coverage];

    pub(crate) fn plan_id(self) -> &'static str {
        match self {
            Self::Keyword => "keyword-v1",
            Self::Coverage => "keyword-coverage-v1",
        }
    }

    pub(crate) fn engine(self) -> &'static str {
        match self {
            Self::Keyword => "tantivy-bm25",
            Self::Coverage => "tantivy-bm25-coverage",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievalPlan {
    pub id: String,
    pub version: u32,
    pub method: RetrievalMethod,
    pub default_limit: usize,
    pub evaluation: Option<PlanEvaluation>,
}

impl RetrievalPlan {
    pub(crate) fn builtin(method: RetrievalMethod) -> Self {
        Self {
            id: method.plan_id().into(),
            version: PLAN_VERSION,
            method,
            default_limit: DEFAULT_LIMIT,
            evaluation: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEvaluation {
    pub p95_latency_budget_ms: f64,
    /// The `evaluate` report the plan was chosen with. Plans saved before evidence-level scoring
    /// (spec 28.11 step 4d) keep their document-level report.
    pub report: serde_json::Value,
}

impl PlanEvaluation {
    fn limit(&self) -> Option<usize> {
        self.report["limit"].as_u64().map(|limit| limit as usize)
    }
}

pub(crate) struct PlanStore {
    directory: crate::storage::RetryDirectory,
    active: RwLock<RetrievalPlan>,
}

impl PlanStore {
    pub fn open(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path)
            .map_err(|_| Error::Io("Could not create the plan directory"))?;
        let directory = crate::storage::RetryDirectory::open(path).map_err(|_| Error::Plan)?;
        let active =
            match read_json::<String>(&directory, Path::new(ACTIVE)).map_err(|_| Error::Plan)? {
                Some(id) => load_plan(&directory, &id).map_err(|_| Error::Plan)?,
                None => RetrievalPlan::builtin(RetrievalMethod::Keyword),
            };
        Ok(Self {
            directory,
            active: RwLock::new(active),
        })
    }

    pub fn active_route(&self) -> Result<(String, RetrievalMethod, usize)> {
        let active = self.active.read().map_err(|_| Error::Plan)?;
        Ok((active.id.clone(), active.method, active.default_limit))
    }

    pub fn active(&self) -> Result<RetrievalPlan> {
        Ok(self.active.read().map_err(|_| Error::Plan)?.clone())
    }

    pub fn activate(&self, id: &str, cancel: &Cancellation) -> Result<RetrievalPlan> {
        cancel.check()?;
        let plan = load_plan(&self.directory, id)?;
        let _file_lock = self
            .directory
            .acquire_lock(&Lock {
                filepath: ".knowledge-nest-plan.lock".into(),
                is_blocking: false,
            })
            .map_err(|error| match error {
                LockError::LockBusy => Error::Busy,
                _ => Error::Plan,
            })?;
        cancel.check()?;
        write_json(&self.directory, Path::new(ACTIVE), id).map_err(|_| Error::Plan)?;
        *self.active.write().map_err(|_| Error::Plan)? = plan.clone();
        Ok(plan)
    }

    pub fn save(
        &self,
        method: RetrievalMethod,
        evaluation: PlanEvaluation,
        cancel: &Cancellation,
    ) -> Result<String> {
        let encoded =
            serde_json::to_vec(&(PLAN_VERSION, method, &evaluation)).map_err(|_| Error::Plan)?;
        let id = format!("evaluated-{}", blake3::hash(&encoded).to_hex());
        let plan = RetrievalPlan {
            id: id.clone(),
            version: PLAN_VERSION,
            method,
            default_limit: evaluation.limit().ok_or(Error::Plan)?,
            evaluation: Some(evaluation),
        };
        cancel.check()?;
        write_json(&self.directory, &plan_path(&id)?, &plan).map_err(|_| Error::Plan)?;
        Ok(id)
    }
}

fn load_plan(directory: &crate::storage::RetryDirectory, id: &str) -> Result<RetrievalPlan> {
    if let Some(method) = RetrievalMethod::ALL
        .into_iter()
        .find(|method| method.plan_id() == id)
    {
        return Ok(RetrievalPlan::builtin(method));
    }
    let plan: RetrievalPlan = read_json(directory, &plan_path(id)?)
        .map_err(|_| Error::Plan)?
        .ok_or(Error::PlanNotFound)?;
    if plan.id != id
        || plan.version != PLAN_VERSION
        || !plan.evaluation.as_ref().is_some_and(|evaluation| {
            evaluation.limit() == Some(plan.default_limit)
                && (1..=100).contains(&plan.default_limit)
        })
    {
        return Err(Error::Plan);
    }
    Ok(plan)
}

fn plan_path(id: &str) -> Result<PathBuf> {
    let Some(hash) = id.strip_prefix("evaluated-") else {
        return Err(Error::PlanNotFound);
    };
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::PlanNotFound);
    }
    Ok(format!("{id}.json").into())
}
