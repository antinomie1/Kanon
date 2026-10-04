//! Translation between the Milky wire model and the Kanon pipeline contract.
//!
//! # Why mapping is an explicit, documented layer
//! Milky and Kanon do not model conversations the same way. Milky names a conversation by
//! `message_scene` plus `peer_id`; Kanon carries a single opaque `channel_id` on both the inbound
//! and the outbound path and never parses it. Something must therefore define that opaque string,
//! and it must be defined exactly once: this module owns both directions, so the string written on
//! ingest is the string read back on delivery, and no other component needs to know its grammar.
//!
//! ## The channel identifier
//! Inbound messages are ingested with `channel_id = "<scene>:<peer_id>"`, where `<scene>` is one of
//! `friend`, `group` or `temp`. Delivery parses that same form to decide which Milky send endpoint
//! to call. A channel identifier in any other shape is rejected with an explicit error rather than
//! guessed at, because guessing would silently deliver a group message to a friend.
//!
//! ## Segment fidelity
//! Kanon models six segment kinds. Milky models fourteen inbound and ten outbound kinds, so the
//! two sets overlap only partially:
//! - kinds both sides model (`text`, `mention`, `reply`, `image`, `record`) map to their native
//!   Kanon field, which is what lets the model *see* an image URL or a reply target at all;
//! - every other kind (`face`, `video`, `file`, `forward`, `market_face`, `light_app`, `xml`,
//!   `markdown`, and any kind a newer Milky adds) is preserved as a
//!   [`RawCustomSegment`](kanon_proto::v1::RawCustomSegment) named `milky.<type>` whose payload is
//!   the segment's own `data` object, so no content is dropped and an outbound custom segment of
//!   the same name round-trips byte-for-byte.
//!
//! Extras that a native Kanon field cannot hold (an image's width, height and resource id, for
//! instance) are intentionally dropped from the *segment*; the image itself is carried by its URL
//! and the full inbound event stays available to operators in the protocol access log. Duplicating
//! such a segment into both a native field and a custom one would make every downstream consumer
//! render the same picture twice.

use kanon_proto::prost_types;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AudioSegment, ImageSegment, MentionSegment, MessageSegment, PipelineEventRequest,
    RawCustomSegment, ReplySegment, TextSegment, audio_segment, file_segment, image_segment,
    video_segment,
};
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::protocol::{
    Event, IncomingForwardedMessage, IncomingMessage, IncomingSegment, OutgoingSegment,
    OutgoingSegmentFaceData, OutgoingSegmentImageData, OutgoingSegmentVideoData,
};

/// Prefix marking a custom segment as a preserved Milky payload.
///
/// A prefix rather than a bare type name so a custom segment coming from a different platform can
/// never be mistaken for a Milky one, and so an outbound segment that carries no prefix is
/// rejected instead of being sent to the protocol implementation as garbage.
pub const CUSTOM_SEGMENT_PREFIX: &str = "milky.";

/// Scene prefix of a private conversation channel identifier.
pub const CHANNEL_FRIEND: &str = "friend";
/// Scene prefix of a group conversation channel identifier.
pub const CHANNEL_GROUP: &str = "group";
/// Scene prefix of a temporary-session channel identifier.
pub const CHANNEL_TEMP: &str = "temp";

/// Metadata key carrying the Milky event type of an ingested message.
pub const META_EVENT_TYPE: &str = "milky.event_type";
/// Metadata key carrying the conversation scene.
pub const META_SCENE: &str = "milky.message_scene";
/// Metadata key carrying the Milky message sequence number.
pub const META_MESSAGE_SEQ: &str = "milky.message_seq";
/// Metadata key carrying the conversation peer (friend or group number).
pub const META_PEER_ID: &str = "milky.peer_id";
/// Metadata key carrying the sender's QQ number.
pub const META_SENDER_ID: &str = "milky.sender_id";
/// Metadata key carrying the logged-in bot QQ number.
pub const META_SELF_ID: &str = "milky.self_id";
/// Metadata key carrying the message's Unix timestamp in seconds.
pub const META_TIME: &str = "milky.time";
/// Metadata key carrying the group name, when the scene is a group.
pub const META_GROUP_NAME: &str = "milky.group_name";
/// Metadata key carrying the group number of a temporary session, when known.
pub const META_TEMP_GROUP_ID: &str = "milky.temp_group_id";
/// Metadata key carrying the sender's display name.
pub const META_SENDER_NAME: &str = "milky.sender_name";
/// Metadata key carrying the sender's group card, when the scene is a group.
pub const META_SENDER_CARD: &str = "milky.sender_card";
/// Metadata key carrying the sender's group role, when the scene is a group.
pub const META_SENDER_ROLE: &str = "milky.sender_role";

/// Conversation kind of a Kanon channel identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelScene {
    /// One-to-one conversation with a friend.
    Friend,
    /// Group conversation.
    Group,
    /// Temporary session opened from inside a group.
    Temp,
}

impl ChannelScene {
    /// Canonical prefix of this scene in a channel identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Friend => CHANNEL_FRIEND,
            Self::Group => CHANNEL_GROUP,
            Self::Temp => CHANNEL_TEMP,
        }
    }
}

/// A parsed Kanon channel identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelRef {
    /// Conversation kind.
    pub scene: ChannelScene,
    /// Friend QQ number or group number, depending on the scene.
    pub peer_id: i64,
}

/// Builds the Kanon channel identifier for a Milky conversation.
pub fn channel_id(scene: ChannelScene, peer_id: i64) -> String {
    format!("{}:{}", scene.as_str(), peer_id)
}

