//! Shared, hot-swappable handle to the node's agent runtime.
//!
//! # Why a slot instead of a plain `Arc<dyn Agent>`
//! The model provider is no longer decided once at process start: the management console can
//! configure, replace or clear it on a running node. Every consumer of the provider — the
//! pipeline worker that answers chat messages, the management gateway that serves
//! `/api/v1/chat/completions`, and the IPC `RequestLLM` gateway used by plugin hosts — must
//! therefore read the *current* agent at call time instead of capturing one at construction.
//!
//! [`AgentSlot`] is that single point of truth. It is deliberately tiny: a read-mostly
//! `Option<Arc<dyn Agent>>` behind a synchronous lock, so consumers pay one short lock acquisition
//! plus an `Arc` clone per call and never hold the lock across an `await`.

use std::sync::{Arc, RwLock};

use crate::agent::Agent;

/// Shared handle resolving the node's active [`Agent`], if any.
///
/// An empty slot means "no model provider is configured", which is a valid, reportable state
/// (chat completions answer `503` rather than inventing a provider).
#[derive(Default)]
pub struct AgentSlot {
    /// Current agent. `None` until a provider is configured.
    ///
    /// A synchronous lock is used on purpose: every operation on the guarded value is a clone
    /// of an `Arc`, so there is nothing to await and no reason to pay for an async lock.
    current: RwLock<Option<Arc<dyn Agent>>>,
}

impl std::fmt::Debug for AgentSlot {
    /// Reports only the provider identity: the agent itself holds memory, hooks and credentials.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let current = self.read();
        f.debug_struct("AgentSlot")
            .field("configured", &current.is_some())
            .field(
                "model",
                &current
                    .as_ref()
                    .map(|agent| agent.config().default_model.as_str()),
            )
            .finish()
    }
}

impl AgentSlot {
    /// Creates an empty slot (no provider configured).
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a slot that already holds an agent.
    pub fn with_agent(agent: Arc<dyn Agent>) -> Self {
        Self {
            current: RwLock::new(Some(agent)),
        }
    }

    /// Returns the currently configured agent, if any.
    pub fn current(&self) -> Option<Arc<dyn Agent>> {
        self.read().clone()
    }

    /// Replaces the configured agent; `None` clears the provider.
    pub fn set(&self, agent: Option<Arc<dyn Agent>>) {
        *self.write() = agent;
    }

    /// Returns whether a provider is currently configured.
    pub fn is_configured(&self) -> bool {
        self.read().is_some()
    }

    /// Acquires the read guard.
    ///
    /// A poisoned lock can only mean another thread panicked while swapping a plain
    /// `Option<Arc<dyn Agent>>`; the value itself cannot be left inconsistent, so the guard is
    /// recovered explicitly instead of propagating a panic into every request handler.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Option<Arc<dyn Agent>>> {
        self.current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Acquires the write guard, recovering from poisoning for the same reason as [`Self::read`].
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Option<Arc<dyn Agent>>> {
        self.current
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
