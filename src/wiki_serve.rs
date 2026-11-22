//! The screen of Wiki and PARA projects (spec 15 and 28.12 step 5b): pages, sources and work in
//! three columns, and forms whose instructions run as recorded work, one after another.
//!
//! The checks of each request (host, origin, credentials, size, time) are the ones of the RAG
//! server (`serve`), with the rules a browser needs: its own origin, Basic credentials and cookies.
//! The pages and replies are the ones `serveWiki` in `project-wiki.mjs` wrote.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use base64::Engine;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response, StatusCode};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::Cancellation;
use crate::config_value::{ConfigValue, js_number};
use crate::eval_stage::{open_state, parse};
use crate::index_stage::{items, object, sql, string_field, text, truthy};
use crate::markdown::{escape_html, render};
use crate::node_path;
use crate::serve::{
    AbortOnDrop, Body, Reply, Running, ServeSettings, header, hostname, request_target,
};
use crate::transform_stage::StageError;
use crate::wiki_html::CONTENT_SECURITY_POLICY;
use crate::wiki_project::{
    CATEGORIES, WikiRequest, encode_uri_component, link_targets, linked_page, pages, realpath,
    str_of,
};

/// The screen of a project: the base of its commands, and the validated `serve` section.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiServeRequest {
    pub base: WikiRequest,
    pub settings: ServeSettings,
    /// The token of `auth: token`, read from its environment variable.
    #[serde(default)]
    pub token: Option<String>,
}

/// What the screen tells JavaScript: where it listens.
pub trait WikiServeHost: Send + Sync {
    fn listening(&self, port: u16);
}

const WORK_STAGES: [(&str, &str); 13] = [
    ("queued", "順番待ち"),
    ("starting", "処理を開始"),
    ("finding_sources", "資料を確認中"),
    ("extracting_sources", "テキストを抽出中"),
    ("indexing_sources", "資料を索引に追加中"),
    ("writing_pages", "ページを作成中"),
    ("validating_citations", "出典を確認中"),
    ("saving_pages", "ページを保存中"),
    ("answering", "回答を作成中"),
    ("checking_citations", "引用とリンクを検査中"),
    ("reviewing_content", "関連する資料とページを検査中"),
    ("succeeded", "完了"),
    ("failed", "失敗"),
];

const STYLE: &str = concat!(
    ":root{--bg:#f6f5f2;--side:#eeece7;--card:#fff;--text:#1d2127;--muted:#68707c;--line:#e3e0d9;--hover:#e6e3dc;--accent:#2563eb;--accent-text:#fff;--accent-soft:#e7efff;--ok:#15803d;--bad:#dc2626;--warn-bg:#fff1cc;--warn:#8a5a00;--shadow:0 1px 2px rgba(20,20,20,.04),0 6px 20px rgba(20,20,20,.04);color-scheme:light dark}",
    "@media(prefers-color-scheme:dark){:root{--bg:#15171b;--side:#1b1e23;--card:#21252b;--text:#e7e9ec;--muted:#9aa3ad;--line:#30353c;--hover:#2a2f36;--accent:#7aa7ff;--accent-text:#0c1424;--accent-soft:#1f2b44;--ok:#4ade80;--bad:#f87171;--warn-bg:#3a2f12;--warn:#f5c565;--shadow:none}}",
    "*{box-sizing:border-box}",
    "body{margin:0;background:var(--bg);color:var(--text);font:15px/1.75 system-ui,-apple-system,'Segoe UI','Hiragino Sans','Noto Sans JP','Yu Gothic UI',Meiryo,sans-serif}",
    "a{color:var(--accent);text-underline-offset:2px}",
    "main{display:grid;grid-template-columns:290px minmax(0,1fr);min-height:100vh}",
    "main>nav{background:var(--side);border-right:1px solid var(--line);position:sticky;top:0;height:100vh;overflow:auto;padding:18px 14px 32px}",
    ".brand{display:flex;align-items:center;gap:10px;color:var(--text);text-decoration:none;padding:2px 6px 14px}",
    ".brand strong{font-size:16px;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}",
    ".logo{flex:none;width:28px;height:28px;border-radius:8px;background:linear-gradient(135deg,#2563eb,#7c3aed);box-shadow:inset 0 0 0 1px rgba(255,255,255,.2)}",
    ".kind{margin-left:auto;flex:none;font-size:11px;font-weight:700;padding:1px 9px;border-radius:99px;background:var(--accent-soft);color:var(--accent)}",
    ".field{display:block;margin:8px 0 0}.field span{display:block;font-size:12px;color:var(--muted);margin-bottom:3px}",
    "input,textarea{font:inherit;font-size:14px;line-height:1.5;color:inherit;background:var(--bg);border:1px solid var(--line);border-radius:8px;padding:7px 10px;width:100%}",
    "input:focus,textarea:focus{outline:none;border-color:var(--accent);box-shadow:0 0 0 3px var(--accent-soft)}",
    "input[type=file]{padding:5px;font-size:12px;color:var(--muted)}",
    "input[type=file]::file-selector-button{font:inherit;border:1px solid var(--line);background:var(--card);color:var(--text);border-radius:6px;padding:3px 10px;margin-right:8px;cursor:pointer}",
    "button{font:inherit;font-size:13px;line-height:1.4;border:1px solid var(--line);background:var(--card);color:var(--text);border-radius:8px;padding:6px 12px;cursor:pointer;margin:8px 6px 0 0}",
    "button:hover{border-color:var(--accent);color:var(--accent)}",
    "button.primary{background:var(--accent);border-color:var(--accent);color:var(--accent-text);font-weight:600}button.primary:hover{filter:brightness(1.08);color:var(--accent-text)}",
    ".empty{font-size:13px;color:var(--muted);margin:0 8px}",
    ".tree{list-style:none;margin:0;padding:0}.tree .tree{margin-left:10px;padding-left:8px;border-left:1px solid var(--line)}",
    ".tree summary{list-style:none;display:flex;align-items:center;gap:6px;cursor:pointer;padding:3px 8px;border-radius:6px;font-size:13.5px;font-weight:600;color:var(--muted)}",
    ".tree summary::-webkit-details-marker{display:none}",
    ".tree summary::before{content:'';flex:none;width:6px;height:6px;border:solid currentColor;border-width:0 1.6px 1.6px 0;transform:rotate(-45deg);transition:transform .15s;margin:0 3px 0 2px}",
    ".tree details[open]>summary::before{transform:rotate(45deg)}",
    ".tree summary:hover,.tree a:hover{background:var(--hover)}",
    ".tree a{display:flex;align-items:center;gap:8px;padding:4px 8px;border-radius:6px;color:var(--text);text-decoration:none;font-size:14px;line-height:1.45}",
    ".tree a::before{content:'';flex:none;width:15px;height:15px;background:currentColor;opacity:.45;-webkit-mask:var(--icon) center/contain no-repeat;mask:var(--icon) center/contain no-repeat}",
    ".tree .name{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}",
    ".tree a[aria-current]{background:var(--accent-soft);color:var(--accent);font-weight:600}.tree a[aria-current]::before{opacity:1}",
    ".tree .archived{color:var(--muted)}",
    ".badge{margin-left:auto;flex:none;font-size:10.5px;font-weight:700;padding:0 7px;border-radius:99px;background:var(--warn-bg);color:var(--warn)}",
    ".page{--icon:url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Crect x='4' y='3' width='16' height='18' rx='2'/%3E%3Cpath d='M8 8h8M8 12h8M8 16h5'/%3E%3C/svg%3E\")}",
    ".file{--icon:url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M14 3H6a1 1 0 0 0-1 1v16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8z'/%3E%3Cpath d='M14 3v5h5'/%3E%3C/svg%3E\")}",
    ".web{--icon:url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Ccircle cx='12' cy='12' r='9'/%3E%3Cpath d='M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18'/%3E%3C/svg%3E\")}",
    ".view{padding:28px 40px 64px;min-width:0}.view>*{max-width:860px;margin-left:auto;margin-right:auto}",
    ".crumbs{display:flex;flex-wrap:wrap;gap:6px;font-size:13px;color:var(--muted);margin-bottom:12px}.crumbs span+span::before{content:'/';margin-right:6px;opacity:.5}",
    "article{background:var(--card);border:1px solid var(--line);border-radius:16px;padding:36px 44px;box-shadow:var(--shadow);overflow-wrap:anywhere}",
    "article h1{font-size:28px;line-height:1.35;letter-spacing:-.01em;margin:0 0 16px}article.top h1{font-size:32px}",
    "article h2{font-size:16px;margin:32px 0 10px;padding-top:20px;border-top:1px solid var(--line)}",
    "article p:empty{display:none}article ol,article ul{padding-left:1.4em}article li{margin:4px 0}article li::marker{color:var(--muted)}",
    "article code{background:var(--bg);padding:1px 5px;border-radius:5px;font-size:.92em}",
    "article table{border-collapse:collapse;width:100%}article th,article td{border:1px solid var(--line);padding:6px 10px;text-align:left}",
    ".meta{color:var(--muted);font-size:13px;margin:-8px 0 8px}",
    ".source-text h1{font-size:20px;margin:0 0 10px}.source-text h2{font-size:16px;border:0;padding:0;margin:20px 0 8px}.source-text h3{font-size:15px}",
    ".source-text{background:var(--bg);border:1px solid var(--line);border-radius:12px;padding:18px 22px;font-size:14.5px}.source-text>:first-child{margin-top:0}.source-text>:last-child{margin-bottom:0}",
    "pre{white-space:pre-wrap;overflow-wrap:anywhere;font:13px/1.65 ui-monospace,SFMono-Regular,Consolas,monospace;margin:0}",
    "blockquote{margin:8px 0;padding:4px 14px;border-left:3px solid var(--line);color:var(--muted)}",
    ".work{background:var(--card);border:1px solid var(--line);border-left:4px solid var(--accent);border-radius:12px;padding:14px 18px;margin-bottom:16px;box-shadow:var(--shadow);overflow-wrap:anywhere}",
    ".work.succeeded{border-left-color:var(--ok)}.work.failed{border-left-color:var(--bad)}",
    ".work header{display:flex;align-items:center;gap:10px;font-size:15px}.work header span:last-child{font-size:13px;color:var(--muted)}",
    ".work>p{margin:8px 0 0;font-size:14px}.work>blockquote{margin:10px 0 4px;color:var(--text);border-left-color:var(--accent)}.work .hint{margin:0}",
    ".work section h3{display:none}.work section h3+p{margin:8px 0 0;font-size:11.5px;color:var(--muted);font-family:ui-monospace,Consolas,monospace}",
    ".work section>p{font-size:14px}.work section summary{cursor:pointer;font-size:13px;color:var(--muted)}.work section button{margin-top:6px}",
    ".work article{border-top:1px solid var(--line);margin-top:10px;padding:10px 0 0;background:none;border-radius:0;box-shadow:none;border-left:0;border-right:0;border-bottom:0}.work article h4{margin:0 0 6px;font-size:14px}",
    "progress{width:100%;height:6px;accent-color:var(--accent)}",
    ".menu{list-style:none;margin:14px 0 0;padding:0}.menu a{display:block;padding:6px 10px;border-radius:8px;color:var(--text);text-decoration:none;font-size:14px}",
    ".menu a:hover{background:var(--hover)}.menu a[aria-current]{background:var(--accent-soft);color:var(--accent);font-weight:600}",
    ".add{background:var(--card);border:1px solid var(--line);border-radius:12px;box-shadow:var(--shadow)}",
    ".add>summary{list-style:none;cursor:pointer;padding:9px 12px;font-weight:600;font-size:14px;display:flex;align-items:center;gap:10px;color:var(--accent)}.add>summary::-webkit-details-marker{display:none}",
    ".add>summary::before{content:'+';flex:none;width:20px;height:20px;border-radius:50%;background:var(--accent);color:var(--accent-text);display:grid;place-items:center;font-size:15px;line-height:1}",
    ".add[open]>summary{border-bottom:1px solid var(--line)}.add form{padding:12px 12px 14px}.add .hint{margin:0}",
    ".add form details>summary{font-size:12px;color:var(--muted);cursor:pointer;margin-top:10px}.add button{width:100%;margin:12px 0 0}",
    "main>nav h2{display:flex;align-items:baseline;gap:8px;font-size:11px;font-weight:700;letter-spacing:.08em;color:var(--muted);margin:24px 8px 6px}",
    "main>nav h2 small,.archive small{font-weight:400;letter-spacing:0;opacity:.85}",
    ".archive{margin-top:24px}.archive>summary{cursor:pointer;font-size:11px;font-weight:700;letter-spacing:.08em;color:var(--muted);padding:0 8px 6px}.archive small{margin-left:8px}",
    ".hint{font-size:12.5px;line-height:1.6;color:var(--muted);margin:0 0 10px}.lead{font-size:17px;margin:0 0 6px}",
    ".ask{display:flex;gap:8px;margin-top:22px}.ask input{flex:1;padding:10px 14px;font-size:15px;border-radius:10px}.ask button{margin:0;padding:0 20px;border-radius:10px;font-size:14px}",
    ".tiles{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:12px;margin-top:16px}",
    ".tile{display:flex;flex-direction:column;background:var(--card);border:1px solid var(--line);border-radius:12px;padding:14px 16px;color:var(--text);text-decoration:none;box-shadow:var(--shadow)}a.tile:hover{border-color:var(--accent)}",
    ".tile b{font-size:26px;line-height:1.25}.tile span{font-size:13px;font-weight:600}.tile small{font-size:11.5px;line-height:1.5;color:var(--muted)}.tile.warn b{color:var(--warn)}",
    ".tile form{margin-top:auto}.tile button{margin:8px 0 0;padding:3px 10px;font-size:12px}",
    ".panel{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:18px 22px;margin-top:16px;box-shadow:var(--shadow)}.panel h2{font-size:15px;margin:0 0 8px}",
    ".panel label{display:block;font-size:12px;color:var(--muted);margin:10px 0 0}.panel label input,.panel label textarea{margin-top:3px}",
    ".panel details>summary{cursor:pointer;font-size:13px;color:var(--muted);margin-top:12px}.row button:first-of-type{margin-top:0}.row button{margin-top:0}",
    ".cards,.works{list-style:none;margin:0;padding:0}.cards li+li{border-top:1px solid var(--line)}",
    ".cards a{display:grid;grid-template-columns:minmax(0,1fr) auto;gap:2px 12px;padding:10px 12px;margin:0 -12px;border-radius:8px;text-decoration:none;color:var(--text)}.cards a:hover,.works a:hover{background:var(--hover)}",
    ".cards strong{font-size:15px}.cards time{grid-row:1;grid-column:2;font-size:12px;color:var(--muted)}.cards span{grid-column:1/-1;font-size:13px;color:var(--muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}",
    ".works a{display:flex;align-items:center;gap:10px;padding:7px 12px;margin:0 -12px;border-radius:8px;text-decoration:none;color:var(--text);font-size:14px}",
    ".works span{font-size:13px;color:var(--muted)}.works time{margin-left:auto;font-size:12px;color:var(--muted)}",
    ".dot{flex:none;width:8px;height:8px;border-radius:50%;background:var(--accent)}.dot.succeeded{background:var(--ok)}.dot.failed{background:var(--bad)}",
    ".more{margin:8px 0 0;font-size:13px}form.inline{display:inline}form.inline button{margin:0 0 0 8px;padding:1px 9px;font-size:12px;color:var(--muted)}",
    "@media(max-width:820px){.tiles{grid-template-columns:repeat(2,minmax(0,1fr))}}",
    ".pane>summary{display:none}",
    "@media(max-width:820px){main{display:block}main>nav{position:static;height:auto;border-right:0;border-bottom:1px solid var(--line)}.pane>summary{display:list-item;cursor:pointer;font-weight:600;margin-bottom:8px}.view{padding:16px}article{padding:22px 20px;border-radius:12px}article h1,article.top h1{font-size:23px}}",
);

