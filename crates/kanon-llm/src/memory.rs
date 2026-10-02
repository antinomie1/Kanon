//! Conversation memory: an append-only log per session.
//!
//! # Why history is never trimmed one message at a time
//! Providers cache a prompt by its prefix, so a conversation is cheap exactly as long as each
//! request *extends* the previous one. A sliding window breaks that: dropping the oldest message on
//! every turn shifts everything after it, so the whole history is re-read at full price on every
//! request. Memory therefore only ever **appends**.
//!
//! The one way history gets shorter is [`Memory::compact_history`]: the leading part of the log is
//! folded into a summary, once, when the context has grown large (see [`crate::compaction`]). The
//! prefix changes at that moment — one cache miss — and is then stable again until the next
//! compaction, instead of shifting on every turn.
//!
//! The summary lives beside the log, not inside it: history holds only what was said (user,
//! assistant, tool), and the summary is placed in the request's static system block where it
//! belongs.
//!
//! [`InMemory`] is the default backend, backed by a lock-sharded [`DashMap`]; the [`Memory`] trait
//! lets plugins swap in SQLite ([`crate::SqliteMemory`]), Redis, vector or remote stores.

use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::Arc;

use crate::error::MemoryError;
use crate::gateway::types::ChatMessage;

/// A consistent view of one session's memory.
///
/// Read as one value so a reader can never see the summary of one compaction next to the history of
/// another.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemorySnapshot {
    /// Summary of everything compacted away so far, if any compaction happened.
    pub summary: Option<String>,
    /// Messages since the last compaction, oldest first. Only user, assistant and tool messages.
    pub messages: Vec<ChatMessage>,
}

/// What a listing of stored sessions reports about one session (see [`Memory::list_sessions`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredSession {
    /// The session's key.
    pub session_key: String,
    /// User and assistant messages in the history; messages folded into the summary are gone.
    pub message_count: usize,
    /// Text of the earliest user message still in the history.
    pub first_user_message: Option<String>,
    /// Unix seconds of the newest stored message, when the backend records message times.
    pub last_message_at: Option<u64>,
}

/// Pluggable interface for conversational memory backends.
///
/// Implementations may store history in memory, relational databases, distributed caches, or
/// external memory microservices. The contract is *append-only* history: nothing but
/// [`Memory::compact_history`] and [`Memory::clear`] may remove or reorder messages. `clear` drops
/// a session as a whole (deleting a conversation); it never trims one.
#[async_trait]
pub trait Memory: Send + Sync {
    /// Appends a message to the specified session history.
    async fn push_message(
        &self,
        session_key: &str,
        message: ChatMessage,
    ) -> Result<(), MemoryError>;

    /// Appends multiple messages in sequence.
    async fn extend_messages(
        &self,
        session_key: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<(), MemoryError> {
        for msg in messages {
            self.push_message(session_key, msg).await?;
        }
        Ok(())
    }

    /// Reads the summary and the history of a session as one consistent value.
    async fn snapshot(&self, session_key: &str) -> Result<MemorySnapshot, MemoryError>;

    /// Retrieves the history of a session (without the summary).
    async fn get_messages(&self, session_key: &str) -> Result<Vec<ChatMessage>, MemoryError> {
        Ok(self.snapshot(session_key).await?.messages)
    }

    /// Folds the first `covered` messages of the history into `summary`.
    ///
    /// `summary` replaces any earlier summary — it was written from a prompt that already contained
    /// it. Messages appended after the caller took its snapshot are **kept**: only the covered
    /// prefix is removed, which is what lets a compaction run while the conversation continues.
    ///
    /// The operation is atomic: readers see either the old summary and history or the new ones, and
    /// a backend with transactions must not lose the previous state if it fails. Asking to cover
    /// more messages than exist is an error, not a clamp.
    async fn compact_history(
        &self,
        session_key: &str,
        covered: usize,
        summary: String,
    ) -> Result<(), MemoryError>;

    /// Clears history and summary for the specified session.
    async fn clear(&self, session_key: &str) -> Result<(), MemoryError>;

    /// Returns the count of active sessions tracked by this backend.
    async fn session_count(&self) -> Result<usize, MemoryError>;

    /// Lists the stored sessions whose key starts with `prefix`, ordered by key.
    ///
    /// This is how a chat's conversations are found: every conversation of a chat shares the
    /// chat's key prefix. A session without any stored message may be missing from the listing.
    async fn list_sessions(&self, prefix: &str) -> Result<Vec<StoredSession>, MemoryError>;
}

/// Counts the user and assistant messages of a history and finds its first user text.
///
/// Shared by backends that hold histories in memory, so every backend reports the same numbers.
pub fn describe_history(session_key: &str, messages: &[ChatMessage]) -> StoredSession {
    use crate::gateway::types::Role;
    StoredSession {
        session_key: session_key.to_string(),
        message_count: messages
            .iter()
            .filter(|message| matches!(message.role, Role::User | Role::Assistant))
            .count(),
        first_user_message: messages
            .iter()
            .find(|message| message.role == Role::User)
            .and_then(|message| message.content.clone()),
        last_message_at: None,
    }
}

/// In-memory state of one session.
#[derive(Debug, Clone, Default)]
pub struct SessionMemory {
    summary: Option<String>,
    messages: Vec<ChatMessage>,
}

impl SessionMemory {
    /// Creates an empty session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Restores a session from persisted parts.
    pub fn from_parts(summary: Option<String>, messages: Vec<ChatMessage>) -> Self {
        Self { summary, messages }
    }

