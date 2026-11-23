//! Projects of the CLI (spec 28.13 step 6c): finding `KnowledgeNestfile.yaml`, reading and checking it,
//! the plan of each stage, builds, and the errors commands report, as `project.mjs` did.

use std::path::Path;

use crate::config_value::ConfigValue;
use crate::index_stage::{assign, items, object, string_field, text, truthy};
use crate::node_path;
use crate::registry::ModuleInfo;
use crate::source_load::sha256_hex;
use crate::transform_stage::StageError;

pub const CONFIG_NAME: &str = "KnowledgeNestfile.yaml";
pub const PROJECT_TYPES: [&str; 4] = ["mcp", "rag", "wiki", "para"];
pub const STAGES: [&str; 3] = ["source", "transform", "index"];

pub fn is_search_project(kind: &str) -> bool {
    kind == "mcp" || kind == "rag"
}

/// An error a command reports: its code and exit code, or a plain error, with optional data,
/// diagnostics and the module it came from.
#[derive(Debug, Default)]
pub struct CliError {
    pub code: Option<String>,
    pub message: String,
    pub exit: Option<u8>,
    pub data: Option<Box<ConfigValue>>,
    // Boxed to keep the error small.
    #[allow(clippy::box_collection)]
    pub diagnostics: Option<Box<Vec<ConfigValue>>>,
    pub module: Option<String>,
    pub section: Option<String>,
}

impl CliError {
    pub fn new(exit: u8, code: &str, message: impl Into<String>) -> Self {
        Self {
            code: Some(code.to_owned()),
            message: message.into(),
            exit: Some(exit),
            ..Self::default()
        }
    }

    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    pub fn with_diagnostics(mut self, diagnostics: Vec<ConfigValue>) -> Self {
        self.diagnostics = Some(Box::new(diagnostics));
        self
    }
}

impl From<StageError> for CliError {
    /// `stageFailure(error)`: a coded error with its exit code, or a plain error with its code.
    fn from(error: StageError) -> Self {
        Self {
            code: error.code.map(str::to_owned),
            message: error.message,
            exit: error.exit_code,
            data: error.exit_code.and(error.data).map(Box::new),
            ..Self::default()
        }
    }
}

pub fn revision(content: &[u8]) -> String {
    sha256_hex(content)
}

/// `{ data, state, artifacts, corpus }` of a project root.
#[derive(Clone)]
pub struct Paths {
    pub data: String,
    pub state: String,
    pub artifacts: String,
    pub corpus: String,
}

pub fn project_paths(root: &str) -> Paths {
    let data = node_path::join(root, &[".knowledge-nest"]);
    Paths {
        state: node_path::join(&data, &["state.sqlite"]),
        artifacts: node_path::join(&data, &["artifacts"]),
        corpus: node_path::join(&data, &["corpus"]),
        data,
    }
}

/// `lstat(path)` succeeded.
pub fn exists(path: &str) -> Result<bool, CliError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(CliError::from(crate::wiki_project::fs_error(
            "lstat", path, &error,
        ))),
    }
}

/// `findProject(directory)`: the KnowledgeNestfile of an explicit directory, or the nearest one above
/// the working directory.
pub fn find_project(directory: Option<&str>, cwd: &str) -> Result<Option<String>, CliError> {
    if let Some(directory) = directory {
        return Ok(Some(node_path::join(
            &node_path::resolve(cwd, directory),
            &[CONFIG_NAME],
        )));
    }
    let mut current = cwd.to_owned();
    loop {
        let path = node_path::join(&current, &[CONFIG_NAME]);
        if exists(&path)? {
            return Ok(Some(path));
        }
        let parent = node_path::dirname(&current);
        if parent == current {
            return Ok(None);
        }
        current = parent;
    }
}

/// The parsed configuration and the line of each value.
#[derive(Clone, Default)]
pub struct Parsed {
    pub config: ConfigValue,
    pub lines: Vec<(Vec<String>, usize)>,
}

impl Parsed {
    /// No lines, as `() => null` gave none.
    pub fn empty() -> Self {
        Self::default()
    }

    /// `line(path)`: the line of the longest existing prefix of the path.
    pub fn line(&self, path: &[ConfigValue]) -> ConfigValue {
        let path: Vec<String> = path.iter().map(path_text).collect();
        for length in (1..=path.len()).rev() {
            if let Some((_, line)) = self.lines.iter().find(|(key, _)| key[..] == path[..length]) {
                return ConfigValue::Number(*line as f64);
            }
        }
        ConfigValue::Null
    }
}

fn path_text(part: &ConfigValue) -> String {
    match part {
        ConfigValue::String(text) => text.clone(),
        ConfigValue::Number(number) => crate::config_value::js_number(*number),
        other => other.to_js_json(),
    }
}

/// `parseConfig(text)`
pub fn parse_config(text: &str) -> Result<Parsed, (Option<usize>, &'static str)> {
    use crate::yaml_config::PathPart;
    match crate::yaml_config::parse_config(text) {
        Ok(parsed) => Ok(Parsed {
            config: parsed.value,
            lines: parsed
                .lines
                .into_iter()
                .map(|(path, line)| {
                    (
                        path.into_iter()
                            .map(|part| match part {
                                PathPart::Key(key) => key,
                                PathPart::Index(index) => index.to_string(),
                            })
                            .collect(),
                        line,
                    )
                })
                .collect(),
        }),
        Err(error) => Err((error.line, error.message)),
    }
}

/// One configured module of a stage plan.
#[derive(Clone)]
pub struct Step {
    pub module: &'static ModuleInfo,
    /// The instance name; restored settings may have none.
    pub name: Option<ConfigValue>,
    pub args: ConfigValue,
}

