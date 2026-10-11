//! Stream downloads end to end against the mock server. These run without
//! ffmpeg (it is disabled), so outputs are exact concatenations that can be
//! compared byte for byte. `ffmpeg.rs` covers merging when ffmpeg exists.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::Duration;

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockModeEncrypt, KeyIvInit};
use rdm_core::{Engine, EngineConfig, ProxyConfig, Request};
use rdm_media::{
    Error, Ffmpeg, MediaEngine, MediaEvent, MediaJob, MediaOutcome, OutputFormat, StreamKind,
};
use rdm_tests::{Faults, MockConfig, MockServer};

const TS: &str = "video/mp2t";
const M3U8: &str = "application/vnd.apple.mpegurl";

fn segment(seed: u64, len: usize) -> Vec<u8> {
    let mut v = vec![0; len];
    rdm_tests::fill(seed, 0, &mut v);
    v
}

fn media() -> MediaEngine {
    MediaEngine::new(Engine::new(EngineConfig::default()), Ffmpeg::disabled())
}

fn request(server: &MockServer, path: &str) -> Request {
    Request::new(&format!("http://{}{path}", server.addr()))
        .unwrap()
        .proxy(ProxyConfig::Direct)
}

async fn wait(handle: rdm_media::MediaHandle) -> rdm_media::Result<MediaOutcome> {
    tokio::time::timeout(Duration::from_secs(60), handle.wait())
        .await
        .expect("stream download did not finish")
}

fn media_playlist(prefix: &str, n: usize, ext: &str) -> String {
    let mut p = String::from(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n",
    );
    for i in 0..n {
        p.push_str(&format!("#EXTINF:2.0,\n{prefix}{i}.{ext}\n"));
    }
    p.push_str("#EXT-X-ENDLIST\n");
    p
}

/// A master playlist with a low and a high variant of `n` TS segments each.
fn two_variant_hls(n: usize) -> (MockConfig, Vec<u8>, Vec<u8>) {
    let master = "#EXTM3U\n\
        #EXT-X-STREAM-INF:BANDWIDTH=400000,RESOLUTION=640x360\nlo/index.m3u8\n\
        #EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1280x720\nhi/index.m3u8\n";
    let mut cfg = MockConfig::new(1)
        .with_file("/show/master.m3u8", master, M3U8)
        .with_file("/show/lo/index.m3u8", media_playlist("s", n, "ts"), M3U8)
        .with_file("/show/hi/index.m3u8", media_playlist("s", n, "ts"), M3U8);
    let (mut lo, mut hi) = (Vec::new(), Vec::new());
    for i in 0..n {
        let l = segment(100 + i as u64, 1000 + i * 17);
        let h = segment(200 + i as u64, 5000 + i * 31);
        lo.extend_from_slice(&l);
        hi.extend_from_slice(&h);
        cfg = cfg
            .with_file(&format!("/show/lo/s{i}.ts"), l, TS)
            .with_file(&format!("/show/hi/s{i}.ts"), h, TS);
    }
    (cfg, lo, hi)
}

fn completed(outcome: MediaOutcome) -> (std::path::PathBuf, u64) {
    match outcome {
        MediaOutcome::Completed { path, size } => (path, size),
        other => panic!("expected completion, got {other:?}"),
    }
}

