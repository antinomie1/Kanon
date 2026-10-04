//! OpenAI Responses protocol implementation.
//!
//! Protocol-level client conforming to the modern OpenAI `/v1/responses` specification.
//! Designed for stateful agentic workflows, complex reasoning chains, and typed output streams.
//!
//! Compatible with OpenAI `/v1/responses` and compatible gateways (e.g. SambaNova, OpenResponses).

use async_trait::async_trait;
use std::time::Duration;
use tokio_stream::StreamExt;

use crate::error::GatewayError;
use crate::gateway::providers::sse::SseDecoder;
use crate::gateway::types::{
    ChatMessage, ChatRequest, ChatResponse, ContentPart, Role, TokenUsage, ToolCall,
};
use crate::gateway::{ChatChunk, ChatChunkStream, LlmProvider};

/// Private wire structures representing the OpenAI Responses API format.
#[allow(dead_code)]
mod wire {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize)]
    pub struct ResponsesRequest<'a> {
        pub model: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub instructions: Option<String>,
        pub input: Vec<ResponsesInputItem>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tools: Option<Vec<ResponsesToolWire>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub temperature: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub max_output_tokens: Option<u32>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        pub stream: bool,
    }

    #[derive(Debug, Serialize)]
    #[serde(tag = "type")]
    pub enum ResponsesInputItem {
        #[serde(rename = "message")]
        Message {
            role: String,
            content: Vec<ResponsesContentPart>,
        },
        #[serde(rename = "function_call")]
        FunctionCall {
            call_id: String,
            name: String,
            arguments: String,
        },
        #[serde(rename = "function_call_output")]
        FunctionCallOutput { call_id: String, output: String },
    }

    #[derive(Debug, Serialize)]
    #[serde(tag = "type")]
    pub enum ResponsesContentPart {
        #[serde(rename = "input_text")]
        InputText { text: String },
        #[serde(rename = "output_text")]
        OutputText { text: String },
        #[serde(rename = "input_image")]
        InputImage { image_url: String },
    }

    #[derive(Debug, Serialize)]
    pub struct ResponsesToolWire {
        #[serde(rename = "type")]
        pub tool_type: &'static str,
        pub name: String,
        pub description: String,
        pub parameters: serde_json::Value,
    }

    #[derive(Debug, Deserialize)]
    pub struct ResponsesResponseWire {
        #[serde(default)]
        pub id: Option<String>,
        #[serde(default)]
        pub status: Option<String>,
        #[serde(default)]
        pub output: Vec<ResponsesOutputWire>,
        #[serde(default)]
        pub usage: Option<ResponsesUsageWire>,
        #[serde(default)]
        pub error: Option<ResponsesErrorWire>,
    }

    #[derive(Debug, Deserialize)]
    pub struct ResponsesErrorWire {
        pub message: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(tag = "type")]
    pub enum ResponsesOutputWire {
        #[serde(rename = "message")]
        Message {
            #[serde(default)]
            role: Option<String>,
            #[serde(default)]
            content: Vec<ResponsesOutputContentPart>,
        },
        #[serde(rename = "function_call")]
        FunctionCall {
            #[serde(default)]
            call_id: Option<String>,
            name: String,
            arguments: serde_json::Value,
        },
        #[serde(other)]
        Other,
    }

    #[derive(Debug, Deserialize)]
    #[serde(tag = "type")]
    pub enum ResponsesOutputContentPart {
        #[serde(rename = "output_text")]
        OutputText { text: String },
        /// A user-visible refusal is final answer content, not private reasoning.
        #[serde(rename = "refusal")]
        Refusal { refusal: String },
        #[serde(other)]
        Other,
    }

    #[derive(Debug, Deserialize)]
    pub struct ResponsesUsageWire {
        pub input_tokens: Option<u32>,
        pub output_tokens: Option<u32>,
        pub total_tokens: Option<u32>,
        #[serde(default)]
        pub input_tokens_details: Option<ResponsesInputTokensDetailsWire>,
    }

    #[derive(Debug, Deserialize)]
    pub struct ResponsesInputTokensDetailsWire {
        #[serde(default)]
        pub cached_tokens: Option<u32>,
    }
}

