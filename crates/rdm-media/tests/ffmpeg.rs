//! Merging with a real ffmpeg. Each test generates real media with ffmpeg,
//! serves it as HLS, downloads it and inspects the result. Skipped (with a
//! note) when ffmpeg is not installed; CI installs it on Linux.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use rdm_core::{Engine, EngineConfig, ProxyConfig, Request};
use rdm_media::{Ffmpeg, FfmpegConfig, MediaEngine, MediaJob, MediaOutcome, OutputFormat};
use rdm_tests::{MockConfig, MockServer};

/// ffmpeg with an H.264 encoder (what real HLS streams carry). MPEG-4
/// Part 2 is no substitute: ffmpeg cannot read its frame size from MPEG-TS
/// without decoding, so it cannot be stream-copied into MP4.
fn ffmpeg() -> Option<PathBuf> {
    let Some(found) = Ffmpeg::new(FfmpegConfig::Auto, None).locate() else {
        eprintln!("ffmpeg not found; skipping");
        return None;
    };
    let encoders = Command::new(&found)
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()?;
    if !String::from_utf8_lossy(&encoders.stdout).contains("libx264") {
        eprintln!("ffmpeg has no libx264 encoder; skipping");
        return None;
    }
    Some(found)
}

fn run(ffmpeg: &Path, args: &[&str]) {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// ffmpeg's description of a file's streams (it exits non-zero without an
/// output, which is fine).
fn streams(ffmpeg: &Path, file: &Path) -> String {
    let out = Command::new(ffmpeg)
        .arg("-hide_banner")
        .arg("-i")
        .arg(file)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Generates a 3-second HLS stream into `dir`. `extra` selects tracks.
fn make_hls(ffmpeg: &Path, dir: &Path, extra: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    let segs = dir.join("s%d.ts");
    let index = dir.join("index.m3u8");
    let mut args = vec![
        "-f",
        "lavfi",
        "-i",
        "testsrc=duration=3:size=160x120:rate=10",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=3",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-c:a",
        "aac",
        "-shortest",
    ];
    args.extend_from_slice(extra);
    args.extend_from_slice(&[
        "-f",
        "hls",
        "-hls_time",
        "1",
        "-hls_playlist_type",
        "vod",
        "-hls_segment_filename",
    ]);
    let segs = segs.to_string_lossy().into_owned();
    let index = index.to_string_lossy().into_owned();
    args.push(&segs);
    args.push(&index);
    run(ffmpeg, &args);
}

/// Serves every file under `root` at `/<relative path>`.
async fn serve(root: &Path, extra: &[(&str, &str)]) -> MockServer {
    let mut cfg = MockConfig::new(1);
    for entry in walk(root) {
        let rel = entry
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let ct = if rel.ends_with(".m3u8") {
            "application/vnd.apple.mpegurl"
        } else {
            "video/mp2t"
        };
        cfg = cfg.with_file(&format!("/{rel}"), std::fs::read(&entry).unwrap(), ct);
    }
    for (path, body) in extra {
        cfg = cfg.with_file(path, body.to_string(), "application/vnd.apple.mpegurl");
    }
    MockServer::start(cfg).await.unwrap()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

async fn download(
    server: &MockServer,
    path: &str,
    output: PathBuf,
    format: OutputFormat,
    ffmpeg: &Path,
) -> PathBuf {
    let engine = MediaEngine::new(
        Engine::new(EngineConfig::default()),
        Ffmpeg::new(FfmpegConfig::Path(ffmpeg.into()), None),
    );
    let request = Request::new(&format!("http://{}{path}", server.addr()))
        .unwrap()
        .proxy(ProxyConfig::Direct);
    let mut job = MediaJob::new(request, output);
    job.format = format;
    let outcome = tokio::time::timeout(Duration::from_secs(60), engine.start(job).unwrap().wait())
        .await
        .unwrap()
        .unwrap();
    match outcome {
        MediaOutcome::Completed { path, .. } => path,
        other => panic!("{other:?}"),
    }
}

fn is_mp4(path: &Path) -> bool {
    std::fs::read(path).unwrap().get(4..8) == Some(b"ftyp")
}

#[tokio::test(flavor = "multi_thread")]
async fn ts_stream_becomes_mp4() {
    let Some(ff) = ffmpeg() else { return };
    let src = tempfile::tempdir().unwrap();
    make_hls(&ff, src.path(), &[]);
    let server = serve(src.path(), &[]).await;
    let out = tempfile::tempdir().unwrap();

    let path = download(
        &server,
        "/index.m3u8",
        out.path().join("clip.mp4"),
        OutputFormat::Auto,
        &ff,
    )
    .await;
    assert_eq!(path.extension().unwrap(), "mp4");
    assert!(is_mp4(&path));
    let info = streams(&ff, &path);
    assert!(info.contains("Video:") && info.contains("Audio:"), "{info}");
}

#[tokio::test(flavor = "multi_thread")]
async fn separate_audio_and_video_are_merged() {
    let Some(ff) = ffmpeg() else { return };
    let src = tempfile::tempdir().unwrap();
    make_hls(&ff, &src.path().join("v"), &["-map", "0:v"]);
    make_hls(&ff, &src.path().join("a"), &["-map", "1:a"]);
    let master = "#EXTM3U\n\
        #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Tone\",DEFAULT=YES,URI=\"a/index.m3u8\"\n\
        #EXT-X-STREAM-INF:BANDWIDTH=300000,RESOLUTION=160x120,AUDIO=\"aud\"\nv/index.m3u8\n";
    let server = serve(src.path(), &[("/master.m3u8", master)]).await;
    let out = tempfile::tempdir().unwrap();

    let path = download(
        &server,
        "/master.m3u8",
        out.path().join("merged.mp4"),
        OutputFormat::Auto,
        &ff,
    )
    .await;
    assert!(is_mp4(&path));
    let info = streams(&ff, &path);
    assert!(info.contains("Video:") && info.contains("Audio:"), "{info}");

    let mkv = download(
        &server,
        "/master.m3u8",
        out.path().join("merged.mkv"),
        OutputFormat::Mkv,
        &ff,
    )
    .await;
    assert_eq!(mkv.extension().unwrap(), "mkv");
}

#[tokio::test(flavor = "multi_thread")]
async fn audio_is_extracted() {
    let Some(ff) = ffmpeg() else { return };
    let src = tempfile::tempdir().unwrap();
    make_hls(&ff, src.path(), &[]);
    let server = serve(src.path(), &[]).await;
    let out = tempfile::tempdir().unwrap();

    let path = download(
        &server,
        "/index.m3u8",
        out.path().join("tone.mp4"),
        OutputFormat::M4a,
        &ff,
    )
    .await;
    assert_eq!(path.extension().unwrap(), "m4a");
    let info = streams(&ff, &path);
    assert!(
        info.contains("Audio:") && !info.contains("Video:"),
        "{info}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_merge_leaves_no_partial_output() {
    let Some(ff) = ffmpeg() else { return };
    // Garbage "TS" segments: joining works, ffmpeg's remux fails.
    let mut cfg = MockConfig::new(1).with_file(
        "/bad/index.m3u8",
        "#EXTM3U\n#EXTINF:1,\na.ts\n#EXTINF:1,\nb.ts\n#EXT-X-ENDLIST\n",
        "application/vnd.apple.mpegurl",
    );
    for name in ["a", "b"] {
        cfg = cfg.with_file(&format!("/bad/{name}.ts"), vec![0x42u8; 4000], "video/mp2t");
    }
    let server = MockServer::start(cfg).await.unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = MediaEngine::new(
        Engine::new(EngineConfig::default()),
        Ffmpeg::new(FfmpegConfig::Path(ff.clone()), None),
    );
    let request = Request::new(&format!("http://{}/bad/index.m3u8", server.addr()))
        .unwrap()
        .proxy(ProxyConfig::Direct);
    let job = MediaJob::new(request, out.path().join("bad.mp4"));
    let result = engine.start(job).unwrap().wait().await;
    assert!(
        matches!(result, Err(rdm_media::Error::Ffmpeg(_))),
        "{result:?}"
    );
    assert!(!out.path().join("bad.mp4").exists(), "no partial output");
    assert!(
        out.path().join("bad.mp4.rdm-parts").exists(),
        "parts kept for a retry"
    );
}
