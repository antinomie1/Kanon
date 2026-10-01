//! OpenAI Chat Completions protocol implementation.
//!
//! Protocol-level client conforming to the industry-standard OpenAI `/chat/completions`
//! specification. Compatible with any provider or local engine adhering to this wire format
//! (e.g. OpenAI, DeepSeek, Ollama, vLLM, Groq, Mistral, Moonshot, Qwen).
//!
//! Requests use standard fields unless reasoning replay is explicitly enabled. The official
//! DeepSeek HTTPS endpoint enables that extension by default for existing configurations.
//! Reading reasoning from responses is independent of whether an endpoint accepts it as input.

use async_trait::async_trait;
use tokio_stream::StreamExt;

use crate::error::GatewayError;
use crate::gateway::providers::sse::SseDecoder;
use crate::gateway::types::{
    ChatMessage, ChatRequest, ChatResponse, ContentPart, Role, TokenUsage, ToolCall,
};
use crate::gateway::{ChatChunk, ChatChunkStream, LlmProvider};

/// Private wire structures representing the standard OpenAI Chat Completions JSON schema.
#[allow(dead_code)]
mod wire {
    use serde::{Deserialize, Serialize};

    /// Distinguish an absent channel from a present but empty/null channel.
    /// Presence tells the compatibility parser that content is already answer text.
    fn reasoning_channel<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<String>, D::Error> {
        Ok(Some(
            Option::<String>::deserialize(deserializer)?.unwrap_or_default(),
        ))
    }

