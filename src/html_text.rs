//! The text of an HTML page (spec 6.7 and 28.13 step 6a), as `loadHtml` read it with cheerio:
//! scripts, styles, navigation, footers, asides and hidden elements are dropped, headings start
//! with `#` marks, blocks end lines, and the main content (`main`, `[role="main"]`, `article`, or
//! the body) is kept as trimmed nonblank lines.
//!
//! html5ever parses HTML as the WHATWG specification says, as parse5 did for cheerio, so the tree
//! is the same. The `parser` stays `cheerio` because the text and the document versions it is
//! part of stay the same.

use scraper::{Html, Node, Selector};

use crate::config_value::ConfigValue;
use crate::registry::js_space;

const REMOVED: &str = "script, style, noscript, template, nav, footer, aside, [hidden]";
const LINES: [&str; 14] = [
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "p",
    "li",
    "blockquote",
    "pre",
    "div",
    "section",
    "tr",
    "br",
];

fn heading(name: &str) -> Option<usize> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

/// `$(root).text()` after the line breaks of `find(...).before('\n').after('\n')`.
fn write(node: ego_tree::NodeRef<'_, Node>, root: bool, out: &mut String) {
    match node.value() {
        Node::Text(text) => out.push_str(text),
        Node::Element(element) => {
            let name = element.name();
            let line = !root && LINES.contains(&name);
            if line {
                out.push('\n');
            }
            if let Some(level) = heading(name) {
                out.push_str(&"#".repeat(level));
                out.push(' ');
            }
            for child in node.children() {
                write(child, false, out);
            }
            if line {
                out.push('\n');
            }
        }
        _ => {}
    }
}

/// `{ content, format: "markdown", parser: { tool: "cheerio" } }` for an HTML file.
pub fn parse(bytes: &[u8]) -> Result<ConfigValue, String> {
    // TextDecoder('utf-8', { fatal: true }) drops a byte order mark.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "The encoded data was not valid for encoding utf-8".to_owned())?;
    let mut html = Html::parse_document(text);
    let removed = Selector::parse(REMOVED).map_err(|_| "Invalid selector".to_owned())?;
    let ids: Vec<_> = html.select(&removed).map(|element| element.id()).collect();
    for id in ids {
        if let Some(mut node) = html.tree.get_mut(id) {
            node.detach();
        }
    }
    let first = |selector: &str| {
        Selector::parse(selector)
            .ok()
            .and_then(|selector| html.select(&selector).next().map(|element| element.id()))
    };
    let root = first(r#"main, [role="main"]"#)
        .or_else(|| first("article"))
        .or_else(|| first("body"));
    let mut text = String::new();
    if let Some(node) = root.and_then(|id| html.tree.get(id)) {
        write(node, true, &mut text);
    }
    let content = text
        .split('\n')
        .map(|line| {
            line.strip_suffix('\r')
                .unwrap_or(line)
                .trim_matches(js_space)
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ConfigValue::Object(vec![
        ("content".into(), ConfigValue::String(content)),
        ("format".into(), ConfigValue::String("markdown".into())),
        (
            "parser".into(),
            ConfigValue::Object(vec![("tool".into(), ConfigValue::String("cheerio".into()))]),
        ),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_main_content() {
        let parsed = parse(
            "<html><head><title>T</title><script>x</script></head><body><nav>menu</nav><main><h2>手順</h2><p>交換 &amp; 点検</p><div hidden>秘密</div><ul><li>a</li><li>b</li></ul></main><footer>f</footer></body></html>"
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(
            parsed.get("content").and_then(ConfigValue::as_str),
            Some("## 手順\n交換 & 点検\na\nb")
        );
    }
}
