//! The commands of Wiki and PARA projects (spec 14–16 and 28.12 step 5a), in the order and with
//! the checks, errors and records of `runWiki` in `project-wiki.mjs`.

// The entry points are called from `node`, which test builds leave out.
#![cfg_attr(test, allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::fs::File;

use rusqlite::Connection;

use crate::config_value::ConfigValue;
use crate::eval_data::js_trim;
use crate::eval_stage::open_state;
use crate::index_stage::{assign, items, object, sql, string_field, text, truthy};
use crate::node_path;
use crate::registry::js_space;
use crate::source_load::sha256_hex;
use crate::source_stage::iso_now;
use crate::transform_stage::StageError;
use crate::wiki_project::{
    Citation, Fact, In, Project, REVISION_SCHEMA, WikiOutcome, WikiRequest, atomic_write, coded,
    current_document, ensure, fs_error, job, json_or_undefined, line_count, model_output, pages,
    read_citation, read_file, read_text, remember, remove, restore, str_of, strict, unique_index,
    write_page, written_pages,
};
use crate::wiki_work::{self, Tracker};
use crate::{Cancellation, KnowledgeBase};

/// A numbered excerpt the model may cite: its text and the evidence it came from.
pub(crate) struct Ref {
    pub id: usize,
    pub text: String,
    pub evidence: ConfigValue,
}

/// `chunksForModel(documents)`: up to 6000 characters of each document, 16000 in all.
pub(crate) fn chunks_for_model(documents: Vec<(String, ConfigValue)>) -> Vec<Ref> {
    let mut budget: i64 = 16_000;
    let mut found: Vec<Ref> = Vec::new();
    for (content, evidence) in documents {
        if budget < 1 {
            break;
        }
        let text: String = content.chars().take(6000.min(budget as usize)).collect();
        if js_trim(&text).is_empty() {
            continue;
        }
        budget -= text.chars().count() as i64;
        found.push(Ref {
            id: found.len() + 1,
            text,
            evidence,
        });
    }
    found
}

fn number_of(value: Option<&ConfigValue>) -> f64 {
    match value {
        Some(ConfigValue::Number(value)) => *value,
        _ => f64::NAN,
    }
}

/// The facts whose citations resolve, for `ingress` (spec 16: only verified statements are saved).
/// A citation whose quote is not found exactly once in its source is dropped, and so is a fact left
/// without citations. Returns the kept facts and, for each dropped citation, the fact and the quote.
pub(crate) fn verified_facts(facts: Vec<Fact>, refs: &[Ref]) -> (Vec<Fact>, Vec<(String, String)>) {
    let mut kept = Vec::new();
    let mut rejected = Vec::new();
    for fact in facts {
        let (valid, invalid): (Vec<Citation>, Vec<Citation>) =
            fact.citations.into_iter().partition(|citation| {
                citation
                    .id
                    .checked_sub(1)
                    .and_then(|index| refs.get(index))
                    .is_some_and(|reference| {
                        unique_index(&reference.text, &citation.quote).is_some()
                    })
            });
        rejected.extend(
            invalid
                .into_iter()
                .map(|citation| (fact.text.clone(), citation.quote)),
        );
        if !valid.is_empty() {
            kept.push(Fact {
                text: fact.text,
                citations: valid,
            });
        }
    }
    (kept, rejected)
}

/// A statement without the source markers such as `[1]` that a model wrote into it: the page adds
/// its own markers, numbered by citation.
pub(crate) fn without_markers(text: &str) -> String {
    static MARKERS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let markers = MARKERS.get_or_init(|| {
        regex::Regex::new(r"[ \t\u{3000}]*(?:\[\d{1,3}\]|［[0-9０-９]{1,3}］)")
            .expect("valid pattern")
    });
    markers.replace_all(text, "").into_owned()
}

