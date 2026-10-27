//! The task that runs one download: probe, prepare the output file, run the
//! segment workers (or a single stream), checkpoint, and finish.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use reqwest::{Response, StatusCode, header};
use tokio::sync::{Semaphore, broadcast, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet, spawn_blocking};
use tokio::time::{Instant, MissedTickBehavior};

use crate::config::{Destination, DownloadSpec, EngineConfig};
use crate::error::{Error, Result};
use crate::events::{Event, Outcome, Progress, State};
use crate::http::{self, HttpClient};
use crate::probe::{self, ProbeInfo};
use crate::ranges::RangeSet;
use crate::segments::{SegmentId, SegmentInfo, SegmentMap};
use crate::sidecar::{self, Sidecar};
use crate::throttle::TokenBucket;
use crate::writer::OutputFile;

/// Buffered bytes per worker before a positioned write.
const FLUSH_BYTES: usize = 512 * 1024;

/// A failed attempt that still transferred at least this much resets the
/// connection's failure count. Any smaller threshold would let a server
/// that drops every connection after a few bytes keep a download looping
/// for millions of requests instead of failing.
const RETRY_RESET_PROGRESS: u64 = 64 * 1024;

/// Requested lifecycle, driven by the [`DownloadHandle`](crate::DownloadHandle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Control {
    Run,
    Pause,
    Stop,
    Cancel,
}

/// Everything a download task needs from the engine and its handle.
pub(crate) struct Ctx {
    pub config: EngineConfig,
    pub spec: DownloadSpec,
    pub control: Arc<watch::Sender<Control>>,
    pub events: broadcast::Sender<Event>,
    pub limit: Arc<TokenBucket>,
    pub global: Arc<TokenBucket>,
    pub host: Arc<Semaphore>,
}

impl Ctx {
    fn emit(&self, event: Event) {
        // No subscribers is fine.
        let _ = self.events.send(event);
    }

    fn control(&self) -> Control {
        *self.control.borrow()
    }
}

/// Entry point of the spawned task.
pub(crate) async fn run(ctx: Arc<Ctx>) -> Result<Outcome> {
    let result = run_inner(&ctx).await;
    let state = match &result {
        Ok(Outcome::Completed { .. }) => State::Completed,
        Ok(Outcome::Stopped { .. }) => State::Stopped,
        Ok(Outcome::Cancelled) => State::Cancelled,
        Err(e) => State::Failed(error_chain(e)),
    };
    ctx.emit(Event::State(state));
    result
}

