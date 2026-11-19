//! YAML text as the `yaml` package's `stringify(value)` writes it (spec 28.13 step 6b), for the
//! configuration files Dunlin writes: block maps and sequences indented by two spaces, plain
//! scalars where YAML 1.2 reads them back as strings, double quotes otherwise, block scalars for
//! multiline text, and lines folded at 80 columns.
//!
//! Lengths and positions are counted in UTF-16 code units, as JavaScript counts them, so long
//! values fold at the same places.

use crate::config_value::{ConfigValue, js_number};

const LINE_WIDTH: usize = 80;
const MIN_CONTENT_WIDTH: usize = 20;
const DOUBLE_QUOTED_MIN_MULTI_LINE_LENGTH: usize = 40;

#[derive(Clone, Copy, PartialEq)]
enum Fold {
    Flow,
    Block,
    Quoted,
}

#[derive(Clone)]
struct Context {
    indent: String,
    implicit_key: bool,
    indent_at_start: Option<usize>,
    force_block_indent: bool,
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn string(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// `/^(%|---|\.\.\.)/m`
fn contains_document_marker(value: &str) -> bool {
    value
        .split('\n')
        .any(|line| line.starts_with('%') || line.starts_with("---") || line.starts_with("..."))
}

/// `foldFlowLines(text, indent, mode, { indentAtStart, lineWidth: 80, minContentWidth: 20 })`
fn fold(
    text: &str,
    indent: &str,
    mode: Fold,
    indent_at_start: Option<usize>,
    overflowed: &mut bool,
) -> String {
    let text = units(text);
    let indent_len = indent.encode_utf16().count();
    let end_step = (1 + MIN_CONTENT_WIDTH).max((1 + LINE_WIDTH).saturating_sub(indent_len));
    if text.len() <= end_step {
        return string(&text);
    }
    let mut folds: Vec<usize> = Vec::new();
    let mut escaped: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut end = LINE_WIDTH as isize - indent_len as isize;
    if let Some(start) = indent_at_start {
        if start as isize > LINE_WIDTH as isize - (2.max(MIN_CONTENT_WIDTH)) as isize {
            folds.push(0);
        } else {
            end = LINE_WIDTH as isize - start as isize;
        }
    }
    let at = |i: isize| -> Option<u16> {
        if i < 0 {
            None
        } else {
            text.get(i as usize).copied()
        }
    };
    let (space, newline, tab, backslash) = (b' ' as u16, b'\n' as u16, b'\t' as u16, b'\\' as u16);
    let mut split: Option<isize> = None;
    let mut prev: Option<u16> = None;
    let mut overflow = false;
    let mut i: isize = -1;
    let mut esc_start: isize = -1;
    let mut esc_end: isize = -1;
    if mode == Fold::Block {
        i = consume_more_indented(&text, i, indent_len);
        if i != -1 {
            end = i + end_step as isize;
        }
    }
    loop {
        i += 1;
        let Some(mut ch) = at(i) else { break };
        if mode == Fold::Quoted && ch == backslash {
            esc_start = i;
            match at(i + 1) {
                Some(c) if c == b'x' as u16 => i += 3,
                Some(c) if c == b'u' as u16 => i += 5,
                Some(c) if c == b'U' as u16 => i += 9,
                _ => i += 1,
            }
            esc_end = i;
        }
        if ch == newline {
            if mode == Fold::Block {
                i = consume_more_indented(&text, i, indent_len);
            }
            end = i + indent_len as isize + end_step as isize;
            split = None;
        } else {
            if ch == space && prev.is_some_and(|p| p != space && p != newline && p != tab) {
                let next = at(i + 1);
                if next.is_some_and(|n| n != space && n != newline && n != tab) {
                    split = Some(i);
                }
            }
            if i >= end {
                if let Some(at_split) = split.filter(|&s| s != 0) {
                    folds.push(at_split as usize);
                    end = at_split + end_step as isize;
                    split = None;
                } else if mode == Fold::Quoted {
                    while prev == Some(space) || prev == Some(tab) {
                        prev = Some(ch);
                        i += 1;
                        ch = at(i).unwrap_or(0);
                        overflow = true;
                    }
                    let j = if i > esc_end + 1 {
                        i - 2
                    } else {
                        esc_start - 1
                    };
                    if j >= 0 && escaped.contains(&(j as usize)) {
                        return string(&text);
                    }
                    let j = j.max(0) as usize;
                    folds.push(j);
                    escaped.insert(j);
                    end = j as isize + end_step as isize;
                    split = None;
                } else {
                    overflow = true;
                }
            }
        }
        prev = Some(ch);
    }
    if overflow {
        *overflowed = true;
    }
    if folds.is_empty() {
        return string(&text);
    }
    let mut out: Vec<u16> = text[..folds[0]].to_vec();
    let indent_units = units(indent);
    for (index, &at_fold) in folds.iter().enumerate() {
        let end = folds
            .get(index + 1)
            .copied()
            .filter(|&next| next != 0)
            .unwrap_or(text.len());
        if at_fold == 0 {
            out = vec![newline];
            out.extend(&indent_units);
            out.extend(&text[..end]);
        } else {
            if mode == Fold::Quoted && escaped.contains(&at_fold) {
                out.push(text[at_fold]);
                out.push(backslash);
            }
            out.push(newline);
            out.extend(&indent_units);
            out.extend(&text[at_fold + 1..end]);
        }
    }
    string(&out)
}

/// `consumeMoreIndentedLines(text, i, indent)`
fn consume_more_indented(text: &[u16], mut i: isize, indent: usize) -> isize {
    let at = |i: isize| -> Option<u16> { text.get(i as usize).copied().filter(|_| i >= 0) };
    let (space, tab, newline) = (b' ' as u16, b'\t' as u16, b'\n' as u16);
    let mut end = i;
    let mut start = i + 1;
    let mut ch = at(start);
    while ch == Some(space) || ch == Some(tab) {
        if i < start + indent as isize {
            i += 1;
            ch = at(i);
        } else {
            loop {
                i += 1;
                ch = at(i);
                if ch.is_none() || ch == Some(newline) {
                    break;
                }
            }
            end = i;
            start = i + 1;
            ch = at(start);
        }
    }
    end
}

/// `doubleQuotedString(value, ctx)`
fn double_quoted(value: &str, ctx: &Context) -> String {
    let json: Vec<char> = ConfigValue::String(value.to_owned())
        .to_js_json()
        .chars()
        .collect();
    let indent = if !ctx.indent.is_empty() {
        ctx.indent.clone()
    } else if contains_document_marker(value) {
        "  ".into()
    } else {
        String::new()
    };
    let json_units = json.iter().collect::<String>().encode_utf16().count();
    let mut out = String::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let slice = |from: usize, to: usize| {
        json[from.min(json.len())..to.min(json.len())]
            .iter()
            .collect::<String>()
    };
    while i < json.len() {
        let mut ch = json[i];
        if ch == ' ' && json.get(i + 1) == Some(&'\\') && json.get(i + 2) == Some(&'n') {
            out += &slice(start, i);
            out += "\\ ";
            i += 1;
            start = i;
            ch = '\\';
        }
        if ch == '\\' {
            match json.get(i + 1) {
                Some('u') => {
                    out += &slice(start, i);
                    let code = slice(i + 2, i + 6);
                    out += match code.as_str() {
                        "0000" => "\\0".to_owned(),
                        "0007" => "\\a".to_owned(),
                        "000b" => "\\v".to_owned(),
                        "001b" => "\\e".to_owned(),
                        "0085" => "\\N".to_owned(),
                        "00a0" => "\\_".to_owned(),
                        "2028" => "\\L".to_owned(),
                        "2029" => "\\P".to_owned(),
                        _ if code.starts_with("00") => format!("\\x{}", &code[2..]),
                        _ => slice(i, i + 6),
                    }
                    .as_str();
                    i += 5;
                    start = i + 1;
                }
                Some('n') => {
                    if ctx.implicit_key
                        || json.get(i + 2) == Some(&'"')
                        || json_units < DOUBLE_QUOTED_MIN_MULTI_LINE_LENGTH
                    {
                        i += 1;
                    } else {
                        out += &slice(start, i);
                        out += "\n\n";
                        while json.get(i + 2) == Some(&'\\')
                            && json.get(i + 3) == Some(&'n')
                            && json.get(i + 4) != Some(&'"')
                        {
                            out.push('\n');
                            i += 2;
                        }
                        out += &indent;
                        if json.get(i + 2) == Some(&' ') {
                            out.push('\\');
                        }
                        i += 1;
                        start = i + 1;
                    }
                }
                _ => i += 1,
            }
        }
        i += 1;
    }
    let text = if start > 0 {
        out + &slice(start, json.len())
    } else {
        json.iter().collect()
    };
    if ctx.implicit_key {
        text
    } else {
        fold(
            &text,
            &indent,
            Fold::Quoted,
            ctx.indent_at_start,
            &mut false,
        )
    }
}

/// `quotedString(value, ctx)` with the default `singleQuote: null`: single quotes only for text
/// with double quotes and no single quotes.
fn quoted(value: &str, ctx: &Context) -> String {
    if value.contains('"') && !value.contains('\'') {
        single_quoted(value, ctx)
    } else {
        double_quoted(value, ctx)
    }
}

/// `singleQuotedString(value, ctx)`
fn single_quoted(value: &str, ctx: &Context) -> String {
    let around_newline = value.contains(" \n")
        || value.contains("\t\n")
        || value.contains("\n ")
        || value.contains("\n\t");
    if (ctx.implicit_key && value.contains('\n')) || around_newline {
        return double_quoted(value, ctx);
    }
    let indent = if !ctx.indent.is_empty() {
        ctx.indent.clone()
    } else if contains_document_marker(value) {
        "  ".into()
    } else {
        String::new()
    };
    let escaped = value.replace('\'', "''");
    let mut body = String::new();
    let mut rest = escaped.as_str();
    while let Some(index) = rest.find('\n') {
        let run = rest[index..].len() - rest[index..].trim_start_matches('\n').len();
        body += &rest[..index + run];
        body.push('\n');
        body += &indent;
        rest = &rest[index + run..];
    }
    body += rest;
    let text = format!("'{body}'");
    if ctx.implicit_key {
        text
    } else {
        fold(&text, &indent, Fold::Flow, ctx.indent_at_start, &mut false)
    }
}

/// `lineLengthOverLimit(str, lineWidth, indentLength)`
fn line_length_over_limit(value: &[u16], indent: usize) -> bool {
    let limit = LINE_WIDTH as isize - indent as isize;
    let len = value.len() as isize;
    if len <= limit {
        return false;
    }
    let mut start: isize = 0;
    for (i, &unit) in value.iter().enumerate() {
        if unit == b'\n' as u16 {
            if i as isize - start > limit {
                return true;
            }
            start = i as isize + 1;
            if len - start <= limit {
                return false;
            }
        }
    }
    true
}

/// Each run of newlines followed by `indent`, as `.replace(/\n+/g, '$&' + indent)` writes it.
fn indent_newline_runs(text: &str, indent: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '\n' && chars.peek() != Some(&'\n') {
            out += indent;
        }
    }
    out
}

/// `blockString(item, ctx)` for a plain JavaScript string.
fn block_string(value: &str, ctx: &Context) -> String {
    // A block can't end in whitespace unless the last line is nonempty.
    let trailing_blank = value.rfind('\n').is_some_and(|at| {
        !value[at + 1..].is_empty() && value[at + 1..].chars().all(|c| c == ' ' || c == '\t')
    });
    if trailing_blank {
        return quoted(value, ctx);
    }
    let indent = if !ctx.indent.is_empty() {
        ctx.indent.clone()
    } else if ctx.force_block_indent || contains_document_marker(value) {
        "  ".into()
    } else {
        String::new()
    };
    let utf16 = units(value);
    let literal = !line_length_over_limit(&utf16, indent.encode_utf16().count());
    if value.is_empty() {
        return if literal { "|\n".into() } else { ">\n".into() };
    }
    let end_start = value.trim_end_matches(['\n', '\t', ' ']).len();
    let mut end = value[end_start..].to_owned();
    let chomp = match end.find('\n') {
        None => "-",
        Some(position) if end_start == 0 || position != end.len() - 1 => "+",
        Some(_) => "",
    };
    let mut body = value[..end_start].to_owned();
    if !end.is_empty() {
        if end.ends_with('\n') {
            end.pop();
        }
        // `(^|(?<!\n))\n+(?!\n|$)`: a run of newlines that is not at the end gets the indent.
        let mut replaced = String::new();
        let chars: Vec<char> = end.chars().collect();
        let mut k = 0;
        while k < chars.len() {
            if chars[k] == '\n' {
                let mut run_end = k;
                while run_end < chars.len() && chars[run_end] == '\n' {
                    run_end += 1;
                }
                replaced.extend(&chars[k..run_end]);
                if run_end < chars.len() {
                    replaced += &indent;
                }
                k = run_end;
            } else {
                replaced.push(chars[k]);
                k += 1;
            }
        }
        end = replaced;
    }
    let mut start_with_space = false;
    let mut start_nl: isize = -1;
    let mut start_end = 0usize;
    for (index, c) in body.char_indices() {
        start_end = index;
        match c {
            ' ' => start_with_space = true,
            '\n' => start_nl = index as isize,
            _ => break,
        }
        start_end = index + 1;
    }
    let start_len = if start_nl < start_end as isize {
        (start_nl + 1) as usize
    } else {
        start_end
    };
    let mut start = body[..start_len].to_owned();
    if !start.is_empty() {
        body = body[start_len..].to_owned();
        start = indent_newline_runs(&start, &indent);
    }
    let indent_size = if indent.is_empty() { "1" } else { "2" };
    let header = format!("{}{chomp}", if start_with_space { indent_size } else { "" });
    if !literal {
        let folded = folded_block_value(&body, &indent);
        let mut overflow = false;
        let folded_body = fold(
            &format!("{start}{folded}{end}"),
            &indent,
            Fold::Block,
            Some(ctx.indent.encode_utf16().count()),
            &mut overflow,
        );
        if !overflow {
            return format!(">{header}\n{indent}{folded_body}");
        }
    }
    let body = indent_newline_runs(&body, &indent);
    format!("|{header}\n{indent}{start}{body}{end}")
}

/// The value of a folded block before folding: a newline doubled, more-indented lines kept.
fn folded_block_value(value: &str, indent: &str) -> String {
    // `.replace(/\n+/g, '\n$&')`
    let mut doubled = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            let mut run = String::from("\n");
            while chars.peek() == Some(&'\n') {
                chars.next();
                run.push('\n');
            }
            doubled.push('\n');
            doubled += &run;
        } else {
            doubled.push(c);
        }
    }
    // `.replace(/(?:^|\n)([\t ].*)(?:([\n\t ]*)\n(?![\n\t ]))?/g, '$1$2')`: lines that start with
    // a space or tab are not folded.
    let regex = regex::Regex::new(r"(?:^|\n)([\t ][^\n]*)(?:([\n\t ]*)\n([^\n\t ]|$))?")
        .expect("valid pattern");
    let mut kept = String::new();
    let mut last = 0;
    let mut search = 0;
    while let Some(found) = regex.captures_at(&doubled, search) {
        let whole = found.get(0).expect("match");
        kept += &doubled[last..whole.start()];
        kept += found.get(1).map_or("", |m| m.as_str());
        kept += found.get(2).map_or("", |m| m.as_str());
        let lookahead = found.get(3).map_or(0, |m| m.len());
        last = whole.end() - lookahead;
        search = last.max(whole.start() + 1);
        if search > doubled.len() {
            break;
        }
    }
    kept += &doubled[last..];
    indent_newline_runs(&kept, indent)
}

