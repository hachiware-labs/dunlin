//! The configuration file as the new store's stages read it (docs/data-management.md, 9.6): a
//! section is a list, and an entry of the list is a module name with its arguments.
//!
//! Only what the new store can run is accepted. A configuration that asks for anything else is
//! refused with a reason, before anything is built.
//!
//! Not here yet: `extract` as more than one loader, `from`, normalization and context modules,
//! the vector index, and declarations that come from the modules themselves.

use serde_json::{Map, Value, json};
use yaml_rust2::{Yaml, YamlLoader};

use crate::records::{ModuleIdentity, Process};
use crate::transform::Chunking;

/// The sections that hold a list of modules.
pub const SECTIONS: [&str; 4] = ["extract", "transform", "index", "search"];

/// A configuration file: its top-level entries, in the order they are written.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    entries: Vec<(String, Value)>,
}

/// One entry of a section: a module and its arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub name: String,
    pub args: Map<String, Value>,
}

/// A section: its modules in order, and the tag it gives its result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Section {
    pub steps: Vec<Step>,
    pub tag: Option<String>,
}

/// What a configuration asks the stages to do.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// The tag of the `extract` section. The documents are read from the snapshot it points at.
    pub extract_tag: Option<String>,
    pub chunking: Chunking,
    /// The most chunks a search returns.
    pub top_k: usize,
}

fn value(yaml: &Yaml) -> Result<Value, String> {
    Ok(match yaml {
        Yaml::Null => Value::Null,
        Yaml::Boolean(value) => json!(value),
        Yaml::Integer(value) => json!(value),
        Yaml::Real(text) => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| format!("{text} is not a number."))?,
        Yaml::String(text) => json!(text),
        Yaml::Array(values) => Value::Array(values.iter().map(value).collect::<Result<_, _>>()?),
        Yaml::Hash(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, entry)| Ok((key_text(key)?, value(entry)?)))
                .collect::<Result<_, String>>()?,
        ),
        Yaml::Alias(_) | Yaml::BadValue => return Err("A value cannot be read.".into()),
    })
}

fn key_text(key: &Yaml) -> Result<String, String> {
    match key {
        Yaml::String(text) => Ok(text.clone()),
        _ => Err("A key is not text.".into()),
    }
}

/// The entries of a mapping, in the order they are written.
pub(crate) fn entries(yaml: &Yaml) -> Result<Vec<(String, Value)>, String> {
    match yaml {
        Yaml::Hash(entries) => entries
            .iter()
            .map(|(key, entry)| Ok((key_text(key)?, value(entry)?)))
            .collect(),
        _ => Err("A mapping is expected.".into()),
    }
}

/// The one document of a YAML text.
pub(crate) fn document(text: &str) -> Result<Yaml, String> {
    let mut documents = YamlLoader::load_from_str(text).map_err(|error| error.to_string())?;
    if documents.len() != 1 {
        return Err("One YAML document is expected.".into());
    }
    Ok(documents.remove(0))
}

/// The modules of a section value: a list whose entries have one key each.
pub fn section(name: &str, value: &Value) -> Result<Section, String> {
    let Value::Array(entries) = value else {
        return Err(format!("{name} is not a list of modules."));
    };
    let mut section = Section::default();
    for (position, entry) in entries.iter().enumerate() {
        let single = match entry {
            Value::Object(map) if map.len() == 1 => map.iter().next(),
            _ => None,
        };
        let Some((key, args)) = single else {
            return Err(format!(
                "Entry {} of {name} is not one module with its arguments. Check the indentation.",
                position + 1
            ));
        };
        if key == "tag" {
            let Value::String(tag) = args else {
                return Err(format!("The tag of {name} is not a name."));
            };
            if position + 1 != entries.len() {
                return Err(format!("The tag of {name} is not its last entry."));
            }
            section.tag = Some(tag.clone());
        } else if key.contains('.') {
            let args = match args {
                Value::Object(args) => args.clone(),
                Value::Null => Map::new(),
                _ => return Err(format!("The arguments of {key} are not a mapping.")),
            };
            section.steps.push(Step {
                name: key.clone(),
                args,
            });
        } else {
            return Err(format!("{key} in {name} is not a module."));
        }
    }
    Ok(section)
}

