//! Cross-platform unified IPC transport layer and security authentication abstraction for Kanon.
//!
//! Provides a seamless abstraction over Unix Domain Sockets (Linux/macOS) and
//! authenticated TCP loopback streams (Windows), ensuring compatibility with
//! the Tonic/Hyper gRPC ecosystem.

use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tonic::transport::server::Connected;

pub mod listener;
pub mod path;
pub mod stream;

pub use listener::{IpcIncoming, IpcListener};
pub use path::{
    core_socket_path, default_run_dir, ensure_parent_dir, ensure_run_dir, host_socket_path,
};
pub use stream::connect_ipc;
#[cfg(windows)]
pub use stream::connect_tcp;
#[cfg(unix)]
pub use stream::connect_unix;

/// Header key used for Windows TCP loopback token-based authentication.
pub const AUTH_HEADER_KEY: &str = "x-kanon-auth-token";

/// Cross-platform IPC bidirectional byte stream abstraction.
///
/// Encapsulates `tokio::net::UnixStream` on Unix-like operating systems and
/// `tokio::net::TcpStream` on Windows. Implements standard Tokio async I/O traits
/// as well as Tonic's `Connected` trait for direct integration with `tonic::transport::Server`.
pub struct IpcStream {
    #[cfg(unix)]
    inner: tokio::net::UnixStream,
    #[cfg(windows)]
    inner: tokio::net::TcpStream,
}

impl IpcStream {
    /// Creates a new `IpcStream` wrapping an underlying Unix domain socket stream.
    #[cfg(unix)]
    pub fn new(inner: tokio::net::UnixStream) -> Self {
        Self { inner }
    }

    /// Creates a new `IpcStream` wrapping an underlying TCP loopback stream.
    #[cfg(windows)]
    pub fn new(inner: tokio::net::TcpStream) -> Self {
        Self { inner }
    }
}

impl AsyncRead for IpcStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for IpcStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl Connected for IpcStream {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {}
}

/// Tower/Tonic authentication interceptor for verifying IPC credentials.
///
/// On Windows, this interceptor verifies that incoming requests contain a valid
/// 32-byte authentication token in the `x-kanon-auth-token` metadata header using
/// constant-time comparison to prevent timing attacks.
/// On Unix, access is protected by POSIX filesystem permissions (0700).
#[derive(Clone)]
pub struct AuthInterceptor {
    expected_token: String,
}

impl AuthInterceptor {
    /// Constructs a new `AuthInterceptor` with the expected token.
    pub fn new(token: String) -> Self {
        Self {
            expected_token: token,
        }
    }
}

impl tonic::service::Interceptor for AuthInterceptor {
    fn call(&mut self, request: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        // Unix callers without a token retain filesystem authentication; configured tokens
        // are enforced on every platform so the same contract can be exercised in tests.
        #[cfg(unix)]
        if self.expected_token.is_empty() {
            return Ok(request);
        }
        let token = request
            .metadata()
            .get(AUTH_HEADER_KEY)
            .and_then(|v| v.to_str().ok());
        match token {
            Some(token)
                if self.expected_token.len() == 64
                    && constant_time_eq::constant_time_eq(
                        token.as_bytes(),
                        self.expected_token.as_bytes(),
                    ) =>
            {
                Ok(request)
            }
            _ => Err(tonic::Status::unauthenticated(
                "Invalid or missing IPC authentication token",
            )),
        }
    }
}

/// Generates a fresh 32-byte CSPRNG token encoded as lowercase hexadecimal metadata.
pub fn generate_ipc_token() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Reads an endpoint file and rejects remote addresses before any connection is attempted.
pub fn read_loopback_endpoint(path: &std::path::Path) -> std::io::Result<std::net::SocketAddr> {
    let address: std::net::SocketAddr = std::fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "IPC endpoint must use a nonzero loopback port",
        ));
    }
    Ok(address)
}

/// Attaches the startup credential to the first HEADERS frame of every RPC.
#[derive(Clone, Default)]
pub struct ClientAuthInterceptor(pub String);

impl std::fmt::Debug for ClientAuthInterceptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Generated clients include the interceptor in Debug output; never disclose credentials.
        formatter.write_str("ClientAuthInterceptor(<redacted>)")
    }
}

impl tonic::service::Interceptor for ClientAuthInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        if !self.0.is_empty() {
            let token = self
                .0
                .parse()
                .map_err(|_| tonic::Status::unauthenticated("invalid IPC token"))?;
            request.metadata_mut().insert(AUTH_HEADER_KEY, token);
        }
        #[cfg(windows)]
        if self.0.is_empty() {
            return Err(tonic::Status::unauthenticated("missing IPC token"));
        }
        Ok(request)
    }
}

/// Shared authenticated channel type used by core and host clients.
pub type AuthenticatedChannel = tonic::service::interceptor::InterceptedService<
    tonic::transport::Channel,
    ClientAuthInterceptor,
>;
