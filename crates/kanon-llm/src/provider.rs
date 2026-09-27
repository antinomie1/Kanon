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
//! # Why an unknown prefix falls back instead of failing
//! Aggregators (OpenRouter and friends) publish model ids that already contain a vendor prefix
//! (`anthropic/claude-3.5-sonnet`). When the prefix does not name a configured endpoint, the
//! reference is passed through to the default endpoint *verbatim*, which is exactly the id such an
//! aggregator expects. When the prefix does name an endpoint, it is stripped and the remainder is
//! sent upstream, which is what a first-party endpoint expects. Both behaviours are explicit, and
//! neither silently rewrites a model id an operator typed.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::gateway::{LlmProvider, build_provider};
use crate::model::ModelRef;

/// One named model-provider endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// Operator-chosen name used as the prefix of every model reference it serves.
    pub name: String,
    /// Wire protocol: `openai`, `openai_responses` or `anthropic`.
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
        }
    }

    /// Validates the fields the registry and the HTTP client require.
    pub fn validate(&self) -> Result<(), String> {
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
        Ok(())
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

/// Directory of named provider endpoints with lazily built, cached clients.
#[derive(Default)]
pub struct ProviderRegistry {
    /// Configured endpoints by name.
    entries: RwLock<BTreeMap<String, ProviderEntry>>,
    /// Name of the endpoint used when a reference carries no provider.
    default_provider: RwLock<Option<String>>,
    /// Built clients by provider name, invalidated whenever the directory is replaced.
    clients: RwLock<BTreeMap<String, Arc<dyn LlmProvider>>>,
}

impl std::fmt::Debug for ProviderRegistry {
    /// Reports the configured names, never credentials.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field(
                "providers",
                &self.entries().keys().cloned().collect::<Vec<String>>(),
            )
            .field("default_provider", &self.default_provider())
            .finish()
    }
}

impl ProviderRegistry {
    /// Creates an empty registry (no provider configured: chat is disabled).
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a registry holding a single default provider.
    pub fn single(entry: ProviderEntry) -> Result<Self, String> {
        let registry = Self::new();
        let name = entry.name.clone();
        registry.replace(vec![entry], Some(name))?;
        Ok(registry)
    }

    /// Replaces the whole directory and drops every cached client.
    ///
    /// The default must name a configured endpoint: a default pointing nowhere would make every
    /// unprefixed model reference fail at call time instead of at configuration time.
    pub fn replace(
        &self,
        entries: Vec<ProviderEntry>,
        default_provider: Option<String>,
    ) -> Result<(), String> {
        let mut map = BTreeMap::new();
        for entry in entries {
            entry.validate()?;
            if map.contains_key(&entry.name) {
                return Err(format!("provider '{}' is defined twice", entry.name));
            }
            map.insert(entry.name.clone(), entry);
        }

        if let Some(name) = default_provider.as_ref() {
            if !map.contains_key(name) {
                return Err(format!(
                    "default provider '{name}' is not among the configured providers"
                ));
            }
        }

        self.clients
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        *self
            .entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = map;
        *self
            .default_provider
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = default_provider;
        Ok(())
    }

    /// Every configured endpoint, ordered by name.
    pub fn list(&self) -> Vec<ProviderEntry> {
        self.entries().values().cloned().collect()
    }

    /// Configured endpoint names, ordered.
    pub fn names(&self) -> Vec<String> {
        self.entries().keys().cloned().collect()
    }

    /// Looks an endpoint up by name.
    pub fn get(&self, name: &str) -> Option<ProviderEntry> {
        self.entries().get(name.trim()).cloned()
    }

    /// Name of the default endpoint, when one is configured.
    pub fn default_provider(&self) -> Option<String> {
        self.default_provider
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Whether the directory holds no endpoint.
    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }

    /// Resolves a model reference into an endpoint and the model id to send upstream.
    pub fn resolve(&self, reference: &ModelRef) -> Result<ResolvedProvider, String> {
        let (provider_name, upstream_model) = match reference.provider() {
            Some(name) if self.get(name).is_some() => {
                (name.to_string(), reference.model().to_string())
            }
            // An unregistered prefix is an aggregator-style model id: keep the whole string and
            // let the default endpoint parse it.
            Some(_) => {
                let default = self.default_provider().ok_or_else(|| {
                    format!(
                        "model '{}' names provider '{}', which is not configured, and the node has no default provider",
                        reference.canonical(),
                        reference.provider().unwrap_or_default()
                    )
                })?;
                (default, reference.canonical())
            }
            None => {
                let default = self.default_provider().ok_or_else(|| {
                    format!(
                        "model '{}' carries no provider and the node has no default provider",
                        reference.canonical()
                    )
                })?;
                (default, reference.model().to_string())
            }
        };

        let provider = self.client_for(&provider_name)?;
        Ok(ResolvedProvider {
            provider_name,
            provider,
            model: upstream_model,
        })
    }

    /// Builds (or returns the cached) client for one configured endpoint.
    fn client_for(&self, name: &str) -> Result<Arc<dyn LlmProvider>, String> {
        if let Some(cached) = self
            .clients
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(name)
            .cloned()
        {
            return Ok(cached);
        }

        let entry = self
            .get(name)
            .ok_or_else(|| format!("provider '{name}' is not configured"))?;

        let client = build_provider(
            &entry.protocol,
            entry.base_url.clone(),
            entry.api_key.clone(),
            String::new(),
        )?;

        self.clients
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(name.to_string(), client.clone());
        Ok(client)
    }

    /// Acquires the entries guard, recovering from poisoning for the documented reason.
    fn entries(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, ProviderEntry>> {
        self.entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
