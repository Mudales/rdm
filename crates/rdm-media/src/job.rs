//! Downloading a stream: pick renditions, fetch segments concurrently into a
//! parts folder (resumable), decrypt, join, and merge with ffmpeg when
//! needed.

use std::collections::HashMap;
use std::ffi::OsString;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rdm_core::{Destination, DownloadSpec, Engine, Fetcher, Outcome, Request, Url};
use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, broadcast, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::crypto;
use crate::dash;
use crate::error::{Error, Result};
use crate::ffmpeg::Ffmpeg;
use crate::hls::{self, Playlist};
use crate::model::{Container, Manifest, SegmentRef, StreamKind, Track, Variant};

/// Output format for a stream download.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputFormat {
    /// MP4 when possible; WebM/TS kept as-is when ffmpeg is unavailable.
    #[default]
    Auto,
    Mp4,
    Mkv,
    /// Audio only, AAC in MP4 (stream copy).
    M4a,
    /// Audio only, re-encoded to MP3.
    Mp3,
}

impl OutputFormat {
    fn audio_only(self) -> bool {
        matches!(self, OutputFormat::M4a | OutputFormat::Mp3)
    }
}

/// A stream download.
#[derive(Debug, Clone)]
pub struct MediaJob {
    /// The playlist / MPD URL with the browser context (cookies, Referer).
    pub request: Request,
    /// Variant id from [`Manifest::variants`]; best quality if `None`.
    pub variant: Option<String>,
    /// Audio track id; the default track if `None`.
    pub audio: Option<String>,
    /// Desired output path. The extension may change if the chosen format
    /// cannot be produced (e.g. `.ts` when ffmpeg is missing).
    pub output: PathBuf,
    pub format: OutputFormat,
    /// Segments fetched at once.
    pub concurrency: usize,
    pub speed_limit: Option<NonZeroU64>,
}

