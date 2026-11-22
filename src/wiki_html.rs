//! The HTML files of Wiki and PARA pages (spec 13.4 and 28.12 step 5a): each page as a
//! standalone document with its sources, and the page record in a closing comment.
//!
//! A page links up to the contents of its folder (`index.html`), lists its sections when it has
//! three or more, and its citation numbers jump to their citation.

use base64::Engine;

use crate::config_value::ConfigValue;
use crate::index_stage::{field, items, string_field};
use crate::markdown::{escape_html, render};

/// The policy of published pages. The `serve` screen allows its own forms.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'none'";

/// What a page file of this form holds. A page file without it is written again.
pub(crate) const PAGE_MARK: &str = "<nav class=\"up\">";

const STYLE: &str = concat!(
    ":root{--bg:#fff;--card:#fff;--text:#111113;--soft:#3a3a40;--muted:#71717a;--line:#ebebee;--hover:#f1f1f3;--accent:#5b5bd6;--accent-soft:#eeeefb;color-scheme:light dark}",
    "@media(prefers-color-scheme:dark){:root{--bg:#0a0a0b;--card:#141416;--text:#ededef;--soft:#c6c6ce;--muted:#8b8b94;--line:#222226;--hover:#1b1b1f;--accent:#a3a3ff;--accent-soft:#1c1c3a}}",
    "*{box-sizing:border-box}",
    "body{margin:0;background:var(--bg);color:var(--text);font:15px/1.7 Inter,'SF Pro Text',-apple-system,'Segoe UI Variable Text','Segoe UI','Hiragino Sans','Noto Sans JP','Yu Gothic UI',Meiryo,sans-serif;-webkit-font-smoothing:antialiased}",
    "a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline;text-underline-offset:3px}",
    "main{max-width:1008px;margin:0 auto;padding:40px 24px 96px;overflow-wrap:anywhere}",
    "main:has(.toc){display:grid;grid-template-columns:minmax(0,1fr) 208px;column-gap:64px}main>*{grid-column:1;min-width:0}main>.up,main>h1{grid-column:1/-1}",
    ".up{display:flex;flex-wrap:wrap;align-items:center;gap:6px;font-size:13px;color:var(--muted);margin-bottom:20px}.up span::before{content:'/';margin-right:6px;opacity:.5}",
    "h1{font-size:40px;line-height:1.2;font-weight:700;letter-spacing:-.015em;font-feature-settings:'palt';margin:0 0 20px}",
    ".body{color:var(--soft)}.body>p,.body>ul,.body>ol,.body>blockquote{font-size:16px;line-height:1.95;max-width:40em}.body>p{margin:0 0 1.1em;line-break:strict}",
    ".body>p:first-child{font-size:19px;line-height:1.8;color:var(--text);margin-bottom:1.4em}",
    ".body>ul,.body>ol{margin:0 0 1.1em;padding-left:1.4em}.body li{margin:2px 0}.body li::marker{color:var(--muted)}",
    ".body h2{color:var(--text);font-size:23px;line-height:1.4;font-weight:700;letter-spacing:-.01em;font-feature-settings:'palt';margin:56px 0 14px;max-width:calc(40em*16/23);scroll-margin-top:24px}",
    ".body h3{color:var(--text);font-size:17px;line-height:1.5;font-weight:650;margin:28px 0 8px}",
    ".body a:not(.ref){color:var(--text);font-weight:500;text-decoration:underline;text-decoration-color:color-mix(in srgb,var(--accent) 55%,transparent);text-decoration-thickness:1.5px;text-underline-offset:4px}.body a:not(.ref):hover{color:var(--accent);text-decoration-color:var(--accent)}",
    ".body code{background:var(--hover);padding:1px 5px;border-radius:6px;font-size:.92em}pre{overflow:auto;padding:14px 16px;border:1px solid var(--line);border-radius:12px;font:13px/1.65 ui-monospace,SFMono-Regular,Consolas,monospace}pre code{background:none;padding:0}",
    "blockquote{margin:16px 0;padding:2px 16px;border-left:3px solid var(--line);color:var(--muted)}img{max-width:100%;height:auto}",
    "table{border-collapse:collapse;width:100%;font-size:14.5px}th,td{padding:10px 12px;text-align:left;border-bottom:1px solid var(--line)}th{padding:8px 12px;font-size:12.5px;font-weight:600;color:var(--muted)}",
    ".ref{display:inline-block;min-width:18px;padding:0 5px;margin-left:4px;border-radius:6px;background:var(--hover);color:var(--muted);font:600 11px/18px ui-sans-serif,system-ui,sans-serif;font-feature-settings:'tnum';text-align:center;vertical-align:2px}.ref:hover{background:var(--text);color:var(--bg);text-decoration:none}",
    "main>.toc{grid-column:2;grid-row:3/span 3;align-self:start;position:sticky;top:28px;max-height:calc(100vh - 56px);overflow:auto;font-size:13px;line-height:1.5}",
    ".toc h2{font-size:12px;line-height:1.4;font-weight:600;margin:6px 0 10px}.toc ol{list-style:none;margin:0;padding:0;border-left:1px solid var(--line)}",
    ".toc a{display:block;margin-left:-1px;padding:5px 0 5px 14px;border-left:1px solid transparent;color:var(--muted)}.toc a:hover{color:var(--text);border-left-color:var(--text);text-decoration:none}",
    ".notes{margin-top:72px}.notes h2{font-size:14px;font-weight:600;margin:0 0 12px}",
    ".notes ol{list-style:none;padding:0;margin:0;counter-reset:cite;display:grid;grid-template-columns:repeat(auto-fill,minmax(250px,1fr));gap:8px;font-size:12.5px;line-height:1.5}",
    ".notes li{counter-increment:cite;position:relative;padding:10px 12px 10px 42px;border:1px solid var(--line);border-radius:10px;background:var(--card);color:var(--muted);scroll-margin-top:24px}",
    ".notes li::before{content:counter(cite);position:absolute;left:12px;top:10px;min-width:18px;padding:0 5px;border-radius:6px;background:var(--hover);font:600 11px/18px ui-sans-serif,system-ui,sans-serif;font-feature-settings:'tnum';text-align:center}",
    ".notes li:target{border-color:var(--accent);box-shadow:0 0 0 3px var(--accent-soft)}.notes li>a:first-child{color:var(--text);font-weight:500}",
    "@media(max-width:1000px){main:has(.toc){display:flex;flex-direction:column}main:has(.toc) .body{display:contents}main>.up,main>h1{order:-3}main:has(.toc) .body>p:first-child{order:-2}",
    "main>.toc{order:-1;position:static;align-self:stretch;max-width:calc(40em*16/13);max-height:none;margin:4px 0 0;padding:14px 18px;border:1px solid var(--line);border-radius:12px}.toc h2{margin-top:0}.toc ol{columns:2;column-gap:32px}.toc li{break-inside:avoid}.body h2{margin-top:44px}}",
    "@media(max-width:560px){main{padding:28px 18px 64px}h1{font-size:30px}.body>p:first-child{font-size:17.5px}.body h2{font-size:20px}.toc ol{columns:1}}",
);

