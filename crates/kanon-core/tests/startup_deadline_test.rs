//! A child is not ready until its metadata RPC completes within the startup budget.
#![cfg(unix)]
use kanon_core::supervisor::{Supervisor, SupervisorError};
use kanon_proto::v1::plugin_host_service_server::{PluginHostService, PluginHostServiceServer};
use kanon_proto::v1::*;
use tonic::{Request, Response, Status};

struct HungMetadata;
#[tonic::async_trait]
impl PluginHostService for HungMetadata {
    async fn get_plugin_meta(
        &self,
        _: Request<GetPluginMetaRequest>,
    ) -> Result<Response<GetPluginMetaResponse>, Status> {
        std::future::pending().await
    }
    async fn ping(&self, _: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        Err(Status::unimplemented("test"))
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

#[tokio::test]
async fn metadata_and_readiness_share_one_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Supervisor::new(Some(temp.path().to_owned()), None);
    let path = kanon_transport::host_socket_path("hung", Some(temp.path()));
    let (ready, started) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        // Spend part of the budget waiting for readiness, then hang the metadata RPC.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let listener = kanon_transport::IpcListener::bind(path).unwrap();
        ready.send(()).unwrap();
        tonic::transport::Server::builder()
            .add_service(PluginHostServiceServer::new(HungMetadata))
            .serve_with_incoming(listener.incoming())
            .await
            .unwrap();
    });
    let before = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(7),
        supervisor.spawn_plugin("hung", "sleep", &["30"]),
    )
    .await;
    started.await.unwrap();
    assert!(matches!(result, Ok(Err(SupervisorError::Timeout(_)))));
    assert!(before.elapsed() < std::time::Duration::from_millis(5800));
    assert!(supervisor.get_host("hung").await.is_none());
    server.abort();
    let _ = server.await;
}