async fn run_inner(ctx: &Arc<Ctx>) -> Result<Outcome> {
    let started = Instant::now();
    ctx.emit(Event::State(State::Probing));
    let client = HttpClient::new(&ctx.config, &ctx.spec.request, ctx.spec.connection_count())?;

    let mut rx = ctx.control.subscribe();
    let probed = tokio::select! {
        biased;
        _ = until(&mut rx, |c| matches!(c, Control::Stop | Control::Cancel)) => None,
        r = probe_with_retry(ctx, &client) => Some(r?),
    };
    let Some((info, mut body)) = probed else {
        return Ok(match ctx.control() {
            Control::Cancel => Outcome::Cancelled,
            _ => Outcome::Stopped {
                path: None,
                downloaded: 0,
            },
        });
    };

    if let Some(encoding) = &info.stored_encoding {
        let msg = format!(
            "the server holds this as a {encoding}-encoded entity, whose byte ranges do not \
             address the file; downloading over a single connection"
        );
        tracing::warn!("{msg}");
        ctx.emit(Event::Warning(msg));
    }

    let prepared = {
        let (spec, info) = (ctx.spec.clone(), info.clone());
        spawn_blocking(move || prepare(&spec, &info))
            .await
            .map_err(Error::task)??
    };
    for w in &prepared.warnings {
        tracing::warn!("{w}");
        ctx.emit(Event::Warning(w.clone()));
    }
    let resumed_from = prepared.resumed_from;
    let transfer = Arc::new(Transfer::new(ctx.clone(), client, info.clone(), prepared));
    ctx.emit(Event::Started {
        path: transfer.path.clone(),
        info: Box::new(info),
        resumed_from,
        segmented: transfer.segmented(),
    });

    loop {
        match ctx.control() {
            Control::Run => {}
            Control::Pause => {
                transfer.checkpoint().await?;
                ctx.emit(Event::State(State::Paused));
                let _ = rx.wait_for(|c| *c != Control::Pause).await;
                continue;
            }
            Control::Stop => {
                transfer.checkpoint().await?;
                return Ok(Outcome::Stopped {
                    path: Some(transfer.path.clone()),
                    downloaded: transfer.downloaded(),
                });
            }
            Control::Cancel => {
                transfer.discard().await?;
                return Ok(Outcome::Cancelled);
            }
        }

        ctx.emit(Event::State(State::Downloading));
        let result = if transfer.segmented() {
            run_segmented(&transfer).await
        } else {
            run_single(&transfer, body.take()).await
        };
        match result {
            Ok(true) => {
                let size = transfer.finish().await?;
                return Ok(Outcome::Completed {
                    path: transfer.path.clone(),
                    size,
                    resumed_from,
                    elapsed: started.elapsed(),
                });
            }
            Ok(false) => {} // interrupted by pause/stop/cancel: handled at the loop top
            Err(Error::RangeNotHonored) if transfer.segmented() => {
                let msg =
                    "server stopped honoring byte ranges; restarting with a single connection";
                tracing::warn!("{msg}");
                ctx.emit(Event::Warning(msg.into()));
                transfer.fall_back_to_single().await?;
            }
            Err(e) => {
                if let Err(ce) = transfer.checkpoint().await {
                    tracing::warn!("final checkpoint failed: {}", error_chain(&ce));
                }
                return Err(e);
            }
        }
    }
}

