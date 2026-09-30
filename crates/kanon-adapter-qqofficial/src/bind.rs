//! QR-code credential binding through the QQ Open Platform (`q.qq.com`).
//!
//! The operator scans a QR code with mobile QQ and picks (or creates) a bot; the platform then
//! returns the bot's AppID and its AppSecret encrypted with a one-time AES-256-GCM key that only
//! this node knows, so the secret never travels in the clear.

use std::time::Duration;

use base64::Engine;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};

/// Production binding service.
pub const BIND_BASE: &str = "https://q.qq.com";

/// How often a console should poll a pending task.
pub const POLL_INTERVAL_SECONDS: u64 = 2;

const TIMEOUT: Duration = Duration::from_secs(10);

/// A created binding task.
#[derive(Debug, Clone)]
pub struct LoginTask {
    /// Identifier to poll.
    pub task_id: String,
    /// Base64 AES-256 key that decrypts the returned secret; only the caller holds it.
    pub bind_key: String,
    /// Page mobile QQ opens from the QR code.
    pub qrcode_url: String,
}

/// State of a binding task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginStatus {
    /// Not scanned or not confirmed yet; carries QQ's raw status code.
    Pending(i64),
    /// The QR code expired; a new task is needed.
    Expired,
    /// The operator confirmed; the credentials are decrypted.
    Bound {
        /// Bot AppID.
        app_id: String,
        /// Bot AppSecret.
        secret: String,
    },
}

/// Creates a binding task on `base` (normally [`BIND_BASE`]).
pub async fn request_login(base: &str) -> Result<LoginTask, String> {
    let mut key = [0u8; 32];
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| "cannot generate a binding key")?;
    let bind_key = base64::engine::general_purpose::STANDARD.encode(key);

    let data = post(base, "/lite/create_bind_task", json!({"key": bind_key})).await?;
    let task_id = data["task_id"]
        .as_str()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or("QQ binding service returned no task_id")?
        .to_owned();
    let qrcode_url = reqwest::Url::parse_with_params(
        &format!("{base}/qqbot/openclaw/connect.html"),
        &[("task_id", task_id.as_str()), ("_wv", "2")],
    )
    .map_err(|err| format!("invalid binding service URL: {err}"))?
    .to_string();
    Ok(LoginTask {
        task_id,
        bind_key,
        qrcode_url,
    })
}

/// Polls a binding task once, decrypting the secret when the operator has confirmed.
pub async fn poll_login(base: &str, task_id: &str, bind_key: &str) -> Result<LoginStatus, String> {
    let data = post(base, "/lite/poll_bind_result", json!({"task_id": task_id})).await?;
    match data["status"].as_i64().unwrap_or(0) {
        2 => {
            let app_id = data["bot_appid"]
                .as_str()
                .map(str::trim)
                .unwrap_or_default();
            let encrypted = data["bot_encrypt_secret"]
                .as_str()
                .map(str::trim)
                .unwrap_or_default();
            if app_id.is_empty() || encrypted.is_empty() {
                return Err("binding completed but QQ returned incomplete credentials".into());
            }
            Ok(LoginStatus::Bound {
                app_id: app_id.to_owned(),
                secret: decrypt_secret(encrypted, bind_key)?,
            })
        }
        3 => Ok(LoginStatus::Expired),
        status => Ok(LoginStatus::Pending(status)),
    }
}

/// Decrypts `nonce(12) ‖ ciphertext ‖ tag(16)`, base64-encoded, with the task's key.
pub fn decrypt_secret(encrypted: &str, bind_key: &str) -> Result<String, String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let key = engine
        .decode(bind_key)
        .map_err(|_| "binding key is not base64")?;
    let raw = engine
        .decode(encrypted)
        .map_err(|_| "encrypted secret is not base64")?;
    if raw.len() <= 12 + 16 {
        return Err("encrypted secret is too short".into());
    }
    let key = UnboundKey::new(&AES_256_GCM, &key).map_err(|_| "binding key must be 32 bytes")?;
    let (nonce, sealed) = raw.split_at(12);
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| "invalid nonce")?;
    let mut buffer = sealed.to_vec();
    let plain = LessSafeKey::new(key)
        .open_in_place(nonce, Aad::empty(), &mut buffer)
        .map_err(|_| "secret failed authentication; wrong binding key")?;
    String::from_utf8(plain.to_vec()).map_err(|_| "decrypted secret is not UTF-8".into())
}

/// Calls the binding service and returns its `data` object.
async fn post(base: &str, path: &str, body: Value) -> Result<Value, String> {
    let response = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .timeout(TIMEOUT)
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|err| format!("cannot reach the QQ binding service: {err}"))?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|err| {
        format!("QQ binding service returned invalid JSON (HTTP {status}): {err}")
    })?;
    let retcode = body["retcode"].as_i64().unwrap_or(0);
    if !status.is_success() || retcode != 0 {
        let message = body["msg"]
            .as_str()
            .or_else(|| body["message"].as_str())
            .unwrap_or("request failed");
        return Err(format!(
            "QQ binding service error (HTTP {status}, retcode {retcode}): {message}"
        ));
    }
    Ok(body["data"].clone())
}
