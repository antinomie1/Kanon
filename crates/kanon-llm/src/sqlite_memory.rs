//! Embedded SQLite-backed Conversation Memory.
//!
//! Provides durable, multi-session isolated persistence for chat histories, conversation
//! summaries, tool call arguments, and tool outputs.
//!
//! Features:
//! - Multi-session isolation via unique `session_key` indexing;
//! - **Append-only history**: messages are only ever inserted, so a session's prompt prefix is
//!   stable across turns (see [`crate::memory`] for why that matters);
//! - Atomic compaction: the covered prefix is deleted and the summary written in one transaction,
//!   so a failure never loses the previous history;
//! - High-concurrency in-memory read cache (sub-5µs lookups via lock-free [`DashMap`]);
//! - Bounded LRU cache eviction preventing memory leak under millions of sessions;
//! - Transactional commit points and WAL (Write-Ahead Logging) checkpoints;
//! - Strict error semantics: write errors fail fast and prevent silent cache-DB divergence;
//! - Batch inserts within transactional boundaries;
//! - Filesystem directory integration (compatible with `kanon-storage`);
//! - [`SqliteSessionStore`]: the durable copy of session metadata (persona binding, counters,
//!   status), which can share the database file with the conversation history.

use async_trait::async_trait;
use dashmap::DashMap;
use rusqlite::{Connection, params};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use crate::error::MemoryError;
use crate::gateway::types::{ChatMessage, Role, ToolCall};
use crate::memory::{Memory, MemorySnapshot, SessionMemory};
use crate::session::{SessionMetadata, SessionStore};

/// Backward-compatible type alias for [`SqliteMemory`].
pub type PersistentMemory = SqliteMemory;

/// Persistent conversational memory backend backed by an embedded SQLite database.
pub struct SqliteMemory {
    /// Exclusive thread-safe database connection handle.
    conn: Arc<Mutex<Connection>>,
    /// High-performance in-memory read cache preventing repeated disk I/O amplification.
    cache: Arc<DashMap<String, SessionMemory>>,
    /// LRU session access queue tracking recency for cache eviction.
    lru_order: Arc<Mutex<VecDeque<String>>>,
    /// Per-session serialization lock ensuring sequential consistency across concurrent operations
    /// on the same session (preventing SQLite commit and memory cache update order divergence).
    session_locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
    /// Maximum count of active sessions kept in the in-memory cache.
    max_cached_sessions: usize,
}

impl SqliteMemory {
    /// Default in-memory LRU cache capacity (10,000 active sessions).
    pub const DEFAULT_CACHE_CAPACITY: usize = 10_000;

