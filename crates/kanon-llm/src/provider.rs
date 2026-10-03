//! Named model-provider endpoints and the `provider/model` routing they enable.
//!
//! # Why endpoints are named
//! A model reference is `<provider>/<model-id>`, so the node has to answer "which endpoint serves
//! the provider called `xiaomi`?" without consulting the network. [`ProviderRegistry`] is that
//! answer: a small, hot-swappable directory of named endpoints, each carrying its own protocol,
//! base URL and credential. Wiring it here (rather than in the management layer) keeps one owner
//! for the rule that decides which endpoint a model is sent to — the pipeline, the CLI command and
//! the console all observe the same registry.
//!
//! # There is no default provider
//! Which model answers by default is the *node's* decision (one global default model), not a
//! property of an endpoint. The registry therefore never guesses: a reference must name a
//! configured endpoint, and anything else is an explicit error. That includes aggregator ids
//! (OpenRouter and friends publish ids that already contain a vendor prefix): they are addressed as
//! `openrouter/anthropic/claude-3.5-sonnet`, where only the first segment is the provider and the
//! remainder reaches the endpoint verbatim.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::gateway::{LlmProvider, build_provider_with_reasoning_replay};
use crate::model::ModelRef;

/// One named model-provider endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// Operator-chosen name used as the prefix of every model reference it serves.
    pub name: String,
    /// Wire protocol: `openai`, `openai_reasoning`, `openai_responses` or `anthropic`.
    pub protocol: String,
    /// Endpoint base URL, e.g. `https://api.xiaomimimo.com/v1`.
    pub base_url: String,
    /// Credential, when the endpoint requires one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Default sampling temperature for models served by this endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Default generation ceiling for models served by this endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Replay retained reasoning to compatible endpoints; never controls storage or display.
    #[serde(default = "default_replay_reasoning")]
    pub replay_reasoning: bool,
}

fn default_replay_reasoning() -> bool {
    true
}

impl ProviderEntry {
    /// Creates an entry with the mandatory fields and no tuning overrides.
    pub fn new(
        name: impl Into<String>,
        protocol: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into().trim().to_string(),
            protocol: protocol.into().trim().to_lowercase(),
            base_url: base_url.into().trim().to_string(),
            api_key: None,
            temperature: None,
            max_tokens: None,
            replay_reasoning: true,
        }
    }

    /// Validates the fields the registry and the HTTP client require.
    ///
    /// The protocol is checked by constructing the client through [`build_provider_with_reasoning_replay`], the single
    /// owner of the protocol switch, so an endpoint accepted here can never be rejected later when
    /// the registry builds its client.
    pub fn validate(&self) -> Result<(), String> {
        self.build_client().map(|_| ())
    }

    /// Validates the declaration and retains the client that proved the protocol is supported.
    fn build_client(&self) -> Result<Arc<dyn LlmProvider>, String> {
        if self.name.trim().is_empty() {
            return Err("provider name must not be empty".to_string());
        }
        if self.protocol.trim().is_empty() {
            return Err(format!("provider '{}' has no protocol", self.name));
        }
        if !self.base_url.starts_with("http://") && !self.base_url.starts_with("https://") {
            return Err(format!(
                "provider '{}' base URL '{}' must start with http:// or https://",
                self.name, self.base_url
            ));
        }
        build_provider_with_reasoning_replay(
            &self.protocol,
            self.base_url.clone(),
            self.api_key.clone(),
            String::new(),
            self.replay_reasoning,
        )
        .map_err(|err| format!("provider '{}': {err}", self.name))
    }

    /// Returns whether a usable credential is present.
    pub fn has_api_key(&self) -> bool {
        self.api_key
            .as_ref()
            .is_some_and(|key| !key.trim().is_empty())
    }

    /// Returns a copy with the credential removed, safe to report to a console.
    pub fn without_secret(&self) -> Self {
        Self {
            api_key: None,
            ..self.clone()
        }
    }
}

