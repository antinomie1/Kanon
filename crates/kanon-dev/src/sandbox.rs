//! Offline sandbox environment for isolated plugin execution and testing.
//!
//! Spawns a dedicated sub-process plugin host on an isolated temporary endpoint,
//! runs a mock core `BotApiService`, and enables direct interactive or non-interactive
//! testing of commands, pre-filters, and LLM Tool Calling.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use kanon_core::ipc::{CoreApiService, CoreIpcServer, DEFAULT_INGEST_QUEUE_CAPACITY};
use kanon_core::supervisor::{ManagedHost, Supervisor, SupervisorError};
use kanon_proto::prost_types;
use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    CommandExecuteRequest, CommandExecuteResponse, ToolCallRequest, ToolCallResponse,
    audio_segment, image_segment, tool_call_request, tool_call_response,
};

use crate::lint::{LintError, find_manifest_path};

/// Errors occurring during sandbox execution.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// Manifest location or validation failure.
    #[error("Manifest lookup error: {0}")]
    Lint(#[from] LintError),
    /// Host supervisor failure.
    #[error("Supervisor lifecycle error: {0}")]
    Supervisor(#[from] SupervisorError),
    /// File I/O failure.
    #[error("I/O error in sandbox: {0}")]
    Io(#[from] std::io::Error),
    /// gRPC status error.
    #[error("gRPC RPC error from host: {0}")]
    Rpc(Box<tonic::Status>),
    /// Specified command was not found on the plugin.
    #[error("Command '/{0}' is not declared by the plugin")]
    CommandNotFound(String),
    /// Specified tool was not found on the plugin.
    #[error("Tool '{0}' is not declared by the plugin")]
    ToolNotFound(String),
    /// JSON parsing error when processing tool arguments.
    #[error("Invalid JSON tool arguments: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<tonic::Status> for SandboxError {
    fn from(status: tonic::Status) -> Self {
        Self::Rpc(Box::new(status))
    }
}

/// Execution options configuring sandbox behavior.
#[derive(Debug, Clone, Default)]
pub struct SandboxOptions {
    /// Specific command to execute (e.g. `pycalc` or `/pycalc`).
    pub command: Option<String>,
    /// Specific tool to invoke.
    pub tool: Option<String>,
    /// Arguments passed alongside the command or tool.
    pub args: Vec<String>,
    /// Whether to run without opening an interactive terminal REPL.
    pub non_interactive: bool,
}

/// Renders a list of MessageSegments into a readable string.
fn format_segments(segments: &[kanon_proto::v1::MessageSegment]) -> String {
    let mut out = Vec::new();
    for seg in segments {
        if let Some(ref inner) = seg.segment {
            match inner {
                Segment::Text(t) => out.push(t.content.clone()),
                Segment::Image(i) => match &i.source {
                    Some(image_segment::Source::Url(u)) => out.push(format!("[Image URL: {}]", u)),
                    Some(image_segment::Source::FilePath(p)) => {
                        out.push(format!("[Image File: {}]", p))
                    }
                    Some(image_segment::Source::RawBytes(b)) => {
                        out.push(format!("[Image Bytes: {}B]", b.len()))
                    }
                    None => out.push("[Image]".to_string()),
                },
                Segment::Audio(a) => match &a.source {
                    Some(audio_segment::Source::Url(u)) => out.push(format!("[Audio URL: {}]", u)),
                    Some(audio_segment::Source::FilePath(p)) => {
                        out.push(format!("[Audio File: {}]", p))
                    }
                    Some(audio_segment::Source::RawBytes(b)) => {
                        out.push(format!("[Audio Bytes: {}B]", b.len()))
                    }
                    None => out.push("[Audio]".to_string()),
                },
                Segment::Mention(m) => out.push(format!("@{}", m.target_user_id)),
                Segment::Reply(r) => {
                    out.push(format!("[Reply to {}: {}]", r.target_message_id, r.snippet))
                }
                Segment::Custom(c) => out.push(format!("[Custom: {}]", c.type_name)),
            }
        }
    }
    out.join("")
}

/// Converts a `serde_json::Value` into `prost_types::Struct`.
fn json_to_prost_struct(val: &serde_json::Value) -> Option<prost_types::Struct> {
    let map = val.as_object()?;
    let mut fields = std::collections::BTreeMap::new();
    for (k, v) in map {
        fields.insert(k.clone(), json_to_prost_value(v));
    }
    Some(prost_types::Struct { fields })
}

/// Converts a single `serde_json::Value` into `prost_types::Value`.
fn json_to_prost_value(val: &serde_json::Value) -> prost_types::Value {
    let kind = match val {
        serde_json::Value::Null => Some(prost_types::value::Kind::NullValue(0)),
        serde_json::Value::Bool(b) => Some(prost_types::value::Kind::BoolValue(*b)),
        serde_json::Value::Number(n) => Some(prost_types::value::Kind::NumberValue(
            n.as_f64().unwrap_or(0.0),
        )),
        serde_json::Value::String(s) => Some(prost_types::value::Kind::StringValue(s.clone())),
        serde_json::Value::Array(arr) => {
            let values = arr.iter().map(json_to_prost_value).collect();
            Some(prost_types::value::Kind::ListValue(
                prost_types::ListValue { values },
            ))
        }
        serde_json::Value::Object(map) => {
            let mut fields = std::collections::BTreeMap::new();
            for (k, v) in map {
                fields.insert(k.clone(), json_to_prost_value(v));
            }
            Some(prost_types::value::Kind::StructValue(prost_types::Struct {
                fields,
            }))
        }
    };
    prost_types::Value { kind }
}

/// Runs the offline sandbox test driver for a target plugin.
pub async fn run_sandbox(path: &Path, opts: SandboxOptions) -> Result<(), SandboxError> {
    let (manifest_path, _root) = find_manifest_path(path)?;

    println!("============================================================");
    println!(" Kanon Offline Sandbox Runner");
    println!(" Manifest: {}", manifest_path.display());
    println!("============================================================");

    // 1. Create isolated temporary directory for runtime IPC sockets
    let temp_dir = tempdir()?;
    let run_dir = temp_dir.path().to_path_buf();
    let core_sock = run_dir.join("core.sock");

    // 2. Initialize Supervisor with the isolated runtime directory
    let supervisor = Arc::new(Supervisor::new(
        Some(run_dir.clone()),
        Some(core_sock.clone()),
    ));

    // 3. Start Core IPC Server wired with Supervisor
    let (event_tx, _event_rx) = mpsc::channel(DEFAULT_INGEST_QUEUE_CAPACITY);
    let core_api = CoreApiService::new(event_tx).with_supervisor(supervisor.clone());
    let core_server = CoreIpcServer::new(&core_sock, core_api);

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server_task = tokio::spawn(async move {
        let _ = core_server
            .run(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    println!("Spawning plugin host process and completing handshake...");
    let host = supervisor.spawn_from_manifest(&manifest_path, None).await?;

    println!("\n[Host Connected] ID: {}", host.host_id);
    for meta in host.metas() {
        println!(
            "Loaded Plugin: {} ({}) v{}",
            meta.name, meta.id, meta.version
        );
        if !meta.commands.is_empty() {
            println!("Declared Commands:");
            for cmd in &meta.commands {
                println!(
                    "  - /{:<15} Usage: {:<20} ({})",
                    cmd.name, cmd.usage, cmd.description
                );
            }
        }
        if !meta.tools.is_empty() {
            println!("Declared Tools (LLM Function Calling):");
            for tool in &meta.tools {
                println!("  - {:<16} {}", tool.name, tool.description);
            }
        }
    }
    println!("============================================================\n");

    // 4. Non-interactive direct execution mode
    if let Some(ref cmd_name) = opts.command {
        execute_sandbox_command(&host, cmd_name, &opts.args).await?;
        let _ = supervisor.stop_all().await;
        let _ = shutdown_tx.send(());
        let _ = server_task.await;
        return Ok(());
    }

    if let Some(ref tool_name) = opts.tool {
        execute_sandbox_tool(&host, tool_name, &opts.args).await?;
        let _ = supervisor.stop_all().await;
        let _ = shutdown_tx.send(());
        let _ = server_task.await;
        return Ok(());
    }

    if opts.non_interactive {
        println!("Non-interactive probe completed successfully.");
        let _ = supervisor.stop_all().await;
        let _ = shutdown_tx.send(());
        let _ = server_task.await;
        return Ok(());
    }

    // 5. Interactive terminal REPL mode
    println!("Entering interactive terminal sandbox.");
    println!("Commands:");
    println!("  /<cmd> [args...]       Execute a declared slash command");
    println!("  call <tool> [json]     Invoke an LLM Tool with JSON arguments");
    println!("  help                   Display available commands and tools");
    println!("  exit / quit            Shut down the sandbox and exit\n");

    let stdin = tokio::io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    loop {
        print!("kanon-sandbox> ");
        let _ = std::io::Write::flush(&mut std::io::stdout());

        match lines.next_line().await {
            Ok(Some(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                if trimmed == "exit" || trimmed == "quit" || trimmed == "/exit" {
                    println!("Exiting sandbox...");
                    break;
                }

                if trimmed == "help" || trimmed == "/help" {
                    for meta in host.metas() {
                        println!("Plugin: {}", meta.name);
                        for c in &meta.commands {
                            println!("  Command: /{} - {}", c.name, c.description);
                        }
                        for t in &meta.tools {
                            println!("  Tool: {} - {}", t.name, t.description);
                        }
                    }
                    continue;
                }

                if trimmed.starts_with('/') {
                    let parts: Vec<&str> = trimmed.split_whitespace().collect();
                    let cmd_name = parts[0].trim_start_matches('/');
                    let args: Vec<String> = parts.iter().skip(1).map(|s| s.to_string()).collect();

                    if let Err(e) = execute_sandbox_command(&host, cmd_name, &args).await {
                        println!("Error: {}", e);
                    }
                } else if trimmed.starts_with("call ") {
                    let rest = trimmed.trim_start_matches("call ").trim();
                    let mut parts = rest.splitn(2, ' ');
                    let tool_name = parts.next().unwrap_or_default();
                    let json_str = parts.next().unwrap_or("{}");
                    let args = vec![json_str.to_string()];

                    if let Err(e) = execute_sandbox_tool(&host, tool_name, &args).await {
                        println!("Error: {}", e);
                    }
                } else {
                    println!(
                        "Unrecognized input. Start commands with '/' or invoke tools with 'call <name> <json>'. Type 'exit' to quit."
                    );
                }
            }
            Ok(None) => break, // EOF reached (e.g. piped stdin)
            Err(e) => {
                println!("Error reading input: {}", e);
                break;
            }
        }
    }

    // Graceful teardown
    let _ = supervisor.stop_all().await;
    let _ = shutdown_tx.send(());
    let _ = server_task.await;
    println!("Sandbox terminated.");

    Ok(())
}

/// Executes a slash command on the managed host and renders results.
async fn execute_sandbox_command(
    host: &ManagedHost,
    command: &str,
    args: &[String],
) -> Result<CommandExecuteResponse, SandboxError> {
    let clean_cmd = command.trim_start_matches('/');
    let plugin_id = host
        .metas()
        .iter()
        .find(|m| m.commands.iter().any(|c| c.name == clean_cmd))
        .map(|m| m.id.clone())
        .ok_or_else(|| SandboxError::CommandNotFound(clean_cmd.to_string()))?;

    let req = CommandExecuteRequest {
        plugin_id,
        command: clean_cmd.to_string(),
        args: args.to_vec(),
        context: None,
        ..Default::default()
    };

    println!("[Executing] /{} with args: {:?}", clean_cmd, args);
    let start = std::time::Instant::now();
    let resp = host.execute_command(req).await?;
    let elapsed = start.elapsed();

    println!(
        "[Response] (Status: {}, RTT: {:.2}ms)",
        if resp.success { "SUCCESS" } else { "FAILED" },
        elapsed.as_secs_f64() * 1000.0
    );
    if !resp.replies.is_empty() {
        println!("Replies:");
        let rendered = format_segments(&resp.replies);
        println!("  {}", rendered);
    }
    if !resp.error_message.is_empty() {
        println!("Error Message: {}", resp.error_message);
    }
    println!();

    Ok(resp)
}

/// Invokes a declared tool on the managed host and renders results.
async fn execute_sandbox_tool(
    host: &ManagedHost,
    tool_name: &str,
    args: &[String],
) -> Result<ToolCallResponse, SandboxError> {
    let _has_tool = host
        .metas()
        .iter()
        .any(|m| m.tools.iter().any(|t| t.name == tool_name));

    if !_has_tool {
        return Err(SandboxError::ToolNotFound(tool_name.to_string()));
    }

    let json_arg = args.join(" ");
    let parsed_json: serde_json::Value = if json_arg.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&json_arg)?
    };

    let structured_payload = json_to_prost_struct(&parsed_json);
    let req = ToolCallRequest {
        call_id: format!(
            "test_call_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ),
        tool_name: tool_name.to_string(),
        session_id: "sandbox_session".to_string(),
        payload: structured_payload.map(tool_call_request::Payload::StructuredArgs),
    };

    println!(
        "[Invoking Tool] '{}' with payload: {}",
        tool_name, parsed_json
    );
    let start = std::time::Instant::now();
    let resp = host.on_call_tool(req).await?;
    let elapsed = start.elapsed();

    println!(
        "[Response] (Status: {}, RTT: {:.2}ms)",
        if resp.success { "SUCCESS" } else { "FAILED" },
        elapsed.as_secs_f64() * 1000.0
    );
    if let Some(ref payload) = resp.payload {
        match payload {
            tool_call_response::Payload::StructuredResult(s) => {
                let json_val = kanon_llm::tool_router::prost_struct_to_json(s.clone());
                println!(
                    "Result: {}",
                    serde_json::to_string_pretty(&json_val).unwrap_or_default()
                );
            }
            tool_call_response::Payload::RawBytes(bytes) => {
                println!(
                    "Raw bytes (len {}): {}",
                    bytes.len(),
                    String::from_utf8_lossy(bytes)
                );
            }
        }
    }
    if !resp.error_message.is_empty() {
        println!("Error: {}", resp.error_message);
    }
    println!();

    Ok(resp)
}