impl MediaJob {
    pub fn new(request: Request, output: PathBuf) -> Self {
        Self {
            request,
            variant: None,
            audio: None,
            output,
            format: OutputFormat::Auto,
            concurrency: 6,
            speed_limit: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Inspecting,
    Downloading,
    Merging,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProgress {
    pub segments_done: usize,
    pub segments_total: usize,
    pub bytes: u64,
    /// Bytes per second.
    pub speed: u64,
    pub eta: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MediaEvent {
    /// Renditions chosen; `segments` units to fetch (including init
    /// segments), `resumed` of them already on disk.
    Started {
        variant: String,
        audio: Option<String>,
        segments: usize,
        resumed: usize,
    },
    Phase(Phase),
    Progress(MediaProgress),
    Warning(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaOutcome {
    Completed {
        path: PathBuf,
        size: u64,
    },
    /// Stopped; downloaded segments are kept for resuming.
    Stopped,
    /// Cancelled; downloaded segments were deleted.
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Run,
    Stop,
    Cancel,
}

/// Downloads HLS and DASH streams with the core engine's network stack.
#[derive(Debug, Clone)]
pub struct MediaEngine {
    engine: Engine,
    ffmpeg: Ffmpeg,
}

impl MediaEngine {
    pub fn new(engine: Engine, ffmpeg: Ffmpeg) -> Self {
        Self { engine, ffmpeg }
    }

    pub fn ffmpeg(&self) -> &Ffmpeg {
        &self.ffmpeg
    }

    /// Fetches and parses a playlist or MPD.
    pub async fn inspect(&self, request: &Request) -> Result<Manifest> {
        let fetcher = self.engine.fetcher(request)?;
        Ok(load(&fetcher, &request.url).await?.manifest)
    }

    /// Starts a download in the background (inside a Tokio runtime).
    pub fn start(&self, job: MediaJob) -> Result<MediaHandle> {
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| rdm_core::Error::NoRuntime)?;
        let (control, _) = watch::channel(Control::Run);
        let control = Arc::new(control);
        let (events, first) = broadcast::channel(256);
        let ctx = Ctx {
            engine: self.engine.clone(),
            ffmpeg: self.ffmpeg.clone(),
            control: control.clone(),
            events: events.clone(),
        };
        let task = runtime.spawn(async move {
            let result = run(&ctx, job).await;
            if let Err(e) = &result {
                tracing::debug!("stream download failed: {e}");
            }
            result
        });
        Ok(MediaHandle {
            control: MediaControl { control, events },
            first: Mutex::new(Some(first)),
            task,
        })
    }
}

/// Controls a running stream download.
#[derive(Debug)]
pub struct MediaHandle {
    control: MediaControl,
    first: Mutex<Option<broadcast::Receiver<MediaEvent>>>,
    task: JoinHandle<Result<MediaOutcome>>,
}

impl MediaHandle {
    /// The first subscriber gets every event since the start.
    pub fn subscribe(&self) -> broadcast::Receiver<MediaEvent> {
        self.first
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| self.control.events.subscribe())
    }

    pub fn control(&self) -> MediaControl {
        self.control.clone()
    }

    pub async fn wait(self) -> Result<MediaOutcome> {
        self.task.await.map_err(|e| Error::Task(e.to_string()))?
    }
}

/// Cloneable stop/cancel for a stream download.
#[derive(Debug, Clone)]
pub struct MediaControl {
    control: Arc<watch::Sender<Control>>,
    events: broadcast::Sender<MediaEvent>,
}

impl MediaControl {
    /// Stops, keeping downloaded segments for a later resume.
    pub fn stop(&self) {
        self.control.send_if_modified(|c| {
            let change = *c == Control::Run;
            if change {
                *c = Control::Stop;
            }
            change
        });
    }

    /// Stops and deletes downloaded segments.
    pub fn cancel(&self) {
        self.control.send_replace(Control::Cancel);
    }
}

struct Ctx {
    engine: Engine,
    ffmpeg: Ffmpeg,
    control: Arc<watch::Sender<Control>>,
    events: broadcast::Sender<MediaEvent>,
}

impl Ctx {
    fn emit(&self, e: MediaEvent) {
        let _ = self.events.send(e);
    }
}

// ---------------------------------------------------------------------------
// Loading manifests
// ---------------------------------------------------------------------------

struct Loaded {
    manifest: Manifest,
    source: Source,
}

enum Source {
    HlsMaster { master: hls::Master },
    HlsMedia { media: Box<hls::Media> },
    Dash { tracks: HashMap<String, Track> },
}

async fn load(fetcher: &Fetcher, url: &Url) -> Result<Loaded> {
    let fetched = fetcher.get(url, None).await?;
    let text = String::from_utf8_lossy(&fetched.body);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with("#EXTM3U") {
        match hls::parse(&text, &fetched.final_url)? {
            Playlist::Master(master) => {
                // Liveness and duration live in the media playlists.
                let best = best_variant(master.variants.iter().map(|(v, _)| v))
                    .and_then(|v| master.variants.iter().find(|(x, _)| x.id == v.id))
                    .map(|(_, u)| u.clone())
                    .ok_or(Error::Empty)?;
                let media = load_media(fetcher, &best).await?;
                let mut variants: Vec<Variant> =
                    master.variants.iter().map(|(v, _)| v.clone()).collect();
                variants.sort_by_key(|v| std::cmp::Reverse(v.bandwidth));
                let manifest = Manifest {
                    kind: StreamKind::Hls,
                    url: url.to_string(),
                    variants,
                    audio: master.audio.iter().map(|(a, _)| a.clone()).collect(),
                    duration: Some(media.duration),
                    live: !media.ended,
                    protection: master.protection.clone().or(media.protection.clone()),
                };
                Ok(Loaded {
                    manifest,
                    source: Source::HlsMaster { master },
                })
            }
            Playlist::Media(media) => {
                let manifest = Manifest {
                    kind: StreamKind::Hls,
                    url: url.to_string(),
                    variants: vec![Variant {
                        id: "v0".into(),
                        bandwidth: None,
                        width: None,
                        height: None,
                        codecs: None,
                        frame_rate: None,
                        audio_group: None,
                        label: "Default".into(),
                    }],
                    audio: Vec::new(),
                    duration: Some(media.duration),
                    live: !media.ended,
                    protection: media.protection.clone(),
                };
                Ok(Loaded {
                    manifest,
                    source: Source::HlsMedia { media },
                })
            }
        }
    } else if trimmed.starts_with("<?xml") || trimmed.starts_with("<MPD") || text.contains("<MPD") {
        let mpd = dash::parse(&text, &fetched.final_url)?;
        Ok(Loaded {
            manifest: mpd.manifest,
            source: Source::Dash { tracks: mpd.tracks },
        })
    } else {
        Err(Error::NotAStream)
    }
}

async fn load_media(fetcher: &Fetcher, url: &Url) -> Result<Box<hls::Media>> {
    let fetched = fetcher.get(url, None).await?;
    match hls::parse(&String::from_utf8_lossy(&fetched.body), &fetched.final_url)? {
        Playlist::Media(m) => Ok(m),
        Playlist::Master(_) => Err(Error::parse(
            "HLS playlist",
            "a variant points to another master playlist",
        )),
    }
}

fn best_variant<'a>(variants: impl Iterator<Item = &'a Variant>) -> Option<&'a Variant> {
    variants.max_by_key(|v| v.bandwidth.unwrap_or(0))
}

// ---------------------------------------------------------------------------
// Running a job
// ---------------------------------------------------------------------------

/// Tracks to download, named for the parts folder.
struct Plan {
    tracks: Vec<(&'static str, Track)>,
    variant: String,
    audio: Option<String>,
}

async fn plan(fetcher: &Fetcher, loaded: Loaded, job: &MediaJob) -> Result<Plan> {
    let m = &loaded.manifest;
    if let Some(p) = &m.protection {
        return Err(Error::Protected(p.clone()));
    }
    if m.live {
        return Err(Error::Live);
    }
    let variant = match &job.variant {
        Some(id) => m
            .variants
            .iter()
            .find(|v| &v.id == id)
            .ok_or_else(|| Error::NoSuchVariant(id.clone()))?,
        None => best_variant(m.variants.iter()).ok_or(Error::Empty)?,
    }
    .clone();

    match loaded.source {
        Source::HlsMedia { media } => Ok(Plan {
            tracks: vec![("video", media.track)],
            variant: variant.label,
            audio: None,
        }),
        Source::HlsMaster { master } => {
            let url = master
                .variants
                .iter()
                .find(|(v, _)| v.id == variant.id)
                .map(|(_, u)| u.clone())
                .ok_or(Error::Empty)?;
            // Audio renditions that go with this variant.
            let group: Vec<_> = master
                .audio
                .iter()
                .filter(|(a, _)| a.group.is_some() && a.group == variant.audio_group)
                .collect();
            let chosen = match &job.audio {
                Some(id) => group.iter().find(|(a, _)| &a.id == id),
                None => group.iter().find(|(a, _)| a.default).or(group.first()),
            };
            let separate = chosen.and_then(|(a, u)| u.as_ref().map(|u| (a.clone(), u.clone())));

            let mut tracks = Vec::new();
            if !(job.format.audio_only() && separate.is_some()) {
                let media = load_media(fetcher, &url).await?;
                check_media(&media)?;
                tracks.push(("video", media.track));
            }
            let mut audio_label = None;
            if let Some((a, u)) = separate {
                let media = load_media(fetcher, &u).await?;
                check_media(&media)?;
                tracks.push(("audio", media.track));
                audio_label = Some(a.label);
            }
            Ok(Plan {
                tracks,
                variant: variant.label,
                audio: audio_label,
            })
        }
        Source::Dash { mut tracks } => {
            let video_is_audio = m.audio.is_empty();
            let audio = if video_is_audio {
                None
            } else {
                match &job.audio {
                    Some(id) => Some(
                        m.audio
                            .iter()
                            .find(|a| &a.id == id)
                            .ok_or_else(|| Error::NoSuchVariant(id.clone()))?,
                    ),
                    None => m.audio.iter().find(|a| a.default).or(m.audio.first()),
                }
            };
            let mut out = Vec::new();
            if !(job.format.audio_only() && audio.is_some()) {
                out.push(("video", tracks.remove(&variant.id).ok_or(Error::Empty)?));
            }
            if let Some(a) = audio {
                out.push(("audio", tracks.remove(&a.id).ok_or(Error::Empty)?));
            }
            Ok(Plan {
                tracks: out,
                variant: variant.label,
                audio: audio.map(|a| a.label.clone()),
            })
        }
    }
}

fn check_media(media: &hls::Media) -> Result<()> {
    if let Some(p) = &media.protection {
        return Err(Error::Protected(p.clone()));
    }
    if !media.ended {
        return Err(Error::Live);
    }
    Ok(())
}

/// What the parts folder belongs to; a different job wipes it.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PartsInfo {
    url: String,
    variant: Option<String>,
    audio: Option<String>,
    format: OutputFormat,
}

fn parts_dir(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_owned();
    name.push(".rdm-parts");
    PathBuf::from(name)
}

async fn run(ctx: &Ctx, job: MediaJob) -> Result<MediaOutcome> {
    ctx.emit(MediaEvent::Phase(Phase::Inspecting));
    let fetcher = ctx
        .engine
        .fetcher(&job.request)?
        .with_speed_limit(job.speed_limit);
    let loaded = load(&fetcher, &job.request.url).await?;
    let plan = plan(&fetcher, loaded, &job).await?;

    let parts = parts_dir(&job.output);
    prepare_parts(&parts, &job)?;
    let units = plan.tracks.iter().map(|(_, t)| t.units()).sum::<usize>();
    let resumed = count_done(&parts, &plan);
    ctx.emit(MediaEvent::Started {
        variant: plan.variant.clone(),
        audio: plan.audio.clone(),
        segments: units,
        resumed,
    });

    ctx.emit(MediaEvent::Phase(Phase::Downloading));
    match download_all(ctx, &fetcher, &job, &plan, &parts, units, resumed).await? {
        Control::Run => {}
        Control::Stop => return Ok(MediaOutcome::Stopped),
        Control::Cancel => {
            let _ = std::fs::remove_dir_all(&parts);
            return Ok(MediaOutcome::Cancelled);
        }
    }

    ctx.emit(MediaEvent::Phase(Phase::Merging));
    let (path, warning) = finish(ctx, &job, &plan, &parts).await?;
    if let Some(w) = warning {
        ctx.emit(MediaEvent::Warning(w));
    }
    let _ = std::fs::remove_dir_all(&parts);
    let size = std::fs::metadata(&path).map_err(Error::io(&path))?.len();
    Ok(MediaOutcome::Completed { path, size })
}

fn prepare_parts(parts: &Path, job: &MediaJob) -> Result<()> {
    let info = PartsInfo {
        url: job.request.url.to_string(),
        variant: job.variant.clone(),
        audio: job.audio.clone(),
        format: job.format,
    };
    let info_path = parts.join("job.json");
    let same = std::fs::read(&info_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<PartsInfo>(&b).ok())
        .is_some_and(|old| old == info);
    if !same && parts.exists() {
        std::fs::remove_dir_all(parts).map_err(Error::io(parts))?;
    }
    std::fs::create_dir_all(parts).map_err(Error::io(parts))?;
    let json = serde_json::to_vec(&info).map_err(|e| Error::Task(e.to_string()))?;
    std::fs::write(&info_path, json).map_err(Error::io(&info_path))?;
    Ok(())
}

/// Parts file for unit `index` of a track (`None` = init segment).
fn unit_path(parts: &Path, track: &str, index: Option<usize>) -> PathBuf {
    match index {
        Some(i) => parts.join(format!("{track}-{i:06}.seg")),
        None => parts.join(format!("{track}-init.seg")),
    }
}

fn whole_path(parts: &Path, track: &str) -> PathBuf {
    parts.join(format!("{track}-whole.bin"))
}

fn count_done(parts: &Path, plan: &Plan) -> usize {
    plan.tracks
        .iter()
        .map(|(name, t)| {
            if t.whole.is_some() {
                // A finished whole-track download has no sidecar left.
                let p = whole_path(parts, name);
                usize::from(p.exists() && !rdm_core::sidecar_path(&p).exists())
            } else {
                let init = usize::from(t.init.is_some() && unit_path(parts, name, None).exists());
                init + (0..t.segments.len())
                    .filter(|i| unit_path(parts, name, Some(*i)).exists())
                    .count()
            }
        })
        .sum()
}

struct Shared {
    done: AtomicUsize,
    bytes: AtomicU64,
    keys: tokio::sync::Mutex<HashMap<Url, [u8; 16]>>,
}

async fn download_all(
    ctx: &Ctx,
    fetcher: &Fetcher,
    job: &MediaJob,
    plan: &Plan,
    parts: &Path,
    total: usize,
    resumed: usize,
) -> Result<Control> {
    let shared = Arc::new(Shared {
        done: AtomicUsize::new(resumed),
        bytes: AtomicU64::new(0),
        keys: tokio::sync::Mutex::new(HashMap::new()),
    });
    let monitor = spawn_monitor(ctx.events.clone(), shared.clone(), total);
    let limit = Arc::new(Semaphore::new(job.concurrency.max(1)));
    let mut tasks: JoinSet<Result<()>> = JoinSet::new();
    let mut whole_controls = Vec::new();

    for (name, track) in &plan.tracks {
        if let Some(url) = &track.whole {
            let path = whole_path(parts, name);
            if path.exists() && !rdm_core::sidecar_path(&path).exists() {
                continue;
            }
            let mut request = job.request.clone();
            request.url = url.clone();
            let mut spec = DownloadSpec::new(request, Destination::File(path));
            spec.overwrite = true;
            spec.speed_limit = job.speed_limit;
            let handle = ctx.engine.start(spec)?;
            whole_controls.push(handle.control());
            let shared = shared.clone();
            tasks.spawn(async move {
                let mut rx = handle.subscribe();
                let mut last = 0;
                let progress = async {
                    while let Ok(ev) = rx.recv().await {
                        if let rdm_core::Event::Progress(p) = ev {
                            shared
                                .bytes
                                .fetch_add(p.downloaded.saturating_sub(last), Ordering::Relaxed);
                            last = p.downloaded;
                        }
                    }
                };
                let (outcome, ()) = tokio::join!(handle.wait(), progress);
                match outcome? {
                    Outcome::Completed { .. } => {
                        shared.done.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    }
                    _ => Ok(()), // stopped along with the job
                }
            });
            continue;
        }
        let units = track.init.iter().map(|s| (None, s.clone())).chain(
            track
                .segments
                .iter()
                .cloned()
                .enumerate()
                .map(|(i, s)| (Some(i), s)),
        );
        for (index, seg) in units {
            let path = unit_path(parts, name, index);
            if path.exists() {
                continue;
            }
            let (fetcher, shared, limit) = (fetcher.clone(), shared.clone(), limit.clone());
            tasks.spawn(async move {
                let _permit = limit
                    .acquire_owned()
                    .await
                    .map_err(|e| Error::Task(e.to_string()))?;
                fetch_unit(&fetcher, &shared, &seg, &path).await
            });
        }
    }

    let mut rx = ctx.control.subscribe();
    let mut failure = None;
    let stop = loop {
        tokio::select! {
            biased;
            c = rx.wait_for(|c| *c != Control::Run) => break c.map_or(Control::Stop, |c| *c),
            next = tasks.join_next() => match next {
                None => break Control::Run,
                Some(Ok(Ok(()))) => {}
                Some(Ok(Err(e))) => { failure = Some(e); break Control::Run; }
                Some(Err(e)) => { failure = Some(Error::Task(e.to_string())); break Control::Run; }
            },
        }
    };
    for c in &whole_controls {
        if stop == Control::Cancel {
            c.cancel();
        } else {
            c.stop();
        }
    }
    if stop != Control::Run || failure.is_some() {
        tasks.abort_all();
    }
    while tasks.join_next().await.is_some() {}
    monitor.abort();
    match failure {
        Some(e) => Err(e),
        None => Ok(stop),
    }
}

async fn fetch_unit(
    fetcher: &Fetcher,
    shared: &Shared,
    seg: &SegmentRef,
    path: &Path,
) -> Result<()> {
    let fetched = fetcher.get(&seg.url, seg.range.clone()).await?;
    shared
        .bytes
        .fetch_add(fetched.body.len() as u64, Ordering::Relaxed);
    let mut data = fetched.body;
    if let Some(key) = &seg.key {
        let k = {
            let mut keys = shared.keys.lock().await;
            match keys.get(&key.uri) {
                Some(k) => *k,
                None => {
                    let body = fetcher.get(&key.uri, None).await?.body;
                    let k = crypto::key_from(&body)?;
                    keys.insert(key.uri.clone(), k);
                    k
                }
            }
        };
        let iv = key.iv;
        data = tokio::task::spawn_blocking(move || crypto::decrypt_aes128(data, &k, &iv))
            .await
            .map_err(|e| Error::Task(e.to_string()))??;
    }
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, &data)
        .await
        .map_err(Error::io(&tmp))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(Error::io(path))?;
    shared.done.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

fn spawn_monitor(
    events: broadcast::Sender<MediaEvent>,
    shared: Arc<Shared>,
    total: usize,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last = (Instant::now(), 0u64);
        let mut rate: Option<f64> = None;
        loop {
            tick.tick().await;
            let now = Instant::now();
            let bytes = shared.bytes.load(Ordering::Relaxed);
            let done = shared.done.load(Ordering::Relaxed);
            let dt = now.duration_since(last.0).as_secs_f64();
            if dt > 0.0 {
                let inst = bytes.saturating_sub(last.1) as f64 / dt;
                rate = Some(rate.map_or(inst, |r| 0.3 * inst + 0.7 * r));
            }
            last = (now, bytes);
            let speed = rate.unwrap_or(0.0) as u64;
            // Estimate from the average bytes per finished segment.
            let eta = (done > 0 && speed > 0 && done < total).then(|| {
                let per = bytes as f64 / done as f64;
                Duration::from_secs_f64(per * (total - done) as f64 / speed as f64)
            });
            let _ = events.send(MediaEvent::Progress(MediaProgress {
                segments_done: done,
                segments_total: total,
                bytes,
                speed,
                eta,
            }));
        }
    })
}

