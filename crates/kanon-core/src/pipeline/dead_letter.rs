//! Dead-letter queue (DLQ) persistent storage for messages the node accepted but could not handle.
//!
//! When platform delivery encounters hard failures, queue saturation, or tripped circuit
//! breakers, dropped replies are serialized to a cold-storage append-only file
//! (`data/dead_letter/<platform>_<date>.jsonl`). Inbound events the node acknowledged but never
//! processed (because it shut down first) land in the same file, marked `"direction": "inbound"`.
//! This guarantees auditability, zero data loss, and post-incident manual or automated replay.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{DeliverMessageRequest, MessageSegment, PipelineEventRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Directory holding the dead-letter records, relative to the node working directory.
pub const DEFAULT_DEAD_LETTER_DIR: &str = "./data/dead_letter";

/// Which way a dead-lettered message was travelling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeadLetterDirection {
    /// A reply that could not be delivered to its platform.
    #[default]
    Outbound,
    /// An event the node acknowledged but never processed.
    Inbound,
}

/// Serialized payload representing a dropped message written to cold storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadLetterRecord {
    /// Associated event identifier; assigned or deterministically generated.
    pub event_id: String,
    /// Unix timestamp in milliseconds when the failure was recorded.
    pub timestamp_ms: u64,
    /// UTC human-readable timestamp (ISO 8601 representation).
    pub iso_time: String,
    /// Whether this is an undelivered reply or an unprocessed event. Records written before the
    /// field existed are all replies, hence the default.
    #[serde(default)]
    pub direction: DeadLetterDirection,
    /// Chat platform the message belongs to.
    pub platform: String,
    /// Platform channel identifier.
    pub channel_id: String,
    /// Recipient of an outbound reply; empty for an inbound event.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recipient_id: String,
    /// Sender of an inbound event; empty for an outbound reply.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sender_id: String,
    /// Explicit failure reason or circuit breaker state causing the message drop.
    pub reason: String,
    /// Serialized message segments.
    pub segments: Vec<Value>,
}

/// Persistent dead letter writer appending failed messages to partitioned JSONL files.
#[derive(Debug, Clone)]
pub struct DeadLetterWriter {
    base_dir: PathBuf,
}

impl Default for DeadLetterWriter {
    fn default() -> Self {
        Self::new(DEFAULT_DEAD_LETTER_DIR)
    }
}

impl DeadLetterWriter {
    /// Creates a new writer storing records in the specified base directory.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// Returns the active base directory.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Appends a dropped outbound request to the platform's daily `.jsonl` cold storage file.
    ///
    /// # File Partitioning
    /// Records are partitioned by platform and calendar date (UTC):
    /// `<base_dir>/<platform>_<YYYY-MM-DD>.jsonl`
    pub async fn write_record(
        &self,
        request: &DeliverMessageRequest,
        reason: &str,
    ) -> std::io::Result<PathBuf> {
        let (timestamp_ms, iso_time, date_str) = current_time_triplet();
        let event_id = if request.event_id.is_empty() {
            format!("dead-letter-{}-{}", timestamp_ms, request.channel_id)
        } else {
            request.event_id.clone()
        };

        let segments: Vec<Value> = request.segments.iter().map(segment_to_json).collect();

        self.append(
            DeadLetterRecord {
                event_id,
                timestamp_ms,
                iso_time,
                direction: DeadLetterDirection::Outbound,
                platform: request.platform.clone(),
                channel_id: request.channel_id.clone(),
                recipient_id: request.recipient_id.clone(),
                sender_id: String::new(),
                reason: reason.to_string(),
                segments,
            },
            &date_str,
        )
        .await
    }

    /// Appends an inbound event the node acknowledged but never processed.
    pub async fn write_inbound(
        &self,
        event: &PipelineEventRequest,
        reason: &str,
    ) -> std::io::Result<PathBuf> {
        let (timestamp_ms, iso_time, date_str) = current_time_triplet();
        self.append(
            DeadLetterRecord {
                event_id: event.event_id.clone(),
                timestamp_ms,
                iso_time,
                direction: DeadLetterDirection::Inbound,
                platform: event.platform.clone(),
                channel_id: event.channel_id.clone(),
                recipient_id: String::new(),
                sender_id: event.sender_id.clone(),
                reason: reason.to_string(),
                segments: event.segments.iter().map(segment_to_json).collect(),
            },
            &date_str,
        )
        .await
    }

