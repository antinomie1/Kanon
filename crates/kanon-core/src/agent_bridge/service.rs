//! Strongly typed IPC handlers; model arguments never select a caller or a platform event.

use super::*;
use kanon_proto::agent::v1::agent_bridge_service_server::AgentBridgeService;
use kanon_proto::agent::v1::{
    AgentToolRequest, AgentToolResponse, DescribeTurnRequest, DescribeTurnResponse,
};
use tonic::{Request, Response, Status};

#[tonic::async_trait]
impl AgentBridgeService for AgentBridge {
    async fn describe_turn(
        &self,
        request: Request<DescribeTurnRequest>,
    ) -> Result<Response<DescribeTurnResponse>, Status> {
        let request = request.into_inner();
        let turn = self.turn(&request.session_id)?;
        if request.request_id != turn.request_id {
            return Err(Status::failed_precondition("Prompt does not own this turn"));
        }
        Ok(Response::new(DescribeTurnResponse {
            lease_id: turn.lease_id.clone(),
            tools: turn
                .tools
                .definitions
                .iter()
                .map(|tool| kanon_proto::v1::ToolMeta {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: kanon_llm::tool_router::json_to_prost_struct(&tool.parameters),
                })
                .collect(),
            instructions: turn.instructions.clone(),
        }))
    }

    async fn call_tool(
        &self,
        request: Request<AgentToolRequest>,
    ) -> Result<Response<AgentToolResponse>, Status> {
        let request = request.into_inner();
        let turn = self.turn(&request.session_id)?;
        if request.lease_id != turn.lease_id {
            return Err(Status::failed_precondition("Tool lease expired"));
        }
        if request.call_id.is_empty() || request.call_id.len() > 512 || request.name.is_empty() {
            return Err(Status::invalid_argument(
                "Tool call requires a bounded identity and name",
            ));
        }
        let arguments = request
            .arguments
            .ok_or_else(|| Status::invalid_argument("Tool arguments are required"))?;
        let arguments = kanon_llm::tool_router::prost_struct_to_json(arguments)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        // DSH may schedule tools in parallel. Keep the same ordered side-effect contract as
        // builtin and never queue a second call behind a cancelled owner.
        let mut output = tokio::select! {
            biased;
            () = turn.signal.stopped() => return Err(Status::cancelled("Kanon turn stopped")),
            () = turn.revoked.stopped() => return Err(Status::cancelled("Kanon tool lease expired")),
            output = turn.output.lock() => output,
        };
        if output.calls.len() >= 4096 {
            return Err(Status::resource_exhausted("Turn tool-call limit reached"));
        }
        if !output.calls.insert(request.call_id.clone()) {
            return Err(Status::already_exists(
                "Call already admitted; execution must not be retried",
            ));
        }
        let call = kanon_llm::ToolCall {
            id: request.call_id,
            name: request.name,
            arguments,
        };
        let execution = crate::instance::with_tool_instance(
            turn.instance.clone(),
            crate::pipeline::agent_hook::with_turn(
                turn.event.clone(),
                turn.hosts.clone(),
                crate::with_bash_caller(
                    turn.caller.clone(),
                    kanon_llm::with_stop_signal(
                        turn.signal.clone(),
                        turn.tools.execute(
                            &request.session_id,
                            &call,
                            &turn.tools.definitions,
                            turn.factory.hooks(),
                        ),
                    ),
                ),
            ),
        );
        let result = tokio::select! {
            biased;
            () = turn.signal.stopped() => return Err(Status::cancelled("Kanon turn stopped")),
            () = turn.revoked.stopped() => return Err(Status::cancelled("Kanon tool lease expired")),
            result = execution => result.map_err(|e| Status::internal(e.to_string()))?,
        };
        let success = result.record.success;
        output.executed.push(result.record);
        for attachment in result.attachments {
            if !output.attachments.contains(&attachment) {
                output.attachments.push(attachment);
            }
        }
        Ok(Response::new(AgentToolResponse {
            success,
            text: result.text,
        }))
    }
}
