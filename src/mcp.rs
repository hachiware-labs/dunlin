//! MCP over stdio (spec 28.10 step 3c) with the official Rust SDK.
//!
//! The tools, their input schemas, argument checks and results are the ones the JavaScript server
//! published. Without a host, the tools run on the open index: `knowledge.search` (keyword search),
//! `knowledge.read` and `knowledge.explore`. A project server passes a host
//! instead (`serve::Serving`): `knowledge.search` runs the project search plan and `knowledge.read`
//! reads from the serving build or an earlier one. The HTTP server answers `/mcp` with the same
//! tools through `tools_json` and `call_json`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;

use crate::config_value::ConfigValue;
use crate::{Cancellation, DocumentRef, Evidence, ExploreOptions, KnowledgeBase, ReadOutcome};

/// The project search and read of a project server.
pub trait McpHost: Send + Sync {
    /// Runs `knowledge.search` with `{ query, limit?, sourceIds?, budgetChars? }` and returns its
    /// result as JSON text, or the error message.
    fn search(&self, arguments: &ConfigValue, cancel: &Cancellation) -> Result<String, String>;
    /// Reads evidence and returns `{ content, evidence }` as JSON text, or the error message.
    fn read(&self, evidence: &Evidence, cancel: &Cancellation) -> Result<String, String>;
    /// A document of the index served now, whole: its text and its evidence. `None` when the
    /// host does not read documents, and the server reads them from the index it was given.
    fn document(
        &self,
        _document: &DocumentRef,
        _cancel: &Cancellation,
    ) -> Option<Result<Option<ReadOutcome>, String>> {
        None
    }
}

const SCHEMA: &str = "https://json-schema.org/draft/2020-12/schema";
const SOURCE_ID: &str = r#"{"type":"string","pattern":"^[a-zA-Z0-9_.-]{1,64}$"}"#;

