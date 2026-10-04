//! Which pipeline turns may run Bash, exercised through the real engine without Docker.
//!
//! The contract: only a message from an administrator listed by the serving instance's command
//! policy (its own, or the node's) may run Bash. A notice is not a message, so it never can. A
//! shared or observed group session carries other members' words, so it may only when the
//! instance explicitly allows shared contexts — however insistently the model calls the tool.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use kanon_core::instance::{InstanceDraft, InstanceRegistry};
use kanon_core::pipeline::PipelineEngine;
use kanon_core::pipeline::PipelineResult;
use kanon_core::supervisor::Supervisor;
use kanon_core::{
    BashAvailabilityHook, BashExecutionMode, BashLocalConfig, BashPolicy, BashPolicyStore,
    BashScope, BashTool, CommandPolicy, CommandPolicyStore, EventPolicy, EventPolicyStore,
    META_CONVERSATION_KIND, META_NOTICE, SessionScope,
};
use kanon_llm::BuiltinAgent;
use kanon_llm::tool_router::ToolRouter;
use kanon_llm::{ChatRequest, ChatResponse, GatewayError, LlmProvider, Role, ToolCall};
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

/// A pipeline whose only instance serves `qq` as `draft` describes, with local Bash in a temp
/// directory. `qq:admin` is the node's only administrator.
async fn harness(draft: InstanceDraft) -> (Arc<PipelineEngine>, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_path_buf();
    std::mem::forget(temp);
    let registry = Arc::new(InstanceRegistry::in_memory());
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
            registry.clone(),
        )
        .unwrap(),
    );
    let commands = tool.command_policy().clone();
    let agent = Arc::new(
        BuiltinAgent::builder("bash-pipeline", Arc::new(InsistentModel))
            .model("test")
            .tool_arc(tool.clone())
            .hook(BashAvailabilityHook(tool))
            .build(),
    );
    let engine = Arc::new(
        PipelineEngine::new(Arc::new(Supervisor::new(Some(dir.join("run")), None)))
            .with_tool_router(Arc::new(ToolRouter::from_arc(agent)))
            .with_instances(registry.clone())
            .with_command_policy(commands)
            .with_event_policy(Arc::new(EventPolicyStore::new(EventPolicy {
                reply_to_poke: true,
                ..EventPolicy::default()
            }))),
    );
    registry
        .create(
            InstanceDraft {
                name: "Bash Bot".into(),
                enabled: true,
                adapters: vec!["qq".into()],
                ..draft
            },
            None,
        )
        .await
        .unwrap();
    (engine, dir)
}

/// An instance draft with the given group context and Bash scope; everything else is default.
fn draft(scope: SessionScope, observe: bool, bash: BashScope) -> InstanceDraft {
    InstanceDraft {
        session_scope: scope,
        observe_group: observe,
        bash,
        ..Default::default()
    }
}

fn event(sender: &str, kind: &str, notice: Option<&str>) -> PipelineEventRequest {
    message(sender, kind, notice, "please run the build")
}

fn message(sender: &str, kind: &str, notice: Option<&str>, text: &str) -> PipelineEventRequest {
    let mut fields = vec![(META_CONVERSATION_KIND, kind)];
    fields.extend(notice.map(|notice| (META_NOTICE, notice)));
    PipelineEventRequest {
        event_id: format!("{sender}-{kind}-{notice:?}-{text}"),
        platform: "qq".into(),
        channel_id: if kind == "group" {
            "group:1".into()
        } else {
            sender.into()
        },
        sender_id: sender.into(),
        raw_text: text.into(),
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
    let (engine, dir) = harness(draft(SessionScope::User, false, BashScope::OwnContext)).await;
    assert!(ran(&engine, &dir, event("admin", "private", None)).await);
    assert!(ran(&engine, &dir, event("admin", "group", None)).await);
    assert!(!ran(&engine, &dir, event("member", "private", None)).await);
    // A poke's sender never asked for anything, even when it is an administrator's id.
    assert!(!ran(&engine, &dir, event("admin", "group", Some("poke"))).await);
}

#[tokio::test]
async fn shared_and_observed_group_sessions_need_an_explicit_opt_in() {
    for (scope, observe) in [(SessionScope::Group, false), (SessionScope::User, true)] {
        let (engine, dir) = harness(draft(scope, observe, BashScope::OwnContext)).await;
        assert!(
            !ran(&engine, &dir, event("admin", "group", None)).await,
            "{scope:?} observe={observe}"
        );
        // The same instance still serves the administrator's private chat.
        assert!(ran(&engine, &dir, event("admin", "private", None)).await);

        let (engine, dir) = harness(draft(scope, observe, BashScope::SharedContext)).await;
        assert!(
            ran(&engine, &dir, event("admin", "group", None)).await,
            "{scope:?} observe={observe} with shared contexts allowed"
        );
        // Allowing shared contexts never extends Bash beyond administrators or to notices.
        assert!(!ran(&engine, &dir, event("member", "group", None)).await);
        assert!(!ran(&engine, &dir, event("admin", "group", Some("poke"))).await);
    }
}

#[tokio::test]
async fn a_disabled_instance_scope_blocks_even_node_administrators() {
    let (engine, dir) = harness(draft(SessionScope::User, false, BashScope::Disabled)).await;
    assert!(!ran(&engine, &dir, event("admin", "private", None)).await);
}

#[tokio::test]
async fn an_instance_command_policy_replaces_the_node_administrators() {
    let (engine, dir) = harness(InstanceDraft {
        command_policy: Some(CommandPolicy {
            admins: vec!["qq:ops".into()],
            ..CommandPolicy::default()
        }),
        ..Default::default()
    })
    .await;
    // Bash follows the instance's administrators, not the node's.
    assert!(ran(&engine, &dir, event("ops", "private", None)).await);
    assert!(!ran(&engine, &dir, event("admin", "private", None)).await);
    // So do restricted commands: `/model` is administrators-only by default.
    let denied = engine
        .process_event(message("admin", "private", None, "/model"))
        .await;
    assert!(
        matches!(denied, PipelineResult::CommandDenied { .. }),
        "{denied:?}"
    );
    let allowed = engine
        .process_event(message("ops", "private", None, "/model"))
        .await;
    assert!(
        !matches!(allowed, PipelineResult::CommandDenied { .. }),
        "{allowed:?}"
    );
}
