//! `rdm` — headless command-line front end for the rdm download engine.

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

/// Rust Download Manager (headless).
#[derive(Debug, Parser)]
#[command(name = "rdm", version, about)]
struct Cli {}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let _cli = Cli::parse();
    tracing::info!(
        engine = rdm_core::VERSION,
        "rdm-cli scaffold; commands arrive in Phase 1"
    );
    Ok(())
}
