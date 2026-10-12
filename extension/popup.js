import { KINDS, describe, filterLinks, isSupportedKind } from './lib/detect.js';

const api = globalThis.browser ?? globalThis.chrome;
const $ = (sel) => document.querySelector(sel);
const ALL_SITES = { origins: ['<all_urls>'] };

const [tab] = await api.tabs.query({ active: true, currentWindow: true });
const send = (msg) => api.runtime.sendMessage(msg);

function el(tag, attrs = {}, ...children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'text') e.textContent = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else if (v === true) e.setAttribute(k, '');
    else if (v !== false && v != null) e.setAttribute(k, v);
  }
  e.append(...children);
  return e;
}

function message(text, kind = '') {
  const m = $('#message');
  m.textContent = text;
  m.className = `message ${kind}`;
}

function report(reply, success) {
  if (reply?.type === 'error') message(reply.message, 'error');
  else message(success, 'ok');
}

// --- Status ----------------------------------------------------------------

async function checkStatus() {
  const s = $('#status');
  const reply = await send({ type: 'ping' });
  if (reply?.type === 'pong') {
    s.textContent = `Connected to rdm ${reply.version}`;
    s.className = 'status ok';
  } else if (reply?.type === 'notRunning') {
    s.textContent = 'rdm will start when needed';
    s.className = 'status';
  } else {
    s.textContent = 'rdm not found';
    s.className = 'status error';
    s.title = reply?.message ?? '';
    if (reply?.message) message(reply.message, 'error');
  }
}

// Firefox treats host permissions as optional in MV3.
async function checkPermission() {
  const granted = await api.permissions.contains(ALL_SITES).catch(() => true);
  $('#permission').hidden = granted;
}
$('#grant').addEventListener('click', async () => {
  await api.permissions.request(ALL_SITES).catch(() => false);
  checkPermission();
});

// --- Media -----------------------------------------------------------------

async function showMedia() {
  const items = tab?.id != null ? (await send({ type: 'getMedia', tabId: tab.id })) ?? [] : [];
  $('#media-empty').hidden = items.length > 0;
  $('#media').replaceChildren(
    ...items.map((item) => {
      const supported = isSupportedKind(item.kind);
      return el(
        'li',
        { title: item.url },
        el(
          'span',
          { class: 'text' },
          el('span', { class: 'name', text: item.fileName || item.url }),
          el('span', { class: 'meta', text: describe(item) }),
        ),
        el('button', {
          text: 'Download',
          disabled: !supported,
          onclick: async () => {
            const stream = item.kind === 'hls' || item.kind === 'dash';
            const reply = await send({
              type: 'send',
              items: [{ url: item.url, kind: item.kind, title: tab.title, fileName: stream ? undefined : item.fileName }],
              mode: 'ask',
              pageUrl: tab.url,
            });
            report(reply, 'Sent to rdm.');
          },
        }),
      );
    }),
  );
}

// yt-dlp reads the page itself: sites whose player hides the video file.
if (/^https?:/.test(tab?.url ?? '')) {
  $('#find-page').hidden = false;
  $('#find-page').addEventListener('click', async () => {
    const reply = await send({ type: 'send', items: [{ url: tab.url, kind: 'page', title: tab.title }], mode: 'ask', pageUrl: tab.url });
    if (reply?.type === 'error') report(reply, '');
    else window.close();
  });
}

// --- Download all links -----------------------------------------------------

let pageLinks = [];
const KIND_LABELS = { video: 'Video', audio: 'Audio', archives: 'Archives', documents: 'Documents', programs: 'Programs', images: 'Images' };

$('#kinds').append(
  ...Object.keys(KINDS).map((k) =>
    el('label', { class: 'check' }, el('input', { type: 'checkbox', value: k, onchange: updateCount }), KIND_LABELS[k]),
  ),
);
$('#exts').addEventListener('input', updateCount);
$('#same-host').addEventListener('change', updateCount);

