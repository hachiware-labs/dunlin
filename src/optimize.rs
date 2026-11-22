//! Optimization on the new store (docs/data-management.md, chapter 11): an instruction says what
//! to vary, a configuration file is made for each candidate, and each candidate is built and
//! evaluated.
//!
//! Nothing here is new to the store. A candidate is a build and an evaluation like any other, so
//! what two candidates share is made once, and a candidate that was evaluated before is not run
//! again. The optimization itself is one run that makes no snapshot; the runs it starts name it
//! as their cause.
//!
//! Not here yet: options that rewrite `extract`, limits other than the number of candidates,
//! measures other than recall, and the CLI.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::{Map, Value, json};
use yaml_rust2::Yaml;

use crate::assess::{EvalRequest, evaluate, process as judging};
use crate::index::{IndexRequest, index, indexing};
use crate::records::object_id;
use crate::settings::{Plan, SECTIONS, Settings, document, entries, section};
use crate::store::{Outcome, SnapshotId, Store, StoreError, StoreResult};
use crate::transform::{TransformRequest, transform};

pub struct OptimizeRequest<'a> {
    /// The name of the optimization. The tags of its candidates start with it.
    pub name: &'a str,
    /// The instruction, as YAML.
    pub instruction: &'a str,
    /// The folder the instruction names its base configuration in.
    pub project: &'a Path,
    /// Where the directories of indexes are made.
    pub indexes: &'a Path,
    /// Only say what would run.
    pub plan_only: bool,
}

/// What became of a candidate.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Built as far as needed and evaluated.
    Evaluated,
    /// Would be evaluated: the request was only for a plan.
    Planned,
    /// The configuration cannot run. Nothing was built.
    Invalid(String),
    /// The same processing as the candidate with this number.
    Same(usize),
    /// Not run, with the reason.
    Skipped(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// Zero is the base configuration as it is.
    pub number: usize,
    /// The option chosen on each axis.
    pub name: String,
    pub choices: Vec<(String, String)>,
    /// The configuration file of the candidate.
    pub config: String,
    pub state: State,
    /// The sections that were expected to run, found by looking up keys.
    pub expected: Vec<String>,
    /// The documents `transform` was expected to chunk.
    pub items: usize,
    /// The sections that ran.
    pub ran: Vec<String>,
    /// The name of the candidate's index.
    pub tag: Option<String>,
    pub index: Option<SnapshotId>,
    pub report: Option<SnapshotId>,
    pub recall: Option<f64>,
}

#[derive(Debug)]
pub struct Optimization {
    /// The run of the optimization. A plan has none.
    pub run: Option<String>,
    /// The base first, then the candidates in the order of the instruction.
    pub candidates: Vec<Candidate>,
}

struct Axis {
    name: String,
    /// The options with arrays of values written out: a label, and the sections it rewrites.
    variants: Vec<(String, Map<String, Value>)>,
}

