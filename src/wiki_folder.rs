//! The contents and the log of the folder of pages: `index.html` lists every page with a line
//! about it, and `log.html` what was done and when. Both are standalone files, written again from
//! the state after each command that may have changed them.
//!
//! This is the `index` and the `log` of an LLM-Wiki, kept as HTML beside the pages.

use std::collections::HashSet;

use base64::Engine;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::config_value::ConfigValue;
use crate::eval_stage::parse;
use crate::index_stage::{items, sql, string_field, truthy};
use crate::markdown::escape_html;
use crate::node_path;
use crate::transform_stage::StageError;
use crate::wiki_project::{
    Project, atomic_write, encode_uri_component, fs_error, pages, read_text, str_of, url_path,
};

/// The mark of a file this module wrote. A file of the same name without it is left alone.
const CATALOG_MARK: &str = "<!-- knowledge-nest:catalog -->";
const LOG_MARK: &str = "<!-- knowledge-nest:log -->";

/// How many works the log shows, newest first.
const LOG_LENGTH: usize = 300;

const STYLE: &str = concat!(
    ":root{--bg:#fff;--card:#fff;--text:#111113;--muted:#71717a;--line:#ebebee;--hover:#f4f4f6;--accent:#5b5bd6;--accent-soft:#eeeefb;color-scheme:light dark}",
    "@media(prefers-color-scheme:dark){:root{--bg:#0a0a0b;--card:#141416;--text:#ededef;--muted:#8b8b94;--line:#222226;--hover:#1b1b1f;--accent:#a3a3ff;--accent-soft:#1c1c3a}}",
    "*{box-sizing:border-box}[hidden]{display:none!important}",
    "body{margin:0;background:var(--bg);color:var(--text);font:15px/1.7 Inter,'SF Pro Text',-apple-system,'Segoe UI Variable Text','Segoe UI','Hiragino Sans','Noto Sans JP','Yu Gothic UI',Meiryo,sans-serif;-webkit-font-smoothing:antialiased}",
    "body::before{content:'';position:absolute;z-index:-1;inset:0 0 auto;height:320px;pointer-events:none;background:radial-gradient(40% 80% at 30% 0,color-mix(in srgb,var(--accent) 18%,transparent),transparent 70%),radial-gradient(30% 70% at 68% 0,rgba(236,72,153,.10),transparent 70%)}",
    "main{max-width:880px;margin:0 auto;padding:64px 24px 96px}",
    "a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline;text-underline-offset:3px}",
    ".kind{display:inline-block;font-size:11px;font-weight:600;padding:0 9px;border-radius:99px;border:1px solid var(--line);color:var(--muted);margin:0 0 14px}",
    "h1{font-size:44px;line-height:1.2;font-weight:700;letter-spacing:-.015em;font-feature-settings:'palt';margin:0 0 14px}",
    ".lead{font-size:19px;line-height:1.7;color:var(--muted);margin:0 0 6px;max-width:34em}",
    ".meta{font-size:13px;color:var(--muted);margin:0}",
    "#q{display:block;width:100%;margin:28px 0 0;padding:14px 16px;font:inherit;color:inherit;background:var(--card);border:1px solid var(--line);border-radius:12px}",
    "#q:focus{outline:none;border-color:var(--accent);box-shadow:0 0 0 3px var(--accent-soft)}",
    "section{margin-top:36px}",
    "h2{display:flex;align-items:baseline;gap:8px;font-size:15px;font-weight:650;margin:0 0 10px}h2 small{font-size:12px;font-weight:400;color:var(--muted)}",
    ".pages{list-style:none;margin:0;padding:0;border:1px solid var(--line);border-radius:14px;overflow:hidden;background:var(--card)}",
    ".pages li+li{border-top:1px solid var(--line)}",
    ".pages a{display:grid;grid-template-columns:minmax(0,1fr) auto;gap:2px 16px;padding:12px 16px;color:var(--text)}.pages a:hover{background:var(--hover);text-decoration:none}",
    ".pages strong{font-size:15px;font-weight:600}.pages small{font-size:12px;color:var(--muted);white-space:nowrap}",
    ".pages span{grid-column:1/-1;font-size:13px;color:var(--muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}",
    ".guide{margin-top:56px;padding-top:28px;border-top:1px solid var(--line)}.guide ul{margin:0;padding-left:1.3em;font-size:14px;color:var(--muted)}.guide li{margin:4px 0}",
    "code{background:var(--hover);color:var(--text);padding:1px 6px;border-radius:6px;font:13px ui-monospace,SFMono-Regular,Consolas,monospace}",
    ".log{list-style:none;margin:0;padding:0}.log>li{padding:16px 0;border-top:1px solid var(--line)}.log h2{margin:0 0 4px;font-weight:600}.log time{font:13px ui-monospace,SFMono-Regular,Consolas,monospace;color:var(--muted)}",
    ".log p{margin:0;font-size:14px;color:var(--muted)}.log ul{margin:6px 0 0;padding-left:1.3em;font-size:14px}",
    "@media(max-width:640px){main{padding:40px 18px 64px}h1{font-size:32px}}",
);

