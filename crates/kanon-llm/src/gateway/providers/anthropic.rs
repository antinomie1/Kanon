//! Anthropic Messages protocol implementation.
//!
//! Protocol-level client conforming to the Anthropic `/v1/messages` specification.
//! Compatible with any engine or proxy implementing the Anthropic Messages wire format
//! (e.g. Anthropic Claude models, AWS Bedrock Anthropic proxies, Minimax).
//!
//! Handles translation between Kanon domain types and Anthropic's structured content blocks
//! (`text`, `tool_use`, `tool_result`), with top-level system prompt separation.

use async_trait::async_trait;
use std::time::Duration;
use tokio_stream::StreamExt;

use crate::error::GatewayError;
use crate::gateway::providers::sse::SseDecoder;
use crate::gateway::types::{
    ChatMessage, ChatRequest, ChatResponse, ContentPart, Role, TokenUsage, ToolCall,
};
use crate::gateway::{ChatChunk, ChatChunkStream, LlmProvider};

/// Private wire structures representing the Anthropic Messages API format.
#[allow(dead_code)]
mod wire {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize)]
    pub struct AnthropicMessagesRequest<'a> {
        pub model: &'a str,
        pub max_tokens: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub system: Option<String>,
        pub messages: Vec<AnthropicMessageWire>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tools: Option<Vec<AnthropicToolWire>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub temperature: Option<f32>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        pub stream: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct AnthropicMessageWire {
        pub role: String, // "user" | "assistant"
        pub content: Vec<AnthropicContentBlock>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type")]
    pub enum AnthropicContentBlock {
        #[serde(rename = "text")]
        Text { text: String },

        #[serde(rename = "image")]
        Image { source: AnthropicImageSource },

        #[serde(rename = "tool_use")]
        ToolUse {
            id: String,
            name: String,
            input: serde_json::Value,
        },

        #[serde(rename = "tool_result")]
        ToolResult {
            tool_use_id: String,
            content: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            is_error: Option<bool>,
        },
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum AnthropicImageSource {
        /// Inline base64 payload, which is how a local file is transmitted.
        Base64 { media_type: String, data: String },
        /// Remote URL the endpoint fetches itself.
        Url { url: String },
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct AnthropicToolWire {
        pub name: String,
        pub description: String,
        pub input_schema: serde_json::Value,
    }

    #[derive(Debug, Deserialize)]
    pub struct AnthropicMessagesResponse {
        #[serde(default)]
        pub id: Option<String>,
        #[serde(rename = "type")]
        pub message_type: Option<String>,
        pub role: String,
        pub content: Vec<AnthropicContentBlock>,
        pub stop_reason: Option<String>,
        pub usage: Option<AnthropicUsageWire>,
    }

    #[derive(Debug, Deserialize)]
    pub struct AnthropicUsageWire {
        pub input_tokens: u32,
        pub output_tokens: u32,
    }
}

/// Generic, protocol-level HTTP client implementing the Anthropic Messages API.
pub struct AnthropicMessagesProvider {
    client: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    anthropic_version: String,
    default_model: String,
    custom_headers: Vec<(String, String)>,
}

/// Backward-compatible type alias.
pub type AnthropicProvider = AnthropicMessagesProvider;

impl AnthropicMessagesProvider {
    /// Standard Anthropic API version header.
    pub const DEFAULT_VERSION: &'static str = "2023-06-01";
    /// Default maximum token count if unspecified in requests.
    pub const DEFAULT_MAX_TOKENS: u32 = 4096;

    /// Creates a new `AnthropicMessagesProvider` pointing to a base URL or endpoint.
    ///
    /// # Arguments
    /// - `base_url`: Base URL or endpoint (e.g. `https://api.anthropic.com/v1`).
    /// - `api_key`: Secret API key passed via `x-api-key`.
    /// - `default_model`: Default model identifier (e.g. `claude-3-5-sonnet-20241022`).
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        default_model: impl Into<String>,
    ) -> Self {
        let raw_url = base_url.into();
        let trimmed = raw_url.trim_end_matches('/');
        let endpoint = if trimmed.ends_with("/messages") {
            trimmed.to_string()
        } else if trimmed.ends_with("/v1") {
            format!("{trimmed}/messages")
        } else {
            format!("{trimmed}/v1/messages")
        };

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(10)
            .build()
            .unwrap_or_default();

        Self {
            client,
            endpoint,
            api_key,
            anthropic_version: Self::DEFAULT_VERSION.to_string(),
            default_model: default_model.into(),
            custom_headers: Vec::new(),
        }
    }

    /// Sets a custom `anthropic-version` header value.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.anthropic_version = version.into();
        self
    }

    /// Appends a custom HTTP header to all outbound requests.
    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.custom_headers.push((key.into(), value.into()));
        self
    }

    /// Builds the content blocks of a user turn, including any multimodal parts.
    ///
    /// The textual projection is emitted first so a model that ignores images still receives the
    /// caption and the surrounding conversation.
    fn user_blocks(msg: &ChatMessage) -> Vec<wire::AnthropicContentBlock> {
        let mut blocks = Vec::new();
        if let Some(text) = msg.content.as_ref().filter(|text| !text.is_empty()) {
            blocks.push(wire::AnthropicContentBlock::Text { text: text.clone() });
        }
        for part in msg.parts.as_deref().unwrap_or_default() {
            match part {
                ContentPart::Text { text } => {
                    blocks.push(wire::AnthropicContentBlock::Text { text: text.clone() });
                }
                ContentPart::Image { .. } => {
                    let Some(block) = anthropic_image_block(part) else {
                        tracing::warn!("Dropping an image part that resolved to no URL");
                        continue;
                    };
                    blocks.push(block);
                }
            }
        }
        blocks
    }
}

