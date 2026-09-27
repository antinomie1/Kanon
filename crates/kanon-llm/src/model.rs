//! Model identity, capabilities and per-model settings.
//!
//! # Why a model is addressed as `<provider>/<model-id>`
//! The same underlying model id can be served by different endpoints with different credentials,
//! quotas and context limits (`xiaomi/mimo-v2.6-flash` versus an aggregator's
//! `openrouter/xiaomi/mimo-v2.6-flash`). Treating the whole string as an opaque model name — as
//! the gateway used to — makes those two indistinguishable and sends the provider prefix upstream,
//! where it is an invalid model id. [`ModelRef`] therefore splits the reference once, at the first
//! `/`, and keeps the remainder verbatim so aggregator ids that legitimately contain a slash still
//! round-trip.
//!
//! # Why settings live in a catalog
//! Context length and input modalities are properties of the *model*, not of a conversation. They
//! are looked up per request (the pipeline decides whether an image may be attached) and are
//! populated from the provider's own `/models` payload when it carries them, falling back to a
//! conservative built-in default: unknown models are assumed to accept tools and to reject images,
//! which degrades to a textual placeholder instead of a request the upstream rejects.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// A model reference of the form `<provider>/<model-id>`.
///
/// A reference without a `/` carries no provider: the node resolves it against its default
/// provider, which is the documented behaviour for a single-endpoint deployment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelRef {
    /// Provider (endpoint) name, when the reference named one.
    provider: Option<String>,
    /// Model id exactly as the upstream endpoint expects it.
    model: String,
}

impl ModelRef {
    /// Parses `<provider>/<model-id>`, splitting at the first `/` only.
    ///
    /// An empty provider prefix (`/model-id`) is treated as "no provider" rather than as a
    /// provider named by the empty string.
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        match trimmed.split_once('/') {
            Some((provider, model)) if !provider.trim().is_empty() && !model.trim().is_empty() => {
                Self {
                    provider: Some(provider.trim().to_string()),
                    model: model.trim().to_string(),
                }
            }
            _ => Self {
                provider: None,
                model: trimmed.to_string(),
            },
        }
    }

    /// Creates a reference without a provider.
    pub fn bare(model: impl Into<String>) -> Self {
        Self {
            provider: None,
            model: model.into().trim().to_string(),
        }
    }

    /// Creates a fully qualified reference.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: Some(provider.into().trim().to_string()),
            model: model.into().trim().to_string(),
        }
    }

    /// Provider name, when the reference named one.
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    /// Model id as the upstream endpoint expects it.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns a copy resolved to an explicit provider name.
    pub fn with_provider(&self, provider: Option<&str>) -> Self {
        Self {
            provider: provider
                .map(str::to_string)
                .or_else(|| self.provider.clone()),
            model: self.model.clone(),
        }
    }

    /// Whether the reference names neither a provider nor a model.
    pub fn is_empty(&self) -> bool {
        self.provider.is_none() && self.model.is_empty()
    }

    /// Canonical `<provider>/<model-id>` rendering, or the bare model id without a provider.
    pub fn canonical(&self) -> String {
        match &self.provider {
            Some(provider) => format!("{provider}/{}", self.model),
            None => self.model.clone(),
        }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical())
    }
}

/// Input modalities and behaviours a model exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Whether the model accepts text input.
    ///
    /// Enabled by default and only turned off for a model that is genuinely not a chat model; the
    /// pipeline omits the textual projection when it is disabled, which is what an image-only
    /// endpoint expects.
    #[serde(default = "default_true")]
    pub text: bool,
    /// Whether the model accepts image input.
    #[serde(default)]
    pub vision: bool,
    /// Whether the model accepts audio input.
    #[serde(default)]
    pub audio: bool,
    /// Whether the model accepts video input.
    #[serde(default)]
    pub video: bool,
    /// Whether the model can call tools (native function calling or recoverable text markup).
    #[serde(default = "default_true")]
    pub tool_calling: bool,
    /// Whether the model emits a reasoning channel.
    #[serde(default)]
    pub reasoning: bool,
}