async fn probe_with_retry(ctx: &Ctx, client: &HttpClient) -> Result<(ProbeInfo, Option<Response>)> {
    let mut attempt = 0;
    loop {
        let result = {
            let _permit = ctx.host.acquire().await.map_err(|_| closed())?;
            probe::probe(client).await
        };
        match result {
            Ok(r) => return Ok(r),
            Err(e) if e.is_retryable() && attempt < ctx.spec.retry.max_retries => {
                attempt += 1;
                let delay = ctx.spec.retry.delay(attempt, e.retry_after());
                ctx.emit(Event::Retrying {
                    attempt,
                    delay,
                    reason: error_chain(&e),
                });
                tokio::time::sleep(delay).await;
            }
            Err(e) => return Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Preparing the destination
// ---------------------------------------------------------------------------

struct Prepared {
    path: PathBuf,
    file: OutputFile,
    sidecar: Sidecar,
    resumed_from: u64,
    warnings: Vec<String>,
}

enum Existing {
    Missing,
    Found(Sidecar),
    Corrupt(String),
}

fn load_existing(path: &Path) -> Existing {
    match sidecar::load(&sidecar::sidecar_path(path)) {
        Ok(Some(s)) => Existing::Found(s),
        Ok(None) => Existing::Missing,
        Err(e) => Existing::Corrupt(error_chain(&e)),
    }
}

/// Picks the output path and loads any resume state for it.
fn resolve_destination(spec: &DownloadSpec, info: &ProbeInfo) -> Result<(PathBuf, Existing)> {
    let url = spec.request.url.as_str();
    match &spec.destination {
        Destination::File(path) => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(Error::io(parent))?;
            }
            let existing = load_existing(path);
            if matches!(existing, Existing::Missing) && path.exists() && !spec.overwrite {
                return Err(Error::DestinationExists(path.clone()));
            }
            Ok((path.clone(), existing))
        }
        Destination::Directory(dir) => {
            std::fs::create_dir_all(dir).map_err(Error::io(dir))?;
            let name = Path::new(&info.filename);
            let stem = name.file_stem().map(|s| s.to_string_lossy().into_owned());
            let ext = name.extension().map(|s| s.to_string_lossy().into_owned());
            for i in 0..1000 {
                let file_name = match (i, &stem, &ext) {
                    (0, _, _) | (_, None, _) => info.filename.clone(),
                    (i, Some(stem), Some(ext)) => format!("{stem} ({i}).{ext}"),
                    (i, Some(stem), None) => format!("{stem} ({i})"),
                };
                let candidate = dir.join(file_name);
                match load_existing(&candidate) {
                    // Our own earlier attempt at this URL: resume it.
                    Existing::Found(s) if s.url == url => {
                        return Ok((candidate, Existing::Found(s)));
                    }
                    // Another download's partial file.
                    Existing::Found(_) | Existing::Corrupt(_) => continue,
                    Existing::Missing if !candidate.exists() => {
                        return Ok((candidate, Existing::Missing));
                    }
                    Existing::Missing => continue,
                }
            }
            Err(Error::DestinationExists(dir.join(&info.filename)))
        }
    }
}

fn prepare(spec: &DownloadSpec, info: &ProbeInfo) -> Result<Prepared> {
    let url = spec.request.url.as_str();
    let segmented = is_segmentable(info);
    let (path, existing) = resolve_destination(spec, info)?;
    let mut warnings = Vec::new();

    let mut restart = |reason: String| {
        warnings.push(format!("cannot resume: {reason}; restarting from zero"));
        RangeSet::new()
    };
    let completed = match existing {
        Existing::Missing => RangeSet::new(),
        Existing::Corrupt(e) => restart(format!("resume metadata is unreadable ({e})")),
        Existing::Found(_) if !path.exists() => restart("the partial file is missing".into()),
        Existing::Found(sc) => {
            let unverifiable = sc.url != url && info.etag.is_none() && info.last_modified.is_none();
            match sc.validate(info) {
                Err(reason) => restart(reason),
                Ok(()) if unverifiable => restart(
                    "the URL changed and the server sends no ETag or Last-Modified to prove \
                     it is the same file"
                        .into(),
                ),
                Ok(()) => sc.completed,
            }
        }
    };

    let resuming = !completed.is_empty();
    let file = OutputFile::open(&path, !resuming)?;
    if let (true, Some(size)) = (segmented, info.size)
        && file.len()? != size
    {
        file.set_len(size)?;
    }

    let mut sc = Sidecar::new(url, info);
    sc.resumable = segmented;
    sc.completed = completed;
    sidecar::store(&sidecar::sidecar_path(&path), &sc)?;

    Ok(Prepared {
        path,
        file,
        resumed_from: sc.completed.covered(),
        sidecar: sc,
        warnings,
    })
}

fn is_segmentable(info: &ProbeInfo) -> bool {
    info.resumable && info.size.is_some_and(|s| s > 0)
}

// ---------------------------------------------------------------------------
// Shared transfer state
// ---------------------------------------------------------------------------

struct Transfer {
    ctx: Arc<Ctx>,
    client: HttpClient,
    info: ProbeInfo,
    path: PathBuf,
    sidecar_path: PathBuf,
    file: Arc<OutputFile>,
    map: Mutex<SegmentMap>,
    /// Template for checkpoints; `completed` is filled in from `map`.
    sidecar: Mutex<Sidecar>,
    segmented: AtomicBool,
    downloaded: AtomicU64,
    active: AtomicUsize,
    /// Serializes sidecar writes and deletes on the blocking pool.
    disk: Arc<Mutex<()>>,
}

impl Transfer {
    fn new(ctx: Arc<Ctx>, client: HttpClient, info: ProbeInfo, p: Prepared) -> Self {
        let segmented = p.sidecar.resumable;
        let total = if segmented {
            info.size.unwrap_or_default()
        } else {
            0
        };
        let map = SegmentMap::new(total, ctx.spec.min_split, p.sidecar.completed.clone());
        Self {
            sidecar_path: sidecar::sidecar_path(&p.path),
            path: p.path,
            file: Arc::new(p.file),
            downloaded: AtomicU64::new(p.resumed_from),
            map: Mutex::new(map),
            sidecar: Mutex::new(p.sidecar),
            segmented: AtomicBool::new(segmented),
            active: AtomicUsize::new(0),
            disk: Arc::new(Mutex::new(())),
            ctx,
            client,
            info,
        }
    }

    fn map(&self) -> MutexGuard<'_, SegmentMap> {
        lock(&self.map)
    }

    fn segmented(&self) -> bool {
        self.segmented.load(Ordering::Relaxed)
    }

    fn downloaded(&self) -> u64 {
        self.downloaded.load(Ordering::Relaxed)
    }

    async fn throttle(&self, bytes: u64) {
        self.ctx.limit.acquire(bytes).await;
        self.ctx.global.acquire(bytes).await;
    }

    /// Writes `buf` at `offset`, then records it as complete. Leaves `buf`
    /// empty (keeping its allocation).
    async fn write(&self, offset: u64, buf: &mut Vec<u8>) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(buf);
        let file = self.file.clone();
        let (result, mut data) = spawn_blocking(move || {
            let r = file.write_all_at(&data, offset);
            (r, data)
        })
        .await
        .map_err(Error::task)?;
        result?;
        let len = data.len() as u64;
        self.map().commit(offset, len);
        self.downloaded.fetch_add(len, Ordering::Relaxed);
        data.clear();
        *buf = data;
        Ok(())
    }

    /// Makes progress durable: file data first, then the sidecar.
    async fn checkpoint(&self) -> Result<()> {
        let mut sc = lock(&self.sidecar).clone();
        if sc.resumable {
            sc.completed = self.map().completed().clone();
        }
        let (file, path, disk) = (
            self.file.clone(),
            self.sidecar_path.clone(),
            self.disk.clone(),
        );
        spawn_blocking(move || {
            let _guard = lock(&disk);
            file.sync_data()?;
            sidecar::store(&path, &sc)
        })
        .await
        .map_err(Error::task)?
    }

    /// Final flush; removes the sidecar. Returns the file size.
    async fn finish(&self) -> Result<u64> {
        let len = if self.segmented() {
            self.info.size.unwrap_or_else(|| self.downloaded())
        } else {
            self.downloaded()
        };
        let (file, path, disk) = (
            self.file.clone(),
            self.sidecar_path.clone(),
            self.disk.clone(),
        );
        spawn_blocking(move || {
            let _guard = lock(&disk);
            if file.len()? != len {
                file.set_len(len)?;
            }
            file.sync_data()?;
            sidecar::remove(&path)
        })
        .await
        .map_err(Error::task)??;
        Ok(len)
    }

    /// Deletes the partial file and its sidecar.
    async fn discard(&self) -> Result<()> {
        let (file_path, path, disk) = (
            self.path.clone(),
            self.sidecar_path.clone(),
            self.disk.clone(),
        );
        spawn_blocking(move || {
            let _guard = lock(&disk);
            sidecar::remove(&path)?;
            match std::fs::remove_file(&file_path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(Error::io(&file_path)(e)),
            }
        })
        .await
        .map_err(Error::task)?
    }

    /// Switches to single-stream mode, discarding segmented progress.
    async fn fall_back_to_single(&self) -> Result<()> {
        self.segmented.store(false, Ordering::Relaxed);
        lock(&self.sidecar).resumable = false;
        self.truncate().await?;
        self.checkpoint().await
    }

    /// Empties the file and forgets all progress.
    async fn truncate(&self) -> Result<()> {
        self.map().clear(0);
        self.downloaded.store(0, Ordering::Relaxed);
        let file = self.file.clone();
        spawn_blocking(move || file.set_len(0))
            .await
            .map_err(Error::task)?
    }

    fn progress(&self, speed: u64) -> Progress {
        let downloaded = self.downloaded();
        let total = self.info.size;
        let connections = self.active.load(Ordering::Relaxed);
        let segments = if self.segmented() {
            self.map().snapshot()
        } else {
            vec![SegmentInfo {
                start: 0,
                position: downloaded,
                end: total.unwrap_or(downloaded),
                active: connections > 0,
            }]
        };
        let eta = match total {
            Some(t) if speed > 0 => Some(Duration::from_secs(t.saturating_sub(downloaded) / speed)),
            _ => None,
        };
        Progress {
            downloaded,
            total,
            speed,
            eta,
            connections,
            segments,
        }
    }
}

