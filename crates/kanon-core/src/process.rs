//! Cleanup shared by local commands and native dependency installers.

/// Kills the private Unix process group created with `Command::process_group(0)` on every exit.
/// The child handle still owns reaping the direct child; this guard also stops its descendants.
pub(crate) struct ProcessGroup(pub(crate) u32);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SAFETY: an OS child PID is positive and fits pid_t. A negative PID targets only the
        // private group created for this child, never the node's own process group.
        if unsafe { libc::kill(-(self.0 as libc::pid_t), libc::SIGKILL) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                tracing::error!(pid = self.0, %error, "Failed to stop child process group");
            }
        }
    }
}
