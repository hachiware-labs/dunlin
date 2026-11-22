//! Optimization on the new store (docs/data-management.md, chapter 11): an instruction says what
//! to vary, a configuration file is written for each trial, and each file is run like any other:
//! built and evaluated.
//!
//! Nothing here is new to the store. A trial is a run of a configuration file, so what two
//! trials share is made once, and a trial that was evaluated before is not run again. The
//! optimization itself is one run that makes no snapshot; the runs it starts name it as their
//! cause. A trial that fails is recorded with the reason and the next one goes on.
//!
//! Not here yet: options that rewrite `extract`, limits other than the number of trials,
//! measures other than recall, and choosing the next values from the results so far.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Instant;

use serde_json::{Map, Value, json};
use yaml_rust2::Yaml;

use crate::pipeline::{RunRequest, run};
use crate::records::object_id;
use crate::settings::{SECTIONS, Settings, document, entries, plain_name, section};
use crate::store::{Outcome, SnapshotId, Store, StoreError, StoreResult};

/// How many trials may fail one after the other before the rest are left.
const FAILURES: usize = 3;

pub struct OptimizeRequest<'a> {
    /// The name of the optimization. The files and the tags of its trials start with it.
    pub name: &'a str,
    /// The instruction, as YAML.
    pub instruction: &'a str,
    /// The folder of the project: the base configuration is read from it, and the files of the
    /// trials are written below `.dunlin/trials` in it.
    pub project: &'a Path,
    /// Where the directories of indexes are made.
    pub indexes: &'a Path,
    /// Hears what is going on.
    pub progress: &'a dyn Fn(&Event<'_>),
}

/// What an optimization says while it runs.
#[derive(Debug)]
pub enum Event<'a> {
    /// A trial starts: the `position`-th of the `total` that are tried.
    Trial {
        number: usize,
        position: usize,
        total: usize,
        name: &'a str,
    },
    /// A stage of the trial starts, and ends with an outcome.
    Stage {
        stage: &'a str,
        outcome: Option<Outcome>,
    },
    /// The trial ended.
    Done {
        number: usize,
        state: &'a State,
        recall: Option<f64>,
    },
}

/// What became of a trial.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Not tried yet.
    Pending,
    /// Built as far as needed and evaluated.
    Evaluated,
    /// The configuration cannot run. Nothing was built.
    Invalid(String),
    /// The run failed. The trials after it went on.
    Failed(String),
    /// Not tried, with the reason.
    Skipped(String),
}

/// One trial: a configuration file and what running it gave.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// Zero is the base configuration as it is.
    pub number: usize,
    /// The option chosen on each axis.
    pub name: String,
    pub choices: Vec<(String, String)>,
    /// The configuration file of the trial.
    pub config: String,
    /// Where the file is, from the project, with `/`.
    pub file: String,
    pub state: State,
    /// The stages that did work.
    pub ran: Vec<String>,
    /// The tags of what the trial makes itself, one for each stage.
    pub tags: Vec<String>,
    pub index: Option<SnapshotId>,
    pub report: Option<SnapshotId>,
    pub recall: Option<f64>,
    pub elapsed_ms: Option<u64>,
}

#[derive(Debug)]
pub struct Optimization {
    /// The run of the optimization.
    pub run: String,
    /// The base first, then the trials in the order of the instruction.
    pub candidates: Vec<Candidate>,
    /// What the run keeps: `table` prints it.
    pub record: Value,
}

struct Axis {
    name: String,
    /// The options with arrays of values written out: a label, and the sections it rewrites.
    variants: Vec<(String, Map<String, Value>)>,
}

struct Instruction {
    base: String,
    axes: Vec<Axis>,
    data: Option<String>,
    split: Option<String>,
    limit: Option<usize>,
}

fn refused(reason: impl Into<String>) -> StoreError {
    StoreError::new(reason)
}

fn text(yaml: &Yaml, what: &str) -> StoreResult<String> {
    yaml.as_str()
        .map(str::to_owned)
        .ok_or_else(|| refused(format!("The instruction has no {what}.")))
}

