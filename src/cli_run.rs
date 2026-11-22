//! The commands of the `nest` CLI (spec 28.13 step 6c): reading the command line, running each
//! command on the Rust engine and printing the result, as `cli.mjs` and `project.mjs` did. The npm
//! entry point only starts this.

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::Cancellation;
use crate::cli_args::Parsed;
use crate::cli_models::{
    embedding_settings, generator_settings, model_setting, resolve_api_key, resolve_value,
};
use crate::cli_project::{
    CliError, Paths, Project, STAGES, Step, current_build, find_project, is_search_project,
    load_project, module_settings, open_build, pretty_json, project_ocr, restore_modules, revision,
    safe_config, stage,
};
use crate::cli_tools::{CliEnv, with_write_lock};
use crate::config_value::ConfigValue;
use crate::eval_stage::js_string;
use crate::index_stage::{assign, items, object, string_field, text, truthy};
use crate::node_path;
use crate::transform_stage::StageError;
use crate::wiki_project::{atomic_write, fs_error};

/// What the launcher hears while a command runs.
pub trait CliEvents: Send + Sync {
    /// A server starts (`true`) or stops: SIGINT and SIGTERM stop it meanwhile.
    fn serving(&self, _active: bool) {}
    /// The index directory a server opened, for `knowledge-nest:index-open` subscribers.
    fn opened(&self, _path: &str) {}
}

fn print(line: &str) {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(line.as_bytes());
    let _ = stdout.write_all(b"\n");
    let _ = stdout.flush();
}

fn print_error(line: &str) {
    use std::io::Write;
    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(line.as_bytes());
    let _ = stderr.write_all(b"\n");
    let _ = stderr.flush();
}

fn envelope(
    command: Option<&str>,
    status: &str,
    run_id: Option<&str>,
    data: ConfigValue,
    warnings: &[ConfigValue],
    errors: Vec<ConfigValue>,
) -> String {
    object(vec![
        ("schema_version", ConfigValue::Number(1.0)),
        ("command", command.map_or(ConfigValue::Null, text)),
        ("status", text(status)),
        ("run_id", run_id.map_or(ConfigValue::Null, text)),
        ("data", data),
        ("warnings", ConfigValue::Array(warnings.to_vec())),
        ("errors", ConfigValue::Array(errors)),
    ])
    .to_js_json()
}

/// Runs the CLI with `process.argv.slice(2)` and returns the exit code (`cli.mjs`).
pub fn run(
    mut args: Vec<String>,
    env: &CliEnv,
    cancel: &Cancellation,
    events: Arc<dyn CliEvents>,
) -> u8 {
    let end = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let json = args[..end].iter().any(|arg| arg == "--json");
    let fail = |command: Option<&str>, message: &str| {
        if json {
            let error = object(vec![
                ("code", text("INVALID_ARGUMENT")),
                ("message", text(message)),
            ]);
            print(&envelope(
                command,
                "failed",
                None,
                ConfigValue::Null,
                &[],
                vec![error],
            ));
        } else {
            print_error(message);
        }
        2
    };
    let mut project = None;
    if args.first().map(String::as_str) == Some("--project") {
        let taken: Vec<String> = args.drain(..args.len().min(2)).collect();
        project = taken.get(1).cloned();
        let valid = project.as_deref().is_some_and(|project| {
            !project.trim_matches(crate::registry::js_space).is_empty()
                && !project.starts_with("--")
        });
        if !valid {
            return fail(None, "Expected a directory after --project");
        }
    }
    let Some(command) = args.first().cloned() else {
        print(&crate::cli_args::help_text());
        return 0;
    };
    if command == "--help" || command == "-h" {
        print(&crate::cli_args::help_text());
        return 0;
    }
    if ["--version", "-v", "-V", "version"].contains(&command.as_str()) {
        print(&format!("nest {}", env!("CARGO_PKG_VERSION")));
        return 0;
    }
    if command == "help" {
        return match args.get(1) {
            None => {
                print(&crate::cli_args::help_text());
                0
            }
            Some(name) => match crate::cli_args::command_help(name) {
                Some(help) => {
                    print(&help);
                    0
                }
                None => fail(Some(name), &unknown_command(name)),
            },
        };
    }
    if crate::cli_args::usage(&command).is_none() {
        return fail(Some(&command), &unknown_command(&command));
    }
    run_command(
        &command,
        &args[1..],
        project.as_deref(),
        env,
        cancel,
        events,
    )
}

fn unknown_command(command: &str) -> String {
    let hint = crate::cli_args::closest_command(command)
        .map_or(String::new(), |name| format!(" Did you mean nest {name}?"));
    format!("Unknown command: {command}.{hint} Run nest --help for the commands.")
}

fn new_run_id() -> String {
    format!("run-{}", uuid::Uuid::new_v4())
}

fn invalid(message: impl Into<String>) -> CliError {
    CliError::new(2, "INVALID_ARGUMENT", message)
}

/// The state of a command while it runs: its run ID and warnings.
struct Run<'a> {
    command: &'a str,
    run_id: Option<String>,
    warnings: Vec<ConfigValue>,
    env: &'a CliEnv,
    cancel: &'a Cancellation,
    events: Arc<dyn CliEvents>,
    /// What the text output needs besides the result: the search that `ask` answered from, or
    /// `{ project }` for `serve --print-config`.
    context: Option<ConfigValue>,
}

/// What a command printed besides its result: the serve commands print nothing more.
enum Done {
    Data(ConfigValue),
    Quiet,
}

fn run_command(
    command: &str,
    args: &[String],
    explicit: Option<&str>,
    env: &CliEnv,
    cancel: &Cancellation,
    events: Arc<dyn CliEvents>,
) -> u8 {
    let end = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let mut json = args[..end].iter().any(|arg| arg == "--json");
    let mut state = Run {
        command,
        run_id: None,
        warnings: Vec::new(),
        env,
        cancel,
        events,
        context: None,
    };
    let mut kind = String::new();
    let outcome = (|| -> Result<Done, CliError> {
        let parsed = crate::cli_args::parse(command, args).map_err(|message| {
            let mut error = CliError::plain(message);
            error.code = Some("ERR_PARSE_ARGS".into());
            error
        })?;
        json = parsed.flag("json");
        if parsed.flag("help") {
            print(&crate::cli_args::command_help(command).unwrap_or_default());
            return Ok(Done::Quiet);
        }
        kind = parsed.positionals.first().cloned().unwrap_or_default();
        dispatch(&mut state, &parsed, explicit, json)
    })();
    match outcome {
        Ok(Done::Quiet) => 0,
        Ok(Done::Data(data)) => {
            if !json {
                for warning in &state.warnings {
                    print_error(&warning_line(warning));
                }
            }
            if json {
                print(&envelope(
                    Some(command),
                    "succeeded",
                    state.run_id.as_deref(),
                    data,
                    &state.warnings,
                    Vec::new(),
                ));
            } else {
                print(&text_output(command, &kind, &data, state.context.as_ref()));
                if command == "serve" {
                    print_error(
                        "Add this entry to the mcpServers of your MCP client's settings, such as .mcp.json or claude_desktop_config.json.",
                    );
                }
            }
            0
        }
        Err(error) => report_error(command, &state, error, json),
    }
}

/// One configuration diagnostic (an error or a warning) as `file:line section module arg — CODE: hint`.
fn diagnostic_line(item: &ConfigValue) -> String {
    let line = match item.get("line") {
        None | Some(ConfigValue::Null) => "?".into(),
        other => js_string(other),
    };
    let place: Vec<String> = ["section", "module", "arg"]
        .iter()
        .filter_map(|name| match item.get(name) {
            None | Some(ConfigValue::Null) => None,
            other => Some(js_string(other)),
        })
        .filter(|value| !value.is_empty())
        .collect();
    let suggestions: Vec<String> = items(item.get("suggestions"))
        .iter()
        .map(|value| js_string(Some(value)))
        .collect();
    format!(
        "{}:{line} {} — {}: {}{}",
        js_string(item.get("file")),
        place.join(" "),
        js_string(item.get("code")),
        js_string(item.get("hint")),
        if suggestions.is_empty() {
            String::new()
        } else {
            format!(" ({})", suggestions.join(", "))
        }
    )
}

/// A warning for the terminal: its message, or the diagnostic line of a configuration warning.
fn warning_line(warning: &ConfigValue) -> String {
    match warning.get("message").and_then(ConfigValue::as_str) {
        Some(message) => message.to_owned(),
        None => format!("Warning: {}", diagnostic_line(warning)),
    }
}

fn report_error(command: &str, state: &Run, error: CliError, json: bool) -> u8 {
    let code = error.code.clone().unwrap_or_default();
    let argument = code.starts_with("ERR_PARSE_ARGS") || code == "INVALID_ARGUMENT";
    let busy = code == "BUSY"
        || code == "SQLITE_BUSY"
        || error.message.starts_with("Another writer is updating ");
    let exit = error.exit.unwrap_or(if argument {
        2
    } else if busy {
        4
    } else {
        1
    });
    let code = if error.exit.is_some() {
        code
    } else if argument {
        "INVALID_ARGUMENT".into()
    } else if busy {
        "DB_BUSY".into()
    } else {
        "EXECUTION_FAILED".into()
    };
    if json {
        let errors = error
            .diagnostics
            .clone()
            .map(|list| *list)
            .unwrap_or_else(|| {
                let mut entries = vec![("code", text(&code)), ("message", text(&error.message))];
                if let Some(module) = &error.module {
                    entries.push(("module", text(module)));
                    if let Some(section) = &error.section {
                        entries.push(("section", text(section)));
                    }
                }
                vec![object(entries)]
            });
        print(&envelope(
            Some(command),
            "failed",
            state.run_id.as_deref(),
            error.data.clone().map_or(ConfigValue::Null, |data| *data),
            &state.warnings,
            errors,
        ));
    } else {
        print_error(&error.message);
        for item in error.diagnostics.as_deref().into_iter().flatten() {
            print_error(&diagnostic_line(item));
        }
        if let Some(data) = &error.data {
            for file in items(data.get("skipped")) {
                print_error(&format!(
                    "{}:{} — {}",
                    js_string(file.get("source_id")),
                    js_string(file.get("path")),
                    js_string(file.get("reason"))
                ));
            }
        }
    }
    exit
}

fn text_output(
    command: &str,
    kind: &str,
    data: &ConfigValue,
    context: Option<&ConfigValue>,
) -> String {
    if let Some(text) = crate::cli_text::text_output(command, kind, data, context) {
        return text;
    }
    let field = |name: &str| js_string(data.get(name));
    match command {
        "init" => {
            let next = data.get("next").filter(|next| truthy(Some(next)));
            format!(
                "{}{}",
                field("config"),
                next.map_or(String::new(), |next| format!("\n{}", js_string(Some(next))))
            )
        }
        "config" => {
            let mut lines = vec![field("path")];
            // The settings of one project are its `serve` alone.
            let machine = data.get("version").is_some();
            for role in ["embedding", "llm"].into_iter().filter(|_| machine) {
                let setting = data.get(role).filter(|setting| truthy(Some(setting)));
                lines.push(format!(
                    "{role}  {}",
                    setting.map_or("unset".into(), |setting| format!(
                        "{} / {}",
                        js_string(setting.get("provider")),
                        js_string(setting.get("model"))
                    ))
                ));
            }
            let ocr = data
                .get("ocr")
                .and_then(|ocr| ocr.get("tool"))
                .filter(|tool| !matches!(tool, ConfigValue::Null));
            if machine {
                lines.push(format!(
                    "ocr  {}",
                    ocr.map_or("unset".into(), |tool| js_string(Some(tool)))
                ));
            }
            if let Some(serve) = data.get("serve") {
                let access = serve.get("access");
                if let Some(host) = serve.get("host").and_then(ConfigValue::as_str) {
                    lines.push(format!("serve.host  {host}"));
                }
                if let Some(port) = serve
                    .get("port")
                    .filter(|port| matches!(port, ConfigValue::Number(_)))
                {
                    lines.push(format!("serve.port  {}", js_string(Some(port))));
                }
                for key in ["allowed_hosts", "allowed_clients"] {
                    if let Some(list) = access.and_then(|access| access.get(key)) {
                        let names: Vec<String> = items(Some(list))
                            .iter()
                            .map(|item| js_string(Some(item)))
                            .collect();
                        lines.push(format!("serve.{key}  {}", names.join(", ")));
                    }
                }
                if let Some(auth) = access.and_then(|access| access.get("auth")) {
                    lines.push(format!(
                        "serve.auth  {}",
                        match string_field(auth, "token_env") {
                            Some(name) => format!("token ({name})"),
                            None => "none".to_owned(),
                        }
                    ));
                }
            }
            lines.join("\n")
        }
        "dictionary" => {
            if kind == "suggest" {
                crate::cli_dictionary::dictionary_text(data)
            } else {
                field("user_dictionary")
            }
        }
        _ => crate::cli_text::readable(data),
    }
}

fn is_tag_name(value: &str) -> bool {
    use icu_properties::props::{GeneralCategory, GeneralCategoryGroup};
    let categories = icu_properties::CodePointMapData::<GeneralCategory>::new();
    let count = value.chars().count();
    (1..=100).contains(&count)
        && value.chars().all(|c| {
            matches!(c, '_' | '.' | '-')
                || GeneralCategoryGroup::Letter.contains(categories.get(c))
                || GeneralCategoryGroup::Number.contains(categories.get(c))
        })
}

fn blank(value: &str) -> bool {
    value.trim_matches(crate::registry::js_space).is_empty()
}

fn action_count(command: &str, action: Option<&str>) -> Option<i64> {
    let table = action_table(command)?;
    Some(
        table
            .iter()
            .find(|(name, _)| Some(*name) == action)
            .map_or(-1, |(_, count)| *count),
    )
}

/// The actions of a command that takes one, each with its number of positional arguments.
fn action_table(command: &str) -> Option<&'static [(&'static str, i64)]> {
    Some(match command {
        "config" => &[("list", 1), ("set", 2), ("unset", 2)],
        "eval-data" => &[
            ("add", 1),
            ("import", 2),
            ("export", 2),
            ("list", 1),
            ("show", 2),
            ("split", 1),
            ("generate", 1),
            ("review", 2),
            ("sync", 2),
            ("diff", 3),
            ("tag", 3),
            ("use", 2),
        ],
        "report" => &[("show", 2), ("list", 1), ("compare", 3)],
        "dictionary" => &[("suggest", 1), ("add", 2)],
        "evidence-map" => &[
            ("create", 1),
            ("list", 1),
            ("show", 2),
            ("export", 2),
            ("import", 2),
            ("confirm", 2),
        ],
        "modules" => &[("list", 1), ("describe", 2)],
        _ => return None,
    })
}

