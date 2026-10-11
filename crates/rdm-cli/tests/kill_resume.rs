//! Acceptance: kill the `rdm` process mid-download (SIGKILL on Unix,
//! TerminateProcess on Windows: no chance to clean up), run it again, and
//! check the result is byte-identical without refetching completed ranges.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use rdm_tests::{MockConfig, MockServer, verify_file};
use tokio::process::Command;

const MIB: u64 = 1 << 20;

fn rdm(url: &str, dest: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rdm"));
    cmd.args(["get", url, "-o"])
        .arg(dest)
        .args(["-c", "4", "--min-split", "256K", "--no-proxy", "-q"])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    cmd
}

fn sidecar_ranges(dest: &Path) -> Vec<(u64, u64)> {
    let bytes = std::fs::read(rdm_core::sidecar_path(dest)).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    serde_json::from_value(json["completed"].clone()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn killed_process_resumes_to_identical_file() {
    let size = 48 * MIB + 4321;
    let mut cfg = MockConfig::new(size);
    cfg.per_connection_rate = Some(2 * MIB); // ~6 s with 4 connections
    let server = MockServer::start(cfg).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("big.bin");

    let mut child = rdm(&server.url(), &dest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Wait until a checkpoint has recorded real progress, then kill hard.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(p)) = rdm_core::inspect_partial(&dest)
            && p.downloaded >= 4 * MIB
        {
            break;
        }
        assert!(Instant::now() < deadline, "no checkpoint within 30s");
        assert!(child.try_wait().unwrap().is_none(), "rdm exited early");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    child.kill().await.unwrap();

    let partial = rdm_core::inspect_partial(&dest).unwrap().unwrap();
    assert!(partial.downloaded > 0 && partial.downloaded < size);
    let completed_before = sidecar_ranges(&dest);
    let requests_before = server.served_ranges().len();

    server.update(|c| c.per_connection_rate = None);
    let out = rdm(&server.url(), &dest).output().await.unwrap();
    assert!(
        out.status.success(),
        "second run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        dest.display().to_string()
    );

    verify_file(&dest, 0, size).unwrap().unwrap();
    assert!(!rdm_core::sidecar_path(&dest).exists());

    for &(first, last) in &server.served_ranges()[requests_before..] {
        if (first, last) == (0, 0) {
            continue; // the probe
        }
        for &(s, e) in &completed_before {
            assert!(
                last < s || first >= e,
                "refetched {first}-{last}, which was already complete ({s}..{e})"
            );
        }
    }
}
