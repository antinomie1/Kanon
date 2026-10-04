//! Tool policy and token totals apply to every model round, including streamed rounds.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kanon_llm::tool_router::ToolHost;
use kanon_llm::{
    Agent, AgentError, AgentHook, BuiltinAgent, ChatMessage, ChatRequest, ChatResponse,
    GatewayError, InMemory, LlmProvider, Memory, NativeTool, SessionManager, TokenUsage, ToolCall,
    ToolDefinition, TurnOptions,
};
use kanon_proto::v1::{PluginMeta, ToolCallRequest, ToolCallResponse, ToolMeta};
use serde_json::json;
use tokio_stream::StreamExt;

struct ScriptedProvider {
    responses: Mutex<VecDeque<Result<ChatResponse, GatewayError>>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<Result<ChatResponse, GatewayError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        self.requests.lock().unwrap().push(request.clone());
        self.responses.lock().unwrap().pop_front().expect("script")
    }
}

#[derive(Default)]
struct CountedHost(AtomicUsize);

#[async_trait]
impl ToolHost for CountedHost {
    fn host_id(&self) -> &str {
        "external-host"
    }

    fn plugin_metas(&self) -> Vec<PluginMeta> {
        vec![PluginMeta {
            id: "external-plugin".into(),
            tools: vec![ToolMeta {
                name: "external".into(),
                ..Default::default()
            }],
            ..Default::default()
        }]
    }

    async fn call_tool(&self, request: ToolCallRequest) -> Result<ToolCallResponse, tonic::Status> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolCallResponse {
            call_id: request.call_id,
            success: true,
            ..Default::default()
        })
    }
}

fn definition(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: "Counts executions".into(),
        parameters: json!({"type": "object"}),
    }
}

fn call(name: &str) -> ToolCall {
    ToolCall {
        id: format!("call-{name}"),
        name: name.into(),
        arguments: json!({}),
    }
}

fn native(counter: Arc<AtomicUsize>) -> NativeTool {
    NativeTool::new(definition("native"), move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        async { Ok("native result".into()) }
    })
}

struct AddToolsHook {
    inject_response: AtomicBool,
}

#[async_trait]
impl AgentHook for AddToolsHook {
    async fn on_llm_request(&self, _: &str, request: &mut ChatRequest) -> Result<(), AgentError> {
        request.tools.push(definition("hook_added"));
        Ok(())
    }

    async fn on_llm_response(
        &self,
        _: &str,
        response: &mut ChatResponse,
    ) -> Result<(), AgentError> {
        if self.inject_response.swap(false, Ordering::SeqCst) {
            response.tool_calls = vec![call("native"), call("external")];
        }
        Ok(())
    }
}

#[tokio::test]
async fn disabled_tools_cannot_be_advertised_or_dispatched_by_models_or_hooks() {
    for disable_config in [false, true] {
        for source in ["structured", "textual", "hook"] {
            for streaming in [false, true] {
                // The streaming API uses model configuration rather than per-turn options.
                if streaming && !disable_config {
                    continue;
                }
                let response = match source {
                    "structured" => ChatResponse {
                        tool_calls: vec![call("native"), call("external")],
                        ..Default::default()
                    },
                    "textual" => ChatResponse {
                        content: Some(
                            "<tool_call>[{\"name\":\"native\",\"arguments\":{}},\
                             {\"name\":\"external\",\"arguments\":{}}]</tool_call>"
                                .into(),
                        ),
                        ..Default::default()
                    },
                    _ => ChatResponse::default(),
                };
                let provider = Arc::new(ScriptedProvider::new(vec![
                    Ok(response),
                    Ok(ChatResponse {
                        content: Some("next turn works".into()),
                        ..Default::default()
                    }),
                ]));
                let counter = Arc::new(AtomicUsize::new(0));
                let host = Arc::new(CountedHost::default());
                let hosts: Vec<Arc<dyn ToolHost>> = vec![host.clone()];
                let memory = Arc::new(InMemory::new());
                let agent = BuiltinAgent::builder("policy", provider.clone())
                    .memory(memory.clone())
                    .tool(native(counter.clone()))
                    .tool_calling(!disable_config)
                    .hook(AddToolsHook {
                        inject_response: AtomicBool::new(source == "hook"),
                    })
                    .compaction(None)
                    .build();
                if streaming {
                    let mut stream = agent.run_stream("s", "first", &hosts).await.unwrap();
                    let mut failed = false;
                    while let Some(chunk) = stream.next().await {
                        if let Err(error) = chunk {
                            assert!(matches!(
                                error,
                                AgentError::Gateway(GatewayError::InvalidResponse(_))
                            ));
                            failed = true;
                        }
                    }
                    assert!(failed, "{source}");
                } else {
                    let error = agent
                        .run_message_with(
                            "s",
                            ChatMessage::user("first"),
                            &hosts,
                            TurnOptions {
                                without_tools: !disable_config,
                                ..Default::default()
                            },
                        )
                        .await
                        .unwrap_err();
                    assert!(
                        error.to_string().contains("tools are disabled"),
                        "{source}: {error}"
                    );
                }
                assert_eq!(counter.load(Ordering::SeqCst), 0, "{source}");
                assert_eq!(host.0.load(Ordering::SeqCst), 0, "{source}");
                assert!(provider.requests.lock().unwrap()[0].tools.is_empty());
                let history = Memory::get_messages(memory.as_ref(), "s").await.unwrap();
                assert_eq!(history.len(), 2, "the rejected turn is closed once");
                assert!(history.iter().all(|message| message.tool_calls.is_none()));
                assert_eq!(
                    agent.run("s", "next", &hosts).await.unwrap().content,
                    "next turn works"
                );
            }
        }
    }
}

