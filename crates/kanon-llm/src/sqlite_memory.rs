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
//! - A bounded read cache updated atomically with its SQLite connection;
//! - Bounded LRU cache eviction preventing memory leak under millions of sessions;
//! - Transactional commit points and WAL (Write-Ahead Logging) checkpoints;
//! - Strict error semantics: write errors fail fast and prevent silent cache-DB divergence;
//! - Batch inserts within transactional boundaries;
//! - Filesystem directory integration (compatible with `kanon-storage`);
//! - [`SqliteSessionStore`]: the durable copy of session metadata (persona binding, counters,
//!   status), which can share the database file with the conversation history.

use async_trait::async_trait;
use rusqlite::{Connection, params};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use crate::error::MemoryError;
use crate::gateway::types::{ChatMessage, Role, ToolCall};
use crate::memory::{Memory, MemorySnapshot, SessionMemory, StoredSession};
use crate::session::{SessionMetadata, SessionStore};

/// Backward-compatible type alias for [`SqliteMemory`].
pub type PersistentMemory = SqliteMemory;

/// Persistent conversational memory backend backed by an embedded SQLite database.
pub struct SqliteMemory {
    /// One owner for database commits, cache updates and eviction. SQLite already serializes this
    /// connection; a second lock domain would let eviction race a committed cache update.
    state: Mutex<SqliteState>,
    /// Maximum count of active sessions kept in the in-memory cache.
    max_cached_sessions: usize,
}

/// Database and its derived cache, protected together for the duration of each operation.
struct SqliteState {
    conn: Connection,
    cache: HashMap<String, SessionMemory>,
    lru_order: VecDeque<String>,
}

