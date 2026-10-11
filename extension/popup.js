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
          el('span', { class: 'meta', text: supported ? describe(item) : `${describe(item)} · not supported yet` }),
        ),
        el('button', {
          text: 'Download',
          disabled: !supported,
          onclick: async () => {
            const reply = await send({ type: 'send', items: [{ url: item.url, fileName: item.fileName }], mode: 'ask', pageUrl: tab.url });
            report(reply, 'Sent to rdm.');
          },
        }),
      );
    }),
  );
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

// --- Settings ----------------------------------------------------------------

const settings = await send({ type: 'getSettings' });
$('#capture').checked = settings.captureDownloads;
$('#video-button').checked = settings.showVideoButton;
$('#capture').addEventListener('change', (e) => send({ type: 'setSettings', settings: { captureDownloads: e.target.checked } }));
$('#video-button').addEventListener('change', (e) => send({ type: 'setSettings', settings: { showVideoButton: e.target.checked } }));

await Promise.all([checkStatus(), checkPermission(), showMedia(), loadLinks()]);