/// The error for a first positional argument that names no action or project type of `command`.
fn unknown_action(command: &str, given: &str) -> Option<CliError> {
    let known: Vec<&str> = if command == "init" {
        crate::cli_project::PROJECT_TYPES.to_vec()
    } else {
        action_table(command)?
            .iter()
            .map(|(name, _)| *name)
            .collect()
    };
    if known.contains(&given) {
        return None;
    }
    let what = if command == "init" {
        "project type"
    } else {
        "action"
    };
    let hint = crate::cli_args::closest(given, known.iter().copied())
        .map_or(String::new(), |name| format!(" Did you mean {name}?"));
    Some(invalid(format!(
        "Unknown {what} for nest {command}: {given}.{hint} Use {}.",
        known.join(", ")
    )))
}

#[allow(clippy::too_many_lines)]
fn dispatch(
    state: &mut Run,
    parsed: &Parsed,
    explicit: Option<&str>,
    json: bool,
) -> Result<Done, CliError> {
    let command = state.command;
    let env = state.env;
    // The versioned store has its own files. Its own commands always go there; the commands
    // both stores have go there when the configuration file is written for it.
    let project = env.resolve(parsed.string("project").or(explicit).unwrap_or("."));
    if let Some((action, argument)) = crate::cli_store::route(command, parsed, &project, env)? {
        let (data, text) = crate::cli_store::command(
            action,
            argument.as_deref(),
            parsed,
            &project,
            env,
            &|line| {
                print_error(line);
            },
        )?;
        if json {
            return Ok(Done::Data(parse_json(&data.to_string())?));
        }
        print(&text);
        return Ok(Done::Quiet);
    }
    let positionals = &parsed.positionals;
    let value = |name: &str| parsed.string(name);
    let flag = |name: &str| parsed.flag(name);
    let usage = || {
        invalid(format!(
            "Check the arguments of nest {command}.\nUsage:\n{}",
            crate::cli_args::wrapped_usage(command).unwrap_or_default()
        ))
    };
    if command == "serve" && flag("json") && !flag("print-config") {
        return Err(invalid("Use --json with serve --print-config."));
    }
    let first = positionals.first().map(String::as_str);
    if let Some(error) = first.and_then(|given| unknown_action(command, given)) {
        return Err(error);
    }
    let count = match action_count(command, first) {
        Some(count) => count,
        None => match command {
            "tag" => {
                if positionals.len() == 1 {
                    1
                } else {
                    2
                }
            }
            "init" | "cite" | "issue" => 2,
            "instruct" if !flag("persistent") => 2,
            "modules" if first == Some("describe") => 2,
            "save-answer" => 3,
            "ingress" => {
                if flag("pending") {
                    0
                } else if positionals.is_empty() {
                    -1
                } else {
                    positionals.len() as i64
                }
            }
            "search" | "ask" | "modules" | "use" | "query" | "archive" | "undo" | "remove"
            | "instruct" | "progress" => 1,
            "add" if !flag("list") => 1,
            _ => 0,
        },
    };
    if positionals.len() as i64 != count
        || positionals.iter().any(|item| blank(item))
        || value("source") == Some("")
        || value("project") == Some("")
        || (explicit.is_some() && parsed.value("project").is_some())
        || (command == "init" && explicit.is_some())
    {
        return Err(usage());
    }
    let kind = positionals.first().cloned();
    let directory = positionals.get(1).cloned();
    let config_value = value("config");
    if (command == "modules" && !matches!(kind.as_deref(), Some("list" | "describe")))
        || value("tag").is_some_and(|tag| {
            !is_tag_name(tag)
                || value("until").is_some_and(|until| !until.is_empty() && until != "index")
        })
        || value("split").is_some_and(|split| split != "dev" && split != "test")
        || [config_value, value("build"), value("report")]
            .iter()
            .any(|value| value.is_some_and(blank))
        || (command == "dictionary"
            && if kind.as_deref() == Some("add") {
                !config_value.is_some_and(|config| !config.is_empty())
                    || parsed.value("report").is_some()
            } else {
                parsed.value("config").is_some()
            })
        || (command == "tag" && !is_tag_name(kind.as_deref().unwrap_or_default()))
    {
        return Err(usage());
    }
    if command == "init" {
        let kind = kind.as_deref().unwrap_or_default();
        if !crate::cli_project::PROJECT_TYPES.contains(&kind)
            || (parsed.value("source").is_some() && !is_search_project(kind))
            || value("purpose").is_some_and(|purpose| is_search_project(kind) || blank(purpose))
        {
            return Err(usage());
        }
    }
    let mut search_options = SearchOptions {
        source_ids: parsed.strings("source-id"),
        ..SearchOptions::default()
    };
    for (option, max) in [("limit", 100_u32), ("budget-chars", 1_000_000)] {
        let Some(given) = value(option) else {
            continue;
        };
        let number = given
            .parse::<u64>()
            .ok()
            .filter(|_| !given.is_empty() && given.bytes().all(|b| b.is_ascii_digit()));
        let Some(number) = number.filter(|number| (1..=u64::from(max)).contains(number)) else {
            return Err(invalid(format!(
                "Use --{option} with an integer from 1 to {max}."
            )));
        };
        if option == "limit" {
            search_options.limit = Some(number as f64);
        } else {
            search_options.budget_chars = Some(number as f64);
        }
    }
    if let Some(ids) = &search_options.source_ids
        && (ids.len() > 100
            || ids.iter().any(|id| {
                id.is_empty()
                    || id.len() > 64
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
            }))
    {
        return Err(invalid("Provide at most 100 valid source IDs."));
    }
    let position = |stage: &str| STAGES.iter().position(|name| *name == stage);
    let from = value("from");
    let until = value("until");
    if value("input")
        .is_some_and(|input| blank(input) || !matches!(from, Some("transform" | "index")))
        || [from, until]
            .iter()
            .any(|stage| stage.is_some_and(|stage| position(stage).is_none()))
        || matches!((from.and_then(position), until.and_then(position)), (Some(a), Some(b)) if a > b)
    {
        return Err(invalid(
            "Expected source, transform, or index in execution order for --from and --until.",
        ));
    }
    let explicit_project = value("project").or(explicit);

    if command == "modules" {
        let modules: ConfigValue = crate::wiki_project::ordered(crate::registry::modules());
        let data = match &directory {
            None => modules,
            Some(name) => items(Some(&modules))
                .iter()
                .find(|info| string_field(info, "name") == Some(name.as_str()))
                .cloned()
                .ok_or_else(|| {
                    CliError::new(2, "MODULE_UNKNOWN", format!("Unknown module: {name}"))
                })?,
        };
        let path = find_project(explicit_project, &env.cwd)?;
        let active = match &path {
            Some(path) => {
                let input = crate::wiki_project::read_file(path).map_err(CliError::from)?;
                crate::cli_project::parse_config(&input)
                    .ok()
                    .and_then(|parsed| parsed.config.get("tools").cloned())
            }
            None => None,
        };
        let data = match data {
            ConfigValue::Array(list) => ConfigValue::Array(
                list.into_iter()
                    .filter(|info| {
                        if string_field(info, "name") == Some("loader.ocr") {
                            return project_ocr(active.as_ref()).is_some();
                        }
                        match info.get("tool").and_then(ConfigValue::as_str) {
                            None => true,
                            Some(tool) => items(active.as_ref())
                                .iter()
                                .any(|entry| string_field(entry, "name") == Some(tool)),
                        }
                    })
                    .collect(),
            ),
            other => other,
        };
        return Ok(Done::Data(data));
    }
    if command == "add" && flag("list") {
        return Ok(Done::Data(crate::cli_tools::listed_tools()));
    }
    if command == "config" {
        if kind.as_deref() != Some("list") {
            state.run_id = Some(new_run_id());
        }
        let choice = crate::cli_tools::ServeChoice {
            host: value("host"),
            port: value("port"),
            allowed_hosts: value("allowed-hosts"),
            allowed_clients: value("allowed-clients"),
            auth: value("auth"),
            token_env: value("token-env"),
        };
        let scope = value("project").or(explicit);
        if directory.as_deref() == Some("serve") || scope.is_some() || choice.any() {
            let path = find_project(scope, &env.cwd)?.filter(|_| scope.is_some());
            return crate::cli_tools::configure_serve(
                env,
                kind.as_deref().unwrap_or_default(),
                directory.as_deref(),
                &choice,
                path.as_deref(),
                &mut state.warnings,
            )
            .map(Done::Data);
        }
        return crate::cli_tools::configure_global(
            env,
            kind.as_deref().unwrap_or_default(),
            directory.as_deref(),
            value("provider"),
            value("model"),
            value("tool"),
        )
        .map(Done::Data);
    }
    if command == "init" {
        state.run_id = Some(new_run_id());
        return initialize(
            env,
            kind.as_deref().unwrap_or_default(),
            directory.as_deref().unwrap_or_default(),
            parsed,
        )
        .map(Done::Data);
    }
    let path = find_project(explicit_project, &env.cwd)?;
    let project = load_project(
        path.as_deref(),
        command,
        config_value,
        env,
        &mut state.warnings,
    )?;
    let project_type = project.kind.clone();
    let mut serving = None;
    if command == "serve" {
        let (settings, issues) = serve_overrides(&project, parsed);
        if let Some(issue) = issues.first() {
            let hint = js_string(issue.get("hint"));
            return Err(CliError::new(2, "INVALID_CONFIG", hint).with_diagnostics(issues));
        }
        let settings = settings.unwrap_or_default();
        if !flag("print-config") && string_field(&settings, "transport") == Some("http") {
            serve_credential(&settings)?;
        }
        serving = Some(settings);
    }
    if command == "defaults" {
        if parsed.value("apply").is_some() {
            state.run_id = Some(new_run_id());
        }
        return compare_defaults(&project, value("apply")).map(Done::Data);
    }
    if command == "add" || command == "remove" {
        state.run_id = Some(new_run_id());
        let requested = kind.unwrap_or_default();
        return with_execution_lock(&project.paths, || {
            crate::cli_tools::change_tool(env, &project, &requested, command == "remove")
        })
        .map(Done::Data);
    }
    if project.wiki_plan.is_some() {
        if command == "validate" {
            return Ok(Done::Data(object(vec![
                ("valid", ConfigValue::Bool(true)),
                ("type", text(&project_type)),
            ])));
        }
        let read_only = flag("dry-run")
            || ["cite", "rules", "progress"].contains(&command)
            || (command == "verify" && !flag("content"));
        if command == "serve" {
            if flag("print-config") {
                return Err(CliError::new(
                    2,
                    "WRONG_PROJECT_TYPE",
                    "MCP launch settings are available for RAG/MCP projects.",
                ));
            }
            serve_wiki(state, &project, serving.unwrap_or_default())?;
            return Ok(Done::Quiet);
        }
        if !read_only {
            state.run_id = Some(new_run_id());
        }
        let mut input: Vec<ConfigValue> = positionals.iter().map(|item| text(item)).collect();
        if flag("persistent") {
            input.insert(0, ConfigValue::Null);
        }
        let request = wiki_request(
            env,
            &project,
            command,
            ConfigValue::Array(input),
            parsed.object(),
            state.run_id.as_deref(),
            !read_only,
            true,
        )?;
        let request: crate::wiki_project::WikiRequest = deserialize(&request, "Wiki")?;
        let outcome = crate::wiki_commands::run(&request, state.cancel).map_err(CliError::from)?;
        state.warnings.extend(outcome.warnings);
        return Ok(Done::Data(outcome.output));
    }
    if command == "build" || STAGES.contains(&command) {
        if !flag("dry-run") {
            state.run_id = Some(new_run_id());
        }
        return build_project(state, &project, parsed).map(Done::Data);
    }
    if command == "validate" {
        let stages = project
            .plan
            .iter()
            .map(|(stage, steps)| (stage.clone(), module_settings(steps)))
            .collect();
        return Ok(Done::Data(object(vec![
            ("valid", ConfigValue::Bool(true)),
            ("type", text(&project_type)),
            ("stages", ConfigValue::Object(stages)),
        ])));
    }
    if command == "use" {
        state.run_id = Some(new_run_id());
        return adopt_build(&project, &positionals[0]).map(Done::Data);
    }
    if command == "tag" {
        state.run_id = Some(new_run_id());
        return tag_build(
            &project,
            kind.as_deref().unwrap_or_default(),
            directory.as_deref(),
            flag("move"),
        )
        .map(Done::Data);
    }
    if command == "dictionary" {
        if kind.as_deref() == Some("add") {
            state.run_id = Some(new_run_id());
            let candidate = env.resolve(config_value.unwrap_or_default());
            let selected = directory.unwrap_or_default();
            return with_execution_lock(&project.paths, || {
                crate::cli_dictionary::add_dictionary(env, &project, &candidate, &selected)
            })
            .map(Done::Data);
        }
        return crate::cli_dictionary::suggest_from_report(
            env,
            &project,
            value("report"),
            state.cancel,
        )
        .map(Done::Data);
    }
    if command == "evidence-map" {
        if matches!(kind.as_deref(), Some("create" | "import" | "confirm")) {
            state.run_id = Some(new_run_id());
        }
        return evidence_map_command(
            state,
            &project,
            kind.as_deref().unwrap_or_default(),
            directory.as_deref(),
            parsed,
        )
        .map(Done::Data);
    }
    if ["eval-data", "eval", "report"].contains(&command) {
        let action = kind.clone().unwrap_or_default();
        let read_only = ["show", "list", "export", "diff"].contains(&action.as_str())
            || (action == "review"
                && parsed.value("case").is_none()
                && parsed.value("state").is_none());
        let locked = command == "eval" || (command == "eval-data" && action == "generate");
        let rest: Vec<String> = positionals.iter().skip(1).cloned().collect();
        let body = |state: &mut Run| -> Result<ConfigValue, CliError> {
            let connection = open_state(
                &project.paths,
                command == "report" || (command == "eval-data" && read_only),
            )?;
            if command == "report" {
                let request = object(vec![
                    ("state", text(&project.paths.state)),
                    ("action", text(&action)),
                    (
                        "refs",
                        ConfigValue::Array(rest.iter().map(|item| text(item)).collect()),
                    ),
                    ("split", value("split").map_or(ConfigValue::Null, text)),
                ]);
                let request = without_nulls(request, &["split"]);
                return crate::eval_report::reports(&deserialize(&request, "report")?)
                    .map_err(CliError::from);
            }
            if command == "eval-data" {
                if !read_only {
                    ensure_evaluation(&connection)?;
                    state.run_id = Some(new_run_id());
                }
                return eval_data_command(state, &project, &action, &rest, parsed);
            }
            evaluate_command(state, &project, &connection, parsed)
        };
        let data = if locked {
            with_execution_lock(&project.paths, || body(state))?
        } else {
            body(state)?
        };
        return Ok(Done::Data(data));
    }
    if command == "status" {
        return status_command(state, &project).map(Done::Data);
    }
    if command == "serve" && flag("print-config") {
        state.context = Some(object(vec![("project", text(&project.root))]));
        let serving = serving.unwrap_or_default();
        if string_field(&serving, "transport") == Some("http") {
            let endpoints = items(serving.get("endpoints"));
            if !endpoints
                .iter()
                .any(|endpoint| endpoint.as_str() == Some("mcp"))
                || serving.get("port") == Some(&ConfigValue::Number(0.0))
            {
                return Err(invalid(
                    "MCP registration requires an enabled MCP endpoint and a fixed HTTP port.",
                ));
            }
            return Ok(Done::Data(object(vec![(
                "url",
                text(&format!(
                    "{}/mcp",
                    serve_address(
                        &js_string(serving.get("host")),
                        &js_string(serving.get("port"))
                    )
                )),
            )])));
        }
        let mut args = vec![
            text(&env.cli),
            text("serve"),
            text("--project"),
            text(&project.root),
        ];
        if project_type == "rag" {
            args.push(text("--transport"));
            args.push(text("stdio"));
        }
        return Ok(Done::Data(object(vec![
            ("command", text(&env.exec_path)),
            ("args", ConfigValue::Array(args)),
        ])));
    }
    if command == "serve" {
        for warning in &state.warnings {
            print_error(&warning_line(warning));
        }
        serve_project(state, &project, serving.unwrap_or_default())?;
        return Ok(Done::Quiet);
    }
    // search and ask
    let build = current_build(&project.paths, true, value("build"))?.unwrap_or_default();
    let database = open_build(&project.paths, &build)?;
    let plan = if truthy(build.get("search")) && config_value.is_none() {
        restore_modules(build.get("search").unwrap_or(&ConfigValue::Null))
    } else {
        stage(&project.plan, "search").to_vec()
    };
    let model = if command == "ask" {
        answer_model(&project, false)?
    } else {
        None
    };
    let search_build = if config_value.is_some() {
        without(&build, "search_models")
    } else {
        build.clone()
    };
    let query = kind.unwrap_or_default();
    let result = search_project(
        state,
        &database,
        &search_build,
        &plan,
        &query,
        &project,
        &search_options,
    )?;
    if command == "ask" {
        state.context = Some(result.clone());
        let request = object(vec![
            ("plan", module_settings(stage(&project.plan, "ask"))),
            ("search", result),
            ("query", text(&query)),
            ("model", model.unwrap_or_default()),
        ]);
        let request: crate::generate::AnswerRequest = deserialize(&request, "answer")?;
        let host = NoCharge;
        return crate::generate::answer(&request, &host, state.cancel)
            .map(Done::Data)
            .map_err(|failure| with_module(CliError::from(failure.error), failure.module, "ask"));
    }
    Ok(Done::Data(result))
}

