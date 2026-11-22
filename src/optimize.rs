//! Optimization on the new store (docs/data-management.md, chapter 11): an instruction says what
//! to vary, a configuration file is written for each trial, and each file is run like any other:
//! built and evaluated.
//!
//! Nothing here is new to the store. A trial is a run of a configuration file, so what two
//! trials share is made once, and a trial that was evaluated before is not run again. The
//! optimization itself is one run that makes no snapshot; the runs it starts name it as their
//! cause. A trial that fails is recorded with the reason and the next one goes on.
//!
//! The trials are every combination of the options (`search: grid`, the default), or as many as
//! the limit says, chosen one after the other at random (`random`) or from the results so far
//! (`tpe`, see `crate::tpe`). Trials are compared by recall, and by MRR when the recall is the
//! same.
//!
//! Not here yet: options that rewrite `extract`, limits other than the number of trials, and
//! measures other than recall and MRR.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Instant;

use serde_json::{Map, Value, json};
use yaml_rust2::Yaml;

use crate::assess::Report;
use crate::pipeline::{LATEST, RunRequest, run};
use crate::records::object_id;
use crate::settings::{SECTIONS, Settings, document, entries, plain_name, section};
use crate::store::{Outcome, SnapshotId, Store, StoreError, StoreResult};
use crate::tpe::{Domain, Method, Picked, Random, suggest};

/// How many trials may fail one after the other before the rest are left.
const FAILURES: usize = 3;
/// How many times a trial that was tried already is drawn again before it is taken as it is.
const REDRAWS: usize = 20;

pub struct OptimizeRequest<'a> {
    /// The name of the optimization. The files and the tags of its trials start with it.
    pub name: &'a str,
    /// The instruction, as YAML.
    pub instruction: &'a str,
    /// The folder of the project: the base configuration is read from it, and the files of the
    /// trials are written below `.knowledge-nest/trials` in it.
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
        mrr: Option<f64>,
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

/// How the trials are chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Search {
    /// Every combination of the options, in the order of the instruction.
    Grid,
    /// As many trials as the limit says, every value as likely as any other.
    Random,
    /// As many trials as the limit says, each chosen from the results so far.
    Tpe,
}

impl Search {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grid => "grid",
            Self::Random => "random",
            Self::Tpe => "tpe",
        }
    }
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
    /// The name of what the trial makes itself: the name of the optimization and the number.
    pub tag: String,
    /// The kinds of snapshot that have the name: `chunks` and `index` when the trial makes them
    /// itself, and `eval-report` for its result. Empty when the trial was not evaluated.
    pub named: Vec<String>,
    pub index: Option<SnapshotId>,
    pub report: Option<SnapshotId>,
    pub recall: Option<f64>,
    pub mrr: Option<f64>,
    pub elapsed_ms: Option<u64>,
}

