//! rdm-manager end to end: queues, categories, persistence and lifecycle,
//! against the mock server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::Duration;

use rdm_core::EngineConfig;
use rdm_manager::{
    DownloadRecord, Id, MAIN_QUEUE, Manager, ManagerEvent, NewDownload, ProxySetting, Queue,
    Settings, StartMode, Status,
};
use rdm_tests::{ErrorFault, MockConfig, MockServer, verify_file};

const MIB: u64 = 1 << 20;

async fn open(dir: &Path) -> Manager {
    let m = Manager::open(
        &dir.join("data"),
        EngineConfig {
            progress_interval: Duration::from_millis(50),
            checkpoint_interval: Duration::from_millis(200),
            ..EngineConfig::default()
        },
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

fn new(url: &str) -> NewDownload {
    NewDownload {
        url: url.into(),
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
