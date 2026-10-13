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
//! # Configuration comes from files, never the environment
//! Everything an operator configures lives in this document: providers, models, policies and
//! adapters (usually edited through the console), plus the `startup` section the node reads
//! before it serves anything (edited by hand, applied on the next start). A deployment ships a
//! prepared `data/system.json` instead of environment variables, so the node's configuration is
//! always exactly what one file says.
//!
//! # One default model, no default provider
//! The node answers with exactly one *global default model* (`default_model`, a canonical
//! `<provider>/<model-id>`). Providers are only endpoints: none of them is "the default", so there
//! is no second setting that could disagree with the model. Documents written before this rule
//! still carry `default_provider` and the single-endpoint `llm` section; both are read (the latter
//! is migrated into a named provider) and never written back.

use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use kanon_adapter_milky::MilkyConfig;
use kanon_adapter_onebot::OneBotConfig;
use kanon_adapter_qqofficial::QqOfficialConfig;
use kanon_core::{BashPolicy, CommandPolicy, ContextPolicy, EventPolicy, ReplyPolicy};
use kanon_llm::{BUILTIN_AGENT, ModelRef, ModelSpec, ProviderEntry};
use serde::{Deserialize, Serialize};

/// Default location of the node's system configuration, relative to the node working directory.
pub const DEFAULT_SYSTEM_CONFIG: &str = "./data/system.json";

// Stores opened separately by startup and adapter code still write the same document. Hold one
// process-wide writer lock over the complete read/modify/replace operation, not just the rename.
// Config writes are infrequent; a path-indexed lock registry would add ownership complexity here.
static SYSTEM_CONFIG_WRITER: Mutex<()> = Mutex::new(());