function selectedLinks() {
  return filterLinks(pageLinks, {
    kinds: [...document.querySelectorAll('#kinds input:checked')].map((i) => i.value),
    extensions: $('#exts').value.split(/[\s,]+/).filter(Boolean),
    sameHost: $('#same-host').checked,
    pageUrl: tab?.url,
  });
}

function updateCount() {
  const n = selectedLinks().length;
  $('#link-count').textContent = `${n} of ${pageLinks.length} links selected`;
  $('#add-links').disabled = n === 0;
  $('#add-links').textContent = n ? `Add ${n} to queue` : 'Add to queue';
}

async function loadLinks() {
  try {
    const reply = await api.tabs.sendMessage(tab.id, { type: 'collectLinks' }, { frameId: 0 });
    pageLinks = reply?.links ?? [];
  } catch {
    pageLinks = [];
    $('#link-count').textContent = 'Not available on this page.';
    return;
  }
  updateCount();
}

$('#add-links').addEventListener('click', async () => {
  const links = selectedLinks();
  $('#add-links').disabled = true;
  message(`Sending ${links.length} links…`);
  const reply = await send({ type: 'send', items: links.map((l) => ({ url: l.url })), mode: 'queue', pageUrl: tab.url });
  report(reply, `Added ${reply?.ids?.length ?? links.length} downloads to rdm's queue. Start the queue in rdm to begin.`);
  updateCount();
});

// --- This tab ----------------------------------------------------------------

async function showTabFile() {
  const file = tab?.id != null ? await send({ type: 'getTabFile', tabId: tab.id, url: tab.url }) : null;
  $('#tab-file').hidden = !file;
  if (!file) return;
  $('#tab-file-name').textContent = file.fileName || file.url;
  $('#tab-file-name').title = file.url;
}
$('#tab-file-send').addEventListener('click', async () => {
  const reply = await send({ type: 'sendTabFile', tabId: tab.id });
  report(reply, 'Sent to rdm.');
});

// --- Settings ----------------------------------------------------------------

const settings = await send({ type: 'getSettings' });
$('#capture').checked = settings.captureDownloads;
$('#video-button').checked = settings.showVideoButton;
$('#capture').addEventListener('change', (e) => send({ type: 'setSettings', settings: { captureDownloads: e.target.checked } }));
$('#video-button').addEventListener('change', (e) => send({ type: 'setSettings', settings: { showVideoButton: e.target.checked } }));

let host = '';
try {
  host = /^https?:/.test(tab?.url ?? '') ? new URL(tab.url).hostname : '';
} catch {
  /* not a web page */
}

/** Shows the app's capture rules (fetched fresh, or the cached copy). */
async function showRules() {
  const rules = await send({ type: 'getRules', fresh: true });
  if (!rules) return;
  const n = rules.extensions.length;
  const parts = [`${n} file type${n === 1 ? '' : 's'}`];
  if (rules.minSize) parts.push(`larger than ${(rules.minSize / 1e6).toFixed(1).replace(/\.0$/, '')} MB`);
  if (rules.captureInline) parts.push('also when opened in a tab');
  $('#types-summary').textContent = `Takes ${parts.join(', ')}.`;
  if (host) {
    const excluded = rules.excludedSites.some((s) => host === s || host.endsWith(`.${s}`));
    $('#site-row').hidden = false;
    $('#site').checked = !excluded;
    $('#site-label').textContent = `From ${host}`;
  }
}
$('#site').addEventListener('change', async (e) => {
  const reply = await send({ type: 'setSiteCapture', host, capture: e.target.checked });
  if (reply?.type === 'captureRules') message(e.target.checked ? `Downloads from ${host} go to rdm.` : `Downloads from ${host} stay in the browser.`, 'ok');
  else {
    e.target.checked = !e.target.checked;
    report(reply?.type === 'notRunning' ? { type: 'error', message: 'Start rdm to change this.' } : reply, '');
  }
});
$('#choose-types').addEventListener('click', async () => {
  const reply = await send({ type: 'showApp', view: 'captureSettings' });
  if (reply?.type === 'error') report(reply, '');
  else window.close();
});

await Promise.all([checkStatus(), checkPermission(), showTabFile(), showMedia(), loadLinks(), showRules()]);
