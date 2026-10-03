//! Gateway trust-boundary regressions for sandbox origins and remote authentication.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use kanon_api::{ApiServer, ApiState, StartupConfig};
use kanon_core::supervisor::Supervisor;
use std::sync::Arc;
use tower::ServiceExt;

/// Builds an isolated gateway with an optional remote-access credential.
fn state(dir: &std::path::Path, token: Option<&str>) -> ApiState {
    ApiState::builder(Arc::new(Supervisor::new(Some(dir.join("run")), None)))
        .with_config_dir(dir.join("data"))
        .with_startup(StartupConfig {
            api_token: token.map(str::to_string),
            ..Default::default()
        })
        .build()
}

/// Sends one request through the full production router.
async fn request(
    app: &axum::Router,
    path: &str,
    headers: &[(&str, &str)],
) -> axum::response::Response {
    let mut request = Request::builder().uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn management_rejects_opaque_cross_origin_and_rebinding_requests() {
    let dir = tempfile::tempdir().unwrap();
    let app = kanon_api::app(state(dir.path(), None));
    for path in ["/api/v1/providers", "/ws/v1/events", "/"] {
        for headers in [
            vec![("host", "localhost"), ("origin", "null")],
            vec![
                ("host", "localhost"),
                ("origin", "https://attacker.invalid"),
            ],
            vec![
                ("host", "attacker.invalid"),
                ("origin", "http://attacker.invalid"),
            ],
            vec![("host", "localhost"), ("sec-fetch-site", "cross-site")],
            vec![],
        ] {
            let response = request(&app, path, &headers).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{path}: {headers:?}"
            );
            assert!(
                !response
                    .headers()
                    .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            );
        }
    }
    for host in ["localhost", "127.0.0.1:8080", "[::1]:8080"] {
        let origin = format!("http://{host}");
        assert_eq!(
            request(
                &app,
                "/api/v1/providers",
                &[("host", host), ("origin", &origin)]
            )
            .await
            .status(),
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn remote_console_requires_basic_auth_and_still_blocks_opaque_management() {
    let dir = tempfile::tempdir().unwrap();
    let app = kanon_api::app(state(dir.path(), Some("secret")));
    let response = request(&app, "/", &[("host", "console.example")]).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        response.headers()[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .starts_with("Basic ")
    );
    let authorization = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("kanon:secret")
    );
    let headers = [
        ("host", "console.example"),
        ("authorization", authorization.as_str()),
        ("origin", "https://console.example"),
    ];
    assert_eq!(
        request(&app, "/api/v1/providers", &headers).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "/api/v1/providers",
            &[
                ("host", "console.example"),
                ("authorization", &authorization),
                ("origin", "null")
            ]
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn public_listener_requires_a_nonempty_token() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        ApiServer::bind("0.0.0.0:0".parse().unwrap(), state(dir.path(), None))
            .await
            .is_err()
    );
    assert!(
        ApiServer::bind("127.0.0.1:0".parse().unwrap(), state(dir.path(), Some(" ")))
            .await
            .is_err()
    );
    assert!(
        ApiServer::bind(
            "0.0.0.0:0".parse().unwrap(),
            state(dir.path(), Some("secret"))
        )
        .await
        .is_ok()
    );
}
