//! Fetching plugins and market indexes from outside the node.
//!
//! Everything here talks to the network or spawns a process on the operator's behalf, so every
//! input is checked before anything happens and every operation is bounded:
//!
//! - **URLs** must be `https://`. Plain `http://` is accepted only for loopback hosts (a local
//!   mirror or a test server), because a package fetched in clear text over a network can be
//!   swapped in transit, and a plugin is code the node will execute. URLs carrying credentials
//!   are refused so a secret never ends up in logs or error messages. Redirects are checked hop
//!   by hop against the same rule, so an `https` link cannot downgrade to `http`.
//! - **Downloads** have a size cap enforced while streaming (a lying or missing
//!   `Content-Length` cannot get past it) and an overall deadline.
//! - **Git** is the system `git` binary, called with an argument vector (never a shell), `--` in
//!   front of the URL so it can never be read as an option, a whitelist of transports (no
//!   `ext::` helpers, no unauthenticated `git://`), no terminal prompts and a deadline after
//!   which the process is killed. Kanon does not bundle a Git implementation: a node without
//!   `git` reports that explicitly.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use reqwest::Url;

/// Largest plugin package (`.kpk` / `.zip`) accepted by upload or download.
pub const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Largest market index document read from one source.
pub const MAX_INDEX_BYTES: u64 = 2 * 1024 * 1024;

/// Deadline for downloading one plugin package.
pub const PACKAGE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// Deadline for fetching one market index.
pub const INDEX_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Deadline for `git clone`, after which the process is killed.
pub const GIT_CLONE_TIMEOUT: Duration = Duration::from_secs(180);

/// Redirect hops followed before a download is abandoned.
const MAX_REDIRECTS: usize = 5;

/// Why a remote fetch failed.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The URL is malformed or not allowed by the transport policy.
    #[error("{0}")]
    InvalidUrl(String),
    /// The remote document is larger than the caller's limit.
    #[error("the download is larger than the {limit}-byte limit")]
    TooLarge {
        /// Limit that was exceeded, in bytes.
        limit: u64,
    },
    /// The server answered with a non-success status.
    #[error("the server answered HTTP {0}")]
    Status(u16),
    /// The connection, TLS handshake or transfer failed.
    #[error("the download failed: {0}")]
    Transport(String),
    /// The deadline passed before the download finished.
    #[error("the download did not finish within {0:?}")]
    Timeout(Duration),
}

/// Why cloning a repository failed.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// No `git` executable could be started on this node.
    #[error(
        "git is not installed on this node (or not on PATH); install git or install from a package URL instead"
    )]
    Missing,
    /// The repository URL is malformed or uses a transport that is not allowed.
    #[error("{0}")]
    InvalidUrl(String),
    /// The requested branch or tag name is not acceptable.
    #[error("{0}")]
    InvalidRef(String),
    /// `git clone` ran and failed; carries its own explanation.
    #[error("git clone failed: {0}")]
    Failed(String),
    /// The clone did not finish in time and was killed.
    #[error("git clone did not finish within {0:?} and was stopped")]
    Timeout(Duration),
}

/// Validates an `http(s)` URL against the transport policy described in the module docs.
pub fn check_remote_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|err| format!("'{raw}' is not a valid URL: {err}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs carrying credentials are not accepted".to_string());
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_loopback_host(&url) => Ok(url),
        "http" => Err(format!(
            "'{raw}' uses plain http; only https is accepted (http only for loopback addresses)"
        )),
        other => Err(format!(
            "'{raw}' uses the unsupported scheme '{other}'; use an https URL"
        )),
    }
}

