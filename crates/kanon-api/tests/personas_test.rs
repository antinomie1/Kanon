//! Persona library management: creating, editing and removing operator-defined personas.
//!
//! The contract the console depends on: the node ships one read-only base assistant; everything
//! else is the operator's, persisted to `data/personas.json`, applied to the running registry
//! immediately, and protected against removal while an instance still selects it.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::http::{Method, StatusCode};
use kanon_api::ApiState;
use kanon_llm::PersonaStore;
use serde_json::{Value, json};

use common::{error_code, send_json};

/// State whose persona document lives inside an isolated directory.
fn isolated_state(dir: PathBuf) -> (ApiState, Arc<PersonaStore>) {
    let temp = tempfile::tempdir().expect("temp dir");
    let supervisor = Arc::new(kanon_core::supervisor::Supervisor::new(
        Some(temp.path().to_path_buf()),
        None,
    ));
    std::mem::forget(temp);

    let store = Arc::new(PersonaStore::new(dir.join("personas.json")));
    let state = ApiState::builder(supervisor)
        .with_config_dir(dir)
        .with_persona_store(store.clone())
        .build();
    (state, store)
}

async fn create(app: &Router, body: Value) -> (StatusCode, Value) {
    send_json(app, Method::POST, "/api/v1/personas", Some(body)).await
}

fn ids(body: &Value) -> Vec<String> {
    body["personas"]
        .as_array()
        .expect("personas array")
        .iter()
        .map(|persona| persona["id"].as_str().expect("id").to_string())
        .collect()
}

#[tokio::test]
async fn a_created_persona_is_persisted_and_usable_immediately() {
    let dir = tempfile::tempdir().expect("dir");
    let (state, store) = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state.clone());

    let (status, body) = create(
        &app,
        json!({
            "name": "Code Reviewer",
            "description": "Reviews diffs",
            "prompt": "You review code.\r\n\r\nBe concise.  "
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    // The base assistant leads, then the new persona; the id was derived from the name.
    assert_eq!(ids(&body), vec!["assistant", "code-reviewer"]);
    let created = &body["personas"][1];
    assert_eq!(created["kind"], "custom");
    assert_eq!(created["prompt"], "You review code.\n\nBe concise.");

    // Applied to the running registry with no restart.
    let live = state.personas().get("code-reviewer").expect("registered");
    assert_eq!(live.prompt, "You review code.\n\nBe concise.");

    // Persisted, and only the custom persona: the base assistant is never written to disk.
    let reloaded = store.load().expect("reload");
    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded[0].id, "code-reviewer");
    assert_eq!(
        store.load_registry().expect("registry").len(),
        2,
        "a restart restores the base assistant plus the saved persona"
    );
}

#[tokio::test]
async fn ids_are_derived_uniquely_and_non_ascii_names_still_work() {
    let dir = tempfile::tempdir().expect("dir");
    let (state, _store) = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state);

    for (name, expected) in [
        ("Translator", "translator"),
        ("Translator", "translator-2"),
        ("translator!!", "translator-3"),
        ("猫娘助手", "persona"),
        ("狗狗助手", "persona-2"),
    ] {
        let (status, body) = create(&app, json!({ "name": name, "prompt": "p" })).await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert!(
            ids(&body).contains(&expected.to_string()),
            "expected '{expected}' in {:?}",
            ids(&body)
        );
    }
}

