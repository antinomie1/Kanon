//! DSH-owned session operations and request-correlated turn consumption.

use super::{DshClient, DshError, DshStream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::future::Future;
use std::time::Duration;

// DSH sequence positions are JavaScript safe integers, with -1 for an empty journal.
const MAX_SESSION_SEQ: i64 = 9_007_199_254_740_991;

/// A DSH catalog row. Projections are owned and versioned by DSH, not copied into SessionStore.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshSession {
    /// Remote durable session identifier.
    pub session_id: String,
    /// Timestamp in milliseconds, as returned by DSH.
    pub updated_at: u64,
    /// Whether DSH currently runs this session's agent.
    pub running: bool,
    /// Whether the session has no user-visible messages yet.
    pub blank: bool,
    /// Native metadata such as title, model selection and token usage.
    pub projections: Option<Value>,
}

/// One message-aligned window from DSH's authoritative journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshSnapshot {
    /// Durable remote header, including identity and agent preset.
    pub header: Value,
    /// Inclusive event position, or -1 when no event has been appended yet.
    pub cursor: i64,
    /// Ordered journal entries; these are never written into Kanon's builtin memory.
    pub records: Vec<Value>,
    /// More history exists before this window.
    pub has_more: bool,
    /// DSH's complete folded state at this cursor.
    pub projections: Value,
}

/// Result of one exact admitted prompt, excluding earlier turns from a follow snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct DshTurnOutput {
    /// Complete text from durable assistant messages, without duplicate stream chunks.
    pub content: String,
    /// DSH's turn number, correlated through the accepted user message.
    pub turn: u64,
    /// Last durable sequence observed for this completed turn.
    pub through_seq: u64,
}

impl DshClient {
    /// Lists native DSH sessions without creating local metadata or activating their agents.
    pub async fn sessions(&self) -> Result<Vec<DshSession>, DshError> {
        #[derive(Deserialize)]
        struct Catalog {
            items: Vec<DshSession>,
        }
        let value: Catalog = self.call("session/list", json!({"_request": {}})).await?;
        Ok(value.items)
    }

    /// Creates or adopts an explicitly named DSH session; no model or history is supplied.
    pub async fn create_session(
        &self,
        session_id: &str,
        preset: Option<&str>,
    ) -> Result<String, DshError> {
        validate_id(session_id)?;
        let mut request = json!({"sessionId": session_id});
        if let Some(preset) = preset {
            if preset.trim().is_empty() {
                return Err(DshError::Config("agent preset must not be empty".into()));
            }
            request["agentPreset"] = preset.into();
        }
        let value: Value = self
            .call("session/create", json!({"request": request}))
            .await?;
        if value["sessionId"] != session_id {
            return Err(DshError::Protocol(
                "created session has a different identity".into(),
            ));
        }
        Ok(session_id.into())
    }

    /// Opens an ordered journal stream and consumes its required opening snapshot.
    pub async fn follow(&self, session_id: &str) -> Result<(DshSnapshot, DshStream), DshError> {
        validate_id(session_id)?;
        let mut stream = self
            .open_stream(
                "session/follow",
                json!({"request": {
                    "address": {"kind": "session", "sessionId": session_id}, "maxMessages": 100
                }}),
            )
            .await?;
        let value = tokio::time::timeout(self.config.request_timeout(), stream.next())
            .await
            .map_err(|_| DshError::Timeout("session snapshot"))??;
        if value["type"] != "snapshot" || value["header"]["id"] != session_id {
            return Err(DshError::Protocol(
                "session follow did not open with its own snapshot".into(),
            ));
        }
        let snapshot: DshSnapshot =
            serde_json::from_value(value).map_err(|e| DshError::Protocol(e.to_string()))?;
        if !(-1..=MAX_SESSION_SEQ).contains(&snapshot.cursor)
            || (snapshot.cursor == -1 && (!snapshot.records.is_empty() || snapshot.has_more))
        {
            return Err(DshError::Protocol("invalid session snapshot cursor".into()));
        }
        Ok((snapshot, stream))
    }

    /// Reads a recent history window without retaining a subscriber or local history cache.
    pub async fn snapshot(&self, session_id: &str) -> Result<DshSnapshot, DshError> {
        Ok(self.follow(session_id).await?.0)
    }

