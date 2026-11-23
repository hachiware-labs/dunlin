//! The module registry: names, roles, argument definitions and descriptions (spec 26.1, 26.5).
//!
//! Argument definitions are the single source for validation, `nest modules describe` and the
//! defaults that `init` writes. The rules follow the validation that this registry replaced, so
//! existing configurations get the same diagnostics.

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};

use crate::config_value::ConfigValue;

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    /// A string with at least one character, optionally trimmed first and optionally null.
    Str {
        trim: bool,
        nullable: bool,
    },
    /// A list of strings with minimum counts for items and for each item's characters.
    StrList {
        min_items: usize,
        item_min: usize,
    },
    Enum(&'static [&'static str]),
    Int {
        min: i64,
        max: i64,
    },
    Num {
        min: f64,
        max: f64,
    },
    Bool,
    /// Environment variable names mapped to strings.
    EnvMap,
}

#[derive(Clone, Copy, Debug)]
pub enum Default {
    Str(&'static str),
    Int(i64),
    Num(f64),
    Bool(bool),
    EmptyList,
    EmptyMap,
}

#[derive(Clone, Copy, Debug)]
pub enum Presence {
    Required,
    Optional,
    Default(Default),
}

#[derive(Clone, Copy, Debug)]
pub struct Arg {
    pub name: &'static str,
    pub kind: Kind,
    pub presence: Presence,
    /// Changes only speed, so it does not count toward reuse decisions.
    pub speed: bool,
}

#[derive(Debug)]
pub struct ModuleInfo {
    pub name: &'static str,
    pub version: u32,
    pub stage: &'static str,
    pub role: &'static str,
    pub input: &'static str,
    pub output: &'static str,
    pub description: &'static str,
    pub deterministic: bool,
    pub external: bool,
    pub requires: &'static [&'static str],
    pub tool: Option<&'static str>,
    pub args: Vec<Arg>,
}

impl ModuleInfo {
    pub fn unit(&self) -> &'static str {
        if matches!(self.role, "parser" | "preprocess" | "chunk") && self.stage != "search" {
            "document"
        } else {
            "collection"
        }
    }
}

const fn arg(name: &'static str, kind: Kind, presence: Presence) -> Arg {
    Arg {
        name,
        kind,
        presence,
        speed: false,
    }
}

const STRING: Kind = Kind::Str {
    trim: false,
    nullable: false,
};
const ARGV: Kind = Kind::StrList {
    min_items: 1,
    item_min: 1,
};
const UNSUPPORTED: Arg = arg(
    "unsupported",
    Kind::Enum(&["error", "skip"]),
    Presence::Default(Default::Str("error")),
);
const MATCH: Arg = arg(
    "match",
    Kind::StrList {
        min_items: 1,
        item_min: 1,
    },
    Presence::Optional,
);

fn int(name: &'static str, min: i64, max: i64, default: i64) -> Arg {
    arg(
        name,
        Kind::Int { min, max },
        Presence::Default(Default::Int(default)),
    )
}

fn choice(name: &'static str, choices: &'static [&'static str], default: &'static str) -> Arg {
    arg(
        name,
        Kind::Enum(choices),
        Presence::Default(Default::Str(default)),
    )
}

fn model(presence: Presence) -> Arg {
    arg("model", STRING, presence)
}

/// Text put before what an embedding model embeds, for models that tell queries from documents
/// that way (`query: ` and `passage: `, for example). Left out, nothing is put before it.
const fn prefix(name: &'static str) -> Arg {
    arg(name, STRING, Presence::Optional)
}

/// How much a retrieval module's ranks count in `fusion.rrf`. Left out, they count once, and the
/// search keeps the ID it had before the argument existed.
const WEIGHT: Arg = arg(
    "weight",
    Kind::Num {
        min: 0.0,
        max: 100.0,
    },
    Presence::Optional,
);

struct Module {
    name: &'static str,
    stage: &'static str,
    role: &'static str,
    input: &'static str,
    output: &'static str,
    description: &'static str,
    args: Vec<Arg>,
    deterministic: bool,
    external: bool,
    requires: &'static [&'static str],
    tool: Option<&'static str>,
}