/// Whether YAML 1.2 (the core schema) reads the plain text as something other than a string.
fn resolves_to_other_type(text: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let signed = |s: &str| s.strip_prefix(['-', '+']).unwrap_or(s).to_owned();
    // null
    if matches!(text, "" | "~" | "null" | "Null" | "NULL") {
        return true;
    }
    // bool
    if matches!(text, "true" | "True" | "TRUE" | "false" | "False" | "FALSE") {
        return true;
    }
    // int: octal, decimal, hexadecimal
    if let Some(rest) = text.strip_prefix("0o")
        && !rest.is_empty()
        && rest.bytes().all(|b| (b'0'..=b'7').contains(&b))
    {
        return true;
    }
    if digits(&signed(text)) {
        return true;
    }
    if let Some(rest) = text.strip_prefix("0x")
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return true;
    }
    // float: nan and infinities
    let body = signed(text);
    if matches!(body.as_str(), ".inf" | ".Inf" | ".INF") || matches!(text, ".nan" | ".NaN" | ".NAN")
    {
        return true;
    }
    // `^[-+]?(?:\.[0-9]+|[0-9]+(?:\.[0-9]*)?)[eE][-+]?[0-9]+$` and `^[-+]?(?:\.[0-9]+|[0-9]+\.[0-9]*)$`
    let mantissa = |s: &str| -> bool {
        if let Some(fraction) = s.strip_prefix('.') {
            return digits(fraction);
        }
        match s.split_once('.') {
            Some((whole, fraction)) => {
                digits(whole) && fraction.bytes().all(|b| b.is_ascii_digit())
            }
            None => digits(s),
        }
    };
    if let Some(at) = body.find(['e', 'E']) {
        let (m, e) = (&body[..at], &body[at + 1..]);
        if mantissa(m) && digits(e.strip_prefix(['-', '+']).unwrap_or(e)) {
            return true;
        }
    } else if body.contains('.') && mantissa(&body) {
        return true;
    }
    false
}

