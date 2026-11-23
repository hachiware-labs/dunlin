//! Sources given as web addresses: `ingress https://...`, from the CLI or the screen.
//!
//! The page or file is downloaded once and kept with the raw sources: in `raw/<host>/<path>` when
//! the project has a `raw/` folder, else beside uploaded files (`artifacts/uploads/<sha256>/`).
//! Its name has the extension that tells the loader its format, and it is then read like any
//! other file. Each address is one source: fetching it again saves a new version of the source.

use std::time::Duration;

use crate::node_path;
use crate::source_load::sha256_hex;
use crate::transform_stage::StageError;
use crate::wiki_project::{coded, fs_error};

/// The largest download: the limit of files the loader converts (PDF, Office, HTML).
const LIMIT: u64 = 16 * 1024 * 1024;

/// A downloaded source: the saved file and the address it stands for.
pub(crate) struct Fetched {
    pub file: String,
    pub url: String,
}

/// The folder of raw sources: the project's `raw/`, when it has one.
pub(crate) fn raw_folder(root: &str) -> Option<String> {
    let path = node_path::join(root, &["raw"]);
    std::path::Path::new(&path).is_dir().then_some(path)
}

/// Keeps the bytes of a source and returns the file. With a `raw/` folder the file is
/// `raw/<folders>/<name>`; a file of that name with other content stays as it is, and the new one
/// gets the start of its SHA-256 before its extension. Without it, the file is
/// `artifacts/uploads/<sha256>/<name>`.
pub(crate) fn keep_source(
    root: &str,
    artifacts: &str,
    folders: &[String],
    name: &str,
    bytes: &[u8],
) -> Result<String, StageError> {
    let digest = sha256_hex(bytes);
    let (folder, names) = match raw_folder(root) {
        Some(raw) => {
            let parts: Vec<&str> = folders.iter().map(String::as_str).collect();
            let (stem, extension) = match name.rfind('.').filter(|&at| at > 0) {
                Some(at) => (&name[..at], &name[at..]),
                None => (name, ""),
            };
            (
                node_path::join(&raw, &parts),
                vec![
                    name.to_owned(),
                    format!("{stem}-{}{extension}", &digest[..8]),
                ],
            )
        }
        None => (
            node_path::join(artifacts, &["uploads", &digest]),
            vec![name.to_owned()],
        ),
    };
    std::fs::create_dir_all(&folder).map_err(|error| fs_error("mkdir", &folder, &error))?;
    for name in names {
        let file = node_path::join(&folder, &[&name]);
        match std::fs::read(&file) {
            Ok(existing) if existing == bytes => return Ok(file),
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::write(&file, bytes).map_err(|error| fs_error("write", &file, &error))?;
                return Ok(file);
            }
            Err(error) => return Err(fs_error("open", &file, &error)),
        }
    }
    Err(coded(
        4,
        "SOURCE_EXISTS",
        format!("Another file uses the name of this source in {folder}."),
    ))
}

