//! Embedded SQLite-backed Conversation Memory.
//!
//! Provides durable, multi-session isolated persistence for chat histories,
//! system prompts, tool call arguments, and tool outputs.
//!
//! Features:
//! - Multi-session isolation via unique `session_key` indexing;
//! - High-concurrency in-memory read cache (sub-5µs lookups via lock-free [`DashMap`]);
//! - Bounded LRU cache eviction preventing memory leak under millions of sessions;
//! - Transactional commit points and WAL (Write-Ahead Logging) checkpoints;
//! - Strict error semantics: write errors fail fast and prevent silent cache-DB divergence;
//! - Atomic history replacement eliminating destructive clear-and-insert vulnerability windows;
//! - Token-budget and sliding-window pruning synchronized between memory and SQLite;
//! - Batch inserts within transactional boundaries;
//! - Filesystem directory integration (compatible with `kanon-storage`).

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
use crate::memory::{Memory, SessionMemory};

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
    /// Default sliding window message limit per session.
    default_max_messages: usize,
    /// Optional default token budget per session.
    default_max_tokens: Option<usize>,
}

impl SqliteMemory {
    /// Default in-memory LRU cache capacity (10,000 active sessions).
    pub const DEFAULT_CACHE_CAPACITY: usize = 10_000;

    /// Opens or creates an SQLite-backed memory store at the specified filesystem path.
    pub fn open(path: impl AsRef<Path>, default_max_messages: usize) -> Result<Self, MemoryError> {
        let conn = Connection::open(path)?;
        Self::init_connection(conn, default_max_messages)
    }

    /// Opens or creates an SQLite-backed memory store inside a specific directory.
    ///
    /// Automatically ensures that the parent directory exists before creating the database file.
    /// Ideal for integration with isolated plugin storage directories (`./data/plugins/<id>/`).
    pub fn open_in_dir(
        dir: impl AsRef<Path>,
        filename: &str,
        default_max_messages: usize,
    ) -> Result<Self, MemoryError> {
        let dir = dir.as_ref();
        let _ = std::fs::create_dir_all(dir);
        let path = dir.join(filename);
        Self::open(path, default_max_messages)
    }

    /// Opens an in-memory SQLite database, primarily used for testing or transient isolation.
    pub fn open_in_memory(default_max_messages: usize) -> Result<Self, MemoryError> {
        let conn = Connection::open_in_memory()?;
        Self::init_connection(conn, default_max_messages)
    }

