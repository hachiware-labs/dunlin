//! The content check of `verify --content` (spec 14.3 and 28.12 step 5a): pages and sources that
//! changed since the last check are compared with related ones by the model, and the issues it
//! finds are kept per pair of items until either changes.
//!
//! Related items are chosen, ordered and numbered as `project-wiki-inspection.mjs` chose them, so
//! the model receives the same requests.

// The entry points are called from `node`, which test builds leave out.
#![cfg_attr(test, allow(dead_code))]

use std::cmp::Ordering;

use rusqlite::{Connection, OptionalExtension};

use crate::KnowledgeBase;
use crate::config_value::ConfigValue;
use crate::eval_stage::parse;
use crate::evidence_match::nfkc;
use crate::index_stage::{assign, items, object, sql, string_field, text, truthy};
use crate::source_load::sha256_hex;
use crate::transform_stage::StageError;
use crate::wiki_ingress::words;
use crate::wiki_project::{
    In, Project, array, current_document, from_units, line_count, marker_index, model_output,
    nonempty, paragraph_markers, positive_integer, str_of, strict, unique_index, utf16_prefix,
};
use crate::wiki_work::Tracker;

/// `z.toJSONSchema(findings)`
const FINDINGS_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"issues":{"type":"array","items":{"type":"object","properties":{"kind":{"type":"string","enum":["contradiction","source_disagreement","duplicate"]},"first":{"type":"object","properties":{"item":{"type":"integer","exclusiveMinimum":0,"maximum":9007199254740991},"quote":{"type":"string","minLength":1}},"required":["item","quote"],"additionalProperties":false},"second":{"type":"object","properties":{"item":{"type":"integer","exclusiveMinimum":0,"maximum":9007199254740991},"quote":{"type":"string","minLength":1}},"required":["item","quote"],"additionalProperties":false},"description":{"type":"string","minLength":1}},"required":["kind","first","second","description"],"additionalProperties":false}}},"required":["issues"],"additionalProperties":false}"#;

const INSTRUCTIONS: &str = "Assess the numbered excerpts for factual contradictions, disagreement between a source and a page, or duplicate page content. Every comparison must include item 1. Use two verbatim quotes, one from each excerpt. Return an empty list where there is no issue.";

/// A page or source in the check.
struct Item {
    id: String,
    page: bool,
    value: ConfigValue,
    title: String,
    /// The first 4500 UTF-16 units of the text.
    units: Vec<u16>,
    evidence: ConfigValue,
    fingerprint: String,
}

impl Item {
    fn text(&self) -> String {
        from_units(&self.units)
    }

    /// `itemKeys(item)`: its sources and up to 35 distinct words of its start.
    fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = if self.page {
            items(self.value.get("citations"))
                .iter()
                .map(|citation| {
                    format!(
                        "source:{}",
                        crate::eval_stage::js_string(
                            citation
                                .get("source")
                                .and_then(|source| source.get("sourceId"))
                        )
                    )
                })
                .collect()
        } else {
            vec![format!(
                "source:{}",
                crate::eval_stage::js_string(self.value.get("sourceId"))
            )]
        };
        let mut start: Vec<u16> = self.title.encode_utf16().collect();
        start.push(u16::from(b'\n'));
        start.extend(&self.units);
        start.truncate(1000);
        let normalized = nfkc(&from_units(&start)).to_lowercase();
        let mut tokens: Vec<&str> = Vec::new();
        // `/\p{L}/u`: a letter of any kind.
        let categories =
            icu_properties::CodePointMapData::<icu_properties::props::GeneralCategory>::new();
        let letter = |c: char| {
            icu_properties::props::GeneralCategoryGroup::Letter.contains(categories.get(c))
        };
        for word in words(&normalized) {
            if word.chars().count() >= 2 && word.chars().any(letter) && !tokens.contains(&word) {
                tokens.push(word);
            }
        }
        keys.extend(
            tokens
                .into_iter()
                .take(35)
                .map(|token| format!("word:{token}")),
        );
        keys
    }
}

/// `String.prototype.localeCompare` with the root collation.
pub(crate) fn locale_compare(left: &str, right: &str) -> Ordering {
    use std::sync::OnceLock;
    static COLLATOR: OnceLock<Option<icu_collator::CollatorBorrowed<'static>>> = OnceLock::new();
    COLLATOR
        .get_or_init(|| {
            icu_collator::Collator::try_new(Default::default(), Default::default()).ok()
        })
        .as_ref()
        .map_or_else(|| left.cmp(right), |collator| collator.compare(left, right))
}

