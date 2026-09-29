//! Lock-free runtime metrics and Prometheus text exposition.
//!
//! Counters are plain atomics updated on the hot path with `Relaxed` ordering: the
//! management console requires monotonic, eventually consistent numbers, never a globally
//! synchronized snapshot, so paying for cross-thread fences on every message would be
//! wasted work. Gauges that describe live infrastructure (uptime, memory, session counts)
//! are sampled at scrape time from their authoritative owners instead of being mirrored here.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic counters describing pipeline and gateway activity.
#[derive(Debug, Default)]
pub struct MetricsRegistry {
    /// Inbound events that entered the pipeline worker.
    pub events_ingested: AtomicU64,
    /// Events intercepted and blocked by a PreFilter plugin.
    pub events_blocked: AtomicU64,
    /// Events that passed the PreFilter chain.
    pub events_passed: AtomicU64,
    /// Slash commands matched and dispatched to a plugin host.
    pub commands_matched: AtomicU64,
    /// Slash commands parsed but unmatched by any plugin.
    pub commands_not_found: AtomicU64,
    /// LLM conversational replies produced by the pipeline.
    pub llm_replies: AtomicU64,
    /// Outbound messages enqueued towards platform adapters.
    pub outbound_messages: AtomicU64,
    /// Outbound messages accepted by the platform adapter responsible for them.
    pub outbound_delivered: AtomicU64,
    /// Outbound messages that no adapter could deliver.
    pub outbound_failed: AtomicU64,
    /// LLM requests issued by the agent runtime (including tool-calling rounds).
    pub llm_requests: AtomicU64,
    /// Prompt tokens the providers reported across all LLM requests.
    pub llm_prompt_tokens: AtomicU64,
    /// Part of those prompt tokens served from the providers' prompt caches.
    ///
    /// `cached / prompt` is the cache hit rate the prompt layout is built to raise.
    pub llm_cached_tokens: AtomicU64,
    /// Tool calls dispatched by the agent runtime.
    pub tool_calls: AtomicU64,
    /// Failed tool calls reported by native or plugin tools.
    pub tool_call_failures: AtomicU64,
    /// Sandbox chat completions served by the management gateway.
    pub chat_completions: AtomicU64,
    /// Structured log records captured and broadcast to WebSocket subscribers.
    pub log_records: AtomicU64,
    /// Log records dropped because a subscriber lagged behind the broadcast buffer.
    pub log_records_dropped: AtomicU64,
    /// Trace events published to the lifecycle bus.
    pub trace_events: AtomicU64,
    /// Total WebSocket connections accepted.
    pub ws_connections_total: AtomicU64,
    /// Currently open WebSocket connections (gauge).
    pub ws_connections_active: AtomicU64,
}

impl MetricsRegistry {
    /// Creates an empty registry with all counters at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Increments a counter by one.
    pub fn incr(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Adds an arbitrary delta to a counter.
    pub fn add(counter: &AtomicU64, delta: u64) {
        counter.fetch_add(delta, Ordering::Relaxed);
    }

    /// Reads a counter value.
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    /// Records a newly accepted WebSocket connection.
    pub fn ws_opened(&self) {
        Self::incr(&self.ws_connections_total);
        Self::incr(&self.ws_connections_active);
    }

    /// Records a WebSocket connection that has been closed.
    ///
    /// Uses a saturating compare-and-swap loop so a double-close can never wrap the gauge to a
    /// nonsensical `u64::MAX`.
    pub fn ws_closed(&self) {
        let _ = self.ws_connections_active.try_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |current| Some(current.saturating_sub(1)),
        );
    }