#[tokio::test]
async fn an_explicit_id_is_honoured_but_never_reused() {
    let dir = tempfile::tempdir().expect("dir");
    let (state, _store) = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state);

    let (status, _) = create(&app, json!({ "id": "sre", "name": "SRE", "prompt": "p" })).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = create(&app, json!({ "id": "sre", "name": "Other", "prompt": "q" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "unexpected body: {body}");

    // The built-in id is taken too.
    let (status, _) = create(
        &app,
        json!({ "id": "assistant", "name": "Mine", "prompt": "q" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn invalid_personas_are_rejected_before_anything_is_stored() {
    let dir = tempfile::tempdir().expect("dir");
    let (state, store) = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state.clone());

    for payload in [
        json!({ "name": "  ", "prompt": "p" }),
        json!({ "name": "n", "prompt": "  \n " }),
        json!({ "id": "Bad Id", "name": "n", "prompt": "p" }),
        json!({ "id": "instance:x", "name": "n", "prompt": "p" }),
    ] {
        let (status, body) = create(&app, payload.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}: {body}");
        assert_eq!(error_code(&body), "bad_request");
    }

    assert_eq!(state.personas().len(), 1);
    assert!(
        !store.path().exists(),
        "a rejected persona must not reach disk"
    );
}

#[tokio::test]
async fn editing_keeps_the_id_and_the_base_assistant_is_read_only() {
    let dir = tempfile::tempdir().expect("dir");
    let (state, store) = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state.clone());
    create(&app, json!({ "id": "sre", "name": "SRE", "prompt": "old" })).await;

    let (status, body) = send_json(
        &app,
        Method::PUT,
        "/api/v1/personas/sre",
        Some(json!({ "name": "Site Reliability", "description": "d", "prompt": "new" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(state.personas().get("sre").expect("sre").prompt, "new");
    assert_eq!(store.load().expect("load")[0].name, "Site Reliability");

    let (status, body) = send_json(
        &app,
        Method::PUT,
        "/api/v1/personas/assistant",
        Some(json!({ "name": "Hacked", "prompt": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "unexpected body: {body}");
    assert_eq!(
        state.personas().base().prompt,
        "You are a helpful assistant."
    );

    let (status, _) = send_json(
        &app,
        Method::PUT,
        "/api/v1/personas/ghost",
        Some(json!({ "name": "n", "prompt": "p" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn removing_a_persona_unbinds_sessions_but_is_refused_while_an_instance_uses_it() {
    let dir = tempfile::tempdir().expect("dir");
    let (state, store) = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state.clone());
    create(&app, json!({ "id": "sre", "name": "SRE", "prompt": "p" })).await;
    create(
        &app,
        json!({ "id": "spare", "name": "Spare", "prompt": "p" }),
    )
    .await;

    // An instance selecting the persona blocks its removal and is reported as the reason.
    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(json!({
            "name": "Ops Bot",
            "enabled": false,
            "adapters": [],
            "persona_id": "sre"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    let instance_id = body["instance"]["id"].as_str().expect("id").to_string();

    let (_, listed) = send_json(&app, Method::GET, "/api/v1/personas", None).await;
    let sre = listed["personas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|persona| persona["id"] == "sre")
        .expect("sre listed");
    assert_eq!(sre["used_by"], json!([instance_id]));

    let (status, body) = send_json(&app, Method::DELETE, "/api/v1/personas/sre", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "unexpected body: {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(&instance_id),
        "the error must name the instance: {body}"
    );
    assert!(state.personas().get("sre").is_some());

    // An unused persona goes, and sessions bound to it fall back to the base assistant.
    state.sessions().set_persona("chat:1", "spare");
    let (status, body) = send_json(&app, Method::DELETE, "/api/v1/personas/spare", None).await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(ids(&body), vec!["assistant", "sre"]);
    assert!(state.personas().get("spare").is_none());
    assert!(state.sessions().get_persona("chat:1").is_none());
    let persisted: Vec<String> = store
        .load()
        .expect("load")
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(persisted, vec!["sre"]);

    // The built-in and unknown personas cannot be removed.
    let (status, _) = send_json(&app, Method::DELETE, "/api/v1/personas/assistant", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = send_json(&app, Method::DELETE, "/api/v1/personas/ghost", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn instance_personas_are_listed_but_owned_by_their_instance() {
    let dir = tempfile::tempdir().expect("dir");
    let (state, _store) = isolated_state(dir.path().to_path_buf());
    let app = kanon_api::app(state);

    let (status, body) = send_json(
        &app,
        Method::POST,
        "/api/v1/instances",
        Some(json!({
            "name": "Pig Bot",
            "enabled": false,
            "adapters": [],
            "system_prompt": "you are a pig"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");

    let (_, listed) = send_json(&app, Method::GET, "/api/v1/personas", None).await;
    let generated = listed["personas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|persona| persona["kind"] == "instance")
        .expect("the instance prompt is published as a persona");
    let id = generated["id"].as_str().expect("id").to_string();

    // It can be selected like any other, but not edited or deleted here.
    let (status, _) = send_json(
        &app,
        Method::PUT,
        &format!("/api/v1/personas/{id}"),
        Some(json!({ "name": "n", "prompt": "p" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = send_json(
        &app,
        Method::DELETE,
        &format!("/api/v1/personas/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_malformed_or_invalid_document_fails_loudly() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("personas.json");
    let store = PersonaStore::new(&path);

    assert!(store.load().expect("missing file is empty").is_empty());

    std::fs::write(&path, "{ not json").expect("seed");
    assert!(
        store
            .load()
            .expect_err("malformed")
            .contains("Failed to parse")
    );

    std::fs::write(
        &path,
        r#"{"version":1,"personas":[{"id":"Bad Id","name":"n","prompt":"p"}]}"#,
    )
    .expect("seed");
    assert!(store.load().expect_err("invalid id").contains("Bad Id"));

    std::fs::write(&path, r#"{"version":9,"personas":[]}"#).expect("seed");
    assert!(
        store
            .load()
            .expect_err("version")
            .contains("schema version")
    );
}
