//! Generated Milky segments.

use super::*;

/// Received message segment.
#[derive(Debug, Clone, PartialEq)]
pub enum IncomingSegment {
    /// Text message segment.
    Text(String),
    /// Mention message segment.
    Mention(IncomingSegmentMentionData),
    /// Mention-all message segment.
    MentionAll,
    /// Emoji message segment.
    Face(IncomingSegmentFaceData),
    /// Reply message segment.
    Reply(IncomingSegmentReplyData),
    /// Image message segment.
    Image(IncomingSegmentImageData),
    /// Voice message segment.
    Record(IncomingSegmentRecordData),
    /// Video message segment.
    Video(IncomingSegmentVideoData),
    /// File message segment.
    File(IncomingSegmentFileData),
    /// Merged forward message segment.
    Forward(IncomingSegmentForwardData),
    /// Market emoji message segment.
    MarketFace(IncomingSegmentMarketFaceData),
    /// Light app (mini program) message segment.
    LightApp(IncomingSegmentLightAppData),
    /// XML message segment.
    Xml(IncomingSegmentXmlData),
    /// Markdown message segment.
    /// @since 1.3
    Markdown(String),
    /// A segment whose `type` this Milky release does not define.
    ///
    /// LOCAL PATCH: the generator degrades such a segment to a placeholder text segment, which
    /// loses the payload. Keeping the raw `{ "type": ..., "data": ... }` object lets the adapter
    /// report the segment type and forward the untouched payload instead.
    Unknown(serde_json::Value),
}

impl Serialize for IncomingSegment {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            IncomingSegment::Text(text) => serialize_segment_with_data(
                serializer,
                "text",
                &IncomingSegmentTextData { text: text.clone() },
            ),
            IncomingSegment::Mention(data) => {
                serialize_segment_with_data(serializer, "mention", data)
            }
            IncomingSegment::MentionAll => serialize_segment_with_data(
                serializer,
                "mention_all",
                &IncomingSegmentMentionAllData {},
            ),
            IncomingSegment::Face(data) => serialize_segment_with_data(serializer, "face", data),
            IncomingSegment::Reply(data) => serialize_segment_with_data(serializer, "reply", data),
            IncomingSegment::Image(data) => serialize_segment_with_data(serializer, "image", data),
            IncomingSegment::Record(data) => {
                serialize_segment_with_data(serializer, "record", data)
            }
            IncomingSegment::Video(data) => serialize_segment_with_data(serializer, "video", data),
            IncomingSegment::File(data) => serialize_segment_with_data(serializer, "file", data),
            IncomingSegment::Forward(data) => {
                serialize_segment_with_data(serializer, "forward", data)
            }
            IncomingSegment::MarketFace(data) => {
                serialize_segment_with_data(serializer, "market_face", data)
            }
            IncomingSegment::LightApp(data) => {
                serialize_segment_with_data(serializer, "light_app", data)
            }
            IncomingSegment::Xml(data) => serialize_segment_with_data(serializer, "xml", data),
            IncomingSegment::Markdown(content) => serialize_segment_with_data(
                serializer,
                "markdown",
                &IncomingSegmentMarkdownData {
                    content: content.clone(),
                },
            ),
            // LOCAL PATCH: the raw object already has the wire shape `{type, data}`.
            IncomingSegment::Unknown(raw) => raw.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for IncomingSegment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match deserialize_segment_type::<D::Error>(&value)? {
            "text" => Ok(Self::Text(
                deserialize_segment_data::<D::Error, IncomingSegmentTextData>(&value)?.text,
            )),
            "mention" => Ok(Self::Mention(deserialize_segment_data(&value)?)),
            "mention_all" => {
                let _: IncomingSegmentMentionAllData = deserialize_segment_data(&value)?;
                Ok(Self::MentionAll)
            }
            "face" => Ok(Self::Face(deserialize_segment_data(&value)?)),
            "reply" => Ok(Self::Reply(deserialize_segment_data(&value)?)),
            "image" => Ok(Self::Image(deserialize_segment_data(&value)?)),
            "record" => Ok(Self::Record(deserialize_segment_data(&value)?)),
            "video" => Ok(Self::Video(deserialize_segment_data(&value)?)),
            "file" => Ok(Self::File(deserialize_segment_data(&value)?)),
            "forward" => Ok(Self::Forward(deserialize_segment_data(&value)?)),
            "market_face" => Ok(Self::MarketFace(deserialize_segment_data(&value)?)),
            "light_app" => Ok(Self::LightApp(deserialize_segment_data(&value)?)),
            "xml" => Ok(Self::Xml(deserialize_segment_data(&value)?)),
            "markdown" => Ok(Self::Markdown(
                deserialize_segment_data::<D::Error, IncomingSegmentMarkdownData>(&value)?.content,
            )),
            // LOCAL PATCH: an unknown type is data, not an error. The value is cloned because
            // the match scrutinee borrows it for the duration of the match expression.
            _ => Ok(Self::Unknown(value.clone())),
        }
    }
}

