//! The text layer of PDFs, read with `pdf_oxide` (spec 28.9 step 2a).
//!
//! The result has the shape the JavaScript pdfjs reader returned: pages joined by a blank line,
//! with one location per page that has text.

use crate::config_value::ConfigValue;

/// The parser name and version. Both are part of the extraction cache key, so changing the
/// pinned version reads PDFs again.
pub const PARSER: &str = "pdf_oxide";
pub const VERSION: &str = "0.3.78";

fn text(value: &str) -> ConfigValue {
    ConfigValue::String(value.to_owned())
}

/// Returns `{ content, format: "text", locations, parser }`, or an error message.
pub fn parse(bytes: &[u8], check: &dyn Fn() -> bool) -> Result<ConfigValue, String> {
    let bytes = bytes.to_vec();
    // A malformed PDF must not take the build down with it.
    let pages = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<Vec<(usize, String)>, String> {
            let document = pdf_oxide::PdfDocument::from_bytes(bytes)
                .map_err(|error| format!("Could not read the PDF: {error}"))?;
            let count = document
                .page_count()
                .map_err(|error| format!("Could not read the PDF pages: {error}"))?;
            let mut pages = Vec::new();
            for index in 0..count {
                if !check() {
                    return Err("Aborted".into());
                }
                let page = document
                    .extract_text(index)
                    .map_err(|error| format!("Could not read page {}: {error}", index + 1))?;
                pages.push((index + 1, page));
            }
            Ok(pages)
        },
    ))
    .map_err(|_| "The PDF reader failed on this file".to_owned())??;
    let mut content = String::new();
    let mut locations = Vec::new();
    let mut start_line = 1usize;
    for (number, page) in pages {
        let page = page.trim();
        if page.is_empty() {
            continue;
        }
        if !content.is_empty() {
            content.push_str("\n\n");
        }
        content.push_str(page);
        let end_line = start_line + page.split('\n').count() - 1;
        locations.push(ConfigValue::Object(vec![
            ("page".into(), ConfigValue::Number(number as f64)),
            (
                "range".into(),
                ConfigValue::Object(vec![
                    ("startLine".into(), ConfigValue::Number(start_line as f64)),
                    ("endLine".into(), ConfigValue::Number(end_line as f64)),
                ]),
            ),
        ]));
        start_line = end_line + 2;
    }
    Ok(ConfigValue::Object(vec![
        ("content".into(), text(&content)),
        ("format".into(), text("text")),
        ("locations".into(), ConfigValue::Array(locations)),
        (
            "parser".into(),
            ConfigValue::Object(vec![
                ("tool".into(), text(PARSER)),
                ("version".into(), text(VERSION)),
            ]),
        ),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pages_and_locations() {
        let mut pdf = pdf_oxide::api::Pdf::from_text("Hello PDF\nSecond line").unwrap();
        let bytes = pdf.to_bytes().unwrap();
        let parsed = parse(&bytes, &|| true).unwrap();
        let content = parsed.get("content").and_then(ConfigValue::as_str).unwrap();
        assert!(content.contains("Hello PDF"), "{content}");
        let Some(ConfigValue::Array(locations)) = parsed.get("locations") else {
            panic!("no locations")
        };
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].get("page"), Some(&ConfigValue::Number(1.0)));
        assert!(parse(b"not a pdf", &|| true).is_err());
        assert_eq!(parse(&bytes, &|| false).unwrap_err(), "Aborted");
    }
}
