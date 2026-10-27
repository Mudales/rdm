// Test cases for lib/detect.js and lib/capture.js, shared by the Node
// runner (detect.test.mjs, used in CI) and the browser runner (index.html).

import {
  classifyResponse,
  cookieHeader,
  describe,
  extensionOf,
  fileNameFromUrl,
  filterLinks,
  responseSize,
  downloadCandidate,
  dispositionFileName,
  fileLinks,
  isSupportedKind,
  DEFAULT_SETTINGS,
  browserName,
  classifyRequest,
  compareVersions,
} from '../lib/detect.js';
import { DEFAULT_RULES, capturesSite, decide, extensionForMime, glob } from '../lib/capture.js';
// The same cases run against the Rust implementation (rdm-manager).
import shared from './capture-cases.json' with { type: 'json' };

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

  ['browser downloads are candidates only when enabled and http(s)', (t) => {
    const s = DEFAULT_SETTINGS;
    t.deepEqual(downloadCandidate({ url: 'https://x/setup.exe', filename: '', fileSize: -1, totalBytes: 0 }, s), { url: 'https://x/setup.exe', fileName: null, size: null, mime: null });
    t.deepEqual(
      downloadCandidate({ url: 'https://x/download?id=4', finalUrl: 'https://cdn/x', filename: '/Users/me/Downloads/report.pdf', totalBytes: 2048, mime: 'application/pdf' }, s),
      { url: 'https://cdn/x', fileName: 'report.pdf', size: 2048, mime: 'application/pdf' },
    );
    t.equal(downloadCandidate({ url: 'blob:https://x/1', filename: 'a.zip' }, s), null);
    t.equal(downloadCandidate({ url: 'data:application/zip;base64,AA', filename: 'a.zip' }, s), null);
    t.equal(downloadCandidate({ url: 'https://x/a.zip', filename: 'a.zip', byExtensionId: 'other' }, s), null);
    t.equal(downloadCandidate({ url: 'https://x/a.zip', filename: 'a.zip' }, { ...s, captureDownloads: false }), null);
  }],

  ['media is recognised when its request starts, before the server answers', (t) => {
    const s3 = 'https://bucket.s3.eu-west-2.amazonaws.com/lessons/85__%D7%90%D7%99%D7%9F.mp3?response-content-type=audio%2Fmpeg&X-Amz-Signature=abc';
    t.deepEqual(classifyRequest({ url: `${s3}#t=1`, type: 'media' }), {
      kind: 'audio', url: s3, contentType: null, size: null, fileName: '85__אין.mp3', pending: true,
    });
    t.equal(classifyRequest({ url: 'https://x/clip.MP4', type: 'media' }).kind, 'video');
    t.equal(classifyRequest({ url: 'https://x/master.m3u8?t=1', type: 'xmlhttprequest' }).kind, 'hls');
    t.equal(classifyRequest({ url: 'https://x/manifest.mpd', type: 'other' }).kind, 'dash');
    // Only what the address says: no extension, or not a media type.
    t.equal(classifyRequest({ url: 'https://x/stream?id=3', type: 'media' }), null);
    t.equal(classifyRequest({ url: 'https://x/seg-001.ts', type: 'media' }), null);
    t.equal(classifyRequest({ url: 'https://x/cover.jpg', type: 'media' }), null);
    // Pages, frames and images are not media requests; segments of adaptive players are not videos.
    t.equal(classifyRequest({ url: 'https://x/clip.mp4', type: 'main_frame' }), null);
    t.equal(classifyRequest({ url: 'https://x/clip.mp4', type: 'image' }), null);
    t.equal(classifyRequest({ url: 'https://r1.googlevideo.com/videoplayback?mime=video%2Fmp4&x.mp4', type: 'media' }), null);
    t.equal(classifyRequest({ url: 'blob:https://x/1', type: 'media' }), null);
  }],

  ['versions compare by number, not text', (t) => {
    t.ok(compareVersions('0.10.0', '0.9.9') > 0);
    t.ok(compareVersions('0.3.1', '0.3.1') === 0);
    t.ok(compareVersions('0.3', '0.3.1') < 0);
    t.ok(compareVersions('1.0.0', undefined) > 0);
  }],

  ['the browser is named for the app', (t) => {
    t.equal(browserName({}, true), 'firefox');
    t.equal(browserName({ brave: {}, userAgent: 'Chrome/140' }), 'brave');
    t.equal(browserName({ userAgent: 'Mozilla/5.0 Chrome/140.0 Safari/537.36 Edg/140.0' }), 'edge');
    t.equal(browserName({ userAgentData: { brands: [{ brand: 'Google Chrome' }, { brand: 'Chromium' }] } }), 'chrome');
    t.equal(browserName({ userAgentData: { brands: [{ brand: 'Chromium' }] } }), 'chromium');
    t.equal(browserName({ userAgent: 'Chrome/140 OPR/120' }), 'opera');
  }],

  ['downloads from the browser history are never taken', (t) => {
    const s = DEFAULT_SETTINGS;
    const now = Date.parse('2026-10-13T12:00:00Z');
    const item = { url: 'https://x/setup.exe', filename: '', state: 'in_progress', startTime: '2026-10-13T11:59:58Z' };
    t.equal(downloadCandidate(item, s, now)?.url, 'https://x/setup.exe');
    // Chrome loads its history through onCreated: finished, interrupted, or old.
    t.equal(downloadCandidate({ ...item, state: 'complete' }, s, now), null);
    t.equal(downloadCandidate({ ...item, state: 'interrupted' }, s, now), null);
    t.equal(downloadCandidate({ ...item, startTime: '2026-09-01T08:00:00Z' }, s, now), null);
    // A browser that reports neither is trusted.
    t.equal(downloadCandidate({ url: 'https://x/a.zip', filename: 'a.zip' }, s, now)?.url, 'https://x/a.zip');
  }],

  ['Content-Disposition file names', (t) => {
    t.equal(dispositionFileName('attachment; filename="report 1.pdf"'), 'report 1.pdf');
    t.equal(dispositionFileName("inline; filename*=UTF-8''%D7%A9%D7%99%D7%AA%D7%95%D7%A3.pdf; filename=\"x.pdf\""), 'שיתוף.pdf');
    t.equal(dispositionFileName('attachment; filename=../../etc/passwd'), 'passwd');
    t.equal(dispositionFileName('inline'), '');
    t.equal(dispositionFileName(undefined), '');
  }],

  ['"download all links" keeps links to files only', (t) => {
    const links = [
      { url: 'https://x.org/a.zip', text: 'A' },
      { url: 'https://x.org/a.zip#again' },
      { url: 'https://x.org/about.html' },
      { url: 'https://x.org/view.php?id=3' },
      { url: 'https://x.org/docs/' },
      { url: 'mailto:me@x.org' },
      { url: 'https://cdn.x.org/b.PDF?sig=1' },
    ];
    t.deepEqual(fileLinks(links).map((l) => l.url), ['https://x.org/a.zip', 'https://cdn.x.org/b.PDF?sig=1']);
  }],

  ['capture rules: shared cases', (t) => {
    t.deepEqual(DEFAULT_RULES.extensions, shared.defaultExtensions);
    for (const c of shared.cases) {
      const got = decide(shared.rules, { url: c.url, fileName: c.fileName ?? null, size: c.size ?? null });
      t.equal(`${c.url} ${got.decision}`, `${c.url} ${c.expect}`);
      if ('extension' in c) t.equal(got.extension, c.extension);
    }
  }],

  ['capture rules: MIME type names files that have no extension yet', (t) => {
    t.equal(extensionForMime('application/pdf; charset=binary'), 'pdf');
    t.equal(extensionForMime('text/html'), '');
    t.equal(decide(DEFAULT_RULES, { url: 'https://x/get?id=1', mime: 'application/zip' }).decision, 'capture');
    t.equal(decide(DEFAULT_RULES, { url: 'https://x/get?id=1', mime: 'text/html' }).decision, 'notSelected');
    t.equal(decide(DEFAULT_RULES, { url: 'https://x/notes.txt' }).decision, 'notSelected');
    t.ok(glob('a*b*c', 'aXbYc') && !glob('r0*', 'rar') && glob('*', ''));
    t.equal(capturesSite({ ...DEFAULT_RULES, excludedSites: ['x.org'] }, 'DL.X.org'), false);
    t.equal(capturesSite({ ...DEFAULT_RULES, excludedSites: ['x.org'] }, 'notx.org'), true);
  }],

  ["YouTube's player chunks are not offered as videos", (t) => {
    const chunk = resp('https://rr2---sn-nhpax.googlevideo.com/videoplayback?itag=299&range=0-1048575', { 'Content-Type': 'video/mp4', 'Content-Length': '1048576' });
    t.equal(classifyResponse(chunk), null);
    const other = resp('https://cdn.example.com/videoplayback/clip.mp4', { 'Content-Type': 'video/mp4', 'Content-Length': '1048576' });
    t.equal(classifyResponse(other)?.kind, 'video');
  }],

  ['streams are downloadable', (t) => {
    t.equal(isSupportedKind('hls'), true);
    t.equal(isSupportedKind('dash'), true);
    t.equal(isSupportedKind('image'), false);
  }],

  ['descriptions are short and informative', (t) => {
    t.equal(describe({ kind: 'video', size: 5 * 1024 * 1024, contentType: 'video/mp4' }), 'Video · 5.0 MiB · video/mp4');
    t.equal(describe({ kind: 'hls', size: 900, contentType: 'application/vnd.apple.mpegurl' }), 'HLS stream · 900 B');
    t.equal(describe({ kind: 'audio', size: null, contentType: null, pending: true }), 'Audio · not loaded yet');
  }],
];
