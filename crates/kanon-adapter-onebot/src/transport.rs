//! Universal WebSocket transport. Each session exclusively owns its echo correlation map.

use crate::{ConnectionState, OneBotClient, OneBotConfig, OneBotError, State, mapping};
use futures_util::{SinkExt, StreamExt};
use kanon_core::EventIngress;
use kanon_proto::v1::{IngestEventRequest, PipelineEventRequest};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinSet,
    time::{Instant, timeout, timeout_at},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        handshake::server::{Request, Response},
        http::{HeaderValue, StatusCode, header::AUTHORIZATION},
    },
};

/// Bounds handshakes, API calls and stalled socket writes.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// One API request. Dropping the response sender explicitly fails its waiting delivery.
pub(crate) struct Command {
    pub action: String,
    pub params: Value,
    pub reply: oneshot::Sender<Result<Value, OneBotError>>,
    pub deadline: Instant,
}

/// Runs until stopped by the adapter's lifecycle owner; never spawns detached session tasks.
pub(crate) async fn run(
    state: Arc<RwLock<State>>,
    config: OneBotConfig,
    ingress: EventIngress,
    listener: Option<Arc<TcpListener>>,
) {
    if let Some(listener) = listener {
        run_reverse(&state, &config, &ingress, listener).await;
    } else {
        let mut delay = Duration::from_millis(500);
        loop {
            set_state(&state, ConnectionState::Connecting, None);
            let started = Instant::now();
            let outcome = async {
                let mut request = config
                    .ws_url
                    .clone()
                    .into_client_request()
                    .map_err(|_| "invalid WebSocket request".to_owned())?;
                if let Some(token) = &config.access_token {
                    request.headers_mut().insert(
                        AUTHORIZATION,
                        HeaderValue::from_str(&format!("Bearer {token}")).expect("validated token"),
                    );
                }
                let (socket, _) =
                    timeout(REQUEST_TIMEOUT, tokio_tungstenite::connect_async(request))
                        .await
                        .map_err(|_| "WebSocket handshake timed out".to_owned())?
                        .map_err(|err| safe_ws_error(&err))?;
                session(socket, &state, &config, &ingress, None, None).await
            }
            .await;
            set_state(&state, ConnectionState::Disconnected, outcome.err());
            if started.elapsed() >= Duration::from_secs(30) {
                delay = Duration::from_millis(500);
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(30));
        }
    }
}

/// Authenticates a single-account universal connection before any events reach the pipeline.
async fn run_reverse(
    state: &Arc<RwLock<State>>,
    config: &OneBotConfig,
    ingress: &EventIngress,
    listener: Arc<TcpListener>,
) {
    let path = url::Url::parse(&config.ws_url)
        .expect("validated URL")
        .path()
        .to_owned();
    loop {
        set_state(state, ConnectionState::Listening, None);
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                set_state(
                    state,
                    ConnectionState::Disconnected,
                    Some(format!("reverse accept failed: {err}")),
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        let mut self_id = None;
        let handshake =
            tokio_tungstenite::accept_hdr_async(stream, |request: &Request, response: Response| {
                let reject = |status: StatusCode, reason: &str| {
                    let mut response = tokio_tungstenite::tungstenite::http::Response::new(Some(
                        reason.to_owned(),
                    ));
                    *response.status_mut() = status;
                    Err(response)
                };
                if request.uri().path() != path || request.uri().query().is_some() {
                    return reject(StatusCode::NOT_FOUND, "unknown OneBot endpoint");
                }
                if let Some(token) = &config.access_token {
                    let expected = format!("Bearer {token}");
                    if request
                        .headers()
                        .get(AUTHORIZATION)
                        .and_then(|h| h.to_str().ok())
                        != Some(expected.as_str())
                    {
                        return reject(StatusCode::UNAUTHORIZED, "invalid OneBot credential");
                    }
                }
                if request
                    .headers()
                    .get("x-client-role")
                    .and_then(|h| h.to_str().ok())
                    != Some("Universal")
                {
                    return reject(StatusCode::BAD_REQUEST, "X-Client-Role must be Universal");
                }
                let id = request
                    .headers()
                    .get("x-self-id")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|id| *id > 0);
                let Some(id) = id else {
                    return reject(
                        StatusCode::BAD_REQUEST,
                        "X-Self-ID must be a positive account ID",
                    );
                };
                self_id = Some(id.to_string());
                Ok(response)
            });
        let outcome = match timeout(REQUEST_TIMEOUT, handshake).await {
            Ok(Ok(socket)) => {
                session(socket, state, config, ingress, self_id, Some(&listener)).await
            }
            Ok(Err(err)) => Err(safe_ws_error(&err)),
            Err(_) => Err("reverse WebSocket handshake timed out".into()),
        };
        set_state(state, ConnectionState::Listening, outcome.err());
    }
}