struct Instruction {
    base: String,
    axes: Vec<Axis>,
    data: String,
    split: String,
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
        if parsed.tag.is_some() {
            return Err(refused(format!(
                "Option {option} writes a tag. The candidates are named by the optimization."
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
    let limit = match &root["limit"]["candidates"] {
        Yaml::BadValue => None,
        Yaml::Integer(limit) if *limit > 0 => Some(*limit as usize),
        _ => return Err(refused("limit.candidates is not a number above zero.")),
    };
    Ok(Instruction {
        base: text(&root["base"], "base configuration")?,
        axes,
        data: text(&eval["data"], "evaluation data")?,
        split: eval["split"].as_str().unwrap_or("dev").to_owned(),
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

/// What is needed to run candidates: where they start and how they are judged.
struct Ground<'a> {
    store: &'a Store,
    /// The `extracted` snapshot every candidate starts from.
    input: SnapshotId,
    data: SnapshotId,
    split: &'a str,
    indexes: &'a Path,
}

impl Ground<'_> {
    /// The sections that would run, and the documents `transform` would chunk, by looking up
    /// keys. No document is read.
    fn expect(&self, plan: &Plan) -> StoreResult<(Vec<String>, usize)> {
        let store = self.store;
        let chunking = plan.chunking_process();
        let chunks = store.lookup("chunks", &[self.input], &chunking)?;
        let index = match chunks {
            Some(chunks) => store.lookup("index", &[chunks], &indexing())?,
            None => None,
        };
        let report = match index {
            Some(index) => store.lookup(
                "eval-report",
                &[index, self.data],
                &judging(&plan.retrieval(), self.split),
            )?,
            None => None,
        };
        let mut items = 0;
        if chunks.is_none() {
            let applied = chunking.id();
            for (_, item) in store.snapshot(self.input)?.items {
                items += usize::from(store.result(&item, &applied)?.is_none());
            }
        }
        let expected = [
            ("transform", chunks.is_none()),
            ("index", index.is_none()),
            ("eval", report.is_none()),
        ]
        .into_iter()
        .filter(|(_, runs)| *runs)
        .map(|(name, _)| name.to_owned())
        .collect();
        Ok((expected, items))
    }

    /// Builds what the candidate needs and evaluates it.
    fn run(
        &self,
        candidate: &mut Candidate,
        plan: &Plan,
        own: bool,
        cause: &str,
    ) -> StoreResult<()> {
        let config = Some(candidate.config.as_bytes());
        let cause = Some(cause);
        let chunked = transform(
            self.store,
            &TransformRequest {
                input: self.input,
                chunking: plan.chunking,
                context: plan.context,
                tag: None,
                config,
                cause,
            },
        )?;
        let indexed = index(
            self.store,
            &IndexRequest {
                input: chunked.chunks.snapshot,
                directory: self.indexes,
                // A candidate that makes no index of its own uses the base's and moves no name.
                tag: candidate.tag.as_deref().filter(|_| own),
                config,
                cause,
            },
        )?;
        let judged = evaluate(
            self.store,
            &EvalRequest {
                index: indexed.index.snapshot,
                data: self.data,
                retrieval: &plan.retrieval(),
                top_k: plan.top_k,
                split: self.split,
                directory: self.indexes,
                tag: None,
                config,
                cause,
            },
        )?;
        candidate.ran = [
            ("transform", chunked.chunks.outcome),
            ("index", indexed.index.outcome),
            ("eval", judged.report.outcome),
        ]
        .into_iter()
        .filter(|(_, outcome)| *outcome != Outcome::Reused)
        .map(|(name, _)| name.to_owned())
        .collect();
        candidate.index = Some(indexed.index.snapshot);
        candidate.report = Some(judged.report.snapshot);
        candidate.recall = Some(judged.result.recall);
        candidate.state = State::Evaluated;
        Ok(())
    }
}

fn recorded(candidate: &Candidate) -> Value {
    let (state, reason, same) = match &candidate.state {
        State::Evaluated => ("evaluated", None, None),
        State::Planned => ("planned", None, None),
        State::Invalid(reason) => ("invalid", Some(reason.clone()), None),
        State::Same(number) => ("same", None, Some(*number)),
        State::Skipped(reason) => ("skipped", Some(reason.clone()), None),
    };
    json!({
        "number": candidate.number,
        "name": candidate.name,
        "choices": candidate.choices.iter().cloned().collect::<BTreeMap<_, _>>(),
        "config": object_id(candidate.config.as_bytes()).as_str(),
        "state": state,
        "reason": reason,
        "same": same,
        "expected": candidate.expected,
        "items": candidate.items,
        "ran": candidate.ran,
        "tag": candidate.tag,
        "index": candidate.index.map(|id| id.0),
        "report": candidate.report.map(|id| id.0),
        "recall": candidate.recall,
    })
}

/// Makes the candidates of an instruction, builds and evaluates them.
///
/// The names that are served and the tags of the base configuration do not move. An instruction
/// that is wrong in itself is an error; a candidate that cannot run is recorded with the reason
/// and the others go on.
pub fn optimize(store: &Store, request: &OptimizeRequest<'_>) -> StoreResult<Optimization> {
    let instruction = instruction(request.instruction)?;
    let base_text = std::fs::read_to_string(request.project.join(&instruction.base))
        .map_err(|error| refused(format!("{} cannot be read: {error}", instruction.base)))?;
    let base = Settings::parse(&base_text).map_err(refused)?;
    let base_plan = base
        .plan()
        .map_err(|reason| refused(format!("The base configuration cannot run: {reason}")))?;

    let kind = |snapshot| store.snapshot(snapshot).map(|snapshot| snapshot.kind);
    let input = match &base_plan.extract_tag {
        Some(tag) => store.resolve(tag)?,
        None => None,
    }
    .ok_or_else(|| refused("The base configuration is not built yet. Run a build first."))?;
    if kind(input)? != "extracted" {
        return Err(refused("The tag of extract does not name documents."));
    }
    let data = store
        .resolve(&instruction.data)?
        .ok_or_else(|| refused(format!("There is no evaluation data {}.", instruction.data)))?;
    if kind(data)? != "eval-data" {
        return Err(refused(format!(
            "{} is not evaluation data.",
            instruction.data
        )));
    }
    let ground = Ground {
        store,
        input,
        data,
        split: &instruction.split,
        indexes: request.indexes,
    };
    let base_tag = base.section("index").map_err(refused)?.tag;

    // The base is always evaluated, so that what a candidate is compared with is measured the
    // same way.
    let blank = |number, name: String, choices, config, tag| Candidate {
        number,
        name,
        choices,
        config,
        state: State::Planned,
        expected: Vec::new(),
        items: 0,
        ran: Vec::new(),
        tag,
        index: None,
        report: None,
        recall: None,
    };
    let mut candidates = vec![blank(
        0,
        "base".to_owned(),
        Vec::new(),
        base_text.clone(),
        base_tag.clone(),
    )];
    let mut plans: Vec<Option<Plan>> = vec![Some(base_plan)];
    // Which candidates make an index of their own.
    let mut owns = vec![false];
    for (position, chosen) in combinations(&instruction.axes).into_iter().enumerate() {
        let number = position + 1;
        let mut settings = base.clone();
        let mut choices = Vec::new();
        let mut rewritten = Vec::new();
        for (axis, variant) in instruction.axes.iter().zip(chosen) {
            let (label, sections) = &axis.variants[variant];
            choices.push((axis.name.clone(), label.clone()));
            for (name, value) in sections {
                settings.replace(name, value.clone());
                rewritten.push(name.as_str());
            }
        }
        // A rewritten section and those after it are the candidate's own: the base's tags stay
        // where they are.
        let own = rewritten
            .iter()
            .any(|name| *name == "transform" || *name == "index");
        let tag = format!("{}/{number}", request.name);
        if rewritten.contains(&"transform") {
            settings.replace("transform", retagged(&settings, "transform", None));
        }
        if own {
            settings.replace("index", retagged(&settings, "index", Some(&tag)));
        }
        let name = choices
            .iter()
            .map(|(axis, label)| format!("{axis}={label}"))
            .collect::<Vec<_>>()
            .join(",");
        let tag = if own { Some(tag) } else { base_tag.clone() };
        let mut candidate = blank(number, name, choices, settings.to_text(), tag);
        let plan = match settings.plan() {
            Ok(plan) => Some(plan),
            Err(reason) => {
                candidate.state = State::Invalid(reason);
                candidate.tag = None;
                None
            }
        };
        candidates.push(candidate);
        plans.push(plan);
        owns.push(own);
    }

    // Candidates that ask for the same processing are one candidate, however they are written.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (candidate, plan) in candidates.iter_mut().zip(&plans) {
        let Some(plan) = plan else { continue };
        let identity = format!("{} {}", plan.chunking_process().id(), plan.retrieval().id());
        match seen.get(&identity) {
            Some(first) => {
                candidate.state = State::Same(*first);
                candidate.tag = None;
            }
            None => {
                seen.insert(identity, candidate.number);
                (candidate.expected, candidate.items) = ground.expect(plan)?;
            }
        }
    }

    // The cheapest first. What goes over the limit is left, and stays in the list.
    let mut order: Vec<usize> = candidates
        .iter()
        .filter(|candidate| candidate.state == State::Planned && candidate.number > 0)
        .map(|candidate| candidate.number)
        .collect();
    order.sort_by_key(|number| {
        let candidate = &candidates[*number];
        (candidate.expected.len(), candidate.items, *number)
    });
    if let Some(limit) = instruction.limit {
        for number in order.split_off(limit.min(order.len())) {
            candidates[number].state =
                State::Skipped(format!("The limit of {limit} candidates was reached."));
        }
    }
    if request.plan_only {
        return Ok(Optimization {
            run: None,
            candidates,
        });
    }

    let mut draft = store.begin_scan("optimize", "optimization")?;
    draft.set_config(request.instruction.as_bytes())?;
    for candidate in &candidates {
        draft.save(candidate.config.as_bytes())?;
    }
    // The runs of the candidates write objects too.
    draft.flush()?;
    let cause = draft.run().to_owned();
    order.insert(0, 0);
    for number in order {
        let plan = plans[number].as_ref().expect("a planned candidate");
        ground.run(&mut candidates[number], plan, owns[number], &cause)?;
    }
    draft.set_stats(json!({
        "name": request.name,
        "base": object_id(base_text.as_bytes()).as_str(),
        "data": instruction.data,
        "split": instruction.split,
        "metric": "recall",
        "candidates": candidates.iter().map(recorded).collect::<Vec<_>>(),
    }));
    Ok(Optimization {
        run: Some(draft.finish()?),
        candidates,
    })
}
