//! `BotApiService` conversation and persona RPCs: the sessions of one chat and the persona catalog.
//!
//! The conversation RPCs are thin: the pipeline owns how an inbound message maps to a chat and
//! its sessions, and serves the built-in `/ls`, `/new`, `/switch` and `/del` from the same code
//! (see [`crate::pipeline::conversations`]). The persona RPCs change the operator's catalog
//! through the same [`kanon_llm::PersonaStore`] the console uses, so the two can never disagree.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use kanon_llm::{ChatMessage, PersonaChangeError, PersonaError, PersonaKind};
use kanon_proto::v1::{
    AppendConversationRequest, AppendConversationResponse, ConversationInfo, ConversationList,
    ConversationsRequest, DeletePersonaRequest, DeletePersonaResponse, ListPersonasRequest,
    ListPersonasResponse, LlmRole, Persona, PipelineEventRequest, SelectConversationRequest,
    UpsertPersonaResponse,
};

use super::CoreApiService;
use crate::pipeline::PipelineEngine;
use crate::pipeline::conversations::{Chat, ConversationError};

/// Maps a conversation failure onto the status the protocol documents for it.
pub(super) fn conversation_status(err: ConversationError) -> Status {
    match err {
        ConversationError::NoInstance(_) | ConversationError::NotFound(_) => {
            Status::not_found(err.to_string())
        }
        ConversationError::NoModel => Status::unavailable(err.to_string()),
        ConversationError::Busy(_) => Status::failed_precondition(err.to_string()),
        ConversationError::Invalid(_) => Status::invalid_argument(err.to_string()),
        ConversationError::Ambiguous(_)
        | ConversationError::Instance(_)
        | ConversationError::Storage(_) => Status::internal(err.to_string()),
    }
}

/// Maps a refused persona change onto its status.
fn persona_status(err: PersonaChangeError) -> Status {
    match err {
        PersonaChangeError::Persona(
            PersonaError::InvalidId(_) | PersonaError::EmptyName | PersonaError::EmptyPrompt,
        ) => Status::invalid_argument(err.to_string()),
        PersonaChangeError::Persona(PersonaError::NotFound(_)) => {
            Status::not_found(err.to_string())
        }
        PersonaChangeError::Persona(PersonaError::ReadOnly(_))
        | PersonaChangeError::InstanceOwned(_) => Status::failed_precondition(err.to_string()),
        PersonaChangeError::Storage(_) => Status::internal(err.to_string()),
    }
}

impl CoreApiService {
    /// The pipeline whose conversations these RPCs act on.
    fn conversation_engine(&self) -> Result<&Arc<PipelineEngine>, Status> {
        self.engine.as_ref().ok_or_else(|| {
            Status::unavailable("This core runs no pipeline; there are no conversations")
        })
    }

    /// Resolves the chat named by an RPC's `context`.
    async fn chat_of(&self, context: Option<PipelineEventRequest>) -> Result<Chat, Status> {
        let event = context.ok_or_else(|| {
            Status::invalid_argument("the chat must be named by an inbound message as `context`")
        })?;
        self.conversation_engine()?
            .resolve_chat(&event)
            .await
            .map_err(conversation_status)
    }

    /// The chat's conversations in wire form.
    async fn conversation_list(&self, chat: &Chat) -> Result<ConversationList, Status> {
        let conversations = self
            .conversation_engine()?
            .chat_conversations(chat)
            .await
            .map_err(conversation_status)?;
        Ok(ConversationList {
            conversations: conversations
                .into_iter()
                .map(|conversation| ConversationInfo {
                    session_id: conversation.session_id,
                    current: conversation.current,
                    title: conversation.title,
                    message_count: u32::try_from(conversation.message_count).unwrap_or(u32::MAX),
                    last_active_at: i64::try_from(conversation.last_active_at).unwrap_or(i64::MAX),
                })
                .collect(),
        })
    }

    /// Lists the conversations of the chat an inbound message belongs to.
    pub(super) async fn list_conversations_rpc(
        &self,
        request: Request<ConversationsRequest>,
    ) -> Result<Response<ConversationList>, Status> {
        let chat = self.chat_of(request.into_inner().context).await?;
        Ok(Response::new(self.conversation_list(&chat).await?))
    }

    /// Starts a new conversation in the chat and makes it current.
    pub(super) async fn new_conversation_rpc(
        &self,
        request: Request<ConversationsRequest>,
    ) -> Result<Response<ConversationList>, Status> {
        let chat = self.chat_of(request.into_inner().context).await?;
        self.conversation_engine()?
            .start_conversation(&chat)
            .await
            .map_err(conversation_status)?;
        Ok(Response::new(self.conversation_list(&chat).await?))
    }

