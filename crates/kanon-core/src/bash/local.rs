//! Native host execution with bounded output and process-group cleanup.

use super::MAX_BASH_OUTPUT_BYTES;
use std::path::{Path, PathBuf};

pub(super) fn bash_executable() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let paths = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&paths)
            .map(|directory| directory.join("bash"))
            .chain([PathBuf::from("/bin/bash"), PathBuf::from("/usr/bin/bash")])
            .find(|path| {
                std::fs::metadata(path)
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(unix)]
pub(super) async fn execute(command: &str, cwd: &Path, timeout: u64) -> Result<String, String> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    // Clear provider credentials, BASH_ENV and exported functions, while preserving the OS PATH
    // and home directory so ordinary installed interpreters, venvs and development tools work.
    let bash = bash_executable().ok_or("Bash is unavailable on this host")?;
    let mut child = tokio::process::Command::new(bash)
        .args(["--noprofile", "--norc", "-o", "pipefail", "-c", command])
        .current_dir(cwd)
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        )
        .env(
            "HOME",
            std::env::var_os("HOME").unwrap_or_else(|| cwd.as_os_str().to_owned()),
        )
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()
        .map_err(|err| format!("Failed to start Bash: {err}"))?;
    let group = ProcessGroup(child.id().ok_or("Bash started without a process id")? as i32);
    let mut stdout = child.stdout.take().ok_or("Missing Bash stdout")?;
    let mut stderr = child.stderr.take().ok_or("Missing Bash stderr")?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    async fn drain(
        stream: &mut (impl tokio::io::AsyncRead + Unpin),
        data: &mut Vec<u8>,
    ) -> std::io::Result<bool> {
        let mut buffer = [0; 8192];
        let mut truncated = false;
        loop {
            let n = stream.read(&mut buffer).await?;
            if n == 0 {
                return Ok(truncated);
            }
            let retain = n.min(MAX_BASH_OUTPUT_BYTES.saturating_sub(data.len()));
            data.extend_from_slice(&buffer[..retain]);
            truncated |= retain < n;
        }
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout), async {
        tokio::try_join!(
            child.wait(),
            drain(&mut stdout, &mut out),
            drain(&mut stderr, &mut err)
        )
    })
    .await;
    // Kill the entire pipeline on timeout, cancellation or completion, including descendants that
    // still hold output pipes. The RAII guard also runs if this future is dropped by its caller.
    drop(group);
    let (code, timed_out, out_truncated, err_truncated) = match result {
        Ok(Ok((status, out_cut, err_cut))) => (status.code(), false, out_cut, err_cut),
        Ok(Err(error)) => return Err(format!("Bash output/wait failed: {error}")),
        Err(_) => {
            child
                .wait()
                .await
                .map_err(|error| format!("Failed to reap timed-out Bash: {error}"))?;
            (
                None,
                true,
                out.len() == MAX_BASH_OUTPUT_BYTES,
                err.len() == MAX_BASH_OUTPUT_BYTES,
            )
        }
    };
    let output = serde_json::json!({
        "execution_mode": "local", "sandbox": false, "stdout": String::from_utf8_lossy(&out), "stderr": String::from_utf8_lossy(&err),
        "exit_code": code, "timed_out": timed_out,
        "stdout_truncated": out_truncated, "stderr_truncated": err_truncated
    })
    .to_string();
    if !timed_out && code == Some(0) {
        Ok(output)
    } else {
        Err(output)
    }
}

#[cfg(unix)]
struct ProcessGroup(i32);

#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SAFETY: the child created its own positive process group; a negative pid targets only
        // that group. ESRCH means it already exited and needs no cleanup.
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}
