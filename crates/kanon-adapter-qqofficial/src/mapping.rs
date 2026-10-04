//! QQ gateway events to Kanon events.
//!
//! Channel IDs are `group:<group_openid>`, `c2c:<user_openid>`, `guild:<channel_id>` and
//! `guild_dm:<guild_id>`. The event ID is QQ's own message ID, because a passive reply must quote
//! it back as `msg_id` and it is already globally unique.
//!
//! # Quoted messages
//! QQ does not send the quoted message as a segment. A quote is announced by `message_type = 103`
//! and a `ref_msg_idx=<key>` entry in `message_scene.ext`, and QQ usually pushes the quoted
//! content itself as `msg_elements[0]` (text plus attachments). When it does not — e.g. for the
//! bot's own Markdown replies — the adapter falls back to the [`QuoteStore`], which remembers
//! recent messages under the index QQ quotes them by (`msg_idx` for inbound messages, `ref_idx`
//! for the bot's own sends). Guild messages quote by `message_reference.message_id` instead.
//!
//! The quote becomes a reply segment whose snippet is the quoted text with short placeholders
//! for its media, followed by the quoted images as image segments so a vision model can see
//! them — the same shape the OneBot and Milky adapters produce. Transport details that mean
//! nothing to a model (attachment file names that are content hashes, media URLs of voice and
//! video, raw face-tag payloads) are left out.

use std::collections::{HashMap, VecDeque};

use base64::Engine;
use kanon_proto::prost_types;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    ImageSegment, MessageSegment, PipelineEventRequest, RawCustomSegment, ReplySegment,
    TextSegment, image_segment,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::PLATFORM;

/// Prefix of an event ID that names a gateway event rather than a message.
///
/// QQ lets a bot answer a bot-added or friend-added event passively by echoing the event's own ID
/// as `event_id` (instead of a message's `msg_id`); the prefix tells delivery which field to use.
pub const EVENT_ID_PREFIX: &str = "event:";

/// `message_type` of a message that quotes another one.
const MSG_TYPE_QUOTE: i64 = 103;

/// Snippet used when a quote cannot be resolved, so the model still learns that the user was
/// pointing at an earlier message instead of seeing an unexplained utterance.
const UNAVAILABLE_QUOTE: &str = "[unavailable]";

/// What the model sees of a quoted message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    /// Text with media placeholders, e.g. `look at this [image]`.
    pub text: String,
    /// URLs of the quoted images.
    pub images: Vec<String>,
}

/// Recently seen messages, keyed by the index QQ uses when they are quoted.
///
/// Bounded and in-memory: it only backs up `msg_elements`, which QQ normally sends, and losing it
/// on restart merely makes an old quote show as unavailable.
pub struct QuoteStore {
    capacity: usize,
    entries: HashMap<String, Quote>,
    /// Insertion order, oldest first, for eviction.
    order: VecDeque<String>,
}

impl QuoteStore {
    /// Creates a store remembering at most `capacity` messages.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Remembers a message, evicting the oldest one when full.
    pub fn insert(&mut self, key: String, quote: Quote) {
        if key.is_empty() || self.capacity == 0 {
            return;
        }
        if self.entries.insert(key.clone(), quote).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    /// Looks a message up by its quote key.
    pub fn get(&self, key: &str) -> Option<&Quote> {
        self.entries.get(key)
    }
}

/// One media attachment on an inbound message or quoted element.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Attachment {
    content_type: String,
    url: String,
    filename: Option<String>,
    size: Option<u64>,
    /// QQ's own speech recognition of a voice message.
    asr_refer_text: Option<String>,
}

/// What an attachment is, as far as the model is concerned.
#[derive(Debug, PartialEq, Eq)]
enum AttachmentKind {
    Image,
    Voice,
    Video,
    File,
}

impl Attachment {
    fn kind(&self) -> AttachmentKind {
        let kind = self.content_type.to_ascii_lowercase();
        if kind.starts_with("image") {
            AttachmentKind::Image
        } else if kind == "voice"
            || kind.starts_with("audio")
            || kind.contains("silk")
            || kind.contains("amr")
        {
            AttachmentKind::Voice
        } else if kind.starts_with("video") {
            AttachmentKind::Video
        } else {
            AttachmentKind::File
        }
    }