    #[derive(Debug, Serialize)]
    pub struct OpenAiChatRequest<'a> {
        pub model: &'a str,
        pub messages: Vec<OpenAiMessageWire>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tools: Option<Vec<OpenAiToolWire>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub temperature: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub max_tokens: Option<u32>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        pub stream: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct OpenAiMessageWire {
        pub role: String,
        /// Plain text for a classic message, or an array of content parts for a multimodal one.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub content: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub reasoning_content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_calls: Option<Vec<OpenAiToolCallWire>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_call_id: Option<String>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct OpenAiToolWire {
        pub r#type: String, // "function"
        pub function: OpenAiFunctionWire,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct OpenAiFunctionWire {
        pub name: String,
        pub description: String,
        pub parameters: serde_json::Value,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct OpenAiToolCallWire {
        pub id: String,
        pub r#type: String, // "function"
        pub function: OpenAiFunctionCallWire,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct OpenAiFunctionCallWire {
        pub name: String,
        /// OpenAI protocol encodes tool arguments as an escaped JSON string.
        pub arguments: String,
    }

    #[derive(Debug, Deserialize)]
    pub struct OpenAiChatResponse {
        #[serde(default)]
        pub id: Option<String>,
        pub choices: Vec<OpenAiChoiceWire>,
        pub usage: Option<OpenAiUsageWire>,
    }

    #[derive(Debug, Deserialize)]
    pub struct OpenAiChoiceWire {
        pub index: usize,
        pub message: OpenAiResponseMessageWire,
        pub finish_reason: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub struct OpenAiResponseMessageWire {
        pub role: String,
        pub content: Option<String>,
        #[serde(default, deserialize_with = "reasoning_channel")]
        pub reasoning_content: Option<String>,
        #[serde(default)]
        pub tool_calls: Option<Vec<OpenAiToolCallWire>>,
    }

    #[derive(Debug, Deserialize)]
    pub struct OpenAiUsageWire {
        pub prompt_tokens: u32,
        pub completion_tokens: u32,
        pub total_tokens: u32,
        /// OpenAI reports cache hits under `prompt_tokens_details.cached_tokens`.
        #[serde(default)]
        pub prompt_tokens_details: Option<OpenAiPromptTokensDetailsWire>,
        /// DeepSeek reports cache hits as a flat `prompt_cache_hit_tokens`.
        #[serde(default)]
        pub prompt_cache_hit_tokens: Option<u32>,
    }

    #[derive(Debug, Deserialize)]
    pub struct OpenAiPromptTokensDetailsWire {
        #[serde(default)]
        pub cached_tokens: Option<u32>,
    }

    #[derive(Debug, Deserialize)]
    pub struct OpenAiStreamResponse {
        pub choices: Vec<OpenAiStreamChoice>,
    }

    #[derive(Debug, Deserialize)]
    pub struct OpenAiStreamChoice {
        pub index: usize,
        pub delta: OpenAiStreamDelta,
        pub finish_reason: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub struct OpenAiStreamDelta {
        #[serde(default)]
        pub role: Option<String>,
        #[serde(default)]
        pub content: Option<String>,
        #[serde(default, deserialize_with = "reasoning_channel")]
        pub reasoning_content: Option<String>,
        #[serde(default)]
        pub tool_calls: Option<Vec<OpenAiToolCallChunkWire>>,
    }

    #[derive(Debug, Deserialize)]
    pub struct OpenAiToolCallChunkWire {
        #[serde(default)]
        pub index: Option<usize>,
        #[serde(default)]
        pub id: Option<String>,
        #[serde(default)]
        pub function: Option<OpenAiFunctionChunkWire>,
    }

    #[derive(Debug, Deserialize)]
    pub struct OpenAiFunctionChunkWire {
        #[serde(default)]
        pub name: Option<String>,
        #[serde(default)]
        pub arguments: Option<String>,
    }
}

/// Generic, protocol-level HTTP client implementing the OpenAI Chat Completions API.
pub struct OpenAiChatProvider {
    client: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    default_model: String,
    custom_headers: Vec<(String, String)>,
    /// Whether assistant history may include the non-standard `reasoning_content` field.
    replay_reasoning_content: bool,
    /// Operator preference, independent of endpoint support.
    replay_reasoning: bool,
}

/// Backward-compatible type alias.
pub type OpenAiProvider = OpenAiChatProvider;

impl OpenAiChatProvider {
    /// Creates a new `OpenAiChatProvider` from a protocol base URL or full endpoint.
    ///
    /// # Arguments
    /// - `base_url`: Base URL or endpoint (e.g. `https://api.openai.com/v1` or `http://localhost:11434/v1`).
    ///   If the URL does not end with `/chat/completions`, it is automatically appended.
    /// - `api_key`: Optional Bearer authentication token.
    /// - `default_model`: Default model identifier.
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        default_model: impl Into<String>,
    ) -> Self {
        let raw_url = base_url.into();
        let trimmed = raw_url.trim_end_matches('/');
        let endpoint = if trimmed.ends_with("/chat/completions") {
            trimmed.to_string()
        } else {
            format!("{trimmed}/chat/completions")
        };

        // Only a documented endpoint is opted in automatically. A model name, URL substring or
        // reasoning returned by a different provider does not establish request compatibility.
        let replay_reasoning_content = reqwest::Url::parse(&endpoint).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str() == Some("api.deepseek.com")
                && url.port_or_known_default() == Some(443)
        });

        let client = reqwest::Client::builder()
            .connect_timeout(super::CONNECT_TIMEOUT)
            .timeout(super::REQUEST_TIMEOUT)
            .pool_max_idle_per_host(10)
            .build()
            .unwrap_or_default();

        Self {
            client,
            endpoint,
            api_key,
            default_model: default_model.into(),
            custom_headers: Vec::new(),
            replay_reasoning_content,
            replay_reasoning: true,
        }
    }

    /// Enables or disables the `reasoning_content` request extension for this endpoint.
    ///
    /// Enable it only for endpoints whose contract accepts this field (including compatible
    /// proxies). This affects history replay, never model thinking, response parsing or storage.
    pub fn with_reasoning_content(mut self, enabled: bool) -> Self {
        self.replay_reasoning_content = enabled;
        self
    }

    /// Controls history replay without changing endpoint capability or stored messages.
    pub fn with_reasoning_replay(mut self, enabled: bool) -> Self {
        self.replay_reasoning = enabled;
        self
    }

    /// Whether this endpoint is configured to replay the separate reasoning field.
    pub fn replays_reasoning_content(&self) -> bool {
        self.replay_reasoning_content && self.replay_reasoning
    }

    /// Appends a custom HTTP header to all outbound requests (useful for proxies or custom auth).
    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.custom_headers.push((key.into(), value.into()));
        self
    }

    /// Converts request history into wire messages.
    ///
    /// Replay all retained assistant reasoning when both the endpoint and operator allow it.
    /// The mapping only reads history; disabling replay never erases durable reasoning.
    fn map_messages_to_wire(&self, messages: &[ChatMessage]) -> Vec<wire::OpenAiMessageWire> {
        let replay = self.replays_reasoning_content();
        messages
            .iter()
            .filter_map(|msg| self.map_message_to_wire(msg, replay))
            .collect()
    }

    /// Converts an internal domain `ChatMessage` into the wire format `OpenAiMessageWire`.
    ///
    /// `replay_reasoning` decides whether this assistant message carries `reasoning_content`.
    fn map_message_to_wire(
        &self,
        msg: &ChatMessage,
        replay_reasoning: bool,
    ) -> Option<wire::OpenAiMessageWire> {
        let mut msg = msg.clone();
        msg.separate_reasoning();
        // A reasoning-only turn has no valid assistant payload in the standard wire schema.
        // Omit it from this request only; shared durable history and tool-call turns stay intact.
        if !replay_reasoning
            && msg.role == Role::Assistant
            && msg.content.as_deref().is_none_or(str::is_empty)
            && !msg.has_parts()
            && msg.tool_calls.as_ref().is_none_or(Vec::is_empty)
        {
            return None;
        }
        let role = match msg.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
        .to_string();

        let tool_calls = msg.tool_calls.as_ref().map(|calls| {
            calls
                .iter()
                .map(|c| wire::OpenAiToolCallWire {
                    id: c.id.clone(),
                    r#type: "function".to_string(),
                    function: wire::OpenAiFunctionCallWire {
                        name: c.name.clone(),
                        arguments: c.arguments.to_string(),
                    },
                })
                .collect()
        });

        Some(wire::OpenAiMessageWire {
            role,
            content: message_content(&msg),
            reasoning_content: if replay_reasoning && msg.role == Role::Assistant {
                msg.reasoning_content
            } else {
                None
            },
            tool_calls,
            tool_call_id: msg.tool_call_id.clone(),
        })
    }
}

/// Renders a message's content in the OpenAI wire shape.
///
/// A message with multimodal parts becomes a content array (`text` + `image_url` blocks); every
/// other message stays a plain string, which is what endpoints that predate vision expect. A part
/// whose image cannot be materialized is dropped with a warning rather than sent as a broken URL.
fn message_content(msg: &ChatMessage) -> Option<serde_json::Value> {
    let has_parts = msg.has_parts();
    if !has_parts {
        return msg.content.clone().map(serde_json::Value::String);
    }

    let mut blocks: Vec<serde_json::Value> = Vec::new();
    if let Some(text) = msg.content.as_ref().filter(|text| !text.is_empty()) {
        blocks.push(serde_json::json!({ "type": "text", "text": text }));
    }
    for part in msg.parts.as_deref().unwrap_or_default() {
        match part {
            ContentPart::Text { text: part_text } => {
                blocks.push(serde_json::json!({ "type": "text", "text": part_text }));
            }
            ContentPart::Image { .. } => {
                let Some(url) = part.resolved_image_url() else {
                    tracing::warn!("Dropping an image part that resolved to no URL");
                    continue;
                };
                blocks.push(serde_json::json!({
                    "type": "image_url",
                    "image_url": { "url": url },
                }));
            }
        }
    }

    Some(serde_json::Value::Array(blocks))
}

#[async_trait]
impl LlmProvider for OpenAiChatProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let model = if !request.model.is_empty() {
            &request.model
        } else {
            &self.default_model
        };

