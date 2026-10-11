#!/usr/bin/env bash
# Builds rdm-host in release mode and places it where Tauri's bundler
# expects the external binary: crates/rdm-gui/binaries/rdm-host-<triple>.
#
#   scripts/build-host-sidecar.sh                       # host target
#   scripts/build-host-sidecar.sh x86_64-pc-windows-msvc
#   scripts/build-host-sidecar.sh universal-apple-darwin  # lipo of both Macs
set -euo pipefail

cd "$(dirname "$0")/.."
target="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
out="crates/rdm-gui/binaries"
mkdir -p "$out"

build() {
  cargo build --release --locked -p rdm-host --target "$1"
}

if [ "$target" = "universal-apple-darwin" ]; then
  build aarch64-apple-darwin
  build x86_64-apple-darwin
  lipo -create -output "$out/rdm-host-universal-apple-darwin" \
    target/aarch64-apple-darwin/release/rdm-host \
    target/x86_64-apple-darwin/release/rdm-host
  # tauri-build also looks for the per-architecture names.
  cp target/aarch64-apple-darwin/release/rdm-host "$out/rdm-host-aarch64-apple-darwin"
  cp target/x86_64-apple-darwin/release/rdm-host "$out/rdm-host-x86_64-apple-darwin"
else
  build "$target"
  ext=""
  case "$target" in *windows*) ext=".exe" ;; esac
  cp "target/$target/release/rdm-host$ext" "$out/rdm-host-$target$ext"
fi
ls -l "$out"
