//! `loader.command`: parses files with an external command (spec 6.7).
//!
//! The command runs without a shell. It gets a temporary copy of the original as `{input}` or the
//! bytes on standard input, and prints Markdown, text or JSON. Behaviour and messages follow the
//! JavaScript implementation this replaces.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::Cancellation;
use crate::config_value::ConfigValue;

const STDOUT_LIMIT: usize = 16 * 1024 * 1024;
const STDERR_TAIL: usize = 2048;
const TEXT_LIMIT: usize = 1024 * 1024;

/// The arguments of one `loader.command` instance, after validation.
#[derive(Clone, Debug)]
pub struct CommandRoute {
    pub module: String,
    pub command: Vec<String>,
    pub input: String,
    pub output: String,
    /// Environment variables in configuration order, with `${NAME}` references unresolved.
    pub env: Vec<(String, String)>,
    pub env_value: ConfigValue,
    pub version: Option<String>,
    pub version_command: Option<Vec<String>>,
    pub timeout_ms: u64,
    pub concurrency: usize,
}

impl CommandRoute {
    /// Reads `{ module, args }` as JavaScript sends it.
    pub fn from_value(value: &ConfigValue) -> Option<Self> {
        let args = value.get("args")?;
        let strings = |key: &str| match args.get(key) {
            Some(ConfigValue::Array(items)) => Some(
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect(),
            ),
            _ => None,
        };
        let number = |key: &str, fallback: f64| match args.get(key) {
            Some(ConfigValue::Number(value)) => *value,
            _ => fallback,
        };
        let env_value = args
            .get("env")
            .cloned()
            .unwrap_or(ConfigValue::Object(Vec::new()));
        let env = match &env_value {
            ConfigValue::Object(entries) => entries
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_owned()))
                })
                .collect(),
            _ => Vec::new(),
        };
        Some(Self {
            module: value.get("module")?.as_str()?.to_owned(),
            command: strings("command")?,
            input: args
                .get("input")
                .and_then(ConfigValue::as_str)
                .unwrap_or("file")
                .to_owned(),
            output: args
                .get("output")
                .and_then(ConfigValue::as_str)
                .unwrap_or("markdown")
                .to_owned(),
            env,
            env_value,
            version: args
                .get("version")
                .and_then(ConfigValue::as_str)
                .map(str::to_owned),
            version_command: strings("version_command"),
            timeout_ms: number("timeout_ms", 120_000.0) as u64,
            concurrency: number("concurrency", 2.0).max(1.0) as usize,
        })
    }

    /// `{ module, command, input, output, env, version }`, part of the extraction cache key.
    pub fn identity(&self, version: Option<&str>) -> ConfigValue {
        let text = |value: &str| ConfigValue::String(value.to_owned());
        ConfigValue::Object(vec![
            ("module".into(), text(&self.module)),
            (
                "command".into(),
                ConfigValue::Array(self.command.iter().map(|part| text(part)).collect()),
            ),
            ("input".into(), text(&self.input)),
            ("output".into(), text(&self.output)),
            ("env".into(), self.env_value.clone()),
            ("version".into(), version.map_or(ConfigValue::Null, text)),
        ])
    }

    fn environment(&self) -> Result<Vec<(String, String)>, String> {
        self.env
            .iter()
            .map(|(key, value)| {
                crate::config_check::resolve_config_value(
                    value,
                    &format!("loader.command.with.env.{key}"),
                    true,
                )
                .map(|resolved| (key.clone(), resolved.unwrap_or_default()))
            })
            .collect()
    }
}

/// Runs `version_command` once and returns the version text, or the configured `version`.
pub fn command_version(
    route: &CommandRoute,
    root: &str,
    cancel: &Cancellation,
) -> Result<Option<String>, String> {
    if let Some(version) = &route.version {
        return Ok(Some(version.clone()));
    }
    let Some(argv) = &route.version_command else {
        return Ok(None);
    };
    let stdout = execute(
        argv,
        root,
        &route.environment()?,
        route.timeout_ms.min(30_000),
        None,
        cancel,
    )?;
    let version = decode(&stdout)?
        .trim_matches(crate::registry::js_space)
        .to_owned();
    if version.is_empty() {
        return Err(format!(
            "{} version_command printed no version",
            route.module
        ));
    }
    Ok(Some(version))
}

/// Limits each `loader.command` instance to its `concurrency`.
#[derive(Default)]
pub struct Slots {
    active: Mutex<HashMap<String, usize>>,
    freed: Condvar,
}