/// Counts a connection as active while alive.
struct ActiveGuard<'a>(&'a AtomicUsize);

impl<'a> ActiveGuard<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Monitor: progress events and periodic checkpoints
// ---------------------------------------------------------------------------

struct Monitor {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Monitor {
    fn spawn(t: &Arc<Transfer>) -> Self {
        let t = t.clone();
        let (stop, mut stop_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(t.ctx.config.progress_interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut meter = SpeedMeter::new(t.downloaded());
            let mut last_checkpoint = Instant::now();
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stop_rx => break,
                    _ = tick.tick() => {}
                }
                let speed = meter.sample(t.downloaded());
                t.ctx.emit(Event::Progress(t.progress(speed)));
                if t.segmented() && last_checkpoint.elapsed() >= t.ctx.config.checkpoint_interval {
                    last_checkpoint = Instant::now();
                    if let Err(e) = t.checkpoint().await {
                        tracing::warn!("checkpoint failed: {}", error_chain(&e));
                    }
                }
            }
        });
        Self { stop, task }
    }

    /// Stops the monitor and waits, so no checkpoint is still in flight.
    async fn shutdown(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}

/// Exponentially smoothed transfer rate.
struct SpeedMeter {
    last_bytes: u64,
    last_time: Instant,
    rate: Option<f64>,
}

