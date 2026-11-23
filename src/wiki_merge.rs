//! The statements of a page as units, so that a new source adds to a page instead of replacing it.
//!
//! A page is Markdown whose statements end with source markers such as ` [1] [2]`, numbered by
//! the citations of the page. A unit is one statement with its own citations, which can be kept,
//! dropped or joined with the units of another version of the page and numbered again.

use crate::config_value::ConfigValue;
use crate::wiki_project::{marker_index, markers};

/// One statement of a page: its text without source markers, and what it cites.
#[derive(Clone)]
pub(crate) struct Unit {
    pub text: String,
    pub citations: Vec<ConfigValue>,
}

/// The text of a block without the source markers that end it.
fn unmarked(block: &str) -> &str {
    let mut rest = block;
    while let Some(body) = rest.strip_suffix(']') {
        let digits = body.len() - body.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        match body[..body.len() - digits].strip_suffix(" [") {
            Some(before) if digits > 0 => rest = before,
            _ => break,
        }
    }
    rest
}

/// The statements of a page. Blocks without markers, such as headings, belong to the statement
/// that follows them, as the fact that wrote them did.
pub(crate) fn units(markdown: &str, citations: &[ConfigValue]) -> Vec<Unit> {
    let mut found = Vec::new();
    let mut open: Vec<&str> = Vec::new();
    for block in markdown.split("\n\n") {
        let marks = markers(block);
        if marks.is_empty() {
            open.push(block);
            continue;
        }
        open.push(unmarked(block));
        found.push(Unit {
            text: open.join("\n\n"),
            citations: marks
                .iter()
                .filter_map(|mark| marker_index(mark))
                .filter_map(|index| citations.get(index).cloned())
                .collect(),
        });
        open.clear();
    }
    if open.iter().any(|block| !block.is_empty()) {
        found.push(Unit {
            text: open.join("\n\n"),
            citations: Vec::new(),
        });
    }
    found
}

/// The Markdown of the statements, with markers numbered in order, and the citations they number.
pub(crate) fn compose(units: &[Unit]) -> (String, Vec<ConfigValue>) {
    let mut citations = Vec::new();
    let mut blocks = Vec::new();
    for unit in units {
        let mut block = unit.text.clone();
        for citation in &unit.citations {
            citations.push(citation.clone());
            block += &format!(" [{}]", citations.len());
        }
        blocks.push(block);
    }
    (blocks.join("\n\n"), citations)
}

/// What a citation points at: the source, its version and the bytes quoted.
pub(crate) fn passage(citation: &ConfigValue) -> String {
    let source = citation.get("source");
    let part = |name: &str| {
        source
            .and_then(|source| source.get(name))
            .cloned()
            .unwrap_or(ConfigValue::Null)
    };
    ConfigValue::Array(vec![
        part("sourceId"),
        part("objectId"),
        part("version"),
        citation
            .get("byteRange")
            .cloned()
            .unwrap_or(ConfigValue::Null),
    ])
    .to_js_json()
}

/// The earlier statements that a new version of a page left out: neither their text nor any
/// passage they cite is on the page any more.
pub(crate) fn removed<'a>(earlier: &'a [Unit], current: &[Unit]) -> Vec<&'a Unit> {
    let cited: std::collections::HashSet<String> = current
        .iter()
        .flat_map(|unit| unit.citations.iter().map(passage))
        .collect();
    earlier
        .iter()
        .filter(|unit| !unit.citations.is_empty())
        .filter(|unit| current.iter().all(|kept| kept.text != unit.text))
        .filter(|unit| {
            unit.citations
                .iter()
                .all(|citation| !cited.contains(&passage(citation)))
        })
        .collect()
}

/// A page with more statements: those it had, then the new ones. A statement that comes again
/// in the same words stays where it was and cites what the new one cites.
pub(crate) fn extended(earlier: Vec<Unit>, added: Vec<Unit>) -> Vec<Unit> {
    let mut all = earlier;
    for unit in added {
        match all.iter_mut().find(|known| known.text == unit.text) {
            Some(known) => known.citations = unit.citations,
            None => all.push(unit),
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::{Unit, compose, extended, passage, removed, units};
    use crate::config_value::ConfigValue;
    use crate::index_stage::{object, text};

    fn citation(source: &str, start: f64) -> ConfigValue {
        object(vec![
            (
                "source",
                object(vec![
                    ("sourceId", text(source)),
                    ("objectId", text("a.md")),
                    ("version", text("v1")),
                ]),
            ),
            (
                "byteRange",
                object(vec![
                    ("start", ConfigValue::Number(start)),
                    ("end", ConfigValue::Number(start + 4.0)),
                ]),
            ),
        ])
    }

    #[test]
    fn a_page_is_its_statements_and_back() {
        let citations = [citation("a", 0.0), citation("a", 10.0), citation("b", 0.0)];
        let markdown = "## 概要\n\nスクラムは枠組みである。 [1] [2]\n\nスプリントは1か月以内。 [3]";
        let found = units(markdown, &citations);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].text, "## 概要\n\nスクラムは枠組みである。");
        assert_eq!(found[0].citations.len(), 2);
        assert_eq!(found[1].text, "スプリントは1か月以内。");
        let (again, numbered) = compose(&found);
        assert_eq!(again, markdown);
        assert_eq!(numbered.len(), 3);
    }

    #[test]
    fn a_statement_is_removed_when_neither_its_text_nor_its_passage_stays() {
        let citations = [citation("a", 0.0), citation("a", 10.0), citation("a", 20.0)];
        let earlier = units("一。 [1]\n\n二。 [2]\n\n三。 [3]", &citations);
        let current = vec![
            // The same text, cited anew.
            Unit {
                text: "一。".into(),
                citations: vec![citation("b", 0.0)],
            },
            // Other words for the same passage.
            Unit {
                text: "二つめ。".into(),
                citations: vec![citation("a", 10.0)],
            },
        ];
        let gone = removed(&earlier, &current);
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].text, "三。");
    }

    #[test]
    fn new_statements_follow_the_earlier_ones_with_their_own_numbers() {
        let earlier = units(
            "古い事実。 [1]\n\n変わらない事実。 [2]",
            &[citation("a", 0.0), citation("a", 10.0)],
        );
        let added = vec![
            Unit {
                text: "古い事実。".into(),
                citations: vec![citation("b", 0.0)],
            },
            Unit {
                text: "新しい事実。".into(),
                citations: vec![citation("b", 8.0)],
            },
        ];
        let (markdown, citations) = compose(&extended(earlier, added));
        // A statement the page already makes is not written twice; it cites the newer passage.
        assert_eq!(
            markdown,
            "古い事実。 [1]\n\n変わらない事実。 [2]\n\n新しい事実。 [3]"
        );
        assert_eq!(passage(&citations[0]), passage(&citation("b", 0.0)));
        assert_eq!(passage(&citations[1]), passage(&citation("a", 10.0)));
        assert_eq!(citations.len(), 3);
    }
}
