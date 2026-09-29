//! The operator's saved configuration must reach a Rust plugin, at startup and on every reload.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kanon_sdk::prelude::plugin_host_service_client::PluginHostServiceClient;
use kanon_sdk::prelude::{
    KanonHost, Plugin, PluginContext, PluginMeta, PluginResult, ReloadPluginConfigRequest,
    async_trait,
};
use kanon_transport::connect_ipc;
use prost_types::value::Kind;

const PLUGIN_ID: &str = "org.kanon.test.config_delivery";

/// Records every configuration the host hands to the plugin.
#[derive(Clone, Default)]
struct RecordingPlugin {
    configs: Arc<Mutex<Vec<prost_types::Struct>>>,
}

#[async_trait]
impl Plugin for RecordingPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            id: PLUGIN_ID.to_string(),
            ..Default::default()
        }
    }

    async fn on_load(&mut self, ctx: &mut PluginContext) -> PluginResult<()> {
        let config = ctx.config.clone().expect("stored config must be loaded");
        self.configs.lock().unwrap().push(config);
        Ok(())
    }

    async fn on_config_reload(&mut self, config: prost_types::Struct) -> PluginResult<()> {
        self.configs.lock().unwrap().push(config);
        Ok(())
    }
}

fn city(config: &prost_types::Struct) -> Option<&str> {
    match config.fields.get("city")?.kind.as_ref()? {
        Kind::StringValue(city) => Some(city),
        _ => None,
    }
}

async fn connect_with_retry(path: &Path) -> tonic::transport::Channel {
    for _ in 0..50 {
        if let Ok(channel) = connect_ipc(path.to_path_buf()).await {
            return channel;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("endpoint {} never became reachable", path.display());
}

#[tokio::test]
async fn stored_and_reloaded_config_reach_the_plugin() {
    // The host reads `./data/plugins/<id>/config.json` relative to its working directory, which
    // for this test is the crate root; `data/` is ignored by git.
    let data_dir = Path::new("./data/plugins").join(PLUGIN_ID);
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("config.json"), r#"{"city": "Paris"}"#).unwrap();

    let socket_dir = std::env::temp_dir().join("kanon-sdk-tests");
    std::fs::create_dir_all(&socket_dir).unwrap();
    let socket = socket_dir.join(format!("config-delivery-{}.sock", std::process::id()));
    let plugin = RecordingPlugin::default();
    let configs = plugin.configs.clone();
    let host = KanonHost::new(plugin).with_socket_path(socket.clone());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(host.run_with_shutdown(async move {
        let _ = stopped.await;
    }));

    let mut client = PluginHostServiceClient::new(connect_with_retry(&socket).await);
    let response = client
        .reload_plugin_config(ReloadPluginConfigRequest {
            plugin_id: PLUGIN_ID.to_string(),
            config: Some(prost_types::Struct {
                fields: [(
                    "city".to_string(),
                    prost_types::Value {
                        kind: Some(Kind::StringValue("Rome".to_string())),
                    },
                )]
                .into(),
            }),
            version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.success, "{}", response.error_message);

    let cities: Vec<_> = configs
        .lock()
        .unwrap()
        .iter()
        .map(|config| city(config).map(str::to_string))
        .collect();
    assert_eq!(
        cities,
        [Some("Paris".to_string()), Some("Rome".to_string())]
    );

    let _ = stop.send(());
    std::fs::remove_dir_all(&data_dir).unwrap();
}
