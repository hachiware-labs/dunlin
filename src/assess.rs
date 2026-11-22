//! Evaluation on the new store (docs/data-management.md, 11.3): the evaluation data and the
//! result of an evaluation are snapshots, like the documents and the index.
//!
//! An `eval-data` snapshot lists the cases. An `eval-report` snapshot lists what a search found
//! for each case; its parents are the index and the evaluation data, and its key holds the
//! search settings, so the same evaluation is not run twice. The time a search took is not in
//! the report: it would make the same result a different version.
//!
//! Not here yet: places of evidence other than a quote, measures other than whether the
//! evidence was found, and reading the evaluation data the CLI keeps.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::index::search;
use crate::records::{ModuleIdentity, ObjectId, Process, canonical};
use crate::store::{Committed, SnapshotId, Store, StoreError, StoreResult};

/// A question with the evidence that answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Case {
    pub id: String,
    pub question: String,
    pub source: String,
    pub document: String,
    /// The text of the evidence, as it is in the document.
    pub quote: String,
    /// `dev` for tuning or `test` for the final check.
    pub split: String,
}

pub struct EvalRequest<'a> {
    /// The `index` snapshot to search.
    pub index: SnapshotId,
    /// The `eval-data` snapshot.
    pub data: SnapshotId,
    /// The search settings, from the `search` section.
    pub retrieval: &'a Process,
    /// The most chunks a search returns.
    pub top_k: usize,
    /// The cases to use: `dev` or `test`.
    pub split: &'a str,
    /// Where the directories of indexes are.
    pub directory: &'a Path,
    pub tag: Option<&'a str>,
    /// The configuration file as it is now. It is kept with the run.
    pub config: Option<&'a [u8]>,
    /// The run that started this one, such as an optimization.
    pub cause: Option<&'a str>,
}

/// What an evaluation found, read from its snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    /// Each case with the place of the evidence among what was returned, from one.
    pub cases: Vec<(String, Option<usize>)>,
    pub found: usize,
    /// The share of cases whose evidence was returned.
    pub recall: f64,
    /// The mean reciprocal rank: one for evidence returned first, a half for second, and so on,
    /// nothing for a miss. It tells apart results of the same recall by how high the evidence
    /// comes.
    pub mrr: f64,
}

impl Report {
    /// Whether this result is better than `other`: by recall, and by MRR when the recall is the
    /// same.
    pub fn better(recall: f64, mrr: f64, other_recall: f64, other_mrr: f64) -> std::cmp::Ordering {
        recall
            .total_cmp(&other_recall)
            .then(mrr.total_cmp(&other_mrr))
    }
}

#[derive(Debug)]
pub struct EvalOutcome {
    pub report: Committed,
    pub result: Report,
}

fn invalid(what: &str) -> StoreError {
    StoreError::new(format!("The saved {what} is not valid."))
}

/// Saves the cases as the evaluation data `tag` names, and as `default`. The same cases are the
/// same version.
pub fn save_eval_data(store: &Store, tag: &str, cases: &[Case]) -> StoreResult<Committed> {
    let mut draft = store.begin_scan("eval-data", "eval-data")?;
    let mut cases: Vec<&Case> = cases.iter().collect();
    cases.sort_by(|left, right| left.id.cmp(&right.id));
    for pair in cases.windows(2) {
        if pair[0].id == pair[1].id {
            return Err(StoreError::new(format!(
                "Two cases have the ID {}.",
                pair[0].id
            )));
        }
    }
    for case in &cases {
        let saved = json!({
            "question": case.question,
            "source": case.source,
            "document": case.document,
            "quote": case.quote,
            "split": case.split,
        });
        let object = draft.save(canonical(&saved).as_bytes())?;
        draft.list(vec![case.id.clone()], object.to_string());
    }
    draft.set_stats(json!({ "cases": cases.len() }));
    let saved = draft.commit(Some(tag), None)?;
    // What was saved last is also the data a configuration means when it says `default`.
    store.tag(crate::pipeline::LATEST, saved.snapshot)?;
    Ok(saved)
}

