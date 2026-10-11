# Changelog

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
