//! Console turns delegated to the selected DSH runtime without builtin session writes.

use super::*;
use kanon_llm::dsh::DshClient;
use serde_json::json;

pub(super) async fn completion(
    state: ApiState,
    client: Arc<DshClient>,
    request: ChatCompletionRequest,
) -> Result<Response, ApiError> {
    if request.persona_id.is_some()
        || request.protocol.is_some()
        || request.base_url.is_some()
        || request.api_key.is_some()
    {
        return Err(ApiError::BadRequest(
            "DSH owns personas and model settings; use the selected agent's settings".into(),
        ));
    }
    let session_id = request.session_id.trim().to_string();
    let engine = state.pipeline().cloned().ok_or_else(|| {
        ApiError::Unavailable("DSH console requires the running core pipeline".into())
    })?;
    MetricsRegistry::incr(&state.observability().metrics.chat_completions);
    let stream = request.stream;
    let turn = async move {
        let output = engine
            .run_dsh_console(
                client,
                request.instance_id.as_deref(),
                &session_id,
                request.message,
                request.tools,
                request.model.as_deref(),
            )
            .await
            .map_err(map_agent_error)?;
        Ok::<_, ApiError>(json!({
            "agent": "dsh", "session_id": session_id, "content": output.content,
            "executed_tools": output.executed_tools.into_iter().map(ToolCallView::from).collect::<Vec<_>>(),
            "finish_reason": "completed", "persona_id": null,
        }))
    };
    if !stream {
        return Ok(Json(turn.await?).into_response());
    }
    // Only durable assistant text is delivered. Dropping this SSE body drops run_turn's
    // caller, which signals the bounded remote owner to cancel before releasing its reservation.
    let events = futures_util::stream::once(async move {
        let payload = match turn.await {
            Ok(output) => {
                json!({"type": "done", "delta": output["content"], "agent": "dsh", "finish_reason": "completed"})
            }
            Err(error) => json!({"type": "error", "message": error.to_string()}),
        };
        Ok::<Event, Infallible>(Event::default().data(payload.to_string()))
    });
    Ok(Sse::new(events)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response())
}
