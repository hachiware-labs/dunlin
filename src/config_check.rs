//! Validation of MCP and RAG `KnowledgeNestfile.yaml` configurations (spec 26.8).
//!
//! The checks, their order, codes, paths and messages follow the JavaScript validation that
//! this replaced, so that `nest validate` reports the same diagnostics.

use std::collections::HashSet;

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};

use crate::config_value::ConfigValue;
use crate::node_path;
use crate::registry::{self, ArgInfo, ArgResult, ModuleInfo};

/// A path element: a key or a list position.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Key(String),
    Index(usize),
}

impl Part {
    fn text(&self) -> String {
        match self {
            Self::Key(key) => key.clone(),
            Self::Index(index) => index.to_string(),
        }
    }
}

impl Serialize for Part {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Key(key) => serializer.serialize_str(key),
            Self::Index(index) => serializer.serialize_u64(*index as u64),
        }
    }
}

macro_rules! path {
    ($($part:expr),* $(,)?) => { vec![$(Part::from($part)),*] };
}

impl From<&str> for Part {
    fn from(key: &str) -> Self {
        Self::Key(key.to_owned())
    }
}

impl From<String> for Part {
    fn from(key: String) -> Self {
        Self::Key(key)
    }
}

impl From<&String> for Part {
    fn from(key: &String) -> Self {
        Self::Key(key.clone())
    }
}

impl From<usize> for Part {
    fn from(index: usize) -> Self {
        Self::Index(index)
    }
}

/// What a diagnostic expected: text, a number, a list, or an argument description.
pub enum Expected {
    Text(String),
    Number(f64),
    List(Vec<String>),
    Arg(&'static registry::Arg),
}

impl From<&str> for Expected {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<String> for Expected {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl Serialize for Expected {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Text(text) => serializer.serialize_str(text),
            Self::Number(value) => ConfigValue::Number(*value).serialize(serializer),
            Self::List(items) => items.serialize(serializer),
            Self::Arg(arg) => ArgInfo { arg, speed: false }.serialize(serializer),
        }
    }
}

pub struct Diagnostic {
    pub code: &'static str,
    pub path: Vec<Part>,
    pub line: Option<usize>,
    pub module: Option<String>,
    pub received: Option<ConfigValue>,
    pub expected: Expected,
    pub suggestions: Vec<String>,
    pub hint: String,
}

/// Serializes a diagnostic for a file, as the JavaScript `diagnostic` helper shaped it.
pub struct FileDiagnostic<'a> {
    pub file: &'a str,
    pub diagnostic: &'a Diagnostic,
}

impl Serialize for FileDiagnostic<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let item = self.diagnostic;
        let section = match item.path.first() {
            Some(Part::Key(key)) => Some(key.clone()),
            Some(Part::Index(index)) => Some(index.to_string()),
            None => None,
        };
        let arg = match (item.path.get(3), item.path.get(4)) {
            (Some(Part::Key(with)), Some(arg)) if with == "with" => Some(arg.text()),
            _ => None,
        };
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("code", item.code)?;
        map.serialize_entry("file", self.file)?;
        map.serialize_entry("line", &item.line)?;
        map.serialize_entry("section", &section)?;
        map.serialize_entry("module", &item.module)?;
        map.serialize_entry("arg", &arg)?;
        map.serialize_entry("received", &item.received.as_ref().map(safe_value))?;
        map.serialize_entry("expected", &item.expected)?;
        map.serialize_entry("suggestions", &item.suggestions)?;
        map.serialize_entry("hint", &item.hint)?;
        map.end()
    }
}

/// Values that look like secrets are replaced; objects and lists become null.
fn safe_value(value: &ConfigValue) -> ConfigValue {
    match value {
        ConfigValue::String(text) if looks_secret(text) => ConfigValue::String("[redacted]".into()),
        ConfigValue::Null
        | ConfigValue::Bool(_)
        | ConfigValue::Number(_)
        | ConfigValue::NonFinite(_)
        | ConfigValue::String(_) => value.clone(),
        _ => ConfigValue::Null,
    }
}

/// `/^\$\{[A-Z0-9_]+\}$/i` or `/^(?:[^\s:/]+:\/\/|sk-|Bearer |gh[pousr]_)/i`
fn looks_secret(text: &str) -> bool {
    if let Some(name) = text
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        && !name.is_empty()
        && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
    {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("sk-") || lower.starts_with("bearer ") {
        return true;
    }
    if lower.len() >= 4
        && lower.starts_with("gh")
        && "pousr".contains(&lower[2..3])
        && &lower[3..4] == "_"
    {
        return true;
    }
    match text.find("://") {
        Some(index) if index > 0 => text[..index]
            .chars()
            .all(|c| !registry::js_space(c) && c != ':' && c != '/'),
        _ => false,
    }
}

/// Line lookups for diagnostics. Paths are compared with every element as text.
pub struct Lines(Vec<(Vec<String>, usize)>);

impl Lines {
    pub fn new(entries: Vec<(Vec<String>, usize)>) -> Self {
        Self(entries)
    }

    fn line(&self, path: &[Part]) -> Option<usize> {
        let text: Vec<String> = path.iter().map(Part::text).collect();
        (1..=text.len()).rev().find_map(|length| {
            self.0
                .iter()
                .find(|(candidate, _)| candidate.as_slice() == &text[..length])
                .map(|(_, line)| *line)
        })
    }
}

/// Collects diagnostics in the order the checks run.
pub struct Report<'a> {
    lines: &'a Lines,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}

pub struct Details {
    module: Option<String>,
    received: Option<ConfigValue>,
    suggestions: Vec<String>,
}

impl Details {
    pub fn none() -> Self {
        Self {
            module: None,
            received: None,
            suggestions: Vec::new(),
        }
    }

    fn module(name: &str) -> Self {
        Self {
            module: Some(name.to_owned()),
            ..Self::none()
        }
    }

    fn received(mut self, value: ConfigValue) -> Self {
        self.received = Some(value);
        self
    }

    fn suggestions(mut self, suggestions: Vec<String>) -> Self {
        self.suggestions = suggestions;
        self
    }
}

impl<'a> Report<'a> {
    pub fn new(lines: &'a Lines) -> Self {
        Self {
            lines,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn issue(
        &mut self,
        code: &'static str,
        path: Vec<Part>,
        expected: impl Into<Expected>,
        hint: impl Into<String>,
        details: Details,
    ) {
        let diagnostic = self.diagnostic(code, path, expected.into(), hint.into(), details);
        self.errors.push(diagnostic);
    }

    fn warn(
        &mut self,
        code: &'static str,
        path: Vec<Part>,
        expected: impl Into<Expected>,
        hint: impl Into<String>,
        details: Details,
    ) {
        let diagnostic = self.diagnostic(code, path, expected.into(), hint.into(), details);
        self.warnings.push(diagnostic);
    }

    fn diagnostic(
        &self,
        code: &'static str,
        path: Vec<Part>,
        expected: Expected,
        hint: String,
        details: Details,
    ) -> Diagnostic {
        Diagnostic {
            code,
            line: self.lines.line(&path),
            path,
            module: details.module,
            received: details.received,
            expected,
            suggestions: details.suggestions,
            hint,
        }
    }
}

fn is_mapping(value: Option<&ConfigValue>) -> bool {
    matches!(value, Some(ConfigValue::Object(_)))
}

fn entries(value: &ConfigValue) -> &[(String, ConfigValue)] {
    match value {
        ConfigValue::Object(entries) => entries,
        _ => &[],
    }
}

fn keys(value: &ConfigValue) -> Vec<String> {
    entries(value).iter().map(|(key, _)| key.clone()).collect()
}

/// Levenshtein distance, as the JavaScript `distance` helper computed it.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for ch in a.chars() {
        let mut next = vec![row[0] + 1];
        for i in 0..b.len() {
            next.push(
                (next[i] + 1)
                    .min(row[i + 1] + 1)
                    .min(row[i] + usize::from(ch != b[i])),
            );
        }
        row = next;
    }
    row[b.len()]
}

/// Up to three options within four edits, closest first.
fn nearby(name: &ConfigValue, options: &[String]) -> Vec<String> {
    let Some(name) = name.as_str() else {
        return Vec::new();
    };
    if name.encode_utf16().count() > 120 {
        return Vec::new();
    }
    let mut scored: Vec<(&String, usize)> = options
        .iter()
        .map(|option| (option, distance(name, option)))
        .filter(|(_, score)| *score <= 4)
        .collect();
    scored.sort_by_key(|(_, score)| *score);
    scored
        .into_iter()
        .take(3)
        .map(|(option, _)| option.clone())
        .collect()
}

/// Settings from the JavaScript side that the checks need.
pub struct Environment<'a> {
    /// The configuration file, used to resolve relative paths.
    pub file: &'a str,
    /// The tool names and versions bundled with this release.
    pub tools: &'a [(String, String)],
    pub cwd: String,
}

/// One configured module in the validated plan.
pub struct PlanEntry {
    pub module: &'static ModuleInfo,
    pub name: ConfigValue,
    /// Every defined argument in order; `None` stands for an absent optional argument.
    pub args: Vec<(&'static str, Option<ConfigValue>)>,
}

impl PlanEntry {
    fn arg(&self, name: &str) -> Option<&ConfigValue> {
        self.args
            .iter()
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| value.as_ref())
    }

    fn number(&self, name: &str) -> Option<f64> {
        match self.arg(name) {
            Some(ConfigValue::Number(value)) => Some(*value),
            _ => None,
        }
    }

    fn text(&self, name: &str) -> Option<&str> {
        self.arg(name).and_then(ConfigValue::as_str)
    }

    fn set(&mut self, name: &str, value: ConfigValue) {
        if let Some(entry) = self.args.iter_mut().find(|(key, _)| *key == name) {
            entry.1 = Some(value);
        }
    }
}

