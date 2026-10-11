//! Answers requests relayed by `rdm-host` from the browser extension.

use std::sync::Arc;

use rdm_ipc::{AddItem, AddMode, Endpoint, PROTOCOL_VERSION, Request, Response, Server};
use rdm_manager::{Manager, NewDownload, StartMode, StreamChoice, error_chain};
use tauri::AppHandle;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::request_add;

/// Batch adds probe each URL; this many at once.
const PARALLEL_ADDS: usize = 6;

/// Keeps the server alive for the app's lifetime.
pub struct IpcServer(#[allow(dead_code)] pub Server);

pub async fn start(app: AppHandle, manager: Arc<Manager>) -> rdm_ipc::Result<Server> {
    Server::start(Endpoint::for_current_user(), move |request| {
        let app = app.clone();
        let manager = manager.clone();
        async move { handle(&app, &manager, request).await }
    })
    .await
}

async fn handle(app: &AppHandle, manager: &Arc<Manager>, request: Request) -> Response {
    match request {
        Request::Ping => Response::Pong {
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
        },
        Request::Add { items, .. } if items.is_empty() => Response::error("nothing to add"),
        // One item, user confirms: open the prefilled Add dialog.
        Request::Add {
            mut items,
            mode: AddMode::Ask,
        } if items.len() == 1 => {
            request_add(app, items.remove(0));
            Response::Shown
        }
        Request::Add { items, mode } => {
            let start = match mode {
                AddMode::Now => StartMode::Now,
                // Several items without confirmation go into the queue.
                AddMode::Queue | AddMode::Ask => StartMode::Queue,
            };
            add_all(manager, items, start).await
        }
    }
}

async fn add_all(manager: &Arc<Manager>, items: Vec<AddItem>, start: StartMode) -> Response {
    let limit = Arc::new(Semaphore::new(PARALLEL_ADDS));
    let mut tasks = JoinSet::new();
    for (index, item) in items.into_iter().enumerate() {
        let manager = manager.clone();
        let limit = limit.clone();
        tasks.spawn(async move {
            let _permit = limit.acquire_owned().await;
            let url = item.url.clone();
            let result = manager.add(to_new_download(item, start)).await;
            (index, url, result)
        });
    }
    let mut added = Vec::new();
    let mut failures = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((index, _, Ok(record))) => added.push((index, record.id)),
            Ok((_, url, Err(e))) => failures.push(format!("{url}: {}", error_chain(&e))),
            Err(e) => failures.push(e.to_string()),
        }
    }
    added.sort_unstable();
    if added.is_empty() {
        return Response::error(failures.into_iter().next().unwrap_or_default());
    }
    if !failures.is_empty() {
        manager.notice(format!(
            "{} of {} links could not be added: {}",
            failures.len(),
            failures.len() + added.len(),
            failures.join("; ")
        ));
    }
    Response::Added {
        ids: added.into_iter().map(|(_, id)| id).collect(),
    }
}

fn to_new_download(item: AddItem, start: StartMode) -> NewDownload {
    let stream = matches!(item.kind.as_deref(), Some("hls" | "dash")).then(StreamChoice::default);
    NewDownload {
        stream,
        title: item.title,
        url: item.url,
        file_name: item.file_name.filter(|n| !n.trim().is_empty()),
        referer: item.referer.or(item.page_url),
        user_agent: item.user_agent,
        cookies: item.cookies,
        start,
        ..NewDownload::default()
    }
}
