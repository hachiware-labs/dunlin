//! Office documents (.docx, .xlsx, .pptx) as Markdown (spec 6.7 and 28.13 step 6a), with the
//! docling.rs crate at the version of the npm package the JavaScript parser called
//! (`convertAsync(..., { to: 'markdown', imageMode: 'placeholder' })`).

use docling_core::ImageMode;

use crate::config_value::ConfigValue;
use crate::registry::js_space;

/// `{ content, format: "markdown", parser: { tool: "docling-rs" } }` for an Office document.
pub fn parse(bytes: &[u8], name: &str) -> Result<ConfigValue, String> {
    let converter = docling::DocumentConverter::new();
    let source = converter
        .load_named_bytes(name, bytes.to_vec(), None)
        .map_err(|error| error.to_string())?;
    let result = converter
        .convert(source)
        .map_err(|error| error.to_string())?;
    if result.status != docling::ConversionStatus::Success {
        return Err(format!(
            "Docling conversion did not complete ({})",
            result.status.as_str()
        ));
    }
    let (content, _) = result
        .document
        .export_to_markdown_with_images(ImageMode::Placeholder, "");
    if content.trim_matches(js_space).is_empty() {
        return Err("Document has no text.".into());
    }
    Ok(ConfigValue::Object(vec![
        ("content".into(), ConfigValue::String(content)),
        ("format".into(), ConfigValue::String("markdown".into())),
        (
            "parser".into(),
            ConfigValue::Object(vec![(
                "tool".into(),
                ConfigValue::String("docling-rs".into()),
            )]),
        ),
    ]))
}
