//! `BotApiService` conversation and persona RPCs: the sessions of one chat and the persona catalog.

use tonic::{Request, Response, Status};

use kanon_proto::v1::{
    AppendConversationRequest, AppendConversationResponse, ConversationList, ConversationsRequest,
    DeletePersonaRequest, DeletePersonaResponse, ListPersonasRequest, ListPersonasResponse,
    Persona, SelectConversationRequest, UpsertPersonaResponse,
};

use super::CoreApiService;

impl CoreApiService {
    /// Lists the conversations of the chat an inbound message belongs to.
    pub(super) async fn list_conversations_rpc(
        &self,
        _request: Request<ConversationsRequest>,
    ) -> Result<Response<ConversationList>, Status> {
        Err(Status::unimplemented(
            "list_conversations is not implemented yet",
        ))
    }

    /// Starts a new conversation in the chat and makes it current.
    pub(super) async fn new_conversation_rpc(
        &self,
        _request: Request<ConversationsRequest>,
    ) -> Result<Response<ConversationList>, Status> {
        Err(Status::unimplemented(
            "new_conversation is not implemented yet",
        ))
    }

    /// Makes another conversation of the chat current.
    pub(super) async fn switch_conversation_rpc(
        &self,
        _request: Request<SelectConversationRequest>,
    ) -> Result<Response<ConversationList>, Status> {
        Err(Status::unimplemented(
            "switch_conversation is not implemented yet",
        ))
    }

    /// Deletes one conversation of the chat.
    pub(super) async fn delete_conversation_rpc(
        &self,
        _request: Request<SelectConversationRequest>,
    ) -> Result<Response<ConversationList>, Status> {
        Err(Status::unimplemented(
            "delete_conversation is not implemented yet",
        ))
    }

    /// Appends finished user/assistant turns to the chat's current conversation.
    pub(super) async fn append_conversation_rpc(
        &self,
        _request: Request<AppendConversationRequest>,
    ) -> Result<Response<AppendConversationResponse>, Status> {
        Err(Status::unimplemented(
            "append_conversation is not implemented yet",
        ))
    }

    /// Lists the persona catalog.
    pub(super) async fn list_personas_rpc(
        &self,
        _request: Request<ListPersonasRequest>,
    ) -> Result<Response<ListPersonasResponse>, Status> {
        Err(Status::unimplemented(
            "list_personas is not implemented yet",
        ))
    }

    /// Creates or replaces a persona.
    pub(super) async fn upsert_persona_rpc(
        &self,
        _request: Request<Persona>,
    ) -> Result<Response<UpsertPersonaResponse>, Status> {
        Err(Status::unimplemented(
            "upsert_persona is not implemented yet",
        ))
    }

    /// Deletes a persona.
    pub(super) async fn delete_persona_rpc(
        &self,
        _request: Request<DeletePersonaRequest>,
    ) -> Result<Response<DeletePersonaResponse>, Status> {
        Err(Status::unimplemented(
            "delete_persona is not implemented yet",
        ))
    }
}
