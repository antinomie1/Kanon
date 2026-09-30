//! QQ WebSocket gateway session: identify or resume, heartbeat, and event dispatch.
//!
//! One task owns the socket for the lifetime of a configuration. It reconnects with backoff after
//! any failure and resumes the previous session when QQ allows it, so events sent while the
//! socket was down are replayed instead of lost. Only an intents rejection stops it: retrying
//! cannot fix missing bot permissions, and the operator has to act.

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kanon_core::{EventIngress, IngestError};
use kanon_proto::v1::IngestEventRequest;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use crate::api::Api;
use crate::mapping::{self, QuoteStore};
use crate::{ConnectionState, Shared};

/// Group and C2C messages (`1 << 25`), public guild @-messages (`1 << 30`) and guild direct
/// messages (`1 << 12`): every conversation the adapter can answer, and all of them are
/// available to an ordinary bot without private-domain permissions.
pub const INTENTS: u64 = (1 << 25) | (1 << 30) | (1 << 12);

/// Delays between reconnect attempts; the last one repeats.
const BACKOFF: [u64; 6] = [1, 2, 5, 10, 30, 60];

/// Why a session ended.
enum End {
    /// The socket failed or was closed; try again.
    Retry(String),
    /// QQ refused the configuration itself; retrying would fail the same way.
    Fatal(String),
}

/// Resume state carried across reconnects.
#[derive(Default)]
struct Session {
    id: Option<String>,
    seq: Option<u64>,
}

/// Runs the gateway until the task is aborted or QQ rejects the bot's intents.
pub(crate) async fn run(
    state: Arc<RwLock<Shared>>,
    api: Arc<Api>,
    ingress: EventIngress,
    quotes: Arc<Mutex<QuoteStore>>,
) {
    let mut session = Session::default();
    let mut attempt = 0;
    loop {
        set_state(&state, ConnectionState::Connecting, None);
        let mut established = false;
        let end = connect(
            &state,
            &api,
            &ingress,
            &quotes,
            &mut session,
            &mut established,
        )
        .await;
        if established {
            attempt = 0;
        }
        match end {
            End::Fatal(reason) => {
                tracing::error!(reason = %reason, "QQ Official gateway stopped");
                set_state(&state, ConnectionState::Disconnected, Some(reason));
                return;
            }
            End::Retry(reason) => {
                let delay = BACKOFF[attempt.min(BACKOFF.len() - 1)];
                attempt += 1;
                tracing::warn!(reason = %reason, retry_in_secs = delay, "QQ Official gateway disconnected");
                set_state(&state, ConnectionState::Disconnected, Some(reason));
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
        }
    }
}

/// One socket lifetime: connect, identify or resume, then serve until something fails.
async fn connect(
    state: &Arc<RwLock<Shared>>,
    api: &Api,
    ingress: &EventIngress,
    quotes: &Mutex<QuoteStore>,
    session: &mut Session,
    established: &mut bool,
) -> End {
    let token = match api.token().await {
        Ok(token) => token,
        Err(err) => return End::Retry(err),
    };
    let url = match api.gateway_url().await {
        Ok(url) => url,
        Err(err) => return End::Retry(err),
    };
    let (socket, _) = match tokio_tungstenite::connect_async(url.as_str()).await {
        Ok(socket) => socket,
        Err(err) => return End::Retry(format!("cannot connect to the QQ gateway: {err}")),
    };
    let (mut sink, mut stream) = socket.split();

    // The gateway speaks first: HELLO carries the heartbeat interval.
    let interval = loop {
        match stream.next().await {
            Some(Ok(frame)) => match payload(frame) {
                Some(hello) if hello["op"] == 10 => {
                    break hello["d"]["heartbeat_interval"].as_u64().unwrap_or(41_250);
                }
                Some(_) | None => continue,
            },
            Some(Err(err)) => return End::Retry(format!("QQ gateway failed before HELLO: {err}")),
            None => return End::Retry("QQ gateway closed before HELLO".into()),
        }
    };

    let login = match (&session.id, session.seq) {
        (Some(id), Some(seq)) => json!({
            "op": 6,
            "d": {"token": format!("QQBot {token}"), "session_id": id, "seq": seq},
        }),
        _ => json!({
            "op": 2,
            "d": {"token": format!("QQBot {token}"), "intents": INTENTS, "shard": [0, 1], "properties": {}},
        }),
    };
    if let Err(err) = sink.send(Message::text(login.to_string())).await {
        return End::Retry(format!("cannot identify on the QQ gateway: {err}"));
    }

    let mut heartbeat = tokio::time::interval(Duration::from_millis(interval.max(1_000)));
    heartbeat.tick().await;
    // A heartbeat that is still unanswered when the next one is due means the connection is dead
    // even though TCP has not noticed; waiting for the OS would stall the bot for minutes.
    let mut acked = true;

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if !acked {
                    return End::Retry("QQ gateway stopped acknowledging heartbeats".into());
                }
                acked = false;
                let beat = json!({"op": 1, "d": session.seq});
                if let Err(err) = sink.send(Message::text(beat.to_string())).await {
                    return End::Retry(format!("heartbeat failed: {err}"));
                }
            }
            frame = stream.next() => {
                let frame = match frame {
                    Some(Ok(frame)) => frame,
                    Some(Err(err)) => return End::Retry(format!("QQ gateway connection failed: {err}")),
                    None => return End::Retry("QQ gateway closed the connection".into()),
                };
                if let Message::Close(close) = &frame {
                    let (code, reason) = close
                        .as_ref()
                        .map(|close| (u16::from(close.code), close.reason.to_string()))
                        .unwrap_or((1000, String::new()));
                    return closed(api, session, code, &reason).await;
                }
                let Some(payload) = payload(frame) else { continue };
                if let Some(seq) = payload["s"].as_u64() {
                    session.seq = Some(seq);
                }
                match payload["op"].as_u64() {
                    Some(0) => {
                        let kind = payload["t"].as_str().unwrap_or_default();
                        match kind {
                            "READY" => {
                                session.id = payload["d"]["session_id"].as_str().map(str::to_owned);
                                let user = &payload["d"]["user"];
                                let mut shared = state.write().expect("QQ Official state poisoned");
                                shared.bot_id = user["id"].as_str().unwrap_or_default().to_owned();
                                shared.status.bot_name = user["username"].as_str().map(str::to_owned);
                                drop(shared);
                                *established = true;
                                set_state(state, ConnectionState::Connected, None);
                                tracing::info!(bot = %user["username"], "QQ Official gateway ready");
                            }
                            "RESUMED" => {
                                *established = true;
                                set_state(state, ConnectionState::Connected, None);
                                tracing::info!("QQ Official gateway session resumed");
                            }
                            _ => dispatch(
                                state,
                                ingress,
                                quotes,
                                kind,
                                payload["id"].as_str().unwrap_or_default(),
                                &payload["d"],
                            ),
                        }
                    }
                    Some(1) => {
                        let beat = json!({"op": 1, "d": session.seq});
                        if let Err(err) = sink.send(Message::text(beat.to_string())).await {
                            return End::Retry(format!("heartbeat failed: {err}"));
                        }
                    }
                    Some(7) => return End::Retry("QQ gateway asked for a reconnect".into()),
                    Some(9) => {
                        // Invalid session: resuming is impossible, the next attempt identifies anew.
                        *session = Session::default();
                        return End::Retry("QQ gateway invalidated the session".into());
                    }
                    Some(11) => acked = true,
                    _ => {}
                }
            }
        }
    }
}

