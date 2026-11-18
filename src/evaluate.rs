//! `evaluate` and `optimize` of the Node.js API (spec 28.11 step 4d): compare the built-in search
//! plans on questions with evidence quotes, as project evaluation does (spec 9 and 10).
//!
//! Each quote is found in the current document with the rules of `eval`, and a plan recovers it
//! when its top `limit` results cover the quote: all of a short quote, or `minCoverage` of its
//! characters. Quotes that cannot be found count as missed.

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::config_value::ConfigValue;
use crate::engine::{KeywordSnapshot, query_groups};
use crate::evidence_match::{covers, line_bytes, position};
use crate::{
    Cancellation, DocumentRef, Error, Result, RetrievalMethod, SearchHit, SourceLocation, source,
};

const MAX_CASES: usize = 1_000;
const MAX_SET_BYTES: u64 = 4 * 1024 * 1024;
const SHORT_EVIDENCE_CHARS: usize = 40;
const MIN_COVERAGE: f64 = 0.8;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationOptions {
    pub evaluation_set: EvaluationSet,
    pub limit: usize,
    pub repetitions: usize,
    /// A quote of at most this many characters must be covered completely.
    #[serde(default)]
    pub short_evidence_chars: Option<usize>,
    /// The share of a longer quote's characters that results must cover.
    #[serde(default)]
    pub min_coverage: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum EvaluationSet {
    Jsonl(PathBuf),
    Cases(Vec<EvaluationCase>),
}

/// A question and the quotes that answer it. A case may also give one quote directly, as
/// `eval-data import` accepts: `{ question, doc, quote, ... }`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "RawCase")]
pub struct EvaluationCase {
    pub question: String,
    pub evidence: Vec<EvidenceQuote>,
}

/// A quote in a document, `sourceId:objectId`, with the hints `eval-data` takes.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceQuote {
    pub doc: String,
    pub quote: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawCase {
    question: String,
    #[serde(default)]
    evidence: Option<Vec<EvidenceQuote>>,
    #[serde(default)]
    doc: Option<String>,
    #[serde(default)]
    quote: Option<String>,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    suffix: Option<String>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    heading: Option<String>,
}

impl TryFrom<RawCase> for EvaluationCase {
    type Error = &'static str;

    fn try_from(raw: RawCase) -> std::result::Result<Self, Self::Error> {
        let evidence = match (raw.evidence, raw.doc, raw.quote) {
            (Some(evidence), None, None) => evidence,
            (None, Some(doc), Some(quote)) => vec![EvidenceQuote {
                doc,
                quote,
                prefix: raw.prefix,
                suffix: raw.suffix,
                page: raw.page,
                heading: raw.heading,
            }],
            _ => return Err("Give either evidence or one doc and quote"),
        };
        Ok(Self {
            question: raw.question,
            evidence,
        })
    }
}

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
    /// The share of evidence quotes the top `limit` results cover, averaged over questions.
    pub evidence_recall_at_k: f64,
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
    /// Quotes found once in their documents, and the others, which count as missed.
    pub resolved_evidence: usize,
    pub unresolved_evidence: usize,
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

/// A quote found in its document: the byte range and what results must match.
struct Target {
    source_id: String,
    object_id: String,
    version: String,
    content: String,
    range: (f64, f64),
}

/// The corpus locations of a document version, for page hints.
pub(crate) type Locations<'a> = dyn Fn(&DocumentRef, &str) -> Result<Vec<SourceLocation>> + 'a;

pub(crate) fn evaluate(
    snapshot: &KeywordSnapshot<'_>,
    locations: &Locations<'_>,
    options: EvaluationOptions,
    cancel: &Cancellation,
) -> Result<EvaluationReport> {
    let EvaluationOptions {
        evaluation_set,
        limit,
        repetitions,
        short_evidence_chars,
        min_coverage,
    } = options;
    if !(1..=100).contains(&limit) || !(1..=20).contains(&repetitions) {
        return Err(Error::Invalid(
            "Evaluation limit must be 1–100 and repetitions 1–20",
        ));
    }
    let short = short_evidence_chars.unwrap_or(SHORT_EVIDENCE_CHARS);
    let coverage = min_coverage.unwrap_or(MIN_COVERAGE);
    if short > 10_000 || !(coverage > 0.0 && coverage <= 1.0) {
        return Err(Error::Invalid(
            "shortEvidenceChars must be 0–10000 and minCoverage above 0 and at most 1",
        ));
    }
    let cases = load_cases(evaluation_set, cancel)?;
    let encoded = serde_json::to_vec(&cases)
        .map_err(|_| Error::Invalid("Could not encode the evaluation set"))?;
    let evaluation_set_hash = blake3::hash(&encoded).to_hex().to_string();
    let index_revision = snapshot.revision();
    let targets = cases
        .iter()
        .map(|case| {
            case.evidence
                .iter()
                .map(|quote| find(snapshot, locations, quote, cancel))
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    let resolved_evidence = targets
        .iter()
        .flatten()
        .filter(|target| target.is_some())
        .count();
    let unresolved_evidence = targets.iter().flatten().count() - resolved_evidence;
    let methods = RetrievalMethod::ALL;
    let mut measurements = methods.map(|_| Measurements::default());

    for (case_index, case) in cases.iter().enumerate() {
        // Each candidate gets a warm-up on the same query. Alternate the order in timed runs.
        for method in methods {
            cancel.check()?;
            snapshot.search(method, &case.question, limit, cancel)?;
        }
        for round in 0..repetitions {
            for position in 0..methods.len() {
                let index = (position + round + case_index) % methods.len();
                cancel.check()?;
                let start = Instant::now();
                let (hits, _) = snapshot.search(methods[index], &case.question, limit, cancel)?;
                measurements[index]
                    .latency
                    .push(start.elapsed().as_secs_f64() * 1000.0);
                if round == 0 {
                    measurements[index].recall +=
                        recovered(&targets[case_index], &hits, short as f64, coverage);
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
        resolved_evidence,
        unresolved_evidence,
        candidates,
    })
}

/// The quote's byte range in the current document, or `None` when the document is missing or
/// the quote is not found exactly once within its hints.
fn find(
    snapshot: &KeywordSnapshot<'_>,
    locations: &Locations<'_>,
    quote: &EvidenceQuote,
    cancel: &Cancellation,
) -> Result<Option<Target>> {
    cancel.check()?;
    let (source_id, object_id) = quote
        .doc
        .split_once(':')
        .ok_or(Error::Invalid("Evidence doc must be sourceId:objectId"))?;
    let Some(found) = snapshot.object(source_id, object_id)? else {
        return Ok(None);
    };
    let source = found.evidence.source;
    let hints = match quote.page {
        Some(_) => locations(
            &DocumentRef {
                source_id: source.source_id.clone(),
                object_id: source.object_id.clone(),
            },
            &source.version,
        )?,
        None => Vec::new(),
    };
    let mut evidence = vec![("quote".to_owned(), ConfigValue::String(quote.quote.clone()))];
    for (name, value) in [
        ("prefix", &quote.prefix),
        ("suffix", &quote.suffix),
        ("heading", &quote.heading),
    ] {
        if let Some(value) = value {
            evidence.push((name.to_owned(), ConfigValue::String(value.clone())));
        }
    }
    if let Some(page) = quote.page {
        evidence.push(("page".to_owned(), ConfigValue::Number(f64::from(page))));
    }
    let matched = position(&found.content, &ConfigValue::Object(evidence), &hints);
    let Some(range) = matched.get("range") else {
        return Ok(None);
    };
    let bound = |name: &str| match range.get(name) {
        Some(ConfigValue::Number(value)) => *value,
        _ => f64::NAN,
    };
    Ok(Some(Target {
        source_id: source.source_id,
        object_id: source.object_id,
        version: source.version,
        content: found.content,
        range: (bound("start"), bound("end")),
    }))
}

/// The share of a case's quotes that the results cover.
fn recovered(targets: &[Option<Target>], hits: &[SearchHit], short: f64, coverage: f64) -> f64 {
    let covered = targets
        .iter()
        .flatten()
        .filter(|target| {
            let ranges = hits
                .iter()
                .map(|hit| &hit.evidence)
                .filter(|evidence| {
                    evidence.source.source_id == target.source_id
                        && evidence.source.object_id == target.object_id
                        && evidence.source.version == target.version
                })
                .map(|evidence| match evidence.byte_range {
                    Some(range) => (range.start as f64, range.end as f64),
                    None => line_bytes(
                        &target.content,
                        evidence.range.start_line as f64,
                        evidence.range.end_line as f64,
                    ),
                });
            covers(target.range, ranges, &target.content, short, coverage)
        })
        .count();
    covered as f64 / targets.len() as f64
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
        query_groups(&case.question)?;
        if !(1..=100).contains(&case.evidence.len()) {
            return Err(Error::Invalid(
                "Each question must have 1–100 evidence quotes",
            ));
        }
        let mut seen = HashSet::new();
        for quote in &case.evidence {
            let (source_id, object_id) = quote
                .doc
                .split_once(':')
                .ok_or(Error::Invalid("Evidence doc must be sourceId:objectId"))?;
            source::validate_id(source_id)?;
            if object_id.is_empty()
                || quote.quote.trim().is_empty()
                || quote.page == Some(0)
                || !seen.insert((&quote.doc, &quote.quote))
            {
                return Err(Error::Invalid(
                    "Evidence quotes must name a document, quote nonblank text and be unique per question",
                ));
            }
        }
    }
    Ok(cases)
}

#[derive(Default)]
struct Measurements {
    recall: f64,
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
            evidence_recall_at_k: self.recall / query_count as f64,
            p95_latency_ms: self.latency[p95_rank - 1],
            latency_samples,
        }
    }
}