const SELECTION_SCRIPT: &str = "document.addEventListener(\"pointerup\", () => { const selection = getSelection(); const section = document.getElementById(\"content\"); const text = selection && section.contains(selection.anchorNode) && section.contains(selection.focusNode) ? selection.toString().trim() : \"\"; if (text) { document.querySelectorAll(\"[data-selection]\").forEach((input) => { input.value = text.slice(0, 2000); }); document.querySelectorAll(\".selection-label\").forEach((label) => { label.hidden = false; }); } });";

fn stage_label(stage: &str) -> &str {
    WORK_STAGES
        .iter()
        .find(|(name, _)| *name == stage)
        .map_or(stage, |(_, label)| label)
}

/// `String(value)` of a JSON value, as the page template concatenated it.
fn js_text(value: Option<&ConfigValue>) -> String {
    match value {
        None => "undefined".into(),
        Some(ConfigValue::Null) => "null".into(),
        Some(ConfigValue::String(text)) => text.clone(),
        Some(ConfigValue::Number(number)) => js_number(*number),
        Some(ConfigValue::Bool(value)) => value.to_string(),
        Some(ConfigValue::NonFinite(name)) => name.clone(),
        Some(ConfigValue::Array(values)) => values
            .iter()
            .map(|value| match value {
                ConfigValue::Null => String::new(),
                other => js_text(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(ConfigValue::Object(_)) => "[object Object]".into(),
    }
}

/// `JSON.stringify(value, null, 2)`
fn pretty_json(value: &ConfigValue, indent: usize, out: &mut String) {
    let pad = |depth: usize| "  ".repeat(depth);
    match value {
        ConfigValue::Array(values) if !values.is_empty() => {
            out.push_str("[\n");
            for (index, item) in values.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                pretty_json(item, indent + 1, out);
                if index + 1 < values.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(indent));
            out.push(']');
        }
        ConfigValue::Object(entries) if !entries.is_empty() => {
            out.push_str("{\n");
            for (index, (name, item)) in entries.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                out.push_str(&text(name).to_js_json());
                out.push_str(": ");
                pretty_json(item, indent + 1, out);
                if index + 1 < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(indent));
            out.push('}');
        }
        other => out.push_str(&other.to_js_json()),
    }
}

/// `new URLSearchParams(pairs).toString()`
fn search_params(pairs: &[(&str, &str)]) -> String {
    let encode = |value: &str| {
        let mut out = String::new();
        for byte in value.bytes() {
            match byte {
                b' ' => out.push('+'),
                b'*' | b'-' | b'.' | b'_' => out.push(byte as char),
                _ if byte.is_ascii_alphanumeric() => out.push(byte as char),
                _ => out += &format!("%{byte:02X}"),
            }
        }
        out
    };
    pairs
        .iter()
        .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `workLink({ id, page })`
fn work_link(work: &ConfigValue) -> String {
    let id = js_text(work.get("id"));
    match work.get("page").filter(|page| truthy(Some(page))) {
        Some(page) => search_params(&[("work", &id), ("page", &js_text(Some(page)))]),
        None => search_params(&[("work", &id)]),
    }
}

/// `actionForm(token, page, action, value, fields, label)`
fn action_form(
    token: &str,
    page: Option<&str>,
    action: &str,
    value: &str,
    fields: &[(&str, &str)],
    label: &str,
) -> String {
    let mut hidden = vec![
        ("token", token),
        ("page", page.unwrap_or_default()),
        ("value", value),
    ];
    hidden.extend_from_slice(fields);
    format!(
        "<form method=\"post\" action=\"/action\">{}<button name=\"action\" value=\"{action}\">{label}</button></form>",
        hidden
            .iter()
            .map(|(name, input)| format!(
                "<input type=\"hidden\" name=\"{name}\" value=\"{}\">",
                escape_html(input)
            ))
            .collect::<String>()
    )
}

fn line_range(range: Option<&ConfigValue>) -> String {
    format!(
        "L{}–{}",
        js_text(range.and_then(|range| range.get("startLine"))),
        js_text(range.and_then(|range| range.get("endLine")))
    )
}

/// `renderWork(work, token, page)`
fn render_work(work: Option<&ConfigValue>, token: &str, page: Option<&str>) -> String {
    let Some(work) = work else {
        return String::new();
    };
    let status = js_text(work.get("status"));
    let stage = js_text(work.get("stage"));
    let mut out = format!(
        "<section aria-live=\"polite\" data-work-status=\"{status}\"><h3>{}</h3><p>{}</p>",
        escape_html(stage_label(&stage)),
        escape_html(&js_text(work.get("id")))
    );
    if status == "queued" || status == "running" {
        let progress = work.get("progress");
        let total = progress.and_then(|progress| progress.get("total"));
        if let Some(total) = total {
            let completed = js_text(progress.and_then(|progress| progress.get("completed")));
            let maximum = match total {
                ConfigValue::Number(total) => js_number(total.max(1.0)),
                other => js_text(Some(other)),
            };
            out += &format!(
                "<progress value=\"{completed}\" max=\"{maximum}\"></progress><p>{completed} / {}</p>",
                js_text(Some(total))
            );
        }
        return out + "</section>";
    }
    if let Some(error) = work.get("error").filter(|error| truthy(Some(error))) {
        return out
            + &format!(
                "<p>{}</p></section>",
                escape_html(&js_text(error.get("message")))
            );
    }
    let result = work
        .get("result")
        .filter(|result| !matches!(result, ConfigValue::Null))
        .cloned()
        .unwrap_or(ConfigValue::Object(Vec::new()));
    let mut json = String::new();
    pretty_json(&result, 0, &mut json);
    let details = format!(
        "<details><summary>実行結果</summary><pre>{}</pre></details>",
        escape_html(&json)
    );
    let changes: String = items(result.get("pages"))
        .iter()
        .filter(|page| page.get("current").is_some())
        .map(|page| {
            let text_of = |name: &str| {
                page.get(name)
                    .filter(|value| !matches!(value, ConfigValue::Null))
                    .map_or_else(String::new, |value| js_text(Some(value)))
            };
            format!(
                "<details><summary>{} の変更を見る</summary><h4>変更前</h4><pre>{}</pre><h4>変更後</h4><pre>{}</pre></details>",
                escape_html(&js_text(page.get("id"))),
                escape_html(&text_of("previous")),
                escape_html(&text_of("current"))
            )
        })
        .collect();
    let undo = if !items(result.get("previous_pages")).is_empty() || truthy(result.get("rules")) {
        action_form(
            token,
            page,
            "undo",
            &js_text(work.get("id")),
            &[],
            "取り消す",
        )
    } else {
        String::new()
    };
    let answer = if truthy(result.get("answer")) {
        format!(
            "<p>{}</p><p>引用元</p><ul>{}</ul><form method=\"post\" action=\"/action\"><input type=\"hidden\" name=\"token\" value=\"{token}\"><input type=\"hidden\" name=\"value\" value=\"{}\"><label>ページ ID<input name=\"second\" required></label><label>タイトル<input name=\"third\" required></label><button name=\"action\" value=\"save-answer\">ページとして保存</button></form>",
            escape_html(&js_text(result.get("answer"))),
            items(result.get("pages"))
                .iter()
                .map(|page| format!(
                    "<li><a href=\"/?page={}\">{}</a></li>",
                    encode_uri_component(&js_text(page.get("id"))),
                    escape_html(&js_text(page.get("title")))
                ))
                .collect::<String>(),
            escape_html(&js_text(work.get("id")))
        )
    } else {
        String::new()
    };
    let issues: String = items(result.get("issues"))
        .iter()
        .map(|issue| {
            let heading = issue
                .get("description")
                .filter(|value| !matches!(value, ConfigValue::Null))
                .or_else(|| issue.get("code"));
            let evidence = match issue.get("evidence") {
                Some(ConfigValue::Array(evidence)) => evidence
                    .iter()
                    .map(|item| {
                        format!(
                            "<blockquote>{}</blockquote><ul>{}</ul>",
                            escape_html(&js_text(item.get("quote"))),
                            items(item.get("citations"))
                                .iter()
                                .map(|citation| format!(
                                    "<li>{} {}</li>",
                                    escape_html(&js_text(
                                        citation
                                            .get("source")
                                            .and_then(|source| source.get("objectId"))
                                    )),
                                    line_range(citation.get("range"))
                                ))
                                .collect::<String>()
                        )
                    })
                    .collect::<String>(),
                _ => String::new(),
            };
            let id = js_text(issue.get("id"));
            let forms = if string_field(issue, "state") == Some("open") {
                action_form(
                    token,
                    page,
                    "issue",
                    &id,
                    &[("second", "dismissed")],
                    "却下",
                ) + &action_form(
                    token,
                    page,
                    "issue",
                    &id,
                    &[("second", "resolved")],
                    "対応済みにする",
                )
            } else {
                String::new()
            };
            format!(
                "<article><h4>{}</h4>{evidence}{forms}</article>",
                escape_html(&js_text(heading))
            )
        })
        .collect();
    out + &answer + &changes + &undo + &issues + &details + "</section>"
}

/// The folders and files of the left column, in the order they were added.
#[derive(Default)]
struct Tree {
    folders: Vec<(String, Tree)>,
    leaves: Vec<String>,
}

impl Tree {
    /// The folder at `parts`, made when it is missing.
    fn folder(&mut self, parts: &[String]) -> &mut Tree {
        let Some((first, rest)) = parts.split_first() else {
            return self;
        };
        let index = match self.folders.iter().position(|(name, _)| name == first) {
            Some(index) => index,
            None => {
                self.folders.push((first.clone(), Tree::default()));
                self.folders.len() - 1
            }
        };
        self.folders[index].1.folder(rest)
    }

    fn leaf(&mut self, parts: &[String], html: String) {
        self.folder(parts).leaves.push(html);
    }

    /// Nested lists; each folder opens and closes as a `details` element.
    fn html(&self) -> String {
        let mut out = String::from("<ul class=\"tree\">");
        for (name, folder) in &self.folders {
            out += &format!(
                "<li><details open><summary>{}</summary>{}</details></li>",
                escape_html(name),
                folder.html()
            );
        }
        for leaf in &self.leaves {
            out += &format!("<li>{leaf}</li>");
        }
        out + "</ul>"
    }
}

/// What a work did, in a line or two above its details: the passage a citation quotes, the pages
/// an instruction wrote, or how many findings a check has.
fn work_summary(work: &ConfigValue) -> String {
    let status = js_text(work.get("status"));
    let command = js_text(work.get("command"));
    let mut out = format!(
        "<header><span class=\"dot {}\"></span><strong>{}</strong><span>{}</span></header>",
        escape_html(&status),
        command_label(&command),
        escape_html(stage_label(&js_text(work.get("stage"))))
    );
    let Some(result) = work.get("result").filter(|_| status == "succeeded") else {
        return out;
    };
    if command == "cite" {
        let source = result
            .get("evidence")
            .and_then(|evidence| evidence.get("source"));
        out += &format!(
            "<blockquote>{}</blockquote><p class=\"hint\">{} {}</p>",
            escape_html(&js_text(result.get("quote"))),
            escape_html(&js_text(source.and_then(|source| source.get("objectId")))),
            line_range(
                result
                    .get("evidence")
                    .and_then(|evidence| evidence.get("range"))
            )
        );
    }
    if let Some(ConfigValue::Number(sources)) = result.get("sources") {
        out += &format!("<p>原本 {} 件を読みました。</p>", js_number(*sources));
    }
    let written = items(result.get("pages"));
    if command != "query" && !written.is_empty() {
        out += &format!(
            "<p>書いたページ：{}</p>",
            written
                .iter()
                .map(|page| {
                    let id = js_text(page.get("id"));
                    let title = match page.get("title") {
                        Some(ConfigValue::String(title)) => title.clone(),
                        _ => id.clone(),
                    };
                    format!(
                        "<a href=\"/?page={}\">{}</a>",
                        encode_uri_component(&id),
                        escape_html(&title)
                    )
                })
                .collect::<Vec<_>>()
                .join("、")
        );
    }
    if command == "verify" {
        let issues = items(result.get("issues")).len();
        out += &if issues == 0 {
            "<p>指摘はありませんでした。</p>".to_owned()
        } else {
            format!("<p>指摘が {issues} 件あります。</p>")
        };
    }
    let skipped = items(result.get("skipped")).len();
    if skipped > 0 {
        out += &format!("<p>読めない形式の資料 {skipped} 件を飛ばしました。</p>");
    }
    out
}

/// The name of a recorded command on the screen.
fn command_label(command: &str) -> &'static str {
    match command {
        "ingress" => "取り込み",
        "query" => "質問",
        "instruct" => "指示",
        "verify" => "検査",
        "archive" => "アーカイブ",
        "undo" => "取り消し",
        "cite" => "引用の確認",
        "save-answer" => "回答の保存",
        "issue" => "指摘の更新",
        _ => "作業",
    }
}

/// Seconds since 1970 of a time written as `2026-01-02T03:04:05.678Z`.
fn epoch_seconds(iso: &str) -> Option<i64> {
    let number = |range: std::ops::Range<usize>| iso.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some((era * 146_097 + day_of_era - 719_468) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// How long ago a time was, in words. It does not depend on the time zone of the reader.
fn ago(iso: &str, now: i64) -> String {
    let Some(then) = epoch_seconds(iso) else {
        return String::new();
    };
    match (now - then).max(0) {
        seconds if seconds < 60 => "たった今".into(),
        seconds if seconds < 3600 => format!("{} 分前", seconds / 60),
        seconds if seconds < 86_400 => format!("{} 時間前", seconds / 3600),
        seconds => format!("{} 日前", seconds / 86_400),
    }
}

/// The start of a page as one line of plain text, without citation numbers and Markdown marks.
fn snippet(markdown: &str) -> String {
    let mut plain = String::new();
    let mut rest = markdown;
    while let Some(open) = rest.find('[') {
        plain.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find(']') {
            Some(close) if close > 0 && after[..close].bytes().all(|b| b.is_ascii_digit()) => {
                rest = &after[close + 1..];
            }
            _ => {
                plain.push('[');
                rest = after;
            }
        }
    }
    plain.push_str(rest);
    let words: Vec<&str> = plain
        .split_whitespace()
        .map(|word| word.trim_matches(['#', '*', '`', '|', '>', '-']))
        .filter(|word| !word.is_empty())
        .collect();
    let line = words.join(" ");
    if line.chars().count() > 90 {
        format!("{}…", line.chars().take(90).collect::<String>())
    } else {
        line
    }
}

/// A link of the tree to `/?{query}`, marked when it is the one shown. `kind` picks its icon:
/// `page`, `file` or `web`.
fn leaf_link(
    kind: &str,
    query: &str,
    label: &str,
    title: &str,
    current: bool,
    badge: Option<&str>,
) -> String {
    format!(
        "<a class=\"{kind}\" href=\"/?{query}\" title=\"{}\"{}><span class=\"name\">{}</span>{}</a>",
        escape_html(title),
        if current {
            " aria-current=\"page\""
        } else {
            ""
        },
        escape_html(label),
        badge.map_or_else(String::new, |badge| format!(
            "<span class=\"badge\">{}</span>",
            escape_html(badge)
        ))
    )
}

/// Where a source sits in the tree: the folders of its path and its name. Web pages are grouped
/// under `Web` and their host, and named by the rest of their address.
fn source_path(object_id: &str) -> (Vec<String>, String) {
    if crate::wiki_fetch::is_web_address(object_id)
        && let Ok(url) = url::Url::parse(object_id)
    {
        let mut rest = url.path().to_owned();
        if let Some(query) = url.query() {
            rest = format!("{rest}?{query}");
        }
        let rest = percent_encoding::percent_decode_str(&rest)
            .decode_utf8_lossy()
            .into_owned();
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        return (vec!["Web".into(), host], rest);
    }
    let mut parts: Vec<String> = object_id
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    let name = parts.pop().unwrap_or_else(|| object_id.to_owned());
    (parts, name)
}

/// Whether the loader wrote a source's text as Markdown: Markdown, HTML and Office files, and web
/// pages other than plain text, PDF and JSON.
fn markdown_source(object_id: &str) -> bool {
    let web = crate::wiki_fetch::is_web_address(object_id);
    let path = match url::Url::parse(object_id) {
        Ok(url) if web => url.path().to_ascii_lowercase(),
        _ => object_id.to_ascii_lowercase(),
    };
    let name = path.rsplit(['/', '\\']).next().unwrap_or_default();
    let extension = name.rfind('.').map_or("", |at| &name[at..]);
    [
        ".md",
        ".markdown",
        ".html",
        ".htm",
        ".docx",
        ".xlsx",
        ".pptx",
    ]
    .contains(&extension)
        || (web && ![".txt", ".pdf", ".json"].contains(&extension))
}

/// `displayPage(page, known)`: the page as HTML, with links to known pages kept on the screen.
fn display_page(page: &ConfigValue, known: &[String]) -> String {
    let markdown = str_of(page, "markdown");
    let id = str_of(page, "id");
    let mut content = String::with_capacity(markdown.len());
    let mut last = 0;
    for (start, end) in link_targets(markdown) {
        let target = linked_page(id, &markdown[start..end]);
        if let Some(target) = target.filter(|target| known.contains(target)) {
            content += &markdown[last..start];
            content += "?page=";
            content += &encode_uri_component(&target);
            last = end;
        }
    }
    content += &markdown[last..];
    render(&content)
}

/// One form field: text, or an uploaded file with its name and bytes.
enum Field {
    Text(String),
    File { name: String, bytes: Vec<u8> },
}

/// `new URLSearchParams(text)`
fn url_encoded(body: &[u8]) -> Vec<(String, Field)> {
    let text = String::from_utf8_lossy(body);
    let decode = |part: &str| {
        let plus = part.replace('+', " ");
        let bytes = plus.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit()
            {
                out.push(u8::from_str_radix(&plus[i + 1..i + 3], 16).unwrap_or(0));
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    text.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(name), Field::Text(decode(value)))
        })
        .collect()
}

/// A parameter of a `Content-Disposition` header, as browsers write it.
fn disposition_parameter(header: &str, name: &str) -> Option<String> {
    let mut rest = header;
    while let Some(index) = rest.find(';') {
        rest = rest[index + 1..].trim_start();
        let Some((key, value)) = rest.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case(name) {
            continue;
        }
        let value = value.trim_start();
        if let Some(quoted) = value.strip_prefix('"') {
            let end = quoted.find('"').unwrap_or(quoted.len());
            return Some(
                quoted[..end]
                    .replace("%0A", "\n")
                    .replace("%0a", "\n")
                    .replace("%0D", "\r")
                    .replace("%0d", "\r")
                    .replace("%22", "\""),
            );
        }
        let end = value.find(';').unwrap_or(value.len());
        return Some(value[..end].trim().to_owned());
    }
    None
}

/// `new Response(body, { headers }).formData()` for `multipart/form-data`.
fn multipart(content_type: &str, body: &[u8]) -> Result<Vec<(String, Field)>, String> {
    let failure = || "Failed to parse body as FormData.".to_owned();
    let boundary = disposition_parameter(
        &format!(
            ";{}",
            content_type.split_once(';').map_or("", |(_, rest)| rest)
        ),
        "boundary",
    )
    .filter(|boundary| !boundary.is_empty())
    .ok_or_else(failure)?;
    let delimiter = format!("--{boundary}").into_bytes();
    let find = |haystack: &[u8], needle: &[u8], from: usize| {
        haystack
            .get(from..)?
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|index| index + from)
    };
    let mut fields = Vec::new();
    let mut at = find(body, &delimiter, 0).ok_or_else(failure)? + delimiter.len();
    loop {
        if body.get(at..at + 2) == Some(b"--") {
            return Ok(fields);
        }
        if body.get(at..at + 2) != Some(b"\r\n") {
            return Err(failure());
        }
        at += 2;
        let headers_end = find(body, b"\r\n\r\n", at).ok_or_else(failure)?;
        let headers = String::from_utf8_lossy(&body[at..headers_end]).into_owned();
        let content_start = headers_end + 4;
        let mut closing = b"\r\n".to_vec();
        closing.extend_from_slice(&delimiter);
        let content_end = find(body, &closing, content_start).ok_or_else(failure)?;
        let disposition = headers
            .split("\r\n")
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-disposition")
                    .then(|| value.trim().to_owned())
            })
            .ok_or_else(failure)?;
        let name = disposition_parameter(&disposition, "name").ok_or_else(failure)?;
        let content = body[content_start..content_end].to_vec();
        let field = match disposition_parameter(&disposition, "filename") {
            Some(filename) => Field::File {
                name: filename,
                bytes: content,
            },
            None => Field::Text(String::from_utf8_lossy(&content).into_owned()),
        };
        fields.push((name, field));
        at = content_end + closing.len();
    }
}

