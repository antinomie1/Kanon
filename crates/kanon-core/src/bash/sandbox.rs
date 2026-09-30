//! Persistent Docker execution, with explicit reset and serialized commands per workspace.

use bollard::exec::{StartExecOptions, StartExecResults};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::models::{
    ContainerCreateBody, ExecConfig, HealthConfig, HostConfig, HostConfigCgroupnsModeEnum,
    HostConfigLogConfig, Mount, MountBindOptions, MountTypeEnum, ResourcesUlimits, RestartPolicy,
    RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, RestartContainerOptionsBuilder,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedSemaphorePermit, oneshot};

use super::{BashExecutionMode, BashPolicy, BashPolicyStore, BashPrincipal, MAX_BASH_OUTPUT_BYTES};

/// Independent host directory exposed to the sandbox, never the node's configuration directory.
pub const DEFAULT_BASH_WORKSPACE: &str = "./data/bash/workspace";

/// Operator-owned container settings. Model arguments cannot modify these fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BashSandboxConfig {
    /// Local Docker socket or Windows named pipe; remote daemons are not accepted.
    pub endpoint: String,
    /// Prepared image carrying the Kanon sandbox bootstrap contract.
    pub image: String,
    /// Allow public IPv4 networking; private/host/metadata destinations remain blocked.
    pub network: bool,
    /// Container RAM and swap ceiling, in MiB.
    pub memory_mb: u32,
    /// CPU quota measured in cores.
    pub cpus: f64,
    /// Maximum number of processes/threads in the container.
    pub pids_limit: u32,
    /// Maximum size of an individual output file, in MiB.
    pub file_size_mb: u32,
}

impl Default for BashSandboxConfig {
    fn default() -> Self {
        Self {
            endpoint: if cfg!(windows) {
                "npipe:////./pipe/docker_engine"
            } else {
                "unix:///var/run/docker.sock"
            }
            .into(),
            image: "kanon-bash-sandbox:2".into(),
            network: true,
            memory_mb: 512,
            cpus: 1.0,
            pids_limit: 128,
            file_size_mb: 512,
        }
    }
}

impl BashSandboxConfig {
    /// Validates all operator settings before persistence or daemon access.
    pub fn validate(&self) -> Result<(), String> {
        if !(self.endpoint.starts_with("unix:///")
            || self.endpoint.starts_with("npipe:////./pipe/"))
            || self.endpoint.contains('\0')
        {
            return Err("Sandbox endpoint must be a local unix socket or named pipe".into());
        }
        if self.image.is_empty()
            || self.image.starts_with('-')
            || !self
                .image
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || b"._-/:@".contains(&ch))
        {
            return Err("Invalid sandbox image reference".into());
        }
        if !(64..=8192).contains(&self.memory_mb)
            || !self.cpus.is_finite()
            || !(0.1..=8.0).contains(&self.cpus)
            || !(16..=512).contains(&self.pids_limit)
            || !(1..=8192).contains(&self.file_size_mb)
        {
            return Err("Invalid sandbox resource limits".into());
        }
        Ok(())
    }
}

/// Checks the local daemon and prepared image, returning an immutable image id for execution.
pub(super) async fn probe(config: &BashSandboxConfig) -> Result<(Docker, String), String> {
    let docker = connect(config).await?;
    let info = docker
        .info()
        .await
        .map_err(|err| format!("Docker sandbox unavailable: {err}"))?;
    if info.os_type.as_deref() != Some("linux")
        || info.memory_limit != Some(true)
        || info.swap_limit != Some(true)
        || info.cpu_cfs_quota != Some(true)
        || info.pids_limit != Some(true)
        || !info
            .security_options
            .as_ref()
            .is_some_and(|options| options.iter().any(|option| option.contains("seccomp")))
    {
        return Err(
            "Sandbox requires a Linux Docker daemon with memory/CPU/PID limits and seccomp".into(),
        );
    }
    let image = docker
        .inspect_image(&config.image)
        .await
        .map_err(|err| format!("Sandbox image unavailable; build sandbox/bash first: {err}"))?;
    let image_config = image
        .config
        .ok_or("Sandbox image has no runtime configuration")?;
    if image.os.as_deref() != Some("linux")
        || image_config
            .labels
            .as_ref()
            .and_then(|labels| labels.get("org.kanon.bash-sandbox.version"))
            .map(String::as_str)
            != Some("2")
        || image_config
            .volumes
            .as_ref()
            .is_some_and(|volumes| !volumes.is_empty())
    {
        return Err("Image does not satisfy the Kanon sandbox runtime contract".into());
    }
    Ok((docker, image.id.ok_or("Sandbox image has no immutable id")?))
}

