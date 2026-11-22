//! OCR with oar-ocr: PP-OCR models on ONNX Runtime, on the CPU (spec 4.1, 28.9 step 2a).
//!
//! `nest add oar-ocr` prepares ONNX Runtime and the models in a folder with `oar-ocr.json`,
//! which names the files relative to that folder. ONNX Runtime is loaded from there when the first
//! file needs OCR, so the package does not carry it.
//!
//! PDF pages keep their text layer. Pages without text and images are rendered to at most 1540
//! pixels on the longer side and recognized line by line. Each line becomes a location with its
//! page and a box scaled to 0..1000, as the other OCR tool reports them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use oar_ocr::domain::tasks::TextDetectionConfig;
use oar_ocr::oarocr::{OAROCR, OAROCRBuilder};
use serde::Deserialize;

use crate::config_value::ConfigValue;

/// The longer side of a page or image given to OCR, in pixels.
const SIDE: u32 = 1540;
const MAX_PAGES: usize = 128;

#[derive(Debug, Deserialize)]
struct Manifest {
    runtime: String,
    detection: String,
    recognition: String,
    dictionary: String,
}

type Engine = (PathBuf, Arc<OAROCR>);

fn engines() -> &'static Mutex<Option<Engine>> {
    static ENGINE: OnceLock<Mutex<Option<Engine>>> = OnceLock::new();
    ENGINE.get_or_init(Default::default)
}

/// Loads ONNX Runtime once per process. Later calls must name the same library.
fn load_runtime(path: &Path) -> Result<(), String> {
    static RUNTIME: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    let loaded = RUNTIME.get_or_init(|| {
        ort::init_from(path)
            .map_err(|error| {
                format!(
                    "Could not load ONNX Runtime from {}: {error}",
                    path.display()
                )
            })?
            .commit();
        Ok(path.to_path_buf())
    });
    match loaded {
        Ok(loaded) if loaded == path => Ok(()),
        Ok(loaded) => Err(format!(
            "ONNX Runtime is already loaded from {}",
            loaded.display()
        )),
        Err(error) => Err(error.clone()),
    }
}

fn not_prepared(detail: impl std::fmt::Display) -> String {
    format!("oar-ocr is not prepared ({detail}). Run nest add ocr.")
}

fn engine(home: &Path) -> Result<Arc<OAROCR>, String> {
    let mut cached = engines()
        .lock()
        .map_err(|_| "The OCR engine is unavailable".to_owned())?;
    if let Some((path, engine)) = cached.as_ref()
        && path == home
    {
        return Ok(engine.clone());
    }
    let text = std::fs::read_to_string(home.join("oar-ocr.json")).map_err(not_prepared)?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(not_prepared)?;
    load_runtime(&home.join(&manifest.runtime))?;
    // The PP-OCRv6 detection settings for the small and medium models.
    let detection = TextDetectionConfig {
        score_threshold: 0.2,
        box_threshold: 0.45,
        unclip_ratio: 1.4,
        max_candidates: 3000,
        ..TextDetectionConfig::default()
    };
    let built = OAROCRBuilder::new(
        home.join(&manifest.detection),
        home.join(&manifest.recognition),
        home.join(&manifest.dictionary),
    )
    .text_detection_config(detection)
    .build()
    .map_err(|error| format!("Could not start oar-ocr: {error}"))?;
    let built = Arc::new(built);
    *cached = Some((home.to_path_buf(), built.clone()));
    Ok(built)
}

fn text(value: &str) -> ConfigValue {
    ConfigValue::String(value.to_owned())
}

/// Recognized lines of one image: text and a box scaled to 0..1000, in reading order.
fn recognize_lines(
    engine: &OAROCR,
    image: image::RgbImage,
) -> Result<Vec<(String, [f64; 4])>, String> {
    let (width, height) = (image.width().max(1) as f32, image.height().max(1) as f32);
    let results = engine
        .predict(vec![image])
        .map_err(|error| format!("OCR failed: {error}"))?;
    let mut lines = Vec::new();
    for region in results.iter().flat_map(|result| &result.text_regions) {
        let Some(line) = region
            .text
            .as_deref()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        else {
            continue;
        };
        let points = &region.bounding_box.points;
        let fold = |pick: fn(&oar_ocr::processors::Point) -> f32, min: bool| {
            points
                .iter()
                .map(pick)
                .fold(if min { f32::MAX } else { f32::MIN }, |a, b| {
                    if min { a.min(b) } else { a.max(b) }
                })
        };
        let scale =
            |value: f32, size: f32| f64::from((1000.0 * value / size).round().clamp(0.0, 1000.0));
        let bbox = [
            scale(fold(|point| point.x, true), width),
            scale(fold(|point| point.y, true), height),
            scale(fold(|point| point.x, false), width),
            scale(fold(|point| point.y, false), height),
        ];
        lines.push((line.to_owned(), bbox));
    }
    Ok(lines)
}

