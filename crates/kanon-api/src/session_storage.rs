//! Durable session storage for the node (`data/sessions.db`).
//!
//! A conversation has two halves, and both must outlive the process for it to continue after a
//! restart: the **history** (messages and the compaction summary) and the **session record**
//! (persona binding, variables, turn counters, status). Both live in one SQLite database file, in
//! WAL mode, next to the node's other operator state.
//!
//! The session key a conversation continues under is derived from the bot instance and the
//! conversation (`instance:<id>:<conversation>#<generation>`), and both the instance catalog and
//! the generation counter are persisted too — so after a restart, or after an instance is edited,
//! the next message lands in the very same session.

use std::path::Path;
use std::sync::Arc;

use kanon_llm::{SessionManager, SqliteMemory, SqliteSessionStore};

/// Default location of the session database, relative to the node working directory.
pub const DEFAULT_SESSION_DB: &str = "./data/sessions.db";

/// Opens the session database and builds a [`SessionManager`] over it.
///
/// Every stored session is loaded before the manager is returned. A database that cannot be opened
/// or read is an error rather than a quiet fallback to an empty, in-memory manager: starting fresh
/// would look like the bot forgot every conversation.
pub fn open_session_manager(path: impl AsRef<Path>) -> Result<Arc<SessionManager>, String> {
    let path = path.as_ref();

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("Failed to create {}: {err}", parent.display()))?;
    }

    let describe = |err: &dyn std::fmt::Display| {
        format!(
            "Failed to open session storage at {}: {err}",
            path.display()
        )
    };
    let memory = SqliteMemory::open(path).map_err(|err| describe(&err))?;
    let store = SqliteSessionStore::open(path).map_err(|err| describe(&err))?;

    let sessions = SessionManager::new(Arc::new(memory))
        .with_store(Arc::new(store))
        .map_err(|err| describe(&err))?;

    tracing::info!(
        path = %path.display(),
        sessions = sessions.session_count(),
        "Session storage opened"
    );
    Ok(Arc::new(sessions))
}