/// Requires the provider's result-correlation identifier, not the distinct output item `id`.
/// Inventing an identifier would execute a tool whose result cannot be matched to its call.
fn function_call_id(call_id: Option<&str>) -> Result<String, GatewayError> {
    call_id
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| GatewayError::InvalidResponse("function call has no call_id".into()))
}

/// Builds the content parts of a Responses API user message, including any images.
///
/// The textual projection is emitted first so a model that ignores images still receives the
/// caption and the surrounding conversation.
fn user_content(msg: &ChatMessage) -> Vec<wire::ResponsesContentPart> {
    let mut content = Vec::new();
    if let Some(text) = msg.content.as_ref().filter(|text| !text.is_empty()) {
        content.push(wire::ResponsesContentPart::InputText { text: text.clone() });
    }
    for part in msg.parts.as_deref().unwrap_or_default() {
        match part {
            ContentPart::Text { text } => {
                content.push(wire::ResponsesContentPart::InputText { text: text.clone() });
            }
            ContentPart::Image { .. } => {
                let Some(image_url) = part.resolved_image_url() else {
                    tracing::warn!("Dropping an image part that resolved to no URL");
                    continue;
                };
                content.push(wire::ResponsesContentPart::InputImage { image_url });
            }
        }
    }
    content
}

/// Client for the OpenAI Responses API (`/v1/responses`).
///
/// Converts between Kanon internal agent types and the typed items protocol used
/// by modern OpenAI Responses endpoints.
pub struct OpenAiResponsesProvider {
    api_key: String,
    endpoint: String,
    client: reqwest::Client,
    custom_headers: Vec<(String, String)>,
}

impl OpenAiResponsesProvider {
    /// Constructs a new [`OpenAiResponsesProvider`] with default endpoint `https://api.openai.com/v1/responses`.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            endpoint: "https://api.openai.com/v1/responses".to_string(),
            client: reqwest::Client::builder()
                .connect_timeout(super::CONNECT_TIMEOUT)
                .timeout(super::REQUEST_TIMEOUT)
                .build()
                .unwrap_or_default(),
            custom_headers: Vec::new(),
        }
    }

    /// Sets the base URL, automatically normalizing and appending the `/responses` endpoint path.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        let base = base_url.into().trim_end_matches('/').to_string();
        if base.ends_with("/responses") {
            self.endpoint = base;
        } else if base.ends_with("/v1") {
            self.endpoint = format!("{base}/responses");
        } else {
            self.endpoint = format!("{base}/v1/responses");
        }
        self
    }

    /// Overrides the exact target HTTP endpoint URL.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Appends a custom HTTP header to all outbound requests (e.g. for proxy routing or tracing).
    pub fn with_header(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.custom_headers.push((key.into(), val.into()));
        self
    }

    /// Configures the HTTP client connection/request timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        self
    }

    /// Returns the currently configured endpoint URL.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Returns custom headers configured on this client.
    pub fn custom_headers(&self) -> &[(String, String)] {
        &self.custom_headers
    }

    /// Uses one wire mapping for complete and streamed requests so their cached prefixes agree.
    async fn send_request(
        &self,
        req: &ChatRequest,
        stream: bool,
    ) -> Result<reqwest::Response, GatewayError> {
        let mut instructions = Vec::new();
        let mut input = Vec::new();

        // 1. Map messages into Responses API input items and extract system instructions
        for msg in &req.messages {
            let mut msg = msg.clone();
            msg.separate_reasoning();
            match msg.role {
                Role::System => {
                    if let Some(ref text) = msg.content {
                        instructions.push(text.clone());
                    }
                }
                Role::User => {
                    input.push(wire::ResponsesInputItem::Message {
                        role: "user".to_string(),
                        content: user_content(&msg),
                    });
                }
                Role::Assistant => {
                    if let Some(ref text) = msg.content
                        && !text.is_empty()
                    {
                        input.push(wire::ResponsesInputItem::Message {
                            role: "assistant".to_string(),
                            content: vec![wire::ResponsesContentPart::OutputText {
                                text: text.clone(),
                            }],
                        });
                    }
                    if let Some(ref calls) = msg.tool_calls {
                        for call in calls {
                            input.push(wire::ResponsesInputItem::FunctionCall {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                arguments: call.arguments.to_string(),
                            });
                        }
                    }
                }
                Role::Tool => {
                    input.push(wire::ResponsesInputItem::FunctionCallOutput {
                        call_id: msg.tool_call_id.clone().unwrap_or_default(),
                        output: msg.content.clone().unwrap_or_default(),
                    });
                }
            }
        }

        let instructions = if instructions.is_empty() {
            None
        } else {
            Some(instructions.join("\n\n"))
        };

        // 2. Map tool definitions into Responses API flat tool schemas
        let tools = if req.tools.is_empty() {
            None
        } else {
            Some(
                req.tools
                    .iter()
                    .map(|t| wire::ResponsesToolWire {
                        tool_type: "function",
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    })
                    .collect(),
            )
        };

        let body = wire::ResponsesRequest {
            model: &req.model,
            instructions,
            input,
            tools,
            temperature: req.temperature,
            max_output_tokens: req.max_tokens,
            stream,
        };

        // 3. Dispatch HTTP request with bearer authorization & custom headers
        let mut req_builder = self
            .client
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .json(&body);
        // Serialization owns the HTTP payload; the intermediate history can be released now.
        drop(body);

        if !self.api_key.is_empty() {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", self.api_key));
        }

        for (k, v) in &self.custom_headers {
            req_builder = req_builder.header(k, v);
        }

        let resp = req_builder.send().await.map_err(GatewayError::Http)?;
        let status = resp.status();

        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ApiStatus {
                status: status.as_u16(),
                message: error_text,
            });
        }

        Ok(resp)
    }
}

