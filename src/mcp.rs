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
use crate::{Cancellation, Evidence, ExploreOptions, KnowledgeBase};

/// The project search and read of a project server.
pub trait McpHost: Send + Sync {
    /// Runs `knowledge.search` with `{ query, limit?, sourceIds?, budgetChars? }` and returns its
    /// result as JSON text, or the error message.
    fn search(&self, arguments: &ConfigValue, cancel: &Cancellation) -> Result<String, String>;
    /// Reads evidence and returns `{ content, evidence }` as JSON text, or the error message.
    fn read(&self, evidence: &Evidence, cancel: &Cancellation) -> Result<String, String>;
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
            r#"{{"type":"object","$schema":"{SCHEMA}","properties":{{"query":{{"type":"string","description":"Words or a question to search for."}},"limit":{{"type":"integer","minimum":1,"maximum":100,"description":"The most results to return."}},"source_ids":{{"minItems":1,"maxItems":100,"type":"array","items":{SOURCE_ID},"description":"Search only these sources: the source IDs of the project."}},"budget_chars":{{"type":"integer","minimum":1,"maximum":1000000,"description":"The most characters of source context to return."}}}},"required":["query"]}}"#
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

fn read_schema() -> String {
    format!(
        r#"{{"type":"object","$schema":"{SCHEMA}","properties":{{"evidence":{{"type":"object","properties":{{"source":{{"type":"object","properties":{{"sourceId":{{"type":"string"}},"objectId":{{"type":"string"}},"url":{{"type":"string"}},"version":{{"type":"string"}},"modifiedAtMs":{{"anyOf":[{{"type":"integer","minimum":-{SAFE},"maximum":{SAFE}}},{{"type":"null"}}]}}}},"required":["sourceId","objectId","url","version","modifiedAtMs"]}},"range":{{"type":"object","properties":{{"startLine":{{"type":"integer","exclusiveMinimum":0,"maximum":{SAFE}}},"endLine":{{"type":"integer","exclusiveMinimum":0,"maximum":{SAFE}}}}},"required":["startLine","endLine"]}},"acquiredAtMs":{{"type":"integer","minimum":0,"maximum":{SAFE}}},"byteRange":{{"type":"object","properties":{{"start":{{"type":"integer","minimum":0,"maximum":{SAFE}}},"end":{{"type":"integer","exclusiveMinimum":0,"maximum":{SAFE}}}}},"required":["start","end"]}}}},"required":["source","range","acquiredAtMs"]}}}},"required":["evidence"]}}"#
    )
}

fn explore_schema() -> String {
    format!(
        r#"{{"type":"object","$schema":"{SCHEMA}","properties":{{"query":{{"type":"string"}},"sourceIds":{},"seedLimit":{{"type":"integer","minimum":1,"maximum":100}},"maxSteps":{{"type":"integer","minimum":1,"maximum":100}},"timeoutMs":{{"type":"integer","minimum":1,"maximum":60000}}}},"required":["query"]}}"#,
        source_ids()
    )
}

fn tool(name: &'static str, description: &'static str, schema: &str) -> Tool {
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

/// `{ content: [{ type: "text", text }], structuredContent }`
fn result(text: String) -> CallToolResult {
    let structured: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = Some(structured);
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
}

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
        }
    }

    fn tools(&self) -> Vec<(Tool, String)> {
        let project = self.host.is_some();
        let mut tools = vec![(
            tool(
                "knowledge.search",
                if project {
                    "Retrieve indexed evidence and assemble source context using the project search settings."
                } else {
                    "Find indexed source ranges using keyword search."
                },
                &search_schema(project),
            ),
            search_schema(project),
        )];
        if self.search_only {
            return tools;
        }
        tools.push((
            tool(
                "knowledge.read",
                "Read the indexed lines identified by evidence from knowledge.search.",
                &read_schema(),
            ),
            read_schema(),
        ));
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
                host.search(&ConfigValue::Object(options), cancel)
            }
            ("knowledge.read", Some(host)) => {
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
            Ok(text) => format!(
                r#"{{"content":[{{"type":"text","text":{}}}],"structuredContent":{text}}}"#,
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
