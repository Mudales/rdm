// Background script (MV3 service worker in Chrome/Edge, event page in
// Firefox). Watches responses for media, captures browser downloads (and,
// when enabled, files that would open in a tab), runs the right-click menu,
// and talks to the rdm app through the native messaging host.

import { DEFAULT_RULES, capturesSite, decide, extensionForMime, hostOf } from './lib/capture.js';
import {
  DEFAULT_SETTINGS,
  browserName,
  classifyRequest,
  classifyResponse,
  compareVersions,
  cookieHeader,
  describe,
  dispositionFileName,
  downloadCandidate,
  fileLinks,
  fileNameFromUrl,
  header,
  isSupportedKind,
  mediaKey,
  responseSize,
} from './lib/detect.js';

const api = globalThis.browser ?? globalThis.chrome;
const HOST = 'dev.rdm.host';
const MAX_ITEMS_PER_TAB = 100;
/** Alt+click on a link lets the browser download it (Chrome's own gesture). */
const BYPASS_MS = 10_000;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// ---------------------------------------------------------------------------
// Settings (this browser's switches)
// ---------------------------------------------------------------------------

async function getSettings() {
  const stored = await api.storage.local.get('settings');
  return { ...DEFAULT_SETTINGS, ...(stored.settings ?? {}) };
}

// ---------------------------------------------------------------------------
// Media detection, per tab. Mirrored to storage.session because MV3 service
// workers are stopped when idle.
// ---------------------------------------------------------------------------

/** tabId -> Map(url -> item) */
const tabMedia = new Map();
let restored = null;

function restore() {
  restored ??= api.storage.session
    .get('tabMedia')
    .then(({ tabMedia: saved }) => {
      for (const [tabId, items] of Object.entries(saved ?? {})) {
        if (!tabMedia.has(Number(tabId))) tabMedia.set(Number(tabId), new Map(items.map((i) => [i.url, i])));
      }
    })
    .catch(() => {});
  return restored;
}

let persistTimer = null;
function persist() {
  clearTimeout(persistTimer);
  persistTimer = setTimeout(() => {
    const plain = Object.fromEntries([...tabMedia].map(([id, m]) => [id, [...m.values()]]));
    api.storage.session.set({ tabMedia: plain }).catch(() => {});
  }, 300);
}

function updateBadge(tabId) {
  const count = tabMedia.get(tabId)?.size ?? 0;
  api.action.setBadgeText({ tabId, text: count ? String(count) : '' }).catch(() => {});
}
api.action.setBadgeBackgroundColor?.({ color: '#2563eb' });

function clearTab(tabId) {
  if (tabMedia.delete(tabId)) persist();
  updateBadge(tabId);
}

/**
 * Media requested this shortly before the page's address changes belongs to
 * the new page: a single-page site (a playlist) changes the address and
 * starts the next file at about the same time, in either order.
 */
const KEEP_ON_NAVIGATION_MS = 3000;

/** The tab moved to another address: forget the media found before it. */
function clearTabOnNavigation(tabId) {
  const items = tabMedia.get(tabId);
  if (!items) return updateBadge(tabId);
  const since = Date.now() - KEEP_ON_NAVIGATION_MS;
  for (const [url, item] of items) if (!(item.seenAt >= since)) items.delete(url);
  if (!items.size) tabMedia.delete(tabId);
  persist();
  updateBadge(tabId);
}

function changed(tabId) {
  updateBadge(tabId);
  persist();
  refreshMenusSoon();
}

/** Adds (or refreshes, as the most recent) a detected item on a tab. */
function addMedia(tabId, item) {
  restore().then(() => {
    let items = tabMedia.get(tabId);
    if (!items) tabMedia.set(tabId, (items = new Map()));
    const previous = items.get(item.url);
    // Another request for a file already answered adds nothing.
    if (item.pending && previous && !previous.pending) return;
    items.delete(item.url); // re-insert as most recent
    items.set(item.url, { ...previous, ...item, size: item.size ?? previous?.size ?? null, seenAt: Date.now() });
    while (items.size > MAX_ITEMS_PER_TAB) items.delete(items.keys().next().value);
    changed(tabId);
  });
}

