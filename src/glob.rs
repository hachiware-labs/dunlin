//! Glob matching as Node.js `path.matchesGlob` does it.
//!
//! Node uses minimatch with `nocase` on Windows and macOS, `nocaseMagicOnly`, `windowsPathsNoEscape`,
//! `nonegate` and `nocomment`. That means: backslashes are separators, `!` and `#` are literal,
//! wildcards skip names that start with a dot, and only segments with wildcards ignore case.

/// Whether `path` (with `/` separators) matches `pattern`.
pub fn matches(path: &str, pattern: &str) -> bool {
    let nocase = cfg!(any(windows, target_os = "macos"));
    let pattern = pattern.replace('\\', "/");
    if pattern.is_empty() {
        return path.is_empty();
    }
    // On Windows, minimatch splits paths at backslashes too.
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    let file = optimize(path.split('/').collect());
    expand_braces(&pattern).iter().any(|expanded| {
        let mut collapsed = expanded.clone();
        while collapsed.contains("//") {
            collapsed = collapsed.replace("//", "/");
        }
        let segments: Vec<Segment> = collapsed
            .split('/')
            .map(|part| Segment::parse(part, nocase))
            .collect();
        match_segments(&file, &segments)
    })
}

enum Segment {
    Globstar,
    Literal(String),
    Magic {
        tokens: Vec<Token>,
        nocase: bool,
        dot: bool,
    },
}

impl Segment {
    fn parse(text: &str, nocase: bool) -> Self {
        if text == "**" {
            return Self::Globstar;
        }
        let tokens = tokenize(&text.chars().collect::<Vec<_>>());
        if tokens.iter().all(|token| matches!(token, Token::Char(_))) {
            return Self::Literal(text.to_owned());
        }
        // Names that start with a dot need a literal dot, unless the pattern starts with an extended
        // glob that can match nothing (`@(...)`, `?(...)`, `*(...)`, `+(...)`).
        let open = matches!(
            tokens.first(),
            Some(Token::Ext {
                kind: '@' | '?' | '*' | '+',
                ..
            })
        );
        Self::Magic {
            dot: open || text.starts_with('.'),
            tokens,
            nocase,
        }
    }

    fn matches(&self, name: &str) -> bool {
        match self {
            Self::Globstar => true,
            Self::Literal(text) => text == name,
            Self::Magic {
                tokens,
                nocase,
                dot,
            } => {
                // Wildcards skip names that start with a dot, and never match `.` or `..`.
                // Only a leading `!(...)` can match an empty name.
                let negated = matches!(tokens.first(), Some(Token::Ext { kind: '!', .. }));
                if (name.is_empty() && !negated)
                    || name == "."
                    || name == ".."
                    || (name.starts_with('.') && !dot)
                {
                    return false;
                }
                let chars: Vec<char> = name.chars().collect();
                match_tokens(tokens, &chars, 0, *nocase, &|position| {
                    position == chars.len()
                })
            }
        }
    }
}

/// minimatch's level two file optimization: drops inner `.` and empty segments, and resolves
/// `name/..`. The first and last segments stay.
fn optimize(mut parts: Vec<&str>) -> Vec<&str> {
    loop {
        let mut changed = false;
        let mut i = 1;
        while i + 1 < parts.len() {
            if parts[i] == "." || parts[i].is_empty() {
                parts.remove(i);
                changed = true;
            } else {
                i += 1;
            }
        }
        if parts.len() == 2 && parts[0] == "." && (parts[1] == "." || parts[1].is_empty()) {
            parts.pop();
            changed = true;
        }
        let mut index = 1;
        while index < parts.len() {
            let before = parts[index - 1];
            if parts[index] == ".."
                && !before.is_empty()
                && before != "."
                && before != ".."
                && before != "**"
            {
                parts.drain(index - 1..=index);
                changed = true;
                index = index.saturating_sub(1).max(1);
            } else {
                index += 1;
            }
        }
        if !changed {
            break;
        }
    }
    if parts.is_empty() { vec![""] } else { parts }
}

