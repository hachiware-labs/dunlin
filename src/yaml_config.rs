//! Reads `KnowledgeNestfile.yaml` into configuration values and the line of each value.
//!
//! The rules follow the YAML 1.2 core schema used by the JavaScript `yaml` package that this
//! replaced: plain scalars become null, booleans, numbers or strings; aliases and duplicate keys
//! are errors; lines point at the start of each value, or at the key of an empty value.

use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};

use crate::config_value::ConfigValue;

pub const SYNTAX_MESSAGE: &str = "Invalid YAML syntax or duplicate key.";
pub const ALIAS_MESSAGE: &str = "YAML aliases are unsupported.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathPart {
    Key(String),
    Index(usize),
}

#[derive(Debug)]
pub struct ParsedConfig {
    pub value: ConfigValue,
    /// The start line (1-based) of every value, keyed by its path.
    pub lines: Vec<(Vec<PathPart>, usize)>,
}

impl ParsedConfig {
    /// The line of the longest existing prefix of `path`, as diagnostics report it.
    pub fn line(&self, path: &[PathPart]) -> Option<usize> {
        (1..=path.len()).rev().find_map(|length| {
            self.lines
                .iter()
                .find(|(candidate, _)| candidate.as_slice() == &path[..length])
                .map(|(_, line)| *line)
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub line: Option<usize>,
    pub message: &'static str,
}

#[derive(Default)]
struct Events(Vec<(Event, Marker)>);

impl MarkedEventReceiver for Events {
    fn on_event(&mut self, event: Event, mark: Marker) {
        self.0.push((event, mark));
    }
}

pub fn parse_config(text: &str) -> Result<ParsedConfig, SyntaxError> {
    let mut events = Events::default();
    Parser::new_from_str(text)
        .load(&mut events, true)
        .map_err(|error| SyntaxError {
            line: Some(error.marker().line()),
            message: SYNTAX_MESSAGE,
        })?;
    let documents: Vec<usize> = events
        .0
        .iter()
        .enumerate()
        .filter(|(_, (event, _))| matches!(event, Event::DocumentStart))
        .map(|(index, _)| index)
        .collect();
    if let Some(&second) = documents.get(1) {
        return Err(SyntaxError {
            line: Some(events.0[second].1.line()),
            message: SYNTAX_MESSAGE,
        });
    }
    let mut builder = Builder {
        text,
        events: &events.0,
        position: 0,
        lines: Vec::new(),
        path: Vec::new(),
        unlinked: 0,
        aliases: false,
    };
    let value = match documents.first() {
        Some(&start) => {
            builder.position = start + 1;
            if matches!(builder.peek(), Some(Event::DocumentEnd)) {
                ConfigValue::Null
            } else {
                builder.node(None)?
            }
        }
        None => ConfigValue::Null,
    };
    if builder.aliases {
        return Err(SyntaxError {
            line: None,
            message: ALIAS_MESSAGE,
        });
    }
    Ok(ParsedConfig {
        value,
        lines: builder.lines,
    })
}

struct Builder<'a> {
    text: &'a str,
    events: &'a [(Event, Marker)],
    position: usize,
    lines: Vec<(Vec<PathPart>, usize)>,
    path: Vec<PathPart>,
    /// Keys that were not strings. The JavaScript reader could not find their lines.
    unlinked: usize,
    aliases: bool,
}

impl Builder<'_> {
    fn peek(&self) -> Option<&Event> {
        self.events.get(self.position).map(|(event, _)| event)
    }

    fn next(&mut self) -> Result<(&Event, Marker), SyntaxError> {
        let (event, mark) = self.events.get(self.position).ok_or(SyntaxError {
            line: None,
            message: SYNTAX_MESSAGE,
        })?;
        self.position += 1;
        Ok((event, *mark))
    }

    /// Reads one value. `owner` is the line of the key that holds it, used for empty values.
    fn node(&mut self, owner: Option<usize>) -> Result<ConfigValue, SyntaxError> {
        let (event, mark) = self.next()?;
        let event = event.clone();
        match event {
            Event::Alias(_) => {
                self.aliases = true;
                self.record(mark.line());
                Ok(ConfigValue::Null)
            }
            Event::Scalar(text, style, _, tag) => {
                // Empty values and block scalars (`|`, `>`) start on the line of their key.
                let empty = text.is_empty() && style == TScalarStyle::Plain && tag.is_none();
                let block = matches!(style, TScalarStyle::Literal | TScalarStyle::Folded);
                self.record(if empty || block {
                    owner.unwrap_or_else(|| self.indicator_line(mark))
                } else {
                    mark.line()
                });
                Ok(scalar(text, style, tag.as_ref()))
            }
            Event::SequenceStart(..) => {
                self.record(mark.line());
                let mut items = Vec::new();
                while !matches!(self.peek(), Some(Event::SequenceEnd)) {
                    self.path.push(PathPart::Index(items.len()));
                    let item = self.node(None);
                    self.path.pop();
                    items.push(item?);
                }
                self.next()?;
                Ok(ConfigValue::Array(items))
            }
            Event::MappingStart(..) => {
                self.record(mark.line());
                let mut entries: Vec<(String, ConfigValue)> = Vec::new();
                let mut keys: Vec<ConfigValue> = Vec::new();
                while !matches!(self.peek(), Some(Event::MappingEnd)) {
                    let (key_event, key_mark) = self.next()?;
                    let key = match key_event.clone() {
                        Event::Scalar(text, style, _, tag) => scalar(text, style, tag.as_ref()),
                        Event::Alias(_) => {
                            self.aliases = true;
                            ConfigValue::Null
                        }
                        // Complex keys are valid YAML but cannot become object keys here.
                        _ => {
                            return Err(SyntaxError {
                                line: Some(key_mark.line()),
                                message: SYNTAX_MESSAGE,
                            });
                        }
                    };
                    if keys.contains(&key) {
                        return Err(SyntaxError {
                            line: Some(key_mark.line()),
                            message: SYNTAX_MESSAGE,
                        });
                    }
                    let name = key_string(&key);
                    let unlinked = !matches!(key, ConfigValue::String(_));
                    keys.push(key);
                    self.path.push(PathPart::Key(name.clone()));
                    self.unlinked += usize::from(unlinked);
                    let value = self.node(Some(key_mark.line()));
                    self.unlinked -= usize::from(unlinked);
                    self.path.pop();
                    let value = value?;
                    // JavaScript objects keep the first position of a repeated key.
                    match entries.iter_mut().find(|(existing, _)| *existing == name) {
                        Some(entry) => entry.1 = value,
                        None => entries.push((name, value)),
                    }
                }
                self.next()?;
                Ok(ConfigValue::Object(entries))
            }
            _ => Err(SyntaxError {
                line: Some(mark.line()),
                message: SYNTAX_MESSAGE,
            }),
        }
    }

    /// The line of the `-` of an empty sequence item. The parser marks the next token instead,
    /// so this finds the closest earlier line that holds only `-`.
    fn indicator_line(&self, mark: Marker) -> usize {
        let before = self.text.get(..mark.index()).unwrap_or_default();
        let lines: Vec<&str> = before.split('\n').collect();
        lines
            .iter()
            .enumerate()
            .rev()
            .skip(1)
            .find(|(_, line)| {
                let content = line.split(" #").next().unwrap_or_default().trim();
                content == "-"
            })
            .map_or(mark.line(), |(index, _)| index + 1)
    }

    fn record(&mut self, line: usize) {
        if self.unlinked == 0
            && !self.path.is_empty()
            && !self.lines.iter().any(|(path, _)| *path == self.path)
        {
            self.lines.push((self.path.clone(), line));
        }
    }
}

/// A plain scalar resolved with the YAML 1.2 core schema.
fn scalar(text: String, style: TScalarStyle, tag: Option<&Tag>) -> ConfigValue {
    if let Some(tag) = tag {
        let core = tag.handle == "!!" || tag.handle == "tag:yaml.org,2002:";
        return match (core, tag.suffix.as_str()) {
            (true, "null") => ConfigValue::Null,
            (true, "bool" | "int" | "float") => resolve(&text).unwrap_or(ConfigValue::String(text)),
            _ => ConfigValue::String(text),
        };
    }
    if style != TScalarStyle::Plain {
        return ConfigValue::String(text);
    }
    resolve(&text).unwrap_or(ConfigValue::String(text))
}

fn resolve(text: &str) -> Option<ConfigValue> {
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return Some(ConfigValue::Null),
        "true" | "True" | "TRUE" => return Some(ConfigValue::Bool(true)),
        "false" | "False" | "FALSE" => return Some(ConfigValue::Bool(false)),
        ".nan" | ".NaN" | ".NAN" => return Some(ConfigValue::NonFinite("NaN".into())),
        _ => {}
    }
    let (sign, body) = match text.as_bytes().first() {
        Some(b'-') => (-1.0, &text[1..]),
        Some(b'+') => (1.0, &text[1..]),
        _ => (1.0, text),
    };
    if matches!(body, ".inf" | ".Inf" | ".INF") {
        return Some(ConfigValue::NonFinite(
            if sign < 0.0 { "-Infinity" } else { "Infinity" }.into(),
        ));
    }
    if let Some(digits) = text.strip_prefix("0o")
        && !digits.is_empty()
        && digits.bytes().all(|b| (b'0'..=b'7').contains(&b))
    {
        return Some(ConfigValue::Number(radix(digits, 8)));
    }
    if let Some(digits) = text.strip_prefix("0x")
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Some(ConfigValue::Number(radix(digits, 16)));
    }
    if is_decimal(body) {
        return body
            .parse::<f64>()
            .ok()
            .map(|value| ConfigValue::Number(sign * value));
    }
    None
}