impl Serialize for PlanEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Args<'a>(&'a [(&'static str, Option<ConfigValue>)]);
        impl Serialize for Args<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(None)?;
                for (key, value) in self.0 {
                    if let Some(value) = value {
                        map.serialize_entry(key, value)?;
                    }
                }
                map.end()
            }
        }
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("use", self.module.name)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("args", &Args(&self.args))?;
        map.end()
    }
}

pub struct Validation {
    pub plan: Vec<(&'static str, Vec<PlanEntry>)>,
    pub serve: Option<ConfigValue>,
    pub eval: Option<ConfigValue>,
}

const SEQUENCES: [(&str, &[&str]); 5] = [
    ("source", &["source.filesystem", "loader.auto"]),
    ("transform", &["text.normalize", "chunk.length"]),
    ("index", &["index.keyword"]),
    (
        "search",
        &["query.normalize", "retrieval.keyword", "context.assemble"],
    ),
    ("ask", &["answer.generate"]),
];

fn required_roles(stage: &str) -> &'static [&'static str] {
    match stage {
        "source" => &["source", "parser"],
        "transform" => &["chunk"],
        "index" => &["index"],
        "search" => &["retrieval", "context"],
        _ => &["answer"],
    }
}

/// Validates an MCP or RAG configuration.
pub fn validate_rag(
    config: &ConfigValue,
    environment: &Environment,
    report: &mut Report,
) -> Validation {
    let project_type = config
        .get("init")
        .and_then(|init| init.get("type"))
        .and_then(ConfigValue::as_str)
        .unwrap_or_default()
        .to_owned();
    let stages: Vec<(&'static str, &'static [&'static str])> = SEQUENCES
        .iter()
        .copied()
        .filter(|(stage, _)| *stage != "ask" || project_type == "rag")
        .collect();
    for key in keys(config) {
        let known = [
            "init",
            "serve",
            "models",
            "tools",
            "eval-data",
            "eval",
            "trial_limits",
        ]
        .contains(&key.as_str())
            || stages.iter().any(|(stage, _)| *stage == key);
        if !known {
            report.issue(
                "ARG_UNKNOWN",
                path![&key],
                "A supported top-level section",
                "See the KnowledgeNestfile example in section 5.5.",
                Details::none().received(ConfigValue::String(key.clone())),
            );
        }
    }
    check_tools(config.get("tools"), environment.tools, report);
    check_trial_limits(config.get("trial_limits"), report);
    let init = config
        .get("init")
        .cloned()
        .unwrap_or(ConfigValue::Object(Vec::new()));
    for key in keys(&init) {
        if key != "version" && key != "type" {
            report.issue(
                "ARG_UNKNOWN",
                path!["init", &key],
                "version or type",
                "Remove the unsupported init key.",
                Details::none().received(ConfigValue::String(key.clone())),
            );
        }
    }
    check_models(config.get("models"), report);
    let version = init.get("version");
    if version != Some(&ConfigValue::Number(1.0)) {
        report.issue(
            "VERSION_UNSUPPORTED",
            path!["init", "version"],
            Expected::Number(1.0),
            "Generate a version 1 configuration with nest init.",
            Details {
                received: version.cloned(),
                ..Details::none()
            },
        );
    }
    let models = config.get("models");
    let tools = config.get("tools");
    let mut plan: Vec<(&'static str, Vec<PlanEntry>)> = Vec::new();
    for (stage, names) in &stages {
        let Some(section) = config.get(stage) else {
            report.issue(
                "SECTION_MISSING",
                path![*stage],
                *stage,
                format!("Generate the standard section with nest init {project_type} <directory>."),
                Details::none(),
            );
            continue;
        };
        let modules = match section.get("modules") {
            Some(ConfigValue::Array(items)) if is_mapping(Some(section)) => items,
            _ => {
                report.issue(
                    "ARG_TYPE",
                    path![*stage],
                    "A mapping with a modules list",
                    "Write modules as a list of use/with entries.",
                    Details::none(),
                );
                continue;
            }
        };
        for key in keys(section) {
            if key != "modules" {
                report.issue(
                    "ARG_UNKNOWN",
                    path![*stage, &key],
                    "modules",
                    "Move module arguments under with.",
                    Details::none().received(ConfigValue::String(key.clone())),
                );
            }
        }
        let mut role_count: Vec<(&'static str, usize)> = Vec::new();
        let count = |roles: &Vec<(&'static str, usize)>, role: &str| {
            roles
                .iter()
                .find(|(name, _)| *name == role)
                .map_or(0, |(_, n)| *n)
        };
        let mut used: HashSet<&'static str> = HashSet::new();
        let mut explicit: Vec<ConfigValue> = Vec::new();
        let mut current_type = registry::module(names[0]).map_or("", |module| module.input);
        let mut chain_valid = true;
        let mut entries_plan: Vec<PlanEntry> = Vec::new();
        for (i, entry) in modules.iter().enumerate() {
            let entry_path = path![*stage, "modules", i];
            let Some(name) = entry
                .get("use")
                .and_then(ConfigValue::as_str)
                .filter(|_| is_mapping(Some(entry)))
            else {
                report.issue(
                    "ARG_TYPE",
                    entry_path,
                    "A module entry containing a string use",
                    format!("Write use: {}.", names[0]),
                    Details::none(),
                );
                chain_valid = false;
                continue;
            };
            let with_path = |suffix: Vec<Part>| {
                let mut full = entry_path.clone();
                full.extend(suffix);
                full
            };
            let Some(module) = registry::module(name) else {
                let all: Vec<String> = registry::modules()
                    .iter()
                    .map(|module| module.name.to_owned())
                    .collect();
                report.issue(
                    "MODULE_UNKNOWN",
                    with_path(path!["use"]),
                    "A registered module name",
                    "Run nest modules list.",
                    Details::module(name)
                        .received(ConfigValue::String(name.into()))
                        .suggestions(nearby(&ConfigValue::String(name.into()), &all)),
                );
                chain_valid = false;
                continue;
            };
            if module.stage != *stage {
                report.issue(
                    "MODULE_WRONG_STAGE",
                    with_path(path!["use"]),
                    module.stage,
                    format!("Move the module to {}.", module.stage),
                    Details::module(name).received(ConfigValue::String(name.into())),
                );
                chain_valid = false;
                continue;
            }
            let parallel = ["source", "parser", "index", "retrieval"].contains(&module.role)
                && module.output == current_type
                && (module.role != "retrieval" || count(&role_count, "fusion") == 0);
            if module.input != current_type && chain_valid && !parallel {
                report.issue(
                    "TYPE_MISMATCH",
                    with_path(path!["use"]),
                    current_type,
                    "Order modules so outputs connect to the next input.",
                    Details::module(name).received(ConfigValue::String(module.input.into())),
                );
            }
            current_type = module.output;
            match role_count.iter_mut().find(|(role, _)| *role == module.role) {
                Some(entry) => entry.1 += 1,
                None => role_count.push((module.role, 1)),
            }
            if count(&role_count, module.role) > 1
                && ![
                    "source",
                    "parser",
                    "index",
                    "retrieval",
                    "preprocess",
                    "rerank",
                ]
                .contains(&module.role)
            {
                report.issue(
                    "ROLE_DUPLICATE",
                    with_path(path!["use"]),
                    format!("One {} module", module.role),
                    "Remove the duplicate module.",
                    Details::module(name),
                );
            }
            let alias = entry.get("as");
            let instance = alias
                .cloned()
                .unwrap_or_else(|| ConfigValue::String(name.into()));
            let blank = instance
                .as_str()
                .is_none_or(|text| text.trim_matches(registry::js_space).is_empty());
            if blank
                || explicit.contains(&instance)
                || (used.contains(module.name) && alias.is_none())
            {
                report.issue(
                    "ARG_CONFLICT",
                    with_path(path!["as"]),
                    "A unique module name or alias",
                    "Set a distinct as value on repeated modules.",
                    Details::module(name),
                );
            }
            used.insert(module.name);
            explicit.push(instance.clone());
            for key in keys(entry) {
                if !["use", "with", "as"].contains(&key.as_str()) {
                    report.issue(
                        "ARG_UNKNOWN",
                        with_path(path![&key]),
                        "use, with, or as",
                        "Use with for module arguments.",
                        Details::module(name).received(ConfigValue::String(key.clone())),
                    );
                }
            }
            let args = entry
                .get("with")
                .cloned()
                .unwrap_or(ConfigValue::Object(Vec::new()));
            if !is_mapping(Some(&args)) {
                report.issue(
                    "ARG_TYPE",
                    with_path(path!["with"]),
                    "An argument mapping",
                    "Write a mapping under with.",
                    Details::module(name),
                );
                continue;
            }
            let defined: Vec<String> = module.args.iter().map(|arg| arg.name.to_owned()).collect();
            for key in keys(&args) {
                if !defined.contains(&key) {
                    report.issue(
                        "ARG_UNKNOWN",
                        with_path(path!["with", &key]),
                        Expected::List(defined.clone()),
                        format!("Use an argument listed by nest modules describe {name}."),
                        Details::module(name)
                            .received(ConfigValue::String(key.clone()))
                            .suggestions(nearby(&ConfigValue::String(key.clone()), &defined)),
                    );
                }
            }
            let mut planned = PlanEntry {
                module,
                name: instance,
                args: Vec::new(),
            };
            for (arg, result) in registry::read_args(module, &args) {
                let arg_path = with_path(path!["with", arg.name]);
                match result {
                    ArgResult::Invalid(code) => {
                        report.issue(
                            code,
                            arg_path,
                            Expected::Arg(arg),
                            "Use a value allowed by the module argument definition.",
                            Details {
                                received: args.get(arg.name).cloned(),
                                ..Details::module(name)
                            },
                        );
                    }
                    ArgResult::Value { value, defaulted } => {
                        if value.is_some() && defaulted {
                            report.warn("ARG_DEFAULT", arg_path, Expected::Arg(arg),
                                "Write the default value explicitly to make the configuration reproducible.", Details::module(name));
                        }
                        planned.args.push((arg.name, value));
                    }
                }
            }
            module_checks(
                &mut planned,
                &entry_path,
                config,
                environment,
                &role_count,
                models,
                tools,
                report,
            );
            entries_plan.push(planned);
        }
        for role in required_roles(stage) {
            if count(&role_count, role) == 0 {
                let example = names
                    .iter()
                    .find(|name| registry::module(name).is_some_and(|module| module.role == *role))
                    .copied()
                    .unwrap_or_default();
                report.issue(
                    "ROLE_MISSING",
                    path![*stage, "modules"],
                    *role,
                    format!("Add the required module: {example}."),
                    Details::none(),
                );
            }
        }
        if *stage == "search" && current_type != "context" && count(&role_count, "context") > 0 {
            report.issue(
                "TYPE_MISMATCH",
                path![*stage, "modules"],
                "Context assembly as the final module",
                "Move context.assemble to the end.",
                Details::none(),
            );
        }
        plan.push((stage, entries_plan));
    }
    cross_checks(&plan, models, report);
    let serve = validate_serve(&project_type, config.get("serve"), report);
    let eval = validate_evaluation(config, report);
    Validation { plan, serve, eval }
}

#[allow(clippy::too_many_arguments)]
fn module_checks(
    planned: &mut PlanEntry,
    entry_path: &[Part],
    config: &ConfigValue,
    environment: &Environment,
    role_count: &[(&'static str, usize)],
    models: Option<&ConfigValue>,
    tools: Option<&ConfigValue>,
    report: &mut Report,
) {
    let module = planned.module;
    let name = module.name;
    let at = |suffix: Vec<Part>| {
        let mut full = entry_path.to_vec();
        full.extend(suffix);
        full
    };
    let has_tool = |tool: &str| match tools {
        Some(ConfigValue::Array(items)) => items
            .iter()
            .any(|item| item.get("name").and_then(ConfigValue::as_str) == Some(tool)),
        _ => false,
    };
    let model = |reference: Option<&str>| {
        reference.and_then(|reference| models.and_then(|models| models.get(reference)))
    };
    let field = |model: Option<&ConfigValue>, key: &str| {
        model
            .and_then(|model| model.get(key))
            .and_then(ConfigValue::as_str)
            .map(str::to_owned)
    };
    let truthy = |model: Option<&ConfigValue>, key: &str| {
        model
            .and_then(|model| model.get(key))
            .is_some_and(js_truthy)
    };
    let directory = node_path::dirname(environment.file);
    if name == "source.filesystem"
        && let Some(path) = planned.text("path").map(str::to_owned)
    {
        match resolve_config_value(&path, &format!("{name}.with.path"), false) {
            Ok(Some(value)) => planned.set(
                "path",
                ConfigValue::String(node_path::resolve(&directory, &value)),
            ),
            Ok(None) => {}
            Err(message) => report.issue(
                "REQUIREMENT_MISSING",
                at(path!["with", "path"]),
                "A resolvable local path",
                message,
                Details::module(name),
            ),
        }
    }
    if name == "chunk.length"
        && let (Some(overlap), Some(size)) = (planned.number("overlap"), planned.number("size"))
        && overlap >= size
    {
        report.issue(
            "ARG_CONFLICT",
            at(path!["with", "overlap"]),
            "overlap less than size",
            "Reduce overlap or increase size.",
            Details::module(name).received(ConfigValue::Number(overlap)),
        );
    }
    if (name == "loader.ocr" || planned.text("ocr") == Some("loader.ocr"))
        && !OCR_TOOLS.iter().any(|tool| has_tool(tool))
    {
        report.issue(
            "REQUIREMENT_MISSING",
            at(path!["use"]),
            "The ocr tool",
            "Run nest add ocr.",
            Details::module(name),
        );
    }
    if name == "loader.command" && planned.arg("command").is_some() {
        let (issues, warning) = check_command(planned, environment);
        for (field, expected, hint) in issues {
            let mut full = path!["with"];
            full.extend(field);
            report.issue(
                "REQUIREMENT_MISSING",
                at(full),
                expected,
                hint,
                Details::module(name),
            );
        }
        if let Some((field, expected, hint)) = warning {
            let mut full = path!["with"];
            full.extend(field);
            report.warn(
                "ARG_RECOMMENDED",
                at(full),
                expected,
                hint,
                Details::module(name),
            );
        }
    }
    if let Some(tool) = module.tool
        && !has_tool(tool)
    {
        report.issue(
            "REQUIREMENT_MISSING",
            at(path!["use"]),
            format!("The {tool} tool"),
            format!("Run nest add {tool}."),
            Details::module(name),
        );
    }
    if name == "index.keyword" {
        let tokenizer = planned.text("tokenizer").map(str::to_owned);
        if tokenizer.as_deref() == Some("trigram") {
            if planned
                .arg("short_query")
                .is_none_or(|value| *value == ConfigValue::Null)
            {
                planned.set("short_query", ConfigValue::String("bigram".into()));
            }
        } else if planned.text("short_query") == Some("bigram") {
            report.issue(
                "ARG_CONFLICT",
                at(path!["with", "short_query"]),
                "bigram with the trigram tokenizer",
                "Remove short_query or select the trigram tokenizer.",
                Details::module(name),
            );
        }
        let dictionary = planned
            .arg("user_dictionary")
            .filter(|value| js_truthy(value))
            .and_then(ConfigValue::as_str)
            .map(str::to_owned);
        if dictionary.is_some() && tokenizer.as_deref() != Some("lindera") {
            report.issue(
                "ARG_CONFLICT",
                at(path!["with", "user_dictionary"]),
                "A CSV with the lindera tokenizer",
                "Select lindera or remove user_dictionary.",
                Details::module(name),
            );
        }
        if let Some(dictionary) = dictionary {
            match resolve_config_value(&dictionary, &format!("{name}.with.user_dictionary"), false)
            {
                Ok(Some(value)) => planned.set(
                    "user_dictionary",
                    ConfigValue::String(node_path::resolve(&directory, &value)),
                ),
                Ok(None) => {}
                Err(message) => report.issue(
                    "REQUIREMENT_MISSING",
                    at(path!["with", "user_dictionary"]),
                    "A resolvable local CSV path",
                    message,
                    Details::module(name),
                ),
            }
        }
    }
    if name == "rerank.model"
        && let Some(reference) = planned.text("model").filter(|text| !text.is_empty())
    {
        let chosen = model(Some(reference));
        if field(chosen, "kind").as_deref() != Some("chat")
            || !truthy(chosen, "model")
            || !truthy(chosen, "provider")
        {
            report.issue(
                "REQUIREMENT_MISSING",
                at(path!["with", "model"]),
                "A named chat model",
                "Define a chat model under models.",
                Details::module(name),
            );
        }
        let retrievals = role_count
            .iter()
            .find(|(role, _)| *role == "retrieval")
            .map_or(0, |(_, n)| *n);
        let fusion = role_count
            .iter()
            .any(|(role, n)| *role == "fusion" && *n > 0);
        if retrievals > 1 && !fusion {
            report.issue(
                "ARG_CONFLICT",
                at(path!["use"]),
                "Fusion before reranking",
                "Place fusion.rrf before rerank.model.",
                Details::module(name),
            );
        }
    }
    if name == "answer.generate"
        && let Some(reference) = planned.text("model").filter(|text| !text.is_empty())
    {
        let chosen = model(Some(reference));
        if field(chosen, "kind").as_deref() != Some("chat") || !truthy(chosen, "provider") {
            report.issue(
                "REQUIREMENT_MISSING",
                at(path!["with", "model"]),
                "A named chat model",
                "Define the answer model under models.",
                Details::module(name),
            );
        }
    }
    if name == "chunk.semantic" {
        let reference = planned.text("model").filter(|text| !text.is_empty());
        let chosen = model(reference);
        if reference.is_some() && !embedding_model(chosen) {
            report.issue(
                "REQUIREMENT_MISSING",
                at(path!["with", "model"]),
                "An Ollama or OpenAI embedding model",
                "Define an embedding model under models.",
                Details::module(name),
            );
        }
        if let (Some(sentence), Some(max)) = (
            planned.number("sentence_chars"),
            planned.number("max_chars"),
        ) && sentence > max
        {
            report.issue(
                "ARG_CONFLICT",
                at(path!["with", "sentence_chars"]),
                "At most max_chars",
                "Reduce sentence_chars or increase max_chars.",
                Details::module(name),
            );
        }
    }
    let _ = config;
}

fn embedding_model(model: Option<&ConfigValue>) -> bool {
    let text = |key: &str| {
        model
            .and_then(|model| model.get(key))
            .and_then(ConfigValue::as_str)
    };
    text("kind") == Some("embedding")
        && model
            .and_then(|model| model.get("model"))
            .is_some_and(js_truthy)
        && matches!(text("provider"), Some("ollama" | "openai"))
}

fn js_truthy(value: &ConfigValue) -> bool {
    match value {
        ConfigValue::Null => false,
        ConfigValue::Bool(value) => *value,
        ConfigValue::Number(value) => *value != 0.0,
        ConfigValue::NonFinite(name) => name != "NaN",
        ConfigValue::String(text) => !text.is_empty(),
        _ => true,
    }
}

fn cross_checks(
    plan: &[(&'static str, Vec<PlanEntry>)],
    models: Option<&ConfigValue>,
    report: &mut Report,
) {
    let stage = |name: &str| {
        plan.iter()
            .find(|(stage, _)| *stage == name)
            .map(|(_, entries)| entries.as_slice())
    };
    let search = stage("search");
    let index = stage("index").unwrap_or_default();
    let context =
        search.and_then(|entries| entries.iter().find(|entry| entry.module.role == "context"));
    let retrievals: Vec<&PlanEntry> = search
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.module.role == "retrieval")
                .collect()
        })
        .unwrap_or_default();
    let normalized_text = stage("transform")
        .and_then(|entries| {
            entries
                .iter()
                .rev()
                .find(|entry| entry.module.name == "text.normalize")
        })
        .and_then(|entry| entry.text("unicode").map(str::to_owned))
        .unwrap_or_else(|| "none".into());
    let normalized_query = search
        .and_then(|entries| {
            entries
                .iter()
                .rev()
                .find(|entry| entry.module.name == "query.normalize")
        })
        .and_then(|entry| entry.text("unicode").map(str::to_owned))
        .unwrap_or_else(|| "none".into());
    let fusion =
        search.is_some_and(|entries| entries.iter().any(|entry| entry.module.role == "fusion"));
    if fusion != (retrievals.len() > 1) {
        report.issue(
            "ARG_CONFLICT",
            path!["search", "modules"],
            "fusion.rrf after two or more retrieval modules",
            if retrievals.len() > 1 {
                "Add fusion.rrf before context.assemble."
            } else {
                "Remove fusion.rrf or add another retrieval module."
            },
            Details::none(),
        );
    }
    let vectors: Vec<&PlanEntry> = index
        .iter()
        .filter(|entry| ["index.vector", "hannoy.index"].contains(&entry.module.name))
        .collect();
    if vectors.len() > 1
        || index
            .iter()
            .filter(|entry| entry.module.name == "index.keyword")
            .count()
            > 1
    {
        report.issue(
            "ROLE_DUPLICATE",
            path!["index", "modules"],
            "One vector backend and one keyword index",
            "Choose a vector backend for this build.",
            Details::none(),
        );
    }
    if let Some(vector) = vectors.first()
        && let Some(reference) = vector.text("model").filter(|text| !text.is_empty())
        && !embedding_model(models.and_then(|models| models.get(reference)))
    {
        let position = index
            .iter()
            .position(|entry| std::ptr::eq(entry, *vector))
            .unwrap_or_default();
        report.issue(
            "REQUIREMENT_MISSING",
            path!["index", "modules", position, "with", "model"],
            "An Ollama or OpenAI embedding model",
            "Define the embedding model under models.",
            Details::module(vector.module.name),
        );
    }
    for retrieval in &retrievals {
        let target = match retrieval.module.name {
            "retrieval.vector" => Some("index.vector"),
            "hannoy.search" => Some("hannoy.index"),
            _ => None,
        };
        let compatible = match target {
            Some(target) => vectors.iter().any(|vector| {
                vector.module.name == target && vector.arg("model") == retrieval.arg("model")
            }),
            None => index
                .iter()
                .any(|entry| entry.module.name == "index.keyword"),
        };
        if !compatible {
            report.issue(
                "REQUIREMENT_MISSING",
                path!["search", "modules"],
                Expected::List(
                    retrieval
                        .module
                        .requires
                        .iter()
                        .map(|item| (*item).to_owned())
                        .collect(),
                ),
                "Define the matching index and model.",
                Details::module(retrieval.module.name),
            );
        }
    }
    if normalized_text != normalized_query {
        report.issue(
            "REQUIREMENT_MISSING",
            path!["search", "modules"],
            normalized_text,
            "Use the same Unicode normalization for text and queries.",
            Details::module("query.normalize"),
        );
    }
    if let Some(context) = context
        && context.text("retrieval") == Some("fill_budget")
        && let Some(max) = context.number("max_candidates")
        && retrievals
            .iter()
            .any(|retrieval| retrieval.number("top_k").is_some_and(|top| max < top))
    {
        let position = search
            .and_then(|entries| {
                entries
                    .iter()
                    .position(|entry| entry.module.role == "context")
            })
            .unwrap_or_default();
        report.issue(
            "ARG_CONFLICT",
            path!["search", "modules", position, "with", "max_candidates"],
            "At least each retrieval top_k",
            "Increase max_candidates or decrease top_k.",
            Details::module("context.assemble").received(ConfigValue::Number(max)),
        );
    }
}

/// `resolveConfigValue`: replaces `${NAME}` with environment variables.
/// `Ok(None)` stands for a missing variable or blank result when `allow_missing` is set.
pub fn resolve_config_value(
    value: &str,
    field: &str,
    allow_missing: bool,
) -> Result<Option<String>, String> {
    let mut resolved = String::new();
    let mut missing: Option<String> = None;
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let name_end = after.find('}');
        let valid = name_end.is_some_and(|end| registry::environment_name(&after[..end]));
        if !valid {
            resolved.push_str(&rest[..start + 2]);
            rest = after;
            continue;
        }
        let end = name_end.unwrap_or_default();
        let name = &after[..end];
        resolved.push_str(&rest[..start]);
        match std::env::var(name) {
            Ok(variable) => resolved.push_str(&variable),
            Err(_) => {
                missing.get_or_insert_with(|| name.to_owned());
            }
        }
        rest = &after[end + 1..];
    }
    resolved.push_str(rest);
    if let Some(name) = missing {
        if allow_missing {
            return Ok(None);
        }
        return Err(format!(
            "Environment variable {name} is required by {field}"
        ));
    }
    if allow_missing && resolved.trim_matches(registry::js_space).is_empty() {
        return Ok(None);
    }
    if resolved.trim_matches(registry::js_space).is_empty() || resolved.contains('\0') {
        return Err(format!("Expected {field} to be a nonempty string"));
    }
    Ok(Some(resolved))
}

type CommandIssue = (Vec<Part>, String, String);

/// The `loader.command` checks that argument definitions cannot express (spec 6.7).
fn check_command(
    planned: &mut PlanEntry,
    environment: &Environment,
) -> (Vec<CommandIssue>, Option<CommandIssue>) {
    let mut issues: Vec<CommandIssue> = Vec::new();
    let list = |planned: &PlanEntry, key: &str| match planned.arg(key) {
        Some(ConfigValue::Array(items)) => Some(
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect::<Vec<_>>(),
        ),
        _ => None,
    };
    let command = list(planned, "command");
    let version_command = list(planned, "version_command");
    for (field, parts) in [("command", &command), ("version_command", &version_command)] {
        let Some(parts) = parts else { continue };
        for (index, part) in parts.iter().enumerate() {
            for name in placeholders(part).into_iter().flatten() {
                if field == "version_command"
                    || index == 0
                    || !["input", "name", "ext"].contains(&name.as_str())
                {
                    issues.push((
                        path![field, index],
                        if field == "command" {
                            "{input}, {name} or {ext}"
                        } else {
                            "No placeholders"
                        }
                        .into(),
                        "Write a literal brace as {{ or }}.".into(),
                    ));
                }
            }
        }
    }
    let uses_input = command.as_ref().is_some_and(|parts| {
        parts.iter().any(|part| {
            placeholders(part)
                .iter()
                .any(|name| name.as_deref() == Some("input"))
        })
    });
    let input = planned.text("input").map(str::to_owned);
    if input.as_deref() == Some("file") && !uses_input {
        issues.push((
            path!["command"],
            "{input} with input: file".into(),
            "Pass the file path with {input}, or set input: stdin.".into(),
        ));
    }
    if input.as_deref() == Some("stdin") && uses_input {
        issues.push((
            path!["command"],
            "No {input} with input: stdin".into(),
            "Remove {input}, or set input: file.".into(),
        ));
    }
    if planned.arg("version").is_some() && planned.arg("version_command").is_some() {
        issues.push((
            path!["version_command"],
            "Either version or version_command".into(),
            "Remove one of them.".into(),
        ));
    }
    if let Some(env) = planned.arg("env").cloned() {
        for (key, value) in entries(&env) {
            if let Some(text) = value.as_str()
                && let Err(message) =
                    resolve_config_value(text, &format!("loader.command.with.env.{key}"), true)
            {
                issues.push((
                    path!["env", key],
                    "A resolvable environment value".into(),
                    message,
                ));
            }
        }
    }
    for (field, parts) in [("command", command), ("version_command", version_command)] {
        let Some(first) = parts.and_then(|parts| parts.into_iter().next()) else {
            continue;
        };
        match find_executable(&first, environment) {
            Ok(found) => {
                if let Some(ConfigValue::Array(items)) = planned
                    .args
                    .iter_mut()
                    .find(|(key, _)| *key == field)
                    .and_then(|(_, value)| value.as_mut())
                    && let Some(item) = items.first_mut()
                {
                    *item = ConfigValue::String(found);
                }
            }
            Err(message) => {
                issues.push((path![field, 0usize], "An executable file".into(), message))
            }
        }
    }
    let warning = (planned.arg("version").is_none() && planned.arg("version_command").is_none()).then(|| {
        (path!["version"], "version or version_command".to_owned(),
            "Without a version, Knowledge-Nest cannot detect tool updates. Run nest build --force after updating the tool.".to_owned())
    });
    (issues, warning)
}

/// The `/\{\{|\}\}|\{([^{}]*)\}/g` matches of a command part: `Some(name)` for a placeholder,
/// `None` for an escaped brace.
fn placeholders(part: &str) -> Vec<Option<String>> {
    let chars: Vec<char> = part.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' && chars.get(i + 1) == Some(&'{')
            || chars[i] == '}' && chars.get(i + 1) == Some(&'}')
        {
            found.push(None);
            i += 2;
        } else if chars[i] == '{' {
            match chars[i + 1..].iter().position(|c| *c == '{' || *c == '}') {
                Some(offset) if chars[i + 1 + offset] == '}' => {
                    found.push(Some(chars[i + 1..i + 1 + offset].iter().collect()));
                    i += offset + 2;
                }
                _ => i += 1,
            }
        } else {
            i += 1;
        }
    }
    found
}

fn find_executable(command: &str, environment: &Environment) -> Result<String, String> {
    let windows = cfg!(windows);
    let path_like = node_path::is_absolute(command) || command.contains(['\\', '/']);
    let base = if environment.file.is_empty() {
        environment.cwd.clone()
    } else {
        node_path::dirname(environment.file)
    };
    let candidates: Vec<String> = if path_like {
        vec![node_path::resolve(&base, command)]
    } else {
        let path = std::env::var("PATH").unwrap_or_default();
        let separator = if windows { ';' } else { ':' };
        path.split(separator)
            .filter(|part| !part.is_empty())
            .map(|part| node_path::resolve(part, command))
            .collect()
    };
    let has_extension = std::path::Path::new(command).extension().is_some();
    let extensions: Vec<String> = if windows && !has_extension {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|part| !part.is_empty())
            .map(str::to_ascii_lowercase)
            .collect()
    } else {
        vec![String::new()]
    };
    let mut batch: Option<String> = None;
    for candidate in &candidates {
        for extension in &extensions {
            let path = format!("{candidate}{extension}");
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if !metadata.is_file() || !executable(&metadata) {
                continue;
            }
            let lower = path.to_ascii_lowercase();
            if windows && (lower.ends_with(".bat") || lower.ends_with(".cmd")) {
                batch.get_or_insert(path);
                continue;
            }
            return Ok(path);
        }
    }
    if let Some(batch) = batch {
        return Err(format!(
            "{batch} is a batch file, which needs a shell. Call its interpreter directly, for example [\"node\", \"tool.mjs\", \"{{input}}\"]."
        ));
    }
    Err(format!(
        "{} was not found.",
        if path_like {
            candidates[0].clone()
        } else {
            format!("{command} on PATH")
        }
    ))
}

#[cfg(unix)]
fn executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// A zod-like issue: its code and path inside the checked value.
struct Issue {
    code: &'static str,
    path: Vec<Part>,
}

fn issue(code: &'static str, path: Vec<Part>) -> Issue {
    Issue { code, path }
}

/// Integer checks: non-integers are type errors; values past the safe integer range report twice.
fn integer_issues(
    value: Option<&ConfigValue>,
    min: f64,
    max: f64,
    path: Vec<Part>,
    issues: &mut Vec<Issue>,
) {
    match value {
        None => {}
        Some(ConfigValue::Number(number)) if number.fract() == 0.0 => {
            const SAFE: f64 = 9_007_199_254_740_991.0;
            if *number < -SAFE {
                issues.push(issue("too_small", path.clone()));
            }
            if *number > SAFE {
                issues.push(issue("too_big", path.clone()));
            }
            if *number < min {
                issues.push(issue("too_small", path));
            } else if *number > max {
                issues.push(issue("too_big", path));
            }
        }
        Some(_) => issues.push(issue("invalid_type", path)),
    }
}

/// Reports keys outside `known` as one `unrecognized_keys` issue, as zod's strict objects do.
fn unknown_keys(
    value: &ConfigValue,
    known: &[&str],
    path: Vec<Part>,
    issues: &mut Vec<Issue>,
) -> Vec<String> {
    let unknown: Vec<String> = keys(value)
        .into_iter()
        .filter(|key| !known.contains(&key.as_str()))
        .collect();
    if !unknown.is_empty() {
        issues.push(issue("unrecognized_keys", path));
    }
    unknown
}

fn check_models(models: Option<&ConfigValue>, report: &mut Report) {
    let Some(models) = models else { return };
    if !is_mapping(Some(models)) {
        report.issue(
            "ARG_TYPE",
            path!["models"],
            "Named model definitions",
            "Write each model as a mapping.",
            Details::none(),
        );
        return;
    }
    for (name, value) in entries(models) {
        let mut issues = Vec::new();
        if is_mapping(Some(value)) {
            let field = |key: &str| value.get(key);
            if !matches!(
                field("kind").and_then(ConfigValue::as_str),
                Some("chat" | "embedding")
            ) {
                issues.push(issue("invalid_value", path!["kind"]));
            }
            if field("provider").is_some()
                && !matches!(
                    field("provider").and_then(ConfigValue::as_str),
                    Some("openai" | "ollama" | "strata")
                )
            {
                issues.push(issue("invalid_value", path!["provider"]));
            }
            if let Some(model) = field("model")
                && !matches!(model, ConfigValue::Null | ConfigValue::String(_))
            {
                issues.push(issue("invalid_type", path!["model"]));
            }
            if let Some(url) = field("base_url")
                && url.as_str().is_none()
            {
                issues.push(issue("invalid_type", path!["base_url"]));
            }
            match field("api_key_env") {
                Some(ConfigValue::String(text)) if !registry::environment_name(text) => {
                    issues.push(issue("invalid_format", path!["api_key_env"]))
                }
                Some(ConfigValue::String(_)) | None => {}
                Some(_) => issues.push(issue("invalid_type", path!["api_key_env"])),
            }
            integer_issues(
                field("max_input_bytes"),
                1.0,
                f64::INFINITY,
                path!["max_input_bytes"],
                &mut issues,
            );
            unknown_keys(
                value,
                &[
                    "kind",
                    "provider",
                    "model",
                    "base_url",
                    "api_key_env",
                    "max_input_bytes",
                ],
                Vec::new(),
                &mut issues,
            );
        } else {
            issues.push(issue("invalid_type", Vec::new()));
        }
        for item in issues {
            let mut full = path!["models", name];
            full.extend(item.path);
            report.issue(
                if item.code == "unrecognized_keys" {
                    "ARG_UNKNOWN"
                } else {
                    "ARG_TYPE"
                },
                full,
                "A typed chat or embedding model with an environment variable for credentials",
                "Check the model definition and keep credentials in the environment.",
                Details::none(),
            );
        }
        if let Some(text) = value.get("base_url").and_then(ConfigValue::as_str)
            && let Ok(url) = url::Url::parse(text)
            && (!["http", "https"].contains(&url.scheme())
                || !url.username().is_empty()
                || url.password().is_some_and(|password| !password.is_empty())
                || url.query().is_some_and(|query| !query.is_empty())
                || url.fragment().is_some_and(|fragment| !fragment.is_empty()))
        {
            report.issue(
                "ARG_RANGE",
                path!["models", name, "base_url"],
                "A credential-free HTTP(S) endpoint",
                "Use api_key_env for credentials.",
                Details::none(),
            );
        }
    }
}

/// Checks the `tools` list against the bundled catalog.
/// The OCR tools. `ocr` is the former name of `lightonocr`. A project uses at most one.
pub const OCR_TOOLS: [&str; 3] = ["oar-ocr", "lightonocr", "ocr"];

pub fn check_tools(tools: Option<&ConfigValue>, catalog: &[(String, String)], report: &mut Report) {
    let Some(tools) = tools else { return };
    let ConfigValue::Array(items) = tools else {
        report.issue(
            "ARG_TYPE",
            path!["tools"],
            "A list of tool names and versions",
            "Use nest add --list.",
            Details::none(),
        );
        return;
    };
    let mut names: Vec<String> = Vec::new();
    let mut ocr = false;
    for (i, entry) in items.iter().enumerate() {
        let name = entry.get("name").and_then(ConfigValue::as_str);
        let version = entry.get("version");
        let tool = name.and_then(|name| catalog.iter().find(|(tool, _)| tool == name));
        let valid = tool.is_some_and(|(_, expected)| {
            version.and_then(ConfigValue::as_str) == Some(expected.as_str())
        }) && keys(entry)
            .iter()
            .all(|key| key == "name" || key == "version")
            && !names.iter().any(|seen| Some(seen.as_str()) == name);
        if valid
            && name.is_some_and(|name| OCR_TOOLS.contains(&name))
            && std::mem::replace(&mut ocr, true)
        {
            report.issue(
                "ARG_CONFLICT",
                path!["tools", i],
                "One OCR tool",
                "Remove the other OCR tool with nest remove.",
                Details::none(),
            );
        } else if valid {
            names.push(name.unwrap_or_default().to_owned());
        } else {
            report.issue(
                "REQUIREMENT_MISSING",
                path!["tools", i],
                "A unique tool and version from the bundled catalog",
                "Use nest add --list.",
                Details::none(),
            );
        }
    }
}

pub const TRIAL_LIMITS: [&str; 4] = [
    "max_runs",
    "max_elapsed_ms",
    "max_api_requests",
    "max_api_input_bytes",
];

/// `trial_limits` holds nonnegative integer limits.
pub fn trial_limits_valid(limits: &ConfigValue) -> bool {
    let mut issues = Vec::new();
    if !is_mapping(Some(limits)) {
        return false;
    }
    for key in TRIAL_LIMITS {
        integer_issues(limits.get(key), 0.0, f64::INFINITY, Vec::new(), &mut issues);
    }
    unknown_keys(limits, &TRIAL_LIMITS, Vec::new(), &mut issues);
    issues.is_empty()
}

fn check_trial_limits(limits: Option<&ConfigValue>, report: &mut Report) {
    if let Some(limits) = limits
        && !trial_limits_valid(limits)
    {
        report.issue(
            "ARG_TYPE",
            path!["trial_limits"],
            Expected::List(TRIAL_LIMITS.iter().map(|key| (*key).to_owned()).collect()),
            "Set nonnegative integer limits for experimental runs.",
            Details::none(),
        );
    }
}

fn validate_evaluation(config: &ConfigValue, report: &mut Report) -> Option<ConfigValue> {
    if let Some(data) = config.get("eval-data") {
        let valid = is_mapping(Some(data))
            && keys(data).iter().all(|key| key == "use")
            && data.get("use").and_then(ConfigValue::as_str).is_some();
        if !valid {
            report.issue(
                "ARG_TYPE",
                path!["eval-data"],
                "A mapping with a dataset reference in use",
                "Select data with nest eval-data use.",
                Details::none(),
            );
        }
    }
    let settings = config.get("eval")?;
    let Some(ConfigValue::Array(modules)) = settings
        .get("modules")
        .filter(|_| is_mapping(Some(settings)))
    else {
        report.issue(
            "ARG_TYPE",
            path!["eval"],
            "A split and modules list",
            "See section 5.5 for the evaluation configuration.",
            Details::none(),
        );
        return None;
    };
    for key in keys(settings) {
        if !["split", "modules", "include_generated"].contains(&key.as_str()) {
            report.issue(
                "ARG_UNKNOWN",
                path!["eval", &key],
                "split, modules or include_generated",
                "Remove the unknown key.",
                Details::none().received(ConfigValue::String(key.clone())),
            );
        }
    }
    if settings.get("split").and_then(ConfigValue::as_str) != Some("dev") {
        report.issue(
            "ARG_RANGE",
            path!["eval", "split"],
            "dev",
            "Use --split test explicitly to run the test split.",
            Details::none(),
        );
    }
    let include = settings.get("include_generated");
    if include.is_some_and(|value| !matches!(value, ConfigValue::Bool(_))) {
        report.issue(
            "ARG_TYPE",
            path!["eval", "include_generated"],
            "A boolean",
            "Set true or false.",
            Details::none(),
        );
    }
    let include = include.filter(|value| **value != ConfigValue::Null);
    let mut output: Vec<(String, ConfigValue)> = vec![(
        "includeGenerated".into(),
        include.cloned().unwrap_or(ConfigValue::Bool(false)),
    )];
    let mut seen: Vec<String> = Vec::new();
    for (i, item) in modules.iter().enumerate() {
        let item_path = path!["eval", "modules", i];
        let name = item.get("use");
        let name_text = name
            .and_then(ConfigValue::as_str)
            .filter(|name| ["metric.evidence_recall", "metric.latency"].contains(name));
        let Some(name_text) = name_text else {
            report.issue(
                "MODULE_UNKNOWN",
                item_path,
                "metric.evidence_recall or metric.latency",
                "Use a supported evaluation module.",
                Details {
                    received: name.cloned(),
                    ..Details::none()
                },
            );
            continue;
        };
        if seen.iter().any(|seen| seen == name_text) {
            report.issue(
                "ROLE_DUPLICATE",
                item_path.clone(),
                format!("A single {name_text}"),
                "Remove duplicates.",
                Details::module(name_text),
            );
        }
        seen.push(name_text.to_owned());
        for key in keys(item) {
            if key != "use" && key != "with" {
                let mut full = item_path.clone();
                full.push(Part::Key(key.clone()));
                report.issue(
                    "ARG_UNKNOWN",
                    full,
                    "use or with",
                    "Remove the unknown key.",
                    Details::module(name_text).received(ConfigValue::String(key.clone())),
                );
            }
        }
        let args = item.get("with");
        let issues = if name_text == "metric.evidence_recall" {
            recall_issues(args)
        } else {
            latency_issues(args)
        };
        if issues.is_empty() {
            let args = args.cloned().unwrap_or(ConfigValue::Null);
            if name_text == "metric.evidence_recall" {
                // zod returns parsed objects with keys in schema order.
                let ordered = ["at", "short_evidence_chars", "min_coverage", "unresolved"]
                    .iter()
                    .filter_map(|key| {
                        args.get(key)
                            .map(|value| ((*key).to_owned(), value.clone()))
                    })
                    .collect();
                output.push(("recall".into(), ConfigValue::Object(ordered)));
            } else {
                output.push((
                    "percentiles".into(),
                    args.get("percentiles")
                        .cloned()
                        .unwrap_or(ConfigValue::Null),
                ));
            }
        }
        for found in issues {
            let mut full = item_path.clone();
            full.push(Part::Key("with".into()));
            full.extend(found.path);
            report.issue(
                if found.code == "unrecognized_keys" {
                    "ARG_UNKNOWN"
                } else {
                    "ARG_TYPE"
                },
                full,
                format!("Valid {name_text} arguments"),
                "See section 5.5.",
                Details::module(name_text),
            );
        }
    }
    for name in ["metric.evidence_recall", "metric.latency"] {
        if !seen.iter().any(|seen| seen == name) {
            report.issue(
                "ROLE_MISSING",
                path!["eval", "modules"],
                name,
                format!("Add {name}."),
                Details::none(),
            );
        }
    }
    // Later keys replace earlier ones, as repeated assignments do in JavaScript.
    let mut merged: Vec<(String, ConfigValue)> = Vec::new();
    for (key, value) in output {
        match merged.iter_mut().find(|(existing, _)| *existing == key) {
            Some(entry) => entry.1 = value,
            None => merged.push((key, value)),
        }
    }
    Some(ConfigValue::Object(merged))
}

fn number_list_issues(
    value: Option<&ConfigValue>,
    path: Vec<Part>,
    item: impl Fn(&ConfigValue, Vec<Part>, &mut Vec<Issue>),
    issues: &mut Vec<Issue>,
) {
    match value {
        Some(ConfigValue::Array(items)) => {
            for (index, value) in items.iter().enumerate() {
                let mut item_path = path.clone();
                item_path.push(Part::Index(index));
                item(value, item_path, issues);
            }
            if items.is_empty() {
                issues.push(issue("too_small", path));
            }
        }
        _ => issues.push(issue("invalid_type", path)),
    }
}

fn recall_issues(args: Option<&ConfigValue>) -> Vec<Issue> {
    let mut issues = Vec::new();
    let Some(args) = args.filter(|args| is_mapping(Some(args))) else {
        return vec![issue("invalid_type", Vec::new())];
    };
    number_list_issues(
        args.get("at"),
        path!["at"],
        |value, path, issues| match value {
            ConfigValue::String(text) if text == "context" => {}
            ConfigValue::Number(number) if number.fract() == 0.0 => {
                integer_issues(Some(value), 1.0, 100.0, path, issues)
            }
            _ => issues.push(issue("invalid_union", path)),
        },
        &mut issues,
    );
    match args.get("short_evidence_chars") {
        None => issues.push(issue("invalid_type", path!["short_evidence_chars"])),
        value => integer_issues(
            value,
            1.0,
            f64::INFINITY,
            path!["short_evidence_chars"],
            &mut issues,
        ),
    }
    match args.get("min_coverage") {
        Some(ConfigValue::Number(value)) => {
            if *value < 0.0 {
                issues.push(issue("too_small", path!["min_coverage"]));
            } else if *value > 1.0 {
                issues.push(issue("too_big", path!["min_coverage"]));
            }
        }
        _ => issues.push(issue("invalid_type", path!["min_coverage"])),
    }
    if args.get("unresolved").and_then(ConfigValue::as_str) != Some("count_as_missed") {
        issues.push(issue("invalid_value", path!["unresolved"]));
    }
    unknown_keys(
        args,
        &["at", "short_evidence_chars", "min_coverage", "unresolved"],
        Vec::new(),
        &mut issues,
    );
    issues
}

fn latency_issues(args: Option<&ConfigValue>) -> Vec<Issue> {
    let mut issues = Vec::new();
    let Some(args) = args.filter(|args| is_mapping(Some(args))) else {
        return vec![issue("invalid_type", Vec::new())];
    };
    number_list_issues(
        args.get("percentiles"),
        path!["percentiles"],
        |value, path, issues| {
            integer_issues(Some(value), 1.0, 100.0, path, issues);
        },
        &mut issues,
    );
    unknown_keys(args, &["percentiles"], Vec::new(), &mut issues);
    issues
}

const ENDPOINTS_RAG: [&str; 3] = ["responses", "search", "mcp"];
const ENDPOINTS_PAGES: [&str; 1] = ["web"];

fn endpoints(project_type: &str) -> &'static [&'static str] {
    match project_type {
        "rag" => &ENDPOINTS_RAG,
        "wiki" | "para" => &ENDPOINTS_PAGES,
        _ => &[],
    }
}

fn search_project(project_type: &str) -> bool {
    project_type == "mcp" || project_type == "rag"
}

/// The serve section `init` writes for a project type.
pub fn serve_template(project_type: &str) -> ConfigValue {
    if project_type == "mcp" {
        return ConfigValue::Object(vec![(
            "transport".into(),
            ConfigValue::String("stdio".into()),
        )]);
    }
    let mut entries = vec![
        ("transport".into(), ConfigValue::String("http".into())),
        ("host".into(), ConfigValue::String("127.0.0.1".into())),
        (
            "port".into(),
            ConfigValue::Number(if project_type == "rag" {
                4318.0
            } else {
                4319.0
            }),
        ),
        (
            "endpoints".into(),
            ConfigValue::Array(
                endpoints(project_type)
                    .iter()
                    .map(|name| ConfigValue::String((*name).into()))
                    .collect(),
            ),
        ),
        ("access".into(), default_access()),
        (
            "limits".into(),
            ConfigValue::Object(vec![
                ("max_request_bytes".into(), ConfigValue::Number(1_048_576.0)),
                ("request_timeout_seconds".into(), ConfigValue::Number(120.0)),
                ("max_concurrent".into(), ConfigValue::Number(8.0)),
            ]),
        ),
    ];
    if !search_project(project_type) {
        entries.push(("pages".into(), ConfigValue::String("./site".into())));
    }
    ConfigValue::Object(entries)
}

fn default_access() -> ConfigValue {
    ConfigValue::Object(vec![
        (
            "allowed_hosts".into(),
            ConfigValue::Array(vec![
                ConfigValue::String("localhost".into()),
                ConfigValue::String("127.0.0.1".into()),
            ]),
        ),
        ("allowed_origins".into(), ConfigValue::Array(Vec::new())),
        (
            "auth".into(),
            ConfigValue::Object(vec![
                ("type".into(), ConfigValue::String("none".into())),
                ("token_env".into(), ConfigValue::Null),
            ]),
        ),
    ])
}

/// `net.isIP`: 4, 6 or 0.
pub fn ip_version(value: &str) -> u8 {
    if value.parse::<std::net::Ipv4Addr>().is_ok() {
        return 4;
    }
    let (address, zone) = match value.split_once('%') {
        Some((address, zone)) => (address, Some(zone)),
        None => (value, None),
    };
    let zone_valid = zone.is_none_or(|zone| {
        !zone.is_empty()
            && zone
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-.:".contains(c))
    });
    if zone_valid && address.parse::<std::net::Ipv6Addr>().is_ok() {
        return 6;
    }
    0
}

/// A host name or address accepted in `allowed_hosts`, in lower case.
pub fn hostname(value: &str) -> Option<String> {
    if value.is_empty()
        || value
            .chars()
            .any(|c| registry::js_space(c) || "/@?#\\,%".contains(c))
    {
        return None;
    }
    let host = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(value)
        .to_lowercase();
    if ip_version(&host) != 0 {
        return Some(host);
    }
    let bytes = host.as_bytes();
    let domain = !bytes.is_empty()
        && host.chars().count() <= 253
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    let parsed = url::Url::parse(&format!("http://{host}")).ok();
    (domain && parsed.as_ref().and_then(url::Url::host_str) == Some(host.as_str())).then_some(host)
}

fn origin(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        ["http", "https"].contains(&url.scheme()) && url.origin().ascii_serialization() == value
    })
}

