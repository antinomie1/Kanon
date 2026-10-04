//! DSH conversation routing; only the current generation belongs to the local instance catalog.

use super::*;
use kanon_llm::dsh::DshClient;

impl PipelineEngine {
    /// Resolves DSH without consulting builtin model configuration or silently changing engines.
    pub(crate) fn dsh_for(
        &self,
        instance: Option<&BotInstance>,
    ) -> Result<Option<Arc<DshClient>>, ConversationError> {
        match self.agent_factory() {
            Some(factory) => factory
                .dsh_for(instance.and_then(|instance| instance.agent.as_deref()))
                .map_err(ConversationError::Invalid),
            None if instance.is_some_and(|instance| instance.agent.as_deref() == Some("dsh")) => {
                Err(ConversationError::Invalid(
                    "DSH requires a configured agent factory".into(),
                ))
            }
            None => Ok(None),
        }
    }

    pub(super) async fn dsh_conversations(
        &self,
        instance: &BotInstance,
        conversation: &str,
        client: &DshClient,
    ) -> Result<Vec<ConversationInfo>, ConversationError> {
        use futures_util::{StreamExt, TryStreamExt};
        let prefix = instance.dsh_session_prefix(conversation);
        let current = instance.dsh_session_generation(conversation);
        let sessions = client.sessions().await?;
        if sessions.iter().any(|session| {
            session.archived
                && session.session_id == instance.dsh_session_id_at(conversation, current)
        }) {
            return Err(ConversationError::Invalid(
                "current DSH session is archived; create a new conversation or restore it in DSH"
                    .into(),
            ));
        }
        let matching = sessions.into_iter().filter_map(|session| {
            if session.archived {
                return None;
            }
            let generation = session
                .session_id
                .strip_prefix(&prefix)?
                .parse::<u64>()
                .ok()?;
            Some((session, generation))
        });
        // Bounded concurrency keeps legacy message counts exact without retaining another store
        // or opening one connection per historical conversation at once.
        let mut conversations: Vec<_> =
            futures_util::stream::iter(matching.map(|(session, generation)| async move {
                let messages =
                    kanon_llm::dsh::conversation_messages(client, &session.session_id).await?;
                let title = session
                    .projections
                    .as_ref()
                    .and_then(|value| value["values"]["title"]["title"].as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        messages
                            .iter()
                            .find(|message| message.role == Role::User)
                            .and_then(|message| message.content.as_deref())
                            .map(title_from)
                            .unwrap_or_default()
                    });
                Ok::<_, kanon_llm::dsh::DshError>(ConversationInfo {
                    session_id: session.session_id,
                    generation,
                    current: generation == current,
                    title,
                    message_count: messages.len(),
                    last_active_at: session.updated_at / 1000,
                })
            }))
            .buffered(4)
            .try_collect()
            .await?;
        if !conversations
            .iter()
            .any(|conversation| conversation.current)
        {
            conversations.push(ConversationInfo {
                session_id: instance.dsh_session_id_at(conversation, current),
                generation: current,
                current: true,
                title: String::new(),
                message_count: 0,
                last_active_at: 0,
            });
        }
        conversations.sort_by_key(|conversation| conversation.generation);
        Ok(conversations)
    }

    /// Retired journals still reserve their generation; a new chat must never adopt one.
    pub(super) async fn dsh_next_generation(
        &self,
        chat: &Chat,
        client: &DshClient,
    ) -> Result<u64, ConversationError> {
        let instance = self.live_instance(&chat.instance).await?;
        let prefix = instance.dsh_session_prefix(&chat.conversation);
        client
            .sessions()
            .await?
            .iter()
            .filter_map(|session| {
                session
                    .session_id
                    .strip_prefix(&prefix)?
                    .parse::<u64>()
                    .ok()
            })
            .chain(std::iter::once(
                instance.dsh_session_generation(&chat.conversation),
            ))
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| ConversationError::Invalid("DSH session generation exhausted".into()))
    }
}
