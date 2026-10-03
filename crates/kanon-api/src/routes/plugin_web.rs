//! Plugin web surface: static console pages and HTTP routes served by the plugin itself.
//!
//! - `GET /api/v1/plugins/{id}/pages/{*path}` serves files from the plugin's `pages/` folder
//!   (see [`crate::plugin_files`] for the path rules). `/pages` redirects to `/pages/` so the
//!   page's relative links resolve inside the folder.
//! - `ANY /api/v1/plugins/{id}/http/{*path}` forwards the request to the plugin's host through
//!   `MessagePipelineService.OnHttpRequest`, for plugins whose `PluginMeta.serves_http` is set.
//!
//! # Why every response carries `Content-Security-Policy: sandbox`
//! Both surfaces are served from the management gateway's own origin. Without a sandbox, a page
//! or an HTML response from a plugin would run with the console's origin: it could read the
//! console's storage and drive it. The `sandbox` directive gives the document an opaque origin
//! even when it is opened directly rather than through the console's sandboxed frame, so plugin
//! content never runs as the console. Scripts, forms and popups stay allowed so a plugin page can
//! still be an application (it reaches its own routes with relative URLs such as `../http/...`).
//!
//! # Forwarding contract
//! The request body is buffered up to [`MAX_HTTP_BODY_BYTES`]: the whole request travels as one
//! gRPC message, and gRPC's default 4 MiB message limit leaves room for the headers. Hop-by-hop
//! headers (RFC 9110 §7.6.1) are connection-scoped and dropped in both directions; framing
//! (`Content-Length`) is recomputed by the gateway. CORS preflight (`OPTIONS`) is answered by the
//! gateway's CORS layer and never reaches the plugin.
//!
//! Status mapping: unknown plugin or no `serves_http` → `404`; plugin known but its host not
//! running (disabled, crashed, unreachable) → `503`; host error or malformed answer → `502`; no
//! answer within [`HTTP_FORWARD_TIMEOUT`] → `504`; body too large → `413`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get};
use futures_util::StreamExt;
use kanon_core::ManagedHost;
use kanon_proto::v1::{HttpHeader, HttpRequest};

use crate::error::ApiError;
use crate::plugin_files::{PageError, PageTarget, page_content_type, resolve_page};
use crate::routes::plugins::plugin_location;
use crate::state::ApiState;

/// Largest request body forwarded to a plugin.
pub const MAX_HTTP_BODY_BYTES: usize = 3 * 1024 * 1024;

/// How long the gateway waits for a plugin to answer a forwarded request.
pub const HTTP_FORWARD_TIMEOUT: Duration = Duration::from_secs(30);

/// Sandbox applied to everything a plugin serves (see the module docs).
pub const PLUGIN_CONTENT_POLICY: &str =
    "sandbox allow-scripts allow-forms allow-popups allow-modals allow-downloads";

/// Registers the plugin page and HTTP forwarding routes.
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/plugins/:id/pages", get(serve_page))
        .route("/api/v1/plugins/:id/pages/", get(serve_page))
        .route("/api/v1/plugins/:id/pages/*path", get(serve_page))
        .route("/api/v1/plugins/:id/http", any(forward_http))
        .route("/api/v1/plugins/:id/http/", any(forward_http))
        .route("/api/v1/plugins/:id/http/*path", any(forward_http))
        // Sandboxed documents send Origin: null. Credentialed requests keep Basic auth working
        // while these CORS grants never apply to management routes.
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(HeaderValue::from_static("null"))
                .allow_methods(tower_http::cors::AllowMethods::mirror_request())
                .allow_headers(tower_http::cors::AllowHeaders::mirror_request())
                .allow_credentials(true),
        )
}

