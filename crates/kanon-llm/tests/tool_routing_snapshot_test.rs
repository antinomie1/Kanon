//! Advertised tool names resolve to one captured provider, without parsing names during dispatch.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kanon_llm::tool_router::{ToolHost, resolve_tools};
use kanon_llm::{
    Agent, AgentError, BuiltinAgent, ChatRequest, ChatResponse, GatewayError, LlmProvider,
    NativeTool, ToolCall, ToolDefinition,
};
use kanon_proto::v1::{PluginMeta, ToolCallRequest, ToolCallResponse, ToolMeta};
use serde_json::json;

struct Host {
    id: String,
    metadata: Mutex<Vec<PluginMeta>>,
    reads: AtomicUsize,
    calls: Mutex<Vec<String>>,
}

impl Host {
    fn new(id: &str, plugin: &str, tool: &str) -> Arc<Self> {
        Arc::new(Self {
            id: id.into(),
            metadata: Mutex::new(vec![PluginMeta {
                id: plugin.into(),
                tools: vec![ToolMeta {
                    name: tool.into(),
                    ..Default::default()
                }],
                ..Default::default()
            }]),
            reads: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl ToolHost for Host {
    fn host_id(&self) -> &str {
        &self.id
    }

    fn plugin_metas(&self) -> Vec<PluginMeta> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.metadata.lock().unwrap().clone()
    }

    async fn call_tool(&self, request: ToolCallRequest) -> Result<ToolCallResponse, tonic::Status> {
        self.calls.lock().unwrap().push(request.tool_name);
        Ok(ToolCallResponse {
            call_id: request.call_id,
            success: true,
            ..Default::default()
        })
    }
}

struct Provider {
    requested: Vec<ToolCall>,
    requests: Mutex<Vec<ChatRequest>>,
    refresh: Option<Arc<Host>>,
}

#[async_trait]
impl LlmProvider for Provider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        let mut requests = self.requests.lock().unwrap();
        requests.push(request.clone());
        if requests.len() == 1 {
            if let Some(host) = &self.refresh {
                host.metadata.lock().unwrap().clear();
            }
            Ok(ChatResponse {
                tool_calls: self.requested.clone(),
                ..Default::default()
            })
        } else {
            Ok(ChatResponse {
                content: Some("done".into()),
                ..Default::default()
            })
        }
    }
}

fn calls(names: &[&str]) -> Vec<ToolCall> {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| ToolCall {
            id: format!("call-{index}"),
            name: (*name).into(),
            arguments: json!({}),
        })
        .collect()
}

#[tokio::test]
async fn native_plugin_collisions_fail_before_advertisement_or_execution() {
    let host = Host::new("external", "plugin", "bash");
    let provider = Arc::new(Provider {
        requested: calls(&["bash"]),
        requests: Mutex::new(Vec::new()),
        refresh: None,
    });
    let executions = Arc::new(AtomicUsize::new(0));
    let counted = executions.clone();
    let agent = BuiltinAgent::builder("collision", provider.clone())
        .tool(NativeTool::new(
            ToolDefinition {
                name: "bash".into(),
                description: "Native command execution".into(),
                parameters: json!({"type":"object"}),
            },
            move |_, _| {
                counted.fetch_add(1, Ordering::SeqCst);
                async { Ok("executed".into()) }
            },
        ))
        .compaction(None)
        .build();
    let hosts: Vec<Arc<dyn ToolHost>> = vec![host.clone()];
    let error = agent.run("s", "execute", &hosts).await.unwrap_err();
    assert!(matches!(error, AgentError::InvalidRequest(_)));
    assert!(error.to_string().contains("duplicate tool name 'bash'"));
    assert!(provider.requests.lock().unwrap().is_empty());
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(host.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn literal_names_and_refreshed_metadata_keep_their_advertised_targets() {
    for refresh in [false, true] {
        let namespace = Host::new("first", "foo", "bar");
        let literal = Host::new("second", "literal", "foo__bar");
        let provider = Arc::new(Provider {
            requested: calls(&["bar", "foo__bar"]),
            requests: Mutex::new(Vec::new()),
            refresh: refresh.then(|| literal.clone()),
        });
        let agent = BuiltinAgent::builder("snapshot", provider.clone())
            .compaction(None)
            .build();
        let hosts: Vec<Arc<dyn ToolHost>> = vec![namespace.clone(), literal.clone()];
        let output = agent.run("s", "call both", &hosts).await.unwrap();
        assert_eq!(output.content, "done");
        assert_eq!(*namespace.calls.lock().unwrap(), ["bar"]);
        assert_eq!(*literal.calls.lock().unwrap(), ["foo__bar"]);
        assert_eq!(namespace.reads.load(Ordering::SeqCst), 1);
        assert_eq!(literal.reads.load(Ordering::SeqCst), 1);
        assert_eq!(output.executed_tools[0].plugin_id, "foo");
        assert_eq!(output.executed_tools[1].plugin_id, "literal");
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].tools, requests[1].tools);
        assert_eq!(
            requests[0]
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["bar", "foo__bar"]
        );
    }
}

#[test]
fn namespacing_collisions_are_rejected_instead_of_selecting_the_first_provider() {
    // Sanitizing identifiers and adding a namespace can both produce another declared name.
    for hosts in [
        vec![
            Host::new("one", "foo.bar", "lookup"),
            Host::new("two", "foo_bar", "lookup"),
        ],
        vec![
            Host::new("one", "foo", "bar"),
            Host::new("two", "other", "bar"),
            Host::new("three", "literal", "foo__bar"),
        ],
    ] {
        let hosts: Vec<Arc<dyn ToolHost>> = hosts
            .into_iter()
            .map(|host| host as Arc<dyn ToolHost>)
            .collect();
        let error = resolve_tools(&hosts).unwrap_err();
        assert!(error.to_string().contains("duplicate tool name"));
    }
}