// ---------------------------------------------------------------------------------------------
// Helpers.

fn deserialize<T: serde::de::DeserializeOwned>(
    request: &ConfigValue,
    what: &str,
) -> Result<T, CliError> {
    serde_json::from_str(&request.to_js_json())
        .map_err(|error| CliError::plain(format!("Invalid {what} request: {error}")))
}

fn without(value: &ConfigValue, key: &str) -> ConfigValue {
    match value {
        ConfigValue::Object(entries) => ConfigValue::Object(
            entries
                .iter()
                .filter(|(name, _)| name != key)
                .cloned()
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Drops the named fields that are null, as JSON leaves out `undefined`.
fn without_nulls(value: ConfigValue, keys: &[&str]) -> ConfigValue {
    match value {
        ConfigValue::Object(entries) => ConfigValue::Object(
            entries
                .into_iter()
                .filter(|(name, value)| {
                    !(keys.contains(&name.as_str()) && matches!(value, ConfigValue::Null))
                })
                .collect(),
        ),
        other => other,
    }
}

fn optional(value: Option<&str>) -> ConfigValue {
    value.map_or(ConfigValue::Null, text)
}

fn with_module(mut error: CliError, module: Option<String>, section: &str) -> CliError {
    if let Some(module) = module {
        if error.module.is_none() {
            error.module = Some(module);
        }
        if error.section.is_none() {
            error.section = Some(section.into());
        }
    }
    error
}

fn sqlite(error: rusqlite::Error) -> CliError {
    let busy = matches!(&error, rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::DatabaseBusy);
    let mut failure = CliError::plain(error.to_string());
    if busy {
        failure.code = Some("SQLITE_BUSY".into());
    }
    failure
}

/// `openState(paths, readOnly)`
fn open_state(paths: &Paths, read_only: bool) -> Result<Connection, CliError> {
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
    };
    let connection = Connection::open_with_flags(&paths.state, flags).map_err(sqlite)?;
    connection
        .busy_timeout(std::time::Duration::from_millis(500))
        .map_err(sqlite)?;
    Ok(connection)
}

fn ensure_stages(connection: &Connection) -> Result<(), CliError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS stage_outputs (
      stage TEXT NOT NULL, id TEXT NOT NULL, manifest TEXT NOT NULL, PRIMARY KEY (stage, id));
    CREATE TABLE IF NOT EXISTS latest_stage (stage TEXT PRIMARY KEY, id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS build_tags (name TEXT PRIMARY KEY, build_id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS build_search (build_id TEXT PRIMARY KEY, settings TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS tag_search (name TEXT PRIMARY KEY, settings TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS build_config (ref TEXT PRIMARY KEY, config TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS adoption_history (id INTEGER PRIMARY KEY, previous TEXT NOT NULL, adopted TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS serving_generations (id INTEGER PRIMARY KEY, build_id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS tag_history (id INTEGER PRIMARY KEY, name TEXT NOT NULL, previous TEXT, target TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS build_runs (id TEXT PRIMARY KEY, build_id TEXT NOT NULL, config_id TEXT NOT NULL, created TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS index_outputs (key TEXT PRIMARY KEY, build_id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS source_checks (id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, checked_at TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS transform_outputs (key TEXT PRIMARY KEY, output_id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS embeddings (id TEXT PRIMARY KEY, data BLOB NOT NULL);",
        )
        .map_err(sqlite)
}

fn ensure_evaluation(connection: &Connection) -> Result<(), CliError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS evaluation_datasets (id TEXT PRIMARY KEY, data TEXT NOT NULL);\
CREATE TABLE IF NOT EXISTS evaluation_reports (id TEXT PRIMARY KEY, tag TEXT UNIQUE, data TEXT NOT NULL);\
CREATE TABLE IF NOT EXISTS evaluation_dataset_tags (name TEXT PRIMARY KEY, dataset_id TEXT NOT NULL);\
CREATE TABLE IF NOT EXISTS evidence_positions (id TEXT PRIMARY KEY, data TEXT NOT NULL);\
CREATE TABLE IF NOT EXISTS evidence_maps (id TEXT PRIMARY KEY, data TEXT NOT NULL);\
CREATE TABLE IF NOT EXISTS evidence_overrides (id TEXT PRIMARY KEY, data TEXT NOT NULL);",
        )
        .map_err(sqlite)
}

/// `withExecutionLock(paths, run)`
pub fn with_execution_lock<T>(
    paths: &Paths,
    run: impl FnOnce() -> Result<T, CliError>,
) -> Result<T, CliError> {
    use std::fs::TryLockError;
    let path = node_path::join(&paths.data, &["execution.lock"]);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|_| CliError::plain("Could not open the project execution lock"))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(CliError::new(
                4,
                "BUILD_BUSY",
                "Another project operation is running. Retry after it finishes.",
            ));
        }
        Err(TryLockError::Error(_)) => {
            return Err(CliError::plain(
                "Could not acquire the project execution lock",
            ));
        }
    }
    let result = run();
    let _ = file.unlock();
    result
}

fn read_utf8(path: &str) -> Result<String, CliError> {
    crate::wiki_project::read_file(path).map_err(CliError::from)
}

fn parse_json(text: &str) -> Result<ConfigValue, CliError> {
    serde_json::from_str(text).map_err(|error| CliError::plain(format!("{error}")))
}

fn models_of<'a>(project: &'a Project, build: Option<&'a ConfigValue>) -> Option<&'a ConfigValue> {
    build
        .and_then(|build| build.get("search_models"))
        .filter(|models| !matches!(models, ConfigValue::Null))
        .or_else(|| project.config.get("models"))
}

/// `modelSetting(project, build, name)`
fn project_model_setting(
    project: &Project,
    build: Option<&ConfigValue>,
    name: &ConfigValue,
) -> Result<ConfigValue, CliError> {
    let models = models_of(project, build);
    let name = js_string(Some(name));
    model_setting(models, &name)
        .map(Option::unwrap_or_default)
        .map_err(|()| {
            CliError::new(
                2,
                "INVALID_MODEL",
                "Check the model, endpoint and environment references.",
            )
        })
}

fn rerank_steps(plan: &[Step]) -> impl Iterator<Item = &Step> {
    plan.iter().filter(|step| step.module.role == "rerank")
}

fn model_arg(step: &Step) -> ConfigValue {
    step.arg("model").cloned().unwrap_or_default()
}

#[derive(Default)]
struct SearchOptions {
    limit: Option<f64>,
    budget_chars: Option<f64>,
    source_ids: Option<Vec<String>>,
}

fn search_settings(
    plan: &[Step],
    options: &SearchOptions,
    build: Option<&ConfigValue>,
    rerank: ConfigValue,
) -> ConfigValue {
    let mut entries = vec![("plan".to_owned(), module_settings(plan))];
    assign(
        &mut entries,
        "limit",
        options.limit.map(ConfigValue::Number),
    );
    assign(
        &mut entries,
        "budgetChars",
        options.budget_chars.map(ConfigValue::Number),
    );
    assign(
        &mut entries,
        "sourceIds",
        options
            .source_ids
            .as_ref()
            .map(|ids| ConfigValue::Array(ids.iter().map(|id| text(id)).collect())),
    );
    assign(
        &mut entries,
        "build",
        Some(build.cloned().unwrap_or(ConfigValue::Object(Vec::new()))),
    );
    assign(&mut entries, "rerankModels", Some(rerank));
    ConfigValue::Object(entries)
}

/// `searchConfigId(plan, options, build, project)`
fn search_config_id(
    plan: &[Step],
    options: &SearchOptions,
    build: Option<&ConfigValue>,
    project: &Project,
) -> Result<String, CliError> {
    let rerank = rerank_steps(plan)
        .map(|step| project_model_setting(project, build, &model_arg(step)))
        .collect::<Result<Vec<_>, _>>()?;
    let request = search_settings(plan, options, build, ConfigValue::Array(rerank));
    let settings: crate::search_stage::SearchSettings = deserialize(&request, "search settings")?;
    Ok(crate::search_stage::search_config_id(&settings, true))
}

fn model_error(code: Option<&str>, message: &str, exit: Option<u8>) -> ConfigValue {
    let mut entries = Vec::new();
    if let Some(code) = code {
        entries.push(("code", text(code)));
    }
    entries.push(("message", text(message)));
    if let Some(exit) = exit {
        entries.push(("exitCode", ConfigValue::Number(f64::from(exit))));
    }
    object(vec![("error", object(entries))])
}

/// `searchModels(plan, build, project)`: `{ embedders, rerankers, rerankModels }`.
fn search_models(
    plan: &[Step],
    build: &ConfigValue,
    project: &Project,
) -> Result<ConfigValue, CliError> {
    let models = models_of(project, Some(build)).cloned();
    let mut embedders: Vec<(String, ConfigValue)> = Vec::new();
    for step in plan {
        if !["retrieval.vector", "hannoy.search"].contains(&step.module.name) {
            continue;
        }
        let name = js_string(Some(&model_arg(step)));
        if embedders.iter().any(|(key, _)| *key == name) {
            continue;
        }
        let settings = match embedding_settings(models.as_ref(), &name) {
            Ok(settings) => settings,
            Err((exit, code, message)) => model_error(Some(code), message, Some(exit)),
        };
        embedders.push((name, settings));
    }
    let rerank_models = rerank_steps(plan)
        .map(|step| project_model_setting(project, Some(build), &model_arg(step)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut rerankers: Vec<(String, ConfigValue)> = Vec::new();
    for step in rerank_steps(plan) {
        let name = js_string(Some(&model_arg(step)));
        if rerankers.iter().any(|(key, _)| *key == name) {
            continue;
        }
        let spec = project_model_setting(project, Some(build), &model_arg(step))?;
        if matches!(spec, ConfigValue::Null) {
            continue;
        }
        let resolved = resolve_api_key(spec.get("apiKeyEnv"), "rerank model").and_then(|key| {
            generator_settings(
                string_field(&spec, "provider").unwrap_or_default(),
                string_field(&spec, "model"),
                string_field(&spec, "baseUrl"),
                key,
                spec.get("maxInputBytes"),
            )
        });
        rerankers.push((
            name,
            resolved.unwrap_or_else(|message| model_error(None, &message, None)),
        ));
    }
    Ok(object(vec![
        ("embedders", ConfigValue::Object(embedders)),
        ("rerankers", ConfigValue::Object(rerankers)),
        ("rerankModels", ConfigValue::Array(rerank_models)),
    ]))
}

/// A host that answers the index tool path and keeps the error it refused with.
struct ToolHost<'a> {
    env: &'a CliEnv,
    paths: &'a Paths,
    refused: Mutex<Option<CliError>>,
    trial: Option<Arc<crate::trial::Trial>>,
    config_changed: Option<Box<dyn Fn() -> Result<bool, CliError> + Send + Sync + 'a>>,
}

impl<'a> ToolHost<'a> {
    fn new(env: &'a CliEnv, paths: &'a Paths) -> Self {
        Self {
            env,
            paths,
            refused: Mutex::new(None),
            trial: None,
            config_changed: None,
        }
    }

    fn refuse<T>(&self, result: Result<T, CliError>) -> Result<T, String> {
        result.map_err(|error| {
            let message = error.message.clone();
            *self
                .refused
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(error);
            message
        })
    }

    /// `stageFailure(error, refused)`
    fn failure(&self, error: StageError) -> CliError {
        self.refused
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .unwrap_or_else(|| CliError::from(error))
    }
}

impl crate::source_load::Host for ToolHost<'_> {}

impl crate::embed::Charge for ToolHost<'_> {
    fn charge(&self, bytes: usize) -> Result<(), String> {
        match &self.trial {
            Some(trial) => trial.charge(bytes).map_err(|error| error.message),
            None => Ok(()),
        }
    }
}

impl crate::index_stage::IndexHost for ToolHost<'_> {
    fn config_changed(&self) -> Result<bool, String> {
        let changed = self
            .config_changed
            .as_ref()
            .map_or(Ok(false), |changed| changed());
        self.refuse(changed)
    }

    fn index_tool(&self) -> Result<String, String> {
        self.refuse(crate::cli_tools::index_tool_path(self.env, self.paths))
    }
}

impl crate::search_stage::SearchHost for ToolHost<'_> {
    fn index_tool(&self) -> Result<String, String> {
        crate::index_stage::IndexHost::index_tool(self)
    }
}

struct NoCharge;

impl crate::embed::Charge for NoCharge {
    fn charge(&self, _bytes: usize) -> Result<(), String> {
        Ok(())
    }
}

/// Starts the trial of a request and ends it with the outcome, as the Node.js API did.
fn start_trial(
    trial: Option<&ConfigValue>,
    cancel: &Cancellation,
) -> Result<Option<Arc<crate::trial::Trial>>, CliError> {
    let Some(trial) = trial else {
        return Ok(None);
    };
    let request: crate::trial::TrialRequest = deserialize(trial, "trial")?;
    crate::trial::Trial::start(&request, cancel)
        .map(Some)
        .map_err(CliError::from)
}

fn finish_trial<T>(
    trial: Option<&crate::trial::Trial>,
    outcome: Result<T, StageError>,
) -> Result<T, StageError> {
    let Some(trial) = trial else {
        return outcome;
    };
    match outcome {
        Ok(value) => {
            let _ = trial.finish(Ok(()));
            Ok(value)
        }
        Err(failure) => match trial.finish(Err(&failure)) {
            Some(limit) => Err(limit),
            None => Err(failure),
        },
    }
}

fn search_project(
    state: &Run,
    database: &crate::KnowledgeBase,
    build: &ConfigValue,
    plan: &[Step],
    query: &str,
    project: &Project,
    options: &SearchOptions,
) -> Result<ConfigValue, CliError> {
    let models = search_models(plan, build, project)?;
    let mut entries = vec![
        ("plan".to_owned(), module_settings(plan)),
        ("query".to_owned(), text(query)),
    ];
    assign(
        &mut entries,
        "limit",
        options.limit.map(ConfigValue::Number),
    );
    assign(
        &mut entries,
        "budgetChars",
        options.budget_chars.map(ConfigValue::Number),
    );
    assign(
        &mut entries,
        "sourceIds",
        options
            .source_ids
            .as_ref()
            .map(|ids| ConfigValue::Array(ids.iter().map(|id| text(id)).collect())),
    );
    assign(&mut entries, "build", Some(build.clone()));
    let directory = node_path::join(&project.paths.artifacts, &[&js_string(build.get("index"))]);
    assign(&mut entries, "directory", Some(text(&directory)));
    for key in ["embedders", "rerankModels", "rerankers"] {
        assign(&mut entries, key, models.get(key).cloned());
    }
    let request: crate::search_stage::SearchRequest =
        deserialize(&ConfigValue::Object(entries), "search")?;
    let host = ToolHost::new(state.env, &project.paths);
    let vectors = crate::search_stage::VectorCache::default();
    crate::search_stage::run(database, &vectors, &request, &host, state.cancel)
        .map(|outcome| outcome.output)
        .map_err(|failure| with_module(host.failure(failure.error), failure.module, "search"))
}

/// `answerModel(project, { optional })`: the settings of the answer model.
fn answer_model(project: &Project, optional: bool) -> Result<Option<ConfigValue>, CliError> {
    let Some(step) = stage(&project.plan, "ask").first() else {
        return Ok(None);
    };
    let name = js_string(step.arg("model"));
    let setting = project
        .config
        .get("models")
        .and_then(|models| models.get(&name))
        .cloned()
        .unwrap_or_default();
    let model = setting
        .get("model")
        .and_then(ConfigValue::as_str)
        .filter(|model| !blank(model));
    if model.is_none() {
        if optional {
            return Ok(None);
        }
        return Err(CliError::new(
            3,
            "MODEL_REQUIRED",
            format!("Set the model name in models.{name}.model to enable answer generation."),
        ));
    }
    let resolved = (|| {
        let model = resolve_value(setting.get("model"), "answer model")?;
        let base = resolve_value(setting.get("base_url"), "answer endpoint")?;
        let key = resolve_api_key(setting.get("api_key_env"), "answer model")?;
        generator_settings(
            string_field(&setting, "provider").unwrap_or_default(),
            model.as_deref(),
            base.as_deref(),
            key,
            setting.get("max_input_bytes"),
        )
    })();
    resolved.map(Some).map_err(|_| {
        CliError::new(
            2,
            "INVALID_MODEL",
            "Check the answer model, endpoint and credential environment references.",
        )
    })
}

// ---------------------------------------------------------------------------------------------
// init

/// `initialize(type, directory, values)`
fn initialize(
    env: &CliEnv,
    kind: &str,
    directory: &str,
    parsed: &Parsed,
) -> Result<ConfigValue, CliError> {
    let source = parsed.string("source");
    let purpose = parsed.string("purpose");
    let root = env.resolve(directory);
    let paths = crate::cli_project::project_paths(&root);
    let config_path = node_path::join(&root, &[crate::cli_project::CONFIG_NAME]);
    let legacy = node_path::join(&root, &["knowledge-nest.json"]);
    if crate::cli_project::exists(&config_path)?
        || crate::cli_project::exists(&legacy)?
        || crate::cli_project::exists(&paths.data)?
    {
        return Err(CliError::new(
            2,
            "PROJECT_EXISTS",
            format!("A project already exists at {root}"),
        ));
    }
    let path = match source {
        None => "./docs".to_owned(),
        Some(source) => {
            let location = if source.len() > 1
                && source.starts_with('~')
                && matches!(source.as_bytes()[1], b'/' | b'\\')
            {
                node_path::resolve(&env.home, &source[2..])
            } else {
                source.to_owned()
            };
            if !crate::cli_project::exists(&env.resolve(&location))? {
                return Err(CliError::new(
                    2,
                    "SOURCE_MISSING",
                    format!(
                        "Source folder not found: {}. Pass an existing folder to --source.",
                        env.resolve(&location)
                    ),
                ));
            }
            let relative = node_path::relative(&root, &env.resolve(&location));
            let relative = if cfg!(windows) {
                relative.replace('\\', "/")
            } else {
                relative
            };
            if relative.is_empty() {
                ".".into()
            } else {
                relative
            }
        }
    };
    let flag = |name: &str| parsed.flag(name);
    let value = |name: &str| parsed.string(name).map(str::to_owned);
    let (ocr, embedding, llm) = crate::cli_tools::initial_settings(
        env,
        kind,
        &crate::cli_tools::InitOptions {
            flag: &flag,
            value: &value,
        },
    )?;
    let mut config = if is_search_project(kind) {
        crate::cli_tools::rag_template(kind, &path, embedding.as_ref(), llm.as_ref())
    } else {
        crate::cli_tools::wiki_template(
            kind,
            crate::source_load::base_name(&root),
            purpose,
            llm.as_ref(),
        )
    };
    if let Some(tool) = &ocr {
        let ConfigValue::Object(entries) = &mut config else {
            unreachable!()
        };
        let section = if entries.iter().any(|(key, _)| key == "source") {
            "source"
        } else {
            "ingress"
        };
        if let Some((_, ConfigValue::Object(section))) =
            entries.iter_mut().find(|(key, _)| key == section)
            && let Some((_, ConfigValue::Array(modules))) =
                section.iter_mut().find(|(key, _)| key == "modules")
            && let Some(ConfigValue::Object(loader)) = modules
                .iter_mut()
                .find(|entry| string_field(entry, "use") == Some("loader.auto"))
            && let Some((_, ConfigValue::Object(with))) =
                loader.iter_mut().find(|(key, _)| key == "with")
        {
            assign(with, "ocr", Some(text("loader.ocr")));
        }
        let info = crate::cli_tools::find(tool).expect("catalog tool");
        entries.push((
            "tools".into(),
            ConfigValue::Array(vec![object(vec![
                ("name", text(info.name)),
                ("version", text(info.version)),
            ])]),
        ));
    }
    let mkdir = |path: &str| {
        std::fs::create_dir_all(path)
            .map_err(|error| CliError::from(fs_error("mkdir", path, &error)))
    };
    let write_new = |path: &str, content: &str, ignore_existing: bool| {
        use std::io::Write;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut file) => file
                .write_all(content.as_bytes())
                .map_err(|error| CliError::from(fs_error("write", path, &error))),
            Err(error) if ignore_existing && error.kind() == std::io::ErrorKind::AlreadyExists => {
                Ok(())
            }
            Err(error) => Err(CliError::from(fs_error("open", path, &error))),
        }
    };
    mkdir(&root)?;
    write_new(
        &config_path,
        &crate::cli_tools::write_template(&config),
        false,
    )?;
    if is_search_project(kind) && source.is_none() {
        mkdir(&node_path::join(&root, &["docs"]))?;
    }
    if !is_search_project(kind) {
        mkdir(&node_path::join(&root, &["site"]))?;
        write_new(
            &node_path::join(&root, &["rules.md"]),
            "# 運用の決まり\n",
            true,
        )?;
    }
    if kind == "para" {
        for category in ["Projects", "Areas", "Resources", "Archives"] {
            mkdir(&node_path::join(&root, &["site", category]))?;
        }
    }
    mkdir(&paths.artifacts)?;
    {
        let connection = open_state(&paths, false)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
      CREATE TABLE builds (id TEXT PRIMARY KEY, manifest TEXT NOT NULL);
      CREATE TABLE project (id INTEGER PRIMARY KEY CHECK (id = 1), build_id TEXT REFERENCES builds(id));
      INSERT INTO project VALUES (1, NULL);",
            )
            .map_err(sqlite)?;
        ensure_stages(&connection)?;
        ensure_evaluation(&connection)?;
    }
    if !is_search_project(kind) {
        crate::eval_stage::open_state(&paths.state, false)
            .and_then(|connection| crate::wiki_project::ensure(&connection))
            .map_err(CliError::from)?;
    }
    write_new(
        &node_path::join(&root, &[".gitignore"]),
        ".knowledge-nest/\n.env\n.env.*\n",
        true,
    )?;
    let llm_model = llm
        .as_ref()
        .and_then(|llm| string_field(llm, "model"))
        .is_some_and(|model| !model.is_empty());
    let next = match kind {
        "mcp" => "Add documents, then run nest build and nest serve --print-config.".to_owned(),
        "rag" => format!(
            "Add documents and run nest build. {}",
            if llm_model {
                "Run nest ask or nest serve."
            } else {
                "Set models.answerer.model before using ask or the responses endpoint."
            }
        ),
        _ => {
            let missing: Vec<&str> = [
                (!llm_model).then_some("models.writer.model"),
                purpose.is_none().then_some("about.purpose"),
            ]
            .into_iter()
            .flatten()
            .collect();
            if missing.is_empty() {
                "Run nest ingress <path>.".into()
            } else {
                format!("Set {}, then run nest ingress.", missing.join(" and "))
            }
        }
    };
    let next = match &ocr {
        Some(tool) => format!("{next} To prepare {tool}, run nest add ocr."),
        None => next,
    };
    Ok(object(vec![
        ("config", text(&config_path)),
        ("type", text(kind)),
        ("next", text(&next)),
    ]))
}

// ---------------------------------------------------------------------------------------------
// defaults

fn json_of(value: Option<&ConfigValue>) -> Option<String> {
    value.map(ConfigValue::to_js_json)
}

/// `compareDefaults(project, section)`
fn compare_defaults(project: &Project, section: Option<&str>) -> Result<ConfigValue, CliError> {
    let config = &project.config;
    let kind = project.kind.as_str();
    let search = is_search_project(kind);
    let standard = if search {
        let source = items(
            config
                .get("source")
                .and_then(|source| source.get("modules")),
        )
        .iter()
        .find(|entry| string_field(entry, "use") == Some("source.filesystem"))
        .and_then(|entry| entry.get("with"))
        .and_then(|with| with.get("path"))
        .filter(|path| !matches!(path, ConfigValue::Null))
        .map(|path| js_string(Some(path)));
        crate::cli_tools::rag_template(kind, source.as_deref().unwrap_or("./docs"), None, None)
    } else {
        let purpose = config
            .get("about")
            .and_then(|about| about.get("purpose"))
            .and_then(ConfigValue::as_str);
        crate::cli_tools::wiki_template(
            kind,
            crate::source_load::base_name(&project.root),
            purpose,
            None,
        )
    };
    let applicable: Vec<&str> = if search {
        let mut sections = STAGES.to_vec();
        sections.push("search");
        if kind == "rag" {
            sections.push("ask");
        }
        sections.push("serve");
        sections
    } else {
        vec!["ingress", "query", "verify", "archive", "serve"]
    };
    let differences: Vec<ConfigValue> = applicable
        .iter()
        .filter(|key| json_of(config.get(key)) != json_of(standard.get(key)))
        .map(|key| {
            let mut entries = vec![("section".to_owned(), text(key))];
            assign(&mut entries, "current", config.get(key).map(safe_config));
            assign(&mut entries, "proposed", standard.get(key).cloned());
            ConfigValue::Object(entries)
        })
        .collect();
    let Some(section) = section else {
        return Ok(object(vec![(
            "differences",
            ConfigValue::Array(differences),
        )]));
    };
    if !applicable.contains(&section) {
        return Err(invalid(format!(
            "Choose one standard module section: {}",
            applicable.join(", ")
        )));
    }
    let mut next = match config {
        ConfigValue::Object(entries) => entries.clone(),
        _ => Vec::new(),
    };
    assign(&mut next, section, standard.get(section).cloned());
    let next = ConfigValue::Object(next);
    let checked = if search {
        crate::cli_project::validate_rag(&next, &project.config_path, &[])
    } else {
        crate::cli_project::validate_wiki(
            &next,
            &project.config_path,
            &crate::cli_project::Parsed::empty(),
        )
    };
    if !checked.errors.is_empty() {
        return Err(CliError::new(
            2,
            "INVALID_CONFIG",
            "The resulting configuration would still be invalid.",
        )
        .with_diagnostics(checked.errors));
    }
    with_write_lock(&project.paths.data, || {
        if revision(read_utf8(&project.config_path)?.as_bytes()) != project.config_digest {
            return Err(CliError::new(
                4,
                "CONFIG_CHANGED",
                "The KnowledgeNestfile changed. Retry defaults.",
            ));
        }
        atomic_write(
            &project.config_path,
            crate::cli_tools::write_template(&next).as_bytes(),
        )
        .map_err(CliError::from)
    })?;
    Ok(object(vec![
        ("applied", text(section)),
        (
            "differences",
            ConfigValue::Array(
                differences
                    .into_iter()
                    .filter(|change| string_field(change, "section") == Some(section))
                    .collect(),
            ),
        ),
    ]))
}

// ---------------------------------------------------------------------------------------------
// Builds

/// `sourceSettings(project, plan, { ocr })`
fn source_settings(
    env: &CliEnv,
    project: &Project,
    plan: &[Step],
    ocr: bool,
) -> Result<ConfigValue, CliError> {
    let mut scans = Vec::new();
    let mut parsers: Vec<(ConfigValue, &str, ConfigValue)> = Vec::new();
    let mut reject = false;
    for step in plan {
        let category = match step.module.name {
            "source.filesystem" => {
                scans.push(object(vec![
                    ("path", step.arg("path").cloned().unwrap_or_default()),
                    (
                        "exclude",
                        step.arg("exclude")
                            .cloned()
                            .unwrap_or(ConfigValue::Array(Vec::new())),
                    ),
                    ("name", step.name.clone().unwrap_or_default()),
                ]));
                continue;
            }
            "loader.auto" => "auto",
            "loader.text" => "text",
            "loader.office" => "office",
            "loader.pdf" => "pdf",
            "loader.ocr" => "ocr",
            "loader.command" => "command",
            _ => continue,
        };
        let mut args = match &step.args {
            ConfigValue::Object(entries) => entries.clone(),
            _ => Vec::new(),
        };
        if category == "ocr" {
            assign(&mut args, "ocr", Some(text("loader.ocr")));
        }
        let args = ConfigValue::Object(args);
        reject = reject || string_field(&args, "unsupported") == Some("error");
        parsers.push((step.name.clone().unwrap_or_default(), category, args));
    }
    // loader.auto is the fallback, so it is matched last wherever it is written.
    let ordered: Vec<&(ConfigValue, &str, ConfigValue)> = parsers
        .iter()
        .filter(|(_, category, _)| *category != "auto")
        .chain(
            parsers
                .iter()
                .filter(|(_, category, _)| *category == "auto"),
        )
        .collect();
    let routes = ordered
        .iter()
        .map(|(name, category, args)| {
            let mut entries = vec![("name", name.clone()), ("category", text(category))];
            if let Some(matched) = args.get("match").filter(|matched| truthy(Some(matched))) {
                entries.push(("match", matched.clone()));
            }
            let reads = *category == "ocr"
                || (*category == "auto" && string_field(args, "ocr") == Some("loader.ocr"));
            entries.push(("ocr", ConfigValue::Bool(reads)));
            object(entries)
        })
        .collect();
    let commands = ordered
        .iter()
        .filter(|(_, category, _)| *category == "command")
        .map(|(name, _, args)| object(vec![("module", name.clone()), ("args", args.clone())]))
        .collect();
    let mut entries = vec![
        ("scans".to_owned(), ConfigValue::Array(scans)),
        ("reject".to_owned(), ConfigValue::Bool(reject)),
        ("modules".to_owned(), module_settings(plan)),
        ("routes".to_owned(), ConfigValue::Array(routes)),
        ("commands".to_owned(), ConfigValue::Array(commands)),
    ];
    if ocr
        && let ConfigValue::Object(fields) =
            crate::cli_tools::ocr_request(env, project.config.get("tools"))?
    {
        for (key, value) in fields {
            assign(&mut entries, &key, Some(value));
        }
    }
    Ok(ConfigValue::Object(entries))
}

fn embedding_failure((exit, code, message): (u8, &'static str, &'static str)) -> CliError {
    CliError::new(exit, code, message)
}

/// `buildConfiguration(project)`: the saved settings, without secrets and relative to the root.
fn build_configuration(project: &Project) -> Result<ConfigValue, CliError> {
    let mut configuration = safe_config(&project.config);
    if project.config_root != project.root {
        let prefix = node_path::relative(&project.root, &project.config_root).replace('\\', "/");
        for (section, name, arg) in [
            ("source", "source.filesystem", "path"),
            ("index", "index.keyword", "user_dictionary"),
        ] {
            let ConfigValue::Object(entries) = &mut configuration else {
                break;
            };
            let Some((_, ConfigValue::Object(section))) =
                entries.iter_mut().find(|(key, _)| key == section)
            else {
                continue;
            };
            let Some((_, ConfigValue::Array(modules))) =
                section.iter_mut().find(|(key, _)| key == "modules")
            else {
                continue;
            };
            for entry in modules.iter_mut() {
                if string_field(entry, "use") != Some(name) {
                    continue;
                }
                let ConfigValue::Object(fields) = entry else {
                    continue;
                };
                let Some((_, ConfigValue::Object(with))) =
                    fields.iter_mut().find(|(key, _)| key == "with")
                else {
                    continue;
                };
                let Some(path) = with
                    .iter()
                    .find(|(key, _)| key == arg)
                    .map(|(_, value)| value.clone())
                    .filter(|path| truthy(Some(path)))
                else {
                    continue;
                };
                let path = js_string(Some(&path));
                if path.contains("${") && !node_path::is_absolute(&path) {
                    let expanded = resolve_value(Some(&text(&path)), &format!("{name}.with.{arg}"))
                        .map_err(CliError::plain)?;
                    if !node_path::is_absolute(&expanded.unwrap_or_default()) {
                        assign(with, arg, Some(text(&format!("{prefix}/{path}"))));
                    }
                } else {
                    let relative = node_path::relative(
                        &project.root,
                        &node_path::resolve(&project.config_root, &path),
                    )
                    .replace('\\', "/");
                    assign(
                        with,
                        arg,
                        Some(text(if relative.is_empty() { "." } else { &relative })),
                    );
                }
            }
        }
    }
    Ok(configuration)
}

/// `indexSettings(project, { resolve })`
fn index_settings(project: &Project, resolved: bool) -> Result<ConfigValue, CliError> {
    let plan = stage(&project.plan, "index");
    let mut keyword = None;
    let mut vector = None;
    for step in plan {
        match step.module.name {
            "index.keyword" => keyword = Some(step.args.clone()),
            "index.vector" => {
                vector = Some(object(vec![
                    ("backend", text("exact")),
                    ("settings", step.args.clone()),
                ]))
            }
            "hannoy.index" => {
                vector = Some(object(vec![
                    ("backend", text("hannoy")),
                    ("settings", step.args.clone()),
                ]))
            }
            _ => {}
        }
    }
    let mut entries = Vec::new();
    assign(&mut entries, "keyword", keyword);
    assign(&mut entries, "vector", vector.clone());
    assign(&mut entries, "construction", Some(module_settings(plan)));
    if !resolved {
        return Ok(ConfigValue::Object(entries));
    }
    let embedder = match &vector {
        Some(vector) => {
            let name = js_string(
                vector
                    .get("settings")
                    .and_then(|settings| settings.get("model")),
            );
            embedding_settings(project.config.get("models"), &name).map_err(embedding_failure)?
        }
        None => ConfigValue::Null,
    };
    assign(&mut entries, "embedder", Some(embedder));
    let search = stage(&project.plan, "search");
    assign(&mut entries, "search", Some(module_settings(search)));
    let rerank = rerank_steps(search)
        .map(|step| project_model_setting(project, None, &model_arg(step)))
        .collect::<Result<Vec<_>, _>>()?;
    assign(&mut entries, "rerank", Some(ConfigValue::Array(rerank)));
    assign(
        &mut entries,
        "configuration",
        Some(build_configuration(project)?),
    );
    Ok(ConfigValue::Object(entries))
}

/// `buildConfigChanged(project, input)`
fn build_config_changed(project: &Project, input: &str) -> bool {
    if revision(input.as_bytes()) == project.config_digest {
        return false;
    }
    let Ok(parsed) = crate::cli_project::parse_config(input) else {
        return true;
    };
    let config = crate::cli_project::normalize_type(parsed.config, None);
    if !matches!(config, ConfigValue::Object(_) | ConfigValue::Array(_)) {
        return true;
    }
    [
        "init",
        "source",
        "transform",
        "index",
        "search",
        "models",
        "tools",
    ]
    .iter()
    .any(|key| json_of(config.get(key)) != json_of(project.config.get(key)))
}

/// `buildProject(project, command, runId, values, warnings)`
fn build_project(
    state: &mut Run,
    project: &Project,
    parsed: &Parsed,
) -> Result<ConfigValue, CliError> {
    let command = state.command;
    let value = |name: &str| parsed.string(name);
    let selected: Vec<&str> = if command == "build" {
        let start = value("from")
            .and_then(|from| STAGES.iter().position(|stage| *stage == from))
            .unwrap_or(0);
        let end = value("until")
            .and_then(|until| STAGES.iter().position(|stage| *stage == until))
            .map_or(STAGES.len(), |index| index + 1);
        STAGES
            .get(start..end)
            .map(<[&str]>::to_vec)
            .unwrap_or_default()
    } else {
        vec![command]
    };
    let dry_run = parsed.flag("dry-run");
    let started = std::time::Instant::now();
    let env = state.env;
    let cancel = state.cancel;
    let run_id = state.run_id.clone();
    let run = |trial: Option<ConfigValue>,
               warnings: &mut Vec<ConfigValue>|
     -> Result<ConfigValue, CliError> {
        let semantic = stage(&project.plan, "transform")
            .iter()
            .find(|step| step.module.name == "chunk.semantic");
        let transform_embedder = match semantic {
            Some(semantic) if !dry_run && selected.contains(&"transform") => embedding_settings(
                project.config.get("models"),
                &js_string(semantic.arg("model")),
            )
            .map_err(embedding_failure)?,
            _ => ConfigValue::Null,
        };
        let mut request = vec![("command".to_owned(), text(command))];
        assign(&mut request, "runId", run_id.as_deref().map(text));
        for (key, option) in [
            ("from", "from"),
            ("until", "until"),
            ("input", "input"),
            ("tag", "tag"),
        ] {
            assign(&mut request, key, value(option).map(text));
        }
        assign(
            &mut request,
            "candidate",
            Some(ConfigValue::Bool(parsed.value("config").is_some())),
        );
        assign(
            &mut request,
            "force",
            Some(ConfigValue::Bool(parsed.flag("force"))),
        );
        assign(
            &mut request,
            "skipUnsupported",
            Some(ConfigValue::Bool(parsed.flag("skip-unsupported"))),
        );
        assign(&mut request, "dryRun", Some(ConfigValue::Bool(dry_run)));
        assign(&mut request, "state", Some(text(&project.paths.state)));
        assign(
            &mut request,
            "artifacts",
            Some(text(&project.paths.artifacts)),
        );
        assign(&mut request, "corpus", Some(text(&project.paths.corpus)));
        assign(&mut request, "root", Some(text(&project.root)));
        assign(
            &mut request,
            "source",
            Some(source_settings(
                env,
                project,
                stage(&project.plan, "source"),
                !dry_run && selected.first() == Some(&"source"),
            )?),
        );
        assign(
            &mut request,
            "transform",
            Some(object(vec![
                (
                    "modules",
                    module_settings(stage(&project.plan, "transform")),
                ),
                ("embedder", transform_embedder),
            ])),
        );
        assign(
            &mut request,
            "index",
            Some(index_settings(
                project,
                !dry_run && selected.contains(&"index"),
            )?),
        );
        assign(
            &mut request,
            "configIndex",
            project.config.get("index").cloned(),
        );
        assign(&mut request, "trial", trial.clone());
        assign(
            &mut request,
            "elapsedMs",
            Some(ConfigValue::Number(
                started.elapsed().as_secs_f64() * 1000.0,
            )),
        );
        let request: crate::build_stage::BuildRequest =
            deserialize(&ConfigValue::Object(request), "build")?;
        let mut host = ToolHost::new(env, &project.paths);
        host.config_changed = Some(Box::new(|| {
            Ok(build_config_changed(
                project,
                &read_utf8(&project.config_path)?,
            ))
        }));
        host.trial = start_trial(trial.as_ref(), cancel)?;
        let outcome = crate::build_stage::run(request, &host, cancel);
        let outcome = finish_trial(host.trial.as_deref(), outcome);
        match outcome {
            Ok(outcome) => {
                warnings.extend(outcome.warnings);
                Ok(outcome.output)
            }
            Err(error) => Err(host.failure(error)),
        }
    };
    if dry_run {
        return run(None, &mut state.warnings);
    }
    let warnings = &mut state.warnings;
    with_execution_lock(&project.paths, || {
        let connection = open_state(&project.paths, false)?;
        ensure_stages(&connection)?;
        drop(connection);
        let trial = (parsed.value("tag").is_some_and(|tag| truthy(Some(tag)))
            || parsed
                .value("config")
                .is_some_and(|config| truthy(Some(config))))
        .then(|| {
            object(vec![
                ("state", text(&project.paths.state)),
                ("id", optional(run_id.as_deref())),
                ("operation", text(command)),
                ("limits", project.trial_limits.clone()),
            ])
        });
        run(trial, warnings)
    })
}

// ---------------------------------------------------------------------------------------------
// Adoption and tags

struct AdoptHost<'a> {
    write: Box<dyn Fn() -> Result<(), CliError> + 'a>,
    restore: Box<dyn Fn() -> Result<(), CliError> + 'a>,
    refused: Mutex<Option<CliError>>,
}

impl crate::adoption::AdoptHost for AdoptHost<'_> {
    fn write_config(&self) -> Result<(), String> {
        (self.write)().map_err(|error| {
            let message = error.message.clone();
            *self
                .refused
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(error);
            message
        })
    }

    fn restore_config(&self) -> Result<(), String> {
        (self.restore)().map_err(|error| error.message)
    }
}

