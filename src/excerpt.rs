//! Evidence excerpts read from saved corpus revisions.
//!
//! Search responses report each excerpt's position in Unicode scalar values (JavaScript string
//! iteration) so that clients can highlight the text without loading the whole document.

use serde::Serialize;

use crate::{Error, Evidence, Result, SourceLocation};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceExcerpt {
    /// The text of the evidence range.
    pub text: String,
    /// Characters before the excerpt in the document.
    pub start: usize,
    /// `start` plus the characters in `text`.
    pub end: usize,
    /// Source locations, such as pages, that overlap the evidence lines.
    pub locations: Vec<SourceLocation>,
}

pub(crate) fn excerpt(
    content: &str,
    locations: &[SourceLocation],
    evidence: &Evidence,
) -> Result<EvidenceExcerpt> {
    let range = evidence.range;
    if range.start_line == 0 || range.end_line < range.start_line {
        return Err(Error::Invalid("Evidence range is invalid"));
    }
    let (from, to) = match evidence.byte_range {
        Some(bytes) => {
            if content.get(bytes.start..bytes.end).is_none() {
                return Err(Error::Invalid(
                    "Evidence byte range is outside the source object",
                ));
            }
            (bytes.start, bytes.end)
        }
        // Lines end after '\n'. Ranges past the last line stop at the end of the document.
        None => {
            let mut starts = std::iter::once(0)
                .chain(content.match_indices('\n').map(|(index, _)| index + 1))
                .filter(|&offset| offset < content.len());
            let from = starts.nth(range.start_line - 1).unwrap_or(content.len());
            let to = starts
                .nth(range.end_line - range.start_line)
                .unwrap_or(content.len());
            (from, to)
        }
    };
    let text = &content[from..to];
    let start = content[..from].chars().count();
    Ok(EvidenceExcerpt {
        text: text.to_owned(),
        start,
        end: start + text.chars().count(),
        locations: locations
            .iter()
            .filter(|location| {
                location.range.start_line <= range.end_line
                    && location.range.end_line >= range.start_line
            })
            .cloned()
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ByteRange, LineRange, SourceRef};

    fn evidence(start_line: usize, end_line: usize, byte_range: Option<ByteRange>) -> Evidence {
        Evidence {
            source: SourceRef {
                source_id: "docs".into(),
                object_id: "a.md".into(),
                url: "file:///a.md".into(),
                version: "0".repeat(64),
                modified_at_ms: None,
            },
            range: LineRange {
                start_line,
                end_line,
            },
            acquired_at_ms: 0,
            byte_range,
        }
    }

    fn location(start_line: usize, end_line: usize, page: usize) -> SourceLocation {
        SourceLocation {
            range: LineRange {
                start_line,
                end_line,
            },
            page: Some(page),
            symbol: None,
            bbox: None,
        }
    }

    #[test]
    fn counts_characters_before_line_ranges() {
        let content = "見出し\n本文です。\n最後";
        let pages = [location(1, 1, 1), location(2, 3, 2)];
        let middle = excerpt(content, &pages, &evidence(2, 2, None)).unwrap();
        assert_eq!(
            (middle.text.as_str(), middle.start, middle.end),
            ("本文です。\n", 4, 10)
        );
        assert_eq!(middle.locations.len(), 1);
        let last = excerpt(content, &pages, &evidence(3, 9, None)).unwrap();
        assert_eq!((last.text.as_str(), last.start, last.end), ("最後", 10, 12));
        let after = excerpt(content, &[], &evidence(8, 9, None)).unwrap();
        assert_eq!((after.text.as_str(), after.start), ("", 12));
    }

    #[test]
    fn uses_byte_ranges_when_present() {
        let content = "👍🏽 手順\n次";
        let range = ByteRange { start: 9, end: 15 };
        let found = excerpt(content, &[], &evidence(1, 1, Some(range))).unwrap();
        assert_eq!(
            (found.text.as_str(), found.start, found.end),
            ("手順", 3, 5)
        );
        let split = ByteRange { start: 1, end: 4 };
        assert!(excerpt(content, &[], &evidence(1, 1, Some(split))).is_err());
    }
}
