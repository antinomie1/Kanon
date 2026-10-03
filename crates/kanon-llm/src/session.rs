//! Session Scoping, Lifecycle Management, and Metadata Tracking.
//!
//! Provides a unified session abstraction across multiple chat platforms:
//! - Multi-scope key resolution ([`SessionScope`] & [`SessionKey`]);
//! - Session lifecycle status ([`SessionStatus`]);
//! - Metadata tracking (turns, cumulative tokens, created/last active timestamps);
//! - Dynamic session variables store;
//! - Idle session sweeping and graceful session resets;
//! - Optional durability through a [`SessionStore`], so a conversation continues after the node
//!   restarts: the history lives in [`Memory`], and everything else about the session (persona
//!   binding, counters, status) is written through to the store on every change.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use crate::error::MemoryError;
use crate::memory::Memory;

/// Generates current Unix timestamp in seconds.
fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Scope boundary for session isolation.
///
/// Determines how conversation history and session state are partitioned
/// across platforms, channels, users, and threads.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionScope {
    /// Isolated per individual user globally across all channels (`user:{user_id}`).
    User,
    /// Shared by all users within a single channel/group (`channel:{channel_id}`).
    Channel,
    /// Isolated per user within a specific channel (`channel:{channel_id}:user:{user_id}`).
    /// This is the standard default for group chat environments.
    ChannelUser,
    /// Isolated per message thread within a channel (`channel:{channel_id}:thread:{thread_id}`).
    Thread,
    /// Custom user-defined scoping key.
    Custom(String),
}

/// Strongly typed session identifier with scope semantics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionKey {
    raw: String,
    scope: SessionScope,
}

impl SessionKey {
    /// Constructs a global user-scoped session key (`user:{user_id}`).
    pub fn user(user_id: &str) -> Self {
        Self {
            raw: format!("user:{user_id}"),
            scope: SessionScope::User,
        }
    }

    /// Constructs a shared channel-scoped session key (`channel:{channel_id}`).
    pub fn channel(channel_id: &str) -> Self {
        Self {
            raw: format!("channel:{channel_id}"),
            scope: SessionScope::Channel,
        }
    }

    /// Constructs a per-user-in-channel session key (`channel:{channel_id}:user:{user_id}`).
    pub fn channel_user(channel_id: &str, user_id: &str) -> Self {
        Self {
            raw: format!("channel:{channel_id}:user:{user_id}"),
            scope: SessionScope::ChannelUser,
        }
    }

    /// Constructs a thread-scoped session key (`channel:{channel_id}:thread:{thread_id}`).
    pub fn thread(channel_id: &str, thread_id: &str) -> Self {
        Self {
            raw: format!("channel:{channel_id}:thread:{thread_id}"),
            scope: SessionScope::Thread,
        }
    }

    /// Constructs a custom session key.
    pub fn custom(key: impl Into<String>) -> Self {
        let raw = key.into();
        Self {
            scope: SessionScope::Custom(raw.clone()),
            raw,
        }
    }

    /// Parses a legacy colon-delimited string (e.g. `channel_id:sender_id`) or custom key.
    pub fn from_legacy(channel_id: &str, sender_id: &str) -> Self {
        Self {
            raw: format!("{channel_id}:{sender_id}"),
            scope: SessionScope::ChannelUser,
        }
    }

    /// Returns the raw session identifier string.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Returns the session scoping category.
    pub fn scope(&self) -> &SessionScope {
        &self.scope
    }
}

impl std::fmt::Display for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.raw)
    }
}

impl AsRef<str> for SessionKey {
    fn as_ref(&self) -> &str {
        &self.raw
    }
}

impl From<&str> for SessionKey {
    fn from(s: &str) -> Self {
        SessionKey::custom(s)
    }
}

impl From<String> for SessionKey {
    fn from(s: String) -> Self {
        SessionKey::custom(s)
    }
}

/// Operational state of a conversation session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SessionStatus {
    /// Session is currently active and accepting messages.
    #[default]
    Active,
    /// Session has been inactive beyond the soft idle duration.
    Idle,
    /// Session has been explicitly concluded or archived.
    Closed,
    /// Session data is permanently archived.
    Archived,
}