/// Serves one file of a plugin's `pages/` folder.
async fn serve_page(
    State(state): State<ApiState>,
    AxumPath(params): AxumPath<HashMap<String, String>>,
    uri: Uri,
) -> Result<Response, ApiError> {
    let plugin_id = params.get("id").cloned().unwrap_or_default();
    // Decoded by the router, so `%2e%2e` arrives here as `..` and is rejected like it.
    let request_path = params.get("path").map(String::as_str).unwrap_or("");

    let location = plugin_location(&state, &plugin_id).await.ok_or_else(|| {
        ApiError::NotFound(format!(
            "Plugin '{plugin_id}' is not installed on this node"
        ))
    })?;

    let file = match resolve_page(&location.dir, request_path, uri.path().ends_with('/')) {
        Ok(PageTarget::File(file)) => file,
        Ok(PageTarget::Directory) => {
            let target = match uri.query() {
                Some(query) => format!("{}/?{query}", uri.path()),
                None => format!("{}/", uri.path()),
            };
            return Ok(Redirect::permanent(&target).into_response());
        }
        Err(PageError::Invalid(reason)) => return Err(ApiError::BadRequest(reason)),
        Err(PageError::NotFound) => {
            return Err(ApiError::NotFound(format!(
                "Plugin '{plugin_id}' has no page at '/{request_path}'"
            )));
        }
    };

    let body = tokio::fs::read(&file).await?;
    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&page_content_type(&file))
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    // Pages change with the plugin; an upgrade must show up on the next load.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    apply_content_policy(headers);
    Ok(response)
}

/// Adds the sandbox and MIME-sniffing protections to a plugin-served response.
fn apply_content_policy(headers: &mut HeaderMap) {
    headers.append(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PLUGIN_CONTENT_POLICY),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
}

