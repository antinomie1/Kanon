//! Conversation captures: a plugin's claim on a sender's next message.
//!
//! Multi-turn plugin interactions ("guess a number", "reply with the code you received") need the
//! *next* message of one person to reach the plugin that asked the question, instead of the
//! command router or the model. The plugin cannot simply wait for it inside its command handler:
//! the pipeline worker processes events one at a time, so a handler blocking on the next message
//! would block that very message. Instead the handler answers with `capture_seconds`, the core
//! remembers the claim here, and the next matching event is dispatched to the plugin as a
//! continuation.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kanon_proto::v1::PipelineEventRequest;

/// Longest capture a plugin may request; a forgotten capture must not swallow a person's messages
/// for longer than this.
pub const MAX_CAPTURE: Duration = Duration::from_secs(600);

/// Above this many entries expired captures are swept on insert, bounding memory.
const SWEEP_THRESHOLD: usize = 1024;

/// One pending capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// Host that owns the capturing plugin. Stored by id, not by handle, so a restarted or
    /// disabled host is noticed instead of being called through a stale connection.
    pub host_id: String,
    /// Plugin that asked for the capture.
    pub plugin_id: String,
    /// Command (or trigger) name the continuation is reported under.
    pub command: String,
    expires: Instant,
}

/// Identity of a conversation as seen by a capture: platform, channel and sender.
type CaptureKey = (String, String, String);

/// Pending captures, keyed by platform, channel and sender.
#[derive(Debug, Default)]
pub struct CaptureRegistry {
    entries: Mutex<HashMap<CaptureKey, Capture>>,
}

impl CaptureRegistry {
    /// Records that the next message of `event`'s sender belongs to `plugin_id`.
    ///
    /// Returns `false` (and records nothing) when the event names no sender, since there would
    /// be no way to tell whose next message to capture; a request above [`MAX_CAPTURE`] is
    /// clamped to it.
    pub fn capture(
        &self,
        event: &PipelineEventRequest,
        host_id: &str,
        plugin_id: &str,
        command: &str,
        seconds: u32,
    ) -> bool {
        if seconds == 0 || event.sender_id.trim().is_empty() {
            return false;
        }
        let duration = Duration::from_secs(u64::from(seconds)).min(MAX_CAPTURE);
        let now = Instant::now();
        let mut entries = self.lock();
        if entries.len() >= SWEEP_THRESHOLD {
            entries.retain(|_, capture| capture.expires > now);
        }
        entries.insert(
            key(event),
            Capture {
                host_id: host_id.to_string(),
                plugin_id: plugin_id.to_string(),
                command: command.to_string(),
                expires: now + duration,
            },
        );
        true
    }

    /// Removes and returns the live capture for `event`'s sender, if any.
    ///
    /// The capture is consumed whether or not the caller can honour it: a capture covers exactly
    /// one message, so a failed delivery must not leave the sender stuck.
    pub fn take(&self, event: &PipelineEventRequest) -> Option<Capture> {
        let capture = self.lock().remove(&key(event))?;
        (capture.expires > Instant::now()).then_some(capture)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CaptureKey, Capture>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn key(event: &PipelineEventRequest) -> CaptureKey {
    (
        event.platform.clone(),
        event.channel_id.clone(),
        event.sender_id.clone(),
    )
}
