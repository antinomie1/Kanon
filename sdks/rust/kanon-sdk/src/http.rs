//! HTTP routes a plugin serves under the management gateway
//! (`/api/v1/plugins/<plugin_id>/http/<path>`), declared with
//! [`Router::http_route`](crate::router::Router::http_route).
//!
//! ```ignore
//! router.http_route("GET", "/status", |_req| async move { Ok(json!({ "ok": true })) })
//! ```
//!
//! Handlers return anything [`IntoResponse`]: a [`Response`], a `serde_json::Value` (sent as
//! JSON), or text. Routing is by exact method and path; a path no route declares answers 404,
//! and a declared path with another method answers 405.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use kanon_proto::v1::{HttpHeader, HttpRequest, HttpResponse};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::plugin::PluginResult;

/// An HTTP request forwarded by the core.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// Upper-case method, e.g. `GET`.
    pub method: String,
    /// Path below the plugin's route root, starting with `/`.
    pub path: String,
    /// Raw query string without the leading `?`; empty when absent.
    pub query: String,
    /// Headers in arrival order; names as the client sent them.
    pub headers: Vec<(String, String)>,
    /// The raw body.
    pub body: Vec<u8>,
}

impl Request {
    /// The first header called `name`, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The query string's `key=value` pairs, percent-decoded (`+` is a space).
    pub fn query_pairs(&self) -> Vec<(String, String)> {
        self.query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                (percent_decode(key), percent_decode(value))
            })
            .collect()
    }

    /// The first query parameter called `name`, percent-decoded.
    pub fn query_param(&self, name: &str) -> Option<String> {
        self.query_pairs()
            .into_iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    /// The body as UTF-8 text.
    pub fn text(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.body)
    }

    /// The body decoded as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }
}

/// An HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Status code.
    pub status: u16,
    /// Headers to send.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Response {
    /// An empty response with `status`.
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// `200 OK` with a `text/plain` body.
    pub fn text(body: impl Into<String>) -> Self {
        Self::new(200)
            .header("content-type", "text/plain; charset=utf-8")
            .body(body.into().into_bytes())
    }

    /// `200 OK` with `value` as a JSON body; fails only when `value` cannot be serialized (e.g.
    /// a map with non-string keys).
    pub fn json<T: Serialize + ?Sized>(value: &T) -> Result<Self, serde_json::Error> {
        Ok(Self::new(200)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(value)?))
    }

    /// `404 Not Found` with a JSON error body.
    pub fn not_found() -> Self {
        error(404, "not found")
    }

    /// Replaces the status code.
    pub fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    /// Adds a header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Replaces the body.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
}

/// A JSON error response: `{"error": message}`.
fn error(status: u16, message: &str) -> Response {
    Response::new(status)
        .header("content-type", "application/json")
        .body(serde_json::json!({ "error": message }).to_string())
}

/// What an HTTP handler may return.
pub trait IntoResponse {
    /// The response to send.
    fn into_response(self) -> Response;
}

impl IntoResponse for Response {
    fn into_response(self) -> Response {
        self
    }
}

impl IntoResponse for serde_json::Value {
    fn into_response(self) -> Response {
        Response::new(200)
            .header("content-type", "application/json")
            .body(self.to_string())
    }
}

impl IntoResponse for String {
    fn into_response(self) -> Response {
        Response::text(self)
    }
}

impl IntoResponse for &'static str {
    fn into_response(self) -> Response {
        Response::text(self)
    }
}

/// A boxed route handler.
type Handler = Arc<
    dyn Fn(Request) -> Pin<Box<dyn Future<Output = PluginResult<Response>> + Send>> + Send + Sync,
>;

/// The routes of one plugin.
#[derive(Default, Clone)]
pub(crate) struct Routes {
    routes: Vec<(String, String, Handler)>,
}

impl Routes {
    /// Declares `method path`.
    ///
    /// # Panics
    /// On a malformed method or path, or a route declared twice — mistakes to fix at startup.
    pub(crate) fn add<F, Fut, R>(&mut self, method: &str, path: &str, handler: F)
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PluginResult<R>> + Send + 'static,
        R: IntoResponse + 'static,
    {
        let method = method.to_ascii_uppercase();
        assert!(
            !method.is_empty() && method.bytes().all(|byte| byte.is_ascii_uppercase()),
            "HTTP route method '{method}' must be a word such as GET or POST"
        );
        assert!(
            path.starts_with('/'),
            "HTTP route path '{path}' must start with '/'"
        );
        assert!(
            !self
                .routes
                .iter()
                .any(|(m, p, _)| *m == method && p == path),
            "HTTP route {method} {path} is already declared"
        );
        let handler: Handler = Arc::new(move |request| {
            let future = handler(request);
            Box::pin(async move { future.await.map(IntoResponse::into_response) })
        });
        self.routes.push((method, path.to_string(), handler));
    }

    /// Serves `request`: the matching route's response, 404 for an unknown path, 405 for a
    /// known path with another method, and 500 when the handler fails.
    pub(crate) async fn dispatch(&self, request: HttpRequest) -> HttpResponse {
        let request = Request {
            method: request.method.to_ascii_uppercase(),
            path: request.path,
            query: request.query,
            headers: request
                .headers
                .into_iter()
                .map(|header| (header.name, header.value))
                .collect(),
            body: request.body,
        };
        let on_path: Vec<_> = self
            .routes
            .iter()
            .filter(|(_, path, _)| *path == request.path)
            .collect();
        let response = match on_path
            .iter()
            .find(|(method, _, _)| *method == request.method)
        {
            Some((_, _, handler)) => {
                let route = format!("{} {}", request.method, request.path);
                match handler(request).await {
                    Ok(response) => response,
                    // The error stays in the host's log: it may name files, keys or internals,
                    // and anyone who can reach the console port can call routes.
                    Err(err) => {
                        tracing::warn!(%route, error = %err, "HTTP handler failed");
                        error(500, "internal error")
                    }
                }
            }
            None if on_path.is_empty() => Response::not_found(),
            None => {
                let allowed: Vec<&str> = on_path.iter().map(|(m, _, _)| m.as_str()).collect();
                error(405, "method not allowed").header("allow", allowed.join(", "))
            }
        };
        into_wire(response)
    }
}

/// Converts a response into the wire message.
pub(crate) fn into_wire(response: Response) -> HttpResponse {
    HttpResponse {
        status: u32::from(response.status),
        headers: response
            .headers
            .into_iter()
            .map(|(name, value)| HttpHeader { name, value })
            .collect(),
        body: response.body,
    }
}

/// Decodes `%XX` escapes and `+` (a space in query strings). A malformed escape is kept as
/// written, and bytes that do not form UTF-8 are replaced, as browsers do.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' => {
                // `from_str_radix` alone would accept a sign ("%+1"), which is not an escape.
                let hex = input
                    .get(index + 1..index + 3)
                    .filter(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                match hex {
                    Some(byte) => {
                        decoded.push(byte);
                        index += 2;
                    }
                    None => decoded.push(b'%'),
                }
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
