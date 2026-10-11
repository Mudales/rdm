# Releasing rdm

## What a release contains

| Platform | Files | Built on |
|---|---|---|
| macOS (Apple Silicon + Intel) | `rdm_<ver>_universal.dmg`, `rdm.app.tar.gz` | `macos-latest`, `--target universal-apple-darwin` |
| Windows x64 | `rdm_<ver>_x64_en-US.msi`, `rdm_<ver>_x64-setup.exe` (NSIS, per-user install) | `windows-latest` |
| Linux x64 | `rdm_<ver>_amd64.AppImage`, `rdm_<ver>_amd64.deb` | `ubuntu-22.04`, the oldest supported runner, so the binaries work on older glibc |
| Browser extension | `rdm-extension-v<ver>.zip` | — |
| Checksums | `SHA256SUMS` (every asset above) | — |

Every app bundle includes `rdm-host`, the native messaging helper, next to
the main executable, so Settings → Browser → Register works right after
installing. ffmpeg is **not** bundled: it is GPL-licensed, and rdm downloads
it only when the user asks (Settings → Media).

## Cutting a release

1. Make sure CI is green on `main`.
2. Bump the version in `Cargo.toml` (`[workspace.package] version`),
   `crates/rdm-gui/tauri.conf.json` and `extension/manifest.json`, and
   add a section to `CHANGELOG.md`.
3. Commit, then tag and push:

   ```bash
   git tag -a v0.1.0 -m "rdm v0.1.0"
   ```

   ```bash
   git push origin v0.1.0
   ```

4. The **Release** workflow
   ([`.github/workflows/release.yml`](../.github/workflows/release.yml))
   builds the three bundles in parallel and uploads them to a **draft**
   release. It then adds the extension zip and `SHA256SUMS`.
5. Check the draft (the assets are listed, and the notes are right), then
   publish it:

   ```bash
   gh release edit v0.1.0 --draft=false
   ```

To try the bundling locally (macOS shown):

```bash
cargo install tauri-cli --version "^2" --locked
```

```bash
scripts/build-host-sidecar.sh
```

```bash
cd crates/rdm-gui && cargo tauri build --config tauri.bundle.conf.json
```

## Installing unsigned builds

v0.1.0 is not code-signed, so operating systems warn on first launch.

- **macOS:** open the `.dmg` and drag rdm to Applications. The first time,
  right-click rdm → **Open** → **Open**. If macOS says the app "is damaged",
  that is Gatekeeper's quarantine flag on an unsigned download. Clear it
  with this command, then open rdm normally:

  ```bash
  xattr -dr com.apple.quarantine /Applications/rdm.app
  ```

- **Windows:** SmartScreen shows "Windows protected your PC". Click
  **More info** → **Run anyway**. The NSIS installer installs for the
  current user only, so no admin rights are needed.
- **Linux:** `sudo apt install ./rdm_<ver>_amd64.deb`, or make the AppImage
  executable (`chmod +x rdm_*.AppImage`) and run it.

Always check downloads against `SHA256SUMS` (`shasum -a 256 -c SHA256SUMS`
on macOS, `sha256sum -c SHA256SUMS --ignore-missing` on Linux).

## Code signing (when certificates are available)

The release workflow already passes the signing secrets to `tauri-action`.
Adding them to the repository (Settings → Secrets and variables → Actions)
turns signing on with no workflow changes.

### macOS: Developer ID and notarization

Requires an Apple Developer Program membership ($99/year).

1. In Xcode or on developer.apple.com, create a **Developer ID
   Application** certificate. Export it with its private key as a `.p12`.
2. Add these secrets:

   | Secret | Value |
   |---|---|
   | `APPLE_CERTIFICATE` | `base64 -i cert.p12` output |
   | `APPLE_CERTIFICATE_PASSWORD` | the `.p12` export password |
   | `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
   | `APPLE_ID` | the Apple ID email |
   | `APPLE_PASSWORD` | an app-specific password from appleid.apple.com |
   | `APPLE_TEAM_ID` | the 10-character team ID |

3. Tauri signs the app and the `rdm-host` sidecar with the hardened runtime,
   submits the build to Apple's notary service, and staples the ticket. The
   result opens without warnings.

### Windows: Authenticode

You need one of:

- **An OV/EV code-signing certificate** (`.pfx`). Import it in a workflow
  step before `tauri-action`, and set
  `bundle.windows.certificateThumbprint`, `digestAlgorithm: "sha256"` and
  `timestampUrl` (e.g. `http://timestamp.digicert.com`) in
  `tauri.bundle.conf.json`. EV certificates stored on hardware tokens can't
  run in CI, so use a cloud HSM.
- **Azure Trusted Signing:** set `bundle.windows.signCommand` to the
  `trusted-signing-cli` invocation, and provide `AZURE_CLIENT_ID`,
  `AZURE_CLIENT_SECRET` and `AZURE_TENANT_ID` as secrets.

SmartScreen reputation also builds up with download volume, even for signed
files.

### Linux

AppImages and `.deb` files are usually distributed unsigned. To sign them,
publish a detached GPG signature (`gpg --detach-sign --armor`) of
`SHA256SUMS`.

## Browser extension stores

The release zip is for *Load unpacked* (Chrome/Edge/Brave) and
*about:debugging* (Firefox). The `key` in `manifest.json` pins the
development ID `dacdblamhhaedfahmbcllbpgjejdaddi`, which `rdm-host`
allows.

- **Chrome Web Store / Edge Add-ons:** remove the `key` field before
  uploading. The store assigns a new ID. Add that ID to
  `CHROME_EXTENSION_IDS` in `crates/rdm-host/src/lib.rs` so the installed
  helper accepts it.
- **Firefox Add-ons:** submit as is. The `gecko.id` (`rdm@rdm.dev`) is what
  `rdm-host` allows. Listed add-ons are signed by Mozilla; unsigned ones
  only load temporarily.
