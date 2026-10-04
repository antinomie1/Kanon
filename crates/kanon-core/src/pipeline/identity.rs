//! Platform-aware labels for model-visible identity, separate from untrusted message text.

use kanon_proto::v1::PipelineEventRequest;

use crate::conversation::{ContextPolicy, ConversationKind};
use crate::notice::metadata_str;

/// Human-readable channel or group name reported by the adapter, never inferred from an ID.
pub const META_CHANNEL_NAME: &str = "kanon.channel_name";
/// Sender's account nickname, distinct from a group-specific card.
pub const META_SENDER_NICKNAME: &str = "kanon.sender_nickname";
/// Group-specific display name; absent for platforms without group cards.
pub const META_SENDER_CARD: &str = "kanon.sender_card";
/// Identifier semantics reported by an adapter: `qq`, `openid`, or a platform-specific label.
pub const META_IDENTITY_KIND: &str = "kanon.identity_kind";
/// The platform's raw conversation ID, without Kanon's routing prefix.
pub const META_CHANNEL_ID: &str = "kanon.channel_id";

/// Quotes and bounds external names so newlines or brackets cannot forge another label.
fn label(value: &str) -> String {
    let text: String = value.trim().chars().take(120).collect();
    serde_json::Value::String(text).to_string()
}

/// Labels one speaker consistently in both current messages and observed group history.
pub fn speaker_label(event: &PipelineEventRequest, include_id: bool) -> String {
    let meta = event.metadata.as_ref();
    let mut fields = Vec::new();
    if let Some(nickname) =
        metadata_str(meta, META_SENDER_NICKNAME).filter(|name| !name.trim().is_empty())
    {
        fields.push(format!("昵称={}", label(nickname)));
    }
    if let Some(card) = metadata_str(meta, META_SENDER_CARD).filter(|card| !card.trim().is_empty())
    {
        fields.push(format!("群名片={}", label(card)));
    }
    if fields.is_empty()
        && let Some(name) = metadata_str(meta, crate::META_SENDER_NAME)
    {
        fields.push(format!("显示名={}", label(name)));
    }
    if include_id && !event.sender_id.is_empty() {
        let kind = match metadata_str(meta, META_IDENTITY_KIND) {
            Some("qq") => "QQ号",
            Some("openid") => "用户OpenID",
            _ => "用户ID",
        };
        fields.push(format!("{kind}={}", label(&event.sender_id)));
    }
    if fields.is_empty() {
        "发送者（未提供显示名）".into()
    } else {
        fields.join("，")
    }
}

/// Current-turn context; the static system prefix never contains platform or participant values.
pub fn context_labels(event: &PipelineEventRequest, policy: &ContextPolicy) -> String {
    let meta = event.metadata.as_ref();
    // Embedded callers may not report a platform context at all. Do not manufacture a
    // private conversation or an unknown speaker for a text-only internal message.
    if metadata_str(meta, crate::META_CONVERSATION_KIND).is_none()
        && metadata_str(meta, META_IDENTITY_KIND).is_none()
        && metadata_str(meta, META_CHANNEL_NAME).is_none()
        && metadata_str(meta, crate::META_SENDER_NAME).is_none()
        && !policy.include_channel_id
        && !policy.include_sender_id
    {
        return String::new();
    }
    let kind = ConversationKind::from_metadata(meta);
    let kind_name = match kind {
        ConversationKind::Private => "私聊",
        ConversationKind::Group => "群聊",
        ConversationKind::Channel => "频道",
    };
    let mut fields = vec![format!(
        "平台={}，会话类型={kind_name}",
        label(&event.platform)
    )];
    if let Some(name) = metadata_str(meta, META_CHANNEL_NAME).filter(|name| !name.trim().is_empty())
    {
        let field = if kind == ConversationKind::Group {
            "群名"
        } else {
            "会话名称"
        };
        fields.push(format!("{field}={}", label(name)));
    }
    if policy.include_channel_id && !event.channel_id.is_empty() {
        let field = match (kind, metadata_str(meta, META_IDENTITY_KIND)) {
            (ConversationKind::Group, Some("qq")) => "QQ群号",
            (ConversationKind::Group, Some("openid")) => "群OpenID",
            (ConversationKind::Group, _) => "群ID",
            (ConversationKind::Channel, _) => "频道ID",
            _ => "会话ID",
        };
        let id = metadata_str(meta, META_CHANNEL_ID).unwrap_or(&event.channel_id);
        fields.push(format!("{field}={}", label(id)));
    }
    fields.push(speaker_label(event, policy.include_sender_id));
    format!("[会话信息：{}]", fields.join("；"))
}
