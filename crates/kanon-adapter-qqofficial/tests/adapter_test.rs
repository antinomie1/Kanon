//! The adapter against a mock QQ Open Platform: token, gateway WebSocket and REST API.
//!
//! This is the whole round trip a real bot makes — identify, READY, a quoted group message into
//! the core, a passive reply with an image back out, a quote of that reply, and a resumed session
//! after the socket drops — so a protocol regression shows up here rather than in production.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use kanon_adapter_qqofficial::{ConnectionState, Endpoints, QqOfficialAdapter, QqOfficialConfig};
use kanon_core::{AdapterError, EventIngress, PlatformAdapter};
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    DeliverMessageRequest, ImageSegment, IngestEventRequest, MessageSegment, PipelineEventRequest,
    ReplySegment, TextSegment, image_segment,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(5);

/// One accepted gateway connection, driven by the test.
struct Socket {
    to_client: mpsc::UnboundedSender<Message>,
    from_client: mpsc::UnboundedReceiver<Value>,
}

impl Socket {
    fn send(&self, payload: Value) {
        self.to_client
            .send(Message::Text(payload.to_string()))
            .expect("socket open");
    }

    /// Next non-heartbeat payload from the adapter.
    async fn recv(&mut self) -> Value {
        loop {
            let payload = timeout(WAIT, self.from_client.recv())
                .await
                .expect("adapter should send a gateway payload")
                .expect("socket open");
            if payload["op"] != 1 {
                return payload;
            }
        }
    }
}

#[derive(Clone)]
struct Mock {
    addr: SocketAddr,
    /// REST calls as (path, body).
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    sockets: mpsc::UnboundedSender<Socket>,
}

async fn start_mock() -> (Mock, mpsc::UnboundedReceiver<Socket>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (sockets, accepted) = mpsc::unbounded_channel();
    let mock = Mock {
        addr: listener.local_addr().unwrap(),
        calls: Arc::default(),
        sockets,
    };
    let app = Router::new()
        .route("/token", post(token))
        .route("/gateway", get(gateway))
        .route("/ws", get(ws))
        .route("/v2/:scope/:target/:kind", post(v2))
        .with_state(mock.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (mock, accepted)
}

async fn token(Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body, json!({"appId": "APP", "clientSecret": "SECRET"}));
    Json(json!({"access_token": "TOKEN", "expires_in": "7200"}))
}

async fn gateway(State(mock): State<Mock>, headers: HeaderMap) -> Json<Value> {
    assert_eq!(headers["authorization"], "QQBot TOKEN");
    Json(json!({"url": format!("ws://{}/ws", mock.addr)}))
}

async fn ws(State(mock): State<Mock>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| pump(socket, mock))
}

/// Relays frames between the socket and the test's [`Socket`] handle.
async fn pump(mut socket: WebSocket, mock: Mock) {
    let (to_client, mut outbound) = mpsc::unbounded_channel();
    let (inbound, from_client) = mpsc::unbounded_channel();
    let _ = mock.sockets.send(Socket {
        to_client,
        from_client,
    });
    loop {
        tokio::select! {
            message = outbound.recv() => match message {
                Some(message) => {
                    let closing = matches!(message, Message::Close(_));
                    if socket.send(message).await.is_err() || closing {
                        return;
                    }
                }
                None => return,
            },
            frame = socket.recv() => match frame {
                Some(Ok(Message::Text(text))) => {
                    let _ = inbound.send(serde_json::from_str(&text).unwrap());
                }
                Some(Ok(_)) => {}
                _ => return,
            },
        }
    }
}

