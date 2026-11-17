//! Embedding model calls over HTTP (spec 28.9 step 2b): Ollama `api/embed` and OpenAI `embeddings`.
//!
//! Requests and checks follow the JavaScript client they replace (`tools/rag/models.mjs`): the
//! request body is the same JSON, so trial limits count the same bytes, and vectors are checked and
//! normalized in the same order of operations, so similarities match.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::Cancellation;
use crate::config_value::ConfigValue;

/// An embedding model resolved by JavaScript: endpoint URL, key and limits.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbedderSettings {
    /// `ollama` or `openai`.
    pub provider: String,
    pub model: String,
    /// The full endpoint URL (`.../api/embed` or `.../embeddings`).
    pub url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub max_input_bytes: Option<f64>,
}

impl EmbedderSettings {
    /// `[provider, model, url]`, part of the cache keys and stage configuration IDs.
    pub fn identity(&self) -> ConfigValue {
        ConfigValue::Array(vec![
            ConfigValue::String(self.provider.clone()),
            ConfigValue::String(self.model.clone()),
            ConfigValue::String(self.url.clone()),
        ])
    }

    fn label(&self) -> &'static str {
        if self.provider == "openai" {
            "OpenAI embeddings"
        } else {
            "Ollama embeddings"
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct EmbedError {
    /// A machine-readable code, when the JavaScript client gave one.
    pub code: Option<&'static str>,
    pub message: String,
    /// The process exit code for a coded error.
    pub exit_code: Option<u8>,
    pub aborted: bool,
    /// Details for the command result, as `ProjectError` data.
    pub data: Option<ConfigValue>,
}

impl EmbedError {
    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
            exit_code: None,
            aborted: false,
            data: None,
        }
    }

    pub fn coded(code: &'static str, message: impl Into<String>, exit_code: u8) -> Self {
        Self {
            code: Some(code),
            message: message.into(),
            exit_code: Some(exit_code),
            aborted: false,
            data: None,
        }
    }

    /// An error of the native engine: its code, without an exit code of its own.
    pub fn native(error: crate::Error) -> Self {
        if matches!(error, crate::Error::Aborted) {
            return Self::aborted();
        }
        Self {
            code: Some(error.code()),
            message: error.to_string(),
            exit_code: None,
            aborted: false,
            data: None,
        }
    }

    pub fn aborted() -> Self {
        Self {
            code: None,
            message: "Aborted".into(),
            exit_code: None,
            aborted: true,
            data: None,
        }
    }
}

/// Counts a request against the trial limits before it is sent. Returns an error to stop.
pub trait Charge {
    fn charge(&self, bytes: usize) -> Result<(), String>;
}

pub struct Embedder {
    settings: EmbedderSettings,
    agent: ureq::Agent,
}

/// The HTTP client of model calls: fetch's defaults in the JavaScript implementation.
pub(crate) fn model_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        // fetch(..., { redirect: 'error' })
        .max_redirects(0)
        // Node.js fetch ignores HTTP_PROXY, so a local Ollama stays direct.
        .proxy(None)
        .http_status_as_error(false)
        .build()
        .into()
}

/// A model endpoint: its URL, key, request size limit and the name its errors use.
#[derive(Clone)]
pub(crate) struct Endpoint {
    pub url: String,
    pub api_key: Option<String>,
    pub max_input_bytes: Option<f64>,
    /// `Ollama embeddings`, `openai` and so on, as the JavaScript messages named it.
    pub label: String,
}

/// `post`: checks the size limit, counts the request against the trial limits, sends the JSON and
/// reads the JSON response. Cancellation stops waiting at once.
pub(crate) fn post_json(
    agent: &ureq::Agent,
    endpoint: &Endpoint,
    payload: &str,
    charge: &dyn Charge,
    cancel: &Cancellation,
) -> Result<Value, EmbedError> {
    if endpoint
        .max_input_bytes
        .is_some_and(|limit| payload.len() as f64 > limit)
    {
        return Err(EmbedError::coded(
            "MODEL_INPUT_LIMIT",
            "The model request exceeds max_input_bytes.",
            2,
        ));
    }
    charge.charge(payload.len()).map_err(EmbedError::plain)?;
    // The request runs on its own thread so that cancellation stops waiting at once, as an
    // aborted fetch did. An abandoned request ends by itself within the timeout.
    let (sender, receiver) = std::sync::mpsc::channel();
    let agent = agent.clone();
    let endpoint = endpoint.clone();
    let payload = payload.to_owned();
    std::thread::spawn(move || {
        let _ = sender.send(send(&agent, &endpoint, &payload));
    });
    loop {
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(result) => return result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                cancel.check().map_err(|_| EmbedError::aborted())?
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(EmbedError::plain("The model request stopped"));
            }
        }
    }
}

impl Embedder {
    pub fn new(settings: EmbedderSettings) -> Self {
        Self {
            settings,
            agent: model_agent(),
        }
    }