/// Whether an `ingress` input is a web address rather than a path.
pub(crate) fn is_web_address(input: &str) -> bool {
    let lower = input.trim_start().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The address as it is kept: parsed, without its fragment. Only http and https are accepted.
pub(crate) fn normalize(input: &str) -> Result<url::Url, StageError> {
    let invalid = || {
        coded(
            2,
            "INVALID_URL",
            format!("Expected an http or https address: {input}"),
        )
    };
    let mut url = url::Url::parse(input.trim()).map_err(|_| invalid())?;
    if !["http", "https"].contains(&url.scheme()) || url.host_str().is_none_or(str::is_empty) {
        return Err(invalid());
    }
    url.set_fragment(None);
    Ok(url)
}

/// Downloads `input` and keeps it with the raw sources, in a folder of its host and path.
pub(crate) fn fetch(input: &str, root: &str, artifacts: &str) -> Result<Fetched, StageError> {
    let url = normalize(input)?;
    let failed = |message: String| coded(1, "FETCH_FAILED", format!("{url}: {message}"));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .user_agent(concat!("knowledge-nest/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut response = agent
        .get(url.as_str())
        .header(
            "accept",
            "text/html,application/xhtml+xml,application/pdf,text/markdown,text/plain;q=0.9,*/*;q=0.8",
        )
        .call()
        .map_err(|error| failed(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(failed(format!("HTTP {}", status.as_u16())));
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let landed = {
        use ureq::ResponseExt;
        url::Url::parse(&response.get_uri().to_string()).unwrap_or_else(|_| url.clone())
    };
    let bytes = response
        .body_mut()
        .with_config()
        .limit(LIMIT)
        .read_to_vec()
        .map_err(|error| match error {
            ureq::Error::BodyExceedsLimit(_) => {
                failed(format!("larger than {} MiB", LIMIT / 1024 / 1024))
            }
            error => failed(error.to_string()),
        })?;
    let media = media_type(&content_type);
    let name = file_name(&landed, &media);
    let bytes = if is_text(&media, &name) {
        to_utf8(bytes, &content_type, &media)
    } else {
        bytes
    };
    let file = keep_source(root, artifacts, &raw_folders(&landed), &name, &bytes)?;
    Ok(Fetched {
        file,
        url: url.to_string(),
    })
}

/// The folders a web page is kept in under `raw/`: its host and the folders of its path, as
/// `example.com/manuals`.
fn raw_folders(url: &url::Url) -> Vec<String> {
    let mut folders = vec![sanitize(url.host_str().unwrap_or("web"))];
    if let Some(segments) = url.path_segments() {
        let segments: Vec<&str> = segments.collect();
        for segment in &segments[..segments.len().saturating_sub(1)] {
            let decoded = percent_encoding::percent_decode_str(segment)
                .decode_utf8_lossy()
                .into_owned();
            let part = sanitize(&decoded);
            if !part.is_empty() {
                folders.push(part);
            }
        }
    }
    folders
}

/// Whether a file of `raw/`, by its path from there, is where a web address was kept: the
/// folders of the address and its name, with an extension and the start of a SHA-256 or not.
pub(crate) fn kept_at(address: &str, path: &str) -> bool {
    let Ok(url) = url::Url::parse(address) else {
        return false;
    };
    let name = url
        .path_segments()
        .and_then(|mut segments| segments.next_back().map(str::to_owned))
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            percent_encoding::percent_decode_str(&segment)
                .decode_utf8_lossy()
                .into_owned()
        });
    let name = sanitize(&name.unwrap_or_else(|| url.host_str().unwrap_or("page").to_owned()));
    let mut stem = raw_folders(&url).join("/");
    stem.push('/');
    stem.push_str(&name);
    let Some(rest) = path.strip_prefix(&stem) else {
        return false;
    };
    let rest = match rest.strip_prefix('-') {
        Some(after)
            if after.len() >= 8 && after[..8].bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            &after[8..]
        }
        _ => rest,
    };
    rest.is_empty() || (rest.starts_with('.') && !rest.contains('/'))
}

/// The media type of a `content-type` header, in lower case and without parameters.
fn media_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// The extensions the loader reads a media type with; the first is the one to add.
fn extensions(media: &str) -> &'static [&'static str] {
    match media {
        "text/html" | "application/xhtml+xml" => &[".html", ".htm"],
        "application/pdf" => &[".pdf"],
        "text/markdown" | "text/x-markdown" => &[".md", ".markdown"],
        "text/plain" => &[".txt", ".md", ".markdown"],
        "application/json" => &[".json"],
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => &[".docx"],
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => &[".xlsx"],
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => &[".pptx"],
        "image/png" => &[".png"],
        "image/jpeg" => &[".jpg", ".jpeg"],
        "image/webp" => &[".webp"],
        "image/gif" => &[".gif"],
        "image/bmp" => &[".bmp"],
        _ => &[],
    }
}

/// A file name for the address: its last path segment, or its host, with the extension of its
/// media type when the name lacks one the loader would read it with.
fn file_name(url: &url::Url, media: &str) -> String {
    let segment = url
        .path_segments()
        .and_then(|mut segments| segments.next_back().map(str::to_owned))
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            percent_encoding::percent_decode_str(&segment)
                .decode_utf8_lossy()
                .into_owned()
        });
    let mut name =
        sanitize(&segment.unwrap_or_else(|| url.host_str().unwrap_or("page").to_owned()));
    let lower = name.to_ascii_lowercase();
    let wanted = extensions(media);
    if let Some(first) = wanted.first()
        && !wanted.iter().any(|extension| lower.ends_with(extension))
    {
        name.push_str(first);
    }
    name
}

/// A name that is safe as a file name on every platform.
fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    out = out
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' '])
        .to_owned();
    if out.chars().count() > 100 {
        out = out.chars().take(100).collect();
    }
    let stem = out
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let reserved = ["con", "prn", "aux", "nul"].contains(&stem.as_str())
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if out.is_empty() || reserved {
        out.insert_str(0, "page");
    }
    out
}

