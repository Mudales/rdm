# Changelog

## Unreleased

### Improvements
- The extension setup assistant gained an **Open extensions page** button.
  rdm opens that page once while setting the extension up, but a closed tab,
  or a browser that was not running, left nothing but an address to copy and
  paste by hand.

## v0.4.4 (2026-10-29)

### Fixes
- Windows: downloads yt-dlp handled itself failed with *yt-dlp finished
  without reporting the file* whenever the video's name was not plain
  English — every Hebrew YouTube title, for instance — although yt-dlp had
  downloaded the video. yt-dlp prints in the system's ANSI code page, never
  UTF-8, and rdm read the finished file's path from that output: on a
  Hebrew Windows the line was bytes rdm could not read, and on another code
  page the letters were dropped and the path named no file. rdm now asks
  yt-dlp for the path in a file, which yt-dlp always writes as UTF-8, and
  no longer discards output it cannot decode — yt-dlp's own error messages
  survive on those systems too.
- Downloads yt-dlp handled itself showed no progress at all (0 B until they
  finished). Asking yt-dlp to print the finished file's name also makes it
  quiet, and a quiet yt-dlp reports no progress unless it is asked to.

## v0.4.3 (2026-10-29)

### Fixes
- Downloads from servers that keep a file compressed (Google Cloud
  Storage among them, which hosts Warp's installer) came out corrupt: the
  file was the compressed length and would not open. Such a server
  answers a byte range from its compressed copy while sending the body
  decompressed, so the offsets, and the size it reports, are the
  compressed ones. Splitting that into connections wrote decompressed and
  compressed bytes into the same file. rdm now recognises this and
  downloads over one connection, which returns the file itself, and says
  so. A range whose body outruns what was asked for is no longer trusted
  either.

## v0.4.2 (2026-10-27)

### Fixes
- Extension: on playlist sites (lessons, podcasts) the toolbar list lost
  the audio after about six episodes until the page was reloaded. The
  site's player keeps earlier files open, and browsers open only six
  connections to a server, so the next file's request waited for an
  answer, and rdm listed media only once the server answered. Files are
  now listed as soon as they are requested (*not loaded yet* until the
  answer arrives), and the list no longer drops the file that started
  just before the page's address changed.

## v0.4.1 (2026-10-13)

### Fixes
- Windows: a console window flashed on screen whenever rdm ran ffmpeg,
  yt-dlp or Deno (finding and downloading videos, merging, the setup
  assistant's version checks, installs), the antivirus scan or the
  shutdown command. They now run without one.

## v0.4.0 (2026-10-13)

### Setup assistant
- On first start (and from ⋯ → *Set up browser and tools*), rdm offers its
  three add-ons, each installed only when its button is clicked:
  - **Browser extension.** *Add to Chrome* (Edge, Brave, Chromium,
    Firefox) registers the helper, writes the extension (shipped inside
    rdm) to a folder, opens the browser's extensions page, copies the
    folder's path and lists the two clicks left. It turns green as soon as
    the extension connects.
  - **yt-dlp and Deno**, and **ffmpeg**, with their sources and licenses.
  - *Don't show this when rdm starts* turns the startup offer off.
- The extension tells rdm its version. rdm rewrites the extension folder
  when rdm updates, and the extension reloads itself once to pick the new
  version up; the popup explains when it cannot (loaded from elsewhere,
  or an older rdm).

### Videos on web pages
- The video Download button lists the video's qualities itself: yt-dlp
  reads the page as soon as the menu opens (when the page loaded no plain
  video file), and an HLS/DASH stream expands into its qualities. One click
  opens rdm's prompt with that quality chosen; rdm reuses yt-dlp's reading
  of the page instead of running it again.
- Without yt-dlp, the menu (and the Add dialog) offer to install it in one
  click, then look for the video.

### Add dialog
- Shorter: URL, quality, name and folder, with *Start download* and
  *Download later*. *More options* (remembered) has the category, queue,
  connections, speed limit, referer, cookies and checksum.
- *Always save … here* makes the chosen folder the category's.
- *Install ffmpeg* right where a video needs it.
- A second address in the same dialog picks its own category again.

### Fixes
- The extension sent old downloads from the browser's history to rdm (or
  back to the browser) one after another, e.g. every installer the user
  had ever downloaded. Browsers report their history through the same
  event as new downloads; only downloads starting now are taken, and an
  address is never taken twice within 30 seconds or in an implausible
  burst.

## v0.3.1 (2026-10-12)

### Fixes
- Adding a web page address without yt-dlp (or with web pages turned off)
  saved the page's HTML under the page's name. rdm now refuses it and
  points to Settings → Media; the Add dialog disables its add buttons
  with the same advice.

## v0.3.0 (2026-10-12)

### Videos on web pages
- Paste a YouTube, Vimeo or other video page into Add, or use the
  extension's *Find the video on this page*: yt-dlp reads the page, and rdm
  offers one choice per resolution (H.264 first, for compatibility) plus
  *Audio only*. Playlist and channel pages open the batch dialog.
- rdm downloads what yt-dlp finds with its own engine: several connections
  in 10 MB requests (YouTube serves short requests at full speed but slows
  long ones), HLS/DASH segments in parallel, and ffmpeg merging. A 1080p60
  YouTube video (268 MB) took 5.5 s from page to merged MP4.
- yt-dlp's own downloader takes over only for formats rdm cannot fetch
  (RTMP, …) or sites that refuse rdm's requests.
- Settings → Media downloads yt-dlp and Deno (needed for YouTube) on
  request, checked against their published SHA-256, and keeps yt-dlp
  updated weekly. A copy you installed yourself is used as is.
- DRM-protected and live videos are refused.

### Engine
- New per-request size limit (`max_request_size`, `rdm get
  --request-size 10M`): a connection fetches its range as a series of
  short requests.

### Extension
- A small × beside the video Download button hides it for that video.
- YouTube's internal video pieces are no longer listed as videos.

### Fixes
- A DASH stream stored as a single file could hang after it finished.
- The install script failed on macOS (`VERSION…: unbound variable`): the
  system `sh` misreads non-ASCII text in UTF-8 locales.

## v0.2.0 (2026-10-12)

### Choosing what rdm takes from the browser
- New **Capture** tab in Settings: file types in groups (Video, Audio,
  Archives, Programs, Disk images, Documents, Images, Other), selected with
  one click per group or per type, with search, presets and your own
  extensions (wildcards such as `r0*` work).
- Types the browser downloaded without rdm are listed with a count and an
  **Add** button, so the list grows from what you actually download.
- A minimum file size, sites to leave alone (subdomains included), and
  address patterns to leave alone.
- Optional: take PDFs, images and videos that would open in a browser tab.
- The extension follows these rules (it used to have its own fixed list).
  Copied links are offered for the same file types.

### Right-click menu
- rdm ▸ Download link / Add link to the queue / Download selected links /
  Download image or media / Download all links on this page / Download this
  file (a tab showing a PDF etc.) / Detected media ▸ / Take downloads from
  this site / Choose file types / Open rdm.
- Several links open a new list dialog with checkboxes, a filter and
  selection by type.

### Install and update
- Install scripts: `curl … install.sh | sh` for macOS and Linux,
  `irm … install.ps1 | iex` for Windows. They verify `SHA256SUMS`; they
  can also uninstall.
- In-app updates (Settings → About): check now or automatically, then
  install and restart. Updates are signed, and rdm refuses packages that
  don't match its key.

### Fixes
- Downloads that need the browser's login cookies failed with 403 when the
  server redirected to another path (cookies were scoped to the file's
  folder instead of the whole site).
- Linux AppImage: registering the browser helper pointed browsers at a
  temporary path that disappeared when rdm closed.
- The `.deb` listed its dependencies twice.

## v0.1.0 (2026-10-11)

First release. The builds are unsigned; see
[docs/RELEASING.md](docs/RELEASING.md#installing-unsigned-builds).

### Downloads
- Multi-connection downloads with dynamic segmentation: a connection that
  finishes takes over half of the largest range still in progress. On a
  server that caps each connection, 8 connections were 7.9× faster than
  one (1 GiB).
- All connections write into one output file at the right offsets. Crash
  safety: a `kill -9` and a re-run produce a byte-identical file, and
  checksums are re-validated (ETag / Last-Modified / size).
- Falls back to a single stream when a server ignores byte ranges or sends
  no size.
- Retries with backoff (honoring `Retry-After`), per-download and global
  speed limits, a per-host connection cap, HTTP/SOCKS5 proxies, Basic auth,
  cookies.

### Desktop app
- Download list with a progress bar per connection's segment, queues with
  concurrency limits and schedules, categories with their own folders,
  SQLite persistence, tray, single instance, light and dark themes.
- Import/export, checksum verification (yours or the server's), an optional
  antivirus hook, duplicate detection, drag-and-drop, clipboard monitoring.

### Streaming media
- HLS (TS or fMP4, AES-128) and static DASH. You choose the quality, the
  audio track and the format; downloads resume after stopping.
  DRM-protected and live streams are refused.
- ffmpeg is used only when needed. It can be downloaded on request
  (SHA-256 verified) on macOS and Windows.

### Browser integration
- A Manifest V3 extension for Chrome, Edge, Brave and Firefox:
  - detects media, with a Download button on videos ("Protected" for DRM)
  - captures browser downloads (Alt+click to skip)
  - downloads all links on a page
- `rdm-host` native messaging host, registered per browser from the app.
- Private local IPC: a Unix socket or a per-user named pipe.

### Known limitations
- Not code-signed.
- Multi-period DASH, live streams and subtitles are not supported.
- The ffmpeg download is not offered on Linux; install it with your
  package manager.