impl SqliteMemory {
    /// Retains a small working set of active conversations rather than every history read.
    /// Larger read caches duplicate SQLite storage and make LRU touches increasingly expensive.
    pub const DEFAULT_CACHE_CAPACITY: usize = 64;

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
        let state = self.state.lock().await;
        state
            .conn
            .execute_batch("PRAGMA wal_checkpoint(PASSIVE);")?;
        Ok(())
    }

    /// Backward-compatible alias for [`commit_point`].
    pub async fn flush(&self) -> Result<(), MemoryError> {
        self.commit_point().await
    }

    /// Commits and updates the timestamp for a specific session.
    pub async fn commit_session(&self, session_key: &str) -> Result<(), MemoryError> {
        let state = self.state.lock().await;
        let now = current_timestamp();
        state.conn.execute(
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
                 reasoning_content TEXT,
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
        // - `messages.reasoning_content` (separate private reasoning);
        // - `sessions.summary` (compaction).
        //
        // Columns that were dropped from the schema are left in place and ignored:
        // - `sessions.system_prompt`: the persona is composed per request, never stored per
        //   session;
        // - `messages.parts`: a turn's media goes to the model with that turn only. The URLs it
        //   held are signed and expire, and re-sending a dead one made the provider reject every
        //   later request of the session.
        Self::ensure_column(&conn, "messages", "reasoning_content")?;
        Self::ensure_column(&conn, "sessions", "summary")?;

        Ok(Self {
            state: Mutex::new(SqliteState {
                conn,
                cache: HashMap::new(),
                lru_order: VecDeque::new(),
            }),
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

    /// Evicts only between complete operations, while the connection and cache share one lock.
    fn touch_lru(&self, state: &mut SqliteState, session_key: &str) {
        if let Some(pos) = state.lru_order.iter().position(|key| key == session_key) {
            state.lru_order.remove(pos);
        }
        state.lru_order.push_back(session_key.to_string());
        while state.cache.len() > self.max_cached_sessions {
            let Some(key) = state.lru_order.pop_front() else {
                break;
            };
            state.cache.remove(&key);
        }
    }

    /// Decodes one complete history before exposing it to a reader or the optional cache.
    fn load_snapshot(conn: &Connection, session_key: &str) -> Result<MemorySnapshot, MemoryError> {
        let (summary, loaded_messages) = {
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
                "SELECT role, content, tool_calls, tool_call_id, name, reasoning_content
                 FROM messages
                 WHERE session_key = ?1
                 ORDER BY id ASC",
            )?;
            let mut msg_rows = msg_stmt.query(params![session_key])?;

            let mut loaded_messages = Vec::new();
            while let Some(row) = msg_rows.next()? {
                let role_str: String = row.get(0)?;
                let content: Option<String> = row.get(1)?;
                let tool_calls_json: Option<String> = row.get(2)?;
                let tool_call_id: Option<String> = row.get(3)?;
                let name: Option<String> = row.get(4)?;

                let role = match role_str.as_str() {
                    "system" => Role::System,
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    "tool" => Role::Tool,
                    _ => {
                        return Err(MemoryError::Serialization(format!(
                            "unknown message role '{role_str}' in session '{session_key}'"
                        )));
                    }
                };

                let tool_calls: Option<Vec<ToolCall>> = tool_calls_json
                    .map(|json| serde_json::from_str(&json))
                    .transpose()
                    .map_err(|error| {
                        MemoryError::Serialization(format!(
                            "invalid message tool calls in session '{session_key}': {error}"
                        ))
                    })?;
                let mut message = ChatMessage {
                    role,
                    content,
                    reasoning_content: row.get(5)?,
                    parts: None,
                    tool_calls,
                    tool_call_id,
                    name,
                };
                // Decode in memory only: upgrades never rewrite or delete old conversation rows.
                message.separate_reasoning();
                loaded_messages.push(message);
            }

            (summary, loaded_messages)
        };

        Ok(MemorySnapshot {
            summary,
            messages: loaded_messages,
        })
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
///
/// Media parts are not stored: history keeps a message's textual projection only (see the
/// schema notes in `init_connection`).
fn insert_message(
    tx: &rusqlite::Transaction<'_>,
    session_key: &str,
    message: &ChatMessage,
    now: i64,
) -> Result<(), MemoryError> {
    let tool_calls_json = message
        .tool_calls
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| MemoryError::Serialization(error.to_string()))?;
    tx.execute(
        "INSERT INTO messages (session_key, role, content, tool_calls, tool_call_id, name, created_at, reasoning_content)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            session_key,
            role_name(message.role),
            message.content,
            tool_calls_json,
            message.tool_call_id,
            message.name,
            now,
            message.reasoning_content,
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
        let mut state = self.state.lock().await;
        let now = current_timestamp();

        // 1. Batch insert in a single SQLite transaction.
        // If the write fails the whole transaction rolls back and the memory cache stays untouched,
        // so the cache can never run ahead of the database.
        {
            let tx = state.conn.transaction()?;
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
        if let Some(session) = state.cache.get_mut(session_key) {
            session.extend_messages(messages);
            self.touch_lru(&mut state, session_key);
        }
        Ok(())
    }

    async fn snapshot(&self, session_key: &str) -> Result<MemorySnapshot, MemoryError> {
        let mut state = self.state.lock().await;
        if self.max_cached_sessions == 0 {
            // Move decoded history directly to the caller when no read cache was requested.
            return Self::load_snapshot(&state.conn, session_key);
        }
        if !state.cache.contains_key(session_key) {
            // Publish only a fully decoded history so corrupt rows never leave a partial cache.
            let MemorySnapshot { summary, messages } =
                Self::load_snapshot(&state.conn, session_key)?;
            state.cache.insert(
                session_key.to_owned(),
                SessionMemory::from_parts(summary, messages),
            );
        }
        let snapshot = state
            .cache
            .get(session_key)
            .map(|session| session.snapshot())
            .unwrap_or_default();
        self.touch_lru(&mut state, session_key);
        Ok(snapshot)
    }

    async fn compact_history(
        &self,
        session_key: &str,
        covered: usize,
        summary: String,
    ) -> Result<(), MemoryError> {
        let mut state = self.state.lock().await;
        let now = current_timestamp();

        // 1. Delete the covered prefix and store the summary in one transaction. If anything fails
        // (disk full, crash) the transaction aborts and the previous history is untouched.
        {
            let tx = state.conn.transaction()?;
            let held: i64 = tx.query_row(
                "SELECT COUNT(*) FROM messages WHERE session_key = ?1",
                params![session_key],
                |row| row.get(0),
            )?;
            let covered_rows = i64::try_from(covered).map_err(|_| {
                MemoryError::Backend(
                    "compaction message count exceeds SQLite's integer range".into(),
                )
            })?;
            if covered_rows > held {
                return Err(MemoryError::Backend(format!(
                    "cannot compact {covered} messages: the history only holds {held}"
                )));
            }

            // Message ids grow with insertion order, so the covered prefix is the `covered`
            // smallest ids of the session; anything appended since has a larger id and survives.
            tx.execute(
                "DELETE FROM messages
                 WHERE session_key = ?1
                 AND id IN (SELECT id FROM messages WHERE session_key = ?1 ORDER BY id ASC LIMIT ?2)",
                params![session_key, covered_rows],
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
        if let Some(session) = state.cache.get_mut(session_key) {
            session.compact(covered, summary)?;
            self.touch_lru(&mut state, session_key);
        }
        Ok(())
    }

    async fn clear(&self, session_key: &str) -> Result<(), MemoryError> {
        let mut state = self.state.lock().await;

        {
            let tx = state.conn.transaction()?;
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
        state.cache.remove(session_key);
        if let Some(pos) = state.lru_order.iter().position(|k| k == session_key) {
            state.lru_order.remove(pos);
        }
        Ok(())
    }

    async fn session_count(&self) -> Result<usize, MemoryError> {
        let state = self.state.lock().await;
        let count: i64 = state
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                let count: i64 = row.get(0)?;
                Ok(count)
            })?;
        Ok(count as usize)
    }

    async fn list_sessions(&self, prefix: &str) -> Result<Vec<StoredSession>, MemoryError> {
        // The database is authoritative: the cache only ever mirrors committed rows, so reading
        // here never misses a message the cache holds. `substr` compares the prefix literally,
        // where `LIKE` would treat `%` and `_` in a chat id as wildcards.
        let state = self.state.lock().await;
        let mut stmt = state.conn.prepare(
            "SELECT s.session_key,
                    (SELECT COUNT(*) FROM messages m
                      WHERE m.session_key = s.session_key AND m.role IN ('user', 'assistant')),
                    (SELECT m.content FROM messages m
                      WHERE m.session_key = s.session_key AND m.role = 'user'
                      ORDER BY m.id ASC LIMIT 1),
                    (SELECT MAX(m.created_at) FROM messages m WHERE m.session_key = s.session_key)
             FROM sessions s
             WHERE substr(s.session_key, 1, length(?1)) = ?1
             ORDER BY s.session_key",
        )?;
        let mut rows = stmt.query(params![prefix])?;
        let mut sessions = Vec::new();
        while let Some(row) = rows.next()? {
            let count: i64 = row.get(1)?;
            let last: Option<i64> = row.get(3)?;
            sessions.push(StoredSession {
                session_key: row.get(0)?,
                message_count: count.max(0) as usize,
                first_user_message: row.get(2)?,
                last_message_at: last.map(|seconds| seconds.max(0) as u64),
            });
        }
        Ok(sessions)
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

    fn delete(&self, session_key: &str) -> Result<(), MemoryError> {
        self.conn().execute(
            "DELETE FROM session_meta WHERE session_key = ?1",
            params![session_key],
        )?;
        Ok(())
    }
}
