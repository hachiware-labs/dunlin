//! Finding evidence quotes in documents and checking whether search results cover them (spec 9
//! and 10). Project evaluation (`eval_stage`) and the Node.js API's `evaluate` and `optimize`
//! use the same rules.
//!
//! Quotes match on NFKC text with collapsed white space, and ranges are byte offsets of the
//! document, widened to whole grapheme clusters. Positions, suggestions and their JSON are the
//! ones the JavaScript implementation produced.

use std::cmp::Ordering;
use std::collections::HashSet;

use icu_normalizer::ComposingNormalizerBorrowed;
use unicode_segmentation::UnicodeSegmentation;

use crate::SourceLocation;
use crate::config_value::ConfigValue;
use crate::registry::js_space;

fn object(entries: Vec<(&str, ConfigValue)>) -> ConfigValue {
    ConfigValue::Object(
        entries
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    )
}

fn text(value: &str) -> ConfigValue {
    ConfigValue::String(value.to_owned())
}

fn string_field<'a>(value: &'a ConfigValue, name: &str) -> Option<&'a str> {
    value.get(name).and_then(ConfigValue::as_str)
}

fn num(value: f64) -> ConfigValue {
    ConfigValue::Number(value)
}

pub(crate) fn nfkc(value: &str) -> String {
    ComposingNormalizerBorrowed::new_nfkc()
        .normalize(value)
        .into_owned()
}

pub(crate) fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// NFKC text with runs of white space as one space and no space at either end, and for each
/// byte of it the byte range of the grapheme cluster of the document it came from.
pub(crate) struct Normalized {
    pub(crate) value: String,
    pub(crate) map: Vec<(usize, usize)>,
}

pub(crate) fn normalized(document: &str) -> Normalized {
    let normalizer = ComposingNormalizerBorrowed::new_nfkc();
    let mut value = String::new();
    let mut map: Vec<(usize, usize)> = Vec::new();
    let mut offset = 0;
    for segment in document.graphemes(true) {
        let end = offset + segment.len();
        for character in normalizer.normalize(segment).chars() {
            if js_space(character) {
                if value.is_empty() || value.ends_with(' ') {
                    if let Some(last) = map.last_mut().filter(|_| !value.is_empty()) {
                        last.1 = end;
                    }
                    continue;
                }
                value.push(' ');
                map.push((offset, end));
            } else {
                value.push(character);
                for _ in 0..character.len_utf8() {
                    map.push((offset, end));
                }
            }
        }
        offset = end;
    }
    let kept = value.trim_end_matches(' ').len();
    value.truncate(kept);
    Normalized { value, map }
}

pub(crate) fn range_value(start: f64, end: f64) -> ConfigValue {
    object(vec![("start", num(start)), ("end", num(end))])
}

/// Finds the quote of an evidence item in a document: `{ range }`, or `{ reason, candidates }`
/// with suggestions when nothing matched.
pub(crate) fn position(
    document: &str,
    evidence: &ConfigValue,
    locations: &[SourceLocation],
) -> ConfigValue {
    let Normalized { value, map } = normalized(document);
    let query = normalized(string_field(evidence, "quote").unwrap_or_default()).value;
    let affix = |name: &str| {
        string_field(evidence, name)
            .filter(|text| !text.is_empty())
            .map(|text| normalized(text).value)
    };
    let prefix = affix("prefix");
    let suffix = affix("suffix");
    if query.is_empty() {
        return object(vec![("reason", text("empty_quote"))]);
    }
    let permitted = hinted_lines(document, evidence, locations);
    let newlines: Vec<usize> = if permitted.is_some() {
        document
            .bytes()
            .enumerate()
            .filter(|(_, byte)| *byte == b'\n')
            .map(|(index, _)| index)
            .collect()
    } else {
        Vec::new()
    };
    let line_of =
        |offset: usize| (newlines.partition_point(|&newline| newline < offset) + 1) as f64;
    let mut found: Vec<(usize, usize)> = Vec::new();
    let mut from = 0;
    while let Some(relative) = value[from..].find(&query) {
        let index = from + relative;
        from = index + value[index..].chars().next().map_or(1, char::len_utf8);
        if let Some(prefix) = &prefix
            && !value[..index]
                .trim_end_matches(' ')
                .ends_with(prefix.as_str())
        {
            continue;
        }
        if let Some(suffix) = &suffix
            && !value[index + query.len()..]
                .trim_start_matches(' ')
                .starts_with(suffix.as_str())
        {
            continue;
        }
        let range = (map[index].0, map[index + query.len() - 1].1);
        if let Some(permitted) = &permitted
            && !permitted.iter().any(|(start, end)| {
                *start <= line_of(range.0) && *end >= line_of(range.1.saturating_sub(1))
            })
        {
            continue;
        }
        found.push(range);
    }
    let ranges = |found: &[(usize, usize)]| {
        ConfigValue::Array(
            found
                .iter()
                .map(|(start, end)| range_value(*start as f64, *end as f64))
                .collect(),
        )
    };
    match found.len() {
        1 => object(vec![(
            "range",
            range_value(found[0].0 as f64, found[0].1 as f64),
        )]),
        0 => object(vec![
            ("reason", text("quote_not_found")),
            ("candidates", ConfigValue::Array(Vec::new())),
            ("suggestions", nearby_quotes(document, &query)),
        ]),
        _ => object(vec![
            ("reason", text("ambiguous_quote")),
            ("candidates", ranges(&found)),
        ]),
    }
}