async fn connect(config: &BashSandboxConfig) -> Result<Docker, String> {
    config.validate()?;
    #[cfg(unix)]
    let docker = Docker::connect_with_unix(
        config
            .endpoint
            .strip_prefix("unix://")
            .ok_or("This host requires a Unix Docker socket")?,
        10,
        bollard::API_DEFAULT_VERSION,
    );
    #[cfg(windows)]
    let docker =
        Docker::connect_with_named_pipe(config.endpoint.as_str(), 10, bollard::API_DEFAULT_VERSION);
    #[cfg(not(any(unix, windows)))]
    return Err("Docker sandbox is unsupported on this host".into());
    let docker = docker
        .map_err(|err| format!("Docker sandbox unavailable: {err}"))?
        .negotiate_version()
        .await
        .map_err(|err| format!("Docker sandbox unavailable: {err}"))?;
    Ok(docker)
}

/// Chooses a non-root execution identity matching the node's workspace ownership when possible.
pub(super) fn identity() -> (u32, u32) {
    #[cfg(unix)]
    {
        // SAFETY: these identity queries have no pointers or side effects.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        if uid != 0 {
            return (uid, gid);
        }
    }
    (65534, 65534)
}

/// Immutable invocation captured after authorization and argument validation.
pub(super) struct Invocation {
    pub root: PathBuf,
    pub cwd: PathBuf,
    pub command: String,
    pub seconds: u64,
    pub policy: Arc<BashPolicyStore>,
    pub caller: BashPrincipal,
    pub expected: BashPolicy,
}

/// One persistent container per workspace. Commands and resets share one lifecycle lock.
#[derive(Default)]
pub(super) struct SandboxRuntime {
    gate: Arc<Mutex<()>>,
}

