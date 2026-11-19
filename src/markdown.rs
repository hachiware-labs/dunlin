//! Markdown to HTML as markdown-it 15 renders it with `{ html: false }` (spec 28.12 step 5a).
//!
//! Wiki and PARA pages, their published files and the `serve` screen used markdown-it. The
//! events of `pulldown-cmark` are written out in markdown-it's form: the same line breaks, `<hr>`
//! and `<br>` without a slash, the same table markup, links checked and encoded as markdown-it
//! checks and encodes them, and raw HTML written as text.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};

/// `escapeHtml` of markdown-it: `&`, `<`, `>` and `"`.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// `md.validateLink`: no `javascript:`, `vbscript:`, `file:` or `data:` links, except some images.
fn valid_link(url: &str) -> bool {
    let lower = url.trim().to_lowercase();
    let bad = ["vbscript:", "javascript:", "file:", "data:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme));
    !bad || ["gif", "png", "jpeg", "webp"]
        .iter()
        .any(|kind| lower.starts_with(&format!("data:image/{kind};")))
}

/// The parts `mdurl.parse(url, true)` finds.
#[derive(Default)]
struct Url {
    protocol: Option<String>,
    slashes: bool,
    auth: Option<String>,
    port: Option<String>,
    hostname: Option<String>,
    hash: Option<String>,
    search: Option<String>,
    pathname: Option<String>,
}

const SLASHED: [&str; 5] = ["http:", "https:", "ftp:", "gopher:", "file:"];
const NON_HOST: &[char] = &[
    '%', '/', '?', ';', '#', '\'', '{', '}', '|', '\\', '^', '`', '<', '>', '"', ' ', '\r', '\n',
    '\t',
];

fn host_part(part: &str) -> bool {
    part.chars().count() <= 63
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '_' | '-'))
}

/// `mdurl.parse(url, true)`. Indices are in UTF-16 units in JavaScript, but every delimiter it
/// looks for is ASCII, so byte offsets find the same places.
fn parse_url(url: &str) -> Url {
    let mut out = Url::default();
    let mut rest = js_trim(url).to_owned();
    let proto_len = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-')))
        .filter(|&end| end > 0 && rest[end..].starts_with(':'));
    let mut lower = String::new();
    if let Some(end) = proto_len {
        let proto = rest[..=end].to_owned();
        lower = proto.to_lowercase();
        rest = rest[proto.len()..].to_owned();
        out.protocol = Some(proto);
    }
    let hostless = lower == "javascript:";
    let mut slashes = false;
    if rest.starts_with("//") && !(out.protocol.is_some() && hostless) {
        rest = rest[2..].to_owned();
        out.slashes = true;
        slashes = true;
    }
    if !hostless && (slashes || (out.protocol.is_some() && !SLASHED.contains(&lower.as_str()))) {
        let host_end = rest.find(['/', '?', '#']);
        let at = match host_end {
            None => rest.rfind('@'),
            Some(end) => rest[..=end.min(rest.len() - 1)].rfind('@'),
        };
        if let Some(at) = at {
            out.auth = Some(rest[..at].to_owned());
            rest = rest[at + 1..].to_owned();
        }
        let mut host_end = rest.find(NON_HOST).unwrap_or(rest.len());
        if host_end > 0 && rest.as_bytes()[host_end - 1] == b':' {
            host_end -= 1;
        }
        let mut host = rest[..host_end].to_owned();
        rest = rest[host_end..].to_owned();
        // parseHost: a trailing `:digits` is the port.
        let digits = host.len() - host.bytes().rev().take_while(u8::is_ascii_digit).count();
        if digits > 0 && host.as_bytes()[digits - 1] == b':' {
            let port = host[digits..].to_owned();
            if !port.is_empty() {
                out.port = Some(port);
            }
            host.truncate(digits - 1);
        }
        let mut hostname = host;
        let ipv6 = hostname.starts_with('[') && hostname.ends_with(']') && hostname.len() > 1;
        if !ipv6 {
            let parts: Vec<String> = hostname.split('.').map(str::to_owned).collect();
            for (i, part) in parts.iter().enumerate() {
                if part.is_empty() || host_part(part) {
                    continue;
                }
                let replaced: String = part
                    .chars()
                    .map(|c| if (c as u32) > 127 { 'x' } else { c })
                    .collect();
                if host_part(&replaced) {
                    continue;
                }
                let mut valid: Vec<String> = parts[..i].to_vec();
                let mut not_host: Vec<String> = parts[i + 1..].to_vec();
                let start = part
                    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '_' | '-')))
                    .unwrap_or(part.len());
                if part[..start].chars().count() <= 63 {
                    valid.push(part[..start].to_owned());
                    not_host.insert(0, part[start..].to_owned());
                }
                if !not_host.is_empty() {
                    rest = not_host.join(".") + &rest;
                }
                hostname = valid.join(".");
                break;
            }
        }
        if hostname.encode_utf16().count() > 255 {
            hostname.clear();
        }
        if ipv6 {
            hostname = hostname[1..hostname.len() - 1].to_owned();
        }
        out.hostname = Some(hostname);
    }
    if let Some(hash) = rest.find('#') {
        out.hash = Some(rest[hash..].to_owned());
        rest.truncate(hash);
    }
    if let Some(query) = rest.find('?') {
        out.search = Some(rest[query..].to_owned());
        rest.truncate(query);
    }
    if !rest.is_empty() {
        out.pathname = Some(rest);
    }
    out
}