/// Narrows the list to the pages whose line holds every word typed.
const FILTER_SCRIPT: &str = "const q=document.getElementById('q'),rows=[...document.querySelectorAll('[data-text]')];q.addEventListener('input',()=>{const words=q.value.toLowerCase().split(/\\s+/).filter(Boolean);let shown=0;for(const row of rows){const hit=words.every((word)=>row.dataset.text.includes(word));row.hidden=!hit;if(hit)shown++}for(const group of document.querySelectorAll('[data-group]'))group.hidden=!group.querySelector('[data-text]:not([hidden])');document.getElementById('none').hidden=shown>0});";

/// The words of Markdown as plain text: no citation numbers, no link targets, no marks.
pub(crate) fn plain(markdown: &str) -> String {
    let mut text = String::new();
    let mut rest = markdown;
    while let Some(open) = rest.find('[') {
        text.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find(']') {
            Some(close) if close > 0 && after[..close].bytes().all(|b| b.is_ascii_digit()) => {
                rest = &after[close + 1..];
            }
            // A link keeps its words and loses its target.
            Some(close) if after[close + 1..].starts_with('(') => {
                let target = &after[close + 1..];
                match target.find(')') {
                    Some(end) => {
                        text.push_str(&after[..close]);
                        rest = &target[end + 1..];
                    }
                    None => {
                        text.push('[');
                        rest = after;
                    }
                }
            }
            _ => {
                text.push('[');
                rest = after;
            }
        }
    }
    text.push_str(rest);
    text.split_whitespace()
        .map(|word| word.trim_matches(['#', '*', '`', '|', '>', '-']))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn shortened(line: &str, length: usize) -> String {
    if line.chars().count() > length {
        format!("{}…", line.chars().take(length).collect::<String>())
    } else {
        line.to_owned()
    }
}

/// The start of a page as one line of plain text.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn snippet(markdown: &str) -> String {
    shortened(&plain(markdown), 90)
}

/// What a page is about, in a line: its first paragraph, or its first heading when it has none.
fn summary(markdown: &str) -> String {
    let blocks = || {
        markdown
            .split("\n\n")
            .map(str::trim)
            .filter(|block| !block.is_empty())
    };
    let first = blocks()
        .find(|block| !block.starts_with('#'))
        .or_else(|| blocks().next())
        .unwrap_or_default();
    shortened(&plain(first), 110)
}

/// A page as the contents list it.
pub(crate) struct Entry {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub citations: usize,
    /// The day it was last written, as `2026-01-02`.
    pub changed: String,
    pub archived: bool,
}

pub(crate) fn entries<'a>(pages: impl IntoIterator<Item = &'a ConfigValue>) -> Vec<Entry> {
    pages
        .into_iter()
        .map(|page| Entry {
            id: str_of(page, "id").to_owned(),
            title: match string_field(page, "title") {
                Some(title) if !title.is_empty() => title.to_owned(),
                _ => str_of(page, "id").to_owned(),
            },
            summary: summary(str_of(page, "markdown")),
            citations: items(page.get("citations")).len(),
            changed: str_of(page, "changed").chars().take(10).collect(),
            archived: truthy(page.get("archived")),
        })
        .collect()
}

