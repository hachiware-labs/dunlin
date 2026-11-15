//! OCR with LightOnOCR: the managed Python server, started by Rust as a separate process
//! (spec 4.1, 28.9 step 2a).
//!
//! `dunlin add lightonocr` prepares Python and the model. The server starts when the first page
//! needs OCR and stops when the load ends: it exits when its standard input closes. Pages go to it
//! as PNG images; it answers with text blocks marked by boxes in 0..1000.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Deserialize;

use crate::config_value::ConfigValue;

const STARTUP: Duration = Duration::from_secs(180);
/// The longest one page may take to recognize.
const PAGE: Duration = Duration::from_secs(600);

/// The prepared runtime, resolved by the caller.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Lighton {
    pub python: String,
    pub server: String,
    pub model_dir: String,
    pub model: String,
}

struct Worker {
    config: Lighton,
    child: Child,
    stdin: Option<ChildStdin>,
    /// `127.0.0.1:<port>`
    address: String,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // The server exits when its standard input closes.
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn worker() -> &'static Mutex<Option<Worker>> {
    static WORKER: Mutex<Option<Worker>> = Mutex::new(None);
    &WORKER
}

/// Stops the server, if one runs. The loader calls this when a load ends.
pub fn stop() {
    if let Ok(mut worker) = worker().lock() {
        worker.take();
    }
}

fn start(config: &Lighton, check: &dyn Fn() -> bool) -> Result<Worker, String> {
    let mut command = Command::new(&config.python);
    command
        .args([
            "-I",
            "-u",
            &config.server,
            "--device",
            "gpu",
            "--model-dir",
            &config.model_dir,
            "--port",
            "0",
            "--parent-stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start LightOnOCR: {error}"))?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().ok_or("Could not read LightOnOCR")?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let read = reader.read_line(&mut line).map(|count| (count, line));
        let _ = sender.send(read);
        // Keep reading so the server never blocks on a full pipe.
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
    });
    let mut worker = Worker {
        config: config.clone(),
        child,
        stdin,
        address: String::new(),
    };
    let deadline = Instant::now() + STARTUP;
    let line = loop {
        if !check() {
            return Err("Aborted".into());
        }
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(Ok((0, _))) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let code = worker
                    .child
                    .try_wait()
                    .ok()
                    .flatten()
                    .map(|status| status.to_string())
                    .unwrap_or_default();
                return Err(format!("LightOnOCR stopped before startup ({code})"));
            }
            Ok(Ok((_, line))) => break line,
            Ok(Err(error)) => return Err(format!("Could not read LightOnOCR: {error}")),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                return Err("Local LightOnOCR took more than 180 seconds to start".into());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    let started: ConfigValue =
        serde_json::from_str(line.trim()).map_err(|_| "LightOnOCR did not report its endpoint")?;
    let url = started
        .get("url")
        .and_then(ConfigValue::as_str)
        .unwrap_or_default();
    let address = url
        .strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix("/v1"))
        .filter(|address| {
            address
                .strip_prefix("127.0.0.1:")
                .and_then(|port| port.parse::<u16>().ok())
                .is_some_and(|port| port > 0)
        });
    match (started.get("device").and_then(ConfigValue::as_str), address) {
        (Some("gpu"), Some(address)) => worker.address = address.to_owned(),
        _ => return Err("LightOnOCR did not report a local GPU endpoint".into()),
    }
    Ok(worker)
}

/// POSTs JSON to the local server and returns the status and body. The server answers with
/// Content-Length and closes the connection.
fn post(
    address: &str,
    path: &str,
    body: &str,
    check: &dyn Fn() -> bool,
) -> Result<(u16, String), String> {
    let failed = |error: std::io::Error| format!("Could not reach LightOnOCR: {error}");
    let mut stream = TcpStream::connect(address).map_err(failed)?;
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(failed)?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .map_err(failed)?;
    stream.write_all(body.as_bytes()).map_err(failed)?;
    let mut response = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    let deadline = Instant::now() + PAGE;
    loop {
        if !check() {
            return Err("Aborted".into());
        }
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => response.extend_from_slice(&buffer[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err("LightOnOCR took too long to read a page".into());
                }
            }
            Err(error) => return Err(failed(error)),
        }
    }
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("LightOnOCR returned an invalid response")?;
    let head = String::from_utf8_lossy(&response[..split]);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or("LightOnOCR returned an invalid response")?;
    let body = String::from_utf8(response[split + 4..].to_vec())
        .map_err(|_| "LightOnOCR returned invalid text")?;
    Ok((status, body))
}

