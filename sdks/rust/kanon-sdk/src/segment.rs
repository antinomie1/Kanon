//! Builders for outbound message segments, and [`IntoReply`] for "anything a handler may answer
//! with".
//!
//! ```ignore
//! use kanon_sdk::segment;
//! let reply = vec![segment::quote(&event_id), segment::text("pong")];
//! ```

use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AudioSegment, FaceSegment, FileSegment, ImageSegment, MentionSegment, MessageSegment,
    ReplySegment, TextSegment, VideoSegment, audio_segment, file_segment, image_segment,
    video_segment,
};

fn wrap(segment: Segment) -> MessageSegment {
    MessageSegment {
        segment: Some(segment),
    }
}

/// Plain text.
pub fn text(content: impl Into<String>) -> MessageSegment {
    wrap(Segment::Text(TextSegment {
        content: content.into(),
    }))
}

/// An image at a remote URL.
pub fn image_url(url: impl Into<String>) -> MessageSegment {
    image(image_segment::Source::Url(url.into()))
}

/// An image the adapter reads from a local file.
pub fn image_file(path: impl Into<String>) -> MessageSegment {
    image(image_segment::Source::FilePath(path.into()))
}

/// An image from bytes in memory.
pub fn image_bytes(data: Vec<u8>, mime_type: impl Into<String>) -> MessageSegment {
    wrap(Segment::Image(ImageSegment {
        source: Some(image_segment::Source::RawBytes(data)),
        mime_type: Some(mime_type.into()),
        filename: None,
    }))
}

fn image(source: image_segment::Source) -> MessageSegment {
    wrap(Segment::Image(ImageSegment {
        source: Some(source),
        mime_type: None,
        filename: None,
    }))
}

/// A voice message at a remote URL.
pub fn audio_url(url: impl Into<String>) -> MessageSegment {
    audio(audio_segment::Source::Url(url.into()))
}

/// A voice message the adapter reads from a local file.
pub fn audio_file(path: impl Into<String>) -> MessageSegment {
    audio(audio_segment::Source::FilePath(path.into()))
}

/// A voice message from bytes in memory.
pub fn audio_bytes(data: Vec<u8>) -> MessageSegment {
    audio(audio_segment::Source::RawBytes(data))
}

fn audio(source: audio_segment::Source) -> MessageSegment {
    wrap(Segment::Audio(AudioSegment {
        source: Some(source),
        duration_seconds: None,
    }))
}

/// A video at a remote URL.
pub fn video_url(url: impl Into<String>) -> MessageSegment {
    video(video_segment::Source::Url(url.into()))
}

/// A video the adapter reads from a local file.
pub fn video_file(path: impl Into<String>) -> MessageSegment {
    video(video_segment::Source::FilePath(path.into()))
}

fn video(source: video_segment::Source) -> MessageSegment {
    wrap(Segment::Video(VideoSegment {
        source: Some(source),
        mime_type: None,
        filename: None,
    }))
}

/// A file attachment at a remote URL; `name` is what the recipient sees.
pub fn file_url(name: impl Into<String>, url: impl Into<String>) -> MessageSegment {
    file(name, file_segment::Source::Url(url.into()))
}

/// A file attachment the adapter reads from a local path; `name` is what the recipient sees.
pub fn file_path(name: impl Into<String>, path: impl Into<String>) -> MessageSegment {
    file(name, file_segment::Source::FilePath(path.into()))
}

/// A file attachment from bytes in memory; `name` is what the recipient sees.
pub fn file_bytes(name: impl Into<String>, data: Vec<u8>) -> MessageSegment {
    file(name, file_segment::Source::RawBytes(data))
}

fn file(name: impl Into<String>, source: file_segment::Source) -> MessageSegment {
    wrap(Segment::File(FileSegment {
        source: Some(source),
        name: name.into(),
    }))
}

/// A platform emoji by its platform id (e.g. a QQ face id).
pub fn face(id: impl Into<String>) -> MessageSegment {
    wrap(Segment::Face(FaceSegment { id: id.into() }))
}

/// An @-mention of one user.
pub fn mention(user_id: impl Into<String>) -> MessageSegment {
    wrap(Segment::Mention(MentionSegment {
        target_user_id: user_id.into(),
        display_name: String::new(),
        is_all: false,
    }))
}

/// An @-mention of everyone in the group.
pub fn mention_all() -> MessageSegment {
    wrap(Segment::Mention(MentionSegment {
        target_user_id: String::new(),
        display_name: String::new(),
        is_all: true,
    }))
}

/// A quote of the message with `event_id` (sent as the platform's native reply).
pub fn quote(event_id: impl Into<String>) -> MessageSegment {
    wrap(Segment::Reply(ReplySegment {
        target_message_id: event_id.into(),
        snippet: String::new(),
    }))
}

/// Anything a handler may answer with: text, one segment, a list of segments, or nothing.
pub trait IntoReply {
    /// The segments of the reply; empty means "no reply".
    fn into_segments(self) -> Vec<MessageSegment>;
}

impl IntoReply for () {
    fn into_segments(self) -> Vec<MessageSegment> {
        Vec::new()
    }
}

impl IntoReply for String {
    fn into_segments(self) -> Vec<MessageSegment> {
        vec![text(self)]
    }
}

impl IntoReply for &str {
    fn into_segments(self) -> Vec<MessageSegment> {
        vec![text(self)]
    }
}

impl IntoReply for MessageSegment {
    fn into_segments(self) -> Vec<MessageSegment> {
        vec![self]
    }
}

impl IntoReply for Vec<MessageSegment> {
    fn into_segments(self) -> Vec<MessageSegment> {
        self
    }
}

impl<T: IntoReply> IntoReply for Option<T> {
    fn into_segments(self) -> Vec<MessageSegment> {
        self.map(IntoReply::into_segments).unwrap_or_default()
    }
}