/// Decides what a close frame means for the next attempt.
async fn closed(api: &Api, session: &mut Session, code: u16, reason: &str) -> End {
    let detail = format!("QQ gateway closed the connection ({code} {reason})");
    match code {
        // Intents the bot may not use: only a permission change on the QQ side can fix this.
        4914 | 4915 => End::Fatal(format!(
            "{detail}: the bot lacks permission for group/C2C or guild messages"
        )),
        4004 => {
            api.invalidate_token().await;
            End::Retry(detail)
        }
        // Session invalid, sequence out of range or session timed out: identify anew.
        4006 | 4007 | 4009 => {
            *session = Session::default();
            End::Retry(detail)
        }
        _ => End::Retry(detail),
    }
}

/// Maps and enqueues one dispatch without awaiting the pipeline.
fn dispatch(
    state: &RwLock<Shared>,
    ingress: &EventIngress,
    quotes: &Mutex<QuoteStore>,
    kind: &str,
    gateway_id: &str,
    data: &Value,
) {
    let bot_id = state
        .read()
        .expect("QQ Official state poisoned")
        .bot_id
        .clone();
    let mapped = match mapping::map_notice(kind, gateway_id, data) {
        Some(notice) => notice.map(Some),
        None => {
            let mut quotes = quotes.lock().expect("QQ Official quote store poisoned");
            mapping::map_event(kind, data, &bot_id, &mut quotes)
        }
    };
    let event = match mapped {
        Ok(Some(event)) => event,
        Ok(None) => return,
        Err(err) => {
            tracing::warn!(error = %err, "Dropped an unreadable QQ Official event");
            return;
        }
    };
    let event_id = event.event_id.clone();
    // Fast-ACK: the socket loop never waits for the pipeline, so heartbeats keep flowing even
    // while the model is busy. A full queue sheds this one message and says so.
    match ingress.try_ingest(IngestEventRequest {
        platform: event.platform.clone(),
        event: Some(event),
    }) {
        Ok(()) => {}
        Err(IngestError::QueueFull) => {
            tracing::warn!(event_id = %event_id, "Core ingest queue is full; QQ Official message dropped")
        }
        Err(IngestError::Closed) => {
            tracing::warn!(event_id = %event_id, "Core ingest queue is closed; QQ Official message dropped")
        }
    }
}

/// Decodes a text (or UTF-8 binary) frame as a gateway payload.
fn payload(frame: Message) -> Option<Value> {
    let text = match frame {
        Message::Text(text) => text.to_string(),
        Message::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok()?,
        _ => return None,
    };
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(err) => {
            tracing::warn!(error = %err, "Ignored a non-JSON QQ gateway frame");
            None
        }
    }
}

/// Publishes the connection state; `error` replaces the last error only when given.
fn set_state(state: &RwLock<Shared>, next: ConnectionState, error: Option<String>) {
    let mut shared = state.write().expect("QQ Official state poisoned");
    shared.status.connection_state = next;
    shared.status.connected = next == ConnectionState::Connected;
    if next == ConnectionState::Connected {
        shared.status.last_error = None;
    }
    if error.is_some() {
        shared.status.last_error = error;
    }
}
