//! Loading source files for the `source` stage (spec 6, 28.9 step 2a).
//!
//! Files are read, hashed and looked up in the extraction cache in `state.sqlite`. Text files are
//! decoded here; other kinds go to a host (the JavaScript parsers for now). Results, cache rows,
//! stored originals and messages follow the JavaScript `loadCorpus` this replaces.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use std::collections::HashMap;

use crate::Cancellation;
use crate::command_parser::{CommandRoute, Slots, command_version};
use crate::config_value::ConfigValue;
use crate::source_files::{ReadFailure, read_source_file, store_original};

const TEXT_LIMIT: u64 = 1024 * 1024;
const CONVERTED_LIMIT: u64 = 16 * TEXT_LIMIT;
const FLUSH_ROWS: usize = 256;
const FLUSH_BYTES: usize = 64 * 1024 * 1024;
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const FILE_CONCURRENCY: usize = 4;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadRequest {
    pub state: String,
    /// The shared originals directory.
    pub originals: String,
    #[serde(default)]
    pub force: bool,
    /// Version of the OCR tool, part of cache keys for OCR files.
    pub ocr_version: String,
    /// `oar-ocr`, read here, or `lightonocr`, read by the host.
    #[serde(default = "lightonocr")]
    pub ocr_tool: String,
    /// The folder `dunlin add oar-ocr` prepared.
    #[serde(default)]
    pub ocr_home: Option<String>,
    /// The runtime `dunlin add lightonocr` prepared, or none when it is not prepared.
    #[serde(default)]
    pub lighton: Option<crate::lighton::Lighton>,
    /// The project root, where external commands run.
    #[serde(default)]
    pub root: String,
    pub files: Vec<LoadFile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadFile {
    pub id: String,
    pub file: String,
    pub object_id: String,
    /// `text`, `html`, `office`, `pdf`, `ocr` or `command`.
    pub kind: String,
    /// The parser module instance, reported in warnings.
    pub route: String,
    /// `{ module, args }` of the `loader.command` instance for `command` files.
    #[serde(default)]
    pub command: Option<ConfigValue>,
}

/// Parses a file that this module does not read itself.
pub trait Host: Sync {
    /// Returns `{ content, format, locations?, parser? }`, or an error message.
    fn parse(&self, file: &LoadFile, bytes: &[u8]) -> Result<ConfigValue, String>;
    /// Reports how many files finished. Must not block.
    fn progress(&self, _completed: usize) {}
}

pub struct Loaded {
    /// One document per file, in input order.
    pub documents: Vec<ConfigValue>,
    pub warnings: Vec<ConfigValue>,
}

fn lightonocr() -> String {
    "lightonocr".into()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut text = String::with_capacity(64);
    for byte in digest {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// `pathToFileURL(path).href` as Node.js 22 computes it.
pub fn file_url(path: &str) -> String {
    fn encode(text: &str) -> String {
        let mut out = String::new();
        for c in text.chars() {
            if c.is_ascii_alphanumeric() || "-._!$&'()*+,;=@:/".contains(c) {
                out.push(c);
            } else {
                let mut buffer = [0; 4];
                for byte in c.encode_utf8(&mut buffer).bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
        }
        out
    }
    if cfg!(windows) {
        let slashes = path.replace('\\', "/");
        if let Some(rest) = slashes.strip_prefix("//") {
            let (host, tail) = rest.split_once('/').unwrap_or((rest, ""));
            return format!("file://{}/{}", host.to_lowercase(), encode(tail));
        }
        return format!("file:///{}", encode(&slashes));
    }
    format!("file://{}", encode(path))
}

/// `JSON.stringify` of a configuration value, as JavaScript writes it.
fn json(value: &ConfigValue) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn string(text: &str) -> ConfigValue {
    ConfigValue::String(text.to_owned())
}

fn extension(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(0) | None => String::new(),
        Some(index) => name[index..].to_lowercase(),
    }
}

fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// `{ ...target, key: value }`: replaces in place, appends, or removes for `undefined`.
fn assign(entries: &mut Vec<(String, ConfigValue)>, key: &str, value: Option<ConfigValue>) {
    let position = entries.iter().position(|(name, _)| name == key);
    match (position, value) {
        (Some(index), Some(value)) => entries[index].1 = value,
        (None, Some(value)) => entries.push((key.to_owned(), value)),
        // `{ key: undefined }` keeps the key, and JSON drops it.
        (Some(index), None) => {
            entries.remove(index);
        }
        (None, None) => {}
    }
}

struct Pending {
    rows: Vec<(String, String)>,
    originals: Vec<(String, Vec<u8>, String)>,
    bytes: usize,
    flushed: Instant,
}

struct Writer {
    connection: Mutex<Connection>,
    originals_dir: String,
    pending: Mutex<Pending>,
}

impl Writer {
    fn queue(
        &self,
        key: String,
        document: String,
        original: Option<(String, Vec<u8>, String)>,
    ) -> Result<(), String> {
        let ready = {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| "Could not lock the extraction cache".to_owned())?;
            if let Some((sha, bytes, target)) = original
                && !pending
                    .originals
                    .iter()
                    .any(|(existing, _, _)| *existing == sha)
            {
                pending.bytes += bytes.len();
                pending.originals.push((sha, bytes, target));
            }
            pending.rows.push((key, document));
            pending.rows.len() >= FLUSH_ROWS
                || pending.bytes >= FLUSH_BYTES
                || pending.flushed.elapsed() >= FLUSH_INTERVAL
        };
        if ready { self.flush() } else { Ok(()) }
    }

    /// Stores originals, then the cache rows that refer to them, in one short transaction.
    fn flush(&self) -> Result<(), String> {
        let (rows, originals) = {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| "Could not lock the extraction cache".to_owned())?;
            pending.bytes = 0;
            pending.flushed = Instant::now();
            (
                std::mem::take(&mut pending.rows),
                std::mem::take(&mut pending.originals),
            )
        };
        if !originals.is_empty() {
            crate::source_files::original_directory(std::path::Path::new(&self.originals_dir))?;
            for (sha, bytes, target) in &originals {
                store_original(std::path::Path::new(&self.originals_dir), sha, bytes)
                    .map_err(|message| format!("Could not load {target}. {message}"))?;
            }
        }
        if rows.is_empty() {
            return Ok(());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "Could not lock the extraction cache".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| error.to_string())?;
        {
            let mut insert = transaction
                .prepare("INSERT OR IGNORE INTO loaded_files VALUES (?, ?)")
                .map_err(|error| error.to_string())?;
            for (key, document) in &rows {
                insert
                    .execute(params![key, document])
                    .map_err(|error| error.to_string())?;
            }
        }
        transaction.commit().map_err(|error| error.to_string())
    }

    fn cached(&self, key: &str) -> Result<Option<String>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "Could not lock the extraction cache".to_owned())?;
        connection
            .query_row(
                "SELECT document FROM loaded_files WHERE id = ?",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())
    }
}