/// Whether the loader reads the file as text, which must be UTF-8.
fn is_text(media: &str, name: &str) -> bool {
    media.starts_with("text/")
        || media == "application/xhtml+xml"
        || media == "application/json"
        || [".html", ".htm", ".md", ".markdown", ".txt"]
            .iter()
            .any(|extension| name.to_ascii_lowercase().ends_with(extension))
}

/// Text in the character set the server or the page names, as UTF-8 (pages in Shift_JIS or
/// EUC-JP are common). Text whose character set is not known is kept as it is.
fn to_utf8(bytes: Vec<u8>, content_type: &str, media: &str) -> Vec<u8> {
    let label = charset(content_type).or_else(|| {
        (media == "text/html" || media == "application/xhtml+xml" || media.is_empty())
            .then(|| meta_charset(&bytes))
            .flatten()
    });
    let encoding = match label.and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
    {
        Some(encoding) => encoding,
        None => match encoding_rs::Encoding::for_bom(&bytes) {
            Some((encoding, _)) => encoding,
            None => return bytes,
        },
    };
    if encoding == encoding_rs::UTF_8 && std::str::from_utf8(&bytes).is_ok() {
        return bytes;
    }
    let (text, _, _) = encoding.decode(&bytes);
    text.into_owned().into_bytes()
}

/// The `charset` parameter of a `content-type` header.
fn charset(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        (name.trim().eq_ignore_ascii_case("charset"))
            .then(|| value.trim().trim_matches(['"', '\'']).to_owned())
            .filter(|value| !value.is_empty())
    })
}

/// The character set a page declares in its first bytes: `<meta charset="...">`, or the
/// `content` of `<meta http-equiv="content-type">`.
fn meta_charset(bytes: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_ascii_lowercase();
    let found = head.find("charset=")?;
    let rest = head[found + "charset=".len()..].trim_start_matches(['"', '\'', ' ']);
    let label: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || "-_.:".contains(*c))
        .collect();
    (!label.is_empty()).then_some(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(address: &str, media: &str) -> String {
        file_name(&url::Url::parse(address).unwrap(), media)
    }

    #[test]
    fn names_files_by_their_address_and_media_type() {
        assert_eq!(
            name("https://example.com/docs/guide", "text/html"),
            "guide.html"
        );
        assert_eq!(
            name("https://example.com/", "text/html"),
            "example.com.html"
        );
        assert_eq!(
            name("https://example.com/a/b.pdf", "application/pdf"),
            "b.pdf"
        );
        assert_eq!(name("https://example.com/a/b.htm", "text/html"), "b.htm");
        assert_eq!(
            name("https://example.com/index.php", "text/html"),
            "index.php.html"
        );
        assert_eq!(
            name("https://example.com/notes.md", "text/plain"),
            "notes.md"
        );
        assert_eq!(
            name("https://example.com/%E6%89%8B%E9%A0%86", "text/html"),
            "手順.html"
        );
        assert_eq!(
            name("https://example.com/a/file.bin", "application/octet-stream"),
            "file.bin"
        );
        assert_eq!(name("https://example.com/con", "text/plain"), "pagecon.txt");
    }

    #[test]
    fn accepts_only_web_addresses() {
        assert!(is_web_address("https://example.com/"));
        assert!(is_web_address("HTTP://example.com/"));
        assert!(!is_web_address("./manuals"));
        assert!(!is_web_address("file:///etc/passwd"));
        assert!(normalize("ftp://example.com/").is_err());
        assert_eq!(
            normalize("https://example.com/a#part").unwrap().as_str(),
            "https://example.com/a"
        );
    }

    #[test]
    fn decodes_pages_in_their_declared_character_set() {
        let (shift_jis, _, _) = encoding_rs::SHIFT_JIS.encode("<meta charset=\"shift_jis\">点検");
        let text = to_utf8(shift_jis.into_owned(), "text/html", "text/html");
        assert_eq!(
            String::from_utf8(text).unwrap(),
            "<meta charset=\"shift_jis\">点検"
        );
        let (euc, _, _) = encoding_rs::EUC_JP.encode("交換");
        let text = to_utf8(euc.into_owned(), "text/plain; charset=EUC-JP", "text/plain");
        assert_eq!(String::from_utf8(text).unwrap(), "交換");
        assert_eq!(
            to_utf8(b"plain".to_vec(), "text/plain", "text/plain"),
            b"plain"
        );
    }
}
