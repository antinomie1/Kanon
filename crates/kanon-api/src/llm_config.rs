//! Node-local system configuration and its persistence.
//!
//! # Why this exists
//! The management console can change the model provider of a running node. A browser-local
//! setting would be a lie — it would survive neither a different browser nor a node restart —
//! so the provider description is persisted next to the node's own data (`data/system.json`)
//! and re-applied at startup. The file holds a credential, so it is written with mode `0600`
//! and never leaves the node.
//!
//! # One document, several sections
//! `data/system.json` is the node's own configuration document, not an LLM-specific file: the
//! Milky platform adapter stores its section here too. Every section is read-modify-written by
//! this one store, which is what guarantees that saving the model provider cannot erase the
//! adapter's settings, and vice versa. Unknown keys are carried through untouched so a newer
//! component's settings survive a downgrade.
//!
//! # Precedence
//! Providers saved here are the node's own configuration and win over the `KANON_LLM_*`
//! environment bootstrap. Environment variables remain the way to deploy a node with a provider
//! out of the box (containers, CI); the console is the way to change it afterwards. The Milky
//! adapter's configuration follows the same precedence.
//!
//! # One default model, no default provider
//! The node answers with exactly one *global default model* (`default_model`, a canonical
//! `<provider>/<model-id>`). Providers are only endpoints: none of them is "the default", so there
//! is no second setting that could disagree with the model. Documents written before this rule
//! still carry `default_provider` and the single-endpoint `llm` section; both are read (the latter
//! is migrated into a named provider) and never written back.

use std::path::{Path, PathBuf};

use kanon_adapter_milky::MilkyConfig;
use kanon_adapter_onebot::OneBotConfig;
use kanon_core::{ContextPolicy, ReplyPolicy};
use kanon_llm::{ModelRef, ModelSpec, ProviderEntry};
use serde::{Deserialize, Serialize};

/// Default location of the node's system configuration, relative to the node working directory.
pub const DEFAULT_SYSTEM_CONFIG: &str = "./data/system.json";

/// One pre-configured provider template offered by the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderPresetDef {
    /// Stable preset identifier, also used as the derived provider name.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Wire protocol.
    pub protocol: &'static str,
    /// Endpoint base URL.
    pub base_url: &'static str,
}

/// Provider templates offered when configuring an endpoint.
///
/// Exposed as one function (rather than a literal in the route handler) because the provider-name
/// migration matches a persisted base URL against exactly this list: a legacy single-provider
/// document must come back under the same name the console would offer.
pub fn provider_presets() -> Vec<ProviderPresetDef> {
    vec![
        ProviderPresetDef {
            id: "openai",
            name: "OpenAI Official",
            protocol: "openai",
            base_url: "https://api.openai.com/v1",
        },
        ProviderPresetDef {
            id: "anthropic",
            name: "Anthropic Claude",
            protocol: "anthropic",
            base_url: "https://api.anthropic.com/v1",
        },
        ProviderPresetDef {
            id: "deepseek",
            name: "DeepSeek",
            protocol: "openai",
            base_url: "https://api.deepseek.com/v1",
        },
        ProviderPresetDef {
            id: "xiaomi",
            name: "Xiaomi MiMo",
            protocol: "openai",
            base_url: "https://api.xiaomimimo.com/v1",
        },
        ProviderPresetDef {
            id: "ollama",
            name: "Ollama (Local)",
            protocol: "openai",
            base_url: "http://127.0.0.1:11434/v1",
        },
        ProviderPresetDef {
            id: "vllm",
            name: "vLLM (Local / Server)",
            protocol: "openai",
            base_url: "http://127.0.0.1:8000/v1",
        },
        ProviderPresetDef {
            id: "openrouter",
            name: "OpenRouter",
            protocol: "openai",
            base_url: "https://openrouter.ai/api/v1",
        },
        ProviderPresetDef {
            id: "siliconflow",
            name: "SiliconFlow (硅基流动)",
            protocol: "openai",
            base_url: "https://api.siliconflow.cn/v1",
        },
    ]
}