/// An argument that is a whole number between `min` and `max`.
fn whole(step: &Step, name: &str, min: u64, max: u64) -> Result<usize, String> {
    match step.args.get(name).and_then(Value::as_u64) {
        Some(number) if (min..=max).contains(&number) => Ok(number as usize),
        _ => Err(format!(
            "{} needs {name} as a whole number from {min} to {max}.",
            step.name
        )),
    }
}

/// Refuses arguments a module does not have, and values other than the one the new store runs.
fn only(step: &Step, known: &[&str], fixed: &[(&str, &str)]) -> Result<(), String> {
    for name in step.args.keys() {
        if !known.contains(&name.as_str()) && !fixed.iter().any(|(fixed, _)| fixed == name) {
            return Err(format!("{} has no argument {name}.", step.name));
        }
    }
    for (name, expected) in fixed {
        match step.args.get(*name) {
            None => {}
            Some(Value::String(given)) if given == expected => {}
            Some(_) => {
                return Err(format!(
                    "{} takes only {name}: {expected} on the new store.",
                    step.name
                ));
            }
        }
    }
    Ok(())
}

impl Settings {
    pub fn parse(text: &str) -> Result<Self, String> {
        Ok(Self {
            entries: entries(&document(text)?)?,
        })
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    /// Writes a section in place of the one with its name, or after the others.
    pub fn replace(&mut self, name: &str, value: Value) {
        match self.entries.iter_mut().find(|(key, _)| key == name) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((name.to_owned(), value)),
        }
    }

    /// The modules of a section. A section that is not written has none.
    pub fn section(&self, name: &str) -> Result<Section, String> {
        match self.get(name) {
            Some(value) => section(name, value),
            None => Ok(Section::default()),
        }
    }

    /// The file again, in the syntax it is read in. Arguments are written on one line.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for (name, value) in &self.entries {
            let listed = match value {
                Value::Array(entries) if section(name, value).is_ok() => Some(entries),
                _ => None,
            };
            match listed {
                Some(entries) => {
                    text.push_str(&format!("{name}:\n"));
                    for entry in entries {
                        let (key, args) = entry
                            .as_object()
                            .and_then(|map| map.iter().next())
                            .expect("a section entry has one key");
                        text.push_str(&format!("  - {key}: {args}\n"));
                    }
                }
                None => text.push_str(&format!("{name}: {value}\n")),
            }
            text.push('\n');
        }
        text.truncate(text.trim_end().len());
        text.push('\n');
        text
    }

    /// Checks the configuration against what the modules take and what they need of each other,
    /// and says what the stages are to do. The error is the reason it cannot run.
    pub fn plan(&self) -> Result<Plan, String> {
        let extract = self.section("extract")?;
        for step in &extract.steps {
            match step.name.as_str() {
                "source.filesystem" | "loader.text" => {}
                name => return Err(format!("{name} is not on the new store yet.")),
            }
        }

        let transform = self.section("transform")?;
        let mut chunking = None;
        for step in &transform.steps {
            let made = match step.name.as_str() {
                "chunk.length" => {
                    only(step, &["size", "overlap"], &[("unit", "characters")])?;
                    let size = whole(step, "size", 1, 1_048_576)?;
                    let overlap = whole(step, "overlap", 0, 1_048_576)?;
                    if overlap >= size {
                        return Err("chunk.length needs overlap smaller than size.".into());
                    }
                    Chunking::Length { size, overlap }
                }
                "chunk.structure" => {
                    only(step, &["max_chars"], &[])?;
                    Chunking::Structure {
                        max_chars: whole(step, "max_chars", 1, 1_048_576)?,
                    }
                }
                "text.normalize" => {
                    only(step, &[], &[("unicode", "nfkc")])?;
                    continue;
                }
                name => return Err(format!("{name} is not on the new store yet.")),
            };
            if chunking.replace(made).is_some() {
                return Err("transform has more than one chunking module.".into());
            }
        }
        let chunking = chunking.ok_or("transform has no chunking module.")?;

        let index = self.section("index")?;
        let mut keyword = false;
        let mut vector = false;
        for step in &index.steps {
            match step.name.as_str() {
                "index.keyword" => {
                    only(step, &[], &[("tokenizer", "lindera")])?;
                    keyword = true;
                }
                "index.vector" => vector = true,
                name => return Err(format!("{name} is not on the new store yet.")),
            }
        }

        let search = self.section("search")?;
        if search.tag.is_some() {
            return Err("search makes no snapshot, so it has no tag.".into());
        }
        let mut top_k = None;
        for step in &search.steps {
            match step.name.as_str() {
                "retrieval.keyword" => {
                    only(step, &["top_k"], &[])?;
                    if !keyword {
                        return Err("retrieval.keyword needs index.keyword in index.".into());
                    }
                    if top_k.replace(whole(step, "top_k", 1, 100)?).is_some() {
                        return Err("search has more than one retrieval.keyword.".into());
                    }
                }
                "retrieval.vector" if !vector => {
                    return Err("retrieval.vector needs index.vector in index.".into());
                }
                "retrieval.vector" => {}
                name => return Err(format!("{name} is not on the new store yet.")),
            }
        }
        if vector {
            return Err("index.vector is not on the new store yet.".into());
        }
        if !keyword {
            return Err("index has no index module.".into());
        }
        Ok(Plan {
            extract_tag: extract.tag,
            chunking,
            top_k: top_k.ok_or("search has no retrieval module.")?,
        })
    }
}