fn source_ids() -> String {
    format!(r#"{{"minItems":1,"maxItems":100,"type":"array","items":{SOURCE_ID}}}"#)
}

fn search_schema(project: bool) -> String {
    if project {
        // `sourceIds` and `budgetChars` are still accepted for earlier clients (`project_aliases`).
        return format!(
            r#"{{"type":"object","$schema":"{SCHEMA}","properties":{{"query":{{"type":"string","description":"Words or a question to search for."}},"limit":{{"type":"integer","minimum":1,"maximum":100,"description":"The most results to return."}},"source_ids":{{"minItems":1,"maxItems":100,"type":"array","items":{SOURCE_ID},"description":"Search only these sources: the source IDs of the project."}},"budget_chars":{{"type":"integer","minimum":1,"maximum":1000000,"description":"The most characters of source context to return."}},"format":{{"type":"string","enum":["text","json"],"description":"text (the default): the passages found as refs, and the text of the best ones. json: the full result, with scores and evidence."}}}},"required":["query"]}}"#
        );
    }
    format!(
        r#"{{"type":"object","$schema":"{SCHEMA}","properties":{{"query":{{"type":"string"}},"limit":{{"type":"integer","minimum":1,"maximum":100}},"sourceIds":{}}},"required":["query"]}}"#,
        source_ids()
    )
}

/// The earlier names of the project search options, with their current names.
const ALIASES: [(&str, &str); 2] = [("sourceIds", "source_ids"), ("budgetChars", "budget_chars")];

/// The arguments of a project search with the earlier camelCase names renamed, or the message for
/// an option given in both forms.
fn project_aliases(
    input: Option<&serde_json::Map<String, Value>>,
) -> Result<Option<serde_json::Map<String, Value>>, String> {
    let Some(input) = input else {
        return Ok(None);
    };
    let mut renamed = input.clone();
    for (old, new) in ALIASES {
        if let Some(value) = renamed.remove(old) {
            if renamed.contains_key(new) {
                return Err("Specify each search option once.".into());
            }
            renamed.insert(new.to_owned(), value);
        }
    }
    Ok(Some(renamed))
}

const SAFE: &str = "9007199254740991";

/// The `evidence` property of a read: the evidence of a search result.
fn evidence_property() -> String {
    format!(
        r#""evidence":{{"type":"object","properties":{{"source":{{"type":"object","properties":{{"sourceId":{{"type":"string"}},"objectId":{{"type":"string"}},"url":{{"type":"string"}},"version":{{"type":"string"}},"modifiedAtMs":{{"anyOf":[{{"type":"integer","minimum":-{SAFE},"maximum":{SAFE}}},{{"type":"null"}}]}}}},"required":["sourceId","objectId","url","version","modifiedAtMs"]}},"range":{{"type":"object","properties":{{"startLine":{{"type":"integer","exclusiveMinimum":0,"maximum":{SAFE}}},"endLine":{{"type":"integer","exclusiveMinimum":0,"maximum":{SAFE}}}}},"required":["startLine","endLine"]}},"acquiredAtMs":{{"type":"integer","minimum":0,"maximum":{SAFE}}},"byteRange":{{"type":"object","properties":{{"start":{{"type":"integer","minimum":0,"maximum":{SAFE}}},"end":{{"type":"integer","exclusiveMinimum":0,"maximum":{SAFE}}}}},"required":["start","end"]}}}},"required":["source","range","acquiredAtMs"]}}"#
    )
}

fn read_schema() -> String {
    format!(
        r#"{{"type":"object","$schema":"{SCHEMA}","properties":{{{}}},"required":["evidence"]}}"#,
        evidence_property()
    )
}

/// The read of a project: by a ref of `knowledge.search`, or by its evidence as before.
fn project_read_schema() -> String {
    format!(
        r#"{{"type":"object","$schema":"{SCHEMA}","properties":{{"ref":{{"type":"string","minLength":1,"maxLength":1100,"description":"A ref from knowledge.search, such as docs:guide.md#L20-45; or any lines of a document, such as docs:guide.md#L1-200; or docs:guide.md for all of it."}},"around":{{"type":"integer","minimum":0,"maximum":1000,"description":"Lines to read before and after the ref too."}},"max_chars":{{"type":"integer","minimum":100,"maximum":200000,"description":"The most characters to return (default 20000)."}},{}}}}}"#,
        evidence_property()
    )
}

fn explore_schema() -> String {
    format!(
        r#"{{"type":"object","$schema":"{SCHEMA}","properties":{{"query":{{"type":"string"}},"sourceIds":{},"seedLimit":{{"type":"integer","minimum":1,"maximum":100}},"maxSteps":{{"type":"integer","minimum":1,"maximum":100}},"timeoutMs":{{"type":"integer","minimum":1,"maximum":60000}}}},"required":["query"]}}"#,
        source_ids()
    )
}

fn tool(name: &'static str, description: impl Into<String>, schema: &str) -> Tool {
    let description: String = description.into();
    let Ok(Value::Object(schema)) = serde_json::from_str(schema) else {
        unreachable!("tool schemas are valid JSON objects")
    };
    let mut tool = Tool::new(name, description, Arc::new(schema));
    tool.annotations = Some(ToolAnnotations::new().read_only(true));
    tool
}

/// `zod`'s name of a JSON value's type.
fn received(value: Option<&Value>) -> &'static str {
    match value {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

/// The type `zod` names for a schema: `integer` is a number to it.
fn expected(schema: &Value) -> &'static str {
    if let Some(Value::Array(options)) = schema.get("anyOf") {
        return options.first().map_or("unknown", expected);
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => "string",
        Some("integer") | Some("number") => "number",
        Some("array") => "array",
        Some("object") => "object",
        Some("null") => "null",
        _ => "unknown",
    }
}

fn number_text(value: &Value) -> String {
    match value.as_f64() {
        Some(number) => ConfigValue::Number(number).to_js_json(),
        None => value.to_string(),
    }
}

/// Checks a value against the schema subset the tools use and reports issues as `zod` words them.
fn check(schema: &Value, value: Option<&Value>, path: &str, issues: &mut Vec<String>) {
    let mut issue = |message: String| {
        issues.push(if path.is_empty() {
            message
        } else {
            format!("{path}: {message}")
        });
    };
    if let Some(Value::Array(options)) = schema.get("anyOf") {
        // `integer | null`
        if matches!(value, Some(Value::Null)) {
            return;
        }
        if let Some(first) = options.first() {
            check(first, value, path, issues);
        }
        return;
    }
    let kind = schema
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mismatched = !matches!(
        (kind, value),
        ("string", Some(Value::String(_)))
            | ("integer", Some(Value::Number(_)))
            | ("array", Some(Value::Array(_)))
            | ("object", Some(Value::Object(_)))
    );
    if mismatched {
        issue(format!(
            "Invalid input: expected {}, received {}",
            expected(schema),
            received(value)
        ));
        return;
    }
    let Some(value) = value else { return };
    match kind {
        "integer" => {
            let number = value.as_f64().unwrap_or(f64::NAN);
            if number.fract() != 0.0 || number.abs() > 9_007_199_254_740_991.0 {
                issue("Invalid input: expected int, received number".into());
                return;
            }
            if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64)
                && number < minimum
            {
                issue(format!(
                    "Too small: expected number to be >={}",
                    number_text(&schema["minimum"])
                ));
            } else if let Some(minimum) = schema.get("exclusiveMinimum").and_then(Value::as_f64)
                && number <= minimum
            {
                issue(format!(
                    "Too small: expected number to be >{}",
                    number_text(&schema["exclusiveMinimum"])
                ));
            } else if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64)
                && number > maximum
            {
                issue(format!(
                    "Too big: expected number to be <={}",
                    number_text(&schema["maximum"])
                ));
            }
        }
        "string" => {
            let text = value.as_str().unwrap_or_default();
            let length = text.encode_utf16().count() as u64;
            if let Some(minimum) = schema.get("minLength").and_then(Value::as_u64)
                && length < minimum
            {
                issue(format!(
                    "Too small: expected string to have >={minimum} characters"
                ));
            }
            if let Some(maximum) = schema.get("maxLength").and_then(Value::as_u64)
                && length > maximum
            {
                issue(format!(
                    "Too big: expected string to have <={maximum} characters"
                ));
            }
            if let Some(Value::Array(options)) = schema.get("enum")
                && !options.iter().any(|option| option.as_str() == Some(text))
            {
                let options: Vec<String> = options.iter().map(Value::to_string).collect();
                issue(format!(
                    "Invalid option: expected one of {}",
                    options.join("|")
                ));
            }
            if schema.get("pattern").is_some() && !valid_source_id(text) {
                issue(format!(
                    "Invalid string: must match pattern /{}/u",
                    schema["pattern"].as_str().unwrap_or_default()
                ));
            }
        }
        "array" => {
            let items = value.as_array().map(Vec::as_slice).unwrap_or_default();
            if let Some(minimum) = schema.get("minItems").and_then(Value::as_u64)
                && (items.len() as u64) < minimum
            {
                issue(format!(
                    "Too small: expected array to have >={minimum} items"
                ));
            }
            if let Some(maximum) = schema.get("maxItems").and_then(Value::as_u64)
                && items.len() as u64 > maximum
            {
                issue(format!("Too big: expected array to have <={maximum} items"));
            }
            if let Some(item) = schema.get("items") {
                for (i, entry) in items.iter().enumerate() {
                    check(item, Some(entry), &join(path, &i.to_string()), issues);
                }
            }
        }
        "object" => {
            let required: Vec<&str> = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|names| names.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if let Some(Value::Object(properties)) = schema.get("properties") {
                for (name, property) in ordered(properties, schema) {
                    let field = value.get(name);
                    if field.is_none() && !required.contains(&name) {
                        continue;
                    }
                    check(property, field, &join(path, name), issues);
                }
            }
        }
        _ => {}
    }
}