fn stored_config(connection: &Connection, reference: &str) -> Result<Option<String>, CliError> {
    connection
        .query_row(
            "SELECT config FROM build_config WHERE ref = ?1",
            [reference],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite)
}

/// `adoptBuild(project, reference)`
fn adopt_build(project: &Project, reference: &str) -> Result<ConfigValue, CliError> {
    let build = current_build(&project.paths, true, Some(reference))?.unwrap_or_default();
    let connection = open_state(&project.paths, false)?;
    let build_id = js_string(build.get("build_id"));
    let stored = match stored_config(&connection, &format!("tag:{reference}"))? {
        Some(stored) => Some(stored),
        None => stored_config(&connection, &build_id)?,
    };
    let Some(stored) = stored else {
        return Err(CliError::new(
            3,
            "CONFIG_REQUIRED",
            "This build has no saved configuration. Build the candidate again.",
        ));
    };
    let ConfigValue::Object(mut config) = parse_json(&stored)? else {
        return Err(CliError::plain("The saved configuration is not an object."));
    };
    for section in ["init", "serve", "ask", "eval-data", "eval", "trial_limits"] {
        assign(&mut config, section, project.config.get(section).cloned());
    }
    if let Some(answerer) = stage(&project.plan, "ask")
        .first()
        .and_then(|step| step.arg("model"))
    {
        let name = js_string(Some(answerer));
        let mut models = match config.iter().find(|(key, _)| key == "models") {
            Some((_, ConfigValue::Object(models))) => models.clone(),
            _ => Vec::new(),
        };
        assign(
            &mut models,
            &name,
            project
                .config
                .get("models")
                .and_then(|models| models.get(&name))
                .cloned(),
        );
        assign(&mut config, "models", Some(ConfigValue::Object(models)));
    }
    let config = ConfigValue::Object(config);
    let adopted = crate::cli_project::validate_rag(&config, &project.config_path, &[]);
    if !adopted.errors.is_empty() {
        return Err(CliError::new(
            2,
            "INVALID_CONFIG",
            "The adopted settings cannot be resolved. Run validate for the candidate.",
        ));
    }
    let before = read_utf8(&project.config_path)?;
    if revision(before.as_bytes()) != project.config_digest {
        return Err(CliError::new(
            4,
            "CONFIG_CHANGED",
            "The configuration changed. Retry use.",
        ));
    }
    let mut adopted_project = project.clone();
    adopted_project.config = config.clone();
    let search_id = search_config_id(
        stage(&adopted.plan, "search"),
        &SearchOptions::default(),
        Some(&build),
        &adopted_project,
    )?;
    let result = with_write_lock(&project.paths.data, || {
        let replaced = std::cell::Cell::new(false);
        let host = AdoptHost {
            write: Box::new(|| {
                if read_utf8(&project.config_path)? != before {
                    return Err(CliError::new(
                        4,
                        "CONFIG_CHANGED",
                        "The configuration changed. Retry use.",
                    ));
                }
                atomic_write(
                    &project.config_path,
                    crate::cli_tools::write_template(&config).as_bytes(),
                )
                .map_err(CliError::from)?;
                replaced.set(true);
                Ok(())
            }),
            restore: Box::new(|| {
                if replaced.get() {
                    atomic_write(&project.config_path, before.as_bytes())
                        .map_err(CliError::from)?;
                }
                Ok(())
            }),
            refused: Mutex::new(None),
        };
        let mut request = vec![
            ("state".to_owned(), text(&project.paths.state)),
            ("buildId".to_owned(), text(&build_id)),
            ("searchId".to_owned(), text(&search_id)),
            (
                "previous".to_owned(),
                text(&safe_config(&project.config).to_js_json()),
            ),
            (
                "adopted".to_owned(),
                text(&safe_config(&config).to_js_json()),
            ),
        ];
        assign(
            &mut request,
            "dataset",
            config
                .get("eval-data")
                .and_then(|data| data.get("use"))
                .cloned(),
        );
        assign(
            &mut request,
            "settings",
            Some(adopted.eval_settings.clone().unwrap_or_default()),
        );
        let request: crate::adoption::AdoptRequest =
            deserialize(&ConfigValue::Object(request), "adoption")?;
        crate::adoption::adopt(&request, &host).map_err(|error| {
            host.refused
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
                .unwrap_or_else(|| CliError::from(error))
        })
    });
    drop(connection);
    result
}

