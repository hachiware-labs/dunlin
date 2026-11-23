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
    Ref, arg, chunks, chunks_for_model, commit_pages, replace_documents, without_markers,
};
use crate::wiki_project::{
    ANSWER_SCHEMA, CHOSEN_SCHEMA, In, Project, WRITTEN_SCHEMA, atomic_write, coded,
    current_document, facts, fs_error, json_or_undefined, link_targets, linked_page,
    merged_sources, model_output, page_sources, pages, read_file, realpath, remember, source_key,
    str_of, strict, written_pages,
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
    let mut ranked: Vec<(ConfigValue, usize)> = pages(
        project.request,
        connection,
        truthy(settings.get("include_archived")),
    )?
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
            ("question", text(question)),
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
            // The page that holds the quote. A small model gives the number of a source marker
            // of the page for the number of the page: then the one page with the quote is meant.
            let holds = |index: usize| {
                let page = ranked.get(index)?;
                crate::wiki_commands::settled_quote(str_of(page, "markdown"), &citation.quote)
                    .map(|quote| (index, quote))
            };
            let resolved = holds(citation.id - 1).or_else(|| {
                let mut holders = (0..ranked.len()).filter_map(holds);
                let only = holders.next()?;
                holders.next().is_none().then_some(only)
            });
            let Some((index, _)) = resolved else {
                return Err(coded(
                    2,
                    "INVALID_CITATION",
                    "The answer cited an unresolved page quote.",
                ));
            };
            if !cited.contains(&index) {
                cited.push(index);
            }
            for entry in page_sources(&ranked[index]) {
                if !evidence
                    .iter()
                    .any(|known| source_key(known) == source_key(entry))
                {
                    evidence.push(entry.clone());
                }
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
        ("question", text(question)),
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
                        ("sources", get("sources")),
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
    let material = pages(project.request, connection, true)?;
    let mut issues: Vec<ConfigValue> = Vec::new();
    let mut review: Option<ConfigValue> = None;
    let known: HashSet<&str> = material.iter().map(|page| str_of(page, "id")).collect();
    let mut incoming: HashSet<String> = HashSet::new();
    // The pages each page links to.
    let mut linked: HashMap<&str, HashSet<String>> = HashMap::new();
    let only = project.value_str("page").filter(|page| !page.is_empty());
    let mut suggestions: Option<ConfigValue> = None;
    let said = |page: &ConfigValue, what: &str| text(&format!("{}：{what}", str_of(page, "id")));
    let database = project.trigram_database()?;
    // What a page may also take its numbers and quotations from: the rules, as the goal and the
    // deadline of a project, and the names of its sources.
    let rules_text = read_file(&project.rules_file()).unwrap_or_default();
    for (completed, page) in material.iter().enumerate() {
        tracker.progress("checking_citations", Some((completed, material.len())));
        let page_id = page.get("id").cloned().unwrap_or_default();
        let mut texts: Vec<String> = Vec::new();
        for entry in page_sources(page) {
            let source = entry.get("source").cloned().unwrap_or_default();
            let current = if source_key(entry).is_empty() {
                None
            } else {
                current_document(&database, &source, project.cancel)?
            };
            let Some(current) = current else {
                issues.push(object(vec![
                    ("code", text("SOURCE_MISSING")),
                    ("page", page_id.clone()),
                    ("source", entry.clone()),
                    (
                        "description",
                        said(
                            page,
                            &format!(
                                "出典の資料が見つかりません（{}）",
                                str_of(&source, "objectId")
                            ),
                        ),
                    ),
                ]));
                continue;
            };
            texts.push(str_of(&current, "content").to_owned());
            let current_version = current
                .get("evidence")
                .and_then(|evidence| evidence.get("source"))
                .and_then(|source| source.get("version"));
            if !truthy(page.get("archived"))
                && json_or_undefined(current_version) != json_or_undefined(source.get("version"))
            {
                issues.push(object(vec![
                    ("code", text("STALE_VERSION")),
                    ("page", page_id.clone()),
                    ("source", entry.clone()),
                    (
                        "description",
                        said(
                            page,
                            &format!(
                                "出典の資料が新しくなっています（{}）",
                                str_of(&source, "objectId")
                            ),
                        ),
                    ),
                ]));
            }
        }
        // Numbers, dates and quotations are written as the sources write them.
        if !truthy(page.get("archived")) {
            texts.push(rules_text.clone());
            texts.extend(page_sources(page).iter().map(|entry| {
                str_of(
                    &entry.get("source").cloned().unwrap_or_default(),
                    "objectId",
                )
                .to_owned()
            }));
            for literal in unsupported_literals(str_of(page, "markdown"), &texts) {
                issues.push(object(vec![
                    ("code", text("UNSUPPORTED_LITERAL")),
                    ("page", page_id.clone()),
                    ("literal", text(&literal)),
                    (
                        "description",
                        said(page, &format!("「{literal}」が出典の資料に見つかりません")),
                    ),
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
                linked
                    .entry(str_of(page, "id"))
                    .or_default()
                    .insert(id.clone());
                incoming.insert(id);
            } else {
                issues.push(object(vec![
                    ("code", text("BROKEN_LINK")),
                    ("page", page_id.clone()),
                    ("target", text(target)),
                    (
                        "description",
                        said(page, &format!("リンク先のページがありません（{target}）")),
                    ),
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
        if only.is_none() {
            tracker.progress("suggesting_next", None);
            let (found, suggested) = crate::wiki_inspect::suggest(project, connection, &material)?;
            issues.extend(found);
            suggestions = Some(suggested);
        }
    }
    drop(database);
    // A page of a Wiki is in one of the categories of the rules.
    if !project.para() {
        let listed = crate::wiki_project::rule_categories(
            &read_file(&project.rules_file()).unwrap_or_default(),
        );
        for page in material.iter().filter(|page| !truthy(page.get("archived"))) {
            let id = str_of(page, "id");
            let category = str_of(page, "category").trim();
            if category.is_empty() {
                issues.push(object(vec![
                    ("code", text("NO_CATEGORY")),
                    ("page", text(id)),
                    ("description", text(&format!("{id}：分類がありません"))),
                ]));
            } else if !listed.is_empty() && !listed.iter().any(|known| known == category) {
                issues.push(object(vec![
                    ("code", text("UNKNOWN_CATEGORY")),
                    ("page", text(id)),
                    ("category", text(category)),
                    (
                        "description",
                        text(&format!(
                            "{id}：分類「{category}」は決まり（rules.md）の分類にありません"
                        )),
                    ),
                ]));
            }
        }
    }
    // A note of PARA is in a project or an area the rules list, or in a topic of Resources; a
    // project whose deadline has passed may be done.
    if project.para() {
        let rules = read_file(&project.rules_file()).unwrap_or_default();
        let projects = crate::wiki_project::rule_items(&rules, "Projects");
        let areas = crate::wiki_project::rule_items(&rules, "Areas");
        let listed =
            |list: &[(String, String)], name: &str| list.iter().any(|(known, _)| known == name);
        for page in material.iter().filter(|page| !truthy(page.get("archived"))) {
            let id = str_of(page, "id");
            let parts: Vec<&str> = id.split('/').collect();
            let (code, what) = match parts[..] {
                ["Projects" | "Areas" | "Resources", _] => (
                    "NO_PLACE",
                    "どの取り組み・領域・話題のノートかが ID にありません".to_owned(),
                ),
                ["Projects", name, _, ..] if !listed(&projects, name) => (
                    "UNKNOWN_PROJECT",
                    format!("取り組み「{name}」は決まり（rules.md）の Projects にありません"),
                ),
                ["Areas", name, _, ..] if !listed(&areas, name) => (
                    "UNKNOWN_AREA",
                    format!("領域「{name}」は決まり（rules.md）の Areas にありません"),
                ),
                _ => continue,
            };
            issues.push(object(vec![
                ("code", text(code)),
                ("page", text(id)),
                ("description", text(&format!("{id}：{what}"))),
            ]));
        }
        let today: String = iso_now().chars().take(10).collect();
        static DATE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let date = DATE.get_or_init(|| regex::Regex::new(r"\d{4}-\d{2}-\d{2}").expect("valid"));
        for (name, about) in &projects {
            let Some(deadline) = date.find(about).map(|found| found.as_str()) else {
                continue;
            };
            if deadline < today.as_str() {
                issues.push(object(vec![
                    ("code", text("PROJECT_OVERDUE")),
                    ("project", text(name)),
                    ("deadline", text(deadline)),
                    (
                        "description",
                        text(&format!(
                            "Projects/{name}：期限（{deadline}）を過ぎています。終わっていれば nest archive Projects/{name} で Archives へ移します"
                        )),
                    ),
                ]));
            }
        }
    }
    // A page that names another page without linking to it.
    let active: Vec<&ConfigValue> = material
        .iter()
        .filter(|page| !truthy(page.get("archived")))
        .collect();
    let folded = |text: &str| nfkc(text).to_lowercase();
    let titles: Vec<String> = active
        .iter()
        .map(|page| folded(js_trim(str_of(page, "title"))))
        .collect();
    for page in &active {
        let id = str_of(page, "id");
        let body = folded(str_of(page, "markdown"));
        for (other, title) in active.iter().zip(&titles) {
            let target = str_of(other, "id");
            if target == id
                || title.chars().count() < 2
                || !body.contains(title.as_str())
                || linked.get(id).is_some_and(|pages| pages.contains(target))
            {
                continue;
            }
            issues.push(object(vec![
                ("code", text("MISSING_LINK")),
                ("page", text(id)),
                ("target", text(target)),
                (
                    "description",
                    said(
                        page,
                        &format!(
                            "「{}」に触れていますが、そのページ（{target}）へのリンクがありません",
                            js_trim(str_of(other, "title"))
                        ),
                    ),
                ),
            ]));
        }
    }
    for page in &material {
        if !truthy(page.get("archived")) && !incoming.contains(str_of(page, "id")) {
            issues.push(object(vec![
                ("code", text("ORPHAN_PAGE")),
                ("page", page.get("id").cloned().unwrap_or_default()),
                (
                    "description",
                    said(page, "どのページからもリンクされていません"),
                ),
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
    if let Some(suggestions) = suggestions {
        result.push(("suggestions", suggestions));
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

/// The excerpts the model may cite, each with the name of its source: the web address it was
/// read from, or its file name. The model names a page about one source after it.
fn named_sources(refs: &[Ref]) -> In {
    In::List(
        refs.iter()
            .map(|reference| {
                let source = reference.evidence.get("source");
                let field = |name: &str| {
                    source
                        .and_then(|source| string_field(source, name))
                        .unwrap_or_default()
                };
                let url = field("url");
                let name = if url.starts_with("http://") || url.starts_with("https://") {
                    url
                } else {
                    field("objectId")
                };
                In::Map(vec![
                    ("id", In::number(reference.id)),
                    ("name", In::text(name)),
                    ("text", In::text(&reference.text)),
                ])
            })
            .collect(),
    )
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
    if project
        .value_str("fetch")
        .is_some_and(|method| !["browser", "http"].contains(&method))
    {
        return Err(coded(
            2,
            "INVALID_ARGUMENT",
            "Use --fetch browser or --fetch http.",
        ));
    }
    // How many times a page in the browser is scrolled for more: the command, else the settings.
    let scroll = match project.value_str("scroll") {
        Some(given) => given
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|count| *count <= 500)
            .ok_or_else(|| {
                coded(
                    2,
                    "INVALID_ARGUMENT",
                    "Use --scroll with a whole number from 0 to 500.",
                )
            })?,
        None => match loader.get("scroll") {
            Some(ConfigValue::Number(count)) => *count as u32,
            _ => 0,
        },
    };
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
    // Files downloaded from web addresses, with the address each stands for.
    let mut fetched: HashMap<String, String> = HashMap::new();
    // The folder that the files of a folder given as input are in: its name, as `manuals`.
    let mut folder_of: HashMap<String, String> = HashMap::new();
    let mut addresses = 0;
    // How each web address was taken: the browser and its version, or http.
    let mut taken_with: Vec<ConfigValue> = Vec::new();
    // The pages and posts read because a post of X refers to them, with the post.
    let mut followed: Vec<ConfigValue> = Vec::new();
    // Posts of X take the pages they link to and the posts they quote unless told not to.
    let follow = !truthy(project.value("no-follow"))
        && !matches!(loader.get("follow"), Some(ConfigValue::Bool(false)));
    // An address that is a source already: it has the source ID that the address gives below.
    let known = |address: &str| {
        let id = format!(
            "source-{}",
            &sha256_hex(format!("url:{address}").as_bytes())[..24]
        );
        connection
            .query_row("SELECT 1 FROM wiki_sources WHERE id = ?1", [id], |_| Ok(()))
            .is_ok()
    };
    for input in request.args.iter().flatten() {
        if crate::wiki_fetch::is_web_address(input) {
            if dry_run {
                crate::wiki_fetch::normalize(input)?;
                addresses += 1;
                continue;
            }
            // Pages open in the Chrome or Edge of the machine, in the profile of nest, unless
            // the settings or the command say to download them.
            let how = crate::wiki_fetch::Method {
                method: project
                    .value_str("fetch")
                    .or_else(|| string_field(&loader, "fetch"))
                    .unwrap_or("browser"),
                browser: string_field(&loader, "browser").unwrap_or("auto"),
                cwd: &request.cwd,
                home: &request.home,
                scroll,
                follow,
            };
            let download =
                crate::wiki_fetch::fetch(input, &request.root, &request.artifacts, &how, &known)?;
            for warning in &download.warnings {
                warnings.push(object(vec![
                    ("code", text("FETCH_WARNING")),
                    ("message", text(&format!("{}: {warning}", download.url))),
                ]));
            }
            let mut taken = vec![
                ("url", text(&download.url)),
                ("with", text(&download.with)),
            ];
            if let Some(posts) = download.posts {
                taken.push(("posts", ConfigValue::Number(posts as f64)));
            }
            if let Some(scrolled) = &download.scrolled {
                taken.push((
                    "scrolled",
                    object(vec![
                        ("steps", ConfigValue::Number(f64::from(scrolled.steps))),
                        ("items", ConfigValue::Number(scrolled.items as f64)),
                    ]),
                ));
            }
            taken_with.push(object(taken));
            for kept in download.sources {
                if let Some(from) = &kept.from {
                    followed.push(object(vec![
                        ("url", text(&kept.url)),
                        ("from", text(from)),
                        ("with", text(&kept.with)),
                    ]));
                }
                fetched.insert(kept.file.clone(), kept.url.clone());
                add(&mut files, kept.file, kept.url);
            }
            continue;
        }
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
            let name = std::path::Path::new(&root)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            for file in listed {
                folder_of.insert(file.clone(), name.clone());
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
    // A file from outside raw/ is copied into it and read from there: raw/ holds every source.
    // It stays the source of its first path, so a changed file is a new version of it.
    let mut origins: HashMap<String, String> = HashMap::new();
    if !dry_run && let Some(raw) = crate::wiki_fetch::raw_folder(&request.root) {
        let raw = realpath(&raw)?;
        for (file, object_id) in &mut files {
            let within = node_path::relative(&raw, file);
            let outside = within == ".."
                || within.starts_with("../")
                || within.starts_with("..\\")
                || node_path::is_absolute(&within);
            if !outside || fetched.contains_key(file.as_str()) {
                continue;
            }
            let bytes = std::fs::read(&*file).map_err(|error| fs_error("open", file, &error))?;
            let mut folders: Vec<String> =
                folder_of.get(file.as_str()).cloned().into_iter().collect();
            let mut parts: Vec<&str> = object_id.split('/').collect();
            let name = parts.pop().unwrap_or_default().to_owned();
            folders.extend(parts.into_iter().map(str::to_owned));
            let copy = crate::wiki_fetch::keep_source(
                &request.root,
                &request.artifacts,
                &folders,
                &name,
                &bytes,
            )?;
            // The copy keeps the time the file was last changed: it is when the source was written.
            let changed = std::fs::metadata(&*file)
                .and_then(|metadata| metadata.modified())
                .map_err(|error| fs_error("stat", file, &error))?;
            std::fs::File::options()
                .write(true)
                .open(&copy)
                .and_then(|handle| handle.set_modified(changed))
                .map_err(|error| fs_error("utime", &copy, &error))?;
            origins.insert(copy.clone(), std::mem::replace(file, copy));
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
            let key = match (fetched.get(file), upload) {
                (Some(url), _) => format!("url:{url}"),
                (None, Some(name)) => format!("browser:{}", nfkc(name)),
                (None, None) => origins.get(file).unwrap_or(file).clone(),
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
            (
                "files",
                ConfigValue::Number((files.len() + addresses) as f64),
            ),
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
    for (index, (id, file, _)) in grouped.iter().enumerate() {
        let Some(mut document) = by_index.remove(&index) else {
            continue;
        };
        // A downloaded page links to its address rather than to the saved copy.
        if let (Some(url), ConfigValue::Object(entries)) = (fetched.get(file), &mut document) {
            assign(entries, "url", Some(text(url)));
        }
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
        let mut entries = vec![
            ("id", text(request.run_id.as_deref().unwrap_or_default())),
            ("action", text(&request.command)),
            ("sources", ConfigValue::Number(documents as f64)),
            ("pages", ConfigValue::Array(Vec::new())),
            ("skipped", ConfigValue::Array(skipped.to_vec())),
        ];
        if !taken_with.is_empty() {
            entries.push(("fetched", ConfigValue::Array(taken_with.clone())));
        }
        if !followed.is_empty() {
            entries.push(("followed", ConfigValue::Array(followed.clone())));
        }
        remember(connection, object(entries))
    };
    if source_only {
        return finished(connection, documents.len(), &skipped);
    }
    let refs = chunks_for_model(
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
    let existing = pages(request, connection, false)?;
    let previous = pages(request, connection, true)?;
    // A source of a page that this run read in another version.
    let stale = |entry: &ConfigValue| {
        let source = entry.get("source").cloned().unwrap_or_default();
        documents.iter().any(|document| {
            let current = evidence_source(document);
            source.get("sourceId") == current.get("sourceId")
                && json_or_undefined(source.get("version"))
                    != json_or_undefined(current.get("version"))
        })
    };
    let affected: Vec<String> = existing
        .iter()
        .filter(|page| page_sources(page).iter().any(&stale))
        .map(|page| str_of(page, "id").to_owned())
        .collect();
    let field =
        |page: &ConfigValue, name: &str| In::Value(page.get(name).cloned().unwrap_or_default());
    // Whether the pages a source adds to are written again, or only gain text. A model that
    // writes well rewrites them; a small local one copies a page it is shown, so it adds.
    let rewrite = match str_of(&writing, "update") {
        "rewrite" => true,
        "append" => false,
        _ => project.writer(str_of(&writing, "model"))?.provider != "ollama",
    };
    // The pages the model is given in full and returns whole: those written from a source that
    // changed, and, when it rewrites, those it names as the ones the sources add to. A note of
    // PARA is about one source, so no other note is chosen.
    let mut revision = affected.clone();
    if rewrite && !project.para() {
        let others: Vec<&ConfigValue> = existing
            .iter()
            .filter(|page| !affected.iter().any(|id| id == str_of(page, "id")))
            .collect();
        if !others.is_empty() {
            tracker.progress("choosing_pages", None);
            let result = project.generate(
                str_of(&writing, "model"),
                CHOSEN_SCHEMA,
                "Read the list of pages of the wiki and the new sources. Name by ID the existing pages that the sources add to, change or contradict. Return an empty list if there is none.",
                &In::Map(vec![
                    ("about", In::Value(request.about.clone())),
                    ("sources", named_sources(&refs)),
                    (
                        "pages",
                        In::List(
                            others
                                .iter()
                                .map(|page| {
                                    In::Map(vec![
                                        ("id", field(page, "id")),
                                        ("title", field(page, "title")),
                                        ("summary", In::Units(crate::wiki_project::utf16_prefix(str_of(page, "markdown"), 200))),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ]),
            )?;
            let [ConfigValue::Array(chosen)] =
                strict(&result, &["update"]).ok_or_else(model_output)?[..]
            else {
                return Err(model_output());
            };
            let limit = crate::eval_stage::number(writing.get("max_pages_per_job"));
            for id in chosen {
                let id = id.as_str().ok_or_else(model_output)?;
                if (revision.len() as f64) < limit
                    && others.iter().any(|page| str_of(page, "id") == id)
                    && !revision.iter().any(|known| known == id)
                {
                    revision.push(id.to_owned());
                }
            }
        }
    }
    tracker.progress("writing_pages", None);
    let instruction = format!(
        "Write in the language of the project and follow the rules. Write each page in Markdown from the numbered sources, and give the numbers of the sources it is written from; do not list the sources in the text, as the page lists them. State only what the sources say, and copy numbers, dates, names and quotations exactly as the sources write them. Link related pages as [title](/page-id). Where a source contradicts a page, keep both and say that they disagree. {}Return no pages when the sources add nothing new to the wiki or are outside its purpose. {}",
        if !revision.is_empty() {
            "A page listed in pages_needing_revision is given in full: return the whole page, revised with what the sources add or change, keeping what still holds. For any other existing page, return only the new text to add to it. "
        } else if !existing.is_empty() {
            "For an existing page, return only the new text to add to it. "
        } else {
            ""
        },
        if project.para() {
            "Write one note for each source that is of use, as the rules say, at one place only. Its ID is the place where the source will be used: one of places, or Resources/<a new topic> when none fits; a project before an area, an area before a topic. The note is named after its source, and its title says what the source is. Leave the category empty."
        } else {
            "Give each page the category it belongs to: one of the categories in the rules, or, if none fits, a short name of a broad subject that other pages can share (not the kind of page). Page IDs are names without folders."
        }
    );
    // The places of PARA a note can go to: the projects and areas of the rules, and the topics
    // of Resources so far.
    let places: Vec<ConfigValue> = ["Projects", "Areas", "Resources"]
        .iter()
        .flat_map(|top| {
            crate::wiki_project::rule_items(&rules, top)
                .into_iter()
                .map(move |(name, _)| text(&format!("{top}/{name}")))
        })
        .collect();
    let result = project.generate(
        str_of(&writing, "model"),
        WRITTEN_SCHEMA,
        &instruction,
        &In::Map(
            {
                let mut input = vec![("about", In::Value(request.about.clone()))];
                if project.para() {
                    input.push(("places", In::Value(ConfigValue::Array(places))));
                }
                input
            }
            .into_iter()
            .chain(vec![
                ("rules", In::text(&rules)),
                ("sources", named_sources(&refs)),
                (
                    "pages",
                    // A note of PARA is about its own source: the model is given only the notes of
                    // a source that changed.
                    In::List(
                        existing
                            .iter()
                            .filter(|page| {
                                !project.para()
                                    || revision.iter().any(|id| id == str_of(page, "id"))
                            })
                            .map(|page| {
                                let mut entries = vec![
                                    ("id", field(page, "id")),
                                    ("title", field(page, "title")),
                                ];
                                if revision.iter().any(|id| id == str_of(page, "id")) {
                                    entries.push(("markdown", field(page, "markdown")));
                                }
                                In::Map(entries)
                            })
                            .collect(),
                    ),
                ),
                (
                    "pages_needing_revision",
                    In::Value(ConfigValue::Array(
                        revision.iter().map(|id| text(id)).collect(),
                    )),
                ),
            ])
            .collect(),
        ),
    )?;
    let [list] = strict(&result, &["pages"]).ok_or_else(model_output)?[..] else {
        return Err(model_output());
    };
    let written = written_pages(list, true).ok_or_else(model_output)?;
    if written.len() as f64 > crate::eval_stage::number(writing.get("max_pages_per_job")) {
        return Err(coded(
            2,
            "PAGE_LIMIT",
            "The model exceeded max_pages_per_job.",
        ));
    }
    // A source of this run as a source of a page.
    let entry_of = |reference: &Ref| {
        object(vec![(
            "source",
            reference
                .evidence
                .get("source")
                .cloned()
                .unwrap_or_default(),
        )])
    };
    let mut ids: HashSet<String> = HashSet::new();
    let mut changes = Vec::new();
    // The sources the pages written are written from.
    let mut used_sources: HashSet<String> = HashSet::new();
    // The sources of notes not saved for their place, which stay to be worked in again.
    let mut held_sources: HashSet<String> = HashSet::new();
    // The sources a new note of PARA is placed for in this run.
    let mut placed: HashSet<String> = HashSet::new();
    // The projects and areas of PARA, which people list, and the files of the sources by ID.
    let projects = crate::wiki_project::rule_items(&rules, "Projects");
    let areas = crate::wiki_project::rule_items(&rules, "Areas");
    let source_files: HashMap<&str, &str> = grouped
        .iter()
        .map(|(id, file, _)| (id.as_str(), file.as_str()))
        .collect();
    let pages_folder = node_path::resolve(&request.root, &request.pages);
    for mut item in written {
        item.id = item.id.trim().trim_matches('/').to_owned();
        // The sources it names; a page that names none is written from all of them.
        let mut used: Vec<ConfigValue> = Vec::new();
        for number in &item.sources {
            if let Some(reference) = number.checked_sub(1).and_then(|index| refs.get(index)) {
                used = merged_sources(&used, &[entry_of(reference)]);
            }
        }
        if used.is_empty() {
            used = refs.iter().map(entry_of).collect();
        }
        // A new note of PARA is given as its place, and is named after its source. A note that
        // exists is only written again from its own source when it changed; another source the
        // model puts there makes a note of its own at that place.
        let parts: Vec<&str> = item.id.split('/').collect();
        let place = match parts[..] {
            [top, place] => Some((top, place)),
            [top, place, ..]
                if !revision.iter().any(|id| *id == item.id)
                    && existing.iter().any(|page| str_of(page, "id") == item.id) =>
            {
                Some((top, place))
            }
            _ => None,
        };
        if project.para()
            && let Some((top @ ("Projects" | "Areas" | "Resources"), place)) = place
        {
            let name = used
                .first()
                .map(|entry| source_name(entry))
                .unwrap_or_default();
            if !name.is_empty() {
                item.id = format!("{top}/{place}/{name}");
            }
        }
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
        let earlier = existing.iter().find(|page| str_of(page, "id") == item.id);
        if project.para() && earlier.is_none() {
            if let Some(reason) = misplaced(&item.id, &projects, &areas) {
                held_sources.extend(used.iter().map(|entry| source_key(entry).to_owned()));
                warnings.push(object(vec![
                    ("code", text("PLACE_INVALID")),
                    (
                        "message",
                        text(&format!(
                            "{}: not saved, {reason}. Add it to the rules and run ingress --pending, or run ingress again.",
                            item.id
                        )),
                    ),
                    ("page", text(&item.id)),
                ]));
                continue;
            }
            // A source has one place: the first note written for it is kept.
            let key = used
                .first()
                .map(|entry| source_key(entry).to_owned())
                .unwrap_or_default();
            if !placed.insert(key) {
                warnings.push(object(vec![
                    ("code", text("NOTE_SKIPPED")),
                    (
                        "message",
                        text(&format!(
                            "{}: not saved, its source has a note at another place already.",
                            item.id
                        )),
                    ),
                    ("page", text(&item.id)),
                ]));
                continue;
            }
        }
        // The copy of each source of a note of PARA is in the folder of the note, under 資料/,
        // and the note links to it.
        if project.para() {
            let parts: Vec<&str> = item.id.split('/').collect();
            let mut folder: Vec<&str> = parts[..parts.len() - 1].to_vec();
            folder.push("資料");
            let folder = node_path::join(&pages_folder, &folder);
            for entry in &mut used {
                let Some(file) = source_files.get(source_key(entry)) else {
                    continue;
                };
                let bytes = std::fs::read(file).map_err(|error| fs_error("open", file, &error))?;
                let name = std::path::Path::new(file)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let copy = crate::wiki_fetch::keep_in(&folder, &name, &bytes)?;
                let web = entry
                    .get("source")
                    .and_then(|source| string_field(source, "url"))
                    .is_some_and(crate::wiki_fetch::is_web_address);
                if let (false, ConfigValue::Object(entries)) = (web, entry) {
                    let mut source = entries
                        .iter()
                        .find(|(name, _)| name == "source")
                        .map(|(_, value)| value.clone())
                        .unwrap_or_default();
                    if let ConfigValue::Object(fields) = &mut source {
                        assign(
                            fields,
                            "url",
                            Some(text(&crate::source_load::file_url(&copy))),
                        );
                    }
                    assign(entries, "source", Some(source));
                }
            }
        }
        let added = js_trim(&without_markers(&item.markdown)).to_owned();
        // A page given in full comes back whole; any other only gains what is new to it.
        let markdown = match earlier {
            Some(page) if !revision.iter().any(|id| *id == item.id) => {
                appended(str_of(page, "markdown"), &added)
            }
            _ => added,
        };
        if js_trim(&markdown).is_empty()
            || earlier.is_some_and(|page| str_of(page, "markdown") == markdown)
        {
            continue;
        }
        let sources = merged_sources(
            earlier.map(|page| page_sources(page)).unwrap_or_default(),
            &used,
        );
        used_sources.extend(used.iter().map(|entry| source_key(entry).to_owned()));
        // Numbers, dates and quotations that none of its sources has are pointed out.
        let mut texts = vec![rules.clone()];
        for entry in &sources {
            let source = entry.get("source").cloned().unwrap_or_default();
            texts.push(str_of(&source, "objectId").to_owned());
            if let Some(document) = current_document(&database, &source, project.cancel)? {
                texts.push(str_of(&document, "content").to_owned());
            }
        }
        let unsupported = unsupported_literals(&markdown, &texts);
        if !unsupported.is_empty() {
            warnings.push(object(vec![
                ("code", text("UNSUPPORTED_LITERAL")),
                (
                    "message",
                    text(&format!(
                        "{}: not found in its sources: {}",
                        item.id,
                        unsupported.join(", ")
                    )),
                ),
                ("page", text(&item.id)),
                (
                    "literals",
                    ConfigValue::Array(unsupported.iter().map(|literal| text(literal)).collect()),
                ),
            ]));
        }
        changes.push(project.change_page(
            &item.id,
            &item.title,
            markdown,
            sources,
            &previous,
            &item.category,
        )?);
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
    // A source no page is written from adds nothing to the wiki: it is recorded as such.
    let no_material: Vec<&String> = source_ids
        .iter()
        .filter(|id| !used_sources.contains(id.as_str()) && !held_sources.contains(id.as_str()))
        .collect();
    let mut extra = vec![
        ("sources", ConfigValue::Number(documents.len() as f64)),
        ("skipped", ConfigValue::Array(skipped)),
        (
            "source_ids",
            ConfigValue::Array(source_ids.iter().map(|id| text(id)).collect()),
        ),
        ("pages_needing_revision", ConfigValue::Array(needs_revision)),
    ];
    if !taken_with.is_empty() {
        extra.push(("fetched", ConfigValue::Array(taken_with.clone())));
    }
    if !followed.is_empty() {
        extra.push(("followed", ConfigValue::Array(followed.clone())));
    }
    if !no_material.is_empty() {
        extra.push((
            "no_material",
            ConfigValue::Array(
                documents
                    .iter()
                    .map(evidence_source)
                    .filter(|source| {
                        no_material
                            .iter()
                            .any(|id| *id == str_of(source, "sourceId"))
                    })
                    .map(|source| text(str_of(&source, "objectId")))
                    .collect(),
            ),
        ));
    }
    // The categories of the pages written, to add those the rules do not list yet.
    let mut named: Vec<String> = Vec::new();
    for page in &verified {
        let category = str_of(page, "category").trim();
        if !category.is_empty() && !named.iter().any(|known| known == category) {
            named.push(category.to_owned());
        }
    }
    let done = commit_pages(
        project,
        connection,
        tracker,
        verified,
        extra,
        |connection| {
            for id in &source_ids {
                // 1: pages are written from it; 2: it adds nothing to them. A source of a note
                // not saved for its place stays to be worked in.
                let state = if used_sources.contains(id) {
                    1
                } else if held_sources.contains(id) {
                    continue;
                } else {
                    2
                };
                connection
                    .execute(
                        "UPDATE wiki_sources SET reflected = ?2 WHERE id = ?1",
                        rusqlite::params![id, state],
                    )
                    .map_err(sql)?;
            }
            Ok(())
        },
    )?;
    // A category the model gives that the rules do not list is added to them, for people to
    // see and change; so is a topic of Resources that a note of PARA is placed in.
    let (heading, listed) = if project.para() {
        named = Vec::new();
        for page in items(done.get("pages")) {
            let parts: Vec<&str> = str_of(page, "id").split('/').collect();
            if parts.len() > 2
                && parts[0] == "Resources"
                && !named.iter().any(|name| name == parts[1])
            {
                named.push(parts[1].to_owned());
            }
        }
        let topics = crate::wiki_project::rule_items(&rules, "Resources");
        (
            "Resources",
            topics.into_iter().map(|(name, _)| name).collect::<Vec<_>>(),
        )
    } else {
        ("分類", crate::wiki_project::rule_categories(&rules))
    };
    named.retain(|name| !listed.contains(name));
    if !named.is_empty()
        && let Ok(file) = project.rules_path()
    {
        atomic_write(
            &file,
            crate::wiki_project::with_items(&rules, heading, &named).as_bytes(),
        )?;
    }
    Ok(done)
}

/// The name a note of PARA takes from its source: the file name without its extension, or the
/// last part of the path of a web address, or its host.
fn source_name(entry: &ConfigValue) -> String {
    let source = entry.get("source");
    let field = |name: &str| {
        source
            .and_then(|source| string_field(source, name))
            .unwrap_or_default()
    };
    let object_id = field("objectId");
    let last = if crate::wiki_fetch::is_web_address(object_id) {
        url::Url::parse(object_id)
            .ok()
            .and_then(|url| {
                let segment = url
                    .path_segments()
                    .and_then(|mut segments| segments.rfind(|part| !part.is_empty()))
                    .and_then(crate::wiki_project::decode_uri_component);
                segment.or_else(|| url.host_str().map(str::to_owned))
            })
            .unwrap_or_default()
    } else {
        object_id
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    let stem = match last.rfind('.').filter(|&at| at > 0) {
        Some(at) => &last[..at],
        None => last.as_str(),
    };
    stem.trim().to_owned()
}

/// Why a new note of PARA cannot be placed at its ID, if it cannot: it is in a project or an area
/// of the rules, or in a topic of Resources, by the name of its source.
fn misplaced(
    id: &str,
    projects: &[(String, String)],
    areas: &[(String, String)],
) -> Option<String> {
    let parts: Vec<&str> = id.split('/').collect();
    let listed =
        |list: &[(String, String)], name: &str| list.iter().any(|(known, _)| known == name);
    match parts[..] {
        [_, _] | [_] => {
            Some("a note is placed as <Projects|Areas|Resources>/<name>/<source>".to_owned())
        }
        ["Projects", name, ..] if !listed(projects, name) => {
            Some(format!("{name} is not a project of the rules"))
        }
        ["Areas", name, ..] if !listed(areas, name) => {
            Some(format!("{name} is not an area of the rules"))
        }
        ["Projects" | "Areas" | "Resources", ..] => None,
        _ => Some("a new note goes into Projects, Areas or Resources".to_owned()),
    }
}

/// A page with text added: the paragraphs of the text that the page does not have yet, after it.
fn appended(markdown: &str, added: &str) -> String {
    let have: HashSet<&str> = markdown.split("\n\n").map(js_trim).collect();
    let new: Vec<&str> = added
        .split("\n\n")
        .map(js_trim)
        .filter(|paragraph| !paragraph.is_empty() && !have.contains(paragraph))
        .collect();
    if new.is_empty() {
        return markdown.to_owned();
    }
    format!("{}\n\n{}", markdown.trim_end(), new.join("\n\n"))
}

/// The numbers and quotations of a page that none of its sources has, each once: numbers of two
/// or more digits (`2024`, `1,000`, `3.5`) and what is in 「」, 『』 or “”. Link targets and
/// the numbers of a numbered list are not read; spaces and the commas in numbers do not count.
pub(crate) fn unsupported_literals(markdown: &str, sources: &[String]) -> Vec<String> {
    use std::sync::OnceLock;
    static NUMBER: OnceLock<regex::Regex> = OnceLock::new();
    static QUOTE: OnceLock<regex::Regex> = OnceLock::new();
    static LIST: OnceLock<regex::Regex> = OnceLock::new();
    let number = NUMBER.get_or_init(|| regex::Regex::new(r"[0-9][0-9,.]*[0-9]").expect("valid"));
    let quote = QUOTE.get_or_init(|| {
        regex::Regex::new(r"「([^「」]+)」|『([^『』]+)』|“([^“”]+)”").expect("valid")
    });
    let list =
        LIST.get_or_init(|| regex::Regex::new(r"(?m)^[ \t]*[0-9]+[.)][ \t]").expect("valid"));
    let mut readable = String::with_capacity(markdown.len());
    let mut last = 0;
    for (start, end) in link_targets(markdown) {
        readable += &markdown[last..start];
        last = end;
    }
    readable += &markdown[last..];
    let readable = nfkc(&list.replace_all(&readable, ""));
    let joined = nfkc(&sources.join("\n"));
    let compact = |text: &str| -> String {
        text.chars()
            .filter(|c| !c.is_whitespace() && *c != ',')
            .collect()
    };
    let haystack = compact(&joined);
    // A number is in the sources as a whole number of theirs, not as a part of a longer one.
    let as_number = |text: &str| compact(text.trim_end_matches(['.', ',']));
    let numbers: HashSet<String> = number
        .find_iter(&joined)
        .map(|found| as_number(found.as_str()))
        .collect();
    let mut found: Vec<String> = Vec::new();
    for captures in quote.captures_iter(&readable) {
        if let Some(inner) = (1..=3).find_map(|group| captures.get(group)) {
            let wanted = compact(inner.as_str());
            if !wanted.is_empty() && !haystack.contains(&wanted) {
                found.push(inner.as_str().to_owned());
            }
        }
    }
    for literal in number.find_iter(&readable) {
        if !numbers.contains(&as_number(literal.as_str())) {
            found.push(literal.as_str().trim_end_matches(['.', ',']).to_owned());
        }
    }
    let mut seen = HashSet::new();
    found.retain(|literal| seen.insert(literal.clone()));
    found
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbers_and_quotations_are_written_as_the_sources_write_them() {
        let sources = [
            "スプリントは最長1か月。開発者は3人から9人。\n1,000件のうち「完成の定義」を満たすもの"
                .to_owned(),
        ];
        let found = |markdown: &str| super::unsupported_literals(markdown, &sources);
        assert!(found("開発者は３人から９人。1000件。「完成 の定義」。").is_empty());
        assert_eq!(
            found("開発者は10人まで。「完了の定義」は2020年に変わった。"),
            ["完了の定義", "10", "2020"]
        );
        // Link targets and the numbers of a list are not part of what the page says.
        assert!(found("[年表](/年表-2020)\n\n10. 一つ目").is_empty());
        assert!(found("1.5倍").len() == 1);
    }

    #[test]
    fn a_page_gains_only_the_paragraphs_it_does_not_have() {
        assert_eq!(
            super::appended("一。\n\n二。\n", "二。\n\n三。"),
            "一。\n\n二。\n\n三。"
        );
        assert_eq!(super::appended("一。", "一。"), "一。");
    }

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
