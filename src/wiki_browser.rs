//! Web pages opened in the Chrome or Edge of the machine (spec 14.5): the browser starts
//! without a window in the profile of nest, opens the page, waits until it has drawn it, and
//! gives the HTML it shows. It is driven from Rust over the Chrome DevTools Protocol.
//!
//! The profile of nest is a folder beside the user settings (`~/.knowledge-nest/browser/chrome`),
//! one for each browser. `nest browser` opens it with a window, so that the user signs in to the
//! sites to read; pages are read only in it, with the sign-ins kept there.

// The entry points are called from `node`, which test builds leave out.
#![cfg_attr(test, allow(dead_code))]

use std::collections::{HashSet, VecDeque};
use std::io::ErrorKind;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

/// A browser of the machine.
pub(crate) struct Browser {
    /// What it is, as `Chrome` or `Edge`.
    pub name: &'static str,
    pub path: PathBuf,
}

impl Browser {
    /// The name of its profile folder: `chrome`, `edge`, `chromium`, or `browser` for one given
    /// by its path.
    pub fn key(&self) -> String {
        self.name.to_ascii_lowercase()
    }
}

/// The folder of the profiles of nest: `browser` beside the user settings, which are
/// `KNOWLEDGE_NEST_GLOBAL_CONFIG` or `~/.knowledge-nest/config.yaml`.
pub(crate) fn profiles(cwd: &str, home: &str) -> PathBuf {
    let settings = match std::env::var("KNOWLEDGE_NEST_GLOBAL_CONFIG") {
        Ok(path) if !path.is_empty() => crate::node_path::resolve(cwd, &path),
        _ => crate::node_path::join(home, &[".knowledge-nest", "config.yaml"]),
    };
    PathBuf::from(crate::node_path::dirname(&settings)).join("browser")
}