    /// Embeds `input` and returns one unit-length vector per text, in order.
    pub fn embed(
        &self,
        input: &[String],
        charge: &dyn Charge,
        cancel: &Cancellation,
    ) -> Result<Vec<Vec<f64>>, EmbedError> {
        cancel.check().map_err(|_| EmbedError::aborted())?;
        let settings = &self.settings;
        let texts = ConfigValue::Array(
            input
                .iter()
                .map(|text| ConfigValue::String(text.clone()))
                .collect(),
        );
        let model = ConfigValue::String(settings.model.clone());
        let body = if settings.provider == "openai" {
            ConfigValue::Object(vec![
                ("model".into(), model),
                ("input".into(), texts),
                (
                    "encoding_format".into(),
                    ConfigValue::String("float".into()),
                ),
            ])
        } else {
            ConfigValue::Object(vec![
                ("model".into(), model),
                ("input".into(), texts),
                ("truncate".into(), ConfigValue::Bool(false)),
            ])
        };
        let result = self.post(&body.to_js_json(), charge, cancel)?;
        let vectors = if settings.provider == "openai" {
            openai_vectors(&result, input.len())?
        } else {
            result.get("embeddings").cloned().unwrap_or(Value::Null)
        };
        normalize(&vectors, input.len())
    }

    fn post(
        &self,
        payload: &str,
        charge: &dyn Charge,
        cancel: &Cancellation,
    ) -> Result<Value, EmbedError> {
        let settings = &self.settings;
        let endpoint = Endpoint {
            url: settings.url.clone(),
            api_key: settings.api_key.clone(),
            max_input_bytes: settings.max_input_bytes,
            label: settings.label().into(),
        };
        post_json(&self.agent, &endpoint, payload, charge, cancel)
    }
}

fn send(agent: &ureq::Agent, endpoint: &Endpoint, payload: &str) -> Result<Value, EmbedError> {
    let mut request = agent
        .post(&endpoint.url)
        .header("content-type", "application/json");
    if let Some(key) = endpoint.api_key.as_deref().filter(|key| !key.is_empty()) {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let failed = |error: ureq::Error| {
        EmbedError::plain(format!("{} request failed: {error}", endpoint.label))
    };
    let mut response = request.send(payload).map_err(failed)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(EmbedError::plain(format!(
            "{} generation failed (HTTP {status})",
            endpoint.label
        )));
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(1 << 30)
        .read_to_string()
        .map_err(failed)?;
    serde_json::from_str(&text)
        .map_err(|_| EmbedError::plain(format!("{} returned invalid JSON", endpoint.label)))
}

/// OpenAI returns `data: [{ index, embedding }]`, placed by index.
fn openai_vectors(result: &Value, count: usize) -> Result<Value, EmbedError> {
    let mut vectors: Vec<Option<Value>> = Vec::new();
    let data = match result.get("data") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(items)) => items.as_slice(),
        Some(_) => {
            return Err(EmbedError::plain(
                "OpenAI returned an invalid embedding index",
            ));
        }
    };
    for item in data {
        let index = item
            .get("index")
            .and_then(Value::as_f64)
            .filter(|index| index.fract() == 0.0 && *index >= 0.0 && *index < count as f64)
            .map(|index| index as usize);
        let Some(index) = index.filter(|&index| vectors.get(index).is_none_or(Option::is_none))
        else {
            return Err(EmbedError::plain(
                "OpenAI returned an invalid embedding index",
            ));
        };
        if vectors.len() <= index {
            vectors.resize(index + 1, None);
        }
        vectors[index] = Some(item.get("embedding").cloned().unwrap_or(Value::Null));
    }
    Ok(Value::Array(
        vectors
            .into_iter()
            .map(|vector| vector.unwrap_or(Value::Null))
            .collect(),
    ))
}

