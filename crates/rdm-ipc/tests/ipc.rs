//! Server and client over a real local socket.

#![allow(clippy::unwrap_used, clippy::panic)]

use rdm_ipc::{AddItem, AddMode, Client, Endpoint, Error, Request, Response, Server};

/// A unique endpoint for this test (socket path on Unix, pipe on Windows).
fn endpoint(dir: &tempfile::TempDir, name: &str) -> Endpoint {
    if cfg!(windows) {
        Endpoint::custom(format!("rdm-test-{}-{name}", std::process::id()))
    } else {
        Endpoint::custom(dir.path().join("ipc").join(name).to_string_lossy())
    }
}

async fn echo_server(endpoint: Endpoint) -> Server {
    Server::start(endpoint, |req| async move {
        match req {
            Request::Ping => Response::Pong {
                version: "test".into(),
                protocol: rdm_ipc::PROTOCOL_VERSION,
            },
            Request::Add { items, mode } => match mode {
                AddMode::Ask => Response::Shown,
                _ => Response::Added {
                    ids: (1..=items.len() as i64).collect(),
                },
            },
            _ => Response::Ok,
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_and_responses_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let ep = endpoint(&dir, "a.sock");
    let _server = echo_server(ep.clone()).await;

    let mut client = Client::connect(&ep).await.unwrap();
    assert!(matches!(
        client.call(&Request::Ping).await.unwrap(),
        Response::Pong { .. }
    ));
    // Several requests on one connection.
    let add = Request::Add {
        items: vec![
            AddItem {
                url: "https://x/a".into(),
                ..Default::default()
            };
            3
        ],
        mode: AddMode::Queue,
    };
    assert_eq!(
        client.call(&add).await.unwrap(),
        Response::Added { ids: vec![1, 2, 3] }
    );
    // And concurrent clients.
    let calls = (0..8).map(|_| {
        let ep = ep.clone();
        tokio::spawn(async move {
            Client::connect(&ep)
                .await
                .unwrap()
                .call(&Request::Ping)
                .await
        })
    });
    for c in calls {
        assert!(matches!(c.await.unwrap().unwrap(), Response::Pong { .. }));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn second_server_is_refused_and_stale_socket_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let ep = endpoint(&dir, "b.sock");
    let first = echo_server(ep.clone()).await;
    let second = Server::start(ep.clone(), |_| async { Response::Shown }).await;
    assert!(
        matches!(second, Err(Error::AlreadyRunning(_))),
        "{second:?}"
    );
    drop(first);

    #[cfg(unix)]
    {
        // A crash leaves the socket file behind; nothing answers on it.
        let path = std::path::PathBuf::from(ep.to_string());
        std::fs::write(&path, b"stale").unwrap();
    }
    let _server = echo_server(ep.clone()).await;
    let mut client = Client::connect(&ep).await.unwrap();
    assert!(matches!(
        client.call(&Request::Ping).await.unwrap(),
        Response::Pong { .. }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn connecting_without_a_server_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Client::connect(&endpoint(&dir, "none.sock")).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn garbage_gets_an_error_reply() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let ep = endpoint(&dir, "c.sock");
    let _server = echo_server(ep.clone()).await;

    // Speak raw bytes: a valid length prefix followed by non-JSON.
    let stream = interprocess_connect(&ep).await;
    let (mut rx, mut tx) = (&stream, &stream);
    tx.write_all(&5u32.to_ne_bytes()).await.unwrap();
    tx.write_all(b"nope!").await.unwrap();
    let reply: Option<Response> = rdm_ipc::read_frame(&mut rx).await.unwrap();
    assert!(matches!(reply, Some(Response::Error { .. })), "{reply:?}");
    let mut rest = Vec::new();
    let _ = rx.read_to_end(&mut rest).await;
}

async fn interprocess_connect(ep: &Endpoint) -> interprocess::local_socket::tokio::Stream {
    use interprocess::local_socket::tokio::prelude::*;
    let s = ep.to_string();
    #[cfg(unix)]
    let name = s
        .as_str()
        .to_fs_name::<interprocess::local_socket::GenericFilePath>()
        .unwrap();
    #[cfg(windows)]
    let name = s
        .trim_start_matches(r"\\.\pipe\")
        .to_ns_name::<interprocess::local_socket::GenericNamespaced>()
        .unwrap();
    interprocess::local_socket::tokio::Stream::connect(name)
        .await
        .unwrap()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn socket_is_private_to_the_user() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let ep = endpoint(&dir, "d.sock");
    let _server = echo_server(ep.clone()).await;
    let path = std::path::PathBuf::from(ep.to_string());
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    if cfg!(target_os = "linux") {
        assert_eq!(mode(&path), 0o600);
    }
}