#[async_trait]
impl LlmProvider for OpenAiResponsesProvider {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let resp = self.send_request(req, false).await?;

        let raw_bytes = resp.bytes().await.map_err(GatewayError::Http)?;
        let wire_resp: wire::ResponsesResponseWire = serde_json::from_slice(&raw_bytes)
            .map_err(|e| GatewayError::InvalidResponse(e.to_string()))?;

        if let Some(err) = wire_resp.error {
            return Err(GatewayError::ApiStatus {
                status: 200,
                message: err.message,
            });
        }

        // 4. Translate output items (messages & function calls)
        let mut final_content = String::new();
        let mut tool_calls = Vec::new();
        let mut refused = false;

        for item in wire_resp.output {
            match item {
                wire::ResponsesOutputWire::Message { content, .. } => {
                    for part in content {
                        match part {
                            wire::ResponsesOutputContentPart::OutputText { text } => {
                                final_content.push_str(&text);
                            }
                            wire::ResponsesOutputContentPart::Refusal { refusal } => {
                                final_content.push_str(&refusal);
                                refused = true;
                            }
                            wire::ResponsesOutputContentPart::Other => {}
                        }
                    }
                }
                wire::ResponsesOutputWire::FunctionCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    let call_id = function_call_id(call_id.as_deref())?;

                    let parsed_args = match arguments {
                        serde_json::Value::String(s) => serde_json::from_str(&s)?,
                        val @ serde_json::Value::Object(_) => val,
                        _ => {
                            return Err(GatewayError::InvalidResponse(
                                "function call arguments must be a JSON string or object".into(),
                            ));
                        }
                    };

                    tool_calls.push(ToolCall {
                        id: call_id,
                        name,
                        arguments: parsed_args,
                    });
                }
                wire::ResponsesOutputWire::Other => {}
            }
        }

        // A completed transport response can still refuse the task. Keep the visible answer,
        // but never let compaction mistake that refusal for a summary and discard history.
        let finish_reason = if refused {
            Some("refusal".to_string())
        } else if !tool_calls.is_empty() {
            Some("tool_calls".to_string())
        } else {
            wire_resp.status
        };

        let usage = wire_resp.usage.map(|u| TokenUsage {
            prompt_tokens: u.input_tokens.unwrap_or(0),
            cached_tokens: u
                .input_tokens_details
                .and_then(|details| details.cached_tokens)
                .unwrap_or(0),
            completion_tokens: u.output_tokens.unwrap_or(0),
            total_tokens: u
                .total_tokens
                .unwrap_or_else(|| u.input_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0)),
        });

        let mut response = ChatResponse {
            reasoning_content: None,
            content: if final_content.is_empty() {
                None
            } else {
                Some(final_content)
            },
            tool_calls,
            usage,
            finish_reason,
        };
        response.separate_reasoning();
        Ok(response)
    }

    async fn chat_stream(&self, req: &ChatRequest) -> Result<ChatChunkStream, GatewayError> {
        let resp = self.send_request(req, true).await?;

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let mut byte_stream = resp.bytes_stream();

        tokio::spawn(async move {
            let mut decoder = SseDecoder::new();
            let mut calls = std::collections::BTreeMap::<usize, super::PendingToolCall>::new();
            let mut finish_reason = "completed";

            while let Some(chunk_res) = byte_stream.next().await {
                let chunk = match chunk_res {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        let _ = tx.send(Err(GatewayError::Http(error))).await;
                        return;
                    }
                };
                for event in decoder.decode(&chunk) {
                    if event.data.trim() == "[DONE]" {
                        let _ = tx
                            .send(super::finish_tool_calls(calls, Some(finish_reason.into())))
                            .await;
                        return;
                    }
                    let value: serde_json::Value = match serde_json::from_str(&event.data) {
                        Ok(value) => value,
                        Err(error) => {
                            let _ = tx.send(Err(GatewayError::Json(error))).await;
                            return;
                        }
                    };
                    let event_type = event.event.as_deref().or_else(|| value["type"].as_str());
                    if matches!(
                        event_type,
                        Some("response.refusal.delta" | "response.refusal.done")
                    ) {
                        finish_reason = "refusal";
                    }
                    match event_type {
                        Some("response.output_item.added" | "response.output_item.done")
                            if value["item"]["type"] == "function_call" =>
                        {
                            let Some(index) = value["output_index"].as_u64() else {
                                let _ = tx
                                    .send(Err(GatewayError::InvalidResponse(
                                        "function call has no output index".into(),
                                    )))
                                    .await;
                                return;
                            };
                            let item = &value["item"];
                            let id = match function_call_id(item["call_id"].as_str()) {
                                Ok(id) => id,
                                Err(error) => {
                                    let _ = tx.send(Err(error)).await;
                                    return;
                                }
                            };
                            calls.insert(
                                index as usize,
                                super::PendingToolCall {
                                    id,
                                    name: item["name"].as_str().unwrap_or_default().into(),
                                    arguments: item["arguments"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .into(),
                                },
                            );
                        }
                        Some(
                            "response.function_call_arguments.delta"
                            | "response.function_call_arguments.done",
                        ) => {
                            let call = value["output_index"]
                                .as_u64()
                                .and_then(|index| calls.get_mut(&(index as usize)));
                            let Some(call) = call else {
                                let _ = tx
                                    .send(Err(GatewayError::InvalidResponse(
                                        "arguments precede their function call".into(),
                                    )))
                                    .await;
                                return;
                            };
                            if event_type == Some("response.function_call_arguments.done") {
                                if let Some(arguments) = value["arguments"].as_str() {
                                    call.arguments = arguments.into();
                                }
                            } else if let Some(delta) = value["delta"].as_str() {
                                call.arguments.push_str(delta);
                            }
                        }
                        Some("response.output_text.delta" | "response.refusal.delta") => {
                            if let Some(delta) = value["delta"].as_str()
                                && tx.send(Ok(ChatChunk::delta(delta))).await.is_err()
                            {
                                return;
                            }
                        }
                        Some(
                            "response.reasoning_summary_text.delta"
                            | "response.reasoning_text.delta",
                        ) => {
                            if let Some(delta) = value["delta"].as_str()
                                && tx.send(Ok(ChatChunk::reasoning(delta))).await.is_err()
                            {
                                return;
                            }
                        }
                        Some("response.completed" | "response.done") => {
                            let _ = tx
                                .send(super::finish_tool_calls(calls, Some(finish_reason.into())))
                                .await;
                            return;
                        }
                        Some("response.failed" | "response.incomplete" | "error") => {
                            let _ = tx
                                .send(Err(GatewayError::InvalidResponse(value.to_string())))
                                .await;
                            return;
                        }
                        _ => {}
                    }
                }
            }
            let _ = tx
                .send(Err(GatewayError::InvalidResponse(
                    "Responses stream ended before its terminal event".into(),
                )))
                .await;
        });

        Ok(crate::gateway::reasoning::separate_stream(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        )))
    }
}
