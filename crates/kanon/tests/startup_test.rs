//! Node readiness and cancellation while an optional plugin installs its environment.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Reaps the node even when an assertion fails.
struct Node(Child);

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn api_serves_and_shutdown_cancels_pending_clients_and_plugin_install() {
    let root = tempfile::tempdir().unwrap();
    let plugin = root.path().join("plugins/slow");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::create_dir(root.path().join("data")).unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let installer = bin.join("uv");
    // The marker proves startup reached dependency installation; exec keeps cancellation scoped
    // to the installer's own process rather than leaving a shell's sleeping child behind.
    std::fs::write(
        &installer,
        "#!/bin/sh\nprintf '%s' $$ > installer.pid\nexec sleep 60\n",
    )
    .unwrap();
    std::fs::set_permissions(&installer, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(plugin.join("plugin.toml"), "[plugin]\nid='org.kanon.test.slow'\nname='Slow'\nversion='1.0.0'\nruntime='python'\nentrypoint='main.py'\n").unwrap();
    std::fs::write(plugin.join("main.py"), "").unwrap();
    std::fs::write(
        plugin.join("pyproject.toml"),
        "[project]\nname='slow'\nversion='1.0.0'\n",
    )
    .unwrap();

    let address = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    std::fs::write(
        root.path().join("data/system.json"),
        format!("{{\"startup\":{{\"api_addr\":\"{address}\",\"run_dir\":\"run\"}}}}"),
    )
    .unwrap();
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let log = std::fs::File::create(root.path().join("node.log")).unwrap();
    let mut node = Node(
        Command::new(env!("CARGO_BIN_EXE_kanon"))
            .current_dir(root.path())
            .env("PATH", path)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            node.0.try_wait().unwrap().is_none(),
            "node exited: {}",
            std::fs::read_to_string(root.path().join("node.log")).unwrap()
        );
        if plugin.join("installer.pid").exists()
            && let Ok(mut connection) =
                TcpStream::connect_timeout(&address, Duration::from_millis(100))
        {
            connection
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            connection
                .write_all(
                    b"GET /api/v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            let mut response = String::new();
            if connection.read_to_string(&mut response).is_ok()
                && response.contains("\"status\":\"ok\"")
            {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "API was blocked by installation: {}",
            std::fs::read_to_string(root.path().join("node.log")).unwrap()
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // A client can stop sending a request body indefinitely. The 100-continue response proves
    // the server is already waiting in its body reader before shutdown is requested.
    let mut stalled = TcpStream::connect(address).unwrap();
    stalled
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stalled.write_all(b"POST /api/v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\nExpect: 100-continue\r\n\r\n").unwrap();
    let mut response = [0; 128];
    let read = stalled.read(&mut response).unwrap();
    assert!(String::from_utf8_lossy(&response[..read]).contains("100 Continue"));

    assert!(
        Command::new("kill")
            .args(["-TERM", &node.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = node.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "shutdown waited for optional dependency installation or an unfinished HTTP body"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let installer_pid = std::fs::read_to_string(plugin.join("installer.pid")).unwrap();
    assert!(
        !Command::new("kill")
            .args(["-0", installer_pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "installer survived node shutdown"
    );
}
