//! Web pages opened in the Chrome or Edge of the machine (spec 14.5): the browser starts
//! without a window and with a profile of its own, opens the page, waits until it has drawn it,
//! and gives the HTML it shows. It is driven from Rust over the Chrome DevTools Protocol.

// The entry points are called from `node`, which test builds leave out.
#![cfg_attr(test, allow(dead_code))]

use std::collections::{HashSet, VecDeque};
use std::io::ErrorKind;
use std::net::TcpStream;
use std::path::PathBuf;
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

/// The browser to open pages in: `auto` takes the first of Chrome, Edge and Chromium that is
/// installed; `chrome` or `edge` takes only that one; anything else is the path of a browser.
pub(crate) fn find(choice: &str) -> Option<Browser> {
    let wanted = match choice.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => None,
        "chrome" => Some("Chrome"),
        "edge" => Some("Edge"),
        "chromium" => Some("Chromium"),
        _ => {
            let path = PathBuf::from(choice.trim());
            return path.is_file().then_some(Browser {
                name: "Browser",
                path,
            });
        }
    };
    candidates()
        .into_iter()
        .filter(|(name, _)| wanted.is_none_or(|wanted| wanted == *name))
        .find(|(_, path)| path.is_file())
        .map(|(name, path)| Browser { name, path })
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
}

/// What opening an address gave.
pub(crate) enum Opened {
    /// A page, drawn.
    Page(Rendered),
    /// A file that is not a page (a PDF, an Office document): it is downloaded as it is.
    File,
}

/// The running browser and its profile, which go when it is dropped.
struct Running {
    child: Child,
    profile: PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The browser may hold its files for a moment after it stops.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.profile).is_ok() || !self.profile.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
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
}

/// Starts the browser without a window and with a new profile, and returns the port of its
/// DevTools.
fn start(browser: &Browser) -> Result<(Running, u16), String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let profile = std::env::temp_dir().join(format!(
        "knowledge-nest-browser-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&profile).map_err(|error| error.to_string())?;
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
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{} could not start: {error}", browser.path.display()))?;
    let running = Running { child, profile };
    // The browser writes the port of its DevTools to its profile when it is ready.
    let file = running.profile.join("DevToolsActivePort");
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
/// and the network has been quiet for a moment, or when the time runs out. A file that is not a
/// page is left for an ordinary download.
pub(crate) fn open(browser: &Browser, address: &str) -> Result<Opened, String> {
    let (_running, port) = start(browser)?;
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
    let evaluate = |session: &mut Session, expression: &str| -> Result<String, String> {
        let result = session.call(
            "Runtime.evaluate",
            json!({ "expression": expression, "returnByValue": true }),
        )?;
        Ok(result
            .get("result")
            .and_then(|result| result.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    };
    let html = evaluate(&mut session, "document.documentElement.outerHTML")?;
    let url = evaluate(&mut session, "location.href")?;
    Ok(Opened::Page(Rendered {
        html: format!("<!doctype html>\n{html}"),
        url,
        settled,
        version,
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_browser_is_chosen_by_name_or_path() {
        assert!(super::find("C:/no/such/browser.exe").is_none());
        // `auto` finds the same browser as one of the names, when the machine has one.
        if let Some(found) = super::find("auto") {
            let named = super::find(found.name).expect("the same browser by its name");
            assert_eq!(named.path, found.path);
        }
    }
}