/** A file listed when its request started turned out not to be media (or failed). */
function dropPending(tabId, url) {
  restore().then(() => {
    const items = tabMedia.get(tabId);
    if (!items?.get(url)?.pending) return;
    items.delete(url);
    changed(tabId);
  });
}

// As soon as a request for a media file starts: a page that keeps its
// earlier files open (a playlist) can leave the next one waiting for a
// connection, and its answer, for minutes.
api.webRequest.onBeforeRequest.addListener(
  (details) => {
    if (details.tabId < 0) return;
    const item = classifyRequest(details);
    if (item) addMedia(details.tabId, item);
  },
  { urls: ['http://*/*', 'https://*/*'] },
);

api.webRequest.onHeadersReceived.addListener(
  (details) => {
    if (details.tabId < 0) return;
    const item = classifyResponse(details);
    if (item) addMedia(details.tabId, { ...item, pending: false });
    else dropPending(details.tabId, mediaKey(details.url));
  },
  { urls: ['http://*/*', 'https://*/*'] },
  ['responseHeaders'],
);

api.webRequest.onErrorOccurred.addListener(
  (details) => {
    if (details.tabId >= 0) dropPending(details.tabId, mediaKey(details.url));
  },
  { urls: ['http://*/*', 'https://*/*'] },
);

api.tabs.onUpdated.addListener((tabId, info) => {
  // Another page (or another address in a single-page site) starts fresh.
  if (info.status === 'loading' && info.url) restore().then(() => clearTabOnNavigation(tabId));
  if (info.url || info.status === 'complete') refreshMenusSoon();
});
api.tabs.onRemoved.addListener((tabId) => {
  clearTab(tabId);
  tabFiles.delete(tabId);
});
api.tabs.onActivated.addListener(() => refreshMenusSoon());
api.windows?.onFocusChanged?.addListener(() => refreshMenusSoon());

async function mediaFor(tabId) {
  await restore();
  return [...(tabMedia.get(tabId)?.values() ?? [])].reverse();
}

// ---------------------------------------------------------------------------
// Talking to rdm
// ---------------------------------------------------------------------------

const NOT_INSTALLED =
  'rdm is not installed, or not set up for this browser. In rdm, open ⋯ → Set up browser and tools.';

async function native(message) {
  try {
    return await api.runtime.sendNativeMessage(HOST, message);
  } catch (e) {
    const text = String(e?.message ?? e);
    // Chrome: "Specified native messaging host not found."
    // Firefox: "No such native application dev.rdm.host"
    if (/not found|no such native application|access to the specified native messaging host is forbidden/i.test(text)) {
      return { type: 'error', message: NOT_INSTALLED, notInstalled: true };
    }
    return { type: 'error', message: text };
  }
}

/** An item with the browser context servers often require (cookies, User-Agent, Referer). */
async function enrich(item, pageUrl) {
  let cookies = '';
  try {
    cookies = cookieHeader(await api.cookies.getAll({ url: item.url }));
  } catch {
    /* no cookie access for this URL */
  }
  return {
    url: item.url,
    kind: item.kind || undefined,
    title: item.title || undefined,
    fileName: item.fileName || undefined,
    referer: item.referer || pageUrl || undefined,
    pageUrl: pageUrl || undefined,
    userAgent: navigator.userAgent,
    cookies: cookies || undefined,
    // A quality already chosen on the page.
    variant: item.variant || undefined,
    label: item.label || undefined,
    format: item.format || undefined,
  };
}

/** Sends items to rdm. */
async function sendToApp(items, mode, pageUrl) {
  const enriched = await Promise.all(items.map((item) => enrich(item, pageUrl)));
  return native({ type: 'add', mode, items: enriched });
}

// ---------------------------------------------------------------------------
// Introducing ourselves to the app, and updating from it. rdm keeps the
// folder this extension is loaded from up to date; when it says it ships a
// newer version, reloading picks the new files up.
// ---------------------------------------------------------------------------

