//! Scanning source folders and routing files to parsers (spec 6.7).
//!
//! The rules follow the JavaScript `scanFolder` and `resolveParser` this replaces. Files come back
//! unsorted; the caller orders them.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Cancellation;
use crate::glob;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRequest {
    pub path: String,
    #[serde(default)]
    pub exclude: Vec<String>,
    /// The module instance; `source.filesystem` stores its files as source `docs`.
    pub name: String,
    #[serde(default)]
    pub project_root: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScannedFile {
    pub file: String,
    pub object_id: String,
}

#[derive(Debug, Serialize)]
pub struct Scanned {
    pub id: String,
    pub files: Vec<ScannedFile>,
}

pub fn scan(request: &ScanRequest, cancel: &Cancellation) -> Result<Scanned, String> {
    let root = dunce::canonicalize(&request.path)
        .map_err(|error| node_message(&error, "realpath", &request.path))?;
    if !root.is_dir() {
        return Err(format!("Expected a directory for {}", request.name));
    }
    let id = if request.name == "source.filesystem" {
        "docs".to_owned()
    } else {
        request.name.clone()
    };
    let skipped = request
        .project_root
        .as_deref()
        .map(|project| crate::node_path::resolve(project, "Dunlinfile.yaml"));
    let mut found = Vec::new();
    walk(&root, &mut found, cancel)?;
    let root_text = root.to_string_lossy().into_owned();
    let mut files = Vec::new();
    for path in found {
        let file = path.to_string_lossy().into_owned();
        if skipped.as_deref() == Some(file.as_str()) {
            continue;
        }
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| format!("{file} is outside {root_text}"))?;
        let object_id = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if !request
            .exclude
            .iter()
            .any(|pattern| glob::matches(&object_id, pattern))
        {
            files.push(ScannedFile { file, object_id });
        }
    }
    Ok(Scanned { id, files })
}

/// Files under `directory`, skipping names that start with a dot, `node_modules` and links.
fn walk(
    directory: &Path,
    found: &mut Vec<std::path::PathBuf>,
    cancel: &Cancellation,
) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| node_message(&error, "opendir", &directory.to_string_lossy()))?;
    for entry in entries {
        cancel.check().map_err(|_| "Operation aborted".to_owned())?;
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if name.starts_with('.') || name == "node_modules" || kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            walk(&entry.path(), found, cancel)?;
        } else if kind.is_file() {
            found.push(entry.path());
        }
    }
    Ok(())
}

fn node_message(error: &std::io::Error, operation: &str, path: &str) -> String {
    let (code, text) = match error.kind() {
        std::io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        std::io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        std::io::ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        _ => ("EIO", "i/o error"),
    };
    format!("{code}: {text}, {operation} '{path}'")
}

/// A parser module for routing, in matching order.
#[derive(Debug, Clone, Deserialize)]
pub struct Route {
    pub name: String,
    /// `auto`, `text`, `office`, `pdf`, `ocr` or `command`.
    pub category: String,
    #[serde(default, rename = "match")]
    pub patterns: Option<Vec<String>>,
    /// The module reads images and scanned PDFs with OCR.
    #[serde(default)]
    pub ocr: bool,
}

#[derive(Debug, Deserialize)]
pub struct RouteRequest {
    pub routes: Vec<Route>,
    pub files: Vec<RouteFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteFile {
    pub file: String,
    pub object_id: String,
}

/// The parser module and decoding kind of a file, or none when no parser claims it.
#[derive(Debug, Serialize)]
pub struct Routed {
    pub route: Option<String>,
    pub kind: Option<&'static str>,
}

const TEXT: [&str; 19] = [
    ".md",
    ".markdown",
    ".txt",
    ".rs",
    ".js",
    ".mjs",
    ".cjs",
    ".ts",
    ".tsx",
    ".jsx",
    ".py",
    ".go",
    ".java",
    ".cs",
    ".json",
    ".toml",
    ".yaml",
    ".yml",
    ".sql",
];
const HTML: [&str; 2] = [".html", ".htm"];
const OFFICE: [&str; 3] = [".docx", ".xlsx", ".pptx"];
const IMAGES: [&str; 6] = [".png", ".jpg", ".jpeg", ".webp", ".bmp", ".gif"];

/// `path.extname(path).toLowerCase()`.
fn extension(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(0) | None => String::new(),
        Some(index) => name[index..].to_lowercase(),
    }
}

