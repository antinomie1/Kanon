//! A store whose write fails must keep serving exactly what its file says.
//!
//! Each case opens a store on a fresh path, then puts a directory where the document belongs so
//! the atomic write cannot land. The mutation must fail and the live state must not change:
//! otherwise the node would run an unsaved change that the next restart silently reverts.

use std::collections::HashMap;

use kanon_core::{
    InstanceDraft, InstanceRegistry, McpConfigStore, McpServerConfig, McpTransport, PLUGIN_SECTION,
    ToggleStore,
};

#[tokio::test]
async fn failed_writes_leave_live_state_unchanged() {
    let dir = tempfile::tempdir().expect("temp dir");

    let toggles_path = dir.path().join("toggles.json");
    let toggles = ToggleStore::open(&toggles_path)
        .await
        .expect("open toggles");
    std::fs::create_dir(&toggles_path).expect("block toggles file");
    toggles
        .set_enabled(PLUGIN_SECTION, "org.kanon.test", false)
        .await
        .expect_err("toggle write must fail");
    assert!(toggles.is_enabled(PLUGIN_SECTION, "org.kanon.test").await);

    let mcp_path = dir.path().join("mcp.json");
    let mcp = McpConfigStore::open(&mcp_path).await.expect("open mcp");
    std::fs::create_dir(&mcp_path).expect("block mcp file");
    mcp.upsert(McpServerConfig {
        id: "fake".to_string(),
        name: "Fake".to_string(),
        transport: McpTransport::Stdio {
            command: "fake".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
        },
    })
    .await
    .expect_err("mcp write must fail");
    assert!(mcp.list().await.is_empty());

    let instances_path = dir.path().join("instances.json");
    let instances = InstanceRegistry::open(&instances_path)
        .await
        .expect("open instances");
    std::fs::create_dir(&instances_path).expect("block instances file");
    instances
        .create(
            InstanceDraft {
                name: "Bot".to_string(),
                enabled: true,
                ..Default::default()
            },
            None,
        )
        .await
        .expect_err("instance write must fail");
    assert!(instances.is_empty().await);
}
