//! Sending files from the Bash working directory to the chat.
//!
//! Bash makes files — a chart, a report, a converted recording — but its result is text, so
//! without this tool the bot could only describe what it made. `send_file` attaches one file from
//! the working directory to the turn's reply, under the same access rules as Bash itself: the
//! directory holds whatever an administrator's commands produced.
//!
//! The file is opened without following symbolic links at any path component, then copied into
//! the attachment directory before the tool returns. Sandboxed code can plant links in the
//! mounted workspace that point at host files. Refusing links while opening, instead of checking
//! a path and opening it afterwards, leaves no window in which a swapped link makes the node read
//! a host file; the copy then pins the delivered bytes to the ones opened, even if the workspace
//! file changes before the reply goes out.

use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use kanon_llm::{AgentTool, ToolAttachment, ToolDefinition, ToolOutput};
use serde::Deserialize;

use super::{BashExecutionMode, access};

/// Largest file `send_file` attaches.
///
/// It matches the largest single upload a built-in adapter makes (QQ's 20 MiB); a bigger file
/// would be copied only to fail at delivery.
pub const MAX_SEND_FILE_BYTES: u64 = 20 * 1024 * 1024;

/// Where the sandbox mounts the workspace; paths the model saw inside the container start here.
const SANDBOX_MOUNT: &str = "/workspace";

/// Native tool that attaches a file from the Bash working directory to the reply.
///
/// Built by [`super::BashTool::send_file_tool`] so it shares Bash's workspace and access gate.
pub struct SendFileTool {
    /// The sandbox workspace on the host, canonical.
    pub(super) root: PathBuf,
    /// The same gate Bash checks, so whoever may run Bash may send what it made, and nobody else.
    pub(super) gate: Arc<access::Gate>,
    /// Where copies are kept until delivery; swept at node start like every tool attachment.
    pub(super) attachment_dir: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
}

#[async_trait]
impl AgentTool for SendFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "send_file".into(),
            description: "Sends one file from the Bash working directory to the current chat with your reply: images arrive as pictures, audio as a voice message, video as a video and anything else as a named file, as far as the platform supports. Create the file with bash first. Available whenever bash is.".into(),
            parameters: serde_json::json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "path": {"type": "string", "description": "File path relative to the Bash working directory, e.g. out/chart.png; in the container, /workspace/out/chart.png also works"}
                }, "required": ["path"]
            }),
        }
    }

    async fn call(
        &self,
        _session_id: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, String> {
        // Authorization comes from the turn's verified sender, never from the arguments.
        self.gate
            .check(access::current_caller().as_ref())
            .await
            .map_err(|reason| format!("Sending files denied: {reason}"))?;
        let args: Arguments = serde_json::from_value(arguments)
            .map_err(|err| format!("Invalid send_file arguments: {err}"))?;
        let policy = self.gate.policy.get();
        let (root, prefix) = match policy.execution_mode {
            BashExecutionMode::Sandbox => (self.root.clone(), PathBuf::from(SANDBOX_MOUNT)),
            BashExecutionMode::Local => {
                let root = Path::new(&policy.local.working_dir)
                    .canonicalize()
                    .map_err(|err| format!("Invalid local working directory: {err}"))?;
                (root.clone(), root)
            }
        };
        let relative = relative_path(&args.path, &prefix)?;
        let attachment_dir = self.attachment_dir.clone();
        // Opening and copying are blocking file I/O on a file of up to 20 MiB.
        tokio::task::spawn_blocking(move || copy_out(&root, &relative, &attachment_dir))
            .await
            .map_err(|err| format!("send_file stopped: {err}"))?
    }
}

/// Turns the requested path into one below the working directory made only of plain names.
///
/// `prefix` is how the model sees the working directory: `/workspace` in the sandbox, the host
/// directory itself for local execution. `..` is refused outright instead of resolved, so the
/// result can never name anything outside the directory.
fn relative_path(requested: &str, prefix: &Path) -> Result<PathBuf, String> {
    let requested = Path::new(requested);
    let below = requested.strip_prefix(prefix).unwrap_or(requested);
    let mut relative = PathBuf::new();
    for component in below.components() {
        match component {
            Component::Normal(name) => relative.push(name),
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "{} is not inside the Bash working directory; give a path relative to it, \
                     without `..`",
                    requested.display()
                ));
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err("path names no file".into());
    }
    Ok(relative)
}

