use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::engine::{KeywordSnapshot, query_groups};
use crate::{Cancellation, Error, Result, RetrievalMethod, SearchHit, source};

const MAX_CASES: usize = 1_000;
const MAX_SET_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationOptions {
    pub evaluation_set: EvaluationSet,
    pub limit: usize,
    pub repetitions: usize,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum EvaluationSet {
    Jsonl(PathBuf),
    Cases(Vec<EvaluationCase>),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationCase {
    pub query: String,
    pub relevant: Vec<RelevantObject>,
}

pub type RelevantObject = crate::DocumentRef;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizeOptions {
    #[serde(flatten)]
    pub evaluation: EvaluationOptions,
    pub p95_latency_budget_ms: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateMetrics {
    pub plan_id: String,
    pub method: RetrievalMethod,
    pub recall_at_k: f64,
    pub ndcg_at_k: f64,
    pub p95_latency_ms: f64,
    pub latency_samples: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationReport {
    pub index_revision: String,
    pub evaluation_set_hash: String,
    pub query_count: usize,
    pub limit: usize,
    pub repetitions: usize,
    pub candidates: Vec<CandidateMetrics>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationReport {
    #[serde(flatten)]
    pub evaluation: EvaluationReport,
    pub status: &'static str,
    pub p95_latency_budget_ms: f64,
    pub selected_plan_id: Option<String>,
}

pub(crate) fn evaluate(
    snapshot: &KeywordSnapshot<'_>,
    options: EvaluationOptions,
    cancel: &Cancellation,
) -> Result<EvaluationReport> {
    let EvaluationOptions {
        evaluation_set,
        limit,
        repetitions,
    } = options;
    if !(1..=100).contains(&limit) || !(1..=20).contains(&repetitions) {
        return Err(Error::Invalid(
            "Evaluation limit must be 1–100 and repetitions 1–20",
        ));
    }
    let cases = load_cases(evaluation_set, cancel)?;
    let encoded = serde_json::to_vec(&cases)
        .map_err(|_| Error::Invalid("Could not encode the evaluation set"))?;
    let evaluation_set_hash = blake3::hash(&encoded).to_hex().to_string();
    let index_revision = snapshot.revision();
    let methods = RetrievalMethod::ALL;
    let mut measurements = methods.map(|_| Measurements::default());

    for (case_index, case) in cases.iter().enumerate() {
        // Each candidate gets a warm-up on the same query. Alternate the order in timed runs.
        for method in methods {
            cancel.check()?;
            snapshot.search(method, &case.query, limit, cancel)?;
        }
        for round in 0..repetitions {
            for position in 0..methods.len() {
                let index = (position + round + case_index) % methods.len();
                cancel.check()?;
                let start = Instant::now();
                let (hits, _) = snapshot.search(methods[index], &case.query, limit, cancel)?;
                measurements[index]
                    .latency
                    .push(start.elapsed().as_secs_f64() * 1000.0);
                if round == 0 {
                    let (recall, ndcg) = quality(&hits, &case.relevant, limit);
                    measurements[index].recall += recall;
                    measurements[index].ndcg += ndcg;
                }
            }
        }
    }
    cancel.check()?;
    let candidates = methods
        .into_iter()
        .zip(measurements)
        .map(|(method, metrics)| metrics.summarize(method, cases.len()))
        .collect();
    Ok(EvaluationReport {
        index_revision,
        evaluation_set_hash,
        query_count: cases.len(),
        limit,
        repetitions,
        candidates,
    })
}

fn load_cases(input: EvaluationSet, cancel: &Cancellation) -> Result<Vec<EvaluationCase>> {
    let cases = match input {
        EvaluationSet::Cases(cases) => cases,
        EvaluationSet::Jsonl(path) => {
            let file = File::open(path).map_err(|_| Error::Io("Could not open evaluation set"))?;
            let mut data = String::new();
            file.take(MAX_SET_BYTES + 1)
                .read_to_string(&mut data)
                .map_err(|_| Error::Io("Evaluation set must be readable UTF-8"))?;
            if data.len() as u64 > MAX_SET_BYTES {
                return Err(Error::Invalid("Evaluation set exceeds 4 MiB"));
            }
            let mut cases = Vec::new();
            for line in data
                .trim_start_matches('\u{feff}')
                .lines()
                .filter(|line| !line.trim().is_empty())
            {
                cancel.check()?;
                if cases.len() == MAX_CASES {
                    return Err(Error::Invalid("Evaluation set has more than 1000 cases"));
                }
                cases.push(
                    serde_json::from_str(line)
                        .map_err(|_| Error::Invalid("Invalid evaluation JSONL row"))?,
                );
            }
            cases
        }
    };
    if cases.is_empty() || cases.len() > MAX_CASES {
        return Err(Error::Invalid("Evaluation set must contain 1–1000 cases"));
    }
    for case in &cases {
        cancel.check()?;
        query_groups(&case.query)?;
        if !(1..=100).contains(&case.relevant.len()) {
            return Err(Error::Invalid(
                "Each query must have 1–100 relevant objects",
            ));
        }
        let mut seen = HashSet::new();
        for object in &case.relevant {
            source::validate_id(&object.source_id)?;
            if object.object_id.is_empty() || !seen.insert(object) {
                return Err(Error::Invalid(
                    "Relevant objects must have nonempty IDs and be unique per query",
                ));
            }
        }
    }
    Ok(cases)
}

fn quality(hits: &[SearchHit], relevant: &[RelevantObject], limit: usize) -> (f64, f64) {
    let expected = relevant
        .iter()
        .map(|object| (&*object.source_id, &*object.object_id))
        .collect::<HashSet<_>>();
    let mut found = HashSet::new();
    let mut dcg = 0.0;
    for (rank, hit) in hits.iter().enumerate() {
        let source = &hit.evidence.source;
        let key = (&*source.source_id, &*source.object_id);
        if expected.contains(&key) && found.insert(key) {
            dcg += discount(rank);
        }
    }
    let ideal: f64 = (0..limit.min(relevant.len())).map(discount).sum();
    (found.len() as f64 / relevant.len() as f64, dcg / ideal)
}

fn discount(rank: usize) -> f64 {
    1.0 / ((rank + 2) as f64).log2()
}

#[derive(Default)]
struct Measurements {
    recall: f64,
    ndcg: f64,
    latency: Vec<f64>,
}

impl Measurements {
    fn summarize(mut self, method: RetrievalMethod, query_count: usize) -> CandidateMetrics {
        self.latency.sort_by(f64::total_cmp);
        let latency_samples = self.latency.len();
        let p95_rank = (latency_samples * 95).div_ceil(100);
        CandidateMetrics {
            plan_id: method.plan_id().into(),
            method,
            recall_at_k: self.recall / query_count as f64,
            ndcg_at_k: self.ndcg / query_count as f64,
            p95_latency_ms: self.latency[p95_rank - 1],
            latency_samples,
        }
    }
}
