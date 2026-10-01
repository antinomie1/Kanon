//! Delivery-only line splitting for model replies.

use kanon_proto::v1::MessageSegment;
use kanon_proto::v1::message_segment::Segment;

/// Splits only answer text, keeping a quote on the first line and trailing media on the last.
///
/// A whitespace-only line carries no visible content and must not become a delivery. Preserve
/// whitespace on nonblank lines (including indentation), and never duplicate an attachment or
/// emit a quote by itself. Excess lines are joined in the last message to respect the adapter's
/// passive-reply budget. All resulting messages are delivered in the same queue slot.
pub(super) fn split_reply_lines(
    replies: &[MessageSegment],
    limit: usize,
) -> Vec<Vec<MessageSegment>> {
    let limit = limit.max(1);
    let mut messages: Vec<Vec<MessageSegment>> = Vec::new();
    let mut pending = Vec::new();

    for reply in replies {
        if let Some(Segment::Text(text)) = &reply.segment {
            for line in text.content.lines().filter(|line| !line.trim().is_empty()) {
                if messages.len() == limit {
                    let last = messages.last_mut().expect("positive reply limit");
                    last.append(&mut pending);
                    if let Some(MessageSegment {
                        segment: Some(Segment::Text(text)),
                    }) = last.last_mut()
                    {
                        text.content.push('\n');
                        text.content.push_str(line);
                    } else {
                        last.push(MessageSegment {
                            segment: Some(Segment::Text(kanon_proto::v1::TextSegment {
                                content: line.to_string(),
                            })),
                        });
                    }
                    continue;
                }
                pending.push(MessageSegment {
                    segment: Some(Segment::Text(kanon_proto::v1::TextSegment {
                        content: line.to_string(),
                    })),
                });
                messages.push(std::mem::take(&mut pending));
            }
        } else {
            pending.push(reply.clone());
        }
    }

    if let Some(last) = messages.last_mut() {
        last.extend(pending);
    } else if pending
        .iter()
        .any(|reply| !matches!(reply.segment.as_ref(), Some(Segment::Reply(_)) | None))
    {
        // An image-only reply remains deliverable even when every text line was blank.
        messages.push(pending);
    }
    messages
}
