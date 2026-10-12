// In-browser stand-in for the Rust backend, used only when the UI is opened
// outside Tauri (e.g. `python3 -m http.server -d crates/rdm-gui/ui`). It
// simulates downloads so layout and interactions can be checked quickly.

const listeners = new Map();
const emit = (name, payload) => (listeners.get(name) ?? []).forEach((cb) => cb(payload));
const event = (type, data) => emit('rdm://event', { type, data });

const MIB = 1 << 20;
const now = () => Math.floor(Date.now() / 1000);
const home = '/Users/you/Downloads';

let settings = {
  defaultDirectory: home,
  connections: 8,
  minSplit: MIB,
  globalSpeedLimit: null,
  proxy: { mode: 'system' },
  closeToTray: true,
  clipboardMonitoring: true,
  antivirus: null,
  categories: [
    ['Video', 'mp4 mkv webm avi mov'],
    ['Music', 'mp3 m4a flac wav ogg'],
    ['Documents', 'pdf docx xlsx txt epub'],
    ['Archives', 'zip rar 7z tar gz iso dmg'],
    ['Programs', 'exe msi apk deb rpm pkg'],
    ['Other', ''],
  ].map(([name, exts], i) => ({ id: i + 1, name, extensions: exts.split(' ').filter(Boolean), directory: `${home}/${name}` })),
  queues: [{ id: 1, name: 'Main', maxConcurrent: 3, schedule: { enabled: true, start: '01:00', stop: '07:00', days: [1, 2, 3, 4, 5], once: false }, whenDone: 'nothing' }],
  checkUpdates: true,
  webVideo: { enabled: true, autoUpdate: true },
  capture: null, // filled from the catalog below
};

// Mirrors rdm_manager::FILE_TYPES (abridged is fine for previews).
const catalog = {
  groups: [
    ['video', 'Video', 'mp4 mkv webm avi mov wmv flv m4v mpg mpeg 3gp ogv ts m2ts vob rm rmvb asf'],
    ['audio', 'Audio', 'mp3 m4a aac flac wav ogg oga opus wma aiff alac mka'],
    ['archives', 'Archives', 'zip rar 7z tar gz tgz bz2 xz zst lz lzma cab arj'],
    ['programs', 'Programs', 'exe msi msix msu appx apk aab deb rpm appimage pkg jar flatpak snap'],
    ['diskImages', 'Disk images', 'iso img dmg vhd vhdx vmdk qcow2'],
    ['documents', 'Documents', 'pdf epub mobi djvu doc docx xls xlsx ppt pptx odt ods odp rtf csv txt'],
    ['images', 'Images', 'jpg jpeg png gif webp avif svg bmp tif tiff heic psd'],
    ['other', 'Other', 'torrent bin dat srt vtt ttf otf'],
  ].map(([id, name, exts]) => ({ id, name, extensions: exts.split(' ') })),
  defaults: [],
};
catalog.defaults = [
  ...catalog.groups.filter((g) => ['video', 'audio', 'archives', 'programs', 'diskImages'].includes(g.id)).flatMap((g) => g.extensions),
  'pdf', 'epub', 'mobi', 'djvu',
];
settings.capture = { extensions: [...catalog.defaults, 'r0*'], minSize: null, excludedSites: ['update.microsoft.com'], excludedAddresses: [], captureInline: false };
const webVideo = {
  ytdlp: { path: null, version: null, managed: false },
  deno: { path: null, version: null, managed: false },
  downloads: [
    { name: 'yt-dlp', repo: 'yt-dlp/yt-dlp', asset: 'yt-dlp_macos.zip', publisher: 'the yt-dlp project (github.com/yt-dlp/yt-dlp)', license: 'Unlicense (public domain)', sizeMb: 54 },
    { name: 'Deno', repo: 'denoland/deno', asset: 'deno-aarch64-apple-darwin.zip', publisher: 'the Deno project (github.com/denoland/deno)', license: 'MIT', sizeMb: 39 },
  ],
};
let skipped = [
  { extension: 'txt', count: 12, lastSeen: now() - 3600, ignored: false },
  { extension: 'json', count: 3, lastSeen: now() - 86400, ignored: false },
  { extension: 'torrent', count: 1, lastSeen: now() - 7200, ignored: false },
];
const running = new Set();