fn format_url(url: &Url) -> String {
    let mut out = String::new();
    out += url.protocol.as_deref().unwrap_or("");
    if url.slashes {
        out += "//";
    }
    if let Some(auth) = url.auth.as_deref().filter(|auth| !auth.is_empty()) {
        out += auth;
        out.push('@');
    }
    match url.hostname.as_deref() {
        Some(host) if host.contains(':') => out += &format!("[{host}]"),
        Some(host) => out += host,
        None => {}
    }
    if let Some(port) = url.port.as_deref().filter(|port| !port.is_empty()) {
        out.push(':');
        out += port;
    }
    for part in [&url.pathname, &url.search, &url.hash] {
        out += part.as_deref().unwrap_or("");
    }
    out
}

/// `String.prototype.trim`.
fn js_trim(text: &str) -> &str {
    text.trim_matches(crate::registry::js_space)
}

fn recode_host(url: &Url) -> bool {
    url.hostname.as_deref().is_some_and(|host| !host.is_empty())
        && url
            .protocol
            .as_deref()
            .is_none_or(|protocol| ["http:", "https:", "mailto:"].contains(&protocol))
}

fn map_domain(domain: &str, label: impl Fn(&str) -> String) -> String {
    let (local, domain) = match domain.split_once('@') {
        Some((local, domain)) => (format!("{local}@"), domain),
        None => (String::new(), domain),
    };
    let domain = domain.replace(['\u{3002}', '\u{ff0e}', '\u{ff61}'], ".");
    local + &domain.split('.').map(label).collect::<Vec<_>>().join(".")
}

/// `punycode.toASCII`.
fn to_ascii(domain: &str) -> String {
    map_domain(domain, |label| {
        if label.is_ascii() {
            label.to_owned()
        } else {
            idna::punycode::encode_str(label)
                .map_or_else(|| label.to_owned(), |code| format!("xn--{code}"))
        }
    })
}

/// `punycode.toUnicode`.
fn to_unicode(domain: &str) -> String {
    map_domain(domain, |label| match label.strip_prefix("xn--") {
        Some(code) => idna::punycode::decode_to_string(&code.to_lowercase())
            .unwrap_or_else(|| label.to_owned()),
        None => label.to_owned(),
    })
}

/// `mdurl.encode(string)` with its default characters, keeping `%XX` escapes.
fn encode(text: &str) -> String {
    const KEEP: &str = ";/?:@&=+$,-_.!~*'()#";
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'%'
            && i + 3 <= bytes.len()
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            out += &text[i..i + 3];
            i += 3;
            continue;
        }
        if byte < 128 {
            if byte.is_ascii_alphanumeric() || KEEP.as_bytes().contains(&byte) {
                out.push(byte as char);
            } else {
                out += &format!("%{byte:02X}");
            }
            i += 1;
            continue;
        }
        let len = text[i..].chars().next().map_or(1, char::len_utf8);
        for byte in &bytes[i..i + len] {
            out += &format!("%{byte:02X}");
        }
        i += len;
    }
    out
}

