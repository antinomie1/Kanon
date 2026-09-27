//! Discovery of the models an endpoint publishes, with whatever metadata it reports.
//!
//! # Why this exists
//! Per-model settings (context window, input modalities) are properties of the model, and the only
//! party that knows them for certain is the endpoint that serves it. The console therefore offers a
//! "discover" action that reads the provider's own model listing and turns it into catalog entries.
//! Everything an endpoint does not report stays `None` — the node never invents a context window,
//! because a wrong one silently truncates or overflows real conversations.

use kanon_llm::{ModelCapabilities, ModelSettingsSource, ModelSpec, ProviderEntry};
use serde_json::Value;

/// Upper bound on the number of models ingested from one endpoint.
///
/// A misconfigured base URL that answers with an HTML page or an unbounded listing must not grow
/// the node's configuration document without limit.
const MAX_DISCOVERED_MODELS: usize = 2_000;

/// Fetches the models an endpoint publishes and converts them into catalog entries.
///
/// The endpoint shape is chosen by protocol: `anthropic` uses `/v1/models` with the Anthropic auth
/// header, everything else uses the OpenAI-compatible `/v1/models` with a bearer token. Both
/// formats are parsed leniently because proxies reorder and rename fields.
pub async fn discover_models(entry: &ProviderEntry) -> Result<Vec<ModelSpec>, String> {
    entry.validate()?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|err| format!("Failed to build HTTP client: {err}"))?;

    let base_url = entry.base_url.trim().trim_end_matches('/');
    let models_url = if base_url.ends_with("/models") {
        base_url.to_string()
    } else if base_url.ends_with("/v1") {
        format!("{base_url}/models")
    } else {
        format!("{base_url}/v1/models")
    };

    let mut request = client.get(&models_url);
    if entry.protocol == "anthropic" {
        if let Some(key) = entry.api_key.as_deref().filter(|k| !k.trim().is_empty()) {
            request = request
                .header("x-api-key", key.trim())
                .header("anthropic-version", "2023-06-01");
        }
    } else if let Some(key) = entry.api_key.as_deref().filter(|k| !k.trim().is_empty()) {
        request = request.bearer_auth(key.trim());
    }

    let response = request
        .send()
        .await
        .map_err(|err| format!("Failed to reach provider at {models_url}: {err}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "Provider returned HTTP {status} for {models_url}: {body}"
        ));
    }

    let payload: Value = response
        .json()
        .await
        .map_err(|err| format!("Failed to parse the provider model listing: {err}"))?;

    Ok(parse_model_listing(&entry.name, &payload))
}

/// Parses either the OpenAI-compatible or the Ollama listing shape into catalog entries.
pub fn parse_model_listing(provider: &str, payload: &Value) -> Vec<ModelSpec> {
    let items = payload
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| payload.get("models").and_then(Value::as_array))
        .or_else(|| payload.as_array());

    let Some(items) = items else {
        return Vec::new();
    };

    let mut models = Vec::new();
    for item in items.iter().take(MAX_DISCOVERED_MODELS) {
        if let Some(spec) = parse_model_entry(provider, item) {
            models.push(spec);
        }
    }
    models.sort_by(|a, b| a.model.cmp(&b.model));
    models.dedup_by(|a, b| a.model == b.model);
    models
}

/// Parses one entry of a model listing.
fn parse_model_entry(provider: &str, item: &Value) -> Option<ModelSpec> {
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| item.get("name").and_then(Value::as_str))?
        .trim();
    if id.is_empty() {
        return None;
    }

    let mut spec = ModelSpec::new(provider, id);
    spec.source = ModelSettingsSource::Upstream;
    spec.display_name = item
        .get("display_name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string);

    spec.context_length = first_u32(
        item,
        &[
            "context_length",
            "context_window",
            "max_context_length",
            "max_input_tokens",
        ],
    );
    spec.max_output_tokens = first_u32(
        item,
        &["max_output_tokens", "max_completion_tokens", "max_tokens"],
    )
    .or_else(|| nested_u32(item, "top_provider", "max_completion_tokens"));

    if let Some(modalities) = item
        .get("architecture")
        .and_then(|architecture| architecture.get("input_modalities"))
        .and_then(Value::as_array)
    {
        spec.capabilities = capabilities_from_modalities(modalities);
    }

    Some(spec)
}

/// Reads the first numeric field present at the top level of an entry.
fn first_u32(item: &Value, keys: &[&str]) -> Option<u32> {
    keys.iter()
        .find_map(|key| item.get(*key).and_then(value_to_u32))
}

/// Reads a numeric field nested one level deep.
fn nested_u32(item: &Value, outer: &str, inner: &str) -> Option<u32> {
    item.get(outer)?.get(inner).and_then(value_to_u32)
}

/// Converts a JSON number into a token count, rejecting non-integers and zero.
fn value_to_u32(value: &Value) -> Option<u32> {
    let number = value
        .as_u64()
        .or_else(|| value.as_f64().map(|n| n as u64))?;
    if number == 0 {
        return None;
    }
    u32::try_from(number).ok()
}

/// Derives capability flags from an OpenRouter-style modality list.
fn capabilities_from_modalities(modalities: &[Value]) -> ModelCapabilities {
    let has = |needle: &str| {
        modalities.iter().any(|m| {
            m.as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(needle))
        })
    };

    ModelCapabilities {
        vision: has("image"),
        audio: has("audio"),
        video: has("video"),
        ..ModelCapabilities::default()
    }
}