    /// Appends one record to the platform's file for `date_str`.
    async fn append(&self, record: DeadLetterRecord, date_str: &str) -> std::io::Result<PathBuf> {
        let safe_platform = sanitize_filename(&record.platform);
        let filename = format!("{safe_platform}_{date_str}.jsonl");
        let file_path = self.base_dir.join(filename);

        // Ensure target directory exists before file creation
        if let Some(parent) = file_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let serialized = serde_json::to_string(&record).map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to serialize dead letter record: {err}"),
            )
        })?;

        // Atomic append with newline delimiter
        let line = format!("{serialized}\n");
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;

        tracing::warn!(
            platform = %record.platform,
            channel_id = %record.channel_id,
            event_id = %record.event_id,
            direction = ?record.direction,
            path = %file_path.display(),
            reason = %record.reason,
            "Dropped message written to cold storage dead-letter log"
        );

        Ok(file_path)
    }
}

/// Converts a protobuf [`MessageSegment`] to structured JSON for auditing.
fn segment_to_json(segment: &MessageSegment) -> Value {
    match segment.segment.as_ref() {
        Some(Segment::Text(t)) => serde_json::json!({
            "type": "text",
            "content": t.content,
        }),
        Some(Segment::Image(img)) => {
            // The location travels with the record: an operator diagnosing an undelivered picture
            // needs to know which file the adapter could not send (raw bytes stay out of the log).
            let (file_path, url) = match img.source.as_ref() {
                Some(kanon_proto::v1::image_segment::Source::FilePath(path)) => {
                    (Some(path.clone()), None)
                }
                Some(kanon_proto::v1::image_segment::Source::Url(url)) => (None, Some(url.clone())),
                _ => (None, None),
            };
            serde_json::json!({
                "type": "image",
                "mime_type": img.mime_type,
                "filename": img.filename,
                "file_path": file_path,
                "url": url,
                "has_bytes": img.source.as_ref().is_some_and(|s| matches!(s, kanon_proto::v1::image_segment::Source::RawBytes(_))),
            })
        }
        Some(Segment::Audio(aud)) => serde_json::json!({
            "type": "audio",
            "duration_seconds": aud.duration_seconds,
        }),
        Some(Segment::Video(video)) => serde_json::json!({
            "type": "video",
            "filename": video.filename,
            "url": match video.source.as_ref() {
                Some(kanon_proto::v1::video_segment::Source::Url(url)) => Some(url.clone()),
                _ => None,
            },
        }),
        Some(Segment::File(file)) => serde_json::json!({
            "type": "file",
            "name": file.name,
        }),
        Some(Segment::Face(face)) => serde_json::json!({
            "type": "face",
            "id": face.id,
        }),
        Some(Segment::Mention(m)) => serde_json::json!({
            "type": "mention",
            "target_user_id": m.target_user_id,
            "display_name": m.display_name,
            "is_all": m.is_all,
        }),
        Some(Segment::Reply(r)) => serde_json::json!({
            "type": "reply",
            "target_message_id": r.target_message_id,
            "snippet": r.snippet,
        }),
        Some(Segment::Custom(c)) => serde_json::json!({
            "type": "custom",
            "type_name": c.type_name,
        }),
        None => serde_json::json!({ "type": "unknown" }),
    }
}

/// Replaces invalid filesystem characters in platform identifiers.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Computes current Unix timestamp in ms, ISO 8601 representation, and calendar date YYYY-MM-DD.
fn current_time_triplet() -> (u64, String, String) {
    let now = SystemTime::now();
    let duration = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let total_secs = duration.as_secs();
    let millis = duration.as_millis() as u64;

    let date_str = unix_to_date_string(total_secs);
    let time_of_day_secs = total_secs % 86400;
    let hours = time_of_day_secs / 3600;
    let minutes = (time_of_day_secs % 3600) / 60;
    let seconds = time_of_day_secs % 60;

    let iso_time = format!("{date_str}T{hours:02}:{minutes:02}:{seconds:02}Z");
    (millis, iso_time, date_str)
}

/// Converts seconds since Unix epoch to civil UTC date `YYYY-MM-DD` using Howard Hinnant algorithm.
fn unix_to_date_string(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
