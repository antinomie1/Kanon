//! Unified LLM Gateway client and provider abstractions.
//!
//! Exposes the [`LlmProvider`] trait for model backends, domain types in [`types`],
//! and protocol implementations in [`providers`].

pub mod providers;
pub mod types;

use async_trait::async_trait;
use std::pin::Pin;
use std::sync::Arc;
use tokio_stream::Stream;

use crate::error::GatewayError;
pub use providers::{
    AnthropicMessagesProvider, AnthropicProvider, OpenAiChatProvider, OpenAiProvider,
    OpenAiResponsesProvider, SseDecoder, SseEvent,
};
pub use types::{
    ChatChunk, ChatMessage, ChatRequest, ChatResponse, ContentPart, Role, TokenUsage, ToolCall,
    ToolDefinition,
};

/// Pinned, boxed stream of asynchronous chat completion chunks.
pub type ChatChunkStream = Pin<Box<dyn Stream<Item = Result<ChatChunk, GatewayError>> + Send>>;

/// Asynchronous trait defining interaction with an LLM backend.
///
/// Providers translate the unified [`ChatRequest`] domain format into their
/// wire representations, transmit the payload via non-blocking HTTP,
/// and map the response back into [`ChatResponse`] or stream [`ChatChunk`]s.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Executes a chat completion query against the underlying model backend.
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError>;

    /// Executes a streaming chat completion query against the underlying model backend.
    ///
    /// Default implementation wraps a non-streaming [`chat`] call into a two-chunk stream.
    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatChunkStream, GatewayError> {
        let resp = self.chat(request).await?;
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tokio::spawn(async move {
            if let Some(text) = resp.content {
                let _ = tx
                    .send(Ok(ChatChunk {
                        delta_text: text,
                        reasoning_text: None,
                        is_finished: false,
                        finish_reason: None,
                        tool_calls: resp.tool_calls.clone(),
                    }))
                    .await;
            }
            let _ = tx
                .send(Ok(ChatChunk {
                    delta_text: String::new(),
                    reasoning_text: None,
                    is_finished: true,
                    finish_reason: resp.finish_reason,
                    tool_calls: resp.tool_calls,
                }))
                .await;
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

/// High-level LLM gateway managing default parameters and dispatching to a provider backend.
pub struct LlmGateway {
    /// The active backend provider implementation.
    provider: Arc<dyn LlmProvider>,
    /// Default model tag used when not explicitly specified in a request.
    default_model: String,
}

impl std::fmt::Debug for LlmGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmGateway")
            .field("default_model", &self.default_model)
            .finish()
    }
}

impl LlmGateway {
    /// Creates a new `LlmGateway` instance wrapping the given provider.
    pub fn new(provider: Arc<dyn LlmProvider>, default_model: impl Into<String>) -> Self {
        Self {
            provider,
            default_model: default_model.into(),
        }
    }

    /// Returns a reference to the active provider implementation.
    pub fn provider(&self) -> &Arc<dyn LlmProvider> {
        &self.provider
    }

    /// Default model tag used when a request does not name one.
    pub fn default_model(&self) -> &str {
        &self.default_model
    }

    /// Dispatches a chat completion request to the active provider.
    pub async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let mut req = request.clone();
        if req.model.is_empty() {
            req.model = self.default_model.clone();
        }
        self.provider.chat(&req).await
    }

    /// Dispatches a streaming chat completion request to the active provider.
    pub async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> Result<ChatChunkStream, GatewayError> {
        let mut req = request.clone();
        if req.model.is_empty() {
            req.model = self.default_model.clone();
        }
        self.provider.chat_stream(&req).await
    }
}

/// Configured model provider and the model identifier it should default to.
pub type ProviderSetup = (Arc<dyn LlmProvider>, String);

/// Protocol identifiers accepted by [`build_provider`].
pub const SUPPORTED_PROTOCOLS: [&str; 3] = ["openai", "openai_responses", "anthropic"];