async fn v2(
    State(mock): State<Mock>,
    Path((scope, target, kind)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(headers["authorization"], "QQBot TOKEN");
    let mut calls = mock.calls.lock().unwrap();
    calls.push((format!("/v2/{scope}/{target}/{kind}"), body));
    let n = calls.len();
    Json(match kind.as_str() {
        "files" => json!({"file_uuid": "u", "file_info": "FILE_INFO", "ttl": 60}),
        _ => json!({"id": format!("sent-{n}"), "ext_info": {"ref_idx": format!("REFIDX_bot{n}")}}),
    })
}

fn config(enabled: bool) -> QqOfficialConfig {
    QqOfficialConfig {
        enabled,
        app_id: "APP".into(),
        secret: Some("SECRET".into()),
        sandbox: false,
        markdown: false,
    }
}

fn endpoints(mock: &Mock) -> Endpoints {
    Endpoints {
        token_url: format!("http://{}/token", mock.addr),
        api_base: format!("http://{}", mock.addr),
    }
}

async fn next_socket(accepted: &mut mpsc::UnboundedReceiver<Socket>) -> Socket {
    timeout(WAIT, accepted.recv())
        .await
        .expect("adapter should open the gateway")
        .unwrap()
}

async fn next_event(ingest: &mut mpsc::Receiver<IngestEventRequest>) -> PipelineEventRequest {
    timeout(WAIT, ingest.recv())
        .await
        .expect("an event should be ingested")
        .unwrap()
        .event
        .unwrap()
}

async fn wait_for_state(adapter: &QqOfficialAdapter, state: ConnectionState) {
    timeout(WAIT, async {
        while adapter.status().connection_state != state {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("adapter never reached {state:?}: {:?}", adapter.status()));
}

fn reply_snippet(event: &PipelineEventRequest) -> String {
    match &event.segments[0].segment {
        Some(Segment::Reply(reply)) => reply.snippet.clone(),
        other => panic!("expected a reply segment, got {other:?}"),
    }
}

#[tokio::test]
async fn full_round_trip_with_quotes_and_resume() {
    let (mock, mut accepted) = start_mock().await;
    let adapter = QqOfficialAdapter::with_endpoints(config(true), endpoints(&mock)).unwrap();
    let (ingest_tx, mut ingest) = mpsc::channel(8);
    adapter.start(EventIngress::new(ingest_tx)).await.unwrap();

    // Identify with the bot's token and the group/C2C, guild and DM intents.
    let mut socket = next_socket(&mut accepted).await;
    socket.send(json!({"op": 10, "d": {"heartbeat_interval": 30000}}));
    let identify = socket.recv().await;
    assert_eq!(identify["op"], 2);
    assert_eq!(identify["d"]["token"], "QQBot TOKEN");
    assert_eq!(
        identify["d"]["intents"],
        (1u64 << 25) | (1 << 30) | (1 << 12)
    );
    assert_eq!(identify["d"]["shard"], json!([0, 1]));
    socket.send(json!({"op": 0, "s": 1, "t": "READY", "d": {
        "session_id": "SESSION", "user": {"id": "BOT", "username": "kanon-bot"}
    }}));
    wait_for_state(&adapter, ConnectionState::Connected).await;
    assert_eq!(adapter.status().bot_name.as_deref(), Some("kanon-bot"));
    assert!(adapter.is_connected());

    // A group @-message quoting a picture reaches the core with the quote attached.
    socket.send(
        json!({"op": 0, "s": 2, "t": "GROUP_AT_MESSAGE_CREATE", "d": {
            "id": "MSG1",
            "content": " 这是什么",
            "group_openid": "G1",
            "author": {"member_openid": "M1"},
            "message_type": 103,
            "message_scene": {"ext": ["msg_idx=REFIDX_u1", "ref_msg_idx=REFIDX_q"]},
            "msg_elements": [{"msg_idx": "REFIDX_q", "content": "我的猫", "attachments": [
                {"content_type": "image/png", "url": "https://img/cat.png"}
            ]}],
        }}),
    );
    let event = next_event(&mut ingest).await;
    assert_eq!(event.event_id, "MSG1");
    assert_eq!(event.channel_id, "group:G1");
    assert_eq!(reply_snippet(&event), "我的猫 [image]");
    assert!(matches!(event.segments[1].segment, Some(Segment::Image(_))));

    // The reply goes out as a passive reply: text first, then the uploaded image.
    let response = adapter
        .deliver(DeliverMessageRequest {
            platform: "qqofficial".into(),
            channel_id: "group:G1".into(),
            recipient_id: "M1".into(),
            event_id: "MSG1".into(),
            segments: vec![
                MessageSegment {
                    segment: Some(Segment::Text(TextSegment {
                        content: "是一只猫".into(),
                    })),
                },
                MessageSegment {
                    segment: Some(Segment::Image(ImageSegment {
                        source: Some(image_segment::Source::RawBytes(vec![1, 2, 3])),
                        mime_type: None,
                        filename: None,
                    })),
                },
            ],
        })
        .await
        .expect("delivery should succeed");
    assert!(response.success);
    assert_eq!(response.message_id, "sent-3");
    {
        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        let (path, text) = &calls[0];
        assert_eq!(path, "/v2/groups/G1/messages");
        assert_eq!(text["msg_type"], 0);
        assert_eq!(text["content"], "是一只猫");
        assert_eq!(text["msg_id"], "MSG1");
        let (path, upload) = &calls[1];
        assert_eq!(path, "/v2/groups/G1/files");
        assert_eq!(
            upload,
            &json!({"file_type": 1, "srv_send_msg": false, "file_data": "AQID"})
        );
        let (_, media) = &calls[2];
        assert_eq!(media["msg_type"], 7);
        assert_eq!(media["media"], json!({"file_info": "FILE_INFO"}));
        assert_eq!(media["msg_id"], "MSG1");
        assert_ne!(
            text["msg_seq"], media["msg_seq"],
            "msg_seq must differ per reply"
        );
    }

    // Quoting the bot's own reply resolves from what the adapter sent, even without elements.
    socket.send(
        json!({"op": 0, "s": 3, "t": "GROUP_AT_MESSAGE_CREATE", "d": {
            "id": "MSG2",
            "content": "真的吗",
            "group_openid": "G1",
            "author": {"member_openid": "M1"},
            "message_type": 103,
            "message_scene": {"ext": ["ref_msg_idx=REFIDX_bot1"]},
            "msg_elements": [{"msg_idx": "REFIDX_bot1"}],
        }}),
    );
    assert_eq!(reply_snippet(&next_event(&mut ingest).await), "是一只猫");

    // A dropped socket is resumed with the session and the last sequence number.
    socket.to_client.send(Message::Close(None)).unwrap();
    let mut socket = next_socket(&mut accepted).await;
    socket.send(json!({"op": 10, "d": {"heartbeat_interval": 30000}}));
    let resume = socket.recv().await;
    assert_eq!(resume["op"], 6);
    assert_eq!(resume["d"]["session_id"], "SESSION");
    assert_eq!(resume["d"]["seq"], 3);
    socket.send(json!({"op": 0, "s": 4, "t": "RESUMED", "d": {}}));
    wait_for_state(&adapter, ConnectionState::Connected).await;

    adapter.stop().await.unwrap();
    assert_eq!(adapter.status().connection_state, ConnectionState::Stopped);
}

/// Missing intent permissions cannot be fixed by retrying: the adapter stops and says why.
#[tokio::test]
async fn intents_rejection_stops_with_an_explanation() {
    let (mock, mut accepted) = start_mock().await;
    let adapter = QqOfficialAdapter::with_endpoints(config(true), endpoints(&mock)).unwrap();
    let (ingest_tx, _ingest) = mpsc::channel(8);
    adapter.start(EventIngress::new(ingest_tx)).await.unwrap();

    let mut socket = next_socket(&mut accepted).await;
    socket.send(json!({"op": 10, "d": {"heartbeat_interval": 30000}}));
    socket.recv().await;
    socket
        .to_client
        .send(Message::Close(Some(CloseFrame {
            code: 4914,
            reason: "disallowed intents".into(),
        })))
        .unwrap();

    wait_for_state(&adapter, ConnectionState::Disconnected).await;
    let error = adapter.status().last_error.unwrap_or_default();
    assert!(error.contains("4914"), "{error}");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        accepted.try_recv().is_err(),
        "a fatal close must not reconnect"
    );
}

/// A disabled adapter refuses deliveries, and enabling needs credentials.
#[tokio::test]
async fn disabled_adapter_refuses_delivery_and_validates_credentials() {
    let adapter = QqOfficialAdapter::new(QqOfficialConfig::default()).unwrap();
    assert_eq!(adapter.status().connection_state, ConnectionState::Disabled);
    let result = adapter
        .deliver(DeliverMessageRequest {
            platform: "qqofficial".into(),
            channel_id: "c2c:U1".into(),
            segments: vec![MessageSegment {
                segment: Some(Segment::Text(TextSegment {
                    content: "hi".into(),
                })),
            }],
            ..Default::default()
        })
        .await;
    assert!(matches!(result, Err(AdapterError::Configuration { .. })));

    let missing_secret = QqOfficialConfig {
        enabled: true,
        app_id: "APP".into(),
        ..Default::default()
    };
    assert!(QqOfficialAdapter::new(missing_secret.clone()).is_err());
    assert!(adapter.apply(missing_secret).await.is_err());
    assert!(
        !adapter.config().enabled,
        "a rejected apply changes nothing"
    );
}

/// Connects the adapter and completes identify and READY on the mock gateway.
async fn connected(
    mock: &Mock,
    accepted: &mut mpsc::UnboundedReceiver<Socket>,
) -> (
    QqOfficialAdapter,
    Socket,
    mpsc::Receiver<IngestEventRequest>,
) {
    let adapter = QqOfficialAdapter::with_endpoints(config(true), endpoints(mock)).unwrap();
    let (ingest_tx, ingest) = mpsc::channel(8);
    adapter.start(EventIngress::new(ingest_tx)).await.unwrap();
    let mut socket = next_socket(accepted).await;
    socket.send(json!({"op": 10, "d": {"heartbeat_interval": 30000}}));
    socket.recv().await;
    socket.send(
        json!({"op": 0, "s": 1, "t": "READY", "d": {"session_id": "S", "user": {"id": "BOT"}}}),
    );
    wait_for_state(&adapter, ConnectionState::Connected).await;
    (adapter, socket, ingest)
}

fn text_segment(content: &str) -> MessageSegment {
    MessageSegment {
        segment: Some(Segment::Text(TextSegment {
            content: content.into(),
        })),
    }
}

/// Typing in C2C, a native quote, a document sent as a file, and a greeting for being added to a
/// group — which QQ only accepts as a passive reply quoting the gateway event.
#[tokio::test]
async fn typing_quotes_files_and_join_greetings() {
    let (mock, mut accepted) = start_mock().await;
    let (adapter, socket, mut ingest) = connected(&mock, &mut accepted).await;

    socket.send(json!({"op": 0, "s": 2, "t": "C2C_MESSAGE_CREATE", "d": {
        "id": "C1", "content": "在吗", "author": {"user_openid": "U1"},
    }}));
    let private = next_event(&mut ingest).await;
    adapter
        .acknowledge(&private)
        .await
        .expect("typing indicator");
    {
        let calls = mock.calls.lock().unwrap();
        let (path, typing) = calls.last().expect("typing request");
        assert_eq!(path, "/v2/users/U1/messages");
        assert_eq!(typing["msg_type"], 6);
        assert_eq!(typing["input_notify"]["input_type"], 1);
        assert_eq!(typing["msg_id"], "C1");
    }

    let document = std::env::temp_dir().join(format!("kanon-qq-{}.pdf", std::process::id()));
    std::fs::write(&document, b"%PDF").unwrap();
    adapter
        .deliver(DeliverMessageRequest {
            platform: "qqofficial".into(),
            channel_id: "group:G1".into(),
            event_id: "MSG9".into(),
            segments: vec![
                MessageSegment {
                    segment: Some(Segment::Reply(ReplySegment {
                        target_message_id: "MSG9".into(),
                        snippet: String::new(),
                    })),
                },
                text_segment("报告在这"),
                MessageSegment {
                    segment: Some(Segment::Image(ImageSegment {
                        source: Some(image_segment::Source::FilePath(
                            document.to_string_lossy().into_owned(),
                        )),
                        mime_type: Some("application/pdf".into()),
                        filename: None,
                    })),
                },
            ],
            ..Default::default()
        })
        .await
        .expect("delivery");
    std::fs::remove_file(&document).ok();
    {
        let calls = mock.calls.lock().unwrap();
        let recent = &calls[calls.len() - 3..];
        assert_eq!(
            recent[0].1["message_reference"],
            json!({"message_id": "MSG9"})
        );
        assert_eq!(recent[1].0, "/v2/groups/G1/files");
        assert_eq!(recent[1].1["file_type"], 4);
        assert_eq!(
            recent[1].1["file_name"],
            document.file_name().unwrap().to_string_lossy().as_ref()
        );
        assert_eq!(recent[2].1["msg_type"], 7);
    }

    socket.send(
        json!({"op": 0, "s": 3, "t": "GROUP_ADD_ROBOT", "id": "GROUP_ADD_ROBOT:abc",
        "d": {"group_openid": "G2", "op_member_openid": "OP", "timestamp": 1}}),
    );
    let greeting = next_event(&mut ingest).await;
    assert_eq!(greeting.channel_id, "group:G2");
    assert_eq!(greeting.event_id, "event:GROUP_ADD_ROBOT:abc");
    adapter
        .deliver(DeliverMessageRequest {
            platform: "qqofficial".into(),
            channel_id: greeting.channel_id.clone(),
            event_id: greeting.event_id.clone(),
            segments: vec![text_segment("大家好")],
            ..Default::default()
        })
        .await
        .expect("greeting");
    let calls = mock.calls.lock().unwrap();
    let (path, body) = calls.last().unwrap();
    assert_eq!(path, "/v2/groups/G2/messages");
    assert_eq!(body["event_id"], "GROUP_ADD_ROBOT:abc");
    assert!(body.get("msg_id").is_none());
}

/// A split reply line keeps its indentation; only surrounding blank lines are dropped.
#[tokio::test]
async fn delivery_preserves_line_indentation() {
    let (mock, mut accepted) = start_mock().await;
    let (adapter, _socket, _ingest) = connected(&mock, &mut accepted).await;

    adapter
        .deliver(DeliverMessageRequest {
            platform: "qqofficial".into(),
            channel_id: "c2c:U1".into(),
            event_id: "C1".into(),
            segments: vec![text_segment("\n    let x = 1;  \r\n")],
            ..Default::default()
        })
        .await
        .expect("delivery");
    let calls = mock.calls.lock().unwrap();
    let (path, body) = calls.last().unwrap();
    assert_eq!(path, "/v2/users/U1/messages");
    assert_eq!(body["content"], "    let x = 1;  ");

    drop(calls);
    let blank = adapter
        .deliver(DeliverMessageRequest {
            platform: "qqofficial".into(),
            channel_id: "c2c:U1".into(),
            event_id: "C1".into(),
            segments: vec![text_segment(" \n\t ")],
            ..Default::default()
        })
        .await;
    assert!(blank.is_err(), "whitespace-only text must not be sent");
}
