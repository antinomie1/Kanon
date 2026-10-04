//! Borrowed publication context for backend-aware instance validation.

use super::*;

/// Node catalogs consulted while publishing an instance. This is a borrowed snapshot, not a
/// second copy of backend configuration. Embedded callers may omit it entirely.
#[derive(Clone, Copy)]
pub struct InstanceRuntime<'a> {
    /// Builtin persona definitions.
    pub personas: &'a PersonaRegistry,
    /// Builtin session bindings maintained when generated personas change.
    pub sessions: &'a SessionManager,
    /// Builtin model endpoints.
    pub providers: &'a ProviderRegistry,
    /// Default backend inherited by instances without their own choice.
    pub default_agent: &'a str,
}

impl InstanceRuntime<'_> {
    /// Whether builtin model and persona references will actually be used by this instance.
    pub(super) fn builtin(self, instance: &BotInstance) -> bool {
        instance.agent.as_deref().unwrap_or(self.default_agent) == kanon_llm::BUILTIN_AGENT
    }
}

impl InstanceRegistry {
    /// Validates restored references against the configured endpoints before serving messages.
    pub async fn validate_models(
        &self,
        providers: &ProviderRegistry,
        default_agent: &str,
    ) -> Result<(), InstanceError> {
        for instance in self.instances.read().await.values() {
            if instance.agent.as_deref().unwrap_or(default_agent) == kanon_llm::BUILTIN_AGENT {
                Self::validate_model(instance, Some(providers))?;
            }
        }
        Ok(())
    }

    /// Checks model ownership under the same instance guard that protects its publication.
    pub(super) fn validate_model(
        candidate: &BotInstance,
        providers: Option<&ProviderRegistry>,
    ) -> Result<(), InstanceError> {
        if let Some(providers) = providers
            && let Some(model) = candidate.model.as_deref()
        {
            providers
                .resolve(&kanon_llm::ModelRef::parse(model))
                .map_err(|error| {
                    InstanceError::Invalid(format!("instance '{}': {error}", candidate.id))
                })?;
        }
        Ok(())
    }

    /// Checks the live persona catalog only after taking the instance write lock.
    pub(super) fn validate_persona(
        candidate: &BotInstance,
        personas: Option<&PersonaRegistry>,
    ) -> Result<(), InstanceError> {
        if let Some(personas) = personas
            && let Some(id) = candidate.persona_id.as_deref()
            && personas.get(id).is_none()
        {
            return Err(InstanceError::Invalid(format!(
                "persona '{id}' does not exist on this node"
            )));
        }
        Ok(())
    }
}