/// Drives reads, API requests, ping/pong and expiry together, without awaiting core capacity.
async fn session<S>(
    mut socket: WebSocketStream<S>,
    state: &Arc<RwLock<State>>,
    config: &OneBotConfig,
    ingress: &EventIngress,
    mut self_id: Option<String>,
    listener: Option<&TcpListener>,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (sender, mut commands) = mpsc::channel::<Command>(64);
    // Lookups (quotes, forwards, member names) and request answers call the API through this same
    // session, so they run beside the loop that answers them. The set is owned by the session: a
    // disconnect drops every pending lookup. Each task yields the event to ingest, if any.
    let api = sender.clone();
    let mut lookups: JoinSet<Option<PipelineEventRequest>> = JoinSet::new();
    {
        let mut state = state.write().expect("OneBot state poisoned");
        state.sender = Some(sender);
        state.status.connected = true;
        state.status.connection_state = ConnectionState::Connected;
        state.status.self_id = self_id.clone();
        state.status.last_error = None;
    }
    let mut pending: HashMap<String, Command> = HashMap::new();
    let mut sequence = 0u64;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ping_at = Instant::now() + Duration::from_secs(20);
    let mut pong_deadline = None;
    loop {
        tokio::select! {
            frame = socket.next() => {
                let frame = frame.ok_or("WebSocket closed")?.map_err(|e| safe_ws_error(&e))?;
                let value: Value = match frame {
                    Message::Text(text) => serde_json::from_str(&text).map_err(|_| "invalid OneBot JSON")?,
                    Message::Binary(bytes) => serde_json::from_slice(&bytes).map_err(|_| "invalid OneBot JSON")?,
                    Message::Close(_) => return Err("WebSocket closed".into()),
                    Message::Pong(_) => { pong_deadline = None; continue; }
                    Message::Ping(_) => {
                        timeout(REQUEST_TIMEOUT, socket.flush()).await.map_err(|_| "WebSocket pong write timed out")?.map_err(|e| safe_ws_error(&e))?;
                        continue;
                    }
                    _ => continue,
                };
                if value.get("post_type").is_some() {
                    if let Some(id) = value.get("self_id") {
                        let id = mapping::message_id(id)?;
                        if self_id.as_ref().is_some_and(|expected| expected != &id) {
                            return Err("event self_id changed within one OneBot connection".into());
                        }
                        self_id = Some(id.clone());
                        state.write().expect("OneBot state poisoned").status.self_id = Some(id);
                    }
                    match value["post_type"].as_str() {
                        Some("notice") => match mapping::map_notice(&config.platform, &value) {
                            Ok(Some(notice)) => {
                                lookups.spawn(name_notice(api.clone(), notice));
                            }
                            Ok(None) => {}
                            Err(err) => record_error(state, format!("OneBot notice mapping failed: {err}")),
                        },
                        Some("request") => {
                            if let Some(answer) = auto_accept(config, &value) {
                                lookups.spawn(answer_request(api.clone(), answer));
                            }
                        }
                        _ => match mapping::map_event(&config.platform, value) {
                            // Plain messages go straight in, keeping their order; only a message
                            // that needs a lookup waits for it.
                            Ok(Some(event)) if mapping::needs_lookup(&event) => {
                                lookups.spawn(enrich(api.clone(), event));
                            }
                            Ok(Some(event)) => ingest(state, config, ingress, event),
                            Ok(None) => {}
                            Err(err) => record_error(state, format!("OneBot message mapping failed: {err}")),
                        },
                    }
                } else if let Some(echo) = value.get("echo").and_then(Value::as_str) {
                    if let Some(command) = pending.remove(echo) {
                        // The client interprets success/error envelopes and typed or void data.
                        let _ = command.reply.send(Ok(value));
                    }
                }
            }
            Some(joined) = lookups.join_next() => {
                if let Some(event) = joined.map_err(|_| "OneBot lookup task failed")? {
                    ingest(state, config, ingress, event);
                }
            }
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()); };
                if command.reply.is_closed() { continue; }
                if Instant::now() >= command.deadline {
                    let _ = command.reply.send(Err(OneBotError::Timeout { action: command.action }));
                    continue;
                }
                if pending.len() >= 64 {
                    let _ = command.reply.send(Err(OneBotError::Transport("too many pending OneBot API calls".into())));
                    continue;
                }
                sequence += 1;
                let echo = sequence.to_string();
                let payload = json!({ "action": command.action, "params": command.params, "echo": echo });
                timeout_at(command.deadline, socket.send(Message::Text(payload.to_string()))).await
                    .map_err(|_| "WebSocket API write timed out; delivery outcome is unknown")?
                    .map_err(|e| safe_ws_error(&e))?;
                pending.insert(echo, command);
            }
            _ = tick.tick() => {
                let now = Instant::now();
                let expired: Vec<_> = pending.iter()
                    .filter(|(_, command)| command.reply.is_closed() || command.deadline <= now)
                    .map(|(echo, _)| echo.clone()).collect();
                for echo in expired {
                    if let Some(command) = pending.remove(&echo) {
                        let _ = command.reply.send(Err(OneBotError::Timeout { action: command.action }));
                    }
                }
                if pong_deadline.is_some_and(|deadline| now >= deadline) { return Err("WebSocket pong timed out".into()); }
                if now >= ping_at && pong_deadline.is_none() {
                    timeout(REQUEST_TIMEOUT, socket.send(Message::Ping(Vec::new()))).await.map_err(|_| "WebSocket ping write timed out")?.map_err(|e| safe_ws_error(&e))?;
                    pong_deadline = Some(now + Duration::from_secs(20));
                    ping_at = now + Duration::from_secs(20);
                }
            }
            accepted = async { match listener { Some(listener) => listener.accept().await, None => std::future::pending().await } } => {
                // One platform owns one account. Reject extra clients rather than replacing a
                // working socket or allowing two bots to share ambiguous channel identifiers.
                if let Ok((mut stream, _)) = accepted {
                    let _ = timeout(Duration::from_millis(100), stream.write_all(b"HTTP/1.1 409 Conflict\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")).await;
                }
            }
        }
    }
}

