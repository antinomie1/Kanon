//! The model turns the pipeline is running, so `/stop` can reach them.
//!
//! A turn registers itself for as long as it runs, under the instance answering it. `/stop`
//! triggers the [`StopSignal`] of every turn of its instance; the agent then ends the turn at its
//! next wait on the model or a tool (see [`kanon_llm::stop`]).
//!
//! [`SessionLocks`] serializes the writers of one conversation: a model turn holds its session's
//! lock while it runs, and anything else that changes that conversation (deleting it, appending
//! turns from a plugin, a plugin's agent run inside it) only proceeds when it can take the lock at
//! once. A turn writes its messages one by one as it goes, so another writer slipping in between
//! would break the user → assistant order the next request is built from.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use kanon_llm::StopSignal;
use tokio::sync::OwnedMutexGuard;

/// Registry of running turns. Held only for map updates; never across an await.
#[derive(Default)]
pub(crate) struct RunningTurns {
    next_id: AtomicU64,
    turns: Mutex<HashMap<u64, RunningTurn>>,
}

struct RunningTurn {
    /// Instance answering the turn; `None` in a pipeline without instances.
    instance: Option<String>,
    signal: StopSignal,
}

impl RunningTurns {
    /// Registers a turn of `instance` until the returned guard is dropped.
    pub(crate) fn begin(&self, instance: Option<String>) -> TurnGuard<'_> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let signal = StopSignal::new();
        self.lock().insert(
            id,
            RunningTurn {
                instance,
                signal: signal.clone(),
            },
        );
        TurnGuard {
            turns: self,
            id,
            signal,
        }
    }

    /// Stops every running turn of `instance` and returns how many there were.
    ///
    /// A turn that was already stopped is counted again: it is still running until it reaches
    /// its next wait, and the sender should hear that something was stopped.
    pub(crate) fn stop_instance(&self, instance: &str) -> usize {
        let turns = self.lock();
        let mut stopped = 0;
        for turn in turns.values() {
            if turn.instance.as_deref() == Some(instance) {
                turn.signal.stop();
                stopped += 1;
            }
        }
        stopped
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, RunningTurn>> {
        // The map holds plain data and every critical section is a single insert, remove or
        // scan, so a panic elsewhere cannot leave it half-updated; recovering keeps /stop usable.
        self.turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Keeps a turn registered while it runs; dropping it unregisters the turn.
pub(crate) struct TurnGuard<'a> {
    turns: &'a RunningTurns,
    id: u64,
    signal: StopSignal,
}

impl TurnGuard<'_> {
    /// The signal to run the turn under, see [`kanon_llm::with_stop_signal`].
    pub(crate) fn signal(&self) -> StopSignal {
        self.signal.clone()
    }
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        self.turns.lock().remove(&self.id);
    }
}

/// One lock per conversation session, held by whoever is writing that session.
///
/// Entries are weak: a session nobody holds or waits for takes no memory beyond its map slot,
/// and dead slots are swept whenever a new session is locked.
#[derive(Default)]
pub(crate) struct SessionLocks {
    locks: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
}

impl SessionLocks {
    /// Waits for the session's lock; used by the pipeline's own turns, which never contend with
    /// each other (the worker answers one event at a time) and only wait out a short write.
    pub(crate) async fn lock(&self, session_id: &str) -> OwnedMutexGuard<()> {
        self.entry(session_id).lock_owned().await
    }

    /// Takes the session's lock only if nobody holds it; `None` means the session is busy.
    pub(crate) fn try_lock(&self, session_id: &str) -> Option<OwnedMutexGuard<()>> {
        self.entry(session_id).try_lock_owned().ok()
    }

    fn entry(&self, session_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        // Plain data behind a short critical section; a panic elsewhere cannot leave it torn.
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(lock) = locks.get(session_id).and_then(Weak::upgrade) {
            return lock;
        }
        locks.retain(|_, lock| lock.strong_count() > 0);
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(session_id.to_string(), Arc::downgrade(&lock));
        lock
    }
}