/// The folder of a page ID: everything before its last part.
pub(crate) fn folder(id: &str) -> &str {
    id.rfind('/').map_or("", |at| &id[..at])
}

/// The entries by folder, in the order of their IDs. Pages outside every folder come first, and
/// archived pages last, each under a name of their own.
pub(crate) fn groups(entries: &[Entry]) -> Vec<(String, Vec<&Entry>)> {
    let mut groups: Vec<(String, Vec<&Entry>)> = Vec::new();
    let mut put = |name: &str, entry| match groups.iter_mut().find(|(group, _)| group == name) {
        Some((_, list)) => list.push(entry),
        None => groups.push((name.to_owned(), vec![entry])),
    };
    for entry in entries.iter().filter(|entry| !entry.archived) {
        if folder(&entry.id).is_empty() {
            put("トップ", entry);
        }
    }
    for entry in entries.iter().filter(|entry| !entry.archived) {
        if !folder(&entry.id).is_empty() {
            put(folder(&entry.id), entry);
        }
    }
    for entry in entries.iter().filter(|entry| entry.archived) {
        put("アーカイブ", entry);
    }
    groups
}

/// A page that holds every word of a search, and the passage around the first of them.
pub(crate) struct Hit<'a> {
    pub page: &'a ConfigValue,
    pub passage: String,
}

/// The words of a search: lower case, split at spaces.
pub(crate) fn search_words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// The pages whose title or text holds every word. Pages with a word in their title come first.
pub(crate) fn search<'a>(
    pages: impl IntoIterator<Item = &'a ConfigValue>,
    query: &str,
) -> Vec<Hit<'a>> {
    let words = search_words(query);
    if words.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(bool, Hit<'a>)> = pages
        .into_iter()
        .filter_map(|page| {
            let title = str_of(page, "title").to_lowercase();
            let text = plain(str_of(page, "markdown"));
            let lower = text.to_lowercase();
            if !words
                .iter()
                .all(|word| title.contains(word) || lower.contains(word))
            {
                return None;
            }
            // The passage starts a little before the first word found in the text.
            let found = words.iter().filter_map(|word| lower.find(word)).min();
            let start = found.map_or(0, |at| lower[..at].chars().count().saturating_sub(24));
            let passage: String = text.chars().skip(start).take(130).collect();
            let passage = format!(
                "{}{}{}",
                if start > 0 { "…" } else { "" },
                passage.trim(),
                if text.chars().count() > start + 130 {
                    "…"
                } else {
                    ""
                }
            );
            let in_title = words.iter().any(|word| title.contains(word));
            Some((in_title, Hit { page, passage }))
        })
        .collect();
    hits.sort_by_key(|(in_title, _)| !in_title);
    hits.into_iter().map(|(_, hit)| hit).collect()
}

/// Escaped text with each word of a search inside `<mark>`.
pub(crate) fn marked(text: &str, words: &[String]) -> String {
    let lower = text.to_lowercase();
    // Lower case of the same length keeps the places of the letters.
    if lower.len() != text.len() {
        return escape_html(text);
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for word in words.iter().filter(|word| !word.is_empty()) {
        let mut from = 0;
        while let Some(at) = lower[from..].find(word.as_str()) {
            ranges.push((from + at, from + at + word.len()));
            from += at + word.len();
        }
    }
    ranges.sort_unstable();
    let mut out = String::new();
    let mut last = 0;
    for (start, end) in ranges {
        if start < last || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        out += &escape_html(&text[last..start]);
        out += &format!("<mark>{}</mark>", escape_html(&text[start..end]));
        last = end;
    }
    out + &escape_html(&text[last..])
}

/// The path of a page's file from the folder of pages, as a link.
pub(crate) fn page_href(id: &str) -> String {
    id.split('/')
        .map(encode_uri_component)
        .collect::<Vec<_>>()
        .join("/")
        + ".html"
}

fn document(title: &str, mark: &str, body: &str, script: Option<&str>) -> String {
    let (policy, script) = match script {
        Some(script) => (
            format!(
                "; script-src 'sha256-{}'",
                base64::engine::general_purpose::STANDARD.encode(Sha256::digest(script.as_bytes()))
            ),
            format!("<script>{script}</script>"),
        ),
        None => (String::new(), String::new()),
    };
    format!(
        "<!doctype html>\n<html lang=\"ja\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'none'{policy}\">\n<title>{}</title>\n<style>{STYLE}</style>\n{mark}\n<main>{body}</main>\n{script}</html>\n",
        escape_html(title)
    )
}

/// What the contents show of the project besides its pages.
pub(crate) struct About<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub purpose: &'a str,
    pub out_of_scope: &'a str,
    pub sources: usize,
    pub pending: usize,
    /// The link to the rules file from the folder of pages, when it is in the project.
    pub rules: Option<String>,
    pub log: bool,
}

