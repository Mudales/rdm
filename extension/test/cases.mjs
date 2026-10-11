// Test cases for lib/detect.js, shared by the Node runner (detect.test.mjs,
// used in CI) and the browser runner (index.html).

import {
  classifyResponse,
  cookieHeader,
  describe,
  extensionOf,
  fileNameFromUrl,
  filterLinks,
  responseSize,
  shouldCapture,
  DEFAULT_SETTINGS,
} from '../lib/detect.js';

const h = (pairs) => Object.entries(pairs).map(([name, value]) => ({ name, value }));
const resp = (url, headers, statusCode = 200) => ({ url, statusCode, responseHeaders: h(headers) });

export const cases = [
  ['extension and file name come from the last path segment', (t) => {
    t.equal(extensionOf('https://x.org/a/b/Movie.MKV?x=1#t'), 'mkv');
    t.equal(extensionOf('https://x.org/a/b/'), '');
    t.equal(extensionOf('https://x.org/.hidden'), '');
    t.equal(extensionOf('not a url'), '');
    t.equal(fileNameFromUrl('https://x.org/dir/My%20File.mp4?t=1'), 'My File.mp4');
    t.equal(fileNameFromUrl('https://x.org/dir/bad%E0.mp4'), 'bad%E0.mp4');
  }],

  ['size from Content-Range beats Content-Length', (t) => {
    t.equal(responseSize(h({ 'Content-Range': 'bytes 0-1023/5000000', 'Content-Length': '1024' })), 5000000);
    t.equal(responseSize(h({ 'content-length': '42' })), 42);
    t.equal(responseSize(h({})), null);
  }],

  ['video and audio responses are detected', (t) => {
    const v = classifyResponse(resp('https://cdn.x/v/clip.mp4#t=3', { 'Content-Type': 'video/mp4', 'Content-Length': '9000000' }));
    t.deepEqual(v, { kind: 'video', url: 'https://cdn.x/v/clip.mp4', contentType: 'video/mp4', size: 9000000, fileName: 'clip.mp4' });
    const range = classifyResponse(resp('https://cdn.x/v/clip', { 'Content-Type': 'video/webm', 'Content-Range': 'bytes 0-99/7000000' }, 206));
    t.equal(range.kind, 'video');
    t.equal(range.size, 7000000);
    const a = classifyResponse(resp('https://cdn.x/song.mp3', { 'Content-Type': 'application/octet-stream', 'Content-Length': '3000000' }));
    t.equal(a.kind, 'audio');
  }],

  ['streaming playlists are detected by type or extension', (t) => {
    t.equal(classifyResponse(resp('https://x/master.m3u8?token=1', { 'Content-Type': 'text/plain' })).kind, 'hls');
    t.equal(classifyResponse(resp('https://x/playlist', { 'Content-Type': 'application/vnd.apple.mpegurl' })).kind, 'hls');
    t.equal(classifyResponse(resp('https://x/manifest.mpd', { 'Content-Type': 'application/dash+xml' })).kind, 'dash');
  }],

  ['segments, previews, errors and non-media are ignored', (t) => {
    t.equal(classifyResponse(resp('https://x/seg-001.ts', { 'Content-Type': 'video/mp2t', 'Content-Length': '2000000' })), null);
    t.equal(classifyResponse(resp('https://x/chunk-12.m4s', { 'Content-Type': 'video/mp4', 'Content-Length': '2000000' })), null);
    t.equal(classifyResponse(resp('https://x/preview.mp4', { 'Content-Type': 'video/mp4', 'Content-Length': '30000' })), null);
    t.equal(classifyResponse(resp('https://x/clip.mp4', { 'Content-Type': 'video/mp4' }, 404)), null);
    t.equal(classifyResponse(resp('https://x/page.html', { 'Content-Type': 'text/html', 'Content-Length': '900000' })), null);
    t.equal(classifyResponse(resp('blob:https://x/123', { 'Content-Type': 'video/mp4' })), null);
  }],

  ['cookies become a Cookie header', (t) => {
    t.equal(cookieHeader([{ name: 'a', value: '1' }, { name: 'sid', value: 'x=y' }]), 'a=1; sid=x=y');
    t.equal(cookieHeader([]), '');
    t.equal(cookieHeader(undefined), '');
  }],

  ['download-all filters by kind, extension and host', (t) => {
    const links = [
      { url: 'https://x.org/a.zip', text: 'A' },
      { url: 'https://x.org/a.zip#again', text: 'dup' },
      { url: 'https://cdn.other/b.ZIP' },
      { url: 'https://x.org/c.pdf' },
      { url: 'https://x.org/d.mp4' },
      { url: 'https://x.org/page.html' },
      { url: 'javascript:alert(1)' },
      { url: 'mailto:a@b.c' },
      'https://x.org/e.flac',
    ];
    const urls = (opts) => filterLinks(links, { pageUrl: 'https://x.org/list', ...opts }).map((l) => l.url);
    t.deepEqual(urls({ kinds: ['archives'] }), ['https://x.org/a.zip', 'https://cdn.other/b.ZIP']);
    t.deepEqual(urls({ kinds: ['archives'], sameHost: true }), ['https://x.org/a.zip']);
    t.deepEqual(urls({ kinds: ['audio'], extensions: ['.PDF'] }), ['https://x.org/c.pdf', 'https://x.org/e.flac']);
    t.deepEqual(urls({}), []);
  }],

  ['browser downloads are captured by extension only when enabled', (t) => {
    const s = DEFAULT_SETTINGS;
    t.equal(shouldCapture({ url: 'https://x/setup.exe', filename: '' }, s), true);
    t.equal(shouldCapture({ url: 'https://x/download?id=4', finalUrl: 'https://cdn/x', filename: '/Users/me/Downloads/report.pdf' }, s), true);
    t.equal(shouldCapture({ url: 'https://x/page.html', filename: 'page.html' }, s), false);
    t.equal(shouldCapture({ url: 'blob:https://x/1', filename: 'a.zip' }, s), false);
    t.equal(shouldCapture({ url: 'data:application/zip;base64,AA', filename: 'a.zip' }, s), false);
    t.equal(shouldCapture({ url: 'https://x/a.zip', filename: 'a.zip', byExtensionId: 'other' }, s), false);
    t.equal(shouldCapture({ url: 'https://x/a.zip', filename: 'a.zip' }, { ...s, captureDownloads: false }), false);
  }],

  ['descriptions are short and informative', (t) => {
    t.equal(describe({ kind: 'video', size: 5 * 1024 * 1024, contentType: 'video/mp4' }), 'Video · 5.0 MiB · video/mp4');
    t.equal(describe({ kind: 'hls', size: 900, contentType: 'application/vnd.apple.mpegurl' }), 'HLS stream · 900 B');
  }],
];