    /// Reads an earlier page using the cut from the same snapshot, so concurrent writes cannot
    /// splice two different histories together while the operator pages backwards.
    pub async fn page(
        &self,
        session_id: &str,
        through_seq: i64,
        before_seq: u64,
    ) -> Result<Value, DshError> {
        validate_id(session_id)?;
        if !(-1..=MAX_SESSION_SEQ).contains(&through_seq) || before_seq > MAX_SESSION_SEQ as u64 {
            return Err(DshError::Config("invalid session page cursor".into()));
        }
        self.call(
            "session/page",
            json!({"request": {
                "address": {"kind": "session", "sessionId": session_id},
                "throughSeq": through_seq, "beforeSeq": before_seq, "maxMessages": 100
            }}),
        )
        .await
    }

    /// Selects the native DSH provider/model for one session; DSH validates and persists it.
    pub async fn select_model(
        &self,
        session_id: &str,
        provider: &str,
        model: &str,
        effort: Option<&str>,
    ) -> Result<Value, DshError> {
        validate_id(session_id)?;
        let _writing = self.writers.claim(session_id)?;
        if provider.trim().is_empty() || model.trim().is_empty() {
            return Err(DshError::Config(
                "DSH model selection requires provider and model".into(),
            ));
        }
        let mut request = json!({"sessionId": session_id, "provider": provider, "model": model});
        if let Some(effort) = effort {
            request["reasoningEffort"] = effort.into();
        }
        self.call("session/selectModel", json!({"request": request}))
            .await
    }

    /// Updates only the native remote title.
    pub async fn rename_session(&self, session_id: &str, title: &str) -> Result<Value, DshError> {
        validate_id(session_id)?;
        self.call(
            "session/rename",
            json!({"request": {"sessionId": session_id, "title": title}}),
        )
        .await
    }

