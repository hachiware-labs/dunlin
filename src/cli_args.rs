//! The command line of the `dunlin` CLI (spec 28.13 step 6c): the commands with their usage, and
//! the options of each, read as Node.js `util.parseArgs` read them in strict mode.

// Only the CLI uses this outside its tests.
#![cfg_attr(test, allow(dead_code))]

use crate::config_value::ConfigValue;

pub const COMMANDS: [(&str, &str); 33] = [
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
];

pub fn usage(command: &str) -> Option<&'static str> {
    COMMANDS
        .iter()
        .find(|(name, _)| *name == command)
        .map(|(_, usage)| *usage)
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

fn unknown(option: &str) -> String {
    format!(
        "Unknown option '{option}'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"{option}\""
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
                return Err(unknown(&format!("--{name}")));
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
                    return Err(unknown(&format!("-{short}")));
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
                .starts_with("Unknown option '-x'")
        );
        assert_eq!(
            parse("status", &args(&["--", "--json"]))
                .unwrap()
                .positionals,
            ["--json"]
        );
    }
}
