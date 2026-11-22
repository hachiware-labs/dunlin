//! The configuration file as the new store's stages read it (docs/data-management.md, 9.6): a
//! section is a list, and an entry of the list is a module name with its arguments.
//!
//! Only what the new store can run is accepted. A configuration that asks for anything else is
//! refused with a reason, before anything is built.
//!
//! Not here yet: `extract` as more than one loader, normalization modules, the vector index, and
//! declarations that come from the modules themselves.

use std::path::Path;

use serde_json::{Map, Value, json};
use yaml_rust2::{Yaml, YamlLoader};

use crate::extract::Apply;
use crate::records::{ModuleIdentity, Process};
use crate::transform::{Chunking, Context};

/// The sections that hold a list of modules and that a build or a search runs.
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

/// A section: its modules in order, the tag it gives its result, or the name it starts from
/// instead of making anything.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Section {
    pub steps: Vec<Step>,
    pub tag: Option<String>,
    pub from: Option<String>,
}

/// A section that ends in a snapshot: it names one that is there, or says how one is made.
#[derive(Clone, Debug, PartialEq)]
pub enum Stage<T> {
    From(String),
    Make { what: T, tag: Option<String> },
}

impl<T> Stage<T> {
    /// The name the snapshot of the section has: the one it starts from, or its tag.
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::From(name) => Some(name),
            Self::Make { tag, .. } => tag.as_deref(),
        }
    }
}

/// What `extract` reads.
#[derive(Clone, Debug, PartialEq)]
pub struct Scan {
    /// The ID of the source: `id` of `source.filesystem`, or the name of its folder.
    pub source: String,
    /// The folder, as it is written. A relative path starts at the project.
    pub path: String,
    /// Which changes of the folder are taken in.
    pub apply: Apply,
}

/// How `transform` splits the text, and what the chunks say about their place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunks {
    pub chunking: Chunking,
    pub context: Context,
}

impl Chunks {
    /// What the chunking is to the store, with the context it adds.
    pub fn process(&self) -> Process {
        crate::transform::process(self.chunking, self.context)
    }
}

/// What a result is measured with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Judging {
    /// The name of the evaluation data.
    pub data: String,
    /// The cases to use: `dev` or `test`.
    pub split: String,
    /// The name the result is given.
    pub tag: Option<String>,
}

