//! Isolation tests against the real prepared Docker runtime. No host shell fallback is used.

use kanon_core::{
    BashPolicy, BashPolicyStore, BashSandboxConfig, BashTool, CommandPolicy, CommandPolicyStore,
    with_bash_caller,
};
use kanon_llm::AgentTool;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// The administrator every sandbox call runs as.
const ADMIN: &str = "test:sandbox-user";

fn admins() -> Arc<CommandPolicyStore> {
    Arc::new(CommandPolicyStore::new(CommandPolicy {
        admins: vec![ADMIN.into()],
        ..Default::default()
    }))
}
fn policy() -> Arc<BashPolicyStore> {
    Arc::new(BashPolicyStore::new(BashPolicy {
        enabled: true,
        ..Default::default()
    }))
}
async fn invoke(tool: &BashTool, command: &str, seconds: u64) -> Result<Value, Value> {
    let result = with_bash_caller(
        Some(ADMIN.into()),
        tool.call(
            "sandbox-test",
            json!({"command":command, "timeout_seconds":seconds}),
        ),
    )
    .await;
    match result {
        Ok(text) => Ok(serde_json::from_str(&text)
            .unwrap_or_else(|_| panic!("invalid sandbox result: {text}"))),
        Err(text) => Err(serde_json::from_str(&text).unwrap_or(json!({"error":text}))),
    }
}

#[test]
fn defaults_allow_public_network_and_reject_remote_or_unbounded_configuration() {
    let defaults = BashSandboxConfig::default();
    assert!(defaults.network);
    defaults.validate().unwrap();
    for config in [
        BashSandboxConfig {
            endpoint: "tcp://remote:2375".into(),
            ..defaults.clone()
        },
        BashSandboxConfig {
            memory_mb: 0,
            ..defaults.clone()
        },
        BashSandboxConfig {
            cpus: f64::NAN,
            ..defaults.clone()
        },
        BashSandboxConfig {
            pids_limit: 0,
            ..defaults.clone()
        },
        BashSandboxConfig {
            image: "--privileged".into(),
            ..defaults.clone()
        },
    ] {
        assert!(config.validate().is_err());
    }
}

#[tokio::test]
async fn unavailable_sandbox_never_executes_the_command_on_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let policy = policy();
    let mut config = policy.get();
    config.sandbox.endpoint = format!(
        "unix://{}",
        dir.path().join("missing-docker.sock").display()
    );
    policy.set(config);
    let tool = BashTool::new(dir.path(), policy, admins()).unwrap();
    let result = invoke(&tool, "echo unsafe > host-marker", 5)
        .await
        .unwrap_err();
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("Docker sandbox unavailable"),
        "{result}"
    );
    assert!(!dir.path().join("host-marker").exists());
}

#[tokio::test]
#[ignore = "requires local Docker and the sandbox/bash runtime image"]
async fn interpreter_escape_attempts_cannot_read_or_modify_host_files() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let secret = dir.path().join("host-secret");
    std::fs::write(&secret, "host secret must remain private").unwrap();
    let tool = BashTool::new(&workspace, policy(), admins()).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&secret, workspace.join("escape-link")).unwrap();
    let script = format!(
        r#"python3 - <<'PY'
import json, os, pathlib, resource, socket
result = {{'uid': os.getuid(), 'host_socket': pathlib.Path('/var/run/docker.sock').exists()}}
status = pathlib.Path('/proc/self/status').read_text()
result['caps'] = next(line.split()[1] for line in status.splitlines() if line.startswith('CapEff:'))
result['no_new_privs'] = next(line.split()[1] for line in status.splitlines() if line.startswith('NoNewPrivs:'))
for name, path, write in [('host_read', {secret:?}, False), ('link_read', '/workspace/escape-link', False), ('system_write', '/etc/kanon-sandbox-escape', True)]:
    try:
        if write: pathlib.Path(path).write_text('unsafe')
        else: pathlib.Path(path).read_text()
        result[name] = 'escaped'
    except OSError:
        result[name] = 'denied'
pathlib.Path('/workspace/allowed').write_text('sandbox output')
result['file_limit'] = resource.getrlimit(resource.RLIMIT_FSIZE)[1]
print(json.dumps(result))
PY"#,
        secret = secret.to_str().unwrap()
    );
    let result = invoke(&tool, &script, 10).await.unwrap();
    let facts: Value = serde_json::from_str(result["stdout"].as_str().unwrap()).unwrap();
    assert_ne!(facts["uid"], 0);
    assert_eq!(facts["caps"], "0000000000000000");
    assert_eq!(facts["no_new_privs"], "1");
    assert_eq!(facts["host_socket"], false);
    for key in ["host_read", "link_read", "system_write"] {
        assert_eq!(facts[key], "denied", "{facts}");
    }
    assert_eq!(facts["file_limit"], 512_i64 * 1024 * 1024);
    assert_eq!(
        std::fs::read_to_string(&secret).unwrap(),
        "host secret must remain private"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("allowed")).unwrap(),
        "sandbox output"
    );
    tool.reset_sandbox().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local Docker and the sandbox/bash runtime image"]
