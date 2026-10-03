//! HTTP server bootstrap and router assembly.
//!
//! [`app`] builds the complete gateway router (REST + WebSocket + middleware) and is reused
//! verbatim by integration tests, so the tested surface is exactly the deployed surface.
//! [`ApiServer`] owns the bound listener separately from its serving future, which lets callers
//! discover the effective port (useful with port `0`) before traffic starts.

use std::future::Future;
use std::net::SocketAddr;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, header};
use axum::middleware::{self, Next};
use base64::Engine;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

use axum::http::Uri;
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

use crate::error::ApiError;
use crate::routes;
use crate::state::ApiState;
use crate::ws;

/// Static web assets embedded into the microkernel binary.
#[derive(RustEmbed)]
#[folder = "../../webui/dist/"]
struct WebUiAssets;

/// Fallback route handler that serves embedded static WebUI assets with SPA fallback,
/// while guaranteeing structured JSON 404 responses for unmatched `/api/` and `/ws/` paths.
async fn static_or_not_found(uri: Uri) -> Response {
    let path = uri.path();

    // Preserve JSON 404 envelope for any unmatched API or WebSocket routes
    if path.starts_with("/api/") || path.starts_with("/ws/") {
        return routes::not_found().await.into_response();
    }

    let trimmed = path.trim_start_matches('/');
    let target = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };

    if let Some(asset) = WebUiAssets::get(target) {
        let mime = mime_guess::from_path(target).first_or_octet_stream();
        let cache_control = if target.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };

        return (
            [
                (axum::http::header::CONTENT_TYPE, mime.as_ref()),
                (axum::http::header::CACHE_CONTROL, cache_control),
            ],
            asset.data,
        )
            .into_response();
    }

    // SPA fallback: client-side routing routes (e.g. /overview, /plugins, /settings)
    // resolve to index.html with 200 OK.
    if let Some(index) = WebUiAssets::get("index.html") {
        return (
            [
                (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (axum::http::header::CACHE_CONTROL, "no-cache"),
            ],
            index.data,
        )
            .into_response();
    }

    routes::not_found().await.into_response()
}

/// Builds the complete management gateway router.
///
/// Management calls require the console's origin. Plugin content has an opaque origin and
/// may access only the plugin web surface; CORS is scoped to those routes. A configured token
/// protects both the console document and API, using the browser's built-in HTTP authentication.
pub fn app(state: ApiState) -> Router {
    Router::new()
        .merge(routes::api_router())
        .merge(ws::routes())
        .fallback(static_or_not_found)
        .layer(middleware::from_fn_with_state(state.clone(), guard_access))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Enforces the browser trust boundary before handlers can observe a request.
async fn guard_access(State(state): State<ApiState>, mut request: Request, next: Next) -> Response {
    let denied = || {
        ApiError::Unauthorized("Management access requires a trusted origin and host".into())
            .into_response()
    };
    let Some(host) = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return denied();
    };
    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return denied();
    };
    let token = state.startup().api_token.as_deref();
    let hostname = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    // A local listener alone does not stop DNS rebinding: reject attacker-controlled Host names
    // before considering a same-origin request trusted. Remote deployments require a secret.
    if token.is_none()
        && hostname != "localhost"
        && !hostname
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    {
        return denied();
    }

    let mut segments = request.uri().path().split('/');
    let plugin_web = segments.next() == Some("")
        && segments.next() == Some("api")
        && segments.next() == Some("v1")
        && segments.next() == Some("plugins")
        && segments.next().is_some_and(|id| !id.is_empty())
        && matches!(segments.next(), Some("pages" | "http"));
    let origin = request.headers().get(header::ORIGIN);
    if let Some(origin) = origin {
        let trusted = origin.to_str().ok().is_some_and(|origin| {
            if plugin_web && origin == "null" {
                return true;
            }
            origin.parse::<Uri>().is_ok_and(|uri| {
                matches!(uri.scheme_str(), Some("http" | "https"))
                    && uri.authority().is_some_and(|value| value.as_str() == host)
                    && uri.path() == "/"
                    && uri.query().is_none()
            })
        });
        if !trusted {
            return denied();
        }
    } else if request
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|value| value == "cross-site")
    {
        return denied();
    }

    // Browser preflights never carry credentials. They only negotiate the narrow plugin CORS
    // surface; the actual request still has to authenticate. Management preflights are denied.
    let plugin_preflight = plugin_web
        && request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    if let Some(token) = token {
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("kanon:{token}"))
        );
        let actual = request
            .headers()
            .get(header::AUTHORIZATION)
            .map(|value| value.as_bytes())
            .unwrap_or_default();
        let authenticated = actual.len() == expected.len()
            && actual
                .iter()
                .zip(expected.as_bytes())
                .fold(0u8, |difference, (left, right)| difference | (left ^ right))
                == 0;
        if !plugin_preflight && !authenticated {
            let mut response = ApiError::Unauthorized("Management credentials are required".into())
                .into_response();
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"Kanon\", charset=\"UTF-8\""),
            );
            return response;
        }
        // The shared gateway credential must never be exposed to a plugin's HTTP handler.
        request.headers_mut().remove(header::AUTHORIZATION);
    }
    next.run(request).await
}

/// Bound management gateway awaiting its serving future.
pub struct ApiServer {
    listener: TcpListener,
    state: ApiState,
    local_addr: SocketAddr,
}

impl ApiServer {
    /// Binds the gateway to `addr` without starting to serve.
    pub async fn bind(addr: SocketAddr, state: ApiState) -> Result<Self, ApiError> {
        if state
            .startup()
            .api_token
            .as_ref()
            .is_some_and(|token| token.trim().is_empty())
        {
            return Err(ApiError::BadRequest(
                "startup.api_token must not be empty".into(),
            ));
        }
        if !addr.ip().is_loopback() && state.startup().api_token.is_none() {
            return Err(ApiError::BadRequest(
                "A non-loopback API listener requires startup.api_token".into(),
            ));
        }
        let listener = TcpListener::bind(addr).await.map_err(|err| {
            ApiError::Internal(format!(
                "Failed to bind management gateway on {addr}: {err}"
            ))
        })?;

        let local_addr = listener
            .local_addr()
            .map_err(|err| ApiError::Internal(format!("Failed to read bound address: {err}")))?;

        Ok(Self {
            listener,
            state,
            local_addr,
        })
    }

    /// Returns the effective bound address (resolved even when port `0` was requested).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Serves requests until `shutdown` resolves, then drains in-flight connections.
    pub async fn run(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), ApiError> {
        let router = app(self.state);
        let addr = self.local_addr;

        tracing::info!(address = %addr, "Management gateway listening");

        axum::serve(self.listener, router)
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(|err| ApiError::Internal(format!("Management gateway failed: {err}")))
    }
}