fn claims(category: &str, extension: &str) -> bool {
    let text = TEXT.contains(&extension) || HTML.contains(&extension);
    let pdf = extension == ".pdf";
    match category {
        "text" => text,
        "office" => OFFICE.contains(&extension),
        "pdf" => pdf,
        "ocr" => pdf || IMAGES.contains(&extension),
        "command" => true,
        "auto" => text || OFFICE.contains(&extension) || pdf,
        _ => false,
    }
}

pub fn route(request: &RouteRequest) -> Vec<Routed> {
    request
        .files
        .iter()
        .map(|file| {
            let extension = extension(&file.file);
            let route = request.routes.iter().find(|route| {
                route.patterns.as_ref().is_none_or(|patterns| {
                    patterns
                        .iter()
                        .any(|pattern| glob::matches(&file.object_id, pattern))
                }) && (claims(&route.category, &extension)
                    || (route.category == "auto" && route.ocr && claims("ocr", &extension)))
            });
            match route {
                None => Routed {
                    route: None,
                    kind: None,
                },
                Some(route) => {
                    let kind = if route.category == "command" {
                        "command"
                    } else if route.ocr && claims("ocr", &extension) {
                        "ocr"
                    } else if extension == ".pdf" {
                        "pdf"
                    } else if HTML.contains(&extension.as_str()) {
                        "html"
                    } else if OFFICE.contains(&extension.as_str()) {
                        "office"
                    } else {
                        "text"
                    };
                    Routed {
                        route: Some(route.name.clone()),
                        kind: Some(kind),
                    }
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_and_routes() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "a.md",
            "sub/b.pdf",
            ".hidden/c.md",
            "node_modules/d.md",
            "sub/e.png",
            "x.bin",
            "skip/f.md",
        ] {
            let path = root.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        let request = ScanRequest {
            path: root.path().to_string_lossy().into_owned(),
            exclude: vec!["skip/**".into()],
            name: "source.filesystem".into(),
            project_root: None,
        };
        let mut scanned = scan(&request, &Cancellation::default()).unwrap();
        scanned.files.sort_by(|a, b| a.object_id.cmp(&b.object_id));
        let ids: Vec<_> = scanned
            .files
            .iter()
            .map(|file| file.object_id.as_str())
            .collect();
        assert_eq!(scanned.id, "docs");
        assert_eq!(ids, ["a.md", "sub/b.pdf", "sub/e.png", "x.bin"]);
        let routes = RouteRequest {
            routes: vec![
                Route {
                    name: "loader.ocr".into(),
                    category: "ocr".into(),
                    patterns: Some(vec!["sub/*.png".into()]),
                    ocr: true,
                },
                Route {
                    name: "loader.auto".into(),
                    category: "auto".into(),
                    patterns: None,
                    ocr: false,
                },
            ],
            files: scanned
                .files
                .iter()
                .map(|file| RouteFile {
                    file: file.file.clone(),
                    object_id: file.object_id.clone(),
                })
                .collect(),
        };
        let routed: Vec<_> = route(&routes)
            .into_iter()
            .map(|item| (item.route, item.kind))
            .collect();
        assert_eq!(routed[0], (Some("loader.auto".into()), Some("text")));
        assert_eq!(routed[1], (Some("loader.auto".into()), Some("pdf")));
        assert_eq!(routed[2], (Some("loader.ocr".into()), Some("ocr")));
        assert_eq!(routed[3], (None, None));
    }
}