/// The properties of a schema in the order the JSON text lists them.
fn ordered<'a>(
    properties: &'a serde_json::Map<String, Value>,
    schema: &Value,
) -> Vec<(&'a str, &'a Value)> {
    let order = schema
        .get("$order")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut listed: Vec<(&str, &Value)> = properties
        .iter()
        .map(|(name, value)| (name.as_str(), value))
        .collect();
    listed.sort_by_key(|(name, _)| order.iter().position(|item| item == name));
    listed
}

/// Adds `$order` to every object schema: the order of its properties in `text`, which
/// `serde_json::Map` would otherwise sort.
fn with_order(text: &str) -> Value {
    fn walk(schema: &mut Value, parsed: &ConfigValue) {
        let (Value::Object(object), ConfigValue::Object(entries)) = (schema, parsed) else {
            return;
        };
        if let Some(ConfigValue::Object(properties)) = entries
            .iter()
            .find(|(name, _)| name == "properties")
            .map(|(_, value)| value)
        {
            object.insert(
                "$order".into(),
                Value::Array(
                    properties
                        .iter()
                        .map(|(name, _)| Value::String(name.clone()))
                        .collect(),
                ),
            );
            if let Some(Value::Object(children)) = object.get_mut("properties") {
                for (name, child) in properties {
                    if let Some(target) = children.get_mut(name) {
                        walk(target, child);
                    }
                }
            }
        }
        if let (Some(target), Some((_, child))) = (
            object.get_mut("items"),
            entries.iter().find(|(key, _)| key == "items"),
        ) {
            walk(target, child);
        }
    }
    let mut schema: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let parsed: ConfigValue = serde_json::from_str(text).unwrap_or(ConfigValue::Null);
    walk(&mut schema, &parsed);
    schema
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else {
        format!("{path}.{name}")
    }
}

