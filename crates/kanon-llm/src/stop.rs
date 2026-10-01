//! Stopping a running turn from outside it.
//!
//! A turn can run for minutes: a model that keeps calling tools, a slow provider, a long shell
//! command. Whoever started the turn hands it a [`StopSignal`] through [`with_stop_signal`], and
//! anyone holding a clone can stop it. The agent stops only where it can leave history valid:
//! while waiting for the model or for a tool. A tool call already recorded gets a result saying
//! it was stopped, because providers reject a conversation whose tool calls lack results, so a
//! stopped turn never breaks the session's later requests.
//!
//! The signal travels as task-local state, like the turn's caller identity, so the agent API and
//! every caller between the pipeline and the agent stay unchanged.

use std::future::Future;
use std::sync::Arc;

use tokio::sync::watch;

/// A shared switch that stops the turn it was handed to; cloning shares the switch.
#[derive(Debug, Clone)]
pub struct StopSignal(Arc<watch::Sender<bool>>);

impl Default for StopSignal {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(false)))
    }
}

impl StopSignal {
    /// Creates a signal that has not been triggered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stops the turn. Idempotent; a turn that already finished is unaffected.
    pub fn stop(&self) {
        self.0.send_replace(true);
    }

    /// Whether [`StopSignal::stop`] has been called.
    pub fn is_stopped(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once the signal is triggered, at once if it already was.
    pub async fn stopped(&self) {
        let mut receiver = self.0.subscribe();
        // The sender lives in `self`, so the channel cannot close while this waits.
        let _ = receiver.wait_for(|stopped| *stopped).await;
    }
}

tokio::task_local! {
    static TURN_STOP: StopSignal;
}

/// Runs `turn` so that triggering `signal` stops it.
pub async fn with_stop_signal<F: Future>(signal: StopSignal, turn: F) -> F::Output {
    TURN_STOP.scope(signal, turn).await
}

/// The stop signal of the turn running in this task, if its caller provided one.
pub(crate) fn current() -> Option<StopSignal> {
    TURN_STOP.try_with(StopSignal::clone).ok()
}

/// Awaits `future` unless the turn is stopped first, in which case it is dropped and `None`
/// returned. Without a signal the future simply runs to completion.
pub(crate) async fn unless_stopped<F: Future>(
    signal: Option<&StopSignal>,
    future: F,
) -> Option<F::Output> {
    match signal {
        None => Some(future.await),
        Some(signal) => tokio::select! {
            // A turn stopped before the future started does not start it at all.
            biased;
            () = signal.stopped() => None,
            output = future => Some(output),
        },
    }
}
