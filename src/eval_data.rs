//! Evaluation data (spec 9 and 28.11 step 4e): `eval-data list`, `tag`, `show`, `diff`, `use`,
//! `export`, `split`, `review`, `sync`, `add` and `import`, and `generate` (see `eval_generate`).
//!
//! Cases, their IDs, the dataset IDs and the saved JSON are the ones the JavaScript
//! implementation produced. JavaScript reads import files, writes exported files and writes the
//! selected dataset into the configuration file, which this module asks for with `select`.

// The entry points are called from `node`, which test builds leave out.
#![cfg_attr(test, allow(dead_code))]

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;

use crate::config_value::ConfigValue;
use crate::eval_stage::{dataset, invalid, js_string, missing, num, open_state, parse};
use crate::evidence_match::position;
use crate::index_stage::{assign, digest, items, object, sql, string_field, text, truthy};
use crate::registry::js_space;
use crate::source_load::sha256_hex;
use crate::source_stage::iso_now;
use crate::transform_stage::StageError;
use crate::{Cancellation, DocumentRef, KnowledgeBase, SourceLocation};

/// What a question asks with: the words of its quotes, other words, or a code, number or name
/// written in the document. A case may have no kind.
pub(crate) const KINDS: [&str; 3] = ["lexical", "paraphrase", "identifier"];

const STATUSES: [&str; 6] = [
    "generated",
    "provided",
    "reviewed",
    "needs_review",
    "inactive",
    "rejected",
];

/// An `eval-data` command.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalDataRequest {
    pub state: String,
    pub action: String,
    #[serde(default)]
    pub positional: Option<String>,
    #[serde(default)]
    pub other: Option<String>,
    /// `eval-data.use` of the configuration.
    #[serde(default)]
    pub selected: Option<String>,
    /// The text of the file `import` reads.
    #[serde(default)]
    pub input: Option<String>,
    /// The command options, as `parseArgs` gave them.
    #[serde(default)]
    pub values: ConfigValue,
    /// The chat model of `generate`.
    #[serde(default)]
    pub generator: Option<crate::eval_generate::GeneratorChoice>,
}

/// What a command returns, and the dataset to select in the configuration, if any.
pub struct EvalDataOutcome {
    pub output: ConfigValue,
    pub select: Option<String>,
}

/// The command needs the documents of the active build, which the caller opens and passes.
pub const BUILD_NEEDED: &str = "BUILD_NEEDED";

/// A document of the active build, as cases are checked against it.
pub(crate) struct Document {
    source_id: String,
    object_id: String,
    pub(crate) version: String,
    pub(crate) content: String,
    original_hash: Option<String>,
    locations: Vec<SourceLocation>,
}

impl Document {
    pub(crate) fn name(&self) -> String {
        format!("{}:{}", self.source_id, self.object_id)
    }
}

pub(crate) fn js_trim(value: &str) -> &str {
    value.trim_matches(js_space)
}

/// `Number(text)` for option values.
pub(crate) fn js_to_number(value: &str) -> f64 {
    let trimmed = js_trim(value);
    if trimmed.is_empty() {
        return 0.0;
    }
    match trimmed {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(digits) = trimmed.strip_prefix(prefix) {
            return u64::from_str_radix(digits, radix).map_or(f64::NAN, |value| value as f64);
        }
    }
    let decimal = regex::Regex::new(r"^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$").expect("pattern");
    if decimal.is_match(trimmed) {
        trimmed.parse().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}

fn values_string<'a>(values: &'a ConfigValue, name: &str) -> Option<&'a str> {
    string_field(values, name)
}

/// Saves cases as a dataset whose ID is the digest of the cases: `{ id, cases }`.
pub(crate) fn save_dataset(
    connection: &Connection,
    cases: Vec<ConfigValue>,
) -> Result<ConfigValue, StageError> {
    let cases = ConfigValue::Array(cases);
    let id = digest("ed", &cases);
    let output = object(vec![("id", text(&id)), ("cases", cases)]);
    connection
        .execute(
            "INSERT OR IGNORE INTO evaluation_datasets VALUES (?1, ?2)",
            (&id, output.to_js_json()),
        )
        .map_err(sql)?;
    Ok(output)
}

fn id_of(value: &ConfigValue) -> String {
    js_string(value.get("id"))
}

