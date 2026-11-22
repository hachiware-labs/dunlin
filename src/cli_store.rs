//! The versioned store from the command line (docs/data-management.md).
//!
//! `list`, `release`, `optimize` and `results` are the store's own commands. `build`, `eval`,
//! `search` and `eval-data` are also commands of the store before it: they go to the versioned
//! store when the configuration file is written for it, and stay as they were otherwise. The
//! store of a project is `.knowledge-nest/store`.

use std::path::Path;

use serde_json::{Value, json};

use crate::assess::{Case, eval_data, report, save_eval_data};
use crate::cli_args::Parsed;
use crate::cli_project::CliError;
use crate::cli_tools::CliEnv;
use crate::extract::ExtractOutcome;
use crate::index::search;
use crate::optimize::{Event, OptimizeRequest, State, latest, optimize, table};
use crate::pipeline::{LATEST, RunRequest, run};
use crate::settings::{Settings, staged};
use crate::store::{SnapshotId, Store, StoreError};

/// The configuration file a command reads when none is given.
const CONFIG: &str = "KnowledgeNestfile.yaml";

/// The options of a command that only the store before this one takes.
fn earlier(command: &str) -> &'static [&'static str] {
    match command {
        "build" => &[
            "from",
            "until",
            "force",
            "input",
            "dry-run",
            "skip-unsupported",
            "tag",
        ],
        "eval" => &["build", "split", "tag"],
        "search" => &["budget-chars", "source-id"],
        _ => &[],
    }
}

/// What `list` shows: the word of the command line, the kind of snapshot and the heading.
const KINDS: [(&str, &str, &str); 5] = [
    ("documents", "extracted", "Documents"),
    ("chunks", "chunks", "Chunks"),
    ("indexes", "index", "Indexes"),
    ("eval-data", "eval-data", "Eval data"),
    ("results", "eval-report", "Results"),
];

/// The name an index is served under when none is given.
const SERVED: &str = "release";

/// The names of a change that are shown one by one.
const SHOWN: usize = 10;

fn failed(error: StoreError) -> CliError {
    CliError::new(1, "STORE_FAILED", error.to_string())
}

fn invalid(message: impl Into<String>) -> CliError {
    CliError::new(2, "INVALID_ARGUMENT", message)
}

fn read(env: &CliEnv, file: &str) -> Result<String, CliError> {
    std::fs::read_to_string(env.resolve(file))
        .map_err(|error| invalid(format!("{file} cannot be read: {error}")))
}

/// Milliseconds since 1970 as a day and a minute, in UTC.
fn moment(ms: u64) -> String {
    let minutes = ms / 60_000;
    let days = (minutes / 1440) as i64;
    // Days to a civil date (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}Z",
        (minutes % 1440) / 60,
        minutes % 60
    )
}

/// Rows as lines, each column as wide as its widest cell. The last column is not padded.
fn aligned(rows: &[Vec<String>]) -> Vec<String> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| cell.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (column, cell) in row.iter().enumerate() {
                if column + 1 == row.len() {
                    line.push_str(cell);
                } else {
                    let width = widths[column];
                    line.push_str(&format!("{cell:<width$}  "));
                }
            }
            line.trim_end().to_owned()
        })
        .collect()
}

/// A snapshot as it is called: its names, or its version when it has none.
fn called(store: &Store, id: SnapshotId) -> Result<String, CliError> {
    let tags = store.snapshot(id).map_err(failed)?.tags;
    Ok(if tags.is_empty() {
        format!("version {id}")
    } else {
        tags.join(", ")
    })
}

/// A snapshot by its version, with its names when it has any.
fn versioned(store: &Store, id: SnapshotId) -> Result<String, CliError> {
    let tags = store.snapshot(id).map_err(failed)?.tags;
    Ok(if tags.is_empty() {
        format!("version {id}")
    } else {
        format!("version {id} ({})", tags.join(", "))
    })
}