/// Serde default for capability flags that are safe to assume enabled.
fn default_true() -> bool {
    true
}

impl Default for ModelCapabilities {
    /// The conservative default: text and tools yes, media no.
    ///
    /// Assuming vision on an unknown model would make the node attach an image part an upstream
    /// endpoint may reject with an opaque 400; assuming no tools merely loses a capability the
    /// operator can enable explicitly.
    fn default() -> Self {
        Self {
            text: true,
            vision: false,
            audio: false,
            video: false,
            tool_calling: true,
            reasoning: false,
        }
    }
}

impl ModelCapabilities {
    /// Infers capabilities from a model id.
    ///
    /// Used when an endpoint's own listing reports no modality metadata — many
    /// OpenAI-compatible endpoints return bare ids, which used to make a vision model show up as
    /// text-only. The pattern list is short and explicit: a wrong guess costs an upstream 400, so
    /// it leans on well-known family markers and an operator can always correct it in the console.
    pub fn infer_from_model_id(model_id: &str) -> Self {
        let id = model_id.to_ascii_lowercase();
        let has = |needle: &str| id.contains(needle);

        let vision = [
            "vision",
            "vl",
            "multimodal",
            "omni",
            "gpt-4o",
            "gpt-4.1",
            "gpt-4.5",
            "gpt-5",
            "o1",
            "o3",
            "o4",
            "claude-3",
            "claude-4",
            "claude-sonnet",
            "claude-opus",
            "claude-haiku",
            "gemini",
            "llava",
            "pixtral",
            "internvl",
            "minicpm-v",
            "glm-4v",
            "glm-4.5v",
            "qwen-vl",
            "qwen2-vl",
            "qwen2.5-vl",
            "phi-3-vision",
            "phi-4-multimodal",
            "moondream",
            "idefics",
            "smolvlm",
            "gemma-3",
        ]
        .iter()
        .any(|needle| has(needle));

        let audio = [
            "audio",
            "realtime",
            "omni",
            "whisper",
            "voxtral",
            "qwen-audio",
            "qwen2-audio",
        ]
        .iter()
        .any(|needle| has(needle));

        let video = has("video");

        let reasoning = [
            "reasoner",
            "-r1",
            "r1-",
            "thinking",
            "qwq",
            "magistral",
            "deepseek-r",
            "o1",
            "o3",
            "o4",
        ]
        .iter()
        .any(|needle| has(needle));

        Self {
            text: true,
            vision,
            audio,
            video,
            tool_calling: true,
            reasoning,
        }
    }
}

/// Provenance of a model's settings, so the console can explain where a value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSettingsSource {
    /// Nothing was known: conservative defaults apply.
    #[default]
    Unknown,
    /// Values reported by the provider's own model listing.
    Upstream,
    /// Values an operator typed in the console.
    Manual,
}

/// One model in the node's catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// Provider (endpoint) name this model is served by.
    pub provider: String,
    /// Model id as the upstream endpoint expects it.
    pub model: String,
    /// Optional human-readable label shown in the console and by `/model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Maximum context window in tokens, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u32>,
    /// Maximum tokens the model may generate in one reply, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Input modalities and behaviours.
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    /// Default sampling temperature for this model, when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Where these values came from.
    #[serde(default)]
    pub source: ModelSettingsSource,
}

impl ModelSpec {
    /// Creates a spec with conservative defaults for a provider/model pair.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into().trim().to_string(),
            model: model.into().trim().to_string(),
            display_name: None,
            context_length: None,
            max_output_tokens: None,
            capabilities: ModelCapabilities::default(),
            temperature: None,
            source: ModelSettingsSource::Unknown,
        }
    }

    /// Canonical `<provider>/<model-id>` name.
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    /// Reference to this model.
    pub fn reference(&self) -> ModelRef {
        ModelRef::new(self.provider.clone(), self.model.clone())
    }

    /// Label preferred by operator-facing output: the display name when set, else the model id.
    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.model)
    }
}

