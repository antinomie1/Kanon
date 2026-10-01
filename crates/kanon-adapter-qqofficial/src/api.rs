//! QQ Open Platform REST client: app access token, gateway discovery and message sending.
//!
//! One [`Api`] is bound to one set of credentials. Reconfiguring the adapter builds a new one,
//! so a delivery that pinned the old client can never send with a half-updated identity.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::Mutex;

/// Upper bound for ordinary API calls.
const API_TIMEOUT: Duration = Duration::from_secs(30);
/// Media uploads carry the whole file inline, so they get a longer deadline.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// The most bytes one media upload carries.
///
/// QQ takes a single `url` / `file_data` upload of up to 20 MiB; bigger media needs the chunked
/// upload (`upload_prepare` and its parts), which this adapter does not implement.
pub const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;
/// A cached token is refreshed this long before QQ says it expires, so a request never races
/// the expiry.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);

/// Where the adapter reaches the QQ Open Platform.
///
/// Not part of the persisted configuration: production and sandbox are the only real targets,
/// and tests point it at a local mock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// App access token endpoint.
    pub token_url: String,
    /// REST API base URL, without a trailing slash.
    pub api_base: String,
}

impl Endpoints {
    /// The official production or sandbox endpoints.
    pub fn official(sandbox: bool) -> Self {
        Self {
            token_url: "https://bots.qq.com/app/getAppAccessToken".into(),
            api_base: if sandbox {
                "https://sandbox.api.sgroup.qq.com".into()
            } else {
                "https://api.sgroup.qq.com".into()
            },
        }
    }
}

/// Which kind of media a group or C2C upload carries (`file_type` in the QQ API).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// `file_type = 1`.
    Image = 1,
    /// `file_type = 2`.
    Video = 2,
    /// `file_type = 3`; QQ only plays SILK audio.
    Voice = 3,
    /// `file_type = 4`, a named file.
    File = 4,
}

/// Where uploaded media comes from.
pub enum MediaSource {
    /// A public URL QQ fetches itself.
    Url(String),
    /// File contents, sent inline as base64 `file_data`.
    Bytes(Vec<u8>),
}

/// Authenticated REST client for one bot.
pub struct Api {
    http: reqwest::Client,
    endpoints: Endpoints,
    app_id: String,
    secret: String,
    /// Cached access token and the instant it stops being usable. The mutex also makes
    /// concurrent callers share one refresh instead of each fetching a token.
    token: Mutex<Option<(String, Instant)>>,
}

