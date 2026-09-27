//! A minimal in-process Milky protocol implementation used by the integration tests.
//!
//! # Why a fake protocol implementation instead of a mock
//! The adapter's contract is entirely wire-level: it must frame SSE and WebSocket correctly, send
//! the `Bearer` header, unwrap the `status`/`retcode` envelope, and translate segments exactly as
//! the specification prescribes. Mocking the client would test the adapter against its own
//! assumptions; speaking real HTTP to a real server tests it against the specification. The fake
//! therefore implements the two endpoints the protocol defines — `POST /api/:endpoint` and
//! `GET /event` — and nothing else.

#![allow(dead_code)]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// Inbound transport the fake implementation serves on `/event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Answer `/event` with a Server-Sent Events stream.
    Sse,
    /// Answer `/event` with a WebSocket upgrade.
    Ws,
}

/// One API call the fake implementation received.
#[derive(Debug, Clone)]
pub struct RecordedCall {
    /// Milky endpoint name taken from the path.
    pub endpoint: String,
    /// Decoded JSON request body.
    pub body: Value,
    /// `Authorization` header value, when the caller sent one.
    pub authorization: Option<String>,
}

/// Deliberate failure the fake implementation should answer with.
#[derive(Debug, Clone)]
enum Failure {
    /// Answer HTTP 200 with a non-zero `retcode` envelope.
    Retcode { retcode: i32, message: String },
    /// Answer with an HTTP status other than 200.
    Status(u16),
    /// Answer success without a `data` field.
    MissingData,
}

/// Shared state of the fake implementation.
struct FakeState {
    /// Every API call received, in arrival order.
    calls: Mutex<Vec<RecordedCall>>,
    /// Senders of currently attached `/event` subscribers.
    subscribers: Mutex<Vec<mpsc::Sender<String>>>,
    /// Monotonic count of `/event` subscriptions ever accepted.
    ///
    /// Monotonic on purpose: a test that lets a stream end and then waits for the reconnect would
    /// race against a decrement, whereas "wait until one more subscription happened" is exact.
    subscriptions: AtomicUsize,
    /// Configured failure, when any.
    failure: Mutex<Option<Failure>>,
}

/// Handle to a running fake Milky implementation.
pub struct FakeMilky {
    /// Base URL the adapter should be pointed at.
    base_url: String,
    /// Shared handler state.
    state: Arc<FakeState>,
    /// Server task, aborted when the handle drops.
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for FakeMilky {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl FakeMilky {
    /// Starts a fake implementation serving the given inbound transport on an ephemeral port.
    pub async fn start(transport: Transport) -> Self {
        let state = Arc::new(FakeState {
            calls: Mutex::new(Vec::new()),
            subscribers: Mutex::new(Vec::new()),
            subscriptions: AtomicUsize::new(0),
            failure: Mutex::new(None),
        });

        let router = match transport {
            Transport::Sse => Router::new().route("/event", get(sse_handler)),
            Transport::Ws => Router::new().route("/event", get(ws_handler)),
        }
        .route("/api/:endpoint", post(api_handler))
        .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake Milky implementation should bind an ephemeral port");
        let address = listener
            .local_addr()
            .expect("bound listener should report its address");

        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Self {
            base_url: format!("http://{}", display_address(address)),
            state,
            handle,
        }
    }

    /// Base URL of the fake implementation.
    pub fn base_url(&self) -> String {
        self.base_url.clone()
    }

    /// Every API call received so far.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.state.calls.lock().expect("calls lock").clone()
    }

    /// Returns the single call recorded for an endpoint, failing the assertion otherwise.
    pub fn call(&self, endpoint: &str) -> RecordedCall {
        let matching: Vec<RecordedCall> = self
            .calls()
            .into_iter()
            .filter(|call| call.endpoint == endpoint)
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "expected exactly one call to '{endpoint}', recorded: {:?}",
            matching
        );
        matching.into_iter().next().expect("one matching call")
    }

    /// Configures the fake implementation to answer API calls with a failure envelope.
    pub fn fail_with_retcode(&self, retcode: i32, message: &str) {
        *self.state.failure.lock().expect("failure lock") = Some(Failure::Retcode {
            retcode,
            message: message.to_string(),
        });
    }

    /// Configures the fake implementation to answer API calls with an HTTP error status.
    pub fn fail_with_status(&self, status: u16) {
        *self.state.failure.lock().expect("failure lock") = Some(Failure::Status(status));
    }

    /// Configures the fake implementation to answer success without a `data` field.
    pub fn fail_with_missing_data(&self) {
        *self.state.failure.lock().expect("failure lock") = Some(Failure::MissingData);
    }

    /// Clears any configured failure.
    pub fn clear_failure(&self) {
        *self.state.failure.lock().expect("failure lock") = None;
    }