const OWN_VERSION = api.runtime.getManifest().version;
const BROWSER = browserName(navigator, typeof globalThis.browser?.runtime?.getBrowserInfo === 'function');
const HELLO_EVERY_MS = 10 * 60_000;

async function hello({ force = false } = {}) {
  if (!force) {
    const { lastHello = 0 } = await api.storage.session.get('lastHello').catch(() => ({}));
    if (Date.now() - lastHello < HELLO_EVERY_MS) return null;
  }
  let reply = await native({ type: 'hello', extension: { version: OWN_VERSION, browser: BROWSER } });
  // An rdm older than this extension does not know "hello".
  if (reply?.type === 'error' && /bad message|unknown variant/i.test(reply.message)) reply = await native({ type: 'ping' });
  if (reply?.type === 'pong') {
    await api.storage.session.set({ lastHello: Date.now() }).catch(() => {});
    return { ...reply, update: await updateFrom(reply.extension) };
  }
  return reply;
}

/**
 * Reloads once to pick up a newer version rdm wrote to our folder. If the
 * version did not change (the extension was loaded from somewhere else),
 * it is not tried again for that version: the popup explains instead.
 * Returns what the popup should say: null, 'app' (the app is older) or
 * 'extension' (this extension is older and could not update itself).
 */
async function updateFrom(shipped) {
  if (!shipped) return null;
  const cmp = compareVersions(shipped, OWN_VERSION);
  if (cmp < 0) return 'app';
  if (cmp === 0) return null;
  const { reloadedFor } = await api.storage.local.get('reloadedFor').catch(() => ({}));
  if (reloadedFor === shipped) return 'extension';
  await api.storage.local.set({ reloadedFor: shipped }).catch(() => {});
  console.info(`rdm: updating the extension from ${OWN_VERSION} to ${shipped}`);
  api.runtime.reload();
  return null;
}

api.runtime.onInstalled.addListener(() => hello({ force: true }).catch(() => {}));
hello().catch(() => {});

/** Shows a failed menu action on the toolbar icon (menus have no UI). */
function flagError(tabId, reply) {
  if (reply?.type !== 'error' || tabId == null || tabId < 0) return;
  api.action.setBadgeText({ tabId, text: '!' }).catch(() => {});
  api.action.setTitle({ tabId, title: `rdm: ${reply.message}` }).catch(() => {});
  setTimeout(() => {
    updateBadge(tabId);
    api.action.setTitle({ tabId, title: 'rdm' }).catch(() => {});
  }, 8000);
}

// ---------------------------------------------------------------------------
// Capture rules: chosen in the app (Settings → Capture), cached here so a
// download is never held up when the app is not running.
// ---------------------------------------------------------------------------

/** Rules younger than this are used without asking the app again. */
const RULES_FRESH_MS = 30_000;
/** How long a download waits for the app's answer. */
const RULES_TIMEOUT_MS = 1500;
let rulesCache = null; // { rules, fetchedAt }

async function cachedRules() {
  if (!rulesCache) {
    const { captureRules } = await api.storage.local.get('captureRules').catch(() => ({}));
    rulesCache = captureRules?.rules ? captureRules : { rules: DEFAULT_RULES, fetchedAt: 0 };
  }
  return rulesCache;
}

async function storeRules(rules) {
  rulesCache = { rules: { ...DEFAULT_RULES, ...rules }, fetchedAt: Date.now() };
  await api.storage.local.set({ captureRules: rulesCache }).catch(() => {});
  refreshMenusSoon();
  return rulesCache.rules;
}

async function getRules({ maxAge = RULES_FRESH_MS } = {}) {
  const cached = await cachedRules();
  if (Date.now() - cached.fetchedAt < maxAge) return cached.rules;
  const reply = await Promise.race([native({ type: 'getCaptureRules' }), sleep(RULES_TIMEOUT_MS).then(() => null)]);
  if (reply?.type === 'captureRules') return storeRules(reply.rules);
  return cached.rules;
}

