//! `BotApiService` agent RPCs: running the node's agent for a plugin, and runtime metadata refresh.

use tonic::{Request, Response, Status};

use kanon_proto::v1::{
    RefreshPluginMetaRequest, RefreshPluginMetaResponse, RunAgentRequest, RunAgentResponse,
};

use super::CoreApiService;

impl CoreApiService {
    /// Runs the node's agent on behalf of a plugin.
    pub(super) async fn run_agent_rpc(
        &self,
        _request: Request<RunAgentRequest>,
    ) -> Result<Response<RunAgentResponse>, Status> {
        Err(Status::unimplemented("run_agent is not implemented yet"))
    }

    /// Re-reads a host's plugin metadata after the plugin changed it at runtime.
    pub(super) async fn refresh_plugin_meta_rpc(
        &self,
        _request: Request<RefreshPluginMetaRequest>,
    ) -> Result<Response<RefreshPluginMetaResponse>, Status> {
        Err(Status::unimplemented(
            "refresh_plugin_meta is not implemented yet",
        ))
    }
}
