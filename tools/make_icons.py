#!/usr/bin/env python3
"""Generate Rekey's icons without any image library.

Everything is rasterised from signed-distance functions with 4x4 supersampling,
so the output is deterministic and the repository needs no binary art tooling.

Two families are produced:

* the app icon, a rounded keycap in brand colour with a swap arrow, and
* the macOS menu bar icon, which must be a *template* image: black pixels plus
  an alpha channel, so macOS can tint it for light and dark menu bars itself.
"""

import math
import struct
import zlib
from pathlib import Path

BRAND = (79, 70, 229)        # indigo
BRAND_DARK = (67, 56, 202)
WHITE = (255, 255, 255)

OUT = Path(__file__).resolve().parent.parent / "app/src-tauri/icons"
SS = 4  # supersampling factor per axis


def rounded_rect(x, y, w, h, r):
    """Signed distance to a rounded rectangle centred on the origin."""
    def f(px, py):
        dx = abs(px - x) - (w / 2 - r)
        dy = abs(py - y) - (h / 2 - r)
        ax, ay = max(dx, 0.0), max(dy, 0.0)
        return math.hypot(ax, ay) + min(max(dx, dy), 0.0) - r
    return f


def capsule(x0, y0, x1, y1, r):  # noqa: F841 - kept for future shapes

    """Signed distance to a thick line segment with round caps."""
    def f(px, py):
        vx, vy = x1 - x0, y1 - y0
        wx, wy = px - x0, py - y0
        denom = vx * vx + vy * vy
        t = 0.0 if denom == 0 else max(0.0, min(1.0, (wx * vx + wy * vy) / denom))
        return math.hypot(wx - t * vx, wy - t * vy) - r
    return f


def triangle(ax, ay, bx, by, cx, cy):
    """Inside-test for a triangle, expressed as a pseudo-distance."""
    def sign(x1, y1, x2, y2, x3, y3):
        return (x1 - x3) * (y2 - y3) - (x2 - x3) * (y1 - y3)

    def f(px, py):
        d1 = sign(px, py, ax, ay, bx, by)
        d2 = sign(px, py, bx, by, cx, cy)
        d3 = sign(px, py, cx, cy, ax, ay)
        neg = d1 < 0 or d2 < 0 or d3 < 0
        pos = d1 > 0 or d2 > 0 or d3 > 0
        return -1.0 if not (neg and pos) else 1.0
    return f


def union(*fns):
    return lambda px, py: min(f(px, py) for f in fns)


def swap_arrow(size, span=(0.27, 0.73), rows=(0.405, 0.595), weight=1.0):
    """Two opposed arrows: the universal 'these two exchange places' mark.

    The shaft stops exactly where the head begins, so the two read as one arrow
    rather than a bar with a triangle stuck to it.

    `span`, `rows` and `weight` let the same glyph be drawn inside the app
    icon's keycap or, larger and on its own, as the menu bar glyph.
    """
    s = size
    shaft = s * 0.042 * weight
    head_len = s * 0.125 * weight
    head_half = s * 0.105 * weight
    left, right = s * span[0], s * span[1]
    top, bottom = s * rows[0], s * rows[1]

    def arrow(y, tip_x, tail_x):
        direction = 1 if tip_x > tail_x else -1
        base_x = tip_x - direction * head_len
        mid_x = (tail_x + base_x) / 2
        return union(
            rounded_rect(mid_x, y, abs(base_x - tail_x), shaft * 2, shaft * 0.6),
            triangle(tip_x, y, base_x, y - head_half, base_x, y + head_half),
        )

    return union(arrow(top, right, left), arrow(bottom, left, right))


def render(size, template=False):
    """Rasterise one icon at `size` px, returning RGBA rows.

    The app icon is a brand-coloured keycap with the glyph knocked out of it.
    The menu bar icon is the bare glyph: macOS status items are conventionally
    light outline marks, and a solid filled square sits far heavier in the menu
    bar than everything beside it.
    """
    s = size
    key = rounded_rect(s / 2, s / 2, s * 0.82, s * 0.82, s * 0.22)
    if template:
        arrow = swap_arrow(s, span=(0.10, 0.90), rows=(0.34, 0.66), weight=1.30)
    else:
        arrow = swap_arrow(s)

    rows = []
    for py in range(s):
        row = bytearray()
        for px in range(s):
            r = g = b = a = 0.0
            for sy in range(SS):
                for sx in range(SS):
                    fx = px + (sx + 0.5) / SS
                    fy = py + (sy + 0.5) / SS
                    d_key = key(fx, fy)
                    d_arrow = arrow(fx, fy)
                    if template:
                        # Template images are pure black; only alpha carries
                        # the shape. The glyph stands alone, with no keycap.
                        if d_arrow <= 0:
                            a += 1.0
                    else:
                        if d_arrow <= 0 and d_key <= 0:
                            r, g, b = r + WHITE[0], g + WHITE[1], b + WHITE[2]
                            a += 1.0
                        elif d_key <= 0:
                            # Subtle vertical shade so the cap reads as a key.
                            t = fy / s
                            c = [BRAND[i] + (BRAND_DARK[i] - BRAND[i]) * t for i in range(3)]
                            r, g, b = r + c[0], g + c[1], b + c[2]
                            a += 1.0
            n = SS * SS
            alpha = a / n
            if alpha > 0:
                row += bytes((
                    0 if template else round(r / a),
                    0 if template else round(g / a),
                    0 if template else round(b / a),
                    round(alpha * 255),
                ))
            else:
                row += b"\x00\x00\x00\x00"
        rows.append(bytes(row))
    return rows


def write_ico(path, sizes):
    """Write a Windows .ico containing PNG-compressed entries.

    tauri-build requires this file to generate the Windows resource, and
    Windows Vista onward reads PNG data inside an ICO directly, so there is no
    need to emit legacy BMP entries.
    """
    images = []
    for size in sizes:
        png = png_bytes(size, render(size))
        images.append((size, png))

    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries = b""
    for size, png in images:
        # 0 in the width/height byte means 256 px.
        entries += struct.pack(
            "<BBBBHHII",
            0 if size >= 256 else size,
            0 if size >= 256 else size,
            0,            # palette size: 0 for truecolour
            0,            # reserved
            1,            # colour planes
            32,           # bits per pixel
            len(png),
            offset,
        )
        offset += len(png)

    path.write_bytes(header + entries + b"".join(png for _, png in images))
    return path.stat().st_size


def png_bytes(size, rows):
    """The PNG file for `rows`, as bytes."""
    raw = b"".join(b"\x00" + row for row in rows)

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def write_png(path, size, rows):
    png = png_bytes(size, rows)
    path.write_bytes(png)
    return len(png)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    # Sizes Tauri expects for the bundled application icon.
    for size, name in [
        (32, "32x32.png"),
        (128, "128x128.png"),
        (256, "128x128@2x.png"),
        (512, "icon.png"),
    ]:
        n = write_png(OUT / name, size, render(size))
        print(f"  {name:<18} {size:>4}px  {n:>7} bytes")

    # Menu bar / tray: template images, 1x and 2x.
    for size, name in [(22, "trayTemplate.png"), (44, "trayTemplate@2x.png")]:
        n = write_png(OUT / name, size, render(size, template=True))
        print(f"  {name:<18} {size:>4}px  {n:>7} bytes  (template)")

    # Windows resource icon, required by tauri-build.
    n = write_ico(OUT / "icon.ico", [16, 24, 32, 48, 64, 128, 256])
    print(f"  {'icon.ico':<18} multi  {n:>7} bytes")


if __name__ == "__main__":
    main()