struct TagHost<'a> {
    project: &'a Project,
    refused: Mutex<Option<CliError>>,
}

impl crate::build_stage::TagHost for TagHost<'_> {
    fn search_config_id(&self, build: &ConfigValue) -> Result<String, String> {
        let plan = restore_modules(build.get("search").unwrap_or(&ConfigValue::Null));
        search_config_id(&plan, &SearchOptions::default(), Some(build), self.project).map_err(
            |error| {
                let message = error.message.clone();
                *self
                    .refused
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(error);
                message
            },
        )
    }
}

/// `tagBuild(project, name, ref, move)`
fn tag_build(
    project: &Project,
    name: &str,
    reference: Option<&str>,
    moved: bool,
) -> Result<ConfigValue, CliError> {
    ensure_stages(&open_state(&project.paths, false)?)?;
    let mut request = vec![
        ("state".to_owned(), text(&project.paths.state)),
        ("name".to_owned(), text(name)),
    ];
    assign(&mut request, "reference", reference.map(text));
    assign(&mut request, "move", Some(ConfigValue::Bool(moved)));
    assign(
        &mut request,
        "search",
        Some(module_settings(stage(&project.plan, "search"))),
    );
    assign(
        &mut request,
        "configSearch",
        project.config.get("search").cloned(),
    );
    assign(
        &mut request,
        "configModels",
        project.config.get("models").cloned(),
    );
    let request: crate::build_stage::TagRequest =
        deserialize(&ConfigValue::Object(request), "tag")?;
    let host = TagHost {
        project,
        refused: Mutex::new(None),
    };
    crate::build_stage::tag(&request, &host).map_err(|error| {
        host.refused
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .unwrap_or_else(|| CliError::from(error))
    })
}