/// Some names, the first ones one by one.
fn some(names: &[String]) -> String {
    let mut shown: Vec<&str> = names.iter().take(SHOWN).map(String::as_str).collect();
    let more = names.len().saturating_sub(SHOWN);
    let rest = format!("and {more} more");
    if more > 0 {
        shown.push(&rest);
    }
    shown.join(", ")
}

/// What a scan of the folder found, as lines and as data.
fn scanned(scan: &ExtractOutcome) -> (Vec<String>, Value) {
    let mut lines = vec![format!(
        "Documents: {} added, {} updated, {} deleted, {} unchanged.",
        scan.added.len(),
        scan.updated.len(),
        scan.deleted.len(),
        scan.unchanged
    )];
    for (sign, names) in [
        ("+", &scan.added),
        ("~", &scan.updated),
        ("-", &scan.deleted),
    ] {
        if !names.is_empty() {
            lines.push(format!("  {sign} {}", some(names)));
        }
    }
    let withheld = &scan.withheld;
    let left = withheld.added.len() + withheld.updated.len() + withheld.deleted.len();
    if left > 0 {
        lines.push(format!(
            "Not taken in, as apply says: {} added, {} updated, {} deleted.",
            withheld.added.len(),
            withheld.updated.len(),
            withheld.deleted.len()
        ));
        for (sign, names) in [
            ("+", &withheld.added),
            ("~", &withheld.updated),
            ("-", &withheld.deleted),
        ] {
            if !names.is_empty() {
                lines.push(format!("  {sign} {}", some(names)));
            }
        }
    }
    if !scan.skipped.is_empty() {
        lines.push(format!(
            "No loader takes {} files: {}",
            scan.skipped.len(),
            some(&scan.skipped)
        ));
    }
    let data = json!({
        "added": scan.added,
        "updated": scan.updated,
        "deleted": scan.deleted,
        "unchanged": scan.unchanged,
        "withheld": {
            "added": withheld.added,
            "updated": withheld.updated,
            "deleted": withheld.deleted,
        },
        "skipped": scan.skipped,
    });
    (lines, data)
}

