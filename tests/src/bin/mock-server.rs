//! Standalone mock server for manual benchmarks and acceptance checks.
//!
//! ```text
//! mock-server [--port 8080] [--size 1G] [--rate 20M] [--no-ranges] [--seed N]
//! mock-server --verify FILE --size 1G [--seed N]
//! ```
//!
//! `--rate` limits each connection, which models a server or path that caps
//! per-connection throughput. That is exactly the case multi-connection
//! downloading speeds up.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use rdm_tests::{MockConfig, MockServer, parse_size, verify_file};

#[tokio::main]
async fn main() -> Result<()> {
    let mut port = 8080u16;
    let mut config = MockConfig::new(1 << 30);
    let mut verify: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--port" => port = value()?.parse()?,
            "--size" => config.size = parse_size(&value()?).context("bad --size")?,
            "--rate" => {
                config.per_connection_rate = Some(parse_size(&value()?).context("bad --rate")?)
            }
            "--seed" => config.seed = value()?.parse()?,
            "--no-ranges" => config.ranges = false,
            "--verify" => verify = Some(value()?.into()),
            "-h" | "--help" => {
                println!(
                    "mock-server [--port N] [--size SIZE] [--rate SIZE/s per connection] \
                     [--no-ranges] [--seed N]\nmock-server --verify FILE --size SIZE [--seed N]"
                );
                return Ok(());
            }
            other => bail!("unknown argument {other}"),
        }
    }

    if let Some(path) = verify {
        return match verify_file(&path, config.seed, config.size)? {
            Ok(()) => {
                println!("OK: {} matches ({} bytes)", path.display(), config.size);
                Ok(())
            }
            Err(msg) => bail!("MISMATCH: {}: {msg}", path.display()),
        };
    }

    let server = MockServer::bind(SocketAddr::from(([127, 0, 0, 1], port)), config.clone()).await?;
    println!(
        "serving {} bytes at {} (ranges: {}, per-connection rate: {})",
        config.size,
        server.url(),
        config.ranges,
        config
            .per_connection_rate
            .map_or("unlimited".into(), |r| format!("{r} B/s")),
    );
    tokio::signal::ctrl_c().await?;
    println!(
        "served {} requests, {} bytes, peak {} concurrent connections",
        server.requests(),
        server.bytes_served(),
        server.peak_connections()
    );
    Ok(())
}