impl SpeedMeter {
    fn new(bytes: u64) -> Self {
        Self {
            last_bytes: bytes,
            last_time: Instant::now(),
            rate: None,
        }
    }

    fn sample(&mut self, bytes: u64) -> u64 {
        let now = Instant::now();
        let dt = now.duration_since(self.last_time).as_secs_f64();
        if bytes < self.last_bytes {
            // Restarted from zero.
            *self = Self::new(bytes);
            return 0;
        }
        if dt <= 0.0 {
            return self.rate.unwrap_or_default() as u64;
        }
        let instant = (bytes - self.last_bytes) as f64 / dt;
        let rate = match self.rate {
            Some(r) => 0.3 * instant + 0.7 * r,
            None => instant,
        };
        self.rate = Some(rate);
        self.last_bytes = bytes;
        self.last_time = now;
        rate as u64
    }
}

// ---------------------------------------------------------------------------
// Segmented mode
// ---------------------------------------------------------------------------

/// `Ok(true)`: complete. `Ok(false)`: interrupted by a control request.
async fn run_segmented(t: &Arc<Transfer>) -> Result<bool> {
    t.map().reset();
    let monitor = Monitor::spawn(t);
    let mut workers = JoinSet::new();
    for _ in 0..t.ctx.spec.connection_count() {
        workers.spawn(worker(t.clone()));
    }

    let mut failure = None;
    let mut interrupted = false;
    while let Some(joined) = workers.join_next().await {
        match joined.map_err(Error::task).and_then(|r| r) {
            Ok(stopped) => interrupted |= stopped,
            Err(e) if failure.is_none() => {
                failure = Some(e);
                // The other workers' uncommitted bytes are simply refetched
                // next time; nothing half-written is ever marked complete.
                workers.abort_all();
            }
            Err(_) => {} // cancellations caused by abort_all
        }
    }
    monitor.shutdown().await;

    if let Some(e) = failure {
        return Err(e);
    }
    if t.map().is_complete() {
        return Ok(true);
    }
    if interrupted {
        return Ok(false);
    }
    Err(Error::Protocol(
        "all connections finished but the file is incomplete".into(),
    ))
}

