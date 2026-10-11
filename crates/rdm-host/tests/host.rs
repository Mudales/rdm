//! The host binary end to end (browser framing on stdio, IPC to a fake app)
//! and the installer against a temporary home directory.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use rdm_host::install::{Browser, Installer};
use rdm_ipc::{AddItem, AddMode, Endpoint, Request, Response, Server};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

fn endpoint_for(dir: &tempfile::TempDir) -> String {
    if cfg!(windows) {
        format!(
            "rdm-host-test-{}-{}",
            std::process::id(),
            dir.path().display().to_string().len()
        )
    } else {
        dir.path()
            .join("ipc")
            .join("rdm.sock")
            .to_string_lossy()
            .into_owned()
    }
}

/// Runs `rdm-host`, sends `requests` as one browser would, returns replies.
async fn run_host(endpoint: &str, app: &str, requests: &[Request]) -> Vec<Response> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rdm-host"))
        .arg("chrome-extension://dacdblamhhaedfahmbcllbpgjejdaddi/")
        .env("RDM_IPC_SOCKET", endpoint)
        .env("RDM_APP", app)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for r in requests {
        let json = serde_json::to_vec(r).unwrap();
        stdin
            .write_all(&(json.len() as u32).to_ne_bytes())
            .await
            .unwrap();
        stdin.write_all(&json).await.unwrap();
    }
    drop(stdin); // like sendNativeMessage: close after sending

    let mut out = Vec::new();
    let mut stdout = child.stdout.take().unwrap();
    tokio::time::timeout(Duration::from_secs(30), stdout.read_to_end(&mut out))
        .await
        .expect("host did not finish")
        .unwrap();
    assert!(child.wait().await.unwrap().success());

    let mut replies = Vec::new();
    let mut rest = out.as_slice();
    while !rest.is_empty() {
        let len = u32::from_ne_bytes(rest[..4].try_into().unwrap()) as usize;
        replies.push(serde_json::from_slice(&rest[4..4 + len]).unwrap());
        rest = &rest[4 + len..];
    }
    replies
}