/// Instantiates the wire client for one provider configuration.
///
/// This is the single owner of the protocol switch: the environment bootstrap
/// ([`provider_from_env`]) and the management gateway's provider endpoints both build their
/// clients here, so a protocol accepted in one path can never be rejected by the other.
///
/// `protocol` accepts `openai` (alias `openai_chat`), `openai_responses` and `anthropic`;
/// any other value is rejected explicitly instead of silently falling back to a default.
pub fn build_provider(
    protocol: &str,
    base_url: impl Into<String>,
    api_key: Option<String>,
    model: impl Into<String>,
) -> Result<Arc<dyn LlmProvider>, String> {
    let base_url = base_url.into();
    let base_url = base_url.trim().to_string();
    if base_url.is_empty() {
        return Err("Provider base URL must not be empty".to_string());
    }

    let api_key = api_key.filter(|k| !k.trim().is_empty());
    let model = model.into();

    let provider: Arc<dyn LlmProvider> = match protocol {
        "openai" | "openai_chat" => Arc::new(OpenAiChatProvider::new(base_url, api_key, model)),
        // The Responses API carries the credential in its own constructor, so the key is
        // required here rather than optional.
        "openai_responses" => Arc::new(
            OpenAiResponsesProvider::new(api_key.unwrap_or_default()).with_base_url(base_url),
        ),
        "anthropic" => Arc::new(AnthropicMessagesProvider::new(base_url, api_key, model)),
        other => {
            return Err(format!(
                "Unsupported protocol '{other}'; expected one of {}",
                SUPPORTED_PROTOCOLS.join(", ")
            ));
        }
    };

    Ok(provider)
}

/// Configures an [`LlmProvider`] and default model name from standard environment variables:
///
/// - `KANON_LLM_BASE_URL` — model provider base URL. If unset or empty, returns `Ok(None)`.
/// - `KANON_LLM_MODEL` — default model name (default: `gpt-4o-mini`).
/// - `KANON_LLM_API_KEY` — provider credential (optional).
/// - `KANON_LLM_PROTOCOL` — protocol wire format: `openai` (or `openai_chat`), `openai_responses`, or `anthropic`.
pub fn provider_from_env() -> Result<Option<ProviderSetup>, String> {
    let Ok(base_url) = std::env::var("KANON_LLM_BASE_URL") else {
        return Ok(None);
    };
    let base_url = base_url.trim().to_string();
    if base_url.is_empty() {
        return Ok(None);
    }

    let model = std::env::var("KANON_LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let api_key = std::env::var("KANON_LLM_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let protocol = std::env::var("KANON_LLM_PROTOCOL").unwrap_or_else(|_| "openai".to_string());

    let provider = build_provider(&protocol, base_url, api_key, model.clone())
        .map_err(|err| format!("{err} (from KANON_LLM_PROTOCOL)"))?;

    Ok(Some((provider, model)))
}

/// Returns the part of a model response that may be shown to an end user.
///
/// OpenAI-compatible backends that expose a reasoning channel (DeepSeek `reasoning_content`,
/// and compatible gateways) have that channel folded into the response text by
/// [`providers::OpenAiChatProvider`] as a leading `<think>…</think>` block, so the management
/// console can render reasoning separately from the answer. That encoding is a *display*
/// convention: anything sent to a chat platform must be stripped first, otherwise users read
/// the model's internal chain of thought. This function is the single decoder for that
/// convention; text without a block is returned unchanged and borrows the input.
pub fn strip_reasoning_tags(text: &str) -> &str {
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix("<think>") else {
        return text;
    };
    // A truncated block (stream cut off before `</think>`) carries no user-visible answer at
    // all, so it collapses to an empty reply rather than leaking the partial reasoning.
    match rest.split_once("</think>") {
        Some((_, answer)) => answer.trim_start(),
        None => "",
    }
}
