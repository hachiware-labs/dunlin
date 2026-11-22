//! The build commands (spec 28.9 step 2d): `build`, `source`, `transform` and `index` with
//! `--from`, `--until`, `--input`, `--tag`, `--config` and `--dry-run`, and the `tag` command.
//!
//! JavaScript resolves settings from the configuration, holds the execution lock and counts trial
//! runs. Everything else runs here, including scanning the source folders (step 2e), with the
//! results and errors of the JavaScript implementation.

use std::io::Read;
use std::time::Instant;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::Deserialize;

use crate::Cancellation;
use crate::config_value::ConfigValue;
use crate::embed::EmbedderSettings;
use crate::index_stage::{
    IndexHost, IndexRequest, assign, digest, field, items, js_json, object, sql, string_field, text,
};
use crate::source_load::{Host, sha256_hex};
use crate::source_scan::{Route, RouteFile, RouteRequest, ScanRequest, route, scan};
use crate::source_stage::{SourceOutcome, SourceRequest, StageSource};
use crate::transform_stage::{StageError, TransformRequest};

pub const STAGES: [&str; 3] = ["source", "transform", "index"];

/// What the build asks of JavaScript: parsers, trial charges, the configuration file and tools.
pub trait BuildHost: Host + IndexHost {}

impl<T: Host + IndexHost> BuildHost for T {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildRequest {
    /// `build`, `source`, `transform` or `index`.
    pub command: String,
    /// The run ID. Dry runs have none.
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    /// `--input`: a build ID or tag whose saved stages feed `--from`.
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    /// `--config`: the settings come from a candidate file.
    #[serde(default)]
    pub candidate: bool,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub skip_unsupported: bool,
    #[serde(default)]
    pub dry_run: bool,
    /// Milliseconds the run spent before this call (scanning), counted in the first stage.
    #[serde(default)]
    pub elapsed_ms: f64,
    pub state: String,
    pub artifacts: String,
    pub corpus: String,
    pub root: String,
    pub source: SourceSettings,
    pub transform: TransformSettings,
    pub index: IndexSettings,
    /// The `index` section of the configuration, which a dry run compares with the serving build.
    #[serde(default)]
    pub config_index: Option<ConfigValue>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSettings {
    /// The folders of the `source.filesystem` instances, in order. They are scanned when the source
    /// stage runs or a dry run starts there.
    #[serde(default)]
    pub scans: Vec<ScanRequest>,
    #[serde(default)]
    pub routes: Vec<Route>,
    #[serde(default)]
    pub reject: bool,
    /// The `loader.command` instances as `{ module, args }`.
    #[serde(default)]
    pub commands: Vec<ConfigValue>,
    /// `moduleSettings(plan.source)`.
    pub modules: ConfigValue,
    #[serde(default)]
    pub ocr_version: Option<String>,
    #[serde(default)]
    pub ocr_tool: Option<String>,
    #[serde(default)]
    pub ocr_home: Option<String>,
    #[serde(default)]
    pub lighton: Option<crate::lighton::Lighton>,
}

#[derive(Debug, Deserialize)]
pub struct TransformSettings {
    /// `moduleSettings(plan.transform)`.
    pub modules: ConfigValue,
    #[serde(default)]
    pub embedder: Option<EmbedderSettings>,
}

#[derive(Debug, Deserialize)]
pub struct IndexSettings {
    #[serde(default)]
    pub keyword: Option<ConfigValue>,
    #[serde(default)]
    pub vector: Option<ConfigValue>,
    #[serde(default)]
    pub embedder: Option<EmbedderSettings>,
    /// Dry runs need only `construction`.
    #[serde(default)]
    pub search: ConfigValue,
    #[serde(default)]
    pub rerank: ConfigValue,
    /// `moduleSettings(plan.index)`.
    pub construction: ConfigValue,
    #[serde(default)]
    pub configuration: ConfigValue,
}

pub struct BuildOutcome {
    pub output: ConfigValue,
    /// `{ code, message }` objects.
    pub warnings: Vec<ConfigValue>,
}

fn stage_required(message: String) -> StageError {
    StageError::coded("STAGE_REQUIRED", message, 3)
}

fn build_required() -> StageError {
    StageError::coded(
        "BUILD_REQUIRED",
        "No searchable build. Run nest build first.",
        3,
    )
}

fn parse(json: &str, what: &str) -> Result<ConfigValue, StageError> {
    serde_json::from_str(json).map_err(|_| StageError::plain(format!("{what} is not valid JSON.")))
}

/// `Math.round(performance.now() - start)`, with `before` milliseconds spent earlier.
fn elapsed(start: Instant, before: f64) -> ConfigValue {
    ConfigValue::Number((before + start.elapsed().as_secs_f64() * 1000.0).round())
}

/// The stages a command runs, in order.
fn selected(request: &BuildRequest) -> Result<Vec<&'static str>, StageError> {
    let position = |stage: &Option<String>| {
        stage
            .as_deref()
            .map(|stage| STAGES.iter().position(|name| *name == stage))
    };
    if request.command != "build" {
        return STAGES
            .iter()
            .find(|stage| **stage == request.command)
            .map(|stage| vec![*stage])
            .ok_or_else(|| {
                StageError::plain(format!("Unknown build command {}.", request.command))
            });
    }
    let start = match position(&request.from) {
        Some(Some(index)) => index,
        None => 0,
        Some(None) => return Err(invalid_stage()),
    };
    let end = match position(&request.until) {
        Some(Some(index)) => index + 1,
        None => STAGES.len(),
        Some(None) => return Err(invalid_stage()),
    };
    Ok(STAGES[start..end.max(start)].to_vec())
}

fn invalid_stage() -> StageError {
    StageError::coded(
        "INVALID_ARGUMENT",
        "Expected source, transform, or index in execution order for --from and --until.",
        2,
    )
}

/// The build a reference names: a build ID, or a tag (`currentBuild(paths, required, reference)`).
fn named_build(
    connection: &Connection,
    reference: &str,
) -> Result<Option<ConfigValue>, StageError> {
    let manifest: Option<String> = connection
        .query_row(
            "SELECT manifest FROM builds WHERE id = ? OR id = (SELECT build_id FROM build_tags WHERE name = ?)
             ORDER BY id = ? DESC LIMIT 1",
            params![reference, reference, reference],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    manifest
        .map(|json| parse(&json, "A saved build"))
        .transpose()
}

/// The serving build.
fn serving_build(connection: &Connection) -> Result<Option<ConfigValue>, StageError> {
    let manifest: Option<String> = connection
        .query_row(
            "SELECT manifest FROM project JOIN builds ON builds.id = project.build_id",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    manifest
        .map(|json| parse(&json, "A saved build"))
        .transpose()
}

/// `resolveBuildInput`: the build whose saved stages feed `--from`.
fn build_input(
    connection: &Connection,
    request: &BuildRequest,
) -> Result<Option<ConfigValue>, StageError> {
    if let Some(input) = &request.input {
        return named_build(connection, input)?
            .map(Some)
            .ok_or_else(build_required);
    }
    match (&request.from, &request.tag) {
        (Some(_), Some(tag)) => named_build(connection, tag),
        _ => Ok(None),
    }
}

fn latest_stage_id(connection: &Connection, stage: &str) -> Result<Option<String>, StageError> {
    connection
        .query_row(
            "SELECT id FROM latest_stage WHERE stage = ?",
            [stage],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)
}

fn stage_manifest(
    connection: &Connection,
    stage: &str,
    id: &str,
) -> Result<Option<ConfigValue>, StageError> {
    let manifest: Option<String> = connection
        .query_row(
            "SELECT manifest FROM stage_outputs WHERE stage = ? AND id = ?",
            [stage, id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    manifest
        .map(|json| parse(&json, "A saved stage"))
        .transpose()
}

/// `inputStageId`: the saved stage a later stage reads, from the input build or the latest run.
fn input_stage_id(
    connection: &Connection,
    stage: &str,
    fixed: Option<&ConfigValue>,
) -> Result<String, StageError> {
    let id = match fixed {
        Some(build) => string_field(build, &format!("{stage}_id")).map(str::to_owned),
        None => latest_stage_id(connection, stage)?,
    };
    if let Some(id) = id
        && stage_manifest(connection, stage, &id)?.is_some()
    {
        return Ok(id);
    }
    Err(match fixed {
        Some(build) => stage_required(format!(
            "Missing saved {stage} for {}.",
            string_field(build, "build_id").unwrap_or("undefined")
        )),
        None => stage_required(format!(
            "No {stage} output. Run nest {stage} or nest build first."
        )),
    })
}

fn open(state: &str, read_only: bool) -> Result<Connection, StageError> {
    let connection = if read_only {
        Connection::open_with_flags(
            state,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    } else {
        Connection::open(state)
    }
    .map_err(sql)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(10))
        .map_err(sql)?;
    Ok(connection)
}

fn stage_modules<'a>(request: &'a BuildRequest, stage: &str) -> &'a ConfigValue {
    match stage {
        "source" => &request.source.modules,
        "transform" => &request.transform.modules,
        _ => &request.index.construction,
    }
}

/// Scans the source folders in order. Files come back ordered by object ID.
fn scan_sources(
    request: &BuildRequest,
    cancel: &Cancellation,
) -> Result<Vec<StageSource>, StageError> {
    request
        .source
        .scans
        .iter()
        .map(|folder| {
            if !std::path::Path::new(&folder.path).exists() {
                return Err(StageError::coded(
                    "SOURCE_MISSING",
                    format!(
                        "Source folder not found: {}. Create it, or set the path of {} in KnowledgeNestfile.yaml.",
                        folder.path, folder.name
                    ),
                    3,
                ));
            }
            let scanned = scan(
                &ScanRequest {
                    path: folder.path.clone(),
                    exclude: folder.exclude.clone(),
                    name: folder.name.clone(),
                    project_root: folder
                        .project_root
                        .clone()
                        .or_else(|| Some(request.root.clone())),
                },
                cancel,
            )
            .map_err(|message| {
                if cancel.check().is_err() {
                    StageError::aborted()
                } else {
                    StageError::plain(message)
                }
            })?;
            Ok(StageSource {
                id: scanned.id,
                files: scanned
                    .files
                    .into_iter()
                    .map(|file| RouteFile {
                        file: file.file,
                        object_id: file.object_id,
                    })
                    .collect(),
            })
        })
        .collect()
}

pub fn run(
    request: BuildRequest,
    host: &dyn BuildHost,
    cancel: &Cancellation,
) -> Result<BuildOutcome, StageError> {
    let selected = selected(&request)?;
    if request.dry_run {
        return dry_run(&request, &selected, cancel).map(|output| BuildOutcome {
            output,
            warnings: Vec::new(),
        });
    }
    let start = Instant::now();
    // A project built before step 2e moves its stage data to the corpus once (spec 28.7).
    let migrated = crate::stage_store::open(&request.state, &request.corpus, cancel)?.migrated;
    let connection = open(&request.state, false)?;
    let generation: Option<i64> = connection
        .query_row("SELECT max(id) FROM adoption_history", [], |row| row.get(0))
        .map_err(sql)?;
    let fixed = build_input(&connection, &request)?;
    let run_id = request
        .run_id
        .clone()
        .ok_or_else(|| StageError::plain("A build needs a run ID."))?;
    let mut warnings = Vec::new();
    if migrated {
        warnings.push(object(vec![
            ("code", text("DATA_LAYOUT_MIGRATED")),
            (
                "message",
                text("Moved extraction results and chunks into the corpus. Build IDs change once, so earlier evaluation reports cannot be compared with new builds."),
            ),
        ]));
    }
    let mut results = Vec::new();
    let mut source: Option<ConfigValue> = None;
    let mut request = request;
    let mut before = request.elapsed_ms;
    let total = request.elapsed_ms;
    for stage in selected.iter().copied() {
        cancel.check().map_err(StageError::native)?;
        let stage_start = Instant::now();
        let (output, reused) = match stage {
            "index" => {
                let transform_id = input_stage_id(
                    &connection,
                    "transform",
                    if selected.contains(&"transform") {
                        None
                    } else {
                        fixed.as_ref()
                    },
                )?;
                let transformed = stage_manifest(&connection, "transform", &transform_id)?
                    .unwrap_or(ConfigValue::Null);
                if !selected.contains(&"transform")
                    && latest_stage_id(&connection, "source")?.as_deref()
                        != string_field(&transformed, "source_id")
                {
                    warnings.push(object(vec![
                        ("code", text("STALE_STAGE")),
                        (
                            "message",
                            text("Using saved transform output from an earlier source snapshot."),
                        ),
                    ]));
                }
                let tag = request.tag.clone().filter(|tag| !tag.is_empty());
                let activate = request.command == "build"
                    && request.until.is_none()
                    && !request.candidate
                    && tag.is_none();
                let index = crate::index_stage::run(
                    &IndexRequest {
                        state: request.state.clone(),
                        artifacts: request.artifacts.clone(),
                        corpus: request.corpus.clone(),
                        root: request.root.clone(),
                        run_id: run_id.clone(),
                        transform_id,
                        keyword: request.index.keyword.clone(),
                        vector: request.index.vector.clone(),
                        embedder: request.index.embedder.clone(),
                        search: request.index.search.clone(),
                        rerank: request.index.rerank.clone(),
                        construction: request.index.construction.clone(),
                        configuration: request.index.configuration.clone(),
                        force: request.force,
                        activate,
                        tag,
                        generation,
                    },
                    host,
                    cancel,
                )?;
                warnings.extend(index.warnings);
                let ConfigValue::Object(mut summary) = index.summary else {
                    return Err(StageError::plain("Invalid build summary."));
                };
                results.push((
                    "index".to_owned(),
                    object(vec![
                        (
                            "id",
                            field(&ConfigValue::Object(summary.clone()), "build_id")
                                .cloned()
                                .unwrap_or(ConfigValue::Null),
                        ),
                        (
                            "reused",
                            summary
                                .iter()
                                .find(|(name, _)| name == "reused")
                                .map_or(ConfigValue::Null, |(_, value)| value.clone()),
                        ),
                        (
                            "elapsed_ms",
                            elapsed(stage_start, std::mem::take(&mut before)),
                        ),
                    ]),
                ));
                assign(&mut summary, "stages", Some(ConfigValue::Object(results)));
                assign(&mut summary, "elapsed_ms", Some(elapsed(start, total)));
                return Ok(BuildOutcome {
                    output: ConfigValue::Object(summary),
                    warnings,
                });
            }
            "source" => {
                let sources = scan_sources(&request, cancel)?;
                let settings = &mut request.source;
                let outcome = crate::source_stage::run(
                    &SourceRequest {
                        state: request.state.clone(),
                        corpus: request.corpus.clone(),
                        originals: std::path::Path::new(&request.artifacts)
                            .join("originals")
                            .join("shared")
                            .to_string_lossy()
                            .into_owned(),
                        root: request.root.clone(),
                        force: request.force,
                        skip_unsupported: request.skip_unsupported,
                        reject: settings.reject,
                        ocr_version: settings.ocr_version.clone().unwrap_or_default(),
                        ocr_tool: settings
                            .ocr_tool
                            .clone()
                            .unwrap_or_else(|| "lightonocr".into()),
                        ocr_home: settings.ocr_home.clone(),
                        lighton: settings.lighton.take(),
                        sources,
                        routes: settings.routes.clone(),
                        commands: settings.commands.clone(),
                        modules: settings.modules.clone(),
                    },
                    host,
                    cancel,
                )
                .map_err(|error| {
                    if error.aborted {
                        StageError::aborted()
                    } else {
                        StageError::plain(error.message)
                    }
                })?;
                match outcome {
                    SourceOutcome::Done {
                        output,
                        reused,
                        warnings: found,
                    } => {
                        warnings.extend(found);
                        source = Some(output.clone());
                        (output, reused)
                    }
                    SourceOutcome::Unsupported(stats) => {
                        let mut error = StageError::coded(
                            "UNSUPPORTED_FILES",
                            "Unsupported files found. Set source.filesystem with.exclude or use --skip-unsupported.",
                            2,
                        );
                        error.data = Some(stats);
                        return Err(error);
                    }
                }
            }
            _ => {
                let source_id = match source
                    .as_ref()
                    .and_then(|source| string_field(source, "id"))
                {
                    Some(id) => id.to_owned(),
                    None => input_stage_id(&connection, "source", fixed.as_ref())?,
                };
                let outcome = crate::transform_stage::run(
                    &TransformRequest {
                        state: request.state.clone(),
                        corpus: request.corpus.clone(),
                        source_id,
                        force: request.force,
                        modules: request.transform.modules.clone(),
                        embedder: request.transform.embedder.clone(),
                    },
                    host,
                    cancel,
                )?;
                (outcome.output, outcome.reused)
            }
        };
        cancel.check().map_err(StageError::native)?;
        results.push((
            stage.to_owned(),
            object(vec![
                (
                    "id",
                    field(&output, "id").cloned().unwrap_or(ConfigValue::Null),
                ),
                ("reused", ConfigValue::Bool(reused)),
                (
                    "elapsed_ms",
                    elapsed(stage_start, std::mem::take(&mut before)),
                ),
            ]),
        ));
    }
    let mut output = vec![("stages".to_owned(), ConfigValue::Object(results))];
    if let Some(source) = &source {
        assign(&mut output, "files", field(source, "files").cloned());
        assign(&mut output, "skipped", field(source, "skipped").cloned());
    }
    assign(
        &mut output,
        "serving_changed",
        Some(ConfigValue::Bool(false)),
    );
    assign(&mut output, "elapsed_ms", Some(elapsed(start, total)));
    Ok(BuildOutcome {
        output: ConfigValue::Object(output),
        warnings,
    })
}

/// `--dry-run`: the modules of each stage and, from the source stage, what would run for each
/// file; from a later stage, the saved stage it would read.
fn dry_run(
    request: &BuildRequest,
    selected: &[&'static str],
    cancel: &Cancellation,
) -> Result<ConfigValue, StageError> {
    let mut data = vec![
        (
            "stages".to_owned(),
            ConfigValue::Object(
                selected
                    .iter()
                    .map(|stage| {
                        (
                            (*stage).to_owned(),
                            object(vec![("modules", stage_modules(request, stage).clone())]),
                        )
                    })
                    .collect(),
            ),
        ),
        ("serving_changed".to_owned(), ConfigValue::Bool(false)),
    ];
    let connection = open(&request.state, true)?;
    let serving = serving_build(&connection)?;
    let previous = match &serving {
        Some(serving) => match string_field(serving, "source_id") {
            Some(id) => stage_manifest(&connection, "source", id)?,
            None => None,
        },
        None => match latest_stage_id(&connection, "source")? {
            Some(id) => stage_manifest(&connection, "source", &id)?,
            None => None,
        },
    };
    let transform = match serving
        .as_ref()
        .and_then(|serving| string_field(serving, "transform_id"))
    {
        Some(id) => stage_manifest(&connection, "transform", id)?,
        None => None,
    };
    if selected.first() != Some(&"source") {
        let fixed = build_input(&connection, request)?;
        let position = STAGES
            .iter()
            .position(|stage| Some(stage) == selected.first())
            .unwrap_or(1);
        let id = input_stage_id(&connection, STAGES[position - 1], fixed.as_ref())?;
        assign(&mut data, "input_id", Some(text(&id)));
        return Ok(ConfigValue::Object(data));
    }
    let sources = scan_sources(request, cancel)?;
    let files: Vec<(&str, &RouteFile)> = sources
        .iter()
        .flat_map(|source| {
            source
                .files
                .iter()
                .map(move |file| (source.id.as_str(), file))
        })
        .collect();
    let routed = route(&RouteRequest {
        routes: request.source.routes.clone(),
        files: files.iter().map(|(_, file)| (*file).clone()).collect(),
    });
    let mut skipped = Vec::new();
    let mut parsers = std::collections::HashMap::new();
    for ((id, file), routed) in files.iter().zip(&routed) {
        parsers.insert((*id, file.object_id.as_str()), routed.route.clone());
        if routed.route.is_none() {
            skipped.push(object(vec![
                ("source_id", text(id)),
                ("path", text(&file.object_id)),
                ("reason", text("No configured loader for this file format")),
            ]));
        }
    }
    let commands = request
        .source
        .commands
        .iter()
        .map(|command| {
            let args = field(command, "args");
            let mut entry = Vec::new();
            assign(&mut entry, "module", field(command, "module").cloned());
            assign(
                &mut entry,
                "command",
                args.and_then(|args| field(args, "command")).cloned(),
            );
            if let Some(version) = args
                .and_then(|args| field(args, "version_command"))
                .filter(|version| crate::index_stage::truthy(Some(version)))
            {
                assign(&mut entry, "version_command", Some(version.clone()));
            }
            ConfigValue::Object(entry)
        })
        .collect();
    let config_id = |value: Option<&ConfigValue>| {
        value
            .and_then(|value| string_field(value, "config_id"))
            .map(str::to_owned)
    };
    let source_changed =
        config_id(previous.as_ref()) != Some(digest("cfg", &request.source.modules));
    let transform_changed =
        config_id(transform.as_ref()) != Some(digest("cfg", &request.transform.modules));
    let index_changed = {
        let config: Option<String> = match serving
            .as_ref()
            .and_then(|serving| string_field(serving, "build_id"))
        {
            Some(id) => connection
                .query_row(
                    "SELECT config FROM build_config WHERE ref = ?",
                    [id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql)?,
            None => None,
        };
        let hash = if selected.contains(&"index") {
            dictionary_hash(request)?
        } else {
            None
        };
        let built = serving
            .as_ref()
            .and_then(|serving| field(serving, "keyword"))
            .and_then(|keyword| string_field(keyword, "dictionary_hash"))
            .map(str::to_owned);
        match config {
            None => true,
            Some(config) => {
                js_json(field(&parse(&config, "A saved configuration")?, "index"))
                    != js_json(request.config_index.as_ref())
                    || built != hash
            }
        }
    };
    assign(
        &mut data,
        "skipped",
        Some(ConfigValue::Array(skipped.clone())),
    );
    assign(&mut data, "commands", Some(ConfigValue::Array(commands)));
    let skipped: std::collections::HashSet<(String, String)> = skipped
        .iter()
        .map(|entry| {
            (
                string_field(entry, "source_id")
                    .unwrap_or_default()
                    .to_owned(),
                string_field(entry, "path").unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let mut entries = Vec::new();
    for (source_id, path, change) in inventory(&files, previous.as_ref(), cancel)? {
        let skip = skipped.contains(&(source_id.clone(), path.clone()));
        let changed = request.force || change != "unchanged";
        let stages = selected
            .iter()
            .map(|stage| {
                let value = if skip {
                    "skip"
                } else if change == "deleted" {
                    "remove"
                } else if changed {
                    "run"
                } else {
                    let checked = match *stage {
                        "source" => source_changed,
                        "transform" => source_changed || transform_changed,
                        _ => source_changed || transform_changed || index_changed,
                    };
                    if checked { "check_cache" } else { "reuse" }
                };
                ((*stage).to_owned(), text(value))
            })
            .collect();
        let parser = parsers
            .get(&(source_id.as_str(), path.as_str()))
            .cloned()
            .flatten()
            .map_or(ConfigValue::Null, |name| text(&name));
        entries.push(object(vec![
            ("source_id", text(&source_id)),
            ("path", text(&path)),
            ("change", text(change)),
            ("parser", parser),
            ("stages", ConfigValue::Object(stages)),
        ]));
    }
    assign(&mut data, "files", Some(ConfigValue::Array(entries)));
    Ok(ConfigValue::Object(data))
}

/// The SHA-256 of the user dictionary of `index.keyword`, when one is set.
fn dictionary_hash(request: &BuildRequest) -> Result<Option<String>, StageError> {
    let Some(path) = request
        .index
        .keyword
        .as_ref()
        .and_then(|keyword| string_field(keyword, "user_dictionary"))
        .filter(|path| !path.is_empty())
    else {
        return Ok(None);
    };
    std::fs::read(path)
        .map(|bytes| Some(sha256_hex(&bytes)))
        .map_err(|_| {
            StageError::coded(
                "USER_DICTIONARY_REQUIRED",
                format!("Could not read the user dictionary CSV at {path}"),
                2,
            )
        })
}

/// Each scanned file as added, updated or unchanged against the previous source stage, by the
/// SHA-256 of its bytes, then the files that are gone as deleted.
fn inventory(
    files: &[(&str, &RouteFile)],
    previous: Option<&ConfigValue>,
    cancel: &Cancellation,
) -> Result<Vec<(String, String, &'static str)>, StageError> {
    let mut old: Vec<((String, String), Option<String>)> = Vec::new();
    for group in items(previous.and_then(|previous| field(previous, "groups"))) {
        let pair = items(Some(group));
        let id = pair
            .first()
            .and_then(ConfigValue::as_str)
            .unwrap_or_default();
        for document in items(pair.get(1)) {
            let key = (
                id.to_owned(),
                string_field(document, "objectId")
                    .unwrap_or_default()
                    .to_owned(),
            );
            // Before step 2e moved a project, its entries were whole documents.
            let hash = string_field(document, "sha256")
                .or_else(|| {
                    field(document, "corpus")
                        .and_then(|corpus| string_field(corpus, "originalSha256"))
                })
                .map(str::to_owned);
            match old.iter_mut().find(|(existing, _)| *existing == key) {
                Some(entry) => entry.1 = hash,
                None => old.push((key, hash)),
            }
        }
    }
    let mut out = Vec::new();
    for (id, file) in files {
        cancel.check().map_err(StageError::native)?;
        let actual = file_hash(&file.file)?;
        let key = ((*id).to_owned(), file.object_id.clone());
        let change = match old.iter().position(|(existing, _)| *existing == key) {
            None => "added",
            Some(index) => {
                let (_, hash) = old.remove(index);
                if hash.as_deref() == Some(actual.as_str()) {
                    "unchanged"
                } else {
                    "updated"
                }
            }
        };
        out.push((key.0, key.1, change));
    }
    for ((id, path), _) in old {
        out.push((id, path, "deleted"));
    }
    Ok(out)
}

fn file_hash(path: &str) -> Result<String, StageError> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)
        .map_err(|error| StageError::plain(format!("Could not read {path}: {error}")))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| StageError::plain(format!("Could not read {path}: {error}")))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Answers the `tag` command's question about the search configuration ID of a build.
pub trait TagHost {
    /// `searchConfigId(restoreModules(build.search), {}, build, project)`.
    fn search_config_id(&self, build: &ConfigValue) -> Result<String, String>;
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagRequest {
    pub state: String,
    pub name: String,
    /// The build ID or tag to name. The serving build when absent.
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default, rename = "move")]
    pub moved: bool,
    /// `moduleSettings(plan.search)`, for the serving build.
    pub search: ConfigValue,
    /// The `search` and `models` sections of the configuration, saved with a tag of the serving build.
    #[serde(default)]
    pub config_search: Option<ConfigValue>,
    #[serde(default)]
    pub config_models: Option<ConfigValue>,
}

/// `nest tag <name> [build]`: names a build with its search settings and configuration.
/// Returns `{ tag, build_id, search_config_id }`.
pub fn tag(request: &TagRequest, host: &dyn TagHost) -> Result<ConfigValue, StageError> {
    let mut connection = open(&request.state, false)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    let selected = match &request.reference {
        Some(reference) => reference.clone(),
        None => serving_build(&transaction)?
            .as_ref()
            .and_then(|build| string_field(build, "build_id"))
            .map(str::to_owned)
            .ok_or_else(build_required)?,
    };
    let ConfigValue::Object(mut build) =
        named_build(&transaction, &selected)?.ok_or_else(build_required)?
    else {
        return Err(StageError::plain("Invalid build manifest."));
    };
    let build_id = build
        .iter()
        .find(|(name, _)| name == "build_id")
        .and_then(|(_, id)| id.as_str())
        .unwrap_or_default()
        .to_owned();
    // `currentBuild(paths, true, selected)` adds the search settings and their models.
    let one = |query: &str, key: &str| -> Result<Option<String>, StageError> {
        transaction
            .query_row(query, [key], |row| row.get(0))
            .optional()
            .map_err(sql)
    };
    let tag_settings = one("SELECT settings FROM tag_search WHERE name = ?", &selected)?;
    let settings = match &tag_settings {
        Some(settings) => Some(settings.clone()),
        None => one(
            "SELECT settings FROM build_search WHERE build_id = ?",
            &build_id,
        )?,
    };
    if let Some(settings) = settings {
        assign(
            &mut build,
            "search",
            Some(parse(&settings, "Saved search settings")?),
        );
    }
    let configuration_ref = if tag_settings.is_some() {
        format!("tag:{selected}")
    } else {
        build_id.clone()
    };
    if let Some(configuration) = one(
        "SELECT config FROM build_config WHERE ref = ?",
        &configuration_ref,
    )? {
        let configuration = parse(&configuration, "A saved configuration")?;
        assign(
            &mut build,
            "search_models",
            field(&configuration, "models").cloned(),
        );
    }
    if request.reference.is_none() {
        assign(&mut build, "search", Some(request.search.clone()));
    }
    let by_tag = one(
        "SELECT config FROM build_config WHERE ref = ?",
        &format!(
            "tag:{}",
            request.reference.as_deref().unwrap_or("undefined")
        ),
    )?;
    let stored = match by_tag {
        Some(stored) => stored,
        None => {
            one("SELECT config FROM build_config WHERE ref = ?", &build_id)?.ok_or_else(|| {
                StageError::coded(
                    "CONFIG_REQUIRED",
                    "Build the candidate again to save its configuration.",
                    3,
                )
            })?
        }
    };
    let mut configuration = parse(&stored, "A saved configuration")?;
    if request.reference.is_none()
        && let ConfigValue::Object(entries) = &mut configuration
    {
        assign(entries, "search", request.config_search.clone());
        assign(entries, "models", request.config_models.clone());
    }
    let build = ConfigValue::Object(build);
    let search = field(&build, "search").cloned();
    let settings = js_json(search.as_ref());
    let existing: Option<(String, String)> = transaction
        .query_row(
            "SELECT build_id, settings FROM build_tags JOIN tag_search USING (name) WHERE name = ?",
            [&request.name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql)?;
    let differs = existing
        .as_ref()
        .is_some_and(|(id, saved)| *id != build_id || Some(saved) != settings.as_ref());
    if differs && !request.moved {
        return Err(StageError::coded(
            "TAG_EXISTS",
            "Use --move to explicitly change an existing tag.",
            2,
        ));
    }
    if existing.is_none() || differs {
        transaction
            .execute(
                "INSERT OR REPLACE INTO build_tags VALUES (?, ?)",
                params![request.name, build_id],
            )
            .map_err(sql)?;
        transaction
            .execute(
                "INSERT OR REPLACE INTO tag_search VALUES (?, ?)",
                params![request.name, settings],
            )
            .map_err(sql)?;
        transaction
            .execute(
                "INSERT OR REPLACE INTO build_config VALUES (?, ?)",
                params![format!("tag:{}", request.name), configuration.to_js_json()],
            )
            .map_err(sql)?;
        let previous = existing
            .map(|(id, saved)| -> Result<String, StageError> {
                Ok(object(vec![
                    ("build_id", text(&id)),
                    ("search", parse(&saved, "Saved search settings")?),
                ])
                .to_js_json())
            })
            .transpose()?;
        let mut target = vec![("build_id".to_owned(), text(&build_id))];
        assign(&mut target, "search", search);
        transaction
            .execute(
                "INSERT INTO tag_history (name, previous, target) VALUES (?, ?, ?)",
                params![
                    request.name,
                    previous,
                    ConfigValue::Object(target).to_js_json()
                ],
            )
            .map_err(sql)?;
    }
    let search_config_id = host.search_config_id(&build).map_err(StageError::plain)?;
    transaction.commit().map_err(sql)?;
    Ok(object(vec![
        ("tag", text(&request.name)),
        ("build_id", text(&build_id)),
        ("search_config_id", text(&search_config_id)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::Charge;

    const SCHEMA: &str = "CREATE TABLE stage_outputs (stage TEXT NOT NULL, id TEXT NOT NULL, manifest TEXT NOT NULL, PRIMARY KEY (stage, id));
        CREATE TABLE latest_stage (stage TEXT PRIMARY KEY, id TEXT NOT NULL);
        CREATE TABLE transform_outputs (key TEXT PRIMARY KEY, output_id TEXT NOT NULL);
        CREATE TABLE source_checks (id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, checked_at TEXT NOT NULL);
        CREATE TABLE builds (id TEXT PRIMARY KEY, manifest TEXT NOT NULL);
        CREATE TABLE project (id INTEGER PRIMARY KEY CHECK (id = 1), build_id TEXT REFERENCES builds(id));
        INSERT INTO project VALUES (1, NULL);
        CREATE TABLE build_tags (name TEXT PRIMARY KEY, build_id TEXT NOT NULL);
        CREATE TABLE build_search (build_id TEXT PRIMARY KEY, settings TEXT NOT NULL);
        CREATE TABLE tag_search (name TEXT PRIMARY KEY, settings TEXT NOT NULL);
        CREATE TABLE build_config (ref TEXT PRIMARY KEY, config TEXT NOT NULL);
        CREATE TABLE adoption_history (id INTEGER PRIMARY KEY, previous TEXT NOT NULL, adopted TEXT NOT NULL);
        CREATE TABLE serving_generations (id INTEGER PRIMARY KEY, build_id TEXT NOT NULL);
        CREATE TABLE tag_history (id INTEGER PRIMARY KEY, name TEXT NOT NULL, previous TEXT, target TEXT NOT NULL);
        CREATE TABLE build_runs (id TEXT PRIMARY KEY, build_id TEXT NOT NULL, config_id TEXT NOT NULL, created TEXT NOT NULL);
        CREATE TABLE index_outputs (key TEXT PRIMARY KEY, build_id TEXT NOT NULL);
        CREATE TABLE embeddings (id TEXT PRIMARY KEY, data BLOB NOT NULL);";

    struct Answers;

    impl Host for Answers {}

    impl Charge for Answers {
        fn charge(&self, _bytes: usize) -> Result<(), String> {
            Ok(())
        }
    }

    impl IndexHost for Answers {
        fn config_changed(&self) -> Result<bool, String> {
            Ok(false)
        }

        fn index_tool(&self) -> Result<String, String> {
            Err("No index tool in tests".into())
        }
    }

    impl TagHost for Answers {
        fn search_config_id(&self, build: &ConfigValue) -> Result<String, String> {
            Ok(format!(
                "search-{}",
                string_field(build, "build_id").unwrap_or_default()
            ))
        }
    }

    fn json(text: &str) -> ConfigValue {
        serde_json::from_str(text).unwrap()
    }

    fn request(root: &std::path::Path, values: &str) -> BuildRequest {
        let data = root.join(".knowledge-nest");
        let mut text = format!(
            r#"{{{}"state":{},"artifacts":{},"corpus":{},"root":{},
            "source":{{"scans":[{{"path":{},"name":"source.filesystem"}}],"routes":[{{"name":"loader.auto","category":"auto","ocr":false}}],
              "modules":[{{"use":"source.filesystem"}}],"ocrVersion":"none"}},
            "transform":{{"modules":[{{"use":"chunk.length","with":{{"size":40,"overlap":0}}}}]}},
            "index":{{"keyword":{{"tokenizer":"trigram"}},"construction":[{{"use":"index.keyword"}}],"search":[],"rerank":[],"configuration":{{"index":{{}}}}}},
            "configIndex":{{}}"#,
            if values.contains("\"command\"") {
                ""
            } else {
                r#""command":"build","#
            },
            serde_json::to_string(&data.join("state.sqlite")).unwrap(),
            serde_json::to_string(&data.join("artifacts")).unwrap(),
            serde_json::to_string(&data.join("corpus")).unwrap(),
            serde_json::to_string(root).unwrap(),
            serde_json::to_string(&root.join("docs")).unwrap(),
        );
        text.push_str(values);
        text.push('}');
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn runs_stages_dry_runs_and_tags() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".knowledge-nest").join("artifacts")).unwrap();
        std::fs::create_dir_all(root.path().join("docs")).unwrap();
        std::fs::write(
            root.path().join("docs").join("guide.md"),
            "# 点検\n電源を止めます。\n",
        )
        .unwrap();
        Connection::open(root.path().join(".knowledge-nest").join("state.sqlite"))
            .unwrap()
            .execute_batch(SCHEMA)
            .unwrap();
        let cancel = Cancellation::default();

        let fresh = run(request(root.path(), r#","dryRun":true"#), &Answers, &cancel).unwrap();
        let files = items(fresh.output.get("files"));
        assert_eq!(string_field(&files[0], "change"), Some("added"));

        let missing = run(
            request(root.path(), r#","command":"transform","runId":"run-0""#),
            &Answers,
            &cancel,
        )
        .err()
        .unwrap();
        assert_eq!(missing.code, Some("STAGE_REQUIRED"));

        let built = run(
            request(root.path(), r#","runId":"run-1""#),
            &Answers,
            &cancel,
        )
        .unwrap();
        assert!(built.warnings.is_empty());
        let stages = built.output.get("stages").unwrap();
        assert!(stages.get("source").is_some() && stages.get("index").is_some());
        assert_eq!(
            built.output.get("serving_changed"),
            Some(&ConfigValue::Bool(true))
        );
        let build_id = string_field(&built.output, "build_id").unwrap().to_owned();

        let unchanged = run(request(root.path(), r#","dryRun":true"#), &Answers, &cancel).unwrap();
        let file = &items(unchanged.output.get("files"))[0];
        assert_eq!(
            file.get("stages").unwrap().to_js_json(),
            r#"{"source":"reuse","transform":"reuse","index":"reuse"}"#
        );

        let input = run(
            request(
                root.path(),
                r#","dryRun":true,"from":"index","input":"bld-missing""#,
            ),
            &Answers,
            &cancel,
        )
        .err()
        .unwrap();
        assert_eq!(input.code, Some("BUILD_REQUIRED"));

        let tagged = tag(
            &TagRequest {
                state: root
                    .path()
                    .join(".knowledge-nest")
                    .join("state.sqlite")
                    .to_string_lossy()
                    .into_owned(),
                name: "v1".into(),
                reference: None,
                moved: false,
                search: json("[]"),
                config_search: None,
                config_models: None,
            },
            &Answers,
        )
        .unwrap();
        assert_eq!(string_field(&tagged, "build_id"), Some(build_id.as_str()));
        assert_eq!(
            string_field(&tagged, "search_config_id"),
            Some(format!("search-{build_id}").as_str())
        );
    }

    #[test]
    fn keeps_unchanged_documents_without_reading_them() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join(".knowledge-nest");
        let docs = root.path().join("docs");
        std::fs::create_dir_all(data.join("artifacts")).unwrap();
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("a.md"), "# Alpha\nalpha one\n").unwrap();
        std::fs::write(docs.join("b.md"), "# Beta\nbeta two\n").unwrap();
        std::fs::write(docs.join("c.md"), "# Gone\nremoved later\n").unwrap();
        Connection::open(data.join("state.sqlite"))
            .unwrap()
            .execute_batch(SCHEMA)
            .unwrap();
        let cancel = Cancellation::default();
        run(
            request(root.path(), r#","runId":"run-1""#),
            &Answers,
            &cancel,
        )
        .unwrap();
        // b.md changes and c.md goes; a.md stays in the copied index without being read.
        std::fs::write(docs.join("b.md"), "# Delta\ndelta four\n").unwrap();
        std::fs::remove_file(docs.join("c.md")).unwrap();
        let built = run(
            request(root.path(), r#","runId":"run-2""#),
            &Answers,
            &cancel,
        )
        .unwrap();
        assert_eq!(
            field(&built.output, "documents"),
            Some(&ConfigValue::Number(2.0))
        );
        let index = string_field(&built.output, "index")
            .unwrap_or("run-2")
            .to_owned();
        let database = crate::KnowledgeBase::open_with_options(
            &data.join("artifacts").join(index),
            crate::CorpusBackend::Sqlite,
            Some(&data.join("corpus")),
            crate::index_stage::keyword_options(Some(&json(r#"{"tokenizer":"trigram"}"#)), None),
            &cancel,
        )
        .unwrap();
        let read = |object: &str| {
            database
                .get_document(
                    &crate::DocumentRef {
                        source_id: "docs".into(),
                        object_id: object.into(),
                    },
                    &cancel,
                )
                .unwrap()
                .map(|document| document.content)
        };
        assert_eq!(read("a.md").as_deref(), Some("# Alpha\nalpha one\n"));
        assert_eq!(read("b.md").as_deref(), Some("# Delta\ndelta four\n"));
        assert_eq!(read("c.md"), None);
    }

    #[test]
    fn selects_stages_in_order() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("docs")).unwrap();
        let chosen = |values: &str| selected(&request(root.path(), values)).unwrap();
        assert_eq!(chosen(""), ["source", "transform", "index"]);
        assert_eq!(chosen(r#","from":"transform""#), ["transform", "index"]);
        assert_eq!(chosen(r#","until":"transform""#), ["source", "transform"]);
        assert_eq!(chosen(r#","command":"index""#), ["index"]);
    }
}