/// `embedBatch`: one finite, nonempty vector per input, scaled to unit length.
fn normalize(vectors: &Value, count: usize) -> Result<Vec<Vec<f64>>, EmbedError> {
    let Value::Array(vectors) = vectors else {
        return Err(EmbedError::plain(
            "Embedding provider returned the wrong number of vectors",
        ));
    };
    if vectors.len() != count {
        return Err(EmbedError::plain(
            "Embedding provider returned the wrong number of vectors",
        ));
    }
    vectors
        .iter()
        .map(|vector| {
            let values: Option<Vec<f64>> = match vector {
                Value::Array(values) if !values.is_empty() => {
                    values.iter().map(Value::as_f64).collect()
                }
                _ => None,
            };
            let values = values
                .filter(|values| values.iter().all(|value| value.is_finite()))
                .ok_or_else(|| {
                    EmbedError::plain("Embedding provider returned an invalid vector")
                })?;
            let norm = values
                .iter()
                .fold(0.0, |sum, value| sum + value * value)
                .sqrt();
            if !norm.is_finite() || norm == 0.0 {
                return Err(EmbedError::plain(
                    "Embedding provider returned an invalid vector norm",
                ));
            }
            Ok(values.iter().map(|value| value / norm).collect())
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A local HTTP server that answers each request with `respond(body)` and records the bodies.
    pub(crate) fn serve(
        respond: impl Fn(&Value) -> (u16, String) + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = bodies.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body = String::from_utf8(body).unwrap();
                let (status, text) = respond(&serde_json::from_str(&body).unwrap());
                seen.lock().unwrap().push(body);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        (format!("http://{address}"), bodies)
    }

    pub(crate) struct Free;

    impl Charge for Free {
        fn charge(&self, _bytes: usize) -> Result<(), String> {
            Ok(())
        }
    }

    fn settings(provider: &str, url: &str) -> EmbedderSettings {
        EmbedderSettings {
            provider: provider.into(),
            model: "m".into(),
            url: url.into(),
            api_key: None,
            max_input_bytes: None,
        }
    }

    #[test]
    fn embeds_with_ollama_and_normalizes() {
        let (base, bodies) = serve(|body| {
            let count = body["input"].as_array().unwrap().len();
            (
                200,
                serde_json::json!({ "embeddings": vec![[3.0, 4.0]; count] }).to_string(),
            )
        });
        let embedder = Embedder::new(settings("ollama", &format!("{base}/api/embed")));
        let vectors = embedder
            .embed(
                &["a".into(), "\u{2028}\"".into()],
                &Free,
                &Cancellation::default(),
            )
            .unwrap();
        assert_eq!(vectors, vec![vec![0.6, 0.8]; 2]);
        assert_eq!(
            bodies.lock().unwrap()[0],
            "{\"model\":\"m\",\"input\":[\"a\",\"\u{2028}\\\"\"],\"truncate\":false}"
        );
    }

    #[test]
    fn places_openai_vectors_by_index_and_checks_them() {
        let (base, _) = serve(|body| {
            let reply = if body["input"].as_array().unwrap().len() == 2 {
                serde_json::json!({ "data": [
                    { "index": 1, "embedding": [0.0, 2.0] },
                    { "index": 0, "embedding": [2.0, 0.0] },
                ] })
            } else {
                serde_json::json!({ "data": [{ "index": 1, "embedding": [1.0] }] })
            };
            (200, reply.to_string())
        });
        let embedder = Embedder::new(settings("openai", &format!("{base}/embeddings")));
        let cancel = Cancellation::default();
        assert_eq!(
            embedder
                .embed(&["a".into(), "b".into()], &Free, &cancel)
                .unwrap(),
            vec![vec![1.0, 0.0], vec![0.0, 1.0]]
        );
        assert_eq!(
            embedder.embed(&["a".into()], &Free, &cancel).unwrap_err(),
            EmbedError::plain("OpenAI returned an invalid embedding index")
        );
    }

    #[test]
    fn reports_http_failures_and_input_limits() {
        let (base, bodies) = serve(|_| (500, "{}".into()));
        let mut limited = settings("ollama", &format!("{base}/api/embed"));
        limited.max_input_bytes = Some(10.0);
        let cancel = Cancellation::default();
        let error = Embedder::new(limited)
            .embed(&["long text".into()], &Free, &cancel)
            .unwrap_err();
        assert_eq!(error.code, Some("MODEL_INPUT_LIMIT"));
        assert!(bodies.lock().unwrap().is_empty());
        let error = Embedder::new(settings("ollama", &format!("{base}/api/embed")))
            .embed(&["a".into()], &Free, &cancel)
            .unwrap_err();
        assert_eq!(
            error,
            EmbedError::plain("Ollama embeddings generation failed (HTTP 500)")
        );
    }

    #[test]
    fn stops_waiting_when_cancelled() {
        let (base, _) = serve(|_| {
            std::thread::sleep(Duration::from_secs(3));
            (200, "{\"embeddings\":[[1]]}".into())
        });
        let embedder = Embedder::new(settings("ollama", &format!("{base}/api/embed")));
        let cancel = Cancellation::default();
        let stop = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            stop.abort();
        });
        let start = std::time::Instant::now();
        let error = embedder.embed(&["a".into()], &Free, &cancel).unwrap_err();
        assert!(error.aborted);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn rejects_invalid_vectors() {
        let check = |vectors: Value| normalize(&vectors, 1).unwrap_err().message;
        assert_eq!(
            check(serde_json::json!([])),
            "Embedding provider returned the wrong number of vectors"
        );
        assert_eq!(
            check(serde_json::json!([[]])),
            "Embedding provider returned an invalid vector"
        );
        assert_eq!(
            check(serde_json::json!([["1"]])),
            "Embedding provider returned an invalid vector"
        );
        assert_eq!(
            check(serde_json::json!([[0.0]])),
            "Embedding provider returned an invalid vector norm"
        );
        assert_eq!(
            check(serde_json::json!([[1e300, 1e300]])),
            "Embedding provider returned an invalid vector norm"
        );
    }
}
