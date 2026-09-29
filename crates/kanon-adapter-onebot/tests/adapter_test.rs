//! Protocol-level tests with real loopback WebSockets, independent of a QQ installation.
use futures_util::{SinkExt, StreamExt};
use kanon_adapter_onebot::{ConnectionState, OneBotAdapter, OneBotConfig, TransportKind};
use kanon_core::{EventIngress, PlatformAdapter};
use kanon_proto::v1::{
    DeliverMessageRequest, IngestEventRequest, MessageSegment, TextSegment,
    message_segment::Segment,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::timeout,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn delivery(text: &str) -> DeliverMessageRequest {
    DeliverMessageRequest {
        platform: "onebot".into(),
        channel_id: "group:123".into(),
        segments: vec![MessageSegment {
            segment: Some(Segment::Text(TextSegment {
                content: text.into(),
            })),
        }],
        ..Default::default()
    }
}

fn event(id: i64) -> Value {
    json!({"post_type":"message", "message_type":"group", "self_id":100, "user_id":200, "group_id":123, "message_id":id, "time":1234567, "message":[{"type":"text","data":{"text":"hello"}}]})
}

async fn connected(adapter: &OneBotAdapter) {
    timeout(Duration::from_secs(3), async {
        while !adapter.is_connected() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn receive<S>(socket: &mut WebSocketStream<S>) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Ping(_) => socket.flush().await.unwrap(),
                other => panic!("unexpected {other:?}"),
            }
        }
    })
    .await
    .unwrap()
}

async fn reverse() -> (
    Arc<OneBotAdapter>,
    mpsc::Receiver<IngestEventRequest>,
    String,
) {
    let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/onebot", reserve.local_addr().unwrap());
    drop(reserve);
    let adapter = Arc::new(
        OneBotAdapter::new(OneBotConfig {
            enabled: true,
            transport: TransportKind::ReverseWebsocket,
            ws_url: url.clone(),
            access_token: Some("secret".into()),
            ..Default::default()
        })
        .unwrap(),
    );
    let (tx, rx) = mpsc::channel(1);
    adapter.start(EventIngress::new(tx)).await.unwrap();
    (adapter, rx, url)
}

async fn connect_reverse(
    url: &str,
    token: &str,
    role: &str,
) -> Result<Client, tokio_tungstenite::tungstenite::Error> {
    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    request
        .headers_mut()
        .insert("x-client-role", HeaderValue::from_str(role).unwrap());
    request
        .headers_mut()
        .insert("x-self-id", HeaderValue::from_static("100"));
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(s, _)| s)
}