fn radix(digits: &str, base: u32) -> f64 {
    digits.chars().fold(0.0, |value, digit| {
        value * f64::from(base) + f64::from(digit.to_digit(base).unwrap_or(0))
    })
}

/// `(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?`
fn is_decimal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - start
    };
    let whole = digits(&mut i);
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        let fraction = digits(&mut i);
        if whole == 0 && fraction == 0 {
            return false;
        }
    } else if whole == 0 {
        return false;
    }
    if i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
        i += 1;
        if i < bytes.len() && matches!(bytes[i], b'-' | b'+') {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == bytes.len()
}

/// The object key JavaScript uses for a scalar key.
fn key_string(key: &ConfigValue) -> String {
    match key {
        ConfigValue::Null => String::new(),
        ConfigValue::Bool(value) => value.to_string(),
        ConfigValue::Number(value) => {
            serde_json::to_string(&ConfigValue::Number(*value)).unwrap_or_default()
        }
        ConfigValue::NonFinite(name) => name.clone(),
        ConfigValue::String(text) => text.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> PathPart {
        PathPart::Key(name.into())
    }

    #[test]
    fn reads_values_and_lines() {
        let text = "init:\n  version: 1\n  type: rag\nsource:\n  modules:\n    - use: a\n      with:\n    -\n      use: b\nflag: TRUE\nn: -1.5e2\nh: 0x1F\nq: \"1\"\nz: ~\n";
        let parsed = parse_config(text).unwrap();
        assert_eq!(parsed.value.get("flag"), Some(&ConfigValue::Bool(true)));
        assert_eq!(parsed.value.get("n"), Some(&ConfigValue::Number(-150.0)));
        assert_eq!(parsed.value.get("h"), Some(&ConfigValue::Number(31.0)));
        assert_eq!(
            parsed.value.get("q"),
            Some(&ConfigValue::String("1".into()))
        );
        assert_eq!(parsed.value.get("z"), Some(&ConfigValue::Null));
        let modules = [key("source"), key("modules")];
        assert_eq!(parsed.line(&[key("init"), key("type")]), Some(3));
        assert_eq!(
            parsed.line(&[
                modules[0].clone(),
                modules[1].clone(),
                PathPart::Index(0),
                key("with")
            ]),
            Some(7)
        );
        assert_eq!(
            parsed.line(&[modules[0].clone(), modules[1].clone(), PathPart::Index(1)]),
            Some(9)
        );
        assert_eq!(parsed.line(&[key("missing")]), None);
    }

    #[test]
    fn rejects_aliases_duplicates_and_extra_documents() {
        assert_eq!(
            parse_config("a: &x 1\nb: *x\n").unwrap_err().message,
            ALIAS_MESSAGE
        );
        assert_eq!(
            parse_config("a: 1\na: 2\n").unwrap_err(),
            SyntaxError {
                line: Some(2),
                message: SYNTAX_MESSAGE
            }
        );
        assert_eq!(parse_config("a: 1\n---\nb: 2\n").unwrap_err().line, Some(2));
        assert_eq!(parse_config("").unwrap().value, ConfigValue::Null);
        assert!(parse_config("a: [1\n").is_err());
    }
}