/// Everything that has a name or is served, kind by kind.
fn list(store: &Store, only: Option<&str>) -> Result<(Value, String), CliError> {
    if let Some(only) = only
        && !KINDS.iter().any(|(word, _, _)| *word == only)
    {
        let words: Vec<&str> = KINDS.iter().map(|(word, _, _)| *word).collect();
        let hint = crate::cli_args::closest(only, words.iter().copied())
            .map_or(String::new(), |word| format!(" Did you mean {word}?"));
        return Err(invalid(format!(
            "knowledge-nest list does not know {only}.{hint} Use {}.",
            words.join(", ")
        )));
    }
    let names = store.names().map_err(failed)?;
    let served = store.served().map_err(failed)?;
    let mut data = Vec::new();
    let mut lines = Vec::new();
    for (word, kind, heading) in KINDS {
        if only.is_some_and(|only| only != word) {
            continue;
        }
        // The snapshots of the kind that have a name, and the indexes that are served.
        let mut ids: Vec<SnapshotId> = Vec::new();
        for (_, _, id) in names.iter().filter(|(of, _, _)| of == kind) {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
        if kind == "index" {
            for (_, id) in &served {
                if !ids.contains(id) {
                    ids.push(*id);
                }
            }
        }
        // Every result is listed, so that the one before the latest can still be read after
        // its name moved on.
        if kind == "eval-report" {
            for id in store.of_kind(kind).map_err(failed)? {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        let mut rows = Vec::new();
        for id in ids {
            let snapshot = store.snapshot(id).map_err(failed)?;
            let serving: Vec<&str> = served
                .iter()
                .filter(|(_, index)| *index == id)
                .map(|(name, _)| name.as_str())
                .collect();
            let parents = snapshot
                .parents
                .iter()
                .map(|parent| called(store, *parent))
                .collect::<Result<Vec<_>, _>>()?;
            let mut entry = json!({
                "kind": word,
                "names": snapshot.tags,
                "version": id.0,
                "made": moment(snapshot.created_at),
                "from": parents,
            });
            let mut recall = None;
            let what = match kind {
                "extracted" => {
                    entry["documents"] = json!(snapshot.items.len());
                    format!("{} documents", snapshot.items.len())
                }
                "chunks" => {
                    entry["documents"] = json!(snapshot.items.len());
                    format!(
                        "{} documents chunked, from documents {}",
                        snapshot.items.len(),
                        parents.join(" and ")
                    )
                }
                "index" => {
                    entry["served"] = json!(serving);
                    let mut what = format!("from chunks {}", parents.join(" and "));
                    if !serving.is_empty() {
                        what.push_str(&format!("  [served as {}]", serving.join(", ")));
                    }
                    what
                }
                "eval-data" => {
                    entry["cases"] = json!(snapshot.items.len());
                    format!("{} cases", snapshot.items.len())
                }
                _ => {
                    let result = report(store, id).map_err(failed)?;
                    recall = Some(result.recall);
                    entry["recall"] = json!(result.recall);
                    entry["found"] = json!(result.found);
                    entry["cases"] = json!(result.cases.len());
                    format!(
                        "recall {:.3} ({} of {}), index {} with data {}",
                        result.recall,
                        result.found,
                        result.cases.len(),
                        parents.first().map_or("-", String::as_str),
                        parents.get(1).map_or("-", String::as_str),
                    )
                }
            };
            let row = vec![
                String::new(),
                if snapshot.tags.is_empty() {
                    "-".to_owned()
                } else {
                    snapshot.tags.join(", ")
                },
                format!("v{id}"),
                moment(snapshot.created_at),
                what,
            ];
            rows.push((recall, row, entry));
        }
        // Results are read the best first; the rest in the order they were made.
        if kind == "eval-report" {
            rows.sort_by(|left, right| {
                right
                    .0
                    .partial_cmp(&left.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        if rows.is_empty() {
            lines.push(vec![heading.to_owned(), "(none)".to_owned()]);
        }
        for (position, (_, mut row, entry)) in rows.into_iter().enumerate() {
            if position == 0 {
                heading.clone_into(&mut row[0]);
            }
            lines.push(row);
            data.push(entry);
        }
    }
    Ok((json!({ "entries": data }), aligned(&lines).join("\n")))
}

fn usage(command: &str) -> CliError {
    invalid(format!(
        "Check the arguments of knowledge-nest {command}.\nUsage:\n{}",
        crate::cli_args::wrapped_usage(command).unwrap_or_default()
    ))
}

/// The names of the optimizations that were run, in the order they were first run.
fn optimizations(store: &Store) -> Result<Vec<String>, CliError> {
    let mut names: Vec<String> = Vec::new();
    for run in store.runs().map_err(failed)? {
        if run.stage != "optimize" {
            continue;
        }
        if let Some(name) = run.stats["name"].as_str()
            && !names.iter().any(|known| known == name)
        {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// Every evaluation result that has a name, the best first, and the optimizations there are.
fn results(store: &Store) -> Result<(Value, String), CliError> {
    let (mut data, mut text) = list(store, Some("results"))?;
    let names = optimizations(store)?;
    if !names.is_empty() {
        text.push_str(&format!(
            "\nOptimizations: {}. knowledge-nest results <name> shows the trials of one.",
            names.join(", ")
        ));
    }
    data["optimizations"] = json!(names);
    Ok((data, text))
}

/// One evaluation result: each case with the place its evidence was returned at.
fn result(store: &Store, name: &str) -> Result<(Value, String), CliError> {
    let id = store
        .resolve("eval-report", name)
        .map_err(failed)?
        .filter(|id| {
            store
                .snapshot(*id)
                .is_ok_and(|snapshot| snapshot.kind == "eval-report")
        })
        .ok_or_else(|| {
            invalid(format!(
                "There is no result and no optimization named {name}."
            ))
        })?;
    let snapshot = store.snapshot(id).map_err(failed)?;
    let found = report(store, id).map_err(failed)?;
    let questions: Vec<Case> = match snapshot.parents.get(1) {
        Some(data) => eval_data(store, *data).map_err(failed)?,
        None => Vec::new(),
    };
    let asked = |case: &str| questions.iter().find(|known| known.id == case);
    let question = |case: &str| asked(case).map_or("", |known| known.question.as_str());
    // What the search returned for each case, as the report keeps it.
    let mut returned: std::collections::HashMap<&str, Value> = std::collections::HashMap::new();
    for (name, object) in &snapshot.items {
        let saved = crate::records::ObjectId::parse(object)
            .and_then(|object| store.bytes(&object).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        if let (Some(case), Some(saved)) = (name.first(), saved) {
            returned.insert(case.as_str(), saved["returned"].clone());
        }
    }
    let parents = snapshot
        .parents
        .iter()
        .map(|parent| called(store, *parent))
        .collect::<Result<Vec<_>, _>>()?;
    let mut lines = vec![format!(
        "Result {}: recall {:.3} ({} of {}), index {} with data {}",
        versioned(store, id)?,
        found.recall,
        found.found,
        found.cases.len(),
        parents.first().map_or("-", String::as_str),
        parents.get(1).map_or("-", String::as_str),
    )];
    // The cases that were missed are read first.
    let mut cases: Vec<&(String, Option<usize>)> = found.cases.iter().collect();
    cases.sort_by_key(|(case, rank)| (rank.is_some(), rank.unwrap_or(0), case.clone()));
    let mut rows = vec![vec![
        "Found at".to_owned(),
        "Case".to_owned(),
        "Question".to_owned(),
    ]];
    for (case, rank) in &cases {
        rows.push(vec![
            rank.map_or("missed".to_owned(), |rank| rank.to_string()),
            case.clone(),
            question(case).chars().take(80).collect(),
        ]);
    }
    lines.extend(aligned(&rows));
    let data = json!({
        "names": snapshot.tags,
        "version": id.0,
        "recall": found.recall,
        "found": found.found,
        "from": parents,
        "made": moment(snapshot.created_at),
        "cases": cases
            .iter()
            .map(|(case, rank)| json!({
                "id": case,
                "rank": rank,
                "question": question(case),
                "expected": asked(case).map(|known| json!({
                    "source": known.source,
                    "document": known.document,
                    "quote": known.quote,
                })),
                "returned": returned.get(case.as_str()).cloned().unwrap_or(Value::Null),
            }))
            .collect::<Vec<_>>(),
    });
    Ok((data, lines.join("\n")))
}

/// Whether a command line is for the versioned store, and then the action and its argument.
///
/// The store's own commands always are. A command both stores have is when the configuration
/// file it reads is written for the versioned store: the file given, or `KnowledgeNestfile.yaml` of
/// the project. `build` and `eval` then get the file as their argument.
pub fn route(
    command: &str,
    parsed: &Parsed,
    project: &str,
    env: &CliEnv,
) -> Result<Option<(&'static str, Option<String>)>, CliError> {
    let positionals: Vec<&str> = parsed.positionals.iter().map(String::as_str).collect();
    let first = positionals
        .first()
        .copied()
        .filter(|first| !first.trim().is_empty());
    let own = |action: &'static str, needed: bool| {
        if positionals.len() > 1 || (needed && first.is_none()) {
            return Err(usage(command));
        }
        Ok(Some((action, first.map(str::to_owned))))
    };
    let action = match command {
        "list" => return own("list", false),
        "results" => return own("results", false),
        "release" => return own("release", true),
        "optimize" => return own("optimize", true),
        "build" => "build",
        "eval" => "eval",
        "search" => "search",
        "eval-data" => "eval-data",
        "validate" => "validate",
        "status" => "status",
        _ => return Ok(None),
    };
    // `build` and `eval` take the configuration file as their argument too.
    let named = if action == "build" || action == "eval" {
        first.or(parsed.string("config"))
    } else {
        parsed.string("config")
    };
    let file = match named {
        Some(named) => env.resolve(named),
        None => Path::new(project)
            .join(CONFIG)
            .to_string_lossy()
            .into_owned(),
    };
    let argued = (action == "build" || action == "eval") && first.is_some();
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if argued => {
            return Err(invalid(format!("{file} cannot be read: {error}")));
        }
        // The store before this one says what is wrong in its own words.
        Err(_) => return Ok(None),
    };
    if !staged(&text) {
        if argued {
            return Err(invalid(format!(
                "{file} is not written as a list of modules for each of extract, transform and index, so knowledge-nest {command} does not take it as an argument."
            )));
        }
        return Ok(None);
    }
    for name in earlier(command) {
        if parsed.value(name).is_some() {
            return Err(invalid(format!(
                "--{name} does not work with a configuration file of this form yet."
            )));
        }
    }
    match (action, positionals.as_slice()) {
        ("build" | "eval", [] | [_]) | ("validate", []) => Ok(Some((action, Some(file)))),
        // What there is and what is served is what `list` shows.
        ("status", []) => Ok(Some(("list", None))),
        ("search", [query]) if first.is_some() => Ok(Some((action, Some((*query).to_owned())))),
        ("eval-data", [file] | ["import", file]) if *file != "import" => {
            Ok(Some((action, Some((*file).to_owned()))))
        }
        ("eval-data", _) => Err(invalid(
            "With a configuration file of this form, knowledge-nest eval-data takes import <file> [--tag <name>].",
        )),
        _ => Err(usage(command)),
    }
}

/// Runs an action on the versioned store. Gives the result as data and as lines to read; `say`
/// hears what is going on meanwhile.
#[allow(clippy::too_many_lines)]
pub fn command(
    action: &str,
    argument: Option<&str>,
    parsed: &Parsed,
    project: &str,
    env: &CliEnv,
    say: &dyn Fn(&str),
) -> Result<(Value, String), CliError> {
    let project = Path::new(project);
    let indexes = project.join(".knowledge-nest/indexes");
    let store = Store::open(&project.join(".knowledge-nest/store")).map_err(failed)?;

    match action {
        "build" | "eval" => {
            let file = argument.unwrap_or_default();
            let config = std::fs::read_to_string(file)
                .map_err(|error| invalid(format!("{file} cannot be read: {error}")))?;
            let evaluate = action == "eval";
            if evaluate {
                let plan = Settings::parse(&config)
                    .and_then(|settings| settings.plan())
                    .map_err(invalid)?;
                if plan.eval.is_none() {
                    return Err(invalid(format!(
                        "{file} has no eval section, so there is nothing to evaluate with."
                    )));
                }
            }
            let made = run(
                &store,
                &RunRequest {
                    project,
                    indexes: &indexes,
                    config: &config,
                    evaluate,
                    release: parsed.flag("release"),
                    latest: true,
                    cause: None,
                    progress: &|stage, outcome| {
                        if let Some(outcome) = outcome {
                            say(&format!("{stage}: {}", outcome.as_str()));
                        }
                    },
                },
            )
            .map_err(failed)?;
            let mut lines = vec![if made.ran.is_empty() {
                "Nothing ran: every result was there already.".to_owned()
            } else {
                format!("Ran {}.", made.ran.join(", "))
            }];
            let mut documents = Value::Null;
            if let Some(scan) = &made.scan {
                let (found, data) = scanned(scan);
                lines.extend(found);
                documents = data;
            }
            if let Some(chunked) = made.chunked.filter(|chunked| *chunked > 0) {
                lines.push(format!("Chunked {chunked} documents."));
            }
            let tags = store.snapshot(made.index).map_err(failed)?.tags;
            lines.push(if tags.is_empty() {
                format!("Index: version {}", made.index.0)
            } else {
                format!("Index: version {} ({})", made.index.0, tags.join(", "))
            });
            if let Some(result) = &made.result {
                lines.push(format!(
                    "Recall: {:.3} ({} of {} cases)",
                    result.recall,
                    result.found,
                    result.cases.len()
                ));
            }
            if let Some(name) = &made.released {
                lines.push(format!("Served as {name}."));
            }
            let data = json!({
                "ran": made.ran,
                "tags": made.tags,
                "documents": documents,
                "chunked": made.chunked,
                "extracted": made.extracted.map(|id| id.0),
                "chunks": made.chunks.map(|id| id.0),
                "index": made.index.0,
                "report": made.report.map(|id| id.0),
                "recall": made.result.as_ref().map(|result| result.recall),
                "found": made.result.as_ref().map(|result| result.found),
                "cases": made.result.as_ref().map(|result| result.cases.len()),
                "released": made.released,
            });
            Ok((data, lines.join("\n")))
        }
        "list" => list(&store, argument),
        "validate" => {
            let file = argument.unwrap_or_default();
            let config = std::fs::read_to_string(file)
                .map_err(|error| invalid(format!("{file} cannot be read: {error}")))?;
            let plan = Settings::parse(&config)
                .and_then(|settings| settings.plan())
                .map_err(|reason| invalid(format!("{file} cannot run: {reason}")))?;
            let mut does = vec!["extract", "transform", "index"];
            if plan.eval.is_some() {
                does.push("eval");
            }
            let data = json!({ "file": file, "sections": does, "served": plan.release });
            Ok((data, format!("{file} can run: {}.", does.join(", "))))
        }
        "search" => {
            let query = argument.unwrap_or_default();
            let named = parsed
                .string("index")
                .or(parsed.string("build"))
                .unwrap_or(LATEST);
            // An index by its name or version, or the one that is served under a name.
            let served = match store.resolve("index", named).map_err(failed)? {
                Some(_) => None,
                None => store.releases(named).map_err(failed)?.last().copied(),
            };
            let index = &served.map_or(named.to_owned(), |id| id.to_string());
            let index = index.as_str();
            let limit = match parsed.string("limit") {
                None => 5,
                Some(limit) => limit
                    .parse::<usize>()
                    .ok()
                    .filter(|limit| *limit > 0)
                    .ok_or_else(|| invalid("--limit is a number above zero."))?,
            };
            let found = search(&store, &indexes, index, query, limit).map_err(failed)?;
            let shown = match served {
                Some(id) => format!("{named} (served, version {id})"),
                None => named.to_owned(),
            };
            let mut lines = vec![format!(
                "Index {shown}: {} chunks found.",
                if found.is_empty() {
                    "no".to_owned()
                } else {
                    found.len().to_string()
                }
            )];
            for (position, hit) in found.iter().enumerate() {
                lines.push(format!(
                    "{}. {}/{} lines {}-{}",
                    position + 1,
                    hit.source,
                    hit.document,
                    hit.lines.0,
                    hit.lines.1
                ));
                let first = hit
                    .text
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("");
                let shown: String = first.trim().chars().take(100).collect();
                lines.push(format!("   {shown}"));
            }
            let data = json!({
                "index": index,
                "found": found
                    .iter()
                    .map(|hit| json!({
                        "source": hit.source,
                        "document": hit.document,
                        "lines": [hit.lines.0, hit.lines.1],
                        "text": hit.text,
                    }))
                    .collect::<Vec<_>>(),
            });
            Ok((data, lines.join("\n")))
        }
        "release" => {
            let wanted = argument.unwrap_or_default();
            let index = store
                .resolve("index", wanted)
                .map_err(failed)?
                .filter(|id| {
                    store
                        .snapshot(*id)
                        .is_ok_and(|snapshot| snapshot.kind == "index")
                })
                .ok_or_else(|| invalid(format!("There is no index named {wanted}.")))?;
            let name = parsed.string("name").unwrap_or(SERVED);
            let before = store.releases(name).map_err(failed)?.last().copied();
            store.release(name, index).map_err(failed)?;
            let text = match before {
                Some(before) if before == index => {
                    format!("Version {index} is served as {name} already.")
                }
                Some(before) => format!(
                    "Served as {name}: {}. It was version {before}.",
                    versioned(&store, index)?
                ),
                None => format!("Served as {name}: {}.", versioned(&store, index)?),
            };
            let data = json!({
                "name": name,
                "index": index.0,
                "before": before.map(|id| id.0),
            });
            Ok((data, text))
        }
        "eval-data" => {
            let file = argument.unwrap_or_default();
            let cases: Vec<Case> = serde_json::from_str(&read(env, file)?).map_err(|error| {
                invalid(format!(
                    "{file} is not a list of cases with id, question, source, document, quote and split: {error}"
                ))
            })?;
            let tag = parsed.string("tag").unwrap_or(LATEST);
            let saved = save_eval_data(&store, tag, &cases).map_err(failed)?;
            let data = json!({
                "tag": tag,
                "cases": cases.len(),
                "snapshot": saved.snapshot.0,
                "outcome": saved.outcome.as_str(),
            });
            let text = format!(
                "Evaluation data {tag}: {} cases, version {} ({}).",
                cases.len(),
                saved.snapshot.0,
                saved.outcome.as_str()
            );
            Ok((data, text))
        }
        "optimize" => {
            let file = argument.unwrap_or_default();
            let instruction = read(env, file)?;
            let stem = Path::new(file)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let name = parsed.string("name").unwrap_or(&stem);
            let made = optimize(
                &store,
                &OptimizeRequest {
                    name,
                    instruction: &instruction,
                    project,
                    indexes: &indexes,
                    progress: &|event| match event {
                        Event::Trial {
                            number,
                            position,
                            total,
                            name,
                        } => say(&format!("[{position}/{total}] {number:03} {name}")),
                        Event::Stage {
                            stage,
                            outcome: Some(outcome),
                        } => say(&format!("      {stage}: {}", outcome.as_str())),
                        Event::Stage { .. } => {}
                        Event::Done { state, recall, .. } => say(&match (state, recall) {
                            (State::Failed(reason), _) => format!("      failed: {reason}"),
                            (_, Some(recall)) => format!("      recall {recall:.3}"),
                            _ => "      done".to_owned(),
                        }),
                    },
                },
            )
            .map_err(failed)?;
            let mut data = made.record.clone();
            data["run"] = json!(made.run);
            Ok((data, table(&made.record)))
        }
        _ => {
            // Without a name: every result that has one. With a name: the trials of the
            // optimization called so, or the cases of the result called so.
            let Some(name) = argument else {
                return results(&store);
            };
            let Some((record, running)) = latest(&store, Some(name)).map_err(failed)? else {
                return result(&store, name);
            };
            let mut text = table(&record);
            if running {
                let candidates = record["candidates"]
                    .as_array()
                    .map_or(&[][..], Vec::as_slice);
                let waiting = candidates
                    .iter()
                    .filter(|candidate| candidate["state"] == "pending")
                    .count();
                text.push_str(&format!(
                    "\nStill running: {} of {} trials are waiting.",
                    waiting,
                    candidates.len()
                ));
            }
            let mut data = record;
            data["running"] = json!(running);
            Ok((data, text))
        }
    }
}
