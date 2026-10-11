//! Helpers shared by the integration tests.

// Not every test file uses every helper, and clippy.toml's test exemptions
// do not cover helpers outside `#[test]` functions.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rdm_core::{
    Destination, DownloadHandle, DownloadSpec, Engine, EngineConfig, Error, Event, Outcome,
    ProxyConfig, Request, RetryPolicy, State,
};
use rdm_tests::{MockServer, verify_file};
use tokio::sync::broadcast;

pub const MIB: u64 = 1 << 20;

/// Upper bound for any single download in the tests, so a regression
/// shows up as a failure instead of a hung CI job.
const DEADLINE: Duration = Duration::from_secs(60);

pub fn engine() -> Engine {
    engine_with(|_| {})
}

/// An engine with frequent progress events and checkpoints.
pub fn engine_with(f: impl FnOnce(&mut EngineConfig)) -> Engine {
    let mut config = EngineConfig {
        progress_interval: Duration::from_millis(50),
        checkpoint_interval: Duration::from_millis(200),
        ..EngineConfig::default()
    };
    f(&mut config);
    Engine::new(config)
}

pub fn request(url: &str) -> Request {
    Request::new(url).unwrap().proxy(ProxyConfig::Direct)
}

pub fn spec_for(request: Request, dest: Destination, connections: usize) -> DownloadSpec {
    let mut spec = DownloadSpec::new(request, dest);
    spec.connections = NonZeroUsize::new(connections).unwrap();
    spec.min_split = 256 * 1024;
    spec
}

pub fn spec(server: &MockServer, dest: Destination, connections: usize) -> DownloadSpec {
    spec_for(request(&server.url()), dest, connections)
}

/// Retries quickly so failure tests finish fast.
pub fn fast_retry(max_retries: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries,
        base_delay: Duration::from_millis(20),
        max_delay: Duration::from_millis(200),
    }
}

pub async fn finish(handle: DownloadHandle) -> Result<Outcome, Error> {
    tokio::time::timeout(DEADLINE, handle.wait())
        .await
        .expect("download did not finish within the deadline")
}

pub async fn completed(handle: DownloadHandle) -> (PathBuf, u64, u64) {
    match finish(handle).await {
        Ok(Outcome::Completed {
            path,
            size,
            resumed_from,
            ..
        }) => (path, size, resumed_from),
        other => panic!("expected completion, got {other:?}"),
    }
}

pub async fn failed(handle: DownloadHandle) -> Error {
    match finish(handle).await {
        Err(e) => e,
        Ok(o) => panic!("expected failure, got {o:?}"),
    }
}

/// Waits until the download has at least `bytes` on disk.
pub async fn progress_reaches(rx: &mut broadcast::Receiver<Event>, bytes: u64) {
    let wait = async {
        loop {
            match rx.recv().await {
                Ok(Event::Progress(p)) if p.downloaded >= bytes => return,
                Ok(Event::State(State::Failed(e))) => panic!("download failed: {e}"),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("event channel closed: {e}"),
            }
        }
    };
    tokio::time::timeout(DEADLINE, wait)
        .await
        .expect("progress not reached within the deadline");
}

/// Everything still buffered in the receiver.
pub fn drain(rx: &mut broadcast::Receiver<Event>) -> Vec<Event> {
    let mut events = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(e) => events.push(e),
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => return events,
        }
    }
}

/// `(attempt, delay)` of every `Retrying` event.
pub fn retries(events: &[Event]) -> Vec<(u32, Duration)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Retrying { attempt, delay, .. } => Some((*attempt, *delay)),
            _ => None,
        })
        .collect()
}

pub fn warnings(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Warning(w) => Some(w.clone()),
            _ => None,
        })
        .collect()
}

/// The file matches the mock content and the sidecar is gone.
pub fn assert_content(path: &Path, seed: u64, size: u64) {
    if let Err(msg) = verify_file(path, seed, size).unwrap() {
        panic!("{} is wrong: {msg}", path.display());
    }
    assert!(
        !rdm_core::sidecar_path(path).exists(),
        "sidecar should be removed on completion"
    );
}

/// Completed ranges recorded in a download's `.rdm` sidecar.
pub fn sidecar_ranges(dest: &Path) -> Vec<(u64, u64)> {
    let bytes = std::fs::read(rdm_core::sidecar_path(dest)).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    serde_json::from_value(json["completed"].clone()).unwrap()
}
