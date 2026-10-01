//! `send_file` attaches files from the Bash working directory to the reply, for the same callers
//! Bash serves, without ever reading outside that directory.
#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kanon_core::instance::InstanceRegistry;
use kanon_core::{
    BashCaller, BashPolicy, BashPolicyStore, BashTool, CommandPolicy, CommandPolicyStore,
    MAX_SEND_FILE_BYTES, SendFileTool, with_bash_caller,
};
use kanon_llm::{
    Agent, AgentTool, ChatRequest, ChatResponse, GatewayError, LlmProvider, Role, ToolCall,
    ToolOutput,
};
use serde_json::json;

/// A workspace whose Bash only `onebot:alice` may use, and its `send_file` companion.
struct Fixture {
    workspace: tempfile::TempDir,
    attachments: tempfile::TempDir,
    tool: SendFileTool,
}

fn fixture() -> Fixture {
    let workspace = tempfile::tempdir().unwrap();
    let attachments = tempfile::tempdir().unwrap();
    let bash = BashTool::new(
        workspace.path(),
        Arc::new(BashPolicyStore::new(BashPolicy {
            enabled: true,
            ..BashPolicy::default()
        })),
        Arc::new(CommandPolicyStore::new(CommandPolicy {
            admins: vec!["onebot:alice".into()],
            ..CommandPolicy::default()
        })),
        Arc::new(InstanceRegistry::in_memory()),
    )
    .unwrap();
    let tool = bash.send_file_tool(attachments.path());
    Fixture {
        workspace,
        attachments,
        tool,
    }
}

async fn send(fixture: &Fixture, sender: &str, path: &str) -> Result<ToolOutput, String> {
    with_bash_caller(
        Some(BashCaller::new(format!("onebot:{sender}"))),
        fixture.tool.call("group", json!({ "path": path })),
    )
    .await
}

#[tokio::test]
async fn a_workspace_file_is_copied_out_and_attached() {
    let fixture = fixture();
    let out = fixture.workspace.path().join("out");
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("chart.png"), b"\x89PNG chart").unwrap();

    let sent = send(&fixture, "alice", "out/chart.png")
        .await
        .expect("sent");
    assert_eq!(sent.attachments.len(), 1);
    let attachment = &sent.attachments[0];
    assert_eq!(attachment.mime_type, "image/png");
    let copy = Path::new(attachment.file_path.as_deref().unwrap());
    assert!(copy.starts_with(fixture.attachments.path().canonicalize().unwrap()));
    assert_eq!(copy.file_name().unwrap(), "chart.png");
    // The model learns what was attached, never where the node keeps it.
    assert!(sent.text.contains("chart.png"), "{}", sent.text);
    assert!(!sent.text.contains(copy.parent().unwrap().to_str().unwrap()));

    // What is delivered is the copy: later edits in the workspace do not change it.
    std::fs::write(out.join("chart.png"), b"overwritten").unwrap();
    assert_eq!(std::fs::read(copy).unwrap(), b"\x89PNG chart");

    // The container's view of the same file, and a type without a known extension.
    std::fs::write(out.join("data.parquet"), b"PAR1").unwrap();
    let sent = send(&fixture, "alice", "/workspace/out/data.parquet")
        .await
        .expect("sent");
    assert_eq!(sent.attachments[0].mime_type, "application/octet-stream");
}

#[tokio::test]
async fn links_escapes_and_special_files_are_refused() {
    let fixture = fixture();
    let workspace = fixture.workspace.path();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), b"host secret").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        workspace.join("leak.txt"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), workspace.join("host")).unwrap();
    std::fs::create_dir(workspace.join("dir")).unwrap();
    let fifo =
        std::ffi::CString::new(workspace.join("pipe").as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `fifo` is a NUL-terminated path inside the test's own temporary directory.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

    for (path, reason) in [
        ("leak.txt", "symbolic link"),
        ("host/secret.txt", "symbolic link"),
        ("../secret.txt", "not inside"),
        (
            outside.path().join("secret.txt").to_str().unwrap(),
            "not inside",
        ),
        ("missing.txt", "does not exist"),
        ("dir", "not a regular file"),
        ("/workspace", "names no file"),
    ] {
        let err = send(&fixture, "alice", path).await.expect_err(path);
        assert!(err.contains(reason), "{path}: {err}");
    }
    // A FIFO with no writer must not hang the call.
    let err = tokio::time::timeout(Duration::from_secs(5), send(&fixture, "alice", "pipe"))
        .await
        .expect("returns at once")
        .expect_err("a FIFO is no file");
    assert!(err.contains("not a regular file"), "{err}");

    // Nothing was copied out for any of them.
    assert_eq!(
        std::fs::read_dir(fixture.attachments.path())
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn only_bash_administrators_may_send_files_and_size_is_bounded() {
    let fixture = fixture();
    std::fs::write(fixture.workspace.path().join("report.pdf"), b"%PDF").unwrap();

    let err = send(&fixture, "mallory", "report.pdf")
        .await
        .expect_err("not an administrator");
    assert!(err.contains("Sending files denied"), "{err}");
    let err = fixture
        .tool
        .call("group", json!({ "path": "report.pdf" }))
        .await
        .expect_err("no verified sender");
    assert!(err.contains("Sending files denied"), "{err}");

    // Sparse, so the test costs no disk space.
    let big = std::fs::File::create(fixture.workspace.path().join("big.bin")).unwrap();
    big.set_len(MAX_SEND_FILE_BYTES + 1).unwrap();
    let err = send(&fixture, "alice", "big.bin")
        .await
        .expect_err("over the limit");
    assert!(err.contains("at most 20 MiB"), "{err}");
}

/// Calls `send_file` once, then answers with the tool's result.
struct SendingModel;

#[async_trait]
impl LlmProvider for SendingModel {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, GatewayError> {
        if let Some(result) = request.messages.iter().find(|m| m.role == Role::Tool) {
            return Ok(ChatResponse {
                content: result.content.clone(),
                ..ChatResponse::default()
            });
        }
        Ok(ChatResponse {
            tool_calls: vec![ToolCall {
                id: "send-1".into(),
                name: "send_file".into(),
                arguments: json!({ "path": "song.mp3" }),
            }],
            ..ChatResponse::default()
        })
    }
}

#[tokio::test]
async fn an_attached_file_reaches_the_turn_output() {
    let fixture = fixture();
    std::fs::write(fixture.workspace.path().join("song.mp3"), b"ID3").unwrap();
    let workspace = fixture.workspace;
    let attachments = fixture.attachments;
    let agent = Agent::builder("sender", Arc::new(SendingModel))
        .model("test")
        .tool(fixture.tool)
        .build();

    let output = with_bash_caller(
        Some(BashCaller::new("onebot:alice")),
        agent.run("group", "send me the song", &[]),
    )
    .await
    .expect("turn");
    assert_eq!(output.attachments.len(), 1);
    assert_eq!(output.attachments[0].mime_type, "audio/mpeg");
    assert!(
        Path::new(output.attachments[0].file_path.as_deref().unwrap())
            .starts_with(attachments.path().canonicalize().unwrap())
    );
    drop(workspace);
}