/// Forwards one HTTP request to the plugin's host and relays its answer.
async fn forward_http(
    State(state): State<ApiState>,
    AxumPath(params): AxumPath<HashMap<String, String>>,
    request: Request,
) -> Result<Response, ApiError> {
    let plugin_id = params.get("id").cloned().unwrap_or_default();
    let host = http_host(&state, &plugin_id).await?;

    let (parts, body) = request.into_parts();
    if let Some(declared) = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        && declared > MAX_HTTP_BODY_BYTES as u64
    {
        return Err(too_large());
    }
    let body = read_limited(body).await?;

    let forwarded = HttpRequest {
        plugin_id: plugin_id.clone(),
        method: parts.method.as_str().to_ascii_uppercase(),
        path: sub_path(parts.uri.path()),
        query: parts.uri.query().unwrap_or_default().to_string(),
        headers: request_headers(&parts.headers)?,
        body: body.to_vec(),
    };

    let answer = tokio::time::timeout(HTTP_FORWARD_TIMEOUT, host.http_request(forwarded))
        .await
        .map_err(|_| {
            ApiError::Timeout(format!(
                "Plugin '{plugin_id}' did not answer within {}s",
                HTTP_FORWARD_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|status| match status.code() {
            tonic::Code::Unavailable => ApiError::Unavailable(format!(
                "The host of plugin '{plugin_id}' is not reachable: {}",
                status.message()
            )),
            tonic::Code::DeadlineExceeded => ApiError::Timeout(format!(
                "Plugin '{plugin_id}' did not answer in time: {}",
                status.message()
            )),
            _ => ApiError::Upstream(format!(
                "Plugin '{plugin_id}' failed to handle the request: {status}"
            )),
        })?;

    relay_response(&plugin_id, answer)
}

/// Finds the host that serves `plugin_id`'s HTTP routes.
async fn http_host(state: &ApiState, plugin_id: &str) -> Result<Arc<ManagedHost>, ApiError> {
    let Some(host) = state.supervisor().find_host_for_plugin(plugin_id).await else {
        // Without a host the plugin's metadata is unknown, so only "is it installed at all"
        // can be answered: an installed plugin is temporarily down, anything else does not exist.
        return Err(if plugin_location(state, plugin_id).await.is_some() {
            ApiError::Unavailable(format!(
                "Plugin '{plugin_id}' is not running (disabled, not started or failed to launch)"
            ))
        } else {
            ApiError::NotFound(format!("No plugin '{plugin_id}' is installed on this node"))
        });
    };

    let serves_http = host
        .metas()
        .iter()
        .any(|meta| meta.id == plugin_id && meta.serves_http);
    if !serves_http {
        return Err(ApiError::NotFound(format!(
            "Plugin '{plugin_id}' does not serve HTTP routes"
        )));
    }

    let health = host.health().await;
    if health.state == "crashed" || health.state == "restarting" {
        return Err(ApiError::Unavailable(format!(
            "The host of plugin '{plugin_id}' is {}",
            health.state
        )));
    }
    Ok(host)
}

/// Reads the request body, refusing it once it passes the limit.
///
/// Read chunk by chunk rather than trusting `Content-Length`, which a chunked request lacks.
async fn read_limited(body: Body) -> Result<Bytes, ApiError> {
    let mut stream = body.into_data_stream();
    let mut buffer = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| {
            ApiError::BadRequest(format!("Failed to read the request body: {err}"))
        })?;
        if buffer.len() + chunk.len() > MAX_HTTP_BODY_BYTES {
            return Err(too_large());
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(buffer))
}

/// Error for a request body over the forwarding limit.
fn too_large() -> ApiError {
    ApiError::PayloadTooLarge(format!(
        "Plugin requests are limited to {MAX_HTTP_BODY_BYTES} bytes"
    ))
}

/// Extracts the path below `/api/v1/plugins/<id>/http` from the raw request path.
///
/// The raw (still percent-encoded) form is passed on, as any HTTP server would see it; the
/// plugin id segment is skipped by position, so ids containing encoded characters work too.
fn sub_path(raw: &str) -> String {
    // ["", "api", "v1", "plugins", "<id>", "http", "<rest>"]
    match raw.splitn(7, '/').nth(6) {
        Some(rest) => format!("/{rest}"),
        None => "/".to_string(),
    }
}

/// Headers that describe one connection rather than the message (RFC 9110 §7.6.1), plus the
/// framing the gateway recomputes itself.
const HOP_BY_HOP: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Whether `name` must not cross the gateway, including headers the `Connection` header names.
fn is_hop_by_hop(name: &HeaderName, connection_listed: &[String]) -> bool {
    let name = name.as_str();
    HOP_BY_HOP.contains(&name) || connection_listed.iter().any(|listed| listed == name)
}

/// Header names listed in `Connection`, lower-cased.
fn connection_listed(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Converts the end-to-end request headers for the plugin.
///
/// The contract carries header values as strings; a value that is not UTF-8 is refused rather
/// than silently altered.
fn request_headers(headers: &HeaderMap) -> Result<Vec<HttpHeader>, ApiError> {
    let listed = connection_listed(headers);
    headers
        .iter()
        .filter(|(name, _)| !is_hop_by_hop(name, &listed))
        .map(|(name, value)| {
            let value = std::str::from_utf8(value.as_bytes())
                .map_err(|_| ApiError::BadRequest(format!("Header '{name}' is not valid UTF-8")))?;
            Ok(HttpHeader {
                name: name.as_str().to_string(),
                value: value.to_string(),
            })
        })
        .collect()
}

/// Turns the plugin's answer into the gateway's response.
fn relay_response(
    plugin_id: &str,
    answer: kanon_proto::v1::HttpResponse,
) -> Result<Response, ApiError> {
    // The contract treats an unset status as 200.
    let code = if answer.status == 0 {
        200
    } else {
        answer.status
    };
    let status = u16::try_from(code)
        .ok()
        .and_then(|code| StatusCode::from_u16(code).ok())
        // An informational status is not a final answer.
        .filter(|status| !status.is_informational())
        .ok_or_else(|| {
            ApiError::Upstream(format!(
                "Plugin '{plugin_id}' answered with the invalid HTTP status {code}"
            ))
        })?;

    let mut headers = HeaderMap::new();
    for entry in &answer.headers {
        let name = HeaderName::from_bytes(entry.name.trim().as_bytes()).map_err(|_| {
            ApiError::Upstream(format!(
                "Plugin '{plugin_id}' answered with the invalid header name '{}'",
                entry.name
            ))
        })?;
        let value = HeaderValue::from_str(&entry.value).map_err(|_| {
            ApiError::Upstream(format!(
                "Plugin '{plugin_id}' answered with an invalid value for header '{name}'"
            ))
        })?;
        headers.append(name, value);
    }
    let listed = connection_listed(&headers);
    let names: Vec<HeaderName> = headers.keys().cloned().collect();
    for name in names {
        if is_hop_by_hop(&name, &listed) || name == header::CONTENT_LENGTH {
            headers.remove(&name);
        }
    }
    apply_content_policy(&mut headers);

    let mut response = Response::new(Body::from(answer.body));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}
