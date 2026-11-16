//! The `source` stage: routing, loading, identifiers and the stage record (spec 7, 28.9 step 2a).
//!
//! Callers pass scanned and ordered sources. Identifiers are digests of the same JSON the
//! JavaScript implementation hashed, so existing projects keep their stage and build IDs.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Deserialize;

use crate::Cancellation;
use crate::config_value::ConfigValue;
use crate::source_load::{Host, LoadError, LoadFile, LoadRequest, load, sha256_hex};
use crate::source_scan::{Route, RouteFile, RouteRequest, route};

#[derive(Debug, Deserialize)]
pub struct StageSource {
    pub id: String,
    pub files: Vec<RouteFile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRequest {
    pub state: String,
    pub originals: String,
    pub root: String,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub skip_unsupported: bool,
    /// A parser module sets `unsupported: error`.
    #[serde(default)]
    pub reject: bool,
    pub ocr_version: String,
    #[serde(default = "lightonocr")]
    pub ocr_tool: String,
    #[serde(default)]
    pub ocr_home: Option<String>,
    #[serde(default)]
    pub lighton: Option<crate::lighton::Lighton>,
    pub sources: Vec<StageSource>,
    pub routes: Vec<Route>,
    /// The `loader.command` instances by name, as `{ module, args }`.
    #[serde(default)]
    pub commands: Vec<ConfigValue>,
    /// `moduleSettings(plan)` for the source stage.
    pub modules: ConfigValue,
}

pub enum SourceOutcome {
    Done {
        output: ConfigValue,
        reused: bool,
        warnings: Vec<ConfigValue>,
    },
    /// Unsupported files stopped the stage: `{ files, skipped }`.
    Unsupported(ConfigValue),
}

fn lightonocr() -> String {
    "lightonocr".into()
}

fn text(value: &str) -> ConfigValue {
    ConfigValue::String(value.to_owned())
}

/// `kind + '-' + sha256(JSON.stringify(value))`
fn digest(kind: &str, value: &ConfigValue) -> String {
    format!("{kind}-{}", sha256_hex(value.to_js_json().as_bytes()))
}

pub fn run(
    request: &SourceRequest,
    host: &dyn Host,
    cancel: &Cancellation,
) -> Result<SourceOutcome, LoadError> {
    let fail = |message: String| LoadError {
        message,
        aborted: false,
    };
    let files: Vec<(&str, &RouteFile)> = request
        .sources
        .iter()
        .flat_map(|source| {
            source
                .files
                .iter()
                .map(move |file| (source.id.as_str(), file))
        })
        .collect();
    let routed = route(&RouteRequest {
        routes: request.routes.clone(),
        files: files.iter().map(|(_, file)| (*file).clone()).collect(),
    });
    let skipped: Vec<ConfigValue> = files
        .iter()
        .zip(&routed)
        .filter(|(_, routed)| routed.route.is_none())
        .map(|((id, file), _)| {
            ConfigValue::Object(vec![
                ("source_id".into(), text(id)),
                ("path".into(), text(&file.object_id)),
                (
                    "reason".into(),
                    text("No configured loader for this file format"),
                ),
            ])
        })
        .collect();
    let total = ConfigValue::Number(files.len() as f64);
    if !skipped.is_empty() && request.reject && !request.skip_unsupported {
        return Ok(SourceOutcome::Unsupported(ConfigValue::Object(vec![
            ("files".into(), total),
            ("skipped".into(), ConfigValue::Array(skipped)),
        ])));
    }
    let command = |name: &str| {
        request
            .commands
            .iter()
            .find(|value| value.get("module").and_then(ConfigValue::as_str) == Some(name))
            .cloned()
    };
    let mut owners = Vec::new();
    let mut load_files = Vec::new();
    for ((id, file), routed) in files.iter().zip(&routed) {
        let (Some(route_name), Some(kind)) = (&routed.route, routed.kind) else {
            continue;
        };
        owners.push(*id);
        load_files.push(LoadFile {
            id: (*id).to_owned(),
            file: file.file.clone(),
            object_id: file.object_id.clone(),
            kind: kind.to_owned(),
            route: route_name.clone(),
            command: if kind == "command" {
                command(route_name)
            } else {
                None
            },
        });
    }
    let loaded = load(
        &LoadRequest {
            state: request.state.clone(),
            originals: request.originals.clone(),
            force: request.force,
            ocr_version: request.ocr_version.clone(),
            ocr_tool: request.ocr_tool.clone(),
            ocr_home: request.ocr_home.clone(),
            lighton: request.lighton.clone(),
            root: request.root.clone(),
            files: load_files,
        },
        host,
        cancel,
    )?;
    let mut groups: Vec<(String, Vec<ConfigValue>)> = request
        .sources
        .iter()
        .map(|source| (source.id.clone(), Vec::new()))
        .collect();
    for (owner, document) in owners.into_iter().zip(loaded.documents) {
        if let Some(group) = groups.iter_mut().find(|(id, _)| id == owner) {
            group.1.push(document);
        }
    }
    let groups = ConfigValue::Array(
        groups
            .into_iter()
            .map(|(id, documents)| {
                ConfigValue::Array(vec![text(&id), ConfigValue::Array(documents)])
            })
            .collect(),
    );
    let skipped = ConfigValue::Array(skipped);
    let config_id = digest("cfg", &request.modules);
    let id = digest(
        "src",
        &ConfigValue::Array(vec![text(&config_id), groups.clone(), skipped.clone()]),
    );
    let output = ConfigValue::Object(vec![
        ("id".into(), text(&id)),
        ("config_id".into(), text(&config_id)),
        ("modules".into(), request.modules.clone()),
        ("files".into(), total),
        ("skipped".into(), skipped),
        ("groups".into(), groups),
    ]);
    if cancel.check().is_err() {
        return Err(LoadError {
            message: "Aborted".into(),
            aborted: true,
        });
    }
    let reused = save(&request.state, &output, request.force).map_err(fail)?;
    Ok(SourceOutcome::Done {
        output,
        reused,
        warnings: loaded.warnings,
    })
}

/// Records the stage: payloads by content, the manifest, the latest stage and the check time.
fn save(state: &str, output: &ConfigValue, force: bool) -> Result<bool, String> {
    let sql = |error: rusqlite::Error| error.to_string();
    let mut connection = Connection::open(state).map_err(sql)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(10))
        .map_err(sql)?;
    let id = output
        .get("id")
        .and_then(ConfigValue::as_str)
        .unwrap_or_default()
        .to_owned();
    let reused = !force
        && connection
            .query_row(
                "SELECT 1 FROM stage_outputs WHERE stage = ? AND id = ?",
                params!["source", id],
                |_| Ok(()),
            )
            .optional()
            .map_err(sql)?
            .is_some();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    if !reused {
        let mut manifest = match output {
            ConfigValue::Object(entries) => entries.clone(),
            _ => Vec::new(),
        };
        manifest.push(("payloads".into(), ConfigValue::Number(1.0)));
        let mut insert = transaction
            .prepare("INSERT OR IGNORE INTO stage_payloads VALUES (?, ?)")
            .map_err(sql)?;
        let mut put = |content: &ConfigValue| -> Result<String, String> {
            let json = content.to_js_json();
            let key = sha256_hex(json.as_bytes());
            insert.execute(params![key, json]).map_err(sql)?;
            Ok(key)
        };
        let mut stored_groups = Vec::new();
        if let Some(ConfigValue::Array(groups)) = output.get("groups") {
            for group in groups {
                let ConfigValue::Array(pair) = group else {
                    continue;
                };
                let documents = match pair.get(1) {
                    Some(ConfigValue::Array(documents)) => documents,
                    _ => continue,
                };
                let mut stored = Vec::new();
                for document in documents {
                    let ConfigValue::Object(entries) = document else {
                        continue;
                    };
                    let mut entries: Vec<(String, ConfigValue)> = entries.clone();
                    let content = entries
                        .iter()
                        .position(|(key, _)| key == "content")
                        .map(|index| entries.remove(index).1)
                        .unwrap_or(ConfigValue::Null);
                    entries.push(("text".into(), text(&put(&content)?)));
                    stored.push(ConfigValue::Object(entries));
                }
                stored_groups.push(ConfigValue::Array(vec![
                    pair[0].clone(),
                    ConfigValue::Array(stored),
                ]));
            }
        }
        drop(insert);
        if let Some(entry) = manifest.iter_mut().find(|(key, _)| key == "groups") {
            entry.1 = ConfigValue::Array(stored_groups);
        }
        let manifest = ConfigValue::Object(manifest).to_js_json();
        transaction
            .execute(
                "INSERT OR IGNORE INTO stage_outputs VALUES (?, ?, ?)",
                params!["source", id, manifest],
            )
            .map_err(sql)?;
    }
    transaction
        .execute(
            "INSERT INTO latest_stage VALUES (?, ?) ON CONFLICT(stage) DO UPDATE SET id = excluded.id",
            params!["source", id],
        )
        .map_err(sql)?;
    transaction
        .execute(
            "INSERT INTO source_checks (source_id, checked_at) VALUES (?, ?)",
            params![id, iso_now()],
        )
        .map_err(sql)?;
    transaction.commit().map_err(sql)?;
    Ok(reused)
}

