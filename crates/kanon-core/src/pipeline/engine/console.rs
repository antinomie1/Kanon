//! Console DSH admission uses the same bridge and instance tool selector as live turns.

use super::*;
use kanon_llm::{AgentError, ChatMessage, ToolRouterOutput, TurnOptions};

impl PipelineEngine {
    /// Runs a console session with native DSH model/history and Kanon's instance permissions.
    /// Console text cannot assert an IM sender identity, so it never unlocks Bash or hooks.
    pub async fn run_dsh_console(
        &self,
        client: Arc<kanon_llm::dsh::DshClient>,
        instance_id: Option<&str>,
        session_id: &str,
        message: String,
        use_tools: bool,
        native_model: Option<&str>,
    ) -> Result<ToolRouterOutput, AgentError> {
        // The durable id carries only its routing target. A caller cannot adopt another
        // instance's native context by supplying a different instance_id for the same journal.
        let route = session_id.strip_prefix("kanon-console-").and_then(|rest| {
            let uuid = rest.get(..36)?;
            if !uuid.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            }) || rest.get(36..37)? != "-"
            {
                return None;
            }
            rest.get(37..)
        });
        let expected = instance_id
            .map(|id| format!("i%3A{id}"))
            .unwrap_or_default();
        if route != Some(expected.as_str()) {
            return Err(AgentError::InvalidRequest(
                "DSH console session must encode its selected instance target".into(),
            ));
        }
        let factory = self.agent_factory().ok_or_else(|| {
            AgentError::InvalidRequest("Console requires the node's agent factory".into())
        })?;
        let instance = match instance_id {
            Some(id) => Some(
                self.instances()
                    .ok_or_else(|| {
                        AgentError::InvalidRequest("No instance catalog is attached".into())
                    })?
                    .get(id)
                    .await
                    .ok_or_else(|| {
                        AgentError::InvalidRequest(format!("Instance '{id}' does not exist"))
                    })?,
            ),
            None => None,
        };
        if let Some(instance) = &instance
            && factory.agent_id(instance.agent.as_deref()) != "dsh"
        {
            return Err(AgentError::InvalidRequest(
                "Instance no longer selects DSH".into(),
            ));
        }
        let running = self.running_turns().begin(instance_id.map(str::to_string));
        let writing = factory
            .sessions()
            .try_write(session_id)
            .map_err(|_| AgentError::Busy(session_id.into()))?;
        let hosts = match &instance {
            Some(instance) => self.hosts_of(instance).await,
            None => self.enabled_hosts().await,
        };
        let tool_hosts = if use_tools {
            self.tool_hosts(&hosts, instance.as_ref(), "").await
        } else {
            Vec::new()
        };
        let options = TurnOptions {
            without_tools: !use_tools,
            instructions: instance
                .as_ref()
                .filter(|instance| instance.conversation_rules)
                .map(|_| crate::simulation::CONVERSATION_RULES.into()),
            ..TurnOptions::default()
        };
        writing
            .scope(self.run_dsh_turn(
                client,
                &running,
                session_id,
                None,
                &[],
                tool_hosts,
                None,
                options,
                native_model,
                false,
                ChatMessage::user(message),
            ))
            .await
    }
}