/// A queued instruction.
struct Job {
    command: String,
    args: Vec<Option<String>>,
    values: ConfigValue,
    run_id: String,
}

struct WikiServer {
    request: WikiServeRequest,
    hosts: Vec<Option<String>>,
    digest: Option<[u8; 32]>,
    port: u16,
    running: AtomicUsize,
    form_key: String,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
}

/// `authorized(value, digest, true)`: a bearer token, or Basic credentials for the user `knowledge-nest`.
fn authorized(value: Option<&str>, digest: Option<&[u8; 32]>) -> bool {
    let Some(digest) = digest else { return true };
    let Some(value) = value else { return false };
    let bearer = value
        .get(..7)
        .filter(|prefix| prefix.eq_ignore_ascii_case("bearer "))
        .map(|_| value[7..].to_owned())
        .filter(|token| !token.is_empty() && !token.chars().any(char::is_whitespace));
    let password = bearer.or_else(|| {
        let encoded = value
            .get(..6)
            .filter(|prefix| prefix.eq_ignore_ascii_case("basic "))
            .map(|_| &value[6..])
            .filter(|encoded| {
                !encoded.is_empty()
                    && encoded
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"+/=".contains(&byte))
            })?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .or_else(|_| {
                base64::engine::general_purpose::STANDARD_NO_PAD
                    .decode(encoded.trim_end_matches('='))
            })
            .ok()?;
        String::from_utf8_lossy(&decoded)
            .strip_prefix("knowledge-nest:")
            .map(str::to_owned)
    });
    let Some(password) = password else {
        return false;
    };
    let actual: [u8; 32] = Sha256::digest(password.as_bytes()).into();
    actual
        .iter()
        .zip(digest)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn html(status: u16, body: impl Into<String>) -> Reply {
    Reply {
        status,
        headers: Vec::new(),
        body: body.into(),
    }
}

