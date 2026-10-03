//! A host that never becomes responsive cannot retain a command lane indefinitely.

use std::sync::Arc;
use std::time::Duration;

use kanon_core::supervisor::{COMMAND_TIMEOUT, ManagedHost};
use kanon_proto::v1::CommandExecuteRequest;

#[tokio::test]
async fn an_unresponsive_command_channel_releases_its_lane_at_the_deadline() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, connected) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        // Keep the connection alive without answering HTTP/2 readiness. The command's
        // deadline must cover channel readiness as well as an already-dispatched handler.
        let (socket, _) = listener.accept().await.unwrap();
        accepted.send(()).unwrap();
        std::future::pending::<()>().await;
        drop(socket);
    });
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{address}"))
        .unwrap()
        .connect_lazy();
    let host = Arc::new(ManagedHost::new(
        "hung".to_string(),
        std::path::PathBuf::new(),
        channel,
        Vec::new(),
        500,
    ));
    let calling_host = host.clone();
    let command = tokio::spawn(async move {
        calling_host
            .execute_command(CommandExecuteRequest::default())
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), connected)
        .await
        .expect("command reached the host")
        .unwrap();

    tokio::time::pause();
    tokio::time::advance(COMMAND_TIMEOUT + Duration::from_millis(1)).await;
    let error = command.await.unwrap().unwrap_err();
    assert_eq!(error.code(), tonic::Code::DeadlineExceeded);
    assert_eq!(host.circuit_breaker.consecutive_failures(), 1);
    server.abort();
    let _ = server.await;
}
