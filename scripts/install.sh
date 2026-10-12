#!/bin/sh
# Installs rdm on macOS or Linux from the project's GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/Mudales/rdm/main/scripts/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/Mudales/rdm/main/scripts/install.sh | sh -s -- --version v0.2.0
#
# Options:
#   --version TAG   install this release (default: the latest)
#   --prefix DIR    macOS: folder for rdm.app (default: /Applications, or
#                   ~/Applications when /Applications is not writable)
#   --appimage      Linux: install the AppImage even when apt is available
#   --no-browser    do not register rdm with the browsers found
#   --uninstall     remove rdm (downloads, settings and the list are kept)
#   --from DIR      use release files from DIR instead of downloading them
#
# Every file is checked against the release's SHA256SUMS before use.

set -eu

REPO="Mudales/rdm"
TAG=""
PREFIX=""
FORCE_APPIMAGE=0
BROWSER=1
UNINSTALL=0
FROM=""

say() { printf '%s\n' "$*"; }
die() { printf 'rdm install: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || die "--version needs a tag"; TAG=$2; shift ;;
    --prefix) [ $# -ge 2 ] || die "--prefix needs a folder"; PREFIX=$2; shift ;;
    --appimage) FORCE_APPIMAGE=1 ;;
    --no-browser) BROWSER=0 ;;
    --uninstall) UNINSTALL=1 ;;
    --from) [ $# -ge 2 ] || die "--from needs a folder"; FROM=$2; shift ;;
    -h|--help) sed -n '2,17p' "$0" 2>/dev/null || true; exit 0 ;;
    *) die "unknown option $1 (see --help)" ;;
  esac
  shift
done

OS=$(uname -s)
case "$OS" in
  Darwin|Linux) ;;
  *) die "unsupported system $OS. On Windows, run scripts/install.ps1 in PowerShell." ;;
esac

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required"; }

as_root() {
  if [ "$(id -u)" -eq 0 ]; then "$@"; else need sudo; sudo "$@"; fi
}

# --- Uninstall ---------------------------------------------------------------

if [ "$UNINSTALL" -eq 1 ]; then
  if [ "$OS" = Darwin ]; then
    found=0
    for dir in "${PREFIX:-/Applications}" "$HOME/Applications"; do
      app="$dir/rdm.app"
      [ -d "$app" ] || continue
      found=1
      if pgrep -f "$app/Contents/MacOS/" >/dev/null 2>&1; then
        osascript -e 'quit app "rdm"' >/dev/null 2>&1 || true
        sleep 2
      fi
      "$app/Contents/MacOS/rdm-host" uninstall >/dev/null 2>&1 || true
      rm -rf "$app"
      say "Removed $app"
    done
    [ "$found" -eq 1 ] || say "rdm.app was not found."
  else
    if command -v dpkg >/dev/null 2>&1 && dpkg -s rdm >/dev/null 2>&1; then
      /usr/bin/rdm-host uninstall >/dev/null 2>&1 || true
      as_root apt-get remove -y rdm
    fi
    data="${XDG_DATA_HOME:-$HOME/.local/share}"
    if [ -e "$HOME/.local/bin/rdm.AppImage" ]; then
      "$data/dev.rdm.desktop/browser-host/rdm-host" uninstall >/dev/null 2>&1 || true
      rm -f "$HOME/.local/bin/rdm.AppImage" "$data/applications/rdm.desktop" "$data/icons/hicolor/128x128/apps/rdm.png"
      rm -rf "$data/dev.rdm.desktop/browser-host"
      say "Removed the rdm AppImage."
    fi
  fi
  say "Your downloads and rdm's settings were left in place."
  exit 0
fi

# --- Which release -----------------------------------------------------------

