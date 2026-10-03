//! PreFilter interception chain scheduler.
//!
//! Executes plugin `OnPreFilter` handlers in ascending priority order with strict
//! total deadline enforcement (30ms) and individual latency monitoring (5ms threshold).

use std::sync::Arc;
use std::time::{Duration, Instant};

use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{MessageSegment, PipelineEventRequest};

use crate::pipeline::observer::{PipelineObserver, PipelineStage};
use crate::supervisor::ManagedHost;

/// Maximum allowable total duration across the entire PreFilter execution chain.
///
/// If cumulative execution time reaches or exceeds this budget, the microkernel
/// short-circuits the remaining filters to guarantee message pipeline throughput.
pub const PREFILTER_TOTAL_DEADLINE: Duration = Duration::from_millis(30);

/// Performance degradation warning threshold for a single plugin's PreFilter execution.
pub const PREFILTER_WARN_THRESHOLD: Duration = Duration::from_millis(5);

/// Outcome produced after evaluating the PreFilter chain across all active plugin hosts.
#[derive(Debug, Clone, PartialEq)]
pub enum PreFilterOutcome {
    /// Inbound event passed all filters (or remaining filters were short-circuited).
    Passed(PipelineEventRequest),
    /// Inbound event was explicitly blocked by a plugin pre-filter.
    Blocked {
        /// Identifier of the host that intercepted and blocked the event.
        host_id: String,
        /// Optional outbound reply messages emitted by the blocking filter.
        reply_messages: Vec<MessageSegment>,
    },
}

/// Scheduler executing the ordered chain of PreFilters across managed plugin hosts.
pub struct PreFilterChain;

impl PreFilterChain {
    /// Executes the pre-filter chain on the provided inbound event without an observer.
    pub async fn execute(
        event: PipelineEventRequest,
        hosts: &[Arc<ManagedHost>],
    ) -> PreFilterOutcome {
        Self::execute_with_observer(event, hosts, None).await
    }