impl Step {
    pub fn arg(&self, name: &str) -> Option<&ConfigValue> {
        self.args.get(name)
    }
}

pub type Plan = Vec<(String, Vec<Step>)>;

pub fn stage<'a>(plan: &'a Plan, name: &str) -> &'a [Step] {
    plan.iter()
        .find(|(stage, _)| stage == name)
        .map_or(&[], |(_, steps)| steps.as_slice())
}

/// `moduleSettings(plan)`: `{ use, version, name, with }` of each step.
pub fn module_settings(steps: &[Step]) -> ConfigValue {
    ConfigValue::Array(
        steps
            .iter()
            .map(|step| {
                let mut entries = vec![
                    ("use", text(step.module.name)),
                    (
                        "version",
                        ConfigValue::Number(f64::from(step.module.version)),
                    ),
                ];
                if let Some(name) = &step.name {
                    entries.push(("name", name.clone()));
                }
                entries.push(("with", step.args.clone()));
                object(entries)
            })
            .collect(),
    )
}

/// `restoreModules(settings)`: the steps of saved module settings.
pub fn restore_modules(settings: &ConfigValue) -> Vec<Step> {
    items(Some(settings))
        .iter()
        .filter_map(|entry| {
            Some(Step {
                module: crate::registry::module(string_field(entry, "use")?)?,
                name: entry.get("name").cloned(),
                args: entry
                    .get("with")
                    .cloned()
                    .unwrap_or(ConfigValue::Object(Vec::new())),
            })
        })
        .collect()
}

/// A loaded project.
#[derive(Clone)]
pub struct Project {
    pub root: String,
    pub paths: Paths,
    pub config_path: String,
    pub config_root: String,
    pub config_digest: String,
    pub kind: String,
    pub plan: Plan,
    pub wiki_plan: Option<ConfigValue>,
    pub eval_settings: Option<ConfigValue>,
    pub serve_settings: Option<ConfigValue>,
    pub config: ConfigValue,
    pub trial_limits: ConfigValue,
}

/// The tools of this release: `[name, version]`, with the former name of lightonocr.
pub fn tool_catalog() -> Vec<(String, String)> {
    crate::cli_tools::catalog()
        .iter()
        .map(|tool| (tool.name.to_owned(), tool.version.to_owned()))
        .collect()
}

/// `normalizeProjectType(config)`: a legacy stdio search project runs as MCP.
pub fn normalize_type(config: ConfigValue, warnings: Option<&mut Vec<ConfigValue>>) -> ConfigValue {
    let legacy = config
        .get("init")
        .and_then(|init| string_field(init, "type"))
        == Some("rag")
        && config.get("ask").is_none()
        && config
            .get("serve")
            .and_then(|serve| string_field(serve, "transport"))
            == Some("stdio")
        && matches!(config.get("serve"), Some(ConfigValue::Object(entries)) if entries.len() == 1);
    if !legacy {
        return config;
    }
    if let Some(warnings) = warnings {
        warnings.push(object(vec![
            ("code", text("LEGACY_PROJECT_TYPE")),
            (
                "message",
                text("This legacy stdio search project runs as MCP. Set init.type to mcp to migrate it."),
            ),
        ]));
    }
    let ConfigValue::Object(mut entries) = config else {
        return config;
    };
    let mut init = match entries.iter().find(|(key, _)| key == "init") {
        Some((_, ConfigValue::Object(init))) => init.clone(),
        _ => Vec::new(),
    };
    assign(&mut init, "type", Some(text("mcp")));
    assign(&mut entries, "init", Some(ConfigValue::Object(init)));
    ConfigValue::Object(entries)
}

fn ordered(value: impl serde::Serialize) -> ConfigValue {
    serde_json::to_string(&value)
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or(ConfigValue::Null)
}

/// The result of checking a configuration.
pub struct Checked {
    pub errors: Vec<ConfigValue>,
    pub warnings: Vec<ConfigValue>,
    pub plan: Plan,
    pub wiki_plan: Option<ConfigValue>,
    pub eval_settings: Option<ConfigValue>,
    pub serve_settings: Option<ConfigValue>,
}

/// `validateRag(config, file, line)` in Rust (spec 26.8).
pub fn validate_rag(config: &ConfigValue, file: &str, lines: &[(Vec<String>, usize)]) -> Checked {
    use crate::config_check::{Environment, FileDiagnostic, Lines, Report};
    let lines = Lines::new(lines.to_vec());
    let mut report = Report::new(&lines);
    let tools = tool_catalog();
    let environment = Environment {
        file,
        tools: &tools,
        cwd: String::new(),
    };
    let validation = crate::config_check::validate_rag(config, &environment, &mut report);
    let diagnostics = |list: &[crate::config_check::Diagnostic]| -> Vec<ConfigValue> {
        list.iter()
            .map(|diagnostic| ordered(FileDiagnostic { file, diagnostic }))
            .collect()
    };
    let plan = validation
        .plan
        .iter()
        .map(|(stage, entries)| {
            (
                (*stage).to_owned(),
                entries
                    .iter()
                    .map(|entry| {
                        let serialized = ordered(entry);
                        Step {
                            module: entry.module,
                            name: Some(
                                serialized.get("name").cloned().unwrap_or(ConfigValue::Null),
                            ),
                            args: serialized
                                .get("args")
                                .cloned()
                                .unwrap_or(ConfigValue::Object(Vec::new())),
                        }
                    })
                    .collect(),
            )
        })
        .collect();
    Checked {
        errors: diagnostics(&report.errors),
        warnings: diagnostics(&report.warnings),
        plan,
        wiki_plan: None,
        eval_settings: validation.eval.clone(),
        serve_settings: validation.serve.clone(),
    }
}

