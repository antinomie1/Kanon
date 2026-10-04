//! The conversations of one chat: listing, starting, switching, deleting and appending to them.
//!
//! A chat (a private chat, a group member's session, or a shared group session) can hold several
//! conversations. Each is one session `instance:<id>:<conversation>#<generation>`; the instance
//! remembers which generation is current, and every other generation keeps its history in
//! `data/sessions.db` until it is deleted.
//!
//! One implementation serves both front doors: the built-in commands (`/ls`, `/new`, `/switch`,
//! `/del`, answered by the pipeline) and the plugin RPCs (`ListConversations`, `NewConversation`,
//! `SwitchConversation`, `DeleteConversation`, `AppendConversation`). The chat is always derived
//! from an inbound message exactly as when the model answers it, so a plugin and the model never
//! disagree about which conversation is meant.

#[cfg(feature = "dsh")]
mod dsh;

use std::sync::Arc;

use kanon_llm::{ChatMessage, Role, SessionManager};
use kanon_proto::v1::PipelineEventRequest;

use crate::instance::{BotInstance, InstanceError};

use super::engine::{PipelineEngine, instance_conversation_key};

/// Longest title shown for a conversation, in characters.
const TITLE_CHARS: usize = 24;

/// One conversation of a chat, as `/ls` and `ListConversations` show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationInfo {
    /// The conversation's session identifier.
    pub session_id: String,
    /// Its generation; conversations are listed in generation order (oldest first).
    pub generation: u64,
    /// Whether the chat's next message continues this conversation.
    pub current: bool,
    /// First user message, shortened; empty for a conversation without messages.
    pub title: String,
    /// User and assistant messages stored (summarized ones excluded).
    pub message_count: usize,
    /// Unix seconds of the last turn; 0 when it never had one.
    pub last_active_at: u64,
}