    /// Makes another conversation of the chat current.
    pub(super) async fn switch_conversation_rpc(
        &self,
        request: Request<SelectConversationRequest>,
    ) -> Result<Response<ConversationList>, Status> {
        let req = request.into_inner();
        let chat = self.chat_of(req.context).await?;
        self.conversation_engine()?
            .switch_conversation(&chat, &req.session_id)
            .await
            .map_err(conversation_status)?;
        Ok(Response::new(self.conversation_list(&chat).await?))
    }

    /// Deletes one conversation of the chat.
    pub(super) async fn delete_conversation_rpc(
        &self,
        request: Request<SelectConversationRequest>,
    ) -> Result<Response<ConversationList>, Status> {
        let req = request.into_inner();
        let chat = self.chat_of(req.context).await?;
        self.conversation_engine()?
            .delete_conversation(&chat, &req.session_id)
            .await
            .map_err(conversation_status)?;
        Ok(Response::new(self.conversation_list(&chat).await?))
    }

    /// Appends finished user/assistant turns to the chat's current conversation.
    pub(super) async fn append_conversation_rpc(
        &self,
        request: Request<AppendConversationRequest>,
    ) -> Result<Response<AppendConversationResponse>, Status> {
        let req = request.into_inner();
        let messages = req
            .messages
            .into_iter()
            .map(|message| match LlmRole::try_from(message.role) {
                Ok(LlmRole::User) => Ok(ChatMessage::user(message.text)),
                Ok(LlmRole::Assistant) => Ok(ChatMessage::assistant(message.text)),
                _ => Err(Status::invalid_argument(
                    "AppendConversation takes only user and assistant messages",
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let chat = self.chat_of(req.context).await?;
        let session_id = self
            .conversation_engine()?
            .append_to_conversation(&chat, messages)
            .await
            .map_err(conversation_status)?;
        Ok(Response::new(AppendConversationResponse { session_id }))
    }

    /// The persona catalog and its store, when the node shares them with the IPC service.
    fn persona_catalog(
        &self,
    ) -> Result<
        &(
            Arc<kanon_llm::PersonaRegistry>,
            Arc<kanon_llm::PersonaStore>,
        ),
        Status,
    > {
        self.personas
            .as_ref()
            .ok_or_else(|| Status::unavailable("This core has no persona catalog"))
    }

    /// Lists the persona catalog: the base assistant, the operator's and the instances' personas.
    pub(super) async fn list_personas_rpc(
        &self,
        _request: Request<ListPersonasRequest>,
    ) -> Result<Response<ListPersonasResponse>, Status> {
        let (registry, _) = self.persona_catalog()?;
        Ok(Response::new(ListPersonasResponse {
            personas: registry
                .list()
                .into_iter()
                .map(|persona| Persona {
                    builtin: persona.kind == PersonaKind::Builtin,
                    id: persona.id,
                    name: persona.name,
                    prompt: persona.prompt,
                })
                .collect(),
        }))
    }

    /// Creates or replaces an operator-defined persona.
    ///
    /// The name defaults to the id; a replaced persona keeps its description, which the protocol
    /// does not carry.
    pub(super) async fn upsert_persona_rpc(
        &self,
        request: Request<Persona>,
    ) -> Result<Response<UpsertPersonaResponse>, Status> {
        let req = request.into_inner();
        let (registry, store) = self.persona_catalog()?;
        let id = req.id.trim().to_string();
        let name = if req.name.trim().is_empty() {
            id.clone()
        } else {
            req.name
        };
        let description = registry
            .get(&id)
            .map(|existing| existing.description)
            .unwrap_or_default();
        let persona = kanon_llm::Persona::custom(id, name, description, req.prompt)
            .map_err(|err| persona_status(err.into()))?;
        let replaced = store.upsert(registry, persona).map_err(persona_status)?;
        Ok(Response::new(UpsertPersonaResponse { replaced }))
    }

    /// Deletes an operator-defined persona; `deleted` is false when no persona had the id.
    ///
    /// Refused while a bot instance selects the persona. Sessions bound to it fall back to the
    /// base assistant, exactly as when the console deletes it.
    pub(super) async fn delete_persona_rpc(
        &self,
        request: Request<DeletePersonaRequest>,
    ) -> Result<Response<DeletePersonaResponse>, Status> {
        let id = request.into_inner().id;
        let (registry, store) = self.persona_catalog()?;
        if registry.get(&id).is_none() {
            return Ok(Response::new(DeletePersonaResponse { deleted: false }));
        }
        if let Some(instances) = self.engine.as_ref().and_then(|engine| engine.instances()) {
            let users = instances.instances_using_persona(&id).await;
            if !users.is_empty() {
                return Err(Status::failed_precondition(format!(
                    "persona '{id}' is used by instance(s) {}",
                    users.join(", ")
                )));
            }
        }
        store.remove(registry, &id).map_err(persona_status)?;
        if let Some(sessions) = self
            .engine
            .as_ref()
            .and_then(|engine| engine.session_manager())
        {
            sessions.unbind_persona(&id);
        }
        Ok(Response::new(DeletePersonaResponse { deleted: true }))
    }
}
