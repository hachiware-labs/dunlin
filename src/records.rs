//! Identifiers and record shapes of the data management system (docs/data-management.md).
//!
//! An object's ID is the hash of its content. An item is a document at one stage: its parts, each
//! an object with a kind and a place in the document. A snapshot's own ID is a serial number, so
//! what is compared and keyed is the digest of its list of items. Dates, names, paths and storage
//! locations have no field here, so they cannot reach an identifier.

use std::fmt::Write as _;

use icu_normalizer::ComposingNormalizerBorrowed;
use serde_json::{Value, json};

/// The version of the record shapes. It is part of every identifier but the object ID.
pub const FORMAT: u32 = 1;

/// The canonical JSON of a value: object keys in code point order, strings in NFC, integral
/// numbers without a fraction and other numbers in their shortest form.
pub fn canonical(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &ComposingNormalizerBorrowed::new_nfc(), &mut out);
    out
}

fn write_canonical(value: &Value, nfc: &ComposingNormalizerBorrowed<'static>, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                let _ = write!(out, "{value}");
            } else if let Some(value) = number.as_u64() {
                let _ = write!(out, "{value}");
            } else {
                match number.as_f64() {
                    // 2^53: below it every integral f64 is an exact integer.
                    Some(value)
                        if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 =>
                    {
                        let _ = write!(out, "{}", value as i64);
                    }
                    _ => out.push_str(&number.to_string()),
                }
            }
        }
        Value::String(text) => {
            out.push_str(&Value::String(nfc.normalize(text).into_owned()).to_string());
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, nfc, out);
            }
            out.push(']');
        }
        Value::Object(entries) => {
            let mut entries: Vec<(String, &Value)> = entries
                .iter()
                .map(|(key, value)| (nfc.normalize(key).into_owned(), value))
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            out.push('{');
            for (index, (key, value)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(value, nfc, out);
            }
            out.push('}');
        }
    }
}

/// BLAKE3 of the canonical JSON of `[FORMAT, value]`, in the domain of `context`, so that equal
/// values of different kinds get different identifiers.
fn derive(context: &'static str, value: &Value) -> String {
    blake3::Hasher::new_derive_key(context)
        .update(canonical(&json!([FORMAT, value])).as_bytes())
        .finalize()
        .to_hex()
        .to_string()
}

