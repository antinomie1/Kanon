//! What was said in a group since the bot last looked.
//!
//! With observation on, every group message an instance sees is recorded here — answered or not —
//! together with the bot's own replies. When the bot is addressed, the model is shown exactly the
//! entries its session has not seen yet, as leading text of the current turn, and they are marked
//! seen. So each line enters a session's append-only history once, never as a sliding window, and
//! the cached request prefix is untouched.
//!
//! The log is bounded by count and age and lives in memory: after a restart the bot simply starts
//! observing again.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Lines kept per group.
const CAPACITY: usize = 30;

/// Lines older than this are no longer context worth sending.
const MAX_AGE: Duration = Duration::from_secs(30 * 60);

/// Characters kept of one line.
const MAX_LINE_CHARS: usize = 200;

/// One recorded line.
#[derive(Debug)]
struct Entry {
    seq: u64,
    at: Instant,
    speaker: String,
    text: String,
}

/// One group's recent lines and how far each session has read them.
#[derive(Debug, Default)]
struct Channel {
    entries: VecDeque<Entry>,
    /// Session key to the last sequence number that session has seen.
    cursors: HashMap<String, u64>,
}

#[derive(Debug, Default)]
struct Inner {
    channels: HashMap<String, Channel>,
    next_seq: u64,
}

/// Recent group lines, keyed by `<platform>\u{1f}<channel>`.
#[derive(Debug, Default)]
pub struct GroupLog {
    inner: Mutex<Inner>,
}

impl GroupLog {
    /// Records a line and returns its sequence number.
    pub fn record(&self, channel: &str, speaker: &str, text: &str) -> u64 {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.next_seq += 1;
        let seq = inner.next_seq;
        let log = inner.channels.entry(channel.to_owned()).or_default();
        log.entries.push_back(Entry {
            seq,
            at: Instant::now(),
            speaker: speaker.to_owned(),
            text: text.trim().chars().take(MAX_LINE_CHARS).collect(),
        });
        while log.entries.len() > CAPACITY {
            log.entries.pop_front();
        }
        seq
    }

    /// Returns `(speaker, text)` for every recent line `session` has not seen, oldest first.
    pub fn unseen(&self, channel: &str, session: &str) -> Vec<(String, String)> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Some(log) = inner.channels.get_mut(channel) else {
            return Vec::new();
        };
        let now = Instant::now();
        while log
            .entries
            .front()
            .is_some_and(|entry| now.duration_since(entry.at) > MAX_AGE)
        {
            log.entries.pop_front();
        }
        if log.entries.is_empty() {
            inner.channels.remove(channel);
            return Vec::new();
        }
        let seen = log.cursors.get(session).copied().unwrap_or(0);
        log.entries
            .iter()
            .filter(|entry| entry.seq > seen && !entry.text.is_empty())
            .map(|entry| (entry.speaker.clone(), entry.text.clone()))
            .collect()
    }

    /// Marks every line up to `seq` as seen by `session`.
    pub fn mark_seen(&self, channel: &str, session: &str, seq: u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(log) = inner.channels.get_mut(channel) {
            let cursor = log.cursors.entry(session.to_owned()).or_default();
            *cursor = (*cursor).max(seq);
        }
    }
}