async function setSiteCapture(host, capture) {
  const reply = await native({ type: 'setSiteCapture', host, capture });
  if (reply?.type === 'captureRules') await storeRules(reply.rules);
  return reply;
}

// ---------------------------------------------------------------------------
// Capturing browser downloads
// ---------------------------------------------------------------------------

const bypass = new Map(); // url -> expiry
const reissued = new Set(); // downloads we handed back to the browser

function bypassed(url) {
  const until = bypass.get(url);
  if (until && until > Date.now()) return true;
  bypass.delete(url);
  return false;
}

// Safety nets against capture loops: the same address is taken at most once
// in a while, and a burst of downloads nobody could have clicked that fast
// pauses capturing (the browser keeps them).
const RECAPTURE_MS = 30_000;
const BURST = { count: 8, windowMs: 10_000, pauseMs: 60_000 };
const recentCaptures = new Map(); // url -> time taken
let captureTimes = [];
let pausedUntil = 0;

function mayCapture(url) {
  const now = Date.now();
  for (const [u, at] of recentCaptures) if (now - at > RECAPTURE_MS) recentCaptures.delete(u);
  if (now < pausedUntil || recentCaptures.has(url)) return false;
  captureTimes = captureTimes.filter((at) => now - at < BURST.windowMs);
  if (captureTimes.length >= BURST.count) {
    pausedUntil = now + BURST.pauseMs;
    console.warn('rdm: too many downloads at once; leaving them to the browser for a minute');
    return false;
  }
  captureTimes.push(now);
  recentCaptures.set(url, now);
  return true;
}

api.downloads.onCreated.addListener(async (item) => {
  const url = item.finalUrl || item.url;
  if (reissued.delete(url) || bypassed(url)) return;
  const candidate = downloadCandidate(item, await getSettings());
  if (!candidate) return;
  const { decision, extension } = decide(await getRules(), candidate);
  if (decision !== 'capture') {
    // Offered in rdm's settings as a type to add with one click.
    if (decision === 'notSelected' && extension) native({ type: 'reportSkipped', extension });
    return;
  }
  if (!mayCapture(url)) return;

  // Stop the browser's copy first; rdm takes it from here.
  await api.downloads.cancel(item.id).catch(() => {});
  await api.downloads.erase({ id: item.id }).catch(() => {});

  // Without the browser's name, rdm asks the server (Content-Disposition).
  const fileName = candidate.fileName || undefined;
  const reply = await sendToApp([{ url, fileName, referer: item.referrer }], 'ask', item.referrer);
  if (reply?.type === 'error') {
    // Never lose a download: give it back to the browser.
    reissued.add(url);
    api.downloads.download({ url }).catch(() => reissued.delete(url));
    console.warn('rdm: handing the download back to the browser:', reply.message);
  }
});

// ---------------------------------------------------------------------------
// Files that open in a tab (PDFs, images, videos). Remembered per tab for
// the popup and the menu; taken over only when the user enabled it in rdm.
// ---------------------------------------------------------------------------

/** tabId -> { url, fileName, mime, size, referer } of a tab that shows a file. */
const tabFiles = new Map();
const PAGE_TYPES = /^(text\/html|application\/xhtml\+xml|text\/xml|application\/xml|application\/json|text\/plain)$/;

api.webRequest.onHeadersReceived.addListener(
  (details) => {
    if (details.tabId < 0 || details.statusCode !== 200) return;
    const mime = (header(details.responseHeaders, 'content-type') ?? '').split(';')[0].trim().toLowerCase();
    const disposition = header(details.responseHeaders, 'content-disposition') ?? '';
    // Pages are not files; attachments become downloads (handled above).
    if (!mime || /^\s*attachment/i.test(disposition)) {
      tabFiles.delete(details.tabId);
      return;
    }
    const mimeExt = extensionForMime(mime);
    const fileName = dispositionFileName(disposition) || null;
    const file = {
      url: details.url,
      // The server's name, else one that carries the type (a PDF served
      // from /view.php is still a PDF).
      fileName: fileName ?? (mimeExt ? `${fileNameFromUrl(details.url).replace(/\.[^.]*$/, '') || 'file'}.${mimeExt}` : null),
      mime,
      size: responseSize(details.responseHeaders),
      referer: details.initiator ?? details.originUrl ?? undefined,
    };
    if (PAGE_TYPES.test(mime) && !fileName) {
      tabFiles.delete(details.tabId);
      return;
    }
    tabFiles.set(details.tabId, file);
    refreshMenusSoon();
    captureInline(details.tabId, file).catch((e) => console.warn('rdm:', e));
  },
  { urls: ['http://*/*', 'https://*/*'], types: ['main_frame'] },
  ['responseHeaders'],
);

