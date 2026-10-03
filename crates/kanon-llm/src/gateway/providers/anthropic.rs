//! Anthropic Messages protocol implementation.
//!
//! Protocol-level client conforming to the Anthropic `/v1/messages` specification.
//! Compatible with any engine or proxy implementing the Anthropic Messages wire format
//! (e.g. Anthropic Claude models, AWS Bedrock Anthropic proxies, Minimax).
//!
//! Handles translation between Kanon domain types and Anthropic's structured content blocks
//! (`text`, `tool_use`, `tool_result`), with top-level system prompt separation.
//!
//! # Prompt caching
//! Anthropic caches only up to explicit `cache_control` breakpoints, so a stable prefix alone is
//! not enough. Every request marks three: the last tool, the system block and the last content
//! block of the conversation. The request is laid out static-first (tools, then system, then
//! history), so each breakpoint covers a longer, less volatile prefix than the one before it — a
//! new turn only pays for what came after the previous turn's breakpoint. Prompts shorter than the
//! model's minimum cacheable length ignore the markers, so they are harmless there.

use async_trait::async_trait;
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

    /// Marks the end of a cacheable prefix.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct CacheControl {
        #[serde(rename = "type")]
        pub kind: String,
    }

    impl CacheControl {
        /// The only breakpoint kind the API defines.
        pub fn ephemeral() -> Self {
            Self {
                kind: "ephemeral".to_string(),
            }
        }
    }

    #[derive(Debug, Serialize)]
    pub struct AnthropicMessagesRequest<'a> {
        pub model: &'a str,
        pub max_tokens: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub system: Option<Vec<AnthropicSystemBlock>>,
        pub messages: Vec<AnthropicMessageWire>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tools: Option<Vec<AnthropicToolWire>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub temperature: Option<f32>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        pub stream: bool,
    }

    /// One block of the top-level system prompt (the array form is what carries `cache_control`).
    #[derive(Debug, Serialize)]
    pub struct AnthropicSystemBlock {
        #[serde(rename = "type")]
        pub kind: &'static str,
        pub text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub cache_control: Option<CacheControl>,
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
        Text {
            text: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            cache_control: Option<CacheControl>,
        },

        #[serde(rename = "image")]
        Image {
            source: AnthropicImageSource,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            cache_control: Option<CacheControl>,
        },

        #[serde(rename = "tool_use")]
        ToolUse {
            id: String,
            name: String,
            input: serde_json::Value,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            cache_control: Option<CacheControl>,
        },

        #[serde(rename = "tool_result")]
        ToolResult {
            tool_use_id: String,
            content: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            is_error: Option<bool>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            cache_control: Option<CacheControl>,
        },
    }

    impl AnthropicContentBlock {
        /// A plain text block.
        pub fn text(text: impl Into<String>) -> Self {
            Self::Text {
                text: text.into(),
                cache_control: None,
            }
        }

        /// Places a cache breakpoint after this block.
        pub fn mark_cacheable(&mut self) {
            let slot = match self {
                Self::Text { cache_control, .. }
                | Self::Image { cache_control, .. }
                | Self::ToolUse { cache_control, .. }
                | Self::ToolResult { cache_control, .. } => cache_control,
            };
            *slot = Some(CacheControl::ephemeral());
        }
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cache_control: Option<CacheControl>,
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
        /// Input tokens that were neither read from nor written to the cache.
        pub input_tokens: u32,
        pub output_tokens: u32,
        /// Input tokens served from the prompt cache.
        #[serde(default)]
        pub cache_read_input_tokens: u32,
        /// Input tokens written to the prompt cache by this request.
        #[serde(default)]
        pub cache_creation_input_tokens: u32,
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
            .connect_timeout(super::CONNECT_TIMEOUT)
            .timeout(super::REQUEST_TIMEOUT)
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
            blocks.push(wire::AnthropicContentBlock::text(text.clone()));
        }
        for part in msg.parts.as_deref().unwrap_or_default() {
            match part {
                ContentPart::Text { text } => {
                    blocks.push(wire::AnthropicContentBlock::text(text.clone()));
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
    Some(wire::AnthropicContentBlock::Image {
        source,
        cache_control: None,
    })
}

impl AnthropicMessagesProvider {
    /// Translates a domain request into the wire request, with cache breakpoints placed.
    ///
    /// One owner for both the blocking and the streaming call, so the two can never disagree about
    /// the prompt layout — which would make a streamed turn miss the cache its blocking twin wrote.
    fn build_wire_request<'a>(
        &'a self,
        request: &'a ChatRequest,
        stream: bool,
    ) -> wire::AnthropicMessagesRequest<'a> {
        let model = if !request.model.is_empty() {
            request.model.as_str()
        } else {
            self.default_model.as_str()
        };

        // Anthropic requires the system prompt in the top-level `system` field, rather than inside
        // the `messages` array. The (already merged) system messages become one cached block.
        let mut system_text: Option<String> = None;
        let mut messages: Vec<wire::AnthropicMessageWire> = Vec::new();

        for msg in &request.messages {
            let mut msg = msg.clone();
            msg.separate_reasoning();
            match msg.role {
                Role::System => {
                    if let Some(ref text) = msg.content {
                        match &mut system_text {
                            Some(existing) => {
                                existing.push_str("\n\n");
                                existing.push_str(text);
                            }
                            None => system_text = Some(text.clone()),
                        }
                    }
                }
                Role::User => {
                    let blocks = Self::user_blocks(&msg);
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
                        blocks.push(wire::AnthropicContentBlock::text(text.clone()));
                    }
                    if let Some(ref tool_calls) = msg.tool_calls {
                        for tc in tool_calls {
                            blocks.push(wire::AnthropicContentBlock::ToolUse {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                input: tc.arguments.clone(),
                                cache_control: None,
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
                    messages.push(wire::AnthropicMessageWire {
                        role: "user".to_string(),
                        content: vec![wire::AnthropicContentBlock::ToolResult {
                            tool_use_id: msg.tool_call_id.clone().unwrap_or_default(),
                            content: msg.content.clone().unwrap_or_default(),
                            is_error: None,
                            cache_control: None,
                        }],
                    });
                }
            }
        }

        let mut tools: Option<Vec<wire::AnthropicToolWire>> = if request.tools.is_empty() {
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
                        cache_control: None,
                    })
                    .collect(),
            )
        };

        // Breakpoint 1: the end of the tool list — the most static part of the prompt.
        if let Some(last) = tools.as_mut().and_then(|tools| tools.last_mut()) {
            last.cache_control = Some(wire::CacheControl::ephemeral());
        }

        // Breakpoint 2: the end of the system prompt (persona, skills, summary).
        let system = system_text.map(|text| {
            vec![wire::AnthropicSystemBlock {
                kind: "text",
                text,
                cache_control: Some(wire::CacheControl::ephemeral()),
            }]
        });

        // Breakpoint 3: the last block of the conversation, so the next turn — whose prompt starts
        // with this one — reads everything up to here from the cache.
        if let Some(last) = messages
            .last_mut()
            .and_then(|message| message.content.last_mut())
        {
            last.mark_cacheable();
        }

        wire::AnthropicMessagesRequest {
            model,
            max_tokens: request.max_tokens.unwrap_or(Self::DEFAULT_MAX_TOKENS),
            system,
            messages,
            tools,
            temperature: request.temperature,
            stream,
        }
    }

    /// Sends a wire request with the endpoint's credentials and headers.
    async fn send(
        &self,
        wire_req: wire::AnthropicMessagesRequest<'_>,
    ) -> Result<reqwest::Response, GatewayError> {
        let mut req_builder = self.client.post(&self.endpoint).json(&wire_req);
        // Do not retain another complete history while waiting for the provider.
        drop(wire_req);

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
        Ok(resp)
    }
}

