//! rdm-core against a misbehaving server: dropped and stalled connections,
//! latency, error statuses, wrong lengths, changing resources, redirects,
//! cookies, auth and proxies.
//!
//! Every test bounds its runtime (see `common::finish`) and avoids tight
//! timing windows so it is reliable on all CI operating systems.

#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use common::{
    MIB, assert_content, completed, drain, engine, engine_with, failed, fast_retry,
    progress_reaches, request, retries, spec, spec_for, warnings,
};
use rdm_core::{Credentials, Destination, Error, Event, ProxyConfig};
use rdm_tests::{ErrorFault, MockConfig, MockServer};

async fn server(f: impl FnOnce(&mut MockConfig)) -> MockServer {
    let mut cfg = MockConfig::new(4 * MIB);
    f(&mut cfg);
    MockServer::start(cfg).await.unwrap()
}

fn status_of(err: &Error) -> Option<u16> {
    match err {
        Error::HttpStatus { status, .. } => Some(*status),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Connection-level faults
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn dropped_connections_are_retried_and_file_is_exact() {
    let size = 8 * MIB + 99;
    let server = server(|c| {
        c.size = size;
        c.faults.drop_after = Some(300 * 1024);
        c.faults.body_fault_limit = Some(30);
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 4))
        .unwrap();
    let mut rx = handle.subscribe();
    completed(handle).await;
    assert_content(&dest, 0, size);
    assert!(
        !retries(&drain(&mut rx)).is_empty(),
        "drops should cause retries"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trickle_then_drop_does_not_retry_forever() {
    // Every response delivers one byte and then drops. The probe (a 1-byte
    // range) succeeds; each segment attempt "progresses" by a byte, which
    // must not count as progress, or this would loop for ~4 million requests.
    let server = server(|c| c.faults.drop_after = Some(1)).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let mut s = spec(&server, Destination::File(dest.clone()), 2);
    s.retry = fast_retry(2);
    let err = failed(engine().start(s).unwrap()).await;
    assert!(
        err.is_retryable(),
        "expected a transient error, got {err:?}"
    );
    assert!(server.requests() < 20, "{} requests", server.requests());
    // What did arrive is kept so the download can be resumed later.
    let partial = rdm_core::inspect_partial(&dest).unwrap().unwrap();
    assert!(partial.downloaded < 64 * 1024);
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_dropped_immediately_gives_up() {
    // Depending on how the server flushes, the client sees either a dropped
    // connection before any response or headers followed by nothing. Both
    // must end in a bounded number of attempts.
    let server = server(|c| c.faults.drop_after = Some(0)).await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = spec(&server, Destination::File(dir.path().join("out.bin")), 2);
    s.retry = fast_retry(2);
    let err = failed(engine().start(s).unwrap()).await;
    assert!(err.is_retryable(), "{err:?}");
    assert!(server.requests() < 20, "{} requests", server.requests());
}

#[tokio::test(flavor = "multi_thread")]
async fn stalled_connection_hits_read_timeout_and_recovers() {
    let server = server(|c| {
        c.faults.stall_after = Some((512 * 1024, Duration::from_secs(30)));
        // The probe's 1-byte body uses up the first slot; one real stall.
        c.faults.body_fault_limit = Some(2);
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let engine = engine_with(|c| c.read_timeout = Duration::from_millis(500));
    let started = Instant::now();
    let handle = engine
        .start(spec(&server, Destination::File(dest.clone()), 2))
        .unwrap();
    let mut rx = handle.subscribe();
    completed(handle).await;
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "a stalled connection must not hold the download for the full stall"
    );
    assert_content(&dest, 0, 4 * MIB);
    assert!(!retries(&drain(&mut rx)).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn latency_is_tolerated() {
    let server = server(|c| c.faults.latency = Some(Duration::from_millis(300))).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");
    completed(
        engine()
            .start(spec(&server, Destination::File(dest.clone()), 4))
            .unwrap(),
    )
    .await;
    assert_content(&dest, 0, 4 * MIB);
}

#[tokio::test(flavor = "multi_thread")]
async fn single_stream_restarts_from_zero_after_a_drop() {
    let size = 3 * MIB;
    let server = server(|c| {
        c.size = size;
        c.ranges = false;
        c.faults.drop_after = Some(MIB);
        c.faults.body_fault_limit = Some(2);
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let mut s = spec(&server, Destination::File(dest.clone()), 4);
    s.retry = fast_retry(5);
    completed(engine().start(s).unwrap()).await;
    assert_content(&dest, 0, size);
    // Two aborted attempts were thrown away and fetched again.
    assert!(server.bytes_served() >= size + 2 * MIB);
}

#[tokio::test(flavor = "multi_thread")]
async fn aborted_chunked_body_is_not_mistaken_for_eof() {
    // No Content-Length and no ranges: the only signal of a cut connection is
    // the missing final chunk. Treating it as EOF would "complete" a
    // truncated file.
    let size = 2 * MIB;
    let server = server(|c| {
        c.size = size;
        c.ranges = false;
        c.content_length = false;
        c.faults.drop_after = Some(512 * 1024);
        c.faults.body_fault_limit = Some(1);
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let mut s = spec(&server, Destination::File(dest.clone()), 1);
    s.retry = fast_retry(3);
    let (_, got, _) = completed(engine().start(s).unwrap()).await;
    assert_eq!(got, size);
    assert_content(&dest, 0, size);
}

// ---------------------------------------------------------------------------
// Error statuses
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn retry_after_is_honored_while_probing() {
    let server = server(|c| c.faults.error = Some(ErrorFault::first(503, 2).retry_after(1))).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let started = Instant::now();
    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 2))
        .unwrap();
    let mut rx = handle.subscribe();
    completed(handle).await;
    assert!(started.elapsed() >= Duration::from_millis(1900));
    let r = retries(&drain(&mut rx));
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(r.iter().all(|(_, d)| *d >= Duration::from_secs(1)), "{r:?}");
    assert_content(&dest, 0, 4 * MIB);
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_after_is_honored_by_segment_connections() {
    // The probe succeeds; the next four requests (the segment connections)
    // are told to come back in a second.
    let server = server(|c| {
        c.faults.error = Some(ErrorFault::first(429, 4).skip(1).retry_after(1));
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let started = Instant::now();
    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 4))
        .unwrap();
    let mut rx = handle.subscribe();
    completed(handle).await;
    assert!(started.elapsed() >= Duration::from_millis(900));
    let r = retries(&drain(&mut rx));
    assert!(!r.is_empty());
    assert!(r.iter().all(|(_, d)| *d >= Duration::from_secs(1)), "{r:?}");
    assert_content(&dest, 0, 4 * MIB);
}

#[tokio::test(flavor = "multi_thread")]
async fn persistent_503_gives_up_after_retry_limit() {
    let server = server(|c| c.faults.error = Some(ErrorFault::always(503))).await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = spec(&server, Destination::File(dir.path().join("out.bin")), 2);
    s.retry = fast_retry(2);
    let err = failed(engine().start(s).unwrap()).await;
    assert_eq!(status_of(&err), Some(503), "{err:?}");
    assert_eq!(server.requests(), 3, "one attempt plus two retries");
}

#[tokio::test(flavor = "multi_thread")]
async fn not_found_fails_without_retrying() {
    let server = server(|c| c.faults.error = Some(ErrorFault::always(404))).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");
    let err = failed(
        engine()
            .start(spec(&server, Destination::File(dest.clone()), 2))
            .unwrap(),
    )
    .await;
    assert_eq!(status_of(&err), Some(404), "{err:?}");
    assert_eq!(server.requests(), 1);
    assert!(
        !dest.exists(),
        "nothing should be created for a missing file"
    );
}

// ---------------------------------------------------------------------------
// Wrong lengths, misaligned ranges, changing resources
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn content_length_longer_than_body_is_never_reported_complete() {
    let size = MIB;
    let server = server(|c| {
        c.size = size;
        c.ranges = false;
        c.faults.content_length_override = Some(size + 1000);
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let mut s = spec(&server, Destination::File(dest.clone()), 1);
    s.retry = fast_retry(1);
    let err = failed(engine().start(s).unwrap()).await;
    assert!(err.is_retryable(), "{err:?}");
    assert!(rdm_core::sidecar_path(&dest).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn misaligned_content_range_is_rejected() {
    let server = server(|c| c.faults.range_shift = 100).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");
    let err = failed(
        engine()
            .start(spec(&server, Destination::File(dest.clone()), 4))
            .unwrap(),
    )
    .await;
    assert!(matches!(err, Error::Protocol(_)), "{err:?}");
    assert!(
        rdm_core::sidecar_path(&dest).exists(),
        "not marked complete"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn server_that_stops_honoring_ranges_falls_back_to_one_stream() {
    let size = 3 * MIB + 1;
    let server = server(|c| {
        c.size = size;
        c.faults.ranges_until_request = Some(1); // only the probe gets a 206
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 4))
        .unwrap();
    let mut rx = handle.subscribe();
    completed(handle).await;
    assert_content(&dest, 0, size);
    let w = warnings(&drain(&mut rx));
    assert!(w.iter().any(|w| w.contains("single connection")), "{w:?}");
}

/// Starts a rate-limited download whose connections are cut every 512 KiB
/// (so new requests keep arriving), lets it make progress, then applies
/// `change` to the server.
async fn change_resource_mid_download(change: impl FnOnce(&mut MockConfig)) -> (MockServer, Error) {
    let server = server(|c| {
        c.size = 8 * MIB;
        c.per_connection_rate = Some(2 * MIB);
        c.faults.drop_after = Some(512 * 1024);
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");

    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 2))
        .unwrap();
    let mut rx = handle.subscribe();
    progress_reaches(&mut rx, MIB).await;
    server.update(change);
    let err = failed(handle).await;

    // A fresh start notices the change and restarts from zero.
    server.update(|c| {
        c.per_connection_rate = None;
        c.faults = Default::default();
    });
    let handle = engine()
        .start(spec(&server, Destination::File(dest.clone()), 4))
        .unwrap();
    let mut rx = handle.subscribe();
    let (_, size, resumed_from) = completed(handle).await;
    assert_eq!(resumed_from, 0);
    let cfg = server.config();
    assert_eq!(size, cfg.size);
    assert_content(&dest, cfg.seed, cfg.size);
    let w = warnings(&drain(&mut rx));
    assert!(
        w.iter().any(|w| w.contains("restarting from zero")),
        "{w:?}"
    );
    (server, err)
}

#[tokio::test(flavor = "multi_thread")]
async fn size_change_mid_download_is_detected() {
    let (_server, err) = change_resource_mid_download(|c| c.size = 9 * MIB).await;
    assert!(matches!(err, Error::ResourceChanged(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn content_change_mid_download_is_detected() {
    let (_server, err) = change_resource_mid_download(|c| {
        c.seed = 7;
        c.etag = Some("\"v7\"".into());
    })
    .await;
    assert!(matches!(err, Error::ResourceChanged(_)), "{err:?}");
}

// ---------------------------------------------------------------------------
// Redirects, cookies, headers, auth, proxies
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn redirects_are_followed_and_their_cookies_kept() {
    let server = server(|c| {
        c.faults.redirect_cookie = Some("session=abc".into());
        c.faults.require_cookie = Some("session=abc".into());
    })
    .await;
    let url = server.redirect_url(3);
    let info = engine().probe(&request(&url)).await.unwrap();
    assert!(
        info.final_url.path().ends_with("/hop/0/file.bin"),
        "{info:?}"
    );
    assert_eq!(info.filename, "file.bin");

    let dir = tempfile::tempdir().unwrap();
    let handle = engine()
        .start(spec_for(
            request(&url),
            Destination::Directory(dir.path().into()),
            4,
        ))
        .unwrap();
    let (path, _, _) = completed(handle).await;
    assert_eq!(path, dir.path().join("file.bin"));
    assert_content(&path, 0, 4 * MIB);
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_cookies_are_sent() {
    let server = server(|c| c.faults.require_cookie = Some("auth=xyz".into())).await;
    let dir = tempfile::tempdir().unwrap();

    let err = failed(
        engine()
            .start(spec(&server, Destination::File(dir.path().join("a")), 2))
            .unwrap(),
    )
    .await;
    assert_eq!(status_of(&err), Some(403));

    let with_cookies = request(&server.url()).cookies("theme=dark; auth=xyz");
    let dest = dir.path().join("b");
    completed(
        engine()
            .start(spec_for(with_cookies, Destination::File(dest.clone()), 4))
            .unwrap(),
    )
    .await;
    assert_content(&dest, 0, 4 * MIB);

    // A raw Cookie header works too (it is moved into the cookie jar).
    let with_header = request(&server.url()).header("Cookie", "auth=xyz");
    let dest = dir.path().join("c");
    completed(
        engine()
            .start(spec_for(with_header, Destination::File(dest.clone()), 2))
            .unwrap(),
    )
    .await;
    assert_content(&dest, 0, 4 * MIB);
}

#[tokio::test(flavor = "multi_thread")]
async fn referer_and_user_agent_are_forwarded() {
    let server = server(|c| {
        c.faults.require_headers = vec![
            ("referer".into(), "https://videos.example/watch?v=1".into()),
            ("user-agent".into(), "TestBrowser/1.0".into()),
        ];
    })
    .await;
    let dir = tempfile::tempdir().unwrap();

    let err = failed(
        engine()
            .start(spec(&server, Destination::File(dir.path().join("a")), 2))
            .unwrap(),
    )
    .await;
    assert_eq!(status_of(&err), Some(403));

    let req = request(&server.url())
        .referer("https://videos.example/watch?v=1")
        .user_agent("TestBrowser/1.0");
    let dest = dir.path().join("b");
    completed(
        engine()
            .start(spec_for(req, Destination::File(dest.clone()), 4))
            .unwrap(),
    )
    .await;
    assert_content(&dest, 0, 4 * MIB);
}

#[tokio::test(flavor = "multi_thread")]
async fn basic_auth_is_sent_on_every_connection() {
    let server =
        server(|c| c.faults.require_basic_auth = Some(("alice".into(), "s3cret".into()))).await;
    let dir = tempfile::tempdir().unwrap();

    let err = failed(
        engine()
            .start(spec(&server, Destination::File(dir.path().join("a")), 2))
            .unwrap(),
    )
    .await;
    assert_eq!(status_of(&err), Some(401));

    let req = request(&server.url()).basic_auth(Credentials::new("alice", Some("s3cret".into())));
    let dest = dir.path().join("b");
    completed(
        engine()
            .start(spec_for(req, Destination::File(dest.clone()), 4))
            .unwrap(),
    )
    .await;
    assert_content(&dest, 0, 4 * MIB);
    assert!(server.peak_connections() > 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn http_proxy_with_auth_is_used() {
    let server = server(|c| {
        c.faults.require_proxy_auth = Some(("proxyuser".into(), "pp".into()));
    })
    .await;
    let proxy_url = format!("http://{}", server.addr());
    // The target host does not exist: only the proxy can serve it.
    let target = "http://files.rdm-test.invalid/file.bin";
    let dir = tempfile::tempdir().unwrap();

    let no_auth = request(target).proxy(ProxyConfig::Custom {
        url: proxy_url.clone(),
        auth: None,
    });
    let mut s = spec_for(no_auth, Destination::File(dir.path().join("a")), 2);
    s.retry = fast_retry(0);
    let err = failed(engine().start(s).unwrap()).await;
    assert_eq!(status_of(&err), Some(407), "{err:?}");

    let with_auth = request(target).proxy(ProxyConfig::Custom {
        url: proxy_url,
        auth: Some(Credentials::new("proxyuser", Some("pp".into()))),
    });
    let dest = dir.path().join("b");
    let before = server.requests();
    completed(
        engine()
            .start(spec_for(with_auth, Destination::File(dest.clone()), 4))
            .unwrap(),
    )
    .await;
    assert_content(&dest, 0, 4 * MIB);
    assert_eq!(server.proxied_requests(), server.requests());
    assert!(server.requests() > before + 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn events_report_retry_reasons() {
    let server = server(|c| {
        c.faults.error = Some(ErrorFault::first(503, 1));
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = spec(&server, Destination::File(dir.path().join("out.bin")), 1);
    s.retry = fast_retry(3);
    let handle = engine().start(s).unwrap();
    let mut rx = handle.subscribe();
    completed(handle).await;
    let reason = drain(&mut rx).into_iter().find_map(|e| match e {
        Event::Retrying { reason, .. } => Some(reason),
        _ => None,
    });
    assert_eq!(reason.as_deref(), Some("server returned HTTP 503"));
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_cookies_follow_redirects_to_other_paths() {
    // Browsers hand over `name=value` pairs without their `Path`; they must
    // still reach a redirect target outside the original URL's directory.
    let server = server(|c| c.faults.require_cookie = Some("auth=xyz".into())).await;
    let dir = tempfile::tempdir().unwrap();
    let with_cookies = request(&server.redirect_url(2)).cookies("auth=xyz");
    let dest = dir.path().join("r");
    completed(
        engine()
            .start(spec_for(with_cookies, Destination::File(dest.clone()), 4))
            .unwrap(),
    )
    .await;
    assert_content(&dest, 0, 4 * MIB);
}