/// Why a conversation operation failed.
#[derive(Debug, thiserror::Error)]
pub enum ConversationError {
    /// The selected remote agent rejected or failed the operation.
    #[cfg(feature = "dsh")]
    #[error(transparent)]
    Dsh(#[from] kanon_llm::dsh::DshError),
    /// No enabled instance answers on the platform, so no conversation exists there.
    #[error("no enabled bot instance claims platform '{0}'")]
    NoInstance(String),
    /// Several instances claim the platform; the owner is never guessed.
    #[error("instance routing is ambiguous: {0}")]
    Ambiguous(String),
    /// The node has no conversation storage (no model was ever configured).
    #[error("no model is configured")]
    NoModel,
    /// The named session is not one of this chat's conversations.
    #[error("conversation '{0}' does not belong to this chat")]
    NotFound(String),
    /// A model turn (or another writer) is using the conversation right now.
    #[error("conversation '{0}' is busy with a running turn; try again when it has finished")]
    Busy(String),
    /// The request itself is malformed.
    #[error("{0}")]
    Invalid(String),
    /// The instance catalog could not record the change.
    #[error("the conversation choice could not be saved: {0}")]
    Instance(#[from] InstanceError),
    /// The session storage failed.
    #[error("conversation storage failed: {0}")]
    Storage(String),
}

/// A chat whose conversations are meant: the instance answering it and its conversation key.
pub(crate) struct Chat {
    pub(crate) instance: BotInstance,
    pub(crate) conversation: String,
}

/// Shortens a stored user message into a one-line title.
///
/// The stored text is what the model read, so it can start with context the core put in front of
/// the message (`[发送者: …]`, `[时间: …]`, a group log, a speaker label). Leading bracketed tags
/// are skipped and the last line is used, which is where the message itself sits.
fn title_from(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .rev()
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut rest = line;
    while let Some(after) = rest.strip_prefix('[') {
        match after.find(']') {
            Some(end) => rest = after[end + 1..].trim_start(),
            None => break,
        }
    }
    let rest = if rest.is_empty() { line } else { rest };
    let mut title: String = rest.chars().take(TITLE_CHARS).collect();
    if rest.chars().count() > TITLE_CHARS {
        title.push('…');
    }
    title
}

impl PipelineEngine {
    /// Session storage shared by every agent of the node, if a model was ever configured.
    pub(crate) fn session_manager(&self) -> Option<Arc<SessionManager>> {
        match self.agent_factory() {
            Some(factory) => Some(factory.sessions().clone()),
            None => self
                .agent_slot()
                .current()
                .and_then(|agent| agent.session_manager().cloned()),
        }
    }

    /// Resolves the chat an inbound message belongs to, exactly as the model phase does.
    pub(crate) async fn resolve_chat(
        &self,
        event: &PipelineEventRequest,
    ) -> Result<Chat, ConversationError> {
        let registry = self
            .instances()
            .ok_or_else(|| ConversationError::NoInstance(event.platform.clone()))?;
        let instance = match registry.resolve_by_platform(&event.platform).await {
            Ok(Some(instance)) => instance,
            Ok(None) => return Err(ConversationError::NoInstance(event.platform.clone())),
            Err(err) => return Err(ConversationError::Ambiguous(err.to_string())),
        };
        let conversation = instance_conversation_key(event, Some(&instance));
        Ok(Chat {
            instance,
            conversation,
        })
    }

    /// The chat's conversations, oldest first, the current one included even when it is empty.
    pub(crate) async fn chat_conversations(
        &self,
        chat: &Chat,
    ) -> Result<Vec<ConversationInfo>, ConversationError> {
        // The live record, not the copy resolved with the event: an earlier step of the same
        // command may just have changed the current generation.
        let instance = self.live_instance(&chat.instance).await?;
        #[cfg(feature = "dsh")]
        if let Some(client) = self.dsh_for(Some(&instance))? {
            return self
                .dsh_conversations(&instance, &chat.conversation, &client)
                .await;
        }
        let sessions = self.session_manager().ok_or(ConversationError::NoModel)?;
        let prefix = instance.conversation_session_prefix(&chat.conversation);
        let current = instance.session_generation(&chat.conversation);

        let mut conversations: Vec<ConversationInfo> = sessions
            .sessions_with_prefix(&prefix)
            .await
            .map_err(|err| ConversationError::Storage(err.to_string()))?
            .into_iter()
            .filter_map(|session| {
                // Only `<prefix><generation>` keys are this chat's; anything else merely shares
                // the prefix text.
                let generation = session.session_key[prefix.len()..].parse::<u64>().ok()?;
                Some(ConversationInfo {
                    current: generation == current,
                    title: session
                        .first_user_message
                        .as_deref()
                        .map(title_from)
                        .unwrap_or_default(),
                    message_count: session.message_count,
                    last_active_at: session.last_active_at,
                    session_id: session.session_key,
                    generation,
                })
            })
            .collect();
        if !conversations
            .iter()
            .any(|conversation| conversation.current)
        {
            conversations.push(ConversationInfo {
                session_id: instance.session_id_at(&chat.conversation, current),
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

    /// Starts a new, empty conversation and makes it current; returns its session id.
    ///
    /// The generation is one past every generation the chat ever used that still exists (and
    /// past the current one), so `/switch 1` followed by `/new` never lands in an old session.
    pub(crate) async fn start_conversation(
        &self,
        chat: &Chat,
    ) -> Result<String, ConversationError> {
        let conversations = self.chat_conversations(chat).await?;
        let next = conversations
            .iter()
            .map(|conversation| conversation.generation)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| ConversationError::Invalid("conversation generation overflow".into()))?;
        Ok(self.select_generation(chat, next).await?)
    }

    /// Makes an existing conversation of the chat current.
    pub(crate) async fn switch_conversation(
        &self,
        chat: &Chat,
        session_id: &str,
    ) -> Result<ConversationInfo, ConversationError> {
        let conversations = self.chat_conversations(chat).await?;
        let target = conversations
            .into_iter()
            .find(|conversation| conversation.session_id == session_id)
            .ok_or_else(|| ConversationError::NotFound(session_id.to_string()))?;
        self.select_generation(chat, target.generation).await?;
        Ok(ConversationInfo {
            current: true,
            ..target
        })
    }

    /// Deletes one conversation of the chat: its history, summary and session record.
    ///
    /// Deleting the current conversation leaves the chat on a new, empty one. A conversation in
    /// use by a running turn is refused rather than pulled out from under it.
    pub(crate) async fn delete_conversation(
        &self,
        chat: &Chat,
        session_id: &str,
    ) -> Result<ConversationInfo, ConversationError> {
        let sessions = self.session_manager().ok_or(ConversationError::NoModel)?;
        let conversations = self.chat_conversations(chat).await?;
        let target = conversations
            .iter()
            .find(|conversation| conversation.session_id == session_id)
            .cloned()
            .ok_or_else(|| ConversationError::NotFound(session_id.to_string()))?;
        let writing = sessions
            .try_write(session_id)
            .map_err(|_| ConversationError::Busy(session_id.to_string()))?;
        #[cfg(feature = "dsh")]
        let remote = self.dsh_for(Some(&chat.instance))?;
        #[cfg(feature = "dsh")]
        if let Some(client) = &remote {
            // Archive is DSH's restorable retirement operation. No builtin history is touched.
            client.archive_session(session_id).await?;
        }
        #[cfg(not(feature = "dsh"))]
        let remote: Option<()> = None;
        if remote.is_none() {
            writing
                .scope(sessions.delete_session(session_id))
                .await
                .map_err(|err| ConversationError::Storage(err.to_string()))?;
        }
        tracing::info!(
            instance_id = %chat.instance.id,
            session_id = %session_id,
            "Conversation deleted"
        );
        if target.current {
            // One past every generation listed before the deletion, the deleted one included:
            // the chat moves on instead of reopening a number it just used.
            let next = conversations
                .iter()
                .map(|conversation| conversation.generation)
                .max()
                .unwrap_or(0)
                + 1;
            self.select_generation(chat, next).await?;
        }
        Ok(target)
    }

    /// Appends finished user/assistant turns to the chat's current conversation.
    ///
    /// The messages must alternate user/assistant, start with a user message and end with an
    /// assistant one, so the history stays a sequence of complete turns. Refused while a turn
    /// runs in that conversation (see [`kanon_llm::SessionManager`]).
    pub(crate) async fn append_to_conversation(
        &self,
        chat: &Chat,
        messages: Vec<ChatMessage>,
    ) -> Result<String, ConversationError> {
        #[cfg(feature = "dsh")]
        if self.dsh_for(Some(&chat.instance))?.is_some() {
            return Err(ConversationError::Invalid(
                "DSH owns its journal and does not support importing completed builtin turns"
                    .into(),
            ));
        }
        if messages.is_empty() {
            return Err(ConversationError::Invalid(
                "at least one user/assistant pair is required".to_string(),
            ));
        }
        if messages.len() % 2 != 0
            || messages.iter().enumerate().any(|(index, message)| {
                message.role
                    != if index % 2 == 0 {
                        Role::User
                    } else {
                        Role::Assistant
                    }
            })
        {
            return Err(ConversationError::Invalid(
                "messages must alternate user/assistant, starting with user and ending with assistant"
                    .to_string(),
            ));
        }
        let sessions = self.session_manager().ok_or(ConversationError::NoModel)?;
        let instance = self.live_instance(&chat.instance).await?;
        let session_id = instance.conversation_session_id(&chat.conversation);
        let _writing = sessions
            .try_write(&session_id)
            .map_err(|_| ConversationError::Busy(session_id.clone()))?;
        let history = sessions
            .memory()
            .get_messages(&session_id)
            .await
            .map_err(|err| ConversationError::Storage(err.to_string()))?;
        if !history.is_empty() && !kanon_llm::compaction::ends_cleanly(&history) {
            return Err(ConversationError::Invalid(
                "resume or clear the interrupted conversation before appending imported turns"
                    .into(),
            ));
        }
        let turns = messages.len() / 2;
        sessions
            .memory()
            .extend_messages(&session_id, messages)
            .await
            .map_err(|err| ConversationError::Storage(err.to_string()))?;
        for _ in 0..turns {
            sessions.record_turn(&session_id, 0);
        }
        Ok(session_id)
    }

    /// Records `generation` as the chat's current conversation.
    async fn select_generation(
        &self,
        chat: &Chat,
        generation: u64,
    ) -> Result<String, ConversationError> {
        let registry = self
            .instances()
            .ok_or_else(|| ConversationError::NoInstance(String::new()))?;
        #[cfg(feature = "dsh")]
        if let Some(client) = self.dsh_for(Some(&chat.instance))? {
            let id = chat
                .instance
                .dsh_session_id_at(&chat.conversation, generation);
            // Remote creation precedes local publication. A disk failure leaves a recoverable
            // remote session rather than a routing pointer to a session that never existed.
            client.create_session(&id, None).await?;
            registry
                .select_session(
                    &chat.instance.id,
                    &BotInstance::dsh_routing_key(&chat.conversation),
                    generation,
                )
                .await?;
            return Ok(id);
        }
        Ok(registry
            .select_session(&chat.instance.id, &chat.conversation, generation)
            .await?)
    }

    /// The instance's current record; it may have changed since the event was resolved.
    async fn live_instance(
        &self,
        instance: &BotInstance,
    ) -> Result<BotInstance, ConversationError> {
        let registry = self
            .instances()
            .ok_or_else(|| ConversationError::NoInstance(String::new()))?;
        registry
            .get(&instance.id)
            .await
            .ok_or_else(|| InstanceError::NotFound(instance.id.clone()).into())
    }
}