need curl
TMP=$(mktemp -d)
cleanup() {
  if [ -d "$TMP/mnt" ]; then hdiutil detach "$TMP/mnt" -quiet >/dev/null 2>&1 || true; fi
  rm -rf "$TMP"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

if [ -n "$FROM" ]; then
  [ -f "$FROM/SHA256SUMS" ] || die "$FROM/SHA256SUMS not found"
  VERSION=$(sed -n 's/.*rdm_\([0-9][0-9A-Za-z.+-]*\)_universal\.dmg$/\1/p; s/.*rdm_\([0-9][0-9A-Za-z.+-]*\)_amd64\.deb$/\1/p' "$FROM/SHA256SUMS" | head -n 1)
  [ -n "$VERSION" ] || die "no rdm package listed in $FROM/SHA256SUMS"
  TAG="v$VERSION"
else
  if [ -z "$TAG" ]; then
    latest=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") \
      || die "cannot reach GitHub"
    TAG=${latest##*/}
    case "$TAG" in v[0-9]*) ;; *) die "no release found at github.com/$REPO" ;; esac
  fi
  case "$TAG" in v*) ;; *) TAG="v$TAG" ;; esac
  VERSION=${TAG#v}
fi

fetch() {
  if [ -n "$FROM" ]; then
    [ -f "$FROM/$1" ] || die "$FROM/$1 not found"
    cp "$FROM/$1" "$TMP/$1"
  else
    curl -fL --retry 3 --progress-bar -o "$TMP/$1" "https://github.com/$REPO/releases/download/$TAG/$1" \
      || die "cannot download $1 from release $TAG"
  fi
}

verify() {
  expected=$(awk -v f="$1" '$2 == f || $2 == "*" f { print $1 }' "$TMP/SHA256SUMS")
  [ -n "$expected" ] || die "$1 is not listed in SHA256SUMS"
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$TMP/$1" | awk '{ print $1 }')
  else
    actual=$(shasum -a 256 "$TMP/$1" | awk '{ print $1 }')
  fi
  [ "$actual" = "$expected" ] || die "checksum mismatch for $1: the download is damaged or was altered"
}

say "Installing rdm $VERSION..."
fetch SHA256SUMS

# --- macOS -------------------------------------------------------------------

if [ "$OS" = Darwin ]; then
  asset="rdm_${VERSION}_universal.dmg"
  fetch "$asset"
  verify "$asset"

  if [ -n "$PREFIX" ]; then
    dest=$PREFIX
  elif [ -w /Applications ]; then
    dest=/Applications
  else
    dest="$HOME/Applications"
  fi
  mkdir -p "$dest"

  # Quit this copy if it is running (progress is saved on quit).
  if pgrep -f "$dest/rdm.app/Contents/MacOS/" >/dev/null 2>&1; then
    say "Closing the running rdm..."
    osascript -e 'quit app "rdm"' >/dev/null 2>&1 || true
    sleep 2
  fi

  mnt="$TMP/mnt"
  mkdir -p "$mnt"
  hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$mnt" "$TMP/$asset" >/dev/null \
    || die "cannot open $asset"
  rm -rf "$dest/rdm.app.new"
  ditto "$mnt/rdm.app" "$dest/rdm.app.new"
  hdiutil detach "$mnt" -quiet || true
  rmdir "$mnt" 2>/dev/null || true
  rm -rf "$dest/rdm.app"
  mv "$dest/rdm.app.new" "$dest/rdm.app"
  # Files fetched by a browser carry the quarantine flag, which makes
  # macOS refuse an unsigned app; curl does not set it, --from may.
  xattr -dr com.apple.quarantine "$dest/rdm.app" 2>/dev/null || true

  HOST="$dest/rdm.app/Contents/MacOS/rdm-host"
  INSTALLED="$dest/rdm.app"
fi

# --- Linux -------------------------------------------------------------------

if [ "$OS" = Linux ]; then
  case "$(uname -m)" in
    x86_64|amd64) ;;
    *) die "rdm is built for x86_64 Linux only; build it from source for $(uname -m)" ;;
  esac

  if [ "$FORCE_APPIMAGE" -eq 0 ] && command -v apt-get >/dev/null 2>&1 && command -v dpkg >/dev/null 2>&1; then
    asset="rdm_${VERSION}_amd64.deb"
    fetch "$asset"
    verify "$asset"
    # apt reads the file as its own user: make it readable.
    chmod 755 "$TMP"
    chmod 644 "$TMP/$asset"
    as_root apt-get install -y "$TMP/$asset"
    HOST=/usr/bin/rdm-host
    INSTALLED="/usr/bin/rdm-gui (package rdm)"
  else
    asset="rdm_${VERSION}_amd64.AppImage"
    fetch "$asset"
    verify "$asset"
    data="${XDG_DATA_HOME:-$HOME/.local/share}"
    mkdir -p "$HOME/.local/bin" "$data/applications" "$data/icons/hicolor/128x128/apps"
    install -m 755 "$TMP/$asset" "$HOME/.local/bin/rdm.AppImage"
    if [ -z "$FROM" ]; then
      curl -fsSL -o "$data/icons/hicolor/128x128/apps/rdm.png" \
        "https://raw.githubusercontent.com/$REPO/$TAG/crates/rdm-gui/icons/128x128.png" || true
    fi
    cat > "$data/applications/rdm.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=rdm
Comment=Rust Download Manager
Exec=$HOME/.local/bin/rdm.AppImage
Icon=rdm
Categories=Network;FileTransfer;
Terminal=false
EOF
    # The AppImage's own files live in a temporary mount; the browser
    # helper needs a fixed copy that knows how to start the AppImage.
    hostdir="$data/dev.rdm.desktop/browser-host"
    mkdir -p "$hostdir"
    (cd "$TMP" && "$HOME/.local/bin/rdm.AppImage" --appimage-extract usr/bin/rdm-host >/dev/null 2>&1) \
      && install -m 755 "$TMP/squashfs-root/usr/bin/rdm-host" "$hostdir/rdm-host" \
      && printf '%s' "$HOME/.local/bin/rdm.AppImage" > "$hostdir/rdm-app-path" \
      && HOST="$hostdir/rdm-host" || HOST=""
    INSTALLED="$HOME/.local/bin/rdm.AppImage"
    case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) say "Note: $HOME/.local/bin is not on your PATH; start rdm from your app menu." ;; esac
  fi
fi

# --- Browser integration -----------------------------------------------------

if [ "$BROWSER" -eq 1 ] && [ -n "${HOST:-}" ] && [ -x "$HOST" ]; then
  if "$HOST" install >/dev/null 2>&1; then
    say "Registered the browser helper with the browsers found (install the rdm extension to use it)."
  else
    say "Could not register the browser helper; use Settings > Browser in rdm."
  fi
fi

say "rdm $VERSION is installed: $INSTALLED"
