//! Adaptive Circuit Breaker for plugin hosts.
//!
//! Provides cascading failure protection against plugin GC pauses, network stalls,
//! and unhandled sub-process exceptions. Tracks round-trip time (RTT) across a sliding
//! sample window and counts consecutive failures (RPC errors and timeouts).
//!
//! # State Machine Architecture
//!
//! ```text
//!          [ Failure threshold exceeded OR ]
//!          [ Sliding window average RTT > threshold ]
//!      +------------------------------------------------+
//!      |                                                |
//!      v                                                |
//!   +------+       Cooldown elapsed       +----------+  |
//!   | Open | ---------------------------> | HalfOpen | -+ (Probe failed)
//!   +------+                              +----------+
//!      ^                                        |
//!      | (Tripped)                              | (Probe successes >= threshold)
//!      |                                        v
//!   +--------+ <--------------------------------+
//!   | Closed |
//!   +--------+
//! ```
//!
//! - **Closed**: Normal operational state. Inbound calls are dispatched to the host.
//!   Every invocation records its execution RTT or failure. If consecutive failures
//!   exceed `failure_threshold`, or if the sliding window average RTT exceeds
//!   `latency_threshold`, the breaker transitions to `Open`.
//! - **Open**: Circuit is tripped. Calls are fast-skipped immediately without waiting
//!   for gRPC I/O or multi-second timeouts, protecting the main event loop throughput.
//!   Once `cooldown_period` elapses, the next inquiry transitions the breaker to `HalfOpen`.
//! - **HalfOpen**: Trial probe state. Allows a limited number of requests to test if
//!   the host has recovered. If `half_open_success_threshold` consecutive probes succeed,
//!   the circuit recovers to `Closed`. If any probe fails, the circuit trips back to `Open`.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Lifecycle states of the circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    /// Normal operation: requests pass through, latency and errors are tracked.
    Closed,
    /// Circuit tripped: requests are fast-skipped without invoking the host.
    Open,
    /// Cooldown expired: trial requests are permitted to probe host recovery.
    HalfOpen,
}

/// Operational parameters configuring circuit breaker thresholds.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive errors or timeouts required to trip the breaker.
    pub failure_threshold: usize,
    /// Maximum allowable sliding window average RTT before tripping the breaker.
    /// If `None`, RTT-based latency tripping is disabled (e.g. for external chat platforms).
    pub latency_threshold: Option<Duration>,
    /// Capacity of the sliding sample window for RTT calculations.
    pub window_size: usize,
    /// Minimum samples required in the sliding window before RTT can trip the circuit.
    ///
    /// Prevents a single early slow request from prematurely tripping an otherwise healthy host.
    pub min_samples_for_latency: usize,
    /// Duration the circuit remains `Open` before transitioning to `HalfOpen` for probing.
    pub cooldown_period: Duration,
    /// Number of consecutive successful probes in `HalfOpen` required to recover to `Closed`.
    pub half_open_success_threshold: usize,
    /// Maximum number of concurrent in-flight trial probes permitted in `HalfOpen` state.
    /// Defaults to 1 to prevent storming a recovering endpoint.
    pub max_half_open_probes: usize,
}

impl CircuitBreakerConfig {
    /// Configuration baseline for plugin business calls, tripping on failures and timeouts.
    /// Successful calls may perform external I/O, so their duration is not an IPC health signal.
    pub fn for_host() -> Self {
        Self {
            failure_threshold: 5,
            latency_threshold: None,
            window_size: 20,
            min_samples_for_latency: 5,
            cooldown_period: Duration::from_secs(5),
            half_open_success_threshold: 2,
            max_half_open_probes: 1,
        }
    }