fn match_segments(file: &[&str], pattern: &[Segment]) -> bool {
    match (pattern.first(), file.first()) {
        (None, None) => true,
        // A trailing slash on the path is ignored.
        (None, Some(&"")) => file.len() == 1,
        (None, Some(_)) => false,
        (Some(Segment::Globstar), _) => {
            if pattern.len() == 1 {
                // `**` at the end needs at least one segment, none of them hidden.
                return !file.is_empty()
                    && file
                        .iter()
                        .all(|name| !name.starts_with('.') || name.is_empty());
            }
            for skip in 0..=file.len() {
                if match_segments(&file[skip..], &pattern[1..]) {
                    return true;
                }
                if skip < file.len() && (file[skip].starts_with('.') || file[skip].is_empty()) {
                    return false;
                }
            }
            false
        }
        (Some(_), None) => false,
        (Some(segment), Some(name)) => {
            segment.matches(name) && match_segments(&file[1..], &pattern[1..])
        }
    }
}

#[derive(Debug)]
enum Token {
    Char(char),
    Any,
    One,
    Class {
        negated: bool,
        items: Vec<ClassItem>,
    },
    Ext {
        kind: char,
        alternatives: Vec<Vec<Token>>,
    },
}

#[derive(Debug)]
enum ClassItem {
    Char(char),
    Range(char, char),
    Posix(String),
}

fn tokenize(chars: &[char]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if "?*+@!".contains(c)
            && chars.get(i + 1) == Some(&'(')
            && let Some((alternatives, end)) = extglob(chars, i + 2)
        {
            tokens.push(Token::Ext {
                kind: c,
                alternatives,
            });
            i = end + 1;
            continue;
        }
        match c {
            '*' => {
                if !matches!(tokens.last(), Some(Token::Any)) {
                    tokens.push(Token::Any);
                }
                i += 1;
            }
            '?' => {
                tokens.push(Token::One);
                i += 1;
            }
            '[' => match class(chars, i + 1) {
                Some((token, end)) => {
                    tokens.push(token);
                    i = end + 1;
                }
                None => {
                    tokens.push(Token::Char('['));
                    i += 1;
                }
            },
            _ => {
                tokens.push(Token::Char(c));
                i += 1;
            }
        }
    }
    tokens
}

/// Alternatives of an extended glob that starts at `start`, and the index of its `)`.
fn extglob(chars: &[char], start: usize) -> Option<(Vec<Vec<Token>>, usize)> {
    let mut depth = 0;
    let mut parts = Vec::new();
    let mut from = start;
    for (offset, c) in chars[start..].iter().enumerate() {
        let i = start + offset;
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => {
                parts.push(tokenize(&chars[from..i]));
                return Some((parts, i));
            }
            ')' => depth -= 1,
            '|' if depth == 0 => {
                parts.push(tokenize(&chars[from..i]));
                from = i + 1;
            }
            _ => {}
        }
    }
    None
}

/// A character class that starts after `[`, and the index of its `]`.
fn class(chars: &[char], start: usize) -> Option<(Token, usize)> {
    let mut i = start;
    let negated = matches!(chars.get(i), Some('!' | '^'));
    if negated {
        i += 1;
    }
    let mut items = Vec::new();
    let first = i;
    while i < chars.len() {
        let c = chars[i];
        if c == ']' && i > first {
            return Some((Token::Class { negated, items }, i));
        }
        if c == '['
            && chars.get(i + 1) == Some(&':')
            && let Some(end) = chars[i + 2..]
                .windows(2)
                .position(|pair| pair == [':', ']'])
        {
            items.push(ClassItem::Posix(chars[i + 2..i + 2 + end].iter().collect()));
            i += end + 4;
            continue;
        }
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|end| *end != ']') {
            items.push(ClassItem::Range(c, chars[i + 2]));
            i += 3;
            continue;
        }
        items.push(ClassItem::Char(c));
        i += 1;
    }
    None
}

