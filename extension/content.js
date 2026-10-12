// Content script: a "Download" button over videos, link collection for the
// popup's "download all", and Alt+click pass-through to the browser.
//
// The button lives in a closed shadow root so page CSS cannot restyle or
// hide it, and page scripts cannot reach into it.

(() => {
  const api = globalThis.browser ?? globalThis.chrome;
  if (window.__rdmContentLoaded) return;
  window.__rdmContentLoaded = true;

  const MIN_WIDTH = 160;
  const MIN_HEIGHT = 90;
  const HIDE_DELAY = 1500;

  const send = (msg) => api.runtime.sendMessage(msg).catch(() => null);

  // -------------------------------------------------------------------------
  // Messages from the popup
  // -------------------------------------------------------------------------

  api.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (msg?.type !== 'collectLinks') return false;
    // `selectionOnly`: the links inside the selected text (right-click menu).
    const selection = msg.selectionOnly ? window.getSelection() : null;
    const wanted = (node) => !selection || selection.containsNode(node, true);
    const links = [];
    for (const a of document.querySelectorAll('a[href]')) {
      if (wanted(a)) links.push({ url: a.href, text: (a.textContent ?? '').trim().slice(0, 120) });
    }
    for (const m of document.querySelectorAll('video[src], audio[src], source[src]')) {
      if (wanted(m)) links.push({ url: m.src, text: '' });
    }
    sendResponse({ links, pageUrl: location.href, title: document.title });
    return false;
  });

  // Alt+click is the browser's own "download this link"; let it through.
  document.addEventListener(
    'click',
    (e) => {
      if (!e.altKey) return;
      const a = e.target instanceof Element ? e.target.closest('a[href]') : null;
      if (a) send({ type: 'bypass', url: a.href });
    },
    true,
  );

  // -------------------------------------------------------------------------
  // Video button
  // -------------------------------------------------------------------------

  let enabled = true;
  send({ type: 'getSettings' }).then((s) => {
    enabled = s?.showVideoButton !== false;
  });

  /** Videos that played DRM-protected content (EME). */
  const protectedVideos = new WeakSet();
  document.addEventListener(
    'encrypted',
    (e) => {
      if (e.target instanceof HTMLMediaElement) protectedVideos.add(e.target);
    },
    true,
  );
  const isProtected = (v) => protectedVideos.has(v) || v.mediaKeys != null;

  let host = null;
  let ui = null;
  let current = null; // the video the button is attached to
  /** Videos whose button the user closed (until the page reloads). */
  const dismissed = new WeakSet();
  let hideTimer = null;
  let menuOpen = false;

  function build() {
    host = document.createElement('rdm-overlay');
    host.style.cssText = 'all: initial; position: fixed; z-index: 2147483647; top: 0; left: 0; width: 0; height: 0;';
    const root = host.attachShadow({ mode: 'closed' });
    root.innerHTML = `
      <style>
        :host { all: initial; }
        * { box-sizing: border-box; font: 13px/1.35 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif; }
        .bar { position: fixed; display: none; align-items: center; gap: 4px; }
        .btn {
          display: inline-flex; align-items: center; gap: 6px;
          padding: 5px 10px 5px 8px; border: 0; border-radius: 999px; cursor: pointer;
          background: rgba(37, 99, 235, 0.95); color: #fff; font-weight: 600;
          box-shadow: 0 2px 10px rgba(0, 0, 0, 0.35);
        }
        .btn.protected { background: rgba(75, 85, 99, 0.95); cursor: help; }
        .btn svg { width: 15px; height: 15px; fill: none; stroke: currentColor; stroke-width: 2.4; stroke-linecap: round; stroke-linejoin: round; }
        .close {
          display: inline-grid; place-items: center; width: 22px; height: 22px; padding: 0;
          border: 0; border-radius: 50%; cursor: pointer;
          background: rgba(17, 24, 39, 0.8); color: #fff; opacity: 0.75;
          box-shadow: 0 2px 8px rgba(0, 0, 0, 0.3);
        }
        .close:hover { opacity: 1; }
        .close svg { width: 12px; height: 12px; fill: none; stroke: currentColor; stroke-width: 2.6; stroke-linecap: round; }
        .menu {
          position: fixed; display: none; min-width: 280px; max-width: 420px; max-height: 320px; overflow: auto;
          padding: 6px; border-radius: 10px; background: #1f2430; color: #e6e9ef;
          box-shadow: 0 10px 30px rgba(0, 0, 0, 0.45);
        }
        .menu h4 { margin: 4px 6px 6px; font-size: 12px; font-weight: 600; color: #98a2b3; text-transform: uppercase; letter-spacing: .04em; }
        .item { display: block; width: 100%; text-align: left; padding: 7px 8px; border: 0; border-radius: 6px; background: none; color: inherit; cursor: pointer; }
        .item:hover:not(:disabled) { background: #2b3445; }
        .item:disabled { opacity: .5; cursor: default; }
        .item .name { display: block; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
        .item .meta { display: block; font-size: 12px; color: #98a2b3; }
        .note { padding: 8px; color: #98a2b3; }
        .note.error { color: #f87171; }
      </style>
      <div class="bar">
        <button class="btn" part="button" type="button">
          <svg class="icon-dl" viewBox="0 0 24 24"><path d="M12 4v11M7 10l5 5 5-5M5 20h14"/></svg>
          <svg class="icon-lock" viewBox="0 0 24 24" style="display:none"><rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V8a4 4 0 0 1 8 0v3"/></svg>
          <span>Download</span>
        </button>
        <button class="close" type="button" title="Hide for this video" aria-label="Hide the Download button for this video">
          <svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>
        </button>
      </div>
      <div class="menu" role="menu"></div>`;
    ui = {
      bar: root.querySelector('.bar'),
      close: root.querySelector('.close'),
      btn: root.querySelector('.btn'),
      label: root.querySelector('.btn span'),
      iconDownload: root.querySelector('.icon-dl'),
      iconLock: root.querySelector('.icon-lock'),
      menu: root.querySelector('.menu'),
    };
    ui.btn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (current && isProtected(current)) return;
      menuOpen ? closeMenu() : openMenu();
    });
    ui.close.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (current) dismissed.add(current);
      hide();
    });
    for (const el of [ui.bar, ui.menu]) {
      el.addEventListener('mouseenter', () => clearTimeout(hideTimer));
      el.addEventListener('mouseleave', scheduleHide);
    }
    document.documentElement.append(host);
  }

  function place() {
    if (!current || !ui) return;
    const r = current.getBoundingClientRect();
    if (r.width < MIN_WIDTH || r.height < MIN_HEIGHT || r.bottom < 0 || r.top > innerHeight) {
      hide();
      return;
    }
    const top = Math.max(8, r.top + 10);
    const right = Math.max(8, innerWidth - r.right + 10);
    ui.bar.style.top = `${top}px`;
    ui.bar.style.right = `${right}px`;
    ui.menu.style.top = `${top + 36}px`;
    ui.menu.style.right = `${right}px`;
  }

  function show(video) {
    if (!enabled || dismissed.has(video)) return;
    if (!host) build();
    if (current !== video) closeMenu();
    current = video;
    const prot = isProtected(video);
    ui.btn.classList.toggle('protected', prot);
    ui.label.textContent = prot ? 'Protected' : 'Download';
    ui.iconDownload.style.display = prot ? 'none' : '';
    ui.iconLock.style.display = prot ? '' : 'none';
    ui.btn.title = prot
      ? 'This video is DRM-protected. rdm does not download protected content.'
      : 'Download this video with rdm';
    ui.bar.style.display = 'flex';
    place();
    clearTimeout(hideTimer);
  }

  function hide() {
    if (ui) ui.bar.style.display = 'none';
    closeMenu();
    current = null;
  }

  function scheduleHide() {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
      if (!menuOpen) hide();
    }, HIDE_DELAY);
  }

  function closeMenu() {
    menuOpen = false;
    if (ui) ui.menu.style.display = 'none';
  }

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }

  async function openMenu() {
    menuOpen = true;
    ui.menu.replaceChildren(el('div', 'note', 'Looking for media…'));
    ui.menu.style.display = 'block';

    const detected = (await send({ type: 'getMedia' })) ?? [];
    const own = current?.currentSrc;
    const items = [...detected];
    if (own && /^https?:/i.test(own) && !items.some((i) => i.url === own)) {
      items.unshift({ kind: 'video', url: own, fileName: own.split('/').pop()?.split('?')[0] ?? '' });
    }

    const nodes = [el('h4', null, 'Download with rdm')];
    if (!items.length) {
      nodes.push(el('div', 'note', 'Nothing found in what the page loaded. yt-dlp can look for the video:'));
    }
    for (const item of items) {
      const supported = ['video', 'audio', 'hls', 'dash'].includes(item.kind);
      const btn = el('button', 'item');
      btn.type = 'button';
      btn.disabled = !supported;
      btn.append(
        el('span', 'name', item.fileName || item.url),
        el(
          'span',
          'meta',
          supported
            ? [labelFor(item.kind), sizeText(item.size)].filter(Boolean).join(' · ')
            : labelFor(item.kind),
        ),
      );
      btn.addEventListener('click', async () => {
        btn.disabled = true;
        // Streams are named after the page; their URLs say "master.m3u8".
        const stream = item.kind === 'hls' || item.kind === 'dash';
        const reply = await send({
          type: 'send',
          items: [{ url: item.url, kind: item.kind, title: document.title, fileName: stream ? undefined : item.fileName }],
          mode: 'ask',
        });
        if (reply?.type === 'error') {
          ui.menu.replaceChildren(el('div', 'note error', reply.message));
        } else {
          closeMenu();
        }
      });
      nodes.push(btn);
    }
    // yt-dlp reads the page itself (YouTube, Vimeo and many more).
    const find = el('button', 'item');
    find.type = 'button';
    find.append(
      el('span', 'name', 'Find the video on this page'),
      el('span', 'meta', 'yt-dlp reads the page and offers its qualities'),
    );
    find.addEventListener('click', async () => {
      find.disabled = true;
      const reply = await send({
        type: 'send',
        items: [{ url: location.href, kind: 'page', title: document.title }],
        mode: 'ask',
      });
      if (reply?.type === 'error') ui.menu.replaceChildren(el('div', 'note error', reply.message));
      else closeMenu();
    });
    nodes.push(find);
    ui.menu.replaceChildren(...nodes);
    place();
  }

  const labelFor = (kind) => ({ video: 'Video', audio: 'Audio', hls: 'HLS stream', dash: 'DASH stream' })[kind] ?? 'File';
  function sizeText(n) {
    if (!n) return '';
    const units = ['B', 'KiB', 'MiB', 'GiB'];
    let i = 0;
    while (n >= 1024 && i < units.length - 1) {
      n /= 1024;
      i++;
    }
    return `${n.toFixed(i ? 1 : 0)} ${units[i]}`;
  }

  // Hover detection by geometry: players usually cover the <video> with
  // their own control layers, so mouse events rarely reach it directly.
  let pending = false;
  document.addEventListener(
    'mousemove',
    (e) => {
      if (pending || !enabled) return;
      pending = true;
      requestAnimationFrame(() => {
        pending = false;
        const hit = [...document.getElementsByTagName('video')].find((v) => {
          const r = v.getBoundingClientRect();
          return r.width >= MIN_WIDTH && r.height >= MIN_HEIGHT && e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
        });
        if (hit) show(hit);
        else if (current) scheduleHide();
      });
    },
    { passive: true },
  );
  addEventListener('scroll', place, { passive: true, capture: true });
  addEventListener('resize', place, { passive: true });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') closeMenu();
  });
})();
