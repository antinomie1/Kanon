//! Active-turn capabilities for the optional DSH plugin on the core's existing gRPC endpoint.
//!
//! This is routing state only. No remote history, model or durable session data is stored here.

mod service;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use kanon_llm::agent::tool_execution::TurnTools;
use kanon_llm::{AgentFactory, StopSignal, ToolRouterOutput};
use kanon_proto::v1::PipelineEventRequest;
use tokio::sync::Mutex as AsyncMutex;

use crate::supervisor::ManagedHost;

/// Bounded by admitted pipeline turns; absent turns have no tool capabilities.
#[derive(Clone, Default)]
pub struct AgentBridge {
    turns: Arc<Mutex<HashMap<String, Arc<BridgeTurn>>>>,
    closing: Arc<std::sync::atomic::AtomicBool>,
    /// Cleanup can outlive a dropped capability guard and a replaced connection configuration.
    clients: Arc<Mutex<Vec<Arc<kanon_llm::dsh::DshClient>>>>,
}

/// One turn's immutable execution scope and collected delivery metadata.
pub(crate) struct BridgeTurn {
    /// Retained at admission, so endpoint reconfiguration cannot hide an active owner's cleanup.
    pub(crate) client: Arc<kanon_llm::dsh::DshClient>,
    pub(crate) request_id: String,
    pub(crate) lease_id: String,
    /// Trusted instance policy identity, independent of the remote session format.
    pub(crate) instance: Option<String>,
    pub(crate) factory: Arc<AgentFactory>,
    pub(crate) tools: TurnTools,
    pub(crate) instructions: String,
    pub(crate) event: Option<PipelineEventRequest>,
    pub(crate) hosts: Vec<Arc<ManagedHost>>,
    pub(crate) caller: Option<crate::BashCaller>,
    pub(crate) signal: StopSignal,
    pub(crate) revoked: StopSignal,
    pub(crate) output: AsyncMutex<BridgeOutput>,
}

/// Serializes side effects and retains successful media until platform delivery.
#[derive(Default)]
pub(crate) struct BridgeOutput {
    calls: HashSet<String>,
    executed: Vec<kanon_llm::tool_router::ExecutedToolCall>,
    attachments: Vec<kanon_llm::tool_router::ToolAttachment>,
}

impl AgentBridge {
    /// Publishes capabilities before the remote plugin prepares its session.
    pub(crate) fn register(
        self: &Arc<Self>,
        session: &str,
        turn: BridgeTurn,
    ) -> Result<BridgeGuard, kanon_llm::AgentError> {
        let mut turns = self
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.closing.load(std::sync::atomic::Ordering::Acquire) {
            return Err(kanon_llm::AgentError::Stopped);
        }
        if turns.contains_key(session) {
            return Err(kanon_llm::AgentError::Busy(session.into()));
        }
        {
            let mut clients = self
                .clients
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            clients.retain(|client| !client.is_idle());
            if !clients
                .iter()
                .any(|client| Arc::ptr_eq(client, &turn.client))
            {
                clients.push(turn.client.clone());
            }
        }
        let turn = Arc::new(turn);
        turns.insert(session.into(), turn.clone());
        Ok(BridgeGuard {
            bridge: self.clone(),
            session: session.into(),
            turn,
        })
    }

    /// Closes admission and signals every active native owner while retaining its client.
    /// The map lock makes closure atomic with registration; old endpoint clients are included.
    pub(crate) fn shutdown(&self) -> Vec<Arc<kanon_llm::dsh::DshClient>> {
        let turns = self
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.closing
            .store(true, std::sync::atomic::Ordering::Release);
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for turn in turns.values() {
            turn.signal.stop();
            turn.revoked.stop();
            if !clients
                .iter()
                .any(|client| Arc::ptr_eq(client, &turn.client))
            {
                clients.push(turn.client.clone());
            }
        }
        clients
    }

    fn turn(&self, session: &str) -> Result<Arc<BridgeTurn>, tonic::Status> {
        let turns = self
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let turn = turns.get(session).ok_or_else(|| {
            tonic::Status::failed_precondition("No active Kanon turn owns this DSH session")
        })?;
        if turn.signal.is_stopped() || turn.revoked.is_stopped() {
            return Err(tonic::Status::cancelled("Kanon turn stopped"));
        }
        Ok(turn.clone())
    }
}

/// Revokes capability before another turn can adopt the same remote session.
pub(crate) struct BridgeGuard {
    bridge: Arc<AgentBridge>,
    session: String,
    turn: Arc<BridgeTurn>,
}

impl BridgeGuard {
    pub(crate) async fn finish(&self, output: &mut ToolRouterOutput) {
        let mut collected = self.turn.output.lock().await;
        output.executed_tools.append(&mut collected.executed);
        output.attachments.append(&mut collected.attachments);
    }
}

impl Drop for BridgeGuard {
    fn drop(&mut self) {
        self.turn.revoked.stop();
        self.bridge
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.session);
    }
}