/// Thread-safe catalog of known models, shared by the CLI commands, the pipeline and the console.
///
/// The catalog is deliberately in-memory: persistence belongs to the node's configuration store,
/// which replaces the whole catalog on load and after every edit. That keeps one owner for the
/// document and one owner for the in-memory lookup.
#[derive(Debug, Default)]
pub struct ModelCatalog {
    /// Models keyed by canonical `<provider>/<model-id>` name.
    models: RwLock<BTreeMap<String, ModelSpec>>,
}

impl ModelCatalog {
    /// Creates an empty catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces every entry, dropping models that are no longer configured.
    pub fn replace(&self, models: Vec<ModelSpec>) {
        let mut guard = self.write();
        guard.clear();
        for spec in models {
            if spec.provider.trim().is_empty() || spec.model.trim().is_empty() {
                tracing::warn!(
                    provider = %spec.provider,
                    model = %spec.model,
                    "Ignoring a model catalog entry with an empty provider or model id"
                );
                continue;
            }
            guard.insert(spec.full_name(), spec);
        }
    }

    /// Inserts or replaces one entry.
    ///
    /// Rejects an entry without a provider or model id: such an entry can never be addressed by a
    /// `provider/model` reference, so storing it would only hide the operator's mistake.
    pub fn upsert(&self, spec: ModelSpec) -> Result<(), String> {
        if spec.provider.trim().is_empty() {
            return Err("model provider must not be empty".to_string());
        }
        if spec.model.trim().is_empty() {
            return Err("model id must not be empty".to_string());
        }
        self.write().insert(spec.full_name(), spec);
        Ok(())
    }

    /// Removes an entry by canonical name, returning whether it existed.
    pub fn remove(&self, full_name: &str) -> bool {
        self.write().remove(full_name.trim()).is_some()
    }

    /// Removes every model belonging to a provider, returning how many were dropped.
    ///
    /// Deleting a provider endpoint must not leave catalog entries pointing at a credential that
    /// no longer exists.
    pub fn remove_provider(&self, provider: &str) -> usize {
        let mut guard = self.write();
        let doomed: Vec<String> = guard
            .keys()
            .filter(|key| key.starts_with(&format!("{provider}/")))
            .cloned()
            .collect();
        let count = doomed.len();
        for key in doomed {
            guard.remove(&key);
        }
        count
    }

    /// Every entry, ordered by canonical name for stable output.
    pub fn list(&self) -> Vec<ModelSpec> {
        self.read().values().cloned().collect()
    }

    /// Looks an entry up by canonical `<provider>/<model-id>` name.
    pub fn get(&self, reference: &ModelRef) -> Option<ModelSpec> {
        self.read().get(&reference.canonical()).cloned()
    }

    /// Settings for a reference, synthesizing a conservative spec when the model is unknown.
    ///
    /// Never fails: an unknown model must still be usable (that is how a brand-new endpoint is
    /// tried out), it simply gets the conservative defaults and an explicit `unknown` source.
    pub fn settings_for(&self, reference: &ModelRef) -> ModelSpec {
        self.get(reference).unwrap_or_else(|| {
            let spec = match reference.provider() {
                Some(provider) => ModelSpec::new(provider, reference.model()),
                None => ModelSpec::new(String::new(), reference.model()),
            };
            spec
        })
    }

    /// Whether a reference accepts image input.
    pub fn supports_vision(&self, reference: &ModelRef) -> bool {
        self.get(reference)
            .map(|spec| spec.capabilities.vision)
            .unwrap_or(false)
    }

    /// Number of catalog entries.
    pub fn len(&self) -> usize {
        self.read().len()
    }

    /// Whether the catalog is empty.
    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    /// Acquires the read guard, recovering from poisoning (the guarded map cannot be inconsistent).
    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, ModelSpec>> {
        self.models
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Acquires the write guard, recovering from poisoning for the same reason as [`Self::read`].
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, BTreeMap<String, ModelSpec>> {
        self.models
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
