//! Liveness, uptime and resource-footprint endpoint (`GET /api/v1/health`).

use std::sync::atomic::Ordering;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use serde::Serialize;

use crate::metrics::RuntimeGauges;
use crate::state::ApiState;
use crate::system::sample_process_memory;

/// Registers the health endpoint.
pub fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/health", get(health))
}

/// Health and basic runtime metric payload.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Liveness marker; `ok` whenever the microkernel is serving requests.
    pub status: &'static str,
    /// Gateway build version.
    pub version: String,
    /// Seconds elapsed since the gateway started.
    pub uptime_seconds: u64,
    /// Whether an LLM provider is configured (sandbox chat availability).
    pub llm_configured: bool,
    /// Process memory footprint.
    pub memory: MemorySection,
    /// Supervised plugin host summary.
    pub plugins: PluginSection,
    /// Bot instance summary: an adapter alone answers nothing until an instance claims it.
    pub instances: InstanceSection,
    /// Conversation session summary.
    pub sessions: SessionSection,
    /// Real-time channel summary.
    pub realtime: RealtimeSection,
}

/// Process memory footprint in bytes.
#[derive(Debug, Serialize)]
pub struct MemorySection {
    /// Resident set size, absent when the operating system refused to report it.
    pub resident_bytes: Option<u64>,
    /// Virtual address space size, absent when unavailable.
    pub virtual_bytes: Option<u64>,
}

/// Plugin host and plugin instance counts.
#[derive(Debug, Serialize)]
pub struct PluginSection {
    /// Number of supervised host processes.
    pub hosts: usize,
    /// Number of plugin instances declared across those hosts.
    pub loaded: usize,
}

/// Bot instance counters.
#[derive(Debug, Serialize)]
pub struct InstanceSection {
    /// Instances configured on this node.
    pub total: usize,
    /// Instances currently accepting messages.
    pub enabled: usize,
}

/// Session manager counters.
#[derive(Debug, Serialize)]
pub struct SessionSection {
    /// Sessions tracked in total.
    pub total: usize,
    /// Sessions currently in the active state.
    pub active: usize,
}

/// Real-time channel subscriber counters.
#[derive(Debug, Serialize)]
pub struct RealtimeSection {
    /// WebSocket connections currently open.
    pub websocket_connections: u64,
    /// Clients subscribed to the log stream.
    pub log_subscribers: usize,
    /// Clients subscribed to the lifecycle trace stream.
    pub event_subscribers: usize,
}

/// Returns liveness, uptime, memory and resource counts.
///
/// Health always answers `200` while the process is serving: a missing LLM provider is reported
/// through `llm_configured` rather than by failing the probe, so orchestrators do not restart a
/// perfectly healthy core merely because chat debugging is disabled.
async fn health(State(state): State<ApiState>) -> Json<HealthResponse> {
    let hosts = state.supervisor().get_all_hosts().await;
    let loaded = hosts.iter().map(|host| host.metas().len()).sum();

    let instances = state.instances().list().await;
    let instances_enabled = instances.iter().filter(|instance| instance.enabled).count();

    let memory = sample_process_memory();

    Json(HealthResponse {
        status: "ok",
        version: state.version().to_string(),
        uptime_seconds: state.started_at().elapsed().as_secs(),
        llm_configured: state.agent().is_some(),
        memory: MemorySection {
            resident_bytes: memory.map(|m| m.resident_bytes),
            virtual_bytes: memory.map(|m| m.virtual_bytes),
        },
        plugins: PluginSection {
            hosts: hosts.len(),
            loaded,
        },
        instances: InstanceSection {
            total: instances.len(),
            enabled: instances_enabled,
        },
        sessions: SessionSection {
            total: state.sessions().session_count(),
            active: state.sessions().active_session_count(),
        },
        realtime: RealtimeSection {
            websocket_connections: state
                .observability()
                .metrics
                .ws_connections_active
                .load(Ordering::Relaxed),
            log_subscribers: state.observability().logs.subscriber_count(),
            event_subscribers: state.observability().events.subscriber_count(),
        },
    })
}

/// Builds the gauge snapshot shared by the health and metrics endpoints.
///
/// Sampling happens at scrape time so that long-lived gauges (memory, session counts) never
/// drift away from their authoritative owners.
pub async fn sample_gauges(state: &ApiState) -> RuntimeGauges {
    let hosts = state.supervisor().get_all_hosts().await;
    let memory = sample_process_memory();

    RuntimeGauges {
        version: state.version().to_string(),
        uptime_seconds: state.started_at().elapsed().as_secs(),
        resident_memory_bytes: memory.map(|m| m.resident_bytes).unwrap_or_default(),
        virtual_memory_bytes: memory.map(|m| m.virtual_bytes).unwrap_or_default(),
        plugin_hosts: hosts.len(),
        plugins_loaded: hosts.iter().map(|host| host.metas().len()).sum(),
        sessions_total: state.sessions().session_count(),
        sessions_active: state.sessions().active_session_count(),
        ws_connections_active: state
            .observability()
            .metrics
            .ws_connections_active
            .load(Ordering::Relaxed),
    }
}
