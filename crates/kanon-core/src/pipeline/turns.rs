//! The model turns the pipeline is running, so `/stop` can reach them.
//!
//! A turn registers itself for as long as it runs, under the instance answering it. `/stop`
//! triggers the [`StopSignal`] of every turn of its instance; the agent then ends the turn at its
//! next wait on the model or a tool (see [`kanon_llm::stop`]).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use kanon_llm::StopSignal;

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