    /// Stops active work and removes still-pending prompts before retiring a remote session.
    /// DSH cancellation alone retains the inbox, so omitting queue removal could restart work.
    pub async fn stop_session(&self, session_id: &str) -> Result<(), DshError> {
        validate_id(session_id)?;
        let projections: Value = self
            .call(
                "session/projections",
                json!({"request": {"sessionId": session_id}}),
            )
            .await?;
        if !projections.is_null() {
            let values = projections
                .get("values")
                .and_then(Value::as_object)
                .ok_or_else(|| DshError::Protocol("session projections have no values".into()))?;
            if let Some(inbox) = values.get("inbox") {
                for lane in ["next-turn", "next-step"] {
                    let items = inbox[lane]
                        .as_array()
                        .ok_or_else(|| DshError::Protocol("invalid DSH inbox lane".into()))?;
                    for item in items {
                        let id =
                            item["id"]
                                .as_str()
                                .filter(|id| !id.is_empty())
                                .ok_or_else(|| {
                                    DshError::Protocol("pending DSH message has no id".into())
                                })?;
                        let result = self.call::<Value>("session/updateQueue", json!({"request": {
                            "sessionId": session_id, "itemId": id, "action": {"kind": "remove"}
                        }})).await;
                        // The agent may claim a listed message before removal; cancel below also
                        // reaches that active turn. Other failures must not masquerade as stopped.
                        match result {
                            Ok(value) => require_accepted(&value)?,
                            Err(DshError::Remote { code, .. })
                                if code == "session/queue-item-not-found" => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
            }
        }
        let value: Value = self
            .call(
                "session/cancel",
                json!({"request": {"sessionId": session_id}}),
            )
            .await?;
        require_accepted(&value)
    }

    /// Retires a session using DSH's native archive operation; the remote journal remains restorable.
    pub async fn archive_session(&self, session_id: &str) -> Result<Value, DshError> {
        let _writing = self.writers.claim(session_id)?;
        self.archive_owned_session(session_id).await
    }

    async fn archive_owned_session(&self, session_id: &str) -> Result<Value, DshError> {
        self.stop_session(session_id).await?;
        self.call(
            "workspace/archiveSession",
            json!({"request": {"sessionId": session_id, "stopActivity": true}}),
        )
        .await
    }

    /// Runs one prompt while retaining the remote subscription for its full lifetime.
    ///
    /// The initial snapshot is consumed before submission, and a reply is accepted only after
    /// the exact request id appears in a durable user message. Earlier turns cannot be replayed
    /// as new platform replies. Every failure stays a DSH failure, without a builtin fallback.
    pub async fn run_turn(
        &self,
        session_id: &str,
        request_id: &str,
        content: Vec<Value>,
        stop: impl Future<Output = ()> + Send,
    ) -> Result<DshTurnOutput, DshError> {
        self.run_scoped_turn(session_id, request_id, content, stop, false)
            .await
    }

    /// Runs an admitted turn and optionally archives its private journal before releasing it.
    /// Retirement belongs to the bounded owner, so a disconnected caller cannot skip cleanup.
    pub async fn run_scoped_turn(
        &self,
        session_id: &str,
        request_id: &str,
        content: Vec<Value>,
        stop: impl Future<Output = ()> + Send,
        ephemeral: bool,
    ) -> Result<DshTurnOutput, DshError> {
        validate_id(session_id)?;
        validate_id(request_id)?;
        if content.is_empty() {
            return Err(DshError::Config("DSH prompt must contain content".into()));
        }
        let mut stop = std::pin::pin!(stop);
        tokio::select! {
            biased;
            () = &mut stop => return Err(DshError::Stopped),
            () = std::future::ready(()) => {},
        }
        let writing = self.writers.claim(session_id)?;
        let client = self.clone();
        let session_id = session_id.to_string();
        let request_id = request_id.to_string();
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let mut cancel_on_drop = CancelOnDrop(Some(cancel_tx));
        // The bounded owner survives a disconnected caller long enough to stop remote work.
        // Its reservation remains held through cancellation, so new prompts cannot race cleanup.
        let mut task = tokio::spawn(async move {
            let _writing = writing;
            let result = client
                .run_owned_turn(&session_id, &request_id, content, async {
                    let _ = cancel_rx.await;
                })
                .await;
            if ephemeral {
                let retirement = tokio::time::timeout(
                    client.config.request_timeout(),
                    client.archive_owned_session(&session_id),
                )
                .await
                .unwrap_or(Err(DshError::Timeout("private session archive")));
                if let Err(error) = retirement {
                    tracing::warn!(session_id, error = %error, "Private DSH session archive failed");
                    return Err(DshError::Transport(format!(
                        "private DSH session '{session_id}' could not be archived: {error}; turn result: {}",
                        result
                            .as_ref()
                            .err()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "completed".into()),
                    )));
                }
            }
            // The owner may outlive its caller. Cleanup failures must remain observable even
            // when no HTTP response or plugin RPC remains to receive this result.
            if let Err(error) = &result
                && !matches!(error, DshError::Stopped)
            {
                tracing::warn!(session_id, error = %error, "DSH turn owner failed");
            }
            result
        });
        tokio::select! {
            biased;
            () = stop => { cancel_on_drop.stop(); }
            result = &mut task => return result.map_err(task_error)?,
        }
        task.await.map_err(task_error)?
    }

    async fn run_owned_turn(
        &self,
        session_id: &str,
        request_id: &str,
        content: Vec<Value>,
        stop: impl Future<Output = ()>,
    ) -> Result<DshTurnOutput, DshError> {
        let mut submitted = false;
        let result = tokio::select! {
            biased;
            () = stop => Err(DshError::Stopped),
            result = tokio::time::timeout(Duration::from_secs(self.config.turn_timeout_seconds),
                self.consume_turn(session_id, request_id, content, &mut submitted)) =>
                result.map_err(|_| DshError::Timeout("turn")).and_then(|value| value),
        };
        // A lost prompt response is an ambiguous remote commit, never a reason to retry.
        if submitted && result.is_err() {
            tokio::time::timeout(self.config.request_timeout(), self.stop_session(session_id))
                .await
                .unwrap_or(Err(DshError::Timeout("remote cancellation")))
                .map_err(|cleanup| {
                    DshError::Transport(format!(
                        "{}; remote cancellation failed: {cleanup}",
                        result.as_ref().unwrap_err()
                    ))
                })?;
        }
        result
    }

    async fn consume_turn(
        &self,
        session_id: &str,
        request_id: &str,
        content: Vec<Value>,
        submitted: &mut bool,
    ) -> Result<DshTurnOutput, DshError> {
        let (snapshot, mut stream) = self.follow(session_id).await?;
        if snapshot.records.iter().any(|record| {
            record["event"]["type"] == "user/message"
                && record["event"]["data"]["source"]["rpcId"] == request_id
        }) {
            // Reopening a finished request must not replay its answer onto an IM platform.
            return Err(DshError::Remote {
                code: "kanon/request-already-admitted".into(),
                message: "this request already exists in DSH; inspect its session before retrying"
                    .into(),
            });
        }
        // Once the HTTP operation starts, a transport error is an ambiguous remote commit.
        *submitted = true;
        let value: Value = self.call("session/prompt", json!({"request": {
            "sessionId": session_id, "requestId": request_id, "mode": "queue", "content": content
        }})).await?;
        require_accepted(&value)?;
        let mut cursor = snapshot.cursor;
        let mut current_turn = None;
        let mut target_turn = None;
        let mut content = String::new();
        loop {
            let frame = stream.next().await?;
            if frame["type"] != "event" {
                return Err(DshError::Protocol(
                    "unexpected session journal frame".into(),
                ));
            }
            let event = &frame["event"];
            let seq = event["seq"]
                .as_i64()
                .filter(|seq| (0..=MAX_SESSION_SEQ).contains(seq))
                .ok_or_else(|| DshError::Protocol("session event has no sequence".into()))?;
            if seq
                != cursor
                    .checked_add(1)
                    .ok_or_else(|| DshError::Protocol("session sequence overflow".into()))?
            {
                return Err(DshError::Protocol(
                    "session journal is not contiguous".into(),
                ));
            }
            cursor = seq;
            let data = &event["data"];
            match event["type"].as_str() {
                Some("turn/start") => {
                    current_turn = Some(turn_number(data)?);
                }
                Some("user/message") if data["source"]["rpcId"] == request_id => {
                    target_turn = Some(current_turn.ok_or_else(|| {
                        DshError::Protocol("prompt entered without a turn".into())
                    })?);
                }
                Some("assistant/message") if target_turn.is_some() => {
                    if Some(turn_number(data)?) == target_turn {
                        let blocks = data["message"]["content"].as_array().ok_or_else(|| {
                            DshError::Protocol("assistant message has no content".into())
                        })?;
                        for block in blocks {
                            if block["type"] == "text" {
                                let text = block["text"].as_str().ok_or_else(|| {
                                    DshError::Protocol("invalid assistant text block".into())
                                })?;
                                if content.len().saturating_add(text.len()) > super::MAX_WIRE_BYTES
                                {
                                    return Err(DshError::Protocol(
                                        "turn output exceeds size limit".into(),
                                    ));
                                }
                                content.push_str(text);
                            }
                        }
                    }
                }
                Some("turn/end") if target_turn == Some(turn_number(data)?) => {
                    let reason = data["reason"]["kind"]
                        .as_str()
                        .ok_or_else(|| DshError::Protocol("turn end has no reason".into()))?;
                    if reason != "completed" {
                        return Err(DshError::TurnFailed(reason.into()));
                    }
                    return Ok(DshTurnOutput {
                        content,
                        turn: target_turn.expect("matched turn"),
                        through_seq: cursor as u64,
                    });
                }
                Some(_) => {}
                None => return Err(DshError::Protocol("journal event has no type".into())),
            }
        }
    }
}

fn validate_id(id: &str) -> Result<(), DshError> {
    if id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control) {
        return Err(DshError::Config(
            "remote identity must be 1..=1024 bytes without control characters".into(),
        ));
    }
    Ok(())
}

fn require_accepted(value: &Value) -> Result<(), DshError> {
    if value["accepted"] != true {
        return Err(DshError::Protocol(
            "remote command did not acknowledge acceptance".into(),
        ));
    }
    Ok(())
}

fn turn_number(value: &Value) -> Result<u64, DshError> {
    value["turn"]
        .as_u64()
        .ok_or_else(|| DshError::Protocol("event has no turn number".into()))
}

/// Dropping a caller must cancel its remote prompt, not only its local response future.
struct CancelOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

impl CancelOnDrop {
    fn stop(&mut self) {
        if let Some(sender) = self.0.take() {
            // A closed receiver means the bounded owner already finished.
            let _ = sender.send(());
        }
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.stop();
    }
}

fn task_error(error: tokio::task::JoinError) -> DshError {
    DshError::Transport(format!("DSH turn task failed: {error}"))
}
