//! rdm-manager end to end: queues, categories, persistence and lifecycle,
//! against the mock server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::Duration;

use rdm_core::EngineConfig;
use rdm_manager::{
    DownloadRecord, Error, ExportFormat, FfmpegConfig, Id, MAIN_QUEUE, Manager, ManagerEvent,
    NewDownload, ProxySetting, Queue, Settings, StartMode, Status, WhenDone,
};
use rdm_tests::{ErrorFault, MockConfig, MockServer, verify_file};

const MIB: u64 = 1 << 20;

async fn open(dir: &Path) -> Manager {
    let m = Manager::open_with(
        &dir.join("data"),
        EngineConfig {
            progress_interval: Duration::from_millis(50),
            checkpoint_interval: Duration::from_millis(200),
            ..EngineConfig::default()
        },
        FfmpegConfig::Disabled,
    )
    .await
    .unwrap();
    if !dir.join("configured").exists() {
        let mut s = Settings::with_directory(&dir.join("dl"));
        s.proxy = ProxySetting::Direct;
        s.min_split = 256 * 1024;
        m.update_settings(s).unwrap();
        std::fs::write(dir.join("configured"), b"").unwrap();
    }
    m
}

/// Most tests add the same URL several times on purpose.
fn new(url: &str) -> NewDownload {
    NewDownload {
        url: url.into(),
        allow_duplicate: true,
        ..NewDownload::default()
    }
}

