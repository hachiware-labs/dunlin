//! Models and environment references of a project (spec 28.13 step 6b): `${NAME}` in values,
//! `api_key_env`, and the endpoints of generation and embedding models, resolved as
//! `config.mjs` and `models.mjs` resolved them.

use crate::config_value::ConfigValue;
use crate::index_stage::{object, string_field, text};

const OPENAI_BASE: &str = "https://api.openai.com/v1";
const OLLAMA_BASE: &str = "http://127.0.0.1:11434";
const STRATA_BASE: &str = "http://127.0.0.1:8080/v1";

pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// `resolveConfigValue(value, field)`: `${NAME}` replaced by environment variables.
pub fn resolve_value(value: Option<&ConfigValue>, field: &str) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    // A value that is not a string made `value.replace` throw.
    let Some(value) = value.as_str() else {
        return Err("value.replace is not a function".into());
    };
    let mut missing: Option<String> = None;
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let end = after.find('}');
        let name = end.map(|end| &after[..end]);
        let valid = name.is_some_and(|name| {
            let mut chars = name.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        if !valid {
            out += &rest[..start + 2];
            rest = after;
            continue;
        }
        let name = name.unwrap_or_default();
        out += &rest[..start];
        match env(name) {
            Some(variable) => out += &variable,
            None => {
                if missing.is_none() {
                    missing = Some(name.to_owned());
                }
            }
        }
        rest = &after[name.len() + 1..];
    }
    out += rest;
    if let Some(name) = missing {
        return Err(format!(
            "Environment variable {name} is required by {field}"
        ));
    }
    if out.trim_matches(crate::registry::js_space).is_empty() || out.contains('\0') {
        return Err(format!("Expected {field} to be a nonempty string"));
    }
    Ok(Some(out))
}

/// `resolveApiKey(name, field)`
pub fn resolve_api_key(name: Option<&ConfigValue>, field: &str) -> Result<Option<String>, String> {
    let Some(name) = name else {
        return Ok(None);
    };
    let name = &crate::eval_stage::js_string(Some(name));
    let value = env(name).unwrap_or_default();
    if value.trim_matches(crate::registry::js_space).is_empty() {
        return Err(format!(
            "Environment variable {name} is required by {field}.apiKeyEnv"
        ));
    }
    if !value.bytes().all(|byte| (b'!'..=b'~').contains(&byte)) {
        return Err(format!(
            "Environment variable {name} contains an invalid API key for {field}.apiKeyEnv"
        ));
    }
    Ok(Some(value))
}

/// `endpoint(base, route)`: an HTTP(S) base URL without credentials, query or fragment, with the route.
fn endpoint(base: &str, route: &str) -> Result<String, String> {
    let mut url = url::Url::parse(base).map_err(|_| "Invalid URL".to_owned())?;
    if !["https", "http"].contains(&url.scheme())
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(
            "Expected an HTTP(S) model base URL without credentials, query, or fragment".into(),
        );
    }
    let path = format!("{}/{route}", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url.to_string())
}

fn openai_key() -> Result<String, String> {
    env("OPENAI_API_KEY")
        .filter(|key| !key.trim_matches(crate::registry::js_space).is_empty())
        .ok_or_else(|| "Set OPENAI_API_KEY to use OpenAI".to_owned())
}

/// `generatorSettings(provider, model, baseUrl, apiKey, { maxInputBytes })`
pub fn generator_settings(
    provider: &str,
    model: Option<&str>,
    base_url: Option<&str>,
    api_key: Option<String>,
    max_input_bytes: Option<&ConfigValue>,
) -> Result<ConfigValue, String> {
    let (url, key) = match provider {
        "openai" => (
            endpoint(
                base_url
                    .map(str::to_owned)
                    .or_else(|| env("OPENAI_BASE_URL"))
                    .as_deref()
                    .unwrap_or(OPENAI_BASE),
                "responses",
            )?,
            match api_key {
                Some(key) => Some(key),
                None => Some(openai_key()?),
            },
        ),
        "ollama" => (
            endpoint(
                base_url
                    .map(str::to_owned)
                    .or_else(|| env("OLLAMA_BASE_URL"))
                    .as_deref()
                    .unwrap_or(OLLAMA_BASE),
                "api/chat",
            )?,
            api_key,
        ),
        "strata" => (
            endpoint(
                base_url
                    .map(str::to_owned)
                    .or_else(|| env("STRATA_BASE_URL"))
                    .as_deref()
                    .unwrap_or(STRATA_BASE),
                "chat/completions",
            )?,
            api_key.or_else(|| env("STRATA_API_KEY")),
        ),
        _ => return Err("Expected --provider openai, ollama, or strata".into()),
    };
    let mut entries = vec![
        ("provider", text(provider)),
        ("model", model.map_or(ConfigValue::Null, text)),
        ("url", text(&url)),
    ];
    if let Some(key) = key.filter(|key| !key.is_empty()) {
        entries.push(("apiKey", text(&key)));
    }
    if let Some(bytes) = max_input_bytes.filter(|value| !matches!(value, ConfigValue::Null)) {
        entries.push(("maxInputBytes", bytes.clone()));
    }
    // `model` is left out when it is undefined.
    if model.is_none() {
        entries.retain(|(name, _)| *name != "model");
    }
    Ok(object(entries))
}

