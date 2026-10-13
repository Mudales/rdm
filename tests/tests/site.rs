//! Videos on web pages: yt-dlp (a fake one, `src/bin/fake-ytdlp.rs`) reads
//! the page, rdm's engine downloads what it found from the mock server, and
//! yt-dlp's own downloader takes over only when rdm cannot.

// Test helpers outside `#[test]` functions are not covered by clippy.toml's
// test exemptions.
#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use common::{MIB, assert_content, engine};
use rdm_core::Request;
use rdm_media::{Error, Ffmpeg, MediaEngine, MediaEvent, MediaJob, MediaOutcome, YtDlp};
use rdm_tests::{MockConfig, MockServer};
use serde_json::{Value, json};

const PAGE: &str = "https://videos.example/watch?v=42";

/// Copies the fake yt-dlp into `dir` with its configuration.
fn fake_ytdlp(dir: &Path, config: Value) -> PathBuf {
    let name = if cfg!(windows) {
        "fake-ytdlp.exe"
    } else {
        "fake-ytdlp"
    };
    let exe = dir.join(name);
    std::fs::copy(env!("CARGO_BIN_EXE_fake-ytdlp"), &exe).unwrap();
    std::fs::write(dir.join("fake-ytdlp.json"), config.to_string()).unwrap();
    exe
}

fn media(ytdlp: PathBuf) -> MediaEngine {
    MediaEngine::new(engine(), Ffmpeg::disabled()).with_ytdlp(YtDlp::at(ytdlp))
}