async fn public_network_works_and_private_metadata_routes_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = BashTool::new(dir.path(), policy(), admins()).unwrap();
    let script = r#"python3 - <<'PY'
import json, socket, urllib.request
result = {'public': urllib.request.urlopen('https://example.com', timeout=5).status}
for name, host in [('host', 'host.docker.internal'), ('metadata', '169.254.169.254')]:
    try:
        socket.create_connection((host, 80), timeout=1).close()
        result[name] = 'reachable'
    except OSError:
        result[name] = 'blocked'
print(json.dumps(result))
PY"#;
    let result = invoke(&tool, script, 10).await.unwrap();
    let facts: Value = serde_json::from_str(result["stdout"].as_str().unwrap()).unwrap();
    assert_eq!(facts["public"], 200);
    assert_eq!(facts["host"], "blocked");
    assert_eq!(facts["metadata"], "blocked");
    tool.reset_sandbox().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local Docker and the sandbox/bash runtime image"]
async fn operator_can_disable_network_and_model_cannot_override_it() {
    let dir = tempfile::tempdir().unwrap();
    let policy = policy();
    let mut settings = policy.get();
    settings.sandbox.network = false;
    policy.set(settings);
    let tool = BashTool::new(dir.path(), policy, admins()).unwrap();
    let result = invoke(
        &tool,
        "python3 -c \"import socket; socket.create_connection(('1.1.1.1',443),timeout=1)\"",
        5,
    )
    .await
    .unwrap_err();
    assert_ne!(result["exit_code"], 0);
    let override_attempt = with_bash_caller(
        Some(ADMIN.into()),
        tool.call("test", json!({"command":"true", "network":true})),
    )
    .await
    .unwrap_err();
    assert!(override_attempt.contains("Invalid Bash arguments"));
    tool.reset_sandbox().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local Docker and the sandbox/bash runtime image"]
async fn cancellation_restarts_the_container_and_kills_detached_children() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let tool = Arc::new(BashTool::new(&workspace, policy(), admins()).unwrap());
    let task = tokio::spawn({
        let tool = tool.clone();
        async move {
            invoke(&tool, "python3 -c \"import subprocess,time,pathlib; subprocess.Popen(['python3','-c','import time,pathlib; time.sleep(2); pathlib.Path(\\\"/workspace/leaked\\\").write_text(\\\"unsafe\\\")'],start_new_session=True); pathlib.Path('/workspace/started').write_text('ready'); time.sleep(30)\"", 30).await
        }
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while !workspace.join("started").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        !workspace.join("leaked").exists(),
        "setsid child must die with the container"
    );
    tool.reset_sandbox().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local Docker and the sandbox/bash runtime image"]
async fn memory_cpu_pid_and_file_limits_are_enforced_by_the_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let policy = policy();
    let mut settings = policy.get();
    settings.sandbox.memory_mb = 128;
    settings.sandbox.cpus = 0.5;
    settings.sandbox.pids_limit = 64;
    settings.sandbox.file_size_mb = 16;
    policy.set(settings);
    let tool = BashTool::new(dir.path(), policy, admins()).unwrap();
    let script = "python3 -c \"import json,pathlib,resource; p=pathlib.Path('/sys/fs/cgroup'); print(json.dumps({'memory':p.joinpath('memory.max').read_text().strip(),'pids':p.joinpath('pids.max').read_text().strip(),'cpu':p.joinpath('cpu.max').read_text().strip(),'file':resource.getrlimit(resource.RLIMIT_FSIZE)[1]}))\"";
    let result = invoke(&tool, script, 5).await.unwrap();
    let limits: Value = serde_json::from_str(result["stdout"].as_str().unwrap()).unwrap();
    assert_eq!(limits["memory"], (128 * 1024 * 1024).to_string());
    assert_eq!(limits["pids"], "64");
    let cpu: Vec<u64> = limits["cpu"]
        .as_str()
        .unwrap()
        .split_whitespace()
        .map(|value| value.parse().unwrap())
        .collect();
    assert_eq!(cpu[0] * 2, cpu[1]);
    assert_eq!(limits["file"], 16 * 1024 * 1024);
    let oom = invoke(&tool, "python3 -c \"value=bytearray(300*1024*1024)\"", 5)
        .await
        .unwrap_err();
    // Docker's container OOM flag can stay false when only the exec child is killed.
    // The enforced memory ceiling above and SIGKILL exit establish this resource-limit outcome.
    assert_eq!(oom["exit_code"], 137, "{oom}");
    assert_eq!(oom["timed_out"], false, "{oom}");
    tool.reset_sandbox().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local Docker and the sandbox/bash runtime image"]
async fn persistent_container_survives_calls_reconnect_and_background_work_until_reset() {
    let dir = tempfile::tempdir().unwrap();
    let settings = policy();
    let tool = BashTool::new(dir.path(), settings.clone(), admins()).unwrap();
    let first = invoke(&tool, "echo temp > /tmp/persistent-marker; echo home > \"$HOME/persistent-marker\"; python3 -c \"import subprocess; p=subprocess.Popen(['sleep','30'],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True); print(p.pid)\"", 5).await.unwrap();
    let pid: u32 = first["stdout"].as_str().unwrap().trim().parse().unwrap();
    let second = invoke(
        &tool,
        &format!("python3 -c 'import os; os.kill({pid},0)'; cat /tmp/persistent-marker"),
        5,
    )
    .await
    .unwrap();
    assert_eq!(first["container_id"], second["container_id"]);
    assert_eq!(second["container_reused"], true);
    assert_eq!(second["stdout"], "temp\n");
    let package = invoke(&tool, "mkdir -p \"$HOME/.local/bin\"; printf '#!/bin/sh\\nprintf package-ok\\n' > \"$HOME/.local/bin/persistent-cli\"; chmod +x \"$HOME/.local/bin/persistent-cli\"; persistent-cli", 5).await.unwrap();
    assert_eq!(package["stdout"], "package-ok");
    drop(tool);
    let reconnected = BashTool::new(dir.path(), settings.clone(), admins()).unwrap();
    let third = invoke(&reconnected, "cat /tmp/persistent-marker", 5)
        .await
        .unwrap();
    assert_eq!(
        first["container_id"], third["container_id"],
        "a node reconnect must adopt the same managed container"
    );
    let mut changed = settings.get();
    changed.sandbox.network = false;
    settings.set(changed);
    let error = invoke(&reconnected, "true", 5).await.unwrap_err();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("reset the container")
    );
    reconnected.reset_sandbox().await.unwrap();
    let fourth = invoke(
        &reconnected,
        "test ! -e /tmp/persistent-marker; persistent-cli; cat \"$HOME/persistent-marker\"",
        5,
    )
    .await
    .unwrap();
    assert_ne!(first["container_id"], fourth["container_id"]);
    assert_eq!(fourth["stdout"], "package-okhome\n");
    reconnected.reset_sandbox().await.unwrap();
}
