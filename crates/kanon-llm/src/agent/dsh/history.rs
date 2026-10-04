//! Read-only text projection for Kanon's legacy conversation RPCs.
//!
//! This projection is never used as model context or persisted locally. The native agent API
//! exposes the original journal, including tool results and multimodal records, without loss.

use super::{DshClient, DshError, MAX_WIRE_BYTES};
use crate::ChatMessage;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    records: Vec<Value>,
    has_more: bool,
}

/// Reads the human user/assistant transcript at one immutable remote journal cut.
/// Context replacement copies belong to the model surface and never retire human history.
/// Large journals fail explicitly instead of returning an apparently complete partial history.
pub async fn conversation_messages(
    client: &DshClient,
    session_id: &str,
) -> Result<Vec<ChatMessage>, DshError> {
    let snapshot = client.snapshot(session_id).await?;
    let mut pages = vec![snapshot.records];
    let mut has_more = snapshot.has_more;
    let mut bytes = serde_json::to_vec(&pages[0])
        .map_err(|e| DshError::Protocol(e.to_string()))?
        .len();
    while has_more {
        let before = pages
            .last()
            .and_then(|page| page.first())
            .and_then(|record| record["event"]["seq"].as_u64())
            .ok_or_else(|| DshError::Protocol("history page did not advance".into()))?;
        let value = client.page(session_id, snapshot.cursor, before).await?;
        bytes = bytes.saturating_add(value.to_string().len());
        if bytes > MAX_WIRE_BYTES {
            return Err(DshError::Protocol(
                "history exceeds legacy RPC limit; use the paginated agent API".into(),
            ));
        }
        let page: Page =
            serde_json::from_value(value).map_err(|e| DshError::Protocol(e.to_string()))?;
        if page
            .records
            .last()
            .and_then(|record| record["event"]["seq"].as_u64())
            .is_none_or(|seq| seq >= before)
        {
            return Err(DshError::Protocol("history page did not advance".into()));
        }
        has_more = page.has_more;
        pages.push(page.records);
    }
    let mut visible = BTreeMap::new();
    for record in pages.into_iter().rev().flatten() {
        let event = &record["event"];
        let seq = event["seq"]
            .as_u64()
            .ok_or_else(|| DshError::Protocol("history event has no sequence".into()))?;
        let op = &event["surfaceOp"];
        if !op.is_null() && op != "append" {
            // Native compaction replaces only the model context. The human transcript retains
            // the original append events and excludes their model-only replacement copies.
            continue;
        }
        let (blocks, user) = match event["type"].as_str() {
            Some("user/message") => (&event["data"]["content"], true),
            Some("assistant/message") => (&event["data"]["message"]["content"], false),
            _ => continue,
        };
        let blocks = blocks
            .as_array()
            .ok_or_else(|| DshError::Protocol("message has no content".into()))?;
        let text = blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .map(|block| {
                block["text"]
                    .as_str()
                    .ok_or_else(|| DshError::Protocol("invalid history text".into()))
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("");
        visible.insert(
            seq,
            if user {
                ChatMessage::user(text)
            } else {
                ChatMessage::assistant(text)
            },
        );
    }
    Ok(visible.into_values().collect())
}