pub fn link(href: &str, label: &str) -> String {
    format!(
        "<a href=\"{}\">{}</a>",
        escape_html(href),
        escape_html(label)
    )
}

/// The sections of a page: each `h2` gets an id, and three or more of them a table of contents.
pub(crate) fn sectioned(html: &str) -> (String, String) {
    let heading = regex::Regex::new(r"<h2>(.*?)</h2>").expect("a valid pattern");
    let markup = regex::Regex::new(r"<[^>]+>| \[\d+\]").expect("a valid pattern");
    let mut entries: Vec<String> = Vec::new();
    let text = heading
        .replace_all(html, |found: &regex::Captures<'_>| {
            entries.push(markup.replace_all(&found[1], "").into_owned());
            format!("<h2 id=\"section-{}\">{}</h2>", entries.len(), &found[1])
        })
        .into_owned();
    if entries.len() < 3 {
        return (text, String::new());
    }
    let contents = format!(
        "<nav class=\"toc\" aria-label=\"目次\"><h2>目次</h2><ol>{}</ol></nav>",
        entries
            .iter()
            .enumerate()
            .map(|(index, entry)| format!(
                "<li><a href=\"#section-{}\">{entry}</a></li>",
                index + 1
            ))
            .collect::<String>()
    );
    (text, contents)
}