/// Parses a Kanon channel identifier back into a Milky conversation.
///
/// Rejects anything that does not carry a known scene prefix: a delivery target that cannot be
/// attributed to a conversation kind cannot be routed, and inventing one would send a message to
/// the wrong place.
pub fn parse_channel_id(channel_id: &str) -> Result<ChannelRef, MappingError> {
    let (prefix, peer) = channel_id
        .split_once(':')
        .ok_or_else(|| MappingError::Channel(channel_id.to_string()))?;

    let scene = match prefix {
        CHANNEL_FRIEND => ChannelScene::Friend,
        CHANNEL_GROUP => ChannelScene::Group,
        CHANNEL_TEMP => ChannelScene::Temp,
        _ => return Err(MappingError::Channel(channel_id.to_string())),
    };

    let peer_id = peer
        .parse::<i64>()
        .map_err(|_| MappingError::Channel(channel_id.to_string()))?;

    Ok(ChannelRef { scene, peer_id })
}

/// Resolves a Kanon channel identifier into a Milky send target.
///
/// Temporary sessions are rejected here rather than at the call site: Milky defines no API for
/// sending into one, so the limitation belongs with the grammar that knows about scenes, and every
/// caller inherits the explicit failure instead of having to remember it.
pub fn delivery_target(channel_id: &str) -> Result<ChannelRef, MappingError> {
    let target = parse_channel_id(channel_id)?;
    if target.scene == ChannelScene::Temp {
        return Err(MappingError::TempConversation(channel_id.to_string()));
    }
    Ok(target)
}

/// Reasons an inbound or outbound payload cannot be translated.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MappingError {
    /// The channel identifier does not describe a Milky conversation.
    #[error(
        "channel '{0}' does not name a Milky conversation; expected 'friend:<user_id>', 'group:<group_id>' or 'temp:<user_id>'"
    )]
    Channel(String),
    /// The target conversation is a temporary session, which Milky cannot be sent into.
    #[error(
        "conversation '{0}' cannot receive a reply: the Milky protocol exposes no API for sending into a temporary session"
    )]
    TempConversation(String),
    /// A mention target is not a QQ number.
    #[error("mention target '{0}' is not a QQ number")]
    MentionTarget(String),
    /// A reply target is not a Milky message sequence number.
    #[error("reply target '{0}' is not a Milky message sequence number")]
    ReplyTarget(String),
    /// A custom segment does not name a Milky segment type.
    #[error(
        "custom segment '{0}' is not a Milky segment type; custom segments must be named '{CUSTOM_SEGMENT_PREFIX}<type>'"
    )]
    CustomSegment(String),
    /// The segment kind exists in Kanon but Milky cannot send it inside a message.
    #[error("Milky cannot send {0} inside a message")]
    Unsupported(&'static str),
    /// A file segment carries no name; Milky requires one to upload the file.
    #[error("file segment has no name; Milky uploads files under the name they carry")]
    FileName,
    /// A custom segment's payload does not match its declared Milky type.
    #[error("custom segment '{0}' carries a payload that does not match its Milky type: {1}")]
    CustomPayload(String, String),
    /// The message carries no segment at all.
    #[error("message carries no segments; nothing can be sent to the platform")]
    Empty,
}

/// Inbound translation result: the pipeline request plus the fields the adapter reports.
#[derive(Debug, Clone, PartialEq)]
pub struct InboundMessage {
    /// Channel identifier the conversation is keyed by on both directions.
    pub channel_id: String,
    /// Sender's QQ number as a string, matching the pipeline contract.
    pub sender_id: String,
    /// Textual rendering of the message, used for command parsing and as the model's prompt.
    pub raw_text: String,
    /// Native and custom segments describing the message.
    pub segments: Vec<MessageSegment>,
    /// Structured Milky metadata attached to the event.
    pub metadata: prost_types::Struct,
}

/// Translates a Milky `message_receive` event into the Kanon pipeline contract.
///
/// Returns `Ok(None)` for every other event type: the core pipeline routes *conversational turns*,
/// and feeding it a group-name change as if a user had said it would make the bot answer an event
/// nobody sent it. Non-message events are counted and surfaced by the adapter instead.
pub fn inbound_message(
    platform: &str,
    self_id: i64,
    event: &Event,
) -> Result<Option<PipelineEventRequest>, MappingError> {
    let Event::MessageReceive { time, data, .. } = event else {
        return Ok(None);
    };

    let inbound = translate_incoming_message(data, self_id);
    let event_id = format!(
        "{platform}:{self_id}:{}:{}:{}",
        inbound.channel_id, inbound.sender_id, inbound.message_seq
    );

    let mut metadata = inbound.metadata;
    insert_number(&mut metadata, META_SELF_ID, self_id as f64);
    insert_number(&mut metadata, META_TIME, *time as f64);
    // Platform-neutral timestamp the core's context policy renders when it is enabled.
    insert_number(&mut metadata, kanon_core::META_TIMESTAMP, *time as f64);

    Ok(Some(PipelineEventRequest {
        event_id,
        platform: platform.to_string(),
        channel_id: inbound.channel_id,
        sender_id: inbound.sender_id,
        raw_text: inbound.raw_text,
        segments: inbound.segments,
        metadata: Some(metadata),
    }))
}

/// Message-scoped part of an inbound translation.
#[derive(Debug, Clone, PartialEq)]
struct IncomingTranslation {
    /// Conversation channel identifier.
    channel_id: String,
    /// Sender QQ number rendered for the pipeline.
    sender_id: String,
    /// Milky message sequence number.
    message_seq: i64,
    /// Textual rendering of the segments.
    raw_text: String,
    /// Translated segments.
    segments: Vec<MessageSegment>,
    /// Milky metadata collected while translating.
    metadata: prost_types::Struct,
}

