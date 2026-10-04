//! One bounded, owned remote stream; dropping it closes the DSH subscriber connection.

use super::client::remote_error;
use super::{DshClient, DshError, MAX_WIRE_BYTES};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};

/// A single DSH remote.mux subscription, with no detached reader or unbounded inbox.
pub struct DshStream {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    stream_id: String,
}

impl DshClient {
    /// Opens a public Remote stream. The caller must receive its initial snapshot before mutating.
    pub async fn open_stream(&self, endpoint: &str, args: Value) -> Result<DshStream, DshError> {
        let mut url = reqwest::Url::parse(&self.url("/api/remote.mux"))
            .map_err(|e| DshError::Config(e.to_string()))?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme)
            .map_err(|_| DshError::Config("invalid WebSocket scheme".into()))?;
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| DshError::Transport(e.to_string()))?;
        request.headers_mut().extend(self.headers().await?);
        let config = WebSocketConfig {
            max_message_size: Some(MAX_WIRE_BYTES),
            max_frame_size: Some(MAX_WIRE_BYTES),
            ..Default::default()
        };
        let (mut socket, _) = tokio::time::timeout(
            self.config.request_timeout(),
            tokio_tungstenite::connect_async_with_config(request, Some(config), false),
        )
        .await
        .map_err(|_| DshError::Timeout("stream connection"))?
        .map_err(|e| DshError::Transport(e.to_string()))?;
        let stream_id = Self::request_id();
        let open = json!({"type": "open", "streamId": stream_id, "endpoint": endpoint, "payload": {"args": args}});
        tokio::time::timeout(
            self.config.request_timeout(),
            socket.send(Message::Text(open.to_string())),
        )
        .await
        .map_err(|_| DshError::Timeout("stream subscription"))?
        .map_err(|e| DshError::Transport(e.to_string()))?;
        Ok(DshStream { socket, stream_id })
    }
}

impl DshStream {
    /// Receives the next item. Transport closure and malformed frames fail explicitly.
    ///
    /// Time bounds belong to the caller: management snapshots use the request deadline, while
    /// an agent turn may legitimately wait much longer for a tool or user question.
    pub async fn next(&mut self) -> Result<Value, DshError> {
        loop {
            let message = self
                .socket
                .next()
                .await
                .ok_or_else(|| {
                    DshError::Transport("remote stream closed before completion".into())
                })?
                .map_err(|e| DshError::Transport(e.to_string()))?;
            match message {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(&text)
                        .map_err(|e| DshError::Protocol(e.to_string()))?;
                    if value["streamId"] != self.stream_id {
                        return Err(DshError::Protocol("stream correlation id mismatch".into()));
                    }
                    match value["type"].as_str() {
                        Some("item") if value.get("value").is_some() => {
                            return Ok(value["value"].clone());
                        }
                        Some("error") => return Err(remote_error(&value["error"])?),
                        Some("end") => {
                            return Err(DshError::Transport("remote subscription ended".into()));
                        }
                        _ => return Err(DshError::Protocol("unknown remote stream frame".into())),
                    }
                }
                Message::Ping(_) => {
                    // Tungstenite queues the matching pong while reading. Flush it immediately,
                    // including during long tool waits, without another task or message queue.
                    self.socket
                        .flush()
                        .await
                        .map_err(|e| DshError::Transport(e.to_string()))?;
                }
                Message::Pong(_) => {}
                Message::Close(_) => {
                    return Err(DshError::Transport("remote stream closed".into()));
                }
                _ => return Err(DshError::Protocol("unexpected binary remote frame".into())),
            }
        }
    }
}