/// The cases of an `eval-data` snapshot.
pub fn eval_data(store: &Store, snapshot: SnapshotId) -> StoreResult<Vec<Case>> {
    let snapshot = store.snapshot(snapshot)?;
    if snapshot.kind != "eval-data" {
        return Err(StoreError::new("This is not evaluation data."));
    }
    snapshot
        .items
        .iter()
        .map(|(name, object)| {
            let object = ObjectId::parse(object).ok_or_else(|| invalid("case"))?;
            let mut saved: Value =
                serde_json::from_slice(&store.bytes(&object)?).map_err(|_| invalid("case"))?;
            saved["id"] = json!(name.first().ok_or_else(|| invalid("case"))?);
            serde_json::from_value(saved).map_err(|_| invalid("case"))
        })
        .collect()
}

/// What evaluating is to the store: the search settings, and how the result is judged.
pub(crate) fn process(retrieval: &Process, split: &str) -> Process {
    let mut modules = retrieval.modules.clone();
    modules.push(ModuleIdentity {
        name: "eval.evidence".into(),
        compatibility: 1,
        args: json!({ "match": "quote", "split": split }),
    });
    Process { modules }
}

/// What an `eval-report` snapshot holds.
pub fn report(store: &Store, snapshot: SnapshotId) -> StoreResult<Report> {
    let snapshot = store.snapshot(snapshot)?;
    if snapshot.kind != "eval-report" {
        return Err(StoreError::new("This is not the report of an evaluation."));
    }
    let mut cases = Vec::with_capacity(snapshot.items.len());
    for (name, object) in &snapshot.items {
        let object = ObjectId::parse(object).ok_or_else(|| invalid("report"))?;
        let saved: Value =
            serde_json::from_slice(&store.bytes(&object)?).map_err(|_| invalid("report"))?;
        let rank = saved["rank"].as_u64().map(|rank| rank as usize);
        cases.push((name.first().cloned().unwrap_or_default(), rank));
    }
    let found = cases.iter().filter(|(_, rank)| rank.is_some()).count();
    let (recall, mrr) = if cases.is_empty() {
        (0.0, 0.0)
    } else {
        let reciprocal = cases
            .iter()
            .filter_map(|(_, rank)| rank.map(|rank| 1.0 / rank as f64))
            .fold(0.0, |total, part| total + part);
        (
            found as f64 / cases.len() as f64,
            reciprocal / cases.len() as f64,
        )
    };
    Ok(Report {
        cases,
        found,
        recall,
        mrr,
    })
}

/// Searches the index with the question of each case and records whether the evidence was
/// returned. An evaluation of the same index, data and settings is not run again.
pub fn evaluate(store: &Store, request: &EvalRequest<'_>) -> StoreResult<EvalOutcome> {
    let process = process(request.retrieval, request.split);
    let mut draft = store.begin(
        "eval",
        "eval-report",
        &[request.index, request.data],
        &process,
    )?;
    if let Some(config) = request.config {
        draft.set_config(config)?;
    }
    if let Some(cause) = request.cause {
        draft.set_cause(cause);
    }
    if let Some(existing) = draft.existing()? {
        let result = report(store, existing)?;
        draft.set_stats(json!({ "cases": result.cases.len(), "found": result.found }));
        return Ok(EvalOutcome {
            report: draft.finish_reused(existing, request.tag)?,
            result,
        });
    }

    let index = request.index.to_string();
    let started = std::time::Instant::now();
    let mut found = 0;
    let mut cases = 0;
    for case in eval_data(store, request.data)? {
        if case.split != request.split {
            continue;
        }
        cases += 1;
        let returned = search(
            store,
            request.directory,
            &index,
            &case.question,
            request.top_k,
        )?;
        let rank = returned
            .iter()
            .position(|hit| {
                hit.source == case.source
                    && hit.document == case.document
                    && hit.text.contains(&case.quote)
            })
            .map(|position| position + 1);
        found += usize::from(rank.is_some());
        let saved = json!({
            "rank": rank,
            "returned": returned
                .iter()
                .map(|hit| json!({
                    "source": hit.source,
                    "document": hit.document,
                    "lines": [hit.lines.0, hit.lines.1],
                }))
                .collect::<Vec<_>>(),
        });
        let object = draft.save(canonical(&saved).as_bytes())?;
        draft.list(vec![case.id], object.to_string());
    }
    draft.set_stats(json!({
        "cases": cases,
        "found": found,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    }));
    let committed = draft.commit(request.tag, None)?;
    Ok(EvalOutcome {
        result: report(store, committed.snapshot)?,
        report: committed,
    })
}
