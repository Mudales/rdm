//! `rdm-host` — Native Messaging host.
//!
//! Browsers launch this process and talk to it over stdin/stdout using
//! length-prefixed JSON. It relays download requests to the running rdm app.
//! Logs go to stderr only: stdout is reserved for the protocol.
//! Implemented in Phase 4.

use anyhow::Result;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    tracing::warn!("rdm-host scaffold; the native messaging protocol arrives in Phase 4");
    Ok(())
}
