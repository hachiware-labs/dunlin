//! `nest dictionary` (spec 28.13 step 6c): dictionary candidates from the missed evidence of an
//! evaluation report, and adding a user dictionary to a candidate configuration, as
//! `project-dictionary.mjs` did them.

use crate::Cancellation;
use crate::cli_project::{CliError, Project, current_build, open_build, revision, stage};
use crate::cli_tools::CliEnv;
use crate::config_value::ConfigValue;
use crate::eval_stage::js_string;
use crate::index_stage::{items, object, string_field, text, truthy};
use crate::node_path;
use crate::wiki_project::{atomic_write, fs_error};

const MAX_SURFACE: usize = 16;
const BATCH: usize = 32;

fn native(error: crate::Error) -> CliError {
    let mut failure = CliError::plain(error.to_string());
    failure.code = Some(error.code().into());
    failure
}

fn report_output(
    result: Result<ConfigValue, crate::transform_stage::StageError>,
) -> Result<ConfigValue, CliError> {
    result.map_err(CliError::from)
}

fn category(c: char) -> icu_properties::props::GeneralCategory {
    icu_properties::CodePointMapData::<icu_properties::props::GeneralCategory>::new().get(c)
}

fn letter(c: char) -> bool {
    icu_properties::props::GeneralCategoryGroup::Letter.contains(category(c))
}

fn number(c: char) -> bool {
    icu_properties::props::GeneralCategoryGroup::Number.contains(category(c))
}

fn mark(c: char) -> bool {
    icu_properties::props::GeneralCategoryGroup::Mark.contains(category(c))
}

/// `/^[\p{L}\p{N}][\p{L}\p{M}\p{N}_./・×+#-]*[\p{L}\p{M}\p{N}]$/u`
fn term_surface(surface: &str) -> bool {
    let chars: Vec<char> = surface.chars().collect();
    let [first, middle @ .., last] = chars.as_slice() else {
        return false;
    };
    (letter(*first) || number(*first))
        && middle.iter().all(|&c| {
            letter(c)
                || mark(c)
                || number(c)
                || matches!(c, '_' | '.' | '/' | '・' | '×' | '+' | '#' | '-')
        })
        && (letter(*last) || mark(*last) || number(*last))
}

