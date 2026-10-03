//! Configuration commits cover disk failure, rejection, concurrent readers and caller cancellation.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::Method;
use kanon_api::{ApiState, app};
use kanon_core::ManagedHost;
use kanon_proto::v1::plugin_host_service_server::{PluginHostService, PluginHostServiceServer};
use kanon_proto::v1::{
    GetPluginMetaRequest, GetPluginMetaResponse, PingRequest, PingResponse, PluginActionRequest,
    PluginActionResponse, ReloadPluginConfigRequest, ReloadPluginConfigResponse,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, Semaphore};
use tonic::{Request, Response, Status};

use common::{FIXTURE_HOST_ID, FIXTURE_PLUGIN_ID, send_json};

struct HostState {
    config: Mutex<Value>,
    version: AtomicU64,
    calls: AtomicUsize,
    entered: Notify,
    release: Semaphore,
}

struct ControlledHost(Arc<HostState>);

#[tonic::async_trait]
impl PluginHostService for ControlledHost {
    async fn invoke_action(
        &self,
        _: Request<PluginActionRequest>,
    ) -> Result<Response<PluginActionResponse>, Status> {
        Err(Status::unimplemented("no actions"))
    }

    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        Ok(Response::new(PingResponse {
            timestamp: request.into_inner().timestamp,
        }))
    }

    async fn get_plugin_meta(
        &self,
        _: Request<GetPluginMetaRequest>,
    ) -> Result<Response<GetPluginMetaResponse>, Status> {
        Ok(Response::new(GetPluginMetaResponse {
            plugins: vec![common::fixture_meta()],
        }))
    }

    async fn reload_plugin_config(
        &self,
        request: Request<ReloadPluginConfigRequest>,
    ) -> Result<Response<ReloadPluginConfigResponse>, Status> {
        let request = request.into_inner();
        let value = kanon_llm::tool_router::prost_struct_to_json(request.config.unwrap());
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        if value["api_key"] == "blocked" {
            self.0.entered.notify_one();
            self.0.release.acquire().await.unwrap().forget();
        }
        if value["api_key"] == "rejected" {
            return Ok(Response::new(ReloadPluginConfigResponse {
                success: false,
                error_message: "fixture rejection".into(),
                applied_version: self.0.version.load(Ordering::SeqCst),
            }));
        }
        *self.0.config.lock().unwrap() = value.clone();
        self.0.version.store(request.version, Ordering::SeqCst);
        if value["api_key"] == "uncertain" {
            return Err(Status::unavailable(
                "reply lost after applying configuration",
            ));
        }
        Ok(Response::new(ReloadPluginConfigResponse {
            success: true,
            error_message: String::new(),
            applied_version: request.version,
        }))
    }
}

async fn fixture(dir: &Path) -> (ApiState, Arc<HostState>) {
    let state = common::fixture_state(dir.to_path_buf(), false).await;
    let live = Arc::new(HostState {
        config: Mutex::new(json!({"api_key": "old"})),
        version: AtomicU64::new(0),
        calls: AtomicUsize::new(0),
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    state
        .config_store()
        .store(FIXTURE_PLUGIN_ID, &json!({"api_key": "old"}))
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let service = ControlledHost(live.clone());
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(PluginHostServiceServer::new(service))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    let channel = tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    state
        .supervisor()
        .register_managed_host(Arc::new(
            ManagedHost::new(
                FIXTURE_HOST_ID.into(),
                dir.join("host.sock"),
                channel,
                vec![common::fixture_meta()],
                100,
            )
            .with_manifest(common::fixture_manifest()),
        ))
        .await;
    (state, live)
}

fn uri() -> String {
    format!("/api/v1/plugins/{FIXTURE_PLUGIN_ID}/config")
}

#[tokio::test]
async fn persistence_failure_does_not_reconfigure_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let (state, live) = fixture(dir.path()).await;
    let path = state.config_store().config_path(FIXTURE_PLUGIN_ID).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();

    let (status, _) = send_json(
        &app(state.clone()),
        Method::PUT,
        &uri(),
        Some(json!({
            "values": {"api_key": "new"}, "version": 0
        })),
    )
    .await;
    assert_eq!(status, 500);
    assert_eq!(live.calls.load(Ordering::SeqCst), 0);
    assert_eq!(*live.config.lock().unwrap(), json!({"api_key": "old"}));
    assert_eq!(
        state.supervisor().config_version(FIXTURE_PLUGIN_ID).await,
        0
    );
}

#[tokio::test]
async fn rejection_restores_the_exact_file_and_allows_retry_with_the_same_version() {
    let dir = tempfile::tempdir().unwrap();
    let (state, live) = fixture(dir.path()).await;
    let path = state.config_store().config_path(FIXTURE_PLUGIN_ID).unwrap();
    let previous = b"{\"api_key\":\"old\"}\n";
    std::fs::write(&path, previous).unwrap();
    let router = app(state.clone());

    let (status, _) = send_json(
        &router,
        Method::PUT,
        &uri(),
        Some(json!({
            "values": {"api_key": "rejected"}, "version": 0
        })),
    )
    .await;
    assert_eq!(status, 502);
    assert_eq!(std::fs::read(&path).unwrap(), previous.to_vec());
    assert_eq!(*live.config.lock().unwrap(), json!({"api_key": "old"}));
    assert_eq!(
        state.supervisor().config_version(FIXTURE_PLUGIN_ID).await,
        0
    );

    let (status, response) = send_json(
        &router,
        Method::PUT,
        &uri(),
        Some(json!({
            "values": {"api_key": "accepted"}, "version": 0
        })),
    )
    .await;
    assert_eq!(status, 200, "{response}");
    assert_eq!(response["version"], 1);
    assert_eq!(
        state.config_store().load(FIXTURE_PLUGIN_ID).unwrap(),
        *live.config.lock().unwrap()
    );
}

#[tokio::test]
async fn readers_wait_for_a_cancelled_callers_commit_but_other_plugins_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let (state, live) = fixture(dir.path()).await;
    let router = app(state.clone());
    let writer_router = router.clone();
    let writer = tokio::spawn(async move {
        send_json(
            &writer_router,
            Method::PUT,
            &uri(),
            Some(json!({
                "values": {"api_key": "blocked"}, "version": 0
            })),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), live.entered.notified())
        .await
        .unwrap();
    let config_uri = uri();
    let mut reading = Box::pin(send_json(&router, Method::GET, &config_uri, None));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut reading)
            .await
            .is_err()
    );
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            state.supervisor().config_version("another.plugin")
        )
        .await
        .unwrap(),
        0
    );

    writer.abort();
    assert!(writer.await.unwrap_err().is_cancelled());
    live.release.add_permits(1);
    let (status, response) = tokio::time::timeout(Duration::from_secs(2), reading)
        .await
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(response["values"]["api_key"], "blocked");
    assert_eq!(response["version"], 1);
    assert_eq!(
        state.config_store().load(FIXTURE_PLUGIN_ID).unwrap(),
        *live.config.lock().unwrap()
    );
}