/// Everything `data/system.json` says about model routing.
///
/// One value is passed around instead of loose fields because these settings are always read,
/// written and applied together: a default model without its provider, or a model catalog without
/// the endpoints it belongs to, would be an inconsistent node.
#[derive(Debug, Clone, Default)]
pub struct NodeSettings {
    /// Configured provider endpoints.
    pub providers: Vec<ProviderEntry>,
    /// Canonical `<provider>/<model-id>` the node answers with by default.
    pub default_model: Option<String>,
    /// Per-model settings.
    pub models: Vec<ModelSpec>,
    /// Node-wide reply policy inherited by instances without an override.
    pub reply_policy: ReplyPolicy,
    /// Node-wide context-extras policy inherited by instances without an override.
    pub context_policy: ContextPolicy,
}

impl NodeSettings {
    /// Whether any provider is configured.
    pub fn has_providers(&self) -> bool {
        !self.providers.is_empty()
    }

    /// Checks the settings as a whole, before anything is persisted or applied.
    ///
    /// The default model must name a configured provider: a default pointing nowhere would leave
    /// the console describing a node that cannot answer.
    pub fn validate(&self) -> Result<(), String> {
        self.reply_policy.validate()?;

        let mut names = std::collections::HashSet::new();
        for provider in &self.providers {
            provider.validate()?;
            if !names.insert(provider.name.as_str()) {
                return Err(format!("provider '{}' is defined twice", provider.name));
            }
        }

        if let Some(model) = self.default_model.as_deref() {
            let reference = ModelRef::parse(model);
            match reference.provider() {
                Some(provider) if names.contains(provider) => {}
                Some(provider) => {
                    return Err(format!(
                        "default model '{model}' names provider '{provider}', which is not configured"
                    ));
                }
                None => {
                    return Err(format!(
                        "default model '{model}' must be written as <provider>/<model-id>"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Serializable description of one model provider, as the `KANON_LLM_*` environment describes it.
///
/// Field names mirror the environment variables. The same shape is what a document written before
/// named providers existed stored under `llm`, so it doubles as the migration source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmProviderConfig {
    /// Wire protocol: `openai`, `openai_responses` or `anthropic`.
    pub protocol: String,
    /// Provider base URL (without the trailing `/chat/completions`).
    pub base_url: String,
    /// Default model identifier used when a request does not name one.
    pub model: String,
    /// Provider credential. Optional: local runtimes such as Ollama need none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Sampling temperature applied to the node's agent, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Maximum generation tokens applied to the node's agent, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

/// Root document persisted in `data/system.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SystemConfigDocument {
    /// Legacy single-endpoint provider. Read for migration, never written back.
    #[serde(default, skip_serializing)]
    llm: Option<LlmProviderConfig>,
    /// Legacy default-provider name. Read and ignored, never written back: the default is a model.
    #[serde(default, skip_serializing)]
    #[allow(dead_code)]
    default_provider: Option<String>,
    /// Named provider endpoints, keyed by model-reference prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    providers: Option<Vec<ProviderEntry>>,
    /// Canonical `<provider>/<model-id>` the node answers with by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_model: Option<String>,
    /// Per-model settings catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    models: Option<Vec<ModelSpec>>,
    /// Node-wide reply policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reply_policy: Option<ReplyPolicy>,
    /// Node-wide context-extras policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_policy: Option<ContextPolicy>,
    /// Milky platform adapter configuration, when one was saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milky: Option<MilkyConfig>,
    /// OneBot v11 adapter configuration, when saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    onebot: Option<OneBotConfig>,
    /// Every unrecognized key is carried through verbatim.
    ///
    /// The document is shared, forward-compatible node state: writing the provider must never
    /// discard settings another (possibly newer) component stored there.
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

impl LlmProviderConfig {
    /// Reads a provider description from the `KANON_LLM_*` environment variables.
    ///
    /// Returns `None` when `KANON_LLM_BASE_URL` is unset or blank, which means "no provider
    /// configured by the environment" rather than an error. The variable set matches
    /// [`kanon_llm::provider_from_env`], which serves the standalone core binary.
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var("KANON_LLM_BASE_URL").ok()?;
        let base_url = base_url.trim().to_string();
        if base_url.is_empty() {
            return None;
        }

        Some(Self {
            protocol: std::env::var("KANON_LLM_PROTOCOL").unwrap_or_else(|_| "openai".to_string()),
            base_url,
            model: std::env::var("KANON_LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string()),
            api_key: std::env::var("KANON_LLM_API_KEY")
                .ok()
                .filter(|key| !key.trim().is_empty()),
            temperature: None,
            max_tokens: None,
        })
    }

    /// Converts this single-endpoint description into the named directory form.
    ///
    /// Used for the `KANON_LLM_*` environment bootstrap and for migrating a legacy document: the
    /// endpoint is registered under a name derived from its base URL so the resulting model
    /// reference is exactly what the console would have produced, and that model becomes the
    /// node's global default.
    pub fn into_node_settings(&self) -> NodeSettings {
        let name = derive_provider_name(&self.base_url);
        let provider = ProviderEntry {
            name: name.clone(),
            protocol: self.protocol.clone(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
        };
        NodeSettings {
            providers: vec![provider],
            // The model id is always addressed through the endpoint it was configured with:
            // an aggregator id such as `anthropic/claude-3.5-sonnet` becomes
            // `openrouter/anthropic/claude-3.5-sonnet`, where only the first segment routes.
            default_model: Some(format!("{name}/{}", self.model.trim())),
            ..NodeSettings::default()
        }
    }
}

/// Derives a provider name from an endpoint URL.
///
/// A known preset wins, so a URL configured before named providers existed comes back under the
/// name the console offers for the same endpoint (`api.xiaomimimo.com` becomes `xiaomi`). Any other
/// URL yields a sanitized host label, and an address without a hostname yields `local`.
pub fn derive_provider_name(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if let Some(preset) = provider_presets()
        .into_iter()
        .find(|preset| same_endpoint(preset.base_url, trimmed))
    {
        return preset.id.to_string();
    }

    let host = trimmed
        .split("://")
        .nth(1)
        .unwrap_or(trimmed)
        .split('/')
        .next()
        .unwrap_or_default();
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    let host = host
        .strip_prefix("api.")
        .or_else(|| host.strip_prefix("www."))
        .unwrap_or(host);

    let label = if host.is_empty() || host.chars().all(|c| c.is_ascii_digit() || c == '.') {
        "local".to_string()
    } else {
        host.split('.').next().unwrap_or(host).to_string()
    };

    let sanitized: String = label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('-').to_string();
    if sanitized.is_empty() {
        "local".to_string()
    } else {
        sanitized
    }
}

/// Compares two endpoint URLs for identity, ignoring a trailing slash and casing.
fn same_endpoint(left: &str, right: &str) -> bool {
    left.trim()
        .trim_end_matches('/')
        .eq_ignore_ascii_case(right.trim().trim_end_matches('/'))
}

/// Persistence for the node's `data/system.json` document.
#[derive(Debug, Clone)]
pub struct SystemConfigStore {
    /// Absolute or relative path of the persisted document.
    path: PathBuf,
}

impl Default for SystemConfigStore {
    fn default() -> Self {
        Self::new(DEFAULT_SYSTEM_CONFIG)
    }
}

impl SystemConfigStore {
    /// Creates a store bound to an explicit path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Path of the persisted document.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the persisted OneBot v11 configuration.
    pub fn load_onebot(&self) -> Result<Option<OneBotConfig>, String> {
        Ok(self.read_document()?.and_then(|document| document.onebot))
    }

    /// Saves OneBot settings without changing other system configuration sections.
    pub fn save_onebot(&self, config: &OneBotConfig) -> Result<(), String> {
        let mut document = self.read_document()?.unwrap_or_default();
        document.onebot = Some(config.clone());
        self.write_document(&document)
    }

    /// Loads the persisted Milky adapter configuration, if the document carries one.
    pub fn load_milky(&self) -> Result<Option<MilkyConfig>, String> {
        Ok(self.read_document()?.and_then(|document| document.milky))
    }

    /// Persists the Milky adapter configuration, preserving every other section.
    pub fn save_milky(&self, config: &MilkyConfig) -> Result<(), String> {
        let mut document = self.read_document()?.unwrap_or_default();
        document.milky = Some(config.clone());
        self.write_document(&document)
    }

    /// Loads the node's model-routing settings, migrating a legacy single-provider document.
    ///
    /// A document written before named providers existed carries only `llm`. It is migrated in
    /// memory (and rewritten on the next save) so an operator's working endpoint survives the
    /// upgrade instead of showing up as "no provider configured".
    pub fn load_node_settings(&self) -> Result<NodeSettings, String> {
        let document = match self.read_document()? {
            Some(document) => document,
            None => return Ok(NodeSettings::default()),
        };

        let mut settings = NodeSettings {
            reply_policy: document.reply_policy.unwrap_or_default(),
            context_policy: document.context_policy.unwrap_or_default(),
            models: document.models.unwrap_or_default(),
            ..NodeSettings::default()
        };

        match document.providers {
            Some(providers) if !providers.is_empty() => {
                settings.providers = providers;
                settings.default_model = document.default_model;
            }
            _ => {
                if let Some(legacy) = document.llm.as_ref() {
                    let migrated = legacy.into_node_settings();
                    settings.providers = migrated.providers;
                    settings.default_model = migrated.default_model;
                }
            }
        }

        // A default that names no configured endpoint would fail every conversation at call
        // time; dropping it here reports the honest state ("no default model") instead.
        if let Some(model) = settings.default_model.as_deref() {
            let reference = ModelRef::parse(model);
            let served = reference
                .provider()
                .is_some_and(|name| settings.providers.iter().any(|entry| entry.name == name));
            if !served {
                tracing::warn!(
                    default_model = %model,
                    "Persisted default model names no configured provider; starting without one"
                );
                settings.default_model = None;
            }
        }

        Ok(settings)
    }

    /// Persists the node's model-routing settings.
    ///
    /// The write is atomic (temporary file plus rename) so a crash mid-write can never leave a
    /// truncated document that would fail the next startup. Legacy sections are dropped here:
    /// after one save the document holds exactly the current shape.
    pub fn save_node_settings(&self, settings: &NodeSettings) -> Result<(), String> {
        let mut document = self.read_document()?.unwrap_or_default();

        document.providers = Some(settings.providers.clone());
        document.default_model = settings.default_model.clone();
        document.models = Some(settings.models.clone());
        document.reply_policy = Some(settings.reply_policy);
        document.context_policy = Some(settings.context_policy);

        self.write_document(&document)
    }

    /// Reads and parses the document, returning `None` when the file does not exist.
    fn read_document(&self) -> Result<Option<SystemConfigDocument>, String> {
        if !self.path.exists() {
            return Ok(None);
        }

        let raw = std::fs::read_to_string(&self.path)
            .map_err(|err| format!("Failed to read {}: {err}", self.path.display()))?;
        let document: SystemConfigDocument = serde_json::from_str(&raw)
            .map_err(|err| format!("Failed to parse {}: {err}", self.path.display()))?;

        Ok(Some(document))
    }

    /// Serializes and atomically writes the document with owner-only permissions.
    fn write_document(&self, document: &SystemConfigDocument) -> Result<(), String> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create {}: {err}", parent.display()))?;
        }

        let payload = serde_json::to_string_pretty(document)
            .map_err(|err| format!("Failed to serialize system config: {err}"))?;

        let temp_path = self.path.with_extension("json.tmp");
        std::fs::write(&temp_path, payload)
            .map_err(|err| format!("Failed to write {}: {err}", temp_path.display()))?;

        // The document holds a provider credential: restrict it to the node's own user before it
        // becomes visible under its final name.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|err| format!("Failed to restrict {}: {err}", temp_path.display()))?;
        }

        std::fs::rename(&temp_path, &self.path).map_err(|err| {
            format!(
                "Failed to move {} into place at {}: {err}",
                temp_path.display(),
                self.path.display()
            )
        })
    }
}