#[tokio::test]
async fn forward_auth_ingress_delivery_and_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let adapter = Arc::new(
        OneBotAdapter::new(OneBotConfig {
            enabled: true,
            ws_url: format!("ws://{}", listener.local_addr().unwrap()),
            access_token: Some("secret".into()),
            ..Default::default()
        })
        .unwrap(),
    );
    let (tx, mut rx) = mpsc::channel(4);
    adapter.start(EventIngress::new(tx)).await.unwrap();
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_hdr_async(
        stream,
        |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
            assert_eq!(request.headers()["authorization"], "Bearer secret");
            Ok(response)
        },
    )
    .await
    .unwrap();
    connected(&adapter).await;
    socket
        .send(Message::Text(event(1).to_string()))
        .await
        .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap()
            .event
            .unwrap()
            .channel_id,
        "group:123"
    );
    let task = tokio::spawn({
        let adapter = adapter.clone();
        async move { adapter.deliver(delivery("reply")).await }
    });
    let action = receive(&mut socket).await;
    assert_eq!(action["action"], "send_group_msg");
    assert_eq!(action["params"]["group_id"], 123);
    socket
        .send(Message::Text(
            json!({"echo":action["echo"],"status":"ok","retcode":0,"data":{"message_id":-17}})
                .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(task.await.unwrap().unwrap().message_id, "-17");
    socket.close(None).await.unwrap();
    drop(socket);
    let (stream, _) = timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let _socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    connected(&adapter).await;
    adapter.stop().await.unwrap();
    assert!(!adapter.is_connected());
}

#[tokio::test]
async fn reverse_rejects_auth_path_role_and_second_client() {
    let (adapter, _rx, url) = reverse().await;
    assert_eq!(
        adapter.status().connection_state,
        ConnectionState::Listening
    );
    for (url, token, role) in [
        (url.clone(), "wrong", "Universal"),
        (url.replace("/onebot", "/wrong"), "secret", "Universal"),
        (url.clone(), "secret", "Event"),
    ] {
        assert!(connect_reverse(&url, token, role).await.is_err());
        assert!(!adapter.is_connected());
    }
    let _socket = connect_reverse(&url, "secret", "Universal").await.unwrap();
    connected(&adapter).await;
    assert_eq!(adapter.status().self_id.as_deref(), Some("100"));
    assert!(connect_reverse(&url, "secret", "Universal").await.is_err());
    assert!(adapter.is_connected());
    adapter.stop().await.unwrap();
    assert!(connect_reverse(&url, "secret", "Universal").await.is_err());
}

#[tokio::test]
async fn reverse_correlates_out_of_order_calls_and_reports_failure() {
    let (adapter, _rx, url) = reverse().await;
    let mut socket = connect_reverse(&url, "secret", "Universal").await.unwrap();
    connected(&adapter).await;
    let first = tokio::spawn({
        let a = adapter.clone();
        async move { a.deliver(delivery("first")).await }
    });
    let first_wire = receive(&mut socket).await;
    let second = tokio::spawn({
        let a = adapter.clone();
        async move { a.deliver(delivery("second")).await }
    });
    let second_wire = receive(&mut socket).await;
    socket
        .send(Message::Text(
            json!({"echo":second_wire["echo"],"status":"failed","retcode":100,"wording":"secret"})
                .to_string(),
        ))
        .await
        .unwrap();
    socket
        .send(Message::Text(
            json!({"echo":first_wire["echo"],"status":"ok","retcode":0,"data":{"message_id":1}})
                .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(first.await.unwrap().unwrap().message_id, "1");
    let error = second.await.unwrap().unwrap_err().to_string();
    assert!(error.contains("100"));
    assert!(!error.contains("secret"));
    adapter.stop().await.unwrap();
}

#[tokio::test]
async fn quoted_message_is_fetched_before_ingest() {
    let (adapter, mut rx, url) = reverse().await;
    let mut socket = connect_reverse(&url, "secret", "Universal").await.unwrap();
    connected(&adapter).await;
    let mut quoting = event(2);
    quoting["message"] =
        json!([{"type":"reply","data":{"id":"1"}},{"type":"text","data":{"text":"what is this?"}}]);
    socket
        .send(Message::Text(quoting.to_string()))
        .await
        .unwrap();
    let lookup = receive(&mut socket).await;
    assert_eq!(lookup["action"], "get_msg");
    assert_eq!(lookup["params"]["message_id"], 1);
    socket
        .send(Message::Text(
            json!({"echo":lookup["echo"],"status":"ok","retcode":0,"data":{"message":[
                {"type":"text","data":{"text":"look"}},
                {"type":"image","data":{"file":"a.image","url":"https://cdn.example/a.png"}}
            ]}})
            .to_string(),
        ))
        .await
        .unwrap();
    let ingested = timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let segments = ingested.event.unwrap().segments;
    assert!(
        matches!(&segments[0].segment, Some(Segment::Reply(reply)) if reply.snippet == "look[image]")
    );
    assert!(matches!(&segments[1].segment, Some(Segment::Image(_))));
    adapter.stop().await.unwrap();
}

#[tokio::test]
async fn saturated_ingress_does_not_block_api_or_ping_and_disconnect_fails_pending() {
    let (adapter, mut rx, url) = reverse().await;
    let mut socket = connect_reverse(&url, "secret", "Universal").await.unwrap();
    connected(&adapter).await;
    for id in 1..=5 {
        socket
            .send(Message::Text(event(id).to_string()))
            .await
            .unwrap();
    }
    socket.send(Message::Ping(vec![1, 2, 3])).await.unwrap();
    let pong = timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(pong, Message::Pong(vec![1, 2, 3]));
    let task = tokio::spawn({
        let a = adapter.clone();
        async move { a.deliver(delivery("pending")).await }
    });
    receive(&mut socket).await;
    socket.close(None).await.unwrap();
    drop(socket);
    let error = timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("disconnected"));
    assert!(rx.recv().await.is_some());
    let _replacement = connect_reverse(&url, "secret", "Universal").await.unwrap();
    connected(&adapter).await;
    adapter.stop().await.unwrap();
}

#[tokio::test]
async fn reconfiguration_reuses_listener_and_failed_bind_preserves_session() {
    let (adapter, _rx, url) = reverse().await;
    let mut socket = connect_reverse(&url, "secret", "Universal").await.unwrap();
    connected(&adapter).await;
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let old = adapter.config();
    let candidate = OneBotConfig {
        ws_url: format!("ws://{}", occupied.local_addr().unwrap()),
        ..old.clone()
    };
    assert!(adapter.apply(candidate).await.is_err());
    assert_eq!(adapter.config(), old);
    assert!(adapter.is_connected());
    let task = tokio::spawn({
        let a = adapter.clone();
        async move { a.deliver(delivery("pending")).await }
    });
    receive(&mut socket).await;
    adapter
        .apply(OneBotConfig {
            access_token: Some("new-secret".into()),
            ..old
        })
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(connect_reverse(&url, "secret", "Universal").await.is_err());
    let _socket = connect_reverse(&url, "new-secret", "Universal")
        .await
        .unwrap();
    connected(&adapter).await;
    adapter
        .apply(OneBotConfig {
            enabled: false,
            ..adapter.config()
        })
        .await
        .unwrap();
    assert!(!adapter.is_connected());
    assert!(adapter.deliver(delivery("disabled")).await.is_err());
    assert!(
        connect_reverse(&url, "new-secret", "Universal")
            .await
            .is_err()
    );
    adapter.stop().await.unwrap();
}

#[tokio::test]
async fn api_timeout_is_bounded_without_replaying_message() {
    let (adapter, _rx, url) = reverse().await;
    let mut socket = connect_reverse(&url, "secret", "Universal").await.unwrap();
    connected(&adapter).await;
    let task = tokio::spawn({
        let a = adapter.clone();
        async move { a.deliver(delivery("timeout")).await }
    });
    receive(&mut socket).await;
    let error = timeout(Duration::from_secs(17), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(
        timeout(Duration::from_millis(100), socket.next())
            .await
            .is_err()
    );
    adapter.stop().await.unwrap();
}

#[test]
fn config_rejects_invalid_urls_headers_and_reverse_addresses() {
    for url in [
        "http://127.0.0.1:6700",
        "ws://user:pass@localhost",
        "ws://localhost/?access_token=secret",
        "ws://localhost/#secret",
    ] {
        assert!(
            OneBotConfig {
                ws_url: url.into(),
                ..Default::default()
            }
            .prepare()
            .is_err()
        );
    }
    for url in [
        "wss://127.0.0.1:6700",
        "ws://localhost:6700",
        "ws://127.0.0.1:0",
    ] {
        assert!(
            OneBotConfig {
                transport: TransportKind::ReverseWebsocket,
                ws_url: url.into(),
                ..Default::default()
            }
            .prepare()
            .is_err()
        );
    }
    assert!(
        OneBotConfig {
            access_token: Some("secret\r\nInjected: yes".into()),
            ..Default::default()
        }
        .prepare()
        .is_err()
    );
    assert!(
        OneBotConfig {
            transport: TransportKind::ReverseWebsocket,
            ws_url: "ws://[::1]:6700/onebot".into(),
            ..Default::default()
        }
        .prepare()
        .is_ok()
    );
}
