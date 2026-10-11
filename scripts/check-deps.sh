#!/usr/bin/env bash
# Enforces the dependency hard constraints from the project spec:
#   1. No OpenSSL anywhere in the workspace (rustls only).
#   2. rdm-core pulls in no GUI, browser or ffmpeg crates.
set -euo pipefail

fail=0

if cargo tree --workspace --locked -e normal,build --prefix none --no-dedupe \
    | grep -E '^(openssl|openssl-sys|native-tls) ' >/dev/null; then
  echo "error: OpenSSL / native-tls found in the dependency graph:" >&2
  cargo tree --workspace --locked -e normal,build -i openssl-sys >&2 || true
  fail=1
fi

forbidden_core='^(tauri|tauri-[a-z-]+|wry|tao|gtk|webkit2gtk|ffmpeg-sidecar|ffmpeg-next|eframe|egui) '
if cargo tree -p rdm-core --locked -e normal,build --prefix none --no-dedupe \
    | grep -E "$forbidden_core" >/dev/null; then
  echo "error: rdm-core depends on a GUI or ffmpeg crate:" >&2
  cargo tree -p rdm-core --locked -e normal,build --prefix none | grep -E "$forbidden_core" >&2
  fail=1
fi

if [ "$fail" -eq 0 ]; then
  echo "dependency guards: ok"
fi
exit "$fail"
