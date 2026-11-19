//! `ingress`, `query` and `verify` of Wiki and PARA projects (spec 14 and 28.12 step 5a).

// The entry points are called from `node`, which test builds leave out.
#![cfg_attr(test, allow(dead_code))]

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::config_value::ConfigValue;
use crate::eval_data::js_trim;
use crate::evidence_match::nfkc;
use crate::index_stage::{assign, items, object, sql, string_field, text, truthy};
use crate::node_path;
use crate::source_load::{Host, LoadFile, LoadRequest, sha256_hex};
use crate::source_stage::iso_now;
use crate::transform_stage::StageError;
use crate::wiki_commands::{
    Ref, arg, chunks, chunks_for_model, cited_text, commit_pages, ref_inputs, replace_documents,
    verify_facts,
};
use crate::wiki_project::{
    ANSWER_SCHEMA, In, Project, WRITTEN_SCHEMA, coded, current_document, facts, fs_error,
    json_or_undefined, link_targets, linked_page, marker_index, model_output, pages,
    paragraph_markers, read_citation, read_file, realpath, remember, str_of, strict, unique_index,
    utf16_prefix, written_pages,
};
use crate::wiki_work::Tracker;

/// Word-like segments, as `Intl.Segmenter('ja', { granularity: 'word' })` finds them.
pub(crate) fn words(text: &str) -> Vec<&str> {
    let segmenter = icu_segmenter::WordSegmenter::new_dictionary(
        icu_segmenter::options::WordBreakInvariantOptions::default(),
    );
    let mut iterator = segmenter.segment_str(text);
    let mut found = Vec::new();
    let mut last = 0;
    while let Some(boundary) = iterator.next() {
        if boundary == 0 {
            continue;
        }
        if iterator.is_word_like() {
            found.push(&text[last..boundary]);
        }
        last = boundary;
    }
    found
}

