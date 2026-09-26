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
//! - `GET  /api/v1/personas` — persona catalog;
//! - `GET  /api/v1/providers` — model provider catalog plus the node's effective provider;
//! - `PUT  /api/v1/providers/active` — configure the node's provider (persisted, applied live);
//! - `DELETE /api/v1/providers/active` — clear the node's provider;
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
//! Platform adapters terminate the microkernel's platform boundary. [`adapters::WebhookAdapter`]
//! is bundled as a generic HTTP bridge; further adapters — in-process or plugin-provided — plug
//! into the same [`kanon_core::PlatformAdapter`] contract and appear on `GET /api/v1/adapters`.

pub mod adapters;
pub mod error;
pub mod llm_config;
pub mod metrics;
pub mod observability;
pub mod plugin_config;
pub mod routes;
pub mod server;
pub mod state;
pub mod system;
pub mod ws;

pub use adapters::WebhookAdapter;
pub use error::ApiError;
pub use kanon_core::ToggleStore;
pub use llm_config::{LlmProviderConfig, SystemConfigStore};
pub use metrics::{MetricsRegistry, RuntimeGauges};
pub use observability::{
    LogLevel, LogRecord, Observability, TraceEvent, TraceEventBus, TraceRecord,
};
pub use plugin_config::PluginConfigStore;
pub use server::{ApiServer, app};
pub use state::{ApiState, ApiStateBuilder, default_agent_config};