    /// Opens or creates an SQLite-backed memory store at the specified filesystem path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let conn = Connection::open(path)?;
        Self::init_connection(conn)
    }

    /// Opens or creates an SQLite-backed memory store inside a specific directory.
    ///
    /// Automatically ensures that the parent directory exists before creating the database file.
    /// Ideal for integration with isolated plugin storage directories (`./data/plugins/<id>/`).
    pub fn open_in_dir(dir: impl AsRef<Path>, filename: &str) -> Result<Self, MemoryError> {
        let dir = dir.as_ref();
        let _ = std::fs::create_dir_all(dir);
        let path = dir.join(filename);
        Self::open(path)
    }

    /// Opens an in-memory SQLite database, primarily used for testing or transient isolation.
    pub fn open_in_memory() -> Result<Self, MemoryError> {
        let conn = Connection::open_in_memory()?;
        Self::init_connection(conn)
    }

    /// Configures the maximum number of sessions retained in the in-memory read LRU cache.
    pub fn with_cache_capacity(mut self, capacity: usize) -> Self {
        self.max_cached_sessions = capacity;
        self
    }

    /// Returns the active in-memory cache capacity.
    pub fn cache_capacity(&self) -> usize {
        self.max_cached_sessions
    }

    /// Explicitly flushes dirty data and executes a WAL checkpoint.
    ///
    /// Ensures all pending Write-Ahead Log pages are safely written back to the
    /// main database file without blocking concurrent readers.
    pub async fn commit_point(&self) -> Result<(), MemoryError> {
        let conn = self.conn.lock().await;
        conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);")?;
        Ok(())
    }

    /// Backward-compatible alias for [`commit_point`].
    pub async fn flush(&self) -> Result<(), MemoryError> {
        self.commit_point().await
    }

    /// Commits and updates the timestamp for a specific session.
    pub async fn commit_session(&self, session_key: &str) -> Result<(), MemoryError> {
        let lock = self.session_lock(session_key);
        let _guard = lock.lock().await;

        let now = current_timestamp();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE session_key = ?2",
            params![now, session_key],
        )?;
        Ok(())
    }

    /// Initializes connection pragmas and establishes the relational schema.
    fn init_connection(conn: Connection) -> Result<Self, MemoryError> {
        // High-performance concurrency pragmas:
        // WAL mode enables concurrent readers while writers append to the log.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;

             CREATE TABLE IF NOT EXISTS sessions (
                 session_key TEXT PRIMARY KEY,
                 summary TEXT,
                 updated_at INTEGER NOT NULL
             );

             CREATE TABLE IF NOT EXISTS messages (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_key TEXT NOT NULL,
                 role TEXT NOT NULL,
                 content TEXT,
                 parts TEXT,
                 tool_calls TEXT,
                 tool_call_id TEXT,
                 name TEXT,
                 created_at INTEGER NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_key, id);",
        )?;

        // Databases created by earlier versions lack columns added since. SQLite cannot express
        // `ADD COLUMN IF NOT EXISTS`, so the schema is inspected and missing columns are added
        // explicitly instead of failing the query, which keeps an operator's history usable across
        // the upgrade:
        // - `messages.parts` (multimodal messages);
        // - `sessions.summary` (compaction). The former `sessions.system_prompt` column is left
        //   in place and ignored: the persona is composed per request, never stored per session.
        Self::ensure_column(&conn, "messages", "parts")?;
        Self::ensure_column(&conn, "sessions", "summary")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            cache: Arc::new(DashMap::new()),
            lru_order: Arc::new(Mutex::new(VecDeque::new())),
            session_locks: Arc::new(DashMap::new()),
            max_cached_sessions: Self::DEFAULT_CACHE_CAPACITY,
        })
    }

    /// Adds a nullable `TEXT` column to a table when it is not there yet.
    fn ensure_column(conn: &Connection, table: &str, column: &str) -> Result<(), MemoryError> {
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
        let mut rows = stmt.query(params![table])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            if name == column {
                return Ok(());
            }
        }
        drop(rows);
        drop(stmt);
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT;"))?;
        Ok(())
    }

    /// Retrieves or allocates the serialization lock for a session key.
    fn session_lock(&self, session_key: &str) -> Arc<Mutex<()>> {
        self.session_locks
            .entry(session_key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Updates LRU order and evicts least recently accessed sessions if cache capacity is exceeded.
    async fn touch_lru(&self, session_key: &str) {
        let mut lru = self.lru_order.lock().await;
        if let Some(pos) = lru.iter().position(|k| k == session_key) {
            lru.remove(pos);
        }
        lru.push_back(session_key.to_string());

        // Evict LRU entries from memory cache when exceeding capacity
        while self.cache.len() > self.max_cached_sessions && !lru.is_empty() {
            if let Some(evicted_key) = lru.pop_front() {
                if evicted_key != session_key {
                    self.cache.remove(&evicted_key);
                    // Also evict the session lock if it is no longer actively held
                    if let Some(entry) = self.session_locks.get(&evicted_key)
                        && Arc::strong_count(&entry) <= 2
                    {
                        drop(entry);
                        self.session_locks.remove(&evicted_key);
                    }
                } else {
                    // Put back if it's the current active key and break
                    lru.push_back(evicted_key);
                    break;
                }
            }
        }
    }

    /// Ensures a session is loaded from SQLite into the in-memory read cache.
    async fn ensure_session_cached(&self, session_key: &str) -> Result<(), MemoryError> {
        if self.cache.contains_key(session_key) {
            self.touch_lru(session_key).await;
            return Ok(());
        }

        let (summary, loaded_messages) = {
            let conn = self.conn.lock().await;

            // Query the summary of earlier compactions
            let mut session_stmt =
                conn.prepare("SELECT summary FROM sessions WHERE session_key = ?1")?;
            let mut session_rows = session_stmt.query(params![session_key])?;
            let summary: Option<String> = if let Some(row) = session_rows.next()? {
                row.get(0)?
            } else {
                None
            };

            // Query historical messages ordered chronologically
            let mut msg_stmt = conn.prepare(
                "SELECT role, content, parts, tool_calls, tool_call_id, name
                 FROM messages
                 WHERE session_key = ?1
                 ORDER BY id ASC",
            )?;
            let mut msg_rows = msg_stmt.query(params![session_key])?;

            let mut loaded_messages = Vec::new();
            while let Some(row) = msg_rows.next()? {
                let role_str: String = row.get(0)?;
                let content: Option<String> = row.get(1)?;
                let parts_json: Option<String> = row.get(2)?;
                let tool_calls_json: Option<String> = row.get(3)?;
                let tool_call_id: Option<String> = row.get(4)?;
                let name: Option<String> = row.get(5)?;

                let role = match role_str.as_str() {
                    "system" => Role::System,
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    "tool" => Role::Tool,
                    _ => Role::User,
                };

                let tool_calls: Option<Vec<ToolCall>> =
                    tool_calls_json.and_then(|s| serde_json::from_str(&s).ok());
                // A row whose parts cannot be decoded keeps its textual projection: failing the
                // whole session load over one malformed media payload would hide the conversation.
                let parts = parts_json.and_then(|s| match serde_json::from_str(&s) {
                    Ok(parts) => Some(parts),
                    Err(err) => {
                        tracing::warn!(error = %err, "Dropping undecodable message parts");
                        None
                    }
                });

                loaded_messages.push(ChatMessage {
                    role,
                    content,
                    parts,
                    tool_calls,
                    tool_call_id,
                    name,
                });
            }

            (summary, loaded_messages)
        };

        self.cache.insert(
            session_key.to_string(),
            SessionMemory::from_parts(summary, loaded_messages),
        );
        self.touch_lru(session_key).await;
        Ok(())
    }
}

fn current_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Wire name of a role, as stored in the `messages` table.
fn role_name(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// Inserts one message row inside an open transaction.
fn insert_message(
    tx: &rusqlite::Transaction<'_>,
    session_key: &str,
    message: &ChatMessage,
    now: i64,
) -> Result<(), MemoryError> {
    let tool_calls_json = message
        .tool_calls
        .as_ref()
        .and_then(|calls| serde_json::to_string(calls).ok());
    let parts_json = message
        .parts
        .as_ref()
        .and_then(|parts| serde_json::to_string(parts).ok());

    tx.execute(
        "INSERT INTO messages (session_key, role, content, parts, tool_calls, tool_call_id, name, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            session_key,
            role_name(message.role),
            message.content,
            parts_json,
            tool_calls_json,
            message.tool_call_id,
            message.name,
            now,
        ],
    )?;
    Ok(())
}

#[async_trait]
impl Memory for SqliteMemory {
    async fn push_message(
        &self,
        session_key: &str,
        message: ChatMessage,
    ) -> Result<(), MemoryError> {
        self.extend_messages(session_key, vec![message]).await
    }

    async fn extend_messages(
        &self,
        session_key: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<(), MemoryError> {
        let lock = self.session_lock(session_key);
        let _guard = lock.lock().await;

        self.ensure_session_cached(session_key).await?;

        let now = current_timestamp();

        // 1. Batch insert in a single SQLite transaction.
        // If the write fails the whole transaction rolls back and the memory cache stays untouched,
        // so the cache can never run ahead of the database.
        {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT OR IGNORE INTO sessions (session_key, summary, updated_at) VALUES (?1, NULL, ?2)",
                params![session_key, now],
            )?;
            for message in &messages {
                insert_message(&tx, session_key, message, now)?;
            }
            tx.commit()?;
        }

        // 2. ONLY upon successful database commit, update the in-memory read cache.
        self.cache
            .entry(session_key.to_string())
            .or_default()
            .extend_messages(messages);

        self.touch_lru(session_key).await;
        Ok(())
    }

    async fn snapshot(&self, session_key: &str) -> Result<MemorySnapshot, MemoryError> {
        let lock = self.session_lock(session_key);
        let _guard = lock.lock().await;

        self.ensure_session_cached(session_key).await?;
        Ok(self
            .cache
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
        let lock = self.session_lock(session_key);
        let _guard = lock.lock().await;

        self.ensure_session_cached(session_key).await?;

        let held = self
            .cache
            .get(session_key)
            .map(|session| session.len())
            .unwrap_or(0);
        if covered > held {
            return Err(MemoryError::Backend(format!(
                "cannot compact {covered} messages: the history only holds {held}"
            )));
        }

        let now = current_timestamp();

        // 1. Delete the covered prefix and store the summary in one transaction. If anything fails
        // (disk full, crash) the transaction aborts and the previous history is untouched.
        {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;

            // Message ids grow with insertion order, so the covered prefix is the `covered`
            // smallest ids of the session; anything appended since has a larger id and survives.
            tx.execute(
                "DELETE FROM messages
                 WHERE session_key = ?1
                 AND id IN (SELECT id FROM messages WHERE session_key = ?1 ORDER BY id ASC LIMIT ?2)",
                params![session_key, covered as i64],
            )?;
            tx.execute(
                "INSERT INTO sessions (session_key, summary, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(session_key) DO UPDATE SET
                     summary = excluded.summary,
                     updated_at = excluded.updated_at",
                params![session_key, summary, now],
            )?;
            tx.commit()?;
        }

        // 2. ONLY upon successful commit, mirror the fold into the read cache.
        if let Some(mut session) = self.cache.get_mut(session_key) {
            session.compact(covered, summary)?;
        }

        self.touch_lru(session_key).await;
        Ok(())
    }

    async fn clear(&self, session_key: &str) -> Result<(), MemoryError> {
        let lock = self.session_lock(session_key);
        let _guard = lock.lock().await;

        {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM messages WHERE session_key = ?1",
                params![session_key],
            )?;
            tx.execute(
                "DELETE FROM sessions WHERE session_key = ?1",
                params![session_key],
            )?;
            tx.commit()?;
        }
        self.cache.remove(session_key);
        let mut lru = self.lru_order.lock().await;
        if let Some(pos) = lru.iter().position(|k| k == session_key) {
            lru.remove(pos);
        }
        Ok(())
    }

    async fn session_count(&self) -> Result<usize, MemoryError> {
        let conn = self.conn.lock().await;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| {
            let count: i64 = row.get(0)?;
            Ok(count)
        })?;
        Ok(count as usize)
    }
}