/// `validateServe(type, input, issue)`: the settings and the issues of a serve section.
pub fn validate_serve(
    kind: &str,
    input: Option<&ConfigValue>,
) -> (Option<ConfigValue>, Vec<ConfigValue>) {
    use crate::config_check::{Lines, Report};
    let lines = Lines::new(Vec::new());
    let mut report = Report::new(&lines);
    let settings = crate::config_check::validate_serve(kind, input, &mut report);
    let issues = report
        .errors
        .iter()
        .map(|item| {
            object(vec![
                ("code", text(item.code)),
                ("path", ordered(&item.path)),
                ("expected", ordered(&item.expected)),
                ("hint", text(&item.hint)),
            ])
        })
        .collect();
    (settings, issues)
}

/// `checkTools(config, issue)`: the issues of the tools list.
pub fn check_tools(tools: Option<&ConfigValue>) -> Vec<ConfigValue> {
    use crate::config_check::{Lines, Report};
    let Some(tools) = tools else {
        return Vec::new();
    };
    let lines = Lines::new(Vec::new());
    let mut report = Report::new(&lines);
    crate::config_check::check_tools(Some(tools), &tool_catalog(), &mut report);
    report
        .errors
        .iter()
        .map(|item| {
            object(vec![
                ("code", text(item.code)),
                ("path", ordered(&item.path)),
                ("expected", ordered(&item.expected)),
                ("hint", text(&item.hint)),
            ])
        })
        .collect()
}

/// `projectOcr(tools)`: the OCR tool a project records, with the former name read as lightonocr.
pub fn project_ocr(tools: Option<&ConfigValue>) -> Option<(String, ConfigValue)> {
    let entry = items(tools).iter().find(|item| {
        string_field(item, "name")
            .and_then(crate::cli_tools::find)
            .is_some_and(|tool| tool.ocr)
    })?;
    let name = string_field(entry, "name").unwrap_or_default();
    let tool = if name == "ocr" { "lightonocr" } else { name };
    Some((
        tool.to_owned(),
        entry.get("version").cloned().unwrap_or(ConfigValue::Null),
    ))
}

/// The arguments of a Wiki module, checked as its zod schema checked them: the parsed object in
/// schema order, or none.
fn wiki_args(name: &str, value: &ConfigValue) -> Option<ConfigValue> {
    let ConfigValue::Object(entries) = value else {
        return None;
    };
    let fields: &[&str] = match name {
        "loader.auto" => &["unsupported", "ocr", "fetch", "browser"],
        "index.keyword" => &["tokenizer", "short_query"],
        "wiki.write" | "para.write" => &["model", "rules", "max_pages_per_job", "update"],
        "citation.verify" => &["on_failure"],
        "wiki.search" => &["include_archived"],
        "wiki.answer" => &["model"],
        "wiki.archive" => &["keep_links"],
        "check.contradictions" => &["model", "scope"],
        _ => &[],
    };
    if entries
        .iter()
        .any(|(key, _)| !fields.contains(&key.as_str()))
    {
        return None;
    }
    let get = |key: &str| {
        entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    };
    let mut parsed = Vec::new();
    for &field in fields {
        // How an ingest changes the pages that exist: optional, with a default by the model.
        // How web pages are taken, and in which browser: optional, a browser of the machine.
        if ["update", "fetch", "browser"].contains(&field) && get(field).is_none() {
            continue;
        }
        let value = get(field)?;
        let valid = match (name, field) {
            ("loader.auto", "unsupported") => matches!(value.as_str(), Some("error" | "skip")),
            ("loader.auto", "ocr") => matches!(value.as_str(), Some("none" | "loader.ocr")),
            ("loader.auto", "fetch") => matches!(value.as_str(), Some("browser" | "http")),
            ("loader.auto", "browser") => value
                .as_str()
                .is_some_and(|browser| !browser.trim().is_empty()),
            ("index.keyword", "tokenizer") => value.as_str() == Some("trigram"),
            ("index.keyword", "short_query") => value.as_str() == Some("bigram"),
            (_, "model" | "rules") => value.as_str().is_some(),
            (_, "max_pages_per_job") => matches!(value, ConfigValue::Number(n)
                if n.fract() == 0.0 && (1.0..=32.0).contains(n)),
            ("citation.verify", "on_failure") => value.as_str() == Some("reject"),
            (_, "update") => matches!(value.as_str(), Some("rewrite" | "append")),
            (_, "include_archived") => matches!(value, ConfigValue::Bool(_)),
            (_, "keep_links") => value == &ConfigValue::Bool(true),
            (_, "scope") => value.as_str() == Some("changed_and_related"),
            _ => false,
        };
        if !valid {
            return None;
        }
        parsed.push((field.to_owned(), value.clone()));
    }
    Some(ConfigValue::Object(parsed))
}