#[tokio::test]
async fn concurrent_unversioned_updates_preserve_host_and_file_order() {
    let dir = tempfile::tempdir().unwrap();
    let (state, live) = fixture(dir.path()).await;
    let first_router = app(state.clone());
    let second_router = first_router.clone();
    let first = tokio::spawn(async move {
        send_json(
            &first_router,
            Method::PUT,
            &uri(),
            Some(json!({
                "values": {"api_key": "blocked"}
            })),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), live.entered.notified())
        .await
        .unwrap();
    let second = tokio::spawn(async move {
        send_json(
            &second_router,
            Method::PUT,
            &uri(),
            Some(json!({
                "values": {"api_key": "second"}
            })),
        )
        .await
    });
    live.release.add_permits(1);
    let (status, first) = first.await.unwrap();
    assert_eq!(status, 200, "{first}");
    let (status, second) = second.await.unwrap();
    assert_eq!(status, 200, "{second}");
    assert_eq!(first["version"], 1);
    assert_eq!(second["version"], 2);
    assert_eq!(*live.config.lock().unwrap(), json!({"api_key": "second"}));
    assert_eq!(
        state.config_store().load(FIXTURE_PLUGIN_ID).unwrap(),
        *live.config.lock().unwrap()
    );
}

#[tokio::test]
async fn shutdown_drains_detached_commits_but_cancelled_queued_edits_never_apply() {
    let dir = tempfile::tempdir().unwrap();
    let (state, live) = fixture(dir.path()).await;
    let router = app(state.clone());
    let writer_router = router.clone();
    let writer = tokio::spawn(async move {
        send_json(
            &writer_router,
            Method::PUT,
            &uri(),
            Some(json!({ "values": {"api_key": "blocked"} })),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), live.entered.notified())
        .await
        .unwrap();

    // Poll and cancel another HTTP request while it waits for the first transaction's lock.
    // It must never become a detached edit after its caller has already gone away.
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            send_json(
                &router,
                Method::PUT,
                &uri(),
                Some(json!({ "values": {"api_key": "cancelled"} })),
            ),
        )
        .await
        .is_err()
    );
    writer.abort();
    assert!(writer.await.unwrap_err().is_cancelled());

    let mut stopping = Box::pin(state.supervisor().stop_all());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut stopping)
            .await
            .is_err()
    );
    assert!(
        state
            .supervisor()
            .find_host_for_plugin(FIXTURE_PLUGIN_ID)
            .await
            .is_some()
    );
    live.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), stopping)
        .await
        .unwrap()
        .unwrap();

    assert!(
        state
            .supervisor()
            .find_host_for_plugin(FIXTURE_PLUGIN_ID)
            .await
            .is_none()
    );
    assert_eq!(live.calls.load(Ordering::SeqCst), 1);
    assert_eq!(*live.config.lock().unwrap(), json!({"api_key": "blocked"}));
    assert_eq!(
        state.config_store().load(FIXTURE_PLUGIN_ID).unwrap(),
        *live.config.lock().unwrap()
    );
    assert_eq!(
        state.supervisor().config_version(FIXTURE_PLUGIN_ID).await,
        1
    );
}

#[tokio::test]
async fn uncertain_rpc_restores_the_file_and_removes_the_host_from_routing() {
    let dir = tempfile::tempdir().unwrap();
    let (state, live) = fixture(dir.path()).await;
    let (status, response) = send_json(
        &app(state.clone()),
        Method::PUT,
        &uri(),
        Some(json!({
            "values": {"api_key": "uncertain"}, "version": 0
        })),
    )
    .await;
    assert_eq!(status, 502, "{response}");
    assert_eq!(
        *live.config.lock().unwrap(),
        json!({"api_key": "uncertain"})
    );
    assert!(
        state
            .supervisor()
            .find_host_for_plugin(FIXTURE_PLUGIN_ID)
            .await
            .is_none()
    );
    assert_eq!(
        state.config_store().load(FIXTURE_PLUGIN_ID).unwrap(),
        json!({"api_key": "old"})
    );
    assert_eq!(
        state.supervisor().config_version(FIXTURE_PLUGIN_ID).await,
        0
    );
}