    /// Configures the default token budget for sessions in this memory store.
    pub fn with_token_budget(mut self, budget: usize) -> Self {
        self.default_max_tokens = Some(budget);
        self
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

    /// Returns estimated token utilization for a session.
    pub async fn estimated_tokens(&self, session_key: &str) -> usize {
        let lock = self.session_lock(session_key);
        let _guard = lock.lock().await;

        if self.ensure_session_cached(session_key).await.is_err() {
            return 0;
        }
        self.cache
            .get(session_key)
            .map(|s| s.estimated_tokens())
            .unwrap_or(0)
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
    fn init_connection(conn: Connection, default_max_messages: usize) -> Result<Self, MemoryError> {
        // High-performance concurrency pragmas:
        // WAL mode enables concurrent readers while writers append to the log.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             
             CREATE TABLE IF NOT EXISTS sessions (
                 session_key TEXT PRIMARY KEY,
                 system_prompt TEXT,
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

        // A database created before multimodal messages existed has no `parts` column. Adding it
        // explicitly (instead of failing the query) keeps an operator's history usable across the
        // upgrade; SQLite cannot express `ADD COLUMN IF NOT EXISTS`, so the schema is inspected.
        let has_parts: bool = {
            let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('messages')")?;
            let mut rows = stmt.query([])?;
            let mut found = false;
            while let Some(row) = rows.next()? {
                let name: String = row.get(0)?;
                if name == "parts" {
                    found = true;
                    break;
                }
            }
            found
        };
        if !has_parts {
            conn.execute_batch("ALTER TABLE messages ADD COLUMN parts TEXT;")?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            cache: Arc::new(DashMap::new()),
            lru_order: Arc::new(Mutex::new(VecDeque::new())),
            session_locks: Arc::new(DashMap::new()),
            max_cached_sessions: Self::DEFAULT_CACHE_CAPACITY,
            default_max_messages,
            default_max_tokens: None,
        })
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

        let (system_prompt, loaded_messages) = {
            let conn = self.conn.lock().await;

            // Query session system prompt
            let mut session_stmt =
                conn.prepare("SELECT system_prompt FROM sessions WHERE session_key = ?1")?;
            let mut session_rows = session_stmt.query(params![session_key])?;
            let system_prompt: Option<String> = if let Some(row) = session_rows.next()? {
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

            (system_prompt, loaded_messages)
        };

        let mut session_mem =
            SessionMemory::with_budget(self.default_max_messages, self.default_max_tokens);
        if let Some(prompt) = system_prompt {
            session_mem.set_system_prompt(prompt);
        }
        session_mem.extend_messages(loaded_messages);

        self.cache.insert(session_key.to_string(), session_mem);
        self.touch_lru(session_key).await;
        Ok(())
    }

    /// Internal helper pruning physical SQLite messages when in-memory window evicts old items.
    async fn sync_prune_sqlite(
        &self,
        session_key: &str,
        retain_limit: usize,
    ) -> Result<(), MemoryError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM messages 
             WHERE session_key = ?1 
             AND id NOT IN (
                 SELECT id FROM messages WHERE session_key = ?1 ORDER BY id DESC LIMIT ?2
             )",
            params![session_key, retain_limit as i64],
        )?;
        Ok(())
    }
}

fn current_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[async_trait]
impl Memory for SqliteMemory {
    async fn push_message(
        &self,
        session_key: &str,
        message: ChatMessage,
    ) -> Result<(), MemoryError> {
        let lock = self.session_lock(session_key);
        let _guard = lock.lock().await;

        self.ensure_session_cached(session_key).await?;

        let role_str = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };

        let tool_calls_json = message
            .tool_calls
            .as_ref()
            .and_then(|calls| serde_json::to_string(calls).ok());
        let parts_json = message
            .parts
            .as_ref()
            .and_then(|parts| serde_json::to_string(parts).ok());

        let now = current_timestamp();