/// `index.html`: every page by folder with a line about it, a box that narrows the list, and
/// how the pages are kept.
pub(crate) fn catalog(about: &About<'_>, entries: &[Entry]) -> String {
    let active = entries.iter().filter(|entry| !entry.archived).count();
    let changed = entries.iter().map(|entry| entry.changed.as_str()).max();
    let mut body = format!(
        "<header><p class=\"kind\">{}</p><h1>{}</h1>",
        escape_html(about.kind),
        escape_html(about.title)
    );
    if !about.purpose.is_empty() {
        body += &format!("<p class=\"lead\">{}</p>", escape_html(about.purpose));
    }
    if !about.out_of_scope.is_empty() {
        body += &format!(
            "<p class=\"meta\">範囲外：{}</p>",
            escape_html(about.out_of_scope)
        );
    }
    body += &format!(
        "<p class=\"meta\">ページ {active} 件 · 原本 {} 件{}{}</p>",
        about.sources,
        if about.pending > 0 {
            format!("（未反映 {} 件）", about.pending)
        } else {
            String::new()
        },
        changed
            .filter(|day| !day.is_empty())
            .map_or_else(String::new, |day| format!(
                " · 最終更新 {}",
                escape_html(day)
            ))
    );
    body += "<input id=\"q\" type=\"search\" placeholder=\"ページを絞り込む（題名と要約）\" aria-label=\"ページを絞り込む\"></header>";
    if entries.is_empty() {
        body += "<section><p class=\"meta\">まだページがありません。<code>nest ingress</code> に資料を渡すと、AI が読んでページを書きます。</p></section>";
    }
    for (name, list) in groups(entries) {
        body += &format!(
            "<section data-group><h2>{}<small>{}</small></h2><ul class=\"pages\">",
            escape_html(&name),
            list.len()
        );
        for entry in list {
            let line = format!("{} {} {}", entry.title, entry.summary, entry.id).to_lowercase();
            body += &format!(
                "<li data-text=\"{}\"><a href=\"{}\"><strong>{}</strong><small>出典 {} · {}</small><span>{}</span></a></li>",
                escape_html(&line),
                escape_html(&page_href(&entry.id)),
                escape_html(&entry.title),
                entry.citations,
                escape_html(&entry.changed),
                escape_html(&entry.summary)
            );
        }
        body += "</ul></section>";
    }
    body += "<p id=\"none\" class=\"meta\" hidden>該当するページがありません。</p>";
    body += concat!(
        "<section class=\"guide\"><h2>この Wiki の保守</h2><ul>",
        "<li>ページは AI が資料を読んで書き、Knowledge-Nest が引用を確かめて保存します。このフォルダの HTML を直接書き換えても、次の更新で上書きされます。</li>",
        "<li>資料を足す：<code>nest ingress &lt;ファイルか URL&gt;</code></li>",
        "<li>質問する：<code>nest query \"質問\"</code>（本文の検索は <code>nest serve</code> の画面でもできます）</li>",
        "<li>ページを直す：<code>nest instruct &lt;ページ ID&gt; \"指示\"</code></li>",
        "<li>点検する：<code>nest verify</code>（<code>--content</code> を付けると、ページ同士の食い違いも AI が調べます）</li>",
    );
    if let Some(rules) = &about.rules {
        body += &format!(
            "<li>書き方の決まり：<a href=\"{}\">rules.md</a>（<code>nest instruct --persistent \"決まり\"</code> で足します）</li>",
            escape_html(rules)
        );
    }
    if about.log {
        body += "<li>これまでの作業：<a href=\"log.html\">作業の記録</a></li>";
    }
    body += "</ul></section>";
    document(about.title, CATALOG_MARK, &body, Some(FILTER_SCRIPT))
}

