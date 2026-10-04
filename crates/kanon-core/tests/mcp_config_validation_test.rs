//! Configuration files and API mutations share the same MCP server validation.

use std::collections::HashMap;

use kanon_core::mcp::{McpConfigStore, McpError, McpServerConfig, McpTransport};
use serde_json::json;

fn config() -> McpServerConfig {
    McpServerConfig {
        id: "valid".to_string(),
        name: "Valid server".to_string(),
        transport: McpTransport::Http {
            url: "https://example.com/mcp".to_string(),
            headers: HashMap::new(),
        },
    }
}

#[tokio::test]
async fn loaded_servers_and_api_mutations_reject_the_same_invalid_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    let mut invalid = Vec::new();
    for id in [" padded", "padded ", "bad/id", ""] {
        invalid.push(McpServerConfig {
            id: id.to_string(),
            ..config()
        });
    }
    invalid.push(McpServerConfig {
        name: " ".to_string(),
        ..config()
    });
    invalid.push(McpServerConfig {
        transport: McpTransport::Stdio {
            command: " ".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
        },
        ..config()
    });
    for url in ["http://", "file:///tmp/mcp", "https://[invalid/"] {
        invalid.push(McpServerConfig {
            transport: McpTransport::Http {
                url: url.to_string(),
                headers: HashMap::new(),
            },
            ..config()
        });
    }
    for (name, value) in [
        ("invalid name", "value"),
        ("authorization", "secret\r\ninjected: value"),
    ] {
        invalid.push(McpServerConfig {
            transport: McpTransport::Http {
                url: "https://example.com/mcp".to_string(),
                headers: HashMap::from([(name.to_string(), value.to_string())]),
            },
            ..config()
        });
    }
    let store = McpConfigStore::in_memory();
    for invalid in invalid {
        let error = store.upsert(invalid.clone()).await.unwrap_err();
        assert!(matches!(error, McpError::InvalidConfig(_)));
        assert!(!error.to_string().contains("secret"));
        assert!(store.list().await.is_empty());
        std::fs::write(&path, json!({"servers": [invalid]}).to_string()).unwrap();
        assert!(McpConfigStore::open(&path).await.is_err());
    }
}

#[tokio::test]
async fn duplicate_ids_and_unsupported_document_versions_fail_loading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    for (document, expected) in [
        (json!({"version": 2, "servers": []}), "unsupported"),
        (json!({"servers": [config(), config()]}), "duplicate"),
    ] {
        std::fs::write(&path, document.to_string()).unwrap();
        let error = McpConfigStore::open(&path).await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[tokio::test]
async fn only_missing_files_are_empty_and_legacy_documents_keep_their_extra_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    assert!(
        McpConfigStore::open(&path)
            .await
            .unwrap()
            .list()
            .await
            .is_empty()
    );
    assert!(McpConfigStore::open(dir.path()).await.is_err());

    std::fs::write(
        &path,
        json!({"servers": [config()], "extension": {"keep": true}}).to_string(),
    )
    .unwrap();
    let store = McpConfigStore::open(&path).await.unwrap();
    assert_eq!(store.list().await, [config()]);
    store.upsert(config()).await.unwrap();
    let written: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(written["version"], 1);
    assert_eq!(written["extension"], json!({"keep": true}));
}

#[cfg(unix)]
#[tokio::test]
async fn a_metadata_failure_is_not_misreported_as_a_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::os::unix::fs::symlink("mcp.json", &path).unwrap();
    let error = McpConfigStore::open(path).await.unwrap_err();
    assert!(error.to_string().contains("failed to read"), "{error}");
}

#[tokio::test]
async fn a_corrupted_persisted_document_is_not_overwritten_or_published() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    let store = McpConfigStore::open(&path).await.unwrap();
    store.upsert(config()).await.unwrap();
    let corrupt = json!({"version": 99, "servers": [config()]}).to_string();
    std::fs::write(&path, &corrupt).unwrap();
    let replacement = McpServerConfig {
        name: "Replacement".to_string(),
        ..config()
    };
    assert!(store.upsert(replacement).await.is_err());
    assert!(store.remove("valid").await.is_err());
    assert_eq!(store.get("valid").await, Some(config()));
    assert_eq!(std::fs::read_to_string(path).unwrap(), corrupt);
}
