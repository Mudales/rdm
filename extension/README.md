# rdm browser extension

A Manifest V3 extension for Chrome, Edge, Brave, Chromium and Firefox (128+).
It sends downloads, videos and links to the rdm app. It's plain JavaScript
with no build step.

## What it does

- **Detects media.** `webRequest.onHeadersReceived` watches responses for
  video/audio content types and `.m3u8` / `.mpd` playlists. Stream segments,
  small previews and failed responses are ignored. The toolbar badge shows
  how many items were found on the tab.
- **Adds a Download button on videos.** Hovering a video shows a button in
  a closed shadow root, so page CSS and scripts can't touch it. It lists the
  media found on the page. Videos that use encrypted media (EME/DRM) show
  **Protected** instead, and nothing is offered.
- **Captures browser downloads.** Downloads whose file type is on the list
  are cancelled in the browser and handed to rdm's Add dialog. Alt+click a
  link to let the browser download it instead. If rdm can't be reached, the
  download goes back to the browser, so it's never lost.
- **Downloads all links.** The popup filters a page's links by type (video,
  audio, archives, documents, programs, images), by extra extensions, or to
  the same site only, and adds the selection to rdm's queue.
- **Adds context menus:** "Download link with rdm" and "Download this media
  with rdm".

Every request carries the page URL as `Referer`, the browser's
`User-Agent`, and the cookies for the target URL, because many hosts refuse
downloads without them. rdm keeps cookies in memory only.

HLS and DASH playlists are downloadable. rdm's Add dialog lets you pick the
quality, the audio track and the format, and the file is named after the
page title. DRM-protected streams are refused.

## How it reaches the app

```
extension ──native messaging──▶ rdm-host ──local socket──▶ rdm app
```

The browser starts `rdm-host` (installed next to the app) for each message.
`rdm-host` relays the message to the running app and starts the app if it
isn't running.

## Installing for development

1. **Register `rdm-host` with your browsers.** In the app, open Settings →
   Browser → Register. From a source checkout you can also do it by hand:

   ```bash
   cargo build -p rdm-host -p rdm-gui
   ```

   ```bash
   ./target/debug/rdm-host install
   ```

   With no flags it registers with every browser it finds. Add
   `--browser chrome` (or edge, brave, chromium, firefox) to choose.
   `rdm-host status` shows the current state, and `rdm-host uninstall`
   removes the registration.

2. **Load the extension.**
   - **Chrome / Edge / Brave:** open `chrome://extensions`, turn on Developer
     mode, click *Load unpacked* and pick this `extension/` folder. The
     `key` in `manifest.json` fixes the extension ID to
     `dacdblamhhaedfahmbcllbpgjejdaddi`, which is the ID `rdm-host`
     allows.
   - **Firefox:** open `about:debugging#/runtime/this-firefox`, click *Load
     Temporary Add-on* and pick `manifest.json`. The add-on ID is
     `rdm@rdm.dev`. Firefox asks for website access separately; the popup
     has a button for it.

A store listing gets a different ID. Add it with
`rdm-host install --extension-id <id>` and to `CHROME_EXTENSION_IDS` in
`crates/rdm-host/src/lib.rs`.

## Tests

The logic is kept in [`lib/detect.js`](lib/detect.js), which uses no
browser APIs: media classification, link filtering and download capture
rules. Its cases in [`test/cases.mjs`](test/cases.mjs) run under Node:

```bash
node --test extension/test/
```

They also run in a browser. Serve this folder and open `test/index.html`:

```bash
python3 -m http.server 8000 --directory extension
```