#[tokio::test(flavor = "multi_thread")]
async fn relays_requests_to_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let ep = endpoint_for(&dir);
    let _app = Server::start(Endpoint::custom(ep.clone()), |req| async move {
        match req {
            Request::Ping => Response::Pong {
                version: "9.9.9".into(),
                protocol: 1,
            },
            Request::Add { items, .. } => Response::Added {
                ids: items.iter().map(|i| i.url.len() as i64).collect(),
            },
        }
    })
    .await
    .unwrap();

    let replies = run_host(
        &ep,
        "/nonexistent/rdm",
        &[
            Request::Ping,
            Request::Add {
                items: vec![AddItem {
                    url: "https://example.com/file.zip".into(),
                    cookies: Some("a=1".into()),
                    ..AddItem::default()
                }],
                mode: AddMode::Queue,
            },
        ],
    )
    .await;
    assert_eq!(
        replies,
        vec![
            Response::Pong {
                version: "9.9.9".into(),
                protocol: 1
            },
            Response::Added { ids: vec![28] },
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ping_without_app_reports_not_running_and_never_launches() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("launched");
    // If the host tried to launch this "app", the spawn would fail loudly
    // and the reply would be an error instead of NotRunning.
    let replies = run_host(
        &endpoint_for(&dir),
        &marker.to_string_lossy(),
        &[Request::Ping],
    )
    .await;
    assert_eq!(replies, vec![Response::NotRunning]);
}

#[tokio::test(flavor = "multi_thread")]
async fn add_without_installed_app_explains_the_problem() {
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let replies = run_host(
        &endpoint_for(&dir),
        "/nonexistent/rdm",
        &[Request::Add {
            items: vec![AddItem {
                url: "https://example.com/a".into(),
                ..AddItem::default()
            }],
            mode: AddMode::Ask,
        }],
    )
    .await;
    match &replies[..] {
        [Response::Error { message }] => {
            assert!(message.contains("could not start rdm"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "should fail fast"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_input_gets_an_error_reply() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rdm-host"))
        .env("RDM_IPC_SOCKET", endpoint_for(&dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&4u32.to_ne_bytes()).await.unwrap();
    stdin.write_all(b"nope").await.unwrap();
    drop(stdin);
    let mut out = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut out)
        .await
        .unwrap();
    let reply: Response = serde_json::from_slice(&out[4..]).unwrap();
    assert!(matches!(reply, Response::Error { .. }), "{reply:?}");
}

#[cfg(unix)]
#[test]
fn installer_writes_and_removes_manifests() {
    let home = tempfile::tempdir().unwrap();
    let host = PathBuf::from("/opt/rdm/rdm-host");
    let mut installer = Installer::with_home(home.path().into(), host.clone());
    installer.allow_chrome_extension("abcdefghijklmnopabcdefghijklmnop");

    // Nothing detected in an empty home.
    assert!(
        installer
            .status()
            .iter()
            .all(|s| !s.detected && !s.installed)
    );

    for b in Browser::ALL {
        let path = installer.install(b).unwrap();
        assert!(path.starts_with(home.path()));
        let m: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(m["name"], "dev.rdm.host");
        assert_eq!(m["type"], "stdio");
        assert_eq!(m["path"], "/opt/rdm/rdm-host");
        if b == Browser::Firefox {
            assert_eq!(m["allowed_extensions"], serde_json::json!(["rdm@rdm.dev"]));
            assert!(m.get("allowed_origins").is_none());
        } else {
            assert_eq!(
                m["allowed_origins"],
                serde_json::json!([
                    "chrome-extension://dacdblamhhaedfahmbcllbpgjejdaddi/",
                    "chrome-extension://abcdefghijklmnopabcdefghijklmnop/"
                ])
            );
        }
    }
    let status = installer.status();
    assert!(
        status
            .iter()
            .all(|s| s.installed && s.current && s.detected)
    );

    let elsewhere = Installer::with_home(home.path().into(), PathBuf::from("/other/rdm-host"));
    assert!(elsewhere.status().iter().all(|s| s.installed && !s.current));

    let chrome = installer.manifest_path(Browser::Chrome);
    if cfg!(target_os = "macos") {
        assert!(chrome.ends_with(
            "Library/Application Support/Google/Chrome/NativeMessagingHosts/dev.rdm.host.json"
        ));
    } else {
        assert!(chrome.ends_with(".config/google-chrome/NativeMessagingHosts/dev.rdm.host.json"));
    }

    for b in Browser::ALL {
        assert!(installer.uninstall(b).unwrap());
        assert!(!installer.uninstall(b).unwrap());
    }
    assert!(installer.status().iter().all(|s| !s.installed));
}

#[cfg(windows)]
#[test]
fn installer_writes_and_removes_manifests_and_registry_keys() {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let home = tempfile::tempdir().unwrap();
    let base = format!(r"Software\rdm-test-{}", std::process::id());
    let mut installer =
        Installer::with_home(home.path().into(), PathBuf::from(r"C:\rdm\rdm-host.exe"));
    installer.set_registry_base(base.clone());

    for b in Browser::ALL {
        let path = installer.install(b).unwrap();
        assert!(path.is_file());
    }
    for b in Browser::ALL {
        let vendor = match b {
            Browser::Chrome => r"Google\Chrome",
            Browser::Chromium => "Chromium",
            Browser::Edge => r"Microsoft\Edge",
            Browser::Brave => r"BraveSoftware\Brave-Browser",
            Browser::Firefox => "Mozilla",
        };
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(format!(
                r"{base}\{vendor}\NativeMessagingHosts\dev.rdm.host"
            ))
            .unwrap();
        let value: String = key.get_value("").unwrap();
        assert_eq!(PathBuf::from(value), installer.manifest_path(b));
    }
    for b in Browser::ALL {
        installer.uninstall(b).unwrap();
    }
    assert!(
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(format!(r"{base}\Mozilla\NativeMessagingHosts\dev.rdm.host"))
            .is_err()
    );
    let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&base);
}