let nextId = 1;
const records = new Map();
const sims = new Map(); // id -> { segments, speed }

function categoryFor(name) {
  const ext = name.split('.').pop().toLowerCase();
  return (settings.categories.find((c) => c.extensions.includes(ext)) ?? settings.categories.find((c) => !c.extensions.length))?.id ?? null;
}

function record(fileName, size, status, downloaded, extra = {}) {
  const categoryId = categoryFor(fileName);
  const r = {
    id: nextId,
    url: `https://downloads.example.com/files/${encodeURIComponent(fileName)}`,
    fileName,
    directory: settings.categories.find((c) => c.id === categoryId)?.directory ?? home,
    categoryId,
    queueId: 1,
    status,
    error: null,
    size,
    downloaded,
    resumable: true,
    referer: null,
    userAgent: null,
    connections: null,
    speedLimit: null,
    addedAt: now() - (20 - nextId) * 3600,
    completedAt: status === 'completed' ? now() : null,
    position: nextId,
    ...extra,
  };
  nextId++;
  records.set(r.id, r);
  return r;
}

function startSim(r) {
  const n = 6;
  const segs = [];
  const total = r.size;
  for (let i = 0; i < n; i++) {
    const s = Math.floor((total * i) / n);
    const e = Math.floor((total * (i + 1)) / n);
    segs.push([s, s + Math.floor((e - s) * Math.random() * 0.6), e]);
  }
  sims.set(r.id, { segments: segs, speed: 6 * MIB + Math.random() * 10 * MIB });
}

record('ubuntu-24.04.2-desktop-amd64.iso', 6_114_656_256, 'downloading', 0);
record('Big.Buck.Bunny.1080p.mkv', 724 * MIB, 'downloading', 0);
record('lecture-07-distributed-systems.mp4', 412 * MIB, 'queued', 0);
record('Annual Report 2025.pdf', 8.4 * MIB, 'completed', 8.4 * MIB, { checksum: 'sha-256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08', checksumOk: true, scan: 'Scanned: no problems reported' });
record('podcast-episode-142.mp3', 61 * MIB, 'completed', 61 * MIB);
record('rustup-init.exe', 11 * MIB, 'failed', 3 * MIB, { error: 'server returned HTTP 403' });
record('photos-backup.zip', 2.2 * 1024 * MIB, 'stopped', 900 * MIB);
record('dataset.tar.gz', null, 'stopped', 0, { resumable: null });
for (const r of records.values()) if (r.status === 'downloading') startSim(r);

function tick() {
  const progress = [];
  for (const [id, sim] of sims) {
    const r = records.get(id);
    if (!r || r.status !== 'downloading') continue;
    const per = (sim.speed * 0.5) / sim.segments.length;
    for (const seg of sim.segments) seg[1] = Math.min(seg[2], seg[1] + per * (0.6 + Math.random() * 0.8));
    sim.segments = sim.segments.filter(([, p, e]) => p < e);
    r.downloaded = r.size - sim.segments.reduce((sum, [, p, e]) => sum + (e - p), 0);
    if (!sim.segments.length) {
      sims.delete(id);
      Object.assign(r, { status: 'completed', downloaded: r.size, completedAt: now() });
      event('updated', { ...r });
      continue;
    }
    progress.push({
      id,
      downloaded: r.downloaded,
      total: r.size,
      speed: Math.round(sim.speed * (0.9 + Math.random() * 0.2)),
      etaSecs: Math.round((r.size - r.downloaded) / sim.speed),
      connections: sim.segments.length,
      segments: sim.segments.map((s) => s.map(Math.round)),
    });
  }
  if (progress.length) event('progress', progress);
}
setInterval(tick, 500);

function queues() {
  return settings.queues.map((q) => ({
    ...q,
    running: running.has(q.id),
    active: [...records.values()].filter((r) => r.queueId === q.id && r.status === 'downloading').length,
    waiting: [...records.values()].filter((r) => r.queueId === q.id && r.status === 'queued').length,
  }));
}