    /// Appends a message.
    pub fn push_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
    }

    /// Appends several messages in order.
    pub fn extend_messages(&mut self, messages: impl IntoIterator<Item = ChatMessage>) {
        self.messages.extend(messages);
    }

    /// The summary, if a compaction happened.
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// The history, oldest first.
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Number of messages in the history.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the history is empty.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// A consistent copy of summary and history.
    pub fn snapshot(&self) -> MemorySnapshot {
        MemorySnapshot {
            summary: self.summary.clone(),
            messages: self.messages.clone(),
        }
    }

    /// Folds the first `covered` messages into `summary`, keeping the rest.
    pub fn compact(&mut self, covered: usize, summary: String) -> Result<(), MemoryError> {
        if covered > self.messages.len() {
            return Err(MemoryError::Backend(format!(
                "cannot compact {covered} messages: the history only holds {}",
                self.messages.len()
            )));
        }
        self.messages.drain(..covered);
        self.summary = Some(summary);
        Ok(())
    }
}

/// Default in-memory conversation memory.
///
/// Sessions are keyed by an opaque string (`channel_id:sender_id` by convention) in a concurrent
/// lock-sharded [`DashMap`], so sessions are read and updated independently without a global lock.
#[derive(Default)]
pub struct InMemory {
    /// Sharded concurrent map storing per-session state.
    sessions: DashMap<String, SessionMemory>,
}

impl InMemory {
    /// Creates an empty memory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Formats a standard Kanon composite session key from channel and sender IDs.
    pub fn make_session_key(channel_id: &str, sender_id: &str) -> String {
        format!("{channel_id}:{sender_id}")
    }

    /// Synchronous shortcut appending a message to a session (embedded use and tests).
    pub fn push_message(&self, session_key: &str, message: ChatMessage) {
        self.sessions
            .entry(session_key.to_string())
            .or_default()
            .push_message(message);
    }

    /// Synchronous shortcut reading a session's history.
    pub fn get_messages(&self, session_key: &str) -> Vec<ChatMessage> {
        self.sessions
            .get(session_key)
            .map(|session| session.messages().to_vec())
            .unwrap_or_default()
    }

    /// Synchronous shortcut clearing a session.
    pub fn clear(&self, session_key: &str) {
        self.sessions.remove(session_key);
    }

    /// Synchronous shortcut returning the number of sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

#[async_trait]
impl Memory for InMemory {
    async fn push_message(
        &self,
        session_key: &str,
        message: ChatMessage,
    ) -> Result<(), MemoryError> {
        InMemory::push_message(self, session_key, message);
        Ok(())
    }

    async fn snapshot(&self, session_key: &str) -> Result<MemorySnapshot, MemoryError> {
        Ok(self
            .sessions
            .get(session_key)
            .map(|session| session.snapshot())
            .unwrap_or_default())
    }

    async fn compact_history(
        &self,
        session_key: &str,
        covered: usize,
        summary: String,
    ) -> Result<(), MemoryError> {
        // The shard lock is held for the whole fold, so no reader observes a half-compacted state
        // and a concurrent push lands either before it (and is covered or kept) or after it.
        match self.sessions.get_mut(session_key) {
            Some(mut session) => session.compact(covered, summary),
            None if covered == 0 => Ok(()),
            None => Err(MemoryError::Backend(format!(
                "cannot compact {covered} messages of unknown session '{session_key}'"
            ))),
        }
    }

    async fn clear(&self, session_key: &str) -> Result<(), MemoryError> {
        InMemory::clear(self, session_key);
        Ok(())
    }

    async fn session_count(&self) -> Result<usize, MemoryError> {
        Ok(InMemory::session_count(self))
    }

    async fn list_sessions(&self, prefix: &str) -> Result<Vec<StoredSession>, MemoryError> {
        let mut sessions: Vec<StoredSession> = self
            .sessions
            .iter()
            .filter(|entry| entry.key().starts_with(prefix))
            .map(|entry| describe_history(entry.key(), entry.value().messages()))
            .collect();
        sessions.sort_by(|a, b| a.session_key.cmp(&b.session_key));
        Ok(sessions)
    }
}

#[async_trait]
impl Memory for Arc<dyn Memory> {
    async fn push_message(
        &self,
        session_key: &str,
        message: ChatMessage,
    ) -> Result<(), MemoryError> {
        (**self).push_message(session_key, message).await
    }

    async fn extend_messages(
        &self,
        session_key: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<(), MemoryError> {
        (**self).extend_messages(session_key, messages).await
    }

    async fn snapshot(&self, session_key: &str) -> Result<MemorySnapshot, MemoryError> {
        (**self).snapshot(session_key).await
    }

    async fn get_messages(&self, session_key: &str) -> Result<Vec<ChatMessage>, MemoryError> {
        (**self).get_messages(session_key).await
    }

    async fn compact_history(
        &self,
        session_key: &str,
        covered: usize,
        summary: String,
    ) -> Result<(), MemoryError> {
        (**self)
            .compact_history(session_key, covered, summary)
            .await
    }

    async fn clear(&self, session_key: &str) -> Result<(), MemoryError> {
        (**self).clear(session_key).await
    }

    async fn session_count(&self) -> Result<usize, MemoryError> {
        (**self).session_count().await
    }

    async fn list_sessions(&self, prefix: &str) -> Result<Vec<StoredSession>, MemoryError> {
        (**self).list_sessions(prefix).await
    }
}
