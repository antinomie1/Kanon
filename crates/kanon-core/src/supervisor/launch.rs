//! Host process launch, dependency resolution and restart from recorded recipes.

use super::*;

impl Supervisor {
    /// Spawns a plugin host sub-process, waits for its IPC socket readiness,
    /// performs the initial `GetPluginMeta` handshake, and registers it with default priority (500).
    pub async fn spawn_plugin(
        &self,
        host_id: &str,
        executable_path: impl AsRef<Path>,
        args: &[&str],
    ) -> Result<Arc<ManagedHost>, SupervisorError> {
        self.spawn_plugin_with_priority(host_id, executable_path, args, 500)
            .await
    }

    /// Spawns a plugin host sub-process with explicit execution priority,
    /// waits for its IPC socket readiness, performs the initial `GetPluginMeta`
    /// handshake, and registers it in the supervisor registry.
    pub async fn spawn_plugin_with_priority(
        &self,
        host_id: &str,
        executable_path: impl AsRef<Path>,
        args: &[&str],
        priority: i32,
    ) -> Result<Arc<ManagedHost>, SupervisorError> {
        let launching = LaunchGuard::new(&self.launching, host_id)?;
        if self.get_host(host_id).await.is_some() {
            return Err(SupervisorError::HostBusy(host_id.to_string()));
        }
        let spec = LaunchSpec::Direct {
            executable: executable_path.as_ref().to_path_buf(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
            priority,
        };
        self.launch_host(
            &launching,
            executable_path.as_ref(),
            args,
            priority,
            spec,
            None,
        )
        .await
    }

    /// Core host launch routine shared by every spawn path.
    ///
    /// `spec` records how the process was launched so that a later control-plane restart
    /// can faithfully reconstruct the same command line, and `manifest` retains the static
    /// plugin declaration for metadata / configuration-schema queries.
    pub(super) async fn launch_host(
        &self,
        launching: &LaunchGuard,
        executable_path: &Path,
        args: &[&str],
        priority: i32,
        spec: LaunchSpec,
        manifest: Option<PluginManifest>,
    ) -> Result<Arc<ManagedHost>, SupervisorError> {
        // Every caller reserves the whole operation, including dependency installation. The
        // host's registration is acknowledged without dialing it while that reservation lives.
        let host_id = launching.host_id.as_str();
        let socket_path = host_socket_path(host_id, Some(&self.run_dir));

        tracing::info!(
            host_id = %host_id,
            priority = priority,
            executable = %executable_path.display(),
            socket = %socket_path.display(),
            "Spawning plugin host process"
        );

        let mut cmd = Command::new(executable_path);
        cmd.args(args)
            .env("KANON_HOST_ID", host_id)
            .env("KANON_HOST_SOCK", &socket_path)
            .env("KANON_CORE_SOCK", &self.core_sock_path)
            .env("KANON_IPC_TOKEN", &self.ipc_token)
            .kill_on_drop(true);

        let mut child = cmd.spawn()?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

        // Wait for child process to bind the socket and become ready.
        // We poll every 50ms with a 5-second deadline.
        let channel = match self
            .wait_for_readiness(&mut child, host_id, &socket_path, deadline)
            .await
        {
            Ok(ch) => ch,
            Err(e) => {
                // The process never became usable, so there is nothing to unload gracefully:
                // kill it outright to prevent an orphan.
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(e);
            }
        };

        // Create gRPC clients for handshake and future message pipeline calls.
        let mut host_client = PluginHostServiceClient::with_interceptor(
            channel.clone(),
            kanon_transport::ClientAuthInterceptor(self.ipc_token.clone()),
        );
        let pipeline_client = MessagePipelineServiceClient::with_interceptor(
            channel,
            kanon_transport::ClientAuthInterceptor(self.ipc_token.clone()),
        );

        // Perform initial GetPluginMeta handshake to verify contract compatibility
        // and discover static command/tool definitions.
        tracing::debug!(host_id = %host_id, "Conducting GetPluginMeta handshake");
        let meta_response = match tokio::time::timeout_at(
            deadline,
            host_client.get_plugin_meta(GetPluginMetaRequest {}),
        )
        .await
        {
            Ok(Ok(response)) => response,
            outcome => {
                // The same deadline includes HTTP/2 readiness and metadata. Reap the failed
                // child before returning so neither a hung RPC nor retries leak processes.
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(match outcome {
                    Ok(Err(status)) => SupervisorError::from(status),
                    Err(_) => SupervisorError::Timeout(host_id.to_string()),
                    Ok(Ok(_)) => unreachable!(),
                });
            }
        };

        let plugins = meta_response.into_inner().plugins;
        tracing::info!(
            host_id = %host_id,
            plugin_count = plugins.len(),
            "Handshake completed successfully with plugin host"
        );

        let managed_host = Arc::new(ManagedHost {
            host_id: host_id.to_string(),
            socket_path: socket_path.clone(),
            child: Mutex::new(Some(child)),
            host_client,
            pipeline_client,
            meta: std::sync::RwLock::new(plugins),
            priority,
            launch_spec: Some(spec),
            manifest,
            circuit_breaker: Arc::new(CircuitBreaker::with_defaults()),
            health: Mutex::new(HostHealth::default()),
        });

        self.hosts
            .write()
            .await
            .insert(host_id.to_string(), managed_host.clone());

        Ok(managed_host)
    }

    /// Spawns a plugin sub-process based on a `plugin.toml` manifest file,
    /// dynamically resolving the runtime launcher (Rust native binary, the plugin's own Python
    /// virtual environment, or Node/Bun runtime for TypeScript).
    ///
    /// Every plugin runs in its own environment (`<plugin>/.venv`, `<plugin>/node_modules`). With
    /// a [`DependencyInstaller`] that environment is created or refreshed with the plugin's native
    /// tool first; without one it must already exist. Either way a missing environment reports
    /// the plugin as `RuntimeUnavailable` before any process starts, instead of crashing on an
    /// import error inside a restart loop.
    pub async fn spawn_from_manifest(
        &self,
        manifest_path: impl AsRef<Path>,
        executable_override: Option<&Path>,
    ) -> Result<Arc<ManagedHost>, SupervisorError> {
        let manifest_path_ref = manifest_path.as_ref();
        let manifest = PluginManifest::load_from_file(manifest_path_ref)
            .map_err(|e| SupervisorError::Manifest(e.to_string()))?;
        let host_id = manifest.plugin.id.replace('.', "_");
        let launching = LaunchGuard::new(&self.launching, &host_id)?;
        if self.get_host(&host_id).await.is_some() {
            return Err(SupervisorError::HostBusy(host_id));
        }
        self.launch_manifest(manifest_path_ref, executable_override, manifest, &launching)
            .await
    }

    /// Resolves dependencies and starts a manifest while its caller owns the launch reservation.
    pub(super) async fn launch_manifest(
        &self,
        manifest_path_ref: &Path,
        executable_override: Option<&Path>,
        manifest: PluginManifest,
        launching: &LaunchGuard,
    ) -> Result<Arc<ManagedHost>, SupervisorError> {
        if let Some(adapter) = manifest
            .adapter
            .as_ref()
            .filter(|adapter| adapter.sends_no_media())
        {
            tracing::warn!(
                plugin_id = %manifest.plugin.id,
                platform = %adapter.platform,
                "Adapter plugin declares no send_image/send_voice/send_video/send_file capability; \
                 tool-produced media is left out of its replies. Declare the kinds the platform \
                 delivers under [adapter] capabilities in plugin.toml"
            );
        }

        // A plugin built for another node version is refused before anything is installed or
        // started, and recorded as unavailable with the reason so the console shows it.
        if let Err(err) = crate::manifest::check_kanon_version(&manifest.plugin) {
            let reason = err.to_string();
            self.record_unavailable_plugin(
                manifest,
                manifest_path_ref.to_path_buf(),
                reason.clone(),
            )
            .await;
            return Err(SupervisorError::RuntimeUnavailable {
                runtime: "kanon".to_string(),
                reason,
            });
        }

        let priority = manifest.plugin.priority.unwrap_or(500);
        let parent = manifest_path_ref.parent().unwrap_or_else(|| Path::new("."));

        // Manifest-driven launches are replayed through the manifest on restart, so the
        // recipe only needs to remember the manifest location and priority.
        let spec = LaunchSpec::Manifest {
            manifest_path: manifest_path_ref.to_path_buf(),
            executable_override: executable_override.map(Path::to_path_buf),
            priority,
        };

        // The resolution steps below return early with `?`. Running them inside this block keeps
        // those early returns from skipping the unavailable-plugin bookkeeping that follows.
        let result = async {
            if let Some(override_path) = executable_override {
                self.launch_host(
                    launching,
                    override_path,
                    &[],
                    priority,
                    spec,
                    Some(manifest.clone()),
                )
                .await
            } else {
                match manifest.plugin.runtime.as_str() {
                    "rust" => {
                        let exec_path = parent.join(&manifest.plugin.entrypoint);
                        self.launch_host(
                            launching,
                            &exec_path,
                            &[],
                            priority,
                            spec,
                            Some(manifest.clone()),
                        )
                        .await
                    }
                    "python" => {
                        // Each plugin runs in its own environment so two plugins can never
                        // disagree about a package version. There is deliberately no fallback to a
                        // shared or system interpreter: it would start without the plugin's packages.
                        let python_bin = match &self.dependencies {
                            Some(installer) => installer.prepare_python(parent).await,
                            None => deps::plugin_python(parent).ok_or_else(|| {
                                format!(
                                    "Python environment '{}' not found; run `uv sync` in the plugin directory",
                                    parent.join(".venv").display()
                                )
                            }),
                        }
                        .map_err(|reason| SupervisorError::RuntimeUnavailable {
                            runtime: "python".to_string(),
                            reason,
                        })?;

                        // The host runner ships with the SDK, which the plugin's environment
                        // depends on, so it is run from there: no path to configure, and it works
                        // wherever the plugin is installed.
                        let manifest_str = manifest_path_ref.to_string_lossy();
                        let args = ["-m", "kanon_host.main", "--plugin", manifest_str.as_ref()];

                        self.launch_host(
                            launching,
                            &python_bin,
                            &args,
                            priority,
                            spec,
                            Some(manifest.clone()),
                        )
                        .await
                    }
                    "typescript" | "ts" => {
                        match &self.dependencies {
                            Some(installer) => installer.prepare_node(parent).await,
                            None => ensure_node_modules(parent),
                        }
                        .map_err(|reason| SupervisorError::RuntimeUnavailable {
                            runtime: "typescript".to_string(),
                            reason,
                        })?;

                        let node_bin = self
                            .typescript_runtime
                            .clone()
                            .or_else(|| find_binary_in_path("bun"))
                            .or_else(|| find_binary_in_path("node"))
                            .ok_or_else(|| SupervisorError::RuntimeUnavailable {
                                runtime: "typescript".to_string(),
                                reason: "Neither bun nor node was found in PATH".to_string(),
                            })?;

                        // A plugin that depends on the SDK carries the host runner in its own
                        // `node_modules`; inside a Kanon checkout the built SDK is used instead.
                        let installed_host = parent
                            .join("node_modules/@kanon/sdk-and-host/dist/src/host/index.js");
                        let host_script = installed_host
                            .is_file()
                            .then_some(installed_host)
                            .or_else(|| find_file_upwards(parent, "sdks/typescript/dist/src/host/index.js"))
                            .or_else(|| find_file_upwards(parent, "dist/src/host/index.js"))
                            .ok_or_else(|| SupervisorError::RuntimeUnavailable {
                                runtime: "typescript".to_string(),
                                reason: "Could not locate TypeScript host runner script (dist/src/host/index.js)"
                                    .to_string(),
                            })?;

                        let host_script_str = host_script.to_string_lossy();
                        let manifest_str = manifest_path_ref.to_string_lossy();
                        let args = [host_script_str.as_ref(), "--plugin", manifest_str.as_ref()];

                        self.launch_host(
                            launching,
                            &node_bin,
                            &args,
                            priority,
                            spec,
                            Some(manifest.clone()),
                        )
                        .await
                    }
                    other => Err(SupervisorError::RuntimeUnavailable {
                        runtime: other.to_string(),
                        reason: format!("Unsupported plugin runtime '{other}' declared in manifest"),
                    }),
                }
            }
        }
        .await;

        match result {
            Ok(host) => {
                self.remove_unavailable_plugin(&manifest.plugin.id).await;
                Ok(host)
            }
            Err(SupervisorError::RuntimeUnavailable { runtime, reason }) => {
                self.record_unavailable_plugin(
                    manifest,
                    manifest_path_ref.to_path_buf(),
                    reason.clone(),
                )
                .await;
                Err(SupervisorError::RuntimeUnavailable { runtime, reason })
            }
            Err(err) => Err(err),
        }
    }

    /// Restarts the host process identified by `host_id` using its recorded launch recipe.
    ///
    /// The old process is terminated first, then replaced after a fresh handshake. A failed
    /// launch retains its recipe and crashed state so the watchdog can retry it later.
    pub async fn restart_host(&self, host_id: &str) -> Result<Arc<ManagedHost>, SupervisorError> {
        let host = self
            .get_host(host_id)
            .await
            .ok_or_else(|| SupervisorError::HostNotFound(host_id.to_string()))?;

        let spec = host
            .launch_spec()
            .cloned()
            .ok_or_else(|| SupervisorError::RestartUnavailable(host_id.to_string()))?;
        let manifest = host.manifest().cloned();
        let launching = LaunchGuard::new(&self.launching, host_id)?;

        tracing::info!(host_id = %host_id, "Restarting plugin host process");

        {
            let mut child = host.child.lock().await;
            if let Some(mut child) = child.take() {
                terminate_child(&mut child, HOST_SHUTDOWN_GRACE).await?;
            }
        }
        let restarts = host.health().await.restarts;
        host.report_health("restarting", restarts, None).await;

        let result = match &spec {
            LaunchSpec::Direct {
                executable,
                args,
                priority,
            } => {
                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                self.launch_host(
                    &launching,
                    executable,
                    &arg_refs,
                    *priority,
                    spec.clone(),
                    manifest,
                )
                .await
            }
            LaunchSpec::Manifest {
                manifest_path,
                executable_override,
                ..
            } => {
                let manifest = PluginManifest::load_from_file(manifest_path)
                    .map_err(|error| SupervisorError::Manifest(error.to_string()));
                match manifest {
                    Ok(manifest) if manifest.plugin.id.replace('.', "_") == host_id => {
                        self.launch_manifest(
                            manifest_path,
                            executable_override.as_deref(),
                            manifest,
                            &launching,
                        )
                        .await
                    }
                    Ok(_) => Err(SupervisorError::Manifest(
                        "Plugin id changed; restart the node to load its new identity".to_string(),
                    )),
                    Err(error) => Err(error),
                }
            }
        };
        if let Err(error) = &result {
            host.report_health("crashed", restarts, Some(error.to_string()))
                .await;
        }
        result
    }
}