// ---------------------------------------------------------------------------
// Joining and merging
// ---------------------------------------------------------------------------

/// Concatenates a track's units into one file.
fn join_track(parts: &Path, name: &str, track: &Track) -> Result<PathBuf> {
    let out = parts.join(format!("{name}.{}", track.container.extension()));
    if track.whole.is_some() {
        let src = whole_path(parts, name);
        std::fs::rename(&src, &out).map_err(Error::io(&src))?;
        return Ok(out);
    }
    let mut file = std::fs::File::create(&out).map_err(Error::io(&out))?;
    let inputs = track
        .init
        .iter()
        .map(|_| unit_path(parts, name, None))
        .chain((0..track.segments.len()).map(|i| unit_path(parts, name, Some(i))));
    for input in inputs {
        let mut src = std::fs::File::open(&input).map_err(Error::io(&input))?;
        std::io::copy(&mut src, &mut file).map_err(Error::io(&out))?;
    }
    file.sync_all().map_err(Error::io(&out))?;
    Ok(out)
}

fn move_to(src: &Path, dest: &Path) -> Result<PathBuf> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(Error::io(dir))?;
    }
    std::fs::rename(src, dest).map_err(Error::io(dest))?;
    Ok(dest.to_path_buf())
}

fn os(s: impl AsRef<std::ffi::OsStr>) -> OsString {
    s.as_ref().to_owned()
}