impl Module {
    fn new(
        name: &'static str,
        (stage, role, input, output): (&'static str, &'static str, &'static str, &'static str),
        args: Vec<Arg>,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            stage,
            role,
            input,
            output,
            description,
            args,
            deterministic: true,
            external: false,
            requires: &[],
            tool: None,
        }
    }

    fn external(mut self, requires: &'static [&'static str]) -> Self {
        self.external = true;
        self.deterministic = false;
        self.requires = requires;
        self
    }

    fn requires(mut self, requires: &'static [&'static str]) -> Self {
        self.requires = requires;
        self
    }

    fn tool(mut self, tool: &'static str) -> Self {
        self.tool = Some(tool);
        self
    }

    fn info(self) -> ModuleInfo {
        ModuleInfo {
            name: self.name,
            version: 1,
            stage: self.stage,
            role: self.role,
            input: self.input,
            output: self.output,
            description: self.description,
            deterministic: self.deterministic,
            external: self.external,
            requires: self.requires,
            tool: self.tool,
            args: self.args,
        }
    }
}

/// All modules in registration order.
pub fn modules() -> &'static [ModuleInfo] {
    static MODULES: std::sync::OnceLock<Vec<ModuleInfo>> = std::sync::OnceLock::new();
    MODULES.get_or_init(build)
}

pub fn module(name: &str) -> Option<&'static ModuleInfo> {
    modules().iter().find(|module| module.name == name)
}

