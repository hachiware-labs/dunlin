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

const STYLE: &str = "body{font:1rem/1.6 system-ui;margin:0;color:#20252b;background:#faf9f6}main{display:grid;grid-template-columns:minmax(12rem,1fr) minmax(0,3fr) minmax(15rem,1.4fr);min-height:100vh}section,nav,aside{padding:1.25rem;border-right:1px solid #ccc}textarea{width:100%}pre{white-space:pre-wrap;overflow-wrap:anywhere}.pane>summary{display:none}@media(max-width:800px){main{display:block}.pane>summary{display:list-item}}";

const SELECTION_SCRIPT: &str = "document.addEventListener(\"pointerup\", () => { const selection = getSelection(); const section = document.querySelector(\"main>section\"); const text = selection && section.contains(selection.anchorNode) && section.contains(selection.focusNode) ? selection.toString().trim() : \"\"; if (text) { document.getElementById(\"action-target\").value = text.slice(0, 2000); document.getElementById(\"selection-label\").hidden = false; } });";

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
        if !supported.contains(&action.as_str())
            || (!["verify", "content", "archive", "restore", "pending"].contains(&action.as_str())
                && value.is_empty()
                && upload.is_none())
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
        if action == "ingress" && upload.is_none() {
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
        let page_link = |page: &&ConfigValue| {
            format!(
                "<li><a href=\"/?page={}\">{}</a></li>",
                encode_uri_component(&js_text(page.get("id"))),
                escape_html(&js_text(page.get("title")))
            )
        };
        let menu: String = if base.kind == "para" {
            CATEGORIES
                .iter()
                .map(|category| {
                    format!(
                        "<li>{category}<ul>{}</ul></li>",
                        active
                            .iter()
                            .filter(|page| str_of(page, "id").starts_with(&format!("{category}/")))
                            .map(page_link)
                            .collect::<String>()
                    )
                })
                .collect()
        } else {
            active.iter().map(page_link).collect()
        };
        let archived: String = content
            .iter()
            .filter(|page| truthy(page.get("archived")))
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
            .prepare("SELECT data FROM wiki_sources ORDER BY id")
            .map_err(sql)?;
        let sources: Vec<ConfigValue> = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sql)?
            .map(|row| row.map_err(sql).and_then(|data| parse(&data)))
            .collect::<Result<_, _>>()?;
        let mut recent = active.clone();
        recent.sort_by(|a, b| {
            crate::wiki_inspect::locale_compare(
                &js_text(b.get("changed")),
                &js_text(a.get("changed")),
            )
        });
        let recent: String = recent.iter().take(8).map(page_link).collect();
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
        let selection_key = uuid::Uuid::new_v4().to_string();
        let policy = CONTENT_SECURITY_POLICY.replace("form-action 'none'", "form-action 'self'");
        let form_key = &self.form_key;
        let known: Vec<String> = content
            .iter()
            .map(|page| str_of(page, "id").to_owned())
            .collect();
        let center = match (current, &rules) {
            (Some(current), _) => format!(
                "<h1>{}</h1>{}{}<h2>出典</h2><ol>{}</ol>",
                escape_html(&js_text(current.get("title"))),
                if truthy(current.get("archived")) {
                    "<p>アーカイブ済み</p>"
                } else {
                    ""
                },
                display_page(current, &known),
                items(current.get("citations"))
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
            ),
            (None, Some(rules)) => render(rules),
            (None, None) => format!(
                "<h1>{}</h1><h2>目的と範囲</h2><p>{}</p><p>{}</p><p>原本 {} 件、未反映 {count} 件</p>{}<h2>最近更新したページ</h2><ul>{recent}</ul>",
                escape_html(&js_text(about.get("title"))),
                escape_html(&js_text(about.get("purpose"))),
                escape_html(&js_text(about.get("out_of_scope"))),
                sources.len(),
                match (open_issues, inspection) {
                    (Some(open), Some(inspection)) => format!(
                        "<p><a href=\"/?{}\">前回の検査で未対応 {open} 件</a></p>",
                        escape_html(&work_link(inspection))
                    ),
                    _ => String::new(),
                }
            ),
        };
        let current_id = current.map(|page| js_text(page.get("id")));
        let body = format!(
            concat!(
                "<!doctype html><html lang=\"ja\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">",
                "{refresh}<title>{title}</title><style>{style}</style>",
                "<main><nav><details class=\"pane\" open><summary>資料の階層</summary><a href=\"/\">トップ</a> · <a href=\"/?rules\">運用の決まり</a><h2>ページ</h2><ul>{menu}</ul><h2>原本</h2><ul>{sources}",
                "</ul><h2>アーカイブ</h2><ul>{archived}</ul></details></nav><section>{center}",
                "</section><aside><details class=\"pane\" open><summary>質問と操作</summary><h2>質問と操作</h2>{target}",
                "<form method=\"post\" enctype=\"multipart/form-data\" action=\"/action{action}\"><input type=\"hidden\" name=\"token\" value=\"{form_key}\">",
                "<input type=\"hidden\" name=\"page\" value=\"{page}\">",
                "<label>質問、資料のパス、ページ ID、または作業 ID<textarea name=\"value\" rows=\"4\"></textarea></label><button name=\"action\" value=\"query\">質問する</button>",
                "<label>資料ファイル <input type=\"file\" name=\"upload\"></label><button name=\"action\" value=\"ingress\">資料を追加</button><button name=\"action\" value=\"pending\">未反映の資料を取り込む</button>",
                "<button name=\"action\" value=\"verify\">検査</button><button name=\"action\" value=\"content\">内容を検査</button>",
                "<button name=\"action\" value=\"archive\">アーカイブ</button><button name=\"action\" value=\"restore\">復元</button><button name=\"action\" value=\"undo\">取り消す</button>",
                "<details><summary>変更・出典</summary><label>対象の節・範囲、保存先ページ ID、または指摘の状態<input name=\"second\" id=\"action-target\"></label>",
                "<label>補足の記録者、またはページのタイトル<input name=\"third\"></label><label>利用者の補足<textarea name=\"supplement\" rows=\"2\"></textarea></label>",
                "{page_actions}",
                "<button name=\"action\" value=\"save-answer\">回答をページに保存</button><button name=\"action\" value=\"issue\">指摘の状態を更新</button>",
                "<button name=\"action\" value=\"rule\">今後の決まりに追記</button></details></form>",
                "{work}",
                "<h3>最近の作業</h3><ul>{history}</ul></details></aside></main>",
                "{script}</html>"
            ),
            refresh = if working {
                "<meta http-equiv=\"refresh\" content=\"2\">"
            } else {
                ""
            },
            title = escape_html(&js_text(about.get("title"))),
            style = STYLE,
            menu = menu,
            sources = sources
                .iter()
                .map(|source| format!("<li>{}</li>", escape_html(&js_text(source.get("objectId")))))
                .collect::<String>(),
            archived = archived,
            center = center,
            target = match current {
                Some(current) => format!(
                    "<p>対象 {}</p><p id=\"selection-label\" hidden>選択範囲を対象にします</p>",
                    escape_html(&js_text(current.get("title")))
                ),
                None => String::new(),
            },
            action = current_id.as_deref().map_or_else(String::new, |id| format!(
                "?page={}",
                encode_uri_component(id)
            )),
            form_key = form_key,
            page = escape_html(current_id.as_deref().unwrap_or_default()),
            page_actions = if current.is_some() {
                "<button name=\"action\" value=\"instruct\">ページへの指示を実行</button><button name=\"action\" value=\"cite\">指定した番号の出典を開く</button>"
            } else {
                ""
            },
            work = render_work(work.as_ref(), form_key, current_id.as_deref()),
            history = history
                .iter()
                .map(|item| format!(
                    "<li><a href=\"/?{}\">{}</a></li>",
                    escape_html(&work_link(item)),
                    escape_html(&format!(
                        "{} · {}",
                        js_text(item.get("command")),
                        stage_label(&js_text(item.get("stage")))
                    ))
                ))
                .collect::<String>(),
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