        let messages = self.map_messages_to_wire(&request.messages);

        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(
                request
                    .tools
                    .iter()
                    .map(|t| wire::OpenAiToolWire {
                        r#type: "function".to_string(),
                        function: wire::OpenAiFunctionWire {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: t.parameters.clone(),
                        },
                    })
                    .collect(),
            )
        };

        let wire_req = wire::OpenAiChatRequest {
            model,
            messages,
            tools,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            stream: false,
        };

        let mut req_builder = self.client.post(&self.endpoint).json(&wire_req);

        if let Some(ref key) = self.api_key {
            req_builder = req_builder.header("Authorization", format!("Bearer {key}"));
        }

        for (k, v) in &self.custom_headers {
            req_builder = req_builder.header(k, v);
        }

        let resp = req_builder.send().await?;

        let status = resp.status();
        if !status.is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ApiStatus {
                status: status.as_u16(),
                message: err_body,
            });
        }

        let wire_resp: wire::OpenAiChatResponse = resp.json().await?;

        let choice = wire_resp.choices.into_iter().next().ok_or_else(|| {
            GatewayError::InvalidResponse("No choices returned in response".to_string())
        })?;

        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| {
                let parsed_args = serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                    .unwrap_or_else(|_| serde_json::Value::Object(Default::default()));

                ToolCall {
                    id: tc.id,
                    name: tc.function.name,
                    arguments: parsed_args,
                }
            })
            .collect();

        let usage = wire_resp.usage.map(|u| TokenUsage {
            prompt_tokens: u.prompt_tokens,
            cached_tokens: u
                .prompt_tokens_details
                .and_then(|details| details.cached_tokens)
                .or(u.prompt_cache_hit_tokens)
                .unwrap_or(0),
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        });

        let mut response = ChatResponse {
            content: choice.message.content,
            reasoning_content: choice.message.reasoning_content,
            tool_calls,
            finish_reason: choice.finish_reason,
            usage,
        };
        response.separate_reasoning();
        Ok(response)
    }

    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatChunkStream, GatewayError> {
        let model = if !request.model.is_empty() {
            &request.model
        } else {
            &self.default_model
        };

        let messages = self.map_messages_to_wire(&request.messages);

        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(
                request
                    .tools
                    .iter()
                    .map(|t| wire::OpenAiToolWire {
                        r#type: "function".to_string(),
                        function: wire::OpenAiFunctionWire {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: t.parameters.clone(),
                        },
                    })
                    .collect(),
            )
        };

        let wire_req = wire::OpenAiChatRequest {
            model,
            messages,
            tools,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            stream: true,
        };

        let mut req_builder = self.client.post(&self.endpoint).json(&wire_req);

        if let Some(ref key) = self.api_key {
            req_builder = req_builder.header("Authorization", format!("Bearer {key}"));
        }

        for (k, v) in &self.custom_headers {
            req_builder = req_builder.header(k, v);
        }

        let resp = req_builder.send().await?;

        let status = resp.status();
        if !status.is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ApiStatus {
                status: status.as_u16(),
                message: err_body,
            });
        }

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let mut byte_stream = resp.bytes_stream();

        tokio::spawn(async move {
            let mut decoder = SseDecoder::new();
            let mut has_finished = false;

            while let Some(chunk_res) = byte_stream.next().await {
                let chunk = match chunk_res {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(GatewayError::Http(e))).await;
                        return;
                    }
                };

                let events = decoder.decode(&chunk);
                for ev in events {
                    if ev.data.trim() == "[DONE]" {
                        if !has_finished {
                            let _ = tx.send(Ok(ChatChunk::done(Some("stop".to_string())))).await;
                        }
                        return;
                    }

                    if let Ok(stream_resp) =
                        serde_json::from_str::<wire::OpenAiStreamResponse>(&ev.data)
                    {
                        for choice in stream_resp.choices {
                            let finish_reason = choice.finish_reason;
                            let is_done = finish_reason.is_some();
                            if is_done {
                                has_finished = true;
                            }

                            let delta_text = choice.delta.content.unwrap_or_default();
                            let reasoning_text = choice.delta.reasoning_content;

                            let tool_calls: Vec<ToolCall> = choice
                                .delta
                                .tool_calls
                                .unwrap_or_default()
                                .into_iter()
                                .map(|tc| ToolCall {
                                    id: tc.id.unwrap_or_default(),
                                    name: tc
                                        .function
                                        .as_ref()
                                        .and_then(|f| f.name.clone())
                                        .unwrap_or_default(),
                                    arguments: tc
                                        .function
                                        .as_ref()
                                        .and_then(|f| f.arguments.as_ref())
                                        .and_then(|args| serde_json::from_str(args).ok())
                                        .unwrap_or(serde_json::json!({})),
                                })
                                .collect();

                            let has_content = !delta_text.is_empty()
                                || reasoning_text.is_some()
                                || !tool_calls.is_empty();

                            if has_content {
                                let out_chunk = ChatChunk {
                                    delta_text,
                                    reasoning_text,
                                    is_finished: false,
                                    finish_reason: None,
                                    tool_calls,
                                };

                                if tx.send(Ok(out_chunk)).await.is_err() {
                                    return;
                                }
                            }

                            if is_done {
                                let done_chunk = ChatChunk::done(finish_reason);
                                if tx.send(Ok(done_chunk)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }

            if !has_finished {
                let _ = tx.send(Ok(ChatChunk::done(None))).await;
            }
        });

        Ok(crate::gateway::reasoning::separate_stream(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        )))
    }
}
