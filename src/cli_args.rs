//! The command line of the `dunlin` CLI (spec 28.13 step 6c): the commands with their usage, and
//! the options of each, read as Node.js `util.parseArgs` read them in strict mode.

// Only the CLI uses this outside its tests.
#![cfg_attr(test, allow(dead_code))]

use crate::config_value::ConfigValue;

pub const COMMANDS: [(&str, &str); 34] = [
    (
        "init",
        "dunlin init <mcp|rag|wiki|para> <directory> [--source <directory> | --purpose <text>] [--embedding-model <name>] [--embedding-provider <ollama|openai>] [--llm-model <name>] [--llm-provider <ollama|openai|strata>] [--no-embedding] [--no-llm] [--ocr | --no-ocr] [--ocr-tool <oar-ocr|lightonocr>] [--json]",
    ),
    (
        "config",
        "dunlin config list | dunlin config set <embedding|llm> --model <name> [--provider <name>] | dunlin config set ocr [--tool oar-ocr|lightonocr] | dunlin config unset <embedding|llm|ocr> [--json]",
    ),
    (
        "build",
        "dunlin build [--project <directory>] [--from <stage>] [--input <build>] [--until <stage>] [--force] [--skip-unsupported] [--dry-run] [--config <file>] [--tag <name>] [--json]",
    ),
    (
        "source",
        "dunlin source [--project <directory>] [--config <file>] [--dry-run] [--skip-unsupported] [--json]",
    ),
    (
        "transform",
        "dunlin transform [--project <directory>] [--config <file>] [--dry-run] [--json]",
    ),
    (
        "index",
        "dunlin index [--project <directory>] [--config <file>] [--dry-run] [--json]",
    ),
    (
        "search",
        "dunlin search <query> [--project <directory>] [--build <ref>] [--config <file>] [--limit <n>] [--budget-chars <n>] [--source-id <id>] [--json]",
    ),
    (
        "ask",
        "dunlin ask <question> [--project <directory>] [--build <ref>] [--config <file>] [--json]",
    ),
    (
        "serve",
        "dunlin serve [--project <directory>] [--transport <http|stdio>] [--host <address>] [--port <port>] [--print-config [--json]]",
    ),
    ("status", "dunlin status [--project <directory>] [--json]"),
    (
        "validate",
        "dunlin validate [--project <directory>] [--config <file>] [--json]",
    ),
    (
        "defaults",
        "dunlin defaults [--apply <section>] [--project <directory>] [--json]",
    ),
    (
        "modules",
        "dunlin modules <list|describe <name>> [--project <directory>] [--json]",
    ),
    (
        "add",
        "dunlin add <tool> | dunlin add --list [--project <directory>] [--json]",
    ),
    (
        "remove",
        "dunlin remove <tool> [--project <directory>] [--json]",
    ),
    ("use", "dunlin use <build> [--project <directory>] [--json]"),
    (
        "tag",
        "dunlin tag <name> [build] [--move] [--project <directory>] [--json]",
    ),
    (
        "eval-data",
        "dunlin eval-data <add|import|export|list|show|split|generate|review|sync|diff|tag|use> [--project <directory>] [--json]",
    ),
    (
        "eval",
        "dunlin eval [--project <directory>] [--build <ref>] [--config <file>] [--split dev|test] [--tag <name>] [--json]",
    ),
    (
        "report",
        "dunlin report <show|list|compare> [--project <directory>] [--json]",
    ),
    (
        "dictionary",
        "dunlin dictionary suggest [--report <ref>] | dunlin dictionary add <file> --config <candidate> [--project <directory>] [--json]",
    ),
    (
        "evidence-map",
        "dunlin evidence-map <create|list|show|export|import|confirm> [--project <directory>] [--build <ref>] [--json]",
    ),
    (
        "ingress",
        "dunlin ingress <path...> | dunlin ingress --pending [--project <directory>] [--dry-run] [--source-only] [--json]",
    ),
    (
        "query",
        "dunlin query <question> [--page <id>] [--target <text>] [--project <directory>] [--json]",
    ),
    (
        "verify",
        "dunlin verify [--page <id>] [--content] [--all-issues] [--project <directory>] [--json]",
    ),
    (
        "issue",
        "dunlin issue <id> <open|resolved|dismissed> [--project <directory>] [--json]",
    ),
    ("rules", "dunlin rules [--project <directory>] [--json]"),
    (
        "progress",
        "dunlin progress <work-id> [--project <directory>] [--json]",
    ),
    (
        "cite",
        "dunlin cite <page> <number> [--project <directory>] [--json]",
    ),
    (
        "instruct",
        "dunlin instruct <page> <instruction> [--target <text>] [--supplement <text> --by <name>] | dunlin instruct --persistent <rule> [--project <directory>] [--json]",
    ),
    (
        "save-answer",
        "dunlin save-answer <query-run> <page> <title> [--project <directory>] [--json]",
    ),
    (
        "archive",
        "dunlin archive <page> [--project <directory>] [--restore] [--json]",
    ),
    (
        "undo",
        "dunlin undo <work-id> [--project <directory>] [--json]",
    ),
    (
        "store",
        "dunlin store run <config> [--release] | dunlin store eval-data <file> [--tag <name>] | dunlin store optimize <instruction> [--name <name>] | dunlin store results [<name>] [--project <directory>] [--json]",
    ),
];

