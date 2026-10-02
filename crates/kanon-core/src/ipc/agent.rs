//! `BotApiService` agent RPCs: running the node's agent for a plugin, and runtime metadata refresh.

use base64::Engine;
use tonic::{Request, Response, Status};

use kanon_llm::ContentPart;
use kanon_proto::v1::image_segment::Source;
use kanon_proto::v1::{
    ImageSegment, RefreshPluginMetaRequest, RefreshPluginMetaResponse, RunAgentRequest,
    RunAgentResponse,
};

use super::CoreApiService;
use super::conversations::conversation_status;
use crate::pipeline::{AgentRun, AgentRunError, MAX_INBOUND_IMAGE_BYTES};
use crate::supervisor::SupervisorError;

/// Converts a plugin's images into model input, rejecting what cannot be sent.
///
/// URLs and file paths pass through; raw bytes become an inline `data:` URL, which needs the
/// image's MIME type because providers cannot guess it.
#[allow(clippy::result_large_err)]
pub(super) fn image_parts(
    images: Vec<ImageSegment>,
    what: &str,
) -> Result<Vec<ContentPart>, Status> {
    images
        .into_iter()
        .map(|image| {
            let mime_type = image
                .mime_type
                .map(|mime| mime.trim().to_string())
                .filter(|mime| !mime.is_empty());
            match image.source {
                Some(Source::Url(url)) if !url.trim().is_empty() => {
                    Ok(ContentPart::image_url(url, mime_type))
                }
                Some(Source::FilePath(path)) if !path.trim().is_empty() => {
                    Ok(ContentPart::image_file(path, mime_type))
                }
                Some(Source::RawBytes(bytes)) => {
                    let Some(mime) = mime_type.filter(|mime| mime.starts_with("image/")) else {
                        return Err(Status::invalid_argument(format!(
                            "{what}: an image given as bytes needs its `mime_type` (image/...)"
                        )));
                    };
                    if bytes.is_empty() || bytes.len() > MAX_INBOUND_IMAGE_BYTES {
                        return Err(Status::invalid_argument(format!(
                            "{what}: an image must be 1 byte to {MAX_INBOUND_IMAGE_BYTES} bytes, \
                             got {}",
                            bytes.len()
                        )));
                    }
                    let url = format!(
                        "data:{mime};base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(&bytes)
                    );
                    Ok(ContentPart::image_url(url, Some(mime)))
                }
                _ => Err(Status::invalid_argument(format!(
                    "{what}: an image needs a URL, a file path or bytes"
                ))),
            }
        })
        .collect()
}

/// Maps a failed run onto the status the protocol documents for it.
fn run_status(err: AgentRunError) -> Status {
    match err {
        AgentRunError::Conversation(err) => conversation_status(err),
        AgentRunError::UnknownModel(_) | AgentRunError::Invalid(_) => {
            Status::invalid_argument(err.to_string())
        }
        AgentRunError::Stopped => Status::aborted(err.to_string()),
        AgentRunError::Failed(_) => Status::unavailable(err.to_string()),
    }
}

impl CoreApiService {
    /// Runs the node's agent on behalf of a plugin.
    pub(super) async fn run_agent_rpc(
        &self,
        request: Request<RunAgentRequest>,
    ) -> Result<Response<RunAgentResponse>, Status> {
        let req = request.into_inner();
        let engine = self
            .engine
            .as_ref()
            .ok_or_else(|| Status::unavailable("This core runs no pipeline; there is no agent"))?;
        let run = AgentRun {
            images: image_parts(req.images, "RunAgent")?,
            plugin_id: req.plugin_id,
            prompt: req.prompt,
            context: req.context,
            in_conversation: req.in_conversation,
            instructions: Some(req.system_prompt).filter(|prompt| !prompt.trim().is_empty()),
            model: Some(req.model).filter(|model| !model.trim().is_empty()),
            use_tools: req.use_tools,
            max_steps: (req.max_steps > 0).then_some(req.max_steps as usize),
        };
        let output = engine.run_agent(run).await.map_err(run_status)?;
        Ok(Response::new(RunAgentResponse {
            content: output.content,
            attachments: output
                .attachments
                .iter()
                .map(kanon_llm::ToolAttachment::to_proto)
                .collect(),
            tools: output.tools,
            session_id: output.session_id,
        }))
    }

    /// Re-reads a host's plugin metadata after the plugin changed it at runtime.
    pub(super) async fn refresh_plugin_meta_rpc(
        &self,
        request: Request<RefreshPluginMetaRequest>,
    ) -> Result<Response<RefreshPluginMetaResponse>, Status> {
        let host_id = request.into_inner().host_id;
        let supervisor = self
            .supervisor
            .as_ref()
            .ok_or_else(|| Status::unavailable("no supervisor is attached to this core"))?;
        let plugin_ids =
            supervisor
                .refresh_plugin_meta(&host_id)
                .await
                .map_err(|err| match err {
                    SupervisorError::HostNotFound(_) => Status::not_found(err.to_string()),
                    SupervisorError::PluginNotFound(_) => {
                        Status::failed_precondition(err.to_string())
                    }
                    SupervisorError::Rpc(status) => *status,
                    other => Status::internal(other.to_string()),
                })?;
        Ok(Response::new(RefreshPluginMetaResponse { plugin_ids }))
    }
}
