//! Host execution and automatic review exercise the real execution gate without Docker.
#![cfg(unix)]

use async_trait::async_trait;
use kanon_core::{
    BashExecutionMode, BashLocalConfig, BashPolicy, BashPolicyStore, BashPrincipal,
    BashReviewDecision, BashReviewRequest, BashReviewer, BashTool, ModelBashReviewer,
    with_bash_caller,
};
use kanon_llm::{
    AgentConfig, AgentFactory, AgentSlot, AgentTool, ChatMessage, ChatRequest, ChatResponse,
    GatewayError, InMemory, LlmProvider, Memory, PersonaRegistry, SessionManager,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

fn caller() -> BashPrincipal {
    BashPrincipal {
        platform: "test".into(),
        user_id: "owner".into(),
    }
}
fn policy(root: &std::path::Path, review: bool) -> Arc<BashPolicyStore> {
    Arc::new(BashPolicyStore::new(BashPolicy {
        execution_mode: BashExecutionMode::Local,
        local: BashLocalConfig {
            working_dir: root.to_string_lossy().into_owned(),
            auto_review: review,
            review_model: None,
        },
        allowlist: vec![caller()],
        ..Default::default()
    }))
}
async fn call(tool: &BashTool, command: &str) -> Result<String, String> {
    with_bash_caller(caller(), tool.call("test", json!({"command":command}))).await
}

#[tokio::test]
async fn local_mode_runs_without_docker_and_keeps_sender_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let settings = policy(dir.path(), false);
    let mut updated = settings.get();
    updated.sandbox.endpoint = "unix:///missing-docker.sock".into();
    settings.set(updated);
    let tool = BashTool::new(dir.path().join("sandbox"), settings).unwrap();
    let output = call(
        &tool,
        "printf host > local-result; python3 -c 'print(1+1)' ",
    )
    .await
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["execution_mode"], "local");
    assert_eq!(value["stdout"], "2\n");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("local-result")).unwrap(),
        "host"
    );
    assert!(
        tool.call("test", json!({"command":"touch forbidden"}))
            .await
            .unwrap_err()
            .contains("no permission")
    );
    assert!(!dir.path().join("forbidden").exists());
}

struct FixedReviewer {
    allow: bool,
    fail: bool,
    seen: Mutex<Vec<BashReviewRequest>>,
}
#[async_trait]
impl BashReviewer for FixedReviewer {
    fn available(&self, _: Option<&str>) -> bool {
        true
    }
    async fn review(
        &self,
        request: BashReviewRequest,
        _: Option<&str>,
    ) -> Result<BashReviewDecision, String> {
        self.seen.lock().unwrap().push(request);
        if self.fail {
            Err("review service failed".into())
        } else {
            Ok(BashReviewDecision {
                allow: self.allow,
                reason: "test decision".into(),
            })
        }
    }
}

#[tokio::test]
async fn only_explicit_review_approval_can_start_a_host_process() {
    let dir = tempfile::tempdir().unwrap();
    let tool = BashTool::new(dir.path().join("sandbox"), policy(dir.path(), true)).unwrap();
    assert!(
        call(&tool, "touch missing-review")
            .await
            .unwrap_err()
            .contains("reviewer is unavailable")
    );
    for (name, allow, fail) in [
        ("approved", true, false),
        ("denied", false, false),
        ("failed", true, true),
    ] {
        let reviewer = Arc::new(FixedReviewer {
            allow,
            fail,
            seen: Mutex::new(Vec::new()),
        });
        tool.set_reviewer(reviewer.clone());
        let command = format!("touch {name}");
        let result = call(&tool, &command).await;
        assert_eq!(result.is_ok(), allow && !fail, "{result:?}");
        assert_eq!(dir.path().join(name).exists(), allow && !fail);
        let seen = reviewer.seen.lock().unwrap();
        assert_eq!(seen[0].command, command);
        assert_eq!(
            seen[0].cwd,
            dir.path().canonicalize().unwrap().to_string_lossy()
        );
    }
    assert!(!dir.path().join("missing-review").exists());
}

struct GatedReviewer {
    started: Notify,
    release: Notify,
}
#[async_trait]
impl BashReviewer for GatedReviewer {
    fn available(&self, _: Option<&str>) -> bool {
        true
    }
    async fn review(
        &self,
        _: BashReviewRequest,
        _: Option<&str>,
    ) -> Result<BashReviewDecision, String> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(BashReviewDecision {
            allow: true,
            reason: "approved".into(),
        })
    }
}
#[tokio::test]
async fn approval_does_not_survive_permission_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let settings = policy(dir.path(), true);
    let tool = Arc::new(BashTool::new(dir.path().join("sandbox"), settings.clone()).unwrap());
    let reviewer = Arc::new(GatedReviewer {
        started: Notify::new(),
        release: Notify::new(),
    });
    tool.set_reviewer(reviewer.clone());
    let task = tokio::spawn({
        let tool = tool.clone();
        async move { call(&tool, "touch revoked").await }
    });
    reviewer.started.notified().await;
    let mut next = settings.get();
    next.allowlist.clear();
    settings.set(next);
    reviewer.release.notify_one();
    assert!(
        task.await
            .unwrap()
            .unwrap_err()
            .contains("permission or configuration changed")
    );
    assert!(!dir.path().join("revoked").exists());
}

struct ReviewModel {
    response: Mutex<String>,
    requests: Mutex<Vec<ChatRequest>>,
}
#[async_trait]
impl LlmProvider for ReviewModel {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(ChatResponse {
            content: Some(self.response.lock().unwrap().clone()),
            ..Default::default()
        })
    }
}
#[tokio::test]
async fn live_model_review_has_no_tools_or_history_and_malformed_json_denies() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(ReviewModel {
        response: Mutex::new(r#"{"allow":true,"reason":"routine workspace write"}"#.into()),
        requests: Mutex::new(Vec::new()),
    });
    let memory = Arc::new(InMemory::new());
    Memory::push_message(
        memory.as_ref(),
        "conversation",
        ChatMessage::user("private conversation history"),
    )
    .await
    .unwrap();
    let factory = Arc::new(AgentFactory::new(
        "test",
        Arc::new(AgentSlot::new()),
        memory.clone(),
        Arc::new(SessionManager::new(memory.clone())),
        Arc::new(PersonaRegistry::default()),
        vec![],
        vec![],
    ));
    factory.install(
        "review",
        model.clone(),
        AgentConfig {
            default_model: "review-model".into(),
            ..Default::default()
        },
    );
    let tool = BashTool::new(dir.path().join("sandbox"), policy(dir.path(), true)).unwrap();
    tool.set_reviewer(Arc::new(ModelBashReviewer::new(Arc::downgrade(&factory))));
    call(&tool, "touch model-approved").await.unwrap();
    let requests = model.requests.lock().unwrap().clone();
    assert!(requests[0].tools.is_empty());
    assert_eq!(requests[0].messages.len(), 2);
    assert!(
        !serde_json::to_string(&requests[0])
            .unwrap()
            .contains("private conversation history")
    );
    assert_eq!(
        memory
            .snapshot("conversation")
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
    *model.response.lock().unwrap() = "yes, go ahead".into();
    assert!(
        call(&tool, "touch invalid-approval")
            .await
            .unwrap_err()
            .contains("Invalid Bash review decision")
    );
    assert!(!dir.path().join("invalid-approval").exists());
}