fn label(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Writes out an option: every argument given as an array is one value in each variant.
fn variants(
    option: &str,
    sections: &Map<String, Value>,
) -> StoreResult<Vec<(String, Map<String, Value>)>> {
    // Where the arrays are: section, entry, module, argument.
    let mut slots: Vec<(String, usize, String, String, Vec<Value>)> = Vec::new();
    for (name, value) in sections {
        if name == "eval" {
            return Err(refused(format!(
                "Option {option} rewrites eval. Every trial is measured the same way."
            )));
        }
        if !SECTIONS.contains(&name.as_str()) {
            return Err(refused(format!(
                "Option {option} rewrites {name}, which is not a section of modules."
            )));
        }
        if name == "extract" {
            return Err(refused(format!(
                "Option {option} rewrites extract. That is not on the new store yet."
            )));
        }
        let parsed =
            section(name, value).map_err(|reason| refused(format!("{option}: {reason}")))?;
        if parsed.tag.is_some() || parsed.from.is_some() {
            return Err(refused(format!(
                "Option {option} writes a tag. The trials are named by the optimization."
            )));
        }
        for (position, step) in parsed.steps.iter().enumerate() {
            for (argument, given) in &step.args {
                if let Value::Array(values) = given {
                    if values.is_empty() {
                        return Err(refused(format!(
                            "{argument} of {} in option {option} has no value to try.",
                            step.name
                        )));
                    }
                    slots.push((
                        name.clone(),
                        position,
                        step.name.clone(),
                        argument.clone(),
                        values.clone(),
                    ));
                }
            }
        }
    }
    let mut made = vec![(Vec::<String>::new(), sections.clone())];
    for (name, position, module, argument, values) in &slots {
        let mut next = Vec::with_capacity(made.len() * values.len());
        for (labels, sections) in &made {
            for value in values {
                let mut sections = sections.clone();
                sections[name][*position][module][argument] = value.clone();
                let mut labels = labels.clone();
                labels.push(format!("{argument}={}", label(value)));
                next.push((labels, sections));
            }
        }
        made = next;
    }
    Ok(made
        .into_iter()
        .map(|(labels, sections)| {
            let name = if labels.is_empty() {
                option.to_owned()
            } else {
                format!("{option}({})", labels.join(","))
            };
            (name, sections)
        })
        .collect())
}

fn instruction(source: &str) -> StoreResult<Instruction> {
    let whole = document(source).map_err(refused)?;
    let root = &whole["optimize"];
    if root.is_badvalue() {
        return Err(refused("The instruction has no optimize section."));
    }
    let mut axes: Vec<Axis> = Vec::new();
    let mut rewritten: HashMap<String, String> = HashMap::new();
    for axis in root["vary"]
        .as_vec()
        .ok_or_else(|| refused("The instruction has nothing to vary."))?
    {
        let name = text(&axis["name"], "name for an axis")?;
        if axes.iter().any(|axis| axis.name == name) {
            return Err(refused(format!("Two axes are named {name}.")));
        }
        let options = entries(&axis["options"])
            .map_err(|_| refused(format!("Axis {name} has no options.")))?;
        if options.is_empty() {
            return Err(refused(format!("Axis {name} has no options.")));
        }
        let mut made = Vec::new();
        for (option, sections) in options {
            let Value::Object(sections) = sections else {
                return Err(refused(format!(
                    "Option {option} of axis {name} is not a mapping of sections."
                )));
            };
            for section in sections.keys() {
                match rewritten.get(section) {
                    Some(other) if *other != name => {
                        return Err(refused(format!(
                            "Axes {other} and {name} both rewrite {section}. \
                             Put what changes together in one option."
                        )));
                    }
                    _ => {
                        rewritten.insert(section.clone(), name.clone());
                    }
                }
            }
            made.extend(variants(&option, &sections)?);
        }
        axes.push(Axis {
            name,
            variants: made,
        });
    }
    if axes.is_empty() {
        return Err(refused("The instruction has nothing to vary."));
    }
    let eval = &root["eval"];
    match eval["metric"].as_str() {
        None | Some("recall") => {}
        Some(other) => {
            return Err(refused(format!(
                "The metric {other} is not on the new store yet. Use recall."
            )));
        }
    }
    let split = match eval["split"].as_str() {
        None => None,
        Some(split) if split == "dev" || split == "test" => Some(split.to_owned()),
        Some(_) => return Err(refused("eval.split is dev or test.")),
    };
    let limit = match &root["limit"]["candidates"] {
        Yaml::BadValue => None,
        Yaml::Integer(limit) if *limit > 0 => Some(*limit as usize),
        _ => return Err(refused("limit.candidates is not a number above zero.")),
    };
    Ok(Instruction {
        base: text(&root["base"], "base configuration")?,
        axes,
        data: eval["data"].as_str().map(str::to_owned),
        split,
        limit,
    })
}

/// The entries of a section without its tag, and with `tag` when one is given.
fn retagged(settings: &Settings, name: &str, tag: Option<&str>) -> Value {
    let mut entries: Vec<Value> = settings
        .get(name)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.get("tag").is_none())
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if let Some(tag) = tag {
        entries.push(json!({ "tag": tag }));
    }
    Value::Array(entries)
}

