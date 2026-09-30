//! QR binding against a mock `q.qq.com`: task creation, polling and secret decryption.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use kanon_adapter_qqofficial::bind::{self, LoginStatus};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use serde_json::{Value, json};

/// Encrypts like the binding service: base64(nonce ‖ ciphertext ‖ tag).
fn seal(secret: &str, bind_key: &str) -> String {
    let engine = base64::engine::general_purpose::STANDARD;
    let key =
        LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &engine.decode(bind_key).unwrap()).unwrap());
    let nonce = [7u8; 12];
    let mut data = secret.as_bytes().to_vec();
    key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut data)
        .unwrap();
    engine.encode([nonce.as_slice(), &data].concat())
}

/// Binding service state: the key the node registered and the poll answer to give.
#[derive(Clone, Default)]
struct Service {
    key: Arc<Mutex<String>>,
    status: Arc<Mutex<i64>>,
}

async fn create(State(service): State<Service>, Json(body): Json<Value>) -> Json<Value> {
    *service.key.lock().unwrap() = body["key"].as_str().unwrap().to_owned();
    Json(json!({"retcode": 0, "data": {"task_id": "TASK 1"}}))
}

async fn poll(State(service): State<Service>, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["task_id"], "TASK 1");
    let status = *service.status.lock().unwrap();
    let key = service.key.lock().unwrap().clone();
    Json(json!({"retcode": 0, "data": {
        "status": status,
        "bot_appid": "102030",
        "bot_encrypt_secret": seal("the-secret", &key),
    }}))
}

#[tokio::test]
async fn login_task_polls_to_decrypted_credentials() {
    let service = Service::default();
    let app = Router::new()
        .route("/lite/create_bind_task", post(create))
        .route("/lite/poll_bind_result", post(poll))
        .with_state(service.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let task = bind::request_login(&base).await.unwrap();
    assert_eq!(task.task_id, "TASK 1");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&task.bind_key)
            .unwrap()
            .len(),
        32
    );
    assert_eq!(
        task.qrcode_url,
        format!("{base}/qqbot/openclaw/connect.html?task_id=TASK+1&_wv=2")
    );

    *service.status.lock().unwrap() = 1;
    assert_eq!(
        bind::poll_login(&base, &task.task_id, &task.bind_key)
            .await
            .unwrap(),
        LoginStatus::Pending(1)
    );

    *service.status.lock().unwrap() = 2;
    assert_eq!(
        bind::poll_login(&base, &task.task_id, &task.bind_key)
            .await
            .unwrap(),
        LoginStatus::Bound {
            app_id: "102030".into(),
            secret: "the-secret".into(),
        }
    );

    *service.status.lock().unwrap() = 3;
    assert_eq!(
        bind::poll_login(&base, &task.task_id, &task.bind_key)
            .await
            .unwrap(),
        LoginStatus::Expired
    );
}

#[test]
fn a_secret_sealed_for_another_key_is_rejected() {
    let engine = base64::engine::general_purpose::STANDARD;
    let key = engine.encode([1u8; 32]);
    let other = engine.encode([2u8; 32]);
    let sealed = seal("s", &key);
    assert_eq!(bind::decrypt_secret(&sealed, &key).unwrap(), "s");
    assert!(bind::decrypt_secret(&sealed, &other).is_err());
    assert!(bind::decrypt_secret("AAAA", &key).is_err());
}
