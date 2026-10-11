// rdm desktop UI.
//
// Talks to the Rust side through Tauri commands (`invoke`) and the
// `rdm://event` stream of manager events. Opened in a plain browser (no
// Tauri), it loads mock.js instead so the UI can be developed and checked
// without building the app.
//
// All DOM is built with textContent, never innerHTML with data: file names
// and URLs come from arbitrary servers.

const backend = window.__TAURI__
  ? {
      invoke: (cmd, args) => window.__TAURI__.core.invoke(cmd, args),
      listen: (name, cb) => window.__TAURI__.event.listen(name, (e) => cb(e.payload)),
    }
  : (await import('./mock.js')).backend;

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
const MAIN_QUEUE = 1;

const state = {
  records: new Map(), // id -> DownloadRecord
  live: new Map(), // id -> LiveProgress
  settings: null,
  queues: [],
  view: 'all', // all | unfinished | finished | cat:<id> | queue:<id>
  selected: new Set(),
  anchor: null,
  search: '',
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Creates an element. `attrs` keys starting with "on" become listeners. */
function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'class') el.className = v;
    else if (k === 'text') el.textContent = v;
    else if (k in el && typeof v !== 'string') el[k] = v;
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const c of children.flat()) if (c != null) el.append(c);
  return el;
}

const SVG_NS = 'http://www.w3.org/2000/svg';
/** Builds an icon from a list of [element, attributes] pairs. */
function icon(shapes) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
  for (const [tag, attrs] of shapes) {
    const el = document.createElementNS(SVG_NS, tag);
    for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
    svg.append(el);
  }
  return svg;
}

const UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
function fmtBytes(n) {
  if (n == null) return '—';
  let v = n;
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  if (i === 0) return `${n} B`;
  return `${v.toFixed(v < 10 ? 2 : v < 100 ? 1 : 0)} ${UNITS[i]}`;
}
const fmtSpeed = (n) => (n ? `${fmtBytes(n)}/s` : '');
const pad = (n) => String(n).padStart(2, '0');
function fmtEta(s) {
  if (s == null) return '';
  const hrs = Math.floor(s / 3600);
  const min = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (hrs) return `${hrs}:${pad(min)}:${pad(sec)}`;
  if (min) return `${min}:${pad(sec)}`;
  return `${sec}s`;
}
function fmtDate(unix) {
  if (!unix) return '';
  const d = new Date(unix * 1000);
  const now = new Date();
  if (d.toDateString() === now.toDateString()) {
    return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  }
  return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

function errorText(e) {
  return typeof e === 'string' ? e : e?.message ?? String(e);
}

function toast(message, isError = false) {
  const el = h('div', { class: `toast${isError ? ' error' : ''}`, text: message });
  $('#toasts').append(el);
  setTimeout(() => el.remove(), isError ? 9000 : 5000);
}

async function call(cmd, args) {
  try {
    return await backend.invoke(cmd, args);
  } catch (e) {
    toast(errorText(e), true);
    throw e;
  }
}

// ---------------------------------------------------------------------------
// Categories and colors
// ---------------------------------------------------------------------------

const CATEGORY_LOOK = {
  video: ['#ef4444', [['path', { d: 'M4 6h11v12H4zM15 10l5-3v10l-5-3' }]]],
  music: ['#a855f7', [['path', { d: 'M9 18V5l11-2v13' }], ['circle', { cx: 6, cy: 18, r: 3 }], ['circle', { cx: 17, cy: 16, r: 3 }]]],
  documents: ['#3b82f6', [['path', { d: 'M6 3h8l4 4v14H6zM14 3v4h4M9 12h6M9 16h6' }]]],
  archives: ['#f59e0b', [['path', { d: 'M4 8h16v12H4zM3 4h18v4H3zM10 12h4' }]]],
  programs: ['#10b981', [['path', { d: 'M4 5h16v14H4zM8 9l3 3-3 3M13 15h3' }]]],
  other: ['#64748b', [['path', { d: 'M6 3h8l4 4v14H6zM14 3v4h4' }]]],
};

const category = (id) => state.settings?.categories.find((c) => c.id === id);
const queue = (id) => state.queues.find((q) => q.id === id);

function categoryIcon(id) {
  const [color, shapes] = CATEGORY_LOOK[category(id)?.name.toLowerCase()] ?? CATEGORY_LOOK.other;
  const el = h('span', { class: 'cat-icon' }, icon(shapes));
  el.style.background = color;
  return el;
}

let palette = {};
function readPalette() {
  const css = getComputedStyle(document.documentElement);
  for (const name of ['accent', 'done', 'error', 'muted', 'track', 'warn']) {
    palette[name] = css.getPropertyValue(`--${name}`).trim();
  }
}
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
  readPalette();
  scheduleRender();
});

/**
 * Draws a progress bar. For running downloads every unfinished segment is a
 * gap in the bar, so you can see the connections working through the file.
 */
function drawProgress(canvas, r) {
  const dpr = window.devicePixelRatio || 1;
  const w = Math.round(canvas.clientWidth * dpr);
  const hgt = Math.round(canvas.clientHeight * dpr);
  if (!w || !hgt) return;
  if (canvas.width !== w) canvas.width = w;
  if (canvas.height !== hgt) canvas.height = hgt;
  const ctx = canvas.getContext('2d');
  const live = state.live.get(r.id);
  const total = live?.total ?? r.size;
  const color = {
    completed: palette.done,
    failed: palette.error,
    downloading: palette.accent,
    queued: palette.warn,
  }[r.status] ?? palette.muted;

  ctx.fillStyle = palette.track;
  ctx.fillRect(0, 0, w, hgt);
  ctx.fillStyle = color;
  if (r.status === 'completed') {
    ctx.fillRect(0, 0, w, hgt);
  } else if (live && total && live.segments.length) {
    ctx.fillRect(0, 0, w, hgt);
    ctx.fillStyle = palette.track;
    for (const [, pos, end] of live.segments) {
      const x0 = Math.floor((pos / total) * w);
      const x1 = Math.ceil((end / total) * w);
      ctx.fillRect(x0, 0, x1 - x0, hgt);
    }
    // A bright tick where each connection is currently writing.
    ctx.fillStyle = color;
    ctx.globalAlpha = 0.55;
    for (const [start, pos] of live.segments) {
      if (pos > start || pos === 0) ctx.fillRect(Math.floor((pos / total) * w), 0, Math.max(1, dpr), hgt);
    }
    ctx.globalAlpha = 1;
  } else {
    const f = fraction(r);
    if (f != null) ctx.fillRect(0, 0, Math.round(f * w), hgt);
  }
}

