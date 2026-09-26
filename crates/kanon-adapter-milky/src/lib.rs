//! Milky protocol platform adapter for the Kanon microkernel.
//!
//! # What this crate is
//! An in-process (way-one) Kanon platform adapter for [Milky](https://milky.ntqqrev.org/), the QQ
//! bot application interface standard. It owns exactly one platform identifier, receives inbound
//! events from a Milky *protocol implementation* (Lagrange.Milky, Acidify/Yogurt, LuckyLilliaBot,
//! ...) and delivers outbound replies back through that implementation's HTTP API.
//!
//! # Layout
//! | Module | Responsibility |
//! | :--- | :--- |
//! | [`protocol`] | Vendored Milky 1.3 wire types: 21 events, both segment unions, all 65 endpoints |
//! | [`client`] | Typed HTTP client: one transport path, one error vocabulary |
//! | [`event_source`] | SSE and WebSocket readers with reconnection |
//! | [`mapping`] | Translation between Milky and the Kanon pipeline contract |
//! | [`config`] | Configuration shape, validation, environment bootstrap |
//! | [`adapter`] | [`PlatformAdapter`](kanon_core::PlatformAdapter) implementation and live status |
//!
//! # Operational shape
//! Milky implementations *serve* HTTP; this adapter is the client on both directions — it calls
//! `POST /api/:endpoint` for outbound messages and subscribes to `GET /event` for inbound ones.
//! Nothing listens on a port of its own, which is why the adapter needs no inbound route and why
//! the platform identifier only ever appears on the outbound routing path.
//!
//! # Example
//! ```no_run
//! use kanon_adapter_milky::{MilkyAdapter, MilkyConfig};
//!
//! # fn build() -> Result<MilkyAdapter, Box<dyn std::error::Error>> {
//! let adapter = MilkyAdapter::new(MilkyConfig {
//!     enabled: true,
//!     base_url: "http://127.0.0.1:3010".to_string(),
//!     access_token: Some("secret".to_string()),
//!     ..MilkyConfig::default()
//! })?;
//!
//! // Registration is the composition root's job (`supervisor.adapters().register(..)`), and the
//! // core's ingest handle arrives later through `PlatformAdapter::start`.
//! # Ok(adapter)
//! # }
//! ```

pub mod adapter;
pub mod client;
pub mod config;
pub mod event_source;
pub mod mapping;
pub mod protocol;

pub use adapter::{
    ConnectionState, MilkyAdapter, MilkyImplementation, MilkyLogin, MilkyStatus, MilkyTestReport,
};
pub use client::{MilkyClient, MilkyError};
pub use config::{ConfigError, DEFAULT_PLATFORM, MilkyConfig, TransportKind};
