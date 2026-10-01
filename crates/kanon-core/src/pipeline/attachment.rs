//! Outbound segments for the media a turn's tools produced.
//!
//! A tool attachment carries a MIME type, not a segment kind. This module picks the kind the MIME
//! type names — image, voice, video, or a named file for everything else — and keeps only the
//! kinds the target adapter declares it can send. A kind the platform cannot send is left out and
//! named in a short note: the user learns something is missing, and the rest of the reply still
//! goes out instead of the whole delivery failing on one segment the adapter would reject.

use kanon_llm::ToolAttachment;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AudioSegment, FileSegment, ImageSegment, MessageSegment, VideoSegment, audio_segment,
    file_segment, image_segment, video_segment,
};

use crate::adapter::Capability;

/// The kind of segment an attachment is delivered as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// `image/*`.
    Image,
    /// `audio/*`, delivered as a voice message.
    Voice,
    /// `video/*`.
    Video,
    /// Every other type, delivered as a named file.
    File,
}

impl MediaKind {
    /// Picks the kind a MIME type names; anything that is not image, audio or video is a file.
    pub fn of(mime_type: &str) -> Self {
        let mime = mime_type.trim().to_ascii_lowercase();
        if mime.starts_with("image/") {
            Self::Image
        } else if mime.starts_with("audio/") {
            Self::Voice
        } else if mime.starts_with("video/") {
            Self::Video
        } else {
            Self::File
        }
    }

    /// The capability an adapter declares to accept this kind.
    pub fn capability(self) -> Capability {
        match self {
            Self::Image => Capability::SendImage,
            Self::Voice => Capability::SendVoice,
            Self::Video => Capability::SendVideo,
            Self::File => Capability::SendFile,
        }
    }

    /// How the kind is named in the note for a left-out attachment.
    fn noun(self) -> &'static str {
        match self {
            Self::Image => "图片",
            Self::Voice => "语音",
            Self::Video => "视频",
            Self::File => "文件",
        }
    }
}

/// Segments to deliver for a turn's attachments, plus notes for the ones left out.
#[derive(Debug, Default, PartialEq)]
pub struct AttachmentSegments {
    /// One segment per deliverable attachment, in the order the tools produced them.
    pub segments: Vec<MessageSegment>,
    /// One user-facing line per attachment the platform cannot send.
    pub notes: Vec<String>,
}

/// Builds the outbound segments for `attachments`.
///
/// `supported` is the target adapter's capability list, or `None` when no adapter serves the
/// platform; then nothing is filtered, and delivery itself reports the missing adapter.
pub fn attachment_segments(
    attachments: &[ToolAttachment],
    supported: Option<&[Capability]>,
) -> AttachmentSegments {
    let mut out = AttachmentSegments::default();
    for attachment in attachments {
        let Some(source) = Source::of(attachment) else {
            tracing::warn!(
                mime_type = %attachment.mime_type,
                "Tool attachment has neither a file path nor a URL; dropping it"
            );
            continue;
        };
        let kind = MediaKind::of(&attachment.mime_type);
        let name = display_name(attachment);
        if supported.is_some_and(|declared| !declared.contains(&kind.capability())) {
            tracing::warn!(
                mime_type = %attachment.mime_type,
                capability = ?kind.capability(),
                "Target adapter cannot send this attachment kind; leaving it out of the reply"
            );
            out.notes
                .push(format!("[未能发送{} {name}：当前平台不支持]", kind.noun()));
            continue;
        }
        out.segments.push(segment(kind, source, attachment, name));
    }
    out
}

/// Where an attachment's bytes are, in the order the core prefers them.
enum Source<'a> {
    /// A file the core or a plugin wrote; the adapter reads or uploads it.
    Path(&'a str),
    /// A remote resource the platform fetches itself.
    Url(&'a str),
}

impl<'a> Source<'a> {
    /// A local file wins over a URL: it is already materialized and cannot expire.
    fn of(attachment: &'a ToolAttachment) -> Option<Self> {
        match (&attachment.file_path, &attachment.url) {
            (Some(path), _) => Some(Self::Path(path)),
            (None, Some(url)) => Some(Self::Url(url)),
            (None, None) => None,
        }
    }
}

/// Builds the segment of `kind` for one attachment.
///
/// Images carry no file name: Milky shows an image's name as its preview text, where a generated
/// file name would only be noise.
fn segment(
    kind: MediaKind,
    source: Source<'_>,
    attachment: &ToolAttachment,
    name: String,
) -> MessageSegment {
    let mime_type = Some(attachment.mime_type.clone());
    let segment = match kind {
        MediaKind::Image => Segment::Image(ImageSegment {
            source: Some(match source {
                Source::Path(path) => image_segment::Source::FilePath(path.to_string()),
                Source::Url(url) => image_segment::Source::Url(url.to_string()),
            }),
            mime_type,
            filename: None,
        }),
        MediaKind::Voice => Segment::Audio(AudioSegment {
            source: Some(match source {
                Source::Path(path) => audio_segment::Source::FilePath(path.to_string()),
                Source::Url(url) => audio_segment::Source::Url(url.to_string()),
            }),
            duration_seconds: None,
        }),
        MediaKind::Video => Segment::Video(VideoSegment {
            source: Some(match source {
                Source::Path(path) => video_segment::Source::FilePath(path.to_string()),
                Source::Url(url) => video_segment::Source::Url(url.to_string()),
            }),
            mime_type,
            filename: Some(name),
        }),
        MediaKind::File => Segment::File(FileSegment {
            source: Some(match source {
                Source::Path(path) => file_segment::Source::FilePath(path.to_string()),
                Source::Url(url) => file_segment::Source::Url(url.to_string()),
            }),
            name,
        }),
    };
    MessageSegment {
        segment: Some(segment),
    }
}

/// The name a recipient sees: the file's own name, the last component of the URL path, or a
/// generic name carrying the MIME type's extension.
///
/// Tools choose the name by naming the file they write, so the attachment contract needs no
/// separate name field.
fn display_name(attachment: &ToolAttachment) -> String {
    let from_path = attachment.file_path.as_deref().and_then(|path| {
        std::path::Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    let from_url = || {
        let url = attachment.url.as_deref()?;
        let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
        let path = rest.split(['?', '#']).next().unwrap_or_default();
        // Everything before the first slash is the host, which is not a file name.
        let (_, path) = path.split_once('/')?;
        path.rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .map(str::to_string)
    };
    from_path.or_else(from_url).unwrap_or_else(|| {
        format!(
            "attachment.{}",
            crate::mcp::extension_for_mime(&attachment.mime_type)
        )
    })
}
