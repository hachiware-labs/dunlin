//! What the CLI prints without `--json`: short text for people. `--json` keeps the full data.

// Only the CLI uses this outside its tests.
#![cfg_attr(test, allow(dead_code))]

use crate::config_value::ConfigValue;
use crate::eval_stage::js_string;
use crate::index_stage::{items, string_field, truthy};

fn field(data: &ConfigValue, name: &str) -> String {
    js_string(data.get(name))
}

fn present(value: Option<&ConfigValue>) -> Option<&ConfigValue> {
    value.filter(|value| !matches!(value, ConfigValue::Null))
}

/// A number as people read it: whole numbers without decimals, others to three places.
pub fn number(value: Option<&ConfigValue>) -> String {
    match value {
        Some(ConfigValue::Number(number)) if number.fract() == 0.0 => format!("{number:.0}"),
        Some(ConfigValue::Number(number)) => {
            let text = format!("{number:.3}");
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
        Some(ConfigValue::Null) | None => "-".into(),
        other => js_string(other),
    }
}

/// A score from 0 to 1 with three decimals.
fn score(value: Option<&ConfigValue>) -> String {
    match value {
        Some(ConfigValue::Number(number)) => format!("{number:.3}"),
        Some(ConfigValue::Null) | None => "-".into(),
        other => js_string(other),
    }
}

fn bytes(value: Option<&ConfigValue>) -> String {
    let Some(ConfigValue::Number(size)) = value else {
        return "-".into();
    };
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut size = *size;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < units.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size:.0} {}", units[unit])
    } else {
        format!("{size:.1} {}", units[unit])
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// Rows of `label  value` with the values lined up.
fn table(rows: &[(String, String)], indent: &str) -> Vec<String> {
    let width = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    rows.iter()
        .map(|(label, value)| {
            let pad = width - label.chars().count();
            format!("{indent}{label}{}  {value}", " ".repeat(pad))
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn row(label: &str, value: impl Into<String>) -> (String, String) {
    (label.to_owned(), value.into())
}

/// Any value as indented `key: value` lines, for data without a format of its own.
pub fn readable(value: &ConfigValue) -> String {
    let mut lines = Vec::new();
    write_readable(value, 0, &mut lines);
    if lines.is_empty() {
        return "Done.".into();
    }
    lines.join("\n")
}

fn scalar(value: &ConfigValue) -> Option<String> {
    match value {
        ConfigValue::Null => Some("none".into()),
        ConfigValue::Bool(flag) => Some(if *flag { "yes" } else { "no" }.into()),
        ConfigValue::Number(_) => Some(number(Some(value))),
        ConfigValue::String(text) if !text.contains('\n') => Some(text.clone()),
        ConfigValue::Array(list) if list.is_empty() => Some("none".into()),
        ConfigValue::Object(entries) if entries.is_empty() => Some("none".into()),
        ConfigValue::Array(list)
            if list.iter().all(|item| {
                matches!(
                    item,
                    ConfigValue::Number(_) | ConfigValue::Bool(_) | ConfigValue::String(_)
                ) && !item.as_str().is_some_and(|text| text.contains('\n'))
            }) =>
        {
            let joined: Vec<String> = list.iter().filter_map(scalar).collect();
            let line = joined.join(", ");
            (line.chars().count() <= 100).then_some(line)
        }
        ConfigValue::NonFinite(name) => Some(name.clone()),
        _ => None,
    }
}

/// Keys stay as written, since people type them back into KnowledgeNestfile.yaml.
fn label(key: &str) -> String {
    key.to_owned()
}

/// The module names of a stage section, such as `{ modules: [{ use }] }`.
fn module_names(section: Option<&ConfigValue>) -> String {
    let names: Vec<String> = items(section.and_then(|section| section.get("modules")))
        .iter()
        .map(|module| field(module, "use"))
        .collect();
    if names.is_empty() {
        section.map_or("none".into(), |section| {
            scalar(section).unwrap_or_else(|| section.to_js_json())
        })
    } else {
        names.join(", ")
    }
}

fn defaults_text(data: &ConfigValue) -> String {
    let differences = items(data.get("differences"));
    let mut lines = Vec::new();
    if let Some(section) = present(data.get("applied")) {
        lines.push(format!(
            "Applied the current defaults of {} to KnowledgeNestfile.yaml.",
            js_string(Some(section))
        ));
    }
    if differences.is_empty() {
        if lines.is_empty() {
            lines.push("KnowledgeNestfile.yaml uses the current defaults.".into());
        }
        return lines.join("\n");
    }
    lines.push(format!(
        "{} of KnowledgeNestfile.yaml {} from the current defaults:",
        plural(differences.len(), "section", "sections"),
        if differences.len() == 1 {
            "differs"
        } else {
            "differ"
        }
    ));
    for difference in differences {
        let current = module_names(difference.get("current"));
        let proposed = module_names(difference.get("proposed"));
        let section = field(difference, "section");
        if current == proposed {
            lines.push(format!("  {section:<10}{current} (settings differ)"));
        } else {
            lines.push(format!("  {section:<10}yours:    {current}"));
            lines.push(format!("  {:<10}defaults: {proposed}", ""));
        }
    }
    lines.push(
        "Run knowledge-nest defaults --apply <section> to use the defaults of a section; --json shows every setting."
            .into(),
    );
    lines.join("\n")
}

fn write_readable(value: &ConfigValue, depth: usize, lines: &mut Vec<String>) {
    let pad = "  ".repeat(depth);
    match value {
        ConfigValue::Object(entries) => {
            for (key, item) in entries {
                match (scalar(item), item) {
                    (Some(text), _) => lines.push(format!("{pad}{}: {text}", label(key))),
                    (None, ConfigValue::String(text)) => {
                        lines.push(format!("{pad}{}:", label(key)));
                        for line in text.trim_end_matches('\n').split('\n') {
                            lines.push(format!("{pad}  {line}").trim_end().to_owned());
                        }
                    }
                    (None, _) => {
                        lines.push(format!("{pad}{}:", label(key)));
                        write_readable(item, depth + 1, lines);
                    }
                }
            }
        }
        ConfigValue::Array(list) => {
            for item in list {
                match scalar(item) {
                    Some(text) => lines.push(format!("{pad}- {text}")),
                    None => {
                        let mut nested = Vec::new();
                        write_readable(item, depth + 1, &mut nested);
                        if let Some(first) = nested.first_mut() {
                            let inner = "  ".repeat(depth + 1);
                            *first = format!("{pad}- {}", first.trim_start_matches(&inner));
                        }
                        lines.extend(nested);
                    }
                }
            }
        }
        ConfigValue::String(text) => {
            for line in text.trim_end_matches('\n').split('\n') {
                lines.push(format!("{pad}{line}"));
            }
        }
        other => lines.push(format!("{pad}{}", scalar(other).unwrap_or_default())),
    }
}

fn skipped_lines(data: &ConfigValue) -> Vec<String> {
    let skipped = items(data.get("skipped"));
    if skipped.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!(
        "Skipped {}:",
        plural(skipped.len(), "file", "files")
    )];
    for file in skipped {
        lines.push(format!(
            "  {}:{} — {}",
            js_string(file.get("source_id")),
            js_string(file.get("path")),
            js_string(file.get("reason"))
        ));
    }
    lines
}

fn stage_rows(data: &ConfigValue) -> Vec<(String, String)> {
    let ConfigValue::Object(stages) = data.get("stages").cloned().unwrap_or_default() else {
        return Vec::new();
    };
    stages
        .iter()
        .map(|(stage, result)| {
            if let Some(modules) = present(result.get("modules")) {
                let names: Vec<String> = items(Some(modules))
                    .iter()
                    .map(|module| {
                        string_field(module, "name")
                            .or_else(|| string_field(module, "use"))
                            .unwrap_or_default()
                            .to_owned()
                    })
                    .collect();
                return row(stage, names.join(", "));
            }
            row(
                stage,
                format!(
                    "{:<6} {:>6} ms",
                    if truthy(result.get("reused")) {
                        "reused"
                    } else {
                        "run"
                    },
                    number(result.get("elapsed_ms"))
                ),
            )
        })
        .collect()
}

/// `build`, `source`, `transform` and `index`, with or without `--dry-run`.
fn build_text(command: &str, data: &ConfigValue) -> String {
    let mut lines = Vec::new();
    if let Some(files) = data
        .get("files")
        .filter(|files| matches!(files, ConfigValue::Array(_)))
    {
        // A dry run: what would run, and nothing written.
        lines.push("Dry run: nothing was written.".to_owned());
        lines.extend(table(&stage_rows(data), "  "));
        let files = items(Some(files));
        let changed: Vec<&ConfigValue> = files
            .iter()
            .filter(|file| string_field(file, "change") != Some("unchanged"))
            .collect();
        lines.push(format!(
            "Files: {} ({} changed)",
            files.len(),
            changed.len()
        ));
        for file in changed.iter().take(20) {
            lines.push(format!(
                "  {:<9} {}:{}",
                js_string(file.get("change")),
                js_string(file.get("source_id")),
                js_string(file.get("path"))
            ));
        }
        if changed.len() > 20 {
            lines.push(format!("  and {} more", changed.len() - 20));
        }
        lines.extend(skipped_lines(data));
        for item in items(data.get("commands")) {
            lines.push(format!("Would run: {}", readable(item).replace('\n', "; ")));
        }
        if present(data.get("serving_changed")).is_some() {
            lines.push(format!(
                "Serving would change: {}",
                if truthy(data.get("serving_changed")) {
                    "yes"
                } else {
                    "no"
                }
            ));
        }
        return lines.join("\n");
    }
    if truthy(data.get("build_id")) {
        let reused = truthy(data.get("reused"));
        let files = present(data.get("files")).map_or(String::new(), |_| {
            format!(
                " from {} of {} files",
                field(data, "documents"),
                field(data, "files")
            )
        });
        lines.push(format!(
            "{} {}{files} in {} ms.",
            if reused { "Reused build" } else { "Built" },
            field(data, "build_id"),
            number(data.get("elapsed_ms"))
        ));
        if let Some(tag) = present(data.get("tag")) {
            lines.push(format!("Tag: {}", js_string(Some(tag))));
        }
    } else {
        lines.push(format!(
            "Ran {command} in {} ms.",
            number(data.get("elapsed_ms"))
        ));
        if present(data.get("files")).is_some() {
            lines.push(format!("Files: {}", field(data, "files")));
        }
    }
    lines.extend(table(&stage_rows(data), "  "));
    lines.extend(skipped_lines(data));
    if truthy(data.get("build_id")) {
        if truthy(data.get("serving_changed")) {
            lines.push("Now serving this build.".to_owned());
        } else {
            lines.push(format!(
                "The project still serves its previous build. Run knowledge-nest use {} to serve this one.",
                field(data, "build_id")
            ));
        }
        lines.push(
            "Next: knowledge-nest search \"<words>\" or knowledge-nest ask \"<question>\""
                .to_owned(),
        );
    }
    lines.join("\n")
}

fn search_text(data: &ConfigValue) -> String {
    let results = items(data.get("results"));
    let context = data.get("context");
    let body = context.and_then(|context| context.get("text"));
    if results.is_empty() || !truthy(body) {
        return "No matches. Try other words, or check knowledge-nest status for the serving build.".into();
    }
    let context = context.cloned().unwrap_or_default();
    let mut out = js_string(body).trim_end_matches('\n').to_owned();
    out.push_str(&format!(
        "\n\n{}; {} of {} characters.",
        plural(results.len(), "result", "results"),
        number(context.get("chars")),
        number(context.get("budget_chars"))
    ));
    out
}

/// Where a search result came from: `file:start-end — heading`.
fn place(result: &ConfigValue) -> String {
    let document = js_string(result.get("document_id"));
    let path = document
        .split_once(':')
        .map_or(document.as_str(), |(_, path)| path);
    let range = result
        .get("evidence")
        .and_then(|evidence| evidence.get("range"));
    let lines = match (
        range.and_then(|range| range.get("startLine")),
        range.and_then(|range| range.get("endLine")),
    ) {
        (Some(start), Some(end)) if start == end => format!(":{}", number(Some(start))),
        (Some(start), Some(end)) => format!(":{}-{}", number(Some(start)), number(Some(end))),
        _ => String::new(),
    };
    let page = present(result.get("page"))
        .map_or(String::new(), |page| format!(" p.{}", number(Some(page))));
    let heading = string_field(result, "heading")
        .filter(|heading| !heading.is_empty())
        .map_or(String::new(), |heading| format!(" — {heading}"));
    format!("{path}{lines}{page}{heading}")
}

/// `ask`: the answer with numbered markers, then each source with its lines.
pub fn answer_text(response: &ConfigValue, search: Option<&ConfigValue>) -> String {
    let content = items(response.get("output"))
        .first()
        .and_then(|output| items(output.get("content")).first())
        .cloned()
        .unwrap_or_default();
    let answer = js_string(content.get("text"));
    let annotations = items(content.get("annotations"));
    let citations = items(response.get("knowledge_nest_citations"));
    let results = search
        .map(|search| items(search.get("results")))
        .unwrap_or_default();
    // Each source once, numbered in order of first citation.
    let mut sources: Vec<(String, String)> = Vec::new();
    let mut markers: Vec<(usize, usize)> = Vec::new();
    for (index, annotation) in annotations.iter().enumerate() {
        let file = js_string(annotation.get("file_id"));
        let detail = citations.iter().find(|detail| {
            detail.get("annotation_index") == Some(&ConfigValue::Number(index as f64))
        });
        let range = detail.and_then(|detail| detail.get("source_range"));
        let found = results.iter().find(|result| {
            js_string(result.get("document_id")) == file
                && range.is_none_or(|range| result.get("range") == Some(range))
        });
        let key = format!(
            "{file}#{}",
            range.map(ConfigValue::to_js_json).unwrap_or_default()
        );
        let label = found.map_or_else(
            || {
                file.split_once(':')
                    .map_or(file.as_str(), |(_, path)| path)
                    .to_owned()
            },
            place,
        );
        let number = match sources.iter().position(|(existing, _)| *existing == key) {
            Some(position) => position + 1,
            None => {
                sources.push((key, label));
                sources.len()
            }
        };
        let at = match annotation.get("index") {
            Some(ConfigValue::Number(at)) => *at as usize,
            _ => answer.chars().count(),
        };
        if !markers.contains(&(at, number)) {
            markers.push((at, number));
        }
    }
    let mut out = String::new();
    let characters: Vec<char> = answer.chars().collect();
    for (position, character) in characters.iter().enumerate() {
        if *character == '\n' {
            push_markers(&mut out, &markers, position);
        }
        out.push(*character);
    }
    push_markers(&mut out, &markers, characters.len());
    if !sources.is_empty() {
        out.push_str("\n\nSources");
        for (index, (_, label)) in sources.iter().enumerate() {
            out.push_str(&format!("\n[{}] {label}", index + 1));
        }
    }
    out
}

fn push_markers(out: &mut String, markers: &[(usize, usize)], position: usize) {
    for (_, number) in markers.iter().filter(|(at, _)| *at == position) {
        out.push_str(&format!(" [{number}]"));
    }
}

fn status_text(data: &ConfigValue) -> String {
    if !truthy(data.get("build_id")) {
        return "No build yet. Add documents and run knowledge-nest build.".into();
    }
    let mut rows = vec![
        row("Build", field(data, "build_id")),
        row("Search", field(data, "search_config_id")),
    ];
    let report = present(data.get("evaluation_report"));
    let mut evaluation = field(data, "evaluation_status");
    if let Some(report) = report {
        let scores = match report.get("scores") {
            Some(ConfigValue::Object(scores)) => scores
                .iter()
                .map(|(name, value)| format!("{name} {}", score(Some(value))))
                .collect::<Vec<_>>()
                .join(", "),
            _ => String::new(),
        };
        let name = present(report.get("tag")).or(report.get("id"));
        evaluation = format!("{evaluation}: {scores} (report {})", js_string(name));
    }
    rows.push(row("Evaluation", evaluation));
    let skipped = items(data.get("skipped")).len();
    let mut documents = format!("{} ({skipped} skipped)", number(data.get("documents")));
    if let Some(changed) = present(data.get("documents_changed_since_evaluation")) {
        documents.push_str(&format!(
            ", {} changed since the evaluation",
            number(Some(changed))
        ));
    }
    rows.push(row("Documents", documents));
    let changed: Vec<String> = items(data.get("changed_config_sections"))
        .iter()
        .map(|section| js_string(Some(section)))
        .collect();
    if !changed.is_empty() {
        rows.push(row(
            "Changed",
            format!(
                "{} in KnowledgeNestfile.yaml since this build. Run knowledge-nest build to apply.",
                changed.join(", ")
            ),
        ));
    }
    if let Some(storage) = present(data.get("storage")) {
        rows.push(row(
            "Storage",
            format!(
                "index {}, corpus {}, state {}, originals {}",
                bytes(storage.get("build_index_logical_bytes")),
                bytes(storage.get("shared_corpus_logical_bytes")),
                bytes(storage.get("shared_state_logical_bytes")),
                bytes(storage.get("shared_originals_logical_bytes"))
            ),
        ));
    }
    if let Some(trials) = present(data.get("trials")) {
        let usage = trials.get("usage").cloned().unwrap_or_default();
        let runs = number(usage.get("runs"));
        if runs != "0" {
            rows.push(row(
                "Trials",
                format!(
                    "{runs} runs, {} API requests",
                    number(usage.get("api_requests"))
                ),
            ));
        }
    }
    let mut lines = table(&rows, "");
    lines.extend(skipped_lines(data));
    lines.join("\n")
}

fn validate_text(data: &ConfigValue) -> String {
    let mut lines = vec![format!(
        "KnowledgeNestfile.yaml is valid ({} project).",
        field(data, "type")
    )];
    if let Some(ConfigValue::Object(stages)) = data.get("stages") {
        let rows: Vec<(String, String)> = stages
            .iter()
            .map(|(stage, modules)| {
                let names: Vec<String> = items(Some(modules))
                    .iter()
                    .map(|module| js_string(module.get("name")))
                    .collect();
                row(stage, names.join(", "))
            })
            .collect();
        lines.extend(table(&rows, "  "));
    }
    lines.join("\n")
}

fn modules_text(data: &ConfigValue) -> String {
    match data {
        ConfigValue::Array(modules) => {
            let mut lines = Vec::new();
            let mut stage = String::new();
            for module in modules {
                let current = field(module, "stage");
                if current != stage {
                    if !lines.is_empty() {
                        lines.push(String::new());
                    }
                    lines.push(current.clone());
                    stage = current;
                }
                lines.push(format!(
                    "  {:<24}{}",
                    field(module, "name"),
                    field(module, "description")
                ));
            }
            lines.push(String::new());
            lines.push(
                "Run knowledge-nest modules describe <name> for the arguments of a module.".into(),
            );
            lines.join("\n")
        }
        module => {
            let mut lines = vec![
                format!(
                    "{} (version {})",
                    field(module, "name"),
                    number(module.get("version"))
                ),
                field(module, "description"),
                String::new(),
                format!(
                    "Stage: {}; role: {}; input: {}; output: {}",
                    field(module, "stage"),
                    field(module, "role"),
                    field(module, "input"),
                    field(module, "output")
                ),
            ];
            if let Some(ConfigValue::Object(args)) = module.get("args") {
                if !args.is_empty() {
                    lines.push(String::new());
                    lines.push("Arguments".into());
                }
                for (name, spec) in args {
                    let default = spec.get("default").map_or("-".into(), |value| {
                        scalar(value).unwrap_or_else(|| value.to_js_json())
                    });
                    let kind = match present(spec.get("choices")) {
                        Some(values) => format!(
                            "one of {}",
                            scalar(values).unwrap_or_else(|| values.to_js_json())
                        ),
                        None => field(spec, "type"),
                    };
                    let mut detail = format!("{kind}, default {default}");
                    match (present(spec.get("min")), present(spec.get("max"))) {
                        (Some(min), Some(max)) => {
                            detail.push_str(&format!(
                                ", {} to {}",
                                number(Some(min)),
                                number(Some(max))
                            ));
                        }
                        (Some(min), None) => {
                            detail.push_str(&format!(", at least {}", number(Some(min))))
                        }
                        _ => {}
                    }
                    if let Some(description) = string_field(spec, "description") {
                        detail.push_str(&format!(" — {description}"));
                    }
                    lines.push(format!("  {name:<20}{detail}"));
                }
            }
            let requires: Vec<String> = items(module.get("requires"))
                .iter()
                .map(|item| js_string(Some(item)))
                .collect();
            if !requires.is_empty() {
                lines.push(format!("Requires: {}", requires.join(", ")));
            }
            lines.join("\n")
        }
    }
}

fn tools_text(data: &ConfigValue) -> String {
    let mut lines: Vec<String> = items(Some(data))
        .iter()
        .map(|tool| {
            let modules: Vec<String> = items(tool.get("modules"))
                .iter()
                .map(|module| js_string(Some(module)))
                .collect();
            format!(
                "  {:<12}{}  (version {})",
                field(tool, "name"),
                modules.join(", "),
                truncate(&field(tool, "version"), 24)
            )
        })
        .collect();
    lines.insert(0, "Tools you can add:".into());
    lines.push("Run knowledge-nest add <tool> to install one.".into());
    lines.join("\n")
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(width - 1).collect();
    cut.push('…');
    cut
}

fn scores_lines(scores: Option<&ConfigValue>, indent: &str) -> Vec<String> {
    match scores {
        Some(ConfigValue::Object(scores)) => table(
            &scores
                .iter()
                .map(|(name, value)| row(name, score(Some(value))))
                .collect::<Vec<_>>(),
            indent,
        ),
        _ => Vec::new(),
    }
}

/// `eval` and `report show`: the scores first, then what was measured.
fn report_text(data: &ConfigValue, evaluated: bool) -> String {
    let name =
        present(data.get("tag")).map_or_else(|| field(data, "id"), |tag| js_string(Some(tag)));
    let mut lines = vec![format!(
        "{} {} ({} split, {}) on {}",
        if evaluated { "Evaluated" } else { "Report" },
        name,
        field(data, "split"),
        plural(
            data.get("cases")
                .and_then(|cases| match cases {
                    ConfigValue::Number(count) => Some(*count as usize),
                    _ => None,
                })
                .unwrap_or_else(|| items(data.get("rows")).len()),
            "case",
            "cases"
        ),
        field(data, "build_id")
    )];
    lines.extend(scores_lines(data.get("scores"), "  "));
    if let Some(evidence) = present(data.get("evidence")) {
        lines.push(format!(
            "Evidence: {} located, {} not located",
            number(evidence.get("resolved")),
            number(evidence.get("unresolved"))
        ));
    }
    if let Some(context) = present(data.get("context")) {
        lines.push(format!(
            "Context: {} characters on average (budget {})",
            number(context.get("average_chars")),
            number(data.get("budget_chars"))
        ));
    }
    if let Some(latency) = present(data.get("latency_ms")) {
        lines.push(format!(
            "Latency: {} ms median, {} ms p95",
            number(latency.get("50")),
            number(latency.get("95"))
        ));
    }
    let missed: Vec<&ConfigValue> = items(data.get("rows"))
        .iter()
        .filter(|row| {
            matches!(row.get("scores"), Some(ConfigValue::Object(scores))
                if scores.iter().any(|(_, value)| matches!(value, ConfigValue::Number(n) if *n < 1.0)))
        })
        .collect();
    if !missed.is_empty() {
        lines.push(format!("Cases below 1 ({}):", missed.len()));
        for case in missed.iter().take(10) {
            let scores = match case.get("scores") {
                Some(ConfigValue::Object(scores)) => scores
                    .iter()
                    .map(|(name, value)| format!("{name} {}", score(Some(value))))
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => String::new(),
            };
            lines.push(format!(
                "  {}  {}  {scores}",
                field(case, "id"),
                field(case, "group")
            ));
        }
    }
    if evaluated {
        lines.push(format!("Next: knowledge-nest report show {name}"));
    }
    lines.join("\n")
}

fn report_list_text(data: &ConfigValue) -> String {
    let groups = items(Some(data));
    if groups.is_empty() {
        return "No reports yet. Run knowledge-nest eval.".into();
    }
    let mut lines = Vec::new();
    for group in groups {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(format!(
            "Dataset {} ({} split)",
            field(group, "dataset_id"),
            field(group, "split")
        ));
        let columns: Vec<String> = items(group.get("columns"))
            .iter()
            .map(|column| js_string(Some(column)))
            .collect();
        let results = items(group.get("results"));
        let metrics: Vec<String> = results
            .iter()
            .filter_map(|result| match result.get("scores") {
                Some(ConfigValue::Object(scores)) => Some(
                    scores
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .next()
            .unwrap_or_default();
        let mut header = vec!["report".to_owned()];
        header.extend(metrics.iter().cloned());
        header.extend(columns.iter().cloned());
        let mut rows = vec![header];
        for result in results {
            let mut cells = vec![
                present(result.get("tag"))
                    .map_or_else(|| field(result, "id"), |tag| js_string(Some(tag))),
            ];
            for metric in &metrics {
                cells.push(score(
                    result.get("scores").and_then(|scores| scores.get(metric)),
                ));
            }
            for column in &columns {
                let value = result
                    .get("settings")
                    .and_then(|settings| settings.get(column));
                cells.push(value.map_or("-".into(), |value| {
                    scalar(value).unwrap_or_else(|| value.to_js_json())
                }));
            }
            rows.push(cells);
        }
        lines.extend(grid(&rows, "  "));
    }
    lines.join("\n")
}

/// Columns lined up under their headers.
fn grid(rows: &[Vec<String>], indent: &str) -> Vec<String> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| cell.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|row| {
            let cells: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(column, cell)| {
                    format!(
                        "{cell}{}",
                        " ".repeat(widths[column] - cell.chars().count())
                    )
                })
                .collect();
            format!("{indent}{}", cells.join("  "))
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn compare_text(data: &ConfigValue) -> String {
    let mut lines = vec![format!(
        "Compared {} with {} on {} common cases{}.",
        field(data, "a"),
        field(data, "b"),
        number(data.get("common_cases")),
        if truthy(data.get("provisional")) {
            " (provisional)"
        } else {
            ""
        }
    )];
    if !truthy(data.get("comparable")) {
        lines.push(
            "The reports used different data or methods; the deltas are not comparable.".into(),
        );
    }
    if let Some(ConfigValue::Object(scores)) = data.get("scores") {
        let mut rows = vec![vec![
            "metric".to_owned(),
            "delta".to_owned(),
            "wins".to_owned(),
            "losses".to_owned(),
            "ties".to_owned(),
        ]];
        for (name, value) in scores {
            rows.push(vec![
                name.clone(),
                match value.get("delta") {
                    Some(ConfigValue::Number(delta)) => format!("{delta:+.3}"),
                    other => score(other),
                },
                number(value.get("wins")),
                number(value.get("losses")),
                number(value.get("ties")),
            ]);
        }
        lines.extend(grid(&rows, "  "));
    }
    if let Some(ConfigValue::Object(differences)) = data.get("differences")
        && !differences.is_empty()
    {
        lines.push("Settings that differ:".into());
        for (name, value) in differences {
            let shown = |item: Option<&ConfigValue>| {
                item.map_or("-".into(), |item| {
                    scalar(item).unwrap_or_else(|| item.to_js_json())
                })
            };
            let pair = items(Some(value));
            let text = if pair.len() == 2 {
                format!("{} → {}", shown(pair.first()), shown(pair.get(1)))
            } else {
                shown(Some(value))
            };
            lines.push(format!("  {name}: {text}"));
        }
    }
    lines.join("\n")
}

fn eval_data_text(action: &str, data: &ConfigValue) -> String {
    match action {
        "add" => format!(
            "Added. The evaluation data {} now has {}.",
            field(data, "id"),
            plural(
                match data.get("cases") {
                    Some(ConfigValue::Number(count)) => *count as usize,
                    _ => 0,
                },
                "case",
                "cases"
            )
        ),
        "list" => {
            let datasets = items(data.get("datasets"));
            if datasets.is_empty() {
                return "No evaluation data yet. Add a case with knowledge-nest eval-data add --question <text> --doc <file> --quote <text>.".into();
            }
            let selected = field(data, "selected");
            let mut rows = vec![vec![
                String::new(),
                "id".to_owned(),
                "cases".to_owned(),
                "tags".to_owned(),
            ]];
            for dataset in datasets {
                let id = field(dataset, "id");
                let tags: Vec<String> = items(dataset.get("tags"))
                    .iter()
                    .map(|tag| js_string(Some(tag)))
                    .collect();
                rows.push(vec![
                    if selected.ends_with(&id) { "*" } else { "" }.to_owned(),
                    id,
                    number(dataset.get("cases")),
                    tags.join(", "),
                ]);
            }
            let mut lines = grid(&rows, "");
            lines.push("* the evaluation data that knowledge-nest eval uses".into());
            lines.join("\n")
        }
        "show" if present(data.get("cases")).is_some() => {
            let mut lines = vec![format!("Evaluation data {}", field(data, "id"))];
            for case in items(data.get("cases")) {
                lines.push(String::new());
                let kind = match present(case.get("kind")) {
                    Some(_) => format!(", {}", field(case, "kind")),
                    None => String::new(),
                };
                lines.push(format!(
                    "{}  [{}, {}{kind}]",
                    field(case, "id"),
                    field(case, "split"),
                    field(case, "state")
                ));
                lines.push(format!("  Q: {}", field(case, "question")));
                for evidence in items(case.get("evidence")) {
                    lines.push(format!(
                        "  {}: {}",
                        field(evidence, "doc"),
                        field(evidence, "quote").replace('\n', " ")
                    ));
                }
            }
            lines.join("\n")
        }
        _ => readable(data),
    }
}

fn evidence_map_text(action: &str, data: &ConfigValue) -> String {
    match action {
        "list" => {
            let maps = items(Some(data));
            if maps.is_empty() {
                return "No evidence maps yet. Run knowledge-nest evidence-map create.".into();
            }
            maps.iter()
                .map(|map| field(map, "id"))
                .collect::<Vec<_>>()
                .join("\n")
        }
        "create" | "show" if present(data.get("positions")).is_some() => {
            let mut lines = vec![format!(
                "Evidence map {} for {}",
                field(data, "id"),
                field(data, "build_id")
            )];
            let ConfigValue::Object(positions) = data.get("positions").cloned().unwrap_or_default()
            else {
                return lines.join("\n");
            };
            let located = positions
                .iter()
                .filter(|(_, position)| present(position.get("range")).is_some())
                .count();
            lines.push(format!("{located} of {} quotes located.", positions.len()));
            for (key, position) in &positions {
                let state = if truthy(position.get("confirmed")) {
                    "confirmed"
                } else if present(position.get("range")).is_some() {
                    "located"
                } else {
                    "not located"
                };
                lines.push(format!(
                    "  {:<11} {}: {}",
                    state,
                    field(position, "doc"),
                    field(position, "quote").replace('\n', " ")
                ));
                if present(position.get("range")).is_none() {
                    lines.push(format!("              key {key}"));
                }
            }
            lines.join("\n")
        }
        _ => readable(data),
    }
}

/// `serve --print-config`: an `mcpServers` entry to paste into an MCP client's settings.
fn mcp_config_text(data: &ConfigValue, context: Option<&ConfigValue>) -> String {
    let args = items(data.get("args"));
    let project = context
        .and_then(|context| context.get("project"))
        .or_else(|| {
            args.iter()
                .position(|arg| arg.as_str() == Some("--project"))
                .and_then(|at| args.get(at + 1))
        })
        .and_then(ConfigValue::as_str)
        .unwrap_or_default();
    let base = project
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let name: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = if name.trim_matches('-').is_empty() {
        "knowledge-nest".to_owned()
    } else {
        format!("knowledge-nest-{}", name.trim_matches('-'))
    };
    let server = match data.get("url") {
        Some(url) => crate::index_stage::object(vec![
            ("type", crate::index_stage::text("http")),
            ("url", url.clone()),
        ]),
        None => data.clone(),
    };
    let config = crate::index_stage::object(vec![(
        "mcpServers",
        ConfigValue::Object(vec![(name, server)]),
    )]);
    let mut out = String::new();
    pretty(&config, 0, &mut out);
    out
}

/// JSON with two-space indents, keys in their order.
fn pretty(value: &ConfigValue, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth + 1);
    match value {
        ConfigValue::Object(entries) if !entries.is_empty() => {
            out.push_str("{\n");
            for (index, (key, item)) in entries.iter().enumerate() {
                out.push_str(&format!(
                    "{pad}{}: ",
                    ConfigValue::String(key.clone()).to_js_json()
                ));
                pretty(item, depth + 1, out);
                out.push_str(if index + 1 < entries.len() {
                    ",\n"
                } else {
                    "\n"
                });
            }
            out.push_str(&format!("{}}}", "  ".repeat(depth)));
        }
        ConfigValue::Array(list) if !list.is_empty() => {
            out.push_str("[\n");
            for (index, item) in list.iter().enumerate() {
                out.push_str(&pad);
                pretty(item, depth + 1, out);
                out.push_str(if index + 1 < list.len() { ",\n" } else { "\n" });
            }
            out.push_str(&format!("{}]", "  ".repeat(depth)));
        }
        other => out.push_str(&other.to_js_json()),
    }
}

/// The text of a command's result, or `None` where the CLI prints it another way. `context` is
/// the search that `ask` answered from, or `{ project }` for `serve --print-config`.
pub fn text_output(
    command: &str,
    kind: &str,
    data: &ConfigValue,
    context: Option<&ConfigValue>,
) -> Option<String> {
    Some(match command {
        "build" | "source" | "transform" | "index" => build_text(command, data),
        "search" => search_text(data),
        "ask" => answer_text(data, context),
        "status" => status_text(data),
        "validate" if present(data.get("stages")).is_some() => validate_text(data),
        "validate" => format!(
            "KnowledgeNestfile.yaml is valid ({} project).",
            field(data, "type")
        ),
        "modules" => modules_text(data),
        "add" if matches!(data, ConfigValue::Array(_)) => tools_text(data),
        "defaults" => defaults_text(data),
        "ingress" if data.get("pending").is_some() && data.get("files").is_some() => {
            let mut lines = vec![format!(
                "Dry run: {} found, {} pending. Nothing was written.",
                plural(
                    match data.get("files") {
                        Some(ConfigValue::Number(count)) => *count as usize,
                        _ => 0,
                    },
                    "file",
                    "files"
                ),
                number(data.get("pending"))
            )];
            lines.extend(skipped_lines(data));
            lines.join("\n")
        }
        "use" => format!(
            "Now serving {} ({}).{}",
            field(data, "build_id"),
            field(data, "evaluation_status"),
            if truthy(data.get("restart_mcp")) {
                " Restart MCP clients that use this project."
            } else {
                ""
            }
        ),
        "tag" => format!(
            "Tagged {} as {}.",
            field(data, "build_id"),
            field(data, "tag")
        ),
        "eval" => report_text(data, true),
        "report" => match kind {
            "list" => report_list_text(data),
            "compare" => compare_text(data),
            _ => report_text(data, false),
        },
        "eval-data" => eval_data_text(kind, data),
        "evidence-map" => evidence_map_text(kind, data),
        "serve" => mcp_config_text(data, context),
        "query" if string_field(data, "status") == Some("insufficient_evidence") => {
            "The wiki pages do not answer this question. Add sources with knowledge-nest ingress, or ask another way.".into()
        }
        "query" if data.get("answer").is_some() => {
            let mut lines = vec![field(data, "answer")];
            let pages = items(data.get("pages"));
            if !pages.is_empty() {
                lines.push(String::new());
                lines.push("Pages".into());
                for page in pages {
                    lines.push(format!("  {}  {}", field(page, "id"), field(page, "title")));
                }
            }
            if let Some(id) = string_field(data, "id").filter(|id| !id.is_empty()) {
                lines.push(String::new());
                lines.push(format!("Save it as a page: knowledge-nest save-answer {id} <page> <title>"));
            }
            lines.join("\n")
        }
        "rules" if data.get("content").is_some() => field(data, "content").trim_end().to_owned(),
        "verify" if items(data.get("issues")).is_empty() && data.get("issues").is_some() => {
            "No issues found.".into()
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> ConfigValue {
        crate::eval_stage::parse(text).unwrap()
    }

    #[test]
    fn prints_values_as_indented_lines() {
        let value = json(
            r#"{"selected":null,"datasets":[{"id":"ed-1","cases":2,"tags":[]}],"note":"a\nb"}"#,
        );
        assert_eq!(
            readable(&value),
            "selected: none\ndatasets:\n  - id: ed-1\n    cases: 2\n    tags: none\nnote:\n  a\n  b"
        );
        assert_eq!(number(Some(&ConfigValue::Number(0.7639))), "0.764");
        assert_eq!(bytes(Some(&ConfigValue::Number(6027.0))), "5.9 KB");
    }

    #[test]
    fn marks_each_cited_source_once() {
        let response = json(
            r#"{"output":[{"content":[{"text":"点検は毎月。\n停止する。","annotations":[
            {"file_id":"docs:a.md","index":6},{"file_id":"docs:a.md","index":6},{"file_id":"docs:b.md","index":12}]}]}],
            "knowledge_nest_citations":[{"annotation_index":0,"source_range":[0,10]},{"annotation_index":1,"source_range":[0,10]},
            {"annotation_index":2,"source_range":[0,5]}]}"#,
        );
        let search = json(
            r#"{"results":[{"document_id":"docs:a.md","range":[0,10],"heading":"保全","page":null,
            "evidence":{"range":{"startLine":1,"endLine":7}}}]}"#,
        );
        assert_eq!(
            answer_text(&response, Some(&search)),
            "点検は毎月。 [1]\n停止する。 [2]\n\nSources\n[1] a.md:1-7 — 保全\n[2] b.md"
        );
    }

    #[test]
    fn leads_with_what_happened() {
        let build = json(
            r#"{"build_id":"bld-1","files":3,"documents":3,"skipped":[],"serving_changed":true,"reused":false,
            "stages":{"source":{"id":"src-1","reused":true,"elapsed_ms":18}},"elapsed_ms":119}"#,
        );
        let text = text_output("build", "", &build, None).unwrap();
        assert!(
            text.starts_with("Built bld-1 from 3 of 3 files in 119 ms.\n  source  reused"),
            "{text}"
        );
        let status = json(r#"{"build_id":null,"trials":{}}"#);
        assert!(
            text_output("status", "", &status, None)
                .unwrap()
                .starts_with("No build yet.")
        );
        let report = json(
            r#"{"id":"run-1","tag":"r1","split":"dev","cases":2,"build_id":"bld-1","scores":{"evidence_recall@context":0.5},
            "rows":[{"id":"case-1","group":"docs:a.md","scores":{"evidence_recall@context":0}}]}"#,
        );
        let text = text_output("eval", "", &report, None).unwrap();
        assert!(text.contains("evidence_recall@context  0.500"), "{text}");
        assert!(text.contains("case-1  docs:a.md"), "{text}");
        let launch =
            json(r#"{"command":"node","args":["cli.mjs","serve","--project","C:\\work\\my kb"]}"#);
        let expected = r#"{
  "mcpServers": {
    "knowledge-nest-my-kb": {
      "command": "node",
      "args": [
        "cli.mjs",
        "serve",
        "--project",
        "C:\\work\\my kb"
      ]
    }
  }
}"#;
        assert_eq!(text_output("serve", "", &launch, None).unwrap(), expected);
    }
}