/// Ephemeral runtime metadata and state attributes associated with an active conversation session.
///
/// **Boundary & Persistence Semantics**:
/// Conversational messages and summaries are owned by a [`Memory`] backend (durable with
/// [`SqliteMemory`](crate::SqliteMemory)); this record — persona binding, variables, turn tallies,
/// status — is owned by [`SessionManager`], which keeps it in memory and, when it has a
/// [`SessionStore`], writes every change through so the session survives a restart intact.
///
/// The [`RuntimeSessionMetadata`] type alias is kept for call sites that want to say "the record
/// the manager tracks" rather than "a stored document".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    /// Unique session identifier key.
    pub session_key: String,
    /// Scoping boundary of this session.
    pub scope: SessionScope,
    /// Current lifecycle status.
    pub status: SessionStatus,
    /// Epoch timestamp (seconds) when session was created.
    pub created_at: u64,
    /// Epoch timestamp (seconds) of last interaction.
    pub last_active_at: u64,
    /// Number of message interaction turns executed.
    pub turn_count: usize,
    /// Cumulative token usage tracked across this session.
    pub total_tokens_used: usize,
    /// Identifier of the active persona bound to this session, if configured.
    pub persona_id: Option<String>,
    /// Arbitrary session-scoped state variables (e.g. user language, preferences).
    pub variables: HashMap<String, String>,
}

impl SessionMetadata {
    /// Creates a new `SessionMetadata` initialized with the current timestamp.
    pub fn new(session_key: impl Into<String>, scope: SessionScope) -> Self {
        let now = current_unix_timestamp();
        Self {
            session_key: session_key.into(),
            scope,
            status: SessionStatus::Active,
            created_at: now,
            last_active_at: now,
            turn_count: 0,
            total_tokens_used: 0,
            persona_id: None,
            variables: HashMap::new(),
        }
    }

    /// Evaluates if the session has been idle longer than the specified timeout duration.
    pub fn is_idle(&self, max_idle: Duration) -> bool {
        let idle_threshold = max_idle.as_secs();
        current_unix_timestamp().saturating_sub(self.last_active_at) >= idle_threshold
    }
}

/// Alias for the session record a [`SessionManager`] tracks.
pub type RuntimeSessionMetadata = SessionMetadata;

/// Durable copy of session metadata.
///
/// The store is synchronous on purpose: a write is one small row, and keeping it synchronous lets
/// every [`SessionManager`] mutator stay a plain method instead of turning the whole session API
/// async. Implementations must therefore be quick (an embedded database, not a network call).
/// Writes run under the metadata shard lock to preserve commit order; implementations must not
/// call back into the same manager.
pub trait SessionStore: Send + Sync {
    /// Loads every stored session record.
    fn load_all(&self) -> Result<Vec<SessionMetadata>, MemoryError>;

    /// Stores (inserts or replaces) one session record.
    fn save(&self, metadata: &SessionMetadata) -> Result<(), MemoryError>;

    /// Removes one session record; removing a record that does not exist is not an error.
    fn delete(&self, session_key: &str) -> Result<(), MemoryError>;
}

/// One stored session as a conversation listing shows it (see [`SessionManager::sessions_with_prefix`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOverview {
    /// The session's key.
    pub session_key: String,
    /// User and assistant messages in the history (summarized ones excluded).
    pub message_count: usize,
    /// Text of the earliest user message still in the history.
    pub first_user_message: Option<String>,
    /// Unix seconds of the last turn; 0 when the session never had one.
    pub last_active_at: u64,
}

tokio::task_local! {
    /// The writer explicitly delegated by a caller to its agent invocation.
    static SESSION_WRITER: std::cell::RefCell<Option<SessionWriteGuard>>;
}

/// Exclusive ownership of one session's writes, transferable to a streaming producer.
#[derive(Clone)]
pub struct SessionWriteGuard {
    guard: Arc<tokio::sync::OwnedMutexGuard<()>>,
}

impl SessionWriteGuard {
    /// Runs an agent invocation under this already acquired writer.
    ///
    /// Tokio tasks do not inherit this scope: background producers must retain their own clone
    /// until their final memory write, even if the response consumer disconnects.
    pub async fn scope<F: std::future::Future>(&self, future: F) -> F::Output {
        SESSION_WRITER
            .scope(std::cell::RefCell::new(Some(self.clone())), future)
            .await
    }
}

/// Per-session writers shared by owners and waiters, without retaining inactive mutexes.
#[derive(Default)]
pub(crate) struct SessionWriters {
    writers: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
}

impl SessionWriters {
    /// Returns the same writer while any owner or waiter still holds it.
    pub(crate) fn writer(&self, session_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut writers = self
            .writers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(writer) = writers.get(session_id).and_then(Weak::upgrade) {
            return writer;
        }
        // A waiter holds a strong reference too. Removing only expired entries avoids creating
        // a second mutex while another operation is still queued on the original writer.
        writers.retain(|_, writer| writer.strong_count() > 0);
        let writer = Arc::new(tokio::sync::Mutex::new(()));
        writers.insert(session_id.to_string(), Arc::downgrade(&writer));
        writer
    }
}