    /// Configuration baseline tailored for external chat platform delivery (Webhooks, HTTP REST).
    /// Latency-based tripping is disabled because WAN/HTTP RTT naturally exceeds IPC thresholds.
    /// Trips exclusively on delivery failures and timeouts, with single-probe recovery.
    pub fn for_platform() -> Self {
        Self {
            failure_threshold: 5,
            latency_threshold: None,
            window_size: 20,
            min_samples_for_latency: 5,
            cooldown_period: Duration::from_secs(30),
            half_open_success_threshold: 1,
            max_half_open_probes: 1,
        }
    }
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self::for_host()
    }
}

/// Internal state variables guarded by a fast in-memory mutex.
struct CircuitBreakerInner {
    state: CircuitState,
    consecutive_failures: usize,
    consecutive_successes: usize,
    half_open_in_flight: usize,
    rtt_window: VecDeque<Duration>,
    last_state_change: Instant,
    tripped_at: Option<Instant>,
    last_failure_reason: Option<String>,
}

/// Thread-safe adaptive circuit breaker for a managed plugin host or outbound delivery platform.
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    inner: Mutex<CircuitBreakerInner>,
}

impl CircuitBreaker {
    /// Creates a new circuit breaker with the specified configuration.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        let inner = CircuitBreakerInner {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            consecutive_successes: 0,
            half_open_in_flight: 0,
            rtt_window: VecDeque::with_capacity(config.window_size),
            last_state_change: Instant::now(),
            tripped_at: None,
            last_failure_reason: None,
        };
        Self {
            config,
            inner: Mutex::new(inner),
        }
    }

    /// Creates a new circuit breaker with standard host IPC defaults.
    pub fn with_defaults() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }

    /// Creates a new circuit breaker configured specifically for plugin host IPC.
    pub fn for_host() -> Self {
        Self::new(CircuitBreakerConfig::for_host())
    }

    /// Creates a new circuit breaker configured specifically for platform outbound delivery.
    pub fn for_platform() -> Self {
        Self::new(CircuitBreakerConfig::for_platform())
    }

    /// Reports eligibility without reserving a recovery probe (safe for tool discovery).
    pub fn is_available(&self) -> bool {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.state {
            CircuitState::Closed => true,
            CircuitState::Open => guard.last_state_change.elapsed() >= self.config.cooldown_period,
            CircuitState::HalfOpen => guard.half_open_in_flight < self.config.max_half_open_probes,
        }
    }

    /// Reserves a request whose cancellation is recorded if the RPC future is dropped.
    pub fn try_acquire(&self) -> Option<RequestPermit<'_>> {
        self.allow_request().then(|| RequestPermit {
            breaker: self,
            completed: false,
        })
    }

    /// Evaluates whether an outbound request to this target should be permitted.
    ///
    /// - If `Closed`: returns `true`.
    /// - If `Open`: evaluates whether the cooldown window has elapsed. If so, transitions
    ///   the state to `HalfOpen`, grants a single probe permit, and returns `true`. Otherwise returns `false`
    ///   to immediately trigger fast-skip.
    /// - If `HalfOpen`: checks whether current in-flight probes are within `max_half_open_probes`.
    ///   If permitted, increments in-flight count and returns `true`; otherwise returns `false`
    ///   to prevent concurrent probe storms.
    pub fn allow_request(&self) -> bool {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                let now = Instant::now();
                if now.duration_since(guard.last_state_change) >= self.config.cooldown_period {
                    tracing::info!(
                        cooldown_secs = self.config.cooldown_period.as_secs_f64(),
                        "Circuit breaker cooldown expired; transitioning to HalfOpen for probe"
                    );
                    guard.state = CircuitState::HalfOpen;
                    guard.last_state_change = now;
                    guard.consecutive_successes = 0;
                    guard.half_open_in_flight = 1;
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                if guard.half_open_in_flight < self.config.max_half_open_probes {
                    guard.half_open_in_flight += 1;
                    true
                } else {
                    // Maximum in-flight probes already active; reject concurrent requests
                    false
                }
            }
        }
    }

    /// Records a successfully completed RPC operation and its measured round-trip time.
    ///
    /// # Behavior by State
    /// - **Closed**: Resets `consecutive_failures` to 0 and appends `rtt` to the sliding window.
    ///   If `latency_threshold` is configured, checks whether window average RTT exceeds
    ///   the threshold, and if so, trips the circuit to `Open`.
    /// - **HalfOpen**: Decrements `half_open_in_flight` and increments `consecutive_successes`.
    ///   When `half_open_success_threshold` is reached, recovers the circuit back to `Closed`.
    /// - **Open**: Leaves state as Open, clearing in-flight counter.
    pub fn record_success(&self, rtt: Duration) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();

        // Maintain sliding window bounds
        if guard.rtt_window.len() >= self.config.window_size {
            guard.rtt_window.pop_front();
        }
        guard.rtt_window.push_back(rtt);

        match guard.state {
            CircuitState::Closed => {
                guard.consecutive_failures = 0;

                // Inspect sliding window average RTT if configured
                if let Some(threshold) = self.config.latency_threshold
                    && guard.rtt_window.len() >= self.config.min_samples_for_latency
                {
                    let total_micros: u128 = guard.rtt_window.iter().map(|d| d.as_micros()).sum();
                    let avg_micros = total_micros / (guard.rtt_window.len() as u128);
                    let avg_rtt = Duration::from_micros(avg_micros as u64);

                    if avg_rtt >= threshold {
                        tracing::warn!(
                            avg_rtt_ms = avg_rtt.as_millis(),
                            threshold_ms = threshold.as_millis(),
                            window_samples = guard.rtt_window.len(),
                            "Sliding window average RTT exceeded threshold; tripping circuit to Open"
                        );
                        guard.state = CircuitState::Open;
                        guard.last_state_change = now;
                        guard.tripped_at = Some(now);
                        guard.half_open_in_flight = 0;
                        guard.last_failure_reason = Some(format!(
                            "Average RTT ({:.2}ms) exceeded threshold ({:.2}ms)",
                            avg_rtt.as_secs_f64() * 1000.0,
                            threshold.as_secs_f64() * 1000.0
                        ));
                    }
                }
            }
            CircuitState::HalfOpen => {
                guard.half_open_in_flight = guard.half_open_in_flight.saturating_sub(1);
                guard.consecutive_successes += 1;
                tracing::debug!(
                    success_count = guard.consecutive_successes,
                    required = self.config.half_open_success_threshold,
                    "HalfOpen probe succeeded"
                );

                if guard.consecutive_successes >= self.config.half_open_success_threshold {
                    tracing::info!(
                        success_threshold = self.config.half_open_success_threshold,
                        "HalfOpen probe threshold satisfied; recovering circuit to Closed"
                    );
                    guard.state = CircuitState::Closed;
                    guard.last_state_change = now;
                    guard.consecutive_failures = 0;
                    guard.consecutive_successes = 0;
                    guard.half_open_in_flight = 0;
                    guard.last_failure_reason = None;
                }
            }
            CircuitState::Open => {
                guard.half_open_in_flight = 0;
            }
        }
    }

    /// Records a failed RPC invocation (timeout or gRPC status error).
    ///
    /// # Behavior by State
    /// - **Closed**: Increments `consecutive_failures`. If `failure_threshold` is reached,
    ///   trips the breaker to `Open`.
    /// - **HalfOpen**: A single probe failure immediately aborts recovery and trips
    ///   the breaker back to `Open` for another cooldown period.
    /// - **Open**: Increments failure count and refreshes `last_state_change`.
    pub fn record_failure(&self, reason: &str) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        guard.consecutive_failures += 1;
        guard.last_failure_reason = Some(reason.to_string());

        match guard.state {
            CircuitState::Closed => {
                if guard.consecutive_failures >= self.config.failure_threshold {
                    tracing::warn!(
                        failures = guard.consecutive_failures,
                        threshold = self.config.failure_threshold,
                        reason = %reason,
                        "Consecutive failure threshold exceeded; tripping circuit to Open"
                    );
                    guard.state = CircuitState::Open;
                    guard.last_state_change = now;
                    guard.tripped_at = Some(now);
                    guard.half_open_in_flight = 0;
                }
            }
            CircuitState::HalfOpen => {
                tracing::warn!(
                    reason = %reason,
                    "HalfOpen trial probe failed; tripping circuit back to Open"
                );
                guard.state = CircuitState::Open;
                guard.last_state_change = now;
                guard.tripped_at = Some(now);
                guard.consecutive_successes = 0;
                guard.half_open_in_flight = 0;
            }
            CircuitState::Open => {
                guard.last_state_change = now;
                guard.half_open_in_flight = 0;
            }
        }
    }

    /// Returns the current lifecycle state of the circuit breaker.
    ///
    /// Lazily evaluates cooldown expiration: if currently `Open` and `cooldown_period`
    /// has passed, returns `HalfOpen`.
    pub fn state(&self) -> CircuitState {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if guard.state == CircuitState::Open {
            let now = Instant::now();
            if now.duration_since(guard.last_state_change) >= self.config.cooldown_period {
                guard.state = CircuitState::HalfOpen;
                guard.last_state_change = now;
                guard.consecutive_successes = 0;
            }
        }
        guard.state
    }

    /// Returns the number of consecutive recorded failures.
    pub fn consecutive_failures(&self) -> usize {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.consecutive_failures
    }

    /// Returns the number of consecutive successful trial probes in `HalfOpen`.
    pub fn consecutive_successes(&self) -> usize {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.consecutive_successes
    }

    /// Computes the arithmetic mean round-trip latency across the sliding window.
    ///
    /// Returns `None` if the sliding sample window is currently empty.
    pub fn average_rtt(&self) -> Option<Duration> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if guard.rtt_window.is_empty() {
            return None;
        }
        let total_micros: u128 = guard.rtt_window.iter().map(|d| d.as_micros()).sum();
        let avg_micros = total_micros / (guard.rtt_window.len() as u128);
        Some(Duration::from_micros(avg_micros as u64))
    }

    /// Returns the most recent failure explanation, if recorded.
    pub fn last_failure_reason(&self) -> Option<String> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.last_failure_reason.clone()
    }

    /// Manually trips the circuit to `Open` with an explicit reason.
    ///
    /// Primarily used in testing and administrative control operations.
    pub fn trip(&self, reason: &str) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        guard.state = CircuitState::Open;
        guard.last_state_change = now;
        guard.tripped_at = Some(now);
        guard.last_failure_reason = Some(reason.to_string());
    }

    /// Manually forces the circuit to recover to `Closed` and resets all counters.
    pub fn reset(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.state = CircuitState::Closed;
        guard.consecutive_failures = 0;
        guard.consecutive_successes = 0;
        guard.half_open_in_flight = 0;
        guard.rtt_window.clear();
        guard.last_state_change = Instant::now();
        guard.tripped_at = None;
        guard.last_failure_reason = None;
    }

    /// Returns a snapshot reference to the active configuration parameters.
    pub fn config(&self) -> &CircuitBreakerConfig {
        &self.config
    }
}

/// An in-flight RPC reservation; dropping a cancelled future releases its recovery slot.
pub struct RequestPermit<'a> {
    breaker: &'a CircuitBreaker,
    completed: bool,
}

impl RequestPermit<'_> {
    /// Completes this reservation with a successful response.
    pub fn success(mut self, rtt: Duration) {
        self.completed = true;
        self.breaker.record_success(rtt);
    }

    /// Completes this reservation with an explicit RPC failure.
    pub fn failure(mut self, reason: &str) {
        self.completed = true;
        self.breaker.record_failure(reason);
    }
}

impl Drop for RequestPermit<'_> {
    fn drop(&mut self) {
        if !self.completed {
            // A caller timeout drops the RPC before a status arrives. Treat that as failure,
            // otherwise the sole half-open slot would remain reserved forever.
            self.breaker
                .record_failure("RPC cancelled before completion");
        }
    }
}
