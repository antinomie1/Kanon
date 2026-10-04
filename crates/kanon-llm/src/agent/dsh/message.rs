//! Converts the current user message without importing builtin history or prompt settings.

use crate::{AgentError, ChatMessage, ContentPart, Role};
use serde_json::{Value, json};

/// Converts current input before remote session creation can have any side effect.
/// Images must already be inlined; DSH must never read a Kanon-local path.
pub fn message_content(message: ChatMessage) -> Result<Vec<Value>, AgentError> {
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
    if content.is_empty() {
        return Err(AgentError::InvalidRequest(
            "DSH prompt must contain content".into(),
        ));
    }
    Ok(content)
}