#[async_trait]
impl LlmProvider for AnthropicMessagesProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let wire_req = self.build_wire_request(request, false);
        let resp = self.send(wire_req).await?;
        let wire_resp: wire::AnthropicMessagesResponse = resp.json().await?;

        let mut text_output = String::new();
        let mut tool_calls = Vec::new();

        for block in wire_resp.content {
            match block {
                wire::AnthropicContentBlock::Text { text, .. } => {
                    text_output.push_str(&text);
                }
                wire::AnthropicContentBlock::ToolUse {
                    id, name, input, ..
                } => {
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

        // `input_tokens` excludes everything the cache served or stored, so the prompt size the
        // rest of the node reasons about is the sum of the three.
        let usage = wire_resp.usage.map(|u| {
            let prompt_tokens =
                u.input_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens;
            TokenUsage {
                prompt_tokens,
                cached_tokens: u.cache_read_input_tokens,
                completion_tokens: u.output_tokens,
                total_tokens: prompt_tokens + u.output_tokens,
            }
        });

        let mut response = ChatResponse {
            reasoning_content: None,
            content,
            tool_calls,
            finish_reason,
            usage,
        };
        response.separate_reasoning();
        Ok(response)
    }

    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatChunkStream, GatewayError> {
        let wire_req = self.build_wire_request(request, true);
        let resp = self.send(wire_req).await?;

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let mut byte_stream = resp.bytes_stream();

        tokio::spawn(async move {
            let mut decoder = SseDecoder::new();
            let mut finish_reason: Option<String> = None;
            let mut calls = std::collections::BTreeMap::<usize, super::PendingToolCall>::new();

            while let Some(chunk_res) = byte_stream.next().await {
                let chunk = match chunk_res {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        let _ = tx.send(Err(GatewayError::Http(error))).await;
                        return;
                    }
                };
                for event in decoder.decode(&chunk) {
                    let value: serde_json::Value = match serde_json::from_str(&event.data) {
                        Ok(value) => value,
                        Err(error) => {
                            let _ = tx.send(Err(GatewayError::Json(error))).await;
                            return;
                        }
                    };
                    let event_type = event.event.as_deref().or_else(|| value["type"].as_str());
                    match event_type {
                        Some("content_block_start")
                            if value["content_block"]["type"] == "tool_use" =>
                        {
                            let Some(index) = value["index"].as_u64() else {
                                let _ = tx
                                    .send(Err(GatewayError::InvalidResponse(
                                        "tool block has no index".into(),
                                    )))
                                    .await;
                                return;
                            };
                            let block = &value["content_block"];
                            // Empty input is a placeholder; input_json_delta carries the actual JSON.
                            let input = block.get("input").filter(|input| {
                                input.as_object().is_some_and(|object| !object.is_empty())
                            });
                            calls.insert(
                                index as usize,
                                super::PendingToolCall {
                                    id: block["id"].as_str().unwrap_or_default().into(),
                                    name: block["name"].as_str().unwrap_or_default().into(),
                                    arguments: input
                                        .map(serde_json::Value::to_string)
                                        .unwrap_or_default(),
                                },
                            );
                        }
                        Some("content_block_delta") => match value["delta"]["type"].as_str() {
                            Some("text_delta") => {
                                if let Some(text) = value["delta"]["text"].as_str()
                                    && tx.send(Ok(ChatChunk::delta(text))).await.is_err()
                                {
                                    return;
                                }
                            }
                            Some("thinking_delta") => {
                                if let Some(text) = value["delta"]["thinking"].as_str()
                                    && tx.send(Ok(ChatChunk::reasoning(text))).await.is_err()
                                {
                                    return;
                                }
                            }
                            Some("input_json_delta") => {
                                let call = value["index"]
                                    .as_u64()
                                    .and_then(|index| calls.get_mut(&(index as usize)));
                                let Some(call) = call else {
                                    let _ = tx
                                        .send(Err(GatewayError::InvalidResponse(
                                            "arguments precede their tool block".into(),
                                        )))
                                        .await;
                                    return;
                                };
                                if let Some(partial) = value["delta"]["partial_json"].as_str() {
                                    call.arguments.push_str(partial);
                                }
                            }
                            _ => {}
                        },
                        Some("message_delta") => {
                            finish_reason = value["delta"]["stop_reason"].as_str().map(|reason| {
                                if reason == "tool_use" {
                                    "tool_calls"
                                } else {
                                    reason
                                }
                                .to_string()
                            });
                        }
                        Some("message_stop") => {
                            let _ = tx
                                .send(super::finish_tool_calls(calls, finish_reason))
                                .await;
                            return;
                        }
                        Some("error") => {
                            let _ = tx
                                .send(Err(GatewayError::InvalidResponse(
                                    value["error"].to_string(),
                                )))
                                .await;
                            return;
                        }
                        _ => {}
                    }
                }
            }
            let _ = tx
                .send(Err(GatewayError::InvalidResponse(
                    "Anthropic stream ended before message_stop".into(),
                )))
                .await;
        });

        Ok(crate::gateway::reasoning::separate_stream(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        )))
    }
}