async function captureInline(tabId, file) {
  if (bypassed(file.url) || !(await getSettings()).captureDownloads) return;
  const rules = await getRules();
  if (!rules.captureInline) return;
  if (decide(rules, file).decision !== 'capture') return;
  const reply = await sendToApp([{ url: file.url, fileName: file.fileName ?? undefined, referer: file.referer }], 'ask', file.referer);
  if (reply?.type === 'error') return; // it stays in the tab
  // Leave the file: back to the page the link was on, or close a tab that
  // was opened just for it.
  api.tabs.goBack(tabId).catch(() => api.tabs.remove(tabId).catch(() => {}));
}

// ---------------------------------------------------------------------------
// Right-click menu
// ---------------------------------------------------------------------------

const ALL_CONTEXTS = ['page', 'frame', 'link', 'selection', 'image', 'video', 'audio'];
const MAX_MENU_MEDIA = 8;
/** Media listed under "Detected media", in menu order. */
let menuMedia = [];

/** Calls a contextMenus method, ignoring failures (e.g. an item that is already gone). */
function menuCall(method, ...args) {
  try {
    if (method === 'create') {
      // create() reports errors through lastError only; read it.
      api.contextMenus.create(...args, () => void api.runtime.lastError);
    } else {
      Promise.resolve(api.contextMenus[method](...args)).catch(() => {});
    }
  } catch {
    /* not available here */
  }
}

function setupMenus() {
  menuCall('removeAll');
  const add = (props) => menuCall('create', { contexts: ALL_CONTEXTS, ...props });
  add({ id: 'rdm', title: 'rdm' });
  add({ id: 'rdm-link', parentId: 'rdm', title: 'Download link with rdm…', contexts: ['link'] });
  add({ id: 'rdm-link-queue', parentId: 'rdm', title: 'Add link to the queue', contexts: ['link'] });
  add({ id: 'rdm-selection', parentId: 'rdm', title: 'Download selected links…', contexts: ['selection'] });
  add({ id: 'rdm-media', parentId: 'rdm', title: 'Download this media…', contexts: ['video', 'audio'] });
  add({ id: 'rdm-image', parentId: 'rdm', title: 'Download image…', contexts: ['image'] });
  add({ id: 'rdm-page-file', parentId: 'rdm', title: 'Download this file…', contexts: ['page'], visible: false });
  add({ id: 'rdm-page-video', parentId: 'rdm', title: 'Find the video on this page (yt-dlp)…', contexts: ['page', 'frame', 'video', 'audio'] });
  add({ id: 'rdm-all-links', parentId: 'rdm', title: 'Download all links on this page…' });
  add({ id: 'rdm-streams', parentId: 'rdm', title: 'Detected media', visible: false });
  add({ id: 'rdm-sep', parentId: 'rdm', type: 'separator' });
  add({ id: 'rdm-site', parentId: 'rdm', type: 'checkbox', checked: true, title: 'Take downloads from this site' });
  add({ id: 'rdm-types', parentId: 'rdm', title: 'Choose file types…' });
  add({ id: 'rdm-open', parentId: 'rdm', title: 'Open rdm' });
  menuMedia = [];
  refreshMenusSoon();
}
api.runtime.onInstalled.addListener(setupMenus);
api.runtime.onStartup?.addListener(setupMenus);