/// The citation numbers of a page's text (` [1]`) as links to their citation in the list below.
pub(crate) fn cited_markers(html: &str, citations: usize) -> String {
    let marker = regex::Regex::new(r" \[(\d+)\]").expect("a valid pattern");
    marker
        .replace_all(html, |found: &regex::Captures<'_>| {
            match found[1].parse::<usize>() {
                Ok(number) if (1..=citations).contains(&number) => {
                    format!("<a class=\"ref\" href=\"#cite-{number}\">{number}</a>")
                }
                _ => found[0].to_owned(),
            }
        })
        .into_owned()
}

/// `renderDocument(title, body, { metadata })`: the way up is the contents of the folder, as far
/// above the page as its ID is deep.
pub fn render_document(title: &str, body: &str, metadata: Option<&ConfigValue>) -> String {
    let title = escape_html(title);
    let footer = metadata.map_or_else(String::new, |metadata| {
        format!(
            "<!-- knowledge-nest:publication {} -->\n",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(metadata.to_js_json())
        )
    });
    let id = metadata
        .and_then(|metadata| string_field(metadata, "id"))
        .unwrap_or_default();
    let navigation = if id.is_empty() {
        String::new()
    } else {
        let parts: Vec<&str> = id.split('/').collect();
        format!(
            "<nav class=\"up\"><a href=\"{}index.html\">目録</a>{}</nav>",
            "../".repeat(parts.len() - 1),
            parts[..parts.len() - 1]
                .iter()
                .map(|part| format!("<span>{}</span>", escape_html(part)))
                .collect::<String>()
        )
    };
    format!(
        "<!doctype html>
<html lang=\"ja\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<meta http-equiv=\"Content-Security-Policy\" content=\"{CONTENT_SECURITY_POLICY}\">
<title>{title} | Knowledge-Nest</title>
<style>{STYLE}</style>
<main>{navigation}<h1>{title}</h1>{body}</main>
</html>\n{footer}"
    )
}

