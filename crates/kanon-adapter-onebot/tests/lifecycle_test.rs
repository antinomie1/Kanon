//! Cancellation and local-file delivery checks against a real reverse WebSocket peer.

use std::{future::Future, path::PathBuf, sync::Arc, task::Poll, time::Duration};

use futures_util::{SinkExt, StreamExt};
use kanon_adapter_onebot::{ConnectionState, OneBotAdapter, OneBotConfig, TransportKind};
use kanon_core::{EventIngress, PlatformAdapter};
use kanon_proto::v1::{
    AudioSegment, DeliverMessageRequest, ImageSegment, MessageSegment, audio_segment,
    image_segment, message_segment::Segment,
};
use serde_json::{Value, json};
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

async fn adapter() -> (Arc<OneBotAdapter>, String) {
    let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/onebot", reserve.local_addr().unwrap());
    drop(reserve);
    let adapter = Arc::new(
        OneBotAdapter::new(OneBotConfig {
            enabled: true,
            transport: TransportKind::ReverseWebsocket,
            ws_url: url.clone(),
            access_token: Some("before".into()),
            ..Default::default()
        })
        .unwrap(),
    );
    (adapter, url)
}

async fn connect(url: &str, token: &str) -> Client {
    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    request
        .headers_mut()
        .insert("x-client-role", HeaderValue::from_static("Universal"));
    request
        .headers_mut()
        .insert("x-self-id", HeaderValue::from_static("100"));
    timeout(
        Duration::from_secs(3),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .unwrap()
    .unwrap()
    .0
}

async fn wait_state(adapter: &OneBotAdapter, expected: ConnectionState) {
    timeout(Duration::from_secs(3), async {
        while adapter.status().connection_state != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

/// A first poll dispatches the mutation; dropping its waiter models a canceled HTTP handler.
async fn cancel_waiter(future: impl Future) {
    let mut future = Box::pin(future);
    assert!(matches!(futures_util::poll!(&mut future), Poll::Pending));
    drop(future);
}

#[tokio::test]
async fn canceled_lifecycle_waiters_complete_start_apply_and_stop() {
    let (adapter, url) = adapter().await;
    let (sender, _receiver) = mpsc::channel(1);
    cancel_waiter(adapter.start(EventIngress::new(sender.clone()))).await;
    wait_state(&adapter, ConnectionState::Listening).await;
    let first = connect(&url, "before").await;
    wait_state(&adapter, ConnectionState::Connected).await;

    let config = OneBotConfig {
        access_token: Some("after".into()),
        ..adapter.config()
    };
    let (saved_tx, saved_rx) = tokio::sync::oneshot::channel();
    cancel_waiter(adapter.update_config(
        move |_| config,
        move |config| {
            saved_tx.send(config.clone()).unwrap();
            Ok(())
        },
    ))
    .await;
    let saved = timeout(Duration::from_secs(3), saved_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.access_token.as_deref(), Some("after"));
    wait_state(&adapter, ConnectionState::Listening).await;
    assert_eq!(adapter.config().access_token.as_deref(), Some("after"));
    assert!(!adapter.is_connected());
    drop(first);
    let second = connect(&url, "after").await;
    wait_state(&adapter, ConnectionState::Connected).await;

    cancel_waiter(adapter.stop()).await;
    wait_state(&adapter, ConnectionState::Stopped).await;
    assert!(!adapter.is_connected());
    let address = url
        .strip_prefix("ws://")
        .unwrap()
        .strip_suffix("/onebot")
        .unwrap();
    assert!(TcpStream::connect(address).await.is_err());
    drop(second);

    // Cancellation must not poison the lifecycle lock or leave an occupied, ownerless listener.
    adapter.start(EventIngress::new(sender)).await.unwrap();
    let _third = connect(&url, "after").await;
    wait_state(&adapter, ConnectionState::Connected).await;
    adapter.stop().await.unwrap();
}

#[tokio::test]
async fn dropping_adapter_after_canceled_start_releases_transport() {
    let (adapter, url) = adapter().await;
    let (sender, _receiver) = mpsc::channel(1);
    cancel_waiter(adapter.start(EventIngress::new(sender))).await;
    drop(adapter);
    // Let the short mutation complete and release its final lifecycle Arc. Its Drop aborts
    // the transport, so no orphan task can retain the listening port.
    let address = url
        .strip_prefix("ws://")
        .unwrap()
        .strip_suffix("/onebot")
        .unwrap();
    tokio::task::yield_now().await;
    timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(listener) = TcpListener::bind(address).await {
                drop(listener);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

struct Attachment(PathBuf);

impl Attachment {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("kanon-onebot-media-{}-{nonce}", std::process::id()));
        std::fs::write(&path, [1, 2, 3]).unwrap();
        Self(path)
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn media_request(path: &str) -> DeliverMessageRequest {
    DeliverMessageRequest {
        platform: "onebot".into(),
        channel_id: "group:123".into(),
        segments: vec![
            MessageSegment {
                segment: Some(Segment::Image(ImageSegment {
                    source: Some(image_segment::Source::FilePath(path.into())),
                    ..Default::default()
                })),
            },
            MessageSegment {
                segment: Some(Segment::Audio(AudioSegment {
                    source: Some(audio_segment::Source::FilePath(path.into())),
                    ..Default::default()
                })),
            },
        ],
        ..Default::default()
    }
}

#[tokio::test]
async fn local_attachments_are_sent_as_bytes_and_read_failures_do_not_send() {
    let (adapter, url) = adapter().await;
    let (sender, _receiver) = mpsc::channel(1);
    adapter.start(EventIngress::new(sender)).await.unwrap();
    let mut socket = connect(&url, "before").await;
    wait_state(&adapter, ConnectionState::Connected).await;
    let file = Attachment::new();
    let request = media_request(file.0.to_str().unwrap());
    let task = tokio::spawn({
        let adapter = adapter.clone();
        async move { adapter.deliver(request).await }
    });
    let Message::Text(text) = timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    else {
        panic!("expected API call")
    };
    let wire: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        wire["params"]["message"],
        json!([
            {"type": "image", "data": {"file": "base64://AQID"}},
            {"type": "record", "data": {"file": "base64://AQID"}}
        ])
    );
    socket
        .send(Message::Text(
            json!({"echo": wire["echo"], "status": "ok", "retcode": 0, "data": {"message_id": 1}})
                .to_string(),
        ))
        .await
        .unwrap();
    assert!(task.await.unwrap().unwrap().success);

    let missing = file.0.with_extension("missing");
    let error = adapter
        .deliver(media_request(missing.to_str().unwrap()))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot read OneBot image attachment")
    );
    let mut audio_only = media_request(missing.to_str().unwrap());
    audio_only.segments.remove(0);
    let error = adapter.deliver(audio_only).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot read OneBot audio attachment")
    );
    assert!(
        timeout(Duration::from_millis(100), socket.next())
            .await
            .is_err()
    );
    adapter.stop().await.unwrap();
}

/// A FIFO holds file preparation at a real I/O boundary while the adapter changes sessions.
#[cfg(unix)]
#[tokio::test]
async fn connection_switch_during_attachment_read_does_not_retarget_delivery() {
    let (adapter, url) = adapter().await;
    let (sender, _receiver) = mpsc::channel(1);
    adapter.start(EventIngress::new(sender)).await.unwrap();
    let _original = connect(&url, "before").await;
    wait_state(&adapter, ConnectionState::Connected).await;
    let file = Attachment::new();
    std::fs::remove_file(&file.0).unwrap();
    // Created in-process: spawning `mkfifo` would fork this multi-threaded test binary, and the
    // child would briefly hold every open socket, including listeners other tests just closed.
    let fifo = std::ffi::CString::new(file.0.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let mut request = media_request(file.0.to_str().unwrap());
    request.segments.truncate(1);
    let mut delivery = Box::pin(adapter.deliver(request));
    assert!(matches!(futures_util::poll!(&mut delivery), Poll::Pending));

    adapter
        .apply(OneBotConfig {
            access_token: Some("after".into()),
            ..adapter.config()
        })
        .await
        .unwrap();
    let mut replacement = connect(&url, "after").await;
    wait_state(&adapter, ConnectionState::Connected).await;
    // Release the blocked file read only after the replacement session is available.
    tokio::fs::write(&file.0, [1, 2, 3]).await.unwrap();
    let error = timeout(Duration::from_secs(2), delivery)
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("connection closed"));
    assert!(
        timeout(Duration::from_millis(100), replacement.next())
            .await
            .is_err()
    );
    adapter.stop().await.unwrap();
}