    /// Download URL with a scheme; guild attachments arrive without one.
    fn url(&self) -> String {
        let url = self.url.trim();
        if url.starts_with("http://") || url.starts_with("https://") {
            url.to_owned()
        } else if let Some(rest) = url.strip_prefix("//") {
            format!("https://{rest}")
        } else {
            format!("https://{url}")
        }
    }

    fn transcript(&self) -> Option<&str> {
        self.asr_refer_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }

    /// The name a user gave a file; image and media names are content hashes and not shown.
    fn file_name(&self) -> Option<&str> {
        self.filename
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    /// Short text form, used in `raw_text` and quote snippets.
    fn placeholder(&self) -> String {
        match self.kind() {
            AttachmentKind::Image => "[image]".into(),
            AttachmentKind::Voice => match self.transcript() {
                Some(text) => format!("[voice: {text}]"),
                None => "[voice]".into(),
            },
            AttachmentKind::Video => "[video]".into(),
            AttachmentKind::File => match self.file_name() {
                Some(name) => format!("[file:{name}]"),
                None => "[file]".into(),
            },
        }
    }

    /// Segment for an attachment of the message being ingested.
    fn segment(&self) -> Result<MessageSegment, String> {
        let segment = match self.kind() {
            AttachmentKind::Image => Segment::Image(ImageSegment {
                source: Some(image_segment::Source::Url(self.url())),
                mime_type: Some(self.content_type.clone()).filter(|kind| kind.contains('/')),
                filename: None,
            }),
            // Kept as text: QQ voice is SILK behind a short-lived URL no model can play, while
            // the transcript QQ already made is exactly what the model needs.
            AttachmentKind::Voice => custom(
                "qqofficial.voice",
                match self.transcript() {
                    Some(text) => json!({"text": text}),
                    None => json!({}),
                },
            )?,
            AttachmentKind::Video => custom("qqofficial.video", json!({}))?,
            AttachmentKind::File => {
                let mut payload = json!({"file_name": self.file_name().unwrap_or("file")});
                if let Some(size) = self.size {
                    payload["file_size"] = json!(size);
                }
                custom("qqofficial.file", payload)?
            }
        };
        Ok(MessageSegment {
            segment: Some(segment),
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Author {
    id: String,
    user_openid: String,
    member_openid: String,
    username: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Mention {
    /// Set on the entry that mentions this bot.
    is_you: bool,
}

/// One element of `msg_elements`; for a quote, the first one is the quoted message.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MsgElement {
    msg_idx: Option<String>,
    content: String,
    attachments: Vec<Attachment>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Scene {
    /// `key=value` entries such as `msg_idx=REFIDX_…` and `ref_msg_idx=REFIDX_…`.
    ext: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Reference {
    message_id: String,
}

/// The fields of every QQ message event this adapter reads.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MessageEvent {
    id: String,
    content: String,
    timestamp: String,
    author: Author,
    group_openid: String,
    channel_id: String,
    guild_id: String,
    attachments: Vec<Attachment>,
    mentions: Vec<Mention>,
    message_type: Option<i64>,
    msg_elements: Vec<MsgElement>,
    message_scene: Scene,
    message_reference: Option<Reference>,
}

impl MessageEvent {
    /// Reads one `key=value` entry of `message_scene.ext`.
    fn scene_ext(&self, key: &str) -> Option<String> {
        self.message_scene.ext.iter().find_map(|entry| {
            let (name, value) = entry.split_once('=')?;
            (name.trim() == key && !value.trim().is_empty()).then(|| value.trim().to_owned())
        })
    }

    /// The key the quoted message is known by, if this message quotes one.
    fn quote_key(&self) -> Option<String> {
        if self.message_type == Some(MSG_TYPE_QUOTE)
            && let Some(key) = self.msg_elements.first().and_then(|el| el.msg_idx.clone())
            && !key.is_empty()
        {
            return Some(key);
        }
        self.scene_ext("ref_msg_idx").or_else(|| {
            self.message_reference
                .as_ref()
                .map(|reference| reference.message_id.clone())
                .filter(|id| !id.is_empty())
        })
    }
}

/// Maps a bot-added-to-group or friend-added dispatch to a notice event; `None` for any other.
///
/// `gateway_id` is the dispatch's top-level `id`, which a greeting must quote to be delivered as
/// a passive reply. The operator who added the bot is known only by an openid, so no actor name
/// is reported.
pub fn map_notice(
    event_type: &str,
    gateway_id: &str,
    data: &Value,
) -> Option<Result<PipelineEventRequest, String>> {
    let (notice, scene, peer, sender, kind) = match event_type {
        "GROUP_ADD_ROBOT" => (
            "bot_join",
            "group",
            &data["group_openid"],
            &data["op_member_openid"],
            "group",
        ),
        "FRIEND_ADD" => (
            "friend_add",
            "c2c",
            &data["openid"],
            &data["openid"],
            "private",
        ),
        _ => return None,
    };
    let peer = peer.as_str().unwrap_or_default();
    if peer.is_empty() || gateway_id.is_empty() {
        return Some(Err(format!(
            "{event_type} event lacks its conversation or event ID"
        )));
    }
    let metadata = json!({
        kanon_core::META_NOTICE: notice,
        kanon_core::META_CONVERSATION_KIND: kind,
        "qqofficial.scene": scene,
    });
    Some(to_struct(&metadata).map(|metadata| PipelineEventRequest {
        event_id: format!("{EVENT_ID_PREFIX}{gateway_id}"),
        platform: PLATFORM.into(),
        channel_id: format!("{scene}:{peer}"),
        sender_id: sender.as_str().unwrap_or_default().to_owned(),
        raw_text: format!("[{notice}]"),
        segments: Vec::new(),
        metadata: Some(metadata),
    }))
}

/// Maps one gateway dispatch to a pipeline event; non-message events yield `None`.
///
/// Every message is also remembered in `quotes`, so a later quote of it can be resolved even when
/// QQ omits the quoted content. `bot_id` is the bot's own user ID from `READY`, used to drop the
/// bot's @-marker from guild messages.
pub fn map_event(
    event_type: &str,
    data: &Value,
    bot_id: &str,
    quotes: &mut QuoteStore,
) -> Result<Option<PipelineEventRequest>, String> {
    let event = MessageEvent::deserialize(data)
        .map_err(|err| format!("malformed {event_type} event: {err}"))?;

    // (channel scheme, peer, sender, conversation kind, whether the bot was addressed)
    let (scene, peer, sender, kind, mentioned) = match event_type {
        "GROUP_AT_MESSAGE_CREATE" => (
            "group",
            &event.group_openid,
            &event.author.member_openid,
            "group",
            true,
        ),
        "GROUP_MESSAGE_CREATE" => (
            "group",
            &event.group_openid,
            &event.author.member_openid,
            "group",
            event.mentions.iter().any(|mention| mention.is_you),
        ),
        "C2C_MESSAGE_CREATE" => (
            "c2c",
            &event.author.user_openid,
            &event.author.user_openid,
            "private",
            false,
        ),
        "AT_MESSAGE_CREATE" => (
            "guild",
            &event.channel_id,
            &event.author.id,
            "channel",
            true,
        ),
        "DIRECT_MESSAGE_CREATE" => (
            "guild_dm",
            &event.guild_id,
            &event.author.id,
            "private",
            false,
        ),
        _ => return Ok(None),
    };
    if event.id.is_empty() || peer.is_empty() || sender.is_empty() {
        return Err(format!(
            "{event_type} event lacks its message, conversation or sender ID"
        ));
    }

    let content = clean_content(&event.content, bot_id);
    let mut segments = Vec::new();

    if let Some(key) = event.quote_key() {
        let quote = event
            .msg_elements
            .first()
            .map(|element| {
                quote_of(
                    &clean_content(&element.content, bot_id),
                    &element.attachments,
                )
            })
            .filter(|quote| !quote.text.is_empty())
            .or_else(|| quotes.get(&key).cloned())
            .unwrap_or_else(|| Quote {
                text: UNAVAILABLE_QUOTE.into(),
                images: Vec::new(),
            });
        segments.push(MessageSegment {
            segment: Some(Segment::Reply(ReplySegment {
                target_message_id: key,
                snippet: quote.text,
            })),
        });
        segments.extend(quote.images.into_iter().map(|url| MessageSegment {
            segment: Some(Segment::Image(ImageSegment {
                source: Some(image_segment::Source::Url(url)),
                mime_type: None,
                filename: None,
            })),
        }));
    }

    if !content.is_empty() {
        segments.push(MessageSegment {
            segment: Some(Segment::Text(TextSegment {
                content: content.clone(),
            })),
        });
    }
    for attachment in &event.attachments {
        segments.push(attachment.segment()?);
    }

    let own = quote_of(&content, &event.attachments);
    let raw_text = own.text.clone();
    let record_key = event
        .scene_ext("msg_idx")
        .unwrap_or_else(|| event.id.clone());
    quotes.insert(record_key, own);

    let mut metadata = json!({
        "qqofficial.scene": scene,
        "qqofficial.msg_id": event.id,
        kanon_core::META_CONVERSATION_KIND: kind,
        kanon_core::META_BOT_MENTIONED: mentioned,
    });
    if !event.timestamp.is_empty() {
        metadata[kanon_core::META_TIMESTAMP_TEXT] = json!(event.timestamp);
    }
    if !event.author.username.is_empty() {
        metadata["qqofficial.sender_name"] = json!(event.author.username);
        metadata[kanon_core::META_SENDER_NAME] = json!(event.author.username);
    }
    if scene == "guild" && !event.guild_id.is_empty() {
        metadata["qqofficial.guild_id"] = json!(event.guild_id);
    }

    Ok(Some(PipelineEventRequest {
        event_id: event.id.clone(),
        platform: PLATFORM.into(),
        channel_id: format!("{scene}:{peer}"),
        sender_id: sender.clone(),
        raw_text,
        segments,
        metadata: Some(to_struct(&metadata)?),
    }))
}

/// The model-facing form of a message: text with media placeholders plus its image URLs.
fn quote_of(content: &str, attachments: &[Attachment]) -> Quote {
    let mut parts: Vec<String> = Vec::new();
    if !content.is_empty() {
        parts.push(content.to_owned());
    }
    parts.extend(attachments.iter().map(Attachment::placeholder));
    Quote {
        text: parts.join(" "),
        images: attachments
            .iter()
            .filter(|attachment| attachment.kind() == AttachmentKind::Image)
            .map(Attachment::url)
            .collect(),
    }
}

/// Rewrites QQ's inline tags into readable text.
///
/// - `<faceType=…,faceId="…",ext="<base64 JSON>">` (a QQ emoji or sticker) becomes
///   `[表情:<name>]`, using the name carried in `ext`; the base64 itself is never shown.
/// - `<@!id>` / `<@id>` becomes `@id`, except the bot's own marker, which only says the bot was
///   addressed — already reported as metadata — and is removed.
///
/// Any other `<…>` is ordinary text and left alone.
pub fn clean_content(content: &str, bot_id: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find('>') else {
            // A lone `<` is ordinary text.
            out.push_str(tail);
            rest = "";
            break;
        };
        let tag = &tail[1..end];
        if tag.starts_with("faceType=") {
            out.push_str(&face_label(tag));
        } else if let Some(id) = tag.strip_prefix('@') {
            let id = id.trim_start_matches('!');
            if bot_id.is_empty() || id != bot_id {
                out.push('@');
                out.push_str(id);
            }
        } else {
            out.push_str(&tail[..=end]);
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// `[表情:<name>]` for a face tag, or `[表情]` when its `ext` carries no readable name.
fn face_label(tag: &str) -> String {
    let name = tag
        .split_once("ext=\"")
        .and_then(|(_, ext)| ext.split('"').next())
        .and_then(|ext| base64::engine::general_purpose::STANDARD.decode(ext).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|json| json["text"].as_str().map(str::trim).map(str::to_owned))
        .filter(|name| !name.is_empty());
    match name {
        Some(name) => format!("[表情:{name}]"),
        None => "[表情]".into(),
    }
}

fn custom(type_name: &str, payload: Value) -> Result<Segment, String> {
    Ok(Segment::Custom(RawCustomSegment {
        type_name: type_name.into(),
        payload: Some(to_struct(&payload)?),
    }))
}

/// Converts a flat JSON object (strings, booleans and numbers) into a protobuf `Struct`.
fn to_struct(value: &Value) -> Result<prost_types::Struct, String> {
    let object = value.as_object().ok_or("payload must be an object")?;
    for (key, value) in object {
        if !matches!(value, Value::String(_) | Value::Bool(_) | Value::Number(_)) {
            return Err(format!("unsupported payload value for '{key}': {value}"));
        }
    }
    kanon_proto::json::json_to_prost_struct(value).ok_or_else(|| "payload must be an object".into())
}
