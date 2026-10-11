//! Downloads the platform's static ffmpeg build into a folder, the same way
//! the app's Settings → Media does (SHA-256 verified).
//!
//! ```text
//! cargo run -p rdm-media --example install-ffmpeg -- <tools-dir>
//! ```

use rdm_core::{Engine, EngineConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .ok_or("usage: install-ffmpeg <tools-dir>")?;
    let Some(source) = rdm_media::ffmpeg_source() else {
        println!("{}", rdm_media::install_hint());
        return Ok(());
    };
    println!(
        "downloading ffmpeg from {} ({}, ~{} MB)",
        source.publisher, source.license, source.size_mb
    );
    let path =
        rdm_media::install_ffmpeg(&Engine::new(EngineConfig::default()), dir.as_ref()).await?;
    println!("installed {}", path.display());
    println!("{}", rdm_media::Ffmpeg::version(&path).await?);
    Ok(())
}
