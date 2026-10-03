//! Cross-platform IPC listener abstraction for Unix domain sockets and TCP loopback.
//!
//! Provides [`IpcListener`] and [`IpcIncoming`], which integrate seamlessly
//! with Tonic's gRPC server (`serve_with_incoming`).

use crate::IpcStream;
use crate::path::ensure_parent_dir;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A cross-platform listener for incoming IPC connections.
///
/// On Unix platforms, this listener wraps a [`tokio::net::UnixListener`].
/// On Windows platforms, this listener wraps a [`tokio::net::TcpListener`].
pub struct IpcListener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    inner: tokio::net::TcpListener,
    ownership: EndpointOwnership,
}

impl IpcListener {
    /// Binds a filesystem endpoint, retaining exclusive ownership until the listener drops.
    /// Windows publishes an ephemeral IPv4 loopback address in the endpoint file.
    pub fn bind(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        ensure_parent_dir(path)?;
        let ownership = EndpointOwnership::acquire(path)?;
        #[cfg(unix)]
        let inner = tokio::net::UnixListener::bind(path)?;
        #[cfg(windows)]
        let inner = {
            use std::io::Write;
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            listener.set_nonblocking(true)?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            write!(file, "{}", listener.local_addr()?)?;
            tokio::net::TcpListener::from_std(listener)?
        };
        let mut ownership = ownership;
        ownership.identity = Some(std::fs::symlink_metadata(path)?);
        Ok(Self { inner, ownership })
    }

    /// Asynchronously accepts a new incoming IPC connection.
    pub async fn accept(&self) -> std::io::Result<IpcStream> {
        #[cfg(unix)]
        {
            let (stream, _) = self.inner.accept().await?;
            Ok(IpcStream::new(stream))
        }
        #[cfg(windows)]
        {
            let (stream, _) = self.inner.accept().await?;
            Ok(IpcStream::new(stream))
        }
    }

    /// Converts this listener into an [`IpcIncoming`] stream for use with Tonic's
    /// `Server::serve_with_incoming`.
    pub fn incoming(self) -> IpcIncoming {
        IpcIncoming {
            _ownership: self.ownership,
            #[cfg(unix)]
            inner: self.inner,
            #[cfg(windows)]
            inner: self.inner,
        }
    }
}

/// An incoming stream of [`IpcStream`] connections.
///
/// Implements [`futures_core::Stream`] so that it can be passed directly to
/// `tonic::transport::Server::serve_with_incoming`.
pub struct IpcIncoming {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    inner: tokio::net::TcpListener,
    _ownership: EndpointOwnership,
}

impl futures_core::Stream for IpcIncoming {
    type Item = std::io::Result<IpcStream>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        #[cfg(unix)]
        {
            match self.inner.poll_accept(cx) {
                Poll::Ready(Ok((stream, _))) => Poll::Ready(Some(Ok(IpcStream::new(stream)))),
                Poll::Ready(Err(err)) => Poll::Ready(Some(Err(err))),
                Poll::Pending => Poll::Pending,
            }
        }
        #[cfg(windows)]
        {
            match self.inner.poll_accept(cx) {
                Poll::Ready(Ok((stream, _))) => Poll::Ready(Some(Ok(IpcStream::new(stream)))),
                Poll::Ready(Err(err)) => Poll::Ready(Some(Err(err))),
                Poll::Pending => Poll::Pending,
            }
        }
    }
}

/// The persistent lock file is never unlinked: replacing it would split lock ownership.
struct EndpointOwnership {
    path: std::path::PathBuf,
    _lock: std::fs::File,
    identity: Option<std::fs::Metadata>,
}

impl EndpointOwnership {
    fn acquire(path: &std::path::Path) -> std::io::Result<Self> {
        use std::io::{Error, ErrorKind};
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
        }
        let lock = options.open(lock_path)?;
        lock.try_lock()
            .map_err(|e| Error::new(ErrorKind::AddrInUse, e))?;
        match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileTypeExt;
                    if !meta.file_type().is_socket() {
                        return Err(Error::new(
                            ErrorKind::AlreadyExists,
                            "IPC endpoint is not a socket",
                        ));
                    }
                    let probe =
                        socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
                    probe.set_nonblocking(true)?;
                    match probe.connect(&socket2::SockAddr::unix(path)?) {
                        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {}
                        _ => {
                            return Err(Error::new(ErrorKind::AddrInUse, "IPC endpoint is active"));
                        }
                    }
                }
                #[cfg(windows)]
                {
                    if !meta.is_file() || meta.file_type().is_symlink() {
                        return Err(Error::new(
                            ErrorKind::AlreadyExists,
                            "invalid IPC endpoint file",
                        ));
                    }
                    let addr = crate::read_loopback_endpoint(path)?;
                    match std::net::TcpStream::connect_timeout(
                        &addr,
                        std::time::Duration::from_millis(100),
                    ) {
                        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {}
                        _ => {
                            return Err(Error::new(ErrorKind::AddrInUse, "IPC endpoint is active"));
                        }
                    }
                }
                if !same_file(&meta, &std::fs::symlink_metadata(path)?) {
                    return Err(Error::new(
                        ErrorKind::AddrInUse,
                        "IPC endpoint changed during stale check",
                    ));
                }
                std::fs::remove_file(path)?;
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(Self {
            path: path.to_owned(),
            _lock: lock,
            identity: None,
        })
    }
}

fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(windows)]
    {
        left.created().ok() == right.created().ok() && left.len() == right.len()
    }
}

impl Drop for EndpointOwnership {
    fn drop(&mut self) {
        if let Some(identity) = &self.identity
            && let Ok(current) = std::fs::symlink_metadata(&self.path)
            && same_file(identity, &current)
            && let Err(error) = std::fs::remove_file(&self.path)
        {
            tracing::warn!(%error, path = %self.path.display(), "Failed to remove owned IPC endpoint");
        }
    }
}