pub(crate) fn valid_source_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

/// The schema-checked arguments with unknown keys removed, or the `zod` issues.
fn arguments(
    name: &str,
    schema: &str,
    input: Option<&serde_json::Map<String, Value>>,
) -> Result<Value, String> {
    let schema = with_order(schema);
    let input = Value::Object(input.cloned().unwrap_or_default());
    let mut issues = Vec::new();
    check(&schema, Some(&input), "", &mut issues);
    if !issues.is_empty() {
        return Err(format!(
            "Input validation error: Invalid arguments for tool {name}: {}",
            issues.join(", ")
        ));
    }
    let mut kept = serde_json::Map::new();
    if let (Some(Value::Object(properties)), Value::Object(input)) =
        (schema.get("properties"), &input)
    {
        for (key, value) in input {
            if properties.contains_key(key) {
                kept.insert(key.clone(), value.clone());
            }
        }
    }
    Ok(Value::Object(kept))
}

/// The JSON text of a result as `JSON.stringify` writes it, from a value with sorted keys.
fn js_text(value: &impl serde::Serialize) -> Result<String, String> {
    // Through `Value`, whose maps sort their keys, as the native API's results did.
    let value = serde_json::to_value(value).map_err(|error| error.to_string())?;
    let text = serde_json::to_string(&value).map_err(|error| error.to_string())?;
    let parsed: ConfigValue = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    Ok(parsed.to_js_json())
}

/// `{ content: [{ type: "text", text }], structuredContent }`, the structured content only when the
/// text is JSON: the text a project search and read write for a model has none.
fn result(text: String) -> CallToolResult {
    let structured = serde_json::from_str::<Value>(&text).ok();
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = structured;
    result.is_error = None;
    result
}

