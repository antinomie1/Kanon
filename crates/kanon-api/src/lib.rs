//! # Kanon API Module
//!
//! Headless management gateway for the Kanon microkernel: a decoupled control plane exposing
//! RESTful endpoints and real-time WebSocket channels for external WebUI consoles and
//! management clients.
//!
//! This crate is a **library only**: it owns no process entrypoint. [`crate::ApiServer`] and
//! [`crate::ApiState`] are assembled into a running node by the `kanon` binary in `crates/kanon`.
//!
//! ## Endpoints
//! - `GET  /api/v1/health` — liveness, uptime and memory footprint;
//! - `GET  /api/v1/metrics` — Prometheus text exposition;
//! - `GET  /api/v1/adapters` — platform adapter catalog (built-in and plugin);
//! - `POST /api/v1/adapters/:platform/ingest` — Fast-ACK inbound message ingress;
//! - `GET  /api/v1/plugins` — supervised hosts and plugin catalog;
//! - `GET  /api/v1/plugins/:id/config` — current values plus declaration schema;
//! - `PUT  /api/v1/plugins/:id/config` — validate, hot reload, persist;
//! - `POST /api/v1/plugins/:id/restart` — restart the owning host process;
//! - `GET  /api/v1/sessions` — paginated session metadata;
//! - `POST /api/v1/sessions/:id/reset` — clear history, keep persona and variables;
//! - `POST /api/v1/sessions/:id/persona` — hot-swap the session persona;
//! - `GET  /api/v1/personas` — persona catalog (built-in, operator-defined and instance personas);
//! - `POST /api/v1/personas` — create an operator-defined persona;
//! - `PUT  /api/v1/personas/:id` — edit one; `DELETE` removes it (refused while an instance uses it);
//! - `GET  /api/v1/providers` — named provider endpoints plus protocol presets;
//! - `POST /api/v1/providers` — create or replace one named provider endpoint;
//! - `POST /api/v1/providers/delete` — remove one named provider (and the default model it served);
//! - `POST /api/v1/providers/test` — probe a configured endpoint with its stored credential;
//! - `GET  /api/v1/models` — per-model settings catalog (`provider/model-id`);
//! - `PUT  /api/v1/models` — upsert one model catalog entry;
//! - `PUT  /api/v1/models/default` — set (or clear) the one global default model;
//! - `POST /api/v1/models/delete` — remove one model catalog entry;
//! - `POST /api/v1/models/discover` — read a provider's own model listing;
//! - `GET  /api/v1/system/reply-policy` — node-wide reply policy;
//! - `PUT  /api/v1/system/reply-policy` — update the node-wide reply policy;
//! - `POST /api/v1/chat/completions` — sandbox chat, JSON or `text/event-stream`;
//! - `GET  /ws/v1/logs` — structured log broadcast with level / plugin filters;
//! - `GET  /ws/v1/events` — end-to-end message lifecycle trace bus.
//!
//! ## Composition
//! The gateway never owns business state: it composes owners that already exist in the
//! microkernel — [`kanon_core::Supervisor`] for plugin processes, [`kanon_llm::SessionManager`]
//! and [`kanon_llm::PersonaRegistry`] for conversations, and the [`kanon_llm::Agent`] for
//! reasoning. [`state::ApiState::builder`] wires them together and, when a model provider is
//! supplied, attaches the trace event bus as an agent hook so LLM and tool-calling stages join
//! the same timeline as pipeline stages.
//!
//! ## Data plane
//! Platform adapters terminate the kernel's platform boundary: in-process adapters and
//! plugin-provided ones both implement the same [`kanon_core::PlatformAdapter`] contract and
//! appear on `GET /api/v1/adapters`.

pub mod adapters;
pub mod error;
pub mod llm_config;
pub mod metrics;
pub mod model_discovery;
pub mod observability;
pub mod persona_store;
pub mod plugin_config;
pub mod routes;
pub mod server;
pub mod session_storage;
pub mod state;
pub mod system;
pub mod ws;

pub use error::ApiError;
pub use kanon_core::ToggleStore;
pub use llm_config::{
    LlmProviderConfig, NodeSettings, SystemConfigStore, derive_provider_name, provider_presets,
};
pub use metrics::{MetricsRegistry, RuntimeGauges};
pub use observability::{
    LogLevel, LogRecord, Observability, TraceEvent, TraceEventBus, TraceRecord,
};
pub use persona_store::PersonaStore;
pub use plugin_config::PluginConfigStore;
pub use server::{ApiServer, app};
pub use session_storage::{DEFAULT_SESSION_DB, open_session_manager};
pub use state::{ApiState, ApiStateBuilder, default_agent_config};