/// `citationLocations(locations)`: the distinct pages and symbols the cited lines cover.
pub fn citation_locations(locations: Option<&ConfigValue>) -> String {
    let mut labels: Vec<String> = Vec::new();
    for location in items(locations) {
        let label = match field(location, "page") {
            Some(ConfigValue::Number(page)) if *page != 0.0 => {
                format!("p. {}", crate::config_value::js_number(*page))
            }
            _ => match field(location, "symbol") {
                Some(ConfigValue::String(symbol)) if !symbol.is_empty() => symbol.clone(),
                _ => continue,
            },
        };
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels.join(", ")
}

/// The scheme of an absolute URL with `:`, as `URL.parse(url)?.protocol` gives it.
fn protocol(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .map(|parsed| format!("{}:", parsed.scheme()))
}

fn line(range: Option<&ConfigValue>, name: &str) -> String {
    match range.and_then(|range| field(range, name)) {
        Some(ConfigValue::Number(value)) => crate::config_value::js_number(*value),
        Some(other) => other.to_js_json(),
        None => "undefined".into(),
    }
}

/// `renderPublication({ title, markdown, citations }, metadata)`
pub fn render_publication(
    title: &str,
    markdown: &str,
    citations: &[ConfigValue],
    metadata: &ConfigValue,
) -> String {
    let entries: Vec<String> = citations
        .iter()
        .enumerate()
        .map(|(index, citation)| {
            let source = field(citation, "source");
            let source_url = source
                .and_then(|source| string_field(source, "url"))
                .unwrap_or_default();
            let object_id = source
                .and_then(|source| string_field(source, "objectId"))
                .unwrap_or_default();
            let original = field(citation, "original")
                .filter(|value| !matches!(value, ConfigValue::Null | ConfigValue::Bool(false)));
            let original_uri = original
                .and_then(|original| string_field(original, "uri"))
                .unwrap_or_default();
            let use_original = original.is_some() && source_url.starts_with("knowledge-nest://");
            let url = if use_original {
                original_uri
            } else {
                source_url
            };
            // A source in the project is a path from the page, which has no scheme.
            let source_link = match protocol(url).as_deref() {
                Some("http:" | "https:" | "file:" | "s3:" | "knowledge-nest:") => {
                    link(url, object_id)
                }
                None if !url.is_empty() => link(url, object_id),
                _ => escape_html(object_id),
            };
            let position = citation_locations(field(citation, "locations"));
            let range = field(citation, "range");
            let original_link = if original.is_some() && !use_original {
                format!(" · {}", link(original_uri, "原本"))
            } else {
                String::new()
            };
            format!(
                "<li id=\"cite-{}\">{source_link} · L{}–L{}{}{original_link}</li>",
                index + 1,
                line(range, "startLine"),
                line(range, "endLine"),
                if position.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", escape_html(&position))
                }
            )
        })
        .collect();
    let sources = if entries.is_empty() {
        String::new()
    } else {
        format!(
            "<section class=\"notes\"><h2>出典</h2><ol>{}</ol></section>",
            entries.join("\n")
        )
    };
    let (text, contents) = sectioned(&render(markdown));
    render_document(
        title,
        &format!(
            "{contents}<article class=\"body\">{}</article>{sources}",
            cited_markers(&text, citations.len())
        ),
        Some(metadata),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_stage::{object, text};

    #[test]
    fn three_sections_make_a_table_of_contents() {
        let (text, contents) =
            sectioned("<p>lead [1]</p><h2>a &amp; b</h2><h2><code>c</code> [2]</h2><h2>d</h2>");
        assert_eq!(
            text,
            concat!(
                "<p>lead [1]</p><h2 id=\"section-1\">a &amp; b</h2>",
                "<h2 id=\"section-2\"><code>c</code> [2]</h2><h2 id=\"section-3\">d</h2>"
            )
        );
        assert_eq!(
            contents,
            concat!(
                "<nav class=\"toc\" aria-label=\"目次\"><h2>目次</h2><ol>",
                "<li><a href=\"#section-1\">a &amp; b</a></li>",
                "<li><a href=\"#section-2\">c</a></li>",
                "<li><a href=\"#section-3\">d</a></li></ol></nav>"
            )
        );
        let (text, contents) = sectioned("<h2>a</h2><p>x</p><h2>b</h2>");
        assert_eq!(
            text,
            "<h2 id=\"section-1\">a</h2><p>x</p><h2 id=\"section-2\">b</h2>"
        );
        assert_eq!(contents, "");
    }

    #[test]
    fn a_page_links_up_to_the_contents_and_down_to_its_citations() {
        let page = object(vec![("id", text("a/b/scrum"))]);
        let citation = object(vec![
            (
                "source",
                object(vec![
                    ("url", text("../../../docs/scrum.md")),
                    ("objectId", text("scrum.md")),
                ]),
            ),
            (
                "range",
                object(vec![
                    ("startLine", ConfigValue::Number(3.0)),
                    ("endLine", ConfigValue::Number(4.0)),
                ]),
            ),
        ]);
        let html = render_publication("スクラム", "枠組みである。 [1] [2]", &[citation], &page);
        assert!(html.contains(
            "<main><nav class=\"up\"><a href=\"../../index.html\">目録</a><span>a</span><span>b</span></nav><h1>スクラム</h1><article class=\"body\"><p>枠組みである。<a class=\"ref\" href=\"#cite-1\">1</a> [2]</p>\n</article>"
        ));
        assert!(html.contains(
            "<section class=\"notes\"><h2>出典</h2><ol><li id=\"cite-1\"><a href=\"../../../docs/scrum.md\">scrum.md</a> · L3–L4</li></ol></section></main>"
        ));
        assert!(html.contains(PAGE_MARK));
    }
}
