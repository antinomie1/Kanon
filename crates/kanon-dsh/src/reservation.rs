//! Tracks locally admitted remote mutations through caller disconnect and cancellation.

use crate::DshError;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(crate) struct Reservations {
    sessions: Mutex<HashSet<String>>,
    idle: tokio::sync::Notify,
}

impl Reservations {
    /// Refuses a second writer until the remote owner has finished its cleanup.
    pub(crate) fn claim(self: &Arc<Self>, id: &str) -> Result<Reservation, DshError> {
        if !self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.into())
        {
            return Err(DshError::Remote {
                code: "session/writer-held".into(),
                message: "DSH session has a running mutation or cancellation".into(),
            });
        }
        Ok(Reservation {
            owner: self.clone(),
            id: id.into(),
        })
    }
    pub(crate) fn is_idle(&self) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }

    /// Waits for admitted mutations and their remote cleanup, without retaining another task list.
    pub(crate) async fn wait_idle(&self) {
        loop {
            let idle = self.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if self.is_idle() {
                return;
            }
            idle.await;
        }
    }
}

/// Owned by the bounded remote task, rather than by its possibly disconnected HTTP caller.
pub(crate) struct Reservation {
    owner: Arc<Reservations>,
    id: String,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut sessions = self
            .owner
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.remove(&self.id);
        if sessions.is_empty() {
            self.owner.idle.notify_waiters();
        }
    }
}