/// `plainString(item, ctx)` for a plain JavaScript string.
fn plain_string(value: &str, ctx: &Context) -> String {
    if ctx.implicit_key && value.contains('\n') {
        return quoted(value, ctx);
    }
    let first = value.chars().next();
    let last = value.chars().last();
    let bad_start = first.is_some_and(|c| "\n\t ,[]{}#&*!|>'\"%@`".contains(c))
        || value == "?"
        || value == "-"
        || value.starts_with("? ")
        || value.starts_with("?\t")
        || value.starts_with("- ")
        || value.starts_with("-\t");
    let inner = value.contains("\n ")
        || value.contains("\n\t")
        || value.contains(": ")
        || value.contains(":\t")
        || value.contains(" \n")
        || value.contains("\t\n")
        || value.contains("\n#")
        || value.contains("\t#")
        || value.contains(" #");
    let bad_end = last.is_some_and(|c| "\n\t :".contains(c));
    if bad_start || inner || bad_end {
        return if ctx.implicit_key || !value.contains('\n') {
            quoted(value, ctx)
        } else {
            block_string(value, ctx)
        };
    }
    if !ctx.implicit_key && value.contains('\n') {
        return block_string(value, ctx);
    }
    if contains_document_marker(value) {
        if ctx.indent.is_empty() {
            let mut forced = ctx.clone();
            forced.force_block_indent = true;
            return block_string(value, &forced);
        } else if ctx.implicit_key && ctx.indent == INDENT_STEP {
            return quoted(value, ctx);
        }
    }
    let text = indent_newline_runs_with_blank(value, &ctx.indent);
    if resolves_to_other_type(&text) {
        return quoted(value, ctx);
    }
    if ctx.implicit_key {
        text
    } else {
        fold(
            &text,
            &ctx.indent,
            Fold::Flow,
            ctx.indent_at_start,
            &mut false,
        )
    }
}

