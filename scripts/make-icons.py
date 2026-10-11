#!/usr/bin/env python3
"""Generates the rdm app icons (PNG, ICO, ICNS) with the standard library only.

The icon is a blue rounded square with a white "download into tray" glyph.
Run from the repository root:  python3 scripts/make-icons.py
"""

import struct
import zlib
from pathlib import Path

OUT = Path("crates/rdm-gui/icons")
BASE = 512
SS = 3  # supersampling per axis

TOP = (59, 130, 246)
BOTTOM = (29, 78, 216)


def inside_round_rect(x, y, x0, y0, x1, y1, r):
    if x < x0 or x > x1 or y < y0 or y > y1:
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def inside_triangle(x, y, a, b, c):
    def side(p, q):
        return (x - q[0]) * (p[1] - q[1]) - (p[0] - q[0]) * (y - q[1])

    d1, d2, d3 = side(a, b), side(b, c), side(c, a)
    neg = d1 < 0 or d2 < 0 or d3 < 0
    pos = d1 > 0 or d2 > 0 or d3 > 0
    return not (neg and pos)


def glyph(x, y):
    shaft = 0.435 <= x <= 0.565 and 0.20 <= y <= 0.50
    head = inside_triangle(x, y, (0.27, 0.47), (0.73, 0.47), (0.50, 0.71))
    tray = inside_round_rect(x, y, 0.25, 0.77, 0.75, 0.845, 0.035)
    return shaft or head or tray


def render(size):
    px = bytearray(size * size * 4)
    n = SS * SS
    for j in range(size):
        for i in range(size):
            bg = fg = 0
            for sj in range(SS):
                for si in range(SS):
                    x = (i + (si + 0.5) / SS) / size
                    y = (j + (sj + 0.5) / SS) / size
                    if inside_round_rect(x, y, 0.06, 0.06, 0.94, 0.94, 0.2):
                        bg += 1
                        if glyph(x, y):
                            fg += 1
            if bg == 0:
                continue
            t = j / (size - 1)
            base = [TOP[k] + (BOTTOM[k] - TOP[k]) * t for k in range(3)]
            w = fg / bg
            o = (j * size + i) * 4
            for k in range(3):
                px[o + k] = round(base[k] * (1 - w) + 255 * w)
            px[o + 3] = round(255 * bg / n)
    return px


def downscale(px, size, factor):
    out_size = size // factor
    out = bytearray(out_size * out_size * 4)
    for j in range(out_size):
        for i in range(out_size):
            acc = [0, 0, 0, 0]
            for dj in range(factor):
                for di in range(factor):
                    o = ((j * factor + dj) * size + i * factor + di) * 4
                    a = px[o + 3]
                    for k in range(3):
                        acc[k] += px[o + k] * a  # premultiplied
                    acc[3] += a
            o = (j * out_size + i) * 4
            if acc[3]:
                for k in range(3):
                    out[o + k] = round(acc[k] / acc[3])
            out[o + 3] = round(acc[3] / (factor * factor))
    return out


def png(px, size):
    raw = b"".join(b"\x00" + bytes(px[r * size * 4:(r + 1) * size * 4]) for r in range(size))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def ico(images):
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, data = b"", b""
    for size, blob in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(blob), offset + len(data))
        data += blob
    return header + entries + data


def icns(images):
    body = b"".join(kind + struct.pack(">I", len(blob) + 8) + blob for kind, blob in images)
    return b"icns" + struct.pack(">I", len(body) + 8) + body


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    base = render(BASE)
    sizes = {512: base}
    for f in (2, 4, 8, 16, 32):
        sizes[BASE // f] = downscale(base, BASE, f)
    pngs = {s: png(p, s) for s, p in sizes.items()}

    (OUT / "32x32.png").write_bytes(pngs[32])
    (OUT / "128x128.png").write_bytes(pngs[128])
    (OUT / "128x128@2x.png").write_bytes(pngs[256])
    (OUT / "icon.png").write_bytes(pngs[512])
    (OUT / "icon.ico").write_bytes(ico([(s, pngs[s]) for s in (16, 32, 64, 128, 256)]))
    (OUT / "icon.icns").write_bytes(
        icns([(b"icp4", pngs[16]), (b"icp5", pngs[32]), (b"icp6", pngs[64]),
              (b"ic07", pngs[128]), (b"ic08", pngs[256]), (b"ic09", pngs[512])])
    )
    print("icons written to", OUT)


if __name__ == "__main__":
    main()
