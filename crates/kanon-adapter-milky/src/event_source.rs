//! Inbound event transports for the Milky `/event` endpoint.
//!
//! # Two transports, one payload
//! A Milky protocol implementation offers a single `/event` endpoint. A request carrying an
//! `Upgrade: websocket` header becomes a WebSocket connection; any other request is answered as a
//! Server-Sent Events stream. Both carry byte-identical JSON, so this module implements two
//! *framing* readers over one decoding path: [`decode_event`] is the only place that knows what a
//! Milky event is.
//!
//! # Why the loops reconnect instead of failing
//! An event stream is the adapter's entire inbound side; leaving it dead after the first network
//! blip would silently turn the bot deaf while the console still shows it as registered. The
//! reconnect loop therefore retries forever with exponential backoff bounded by
//! [`RECONNECT_MAX_BACKOFF`], and resets that backoff once a connection has been healthy for
//! [`HEALTHY_CONNECTION`] — so a protocol implementation that restarts every night reconnects
//! immediately, while one that is permanently down is polled every half minute.
//!
//! # Why the stream client has no total timeout
//! `reqwest`'s request timeout covers the whole response, body included, which would abort a
//! healthy SSE stream after that many seconds of a quiet conversation. The streaming client
//! therefore sets only a connect timeout, plus TCP keepalive so a peer that vanished without
//! closing its socket is still detected and reported instead of hanging for hours.

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;

use crate::protocol::Event;

use crate::client::MilkyError;
use crate::config::{
    MilkyConfig, RECONNECT_INITIAL_BACKOFF, RECONNECT_MAX_BACKOFF, REQUEST_TIMEOUT, TransportKind,
};

/// A connection that stayed up at least this long is treated as healthy, resetting the backoff.
const HEALTHY_CONNECTION: Duration = Duration::from_secs(30);

/// Idle TCP keepalive probe interval for the SSE connection.
///
/// Without it, a protocol implementation killed by the operating system (power loss, container
/// kill without FIN) would leave the stream open and the adapter blind indefinitely, because a
/// quiet conversation produces no traffic to reveal the dead peer.
const SSE_TCP_KEEPALIVE: Duration = Duration::from_secs(60);

/// Capacity of the channel between a transport loop and the adapter.
///
/// Small on purpose: the adapter drains it synchronously into a non-blocking ingest queue, so a
/// backlog here means the core is saturated and the events are stale anyway. TCP backpressure on
/// the event stream is the honest response.
const STREAM_CHANNEL_CAPACITY: usize = 256;

/// What a transport loop reports to the adapter.
#[derive(Debug)]
pub enum StreamEvent {
    /// The stream was established; the adapter is live again.
    Connected,
    /// A decoded protocol event arrived.
    Event(Box<Event>),
    /// The stream dropped; the loop is about to retry after its backoff.
    ///
    /// The reason is carried verbatim so the console can show operators *why* an adapter keeps
    /// reconnecting instead of only that it does.
    Disconnected(String),
}

/// Handle owning a running transport loop.
///
/// Dropping the handle also stops the loop: an unreferenced stream would otherwise keep a live
/// connection and a task alive with nobody able to observe or stop it.
pub struct EventSourceHandle {
    /// Set to `true` to ask the loop to return.
    stop: watch::Sender<bool>,
    /// The loop task itself, awaited by [`EventSourceHandle::shutdown`].
    ///
    /// Optional so `shutdown(self)` can take it out of a value that implements `Drop` without
    /// moving out of a type that owns a destructor.
    task: Option<JoinHandle<()>>,
}