impl Candidate {
    /// The order of the results: evaluated trials first, the best first; the others after, in
    /// their order.
    fn rank(&self, other: &Self) -> std::cmp::Ordering {
        match (self.recall.zip(self.mrr), other.recall.zip(other.mrr)) {
            (Some((recall, mrr)), Some((other_recall, other_mrr))) => {
                Report::better(other_recall, other_mrr, recall, mrr)
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then(self.number.cmp(&other.number))
    }
}

#[derive(Debug)]
pub struct Optimization {
    /// The run of the optimization.
    pub run: String,
    /// The base first, then the trials in the order they were made.
    pub candidates: Vec<Candidate>,
    /// What the run keeps: `table` prints it.
    pub record: Value,
}

/// What an argument of an option can be.
enum Slot {
    /// One of these values.
    Values(Vec<Value>),
    /// A whole number in a range, with `search: random` or `tpe`.
    Range { low: i64, high: i64, log: bool },
}

/// Where in the sections of an option an argument that varies is, and what it can be.
struct Place {
    section: String,
    position: usize,
    module: String,
    argument: String,
    slot: Slot,
}

/// An option of an axis: a label, the sections it rewrites, and the arguments in them that vary.
struct Choice {
    label: String,
    sections: Map<String, Value>,
    places: Vec<Place>,
}

impl Choice {
    /// The sections with a value for each argument that varies, and the label that says which.
    fn written(&self, values: &[Value]) -> (String, Map<String, Value>) {
        let mut sections = self.sections.clone();
        let mut labels = Vec::new();
        for (place, value) in self.places.iter().zip(values) {
            sections[&place.section][place.position][&place.module][&place.argument] =
                value.clone();
            labels.push(format!("{}={}", place.argument, label(value)));
        }
        let label = if labels.is_empty() {
            self.label.clone()
        } else {
            format!("{}({})", self.label, labels.join(","))
        };
        (label, sections)
    }

    /// Every variant: one for each combination of the values of the arguments that vary.
    fn variants(&self) -> StoreResult<Vec<(String, Map<String, Value>)>> {
        let mut made: Vec<Vec<Value>> = vec![Vec::new()];
        for place in &self.places {
            let Slot::Values(values) = &place.slot else {
                return Err(refused(format!(
                    "Option {} gives {} a range. A range is tried with search: random or tpe.",
                    self.label, place.argument
                )));
            };
            made = made
                .into_iter()
                .flat_map(|chosen| {
                    values.iter().map(move |value| {
                        let mut chosen = chosen.clone();
                        chosen.push(value.clone());
                        chosen
                    })
                })
                .collect();
        }
        Ok(made.iter().map(|values| self.written(values)).collect())
    }
}

struct Axis {
    name: String,
    choices: Vec<Choice>,
}

struct Instruction {
    base: String,
    axes: Vec<Axis>,
    data: Option<String>,
    split: Option<String>,
    limit: Option<usize>,
    search: Search,
    seed: u64,
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

/// `{ int: [low, high], log: true }`: a range of whole numbers. `None` for any other value.
fn range(option: &str, argument: &str, given: &Value) -> StoreResult<Option<Slot>> {
    let Value::Object(map) = given else {
        return Ok(None);
    };
    let Some(bounds) = map.get("int") else {
        return Ok(None);
    };
    let wrong = || {
        refused(format!(
            "{argument} in option {option} is a range written as {{ int: [low, high] }}, with log: true or false."
        ))
    };
    if map.keys().any(|key| key != "int" && key != "log") {
        return Err(wrong());
    }
    let [low, high] = bounds.as_array().map(Vec::as_slice).ok_or_else(wrong)? else {
        return Err(wrong());
    };
    let (Some(low), Some(high)) = (low.as_i64(), high.as_i64()) else {
        return Err(wrong());
    };
    let log = match map.get("log") {
        None => false,
        Some(Value::Bool(log)) => *log,
        Some(_) => return Err(wrong()),
    };
    if low > high {
        return Err(refused(format!(
            "The range of {argument} in option {option} starts above where it ends."
        )));
    }
    if log && low < 1 {
        return Err(refused(format!(
            "The range of {argument} in option {option} is on a logarithmic scale, so it starts at 1 or above."
        )));
    }
    Ok(Some(Slot::Range { low, high, log }))
}

/// Reads an option: the sections it rewrites, and the arguments in them given as a list of
/// values or as a range.
fn choice(option: &str, sections: Map<String, Value>) -> StoreResult<Choice> {
    let mut places = Vec::new();
    for (name, value) in &sections {
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
                let slot = match given {
                    Value::Array(values) if values.is_empty() => {
                        return Err(refused(format!(
                            "{argument} of {} in option {option} has no value to try.",
                            step.name
                        )));
                    }
                    Value::Array(values) => Slot::Values(values.clone()),
                    other => match range(option, argument, other)? {
                        Some(slot) => slot,
                        None => continue,
                    },
                };
                places.push(Place {
                    section: name.clone(),
                    position,
                    module: step.name.clone(),
                    argument: argument.clone(),
                    slot,
                });
            }
        }
    }
    Ok(Choice {
        label: option.to_owned(),
        sections,
        places,
    })
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
        let mut choices = Vec::new();
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
            choices.push(choice(&option, sections)?);
        }
        axes.push(Axis { name, choices });
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
    let search = match root["search"].as_str() {
        None if root["search"].is_badvalue() => Search::Grid,
        Some("grid") => Search::Grid,
        Some("random") => Search::Random,
        Some("tpe") => Search::Tpe,
        _ => return Err(refused("search is grid, random or tpe.")),
    };
    let seed = match &root["seed"] {
        Yaml::BadValue => 0,
        Yaml::Integer(seed) if *seed >= 0 => *seed as u64,
        _ => return Err(refused("seed is a whole number, 0 or above.")),
    };
    if search != Search::Grid && limit.is_none() {
        return Err(refused(format!(
            "search: {} tries as many trials as limit.candidates says. Give the limit.",
            search.as_str()
        )));
    }
    if search == Search::Grid && !root["seed"].is_badvalue() {
        return Err(refused(
            "seed is for search: random or tpe. search: grid tries every combination in order.",
        ));
    }
    Ok(Instruction {
        base: text(&root["base"], "base configuration")?,
        axes,
        data: eval["data"].as_str().map(str::to_owned),
        split,
        limit,
        search,
        seed,
    })
}

