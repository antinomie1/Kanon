//! Endpoint ownership and address validation regressions.
use kanon_transport::{IpcListener, generate_ipc_token, read_loopback_endpoint};

#[tokio::test]
#[cfg(unix)]
async fn listener_refuses_active_socket_files_and_symlinks() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("core.sock");
    let listener = IpcListener::bind(&path).unwrap();
    assert!(IpcListener::bind(&path).is_err());
    let _client = tokio::net::UnixStream::connect(&path).await.unwrap();
    listener.accept().await.unwrap();
    drop(listener);
    assert!(!path.exists());
    std::fs::write(&path, "keep").unwrap();
    assert!(IpcListener::bind(&path).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(temp.path().join("missing"), &path).unwrap();
    assert!(IpcListener::bind(&path).is_err());
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[tokio::test]
#[cfg(unix)]
async fn stale_socket_is_reclaimed_but_replacement_survives_drop() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("host.sock");
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    let listener = IpcListener::bind(&path).unwrap();
    let incoming = listener.incoming();
    assert!(IpcListener::bind(&path).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "replacement").unwrap();
    drop(incoming);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement");
}

#[test]
fn credential_and_endpoint_validation() {
    let first = generate_ipc_token().unwrap();
    assert_eq!(first.len(), 64);
    assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_ne!(first, generate_ipc_token().unwrap());
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("endpoint");
    for invalid in ["192.0.2.1:8080", "localhost:8080", "127.0.0.1:0", "garbage"] {
        std::fs::write(&path, invalid).unwrap();
        assert!(read_loopback_endpoint(&path).is_err());
    }
    std::fs::write(&path, "127.0.0.1:12345").unwrap();
    assert_eq!(read_loopback_endpoint(&path).unwrap().port(), 12345);
}