/// `new Date().toISOString()`
pub(crate) fn iso_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let millis = now.as_millis() as i64;
    let days = millis.div_euclid(86_400_000);
    let in_day = millis.rem_euclid(86_400_000);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        in_day / 3_600_000,
        in_day / 60_000 % 60,
        in_day / 1000 % 60,
        in_day % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoHost;

    impl Host for NoHost {
        fn parse(&self, _file: &LoadFile, _bytes: &[u8]) -> Result<ConfigValue, String> {
            Err("unsupported".into())
        }
    }

    #[test]
    fn records_the_stage_and_reuses_it() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state.sqlite");
        Connection::open(&state)
            .unwrap()
            .execute_batch(
                "CREATE TABLE stage_outputs (stage TEXT NOT NULL, id TEXT NOT NULL, manifest TEXT NOT NULL, PRIMARY KEY (stage, id));
                 CREATE TABLE latest_stage (stage TEXT PRIMARY KEY, id TEXT NOT NULL);
                 CREATE TABLE source_checks (id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, checked_at TEXT NOT NULL);
                 CREATE TABLE loaded_files (id TEXT PRIMARY KEY, document TEXT NOT NULL);
                 CREATE TABLE stage_payloads (id TEXT PRIMARY KEY, data TEXT NOT NULL);",
            )
            .unwrap();
        std::fs::write(root.path().join("a.md"), "# A").unwrap();
        std::fs::write(root.path().join("b.bin"), "x").unwrap();
        let file = |name: &str| RouteFile {
            file: root.path().join(name).to_string_lossy().into_owned(),
            object_id: name.into(),
        };
        let request = |reject: bool, skip_unsupported: bool| SourceRequest {
            state: state.to_string_lossy().into_owned(),
            originals: root.path().join("originals").to_string_lossy().into_owned(),
            root: root.path().to_string_lossy().into_owned(),
            force: false,
            skip_unsupported,
            reject,
            ocr_version: "1".into(),
            ocr_tool: "lightonocr".into(),
            ocr_home: None,
            lighton: None,
            sources: vec![StageSource {
                id: "docs".into(),
                files: vec![file("a.md"), file("b.bin")],
            }],
            routes: vec![Route {
                name: "loader.auto".into(),
                category: "auto".into(),
                patterns: None,
                ocr: false,
            }],
            commands: Vec::new(),
            modules: ConfigValue::Array(vec![]),
        };
        let cancel = Cancellation::default();
        let Ok(SourceOutcome::Unsupported(stats)) = run(&request(true, false), &NoHost, &cancel)
        else {
            panic!("expected unsupported")
        };
        assert_eq!(stats.get("files"), Some(&ConfigValue::Number(2.0)));
        let Ok(SourceOutcome::Done {
            output,
            reused,
            warnings,
        }) = run(&request(true, true), &NoHost, &cancel)
        else {
            panic!("expected a stage")
        };
        assert!(!reused);
        assert!(warnings.is_empty());
        let id = output
            .get("id")
            .and_then(ConfigValue::as_str)
            .unwrap()
            .to_owned();
        assert!(id.starts_with("src-"));
        let Ok(SourceOutcome::Done {
            output: again,
            reused,
            ..
        }) = run(&request(false, false), &NoHost, &cancel)
        else {
            panic!("expected a stage")
        };
        assert!(reused);
        assert_eq!(again, output);
        let connection = Connection::open(&state).unwrap();
        let manifest: String = connection
            .query_row(
                "SELECT manifest FROM stage_outputs WHERE stage = 'source' AND id = ?",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(manifest.ends_with(",\"payloads\":1}"), "{manifest}");
        assert!(!manifest.contains("\"content\""));
        let checks: i64 = connection
            .query_row("SELECT count(*) FROM source_checks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(checks, 2);
        let latest: String = connection
            .query_row(
                "SELECT id FROM latest_stage WHERE stage = 'source'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(latest, id);
    }

    #[test]
    fn formats_iso_time() {
        let now = iso_now();
        assert_eq!(now.len(), 24);
        assert!(now.ends_with('Z'));
        assert_eq!(
            digest("cfg", &ConfigValue::Array(vec![])),
            format!("cfg-{}", sha256_hex(b"[]"))
        );
    }
}
