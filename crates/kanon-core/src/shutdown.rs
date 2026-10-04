//! Shutdown signals shared by the node binaries.
//!
//! SIGTERM matters as much as Ctrl-C: service managers (`systemctl stop`), container runtimes
//! (`docker stop`) and a plain `kill` all send it. A node that dies without running its shutdown
//! path never stops its plugin hosts, and those hosts keep their platform connections open — the
//! fresh node then spawns *new* hosts, so two processes serve every message and the user sees
//! duplicate replies while nothing looks broken in the console.

/// Time reserved for HTTP/IPC connections after conversation work has drained.
const SERVER_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Finishes a signalled server without letting a stalled request prevent process shutdown.
///
/// Callers signal graceful shutdown first and keep dependent services alive until this returns.
/// The runtime closes remaining connection tasks when the process exits; aborting the serving
/// task after this grace lets the caller finish stopping adapters and owned child processes.
pub async fn finish_server<E: std::fmt::Display>(
    name: &str,
    mut task: tokio::task::JoinHandle<Result<(), E>>,
) -> Result<(), String> {
    let result = match tokio::time::timeout(SERVER_DRAIN_GRACE, &mut task).await {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(
                server = name,
                "Server drain deadline reached; forcing shutdown"
            );
            task.abort();
            let result = task.await;
            if result.as_ref().is_err_and(|error| error.is_cancelled()) {
                return Ok(());
            }
            result
        }
    };
    result
        .map_err(|error| format!("{name} task failed: {error}"))?
        .map_err(|error| format!("{name} failed: {error}"))
}

/// Completes when the process is asked to stop, by SIGINT or (on Unix) SIGTERM.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(err) => {
                // Without the handler the process still dies on SIGTERM, but silently: the hosts
                // would be orphaned again. Say so instead of pretending shutdown is covered.
                tracing::error!(error = %err, "Failed to install the SIGTERM handler");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("SIGINT received; draining background tasks");
            }
            _ = sigterm.recv() => {
                tracing::info!("SIGTERM received; draining background tasks");
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("Shutdown signal received; draining background tasks");
    }
}