/// Translates the scene-specific payload of an inbound message.
///
/// `self_id` is needed to answer whether the bot itself was addressed, which the core's reply
/// policy consumes; it is the only platform fact the policy cannot infer.
fn translate_incoming_message(message: &IncomingMessage, self_id: i64) -> IncomingTranslation {
    let (scene, peer_id, message_seq, sender_id, segments, sender_name, extra) = match message {
        IncomingMessage::Friend {
            peer_id,
            message_seq,
            sender_id,
            segments,
            friend,
            ..
        } => (
            ChannelScene::Friend,
            *peer_id,
            *message_seq,
            *sender_id,
            segments,
            Some(friend.nickname.clone()),
            Vec::new(),
        ),
        IncomingMessage::Group {
            peer_id,
            message_seq,
            sender_id,
            segments,
            group,
            group_member,
            ..
        } => (
            ChannelScene::Group,
            *peer_id,
            *message_seq,
            *sender_id,
            segments,
            Some(group_member.nickname.clone()),
            vec![
                (
                    META_GROUP_NAME.to_string(),
                    Value::String(group.group_name.clone()),
                ),
                (
                    META_SENDER_CARD.to_string(),
                    Value::String(group_member.card.clone()),
                ),
                (
                    META_SENDER_ROLE.to_string(),
                    Value::String(group_member.role.clone()),
                ),
            ],
        ),
        IncomingMessage::Temp {
            peer_id,
            message_seq,
            sender_id,
            segments,
            group,
            ..
        } => (
            ChannelScene::Temp,
            *peer_id,
            *message_seq,
            *sender_id,
            segments,
            None,
            group
                .as_ref()
                .map(|group| vec![(META_TEMP_GROUP_ID.to_string(), json!(group.group_id))])
                .unwrap_or_default(),
        ),
    };

    let mut metadata_fields: Map<String, Value> = Map::new();
    metadata_fields.insert(META_SCENE.to_string(), json!(scene.as_str()));
    metadata_fields.insert(META_MESSAGE_SEQ.to_string(), json!(message_seq));
    metadata_fields.insert(META_PEER_ID.to_string(), json!(peer_id));
    metadata_fields.insert(META_SENDER_ID.to_string(), json!(sender_id));
    // Platform-neutral facts the core's reply policy consumes. Milky reports the scene and the
    // mention targets; deciding whether they mean "answer this" stays a core (instance) decision.
    metadata_fields.insert(
        kanon_core::META_CONVERSATION_KIND.to_string(),
        json!(match scene {
            ChannelScene::Group => "group",
            ChannelScene::Friend | ChannelScene::Temp => "private",
        }),
    );
    metadata_fields.insert(
        kanon_core::META_BOT_MENTIONED.to_string(),
        json!(mentions_bot(segments, self_id)),
    );
    if let Some(name) = sender_name {
        metadata_fields.insert(META_SENDER_NAME.to_string(), Value::String(name));
    }
    for (key, value) in extra {
        metadata_fields.insert(key, value);
    }
    // Platform-neutral name (group card first) and role, read by the core for speaker labels and
    // command access.
    let generic_name = [META_SENDER_CARD, META_SENDER_NAME]
        .into_iter()
        .filter_map(|key| metadata_fields.get(key).and_then(Value::as_str))
        .map(str::trim)
        .find(|name| !name.is_empty())
        .map(str::to_owned);
    if let Some(name) = generic_name {
        metadata_fields.insert(kanon_core::META_SENDER_NAME.to_string(), json!(name));
    }
    if let Some(role) = metadata_fields
        .get(META_SENDER_ROLE)
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())
        .map(str::to_owned)
    {
        metadata_fields.insert(kanon_core::META_SENDER_ROLE.to_string(), json!(role));
    }

    IncomingTranslation {
        channel_id: channel_id(scene, peer_id),
        sender_id: sender_id.to_string(),
        message_seq,
        raw_text: render_text(segments),
        segments: segments.iter().flat_map(incoming_segments).collect(),
        metadata: json_to_struct(&Value::Object(metadata_fields)),
    }
}

/// Translates one inbound Milky segment into one or more Kanon segments.
///
/// A reply normally maps to a single `reply` segment, but its quoted payload may itself contain
/// media. Those are surfaced as additional image/audio segments so a picture the user quoted
/// reaches the model, instead of only being named inside the snippet.
pub fn incoming_segments(segment: &IncomingSegment) -> Vec<MessageSegment> {
    let IncomingSegment::Reply(data) = segment else {
        return vec![incoming_segment(segment)];
    };

    let mut rendered = vec![incoming_segment(segment)];
    for quoted in &data.segments {
        match quoted {
            IncomingSegment::Image(image) => rendered.push(MessageSegment {
                segment: Some(Segment::Image(ImageSegment {
                    source: Some(image_segment::Source::Url(image.temp_url.clone())),
                    mime_type: None,
                    filename: None,
                })),
            }),
            IncomingSegment::Record(record) => rendered.push(MessageSegment {
                segment: Some(Segment::Audio(AudioSegment {
                    source: Some(audio_segment::Source::Url(record.temp_url.clone())),
                    duration_seconds: Some(record.duration),
                })),
            }),
            _ => {}
        }
    }
    rendered
}

/// Whether a Milky message addresses the bot itself.
///
/// `@all` counts: the mention does include the bot, and a bot configured to answer when mentioned
/// should not ignore exactly the announcement every participant is expected to see.
pub fn mentions_bot(segments: &[IncomingSegment], self_id: i64) -> bool {
    segments.iter().any(|segment| match segment {
        IncomingSegment::Mention(data) => data.user_id == self_id,
        IncomingSegment::MentionAll => true,
        _ => false,
    })
}