// ---------------------------------------------------------------------------------------------
// Evaluation

fn write_new_file(path: &str, content: &str) -> Result<(), CliError> {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| file.write_all(content.as_bytes()))
        .map_err(|error| CliError::from(fs_error("open", path, &error)))
}

/// `evidenceMapCommand(project, action, reference, values)`
fn evidence_map_command(
    state: &Run,
    project: &Project,
    action: &str,
    reference: Option<&str>,
    parsed: &Parsed,
) -> Result<ConfigValue, CliError> {
    let output = parsed.string("output");
    if action == "export" && output.is_none_or(str::is_empty) {
        return Err(CliError::new(
            2,
            "INVALID_EVAL_DATA",
            "Use --output <file>.",
        ));
    }
    let mut request = vec![
        ("state".to_owned(), text(&project.paths.state)),
        (
            "action".to_owned(),
            text(if action == "export" { "show" } else { action }),
        ),
    ];
    assign(&mut request, "reference", reference.map(text));
    assign(
        &mut request,
        "dataset",
        project
            .config
            .get("eval-data")
            .and_then(|data| data.get("use"))
            .cloned(),
    );
    for key in ["key", "start", "end", "reviewer"] {
        assign(&mut request, key, parsed.string(key).map(text));
    }
    let result = if ["create", "import", "confirm"].contains(&action) {
        let build =
            current_build(&project.paths, true, parsed.string("build"))?.unwrap_or_default();
        let database = open_build(&project.paths, &build)?;
        assign(&mut request, "build", Some(build));
        let request: crate::eval_stage::EvidenceMapRequest =
            deserialize(&ConfigValue::Object(request), "evidence map")?;
        crate::eval_stage::evidence_maps(Some(&database), &request, state.cancel)
    } else {
        let request: crate::eval_stage::EvidenceMapRequest =
            deserialize(&ConfigValue::Object(request), "evidence map")?;
        crate::eval_stage::evidence_maps(None, &request, state.cancel)
    }
    .map_err(CliError::from)?;
    if action != "export" {
        return Ok(result);
    }
    write_new_file(output.unwrap_or_default(), &(pretty_json(&result) + "\n"))?;
    Ok(object(vec![(
        "id",
        result.get("id").cloned().unwrap_or_default(),
    )]))
}

/// `generatorChoice(project, values)`: the chat model of `eval-data generate`.
fn generator_choice(project: &Project, parsed: &Parsed) -> ConfigValue {
    let models = project.config.get("models");
    let chat: Vec<&String> = match models {
        Some(ConfigValue::Object(entries)) => entries
            .iter()
            .filter(|(_, model)| {
                string_field(model, "kind") == Some("chat") && truthy(model.get("model"))
            })
            .map(|(name, _)| name)
            .collect(),
        _ => Vec::new(),
    };
    let name = parsed
        .string("model")
        .map(str::to_owned)
        .or_else(|| (chat.len() == 1).then(|| chat[0].clone()));
    let Some(name) = name else {
        return object(Vec::new());
    };
    let model = models
        .and_then(|models| models.get(&name))
        .cloned()
        .unwrap_or_default();
    if string_field(&model, "kind") != Some("chat") || !truthy(model.get("model")) {
        return object(Vec::new());
    }
    let settings = (|| {
        let resolved = resolve_value(model.get("model"), "model")?;
        let base = resolve_value(model.get("base_url"), "base_url")?;
        let key = resolve_api_key(model.get("api_key_env"), "model")?;
        generator_settings(
            string_field(&model, "provider").unwrap_or_default(),
            resolved.as_deref(),
            base.as_deref(),
            key,
            model.get("max_input_bytes"),
        )
    })();
    match settings {
        Ok(settings) => object(vec![("name", text(&name)), ("settings", settings)]),
        Err(_) => object(vec![("name", text(&name))]),
    }
}

/// `selectDataset(project, id)`: the selected evaluation data, written into the KnowledgeNestfile.
fn select_dataset(project: &Project, id: &str) -> Result<(), CliError> {
    with_write_lock(&project.paths.data, || {
        let current = read_utf8(&project.config_path)?;
        if revision(current.as_bytes()) != project.config_digest {
            return Err(CliError::new(
                4,
                "CONFIG_CHANGED",
                "The settings changed; retry the selection.",
            ));
        }
        let mut config = match &project.config {
            ConfigValue::Object(entries) => entries.clone(),
            _ => Vec::new(),
        };
        assign(
            &mut config,
            "eval-data",
            Some(object(vec![("use", text(&format!("eval-data:{id}")))])),
        );
        if config
            .iter()
            .find(|(key, _)| key == "eval")
            .is_none_or(|(_, value)| matches!(value, ConfigValue::Null))
        {
            let eval = parse_json(
                r#"{"split":"dev","modules":[{"use":"metric.evidence_recall","with":{"at":["context",10],"short_evidence_chars":40,"min_coverage":0.8,"unresolved":"count_as_missed"}},{"use":"metric.latency","with":{"percentiles":[50,95,99]}}]}"#,
            )?;
            assign(&mut config, "eval", Some(eval));
        }
        let yaml = crate::yaml_emit::stringify(&ConfigValue::Object(config));
        atomic_write(&project.config_path, yaml.as_bytes()).map_err(CliError::from)
    })
}

/// `evalDataCommand(project, action, [positional, other], values)`
fn eval_data_command(
    state: &Run,
    project: &Project,
    action: &str,
    rest: &[String],
    parsed: &Parsed,
) -> Result<ConfigValue, CliError> {
    let positional = rest.first();
    let mut request = vec![
        ("state".to_owned(), text(&project.paths.state)),
        ("action".to_owned(), text(action)),
    ];
    assign(
        &mut request,
        "positional",
        positional.map(|item| text(item)),
    );
    assign(&mut request, "other", rest.get(1).map(|item| text(item)));
    assign(
        &mut request,
        "selected",
        project
            .config
            .get("eval-data")
            .and_then(|data| data.get("use"))
            .cloned(),
    );
    assign(&mut request, "values", Some(parsed.object()));
    if action == "generate" {
        assign(
            &mut request,
            "generator",
            Some(generator_choice(project, parsed)),
        );
    }
    let with_build = |mut request: Vec<(String, ConfigValue)>| -> Result<crate::eval_data::EvalDataOutcome, CliError> {
        let build = current_build(&project.paths, true, None)?.unwrap_or_default();
        let database = open_build(&project.paths, &build)?;
        if action == "import" {
            assign(&mut request, "input", Some(text(&read_utf8(positional.map_or("undefined", String::as_str))?)));
        }
        let request: crate::eval_data::EvalDataRequest = deserialize(&ConfigValue::Object(request), "eval-data")?;
        crate::eval_data::eval_data(Some(&database), &request, state.cancel).map_err(CliError::from)
    };
    let outcome = if ["add", "import", "sync", "generate"].contains(&action) {
        with_build(request.clone())?
    } else {
        let parsed_request: crate::eval_data::EvalDataRequest =
            deserialize(&ConfigValue::Object(request.clone()), "eval-data")?;
        match crate::eval_data::eval_data(None, &parsed_request, state.cancel) {
            Err(error) if error.code == Some("BUILD_NEEDED") => with_build(request)?,
            other => other.map_err(CliError::from)?,
        }
    };
    if let Some(id) = &outcome.select {
        select_dataset(project, id)?;
    }
    if action != "export" {
        return Ok(outcome.output);
    }
    let output = parsed.string("output").filter(|output| !output.is_empty());
    let Some(output) = output else {
        return Err(CliError::new(
            2,
            "INVALID_EVAL_DATA",
            "Use --output <file>.",
        ));
    };
    let cases = items(outcome.output.get("cases"));
    let lines: Vec<String> = cases.iter().map(ConfigValue::to_js_json).collect();
    write_new_file(output, &(lines.join("\n") + "\n"))?;
    Ok(object(vec![
        ("id", outcome.output.get("id").cloned().unwrap_or_default()),
        ("cases", ConfigValue::Number(cases.len() as f64)),
    ]))
}

fn latest_stage_id(connection: &Connection, stage: &str) -> Result<Option<String>, CliError> {
    connection
        .query_row(
            "SELECT id FROM latest_stage WHERE stage = ?1",
            [stage],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite)
}