/// Each `loader.command` instance by module name, with its resolved version.
type Commands = HashMap<String, (CommandRoute, Option<String>)>;

/// An error message for the first file that failed, in input order.
pub struct LoadError {
    pub message: String,
    pub aborted: bool,
}

pub fn load(
    request: &LoadRequest,
    host: &dyn Host,
    cancel: &Cancellation,
) -> Result<Loaded, LoadError> {
    let fail = |message: String| LoadError {
        message,
        aborted: false,
    };
    let connection = Connection::open(&request.state).map_err(|error| fail(error.to_string()))?;
    connection
        .busy_timeout(Duration::from_secs(10))
        .map_err(|error| fail(error.to_string()))?;
    let writer = Writer {
        connection: Mutex::new(connection),
        originals_dir: request.originals.clone(),
        pending: Mutex::new(Pending {
            rows: Vec::new(),
            originals: Vec::new(),
            bytes: 0,
            flushed: Instant::now(),
        }),
    };
    // Each loader.command instance resolves its version once, before any file is parsed with it.
    let mut commands: Commands = HashMap::new();
    for file in &request.files {
        let Some(value) = &file.command else { continue };
        let route = CommandRoute::from_value(value)
            .ok_or_else(|| fail("Invalid loader.command settings".into()))?;
        if !commands.contains_key(&route.module) {
            let version = command_version(&route, &request.root, cancel).map_err(fail)?;
            commands.insert(route.module.clone(), (route, version));
        }
    }
    let slots = Slots::default();
    let ocr = request.files.iter().any(|file| file.kind == "ocr");
    let workers = if ocr { 1 } else { FILE_CONCURRENCY }.min(request.files.len().max(1));
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let completed = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<Result<ConfigValue, LoadError>>>> =
        request.files.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while !stop.load(Ordering::SeqCst) {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(file) = request.files.get(index) else {
                        break;
                    };
                    let result = if cancel.check().is_err() {
                        Err(LoadError {
                            message: "Operation aborted".into(),
                            aborted: true,
                        })
                    } else {
                        load_one(request, file, &writer, host, &commands, &slots, cancel).map_err(
                            |message| LoadError {
                                message: if message.starts_with("Could not load ") {
                                    message
                                } else {
                                    format!(
                                        "Could not load {}:{}. {message}",
                                        file.id, file.object_id
                                    )
                                },
                                aborted: false,
                            },
                        )
                    };
                    // A parser that stopped because of cancellation reports the cancellation.
                    let result = match result {
                        Err(error) if cancel.check().is_err() => Err(LoadError {
                            message: error.message,
                            aborted: true,
                        }),
                        other => other,
                    };
                    if result.is_err() {
                        stop.store(true, Ordering::SeqCst);
                    } else {
                        host.progress(completed.fetch_add(1, Ordering::SeqCst) + 1);
                    }
                    if let Ok(mut slot) = results[index].lock() {
                        *slot = Some(result);
                    }
                }
            });
        }
    });
    crate::lighton::stop();
    // Keep the files that finished, so a failed or cancelled build can reuse them.
    let flushed = writer.flush();
    let mut documents = Vec::new();
    let mut warnings = Vec::new();
    for (file, slot) in request.files.iter().zip(results) {
        match slot.into_inner().ok().flatten() {
            Some(Ok(document)) => {
                let empty = document
                    .get("content")
                    .and_then(ConfigValue::as_str)
                    .is_some_and(|content| {
                        content.trim_matches(crate::registry::js_space).is_empty()
                    });
                if empty {
                    warnings.push(ConfigValue::Object(vec![
                        ("code".into(), string("EMPTY_EXTRACTION")),
                        ("section".into(), string("source")),
                        ("module".into(), string(&file.route)),
                        (
                            "target".into(),
                            string(&format!("{}:{}", file.id, file.object_id)),
                        ),
                        (
                            "message".into(),
                            string("No text extracted. Scanned documents require OCR."),
                        ),
                    ]));
                }
                documents.push(document);
            }
            Some(Err(error)) => return Err(error),
            None => break,
        }
    }
    flushed.map_err(fail)?;
    Ok(Loaded {
        documents,
        warnings,
    })
}