pub fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "::1" || (ip_version(host) == 4 && host.starts_with("127."))
}

/// An address or a range of addresses (`192.168.11.0/24`) of `allowed_clients`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    address: std::net::IpAddr,
    prefix: u32,
}

impl Network {
    /// An address, or an address and the number of leading bits a client shares with it.
    pub fn parse(value: &str) -> Option<Self> {
        let (address, prefix) = match value.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (value, None),
        };
        let address = address.parse::<std::net::IpAddr>().ok()?.to_canonical();
        let bits = if address.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => bits,
            Some(digits) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
                digits
                    .parse::<u32>()
                    .ok()
                    .filter(|prefix| *prefix <= bits)?
            }
            Some(_) => return None,
        };
        Some(Self { address, prefix })
    }

    pub fn contains(&self, address: std::net::IpAddr) -> bool {
        use std::net::IpAddr;
        match (self.address, address.to_canonical()) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                let mask = u32::MAX.checked_shl(32 - self.prefix).unwrap_or(0);
                u32::from(network) & mask == u32::from(address) & mask
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                let mask = u128::MAX.checked_shl(128 - self.prefix).unwrap_or(0);
                u128::from(network) & mask == u128::from(address) & mask
            }
            _ => false,
        }
    }
}

/// The ranges of a home or an office network, and of the machine itself: private, link-local
/// and loopback addresses.
const PRIVATE_RANGES: [&str; 8] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "127.0.0.0/8",
    "::1",
    "fc00::/7",
    "fe80::/10",
];