/// Text message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentTextData {
    /// Text content.
    #[serde(rename = "text")]
    pub text: String,
}

/// Mention message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentMentionData {
    /// Mentioned QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Mentioned name without the `@` prefix.
    /// @since 1.2
    #[serde(rename = "name")]
    pub name: String,
}

/// Mention-all message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentMentionAllData {}

/// Emoji message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentFaceData {
    /// Emoji ID.
    #[serde(rename = "face_id")]
    pub face_id: String,
    /// Whether it is a super emoji.
    /// @since 1.1
    #[serde(rename = "is_large")]
    pub is_large: bool,
}

/// Reply message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentReplyData {
    /// Sequence number of the quoted message.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
    /// QQ number of the sender of the quoted message.
    /// @since 1.2
    #[serde(rename = "sender_id")]
    pub sender_id: i64,
    /// Name of the sender of the quoted message, available only in merged forwards.
    /// @since 1.2
    #[serde(
        rename = "sender_name",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sender_name: Option<String>,
    /// Unix timestamp of the quoted message.
    /// @since 1.2
    #[serde(rename = "time")]
    pub time: i64,
    /// Content of the quoted message.
    /// @since 1.2
    #[serde(
        rename = "segments",
        deserialize_with = "deserialize_incoming_segment_list"
    )]
    pub segments: Vec<IncomingSegment>,
}

/// Image message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentImageData {
    /// Resource ID.
    #[serde(rename = "resource_id")]
    pub resource_id: String,
    /// Temporary URL.
    #[serde(rename = "temp_url")]
    pub temp_url: String,
    /// Image width.
    #[serde(rename = "width")]
    pub width: i32,
    /// Image height.
    #[serde(rename = "height")]
    pub height: i32,
    /// Image preview text.
    #[serde(rename = "summary")]
    pub summary: String,
    /// Image type.
    #[serde(rename = "sub_type")]
    pub sub_type: String,
}

/// Voice message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentRecordData {
    /// Resource ID.
    #[serde(rename = "resource_id")]
    pub resource_id: String,
    /// Temporary URL.
    #[serde(rename = "temp_url")]
    pub temp_url: String,
    /// Voice duration in seconds.
    #[serde(rename = "duration")]
    pub duration: i32,
}

/// Video message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentVideoData {
    /// Resource ID.
    #[serde(rename = "resource_id")]
    pub resource_id: String,
    /// Temporary URL.
    #[serde(rename = "temp_url")]
    pub temp_url: String,
    /// Video width.
    #[serde(rename = "width")]
    pub width: i32,
    /// Video height.
    #[serde(rename = "height")]
    pub height: i32,
    /// Video duration in seconds.
    #[serde(rename = "duration")]
    pub duration: i32,
}

/// File message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentFileData {
    /// File ID.
    #[serde(rename = "file_id")]
    pub file_id: String,
    /// File name.
    #[serde(rename = "file_name")]
    pub file_name: String,
    /// File size in bytes.
    #[serde(rename = "file_size")]
    pub file_size: i64,
    /// TriSHA1 hash of the file, only present for private chat files.
    #[serde(rename = "file_hash", default, skip_serializing_if = "Option::is_none")]
    pub file_hash: Option<String>,
}

/// Merged forward message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentForwardData {
    /// Merged forward ID.
    #[serde(rename = "forward_id")]
    pub forward_id: String,
    /// Merged forward title.
    /// @since 1.1
    #[serde(rename = "title")]
    pub title: String,
    /// Merged forward preview text.
    /// @since 1.1
    #[serde(rename = "preview")]
    pub preview: Vec<String>,
    /// Merged forward summary.
    /// @since 1.1
    #[serde(rename = "summary")]
    pub summary: String,
}