/// Renders the human-readable text of a Milky message.
///
/// This is what command parsing and the model prompt see, so it must read the way a participant
/// would read the message: media becomes a short placeholder instead of vanishing, which keeps a
/// picture-only message from looking empty.
pub fn render_text(segments: &[IncomingSegment]) -> String {
    let mut rendered = String::new();
    for segment in segments {
        match segment {
            IncomingSegment::Text(text) => rendered.push_str(text),
            IncomingSegment::Mention(data) => {
                rendered.push('@');
                if data.name.is_empty() {
                    rendered.push_str(&data.user_id.to_string());
                } else {
                    rendered.push_str(&data.name);
                }
            }
            IncomingSegment::MentionAll => rendered.push_str("@all"),
            IncomingSegment::Face(_) => rendered.push_str("[face]"),
            // The quoted message is carried by the reply *segment*, so it is not inlined here:
            // duplicating it would make the model believe the quote was said again. The
            // model-context builder reads the segment itself when a quote is useful.
            IncomingSegment::Reply(_) => {}
            IncomingSegment::Image(_) => rendered.push_str("[image]"),
            IncomingSegment::Record(_) => rendered.push_str("[voice]"),
            IncomingSegment::Video(_) => rendered.push_str("[video]"),
            IncomingSegment::File(data) => {
                rendered.push_str("[file:");
                rendered.push_str(&data.file_name);
                rendered.push(']');
            }
            IncomingSegment::Forward(data) => {
                rendered.push_str("[forward:");
                rendered.push_str(&data.title);
                rendered.push(']');
            }
            IncomingSegment::MarketFace(_) => rendered.push_str("[emoji]"),
            IncomingSegment::LightApp(data) => {
                rendered.push_str("[app:");
                rendered.push_str(&data.app_name);
                rendered.push(']');
            }
            IncomingSegment::Xml(_) => rendered.push_str("[xml]"),
            // Markdown *is* text as far as a model is concerned, so it is passed through verbatim.
            IncomingSegment::Markdown(content) => rendered.push_str(content),
            // A type this release does not know. Its `type` field is still readable, so the
            // placeholder names the kind instead of reporting a generic unknown.
            IncomingSegment::Unknown(raw) => {
                rendered.push('[');
                rendered.push_str(&unknown_segment_type(raw));
                rendered.push(']');
            }
        }
    }
    rendered
}

/// Translates one inbound Milky segment into a Kanon segment.
pub fn incoming_segment(segment: &IncomingSegment) -> MessageSegment {
    let native = match segment {
        IncomingSegment::Text(text) => Segment::Text(TextSegment {
            content: text.clone(),
        }),
        IncomingSegment::Mention(data) => Segment::Mention(MentionSegment {
            target_user_id: data.user_id.to_string(),
            display_name: data.name.clone(),
            is_all: false,
        }),
        IncomingSegment::MentionAll => Segment::Mention(MentionSegment {
            target_user_id: String::new(),
            display_name: String::new(),
            is_all: true,
        }),
        IncomingSegment::Reply(data) => Segment::Reply(ReplySegment {
            target_message_id: data.message_seq.to_string(),
            snippet: render_text(&data.segments),
        }),
        IncomingSegment::Image(data) => Segment::Image(ImageSegment {
            source: Some(image_segment::Source::Url(data.temp_url.clone())),
            mime_type: None,
            filename: None,
        }),
        IncomingSegment::Record(data) => Segment::Audio(AudioSegment {
            source: Some(audio_segment::Source::Url(data.temp_url.clone())),
            duration_seconds: Some(data.duration),
        }),
        other => return custom_segment(other),
    };

    MessageSegment {
        segment: Some(native),
    }
}