/// `embeddingEndpoint(provider, model, baseUrl, apiKey)`
pub fn embedding_endpoint(
    provider: &str,
    model: Option<&str>,
    base_url: Option<&str>,
    api_key: Option<String>,
) -> Result<ConfigValue, String> {
    let (url, key) = match provider {
        "ollama" => (
            endpoint(
                base_url
                    .map(str::to_owned)
                    .or_else(|| env("OLLAMA_BASE_URL"))
                    .as_deref()
                    .unwrap_or(OLLAMA_BASE),
                "api/embed",
            )?,
            api_key,
        ),
        "openai" => (
            endpoint(
                base_url
                    .map(str::to_owned)
                    .or_else(|| env("OPENAI_BASE_URL"))
                    .as_deref()
                    .unwrap_or(OPENAI_BASE),
                "embeddings",
            )?,
            match api_key {
                Some(key) => Some(key),
                None => Some(openai_key()?),
            },
        ),
        _ => return Err("Expected --embedding-provider ollama or openai".into()),
    };
    let mut entries = vec![("provider", text(provider))];
    if let Some(model) = model {
        entries.push(("model", text(model)));
    }
    entries.push(("url", text(&url)));
    if let Some(key) = key {
        entries.push(("apiKey", text(&key)));
    }
    Ok(object(entries))
}

/// A model entry under `models`, when it exists.
pub fn model<'a>(models: Option<&'a ConfigValue>, name: &str) -> Option<&'a ConfigValue> {
    models.and_then(|models| models.get(name))
}

/// `embeddingSettings(config, name)`: the embedding model resolved for requests.
pub fn embedding_settings(
    models: Option<&ConfigValue>,
    name: &str,
) -> Result<ConfigValue, (u8, &'static str, &'static str)> {
    let entry = model(models, name);
    let kind = entry.and_then(|entry| string_field(entry, "kind"));
    let value = entry.and_then(|entry| entry.get("model"));
    if kind != Some("embedding") || !crate::index_stage::truthy(value) {
        return Err((
            2,
            "VECTOR_MODEL_REQUIRED",
            "Define an embedding model under models.",
        ));
    }
    let entry = entry.unwrap_or(&ConfigValue::Null);
    let resolved = (|| {
        let model = resolve_value(entry.get("model"), "model")?;
        let base = resolve_value(entry.get("base_url"), "base_url")?;
        let key = resolve_api_key(entry.get("api_key_env"), "model")?;
        embedding_endpoint(
            string_field(entry, "provider").unwrap_or_default(),
            model.as_deref(),
            base.as_deref(),
            key,
        )
    })();
    let Ok(ConfigValue::Object(mut entries)) = resolved else {
        return Err((
            2,
            "INVALID_VECTOR_MODEL",
            "Check the embedding provider, model, endpoint and environment references.",
        ));
    };
    if let Some(bytes) = entry.get("max_input_bytes") {
        crate::index_stage::assign(&mut entries, "maxInputBytes", Some(bytes.clone()));
    }
    Ok(ConfigValue::Object(entries))
}

/// `modelSetting(project, build, name)`: `{ provider, model, baseUrl, apiKeyEnv, maxInputBytes }`.
pub fn model_setting(models: Option<&ConfigValue>, name: &str) -> Result<Option<ConfigValue>, ()> {
    let Some(entry) = model(models, name) else {
        return Ok(None);
    };
    let provider = string_field(entry, "provider").unwrap_or_default();
    let fallback = match provider {
        "openai" => env("OPENAI_BASE_URL").unwrap_or_else(|| "https://api.openai.com/v1".into()),
        "ollama" => env("OLLAMA_BASE_URL").unwrap_or_else(|| "http://127.0.0.1:11434".into()),
        "strata" => env("STRATA_BASE_URL").unwrap_or_else(|| "http://127.0.0.1:8080/v1".into()),
        _ => String::new(),
    };
    let base = resolve_value(entry.get("base_url"), "base_url").map_err(|_| ())?;
    let url = url::Url::parse(base.as_deref().unwrap_or(&fallback)).map_err(|_| ())?;
    if !["http", "https"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    let model = resolve_value(entry.get("model"), "model").map_err(|_| ())?;
    let mut entries = Vec::new();
    if let Some(provider) = entry.get("provider") {
        entries.push(("provider", provider.clone()));
    }
    entries.push(("model", model.as_deref().map_or(ConfigValue::Null, text)));
    entries.push(("baseUrl", text(url.as_str())));
    if let Some(key) = entry.get("api_key_env") {
        entries.push(("apiKeyEnv", key.clone()));
    }
    if let Some(bytes) = entry.get("max_input_bytes") {
        entries.push(("maxInputBytes", bytes.clone()));
    }
    if model.is_none() {
        entries.retain(|(name, _)| *name != "model");
    }
    Ok(Some(object(entries)))
}
