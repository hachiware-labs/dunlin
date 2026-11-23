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
    /// How it was taken: the browser and its version, as `Chrome/131.0.6778.86`, or `http`.
    pub with: String,
    /// What the user should know about how it was taken.
    pub warning: Option<String>,
    /// How the page was scrolled for more, when it was.
    pub scrolled: Option<crate::wiki_browser::Scrolled>,
}

/// How web pages are taken: `browser` opens them in the Chrome or Edge of the machine, in the
/// profile of nest, and keeps them as drawn; `http` downloads them as the server sends them.
pub(crate) struct Method<'a> {
    pub method: &'a str,
    /// `auto`, `chrome`, `edge` or the path of a browser.
    pub browser: &'a str,
    /// The folders the user settings are found from: the current one and the home.
    pub cwd: &'a str,
    pub home: &'a str,
    /// How many times a page is scrolled for more: 0 for none.
    pub scroll: u32,
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
    match raw_folder(root) {
        Some(raw) => {
            let parts: Vec<&str> = folders.iter().map(String::as_str).collect();
            keep_in(&node_path::join(&raw, &parts), name, bytes)
        }
        None => {
            let folder = node_path::join(artifacts, &["uploads", &sha256_hex(bytes)]);
            write_once(&folder, vec![name.to_owned()], bytes)
        }
    }
}

/// Keeps a file in a folder under its name, never over another: the same bytes under that name
/// are the file already, and other bytes get the first 8 characters of their SHA-256 before the
/// extension. Returns the path of the file.
pub(crate) fn keep_in(folder: &str, name: &str, bytes: &[u8]) -> Result<String, StageError> {
    let digest = sha256_hex(bytes);
    let (stem, extension) = match name.rfind('.').filter(|&at| at > 0) {
        Some(at) => (&name[..at], &name[at..]),
        None => (name, ""),
    };
    let names = vec![
        name.to_owned(),
        format!("{stem}-{}{extension}", &digest[..8]),
    ];
    write_once(folder, names, bytes)
}

/// Writes the bytes under the first of the names that is free or holds them already.
fn write_once(folder: &str, names: Vec<String>, bytes: &[u8]) -> Result<String, StageError> {
    std::fs::create_dir_all(folder).map_err(|error| fs_error("mkdir", folder, &error))?;
    for name in names {
        let file = node_path::join(folder, &[&name]);
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
pub(crate) fn fetch(
    input: &str,
    root: &str,
    artifacts: &str,
    how: &Method<'_>,
) -> Result<Fetched, StageError> {
    let url = normalize(input)?;
    if how.method != "browser" {
        return download(url, root, artifacts, None);
    }
    // Pages are read only in the profile of nest, where the user has signed in; without it they
    // are not read at all, so that a page never comes in as a stranger sees it.
    let browsers = crate::wiki_browser::installed(how.browser);
    if browsers.is_empty() {
        return Err(coded(
            3,
            "BROWSER_NOT_FOUND",
            if matches!(how.browser.trim(), "" | "auto") {
                "No Chrome or Edge was found. Install one of them, then run nest browser.".to_owned()
            } else {
                format!(
                    "The browser {} was not found. Install Chrome or Edge, or set the browser of loader.auto to auto.",
                    how.browser
                )
            },
        ));
    }
    let profiles = crate::wiki_browser::profiles(how.cwd, how.home);
    let Some((browser, profile)) = browsers.into_iter().find_map(|browser| {
        let profile = profiles.join(browser.key());
        profile.is_dir().then_some((browser, profile))
    }) else {
        return Err(coded(
            3,
            "BROWSER_NOT_READY",
            format!(
                "nest has no browser profile yet, so {url} was not read. Run nest browser, sign in to the sites to read, close the browser, and run this again."
            ),
        ));
    };
    if crate::wiki_browser::in_use(&profile) {
        return Err(coded(
            3,
            "BROWSER_BUSY",
            format!(
                "The browser profile of nest is open, so {url} was not read. Close the browser of nest browser, or wait for the other ingest, and run this again."
            ),
        ));
    }
    let opened = crate::wiki_browser::open(&browser, &profile, url.as_str(), how.scroll)
        .map_err(|message| coded(1, "FETCH_FAILED", format!("{url}: {message}")))?;
    let crate::wiki_browser::Opened::Page(page) = opened else {
        // A PDF or another file is taken as the server sends it.
        return download(url, root, artifacts, None);
    };
    let landed = url::Url::parse(&page.url)
        .ok()
        .filter(|landed| ["http", "https"].contains(&landed.scheme()))
        .unwrap_or_else(|| url.clone());
    if signed_out(&url, &landed) {
        return Err(coded(
            3,
            "BROWSER_SIGNED_OUT",
            format!(
                "{url} led to {landed}, a page to sign in, so it was not read. Run nest browser {url}, sign in, close the browser, and run this again."
            ),
        ));
    }
    let name = file_name(&landed, "text/html");
    let file = keep_source(
        root,
        artifacts,
        &raw_folders(&landed),
        &name,
        page.html.as_bytes(),
    )?;
    Ok(Fetched {
        file,
        url: url.to_string(),
        warning: warning(&page),
        with: page.version,
        scrolled: page.scrolled,
    })
}

/// What the user should know about how a page was drawn: that it did not finish loading, and
/// that it had more when the scrolls ran out.
fn warning(page: &crate::wiki_browser::Rendered) -> Option<String> {
    let mut notes = Vec::new();
    if !page.settled {
        notes.push(
            "The page did not finish loading within 30 seconds; it was kept as it was drawn by then."
                .to_owned(),
        );
    }
    if let Some(scrolled) = page.scrolled.as_ref().filter(|scrolled| scrolled.cut) {
        notes.push(format!(
            "The page still had more after {} scrolls; raise --scroll to take more of it.",
            scrolled.steps
        ));
    }
    (!notes.is_empty()).then(|| notes.join(" "))
}

/// Whether a page sent the browser to a page to sign in instead: it ended at another address,
/// whose host or path speaks of signing in.
fn signed_out(asked: &url::Url, landed: &url::Url) -> bool {
    if asked.host_str() == landed.host_str() && asked.path() == landed.path() {
        return false;
    }
    let place = format!(
        "{}{}",
        landed.host_str().unwrap_or_default(),
        landed.path()
    )
    .to_ascii_lowercase();
    [
        "login", "log-in", "log_in", "signin", "sign-in", "sign_in", "/auth", "accounts.", "/sso",
    ]
    .iter()
    .any(|word| place.contains(word))
}

/// Downloads a page or a file once over HTTP, as the server sends it.
fn download(
    url: url::Url,
    root: &str,
    artifacts: &str,
    warning: Option<String>,
) -> Result<Fetched, StageError> {
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
        with: "http".to_owned(),
        warning,
        scrolled: None,
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
    fn a_page_that_ends_at_a_sign_in_page_is_signed_out() {
        let check = |asked: &str, landed: &str| {
            signed_out(
                &url::Url::parse(asked).unwrap(),
                &url::Url::parse(landed).unwrap(),
            )
        };
        assert!(check(
            "https://x.com/i/bookmarks",
            "https://x.com/i/flow/login?redirect_after_login=%2Fi%2Fbookmarks"
        ));
        assert!(check(
            "https://mail.example.com/inbox",
            "https://accounts.example.com/v3/signin/identifier"
        ));
        // Another page that is no sign-in page, and the page itself, are pages as they are.
        assert!(!check("https://x.com/i/bookmarks", "https://x.com/i/history"));
        assert!(!check(
            "https://example.com/login",
            "https://example.com/login?from=menu"
        ));
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
