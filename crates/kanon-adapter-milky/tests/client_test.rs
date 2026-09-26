//! Typed client behaviour against a real HTTP endpoint.
//!
//! The fake implementation speaks the protocol's envelope, so these tests pin the wire contract
//! rather than a mock's expectations: the `Bearer` header, the empty-object body of parameterless
//! endpoints, envelope unwrapping, and the exact error reported for each failure class.

mod common;

use common::{FakeMilky, Transport};
use kanon_adapter_milky::client::{MilkyClient, MilkyError};
use kanon_adapter_milky::config::MilkyConfig;
use kanon_adapter_milky::protocol::{
    GetHistoryMessagesInput, OutgoingSegment, SendGroupMessageInput,
};
use kanon_core::AdapterError;

/// Builds a client pointed at a fake implementation, optionally with an access token.
fn client_for(fake: &FakeMilky, token: Option<&str>) -> MilkyClient {
    let config = MilkyConfig {
        enabled: true,
        base_url: fake.base_url(),
        access_token: token.map(str::to_string),
        ..MilkyConfig::default()
    }
    .prepare()
    .expect("test configuration should be valid");

    MilkyClient::new(config).expect("client should build")
}

/// A successful call unwraps the `data` payload.
#[tokio::test]
async fn successful_call_unwraps_data() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let client = client_for(&fake, None);

    let login = client
        .get_login_info()
        .await
        .expect("login information should be returned");

    assert_eq!(login.uin, 10001);
    assert_eq!(login.nickname, "Kanon Test");
}

/// A non-zero `retcode` is surfaced with its own reason, never treated as an empty success.
#[tokio::test]
async fn failure_envelope_reports_retcode_and_message() {
    let fake = FakeMilky::start(Transport::Sse).await;
    fake.fail_with_retcode(-403, "not logged in");
    let client = client_for(&fake, None);

    let error = client
        .get_login_info()
        .await
        .expect_err("a failed envelope must be an error");

    match error {
        MilkyError::Api {
            endpoint,
            retcode,
            message,
        } => {
            assert_eq!(endpoint, "get_login_info");
            assert_eq!(retcode, -403);
            assert_eq!(message, "not logged in");
        }
        other => panic!("expected an API failure, got {other:?}"),
    }
}

/// An HTTP status other than 200 is reported as an HTTP failure, not a decode failure.
#[tokio::test]
async fn non_200_status_is_reported_as_http_failure() {
    let fake = FakeMilky::start(Transport::Sse).await;
    fake.fail_with_status(401);
    let client = client_for(&fake, None);

    let error = client
        .get_login_info()
        .await
        .expect_err("HTTP 401 must be an error");

    match error {
        MilkyError::Http { endpoint, status } => {
            assert_eq!(endpoint, "get_login_info");
            assert_eq!(status, 401);
        }
        other => panic!("expected an HTTP failure, got {other:?}"),
    }
}

/// A value-returning endpoint that omits `data` is a protocol violation, not an empty result.
#[tokio::test]
async fn missing_data_on_a_value_endpoint_is_a_payload_error() {
    let fake = FakeMilky::start(Transport::Sse).await;
    fake.fail_with_missing_data();
    let client = client_for(&fake, None);

    let error = client
        .get_login_info()
        .await
        .expect_err("a missing payload must be an error");

    assert!(matches!(error, MilkyError::Payload(_)));
    assert!(error.to_string().contains("no data"));
}

/// A void endpoint tolerates an implementation that omits `data`, as the protocol allows.
#[tokio::test]
async fn void_call_tolerates_missing_data() {
    let fake = FakeMilky::start(Transport::Sse).await;
    fake.fail_with_missing_data();
    let client = client_for(&fake, None);

    client
        .mark_message_as_read(&kanon_adapter_milky::protocol::MarkMessageAsReadInput {
            message_scene: "group".to_string(),
            peer_id: 30003,
            message_seq: 1,
        })
        .await
        .expect("a void endpoint should accept an empty payload");
}