fn runs(dir: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(dir.join("fake-ytdlp.args"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn site_job(dir: &Path) -> MediaJob {
    let mut job = MediaJob::new(
        Request::new(PAGE).unwrap().cookies("sid=s3cret; lang=en"),
        dir.join("out").join("video.mp4"),
    );
    job.site = true;
    job.concurrency = 3;
    job
}

async fn run(media: &MediaEngine, job: MediaJob) -> (Result<MediaOutcome, Error>, Vec<MediaEvent>) {
    let handle = media.start(job).unwrap();
    let mut rx = handle.subscribe();
    let result = handle.wait().await;
    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    (result, events)
}

#[tokio::test(flavor = "multi_thread")]
async fn found_video_is_downloaded_by_rdm_in_short_requests() {
    let size = 3 * MIB + 777;
    let mut config = MockConfig::new(size);
    // The site wants the headers yt-dlp reported for the format.
    config.faults.require_headers = vec![("X-Site".into(), "yes".into())];
    let server = MockServer::start(config).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(
        dir.path(),
        json!({"info": {
            "_type": "video", "id": "42", "title": "Clip", "webpage_url": PAGE,
            "formats": [
                {"format_id": "18", "ext": "mp4", "protocol": "https", "url": server.url(),
                 "vcodec": "avc1.42001E", "acodec": "mp4a.40.2", "height": 360, "filesize": size,
                 "http_headers": {"X-Site": "yes"},
                 "downloader_options": {"http_chunk_size": 262_144}}
            ]
        }}),
    );
    let media = media(ytdlp);

    let info = media
        .inspect_site(&Request::new(PAGE).unwrap())
        .await
        .unwrap();
    assert_eq!(info.title, "Clip");
    assert_eq!(info.choices.len(), 1);

    let (result, events) = run(&media, site_job(dir.path())).await;
    let MediaOutcome::Completed { path, size: got } = result.unwrap() else {
        panic!("not completed")
    };
    assert_eq!(path, dir.path().join("out").join("video.mp4"));
    assert_eq!(got, size);
    assert_content(&path, 0, size);
    assert!(
        events.iter().any(
            |e| matches!(e, MediaEvent::Started { variant, .. } if variant.starts_with("360p"))
        ),
        "{events:?}"
    );

    // Short requests, as the format asked for.
    let ranges = server.served_ranges();
    assert!(ranges.len() >= 12, "{ranges:?}");
    assert!(ranges.iter().all(|(a, b)| b - a < 262_144), "{ranges:?}");

    // yt-dlp got the browser cookies through a private file that is gone.
    let first = &runs(dir.path())[1];
    assert!(first.iter().any(|a| a == "--dump-single-json"));
    assert!(first.iter().any(|a| a == "--ignore-config"));
    let cookie_arg = first
        .iter()
        .position(|a| a == "--cookies")
        .map(|i| PathBuf::from(&first[i + 1]))
        .unwrap();
    assert!(
        !cookie_arg.exists(),
        "the cookie file is deleted after the run"
    );
    let cookies = std::fs::read_to_string(dir.path().join("fake-ytdlp.cookies")).unwrap();
    assert!(cookies.contains("\tsid\ts3cret\n"), "{cookies}");
    assert!(
        !first.iter().any(|a| a.contains("s3cret")),
        "cookies never on the command line"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn other_protocols_are_downloaded_by_ytdlp() {
    let dir = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(
        dir.path(),
        json!({
            "info": {"title": "Old stream", "formats": [
                {"format_id": "rtmp-720", "ext": "flv", "protocol": "rtmp", "url": "rtmp://media.example/live/x",
                 "vcodec": "h264", "acodec": "aac", "height": 720}
            ]},
            "download": {"bytes": 5000, "ext": "mp4"}
        }),
    );
    let (result, events) = run(&media(ytdlp), site_job(dir.path())).await;
    let MediaOutcome::Completed { path, size } = result.unwrap() else {
        panic!("not completed")
    };
    assert_eq!(
        (path.clone(), size),
        (dir.path().join("out").join("video.mp4"), 5000)
    );
    let data = std::fs::read(&path).unwrap();
    assert!(data.iter().enumerate().all(|(i, b)| *b == (i % 251) as u8));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, MediaEvent::Progress(p) if p.bytes == 5000)),
        "{events:?}"
    );
    let download_run = runs(dir.path()).pop().unwrap();
    let f = download_run.iter().position(|a| a == "-f").unwrap();
    assert_eq!(download_run[f + 1], "rtmp-720");
    assert!(
        !download_run
            .iter()
            .any(|a| a == "--allow-unplayable-formats")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_site_refusing_rdm_falls_back_to_ytdlp() {
    let mut config = MockConfig::new(MIB);
    config.faults.require_headers = vec![("X-Only-Ytdlp".into(), "1".into())];
    let server = MockServer::start(config).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(
        dir.path(),
        json!({
            "info": {"title": "Picky", "formats": [
                {"format_id": "hd", "ext": "mp4", "protocol": "https", "url": server.url(),
                 "vcodec": "avc1", "acodec": "mp4a", "height": 1080}
            ]},
            "download": {"bytes": 4096, "ext": "mp4"}
        }),
    );
    let (result, events) = run(&media(ytdlp), site_job(dir.path())).await;
    assert!(matches!(
        result.unwrap(),
        MediaOutcome::Completed { size: 4096, .. }
    ));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, MediaEvent::Warning(w) if w.contains("403"))),
        "{events:?}"
    );
    assert!(!dir.path().join("out").join("video.mp4.rdm-parts").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn drm_is_refused_and_playlists_are_listed() {
    let dir = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(
        dir.path(),
        json!({"error": "WARNING: x\nERROR: [example] 42: This video is DRM protected"}),
    );
    let page = Request::new(PAGE).unwrap();
    assert!(matches!(
        media(ytdlp).inspect_site(&page).await,
        Err(Error::Protected(_))
    ));

    let dir = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(
        dir.path(),
        json!({"info": {"_type": "playlist", "title": "Talks", "entries": [
            {"url": "https://videos.example/watch?v=1", "title": "One"},
            {"url": "https://videos.example/watch?v=2", "title": "Two"}
        ]}}),
    );
    let media = media(ytdlp);
    let info = media.inspect_site(&page).await.unwrap();
    assert_eq!(info.entries.len(), 2);
    // A playlist page is not one video to download.
    let (result, _) = run(&media, site_job(dir.path())).await;
    assert!(result.is_err());

    let missing = MediaEngine::new(engine(), Ffmpeg::disabled());
    assert!(matches!(
        missing.inspect_site(&page).await,
        Err(Error::YtDlpMissing)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_manager_reads_web_pages_with_ytdlp() {
    use rdm_manager::{
        Manager, NewDownload, ProxySetting, Settings, StartMode, Status, StreamChoice,
    };

    let size = 2 * MIB + 99;
    let server = MockServer::start(MockConfig::new(size).with_file(
        "/watch",
        "<html><title>Clip</title></html>",
        "text/html",
    ))
    .await
    .unwrap();
    let page = format!("http://{}/watch", server.addr());
    let dir = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(
        dir.path(),
        json!({"info": {
            "title": "A Clip: part 1/2", "webpage_url": page,
            "formats": [
                {"format_id": "140", "ext": "m4a", "protocol": "https", "url": "http://127.0.0.1:9/never",
                 "vcodec": "none", "acodec": "mp4a.40.2", "filesize": 1000},
                {"format_id": "18", "ext": "mp4", "protocol": "https", "url": server.url(),
                 "vcodec": "avc1", "acodec": "mp4a", "height": 360, "filesize": size}
            ]
        }}),
    );
    let manager = Manager::open_with_tools(
        &dir.path().join("data"),
        rdm_core::EngineConfig::default(),
        rdm_media::FfmpegConfig::Disabled,
        YtDlp::at(ytdlp),
    )
    .await
    .unwrap();
    let mut settings = Settings::with_directory(&dir.path().join("dl"));
    settings.proxy = ProxySetting::Direct;
    manager.update_settings(settings).unwrap();

    // An HTML page: yt-dlp reads it and its qualities are offered.
    let probe = manager
        .probe(&NewDownload {
            url: page.clone(),
            ..NewDownload::default()
        })
        .await
        .unwrap();
    assert!(probe.page);
    let site = probe.site.expect("site info");
    assert_eq!(site.choices[0].id, "18");
    assert_eq!(probe.file_name, "A Clip_ part 1_2.mp4");

    // With yt-dlp turned off, the page is just a page.
    let mut off = manager.settings();
    off.web_video.enabled = false;
    manager.update_settings(off).unwrap();
    let plain = manager
        .probe(&NewDownload {
            url: page.clone(),
            ..NewDownload::default()
        })
        .await
        .unwrap();
    assert!(plain.page && plain.site.is_none());
    let mut on = manager.settings();
    on.web_video.enabled = true;
    manager.update_settings(on).unwrap();

    let record = manager
        .add(NewDownload {
            url: page,
            title: Some(site.title.clone()),
            stream: Some(StreamChoice {
                site: true,
                variant: Some("18".into()),
                ..StreamChoice::default()
            }),
            start: StartMode::Now,
            ..NewDownload::default()
        })
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let done = loop {
        let r = manager.get(record.id).unwrap();
        if r.status == Status::Completed || r.status == Status::Failed {
            break r;
        }
        assert!(std::time::Instant::now() < deadline, "timed out: {r:?}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(done.status, Status::Completed, "{:?}", done.error);
    assert_content(&done.path(), 0, size);
    assert_eq!(
        done.stream.and_then(|s| s.label).as_deref(),
        Some("360p · H.264 · 2.1 MB")
    );
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_web_page_is_never_saved_as_the_download() {
    use rdm_manager::{Manager, NewDownload, ProxySetting, Settings};

    let server = MockServer::start(MockConfig::new(1000).with_file(
        "/watch",
        "<html>video page</html>",
        "text/html",
    ))
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::open_with_tools(
        &dir.path().join("data"),
        rdm_core::EngineConfig::default(),
        rdm_media::FfmpegConfig::Disabled,
        YtDlp::disabled(),
    )
    .await
    .unwrap();
    let mut settings = Settings::with_directory(&dir.path().join("dl"));
    settings.proxy = ProxySetting::Direct;
    manager.update_settings(settings).unwrap();
    let err = manager
        .add(NewDownload {
            url: format!("http://{}/watch", server.addr()),
            ..NewDownload::default()
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("yt-dlp"), "{err}");
    assert!(manager.list().is_empty());
    // Real files from the same server still work.
    manager
        .add(NewDownload {
            url: server.url(),
            ..NewDownload::default()
        })
        .await
        .unwrap();
    manager.shutdown().await;
}

/// The qualities listed on the page (the extension's video button) are shown
/// again in the Add dialog: yt-dlp reads the page once for both.
#[tokio::test(flavor = "multi_thread")]
async fn a_page_read_by_ytdlp_is_reused_for_a_while() {
    use rdm_manager::{Manager, NewDownload, StreamChoice};

    let dir = tempfile::tempdir().unwrap();
    let ytdlp = fake_ytdlp(
        dir.path(),
        json!({"info": {
            "title": "Clip", "webpage_url": PAGE,
            "formats": [
                {"format_id": "18", "ext": "mp4", "protocol": "https", "url": "https://cdn.example/v.mp4",
                 "vcodec": "avc1", "acodec": "mp4a", "height": 360, "filesize": 1000}
            ]
        }}),
    );
    let manager = Manager::open_with_tools(
        &dir.path().join("data"),
        rdm_core::EngineConfig::default(),
        rdm_media::FfmpegConfig::Disabled,
        YtDlp::at(ytdlp),
    )
    .await
    .unwrap();
    let page = |variant: Option<&str>| NewDownload {
        url: PAGE.into(),
        stream: Some(StreamChoice {
            site: true,
            variant: variant.map(str::to_owned),
            ..StreamChoice::default()
        }),
        ..NewDownload::default()
    };
    let reads = || {
        runs(dir.path())
            .iter()
            .filter(|args| args.iter().any(|a| a == "--dump-single-json"))
            .count()
    };

    let first = manager.probe(&page(None)).await.unwrap();
    let again = manager.probe(&page(Some("18"))).await.unwrap();
    assert_eq!(first.site, again.site);
    assert_eq!(reads(), 1, "the second probe reused the first reading");

    // Another page is read on its own.
    let mut other = page(None);
    other.url = "https://videos.example/watch?v=43".into();
    manager.probe(&other).await.unwrap();
    assert_eq!(reads(), 2);
}
