// Background script (MV3 service worker in Chrome/Edge, event page in
// Firefox). Watches responses for media, captures browser downloads, and
// talks to the rdm app through the native messaging host.

import { DEFAULT_SETTINGS, classifyResponse, cookieHeader, fileNameFromUrl, shouldCapture } from './lib/detect.js';

const api = globalThis.browser ?? globalThis.chrome;
const HOST = 'dev.rdm.host';
const MAX_ITEMS_PER_TAB = 100;
/** Alt+click on a link lets the browser download it (Chrome's own gesture). */
const BYPASS_MS = 10_000;

// ---------------------------------------------------------------------------
// Settings
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

api.webRequest.onHeadersReceived.addListener(
  (details) => {
    if (details.tabId < 0) return;
    const item = classifyResponse(details);
    if (!item) return;
    restore().then(() => {
      let items = tabMedia.get(details.tabId);
      if (!items) tabMedia.set(details.tabId, (items = new Map()));
      const previous = items.get(item.url);
      items.delete(item.url); // re-insert as most recent
      items.set(item.url, { ...previous, ...item, size: item.size ?? previous?.size ?? null, seenAt: Date.now() });
      while (items.size > MAX_ITEMS_PER_TAB) items.delete(items.keys().next().value);
      updateBadge(details.tabId);
      persist();
    });
  },
  { urls: ['http://*/*', 'https://*/*'] },
  ['responseHeaders'],
);

api.tabs.onUpdated.addListener((tabId, info) => {
  // A new document (not a same-document/history navigation) starts fresh.
  if (info.status === 'loading' && info.url) clearTab(tabId);
});
api.tabs.onRemoved.addListener(clearTab);

async function mediaFor(tabId) {
  await restore();
  return [...(tabMedia.get(tabId)?.values() ?? [])].reverse();
}

// ---------------------------------------------------------------------------
// Talking to rdm
// ---------------------------------------------------------------------------

const NOT_INSTALLED =
  'rdm is not installed, or not registered with this browser. In rdm, open Settings → Browser and click Register.';

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

/** Adds browser context (cookies, User-Agent, Referer) and sends items. */
async function sendToApp(items, mode, pageUrl) {
  const enriched = await Promise.all(
    items.map(async (item) => {
      let cookies = '';
      try {
        cookies = cookieHeader(await api.cookies.getAll({ url: item.url }));
      } catch {
        /* no cookie access for this URL */
      }
      return {
        url: item.url,
        fileName: item.fileName || undefined,
        referer: item.referer || pageUrl || undefined,
        pageUrl: pageUrl || undefined,
        userAgent: navigator.userAgent,
        cookies: cookies || undefined,
      };
    }),
  );
  return native({ type: 'add', mode, items: enriched });
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

api.downloads.onCreated.addListener(async (item) => {
  const url = item.finalUrl || item.url;
  if (reissued.delete(url) || bypassed(url)) return;
  const settings = await getSettings();
  if (!shouldCapture(item, settings)) return;

  // Stop the browser's copy first; rdm takes it from here.
  await api.downloads.cancel(item.id).catch(() => {});
  await api.downloads.erase({ id: item.id }).catch(() => {});

  const fileName = (item.filename ?? '').split(/[\\/]/).pop() || fileNameFromUrl(url) || undefined;
  const reply = await sendToApp([{ url, fileName, referer: item.referrer }], 'ask', item.referrer);
  if (reply?.type === 'error') {
    // Never lose a download: give it back to the browser.
    reissued.add(url);
    api.downloads.download({ url }).catch(() => reissued.delete(url));
    console.warn('rdm: handing the download back to the browser:', reply.message);
  }
});

// ---------------------------------------------------------------------------
// Context menus
// ---------------------------------------------------------------------------

api.runtime.onInstalled.addListener(async () => {
  await api.contextMenus.removeAll();
  api.contextMenus.create({ id: 'rdm-link', title: 'Download link with rdm', contexts: ['link'] });
  api.contextMenus.create({ id: 'rdm-media', title: 'Download this media with rdm', contexts: ['video', 'audio'] });
});

api.contextMenus.onClicked.addListener((info) => {
  const url = info.menuItemId === 'rdm-link' ? info.linkUrl : info.srcUrl;
  if (!/^https?:/i.test(url ?? '')) return; // blob: media is a stream
  sendToApp([{ url }], 'ask', info.pageUrl);
});

// ---------------------------------------------------------------------------
// Messages from the content script and the popup
// ---------------------------------------------------------------------------

const handlers = {
  async getMedia(msg, sender) {
    return mediaFor(msg.tabId ?? sender.tab?.id);
  },
  async send(msg, sender) {
    return sendToApp(msg.items, msg.mode ?? 'ask', msg.pageUrl ?? sender.tab?.url);
  },
  async ping() {
    return native({ type: 'ping' });
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
};

api.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  const handler = handlers[msg?.type];
  if (!handler) return false;
  handler(msg, sender).then(sendResponse, (e) => sendResponse({ type: 'error', message: String(e?.message ?? e) }));
  return true; // async response
});