fn cases_of(data: &ConfigValue) -> Vec<ConfigValue> {
    items(data.get("cases")).to_vec()
}

/// `{ ...value, [name]: entry }`; `None` leaves the name out, as `undefined` does in JSON.
fn with(value: &ConfigValue, changes: Vec<(&str, Option<ConfigValue>)>) -> ConfigValue {
    let mut entries = match value {
        ConfigValue::Object(entries) => entries.clone(),
        _ => Vec::new(),
    };
    for (name, entry) in changes {
        assign(&mut entries, name, entry);
    }
    ConfigValue::Object(entries)
}

/// The current documents of the build with their corpus metadata.
fn documents(database: &KnowledgeBase, cancel: &Cancellation) -> Result<Vec<Document>, StageError> {
    let plain = |error: crate::Error| StageError::plain(error.to_string());
    let snapshot = database.snapshot_documents(cancel).map_err(plain)?;
    let mut documents = Vec::with_capacity(snapshot.documents.len());
    for found in snapshot.documents {
        cancel.check().map_err(|_| StageError::aborted())?;
        let source = found.evidence.source;
        let reference = DocumentRef {
            source_id: source.source_id.clone(),
            object_id: source.object_id.clone(),
        };
        let corpus = database
            .get_corpus_document(&reference, &source.version, cancel)
            .map_err(plain)?;
        let (original_hash, locations) = match corpus {
            Some(corpus) => (corpus.metadata.original_sha256, corpus.metadata.locations),
            None => (None, Vec::new()),
        };
        documents.push(Document {
            source_id: source.source_id,
            object_id: source.object_id,
            version: source.version,
            content: found.content,
            original_hash,
            locations,
        });
    }
    Ok(documents)
}

pub(crate) fn find_document<'a>(
    documents: &'a [Document],
    name: &str,
) -> Result<&'a Document, StageError> {
    let matched: Vec<&Document> = documents
        .iter()
        .filter(|document| name == document.object_id || name == document.name())
        .collect();
    if matched.len() != 1 {
        return Err(invalid(format!(
            "Expected one document for {name}. Use source-id:relative/path."
        )));
    }
    Ok(matched[0])
}