    /// Renders the complete registry in Prometheus text exposition format (`0.0.4`).
    ///
    /// `gauges` carries values sampled from external owners at scrape time.
    pub fn render_prometheus(&self, gauges: &RuntimeGauges) -> String {
        let mut out = String::with_capacity(2048);

        metric(
            &mut out,
            "kanon_build_info",
            "Build and version information of the running core.",
            "gauge",
            &format!("kanon_build_info{{version=\"{}\"}} 1", gauges.version),
        );
        metric(
            &mut out,
            "kanon_uptime_seconds",
            "Seconds elapsed since the core process started.",
            "gauge",
            &format!("kanon_uptime_seconds {}", gauges.uptime_seconds),
        );
        metric(
            &mut out,
            "kanon_process_resident_memory_bytes",
            "Resident set size of the core process in bytes.",
            "gauge",
            &format!(
                "kanon_process_resident_memory_bytes {}",
                gauges.resident_memory_bytes
            ),
        );
        metric(
            &mut out,
            "kanon_process_virtual_memory_bytes",
            "Virtual memory size of the core process in bytes.",
            "gauge",
            &format!(
                "kanon_process_virtual_memory_bytes {}",
                gauges.virtual_memory_bytes
            ),
        );
        metric(
            &mut out,
            "kanon_plugin_hosts",
            "Number of plugin host processes currently supervised.",
            "gauge",
            &format!("kanon_plugin_hosts {}", gauges.plugin_hosts),
        );
        metric(
            &mut out,
            "kanon_plugins_loaded",
            "Number of plugin instances currently loaded across all hosts.",
            "gauge",
            &format!("kanon_plugins_loaded {}", gauges.plugins_loaded),
        );
        metric(
            &mut out,
            "kanon_sessions_total",
            "Conversation sessions tracked by the session manager.",
            "gauge",
            &format!("kanon_sessions_total {}", gauges.sessions_total),
        );
        metric(
            &mut out,
            "kanon_sessions_active",
            "Conversation sessions currently in the active state.",
            "gauge",
            &format!("kanon_sessions_active {}", gauges.sessions_active),
        );
        metric(
            &mut out,
            "kanon_websocket_connections",
            "Currently open management WebSocket connections.",
            "gauge",
            &format!(
                "kanon_websocket_connections {}",
                gauges.ws_connections_active
            ),
        );

        counter_metric(
            &mut out,
            "kanon_events_ingested_total",
            "Inbound events that entered the message pipeline.",
            Self::get(&self.events_ingested),
        );
        counter_metric(
            &mut out,
            "kanon_events_blocked_total",
            "Events intercepted and blocked by a PreFilter plugin.",
            Self::get(&self.events_blocked),
        );
        counter_metric(
            &mut out,
            "kanon_events_passed_total",
            "Events that passed the PreFilter chain.",
            Self::get(&self.events_passed),
        );
        counter_metric(
            &mut out,
            "kanon_commands_matched_total",
            "Slash commands matched and dispatched to a plugin host.",
            Self::get(&self.commands_matched),
        );
        counter_metric(
            &mut out,
            "kanon_commands_not_found_total",
            "Slash commands parsed but unmatched by any plugin.",
            Self::get(&self.commands_not_found),
        );
        counter_metric(
            &mut out,
            "kanon_llm_replies_total",
            "Conversational LLM replies produced by the pipeline.",
            Self::get(&self.llm_replies),
        );
        counter_metric(
            &mut out,
            "kanon_llm_requests_total",
            "LLM completion requests issued by the agent runtime.",
            Self::get(&self.llm_requests),
        );
        counter_metric(
            &mut out,
            "kanon_llm_prompt_tokens_total",
            "Prompt tokens reported by model providers.",
            Self::get(&self.llm_prompt_tokens),
        );
        counter_metric(
            &mut out,
            "kanon_llm_cached_prompt_tokens_total",
            "Prompt tokens model providers served from their prompt cache.",
            Self::get(&self.llm_cached_tokens),
        );
        counter_metric(
            &mut out,
            "kanon_tool_calls_total",
            "Tool calls dispatched by the agent runtime.",
            Self::get(&self.tool_calls),
        );
        counter_metric(
            &mut out,
            "kanon_tool_call_failures_total",
            "Tool calls that completed with a failure result.",
            Self::get(&self.tool_call_failures),
        );
        counter_metric(
            &mut out,
            "kanon_outbound_messages_total",
            "Outbound messages enqueued towards platform adapters.",
            Self::get(&self.outbound_messages),
        );
        counter_metric(
            &mut out,
            "kanon_outbound_delivered_total",
            "Outbound messages accepted by a platform adapter.",
            Self::get(&self.outbound_delivered),
        );
        counter_metric(
            &mut out,
            "kanon_outbound_failed_total",
            "Outbound messages that could not be delivered by any adapter.",
            Self::get(&self.outbound_failed),
        );
        counter_metric(
            &mut out,
            "kanon_chat_completions_total",
            "Sandbox chat completions served by the management gateway.",
            Self::get(&self.chat_completions),
        );
        counter_metric(
            &mut out,
            "kanon_log_records_total",
            "Structured log records captured and broadcast.",
            Self::get(&self.log_records),
        );
        counter_metric(
            &mut out,
            "kanon_log_records_dropped_total",
            "Log records dropped due to slow WebSocket subscribers.",
            Self::get(&self.log_records_dropped),
        );
        counter_metric(
            &mut out,
            "kanon_trace_events_total",
            "Lifecycle trace events published to the event bus.",
            Self::get(&self.trace_events),
        );
        counter_metric(
            &mut out,
            "kanon_websocket_connections_total",
            "Total management WebSocket connections accepted.",
            Self::get(&self.ws_connections_total),
        );

        out
    }
}

/// Gauge values sampled at scrape time from their authoritative owners.
#[derive(Debug, Clone, Default)]
pub struct RuntimeGauges {
    /// Core build version.
    pub version: String,
    /// Process uptime in whole seconds.
    pub uptime_seconds: u64,
    /// Resident set size in bytes.
    pub resident_memory_bytes: u64,
    /// Virtual memory size in bytes.
    pub virtual_memory_bytes: u64,
    /// Number of supervised plugin host processes.
    pub plugin_hosts: usize,
    /// Number of plugins loaded across all hosts.
    pub plugins_loaded: usize,
    /// Number of tracked conversation sessions.
    pub sessions_total: usize,
    /// Number of active conversation sessions.
    pub sessions_active: usize,
    /// Currently open management WebSocket connections.
    pub ws_connections_active: u64,
}

/// Appends one counter family (`# HELP` / `# TYPE` / sample line).
fn counter_metric(out: &mut String, name: &str, help: &str, value: u64) {
    metric(out, name, help, "counter", &format!("{name} {value}"));
}

/// Appends one metric family header followed by a pre-rendered sample line.
fn metric(out: &mut String, name: &str, help: &str, kind: &str, sample: &str) {
    // Writing into a String is infallible; a failure here would require an allocation error.
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
    let _ = writeln!(out, "{sample}");
}