/// `validateWiki(config, file, line)`
pub fn validate_wiki(config: &ConfigValue, file: &str, parsed: &Parsed) -> Checked {
    let mut errors: Vec<ConfigValue> = Vec::new();
    let mut add = |code: &str, path: Vec<ConfigValue>, expected: ConfigValue, hint: &str| {
        errors.push(object(vec![
            ("code", text(code)),
            ("file", text(file)),
            ("line", parsed.line(&path)),
            (
                "section",
                path.first().cloned().unwrap_or(ConfigValue::Null),
            ),
            ("module", ConfigValue::Null),
            ("arg", path.last().cloned().unwrap_or(ConfigValue::Null)),
            ("received", ConfigValue::Null),
            ("expected", expected),
            ("suggestions", ConfigValue::Array(Vec::new())),
            ("hint", text(hint)),
        ]));
    };
    let key = |name: &str| text(name);
    let list = |names: &[&str]| ConfigValue::Array(names.iter().map(|name| text(name)).collect());
    let known = [
        "init", "about", "models", "tools", "ingress", "query", "verify", "archive", "serve",
    ];
    if let ConfigValue::Object(entries) = config {
        for (name, _) in entries {
            if !known.contains(&name.as_str()) {
                add(
                    "ARG_UNKNOWN",
                    vec![key(name)],
                    list(&known),
                    "Remove the unknown section.",
                );
            }
        }
    }
    for issue in check_tools(config.get("tools")) {
        let path = items(issue.get("path")).to_vec();
        add(
            string_field(&issue, "code").unwrap_or_default(),
            path,
            issue.get("expected").cloned().unwrap_or(ConfigValue::Null),
            string_field(&issue, "hint").unwrap_or_default(),
        );
    }
    let init = config.get("init");
    if init.and_then(|init| init.get("version")) != Some(&ConfigValue::Number(1.0)) {
        add(
            "VERSION_UNSUPPORTED",
            vec![key("init"), key("version")],
            ConfigValue::Number(1.0),
            "Create a version 1 project.",
        );
    }
    let kind = init
        .and_then(|init| string_field(init, "type"))
        .unwrap_or_default()
        .to_owned();
    let writer = if kind == "wiki" {
        "wiki.write"
    } else {
        "para.write"
    };
    let writer = if kind == "wiki" || kind == "para" {
        writer.to_owned()
    } else {
        format!("{kind}.write")
    };
    let checks: Vec<(&str, Vec<String>)> = vec![
        (
            "ingress",
            vec![
                "loader.auto".into(),
                "index.keyword".into(),
                writer,
                "citation.verify".into(),
            ],
        ),
        ("query", vec!["wiki.search".into(), "wiki.answer".into()]),
        (
            "verify",
            [
                "check.citations",
                "check.stale_versions",
                "check.links",
                "check.orphans",
            ]
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        ),
        ("archive", vec!["wiki.archive".into()]),
    ];
    let mut plan: Vec<(String, ConfigValue)> = Vec::new();
    type Add<'a> = dyn FnMut(&str, Vec<ConfigValue>, ConfigValue, &str) + 'a;
    let sequence = |stage: &str, field: &str, names: &[String], add: &mut Add| -> ConfigValue {
        let fields: &[&str] = if stage == "verify" {
            &["modules", "content"]
        } else {
            &["modules"]
        };
        if field == "modules"
            && let Some(ConfigValue::Object(entries)) = config.get(stage)
        {
            for (name, _) in entries {
                if !fields.contains(&name.as_str()) {
                    add(
                        "ARG_UNKNOWN",
                        vec![key(stage), key(name)],
                        list(fields),
                        "Remove the unknown key.",
                    );
                }
            }
        }
        let entries = match config.get(stage).and_then(|section| section.get(field)) {
            Some(ConfigValue::Array(entries)) if entries.len() == names.len() => entries.clone(),
            _ => {
                add(
                    "SECTION_MISSING",
                    vec![key(stage), key(field)],
                    ConfigValue::Array(names.iter().map(|name| text(name)).collect()),
                    "Use the standard sequence from a newly initialized project.",
                );
                return ConfigValue::Array(Vec::new());
            }
        };
        ConfigValue::Array(
            entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let name = &names[index];
                    let path = vec![key(stage), key(field), ConfigValue::Number(index as f64)];
                    if string_field(entry, "use") != Some(name.as_str()) {
                        let mut at = path.clone();
                        at.push(key("use"));
                        add(
                            "MODULE_UNKNOWN",
                            at,
                            text(name),
                            "Use the supported module in this position.",
                        );
                    }
                    if let ConfigValue::Object(fields) = entry
                        && fields
                            .iter()
                            .any(|(name, _)| name != "use" && name != "with")
                    {
                        add(
                            "ARG_UNKNOWN",
                            path.clone(),
                            text("use and with"),
                            "Remove the unknown key.",
                        );
                    }
                    let with = entry
                        .get("with")
                        .filter(|value| !matches!(value, ConfigValue::Null))
                        .cloned()
                        .unwrap_or(ConfigValue::Object(Vec::new()));
                    let args = if matches!(
                        name.as_str(),
                        "check.citations"
                            | "check.stale_versions"
                            | "check.links"
                            | "check.orphans"
                    ) {
                        matches!(&with, ConfigValue::Object(entries) if entries.is_empty())
                            .then(|| ConfigValue::Object(Vec::new()))
                    } else {
                        wiki_args(name, &with)
                    };
                    if args.is_none() {
                        let mut at = path.clone();
                        at.push(key("with"));
                        add(
                            "ARG_TYPE",
                            at,
                            text(&format!("{name} arguments")),
                            "Use the standard arguments in section 13.5.",
                        );
                    }
                    if let Some(model) = entry
                        .get("with")
                        .and_then(|with| string_field(with, "model"))
                        && config
                            .get("models")
                            .and_then(|models| models.get(model))
                            .and_then(|model| string_field(model, "kind"))
                            != Some("chat")
                    {
                        let mut at = path.clone();
                        at.push(key("with"));
                        at.push(key("model"));
                        add(
                            "REQUIREMENT_MISSING",
                            at,
                            text("A chat model reference"),
                            "Define the model under models.",
                        );
                    }
                    args.unwrap_or(ConfigValue::Null)
                })
                .collect(),
        )
    };
    for (stage, names) in &checks {
        let steps = sequence(stage, "modules", names, &mut add);
        plan.push(((*stage).to_owned(), steps));
    }
    let ingress_ocr = plan
        .iter()
        .find(|(stage, _)| stage == "ingress")
        .and_then(|(_, steps)| items(Some(steps)).first().cloned())
        .and_then(|loader| loader.get("ocr").cloned());
    if ingress_ocr.as_ref().and_then(ConfigValue::as_str) == Some("loader.ocr")
        && project_ocr(config.get("tools")).is_none()
    {
        add(
            "REQUIREMENT_MISSING",
            vec![
                key("ingress"),
                key("modules"),
                ConfigValue::Number(0.0),
                key("with"),
                key("ocr"),
            ],
            text("The ocr tool"),
            "Run nest add ocr.",
        );
    }
    let content = sequence(
        "verify",
        "content",
        &["check.contradictions".to_owned()],
        &mut add,
    );
    plan.push(("content".into(), content));
    let about_valid = match config.get("about") {
        Some(ConfigValue::Object(entries)) => {
            entries
                .iter()
                .all(|(name, _)| ["title", "purpose", "out_of_scope"].contains(&name.as_str()))
                && matches!(config.get("about").and_then(|about| about.get("title")), Some(ConfigValue::String(title)) if !title.is_empty())
                && matches!(
                    config.get("about").and_then(|about| about.get("purpose")),
                    Some(ConfigValue::String(_))
                )
                && matches!(
                    config
                        .get("about")
                        .and_then(|about| about.get("out_of_scope")),
                    Some(ConfigValue::String(_))
                )
        }
        _ => false,
    };
    if !about_valid {
        add(
            "ARG_TYPE",
            vec![key("about")],
            text("title, purpose, and out_of_scope"),
            "Describe the project purpose and scope.",
        );
    }
    let (serve_settings, issues) = validate_serve(&kind, config.get("serve"));
    for issue in issues {
        add(
            string_field(&issue, "code").unwrap_or_default(),
            items(issue.get("path")).to_vec(),
            issue.get("expected").cloned().unwrap_or(ConfigValue::Null),
            string_field(&issue, "hint").unwrap_or_default(),
        );
    }
    let models_valid = match config.get("models") {
        Some(ConfigValue::Object(models)) => {
            models.iter().all(|(_, model)| wiki_model_valid(model))
        }
        _ => false,
    };
    if !models_valid {
        add(
            "ARG_TYPE",
            vec![key("models")],
            text("Named chat models and environment variable references"),
            "Use models.writer from the standard template.",
        );
    } else if let Some(ConfigValue::Object(models)) = config.get("models") {
        for (name, model) in models {
            let Some(base) = string_field(model, "base_url").filter(|base| !base.is_empty()) else {
                continue;
            };
            if let Ok(url) = url::Url::parse(base)
                && (!["http", "https"].contains(&url.scheme())
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some_and(|query| !query.is_empty())
                    || url.fragment().is_some_and(|fragment| !fragment.is_empty()))
            {
                add(
                    "ARG_RANGE",
                    vec![key("models"), key(name), key("base_url")],
                    text("A credential-free HTTP(S) endpoint"),
                    "Use api_key_env for credentials.",
                );
            }
        }
    }
    Checked {
        errors,
        warnings: Vec::new(),
        plan: Vec::new(),
        wiki_plan: Some(ConfigValue::Object(plan)),
        eval_settings: None,
        serve_settings,
    }
}