fn assert_no_parts(output: &Path) {
    let parts = format!("{}.rdm-parts", output.display());
    assert!(
        !Path::new(&parts).exists(),
        "parts folder should be removed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn inspect_lists_variants_best_first() {
    let (cfg, ..) = two_variant_hls(3);
    let server = MockServer::start(cfg).await.unwrap();
    let m = media()
        .inspect(&request(&server, "/show/master.m3u8"))
        .await
        .unwrap();
    assert_eq!(m.kind, StreamKind::Hls);
    assert!(!m.live);
    assert_eq!(m.duration, Some(6.0));
    let labels: Vec<_> = m.variants.iter().map(|v| v.label.as_str()).collect();
    assert_eq!(labels, ["720p · 2.0 Mbps", "360p · 400 kbps"]);
    assert_eq!(m.variants[0].id, "v1");
}

#[tokio::test(flavor = "multi_thread")]
async fn hls_ts_best_variant_without_ffmpeg_is_saved_as_ts() {
    let (cfg, _lo, hi) = two_variant_hls(6);
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("show.mp4");

    let handle = media()
        .start(MediaJob::new(
            request(&server, "/show/master.m3u8"),
            output.clone(),
        ))
        .unwrap();
    let mut rx = handle.subscribe();
    let (path, size) = completed(wait(handle).await.unwrap());
    assert_eq!(path, dir.path().join("show.ts"));
    assert_eq!(std::fs::read(&path).unwrap(), hi);
    assert_eq!(size, hi.len() as u64);
    assert_no_parts(&output);
    assert!(
        !server.paths().iter().any(|p| p.starts_with("/show/lo/s")),
        "low variant untouched"
    );

    let mut warned = false;
    while let Ok(e) = rx.try_recv() {
        warned |= matches!(e, MediaEvent::Warning(w) if w.contains(".ts"));
    }
    assert!(warned, "missing ffmpeg should be explained");
}

#[tokio::test(flavor = "multi_thread")]
async fn chosen_variant_is_downloaded() {
    let (cfg, lo, _hi) = two_variant_hls(4);
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut job = MediaJob::new(
        request(&server, "/show/master.m3u8"),
        dir.path().join("lo.mp4"),
    );
    job.variant = Some("v0".into());
    let (path, _) = completed(wait(media().start(job).unwrap()).await.unwrap());
    assert_eq!(std::fs::read(path).unwrap(), lo);

    let mut bad = MediaJob::new(
        request(&server, "/show/master.m3u8"),
        dir.path().join("x.mp4"),
    );
    bad.variant = Some("v9".into());
    assert!(matches!(
        wait(media().start(bad).unwrap()).await,
        Err(Error::NoSuchVariant(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn aes128_segments_are_decrypted() {
    let key = [0x5au8; 16];
    let n = 5;
    let mut playlist = String::from(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:40\n#EXT-X-KEY:METHOD=AES-128,URI=\"/keys/k1\"\n",
    );
    let mut cfg =
        MockConfig::new(1).with_file("/keys/k1", key.to_vec(), "application/octet-stream");
    let mut plain = Vec::new();
    for i in 0..n {
        let seg = segment(300 + i as u64, 2000 + i * 7);
        plain.extend_from_slice(&seg);
        // From segment 3 on, a key tag with an explicit IV applies (a key
        // tag covers every following segment); earlier ones use the
        // media sequence number.
        if i == 3 {
            playlist.push_str("#EXT-X-KEY:METHOD=AES-128,URI=\"/keys/k1\",IV=0x000102030405060708090A0B0C0D0E0F\n");
        }
        let iv = if i >= 3 {
            0x000102030405060708090A0B0C0D0E0Fu128.to_be_bytes()
        } else {
            (40 + i as u128).to_be_bytes()
        };
        let enc = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into())
            .encrypt_padded_vec::<Pkcs7>(&seg);
        cfg = cfg.with_file(&format!("/enc/{i}.ts"), enc, TS);
        playlist.push_str(&format!("#EXTINF:2,\n/enc/{i}.ts\n"));
    }
    playlist.push_str("#EXT-X-ENDLIST\n");
    let server = MockServer::start(cfg.with_file("/enc/index.m3u8", playlist, M3U8))
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();

    let job = MediaJob::new(
        request(&server, "/enc/index.m3u8"),
        dir.path().join("enc.mp4"),
    );
    let (path, _) = completed(wait(media().start(job).unwrap()).await.unwrap());
    assert_eq!(std::fs::read(path).unwrap(), plain);
    assert_eq!(server.hits("/keys/k1"), 1, "the key is fetched once");
}

#[tokio::test(flavor = "multi_thread")]
async fn fmp4_with_init_segment_and_byte_ranges() {
    let init = segment(1, 700);
    let blob: Vec<u8> = segment(2, 9000);
    let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\"\n\
        #EXT-X-BYTERANGE:3000@0\n#EXTINF:2,\nmedia.m4s\n\
        #EXT-X-BYTERANGE:2500\n#EXTINF:2,\nmedia.m4s\n\
        #EXT-X-BYTERANGE:3500@5500\n#EXTINF:2,\nmedia.m4s\n#EXT-X-ENDLIST\n";
    let cfg = MockConfig::new(1)
        .with_file("/f/index.m3u8", playlist, M3U8)
        .with_file("/f/init.mp4", init.clone(), "video/mp4")
        .with_file("/f/media.m4s", blob.clone(), "video/iso.segment");
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();

    let job = MediaJob::new(request(&server, "/f/index.m3u8"), dir.path().join("f.mp4"));
    let (path, _) = completed(wait(media().start(job).unwrap()).await.unwrap());
    assert_eq!(path, dir.path().join("f.mp4"), "fMP4 needs no ffmpeg");
    let mut expected = init;
    expected.extend_from_slice(&blob[..9000]);
    assert_eq!(std::fs::read(path).unwrap(), expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn dash_template_video_track() {
    let mpd = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT8S">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/seg-$Number%03d$.m4s" startNumber="1" duration="2" timescale="1"/>
      <Representation id="hd" bandwidth="3000000" width="1280" height="720" codecs="avc1.64001f"/>
      <Representation id="sd" bandwidth="600000" width="640" height="360" codecs="avc1.4d401e"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
    let mut cfg = MockConfig::new(1).with_file("/d/manifest.mpd", mpd, "application/dash+xml");
    let mut expected = segment(10, 800);
    cfg = cfg.with_file("/d/hd/init.mp4", expected.clone(), "video/mp4");
    for i in 1..=4 {
        let s = segment(10 + i, 3000);
        expected.extend_from_slice(&s);
        cfg = cfg.with_file(&format!("/d/hd/seg-{i:03}.m4s"), s, "video/iso.segment");
    }
    let server = MockServer::start(cfg).await.unwrap();
    let engine = media();
    let m = engine
        .inspect(&request(&server, "/d/manifest.mpd"))
        .await
        .unwrap();
    assert_eq!(m.kind, StreamKind::Dash);
    assert_eq!(m.variants[0].label, "720p · 3.0 Mbps · avc1");

    let dir = tempfile::tempdir().unwrap();
    let job = MediaJob::new(
        request(&server, "/d/manifest.mpd"),
        dir.path().join("d.mp4"),
    );
    let (path, _) = completed(wait(engine.start(job).unwrap()).await.unwrap());
    assert_eq!(std::fs::read(path).unwrap(), expected);
    assert!(!server.paths().iter().any(|p| p.starts_with("/d/sd/")));
}

#[tokio::test(flavor = "multi_thread")]
async fn live_and_protected_streams_are_refused_before_downloading() {
    let live = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXTINF:2,\nb.ts\n";
    let drm = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://key\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n";
    let widevine = r#"<MPD type="static" mediaPresentationDuration="PT4S"><Period><AdaptationSet mimeType="video/mp4">
        <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/>
        <Representation id="v" bandwidth="1"><SegmentTemplate media="$Number$.m4s" duration="2"/></Representation>
        </AdaptationSet></Period></MPD>"#;
    let cfg = MockConfig::new(1)
        .with_file("/live.m3u8", live, M3U8)
        .with_file("/drm.m3u8", drm, M3U8)
        .with_file("/wv.mpd", widevine, "application/dash+xml");
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();

    let run = |path: &str| MediaJob::new(request(&server, path), dir.path().join("out.mp4"));
    assert!(matches!(
        wait(media().start(run("/live.m3u8")).unwrap()).await,
        Err(Error::Live)
    ));
    assert!(matches!(
        wait(media().start(run("/drm.m3u8")).unwrap()).await,
        Err(Error::Protected(_))
    ));
    assert!(matches!(
        wait(media().start(run("/wv.mpd")).unwrap()).await,
        Err(Error::Protected(_))
    ));
    assert!(
        server
            .paths()
            .iter()
            .all(|p| p.ends_with(".m3u8") || p.ends_with(".mpd")),
        "no segment may be fetched: {:?}",
        server.paths()
    );
    assert!(matches!(
        media().inspect(&request(&server, "/not-a-playlist")).await,
        Err(Error::NotAStream)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn separate_audio_without_ffmpeg_keeps_parts_for_later() {
    let master = "#EXTM3U\n\
        #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",DEFAULT=YES,URI=\"a/index.m3u8\"\n\
        #EXT-X-STREAM-INF:BANDWIDTH=1000000,AUDIO=\"aud\"\nv/index.m3u8\n";
    let mut cfg = MockConfig::new(1)
        .with_file("/m/master.m3u8", master, M3U8)
        .with_file("/m/v/index.m3u8", media_playlist("v", 2, "ts"), M3U8)
        .with_file("/m/a/index.m3u8", media_playlist("a", 2, "aac"), M3U8);
    for i in 0..2 {
        cfg = cfg
            .with_file(&format!("/m/v/v{i}.ts"), segment(i, 500), TS)
            .with_file(&format!("/m/a/a{i}.aac"), segment(50 + i, 200), "audio/aac");
    }
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("m.mp4");

    let job = MediaJob::new(request(&server, "/m/master.m3u8"), output.clone());
    let err = wait(media().start(job).unwrap()).await.unwrap_err();
    assert!(matches!(err, Error::FfmpegMissing(_)), "{err:?}");
    let parts = std::path::PathBuf::from(format!("{}.rdm-parts", output.display()));
    assert!(parts.join("audio.aac").exists() && parts.join("video.ts").exists());

    // Audio-only output with a separate audio track downloads no video
    // segments (the variant playlist itself is read while inspecting).
    let mut audio = MediaJob::new(request(&server, "/m/master.m3u8"), dir.path().join("a.m4a"));
    audio.format = OutputFormat::M4a;
    let before = server.paths().len();
    let err = wait(media().start(audio).unwrap()).await.unwrap_err();
    assert!(matches!(err, Error::FfmpegMissing(_)));
    assert!(
        !server.paths()[before..]
            .iter()
            .any(|p| p.starts_with("/m/v/v"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_then_resume_fetches_each_segment_once() {
    let n = 20;
    let mut cfg = MockConfig::new(1).with_file("/r/index.m3u8", media_playlist("s", n, "ts"), M3U8);
    let mut expected = Vec::new();
    for i in 0..n {
        let s = segment(400 + i as u64, 1500);
        expected.extend_from_slice(&s);
        cfg = cfg.with_file(&format!("/r/s{i}.ts"), s, TS);
    }
    cfg.faults = Faults {
        latency: Some(Duration::from_millis(100)),
        ..Faults::default()
    };
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("r.mp4");
    let job = || {
        let mut j = MediaJob::new(request(&server, "/r/index.m3u8"), output.clone());
        j.concurrency = 2;
        j
    };

    let handle = media().start(job()).unwrap();
    let mut rx = handle.subscribe();
    let control = handle.control();
    loop {
        if let Ok(MediaEvent::Progress(p)) = rx.recv().await
            && p.segments_done >= 4
        {
            break;
        }
    }
    control.stop();
    assert_eq!(wait(handle).await.unwrap(), MediaOutcome::Stopped);
    let fetched_before = server
        .paths()
        .iter()
        .filter(|p| p.starts_with("/r/s"))
        .count();
    assert!(fetched_before < n, "stopped early ({fetched_before})");

    server.update(|c| c.faults = Faults::default());
    let handle = media().start(job()).unwrap();
    let mut rx = handle.subscribe();
    let (path, _) = completed(wait(handle).await.unwrap());
    assert_eq!(std::fs::read(path).unwrap(), expected);
    let resumed = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|e| match e {
        MediaEvent::Started { resumed, .. } => Some(resumed),
        _ => None,
    });
    assert!(resumed.unwrap() >= 4);
    for i in 0..n {
        assert!(
            server.hits(&format!("/r/s{i}.ts")) <= 1 + usize::from(i < 2),
            "segment {i} refetched"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_deletes_parts() {
    let n = 10;
    let mut cfg = MockConfig::new(1).with_file("/c/index.m3u8", media_playlist("s", n, "ts"), M3U8);
    for i in 0..n {
        cfg = cfg.with_file(&format!("/c/s{i}.ts"), segment(i as u64, 1000), TS);
    }
    cfg.faults.latency = Some(Duration::from_millis(150));
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("c.mp4");
    let mut job = MediaJob::new(request(&server, "/c/index.m3u8"), output.clone());
    job.concurrency = 1;
    let handle = media().start(job).unwrap();
    let mut rx = handle.subscribe();
    loop {
        if let Ok(MediaEvent::Progress(p)) = rx.recv().await
            && p.segments_done >= 1
        {
            break;
        }
    }
    handle.control().cancel();
    assert_eq!(wait(handle).await.unwrap(), MediaOutcome::Cancelled);
    assert_no_parts(&output);
}