#[tokio::test]
async fn session_tokens_include_every_complete_and_streamed_tool_round_once() {
    for streaming in [false, true] {
        for at_limit in [false, true] {
            let responses = vec![
                ChatResponse {
                    tool_calls: vec![call("native")],
                    usage: Some(TokenUsage {
                        total_tokens: 100,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ChatResponse {
                    content: Some("second round".into()),
                    tool_calls: vec![call("external")],
                    ..Default::default()
                },
                ChatResponse {
                    content: Some("finished".into()),
                    usage: Some(TokenUsage {
                        total_tokens: 300,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ];
            let provider = Arc::new(ScriptedProvider::new(
                responses.iter().cloned().map(Ok).collect(),
            ));
            let sessions = Arc::new(SessionManager::new(Arc::new(InMemory::new())));
            let counter = Arc::new(AtomicUsize::new(0));
            let host = Arc::new(CountedHost::default());
            let hosts: Vec<Arc<dyn ToolHost>> = vec![host.clone()];
            let agent = BuiltinAgent::builder("usage", provider.clone())
                .session_manager(sessions.clone())
                .system_prompt("Count all repeated input, tools, and generated responses.")
                .tool(native(counter.clone()))
                .max_iterations(if at_limit { 1 } else { 2 })
                .compaction(None)
                .build();
            if streaming {
                let mut stream = agent.run_stream("s", "first", &hosts).await.unwrap();
                while let Some(chunk) = stream.next().await {
                    chunk.unwrap();
                }
            } else {
                agent.run("s", "first", &hosts).await.unwrap();
            }
            let requests = provider.requests.lock().unwrap();
            assert_eq!(requests.len(), if at_limit { 2 } else { 3 });
            let expected: usize = requests
                .iter()
                .zip(&responses)
                .map(|(request, response)| {
                    if !streaming && let Some(usage) = &response.usage {
                        usage.total_tokens as usize
                    } else {
                        kanon_llm::token::estimate_request_tokens(request)
                            + kanon_llm::token::estimate_message_tokens(
                                &response.assistant_message(),
                            )
                    }
                })
                .sum();
            let metadata = sessions.get_metadata("s").unwrap();
            assert_eq!(metadata.turn_count, 1);
            assert_eq!(
                metadata.total_tokens_used, expected,
                "streaming={streaming}, at_limit={at_limit}"
            );
            assert_eq!(counter.load(Ordering::SeqCst), 1);
            assert_eq!(host.0.load(Ordering::SeqCst), usize::from(!at_limit));
        }
    }
}

#[tokio::test]
async fn failed_turns_keep_the_existing_uncounted_usage_convention() {
    let provider = Arc::new(ScriptedProvider::new(vec![
        Ok(ChatResponse {
            tool_calls: vec![call("native")],
            usage: Some(TokenUsage {
                total_tokens: 100,
                ..Default::default()
            }),
            ..Default::default()
        }),
        Err(GatewayError::InvalidResponse("second round failed".into())),
    ]));
    let sessions = Arc::new(SessionManager::new(Arc::new(InMemory::new())));
    sessions.get_or_create("s");
    let counter = Arc::new(AtomicUsize::new(0));
    let agent = BuiltinAgent::builder("failed-usage", provider)
        .session_manager(sessions.clone())
        .tool(native(counter.clone()))
        .compaction(None)
        .build();
    assert!(agent.run("s", "first", &[]).await.is_err());
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    let metadata = sessions.get_metadata("s").unwrap();
    assert_eq!(metadata.turn_count, 0);
    assert_eq!(metadata.total_tokens_used, 0);
}