fn load_one(
    request: &LoadRequest,
    file: &LoadFile,
    writer: &Writer,
    host: &dyn Host,
    commands: &Commands,
    slots: &Slots,
    cancel: &Cancellation,
) -> Result<ConfigValue, String> {
    let command = match &file.command {
        Some(value) => {
            let module = value
                .get("module")
                .and_then(ConfigValue::as_str)
                .unwrap_or_default();
            Some(
                commands
                    .get(module)
                    .ok_or("Unknown loader.command instance")?,
            )
        }
        None => None,
    };
    let limit = if file.kind == "text" {
        TEXT_LIMIT
    } else {
        CONVERTED_LIMIT
    };
    let read =
        read_source_file(std::path::Path::new(&file.file), limit).map_err(
            |failure| match failure {
                ReadFailure::TooLarge => format!("Source file exceeds {} MiB", limit / TEXT_LIMIT),
                ReadFailure::Changed => "A source file changed while it was being indexed".into(),
                ReadFailure::NotFile => "Expected a local file".into(),
                ReadFailure::Io(message) => message,
            },
        )?;
    let extension = extension(&file.file);
    let hash = sha256_hex(&read.bytes);
    let key = match command {
        Some((route, version)) => json(&ConfigValue::Array(vec![
            string("loader.command-v1"),
            string(&file.id),
            string(&file.object_id),
            string(&hash),
            route.identity(version.as_deref()),
        ])),
        None => {
            let mut parts = vec![
                string("loader.auto-v1"),
                string(&file.id),
                string(&file.object_id),
                string(&extension),
                string(&hash),
            ];
            if file.kind == "ocr" {
                parts.push(string(&request.ocr_version));
            }
            // PDFs read in Rust: switching parsers reads them again (spec 28.9 step 2a).
            if file.kind == "pdf" {
                parts.push(string(&format!(
                    "{}@{}",
                    crate::pdf_text::PARSER,
                    crate::pdf_text::VERSION
                )));
            }
            json(&ConfigValue::Array(parts))
        }
    };
    let document = [
        ("objectId", Some(string(&file.object_id))),
        ("url", Some(string(&file_url(&file.file)))),
        (
            "modifiedAtMs",
            read.modified_at_ms
                .map(|value| ConfigValue::Number(value as f64)),
        ),
    ];
    if !request.force
        && let Some(row) = writer.cached(&key)?
    {
        let ConfigValue::Object(mut entries) =
            serde_json::from_str::<ConfigValue>(&row).map_err(|error| error.to_string())?
        else {
            return Err("Invalid extraction cache row".into());
        };
        for (name, value) in document {
            assign(&mut entries, name, value);
        }
        return Ok(ConfigValue::Object(entries));
    }
    let parsed = if file.kind == "text" {
        let content = std::str::from_utf8(&read.bytes)
            .map_err(|_| "The encoded data was not valid for encoding utf-8".to_owned())?;
        let format = if extension == ".md" || extension == ".markdown" {
            "markdown"
        } else {
            "text"
        };
        ConfigValue::Object(vec![
            ("content".into(), string(content)),
            ("format".into(), string(format)),
        ])
    } else if let Some((route, version)) = command {
        let name = base_name(&file.file);
        crate::command_parser::parse(
            route,
            &read.bytes,
            name,
            &request.root,
            version.as_deref(),
            slots,
            cancel,
        )?
    } else if file.kind == "pdf" {
        crate::pdf_text::parse(&read.bytes, &|| cancel.check().is_ok())?
    } else if file.kind == "ocr" && request.ocr_tool == "oar-ocr" {
        let home = request
            .ocr_home
            .as_deref()
            .ok_or("oar-ocr is not prepared. Run dunlin add ocr.")?;
        let version = request
            .ocr_version
            .strip_prefix("oar-ocr@")
            .unwrap_or(&request.ocr_version);
        crate::ocr::parse(home, version, &read.bytes, &|| cancel.check().is_ok())?
    } else if file.kind == "ocr" {
        crate::lighton::parse(request.lighton.as_ref(), &read.bytes, &|| {
            cancel.check().is_ok()
        })?
    } else {
        host.parse(file, &read.bytes)?
    };
    let content = parsed
        .get("content")
        .and_then(ConfigValue::as_str)
        .ok_or("Parser returned no text")?;
    if content.len() > TEXT_LIMIT as usize {
        return Err("Extracted text exceeds 1 MiB".into());
    }
    let mut corpus = Vec::new();
    for name in ["locations", "parser"] {
        if let Some(value) = parsed
            .get(name)
            .filter(|value| **value != ConfigValue::Null)
        {
            corpus.push((name.to_owned(), value.clone()));
        }
    }
    corpus.push(("originalSha256".into(), string(&hash)));
    let mut entries: Vec<(String, ConfigValue)> = document
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| (name.to_owned(), value)))
        .collect();
    entries.push(("content".into(), string(content)));
    entries.push((
        "format".into(),
        parsed.get("format").cloned().unwrap_or(ConfigValue::Null),
    ));
    entries.push(("corpus".into(), ConfigValue::Object(corpus)));
    let input = ConfigValue::Object(entries);
    writer.queue(
        key,
        json(&input),
        Some((hash, read.bytes, format!("{}:{}", file.id, file.object_id))),
    )?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Html;

    impl Host for Html {
        fn parse(&self, file: &LoadFile, bytes: &[u8]) -> Result<ConfigValue, String> {
            if file.kind == "html" {
                Ok(ConfigValue::Object(vec![
                    ("content".into(), string(&String::from_utf8_lossy(bytes))),
                    ("format".into(), string("markdown")),
                    (
                        "parser".into(),
                        ConfigValue::Object(vec![("tool".into(), string("test"))]),
                    ),
                ]))
            } else {
                Err("unsupported".into())
            }
        }
    }

    #[test]
    fn loads_caches_and_reports_failures() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state.sqlite");
        Connection::open(&state)
            .unwrap()
            .execute_batch(
                "CREATE TABLE loaded_files (id TEXT PRIMARY KEY, document TEXT NOT NULL)",
            )
            .unwrap();
        std::fs::write(
            root.path().join("a.md"),
            "# A
",
        )
        .unwrap();
        std::fs::write(root.path().join("b.html"), "<p>b</p>").unwrap();
        std::fs::write(root.path().join("bad.txt"), [0xff_u8]).unwrap();
        let file = |name: &str, kind: &str| LoadFile {
            id: "docs".into(),
            file: root.path().join(name).to_string_lossy().into_owned(),
            object_id: name.into(),
            kind: kind.into(),
            route: "loader.auto".into(),
            command: None,
        };
        let request = |files| LoadRequest {
            state: state.to_string_lossy().into_owned(),
            originals: root.path().join("originals").to_string_lossy().into_owned(),
            force: false,
            ocr_version: "v1".into(),
            ocr_tool: "lightonocr".into(),
            ocr_home: None,
            lighton: None,
            root: root.path().to_string_lossy().into_owned(),
            files,
        };
        let cancel = Cancellation::default();
        let loaded = load(
            &request(vec![file("a.md", "text"), file("b.html", "html")]),
            &Html,
            &cancel,
        )
        .unwrap_or_else(|error| panic!("{}", error.message));
        assert_eq!(loaded.documents.len(), 2);
        assert_eq!(loaded.documents[0].get("format"), Some(&string("markdown")));
        assert!(
            loaded.documents[1]
                .get("corpus")
                .and_then(|corpus| corpus.get("parser"))
                .is_some()
        );
        assert!(loaded.warnings.is_empty());
        let rows: i64 = Connection::open(&state)
            .unwrap()
            .query_row("SELECT count(*) FROM loaded_files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 2);
        assert_eq!(
            std::fs::read_dir(root.path().join("originals"))
                .unwrap()
                .count(),
            2
        );
        let again = load(&request(vec![file("a.md", "text")]), &Html, &cancel)
            .unwrap_or_else(|error| panic!("{}", error.message));
        assert_eq!(again.documents[0], loaded.documents[0]);
        let error = load(&request(vec![file("bad.txt", "text")]), &Html, &cancel)
            .err()
            .unwrap();
        assert_eq!(
            error.message,
            "Could not load docs:bad.txt. The encoded data was not valid for encoding utf-8"
        );
        assert!(!error.aborted);
        let aborted = Cancellation::default();
        aborted.abort();
        let error = load(&request(vec![file("a.md", "text")]), &Html, &aborted)
            .err()
            .unwrap();
        assert!(error.aborted);
        let large = "x".repeat(TEXT_LIMIT as usize + 1);
        std::fs::write(root.path().join("big.md"), large).unwrap();
        let error = load(&request(vec![file("big.md", "text")]), &Html, &cancel)
            .err()
            .unwrap();
        assert!(error.message.ends_with("Source file exceeds 1 MiB"));
    }

    #[test]
    fn hashes_and_builds_file_urls() {
        assert_eq!(
            sha256_hex(b"same"),
            "0967115f2813a3541eaef77de9d9d5773f1c0c04314b0bbfe4ff3b3b1c55b5d5"
        );
        if cfg!(windows) {
            for [path, expected] in [
                ["C:\\a b\\c#d?e%f.md", "file:///C:/a%20b/c%23d%3Fe%25f.md"],
                [
                    "C:\\日本\\x[1]^|{}`.md",
                    "file:///C:/%E6%97%A5%E6%9C%AC/x%5B1%5D%5E%7C%7B%7D%60.md",
                ],
                [
                    "C:\\a\\semi;colon,comma=eq@at+plus&amp'q\".md",
                    "file:///C:/a/semi;colon,comma=eq@at+plus&amp'q%22.md",
                ],
                ["\\\\server\\share\\f.md", "file://server/share/f.md"],
                ["C:\\a\\tab\there.md", "file:///C:/a/tab%09here.md"],
                ["C:\\a\\~tilde!$()*.md", "file:///C:/a/%7Etilde!$()*.md"],
                ["C:\\a\\<lt>.md", "file:///C:/a/%3Clt%3E.md"],
                ["C:\\a\\emoji👍.md", "file:///C:/a/emoji%F0%9F%91%8D.md"],
            ] {
                assert_eq!(file_url(path), expected, "{path}");
            }
        }
    }
}
