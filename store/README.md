# Publishing the extension in the browser stores

| Store | Covers | Account | Package |
|---|---|---|---|
| [Chrome Web Store](https://chrome.google.com/webstore/devconsole) | Chrome, Brave (installs from this store), Edge (optional) | Google account + one-time US$5 registration | `rdm-extension-v<ver>-chromium.zip` |
| [Microsoft Edge Add-ons](https://partner.microsoft.com/dashboard/microsoftedge/overview) | Edge | Microsoft account, free Partner Center registration | `rdm-extension-v<ver>-chromium.zip` (same file) |
| [Firefox Add-ons (AMO)](https://addons.mozilla.org/developers/) | Firefox | Firefox account, free | `rdm-extension-v<ver>-firefox.zip` |

Every release attaches the three packages, built by
[`scripts/package-extension.py`](../scripts/package-extension.py). To build
them locally: `python3 scripts/package-extension.py dist`.

The store text (name, descriptions, permission justifications, privacy
answers, notes for reviewers) is in [listing.md](listing.md). The privacy
policy is [docs/PRIVACY.md](../docs/PRIVACY.md), at
https://github.com/Mudales/rdm/blob/main/docs/PRIVACY.md.

## Images ([images/](images))

| File | Size | Use |
|---|---|---|
| `popup.png`, `capture.png`, `batch.png` | 1280×800 | Screenshots (all three stores) |
| `promo-small.png` | 440×280 | Chrome "small promo tile", Edge "small promotional tile" |
| `logo-300.png` | 300×300 | Edge store logo |
| `../extension/icons/128.png` | 128×128 | Chrome store icon |

They are rendered from the pages in [shots/](shots), which show the real
popup and app UI with sample data: `python3 store/render.py` (needs Chrome,
Chromium, Edge or Brave installed).

## The extension ID (Chrome and Edge)

The desktop helper `rdm-host` only answers extensions whose ID it knows.
Each store assigns its own ID, so the IDs must be added to rdm-host
(`CHROME_EXTENSION_IDS` in `crates/rdm-host/src/lib.rs`) and shipped in an
rdm release **before** people install the store version. Otherwise it shows
"rdm not found". Firefox keeps the ID from the manifest (`rdm@rdm.dev`),
which rdm-host already allows.

## Steps

1. **Chrome Web Store**
   1. Register at the developer console (pay the US$5 fee once). Verify
      the contact email.
   2. *New item* → upload `rdm-extension-v<ver>-chromium.zip`. Don't
      submit yet.
   3. Copy the **Item ID** (32 letters) from the item's page.
   4. Store listing: text from listing.md; category; language English;
      screenshots; small promo tile; icon.
   5. Privacy practices: single purpose, permission justifications and
      data usage from listing.md; privacy policy URL.
   6. Distribution: Public, all regions.
2. **Edge Add-ons**
   1. Register in Partner Center (Microsoft Edge program).
   2. *Create new extension* → upload the same chromium zip.
   3. Availability: Public, all markets.
   4. Properties: category Productivity, privacy policy URL ("Yes, it
      accesses personal information": cookies, passed to the local app
      only), website and support URLs.
   5. Store listings → English: description from listing.md, logo
      (`logo-300.png`), small promotional tile, screenshots.
   6. Note the extension's **ID** shown for the item (the 32-letter
      "CRX ID").
3. **Send the two IDs** so they can be added to rdm-host and released
   (a patch release; installed copies get it through the in-app updater).
4. **Submit** both for review, with the "Notes for reviewers" from
   listing.md (Chrome: in the submission dialog; Edge: "Notes for
   certification").
5. **Firefox (AMO)**
   1. Sign in at the developer hub → *Submit a New Add-on* → *On this
      site* (listed).
   2. Upload `rdm-extension-v<ver>-firefox.zip`. The validator may warn
      about `<all_urls>` and `nativeMessaging`; that's expected.
   3. Source code: "No" (nothing is minified, generated or transpiled).
   4. Listing: summary, description, category *Download Management*,
      license MIT, privacy policy (paste docs/PRIVACY.md), homepage and
      support URLs, screenshots. Tick that it requires other software (the
      rdm app).
   5. Notes to reviewer: from listing.md.

Reviews take from a day to a few weeks. Chrome and Edge review extensions
with `<all_urls>` and native messaging more closely; the justifications in
listing.md answer the usual questions.

## Updates

For a new version, bump `extension/manifest.json`, tag the release, and
upload the new `-chromium.zip` (Chrome, Edge) and `-firefox.zip` (AMO) from
the release page to each store's existing item.