fn failure(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// The range checks of the JavaScript API's `evidenceInput`, which the schema does not cover.
fn evidence_limits(evidence: &Value) -> Result<(), String> {
    let limit = 1_048_576.0;
    let integer = |value: Option<&Value>, low: f64, high: f64, name: &str| match value
        .and_then(Value::as_f64)
    {
        Some(number) if number.fract() == 0.0 && number >= low && number <= high => Ok(number),
        _ => Err(format!(
            "{name} must be an integer between {} and {}",
            ConfigValue::Number(low).to_js_json(),
            ConfigValue::Number(high).to_js_json()
        )),
    };
    let range = &evidence["range"];
    let start = integer(range.get("startLine"), 1.0, limit, "startLine")?;
    integer(range.get("endLine"), start, limit, "endLine")?;
    if let Some(bytes) = evidence.get("byteRange") {
        let start = integer(bytes.get("start"), 0.0, limit - 1.0, "byteRange.start")?;
        integer(bytes.get("end"), start + 1.0, limit, "byteRange.end")?;
    }
    Ok(())
}

pub struct McpServer {
    database: Arc<KnowledgeBase>,
    host: Option<Arc<dyn McpHost>>,
    search_only: bool,
    /// What the documents of a project are, for the descriptions of its tools.
    about: Option<String>,
}

/// The most characters a read by ref returns when `max_chars` is not given.
const READ_CHARS: usize = 20_000;

impl McpServer {
    pub fn new(
        database: Arc<KnowledgeBase>,
        host: Option<Arc<dyn McpHost>>,
        search_only: bool,
    ) -> Self {
        Self {
            database,
            host,
            search_only,
            about: None,
        }
    }

    /// Says in the descriptions of the tools what the documents are, such as how many there are
    /// and in which sources.
    #[must_use]
    pub fn with_about(mut self, about: Option<String>) -> Self {
        self.about = about;
        self
    }

    fn tools(&self) -> Vec<(Tool, String)> {
        let project = self.host.is_some();
        let about = self
            .about
            .as_deref()
            .map_or(String::new(), |about| format!(" {about}"));
        let search = if project {
            format!(
                "Search the documents of this project and get the passages that answer, each with the document and lines it is in.{about} Returns a numbered list of passages as refs (document#Lstart-end) with their headings, then the text of the best ones. Rephrase and search again when the passages miss; read more of a passage with knowledge.read and its ref."
            )
        } else {
            "Find indexed source ranges using keyword search.".to_owned()
        };
        let mut tools = vec![(
            tool("knowledge.search", search, &search_schema(project)),
            search_schema(project),
        )];
        if self.search_only {
            return tools;
        }
        let (read, schema) = if project {
            (
                format!(
                    "Read lines of a document of this project, numbered: a ref from knowledge.search, with lines around it if asked, or any lines of a document by its ref.{about}"
                ),
                project_read_schema(),
            )
        } else {
            (
                "Read the indexed lines identified by evidence from knowledge.search.".to_owned(),
                read_schema(),
            )
        };
        tools.push((tool("knowledge.read", read, &schema), schema));
        if project {
            return tools;
        }
        tools.push((
            tool(
                "knowledge.explore",
                "Collect evidence by searching indexed sources and following their Markdown links.",
                &explore_schema(),
            ),
            explore_schema(),
        ));
        tools
    }

    /// The arguments of a call checked against the tool's schema, with the earlier names of the
    /// project search options renamed first.
    fn checked(
        &self,
        name: &str,
        schema: &str,
        input: Option<&serde_json::Map<String, Value>>,
    ) -> Result<Value, String> {
        if self.host.is_some() && name == "knowledge.search" {
            return arguments(name, schema, project_aliases(input)?.as_ref()).map_err(|message| {
                // Name each option as the caller wrote it.
                let mut message = message;
                for (old, new) in ALIASES {
                    if input.is_some_and(|input| input.contains_key(old)) {
                        for end in [':', '.'] {
                            message =
                                message.replace(&format!("{new}{end}"), &format!("{old}{end}"));
                        }
                    }
                }
                message
            });
        }
        arguments(name, schema, input)
    }

    /// Runs a checked tool call. Blocking: the caller runs it off the async runtime.
    fn run(&self, name: &str, arguments: &Value, cancel: &Cancellation) -> Result<String, String> {
        let native = |error: crate::Error| error.to_string();
        let string = |name: &str| {
            arguments
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        let number = |name: &str| arguments.get(name).and_then(Value::as_u64);
        let ids = |name: &str| -> Option<Vec<String>> {
            arguments.get(name).and_then(Value::as_array).map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
        };
        match (name, &self.host) {
            ("knowledge.search", Some(host)) => {
                let twice =
                    |a: &str, b: &str| arguments.get(a).is_some() && arguments.get(b).is_some();
                if twice("budgetChars", "budget_chars") || twice("sourceIds", "source_ids") {
                    return Err("Specify each search option once.".into());
                }
                let mut options = vec![(
                    "query".to_owned(),
                    ConfigValue::String(string("query").into()),
                )];
                let mut put = |key: &str, value: Option<&Value>| {
                    if let Some(value) =
                        value.and_then(|value| serde_json::from_str(&value.to_string()).ok())
                    {
                        options.push((key.to_owned(), value));
                    }
                };
                put("limit", arguments.get("limit"));
                put(
                    "sourceIds",
                    arguments
                        .get("sourceIds")
                        .or_else(|| arguments.get("source_ids")),
                );
                put(
                    "budgetChars",
                    arguments
                        .get("budgetChars")
                        .or_else(|| arguments.get("budget_chars")),
                );
                let found = host.search(&ConfigValue::Object(options), cancel)?;
                if string("format") == "json" {
                    return Ok(found);
                }
                passages(&found)
            }
            ("knowledge.read", Some(host)) if arguments.get("ref").is_some() => {
                if arguments.get("evidence").is_some() {
                    return Err("Give a ref or evidence, not both.".into());
                }
                let (document, lines) = parse_ref(string("ref"))?;
                let read = match host.document(&document, cancel) {
                    Some(read) => read?,
                    None => self
                        .database
                        .get_document(&document, cancel)
                        .map_err(native)?,
                };
                let Some(read) = read else {
                    return Err(format!(
                        "There is no document {}:{} in the index served. Use the document of a ref from knowledge.search.",
                        document.source_id, document.object_id
                    ));
                };
                let around = number("around").unwrap_or(0) as usize;
                let budget = number("max_chars").map_or(READ_CHARS, |chars| chars as usize);
                read_lines(&document, &read.content, lines, around, budget)
            }
            ("knowledge.read", Some(host)) => {
                if arguments.get("evidence").is_none() {
                    return Err("Give the ref of a passage from knowledge.search, such as docs:guide.md#L20-45.".into());
                }
                evidence_limits(&arguments["evidence"])?;
                let evidence: Evidence = serde_json::from_value(arguments["evidence"].clone())
                    .map_err(|_| "Expected source evidence from search".to_owned())?;
                host.read(&evidence, cancel)
            }
            ("knowledge.search", None) => {
                let query = string("query");
                if query.len() > 4096 {
                    return Err("Expected a query of at most 4096 UTF-8 bytes".into());
                }
                let outcome = self
                    .database
                    .search_with_sources(
                        query,
                        number("limit").map(|limit| limit as usize),
                        None,
                        ids("sourceIds").as_deref().unwrap_or_default(),
                        cancel,
                    )
                    .map_err(native)?;
                js_text(&outcome)
            }
            ("knowledge.read", None) => {
                evidence_limits(&arguments["evidence"])?;
                let evidence: Evidence = serde_json::from_value(arguments["evidence"].clone())
                    .map_err(|_| "Expected source evidence from search".to_owned())?;
                js_text(&self.database.read(&evidence, cancel).map_err(native)?)
            }
            ("knowledge.explore", None) => {
                let query = string("query");
                if query.len() > 4096 {
                    return Err("Expected a query of at most 4096 UTF-8 bytes".into());
                }
                let timeout_ms = number("timeoutMs").unwrap_or(1000) as u32;
                let options = ExploreOptions {
                    max_steps: number("maxSteps").unwrap_or(12) as usize,
                    timeout_ms,
                    seed_limit: number("seedLimit").map(|limit| limit as usize),
                    source_ids: ids("sourceIds").unwrap_or_default(),
                };
                let cancel = cancel.clone().with_deadline(Some(
                    Instant::now() + Duration::from_millis(timeout_ms.into()),
                ));
                js_text(
                    &self
                        .database
                        .explore(query, options, &cancel)
                        .map_err(native)?,
                )
            }
            _ => Err(format!("Tool {name} not found")),
        }
    }
}

/// The ref of a search result: its document and lines, `docs:guide.md#L20-45`.
fn passage_ref(result: &Value) -> String {
    let document = result["document_id"].as_str().unwrap_or_default();
    let range = &result["evidence"]["range"];
    match (range["startLine"].as_u64(), range["endLine"].as_u64()) {
        (Some(start), Some(end)) if start == end => format!("{document}#L{start}"),
        (Some(start), Some(end)) => format!("{document}#L{start}-{end}"),
        _ => document.to_owned(),
    }
}

/// A project search written for a model: the passages found as refs with their headings, then
/// the text of the best ones. The scores, versions and evidence stay in the JSON of `format: json`.
fn passages(text: &str) -> Result<String, String> {
    let found: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    // A search a Node.js caller passed to `serveMcp` may answer in another form: kept as it is.
    if !found["results"].is_array() || !found["context"].is_object() {
        return Ok(text.to_owned());
    }
    let query = found["query"].as_str().unwrap_or_default();
    let results = found["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut out = if results.is_empty() {
        format!(
            "No passages found for \"{query}\". Search again with other words: a synonym, or a word the documents would use.\n"
        )
    } else {
        format!(
            "{} passages for \"{query}\". Read more of one with knowledge.read and its ref.\n",
            results.len()
        )
    };
    for (number, result) in results.iter().enumerate() {
        let rank = result["rank"].as_u64().unwrap_or(number as u64 + 1);
        out.push_str(&format!("{rank}. {}", passage_ref(result)));
        if let Some(heading) = result["heading"]
            .as_str()
            .filter(|heading| !heading.is_empty())
        {
            out.push_str(&format!(" — {heading}"));
        }
        out.push('\n');
    }
    for warning in found["warnings"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if let Some(message) = warning["message"].as_str() {
            out.push_str(&format!("Note: {message}\n"));
        }
    }
    let context = found["context"]["text"].as_str().unwrap_or_default();
    if !context.trim().is_empty() {
        out.push_str("\nText of the best passages:\n\n");
        out.push_str(&context.replace("\r\n", "\n"));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

/// The document and the lines of a ref: `docs:guide.md#L20-45`, `docs:guide.md#L20`, or the
/// document alone.
fn parse_ref(text: &str) -> Result<(DocumentRef, Option<(usize, usize)>), String> {
    let wrong = || {
        format!(
            "{text} is not a ref. A ref is a document and its lines, such as docs:guide.md#L20-45, as knowledge.search gives them."
        )
    };
    let (document, lines) = match text.rfind("#L") {
        Some(at) => (&text[..at], Some(&text[at + 2..])),
        None => (text, None),
    };
    let lines = match lines {
        None => None,
        Some(lines) => {
            let number = |text: &str| text.trim().parse::<usize>().ok().filter(|n| *n > 0);
            let (start, end) = match lines.split_once('-') {
                Some((start, end)) => (number(start), number(end.trim_start_matches('L'))),
                None => (number(lines), number(lines)),
            };
            match (start, end) {
                (Some(start), Some(end)) if start <= end => Some((start, end)),
                _ => return Err(wrong()),
            }
        }
    };
    let (source_id, object_id) = document.split_once(':').ok_or_else(wrong)?;
    if !valid_source_id(source_id) || object_id.is_empty() {
        return Err(wrong());
    }
    Ok((
        DocumentRef {
            source_id: source_id.to_owned(),
            object_id: object_id.to_owned(),
        },
        lines,
    ))
}

/// Lines of a document, numbered, with `around` lines before and after, within `budget`
/// characters; the ref to read on with when the budget stops them.
fn read_lines(
    document: &DocumentRef,
    content: &str,
    lines: Option<(usize, usize)>,
    around: usize,
    budget: usize,
) -> Result<String, String> {
    let name = format!("{}:{}", document.source_id, document.object_id);
    let mut all: Vec<&str> = content
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    // The newline at the end ends the last line; it starts none.
    if content.ends_with('\n') {
        all.pop();
    }
    let total = all.len();
    if total == 0 {
        return Ok(format!("{name} has no text.\n"));
    }
    let (start, end) = lines.unwrap_or((1, total));
    if start > total {
        return Err(format!(
            "{name} has {total} lines. Ask for lines up to L{total}."
        ));
    }
    let first = start.saturating_sub(around).max(1);
    let last = end.saturating_add(around).min(total);
    let width = last.to_string().len();
    let mut body = String::new();
    let mut chars = 0;
    let mut shown = first - 1;
    for (number, line) in all.iter().enumerate().take(last).skip(first - 1) {
        let numbered = format!("{:>width$}| {line}\n", number + 1);
        let length = numbered.chars().count();
        if chars + length > budget && shown >= first {
            break;
        }
        chars += length;
        body.push_str(&numbered);
        shown = number + 1;
    }
    let mut out = format!("{name}#L{first}-{shown} ({total} lines in the document)\n{body}");
    if shown < last {
        out.push_str(&format!(
            "… stopped at L{shown} to stay within {budget} characters. Read on with {name}#L{}-{last}.\n",
            shown + 1
        ));
    }
    Ok(out)
}

fn string_json(text: &str) -> String {
    ConfigValue::String(text.to_owned()).to_js_json()
}

fn error_json(message: &str) -> String {
    format!(
        r#"{{"content":[{{"type":"text","text":{}}}],"isError":true}}"#,
        string_json(message)
    )
}

impl McpServer {
    /// The `tools/list` result as the JavaScript server wrote it, keys in its order.
    pub fn tools_json(&self) -> String {
        let tools: Vec<String> = self
            .tools()
            .iter()
            .map(|(tool, schema)| {
                format!(
                    r#"{{"name":{},"description":{},"inputSchema":{schema},"annotations":{{"readOnlyHint":true}}}}"#,
                    string_json(&tool.name),
                    string_json(tool.description.as_deref().unwrap_or_default())
                )
            })
            .collect();
        format!(r#"{{"tools":[{}]}}"#, tools.join(","))
    }

    /// The `tools/call` result as JSON text, or the message of the JSON-RPC error for an unknown
    /// tool. Blocking: the caller runs it off the async runtime.
    pub fn call_json(
        &self,
        name: &str,
        input: Option<&serde_json::Map<String, Value>>,
        cancel: &Cancellation,
    ) -> Result<String, String> {
        let Some((_, schema)) = self.tools().into_iter().find(|(tool, _)| tool.name == name) else {
            return Err(format!("Tool {name} not found"));
        };
        let checked = match self.checked(name, &schema, input) {
            Ok(checked) => checked,
            Err(message) => return Ok(error_json(&message)),
        };
        Ok(match self.run(name, &checked, cancel) {
            Ok(text) if serde_json::from_str::<Value>(&text).is_ok() => format!(
                r#"{{"content":[{{"type":"text","text":{}}}],"structuredContent":{text}}}"#,
                string_json(&text)
            ),
            Ok(text) => format!(
                r#"{{"content":[{{"type":"text","text":{}}}]}}"#,
                string_json(&text)
            ),
            Err(message) => error_json(&message),
        })
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new(
            "knowledge-nest",
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            self.tools().into_iter().map(|(tool, _)| tool).collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.to_string();
        let Some((_, schema)) = self.tools().into_iter().find(|(tool, _)| tool.name == name) else {
            return Err(ErrorData::invalid_params(
                format!("Tool {name} not found"),
                None,
            ));
        };
        let arguments = match self.checked(&name, &schema, request.arguments.as_ref()) {
            Ok(arguments) => arguments,
            Err(message) => return Ok(failure(message).into()),
        };
        let cancel = Cancellation::default();
        // A cancelled request stops the native work it started.
        let watched = cancel.clone();
        let token = context.ct.clone();
        let watcher = tokio::spawn(async move {
            token.cancelled().await;
            watched.abort();
        });
        let server = Self {
            database: self.database.clone(),
            host: self.host.clone(),
            search_only: self.search_only,
            about: self.about.clone(),
        };
        let outcome =
            tokio::task::spawn_blocking(move || server.run(&name, &arguments, &cancel)).await;
        watcher.abort();
        Ok(match outcome {
            Ok(Ok(text)) => result(text),
            Ok(Err(message)) => failure(message),
            Err(_) => failure("The tool stopped unexpectedly.".into()),
        }
        .into())
    }
}

/// Serves MCP on standard input and output until the client closes it or `cancel` aborts.
pub fn serve_stdio(server: McpServer, cancel: &Cancellation) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let service = server
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|error| error.to_string())?;
        let token = service.cancellation_token();
        let stop = cancel.clone();
        let watcher = tokio::spawn(async move {
            while stop.check().is_ok() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            token.cancel();
        });
        let finished = service.waiting().await.map_err(|error| error.to_string());
        watcher.abort();
        finished.map(|_| ())
    })
}