/// `mdurl.decode(string, ";/?:@&=+$,#%")`: decodes UTF-8 escapes, except the reserved ASCII ones.
fn decode(text: &str) -> String {
    const EXCLUDE: &[u8] = b";/?:@&=+$,#%";
    let bytes = text.as_bytes();
    let hex = |at: usize| -> Option<u32> {
        (at + 3 <= bytes.len()
            && bytes[at] == b'%'
            && bytes[at + 1].is_ascii_hexdigit()
            && bytes[at + 2].is_ascii_hexdigit())
        .then(|| u32::from_str_radix(&text[at + 1..at + 3], 16).unwrap_or(0))
    };
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        // A run of escapes, decoded as mdurl decodes one match of /(%[a-f0-9]{2})+/.
        let mut run = Vec::new();
        let mut j = i;
        while let Some(value) = hex(j) {
            run.push(value);
            j += 3;
        }
        if run.is_empty() {
            let len = text[i..].chars().next().map_or(1, char::len_utf8);
            out += &text[i..i + len];
            i += len;
            continue;
        }
        let mut k = 0;
        while k < run.len() {
            let b1 = run[k];
            let next = |n: usize| run.get(k + n).copied().filter(|b| b & 0xC0 == 0x80);
            if b1 < 0x80 {
                if EXCLUDE.contains(&(b1 as u8)) {
                    out += &format!("%{b1:02X}");
                } else {
                    out.push(b1 as u8 as char);
                }
                k += 1;
            } else if b1 & 0xE0 == 0xC0 && k + 1 < run.len() && next(1).is_some() {
                let chr = ((b1 << 6) & 0x7C0) | (run[k + 1] & 0x3F);
                if chr < 0x80 {
                    out += "\u{fffd}\u{fffd}";
                } else {
                    out.push(char::from_u32(chr).unwrap_or('\u{fffd}'));
                }
                k += 2;
            } else if b1 & 0xF0 == 0xE0
                && k + 2 < run.len()
                && next(1).is_some()
                && next(2).is_some()
            {
                let chr = ((b1 << 12) & 0xF000) | ((run[k + 1] << 6) & 0xFC0) | (run[k + 2] & 0x3F);
                if chr < 0x800 || (0xD800..=0xDFFF).contains(&chr) {
                    out += "\u{fffd}\u{fffd}\u{fffd}";
                } else {
                    out.push(char::from_u32(chr).unwrap_or('\u{fffd}'));
                }
                k += 3;
            } else if b1 & 0xF8 == 0xF0
                && k + 3 < run.len()
                && next(1).is_some()
                && next(2).is_some()
                && next(3).is_some()
            {
                let chr = ((b1 << 18) & 0x1C_0000)
                    | ((run[k + 1] << 12) & 0x3F000)
                    | ((run[k + 2] << 6) & 0xFC0)
                    | (run[k + 3] & 0x3F);
                if !(0x10000..=0x10FFFF).contains(&chr) {
                    out += "\u{fffd}\u{fffd}\u{fffd}\u{fffd}";
                } else {
                    out.push(char::from_u32(chr).unwrap_or('\u{fffd}'));
                }
                k += 4;
            } else {
                out.push('\u{fffd}');
                k += 1;
            }
        }
        i = j;
    }
    out
}

/// `md.normalizeLink`.
fn normalize_link(url: &str) -> String {
    let mut parsed = parse_url(url);
    if recode_host(&parsed) {
        parsed.hostname = parsed.hostname.as_deref().map(to_ascii);
    }
    for part in [
        &mut parsed.auth,
        &mut parsed.hostname,
        &mut parsed.pathname,
        &mut parsed.search,
        &mut parsed.hash,
    ] {
        if let Some(value) = part.as_mut().filter(|value| !value.is_empty()) {
            *value = encode(value);
        }
    }
    format_url(&parsed)
}

/// `md.normalizeLinkText`.
fn normalize_link_text(url: &str) -> String {
    let mut parsed = parse_url(url);
    if recode_host(&parsed) {
        parsed.hostname = parsed.hostname.as_deref().map(to_unicode);
    }
    decode(&format_url(&parsed))
}

/// An open link or image: how its end is written.
enum Open {
    /// `<a ...>`, closed with `</a>`.
    Anchor,
    /// A link markdown-it does not accept, written as its source text. `inner_end` is where its
    /// text ended so far.
    Rejected { end: usize, inner_end: usize },
}

