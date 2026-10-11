//! End-to-end tests of rdm-core against the mock server.

// Test helpers outside `#[test]` functions are not covered by clippy.toml's
// test exemptions.
#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use std::num::{NonZeroU64, NonZeroUsize};
use std::time::{Duration, Instant};

use common::{
    MIB, assert_content, completed, engine, finish, progress_reaches, sidecar_ranges, spec,
};
use rdm_core::{Destination, Engine, EngineConfig, Error, Event, Outcome, State};
use rdm_tests::{MockConfig, MockServer};

#[tokio::test(flavor = "multi_thread")]
async fn segmented_download_is_byte_exact() {
    let size = 5 * MIB + 12_345;
    let server = MockServer::start(MockConfig::new(size)).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 6))
        .unwrap();
    let (path, got_size, resumed) = completed(handle).await;
    assert_eq!(
        (path.as_path(), got_size, resumed),
        (dest.as_path(), size, 0)
    );
    assert_content(&dest, 0, size);
    assert!(
        server.peak_connections() > 1,
        "should use several connections"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn multiple_connections_beat_one_when_each_is_capped() {
    let size = 4 * MIB;
    let mut cfg = MockConfig::new(size);
    cfg.per_connection_rate = Some(MIB); // 1 MiB/s per connection
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();

    let mut timings = Vec::new();
    for conns in [1, 4] {
        let dest = dir.path().join(format!("c{conns}.bin"));
        let t = Instant::now();
        let handle = engine()
            .start(spec(&server, Destination::File(dest.clone()), conns))
            .unwrap();
        completed(handle).await;
        timings.push(t.elapsed());
        assert_content(&dest, 0, size);
    }
    let (single, multi) = (timings[0], timings[1]);
    eprintln!("1 connection: {single:?}, 4 connections: {multi:?}");
    assert!(
        multi * 2 < single,
        "4 connections ({multi:?}) should be well over 2x faster than 1 ({single:?})"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_then_start_resumes_without_refetching() {
    let size = 8 * MIB;
    let mut cfg = MockConfig::new(size);
    cfg.per_connection_rate = Some(2 * MIB);
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 2))
        .unwrap();
    let mut rx = handle.subscribe();
    progress_reaches(&mut rx, 2 * MIB).await;
    handle.stop();
    let downloaded = match finish(handle).await.unwrap() {
        Outcome::Stopped { downloaded, .. } => downloaded,
        other => panic!("expected stop, got {other:?}"),
    };
    let partial = rdm_core::inspect_partial(&dest).unwrap().unwrap();
    assert_eq!(partial.downloaded, downloaded);
    assert!(downloaded >= 2 * MIB && downloaded < size);
    let completed_ranges = sidecar_ranges(&dest);
    let requests_before = server.served_ranges().len();

    server.update(|c| c.per_connection_rate = None);
    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 4))
        .unwrap();
    let (_, _, resumed_from) = completed(handle).await;
    assert_eq!(resumed_from, downloaded);
    assert_content(&dest, 0, size);

    // Apart from the 1-byte probe, the second run must not request any
    // byte that was already on disk.
    let second_run: Vec<_> = server.served_ranges()[requests_before..]
        .iter()
        .copied()
        .filter(|&r| r != (0, 0))
        .collect();
    for (first, last) in &second_run {
        for (s, e) in &completed_ranges {
            assert!(
                last < s || first >= e,
                "re-requested {first}-{last}, which overlaps completed {s}..{e}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_resource_restarts_from_zero() {
    let size = 6 * MIB;
    let mut cfg = MockConfig::new(size);
    cfg.per_connection_rate = Some(2 * MIB);
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 2))
        .unwrap();
    let mut rx = handle.subscribe();
    progress_reaches(&mut rx, MIB).await;
    handle.stop();
    finish(handle).await.unwrap();

    server.update(|c| {
        c.seed = 1;
        c.etag = Some("\"v1\"".into());
        c.per_connection_rate = None;
    });
    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 4))
        .unwrap();
    let mut rx = handle.subscribe();
    let (_, _, resumed_from) = completed(handle).await;
    assert_eq!(resumed_from, 0);
    let mut warned = false;
    while let Ok(ev) = rx.try_recv() {
        if let Event::Warning(w) = ev {
            warned |= w.contains("ETag changed");
        }
    }
    assert!(warned, "a restart warning should be emitted");
    assert_content(&dest, 1, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_range_support_falls_back_to_one_stream() {
    let size = 3 * MIB + 7;
    let mut cfg = MockConfig::new(size);
    cfg.ranges = false;
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 8))
        .unwrap();
    let mut rx = handle.subscribe();
    completed(handle).await;
    assert_content(&dest, 0, size);
    assert_eq!(server.peak_connections(), 1);
    // The probe's 200 body is reused, so the file is fetched exactly once.
    assert_eq!(server.requests(), 1);
    assert_eq!(server.bytes_served(), size);
    let segmented = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|ev| match ev {
        Event::Started { segmented, .. } => Some(segmented),
        _ => None,
    });
    assert_eq!(segmented, Some(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_length_streams_to_eof() {
    let size = 2 * MIB + 3;
    let mut cfg = MockConfig::new(size);
    cfg.ranges = false;
    cfg.content_length = false;
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 4))
        .unwrap();
    let (_, got, _) = completed(handle).await;
    assert_eq!(got, size);
    assert_content(&dest, 0, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn directory_destination_uses_server_name_and_avoids_clobbering() {
    let size = MIB;
    let mut cfg = MockConfig::new(size);
    cfg.content_disposition = Some("attachment; filename=\"report.bin\"".into());
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("report.bin"), b"someone else's file").unwrap();

    let handle = engine()
        .start(spec(&server, Destination::Directory(dir.path().into()), 2))
        .unwrap();
    let (path, _, _) = completed(handle).await;
    assert_eq!(path, dir.path().join("report (1).bin"));
    assert_content(&path, 0, size);
    assert_eq!(
        std::fs::read(dir.path().join("report.bin")).unwrap(),
        b"someone else's file"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_file_is_not_overwritten_by_default() {
    let server = MockServer::start(MockConfig::new(1024)).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("taken.bin");
    std::fs::write(&dest, b"keep me").unwrap();

    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 2))
        .unwrap();
    let err = finish(handle).await.unwrap_err();
    assert!(matches!(err, Error::DestinationExists(_)), "{err:?}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"keep me");

    let mut s = spec(&server, Destination::File(dest.clone()), 2);
    s.overwrite = true;
    completed(engine().start(s).unwrap()).await;
    assert_content(&dest, 0, 1024);
}

#[tokio::test(flavor = "multi_thread")]
async fn speed_limit_is_enforced() {
    let size = 2 * MIB;
    let server = MockServer::start(MockConfig::new(size)).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let mut s = spec(&server, Destination::File(dest.clone()), 4);
    s.speed_limit = NonZeroU64::new(MIB);
    let t = Instant::now();
    completed(engine().start(s).unwrap()).await;
    let secs = t.elapsed().as_secs_f64();
    assert!((1.5..4.0).contains(&secs), "2 MiB at 1 MiB/s took {secs}s");
    assert_content(&dest, 0, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn global_limit_applies_across_downloads() {
    let size = MIB;
    let server = MockServer::start(MockConfig::new(size)).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let engine = engine();
    engine.set_global_speed_limit(NonZeroU64::new(MIB));

    let t = Instant::now();
    let a = engine
        .start(spec(&server, Destination::File(dir.path().join("a")), 2))
        .unwrap();
    let b = engine
        .start(spec(&server, Destination::File(dir.path().join("b")), 2))
        .unwrap();
    completed(a).await;
    completed(b).await;
    let secs = t.elapsed().as_secs_f64();
    assert!(
        (1.5..4.0).contains(&secs),
        "2 x 1 MiB at 1 MiB/s total took {secs}s"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_and_resume_in_process() {
    let size = 6 * MIB;
    let mut cfg = MockConfig::new(size);
    cfg.per_connection_rate = Some(2 * MIB);
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 3))
        .unwrap();
    let mut rx = handle.subscribe();
    progress_reaches(&mut rx, MIB).await;
    handle.pause();
    loop {
        if let Ok(Event::State(State::Paused)) = rx.recv().await {
            break;
        }
    }
    // Give the server a moment to notice the closed connections, then make
    // sure nothing more is transferred.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let served = server.bytes_served();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        server.bytes_served(),
        served,
        "transfer should stop while paused"
    );
    let partial = rdm_core::inspect_partial(&dest).unwrap().unwrap();
    assert!(
        partial.downloaded >= MIB,
        "pause should checkpoint progress"
    );

    server.update(|c| c.per_connection_rate = None);
    handle.resume();
    completed(handle).await;
    assert_content(&dest, 0, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_deletes_partial_data() {
    let mut cfg = MockConfig::new(8 * MIB);
    cfg.per_connection_rate = Some(MIB);
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 2))
        .unwrap();
    let mut rx = handle.subscribe();
    progress_reaches(&mut rx, 256 * 1024).await;
    handle.cancel();
    assert_eq!(finish(handle).await.unwrap(), Outcome::Cancelled);
    assert!(!dest.exists());
    assert!(!rdm_core::sidecar_path(&dest).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn per_host_connection_cap_is_respected() {
    let size = 4 * MIB;
    let mut cfg = MockConfig::new(size);
    cfg.per_connection_rate = Some(4 * MIB);
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig {
        max_connections_per_host: NonZeroUsize::new(2).unwrap(),
        ..EngineConfig::default()
    });

    let a = engine
        .start(spec(&server, Destination::File(dir.path().join("a")), 8))
        .unwrap();
    let b = engine
        .start(spec(&server, Destination::File(dir.path().join("b")), 8))
        .unwrap();
    completed(a).await;
    completed(b).await;
    // Two downloads asked for 8 connections each; a broken limiter shows up
    // to 16. The server notices a connection the client dropped (a segment
    // split by another worker) slightly after the client has released its
    // slot and opened the next one, so one extra may be observed.
    assert!(
        server.peak_connections() <= 3,
        "peak was {}, cap is 2",
        server.peak_connections()
    );
    assert_content(&dir.path().join("a"), 0, size);
    assert_content(&dir.path().join("b"), 0, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn http_errors_are_reported() {
    let server = MockServer::start(MockConfig::new(10)).await.unwrap();
    let request = common::request(&format!("http://{}/x", server.addr()));
    // The mock answers every path, so use a closed port for a network error.
    let closed = common::request("http://127.0.0.1:9/");
    let info = engine().probe(&request).await.unwrap();
    assert_eq!(info.size, Some(10));
    assert!(info.resumable);
    assert_eq!(info.filename, "x");
    let err = engine().probe(&closed).await.unwrap_err();
    assert!(err.is_retryable(), "{err:?}");
}