/// A model reference resolved against a concrete endpoint.
#[derive(Clone)]
pub struct ResolvedProvider {
    /// Name of the endpoint that will serve the request.
    pub provider_name: String,
    /// Live provider client for that endpoint.
    pub provider: Arc<dyn LlmProvider>,
    /// Model id to send upstream (prefix stripped when the endpoint matched).
    pub model: String,
}

impl std::fmt::Debug for ResolvedProvider {
    /// Reports the routing decision only; the client owns credentials and HTTP state.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedProvider")
            .field("provider_name", &self.provider_name)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// One declaration and its client, replaced together so credentials cannot become stale.
struct RegisteredProvider {
    entry: ProviderEntry,
    client: Arc<dyn LlmProvider>,
}

/// Directory of named provider endpoints and their shared clients.
#[derive(Default)]
pub struct ProviderRegistry {
    /// Configured endpoints by name.
    entries: RwLock<BTreeMap<String, RegisteredProvider>>,
}

impl std::fmt::Debug for ProviderRegistry {
    /// Reports the configured names, never credentials.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field(
                "providers",
                &self.entries().keys().cloned().collect::<Vec<String>>(),
            )
            .finish()
    }
}

impl ProviderRegistry {
    /// Creates an empty registry (no provider configured: chat is disabled).
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a registry holding a single endpoint.
    pub fn single(entry: ProviderEntry) -> Result<Self, String> {
        let registry = Self::new();
        registry.replace(vec![entry])?;
        Ok(registry)
    }

    /// Replaces the whole directory and drops every cached client.
    ///
    /// Validation happens before anything is swapped, so a rejected directory leaves the previous
    /// one serving.
    pub fn replace(&self, entries: Vec<ProviderEntry>) -> Result<(), String> {
        let mut map = BTreeMap::new();
        for entry in entries {
            if map.contains_key(&entry.name) {
                return Err(format!("provider '{}' is defined twice", entry.name));
            }
            let client = entry.build_client()?;
            map.insert(entry.name.clone(), RegisteredProvider { entry, client });
        }

        // Validation already constructs every client. Publish those clients with their entries
        // in one swap instead of lazily rebuilding and racing a separate cache invalidation.
        *self
            .entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = map;
        Ok(())
    }

    /// Publishes an already validated directory without constructing its clients a second time.
    pub(crate) fn replace_with(&self, staged: Self) {
        let entries = staged
            .entries
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *self
            .entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = entries;
    }

    /// Every configured endpoint, ordered by name.
    pub fn list(&self) -> Vec<ProviderEntry> {
        self.entries()
            .values()
            .map(|registered| registered.entry.clone())
            .collect()
    }

    /// Configured endpoint names, ordered.
    pub fn names(&self) -> Vec<String> {
        self.entries().keys().cloned().collect()
    }

    /// Looks an endpoint up by name.
    pub fn get(&self, name: &str) -> Option<ProviderEntry> {
        self.entries()
            .get(name.trim())
            .map(|registered| registered.entry.clone())
    }

    /// Whether the directory holds no endpoint.
    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }

    /// Resolves a model reference into an endpoint and the model id to send upstream.
    ///
    /// The reference must be `<provider>/<model-id>` with `<provider>` naming a configured
    /// endpoint; the prefix is stripped and the remainder is sent upstream untouched.
    pub fn resolve(&self, reference: &ModelRef) -> Result<ResolvedProvider, String> {
        let provider_name = reference.provider().ok_or_else(|| {
            format!(
                "model '{}' names no provider; write it as <provider>/<model-id>",
                reference.canonical()
            )
        })?;

        let entries = self.entries();
        let registered = entries.get(provider_name).ok_or_else(|| {
            format!(
                "model '{}' names provider '{provider_name}', which is not configured (configured: {})",
                reference.canonical(),
                entries.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })?;

        Ok(ResolvedProvider {
            provider_name: provider_name.to_string(),
            provider: registered.client.clone(),
            model: reference.model().to_string(),
        })
    }

    /// Acquires the entries guard, recovering from poisoning for the documented reason.
    fn entries(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, RegisteredProvider>> {
        self.entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
