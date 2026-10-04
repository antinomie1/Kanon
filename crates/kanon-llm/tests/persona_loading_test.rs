//! Persisted personas must identify one unambiguous prompt after normalization.

use kanon_llm::PersonaStore;

#[test]
fn duplicate_normalized_persona_ids_are_rejected_before_registry_publication() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("personas.json");
    std::fs::write(
        &path,
        r#"{"version":1,"personas":[
        {"id":"pirate","name":"Pirate","prompt":"First prompt"},
        {"id":" pirate ","name":"Other","prompt":"Another prompt"}
    ]}"#,
    )
    .unwrap();
    let store = PersonaStore::new(path);
    let error = store.load().unwrap_err();
    assert!(
        error.contains("pirate") && error.contains("defined twice"),
        "{error}"
    );
    assert!(store.load_registry().is_err());
}