        // 1. Transactionally write message to SQLite
        // If write fails, return error immediately without modifying in-memory cache!
        {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT OR IGNORE INTO sessions (session_key, system_prompt, updated_at) VALUES (?1, NULL, ?2)",
                params![session_key, now],
            )?;
            tx.execute(
                "INSERT INTO messages (session_key, role, content, parts, tool_calls, tool_call_id, name, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    session_key,
                    role_str,
                    message.content,
                    parts_json,
                    tool_calls_json,
                    message.tool_call_id,
                    message.name,
                    now,
                ],
            )?;
            tx.commit()?;
        }

        // 2. ONLY upon successful database commit, update in-memory read cache
        let retained_len = {
            let mut session = self
                .cache
                .entry(session_key.to_string())
                .or_insert_with(|| {
                    SessionMemory::with_budget(self.default_max_messages, self.default_max_tokens)
                });
            session.push_message(message);
            session.len()
        };

        self.touch_lru(session_key).await;

        // 3. Keep persistent storage in sync with sliding window
        self.sync_prune_sqlite(session_key, retained_len).await?;
        Ok(())
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

        // 1. Batch insert in a single SQLite transaction
        // If write fails, the entire transaction rolls back and memory cache remains untouched!
        {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT OR IGNORE INTO sessions (session_key, system_prompt, updated_at) VALUES (?1, NULL, ?2)",
                params![session_key, now],
            )?;
            for message in &messages {
                let role_str = match message.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
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
                        role_str,
                        message.content,
                        parts_json,
                        tool_calls_json,
                        message.tool_call_id,
                        message.name,
                        now,
                    ],
                )?;
            }
            tx.commit()?;
        }

        // 2. ONLY upon successful database commit, update in-memory read cache
        let retained_len = {
            let mut session = self
                .cache
                .entry(session_key.to_string())
                .or_insert_with(|| {
                    SessionMemory::with_budget(self.default_max_messages, self.default_max_tokens)
                });
            session.extend_messages(messages);
            session.len()
        };

        self.touch_lru(session_key).await;
        self.sync_prune_sqlite(session_key, retained_len).await?;
        Ok(())
    }

    async fn set_system_prompt(
        &self,
        session_key: &str,
        prompt: String,
    ) -> Result<(), MemoryError> {
        let lock = self.session_lock(session_key);
        let _guard = lock.lock().await;

        self.ensure_session_cached(session_key).await?;

        let now = current_timestamp();

        // 1. Upsert into sessions table
        {
            let conn = self.conn.lock().await;
            conn.execute(
                "INSERT INTO sessions (session_key, system_prompt, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(session_key) DO UPDATE SET
                     system_prompt = excluded.system_prompt,
                     updated_at = excluded.updated_at",
                params![session_key, prompt, now],
            )?;
        }

        // 2. Update in-memory read cache
        self.cache
            .entry(session_key.to_string())
            .or_insert_with(|| {
                SessionMemory::with_budget(self.default_max_messages, self.default_max_tokens)
            })
            .set_system_prompt(prompt);

        self.touch_lru(session_key).await;
        Ok(())
    }

    async fn get_system_prompt(&self, session_key: &str) -> Result<Option<String>, MemoryError> {
        let lock = self.session_lock(session_key);
        let _guard = lock.lock().await;

        self.ensure_session_cached(session_key).await?;
        Ok(self
            .cache
            .get(session_key)
            .and_then(|s| s.system_prompt().map(|p| p.to_string())))
    }

    async fn get_messages(&self, session_key: &str) -> Result<Vec<ChatMessage>, MemoryError> {
        let lock = self.session_lock(session_key);
        let _guard = lock.lock().await;

        self.ensure_session_cached(session_key).await?;
        Ok(self
            .cache
            .get(session_key)
            .map(|s| s.get_messages())
            .unwrap_or_default())
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

    async fn replace_history(
        &self,
        session_key: &str,
        system_prompt: Option<String>,
        messages: Vec<ChatMessage>,
    ) -> Result<(), MemoryError> {
        let lock = self.session_lock(session_key);
        let _guard = lock.lock().await;

        let now = current_timestamp();

        // 1. Execute deletion, session upsert, and messages insertion within a single atomic transaction.
        // If anything fails (e.g. disk failure, crash), the entire transaction aborts,
        // and prior conversation history remains completely safe and uncorrupted!
        {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;

            tx.execute(
                "DELETE FROM messages WHERE session_key = ?1",
                params![session_key],
            )?;

            tx.execute(
                "INSERT INTO sessions (session_key, system_prompt, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(session_key) DO UPDATE SET
                     system_prompt = excluded.system_prompt,
                     updated_at = excluded.updated_at",
                params![session_key, system_prompt, now],
            )?;

            for message in &messages {
                let role_str = match message.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
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
                        role_str,
                        message.content,
                        parts_json,
                        tool_calls_json,
                        message.tool_call_id,
                        message.name,
                        now,
                    ],
                )?;
            }

            tx.commit()?;
        }

        // 2. ONLY upon successful database commit, update in-memory read cache
        {
            let mut session = self
                .cache
                .entry(session_key.to_string())
                .or_insert_with(|| {
                    SessionMemory::with_budget(self.default_max_messages, self.default_max_tokens)
                });
            session.clear_messages();
            if let Some(prompt) = system_prompt {
                session.set_system_prompt(prompt);
            }
            session.extend_messages(messages);
        }

        self.touch_lru(session_key).await;
        Ok(())
    }
}
