//! Integration tests for the Adaptive Circuit Breaker and pipeline fast-skip mechanism.
//!
//! Tests state machine transitions (Closed -> Open -> HalfOpen -> Closed), sliding window
//! RTT latency tripping, PreFilter fast-skip with observation stage emission, and ToolRouter
//! short-circuiting.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use kanon_core::pipeline::{
    PipelineEngine, PipelineObserver, PipelineResult, PipelineStage, PreFilterChain,
    PreFilterOutcome,
};
use kanon_core::supervisor::{
    CircuitBreaker, CircuitBreakerConfig, CircuitState, ManagedHost, Supervisor,
};
use kanon_proto::v1::{PipelineEventRequest, PluginMeta, ToolCallRequest};

/// Recording observer collecting lifecycle stages for test assertions.
#[derive(Default)]
struct TestObserver {
    stages: Mutex<Vec<PipelineStage>>,
}

impl PipelineObserver for TestObserver {
    fn on_stage(&self, stage: &PipelineStage) {
        if let Ok(mut lock) = self.stages.try_lock() {
            lock.push(stage.clone());
        }
    }
}

/// Helper creating a dummy ManagedHost with an unattached channel (sufficient for circuit breaker checks).
fn create_test_host(
    host_id: &str,
    priority: i32,
    config: CircuitBreakerConfig,
) -> Arc<ManagedHost> {
    let dummy_endpoint = tonic::transport::Endpoint::from_static("http://127.0.0.1:50051");
    let channel = dummy_endpoint.connect_lazy();
    let cb = Arc::new(CircuitBreaker::new(config));
    Arc::new(
        ManagedHost::new(
            host_id.to_string(),
            PathBuf::from(format!("/tmp/{}.sock", host_id)),
            channel,
            vec![PluginMeta {
                id: format!("plugin.{}", host_id),
                name: format!("Plugin {}", host_id),
                version: "0.1.0".to_string(),
                author: "Test".to_string(),
                description: "Test".to_string(),
                commands: vec![],
                tools: vec![],
                ..Default::default()
            }],
            priority,
        )
        .with_circuit_breaker(cb),
    )
}

#[tokio::test]
async fn test_circuit_breaker_consecutive_failures_and_cooldown_recovery() {
    let config = CircuitBreakerConfig {
        failure_threshold: 3,
        latency_threshold: Some(Duration::from_millis(50)),
        window_size: 10,
        min_samples_for_latency: 5,
        cooldown_period: Duration::from_millis(50), // fast cooldown for test
        half_open_success_threshold: 2,
        max_half_open_probes: 1,
    };
    let cb = CircuitBreaker::new(config);

    // Initial state: Closed
    assert_eq!(cb.state(), CircuitState::Closed);
    assert!(cb.allow_request());

    // Record 2 failures (< threshold 3): remains Closed
    cb.record_failure("error 1");
    cb.record_failure("error 2");
    assert_eq!(cb.consecutive_failures(), 2);
    assert_eq!(cb.state(), CircuitState::Closed);
    assert!(cb.allow_request());

    // 3rd failure: trips to Open
    cb.record_failure("error 3");
    assert_eq!(cb.consecutive_failures(), 3);
    assert_eq!(cb.state(), CircuitState::Open);
    assert!(!cb.allow_request()); // fast-skip!

    // Wait for cooldown period (50ms) to elapse
    tokio::time::sleep(Duration::from_millis(60)).await;

    // Next request inquiry transitions to HalfOpen
    assert!(cb.allow_request(), "Request should be permitted for probe");
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    // First probe succeeds: remains HalfOpen
    cb.record_success(Duration::from_millis(5));
    assert_eq!(cb.consecutive_successes(), 1);
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    // Second probe succeeds (satisfies half_open_success_threshold = 2): recovers to Closed!
    cb.record_success(Duration::from_millis(5));
    assert_eq!(cb.state(), CircuitState::Closed);
    assert_eq!(cb.consecutive_failures(), 0);
    assert!(cb.allow_request());
}

#[tokio::test]
async fn test_circuit_breaker_half_open_probe_failure_trips_back_to_open() {
    let config = CircuitBreakerConfig {
        failure_threshold: 1,
        latency_threshold: Some(Duration::from_millis(50)),
        window_size: 10,
        min_samples_for_latency: 5,
        cooldown_period: Duration::from_millis(30),
        half_open_success_threshold: 2,
        max_half_open_probes: 1,
    };
    let cb = CircuitBreaker::new(config);

    // Trip immediately with 1 failure
    cb.record_failure("initial timeout");
    assert_eq!(cb.state(), CircuitState::Open);
    assert!(!cb.allow_request());

    // Wait for cooldown
    tokio::time::sleep(Duration::from_millis(40)).await;

    // Transitions to HalfOpen on probe
    assert!(cb.allow_request());
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    // Probe fails: must trip back to Open immediately!
    cb.record_failure("probe failed");
    assert_eq!(cb.state(), CircuitState::Open);
    assert!(!cb.allow_request());
}