macro_rules! identifier {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            fn from_hex(hex: &str) -> Self {
                Self(format!(concat!($prefix, "-{}"), hex))
            }

            /// An identifier read back from a record.
            pub fn parse(text: &str) -> Option<Self> {
                let hex = text.strip_prefix(concat!($prefix, "-"))?;
                (hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .then(|| Self(text.to_owned()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

identifier!(
    /// Content: `obj-` and the plain BLAKE3 of the bytes. Metadata is not part of it.
    ObjectId,
    "obj"
);
identifier!(
    /// What one document is at one stage: its parts, in order.
    ItemId,
    "itm"
);
identifier!(
    /// The modules that apply to an item.
    ProcessId,
    "prc"
);
identifier!(
    /// What a snapshot holds: its type and its list of items. Two snapshots with the same digest
    /// hold the same thing, whatever their serial numbers.
    ListDigest,
    "lst"
);
identifier!(
    /// "This process on these inputs": looked up before a stage runs.
    SnapshotKey,
    "sky"
);

/// The ID of the object with these bytes.
pub fn object_id(bytes: &[u8]) -> ObjectId {
    ObjectId::from_hex(blake3::hash(bytes).to_hex().as_str())
}

/// A position in the original document.
#[derive(Clone, Debug, PartialEq)]
pub enum Location {
    /// Lines (1-based, inclusive) and bytes (start inclusive, end exclusive) of the extracted text.
    TextRange {
        lines: (u64, u64),
        bytes: (u64, u64),
    },
    /// A page (1-based) and a region on it: left, top, right, bottom.
    PageRegion {
        page: u64,
        region: [f64; 4],
    },
    CellRange {
        sheet: String,
        range: String,
    },
    TimeRange {
        start_ms: u64,
        end_ms: u64,
    },
}

fn pair(value: &Value) -> Option<(u64, u64)> {
    match value.as_array()?.as_slice() {
        [start, end] => Some((start.as_u64()?, end.as_u64()?)),
        _ => None,
    }
}

impl Location {
    fn from_value(value: &Value) -> Option<Self> {
        Some(match value.get("type")?.as_str()? {
            "text_range" => Self::TextRange {
                lines: pair(value.get("lines")?)?,
                bytes: pair(value.get("bytes")?)?,
            },
            "page_region" => {
                let region = value.get("region")?.as_array()?;
                let [left, top, right, bottom] = region.as_slice() else {
                    return None;
                };
                Self::PageRegion {
                    page: value.get("page")?.as_u64()?,
                    region: [
                        left.as_f64()?,
                        top.as_f64()?,
                        right.as_f64()?,
                        bottom.as_f64()?,
                    ],
                }
            }
            "cell_range" => Self::CellRange {
                sheet: value.get("sheet")?.as_str()?.to_owned(),
                range: value.get("range")?.as_str()?.to_owned(),
            },
            "time_range" => Self::TimeRange {
                start_ms: value.get("start_ms")?.as_u64()?,
                end_ms: value.get("end_ms")?.as_u64()?,
            },
            _ => return None,
        })
    }

    fn to_value(&self) -> Value {
        match self {
            Self::TextRange { lines, bytes } => json!({
                "type": "text_range",
                "lines": [lines.0, lines.1],
                "bytes": [bytes.0, bytes.1],
            }),
            Self::PageRegion { page, region } => json!({
                "type": "page_region",
                "page": page,
                "region": region,
            }),
            Self::CellRange { sheet, range } => json!({
                "type": "cell_range",
                "sheet": sheet,
                "range": range,
            }),
            Self::TimeRange { start_ms, end_ms } => json!({
                "type": "time_range",
                "start_ms": start_ms,
                "end_ms": end_ms,
            }),
        }
    }
}

/// `null` is no location; anything else must be one.
fn optional_location(value: Option<&Value>) -> Option<Option<Location>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(value) => Location::from_value(value).map(Some),
    }
}

fn location_value(location: Option<&Location>) -> Value {
    location.map_or(Value::Null, Location::to_value)
}

/// The part a derived part comes from: OCR text of an image, a caption, a transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct Derived {
    pub object: ObjectId,
    pub location: Option<Location>,
}

/// An object at a place in a document. What the content is, is said here and not on the object:
/// the same bytes can be Markdown in one document and plain text in another.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub object: ObjectId,
    /// `text`, `table`, `image`, `chunks`, `embedding` and so on.
    pub kind: String,
    /// `text/plain`, `image/png` and so on.
    pub media_type: String,
    pub location: Option<Location>,
    pub derived_from: Option<Derived>,
    /// The role in the document, such as heading levels, as a JSON object.
    pub attrs: Value,
}

impl Part {
    /// A part read back from a saved item.
    pub fn from_value(value: &Value) -> Option<Self> {
        let derived_from = match value.get("derived_from") {
            None | Some(Value::Null) => None,
            Some(derived) => Some(Derived {
                object: ObjectId::parse(derived.get("object")?.as_str()?)?,
                location: optional_location(derived.get("location"))?,
            }),
        };
        Some(Self {
            object: ObjectId::parse(value.get("object")?.as_str()?)?,
            kind: value.get("kind")?.as_str()?.to_owned(),
            media_type: value.get("media_type")?.as_str()?.to_owned(),
            location: optional_location(value.get("location"))?,
            derived_from,
            attrs: value.get("attrs").cloned().unwrap_or(Value::Null),
        })
    }

    /// The part as an item saves and hashes it.
    pub fn to_value(&self) -> Value {
        json!({
            "object": self.object.as_str(),
            "kind": self.kind,
            "media_type": self.media_type,
            "location": location_value(self.location.as_ref()),
            "derived_from": self.derived_from.as_ref().map(|derived| json!({
                "object": derived.object.as_str(),
                "location": location_value(derived.location.as_ref()),
            })),
            "attrs": self.attrs,
        })
    }
}

/// The parts of an item as its object holds them.
pub fn item_value(parts: &[Part]) -> Value {
    Value::Array(parts.iter().map(Part::to_value).collect())
}

/// The ID of a document's item: all of its parts, in order.
pub fn item_id(parts: &[Part]) -> ItemId {
    ItemId::from_hex(&derive("Dunlin data management item", &item_value(parts)))
}

/// What identifies one module of a process.
///
/// `args` holds only what changes the result, with defaults filled in. The caller has replaced a
/// model reference with its resolved identity and an external file's path with the hash of its
/// content, and has left out secrets, concurrency, time limits and arguments that only steer
/// scanning.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleIdentity {
    pub name: String,
    /// The output compatibility version of the module.
    pub compatibility: u32,
    pub args: Value,
}

/// The modules of a process, in the order they run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Process {
    pub modules: Vec<ModuleIdentity>,
}

impl Process {
    fn to_value(&self) -> Value {
        Value::Array(
            self.modules
                .iter()
                .map(|module| {
                    json!({
                        "name": module.name,
                        "compatibility": module.compatibility,
                        "args": module.args,
                    })
                })
                .collect(),
        )
    }

    /// The ID under which an object remembers what this process made of it. Give only the
    /// modules that apply to the item.
    pub fn id(&self) -> ProcessId {
        ProcessId::from_hex(&derive("Dunlin data management process", &self.to_value()))
    }
}