/// Market emoji message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentMarketFaceData {
    /// Market emoji package ID.
    /// @since 1.1
    #[serde(rename = "emoji_package_id")]
    pub emoji_package_id: i32,
    /// Market emoji ID.
    /// @since 1.1
    #[serde(rename = "emoji_id")]
    pub emoji_id: String,
    /// Market emoji key.
    /// @since 1.1
    #[serde(rename = "key")]
    pub key: String,
    /// Market emoji preview text.
    /// @since 1.1
    #[serde(rename = "summary")]
    pub summary: String,
    /// Market emoji URL.
    #[serde(rename = "url")]
    pub url: String,
}

/// Light app (mini program) message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentLightAppData {
    /// Light app (mini program) name.
    #[serde(rename = "app_name")]
    pub app_name: String,
    /// Light app (mini program) JSON data.
    #[serde(rename = "json_payload")]
    pub json_payload: String,
}

/// XML message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentXmlData {
    /// Service ID.
    #[serde(rename = "service_id")]
    pub service_id: i32,
    /// XML data.
    #[serde(rename = "xml_payload")]
    pub xml_payload: String,
}

/// Markdown message segment data.
/// @since 1.3
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingSegmentMarkdownData {
    /// Markdown content.
    #[serde(rename = "content")]
    pub content: String,
}

/// Sent forwarded message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingForwardedMessage {
    /// Sender QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// Sender name.
    #[serde(rename = "sender_name")]
    pub sender_name: String,
    /// Message Unix timestamp in seconds.
    /// @since 1.3
    #[serde(rename = "time", default, skip_serializing_if = "Option::is_none")]
    pub time: Option<i64>,
    /// List of message segments.
    #[serde(
        rename = "segments",
        deserialize_with = "deserialize_drop_bad_outgoing_segment_list"
    )]
    pub segments: Vec<OutgoingSegment>,
}

/// Sent message segment.
#[derive(Debug, Clone, PartialEq)]
pub enum OutgoingSegment {
    /// Text message segment.
    Text(String),
    /// Mention message segment.
    Mention(i64),
    /// Mention-all message segment.
    MentionAll,
    /// Emoji message segment.
    Face(OutgoingSegmentFaceData),
    /// Reply message segment.
    Reply(i64),
    /// Image message segment.
    Image(OutgoingSegmentImageData),
    /// Voice message segment.
    Record(String),
    /// Video message segment.
    Video(OutgoingSegmentVideoData),
    /// Merged forward message segment.
    Forward(OutgoingSegmentForwardData),
    /// Light app (mini program) message segment.
    /// @since 1.2
    LightApp(String),
}

impl Serialize for OutgoingSegment {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            OutgoingSegment::Text(text) => serialize_segment_with_data(
                serializer,
                "text",
                &OutgoingSegmentTextData { text: text.clone() },
            ),
            OutgoingSegment::Mention(user_id) => serialize_segment_with_data(
                serializer,
                "mention",
                &OutgoingSegmentMentionData { user_id: *user_id },
            ),
            OutgoingSegment::MentionAll => serialize_segment_with_data(
                serializer,
                "mention_all",
                &OutgoingSegmentMentionAllData {},
            ),
            OutgoingSegment::Face(data) => serialize_segment_with_data(serializer, "face", data),
            OutgoingSegment::Reply(message_seq) => serialize_segment_with_data(
                serializer,
                "reply",
                &OutgoingSegmentReplyData {
                    message_seq: *message_seq,
                },
            ),
            OutgoingSegment::Image(data) => serialize_segment_with_data(serializer, "image", data),
            OutgoingSegment::Record(uri) => serialize_segment_with_data(
                serializer,
                "record",
                &OutgoingSegmentRecordData { uri: uri.clone() },
            ),
            OutgoingSegment::Video(data) => serialize_segment_with_data(serializer, "video", data),
            OutgoingSegment::Forward(data) => {
                serialize_segment_with_data(serializer, "forward", data)
            }
            OutgoingSegment::LightApp(json_payload) => serialize_segment_with_data(
                serializer,
                "light_app",
                &OutgoingSegmentLightAppData {
                    json_payload: json_payload.clone(),
                },
            ),
        }
    }
}