/// `verifyFacts(facts, refs)`: the page text with source markers and the citation of each marker.
pub(crate) fn verify_facts(
    facts: &[Fact],
    refs: &[Ref],
) -> Result<(String, Vec<ConfigValue>), StageError> {
    let mut citations: Vec<ConfigValue> = Vec::new();
    let mut paragraphs = Vec::new();
    for fact in facts {
        let mut marks = Vec::new();
        for citation in &fact.citations {
            let reference = refs.get(citation.id - 1);
            let Some((reference, index)) = reference.and_then(|reference| {
                unique_index(&reference.text, &citation.quote).map(|index| (reference, index))
            }) else {
                return Err(coded(
                    2,
                    "INVALID_CITATION",
                    "Every citation must identify a unique verbatim quote in the supplied evidence.",
                ));
            };
            let evidence = &reference.evidence;
            let start = match evidence
                .get("byteRange")
                .and_then(|range| range.get("start"))
            {
                Some(ConfigValue::Number(start)) => *start,
                _ => 0.0,
            };
            let prefix = &reference.text[..index];
            let start_line = number_of(
                evidence
                    .get("range")
                    .and_then(|range| range.get("startLine")),
            ) + prefix.matches('\n').count() as f64;
            let end_line = start_line + line_count(&citation.quote) as f64 - 1.0;
            let mut entries = match evidence {
                ConfigValue::Object(entries) => entries.clone(),
                _ => Vec::new(),
            };
            assign(
                &mut entries,
                "range",
                Some(object(vec![
                    ("startLine", ConfigValue::Number(start_line)),
                    ("endLine", ConfigValue::Number(end_line)),
                ])),
            );
            assign(
                &mut entries,
                "byteRange",
                Some(object(vec![
                    ("start", ConfigValue::Number(start + prefix.len() as f64)),
                    (
                        "end",
                        ConfigValue::Number(start + (prefix.len() + citation.quote.len()) as f64),
                    ),
                ])),
            );
            citations.push(ConfigValue::Object(entries));
            marks.push(format!("[{}]", citations.len()));
        }
        paragraphs.push(format!(
            "{} {}",
            js_trim(&without_markers(&fact.text)),
            marks.join(" ")
        ));
    }
    Ok((paragraphs.join("\n\n"), citations))
}

/// `citedText(database, pages)`: the cited excerpts of pages, each once.
pub(crate) fn cited_text(
    database: &KnowledgeBase,
    selected: &[ConfigValue],
    cancel: &Cancellation,
) -> Result<Vec<Ref>, StageError> {
    let mut order: Vec<String> = Vec::new();
    let mut found: HashMap<String, ConfigValue> = HashMap::new();
    for page in selected {
        for item in items(page.get("citations")) {
            let source = item.get("source").cloned().unwrap_or_default();
            let part = |name: &str| source.get(name).cloned().unwrap_or(ConfigValue::Null);
            let key = ConfigValue::Array(vec![
                part("sourceId"),
                part("objectId"),
                part("version"),
                item.get("byteRange").cloned().unwrap_or(ConfigValue::Null),
            ])
            .to_js_json();
            if !found.contains_key(&key) {
                order.push(key.clone());
            }
            found.insert(key, item.clone());
        }
    }
    let mut documents = Vec::new();
    for key in order {
        let item = found.remove(&key).unwrap_or_default();
        let content = read_citation(database, &item, None, cancel)?;
        documents.push((content, item));
    }
    Ok(chunks_for_model(documents))
}

pub(crate) fn ref_inputs(refs: &[Ref]) -> In {
    In::List(
        refs.iter()
            .map(|reference| {
                In::Map(vec![
                    ("id", In::number(reference.id)),
                    ("text", In::text(&reference.text)),
                ])
            })
            .collect(),
    )
}

/// The project execution lock, as `withExecutionLock` took it.
fn lock(path: Option<&str>) -> Result<Option<File>, StageError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| {
            StageError::native(crate::Error::Io(
                "Could not open the project execution lock",
            ))
        })?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Err(coded(
            4,
            "BUILD_BUSY",
            "Another project operation is running. Retry after it finishes.",
        )),
        Err(std::fs::TryLockError::Error(_)) => Err(StageError::native(crate::Error::Io(
            "Could not acquire the project execution lock",
        ))),
    }
}

/// Whether a command only reads: `--dry-run`, `cite`, `rules`, `progress` and `verify` without
/// `--content`.
pub(crate) fn read_only(command: &str, values: &ConfigValue) -> bool {
    truthy(values.get("dry-run"))
        || ["cite", "rules", "progress"].contains(&command)
        || (command == "verify" && !truthy(values.get("content")))
}

/// Runs a command: as recorded work under the execution lock when it changes the project.
pub fn run(request: &WikiRequest, cancel: &Cancellation) -> Result<WikiOutcome, StageError> {
    let command = request.command.as_str();
    let read_only = read_only(command, &request.values);
    let connection = open_state(&request.state, read_only)?;
    if !read_only {
        ensure(&connection)?;
    }
    let project = Project { request, cancel };
    let mut warnings = Vec::new();
    // The CLI records the commands that change the project; the screen records every instruction.
    let output = if request.track {
        let print =
            request.print_progress && ["ingress", "query", "instruct", "verify"].contains(&command);
        wiki_work::track(
            &request.state,
            request.run_id.as_deref().unwrap_or_default(),
            command,
            project.value_str("page"),
            print,
            |tracker| {
                let _lock = if read_only {
                    None
                } else {
                    lock(request.lock.as_deref())?
                };
                execute(&project, &connection, tracker, &mut warnings)
            },
            ConfigValue::clone,
        )?
    } else {
        execute(&project, &connection, &Tracker::none(), &mut warnings)?
    };
    Ok(WikiOutcome { output, warnings })
}