let refreshTimer = null;
function refreshMenusSoon() {
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => refreshMenus().catch(() => {}), 150);
}

/** Updates the parts of the menu that depend on the active tab. */
async function refreshMenus() {
  const [tab] = await api.tabs.query({ active: true, lastFocusedWindow: true });
  if (!tab) return;
  const { rules } = await cachedRules();
  const host = hostOf(tab.url ?? '');
  menuCall('update', 'rdm-site', {
    title: host ? `Take downloads from ${host}` : 'Take downloads from this site',
    checked: !host || capturesSite(rules, host),
    enabled: !!host,
  });
  const file = tabFiles.get(tab.id);
  menuCall('update', 'rdm-page-file', { visible: !!file && file.url === tab.url });

  // All slots, not just the last list's: a restarted worker forgets it.
  for (let i = 0; i < MAX_MENU_MEDIA; i++) menuCall('remove', `rdm-stream-${i}`);
  menuMedia = (await mediaFor(tab.id)).filter((i) => isSupportedKind(i.kind)).slice(0, MAX_MENU_MEDIA);
  menuMedia.forEach((item, i) => {
    const name = item.fileName || fileNameFromUrl(item.url) || item.url;
    const title = `${describe(item)} — ${name}`;
    menuCall('create', {
      id: `rdm-stream-${i}`,
      parentId: 'rdm-streams',
      title: title.length > 80 ? `${title.slice(0, 79)}…` : title,
      contexts: ALL_CONTEXTS,
    });
  });
  menuCall('update', 'rdm-streams', {
    visible: menuMedia.length > 0,
    title: `Detected media (${menuMedia.length})`,
  });
}

/** Asks a frame's content script for its links. */
async function collectLinks(tabId, frameId, selectionOnly) {
  try {
    const reply = await api.tabs.sendMessage(tabId, { type: 'collectLinks', selectionOnly }, { frameId: frameId ?? 0 });
    return reply?.links ?? [];
  } catch {
    return []; // no content script here (e.g. a browser page)
  }
}

function mediaItem(item, tab) {
  const stream = item.kind === 'hls' || item.kind === 'dash';
  // Streams are named after the page; their URLs say "master.m3u8".
  return { url: item.url, kind: item.kind, title: tab?.title, fileName: stream ? undefined : item.fileName };
}

async function onMenu(info, tab) {
  const tabId = tab?.id;
  const pageUrl = info.frameUrl ?? info.pageUrl ?? tab?.url;
  const id = String(info.menuItemId);
  if (id.startsWith('rdm-stream-')) {
    const item = menuMedia[Number(id.slice('rdm-stream-'.length))];
    return item && sendToApp([mediaItem(item, tab)], 'ask', tab?.url);
  }
  switch (id) {
    case 'rdm-link':
      return sendToApp([{ url: info.linkUrl }], 'ask', pageUrl);
    case 'rdm-link-queue':
      return sendToApp([{ url: info.linkUrl }], 'queue', pageUrl);
    case 'rdm-image':
      return sendToApp([{ url: info.srcUrl }], 'ask', pageUrl);
    case 'rdm-media': {
      if (/^https?:/i.test(info.srcUrl ?? '')) {
        return sendToApp([{ url: info.srcUrl, kind: info.mediaType === 'audio' ? 'audio' : 'video', title: tab?.title }], 'ask', pageUrl);
      }
      // A blob: player plays a stream; use the one detected on this tab,
      // or let yt-dlp read the page.
      const best = (await mediaFor(tabId)).find((i) => isSupportedKind(i.kind));
      if (best) return sendToApp([mediaItem(best, tab)], 'ask', tab?.url);
      return sendToApp([{ url: tab?.url, kind: 'page', title: tab?.title }], 'ask', tab?.url);
    }
    case 'rdm-page-video':
      if (!/^https?:/i.test(info.pageUrl ?? tab?.url ?? '')) return undefined;
      return sendToApp([{ url: info.pageUrl ?? tab.url, kind: 'page', title: tab?.title }], 'ask', info.pageUrl ?? tab.url);
    case 'rdm-page-file': {
      const file = tabFiles.get(tabId);
      return file && sendToApp([{ url: file.url, fileName: file.fileName ?? undefined, referer: file.referer }], 'ask', file.referer);
    }
    case 'rdm-selection':
    case 'rdm-all-links': {
      const links = await collectLinks(tabId, info.frameId, id === 'rdm-selection');
      // A selection is deliberate: keep every link in it. "All links"
      // keeps the ones that point to files.
      const chosen = id === 'rdm-selection' ? links.filter((l) => /^https?:/i.test(l.url)) : fileLinks(links);
      const unique = [...new Map(chosen.map((l) => [l.url.split('#')[0], l])).values()];
      if (!unique.length) return { type: 'error', message: id === 'rdm-selection' ? 'No links in the selection.' : 'No links to files found on this page.' };
      return sendToApp(unique.map((l) => ({ url: l.url })), 'ask', pageUrl);
    }
    case 'rdm-site': {
      const host = hostOf(tab?.url ?? '');
      if (!host) return;
      const reply = await setSiteCapture(host, info.checked);
      if (reply?.type !== 'captureRules') refreshMenusSoon(); // put the checkmark back
      return reply;
    }
    case 'rdm-types':
      return native({ type: 'show', view: 'captureSettings' });
    case 'rdm-open':
      return native({ type: 'show' });
  }
  return undefined;
}