/// `nest eval`
fn evaluate_command(
    state: &mut Run,
    project: &Project,
    connection: &Connection,
    parsed: &Parsed,
) -> Result<ConfigValue, CliError> {
    let Some(eval_settings) = project.eval_settings.clone() else {
        return Err(CliError::new(
            3,
            "EVAL_DATA_REQUIRED",
            "Add evaluation data before running eval.",
        ));
    };
    let config_value = parsed.string("config");
    let build = current_build(&project.paths, true, parsed.string("build"))?.unwrap_or_default();
    let latest = latest_stage_id(connection, "source")?;
    if let Some(latest) = latest
        && Some(latest.as_str()) != build.get("source_id").and_then(ConfigValue::as_str)
    {
        state.warnings.push(object(vec![
            ("code", text("OLDER_SOURCE_SNAPSHOT")),
            (
                "message",
                text("The selected build uses an earlier saved source snapshot."),
            ),
        ]));
    }
    let plan = if truthy(build.get("search")) && config_value.is_none() {
        restore_modules(build.get("search").unwrap_or(&ConfigValue::Null))
    } else {
        stage(&project.plan, "search").to_vec()
    };
    let top = plan
        .iter()
        .filter(|step| step.module.role == "retrieval")
        .map(|step| match step.arg("top_k") {
            Some(ConfigValue::Number(value)) => *value,
            _ => f64::NAN,
        })
        .fold(f64::NEG_INFINITY, |max, value| {
            if value.is_nan() || max.is_nan() {
                f64::NAN
            } else {
                max.max(value)
            }
        });
    let conflict = items(
        eval_settings
            .get("recall")
            .and_then(|recall| recall.get("at")),
    )
    .iter()
    .any(|at| matches!(at, ConfigValue::Number(at) if *at > top));
    if conflict {
        return Err(CliError::new(
            2,
            "ARG_CONFLICT",
            "Evaluation uses k greater than search top_k.",
        ));
    }
    let build_id = js_string(build.get("build_id"));
    let stored = stored_config(connection, &build_id)?;
    let config = match stored {
        Some(stored) => parse_json(&stored)?,
        None => object(Vec::new()),
    };
    let mut settings = match &eval_settings {
        ConfigValue::Object(entries) => entries.clone(),
        _ => Vec::new(),
    };
    let budget = plan
        .iter()
        .find(|step| step.module.role == "context")
        .and_then(|step| step.arg("budget_chars"))
        .cloned();
    assign(&mut settings, "contextBudget", budget);
    let construction = STAGES
        .iter()
        .filter_map(|stage| {
            build
                .get("construction")
                .and_then(|construction| construction.get(stage))
                .filter(|value| !matches!(value, ConfigValue::Null))
                .or_else(|| config.get(stage))
                .cloned()
                .map(|value| ((*stage).to_owned(), value))
        })
        .collect();
    assign(
        &mut settings,
        "construction",
        Some(ConfigValue::Object(construction)),
    );
    assign(&mut settings, "searchModules", Some(module_settings(&plan)));
    let run_id = new_run_id();
    state.run_id = Some(run_id.clone());
    let database = open_build(&project.paths, &build)?;
    let search_build = if config_value.is_some() {
        without(&build, "search_models")
    } else {
        build.clone()
    };
    let models = search_models(&plan, &search_build, project)?;
    let mut search = vec![
        ("plan".to_owned(), module_settings(&plan)),
        ("query".to_owned(), text("")),
        ("build".to_owned(), search_build.clone()),
        (
            "directory".to_owned(),
            text(&node_path::join(
                &project.paths.artifacts,
                &[&js_string(build.get("index"))],
            )),
        ),
    ];
    for key in ["embedders", "rerankModels", "rerankers"] {
        assign(&mut search, key, models.get(key).cloned());
    }
    let trial = (parsed
        .string("build")
        .is_some_and(|build| !build.is_empty())
        || config_value.is_some_and(|config| !config.is_empty()))
    .then(|| {
        object(vec![
            ("state", text(&project.paths.state)),
            ("id", text(&run_id)),
            ("operation", text("eval")),
            ("limits", project.trial_limits.clone()),
        ])
    });
    let mut request = vec![
        ("search".to_owned(), ConfigValue::Object(search)),
        ("state".to_owned(), text(&project.paths.state)),
        ("build".to_owned(), build.clone()),
    ];
    assign(
        &mut request,
        "dataset",
        project
            .config
            .get("eval-data")
            .and_then(|data| data.get("use"))
            .cloned(),
    );
    assign(
        &mut request,
        "settings",
        Some(ConfigValue::Object(settings)),
    );
    assign(&mut request, "split", parsed.string("split").map(text));
    assign(&mut request, "tag", parsed.string("tag").map(text));
    assign(&mut request, "runId", Some(text(&run_id)));
    assign(&mut request, "trial", trial.clone());
    let mut host = ToolHost::new(state.env, &project.paths);
    host.trial = start_trial(trial.as_ref(), state.cancel)?;
    let request: crate::eval_stage::EvaluateRequest =
        deserialize(&ConfigValue::Object(request), "evaluation")?;
    let vectors = crate::search_stage::VectorCache::default();
    let outcome = crate::eval_stage::evaluate(&database, &vectors, request, &host, state.cancel);
    let module = match &outcome {
        Err(failure) => failure.module.clone(),
        Ok(_) => None,
    };
    let outcome = finish_trial(
        host.trial.as_deref(),
        outcome.map_err(|failure| failure.error),
    );
    outcome.map_err(|error| with_module(host.failure(error), module, "search"))
}

// ---------------------------------------------------------------------------------------------
// status

/// `readStage(db, stage, id)`
fn read_stage(
    connection: &Connection,
    stage: &str,
    id: Option<&str>,
) -> Result<Option<ConfigValue>, CliError> {
    let row: Option<String> = match id {
        None => connection
            .query_row(
                "SELECT manifest FROM stage_outputs JOIN latest_stage USING (stage, id) WHERE stage = ?1",
                [stage],
                |row| row.get(0),
            )
            .optional(),
        Some(id) => connection
            .query_row("SELECT manifest FROM stage_outputs WHERE stage = ?1 AND id = ?2", [stage, id], |row| row.get(0))
            .optional(),
    }
    .map_err(sqlite)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let mut output = parse_json(&row)?;
    if output.get("payloads") != Some(&ConfigValue::Number(1.0)) {
        return Ok(Some(output));
    }
    let get = |key: &ConfigValue| -> Result<ConfigValue, CliError> {
        let data: Option<String> = connection
            .query_row(
                "SELECT data FROM stage_payloads WHERE id = ?1",
                [js_string(Some(key))],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite)?;
        let data = data.ok_or_else(|| {
            CliError::new(3, "STAGE_REQUIRED", "A saved stage payload is missing.")
        })?;
        parse_json(&data)
    };
    if stage == "source"
        && let ConfigValue::Object(entries) = &mut output
    {
        let groups = entries
            .iter()
            .find(|(key, _)| key == "groups")
            .map(|(_, groups)| groups.clone());
        let mut next = Vec::new();
        for group in items(groups.as_ref()) {
            let source = items(Some(group)).first().cloned().unwrap_or_default();
            let mut documents = Vec::new();
            for document in items(items(Some(group)).get(1)) {
                let ConfigValue::Object(fields) = document else {
                    continue;
                };
                let key = document.get("text").cloned().unwrap_or_default();
                let mut fields: Vec<(String, ConfigValue)> = fields
                    .iter()
                    .filter(|(name, _)| name != "text")
                    .cloned()
                    .collect();
                assign(&mut fields, "content", Some(get(&key)?));
                documents.push(ConfigValue::Object(fields));
            }
            next.push(ConfigValue::Array(vec![
                source,
                ConfigValue::Array(documents),
            ]));
        }
        assign(entries, "groups", Some(ConfigValue::Array(next)));
    }
    Ok(Some(output))
}

/// `earlierLayout(source)`
fn earlier_layout(source: Option<&ConfigValue>) -> bool {
    let Some(source) = source else {
        return false;
    };
    truthy(source.get("migrated"))
        || items(source.get("groups")).iter().any(|group| {
            items(items(Some(group)).get(1))
                .iter()
                .any(|document| document.get("version").is_none())
        })
}

fn versions(source: Option<&ConfigValue>) -> Vec<(String, String)> {
    let mut map: Vec<(String, String)> = Vec::new();
    for group in items(source.and_then(|source| source.get("groups"))) {
        let source_id = js_string(items(Some(group)).first());
        for document in items(items(Some(group)).get(1)) {
            let key = format!("{source_id}:{}", js_string(document.get("objectId")));
            let pick = |value: Option<&ConfigValue>| {
                value
                    .filter(|value| !matches!(value, ConfigValue::Null))
                    .map(|value| js_string(Some(value)))
            };
            let hash = pick(document.get("sha256"))
                .or_else(|| {
                    pick(
                        document
                            .get("corpus")
                            .and_then(|corpus| corpus.get("originalSha256")),
                    )
                })
                .or_else(|| pick(document.get("version")))
                .unwrap_or_else(|| {
                    let content = pick(document.get("content")).unwrap_or_default();
                    revision(content.as_bytes())
                });
            match map.iter_mut().find(|(name, _)| *name == key) {
                Some(entry) => entry.1 = hash,
                None => map.push((key, hash)),
            }
        }
    }
    map
}

/// `directoryBytes(root)`: the logical size of the files under a folder.
fn directory_bytes(root: &str) -> Result<f64, CliError> {
    let mut bytes = 0_u64;
    let mut folders = vec![root.to_owned()];
    while let Some(folder) = folders.pop() {
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(CliError::from(fs_error("scandir", &folder, &error))),
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path().to_string_lossy().into_owned();
            if kind.is_dir() {
                folders.push(path);
            } else if kind.is_file() {
                match std::fs::symlink_metadata(&path) {
                    Ok(info) => bytes += info.len(),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(CliError::from(fs_error("lstat", &path, &error))),
                }
            }
        }
    }
    Ok(bytes as f64)
}