pub fn usage(command: &str) -> Option<&'static str> {
    COMMANDS
        .iter()
        .find(|(name, _)| *name == command)
        .map(|(_, usage)| *usage)
}

/// The commands of `dunlin --help` in groups, each with what it does.
pub const GROUPS: [(&str, &[(&str, &str)]); 6] = [
    (
        "Start",
        &[
            ("init", "Create a project: mcp, rag, wiki or para"),
            (
                "config",
                "Set the models and OCR tool that new projects use",
            ),
            ("add", "Install an optional tool such as OCR or hannoy"),
            ("remove", "Remove an installed tool from the project"),
        ],
    ),
    (
        "Build and search (mcp, rag)",
        &[
            ("build", "Read the documents and build the search index"),
            ("source", "Run only the scan and extraction stage"),
            ("transform", "Run only the normalization and chunking stage"),
            ("index", "Run only the indexing stage"),
            (
                "search",
                "Search the documents and print the passages found",
            ),
            ("ask", "Answer a question from the documents, with sources"),
            ("serve", "Serve the project over MCP or HTTP"),
            (
                "status",
                "Show the serving build, its evaluation and storage",
            ),
            ("use", "Serve another build"),
            ("tag", "Name a build"),
        ],
    ),
    (
        "Evaluate (mcp, rag)",
        &[
            (
                "eval-data",
                "Manage the questions and quotes used to evaluate",
            ),
            (
                "evidence-map",
                "Locate the quotes of the evaluation data in a build",
            ),
            ("eval", "Evaluate a build with the evaluation data"),
            ("report", "Show and compare evaluation reports"),
            ("dictionary", "Suggest and add user dictionary words"),
        ],
    ),
    (
        "Wiki and PARA",
        &[
            ("ingress", "Read sources and write wiki pages from them"),
            ("query", "Answer a question from the wiki pages"),
            ("verify", "Check pages against their sources"),
            (
                "issue",
                "Open, resolve or dismiss an issue that verify found",
            ),
            ("rules", "Print the writing rules of the wiki"),
            ("progress", "Show the progress of a running job"),
            ("cite", "Show the source of a citation on a page"),
            ("instruct", "Ask for a change to a page, or add a rule"),
            ("save-answer", "Save a query answer as a page"),
            ("archive", "Archive or restore a page"),
            ("undo", "Undo the changes of a job"),
        ],
    ),
    (
        "Configuration",
        &[
            ("validate", "Check Dunlinfile.yaml"),
            (
                "defaults",
                "Compare Dunlinfile.yaml with the current defaults",
            ),
            ("modules", "List the modules and their arguments"),
        ],
    ),
    (
        "Versioned store (in development)",
        &[(
            "store",
            "Run a configuration file or an optimization on the versioned store",
        )],
    ),
];

/// What a command does, as `dunlin --help` lists it.
pub fn description(command: &str) -> Option<&'static str> {
    GROUPS
        .iter()
        .flat_map(|(_, commands)| commands.iter())
        .find(|(name, _)| *name == command)
        .map(|(_, description)| *description)
}