/// Produces the output file. Returns its path and an optional warning.
async fn finish(
    ctx: &Ctx,
    job: &MediaJob,
    plan: &Plan,
    parts: &Path,
) -> Result<(PathBuf, Option<String>)> {
    let joined: Vec<(PathBuf, Container)> = {
        let parts = parts.to_path_buf();
        let tracks: Vec<_> = plan.tracks.iter().map(|(n, t)| (*n, t.clone())).collect();
        tokio::task::spawn_blocking(move || {
            tracks
                .iter()
                .map(|(n, t)| join_track(&parts, n, t).map(|p| (p, t.container)))
                .collect::<Result<Vec<_>>>()
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))??
    };
    let ffmpeg = ctx.ffmpeg.locate();
    let out = &job.output;

    match (joined.as_slice(), job.format) {
        ([(src, container)], OutputFormat::Auto) => match (container, &ffmpeg) {
            (Container::Mp4, _) => Ok((move_to(src, &out.with_extension("mp4"))?, None)),
            (Container::Ts, Some(f)) => {
                let dest = out.with_extension("mp4");
                Ffmpeg::run(f, &[os("-i"), os(src), os("-map"), os("0"), os("-c"), os("copy"), os(&dest)]).await?;
                Ok((dest, None))
            }
            (Container::Ts, None) => Ok((
                move_to(src, &out.with_extension("ts"))?,
                Some("ffmpeg was not found, so the video was saved as .ts (MPEG-TS), which most players handle.".into()),
            )),
            (c, _) => Ok((move_to(src, &out.with_extension(c.extension()))?, None)),
        },
        ([(src, Container::Mp4)], OutputFormat::Mp4) => Ok((move_to(src, &out.with_extension("mp4"))?, None)),
        ([(src, _)], format @ (OutputFormat::Mp4 | OutputFormat::Mkv)) => {
            let f = ffmpeg.ok_or(Error::FfmpegMissing("convert the stream to the chosen format"))?;
            let dest = out.with_extension(if format == OutputFormat::Mkv { "mkv" } else { "mp4" });
            Ffmpeg::run(&f, &[os("-i"), os(src), os("-map"), os("0"), os("-c"), os("copy"), os(&dest)]).await?;
            Ok((dest, None))
        }
        (inputs, format) if format.audio_only() => {
            let f = ffmpeg.ok_or(Error::FfmpegMissing("extract the audio"))?;
            // With a separate audio track it is the only input.
            let (src, _) = inputs.last().ok_or(Error::Empty)?;
            let (ext, codec): (&str, &[&str]) = if format == OutputFormat::Mp3 {
                ("mp3", &["-c:a", "libmp3lame", "-q:a", "2"])
            } else {
                ("m4a", &["-c:a", "copy"])
            };
            let dest = out.with_extension(ext);
            let mut args = vec![os("-i"), os(src), os("-vn")];
            args.extend(codec.iter().map(os));
            args.push(os(&dest));
            Ffmpeg::run(&f, &args).await?;
            Ok((dest, None))
        }
        ([(video, vc), (audio, ac)], format) => {
            let f = ffmpeg.ok_or(Error::FfmpegMissing("merge the separate video and audio tracks"))?;
            let webm = matches!(vc, Container::WebM) || matches!(ac, Container::WebM);
            let ext = match format {
                OutputFormat::Mkv => "mkv",
                OutputFormat::Auto if webm => "mkv",
                _ => "mp4",
            };
            let dest = out.with_extension(ext);
            Ffmpeg::run(
                &f,
                &[
                    os("-i"), os(video), os("-i"), os(audio),
                    os("-map"), os("0:v:0"), os("-map"), os("1:a:0"),
                    os("-c"), os("copy"), os(&dest),
                ],
            )
            .await?;
            Ok((dest, None))
        }
        _ => Err(Error::Unsupported("unexpected track layout".into())),
    }
}