/// Whether a browser has the profile open: Chrome and Edge hold `lockfile` (Windows) or
/// `SingletonLock` (macOS, Linux) in it while they run.
pub(crate) fn in_use(profile: &Path) -> bool {
    if cfg!(windows) {
        let lock = profile.join("lockfile");
        // The browser keeps the file open for itself; a file left by a browser that crashed
        // opens.
        lock.exists()
            && std::fs::OpenOptions::new()
                .write(true)
                .open(&lock)
                .is_err()
    } else {
        // The link points at `<host>-<process>`.
        let Ok(target) = std::fs::read_link(profile.join("SingletonLock")) else {
            return false;
        };
        let target = target.to_string_lossy().into_owned();
        let Some(process) = target.rsplit('-').next().filter(|id| id.parse::<u32>().is_ok())
        else {
            return true;
        };
        Command::new("kill")
            .args(["-0", process])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}

/// Opens the profile of nest in the browser with a window, at an address or a new tab, and
/// returns when the user closes it. The profile is made when there is none.
pub(crate) fn show(browser: &Browser, profile: &Path, address: Option<&str>) -> Result<(), String> {
    std::fs::create_dir_all(profile).map_err(|error| error.to_string())?;
    let mut command = Command::new(&browser.path);
    command
        .args(["--no-first-run", "--no-default-browser-check"])
        .arg(format!("--user-data-dir={}", profile.display()));
    if let Some(address) = address {
        command.arg(address);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{} could not start: {error}", browser.path.display()))?;
    child.wait().map_err(|error| error.to_string())?;
    Ok(())
}

/// The places browsers are installed in, by name, in the order they are tried.
fn candidates() -> Vec<(&'static str, PathBuf)> {
    let mut found: Vec<(&'static str, PathBuf)> = Vec::new();
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    if cfg!(windows) {
        let roots = [
            env("ProgramFiles"),
            env("ProgramFiles(x86)"),
            env("LOCALAPPDATA"),
        ];
        for (name, parts) in [
            (
                "Chrome",
                &["Google", "Chrome", "Application", "chrome.exe"][..],
            ),
            (
                "Edge",
                &["Microsoft", "Edge", "Application", "msedge.exe"][..],
            ),
            ("Chromium", &["Chromium", "Application", "chrome.exe"][..]),
        ] {
            for root in roots.iter().flatten() {
                found.push((
                    name,
                    parts
                        .iter()
                        .fold(root.clone(), |path, part| path.join(part)),
                ));
            }
        }
    } else if cfg!(target_os = "macos") {
        for (name, path) in [
            (
                "Chrome",
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            ),
            (
                "Edge",
                "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            ),
            (
                "Chromium",
                "/Applications/Chromium.app/Contents/MacOS/Chromium",
            ),
        ] {
            found.push((name, PathBuf::from(path)));
            if let Some(home) = env("HOME") {
                found.push((name, home.join(path.trim_start_matches('/'))));
            }
        }
    } else {
        let directories: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).collect())
            .unwrap_or_default();
        for (name, command) in [
            ("Chrome", "google-chrome"),
            ("Chrome", "google-chrome-stable"),
            ("Edge", "microsoft-edge"),
            ("Edge", "microsoft-edge-stable"),
            ("Chromium", "chromium"),
            ("Chromium", "chromium-browser"),
        ] {
            for directory in &directories {
                found.push((name, directory.join(command)));
            }
        }
    }
    found
}

/// The browsers of a choice that are installed, in the order they are tried, each once: `auto`
/// takes Chrome, Edge and Chromium; `chrome` or `edge` takes only that one; anything else is the
/// path of a browser.
pub(crate) fn installed(choice: &str) -> Vec<Browser> {
    let wanted = match choice.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => None,
        "chrome" => Some("Chrome"),
        "edge" => Some("Edge"),
        "chromium" => Some("Chromium"),
        _ => {
            let path = PathBuf::from(choice.trim());
            return if path.is_file() {
                vec![Browser {
                    name: "Browser",
                    path,
                }]
            } else {
                Vec::new()
            };
        }
    };
    let mut found: Vec<Browser> = Vec::new();
    for (name, path) in candidates() {
        if wanted.is_none_or(|wanted| wanted == name)
            && !found.iter().any(|browser| browser.name == name)
            && path.is_file()
        {
            found.push(Browser { name, path });
        }
    }
    found
}

/// A page as the browser drew it.
pub(crate) struct Rendered {
    pub html: String,
    /// The address it ended at, after redirects.
    pub url: String,
    /// Whether the page settled before the time ran out.
    pub settled: bool,
    /// The browser and its version, as `Chrome/131.0.6778.86`.
    pub version: String,
    /// How the page was scrolled for more, when it was.
    pub scrolled: Option<Scrolled>,
}

/// What scrolling a page for more did.
pub(crate) struct Scrolled {
    /// The times it was scrolled.
    pub steps: u32,
    /// The items (`article` elements) it showed in all, each once.
    pub items: usize,
    /// Whether it ran out of scrolls while the page still grew.
    pub cut: bool,
}

/// Collects the items the page shows now into `window.__nestKept`, each once by its link or
/// text, and tells how many there are and where the page is. Pages that load more as they are
/// scrolled often drop the items that went far out of view, so they are kept as they come.
const COLLECT: &str = r#"(() => {
  const kept = window.__nestKept || (window.__nestKept = { order: [], items: new Map() });
  const selector = 'article, [role="article"]';
  for (const item of document.querySelectorAll(selector)) {
    if (item.parentElement && item.parentElement.closest(selector)) continue;
    const time = item.querySelector('a[href] time');
    const key = time ? time.closest('a').href : (item.id || item.innerText.trim().slice(0, 300));
    if (!key) continue;
    if (!kept.items.has(key)) kept.order.push(key);
    kept.items.set(key, item.outerHTML);
  }
  const root = document.scrollingElement || document.documentElement;
  return JSON.stringify({ items: kept.order.length, height: root.scrollHeight, top: Math.round(root.scrollTop) });
})()"#;

