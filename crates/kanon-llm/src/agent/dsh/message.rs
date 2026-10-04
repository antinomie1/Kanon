//! Converts the current user message without importing builtin history or prompt settings.

use super::{DshClient, DshError};
use crate::{AgentError, ChatMessage, ContentPart, Role, StopSignal, ToolRouterOutput};
use serde_json::json;

/// Runs a current user message under DSH's own model, tools and session journal.
/// Images must already be inlined by the caller; DSH must never read a Kanon-local path.
pub async fn run_message(
    client: &DshClient,
    session_id: &str,
    request_id: &str,
    message: ChatMessage,
    stop: &StopSignal,
    ephemeral: bool,
) -> Result<ToolRouterOutput, AgentError> {
    if message.role != Role::User || message.tool_calls.is_some() {
        return Err(AgentError::InvalidRequest(
            "DSH accepts only current user input".into(),
        ));
    }
    let mut content = Vec::new();
    if let Some(text) = message.content.filter(|text| !text.is_empty()) {
        content.push(json!({"type": "text", "text": text}));
    }
    for part in message.parts.into_iter().flatten() {
        // ChatMessage.content is the complete text projection, including fetch failures.
        // Text parts are intentionally not copied again.
        if let ContentPart::Image { url, .. } = part {
            let url =
                url.ok_or_else(|| AgentError::InvalidRequest("DSH images must be inlined".into()))?;
            let (mime, data) = url
                .strip_prefix("data:")
                .and_then(|value| value.split_once(";base64,"))
                .filter(|(mime, data)| mime.starts_with("image/") && !data.is_empty())
                .ok_or_else(|| {
                    AgentError::InvalidRequest("DSH images require an image data URI".into())
                })?;
            content.push(json!({"type": "image", "mediaType": mime, "data": data}));
        }
    }
    if stop.is_stopped() {
        return Err(AgentError::Stopped);
    }
    let output = client
        .run_scoped_turn(session_id, request_id, content, stop.stopped(), ephemeral)
        .await
        .map_err(|error| match error {
            DshError::Stopped => AgentError::Stopped,
            other => AgentError::Dsh(other),
        })?;
    Ok(ToolRouterOutput {
        content: output.content,
        reasoning: None,
        executed_tools: Vec::new(),
        attachments: Vec::new(),
    })
}