/// An error that leaves the page: the reply of the request checks.
struct HttpFailure(Reply);

impl WikiServer {
    async fn handle(
        self: Arc<Self>,
        request: Request<Incoming>,
        peer: SocketAddr,
    ) -> Response<Body> {
        let mut headers: Vec<(&'static str, String)> = vec![
            ("cache-control", "no-store".into()),
            ("x-content-type-options", "nosniff".into()),
        ];
        let reply = self.route(request, peer, &mut headers).await;
        for (name, value) in reply.headers {
            headers.retain(|(existing, _)| *existing != name);
            headers.push((name, value));
        }
        let mut response = Response::builder().status(
            StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        );
        for (name, value) in headers {
            response = response.header(name, value);
        }
        response
            .body(Full::new(Bytes::from(reply.body)))
            .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
    }

    async fn route(
        self: &Arc<Self>,
        request: Request<Incoming>,
        peer: SocketAddr,
        headers: &mut Vec<(&'static str, String)>,
    ) -> Reply {
        let settings = &self.request.settings;
        let target = header(&request, "host").and_then(|host| request_target(host, self.port));
        let Some((name, authority)) =
            target.filter(|(name, _)| self.hosts.iter().any(|host| host.as_deref() == Some(name)))
        else {
            return Reply::error(403, "HOST_FORBIDDEN", "This Host is not allowed.");
        };
        let own_origin = url::Url::parse(&format!("http://{authority}"))
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_default();
        let source = header(&request, "origin").map(str::to_owned);
        if let Some(source) = &source
            && *source != own_origin
            && !settings.access.allowed_origins.contains(source)
        {
            return Reply::error(403, "ORIGIN_FORBIDDEN", "This Origin is not allowed.");
        }
        if let Some(source) = source.as_ref().filter(|source| !source.is_empty()) {
            headers.push(("access-control-allow-origin", source.clone()));
            headers.push((
                "access-control-expose-headers",
                "Mcp-Session-Id, MCP-Protocol-Version".into(),
            ));
            headers.push(("vary", "Origin".into()));
            if self.digest.is_some() {
                headers.push(("access-control-allow-credentials", "true".into()));
            }
        }
        if request.method() == hyper::Method::OPTIONS
            && source.as_ref().is_some_and(|source| !source.is_empty())
            && header(&request, "access-control-request-method").is_some()
        {
            return Reply {
                status: 204,
                headers: vec![
                    ("access-control-allow-methods", "GET, POST, DELETE, OPTIONS".into()),
                    (
                        "access-control-allow-headers",
                        "Authorization, Content-Type, MCP-Protocol-Version, Mcp-Method, Mcp-Name, Mcp-Session-Id, Last-Event-ID".into(),
                    ),
                ],
                body: String::new(),
            };
        }
        if !authorized(header(&request, "authorization"), self.digest.as_ref()) {
            let mut reply = Reply::error(401, "AUTH_REQUIRED", "Valid authentication is required.");
            reply.headers.push((
                "www-authenticate",
                r#"Basic realm="Knowledge-Nest", charset="UTF-8""#.into(),
            ));
            return reply;
        }
        let limits = &settings.limits;
        if self.running.load(Ordering::SeqCst) >= limits.max_concurrent {
            return Reply::error(429, "REQUEST_LIMIT", "Too many concurrent requests.");
        }
        let path = request
            .uri()
            .path_and_query()
            .map_or("/", |path| path.as_str())
            .to_owned();
        if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
            return Reply::error(400, "INVALID_REQUEST", "Expected a local request path.");
        }
        let length: u64 = header(&request, "content-length")
            .and_then(|length| length.trim().parse().ok())
            .unwrap_or(0);
        if length > limits.max_request_bytes {
            return Reply::error(413, "BODY_LIMIT", "The request body is too large.");
        }
        let Ok(url) =
            url::Url::parse(&format!("http://{authority}")).and_then(|base| base.join(&path))
        else {
            return Reply::error(400, "INVALID_REQUEST", "Expected a local request path.");
        };
        self.running.fetch_add(1, Ordering::SeqCst);
        let _running = Running(&self.running);
        let cancel = Cancellation::default();
        let mut guard = AbortOnDrop(Some(cancel.clone()));
        let client = object(vec![
            ("address", text(&peer.ip().to_string())),
            ("host", text(&name)),
            ("origin", source.as_deref().map_or(ConfigValue::Null, text)),
        ]);
        let timeout = Duration::from_secs(limits.request_timeout_seconds);
        let reply = tokio::time::timeout(
            timeout,
            self.clone().respond(request, url, source, client, length),
        )
        .await;
        match reply {
            Ok(Ok(reply)) => {
                guard.0 = None;
                reply
            }
            Ok(Err(HttpFailure(reply))) => {
                guard.0 = None;
                reply
            }
            Err(_) => Reply::error(
                408,
                "REQUEST_TIMEOUT",
                "The request exceeded its time limit.",
            ),
        }
    }

