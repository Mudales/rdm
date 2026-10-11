//! `rdm-ipc` — the local channel between `rdm-host` (spawned by browsers)
//! and the running rdm app.
//!
//! * [`protocol`]: the JSON messages. The browser extension speaks the same
//!   messages to `rdm-host`, which relays them unchanged.
//! * [`framing`]: 32-bit native-endian length prefix + UTF-8 JSON, exactly
//!   the browser native messaging framing, so one codec serves both hops.
//! * [`transport`]: a Unix domain socket in a `0700` directory (and with
//!   mode `0600` on Linux) or a per-user named pipe on Windows, so only the
//!   same OS user can reach it.

pub mod framing;
pub mod protocol;
pub mod transport;

pub use framing::{MAX_FRAME, read_frame, write_frame};
pub use protocol::{AddItem, AddMode, PROTOCOL_VERSION, Request, Response};
pub use transport::{Client, Endpoint, Server};

/// Errors from the IPC layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("I/O error")]
    Io(#[from] std::io::Error),

    #[error("malformed message")]
    Json(#[from] serde_json::Error),

    #[error("message of {0} bytes exceeds the {MAX_FRAME}-byte limit")]
    TooLarge(usize),

    #[error("the connection closed before a reply arrived")]
    Closed,

    #[error("another rdm instance is already listening on {0}")]
    AlreadyRunning(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