    /// Waits until more than `already_seen` subscriptions have been accepted.
    ///
    /// Tests must not push an event before the adapter has subscribed, because a push with no
    /// subscriber is dropped by design (the fake has no replay buffer, exactly like the protocol).
    pub async fn wait_for_subscription(&self, already_seen: usize) {
        for _ in 0..1000 {
            if self.state.subscriptions.load(Ordering::SeqCst) > already_seen {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("no new /event subscription within 10s");
    }

    /// Pushes one raw event JSON payload to every attached subscriber.
    pub fn push_event(&self, event: Value) {
        let payload = serde_json::to_string(&event).expect("event should serialize");
        let mut subscribers = self.state.subscribers.lock().expect("subscribers lock");
        subscribers.retain(|sender| sender.try_send(payload.clone()).is_ok());
    }

    /// Drops every attached subscriber, simulating a stream that ended or a restarted
    /// implementation.
    pub fn drop_subscribers(&self) {
        self.state
            .subscribers
            .lock()
            .expect("subscribers lock")
            .clear();
    }
}

/// Renders a socket address the way a URL expects it.
///
/// `SocketAddr`'s own display already brackets IPv6 addresses and appends the port, so it is used
/// verbatim rather than re-implemented.
fn display_address(address: SocketAddr) -> String {
    address.to_string()
}

/// Answers one Milky API call with the configured response.
async fn api_handler(
    State(state): State<Arc<FakeState>>,
    Path(endpoint): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let parsed = if body.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&body).unwrap_or(Value::Null)
    };

    state.calls.lock().expect("calls lock").push(RecordedCall {
        endpoint: endpoint.clone(),
        body: parsed,
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
    });

    let failure = state.failure.lock().expect("failure lock").clone();
    match failure {
        Some(Failure::Retcode { retcode, message }) => Json(json!({
            "status": "failed",
            "retcode": retcode,
            "message": message,
        }))
        .into_response(),
        Some(Failure::Status(status)) => Response::builder()
            .status(StatusCode::from_u16(status).expect("valid status code"))
            .body(Body::from("failure"))
            .expect("response should build"),
        Some(Failure::MissingData) => Json(json!({
            "status": "ok",
            "retcode": 0,
        }))
        .into_response(),
        None => Json(json!({
            "status": "ok",
            "retcode": 0,
            "data": success_payload(&endpoint),
        }))
        .into_response(),
    }
}

/// Default success payload of each endpoint the tests exercise.
fn success_payload(endpoint: &str) -> Value {
    match endpoint {
        "get_login_info" => json!({ "uin": 10001, "nickname": "Kanon Test" }),
        "get_impl_info" => json!({
            "impl_name": "FakeMilky",
            "impl_version": "1.0.0",
            "qq_protocol_version": "9.0.0",
            "qq_protocol_type": "linux",
            "milky_version": "1.3",
        }),
        "send_group_message" | "send_private_message" => {
            json!({ "message_seq": 4242, "time": 1_700_000_000 })
        }
        // An empty page is a valid answer, and it lets the optional-parameter test exercise a
        // request whose optional field is sometimes omitted and sometimes present.
        "get_history_messages" => json!({ "messages": [] }),
        _ => json!({}),
    }
}

/// Registers a subscriber and returns its receiver.
fn subscribe(state: &Arc<FakeState>) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel(64);
    state
        .subscribers
        .lock()
        .expect("subscribers lock")
        .push(sender);
    state.subscriptions.fetch_add(1, Ordering::SeqCst);
    receiver
}

/// Serves `/event` as a Server-Sent Events stream.
async fn sse_handler(State(state): State<Arc<FakeState>>) -> Response {
    let receiver = subscribe(&state);

    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|payload| {
            (
                Ok::<_, Infallible>(SseEvent::default().event("milky_event").data(payload)),
                receiver,
            )
        })
    });

    Sse::new(stream).into_response()
}

/// Serves `/event` as a WebSocket connection.
async fn ws_handler(
    State(state): State<Arc<FakeState>>,
    upgrade: axum::extract::WebSocketUpgrade,
) -> Response {
    upgrade.on_upgrade(move |mut socket| async move {
        let mut receiver = subscribe(&state);
        while let Some(payload) = receiver.recv().await {
            if socket
                .send(axum::extract::ws::Message::Text(payload))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

/// Builds a realistic `message_receive` event for a friend conversation.
pub fn friend_message_event(message_seq: i64, sender_id: i64, text: &str) -> Value {
    json!({
        "time": 1_700_000_000_i64,
        "self_id": 10001,
        "event_type": "message_receive",
        "data": {
            "message_scene": "friend",
            "peer_id": sender_id,
            "message_seq": message_seq,
            "sender_id": sender_id,
            "time": 1_700_000_000_i64,
            "segments": [ { "type": "text", "data": { "text": text } } ],
            "friend": {
                "user_id": sender_id,
                "nickname": "Tester",
                "sex": "unknown",
                "qid": "",
                "remark": "",
                "category": { "category_id": 0, "category_name": "" },
            },
        },
    })
}

/// Builds a realistic `message_receive` event for a group conversation.
pub fn group_message_event(message_seq: i64, group_id: i64, sender_id: i64, text: &str) -> Value {
    json!({
        "time": 1_700_000_000_i64,
        "self_id": 10001,
        "event_type": "message_receive",
        "data": {
            "message_scene": "group",
            "peer_id": group_id,
            "message_seq": message_seq,
            "sender_id": sender_id,
            "time": 1_700_000_000_i64,
            "segments": [ { "type": "text", "data": { "text": text } } ],
            "group": {
                "group_id": group_id,
                "group_name": "Test Group",
                "member_count": 3,
                "max_member_count": 200,
                "remark": "",
                "created_time": 1_600_000_000_i64,
                "description": "",
                "question": "",
                "announcement": "",
            },
            "group_member": {
                "user_id": sender_id,
                "nickname": "Tester",
                "sex": "unknown",
                "group_id": group_id,
                "card": "Tester Card",
                "title": "",
                "level": 1,
                "role": "member",
                "join_time": 1_600_000_000_i64,
                "last_sent_time": 1_700_000_000_i64,
            },
        },
    })
}
