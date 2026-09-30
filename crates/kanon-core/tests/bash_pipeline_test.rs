//! Which pipeline turns carry a Bash caller, exercised through the real engine without Docker.
//!
//! The contract: only a message from an explicitly listed administrator, in a conversation whose
//! context is that administrator's own, may run Bash. A notice is not a message, and a shared or
//! observed group session carries other members' words, so neither ever gets a caller — however
//! insistently the model calls the tool.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::pipeline::PipelineEngine;
use kanon_core::supervisor::Supervisor;
use kanon_core::{
    BashAvailabilityHook, BashExecutionMode, BashLocalConfig, BashPolicy, BashPolicyStore,
    BashTool, CommandPolicy, CommandPolicyStore, EventPolicy, EventPolicyStore,
    META_CONVERSATION_KIND, META_NOTICE, SessionScope,
};
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{Agent, ChatRequest, ChatResponse, GatewayError, LlmProvider, Role, ToolCall};
use kanon_proto::prost_types::{self, value::Kind};
use kanon_proto::v1::PipelineEventRequest;
use serde_json::json;

/// A model that always asks Bash to leave a marker, then answers with the tool result.
struct InsistentModel;

#[async_trait]
impl LlmProvider for InsistentModel {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        if request.messages.last().map(|message| message.role) == Some(Role::Tool) {
            return Ok(ChatResponse {
                content: Some("done".into()),
                ..ChatResponse::default()
            });
        }
        Ok(ChatResponse {
            tool_calls: vec![ToolCall {
                id: "bash-1".into(),
                name: "bash".into(),
                arguments: json!({"command":"touch ran"}),
            }],
            ..ChatResponse::default()
        })
    }
}

/// A pipeline whose only instance uses `scope` and `observe`, with local Bash in a temp directory.
async fn harness(scope: SessionScope, observe: bool) -> (Arc<PipelineEngine>, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_path_buf();
    std::mem::forget(temp);
    let tool = Arc::new(
        BashTool::new(
            dir.join("sandbox"),
            Arc::new(BashPolicyStore::new(BashPolicy {
                enabled: true,
                execution_mode: BashExecutionMode::Local,
                local: BashLocalConfig {
                    working_dir: dir.to_string_lossy().into_owned(),
                    auto_review: false,
                    review_model: None,
                },
                ..BashPolicy::default()
            })),
            Arc::new(CommandPolicyStore::new(CommandPolicy {
                admins: vec!["qq:admin".into()],
                ..CommandPolicy::default()
            })),
        )
        .unwrap(),
    );
    let agent = Arc::new(
        Agent::builder("bash-pipeline", Arc::new(InsistentModel))
            .model("test")
            .tool_arc(tool.clone())
            .hook(BashAvailabilityHook(tool))
            .build(),
    );
    let registry = Arc::new(InstanceRegistry::in_memory());
    let engine = Arc::new(
        PipelineEngine::new(Arc::new(Supervisor::new(Some(dir.join("run")), None)))
            .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
            .with_instances(registry.clone())
            .with_event_policy(Arc::new(EventPolicyStore::new(EventPolicy {
                reply_to_poke: true,
                ..EventPolicy::default()
            }))),
    );
    registry
        .create(InstanceDraft {
            name: "Bash Bot".into(),
            enabled: true,
            adapters: vec!["qq".into()],
            session_scope: scope,
            observe_group: observe,
            ..Default::default()
        })
        .await
        .unwrap();
    (engine, dir)
}

fn event(sender: &str, kind: &str, notice: Option<&str>) -> PipelineEventRequest {
    let mut fields = vec![(META_CONVERSATION_KIND, kind)];
    fields.extend(notice.map(|notice| (META_NOTICE, notice)));
    PipelineEventRequest {
        event_id: format!("{sender}-{kind}-{notice:?}"),
        platform: "qq".into(),
        channel_id: if kind == "group" {
            "group:1".into()
        } else {
            sender.into()
        },
        sender_id: sender.into(),
        raw_text: "please run the build".into(),
        segments: Vec::new(),
        metadata: Some(prost_types::Struct {
            fields: fields
                .into_iter()
                .map(|(key, value)| {
                    (
                        key.to_string(),
                        prost_types::Value {
                            kind: Some(Kind::StringValue(value.into())),
                        },
                    )
                })
                .collect(),
        }),
    }
}

/// Runs one event and reports whether its Bash call actually executed.
async fn ran(engine: &PipelineEngine, dir: &Path, event: PipelineEventRequest) -> bool {
    engine.process_event(event).await;
    let marker = dir.join("ran");
    let ran = marker.exists();
    if ran {
        std::fs::remove_file(marker).unwrap();
    }
    ran
}

#[tokio::test]
async fn only_an_administrators_own_conversation_can_run_bash() {
    let (engine, dir) = harness(SessionScope::User, false).await;
    assert!(ran(&engine, &dir, event("admin", "private", None)).await);
    assert!(ran(&engine, &dir, event("admin", "group", None)).await);
    assert!(!ran(&engine, &dir, event("member", "private", None)).await);
    // A poke's sender never asked for anything, even when it is an administrator's id.
    assert!(!ran(&engine, &dir, event("admin", "group", Some("poke"))).await);
}

#[tokio::test]
async fn shared_and_observed_group_sessions_never_carry_a_caller() {
    for (scope, observe) in [(SessionScope::Group, false), (SessionScope::User, true)] {
        let (engine, dir) = harness(scope, observe).await;
        assert!(
            !ran(&engine, &dir, event("admin", "group", None)).await,
            "{scope:?} observe={observe}"
        );
        // The same instance still serves the administrator's private chat.
        assert!(ran(&engine, &dir, event("admin", "private", None)).await);
    }
}
