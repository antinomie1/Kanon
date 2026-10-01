//! Tool attachments become the segment kind their MIME type names, limited to what the target
//! adapter declares it can send.

use kanon_core::Capability;
use kanon_core::pipeline::attachment_segments;
use kanon_llm::ToolAttachment;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AudioSegment, FileSegment, ImageSegment, MessageSegment, VideoSegment, audio_segment,
    file_segment, image_segment, video_segment,
};

const ALL_MEDIA: &[Capability] = &[
    Capability::SendImage,
    Capability::SendVoice,
    Capability::SendVideo,
    Capability::SendFile,
];

fn local(mime: &str, path: &str) -> ToolAttachment {
    ToolAttachment {
        mime_type: mime.into(),
        file_path: Some(path.into()),
        url: None,
    }
}

fn remote(mime: &str, url: &str) -> ToolAttachment {
    ToolAttachment {
        mime_type: mime.into(),
        file_path: None,
        url: Some(url.into()),
    }
}

fn segment(segment: Segment) -> MessageSegment {
    MessageSegment {
        segment: Some(segment),
    }
}

#[test]
fn each_mime_type_becomes_the_matching_segment() {
    let media = attachment_segments(
        &[
            local("image/png", "/data/a/card.png"),
            remote("audio/mpeg", "https://cdn.example/tts/hello.mp3?sig=1"),
            local("video/mp4", "/data/a/clip.mp4"),
            local("application/pdf", "/data/a/1-2/report.pdf"),
            remote("application/pdf", "https://cdn.example/"),
        ],
        Some(ALL_MEDIA),
    );

    assert!(media.notes.is_empty());
    assert_eq!(
        media.segments,
        vec![
            segment(Segment::Image(ImageSegment {
                source: Some(image_segment::Source::FilePath("/data/a/card.png".into())),
                mime_type: Some("image/png".into()),
                filename: None,
            })),
            segment(Segment::Audio(AudioSegment {
                source: Some(audio_segment::Source::Url(
                    "https://cdn.example/tts/hello.mp3?sig=1".into()
                )),
                duration_seconds: None,
            })),
            segment(Segment::Video(VideoSegment {
                source: Some(video_segment::Source::FilePath("/data/a/clip.mp4".into())),
                mime_type: Some("video/mp4".into()),
                filename: Some("clip.mp4".into()),
            })),
            // A file is shown under its own name; a URL without one gets a generic name that
            // still opens with the right application.
            segment(Segment::File(FileSegment {
                source: Some(file_segment::Source::FilePath(
                    "/data/a/1-2/report.pdf".into()
                )),
                name: "report.pdf".into(),
            })),
            segment(Segment::File(FileSegment {
                source: Some(file_segment::Source::Url("https://cdn.example/".into())),
                name: "attachment.pdf".into(),
            })),
        ]
    );
}

#[test]
fn kinds_the_adapter_cannot_send_are_named_instead_of_failing_the_reply() {
    let attachments = [
        local("image/png", "/data/a/card.png"),
        local("application/pdf", "/data/a/report.pdf"),
    ];

    let media = attachment_segments(
        &attachments,
        Some(&[Capability::SendImage, Capability::SendVoice]),
    );
    assert_eq!(media.segments.len(), 1);
    assert!(matches!(media.segments[0].segment, Some(Segment::Image(_))));
    assert_eq!(media.notes, ["[未能发送文件 report.pdf：当前平台不支持]"]);

    // Without an adapter there is no declaration to apply; delivery reports the missing adapter.
    let media = attachment_segments(&attachments, None);
    assert_eq!(media.segments.len(), 2);
    assert!(media.notes.is_empty());
}
