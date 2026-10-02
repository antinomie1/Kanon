//! The core's central key-value store (`data/kv.db`), one namespace per plugin.
//!
//! Plugins keep small state here — counters, flags, cached tokens, per-user settings — without
//! opening a database of their own. Larger or relational data still belongs in the plugin's own
//! directory (`data/plugins/<id>/`), see [`crate::PluginDataDir`].
//!
//! Design:
//! - One SQLite table keyed by `(plugin_id, key)`; a plugin only ever addresses its own rows.
//! - Values are opaque bytes; the SDKs layer JSON on top.
//! - A key may expire. Expired keys are invisible at once (every read filters on the clock) and
//!   their rows are removed lazily: when read, when overwritten, and in one sweep at open — so no
//!   background task is needed and nothing is ever returned past its deadline.
//! - Sizes are bounded ([`MAX_KEY_BYTES`], [`MAX_VALUE_BYTES`]) so one plugin cannot turn the
//!   shared file into a blob store.
//!
//! The API is synchronous: every operation is a single short statement. Async callers run it on
//! the blocking pool (`tokio::task::spawn_blocking`) so a slow disk never stalls the runtime.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::id::{PluginId, PluginIdError};

/// Default location of the store, relative to the node working directory.
pub const DEFAULT_KV_FILE: &str = "./data/kv.db";

/// Longest key, in bytes.
pub const MAX_KEY_BYTES: usize = 256;

/// Largest value, in bytes (1 MiB).
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;

/// Why a KV operation failed.
#[derive(Debug, thiserror::Error)]
pub enum KvError {
    /// The plugin identifier is not a valid namespace.
    #[error("invalid plugin id: {0}")]
    PluginId(#[from] PluginIdError),
    /// The key is empty or longer than [`MAX_KEY_BYTES`].
    #[error("key must be 1..={MAX_KEY_BYTES} bytes, got {0}")]
    Key(usize),
    /// The value is larger than [`MAX_VALUE_BYTES`].
    #[error("value must be at most {MAX_VALUE_BYTES} bytes, got {0}")]
    ValueTooLarge(usize),
    /// The database failed.
    #[error("kv database error: {0}")]
    Database(#[from] rusqlite::Error),
    /// The data directory could not be created.
    #[error("kv store could not be opened: {0}")]
    Io(#[from] std::io::Error),
}

/// The central key-value store.
pub struct KvStore {
    /// One connection; every operation is a single statement, so a mutex is all the
    /// coordination needed and writers never see `SQLITE_BUSY` from each other.
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for KvStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvStore").finish_non_exhaustive()
    }
}

impl KvStore {
    /// Opens (or creates) the store at `path`, creating its directory if needed.
    ///
    /// Fails when the file cannot be opened or is not a usable database: the node then refuses
    /// to start rather than run with plugins silently losing their state.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, KvError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    /// Opens a store that lives only in memory (tests, sandboxes).
    pub fn open_in_memory() -> Result<Self, KvError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, KvError> {
        // WAL keeps readers and the single writer from blocking each other on disk; NORMAL sync
        // is durable across process crashes, which is the failure that matters for plugin state.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS kv (
                 plugin_id  TEXT NOT NULL,
                 key        TEXT NOT NULL,
                 value      BLOB NOT NULL,
                 expires_at INTEGER,
                 PRIMARY KEY (plugin_id, key)
             ) WITHOUT ROWID;",
        )?;
        conn.execute(
            "DELETE FROM kv WHERE expires_at IS NOT NULL AND expires_at <= ?1",
            params![now_secs()],
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Stores `value` under `key`, replacing any previous value and deadline.
    ///
    /// `ttl` of `None` keeps the key until it is deleted; otherwise it expires after `ttl`
    /// (rounded up to whole seconds).
    pub fn set(
        &self,
        plugin_id: &str,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<(), KvError> {
        validate(plugin_id, key)?;
        if value.len() > MAX_VALUE_BYTES {
            return Err(KvError::ValueTooLarge(value.len()));
        }
        let expires_at = ttl.map(|ttl| {
            let secs = ttl.as_secs() + u64::from(ttl.subsec_nanos() > 0);
            now_secs().saturating_add(i64::try_from(secs).unwrap_or(i64::MAX))
        });
        self.conn().execute(
            "INSERT INTO kv (plugin_id, key, value, expires_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (plugin_id, key) DO UPDATE SET value = ?3, expires_at = ?4",
            params![plugin_id, key, value, expires_at],
        )?;
        Ok(())
    }

    /// The value under `key`, or `None` when it is absent or expired.
    pub fn get(&self, plugin_id: &str, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        validate(plugin_id, key)?;
        let conn = self.conn();
        let row: Option<(Vec<u8>, Option<i64>)> = conn
            .query_row(
                "SELECT value, expires_at FROM kv WHERE plugin_id = ?1 AND key = ?2",
                params![plugin_id, key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match row {
            Some((_, Some(expires_at))) if expires_at <= now_secs() => {
                // Expired: drop the row now that we have seen it; the caller sees a miss either way.
                conn.execute(
                    "DELETE FROM kv WHERE plugin_id = ?1 AND key = ?2 AND expires_at <= ?3",
                    params![plugin_id, key, now_secs()],
                )?;
                Ok(None)
            }
            Some((value, _)) => Ok(Some(value)),
            None => Ok(None),
        }
    }

    /// Removes `key`; returns whether a live (unexpired) value was removed.
    pub fn delete(&self, plugin_id: &str, key: &str) -> Result<bool, KvError> {
        validate(plugin_id, key)?;
        // An expired leftover is removed too, but it was already gone as far as callers know.
        let removed: Option<Option<i64>> = self
            .conn()
            .query_row(
                "DELETE FROM kv WHERE plugin_id = ?1 AND key = ?2 RETURNING expires_at",
                params![plugin_id, key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(match removed {
            Some(Some(expires_at)) => expires_at > now_secs(),
            Some(None) => true,
            None => false,
        })
    }

    /// The plugin's live keys starting with `prefix` (empty: all of them), sorted.
    pub fn list(&self, plugin_id: &str, prefix: &str) -> Result<Vec<String>, KvError> {
        PluginId::validate(plugin_id)?;
        let conn = self.conn();
        // `substr` rather than LIKE: the prefix is literal, so `_` and `%` in keys match only
        // themselves.
        let mut statement = conn.prepare(
            "SELECT key FROM kv
              WHERE plugin_id = ?1 AND substr(key, 1, length(?2)) = ?2
                AND (expires_at IS NULL OR expires_at > ?3)
              ORDER BY key",
        )?;
        let keys = statement
            .query_map(params![plugin_id, prefix, now_secs()], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(keys)
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave the connection half-written: every
        // statement is atomic in SQLite, so the connection is safe to keep using.
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Checks the namespace and key before anything touches the database.
fn validate(plugin_id: &str, key: &str) -> Result<(), KvError> {
    PluginId::validate(plugin_id)?;
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(KvError::Key(key.len()));
    }
    Ok(())
}

/// Wall-clock Unix seconds; deadlines are wall-clock so they survive restarts.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
