# Store listing text

Copy-paste text for the Chrome Web Store, Microsoft Edge Add-ons and
addons.mozilla.org (AMO). The steps are in [README.md](README.md).

## Name

rdm — Rust Download Manager

## Summary (Chrome: at most 132 characters; Edge: short description; AMO: summary, at most 250)

Send downloads, videos and links to rdm, a free open-source download manager with multi-connection downloads and resume.

## Description

rdm takes over downloads from your browser and gets them faster and more
reliably: several connections per file, resume after a crash or restart,
queues and schedules, and HLS/DASH video streams.

This extension connects the browser to the rdm desktop app. **It needs the
free rdm app** (Windows, macOS, Linux), available from
https://github.com/Mudales/rdm (install in one line, or with an installer).

What it does:
• Takes over browser downloads of the file types you choose in rdm (video,
  audio, archives, programs, disk images, documents, or your own list).
  Alt+click a link to let the browser download it instead.
• Optionally takes PDFs, images and videos that would open in a tab.
• Shows a Download button on videos, and lists the videos and streams
  (HLS/DASH) found on the page. DRM-protected video is never downloaded.
• Right-click menu: download a link, the selected links, all links on a
  page, an image or video, or the file a tab shows; turn rdm on or off for
  a site.
• Downloads that need your login keep working: the extension passes the
  site's cookies, the page address and your browser's User-Agent to rdm.

Privacy: the extension sends nothing to the developers or any third party.
Everything goes to the rdm app on your own computer. Privacy policy:
https://github.com/Mudales/rdm/blob/main/docs/PRIVACY.md

Open source (MIT): https://github.com/Mudales/rdm

## Category

- Chrome Web Store: Tools (or Productivity → Workflow & Planning)
- Edge Add-ons: Productivity
- AMO: Download Management

## Links

- Website / homepage: https://github.com/Mudales/rdm
- Support: https://github.com/Mudales/rdm/issues
- Privacy policy: https://github.com/Mudales/rdm/blob/main/docs/PRIVACY.md
- License (AMO): MIT

## Single purpose (Chrome)

Hand downloads, videos and links from the browser to the rdm download
manager app installed on the user's computer.

## Permission justifications (Chrome "Privacy practices" tab; Edge asks similar questions)

| Permission | Justification |
|---|---|
| `nativeMessaging` | Sends the download (address, file name, cookies) to the rdm desktop app installed on the same computer. This is the extension's only output channel. |
| `downloads` | Notices downloads the browser starts so that the file types the user selected can be handed to rdm; the browser's copy is cancelled. If rdm is unreachable, the download is given back to the browser. |
| `webRequest` | Reads response headers (content type, size, file name) to detect videos, HLS/DASH streams and files opened in a tab. It only observes; it never blocks or changes requests. |
| `cookies` | Reads the cookies for the address being downloaded and passes them to rdm, because many sites refuse downloads without the user's login. Cookies are not stored by the extension and never leave the computer. |
| `contextMenus` | Adds the rdm right-click menu (download link, selected links, all links, image or media, toggle a site). |
| `storage` | Keeps the extension's own switches and a copy of the file types selected in the rdm app. |
| Host permission `<all_urls>` | Downloads and videos can come from any site: the extension must read response headers and cookies for whatever site the user downloads from, and show the video Download button on any page. |
| Remote code | No. All code is in the package. |

## Data usage (Chrome "Privacy practices")

The extension handles, but does not collect (nothing is sent to the
developer or third parties; data goes only to the rdm app on the same
computer):

- **Authentication information:** site cookies, passed to the local rdm app
  for downloads that need a login. Tick it if the form counts local use.
- **Web history:** addresses of downloaded files and the page they came
  from, passed to the local rdm app. Tick it if the form counts local use.

Certify all three: not sold to third parties; not used or transferred for
purposes unrelated to the single purpose; not used for creditworthiness or
lending.

## Firefox (AMO)

- The manifest declares `data_collection_permissions: { "required": ["none"] }`:
  no data is transmitted to the developer or third parties.
- "Does this add-on require additional software?" Yes: the rdm desktop app.

## Notes for reviewers (all stores)

The extension talks to the rdm desktop app through native messaging, so it
shows "rdm not found" until the app is installed. To test:

1. Install rdm from https://github.com/Mudales/rdm/releases/latest (or run
   `curl -fsSL https://raw.githubusercontent.com/Mudales/rdm/main/scripts/install.sh | sh`
   on macOS/Linux, or
   `irm https://raw.githubusercontent.com/Mudales/rdm/main/scripts/install.ps1 | iex`
   on Windows).
2. Open rdm → Settings → Browser → Register (the install scripts already
   did this).
3. Open a download address such as
   https://github.com/Mudales/rdm/archive/refs/tags/v0.2.0.zip (a zip, one
   of the default file types): rdm's Add dialog opens instead of the
   browser's download.

The source is the same as the GitHub repository at the tag of this version
(`extension/` folder, built with `scripts/package-extension.py`); nothing is
minified or generated.