fn build() -> Vec<ModuleInfo> {
    let parser = ("source", "parser", "files", "corpus");
    let mut list = vec![
        Module::new(
            "source.filesystem",
            ("source", "source", "project", "files"),
            vec![
                arg("path", STRING, Presence::Default(Default::Str("./docs"))),
                arg(
                    "exclude",
                    Kind::StrList {
                        min_items: 0,
                        item_min: 0,
                    },
                    Presence::Default(Default::EmptyList),
                ),
            ],
            "Scan a local folder.",
        ),
        Module::new(
            "loader.auto",
            parser,
            vec![UNSUPPORTED, choice("ocr", &["none", "loader.ocr"], "none")],
            "Route files that no other parser claimed by format, and extract the text and source positions.",
        ),
    ];
    for (category, description) in [
        (
            "loader.text",
            "Read UTF-8 text, Markdown and HTML, preserving headings.",
        ),
        (
            "loader.office",
            "Read Word, Excel and PowerPoint documents as structured text.",
        ),
        (
            "loader.pdf",
            "Extract the existing text layer and page positions from PDFs.",
        ),
    ] {
        list.push(Module::new(
            category,
            parser,
            vec![UNSUPPORTED, MATCH],
            description,
        ));
    }
    list.extend([
        Module::new(
            "loader.ocr",
            parser,
            vec![UNSUPPORTED, MATCH],
            "Extract text and original page positions with the installed OCR model.",
        )
        .requires(&["ocr tool"]),
        Module::new(
            "loader.command",
            parser,
            vec![
                arg("match", ARGV, Presence::Required),
                arg("command", ARGV, Presence::Required),
                choice("input", &["file", "stdin"], "file"),
                choice("output", &["markdown", "text", "json"], "markdown"),
                arg("version", STRING, Presence::Optional),
                arg("version_command", ARGV, Presence::Optional),
                arg("env", Kind::EnvMap, Presence::Default(Default::EmptyMap)),
                Arg {
                    speed: true,
                    ..int("timeout_ms", 1000, 3_600_000, 120_000)
                },
                Arg {
                    speed: true,
                    ..int("concurrency", 1, 4, 2)
                },
            ],
            "Extract matching files with an external command that prints Markdown, text or JSON.",
        )
        .external(&["external command"]),
        Module::new(
            "text.normalize",
            ("transform", "preprocess", "corpus", "corpus"),
            vec![choice("unicode", &["nfkc", "none"], "nfkc")],
            "Normalize text while preserving original positions.",
        ),
        Module::new(
            "chunk.length",
            ("transform", "chunk", "corpus", "chunks"),
            vec![
                choice("unit", &["characters"], "characters"),
                int("size", 1, 1_048_576, 800),
                int("overlap", 0, 1_048_575, 100),
            ],
            "Split into overlapping character windows.",
        ),
        Module::new(
            "chunk.structure",
            ("transform", "chunk", "corpus", "chunks"),
            vec![int("max_chars", 1, 1_048_576, 1600)],
            "Split at headings and page boundaries, with a length cap.",
        ),
        Module::new(
            "chunk.semantic",
            ("transform", "chunk", "corpus", "chunks"),
            vec![
                model(Presence::Required),
                int("max_chars", 1, 1_048_576, 1200),
                int("sentence_chars", 1, 16384, 250),
                arg(
                    "similarity",
                    Kind::Num {
                        min: -1.0,
                        max: 1.0,
                    },
                    Presence::Default(Default::Num(0.55)),
                ),
            ],
            "Split text at low-similarity boundaries between adjacent passages.",
        )
        .external(&["embedding model"]),
        Module::new(
            "chunk.context",
            ("transform", "context", "chunks", "chunks"),
            vec![
                arg(
                    "headings",
                    Kind::Bool,
                    Presence::Default(Default::Bool(true)),
                ),
                arg(
                    "document",
                    Kind::Bool,
                    Presence::Default(Default::Bool(true)),
                ),
                model(Presence::Optional),
                int("document_chars", 100, 1_000_000, 8000),
                int("max_chars", 20, 4000, 300),
            ],
            "Give each chunk its place in its document, which the index searches before the chunk's text: the document's name, the headings above the chunk (Markdown headings, or numbered and marked lines of other text), and optionally a short description a chat model writes from the document. Search results keep the chunk's own range and text.",
        ),
        Module::new(
            "index.keyword",
            ("index", "index", "chunks", "index"),
            vec![
                choice("tokenizer", &["lindera", "trigram", "word"], "lindera"),
                arg(
                    "user_dictionary",
                    Kind::Str {
                        trim: true,
                        nullable: true,
                    },
                    Presence::Optional,
                ),
                arg(
                    "short_query",
                    Kind::Enum(&["bigram", "none"]),
                    Presence::Optional,
                ),
            ],
            "Build the BM25 keyword index.",
        ),
        Module::new(
            "index.vector",
            ("index", "index", "chunks", "index"),
            vec![model(Presence::Required), prefix("document_prefix")],
            "Build an exact vector index from the configured chunks.",
        )
        .external(&["embedding model"]),
        Module::new(
            "hannoy.index",
            ("index", "index", "chunks", "index"),
            vec![
                model(Presence::Required),
                int("ef_construction", 32, 10000, 100),
                prefix("document_prefix"),
            ],
            "Build an incremental HNSW index using the installed hannoy tool.",
        )
        .tool("hannoy")
        .external(&["hannoy tool", "embedding model"]),
        Module::new(
            "query.normalize",
            ("search", "preprocess", "query", "query"),
            vec![choice("unicode", &["nfkc", "none"], "nfkc")],
            "Normalize the query.",
        ),
        Module::new(
            "retrieval.keyword",
            ("search", "retrieval", "query", "candidates"),
            vec![int("top_k", 1, 100, 10), WEIGHT],
            "Retrieve ranked keyword matches.",
        )
        .requires(&["index.keyword"]),
        Module::new(
            "retrieval.vector",
            ("search", "retrieval", "query", "candidates"),
            vec![
                model(Presence::Required),
                int("top_k", 1, 100, 10),
                WEIGHT,
                prefix("query_prefix"),
            ],
            "Rank chunks by cosine similarity to the query embedding.",
        )
        .external(&["index.vector", "embedding model"]),
        Module::new(
            "hannoy.search",
            ("search", "retrieval", "query", "candidates"),
            vec![
                model(Presence::Required),
                int("top_k", 1, 100, 10),
                int("ef_search", 1, 100_000, 100),
                WEIGHT,
                prefix("query_prefix"),
            ],
            "Retrieve candidates from the hannoy HNSW index.",
        )
        .tool("hannoy")
        .external(&["hannoy.index", "embedding model"]),
        Module::new(
            "fusion.rrf",
            ("search", "fusion", "candidates", "candidates"),
            vec![int("k", 1, 1000, 60)],
            "Combine ranked candidate lists using reciprocal rank fusion, weighting each list by the weight of its retrieval module.",
        ),
        Module::new(
            "rerank.model",
            ("search", "rerank", "candidates", "candidates"),
            vec![model(Presence::Required), int("candidates", 1, 32, 10)],
            "Reorder the top candidates with a chat model while keeping their original ranges.",
        )
        .external(&["chat model"]),
        Module::new(
            "rerank.cross_encoder",
            ("search", "rerank", "candidates", "candidates"),
            vec![
                arg("url", STRING, Presence::Required),
                model(Presence::Optional),
                choice("format", &["cohere", "tei"], "cohere"),
                arg("api_key_env", STRING, Presence::Optional),
                int("candidates", 1, 100, 20),
                int("max_chars", 1, 100_000, 2000),
            ],
            "Reorder the top candidates with a reranking model behind a rerank endpoint (Cohere or Jina /v1/rerank, or TEI /rerank) while keeping their original ranges.",
        )
        .external(&["rerank endpoint"]),
        Module::new(
            "context.assemble",
            ("search", "context", "candidates", "context"),
            vec![
                int("budget_chars", 1, 1_000_000, 4000),
                choice("retrieval", &["fill_budget", "top_k"], "fill_budget"),
                int("max_candidates", 1, 100, 100),
                arg(
                    "merge_overlaps",
                    Kind::Bool,
                    Presence::Default(Default::Bool(true)),
                ),
                arg(
                    "truncate_last",
                    Kind::Bool,
                    Presence::Default(Default::Bool(true)),
                ),
                arg(
                    "include_headings",
                    Kind::Bool,
                    Presence::Default(Default::Bool(true)),
                ),
                // Left out, the context is what it was before the argument existed.
                arg(
                    "expand_lines",
                    Kind::Int { min: 0, max: 1000 },
                    Presence::Optional,
                ),
                arg(
                    "expand_section_chars",
                    Kind::Int {
                        min: 1,
                        max: 1_000_000,
                    },
                    Presence::Optional,
                ),
            ],
            "Assemble source excerpts within the character budget, optionally widening each to the section it is in when the section has at most expand_section_chars characters, or else by expand_lines lines around it.",
        ),
        Module::new(
            "answer.generate",
            ("ask", "answer", "context", "response"),
            vec![
                model(Presence::Default(Default::Str("answerer"))),
                arg("cite", Kind::Bool, Presence::Default(Default::Bool(true))),
                choice("on_no_evidence", &["decline"], "decline"),
            ],
            "Generate an answer grounded in the supplied context.",
        )
        .external(&["chat model"]),
    ]);
    list.into_iter().map(Module::info).collect()
}