/// SQLite-backed [`SessionStore`]: one JSON document per session.
///
/// It opens its own connection, so it can point at the same database file as [`SqliteMemory`]
/// (WAL mode lets the two write without blocking each other for long). The connection sits behind
/// a plain mutex because [`SessionStore`] is synchronous by design: each call is one small
/// statement, and a synchronous store keeps every session mutator a plain method.
pub struct SqliteSessionStore {
    conn: std::sync::Mutex<Connection>,
}

impl SqliteSessionStore {
    /// How long a write waits for another connection's transaction before failing.
    const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    /// Opens (creating when needed) the session table in the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        Self::init(Connection::open(path)?)
    }

    /// Opens a private in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self, MemoryError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, MemoryError> {
        // Two connections write to one file, so a busy database is waited for instead of failing
        // the write outright.
        conn.busy_timeout(Self::BUSY_TIMEOUT)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;

             CREATE TABLE IF NOT EXISTS session_meta (
                 session_key TEXT PRIMARY KEY,
                 data TEXT NOT NULL,
                 updated_at INTEGER NOT NULL
             );",
        )?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// Acquires the connection. A poisoned lock only means another writer panicked between two
    /// statements; the connection itself is still consistent, so it is used as it is.
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SessionStore for SqliteSessionStore {
    fn load_all(&self) -> Result<Vec<SessionMetadata>, MemoryError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT session_key, data FROM session_meta")?;
        let mut rows = stmt.query([])?;

        let mut records = Vec::new();
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let data: String = row.get(1)?;
            // A record that cannot be decoded is an error, not a skipped row: dropping a session
            // silently would look like the conversation was never there.
            let record: SessionMetadata = serde_json::from_str(&data).map_err(|err| {
                MemoryError::Serialization(format!("session '{key}' is unreadable: {err}"))
            })?;
            records.push(record);
        }
        Ok(records)
    }

    fn save(&self, metadata: &SessionMetadata) -> Result<(), MemoryError> {
        let data = serde_json::to_string(metadata)
            .map_err(|err| MemoryError::Serialization(err.to_string()))?;
        self.conn().execute(
            "INSERT INTO session_meta (session_key, data, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_key) DO UPDATE SET
                 data = excluded.data,
                 updated_at = excluded.updated_at",
            params![metadata.session_key, data, current_timestamp()],
        )?;
        Ok(())
    }
}