fn command_label(command: &str) -> &'static str {
    match command {
        "ingress" => "取り込み",
        "query" => "質問",
        "instruct" => "指示",
        "verify" => "検査",
        "archive" => "アーカイブ",
        "undo" => "取り消し",
        "save-answer" => "回答の保存",
        _ => "作業",
    }
}

/// `log.html`: the works that finished, newest first. Each starts with its day and its command,
/// as `[2026-01-02] ingress | …`.
pub(crate) fn log(title: &str, works: &[ConfigValue], known: &HashSet<String>) -> String {
    let mut body = format!(
        "<header><p class=\"kind\">作業の記録</p><h1>{}</h1><p class=\"meta\"><a href=\"index.html\">目録へ戻る</a> · 新しい順、最大 {LOG_LENGTH} 件</p></header><ul class=\"log\">",
        escape_html(title)
    );
    for work in works {
        let command = str_of(work, "command");
        let result = work.get("result");
        let field = |name: &str| result.and_then(|result| result.get(name));
        let mut notes: Vec<String> = Vec::new();
        if let Some(ConfigValue::Number(sources)) = field("sources") {
            notes.push(format!(
                "原本 {} 件",
                crate::config_value::js_number(*sources)
            ));
        }
        let written = items(field("pages"));
        if command != "query" && !written.is_empty() {
            notes.push(format!("ページ {} 件", written.len()));
        }
        if let Some(ConfigValue::Array(issues)) = field("issues") {
            notes.push(format!("指摘 {} 件", issues.len()));
        }
        if let Some(page) = string_field(work, "page").filter(|page| !page.is_empty()) {
            notes.push(page.to_owned());
        }
        let day: String = str_of(work, "finished_at").chars().take(10).collect();
        body += &format!(
            "<li><h2><time>[{}] {}</time> | {}</h2>",
            escape_html(&day),
            escape_html(command),
            command_label(command)
        );
        if !notes.is_empty() {
            body += &format!("<p>{}</p>", escape_html(&notes.join(" · ")));
        }
        if command != "query" && !written.is_empty() {
            body += "<ul>";
            for page in written {
                let (id, title, new) = match page {
                    ConfigValue::String(id) => (id.as_str(), id.as_str(), false),
                    page => (
                        str_of(page, "id"),
                        string_field(page, "title").unwrap_or_else(|| str_of(page, "id")),
                        matches!(page.get("previous"), Some(ConfigValue::Null)),
                    ),
                };
                let name = if known.contains(id) {
                    format!(
                        "<a href=\"{}\">{}</a>",
                        escape_html(&page_href(id)),
                        escape_html(title)
                    )
                } else {
                    escape_html(title)
                };
                body += &format!("<li>{name}{}</li>", if new { "（新規）" } else { "" });
            }
            body += "</ul>";
        }
        body += "</li>";
    }
    body += "</ul>";
    document(&format!("作業の記録 | {title}"), LOG_MARK, &body, None)
}

/// Writes a file of this module, unless a file without its mark has the name.
fn write(path: &str, mark: &str, content: &str) -> Result<(), StageError> {
    match read_text(path)? {
        Some(existing) if !existing.contains(mark) || existing == content => Ok(()),
        _ => {
            let parent = node_path::dirname(path);
            std::fs::create_dir_all(&parent).map_err(|error| fs_error("mkdir", &parent, &error))?;
            atomic_write(path, content.as_bytes())
        }
    }
}