/// Every choice of one variant on each axis, the first axis outermost.
fn combinations(axes: &[Axis]) -> Vec<Vec<usize>> {
    let mut made = vec![Vec::new()];
    for axis in axes {
        made = made
            .into_iter()
            .flat_map(|chosen: Vec<usize>| {
                (0..axis.variants.len()).map(move |variant| {
                    let mut chosen = chosen.clone();
                    chosen.push(variant);
                    chosen
                })
            })
            .collect();
    }
    made
}

/// What every trial shares: where it starts, how it is measured and what it is named after.
struct Ground<'a> {
    name: &'a str,
    base: &'a Settings,
    /// The name of the documents every trial starts from.
    start: &'a str,
    data: &'a str,
    split: &'a str,
}

impl Ground<'_> {
    /// The configuration file of a trial, and the tags it gives to what it makes itself.
    ///
    /// The documents are the ones the base's name points at, so no trial scans the folder. A
    /// section a trial rewrites, and those after it, get a tag of the trial; the sections before
    /// keep their modules and lose their tag, so that the base's names do not move. Nothing is
    /// released.
    fn trial(&self, number: usize, rewrites: &[&Map<String, Value>]) -> (String, Vec<String>) {
        let mut settings = self.base.clone();
        let mut rewritten = Vec::new();
        for sections in rewrites {
            for (name, value) in *sections {
                settings.replace(name, value.clone());
                rewritten.push(name.as_str());
            }
        }
        let own_chunks = rewritten.contains(&"transform");
        let own_index = own_chunks || rewritten.contains(&"index");
        let tag = |stage: &str| format!("{}/{number:03}/{stage}", self.name);
        let mut tags = Vec::new();
        settings.replace("extract", json!([{ "from": self.start }]));
        for (section, stage, own) in [
            ("transform", "chunks", own_chunks),
            ("index", "index", own_index),
        ] {
            let tag = own.then(|| tag(stage));
            settings.replace(section, retagged(&settings, section, tag.as_deref()));
            tags.extend(tag);
        }
        settings.replace(
            "eval",
            json!([{ "eval.evidence": { "data": self.data, "split": self.split } }]),
        );
        settings.remove("serve");
        (settings.to_text(), tags)
    }
}

fn recorded(candidate: &Candidate) -> Value {
    let (state, reason) = match &candidate.state {
        State::Pending => ("pending", None),
        State::Evaluated => ("evaluated", None),
        State::Invalid(reason) => ("invalid", Some(reason)),
        State::Failed(reason) => ("failed", Some(reason)),
        State::Skipped(reason) => ("skipped", Some(reason)),
    };
    json!({
        "number": candidate.number,
        "name": candidate.name,
        "choices": candidate.choices.iter().cloned().collect::<BTreeMap<_, _>>(),
        "config": object_id(candidate.config.as_bytes()).as_str(),
        "file": candidate.file,
        "state": state,
        "reason": reason,
        "ran": candidate.ran,
        "tags": candidate.tags,
        "index": candidate.index.map(|id| id.0),
        "report": candidate.report.map(|id| id.0),
        "recall": candidate.recall,
        "elapsed_ms": candidate.elapsed_ms,
    })
}

