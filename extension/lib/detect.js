// Pure logic shared by the background script, the popup and the tests.
// No browser APIs in here, so it runs under `node --test` as well.

export const KINDS = {
  video: ['mp4', 'mkv', 'webm', 'avi', 'mov', 'wmv', 'flv', 'm4v', 'mpg', 'mpeg', '3gp', 'ogv'],
  audio: ['mp3', 'm4a', 'aac', 'flac', 'wav', 'ogg', 'oga', 'opus', 'wma'],
  archives: ['zip', 'rar', '7z', 'tar', 'gz', 'tgz', 'bz2', 'xz', 'zst', 'iso', 'dmg'],
  documents: ['pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'odt', 'ods', 'odp', 'epub', 'csv', 'txt'],
  programs: ['exe', 'msi', 'msix', 'apk', 'deb', 'rpm', 'appimage', 'pkg', 'jar'],
  images: ['jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'bmp', 'tif', 'tiff', 'heic'],
};

/** This browser's own switches. Which file types go to rdm is set in the app. */
export const DEFAULT_SETTINGS = {
  captureDownloads: true,
  showVideoButton: true,
};

const HLS_TYPES = ['application/vnd.apple.mpegurl', 'application/x-mpegurl', 'audio/mpegurl', 'audio/x-mpegurl'];
const DASH_TYPES = ['application/dash+xml'];
/** Pieces of a stream, not something to offer on their own. */
const SEGMENT_EXTENSIONS = ['ts', 'm4s', 'cmfv', 'cmfa', 'aac_seg'];
const SEGMENT_TYPES = ['video/mp2t', 'video/iso.segment', 'audio/iso.segment'];
/** Smaller video/audio responses are previews, sprites or probes. */
const MIN_MEDIA_BYTES = 64 * 1024;

const isHttp = (url) => /^https?:\/\//i.test(url ?? '');

/** Lower-case extension of the URL's last path segment, or ''. */
export function extensionOf(url) {
  try {
    const last = new URL(url).pathname.split('/').pop() ?? '';
    const dot = last.lastIndexOf('.');
    return dot > 0 ? last.slice(dot + 1).toLowerCase() : '';
  } catch {
    return '';
  }
}

/** Decoded last path segment, or ''. */
export function fileNameFromUrl(url) {
  try {
    const last = new URL(url).pathname.split('/').pop() ?? '';
    try {
      return decodeURIComponent(last);
    } catch {
      return last;
    }
  } catch {
    return '';
  }
}

/** The URL without its fragment, used to de-duplicate detections. */
export function mediaKey(url) {
  return String(url).split('#')[0];
}

/** Case-insensitive lookup in a webRequest `responseHeaders` array. */
export function header(headers, name) {
  const lower = name.toLowerCase();
  return headers?.find((h) => h.name.toLowerCase() === lower)?.value;
}

/** Total size from Content-Range (`bytes a-b/total`) or Content-Length. */
export function responseSize(headers) {
  const range = header(headers, 'content-range');
  const total = range?.match(/\/(\d+)\s*$/)?.[1];
  if (total) return Number(total);
  const len = header(headers, 'content-length');
  return len && /^\d+$/.test(len.trim()) ? Number(len) : null;
}

/**
 * Decides whether an observed response is downloadable media.
 * Returns `{ kind, url, contentType, size, fileName }` or null.
 * `kind` is 'video', 'audio', 'hls' or 'dash'.
 */
export function classifyResponse({ url, statusCode, responseHeaders }) {
  if (!isHttp(url) || (statusCode !== 200 && statusCode !== 206)) return null;
  const contentType = (header(responseHeaders, 'content-type') ?? '').split(';')[0].trim().toLowerCase();
  const ext = extensionOf(url);
  const size = responseSize(responseHeaders);
  const result = (kind) => ({ kind, url: mediaKey(url), contentType: contentType || null, size, fileName: fileNameFromUrl(url) });

  if (HLS_TYPES.includes(contentType) || ext === 'm3u8') return result('hls');
  if (DASH_TYPES.includes(contentType) || ext === 'mpd') return result('dash');
  if (SEGMENT_TYPES.includes(contentType) || SEGMENT_EXTENSIONS.includes(ext)) return null;
  if (size != null && size < MIN_MEDIA_BYTES) return null;

  const generic = contentType === '' || contentType === 'application/octet-stream' || contentType === 'binary/octet-stream';
  if (contentType.startsWith('video/') || (generic && KINDS.video.includes(ext))) return result('video');
  if (contentType.startsWith('audio/') || (generic && KINDS.audio.includes(ext))) return result('audio');
  return null;
}

/** `[{name, value}]` cookies → a `Cookie` header value. */
export function cookieHeader(cookies) {
  return (cookies ?? []).map((c) => `${c.name}=${c.value}`).join('; ');
}

/**
 * Filters a page's links for "download all": http(s) only, de-duplicated,
 * matching any selected kind or extra extension, optionally same host only.
 */
export function filterLinks(links, { kinds = [], extensions = [], sameHost = false, pageUrl = '' } = {}) {
  const wanted = new Set([...kinds.flatMap((k) => KINDS[k] ?? []), ...extensions.map((e) => e.replace(/^\./, '').toLowerCase())]);
  let pageHost = '';
  try {
    pageHost = new URL(pageUrl).host;
  } catch {
    /* no page URL */
  }
  const seen = new Set();
  const out = [];
  for (const link of links) {
    const url = mediaKey(link.url ?? link);
    if (!isHttp(url) || seen.has(url)) continue;
    if (!wanted.has(extensionOf(url))) continue;
    if (sameHost && pageHost && new URL(url).host !== pageHost) continue;
    seen.add(url);
    out.push({ url, text: link.text ?? '' });
  }
  return out;
}

/**
 * A browser download rdm could take, before the app's capture rules apply:
 * `{ url, fileName, size, mime }`, or null when capturing is off, another
 * extension started it, or it is not http(s) (blob:, data:, file:).
 * `item` is a downloads.DownloadItem.
 */
export function downloadCandidate(item, settings) {
  if (!settings.captureDownloads || item.byExtensionId) return null;
  const url = item.finalUrl || item.url;
  if (!isHttp(url)) return null;
  const fileName = (item.filename ?? '').split(/[\\/]/).pop() || null;
  const size = item.fileSize > 0 ? item.fileSize : item.totalBytes > 0 ? item.totalBytes : null;
  return { url, fileName, size, mime: item.mime || null };
}

const UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
export function formatBytes(n) {
  if (n == null) return '';
  let v = n;
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(v < 10 ? 1 : 0)} ${UNITS[i]}`;
}

/** Whether rdm can download this detected item. */
export const isSupportedKind = (kind) => ['video', 'audio', 'hls', 'dash'].includes(kind);

/** Short human description of a detected item. */
export function describe(item) {
  const label = { video: 'Video', audio: 'Audio', hls: 'HLS stream', dash: 'DASH stream' }[item.kind] ?? 'File';
  const parts = [label];
  if (item.size) parts.push(formatBytes(item.size));
  if (item.contentType && (item.kind === 'video' || item.kind === 'audio')) parts.push(item.contentType);
  return parts.join(' · ');
}

/** The file name in a Content-Disposition header, or ''. */
export function dispositionFileName(value) {
  if (!value) return '';
  const star = value.match(/filename\*\s*=\s*([^']*)'[^']*'([^;]+)/i);
  if (star) {
    try {
      return decodeURIComponent(star[2].trim().replace(/^"|"$/g, ''));
    } catch {
      /* fall back to the plain parameter */
    }
  }
  const plain = value.match(/filename\s*=\s*("([^"]*)"|[^;]+)/i);
  const name = (plain?.[2] ?? plain?.[1] ?? '').trim();
  return name.split(/[\\/]/).pop() ?? '';
}

/** Extensions of web pages rather than files. */
const PAGE_EXTENSIONS = new Set(['html', 'htm', 'xhtml', 'shtml', 'php', 'asp', 'aspx', 'jsp', 'cgi', 'pl', 'do', 'action', 'cfm']);

/**
 * Links that point to files (an extension that is not a web page's), for
 * "download all links": http(s) only, de-duplicated, in page order.
 */
export function fileLinks(links) {
  const seen = new Set();
  const out = [];
  for (const link of links) {
    const url = mediaKey(link.url ?? link);
    if (!isHttp(url) || seen.has(url)) continue;
    const ext = extensionOf(url);
    if (!ext || PAGE_EXTENSIONS.has(ext)) continue;
    seen.add(url);
    out.push({ url, text: link.text ?? '' });
  }
  return out;
}