/// A Markdown heading line, `^ {0,3}(#{1,6})\s+(.+?)\s*#*\s*$` in JavaScript: its depth and text.
pub(crate) fn heading_line(line: &str) -> Option<(usize, &str)> {
    let spaces = line.bytes().take_while(|byte| *byte == b' ').count();
    if spaces > 3 {
        return None;
    }
    let hashes = line[spaces..]
        .bytes()
        .take_while(|byte| *byte == b'#')
        .count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after = &line[spaces + hashes..];
    // The ends of the white space characters `\s+` can take, the longest first.
    let mut ends = Vec::new();
    for (index, character) in after.char_indices() {
        if !js_space(character) {
            break;
        }
        ends.push(index + character.len_utf8());
    }
    let tail = after
        .trim_end_matches(js_space)
        .trim_end_matches('#')
        .trim_end_matches(js_space)
        .len();
    for start in ends.into_iter().rev() {
        let Some(first) = after[start..].chars().next() else {
            continue;
        };
        let end = (start + first.len_utf8()).max(tail);
        let title = &after[start..end];
        // `.` does not match line terminators.
        if !title.contains(['\r', '\u{2028}', '\u{2029}']) {
            return Some((hashes, title));
        }
    }
    None
}

/// The line ranges that page and heading hints allow, or `None` without hints.
pub(crate) fn hinted_lines(
    document: &str,
    evidence: &ConfigValue,
    locations: &[SourceLocation],
) -> Option<Vec<(f64, f64)>> {
    let page = evidence.get("page");
    let heading = evidence.get("heading");
    if page.is_none() && heading.is_none() {
        return None;
    }
    let page_ranges: Option<Vec<(f64, f64)>> = page.map(|page| {
        locations
            .iter()
            .filter(|location| {
                matches!(page, ConfigValue::Number(page)
                    if location.page.is_some_and(|number| number as f64 == *page))
            })
            .map(|location| {
                (
                    location.range.start_line as f64,
                    location.range.end_line as f64,
                )
            })
            .collect()
    });
    let Some(heading) = heading.and_then(ConfigValue::as_str) else {
        return page_ranges;
    };
    let expected = nfkc(heading).trim_matches(js_space).to_owned();
    let lines: Vec<&str> = document.split('\n').collect();
    let mut sections = Vec::new();
    let mut open: Option<(usize, usize)> = None;
    for (index, line) in lines.iter().enumerate() {
        let Some((depth, title)) = heading_line(line) else {
            continue;
        };
        if let Some((open_depth, open_line)) = open
            && depth <= open_depth
        {
            sections.push((open_line as f64, index as f64));
            open = None;
        }
        if open.is_none() && nfkc(title) == expected {
            open = Some((depth, index + 1));
        }
    }
    if let Some((_, open_line)) = open {
        sections.push((open_line as f64, lines.len() as f64));
    }
    let Some(pages) = page_ranges else {
        return Some(sections);
    };
    Some(
        sections
            .iter()
            .flat_map(|section| {
                pages
                    .iter()
                    .map(|page| (section.0.max(page.0), section.1.min(page.1)))
            })
            .filter(|(start, end)| start <= end)
            .collect(),
    )
}