/** Fraction done in [0, 1], or null when unknown. */
function fraction(r) {
  if (r.status === 'completed') return 1;
  const live = state.live.get(r.id);
  if (live?.stream) return live.stream[1] ? live.stream[0] / live.stream[1] : null;
  const total = live?.total ?? r.size;
  return total ? Math.min(r.downloaded, total) / total : null;
}

function percent(r) {
  if (r.status === 'completed') return '100%';
  const live = state.live.get(r.id);
  if (live?.stream) {
    const f = fraction(r);
    return f == null ? '' : `${Math.floor(f * 1000) / 10}%`;
  }
  const total = live?.total ?? r.size;
  if (!total) return '';
  return `${Math.floor((Math.min(r.downloaded, total) / total) * 1000) / 10}%`;
}

function statusText(r) {
  const live = state.live.get(r.id);
  switch (r.status) {
    case 'downloading':
      return live?.etaSecs != null ? `${fmtEta(live.etaSecs)} left` : 'Downloading';
    case 'queued':
      return 'Queued';
    case 'stopped':
      return r.downloaded ? 'Stopped' : 'Not started';
    case 'completed':
      return 'Completed';
    case 'failed':
      return 'Failed';
    default:
      return r.status;
  }
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

function matchesView(r, view = state.view) {
  if (view === 'all') return true;
  if (view === 'unfinished') return r.status !== 'completed';
  if (view === 'finished') return r.status === 'completed';
  const [kind, id] = view.split(':');
  if (kind === 'cat') return r.categoryId === Number(id);
  if (kind === 'queue') return r.queueId === Number(id);
  return true;
}

function visibleRecords() {
  const q = state.search.trim().toLowerCase();
  return [...state.records.values()]
    .filter((r) => matchesView(r))
    .filter((r) => !q || r.fileName.toLowerCase().includes(q) || r.url.toLowerCase().includes(q))
    .sort((a, b) => b.position - a.position);
}

const selectedRecords = () => [...state.selected].map((id) => state.records.get(id)).filter(Boolean);

/** The queue the queue button controls: the one being viewed, else Main. */
function currentQueue() {
  const [kind, id] = state.view.split(':');
  return queue(kind === 'queue' ? Number(id) : MAIN_QUEUE) ?? state.queues[0];
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

let renderPending = false;
function scheduleRender() {
  if (renderPending) return;
  renderPending = true;
  requestAnimationFrame(() => {
    renderPending = false;
    renderSidebar();
    renderTable();
    renderToolbar();
    renderDetails();
    renderStatus();
  });
}

function renderSidebar() {
  const all = [...state.records.values()];
  const count = (view) => all.filter((r) => matchesView(r, view)).length;
  const item = (view, label, extra) =>
    h(
      'button',
      {
        class: 'nav-item',
        'aria-current': String(state.view === view),
        onclick: () => {
          state.view = view;
          state.selected.clear();
          scheduleRender();
        },
      },
      extra,
      h('span', { class: 'label', text: label }),
      h('span', { class: 'count', text: String(count(view)) }),
    );

  const nav = $('#sidebar');
  nav.replaceChildren(
    ...[
    h('h3', { text: 'Downloads' }),
    item('all', 'All downloads'),
    item('unfinished', 'Unfinished'),
    item('finished', 'Finished'),
    h('h3', { text: 'Categories' }),
    (state.settings?.categories ?? []).map((c) => item(`cat:${c.id}`, c.name, categoryIcon(c.id))),
    h('h3', { text: 'Queues' }),
    state.queues.map((q) =>
      item(
        `queue:${q.id}`,
        q.name,
        h('span', { class: `dot${q.running ? ' on' : ''}`, title: q.running ? 'Running' : 'Stopped' }),
      ),
    ),
    ].flat(),
  );
}

const rowEls = new Map(); // id -> <tr>

function createRow(id) {
  const tr = h('tr', { 'data-id': String(id) });
  tr.append(
    h('td', {}, h('div', { class: 'name-cell' }, h('span', { class: 'icon-slot' }), h('span', { class: 'name' }))),
    h('td', { class: 'num size' }),
    h('td', {}, h('div', { class: 'progress-cell' }, h('canvas'), h('span', { class: 'pct' }))),
    h('td', { class: 'num speed' }),
    h('td', { class: 'status' }),
    h('td', { class: 'added' }),
  );
  return tr;
}

function setText(el, text) {
  if (el.textContent !== text) el.textContent = text;
}

function updateRow(tr, r) {
  const nameCell = $('.name-cell', tr);
  if (tr.dataset.cat !== String(r.categoryId)) {
    tr.dataset.cat = String(r.categoryId);
    $('.icon-slot', nameCell).replaceChildren(categoryIcon(r.categoryId));
  }
  setText($('.name', nameCell), r.fileName);
  nameCell.title = r.url;
  setText($('.size', tr), r.size != null ? fmtBytes(r.size) : r.downloaded ? fmtBytes(r.downloaded) : '—');
  setText($('.pct', tr), percent(r));
  setText($('.speed', tr), r.status === 'downloading' ? fmtSpeed(state.live.get(r.id)?.speed) : '');
  const status = $('.status', tr);
  setText(status, statusText(r));
  status.className = `status status-${r.status}`;
  status.title = r.error ?? '';
  setText($('.added', tr), fmtDate(r.addedAt));
  tr.setAttribute('aria-selected', String(state.selected.has(r.id)));
  drawProgress($('canvas', tr), r);
}

function renderTable() {
  const list = visibleRecords();
  const tbody = $('#rows');
  const visible = new Set(list.map((r) => r.id));
  for (const [id, tr] of rowEls) {
    if (!visible.has(id)) {
      tr.remove();
      rowEls.delete(id);
    }
  }
  let prev = null;
  for (const r of list) {
    let tr = rowEls.get(r.id);
    if (!tr) {
      tr = createRow(r.id);
      rowEls.set(r.id, tr);
    }
    const expected = prev ? prev.nextSibling : tbody.firstChild;
    if (tr !== expected) tbody.insertBefore(tr, expected);
    updateRow(tr, r);
    prev = tr;
  }
  $('#empty').hidden = list.length > 0;
}

function renderToolbar() {
  const sel = selectedRecords();
  $('#btn-start').disabled = !sel.some((r) => r.status !== 'completed' && r.status !== 'downloading');
  $('#btn-stop').disabled = !sel.some((r) => r.status === 'downloading' || r.status === 'queued');
  $('#btn-remove').disabled = sel.length === 0;
  const q = currentQueue();
  const btn = $('#btn-queue');
  btn.disabled = !q;
  if (q) {
    const name = q.id === MAIN_QUEUE ? 'queue' : `“${q.name}”`;
    setText($('#btn-queue-label'), q.running ? `Stop ${name}` : `Start ${name}`);
    btn.title = `${q.running ? 'Stop' : 'Start'} the ${q.name} queue (${q.waiting} waiting, runs ${q.maxConcurrent} at a time)`;
  }
}

let details = { id: null, refs: null };

function renderDetails() {
  const sel = selectedRecords();
  const el = $('#details');
  if (sel.length !== 1) {
    el.hidden = true;
    details = { id: null, refs: null };
    return;
  }
  const r = sel[0];
  el.hidden = false;
  const refs = {
    downloaded: h('dd'),
    connections: h('dd'),
    canvas: h('canvas'),
  };
  const row = (label, value) => [h('dt', { text: label }), value instanceof Node ? value : h('dd', { text: value, title: value })];
  el.replaceChildren(
    h('h4', { text: r.fileName, title: r.fileName }),
    h(
      'div',
      { class: 'buttons' },
      r.status === 'completed' ? h('button', { text: 'Open', onclick: () => call('open_file', { id: r.id }) }) : null,
      h('button', { text: 'Show in folder', onclick: () => call('show_in_folder', { id: r.id }) }),
      h('button', { text: 'Copy URL', onclick: () => copyText(r.url) }),
    ),
    refs.canvas,
    h(
      'dl',
      {},
      row('URL', r.url),
      row('Saved to', `${r.directory}`),
      row('Size', r.size != null ? `${fmtBytes(r.size)} (${r.size.toLocaleString()} bytes)` : 'Unknown'),
      row('Downloaded', refs.downloaded),
      row('Connections', refs.connections),
      r.stream ? row('Stream', [r.stream.label ?? 'Best quality', r.stream.format !== 'auto' ? r.stream.format.toUpperCase() : null].filter(Boolean).join(' · ')) : null,
      row('Resumable', r.resumable == null ? 'Not known yet' : r.resumable ? 'Yes' : 'No — restarts from zero if interrupted'),
      row('Queue', queue(r.queueId)?.name ?? '—'),
      r.checksum ? [h('dt', { text: 'Checksum' }), checksumCell(r)] : null,
      r.scan ? row('Scan', r.scan) : null,
      r.error ? [h('dt', { text: 'Error' }), h('dd', { class: 'err', text: r.error })] : null,
    ),
  );
  if (r.status === 'completed' && !r.checksum) {
    $('.buttons', el).append(
      h('button', {
        text: 'SHA-256',
        title: 'Compute the SHA-256 checksum of the file',
        onclick: async (e) => {
          e.target.disabled = true;
          try {
            const hex = await backend.invoke('compute_checksum', { id: r.id, algorithm: 'sha-256' });
            toast(`SHA-256: ${hex}`);
            copyText(hex);
          } catch (err) {
            toast(errorText(err), true);
          } finally {
            e.target.disabled = false;
          }
        },
      }),
    );
  }
  details = { id: r.id, refs };
  updateDetailsLive();
}

function checksumCell(r) {
  const [state, cls] =
    r.checksumOk === true ? ['✓ verified', 'checksum-ok'] : r.checksumOk === false ? ['✗ mismatch', 'checksum-bad'] : r.status === 'completed' ? ['verifying…', ''] : ['checked when done', ''];
  return h('dd', { title: r.checksum }, h('span', { class: cls, text: state }), ` ${r.checksum}`);
}

function updateDetailsLive() {
  const r = state.records.get(details.id);
  if (!r || !details.refs) return;
  const live = state.live.get(r.id);
  setText(details.refs.downloaded, `${fmtBytes(r.downloaded)}${r.size ? ` of ${fmtBytes(r.size)}` : ''}`);
  setText(
    details.refs.connections,
    !live ? '—' : live.stream ? `${live.stream[0]} of ${live.stream[1]} segments · ${fmtSpeed(live.speed) || '—'}` : `${live.connections} active · ${fmtSpeed(live.speed) || '—'}`,
  );
  drawProgress(details.refs.canvas, r);
}

function renderStatus() {
  const active = [...state.records.values()].filter((r) => r.status === 'downloading').length;
  const speed = [...state.live.values()].reduce((sum, p) => sum + p.speed, 0);
  setText($('#status-active'), active ? `${active} downloading` : 'Idle');
  setText($('#status-speed'), speed ? `Total ${fmtSpeed(speed)}` : '');
}

/** Cheap refresh for progress ticks: only rows of running downloads. */
function renderProgress() {
  for (const id of state.live.keys()) {
    const tr = rowEls.get(id);
    const r = state.records.get(id);
    if (tr && r) updateRow(tr, r);
  }
  if (state.live.has(details.id)) updateDetailsLive();
  renderStatus();
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast('Copied');
  } catch {
    toast('Could not copy to the clipboard', true);
  }
}

// ---------------------------------------------------------------------------
// Selection, keyboard, context menu
// ---------------------------------------------------------------------------

function rowId(target) {
  const tr = target.closest('tr[data-id]');
  return tr ? Number(tr.dataset.id) : null;
}

function selectFromClick(id, e) {
  const list = visibleRecords().map((r) => r.id);
  if (e.shiftKey && state.anchor != null && list.includes(state.anchor)) {
    const [a, b] = [list.indexOf(state.anchor), list.indexOf(id)].sort((x, y) => x - y);
    if (!(e.metaKey || e.ctrlKey)) state.selected.clear();
    for (const i of list.slice(a, b + 1)) state.selected.add(i);
  } else if (e.metaKey || e.ctrlKey) {
    if (state.selected.has(id)) state.selected.delete(id);
    else state.selected.add(id);
    state.anchor = id;
  } else {
    state.selected = new Set([id]);
    state.anchor = id;
  }
  scheduleRender();
}

$('#rows').addEventListener('click', (e) => {
  const id = rowId(e.target);
  if (id != null) selectFromClick(id, e);
});
$('#rows').addEventListener('dblclick', (e) => {
  const r = state.records.get(rowId(e.target));
  if (!r) return;
  if (r.status === 'completed') call('open_file', { id: r.id });
  else if (r.status !== 'downloading') call('start_download', { id: r.id });
});
$('#table-wrap').addEventListener('click', (e) => {
  if (e.target === $('#table-wrap') || e.target.closest('.empty')) {
    state.selected.clear();
    scheduleRender();
  }
});

const isMod = (e) => e.metaKey || e.ctrlKey;
document.addEventListener('keydown', (e) => {
  if (document.querySelector('dialog[open]')) return;
  const typing = e.target.matches('input, textarea, select');
  if (isMod(e) && e.key.toLowerCase() === 'n') {
    e.preventDefault();
    openAdd();
  } else if (typing) {
    return;
  } else if (isMod(e) && e.key.toLowerCase() === 'a') {
    e.preventDefault();
    state.selected = new Set(visibleRecords().map((r) => r.id));
    scheduleRender();
  } else if (e.key === 'Delete' || (e.key === 'Backspace' && isMod(e))) {
    if (state.selected.size) openRemove();
  } else if (e.key === 'Enter') {
    const r = selectedRecords()[0];
    if (r?.status === 'completed') call('open_file', { id: r.id });
  } else if (e.key === 'Escape') {
    hideContext();
    state.selected.clear();
    scheduleRender();
  }
});

function hideContext() {
  $('#context').hidden = true;
}

$('#rows').addEventListener('contextmenu', (e) => {
  const id = rowId(e.target);
  if (id == null) return;
  e.preventDefault();
  if (!state.selected.has(id)) {
    state.selected = new Set([id]);
    state.anchor = id;
    scheduleRender();
  }
  const sel = selectedRecords();
  const one = sel.length === 1 ? sel[0] : null;
  const entry = (label, enabled, action) =>
    h('li', {}, h('button', { text: label, disabled: !enabled, onclick: () => { hideContext(); action(); } }));
  const menu = $('#context');
  menu.replaceChildren(
    entry('Open', one?.status === 'completed', () => call('open_file', { id: one.id })),
    entry('Show in folder', !!one, () => call('show_in_folder', { id: one.id })),
    h('hr'),
    entry('Resume', sel.some((r) => r.status !== 'completed' && r.status !== 'downloading'), startSelected),
    entry('Stop', sel.some((r) => r.status === 'downloading' || r.status === 'queued'), stopSelected),
    entry('Add to queue', sel.some((r) => r.status === 'stopped' || r.status === 'failed'), () =>
      sel.forEach((r) => call('enqueue_download', { id: r.id })),
    ),
    h('hr'),
    entry('Copy URL', !!one, () => copyText(one.url)),
    entry(sel.length > 1 ? `Remove ${sel.length} downloads…` : 'Remove…', true, openRemove),
  );
  menu.hidden = false;
  const { innerWidth: vw, innerHeight: vh } = window;
  const rect = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(e.clientX, vw - rect.width - 8)}px`;
  menu.style.top = `${Math.min(e.clientY, vh - rect.height - 8)}px`;
});
document.addEventListener('click', (e) => {
  if (!e.target.closest('#context')) hideContext();
});
window.addEventListener('blur', hideContext);
$('#table-wrap').addEventListener('scroll', hideContext);

// ---------------------------------------------------------------------------
// Toolbar actions
// ---------------------------------------------------------------------------

function startSelected() {
  for (const r of selectedRecords()) {
    if (r.status !== 'completed' && r.status !== 'downloading') call('start_download', { id: r.id });
  }
}
function stopSelected() {
  for (const r of selectedRecords()) {
    if (r.status === 'downloading' || r.status === 'queued') call('stop_download', { id: r.id });
  }
}

$('#btn-add').addEventListener('click', () => openAdd());
$('#btn-add-empty').addEventListener('click', () => openAdd());
$('#btn-start').addEventListener('click', startSelected);
$('#btn-stop').addEventListener('click', stopSelected);
$('#btn-remove').addEventListener('click', openRemove);
$('#btn-settings').addEventListener('click', openSettings);
$('#btn-queue').addEventListener('click', () => {
  const q = currentQueue();
  if (q) call(q.running ? 'stop_queue' : 'start_queue', { id: q.id });
});
$('#search').addEventListener('input', (e) => {
  state.search = e.target.value;
  scheduleRender();
});

// ---------------------------------------------------------------------------
// Remove dialog
// ---------------------------------------------------------------------------

function openRemove() {
  const sel = selectedRecords();
  if (!sel.length) return;
  const dlg = $('#dlg-remove');
  $('#remove-text').textContent =
    sel.length === 1 ? `Remove “${sel[0].fileName}” from the list?` : `Remove ${sel.length} downloads from the list?`;
  $('#remove-files').checked = false;
  dlg.returnValue = '';
  dlg.showModal();
  dlg.addEventListener(
    'close',
    () => {
      if (dlg.returnValue !== 'ok') return;
      const deleteFile = $('#remove-files').checked;
      for (const r of sel) {
        call('remove_download', { id: r.id, deleteFile });
        state.selected.delete(r.id);
      }
    },
    { once: true },
  );
}

// ---------------------------------------------------------------------------
// Add dialog
// ---------------------------------------------------------------------------

const add = { probeToken: 0, dirTouched: false, nameTouched: false, stream: null, title: null };

function fillSelect(select, items, value) {
  select.replaceChildren(...items.map(([v, label]) => h('option', { value: String(v), text: label })));
  select.value = String(value);
}

/**
 * Opens the Add dialog, optionally prefilled from the browser extension or
 * the clipboard: `{ url, fileName, referer, pageUrl, userAgent, cookies }`.
 */
function openAdd(prefill = {}) {
  const s = state.settings;
  const url = prefill.url ?? '';
  add.dirTouched = false;
  add.nameTouched = !!prefill.fileName;
  add.title = prefill.title ?? null;
  showStreamOptions(null);
  add.probeToken++;
  $('#form-add').reset();
  $('#add-url').value = url;
  $('#add-name').value = prefill.fileName ?? '';
  $('#add-referer').value = prefill.referer ?? prefill.pageUrl ?? '';
  $('#add-ua').value = prefill.userAgent ?? '';
  $('#add-cookies').value = prefill.cookies ?? '';
  $('#add-checksum').value = '';
  showDuplicate(null);
  $('#add-error').textContent = '';
  setProbeInfo('');
  fillSelect($('#add-category'), [['', 'Automatic (by extension)'], ...s.categories.map((c) => [c.id, c.name])], '');
  fillSelect($('#add-queue'), state.queues.map((q) => [q.id, q.name]), currentQueue()?.id ?? MAIN_QUEUE);
  $('#add-dir').value = s.defaultDirectory;
  $('#dlg-add').showModal();
  $('#add-url').focus();
  if (url) probe();
}

function setProbeInfo(text, ok = false) {
  const el = $('#add-info');
  el.textContent = text;
  el.classList.toggle('ok', ok);
}

/** Warns that the download is already in the list. */
function showDuplicate(record) {
  $('#add-duplicate').hidden = !record;
  $('#add-allow-duplicate').checked = false;
  if (record) {
    const status = { completed: 'finished', downloading: 'downloading', queued: 'queued', stopped: 'stopped', failed: 'failed' }[record.status] ?? record.status;
    $('#add-duplicate-text').textContent = `Already in your list as “${record.fileName}” (${status}).`;
  }
}

/** Shows the quality/audio/format pickers for a stream manifest (or hides them). */
function showStreamOptions(manifest) {
  add.stream = manifest;
  $('#add-stream').hidden = !manifest;
  if (!manifest) return;
  fillSelect($('#add-variant'), manifest.variants.map((v) => [v.id, v.label]), manifest.variants[0]?.id ?? '');
  const audio = manifest.audio;
  $('#add-audio').disabled = audio.length === 0;
  fillSelect(
    $('#add-audio'),
    audio.length ? audio.map((a) => [a.id, a.label]) : [['', 'Included in video']],
    (audio.find((a) => a.default) ?? audio[0])?.id ?? '',
  );
}

$('#add-format').addEventListener('change', () => {
  // Keep the file name's extension in step with the format.
  const ext = { auto: 'mp4', mp4: 'mp4', mkv: 'mkv', m4a: 'm4a', mp3: 'mp3' }[$('#add-format').value];
  const name = $('#add-name');
  if (add.stream && name.value) name.value = name.value.replace(/\.[^.]+$/, '') + `.${ext}`;
});

function numberOrNull(input, scale = 1) {
  const v = input.value.trim();
  return v === '' ? null : Math.round(Number(v) * scale);
}

function addRequest(start) {
  const text = (id) => $(id).value.trim() || null;
  const cat = $('#add-category').value;
  return {
    url: $('#add-url').value.trim(),
    fileName: text('#add-name'),
    directory: text('#add-dir'),
    categoryId: cat === '' ? null : Number(cat),
    queueId: Number($('#add-queue').value),
    referer: text('#add-referer'),
    userAgent: text('#add-ua'),
    cookies: text('#add-cookies'),
    connections: numberOrNull($('#add-conns')),
    speedLimit: numberOrNull($('#add-limit'), 1024) || null,
    title: add.title,
    checksum: text('#add-checksum'),
    allowDuplicate: $('#add-allow-duplicate').checked,
    stream: add.stream
      ? {
          variant: $('#add-variant').value || null,
          audio: $('#add-audio').value || null,
          format: $('#add-format').value,
          label: $('#add-variant').selectedOptions[0]?.textContent ?? null,
        }
      : null,
    start,
  };
}

async function probe() {
  const url = $('#add-url').value.trim();
  if (!url) return;
  const token = ++add.probeToken;
  setProbeInfo('Checking…');
  $('#add-error').textContent = '';
  try {
    const request = addRequest('now');
    if (!add.nameTouched) request.fileName = null;
    if (!add.dirTouched) request.directory = null;
    const p = await backend.invoke('probe_url', { request });
    if (token !== add.probeToken) return;
    if (!add.nameTouched) $('#add-name').value = p.fileName;
    if ($('#add-category').value === '' && p.categoryId != null) {
      // Show which category was picked, but keep "automatic" semantics.
      $('#add-category').value = String(p.categoryId);
    }
    if (!add.dirTouched) $('#add-dir').value = p.directory;
    showStreamOptions(p.stream ?? null);
    showDuplicate(p.duplicate ?? null);
    if (p.stream) {
      const s = p.stream;
      const parts = [`${s.kind.toUpperCase()} stream`, `${s.variants.length} ${s.variants.length === 1 ? 'quality' : 'qualities'}`];
      if (s.duration) parts.push(fmtEta(Math.round(s.duration)));
      if (s.audio.length) parts.push(`${s.audio.length} audio tracks`);
      setProbeInfo(parts.join(' · '), true);
      return;
    }
    const parts = [p.size != null ? fmtBytes(p.size) : 'Size unknown'];
    if (p.contentType) parts.push(p.contentType.split(';')[0]);
    parts.push(p.resumable ? 'resumable, multi-connection' : 'not resumable (one connection)');
    if (p.serverChecksum) parts.push('checksum published by the server');
    setProbeInfo(parts.join(' · '), true);
  } catch (e) {
    if (token !== add.probeToken) return;
    setProbeInfo('');
    showStreamOptions(null);
    $('#add-error').textContent = errorText(e);
  }
}

$('#add-url').addEventListener('change', probe);
$('#add-url').addEventListener('paste', () => setTimeout(probe, 0));
$('#add-check').addEventListener('click', probe);
$('#add-name').addEventListener('input', () => (add.nameTouched = true));
$('#add-dir').addEventListener('input', () => (add.dirTouched = true));
$('#add-category').addEventListener('change', () => {
  const c = category(Number($('#add-category').value));
  if (!add.dirTouched) $('#add-dir').value = c ? c.directory : state.settings.defaultDirectory;
});
$('#add-browse').addEventListener('click', async () => {
  const dir = await call('pick_folder', { start: $('#add-dir').value || null });
  if (dir) {
    $('#add-dir').value = dir;
    add.dirTouched = true;
  }
});
$('#add-cancel').addEventListener('click', () => $('#dlg-add').close());
// Show the next waiting browser/clipboard request once this one is done.
$('#dlg-add').addEventListener('close', () => setTimeout(showNextPending, 0));

const pendingAdds = [];
async function drainPending() {
  try {
    pendingAdds.push(...(await backend.invoke('take_pending_adds')));
  } catch {
    return;
  }
  showNextPending();
}
function showNextPending() {
  if ($('#dlg-add').open || !pendingAdds.length || !state.settings) return;
  openAdd(pendingAdds.shift());
}

async function submitAdd(start) {
  const request = addRequest(start);
  if (!request.url) {
    $('#add-error').textContent = 'Enter a URL.';
    return;
  }
  for (const b of $$('#dlg-add .actions button')) b.disabled = true;
  $('#add-error').textContent = '';
  try {
    const record = await backend.invoke('add_download', { request });
    state.records.set(record.id, record);
    $('#dlg-add').close();
    state.selected = new Set([record.id]);
    state.anchor = record.id;
    if (!matchesView(record)) state.view = 'all';
    scheduleRender();
  } catch (e) {
    const message = errorText(e);
    if (message.startsWith('already in the list')) {
      $('#add-duplicate').hidden = false;
      $('#add-duplicate-text').textContent = `${message[0].toUpperCase()}${message.slice(1)}.`;
    } else {
      $('#add-error').textContent = message;
    }
  } finally {
    for (const b of $$('#dlg-add .actions button')) b.disabled = false;
  }
}

$('#form-add').addEventListener('submit', (e) => {
  e.preventDefault();
  submitAdd('now');
});
$('#add-queue-btn').addEventListener('click', () => submitAdd('queue'));
$('#add-paused').addEventListener('click', () => submitAdd('paused'));

// ---------------------------------------------------------------------------
// Settings dialog
// ---------------------------------------------------------------------------

let draft = null;

function showTab(name) {
  for (const b of $$('#dlg-settings .tabs button')) b.setAttribute('aria-selected', String(b.dataset.tab === name));
  for (const s of $$('#dlg-settings .tab')) s.hidden = s.dataset.tab !== name;
}
for (const b of $$('#dlg-settings .tabs button')) b.addEventListener('click', () => showTab(b.dataset.tab));

async function browseInto(input) {
  const dir = await call('pick_folder', { start: input.value || null });
  if (dir) {
    input.value = dir;
    input.dispatchEvent(new Event('input'));
  }
}
for (const b of $$('[data-browse]')) b.addEventListener('click', () => browseInto($(`#${b.dataset.browse}`)));

function renderCategoryEditor() {
  const box = $('#set-categories');
  box.className = 'list-editor categories';
  box.replaceChildren(
    ...draft.categories.map((c, i) => {
      const dir = h('input', { type: 'text', value: c.directory, spellcheck: 'false', oninput: (e) => (c.directory = e.target.value) });
      return h(
        'div',
        { class: 'item' },
        h('input', { type: 'text', value: c.name, 'aria-label': 'Name', oninput: (e) => (c.name = e.target.value) }),
        h('input', {
          type: 'text',
          value: c.extensions.join(' '),
          placeholder: 'All other files',
          'aria-label': 'Extensions',
          spellcheck: 'false',
          oninput: (e) => (c.extensions = e.target.value.split(/[\s,]+/).filter(Boolean)),
        }),
        dir,
        h('button', { type: 'button', text: 'Browse…', onclick: () => browseInto(dir) }),
        h('button', {
          type: 'button',
          title: 'Remove category',
          'aria-label': `Remove ${c.name}`,
          text: '✕',
          onclick: () => {
            draft.categories.splice(i, 1);
            renderCategoryEditor();
          },
        }),
      );
    }),
  );
}

const WEEKDAYS = ['M', 'T', 'W', 'T', 'F', 'S', 'S'];

function renderQueueEditor() {
  const box = $('#set-queues');
  box.className = 'list-editor';
  box.replaceChildren(
    ...draft.queues.map((q, i) => {
      q.schedule ??= { enabled: false, start: null, stop: null, days: [], once: false };
      q.whenDone ??= 'nothing';
      const s = q.schedule;
      const time = (key, label) =>
        h('label', {}, `${label} `, h('input', { type: 'time', value: s[key] ?? '', 'aria-label': `${q.name} ${label}`, oninput: (e) => (s[key] = e.target.value || null) }));
      return h(
        'div',
        { class: 'queue-block' },
        h(
          'div',
          { class: 'item', style: 'display:grid;grid-template-columns:1fr 150px auto;gap:6px;align-items:center' },
          h('input', { type: 'text', value: q.name, 'aria-label': 'Queue name', oninput: (e) => (q.name = e.target.value) }),
          h('label', { class: 'check' }, 'Runs', h('input', { type: 'number', min: '1', max: '32', value: String(q.maxConcurrent), 'aria-label': 'Downloads at a time', oninput: (e) => (q.maxConcurrent = Number(e.target.value)) }), 'at once'),
          h('button', {
            type: 'button',
            text: '✕',
            title: q.id === MAIN_QUEUE ? 'The main queue cannot be removed' : 'Remove queue',
            disabled: q.id === MAIN_QUEUE,
            onclick: () => {
              draft.queues.splice(i, 1);
              renderQueueEditor();
            },
          }),
        ),
        h(
          'div',
          { class: 'sched' },
          h('label', { class: 'check' }, h('input', { type: 'checkbox', checked: s.enabled, onchange: (e) => (s.enabled = e.target.checked) }), 'Schedule'),
          time('start', 'start'),
          time('stop', 'stop'),
          h(
            'span',
            { class: 'days', title: 'Days (none checked = every day)' },
            WEEKDAYS.map((d, k) =>
              h('label', {}, d, h('input', {
                type: 'checkbox',
                checked: s.days.includes(k + 1),
                'aria-label': ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday'][k],
                onchange: (e) => {
                  s.days = s.days.filter((x) => x !== k + 1);
                  if (e.target.checked) s.days.push(k + 1);
                },
              })),
            ),
          ),
          h('label', { class: 'check' }, h('input', { type: 'checkbox', checked: s.once, onchange: (e) => (s.once = e.target.checked) }), 'once'),
          h(
            'label',
            {},
            'When done ',
            (() => {
              const sel = h('select', { 'aria-label': `${q.name}: when done`, onchange: (e) => (q.whenDone = e.target.value) },
                h('option', { value: 'nothing', text: 'do nothing' }),
                h('option', { value: 'quit', text: 'quit rdm' }),
                h('option', { value: 'shutdown', text: 'shut down the computer' }),
              );
              sel.value = q.whenDone;
              return sel;
            })(),
          ),
        ),
      );
    }),
  );
}

$('#set-add-category').addEventListener('click', () => {
  const id = Math.max(0, ...draft.categories.map((c) => c.id)) + 1;
  draft.categories.push({ id, name: 'New category', extensions: [], directory: `${draft.defaultDirectory}` });
  renderCategoryEditor();
});
$('#set-add-queue').addEventListener('click', () => {
  const id = Math.max(0, ...draft.queues.map((q) => q.id)) + 1;
  draft.queues.push({ id, name: `Queue ${id}`, maxConcurrent: 1, schedule: { enabled: false, start: null, stop: null, days: [], once: false }, whenDone: 'nothing' });
  renderQueueEditor();
});

function openSettings() {
  draft = structuredClone(state.settings);
  $('#set-dir').value = draft.defaultDirectory;
  $('#set-tray').checked = draft.closeToTray;
  $('#set-av').value = draft.antivirus ?? '';
  $('#set-clipboard').checked = draft.clipboardMonitoring;
  refreshFfmpeg();
  $('#set-browsers-error').textContent = '';
  refreshBrowsers();
  $('#set-conns').value = draft.connections;
  $('#set-split').value = Math.round(draft.minSplit / 1024);
  $('#set-limit').value = draft.globalSpeedLimit ? Math.round(draft.globalSpeedLimit / 1024) : 0;
  for (const r of $$('input[name=proxy]')) r.checked = r.value === draft.proxy.mode;
  $('#set-proxy-url').value = draft.proxy.url ?? '';
  $('#set-error').textContent = '';
  renderCategoryEditor();
  renderQueueEditor();
  showTab('general');
  $('#dlg-settings').showModal();
}

$('#set-cancel').addEventListener('click', () => $('#dlg-settings').close());
$('#set-av-preset').addEventListener('change', (e) => {
  if (e.target.value) $('#set-av').value = e.target.value;
  e.target.value = '';
});

function renderBrowsers(list) {
  $('#set-browsers').replaceChildren(
    ...list.map((b) => {
      const state = b.installed
        ? b.current
          ? ['Registered', true]
          : ['Registered to another copy of rdm', false]
        : [b.detected ? 'Not registered' : 'Not registered (browser not found)', false];
      return h(
        'div',
        { class: 'item' },
        h('strong', { text: b.name }),
        h('span', { class: `state${state[1] ? ' ok' : ''}`, text: state[0], title: b.manifestPath }),
        b.installed && b.current
          ? h('button', { type: 'button', text: 'Remove', onclick: () => browserAction('uninstall_browser_integration', b.browser) })
          : h('button', { type: 'button', text: 'Register', onclick: () => browserAction('install_browser_integration', b.browser) }),
      );
    }),
  );
}
async function refreshBrowsers() {
  try {
    renderBrowsers(await backend.invoke('browser_integration_status'));
  } catch (e) {
    $('#set-browsers').replaceChildren();
    $('#set-browsers-error').textContent = errorText(e);
  }
}
async function browserAction(cmd, browser) {
  $('#set-browsers-error').textContent = '';
  try {
    renderBrowsers(await backend.invoke(cmd, { browser }));
  } catch (e) {
    $('#set-browsers-error').textContent = errorText(e);
  }
}
$('#set-browsers-install').addEventListener('click', () => browserAction('install_browser_integration', null));

let ffmpegInfo = null;
function renderFfmpeg(s) {
  ffmpegInfo = s;
  const status = $('#ffmpeg-status');
  status.textContent = s.path ? `Found: ${s.version ?? 'ffmpeg'} (${s.path})` : 'Not found.';
  status.className = s.path ? 'ok' : '';
  const btn = $('#ffmpeg-install');
  btn.hidden = !!s.path || !s.download;
  if (s.download) btn.textContent = `Download ffmpeg (~${s.download.sizeMb} MB)`;
  $('#ffmpeg-hint').textContent = s.path ? '' : s.hint;
}
async function refreshFfmpeg() {
  $('#ffmpeg-error').textContent = '';
  try {
    renderFfmpeg(await backend.invoke('ffmpeg_status'));
  } catch (e) {
    $('#ffmpeg-error').textContent = errorText(e);
  }
}
$('#ffmpeg-install').addEventListener('click', async () => {
  const d = ffmpegInfo?.download;
  if (!d) return;
  const ok = confirm(
    `Download ffmpeg?\n\nA prebuilt ffmpeg (${d.license}, about ${d.sizeMb} MB) will be downloaded from ${d.publisher}, ` +
      `checked against its published SHA-256 checksum, and kept in rdm's own folder. ffmpeg is a separate program under its own license.`,
  );
  if (!ok) return;
  const btn = $('#ffmpeg-install');
  btn.disabled = true;
  btn.textContent = 'Downloading…';
  $('#ffmpeg-error').textContent = '';
  try {
    renderFfmpeg(await backend.invoke('install_ffmpeg'));
    toast('ffmpeg is ready.');
  } catch (e) {
    $('#ffmpeg-error').textContent = errorText(e);
    renderFfmpeg(ffmpegInfo);
  } finally {
    btn.disabled = false;
  }
});
$('#form-settings').addEventListener('submit', async (e) => {
  e.preventDefault();
  const mode = $('input[name=proxy]:checked')?.value ?? 'system';
  const limit = Number($('#set-limit').value) || 0;
  const settings = {
    ...draft,
    defaultDirectory: $('#set-dir').value.trim(),
    closeToTray: $('#set-tray').checked,
    antivirus: $('#set-av').value.trim() || null,
    clipboardMonitoring: $('#set-clipboard').checked,
    connections: Number($('#set-conns').value),
    minSplit: Number($('#set-split').value) * 1024,
    globalSpeedLimit: limit > 0 ? limit * 1024 : null,
    proxy: mode === 'custom' ? { mode, url: $('#set-proxy-url').value.trim() } : { mode },
  };
  try {
    state.settings = await backend.invoke('update_settings', { settings });
    $('#dlg-settings').close();
    scheduleRender();
  } catch (err) {
    $('#set-error').textContent = errorText(err);
  }
});

// ---------------------------------------------------------------------------
// More menu: import / export
// ---------------------------------------------------------------------------

function reportImport(r) {
  if (!r) return;
  const parts = [`Added ${r.added}`];
  if (r.duplicates) parts.push(`${r.duplicates} already in the list`);
  if (r.invalid.length) parts.push(`${r.invalid.length} skipped`);
  toast(parts.join(' · '), r.added === 0 && r.invalid.length > 0);
}

$('#btn-more').addEventListener('click', (e) => {
  e.stopPropagation();
  const sel = [...state.selected];
  const item = (label, action, enabled = true) =>
    h('li', {}, h('button', { text: label, disabled: !enabled, onclick: () => { hideContext(); action(); } }));
  const exportTo = async (format, ids = null) => {
    const path = await call('export_list', { format, ids });
    if (path) toast(`Exported to ${path}`);
  };
  const menu = $('#context');
  menu.replaceChildren(
    item('Import list…', async () => reportImport(await call('import_list'))),
    h('hr'),
    item('Export list (rdm JSON)…', () => exportTo('json')),
    item('Export URLs (text)…', () => exportTo('urls')),
    item(`Export ${sel.length || ''} selected…`.replace('  ', ' '), () => exportTo('json', sel), sel.length > 0),
  );
  menu.hidden = false;
  const r = e.currentTarget.getBoundingClientRect();
  const w = menu.getBoundingClientRect().width;
  menu.style.left = `${Math.max(8, r.right - w)}px`;
  menu.style.top = `${r.bottom + 4}px`;
});

// ---------------------------------------------------------------------------
// Shutdown countdown ("when the queue is done: shut down")
// ---------------------------------------------------------------------------

let countdownTimer = null;
function startCountdown(queueName) {
  const dlg = $('#dlg-countdown');
  let left = 60;
  const render = () => ($('#countdown-text').textContent = `Queue “${queueName}” finished. The computer will shut down in ${left} seconds.`);
  render();
  clearInterval(countdownTimer);
  dlg.returnValue = '';
  dlg.showModal();
  countdownTimer = setInterval(() => {
    left -= 1;
    render();
    if (left <= 0) dlg.close('now');
  }, 1000);
  dlg.addEventListener(
    'close',
    () => {
      clearInterval(countdownTimer);
      if (dlg.returnValue === 'now') call('shutdown_computer');
      else toast('Shutdown cancelled.');
    },
    { once: true },
  );
}

// ---------------------------------------------------------------------------
// Drag and drop: links from a browser, or a .txt / .json list
// ---------------------------------------------------------------------------

const URL_RE = /https?:\/\/[^\s"'<>]+/g;
let dragDepth = 0;
const overlay = $('#drop-overlay');
const acceptsDrop = (e) => [...(e.dataTransfer?.types ?? [])].some((t) => t === 'Files' || t === 'text/uri-list' || t === 'text/plain');

document.addEventListener('dragenter', (e) => {
  if (!acceptsDrop(e) || document.querySelector('dialog[open]')) return;
  e.preventDefault();
  dragDepth++;
  overlay.hidden = false;
});
document.addEventListener('dragover', (e) => {
  if (acceptsDrop(e)) e.preventDefault();
});
document.addEventListener('dragleave', () => {
  dragDepth = Math.max(0, dragDepth - 1);
  if (!dragDepth) overlay.hidden = true;
});
document.addEventListener('drop', async (e) => {
  e.preventDefault();
  dragDepth = 0;
  overlay.hidden = true;
  if (document.querySelector('dialog[open]')) return;
  const files = [...(e.dataTransfer?.files ?? [])].filter((f) => /\.(txt|json|lst|csv)$/i.test(f.name) && f.size < 5_000_000);
  if (files.length) {
    for (const f of files) reportImport(await call('import_text', { text: await f.text() }));
    return;
  }
  const text = e.dataTransfer.getData('text/uri-list') || e.dataTransfer.getData('text/plain') || '';
  const urls = [...new Set(text.match(URL_RE) ?? [])];
  if (urls.length === 1) openAdd({ url: urls[0] });
  else if (urls.length > 1) reportImport(await call('import_text', { text: urls.join('\n') }));
  else toast('Nothing to download in what was dropped.', true);
});

// ---------------------------------------------------------------------------
// Events and startup
// ---------------------------------------------------------------------------

function handleEvent({ type, data }) {
  switch (type) {
    case 'added':
    case 'updated':
      state.records.set(data.id, data);
      if (data.status !== 'downloading') state.live.delete(data.id);
      scheduleRender();
      break;
    case 'removed':
      state.records.delete(data);
      state.live.delete(data);
      state.selected.delete(data);
      scheduleRender();
      break;
    case 'progress':
      state.live = new Map(data.map((p) => [p.id, p]));
      for (const p of data) {
        const r = state.records.get(p.id);
        if (r) {
          r.downloaded = p.downloaded;
          if (p.total != null) r.size = p.total;
        }
      }
      renderProgress();
      break;
    case 'settings':
      state.settings = data;
      scheduleRender();
      break;
    case 'queues':
      state.queues = data;
      scheduleRender();
      break;
    case 'notice':
      toast(data.message);
      break;
    case 'queueFinished':
      if (data.whenDone === 'shutdown') startCountdown(data.name);
      else if (data.whenDone === 'nothing') toast(`Queue “${data.name}” finished.`);
      break;
  }
}

async function loadAll() {
  const [records, settings, queues, live] = await Promise.all([
    backend.invoke('list_downloads'),
    backend.invoke('get_settings'),
    backend.invoke('get_queues'),
    backend.invoke('live_progress'),
  ]);
  state.records = new Map(records.map((r) => [r.id, r]));
  state.settings = settings;
  state.queues = queues;
  state.live = new Map(live.map((p) => [p.id, p]));
  scheduleRender();
}

readPalette();
await backend.listen('rdm://event', handleEvent);
await backend.listen('rdm://resync', loadAll);
await backend.listen('rdm://add-request', drainPending);
await loadAll();
await drainPending();
new ResizeObserver(() => scheduleRender()).observe($('#table-wrap'));