/// `[a, b].sort()`, which compares UTF-16 code units.
fn utf16_order(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

/// `entityId(first, second)`
fn pair_id(first: &str, second: &str) -> String {
    let mut ids = [first, second];
    ids.sort_by(|a, b| utf16_order(a, b));
    ConfigValue::Array(ids.iter().map(|id| text(id)).collect()).to_js_json()
}

/// `excerpt(item, quote)`: the quoted text with its sources, or none when it is not unique.
fn excerpt(item: &Item, quote: &str) -> Option<ConfigValue> {
    let content = item.text();
    let at = unique_index(&content, quote)?;
    if item.page {
        let (_, markers) = paragraph_markers(&content, quote)?;
        let citations: Vec<ConfigValue> = markers
            .iter()
            .filter_map(|marker| {
                marker_index(marker)
                    .and_then(|index| items(item.value.get("citations")).get(index).cloned())
            })
            .filter(|citation| truthy(Some(citation)))
            .collect();
        if citations.is_empty() {
            return None;
        }
        return Some(object(vec![
            ("page", item.value.get("id").cloned().unwrap_or_default()),
            ("quote", text(quote)),
            ("citations", ConfigValue::Array(citations)),
        ]));
    }
    let prefix = &content[..at];
    let start_line = 1 + prefix.matches('\n').count();
    let end_line = start_line + line_count(quote) - 1;
    let mut entries = match &item.evidence {
        ConfigValue::Object(entries) => entries.clone(),
        _ => Vec::new(),
    };
    assign(
        &mut entries,
        "range",
        Some(object(vec![
            ("startLine", ConfigValue::Number(start_line as f64)),
            ("endLine", ConfigValue::Number(end_line as f64)),
        ])),
    );
    assign(
        &mut entries,
        "byteRange",
        Some(object(vec![
            ("start", ConfigValue::Number(prefix.len() as f64)),
            (
                "end",
                ConfigValue::Number((prefix.len() + quote.len()) as f64),
            ),
        ])),
    );
    Some(object(vec![
        ("source", item.value.clone()),
        ("quote", text(quote)),
        (
            "citations",
            ConfigValue::Array(vec![ConfigValue::Object(entries)]),
        ),
    ]))
}

/// The pages and current sources to check, in the order `getMaterial` returned them.
fn material(
    connection: &Connection,
    database: &KnowledgeBase,
    pages: &[ConfigValue],
    project: &Project<'_>,
) -> Result<Vec<Item>, StageError> {
    let mut found: Vec<Item> = Vec::new();
    let put = |found: &mut Vec<Item>, item: Item| match found
        .iter()
        .position(|other| other.id == item.id)
    {
        Some(index) => found[index] = item,
        None => found.push(item),
    };
    for page in pages.iter().filter(|page| !truthy(page.get("archived"))) {
        let title = crate::eval_stage::js_string(page.get("title"));
        let markdown = str_of(page, "markdown");
        let versions = ConfigValue::Array(
            items(page.get("citations"))
                .iter()
                .map(|citation| {
                    ConfigValue::Array(vec![
                        citation
                            .get("source")
                            .and_then(|source| source.get("version"))
                            .cloned()
                            .unwrap_or(ConfigValue::Null),
                        citation
                            .get("byteRange")
                            .cloned()
                            .unwrap_or(ConfigValue::Null),
                    ])
                })
                .collect(),
        );
        let fingerprint = ConfigValue::Array(vec![
            page.get("title").cloned().unwrap_or(ConfigValue::Null),
            page.get("markdown").cloned().unwrap_or(ConfigValue::Null),
            versions,
        ])
        .to_js_json();
        put(
            &mut found,
            Item {
                id: format!("page:{}", str_of(page, "id")),
                page: true,
                value: page.clone(),
                title,
                units: utf16_prefix(markdown, 4500),
                evidence: ConfigValue::Null,
                fingerprint: sha256_hex(fingerprint.as_bytes()),
            },
        );
    }
    let mut statement = connection
        .prepare("SELECT data FROM wiki_sources")
        .map_err(sql)?;
    let rows: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    for data in rows {
        let source = parse(&data)?;
        let Some(document) = current_document(database, &source, project.cancel)? else {
            continue;
        };
        let evidence = document.get("evidence").cloned().unwrap_or_default();
        let version = evidence
            .get("source")
            .and_then(|source| source.get("version"));
        if version != source.get("version") {
            continue;
        }
        put(
            &mut found,
            Item {
                id: format!(
                    "source:{}",
                    crate::eval_stage::js_string(source.get("sourceId"))
                ),
                page: false,
                title: crate::eval_stage::js_string(source.get("objectId")),
                units: utf16_prefix(str_of(&document, "content"), 4500),
                evidence,
                fingerprint: crate::eval_stage::js_string(source.get("version")),
                value: source,
            },
        );
    }
    Ok(found)
}

struct Side {
    item: usize,
    quote: String,
}

struct Finding {
    kind: String,
    first: Side,
    second: Side,
    description: String,
}

fn findings(value: &ConfigValue) -> Option<Vec<Finding>> {
    let [list] = strict(value, &["issues"])?[..] else {
        return None;
    };
    let side = |value: &ConfigValue| {
        let [item, quote] = strict(value, &["item", "quote"])?[..] else {
            return None;
        };
        Some(Side {
            item: positive_integer(item)?,
            quote: nonempty(quote)?,
        })
    };
    array(list)?
        .iter()
        .map(|issue| {
            let [kind, first, second, description] =
                strict(issue, &["kind", "first", "second", "description"])?[..]
            else {
                return None;
            };
            let kind = kind.as_str().filter(|kind| {
                ["contradiction", "source_disagreement", "duplicate"].contains(kind)
            })?;
            Some(Finding {
                kind: kind.to_owned(),
                first: side(first)?,
                second: side(second)?,
                description: nonempty(description)?,
            })
        })
        .collect()
}

/// `inspectWikiContent(db, database, pages, { page, generate, progress })`: the issues of current
/// pairs and the size of this check.
pub(crate) fn inspect(
    project: &Project<'_>,
    connection: &Connection,
    database: &KnowledgeBase,
    pages: &[ConfigValue],
    tracker: &Tracker,
) -> Result<(Vec<ConfigValue>, ConfigValue), StageError> {
    let model = items(project.request.plan.get("content"))
        .first()
        .and_then(|content| string_field(content, "model"))
        .unwrap_or_default()
        .to_owned();
    let only = project.value_str("page").filter(|page| !page.is_empty());
    let material = material(connection, database, pages, project)?;
    let reviewed = |id: &str| -> Result<Option<String>, StageError> {
        connection
            .query_row(
                "SELECT fingerprint FROM wiki_reviewed_material WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)
    };
    let mut anchors = Vec::new();
    for (index, item) in material.iter().enumerate() {
        let selected = match only {
            Some(page) => item.id == format!("page:{page}"),
            None => reviewed(&item.id)?.as_deref() != Some(item.fingerprint.as_str()),
        };
        if selected {
            anchors.push(index);
        }
    }
    let keys: Vec<Vec<String>> = material.iter().map(Item::keys).collect();
    let mut postings: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, item_keys) in keys.iter().enumerate() {
        for key in item_keys {
            match postings.iter_mut().find(|(name, _)| name == key) {
                Some((_, list)) => list.push(index),
                None => postings.push((key.clone(), vec![index])),
            }
        }
    }
    let posting = |key: &str| {
        postings
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, list)| list.as_slice())
            .unwrap_or_default()
    };
    let mut batches = 0;
    let mut pairs = 0;
    for (completed, &anchor) in anchors.iter().enumerate() {
        tracker.progress("reviewing_content", Some((completed, anchors.len())));
        let first = &material[anchor];
        let mut similarity: Vec<(usize, f64)> = Vec::new();
        let mut seen: Vec<&String> = Vec::new();
        for key in &keys[anchor] {
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let candidates = posting(key);
            let source_key = key.starts_with("source:");
            if candidates.len() > 40 && !source_key {
                continue;
            }
            for &other in candidates {
                if other == anchor || (!first.page && !material[other].page) {
                    continue;
                }
                let relevance = if source_key {
                    100.0
                } else {
                    1.0 / candidates.len() as f64
                };
                match similarity.iter_mut().find(|(index, _)| *index == other) {
                    Some((_, score)) => *score += relevance,
                    None => similarity.push((other, relevance)),
                }
            }
        }
        similarity.sort_by(|a, b| {
            let difference = b.1 - a.1;
            if difference > 0.0 {
                Ordering::Greater
            } else if difference < 0.0 {
                Ordering::Less
            } else {
                locale_compare(&material[a.0].id, &material[b.0].id)
            }
        });
        let mut unreviewed = Vec::new();
        for &(other, _) in similarity.iter().take(6) {
            let item = &material[other];
            let record: Option<String> = connection
                .query_row(
                    "SELECT data FROM wiki_content_pairs WHERE id = ?1",
                    [pair_id(&first.id, &item.id)],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql)?;
            let current = match record {
                None => false,
                Some(data) => {
                    let pair = parse(&data)?;
                    let versions = pair.get("versions");
                    let version = |id: &str| {
                        versions
                            .and_then(|versions| versions.get(id))
                            .and_then(ConfigValue::as_str)
                    };
                    version(&first.id) == Some(first.fingerprint.as_str())
                        && version(&item.id) == Some(item.fingerprint.as_str())
                }
            };
            if !current {
                unreviewed.push(other);
            }
        }
        if !unreviewed.is_empty() {
            let options: Vec<&Item> = std::iter::once(anchor)
                .chain(unreviewed.iter().copied())
                .map(|index| &material[index])
                .collect();
            let input = In::Map(vec![(
                "items",
                In::List(
                    options
                        .iter()
                        .enumerate()
                        .map(|(index, item)| {
                            In::Map(vec![
                                ("item", In::number(index + 1)),
                                ("type", In::text(if item.page { "page" } else { "source" })),
                                ("title", In::text(&item.title)),
                                ("text", In::Units(item.units.clone())),
                            ])
                        })
                        .collect(),
                ),
            )]);
            let answer = project.generate(&model, FINDINGS_SCHEMA, INSTRUCTIONS, &input)?;
            let found = findings(&answer).ok_or_else(model_output)?;
            let mut by_other: Vec<Vec<ConfigValue>> = vec![Vec::new(); unreviewed.len()];
            for finding in found {
                if (finding.first.item != 1 && finding.second.item != 1)
                    || finding.first.item == finding.second.item
                {
                    continue;
                }
                let (Some(a), Some(b)) = (
                    options.get(finding.first.item - 1),
                    options.get(finding.second.item - 1),
                ) else {
                    continue;
                };
                if (finding.kind == "source_disagreement") != (a.page != b.page) {
                    continue;
                }
                let mut ordered = [(*a, &finding.first), (*b, &finding.second)];
                ordered.sort_by(|x, y| locale_compare(&x.0.id, &y.0.id));
                let evidence: Option<Vec<ConfigValue>> = ordered
                    .iter()
                    .map(|(item, side)| excerpt(item, &side.quote))
                    .collect();
                let Some(evidence) = evidence else {
                    continue;
                };
                let identity = ConfigValue::Array(vec![
                    text(&finding.kind),
                    ConfigValue::Array(
                        ordered
                            .iter()
                            .map(|(item, side)| {
                                ConfigValue::Array(vec![text(&item.id), text(&nfkc(&side.quote))])
                            })
                            .collect(),
                    ),
                ]);
                let id = format!("issue-{}", sha256_hex(identity.to_js_json().as_bytes()));
                let index = finding.first.item.max(finding.second.item) - 2;
                by_other[index].push(object(vec![
                    ("id", text(&id)),
                    ("code", text("CONTENT_CANDIDATE")),
                    ("kind", text(&finding.kind)),
                    ("description", text(&finding.description)),
                    (
                        "pages",
                        ConfigValue::Array(
                            ordered
                                .iter()
                                .filter(|(item, _)| item.page)
                                .map(|(item, _)| item.value.get("id").cloned().unwrap_or_default())
                                .collect(),
                        ),
                    ),
                    ("evidence", ConfigValue::Array(evidence)),
                ]));
            }
            for (index, &other) in unreviewed.iter().enumerate() {
                let item = &material[other];
                let record = object(vec![
                    (
                        "versions",
                        ConfigValue::Object(vec![
                            (first.id.clone(), text(&first.fingerprint)),
                            (item.id.clone(), text(&item.fingerprint)),
                        ]),
                    ),
                    (
                        "issues",
                        ConfigValue::Array(std::mem::take(&mut by_other[index])),
                    ),
                ]);
                connection
                    .execute(
                        "INSERT OR REPLACE INTO wiki_content_pairs VALUES (?1, ?2)",
                        [pair_id(&first.id, &item.id), record.to_js_json()],
                    )
                    .map_err(sql)?;
            }
            batches += 1;
            pairs += unreviewed.len();
        }
        if only.is_none() {
            connection
                .execute(
                    "INSERT OR REPLACE INTO wiki_reviewed_material VALUES (?1, ?2)",
                    [&first.id, &first.fingerprint],
                )
                .map_err(sql)?;
        }
    }
    tracker.progress("reviewing_content", Some((anchors.len(), anchors.len())));
    let mut issues = Vec::new();
    let mut statement = connection
        .prepare("SELECT data FROM wiki_content_pairs")
        .map_err(sql)?;
    let rows: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    for data in rows {
        let record = parse(&data)?;
        let current = match record.get("versions") {
            Some(ConfigValue::Object(versions)) => versions.iter().all(|(id, fingerprint)| {
                material
                    .iter()
                    .find(|item| &item.id == id)
                    .is_some_and(|item| fingerprint.as_str() == Some(item.fingerprint.as_str()))
            }),
            _ => true,
        };
        if current {
            issues.extend(items(record.get("issues")).iter().cloned());
        }
    }
    let review = object(vec![
        ("changed_items", ConfigValue::Number(anchors.len() as f64)),
        ("compared_pairs", ConfigValue::Number(pairs as f64)),
        ("model_calls", ConfigValue::Number(f64::from(batches))),
    ]);
    Ok((issues, review))
}