impl EventSourceHandle {
    /// Stops the loop and waits for it to finish.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for EventSourceHandle {
    fn drop(&mut self) {
        // Best-effort stop plus an abort: `shutdown` already awaited the task in the normal path,
        // so this only matters when a handle is dropped accidentally, and aborting is then the
        // only way to guarantee the connection does not outlive its owner.
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Factory for inbound event streams of one Milky implementation.
pub struct EventSource {
    /// Connection settings (base URL, credential, transport).
    config: MilkyConfig,
    /// Client used for the SSE transport.
    ///
    /// Separate from the API client because the two need different timeouts: API calls must fail
    /// fast, while a stream must survive hours of silence.
    http: reqwest::Client,
}

impl EventSource {
    /// Builds an event source for a prepared configuration.
    pub fn new(config: MilkyConfig) -> Result<Self, MilkyError> {
        let http = reqwest::Client::builder()
            .connect_timeout(REQUEST_TIMEOUT)
            .tcp_keepalive(SSE_TCP_KEEPALIVE)
            .build()
            .map_err(|err| {
                MilkyError::Transport(format!("failed to build streaming HTTP client: {err}"))
            })?;

        Ok(Self { config, http })
    }

    /// Spawns the reconnect loop and returns its handle together with the event receiver.
    ///
    /// The channel is created here rather than by the caller so that its capacity stays an
    /// internal detail of the transport: the adapter drains it synchronously and must never be
    /// tempted to resize the only buffer standing between a slow consumer and a lost event.
    pub fn spawn(self) -> (EventSourceHandle, mpsc::Receiver<StreamEvent>) {
        let (sink, receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(self.run(sink, stop_rx));
        (
            EventSourceHandle {
                stop: stop_tx,
                task: Some(task),
            },
            receiver,
        )
    }

    /// Runs the reconnect loop until the stop flag is raised.
    async fn run(self, sink: mpsc::Sender<StreamEvent>, mut stop: watch::Receiver<bool>) {
        let mut backoff = RECONNECT_INITIAL_BACKOFF;

        loop {
            if *stop.borrow() {
                return;
            }

            let started = Instant::now();
            let outcome = match self.config.transport {
                TransportKind::Sse => self.sse_once(&sink, &mut stop).await,
                TransportKind::Websocket => self.websocket_once(&sink, &mut stop).await,
            };

            if *stop.borrow() {
                return;
            }

            // A clean end of stream is still a disconnect: the adapter must not keep reporting a
            // connection that no longer exists, so both outcomes are reported as a drop.
            let reason = match outcome {
                Ok(()) => "event stream closed by the protocol implementation".to_string(),
                Err(reason) => reason,
            };

            if started.elapsed() >= HEALTHY_CONNECTION {
                backoff = RECONNECT_INITIAL_BACKOFF;
            }
            if sink.send(StreamEvent::Disconnected(reason)).await.is_err() {
                // The adapter dropped its receiver: nothing left to report to.
                return;
            }

            backoff = (backoff * 2).min(RECONNECT_MAX_BACKOFF);

            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = stop.changed() => return,
            }
        }
    }

    /// Reads one Server-Sent Events connection until it ends.
    async fn sse_once(
        &self,
        sink: &mpsc::Sender<StreamEvent>,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<(), String> {
        let mut request = self
            .http
            .get(format!("{}/event", self.config.base_url))
            .header(reqwest::header::ACCEPT, "text/event-stream");
        if let Some(header) = self.config.authorization_header() {
            request = request.header(reqwest::header::AUTHORIZATION, header);
        }

        let response = request
            .send()
            .await
            .map_err(|err| format!("failed to open event stream: {err}"))?;

        let status = response.status();
        if status != reqwest::StatusCode::OK {
            return Err(format!("event stream returned HTTP {status}"));
        }

        send(sink, StreamEvent::Connected).await?;

        let mut stream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::with_capacity(8192);
        // Payload accumulated from consecutive `data:` lines; the SSE specification joins them
        // with newlines, and Milky implementations do split pretty-printed JSON across lines.
        let mut payload = String::new();

        loop {
            let chunk = tokio::select! {
                chunk = stream.next() => chunk,
                _ = stop.changed() => return Ok(()),
            };

            let Some(chunk) = chunk else {
                return Err("event stream ended".to_string());
            };
            let chunk = chunk.map_err(|err| format!("event stream failed: {err}"))?;
            buffer.extend_from_slice(&chunk);

            while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                let raw: Vec<u8> = buffer.drain(..=newline).collect();
                let lossy = String::from_utf8_lossy(&raw[..raw.len() - 1]);
                let line = lossy.trim_end_matches('\r');

                if line.is_empty() {
                    // A blank line terminates one event.
                    if !payload.is_empty() {
                        dispatch(&payload, sink).await?;
                        payload.clear();
                    }
                    continue;
                }

                if let Some(rest) = line.strip_prefix("data:") {
                    if !payload.is_empty() {
                        payload.push('\n');
                    }
                    payload.push_str(rest.strip_prefix(' ').unwrap_or(rest));
                }
                // `event:`, `id:`, `retry:` and `:` comment lines carry nothing the adapter needs;
                // keepalive comments in particular must be ignored rather than parsed.
            }
        }
    }

    /// Reads one WebSocket connection until it ends.
    async fn websocket_once(
        &self,
        sink: &mpsc::Sender<StreamEvent>,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<(), String> {
        let mut request = self
            .config
            .event_url()
            .into_client_request()
            .map_err(|err| format!("invalid event URL: {err}"))?;

        if let Some(header) = self.config.authorization_header() {
            let value = HeaderValue::from_str(&header)
                .map_err(|err| format!("invalid access token header: {err}"))?;
            request.headers_mut().insert(AUTHORIZATION, value);
        }

        let (mut socket, _response) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|err| format!("failed to open event WebSocket: {err}"))?;

        send(sink, StreamEvent::Connected).await?;

        loop {
            let message = tokio::select! {
                message = socket.next() => message,
                _ = stop.changed() => return Ok(()),
            };

            match message {
                None => return Err("event WebSocket ended".to_string()),
                Some(Err(err)) => return Err(format!("event WebSocket failed: {err}")),
                Some(Ok(Message::Text(text))) => dispatch(&text, sink).await?,
                Some(Ok(Message::Binary(bytes))) => match String::from_utf8(bytes) {
                    Ok(text) => dispatch(&text, sink).await?,
                    Err(err) => {
                        tracing::debug!(error = %err, "Ignoring non-UTF-8 event frame");
                    }
                },
                Some(Ok(Message::Close(frame))) => {
                    return Err(match frame {
                        Some(frame) => format!(
                            "event WebSocket closed by the protocol implementation: {} {}",
                            frame.code, frame.reason
                        ),
                        None => "event WebSocket closed by the protocol implementation".to_string(),
                    });
                }
                // Ping/Pong frames are answered by the WebSocket library itself; a raw `Frame`
                // carries no protocol-level payload the adapter could use.
                Some(Ok(_)) => {}
            }
        }
    }
}

/// Forwards one stream event, reporting a closed channel as an error string.
async fn send(sink: &mpsc::Sender<StreamEvent>, event: StreamEvent) -> Result<(), String> {
    sink.send(event)
        .await
        .map_err(|_| "the adapter dropped its event channel".to_string())
}

/// Decodes one JSON frame from either transport.
///
/// An undecodable frame is logged and skipped rather than failing the stream: a Milky
/// implementation newer than this adapter may push an event type this release does not know, and
/// tearing down a working connection over it would turn a future-proofing gap into an outage.
async fn dispatch(payload: &str, sink: &mpsc::Sender<StreamEvent>) -> Result<(), String> {
    match serde_json::from_str::<Event>(payload) {
        Ok(event) => send(sink, StreamEvent::Event(Box::new(event))).await,
        Err(err) => {
            tracing::debug!(error = %err, "Ignoring undecodable Milky event frame");
            Ok(())
        }
    }
}