impl Network {
    /// Whether every address of the range belongs to a home or an office network.
    pub fn is_private(&self) -> bool {
        PRIVATE_RANGES
            .iter()
            .filter_map(|range| Network::parse(range))
            .any(|range| self.prefix >= range.prefix && range.contains(self.address))
    }
}

/// Whether `allowed_clients` names somebody, and nobody outside home and office networks.
pub fn private_clients(access: Option<&ConfigValue>) -> bool {
    match access.and_then(|access| access.get("allowed_clients")) {
        Some(ConfigValue::Array(ranges)) => {
            !ranges.is_empty()
                && ranges.iter().all(|range| {
                    range
                        .as_str()
                        .and_then(Network::parse)
                        .is_some_and(|range| range.is_private())
                })
        }
        _ => false,
    }
}

/// Whether a client may connect: any client when no range is listed, else the machine itself
/// and the clients inside a listed range.
pub fn client_allowed(networks: &[Network], address: std::net::IpAddr) -> bool {
    networks.is_empty()
        || address.to_canonical().is_loopback()
        || networks.iter().any(|network| network.contains(address))
}

/// Validates a serve section and returns the settings with defaults, or `None` on errors.
pub fn validate_serve(
    project_type: &str,
    input: Option<&ConfigValue>,
    report: &mut Report,
) -> Option<ConfigValue> {
    let Some(input) = input.filter(|input| is_mapping(Some(input))) else {
        report.issue(
            if input.is_none() {
                "SECTION_MISSING"
            } else {
                "ARG_TYPE"
            },
            path!["serve"],
            "serve settings",
            "Use the serve section generated by init.",
            Details::none(),
        );
        return None;
    };
    // `??` treats null like a missing value.
    let transport = input
        .get("transport")
        .filter(|value| **value != ConfigValue::Null)
        .cloned()
        .or_else(|| (!search_project(project_type)).then(|| ConfigValue::String("http".into())));
    if transport.as_ref().and_then(ConfigValue::as_str) == Some("stdio") {
        if !search_project(project_type) {
            report.issue(
                "ARG_RANGE",
                path!["serve", "transport"],
                "http",
                "Wiki and PARA serve the web interface over HTTP.",
                Details::none(),
            );
        }
        for key in keys(input).into_iter().filter(|key| key != "transport") {
            let conflict =
                ["host", "port", "endpoints", "access", "limits"].contains(&key.as_str());
            report.issue(
                if conflict {
                    "ARG_CONFLICT"
                } else {
                    "ARG_UNKNOWN"
                },
                path!["serve", &key],
                "{ transport: stdio }",
                "Remove the HTTP setting when using stdio.",
                Details::none(),
            );
        }
        return Some(ConfigValue::Object(vec![(
            "transport".into(),
            ConfigValue::String("stdio".into()),
        )]));
    }
    if transport.as_ref().and_then(ConfigValue::as_str) != Some("http") || project_type == "mcp" {
        report.issue(
            "ARG_RANGE",
            path!["serve", "transport"],
            if project_type == "mcp" {
                "stdio"
            } else {
                "http or stdio"
            },
            "Set the transport for this project type.",
            Details::none(),
        );
        return None;
    }
    let issues = serve_issues(input);
    if !issues.is_empty() {
        for (found, unknown) in issues {
            let code = match found.code {
                "unrecognized_keys" => "ARG_UNKNOWN",
                "invalid_type" => "ARG_TYPE",
                _ => "ARG_RANGE",
            };
            let names: Vec<Option<String>> = if found.code == "unrecognized_keys" {
                unknown.into_iter().map(Some).collect()
            } else {
                vec![None]
            };
            for name in names {
                let mut full = path!["serve"];
                full.extend(found.path.iter().cloned());
                if let Some(name) = name {
                    full.push(Part::Key(name));
                }
                report.issue(
                    code,
                    full,
                    "An HTTP serve setting from §27.5",
                    "Check the type and allowed range.",
                    Details::none(),
                );
            }
        }
        return None;
    }
    let selected = merge_serve(project_type, input);
    if project_type == "rag" && input.get("pages").is_some() {
        report.issue(
            "ARG_UNKNOWN",
            path!["serve", "pages"],
            "Wiki or PARA setting",
            "Remove pages from RAG serve.",
            Details::none(),
        );
    }
    let allowed: Vec<String> = endpoints(project_type)
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    if let Some(ConfigValue::Array(names)) = selected.get("endpoints") {
        for (i, name) in names.iter().enumerate() {
            let text = name.as_str().unwrap_or_default();
            if !allowed.iter().any(|allowed| allowed == text) {
                report.issue(
                    "ARG_RANGE",
                    path!["serve", "endpoints", i],
                    Expected::List(allowed.clone()),
                    "Choose an endpoint provided by this project type.",
                    Details::none(),
                );
            } else if names.iter().position(|other| other == name) != Some(i) {
                report.issue(
                    "ARG_CONFLICT",
                    path!["serve", "endpoints", i],
                    "Unique endpoint names",
                    "List each endpoint once.",
                    Details::none(),
                );
            }
        }
    }
    let auth = selected.get("access").and_then(|access| access.get("auth"));
    let auth_type = auth
        .and_then(|auth| auth.get("type"))
        .and_then(ConfigValue::as_str)
        .unwrap_or_default();
    let host = selected
        .get("host")
        .and_then(ConfigValue::as_str)
        .unwrap_or_default();
    // Outside loopback, a token is asked of every client, unless only the clients of home and
    // office networks are let in.
    if !is_loopback(host) && auth_type != "token" && !private_clients(selected.get("access")) {
        report.issue(
            "ARG_CONFLICT",
            path!["serve", "host"],
            "Loopback or token authentication",
            "Set access.auth.type to token before binding outside loopback.",
            Details::none(),
        );
    }
    let token_env = auth.and_then(|auth| auth.get("token_env"));
    if auth_type == "token" && !token_env.is_some_and(js_truthy) {
        report.issue(
            "REQUIREMENT_MISSING",
            path!["serve", "access", "auth", "token_env"],
            "An environment variable name",
            "Set token_env and export the matching secret.",
            Details::none(),
        );
    }
    if auth_type == "none" && token_env != Some(&ConfigValue::Null) {
        report.issue(
            "ARG_CONFLICT",
            path!["serve", "access", "auth", "token_env"],
            "null with auth.type none",
            "Remove token_env or enable token authentication.",
            Details::none(),
        );
    }
    Some(selected)
}