/// The configured token is sent as the exact `Authorization` header the protocol requires.
#[tokio::test]
async fn access_token_is_sent_as_bearer_header() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let client = client_for(&fake, Some("s3cret"));

    client.get_login_info().await.expect("call should succeed");

    let call = fake.call("get_login_info");
    assert_eq!(call.authorization.as_deref(), Some("Bearer s3cret"));
}

/// Without a token no `Authorization` header is sent at all.
#[tokio::test]
async fn absent_token_sends_no_authorization_header() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let client = client_for(&fake, None);

    client.get_login_info().await.expect("call should succeed");

    assert!(fake.call("get_login_info").authorization.is_none());
}

/// A parameterless endpoint still posts an empty JSON object, as the protocol mandates.
#[tokio::test]
async fn parameterless_endpoint_posts_an_empty_object() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let client = client_for(&fake, None);

    client.get_impl_info().await.expect("call should succeed");

    assert_eq!(fake.call("get_impl_info").body, serde_json::json!({}));
}

/// Typed inputs are serialized into the exact parameter object the protocol defines.
#[tokio::test]
async fn typed_input_is_serialized() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let client = client_for(&fake, None);

    let response = client
        .send_group_message(&SendGroupMessageInput {
            group_id: 30003,
            message: vec![OutgoingSegment::Text("hi".to_string())],
        })
        .await
        .expect("send should succeed");

    assert_eq!(response.message_seq, 4242);
    assert_eq!(response.time, 1_700_000_000);
    assert_eq!(
        fake.call("send_group_message").body,
        serde_json::json!({
            "group_id": 30003,
            "message": [ { "type": "text", "data": { "text": "hi" } } ],
        })
    );
}

/// Optional parameters are omitted rather than sent as null.
#[tokio::test]
async fn optional_parameters_are_omitted() {
    let fake = FakeMilky::start(Transport::Sse).await;
    let client = client_for(&fake, None);

    client
        .get_history_messages(&GetHistoryMessagesInput {
            message_scene: "group".to_string(),
            peer_id: 30003,
            start_message_seq: None,
            limit: 20,
        })
        .await
        .expect("call should succeed");

    client
        .get_history_messages(&GetHistoryMessagesInput {
            message_scene: "group".to_string(),
            peer_id: 30003,
            start_message_seq: Some(10),
            limit: 20,
        })
        .await
        .expect("call should succeed");

    let bodies: Vec<serde_json::Value> = fake
        .calls()
        .into_iter()
        .filter(|call| call.endpoint == "get_history_messages")
        .map(|call| call.body)
        .collect();

    assert_eq!(
        bodies,
        vec![
            serde_json::json!({
                "message_scene": "group", "peer_id": 30003, "limit": 20,
            }),
            serde_json::json!({
                "message_scene": "group", "peer_id": 30003, "start_message_seq": 10, "limit": 20,
            }),
        ]
    );
}

/// An unreachable endpoint is reported as a transport failure.
#[tokio::test]
async fn unreachable_endpoint_is_a_transport_failure() {
    // Port 1 on loopback is not listening; the request must fail instead of hanging.
    let config = MilkyConfig {
        enabled: true,
        base_url: "http://127.0.0.1:1".to_string(),
        ..MilkyConfig::default()
    }
    .prepare()
    .expect("test configuration should be valid");
    let client = MilkyClient::new(config).expect("client should build");

    let error = client
        .get_login_info()
        .await
        .expect_err("an unreachable endpoint must fail");

    assert!(matches!(error, MilkyError::Transport(_)));
}

/// Client errors are translated into the core's delivery vocabulary with the platform attached.
#[tokio::test]
async fn client_errors_become_adapter_delivery_errors() {
    let fake = FakeMilky::start(Transport::Sse).await;
    fake.fail_with_retcode(-404, "friend not found");
    let client = client_for(&fake, None);

    let error = client
        .get_login_info()
        .await
        .expect_err("call should fail")
        .into_adapter_error("milky");

    match error {
        AdapterError::Delivery { platform, reason } => {
            assert_eq!(platform, "milky");
            assert!(reason.contains("-404"));
            assert!(reason.contains("friend not found"));
        }
        other => panic!("expected a delivery error, got {other:?}"),
    }
}