/// Checks a case against the documents and gives it its stored form (spec 9).
pub(crate) fn make_case(
    input: &ConfigValue,
    documents: &[Document],
    prior: &[ConfigValue],
) -> Result<ConfigValue, StageError> {
    let question = match input.get("question") {
        Some(ConfigValue::String(question)) if !js_trim(question).is_empty() => question.clone(),
        _ => return Err(invalid("A question is required.")),
    };
    let evidence = match input.get("evidence") {
        None | Some(ConfigValue::Null) => {
            let mut item = Vec::new();
            for name in ["doc", "quote", "prefix", "suffix", "page", "heading"] {
                assign(&mut item, name, input.get(name).cloned());
            }
            vec![ConfigValue::Object(item)]
        }
        Some(ConfigValue::Array(items)) => items.clone(),
        Some(_) => return Err(invalid("A case requires at least one evidence quote.")),
    };
    if evidence.is_empty() {
        return Err(invalid("A case requires at least one evidence quote."));
    }
    let state_name = string_field(input, "state");
    let qualified = regex::Regex::new(r"(?i)^[a-z\d_.-]+:.+").expect("pattern");
    let mut references = Vec::with_capacity(evidence.len());
    for item in &evidence {
        let get = |name: &str| item.get(name);
        let text_or_absent = |name: &str| matches!(get(name), None | Some(ConfigValue::String(_)));
        let page_ok = match get("page") {
            None => true,
            Some(ConfigValue::Number(page)) => {
                page.fract() == 0.0 && page.abs() <= 9_007_199_254_740_991.0 && *page >= 1.0
            }
            Some(_) => false,
        };
        let (doc, quote) = match (get("doc"), get("quote")) {
            (Some(ConfigValue::String(doc)), Some(ConfigValue::String(quote)))
                if !js_trim(quote).is_empty() =>
            {
                (doc.clone(), quote.clone())
            }
            _ => {
                return Err(invalid(
                    "Expected a document and quote with valid location hints.",
                ));
            }
        };
        if !["prefix", "suffix", "heading"]
            .iter()
            .all(|name| text_or_absent(name))
            || !page_ok
        {
            return Err(invalid(
                "Expected a document and quote with valid location hints.",
            ));
        }
        let historical = matches!(state_name, Some("inactive" | "needs_review" | "rejected"))
            && qualified.is_match(&doc)
            && matches!(get("version"), Some(ConfigValue::String(_)));
        let mut reference = vec![
            ("doc".to_owned(), text(&doc)),
            ("quote".to_owned(), text(&quote)),
        ];
        for name in ["prefix", "suffix", "page", "heading"] {
            if truthy(get(name)) {
                reference.push((name.to_owned(), get(name).cloned().unwrap_or_default()));
            }
        }
        let historical_reference = |reference: &Vec<(String, ConfigValue)>| {
            let mut kept = reference.clone();
            assign(&mut kept, "version", get("version").cloned());
            assign(&mut kept, "original_hash", get("original_hash").cloned());
            ConfigValue::Object(kept)
        };
        let document = match find_document(documents, &doc) {
            Ok(document) => document,
            Err(error) => {
                if historical {
                    references.push(historical_reference(&reference));
                    continue;
                }
                return Err(error);
            }
        };
        let found = position(
            &document.content,
            &ConfigValue::Object(reference.clone()),
            &document.locations,
        );
        if found.get("range").is_none() {
            if historical {
                references.push(historical_reference(&reference));
                continue;
            }
            let candidates = items(found.get("candidates"));
            let detail = if candidates.is_empty() {
                String::new()
            } else {
                format!(": {}", ConfigValue::Array(candidates.to_vec()).to_js_json())
            };
            return Err(invalid(format!(
                "Quote in {doc} is {}{detail}",
                js_string(found.get("reason"))
            )));
        }
        assign(&mut reference, "doc", Some(text(&document.name())));
        reference.push(("version".into(), text(&document.version)));
        assign(
            &mut reference,
            "original_hash",
            document
                .original_hash
                .as_deref()
                .map(text)
                .or_else(|| get("original_hash").cloned()),
        );
        references.push(ConfigValue::Object(reference));
    }
    let group = match input.get("group") {
        None | Some(ConfigValue::Null) => {
            let mut docs: Vec<String> = references
                .iter()
                .map(|reference| js_string(reference.get("doc")))
                .collect();
            docs.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            docs.first().map(|doc| text(doc)).unwrap_or_default()
        }
        Some(group) => group.clone(),
    };
    let split = prior
        .iter()
        .find(|item| {
            item.get("group")
                .is_some_and(|existing| existing.to_js_json() == group.to_js_json())
        })
        .and_then(|item| {
            item.get("split")
                .filter(|split| !matches!(split, ConfigValue::Null))
        })
        .or_else(|| {
            input
                .get("split")
                .filter(|split| !matches!(split, ConfigValue::Null))
        })
        .cloned()
        .unwrap_or_else(|| text("dev"));
    let state = input
        .get("state")
        .filter(|state| !matches!(state, ConfigValue::Null))
        .cloned()
        .unwrap_or_else(|| text("provided"));
    let group_ok = matches!(&group, ConfigValue::String(group) if !group.is_empty());
    let split_ok = matches!(split.as_str(), Some("dev" | "test"));
    let state_ok = state
        .as_str()
        .is_some_and(|state| STATUSES.contains(&state));
    if !group_ok || !split_ok || !state_ok {
        return Err(invalid("Invalid case group, split, or state."));
    }
    match input.get("kind") {
        None | Some(ConfigValue::Null) => {}
        Some(ConfigValue::String(kind)) if KINDS.contains(&kind.as_str()) => {}
        Some(_) => return Err(invalid("Invalid case kind.")),
    }
    let id = match input.get("id") {
        None => {
            let keyed: Vec<ConfigValue> = references
                .iter()
                .map(|reference| match reference {
                    ConfigValue::Object(entries) => ConfigValue::Object(
                        entries
                            .iter()
                            .filter(|(name, _)| name != "version" && name != "original_hash")
                            .cloned()
                            .collect(),
                    ),
                    other => other.clone(),
                })
                .collect();
            text(&digest(
                "case",
                &ConfigValue::Array(vec![text(&question), ConfigValue::Array(keyed)]),
            ))
        }
        Some(ConfigValue::String(id))
            if regex::Regex::new(r"^case-[a-f0-9]{64}$")
                .expect("pattern")
                .is_match(id) =>
        {
            text(id)
        }
        Some(_) => return Err(invalid("Invalid case ID.")),
    };
    let mut case = vec![
        ("id".to_owned(), id),
        ("question".to_owned(), text(&question)),
        ("evidence".to_owned(), ConfigValue::Array(references)),
        ("group".to_owned(), group),
        ("split".to_owned(), split),
        ("state".to_owned(), state),
        (
            "input_by".to_owned(),
            input
                .get("input_by")
                .filter(|value| !matches!(value, ConfigValue::Null))
                .cloned()
                .unwrap_or_else(|| text("provided")),
        ),
        (
            "created_at".to_owned(),
            input
                .get("created_at")
                .filter(|value| !matches!(value, ConfigValue::Null))
                .cloned()
                .unwrap_or_else(|| text(&iso_now())),
        ),
    ];
    for name in ["reviewed_by", "generation", "reason", "kind"] {
        if truthy(input.get(name)) {
            case.push((
                name.to_owned(),
                input.get(name).cloned().unwrap_or_default(),
            ));
        }
    }
    Ok(ConfigValue::Object(case))
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

fn done(output: ConfigValue) -> Result<EvalDataOutcome, StageError> {
    Ok(EvalDataOutcome {
        output,
        select: None,
    })
}

fn selecting(output: ConfigValue, id: String) -> Result<EvalDataOutcome, StageError> {
    Ok(EvalDataOutcome {
        output,
        select: Some(id),
    })
}

fn need_build() -> StageError {
    StageError::coded(BUILD_NEEDED, "Open the active build for this command.", 1)
}

/// Runs an `eval-data` command. Without `database`, a command that reads documents fails with
/// `BUILD_NEEDED` once its arguments are checked.
pub fn eval_data(
    database: Option<&KnowledgeBase>,
    request: &EvalDataRequest,
    cancel: &Cancellation,
) -> Result<EvalDataOutcome, StageError> {
    let read_only = ["show", "list", "export", "diff"].contains(&request.action.as_str())
        || (request.action == "review"
            && request.values.get("case").is_none()
            && request.values.get("state").is_none());
    let connection = open_state(&request.state, read_only)?;
    let values = &request.values;
    let positional = request.positional.as_deref();
    let selected = request.selected.as_deref();
    match request.action.as_str() {
        "list" => {
            let latest: Option<String> = connection
                .query_row(
                    "SELECT data FROM evaluation_datasets ORDER BY rowid DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql)?;
            let tagged: Vec<(String, String)> =
                if table_exists(&connection, "evaluation_dataset_tags")? {
                    let mut statement = connection
                        .prepare(
                            "SELECT name, dataset_id FROM evaluation_dataset_tags ORDER BY name",
                        )
                        .map_err(sql)?;
                    statement
                        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                        .map_err(sql)?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(sql)?
                } else {
                    Vec::new()
                };
            let mut refs: Vec<String> = Vec::new();
            for (_, id) in &tagged {
                if !refs.contains(id) {
                    refs.push(id.clone());
                }
            }
            if let Some(latest) = latest {
                let id = id_of(&parse(&latest)?);
                if !refs.contains(&id) {
                    refs.push(id);
                }
            }
            let datasets = refs
                .iter()
                .map(|id| {
                    let data = dataset(&connection, Some(id))?;
                    Ok(object(vec![
                        ("id", text(id)),
                        ("cases", num(cases_of(&data).len() as f64)),
                        (
                            "tags",
                            ConfigValue::Array(
                                tagged
                                    .iter()
                                    .filter(|(_, dataset_id)| dataset_id == id)
                                    .map(|(name, _)| text(name))
                                    .collect(),
                            ),
                        ),
                    ]))
                })
                .collect::<Result<Vec<_>, StageError>>()?;
            done(object(vec![
                ("selected", selected.map_or(ConfigValue::Null, text)),
                ("datasets", ConfigValue::Array(datasets)),
            ]))
        }
        "tag" => {
            let name = positional.unwrap_or_default();
            let pattern = regex::Regex::new(r"^[\p{L}\p{N}_.-]{1,100}$").expect("pattern");
            if !pattern.is_match(name) {
                return Err(invalid("Invalid evaluation dataset tag."));
            }
            let id = id_of(&dataset(&connection, request.other.as_deref())?);
            let prior: Option<String> = connection
                .query_row(
                    "SELECT dataset_id FROM evaluation_dataset_tags WHERE name = ?1",
                    [name],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql)?;
            if prior.is_some_and(|prior| prior != id) {
                return Err(invalid("The dataset tag is already assigned."));
            }
            connection
                .execute(
                    "INSERT OR IGNORE INTO evaluation_dataset_tags VALUES (?1, ?2)",
                    (name, &id),
                )
                .map_err(sql)?;
            done(object(vec![("tag", text(name)), ("id", text(&id))]))
        }
        "show" | "export" => done(dataset(&connection, positional)?),
        "diff" => {
            let left = dataset(&connection, positional)?;
            let right = dataset(&connection, request.other.as_deref())?;
            let keyed = |data: &ConfigValue| {
                let mut map: Vec<(String, ConfigValue)> = Vec::new();
                for item in items(data.get("cases")) {
                    assign(&mut map, &id_of(item), Some(item.clone()));
                }
                map
            };
            let (before, after) = (keyed(&left), keyed(&right));
            let lookup = |map: &[(String, ConfigValue)], id: &str| {
                map.iter()
                    .find(|(key, _)| key == id)
                    .map(|(_, value)| value.clone())
            };
            let added = cases_of(&right)
                .into_iter()
                .filter(|item| lookup(&before, &id_of(item)).is_none())
                .collect();
            let removed = cases_of(&left)
                .into_iter()
                .filter(|item| lookup(&after, &id_of(item)).is_none())
                .collect();
            let changed = cases_of(&right)
                .into_iter()
                .filter_map(|item| {
                    let id = id_of(&item);
                    let old = lookup(&before, &id)?;
                    let new = lookup(&after, &id)?;
                    (old.to_js_json() != new.to_js_json())
                        .then(|| object(vec![("before", old), ("after", item)]))
                })
                .collect();
            done(object(vec![
                ("from", left.get("id").cloned().unwrap_or_default()),
                ("to", right.get("id").cloned().unwrap_or_default()),
                ("added", ConfigValue::Array(added)),
                ("removed", ConfigValue::Array(removed)),
                ("changed", ConfigValue::Array(changed)),
            ]))
        }
        "use" => {
            let id = id_of(&dataset(&connection, positional)?);
            selecting(object(vec![("id", text(&id))]), id)
        }
        "split" => {
            let cases = cases_of(&dataset(&connection, selected)?);
            let ratio_text = values_string(values, "test-ratio");
            let ratio = ratio_text.map_or(f64::NAN, js_to_number);
            let seed = values_string(values, "seed").unwrap_or_default();
            let seed_ok = regex::Regex::new(r"^-?\d+$")
                .expect("pattern")
                .is_match(seed);
            if ratio_text.is_none()
                || !ratio.is_finite()
                || !(0.0..=1.0).contains(&ratio)
                || !seed_ok
            {
                return Err(invalid("Set --test-ratio (0 to 1) and an integer --seed."));
            }
            let mut groups: Vec<String> = Vec::new();
            for item in &cases {
                let group = js_string(item.get("group"));
                if !groups.contains(&group) {
                    groups.push(group);
                }
            }
            groups.sort_by_cached_key(|group| sha256_hex(format!("{seed}:{group}").as_bytes()));
            let count = (groups.len() as f64 * ratio).round() as usize;
            let test: HashSet<&String> = groups.iter().take(count).collect();
            let split = cases
                .iter()
                .map(|item| {
                    let side = if test.contains(&js_string(item.get("group"))) {
                        "test"
                    } else {
                        "dev"
                    };
                    with(item, vec![("split", Some(text(side)))])
                })
                .collect();
            let result = save_dataset(&connection, split)?;
            let id = id_of(&result);
            selecting(
                object(vec![
                    ("id", text(&id)),
                    ("cases", num(cases_of(&result).len() as f64)),
                ]),
                id,
            )
        }
        "review" => review(database, &connection, request, cancel),
        "sync" | "add" | "import" | "generate" => {
            let database = database.ok_or_else(need_build)?;
            let documents = documents(database, cancel)?;
            match request.action.as_str() {
                "sync" => sync(&connection, positional, &documents),
                "generate" => {
                    crate::eval_generate::generate_cases(&connection, request, &documents, cancel)
                }
                _ => add(&connection, request, &documents),
            }
        }
        other => Err(invalid(format!("Unknown eval-data action: {other}"))),
    }
}

fn review(
    database: Option<&KnowledgeBase>,
    connection: &Connection,
    request: &EvalDataRequest,
    cancel: &Cancellation,
) -> Result<EvalDataOutcome, StageError> {
    let values = &request.values;
    let data = dataset(connection, request.positional.as_deref())?;
    let id = id_of(&data);
    let cases = cases_of(&data);
    if values.get("case").is_none() && values.get("state").is_none() {
        let pending = cases
            .iter()
            .filter(|item| {
                matches!(
                    string_field(item, "state"),
                    Some("generated" | "needs_review")
                )
            })
            .cloned()
            .collect();
        return done(object(vec![
            ("id", text(&id)),
            ("cases", ConfigValue::Array(pending)),
        ]));
    }
    let mut ids: Vec<String> = Vec::new();
    for item in items(values.get("case")) {
        let item = js_string(Some(item));
        if !ids.contains(&item) {
            ids.push(item);
        }
    }
    let state = values_string(values, "state");
    let usable = !ids.is_empty()
        && matches!(state, Some("reviewed" | "rejected"))
        && values_string(values, "reviewer").is_some_and(|reviewer| !js_trim(reviewer).is_empty())
        && matches!(
            values_string(values, "reviewer-type"),
            Some("human" | "agent")
        );
    if !usable {
        return Err(invalid(
            "Set --case <id> --state reviewed|rejected --reviewer <name> --reviewer-type human|agent. Repeat --case to review several cases.",
        ));
    }
    let existing = ids
        .iter()
        .map(|wanted| {
            cases
                .iter()
                .find(|item| id_of(item) == *wanted)
                .cloned()
                .ok_or_else(|| missing(format!("Unknown case: {wanted}")))
        })
        .collect::<Result<Vec<_>, StageError>>()?;
    let documents = if state == Some("reviewed") {
        let database = database.ok_or_else(need_build)?;
        Some(documents(database, cancel)?)
    } else {
        None
    };
    let at = iso_now();
    let reviewer = object(vec![
        (
            "type",
            values.get("reviewer-type").cloned().unwrap_or_default(),
        ),
        ("id", values.get("reviewer").cloned().unwrap_or_default()),
        ("at", text(&at)),
    ]);
    let mut reviewed: Vec<(String, ConfigValue)> = Vec::new();
    for item in &existing {
        let evidence = match &documents {
            Some(documents) => {
                let checked = make_case(
                    &with(item, vec![("state", Some(text("reviewed")))]),
                    documents,
                    &cases,
                )?;
                checked.get("evidence").cloned()
            }
            None => item.get("evidence").cloned(),
        };
        reviewed.push((
            id_of(item),
            with(
                item,
                vec![
                    ("evidence", evidence),
                    ("state", state.map(text)),
                    ("reason", None),
                    ("reviewed_by", Some(reviewer.clone())),
                ],
            ),
        ));
    }
    let updated = save_dataset(
        connection,
        cases
            .iter()
            .map(|item| {
                let item_id = id_of(item);
                reviewed
                    .iter()
                    .find(|(id, _)| *id == item_id)
                    .map_or_else(|| item.clone(), |(_, value)| value.clone())
            })
            .collect(),
    )?;
    let updated_id = id_of(&updated);
    selecting(
        object(vec![
            ("id", text(&updated_id)),
            ("case", reviewed[0].1.clone()),
            (
                "cases",
                ConfigValue::Array(reviewed.into_iter().map(|(_, value)| value).collect()),
            ),
        ]),
        updated_id,
    )
}

fn sync(
    connection: &Connection,
    positional: Option<&str>,
    documents: &[Document],
) -> Result<EvalDataOutcome, StageError> {
    let data = dataset(connection, positional)?;
    let id = id_of(&data);
    let cases = cases_of(&data);
    let mut current: Vec<(String, &Document)> = Vec::new();
    for document in documents {
        let name = document.name();
        match current.iter_mut().find(|(existing, _)| *existing == name) {
            Some(entry) => entry.1 = document,
            None => current.push((name, document)),
        }
    }
    let get = |doc: &str| {
        current
            .iter()
            .find(|(name, _)| name == doc)
            .map(|(_, document)| *document)
    };
    let referenced: HashSet<String> = cases
        .iter()
        .flat_map(|item| {
            items(item.get("evidence"))
                .iter()
                .map(|reference| js_string(reference.get("doc")))
        })
        .collect();
    let uncovered: Vec<ConfigValue> = current
        .iter()
        .filter(|(name, _)| !referenced.contains(name))
        .map(|(name, _)| text(name))
        .collect();
    let found = |document: &Document, reference: &ConfigValue| {
        position(&document.content, reference, &document.locations)
            .get("range")
            .is_some()
    };
    let updated: Vec<ConfigValue> = cases
        .iter()
        .map(|item| {
            if string_field(item, "state") == Some("rejected") {
                return item.clone();
            }
            let references = items(item.get("evidence"));
            let evidence: Vec<ConfigValue> = references
                .iter()
                .map(|reference| match get(&js_string(reference.get("doc"))) {
                    Some(document) if found(document, reference) => with(
                        reference,
                        vec![
                            ("version", Some(text(&document.version))),
                            ("original_hash", document.original_hash.as_deref().map(text)),
                        ],
                    ),
                    _ => reference.clone(),
                })
                .collect();
            if evidence
                .iter()
                .all(|reference| get(&js_string(reference.get("doc"))).is_none())
            {
                return with(
                    item,
                    vec![
                        ("state", Some(text("inactive"))),
                        ("reason", Some(text("source_missing"))),
                    ],
                );
            }
            let changed =
                references
                    .iter()
                    .any(|reference| match get(&js_string(reference.get("doc"))) {
                        None => true,
                        Some(document) => {
                            string_field(reference, "version") != Some(document.version.as_str())
                                && (!truthy(reference.get("original_hash"))
                                    || string_field(reference, "original_hash")
                                        != document.original_hash.as_deref())
                                && !found(document, reference)
                        }
                    });
            if changed || string_field(item, "state") == Some("inactive") {
                return with(
                    item,
                    vec![
                        ("evidence", Some(ConfigValue::Array(evidence))),
                        ("state", Some(text("needs_review"))),
                        ("reason", Some(text("source_changed"))),
                    ],
                );
            }
            with(item, vec![("evidence", Some(ConfigValue::Array(evidence)))])
        })
        .collect();
    let count = |state: &str| {
        updated
            .iter()
            .filter(|item| string_field(item, "state") == Some(state))
            .count() as f64
    };
    let needs_review = count("needs_review");
    let inactive = count("inactive");
    let total = updated.len() as f64;
    let result = save_dataset(connection, updated)?;
    done(object(vec![
        ("id", result.get("id").cloned().unwrap_or_default()),
        ("previous", text(&id)),
        ("cases", num(total)),
        ("uncovered_documents", ConfigValue::Array(uncovered)),
        ("needs_review", num(needs_review)),
        ("inactive", num(inactive)),
    ]))
}

fn add(
    connection: &Connection,
    request: &EvalDataRequest,
    documents: &[Document],
) -> Result<EvalDataOutcome, StageError> {
    let mut values = request.values.clone();
    if request.action == "add" && truthy(values.get("from-search")) {
        values = from_search(values, documents)?;
    }
    let prior = match request.selected.as_deref() {
        Some(selected) if request.action == "add" && !selected.is_empty() => {
            cases_of(&dataset(connection, Some(selected))?)
        }
        _ => Vec::new(),
    };
    let input: Vec<ConfigValue> = if request.action == "add" {
        let page = values
            .get("page")
            .map(|page| num(js_to_number(&js_string(Some(page)))));
        vec![with(&values, vec![("page", page)])]
    } else {
        let text_input = request.input.as_deref().unwrap_or_default();
        let lines: Vec<&str> = regex::Regex::new(r"\r?\n")
            .expect("pattern")
            .split(text_input)
            .collect();
        let mut parsed = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if js_trim(line).is_empty() {
                continue;
            }
            parsed.push(
                serde_json::from_str::<ConfigValue>(line)
                    .map_err(|_| invalid(format!("Invalid JSON at line {}", index + 1)))?,
            );
        }
        parsed
    };
    let mut cases = prior;
    for item in &input {
        let entry = make_case(item, documents, &cases)?;
        let id = id_of(&entry);
        if cases.iter().any(|existing| id_of(existing) == id) {
            return Err(invalid(format!("Duplicate case: {id}")));
        }
        cases.push(entry);
    }
    if cases.is_empty() {
        return Err(missing("The evaluation dataset is empty."));
    }
    let result = save_dataset(connection, cases)?;
    let id = id_of(&result);
    selecting(
        object(vec![
            ("id", text(&id)),
            ("cases", num(cases_of(&result).len() as f64)),
        ]),
        id,
    )
}

/// `add --from-search <file> --hit <n>`: the quote is the text of a saved search hit.
fn from_search(values: ConfigValue, documents: &[Document]) -> Result<ConfigValue, StageError> {
    let unreadable = || invalid("Provide JSON saved from nest search --json.");
    let path = js_string(values.get("from-search"));
    let record: ConfigValue = std::fs::read_to_string(&path)
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok())
        .ok_or_else(unreadable)?;
    let hit = values
        .get("hit")
        .map_or(f64::NAN, |hit| js_to_number(&js_string(Some(hit))));
    let data = record.get("data");
    let results = data.and_then(|data| {
        data.get("results")
            .filter(|results| !matches!(results, ConfigValue::Null))
            .or_else(|| data.get("hits"))
    });
    let evidence =
        (string_field(&record, "command") == Some("search") && hit.fract() == 0.0 && hit >= 1.0)
            .then(|| items(results).get(hit as usize - 1))
            .flatten()
            .and_then(|result| result.get("evidence"));
    let usable = |evidence: &&ConfigValue| {
        truthy(evidence.get("byteRange")) && truthy(evidence.get("source"))
    };
    let safe = hit.fract() == 0.0 && hit.abs() <= 9_007_199_254_740_991.0 && hit >= 1.0;
    let Some(evidence) = evidence.filter(usable).filter(|_| safe) else {
        return Err(invalid("Set --hit to the one-based search hit number."));
    };
    let source = evidence.get("source").cloned().unwrap_or_default();
    let doc = format!(
        "{}:{}",
        js_string(source.get("sourceId")),
        js_string(source.get("objectId"))
    );
    let document = documents
        .iter()
        .rev()
        .find(|document| document.name() == doc)
        .filter(|document| {
            source.get("version").and_then(ConfigValue::as_str) == Some(document.version.as_str())
        })
        .ok_or_else(|| invalid("The search hit no longer matches the active document version."))?;
    let range = evidence.get("byteRange").cloned().unwrap_or_default();
    let bound = |name: &str| match range.get(name) {
        Some(ConfigValue::Number(value))
            if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0 =>
        {
            Some(*value)
        }
        _ => None,
    };
    let bytes = document.content.as_bytes();
    let continuation = |offset: f64| {
        bytes
            .get(offset as usize)
            .is_some_and(|byte| byte & 0xc0 == 0x80)
    };
    let (Some(start), Some(end)) = (bound("start"), bound("end")) else {
        return Err(invalid("Invalid source byte range in the search hit."));
    };
    if start < 0.0
        || end <= start
        || end > bytes.len() as f64
        || continuation(start)
        || continuation(end)
    {
        return Err(invalid("Invalid source byte range in the search hit."));
    }
    let quote = String::from_utf8_lossy(&bytes[start as usize..end as usize]).into_owned();
    Ok(with(
        &values,
        vec![
            ("doc", Some(text(&doc))),
            ("quote", Some(text(js_trim(&quote)))),
        ],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numbers_like_javascript() {
        assert_eq!(js_to_number(" 3 "), 3.0);
        assert_eq!(js_to_number(""), 0.0);
        assert_eq!(js_to_number("0x10"), 16.0);
        assert!(js_to_number("abc").is_nan());
        assert!(js_to_number("inf").is_nan());
        assert_eq!(js_to_number("1e2"), 100.0);
    }
}