async fn wait_for(
    m: &Manager,
    id: Id,
    what: &str,
    pred: impl Fn(&DownloadRecord) -> bool,
) -> DownloadRecord {
    for _ in 0..1500 {
        if let Some(r) = m.get(id).filter(&pred) {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("download {id} never became {what}: {:?}", m.get(id));
}

async fn completed(m: &Manager, id: Id) -> DownloadRecord {
    wait_for(m, id, "completed", |r| {
        r.status == Status::Completed || r.status == Status::Failed
    })
    .await
}

fn assert_file(r: &DownloadRecord, seed: u64, size: u64) {
    assert_eq!(r.status, Status::Completed, "{r:?}");
    if let Err(e) = verify_file(&r.path(), seed, size).unwrap() {
        panic!("{}: {e}", r.path().display());
    }
}

async fn mock(size: u64, f: impl FnOnce(&mut MockConfig)) -> MockServer {
    let mut cfg = MockConfig::new(size);
    f(&mut cfg);
    MockServer::start(cfg).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn add_probes_and_files_into_category_folder() {
    let server = mock(2 * MIB, |c| {
        c.content_disposition = Some("attachment; filename=\"clip.mp4\"".into());
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let mut events = m.subscribe();

    let r = m.add(new(&server.url())).await.unwrap();
    assert_eq!(r.file_name, "clip.mp4");
    assert_eq!(r.directory, dir.path().join("dl").join("Video"));
    assert_eq!(r.size, Some(2 * MIB));
    let r = completed(&m, r.id).await;
    assert_file(&r, 0, 2 * MIB);
    assert_eq!(r.resumable, Some(true));
    assert!(r.completed_at.is_some());

    let mut kinds = Vec::new();
    while let Ok(e) = events.try_recv() {
        kinds.push(match e {
            ManagerEvent::Added(_) => "added",
            ManagerEvent::Updated(r) if r.status == Status::Completed => "completed",
            _ => continue,
        });
    }
    assert_eq!(kinds.first(), Some(&"added"));
    assert_eq!(kinds.last(), Some(&"completed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn file_names_never_collide() {
    let server = mock(MIB, |_| {}).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let docs = dir.path().join("dl").join("Documents");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("report.pdf"), b"existing").unwrap();

    let mut n = new(&server.url());
    n.file_name = Some("report.pdf".into());
    n.start = StartMode::Paused;
    let a = m.add(n.clone()).await.unwrap();
    let b = m.add(n).await.unwrap();
    assert_eq!(a.file_name, "report (1).pdf");
    assert_eq!(b.file_name, "report (2).pdf");
    assert_eq!(a.status, Status::Stopped);
    assert_eq!(std::fs::read(docs.join("report.pdf")).unwrap(), b"existing");
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_respects_its_limit_and_drains() {
    let server = mock(2 * MIB, |c| c.per_connection_rate = Some(2 * MIB)).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let mut s = m.settings();
    s.queues[0].max_concurrent = 2;
    m.update_settings(s).unwrap();

    let mut ids = Vec::new();
    for i in 0..4 {
        let mut n = new(&server.url());
        n.file_name = Some(format!("f{i}.bin"));
        n.start = StartMode::Queue;
        n.connections = Some(2);
        ids.push(m.add(n).await.unwrap().id);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        m.list().iter().all(|r| r.status == Status::Queued),
        "queue not started yet"
    );
    assert_eq!(m.queues()[0].waiting, 4);

    m.start_queue(MAIN_QUEUE).unwrap();
    let mut peak = 0;
    loop {
        let list = m.list();
        let running = list
            .iter()
            .filter(|r| r.status == Status::Downloading)
            .count();
        peak = peak.max(running);
        assert!(running <= 2, "queue limit exceeded: {running}");
        if list.iter().all(|r| r.status == Status::Completed) {
            break;
        }
        assert!(list.iter().all(|r| r.status != Status::Failed), "{list:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(peak, 2);
    for id in ids {
        assert_file(&m.get(id).unwrap(), 0, 2 * MIB);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_keeps_progress_and_start_resumes() {
    let size = 6 * MIB;
    let server = mock(size, |c| c.per_connection_rate = Some(2 * MIB)).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;

    let r = m.add(new(&server.url())).await.unwrap();
    wait_for(&m, r.id, "partly downloaded", |r| r.downloaded >= MIB).await;
    m.stop(r.id).unwrap();
    let stopped = wait_for(&m, r.id, "stopped", |r| r.status == Status::Stopped).await;
    assert!(stopped.downloaded >= MIB && stopped.downloaded < size);

    server.update(|c| c.per_connection_rate = None);
    m.start(r.id).unwrap();
    assert_file(&completed(&m, r.id).await, 0, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn state_survives_restart() {
    let size = 8 * MIB;
    let server = mock(size, |c| c.per_connection_rate = Some(MIB)).await;
    let dir = tempfile::tempdir().unwrap();

    let (done_id, partial_id) = {
        let m = open(dir.path()).await;
        let mut quick = new(&server.url());
        quick.file_name = Some("quick.bin".into());
        quick.speed_limit = None;
        server.update(|c| c.per_connection_rate = None);
        let done = m.add(quick).await.unwrap();
        completed(&m, done.id).await;

        server.update(|c| c.per_connection_rate = Some(MIB));
        let mut slow = new(&server.url());
        slow.file_name = Some("slow.bin".into());
        slow.referer = Some("https://example.com/page".into());
        let partial = m.add(slow).await.unwrap();
        wait_for(&m, partial.id, "partly downloaded", |r| r.downloaded >= MIB).await;
        // Quit while it is still running.
        m.shutdown().await;
        (done.id, partial.id)
    };

    let m = open(dir.path()).await;
    let list = m.list();
    assert_eq!(list.len(), 2);
    let done = m.get(done_id).unwrap();
    assert_file(&done, 0, size);
    let partial = m.get(partial_id).unwrap();
    assert_eq!(
        partial.status,
        Status::Stopped,
        "running downloads come back stopped"
    );
    assert!(partial.downloaded >= MIB, "{partial:?}");
    assert_eq!(partial.referer.as_deref(), Some("https://example.com/page"));
    assert_eq!(m.settings().min_split, 256 * 1024, "settings persisted");

    server.update(|c| c.per_connection_rate = None);
    m.start(partial_id).unwrap();
    assert_file(&completed(&m, partial_id).await, 0, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_with_and_without_deleting_files() {
    let server = mock(4 * MIB, |_| {}).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;

    let keep = m.add(new(&server.url())).await.unwrap();
    let keep = completed(&m, keep.id).await;
    m.remove(keep.id, false).unwrap();
    assert!(m.get(keep.id).is_none());
    assert!(keep.path().exists(), "file kept");

    let gone = m.add(new(&server.url())).await.unwrap();
    let gone = completed(&m, gone.id).await;
    m.remove(gone.id, true).unwrap();
    assert!(!gone.path().exists(), "file deleted");

    // Removing a running download with its data.
    server.update(|c| c.per_connection_rate = Some(MIB));
    let mut events = m.subscribe();
    let running = m.add(new(&server.url())).await.unwrap();
    wait_for(&m, running.id, "started", |r| r.downloaded > 0).await;
    m.remove(running.id, true).unwrap();
    let removed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(ManagerEvent::Removed(id)) = events.recv().await
                && id == running.id
            {
                return;
            }
        }
    })
    .await;
    assert!(removed.is_ok(), "no Removed event");
    assert!(!running.path().exists());
    assert!(!rdm_core::sidecar_path(&running.path()).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn failures_are_recorded_and_retry_works() {
    let server = mock(MIB, |c| c.faults.error = Some(ErrorFault::always(404))).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;

    // Probing a missing file fails the add itself.
    assert!(m.add(new(&server.url())).await.is_err());

    let mut n = new(&server.url());
    n.file_name = Some("x.bin".into());
    let r = m.add(n).await.unwrap();
    let failed = completed(&m, r.id).await;
    assert_eq!(failed.status, Status::Failed);
    assert!(
        failed.error.as_deref().unwrap().contains("404"),
        "{failed:?}"
    );

    server.update(|c| c.faults = Default::default());
    m.start(r.id).unwrap();
    let r = completed(&m, r.id).await;
    assert_file(&r, 0, MIB);
    assert_eq!(r.error, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_queue_puts_its_downloads_back_in_line() {
    let server = mock(8 * MIB, |c| c.per_connection_rate = Some(MIB)).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;

    let mut n = new(&server.url());
    n.start = StartMode::Queue;
    let r = m.add(n).await.unwrap();
    m.start_queue(MAIN_QUEUE).unwrap();
    wait_for(&m, r.id, "downloading", |r| r.status == Status::Downloading).await;
    m.stop_queue(MAIN_QUEUE).unwrap();
    wait_for(&m, r.id, "queued", |r| r.status == Status::Queued).await;
    assert!(!m.queues()[0].running);
    m.stop(r.id).unwrap();
    assert_eq!(m.get(r.id).unwrap().status, Status::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_are_validated_and_queue_removal_moves_downloads() {
    let server = mock(MIB, |_| {}).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;

    let mut bad = m.settings();
    bad.connections = 0;
    assert!(m.update_settings(bad).is_err());

    let mut s = m.settings();
    s.queues.push(Queue {
        id: 2,
        name: "Night".into(),
        max_concurrent: 1,
        schedule: Default::default(),
        when_done: Default::default(),
    });
    m.update_settings(s).unwrap();
    let mut n = new(&server.url());
    n.queue_id = Some(2);
    n.start = StartMode::Paused;
    n.file_name = Some("n.bin".into());
    let r = m.add(n).await.unwrap();
    assert_eq!(r.queue_id, 2);

    let mut s = m.settings();
    s.queues.retain(|q| q.id != 2);
    m.update_settings(s).unwrap();
    assert_eq!(m.get(r.id).unwrap().queue_id, MAIN_QUEUE);
}

#[tokio::test(flavor = "multi_thread")]
async fn cookies_are_used_but_never_stored() {
    let secret = "auth=very-secret-cookie-value";
    let server = mock(MIB, |c| c.faults.require_cookie = Some(secret.into())).await;
    let dir = tempfile::tempdir().unwrap();
    {
        let m = open(dir.path()).await;
        let mut n = new(&server.url());
        n.cookies = Some(format!("theme=dark; {secret}"));
        let r = m.add(n).await.unwrap();
        assert_file(&completed(&m, r.id).await, 0, MIB);
        m.shutdown().await;
    }
    for entry in std::fs::read_dir(dir.path().join("data")).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        let found = bytes.windows(secret.len()).any(|w| w == secret.as_bytes());
        assert!(!found, "cookie found on disk");
    }
}

fn hls_server_config(n: usize) -> (MockConfig, Vec<u8>) {
    let mut playlist = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:2\n");
    let mut cfg = MockConfig::new(1);
    let mut expected = Vec::new();
    for i in 0..n {
        let mut seg = vec![0u8; 3000 + i];
        rdm_tests::fill(900 + i as u64, 0, &mut seg);
        expected.extend_from_slice(&seg);
        playlist.push_str(&format!("#EXTINF:2,\nseg{i}.ts\n"));
        cfg = cfg.with_file(&format!("/live/seg{i}.ts"), seg, "video/mp2t");
    }
    playlist.push_str("#EXT-X-ENDLIST\n");
    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1500000,RESOLUTION=1280x720\nindex.m3u8\n";
    cfg = cfg
        .with_file(
            "/live/index.m3u8",
            playlist,
            "application/vnd.apple.mpegurl",
        )
        .with_file("/live/master.m3u8", master, "application/vnd.apple.mpegurl");
    (cfg, expected)
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_are_detected_named_by_title_and_downloaded() {
    let (cfg, expected) = hls_server_config(5);
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let url = format!("http://{}/live/master.m3u8", server.addr());

    let probe = m.probe(&new(&url)).await.unwrap();
    let manifest = probe.stream.expect("a stream manifest");
    assert_eq!(manifest.variants[0].label, "720p · 1.5 Mbps");
    assert_eq!(probe.file_name, format!("{} video.mp4", server.addr().ip()));

    let mut n = new(&url);
    n.title = Some("Conference: Keynote / Day 1".into());
    let r = m.add(n).await.unwrap();
    assert_eq!(r.file_name, "Conference_ Keynote _ Day 1.mp4");
    assert!(r.stream.is_some());
    assert_eq!(r.directory, dir.path().join("dl").join("Video"));

    let done = completed(&m, r.id).await;
    assert_eq!(done.status, Status::Completed, "{:?}", done.error);
    // ffmpeg is disabled in tests, so the stream stays MPEG-TS.
    assert_eq!(done.file_name, "Conference_ Keynote _ Day 1.ts");
    assert_eq!(std::fs::read(done.path()).unwrap(), expected);
    assert_eq!(
        done.stream.unwrap().label.as_deref(),
        Some("720p · 1.5 Mbps")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn protected_streams_are_refused_when_probing() {
    let drm =
        "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://k\"\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n";
    let server = mock(1, |_| {}).await;
    drop(server);
    let mut cfg = MockConfig::new(1);
    cfg = cfg.with_file("/drm.m3u8", drm, "application/vnd.apple.mpegurl");
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let err = m
        .probe(&new(&format!("http://{}/drm.m3u8", server.addr())))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("protected content"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicates_are_reported_unless_allowed() {
    let server = mock(MIB, |_| {}).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let url = server.url();

    let first = m.add(new(&url)).await.unwrap();
    completed(&m, first.id).await;

    let strict = NewDownload {
        url: format!("{url}#again"),
        ..NewDownload::default()
    };
    let probe = m.probe(&strict).await.unwrap();
    assert_eq!(probe.duplicate.as_ref().map(|d| d.id), Some(first.id));
    match m.add(strict.clone()).await {
        Err(Error::Duplicate(existing)) => assert_eq!(existing.id, first.id),
        other => panic!("{other:?}"),
    }
    let again = m
        .add(NewDownload {
            allow_duplicate: true,
            ..strict
        })
        .await
        .unwrap();
    assert_ne!(
        again.file_name, first.file_name,
        "a second copy gets its own name"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn checksums_are_verified() {
    let size = MIB;
    let content = rdm_tests::expected_content(0, size);
    let good = rdm_manager::Checksum {
        algorithm: rdm_manager::Algorithm::Sha256,
        hex: sha256_hex(&content),
    };
    let server = mock(size, |_| {}).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;

    assert!(matches!(
        m.add(NewDownload {
            checksum: Some("md5:nope".into()),
            ..new(&server.url())
        })
        .await,
        Err(Error::InvalidChecksum(_))
    ));

    let ok = m
        .add(NewDownload {
            checksum: Some(good.to_string().to_uppercase().replace("SHA-256", "sha256")),
            ..new(&server.url())
        })
        .await
        .unwrap();
    let ok = wait_for(&m, ok.id, "verified", |r| r.checksum_ok.is_some()).await;
    assert_eq!((ok.status, ok.checksum_ok), (Status::Completed, Some(true)));

    let bad = m
        .add(NewDownload {
            checksum: Some(format!("sha-256:{}", "0".repeat(64))),
            ..new(&server.url())
        })
        .await
        .unwrap();
    let bad = wait_for(&m, bad.id, "verified", |r| r.checksum_ok.is_some()).await;
    assert_eq!((bad.status, bad.checksum_ok), (Status::Failed, Some(false)));
    assert!(bad.error.as_deref().unwrap().contains("checksum mismatch"));
    assert!(bad.path().exists(), "the file is kept for inspection");

    let hex = m
        .compute_checksum(ok.id, rdm_manager::Algorithm::Sha256)
        .await
        .unwrap();
    assert_eq!(hex, good.hex);
}

fn sha256_hex(data: &[u8]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("x");
    std::fs::write(&f, data).unwrap();
    rdm_manager::hash_file(&f, rdm_manager::Algorithm::Sha256).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn server_published_digest_is_verified_automatically() {
    let size = MIB;
    let content = rdm_tests::expected_content(0, size);
    let digest = |bytes: &[u8]| {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x");
        std::fs::write(&f, bytes).unwrap();
        let hex = rdm_manager::hash_file(&f, rdm_manager::Algorithm::Sha256).unwrap();
        let raw: Vec<u8> = (0..32)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        format!("sha-256=:{}:", rdm_tests::base64(&raw))
    };
    let server = mock(size, |c| {
        c.extra_headers = vec![("Repr-Digest".into(), digest(&content))]
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;

    let p = m.probe(&new(&server.url())).await.unwrap();
    assert!(
        p.server_checksum
            .as_deref()
            .unwrap()
            .starts_with("sha-256:")
    );
    let r = m.add(new(&server.url())).await.unwrap();
    let r = wait_for(&m, r.id, "verified", |r| r.checksum_ok.is_some()).await;
    assert_eq!((r.status, r.checksum_ok), (Status::Completed, Some(true)));

    // A server whose digest does not match what it sends is caught.
    server.update(|c| c.extra_headers = vec![("Repr-Digest".into(), digest(b"something else"))]);
    let r = m.add(new(&server.url())).await.unwrap();
    let r = wait_for(&m, r.id, "verified", |r| r.checksum_ok.is_some()).await;
    assert_eq!((r.status, r.checksum_ok), (Status::Failed, Some(false)));
}

#[tokio::test(flavor = "multi_thread")]
async fn schedules_start_and_stop_queues() {
    let server = mock(8 * MIB, |c| c.per_connection_rate = Some(MIB)).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let n = NewDownload {
        start: StartMode::Queue,
        ..new(&server.url())
    };
    let r = m.add(n).await.unwrap();

    // Schedule 01:00–01:30 local time, once; then drive the clock across it.
    let tz = jiff::tz::TimeZone::system();
    let day = jiff::Zoned::now().date().tomorrow().unwrap();
    let at = |h: i8, mi: i8| day.at(h, mi, 0, 0).to_zoned(tz.clone()).unwrap();
    let mut s = m.settings();
    s.queues[0].schedule = rdm_manager::Schedule {
        enabled: true,
        start: Some("01:00".into()),
        stop: Some("01:30".into()),
        days: vec![],
        once: true,
    };
    m.update_settings(s).unwrap();

    m.check_schedules_at(at(0, 59));
    assert!(!m.queues()[0].running);
    m.check_schedules_at(at(1, 0));
    assert!(m.queues()[0].running, "started at 01:00");
    wait_for(&m, r.id, "downloading", |r| r.status == Status::Downloading).await;

    m.check_schedules_at(at(1, 30));
    assert!(!m.queues()[0].running, "stopped at 01:30");
    wait_for(&m, r.id, "back in line", |r| r.status == Status::Queued).await;
    assert!(
        !m.settings().queues[0].schedule.enabled,
        "a one-time schedule disables itself"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn antivirus_hook_runs_on_finished_files() {
    let server = mock(MIB, |_| {}).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let log = dir.path().join("scanned.txt");
    let mut s = m.settings();
    // Records the scanned path, then reports a "virus" for files ending in 2.
    s.antivirus = Some(format!(
        "sh -c 'echo \"$0\" >> {}; case \"$0\" in *2.bin) echo FOUND: Test-Signature; exit 1;; esac' {{file}}",
        log.display()
    ));
    m.update_settings(s).unwrap();

    let clean = m
        .add(NewDownload {
            file_name: Some("one.bin".into()),
            ..new(&server.url())
        })
        .await
        .unwrap();
    let flagged = m
        .add(NewDownload {
            file_name: Some("two2.bin".into()),
            ..new(&server.url())
        })
        .await
        .unwrap();
    let clean = wait_for(&m, clean.id, "scanned", |r| r.scan.is_some()).await;
    let flagged = wait_for(&m, flagged.id, "scanned", |r| r.scan.is_some()).await;
    assert!(clean.scan.unwrap().contains("no problems"));
    let report = flagged.scan.unwrap();
    assert!(
        report.contains("exit 1") && report.contains("Test-Signature"),
        "{report}"
    );
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("one.bin") && logged.contains("two2.bin"));
}

#[tokio::test(flavor = "multi_thread")]
async fn import_and_export() {
    let server = mock(MIB, |_| {}).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let base = format!("http://{}", server.addr());

    let text = format!("# list\n{base}/a.zip\n{base}/b.pdf\n{base}/a.zip\nnot-a-url\n");
    let report = m.import(&text, StartMode::Paused).await;
    assert_eq!(
        (report.added, report.duplicates, report.invalid.len()),
        (2, 0, 1)
    );
    let again = m.import(&text, StartMode::Paused).await;
    assert_eq!((again.added, again.duplicates), (0, 2));

    let names: Vec<_> = m.list().into_iter().map(|r| r.file_name).collect();
    assert_eq!(names, ["a.zip", "b.pdf"]);
    let urls = m.export(ExportFormat::Urls, None);
    assert_eq!(urls, format!("{base}/a.zip\n{base}/b.pdf\n"));

    // JSON moves the list to another install.
    let json = m.export(ExportFormat::Json, None);
    let other_dir = tempfile::tempdir().unwrap();
    let other = open(other_dir.path()).await;
    let r = other.import(&json, StartMode::Paused).await;
    assert_eq!(r.added, 2);
    assert_eq!(other.list()[1].file_name, "b.pdf");
}

#[tokio::test(flavor = "multi_thread")]
async fn finishing_queue_reports_its_action() {
    let server = mock(MIB, |_| {}).await;
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let mut s = m.settings();
    s.queues[0].when_done = WhenDone::Shutdown;
    m.update_settings(s).unwrap();
    let mut events = m.subscribe();

    for name in ["q1.bin", "q2.bin"] {
        let n = NewDownload {
            file_name: Some(name.into()),
            start: StartMode::Queue,
            ..new(&server.url())
        };
        m.add(n).await.unwrap();
    }
    m.start_queue(MAIN_QUEUE).unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(ManagerEvent::QueueFinished {
                queue_id,
                when_done,
                ..
            }) = events.recv().await
            {
                return (queue_id, when_done);
            }
        }
    })
    .await
    .expect("queue never finished");
    assert_eq!(finished, (MAIN_QUEUE, WhenDone::Shutdown));
    assert!(m.list().iter().all(|r| r.status == Status::Completed));
    assert!(!m.queues()[0].running, "a finished queue stops");
}