/// One pre-configured provider template offered by the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
pub fn provider_presets() -> &'static [ProviderPresetDef] {
    &[
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
            protocol: "openai_reasoning",
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
#[derive(Debug, Clone)]
pub struct NodeSettings {
    /// Configured provider endpoints.
    pub providers: Vec<ProviderEntry>,
    /// Agent that answers for every instance without an agent override.
    pub default_agent: String,
    /// Canonical `<provider>/<model-id>` the node answers with by default.
    pub default_model: Option<String>,
    /// Per-model settings.
    pub models: Vec<ModelSpec>,
    /// Node-wide reply policy inherited by instances without an override.
    pub reply_policy: ReplyPolicy,
    /// Node-wide context-extras policy inherited by instances without an override.
    pub context_policy: ContextPolicy,
    /// Node-wide notice policy: which joins, pokes and recalls the bot reacts to.
    pub event_policy: EventPolicy,
    /// Node-wide command permissions and bot administrators.
    pub command_policy: CommandPolicy,
    /// Bash tool switch and execution backend; who may use it comes from `command_policy.admins`.
    pub bash_policy: BashPolicy,
}

impl Default for NodeSettings {
    /// A node nobody has configured: no provider or model, default policies, and the built-in
    /// agent, the one agent that needs no configuration of its own.
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            default_agent: BUILTIN_AGENT.to_string(),
            default_model: None,
            models: Vec::new(),
            reply_policy: ReplyPolicy::default(),
            context_policy: ContextPolicy::default(),
            event_policy: EventPolicy::default(),
            command_policy: CommandPolicy::default(),
            bash_policy: BashPolicy::default(),
        }
    }
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
        kanon_llm::check_agent_id(&self.default_agent)?;
        self.reply_policy.validate()?;
        self.command_policy.clone().prepare()?;
        self.bash_policy.validate()?;

        let mut names = std::collections::HashSet::new();
        for provider in &self.providers {
            provider.validate()?;
            if !names.insert(provider.name.as_str()) {
                return Err(format!("provider '{}' is defined twice", provider.name));
            }
        }

        let mut references = std::collections::HashSet::new();
        for model in &self.models {
            model.validate()?;
            if !names.contains(model.provider.as_str()) {
                return Err(format!(
                    "model '{}' names provider '{}', which is not configured",
                    model.full_name(),
                    model.provider
                ));
            }
            let reference = model.full_name();
            if !references.insert(reference.clone()) {
                return Err(format!("model '{reference}' is defined twice"));
            }
        }

        for (label, model) in [
            ("default model", self.default_model.as_deref()),
            (
                "Bash review model",
                self.bash_policy
                    .local
                    .review_model
                    .as_deref()
                    .filter(|model| !model.trim().is_empty()),
            ),
        ] {
            let Some(model) = model else {
                continue;
            };
            let reference = ModelRef::parse(model);
            match reference.provider() {
                Some(provider) if names.contains(provider) => {}
                Some(provider) => {
                    return Err(format!(
                        "{label} '{model}' names provider '{provider}', which is not configured"
                    ));
                }
                None => {
                    return Err(format!(
                        "{label} '{model}' must be written as <provider>/<model-id>"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Single-endpoint provider description stored under `llm` by documents written before named
/// providers existed. Read only to migrate such a document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmProviderConfig {
    /// Wire protocol: `openai`, `openai_reasoning`, `openai_responses` or `anthropic`.
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

/// Settings the node needs before it serves anything, stored under `startup`.
///
/// The console never writes this section; an operator edits it by hand and it takes effect on the
/// next start. Unknown keys are rejected so a misspelt setting fails loudly instead of being
/// ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StartupConfig {
    /// Management gateway bind address; loopback by default so it is never exposed by accident.
    pub api_addr: SocketAddr,
    /// Optional management password (HTTP Basic user `kanon`), required for non-loopback binds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_token: Option<String>,
    /// `tracing` filter directive, such as `info` or `kanon_core=debug,info`.
    pub log: String,
    /// Directory for the IPC sockets. When unset, the platform runtime directory is used
    /// (`$XDG_RUNTIME_DIR/kanon/run`, or a per-user directory under `/tmp`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_dir: Option<PathBuf>,
    /// Interpreter for TypeScript plugins. When unset, `bun` and then `node` are looked up on
    /// `PATH`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub typescript_runtime: Option<PathBuf>,
    /// Install a Python or TypeScript plugin's dependencies with its native tool (`uv sync`,
    /// `bun install`, `npm ci`) before launch when its environment is missing or out of date.
    /// When `false`, the operator installs them and a missing environment is reported instead.
    pub install_dependencies: bool,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            api_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            api_token: None,
            log: "info".to_string(),
            run_dir: None,
            typescript_runtime: None,
            install_dependencies: true,
        }
    }
}

/// Plugin market sources, stored under `plugin_market`.
///
/// Like `startup`, the console never writes this section: an operator lists the index URLs by
/// hand. It is read on every market request, so an edit applies on the next visit without a
/// restart. Unknown keys are rejected so a misspelt setting fails loudly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginMarketConfig {
    /// Index documents to read, in priority order (an id listed twice is taken from the first).
    pub indexes: Vec<String>,
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
    /// Agent the node answers with by default; absent in documents written before agents were
    /// selectable, which therefore keep the built-in agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_agent: Option<String>,
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
    /// Node-wide notice policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event_policy: Option<EventPolicy>,
    /// Node-wide command permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command_policy: Option<CommandPolicy>,
    /// Bash tool switch and execution backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bash_policy: Option<BashPolicy>,
    /// Milky platform adapter configuration, when one was saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milky: Option<MilkyConfig>,
    /// OneBot v11 adapter configuration, when saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    onebot: Option<OneBotConfig>,
    /// QQ Official adapter configuration, when saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    qqofficial: Option<QqOfficialConfig>,
    /// Startup settings; carried through every write, never changed by the console.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    startup: Option<StartupConfig>,
    /// Plugin market indexes; carried through every write, never changed by the console.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plugin_market: Option<PluginMarketConfig>,
    /// Every unrecognized key is carried through verbatim.
    ///
    /// The document is shared, forward-compatible node state: writing the provider must never
    /// discard settings another (possibly newer) component stored there.
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

impl LlmProviderConfig {
    /// Converts this single-endpoint description into the named directory form.
    ///
    /// Used for migrating a legacy document: the endpoint is registered under a name derived from its base URL so the resulting model
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
            replay_reasoning: true,
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
        .iter()
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

    /// Loads the startup settings, falling back to the defaults when the section is absent.
    pub fn load_startup(&self) -> Result<StartupConfig, String> {
        Ok(self
            .read_document()?
            .and_then(|document| document.startup)
            .unwrap_or_default())
    }

    /// Loads the plugin market sources, empty when the section is absent.
    pub fn load_plugin_market(&self) -> Result<PluginMarketConfig, String> {
        Ok(self
            .read_document()?
            .and_then(|document| document.plugin_market)
            .unwrap_or_default())
    }

    /// Loads the persisted OneBot v11 configuration.
    pub fn load_onebot(&self) -> Result<Option<OneBotConfig>, String> {
        Ok(self.read_document()?.and_then(|document| document.onebot))
    }

    /// Saves OneBot settings without changing other system configuration sections.
    pub fn save_onebot(&self, config: &OneBotConfig) -> Result<(), String> {
        let _writing = SYSTEM_CONFIG_WRITER
            .lock()
            .map_err(|_| "System config writer lock is poisoned")?;
        let mut document = self.read_document()?.unwrap_or_default();
        document.onebot = Some(config.clone());
        self.write_document(&document)
    }

    /// Loads the persisted QQ Official adapter configuration.
    pub fn load_qqofficial(&self) -> Result<Option<QqOfficialConfig>, String> {
        Ok(self
            .read_document()?
            .and_then(|document| document.qqofficial))
    }

    /// Saves QQ Official settings, preserving every other section.
    pub fn save_qqofficial(&self, config: &QqOfficialConfig) -> Result<(), String> {
        let _writing = SYSTEM_CONFIG_WRITER
            .lock()
            .map_err(|_| "System config writer lock is poisoned")?;
        let mut document = self.read_document()?.unwrap_or_default();
        document.qqofficial = Some(config.clone());
        self.write_document(&document)
    }

    /// Loads the persisted Milky adapter configuration, if the document carries one.
    pub fn load_milky(&self) -> Result<Option<MilkyConfig>, String> {
        Ok(self.read_document()?.and_then(|document| document.milky))
    }

    /// Persists the Milky adapter configuration, preserving every other section.
    pub fn save_milky(&self, config: &MilkyConfig) -> Result<(), String> {
        let _writing = SYSTEM_CONFIG_WRITER
            .lock()
            .map_err(|_| "System config writer lock is poisoned")?;
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

        let settings = NodeSettings {
            providers: document.providers.unwrap_or_default(),
            default_model: document.default_model,
            default_agent: document
                .default_agent
                .unwrap_or_else(|| BUILTIN_AGENT.to_string()),
            reply_policy: document.reply_policy.unwrap_or_default(),
            context_policy: document.context_policy.unwrap_or_default(),
            event_policy: document.event_policy.unwrap_or_default(),
            command_policy: document.command_policy.unwrap_or_default().prepare()?,
            bash_policy: document.bash_policy.unwrap_or_default(),
            models: document.models.unwrap_or_default(),
        };

        // Disk edits obey the same contract as API updates; never guess a replacement default.
        settings.validate()?;
        Ok(settings)
    }

    /// Persists the node's model-routing settings.
    ///
    /// The write is atomic (temporary file plus rename) so a crash mid-write can never leave a
    /// truncated document that would fail the next startup. Legacy sections are dropped here:
    /// after one save the document holds exactly the current shape.
    pub fn save_node_settings(&self, settings: &NodeSettings) -> Result<(), String> {
        let _writing = SYSTEM_CONFIG_WRITER
            .lock()
            .map_err(|_| "System config writer lock is poisoned")?;
        let mut document = self.read_document()?.unwrap_or_default();

        document.providers = Some(settings.providers.clone());
        document.default_agent = Some(settings.default_agent.clone());
        document.default_model = settings.default_model.clone();
        document.models = Some(settings.models.clone());
        document.reply_policy = Some(settings.reply_policy);
        document.context_policy = Some(settings.context_policy);
        document.event_policy = Some(settings.event_policy);
        document.command_policy = Some(settings.command_policy.clone());
        document.bash_policy = Some(settings.bash_policy.clone());

        self.write_document(&document)
    }

    /// Reads and parses the document, returning `None` when the file does not exist.
    fn read_document(&self) -> Result<Option<SystemConfigDocument>, String> {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("Failed to read {}: {err}", self.path.display())),
        };
        let mut document: SystemConfigDocument = serde_json::from_str(&raw)
            .map_err(|err| format!("Failed to parse {}: {err}", self.path.display()))?;

        // Every section writer must carry the migration forward before serialization drops `llm`.
        // An explicit provider directory, including an empty one, always wins over legacy data.
        if document.providers.is_none()
            && let Some(legacy) = document.llm.as_ref()
        {
            let migrated = legacy.into_node_settings();
            document.providers = Some(migrated.providers);
            if document.default_model.is_none() {
                document.default_model = migrated.default_model;
            }
        }
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

        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        // NamedTempFile creates an exclusive, owner-only file on Unix. Credentials are never
        // written to a shared name or exposed with default permissions before a later chmod.
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|err| format!("Failed to create system config temporary file: {err}"))?;
        temporary
            .write_all(payload.as_bytes())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|err| format!("Failed to write system config: {err}"))?;
        temporary
            .persist(&self.path)
            .map_err(|err| format!("Failed to replace {}: {err}", self.path.display()))?;
        Ok(())
    }
}