/// A model of a Wiki or PARA project, as its zod schema checked it.
fn wiki_model_valid(model: &ConfigValue) -> bool {
    let ConfigValue::Object(entries) = model else {
        return false;
    };
    let allowed = [
        "provider",
        "kind",
        "base_url",
        "model",
        "api_key_env",
        "max_input_bytes",
    ];
    if entries
        .iter()
        .any(|(name, _)| !allowed.contains(&name.as_str()))
    {
        return false;
    }
    let get = |name: &str| model.get(name);
    let provider = matches!(
        get("provider").and_then(ConfigValue::as_str),
        Some("ollama" | "openai" | "strata")
    );
    let kind = get("kind").and_then(ConfigValue::as_str) == Some("chat");
    let base = get("base_url").is_none_or(|value| value.as_str().is_some());
    let name = matches!(
        get("model"),
        Some(ConfigValue::String(_) | ConfigValue::Null)
    );
    let key = get("api_key_env").is_none_or(|value| {
        value.as_str().is_some_and(|text| {
            let mut chars = text.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    });
    let bytes = get("max_input_bytes").is_none_or(|value| {
        matches!(value, ConfigValue::Number(n) if n.fract() == 0.0 && *n > 0.0 && *n <= 9_007_199_254_740_991.0)
    });
    provider && kind && base && name && key && bytes
}

/// `trialLimits(active, candidate)`: the stricter of the adopted and candidate limits.
pub fn trial_limits(active: Option<&ConfigValue>, candidate: Option<&ConfigValue>) -> ConfigValue {
    let fields = [
        "max_runs",
        "max_elapsed_ms",
        "max_api_requests",
        "max_api_input_bytes",
    ];
    let number =
        |limits: Option<&ConfigValue>, key: &str| match limits.and_then(|limits| limits.get(key)) {
            Some(ConfigValue::Number(value)) => *value,
            _ => f64::INFINITY,
        };
    ConfigValue::Object(
        fields
            .iter()
            .filter_map(|key| {
                let limit = number(active, key).min(number(candidate, key));
                limit
                    .is_finite()
                    .then(|| ((*key).to_owned(), ConfigValue::Number(limit)))
            })
            .collect(),
    )
}

/// `loadProject(configPath, command, candidate, warnings)`
pub fn load_project(
    config_path: Option<&str>,
    command: &str,
    candidate: Option<&str>,
    env: &crate::cli_tools::CliEnv,
    warnings: &mut Vec<ConfigValue>,
) -> Result<Project, CliError> {
    let cwd = env.cwd.as_str();
    let Some(config_path) = config_path.filter(|path| exists(path).unwrap_or(false)) else {
        return Err(CliError::new(
            3,
            "PROJECT_REQUIRED",
            "No KnowledgeNestfile.yaml found. Run nest init <mcp|rag|wiki|para> <directory>.",
        ));
    };
    let root = node_path::dirname(config_path);
    let file = match candidate {
        Some(candidate) => node_path::resolve(cwd, candidate),
        None => config_path.to_owned(),
    };
    let invalid = || {
        CliError::new(
            2,
            "INVALID_CONFIG",
            "Could not read a valid KnowledgeNestfile.yaml",
        )
    };
    let input = std::fs::read(&file).map_err(|_| invalid())?;
    let input_text = std::str::from_utf8(&input)
        .map_err(|_| invalid())?
        .to_owned();
    let parsed = match parse_config(&input_text) {
        Ok(parsed) => parsed,
        Err((line, message)) => {
            return Err(
                CliError::new(2, "INVALID_CONFIG", message).with_diagnostics(vec![object(vec![
                    ("code", text("YAML_SYNTAX")),
                    ("file", text(&file)),
                    (
                        "line",
                        line.map_or(ConfigValue::Null, |line| ConfigValue::Number(line as f64)),
                    ),
                    ("section", ConfigValue::Null),
                    ("module", ConfigValue::Null),
                    ("arg", ConfigValue::Null),
                    ("received", ConfigValue::Null),
                    ("expected", text("Valid YAML")),
                    ("suggestions", ConfigValue::Array(Vec::new())),
                    ("hint", text(message)),
                ])]),
            );
        }
    };
    let config = normalize_type(parsed.config.clone(), Some(warnings));
    let kind = config
        .get("init")
        .and_then(|init| string_field(init, "type"))
        .unwrap_or_default()
        .to_owned();
    let authoring = [
        "ingress",
        "query",
        "verify",
        "archive",
        "undo",
        "issue",
        "rules",
        "cite",
        "instruct",
        "save-answer",
        "progress",
    ]
    .contains(&command);
    if !PROJECT_TYPES.contains(&kind.as_str()) {
        let message = "Expected init.type in the KnowledgeNestfile. Generate a new standard file with nest init.";
        return Err(
            CliError::new(2, "SECTION_MISSING", message).with_diagnostics(vec![object(vec![
                ("code", text("SECTION_MISSING")),
                ("file", text(&file)),
                ("line", parsed.line(&[text("init"), text("type")])),
                ("section", text("init")),
                ("module", ConfigValue::Null),
                ("arg", text("type")),
                ("received", ConfigValue::Null),
                (
                    "expected",
                    ConfigValue::Array(PROJECT_TYPES.iter().map(|kind| text(kind)).collect()),
                ),
                ("suggestions", ConfigValue::Array(Vec::new())),
                ("hint", text(message)),
            ])]),
        );
    }
    let active = match candidate {
        None => Some(config.clone()),
        Some(_) => {
            let text = std::fs::read(config_path).map_err(|error| {
                CliError::from(crate::wiki_project::fs_error("open", config_path, &error))
            })?;
            parse_config(&String::from_utf8_lossy(&text))
                .ok()
                .map(|parsed| normalize_type(parsed.config, None))
        }
    };
    if candidate.is_some() {
        if active
            .as_ref()
            .and_then(|active| active.get("init"))
            .and_then(|init| string_field(init, "type"))
            != Some(kind.as_str())
        {
            return Err(CliError::new(
                2,
                "WRONG_PROJECT_TYPE",
                "Candidate and project must use the same init.type.",
            ));
        }
        if let Some(limits) = active
            .as_ref()
            .and_then(|active| active.get("trial_limits"))
            && !crate::config_check::trial_limits_valid(limits)
        {
            return Err(CliError::new(
                2,
                "INVALID_CONFIG",
                "Fix trial_limits in the project KnowledgeNestfile.",
            ));
        }
    }
    if command == "ask" && kind != "rag" {
        return Err(CliError::new(
            2,
            "WRONG_PROJECT_TYPE",
            "ask requires a RAG project.",
        ));
    }
    if !is_search_project(&kind)
        && !authoring
        && !["serve", "validate", "defaults", "add", "remove"].contains(&command)
    {
        return Err(CliError::new(
            2,
            "WRONG_PROJECT_TYPE",
            "This command requires a RAG or MCP project.",
        ));
    }
    if is_search_project(&kind) && authoring {
        return Err(CliError::new(
            2,
            "WRONG_PROJECT_TYPE",
            "This command requires a Wiki or PARA project.",
        ));
    }
    let effective = if candidate.is_some() && command == "eval" {
        let ConfigValue::Object(mut entries) = config.clone() else {
            unreachable!()
        };
        for section in ["eval-data", "eval"] {
            assign(
                &mut entries,
                section,
                active
                    .as_ref()
                    .and_then(|active| active.get(section))
                    .cloned(),
            );
        }
        ConfigValue::Object(entries)
    } else {
        config.clone()
    };
    if candidate.is_some()
        && command == "eval"
        && ["eval-data", "eval"].iter().any(|section| {
            config.get(section).is_some()
                && config.get(section).map(ConfigValue::to_js_json)
                    != active
                        .as_ref()
                        .and_then(|active| active.get(section))
                        .map(ConfigValue::to_js_json)
        })
    {
        warnings.push(object(vec![
            ("code", text("EVAL_CONDITIONS_PINNED")),
            (
                "message",
                text("This evaluation uses the dataset and method from the project KnowledgeNestfile."),
            ),
        ]));
    }
    // The server runs with the settings of the machine wherever the project kept the standard
    // value, so the project is checked as it will run here.
    let run = match (effective.get("serve"), crate::cli_tools::global_serve(env)?) {
        (Some(serve), Some(user)) => {
            let ConfigValue::Object(mut entries) = effective.clone() else {
                unreachable!()
            };
            assign(
                &mut entries,
                "serve",
                Some(crate::config_check::with_user_serve(&kind, serve, &user)),
            );
            ConfigValue::Object(entries)
        }
        _ => effective.clone(),
    };
    let checked = if is_search_project(&kind) {
        validate_rag(&run, &file, &parsed.lines)
    } else {
        validate_wiki(&run, &file, &parsed)
    };
    warnings.extend(checked.warnings.iter().cloned());
    if !checked.errors.is_empty() && command != "defaults" {
        return Err(CliError::new(
            2,
            "INVALID_CONFIG",
            format!("{} configuration error(s).", checked.errors.len()),
        )
        .with_diagnostics(checked.errors));
    }
    let paths = project_paths(&root);
    if !exists(&paths.state)? {
        return Err(CliError::new(
            3,
            "STATE_REQUIRED",
            format!("Project state is missing. Initialize a project with nest init {kind}."),
        ));
    }
    if is_search_project(&kind)
        && [
            "build", "index", "eval", "search", "ask", "serve", "validate",
        ]
        .contains(&command)
    {
        let hannoy = ["index", "search"]
            .iter()
            .flat_map(|name| stage(&checked.plan, name).iter())
            .find(|step| step.module.tool == Some("hannoy"))
            .cloned();
        if let Some(step) = hannoy
            && let Err(failure) = crate::cli_tools::check_index_tool(env, &paths)
        {
            let rows = items(
                effective
                    .get(step.module.stage)
                    .and_then(|section| section.get("modules")),
            );
            let index = rows
                .iter()
                .position(|row| string_field(row, "use") == Some(step.module.name));
            let line = parsed.line(&[
                text(step.module.stage),
                text("modules"),
                index.map_or(ConfigValue::Number(-1.0), |index| {
                    ConfigValue::Number(index as f64)
                }),
                text("use"),
            ]);
            return Err(
                CliError::new(2, "REQUIREMENT_MISSING", failure.message.clone()).with_diagnostics(
                    vec![object(vec![
                        ("code", text("REQUIREMENT_MISSING")),
                        ("file", text(&file)),
                        ("line", line),
                        ("section", text(step.module.stage)),
                        ("module", text(step.module.name)),
                        ("arg", ConfigValue::Null),
                        ("received", ConfigValue::Null),
                        (
                            "expected",
                            text(&format!(
                                "Installed hannoy {}",
                                crate::cli_tools::HANNOY_VERSION
                            )),
                        ),
                        ("suggestions", ConfigValue::Array(Vec::new())),
                        ("hint", text(&failure.message)),
                    ])],
                ),
            );
        }
    }
    let trial_limits = trial_limits(
        active
            .as_ref()
            .and_then(|active| active.get("trial_limits")),
        config.get("trial_limits"),
    );
    Ok(Project {
        config_root: node_path::dirname(&file),
        config_digest: revision(&input),
        root,
        paths,
        config_path: config_path.to_owned(),
        plan: checked.plan,
        wiki_plan: checked.wiki_plan,
        eval_settings: checked.eval_settings,
        serve_settings: checked.serve_settings,
        kind,
        config: effective,
        trial_limits,
    })
}

/// The words of a setting name that mark a secret, matched as whole words.
const SECRET: &[&str] = &[
    "apikey",
    "password",
    "passwd",
    "token",
    "secret",
    "authorization",
    "credential",
    "credentials",
];

/// Whether a setting name holds a secret: one of its words (split at `_`, `-`, `.` and lower-to-upper
/// case changes) is in `SECRET`, or it contains `api_key`. Whole words keep names such as `tokenizer`
/// and `max_tokens`, which an earlier substring match dropped from the saved settings.
fn secret_key(key: &str) -> bool {
    let mut words = vec![String::new()];
    let mut previous_lower = false;
    for character in key.chars() {
        if matches!(character, '_' | '-' | '.') {
            words.push(String::new());
            previous_lower = false;
            continue;
        }
        if character.is_uppercase() && previous_lower {
            words.push(String::new());
        }
        previous_lower = character.is_lowercase() || character.is_ascii_digit();
        words
            .last_mut()
            .expect("a word")
            .extend(character.to_lowercase());
    }
    let lower = key.to_lowercase();
    lower.contains("api_key")
        || lower.contains("api-key")
        || words.iter().any(|word| SECRET.contains(&word.as_str()))
        || words
            .windows(2)
            .any(|pair| pair[0] == "api" && pair[1] == "key")
}

/// `safeConfig(value)`: settings without secrets and URLs without credentials, queries or fragments.
pub fn safe_config(value: &ConfigValue) -> ConfigValue {
    match value {
        ConfigValue::Array(items) => ConfigValue::Array(items.iter().map(safe_config).collect()),
        ConfigValue::Object(entries) => ConfigValue::Object(
            entries
                .iter()
                .filter(|(key, _)| {
                    let lower = key.to_lowercase();
                    !secret_key(key) || lower.ends_with("_env") || lower.ends_with("-env")
                })
                .map(|(key, child)| (key.clone(), safe_config(child)))
                .collect(),
        ),
        ConfigValue::String(text) => {
            let Ok(mut url) = url::Url::parse(text) else {
                return value.clone();
            };
            if !["http", "https"].contains(&url.scheme())
                || (url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none_or(str::is_empty)
                    && url.fragment().is_none_or(str::is_empty))
            {
                return value.clone();
            }
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            ConfigValue::String(url.to_string())
        }
        other => other.clone(),
    }
}

/// `currentBuild(paths, required, reference)`: the serving build, or the build a reference names.
pub fn current_build(
    paths: &Paths,
    required: bool,
    reference: Option<&str>,
) -> Result<Option<ConfigValue>, CliError> {
    use rusqlite::OptionalExtension;
    let connection = crate::eval_stage::open_state(&paths.state, true).map_err(CliError::from)?;
    let sql = |error: rusqlite::Error| CliError::from(crate::index_stage::sql(error));
    let row: Option<String> = match reference {
        None => connection
            .query_row("SELECT manifest FROM project JOIN builds ON builds.id = project.build_id", [], |row| row.get(0))
            .optional()
            .map_err(sql)?,
        Some(reference) => connection
            .query_row(
                "SELECT manifest FROM builds WHERE id = ?1 OR id = (SELECT build_id FROM build_tags WHERE name = ?2)
        ORDER BY id = ?3 DESC LIMIT 1",
                [reference, reference, reference],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?,
    };
    if let Some(row) = row {
        let mut manifest = crate::eval_stage::parse(&row).map_err(CliError::from)?;
        if let Some(reference) = reference {
            let tag: Option<String> = connection
                .query_row(
                    "SELECT settings FROM tag_search WHERE name = ?1",
                    [reference],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql)?;
            let build_id = crate::eval_stage::js_string(manifest.get("build_id"));
            let stored = match &tag {
                Some(settings) => Some(settings.clone()),
                None => connection
                    .query_row(
                        "SELECT settings FROM build_search WHERE build_id = ?1",
                        [&build_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(sql)?,
            };
            let reference_key = if tag.is_some() {
                format!("tag:{reference}")
            } else {
                build_id
            };
            let configuration: Option<String> = connection
                .query_row(
                    "SELECT config FROM build_config WHERE ref = ?1",
                    [&reference_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql)?;
            if let ConfigValue::Object(entries) = &mut manifest {
                if let Some(stored) = stored {
                    assign(
                        entries,
                        "search",
                        Some(crate::eval_stage::parse(&stored).map_err(CliError::from)?),
                    );
                }
                if let Some(configuration) = configuration {
                    let parsed =
                        crate::eval_stage::parse(&configuration).map_err(CliError::from)?;
                    assign(entries, "search_models", parsed.get("models").cloned());
                }
            }
        }
        return Ok(Some(manifest));
    }
    if let (true, Some(reference)) = (required, reference) {
        return Err(CliError::new(
            3,
            "BUILD_NOT_FOUND",
            format!(
                "Unknown build: {reference}. Pass a build tag or ID; nest status shows the current build."
            ),
        ));
    }
    if required {
        return Err(CliError::new(
            3,
            "BUILD_REQUIRED",
            "No searchable build. Run nest build first.",
        ));
    }
    Ok(None)
}

/// The keyword options of a build's index (`keywordOptions`).
pub fn open_build(paths: &Paths, build: &ConfigValue) -> Result<crate::KnowledgeBase, CliError> {
    let index = node_path::join(
        &paths.artifacts,
        &[crate::eval_stage::js_string(build.get("index")).as_str()],
    );
    let cancel = crate::Cancellation::default();
    let exists = crate::KnowledgeBase::inspect_index(Path::new(&index), &cancel)
        .map_err(|error| CliError::from(StageError::native(error)))?;
    if exists.is_none() {
        return Err(CliError::new(
            3,
            "BUILD_REQUIRED",
            "The build index is missing. Run nest build first.",
        ));
    }
    let keyword = build.get("keyword");
    let snapshot = keyword
        .filter(|keyword| string_field(keyword, "tokenizer") == Some("lindera"))
        .and_then(|keyword| string_field(keyword, "dictionary_hash"))
        .map(|hash| dictionary_snapshot(paths, hash));
    let options = crate::index_stage::keyword_options(keyword, snapshot.as_deref().map(Path::new));
    let corpus = truthy(build.get("shared_corpus")).then(|| paths.corpus.clone());
    crate::KnowledgeBase::open_with_options(
        Path::new(&index),
        crate::CorpusBackend::Sqlite,
        corpus.as_deref().map(Path::new),
        options,
        &cancel,
    )
    .map_err(|error| CliError::from(StageError::native(error)))
}

pub fn dictionary_snapshot(paths: &Paths, hash: &str) -> String {
    node_path::join(&paths.artifacts, &["dictionaries", &format!("{hash}.csv")])
}

/// `JSON.stringify(value, null, 2)`
pub fn pretty_json(value: &ConfigValue) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out
}

fn write_pretty(value: &ConfigValue, indent: usize, out: &mut String) {
    let pad = |depth: usize| "  ".repeat(depth);
    match value {
        ConfigValue::Array(values) if !values.is_empty() => {
            out.push_str("[\n");
            for (index, item) in values.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                write_pretty(item, indent + 1, out);
                if index + 1 < values.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(indent));
            out.push(']');
        }
        ConfigValue::Object(entries) if !entries.is_empty() => {
            out.push_str("{\n");
            for (index, (name, item)) in entries.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                out.push_str(&text(name).to_js_json());
                out.push_str(": ");
                write_pretty(item, indent + 1, out);
                if index + 1 < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(indent));
            out.push('}');
        }
        other => out.push_str(&other.to_js_json()),
    }
}