impl Slots {
    fn acquire(&self, route: &CommandRoute) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while active.get(&route.module).copied().unwrap_or(0) >= route.concurrency {
            active = self
                .freed
                .wait(active)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *active.entry(route.module.clone()).or_default() += 1;
    }

    fn release(&self, route: &CommandRoute) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = active.get_mut(&route.module) {
            *count = count.saturating_sub(1);
        }
        self.freed.notify_all();
    }
}

/// Parses one file. Returns `{ content, format, locations?, parser }`.
pub fn parse(
    route: &CommandRoute,
    bytes: &[u8],
    name: &str,
    root: &str,
    version: Option<&str>,
    slots: &Slots,
    cancel: &Cancellation,
) -> Result<ConfigValue, String> {
    slots.acquire(route);
    let result = parse_inner(route, bytes, name, root, version, cancel);
    slots.release(route);
    result
}

fn parse_inner(
    route: &CommandRoute,
    bytes: &[u8],
    name: &str,
    root: &str,
    version: Option<&str>,
    cancel: &Cancellation,
) -> Result<ConfigValue, String> {
    let extension = match name.rfind('.') {
        Some(0) | None => String::new(),
        Some(index) => name[index..].to_lowercase(),
    };
    let directory = if route.input == "file" {
        Some(TempDir::new()?)
    } else {
        None
    };
    let input = match &directory {
        Some(directory) => {
            let suffix: String = extension.chars().skip(1).collect();
            let keep = !suffix.is_empty()
                && suffix.chars().count() <= 16
                && suffix
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
            let path = directory.0.join(format!(
                "source{}",
                if keep { extension.as_str() } else { "" }
            ));
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .and_then(|mut file| file.write_all(bytes))
                .map_err(|error| error.to_string())?;
            Some(path.to_string_lossy().into_owned())
        }
        None => None,
    };
    let argv: Vec<String> = route
        .command
        .iter()
        .enumerate()
        .map(|(index, part)| {
            if index == 0 {
                part.clone()
            } else {
                substitute(part, input.as_deref(), name, &extension)
            }
        })
        .collect();
    let stdin = (route.input == "stdin").then_some(bytes);
    let stdout = execute(
        &argv,
        root,
        &route.environment()?,
        route.timeout_ms,
        stdin,
        cancel,
    )?;
    let mut parsed = output(&route.output, &decode(&stdout)?)?;
    let first = &route.command[0];
    let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
    let lower = base.to_ascii_lowercase();
    let tool = if lower.ends_with(".exe") || lower.ends_with(".com") {
        &base[..base.len() - 4]
    } else {
        base
    };
    let mut parser = vec![
        ("tool".to_owned(), ConfigValue::String(tool.to_owned())),
        (
            "module".to_owned(),
            ConfigValue::String(route.module.clone()),
        ),
    ];
    if let Some(version) = version {
        parser.push((
            "version".to_owned(),
            ConfigValue::String(version.to_owned()),
        ));
    }
    if let ConfigValue::Object(entries) = &mut parsed {
        entries.push(("parser".to_owned(), ConfigValue::Object(parser)));
    }
    drop(directory);
    Ok(parsed)
}

/// A temporary directory removed on drop.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Result<Self, String> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|time| time.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "knowledge-nest-parse-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).map_err(|error| error.to_string())?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `{{` and `}}` become braces; `{input}`, `{name}` and `{ext}` become their values.
fn substitute(part: &str, input: Option<&str>, name: &str, extension: &str) -> String {
    let chars: Vec<char> = part.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' && chars.get(i + 1) == Some(&'{') {
            out.push('{');
            i += 2;
        } else if chars[i] == '}' && chars.get(i + 1) == Some(&'}') {
            out.push('}');
            i += 2;
        } else if chars[i] == '{'
            && let Some(offset) = chars[i + 1..].iter().position(|c| *c == '{' || *c == '}')
            && chars[i + 1 + offset] == '}'
        {
            let key: String = chars[i + 1..i + 1 + offset].iter().collect();
            out.push_str(match key.as_str() {
                "input" => input.unwrap_or("undefined"),
                "name" => name,
                "ext" => extension,
                _ => "undefined",
            });
            i += offset + 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// UTF-8 without a byte order mark.
fn decode(bytes: &[u8]) -> Result<String, String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "Parser output is not valid UTF-8".to_owned())?;
    Ok(text.strip_prefix('\u{feff}').unwrap_or(text).to_owned())
}