pub fn fit(image: image::DynamicImage) -> image::RgbImage {
    let longer = image.width().max(image.height());
    let image = if longer > SIDE {
        image.resize(SIDE, SIDE, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    image.to_rgb8()
}

/// A PDF page: its text layer, or the rendered image when it has none.
type Page = Result<String, image::RgbImage>;

/// Recognizes one page image: blocks of text with boxes scaled to 0..1000, in reading order.
pub type Recognize<'a> = dyn FnMut(image::RgbImage) -> Result<Vec<(String, [f64; 4])>, String> + 'a;

/// Reads a scanned PDF or an image with an OCR tool. PDF pages keep their text layer; the other
/// pages are rendered and recognized. Returns `{ content, format: "text", locations, parser }`.
pub fn read(
    bytes: &[u8],
    check: &dyn Fn() -> bool,
    recognize: &mut Recognize,
    parser: ConfigValue,
) -> Result<ConfigValue, String> {
    let mut lines: Vec<String> = Vec::new();
    let mut locations = Vec::new();
    let mut add = |page: usize, block: &str, bbox: Option<[f64; 4]>| {
        let start = lines.len() + 1;
        lines.extend(
            block
                .split('\n')
                .map(|line| line.trim_end_matches('\r').to_owned()),
        );
        let mut location = vec![
            ("page".to_owned(), ConfigValue::Number(page as f64)),
            (
                "range".to_owned(),
                ConfigValue::Object(vec![
                    ("startLine".into(), ConfigValue::Number(start as f64)),
                    ("endLine".into(), ConfigValue::Number(lines.len() as f64)),
                ]),
            ),
        ];
        if let Some(bbox) = bbox {
            location.push((
                "bbox".into(),
                ConfigValue::Array(bbox.into_iter().map(ConfigValue::Number).collect()),
            ));
        }
        locations.push(ConfigValue::Object(location));
    };
    if bytes.starts_with(b"%PDF-") {
        let pages = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<Vec<(usize, Page)>, String> {
                let document = pdf_oxide::PdfDocument::from_bytes(bytes.to_vec())
                    .map_err(|error| format!("Could not read the PDF: {error}"))?;
                let count = document
                    .page_count()
                    .map_err(|error| format!("Could not read the PDF pages: {error}"))?;
                if count > MAX_PAGES {
                    return Err(format!("OCR supports at most {MAX_PAGES} pages per PDF"));
                }
                let mut pages = Vec::new();
                for index in 0..count {
                    if !check() {
                        return Err("Aborted".into());
                    }
                    let layer = document.extract_text(index).unwrap_or_default();
                    if !layer.trim().is_empty() {
                        pages.push((index + 1, Ok(layer)));
                        continue;
                    }
                    let options = pdf_oxide::rendering::RenderOptions::default();
                    let rendered = pdf_oxide::rendering::render_page_fit(
                        &document, index, SIDE, SIDE, &options,
                    )
                    .map_err(|error| format!("Could not render page {}: {error}", index + 1))?;
                    let image = image::load_from_memory(&rendered.data)
                        .map_err(|error| format!("Could not render page {}: {error}", index + 1))?;
                    pages.push((index + 1, Err(fit(image))));
                }
                Ok(pages)
            },
        ))
        .map_err(|_| "The PDF reader failed on this file".to_owned())??;
        for (page, content) in pages {
            if !check() {
                return Err("Aborted".into());
            }
            match content {
                Ok(layer) => add(page, layer.trim(), None),
                Err(image) => {
                    for (block, bbox) in recognize(image)? {
                        if !block.trim().is_empty() {
                            add(page, block.trim(), Some(bbox));
                        }
                    }
                }
            }
        }
    } else {
        let image = image::load_from_memory(bytes)
            .map_err(|error| format!("Could not read the image: {error}"))?;
        for (block, bbox) in recognize(fit(image))? {
            if !block.trim().is_empty() {
                add(1, block.trim(), Some(bbox));
            }
        }
    }
    if lines.iter().all(|line| line.trim().is_empty()) {
        return Err("OCR did not find any document text".into());
    }
    Ok(ConfigValue::Object(vec![
        ("content".into(), text(&lines.join("\n"))),
        ("format".into(), text("text")),
        ("locations".into(), ConfigValue::Array(locations)),
        ("parser".into(), parser),
    ]))
}

/// Reads a scanned PDF or an image with oar-ocr from the folder `nest add oar-ocr` prepared.
pub fn parse(
    home: &str,
    version: &str,
    bytes: &[u8],
    check: &dyn Fn() -> bool,
) -> Result<ConfigValue, String> {
    let home = Path::new(home);
    let mut loaded: Option<Arc<OAROCR>> = None;
    let mut recognize = |image: image::RgbImage| {
        let engine = match &loaded {
            Some(engine) => Arc::clone(engine),
            None => loaded.insert(engine(home)?).clone(),
        };
        recognize_lines(&engine, image)
    };
    let parser = ConfigValue::Object(vec![
        ("tool".into(), text("oar-ocr")),
        ("version".into(), text(version)),
    ]);
    read(bytes, check, &mut recognize, parser)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_missing_setup() {
        let home = tempfile::tempdir().unwrap();
        let image = image::RgbImage::from_pixel(8, 8, image::Rgb([255, 255, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let error = parse(&home.path().to_string_lossy(), "1", &png, &|| true).unwrap_err();
        assert!(error.contains("Run nest add ocr"), "{error}");
        let text_layer = pdf_oxide::api::Pdf::from_text("Text layer")
            .unwrap()
            .to_bytes()
            .unwrap();
        let parsed = parse(&home.path().to_string_lossy(), "1", &text_layer, &|| true).unwrap();
        assert!(
            parsed
                .get("content")
                .and_then(ConfigValue::as_str)
                .unwrap()
                .contains("Text layer")
        );
    }
}
