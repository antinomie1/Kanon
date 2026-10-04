//! Backend selection and optional remote transport publication under the factory routing lock.

use super::*;

impl AgentFactory {
    /// Resolves the selected agent identity without interpreting a model override as a backend.
    pub fn agent_id(&self, instance_agent: Option<&str>) -> String {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        instance_agent.unwrap_or(&state.default_agent).to_string()
    }

    /// Publishes an already-prepared backend after its configuration has been saved.
    ///
    /// Default selection and client change together: no reader can see a DSH selection paired
    /// with a client from a different configuration generation.
    pub fn set_backend(
        &self,
        default_agent: &str,
        #[cfg(feature = "dsh")] dsh: Option<Arc<crate::dsh::DshClient>>,
    ) -> Result<(), String> {
        crate::agent::check_agent_id(default_agent)?;
        #[cfg(feature = "dsh")]
        if default_agent == "dsh" && dsh.is_none() {
            return Err("DSH must be configured before selecting it as the default agent".into());
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.default_agent = default_agent.into();
        #[cfg(feature = "dsh")]
        {
            state.dsh = dsh;
        }
        Ok(())
    }

    /// Prepares a transport without changing live state, preserving the connection pool when
    /// unrelated node settings changed. No connection or credential read happens here.
    #[cfg(feature = "dsh")]
    pub fn prepare_dsh(
        &self,
        config: Option<&crate::dsh::DshConfig>,
    ) -> Result<Option<Arc<crate::dsh::DshClient>>, String> {
        let Some(config) = config else {
            return Ok(None);
        };
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(client) = &state.dsh
            && client.config() == config
        {
            return Ok(Some(client.clone()));
        }
        drop(state);
        crate::dsh::DshClient::new(config.clone())
            .map(Arc::new)
            .map(Some)
            .map_err(|e| e.to_string())
    }

    /// Resolves a remote runtime for the chosen agent. `None` means an explicit builtin route;
    /// a selected but unconfigured DSH backend is an error, never a builtin fallback.
    #[cfg(feature = "dsh")]
    pub fn dsh_for(
        &self,
        instance_agent: Option<&str>,
    ) -> Result<Option<Arc<crate::dsh::DshClient>>, String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match instance_agent.unwrap_or(&state.default_agent) {
            crate::agent::BUILTIN_AGENT => Ok(None),
            "dsh" => state
                .dsh
                .clone()
                .map(Some)
                .ok_or_else(|| "DSH is selected but its connection is not configured".into()),
            unknown => Err(format!("unknown agent '{unknown}'")),
        }
    }

    /// Returns the configured remote client for its native management surface, regardless of
    /// which backend is the default for new conversations.
    #[cfg(feature = "dsh")]
    pub fn dsh_client(&self) -> Option<Arc<crate::dsh::DshClient>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .dsh
            .clone()
    }
}

/// A complete conversation runtime. Remote engines own their model and durable context;
/// selecting one never constructs a builtin provider or memory adapter for it.
#[derive(Clone)]
pub enum ConversationBackend {
    /// Kanon's local model loop.
    Builtin(Arc<dyn Agent>),
    /// Optional remote deepseek-harness runtime.
    #[cfg(feature = "dsh")]
    Dsh(Arc<crate::dsh::DshClient>),
}

impl ConversationBackend {
    /// Stable backend identity, independent of its provider or current model.
    pub fn id(&self) -> &'static str {
        match self {
            Self::Builtin(_) => crate::agent::BUILTIN_AGENT,
            #[cfg(feature = "dsh")]
            Self::Dsh(_) => "dsh",
        }
    }

    /// Returns the builtin runtime only when this route actually selected it.
    pub fn builtin(&self) -> Option<&Arc<dyn Agent>> {
        match self {
            Self::Builtin(agent) => Some(agent),
            #[cfg(feature = "dsh")]
            Self::Dsh(_) => None,
        }
    }
}

impl AgentFactory {
    /// Resolves backend first, and consults the node model directory only for builtin.
    /// `None` means builtin has no configured model; a missing remote connection is an error.
    pub fn conversation_backend(
        &self,
        instance_agent: Option<&str>,
        model: Option<&str>,
    ) -> Result<Option<ConversationBackend>, String> {
        #[cfg(feature = "dsh")]
        if let Some(client) = self.dsh_for(instance_agent)? {
            return Ok(Some(ConversationBackend::Dsh(client)));
        }
        #[cfg(not(feature = "dsh"))]
        crate::agent::check_agent_id(&self.agent_id(instance_agent))?;
        Ok(self
            .agent_for_model(model)
            .map(ConversationBackend::Builtin))
    }
}
