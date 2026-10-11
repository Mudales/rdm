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
  categories: [
    ['Video', 'mp4 mkv webm avi mov'],
    ['Music', 'mp3 m4a flac wav ogg'],
    ['Documents', 'pdf docx xlsx txt epub'],
    ['Archives', 'zip rar 7z tar gz iso dmg'],
    ['Programs', 'exe msi apk deb rpm pkg'],
    ['Other', ''],
  ].map(([name, exts], i) => ({ id: i + 1, name, extensions: exts.split(' ').filter(Boolean), directory: `${home}/${name}` })),
  queues: [{ id: 1, name: 'Main', maxConcurrent: 3 }],
};
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
record('Annual Report 2025.pdf', 8.4 * MIB, 'completed', 8.4 * MIB);
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
    await delay(400);
    if (!/^https?:\/\//.test(request.url)) throw `invalid URL \`${request.url}\`: relative URL without a base`;
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
