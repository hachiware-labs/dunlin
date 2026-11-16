//! Character-window chunking for the `chunk.length` and `chunk.structure` modules.
//!
//! The rules follow the JavaScript chunker that this module replaced, including its regular
//! expressions, so that existing builds keep the same chunk ranges and reuse keys.

use std::collections::HashSet;

use icu_normalizer::ComposingNormalizerBorrowed;
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use crate::{ByteRange, Cancellation, LineRange, Result};

/// Documents and options for one chunking call.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChunkRequest {
    pub documents: Vec<ChunkDocument>,
    /// Maximum characters (Unicode scalar values) per chunk. A grapheme is never split.
    pub size: usize,
    /// Characters repeated from the end of the previous chunk.
    pub overlap: usize,
    /// Split at headings and page boundaries instead of preferring sentence ends.
    #[serde(default)]
    pub structure: bool,
    /// Store the NFKC form of each chunk as its search text.
    #[serde(default)]
    pub nfkc: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChunkDocument {
    pub content: String,
    /// Read Markdown headings for the chunk title.
    #[serde(default)]
    pub markdown: bool,
    /// One-based lines that start a page. Used with `structure`.
    #[serde(default)]
    pub boundaries: Vec<usize>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextChunk {
    pub heading: String,
    pub range: LineRange,
    pub byte_range: ByteRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_text: Option<String>,
}

struct Unit {
    byte: usize,
    line: usize,
    count: usize,
    break_after: bool,
}

pub fn chunk_documents(
    request: &ChunkRequest,
    cancel: &Cancellation,
) -> Result<Vec<Vec<TextChunk>>> {
    request
        .documents
        .iter()
        .map(|document| {
            cancel.check()?;
            Ok(chunk_document(document, request))
        })
        .collect()
}

fn chunk_document(document: &ChunkDocument, options: &ChunkRequest) -> Vec<TextChunk> {
    let content = document.content.as_str();
    let structure = options.structure;
    let boundaries: HashSet<usize> = if structure {
        document.boundaries.iter().copied().collect()
    } else {
        HashSet::new()
    };
    let sections: HashSet<usize> = if structure {
        content
            .split('\n')
            .enumerate()
            .filter(|(_, line)| section_heading(line))
            .map(|(index, _)| index + 1)
            .collect()
    } else {
        HashSet::new()
    };

    let mut units = Vec::new();
    let mut line = 1;
    for (byte, segment) in content.grapheme_indices(true) {
        units.push(Unit {
            byte,
            line,
            count: segment.chars().count(),
            break_after: matches!(
                segment.chars().next_back(),
                Some('\n' | '。' | '.' | '!' | '?')
            ),
        });
        if segment.ends_with('\n') {
            line += 1;
        }
    }
    if units.is_empty() {
        return Vec::new();
    }
    units.push(Unit {
        byte: content.len(),
        line,
        count: 0,
        break_after: false,
    });

    let normalizer = options.nfkc.then(ComposingNormalizerBorrowed::new_nfkc);
    let last = units.len() - 1;
    let mut chunks = Vec::new();
    let (mut start, mut end) = (0, 0);
    while end < last {
        let previous = end;
        let mut length = 0;
        end = start;
        while end < last && (end == start || length + units[end].count <= options.size) {
            let line = units[end].line;
            if structure
                && end > start
                && line != units[end - 1].line
                && (boundaries.contains(&line) || sections.contains(&line))
            {
                break;
            }
            length += units[end].count;
            end += 1;
        }
        while end <= previous && end < last {
            end += 1;
        }
        // Prefer a sentence end in the last quarter of the window.
        let min = start.max(previous).max(end - (end - start) / 4);
        if !structure && end < last {
            let mut candidate = end - 1;
            while candidate > min {
                if units[candidate - 1].break_after {
                    end = candidate;
                    break;
                }
                candidate -= 1;
            }
        }
        let (first, next) = (&units[start], &units[end]);
        let text = &content[first.byte..next.byte];
        chunks.push(TextChunk {
            heading: if document.markdown {
                markdown_heading(text)
            } else {
                String::new()
            },
            range: LineRange {
                start_line: first.line,
                end_line: units[end - 1].line,
            },
            byte_range: ByteRange {
                start: first.byte,
                end: next.byte,
            },
            search_text: normalizer
                .as_ref()
                .map(|normalizer| normalizer.normalize(text).into_owned()),
        });
        let mut tail = 0;
        let mut next_start = end;
        while next_start > start + 1 && tail + units[next_start - 1].count <= options.overlap {
            next_start -= 1;
            tail += units[next_start].count;
        }
        start = next_start;
    }
    chunks
}

/// JavaScript `\s`: WhiteSpace and LineTerminator, which differs from `char::is_whitespace`.
fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `/^\s{0,3}#{1,6}\s+\S/u` for one line.
fn section_heading(line: &str) -> bool {
    let rest = line.trim_start_matches(js_space);
    if line.chars().count() - rest.chars().count() > 3 {
        return false;
    }
    let text = rest.trim_start_matches('#');
    let hashes = rest.len() - text.len();
    if !(1..=6).contains(&hashes) {
        return false;
    }
    let body = text.trim_start_matches(js_space);
    body.len() < text.len() && !body.is_empty()
}

/// The trimmed first capture of `/^\s{0,3}#{1,6} +([^\r\n]+)/mu`, or an empty string.
fn markdown_heading(text: &str) -> String {
    let starts = std::iter::once(0).chain(
        text.char_indices()
            .filter(|(_, c)| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            .map(|(index, c)| index + c.len_utf8()),
    );
    for start in starts {
        if let Some(capture) = heading_at(&text[start..]) {
            return capture.trim_matches(js_space).to_owned();
        }
    }
    String::new()
}

fn heading_at(text: &str) -> Option<&str> {
    let rest = text.trim_start_matches(js_space);
    if text.chars().count() - rest.chars().count() > 3 {
        return None;
    }
    let after_hashes = rest.trim_start_matches('#');
    if !(1..=6).contains(&(rest.len() - after_hashes.len())) {
        return None;
    }
    let body = after_hashes.trim_start_matches(' ');
    let spaces = after_hashes.len() - body.len();
    if spaces == 0 {
        return None;
    }
    let line = &body[..body.find(['\r', '\n']).unwrap_or(body.len())];
    if !line.is_empty() {
        Some(line)
    } else if spaces >= 2 {
        // `/ +/` gives back its last space so that `[^\r\n]+` can match it.
        Some(&after_hashes[spaces - 1..spaces])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(content: &str, size: usize, overlap: usize, structure: bool) -> Vec<TextChunk> {
        let request = ChunkRequest {
            documents: vec![ChunkDocument {
                content: content.to_owned(),
                markdown: true,
                boundaries: vec![],
            }],
            size,
            overlap,
            structure,
            nfkc: true,
        };
        chunk_documents(&request, &Cancellation::default())
            .unwrap()
            .remove(0)
    }

    #[test]
    fn covers_the_document_with_overlapping_windows() {
        let output = chunks("# 作業\nＡＢＣを確認する。\n次の手順です。", 11, 2, false);
        assert_eq!(output[0].heading, "作業");
        assert_eq!(output[0].byte_range.start, 0);
        assert_eq!(
            output.last().unwrap().byte_range.end,
            "# 作業\nＡＢＣを確認する。\n次の手順です。".len()
        );
        assert!(output[0].search_text.as_deref().unwrap().contains("ABC"));
        assert!(chunks("", 10, 0, false).is_empty());
    }

    #[test]
    fn reads_headings_like_the_javascript_expressions() {
        assert_eq!(markdown_heading("本文\n## 手順  \n続き"), "手順");
        assert_eq!(markdown_heading("\n\n# 先頭"), "先頭");
        assert_eq!(markdown_heading("#  \n## 後"), "");
        assert_eq!(markdown_heading("####### 七つ"), "");
        assert_eq!(markdown_heading("    # 字下げ"), "");
        assert!(section_heading("   ### 見出し"));
        assert!(!section_heading("#見出し"));
        assert!(!section_heading("# \r"));
        assert!(section_heading("#\u{3000}全角空白"));
    }

    #[test]
    fn splits_at_sections_with_structure() {
        let output = chunks("# A\n本文\n# B\n本文", 100, 0, true);
        assert_eq!(output.len(), 2);
        assert_eq!(output[1].heading, "B");
        assert_eq!(output[1].range.start_line, 3);
    }
}