/// The result of reading one argument.
#[derive(Debug, PartialEq)]
pub enum ArgResult {
    /// The value after defaults and trimming. `None` for an absent optional argument.
    Value {
        value: Option<ConfigValue>,
        defaulted: bool,
    },
    /// `ARG_MISSING`, `ARG_TYPE` or `ARG_RANGE`.
    Invalid(&'static str),
}

impl Arg {
    pub fn read(&self, input: Option<&ConfigValue>) -> ArgResult {
        let Some(input) = input else {
            return match self.presence {
                Presence::Required => ArgResult::Invalid("ARG_MISSING"),
                Presence::Optional => ArgResult::Value {
                    value: None,
                    defaulted: false,
                },
                Presence::Default(default) => ArgResult::Value {
                    value: Some(default.value()),
                    defaulted: true,
                },
            };
        };
        match self.kind.read(input) {
            Ok(value) => ArgResult::Value {
                value: Some(value),
                defaulted: false,
            },
            Err(code) => ArgResult::Invalid(code),
        }
    }

    pub fn default_value(&self) -> Option<ConfigValue> {
        match self.presence {
            Presence::Default(default) => Some(default.value()),
            _ => None,
        }
    }
}

impl Default {
    fn value(self) -> ConfigValue {
        match self {
            Self::Str(text) => ConfigValue::String(text.into()),
            Self::Int(value) => ConfigValue::Number(value as f64),
            Self::Num(value) => ConfigValue::Number(value),
            Self::Bool(value) => ConfigValue::Bool(value),
            Self::EmptyList => ConfigValue::Array(Vec::new()),
            Self::EmptyMap => ConfigValue::Object(Vec::new()),
        }
    }
}

const TYPE: &str = "ARG_TYPE";
const RANGE: &str = "ARG_RANGE";

impl Kind {
    fn read(self, input: &ConfigValue) -> Result<ConfigValue, &'static str> {
        match (self, input) {
            (Self::Str { nullable: true, .. }, ConfigValue::Null) => Ok(ConfigValue::Null),
            (Self::Str { trim, .. }, ConfigValue::String(text)) => {
                let text = if trim {
                    text.trim_matches(js_space)
                } else {
                    text
                };
                if text.is_empty() {
                    Err(RANGE)
                } else {
                    Ok(ConfigValue::String(text.into()))
                }
            }
            (
                Self::StrList {
                    min_items,
                    item_min,
                },
                ConfigValue::Array(items),
            ) => {
                for item in items {
                    match item {
                        ConfigValue::String(text) if text.chars().count() < item_min => {
                            return Err(RANGE);
                        }
                        ConfigValue::String(_) => {}
                        _ => return Err(TYPE),
                    }
                }
                if items.len() < min_items {
                    return Err(RANGE);
                }
                Ok(input.clone())
            }
            (Self::Enum(choices), ConfigValue::String(text))
                if choices.contains(&text.as_str()) =>
            {
                Ok(input.clone())
            }
            (Self::Enum(_), _) => Err(RANGE),
            (Self::Int { min, max }, ConfigValue::Number(value)) => {
                if value.fract() != 0.0 {
                    Err(TYPE)
                } else if *value < min as f64 || *value > max as f64 {
                    Err(RANGE)
                } else {
                    Ok(input.clone())
                }
            }
            (Self::Num { min, max }, ConfigValue::Number(value)) => {
                if *value < min || *value > max {
                    Err(RANGE)
                } else {
                    Ok(input.clone())
                }
            }
            (Self::Bool, ConfigValue::Bool(_)) => Ok(input.clone()),
            (Self::EnvMap, ConfigValue::Object(entries)) => {
                for (key, value) in entries {
                    if !environment_name(key) {
                        return Err(RANGE);
                    }
                    if !matches!(value, ConfigValue::String(_)) {
                        return Err(TYPE);
                    }
                }
                Ok(input.clone())
            }
            _ => Err(TYPE),
        }
    }