/// Scrolls the page down by most of a screen.
const SCROLL: &str = "window.scrollBy(0, Math.max(200, Math.round(window.innerHeight * 0.8))); ''";

/// The kept items as one page, in the order they came, with the title and address of the page.
const KEPT: &str = r#"(() => {
  const kept = window.__nestKept;
  const escape = (text) => String(text).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  const items = kept.order.map((key) => kept.items.get(key)).join('');
  return '<!doctype html><html><head><meta charset="utf-8"><base href="' + escape(location.href) + '"><title>'
    + escape(document.title) + '</title></head><body><main>' + items + '</main></body></html>';
})()"#;

/// How long one scroll waits for what it loads.
const STEP: Duration = Duration::from_secs(2);

/// What opening an address gave.
pub(crate) enum Opened {
    /// A page, drawn.
    Page(Rendered),
    /// A file that is not a page (a PDF, an Office document): it is downloaded as it is.
    File,
}

/// The running browser, which stops when it is dropped. It is asked to close first, so that it
/// leaves its profile as a browser that was closed; it is stopped when it does not.
struct Running {
    child: Child,
    /// Whether it was asked to close.
    closing: bool,
}

impl Drop for Running {
    fn drop(&mut self) {
        let started = Instant::now();
        while self.closing && started.elapsed() < Duration::from_secs(5) {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How long the browser has to start, and a page to settle.
const START: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(30);
/// How long the network is quiet before a page counts as drawn.
const QUIET: Duration = Duration::from_millis(500);

/// A connection to a page of the browser.
struct Session {
    socket: WebSocket<TcpStream>,
    next: u64,
    events: VecDeque<Value>,
}

impl Session {
    fn send(&mut self, method: &str, params: Value) -> Result<u64, String> {
        self.next += 1;
        let id = self.next;
        let message = json!({ "id": id, "method": method, "params": params }).to_string();
        self.socket
            .send(Message::text(message))
            .map_err(|error| format!("DevTools: {error}"))?;
        Ok(id)
    }

    /// The next message, or none when nothing came within the read timeout.
    fn receive(&mut self) -> Result<Option<Value>, String> {
        match self.socket.read() {
            Ok(Message::Text(text)) => Ok(Some(
                serde_json::from_str(text.as_str()).unwrap_or(Value::Null),
            )),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                Ok(None)
            }
            Err(error) => Err(format!("DevTools: {error}")),
        }
    }

    /// Calls a method and returns its result; the events that come meanwhile are kept.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.send(method, params)?;
        let started = Instant::now();
        while started.elapsed() < SETTLE {
            let Some(message) = self.receive()? else {
                continue;
            };
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(format!("{method}: {error}"));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            if message.get("method").is_some() {
                self.events.push_back(message);
            }
        }
        Err(format!("{method}: no answer from the browser"))
    }

    /// The next event, from those kept or the connection.
    fn event(&mut self) -> Result<Option<Value>, String> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        Ok(self
            .receive()?
            .filter(|message| message.get("method").is_some()))
    }