impl SandboxRuntime {
    pub(super) fn lock_policy_update(&self) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
        self.gate.clone().try_lock_owned().map_err(|_| {
            "Sandbox is busy; retry the endpoint change after execution finishes".into()
        })
    }

    pub(super) async fn require_reset_before_endpoint_change(
        &self,
        root: &Path,
        current: &BashSandboxConfig,
    ) -> Result<(), String> {
        let docker = connect(current).await?;
        match docker.inspect_container(&container_name(root), None).await {
            Ok(_) => Err("Reset the existing sandbox before changing its Docker endpoint".into()),
            Err(error) if not_found(&error) => Ok(()),
            Err(error) => Err(format!("Cannot verify the old Docker endpoint: {error}")),
        }
    }

    pub(super) async fn execute(
        &self,
        call: Invocation,
        slot: OwnedSemaphorePermit,
    ) -> Result<String, String> {
        let (sender, mut cancel) = oneshot::channel();
        let _cancel = CancelOnDrop(Some(sender));
        let gate = self.gate.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let _guard = tokio::select! {
                guard = gate.lock_owned() => guard,
                _ = &mut cancel => return Err("Sandbox execution cancelled".into()),
            };
            run(call, cancel).await
        })
        .await
        .map_err(|error| format!("Sandbox worker failed: {error}"))?
    }

    pub(super) async fn reset(
        &self,
        root: &Path,
        config: &BashSandboxConfig,
    ) -> Result<(), String> {
        let _guard = self
            .gate
            .try_lock()
            .map_err(|_| "Sandbox is executing a command; retry reset after it finishes")?;
        let docker = connect(config).await?;
        let name = container_name(root);
        match docker.inspect_container(&name, None).await {
            Ok(info) => {
                verify_owner(
                    info.config
                        .as_ref()
                        .and_then(|config| config.labels.as_ref()),
                    root,
                )?;
                docker
                    .remove_container(
                        &name,
                        Some(
                            RemoveContainerOptionsBuilder::default()
                                .force(true)
                                .v(true)
                                .build(),
                        ),
                    )
                    .await
                    .map_err(|error| error.to_string())
            }
            Err(error) if not_found(&error) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

struct CancelOnDrop(Option<oneshot::Sender<()>>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

fn cancelled(cancel: &mut oneshot::Receiver<()>) -> bool {
    !matches!(cancel.try_recv(), Err(oneshot::error::TryRecvError::Empty))
}

fn authorize(call: &Invocation) -> Result<(), String> {
    let current = call.policy.get();
    if current.execution_mode != BashExecutionMode::Sandbox
        || current != call.expected
        || !current.allows(Some(&call.caller))
    {
        return Err("Bash execution denied: permission or configuration changed".into());
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn workspace_key(root: &Path) -> String {
    digest(root.as_os_str().as_encoded_bytes())
}
fn container_name(root: &Path) -> String {
    format!("kanon-bash-{}", &workspace_key(root)[..32])
}
fn not_found(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}
fn verify_owner(labels: Option<&HashMap<String, String>>, root: &Path) -> Result<(), String> {
    if labels.and_then(|labels| labels.get("org.kanon.bash-sandbox.workspace"))
        != Some(&workspace_key(root))
        || labels
            .and_then(|labels| labels.get("org.kanon.bash-sandbox.managed"))
            .map(String::as_str)
            != Some("2")
    {
        return Err(
            "Container name is occupied by a different owner; refusing to use or remove it".into(),
        );
    }
    Ok(())
}

async fn ready(docker: &Docker, id: &str) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let info = docker
                .inspect_container(id, None)
                .await
                .map_err(|error| error.to_string())?;
            let state = info.state.ok_or("Container has no state")?;
            if state.running != Some(true) {
                return Err("Sandbox initialization stopped; inspect its Docker logs".into());
            }
            if state
                .health
                .and_then(|health| health.status)
                .is_some_and(|status| status.to_string() == "healthy")
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| "Sandbox initialization timed out")?
}

async fn ensure_container(
    docker: &Docker,
    call: &Invocation,
    image: String,
) -> Result<(String, bool), String> {
    let name = container_name(&call.root);
    let fingerprint = digest(
        &serde_json::to_vec(&(&call.expected.sandbox, &image, identity()))
            .map_err(|error| error.to_string())?,
    );
    let mut reused = true;
    match docker.inspect_container(&name, None).await {
        Ok(_) => {}
        Err(error) if not_found(&error) => {
            let mut body = container_body(&call.root, &call.expected.sandbox, image)?;
            body.labels.as_mut().unwrap().insert(
                "org.kanon.bash-sandbox.workspace".into(),
                workspace_key(&call.root),
            );
            body.labels.as_mut().unwrap().insert(
                "org.kanon.bash-sandbox.fingerprint".into(),
                fingerprint.clone(),
            );
            match docker
                .create_container(
                    Some(CreateContainerOptionsBuilder::default().name(&name).build()),
                    body,
                )
                .await
            {
                Ok(_) => reused = false,
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 409, ..
                }) => {}
                Err(error) => return Err(format!("Failed to create persistent sandbox: {error}")),
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    let info = docker
        .inspect_container(&name, None)
        .await
        .map_err(|error| error.to_string())?;
    let labels = info
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref());
    verify_owner(labels, &call.root)?;
    if labels.and_then(|labels| labels.get("org.kanon.bash-sandbox.fingerprint"))
        != Some(&fingerprint)
    {
        return Err("Sandbox image or isolation settings changed; reset the container in Tools to apply them".into());
    }
    if info.state.as_ref().and_then(|state| state.running) != Some(true) {
        docker
            .start_container(&name, None)
            .await
            .map_err(|error| error.to_string())?;
    }
    ready(docker, &name).await?;
    Ok((info.id.ok_or("Sandbox has no container id")?, reused))
}

async fn run(call: Invocation, mut cancel: oneshot::Receiver<()>) -> Result<String, String> {
    authorize(&call)?;
    let (docker, image) = probe(&call.expected.sandbox).await?;
    if cancelled(&mut cancel) {
        return Err("Sandbox execution cancelled".into());
    }
    let (id, reused) = ensure_container(&docker, &call, image).await?;
    if cancelled(&mut cancel) {
        return Err("Sandbox execution cancelled".into());
    }
    authorize(&call)?;
    let relative = call
        .cwd
        .strip_prefix(&call.root)
        .map_err(|error| error.to_string())?;
    let cwd = format!(
        "/workspace/{}",
        relative
            .components()
            .map(|part| part.as_os_str().to_str().ok_or("Invalid cwd encoding"))
            .collect::<Result<Vec<_>, _>>()?
            .join("/")
    );
    let (uid, gid) = identity();
    let exec = docker
        .create_exec(
            &id,
            ExecConfig {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                attach_stdin: Some(false),
                tty: Some(false),
                privileged: Some(false),
                user: Some("0:0".into()),
                working_dir: Some(cwd),
                cmd: Some(vec![
                    "/usr/local/libexec/kanon-sandbox-exec".into(),
                    uid.to_string(),
                    gid.to_string(),
                    call.seconds.to_string(),
                    call.command,
                ]),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    if cancelled(&mut cancel) {
        return Err("Sandbox execution cancelled".into());
    }
    // The container persists, but each exec still requires fresh sender authorization.
    let current = call.policy.get();
    if current != call.expected || !current.allows(Some(&call.caller)) {
        return Err("Bash execution denied: permission or configuration changed".into());
    }
    let mut capture = Capture::default();
    let started = std::time::Instant::now();
    let execution = async {
        match docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    tty: false,
                    output_capacity: Some(8192),
                }),
            )
            .await
            .map_err(|error| error.to_string())?
        {
            StartExecResults::Attached { mut output, .. } => {
                while let Some(frame) = output.next().await {
                    match frame.map_err(|error| error.to_string())? {
                        LogOutput::StdOut { message } | LogOutput::Console { message } => {
                            capture.append(true, &message)
                        }
                        LogOutput::StdErr { message } => capture.append(false, &message),
                        LogOutput::StdIn { .. } => {}
                    }
                }
            }
            StartExecResults::Detached => return Err("Unexpected detached command stream".into()),
        }
        loop {
            let state = docker
                .inspect_exec(&exec.id)
                .await
                .map_err(|error| error.to_string())?;
            if state.running != Some(true) {
                return state
                    .exit_code
                    .ok_or("Command returned no exit code".into());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let outcome = tokio::select! {
        _ = &mut cancel => Err("Sandbox execution cancelled".to_string()),
        result = tokio::time::timeout(Duration::from_secs(call.seconds + 3), execution) => result.map_err(|_| "Sandbox execution timed out".to_string()).and_then(|result| result),
    };
    let code = outcome.as_ref().ok().copied();
    let timed_out = outcome
        .as_ref()
        .err()
        .is_some_and(|error| error.contains("timed out"))
        || matches!(code, Some(124 | 137))
            && started.elapsed() >= Duration::from_millis(call.seconds * 1000 - 200);
    let oom = docker
        .inspect_container(&id, None)
        .await
        .map_err(|error| error.to_string())?
        .state
        .and_then(|state| state.oom_killed)
        .unwrap_or(false);
    // Restart only on interruption/abnormal termination. This kills escaped/detached children,
    // preserves the container id and mounted files, and never interrupts another serialized call.
    if outcome.is_err() || timed_out || code == Some(137) || oom {
        docker
            .restart_container(
                &id,
                Some(RestartContainerOptionsBuilder::default().t(1).build()),
            )
            .await
            .map_err(|error| format!("Failed to restart interrupted sandbox: {error}"))?;
        ready(&docker, &id).await?;
    }
    if let Err(error) = outcome
        && !timed_out
    {
        return Err(error);
    }
    let result = serde_json::json!({"stdout":String::from_utf8_lossy(&capture.stdout), "stderr":String::from_utf8_lossy(&capture.stderr),
        "exit_code":code, "timed_out":timed_out, "oom_killed":oom, "stdout_truncated":capture.stdout_truncated,
        "stderr_truncated":capture.stderr_truncated, "sandbox":true, "execution_mode":"sandbox", "workspace":"/workspace", "container_id":id, "container_reused":reused}).to_string();
    if code == Some(0) && !timed_out && !oom {
        Ok(result)
    } else {
        Err(result)
    }
}

fn container_body(
    root: &Path,
    config: &BashSandboxConfig,
    image: String,
) -> Result<ContainerCreateBody, String> {
    let source = root
        .to_str()
        .ok_or("Sandbox workspace must have a UTF-8 path")?;
    let (uid, gid) = identity();
    let memory = i64::from(config.memory_mb) * 1024 * 1024;
    Ok(ContainerCreateBody {
        image: Some(image),
        user: Some("0:0".into()),
        entrypoint: Some(vec!["/usr/local/libexec/kanon-sandbox-init".into()]),
        cmd: Some(vec![uid.to_string(), gid.to_string()]),
        working_dir: Some("/workspace".into()),
        open_stdin: Some(false),
        tty: Some(false),
        healthcheck: Some(HealthConfig {
            test: Some(vec![
                "CMD".into(),
                "/usr/bin/test".into(),
                "-f".into(),
                "/tmp/kanon-ready".into(),
            ]),
            interval: Some(1_000_000_000),
            timeout: Some(1_000_000_000),
            start_period: Some(3_000_000_000),
            start_interval: Some(100_000_000),
            retries: Some(3),
        }),
        env: Some(vec![
            "HOME=/workspace/.home".into(),
            "LANG=C.UTF-8".into(),
            "BASH_ENV=".into(),
            "ENV=".into(),
            "LD_PRELOAD=".into(),
            "LD_LIBRARY_PATH=".into(),
            "LD_AUDIT=".into(),
        ]),
        labels: Some(HashMap::from([(
            "org.kanon.bash-sandbox.managed".into(),
            "2".into(),
        )])),
        host_config: Some(HostConfig {
            init: Some(true),
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                maximum_retry_count: None,
            }),
            privileged: Some(false),
            readonly_rootfs: Some(true),
            cap_drop: Some(vec!["ALL".into()]),
            // The trusted image bootstrap needs only these setup capabilities. setpriv removes
            // every capability and bounding-set bit before timeout/Bash/Python/Node start.
            cap_add: Some(
                ["NET_ADMIN", "SETUID", "SETGID", "SETPCAP"]
                    .map(str::to_string)
                    .to_vec(),
            ),
            security_opt: Some(vec!["no-new-privileges=true".into()]),
            memory: Some(memory),
            memory_swap: Some(memory),
            nano_cpus: Some((config.cpus * 1e9) as i64),
            pids_limit: Some(i64::from(config.pids_limit)),
            network_mode: Some(if config.network { "bridge" } else { "none" }.into()),
            extra_hosts: Some(vec!["host.docker.internal:host-gateway".into()]),
            ipc_mode: Some("private".into()),
            cgroupns_mode: Some(HostConfigCgroupnsModeEnum::PRIVATE),
            sysctls: Some(HashMap::from([
                ("net.ipv6.conf.all.disable_ipv6".into(), "1".into()),
                ("net.ipv6.conf.default.disable_ipv6".into(), "1".into()),
            ])),
            tmpfs: Some(HashMap::from([(
                "/tmp".into(),
                "rw,nosuid,nodev,size=128m,mode=1777".into(),
            )])),
            mounts: Some(vec![Mount {
                typ: Some(MountTypeEnum::BIND),
                source: Some(source.into()),
                target: Some("/workspace".into()),
                read_only: Some(false),
                bind_options: Some(MountBindOptions {
                    non_recursive: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ulimits: Some(vec![
                ResourcesUlimits {
                    name: Some("nofile".into()),
                    soft: Some(4096),
                    hard: Some(4096),
                },
                ResourcesUlimits {
                    name: Some("fsize".into()),
                    soft: Some(i64::from(config.file_size_mb) * 1024 * 1024),
                    hard: Some(i64::from(config.file_size_mb) * 1024 * 1024),
                },
                ResourcesUlimits {
                    name: Some("core".into()),
                    soft: Some(0),
                    hard: Some(0),
                },
            ]),
            log_config: Some(HostConfigLogConfig {
                typ: Some("local".into()),
                config: Some(HashMap::from([
                    ("max-size".into(), "1m".into()),
                    ("max-file".into(), "1".into()),
                    ("compress".into(), "false".into()),
                ])),
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
}

#[derive(Default)]
struct Capture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}
impl Capture {
    fn append(&mut self, stdout: bool, bytes: &[u8]) {
        let (data, truncated) = if stdout {
            (&mut self.stdout, &mut self.stdout_truncated)
        } else {
            (&mut self.stderr, &mut self.stderr_truncated)
        };
        let retain = bytes
            .len()
            .min(MAX_BASH_OUTPUT_BYTES.saturating_sub(data.len()));
        data.extend_from_slice(&bytes[..retain]);
        *truncated |= retain < bytes.len();
    }
}