/// Clears the command sender whenever no session owns it.
fn set_state(state: &Arc<RwLock<State>>, connection_state: ConnectionState, error: Option<String>) {
    let mut state = state.write().expect("OneBot state poisoned");
    state.sender = None;
    state.status.connected = false;
    state.status.self_id = None;
    state.status.connection_state = connection_state;
    if error.is_some() {
        state.status.last_error = error;
    }
}

/// Pushes an event without waiting for core capacity.
fn ingest(
    state: &Arc<RwLock<State>>,
    config: &OneBotConfig,
    ingress: &EventIngress,
    event: PipelineEventRequest,
) {
    if let Err(err) = ingress.try_ingest(IngestEventRequest {
        platform: config.platform.clone(),
        event: Some(event),
    }) {
        record_error(state, format!("OneBot message rejected by core: {err}"));
    }
}

fn record_error(state: &Arc<RwLock<State>>, error: String) {
    tracing::warn!(error = %error, "OneBot ingress failed");
    state
        .write()
        .expect("OneBot state poisoned")
        .status
        .last_error = Some(error);
}

/// HTTP handshake errors may contain headers; expose only their status code.
fn safe_ws_error(error: &tokio_tungstenite::tungstenite::Error) -> String {
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            format!("WebSocket handshake rejected ({})", response.status())
        }
        tokio_tungstenite::tungstenite::Error::Io(error) => {
            format!("WebSocket I/O error: {}", error.kind())
        }
        _ => "WebSocket protocol or TLS error".into(),
    }
}