    /// The value of an expression in the page, as text.
    fn evaluate(&mut self, expression: &str) -> Result<String, String> {
        let result = self.call(
            "Runtime.evaluate",
            json!({ "expression": expression, "returnByValue": true }),
        )?;
        if let Some(error) = result.get("exceptionDetails") {
            return Err(format!("the page could not run a script: {error}"));
        }
        Ok(result
            .get("result")
            .and_then(|result| result.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    /// Waits until the requests the page makes are done and it has been quiet for a moment, or
    /// until `limit` has passed.
    fn wait_quiet(&mut self, limit: Duration) -> Result<(), String> {
        let started = Instant::now();
        let mut pending: HashSet<String> = HashSet::new();
        let mut quiet_since = Instant::now();
        while started.elapsed() < limit {
            if pending.is_empty() && quiet_since.elapsed() >= QUIET {
                break;
            }
            let Some(event) = self.event()? else {
                continue;
            };
            let id = event
                .get("params")
                .and_then(|params| params.get("requestId"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            match event
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "Network.requestWillBeSent" => {
                    pending.insert(id);
                }
                "Network.loadingFinished" | "Network.loadingFailed" => {
                    pending.remove(&id);
                    if pending.is_empty() {
                        quiet_since = Instant::now();
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Scrolls the page down up to `limit` times, keeping the items it shows, and stops early when
/// three scrolls in a row bring nothing new. Returns what it did and the kept items as a page,
/// or none when the page shows fewer than two items.
fn scroll_for_more(session: &mut Session, limit: u32) -> Result<(Scrolled, Option<String>), String> {
    let mut state = session.evaluate(COLLECT)?;
    let mut steps = 0;
    let mut still = 0;
    while steps < limit && still < 3 {
        session.evaluate(SCROLL)?;
        steps += 1;
        session.wait_quiet(STEP)?;
        let next = session.evaluate(COLLECT)?;
        still = if next == state { still + 1 } else { 0 };
        state = next;
    }
    let items = serde_json::from_str::<Value>(&state)
        .ok()
        .and_then(|state| state.get("items").and_then(Value::as_u64))
        .unwrap_or(0) as usize;
    let html = if items >= 2 {
        Some(session.evaluate(KEPT)?)
    } else {
        None
    };
    Ok((
        Scrolled {
            steps,
            items,
            cut: still < 3,
        },
        html,
    ))
}

/// Starts the browser without a window in a profile, and returns the port of its DevTools.
fn start(browser: &Browser, profile: &Path) -> Result<(Running, u16), String> {
    // The port of an earlier start may be left in the profile.
    let file = profile.join("DevToolsActivePort");
    let _ = std::fs::remove_file(&file);
    let child = Command::new(&browser.path)
        .args([
            "--headless=new",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-default-apps",
            "--disable-sync",
            "--disable-notifications",
            "--deny-permission-prompts",
            "--mute-audio",
            "--hide-scrollbars",
            // A tall window draws more of a page at once, and scrolls further each time.
            "--window-size=1280,2000",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{} could not start: {error}", browser.path.display()))?;
    let mut running = Running {
        child,
        closing: false,
    };
    // The browser writes the port of its DevTools to its profile when it is ready.
    let started = Instant::now();
    while started.elapsed() < START {
        if let Ok(text) = std::fs::read_to_string(&file)
            && let Some(port) = text
                .lines()
                .next()
                .and_then(|line| line.trim().parse().ok())
        {
            return Ok((running, port));
        }
        if !matches!(running.child.try_wait(), Ok(None)) {
            return Err(format!(
                "{} stopped as it started; another browser may have the profile open",
                browser.name
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "{} did not start within {} seconds",
        browser.name,
        START.as_secs()
    ))
}

/// A JSON document of the DevTools of the browser, as `/json/version`.
fn devtools_json(port: u16, path: &str) -> Result<Value, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .proxy(None)
        .build()
        .into();
    let text = agent
        .get(&format!("http://127.0.0.1:{port}{path}"))
        .call()
        .and_then(|mut response| response.body_mut().read_to_string())
        .map_err(|error| format!("DevTools: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("DevTools: {error}"))
}

/// Opens an address in the browser and gives the page as it drew it, once the page has loaded
/// and the network has been quiet for a moment, or when the time runs out; with `scroll`, after
/// scrolling it for more up to that many times. A file that is not a page is left for an
/// ordinary download.
pub(crate) fn open(
    browser: &Browser,
    profile: &Path,
    address: &str,
    scroll: u32,
) -> Result<Opened, String> {
    let (mut running, port) = start(browser, profile)?;
    let version = devtools_json(port, "/json/version")?
        .get("Browser")
        .and_then(Value::as_str)
        .unwrap_or(browser.name)
        .to_owned();
    let pages = devtools_json(port, "/json/list")?;
    let socket_url = pages
        .as_array()
        .into_iter()
        .flatten()
        .find(|target| target.get("type").and_then(Value::as_str) == Some("page"))
        .and_then(|target| target.get("webSocketDebuggerUrl"))
        .and_then(Value::as_str)
        .ok_or("DevTools: the browser has no page")?
        .to_owned();
    let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|error| error.to_string())?;
    let (socket, _) = tungstenite::client(socket_url.as_str(), stream)
        .map_err(|error| format!("DevTools: {error}"))?;
    let mut session = Session {
        socket,
        next: 0,
        events: VecDeque::new(),
    };
    let read = read(&mut session, address, version, scroll);
    // Closed as a person closes it, the browser leaves its profile in order.
    running.closing = session.send("Browser.close", json!({})).is_ok();
    read
}

/// Opens an address in the page of a session and gives what it drew.
fn read(
    session: &mut Session,
    address: &str,
    version: String,
    scroll: u32,
) -> Result<Opened, String> {
    session.call("Page.enable", json!({}))?;
    session.call("Network.enable", json!({}))?;
    let navigated = session.call("Page.navigate", json!({ "url": address }))?;
    if let Some(error) = navigated
        .get("errorText")
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty())
    {
        return Err(error.to_owned());
    }
    let frame = navigated
        .get("frameId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let started = Instant::now();
    let mut loaded = false;
    let mut pending: HashSet<String> = HashSet::new();
    let mut quiet_since = Instant::now();
    let settled = loop {
        if started.elapsed() >= SETTLE {
            break false;
        }
        if loaded && pending.is_empty() && quiet_since.elapsed() >= QUIET {
            break true;
        }
        let Some(event) = session.event()? else {
            continue;
        };
        let params = event.get("params").cloned().unwrap_or(Value::Null);
        let text = |name: &str| {
            params
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        match event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "Page.loadEventFired" => {
                loaded = true;
                quiet_since = Instant::now();
            }
            "Network.requestWillBeSent" => {
                pending.insert(text("requestId"));
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                pending.remove(&text("requestId"));
                if pending.is_empty() {
                    quiet_since = Instant::now();
                }
            }
            "Network.responseReceived"
                if text("type") == "Document" && text("frameId") == frame =>
            {
                let response = params.get("response").cloned().unwrap_or(Value::Null);
                let status = response.get("status").and_then(Value::as_u64).unwrap_or(0);
                if status >= 400 {
                    return Err(format!("HTTP {status}"));
                }
                let mime = response
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !mime.is_empty() && mime != "text/html" && mime != "application/xhtml+xml" {
                    return Ok(Opened::File);
                }
            }
            _ => {}
        }
    };
    // A page scrolled for more is kept as the items it showed; a page with fewer than two items
    // is kept as it is drawn at the end.
    let (scrolled, kept) = if scroll > 0 {
        let (scrolled, kept) = scroll_for_more(session, scroll)?;
        (Some(scrolled), kept)
    } else {
        (None, None)
    };
    let html = match kept {
        Some(html) => html,
        None => format!(
            "<!doctype html>\n{}",
            session.evaluate("document.documentElement.outerHTML")?
        ),
    };
    let url = session.evaluate("location.href")?;
    Ok(Opened::Page(Rendered {
        html,
        url,
        settled,
        version,
        scrolled,
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_browser_is_chosen_by_name_or_path() {
        assert!(super::installed("C:/no/such/browser.exe").is_empty());
        // `auto` finds the same browsers as their names, each once, when the machine has them.
        let found = super::installed("auto");
        for browser in &found {
            let named = super::installed(browser.name);
            assert_eq!(named.len(), 1);
            assert_eq!(named[0].path, browser.path);
            assert_eq!(found.iter().filter(|other| other.name == browser.name).count(), 1);
        }
    }
}