/// Pairs of adjacent characters.
pub(crate) fn grams(value: &str) -> HashSet<(char, char)> {
    let characters: Vec<char> = value.chars().collect();
    characters
        .windows(2)
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

/// Lines that look like the quote, for a quote that was not found: up to five, most similar first.
pub(crate) fn nearby_quotes(document: &str, query: &str) -> ConfigValue {
    let expected = grams(query);
    if expected.len() < 3 {
        return ConfigValue::Array(Vec::new());
    }
    let limit = 400.max(utf16_len(query) * 3);
    let mut found: Vec<(usize, usize, f64)> = Vec::new();
    let mut byte = 0;
    for line in document.split_inclusive('\n') {
        let end = byte + line.len();
        if utf16_len(line) <= limit {
            let mut collapsed = String::new();
            for character in nfkc(line).chars() {
                if !js_space(character) {
                    collapsed.push(character);
                } else if !collapsed.ends_with(' ') {
                    collapsed.push(' ');
                }
            }
            let got = grams(collapsed.trim_matches(' '));
            let overlap = got.iter().filter(|gram| expected.contains(gram)).count();
            let similarity = (2 * overlap) as f64 / (got.len() + expected.len()) as f64;
            if similarity >= 0.45 {
                found.push((byte, end - usize::from(line.ends_with('\n')), similarity));
            }
        }
        byte = end;
    }
    found.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    found.truncate(5);
    ConfigValue::Array(
        found
            .into_iter()
            .map(|(start, end, similarity)| {
                object(vec![
                    ("start", num(start as f64)),
                    ("end", num(end as f64)),
                    ("similarity", num(similarity)),
                ])
            })
            .collect(),
    )
}

/// `buffer.subarray(start, end).toString()` counted in code points.
pub(crate) fn characters(bytes: &[u8], start: f64, end: f64) -> usize {
    let clamp = |value: f64| -> usize {
        if value.is_nan() || value <= 0.0 {
            0
        } else {
            (value as usize).min(bytes.len())
        }
    };
    let (start, end) = (clamp(start), clamp(end));
    if end <= start {
        return 0;
    }
    String::from_utf8_lossy(&bytes[start..end]).chars().count()
}

/// The byte range of lines `start_line` to `end_line` (1-based, inclusive), clamped to the
/// document.
pub(crate) fn line_bytes(content: &str, start_line: f64, end_line: f64) -> (f64, f64) {
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let index = |line: f64| -> usize {
        if line.is_nan() || line <= 0.0 {
            0
        } else {
            (line as usize).min(lines.len())
        }
    };
    let first = index(start_line - 1.0);
    let last = index(end_line).max(first);
    let start: usize = lines[..first].iter().map(|line| line.len()).sum();
    let length: usize = lines[first..last].iter().map(|line| line.len()).sum();
    (start as f64, (start + length) as f64)
}

/// Whether result ranges in the same document cover enough of an evidence range: all of a short
/// one, or `min_coverage` of its characters.
pub(crate) fn covers(
    target: (f64, f64),
    results: impl IntoIterator<Item = (f64, f64)>,
    content: &str,
    short_evidence_chars: f64,
    min_coverage: f64,
) -> bool {
    let mut ranges: Vec<(f64, f64)> = results
        .into_iter()
        .map(|(start, end)| (target.0.max(start), target.1.min(end)))
        .filter(|(start, end)| start < end)
        .collect();
    ranges.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
    let bytes = content.as_bytes();
    let length = characters(bytes, target.0, target.1) as f64;
    let mut end = target.0;
    let mut count = 0;
    for (from, to) in ranges {
        if to <= end {
            continue;
        }
        count += characters(bytes, from.max(end), to);
        end = to;
    }
    let required = if length <= short_evidence_chars {
        length
    } else {
        length * min_coverage
    };
    count as f64 >= required
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(entries: Vec<(&str, ConfigValue)>) -> ConfigValue {
        object(entries)
    }

    #[test]
    fn matches_quotes_across_white_space_and_width() {
        let document = "前文\n\nフィルターは　６か月ごとに\n交換する。\n";
        let found = position(
            document,
            &evidence(vec![("quote", text("フィルターは 6か月ごとに 交換する。"))]),
            &[],
        );
        let start = document.find('フ').unwrap();
        assert_eq!(
            found.to_js_json(),
            format!(
                "{{\"range\":{{\"start\":{start},\"end\":{}}}}}",
                document.len() - 1
            )
        );
    }

    #[test]
    fn reads_markdown_headings_like_the_javascript_pattern() {
        assert_eq!(heading_line("## 手順 ##"), Some((2, "手順")));
        assert_eq!(heading_line("   # a"), Some((1, "a")));
        assert_eq!(heading_line("    # a"), None);
        assert_eq!(heading_line("####### a"), None);
        assert_eq!(heading_line("#a"), None);
        assert_eq!(heading_line("## a\r"), Some((2, "a")));
        assert_eq!(heading_line("## a\rb"), None);
        assert_eq!(heading_line("##  "), Some((2, " ")));
        assert_eq!(heading_line("# #"), Some((1, "#")));
    }
}
