//! Host health monitoring, bounded restarts and coordinated shutdown.

use super::*;

impl Supervisor {
    /// Starts the host watchdog, which notices crashed hosts and brings them back.
    ///
    /// Why this is separate from shutdown handling: a host can die at any time (panic, OOM,
    /// `SIGKILL`, a plugin's own `process.exit`). Without a monitor the supervisor keeps the dead
    /// host in its registry, so the console shows it as healthy and every routed event fails
    /// against a closed socket. The watchdog retains the launch recipe, relaunches with
    /// exponential backoff, and parks a repeatedly-crashing host as `crashed` so the
    /// fault is visible instead of silently looped.
    ///
    /// Disabled plugins are never restarted: their absence is intentional.
    pub fn spawn_host_watchdog(
        self: &Arc<Self>,
        plugin_state: Arc<ToggleStore>,
        interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            let mut budgets: HashMap<String, RestartBudget> = HashMap::new();
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                ticker.tick().await;
                let hosts = supervisor.get_all_hosts().await.len();
                supervisor.watchdog_tick(&plugin_state, &mut budgets).await;
                // Debug-level heartbeat: a watchdog that stopped running would otherwise leave a
                // dead host advertised as healthy with nothing in the log to explain it.
                tracing::debug!(hosts, "Host watchdog pass complete");
            }
        })
    }

    /// One watchdog pass: inspects every host with a child process.
    pub(super) async fn watchdog_tick(
        &self,
        plugin_state: &ToggleStore,
        budgets: &mut HashMap<String, RestartBudget>,
    ) {
        for host in self.get_all_hosts().await {
            // A retained entry can temporarily have no child while an operator restarts it.
            // Only retry once that whole launch has finished, including dependency setup.
            if lock_launching(&self.launching).contains(&host.host_id) {
                continue;
            }
            let Some(plugin_id) = host.primary_plugin_id() else {
                continue;
            };
            // Configuration commits and operator lifecycle actions use the same plugin lock.
            // A restart must read only a committed file, never an in-flight candidate.
            let _configuration = self.lock_plugin_config(&plugin_id).await;
            if !self
                .get_host(&host.host_id)
                .await
                .is_some_and(|current| Arc::ptr_eq(&current, &host))
            {
                continue;
            }

            let attempts = budgets
                .get(&host.host_id)
                .map(|budget| budget.attempts)
                .unwrap_or(0);

            if !plugin_state.is_enabled(PLUGIN_SECTION, &plugin_id).await {
                // The control plane stops disabled hosts; if one is still present, mark it so the
                // console shows why it is not serving.
                host.report_health("disabled", attempts, None).await;
                continue;
            }

            let health = host.health().await;
            let failure = {
                let mut guard = host.child.lock().await;
                match guard.as_mut() {
                    // `try_wait` reaps the child; a missing child with a launch recipe means
                    // the previous launch failed and still needs supervision.
                    Some(child) => match child.try_wait() {
                        Ok(status) => status.map(|status| format!("exited with {status}")),
                        Err(error) => {
                            tracing::error!(host_id = %host.host_id, %error, "Failed to inspect host process");
                            continue;
                        }
                    },
                    None if host.is_restartable() => Some(
                        health
                            .last_error
                            .clone()
                            .unwrap_or_else(|| "host has no running child".to_string()),
                    ),
                    // Externally registered hosts have no child handle to observe.
                    None => continue,
                }
            };

            let Some(failure) = failure else {
                // Still alive: a host that stayed up long enough has earned a fresh budget.
                let budget = budgets.entry(host.host_id.clone()).or_default();
                let healthy = budget
                    .last_attempt
                    .map(|at| at.elapsed() >= HOST_WATCHDOG_HEALTHY_AFTER)
                    .unwrap_or(true);
                if budget.attempts > 0 && healthy {
                    tracing::info!(
                        host_id = %host.host_id,
                        restarts = budget.attempts,
                        "Host stayed up after restart; clearing its restart budget"
                    );
                    budget.attempts = 0;
                    budget.next_at = None;
                }
                host.report_health("running", budget.attempts, None).await;
                continue;
            };

            tracing::error!(
                host_id = %host.host_id,
                plugin_id = %plugin_id,
                reason = %failure,
                "Plugin host is not running"
            );

            if !host.is_restartable() {
                tracing::error!(
                    host_id = %host.host_id,
                    "Host has no launch recipe; removing it from the registry instead of restarting"
                );
                let _ = self.stop_host(&host.host_id).await;
                continue;
            }

            let budget = budgets.entry(host.host_id.clone()).or_default();
            let now = Instant::now();

            if budget.attempts >= HOST_WATCHDOG_MAX_RESTARTS {
                // Keep the parked entry visible, including its recipe for an operator restart.
                if health.state != "crashed" {
                    tracing::error!(
                        host_id = %host.host_id,
                        plugin_id = %plugin_id,
                        attempts = budget.attempts,
                        "Host crashed after repeated restarts; parking it as crashed until an operator restarts it"
                    );
                }
                host.report_health("crashed", budget.attempts, Some(failure.clone()))
                    .await;
                continue;
            }

            if let Some(next_at) = budget.next_at
                && next_at > now
            {
                host.report_health("restarting", budget.attempts, Some(failure.clone()))
                    .await;
                continue;
            }

            // Exponential backoff: 2s, 4s, 8s, 16s, capped at 30s.
            let backoff = Duration::from_secs(2u64.saturating_pow(budget.attempts + 1).min(30));
            budget.attempts += 1;
            let attempt = budget.attempts;
            budget.last_attempt = Some(now);
            budget.next_at = Some(now + backoff);

            tracing::warn!(
                host_id = %host.host_id,
                plugin_id = %plugin_id,
                attempt,
                max_attempts = HOST_WATCHDOG_MAX_RESTARTS,
                backoff_ms = backoff.as_millis() as u64,
                "Restarting crashed plugin host"
            );

            match self.restart_host(&host.host_id).await {
                Ok(restarted) => {
                    // Carry the attempt count forward so the console shows the full story.
                    restarted.report_health("running", attempt, None).await;
                }
                Err(err) => {
                    tracing::error!(
                        host_id = %host.host_id,
                        error = %err,
                        "Failed to restart crashed plugin host"
                    );
                    budget.next_at = Some(Instant::now() + backoff);
                    host.report_health("restarting", attempt, Some(err.to_string()))
                        .await;
                }
            }
        }
    }

    /// Stops a specific managed host and cleans up its socket.
    pub async fn stop_host(&self, host_id: &str) -> Result<(), SupervisorError> {
        let host = {
            let mut hosts = self.hosts.write().await;
            hosts.remove(host_id)
        };

        if let Some(host) = host {
            let mut child_guard = host.child.lock().await;
            if let Some(mut child) = child_guard.take() {
                // Graceful first: plugins flush state and close platform connections in
                // `on_unload`, which a bare kill would skip entirely.
                if let Err(err) = terminate_child(&mut child, HOST_SHUTDOWN_GRACE).await {
                    tracing::warn!(host_id = %host_id, error = %err, "Failed to terminate host cleanly");
                }
                tracing::info!(host_id = %host_id, "Host process terminated");
            } else {
                // Externally registered hosts have no child handle. They cannot be signalled, so
                // they rely on their own core-liveness watchdog to stop; the removal above means
                // the core no longer routes anything to them in the meantime.
                tracing::info!(
                    host_id = %host_id,
                    "Host had no child handle (externally registered); it stops via its own core-liveness watchdog"
                );
            }
            Ok(())
        } else {
            Err(SupervisorError::HostNotFound(host_id.to_string()))
        }
    }

    /// Finishes outstanding configuration commits, then stops all managed host processes.
    ///
    /// The caller must first stop HTTP, watchdogs and other lifecycle work from admitting new
    /// operations. Configuration handlers reserve their lock before detaching, so after those
    /// entry points drain this snapshot includes every remaining commit or rollback, even one
    /// that has already removed its host. No host may be killed halfway through that transaction.
    pub async fn stop_all(&self) -> Result<(), SupervisorError> {
        let configurations: Vec<_> = self
            .config_versions
            .read()
            .await
            .values()
            .cloned()
            .collect();
        for configuration in configurations {
            drop(configuration.lock().await);
        }
        let host_ids: Vec<String> = self.hosts.read().await.keys().cloned().collect();
        for id in host_ids {
            let _ = self.stop_host(&id).await;
        }
        Ok(())
    }
}