/// `new MarkdownIt({ html: false }).render(markdown)`.
pub fn render(markdown: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_SUBSCRIPT;
    let mut out = String::new();
    let mut aligns: Vec<Alignment> = Vec::new();
    let mut cell = 0;
    let mut in_head = false;
    let mut body = false;
    let mut links: Vec<Open> = Vec::new();
    // An image collects its alternative text until it ends.
    let mut image: Option<(String, String, String, usize)> = None;
    // The events inside an autolink or an image already written as source text, with the depth of
    // nested links or images.
    let mut skip: Option<(bool, usize)> = None;
    let mut html_block: Option<String> = None;
    // markdown-it ends the text of an indented code block with a line break.
    let mut indented = false;
    let block = |out: &mut String| {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
    };
    for (event, range) in Parser::new_ext(markdown, options).into_offset_iter() {
        if let Some((is_image, depth)) = skip.as_mut() {
            match (&event, *is_image) {
                (Event::Start(Tag::Link { .. }), false)
                | (Event::Start(Tag::Image { .. }), true) => *depth += 1,
                (Event::End(TagEnd::Link), false) | (Event::End(TagEnd::Image), true) => {
                    if *depth == 0 {
                        skip = None;
                    } else {
                        *depth -= 1;
                    }
                }
                _ => {}
            }
            continue;
        }
        if let Some(Open::Rejected { inner_end, .. }) = links.last_mut()
            && !matches!(event, Event::End(TagEnd::Link))
        {
            *inner_end = (*inner_end).max(range.end);
        }
        if let Some((alt, _, _, depth)) = image.as_mut() {
            match event {
                Event::Start(Tag::Image { .. }) => *depth += 1,
                Event::End(TagEnd::Image) if *depth > 0 => *depth -= 1,
                Event::End(TagEnd::Image) => {
                    let (alt, src, title, _) = image.take().unwrap_or_default();
                    out += &format!(
                        "<img src=\"{}\" alt=\"{}\"",
                        escape_html(&src),
                        escape_html(&alt)
                    );
                    if !title.is_empty() {
                        out += &format!(" title=\"{}\"", escape_html(&title));
                    }
                    out.push('>');
                }
                Event::Text(text)
                | Event::Code(text)
                | Event::InlineHtml(text)
                | Event::Html(text) => alt.push_str(&text),
                Event::SoftBreak | Event::HardBreak => alt.push('\n'),
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    block(&mut out);
                    out += "<p>";
                }
                Tag::Heading { level, .. } => {
                    block(&mut out);
                    out += &format!("<{level}>");
                }
                Tag::BlockQuote(_) => {
                    block(&mut out);
                    out += "<blockquote>\n";
                }
                Tag::CodeBlock(kind) => {
                    block(&mut out);
                    out += "<pre><code";
                    indented = matches!(kind, CodeBlockKind::Indented);
                    if let CodeBlockKind::Fenced(info) = kind {
                        let info = js_trim(&info);
                        if !info.is_empty() {
                            let lang = info.split(char::is_whitespace).next().unwrap_or("");
                            out += &format!(" class=\"language-{}\"", escape_html(lang));
                        }
                    }
                    out.push('>');
                }
                Tag::HtmlBlock => {
                    block(&mut out);
                    html_block = Some(String::new());
                }
                Tag::List(start) => {
                    block(&mut out);
                    out += &match start {
                        None => "<ul>\n".to_owned(),
                        Some(1) => "<ol>\n".to_owned(),
                        Some(start) => format!("<ol start=\"{start}\">\n"),
                    };
                }
                Tag::Item => {
                    block(&mut out);
                    out += "<li>";
                }
                Tag::Table(columns) => {
                    block(&mut out);
                    aligns = columns;
                    body = false;
                    out += "<table>\n";
                }
                Tag::TableHead => {
                    in_head = true;
                    cell = 0;
                    out += "<thead>\n<tr>\n";
                }
                Tag::TableRow => {
                    if !body {
                        body = true;
                        out += "<tbody>\n";
                    }
                    cell = 0;
                    out += "<tr>\n";
                }
                Tag::TableCell => {
                    let name = if in_head { "th" } else { "td" };
                    let align = match aligns.get(cell) {
                        Some(Alignment::Left) => " style=\"text-align:left\"",
                        Some(Alignment::Center) => " style=\"text-align:center\"",
                        Some(Alignment::Right) => " style=\"text-align:right\"",
                        _ => "",
                    };
                    out += &format!("<{name}{align}>");
                }
                Tag::Emphasis => out += "<em>",
                Tag::Strong => out += "<strong>",
                Tag::Strikethrough => out += "<s>",
                Tag::Subscript | Tag::Superscript => out.push('~'),
                Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    ..
                } => {
                    let email = link_type == LinkType::Email;
                    let href = normalize_link(&if email {
                        format!("mailto:{dest_url}")
                    } else {
                        dest_url.to_string()
                    });
                    let autolink = matches!(link_type, LinkType::Autolink | LinkType::Email);
                    if !valid_link(&href) {
                        if autolink {
                            out += &escape_html(&markdown[range.clone()]);
                            skip = Some((false, 0));
                        } else {
                            out.push('[');
                            links.push(Open::Rejected {
                                end: range.end,
                                inner_end: range.start + 1,
                            });
                        }
                        continue;
                    }
                    out += &format!("<a href=\"{}\"", escape_html(&href));
                    if !title.is_empty() {
                        out += &format!(" title=\"{}\"", escape_html(&title));
                    }
                    out.push('>');
                    if autolink {
                        let label = normalize_link_text(&dest_url);
                        out += &escape_html(&label);
                        out += "</a>";
                        skip = Some((false, 0));
                        continue;
                    }
                    links.push(Open::Anchor);
                }
                Tag::Image {
                    dest_url, title, ..
                } => {
                    let src = normalize_link(&dest_url);
                    if !valid_link(&src) {
                        out += &escape_html(&markdown[range.clone()]);
                        skip = Some((true, 0));
                        continue;
                    }
                    image = Some((String::new(), src, title.to_string(), 0));
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => out += "</p>\n",
                TagEnd::Heading(level) => out += &format!("</{level}>\n"),
                TagEnd::BlockQuote(_) => {
                    // An empty quote closes on the same line, as markdown-it writes it.
                    if out.ends_with("<blockquote>\n") {
                        out.pop();
                    } else {
                        block(&mut out);
                    }
                    out += "</blockquote>\n";
                }
                TagEnd::CodeBlock => {
                    if indented && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    indented = false;
                    out += "</code></pre>\n";
                }
                TagEnd::HtmlBlock => {
                    let text = html_block.take().unwrap_or_default();
                    out += &format!("<p>{}</p>\n", escape_html(js_trim(&text)));
                }
                TagEnd::List(ordered) => {
                    block(&mut out);
                    out += if ordered { "</ol>\n" } else { "</ul>\n" };
                }
                TagEnd::Item => out += "</li>\n",
                TagEnd::Table => {
                    if body {
                        out += "</tbody>\n";
                    }
                    out += "</table>\n";
                }
                TagEnd::TableHead => {
                    in_head = false;
                    out += "</tr>\n</thead>\n";
                }
                TagEnd::TableRow => out += "</tr>\n",
                TagEnd::TableCell => {
                    out += if in_head { "</th>\n" } else { "</td>\n" };
                    cell += 1;
                }
                TagEnd::Emphasis => out += "</em>",
                TagEnd::Strong => out += "</strong>",
                TagEnd::Strikethrough => out += "</s>",
                TagEnd::Subscript | TagEnd::Superscript => out.push('~'),
                TagEnd::Link => match links.pop() {
                    Some(Open::Rejected { end, inner_end }) => {
                        out += &escape_html(&markdown[inner_end.min(end)..end]);
                    }
                    _ => out += "</a>",
                },
                _ => {}
            },
            Event::Text(text) => match html_block.as_mut() {
                Some(block) => block.push_str(&text),
                None => out += &escape_html(&text),
            },
            Event::Code(text) => out += &format!("<code>{}</code>", escape_html(&text)),
            Event::Html(text) | Event::InlineHtml(text) => match html_block.as_mut() {
                Some(block) => block.push_str(&text),
                None => out += &escape_html(&text),
            },
            Event::SoftBreak => out.push('\n'),
            Event::HardBreak => out += "<br>\n",
            Event::Rule => {
                block(&mut out);
                out += "<hr>\n";
            }
            Event::FootnoteReference(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                out += &escape_html(&text)
            }
            Event::TaskListMarker(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_like_markdown_it() {
        assert_eq!(
            render("# a\n\nb *c*\n"),
            "<h1>a</h1>\n<p>b <em>c</em></p>\n"
        );
        assert_eq!(
            render("- a\n- b\n"),
            "<ul>\n<li>a</li>\n<li>b</li>\n</ul>\n"
        );
        assert_eq!(render("x<script>y\n"), "<p>x&lt;script&gt;y</p>\n");
        assert_eq!(render("~a~ ~~b~~"), "<p>~a~ <s>b</s></p>\n");
        assert_eq!(render("---\n"), "<hr>\n");
    }

    #[test]
    fn encodes_links_like_markdown_it() {
        assert_eq!(
            normalize_link("http://例え.jp/a b"),
            "http://xn--r8jz45g.jp/a%20b"
        );
        assert_eq!(normalize_link("a%20b[c]"), "a%20b%5Bc%5D");
        assert!(!valid_link("JavaScript:alert(1)"));
        assert!(valid_link("data:image/png;base64,AA"));
        assert_eq!(
            normalize_link_text("http://a/%E3%81%82%2F"),
            "http://a/あ%2F"
        );
    }
}