fn csv_field(value: &str) -> String {
    if value.contains(['"', ',', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// `bytes.subarray(start, end).toString()`
fn slice(bytes: &[u8], start: usize, end: usize) -> String {
    let end = end.min(bytes.len());
    let start = start.min(end);
    String::from_utf8_lossy(&bytes[start..end]).into_owned()
}

#[derive(Clone)]
struct Token {
    start: usize,
    end: usize,
    term: Option<String>,
    reading: Option<String>,
    surface: String,
}

fn search_terms(tokens: &[Token]) -> Vec<String> {
    tokens
        .iter()
        .filter_map(|token| token.term.clone())
        .collect()
}

fn segmentation(tokens: &[Token]) -> ConfigValue {
    ConfigValue::Array(
        tokens
            .iter()
            .map(|token| {
                object(vec![
                    ("surface", text(&token.surface)),
                    (
                        "term",
                        token.term.as_deref().map_or(ConfigValue::Null, text),
                    ),
                ])
            })
            .collect(),
    )
}

/// `indexOf(needle, from)` on bytes.
fn index_of(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(haystack.len()));
    }
    (from..=haystack.len().saturating_sub(needle.len()))
        .find(|&at| haystack[at..].starts_with(needle))
}

struct Text {
    bytes: Vec<u8>,
    tokens: Vec<Token>,
}

struct Evidence {
    bytes: Vec<u8>,
    start: usize,
    end: usize,
    tokens: Vec<Token>,
}

struct Candidate {
    surface: String,
    csv: String,
    start: usize,
    end: usize,
    question_tokens: ConfigValue,
    evidence_tokens: ConfigValue,
    queries: ConfigValue,
}

/// `candidatesForEvidence(question, evidence)`
fn candidates_for_evidence(question: &Text, evidence: &Evidence) -> Vec<Candidate> {
    let mut starts = std::collections::HashMap::new();
    let mut ends = std::collections::HashMap::new();
    for (index, token) in evidence.tokens.iter().enumerate() {
        starts.insert(token.start, index);
        ends.insert(token.end, index + 1);
    }
    let evidence_terms: std::collections::HashSet<String> =
        search_terms(&evidence.tokens).into_iter().collect();
    let mut candidates: Vec<Candidate> = Vec::new();
    for (i, first) in question.tokens.iter().enumerate() {
        for j in i..question.tokens.len() {
            let last = &question.tokens[j];
            let surface = slice(&question.bytes, first.start, last.end);
            if surface.chars().count() > MAX_SURFACE
                || surface.chars().any(crate::registry::js_space)
            {
                break;
            }
            if !term_surface(&surface)
                || !surface.chars().any(letter)
                || candidates
                    .iter()
                    .any(|candidate| candidate.surface == surface)
            {
                continue;
            }
            let query_tokens = &question.tokens[i..=j];
            let query_terms = search_terms(query_tokens);
            if !query_terms.is_empty()
                && query_terms.iter().all(|term| evidence_terms.contains(term))
            {
                continue;
            }
            let length = last.end.saturating_sub(first.start);
            let mut at = index_of(&evidence.bytes, surface.as_bytes(), evidence.start);
            while let Some(position) = at.filter(|position| position + length <= evidence.end) {
                at = index_of(&evidence.bytes, surface.as_bytes(), position + 1);
                let (Some(&from), Some(&to)) =
                    (starts.get(&position), ends.get(&(position + length)))
                else {
                    continue;
                };
                let evidence_tokens = evidence.tokens.get(from..to).unwrap_or_default();
                let document_terms = search_terms(evidence_tokens);
                if query_terms.is_empty() && document_terms.is_empty() {
                    continue;
                }
                let same = |q: Option<&Token>, d: Option<&Token>| match (q, d) {
                    (Some(q), Some(d)) => q.surface == d.surface && q.term == d.term,
                    _ => false,
                };
                if same(query_tokens.first(), evidence_tokens.first())
                    || same(query_tokens.last(), evidence_tokens.last())
                {
                    continue;
                }
                let split = if query_tokens.len() >= evidence_tokens.len() {
                    query_tokens
                } else {
                    evidence_tokens
                };
                let fragment = split
                    .iter()
                    .rev()
                    .find(|token| {
                        token.term.as_deref().is_some_and(|term| !term.is_empty())
                            && token.surface.chars().count() > 1
                    })
                    .or_else(|| {
                        split.iter().rev().find(|token| {
                            token.term.as_deref().is_some_and(|term| !term.is_empty())
                        })
                    });
                let part = match fragment {
                    Some(fragment) if fragment.surface != surface => fragment.surface.clone(),
                    _ => surface.chars().skip(1).collect(),
                };
                let reading = if split.iter().all(|token| {
                    token
                        .reading
                        .as_deref()
                        .is_some_and(|reading| !reading.is_empty())
                }) {
                    split
                        .iter()
                        .filter_map(|token| token.reading.clone())
                        .collect::<String>()
                } else {
                    "*".into()
                };
                candidates.push(Candidate {
                    csv: [surface.as_str(), "名詞", &reading]
                        .iter()
                        .map(|field| csv_field(field))
                        .collect::<Vec<_>>()
                        .join(","),
                    surface: surface.clone(),
                    start: first.start,
                    end: last.end,
                    question_tokens: segmentation(query_tokens),
                    evidence_tokens: segmentation(evidence_tokens),
                    queries: object(vec![
                        ("whole", text(&String::from_utf8_lossy(&question.bytes))),
                        (
                            "partial",
                            text(&format!(
                                "{}{part}{}",
                                slice(&question.bytes, 0, first.start),
                                slice(&question.bytes, last.end, question.bytes.len())
                            )),
                        ),
                    ]),
                });
                break;
            }
        }
    }
    // Retain the smallest spans whose boundaries differ.
    candidates.sort_by_key(|candidate| candidate.end - candidate.start);
    let mut kept: Vec<Candidate> = Vec::new();
    let spans: Vec<(usize, usize)> = candidates
        .iter()
        .map(|candidate| (candidate.start, candidate.end))
        .collect();
    for (index, candidate) in candidates.into_iter().enumerate() {
        if !spans[..index]
            .iter()
            .any(|(start, end)| candidate.start <= *start && *end <= candidate.end)
        {
            kept.push(candidate);
        }
    }
    kept
}

fn normalize(value: &str, enabled: bool) -> String {
    if enabled {
        crate::evidence_match::nfkc(value)
    } else {
        value.to_owned()
    }
}

fn tokens_of(analysis: &ConfigValue, bytes: &[u8]) -> Vec<Token> {
    items(Some(analysis))
        .iter()
        .map(|token| {
            let position = |name: &str| match token.get(name) {
                Some(ConfigValue::Number(value)) => *value as usize,
                _ => 0,
            };
            let (start, end) = (position("start"), position("end"));
            Token {
                start,
                end,
                term: token
                    .get("term")
                    .and_then(ConfigValue::as_str)
                    .map(str::to_owned),
                reading: token
                    .get("reading")
                    .and_then(ConfigValue::as_str)
                    .map(str::to_owned),
                surface: slice(bytes, start, end),
            }
        })
        .collect()
}

struct Analyzed {
    miss: ConfigValue,
    context: Option<(Text, Evidence)>,
}

/// `analyzeMisses(misses, report, dictionary)`
fn analyze_misses(
    misses: &[ConfigValue],
    report: &ConfigValue,
    dictionary: Option<&str>,
) -> Result<Vec<Analyzed>, CliError> {
    let find = |list: Option<&ConfigValue>, name: &str| {
        items(list)
            .iter()
            .find(|entry| string_field(entry, "use") == Some(name))
            .cloned()
    };
    let normalize_query = find(report.get("search_settings"), "query.normalize")
        .and_then(|entry| {
            entry
                .get("with")
                .and_then(|with| string_field(with, "unicode"))
                .map(str::to_owned)
        })
        .as_deref()
        == Some("nfkc");
    let normalize_document = find(
        report
            .get("construction")
            .and_then(|construction| construction.get("transform")),
        "text.normalize",
    )
    .and_then(|entry| {
        entry
            .get("with")
            .and_then(|with| string_field(with, "unicode"))
            .map(str::to_owned)
    })
    .as_deref()
        == Some("nfkc");
    let tokenizer = std::cell::OnceCell::new();
    let mut analyzed = Vec::new();
    for batch in misses.chunks(BATCH) {
        let contexts: Vec<Option<(String, String, usize, usize)>> = batch
            .iter()
            .map(|miss| {
                if truthy(miss.get("reason")) {
                    return None;
                }
                let prefix = normalize(&js_string(miss.get("before")), normalize_document);
                let excerpt = normalize(&js_string(miss.get("text")), normalize_document);
                let start = prefix.len();
                let question = js_string(miss.get("question"));
                let query = normalize(
                    question.trim_matches(crate::registry::js_space),
                    normalize_query,
                );
                let body = format!(
                    "{prefix}{excerpt}{}",
                    normalize(&js_string(miss.get("after")), normalize_document)
                );
                Some((query, body, start, start + excerpt.len()))
            })
            .collect();
        let mut texts: Vec<String> = Vec::new();
        for (query, body, ..) in contexts.iter().flatten() {
            for value in [query, body] {
                if !texts.contains(value) {
                    texts.push(value.clone());
                }
            }
        }
        let mut analyses: Vec<ConfigValue> = Vec::new();
        if !texts.is_empty() {
            if tokenizer.get().is_none() {
                let opened =
                    crate::japanese::JapaneseTokenizer::open(dictionary.map(std::path::Path::new))
                        .map_err(native)?;
                let _ = tokenizer.set(opened);
            }
            let tokenizer = tokenizer.get().expect("tokenizer");
            for value in &texts {
                let tokens = tokenizer.analyze(value).map_err(native)?;
                analyses.push(crate::wiki_project::ordered(tokens));
            }
        }
        let analysis = |value: &str| {
            texts
                .iter()
                .position(|text| text == value)
                .map(|index| analyses[index].clone())
                .unwrap_or_default()
        };
        for (miss, context) in batch.iter().zip(contexts) {
            let context = context.map(|(query, body, start, end)| {
                let query_bytes = query.as_bytes().to_vec();
                let body_bytes = body.as_bytes().to_vec();
                let question_tokens = tokens_of(&analysis(&query), &query_bytes);
                let evidence_tokens = tokens_of(&analysis(&body), &body_bytes)
                    .into_iter()
                    .filter(|token| token.start < end && token.end > start)
                    .collect();
                (
                    Text {
                        bytes: query_bytes,
                        tokens: question_tokens,
                    },
                    Evidence {
                        bytes: body_bytes,
                        start,
                        end,
                        tokens: evidence_tokens,
                    },
                )
            });
            analyzed.push(Analyzed {
                miss: miss.clone(),
                context,
            });
        }
    }
    Ok(analyzed)
}

/// `suggestDictionary(report, misses, dictionary)`
fn suggest_dictionary(
    report: &ConfigValue,
    misses: impl FnOnce() -> Result<ConfigValue, CliError>,
    dictionary: Option<&str>,
) -> Result<ConfigValue, CliError> {
    if !items(report.get("search_settings"))
        .iter()
        .any(|entry| string_field(entry, "use") == Some("retrieval.keyword"))
    {
        return Err(CliError::new(
            2,
            "KEYWORD_SEARCH_REQUIRED",
            "Choose a report that used Lindera keyword search.",
        ));
    }
    let uses_context = report
        .get("scores")
        .and_then(|scores| scores.get("evidence_recall@context"))
        .is_some();
    let mut findings: Vec<ConfigValue> = Vec::new();
    let mut candidates: Vec<(String, String, Vec<ConfigValue>)> = Vec::new();
    let names = ["dictionary", "partial_match", "other", "unresolved"];
    let mut classes: Vec<Vec<String>> = vec![Vec::new(); 4];
    let misses = misses()?;
    for Analyzed { miss, context } in analyze_misses(items(Some(&misses)), report, dictionary)? {
        let raw = js_string(miss.get("question"));
        let mut finding = vec![
            (
                "case_id".to_owned(),
                miss.get("case_id").cloned().unwrap_or_default(),
            ),
            (
                "question".to_owned(),
                miss.get("question").cloned().unwrap_or_default(),
            ),
            (
                "doc".to_owned(),
                miss.get("doc").cloned().unwrap_or_default(),
            ),
            (
                "quote".to_owned(),
                miss.get("quote").cloned().unwrap_or_default(),
            ),
        ];
        if let Some(url) = miss.get("url").filter(|url| truthy(Some(url))) {
            finding.push(("url".into(), url.clone()));
        }
        let mut record = |classification: &str, details: Option<Vec<(&str, ConfigValue)>>| {
            let index = names
                .iter()
                .position(|name| *name == classification)
                .unwrap_or(3);
            if !classes[index].contains(&raw) {
                classes[index].push(raw.clone());
            }
            if let Some(details) = details {
                let mut entry = finding.clone();
                entry.push(("classification".into(), text(classification)));
                entry.extend(
                    details
                        .into_iter()
                        .map(|(key, value)| (key.to_owned(), value)),
                );
                findings.push(ConfigValue::Object(entry));
            }
        };
        if let Some(reason) = miss.get("reason").filter(|reason| truthy(Some(reason))) {
            record("unresolved", Some(vec![("reason", reason.clone())]));
            continue;
        }
        let Some((question, evidence)) = context else {
            continue;
        };
        let proposed = candidates_for_evidence(&question, &evidence);
        if !proposed.is_empty() {
            record("dictionary", None);
            for entry in proposed {
                let position = match candidates
                    .iter()
                    .position(|(surface, ..)| *surface == entry.surface)
                {
                    Some(position) => position,
                    None => {
                        candidates.push((entry.surface.clone(), entry.csv.clone(), Vec::new()));
                        candidates.len() - 1
                    }
                };
                let mut example = finding.clone();
                example.push(("question_tokens".into(), entry.question_tokens));
                example.push(("evidence_tokens".into(), entry.evidence_tokens));
                example.push(("queries".into(), entry.queries));
                candidates[position].2.push(ConfigValue::Object(example));
            }
            continue;
        }
        let mut query_terms: Vec<String> = Vec::new();
        for term in search_terms(&question.tokens) {
            if !query_terms.contains(&term) {
                query_terms.push(term);
            }
        }
        let evidence_terms = search_terms(&evidence.tokens);
        if query_terms.iter().any(|term| {
            !evidence_terms.contains(term)
                && evidence_terms
                    .iter()
                    .any(|longer| longer.contains(term.as_str()))
        }) {
            record("partial_match", None);
            continue;
        }
        let matched: Vec<ConfigValue> = query_terms
            .iter()
            .filter(|term| evidence_terms.contains(term))
            .map(|term| text(term))
            .collect();
        if matched.is_empty() {
            record(
                "unresolved",
                Some(vec![("reason", text("no_term_alignment"))]),
            );
        } else {
            let reason = match miss.get("in_results") {
                None => "matching_terms",
                Some(in_results) if truthy(Some(in_results)) && uses_context => {
                    "excluded_after_ranking"
                }
                Some(_) => "below_evaluation_cutoff",
            };
            record(
                "other",
                Some(vec![
                    ("reason", text(reason)),
                    ("matched_terms", ConfigValue::Array(matched)),
                ]),
            );
        }
    }
    let legacy = findings
        .iter()
        .any(|finding| string_field(finding, "reason") == Some("legacy_mixed_recovery"));
    let mut output = vec![
        ("report_id", report.get("id").cloned().unwrap_or_default()),
        (
            "build_id",
            report.get("build_id").cloned().unwrap_or_default(),
        ),
        (
            "counts",
            ConfigValue::Object(
                names
                    .iter()
                    .zip(&classes)
                    .map(|(name, list)| {
                        ((*name).to_owned(), ConfigValue::Number(list.len() as f64))
                    })
                    .collect(),
            ),
        ),
        (
            "candidates",
            ConfigValue::Array(
                candidates
                    .into_iter()
                    .map(|(surface, csv, examples)| {
                        object(vec![
                            ("surface", text(&surface)),
                            ("csv", text(&csv)),
                            ("examples", ConfigValue::Array(examples)),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("findings", ConfigValue::Array(findings)),
    ];
    if legacy {
        output.push((
            "hint",
            text("Run nest eval again for per-evidence diagnostics."),
        ));
    }
    Ok(object(output))
}

/// `suggestFromReport(project, reference)`
pub fn suggest_from_report(
    env: &CliEnv,
    project: &Project,
    reference: Option<&str>,
    cancel: &Cancellation,
) -> Result<ConfigValue, CliError> {
    let _ = env;
    let mut request = vec![("state".to_owned(), text(&project.paths.state))];
    crate::index_stage::assign(&mut request, "report", reference.map(text));
    crate::index_stage::assign(
        &mut request,
        "dataset",
        project
            .config
            .get("eval-data")
            .and_then(|data| data.get("use"))
            .cloned(),
    );
    crate::index_stage::assign(
        &mut request,
        "includeGenerated",
        Some(ConfigValue::Bool(truthy(
            project
                .eval_settings
                .as_ref()
                .and_then(|settings| settings.get("includeGenerated")),
        ))),
    );
    let request: crate::eval_report::DictionaryRequest =
        serde_json::from_str(&ConfigValue::Object(request).to_js_json())
            .map_err(|error| CliError::plain(format!("Invalid report request: {error}")))?;
    let evaluation = report_output(crate::eval_report::dictionary_evaluation(&request))?;
    let report = evaluation.get("report").cloned().unwrap_or_default();
    let data = evaluation.get("data").cloned().unwrap_or_default();
    let build_id = js_string(report.get("build_id"));
    let build = current_build(&project.paths, true, Some(&build_id))?.unwrap_or_default();
    if build
        .get("keyword")
        .and_then(|keyword| string_field(keyword, "tokenizer"))
        != Some("lindera")
    {
        return Err(CliError::new(
            2,
            "LINDERA_REQUIRED",
            "Choose an evaluation report for a Lindera build.",
        ));
    }
    let database = open_build(&project.paths, &build)?;
    let dictionary = build
        .get("keyword")
        .and_then(|keyword| string_field(keyword, "dictionary_hash"))
        .filter(|hash| !hash.is_empty())
        .map(|hash| crate::cli_project::dictionary_snapshot(&project.paths, hash));
    let misses = || {
        let request = object(vec![
            ("state", text(&project.paths.state)),
            ("report", report.clone()),
            ("data", data.clone()),
        ]);
        let request: crate::eval_report::MissesRequest =
            serde_json::from_str(&request.to_js_json())
                .map_err(|error| CliError::plain(format!("Invalid misses request: {error}")))?;
        report_output(crate::eval_report::dictionary_misses(
            &database, &request, cancel,
        ))
    };
    suggest_dictionary(&report, misses, dictionary.as_deref())
}

/// `dictionaryText(data)`
pub fn dictionary_text(data: &ConfigValue) -> String {
    let counts = data.get("counts").cloned().unwrap_or_default();
    let count = |name: &str| js_string(counts.get(name));
    let tokens = |list: Option<&ConfigValue>| {
        items(list)
            .iter()
            .map(|token| {
                let surface = js_string(token.get("surface"));
                match token.get("term") {
                    Some(ConfigValue::String(term)) if *term == surface => surface,
                    Some(ConfigValue::String(term)) => format!("{surface} ({term})"),
                    _ => format!("{surface} (ignored)"),
                }
            })
            .collect::<Vec<_>>()
            .join(" / ")
    };
    let mut lines = vec![
        format!("評価レポート {}", js_string(data.get("report_id"))),
        format!(
            "辞書候補 {}件 · 部分一致 {}件 · 辞書対象外 {}件 · 未解決 {}件",
            count("dictionary"),
            count("partial_match"),
            count("other"),
            count("unresolved")
        ),
    ];
    for candidate in items(data.get("candidates")) {
        lines.push(js_string(candidate.get("csv")));
        for entry in items(candidate.get("examples")) {
            lines.push(format!(
                "  {} — {}",
                js_string(entry.get("doc")),
                js_string(entry.get("quote"))
            ));
            lines.push(format!(
                "  質問の分かち書き {}",
                tokens(entry.get("question_tokens"))
            ));
            lines.push(format!(
                "  根拠の分かち書き {}",
                tokens(entry.get("evidence_tokens"))
            ));
            let queries = entry.get("queries");
            lines.push(format!(
                "  語全体の質問 {}",
                js_string(queries.and_then(|queries| queries.get("whole")))
            ));
            lines.push(format!(
                "  部分語の質問 {}",
                js_string(queries.and_then(|queries| queries.get("partial")))
            ));
        }
    }
    for finding in items(data.get("findings")) {
        let label = if string_field(finding, "classification") == Some("other") {
            "辞書対象外"
        } else {
            "未解決"
        };
        lines.push(format!(
            "  {label} ({}) — {} [{}]",
            js_string(finding.get("reason")),
            js_string(finding.get("question")),
            js_string(finding.get("doc"))
        ));
    }
    if let Some(hint) = data.get("hint").filter(|hint| truthy(Some(hint))) {
        lines.push(js_string(Some(hint)));
    }
    lines.join("\n")
}

/// Sets one scalar of a YAML file in place, as `parseDocument(input).setIn(key, value)` did for a
/// key that holds a scalar: the rest of the file, its comments and layout stay as they are.
fn set_scalar(input: &str, path: &[crate::yaml_config::PathPart], value: &str) -> Option<String> {
    let parsed = crate::yaml_config::parse_config(input).ok()?;
    let line = parsed
        .lines
        .iter()
        .find(|(candidate, _)| candidate.as_slice() == path)
        .map(|(_, line)| *line)?;
    let crate::yaml_config::PathPart::Key(key) = path.last()? else {
        return None;
    };
    let mut out = String::with_capacity(input.len() + value.len());
    let mut found = false;
    for (index, raw) in input.split_inclusive('\n').enumerate() {
        let ending = if raw.ends_with("\r\n") {
            "\r\n"
        } else if raw.ends_with('\n') {
            "\n"
        } else {
            ""
        };
        let body = &raw[..raw.len() - ending.len()];
        let trimmed = body.trim_start();
        let indent = &body[..body.len() - trimmed.len()];
        let prefix = trimmed.strip_prefix("- ").map_or("", |_| "- ");
        let rest = &trimmed[prefix.len()..];
        if !found && index + 1 == line && rest.starts_with(&format!("{key}:")) {
            out += &format!("{indent}{prefix}{key}: {value}{ending}");
            found = true;
            continue;
        }
        out += raw;
    }
    found.then_some(out)
}

/// `addDictionary(project, configPath, selected)`
pub fn add_dictionary(
    env: &CliEnv,
    project: &Project,
    config_path: &str,
    selected: &str,
) -> Result<ConfigValue, CliError> {
    let realpath = |path: &str| crate::wiki_project::realpath(path).map_err(CliError::from);
    if realpath(config_path)? == realpath(&project.config_path)? {
        return Err(CliError::new(
            2,
            "CANDIDATE_REQUIRED",
            "Use --config with a separate candidate configuration.",
        ));
    }
    let keyword = stage(&project.plan, "index")
        .iter()
        .find(|step| step.module.name == "index.keyword");
    let Some(keyword) = keyword.filter(|keyword| {
        keyword.arg("tokenizer").and_then(ConfigValue::as_str) == Some("lindera")
    }) else {
        return Err(CliError::new(
            2,
            "LINDERA_REQUIRED",
            "Set the candidate keyword tokenizer to lindera.",
        ));
    };
    let read = |path: &str| crate::wiki_project::read_file(path).map_err(CliError::from);
    let input = read(config_path)?;
    let changed = || {
        CliError::new(
            4,
            "CANDIDATE_CHANGED",
            "The candidate configuration changed. Retry with its latest contents.",
        )
    };
    if revision(input.as_bytes()) != project.config_digest {
        return Err(changed());
    }
    let existing = keyword
        .arg("user_dictionary")
        .and_then(ConfigValue::as_str)
        .filter(|path| !path.is_empty());
    let csv = crate::japanese::merge_user_records(
        existing.map(std::path::Path::new),
        std::path::Path::new(&env.resolve(selected)),
    )
    .map_err(|error| {
        if error.code() == "INVALID_ARGUMENT" {
            CliError::new(
                2,
                "INVALID_USER_DICTIONARY",
                format!(
                    "Check the selected CSV {selected}{}. {error}",
                    existing.map_or(String::new(), |existing| format!(
                        " and existing CSV {existing}"
                    ))
                ),
            )
        } else {
            native(error)
        }
    })?;
    let file = format!("user-dictionary-{}.csv", revision(csv.as_bytes()));
    let path = node_path::join(&node_path::dirname(config_path), &[&file]);
    let current = match std::fs::read(&path) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(CliError::from(fs_error("open", &path, &error))),
    };
    if current.as_ref().is_some_and(|current| *current != csv) {
        return Err(CliError::new(
            2,
            "DICTIONARY_EXISTS",
            format!("Another dictionary already exists at {path}"),
        ));
    }
    if current.is_none() {
        atomic_write(&path, csv.as_bytes()).map_err(CliError::from)?;
    }
    if read(config_path)? != input {
        return Err(changed());
    }
    let index = items(
        project
            .config
            .get("index")
            .and_then(|index| index.get("modules")),
    )
    .iter()
    .position(|entry| string_field(entry, "use") == Some("index.keyword"))
    .unwrap_or(0);
    use crate::yaml_config::PathPart;
    let key = [
        PathPart::Key("index".into()),
        PathPart::Key("modules".into()),
        PathPart::Index(index),
        PathPart::Key("with".into()),
        PathPart::Key("user_dictionary".into()),
    ];
    let dictionary = format!("./{file}");
    let parsed =
        crate::yaml_config::parse_config(&input).map_err(|error| CliError::plain(error.message))?;
    let current_value = parsed
        .lines
        .iter()
        .any(|(candidate, _)| candidate.as_slice() == key)
        && items(
            parsed
                .value
                .get("index")
                .and_then(|index| index.get("modules")),
        )
        .get(index)
        .and_then(|entry| entry.get("with"))
        .and_then(|with| with.get("user_dictionary"))
        .and_then(ConfigValue::as_str)
            == Some(dictionary.as_str());
    if !current_value {
        let updated = match set_scalar(&input, &key, &dictionary) {
            Some(updated) => updated,
            None => {
                // The key is not on a line of its own: write the whole document again.
                let mut config = parsed.value.clone();
                set_in(&mut config, &["index", "modules"], index, &dictionary);
                crate::yaml_emit::stringify(&config)
            }
        };
        atomic_write(config_path, updated.as_bytes()).map_err(CliError::from)?;
    }
    Ok(object(vec![
        ("config", text(config_path)),
        ("user_dictionary", text(&path)),
    ]))
}

fn set_in(config: &mut ConfigValue, sections: &[&str], index: usize, dictionary: &str) {
    let mut node = config;
    for section in sections {
        let ConfigValue::Object(entries) = node else {
            return;
        };
        let Some((_, child)) = entries.iter_mut().find(|(key, _)| key == section) else {
            return;
        };
        node = child;
    }
    let ConfigValue::Array(modules) = node else {
        return;
    };
    let Some(ConfigValue::Object(entry)) = modules.get_mut(index) else {
        return;
    };
    if !entry.iter().any(|(key, _)| key == "with") {
        entry.push(("with".into(), ConfigValue::Object(Vec::new())));
    }
    if let Some((_, ConfigValue::Object(with))) = entry.iter_mut().find(|(key, _)| key == "with") {
        crate::index_stage::assign(with, "user_dictionary", Some(text(dictionary)));
    }
}