/// The entries of a section without its tag, and with `tag` when one is given.
fn retagged(settings: &Settings, name: &str, tag: Option<&String>) -> Value {
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
fn combinations(counts: &[usize]) -> Vec<Vec<usize>> {
    let mut made = vec![Vec::new()];
    for count in counts {
        made = made
            .into_iter()
            .flat_map(|chosen: Vec<usize>| {
                (0..*count).map(move |variant| {
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
    folder: &'a str,
}

impl Ground<'_> {
    /// The name of a trial. Each kind of snapshot has its own names, so one name does for the
    /// chunks, the index and the result of a trial.
    fn tag(&self, number: usize) -> String {
        format!("{}/{number:03}", self.name)
    }

    /// The configuration file of a trial, and the kinds of snapshot it gives its name to.
    ///
    /// The documents are the ones the base's name points at, so no trial scans the folder. A
    /// section a trial rewrites, and those after it, get the name of the trial; the sections
    /// before keep their modules and lose their tag, so that the base's names do not move. The
    /// result always gets the name. Nothing is released.
    fn trial(&self, number: usize, rewrites: &[Map<String, Value>]) -> (String, Vec<String>) {
        let mut settings = self.base.clone();
        let mut rewritten = Vec::new();
        for sections in rewrites {
            for (name, value) in sections {
                settings.replace(name, value.clone());
                rewritten.push(name.as_str());
            }
        }
        let own_chunks = rewritten.contains(&"transform");
        let own_index = own_chunks || rewritten.contains(&"index");
        let tag = self.tag(number);
        let mut named = Vec::new();
        settings.replace("extract", json!([{ "from": self.start }]));
        for (section, kind, own) in [
            ("transform", "chunks", own_chunks),
            ("index", "index", own_index),
        ] {
            settings.replace(section, retagged(&settings, section, own.then_some(&tag)));
            if own {
                named.push(kind.to_owned());
            }
        }
        settings.replace(
            "eval",
            json!([
                { "eval.evidence": { "data": self.data, "split": self.split } },
                { "tag": tag },
            ]),
        );
        named.push("eval-report".to_owned());
        settings.remove("serve");
        (settings.to_text(), named)
    }

    /// A trial that is not tried yet: its file, its name, and whether it can run.
    fn candidate(
        &self,
        number: usize,
        choices: Vec<(String, String)>,
        rewrites: &[Map<String, Value>],
    ) -> Candidate {
        let (config, named) = self.trial(number, rewrites);
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
        Candidate {
            number,
            name,
            choices,
            config,
            file: format!("{}/{number:03}.yaml", self.folder),
            tag: self.tag(number),
            named: if state == State::Pending {
                named
            } else {
                Vec::new()
            },
            state,
            ran: Vec::new(),
            index: None,
            report: None,
            recall: None,
            mrr: None,
            elapsed_ms: None,
        }
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
        "tag": candidate.tag,
        "named": candidate.named,
        "index": candidate.index.map(|id| id.0),
        "report": candidate.report.map(|id| id.0),
        "recall": candidate.recall,
        "mrr": candidate.mrr,
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
    let score = |candidate: &Value| {
        candidate["recall"]
            .as_f64()
            .map(|recall| (recall, candidate["mrr"].as_f64().unwrap_or(0.0)))
    };
    order.sort_by(|a, b| {
        match (score(a), score(b)) {
            (Some((recall, mrr)), Some((other_recall, other_mrr))) => {
                Report::better(other_recall, other_mrr, recall, mrr)
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then(
            a["number"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&b["number"].as_u64().unwrap_or(0)),
        )
    });
    let mut rows = vec![[
        "Trial".to_owned(),
        "Recall".to_owned(),
        "MRR".to_owned(),
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
            candidate["mrr"]
                .as_f64()
                .filter(|_| recall.is_some())
                .map_or("-".into(), |mrr| format!("{mrr:.3}")),
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
    let widths: Vec<usize> = (0..6)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let chosen = match (record["search"].as_str(), record["seed"].as_u64()) {
        (Some(search), Some(seed)) if search != "grid" => {
            format!("; {search}, seed {seed}")
        }
        _ => String::new(),
    };
    let mut lines = vec![format!(
        "Optimization {} (recall, then MRR, on {}, {}{chosen})",
        record["name"].as_str().unwrap_or_default(),
        record["data"].as_str().unwrap_or_default(),
        record["split"].as_str().unwrap_or_default(),
    )];
    for row in &rows {
        let mut line = String::new();
        for (cell, width) in row.iter().zip(&widths) {
            line.push_str(&format!("{cell:<width$}  "));
        }
        line.push_str(&row[6]);
        lines.push(line);
    }
    if let Some(stopped) = record["stopped"].as_str() {
        lines.push(stopped.to_owned());
    }
    lines.push(format!(
        "The files of the trials are in {}.",
        record["folder"].as_str().unwrap_or_default()
    ));
    let documents = &record["documents"];
    if let (Some(name), Some(version), Some(count)) = (
        documents["name"].as_str(),
        documents["version"].as_i64(),
        documents["count"].as_u64(),
    ) {
        lines.push(format!(
            "Every trial used the documents {name}, version {version} ({count} documents)."
        ));
    }
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

/// Runs one trial and records what it gave. True when it failed.
fn attempt(
    store: &Store,
    request: &OptimizeRequest<'_>,
    cause: &str,
    candidate: &mut Candidate,
    position: usize,
    total: usize,
) -> bool {
    (request.progress)(&Event::Trial {
        number: candidate.number,
        position,
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
            evaluate: true,
            release: false,
            latest: false,
            cause: Some(cause),
            progress: &|stage, outcome| {
                (request.progress)(&Event::Stage { stage, outcome });
            },
        },
    );
    candidate.elapsed_ms = Some(started.elapsed().as_millis() as u64);
    let failed = match outcome {
        Ok(made) => {
            candidate.ran = made.ran;
            candidate.index = Some(made.index);
            candidate.report = made.report;
            candidate.recall = made.result.as_ref().map(|result| result.recall);
            candidate.mrr = made.result.as_ref().map(|result| result.mrr);
            candidate.state = State::Evaluated;
            false
        }
        Err(error) => {
            candidate.state = State::Failed(error.to_string());
            candidate.named.clear();
            true
        }
    };
    (request.progress)(&Event::Done {
        number: candidate.number,
        state: &candidate.state,
        recall: candidate.recall,
        mrr: candidate.mrr,
    });
    failed
}

fn write(project: &Path, candidate: &Candidate) -> StoreResult<()> {
    let path = project.join(&candidate.file);
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|error| refused(format!("{} cannot be written: {error}", folder.display())))?;
    }
    std::fs::write(&path, &candidate.config)
        .map_err(|error| refused(format!("{} cannot be written: {error}", candidate.file)))
}

/// A trial of a random or TPE search: its number, the value of each parameter it used, and the
/// sections those values gave.
struct Tried {
    number: usize,
    picked: Vec<(usize, Picked)>,
    sections: Vec<Map<String, Value>>,
}

/// The parameters a trial can vary, as the choosers see them: one for the option of each axis,
/// and one for each argument that varies in each option.
struct Space {
    domains: Vec<Domain>,
    /// For each axis, the parameter of its option.
    axes: Vec<usize>,
    /// For each axis and option, the parameters of its arguments.
    places: Vec<Vec<Vec<usize>>>,
}

impl Space {
    fn new(axes: &[Axis]) -> Self {
        let mut domains = Vec::new();
        let mut of_axes = Vec::new();
        let mut places = Vec::new();
        for axis in axes {
            of_axes.push(domains.len());
            domains.push(Domain::Choice(axis.choices.len()));
            let mut of_choices = Vec::new();
            for choice in &axis.choices {
                let mut of_places = Vec::new();
                for place in &choice.places {
                    of_places.push(domains.len());
                    domains.push(match &place.slot {
                        Slot::Values(values) => Domain::Choice(values.len()),
                        Slot::Range { low, high, log } => Domain::Int {
                            low: *low,
                            high: *high,
                            log: *log,
                        },
                    });
                }
                of_choices.push(of_places);
            }
            places.push(of_choices);
        }
        Self {
            domains,
            axes: of_axes,
            places,
        }
    }
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
        .ok_or_else(|| refused("The base configuration has no extract section."))?
        .name()
        .unwrap_or(LATEST);
    // Every trial starts from these documents, as they are now: the folder is not scanned, so
    // that the trials differ in their settings and in nothing else.
    let corpus = store.snapshot(store.resolve("extracted", start)?.ok_or_else(not_built)?)?;
    if corpus.kind != "extracted" {
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
        .resolve("eval-data", data)?
        .ok_or_else(|| refused(format!("There is no evaluation data {data}.")))?;
    if kind(found)? != "eval-data" {
        return Err(refused(format!("{data} is not evaluation data.")));
    }
    let folder = format!(".knowledge-nest/trials/{}", request.name);
    let ground = Ground {
        name: request.name,
        base: &base,
        start,
        data,
        split,
        folder: &folder,
    };
    // With a grid, every combination is known before anything runs, and an option given a
    // range is refused before anything is written.
    let grid = match instruction.search {
        Search::Grid => Some(
            instruction
                .axes
                .iter()
                .map(|axis| {
                    axis.choices
                        .iter()
                        .map(Choice::variants)
                        .collect::<StoreResult<Vec<_>>>()
                        .map(|variants| variants.into_iter().flatten().collect::<Vec<_>>())
                })
                .collect::<StoreResult<Vec<_>>>()?,
        ),
        Search::Random | Search::Tpe => None,
    };

    let record = |candidates: &[Candidate], stopped: Option<&str>| {
        let mut record = json!({
            "name": request.name,
            "folder": folder,
            "base": object_id(base_text.as_bytes()).as_str(),
            "data": data,
            "split": split,
            "documents": {
                "name": start,
                "version": corpus.id.0,
                "count": corpus.items.len(),
            },
            "metric": "recall, then mrr",
            "search": instruction.search.as_str(),
            "candidates": candidates.iter().map(recorded).collect::<Vec<_>>(),
        });
        if instruction.search != Search::Grid {
            record["seed"] = json!(instruction.seed);
        }
        if let Some(stopped) = stopped {
            record["stopped"] = json!(stopped);
        }
        record
    };

    // The base is always tried, and first, so that what a trial is compared with is measured
    // the same way.
    let mut candidates = vec![ground.candidate(0, Vec::new(), &[])];
    if let Some(variants) = &grid {
        let counts: Vec<usize> = variants.iter().map(Vec::len).collect();
        for chosen in combinations(&counts) {
            let picked: Vec<&(String, Map<String, Value>)> = variants
                .iter()
                .zip(chosen)
                .map(|(axis, variant)| &axis[variant])
                .collect();
            let choices = instruction
                .axes
                .iter()
                .zip(&picked)
                .map(|(axis, (label, _))| (axis.name.clone(), label.clone()))
                .collect();
            let rewrites: Vec<Map<String, Value>> = picked
                .iter()
                .map(|(_, sections)| sections.clone())
                .collect();
            let number = candidates.len();
            candidates.push(ground.candidate(number, choices, &rewrites));
        }
        // In the order of the instruction. What goes over the limit is left, and stays in the
        // list.
        if let Some(limit) = instruction.limit {
            let mut pending = candidates
                .iter_mut()
                .filter(|candidate| candidate.number != 0 && candidate.state == State::Pending);
            for candidate in pending.by_ref().skip(limit) {
                candidate.state =
                    State::Skipped(format!("The limit of {limit} candidates was reached."));
                candidate.named.clear();
            }
        }
        // Every trial has a file, also the ones that are not run: the reason is in the list.
        for candidate in &candidates {
            write(request.project, candidate)?;
        }
    } else {
        write(request.project, &candidates[0])?;
    }

    let mut draft = store.begin_scan("optimize", "optimization")?;
    draft.set_config(request.instruction.as_bytes())?;
    for candidate in &candidates {
        draft.save(candidate.config.as_bytes())?;
    }
    // The runs of the trials write objects too.
    draft.flush()?;
    draft.report_progress(&record(&candidates, None))?;
    let cause = draft.run().to_owned();
    let mut failures = 0;
    let mut stopped = None;
    let too_many = || format!("{FAILURES} trials in a row failed, so the rest were not tried.");

    if grid.is_some() {
        let order: Vec<usize> = candidates
            .iter()
            .filter(|candidate| candidate.state == State::Pending)
            .map(|candidate| candidate.number)
            .collect();
        let total = order.len();
        for (position, number) in order.into_iter().enumerate() {
            if failures >= FAILURES {
                candidates[number].state = State::Skipped(too_many());
                candidates[number].named.clear();
                continue;
            }
            let failed = attempt(
                store,
                request,
                &cause,
                &mut candidates[number],
                position + 1,
                total,
            );
            failures = if failed { failures + 1 } else { 0 };
            draft.report_progress(&record(&candidates, None))?;
        }
    } else {
        // One trial after the other, each chosen from what the ones before gave.
        let limit = instruction.limit.unwrap_or(1);
        let total = limit + 1;
        let method = match instruction.search {
            Search::Tpe => Method::Tpe {
                startup: (limit / 3).clamp(3, 10),
            },
            Search::Random | Search::Grid => Method::Random,
        };
        let space = Space::new(&instruction.axes);
        let mut random = Random::new(instruction.seed);
        // The values of each trial, and the sections they gave.
        let mut tried: Vec<Tried> = Vec::new();
        if attempt(store, request, &cause, &mut candidates[0], 1, total) {
            failures += 1;
        }
        draft.report_progress(&record(&candidates, None))?;
        for number in 1..=limit {
            if failures >= FAILURES {
                stopped = Some(too_many());
                break;
            }
            // What each parameter was in the trials so far, the best trial first.
            let mut ranked: Vec<&Tried> = tried.iter().collect();
            ranked.sort_by(|a, b| candidates[a.number].rank(&candidates[b.number]));
            let history = |parameter: usize| -> Vec<Picked> {
                ranked
                    .iter()
                    .filter_map(|tried| {
                        tried
                            .picked
                            .iter()
                            .find(|(at, _)| *at == parameter)
                            .map(|(_, value)| *value)
                    })
                    .collect()
            };
            let mut next = None;
            for draw in 0..REDRAWS {
                let mut picked = Vec::new();
                let mut choices = Vec::new();
                let mut rewrites = Vec::new();
                for (at, axis) in instruction.axes.iter().enumerate() {
                    let parameter = space.axes[at];
                    let Picked::Choice(option) = suggest(
                        &space.domains[parameter],
                        &history(parameter),
                        method,
                        &mut random,
                    ) else {
                        unreachable!("the option of an axis is a choice");
                    };
                    picked.push((parameter, Picked::Choice(option)));
                    let chosen = &axis.choices[option];
                    let mut values = Vec::new();
                    for (place, parameter) in chosen.places.iter().zip(&space.places[at][option]) {
                        let value = suggest(
                            &space.domains[*parameter],
                            &history(*parameter),
                            method,
                            &mut random,
                        );
                        picked.push((*parameter, value));
                        values.push(match (&place.slot, value) {
                            (Slot::Values(given), Picked::Choice(index)) => given[index].clone(),
                            (_, Picked::Int(value)) => json!(value),
                            (Slot::Range { low, .. }, Picked::Choice(_)) => json!(low),
                        });
                    }
                    let (label, sections) = chosen.written(&values);
                    choices.push((axis.name.clone(), label));
                    rewrites.push(sections);
                }
                // A trial that was tried already gives nothing new: it is drawn again, a few
                // times, and taken as it is when nothing else comes.
                let again = tried.iter().any(|tried| tried.sections == rewrites);
                next = Some((picked, choices, rewrites));
                if !again || draw + 1 == REDRAWS {
                    break;
                }
            }
            let Some((picked, choices, rewrites)) = next else {
                break;
            };
            let mut candidate = ground.candidate(number, choices, &rewrites);
            write(request.project, &candidate)?;
            draft.save(candidate.config.as_bytes())?;
            draft.flush()?;
            if candidate.state == State::Pending {
                let failed = attempt(store, request, &cause, &mut candidate, number + 1, total);
                failures = if failed { failures + 1 } else { 0 };
            }
            candidates.push(candidate);
            tried.push(Tried {
                number,
                picked,
                sections: rewrites,
            });
            draft.report_progress(&record(&candidates, None))?;
        }
    }
    let record = record(&candidates, stopped.as_deref());
    draft.set_stats(record.clone());
    Ok(Optimization {
        run: draft.finish()?,
        candidates,
        record,
    })
}