/// `.replace(/\n+/g, '$&\n' + indent)`
fn indent_newline_runs_with_blank(text: &str, indent: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '\n' && chars.peek() != Some(&'\n') {
            out.push('\n');
            out += indent;
        }
    }
    out
}

/// `stringifyString(item, ctx)` with the default plain type.
fn scalar_string(value: &str, ctx: &Context) -> String {
    let forced = value.chars().any(|c| {
        let code = c as u32;
        code <= 0x08 || (0x0b..=0x1f).contains(&code) || (0x7f..=0x9f).contains(&code)
    });
    if forced {
        double_quoted(value, ctx)
    } else {
        plain_string(value, ctx)
    }
}

fn scalar(value: &ConfigValue, ctx: &Context) -> String {
    match value {
        ConfigValue::Null => "null".into(),
        ConfigValue::Bool(true) => "true".into(),
        ConfigValue::Bool(false) => "false".into(),
        ConfigValue::Number(number) if *number == 0.0 && number.is_sign_negative() => "-0".into(),
        ConfigValue::Number(number) => js_number(*number),
        ConfigValue::NonFinite(name) => match name.as_str() {
            "NaN" => ".nan".into(),
            "-Infinity" => "-.inf".into(),
            _ => ".inf".into(),
        },
        ConfigValue::String(text) => scalar_string(text, ctx),
        ConfigValue::Array(_) | ConfigValue::Object(_) => collection(value, ctx),
    }
}

