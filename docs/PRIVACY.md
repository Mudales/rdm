# Privacy policy

*Applies to the rdm desktop app and the rdm browser extension. Last
updated: 2026-10-12.*

**rdm collects no data.** Nothing is sent to the rdm developers or to anyone
else. There are no accounts, analytics, telemetry or ads.

## What the browser extension handles, and where it goes

To hand a download from your browser to the rdm app on **your own
computer**, the extension reads:

| Data | Why |
|---|---|
| The address of the file or video you download or click | So rdm can download it |
| The page's address, title, and your browser's User-Agent | Many sites refuse downloads without the referring page and a browser User-Agent; the title names saved videos |
| The site's cookies for that address | Downloads that need you to be logged in only work with your cookies |
| Response types and sizes of pages you visit (`webRequest`) | To notice videos and files that can be downloaded |

This information goes **only** to the rdm app on the same computer, through
the browser's native messaging channel (a private connection between the
browser and a program installed on your machine). It never leaves your
computer except as the download requests rdm makes to the sites you chose to
download from.

The extension stores two kinds of settings in the browser's local storage:
its own switches (for example "Send browser downloads to rdm") and a copy of
the file types you selected in the app. The list of media detected on a tab
is kept in memory (and in session storage, cleared when the browser
closes).

## What the app stores

The rdm app keeps your download list, settings and partial downloads on your
computer (in its app data folder and your download folders). Cookies
received from the browser are kept **in memory only** and are never written
to disk. Exported download lists never contain cookies.

## Network access

The app connects to the servers you download from. It also checks GitHub
for new rdm versions if automatic update checks are on (Settings → About),
and downloads ffmpeg from its publisher only when you ask it to. These
requests carry no information about you beyond what any HTTP request does,
such as your IP address and a User-Agent naming the program.

## Changes and contact

Changes to this policy are made in this file, and its history is public on
GitHub. Questions: open an issue at
<https://github.com/Mudales/rdm/issues>.