    fn type_name(self) -> Option<&'static str> {
        match self {
            // A nullable string has no single type name in the descriptions.
            Self::Str { nullable: true, .. } => None,
            Self::Str { .. } => Some("string"),
            Self::StrList { .. } => Some("array"),
            Self::Enum(_) => Some("one of the listed values"),
            Self::Int { .. } => Some("integer"),
            Self::Num { .. } => Some("number"),
            Self::Bool => Some("boolean"),
            Self::EnvMap => Some("mapping"),
        }
    }
}

/// `/^[A-Za-z_][A-Za-z0-9_]*$/`
pub fn environment_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// JavaScript `\s` and `String.prototype.trim`: WhiteSpace and LineTerminator.
pub fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// Describes one argument as `nest modules describe` prints it.
pub struct ArgInfo<'a> {
    pub arg: &'a Arg,
    /// Diagnostics describe every argument as affecting output.
    pub speed: bool,
}

impl Serialize for ArgInfo<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let arg = self.arg;
        let mut map = serializer.serialize_map(None)?;
        if let Some(name) = arg.kind.type_name() {
            map.serialize_entry("type", name)?;
        }
        if let Some(default) = arg.default_value() {
            map.serialize_entry("default", &default)?;
        }
        if let Kind::Enum(choices) = arg.kind {
            map.serialize_entry("choices", choices)?;
        }
        match arg.kind {
            Kind::Int { min, max } => {
                map.serialize_entry("min", &min)?;
                map.serialize_entry("max", &max)?;
            }
            Kind::Num { min, max } => {
                map.serialize_entry("min", &ConfigValue::Number(min))?;
                map.serialize_entry("max", &ConfigValue::Number(max))?;
            }
            _ => {}
        }
        if matches!(arg.presence, Presence::Optional) {
            map.serialize_entry("optional", &true)?;
        }
        map.serialize_entry("effect", if self.speed { "speed" } else { "output" })?;
        map.end()
    }
}