/// What a configuration asks the stages to do.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// Not written when `transform` starts from a name.
    pub extract: Option<Stage<Scan>>,
    /// Not written when `index` starts from a name.
    pub transform: Option<Stage<Chunks>>,
    pub index: Stage<()>,
    /// The most chunks a search returns.
    pub top_k: usize,
    pub eval: Option<Judging>,
    /// The name that is served, from `serve`.
    pub release: Option<String>,
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
        } else if key == "from" {
            let Value::String(from) = args else {
                return Err(format!("The from of {name} is not a name."));
            };
            if position != 0 {
                return Err(format!("The from of {name} is not its first entry."));
            }
            section.from = Some(from.clone());
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
    if section.from.is_some() && (!section.steps.is_empty() || section.tag.is_some()) {
        return Err(format!(
            "{name} starts from a name, so it has no modules and no tag."
        ));
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

/// `context` of a chunking module: what a chunk says about its place in the document.
fn context_asked(step: &Step) -> Result<Context, String> {
    let module = &step.name;
    let asked = match step.args.get("context") {
        None => return Ok(Context::default()),
        Some(Value::Object(asked)) => asked,
        Some(_) => return Err(format!("{module} takes context as a mapping.")),
    };
    let mut context = Context::default();
    for (name, value) in asked {
        let part = match name.as_str() {
            "document" => &mut context.document,
            "headings" => &mut context.headings,
            "prepend" => &mut context.prepend,
            _ => return Err(format!("The context of {module} has no {name}.")),
        };
        *part = value
            .as_bool()
            .ok_or_else(|| format!("The context of {module} takes {name} as true or false."))?;
    }
    if context.prepend && !context.document && !context.headings {
        return Err(format!(
            "The context of {module} has nothing to prepend. Add document or headings."
        ));
    }
    if context.document && !context.prepend {
        return Err(format!(
            "The context of {module} uses document only with prepend."
        ));
    }
    Ok(context)
}

/// `apply` of `source.filesystem`: the changes of the folder that are taken in.
fn changes_taken(value: &Value) -> Result<Apply, String> {
    const REASON: &str = "source.filesystem takes apply as a list of add, update and delete.";
    let Value::Array(names) = value else {
        return Err(REASON.into());
    };
    let mut apply = Apply {
        add: false,
        update: false,
        delete: false,
    };
    for name in names {
        let change = match name.as_str() {
            Some("add") => &mut apply.add,
            Some("update") => &mut apply.update,
            Some("delete") => &mut apply.delete,
            _ => return Err(REASON.into()),
        };
        if std::mem::replace(change, true) {
            return Err(REASON.into());
        }
    }
    Ok(apply)
}

/// A name of a source or of an optimization: letters, digits, `_`, `.` and `-`.
/// Whether a configuration file is written for the versioned store: a section that builds is
/// a list of modules there. The commands that both stores have go by this.
pub fn staged(text: &str) -> bool {
    Settings::parse(text).is_ok_and(|settings| {
        ["extract", "transform", "index"]
            .iter()
            .any(|name| settings.get(name).is_some_and(Value::is_array))
    })
}

pub(crate) fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// What `extract` reads, from its modules.
fn scan(extract: &Section) -> Result<Scan, String> {
    let mut scan = None;
    for step in &extract.steps {
        match step.name.as_str() {
            "source.filesystem" => {
                only(step, &["path", "id", "apply"], &[])?;
                let Some(path) = step.args.get("path").and_then(Value::as_str) else {
                    return Err("source.filesystem needs path as a folder.".into());
                };
                let source = match step.args.get("id") {
                    Some(id) => id.as_str().map(str::to_owned),
                    None => Path::new(path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned()),
                }
                .filter(|source| plain_name(source))
                .ok_or("source.filesystem needs id as a name of letters, digits, _, . and -.")?;
                let apply = match step.args.get("apply") {
                    Some(changes) => changes_taken(changes)?,
                    None => Apply::ALL,
                };
                let made = Scan {
                    source,
                    path: path.to_owned(),
                    apply,
                };
                if scan.replace(made).is_some() {
                    return Err(
                        "More than one source.filesystem is not on the new store yet.".into(),
                    );
                }
            }
            "loader.text" => {}
            name => return Err(format!("{name} is not on the new store yet.")),
        }
    }
    scan.ok_or_else(|| "extract has no source.filesystem.".into())
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

    /// Takes a top-level entry out.
    pub fn remove(&mut self, name: &str) {
        self.entries.retain(|(key, _)| key != name);
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
    #[allow(clippy::too_many_lines)]
    pub fn plan(&self) -> Result<Plan, String> {
        let extract = self.section("extract")?;
        let extract = match &extract.from {
            Some(from) => Some(Stage::From(from.clone())),
            None if extract.steps.is_empty() && extract.tag.is_none() => None,
            None => Some(Stage::Make {
                what: scan(&extract)?,
                tag: extract.tag.clone(),
            }),
        };

        let transform = self.section("transform")?;
        let mut chunking = None;
        let mut context = Context::default();
        for step in &transform.steps {
            let made = match step.name.as_str() {
                "chunk.length" => {
                    only(
                        step,
                        &["size", "overlap", "context"],
                        &[("unit", "characters")],
                    )?;
                    context = context_asked(step)?;
                    let size = whole(step, "size", 1, 1_048_576)?;
                    let overlap = whole(step, "overlap", 0, 1_048_576)?;
                    if overlap >= size {
                        return Err("chunk.length needs overlap smaller than size.".into());
                    }
                    Chunking::Length { size, overlap }
                }
                "chunk.structure" => {
                    only(step, &["max_chars", "context"], &[])?;
                    context = context_asked(step)?;
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
        let transform = match (&transform.from, chunking) {
            (Some(from), _) => Some(Stage::From(from.clone())),
            (None, Some(chunking)) => Some(Stage::Make {
                what: Chunks { chunking, context },
                tag: transform.tag.clone(),
            }),
            (None, None) if transform.steps.is_empty() && transform.tag.is_none() => None,
            (None, None) => return Err("transform has no chunking module.".into()),
        };

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
        // An index that is named, not made, is taken to be a keyword index: that is checked
        // when it is opened.
        keyword |= index.from.is_some();

        let search = self.section("search")?;
        if search.tag.is_some() || search.from.is_some() {
            return Err("search makes no snapshot, so it has no tag and no from.".into());
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
        let index = match &index.from {
            Some(from) => Stage::From(from.clone()),
            None => Stage::Make {
                what: (),
                tag: index.tag.clone(),
            },
        };
        if matches!(index, Stage::Make { .. }) && transform.is_none() {
            return Err("index needs transform before it, or a name to start from.".into());
        }
        if matches!(transform, Some(Stage::Make { .. })) && extract.is_none() {
            return Err("transform needs extract before it, or a name to start from.".into());
        }

        let eval = self.section("eval")?;
        if eval.from.is_some() {
            return Err("eval makes its result, so it has no from.".into());
        }
        if eval.tag.is_some() && eval.steps.is_empty() {
            return Err("eval has a tag and no eval.evidence.".into());
        }
        let mut judging = None;
        for step in &eval.steps {
            if step.name != "eval.evidence" {
                return Err(format!("{} is not on the new store yet.", step.name));
            }
            only(step, &["data", "split"], &[("match", "quote")])?;
            let Some(data) = step.args.get("data").and_then(Value::as_str) else {
                return Err("eval.evidence needs data as the name of evaluation data.".into());
            };
            let split = match step.args.get("split") {
                None => "dev",
                Some(Value::String(split)) if split == "dev" || split == "test" => split,
                Some(_) => return Err("eval.evidence takes split as dev or test.".into()),
            };
            let made = Judging {
                data: data.to_owned(),
                split: split.to_owned(),
                tag: eval.tag.clone(),
            };
            if judging.replace(made).is_some() {
                return Err("eval has more than one eval.evidence.".into());
            }
        }

        let release = match self.get("serve") {
            None => None,
            Some(serve) => match serve.get("tag") {
                None => None,
                Some(Value::String(tag)) => Some(tag.clone()),
                Some(_) => return Err("The tag of serve is not a name.".into()),
            },
        };
        Ok(Plan {
            extract,
            transform,
            index,
            top_k: top_k.ok_or("search has no retrieval module.")?,
            eval: judging,
            release,
        })
    }
}

impl Plan {
    /// How `transform` makes the chunks, when it makes them.
    pub fn chunks(&self) -> Option<&Chunks> {
        match &self.transform {
            Some(Stage::Make { what, .. }) => Some(what),
            _ => None,
        }
    }

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

    const CONFIG: &str = "extract:\n  - source.filesystem: { path: ./docs }\n  - loader.text: {}\n  - tag: docs\n\ntransform:\n  - chunk.length: { unit: characters, size: 200, overlap: 20 }\n  - tag: chunks-200\n\nindex:\n  - index.keyword: { tokenizer: lindera }\n  - tag: main\n\nsearch:\n  - retrieval.keyword: { top_k: 10 }\n\neval:\n  - eval.evidence: { data: default }\n\nserve:\n  tag: release\n";

    fn replaced(section: &str, entries: &str) -> Settings {
        let mut settings = Settings::parse(CONFIG).unwrap();
        let given = Settings::parse(&format!("{section}:\n{entries}")).unwrap();
        settings.replace(section, given.get(section).unwrap().clone());
        settings
    }

    #[test]
    fn reads_a_configuration_and_writes_it_again() {
        let settings = Settings::parse(CONFIG).unwrap();
        let plan = settings.plan().unwrap();
        assert_eq!(
            plan.extract,
            Some(Stage::Make {
                what: Scan {
                    source: "docs".into(),
                    path: "./docs".into(),
                    apply: Apply::ALL,
                },
                tag: Some("docs".into()),
            })
        );
        assert_eq!(
            plan.transform,
            Some(Stage::Make {
                what: Chunks {
                    chunking: Chunking::Length {
                        size: 200,
                        overlap: 20
                    },
                    context: Context::default(),
                },
                tag: Some("chunks-200".into()),
            })
        );
        assert_eq!(
            plan.index,
            Stage::Make {
                what: (),
                tag: Some("main".into())
            }
        );
        assert_eq!(plan.top_k, 10);
        assert_eq!(
            plan.eval,
            Some(Judging {
                data: "default".into(),
                split: "dev".into(),
                tag: None,
            })
        );
        assert_eq!(plan.release.as_deref(), Some("release"));

        let written = settings.to_text();
        assert!(written.contains("transform:\n  - chunk.length: {"));
        assert_eq!(Settings::parse(&written).unwrap(), settings);
    }

    #[test]
    fn starts_a_section_from_a_name() {
        // The documents are the ones a name points at: nothing is scanned.
        let plan = replaced("extract", "  - from: docs\n").plan().unwrap();
        assert_eq!(plan.extract, Some(Stage::From("docs".into())));
        assert_eq!(plan.extract.as_ref().unwrap().name(), Some("docs"));
        assert!(plan.chunks().is_some());

        // Only a search: the index is named, and the sections before it are not written.
        let mut settings = replaced("index", "  - from: main\n");
        settings.remove("extract");
        settings.remove("transform");
        let plan = settings.plan().unwrap();
        assert_eq!(
            (plan.extract, plan.transform, plan.index),
            (None, None, Stage::From("main".into()))
        );

        assert_eq!(
            replaced("transform", "  - from: chunks-200\n  - tag: mine\n")
                .plan()
                .unwrap_err(),
            "transform starts from a name, so it has no modules and no tag."
        );
        let mut settings = Settings::parse(CONFIG).unwrap();
        settings.remove("extract");
        assert_eq!(
            settings.plan().unwrap_err(),
            "transform needs extract before it, or a name to start from."
        );
    }

    #[test]
    fn says_why_a_configuration_cannot_run() {
        let reason = |section: &str, entries: &str| replaced(section, entries).plan().unwrap_err();
        assert_eq!(
            reason(
                "extract",
                "  - source.filesystem: { path: ./docs, apply: [add, remove] }\n"
            ),
            "source.filesystem takes apply as a list of add, update and delete."
        );
        let only_new = replaced(
            "extract",
            "  - source.filesystem: { path: ./docs, id: manuals, apply: [add] }\n",
        );
        assert_eq!(
            only_new.plan().unwrap().extract,
            Some(Stage::Make {
                what: Scan {
                    source: "manuals".into(),
                    path: "./docs".into(),
                    apply: Apply {
                        add: true,
                        update: false,
                        delete: false
                    },
                },
                tag: None,
            })
        );
        assert_eq!(
            reason(
                "transform",
                "  - chunk.structure: { max_chars: 200, context: { prepend: true } }\n"
            ),
            "The context of chunk.structure has nothing to prepend. Add document or headings."
        );
        assert_eq!(
            reason(
                "transform",
                "  - chunk.structure: { max_chars: 200, context: { document: true } }\n"
            ),
            "The context of chunk.structure uses document only with prepend."
        );
        let before = Settings::parse(CONFIG).unwrap().plan().unwrap();
        let before = before.chunks().unwrap().process().id();
        let plan = replaced(
            "transform",
            "  - chunk.length: { size: 200, overlap: 20, context: { document: true, headings: true, prepend: true } }\n",
        )
        .plan()
        .unwrap();
        let chunks = plan.chunks().unwrap();
        assert_eq!(
            chunks.context,
            Context {
                document: true,
                headings: true,
                prepend: true
            }
        );
        // The context is part of what the chunking is: another context is another result.
        assert_ne!(chunks.process().id(), before);
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
        assert_eq!(
            reason("eval", "  - eval.evidence: { split: dev }\n"),
            "eval.evidence needs data as the name of evaluation data."
        );
    }
}
