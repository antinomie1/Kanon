//! Sandbox chat and the tool catalog share global plugin selection, including resident hosts.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use kanon_api::{ApiState, default_agent_config};
use kanon_core::Supervisor;
use kanon_core::toggle::PLUGIN_SECTION;
use kanon_llm::{ChatRequest, ChatResponse, GatewayError, LlmProvider, ToolCall};
use serde_json::json;

#[derive(Default)]
struct ToolCatalogProvider {
    call_disabled_tool: AtomicBool,
}

#[async_trait]
impl LlmProvider for ToolCatalogProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        if self.call_disabled_tool.swap(false, Ordering::SeqCst) {
            return Ok(ChatResponse {
                tool_calls: vec![ToolCall {
                    id: "unadvertised-call".to_string(),
                    name: "fixture_tool".to_string(),
                    arguments: json!({}),
                }],
                finish_reason: Some("tool_calls".to_string()),
                ..Default::default()
            });
        }
        Ok(ChatResponse {
            content: Some(
                json!(
                    request
                        .tools
                        .iter()
                        .map(|tool| &tool.name)
                        .collect::<Vec<_>>()
                )
                .to_string(),
            ),
            finish_reason: Some("stop".to_string()),
            ..Default::default()
        })
    }
}

async fn state(root: &Path, provider: Arc<ToolCatalogProvider>) -> ApiState {
    let supervisor = Arc::new(Supervisor::new(Some(root.join("run")), None));
    common::register_fixture_host(&supervisor).await;
    ApiState::builder(supervisor)
        .with_config_dir(root.join("config"))
        .with_llm_provider("test", provider, default_agent_config("mock-model"))
        .build()
}

#[tokio::test]
async fn resident_plugin_tools_disappear_from_catalog_and_sandbox_when_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path(), Arc::new(ToolCatalogProvider::default())).await;
    let app = kanon_api::app(state.clone());

    for (index, enabled) in [true, false, true].into_iter().enumerate() {
        // Retain the host to exercise policy independently of best-effort process shutdown.
        state
            .plugin_state()
            .set_enabled(PLUGIN_SECTION, common::FIXTURE_PLUGIN_ID, enabled)
            .await
            .unwrap();
        assert!(
            state
                .supervisor()
                .get_host(common::FIXTURE_HOST_ID)
                .await
                .is_some()
        );
        let (status, catalog) = common::send_json(&app, Method::GET, "/api/v1/tools", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(catalog["plugin"], json!(usize::from(enabled)));

        let (status, reply) = common::send_json(&app, Method::POST, "/api/v1/chat/completions", Some(json!({
            "session_id": format!("toggle-{index}"), "message": "show available tools", "tools": true,
        }))).await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        let names = if enabled {
            json!(["fixture_tool"])
        } else {
            json!([])
        };
        assert_eq!(reply["content"], json!(names.to_string()));
    }
}

#[tokio::test]
async fn a_model_cannot_dispatch_an_unadvertised_disabled_plugin_tool() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ToolCatalogProvider::default());
    provider.call_disabled_tool.store(true, Ordering::SeqCst);
    let state = state(dir.path(), provider).await;
    state
        .plugin_state()
        .set_enabled(PLUGIN_SECTION, common::FIXTURE_PLUGIN_ID, false)
        .await
        .unwrap();
    let app = kanon_api::app(state.clone());
    let (status, reply) = common::send_json(
        &app,
        Method::POST,
        "/api/v1/chat/completions",
        Some(json!({
            "session_id": "disabled-dispatch", "message": "try the remembered tool", "tools": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["executed_tools"][0]["plugin_id"], "unknown");
    assert_eq!(reply["executed_tools"][0]["success"], false);
    let messages = state
        .sessions()
        .memory()
        .get_messages("disabled-dispatch")
        .await
        .unwrap();
    assert!(messages.iter().any(|message| message.content.as_deref() == Some("Tool 'fixture_tool' not registered")));
}
