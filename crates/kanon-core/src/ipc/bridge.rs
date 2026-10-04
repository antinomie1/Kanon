//! Optional native-agent credential handoff inside the already protected core run directory.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Keeps the published credential scoped to the lifetime of this bound IPC server.
pub(super) struct BridgeToken(PathBuf);

impl BridgeToken {
    pub(super) fn publish(socket: &Path, token: &str) -> std::io::Result<Option<Self>> {
        if token.is_empty() {
            // Embedded cores may deliberately use filesystem-only authentication on Unix.
            return Ok(None);
        }
        let parent = socket
            .parent()
            .ok_or_else(|| std::io::Error::other("IPC socket has no parent"))?;
        let path = socket.with_extension("agent-token");
        // NamedTempFile starts owner-readable only. Replace atomically after the IPC bind has
        // established ownership; another process that failed to bind cannot replace this token.
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(token.as_bytes())?;
        file.flush()?;
        file.persist(&path).map_err(|error| error.error)?;
        Ok(Some(Self(path)))
    }
}

impl Drop for BridgeToken {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "Could not remove native-agent IPC credential");
        }
    }
}
