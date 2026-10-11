//! Local socket server and client.

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::{ListenerOptions, Name};
use tokio::task::JoinHandle;

use crate::framing::{read_frame, write_frame};
use crate::{Error, Request, Response, Result};

/// Overrides the endpoint (a socket path on Unix, a pipe name on Windows).
/// Used by tests and for running several instances side by side.
pub const ENDPOINT_ENV: &str = "RDM_IPC_SOCKET";

/// Where the app listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(windows)]
    pipe: String,
}

impl Endpoint {
    /// The current user's endpoint, or the one named by `RDM_IPC_SOCKET`.
    ///
    /// * Linux: `$XDG_RUNTIME_DIR/rdm/rdm.sock`
    /// * macOS (and Linux without a runtime dir):
    ///   `<data dir>/dev.rdm.desktop/ipc/rdm.sock`
    /// * Windows: the named pipe `\\.\pipe\rdm-<user>`
    pub fn for_current_user() -> Self {
        if let Some(custom) = std::env::var_os(ENDPOINT_ENV).filter(|v| !v.is_empty()) {
            return Self::custom(custom.to_string_lossy());
        }
        #[cfg(unix)]
        {
            let dir = dirs::runtime_dir()
                .map(|d| d.join("rdm"))
                .or_else(|| dirs::data_dir().map(|d| d.join("dev.rdm.desktop").join("ipc")))
                .unwrap_or_else(|| std::env::temp_dir().join("rdm"));
            Self {
                path: dir.join("rdm.sock"),
            }
        }
        #[cfg(windows)]
        {
            let user: String = std::env::var("USERNAME")
                .unwrap_or_default()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                .collect();
            Self {
                pipe: format!("rdm-{user}"),
            }
        }
    }

    /// A specific endpoint: a socket path on Unix, a pipe name on Windows.
    pub fn custom(value: impl Into<String>) -> Self {
        #[cfg(unix)]
        {
            Self {
                path: PathBuf::from(value.into()),
            }
        }
        #[cfg(windows)]
        {
            Self { pipe: value.into() }
        }
    }

    fn name(&self) -> io::Result<Name<'_>> {
        #[cfg(unix)]
        {
            use interprocess::local_socket::GenericFilePath;
            self.path.as_path().to_fs_name::<GenericFilePath>()
        }
        #[cfg(windows)]
        {
            use interprocess::local_socket::GenericNamespaced;
            self.pipe.as_str().to_ns_name::<GenericNamespaced>()
        }
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        #[cfg(unix)]
        {
            write!(f, "{}", self.path.display())
        }
        #[cfg(windows)]
        {
            write!(f, r"\\.\pipe\{}", self.pipe)
        }
    }
}

/// A connection to the app.
#[derive(Debug)]
pub struct Client {
    stream: Stream,
}

impl Client {
    pub async fn connect(endpoint: &Endpoint) -> io::Result<Self> {
        let stream = Stream::connect(endpoint.name()?).await?;
        Ok(Self { stream })
    }

    /// Sends a request and waits for its response.
    pub async fn call(&mut self, request: &Request) -> Result<Response> {
        let (mut rx, mut tx) = (&self.stream, &self.stream);
        write_frame(&mut tx, request).await?;
        read_frame(&mut rx).await?.ok_or(Error::Closed)
    }
}

/// A running server; stops accepting when dropped.
#[derive(Debug)]
pub struct Server {
    task: JoinHandle<()>,
    endpoint: Endpoint,
}

impl Server {
    /// Starts listening on `endpoint`, answering every request with
    /// `handler`. Fails with [`Error::AlreadyRunning`] if another server is
    /// already answering there; a stale socket file from a crash is replaced.
    pub async fn start<F, Fut>(endpoint: Endpoint, handler: F) -> Result<Self>
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        if Client::connect(&endpoint).await.is_ok() {
            return Err(Error::AlreadyRunning(endpoint.to_string()));
        }
        let listener = listen(&endpoint)?;
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            loop {
                let stream = match listener.accept().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("IPC accept failed: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let handler = handler.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_connection(stream, handler.as_ref()).await {
                        tracing::debug!("IPC connection ended: {e}");
                    }
                });
            }
        });
        Ok(Self { task, endpoint })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.endpoint.path);
    }
}

fn listen(endpoint: &Endpoint) -> Result<interprocess::local_socket::tokio::Listener> {
    let options = ListenerOptions::new().name(endpoint.name()?);
    #[cfg(unix)]
    let options = {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if let Some(dir) = endpoint.path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
            // Tighten a pre-existing directory too: only this user may
            // even look up the socket.
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        // Nothing answered on connect, so any existing file is stale.
        options.try_overwrite(true)
    };
    // The private directory is the real access control. Linux also honors
    // the socket's own mode; macOS cannot set it (fchmod on a socket fails).
    #[cfg(target_os = "linux")]
    let options = {
        use interprocess::os::unix::local_socket::ListenerOptionsExt;
        options.mode(0o600)
    };
    Ok(options.create_tokio()?)
}

async fn serve_connection<F, Fut>(stream: Stream, handler: &F) -> Result<()>
where
    F: Fn(Request) -> Fut,
    Fut: Future<Output = Response>,
{
    let (mut rx, mut tx) = (&stream, &stream);
    loop {
        let response = match read_frame::<_, Request>(&mut rx).await {
            Ok(Some(request)) => handler(request).await,
            Ok(None) => return Ok(()),
            // Answer garbage once, then hang up: framing may be lost.
            Err(e @ (Error::Json(_) | Error::TooLarge(_))) => {
                write_frame(&mut tx, &Response::error(format!("bad request: {e}"))).await?;
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        write_frame(&mut tx, &response).await?;
    }
}