api.contextMenus.onClicked.addListener((info, tab) => {
  onMenu(info, tab).then(
    (reply) => flagError(tab?.id, reply),
    (e) => flagError(tab?.id, { type: 'error', message: String(e?.message ?? e) }),
  );
});

// ---------------------------------------------------------------------------
// Messages from the content script and the popup
// ---------------------------------------------------------------------------

const handlers = {
  async getMedia(msg, sender) {
    return mediaFor(msg.tabId ?? sender.tab?.id);
  },
  async send(msg, sender) {
    const title = sender.tab?.title;
    const items = msg.items.map((i) => ({ title, ...i }));
    return sendToApp(items, msg.mode ?? 'ask', msg.pageUrl ?? sender.tab?.url);
  },
  async ping() {
    return hello({ force: true });
  },
  /** The qualities of a page's video (yt-dlp) or of a stream, for the video button. */
  async formats(msg, sender) {
    const pageUrl = msg.pageUrl ?? sender.tab?.url;
    const item = await enrich({ title: sender.tab?.title, ...msg.item }, pageUrl);
    return native({ type: 'formats', item });
  },
  async bypass(msg) {
    bypass.set(msg.url, Date.now() + BYPASS_MS);
    return true;
  },
  async getSettings() {
    return getSettings();
  },
  async setSettings(msg) {
    const settings = { ...(await getSettings()), ...msg.settings };
    await api.storage.local.set({ settings });
    return settings;
  },
  /** The app's rules, refreshed when the popup opens. */
  async getRules(msg) {
    return getRules({ maxAge: msg.fresh ? 0 : RULES_FRESH_MS });
  },
  async setSiteCapture(msg) {
    return setSiteCapture(msg.host, msg.capture);
  },
  async showApp(msg) {
    return native({ type: 'show', view: msg.view ?? 'downloads' });
  },
  /** The file the tab shows, if it shows one (a PDF, an image…). */
  async getTabFile(msg) {
    const file = tabFiles.get(msg.tabId);
    return file && file.url === msg.url ? file : null;
  },
  async sendTabFile(msg) {
    const file = tabFiles.get(msg.tabId);
    if (!file) return { type: 'error', message: 'This tab does not show a file.' };
    return sendToApp([{ url: file.url, fileName: file.fileName ?? undefined, referer: file.referer }], 'ask', file.referer);
  },
};

api.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  const handler = handlers[msg?.type];
  if (!handler) return false;
  handler(msg, sender).then(sendResponse, (e) => sendResponse({ type: 'error', message: String(e?.message ?? e) }));
  return true; // async response
});