/// Comprehensive session lifecycle and state manager.
///
/// Wraps an underlying [`Memory`] store and maintains concurrent, lock-sharded
/// session metadata across all active conversations.
pub struct SessionManager {
    /// Backing conversation memory store.
    memory: Arc<dyn Memory>,
    /// Concurrent map of session metadata.
    metadata: DashMap<String, SessionMetadata>,
    /// Default session scope applied when creating sessions from raw keys.
    default_scope: SessionScope,
    /// Durable copy of `metadata`, when the node keeps sessions across restarts.
    store: Option<Arc<dyn SessionStore>>,
    /// Weak entries avoid retaining a mutex for every historical session.
    writers: SessionWriters,
}

impl SessionManager {
    /// Constructs a new `SessionManager` wrapping the given memory store.
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self {
            memory,
            metadata: DashMap::new(),
            default_scope: SessionScope::ChannelUser,
            store: None,
            writers: SessionWriters::default(),
        }
    }

    /// Immediately acquires the session writer; external mutations must never queue behind a turn.
    pub fn try_write(&self, session_id: &str) -> Result<SessionWriteGuard, MemoryError> {
        self.writers
            .writer(session_id)
            .try_lock_owned()
            .map(|guard| SessionWriteGuard {
                guard: Arc::new(guard),
            })
            .map_err(|_| MemoryError::Busy(session_id.to_string()))
    }

    /// Waits for a prior writer, for ordered inbound turns and background compaction only.
    pub async fn write(&self, session_id: &str) -> SessionWriteGuard {
        SessionWriteGuard {
            guard: Arc::new(self.writers.writer(session_id).lock_owned().await),
        }
    }

    /// Acquires an agent writer or accepts the writer explicitly delegated by its caller.
    pub fn agent_write(&self, session_id: &str) -> Result<SessionWriteGuard, MemoryError> {
        let writer = self.writers.writer(session_id);
        if let Ok(Some(guard)) = SESSION_WRITER.try_with(|guard| {
            let mut delegated = guard.borrow_mut();
            if delegated.as_ref().is_some_and(|guard| {
                Arc::ptr_eq(tokio::sync::OwnedMutexGuard::mutex(&guard.guard), &writer)
            }) {
                // Delegation is consumed once: nested tools must acquire their own writer.
                delegated.take()
            } else {
                None
            }
        }) {
            return Ok(guard);
        }
        self.try_write(session_id)
    }

    /// Makes the manager durable: every stored session is loaded now, and every later change is
    /// written through.
    ///
    /// Loading is all-or-nothing and happens before the manager serves anything, so a store that
    /// cannot be read fails startup instead of silently starting from an empty session list.
    pub fn with_store(mut self, store: Arc<dyn SessionStore>) -> Result<Self, MemoryError> {
        for record in store.load_all()? {
            self.metadata.insert(record.session_key.clone(), record);
        }
        self.store = Some(store);
        Ok(self)
    }

    /// Writes one record through to the store, if there is one.
    ///
    /// A failed write is logged, never swallowed and never fatal: losing a counter update must not
    /// turn a delivered reply into an error, but an operator has to be able to see that the store
    /// is failing.
    fn persist(&self, metadata: &SessionMetadata) {
        if let Some(store) = &self.store
            && let Err(err) = store.save(metadata)
        {
            tracing::error!(
                session_key = %metadata.session_key,
                error = %err,
                "Failed to persist session metadata; the session will not survive a restart"
            );
        }
    }

    /// Applies `change` to a session (creating it when missing) and persists it if `change`
    /// reports that it altered anything.
    ///
    /// Keep the existing shard lock through persistence. Releasing it before saving would let
    /// an older snapshot overwrite a newer update or resurrect a concurrently deleted record.
    fn update(&self, session_key: &str, change: impl FnOnce(&mut SessionMetadata) -> bool) {
        let mut entry = self
            .metadata
            .entry(session_key.to_string())
            .or_insert_with(|| SessionMetadata::new(session_key, self.default_scope.clone()));
        if change(&mut entry) {
            self.persist(&entry);
        }
    }

    /// Like [`Self::update`], but only for a session that already exists.
    fn update_existing(
        &self,
        session_key: &str,
        change: impl FnOnce(&mut SessionMetadata) -> bool,
    ) {
        if let Some(mut entry) = self.metadata.get_mut(session_key)
            && change(&mut entry)
        {
            self.persist(&entry);
        }
    }

    /// Sets the default session scope for newly discovered sessions.
    pub fn with_default_scope(mut self, scope: SessionScope) -> Self {
        self.default_scope = scope;
        self
    }

    /// Returns a reference to the underlying memory store.
    pub fn memory(&self) -> &Arc<dyn Memory> {
        &self.memory
    }

    /// Retrieves or initializes metadata for a given session key.
    pub fn get_or_create(&self, session_key: &str) -> SessionMetadata {
        self.get_or_create_scoped(session_key, self.default_scope.clone())
    }

    /// Retrieves or initializes metadata with an explicit session scope.
    pub fn get_or_create_with_scope(&self, key: &SessionKey) -> SessionMetadata {
        self.get_or_create_scoped(key.as_str(), key.scope().clone())
    }

    /// Reads a session, creating (and persisting) it on first sight.
    fn get_or_create_scoped(&self, session_key: &str, scope: SessionScope) -> SessionMetadata {
        use dashmap::mapref::entry::Entry;

        match self.metadata.entry(session_key.to_string()) {
            Entry::Occupied(existing) => existing.get().clone(),
            Entry::Vacant(slot) => {
                let record = SessionMetadata::new(session_key, scope);
                let entry = slot.insert(record);
                self.persist(&entry);
                entry.clone()
            }
        }
    }

    /// Returns existing metadata for a session, if present.
    pub fn get_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        self.metadata.get(session_key).map(|entry| entry.clone())
    }

    /// Records an interaction turn, updating timestamps, turn counters, and token tallies.
    pub fn record_turn(&self, session_key: &str, turn_tokens: usize) {
        let now = current_unix_timestamp();
        self.update(session_key, |entry| {
            entry.last_active_at = now;
            entry.turn_count += 1;
            entry.total_tokens_used += turn_tokens;
            entry.status = SessionStatus::Active;
            true
        });
    }

    /// Sets the active persona identifier for a session.
    ///
    /// The pipeline calls this for every message of a conversation whose instance chose a persona,
    /// so re-binding the persona a session already has is a no-op — and costs no write.
    pub fn set_persona(&self, session_key: &str, persona_id: impl Into<String>) {
        let pid = persona_id.into();
        self.update(session_key, |entry| {
            if entry.persona_id.as_deref() == Some(pid.as_str()) {
                return false;
            }
            entry.persona_id = Some(pid);
            entry.last_active_at = current_unix_timestamp();
            true
        });
    }

    /// Returns the active persona identifier for a session, if configured.
    pub fn get_persona(&self, session_key: &str) -> Option<String> {
        self.metadata
            .get(session_key)
            .and_then(|m| m.persona_id.clone())
    }

    /// Clears the persona binding of every session that uses `persona_id`, returning how many
    /// sessions were unbound.
    ///
    /// Called when a persona is deleted: those sessions fall back to the base assistant instead of
    /// pointing at a persona that no longer exists.
    pub fn unbind_persona(&self, persona_id: &str) -> usize {
        let mut unbound = 0;
        for mut entry in self.metadata.iter_mut() {
            if entry.persona_id.as_deref() == Some(persona_id) {
                entry.persona_id = None;
                self.persist(&entry);
                unbound += 1;
            }
        }
        unbound
    }

    /// Removes the persona binding of one session, which then uses the base assistant.
    pub fn clear_persona(&self, session_key: &str) {
        self.update_existing(session_key, |meta| {
            meta.persona_id = None;
            meta.last_active_at = current_unix_timestamp();
            true
        });
    }

    /// Sets a session-scoped state variable.
    pub fn set_variable(
        &self,
        session_key: &str,
        key: impl Into<String>,
        value: impl Into<String>,
    ) {
        let k = key.into();
        let v = value.into();
        self.update(session_key, |entry| {
            entry.variables.insert(k, v);
            entry.last_active_at = current_unix_timestamp();
            true
        });
    }

    /// Retrieves a session-scoped variable value.
    pub fn get_variable(&self, session_key: &str, key: &str) -> Option<String> {
        self.metadata
            .get(session_key)
            .and_then(|m| m.variables.get(key).cloned())
    }

    /// Returns a copy of all variables defined on a session.
    pub fn get_variables(&self, session_key: &str) -> HashMap<String, String> {
        self.metadata
            .get(session_key)
            .map(|m| m.variables.clone())
            .unwrap_or_default()
    }

    /// Removes a session-scoped variable.
    pub fn remove_variable(&self, session_key: &str, key: &str) -> Option<String> {
        let mut removed = None;
        self.update_existing(session_key, |meta| {
            removed = meta.variables.remove(key);
            removed.is_some()
        });
        removed
    }

    /// Clears the session history in memory, resets turn count and tokens,
    /// while preserving configured persona and session variables.
    pub async fn reset_session(&self, session_key: &str) -> Result<(), MemoryError> {
        let _writing = self.agent_write(session_key)?;
        self.memory.clear(session_key).await?;

        self.update_existing(session_key, |meta| {
            meta.turn_count = 0;
            meta.total_tokens_used = 0;
            meta.status = SessionStatus::Active;
            meta.last_active_at = current_unix_timestamp();
            true
        });

        Ok(())
    }

    /// Marks a session as closed.
    pub fn close_session(&self, session_key: &str) {
        self.update_existing(session_key, |meta| {
            meta.status = SessionStatus::Closed;
            meta.last_active_at = current_unix_timestamp();
            true
        });
    }

    /// Sweeps sessions that have been idle longer than `max_idle`, updating their status to `Idle`.
    ///
    /// Returns the number of sessions transitioned to idle.
    pub fn sweep_idle_sessions(&self, max_idle: Duration) -> usize {
        let mut swept = 0;
        for mut entry in self.metadata.iter_mut() {
            if entry.status == SessionStatus::Active && entry.is_idle(max_idle) {
                entry.status = SessionStatus::Idle;
                self.persist(&entry);
                swept += 1;
            }
        }
        swept
    }

    /// Returns the number of currently tracked sessions.
    pub fn session_count(&self) -> usize {
        self.metadata.len()
    }

    /// Returns the number of sessions currently marked as `Active`.
    pub fn active_session_count(&self) -> usize {
        self.metadata
            .iter()
            .filter(|m| m.status == SessionStatus::Active)
            .count()
    }

    /// Returns a snapshot list of all tracked session metadata.
    pub fn list_sessions(&self) -> Vec<SessionMetadata> {
        self.metadata.iter().map(|entry| entry.clone()).collect()
    }

    /// Lists every session whose key starts with `prefix`, ordered by key.
    ///
    /// Sessions come from both halves of a conversation: the history in [`Memory`] and the
    /// records this manager tracks. A session known to only one half (a record without messages
    /// yet, or history written by a memory-only caller) is still listed.
    pub async fn sessions_with_prefix(
        &self,
        prefix: &str,
    ) -> Result<Vec<SessionOverview>, MemoryError> {
        let mut sessions: HashMap<String, SessionOverview> = self
            .memory
            .list_sessions(prefix)
            .await?
            .into_iter()
            .map(|stored| {
                (
                    stored.session_key.clone(),
                    SessionOverview {
                        session_key: stored.session_key,
                        message_count: stored.message_count,
                        first_user_message: stored.first_user_message,
                        last_active_at: stored.last_message_at.unwrap_or(0),
                    },
                )
            })
            .collect();
        for entry in self.metadata.iter() {
            if !entry.key().starts_with(prefix) {
                continue;
            }
            // A record is created before its first turn (a persona binding, for one), so its
            // timestamps only mean "last turn" once a turn was recorded.
            let last_turn = if entry.turn_count > 0 {
                entry.last_active_at
            } else {
                0
            };
            let overview = sessions
                .entry(entry.key().clone())
                .or_insert_with(|| SessionOverview {
                    session_key: entry.key().clone(),
                    message_count: 0,
                    first_user_message: None,
                    last_active_at: 0,
                });
            overview.last_active_at = overview.last_active_at.max(last_turn);
        }
        let mut sessions: Vec<SessionOverview> = sessions.into_values().collect();
        sessions.sort_by(|a, b| a.session_key.cmp(&b.session_key));
        Ok(sessions)
    }

    /// Deletes a whole session: its history, its summary and its record.
    ///
    /// This is the only way a conversation's messages are removed besides compaction (see
    /// [`crate::memory`]): the session disappears as a unit, so no remaining session ever has a
    /// gap. History goes first; if removing the record then fails, the record is kept on disk
    /// and in memory alike and the error is returned, so a retry finishes the job.
    pub async fn delete_session(&self, session_key: &str) -> Result<(), MemoryError> {
        let _writing = self.agent_write(session_key)?;
        self.memory.clear(session_key).await?;
        // Acquire the same shard as metadata writers, including when the key is absent. This
        // keeps a pending creation or update from saving a row after its deletion committed.
        let entry = self.metadata.entry(session_key.to_string());
        if let Some(store) = &self.store {
            store.delete(session_key)?;
        }
        if let dashmap::mapref::entry::Entry::Occupied(entry) = entry {
            entry.remove();
        }
        Ok(())
    }
}
