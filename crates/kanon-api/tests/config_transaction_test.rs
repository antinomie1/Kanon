//! Configuration writes preserve concurrent edits and fail without publishing partial state.

use std::sync::{Arc, Barrier};

use kanon_adapter_onebot::OneBotConfig;
use kanon_api::{ApiState, NodeSettings, SystemConfigStore};
use kanon_core::Supervisor;
use kanon_llm::ProviderEntry;

#[test]
fn separate_store_handles_preserve_other_sections_during_concurrent_saves() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("system.json");
    let start = Arc::new(Barrier::new(2));
    std::thread::scope(|scope| {
        let first = start.clone();
        let first_path = path.clone();
        scope.spawn(move || {
            let store = SystemConfigStore::new(first_path);
            first.wait();
            for _ in 0..30 {
                store.save_node_settings(&NodeSettings::default()).unwrap();
            }
        });
        scope.spawn(|| {
            let store = SystemConfigStore::new(&path);
            start.wait();
            let config = OneBotConfig::default();
            for _ in 0..30 {
                store.save_onebot(&config).unwrap();
            }
        });
    });
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(document.get("providers").is_some());
    assert!(document.get("onebot").is_some());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn concurrent_partial_updates_merge_and_publish_the_saved_document() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SystemConfigStore::new(dir.path().join("system.json")));
    let state = ApiState::builder(Arc::new(Supervisor::new(
        Some(dir.path().join("run")),
        None,
    )))
    .with_system_config(store.clone())
    .build();
    let start = Arc::new(Barrier::new(8));
    // reqwest provider construction requires a Tokio runtime even though this test makes no
    // network calls. Each OS thread enters the same runtime before updating the directory.
    let runtime = tokio::runtime::Handle::current();
    std::thread::scope(|scope| {
        for index in 0..8 {
            let state = state.clone();
            let start = start.clone();
            let runtime = runtime.clone();
            scope.spawn(move || {
                let _entered = runtime.enter();
                start.wait();
                state
                    .update_node_settings(|settings| {
                        settings.providers.push(ProviderEntry::new(
                            format!("provider{index}"),
                            "openai",
                            "http://127.0.0.1:9/v1",
                        ));
                        Ok(())
                    })
                    .unwrap();
            });
        }
    });
    let saved = store.load_node_settings().unwrap();
    assert_eq!(saved.providers.len(), 8);
    assert_eq!(state.node_settings().providers, saved.providers);
    assert_eq!(state.agent_factory().providers().list().len(), 8);
}

#[tokio::test]
async fn a_failed_save_does_not_publish_the_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("system.json");
    std::fs::create_dir(&path).unwrap();
    let state = ApiState::builder(Arc::new(Supervisor::new(
        Some(dir.path().join("run")),
        None,
    )))
    .with_system_config(Arc::new(SystemConfigStore::new(path)))
    .build();
    let before = state.reply_policy().get();
    assert!(
        state
            .update_node_settings(|settings| {
                settings.reply_policy.split_lines = !before.split_lines;
                Ok(())
            })
            .is_err()
    );
    assert_eq!(state.node_settings().reply_policy, before);
    assert_eq!(state.reply_policy().get(), before);
}