/// `{ ...serveTemplate(type), ...data, access: { ...access, ...data.access, auth: {...} }, limits: {...} }`
fn merge_serve(project_type: &str, input: &ConfigValue) -> ConfigValue {
    fn assign(target: &mut Vec<(String, ConfigValue)>, source: &[(String, ConfigValue)]) {
        for (key, value) in source {
            match target.iter_mut().find(|(existing, _)| existing == key) {
                Some(entry) => entry.1 = value.clone(),
                None => target.push((key.clone(), value.clone())),
            }
        }
    }
    let ConfigValue::Object(mut merged) = serve_template(project_type) else {
        unreachable!()
    };
    assign(&mut merged, entries(input));
    let ConfigValue::Object(mut access) = default_access() else {
        unreachable!()
    };
    let given_access = input.get("access");
    assign(&mut access, given_access.map(entries).unwrap_or_default());
    let ConfigValue::Object(mut auth) = default_access()
        .get("auth")
        .cloned()
        .unwrap_or(ConfigValue::Null)
    else {
        unreachable!()
    };
    assign(
        &mut auth,
        given_access
            .and_then(|access| access.get("auth"))
            .map(entries)
            .unwrap_or_default(),
    );
    assign(&mut access, &[("auth".into(), ConfigValue::Object(auth))]);
    let ConfigValue::Object(mut limits) = serve_template("rag")
        .get("limits")
        .cloned()
        .unwrap_or(ConfigValue::Null)
    else {
        unreachable!()
    };
    assign(
        &mut limits,
        input.get("limits").map(entries).unwrap_or_default(),
    );
    assign(
        &mut merged,
        &[
            ("access".into(), ConfigValue::Object(access)),
            ("limits".into(), ConfigValue::Object(limits)),
        ],
    );
    ConfigValue::Object(merged)
}