/// Renders one multimodal image part as an Anthropic image block.
///
/// Anthropic accepts a remote URL or an inline base64 payload. The shared resolver returns a
/// `data:` URI for local files, which is split back into its media type and payload here.
fn anthropic_image_block(part: &ContentPart) -> Option<wire::AnthropicContentBlock> {
    let url = part.resolved_image_url()?;
    let source = match url.strip_prefix("data:") {
        Some(rest) => {
            let (meta, data) = rest.split_once(',')?;
            let media_type = meta.strip_suffix(";base64").unwrap_or(meta).to_string();
            wire::AnthropicImageSource::Base64 {
                media_type,
                data: data.to_string(),
            }
        }
        None => wire::AnthropicImageSource::Url { url },
    };
    Some(wire::AnthropicContentBlock::Image { source })
}

#[async_trait]
impl LlmProvider for AnthropicMessagesProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let model = if !request.model.is_empty() {
            &request.model
        } else {
            &self.default_model
        };

        let max_tokens = request.max_tokens.unwrap_or(Self::DEFAULT_MAX_TOKENS);

        // Anthropic requires system prompt to be in the top-level `system` field,
        // rather than inside the `messages` array.
        let mut system_prompt: Option<String> = None;
        let mut messages: Vec<wire::AnthropicMessageWire> = Vec::new();

        for msg in &request.messages {
            match msg.role {
                Role::System => {
                    if let Some(ref text) = msg.content {
                        if let Some(existing) = &mut system_prompt {
                            existing.push_str("\n\n");
                            existing.push_str(text);
                        } else {
                            system_prompt = Some(text.clone());
                        }
                    }
                }
                Role::User => {
                    let blocks = Self::user_blocks(msg);
                    if !blocks.is_empty() {
                        messages.push(wire::AnthropicMessageWire {
                            role: "user".to_string(),
                            content: blocks,
                        });
                    }
                }
                Role::Assistant => {
                    let mut blocks = Vec::new();
                    if let Some(ref text) = msg.content
                        && !text.is_empty()
                    {
                        blocks.push(wire::AnthropicContentBlock::Text { text: text.clone() });
                    }
                    if let Some(ref tool_calls) = msg.tool_calls {
                        for tc in tool_calls {
                            blocks.push(wire::AnthropicContentBlock::ToolUse {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                input: tc.arguments.clone(),
                            });
                        }
                    }
                    if !blocks.is_empty() {
                        messages.push(wire::AnthropicMessageWire {
                            role: "assistant".to_string(),
                            content: blocks,
                        });
                    }
                }
                Role::Tool => {
                    // Anthropic specifies tool execution output is returned inside a "user" turn
                    // with type: "tool_result".
                    let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
                    let content = msg.content.clone().unwrap_or_default();
                    let block = wire::AnthropicContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error: None,
                    };
                    messages.push(wire::AnthropicMessageWire {
                        role: "user".to_string(),
                        content: vec![block],
                    });
                }
            }
        }

        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(
                request
                    .tools
                    .iter()
                    .map(|t| wire::AnthropicToolWire {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        input_schema: t.parameters.clone(),
                    })
                    .collect(),
            )
        };

        let wire_req = wire::AnthropicMessagesRequest {
            model,
            max_tokens,
            system: system_prompt,
            messages,
            tools,
            temperature: request.temperature,
            stream: false,
        };

        let mut req_builder = self.client.post(&self.endpoint).json(&wire_req);

        if let Some(ref key) = self.api_key {
            req_builder = req_builder.header("x-api-key", key);
        }
        req_builder = req_builder.header("anthropic-version", &self.anthropic_version);

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

        let wire_resp: wire::AnthropicMessagesResponse = resp.json().await?;

        let mut text_output = String::new();
        let mut tool_calls = Vec::new();

        for block in wire_resp.content {
            match block {
                wire::AnthropicContentBlock::Text { text } => {
                    text_output.push_str(&text);
                }
                wire::AnthropicContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: input,
                    });
                }
                wire::AnthropicContentBlock::ToolResult { .. } => {}
                // A response never carries an image block; ignoring one keeps the parser total
                // instead of failing on a proxy that echoes content back.
                wire::AnthropicContentBlock::Image { .. } => {}
            }
        }

        let content = if text_output.is_empty() {
            None
        } else {
            Some(text_output)
        };

        let finish_reason = match wire_resp.stop_reason.as_deref() {
            Some("tool_use") => Some("tool_calls".to_string()),
            Some(other) => Some(other.to_string()),
            None => None,
        };

        let usage = wire_resp.usage.map(|u| TokenUsage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            total_tokens: u.input_tokens + u.output_tokens,
        });

        Ok(ChatResponse {
            content,
            tool_calls,
            finish_reason,
            usage,
        })
    }

    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatChunkStream, GatewayError> {
        let model = if !request.model.is_empty() {
            &request.model
        } else {
            &self.default_model
        };

        let max_tokens = request.max_tokens.unwrap_or(Self::DEFAULT_MAX_TOKENS);

        let mut system_prompt: Option<String> = None;
        let mut messages: Vec<wire::AnthropicMessageWire> = Vec::new();

        for msg in &request.messages {
            match msg.role {
                Role::System => {
                    if let Some(ref text) = msg.content {
                        if let Some(existing) = &mut system_prompt {
                            existing.push_str("\n\n");
                            existing.push_str(text);
                        } else {
                            system_prompt = Some(text.clone());
                        }
                    }
                }
                Role::User => {
                    let blocks = Self::user_blocks(msg);
                    if !blocks.is_empty() {
                        messages.push(wire::AnthropicMessageWire {
                            role: "user".to_string(),
                            content: blocks,
                        });
                    }
                }
                Role::Assistant => {
                    let mut blocks = Vec::new();
                    if let Some(ref text) = msg.content
                        && !text.is_empty()
                    {
                        blocks.push(wire::AnthropicContentBlock::Text { text: text.clone() });
                    }
                    if let Some(ref tool_calls) = msg.tool_calls {
                        for tc in tool_calls {
                            blocks.push(wire::AnthropicContentBlock::ToolUse {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                input: tc.arguments.clone(),
                            });
                        }
                    }
                    if !blocks.is_empty() {
                        messages.push(wire::AnthropicMessageWire {
                            role: "assistant".to_string(),
                            content: blocks,
                        });
                    }
                }
                Role::Tool => {
                    let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
                    let content = msg.content.clone().unwrap_or_default();
                    let block = wire::AnthropicContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error: None,
                    };
                    messages.push(wire::AnthropicMessageWire {
                        role: "user".to_string(),
                        content: vec![block],
                    });
                }
            }
        }

        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(
                request
                    .tools
                    .iter()
                    .map(|t| wire::AnthropicToolWire {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        input_schema: t.parameters.clone(),
                    })
                    .collect(),
            )
        };

        let wire_req = wire::AnthropicMessagesRequest {
            model,
            max_tokens,
            system: system_prompt,
            messages,
            tools,
            temperature: request.temperature,
            stream: true,
        };

        let mut req_builder = self.client.post(&self.endpoint).json(&wire_req);

        if let Some(ref key) = self.api_key {
            req_builder = req_builder.header("x-api-key", key);
        }
        req_builder = req_builder.header("anthropic-version", &self.anthropic_version);

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
            let mut finish_reason: Option<String> = None;

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
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&ev.data) {
                        let event_type = ev.event.as_deref().or_else(|| val["type"].as_str());

                        if let Some("content_block_delta") = event_type {
                            if let Some("text_delta") = val["delta"]["type"].as_str()
                                && let Some(text) = val["delta"]["text"].as_str()
                                && tx.send(Ok(ChatChunk::delta(text))).await.is_err()
                            {
                                return;
                            }
                        } else if let Some("message_delta") = event_type {
                            if let Some(stop_reason) = val["delta"]["stop_reason"].as_str() {
                                finish_reason = Some(match stop_reason {
                                    "tool_use" => "tool_calls".to_string(),
                                    other => other.to_string(),
                                });
                            }
                        } else if let Some("message_stop") = event_type {
                            let _ = tx.send(Ok(ChatChunk::done(finish_reason.clone()))).await;
                            return;
                        }
                    }
                }
            }

            let _ = tx.send(Ok(ChatChunk::done(finish_reason))).await;
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}
