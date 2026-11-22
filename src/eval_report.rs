//! Evaluation reports (spec 28.11 step 4b): `report show`, `list` and `compare`, the evaluation
//! status of a build, and the missed evidence that dictionary suggestions analyze.
//!
//! Outputs, their JSON key order and the bootstrap draws are the ones the JavaScript
//! implementation produced.

// The entry points are called from `node`, which test builds leave out.
#![cfg_attr(test, allow(dead_code))]

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;

use crate::config_value::ConfigValue;
use crate::eval_stage::{
    Document, dataset, evaluation_cases, has_page, invalid, js_string, load_document, mean,
    method_id, missing, num, open_state, parse, position_key, primary_recall,
};
use crate::index_stage::{assign, items, js_json, object, sql, string_field, text, truthy};
use crate::transform_stage::StageError;
use crate::{Cancellation, KnowledgeBase};

/// `report show <ref>`, `report list` or `report compare <a> <b>`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRequest {
    pub state: String,
    pub action: String,
    #[serde(default)]
    pub refs: Vec<String>,
    #[serde(default)]
    pub split: Option<String>,
}

/// The evaluation status of a build and search configuration.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusRequest {
    pub state: String,
    pub build_id: String,
    pub search_id: String,
    /// `eval-data.use` of the configuration.
    #[serde(default)]
    pub dataset: Option<String>,
    /// The validated `eval` settings, or `null` without an `eval` section.
    #[serde(default)]
    pub settings: ConfigValue,
}

/// The report and data that dictionary suggestions use: `--report`, or the latest development
/// report of the selected data.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryRequest {
    pub state: String,
    #[serde(default)]
    pub report: Option<String>,
    #[serde(default)]
    pub dataset: Option<String>,
    #[serde(default)]
    pub include_generated: bool,
}

/// The evidence a development report missed.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissesRequest {
    pub state: String,
    pub report: ConfigValue,
    pub data: ConfigValue,
}

fn report_required(message: impl Into<String>) -> StageError {
    StageError::coded("REPORT_REQUIRED", message, 3)
}

