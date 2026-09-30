//! Integration tests for Core IPC `RequestLLM` streaming RPC.

use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tonic::{Request, async_trait};

use kanon_core::ipc::CoreApiService;
use kanon_llm::error::GatewayError;
use kanon_llm::gateway::types::{ChatChunk, ChatRequest, ChatResponse};
use kanon_llm::gateway::{ChatChunkStream, LlmGateway, LlmProvider};
use kanon_proto::v1::LlmRequest;
use kanon_proto::v1::bot_api_service_server::BotApiService;

struct MockStreamingProvider;

#[async_trait]
impl LlmProvider for MockStreamingProvider {
    async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        Ok(ChatResponse {
            reasoning_content: None,
            content: Some("Full content".to_string()),
            tool_calls: vec![],
            finish_reason: Some("stop".to_string()),
            usage: None,
        })
    }

    async fn chat_stream(&self, _request: &ChatRequest) -> Result<ChatChunkStream, GatewayError> {
        let (tx, rx) = mpsc::channel(3);
        tokio::spawn(async move {
            let _ = tx.send(Ok(ChatChunk::delta("Hello "))).await;
            let _ = tx.send(Ok(ChatChunk::delta("from "))).await;
            let _ = tx.send(Ok(ChatChunk::delta("gRPC stream!"))).await;
            let _ = tx.send(Ok(ChatChunk::done(Some("stop".to_string())))).await;
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

#[tokio::test]
async fn test_core_request_llm_streaming_with_gateway() {
    let (event_tx, _event_rx) = mpsc::channel(16);
    let provider = Arc::new(MockStreamingProvider);
    let gateway = Arc::new(LlmGateway::new(provider, "test-model"));

    let service = CoreApiService::new(event_tx).with_gateway(gateway);

    let request = Request::new(LlmRequest {
        prompt: "Tell me a joke".to_string(),
        model: "test-model".to_string(),
        parameters: None,
    });

    let response = service
        .request_llm(request)
        .await
        .expect("RequestLLM failed");

    let mut stream = response.into_inner();

    let mut accumulated = String::new();
    let mut finished = false;

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.expect("Error in stream chunk");
        accumulated.push_str(&chunk.delta_text);
        if chunk.is_finished {
            finished = true;
        }
    }

    assert!(finished);
    assert_eq!(accumulated, "Hello from gRPC stream!");
}

#[tokio::test]
async fn test_core_request_llm_unavailable_without_provider() {
    let (event_tx, _event_rx) = mpsc::channel(16);
    let service = CoreApiService::new(event_tx); // No agent slot configured

    let request = Request::new(LlmRequest {
        prompt: "ping".to_string(),
        model: "default".to_string(),
        parameters: None,
    });

    // A node without a provider must refuse explicitly. Answering with a synthetic completion
    // would let a plugin mistake a misconfigured node for a working model backend.
    let status = service
        .request_llm(request)
        .await
        .expect_err("RequestLLM must fail when no provider is configured");

    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert!(status.message().contains("No LLM provider"));
}

#[tokio::test]
async fn test_core_request_llm_streaming_with_parameters() {
    use tokio::sync::Mutex;

    struct ParamCapturingProvider {
        captured: Arc<Mutex<Option<ChatRequest>>>,
    }

    #[async_trait]
    impl LlmProvider for ParamCapturingProvider {
        async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
            unimplemented!()
        }

        async fn chat_stream(
            &self,
            request: &ChatRequest,
        ) -> Result<ChatChunkStream, GatewayError> {
            *self.captured.lock().await = Some(request.clone());
            let (tx, rx) = mpsc::channel(2);
            tokio::spawn(async move {
                let _ = tx.send(Ok(ChatChunk::delta("Captured "))).await;
                let _ = tx.send(Ok(ChatChunk::done(Some("stop".to_string())))).await;
            });
            Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
        }
    }

    let captured = Arc::new(Mutex::new(None));
    let provider = Arc::new(ParamCapturingProvider {
        captured: captured.clone(),
    });
    let gateway = Arc::new(LlmGateway::new(provider, "test-model"));

    let (event_tx, _event_rx) = mpsc::channel(16);
    let service = CoreApiService::new(event_tx).with_gateway(gateway);

    let mut param_fields = std::collections::BTreeMap::new();
    param_fields.insert(
        "system_prompt".to_string(),
        kanon_proto::prost_types::Value {
            kind: Some(kanon_proto::prost_types::value::Kind::StringValue(
                "You are an assistant".to_string(),
            )),
        },
    );
    param_fields.insert(
        "temperature".to_string(),
        kanon_proto::prost_types::Value {
            kind: Some(kanon_proto::prost_types::value::Kind::NumberValue(0.7)),
        },
    );
    param_fields.insert(
        "max_tokens".to_string(),
        kanon_proto::prost_types::Value {
            kind: Some(kanon_proto::prost_types::value::Kind::NumberValue(256.0)),
        },
    );

    let request = Request::new(LlmRequest {
        prompt: "Hello with params".to_string(),
        model: "custom-model".to_string(),
        parameters: Some(kanon_proto::prost_types::Struct {
            fields: param_fields,
        }),
    });

    let response = service
        .request_llm(request)
        .await
        .expect("RequestLLM failed");

    let mut stream = response.into_inner();
    let mut accumulated = String::new();
    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.expect("Stream chunk error");
        accumulated.push_str(&chunk.delta_text);
    }

    assert_eq!(accumulated, "Captured ");

    // Verify parameter extraction in captured ChatRequest
    let req = captured
        .lock()
        .await
        .clone()
        .expect("Request was not captured");
    assert_eq!(req.model, "custom-model");
    assert_eq!(req.temperature, Some(0.7));
    assert_eq!(req.max_tokens, Some(256));
    assert_eq!(req.messages.len(), 2);
    assert_eq!(
        req.messages[0].role,
        kanon_llm::gateway::types::Role::System
    );
    assert_eq!(
        req.messages[0].content.as_deref(),
        Some("You are an assistant")
    );
    assert_eq!(req.messages[1].role, kanon_llm::gateway::types::Role::User);
    assert_eq!(
        req.messages[1].content.as_deref(),
        Some("Hello with params")
    );
}
