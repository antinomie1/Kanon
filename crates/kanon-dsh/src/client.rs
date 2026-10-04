//! Bounded, correlated DSH unary RPCs using the public Web Connection envelope.

use super::{DshConfig, DshError, MAX_WIRE_BYTES};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::AsyncReadExt;

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// One configured DSH endpoint. Clones reuse the HTTP connection pool, not a conversation store.
#[derive(Clone)]
pub struct DshClient {
    pub(super) config: DshConfig,
    http: reqwest::Client,
    pub(super) writers: std::sync::Arc<crate::reservation::Reservations>,
}

impl DshClient {
    /// Builds a lazy transport; an unused optional backend opens no connection or credential.
    pub fn new(config: DshConfig) -> Result<Self, DshError> {
        config.validate()?;
        let http = reqwest::Client::builder()
            .connect_timeout(config.request_timeout())
            .timeout(config.request_timeout())
            // A redirect must not forward a browser credential to a different deployment.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| DshError::Transport(e.to_string()))?;
        Ok(Self {
            config,
            http,
            writers: Default::default(),
        })
    }

    /// Connection settings only; remote agent settings are read with `settings/describe`.
    pub fn config(&self) -> &DshConfig {
        &self.config
    }

    /// Whether this endpoint has no admitted mutation or cleanup owner remaining.
    pub fn is_idle(&self) -> bool {
        self.writers.is_idle()
    }

    /// Waits for already admitted owners to finish remote cancellation and retirement.
    /// Admission must be closed by the caller first; this never retries remote mutations.
    pub async fn wait_idle(&self) {
        self.writers.wait_idle().await;
    }

    /// Calls one public unary method with its exact named argument object.
    ///
    /// No mutation is retried automatically: a lost response may follow an accepted commit.
    pub async fn call<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        args: Value,
    ) -> Result<T, DshError> {
        if endpoint.is_empty()
            || endpoint.split('/').any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$' | b'-'))
            })
            || !args.is_object()
        {
            return Err(DshError::Config(
                "RPC requires a method path and named argument object".into(),
            ));
        }
        let rpc_id = Self::request_id();
        let request = self
            .http
            .post(self.url(&format!("/api/{endpoint}")))
            .headers(self.headers().await?)
            .json(&json!({
                "type": "client-request", "rpcId": rpc_id, "method": endpoint,
                "payload": {"args": args}
            }));
        let mut response = request.send().await.map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(DshError::Transport(format!(
                "{endpoint}: HTTP {}",
                response.status()
            )));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_WIRE_BYTES as u64)
        {
            return Err(DshError::Protocol("RPC response exceeds size limit".into()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if body.len().saturating_add(chunk.len()) > MAX_WIRE_BYTES {
                return Err(DshError::Protocol("RPC response exceeds size limit".into()));
            }
            body.extend_from_slice(&chunk);
        }
        let value: Value =
            serde_json::from_slice(&body).map_err(|e| DshError::Protocol(e.to_string()))?;
        if value["type"] != "server-response" || value["rpcId"] != rpc_id {
            return Err(DshError::Protocol(
                "RPC envelope or correlation id mismatch".into(),
            ));
        }
        let result = &value["result"];
        match result["ok"].as_bool() {
            Some(true) if result.get("value").is_some() => {
                serde_json::from_value(result["value"].clone())
                    .map_err(|e| DshError::Protocol(e.to_string()))
            }
            Some(false) => Err(remote_error(&result["error"])?),
            _ => Err(DshError::Protocol("RPC has no result".into())),
        }
    }

    /// Reads the authoritative DSH settings, including schema, redactions and revisions.
    pub async fn settings(&self) -> Result<Value, DshError> {
        self.call("settings/describe", json!({})).await
    }

    /// Updates one DSH settings namespace with the revision returned by its owner.
    pub async fn update_settings(
        &self,
        namespace: &str,
        patch: Value,
        revision: u64,
    ) -> Result<Value, DshError> {
        if namespace.trim().is_empty() || !patch.is_object() {
            return Err(DshError::Config(
                "settings require a namespace and object patch".into(),
            ));
        }
        self.call(
            "settings/update",
            json!({"ns": namespace, "patch": patch, "expectedRevision": revision}),
        )
        .await
    }

    /// Reads DSH's model catalog; no Kanon provider or model configuration is consulted.
    pub async fn models(&self) -> Result<Value, DshError> {
        self.call("session/modelCatalog", json!({})).await
    }

    pub(super) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.config.base_url.trim_end_matches('/'))
    }

    /// Allocates an opaque prompt or RPC correlation id without reading configuration.
    pub fn request_id() -> String {
        // Correlation ids only need uniqueness within this process's pending requests. Including
        // a timestamp also keeps prompt deduplication distinct across node restarts.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!(
            "kanon-{}-{now}-{}",
            std::process::id(),
            NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
        )
    }

    pub(super) async fn headers(&self) -> Result<reqwest::header::HeaderMap, DshError> {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(path) = &self.config.cookie_file {
            let file = tokio::fs::File::open(path)
                .await
                .map_err(|e| DshError::Authentication(format!("cannot open cookie_file: {e}")))?;
            let mut bytes = Vec::new();
            file.take(16_385)
                .read_to_end(&mut bytes)
                .await
                .map_err(|e| DshError::Authentication(format!("cannot read cookie_file: {e}")))?;
            if bytes.len() > 16_384 {
                return Err(DshError::Authentication(
                    "cookie_file exceeds 16 KiB".into(),
                ));
            }
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| DshError::Authentication("cookie_file must be UTF-8".into()))?
                .trim();
            if text.is_empty() {
                return Err(DshError::Authentication("cookie_file is empty".into()));
            }
            let mut cookie = reqwest::header::HeaderValue::from_str(text).map_err(|_| {
                DshError::Authentication("cookie_file must contain one Cookie header value".into())
            })?;
            cookie.set_sensitive(true);
            headers.insert(reqwest::header::COOKIE, cookie);
        }
        Ok(headers)
    }
}

pub(super) fn remote_error(value: &Value) -> Result<DshError, DshError> {
    let code = value["code"]
        .as_str()
        .ok_or_else(|| DshError::Protocol("remote error has no code".into()))?;
    let message = value["message"]
        .as_str()
        .ok_or_else(|| DshError::Protocol("remote error has no message".into()))?;
    Ok(DshError::Remote {
        code: code.into(),
        message: message.into(),
    })
}

fn transport_error(error: reqwest::Error) -> DshError {
    if error.is_timeout() {
        DshError::Timeout("request")
    } else {
        DshError::Transport(error.without_url().to_string())
    }
}