/// The name of an item within its type: source ID and document ID for documents.
pub type ItemName = Vec<String>;

/// The items of a snapshot in the order they are saved and compared: by their normalized names.
pub fn list_value(items: &[(ItemName, String)]) -> Value {
    let nfc = ComposingNormalizerBorrowed::new_nfc();
    let mut items: Vec<(ItemName, &str)> = items
        .iter()
        .map(|(name, id)| {
            let name = name
                .iter()
                .map(|part| nfc.normalize(part).into_owned())
                .collect();
            (name, id.as_str())
        })
        .collect();
    items.sort();
    Value::Array(
        items
            .into_iter()
            .map(|(name, id)| json!([name, id]))
            .collect(),
    )
}

/// A saved list of items.
pub fn list_from_value(value: &Value) -> Option<Vec<(ItemName, String)>> {
    value
        .as_array()?
        .iter()
        .map(|item| {
            let [name, id] = item.as_array()?.as_slice() else {
                return None;
            };
            let name = name
                .as_array()?
                .iter()
                .map(|part| part.as_str().map(str::to_owned))
                .collect::<Option<ItemName>>()?;
            Some((name, id.as_str()?.to_owned()))
        })
        .collect()
}

/// The digest of a snapshot's type and items.
pub fn list_digest(kind: &str, items: &[(ItemName, String)]) -> ListDigest {
    ListDigest::from_hex(&derive(
        "Dunlin data management list",
        &json!({ "type": kind, "items": list_value(items) }),
    ))
}