/// `runWiki(db, project, command, args, values, runId)`
pub(crate) fn execute(
    project: &Project<'_>,
    connection: &Connection,
    tracker: &Tracker,
    warnings: &mut Vec<ConfigValue>,
) -> Result<ConfigValue, StageError> {
    let command = project.request.command.as_str();
    if command == "verify" && pages(connection, true)?.is_empty() {
        return Ok(object(vec![("issues", ConfigValue::Array(Vec::new()))]));
    }
    match command {
        "issue" => issue(project, connection),
        "progress" => {
            let id = project
                .request
                .args
                .first()
                .cloned()
                .flatten()
                .unwrap_or_default();
            wiki_work::status(connection, &id)?
                .ok_or_else(|| coded(3, "JOB_REQUIRED", "No work found for that run ID."))
        }
        "cite" => cite(project, connection),
        "save-answer" => save_answer(project, connection, tracker),
        "undo" => undo(project, connection),
        "archive" => archive(project, connection, tracker),
        "rules" => Ok(object(vec![(
            "content",
            text(&read_file(&project.rules_file())?),
        )])),
        "instruct" => instruct(project, connection, tracker),
        "query" => crate::wiki_ingress::query(project, connection, tracker),
        "verify" => crate::wiki_ingress::verify(project, connection, tracker),
        "ingress" => crate::wiki_ingress::ingress(project, connection, tracker, warnings),
        _ => Err(coded(2, "INVALID_ARGUMENT", "Unknown authoring command.")),
    }
}

pub(crate) fn arg<'a>(project: &'a Project<'_>, index: usize) -> &'a str {
    project
        .request
        .args
        .get(index)
        .and_then(Option::as_deref)
        .unwrap_or_default()
}