impl Api {
    /// Builds a client; no network I/O happens until the first call.
    pub fn new(endpoints: Endpoints, app_id: String, secret: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            endpoints,
            app_id,
            secret,
            token: Mutex::new(None),
        }
    }

    /// AppID this client authenticates as.
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// Returns a valid access token, fetching a new one when the cached one is about to expire.
    pub async fn token(&self) -> Result<String, String> {
        let mut cached = self.token.lock().await;
        if let Some((token, valid_until)) = cached.as_ref()
            && Instant::now() < *valid_until
        {
            return Ok(token.clone());
        }

        let response = self
            .http
            .post(&self.endpoints.token_url)
            .timeout(API_TIMEOUT)
            .json(&json!({"appId": self.app_id, "clientSecret": self.secret}))
            .send()
            .await
            .map_err(|err| format!("cannot reach the QQ token endpoint: {err}"))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|err| format!("QQ token endpoint returned invalid JSON: {err}"))?;
        let token = body["access_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                format!(
                    "QQ refused the app credentials (HTTP {status}): {}",
                    error_text(&body)
                )
            })?
            .to_owned();
        // QQ sends `expires_in` as a decimal string; accept a number too.
        let lifetime = match &body["expires_in"] {
            Value::String(text) => text.parse::<u64>().ok(),
            value => value.as_u64(),
        }
        .ok_or("QQ token response has no valid expires_in")?;
        let valid_until =
            Instant::now() + Duration::from_secs(lifetime).saturating_sub(TOKEN_MARGIN);
        *cached = Some((token.clone(), valid_until));
        Ok(token)
    }

    /// Drops the cached token, e.g. after the gateway rejected it.
    pub async fn invalidate_token(&self) {
        *self.token.lock().await = None;
    }

    /// Resolves the WebSocket gateway URL.
    pub async fn gateway_url(&self) -> Result<String, String> {
        let body = self.request(reqwest::Method::GET, "/gateway", None).await?;
        body["url"]
            .as_str()
            .filter(|url| !url.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "QQ /gateway response has no url".into())
    }

    /// Sends one message body to a group (`scene = "group"`) or a user (`"c2c"`).
    pub async fn send_v2(&self, scene: &str, target: &str, body: &Value) -> Result<Value, String> {
        let path = match scene {
            "group" => format!("/v2/groups/{target}/messages"),
            _ => format!("/v2/users/{target}/messages"),
        };
        self.request(reqwest::Method::POST, &path, Some(body)).await
    }

    /// Uploads media for a group or C2C target and returns its `file_info` handle.
    pub async fn upload(
        &self,
        scene: &str,
        target: &str,
        kind: MediaKind,
        source: MediaSource,
        file_name: Option<&str>,
    ) -> Result<String, String> {
        use base64::Engine;

        let path = match scene {
            "group" => format!("/v2/groups/{target}/files"),
            _ => format!("/v2/users/{target}/files"),
        };
        // `srv_send_msg = false`: the upload only yields a handle; the message that carries it
        // is sent separately so it can be a passive reply to the user's message.
        let mut body = json!({"file_type": kind as u8, "srv_send_msg": false});
        if kind == MediaKind::File {
            body["file_name"] = json!(file_name.unwrap_or("file"));
        }
        match source {
            MediaSource::Url(url) => body["url"] = json!(url),
            // Checked here so an oversized attachment fails with its size, not with whatever QQ
            // answers after receiving a 30 MiB request body.
            MediaSource::Bytes(bytes) if bytes.len() > MAX_UPLOAD_BYTES => {
                return Err(format!(
                    "attachment is {:.1} MiB; one QQ upload holds at most {} MiB",
                    bytes.len() as f64 / (1024.0 * 1024.0),
                    MAX_UPLOAD_BYTES >> 20
                ));
            }
            MediaSource::Bytes(bytes) => {
                body["file_data"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes))
            }
        }
        let response = self
            .request_with(reqwest::Method::POST, &path, Some(&body), UPLOAD_TIMEOUT)
            .await?;
        response["file_info"]
            .as_str()
            .filter(|info| !info.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "QQ media upload response has no file_info".into())
    }

    /// Downloads a public resource of at most `limit` bytes.
    ///
    /// The body streams in chunks and the download stops at the limit, so an unexpectedly large
    /// resource cannot fill memory before it is rejected. No QQ credentials are attached: the
    /// resource belongs to someone else.
    pub async fn download(&self, url: &str, limit: usize) -> Result<Vec<u8>, String> {
        let failed = |err: reqwest::Error| format!("cannot download {url}: {err}");
        let mut response = self
            .http
            .get(url)
            .timeout(UPLOAD_TIMEOUT)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(failed)?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(failed)? {
            if body.len() + chunk.len() > limit {
                return Err(format!(
                    "{url} is larger than the {} MiB one QQ upload holds",
                    limit >> 20
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// Posts a message to a guild channel (`/channels/{id}/messages`).
    pub async fn send_channel(&self, channel_id: &str, body: &Value) -> Result<Value, String> {
        let path = format!("/channels/{channel_id}/messages");
        self.request(reqwest::Method::POST, &path, Some(body)).await
    }

    /// Posts a guild direct message (`/dms/{guild_id}/messages`).
    pub async fn send_dm(&self, guild_id: &str, body: &Value) -> Result<Value, String> {
        let path = format!("/dms/{guild_id}/messages");
        self.request(reqwest::Method::POST, &path, Some(body)).await
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        self.request_with(method, path, body, API_TIMEOUT).await
    }

    /// Performs one authenticated call and turns every QQ failure shape into an error string.
    async fn request_with(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let token = self.token().await?;
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.endpoints.api_base))
            .timeout(timeout)
            .header("Authorization", format!("QQBot {token}"));
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|err| format!("QQ API {path} unreachable: {err}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|err| format!("QQ API {path} response unreadable: {err}"))?;
        let value: Value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).map_err(|_| {
                format!(
                    "QQ API {path} returned non-JSON (HTTP {status}): {}",
                    text.chars().take(200).collect::<String>()
                )
            })?
        };
        // Errors arrive as a non-2xx status, but some endpoints report a business failure with a
        // nonzero `code` in a 200 body; both must fail the call.
        let code = value["code"].as_i64().unwrap_or(0);
        if !status.is_success() || code != 0 {
            return Err(format!(
                "QQ API {path} failed (HTTP {status}, code {code}): {}",
                error_text(&value)
            ));
        }
        Ok(value)
    }
}

/// The human-readable part of a QQ error body.
fn error_text(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["msg"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| body.to_string())
}