fn same(a: char, b: char, nocase: bool) -> bool {
    a == b || (nocase && a.to_lowercase().eq(b.to_lowercase()))
}

fn in_class(items: &[ClassItem], c: char, nocase: bool) -> bool {
    let variants: Vec<char> = if nocase {
        c.to_lowercase()
            .chain(c.to_uppercase())
            .chain([c])
            .collect()
    } else {
        vec![c]
    };
    variants.iter().any(|&c| {
        items.iter().any(|item| match item {
            ClassItem::Char(item) => *item == c,
            ClassItem::Range(low, high) => *low <= c && c <= *high,
            ClassItem::Posix(name) => match name.as_str() {
                "alnum" => c.is_alphanumeric(),
                "alpha" => c.is_alphabetic(),
                "ascii" => c.is_ascii(),
                "blank" => c == ' ' || c == '\t',
                "cntrl" => c.is_control(),
                "digit" => c.is_ascii_digit(),
                "graph" => !c.is_control() && !c.is_whitespace(),
                "lower" => c.is_lowercase(),
                "print" => !c.is_control(),
                "punct" => c.is_ascii_punctuation(),
                "space" => c.is_whitespace(),
                "upper" => c.is_uppercase(),
                "word" => c.is_alphanumeric() || c == '_',
                "xdigit" => c.is_ascii_hexdigit(),
                _ => false,
            },
        })
    })
}

/// Matches one alternative from a position and calls the continuation with where it ended.
type Once<'a> = dyn Fn(usize, &dyn Fn(usize) -> bool) -> bool + 'a;

/// Matches `tokens` against `name` from `position`, then calls `rest` with where it ended.
fn match_tokens(
    tokens: &[Token],
    name: &[char],
    position: usize,
    nocase: bool,
    rest: &dyn Fn(usize) -> bool,
) -> bool {
    let Some((token, tail)) = tokens.split_first() else {
        return rest(position);
    };
    let next = |position: usize| match_tokens(tail, name, position, nocase, rest);
    match token {
        Token::Char(c) => {
            name.get(position)
                .is_some_and(|actual| same(*actual, *c, nocase))
                && next(position + 1)
        }
        Token::One => position < name.len() && next(position + 1),
        Token::Any => (position..=name.len()).any(next),
        Token::Class { negated, items } => {
            name.get(position)
                .is_some_and(|actual| in_class(items, *actual, nocase) != *negated)
                && next(position + 1)
        }
        Token::Ext { kind, alternatives } => {
            let once = |from: usize, then: &dyn Fn(usize) -> bool| {
                alternatives
                    .iter()
                    .any(|alternative| match_tokens(alternative, name, from, nocase, then))
            };
            match kind {
                '@' => once(position, &next),
                '?' => next(position) || once(position, &next),
                '*' | '+' => {
                    fn repeat(
                        from: usize,
                        count: usize,
                        minimum: usize,
                        once: &Once<'_>,
                        next: &dyn Fn(usize) -> bool,
                    ) -> bool {
                        (count >= minimum && next(from))
                            || once(from, &|end| {
                                end > from && repeat(end, count + 1, minimum, once, next)
                            })
                    }
                    repeat(position, 0, usize::from(*kind == '+'), &once, &next)
                }
                // `!(...)`: any text that none of the alternatives matches in full.
                _ => (position..=name.len()).any(|end| {
                    !alternatives.iter().any(|alternative| {
                        match_tokens(alternative, &name[..end], position, nocase, &|finish| {
                            finish == end
                        })
                    }) && next(end)
                }),
            }
        }
    }
}

