//! Atomic publication of conversation generation choices.

use super::*;

impl InstanceRegistry {
    /// Makes `generation` the current session of one conversation and returns its identifier.
    ///
    /// Used by `/new` (a generation no session has used), `/switch` (an existing one) and `/del`.
    /// Other sessions of the conversation are left untouched: their history stays where it is.
    pub async fn select_session(
        &self,
        id: &str,
        conversation: &str,
        generation: u64,
    ) -> Result<String, InstanceError> {
        self.select_session_checked(id, conversation, generation, None)
            .await
    }

    /// Publishes a routing choice only while its admission snapshot is still current.
    /// Remote session creation must not overwrite a concurrent backend or routing edit.
    pub(crate) async fn select_session_checked(
        &self,
        id: &str,
        conversation: &str,
        generation: u64,
        expected: Option<&BotInstance>,
    ) -> Result<String, InstanceError> {
        let mut instances = self.instances.write().await;
        if let Some(expected) = expected
            && instances.get(id) != Some(expected)
        {
            return Err(InstanceError::Invalid(
                "instance changed during conversation selection; retry with its current route"
                    .into(),
            ));
        }
        let mut next = instances.clone();
        let instance = next
            .get_mut(id)
            .ok_or_else(|| InstanceError::NotFound(id.to_string()))?;

        instance
            .session_generations
            .insert(conversation.to_string(), generation);
        let session_id = instance.conversation_session_id(conversation);
        self.commit(&mut instances, next)?;

        Ok(session_id)
    }
}