    /// Executes the pre-filter chain on the provided inbound event with optional lifecycle observation.
    ///
    /// # Execution Guarantees
    /// 1. **Priority Ordering**: Filters run in strictly ascending priority order
    ///    (lower numerical value executes earlier). Ties are broken deterministically by `host_id`.
    /// 2. **Adaptive Circuit Breaker Short-Circuit (Fast-Skip)**: If a host's circuit breaker
    ///    is currently `Open`, the filter is immediately skipped without waiting for timeouts,
    ///    and an observation event is emitted to notify control planes.
    /// 3. **Strict 30ms Deadline**: The cumulative elapsed time across all filters is bounded by
    ///    `PREFILTER_TOTAL_DEADLINE`. If the deadline expires, subsequent filters are skipped and
    ///    the event proceeds downstream.
    /// 4. **Latency Warnings & Beacon Recording**: Any single plugin taking longer than
    ///    `PREFILTER_WARN_THRESHOLD` (5ms) triggers a warning log, and measured RTT is recorded
    ///    into the host's circuit breaker sliding window.
    /// 5. **Action Semantics**:
    ///    - `Action::Pass (0)`: Proceeds to the next filter in sequence.
    ///    - `Action::Block (1)`: Immediately halts the chain and returns [`PreFilterOutcome::Blocked`].
    ///    - `Action::Modify (2)`: Updates the event text and propagates modified text downstream.
    pub async fn execute_with_observer(
        event: PipelineEventRequest,
        hosts: &[Arc<ManagedHost>],
        observer: Option<&Arc<dyn PipelineObserver>>,
    ) -> PreFilterOutcome {
        if hosts.is_empty() {
            return PreFilterOutcome::Passed(event);
        }

        // Sort hosts by priority ascending; break ties using host_id.
        let mut sorted_hosts: Vec<Arc<ManagedHost>> = hosts.to_vec();
        sorted_hosts.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.host_id.cmp(&b.host_id))
        });

        let mut current_event = event;
        let chain_start = Instant::now();
        let deadline = chain_start + PREFILTER_TOTAL_DEADLINE;

        for host in sorted_hosts {
            let now = Instant::now();
            if now >= deadline {
                tracing::warn!(
                    event_id = %current_event.event_id,
                    elapsed_ms = chain_start.elapsed().as_millis(),
                    "PreFilter total deadline (30ms) reached; short-circuiting remaining plugins"
                );
                break;
            }

            // Reserve only when there is time to call the host. The permit also releases
            // a half-open recovery slot if the entire filter chain is cancelled.
            // Adaptive Circuit Breaker Check: Fast-skip if breaker is Open
            let Some(permit) = host.circuit_breaker.try_acquire() else {
                tracing::warn!(
                    host_id = %host.host_id,
                    event_id = %current_event.event_id,
                    failures = host.circuit_breaker.consecutive_failures(),
                    state = ?host.circuit_breaker.state(),
                    "Host circuit breaker is OPEN; fast-skipping PreFilter without timeout"
                );
                if let Some(obs) = observer {
                    obs.on_stage(&PipelineStage::CircuitBreakerTripped {
                        event_id: current_event.event_id.clone(),
                        host_id: host.host_id.clone(),
                        phase: "pre_filter".to_string(),
                        reason: format!(
                            "Circuit breaker OPEN (state: {:?}, consecutive failures: {})",
                            host.circuit_breaker.state(),
                            host.circuit_breaker.consecutive_failures()
                        ),
                    });
                }
                continue;
            };

            let remaining_budget = deadline - now;
            let filter_start = Instant::now();

            let filter_result = match tokio::time::timeout(
                remaining_budget,
                host.pre_filter(current_event.clone()),
            )
            .await
            {
                Ok(Ok(result)) => {
                    let filter_elapsed = filter_start.elapsed();
                    permit.success(filter_elapsed);
                    result
                }
                Ok(Err(status)) => {
                    permit.failure(&format!("PreFilter gRPC error: {}", status.code()));
                    tracing::error!(
                        host_id = %host.host_id,
                        event_id = %current_event.event_id,
                        error = %status,
                        "Host PreFilter returned gRPC error; bypassing this filter"
                    );
                    continue;
                }
                Err(_timeout_elapsed) => {
                    permit.failure("PreFilter execution timed out");
                    tracing::warn!(
                        host_id = %host.host_id,
                        event_id = %current_event.event_id,
                        total_elapsed_ms = chain_start.elapsed().as_millis(),
                        "PreFilter call timed out; 30ms total budget expired. Short-circuiting remaining chain"
                    );
                    break;
                }
            };

            let filter_elapsed = filter_start.elapsed();
            if filter_elapsed > PREFILTER_WARN_THRESHOLD {
                tracing::warn!(
                    host_id = %host.host_id,
                    elapsed_ms = filter_elapsed.as_millis(),
                    "PreFilter execution exceeded 5ms latency threshold"
                );
            }

            match filter_result.action {
                // Action::Pass = 0
                0 => {
                    tracing::trace!(
                        host_id = %host.host_id,
                        event_id = %current_event.event_id,
                        "PreFilter passed"
                    );
                }
                // Action::Block = 1
                1 => {
                    tracing::info!(
                        host_id = %host.host_id,
                        event_id = %current_event.event_id,
                        reply_count = filter_result.reply_messages.len(),
                        "PreFilter blocked event"
                    );
                    return PreFilterOutcome::Blocked {
                        host_id: host.host_id.clone(),
                        reply_messages: filter_result.reply_messages,
                    };
                }
                // Action::Modify = 2
                2 => {
                    tracing::debug!(
                        host_id = %host.host_id,
                        event_id = %current_event.event_id,
                        old_text = %current_event.raw_text,
                        new_text = %filter_result.modified_text,
                        "PreFilter modified event text"
                    );
                    current_event.raw_text = filter_result.modified_text.clone();
                    // Also synchronize the modified text into the primary text segment if present.
                    for segment in &mut current_event.segments {
                        if let Some(Segment::Text(ref mut text)) = segment.segment {
                            text.content = filter_result.modified_text.clone();
                            break;
                        }
                    }
                }
                unknown => {
                    tracing::warn!(
                        host_id = %host.host_id,
                        action = unknown,
                        "Unknown PreFilter action enum value received; treating as Pass"
                    );
                }
            }
        }

        PreFilterOutcome::Passed(current_event)
    }
}
