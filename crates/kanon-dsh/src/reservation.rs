//! Tracks locally admitted remote mutations through caller disconnect and cancellation.

use crate::DshError;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(crate) struct Reservations(Mutex<HashSet<String>>);

impl Reservations {
    /// Refuses a second writer until the remote owner has finished its cleanup.
    pub(crate) fn claim(self: &Arc<Self>, id: &str) -> Result<Reservation, DshError> {
        if !self
            .0
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
}

/// Owned by the bounded remote task, rather than by its possibly disconnected HTTP caller.
pub(crate) struct Reservation {
    owner: Arc<Reservations>,
    id: String,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.owner
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}