/// Extracts the `type` discriminator of a segment this release does not define.
///
/// Falls back to `unknown` when the payload does not even carry a readable type, so callers can
/// always name the segment they preserved.
fn unknown_segment_type(raw: &Value) -> String {
    raw.get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

/// Wraps a Milky segment that has no native Kanon field into a custom segment.
fn custom_segment(segment: &IncomingSegment) -> MessageSegment {
    let (type_name, payload) = match segment {
        // A segment type this Milky release does not define. The original JSON is preserved
        // verbatim so an operator (or a newer adapter) can still see what arrived, and the
        // segment keeps the type name the protocol implementation used, so a round trip through
        // an adapter that does understand it stays possible.
        IncomingSegment::Unknown(raw) => (unknown_segment_type(raw), raw.clone()),
        other => {
            let (name, payload) = known_custom_payload(other);
            (name.to_string(), payload)
        }
    };

    MessageSegment {
        segment: Some(Segment::Custom(RawCustomSegment {
            type_name: format!("{CUSTOM_SEGMENT_PREFIX}{type_name}"),
            payload: Some(json_to_struct(&payload)),
        })),
    }
}

/// Returns the custom segment name and payload of a segment this release does define.
///
/// Split out of [`custom_segment`] so that the unknown-type arm can return an owned name while
/// this one keeps returning a borrowed static one, without either arm paying for the other.
fn known_custom_payload(segment: &IncomingSegment) -> (&'static str, Value) {
    match segment {
        IncomingSegment::Face(data) => (
            "face",
            json!({
                "face_id": data.face_id,
                "is_large": data.is_large,
            }),
        ),
        IncomingSegment::Video(data) => (
            "video",
            json!({
                "resource_id": data.resource_id,
                "temp_url": data.temp_url,
                "width": data.width,
                "height": data.height,
                "duration": data.duration,
            }),
        ),
        IncomingSegment::File(data) => (
            "file",
            json!({
                "file_id": data.file_id,
                "file_name": data.file_name,
                "file_size": data.file_size,
                "file_hash": data.file_hash,
            }),
        ),
        IncomingSegment::Forward(data) => (
            "forward",
            json!({
                "forward_id": data.forward_id,
                "title": data.title,
                "preview": data.preview,
                "summary": data.summary,
            }),
        ),
        IncomingSegment::MarketFace(data) => (
            "market_face",
            json!({
                "emoji_package_id": data.emoji_package_id,
                "emoji_id": data.emoji_id,
                "key": data.key,
                "summary": data.summary,
                "url": data.url,
            }),
        ),
        IncomingSegment::LightApp(data) => (
            "light_app",
            json!({
                "app_name": data.app_name,
                "json_payload": data.json_payload,
            }),
        ),
        IncomingSegment::Xml(data) => (
            "xml",
            json!({
                "service_id": data.service_id,
                "xml_payload": data.xml_payload,
            }),
        ),
        IncomingSegment::Markdown(content) => ("markdown", json!({ "content": content })),
        // The native kinds are handled by the caller; reaching this arm would mean the two
        // matches disagree, so it is reported as an unknown payload instead of an empty one.
        IncomingSegment::Text(_)
        | IncomingSegment::Mention(_)
        | IncomingSegment::MentionAll
        | IncomingSegment::Reply(_)
        | IncomingSegment::Image(_)
        | IncomingSegment::Record(_)
        | IncomingSegment::Unknown(_) => ("unknown", Value::Null),
    }
}

/// A file Milky sends through its upload API, since a Milky message has no file segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundFile {
    /// File URI in one of the three forms Milky accepts.
    pub uri: String,
    /// Name the recipient sees.
    pub name: String,
}

/// A delivery split the way Milky sends it: one message, then each file uploaded after it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboundDelivery {
    /// Message segments; empty when the reply holds nothing but files (and a quote of them).
    pub message: Vec<OutgoingSegment>,
    /// Files uploaded after the message, in order.
    pub files: Vec<OutboundFile>,
}

/// Translates Kanon segments into a Milky message plus the files uploaded after it.
///
/// Fails as soon as one segment cannot be represented, instead of dropping it: a reply that
/// silently loses its attachment is worse than a delivery the operator can see failing. A quote
/// only decorates a message, so when nothing else is left for the message it is not sent alone —
/// an upload cannot quote.
pub fn outbound_delivery(segments: &[MessageSegment]) -> Result<OutboundDelivery, MappingError> {
    let mut message = Vec::new();
    let mut files = Vec::new();
    for segment in segments {
        match segment.segment.as_ref() {
            Some(Segment::File(file)) => {
                if file.name.trim().is_empty() {
                    return Err(MappingError::FileName);
                }
                files.push(OutboundFile {
                    uri: media_uri(file.source.as_ref())?,
                    name: file.name.clone(),
                });
            }
            _ => message.push(outbound_segment(segment)?),
        }
    }

    if message
        .iter()
        .all(|segment| matches!(segment, OutgoingSegment::Reply(_)))
    {
        message.clear();
    }
    if message.is_empty() && files.is_empty() {
        return Err(MappingError::Empty);
    }
    Ok(OutboundDelivery { message, files })
}

/// Translates one Kanon segment into a Milky outbound segment.
pub fn outbound_segment(segment: &MessageSegment) -> Result<OutgoingSegment, MappingError> {
    match segment.segment.as_ref() {
        Some(Segment::Text(text)) => Ok(OutgoingSegment::Text(text.content.clone())),
        Some(Segment::Mention(mention)) => {
            if mention.is_all {
                return Ok(OutgoingSegment::MentionAll);
            }
            let user_id = mention
                .target_user_id
                .trim()
                .parse::<i64>()
                .map_err(|_| MappingError::MentionTarget(mention.target_user_id.clone()))?;
            Ok(OutgoingSegment::Mention(user_id))
        }
        // A quote of an ingested event names its full Kanon event ID, whose last component is the
        // Milky message sequence.
        Some(Segment::Reply(reply)) => {
            let message_seq = reply
                .target_message_id
                .rsplit(':')
                .next()
                .unwrap_or_default()
                .trim()
                .parse::<i64>()
                .map_err(|_| MappingError::ReplyTarget(reply.target_message_id.clone()))?;
            Ok(OutgoingSegment::Reply(message_seq))
        }
        Some(Segment::Image(image)) => Ok(OutgoingSegment::Image(OutgoingSegmentImageData {
            uri: media_uri(image.source.as_ref())?,
            sub_type: "normal".to_string(),
            summary: image.filename.clone(),
        })),
        Some(Segment::Audio(audio)) => {
            Ok(OutgoingSegment::Record(media_uri(audio.source.as_ref())?))
        }
        Some(Segment::Video(video)) => Ok(OutgoingSegment::Video(OutgoingSegmentVideoData {
            uri: media_uri(video.source.as_ref())?,
            thumb_uri: None,
        })),
        Some(Segment::Face(face)) => Ok(OutgoingSegment::Face(OutgoingSegmentFaceData {
            face_id: face.id.clone(),
            is_large: false,
        })),
        // Milky sends files through upload APIs, not as message segments; failing names the
        // problem instead of delivering the message without its file.
        Some(Segment::File(_)) => Err(MappingError::Unsupported(
            "files (they are uploaded with upload_group_file / upload_private_file)",
        )),
        Some(Segment::Custom(custom)) => custom_outbound_segment(custom),
        // A segment with no kind set carries nothing to send; surfacing it beats sending an
        // empty segment the implementation would reject with an opaque parameter error.
        None => Err(MappingError::Empty),
    }
}