fn output(kind: &str, text: &str) -> Result<ConfigValue, String> {
    let parsed = if kind == "json" {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|error| format!("Parser output is not valid JSON: {error}"))?;
        check_json(&value).map_err(|(path, message)| {
            format!(
                "Parser output does not match the JSON format at {}: {message}",
                if path.is_empty() {
                    "the top level".into()
                } else {
                    path
                }
            )
        })?;
        serde_json::from_str::<ConfigValue>(text)
            .map_err(|error| format!("Parser output is not valid JSON: {error}"))?
    } else {
        ConfigValue::Object(vec![
            ("content".into(), ConfigValue::String(text.to_owned())),
            ("format".into(), ConfigValue::String(kind.to_owned())),
        ])
    };
    // Keep the schema's key order: content, format, locations.
    let content = parsed
        .get("content")
        .and_then(ConfigValue::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut entries = vec![
        ("content".to_owned(), ConfigValue::String(content.clone())),
        (
            "format".to_owned(),
            parsed.get("format").cloned().unwrap_or(ConfigValue::Null),
        ),
    ];
    // zod returned each location with its keys in schema order.
    if let Some(ConfigValue::Array(locations)) = parsed.get("locations") {
        let ordered = locations
            .iter()
            .map(|location| {
                let mut fields = Vec::new();
                if let Some(range) = location.get("range") {
                    let bound =
                        |key: &str| range.get(key).map(|value| (key.to_owned(), value.clone()));
                    fields.push((
                        "range".to_owned(),
                        ConfigValue::Object(
                            bound("startLine")
                                .into_iter()
                                .chain(bound("endLine"))
                                .collect(),
                        ),
                    ));
                }
                for key in ["page", "symbol", "bbox"] {
                    if let Some(value) = location.get(key) {
                        fields.push((key.to_owned(), value.clone()));
                    }
                }
                ConfigValue::Object(fields)
            })
            .collect();
        entries.push(("locations".to_owned(), ConfigValue::Array(ordered)));
    }
    if content.trim_matches(crate::registry::js_space).is_empty() {
        return Err("Parser output has no text".into());
    }
    if content.len() > TEXT_LIMIT {
        return Err("Extracted text exceeds 1 MiB".into());
    }
    let lines = content.split('\n').count() - usize::from(content.ends_with('\n'));
    if let Some(ConfigValue::Array(locations)) = parsed.get("locations") {
        for location in locations {
            if let Some(ConfigValue::Number(end)) =
                location.get("range").and_then(|range| range.get("endLine"))
                && *end as usize > lines
            {
                return Err(format!(
                    "Parser location ends at line {end}, after the last line {lines}"
                ));
            }
        }
    }
    Ok(ConfigValue::Object(entries))
}

/// `{ content: string, format: markdown|text, locations?: [...] }` with no other keys.
fn check_json(value: &serde_json::Value) -> Result<(), (String, String)> {
    use serde_json::Value;
    let integer = |value: &Value, min: i64, max: i64| {
        value
            .as_f64()
            .is_some_and(|n| n.fract() == 0.0 && n >= min as f64 && n <= max as f64)
    };
    let Value::Object(map) = value else {
        return Err((String::new(), "expected an object".into()));
    };
    if !map.get("content").is_some_and(Value::is_string) {
        return Err(("content".into(), "expected a string".into()));
    }
    if !matches!(
        map.get("format").and_then(Value::as_str),
        Some("markdown" | "text")
    ) {
        return Err(("format".into(), "expected markdown or text".into()));
    }
    if let Some(locations) = map.get("locations") {
        let Value::Array(items) = locations else {
            return Err(("locations".into(), "expected an array".into()));
        };
        for (index, item) in items.iter().enumerate() {
            let at = |field: &str| {
                format!(
                    "locations.{index}{}{field}",
                    if field.is_empty() { "" } else { "." }
                )
            };
            let Value::Object(location) = item else {
                return Err((at(""), "expected an object".into()));
            };
            let Some(Value::Object(range)) = location.get("range") else {
                return Err((at("range"), "expected an object".into()));
            };
            for key in ["startLine", "endLine"] {
                if !range
                    .get(key)
                    .is_some_and(|value| integer(value, 1, i64::MAX))
                {
                    return Err((
                        at(&format!("range.{key}")),
                        "expected an integer of at least 1".into(),
                    ));
                }
            }
            if range
                .keys()
                .any(|key| key != "startLine" && key != "endLine")
            {
                return Err((at("range"), "unrecognized key".into()));
            }
            if range["endLine"].as_f64() < range["startLine"].as_f64() {
                return Err((
                    at("range"),
                    "endLine must not be less than startLine".into(),
                ));
            }
            if location
                .get("page")
                .is_some_and(|page| !integer(page, 1, i64::MAX))
            {
                return Err((at("page"), "expected an integer of at least 1".into()));
            }
            if location
                .get("symbol")
                .is_some_and(|symbol| symbol.as_str().is_none_or(str::is_empty))
            {
                return Err((at("symbol"), "expected a nonempty string".into()));
            }
            if let Some(bbox) = location.get("bbox")
                && !bbox.as_array().is_some_and(|items| {
                    items.len() == 4 && items.iter().all(|item| integer(item, 0, 1000))
                })
            {
                return Err((at("bbox"), "expected four integers from 0 to 1000".into()));
            }
            if location
                .keys()
                .any(|key| !["range", "page", "symbol", "bbox"].contains(&key.as_str()))
            {
                return Err((at(""), "unrecognized key".into()));
            }
        }
    }
    if map
        .keys()
        .any(|key| !["content", "format", "locations"].contains(&key.as_str()))
    {
        return Err((String::new(), "unrecognized key".into()));
    }
    Ok(())
}