impl Plan {
    /// What the search is to the store. It is part of the key of an evaluation, and of nothing
    /// a build makes.
    pub fn retrieval(&self) -> Process {
        Process {
            modules: vec![ModuleIdentity {
                name: "retrieval.keyword".into(),
                compatibility: 1,
                args: json!({ "top_k": self.top_k }),
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "extract:\n  - source.filesystem: { path: ./docs }\n  - loader.text: {}\n  - tag: docs\n\ntransform:\n  - chunk.length: { unit: characters, size: 200, overlap: 20 }\n  - tag: chunks-200\n\nindex:\n  - index.keyword: { tokenizer: lindera }\n  - tag: main\n\nsearch:\n  - retrieval.keyword: { top_k: 10 }\n\nserve:\n  tag: release\n";

    #[test]
    fn reads_a_configuration_and_writes_it_again() {
        let settings = Settings::parse(CONFIG).unwrap();
        let plan = settings.plan().unwrap();
        assert_eq!(plan.extract_tag.as_deref(), Some("docs"));
        assert_eq!(
            plan.chunking,
            Chunking::Length {
                size: 200,
                overlap: 20
            }
        );
        assert_eq!(plan.top_k, 10);
        assert_eq!(
            settings.section("index").unwrap().tag.as_deref(),
            Some("main")
        );

        let written = settings.to_text();
        assert!(written.contains("transform:\n  - chunk.length: {"));
        assert_eq!(Settings::parse(&written).unwrap(), settings);
    }

    #[test]
    fn says_why_a_configuration_cannot_run() {
        let reason = |section: &str, entries: &str| {
            let mut settings = Settings::parse(CONFIG).unwrap();
            let replaced = Settings::parse(&format!("{section}:\n{entries}")).unwrap();
            settings.replace(section, replaced.get(section).unwrap().clone());
            settings.plan().unwrap_err()
        };
        assert_eq!(
            reason("search", "  - retrieval.vector: { top_k: 10 }\n"),
            "retrieval.vector needs index.vector in index."
        );
        assert_eq!(
            reason(
                "transform",
                "  - chunk.length: { size: 100, overlap: 100 }\n"
            ),
            "chunk.length needs overlap smaller than size."
        );
        assert_eq!(
            reason("transform", "  - chunk.length: { size: 100, step: 5 }\n"),
            "chunk.length has no argument step."
        );
        assert_eq!(
            reason("transform", "  - chunk.semantic: {}\n"),
            "chunk.semantic is not on the new store yet."
        );
        assert_eq!(
            reason(
                "transform",
                "  - chunk.length:\n    size: 100\n    overlap: 0\n"
            ),
            "Entry 1 of transform is not one module with its arguments. Check the indentation."
        );
    }
}