/// Renders a Kanon media source as a Milky file URI.
///
/// Milky accepts exactly three URI shapes (`file://`, `http(s)://`, `base64://`), so a Kanon
/// source maps onto one of them without a fallback path: a file path becomes `file://`, an HTTP
/// URL is passed through, and inline bytes become `base64://`.
fn media_uri(source: Option<&impl MediaSource>) -> Result<String, MappingError> {
    match source {
        Some(source) => source.to_milky_uri(),
        None => Err(MappingError::Empty),
    }
}

/// A Kanon media source that can be rendered as a Milky URI.
///
/// Implemented for each generated media `Source` oneof so the identical oneofs share one
/// conversion contract instead of scattered copies of the same match.
pub trait MediaSource {
    /// Renders this source as one of the three URI forms Milky accepts.
    fn to_milky_uri(&self) -> Result<String, MappingError>;
}

impl MediaSource for image_segment::Source {
    fn to_milky_uri(&self) -> Result<String, MappingError> {
        match self {
            image_segment::Source::Url(url) => Ok(url.clone()),
            image_segment::Source::FilePath(path) => Ok(file_uri(path)),
            image_segment::Source::RawBytes(bytes) => Ok(base64_uri(bytes)),
        }
    }
}

impl MediaSource for video_segment::Source {
    fn to_milky_uri(&self) -> Result<String, MappingError> {
        match self {
            video_segment::Source::Url(url) => Ok(url.clone()),
            video_segment::Source::FilePath(path) => Ok(file_uri(path)),
            video_segment::Source::RawBytes(bytes) => Ok(base64_uri(bytes)),
        }
    }
}

impl MediaSource for file_segment::Source {
    fn to_milky_uri(&self) -> Result<String, MappingError> {
        match self {
            file_segment::Source::Url(url) => Ok(url.clone()),
            file_segment::Source::FilePath(path) => Ok(file_uri(path)),
            file_segment::Source::RawBytes(bytes) => Ok(base64_uri(bytes)),
        }
    }
}

impl MediaSource for audio_segment::Source {
    fn to_milky_uri(&self) -> Result<String, MappingError> {
        match self {
            audio_segment::Source::Url(url) => Ok(url.clone()),
            audio_segment::Source::FilePath(path) => Ok(file_uri(path)),
            audio_segment::Source::RawBytes(bytes) => Ok(base64_uri(bytes)),
        }
    }
}

/// Renders a local file path as a `file://` URI.
fn file_uri(path: &str) -> String {
    if path.starts_with("file://") {
        path.to_string()
    } else {
        format!("file://{path}")
    }
}

/// Renders inline bytes as a `base64://` URI.
fn base64_uri(bytes: &[u8]) -> String {
    use base64::Engine;
    format!(
        "base64://{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Restores a preserved Milky segment from a Kanon custom segment.
///
/// Only the outbound kinds are restorable. An inbound-only kind (`file`, `forward`, `market_face`,
/// `xml`, `markdown`) has no send endpoint in the Milky API, so it is reported as an unsupported
/// custom segment instead of being sent as something the protocol implementation would reject.
fn custom_outbound_segment(custom: &RawCustomSegment) -> Result<OutgoingSegment, MappingError> {
    let type_name = custom
        .type_name
        .strip_prefix(CUSTOM_SEGMENT_PREFIX)
        .ok_or_else(|| MappingError::CustomSegment(custom.type_name.clone()))?;

    let payload = custom
        .payload
        .as_ref()
        .map(struct_to_json)
        .transpose()
        .map_err(|error| MappingError::CustomPayload(type_name.into(), error.to_string()))?
        .unwrap_or(Value::Null);

    // Each arm rebuilds the very JSON shape `known_custom_payload` produced inbound, so a segment
    // that made a full round trip through the pipeline is restored without loss.
    match type_name {
        "face" => decode_custom(type_name, payload, |data| {
            Ok(OutgoingSegment::Face(OutgoingSegmentFaceData {
                face_id: string_field(&data, "face_id")?,
                is_large: data
                    .get("is_large")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }))
        }),
        "video" => decode_custom(type_name, payload, |data| {
            Ok(OutgoingSegment::Video(OutgoingSegmentVideoData {
                uri: string_field(&data, "temp_url")?,
                thumb_uri: data
                    .get("thumb_uri")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }))
        }),
        "light_app" => decode_custom(type_name, payload, |data| {
            Ok(OutgoingSegment::LightApp(string_field(
                &data,
                "json_payload",
            )?))
        }),
        other => Err(MappingError::CustomSegment(format!(
            "{CUSTOM_SEGMENT_PREFIX}{other}"
        ))),
    }
}

/// Decodes a custom payload object with a typed builder, mapping JSON failures to payload errors.
fn decode_custom(
    type_name: &str,
    payload: Value,
    build: impl FnOnce(Map<String, Value>) -> Result<OutgoingSegment, MappingError>,
) -> Result<OutgoingSegment, MappingError> {
    let object = payload.as_object().cloned().ok_or_else(|| {
        MappingError::CustomPayload(
            type_name.to_string(),
            "payload is not a JSON object".to_string(),
        )
    })?;

    build(object).map_err(|err| MappingError::CustomPayload(type_name.to_string(), err.to_string()))
}

/// Reads a required string field out of a custom payload object.
fn string_field(object: &Map<String, Value>, key: &str) -> Result<String, MappingError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            MappingError::CustomPayload(key.to_string(), format!("missing field '{key}'"))
        })
}

