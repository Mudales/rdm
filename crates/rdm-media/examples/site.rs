//! Reads a web page with yt-dlp and downloads one of its qualities with
//! rdm's engine (or yt-dlp's, when rdm cannot).
//!
//! ```text
//! cargo run -p rdm-media --example site -- <page-url> [choice-id] [output-file]
//! ```
//!
//! Without a choice it lists the qualities and downloads the best one.
//! yt-dlp, Deno and ffmpeg are found the way rdm finds them (`$RDM_YTDLP`,
//! `$RDM_DENO`, `$RDM_FFMPEG`, then `PATH`).

use std::time::Instant;

use rdm_core::{Engine, EngineConfig, Request};
use rdm_media::{Ffmpeg, FfmpegConfig, MediaEngine, MediaEvent, MediaJob, MediaOutcome, YtDlp};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let page = args
        .next()
        .ok_or("usage: site <page-url> [choice-id] [output-file]")?;
    let choice = args.next();
    let output = args.next().unwrap_or_else(|| "site-video.mp4".into());

    let media = MediaEngine::new(
        Engine::new(EngineConfig::default()),
        Ffmpeg::new(FfmpegConfig::Auto, None),
    )
    .with_ytdlp(YtDlp::new(None));
    let request = Request::new(&page)?;

    let started = Instant::now();
    let info = media.inspect_site(&request).await?;
    println!(
        "{} ({}) read in {:.1}s",
        info.title,
        info.extractor,
        started.elapsed().as_secs_f64()
    );
    for c in &info.choices {
        println!("  {:<12} {}", c.id, c.label);
    }
    for e in &info.entries {
        println!("  {}  {}", e.url, e.title.as_deref().unwrap_or(""));
    }

    let mut job = MediaJob::new(request, output.into());
    job.site = true;
    job.variant = choice;
    job.concurrency = 8;
    let started = Instant::now();
    let handle = media.start(job)?;
    let mut rx = handle.subscribe();
    let watch = tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            match event {
                MediaEvent::Started { variant, .. } => println!("downloading {variant}"),
                MediaEvent::Warning(w) => println!("warning: {w}"),
                _ => {}
            }
        }
    });
    let outcome = handle.wait().await;
    watch.abort();
    match outcome? {
        MediaOutcome::Completed { path, size } => {
            let secs = started.elapsed().as_secs_f64();
            println!(
                "done: {} ({:.1} MB in {secs:.1}s, {:.1} MB/s)",
                path.display(),
                size as f64 / 1e6,
                size as f64 / 1e6 / secs
            );
        }
        other => println!("{other:?}"),
    }
    Ok(())
}
