//! Persona removal orders session unbinding, durable catalog changes and concurrent bindings.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use kanon_llm::error::MemoryError;
use kanon_llm::memory::InMemory;
use kanon_llm::{
    Persona, PersonaChangeError, PersonaError, PersonaRegistry, PersonaStore, SessionManager,
    SqliteSessionStore,
};

#[test]
fn failed_deletion_keeps_the_persona_and_only_publishes_durable_unbindings() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("sessions.db");
    let session_store = Arc::new(SqliteSessionStore::open(&db).unwrap());
    let sessions = SessionManager::new(Arc::new(InMemory::new()))
        .with_store(session_store.clone())
        .unwrap();
    let registry = PersonaRegistry::new();
    let store = PersonaStore::new(dir.path().join("personas.json"));
    let persona = Persona::custom("pirate", "Pirate", "", "Arr.").unwrap();
    store.create(&registry, persona.clone()).unwrap();
    for key in ["a", "b"] {
        sessions.set_persona(key, "pirate").unwrap();
    }
    let order = sessions.list_sessions();

    // Refuse the second row, after one unbinding has already committed. A cross-store rollback
    // cannot be promised: retain the persona, preserve live/disk agreement and make retry safe.
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER reject_unbind BEFORE UPDATE ON session_meta
         WHEN NEW.session_key = '{}'
         BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
            order[1].session_key,
        ))
        .unwrap();
    assert!(matches!(
        store.remove(&registry, "pirate", Some(&sessions)),
        Err(PersonaChangeError::Session(_))
    ));
    assert!(registry.get("pirate").is_some());
    assert_eq!(store.load().unwrap(), vec![persona.clone()]);
    assert!(sessions.get_persona(&order[0].session_key).is_none());
    assert_eq!(
        sessions.get_persona(&order[1].session_key).as_deref(),
        Some("pirate")
    );
    let restored = SessionManager::new(Arc::new(InMemory::new()))
        .with_store(session_store)
        .unwrap();
    for key in ["a", "b"] {
        assert_eq!(sessions.get_persona(key), restored.get_persona(key));
    }

    connection
        .execute_batch("DROP TRIGGER reject_unbind;")
        .unwrap();
    store.remove(&registry, "pirate", Some(&sessions)).unwrap();
    assert!(registry.get("pirate").is_none());
    assert!(store.load().unwrap().is_empty());
    assert!(
        sessions
            .list_sessions()
            .iter()
            .all(|row| row.persona_id.is_none())
    );
    assert!(matches!(
        store.update(&registry, persona),
        Err(PersonaChangeError::Persona(PersonaError::NotFound(_)))
    ));
    assert!(
        store.load().unwrap().is_empty(),
        "a stale PUT must not recreate a deleted persona"
    );
}

#[test]
fn deletion_cannot_miss_a_binding_already_validated_against_the_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(PersonaRegistry::new());
    let store = Arc::new(PersonaStore::new(dir.path().join("personas.json")));
    store
        .create(
            &registry,
            Persona::custom("pirate", "Pirate", "", "Arr.").unwrap(),
        )
        .unwrap();
    let sessions = Arc::new(SessionManager::new(Arc::new(InMemory::new())));
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let binding = {
        let registry = registry.clone();
        let sessions = sessions.clone();
        std::thread::spawn(move || {
            let _writer = sessions.try_write("new").unwrap();
            registry
                .with_persona("pirate", |_| {
                    entered.send(()).unwrap();
                    release_rx.recv().unwrap();
                    sessions.set_persona("new", "pirate").unwrap();
                })
                .unwrap();
        })
    };
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (done, done_rx) = mpsc::channel();
    let deletion = {
        let registry = registry.clone();
        let sessions = sessions.clone();
        let store = store.clone();
        std::thread::spawn(move || {
            let result = store.remove(&registry, "pirate", Some(&sessions));
            done.send(()).unwrap();
            result
        })
    };
    let overtook = done_rx.recv_timeout(Duration::from_millis(100)).is_ok();
    release.send(()).unwrap();
    binding.join().unwrap();
    match deletion.join().unwrap() {
        Ok(_) => {}
        Err(PersonaChangeError::Session(MemoryError::Busy(_))) => {
            // The binder can still own its writer just after releasing its persona guard.
            store.remove(&registry, "pirate", Some(&sessions)).unwrap();
        }
        other => panic!("unexpected removal: {other:?}"),
    }
    assert!(!overtook, "deletion overtook an in-progress binding");
    assert!(sessions.get_persona("new").is_none());
    assert!(
        registry
            .with_persona("pirate", |_| sessions.set_persona("late", "pirate"))
            .is_none()
    );
    assert!(sessions.get_metadata("late").is_none());
}
