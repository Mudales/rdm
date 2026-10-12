// Which browser downloads go to rdm. The rules come from the app
// (Settings → Capture); this mirrors `CaptureSettings::decide` in
// crates/rdm-manager/src/capture.rs, and both run test/capture-cases.json.

/** Used until the app has been reached once (same as the app's defaults). */
export const DEFAULT_RULES = {
  extensions: [
    'mp4', 'mkv', 'webm', 'avi', 'mov', 'wmv', 'flv', 'm4v', 'mpg', 'mpeg', '3gp', 'ogv', 'ts', 'm2ts', 'vob', 'rm', 'rmvb', 'asf',
    'mp3', 'm4a', 'aac', 'flac', 'wav', 'ogg', 'oga', 'opus', 'wma', 'aiff', 'alac', 'mka',
    'zip', 'rar', '7z', 'tar', 'gz', 'tgz', 'bz2', 'xz', 'zst', 'lz', 'lzma', 'cab', 'arj',
    'exe', 'msi', 'msix', 'msu', 'appx', 'apk', 'aab', 'deb', 'rpm', 'appimage', 'pkg', 'jar', 'flatpak', 'snap',
    'iso', 'img', 'dmg', 'vhd', 'vhdx', 'vmdk', 'qcow2',
    'pdf', 'epub', 'mobi', 'djvu',
  ],
  minSize: null,
  excludedSites: [],
  excludedAddresses: [],
  captureInline: false,
};

/** Extensions for common MIME types, for downloads whose name is not known yet. */
const MIME_EXTENSIONS = {
  'application/pdf': 'pdf',
  'application/zip': 'zip',
  'application/x-zip-compressed': 'zip',
  'application/x-7z-compressed': '7z',
  'application/vnd.rar': 'rar',
  'application/x-rar-compressed': 'rar',
  'application/gzip': 'gz',
  'application/x-gzip': 'gz',
  'application/x-tar': 'tar',
  'application/x-bzip2': 'bz2',
  'application/x-xz': 'xz',
  'application/zstd': 'zst',
  'application/x-msdownload': 'exe',
  'application/x-msdos-program': 'exe',
  'application/vnd.microsoft.portable-executable': 'exe',
  'application/x-msi': 'msi',
  'application/x-ms-installer': 'msi',
  'application/x-apple-diskimage': 'dmg',
  'application/x-iso9660-image': 'iso',
  'application/vnd.android.package-archive': 'apk',
  'application/vnd.debian.binary-package': 'deb',
  'application/x-debian-package': 'deb',
  'application/x-rpm': 'rpm',
  'application/java-archive': 'jar',
  'application/epub+zip': 'epub',
  'application/x-bittorrent': 'torrent',
  'application/msword': 'doc',
  'application/vnd.openxmlformats-officedocument.wordprocessingml.document': 'docx',
  'application/vnd.ms-excel': 'xls',
  'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet': 'xlsx',
  'application/vnd.ms-powerpoint': 'ppt',
  'application/vnd.openxmlformats-officedocument.presentationml.presentation': 'pptx',
  'application/json': 'json',
  'text/plain': 'txt',
  'text/csv': 'csv',
  'video/mp4': 'mp4',
  'video/webm': 'webm',
  'video/x-matroska': 'mkv',
  'video/quicktime': 'mov',
  'video/x-msvideo': 'avi',
  'video/mpeg': 'mpg',
  'audio/mpeg': 'mp3',
  'audio/mp4': 'm4a',
  'audio/ogg': 'ogg',
  'audio/wav': 'wav',
  'audio/x-wav': 'wav',
  'audio/flac': 'flac',
  'image/jpeg': 'jpg',
  'image/png': 'png',
  'image/gif': 'gif',
  'image/webp': 'webp',
  'image/avif': 'avif',
  'image/svg+xml': 'svg',
};

export function extensionForMime(mime) {
  return MIME_EXTENSIONS[String(mime ?? '').split(';')[0].trim().toLowerCase()] ?? '';
}

/** Lower-case extension of a file name, or ''. */
export function nameExtension(name) {
  const last = String(name ?? '').split(/[\\/]/).pop();
  const dot = last.lastIndexOf('.');
  return dot > 0 ? last.slice(dot + 1).toLowerCase() : '';
}

/** Lower-case extension of the URL's last path segment, or ''. */
export function urlExtension(url) {
  try {
    return nameExtension(new URL(url).pathname.split('/').pop() ?? '');
  } catch {
    return '';
  }
}

/** `*` matches any run of characters. */
export function glob(pattern, text) {
  let p = 0;
  let t = 0;
  let star = -1;
  let mark = 0;
  while (t < text.length) {
    if (p < pattern.length && pattern[p] === '*') {
      star = p++;
      mark = t;
    } else if (p < pattern.length && pattern[p] === text[t]) {
      p++;
      t++;
    } else if (star >= 0) {
      p = star + 1;
      t = ++mark;
    } else {
      return false;
    }
  }
  while (pattern[p] === '*') p++;
  return p === pattern.length;
}

/** `example.com` matches itself and its subdomains. */
export function hostMatches(host, site) {
  return !!site && (host === site || host.endsWith(`.${site}`));
}

export function hostOf(url) {
  try {
    return new URL(url).hostname.toLowerCase();
  } catch {
    return '';
  }
}

export function selectsExtension(rules, ext) {
  return rules.extensions.some((e) => glob(e, ext));
}

/**
 * Whether a download of `url` (named `fileName`, `size` bytes when known)
 * goes to rdm. Returns `{ decision, extension? }` with `decision` one of
 * 'capture', 'notSelected', 'excludedSite', 'excludedAddress', 'tooSmall'.
 */
export function decide(rules, { url, fileName = null, size = null, mime = null }) {
  const host = hostOf(url);
  if (rules.excludedSites.some((site) => hostMatches(host, site))) return { decision: 'excludedSite' };
  const lowerUrl = String(url).toLowerCase();
  const addressHit = rules.excludedAddresses.some((p) => {
    const pattern = p.toLowerCase();
    return pattern.includes('*') ? glob(pattern, lowerUrl) : lowerUrl.startsWith(pattern);
  });
  if (addressHit) return { decision: 'excludedAddress' };
  let extension = fileName && fileName.trim() ? nameExtension(fileName) : urlExtension(url);
  if (!extension && mime) extension = extensionForMime(mime);
  if (!extension || !selectsExtension(rules, extension)) return { decision: 'notSelected', extension };
  if (rules.minSize != null && size != null && size >= 0 && size < rules.minSize) return { decision: 'tooSmall' };
  return { decision: 'capture' };
}

/** Whether downloads from `host` go to rdm at all. */
export function capturesSite(rules, host) {
  return !rules.excludedSites.some((site) => hostMatches(String(host).toLowerCase(), site));
}