enum SegmentEnd {
    Done,
    Stopped,
    /// The request ended at the request size limit; more of the segment
    /// is left.
    More,
}

/// Takes segments until there is nothing left to take. `Ok(true)` if it
/// stopped because of a control request.
async fn worker(t: Arc<Transfer>) -> Result<bool> {
    let mut rx = t.ctx.control.subscribe();
    let mut buf = Vec::with_capacity(FLUSH_BYTES + 64 * 1024);
    loop {
        if *rx.borrow() != Control::Run {
            return Ok(true);
        }
        let Some(id) = t.map().assign() else {
            return Ok(false);
        };
        let result = download_segment(&t, id, &mut rx, &mut buf).await;
        t.map().release(id);
        match result? {
            SegmentEnd::Done | SegmentEnd::More => {}
            SegmentEnd::Stopped => return Ok(true),
        }
    }
}

/// Downloads one segment, retrying transient failures from where it left off.
async fn download_segment(
    t: &Transfer,
    id: SegmentId,
    rx: &mut watch::Receiver<Control>,
    buf: &mut Vec<u8>,
) -> Result<SegmentEnd> {
    let policy = &t.ctx.spec.retry;
    let mut failures = 0;
    loop {
        let before = t.map().bounds(id).map_or(0, |(cursor, _)| cursor);
        let err = match fetch_range(t, id, rx, buf).await {
            Ok(SegmentEnd::More) => {
                failures = 0;
                continue;
            }
            Ok(end) => return Ok(end),
            Err(e) => e,
        };
        let Some((cursor, end)) = t.map().bounds(id) else {
            return Err(err);
        };
        if cursor >= end {
            return Ok(SegmentEnd::Done);
        }
        if !err.is_retryable() {
            return Err(err);
        }
        if cursor.saturating_sub(before) >= RETRY_RESET_PROGRESS {
            failures = 0; // this attempt got somewhere before failing
        }
        failures += 1;
        if failures > policy.max_retries {
            return Err(err);
        }
        let delay = policy.delay(failures, err.retry_after());
        tracing::debug!(
            segment = id,
            attempt = failures,
            ?delay,
            "retrying: {}",
            error_chain(&err)
        );
        t.ctx.emit(Event::Retrying {
            attempt: failures,
            delay,
            reason: error_chain(&err),
        });
        tokio::select! {
            biased;
            _ = until_stopped(rx) => return Ok(SegmentEnd::Stopped),
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// One HTTP request for the rest of a segment (or the next part of it, with
/// a request size limit).
async fn fetch_range(
    t: &Transfer,
    id: SegmentId,
    rx: &mut watch::Receiver<Control>,
    buf: &mut Vec<u8>,
) -> Result<SegmentEnd> {
    let _permit = tokio::select! {
        biased;
        _ = until_stopped(rx) => return Ok(SegmentEnd::Stopped),
        p = t.ctx.host.acquire() => p.map_err(|_| closed())?,
    };
    let Some((start, end)) = t.map().bounds(id) else {
        return Ok(SegmentEnd::Done);
    };
    if start >= end {
        return Ok(SegmentEnd::Done);
    }
    let request_end = match t.ctx.spec.max_request_size {
        Some(max) => end.min(start.saturating_add(max.get())),
        None => end,
    };
    let requested = request_end - start;

    let mut req = t
        .client
        .get()
        .header(header::RANGE, format!("bytes={start}-{}", request_end - 1));
    if let Some(v) = t.info.if_range() {
        req = req.header(header::IF_RANGE, v);
    }
    let mut resp = tokio::select! {
        biased;
        _ = until_stopped(rx) => return Ok(SegmentEnd::Stopped),
        r = req.send() => r.map_err(Error::network)?,
    };
    check_range_response(t, &resp, start)?;

    let _active = ActiveGuard::new(&t.active);
    let mut buf_offset = start;
    let mut received = 0u64;
    let outcome = loop {
        let chunk = tokio::select! {
            biased;
            _ = until_stopped(rx) => break Ok(SegmentEnd::Stopped),
            c = resp.chunk() => c,
        };
        let bytes = match chunk {
            Ok(Some(b)) => b,
            Ok(None) => {
                break match t.map().bounds(id) {
                    // The segment may have shrunk (work stealing) below the
                    // requested end; only bytes short of both are missing.
                    Some((cursor, seg_end)) if cursor < seg_end.min(request_end) => {
                        Err(Error::Truncated {
                            received: cursor - start,
                            expected: request_end - start,
                        })
                    }
                    Some((cursor, seg_end)) if cursor < seg_end => Ok(SegmentEnd::More),
                    _ => Ok(SegmentEnd::Done),
                };
            }
            Err(e) => break Err(Error::network(e)),
        };
        received += bytes.len() as u64;
        if received > requested {
            // The body outruns the range that was asked for, so it is not that
            // range: the surplus would otherwise be dropped silently and the
            // bytes that were kept blamed on offsets they never came from.
            break Err(Error::RangeNotHonored);
        }
        tokio::select! {
            biased;
            _ = until_stopped(rx) => break Ok(SegmentEnd::Stopped),
            _ = t.throttle(bytes.len() as u64) => {}
        }
        let claim = t.map().claim(id, bytes.len() as u64);
        if buf.is_empty() {
            buf_offset = claim.offset;
        }
        buf.extend_from_slice(bytes.get(..claim.len as usize).unwrap_or_default());
        if claim.exhausted {
            break Ok(SegmentEnd::Done);
        }
        if buf.len() >= FLUSH_BYTES
            && let Err(e) = t.write(buf_offset, buf).await
        {
            break Err(e);
        }
    };
    // Always persist what was claimed, even when stopping or failing, so the
    // segment cursor equals the committed position before any retry.
    t.write(buf_offset, buf).await?;
    outcome
}

fn check_range_response(t: &Transfer, resp: &Response, start: u64) -> Result<()> {
    match resp.status() {
        StatusCode::PARTIAL_CONTENT => {}
        StatusCode::OK => {
            let etag = http::header_str(resp.headers(), header::ETAG);
            return Err(match (&t.info.etag, etag) {
                (Some(old), Some(new)) if *old != new => {
                    Error::ResourceChanged(format!("ETag changed from {old} to {new}"))
                }
                _ => Error::RangeNotHonored,
            });
        }
        _ => return Err(http::status_error(resp)),
    }
    // A server that starts encoding the entity part-way through a download is
    // no longer sending bytes that belong at `start`; the single-stream path
    // is the only one that can still make sense of them.
    if let Some(encoding) = http::stored_encoding(resp.headers()) {
        tracing::warn!("the server switched to a {encoding}-encoded entity");
        return Err(Error::RangeNotHonored);
    }
    let cr = http::content_range(resp.headers())
        .ok_or_else(|| Error::Protocol("206 response without a valid Content-Range".into()))?;
    if cr.first != start {
        return Err(Error::Protocol(format!(
            "requested bytes from {start} but the server sent from {}",
            cr.first
        )));
    }
    if cr.total.is_some() && cr.total != t.info.size {
        return Err(Error::ResourceChanged(format!(
            "size changed from {} to {}",
            t.info.size.unwrap_or_default(),
            cr.total.unwrap_or_default()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Single-stream mode (no ranges or unknown size)
// ---------------------------------------------------------------------------

/// Streams the whole body over one connection. Not resumable: every retry
/// (and every resume after a pause) starts from zero.
async fn run_single(t: &Arc<Transfer>, mut body: Option<Response>) -> Result<bool> {
    let mut rx = t.ctx.control.subscribe();
    let policy = &t.ctx.spec.retry;
    let monitor = Monitor::spawn(t);
    let mut failures = 0;
    let result = loop {
        if let Err(e) = t.truncate().await {
            break Err(e);
        }
        match stream_single(t, body.take(), &mut rx).await {
            Ok(done) => break Ok(done),
            Err(e) if e.is_retryable() && failures < policy.max_retries => {
                failures += 1;
                let delay = policy.delay(failures, e.retry_after());
                t.ctx.emit(Event::Retrying {
                    attempt: failures,
                    delay,
                    reason: format!("{} (restarting from zero)", error_chain(&e)),
                });
                tokio::select! {
                    biased;
                    _ = until_stopped(&mut rx) => break Ok(false),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            Err(e) => break Err(e),
        }
    };
    monitor.shutdown().await;
    result
}

async fn stream_single(
    t: &Transfer,
    body: Option<Response>,
    rx: &mut watch::Receiver<Control>,
) -> Result<bool> {
    let _permit = tokio::select! {
        biased;
        _ = until_stopped(rx) => return Ok(false),
        p = t.ctx.host.acquire() => p.map_err(|_| closed())?,
    };
    let mut resp = match body {
        Some(r) => r,
        None => tokio::select! {
            biased;
            _ = until_stopped(rx) => return Ok(false),
            r = t.client.get().send() => r.map_err(Error::network)?,
        },
    };
    if resp.status() != StatusCode::OK {
        return Err(http::status_error(&resp));
    }
    let expected = resp.content_length();

    let _active = ActiveGuard::new(&t.active);
    let mut buf = Vec::with_capacity(FLUSH_BYTES + 64 * 1024);
    let mut pos = 0u64;
    let outcome = loop {
        let chunk = tokio::select! {
            biased;
            _ = until_stopped(rx) => break Ok(false),
            c = resp.chunk() => c,
        };
        let bytes = match chunk {
            Ok(Some(b)) => b,
            Ok(None) => break Ok(true),
            Err(e) => break Err(Error::network(e)),
        };
        tokio::select! {
            biased;
            _ = until_stopped(rx) => break Ok(false),
            _ = t.throttle(bytes.len() as u64) => {}
        }
        buf.extend_from_slice(&bytes);
        pos += bytes.len() as u64;
        if buf.len() >= FLUSH_BYTES
            && let Err(e) = t.write(pos - buf.len() as u64, &mut buf).await
        {
            break Err(e);
        }
    };
    t.write(pos - buf.len() as u64, &mut buf).await?;
    match (outcome?, expected) {
        (true, Some(expected)) if pos != expected => Err(Error::Truncated {
            received: pos,
            expected,
        }),
        (done, _) => Ok(done),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolves once the control value is no longer `Run`.
async fn until_stopped(rx: &mut watch::Receiver<Control>) {
    until(rx, |c| *c != Control::Run).await;
}

async fn until(rx: &mut watch::Receiver<Control>, pred: impl FnMut(&Control) -> bool) {
    // An error means the sender is gone, which only happens on shutdown.
    let _ = rx.wait_for(pred).await;
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // No code panics while holding these locks, but never propagate poison.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn closed() -> Error {
    Error::Task("connection limiter closed".into())
}

/// `outer: inner: innermost`, for messages that leave the error type.
pub(crate) fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut s = e.to_string();
    let mut source = e.source();
    while let Some(inner) = source {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        source = inner.source();
    }
    s
}