/// `commitPages(changes, extra, beforeCommit)`: publishes the pages, then records them and the job
/// in one transaction. A failure restores the published files.
pub(crate) fn commit_pages(
    project: &Project<'_>,
    connection: &Connection,
    tracker: &Tracker,
    changes: Vec<ConfigValue>,
    extra: Vec<(&str, ConfigValue)>,
    before_commit: impl FnOnce(&Connection) -> Result<(), StageError>,
) -> Result<ConfigValue, StageError> {
    tracker.progress("saving_pages", Some((0, changes.len())));
    let prior: HashMap<String, ConfigValue> = pages(connection, true)?
        .into_iter()
        .map(|page| (str_of(&page, "id").to_owned(), page))
        .collect();
    let mut snapshots: Vec<(String, Option<String>)> = Vec::new();
    let outcome = (|| {
        for page in &changes {
            let path = project.publication_path(str_of(page, "id"))?;
            snapshots.push((path.clone(), read_text(&path)?));
            project.save_page(page)?;
        }
        connection.execute_batch("BEGIN IMMEDIATE").map_err(sql)?;
        let written = (|| {
            for page in &changes {
                write_page(connection, page)?;
            }
            before_commit(connection)?;
            let mut entries = vec![
                (
                    "id",
                    text(project.request.run_id.as_deref().unwrap_or_default()),
                ),
                ("action", text(&project.request.command)),
            ];
            entries.extend(extra);
            entries.push((
                "pages",
                ConfigValue::Array(
                    changes
                        .iter()
                        .map(|after| {
                            let before = prior.get(str_of(after, "id"));
                            object(vec![
                                ("id", after.get("id").cloned().unwrap_or_default()),
                                ("title", after.get("title").cloned().unwrap_or_default()),
                                (
                                    "previous",
                                    before
                                        .and_then(|before| before.get("markdown"))
                                        .filter(|value| !matches!(value, ConfigValue::Null))
                                        .cloned()
                                        .unwrap_or(ConfigValue::Null),
                                ),
                                (
                                    "current",
                                    after.get("markdown").cloned().unwrap_or_default(),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ));
            entries.push((
                "previous_pages",
                ConfigValue::Array(
                    changes
                        .iter()
                        .map(|after| {
                            object(vec![
                                ("id", after.get("id").cloned().unwrap_or_default()),
                                (
                                    "before",
                                    prior
                                        .get(str_of(after, "id"))
                                        .cloned()
                                        .unwrap_or(ConfigValue::Null),
                                ),
                                ("after", after.clone()),
                            ])
                        })
                        .collect(),
                ),
            ));
            remember(connection, object(entries))
        })();
        match written {
            Ok(result) => match connection.execute_batch("COMMIT") {
                Ok(()) => Ok(result),
                Err(error) => {
                    let _ = connection.execute_batch("ROLLBACK");
                    Err(sql(error))
                }
            },
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    })();
    if outcome.is_err() {
        restore(std::mem::take(&mut snapshots));
    }
    outcome
}

fn issue(project: &Project<'_>, connection: &Connection) -> Result<ConfigValue, StageError> {
    let id = arg(project, 0);
    let state = arg(project, 1);
    let valid_id = id.len() == 70
        && id.starts_with("issue-")
        && id[6..]
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !valid_id || !["open", "resolved", "dismissed"].contains(&state) {
        return Err(coded(
            2,
            "INVALID_ISSUE",
            "Provide an issue ID followed by open, resolved or dismissed.",
        ));
    }
    connection
        .execute(
            "INSERT INTO wiki_issues VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET state = excluded.state",
            [id, state],
        )
        .map_err(sql)?;
    Ok(object(vec![("id", text(id)), ("state", text(state))]))
}

fn cite(project: &Project<'_>, connection: &Connection) -> Result<ConfigValue, StageError> {
    let id = arg(project, 0);
    let index = crate::wiki_project::marker_index(arg(project, 1));
    let citation = pages(connection, true)?
        .into_iter()
        .find(|page| str_of(page, "id") == id)
        .and_then(|page| index.and_then(|index| items(page.get("citations")).get(index).cloned()));
    let Some(citation) = citation else {
        return Err(coded(
            3,
            "CITATION_REQUIRED",
            "Provide a page ID and one-based citation number.",
        ));
    };
    let database = project.database()?;
    let quote = read_citation(&database, &citation, None, project.cancel)?;
    Ok(object(vec![
        ("evidence", citation),
        ("quote", text(&quote)),
    ]))
}

fn save_answer(
    project: &Project<'_>,
    connection: &Connection,
    tracker: &Tracker,
) -> Result<ConfigValue, StageError> {
    let (job_id, id, title) = (arg(project, 0), arg(project, 1), arg(project, 2));
    let found = job(connection, job_id)?;
    let facts = found
        .as_ref()
        .map(|job| items(job.get("facts")))
        .unwrap_or_default();
    if found.as_ref().and_then(|job| string_field(job, "action")) != Some("query")
        || facts.is_empty()
    {
        return Err(coded(
            3,
            "JOB_REQUIRED",
            "Supply a query run with sourced facts.",
        ));
    }
    let all = pages(connection, true)?;
    if all.iter().any(|page| str_of(page, "id") == id) {
        return Err(coded(4, "PAGE_EXISTS", "Choose an unused page ID."));
    }
    let database = project.database()?;
    let mut citations = Vec::new();
    let mut markdown = Vec::new();
    for fact in facts {
        let mut marks = Vec::new();
        for evidence in items(fact.get("evidence")) {
            citations.push(evidence.clone());
            marks.push(format!("[{}]", citations.len()));
        }
        markdown.push(format!(
            "{} {}",
            without_markers(&crate::eval_stage::js_string(fact.get("text"))),
            marks.join(" ")
        ));
    }
    let page = project.change_page(id, title, markdown.join("\n\n"), citations, &all)?;
    let page = project.attach_originals(&database, page)?;
    commit_pages(
        project,
        connection,
        tracker,
        vec![page],
        vec![("query", text(job_id))],
        |_| Ok(()),
    )
}

fn undo(project: &Project<'_>, connection: &Connection) -> Result<ConfigValue, StageError> {
    let run_id = project.request.run_id.as_deref().unwrap_or_default();
    let found = job(connection, arg(project, 0))?;
    if let Some(rules) = found
        .as_ref()
        .and_then(|job| job.get("rules"))
        .filter(|rules| truthy(Some(rules)))
    {
        let file = project.rules_path()?;
        let before = str_of(rules, "before");
        let after = str_of(rules, "after");
        if read_file(&file)? != after {
            return Err(coded(
                4,
                "RULES_CHANGED",
                "The rules changed after this work.",
            ));
        }
        atomic_write(&file, before.as_bytes())?;
        let job_id = found
            .as_ref()
            .and_then(|job| job.get("id"))
            .cloned()
            .unwrap_or_default();
        return remember(
            connection,
            object(vec![
                ("id", text(run_id)),
                ("action", text("undo")),
                ("job", job_id),
            ]),
        )
        .inspect_err(|_| {
            let _ = atomic_write(&file, after.as_bytes());
        });
    }
    let Some(found) = found.filter(|job| {
        job.get("previous_pages")
            .is_some_and(|value| truthy(Some(value)))
    }) else {
        return Err(coded(
            3,
            "JOB_REQUIRED",
            "The job has no reversible page changes.",
        ));
    };
    let current = pages(connection, true)?;
    let mut changes: Vec<(String, Option<ConfigValue>)> = Vec::new();
    for entry in items(found.get("previous_pages")) {
        let id = str_of(entry, "id");
        let now = current.iter().find(|page| str_of(page, "id") == id);
        if json_or_undefined(now) != json_or_undefined(entry.get("after")) {
            return Err(coded(
                4,
                "PAGE_CHANGED",
                format!("A newer work changed {id}."),
            ));
        }
        let before = entry
            .get("before")
            .filter(|before| truthy(Some(before)))
            .cloned();
        changes.push((id.to_owned(), before));
    }
    let mut snapshots: Vec<(String, Option<String>)> = Vec::new();
    let outcome = (|| {
        if let Some(archive) = found
            .get("archive_file")
            .filter(|value| truthy(Some(value)))
        {
            let path = project.publication_path(str_of(archive, "id"))?;
            snapshots.push((path.clone(), read_text(&path)?));
            match archive.get("before") {
                Some(ConfigValue::String(before)) => atomic_write(&path, before.as_bytes())?,
                Some(ConfigValue::Null) => remove(&path)?,
                Some(other) => atomic_write(&path, other.to_js_json().as_bytes())?,
                None => atomic_write(&path, b"undefined")?,
            }
        }
        for (id, before) in &changes {
            let path = project.publication_path(id)?;
            snapshots.push((path.clone(), read_text(&path)?));
            match before {
                Some(before) => project.save_page(before)?,
                None => remove(&path)?,
            }
        }
        connection.execute_batch("BEGIN IMMEDIATE").map_err(sql)?;
        let written = (|| {
            for (id, before) in &changes {
                match before {
                    Some(before) => write_page(connection, before)?,
                    None => {
                        connection
                            .execute("DELETE FROM wiki_pages WHERE id = ?1", [id])
                            .map_err(sql)?;
                    }
                }
            }
            for source in items(found.get("source_ids")) {
                connection
                    .execute(
                        "UPDATE wiki_sources SET reflected = 0 WHERE id = ?1",
                        [crate::eval_stage::js_string(Some(source))],
                    )
                    .map_err(sql)?;
            }
            remember(
                connection,
                object(vec![
                    ("id", text(run_id)),
                    ("action", text("undo")),
                    ("job", found.get("id").cloned().unwrap_or_default()),
                    (
                        "pages",
                        ConfigValue::Array(changes.iter().map(|(id, _)| text(id)).collect()),
                    ),
                ]),
            )
        })();
        match written {
            Ok(result) => match connection.execute_batch("COMMIT") {
                Ok(()) => Ok(result),
                Err(error) => {
                    let _ = connection.execute_batch("ROLLBACK");
                    Err(sql(error))
                }
            },
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    })();
    if outcome.is_err() {
        restore(snapshots);
    }
    outcome
}

fn archive(
    project: &Project<'_>,
    connection: &Connection,
    tracker: &Tracker,
) -> Result<ConfigValue, StageError> {
    let target = arg(project, 0);
    let all = pages(connection, true)?;
    let Some(page) = all.iter().find(|page| str_of(page, "id") == target) else {
        return Err(coded(
            3,
            "PAGE_REQUIRED",
            format!("Page not found: {target}"),
        ));
    };
    let archived = !truthy(project.value("restore"));
    let ConfigValue::Object(mut next) = page.clone() else {
        return Err(StageError::plain("Invalid page record"));
    };
    assign(&mut next, "archived", Some(ConfigValue::Bool(archived)));
    assign(&mut next, "changed", Some(text(&iso_now())));
    let page_id = str_of(page, "id").to_owned();
    let stored_archive = string_field(page, "archive_id")
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    if project.para() && !page_id.starts_with("Archives/") {
        let archive_id = archived.then(|| format!("Archives/{page_id}"));
        assign(
            &mut next,
            "archive_id",
            archive_id.clone().map(|id| text(&id)),
        );
        if let Some(archive_id) = archive_id.or(stored_archive.clone()) {
            let lower = archive_id.to_lowercase();
            if all.iter().any(|item| {
                str_of(item, "id").to_lowercase() == lower
                    || (string_field(item, "archive_id")
                        .is_some_and(|id| id.to_lowercase() == lower)
                        && str_of(item, "id") != page_id)
            }) {
                return Err(coded(
                    4,
                    "PAGE_EXISTS",
                    "An item uses the archive path for this page.",
                ));
            }
            let archive_path = project.publication_path(&archive_id)?;
            let original = read_text(&archive_path)?;
            if archived && original.is_some() && stored_archive.is_none() {
                return Err(coded(
                    4,
                    "PAGE_EXISTS",
                    "An HTML file already uses that archive path.",
                ));
            }
            let next = ConfigValue::Object(next);
            let outcome = (|| {
                if archived {
                    let ConfigValue::Object(mut copy) = next.clone() else {
                        return Err(StageError::plain("Invalid page record"));
                    };
                    assign(&mut copy, "id", Some(text(&archive_id)));
                    assign(&mut copy, "archive_id", None);
                    project.save_page(&ConfigValue::Object(copy))?;
                } else {
                    remove(&archive_path)?;
                }
                commit_pages(
                    project,
                    connection,
                    tracker,
                    vec![next.clone()],
                    vec![
                        ("page", text(&page_id)),
                        ("archived", ConfigValue::Bool(archived)),
                        (
                            "archive_file",
                            object(vec![
                                ("id", text(&archive_id)),
                                (
                                    "before",
                                    original.as_deref().map_or(ConfigValue::Null, text),
                                ),
                            ]),
                        ),
                    ],
                    |_| Ok(()),
                )
            })();
            if outcome.is_err() {
                let _ = match &original {
                    None => remove(&archive_path),
                    Some(content) => atomic_write(&archive_path, content.as_bytes()),
                };
            }
            return outcome;
        }
    }
    commit_pages(
        project,
        connection,
        tracker,
        vec![ConfigValue::Object(next)],
        vec![
            ("page", text(&page_id)),
            ("archived", ConfigValue::Bool(archived)),
        ],
        |_| Ok(()),
    )
}

/// `args[1].replace(/\s+/gu, ' ').trim()`
fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if js_space(c) {
            space = true;
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    js_trim(&out).to_owned()
}

fn instruct(
    project: &Project<'_>,
    connection: &Connection,
    tracker: &Tracker,
) -> Result<ConfigValue, StageError> {
    let run_id = project.request.run_id.as_deref().unwrap_or_default();
    let instruction = arg(project, 1);
    let writing = items(project.request.plan.get("ingress"))
        .get(2)
        .cloned()
        .unwrap_or_default();
    if truthy(project.value("persistent")) {
        let file = project.rules_path()?;
        let before = read_file(&file)?;
        let after = format!(
            "{}\n\n- {}\n",
            before.trim_end_matches(js_space),
            one_line(instruction)
        );
        atomic_write(&file, after.as_bytes())?;
        return remember(
            connection,
            object(vec![
                ("id", text(run_id)),
                ("action", text(&project.request.command)),
                (
                    "rules",
                    object(vec![("before", text(&before)), ("after", text(&after))]),
                ),
            ]),
        )
        .inspect_err(|_| {
            let _ = atomic_write(&file, before.as_bytes());
        });
    }
    let prior = pages(connection, true)?;
    let target = arg(project, 0);
    let Some(page) = prior
        .iter()
        .find(|page| str_of(page, "id") == target)
        .filter(|page| !truthy(page.get("archived")))
    else {
        return Err(coded(
            3,
            "PAGE_REQUIRED",
            format!("Select an active page: {target}"),
        ));
    };
    let page = page.clone();
    let rules = read_file(&project.rules_file())?;
    let database = project.database()?;
    let mut refs = cited_text(&database, std::slice::from_ref(&page), project.cancel)?;
    let supplement = project
        .value_str("supplement")
        .filter(|text| !text.is_empty());
    let by = project.value_str("by");
    if let Some(supplement) = supplement
        && (js_trim(supplement).is_empty() || by.is_none_or(|by| js_trim(by).is_empty()))
    {
        return Err(coded(
            2,
            "AUTHOR_REQUIRED",
            "Set --by when recording a supplement.",
        ));
    }
    let source_id = format!("supplement-{}", run_id.get(4..).unwrap_or_default());
    let document = supplement.map(|supplement| {
        format!(
            "Contributor: {}\nDate: {}\n\n{supplement}",
            by.unwrap_or("undefined"),
            iso_now()
        )
    });
    let mut model_refs: Vec<In> = refs
        .iter()
        .map(|reference| {
            In::Map(vec![
                ("id", In::number(reference.id)),
                ("text", In::text(&reference.text)),
            ])
        })
        .collect();
    if let Some(content) = &document {
        model_refs.push(In::Map(vec![
            ("id", In::number(refs.len() + 1)),
            ("text", In::text(content)),
        ]));
    }
    tracker.progress("writing_pages", None);
    let proposal = project.generate(
        str_of(&writing, "model"),
        REVISION_SCHEMA,
        "Revise the target page based on the user instruction and existing rules. Keep fact citations to exact quotes from numbered sources. If the instruction contradicts the originals, return the conflict and no pages unless an explicit supplement is supplied. When using a supplement, clearly attribute the supplement to the contributor and preserve conflicting material as distinct. Keep the target page ID. Additional page IDs may be used for an explicitly requested split.",
        &In::Map(vec![
            ("about", In::Value(project.request.about.clone())),
            ("rules", In::text(&rules)),
            ("instruction", In::text(instruction)),
            ("target", In::text(project.value_str("target").unwrap_or("page"))),
            (
                "page",
                In::Map(vec![
                    ("id", In::Value(page.get("id").cloned().unwrap_or_default())),
                    ("title", In::Value(page.get("title").cloned().unwrap_or_default())),
                    ("markdown", In::Value(page.get("markdown").cloned().unwrap_or_default())),
                ]),
            ),
            ("sources", In::List(model_refs)),
            ("supplement", In::Value(ConfigValue::Bool(document.is_some()))),
        ]),
    )?;
    let [conflict, proposed] =
        strict(&proposal, &["conflict", "pages"]).ok_or_else(model_output)?[..]
    else {
        return Err(model_output());
    };
    let conflict = conflict.as_str().ok_or_else(model_output)?.to_owned();
    let proposed = written_pages(proposed).ok_or_else(model_output)?;
    if !conflict.is_empty() && document.is_none() {
        return Ok(object(vec![
            ("conflict", text(&conflict)),
            ("requires", text("--supplement <text> --by <name>")),
        ]));
    }
    let page_id = str_of(&page, "id").to_owned();
    let max_pages = crate::eval_stage::number(writing.get("max_pages_per_job"));
    let ids: HashSet<String> = proposed.iter().map(|item| item.id.to_lowercase()).collect();
    if proposed.len() as f64 > max_pages
        || ids.len() != proposed.len()
        || (!proposed.is_empty() && !proposed.iter().any(|item| item.id == page_id))
        || proposed.iter().any(|item| {
            item.id != page_id
                && prior
                    .iter()
                    .any(|existing| str_of(existing, "id") == item.id)
        })
    {
        return Err(coded(
            2,
            "INVALID_PAGE",
            "The revision contains an invalid set of pages.",
        ));
    }
    let mut source: Option<ConfigValue> = None;
    let supplement_id = refs.len() + 1;
    if let Some(content) = &document
        && proposed.iter().any(|item| {
            item.facts.iter().any(|fact| {
                fact.citations
                    .iter()
                    .any(|citation| citation.id == supplement_id)
            })
        })
    {
        let all_facts: Vec<Fact> = proposed
            .iter()
            .flat_map(|item| item.facts.iter())
            .map(|fact| Fact {
                text: fact.text.clone(),
                citations: fact
                    .citations
                    .iter()
                    .map(|citation| crate::wiki_project::Citation {
                        id: citation.id,
                        quote: citation.quote.clone(),
                    })
                    .collect(),
            })
            .collect();
        let mut checked: Vec<Ref> = refs
            .iter()
            .map(|reference| Ref {
                id: reference.id,
                text: reference.text.clone(),
                evidence: reference.evidence.clone(),
            })
            .collect();
        checked.push(Ref {
            id: supplement_id,
            text: content.clone(),
            evidence: object(vec![
                (
                    "range",
                    object(vec![("startLine", ConfigValue::Number(1.0))]),
                ),
                (
                    "byteRange",
                    object(vec![("start", ConfigValue::Number(0.0))]),
                ),
            ]),
        });
        verify_facts(&all_facts, &checked)?;
        let digest = save_shared_original(project, content.as_bytes())?;
        let input = object(vec![
            ("objectId", text(&format!("{source_id}.txt"))),
            ("url", text(&format!("urn:knowledge-nest:{source_id}"))),
            ("format", text("text")),
            ("content", text(content)),
            ("corpus", object(vec![("originalSha256", text(&digest))])),
            ("chunks", chunks(content, false)?),
        ]);
        replace_documents(&database, &source_id, vec![input], project.cancel)?;
        let saved = current_document(
            &database,
            &object(vec![
                ("sourceId", text(&source_id)),
                ("objectId", text(&format!("{source_id}.txt"))),
            ]),
            project.cancel,
        )?
        .ok_or_else(|| StageError::plain("Cannot read properties of null (reading 'evidence')"))?;
        let evidence = saved.get("evidence").cloned().unwrap_or_default();
        source = evidence.get("source").cloned();
        refs.push(Ref {
            id: supplement_id,
            text: content.clone(),
            evidence,
        });
    }
    tracker.progress("validating_citations", None);
    let mut changes = Vec::new();
    for item in &proposed {
        let (markdown, citations) = verify_facts(&item.facts, &refs)?;
        let changed = project.change_page(&item.id, &item.title, markdown, citations, &prior)?;
        changes.push(project.attach_originals(&database, changed)?);
    }
    let mut extra = vec![("target", text(&page_id))];
    if let Some(source) = &source {
        extra.push(("supplement", source.clone()));
    }
    let record = source.clone();
    commit_pages(
        project,
        connection,
        tracker,
        changes,
        extra,
        move |connection| {
            if let Some(source) = record {
                connection
                    .execute(
                        "INSERT INTO wiki_sources VALUES (?1, ?2, 1)",
                        [&source_id, &source.to_js_json()],
                    )
                    .map_err(sql)?;
            }
            Ok(())
        },
    )
}

/// Saves a supplement as a shared original (`prepareDocument` with `sharedOriginals`) and returns
/// its SHA-256.
fn save_shared_original(project: &Project<'_>, bytes: &[u8]) -> Result<String, StageError> {
    let parent = node_path::join(&project.request.artifacts, &["originals", "shared"]);
    std::fs::create_dir_all(&parent).map_err(|error| fs_error("mkdir", &parent, &error))?;
    let digest = sha256_hex(bytes);
    let path = node_path::join(&parent, &[&format!("{digest}.bin")]);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => atomic_write(&path, bytes)?,
        Err(error) => return Err(fs_error("lstat", &path, &error)),
        Ok(entry) if !entry.is_file() || entry.file_type().is_symlink() => {
            return Err(StageError::plain(format!(
                "Expected a regular file at {path}"
            )));
        }
        Ok(_) => {
            let existing = std::fs::read(&path).map_err(|error| fs_error("open", &path, &error))?;
            if existing != bytes {
                return Err(StageError::plain(
                    "Stored original file does not match its SHA-256 digest",
                ));
            }
        }
    }
    Ok(digest)
}

/// The chunks the Wiki keyword index stores: 800 characters with 100 of overlap, NFKC search text.
pub(crate) fn chunks(content: &str, markdown: bool) -> Result<ConfigValue, StageError> {
    let request = crate::chunk::ChunkRequest {
        documents: vec![crate::chunk::ChunkDocument {
            content: content.to_owned(),
            markdown,
            boundaries: Vec::new(),
        }],
        size: 800,
        overlap: 100,
        structure: false,
        nfkc: true,
    };
    let mut chunked = crate::chunk::chunk_documents(&request, &Cancellation::default())
        .map_err(StageError::native)?;
    Ok(crate::wiki_project::ordered(
        chunked.pop().unwrap_or_default(),
    ))
}

/// `database.replaceDocuments(sourceId, documents)`
pub(crate) fn replace_documents(
    database: &KnowledgeBase,
    source_id: &str,
    documents: Vec<ConfigValue>,
    cancel: &Cancellation,
) -> Result<(), StageError> {
    let inputs: Vec<crate::source::DocumentInput> =
        serde_json::from_str(&ConfigValue::Array(documents).to_js_json())
            .map_err(|_| StageError::native(crate::Error::Invalid("Invalid source documents")))?;
    database
        .replace_document_inputs(source_id, inputs, cancel)
        .map(|_| ())
        .map_err(StageError::native)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(text: &str, quotes: &[(usize, &str)]) -> Fact {
        Fact {
            text: text.to_owned(),
            citations: quotes
                .iter()
                .map(|(id, quote)| Citation {
                    id: *id,
                    quote: (*quote).to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn removes_the_markers_a_model_wrote() {
        assert_eq!(without_markers("緊急停止 [1]"), "緊急停止");
        assert_eq!(
            without_markers("交換は2年 [1][2]ごと［３］。"),
            "交換は2年ごと。"
        );
        assert_eq!(without_markers("配列 a[i] は残す"), "配列 a[i] は残す");
    }

    #[test]
    fn keeps_the_facts_that_quote_their_sources() {
        let refs = [Ref {
            id: 1,
            text: "| A | 2 |\n| B | 3 |\nA\nA\n".to_owned(),
            evidence: ConfigValue::Null,
        }];
        let (kept, rejected) = verified_facts(
            vec![
                fact("A is 2.", &[(1, "| A | 2 |")]),
                fact("B is 3.", &[(1, "| A | 2 |\n| C | 3 |")]),
                fact("Both.", &[(1, "| B | 3 |"), (2, "x"), (0, "y")]),
                fact("Twice.", &[(1, "A\nA")]),
            ],
            &refs,
        );
        let kept: Vec<(&str, usize)> = kept
            .iter()
            .map(|fact| (fact.text.as_str(), fact.citations.len()))
            .collect();
        assert_eq!(kept, [("A is 2.", 1), ("Both.", 1), ("Twice.", 1)]);
        let rejected: Vec<&str> = rejected.iter().map(|(fact, _)| fact.as_str()).collect();
        assert_eq!(rejected, ["B is 3.", "Both.", "Both."]);
        assert!(
            verify_facts(
                &verified_facts(vec![fact("B", &[(1, "| C |")])], &refs).0,
                &refs
            )
            .is_ok()
        );
    }
}