/// Runs a command without a shell and returns standard output.
fn execute(
    argv: &[String],
    cwd: &str,
    env: &[(String, String)],
    timeout_ms: u64,
    stdin: Option<&[u8]>,
    cancel: &Cancellation,
) -> Result<Vec<u8>, String> {
    if cancel.check().is_err() {
        return Err("Parsing was cancelled".into());
    }
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .envs(
            env.iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start {}: {error}", argv[0]))?;
    let writer = stdin.map(|bytes| {
        let mut pipe = child.stdin.take();
        let bytes = bytes.to_vec();
        std::thread::spawn(move || {
            if let Some(pipe) = pipe.as_mut() {
                let _ = pipe.write_all(&bytes);
            }
        })
    });
    let too_large = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stdout_reader = {
        let mut pipe = child.stdout.take();
        let too_large = too_large.clone();
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let mut buffer = [0; 65536];
            while let Some(pipe) = pipe.as_mut() {
                match pipe.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if output.len() + read > STDOUT_LIMIT {
                            too_large.store(true, std::sync::atomic::Ordering::SeqCst);
                            break;
                        }
                        output.extend_from_slice(&buffer[..read]);
                    }
                }
            }
            output
        })
    };
    let stderr_reader = {
        let mut pipe = child.stderr.take();
        std::thread::spawn(move || {
            let mut tail: Vec<u8> = Vec::new();
            let mut buffer = [0; 4096];
            while let Some(pipe) = pipe.as_mut() {
                match pipe.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        tail.extend_from_slice(&buffer[..read]);
                        if tail.len() > STDERR_TAIL {
                            tail.drain(..tail.len() - STDERR_TAIL);
                        }
                    }
                }
            }
            tail
        })
    };
    let started = Instant::now();
    let mut failure: Option<String> = None;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break Some(status);
        }
        if failure.is_none() {
            if started.elapsed() >= Duration::from_millis(timeout_ms) {
                failure = Some(format!("Parser timed out after {timeout_ms} ms"));
            } else if too_large.load(std::sync::atomic::Ordering::SeqCst) {
                failure = Some("Parser output exceeds 16 MiB".into());
            } else if cancel.check().is_err() {
                failure = Some("Parsing was cancelled".into());
            }
            if failure.is_some() {
                let _ = child.kill();
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    if too_large.load(std::sync::atomic::Ordering::SeqCst) && failure.is_none() {
        failure = Some("Parser output exceeds 16 MiB".into());
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    let status = status.ok_or("The parser did not finish")?;
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr)
            .trim_matches(crate::registry::js_space)
            .to_owned();
        let code = status
            .code()
            .map_or_else(|| "a signal".to_owned(), |code| code.to_string());
        return Err(format!(
            "Parser exited with {code}{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_placeholders_and_checks_output() {
        assert_eq!(
            substitute(
                "a{{x}}b{input}-{name}{ext}{zz}",
                Some("/t/source.md"),
                "doc.MD",
                ".md"
            ),
            "a{x}b/t/source.md-doc.MD.mdundefined"
        );
        assert!(output("markdown", "  \n").is_err());
        let parsed = output("json", r#"{"content":"a\nb\n","format":"text","locations":[{"page":1,"range":{"startLine":1,"endLine":2}}]}"#).unwrap();
        assert_eq!(
            parsed.get("format"),
            Some(&ConfigValue::String("text".into()))
        );
        assert!(
            output("json", r#"{"content":"a","format":"text","extra":1}"#)
                .unwrap_err()
                .contains("does not match")
        );
        assert!(output("json", r#"{"content":"a","format":"text","locations":[{"range":{"startLine":1,"endLine":5}}]}"#).unwrap_err().contains("after the last line 1"));
        assert_eq!(decode("\u{feff}x".as_bytes()).unwrap(), "x");
    }
}