/// Completes a message with what OneBot does not push: the quoted message, the content of merged
/// forwards and the names behind @-mentions.
///
/// Every lookup is best-effort. A recalled quote, an expired forward or a member who left must not
/// drop the message that referenced them, so failures are logged and the event goes on as is.
async fn enrich(
    api: mpsc::Sender<Command>,
    mut event: PipelineEventRequest,
) -> Option<PipelineEventRequest> {
    if let Some(message_id) = mapping::reply_target(&event) {
        let quoted = OneBotClient::call_on::<_, Value>(
            api.clone(),
            "get_msg",
            &json!({"message_id": message_id}),
        )
        .await;
        if let Err(err) = quoted
            .map_err(|e| e.to_string())
            .and_then(|data| mapping::attach_quote(&mut event, &data["message"]))
        {
            tracing::warn!(error = %err, "OneBot quoted message unavailable");
        }
    }
    for forward_id in mapping::forward_ids(&event) {
        // The standard names the parameter `id`; NapCat and LLOneBot read `message_id`.
        let params = json!({"id": forward_id, "message_id": forward_id});
        let fetched =
            OneBotClient::call_on::<_, Value>(api.clone(), "get_forward_msg", &params).await;
        if let Err(err) = fetched
            .map_err(|e| e.to_string())
            .and_then(|data| mapping::attach_forward(&mut event, &forward_id, &data))
        {
            tracing::warn!(error = %err, "OneBot merged forward unavailable");
        }
    }
    if let Some(group_id) = mapping::group_of(&event) {
        for user_id in mapping::unnamed_mentions(&event) {
            if let Some(name) = display_name(&api, Some(group_id), user_id).await {
                mapping::name_mention(&mut event, user_id, &name);
            }
        }
    }
    Some(event)
}

/// Puts a name on a notice's actor; the QQ number stands in when no name can be read.
async fn name_notice(
    api: mpsc::Sender<Command>,
    notice: mapping::Notice,
) -> Option<PipelineEventRequest> {
    let mut event = notice.event;
    if let Some(actor) = notice.actor_id {
        let name = display_name(&api, notice.group_id, actor)
            .await
            .unwrap_or_else(|| actor.to_string());
        mapping::set_notice_actor(&mut event, &name);
    }
    Some(event)
}

/// Group card, else nickname, of an account; `None` when the implementation cannot tell.
async fn display_name(
    api: &mpsc::Sender<Command>,
    group_id: Option<i64>,
    user_id: i64,
) -> Option<String> {
    let data = match group_id {
        Some(group_id) => {
            let params = json!({"group_id": group_id, "user_id": user_id, "no_cache": false});
            OneBotClient::call_on::<_, Value>(api.clone(), "get_group_member_info", &params).await
        }
        None => {
            let params = json!({"user_id": user_id, "no_cache": false});
            OneBotClient::call_on::<_, Value>(api.clone(), "get_stranger_info", &params).await
        }
    }
    .ok()?;
    ["card", "nickname"]
        .into_iter()
        .filter_map(|key| data[key].as_str())
        .map(str::trim)
        .find(|name| !name.is_empty())
        .map(str::to_owned)
}

/// The API call that accepts a request, when the configuration says to.
fn auto_accept(config: &OneBotConfig, value: &Value) -> Option<(&'static str, Value)> {
    match mapping::map_request(value)? {
        mapping::Request::Friend { flag } if config.auto_accept_friends => Some((
            "set_friend_add_request",
            json!({"flag": flag, "approve": true}),
        )),
        mapping::Request::GroupInvite { flag } if config.auto_accept_group_invites => Some((
            "set_group_add_request",
            json!({"flag": flag, "sub_type": "invite", "approve": true}),
        )),
        _ => None,
    }
}

/// Answers a request; nothing is ingested.
async fn answer_request(
    api: mpsc::Sender<Command>,
    (action, params): (&'static str, Value),
) -> Option<PipelineEventRequest> {
    match OneBotClient::request(api, action, &params).await {
        Ok(_) => tracing::info!(action, "OneBot request accepted automatically"),
        Err(err) => tracing::warn!(action, error = %err, "OneBot request could not be accepted"),
    }
    None
}
