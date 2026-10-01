//! `CallPlatformApi`: plugin pass-through calls into a built-in adapter's platform API.

use std::sync::Arc;

use async_trait::async_trait;
use kanon_core::ipc::CoreApiService;
use kanon_core::supervisor::Supervisor;
use kanon_core::{AdapterError, Capability, PlatformAdapter};
use kanon_llm::tool_router::{json_to_prost_struct, prost_value_to_json};
use kanon_proto::v1::bot_api_service_server::BotApiService;
use kanon_proto::v1::{DeliverMessageRequest, DeliverMessageResponse, PlatformApiRequest};
use serde_json::json;
use tokio::sync::mpsc;
use tonic::{Code, Request};

/// Adapter whose API echoes the action and parameters back.
struct EchoApi;

#[async_trait]
impl PlatformAdapter for EchoApi {
    fn platform(&self) -> &str {
        "qq"
    }

    fn capabilities(&self) -> &[Capability] {
        &[Capability::PlatformApi]
    }

    async fn deliver(
        &self,
        _request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        Ok(DeliverMessageResponse::default())
    }

    async fn call_api(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AdapterError> {
        if action == "fail" {
            return Err(AdapterError::Api {
                platform: "qq".to_string(),
                action: action.to_string(),
                reason: "retcode 100".to_string(),
            });
        }
        Ok(json!({ "action": action, "params": params }))
    }
}

/// Adapter without a platform API.
struct NoApi;

#[async_trait]
impl PlatformAdapter for NoApi {
    fn platform(&self) -> &str {
        "plain"
    }

    async fn deliver(
        &self,
        _request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        Ok(DeliverMessageResponse::default())
    }
}

async fn service() -> CoreApiService {
    let dir = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(Supervisor::new(Some(dir.path().to_path_buf()), None));
    supervisor
        .adapters()
        .register(Arc::new(EchoApi))
        .await
        .unwrap();
    supervisor
        .adapters()
        .register(Arc::new(NoApi))
        .await
        .unwrap();
    let (event_tx, _event_rx) = mpsc::channel(4);
    CoreApiService::new(event_tx).with_supervisor(supervisor)
}

fn call(platform: &str, action: &str) -> Request<PlatformApiRequest> {
    Request::new(PlatformApiRequest {
        platform: platform.to_string(),
        action: action.to_string(),
        params: json_to_prost_struct(&json!({ "group_id": 42 })),
    })
}

#[tokio::test]
async fn a_call_reaches_the_adapter_and_returns_its_result() {
    let response = service()
        .await
        .call_platform_api(call("qq", "get_group_info"))
        .await
        .expect("call succeeds")
        .into_inner();
    assert_eq!(
        prost_value_to_json(response.result.expect("result")),
        json!({ "action": "get_group_info", "params": { "group_id": 42.0 } })
    );
}

#[tokio::test]
async fn failures_map_to_distinct_statuses() {
    let service = service().await;
    for (platform, action, code) in [
        ("qq", "../admin", Code::InvalidArgument),
        ("qq", "", Code::InvalidArgument),
        ("telegram", "get_me", Code::NotFound),
        ("plain", "get_me", Code::Unimplemented),
        ("qq", "fail", Code::Unavailable),
    ] {
        let status = service
            .call_platform_api(call(platform, action))
            .await
            .expect_err("call fails");
        assert_eq!(status.code(), code, "{platform}/{action}: {status:?}");
    }
}
