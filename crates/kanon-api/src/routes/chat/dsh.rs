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
        || request.model.is_some()
        || request.protocol.is_some()
        || request.base_url.is_some()
        || request.api_key.is_some()
    {
        return Err(ApiError::BadRequest(
            "DSH owns personas and model settings; use the selected agent's settings".into(),
        ));
    }
    let session_id = request.session_id.trim().to_string();
    let writing = state.sessions().try_write(&session_id)?;
    MetricsRegistry::incr(&state.observability().metrics.chat_completions);
    let stream = request.stream;
    let turn = async move {
        let _writing = writing;
        client
            .create_session(&session_id, None)
            .await
            .map_err(|error| ApiError::Upstream(error.to_string()))?;
        let output = client
            .run_turn(
                &session_id,
                &DshClient::request_id(),
                vec![json!({"type": "text", "text": request.message})],
                std::future::pending(),
            )
            .await
            .map_err(|error| ApiError::Upstream(error.to_string()))?;
        Ok::<_, ApiError>(json!({
            "agent": "dsh", "session_id": session_id, "content": output.content,
            "turn": output.turn, "through_seq": output.through_seq,
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