/// `statusDetails(state, project, build, evaluation, warnings)`
fn status_details(
    connection: &Connection,
    project: &Project,
    build: &ConfigValue,
    evaluation: &ConfigValue,
    warnings: &mut Vec<ConfigValue>,
) -> Result<Vec<(String, ConfigValue)>, CliError> {
    let paths = &project.paths;
    let source_id = js_string(build.get("source_id"));
    let snapshot = read_stage(connection, "source", Some(&source_id))?;
    let report = evaluation
        .get("report")
        .filter(|report| truthy(Some(report)));
    let previous: Option<String> = match report {
        Some(report) => connection
            .query_row(
                "SELECT manifest FROM builds WHERE id = ?1",
                [js_string(report.get("build_id"))],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite)?,
        None => None,
    };
    let assessed = match &previous {
        Some(previous) => {
            let manifest = parse_json(previous)?;
            read_stage(
                connection,
                "source",
                Some(&js_string(manifest.get("source_id"))),
            )?
        }
        None => None,
    };
    if assessed.is_some() && earlier_layout(assessed.as_ref()) && !earlier_layout(snapshot.as_ref())
    {
        warnings.push(object(vec![
            ("code", text("REPORT_BEFORE_LAYOUT_CHANGE")),
            (
                "message",
                text("The latest evaluation report measured a build made before the data layout change. Run eval again to compare new builds."),
            ),
        ]));
    }
    let active = versions(snapshot.as_ref());
    let old = versions(assessed.as_ref());
    let updated = if assessed.is_some() {
        let changed = active
            .iter()
            .filter(|(name, hash)| {
                old.iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value)
                    != Some(hash)
            })
            .count();
        let removed = old
            .iter()
            .filter(|(name, _)| !active.iter().any(|(key, _)| key == name))
            .count();
        ConfigValue::Number((changed + removed) as f64)
    } else {
        ConfigValue::Null
    };
    let has_checks = connection
        .query_row(
            "SELECT name FROM sqlite_master WHERE name = 'source_checks'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(sqlite)?
        .is_some();
    let last: Option<Option<String>> = if has_checks {
        connection
            .query_row(
                "SELECT checked_at FROM source_checks WHERE source_id = ?1 ORDER BY id DESC LIMIT 1",
                [&source_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(sqlite)?
    } else {
        None
    };
    let index = directory_bytes(&node_path::join(
        &paths.artifacts,
        &[&js_string(build.get("index"))],
    ))?;
    let originals = directory_bytes(&node_path::join(&paths.artifacts, &["originals"]))?;
    let corpus = directory_bytes(&paths.corpus)?;
    let metadata = std::fs::symlink_metadata(&paths.state)
        .map_err(|error| CliError::from(fs_error("lstat", &paths.state, &error)))?;
    let previous_config = stored_config(connection, &js_string(build.get("build_id")))?;
    let config = match previous_config {
        Some(config) => parse_json(&config)?,
        None => object(Vec::new()),
    };
    let changed: Vec<ConfigValue> = ["source", "transform", "index", "search"]
        .iter()
        .filter(|section| {
            json_of(project.config.get(section).map(safe_config).as_ref())
                != json_of(config.get(section))
        })
        .map(|section| text(section))
        .collect();
    Ok(vec![
        ("documents_changed_since_evaluation".into(), updated),
        (
            "source_checked_at".into(),
            last.flatten()
                .map_or(ConfigValue::Null, |value| text(&value)),
        ),
        (
            "changed_config_sections".into(),
            ConfigValue::Array(changed),
        ),
        (
            "storage".into(),
            object(vec![
                ("build_index_logical_bytes", ConfigValue::Number(index)),
                (
                    "shared_originals_logical_bytes",
                    ConfigValue::Number(originals),
                ),
                ("shared_corpus_logical_bytes", ConfigValue::Number(corpus)),
                (
                    "shared_state_logical_bytes",
                    ConfigValue::Number(metadata.len() as f64),
                ),
            ]),
        ),
    ])
}

/// `nest status`
fn status_command(state: &mut Run, project: &Project) -> Result<ConfigValue, CliError> {
    let build = current_build(&project.paths, false, None)?;
    let connection = open_state(&project.paths, true)?;
    let search_id = search_config_id(
        stage(&project.plan, "search"),
        &SearchOptions::default(),
        build.as_ref(),
        project,
    )?;
    let evaluation = match &build {
        Some(build) => {
            let mut request = vec![
                ("state".to_owned(), text(&project.paths.state)),
                (
                    "buildId".to_owned(),
                    build.get("build_id").cloned().unwrap_or_default(),
                ),
                ("searchId".to_owned(), text(&search_id)),
            ];
            assign(
                &mut request,
                "dataset",
                project
                    .config
                    .get("eval-data")
                    .and_then(|data| data.get("use"))
                    .cloned(),
            );
            assign(
                &mut request,
                "settings",
                Some(project.eval_settings.clone().unwrap_or_default()),
            );
            let request: crate::eval_report::StatusRequest =
                deserialize(&ConfigValue::Object(request), "report")?;
            Some(crate::eval_report::evaluation_status(&request).map_err(CliError::from)?)
        }
        None => None,
    };
    let usage = crate::trial::usage(&project.paths.state)
        .map_err(|error| CliError::plain(error.message))?;
    let trials = object(vec![
        ("limits", project.trial_limits.clone()),
        ("usage", usage),
    ]);
    let Some(build) = build else {
        return Ok(object(vec![
            ("build_id", ConfigValue::Null),
            ("trials", trials),
        ]));
    };
    let evaluation = evaluation.unwrap_or_default();
    let mut data = vec![
        (
            "build_id".to_owned(),
            build.get("build_id").cloned().unwrap_or_default(),
        ),
        ("search_config_id".to_owned(), text(&search_id)),
    ];
    assign(
        &mut data,
        "evaluation_status",
        evaluation.get("state").cloned(),
    );
    assign(
        &mut data,
        "evaluation_report",
        evaluation.get("report").cloned(),
    );
    assign(&mut data, "documents", build.get("documents").cloned());
    assign(&mut data, "skipped", build.get("skipped").cloned());
    assign(&mut data, "trials", Some(trials));
    for (key, value) in status_details(
        &connection,
        project,
        &build,
        &evaluation,
        &mut state.warnings,
    )? {
        assign(&mut data, &key, Some(value));
    }
    Ok(ConfigValue::Object(data))
}

// ---------------------------------------------------------------------------------------------
// Serving

/// `withServeOverrides(type, settings, values, issue)`: the settings and their issues.
fn serve_overrides(project: &Project, parsed: &Parsed) -> (Option<ConfigValue>, Vec<ConfigValue>) {
    let kind = project.kind.as_str();
    let settings = project.serve_settings.clone().unwrap_or_default();
    let mut issues = Vec::new();
    let file = project.config_path.clone();
    let mut issue =
        |code: ConfigValue, path: &[ConfigValue], expected: ConfigValue, hint: ConfigValue| {
            issues.push(object(vec![
                ("code", code),
                ("file", text(&file)),
                ("line", ConfigValue::Null),
                ("section", text("serve")),
                ("module", ConfigValue::Null),
                ("arg", path.last().cloned().unwrap_or_default()),
                ("received", ConfigValue::Null),
                ("expected", expected),
                ("suggestions", ConfigValue::Array(Vec::new())),
                ("hint", hint),
            ]));
        };
    let transport = parsed.string("transport");
    if transport.is_some_and(|transport| transport != "http" && transport != "stdio") {
        issue(
            text("ARG_RANGE"),
            &[text("serve"), text("transport")],
            text("http or stdio"),
            text("Use --transport http or --transport stdio."),
        );
    }
    if transport == Some("stdio")
        && (parsed.value("host").is_some() || parsed.value("port").is_some())
    {
        issue(
            text("ARG_CONFLICT"),
            &[text("serve"), text("transport")],
            text("stdio without --host or --port"),
            text("Use HTTP to override the host or port."),
        );
    }
    let current = settings.get("transport").cloned().unwrap_or_default();
    let chosen = transport.map_or(current.clone(), text);
    let mut next = if chosen == current {
        match &settings {
            ConfigValue::Object(entries) => entries.clone(),
            _ => Vec::new(),
        }
    } else if chosen.as_str() == Some("stdio") {
        vec![("transport".to_owned(), chosen)]
    } else {
        let mut template = match crate::config_check::serve_template(kind) {
            ConfigValue::Object(entries) => entries,
            _ => Vec::new(),
        };
        assign(&mut template, "transport", Some(chosen));
        template
    };
    if let Some(host) = parsed.string("host") {
        assign(&mut next, "host", Some(text(host)));
    }
    if let Some(port) = parsed.string("port") {
        let number = !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit());
        let value = if number {
            ConfigValue::Number(port.parse::<f64>().unwrap_or(f64::INFINITY))
        } else {
            text(port)
        };
        assign(&mut next, "port", Some(value));
    }
    let (validated, problems) =
        crate::cli_project::validate_serve(kind, Some(&ConfigValue::Object(next)));
    for problem in problems {
        issue(
            problem.get("code").cloned().unwrap_or_default(),
            items(problem.get("path")),
            problem.get("expected").cloned().unwrap_or_default(),
            problem.get("hint").cloned().unwrap_or_default(),
        );
    }
    (validated, issues)
}

/// `serveCredential(settings)`: the token of `auth: token` must be set.
fn serve_credential(settings: &ConfigValue) -> Result<(), CliError> {
    let auth = settings.get("access").and_then(|access| access.get("auth"));
    if auth.and_then(|auth| string_field(auth, "type")) != Some("token") {
        return Ok(());
    }
    let name = js_string(auth.and_then(|auth| auth.get("token_env")));
    let token = std::env::var(&name).ok().unwrap_or_default();
    if token.is_empty() || !token.bytes().all(|byte| (b'!'..=b'~').contains(&byte)) {
        return Err(CliError::new(
            2,
            "AUTH_TOKEN_REQUIRED",
            "Set a nonempty token in the configured environment variable.",
        ));
    }
    Ok(())
}

fn serve_token(settings: &ConfigValue) -> ConfigValue {
    let auth = settings.get("access").and_then(|access| access.get("auth"));
    if auth.and_then(|auth| string_field(auth, "type")) != Some("token") {
        return ConfigValue::Null;
    }
    let name = js_string(auth.and_then(|auth| auth.get("token_env")));
    std::env::var(&name)
        .ok()
        .map_or(ConfigValue::Null, |token| text(&token))
}

/// `serveAddress(host, port)`
pub fn serve_address(host: &str, port: &str) -> String {
    if crate::config_check::ip_version(host) == 6 {
        format!("http://[{host}]:{port}")
    } else {
        format!("http://{host}:{port}")
    }
}

/// The host of a search server: the models of a build, the index tool, and where it listens.
struct SearchServer {
    env: CliEnv,
    project: Project,
    host: String,
    events: Arc<dyn CliEvents>,
}

impl crate::embed::Charge for SearchServer {
    fn charge(&self, _bytes: usize) -> Result<(), String> {
        Ok(())
    }
}

impl crate::search_stage::SearchHost for SearchServer {
    fn index_tool(&self) -> Result<String, String> {
        crate::cli_tools::index_tool_path(&self.env, &self.project.paths)
            .map_err(|error| error.message)
    }
}

impl crate::serve::ServeHost for SearchServer {
    fn search_models(&self, build: &ConfigValue) -> Result<ConfigValue, String> {
        Ok(
            search_models(stage(&self.project.plan, "search"), build, &self.project)
                .unwrap_or_else(|error| {
                    let mut entries = Vec::new();
                    if let Some(code) = &error.code {
                        entries.push(("code", text(code)));
                    }
                    entries.push(("message", text(&error.message)));
                    if let Some(exit) = error.exit {
                        entries.push(("exitCode", ConfigValue::Number(f64::from(exit))));
                    }
                    object(vec![("error", object(entries))])
                }),
        )
    }

    fn listening(&self, address: &ConfigValue) {
        match address.get("port") {
            None | Some(ConfigValue::Null) => print_error(&format!(
                "Knowledge-Nest MCP stdio ({})",
                js_string(address.get("buildId"))
            )),
            Some(port) => print_error(&format!(
                "Knowledge-Nest {}: {}/",
                self.project.kind,
                serve_address(&self.host, &js_string(Some(port)))
            )),
        }
    }

    fn opened(&self, directory: &str) {
        self.events.opened(directory);
    }
}

/// `serveProject(project, settings)`
fn serve_project(state: &Run, project: &Project, settings: ConfigValue) -> Result<(), CliError> {
    let http = string_field(&settings, "transport") == Some("http");
    if http {
        serve_credential(&settings)?;
    }
    let token = if http {
        serve_token(&settings)
    } else {
        ConfigValue::Null
    };
    let responses = items(settings.get("endpoints"))
        .iter()
        .any(|endpoint| endpoint.as_str() == Some("responses"));
    let model = if http && responses {
        answer_model(project, true)?
    } else {
        None
    };
    let paths = object(vec![
        ("data", text(&project.paths.data)),
        ("state", text(&project.paths.state)),
        ("artifacts", text(&project.paths.artifacts)),
        ("corpus", text(&project.paths.corpus)),
    ]);
    let request = object(vec![
        ("projectType", text(&project.kind)),
        ("settings", settings.clone()),
        ("token", token),
        ("paths", paths),
        ("search", module_settings(stage(&project.plan, "search"))),
        ("ask", module_settings(stage(&project.plan, "ask"))),
        ("model", model.unwrap_or_default()),
    ]);
    let request: crate::serve::ServeRequest = deserialize(&request, "serve")?;
    let host: Arc<dyn crate::serve::ServeHost> = Arc::new(SearchServer {
        env: state.env.clone(),
        project: project.clone(),
        host: js_string(settings.get("host")),
        events: state.events.clone(),
    });
    let stdio = request.settings.transport == "stdio";
    let search_only = request.project_type == "rag";
    state.events.serving(true);
    let outcome = crate::serve::Serving::open(request, host)
        .map_err(CliError::from)
        .and_then(|serving| {
            let serving = Arc::new(serving);
            if stdio {
                crate::serve::serve_stdio(serving, search_only, state.cancel)
            } else {
                crate::serve::serve_http(serving, state.cancel)
            }
            .map_err(CliError::plain)
        });
    state.events.serving(false);
    outcome
}

struct WikiServer {
    kind: String,
    host: String,
}

impl crate::wiki_serve::WikiServeHost for WikiServer {
    fn listening(&self, port: u16) {
        print_error(&format!(
            "Knowledge-Nest {}: {}/",
            self.kind,
            serve_address(&self.host, &port.to_string())
        ));
    }
}

/// `serveWiki(project, settings)`
fn serve_wiki(state: &Run, project: &Project, settings: ConfigValue) -> Result<(), CliError> {
    let base = wiki_request(
        state.env,
        project,
        "serve",
        ConfigValue::Array(Vec::new()),
        object(Vec::new()),
        None,
        false,
        false,
    )?;
    let request = object(vec![
        ("base", base),
        ("settings", settings.clone()),
        ("token", serve_token(&settings)),
    ]);
    let request: crate::wiki_serve::WikiServeRequest = deserialize(&request, "Wiki serve")?;
    let host = Arc::new(WikiServer {
        kind: project.kind.clone(),
        host: js_string(settings.get("host")),
    });
    state.events.serving(true);
    let outcome = crate::wiki_serve::serve(request, host, state.cancel).map_err(CliError::plain);
    state.events.serving(false);
    outcome
}

/// `wikiModel(config, name)`: the settings of a chat model, or why it cannot be used.
fn wiki_model(config: &ConfigValue, name: &str) -> ConfigValue {
    let model = config
        .get("models")
        .and_then(|models| models.get(name))
        .cloned()
        .unwrap_or_default();
    let usable = string_field(&model, "kind") == Some("chat")
        && model
            .get("model")
            .and_then(ConfigValue::as_str)
            .is_some_and(|model| !blank(model));
    if !usable {
        return object(vec![("code", text("MODEL_REQUIRED"))]);
    }
    let settings = (|| {
        let resolved = resolve_value(model.get("model"), &format!("models.{name}.model"))?;
        let base = resolve_value(model.get("base_url"), &format!("models.{name}.base_url"))?;
        let key = resolve_api_key(model.get("api_key_env"), &format!("models.{name}"))?;
        generator_settings(
            string_field(&model, "provider").unwrap_or_default(),
            resolved.as_deref(),
            base.as_deref(),
            key,
            model.get("max_input_bytes"),
        )
    })();
    match settings {
        Ok(settings) => object(vec![("settings", settings)]),
        Err(_) => object(vec![("code", text("INVALID_MODEL"))]),
    }
}

/// `wikiRequest(project, command, args, values, runId, { track, print })`
#[allow(clippy::too_many_arguments)]
fn wiki_request(
    env: &CliEnv,
    project: &Project,
    command: &str,
    args: ConfigValue,
    values: ConfigValue,
    run_id: Option<&str>,
    track: bool,
    print_progress: bool,
) -> Result<ConfigValue, CliError> {
    let plan = project.wiki_plan.clone().unwrap_or_default();
    let model_of = |section: &str, index: usize| {
        items(plan.get(section))
            .get(index)
            .and_then(|step| step.get("model"))
            .and_then(ConfigValue::as_str)
            .map(str::to_owned)
    };
    let names: Vec<String> = [
        model_of("ingress", 2),
        model_of("query", 1),
        model_of("content", 0),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut models = Vec::new();
    for name in &names {
        assign(&mut models, name, Some(wiki_model(&project.config, name)));
    }
    let mut request = vec![
        ("state".to_owned(), text(&project.paths.state)),
        (
            "lock".to_owned(),
            text(&node_path::join(&project.paths.data, &["execution.lock"])),
        ),
        ("root".to_owned(), text(&project.root)),
        ("configRoot".to_owned(), text(&project.config_root)),
        ("artifacts".to_owned(), text(&project.paths.artifacts)),
        ("corpus".to_owned(), text(&project.paths.corpus)),
        ("type".to_owned(), text(&project.kind)),
    ];
    assign(&mut request, "about", project.config.get("about").cloned());
    assign(&mut request, "plan", Some(plan.clone()));
    assign(
        &mut request,
        "pages",
        project
            .serve_settings
            .as_ref()
            .and_then(|settings| settings.get("pages"))
            .cloned(),
    );
    assign(&mut request, "models", Some(ConfigValue::Object(models)));
    assign(&mut request, "command", Some(text(command)));
    assign(&mut request, "args", Some(args));
    assign(&mut request, "values", Some(values.clone()));
    assign(&mut request, "runId", run_id.map(text));
    assign(&mut request, "track", Some(ConfigValue::Bool(track)));
    assign(
        &mut request,
        "printProgress",
        Some(ConfigValue::Bool(print_progress)),
    );
    assign(&mut request, "cwd", Some(text(&env.cwd)));
    assign(&mut request, "home", Some(text(&env.home)));
    if command == "serve" || (command == "ingress" && !truthy(values.get("dry-run"))) {
        assign(
            &mut request,
            "load",
            Some(crate::cli_tools::ocr_request(
                env,
                project.config.get("tools"),
            )?),
        );
    }
    Ok(ConfigValue::Object(request))
}