/// The key of a stage's result: the type it makes, what its inputs hold and every module of the
/// section. It uses the digests of the inputs, not their serial numbers, so that an input with the
/// same items leads to the same key.
pub fn snapshot_key(kind: &str, inputs: &[ListDigest], process: &Process) -> SnapshotKey {
    SnapshotKey::from_hex(&derive(
        "Dunlin data management snapshot key",
        &json!({
            "type": kind,
            "inputs": inputs.iter().map(ListDigest::as_str).collect::<Vec<_>>(),
            "process": process.to_value(),
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_part() -> Part {
        Part {
            object: object_id(b"# A\nbody\n"),
            kind: "text".into(),
            media_type: "text/markdown".into(),
            location: Some(Location::TextRange {
                lines: (1, 2),
                bytes: (0, 9),
            }),
            derived_from: None,
            attrs: json!({ "headings": ["A"] }),
        }
    }

    fn name(document: &str) -> ItemName {
        vec!["docs".into(), document.into()]
    }

    fn files() -> Vec<(ItemName, String)> {
        vec![
            (name("a.md"), object_id(b"a").to_string()),
            (name("b.md"), object_id(b"b").to_string()),
        ]
    }

    fn chunking(size: u32) -> Process {
        Process {
            modules: vec![
                ModuleIdentity {
                    name: "text.normalize".into(),
                    compatibility: 1,
                    args: json!({ "unicode": "nfkc" }),
                },
                ModuleIdentity {
                    name: "chunk.length".into(),
                    compatibility: 1,
                    args: json!({ "unit": "characters", "size": size, "overlap": 100 }),
                },
            ],
        }
    }

    #[test]
    fn writes_canonical_json() {
        // Keys in code point order, also when they arrive decomposed.
        let value = json!({ "b": 1, "a": [true, null], "e\u{301}": "x", "Z": {} });
        assert_eq!(
            canonical(&value),
            "{\"Z\":{},\"a\":[true,null],\"b\":1,\"\u{e9}\":\"x\"}"
        );
        // Strings in NFC, with the escapes of JSON.
        assert_eq!(canonical(&json!("か\u{3099}\n\"")), "\"が\\n\\\"\"");
        // Integral numbers without a fraction, others in their shortest form.
        assert_eq!(
            canonical(&json!([
                1.0,
                -0.0,
                0.1,
                1.5e300,
                18446744073709551615u64,
                -7
            ])),
            "[1,0,0.1,1.5e+300,18446744073709551615,-7]"
        );
    }

    #[test]
    fn names_an_object_by_the_hash_of_its_bytes() {
        // The BLAKE3 of no bytes, as its specification gives it.
        assert_eq!(
            object_id(b"").as_str(),
            "obj-af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_ne!(object_id(b"a"), object_id(b"b"));
        assert_eq!(
            ObjectId::parse(object_id(b"a").as_str()),
            Some(object_id(b"a"))
        );
        assert_eq!(ObjectId::parse("itm-00"), None);
    }

    #[test]
    fn keeps_fixed_identifiers() {
        // A change here changes every saved identifier: raise FORMAT, or a module's compatibility
        // version when its output changed.
        assert_eq!(
            [
                item_id(&[text_part()]).to_string(),
                chunking(800).id().to_string(),
                list_digest("files", &files()).to_string(),
                snapshot_key("chunks", &[list_digest("files", &files())], &chunking(800))
                    .to_string(),
            ],
            [
                "itm-6fe0524f4adbb45e0a396827af87897de8c7896de623c7015b01aa3af1ed3477",
                "prc-ba94d72b3abf0a88b8b01c8259c62198f975da3f7548b68d26cd61328a8ba348",
                "lst-455195414b5e2c7a55ebf5b5d55227534e7dee1fe6ce92d1296abed75a144c5d",
                "sky-7a01e06cd905bd58dab9be7d9335877ad44487e2ddf38b7d4c3f48dbe990660d",
            ]
        );
        // What is hashed for the list: BLAKE3 of `[1,<this>]` in the domain of lists.
        assert_eq!(
            canonical(&json!({ "type": "files", "items": list_value(&files()) })),
            format!(
                "{{\"items\":[[[\"docs\",\"a.md\"],\"{}\"],[[\"docs\",\"b.md\"],\"{}\"]],\
                 \"type\":\"files\"}}",
                object_id(b"a"),
                object_id(b"b")
            )
        );
    }

    #[test]
    fn digests_a_list_by_its_names_and_contents() {
        let digest = list_digest("files", &files());
        // The order the items arrive in does not matter, and names are compared in NFC.
        let mut reversed = files();
        reversed.reverse();
        assert_eq!(list_digest("files", &reversed), digest);
        let composed = vec![(name("ガイド.md"), "x".to_owned())];
        let decomposed = vec![(name("カ\u{3099}イト\u{3099}.md"), "x".to_owned())];
        assert_eq!(
            list_digest("files", &decomposed),
            list_digest("files", &composed)
        );
        assert_eq!(list_from_value(&list_value(&reversed)), Some(files()));
        // The type, a name, a content and the number of items each change it.
        assert_ne!(list_digest("extracted", &files()), digest);
        let mut other = files();
        other[1].0 = name("c.md");
        assert_ne!(list_digest("files", &other), digest);
        let mut other = files();
        other[1].1 = object_id(b"c").to_string();
        assert_ne!(list_digest("files", &other), digest);
        assert_ne!(list_digest("files", &files()[..1]), digest);
    }

    #[test]
    fn separates_keys_and_kinds() {
        let input = [list_digest("files", &files())];
        let key = snapshot_key("chunks", &input, &chunking(800));
        assert_ne!(snapshot_key("index", &input, &chunking(800)), key);
        assert_ne!(snapshot_key("chunks", &input, &chunking(400)), key);
        assert_ne!(snapshot_key("chunks", &[], &chunking(800)), key);
        assert_ne!(chunking(400).id(), chunking(800).id());
        // The order of modules is part of a process.
        let mut reversed = chunking(800);
        reversed.modules.reverse();
        assert_ne!(reversed.id(), chunking(800).id());
        // Equal values of different kinds do not share an identifier.
        let value = json!([]);
        assert_ne!(
            derive("Dunlin data management item", &value),
            derive("Dunlin data management process", &value)
        );
    }

    #[test]
    fn covers_every_part_of_an_item() {
        let item = item_id(&[text_part()]);
        let mut moved = text_part();
        moved.location = Some(Location::TextRange {
            lines: (1, 3),
            bytes: (0, 9),
        });
        assert_ne!(item_id(&[moved]), item);
        // The same bytes as another media type are another item over the same object.
        let mut plain = text_part();
        plain.media_type = "text/plain".into();
        assert_eq!(plain.object, text_part().object);
        assert_ne!(item_id(&[plain]), item);
        let figure = Part {
            object: object_id(b"png"),
            kind: "image".into(),
            media_type: "image/png".into(),
            location: Some(Location::PageRegion {
                page: 2,
                region: [0.0, 0.25, 1.0, 0.5],
            }),
            derived_from: None,
            attrs: json!({}),
        };
        // A figure that changes while the text stays is a different item.
        let with_figure = item_id(&[text_part(), figure.clone()]);
        assert_ne!(with_figure, item);
        assert_ne!(item_id(&[figure.clone(), text_part()]), with_figure);
        let mut caption = text_part();
        caption.derived_from = Some(Derived {
            object: figure.object.clone(),
            location: figure.location.clone(),
        });
        assert_ne!(item_id(&[caption.clone()]), item);
        // An item reads back as it was saved.
        let saved = item_value(&[text_part(), figure.clone(), caption.clone()]);
        let read: Option<Vec<Part>> = saved
            .as_array()
            .unwrap()
            .iter()
            .map(Part::from_value)
            .collect();
        assert_eq!(read, Some(vec![text_part(), figure, caption]));
    }
}
