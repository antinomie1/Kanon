//! Failed launches remain supervised, and competing launches cannot replace their owner.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use kanon_core::supervisor::{HostRegistration, Supervisor, SupervisorError};
use kanon_core::toggle::ToggleStore;
use kanon_proto::v1::plugin_host_service_server::{PluginHostService, PluginHostServiceServer};
use kanon_proto::v1::*;
use tonic::{Request, Response, Status};

/// Keeps the handshake independent of the shell child's deliberately failing launch recipe.
#[derive(Default)]
struct MetadataHost {
    /// Holds an external attachment inside its metadata RPC while a launch competes with it.
    handshake: Option<Arc<HandshakeGate>>,
    /// Exercises a rejected metadata RPC without inventing a fallback declaration.
    reject: bool,
}

#[derive(Default)]
struct HandshakeGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[tonic::async_trait]
impl PluginHostService for MetadataHost {
    async fn get_plugin_meta(
        &self,
        _: Request<GetPluginMetaRequest>,
    ) -> Result<Response<GetPluginMetaResponse>, Status> {
        if let Some(gate) = &self.handshake {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        if self.reject {
            return Err(Status::unavailable("metadata unavailable"));
        }
        Ok(Response::new(GetPluginMetaResponse {
            plugins: vec![PluginMeta {
                id: "retry".to_string(),
                ..Default::default()
            }],
        }))
    }

    async fn ping(&self, _: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        Ok(Response::new(PingResponse::default()))
    }

    async fn reload_plugin_config(
        &self,
        _: Request<ReloadPluginConfigRequest>,
    ) -> Result<Response<ReloadPluginConfigResponse>, Status> {
        Err(Status::unimplemented("test"))
    }

    async fn invoke_action(
        &self,
        _: Request<PluginActionRequest>,
    ) -> Result<Response<PluginActionResponse>, Status> {
        Err(Status::unimplemented("test"))
    }
}

fn write_launcher(path: &std::path::Path) {
    std::fs::write(path, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn watchdog_retries_a_failed_relaunch_after_the_executable_recovers() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path().join("run");
    let supervisor = Arc::new(Supervisor::new(Some(run_dir.clone()), None));
    let socket = kanon_transport::host_socket_path("retry", Some(&run_dir));
    let listener = kanon_transport::IpcListener::bind(socket).unwrap();
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(PluginHostServiceServer::new(MetadataHost::default()))
            .serve_with_incoming(listener.incoming())
            .await
            .unwrap();
    });
    let executable = temp.path().join("launcher");
    write_launcher(&executable);
    let original = supervisor
        .spawn_plugin("retry", &executable, &[])
        .await
        .unwrap();
    let pid = original.pid().await.unwrap();
    std::fs::remove_file(&executable).unwrap();
    // This is the real failure path: a previously healthy child exits, then its first
    // automatic relaunch cannot spawn. The supervision entry must survive that failure.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);
    let watchdog = supervisor.spawn_host_watchdog(
        Arc::new(ToggleStore::in_memory()),
        Duration::from_millis(20),
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if original.health().await.restarts == 1 && original.pid().await.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the first relaunch failed");
    let retained = supervisor.get_host("retry").await.expect("retained recipe");
    assert!(Arc::ptr_eq(&original, &retained));
    assert_eq!(retained.health().await.state, "restarting");

    write_launcher(&executable);
    let recovered = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let host = supervisor.get_host("retry").await.unwrap();
            if host.pid().await.is_some() && host.health().await.restarts >= 2 {
                break host;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("watchdog retried after recovery");
    assert!(!Arc::ptr_eq(&original, &recovered));
    assert_eq!(recovered.health().await.state, "running");
    watchdog.abort();
    let _ = watchdog.await;
    supervisor.stop_all().await.unwrap();
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn an_external_handshake_reserves_its_host_until_registration_finishes() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path().join("run");
    let supervisor = Arc::new(Supervisor::new(Some(run_dir.clone()), None));
    let socket = kanon_transport::host_socket_path("retry", Some(&run_dir));
    let listener = kanon_transport::IpcListener::bind(&socket).unwrap();
    let gate = Arc::new(HandshakeGate::default());
    let host = MetadataHost {
        handshake: Some(gate.clone()),
        ..Default::default()
    };
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(PluginHostServiceServer::new(host))
            .serve_with_incoming(listener.incoming())
            .await
            .unwrap();
    });
    let attaching = supervisor.clone();
    let registration = tokio::spawn(async move {
        attaching
            .register_host_endpoint("retry", "rust", socket.to_str().unwrap(), &[])
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
        .await
        .expect("external metadata handshake started");
    assert!(supervisor.get_host("retry").await.is_none());

    // The registry is still empty, but the external handshake owns this identity. A process
    // launch here used to pass the precheck and race the attachment's later publication.
    assert!(matches!(
        supervisor
            .spawn_plugin("retry", temp.path().join("missing-executable"), &[])
            .await,
        Err(SupervisorError::HostBusy(_))
    ));
    assert!(matches!(
        supervisor
            .register_host_endpoint("retry", "rust", "missing.sock", &[])
            .await,
        Ok(HostRegistration::Launching)
    ));
    gate.release.notify_one();
    let HostRegistration::Registered(host) = registration.await.unwrap().unwrap() else {
        panic!("the reservation owner must publish the external host");
    };
    assert!(Arc::ptr_eq(
        &host,
        &supervisor.get_host("retry").await.unwrap()
    ));
    assert!(host.declares_plugin("retry"));
    supervisor.stop_all().await.unwrap();
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn a_pending_launch_excludes_competitors_and_releases_its_reservation_on_cancel() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(Supervisor::new(Some(temp.path().join("run")), None));
    let launching = supervisor.clone();
    let first =
        tokio::spawn(async move { launching.spawn_plugin("pending", "sleep", &["30"]).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(
                supervisor
                    .register_host_endpoint("pending", "rust", "missing.sock", &[])
                    .await,
                Ok(HostRegistration::Launching)
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first launch owns its handshake");
    assert!(matches!(
        supervisor.spawn_plugin("pending", "sleep", &["30"]).await,
        Err(SupervisorError::HostBusy(_))
    ));
    first.abort();
    let _ = first.await;
    // A canceled reservation must not prevent the operator correcting a failed launch.
    assert!(matches!(
        supervisor
            .spawn_plugin("pending", temp.path().join("missing-executable"), &[])
            .await,
        Err(SupervisorError::Io(_))
    ));
}

#[tokio::test]
async fn failed_external_metadata_never_publishes_a_host_and_releases_the_reservation() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path().join("run");
    let supervisor = Supervisor::new(Some(run_dir.clone()), None);
    let socket = kanon_transport::host_socket_path("retry", Some(&run_dir));
    let listener = kanon_transport::IpcListener::bind(&socket).unwrap();
    let service = MetadataHost {
        reject: true,
        ..Default::default()
    };
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(PluginHostServiceServer::new(service))
            .serve_with_incoming(listener.incoming())
            .await
            .unwrap();
    });
    // A second attempt must perform the handshake again, not accept a fabricated registry entry
    // or remain stuck behind the failed attempt's launch reservation.
    for _ in 0..2 {
        let result = supervisor
            .register_host_endpoint("retry", "rust", socket.to_str().unwrap(), &["retry".into()])
            .await;
        assert!(matches!(result, Err(SupervisorError::Rpc(status))
            if status.code() == tonic::Code::Unavailable));
        assert!(supervisor.get_host("retry").await.is_none());
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn stalled_external_metadata_times_out_without_leaving_a_reservation() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path().join("run");
    let supervisor = Arc::new(Supervisor::new(Some(run_dir.clone()), None));
    let socket = kanon_transport::host_socket_path("retry", Some(&run_dir));
    let listener = kanon_transport::IpcListener::bind(&socket).unwrap();
    let service = MetadataHost {
        handshake: Some(Arc::new(HandshakeGate::default())),
        ..Default::default()
    };
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(PluginHostServiceServer::new(service))
            .serve_with_incoming(listener.incoming())
            .await
            .unwrap();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(7),
        supervisor.register_host_endpoint("retry", "rust", socket.to_str().unwrap(), &[]),
    )
    .await
    .expect("the full handshake must have a finite deadline");
    assert!(matches!(result, Err(SupervisorError::Timeout(_))));
    assert!(supervisor.get_host("retry").await.is_none());
    assert!(matches!(
        supervisor
            .spawn_plugin("retry", temp.path().join("missing-executable"), &[])
            .await,
        Err(SupervisorError::Io(_))
    ));
    server.abort();
    let _ = server.await;
}