    async fn respond(
        self: Arc<Self>,
        request: Request<Incoming>,
        url: url::Url,
        source: Option<String>,
        client: ConfigValue,
        length: u64,
    ) -> Result<Reply, HttpFailure> {
        let limit = self.request.settings.limits.max_request_bytes;
        let method = request.method().clone();
        let content_type = header(&request, "content-type")
            .unwrap_or_default()
            .to_owned();
        let chunked = header(&request, "transfer-encoding").is_some();
        let mut bytes = Vec::new();
        if length > 0 || chunked {
            let mut body = request.into_body();
            loop {
                match body.frame().await {
                    None => break,
                    Some(Ok(frame)) => {
                        if let Ok(data) = frame.into_data() {
                            if (bytes.len() + data.len()) as u64 > limit {
                                return Err(HttpFailure(Reply::error(
                                    413,
                                    "BODY_LIMIT",
                                    "The request body is too large.",
                                )));
                            }
                            bytes.extend_from_slice(&data);
                        }
                    }
                    Some(Err(_)) => {
                        return Err(HttpFailure(Reply::error(
                            408,
                            "REQUEST_CLOSED",
                            "The request ended.",
                        )));
                    }
                }
            }
        }
        if !self
            .request
            .settings
            .endpoints
            .iter()
            .any(|endpoint| endpoint == "web")
        {
            return Err(HttpFailure(Reply::error(
                404,
                "NOT_FOUND",
                "The endpoint is unavailable.",
            )));
        }
        let policy = CONTENT_SECURITY_POLICY.replace("form-action 'none'", "form-action 'self'");
        let mut page_headers = vec![
            (
                "content-security-policy",
                format!("{policy}; frame-ancestors 'none'"),
            ),
            ("x-content-type-options", "nosniff".into()),
            ("cache-control", "no-store".into()),
            ("content-type", "text/html; charset=utf-8".into()),
        ];
        let server = self.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            if method == hyper::Method::POST {
                server.post(&url, source.as_deref(), &content_type, &bytes, client)
            } else if method != hyper::Method::GET {
                Ok(html(405, ""))
            } else {
                server.page(&url)
            }
        })
        .await
        .unwrap_or_else(|_| Err(StageError::plain("The request could not be completed.")));
        let mut reply = outcome.unwrap_or_else(|error| html(400, escape_html(&error.message)));
        // The page sets its own policy with the nonce of its script.
        for (name, value) in std::mem::take(&mut reply.headers) {
            page_headers.retain(|(existing, _)| *existing != name);
            page_headers.push((name, value));
        }
        reply.headers = page_headers;
        Ok(reply)
    }

    fn base(&self) -> &WikiRequest {
        &self.request.base
    }

    /// A source in the right column: where it came from, the pages that cite it and its text as
    /// the loader read it.
    fn source_view(
        &self,
        id: &str,
        source: &ConfigValue,
        reflected: bool,
        content: &[ConfigValue],
    ) -> Result<String, StageError> {
        let cancel = Cancellation::default();
        let project = crate::wiki_project::Project {
            request: self.base(),
            cancel: &cancel,
        };
        let database = project.database()?;
        let document = crate::wiki_project::current_document(&database, source, &cancel)?;
        drop(database);
        let object_id = js_text(source.get("objectId"));
        // A web page is named by its whole address; a file by its name.
        let name = if crate::wiki_fetch::is_web_address(&object_id) {
            object_id.clone()
        } else {
            source_path(&object_id).1
        };
        let address = document
            .as_ref()
            .and_then(|document| document.get("evidence"))
            .and_then(|evidence| evidence.get("source"))
            .and_then(|source| source.get("url"))
            .or_else(|| source.get("url"))
            .and_then(ConfigValue::as_str)
            .filter(|url| crate::wiki_fetch::is_web_address(url))
            .map(|url| {
                format!(
                    "<p><a href=\"{}\" rel=\"noreferrer noopener\" target=\"_blank\">元のページを開く ↗</a></p>",
                    escape_html(url)
                )
            })
            .unwrap_or_default();
        let citing: String = content
            .iter()
            .filter(|page| {
                items(page.get("citations")).iter().any(|citation| {
                    citation
                        .get("source")
                        .and_then(|source| source.get("sourceId"))
                        .and_then(ConfigValue::as_str)
                        == Some(id)
                })
            })
            .map(|page| {
                let target = page
                    .get("archive_id")
                    .filter(|value| !matches!(value, ConfigValue::Null))
                    .or_else(|| page.get("id"));
                format!(
                    "<li><a href=\"/?page={}\">{}</a></li>",
                    encode_uri_component(&js_text(target)),
                    escape_html(&js_text(page.get("title")))
                )
            })
            .collect();
        let text = match &document {
            Some(document) => {
                let body = str_of(document, "content");
                if markdown_source(&object_id) {
                    render(body)
                } else {
                    format!("<pre>{}</pre>", escape_html(body))
                }
            }
            None => "<p>本文を読み出せませんでした。</p>".to_owned(),
        };
        Ok(format!(
            "<h1>{}</h1><p class=\"meta\">原本{}{}</p>{address}<h2>引用しているページ</h2>{}<h2>本文</h2><div class=\"source-text\">{text}</div>",
            escape_html(&name),
            // A web page's heading is already its address.
            if name == object_id {
                String::new()
            } else {
                format!(" · {}", escape_html(&object_id))
            },
            if reflected {
                ""
            } else {
                " · 未反映（ページにはまだ書かれていません）"
            },
            if citing.is_empty() {
                "<p>まだありません。</p>".to_owned()
            } else {
                format!("<ul>{citing}</ul>")
            },
        ))
    }

    /// A form instruction: checks it, queues it as work and redirects to its progress.
    fn post(
        &self,
        url: &url::Url,
        source: Option<&str>,
        content_type: &str,
        body: &[u8],
        client: ConfigValue,
    ) -> Result<Reply, StageError> {
        if source.is_none_or(str::is_empty) || url.path() != "/action" {
            return Ok(html(403, ""));
        }
        let form = if content_type.starts_with("multipart/form-data") {
            multipart(content_type, body).map_err(StageError::plain)?
        } else {
            url_encoded(body)
        };
        let get = |key: &str| {
            form.iter()
                .find(|(name, _)| name == key)
                .map(|(_, field)| field)
        };
        let text_of = |key: &str| match get(key) {
            Some(Field::Text(value)) => crate::eval_data::js_trim(value).to_owned(),
            _ => String::new(),
        };
        if !matches!(get("token"), Some(Field::Text(token)) if *token == self.form_key) {
            return Ok(html(403, ""));
        }
        let action = match get("action") {
            Some(Field::Text(action)) => action.clone(),
            _ => String::new(),
        };
        let mut value = text_of("value");
        let page = text_of("page");
        let second = text_of("second");
        let third = text_of("third");
        let mut options: Vec<(&str, ConfigValue)> = Vec::new();
        let supported = [
            "ingress",
            "pending",
            "query",
            "verify",
            "content",
            "archive",
            "restore",
            "undo",
            "cite",
            "instruct",
            "rule",
            "save-answer",
            "issue",
        ];
        let upload = match get("upload") {
            Some(Field::File { name, bytes }) if action == "ingress" && !bytes.is_empty() => {
                Some((name.clone(), bytes.clone()))
            }
            _ => None,
        };
        // A web address to fetch, from its own field or written where a path would go.
        let address = match text_of("url") {
            url if !url.is_empty() && action == "ingress" && upload.is_none() => Some(url),
            _ if action == "ingress"
                && upload.is_none()
                && crate::wiki_fetch::is_web_address(&value) =>
            {
                Some(value.clone())
            }
            _ => None,
        };
        if let Some(address) = &address {
            if crate::wiki_fetch::normalize(address).is_err() {
                return Ok(html(400, "http または https の URL を指定してください"));
            }
            value = address.clone();
        }
        if !supported.contains(&action.as_str())
            || (!["verify", "content", "archive", "restore", "pending"].contains(&action.as_str())
                && value.is_empty()
                && upload.is_none()
                && address.is_none())
        {
            return Ok(html(400, "無効な操作"));
        }
        let mut command = action.clone();
        let mut input: Vec<Option<String>> = vec![Some(value.clone())];
        let some = |text: &str| Some(text.to_owned());
        if action == "pending" {
            command = "ingress".into();
            options.push(("pending", ConfigValue::Bool(true)));
            input = Vec::new();
        }
        if ["query", "verify", "content"].contains(&action.as_str()) && !page.is_empty() {
            options.push(("page", text(&page)));
        }
        if action == "query" && !page.is_empty() && !second.is_empty() {
            options.push(("target", text(&second)));
        }
        if ["verify", "content"].contains(&action.as_str()) {
            command = "verify".into();
            input = Vec::new();
            options.push(("content", ConfigValue::Bool(action == "content")));
        }
        if ["archive", "restore"].contains(&action.as_str()) {
            command = "archive".into();
            input = vec![some(if page.is_empty() { &value } else { &page })];
            options.push(("restore", ConfigValue::Bool(action == "restore")));
        }
        match action.as_str() {
            "cite" => input = vec![some(&page), some(&value)],
            "issue" => input = vec![some(&value), some(&second)],
            "save-answer" => input = vec![some(&value), some(&second), some(&third)],
            "instruct" => {
                input = vec![some(&page), some(&value)];
                if !second.is_empty() {
                    options.push(("target", text(&second)));
                }
                let supplement = text_of("supplement");
                if !supplement.is_empty() {
                    options.push(("supplement", text(&supplement)));
                    options.push(("by", text(&third)));
                }
            }
            "rule" => {
                command = "instruct".into();
                options.push(("persistent", ConfigValue::Bool(true)));
                input = vec![None, some(&value)];
            }
            _ => {}
        }
        if let Some((name, bytes)) = &upload {
            let name = basename(&name.replace('\\', "/"));
            let lower = name.to_ascii_lowercase();
            let stem = lower.split('.').next().unwrap_or_default();
            let reserved = (stem.len() == 3 && ["con", "prn", "aux", "nul"].contains(&stem))
                || (stem.len() == 4
                    && (stem.starts_with("com") || stem.starts_with("lpt"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'));
            if name.starts_with('.')
                || name.encode_utf16().count() > 128
                || name
                    .chars()
                    .any(|c| "<>:\"|?*".contains(c) || is_control(c))
                || name.ends_with(['.', ' '])
                || reserved
            {
                return Ok(html(400, "資料ファイルの名前が無効です"));
            }
            let folder = node_path::join(
                &self.base().artifacts,
                &["uploads", &crate::source_load::sha256_hex(bytes)],
            );
            std::fs::create_dir_all(&folder)
                .map_err(|error| crate::wiki_project::fs_error("mkdir", &folder, &error))?;
            value = node_path::join(&folder, &[&name]);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&value)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(bytes)
                        .map_err(|error| crate::wiki_project::fs_error("write", &value, &error))?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(crate::wiki_project::fs_error("open", &value, &error)),
            }
            options.push(("uploadName", text(&name)));
            input = vec![Some(value.clone())];
        }
        let persistent = options.iter().any(|(name, _)| *name == "persistent");
        if input
            .iter()
            .any(|item| item.as_deref().is_none_or(str::is_empty))
            && !persistent
        {
            return Ok(html(400, "操作に必要な入力が不足しています"));
        }
        if action == "ingress" && upload.is_none() && address.is_none() {
            let base = self.base();
            let file = realpath(&node_path::resolve(&base.root, &value))?;
            let root = realpath(&base.root)?;
            let within = node_path::relative(&root, &file);
            let separator = if cfg!(windows) { '\\' } else { '/' };
            let regular = std::fs::metadata(&file).is_ok_and(|entry| entry.is_file());
            if within.is_empty()
                || node_path::is_absolute(&within)
                || within == ".."
                || within.starts_with(&format!("..{separator}"))
                || within.split(separator).any(|part| part.starts_with('.'))
                || !regular
            {
                return Ok(html(400, "プロジェクト内の資料ファイルを指定してください"));
            }
            input = vec![Some(file)];
        }
        let run_id = format!("run-{}", uuid::Uuid::new_v4());
        let connection = open_state(&self.base().state, false)?;
        crate::wiki_work::queue(
            &connection,
            &run_id,
            &command,
            Some(page.as_str()),
            Some(client),
        )?;
        drop(connection);
        if let Ok(jobs) = self.jobs.lock()
            && let Some(sender) = jobs.as_ref()
        {
            let _ = sender.send(Job {
                command,
                args: input,
                values: ConfigValue::Object(
                    options
                        .into_iter()
                        .map(|(name, value)| (name.to_owned(), value))
                        .collect(),
                ),
                run_id: run_id.clone(),
            });
        }
        let location = if page.is_empty() {
            search_params(&[("work", &run_id)])
        } else {
            search_params(&[("work", &run_id), ("page", &page)])
        };
        Ok(Reply {
            status: 303,
            headers: vec![("location", format!("/?{location}"))],
            body: String::new(),
        })
    }

    /// The screen: the hierarchy, the current page or the top, and the instructions and work.
    fn page(&self, url: &url::Url) -> Result<Reply, StageError> {
        let base = self.base();
        let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let param = |name: &str| {
            query
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        let connection = open_state(&base.state, false)?;
        let work = match param("work").filter(|id| !id.is_empty()) {
            Some(id) => crate::wiki_work::status(&connection, id)?,
            None => None,
        };
        let working = work
            .as_ref()
            .and_then(|work| string_field(work, "status"))
            .is_some_and(|status| status == "queued" || status == "running");
        let content = pages(&connection, true)?;
        let requested = param("page");
        let current = requested.and_then(|requested| {
            content.iter().find(|page| {
                str_of(page, "id") == requested
                    || string_field(page, "archive_id") == Some(requested)
            })
        });
        let about = &base.about;
        let active: Vec<&ConfigValue> = content
            .iter()
            .filter(|page| !truthy(page.get("archived")))
            .collect();
        let current_page = current.map(|page| js_text(page.get("id")));
        // Pages as a tree of folders: the parts of their IDs (the PARA categories come first).
        let mut page_tree = Tree::default();
        if base.kind == "para" {
            for category in CATEGORIES {
                page_tree.folder(&[category.to_string()]);
            }
        }
        for page in &active {
            let id = js_text(page.get("id"));
            let mut parts: Vec<String> = id.split('/').map(str::to_owned).collect();
            parts.pop();
            page_tree.leaf(
                &parts,
                leaf_link(
                    "page",
                    &format!("page={}", encode_uri_component(&id)),
                    &js_text(page.get("title")),
                    &id,
                    current_page.as_deref() == Some(id.as_str()),
                    None,
                ),
            );
        }
        let mut archive_tree = Tree::default();
        for page in content.iter().filter(|page| truthy(page.get("archived"))) {
            let target = js_text(
                page.get("archive_id")
                    .filter(|value| !matches!(value, ConfigValue::Null))
                    .or_else(|| page.get("id")),
            );
            archive_tree.leaf(
                &[],
                leaf_link(
                    "page archived",
                    &format!("page={}", encode_uri_component(&target)),
                    &js_text(page.get("title")),
                    &target,
                    current_page.as_deref() == Some(js_text(page.get("id")).as_str()),
                    None,
                ),
            );
        }
        let count: i64 = connection
            .query_row(
                "SELECT count(*) AS n FROM wiki_sources WHERE reflected = 0",
                [],
                |row| row.get(0),
            )
            .map_err(sql)?;
        let history = crate::wiki_work::latest(&connection)?;
        let inspection = history.iter().find(|item| {
            string_field(item, "command") == Some("verify")
                && !truthy(item.get("page"))
                && string_field(item, "status") == Some("succeeded")
        });
        let open_issues = match inspection
            .and_then(|item| item.get("result"))
            .and_then(|result| result.get("issues"))
        {
            Some(ConfigValue::Array(issues)) => Some(
                issues
                    .iter()
                    .filter(|issue| {
                        let state: Option<String> = connection
                            .query_row(
                                "SELECT state FROM wiki_issues WHERE id = ?1",
                                [js_text(issue.get("id"))],
                                |row| row.get(0),
                            )
                            .ok();
                        state.as_deref().unwrap_or("open") == "open"
                    })
                    .count(),
            ),
            _ => None,
        };
        let mut statement = connection
            .prepare("SELECT id, data, reflected FROM wiki_sources ORDER BY id")
            .map_err(sql)?;
        let sources: Vec<(String, ConfigValue, bool)> = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                ))
            })
            .map_err(sql)?
            .map(|row| {
                row.map_err(sql)
                    .and_then(|(id, data, reflected)| Ok((id, parse(&data)?, reflected)))
            })
            .collect::<Result<_, _>>()?;
        drop(statement);
        let requested_source = param("source").filter(|_| current.is_none());
        let current_source = requested_source
            .and_then(|requested| sources.iter().find(|(id, _, _)| id == requested));
        // Sources as a tree: the folders of their paths, and web pages by their host.
        let mut source_tree = Tree::default();
        for (id, source, reflected) in &sources {
            let object_id = js_text(source.get("objectId"));
            let (parts, label) = source_path(&object_id);
            source_tree.leaf(
                &parts,
                leaf_link(
                    if crate::wiki_fetch::is_web_address(&object_id) {
                        "web"
                    } else {
                        "file"
                    },
                    &format!("source={}", encode_uri_component(id)),
                    &label,
                    &object_id,
                    requested_source == Some(id.as_str()),
                    (!reflected).then_some("未反映"),
                ),
            );
        }
        let mut recent = active.clone();
        recent.sort_by(|a, b| {
            crate::wiki_inspect::locale_compare(
                &js_text(b.get("changed")),
                &js_text(a.get("changed")),
            )
        });
        let rules = if param("rules").is_some() {
            let file = node_path::resolve(
                &base.config_root,
                str_of(
                    &items(base.plan.get("ingress"))
                        .get(2)
                        .cloned()
                        .unwrap_or_default(),
                    "rules",
                ),
            );
            Some(crate::wiki_project::read_file(&file)?)
        } else {
            None
        };
        // What the right column shows. Each view has its content and the actions that belong to it.
        enum Shown<'a> {
            Page(&'a ConfigValue),
            Source(&'a (String, ConfigValue, bool)),
            Rules(&'a str),
            History,
            Top,
        }
        let shown = match (current, current_source, &rules) {
            (Some(page), _, _) => Shown::Page(page),
            (None, Some(source), _) => Shown::Source(source),
            (None, None, Some(rules)) => Shown::Rules(rules),
            (None, None, None) if param("history").is_some() => Shown::History,
            (None, None, None) => Shown::Top,
        };
        let selection_key = uuid::Uuid::new_v4().to_string();
        let policy = CONTENT_SECURITY_POLICY.replace("form-action 'none'", "form-action 'self'");
        let form_key = &self.form_key;
        let known: Vec<String> = content
            .iter()
            .map(|page| str_of(page, "id").to_owned())
            .collect();
        let current_id = current.map(|page| js_text(page.get("id")));
        let action = current_id.as_deref().map_or_else(String::new, |id| {
            format!("?page={}", encode_uri_component(id))
        });
        // The start of a form that queues an instruction for the shown page, if any.
        let form = |class: &str| {
            format!(
                "<form class=\"{class}\" method=\"post\" action=\"/action{action}\"><input type=\"hidden\" name=\"token\" value=\"{form_key}\"><input type=\"hidden\" name=\"page\" value=\"{}\">",
                escape_html(current_id.as_deref().unwrap_or_default())
            )
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        let work_list = |works: &[ConfigValue]| {
            if works.is_empty() {
                return "<p class=\"empty\">まだありません</p>".to_owned();
            }
            format!(
                "<ul class=\"works\">{}</ul>",
                works
                    .iter()
                    .map(|item| format!(
                        "<li><a href=\"/?{}\"><span class=\"dot {}\"></span><strong>{}</strong><span>{}</span><time>{}</time></a></li>",
                        escape_html(&work_link(item)),
                        escape_html(&js_text(item.get("status"))),
                        command_label(&js_text(item.get("command"))),
                        escape_html(stage_label(&js_text(item.get("stage")))),
                        escape_html(&ago(&js_text(item.get("queued_at")), now))
                    ))
                    .collect::<String>()
            )
        };
        let (name, center, panels) = match &shown {
            Shown::Page(page) => {
                let archived = truthy(page.get("archived"));
                let center = format!(
                    "<h1>{}</h1>{}{}<h2>出典</h2><ol>{}</ol>",
                    escape_html(&js_text(page.get("title"))),
                    if archived {
                        "<p>アーカイブ済み</p>"
                    } else {
                        ""
                    },
                    display_page(page, &known),
                    items(page.get("citations"))
                        .iter()
                        .enumerate()
                        .map(|(index, citation)| {
                            let source = citation.get("source");
                            let name = escape_html(&js_text(
                                source.and_then(|source| source.get("objectId")),
                            ));
                            let label = match source
                                .and_then(|source| source.get("sourceId"))
                                .and_then(ConfigValue::as_str)
                            {
                                Some(id) => format!(
                                    "<a href=\"/?source={}\">{name}</a>",
                                    encode_uri_component(id)
                                ),
                                None => name,
                            };
                            format!(
                                "<li>{label} {}{}<input type=\"hidden\" name=\"value\" value=\"{}\"><button name=\"action\" value=\"cite\">引用箇所を見る</button></form></li>",
                                line_range(citation.get("range")),
                                form("inline"),
                                index + 1
                            )
                        })
                        .collect::<String>()
                );
                let manage = format!(
                    "<section class=\"panel\"><h2>このページの管理</h2><p class=\"hint\">検査は引用とリンクを確かめます。内容の検査は AI が関連ページとの食い違いを調べます。</p>{}<button name=\"action\" value=\"verify\">検査</button><button name=\"action\" value=\"content\">内容を検査</button>{}</form></section>",
                    form("row"),
                    if archived {
                        "<button name=\"action\" value=\"restore\">アーカイブから戻す</button>"
                    } else {
                        "<button name=\"action\" value=\"archive\">アーカイブする</button>"
                    }
                );
                let panels = if archived {
                    manage
                } else {
                    format!(
                        concat!(
                            "<section class=\"panel\"><h2>このページに質問する</h2>{}<input type=\"hidden\" name=\"second\" data-selection>",
                            "<textarea name=\"value\" rows=\"2\" required placeholder=\"このページの内容について質問します\"></textarea>",
                            "<p class=\"hint selection-label\" hidden>本文で選択した範囲について質問します</p>",
                            "<button class=\"primary\" name=\"action\" value=\"query\">質問する</button></form></section>",
                            "<section class=\"panel\"><h2>AI に指示して直す</h2><p class=\"hint\">ページは直接編集せず、指示で直します。引用は保たれます。</p>{}",
                            "<textarea name=\"value\" rows=\"2\" required placeholder=\"例：点検前の確認事項を箇条書きにしてください\"></textarea>",
                            "<label>対象の節・範囲（本文を選択すると入ります）<input name=\"second\" data-selection></label>",
                            "<details><summary>資料にない内容を補足する</summary><label>補足の内容<textarea name=\"supplement\" rows=\"2\"></textarea></label>",
                            "<label>記録者<input name=\"third\"></label></details>",
                            "<button class=\"primary\" name=\"action\" value=\"instruct\">指示を実行</button></form></section>{}"
                        ),
                        form(""),
                        form(""),
                        manage
                    )
                };
                ("page", center, panels)
            }
            Shown::Source((id, source, reflected)) => {
                let object_id = js_text(source.get("objectId"));
                let mut panels = String::new();
                if !reflected {
                    panels += &format!(
                        "<section class=\"panel\"><h2>ページに反映する</h2><p class=\"hint\">この原本は保存されていますが、AI はまだページに書いていません。未反映の原本をまとめて反映します。</p>{}<button class=\"primary\" name=\"action\" value=\"pending\">未反映の原本を反映する</button></form></section>",
                        form("row")
                    );
                }
                if crate::wiki_fetch::is_web_address(&object_id) {
                    panels += &format!(
                        "<section class=\"panel\"><h2>取り直す</h2><p class=\"hint\">同じ URL をもう一度取得します。中身が変わっていれば、この原本の新しい版になります。</p>{}<input type=\"hidden\" name=\"url\" value=\"{}\"><button name=\"action\" value=\"ingress\">この URL を取り直す</button></form></section>",
                        form("row"),
                        escape_html(&object_id)
                    );
                }
                (
                    "source",
                    self.source_view(id, source, *reflected, &content)?,
                    panels,
                )
            }
            Shown::Rules(rules) => (
                "rules",
                render(rules),
                format!(
                    "<section class=\"panel\"><h2>決まりを追加する</h2><p class=\"hint\">ここに足した決まりは、これ以降のすべての取り込みと指示に効きます。</p>{}<textarea name=\"value\" rows=\"2\" required placeholder=\"例：手順は番号付きで書く\"></textarea><button class=\"primary\" name=\"action\" value=\"rule\">決まりを追加</button></form></section>",
                    form("")
                ),
            ),
            Shown::History => (
                "history",
                format!(
                    "<h1>作業の履歴</h1><p class=\"meta\">取り込み、質問、指示、検査は、1回ごとに作業として記録されます。開くと結果を見られ、取り消せるものは取り消せます。</p>{}",
                    work_list(&history)
                ),
                String::new(),
            ),
            Shown::Top => {
                let scope = js_text(about.get("out_of_scope"));
                let center = format!(
                    "<h1>{}</h1><p class=\"lead\">{}</p>{}{}<input name=\"value\" required placeholder=\"この {kind} に質問する\"><button class=\"primary\" name=\"action\" value=\"query\">質問する</button></form>",
                    escape_html(&js_text(about.get("title"))),
                    escape_html(&js_text(about.get("purpose"))),
                    if scope.is_empty() {
                        String::new()
                    } else {
                        format!("<p class=\"meta\">範囲外：{}</p>", escape_html(&scope))
                    },
                    form("ask"),
                    kind = if base.kind == "para" { "PARA" } else { "Wiki" },
                );
                let issues = match (open_issues, inspection) {
                    (Some(open), Some(inspection)) => format!(
                        "<a class=\"tile\" href=\"/?{}\"><b>{open}</b><span>未対応の指摘</span><small>前回の検査の結果を開く</small></a>",
                        escape_html(&work_link(inspection))
                    ),
                    _ => "<div class=\"tile\"><b>–</b><span>未対応の指摘</span><small>まだ検査していません</small></div>".to_owned(),
                };
                let tiles = format!(
                    concat!(
                        "<div class=\"tiles\"><a class=\"tile\" href=\"#pages\"><b>{}</b><span>ページ</span><small>AI がまとめた記事</small></a>",
                        "<div class=\"tile\"><b>{}</b><span>原本</span><small>取り込んだ資料</small></div>",
                        "<div class=\"tile{}\"><b>{count}</b><span>未反映</span><small>まだページに書かれていない原本</small>{}</div>{issues}</div>"
                    ),
                    active.len(),
                    sources.len(),
                    if count > 0 { " warn" } else { "" },
                    if count > 0 {
                        format!(
                            "{}<button name=\"action\" value=\"pending\">反映する</button></form>",
                            form("row")
                        )
                    } else {
                        String::new()
                    },
                    count = count,
                    issues = issues,
                );
                let categories = if base.kind == "para" {
                    format!(
                        "<div class=\"tiles\">{}</div>",
                        CATEGORIES
                            .iter()
                            .map(|category| format!(
                                "<div class=\"tile\"><b>{}</b><span>{category}</span><small>{}</small></div>",
                                active
                                    .iter()
                                    .filter(|page| str_of(page, "id")
                                        .starts_with(&format!("{category}/")))
                                    .count(),
                                match *category {
                                    "Projects" => "期限のある取り組み",
                                    "Areas" => "続けて担当する領域",
                                    "Resources" => "関心のあるテーマの資料",
                                    _ => "終わったもの、休止中のもの",
                                }
                            ))
                            .collect::<String>()
                    )
                } else {
                    String::new()
                };
                let pages_panel = if recent.is_empty() {
                    "<section class=\"panel\" id=\"pages\"><h2>最近更新したページ</h2><p class=\"empty\">まだページがありません。左上の「資料を取り込む」に URL かファイルを渡すと、AI が読んでページを書きます。</p></section>".to_owned()
                } else {
                    format!(
                        "<section class=\"panel\" id=\"pages\"><h2>最近更新したページ</h2><ul class=\"cards\">{}</ul></section>",
                        recent
                            .iter()
                            .take(8)
                            .map(|page| format!(
                                "<li><a href=\"/?page={}\"><strong>{}</strong><span>{}</span><time>{}</time></a></li>",
                                encode_uri_component(&js_text(page.get("id"))),
                                escape_html(&js_text(page.get("title"))),
                                escape_html(&snippet(str_of(page, "markdown"))),
                                escape_html(&ago(&js_text(page.get("changed")), now))
                            ))
                            .collect::<String>()
                    )
                };
                let panels = format!(
                    concat!(
                        "{tiles}{categories}{pages_panel}",
                        "<section class=\"panel\"><h2>最近の作業</h2>{}<p class=\"more\"><a href=\"/?history\">作業の履歴をすべて見る</a></p></section>",
                        "<section class=\"panel\"><h2>点検する</h2><p class=\"hint\">検査は、引用が原本にあるか、版が古くないか、リンク切れがないかを確かめます。内容の検査は、AI がページ同士の食い違いを調べます。</p>",
                        "{}<button name=\"action\" value=\"verify\">検査</button><button name=\"action\" value=\"content\">内容を検査</button></form></section>"
                    ),
                    work_list(&history[..history.len().min(5)]),
                    form("row"),
                    tiles = tiles,
                    categories = categories,
                    pages_panel = pages_panel,
                );
                ("top", center, panels)
            }
        };
        // Where the shown item sits: its section and folders, then its name.
        let mut crumbs: Vec<String> = Vec::new();
        match &shown {
            Shown::Page(page) => {
                crumbs.push(
                    if truthy(page.get("archived")) {
                        "アーカイブ"
                    } else {
                        "ページ"
                    }
                    .into(),
                );
                let id = js_text(page.get("id"));
                let mut parts: Vec<String> = id.split('/').map(str::to_owned).collect();
                parts.pop();
                crumbs.extend(parts);
                crumbs.push(js_text(page.get("title")));
            }
            Shown::Source((_, source, _)) => {
                let (parts, label) = source_path(&js_text(source.get("objectId")));
                crumbs.push("原本".into());
                crumbs.extend(parts);
                crumbs.push(label);
            }
            Shown::Rules(_) => crumbs.push("運用の決まり".into()),
            Shown::History => crumbs.push("作業の履歴".into()),
            Shown::Top => {}
        }
        let crumbs = if crumbs.is_empty() {
            String::new()
        } else {
            format!(
                "<div class=\"crumbs\">{}</div>",
                crumbs
                    .iter()
                    .map(|part| format!("<span>{}</span>", escape_html(part)))
                    .collect::<String>()
            )
        };
        let tree_or_empty = |tree: &Tree| {
            if tree.folders.is_empty() && tree.leaves.is_empty() {
                "<p class=\"empty\">まだありません</p>".to_owned()
            } else {
                tree.html()
            }
        };
        let current_mark = |on: bool| {
            if on { " aria-current=\"page\"" } else { "" }
        };
        let body = format!(
            concat!(
                "<!doctype html><html lang=\"ja\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">",
                "{refresh}<title>{title}</title><style>{style}</style>",
                "<main><nav><details class=\"pane\" open><summary>メニュー</summary>",
                "<a class=\"brand\" href=\"/\"><span class=\"logo\"></span><strong>{title}</strong><span class=\"kind\">{kind}</span></a>",
                "<details class=\"add\"{add_open}><summary>資料を取り込む</summary>",
                "<form method=\"post\" enctype=\"multipart/form-data\" action=\"/action\"><input type=\"hidden\" name=\"token\" value=\"{form_key}\">",
                "<p class=\"hint\">URL かファイルを渡すと、原本として保存し、AI が読んでページに書きます。</p>",
                "<label class=\"field\"><span>URL</span><input type=\"url\" name=\"url\" placeholder=\"https://example.com/page\"></label>",
                "<label class=\"field\"><span>ファイル</span><input type=\"file\" name=\"upload\"></label>",
                "<details><summary>プロジェクト内のファイルを指定する</summary><label class=\"field\"><span>プロジェクトからのパス</span><input name=\"value\" placeholder=\"manuals/pump.md\"></label></details>",
                "<button class=\"primary\" name=\"action\" value=\"ingress\">取り込む</button></form></details>",
                "<ul class=\"menu\"><li><a href=\"/\"{top_current}>ホーム</a></li><li><a href=\"/?history\"{history_current}>作業の履歴</a></li>",
                "<li><a href=\"/?rules\"{rules_current}>運用の決まり</a></li></ul>",
                "<h2>ページ<small>AI がまとめた記事</small></h2>{menu}<h2>原本<small>取り込んだ資料</small></h2>{sources}",
                "{archived}</details></nav>",
                "<section class=\"view\">{crumbs}{work}<article id=\"content\" class=\"{name}\">{center}</article>{panels}</section></main>",
                "{script}</html>"
            ),
            refresh = if working {
                "<meta http-equiv=\"refresh\" content=\"2\">"
            } else {
                ""
            },
            title = escape_html(&js_text(about.get("title"))),
            style = STYLE,
            kind = if base.kind == "para" { "PARA" } else { "Wiki" },
            add_open = if sources.is_empty() { " open" } else { "" },
            form_key = form_key,
            top_current = current_mark(matches!(shown, Shown::Top)),
            history_current = current_mark(matches!(shown, Shown::History)),
            rules_current = current_mark(matches!(shown, Shown::Rules(_))),
            menu = tree_or_empty(&page_tree),
            sources = tree_or_empty(&source_tree),
            archived = if archive_tree.leaves.is_empty() {
                String::new()
            } else {
                format!(
                    "<details class=\"archive\"{}><summary>アーカイブ<small>{} 件</small></summary>{}</details>",
                    if current.is_some_and(|page| truthy(page.get("archived"))) {
                        " open"
                    } else {
                        ""
                    },
                    archive_tree.leaves.len(),
                    archive_tree.html()
                )
            },
            crumbs = crumbs,
            work = match &work {
                Some(item) => format!(
                    "<div class=\"work {}\">{}{}</div>",
                    escape_html(&js_text(item.get("status"))),
                    work_summary(item),
                    render_work(Some(item), form_key, current_id.as_deref())
                ),
                None => String::new(),
            },
            name = name,
            center = center,
            panels = panels,
            script = if current.is_some() {
                format!("<script nonce=\"{selection_key}\">{SELECTION_SCRIPT}</script>")
            } else {
                String::new()
            },
        );
        Ok(Reply {
            status: 200,
            headers: vec![(
                "content-security-policy",
                format!("{policy}; frame-ancestors 'none'; script-src 'nonce-{selection_key}'"),
            )],
            body,
        })
    }
}

/// `path.basename(name)` on this platform: Windows also drops a drive like `a:`.
fn basename(name: &str) -> String {
    let mut rest = name;
    if cfg!(windows) {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            rest = &rest[2..];
        }
    }
    let separators: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
    let trimmed = rest.trim_end_matches(separators);
    trimmed
        .rsplit(separators)
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// `/\p{C}/u`: control, format, surrogate, private use and unassigned characters.
fn is_control(c: char) -> bool {
    use icu_properties::props::{GeneralCategory, GeneralCategoryGroup};
    let category = icu_properties::CodePointMapData::<GeneralCategory>::new().get(c);
    GeneralCategoryGroup::Other.contains(category)
}

/// Serves the screen until `cancel` aborts, then finishes the queued work.
pub fn serve(
    request: WikiServeRequest,
    host: Arc<dyn WikiServeHost>,
    cancel: &Cancellation,
) -> Result<(), String> {
    let settings = &request.settings;
    let listener =
        std::net::TcpListener::bind((settings.host.as_str(), settings.port)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AddrInUse {
                format!(
                    "listen EADDRINUSE: address already in use {}:{}",
                    settings.host, settings.port
                )
            } else {
                format!("listen {}:{}: {error}", settings.host, settings.port)
            }
        })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let digest = request
        .token
        .as_deref()
        .map(|token| Sha256::digest(token.as_bytes()).into());
    let hosts = settings
        .access
        .allowed_hosts
        .iter()
        .map(|host| hostname(host))
        .collect();
    let header_timeout = Duration::from_secs(settings.limits.request_timeout_seconds.min(60));
    let (sender, receiver) = mpsc::channel::<Job>();
    let base = request.base.clone();
    // Instructions run one after another, as recorded work.
    let worker = std::thread::spawn(move || {
        for job in receiver {
            let mut command = base.clone();
            command.command = job.command;
            command.args = job.args;
            command.values = job.values;
            command.run_id = Some(job.run_id);
            command.track = true;
            command.print_progress = false;
            let _ = crate::wiki_commands::run(&command, &Cancellation::default());
        }
    });
    let server = Arc::new(WikiServer {
        request,
        hosts,
        digest,
        port,
        running: AtomicUsize::new(0),
        form_key: uuid::Uuid::new_v4().to_string(),
        jobs: Mutex::new(Some(sender)),
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let served: Result<(), String> = runtime.block_on({
        let server = server.clone();
        async move {
            let listener =
                tokio::net::TcpListener::from_std(listener).map_err(|error| error.to_string())?;
            host.listening(port);
            loop {
                let accepted = tokio::select! {
                    accepted = listener.accept() => accepted,
                    () = tokio::time::sleep(Duration::from_millis(100)) => {
                        if cancel.check().is_err() {
                            return Ok(());
                        }
                        continue;
                    }
                };
                let Ok((stream, peer)) = accepted else {
                    continue;
                };
                let server = server.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |request| {
                        let server = server.clone();
                        async move { Ok::<_, Infallible>(server.handle(request, peer).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .timer(hyper_util::rt::TokioTimer::new())
                        .header_read_timeout(header_timeout)
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    if let Ok(mut jobs) = server.jobs.lock() {
        jobs.take();
    }
    let _ = worker.join();
    served
}
