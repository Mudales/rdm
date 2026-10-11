# rdm — Rust Download Manager

A cross-platform (macOS, Windows, Linux) download manager written in Rust,
inspired by Internet Download Manager and
[ab-download-manager](https://github.com/amir1376/ab-download-manager).

> **Status:** Phase 1 done. The download engine (`rdm-core`) and the headless
> `rdm` CLI work; the GUI, browser integration and streaming media come next.
> See [Roadmap](#roadmap).

## Architecture

```mermaid
flowchart LR
    subgraph Browser
        EXT["extension/<br/>MV3 extension<br/>(detects media, page links)"]
    end

    subgraph rdm["rdm processes"]
        HOST["rdm-host<br/>native messaging host<br/>(stdio, length-prefixed JSON)"]
        GUI["rdm-gui<br/>Tauri v2 app<br/>(UI, queues, SQLite, tray)"]
        CLI["rdm-cli<br/>headless binary"]
    end

    subgraph libs["libraries"]
        MEDIA["rdm-media<br/>HLS / DASH parsing,<br/>ffmpeg muxing"]
        CORE["rdm-core<br/>download engine<br/>(probe, segmentation, resume,<br/>throttling, progress events)"]
    end

    FFMPEG[["ffmpeg<br/>(sidecar process)"]]
    NET(("HTTP(S) servers"))
    DISK[("output file<br/>+ .rdm sidecar")]

    EXT <-- "native messaging" --> HOST
    HOST <-- "local IPC" --> GUI
    GUI --> MEDIA
    GUI --> CORE
    CLI --> CORE
    MEDIA --> CORE
    MEDIA --> FFMPEG
    CORE --> NET
    CORE --> DISK
```

The dependency rule: **`rdm-core` depends on nothing above it.** It has no
GUI, extension or ffmpeg dependencies, and it can be used on its own as a
library. CI enforces this with [`scripts/check-deps.sh`](scripts/check-deps.sh).

| Path | Kind | Responsibility |
|---|---|---|
| `crates/rdm-core` | lib | Probing, dynamic segmentation, positioned writes to one sparse file, resume metadata, retry/backoff, throttling, progress events |
| `crates/rdm-media` | lib | HLS/DASH manifest parsing, segment download, ffmpeg muxing, AES-128 HLS |
| `crates/rdm-cli` | bin `rdm` | Headless front end, used to test the engine without a GUI |
| `crates/rdm-host` | bin `rdm-host` | Native messaging host, plus manifest installer for Chrome, Edge and Firefox |
| `crates/rdm-gui` | bin `rdm-gui` | Tauri v2 desktop app |
| `extension/` | MV3 | Browser extension |
| `tests/` | crate `rdm-tests` | axum mock server and integration tests |

## Building

Prerequisites: a stable Rust toolchain (installed through
[rustup](https://rustup.rs); `rust-toolchain.toml` selects stable with
rustfmt and clippy). TLS uses rustls, so OpenSSL is never required.

```bash
cargo build --workspace
```

```bash
cargo test --workspace
```

These are the same checks CI runs on all three OSes:

```bash
cargo fmt --all --check
```

```bash
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

```bash
./scripts/check-deps.sh
```

## Using the CLI

```bash
cargo install --path crates/rdm-cli
```

```bash
rdm get https://example.com/big.iso -c 8 --limit 5M
```

- `-o PATH` saves to a file, or into a directory under the server's file
  name. The default is the current directory.
- Ctrl-C stops and keeps progress. Running the same command again (or
  `rdm resume <file>`) continues from where it stopped, after checking that
  the server's file hasn't changed (ETag / Last-Modified / size).
- `rdm probe URL` shows the size, whether the server supports resuming, the
  file name and the validators, without downloading anything.
- The cookie, Referer, User-Agent, Basic auth and proxy options are listed in
  `rdm get --help`.

### Reproducing the speed benchmark

`mock-server` serves deterministic content and can cap each connection's
speed. That cap is the situation where multiple connections help:

```bash
cargo run --release -p rdm-tests --bin mock-server -- --size 1G --rate 25M
```

```bash
cargo run --release -p rdm-cli -- get http://127.0.0.1:8080/file.bin -o one.bin -c 1 --no-proxy
```

```bash
cargo run --release -p rdm-cli -- get http://127.0.0.1:8080/file.bin -o eight.bin -c 8 --no-proxy
```

```bash
cargo run --release -p rdm-tests --bin mock-server -- --verify eight.bin --size 1G
```

On an M-series Mac this took 41.4 s with one connection and 5.2 s with
eight. A `kill -9` partway through, followed by a re-run, produced a file
with the same SHA-256.

## How the engine works

- **Probe:** a `GET` with `Range: bytes=0-0`. A `206` reply gives the size
  and shows that ranges work. With a `200`, the body is reused for a single
  stream, so one-time links still work.
- **Dynamic segmentation:** a connection that finishes takes over the
  second half of the largest range still in progress. Ranges are never split
  below `--min-split` (1 MiB by default).
- **HTTP/1.1 only:** with HTTP/2, all the connections would share one TCP
  stream and the speed-up would disappear.
- **One output file:** every connection writes at its final offset with
  `pwrite` / `seek_write`. No part files are concatenated at the end.
- **Crash safety:** the file data is `fsync`'d *before* `<file>.rdm` (which
  lists the completed ranges) is atomically replaced. A crash can lose
  recent progress but never marks unwritten bytes as complete. Cookies and
  credentials are never written to disk.
- **Resource changes:** every range request carries `If-Range`, so a server
  whose file changed answers `200` rather than mixing old and new bytes.
- **Retries:** exponential backoff with jitter, honouring `Retry-After`.
  Counters are per connection and reset whenever that connection makes
  progress.
- **Throttling:** token buckets per download and global. A connection that
  is over its limit stops reading the socket, so the limit applies on the
  network, not just to disk writes.
- **Politeness:** a per-host connection cap (16 by default) applies across
  all downloads.

Later phases add more prerequisites:
- **Phase 3 (GUI):** the Tauri v2 system dependencies. On Linux that's
  `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev` and
  `patchelf`.
- **Phase 4 (extension):** Node.js, if the extension is written in TypeScript.
- **Phase 5 (media):** ffmpeg on `PATH`. If it's missing, rdm offers to
  download it.

## Project rules

- No `unsafe` code. The workspace sets `unsafe_code = "forbid"`. Positioned
  file writes go through the safe std APIs `std::os::unix::fs::FileExt` and
  `std::os::windows::fs::FileExt`.
- Library code never panics. `unwrap`, `expect`, `panic!`, `todo!` and
  `unimplemented!` are lint errors outside tests.
- Libraries define errors with `thiserror`. Binaries use `anyhow`. Logging
  goes through `tracing`.
- No DRM circumvention. DRM-protected streams (Widevine, PlayReady,
  FairPlay, SAMPLE-AES) are reported as protected content and never
  decrypted.
- Servers are treated politely. A global per-host connection cap and rate
  limit always apply, and any site-mirroring feature respects `robots.txt`.

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| 0 | Workspace, lints, CI, README | ✅ |
| 1 | `rdm-core` engine + `rdm-cli` | ✅ |
| 2 | Mock server fault injection + integration tests | ⏳ |
| 3 | Tauri GUI, SQLite persistence, tray | |
| 4 | MV3 extension + native messaging host | |
| 5 | HLS/DASH + ffmpeg muxing | |
| 6 | Scheduler, import/export, checksums, AV hook, duplicates, drag-and-drop | |
| 7 | Packaging, signing docs, v0.1.0 release | |

## License

MIT. See [LICENSE](LICENSE).
