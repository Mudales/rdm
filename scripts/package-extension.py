#!/usr/bin/env python3
"""Packages the browser extension.

    python3 scripts/package-extension.py [OUT_DIR]   (default: dist)

Writes three zips named after the manifest version:

* rdm-extension-v<ver>.zip          for "Load unpacked" / development. Keeps
                                    the `key`, which pins the extension ID
                                    that rdm-host allows.
* rdm-extension-v<ver>-chromium.zip for the Chrome Web Store and Microsoft
                                    Edge Add-ons (Chrome, Brave, Edge). No
                                    `key` (the store assigns the ID) and no
                                    Firefox-only fields.
* rdm-extension-v<ver>-firefox.zip  for addons.mozilla.org. No `key` and no
                                    service worker (Firefox runs the
                                    background scripts).

The zips are reproducible: sorted entries with fixed timestamps.
"""

import json
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "extension"
SKIP_DIRS = {"test"}
SKIP_FILES = {"README.md", ".DS_Store"}
STAMP = (2020, 1, 1, 0, 0, 0)


def files():
    for path in sorted(SRC.rglob("*")):
        rel = path.relative_to(SRC)
        if path.is_dir() or rel.parts[0] in SKIP_DIRS or path.name in SKIP_FILES:
            continue
        yield rel, path


def write(out, manifest):
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for rel, path in files():
            data = path.read_bytes()
            if rel.as_posix() == "manifest.json":
                data = (json.dumps(manifest, indent=2, ensure_ascii=False) + "\n").encode()
            info = zipfile.ZipInfo(rel.as_posix(), STAMP)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            z.writestr(info, data)
    print(out.relative_to(Path.cwd()) if out.is_relative_to(Path.cwd()) else out)


def main():
    out_dir = Path(sys.argv[1] if len(sys.argv) > 1 else "dist").resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((SRC / "manifest.json").read_text())
    ver = manifest["version"]

    write(out_dir / f"rdm-extension-v{ver}.zip", manifest)

    chromium = {k: v for k, v in manifest.items() if k not in ("key", "browser_specific_settings")}
    chromium["background"] = {k: v for k, v in manifest["background"].items() if k != "scripts"}
    write(out_dir / f"rdm-extension-v{ver}-chromium.zip", chromium)

    firefox = {k: v for k, v in manifest.items() if k != "key"}
    firefox["background"] = {k: v for k, v in manifest["background"].items() if k != "service_worker"}
    write(out_dir / f"rdm-extension-v{ver}-firefox.zip", firefox)


if __name__ == "__main__":
    main()