/// Writes `index.html` and `log.html` in the folder of pages from the state. A page that has
/// one of these names keeps its file.
pub(crate) fn sync(project: &Project<'_>, connection: &Connection) -> Result<(), StageError> {
    let all = pages(connection, true)?;
    let taken = |name: &str| {
        all.iter()
            .any(|page| str_of(page, "id").eq_ignore_ascii_case(name))
    };
    let known: HashSet<String> = all
        .iter()
        .map(|page| str_of(page, "id").to_owned())
        .collect();
    let index = project.publication_path("index")?;
    let folder = node_path::dirname(&index);
    let (sources, pending) = connection
        .query_row(
            "SELECT count(*), coalesce(sum(reflected = 0), 0) FROM wiki_sources",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap_or((0, 0));
    let works: Vec<ConfigValue> = match connection.prepare(&format!(
        "SELECT data FROM wiki_work ORDER BY rowid DESC LIMIT {LOG_LENGTH}"
    )) {
        Ok(mut statement) => statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sql)?
            .filter_map(Result::ok)
            .filter_map(|data| parse(&data).ok())
            .filter(|work| string_field(work, "status") == Some("succeeded"))
            .collect(),
        Err(_) => Vec::new(),
    };
    let rules = project.rules_file();
    let rules = std::path::Path::new(&rules).is_file().then(|| {
        let relative = node_path::relative(&folder, &rules);
        if node_path::is_absolute(&relative) {
            crate::source_load::file_url(&relative)
        } else {
            url_path(&relative)
        }
    });
    // A page file of an earlier form is written again in this one.
    let earlier = |id: &str| -> Result<bool, StageError> {
        Ok(read_text(&project.publication_path(id)?)?
            .is_some_and(|existing| !existing.contains(crate::wiki_html::PAGE_MARK)))
    };
    for page in &all {
        if earlier(str_of(page, "id"))? {
            project.save_page(page)?;
        }
        let archive = string_field(page, "archive_id").filter(|id| !id.is_empty());
        if let (true, Some(archive), ConfigValue::Object(entries)) =
            (truthy(page.get("archived")), archive, page)
        {
            if earlier(archive)? {
                let mut copy = entries.clone();
                crate::index_stage::assign(
                    &mut copy,
                    "id",
                    Some(crate::index_stage::text(archive)),
                );
                crate::index_stage::assign(&mut copy, "archive_id", None);
                project.save_page(&ConfigValue::Object(copy))?;
            }
        }
    }
    let about = &project.request.about;
    let title = str_of(about, "title");
    if !taken("log") {
        write(
            &project.publication_path("log")?,
            LOG_MARK,
            &log(title, &works, &known),
        )?;
    }
    if !taken("index") {
        write(
            &index,
            CATALOG_MARK,
            &catalog(
                &About {
                    kind: if project.para() { "PARA" } else { "Wiki" },
                    title,
                    purpose: str_of(about, "purpose"),
                    out_of_scope: str_of(about, "out_of_scope"),
                    sources: usize::try_from(sources).unwrap_or(0),
                    pending: usize::try_from(pending).unwrap_or(0),
                    rules,
                    log: !taken("log"),
                },
                &entries(&all),
            ),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_stage::{object, text};

    fn page(id: &str, title: &str, markdown: &str, archived: bool) -> ConfigValue {
        object(vec![
            ("id", text(id)),
            ("title", text(title)),
            ("markdown", text(markdown)),
            (
                "citations",
                ConfigValue::Array(vec![object(vec![("id", text("1"))])]),
            ),
            ("archived", ConfigValue::Bool(archived)),
            ("changed", text("2026-11-22T13:03:16.309Z")),
        ])
    }

    fn sample() -> Vec<ConfigValue> {
        vec![
            page(
                "a/scrum",
                "スクラム",
                "スクラムは[スプリント](sprint.html)で進める枠組みである。 [1]\n\n## 役割\n\nPO がいる。 [1]",
                false,
            ),
            page(
                "a/sprint",
                "スプリント",
                "## 期間\n\n1〜4週間の Sprint。 [1]",
                false,
            ),
            page("home", "はじめに", "この Wiki の読み方。 [1]", false),
            page("old/x", "古いページ", "しまった。 [1]", true),
        ]
    }

    #[test]
    fn the_summary_is_the_first_paragraph_without_marks() {
        let entries = entries(&sample());
        assert_eq!(
            entries[0].summary,
            "スクラムはスプリントで進める枠組みである。"
        );
        assert_eq!(entries[1].summary, "1〜4週間の Sprint。");
        assert_eq!(summary("## 概要\n\n## 手順"), "概要");
        assert_eq!(entries[0].changed, "2026-11-22");
        assert_eq!(entries[0].citations, 1);
    }

    #[test]
    fn groups_are_folders_with_loose_pages_first_and_archived_last() {
        let entries = entries(&sample());
        let names: Vec<(String, Vec<&str>)> = groups(&entries)
            .into_iter()
            .map(|(name, list)| (name, list.iter().map(|entry| entry.id.as_str()).collect()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("トップ".to_owned(), vec!["home"]),
                ("a".to_owned(), vec!["a/scrum", "a/sprint"]),
                ("アーカイブ".to_owned(), vec!["old/x"]),
            ]
        );
    }

    #[test]
    fn a_search_needs_every_word_and_puts_titles_first() {
        let pages = sample();
        let ids = |query: &str| -> Vec<&str> {
            search(&pages, query)
                .iter()
                .map(|hit| str_of(hit.page, "id"))
                .collect()
        };
        assert_eq!(ids("スプリント"), vec!["a/sprint", "a/scrum"]);
        assert_eq!(ids("sprint 週間"), vec!["a/sprint"]);
        assert_eq!(ids("スクラム 週間"), Vec::<&str>::new());
        assert_eq!(ids("  "), Vec::<&str>::new());
        assert_eq!(
            search(&pages, "po")[0].passage,
            "…クラムはスプリントで進める枠組みである。 役割 PO がいる。"
        );
    }

    #[test]
    fn marks_the_words_of_a_search() {
        let words = search_words("po <b");
        assert_eq!(
            marked("PO と <b>", &words),
            "<mark>PO</mark> と <mark>&lt;b</mark>&gt;"
        );
    }

    #[test]
    fn the_contents_link_every_page_and_carry_their_script() {
        let html = catalog(
            &About {
                kind: "Wiki",
                title: "保全 <ノート>",
                purpose: "手順をまとめる",
                out_of_scope: "",
                sources: 3,
                pending: 1,
                rules: Some("../rules.md".into()),
                log: true,
            },
            &entries(&sample()),
        );
        assert!(html.contains(CATALOG_MARK));
        assert!(html.contains("<title>保全 &lt;ノート&gt;</title>"));
        assert!(html.contains(
            "<p class=\"meta\">ページ 3 件 · 原本 3 件（未反映 1 件） · 最終更新 2026-11-22</p>"
        ));
        assert!(html.contains("<section data-group><h2>a<small>2</small></h2><ul class=\"pages\"><li data-text=\"スクラム スクラムはスプリントで進める枠組みである。 a/scrum\"><a href=\"a/scrum.html\"><strong>スクラム</strong><small>出典 1 · 2026-11-22</small>"));
        assert!(html.contains("<a href=\"../rules.md\">rules.md</a>"));
        assert!(html.contains("<a href=\"log.html\">作業の記録</a>"));
        let hash = base64::engine::general_purpose::STANDARD
            .encode(Sha256::digest(FILTER_SCRIPT.as_bytes()));
        assert!(html.contains(&format!("script-src 'sha256-{hash}'")));
        assert!(html.contains(&format!("<script>{FILTER_SCRIPT}</script>")));
    }

    #[test]
    fn the_log_starts_each_work_with_its_day_and_command() {
        let work = object(vec![
            ("command", text("ingress")),
            ("status", text("succeeded")),
            ("finished_at", text("2026-11-22T13:03:16.341Z")),
            (
                "result",
                object(vec![
                    ("sources", ConfigValue::Number(2.0)),
                    (
                        "pages",
                        ConfigValue::Array(vec![
                            object(vec![
                                ("id", text("a/scrum")),
                                ("title", text("スクラム")),
                                ("previous", ConfigValue::Null),
                            ]),
                            object(vec![
                                ("id", text("gone")),
                                ("title", text("消えた")),
                                ("previous", text("x")),
                            ]),
                        ]),
                    ),
                ]),
            ),
        ]);
        let known: HashSet<String> = ["a/scrum".to_owned()].into();
        let html = log("ノート", &[work], &known);
        assert!(html.contains(LOG_MARK));
        assert!(html.contains("<li><h2><time>[2026-11-22] ingress</time> | 取り込み</h2><p>原本 2 件 · ページ 2 件</p><ul><li><a href=\"a/scrum.html\">スクラム</a>（新規）</li><li>消えた</li></ul></li>"));
    }
}