#[tokio::test]
async fn test_circuit_breaker_sliding_window_latency_tripping() {
    let config = CircuitBreakerConfig {
        failure_threshold: 10,
        latency_threshold: Some(Duration::from_millis(20)), // 20ms threshold
        window_size: 5,
        min_samples_for_latency: 3,
        cooldown_period: Duration::from_secs(1),
        half_open_success_threshold: 2,
        max_half_open_probes: 1,
    };
    let cb = CircuitBreaker::new(config);

    // Record fast requests (< 20ms)
    cb.record_success(Duration::from_millis(5));
    cb.record_success(Duration::from_millis(10));
    assert_eq!(cb.state(), CircuitState::Closed);

    // Record high latency requests: average exceeds 20ms
    cb.record_success(Duration::from_millis(40));
    // Window samples: [5ms, 10ms, 40ms] -> avg = 18.3ms (< 20ms)
    assert_eq!(cb.state(), CircuitState::Closed);

    cb.record_success(Duration::from_millis(50));
    // Window samples: [5ms, 10ms, 40ms, 50ms] -> avg = 26.25ms (>= 20ms)
    assert_eq!(cb.state(), CircuitState::Open);
    assert!(
        !cb.allow_request(),
        "Breaker must trip on high average latency"
    );
    assert!(cb.last_failure_reason().unwrap().contains("Average RTT"));
}

#[tokio::test]
async fn test_prefilter_fast_skips_open_host_and_emits_observation_event() {
    let config = CircuitBreakerConfig::default();
    let host = create_test_host("stalled_host", 100, config);

    // Manually trip the circuit breaker on this host
    host.circuit_breaker
        .trip("Simulated host network partition");
    assert_eq!(host.circuit_breaker.state(), CircuitState::Open);

    let observer = Arc::new(TestObserver::default());
    let dyn_observer: Arc<dyn PipelineObserver> = observer.clone();

    let event = PipelineEventRequest {
        event_id: "evt-cb-101".to_string(),
        platform: "discord".to_string(),
        channel_id: "chan-1".to_string(),
        sender_id: "user-1".to_string(),
        raw_text: "Hello world".to_string(),
        segments: vec![],
        metadata: None,
    };

    let start = std::time::Instant::now();
    let outcome = PreFilterChain::execute_with_observer(event, &[host], Some(&dyn_observer)).await;
    let elapsed = start.elapsed();

    // Fast-skip must complete in < 5ms without hanging on timeouts
    assert!(
        elapsed < Duration::from_millis(10),
        "Fast-skip took too long: {:?}",
        elapsed
    );

    // Event must pass through unblocked
    match outcome {
        PreFilterOutcome::Passed(evt) => {
            assert_eq!(evt.event_id, "evt-cb-101");
        }
        _ => panic!("Expected PreFilterOutcome::Passed"),
    }

    // Verify observation stage was published
    let stages = observer.stages.lock().await;
    let cb_stage = stages.iter().find(|s| match s {
        PipelineStage::CircuitBreakerTripped { host_id, phase, .. } => {
            host_id == "stalled_host" && phase == "pre_filter"
        }
        _ => false,
    });
    assert!(
        cb_stage.is_some(),
        "Expected PipelineStage::CircuitBreakerTripped to be observed"
    );
}

#[tokio::test]
async fn test_tool_host_call_tool_fast_fails_when_circuit_open() {
    let config = CircuitBreakerConfig::default();
    let host = create_test_host("dead_host", 100, config);
    host.circuit_breaker.trip("Host GC pause timeout");

    let req = ToolCallRequest {
        call_id: "call-1".to_string(),
        tool_name: "calc".to_string(),
        session_id: "session-1".to_string(),
        payload: None,
        ..Default::default()
    };

    let start = std::time::Instant::now();
    let result = host.on_call_tool(req).await;
    let elapsed = start.elapsed();

    // Fast-fail must return immediately (< 5ms)
    assert!(
        elapsed < Duration::from_millis(5),
        "Tool call fast-fail took too long: {:?}",
        elapsed
    );

    match result {
        Err(status) => {
            assert_eq!(status.code(), tonic::Code::Unavailable);
            assert!(status.message().contains("Circuit breaker is OPEN"));
        }
        Ok(_) => panic!("Expected Unavailable status error on open circuit"),
    }
}