function setStatus(id, status) {
  const r = records.get(id);
  if (!r) throw `download ${id} not found`;
  r.status = status;
  if (status === 'downloading') {
    r.error = null;
    if (!r.size) r.size = 300 * MIB;
    startSim(r);
  } else sims.delete(id);
  event('updated', { ...r });
  event('queues', queues());
}

const delay = (ms) => new Promise((res) => setTimeout(res, ms));

const commands = {
  list_downloads: () => [...records.values()].map((r) => ({ ...r })),
  live_progress: () => [],
  get_settings: () => structuredClone(settings),
  get_queues: () => queues(),
  update_settings: ({ settings: s }) => {
    if (!(s.connections >= 1 && s.connections <= 32)) throw 'invalid settings: connections must be between 1 and 32';
    settings = structuredClone(s);
    event('settings', structuredClone(settings));
    event('queues', queues());
    return structuredClone(settings);
  },
  probe_url: async ({ request }) => {
    if (/youtube\.com|youtu\.be|vimeo\.com/.test(request.url)) {
      await new Promise((r) => setTimeout(r, 900));
      if (/list=/.test(request.url)) {
        return {
          url: request.url, finalUrl: request.url, fileName: 'Talks.mp4', size: null, resumable: true, contentType: 'text/html',
          categoryId: 1, directory: `${home}/Video`, stream: null, page: true, duplicate: null, serverChecksum: null,
          site: { title: 'Conference talks 2026', extractor: 'YoutubeTab', pageUrl: request.url, choices: [],
            entries: ['Opening keynote', 'Async Rust in practice', 'Shipping a download manager', 'Q&A'].map((t, i) => ({ url: `https://www.youtube.com/watch?v=talk${i}`, title: t, duration: 1800 + i * 60 })) },
        };
      }
      return {
        url: request.url, finalUrl: request.url, fileName: 'Big Buck Bunny 60fps 4K.mp4', size: 1_372_000_000, resumable: true, contentType: 'text/html',
        categoryId: 1, directory: `${home}/Video`, stream: null, page: true, duplicate: null, serverChecksum: null,
        site: {
          title: 'Big Buck Bunny 60fps 4K', extractor: 'Youtube', pageUrl: request.url, duration: 635, entries: [],
          choices: [
            { id: '315+140', label: '2160p60 · VP9 · 1.4 GB', height: 2160, audioOnly: false, size: 1_372_000_000 },
            { id: '299+140', label: '1080p60 · H.264 · 267 MB', height: 1080, audioOnly: false, size: 267_000_000 },
            { id: '298+140', label: '720p60 · H.264 · 160 MB', height: 720, audioOnly: false, size: 160_000_000 },
            { id: '18', label: '360p · H.264 · 30 MB', height: 360, audioOnly: false, size: 30_000_000 },
            { id: '140', label: 'Audio only · M4A · 10 MB', height: null, audioOnly: true, size: 10_000_000 },
          ],
        },
      };
    }
    await delay(400);
    if (!/^https?:\/\//.test(request.url)) throw `invalid URL \`${request.url}\`: relative URL without a base`;
    if (/\.(m3u8|mpd)(\?|$)/.test(request.url)) {
      return {
        url: request.url,
        finalUrl: request.url,
        fileName: `${request.title ?? 'Stream'}.mp4`,
        size: null,
        resumable: true,
        contentType: 'application/vnd.apple.mpegurl',
        categoryId: 1,
        directory: `${home}/Video`,
        stream: {
          kind: 'hls',
          url: request.url,
          duration: 2843,
          live: false,
          protection: null,
          variants: [
            { id: 'v3', label: '1080p · 6.0 Mbps · avc1+mp4a' },
            { id: 'v2', label: '720p · 3.0 Mbps · avc1+mp4a' },
            { id: 'v1', label: '480p · 1.4 Mbps · avc1+mp4a' },
          ],
          audio: [
            { id: 'a0', label: 'English · en', default: true },
            { id: 'a1', label: 'Español · es', default: false },
          ],
        },
      };
    }
    const duplicate = [...records.values()].find((r) => r.url === request.url) ?? null;
    const fileName = request.fileName ?? decodeURIComponent(request.url.split('/').pop() || 'download');
    const categoryId = request.categoryId ?? categoryFor(fileName);
    return {
      url: request.url,
      finalUrl: request.url,
      fileName,
      size: 128 * MIB,
      resumable: true,
      contentType: 'application/octet-stream',
      categoryId,
      directory: request.directory ?? settings.categories.find((c) => c.id === categoryId)?.directory ?? home,
      duplicate,
      serverChecksum: null,
    };
  },
  add_download: async ({ request }) => {
    const p = await commands.probe_url({ request });
    const r = record(p.fileName, p.size, request.start === 'queue' ? 'queued' : 'stopped', 0, {
      url: request.url,
      directory: p.directory,
      categoryId: p.categoryId,
      queueId: request.queueId ?? 1,
      position: nextId,
      addedAt: now(),
    });
    event('added', { ...r });
    if (request.start === 'now') setStatus(r.id, 'downloading');
    return { ...records.get(r.id) };
  },
  start_download: ({ id }) => setStatus(id, 'downloading'),
  stop_download: ({ id }) => setStatus(id, 'stopped'),
  enqueue_download: ({ id }) => setStatus(id, 'queued'),
  remove_download: ({ id }) => {
    records.delete(id);
    sims.delete(id);
    event('removed', id);
    event('queues', queues());
  },
  start_queue: ({ id }) => {
    running.add(id);
    const q = settings.queues.find((x) => x.id === id);
    const waiting = [...records.values()].filter((r) => r.queueId === id && r.status === 'queued');
    const active = [...records.values()].filter((r) => r.queueId === id && r.status === 'downloading').length;
    waiting.slice(0, Math.max(0, q.maxConcurrent - active)).forEach((r) => setStatus(r.id, 'downloading'));
    event('queues', queues());
  },
  stop_queue: ({ id }) => {
    running.delete(id);
    event('queues', queues());
  },
  open_file: () => {},
  show_in_folder: () => {},
  pick_folder: () => `${home}/Picked`,
  take_pending_adds: () => pending.splice(0),
  capture_catalog: () => structuredClone(catalog),
  skipped_types: () => skipped.filter((t) => !t.ignored && !settings.capture.extensions.includes(t.extension)),
  ignore_skipped: ({ extension }) => {
    skipped = skipped.map((t) => (t.extension === extension ? { ...t, ignored: true } : t));
  },
  add_batch: async ({ items, start }) => {
    await new Promise((r) => setTimeout(r, 400));
    return items.map((item) => {
      const name = decodeURIComponent(new URL(item.url).pathname.split('/').pop() || 'file');
      const r = record(name, 20 * MIB, start === 'now' ? 'downloading' : start === 'queue' ? 'queued' : 'stopped', 0);
      r.url = item.url;
      if (start === 'now') startSim(r);
      event('added', { ...r });
      return r.id;
    });
  },
  app_info: () => ({ version: '0.2.0', canInstall: true, releasesUrl: 'https://github.com/Mudales/rdm/releases', available: null }),
  check_update: async () => {
    await new Promise((r) => setTimeout(r, 700));
    return { version: '0.2.1', currentVersion: '0.2.0', notes: 'See CHANGELOG.md.\n\n- Faster resume\n- Fixes', date: '2026-10-20' };
  },
  install_update: async () => {
    for (let i = 1; i <= 10; i++) {
      await new Promise((r) => setTimeout(r, 150));
      emit('rdm://update-progress', { downloaded: i * 1e6, total: 1e7 });
    }
    throw 'mock: installing is not simulated';
  },
  open_releases_page: () => {},
  web_video_status: () => structuredClone(webVideo),
  install_web_video: async () => {
    await new Promise((r) => setTimeout(r, 1500));
    webVideo.ytdlp = { path: `${home}/../Library/Application Support/dev.rdm.desktop/tools/yt-dlp/yt-dlp_macos`, version: '2026.08.19', managed: true };
    webVideo.deno = { path: `${home}/../Library/Application Support/dev.rdm.desktop/tools/deno/deno`, version: 'deno 2.9.7', managed: true };
    return structuredClone(webVideo);
  },
  update_web_video: async () => {
    await new Promise((r) => setTimeout(r, 800));
    return null;
  },
  export_list: ({ format }) => `${home}/rdm-downloads.${format === 'json' ? 'json' : 'txt'}`,
  import_list: () => ({ added: 3, duplicates: 1, invalid: [] }),
  import_text: ({ text }) => {
    const urls = text.split(/\s+/).filter((u) => /^https?:/.test(u));
    urls.forEach((u) => event('added', { ...record(decodeURIComponent(u.split('/').pop() || 'download'), null, 'queued', 0, { url: u }) }));
    return { added: urls.length, duplicates: 0, invalid: [] };
  },
  compute_checksum: () => 'b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9',
  shutdown_computer: () => {},
  ffmpeg_status: () => ffmpeg,
  install_ffmpeg: async () => {
    await delay(1500);
    ffmpeg = { ...ffmpeg, path: `${home}/../Library/Application Support/dev.rdm.desktop/tools/ffmpeg`, version: 'ffmpeg version 9.0.2' };
    return ffmpeg;
  },
  browser_integration_status: () => browsers.map((b) => ({ ...b })),
  install_browser_integration: ({ browser }) => {
    browsers.filter((b) => (browser ? b.browser === browser : b.detected)).forEach((b) => Object.assign(b, { installed: true, current: true }));
    return browsers.map((b) => ({ ...b }));
  },
  uninstall_browser_integration: ({ browser }) => {
    browsers.filter((b) => !browser || b.browser === browser).forEach((b) => Object.assign(b, { installed: false, current: false }));
    return browsers.map((b) => ({ ...b }));
  },
};

let ffmpeg = {
  path: null,
  version: null,
  download: { url: 'https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffmpeg.zip', publisher: 'Martin Riedl (ffmpeg.martin-riedl.de)', license: 'GPLv3', sizeMb: 29 },
  hint: 'Install ffmpeg with Homebrew (`brew install ffmpeg`) or let rdm download it.',
};

const browsers = [
  ['chrome', 'Chrome', true],
  ['chromium', 'Chromium', false],
  ['edge', 'Edge', true],
  ['brave', 'Brave', false],
  ['firefox', 'Firefox', true],
].map(([browser, name, detected]) => ({ browser, name, detected, installed: false, current: false, manifestPath: `/mock/${browser}/dev.rdm.host.json` }));

// Simulate the browser extension sending a video a few seconds after load.
const pending = [];
setTimeout(() => {
  pending.push([
    {
      url: 'https://videos.example.com/hls/master.m3u8',
      title: 'Rust in 2026 — keynote',
      referer: 'https://videos.example.com/watch?v=42',
      userAgent: navigator.userAgent,
      cookies: 'session=abc123',
    },
  ]);
  emit('rdm://add-request', null);
}, 3000);

/** A "download all links" request, as the extension sends it. */
function batchRequest() {
  const page = 'https://files.example.org/releases/';
  const names = ['rdm-0.2.0.dmg', 'rdm-0.2.0-setup.exe', 'rdm-0.2.0.AppImage', 'Release notes.pdf', 'checksums.txt', 'talk.mp4', 'theme.mp3', 'source.tar.gz', 'logo.png', 'slides.pptx'];
  pending.push(names.map((n) => ({ url: page + encodeURIComponent(n), pageUrl: page, referer: page })));
  emit('rdm://add-request', null);
}

// For poking the UI from the browser console: rdmMock.emit('notice', {message: 'hi'}),
// rdmMock.batch(), rdmMock.show('captureSettings'), rdmMock.update().
window.rdmMock = {
  emit: event,
  batch: batchRequest,
  show: (view) => emit('rdm://show-view', view),
  update: () => emit('rdm://update-available', { version: '0.2.1', currentVersion: '0.2.0', notes: null, date: '2026-10-20' }),
};

export const backend = {
  async invoke(cmd, args = {}) {
    const fn = commands[cmd];
    if (!fn) throw `unknown command ${cmd}`;
    return fn(args);
  },
  async listen(name, cb) {
    if (!listeners.has(name)) listeners.set(name, []);
    listeners.get(name).push(cb);
    return () => {};
  },
};