/// Copies the file into the attachment directory and returns it as an attachment.
fn copy_out(root: &Path, relative: &Path, attachment_dir: &Path) -> Result<ToolOutput, String> {
    let shown = relative.display();
    let mut source = open_beneath(root, relative).map_err(|err| open_error(relative, &err))?;
    let meta = source
        .metadata()
        .map_err(|err| format!("cannot inspect {shown}: {err}"))?;
    if !meta.is_file() {
        return Err(format!("{shown} is not a regular file"));
    }
    if meta.len() > MAX_SEND_FILE_BYTES {
        return Err(too_large(relative, meta.len()));
    }
    // The name travels as a string to adapters and is what the recipient sees.
    let name = relative
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| format!("{shown} has a file name that is not valid UTF-8"))?;
    let mime = crate::mcp::mime_for_extension(
        Path::new(name)
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or_default(),
    );

    let destination = crate::mcp::new_attachment_path(attachment_dir, mime, Some(name))?;
    // A copy that failed or overran is removed at once instead of waiting for the startup sweep.
    let discard = |reason: String| {
        if let Some(slot) = destination.parent()
            && let Err(err) = std::fs::remove_dir_all(slot)
        {
            tracing::warn!(path = %slot.display(), error = %err, "Failed to remove an abandoned attachment copy");
        }
        reason
    };
    let mut target = File::create_new(&destination)
        .map_err(|err| discard(format!("cannot create the attachment copy: {err}")))?;
    // One byte past the limit is read so a file that grew after the size check is caught.
    let copied = std::io::copy(
        &mut (&mut source).take(MAX_SEND_FILE_BYTES + 1),
        &mut target,
    )
    .map_err(|err| discard(format!("cannot copy {shown}: {err}")))?;
    drop(target);
    if copied > MAX_SEND_FILE_BYTES {
        return Err(discard(too_large(relative, copied)));
    }

    // The path crosses a process boundary when the adapter is a plugin, so it is handed over
    // absolute: a relative path would resolve against that host's working directory.
    let absolute = destination
        .canonicalize()
        .map_err(|err| discard(format!("cannot resolve the attachment copy: {err}")))?;
    Ok(ToolOutput {
        // No path here: the model needs to know the file is attached, not where the node keeps it.
        text: format!(
            "Attached {name} ({:.1} KiB, {mime}); it is sent with your reply.",
            copied as f64 / 1024.0
        ),
        attachments: vec![ToolAttachment {
            mime_type: mime.to_string(),
            file_path: Some(absolute.to_string_lossy().into_owned()),
            url: None,
        }],
    })
}

fn too_large(relative: &Path, bytes: u64) -> String {
    format!(
        "{} is {:.1} MiB; send_file attaches at most {} MiB",
        relative.display(),
        bytes as f64 / (1024.0 * 1024.0),
        MAX_SEND_FILE_BYTES >> 20
    )
}

/// Explains a failed open in terms of what the model can fix.
fn open_error(relative: &Path, err: &std::io::Error) -> String {
    let shown = relative.display();
    if is_refused_link(err) {
        return format!(
            "{shown} is or passes through a symbolic link, which send_file does not follow; \
             copy the file into the working directory instead"
        );
    }
    match err.kind() {
        std::io::ErrorKind::NotFound => format!("{shown} does not exist"),
        // A linked directory opened with O_NOFOLLOW | O_DIRECTORY fails this way on Linux.
        std::io::ErrorKind::NotADirectory => format!(
            "{shown} passes through a symbolic link or a file where a directory should be; \
             send_file does not follow links"
        ),
        _ => format!("cannot open {shown}: {err}"),
    }
}

/// Whether an open failed because `O_NOFOLLOW` met a symbolic link.
///
/// Linux and macOS report it as `ELOOP`, FreeBSD as `EMLINK`.
#[cfg(unix)]
fn is_refused_link(err: &std::io::Error) -> bool {
    matches!(err.raw_os_error(), Some(libc::ELOOP | libc::EMLINK))
}

/// Links are refused by resolving and comparing paths on this platform, never by errno.
#[cfg(not(unix))]
fn is_refused_link(_err: &std::io::Error) -> bool {
    false
}

/// Opens `relative` below `root` without following a symbolic link at any component.
///
/// Each component is opened relative to the directory descriptor of the one before it, so a
/// directory renamed or replaced by a link mid-walk cannot redirect the walk: the descriptors
/// already held keep pointing where they were opened.
#[cfg(unix)]
fn open_beneath(root: &Path, relative: &Path) -> std::io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    fn open_at(dir: &OwnedFd, name: &OsStr, flags: libc::c_int) -> std::io::Result<OwnedFd> {
        let name = CString::new(name.as_bytes())?;
        // SAFETY: `name` is NUL-terminated and outlives the call, and `dir` is an open directory
        // descriptor borrowed for its duration.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` was just returned by `openat` and nothing else owns it.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    // The root is operator configuration, already resolved by the caller; only what lies below
    // it can have been made by workspace code.
    let mut current = OwnedFd::from(File::open(root)?);
    let mut names = relative.iter().peekable();
    while let Some(name) = names.next() {
        let flags = if names.peek().is_some() {
            libc::O_RDONLY | libc::O_DIRECTORY
        } else {
            // A FIFO planted under the file's name would block a plain open until something
            // writes to it. O_NONBLOCK returns at once; it changes nothing for a regular file.
            libc::O_RDONLY | libc::O_NONBLOCK
        };
        current = open_at(&current, name, flags)?;
    }
    Ok(File::from(current))
}

/// Opens `relative` below `root`, refusing a path that resolves outside it.
///
/// Windows has no `openat`, so this resolves, checks and then opens. Creating a symbolic link
/// there takes a privilege ordinary processes do not hold, which keeps the gap between the check
/// and the open out of reach of code running in the working directory.
#[cfg(not(unix))]
fn open_beneath(root: &Path, relative: &Path) -> std::io::Result<File> {
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the path resolves outside the Bash working directory",
        ));
    }
    File::open(path)
}