/// Converts a `serde_json` value into a protobuf `Struct` value tree.
///
/// Written locally rather than reusing the LLM crate's converter: the adapter must not depend on
/// the model gateway, and the conversion is a dozen lines of recursion. Note that a protobuf
/// `Struct` represents every number as a double, so integers beyond 2^53 would lose precision; the
/// fields the adapter stores there (QQ numbers, sequence numbers, timestamps) stay far inside that
/// range.
pub fn json_to_struct(value: &Value) -> prost_types::Struct {
    // A non-object remains visible under the adapter's existing wrapper key.
    match kanon_proto::json::json_to_prost_struct(value) {
        Some(fields) => fields,
        None => prost_types::Struct {
            fields: [(
                "value".to_string(),
                kanon_proto::json::json_to_prost_value(value),
            )]
            .into_iter()
            .collect(),
        },
    }
}

/// Converts a protobuf payload without changing non-finite numbers into JSON null.
pub fn struct_to_json(
    value: &prost_types::Struct,
) -> Result<Value, kanon_proto::json::NonFiniteNumber> {
    kanon_proto::json::prost_struct_to_json(value.clone())
}

/// Inserts a numeric metadata field, replacing any previous value.
fn insert_number(metadata: &mut prost_types::Struct, key: &str, value: f64) {
    metadata.fields.insert(
        key.to_string(),
        prost_types::Value {
            kind: Some(prost_types::value::Kind::NumberValue(value)),
        },
    );
}

/// Returns the Milky outbound segment type name of an outbound segment.
///
/// Used by the adapter's diagnostics, where naming the segment that failed is more useful than
/// reporting a bare index.
pub fn outbound_segment_type(segment: &OutgoingSegment) -> &'static str {
    match segment {
        OutgoingSegment::Text(_) => "text",
        OutgoingSegment::Mention(_) => "mention",
        OutgoingSegment::MentionAll => "mention_all",
        OutgoingSegment::Face(_) => "face",
        OutgoingSegment::Reply(_) => "reply",
        OutgoingSegment::Image(_) => "image",
        OutgoingSegment::Record(_) => "record",
        OutgoingSegment::Video(_) => "video",
        OutgoingSegment::Forward(_) => "forward",
        OutgoingSegment::LightApp(_) => "light_app",
    }
}

/// A notice translated for the pipeline, with the account whose name should become its actor.
pub struct Notice {
    /// The notice event; its actor metadata is filled once the name is known.
    pub event: PipelineEventRequest,
    /// Group the actor belongs to, for a group-card lookup.
    pub group_id: Option<i64>,
    /// Account whose display name describes the notice.
    pub actor_id: Option<i64>,
}

/// Translates the notices Kanon reacts to: joins, nudges of the bot and recalls.
///
/// A recall names the recalled message by the event ID [`inbound_message`] gave it, so the core
/// can tell whether the model ever saw it.
pub fn map_notice(platform: &str, event: &Event) -> Option<Notice> {
    let self_id = event.self_id();
    // (kind, scene, peer, conversation sender, actor, recalled message)
    let (kind, scene, peer, sender, actor, target) = match event {
        Event::GroupMemberIncrease { data, .. } if data.user_id == self_id => {
            let inviter = data.invitor_id.or(data.operator_id);
            (
                "bot_join",
                ChannelScene::Group,
                data.group_id,
                inviter.unwrap_or_default(),
                inviter,
                None,
            )
        }
        Event::GroupMemberIncrease { data, .. } => (
            "member_join",
            ChannelScene::Group,
            data.group_id,
            data.user_id,
            Some(data.user_id),
            None,
        ),
        Event::GroupNudge { data, .. } if data.receiver_id == self_id => (
            "poke",
            ChannelScene::Group,
            data.group_id,
            data.sender_id,
            Some(data.sender_id),
            None,
        ),
        Event::FriendNudge { data, .. } if data.is_self_receive && !data.is_self_send => (
            "poke",
            ChannelScene::Friend,
            data.user_id,
            data.user_id,
            Some(data.user_id),
            None,
        ),
        Event::MessageRecall { data, .. } => {
            let scene = match data.message_scene.as_str() {
                "group" => ChannelScene::Group,
                "temp" => ChannelScene::Temp,
                _ => ChannelScene::Friend,
            };
            let target = format!(
                "{platform}:{self_id}:{}:{}:{}",
                channel_id(scene, data.peer_id),
                data.sender_id,
                data.message_seq
            );
            (
                "recall",
                scene,
                data.peer_id,
                data.sender_id,
                Some(data.operator_id),
                Some(target),
            )
        }
        _ => return None,
    };
    let channel = channel_id(scene, peer);
    let group_id = (scene == ChannelScene::Group).then_some(peer);
    let mut metadata = json!({
        kanon_core::META_NOTICE: kind,
        kanon_core::META_CONVERSATION_KIND: if group_id.is_some() { "group" } else { "private" },
        META_EVENT_TYPE: event.event_type(),
    });
    if let Some(target) = target {
        metadata[kanon_core::META_NOTICE_TARGET] = json!(target);
    }
    let time = match event {
        Event::GroupMemberIncrease { time, .. }
        | Event::GroupNudge { time, .. }
        | Event::FriendNudge { time, .. }
        | Event::MessageRecall { time, .. } => *time,
        _ => 0,
    };
    Some(Notice {
        event: PipelineEventRequest {
            event_id: format!("{platform}:{self_id}:notice:{kind}:{time}:{channel}:{sender}"),
            platform: platform.to_string(),
            channel_id: channel,
            sender_id: if sender == 0 {
                String::new()
            } else {
                sender.to_string()
            },
            raw_text: format!("[{kind}]"),
            segments: Vec::new(),
            metadata: Some(json_to_struct(&metadata)),
        },
        group_id,
        actor_id: actor.filter(|id| *id != 0),
    })
}