/// Splits grounded output into blocks: `![label](left, top, right, bottom)` marks each block.
/// Page numbers and images are left out.
pub fn grounded_blocks(content: &str) -> Result<Vec<(String, [f64; 4])>, String> {
    let marker =
        regex::Regex::new(r"!\[([a-z_+]+)\]\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*\)")
            .map_err(|error| error.to_string())?;
    let markers: Vec<_> = marker.captures_iter(content).collect();
    if markers.is_empty() && !content.trim().is_empty() {
        return Err("LightOnOCR returned text without grounding coordinates".into());
    }
    let mut blocks = Vec::new();
    for (index, captures) in markers.iter().enumerate() {
        let label = &captures[1];
        if label == "page_number" || label == "image" {
            continue;
        }
        let bbox: Vec<f64> = (2..=5)
            .map(|group| captures[group].parse::<f64>().unwrap_or(f64::INFINITY))
            .collect();
        let [left, top, right, bottom] = [bbox[0], bbox[1], bbox[2], bbox[3]];
        if right > 1000.0 || bottom > 1000.0 || left >= right || top >= bottom {
            return Err("LightOnOCR returned invalid grounding coordinates".into());
        }
        let start = captures.get(0).map_or(0, |whole| whole.end());
        let end = markers
            .get(index + 1)
            .and_then(|next| next.get(0))
            .map_or(content.len(), |next| next.start());
        blocks.push((content[start..end].to_owned(), [left, top, right, bottom]));
    }
    Ok(blocks)
}

fn recognize(
    config: &Lighton,
    image: image::RgbImage,
    check: &dyn Fn() -> bool,
) -> Result<Vec<(String, [f64; 4])>, String> {
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| format!("Could not encode the page: {error}"))?;
    let url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    );
    let request = serde_json::json!({
        "model": config.model,
        "messages": [{ "role": "user", "content": [
            { "type": "image_url", "image_url": { "url": url } },
            { "type": "text", "text": "grounding" },
        ] }],
        "temperature": 0.1,
        "max_tokens": 4096,
    })
    .to_string();
    let address = {
        let mut current = worker().lock().map_err(|_| "LightOnOCR is unavailable")?;
        if current
            .as_ref()
            .is_none_or(|worker| worker.config != *config)
        {
            current.take();
            *current = Some(start(config, check)?);
        }
        current
            .as_ref()
            .map(|worker| worker.address.clone())
            .unwrap_or_default()
    };
    let (status, body) = post(&address, "/v1/chat/completions", &request, check)?;
    if status != 200 {
        return Err(format!(
            "LightOnOCR chat/completions returned HTTP {status}"
        ));
    }
    let result: ConfigValue =
        serde_json::from_str(&body).map_err(|_| "LightOnOCR returned invalid JSON")?;
    let choice = match result.get("choices") {
        Some(ConfigValue::Array(choices)) => choices.first(),
        _ => None,
    };
    if choice
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(ConfigValue::as_str)
        == Some("length")
    {
        return Err("LightOnOCR output was truncated".into());
    }
    let content = choice
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(ConfigValue::as_str)
        .ok_or("LightOnOCR returned no text")?;
    grounded_blocks(content)
}

/// Reads a scanned PDF or an image with LightOnOCR.
pub fn parse(
    config: Option<&Lighton>,
    bytes: &[u8],
    check: &dyn Fn() -> bool,
) -> Result<ConfigValue, String> {
    let mut recognize_page = |image: image::RgbImage| {
        let config = config.ok_or("Local GPU OCR is not prepared. Run dunlin add ocr.")?;
        recognize(config, image, check)
    };
    let parser = ConfigValue::Object(vec![
        ("tool".into(), ConfigValue::String("lightonocr".into())),
        (
            "model".into(),
            ConfigValue::String(
                config
                    .map(|config| config.model.clone())
                    .unwrap_or_default(),
            ),
        ),
        ("lang".into(), ConfigValue::String("ja".into())),
    ]);
    crate::ocr::read(bytes, check, &mut recognize_page, parser)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_grounded_blocks() {
        let blocks = grounded_blocks(
            "![text](10, 20, 300, 40)第1章\n本文![page_number](1,2,3,4)3![text](10,50,300,90)次",
        )
        .unwrap();
        assert_eq!(
            blocks,
            vec![
                ("第1章\n本文".to_owned(), [10.0, 20.0, 300.0, 40.0]),
                ("次".to_owned(), [10.0, 50.0, 300.0, 90.0])
            ]
        );
        assert!(grounded_blocks("plain").is_err());
        assert!(grounded_blocks("![text](10, 20, 5, 40)x").is_err());
        assert_eq!(grounded_blocks("").unwrap(), vec![]);
    }
}
