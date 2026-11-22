//! `dunlin store`: the versioned store from the command line (docs/data-management.md).
//!
//! The store is not what `dunlin build` and `dunlin search` use yet, so its commands are apart
//! from them: a configuration file in the store's syntax is run, evaluation data is saved, an
//! optimization is run and its results are shown. The store of a project is `.dunlin/store`.

use std::path::Path;

use serde_json::{Value, json};

use crate::assess::{Case, save_eval_data};
use crate::cli_args::Parsed;
use crate::cli_project::CliError;
use crate::cli_tools::CliEnv;
use crate::optimize::{Event, OptimizeRequest, State, latest, optimize, table};
use crate::pipeline::{RunRequest, run};
use crate::store::{Store, StoreError};

const ACTIONS: [&str; 4] = ["run", "eval-data", "optimize", "results"];

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

/// Runs an action of `dunlin store`. Gives the result as data and as lines to read; `say` hears
/// what is going on meanwhile.
pub fn command(
    parsed: &Parsed,
    project: &str,
    env: &CliEnv,
    say: &dyn Fn(&str),
) -> Result<(Value, String), CliError> {
    let usage = || {
        invalid(format!(
            "Check the arguments of dunlin store.\nUsage:\n{}",
            crate::cli_args::wrapped_usage("store").unwrap_or_default()
        ))
    };
    let positionals: Vec<&str> = parsed.positionals.iter().map(String::as_str).collect();
    let (action, argument) = match positionals.as_slice() {
        [action] => (*action, None),
        [action, argument] if !argument.trim().is_empty() => (*action, Some(*argument)),
        _ => return Err(usage()),
    };
    if !ACTIONS.contains(&action) {
        let hint = crate::cli_args::closest(action, ACTIONS)
            .map_or(String::new(), |name| format!(" Did you mean {name}?"));
        return Err(invalid(format!(
            "Unknown action for dunlin store: {action}.{hint} Use {}.",
            ACTIONS.join(", ")
        )));
    }
    if action != "results" && argument.is_none() {
        return Err(usage());
    }
    let project = Path::new(project);
    let indexes = project.join(".dunlin/indexes");
    let store = Store::open(&project.join(".dunlin/store")).map_err(failed)?;

    match action {
        "run" => {
            let config = read(env, argument.unwrap_or_default())?;
            let made = run(
                &store,
                &RunRequest {
                    project,
                    indexes: &indexes,
                    config: &config,
                    release: parsed.flag("release"),
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
        "eval-data" => {
            let file = argument.unwrap_or_default();
            let cases: Vec<Case> = serde_json::from_str(&read(env, file)?).map_err(|error| {
                invalid(format!(
                    "{file} is not a list of cases with id, question, source, document, quote and split: {error}"
                ))
            })?;
            let tag = parsed.string("tag").unwrap_or("default");
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
            let (record, running) = latest(&store, argument).map_err(failed)?.ok_or_else(|| {
                invalid(match argument {
                    Some(name) => format!("There is no optimization named {name}."),
                    None => "There is no optimization yet.".to_owned(),
                })
            })?;
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