/// The usage of a command broken at its options so that each line stays short.
pub fn wrapped_usage(command: &str) -> Option<String> {
    let usage = usage(command)?;
    let mut parts: Vec<String> = Vec::new();
    let mut depth = 0;
    let mut current = String::new();
    for c in usage.chars() {
        match c {
            '[' | '<' => depth += 1,
            ']' | '>' => depth -= 1,
            _ => {}
        }
        if c == ' ' && depth == 0 && current.chars().count() > 0 {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    parts.push(current);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for part in parts {
        if part == "|" {
            lines.push(std::mem::take(&mut line));
            line.push_str("| ");
            continue;
        }
        if !line.is_empty()
            && !line.ends_with("| ")
            && line.chars().count() + part.chars().count() > 76
        {
            lines.push(std::mem::take(&mut line));
            line.push_str("    ");
        }
        if !line.is_empty() && !line.ends_with(' ') {
            line.push(' ');
        }
        line.push_str(&part);
    }
    lines.push(line);
    Some(
        lines
            .iter()
            .map(|line| format!("  {}", line.trim_start_matches("| ").trim_end()))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// The help of one command: what it does and its usage.
pub fn command_help(command: &str) -> Option<String> {
    Some(format!(
        "dunlin {command} — {}\n\nUsage:\n{}",
        description(command).unwrap_or_default(),
        wrapped_usage(command)?
    ))
}

/// The overall help of `dunlin --help`.
pub fn help_text() -> String {
    let mut out = format!(
        "Dunlin {} — search your documents, or keep a wiki written from them.\n\nUsage: dunlin [--project <directory>] <command> [options]\n",
        env!("CARGO_PKG_VERSION")
    );
    for (group, commands) in GROUPS {
        out.push('\n');
        out.push_str(group);
        out.push('\n');
        for (name, description) in commands {
            out.push_str(&format!("  {name:<14}{description}\n"));
        }
    }
    out.push_str(
        "\nOptions for every command\n  --project <directory>  The project folder (default: the nearest Dunlinfile.yaml)\n  --json                 Print one JSON object with schema_version 1\n  --help                 Show the options of a command\n  --version              Print the version\n\nRun dunlin <command> --help for the options of a command.",
    );
    out
}

/// Edit distance with adjacent transpositions (optimal string alignment).
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut rows = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in rows[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[i - 1][j] + 1)
                .min(rows[i][j - 1] + 1)
                .min(rows[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[a.len()][b.len()]
}

/// The closest of `candidates` to a mistyped `input`, if one is close enough to be meant.
pub fn closest<'a>(input: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let input = input.to_lowercase();
    let limit = match input.chars().count() {
        0..=3 => 1,
        4..=7 => 2,
        _ => 3,
    };
    candidates
        .into_iter()
        .map(|candidate| (distance(&input, candidate), candidate))
        .filter(|(score, _)| *score <= limit)
        .min_by_key(|(score, _)| *score)
        .map(|(_, candidate)| candidate)
}

/// The closest command to a mistyped one.
pub fn closest_command(input: &str) -> Option<&'static str> {
    closest(input, COMMANDS.iter().map(|(name, _)| *name))
}

const STAGES: [&str; 3] = ["source", "transform", "index"];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Boolean,
    String,
    Strings,
}

/// The options of a command: `[name, kind]`, as `runProjectCommand` declared them.
fn options(command: &str) -> Vec<(&'static str, Kind)> {
    use Kind::{Boolean, String, Strings};
    let mut options = vec![("help", Boolean), ("json", Boolean)];
    match command {
        "init" => {
            for key in [
                "source",
                "purpose",
                "embedding-model",
                "embedding-provider",
                "llm-model",
                "llm-provider",
                "ocr-tool",
            ] {
                options.push((key, String));
            }
            for key in ["no-embedding", "no-llm", "ocr", "no-ocr"] {
                options.push((key, Boolean));
            }
        }
        "config" => {
            options.push(("provider", String));
            options.push(("model", String));
            options.push(("tool", String));
        }
        _ => options.push(("project", String)),
    }
    if command == "build" || command == "source" {
        options.push(("skip-unsupported", Boolean));
    }
    if command == "build" || STAGES.contains(&command) {
        options.push(("dry-run", Boolean));
    }
    if command == "build" {
        options.push(("from", String));
        options.push(("until", String));
        options.push(("force", Boolean));
        options.push(("tag", String));
        options.push(("input", String));
    }
    if ["validate", "search", "ask", "build", "eval", "dictionary"].contains(&command)
        || STAGES.contains(&command)
    {
        options.push(("config", String));
    }
    if command == "dictionary" {
        options.push(("report", String));
    }
    if ["search", "ask", "eval", "evidence-map"].contains(&command) {
        options.push(("build", String));
    }
    if command == "search" {
        options.push(("limit", String));
        options.push(("budget-chars", String));
        options.push(("source-id", Strings));
    }
    if command == "evidence-map" {
        for name in ["output", "key", "start", "end", "reviewer"] {
            options.push((name, String));
        }
    }
    if ["eval", "report"].contains(&command) {
        options.push(("split", String));
    }
    if command == "eval" {
        options.push(("tag", String));
    }
    if command == "eval-data" {
        for name in [
            "question",
            "doc",
            "quote",
            "prefix",
            "suffix",
            "test-ratio",
            "seed",
            "output",
            "model",
            "count",
            "kind",
            "state",
            "reviewer",
            "reviewer-type",
            "from-search",
            "hit",
            "page",
            "heading",
        ] {
            options.push((name, String));
        }
        options.push(("case", Strings));
    }
    if command == "store" {
        options.push(("release", Boolean));
        options.push(("tag", String));
        options.push(("name", String));
    }
    if command == "serve" {
        options.push(("print-config", Boolean));
        for name in ["transport", "host", "port"] {
            options.push((name, String));
        }
    }
    if command == "ingress" {
        options.push(("dry-run", Boolean));
        options.push(("source-only", Boolean));
        options.push(("pending", Boolean));
    }
    if ["query", "verify"].contains(&command) {
        options.push(("page", String));
    }
    if command == "query" {
        options.push(("target", String));
    }
    if command == "verify" {
        options.push(("content", Boolean));
        options.push(("all-issues", Boolean));
    }
    if command == "instruct" {
        for option in ["target", "supplement", "by"] {
            options.push((option, String));
        }
        options.push(("persistent", Boolean));
    }
    if command == "archive" {
        options.push(("restore", Boolean));
    }
    if command == "tag" {
        options.push(("move", Boolean));
    }
    if command == "defaults" {
        options.push(("apply", String));
    }
    if command == "add" {
        options.push(("list", Boolean));
    }
    options
}

/// The options and positional arguments of a command line, or the message of its parse error.
pub struct Parsed {
    pub values: Vec<(String, ConfigValue)>,
    pub positionals: Vec<String>,
}

impl Parsed {
    pub fn value(&self, name: &str) -> Option<&ConfigValue> {
        self.values
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    pub fn string(&self, name: &str) -> Option<&str> {
        self.value(name).and_then(ConfigValue::as_str)
    }

    pub fn flag(&self, name: &str) -> bool {
        matches!(self.value(name), Some(ConfigValue::Bool(true)))
    }

    pub fn strings(&self, name: &str) -> Option<Vec<String>> {
        match self.value(name) {
            Some(ConfigValue::Array(items)) => Some(
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect(),
            ),
            _ => None,
        }
    }

    /// The values as a JSON object, in the order the options were given.
    pub fn object(&self) -> ConfigValue {
        ConfigValue::Object(self.values.clone())
    }
}

fn unknown(command: &str, option: &str, declared: &[(&'static str, Kind)]) -> String {
    let name = option.trim_start_matches('-');
    let hint = closest(name, declared.iter().map(|(key, _)| *key))
        .map_or(String::new(), |key| format!(" Did you mean --{key}?"));
    format!(
        "Unknown option {option} for dunlin {command}.{hint} Run dunlin {command} --help for its options; to pass a value that starts with '-', put it after --."
    )
}

/// `parseArgs({ args, options, allowPositionals: true })`
pub fn parse(command: &str, args: &[String]) -> Result<Parsed, String> {
    let declared = options(command);
    let find = |name: &str| {
        declared
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, kind)| *kind)
    };
    let mut values: Vec<(String, ConfigValue)> = Vec::new();
    let mut positionals = Vec::new();
    let store =
        |values: &mut Vec<(String, ConfigValue)>, name: &str, kind: Kind, value: ConfigValue| {
            if kind == Kind::Strings {
                match values.iter_mut().find(|(key, _)| key == name) {
                    Some((_, ConfigValue::Array(items))) => items.push(value),
                    _ => values.push((name.to_owned(), ConfigValue::Array(vec![value]))),
                }
            } else {
                match values.iter_mut().find(|(key, _)| key == name) {
                    Some(entry) => entry.1 = value,
                    None => values.push((name.to_owned(), value)),
                }
            }
        };
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        index += 1;
        if arg == "--" {
            positionals.extend(args[index..].iter().cloned());
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (long, None),
            };
            let Some(kind) = find(name) else {
                return Err(unknown(command, &format!("--{name}"), &declared));
            };
            match (kind, inline) {
                (Kind::Boolean, Some(_)) => {
                    return Err(format!("Option '--{name}' does not take an argument"));
                }
                (Kind::Boolean, None) => store(&mut values, name, kind, ConfigValue::Bool(true)),
                (_, Some(value)) => store(&mut values, name, kind, ConfigValue::String(value)),
                (_, None) => {
                    let Some(value) = args.get(index) else {
                        return Err(format!("Option '--{name} <value>' argument missing"));
                    };
                    if value.starts_with('-') {
                        return Err(format!(
                            "Option '--{name}' argument is ambiguous.\nDid you forget to specify the option argument for '--{name}'?\nTo specify an option argument starting with a dash use '--{name}=-XYZ'."
                        ));
                    }
                    index += 1;
                    store(&mut values, name, kind, ConfigValue::String(value.clone()));
                }
            }
            continue;
        }
        if arg.len() > 1 && arg.starts_with('-') {
            // Short options: only -h.
            for short in arg[1..].chars() {
                if short != 'h' {
                    return Err(unknown(command, &format!("-{short}"), &declared));
                }
                store(&mut values, "help", Kind::Boolean, ConfigValue::Bool(true));
            }
            continue;
        }
        positionals.push(arg.clone());
    }
    Ok(Parsed {
        values,
        positionals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn reads_options_as_parse_args() {
        let parsed = parse(
            "search",
            &args(&[
                "q",
                "--limit=3",
                "--source-id",
                "a",
                "--source-id",
                "b",
                "--json",
            ]),
        )
        .unwrap();
        assert_eq!(parsed.positionals, ["q"]);
        assert_eq!(parsed.string("limit"), Some("3"));
        assert_eq!(parsed.strings("source-id").unwrap(), ["a", "b"]);
        assert!(parsed.flag("json"));
        assert_eq!(
            parse("status", &args(&["--project"])).err().unwrap(),
            "Option '--project <value>' argument missing"
        );
        assert!(
            parse("status", &args(&["-x"]))
                .err()
                .unwrap()
                .starts_with("Unknown option -x for dunlin status.")
        );
        assert_eq!(
            parse("status", &args(&["--", "--json"]))
                .unwrap()
                .positionals,
            ["--json"]
        );
        assert!(
            parse("search", &args(&["q", "--limt", "3"]))
                .err()
                .unwrap()
                .contains("Did you mean --limit?")
        );
    }

    #[test]
    fn suggests_the_command_that_was_meant() {
        assert_eq!(closest_command("serach"), Some("search"));
        assert_eq!(closest_command("stauts"), Some("status"));
        assert_eq!(closest_command("bulid"), Some("build"));
        assert_eq!(closest_command("xyzzy"), None);
        assert_eq!(closest("mpc", ["mcp", "rag", "wiki", "para"]), Some("mcp"));
        assert!(help_text().contains("eval-data"));
        for (name, _) in COMMANDS {
            assert!(description(name).is_some(), "{name}");
        }
        let help = command_help("search").unwrap();
        assert!(
            help.lines().all(|line| line.chars().count() <= 100),
            "{help}"
        );
    }
}