struct MockChatProvider;

#[async_trait::async_trait]
impl kanon_llm::gateway::LlmProvider for MockChatProvider {
    async fn chat(
        &self,
        _req: &kanon_llm::gateway::types::ChatRequest,
    ) -> Result<kanon_llm::gateway::types::ChatResponse, kanon_llm::error::GatewayError> {
        Ok(kanon_llm::gateway::types::ChatResponse {
            reasoning_content: None,
            content: Some("Conversational response without tools".to_string()),
            tool_calls: vec![],
            finish_reason: Some("stop".to_string()),
            usage: None,
        })
    }
}

#[tokio::test]
async fn test_pipeline_engine_tool_router_fast_skips_open_host() {
    let supervisor = Arc::new(Supervisor::new(None, None));
    let open_host = create_test_host("open_tool_host", 100, CircuitBreakerConfig::default());
    open_host
        .circuit_breaker
        .trip("Simulated host unresponsive");

    supervisor.register_managed_host(open_host).await;

    let provider = Arc::new(MockChatProvider);
    let memory = Arc::new(kanon_llm::memory::InMemory::new());
    let tool_router = Arc::new(kanon_llm::tool_router::ToolRouter::new(
        provider,
        memory,
        "mock-model",
    ));

    let observer = Arc::new(TestObserver::default());
    let engine = PipelineEngine::new(supervisor)
        .with_tool_router(tool_router)
        .with_observer(observer.clone());

    let event = PipelineEventRequest {
        event_id: "evt-tool-102".to_string(),
        platform: "telegram".to_string(),
        channel_id: "chan-99".to_string(),
        sender_id: "user-99".to_string(),
        raw_text: "What is the weather today?".to_string(),
        segments: vec![],
        metadata: None,
    };

    let result = engine.process_event(event).await;

    // Must still produce conversational reply via LLM
    match result {
        PipelineResult::LlmReplied { content, .. } => {
            assert_eq!(content, "Conversational response without tools");
        }
        other => panic!("Expected LlmReplied outcome, got: {:?}", other),
    }

    // Verify observation stage was recorded for ToolRouter skipping the Open host
    let stages = observer.stages.lock().await;
    let tripped_stage = stages.iter().find(|s| match s {
        PipelineStage::CircuitBreakerTripped { host_id, phase, .. } => {
            host_id == "open_tool_host" && phase == "tool_router"
        }
        _ => false,
    });
    assert!(
        tripped_stage.is_some(),
        "Expected CircuitBreakerTripped stage during tool_router phase"
    );
}

#[tokio::test]
async fn test_platform_circuit_breaker_ignores_high_latency() {
    let cb = CircuitBreaker::for_platform();

    // Record 10 successful deliveries with 120ms WAN RTT each (> 50ms IPC threshold)
    for _ in 0..10 {
        cb.record_success(Duration::from_millis(120));
    }

    // Platform circuit breaker must NOT trip on latency; remains Closed and accepts requests
    assert_eq!(cb.state(), CircuitState::Closed);
    assert!(cb.allow_request());

    // Only trips if failure threshold is reached
    for i in 1..=4 {
        cb.record_failure(&format!("delivery timeout {i}"));
        assert_eq!(cb.state(), CircuitState::Closed);
    }
    cb.record_failure("delivery timeout 5");
    assert_eq!(cb.state(), CircuitState::Open);
    assert!(!cb.allow_request());
}

#[tokio::test]
async fn test_circuit_breaker_half_open_limits_concurrent_probes() {
    let mut config = CircuitBreakerConfig::for_platform();
    config.cooldown_period = Duration::from_millis(30);
    config.max_half_open_probes = 1;

    let cb = CircuitBreaker::new(config);

    // Trip to Open
    for _ in 0..5 {
        cb.record_failure("error");
    }
    assert_eq!(cb.state(), CircuitState::Open);
    assert!(!cb.allow_request());

    // Wait for cooldown
    tokio::time::sleep(Duration::from_millis(40)).await;

    // First caller transitions breaker to HalfOpen and claims the single probe permit
    assert!(cb.allow_request(), "First probe should be granted");
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    // Concurrent callers while probe is in flight must be rejected!
    assert!(
        !cb.allow_request(),
        "Second concurrent probe must be rejected"
    );
    assert!(
        !cb.allow_request(),
        "Third concurrent probe must be rejected"
    );

    // Once in-flight probe finishes successfully, breaker recovers to Closed
    cb.record_success(Duration::from_millis(10));
    assert_eq!(cb.state(), CircuitState::Closed);
    assert!(cb.allow_request(), "Normal traffic admitted after recovery");
}