/// Whether a URL's host is the local machine.
fn is_loopback_host(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    // IPv6 hosts are rendered in brackets (`[::1]`).
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Downloads a document under the transport policy, a size cap and a deadline.
///
/// The cap is enforced twice: up front from `Content-Length` (so an honest server is refused
/// before anything is transferred) and again while the body streams in (so a dishonest or
/// chunked response cannot exceed it either).
pub async fn download(url: &str, limit: u64, timeout: Duration) -> Result<Vec<u8>, FetchError> {
    let url = check_remote_url(url).map_err(FetchError::InvalidUrl)?;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error(format!("more than {MAX_REDIRECTS} redirects"));
            }
            match check_remote_url(attempt.url().as_str()) {
                Ok(_) => attempt.follow(),
                Err(reason) => attempt.error(format!("redirect refused: {reason}")),
            }
        }))
        .user_agent(concat!("kanon/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| FetchError::Transport(err.to_string()))?;

    let transfer = async {
        let mut response = client
            .get(url)
            .send()
            .await
            .map_err(|err| FetchError::Transport(error_chain(&err)))?;
        if !response.status().is_success() {
            return Err(FetchError::Status(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > limit)
        {
            return Err(FetchError::TooLarge { limit });
        }

        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| FetchError::Transport(error_chain(&err)))?
        {
            if body.len() as u64 + chunk.len() as u64 > limit {
                return Err(FetchError::TooLarge { limit });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    };

    tokio::time::timeout(timeout, transfer)
        .await
        .map_err(|_| FetchError::Timeout(timeout))?
}

/// Renders an error with its sources, since reqwest's top-level message alone ("error sending
/// request") rarely says what actually went wrong.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

/// Validates a Git repository URL.
///
/// Accepted: `https://…`, `ssh://…`, scp-style `user@host:path`, `file://…` (a repository on this
/// node, no more powerful than installing from a local folder) and `http://` for loopback hosts.
/// Everything else is refused — in particular `ext::` remote helpers, which run arbitrary
/// commands, and `git://`, which is unauthenticated.
pub fn check_git_url(raw: &str) -> Result<(), GitError> {
    let url = raw.trim();
    if url.is_empty() {
        return Err(GitError::InvalidUrl(
            "the repository URL is empty".to_string(),
        ));
    }
    if url.starts_with('-') || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(GitError::InvalidUrl(format!(
            "'{}' is not a repository URL",
            url.escape_debug()
        )));
    }

    if let Some((scheme, _)) = url.split_once("://") {
        return match scheme.to_ascii_lowercase().as_str() {
            "https" | "ssh" | "file" => Ok(()),
            "http" => check_remote_url(url)
                .map(|_| ())
                .map_err(GitError::InvalidUrl),
            other => Err(GitError::InvalidUrl(format!(
                "repository URLs using '{other}://' are not accepted; use https, ssh or file"
            ))),
        };
    }

    // scp-like syntax: `user@host:path`. The part before the colon must look like `user@host`,
    // which also rules out transport helpers such as `ext::<command>`.
    if let Some((left, path)) = url.split_once(':')
        && let Some((user, host)) = left.split_once('@')
        && !user.is_empty()
        && !host.is_empty()
        && !left.contains('/')
        && !path.is_empty()
        && !path.starts_with(':')
    {
        return Ok(());
    }

    Err(GitError::InvalidUrl(format!(
        "'{url}' is not a supported repository URL; use https://, ssh://, file:// or user@host:path"
    )))
}

/// Validates a branch or tag name passed to `git clone --branch`.
fn check_git_ref(raw: &str) -> Result<(), GitError> {
    let valid = !raw.is_empty()
        && !raw.starts_with('-')
        && !raw.contains("..")
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+'));
    if valid {
        Ok(())
    } else {
        Err(GitError::InvalidRef(format!(
            "'{}' is not a branch or tag name",
            raw.escape_debug()
        )))
    }
}

/// Shallow-clones `url` (optionally at branch or tag `git_ref`) into `dest`, which must not exist.
pub async fn git_clone(
    url: &str,
    git_ref: Option<&str>,
    dest: &Path,
    timeout: Duration,
) -> Result<(), GitError> {
    let url = url.trim();
    check_git_url(url)?;
    let git_ref = git_ref.map(str::trim).filter(|value| !value.is_empty());
    if let Some(name) = git_ref {
        check_git_ref(name)?;
    }

    let mut command = tokio::process::Command::new("git");
    command.args(["clone", "--depth", "1", "--single-branch", "--no-tags"]);
    if let Some(name) = git_ref {
        command.args(["--branch", name]);
    }
    command
        .arg("--")
        .arg(url)
        .arg(dest)
        // A credential prompt would block the request until the deadline; failing at once with
        // git's own "could not read Username" message is what the operator needs to see.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // Dropping the future on timeout must not leave a clone running in the background.
        .kill_on_drop(true);

    let child = command.spawn().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            GitError::Missing
        } else {
            GitError::Failed(format!("could not start git: {err}"))
        }
    })?;

    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| GitError::Timeout(timeout))?
        .map_err(|err| GitError::Failed(format!("could not wait for git: {err}")))?;

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr.trim();
    // Keep the end of git's output: the final lines carry the fatal error.
    let tail: String = if message.chars().count() > 1200 {
        let skip = message.chars().count() - 1200;
        format!("…{}", message.chars().skip(skip).collect::<String>())
    } else {
        message.to_string()
    };
    Err(GitError::Failed(if tail.is_empty() {
        format!("git exited with {}", output.status)
    } else {
        tail
    }))
}