/// A list in `access`: its key, whether it needs an item, and how to check each item.
type ListCheck = (&'static str, bool, fn(&str) -> bool);

/// The zod issues of the HTTP serve schema. Unrecognized keys carry the key names.
fn serve_issues(input: &ConfigValue) -> Vec<(Issue, Vec<String>)> {
    fn add(out: &mut Vec<(Issue, Vec<String>)>, found: Issue) {
        out.push((found, Vec::new()));
    }
    fn unknown(
        out: &mut Vec<(Issue, Vec<String>)>,
        value: &ConfigValue,
        known: &[&str],
        path: Vec<Part>,
    ) {
        let mut extra = Vec::new();
        let names = unknown_keys(value, known, path, &mut extra);
        out.extend(extra.into_iter().map(|found| (found, names.clone())));
    }
    fn integers(
        out: &mut Vec<(Issue, Vec<String>)>,
        value: Option<&ConfigValue>,
        min: f64,
        max: f64,
        path: Vec<Part>,
    ) {
        let mut found = Vec::new();
        integer_issues(value, min, max, path, &mut found);
        out.extend(found.into_iter().map(|found| (found, Vec::new())));
    }
    let mut out: Vec<(Issue, Vec<String>)> = Vec::new();
    if let Some(value) = input.get("transport")
        && value.as_str() != Some("http")
    {
        add(&mut out, issue("invalid_value", path!["transport"]));
    }
    match input.get("host") {
        None => {}
        Some(ConfigValue::String(host)) => {
            if host != "localhost" && ip_version(host) == 0 {
                add(&mut out, issue("custom", path!["host"]));
            }
        }
        Some(_) => add(&mut out, issue("invalid_type", path!["host"])),
    }
    integers(&mut out, input.get("port"), 0.0, 65535.0, path!["port"]);
    match input.get("endpoints") {
        None => {}
        Some(ConfigValue::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                if item.as_str().is_none() {
                    add(&mut out, issue("invalid_type", path!["endpoints", i]));
                }
            }
        }
        Some(_) => add(&mut out, issue("invalid_type", path!["endpoints"])),
    }
    match input.get("pages") {
        None => {}
        Some(ConfigValue::String(text)) if text.is_empty() => {
            add(&mut out, issue("too_small", path!["pages"]))
        }
        Some(ConfigValue::String(_)) => {}
        Some(_) => add(&mut out, issue("invalid_type", path!["pages"])),
    }
    match input.get("access") {
        None => {}
        Some(access) if is_mapping(Some(access)) => {
            let lists: [ListCheck; 3] = [
                ("allowed_hosts", true, |text| hostname(text).is_some()),
                ("allowed_origins", false, origin),
                ("allowed_clients", true, |text| {
                    Network::parse(text).is_some()
                }),
            ];
            for (key, min, valid) in lists {
                match access.get(key) {
                    None => {}
                    Some(ConfigValue::Array(items)) => {
                        for (i, item) in items.iter().enumerate() {
                            match item.as_str() {
                                Some(text) if valid(text) => {}
                                Some(_) => add(&mut out, issue("custom", path!["access", key, i])),
                                None => {
                                    add(&mut out, issue("invalid_type", path!["access", key, i]))
                                }
                            }
                        }
                        if min && items.is_empty() {
                            add(&mut out, issue("too_small", path!["access", key]));
                        }
                    }
                    Some(_) => add(&mut out, issue("invalid_type", path!["access", key])),
                }
            }
            match access.get("auth") {
                None => {}
                Some(auth) if is_mapping(Some(auth)) => {
                    if let Some(value) = auth.get("type")
                        && !matches!(value.as_str(), Some("none" | "token"))
                    {
                        add(
                            &mut out,
                            issue("invalid_value", path!["access", "auth", "type"]),
                        );
                    }
                    match auth.get("token_env") {
                        None | Some(ConfigValue::Null) => {}
                        Some(ConfigValue::String(text)) if registry::environment_name(text) => {}
                        Some(ConfigValue::String(_)) => add(
                            &mut out,
                            issue("invalid_format", path!["access", "auth", "token_env"]),
                        ),
                        Some(_) => add(
                            &mut out,
                            issue("invalid_type", path!["access", "auth", "token_env"]),
                        ),
                    }
                    unknown(
                        &mut out,
                        auth,
                        &["type", "token_env"],
                        path!["access", "auth"],
                    );
                }
                Some(_) => add(&mut out, issue("invalid_type", path!["access", "auth"])),
            }
            unknown(
                &mut out,
                access,
                &[
                    "allowed_hosts",
                    "allowed_origins",
                    "allowed_clients",
                    "auth",
                ],
                path!["access"],
            );
        }
        Some(_) => add(&mut out, issue("invalid_type", path!["access"])),
    }
    match input.get("limits") {
        None => {}
        Some(limits) if is_mapping(Some(limits)) => {
            integers(
                &mut out,
                limits.get("max_request_bytes"),
                1.0,
                67_108_864.0,
                path!["limits", "max_request_bytes"],
            );
            integers(
                &mut out,
                limits.get("request_timeout_seconds"),
                1.0,
                86_400.0,
                path!["limits", "request_timeout_seconds"],
            );
            integers(
                &mut out,
                limits.get("max_concurrent"),
                1.0,
                1024.0,
                path!["limits", "max_concurrent"],
            );
            unknown(
                &mut out,
                limits,
                &[
                    "max_request_bytes",
                    "request_timeout_seconds",
                    "max_concurrent",
                ],
                path!["limits"],
            );
        }
        Some(_) => add(&mut out, issue("invalid_type", path!["limits"])),
    }
    unknown(
        &mut out,
        input,
        &[
            "transport",
            "host",
            "port",
            "endpoints",
            "pages",
            "access",
            "limits",
        ],
        Vec::new(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::{Network, client_allowed, private_clients};
    use crate::config_value::ConfigValue;

    fn ranges(values: &[&str]) -> Vec<Network> {
        values
            .iter()
            .map(|value| Network::parse(value).unwrap())
            .collect()
    }

    #[test]
    fn reads_an_address_or_a_range() {
        for value in [
            "192.168.11.0/24",
            "192.168.11.7",
            "10.0.0.0/8",
            "fd00::/8",
            "::1",
            "0.0.0.0/0",
        ] {
            assert!(Network::parse(value).is_some(), "{value}");
        }
        for value in [
            "192.168.11.*",
            "192.168.11.0/33",
            "192.168.11.0/",
            "192.168.11.0/+8",
            "::1/129",
            "host/24",
            "",
        ] {
            assert!(Network::parse(value).is_none(), "{value}");
        }
    }

    #[test]
    fn allows_the_machine_itself_and_the_listed_ranges() {
        let home = ranges(&["192.168.11.0/24"]);
        for address in [
            "192.168.11.1",
            "192.168.11.254",
            "127.0.0.1",
            "::1",
            "::ffff:192.168.11.9",
        ] {
            assert!(client_allowed(&home, address.parse().unwrap()), "{address}");
        }
        for address in ["192.168.12.1", "192.168.10.255", "100.112.0.5", "fe80::1"] {
            assert!(
                !client_allowed(&home, address.parse().unwrap()),
                "{address}"
            );
        }
        assert!(client_allowed(&[], "203.0.113.9".parse().unwrap()));
        assert!(client_allowed(
            &ranges(&["0.0.0.0/0"]),
            "203.0.113.9".parse().unwrap()
        ));
        assert!(client_allowed(
            &ranges(&["fd00::/8"]),
            "fd12::5".parse().unwrap()
        ));
        assert!(!client_allowed(
            &ranges(&["192.168.11.7"]),
            "192.168.11.8".parse().unwrap()
        ));
    }

    #[test]
    fn tells_home_and_office_ranges_from_the_others() {
        let access = |values: &[&str]| {
            ConfigValue::Object(vec![(
                "allowed_clients".into(),
                ConfigValue::Array(
                    values
                        .iter()
                        .map(|value| ConfigValue::String((*value).into()))
                        .collect(),
                ),
            )])
        };
        for values in [
            &["192.168.11.0/24"][..],
            &["192.168.11.7", "10.0.0.0/8", "172.16.0.0/12"],
            &["fd00::/8", "fe80::/10", "::1", "127.0.0.1"],
        ] {
            assert!(private_clients(Some(&access(values))), "{values:?}");
        }
        for values in [
            &[][..],
            &["0.0.0.0/0"],
            &["192.168.0.0/15"],
            &["172.16.0.0/11"],
            &["192.168.11.0/24", "203.0.113.0/24"],
            &["100.64.0.0/10"],
            &["::/0"],
            &["192.168.11.*"],
        ] {
            assert!(!private_clients(Some(&access(values))), "{values:?}");
        }
        assert!(!private_clients(None));
        assert!(!private_clients(Some(&ConfigValue::Object(Vec::new()))));
    }
}