fn is_collection(value: &ConfigValue) -> bool {
    matches!(value, ConfigValue::Array(_) | ConfigValue::Object(_))
}

fn empty(value: &ConfigValue) -> bool {
    match value {
        ConfigValue::Array(items) => items.is_empty(),
        ConfigValue::Object(entries) => entries.is_empty(),
        _ => false,
    }
}

const INDENT_STEP: &str = "  ";

/// `stringifyPair({ key, value }, ctx)` in a block map.
fn pair(key: &str, value: &ConfigValue, ctx: &Context) -> String {
    let mut key_ctx = ctx.clone();
    key_ctx.implicit_key = true;
    key_ctx.indent = format!("{}{INDENT_STEP}", ctx.indent);
    let key_text = scalar_string(key, &key_ctx);
    let explicit = key_text.encode_utf16().count() > 1024;
    let mut text = if explicit {
        format!("? {key_text}\n{}:", ctx.indent)
    } else {
        format!("{key_text}:")
    };
    let mut value_ctx = key_ctx;
    value_ctx.implicit_key = false;
    value_ctx.indent_at_start = if !explicit && !is_collection(value) {
        Some(text.encode_utf16().count() + 1)
    } else {
        ctx.indent_at_start
    };
    let value_text = scalar(value, &value_ctx);
    let separator = if !explicit && is_collection(value) {
        if value_text.contains('\n') || !empty(value) {
            format!("\n{}", value_ctx.indent)
        } else {
            " ".into()
        }
    } else if value_text.is_empty() || value_text.starts_with('\n') {
        String::new()
    } else {
        " ".into()
    };
    text += &separator;
    text += &value_text;
    text
}

