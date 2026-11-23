//! Evaluation (spec 28.11 step 4a): evidence positions, scoring, reports and evidence maps.
//!
//! Positions, identifiers, reports and maps are the ones the JavaScript implementation made, with
//! the same JSON key order, so that saved positions, maps and earlier reports stay usable. Quotes
//! match on NFKC text with collapsed white space, and ranges are byte offsets of the document,
//! widened to whole grapheme clusters.

// The entry points are called from `node`, which test builds leave out.
#![cfg_attr(test, allow(dead_code))]

use std::cmp::Ordering;
use std::time::Instant;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;

use crate::config_value::{ConfigValue, js_number};
use crate::evidence_match::{covers, line_bytes, nearby_quotes, normalized, position, range_value};
use crate::index_stage::{assign, digest, items, object, sql, string_field, text, truthy};
use crate::registry::js_space;
use crate::search_stage::{SearchFailure, SearchHost, SearchRequest, VectorCache};
use crate::source_load::sha256_hex;
use crate::source_stage::iso_now;
use crate::transform_stage::StageError;
use crate::{Cancellation, DocumentRef, KnowledgeBase, SourceLocation};

/// What `eval` scores: the search of each question, the selected evaluation data and the
/// settings that `evaluation_config_id` covers.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateRequest {
    /// The search plan and models. `query` is replaced by each question.
    pub search: SearchRequest,
    /// `state.sqlite`.
    pub state: String,
    /// The build manifest the report is for.
    pub build: ConfigValue,
    /// `eval-data.use` of the configuration.
    #[serde(default)]
    pub dataset: Option<String>,
    /// The validated `eval` settings with `contextBudget`, `construction` and `searchModules`, in
    /// the order JavaScript built them.
    pub settings: ConfigValue,
    #[serde(default)]
    pub split: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    pub run_id: String,
}

/// An `evidence-map` command.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceMapRequest {
    pub state: String,
    /// `list`, `show`, `import`, `confirm` or `create`.
    pub action: String,
    /// The map ID, or the file to import.
    #[serde(default)]
    pub reference: Option<String>,
    /// `eval-data.use` of the configuration, for `create`.
    #[serde(default)]
    pub dataset: Option<String>,
    /// The build whose documents the positions are in, for `create`, `import` and `confirm`.
    #[serde(default)]
    pub build: ConfigValue,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub reviewer: Option<String>,
}

pub(crate) fn invalid(message: impl Into<String>) -> StageError {
    StageError::coded("INVALID_EVAL_DATA", message, 2)
}

pub(crate) fn missing(message: impl Into<String>) -> StageError {
    StageError::coded("EVAL_DATA_REQUIRED", message, 3)
}

fn unattached(error: StageError) -> SearchFailure {
    SearchFailure {
        error,
        module: None,
    }
}

fn aborted(_: crate::Error) -> StageError {
    StageError::aborted()
}

pub(crate) fn parse(data: &str) -> Result<ConfigValue, StageError> {
    serde_json::from_str(data)
        .map_err(|_| StageError::plain("Saved evaluation data is not valid JSON."))
}

pub(crate) fn num(value: f64) -> ConfigValue {
    ConfigValue::Number(value)
}

pub(crate) fn number(value: Option<&ConfigValue>) -> f64 {
    match value {
        Some(ConfigValue::Number(value)) => *value,
        _ => f64::NAN,
    }
}

pub(crate) fn open_state(path: &str, read_only: bool) -> Result<Connection, StageError> {
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
    };
    let connection = Connection::open_with_flags(path, flags).map_err(sql)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(10))
        .map_err(sql)?;
    Ok(connection)
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool, StageError> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE name = ?1",
            [name],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(sql)
}