impl Serialize for ModuleInfo {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Args<'a>(&'a [Arg]);
        impl Serialize for Args<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(self.0.len()))?;
                for arg in self.0 {
                    map.serialize_entry(
                        arg.name,
                        &ArgInfo {
                            arg,
                            speed: arg.speed,
                        },
                    )?;
                }
                map.end()
            }
        }
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("name", self.name)?;
        map.serialize_entry("version", &self.version)?;
        map.serialize_entry("stage", self.stage)?;
        map.serialize_entry("role", self.role)?;
        map.serialize_entry("input", self.input)?;
        map.serialize_entry("output", self.output)?;
        map.serialize_entry("description", self.description)?;
        map.serialize_entry("deterministic", &self.deterministic)?;
        map.serialize_entry("external", &self.external)?;
        map.serialize_entry("unit", self.unit())?;
        map.serialize_entry("requires", self.requires)?;
        if let Some(tool) = self.tool {
            map.serialize_entry("tool", tool)?;
        }
        map.serialize_entry("args", &Args(&self.args))?;
        map.end()
    }
}

/// Reads every argument of a module in definition order.
pub fn read_args<'a>(module: &'a ModuleInfo, args: &ConfigValue) -> Vec<(&'a Arg, ArgResult)> {
    module
        .args
        .iter()
        .map(|arg| (arg, arg.read(args.get(arg.name))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> ConfigValue {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn reads_arguments_with_defaults_and_codes() {
        let chunk = module("chunk.length").unwrap();
        let read = read_args(chunk, &value(r#"{"size": 1.5, "overlap": -1}"#));
        assert!(matches!(
            read[0].1,
            ArgResult::Value {
                defaulted: true,
                ..
            }
        ));
        assert_eq!(read[1].1, ArgResult::Invalid("ARG_TYPE"));
        assert_eq!(read[2].1, ArgResult::Invalid("ARG_RANGE"));
        let keyword = module("index.keyword").unwrap();
        let read = read_args(keyword, &value(r#"{"user_dictionary": "  a.csv　"}"#));
        assert_eq!(
            read[1].1,
            ArgResult::Value {
                value: Some(ConfigValue::String("a.csv".into())),
                defaulted: false
            }
        );
        let command = module("loader.command").unwrap();
        let read = read_args(command, &value(r#"{"command": [], "env": {"1A": "x"}}"#));
        assert_eq!(read[0].1, ArgResult::Invalid("ARG_MISSING"));
        assert_eq!(read[1].1, ArgResult::Invalid("ARG_RANGE"));
        assert_eq!(read[6].1, ArgResult::Invalid("ARG_RANGE"));
    }

    #[test]
    fn describes_modules_in_registration_order() {
        let names: Vec<_> = modules().iter().map(|module| module.name).collect();
        assert_eq!(names.len(), 24);
        assert_eq!(names[0], "source.filesystem");
        let text = serde_json::to_string(module("hannoy.index").unwrap()).unwrap();
        assert!(
            text.contains(r#""requires":["hannoy tool","embedding model"],"tool":"hannoy","args""#)
        );
    }
}