/// The results of an optimization as lines to read: the trials that were evaluated, the best
/// first, and then the others with the reason.
pub fn table(record: &Value) -> String {
    let empty = Vec::new();
    let candidates = record["candidates"].as_array().unwrap_or(&empty);
    let base = candidates
        .iter()
        .find(|candidate| candidate["number"] == 0)
        .and_then(|candidate| candidate["recall"].as_f64());
    let mut order: Vec<&Value> = candidates.iter().collect();
    order.sort_by(|a, b| {
        let key = |candidate: &Value| {
            (
                candidate["recall"].as_f64().map_or(1, |_| 0),
                -candidate["recall"].as_f64().unwrap_or(0.0),
                candidate["number"].as_u64().unwrap_or(0),
            )
        };
        key(a)
            .partial_cmp(&key(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut rows = vec![[
        "Trial".to_owned(),
        "Recall".to_owned(),
        "To base".to_owned(),
        "Ran".to_owned(),
        "Time".to_owned(),
        "What".to_owned(),
    ]];
    for candidate in order {
        let recall = candidate["recall"].as_f64();
        let ran: Vec<&str> = candidate["ran"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let mut what = candidate["name"].as_str().unwrap_or_default().to_owned();
        if let Some(state) = candidate["state"].as_str().filter(|_| recall.is_none()) {
            what.push_str(&format!(" ({state}"));
            if let Some(reason) = candidate["reason"].as_str() {
                what.push_str(&format!(": {reason}"));
            }
            what.push(')');
        }
        rows.push([
            format!("{:03}", candidate["number"].as_u64().unwrap_or(0)),
            recall.map_or("-".into(), |recall| format!("{recall:.3}")),
            match (recall, base) {
                (Some(recall), Some(base)) if candidate["number"] != 0 => {
                    format!("{:+.3}", recall - base)
                }
                _ => "-".into(),
            },
            if recall.is_none() {
                "-".into()
            } else if ran.is_empty() {
                "nothing".into()
            } else {
                ran.join("+")
            },
            candidate["elapsed_ms"]
                .as_u64()
                .map_or("-".into(), |ms| format!("{:.1}s", ms as f64 / 1000.0)),
            what,
        ]);
    }
    let widths: Vec<usize> = (0..5)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut lines = vec![format!(
        "Optimization {} ({} on {}, {})",
        record["name"].as_str().unwrap_or_default(),
        record["metric"].as_str().unwrap_or_default(),
        record["data"].as_str().unwrap_or_default(),
        record["split"].as_str().unwrap_or_default(),
    )];
    for row in &rows {
        let mut line = String::new();
        for (cell, width) in row.iter().zip(&widths) {
            line.push_str(&format!("{cell:<width$}  "));
        }
        line.push_str(&row[5]);
        lines.push(line);
    }
    lines.push(format!(
        "The files of the trials are in {}.",
        record["folder"].as_str().unwrap_or_default()
    ));
    lines.join("\n")
}

/// What the latest optimization keeps, and whether it is still going. With a name, the latest
/// one of that name.
pub fn latest(store: &Store, name: Option<&str>) -> StoreResult<Option<(Value, bool)>> {
    Ok(store
        .runs()?
        .into_iter()
        .rev()
        .filter(|run| run.stage == "optimize" && run.stats["candidates"].is_array())
        .find(|run| name.is_none_or(|name| run.stats["name"] == name))
        .map(|run| (run.stats, run.state != "complete" && run.state != "failed")))
}

/// Writes a configuration file for each trial of an instruction and runs them one after the
/// other.
///
/// The names that are served and the tags of the base configuration do not move. An instruction
/// that is wrong in itself is an error; a trial that cannot run or that fails is recorded with
/// the reason and the others go on.
#[allow(clippy::too_many_lines)]
pub fn optimize(store: &Store, request: &OptimizeRequest<'_>) -> StoreResult<Optimization> {
    if !plain_name(request.name) {
        return Err(refused(
            "The name of an optimization is letters, digits, _, . and -.",
        ));
    }
    let instruction = instruction(request.instruction)?;
    let base_text = std::fs::read_to_string(request.project.join(&instruction.base))
        .map_err(|error| refused(format!("{} cannot be read: {error}", instruction.base)))?;
    let base = Settings::parse(&base_text).map_err(refused)?;
    let base_plan = base
        .plan()
        .map_err(|reason| refused(format!("The base configuration cannot run: {reason}")))?;

    let kind = |snapshot| store.snapshot(snapshot).map(|snapshot| snapshot.kind);
    let not_built = || refused("The base configuration is not built yet. Run it first.");
    let start = base_plan
        .extract
        .as_ref()
        .and_then(|extract| extract.name())
        .ok_or_else(|| refused("The extract section of the base configuration has no tag."))?;
    if kind(store.resolve(start)?.ok_or_else(not_built)?)? != "extracted" {
        return Err(refused("The tag of extract does not name documents."));
    }
    let judging = base_plan.eval.as_ref();
    let data = instruction
        .data
        .as_deref()
        .or(judging.map(|judging| judging.data.as_str()))
        .ok_or_else(|| {
            refused("The instruction and the base configuration name no evaluation data.")
        })?;
    let split = instruction
        .split
        .as_deref()
        .or(judging.map(|judging| judging.split.as_str()))
        .unwrap_or("dev");
    let found = store
        .resolve(data)?
        .ok_or_else(|| refused(format!("There is no evaluation data {data}.")))?;
    if kind(found)? != "eval-data" {
        return Err(refused(format!("{data} is not evaluation data.")));
    }
    let ground = Ground {
        name: request.name,
        base: &base,
        start,
        data,
        split,
    };

    // The base is always tried, and first, so that what a trial is compared with is measured
    // the same way.
    let folder = format!(".dunlin/trials/{}", request.name);
    let mut candidates = Vec::new();
    let mut chosen: Vec<Vec<usize>> = vec![Vec::new()];
    chosen.extend(combinations(&instruction.axes));
    for (number, chosen) in chosen.into_iter().enumerate() {
        let picked: Vec<&(String, Map<String, Value>)> = instruction
            .axes
            .iter()
            .zip(chosen)
            .map(|(axis, variant)| &axis.variants[variant])
            .collect();
        let choices: Vec<(String, String)> = instruction
            .axes
            .iter()
            .zip(&picked)
            .map(|(axis, (label, _))| (axis.name.clone(), label.clone()))
            .collect();
        let rewrites: Vec<&Map<String, Value>> =
            picked.iter().map(|(_, sections)| sections).collect();
        let (config, tags) = ground.trial(number, &rewrites);
        let state = match Settings::parse(&config).and_then(|settings| settings.plan()) {
            Ok(_) => State::Pending,
            Err(reason) => State::Invalid(reason),
        };
        let name = if number == 0 {
            "base".to_owned()
        } else {
            choices
                .iter()
                .map(|(axis, label)| format!("{axis}={label}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        candidates.push(Candidate {
            number,
            name,
            choices,
            config,
            file: format!("{folder}/{number:03}.yaml"),
            tags: if state == State::Pending {
                tags
            } else {
                Vec::new()
            },
            state,
            ran: Vec::new(),
            index: None,
            report: None,
            recall: None,
            elapsed_ms: None,
        });
    }

    // In the order of the instruction. What goes over the limit is left, and stays in the list.
    let mut order: Vec<usize> = candidates
        .iter()
        .filter(|candidate| candidate.state == State::Pending)
        .map(|candidate| candidate.number)
        .collect();
    if let Some(limit) = instruction.limit {
        let kept = order.iter().filter(|number| **number == 0).count() + limit;
        for number in order.split_off(kept.min(order.len())) {
            candidates[number].state =
                State::Skipped(format!("The limit of {limit} candidates was reached."));
            candidates[number].tags.clear();
        }
    }

    // Every trial has a file, also the ones that are not run: the reason is in the list.
    let written = |error: std::io::Error| refused(format!("{folder} cannot be written: {error}"));
    std::fs::create_dir_all(request.project.join(&folder)).map_err(written)?;
    for candidate in &candidates {
        std::fs::write(request.project.join(&candidate.file), &candidate.config)
            .map_err(written)?;
    }

    let record = |candidates: &[Candidate]| {
        json!({
            "name": request.name,
            "folder": folder,
            "base": object_id(base_text.as_bytes()).as_str(),
            "data": data,
            "split": split,
            "metric": "recall",
            "candidates": candidates.iter().map(recorded).collect::<Vec<_>>(),
        })
    };
    let mut draft = store.begin_scan("optimize", "optimization")?;
    draft.set_config(request.instruction.as_bytes())?;
    for candidate in &candidates {
        draft.save(candidate.config.as_bytes())?;
    }
    // The runs of the trials write objects too.
    draft.flush()?;
    draft.report_progress(&record(&candidates))?;
    let cause = draft.run().to_owned();
    let total = order.len();
    let mut failures = 0;
    for (position, number) in order.into_iter().enumerate() {
        if failures >= FAILURES {
            candidates[number].state = State::Skipped(format!(
                "{FAILURES} trials in a row failed, so the rest were not tried."
            ));
            candidates[number].tags.clear();
            continue;
        }
        let candidate = &mut candidates[number];
        (request.progress)(&Event::Trial {
            number,
            position: position + 1,
            total,
            name: &candidate.name,
        });
        let started = Instant::now();
        let outcome = run(
            store,
            &RunRequest {
                project: request.project,
                indexes: request.indexes,
                config: &candidate.config,
                release: false,
                cause: Some(&cause),
                progress: &|stage, outcome| {
                    (request.progress)(&Event::Stage { stage, outcome });
                },
            },
        );
        candidate.elapsed_ms = Some(started.elapsed().as_millis() as u64);
        match outcome {
            Ok(made) => {
                failures = 0;
                candidate.ran = made.ran;
                candidate.index = Some(made.index);
                candidate.report = made.report;
                candidate.recall = made.result.map(|result| result.recall);
                candidate.state = State::Evaluated;
            }
            Err(error) => {
                failures += 1;
                candidate.state = State::Failed(error.to_string());
                candidate.tags.clear();
            }
        }
        (request.progress)(&Event::Done {
            number,
            state: &candidate.state,
            recall: candidate.recall,
        });
        draft.report_progress(&record(&candidates))?;
    }
    let record = record(&candidates);
    draft.set_stats(record.clone());
    Ok(Optimization {
        run: draft.finish()?,
        candidates,
        record,
    })
}