/// The evaluation data a reference (`eval-data:<id>`, an ID or a tag) selects.
pub(crate) fn dataset(
    connection: &Connection,
    reference: Option<&str>,
) -> Result<ConfigValue, StageError> {
    let id = reference
        .map(|reference| reference.strip_prefix("eval-data:").unwrap_or(reference))
        .filter(|id| !id.is_empty());
    let mut row: Option<String> = None;
    if let Some(id) = id {
        row = connection
            .query_row(
                "SELECT data FROM evaluation_datasets WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?;
        if row.is_none() && table_exists(connection, "evaluation_dataset_tags")? {
            row = connection
                .query_row(
                    "SELECT data FROM evaluation_datasets JOIN evaluation_dataset_tags ON id = dataset_id WHERE name = ?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql)?;
        }
    }
    let row =
        row.ok_or_else(|| missing("Select evaluation data with eval-data add, import, or use."))?;
    parse(&row)
}

/// `String(value)` of JavaScript, for values that reach a message.
pub(crate) fn js_string(value: Option<&ConfigValue>) -> String {
    match value {
        None => "undefined".into(),
        Some(ConfigValue::String(text)) => text.clone(),
        Some(ConfigValue::Number(value)) => js_number(*value),
        Some(ConfigValue::NonFinite(name)) => name.clone(),
        Some(ConfigValue::Null) => "null".into(),
        Some(ConfigValue::Bool(value)) => value.to_string(),
        Some(ConfigValue::Array(_)) => String::new(),
        Some(ConfigValue::Object(_)) => "[object Object]".into(),
    }
}

/// The key of a saved position: the quote, its hints and the document text.
pub(crate) fn position_key(evidence: &ConfigValue, document: &Document) -> String {
    let get = |name: &str| evidence.get(name).cloned().unwrap_or_default();
    let mut key = vec![
        text("nfkc-whitespace-v1"),
        get("doc"),
        get("quote"),
        get("prefix"),
        get("suffix"),
        text(&document.content),
    ];
    if evidence.get("page").is_some() || evidence.get("heading").is_some() {
        key.push(get("page"));
        key.push(get("heading"));
        key.push(document.locations_json.clone());
    }
    digest("pos", &ConfigValue::Array(key))
}

/// A document of the build, as evaluation reads it.
pub(crate) struct Document {
    pub(crate) content: String,
    pub(crate) source_id: String,
    pub(crate) object_id: String,
    pub(crate) version: String,
    pub(crate) url: String,
    /// The locations of its corpus revision, read for evidence with a page hint.
    locations: Vec<SourceLocation>,
    /// `locations` as JavaScript saw them in a position key: sorted keys, or `null` when not read
    /// or empty.
    locations_json: ConfigValue,
}

/// The current document of `source-id:object-id` in the build, with its corpus locations when
/// `metadata` is set.
pub(crate) fn load_document(
    database: &KnowledgeBase,
    doc: &str,
    metadata: bool,
    cancel: &Cancellation,
) -> Result<Option<Document>, StageError> {
    let (source_id, object_id) = match doc.find(':') {
        Some(separator) => (&doc[..separator], &doc[separator + 1..]),
        None => (
            &doc[..doc.char_indices().last().map_or(0, |(index, _)| index)],
            doc,
        ),
    };
    let reference = DocumentRef {
        source_id: source_id.to_owned(),
        object_id: object_id.to_owned(),
    };
    let Some(found) = database
        .get_document(&reference, cancel)
        .map_err(|error| StageError::plain(error.to_string()))?
    else {
        return Ok(None);
    };
    let source = found.evidence.source;
    let mut document = Document {
        content: found.content,
        source_id: source.source_id,
        object_id: source.object_id,
        version: source.version,
        url: source.url,
        locations: Vec::new(),
        locations_json: ConfigValue::Null,
    };
    if metadata {
        let corpus = database
            .get_corpus_document(&reference, &document.version, cancel)
            .map_err(|error| StageError::plain(error.to_string()))?;
        if let Some(corpus) = corpus
            && !corpus.metadata.locations.is_empty()
        {
            // JavaScript received the locations through `serde_json::Value`, with sorted keys.
            let sorted = serde_json::to_value(&corpus.metadata.locations)
                .and_then(|value| serde_json::to_string(&value))
                .map_err(|error| StageError::plain(error.to_string()))?;
            document.locations_json = parse(&sorted)?;
            document.locations = corpus.metadata.locations;
        }
    }
    Ok(Some(document))
}

/// Whether an evidence item has a page hint, which loads the document's locations.
pub(crate) fn has_page(evidence: &ConfigValue) -> bool {
    truthy(evidence.get("page"))
}

/// The byte range of a search result in its document.
fn original_range(content: &str, evidence: &ConfigValue) -> (f64, f64) {
    if let Some(range) = evidence
        .get("byteRange")
        .filter(|range| truthy(Some(range)))
    {
        return (number(range.get("start")), number(range.get("end")));
    }
    let range = evidence.get("range");
    line_bytes(
        content,
        number(range.and_then(|range| range.get("startLine"))),
        number(range.and_then(|range| range.get("endLine"))),
    )
}

/// Whether search results cover enough of an evidence range.
fn covered(
    target: (f64, f64),
    results: &[&ConfigValue],
    document: &Document,
    recall: &ConfigValue,
) -> bool {
    coverage(
        target,
        results,
        document,
        number(recall.get("short_evidence_chars")),
        number(recall.get("min_coverage")),
    )
}

/// Whether search results hold any of an evidence range, even too little of it to count.
fn touched(target: (f64, f64), results: &[&ConfigValue], document: &Document) -> bool {
    coverage(target, results, document, 0.0, f64::MIN_POSITIVE)
}

/// The place of the evidence among `results`, from one: the fewest first results that cover it.
/// `None` when all of them together do not.
fn rank_in(
    target: (f64, f64),
    results: &[&ConfigValue],
    document: &Document,
    recall: &ConfigValue,
) -> Option<usize> {
    if results.is_empty() || !covered(target, results, document, recall) {
        return None;
    }
    // More results never cover less, so the fewest that cover it can be halved down to.
    let (mut low, mut high) = (1, results.len());
    while low < high {
        let middle = (low + high) / 2;
        if covered(target, &results[..middle], document, recall) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    Some(low)
}

/// `mrr@k` and `ndcg@k` of a case from the rank of each of its evidence items. Each item counts
/// alike, as in the recall: the reciprocal ranks are averaged over the items, and the ideal
/// order of nDCG puts every item first. A rank beyond `k`, or none, counts as a miss.
fn rank_scores(ranks: &[Option<usize>], at: &[usize]) -> Vec<(String, f64)> {
    let gain = |rank: usize| 1.0 / (rank as f64 + 1.0).log2();
    let mut scores = Vec::new();
    for &k in at {
        let within: Vec<usize> = ranks
            .iter()
            .filter_map(|rank| rank.filter(|rank| *rank <= k))
            .collect();
        let reciprocal = within.iter().map(|rank| 1.0 / *rank as f64).sum::<f64>();
        let ideal = (1..=ranks.len().min(k)).map(gain).sum::<f64>();
        let found = within.iter().map(|rank| gain(*rank)).sum::<f64>();
        let count = ranks.len().max(1) as f64;
        scores.push((format!("mrr@{k}"), reciprocal / count));
        scores.push((
            format!("ndcg@{k}"),
            if ideal > 0.0 {
                (found / ideal).min(1.0)
            } else {
                0.0
            },
        ));
    }
    scores
}

/// Where each evidence item of a case was kept and lost (docs/retrieval-techniques.md, E2).
struct Stages {
    /// The share of the items each stage held: each retrieval module, all of them together
    /// (`retrieved`), the candidates the context was assembled from, and the context.
    recall: Vec<(String, f64)>,
    /// The place of each item among the candidates, from one.
    rank: Vec<Option<usize>>,
    /// Why each item the primary recall missed was missed; `None` for an item it found.
    lost: Vec<Option<&'static str>>,
}

fn stages(
    matches: &[Match],
    outcome: &crate::search_stage::SearchOutcome,
    recall: &ConfigValue,
    primary: Option<&[bool]>,
    primary_is_context: bool,
) -> Stages {
    let retrieved: Vec<(&str, Vec<&ConfigValue>)> = outcome
        .retrieved
        .iter()
        .map(|(name, found)| (name.as_str(), found.iter().collect()))
        .collect();
    let union: Vec<&ConfigValue> = outcome
        .retrieved
        .iter()
        .flat_map(|(_, found)| found)
        .collect();
    let candidates: Vec<&ConfigValue> = outcome.candidates.iter().collect();
    let context: Vec<&ConfigValue> = outcome.context.iter().collect();
    let budget_ran_out = matches!(outcome.context_stop, "budget" | "no_truncation");
    let held = |list: &[&ConfigValue]| -> Vec<bool> {
        matches
            .iter()
            .map(|matched| match (matched.range, &matched.document) {
                (Some(range), Some(document)) => covered(range, list, document, recall),
                _ => false,
            })
            .collect()
    };
    let share = |flags: &[bool]| {
        flags.iter().filter(|flag| **flag).count() as f64 / flags.len().max(1) as f64
    };
    let mut shares = Vec::new();
    for (name, list) in &retrieved {
        shares.push(((*name).to_owned(), share(&held(list))));
    }
    let in_union = held(&union);
    let in_candidates = held(&candidates);
    shares.push(("retrieved".into(), share(&in_union)));
    shares.push(("candidates".into(), share(&in_candidates)));
    shares.push(("context".into(), share(&held(&context))));
    let rank = matches
        .iter()
        .map(|matched| match (matched.range, &matched.document) {
            (Some(range), Some(document)) => rank_in(range, &candidates, document, recall),
            _ => None,
        })
        .collect();
    let lost = matches
        .iter()
        .enumerate()
        .map(|(index, matched)| {
            if primary.is_some_and(|flags| flags[index]) {
                return None;
            }
            let (Some(range), Some(document)) = (matched.range, &matched.document) else {
                return Some("unresolved");
            };
            Some(if !in_union[index] {
                "not_retrieved"
            } else if !in_candidates[index] {
                "dropped_by_fusion"
            } else if !primary_is_context {
                "ranked_low"
            } else if touched(range, &context, document) {
                "truncated"
            } else if budget_ran_out {
                // The context takes the candidates in order; the budget ended before this one.
                "over_budget"
            } else {
                "ranked_low"
            })
        })
        .collect();
    Stages {
        recall: shares,
        rank,
        lost,
    }
}

/// The `stages` of a report row.
fn stages_value(held: &Stages) -> ConfigValue {
    object(vec![
        (
            "recall",
            ConfigValue::Object(
                held.recall
                    .iter()
                    .map(|(name, share)| (name.clone(), num(*share)))
                    .collect(),
            ),
        ),
        (
            "rank",
            ConfigValue::Array(
                held.rank
                    .iter()
                    .map(|rank| rank.map_or(ConfigValue::Null, |rank| num(rank as f64)))
                    .collect(),
            ),
        ),
        (
            "lost",
            ConfigValue::Array(
                held.lost
                    .iter()
                    .map(|lost| lost.map_or(ConfigValue::Null, text))
                    .collect(),
            ),
        ),
    ])
}

/// The `stages` of a report: the mean share each stage held, and how many evidence items were
/// lost for each reason.
fn stages_summary(rows: &[Stages]) -> ConfigValue {
    let mut names: Vec<&str> = Vec::new();
    for row in rows {
        for (name, _) in &row.recall {
            if !names.contains(&name.as_str()) {
                names.push(name);
            }
        }
    }
    let recall = names
        .iter()
        .map(|name| {
            let shares: Vec<f64> = rows
                .iter()
                .filter_map(|row| {
                    row.recall
                        .iter()
                        .find(|(existing, _)| existing == name)
                        .map(|(_, share)| *share)
                })
                .collect();
            ((*name).to_owned(), mean(&shares))
        })
        .collect();
    let mut lost: Vec<(String, ConfigValue)> = Vec::new();
    for reason in [
        "unresolved",
        "not_retrieved",
        "dropped_by_fusion",
        "ranked_low",
        "over_budget",
        "truncated",
    ] {
        let count = rows
            .iter()
            .flat_map(|row| &row.lost)
            .filter(|lost| **lost == Some(reason))
            .count();
        lost.push((reason.into(), num(count as f64)));
    }
    object(vec![
        ("recall", ConfigValue::Object(recall)),
        ("lost", ConfigValue::Object(lost)),
    ])
}

fn coverage(
    target: (f64, f64),
    results: &[&ConfigValue],
    document: &Document,
    short_evidence_chars: f64,
    min_coverage: f64,
) -> bool {
    let ranges = results
        .iter()
        .filter(|item| {
            let source = item.get("source");
            let field = |name: &str| source.and_then(|source| string_field(source, name));
            field("objectId") == Some(document.object_id.as_str())
                && field("sourceId") == Some(document.source_id.as_str())
                && field("version") == Some(document.version.as_str())
        })
        .map(|item| original_range(&document.content, item));
    covers(
        target,
        ranges,
        &document.content,
        short_evidence_chars,
        min_coverage,
    )
}

pub(crate) fn mean(values: &[f64]) -> ConfigValue {
    if values.is_empty() {
        ConfigValue::Null
    } else {
        num(values.iter().sum::<f64>() / values.len() as f64)
    }
}

pub(crate) fn evaluation_cases<'a>(
    data: &'a ConfigValue,
    split: &str,
    include_generated: bool,
) -> Vec<&'a ConfigValue> {
    let included: &[&str] = if include_generated {
        &["provided", "reviewed", "generated"]
    } else {
        &["provided", "reviewed"]
    };
    items(data.get("cases"))
        .iter()
        .filter(|item| {
            string_field(item, "split") == Some(split)
                && string_field(item, "state").is_some_and(|state| included.contains(&state))
        })
        .collect()
}

/// The recall that decides whether a case recovered its evidence.
pub(crate) fn primary_recall(names: &[String]) -> String {
    if names.iter().any(|name| name == "evidence_recall@context") {
        return "evidence_recall@context".into();
    }
    // Other scores, such as `mrr@10`, do not decide it.
    let largest = names
        .iter()
        .filter_map(|name| name.strip_prefix("evidence_recall@"))
        .map(|at| at.parse::<f64>().unwrap_or(f64::NAN))
        .fold(f64::NEG_INFINITY, |a, b| {
            if a.is_nan() || b.is_nan() {
                f64::NAN
            } else {
                a.max(b)
            }
        });
    format!("evidence_recall@{}", js_number_key(largest))
}

/// A number as a property name: `String(value)`.
fn js_number_key(value: f64) -> String {
    if value.is_nan() {
        "NaN".into()
    } else if value.is_infinite() {
        if value > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else {
        js_number(value)
    }
}

/// `'evidence_recall@' + at`.
fn recall_name(at: &ConfigValue) -> String {
    format!("evidence_recall@{}", js_string(Some(at)))
}

/// Puts integer property names first in ascending order, as JavaScript objects do.
fn js_property_order(entries: Vec<(String, ConfigValue)>) -> Vec<(String, ConfigValue)> {
    let index = |name: &str| -> Option<u32> {
        let value: u32 = name.parse().ok()?;
        (value != u32::MAX && value.to_string() == name).then_some(value)
    };
    let (mut indexed, named): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .partition(|(name, _)| index(name).is_some());
    indexed.sort_by_key(|(name, _)| index(name));
    indexed.extend(named);
    indexed
}

pub(crate) fn method_id(settings: &ConfigValue) -> String {
    let mut entries = Vec::new();
    assign(&mut entries, "recall", settings.get("recall").cloned());
    assign(
        &mut entries,
        "percentiles",
        settings.get("percentiles").cloned(),
    );
    assign(
        &mut entries,
        "include_generated",
        settings.get("includeGenerated").cloned(),
    );
    // `metric.evidence_rank` adds scores but judges no case differently, so it is not here:
    // reports with and without it compare on the scores they share.
    digest("method", &ConfigValue::Object(entries))
}

/// Saves the positions of a dataset in a build's documents as an evidence map.
fn save_evidence_map(
    connection: &Connection,
    dataset_id: ConfigValue,
    build: &ConfigValue,
    positions: Vec<(String, ConfigValue)>,
) -> Result<ConfigValue, StageError> {
    let mut value = vec![("dataset_id".to_owned(), dataset_id)];
    assign(&mut value, "source_id", build.get("source_id").cloned());
    value.push(("positions".into(), ConfigValue::Object(positions)));
    let id = digest("map", &ConfigValue::Object(value.clone()));
    let mut map = vec![("id".to_owned(), text(&id))];
    assign(&mut map, "build_id", build.get("build_id").cloned());
    for (name, entry) in value {
        assign(&mut map, &name, Some(entry));
    }
    let map = ConfigValue::Object(map);
    connection
        .execute(
            "INSERT OR IGNORE INTO evidence_maps VALUES (?1, ?2)",
            (&id, map.to_js_json()),
        )
        .map_err(sql)?;
    Ok(map)
}

/// `{ ...a, ...b }`
fn spread(
    mut entries: Vec<(String, ConfigValue)>,
    more: &ConfigValue,
) -> Vec<(String, ConfigValue)> {
    if let ConfigValue::Object(more) = more {
        for (name, value) in more {
            assign(&mut entries, name, Some(value.clone()));
        }
    }
    entries
}

/// The fields every saved position starts with.
fn position_head(
    key: &str,
    evidence: &ConfigValue,
    document: &Document,
) -> Vec<(String, ConfigValue)> {
    let mut entries = vec![("key".to_owned(), text(key))];
    assign(&mut entries, "doc", evidence.get("doc").cloned());
    assign(&mut entries, "quote", evidence.get("quote").cloned());
    entries.push(("version".into(), text(&document.version)));
    entries.push((
        "content_hash".into(),
        text(&sha256_hex(document.content.as_bytes())),
    ));
    entries
}

/// One evidence item of a case, matched in the build.
struct Match {
    range: Option<(f64, f64)>,
    reason: Option<ConfigValue>,
    document: Option<Document>,
    key: Option<String>,
}

fn range_of(position: &ConfigValue) -> Option<(f64, f64)> {
    let range = position.get("range").filter(|range| truthy(Some(range)))?;
    Some((number(range.get("start")), number(range.get("end"))))
}

/// Scores a build with the selected evaluation data and saves the report (`eval`).
pub fn evaluate(
    database: &KnowledgeBase,
    vectors: &VectorCache,
    mut request: EvaluateRequest,
    host: &dyn SearchHost,
    cancel: &Cancellation,
) -> Result<ConfigValue, SearchFailure> {
    let connection = open_state(&request.state, false).map_err(unattached)?;
    let data = dataset(&connection, request.dataset.as_deref()).map_err(unattached)?;
    let split = request.split.clone().unwrap_or_else(|| "dev".into());
    let settings = request.settings.clone();
    let recall = settings.get("recall").cloned().unwrap_or_default();
    let cases = evaluation_cases(&data, &split, truthy(settings.get("includeGenerated")));
    if cases.is_empty() {
        return Err(unattached(missing(format!(
            "No evaluation cases in split {split}."
        ))));
    }
    if let Some(tag) = request.tag.as_deref().filter(|tag| !tag.is_empty()) {
        let taken = connection
            .query_row(
                "SELECT id FROM evaluation_reports WHERE tag = ?1",
                [tag],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| unattached(sql(error)))?;
        if taken.is_some() {
            return Err(unattached(invalid("Report tag already exists.")));
        }
    }
    let at = items(recall.get("at")).to_vec();
    let rank_at: Vec<usize> = items(settings.get("rank").and_then(|rank| rank.get("at")))
        .iter()
        .map(|value| number(Some(value)))
        .filter(|value| value.is_finite() && *value >= 1.0)
        .map(|value| value as usize)
        .collect();
    let mut stage_rows: Vec<Stages> = Vec::new();
    let mut positions: Vec<(String, ConfigValue)> = Vec::new();
    let mut rows: Vec<ConfigValue> = Vec::new();
    let mut latencies: Vec<f64> = Vec::new();
    let mut context_chars: Vec<ConfigValue> = Vec::new();
    let mut context_stops: Vec<&'static str> = Vec::new();
    let mut complete_rows: Vec<bool> = Vec::new();
    let mut row_scores: Vec<Vec<(String, f64)>> = Vec::new();
    let mut unresolved_total = 0;
    let mut search_id = ConfigValue::Null;
    let fail = |error: StageError| unattached(error);
    for item in &cases {
        cancel.check().map_err(|error| fail(aborted(error)))?;
        let started = Instant::now();
        request.search.query = string_field(item, "question")
            .unwrap_or_default()
            .to_owned();
        let outcome = crate::search_stage::run(database, vectors, &request.search, host, cancel)?;
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        search_id = outcome
            .output
            .get("search_config_id")
            .cloned()
            .unwrap_or_default();
        let mut matches = Vec::new();
        for evidence in items(item.get("evidence")) {
            let doc = string_field(evidence, "doc").unwrap_or_default();
            let Some(document) =
                load_document(database, doc, has_page(evidence), cancel).map_err(fail)?
            else {
                matches.push(Match {
                    range: None,
                    reason: Some(text("source_missing")),
                    document: None,
                    key: None,
                });
                continue;
            };
            let key = position_key(evidence, &document);
            let previous = saved(&connection, "evidence_overrides", &key)
                .and_then(|found| match found {
                    Some(found) => Ok(Some(found)),
                    None => saved(&connection, "evidence_positions", &key),
                })
                .map_err(fail)?;
            let mut matched = match &previous {
                Some(previous) => previous.clone(),
                None => position(&document.content, evidence, &document.locations),
            };
            if string_field(&matched, "reason") == Some("quote_not_found")
                && matched.get("suggestions").is_none()
                && let ConfigValue::Object(entries) = &mut matched
            {
                let query = normalized(string_field(evidence, "quote").unwrap_or_default()).value;
                entries.push((
                    "suggestions".into(),
                    nearby_quotes(&document.content, &query),
                ));
            }
            if previous.is_none() {
                connection
                    .execute(
                        "INSERT OR IGNORE INTO evidence_positions VALUES (?1, ?2)",
                        (&key, matched.to_js_json()),
                    )
                    .map_err(|error| fail(sql(error)))?;
            }
            let entry = spread(position_head(&key, evidence, &document), &matched);
            assign(&mut positions, &key, Some(ConfigValue::Object(entry)));
            matches.push(Match {
                range: range_of(&matched),
                reason: matched.get("reason").cloned(),
                document: Some(document),
                key: Some(key),
            });
        }
        let results = items(outcome.output.get("results"));
        let ranked: Vec<&ConfigValue> = results
            .iter()
            .map(|result| result.get("evidence").unwrap_or(&ConfigValue::Null))
            .collect();
        let context: Vec<&ConfigValue> = outcome.context.iter().collect();
        let mut recovered: Vec<(String, Vec<bool>)> = Vec::new();
        for value in &at {
            let hits: &[&ConfigValue] = match value {
                ConfigValue::String(name) if name == "context" => &context,
                value => {
                    let count = number(Some(value));
                    let count = if count.is_nan() || count <= 0.0 {
                        0
                    } else {
                        (count as usize).min(ranked.len())
                    };
                    &ranked[..count]
                }
            };
            let flags = matches
                .iter()
                .map(|matched| match (matched.range, &matched.document) {
                    (Some(range), Some(document)) => covered(range, hits, document, &recall),
                    _ => false,
                })
                .collect();
            let name = recall_name(value);
            match recovered.iter_mut().find(|(existing, _)| *existing == name) {
                Some(entry) => entry.1 = flags,
                None => recovered.push((name, flags)),
            }
        }
        let mut scores: Vec<(String, f64)> = recovered
            .iter()
            .map(|(name, flags)| {
                let total = flags.iter().filter(|flag| **flag).count() as f64;
                (name.clone(), total / flags.len() as f64)
            })
            .collect();
        let names: Vec<String> = recovered.iter().map(|(name, _)| name.clone()).collect();
        let primary_name = primary_recall(&names);
        let primary = recovered
            .iter()
            .find(|(name, _)| *name == primary_name)
            .map(|(_, flags)| flags.clone());
        let held = stages(
            &matches,
            &outcome,
            &recall,
            primary.as_deref(),
            primary_name == "evidence_recall@context",
        );
        scores.extend(rank_scores(&held.rank, &rank_at));
        let ranked_metric = at
            .iter()
            .find(|value| matches!(value, ConfigValue::Number(at) if *at >= ranked.len() as f64));
        let known_ranked = ranked_metric.and_then(|value| {
            let name = recall_name(value);
            recovered
                .iter()
                .find(|(existing, _)| *existing == name)
                .map(|(_, flags)| flags.clone())
        });
        let evidence = (split == "dev").then(|| {
            ConfigValue::Array(
                matches
                    .iter()
                    .enumerate()
                    .map(|(index, matched)| {
                        if primary.as_ref().is_some_and(|flags| flags[index]) {
                            return object(vec![("recovered", ConfigValue::Bool(true))]);
                        }
                        let mut entries = vec![("recovered".to_owned(), ConfigValue::Bool(false))];
                        assign(
                            &mut entries,
                            "position_key",
                            matched.key.as_deref().map(text),
                        );
                        assign(&mut entries, "reason", matched.reason.clone());
                        if let (Some(range), Some(document)) = (matched.range, &matched.document) {
                            let in_results = match &known_ranked {
                                Some(flags) => flags[index],
                                None => covered(range, &ranked, document, &recall),
                            };
                            entries.push(("in_results".into(), ConfigValue::Bool(in_results)));
                        }
                        ConfigValue::Object(entries)
                    })
                    .collect(),
            )
        });
        let unresolved: Vec<ConfigValue> = matches
            .iter()
            .filter(|matched| matched.range.is_none())
            .map(|matched| matched.reason.clone().unwrap_or_default())
            .collect();
        unresolved_total += unresolved.len();
        let complete = matches.iter().all(|matched| matched.range.is_some());
        let chars = outcome
            .output
            .get("context")
            .and_then(|context| context.get("chars"))
            .cloned()
            .unwrap_or_default();
        let mut row = Vec::new();
        assign(&mut row, "id", item.get("id").cloned());
        assign(&mut row, "group", item.get("group").cloned());
        row.push((
            "scores".into(),
            ConfigValue::Object(
                scores
                    .iter()
                    .map(|(name, score)| (name.clone(), num(*score)))
                    .collect(),
            ),
        ));
        row.push(("complete".into(), ConfigValue::Bool(complete)));
        row.push(("unresolved".into(), ConfigValue::Array(unresolved)));
        row.push(("latency_ms".into(), num(ms)));
        row.push(("context_chars".into(), chars.clone()));
        row.push(("context_stop".into(), text(outcome.context_stop)));
        assign(&mut row, "evidence", evidence);
        row.push(("stages".into(), stages_value(&held)));
        stage_rows.push(held);
        rows.push(ConfigValue::Object(row));
        latencies.push(ms);
        context_chars.push(chars);
        context_stops.push(outcome.context_stop);
        complete_rows.push(complete);
        row_scores.push(scores);
    }
    let names: Vec<String> = row_scores[0].iter().map(|(name, _)| name.clone()).collect();
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let dataset_id = data.get("id").cloned().unwrap_or_default();
    let map = save_evidence_map(&connection, dataset_id.clone(), &request.build, positions)
        .map_err(fail)?;
    let budget = settings.get("contextBudget").cloned();
    let budget_value = number(budget.as_ref());
    let short: Vec<usize> = (0..rows.len())
        .filter(|&index| number(Some(&context_chars[index])) < budget_value)
        .collect();
    let mut reasons: Vec<(String, ConfigValue)> = Vec::new();
    for &index in &short {
        let reason = context_stops[index];
        let count = reasons
            .iter()
            .find(|(name, _)| name == reason)
            .map_or(0.0, |(_, count)| number(Some(count)));
        assign(&mut reasons, reason, Some(num(count + 1.0)));
    }
    let score_of = |index: usize, name: &str| -> f64 {
        row_scores[index]
            .iter()
            .find(|(existing, _)| existing == name)
            .map_or(f64::NAN, |(_, score)| *score)
    };
    let scores = names
        .iter()
        .map(|name| {
            let values: Vec<f64> = (0..rows.len()).map(|index| score_of(index, name)).collect();
            (name.clone(), mean(&values))
        })
        .collect();
    let diagnostic = names
        .iter()
        .map(|name| {
            let values: Vec<f64> = (0..rows.len())
                .filter(|&index| complete_rows[index])
                .map(|index| score_of(index, name))
                .collect();
            (name.clone(), mean(&values))
        })
        .collect();
    let mut latency = Vec::new();
    for percentile in items(settings.get("percentiles")) {
        let p = number(Some(percentile));
        let position = (p * latencies.len() as f64 / 100.0).ceil() - 1.0;
        let index = if position.is_nan() || position <= 0.0 {
            0
        } else {
            position as usize
        };
        assign(
            &mut latency,
            &js_string(Some(percentile)),
            latencies.get(index).map(|value| num(*value)),
        );
    }
    let evidence_count: usize = cases
        .iter()
        .map(|item| items(item.get("evidence")).len())
        .sum();
    let chars: Vec<f64> = context_chars
        .iter()
        .map(|value| number(Some(value)))
        .collect();
    let test_runs = if split == "test" {
        let count: i64 = connection
            .query_row(
                "SELECT count(*) AS n FROM evaluation_reports WHERE json_extract(data, '$.dataset_id') = ?1 AND json_extract(data, '$.split') = 'test'",
                [js_string(Some(&dataset_id))],
                |row| row.get(0),
            )
            .map_err(|error| fail(sql(error)))?;
        Some(num(count as f64 + 1.0))
    } else {
        None
    };
    let evaluation_config_id = digest(
        "ecfg",
        &ConfigValue::Array(vec![search_id.clone(), settings.clone()]),
    );
    let mut report = vec![
        ("id".to_owned(), text(&request.run_id)),
        (
            "tag".into(),
            request.tag.as_deref().map_or(ConfigValue::Null, text),
        ),
    ];
    assign(
        &mut report,
        "build_id",
        request.build.get("build_id").cloned(),
    );
    report.push(("search_config_id".into(), search_id));
    report.push(("dataset_id".into(), dataset_id));
    report.push(("method_id".into(), text(&method_id(&settings))));
    assign(&mut report, "evidence_map_id", map.get("id").cloned());
    report.push((
        "build_config_id".into(),
        request
            .build
            .get("config_id")
            .filter(|value| !matches!(value, ConfigValue::Null))
            .cloned()
            .unwrap_or_default(),
    ));
    report.push(("evaluation_config_id".into(), text(&evaluation_config_id)));
    report.push(("split".into(), text(&split)));
    assign(&mut report, "budget_chars", budget);
    assign(
        &mut report,
        "source_id",
        request.build.get("source_id").cloned(),
    );
    assign(
        &mut report,
        "construction",
        settings.get("construction").cloned(),
    );
    assign(
        &mut report,
        "search_settings",
        settings.get("searchModules").cloned(),
    );
    report.push((
        "evidence".into(),
        object(vec![
            ("resolved", num((evidence_count - unresolved_total) as f64)),
            ("unresolved", num(unresolved_total as f64)),
            (
                "fully_resolved_cases",
                num(complete_rows.iter().filter(|complete| **complete).count() as f64),
            ),
        ]),
    ));
    report.push((
        "context".into(),
        object(vec![
            ("average_chars", mean(&chars)),
            ("short_cases", num(short.len() as f64)),
            ("short_ratio", num(short.len() as f64 / rows.len() as f64)),
            ("reasons", ConfigValue::Object(reasons)),
        ]),
    ));
    report.push(("scores".into(), ConfigValue::Object(scores)));
    report.push(("stages".into(), stages_summary(&stage_rows)));
    report.push(("diagnostic".into(), ConfigValue::Object(diagnostic)));
    report.push((
        "latency_ms".into(),
        ConfigValue::Object(js_property_order(latency)),
    ));
    assign(&mut report, "test_runs", test_runs);
    report.push(("rows".into(), ConfigValue::Array(rows)));
    let report = ConfigValue::Object(report);
    cancel.check().map_err(|error| fail(aborted(error)))?;
    connection
        .execute(
            "INSERT INTO evaluation_reports VALUES (?1, ?2, ?3)",
            (&request.run_id, request.tag.as_deref(), report.to_js_json()),
        )
        .map_err(|error| fail(sql(error)))?;
    Ok(report)
}

/// A saved position or confirmed override.
fn saved(
    connection: &Connection,
    table: &str,
    key: &str,
) -> Result<Option<ConfigValue>, StageError> {
    let data: Option<String> = connection
        .query_row(
            &format!("SELECT data FROM {table} WHERE id = ?1"),
            [key],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    data.as_deref().map(parse).transpose()
}

fn evidence_map(
    connection: &Connection,
    id: Option<&ConfigValue>,
) -> Result<ConfigValue, StageError> {
    let data: Option<String> = match id {
        Some(ConfigValue::String(id)) => connection
            .query_row(
                "SELECT data FROM evidence_maps WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?,
        _ => None,
    };
    let data = data.ok_or_else(|| {
        StageError::coded(
            "MAP_REQUIRED",
            format!("Evidence map not found: {}", js_string(id)),
            3,
        )
    })?;
    parse(&data)
}

fn all_digits(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
}

/// `evidence-map list`, `show`, `create`, `import` and `confirm`.
pub fn evidence_maps(
    database: Option<&KnowledgeBase>,
    request: &EvidenceMapRequest,
    cancel: &Cancellation,
) -> Result<ConfigValue, StageError> {
    let mutates = ["create", "import", "confirm"].contains(&request.action.as_str());
    let connection = open_state(&request.state, !mutates)?;
    let reference = request.reference.clone().map(ConfigValue::String);
    match request.action.as_str() {
        "list" => {
            let mut statement = connection
                .prepare("SELECT id FROM evidence_maps ORDER BY rowid DESC")
                .map_err(sql)?;
            let ids = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(sql)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql)?;
            return Ok(ConfigValue::Array(
                ids.iter()
                    .map(|id| object(vec![("id", text(id))]))
                    .collect(),
            ));
        }
        "show" => return evidence_map(&connection, reference.as_ref()),
        _ => {}
    }
    let database =
        database.ok_or_else(|| StageError::plain("Evidence maps need an open build."))?;
    let build = &request.build;
    if request.action == "import" || request.action == "confirm" {
        let input = if request.action == "import" {
            std::fs::read_to_string(request.reference.as_deref().unwrap_or_default())
                .ok()
                .and_then(|data| serde_json::from_str::<ConfigValue>(&data).ok())
                .ok_or_else(|| invalid("Expected an exported evidence map JSON file."))?
        } else {
            let usable = request.key.as_deref().is_some_and(|key| !key.is_empty())
                && all_digits(request.start.as_deref())
                && all_digits(request.end.as_deref())
                && request
                    .reviewer
                    .as_deref()
                    .is_some_and(|reviewer| !reviewer.trim_matches(js_space).is_empty());
            if !usable {
                return Err(invalid(
                    "Use --key <position-id> --start <byte> --end <byte> --reviewer <name>.",
                ));
            }
            let bound = |value: &Option<String>| {
                num(value
                    .as_deref()
                    .unwrap_or_default()
                    .parse::<f64>()
                    .unwrap_or(f64::NAN))
            };
            ConfigValue::Object(vec![
                ("id".into(), reference.clone().unwrap_or_default()),
                (
                    "positions".into(),
                    ConfigValue::Object(vec![(
                        request.key.clone().unwrap_or_default(),
                        object(vec![(
                            "range",
                            object(vec![
                                ("start", bound(&request.start)),
                                ("end", bound(&request.end)),
                            ]),
                        )]),
                    )]),
                ),
            ])
        };
        let previous = evidence_map(&connection, input.get("id"))?;
        let previous_positions = previous.get("positions").cloned().unwrap_or_default();
        if request.action == "confirm"
            && previous_positions
                .get(request.key.as_deref().unwrap_or_default())
                .is_none()
        {
            return Err(invalid("Unknown position ID."));
        }
        if crate::index_stage::js_json(previous.get("source_id"))
            != crate::index_stage::js_json(build.get("source_id"))
        {
            return Err(invalid(
                "Select a build for the same source snapshot using --build.",
            ));
        }
        let mut edits: Vec<(String, ConfigValue)> = Vec::new();
        let ConfigValue::Object(entries) = &previous_positions else {
            return Err(StageError::plain("The evidence map has no positions."));
        };
        for (key, original) in entries {
            let Some(provided) = input
                .get("positions")
                .and_then(|positions| positions.get(key))
                .filter(|provided| truthy(Some(provided)))
            else {
                continue;
            };
            if request.action == "import"
                && crate::index_stage::js_json(provided.get("range"))
                    == crate::index_stage::js_json(original.get("range"))
            {
                continue;
            }
            let doc = string_field(original, "doc").unwrap_or_default();
            let document = load_document(database, doc, false, cancel)?;
            let content = document
                .as_ref()
                .map_or("", |document| document.content.as_str());
            let bytes = content.as_bytes();
            let range = provided.get("range");
            let bound = |name: &str| match range.and_then(|range| range.get(name)) {
                Some(ConfigValue::Number(value)) if value.fract() == 0.0 => Some(*value),
                _ => None,
            };
            let continuation = |offset: f64| {
                bytes
                    .get(offset as usize)
                    .is_some_and(|byte| byte & 0xc0 == 0x80)
            };
            let valid = match (bound("start"), bound("end")) {
                (Some(start), Some(end)) => {
                    start >= 0.0
                        && end > start
                        && end <= bytes.len() as f64
                        && !continuation(start)
                        && !continuation(end)
                        && Some(sha256_hex(bytes).as_str())
                            == string_field(original, "content_hash")
                }
                _ => false,
            };
            let (Some(start), Some(end), true) = (bound("start"), bound("end"), valid) else {
                return Err(invalid(format!("Invalid byte range for {doc}")));
            };
            edits.push((
                key.clone(),
                object(vec![
                    ("range", range_value(start, end)),
                    ("confirmed", ConfigValue::Bool(true)),
                    (
                        "confirmation",
                        object(vec![
                            (
                                "reviewer",
                                request.reviewer.as_deref().map_or(ConfigValue::Null, text),
                            ),
                            ("at", text(&iso_now())),
                        ]),
                    ),
                ]),
            ));
        }
        let transaction = connection.unchecked_transaction().map_err(sql)?;
        for (key, entry) in &edits {
            transaction
                .execute(
                    "INSERT INTO evidence_overrides VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET data = excluded.data",
                    (key, entry.to_js_json()),
                )
                .map_err(sql)?;
        }
        let mut positions = entries.clone();
        for (key, entry) in &edits {
            let original: Vec<(String, ConfigValue)> = match previous_positions.get(key) {
                Some(ConfigValue::Object(original)) => original
                    .iter()
                    .filter(|(name, _)| name != "reason" && name != "candidates")
                    .cloned()
                    .collect(),
                _ => Vec::new(),
            };
            assign(
                &mut positions,
                key,
                Some(ConfigValue::Object(spread(original, entry))),
            );
        }
        let map = save_evidence_map(
            &transaction,
            previous.get("dataset_id").cloned().unwrap_or_default(),
            build,
            positions,
        )?;
        transaction.commit().map_err(sql)?;
        return Ok(map);
    }
    let selected = dataset(&connection, request.dataset.as_deref())?;
    let mut positions: Vec<(String, ConfigValue)> = Vec::new();
    for item in items(selected.get("cases")) {
        for evidence in items(item.get("evidence")) {
            cancel.check().map_err(aborted)?;
            let doc = string_field(evidence, "doc").unwrap_or_default();
            let Some(document) = load_document(database, doc, has_page(evidence), cancel)? else {
                continue;
            };
            let key = position_key(evidence, &document);
            let found = match saved(&connection, "evidence_overrides", &key)? {
                Some(found) => found,
                None => position(&document.content, evidence, &document.locations),
            };
            let entry = spread(position_head(&key, evidence, &document), &found);
            assign(&mut positions, &key, Some(ConfigValue::Object(entry)));
        }
    }
    save_evidence_map(
        &connection,
        selected.get("id").cloned().unwrap_or_default(),
        build,
        positions,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(entries: Vec<(&str, ConfigValue)>) -> ConfigValue {
        object(entries)
    }

    #[test]
    fn orders_integer_keys_first() {
        let ordered = js_property_order(vec![
            ("95".into(), num(1.0)),
            ("99.9".into(), num(2.0)),
            ("50".into(), num(3.0)),
        ]);
        let names: Vec<&str> = ordered.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["50", "95", "99.9"]);
    }

    fn score(scores: &[(String, f64)], name: &str) -> f64 {
        scores
            .iter()
            .find(|(existing, _)| existing == name)
            .unwrap()
            .1
    }

    #[test]
    fn scores_ranks_of_each_evidence_item() {
        // One item, second: MRR a half, nDCG 1 / log2(3).
        let scores = rank_scores(&[Some(2)], &[1, 10]);
        assert_eq!(score(&scores, "mrr@1"), 0.0);
        assert_eq!(score(&scores, "ndcg@1"), 0.0);
        assert_eq!(score(&scores, "mrr@10"), 0.5);
        assert!((score(&scores, "ndcg@10") - 1.0 / 3f64.log2()).abs() < 1e-12);

        // Two items, first and missing: each counts half.
        let scores = rank_scores(&[Some(1), None], &[10]);
        assert_eq!(score(&scores, "mrr@10"), 0.5);
        assert!((score(&scores, "ndcg@10") - 1.0 / (1.0 + 1.0 / 3f64.log2())).abs() < 1e-12);

        // Two items in one first chunk are as good as it gets.
        let scores = rank_scores(&[Some(1), Some(1)], &[10]);
        assert_eq!(score(&scores, "ndcg@10"), 1.0);
        assert_eq!(score(&scores, "mrr@10"), 1.0);
    }

    #[test]
    fn decides_the_primary_recall_among_other_scores() {
        let names = |list: &[&str]| {
            list.iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            primary_recall(&names(&[
                "mrr@10",
                "evidence_recall@3",
                "evidence_recall@10"
            ])),
            "evidence_recall@10"
        );
        assert_eq!(
            primary_recall(&names(&["ndcg@10", "evidence_recall@context"])),
            "evidence_recall@context"
        );
    }
}
