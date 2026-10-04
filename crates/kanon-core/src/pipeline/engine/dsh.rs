//! DSH turn admission and scoped integration with Kanon's shared tool executor.

use super::*;
use crate::agent_bridge::{AgentBridge, BridgeTurn};
use kanon_llm::agent::tool_execution::TurnTools;
use kanon_llm::{
    AgentError, ChatMessage, StopSignal, ToolRouterError, ToolRouterOutput, TurnOptions,
};

impl PipelineEngine {
    /// Active external-agent capabilities served over the authenticated core IPC listener.
    pub fn agent_bridge(&self) -> &Arc<AgentBridge> {
        &self.dsh_bridge
    }

    pub(crate) async fn run_dsh_turn(
        &self,
        client: Arc<kanon_llm::dsh::DshClient>,
        running: &crate::pipeline::turns::TurnGuard<'_>,
        session_id: &str,
        event: Option<&PipelineEventRequest>,
        hosts: &[Arc<crate::supervisor::ManagedHost>],
        tool_hosts: Vec<Arc<dyn kanon_llm::tool_router::ToolHost>>,
        caller: Option<crate::BashCaller>,
        options: TurnOptions,
        native_model: Option<&str>,
        ephemeral: bool,
        mut message: ChatMessage,
    ) -> Result<ToolRouterOutput, ToolRouterError> {
        if options.persona.is_some() || options.max_iterations.is_some() {
            return Err(AgentError::InvalidRequest(
                "DSH owns persona and turn limits; configure them in DSH".into(),
            ));
        }
        let factory = self
            .agent_factory
            .as_ref()
            .ok_or_else(|| AgentError::InvalidRequest("DSH requires an agent factory".into()))?;
        let tools = if options.without_tools {
            TurnTools::default()
        } else {
            TurnTools::collect(factory.native_tools(), &tool_hosts)?
        };
        let mut instructions = options.instructions.unwrap_or_default();
        let signal = running.signal();
        crate::instance::with_tool_instance(
            running.instance(),
            crate::pipeline::agent_hook::with_optional_turn(
                event.cloned(),
                hosts.to_vec(),
                crate::with_bash_caller(caller.clone(), async {
                    for hook in factory.hooks() {
                        hook.on_system_prompt(session_id, &mut instructions, &tools.definitions)
                            .await?;
                        hook.on_user_message(session_id, &mut message).await?;
                    }
                    Ok::<_, AgentError>(())
                }),
            ),
        )
        .await?;
        // Each explicit invocation has its own identity, including repeated plugin RunAgent
        // calls on the same inbound event. Ingress deduplication must not alias those calls.
        let request_id = kanon_llm::dsh::DshClient::request_id();
        let lease_id = kanon_llm::dsh::DshClient::request_id();
        let guard = self.dsh_bridge.register(
            session_id,
            BridgeTurn {
                request_id: request_id.clone(),
                lease_id: lease_id.clone(),
                instance: running.instance(),
                factory: factory.clone(),
                tools,
                instructions,
                event: event.cloned(),
                hosts: hosts.to_vec(),
                caller,
                signal: signal.clone(),
                revoked: StopSignal::new(),
                output: Default::default(),
            },
        )?;
        // Preparation is required, so a missing/old native plugin fails before model input is
        // submitted. There is no fallback to an unscoped DSH tool environment.
        let content = kanon_llm::dsh::message_content(message)?;
        let prepared_session = session_id.to_string();
        let prepared_request = request_id.clone();
        let output = client
            .run_prepared_turn(
                session_id,
                &request_id,
                content,
                native_model.map(str::to_string),
                move |client| async move {
                    let ready: serde_json::Value = client
                        .call(
                            "kanon/prepare",
                            serde_json::json!({
                                "sessionId": prepared_session, "requestId": prepared_request,
                            }),
                        )
                        .await?;
                    if ready["leaseId"] != lease_id || ready["requestId"] != prepared_request {
                        return Err(kanon_llm::dsh::DshError::Protocol(
                            "DSH bridge prepared a different turn".into(),
                        ));
                    }
                    Ok(())
                },
                signal.stopped(),
                ephemeral,
            )
            .await
            .map_err(|error| match error {
                kanon_llm::dsh::DshError::Stopped => AgentError::Stopped,
                other => AgentError::Dsh(other),
            })?;
        let mut output = ToolRouterOutput {
            content: output.content,
            reasoning: None,
            executed_tools: Vec::new(),
            attachments: Vec::new(),
        };
        guard.finish(&mut output).await;
        Ok(output)
    }
}
