//! The HTML files of Wiki and PARA pages (spec 13.4 and 28.12 step 5a): each page as a
//! standalone document with its sources, and the page record in a closing comment.
//!
//! The markup is what `tools/rag/authoring/html.mjs` wrote.

use base64::Engine;

use crate::config_value::ConfigValue;
use crate::index_stage::{field, items, string_field};
use crate::markdown::{escape_html, render};

/// The policy of published pages. The `serve` screen allows its own forms.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'none'";

pub fn link(href: &str, label: &str) -> String {
    format!(
        "<a href=\"{}\">{}</a>",
        escape_html(href),
        escape_html(label)
    )
}

/// `renderDocument(title, body, { metadata })`
pub fn render_document(title: &str, body: &str, metadata: Option<&ConfigValue>) -> String {
    let title = escape_html(title);
    let footer = metadata.map_or_else(String::new, |metadata| {
        format!(
            "<!-- knowledge-nest:publication {} -->\n",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(metadata.to_js_json())
        )
    });
    format!(
        "<!doctype html>
<html lang=\"ja\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<meta http-equiv=\"Content-Security-Policy\" content=\"{CONTENT_SECURITY_POLICY}\">
<title>{title} | Knowledge-Nest</title>
<style>
  body {{ font: 1rem/1.65 system-ui, sans-serif; max-width: 54rem; margin: auto; padding: 1.5rem; color-scheme: light dark; }}
  nav, section {{ border-block-end: 1px solid GrayText; padding-block: .65rem; }}
  nav a + a {{ margin-inline-start: 1.25rem; }}
  article {{ overflow-wrap: anywhere; }}
  pre {{ overflow: auto; padding: .85rem; border: 1px solid GrayText; border-radius: .35rem; }}
  img {{ max-width: 100%; height: auto; }}
</style>
<main><h1>{title}</h1>{body}</main>
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
        .map(|citation| {
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
            let source_link = match protocol(url).as_deref() {
                Some("http:" | "https:" | "file:" | "s3:" | "knowledge-nest:") => {
                    link(url, object_id)
                }
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
                "<li>{source_link} · L{}–L{}{}{original_link}</li>",
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
            "<section><h2>出典</h2><ol>{}</ol></section>",
            entries.join("\n")
        )
    };
    render_document(
        title,
        &format!("<article>{}</article>{sources}", render(markdown)),
        Some(metadata),
    )
}