impl<'de> Deserialize<'de> for OutgoingSegment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match deserialize_segment_type::<D::Error>(&value)? {
            "text" => Ok(Self::Text(
                deserialize_segment_data::<D::Error, OutgoingSegmentTextData>(&value)?.text,
            )),
            "mention" => Ok(Self::Mention(
                deserialize_segment_data::<D::Error, OutgoingSegmentMentionData>(&value)?.user_id,
            )),
            "mention_all" => {
                let _: OutgoingSegmentMentionAllData = deserialize_segment_data(&value)?;
                Ok(Self::MentionAll)
            }
            "face" => Ok(Self::Face(deserialize_segment_data(&value)?)),
            "reply" => Ok(Self::Reply(
                deserialize_segment_data::<D::Error, OutgoingSegmentReplyData>(&value)?.message_seq,
            )),
            "image" => Ok(Self::Image(deserialize_segment_data(&value)?)),
            "record" => Ok(Self::Record(
                deserialize_segment_data::<D::Error, OutgoingSegmentRecordData>(&value)?.uri,
            )),
            "video" => Ok(Self::Video(deserialize_segment_data(&value)?)),
            "forward" => Ok(Self::Forward(deserialize_segment_data(&value)?)),
            "light_app" => Ok(Self::LightApp(
                deserialize_segment_data::<D::Error, OutgoingSegmentLightAppData>(&value)?
                    .json_payload,
            )),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &[
                    "text",
                    "mention",
                    "mention_all",
                    "face",
                    "reply",
                    "image",
                    "record",
                    "video",
                    "forward",
                    "light_app",
                ],
            )),
        }
    }
}

/// Text message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingSegmentTextData {
    /// Text content.
    #[serde(rename = "text")]
    pub text: String,
}

/// Mention message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingSegmentMentionData {
    /// Mentioned QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
}

/// Mention-all message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingSegmentMentionAllData {}

/// Emoji message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingSegmentFaceData {
    /// Emoji ID.
    #[serde(rename = "face_id")]
    pub face_id: String,
    /// Whether it is a super emoji.
    /// @since 1.1
    #[serde(
        rename = "is_large",
        default = "default_outgoing_segment_face_data_is_large",
        deserialize_with = "deserialize_outgoing_segment_face_data_is_large"
    )]
    pub is_large: bool,
}

/// Reply message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingSegmentReplyData {
    /// Sequence number of the quoted message.
    #[serde(rename = "message_seq")]
    pub message_seq: i64,
}

/// Image message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingSegmentImageData {
    /// File URI, supporting the `file://`, `http(s)://`, and `base64://` formats.
    #[serde(rename = "uri")]
    pub uri: String,
    /// Image type.
    #[serde(
        rename = "sub_type",
        default = "default_outgoing_segment_image_data_sub_type",
        deserialize_with = "deserialize_outgoing_segment_image_data_sub_type"
    )]
    pub sub_type: String,
    /// Image preview text.
    #[serde(rename = "summary", default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Voice message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingSegmentRecordData {
    /// File URI, supporting the `file://`, `http(s)://`, and `base64://` formats.
    #[serde(rename = "uri")]
    pub uri: String,
}

/// Video message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingSegmentVideoData {
    /// File URI, supporting the `file://`, `http(s)://`, and `base64://` formats.
    #[serde(rename = "uri")]
    pub uri: String,
    /// Cover image URI.
    #[serde(rename = "thumb_uri", default, skip_serializing_if = "Option::is_none")]
    pub thumb_uri: Option<String>,
}

/// Merged forward message segment data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingSegmentForwardData {
    /// Merged forward message content.
    #[serde(rename = "messages")]
    pub messages: Vec<OutgoingForwardedMessage>,
    /// Merged forward title.
    /// @since 1.2
    #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Merged forward preview text; if provided, at least 1 and at most 4 entries.
    /// @since 1.2
    #[serde(rename = "preview", default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<Vec<String>>,
    /// Merged forward summary.
    /// @since 1.2
    #[serde(rename = "summary", default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Preview display text of the merged forward, effective only on mobile QQ.
    /// @since 1.2
    #[serde(rename = "prompt", default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// Light app (mini program) message segment data.
/// @since 1.2
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingSegmentLightAppData {
    /// Light app (mini program) JSON data.
    #[serde(rename = "json_payload")]
    pub json_payload: String,
}

// ####################################
// API Input and Output Structs
// ####################################