/// Bash-style brace expansion: `{a,b}`, `{1..3}`, `{01..10..2}` and `{a..e}`.
pub fn expand_braces(pattern: &str) -> Vec<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut search = 0;
    while let Some(offset) = chars[search..].iter().position(|c| *c == '{') {
        let open = search + offset;
        let mut depth = 0;
        let mut close = None;
        let mut commas = Vec::new();
        for (i, c) in chars.iter().enumerate().skip(open) {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i);
                        break;
                    }
                }
                ',' if depth == 1 => commas.push(i),
                _ => {}
            }
        }
        let Some(close) = close else { break };
        let prefix: String = chars[..open].iter().collect();
        let suffix: String = chars[close + 1..].iter().collect();
        let body: String = chars[open + 1..close].iter().collect();
        let options: Vec<String> = if !commas.is_empty() {
            let mut parts = Vec::new();
            let mut from = open + 1;
            for comma in commas.iter().chain([&close]) {
                parts.push(chars[from..*comma].iter().collect::<String>());
                from = comma + 1;
            }
            parts.iter().flat_map(|part| expand_braces(part)).collect()
        } else if let Some(sequence) = sequence(&body) {
            sequence
        } else {
            search = open + 1;
            continue;
        };
        return options
            .iter()
            .flat_map(|option| expand_braces(&format!("{prefix}{option}{suffix}")))
            .collect();
    }
    vec![pattern.to_owned()]
}

fn sequence(body: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = body.split("..").collect();
    if parts.len() != 2 && parts.len() != 3 {
        return None;
    }
    let step = match parts.get(2) {
        Some(step) => step.parse::<i64>().ok()?.unsigned_abs().max(1),
        None => 1,
    };
    if let (Ok(start), Ok(end)) = (parts[0].parse::<i64>(), parts[1].parse::<i64>()) {
        let padded = [parts[0], parts[1]].iter().any(|part| {
            let digits = part.trim_start_matches('-');
            digits.len() > 1 && digits.starts_with('0')
        });
        let width = if padded {
            parts[0].len().max(parts[1].len())
        } else {
            0
        };
        let mut values = Vec::new();
        let mut value = start;
        loop {
            let text = if value < 0 {
                format!("-{:0width$}", -value, width = width.saturating_sub(1))
            } else {
                format!("{value:0width$}")
            };
            values.push(text);
            if value == end {
                break;
            }
            let next = if start <= end {
                value + step as i64
            } else {
                value - step as i64
            };
            if (start <= end && next > end) || (start > end && next < end) {
                break;
            }
            value = next;
        }
        return Some(values);
    }
    let (Some(start), Some(end)) = (single(parts[0]), single(parts[1])) else {
        return None;
    };
    let (low, high) = (start as u32, end as u32);
    let values: Vec<u32> = if low <= high {
        (low..=high).step_by(step as usize).collect()
    } else {
        (high..=low).rev().step_by(step as usize).collect()
    };
    Some(
        values
            .into_iter()
            .filter_map(char::from_u32)
            .map(String::from)
            .collect(),
    )
}

fn single(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_like_node() {
        for (path, pattern, expected) in [
            ("a/B.MD", "**/*.md", cfg!(any(windows, target_os = "macos"))),
            ("Docs/a.md", "docs/*.md", false),
            (".hidden/a.md", "**/*.md", false),
            ("a/.b.md", "**/*.md", false),
            ("a/.b.md", "**/.*.md", true),
            ("x/y/z.md", "x/**", true),
            ("x", "x/**", false),
            ("a.md", "{a,b}.md", true),
            ("a1.md", "a{1..3}.md", true),
            ("ab.md", "a[bc].md", true),
            ("a.md", "!(b).md", true),
            ("b.md", "!(b).md", false),
            ("ab.md", "+(a|b).md", true),
            ("a/b/", "a/b", true),
            ("../x", "**/x", false),
            ("drawings/x/pump.vsdx", "drawings/**/*.vsdx", true),
            ("pump.vsdx", "*.vsdx", true),
        ] {
            assert_eq!(matches(path, pattern), expected, "{path} {pattern}");
        }
        assert_eq!(
            expand_braces("a{1..3}b{x,y}"),
            ["a1bx", "a1by", "a2bx", "a2by", "a3bx", "a3by"]
        );
        assert_eq!(expand_braces("{01..03}"), ["01", "02", "03"]);
        assert_eq!(expand_braces("{a}"), ["{a}"]);
    }
}
