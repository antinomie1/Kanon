//! Global plugin switches apply at every entry point and cannot be overridden by an instance.

use std::path::PathBuf;
use std::sync::Arc;

use kanon_core::instance::{InstanceDraft, InstanceRegistry, ItemPolicy};
use kanon_core::supervisor::{ManagedHost, filter_plugin_hosts};
use kanon_core::toggle::{PLUGIN_SECTION, ToggleStore};
use kanon_proto::v1::PluginMeta;

fn host(plugin_id: &str) -> Arc<ManagedHost> {
    Arc::new(ManagedHost::new(
        format!("host_{plugin_id}"),
        PathBuf::from(format!("run/host_{plugin_id}.sock")),
        tonic::transport::Channel::from_static("http://127.0.0.1:9").connect_lazy(),
        vec![PluginMeta {
            id: plugin_id.to_string(),
            ..Default::default()
        }],
        0,
    ))
}

#[tokio::test]
async fn shared_selection_resolves_global_and_instance_policy_independently() {
    let hosts = vec![host("allowed"), host("disabled")];
    let toggles = ToggleStore::in_memory();
    toggles
        .set_enabled(PLUGIN_SECTION, "disabled", false)
        .await
        .unwrap();
    let selected = filter_plugin_hosts(hosts.clone(), Some(&toggles), None).await;
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].primary_plugin_id().as_deref(), Some("allowed"));

    let mut instance = InstanceRegistry::in_memory()
        .create(
            InstanceDraft {
                name: "Fixture".to_string(),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    instance
        .plugins
        .insert("disabled".to_string(), ItemPolicy::Enable);
    instance
        .plugins
        .insert("allowed".to_string(), ItemPolicy::Disable);
    assert!(
        filter_plugin_hosts(hosts.clone(), Some(&toggles), Some(&instance))
            .await
            .is_empty()
    );

    // Embedded callers may omit the global store, but must still honor an explicit instance veto.
    let selected = filter_plugin_hosts(hosts.clone(), None, Some(&instance)).await;
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].primary_plugin_id().as_deref(), Some("disabled"));
    assert_eq!(filter_plugin_hosts(hosts, None, None).await.len(), 2);
}