/// `stringifyCollection` in block style, or `{}` and `[]` when empty.
fn collection(value: &ConfigValue, ctx: &Context) -> String {
    let (lines, flow): (Vec<String>, &str) = match value {
        ConfigValue::Object(entries) => {
            let mut item_ctx = ctx.clone();
            item_ctx.indent = ctx.indent.clone();
            (
                entries
                    .iter()
                    .map(|(key, item)| pair(key, item, &item_ctx))
                    .collect(),
                "{}",
            )
        }
        ConfigValue::Array(items) => {
            let mut item_ctx = ctx.clone();
            item_ctx.indent = format!("{}{INDENT_STEP}", ctx.indent);
            (
                items
                    .iter()
                    .map(|item| format!("- {}", scalar(item, &item_ctx)))
                    .collect(),
                "[]",
            )
        }
        _ => return scalar(value, ctx),
    };
    if lines.is_empty() {
        return flow.into();
    }
    let mut out = lines[0].clone();
    for line in &lines[1..] {
        if line.is_empty() {
            out.push('\n');
        } else {
            out += &format!("\n{}{line}", ctx.indent);
        }
    }
    out
}

/// `stringify(value)` of the `yaml` package with its default options.
pub fn stringify(value: &ConfigValue) -> String {
    let ctx = Context {
        indent: String::new(),
        implicit_key: false,
        indent_at_start: None,
        force_block_indent: false,
    };
    format!("{}\n", scalar(value, &ctx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(json: &str) -> ConfigValue {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn writes_block_collections_like_the_yaml_package() {
        assert_eq!(
            stringify(&value(
                r#"{"init":{"version":1,"type":"wiki"},"about":{"title":"kb","out_of_scope":""},"list":[{"use":"a","with":{}}],"empty":[],"n":null,"t":true,"f":0.8}"#
            )),
            "init:\n  version: 1\n  type: wiki\nabout:\n  title: kb\n  out_of_scope: \"\"\nlist:\n  - use: a\n    with: {}\nempty: []\nn: null\nt: true\nf: 0.8\n"
        );
        assert_eq!(
            stringify(&value(
                r##"{"a":"true","b":"12","c":"a: b","d":"#x","e":"it's"}"##
            )),
            "a: \"true\"\nb: \"12\"\nc: \"a: b\"\nd: \"#x\"\ne: it's\n"
        );
    }
}