/// Records the actor's display name on a notice.
pub fn set_notice_actor(notice: &mut PipelineEventRequest, name: &str) {
    if let Some(metadata) = notice.metadata.as_mut() {
        metadata.fields.insert(
            kanon_core::META_NOTICE_ACTOR.into(),
            kanon_proto::json::json_to_prost_value(&json!(name)),
        );
    }
}

/// IDs of merged forwards in an event whose content has not been fetched yet.
pub fn forward_ids(event: &PipelineEventRequest) -> Vec<String> {
    event
        .segments
        .iter()
        .filter_map(|segment| match &segment.segment {
            Some(Segment::Custom(custom))
                if custom.type_name == format!("{CUSTOM_SEGMENT_PREFIX}forward") =>
            {
                match custom
                    .payload
                    .as_ref()?
                    .fields
                    .get("forward_id")?
                    .kind
                    .as_ref()?
                {
                    prost_types::value::Kind::StringValue(id) => Some(id.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

/// Stores a fetched merged forward in its segment as `messages: [{sender, text, images}]`.
pub fn attach_forward(
    event: &mut PipelineEventRequest,
    forward_id: &str,
    messages: &[IncomingForwardedMessage],
) -> Result<(), String> {
    let rendered: Vec<Value> = messages
        .iter()
        .map(|message| {
            let images: Vec<&str> = message
                .segments
                .iter()
                .filter_map(|segment| match segment {
                    IncomingSegment::Image(image) => Some(image.temp_url.as_str()),
                    _ => None,
                })
                .collect();
            json!({
                "sender": message.sender_name,
                "text": render_text(&message.segments),
                "images": images,
            })
        })
        .collect();
    let target = forward_id.to_string();
    let custom = event
        .segments
        .iter_mut()
        .find_map(|segment| match &mut segment.segment {
            Some(Segment::Custom(custom))
                if custom.type_name == format!("{CUSTOM_SEGMENT_PREFIX}forward")
                    && custom
                        .payload
                        .as_ref()
                        .and_then(|payload| payload.fields.get("forward_id"))
                        .and_then(|value| value.kind.as_ref())
                        == Some(&prost_types::value::Kind::StringValue(target.clone())) =>
            {
                Some(custom)
            }
            _ => None,
        })
        .ok_or_else(|| format!("event has no forward segment {forward_id}"))?;
    custom
        .payload
        .get_or_insert_with(Default::default)
        .fields
        .insert(
            "messages".into(),
            kanon_proto::json::json_to_prost_value(&Value::Array(rendered)),
        );
    Ok(())
}

/// Translates a friend request or group invitation into a notice the core may accept.
///
/// The request token names what [`accept_request_input`] needs: the initiator's UID for a friend
/// request, the group and invitation sequence for an invitation.
pub fn map_request(platform: &str, event: &Event) -> Option<PipelineEventRequest> {
    let self_id = event.self_id();
    let (kind, channel, sender, token) = match event {
        Event::FriendRequest { data, .. } => (
            "friend_request",
            channel_id(ChannelScene::Friend, data.initiator_id),
            data.initiator_id,
            format!("friend:{}", data.initiator_uid),
        ),
        Event::GroupInvitation { data, .. } => (
            "group_invite",
            channel_id(ChannelScene::Group, data.group_id),
            data.initiator_id,
            format!("group:{}:{}", data.group_id, data.invitation_seq),
        ),
        _ => return None,
    };
    let metadata = json!({
        kanon_core::META_NOTICE: kind,
        kanon_core::META_REQUEST_TOKEN: token,
        kanon_core::META_NOTICE_ACTOR: sender.to_string(),
        META_EVENT_TYPE: event.event_type(),
    });
    Some(PipelineEventRequest {
        event_id: format!("{platform}:{self_id}:request:{token}"),
        platform: platform.to_string(),
        channel_id: channel,
        sender_id: sender.to_string(),
        raw_text: format!("[{kind}]"),
        segments: Vec::new(),
        metadata: Some(json_to_struct(&metadata)),
    })
}

/// What a request notice's token asks the Milky API to accept.
pub enum AcceptRequest {
    /// `accept_friend_request` for this initiator UID.
    Friend(String),
    /// `accept_group_invitation` for this group and invitation sequence.
    Group(i64, i64),
}

/// Reads the request token [`map_request`] wrote.
pub fn accept_request_input(event: &PipelineEventRequest) -> Result<AcceptRequest, String> {
    let token = match event
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.fields.get(kanon_core::META_REQUEST_TOKEN))
        .and_then(|value| value.kind.as_ref())
    {
        Some(prost_types::value::Kind::StringValue(token)) => token.as_str(),
        _ => return Err("request event lacks its token".into()),
    };
    if let Some(uid) = token.strip_prefix("friend:").filter(|uid| !uid.is_empty()) {
        return Ok(AcceptRequest::Friend(uid.to_string()));
    }
    let parsed = token.strip_prefix("group:").and_then(|rest| {
        let (group, seq) = rest.split_once(':')?;
        Some((group.parse().ok()?, seq.parse().ok()?))
    });
    match parsed {
        Some((group, seq)) => Ok(AcceptRequest::Group(group, seq)),
        None => Err(format!("'{token}' is not a Milky request token")),
    }
}