pub(crate) fn query(
    project: &Project<'_>,
    connection: &Connection,
    tracker: &Tracker,
) -> Result<ConfigValue, StageError> {
    let plan = items(project.request.plan.get("query"));
    let settings = plan.first().cloned().unwrap_or_default();
    let model = plan.get(1).cloned().unwrap_or_default();
    let question = arg(project, 0);
    let normalized = nfkc(question);
    let terms: Vec<String> = words(&normalized)
        .into_iter()
        .map(str::to_lowercase)
        .collect();
    let only = project.value_str("page").filter(|page| !page.is_empty());
    let mut ranked: Vec<(ConfigValue, usize)> =
        pages(connection, truthy(settings.get("include_archived")))?
            .into_iter()
            .filter(|page| only.is_none_or(|id| str_of(page, "id") == id))
            .map(|page| {
                let haystack = nfkc(&format!(
                    "{}\n{}",
                    crate::eval_stage::js_string(page.get("title")),
                    crate::eval_stage::js_string(page.get("markdown"))
                ))
                .to_lowercase();
                let rank = terms
                    .iter()
                    .filter(|term| haystack.contains(term.as_str()))
                    .count();
                (page, rank)
            })
            .filter(|(_, rank)| *rank > 0 || only.is_some())
            .collect();
    // Stable, as Array.prototype.sort is.
    ranked.sort_by_key(|(_, rank)| std::cmp::Reverse(*rank));
    ranked.truncate(8);
    let ranked: Vec<ConfigValue> = ranked.into_iter().map(|(page, _)| page).collect();
    if ranked.is_empty() {
        return Ok(object(vec![
            ("answer", text("")),
            ("pages", ConfigValue::Array(Vec::new())),
            ("status", text("insufficient_evidence")),
        ]));
    }
    tracker.progress("answering", None);
    let field =
        |page: &ConfigValue, name: &str| In::Value(page.get(name).cloned().unwrap_or_default());
    let result = project.generate(
        str_of(&model, "model"),
        ANSWER_SCHEMA,
        "Answer with facts and exact quotes copied from the numbered pages. Return no facts if the question is outside scope.",
        &In::Map(vec![
            ("about", In::Value(project.request.about.clone())),
            ("question", In::text(question)),
            ("target", In::text(project.value_str("target").unwrap_or("page"))),
            (
                "pages",
                In::List(
                    ranked
                        .iter()
                        .enumerate()
                        .map(|(index, page)| {
                            In::Map(vec![
                                ("id", In::number(index + 1)),
                                ("title", field(page, "title")),
                                ("markdown", field(page, "markdown")),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]),
    )?;
    let [list] = strict(&result, &["facts"]).ok_or_else(model_output)?[..] else {
        return Err(model_output());
    };
    let answered = facts(list, false).ok_or_else(model_output)?;
    let mut cited: Vec<usize> = Vec::new();
    let mut found = Vec::new();
    for fact in &answered {
        let mut evidence = Vec::new();
        for citation in &fact.citations {
            let page = ranked.get(citation.id - 1);
            let markdown = page
                .map(|page| str_of(page, "markdown"))
                .unwrap_or_default();
            if page.is_none() || unique_index(markdown, &citation.quote).is_none() {
                return Err(coded(
                    2,
                    "INVALID_CITATION",
                    "The answer cited an unresolved page quote.",
                ));
            }
            if !cited.contains(&(citation.id - 1)) {
                cited.push(citation.id - 1);
            }
            let Some((_, markers)) = paragraph_markers(markdown, &citation.quote) else {
                return Err(coded(
                    2,
                    "INVALID_CITATION",
                    "The quoted page fact has no source markers.",
                ));
            };
            let citations = items(page.and_then(|page| page.get("citations")));
            for marker in markers {
                evidence.push(
                    marker_index(&marker)
                        .and_then(|index| citations.get(index).cloned())
                        .unwrap_or(ConfigValue::Null),
                );
            }
        }
        found.push(object(vec![
            ("text", text(&fact.text)),
            ("evidence", ConfigValue::Array(evidence)),
        ]));
    }
    let answer = answered
        .iter()
        .map(|fact| fact.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut entries = vec![
        (
            "id",
            text(project.request.run_id.as_deref().unwrap_or_default()),
        ),
        ("action", text(&project.request.command)),
        ("answer", text(&answer)),
    ];
    if found.is_empty() {
        entries.push(("status", text("insufficient_evidence")));
    } else {
        entries.push(("facts", ConfigValue::Array(found)));
    }
    entries.push((
        "pages",
        ConfigValue::Array(
            cited
                .iter()
                .map(|&index| {
                    let page = &ranked[index];
                    let get = |name: &str| page.get(name).cloned().unwrap_or_default();
                    object(vec![
                        ("id", get("id")),
                        ("title", get("title")),
                        ("citations", get("citations")),
                    ])
                })
                .collect(),
        ),
    ));
    remember(connection, object(entries))
}

/// The value without `acquiredAtMs` and `modifiedAtMs` at any depth, the JSON replacer of issue IDs.
fn without_times(value: &ConfigValue) -> ConfigValue {
    match value {
        ConfigValue::Object(entries) => ConfigValue::Object(
            entries
                .iter()
                .filter(|(name, _)| name != "acquiredAtMs" && name != "modifiedAtMs")
                .map(|(name, value)| (name.clone(), without_times(value)))
                .collect(),
        ),
        ConfigValue::Array(values) => {
            ConfigValue::Array(values.iter().map(without_times).collect())
        }
        other => other.clone(),
    }
}

pub(crate) fn verify(
    project: &Project<'_>,
    connection: &Connection,
    tracker: &Tracker,
) -> Result<ConfigValue, StageError> {
    let material = pages(connection, true)?;
    let mut issues: Vec<ConfigValue> = Vec::new();
    let mut review: Option<ConfigValue> = None;
    let known: HashSet<&str> = material.iter().map(|page| str_of(page, "id")).collect();
    let mut incoming: HashSet<String> = HashSet::new();
    let database = project.trigram_database()?;
    for (completed, page) in material.iter().enumerate() {
        tracker.progress("checking_citations", Some((completed, material.len())));
        let page_id = page.get("id").cloned().unwrap_or_default();
        for item in items(page.get("citations")) {
            let valid = match read_citation(&database, item, None, project.cancel) {
                Ok(_) => true,
                Err(error) if error.code == Some("INVALID_CITATION") => false,
                Err(error) => return Err(error),
            };
            let source = item.get("source").cloned().unwrap_or_default();
            let current = current_document(&database, &source, project.cancel)?;
            let current_version = current
                .as_ref()
                .and_then(|document| document.get("evidence"))
                .and_then(|evidence| evidence.get("source"))
                .and_then(|source| source.get("version"));
            if !valid {
                issues.push(object(vec![
                    ("code", text("CITATION_MISSING")),
                    ("page", page_id.clone()),
                    ("evidence", item.clone()),
                ]));
            } else if !truthy(page.get("archived"))
                && json_or_undefined(current_version) != json_or_undefined(source.get("version"))
            {
                issues.push(object(vec![
                    ("code", text("STALE_VERSION")),
                    ("page", page_id.clone()),
                    ("evidence", item.clone()),
                ]));
            }
        }
        let markdown = str_of(page, "markdown");
        for (start, end) in link_targets(markdown) {
            let target = &markdown[start..end];
            let Some(id) = linked_page(str_of(page, "id"), target) else {
                continue;
            };
            if known.contains(id.as_str()) {
                incoming.insert(id);
            } else {
                issues.push(object(vec![
                    ("code", text("BROKEN_LINK")),
                    ("page", page_id.clone()),
                    ("target", text(target)),
                ]));
            }
        }
    }
    let content = truthy(project.value("content"));
    if content {
        let (found, checked) =
            crate::wiki_inspect::inspect(project, connection, &database, &material, tracker)?;
        issues.extend(found);
        review = Some(checked);
    }
    drop(database);
    for page in &material {
        if !truthy(page.get("archived")) && !incoming.contains(str_of(page, "id")) {
            issues.push(object(vec![
                ("code", text("ORPHAN_PAGE")),
                ("page", page.get("id").cloned().unwrap_or_default()),
            ]));
        }
    }
    let table = connection
        .query_row(
            "SELECT name FROM sqlite_master WHERE name = 'wiki_issues'",
            [],
            |_| Ok(()),
        )
        .is_ok();
    let mut order: Vec<String> = Vec::new();
    let mut active: HashMap<String, ConfigValue> = HashMap::new();
    for issue in issues {
        let ConfigValue::Object(mut entries) = issue else {
            continue;
        };
        let id = match entries.iter().find(|(name, _)| name == "id") {
            Some((_, ConfigValue::String(id))) => id.clone(),
            _ => {
                let identity = ConfigValue::Object(
                    entries
                        .iter()
                        .filter(|(name, _)| name != "description")
                        .cloned()
                        .collect(),
                );
                format!(
                    "issue-{}",
                    sha256_hex(without_times(&identity).to_js_json().as_bytes())
                )
            }
        };
        let state: String = if table {
            connection
                .query_row(
                    "SELECT state FROM wiki_issues WHERE id = ?1",
                    [&id],
                    |row| row.get(0),
                )
                .unwrap_or_else(|_| "open".to_owned())
        } else {
            "open".to_owned()
        };
        assign(&mut entries, "id", Some(text(&id)));
        assign(&mut entries, "state", Some(text(&state)));
        if !active.contains_key(&id) {
            order.push(id.clone());
        }
        active.insert(id, ConfigValue::Object(entries));
    }
    let all = truthy(project.value("all-issues"));
    let only = project.value_str("page").filter(|page| !page.is_empty());
    let listed: Vec<ConfigValue> = order
        .into_iter()
        .filter_map(|id| active.remove(&id))
        .filter(|issue| all || string_field(issue, "state") == Some("open"))
        .filter(|issue| {
            only.is_none_or(|page| {
                string_field(issue, "page") == Some(page)
                    || items(issue.get("pages"))
                        .iter()
                        .any(|value| value.as_str() == Some(page))
            })
        })
        .collect();
    let mut result = vec![("issues", ConfigValue::Array(listed))];
    if let Some(review) = review {
        result.push(("review", review));
    }
    if !content {
        return Ok(object(result));
    }
    let mut entries = vec![
        (
            "id",
            text(project.request.run_id.as_deref().unwrap_or_default()),
        ),
        ("action", text(&project.request.command)),
        ("checked_at", text(&iso_now())),
    ];
    entries.extend(result);
    remember(connection, object(entries))
}

/// The files under a folder, skipping names that start with a dot, `node_modules` and links.
fn list_files(directory: &str, found: &mut Vec<String>) -> Result<(), StageError> {
    let entries =
        std::fs::read_dir(directory).map_err(|error| fs_error("opendir", directory, &error))?;
    for entry in entries {
        let entry = entry.map_err(|error| fs_error("opendir", directory, &error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(|error| fs_error("opendir", directory, &error))?;
        if kind.is_symlink() {
            continue;
        }
        let file = node_path::join(directory, &[&name]);
        if kind.is_dir() {
            list_files(&file, found)?;
        } else if kind.is_file() {
            found.push(file);
        }
    }
    Ok(())
}

/// The progress of a load, recorded as `extracting_sources`.
struct LoadHost<'a> {
    tracker: &'a Tracker,
    total: usize,
}

impl Host for LoadHost<'_> {
    fn progress(&self, completed: usize) {
        self.tracker
            .progress("extracting_sources", Some((completed, self.total)));
    }
}

fn evidence_source(document: &ConfigValue) -> ConfigValue {
    document
        .get("evidence")
        .and_then(|evidence| evidence.get("source"))
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn ingress(
    project: &Project<'_>,
    connection: &Connection,
    tracker: &Tracker,
    warnings: &mut Vec<ConfigValue>,
) -> Result<ConfigValue, StageError> {
    let request = project.request;
    let plan = items(request.plan.get("ingress"));
    let loader = plan.first().cloned().unwrap_or_default();
    let writing = plan.get(2).cloned().unwrap_or_default();
    let dry_run = truthy(project.value("dry-run"));
    let source_only = truthy(project.value("source-only"));
    if !dry_run && !source_only {
        if js_trim(str_of(&request.about, "purpose")).is_empty() {
            return Err(coded(
                3,
                "PURPOSE_REQUIRED",
                "Set about.purpose before writing pages.",
            ));
        }
        project.writer(str_of(&writing, "model"))?;
    }
    if !dry_run {
        tracker.progress("finding_sources", None);
    }
    // Files by path, in the order they were first found.
    let mut files: Vec<(String, String)> = Vec::new();
    let add = |files: &mut Vec<(String, String)>, file: String, object_id: String| match files
        .iter_mut()
        .find(|(path, _)| *path == file)
    {
        Some(entry) => entry.1 = object_id,
        None => files.push((file, object_id)),
    };
    for input in request.args.iter().flatten() {
        let home = input
            .strip_prefix("~/")
            .or_else(|| input.strip_prefix("~\\"));
        let path = match home {
            Some(rest) => node_path::resolve(&request.home, rest),
            None => node_path::resolve(&request.cwd, input),
        };
        let root = realpath(&path)?;
        let metadata = std::fs::metadata(&root).map_err(|error| fs_error("stat", &root, &error))?;
        if metadata.is_dir() {
            let mut listed = Vec::new();
            list_files(&root, &mut listed)?;
            for file in listed {
                let relative = node_path::relative(&root, &file);
                let object_id = if cfg!(windows) {
                    relative.replace('\\', "/")
                } else {
                    relative
                };
                add(&mut files, file, object_id);
            }
        } else {
            let name = std::path::Path::new(&root)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            add(&mut files, root.clone(), name);
        }
    }
    let pending_only = truthy(project.value("pending"));
    if request.args.iter().flatten().next().is_none() && !pending_only {
        return Err(coded(
            2,
            "SOURCE_REQUIRED",
            "Provide a path or specify --pending.",
        ));
    }
    let upload = project.value_str("uploadName");
    let grouped: Vec<(String, String, String)> = files
        .iter()
        .map(|(file, object_id)| {
            let key = match upload {
                Some(name) => format!("browser:{}", nfkc(name)),
                None => file.clone(),
            };
            (
                format!("source-{}", &sha256_hex(key.as_bytes())[..24]),
                file.clone(),
                object_id.clone(),
            )
        })
        .collect();
    let routed = crate::source_scan::route(&crate::source_scan::RouteRequest {
        routes: vec![crate::source_scan::Route {
            name: "loader.auto".into(),
            category: "auto".into(),
            patterns: None,
            ocr: string_field(&loader, "ocr") == Some("loader.ocr"),
        }],
        files: grouped
            .iter()
            .map(|(_, file, object_id)| crate::source_scan::RouteFile {
                file: file.clone(),
                object_id: object_id.clone(),
            })
            .collect(),
    });
    let skipped: Vec<ConfigValue> = grouped
        .iter()
        .zip(&routed)
        .filter(|(_, routed)| routed.route.is_none())
        .map(|((id, _, object_id), _)| {
            object(vec![
                ("source_id", text(id)),
                ("path", text(object_id)),
                ("reason", text("No configured loader for this file format")),
            ])
        })
        .collect();
    let pending: Vec<(String, String)> = if pending_only {
        let mut statement = connection
            .prepare("SELECT id, data FROM wiki_sources WHERE reflected = 0")
            .map_err(sql)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)?
    } else {
        Vec::new()
    };
    if dry_run {
        return Ok(object(vec![
            ("files", ConfigValue::Number(files.len() as f64)),
            ("pending", ConfigValue::Number(pending.len() as f64)),
            ("skipped", ConfigValue::Array(skipped)),
        ]));
    }
    if !skipped.is_empty() && string_field(&loader, "unsupported") == Some("error") {
        return Err(coded(
            2,
            "UNSUPPORTED_FILES",
            ConfigValue::Array(skipped.clone()).to_js_json(),
        ));
    }
    let loading: Vec<(usize, LoadFile)> = grouped
        .iter()
        .zip(&routed)
        .enumerate()
        .filter_map(|(index, ((id, file, object_id), routed))| {
            Some((
                index,
                LoadFile {
                    id: id.clone(),
                    file: file.clone(),
                    object_id: object_id.clone(),
                    kind: routed.kind?.to_owned(),
                    route: routed.route.clone()?,
                    command: None,
                },
            ))
        })
        .collect();
    let total = files.len() - skipped.len();
    let mut load = match request.load.clone() {
        Some(ConfigValue::Object(entries)) => entries,
        _ => Vec::new(),
    };
    for (name, value) in [
        ("state", text(&request.state)),
        ("corpus", text(&request.corpus)),
        (
            "originals",
            text(&node_path::join(
                &request.artifacts,
                &["originals", "shared"],
            )),
        ),
        ("force", ConfigValue::Bool(false)),
        ("root", text(&request.cwd)),
        ("files", ConfigValue::Array(Vec::new())),
    ] {
        assign(&mut load, name, Some(value));
    }
    let mut load_request: LoadRequest =
        serde_json::from_str(&ConfigValue::Object(load).to_js_json())
            .map_err(|error| StageError::plain(format!("Invalid load request: {error}")))?;
    load_request.files = loading.iter().map(|(_, file)| file.clone()).collect();
    tracker.progress("extracting_sources", Some((0, total)));
    let loaded =
        crate::source_load::load(&load_request, &LoadHost { tracker, total }, project.cancel)
            .map_err(|error| {
                if error.aborted {
                    StageError::aborted()
                } else {
                    StageError::plain(error.message)
                }
            })?;
    warnings.extend(loaded.warnings);
    let mut by_index: HashMap<usize, ConfigValue> = loading
        .iter()
        .map(|(index, _)| *index)
        .zip(loaded.documents)
        .collect();
    let database = project.database()?;
    let mut documents: Vec<ConfigValue> = Vec::new();
    tracker.progress("indexing_sources", Some((0, grouped.len())));
    for (index, (id, _, _)) in grouped.iter().enumerate() {
        let Some(document) = by_index.remove(&index) else {
            continue;
        };
        let content = str_of(&document, "content").to_owned();
        let mut input = Vec::new();
        for name in [
            "objectId",
            "url",
            "format",
            "content",
            "corpus",
            "modifiedAtMs",
        ] {
            if let Some(value) = document.get(name) {
                input.push((name, value.clone()));
            }
        }
        input.push((
            "chunks",
            chunks(&content, str_of(&document, "format") == "markdown")?,
        ));
        replace_documents(&database, id, vec![object(input)], project.cancel)?;
        let reference = object(vec![
            ("sourceId", text(id)),
            (
                "objectId",
                document.get("objectId").cloned().unwrap_or_default(),
            ),
        ]);
        let saved = current_document(&database, &reference, project.cancel)?.ok_or_else(|| {
            StageError::plain("Cannot read properties of null (reading 'evidence')")
        })?;
        connection
            .execute(
                "INSERT INTO wiki_sources VALUES (?1, ?2, 0) ON CONFLICT(id) DO UPDATE SET data = excluded.data, reflected = 0",
                [id, &evidence_source(&saved).to_js_json()],
            )
            .map_err(sql)?;
        documents.push(saved);
        tracker.progress("indexing_sources", Some((documents.len(), grouped.len())));
    }
    for (id, data) in &pending {
        if documents
            .iter()
            .any(|document| string_field(&evidence_source(document), "sourceId") == Some(id))
        {
            continue;
        }
        let saved = crate::eval_stage::parse(data)?;
        if let Some(current) = current_document(&database, &saved, project.cancel)? {
            documents.push(current);
        }
    }
    let finished = |connection: &Connection, documents: usize, skipped: &[ConfigValue]| {
        remember(
            connection,
            object(vec![
                ("id", text(request.run_id.as_deref().unwrap_or_default())),
                ("action", text(&request.command)),
                ("sources", ConfigValue::Number(documents as f64)),
                ("pages", ConfigValue::Array(Vec::new())),
                ("skipped", ConfigValue::Array(skipped.to_vec())),
            ]),
        )
    };
    if source_only {
        return finished(connection, documents.len(), &skipped);
    }
    let mut refs = chunks_for_model(
        documents
            .iter()
            .map(|document| {
                (
                    str_of(document, "content").to_owned(),
                    document.get("evidence").cloned().unwrap_or_default(),
                )
            })
            .collect(),
    );
    if refs.is_empty() {
        return finished(connection, documents.len(), &skipped);
    }
    let rules = read_file(&project.rules_file())?;
    let existing = pages(connection, false)?;
    let previous = pages(connection, true)?;
    let affected: Vec<String> = existing
        .iter()
        .filter(|page| {
            items(page.get("citations")).iter().any(|citation| {
                let source = citation.get("source").cloned().unwrap_or_default();
                documents.iter().any(|document| {
                    let current = evidence_source(document);
                    source.get("sourceId") == current.get("sourceId")
                        && json_or_undefined(source.get("version"))
                            != json_or_undefined(current.get("version"))
                })
            })
        })
        .map(|page| str_of(page, "id").to_owned())
        .collect();
    let present: HashSet<String> = documents
        .iter()
        .map(|document| str_of(&evidence_source(document), "sourceId").to_owned())
        .collect();
    if !affected.is_empty() {
        let untouched: Vec<ConfigValue> = existing
            .iter()
            .filter(|page| affected.iter().any(|id| id == str_of(page, "id")))
            .map(|page| {
                let ConfigValue::Object(mut entries) = page.clone() else {
                    return page.clone();
                };
                let kept: Vec<ConfigValue> = items(page.get("citations"))
                    .iter()
                    .filter(|citation| {
                        let source = citation.get("source").cloned().unwrap_or_default();
                        !present.contains(str_of(&source, "sourceId"))
                    })
                    .cloned()
                    .collect();
                assign(&mut entries, "citations", Some(ConfigValue::Array(kept)));
                ConfigValue::Object(entries)
            })
            .collect();
        for item in cited_text(&database, &untouched, project.cancel)? {
            refs.push(Ref {
                id: refs.len() + 1,
                text: item.text,
                evidence: item.evidence,
            });
        }
    }
    tracker.progress("writing_pages", None);
    let instruction = format!(
        "Write in the language of the project. Create or update relevant pages only, using exact supporting quotes. Return zero pages for out-of-scope sources. Use safe relative page IDs. {}Each fact must have a citation.",
        if project.para() {
            "An ID must start with Projects/, Areas/, Resources/ or Archives/. "
        } else {
            ""
        }
    );
    let field =
        |page: &ConfigValue, name: &str| In::Value(page.get(name).cloned().unwrap_or_default());
    let result = project.generate(
        str_of(&writing, "model"),
        WRITTEN_SCHEMA,
        &instruction,
        &In::Map(vec![
            ("about", In::Value(request.about.clone())),
            ("rules", In::text(&rules)),
            ("sources", ref_inputs(&refs)),
            (
                "pages",
                In::List(
                    existing
                        .iter()
                        .map(|page| {
                            In::Map(vec![
                                ("id", field(page, "id")),
                                ("title", field(page, "title")),
                                (
                                    "markdown",
                                    In::Units(utf16_prefix(str_of(page, "markdown"), 1200)),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "pages_needing_revision",
                In::Value(ConfigValue::Array(
                    affected.iter().map(|id| text(id)).collect(),
                )),
            ),
        ]),
    )?;
    let [list] = strict(&result, &["pages"]).ok_or_else(model_output)?[..] else {
        return Err(model_output());
    };
    let written = written_pages(list).ok_or_else(model_output)?;
    if written.len() as f64 > crate::eval_stage::number(writing.get("max_pages_per_job")) {
        return Err(coded(
            2,
            "PAGE_LIMIT",
            "The model exceeded max_pages_per_job.",
        ));
    }
    let mut ids: HashSet<String> = HashSet::new();
    let mut changes = Vec::new();
    for item in &written {
        let lower = item.id.to_lowercase();
        if ids.contains(&lower) {
            return Err(coded(
                2,
                "INVALID_PAGE",
                format!("Duplicate page ID: {}", item.id),
            ));
        }
        if previous
            .iter()
            .any(|page| str_of(page, "id") == item.id && truthy(page.get("archived")))
        {
            return Err(coded(
                4,
                "PAGE_ARCHIVED",
                "Restore the archived page before changing it.",
            ));
        }
        ids.insert(lower);
        let (markdown, citations) = verify_facts(&item.facts, &refs)?;
        changes.push(project.change_page(&item.id, &item.title, markdown, citations, &previous)?);
    }
    tracker.progress("validating_citations", Some((0, changes.len())));
    let mut verified = Vec::new();
    for page in changes {
        verified.push(project.attach_originals(&database, page)?);
    }
    let source_ids: Vec<String> = documents
        .iter()
        .map(|document| str_of(&evidence_source(document), "sourceId").to_owned())
        .collect();
    let needs_revision: Vec<ConfigValue> = affected
        .iter()
        .filter(|id| !ids.contains(&id.to_lowercase()))
        .map(|id| text(id))
        .collect();
    let reflected: HashSet<String> = verified
        .iter()
        .flat_map(|page| items(page.get("citations")).to_vec())
        .map(|citation| {
            let source = citation.get("source").cloned().unwrap_or_default();
            str_of(&source, "sourceId").to_owned()
        })
        .collect();
    let extra = vec![
        ("sources", ConfigValue::Number(documents.len() as f64)),
        ("skipped", ConfigValue::Array(skipped)),
        (
            "source_ids",
            ConfigValue::Array(source_ids.iter().map(|id| text(id)).collect()),
        ),
        ("pages_needing_revision", ConfigValue::Array(needs_revision)),
    ];
    commit_pages(
        project,
        connection,
        tracker,
        verified,
        extra,
        |connection| {
            for id in &source_ids {
                if reflected.contains(id) {
                    connection
                        .execute("UPDATE wiki_sources SET reflected = 1 WHERE id = ?1", [id])
                        .map_err(sql)?;
                }
            }
            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_words_as_intl_segmenter() {
        // The words Node.js 22 finds with Intl.Segmenter('ja', { granularity: 'word' }).
        assert_eq!(
            super::words("フィルターは毎月交換する。清掃は毎週行う。"),
            [
                "フィルター",
                "は",
                "毎月",
                "交換",
                "する",
                "清掃",
                "は",
                "毎週",
                "行う"
            ]
        );
        assert_eq!(
            super::words("date: 2026-11-19t00:00:00.000z"),
            ["date", "2026", "11", "19t00", "00", "00.000z"]
        );
    }
}