fn report_by_ref(connection: &Connection, reference: &str) -> Result<ConfigValue, StageError> {
    let data: Option<String> = connection
        .query_row(
            "SELECT data FROM evaluation_reports WHERE id = ?1 OR tag = ?2 LIMIT 1",
            (reference, reference),
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    parse(&data.ok_or_else(|| report_required(format!("Unknown report: {reference}")))?)
}

fn report_rows(
    connection: &Connection,
    query: &str,
    values: &[&str],
) -> Result<Vec<ConfigValue>, StageError> {
    let mut statement = connection.prepare(query).map_err(sql)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(values), |row| {
            row.get::<_, String>(0)
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    rows.iter().map(|data| parse(data)).collect()
}

/// `Number(value)` for the values reports hold: `null` is 0 and a missing value is NaN.
fn js_number_of(value: Option<&ConfigValue>) -> f64 {
    match value {
        None => f64::NAN,
        Some(ConfigValue::Null) => 0.0,
        Some(ConfigValue::Number(value)) => *value,
        Some(ConfigValue::Bool(value)) => f64::from(u8::from(*value)),
        Some(ConfigValue::NonFinite(name)) => match name.as_str() {
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            _ => f64::NAN,
        },
        Some(ConfigValue::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                0.0
            } else {
                trimmed.parse().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

/// `a === b` for the primitive values of reports, where `None` is `undefined`.
fn strictly_equal(a: Option<&ConfigValue>, b: Option<&ConfigValue>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(ConfigValue::Number(a)), Some(ConfigValue::Number(b))) => a == b,
        (Some(ConfigValue::NonFinite(_)), _) | (_, Some(ConfigValue::NonFinite(_))) => false,
        (Some(ConfigValue::Array(_) | ConfigValue::Object(_)), _) => false,
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Settings as dotted names: `{ a: { b: 1 } }` is `{ 'a.b': 1 }`, and arrays use their indexes.
fn flatten(value: Option<&ConfigValue>, prefix: &str, out: &mut Vec<(String, ConfigValue)>) {
    let entries: Vec<(String, &ConfigValue)> = match value {
        Some(ConfigValue::Object(entries)) => entries
            .iter()
            .map(|(name, child)| (name.clone(), child))
            .collect(),
        Some(ConfigValue::Array(values)) => values
            .iter()
            .enumerate()
            .map(|(index, child)| (index.to_string(), child))
            .collect(),
        _ => return,
    };
    for (key, child) in entries {
        let name = if prefix.is_empty() {
            key
        } else {
            format!("{prefix}.{key}")
        };
        if matches!(child, ConfigValue::Object(_) | ConfigValue::Array(_)) {
            let mut nested = Vec::new();
            flatten(Some(child), &name, &mut nested);
            for (nested_name, nested_value) in nested {
                assign(out, &nested_name, Some(nested_value));
            }
        } else {
            assign(out, &name, Some(child.clone()));
        }
    }
}

/// Without these fields: `({ a, b, ...rest }) => rest`.
fn without(value: &ConfigValue, names: &[&str]) -> Vec<(String, ConfigValue)> {
    match value {
        ConfigValue::Object(entries) => entries
            .iter()
            .filter(|(name, _)| !names.contains(&name.as_str()))
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

fn list(connection: &Connection, split: &str) -> Result<ConfigValue, StageError> {
    let mut ranked: Vec<ConfigValue> = report_rows(
        connection,
        "SELECT data FROM evaluation_reports WHERE json_extract(data, '$.split') = ?1 ORDER BY rowid DESC",
        &[split],
    )?
    .into_iter()
    .map(|report| {
        let cases = items(report.get("rows")).len();
        let mut entries = without(&report, &["rows"]);
        entries.push(("cases".into(), num(cases as f64)));
        ConfigValue::Object(entries)
    })
    .collect();
    let context = |report: &ConfigValue| match report
        .get("scores")
        .and_then(|scores| scores.get("evidence_recall@context"))
    {
        None | Some(ConfigValue::Null) => -1.0,
        value => js_number_of(value),
    };
    let id = |report: &ConfigValue, name: &str| {
        string_field(report, name).unwrap_or_default().to_owned()
    };
    // The identifiers are lowercase hexadecimal after a common prefix, where code point order is
    // the order localeCompare gave.
    ranked.sort_by(|a, b| {
        id(a, "dataset_id")
            .cmp(&id(b, "dataset_id"))
            .then_with(|| id(a, "method_id").cmp(&id(b, "method_id")))
            .then_with(|| {
                (context(b) - context(a))
                    .partial_cmp(&0.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let mut groups: Vec<((String, String, String), Vec<ConfigValue>)> = Vec::new();
    for report in ranked {
        let key = (
            js_json(report.get("dataset_id")).unwrap_or_default(),
            js_json(report.get("method_id")).unwrap_or_default(),
            js_json(report.get("split")).unwrap_or_default(),
        );
        match groups.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, members)) => members.push(report),
            None => groups.push((key, vec![report])),
        }
    }
    let output = groups
        .into_iter()
        .map(|(_, results)| {
            let settings: Vec<Vec<(String, ConfigValue)>> = results
                .iter()
                .map(|report| {
                    let mut combined = match report.get("construction") {
                        Some(ConfigValue::Object(entries)) => entries.clone(),
                        _ => Vec::new(),
                    };
                    assign(
                        &mut combined,
                        "search",
                        Some(report.get("search_settings").cloned().unwrap_or_default()),
                    );
                    let mut flat = Vec::new();
                    flatten(Some(&ConfigValue::Object(combined)), "", &mut flat);
                    assign(
                        &mut flat,
                        "budget_chars",
                        report.get("budget_chars").cloned(),
                    );
                    flat
                })
                .collect();
            let get = |index: usize, field: &str| {
                settings[index]
                    .iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, value)| value)
            };
            let mut fields: Vec<String> = Vec::new();
            for item in &settings {
                for (name, _) in item {
                    if !fields.contains(name) {
                        fields.push(name.clone());
                    }
                }
            }
            fields.retain(|field| {
                field == "budget_chars"
                    || (0..settings.len())
                        .any(|index| !strictly_equal(get(index, field), get(0, field)))
            });
            let first = &results[0];
            let mut group = Vec::new();
            for name in ["dataset_id", "method_id", "split"] {
                assign(&mut group, name, first.get(name).cloned());
            }
            group.push((
                "columns".into(),
                ConfigValue::Array(fields.iter().map(|field| text(field)).collect()),
            ));
            group.push((
                "results".into(),
                ConfigValue::Array(
                    results
                        .iter()
                        .enumerate()
                        .map(|(index, report)| {
                            let mut entries = without(report, &["construction", "search_settings"]);
                            entries.push((
                                "settings".into(),
                                ConfigValue::Object(
                                    fields
                                        .iter()
                                        .map(|field| {
                                            let value = match get(index, field) {
                                                None | Some(ConfigValue::Null) => ConfigValue::Null,
                                                Some(value) => value.clone(),
                                            };
                                            (field.clone(), value)
                                        })
                                        .collect(),
                                ),
                            ));
                            ConfigValue::Object(entries)
                        })
                        .collect(),
                ),
            ));
            ConfigValue::Object(group)
        })
        .collect();
    Ok(ConfigValue::Array(output))
}

/// A 95% interval of the mean difference from 1000 bootstrap draws of whole groups, with the
/// fixed seed of the JavaScript implementation. `None` below five groups.
fn confidence_interval(groups: &[Vec<f64>]) -> Option<(f64, f64)> {
    if groups.len() < 5 {
        return None;
    }
    let summaries: Vec<(f64, usize)> = groups
        .iter()
        .map(|values| {
            (
                values.iter().fold(0.0, |sum, value| sum + value),
                values.len(),
            )
        })
        .collect();
    let mut seed: u32 = 0x7254b13;
    let mut draws = Vec::with_capacity(1000);
    for _ in 0..1000 {
        let mut sum = 0.0;
        let mut count = 0;
        for _ in 0..summaries.len() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let index =
                (f64::from(seed) / 4_294_967_296.0 * summaries.len() as f64).floor() as usize;
            let (value, size) = summaries[index];
            sum += value;
            count += size;
        }
        draws.push(sum / count as f64);
    }
    draws.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some((draws[25], draws[974]))
}

fn compare(connection: &Connection, refs: &[String]) -> Result<ConfigValue, StageError> {
    let a = report_by_ref(connection, refs.first().map_or("undefined", String::as_str))?;
    let b = report_by_ref(connection, refs.get(1).map_or("undefined", String::as_str))?;
    let mut right: HashMap<String, &ConfigValue> = HashMap::new();
    for row in items(b.get("rows")) {
        right.insert(js_json(row.get("id")).unwrap_or_default(), row);
    }
    let paired: Vec<(&ConfigValue, &ConfigValue)> = items(a.get("rows"))
        .iter()
        .filter_map(|row| {
            right
                .get(&js_json(row.get("id")).unwrap_or_default())
                .map(|other| (row, *other))
        })
        .collect();
    let mut differences = Vec::new();
    for key in [
        "build_id",
        "source_id",
        "search_config_id",
        "dataset_id",
        "method_id",
        "split",
        "budget_chars",
    ] {
        if !strictly_equal(a.get(key), b.get(key)) {
            differences.push((
                key.to_owned(),
                ConfigValue::Array(vec![
                    a.get(key).cloned().unwrap_or_default(),
                    b.get(key).cloned().unwrap_or_default(),
                ]),
            ));
        }
    }
    let differs = |key: &str| differences.iter().any(|(name, _)| name == key);
    let score_names: Vec<String> = match a.get("scores") {
        Some(ConfigValue::Object(entries)) => entries
            .iter()
            .map(|(name, _)| name.clone())
            .filter(|name| {
                b.get("scores")
                    .and_then(|scores| scores.get(name))
                    .is_some()
            })
            .collect(),
        _ => Vec::new(),
    };
    let score = |row: &ConfigValue, name: &str| {
        row.get("scores")
            .and_then(|scores| scores.get(name))
            .cloned()
    };
    let scores: Vec<(String, ConfigValue)> = score_names
        .iter()
        .map(|name| {
            let deltas: Vec<f64> = paired
                .iter()
                .map(|(left, right)| {
                    js_number_of(score(right, name).as_ref())
                        - js_number_of(score(left, name).as_ref())
                })
                .collect();
            let wins = deltas.iter().filter(|value| **value > 0.0).count();
            let losses = deltas.iter().filter(|value| **value < 0.0).count();
            let ties = deltas.len() - wins - losses;
            let mut groups: Vec<(String, Vec<f64>)> = Vec::new();
            for ((left, _), delta) in paired.iter().zip(&deltas) {
                let group = js_json(left.get("group")).unwrap_or_else(|| "undefined".into());
                match groups.iter_mut().find(|(existing, _)| *existing == group) {
                    Some((_, values)) => values.push(*delta),
                    None => groups.push((group, vec![*delta])),
                }
            }
            let interval = confidence_interval(
                &groups
                    .into_iter()
                    .map(|(_, values)| values)
                    .collect::<Vec<_>>(),
            );
            (
                name.clone(),
                object(vec![
                    ("delta", mean(&deltas)),
                    ("wins", num(wins as f64)),
                    ("losses", num(losses as f64)),
                    ("ties", num(ties as f64)),
                    (
                        "interval_95",
                        interval.map_or(ConfigValue::Null, |(low, high)| {
                            ConfigValue::Array(vec![num(low), num(high)])
                        }),
                    ),
                    (
                        "uncertain",
                        interval.map_or(ConfigValue::Null, |(low, high)| {
                            ConfigValue::Bool(low <= 0.0 && high >= 0.0)
                        }),
                    ),
                ]),
            )
        })
        .collect();
    let rows_a = items(a.get("rows")).len();
    let rows_b = items(b.get("rows")).len();
    let comparable = paired.len() == rows_a
        && paired.len() == rows_b
        && !["dataset_id", "method_id", "split", "budget_chars"]
            .iter()
            .any(|key| differs(key));
    let mut comparison = Vec::new();
    assign(&mut comparison, "a", a.get("id").cloned());
    assign(&mut comparison, "b", b.get("id").cloned());
    comparison.push((
        "differences".into(),
        ConfigValue::Object(differences.clone()),
    ));
    comparison.push(("common_cases".into(), num(paired.len() as f64)));
    comparison.push(("comparable".into(), ConfigValue::Bool(comparable)));
    comparison.push((
        "provisional".into(),
        ConfigValue::Bool(differs("source_id")),
    ));
    if !comparable {
        comparison.push(("scores".into(), ConfigValue::Object(Vec::new())));
        comparison.push(("common_case_scores".into(), ConfigValue::Object(scores)));
        return Ok(ConfigValue::Object(comparison));
    }
    comparison.push(("scores".into(), ConfigValue::Object(scores)));
    if string_field(&a, "split") != Some("dev") {
        return Ok(ConfigValue::Object(comparison));
    }
    let primary = primary_recall(&score_names);
    let data = dataset(connection, string_field(&a, "dataset_id"))?;
    let mut questions: HashMap<String, ConfigValue> = HashMap::new();
    for item in items(data.get("cases")) {
        assign_question(&mut questions, item);
    }
    let cases = paired
        .iter()
        .filter_map(|(before, after)| {
            let baseline = score(before, &primary)?;
            let trial = score(after, &primary)?;
            let one =
                |value: &ConfigValue| matches!(value, ConfigValue::Number(value) if *value == 1.0);
            if one(&baseline) && one(&trial) {
                return None;
            }
            let (from, to) = (js_number_of(Some(&baseline)), js_number_of(Some(&trial)));
            let status = if to < from {
                "worse"
            } else if one(&trial) {
                "recovered"
            } else {
                "still_missing"
            };
            let mut entry = Vec::new();
            assign(&mut entry, "id", before.get("id").cloned());
            assign(
                &mut entry,
                "question",
                questions
                    .get(&js_json(before.get("id")).unwrap_or_default())
                    .cloned(),
            );
            entry.push(("before".into(), baseline));
            entry.push(("after".into(), trial));
            entry.push(("delta".into(), num(to - from)));
            entry.push(("status".into(), text(status)));
            Some(ConfigValue::Object(entry))
        })
        .collect();
    comparison.push(("case_metric".into(), text(&primary)));
    comparison.push(("cases".into(), ConfigValue::Array(cases)));
    Ok(ConfigValue::Object(comparison))
}

fn assign_question(questions: &mut HashMap<String, ConfigValue>, item: &ConfigValue) {
    let key = js_json(item.get("id")).unwrap_or_default();
    match item.get("question") {
        Some(question) => {
            questions.insert(key, question.clone());
        }
        None => {
            questions.remove(&key);
        }
    }
}

/// `report show`, `list` and `compare`.
pub fn reports(request: &ReportRequest) -> Result<ConfigValue, StageError> {
    let connection = open_state(&request.state, true)?;
    match request.action.as_str() {
        "show" => report_by_ref(
            &connection,
            request.refs.first().map_or("undefined", String::as_str),
        ),
        "list" => list(&connection, request.split.as_deref().unwrap_or("dev")),
        _ => compare(&connection, &request.refs),
    }
}

/// `{ state, report }`: whether the latest development report of the selected data and method
/// is for this build and search configuration.
pub fn evaluation_status(request: &StatusRequest) -> Result<ConfigValue, StageError> {
    let unevaluated = || {
        object(vec![
            ("state", text("unevaluated")),
            ("report", ConfigValue::Null),
        ])
    };
    let reference = request
        .dataset
        .as_deref()
        .filter(|reference| !reference.is_empty());
    let (Some(reference), true) = (reference, truthy(Some(&request.settings))) else {
        return Ok(unevaluated());
    };
    let connection = open_state(&request.state, true)?;
    let selected = match dataset(&connection, Some(reference)) {
        Ok(data) => js_string(data.get("id")),
        Err(error) if error.code == Some("EVAL_DATA_REQUIRED") => return Ok(unevaluated()),
        Err(error) => return Err(error),
    };
    let method = method_id(&request.settings);
    let reports = report_rows(
        &connection,
        "SELECT data FROM evaluation_reports WHERE json_extract(data, '$.dataset_id') = ?1 AND json_extract(data, '$.method_id') = ?2 AND json_extract(data, '$.split') = 'dev' ORDER BY rowid DESC",
        &[&selected, &method],
    )?;
    let matching = reports.iter().find(|report| {
        string_field(report, "build_id") == Some(request.build_id.as_str())
            && string_field(report, "search_config_id") == Some(request.search_id.as_str())
    });
    let Some(latest) = matching.or(reports.first()) else {
        return Ok(unevaluated());
    };
    let unresolved: usize = items(latest.get("rows"))
        .iter()
        .map(|row| items(row.get("unresolved")).len())
        .sum();
    let mut report = Vec::new();
    for name in ["id", "build_id", "search_config_id", "scores"] {
        assign(&mut report, name, latest.get(name).cloned());
    }
    report.push(("unresolved".into(), num(unresolved as f64)));
    Ok(object(vec![
        (
            "state",
            text(if matching.is_some() {
                "evaluated"
            } else {
                "stale"
            }),
        ),
        ("report", ConfigValue::Object(report)),
    ]))
}

/// `{ report, data }` for dictionary suggestions.
pub fn dictionary_evaluation(request: &DictionaryRequest) -> Result<ConfigValue, StageError> {
    let hint = "Add a development evaluation question with knowledge-nest eval-data add --question <text> --doc <path> --quote <text>.";
    let connection = open_state(&request.state, true)?;
    let (report, data) = match &request.report {
        Some(reference) => {
            let report = report_by_ref(&connection, reference)?;
            if string_field(&report, "split") != Some("dev") {
                return Err(invalid(
                    "Dictionary suggestions require a development report.",
                ));
            }
            let data = dataset(&connection, string_field(&report, "dataset_id"))?;
            (report, data)
        }
        None => {
            let selected = request
                .dataset
                .as_deref()
                .filter(|selected| !selected.is_empty())
                .ok_or_else(|| missing(hint))?;
            let data = dataset(&connection, Some(selected))?;
            if evaluation_cases(&data, "dev", request.include_generated).is_empty() {
                return Err(missing(hint));
            }
            let id = js_string(data.get("id"));
            let report = report_rows(
                &connection,
                "SELECT data FROM evaluation_reports WHERE json_extract(data, '$.dataset_id') = ?1 AND json_extract(data, '$.split') = 'dev' ORDER BY rowid DESC LIMIT 1",
                &[&id],
            )?
            .into_iter()
            .next()
            .ok_or_else(|| report_required("Run knowledge-nest eval to create a development report first."))?;
            (report, data)
        }
    };
    Ok(object(vec![("report", report), ("data", data)]))
}

/// The evidence of a development report that was not recovered, with the line around each
/// resolved quote.
pub fn dictionary_misses(
    database: &KnowledgeBase,
    request: &MissesRequest,
    cancel: &Cancellation,
) -> Result<ConfigValue, StageError> {
    let connection = open_state(&request.state, true)?;
    let map_id = js_string(request.report.get("evidence_map_id"));
    let map: Option<String> = connection
        .query_row(
            "SELECT data FROM evidence_maps WHERE id = ?1",
            [&map_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    let map = parse(&map.ok_or_else(|| {
        StageError::coded(
            "MAP_REQUIRED",
            "Run knowledge-nest eval to record the evidence positions first.",
            3,
        )
    })?)?;
    let positions = map.get("positions").cloned().unwrap_or_default();
    let mut cases: HashMap<String, &ConfigValue> = HashMap::new();
    for item in items(request.data.get("cases")) {
        cases.insert(js_json(item.get("id")).unwrap_or_default(), item);
    }
    let score_names: Vec<String> = match request.report.get("scores") {
        Some(ConfigValue::Object(entries)) => {
            entries.iter().map(|(name, _)| name.clone()).collect()
        }
        _ => Vec::new(),
    };
    let primary = primary_recall(&score_names);
    let mut documents: HashMap<(String, bool), Option<Document>> = HashMap::new();
    let mut found = Vec::new();
    for result in items(request.report.get("rows")) {
        let score = result.get("scores").and_then(|scores| scores.get(&primary));
        if matches!(score, Some(ConfigValue::Number(value)) if *value == 1.0) {
            continue;
        }
        let item = cases
            .get(&js_json(result.get("id")).unwrap_or_default())
            .copied()
            .ok_or_else(|| StageError::plain("The report has a case the evaluation data lacks."))?;
        let question = item.get("question").cloned();
        for (index, evidence) in items(item.get("evidence")).iter().enumerate() {
            cancel.check().map_err(|_| StageError::aborted())?;
            let saved = result
                .get("evidence")
                .and_then(|list| items(Some(list)).get(index));
            if saved.is_some_and(|saved| truthy(saved.get("recovered"))) {
                continue;
            }
            let mut finding = Vec::new();
            assign(&mut finding, "case_id", item.get("id").cloned());
            assign(&mut finding, "question", question.clone());
            assign(&mut finding, "doc", evidence.get("doc").cloned());
            assign(&mut finding, "quote", evidence.get("quote").cloned());
            if saved.is_none() && js_number_of(score) > 0.0 {
                finding.push(("reason".into(), text("legacy_mixed_recovery")));
                found.push(ConfigValue::Object(finding));
                break;
            }
            if let Some(reason) = saved
                .and_then(|saved| saved.get("reason"))
                .filter(|reason| truthy(Some(reason)))
            {
                finding.push(("reason".into(), reason.clone()));
                found.push(ConfigValue::Object(finding));
                continue;
            }
            let doc = string_field(evidence, "doc").unwrap_or_default();
            let with_metadata = saved.is_none() && has_page(evidence);
            let key = match saved.and_then(|saved| saved.get("position_key")) {
                Some(key) => Some(js_string(Some(key))),
                None => read(&mut documents, database, doc, with_metadata, cancel)?
                    .map(|document| position_key(evidence, document)),
            };
            let present = read(&mut documents, database, doc, false, cancel)?.is_some();
            let position = key
                .filter(|_| present)
                .and_then(|key| positions.get(&key).cloned());
            let range = position
                .as_ref()
                .and_then(|position| position.get("range"))
                .filter(|range| truthy(Some(range)));
            let Some(range) = range else {
                let reason = position
                    .as_ref()
                    .and_then(|position| position.get("reason"))
                    .cloned()
                    .unwrap_or_else(|| text("source_missing"));
                finding.push(("reason".into(), reason));
                found.push(ConfigValue::Object(finding));
                continue;
            };
            let Some(document) = read(&mut documents, database, doc, false, cancel)? else {
                continue;
            };
            let bytes = document.content.as_bytes();
            let offset = |name: &str| {
                let value = js_number_of(range.get(name));
                if value.is_nan() || value <= 0.0 {
                    0
                } else {
                    (value as usize).min(bytes.len())
                }
            };
            let (start, end) = (offset("start"), offset("end"));
            let line_start = bytes[..start]
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |index| index + 1);
            let line_end = bytes[end..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |index| end + index);
            let slice =
                |from: usize, to: usize| text(&String::from_utf8_lossy(&bytes[from..to.max(from)]));
            finding.push(("url".into(), text(&document.url)));
            assign(
                &mut finding,
                "in_results",
                saved.and_then(|saved| saved.get("in_results")).cloned(),
            );
            finding.push(("before".into(), slice(line_start, start)));
            finding.push(("text".into(), slice(start, end)));
            finding.push(("after".into(), slice(end, line_end)));
            found.push(ConfigValue::Object(finding));
        }
    }
    Ok(ConfigValue::Array(found))
}

/// A document of the build, read once per command.
fn read<'a>(
    documents: &'a mut HashMap<(String, bool), Option<Document>>,
    database: &KnowledgeBase,
    doc: &str,
    metadata: bool,
    cancel: &Cancellation,
) -> Result<Option<&'a Document>, StageError> {
    let key = (doc.to_owned(), metadata);
    if !documents.contains_key(&key) {
        let loaded = load_document(database, doc, metadata, cancel)?;
        documents.insert(key.clone(), loaded);
    }
    Ok(documents.get(&key).and_then(Option::as_ref))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_the_bootstrap_of_the_javascript_implementation() {
        let groups: Vec<Vec<f64>> = vec![
            vec![1.0],
            vec![0.0, 1.0],
            vec![-1.0],
            vec![0.5],
            vec![0.0],
            vec![1.0, 1.0],
        ];
        let (low, high) = confidence_interval(&groups).unwrap();
        assert!(low <= high);
        assert!(confidence_interval(&groups[..4]).is_none());
    }

    #[test]
    fn flattens_settings_with_dotted_names() {
        let value = object(vec![
            (
                "transform",
                ConfigValue::Array(vec![object(vec![("use", text("chunk"))])]),
            ),
            ("empty", ConfigValue::Object(Vec::new())),
            ("none", ConfigValue::Null),
        ]);
        let mut flat = Vec::new();
        flatten(Some(&value), "", &mut flat);
        assert_eq!(
            ConfigValue::Object(flat).to_js_json(),
            r#"{"transform.0.use":"chunk","none":null}"#
        );
    }
}
