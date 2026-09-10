#!/usr/bin/env python3
"""Generate PrincessIDE's application icons.

Tauri's `tauri::generate_context!` embeds a window icon, so the build needs real
PNG/ICO files.  Rather than commit opaque binaries with no provenance, this
script draws them from scratch using only the Python standard library (no PIL,
no ImageMagick) and is checked in next to the icons it produces.

Usage:  python3 apps/desktop/scripts/gen-icons.py
Writes: apps/desktop/src-tauri/icons/{32x32,128x128,128x128@2x}.png, icon.ico
"""

from __future__ import annotations

import struct
import zlib
from pathlib import Path

ICON_DIR = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"

BG = (0x16, 0x18, 0x1D, 0xFF)      # --bg of the IDE chrome
CROWN = (0xFF, 0x6F, 0xA5, 0xFF)   # PrincessIDE pink
BAND = (0xFF, 0xB3, 0xD1, 0xFF)    # lighter band
JEWEL = (0xFF, 0xF3, 0xF7, 0xFF)


def in_rect(x: float, y: float, x0: float, y0: float, x1: float, y1: float) -> bool:
    return x0 <= x <= x1 and y0 <= y <= y1


def in_triangle(px: float, py: float, a, b, c) -> bool:
    def sign(p, q, r):
        return (p[0] - r[0]) * (q[1] - r[1]) - (q[0] - r[0]) * (p[1] - r[1])

    d1, d2, d3 = sign((px, py), a, b), sign((px, py), b, c), sign((px, py), c, a)
    has_neg = d1 < 0 or d2 < 0 or d3 < 0
    has_pos = d1 > 0 or d2 > 0 or d3 > 0
    return not (has_neg and has_pos)


def sample(x: float, y: float) -> tuple[int, int, int, int]:
    """Colour of the icon at normalised coordinates (0..1, origin top-left)."""
    # Crown peaks: three triangles over a band.
    peaks = (
        ((0.20, 0.64), (0.38, 0.64), (0.29, 0.26)),
        ((0.38, 0.64), (0.62, 0.64), (0.50, 0.16)),
        ((0.62, 0.64), (0.80, 0.64), (0.71, 0.26)),
    )
    for a, b, c in peaks:
        if in_triangle(x, y, a, b, c):
            return CROWN
    if in_rect(x, y, 0.18, 0.62, 0.82, 0.80):
        return BAND
    # Jewels sitting on the two outer peaks.
    for cx, cy in ((0.29, 0.24), (0.71, 0.24), (0.50, 0.14)):
        if (x - cx) ** 2 + (y - cy) ** 2 <= 0.012 ** 2:
            return JEWEL
    return BG


def render(size: int) -> bytes:
    rows = bytearray()
    for py in range(size):
        rows.append(0)  # PNG filter type 0 (None)
        for px in range(size):
            # 4x4 supersampling for smooth edges at small sizes.
            acc = [0, 0, 0, 0]
            n = 0
            for sy in range(4):
                for sx in range(4):
                    x = (px + (sx + 0.5) / 4) / size
                    y = (py + (sy + 0.5) / 4) / size
                    c = sample(x, y)
                    for i in range(4):
                        acc[i] += c[i]
                    n += 1
            rows.extend(bytes(v // n for v in acc))
    return bytes(rows)


def png(size: int) -> bytes:
    raw = render(size)

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def ico(png_bytes: bytes, size: int) -> bytes:
    """Single-image ICO wrapping a PNG (supported since Vista)."""
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack(
        "<BBBBHHII",
        0 if size >= 256 else size,
        0 if size >= 256 else size,
        0,
        0,
        1,
        32,
        len(png_bytes),
        6 + 16,
    )
    return header + entry + png_bytes


def main() -> None:
    ICON_DIR.mkdir(parents=True, exist_ok=True)
    targets = {
        "32x32.png": 32,
        "128x128.png": 128,
        "128x128@2x.png": 256,
        "icon.png": 512,
    }
    for name, size in targets.items():
        data = png(size)
        (ICON_DIR / name).write_bytes(data)
        print(f"wrote {ICON_DIR / name} ({size}x{size}, {len(data)} bytes)")

    ico_data = ico(png(256), 256)
    (ICON_DIR / "icon.ico").write_bytes(ico_data)
    print(f"wrote {ICON_DIR / 'icon.ico'} (256x256, {len(ico_data)} bytes)")


if __name__ == "__main__":
    main()
