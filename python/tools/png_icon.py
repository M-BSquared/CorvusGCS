#!/usr/bin/env python3
"""Fit a PNG into a square icon of a given size, with the standard library only.

The Linux and Windows builds need the logo as a 256 by 256 icon. ImageMagick
does it where it is installed; this is the same job for a build host without
it, so the build never depends on a tool it cannot name in its first second.

Reads 8 bit RGB and RGBA PNGs without interlacing (the logo is one), scales
to fit with a box filter on premultiplied colour, centres on a transparent
square and writes an RGBA PNG.

    python3 tools/png_icon.py <in.png> <out.png> [size]
"""
from __future__ import annotations

import struct
import sys
import zlib
from pathlib import Path

SIGNATURE = b"\x89PNG\r\n\x1a\n"


def read_png(data: bytes) -> tuple[int, int, list[bytearray]]:
    """Width, height and rows of RGBA bytes."""
    if not data.startswith(SIGNATURE):
        raise ValueError("not a PNG")
    pos, idat, header = len(SIGNATURE), b"", None
    while pos < len(data):
        size, kind = struct.unpack(">I4s", data[pos : pos + 8])
        body = data[pos + 8 : pos + 8 + size]
        pos += 12 + size
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    if header is None:
        raise ValueError("no IHDR")
    width, height, depth, colour, _, _, interlace = header
    if depth != 8 or colour not in (2, 6) or interlace:
        raise ValueError("only 8 bit RGB/RGBA PNGs without interlacing are read")
    bpp = 3 if colour == 2 else 4
    raw = zlib.decompress(idat)
    stride = width * bpp
    rows: list[bytearray] = []
    prev = bytearray(stride)
    for y in range(height):
        start = y * (stride + 1)
        kind_byte = raw[start]
        line = bytearray(raw[start + 1 : start + 1 + stride])
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if kind_byte == 1:
                line[i] = (line[i] + a) & 255
            elif kind_byte == 2:
                line[i] = (line[i] + b) & 255
            elif kind_byte == 3:
                line[i] = (line[i] + ((a + b) >> 1)) & 255
            elif kind_byte == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 255
        prev = line
        if bpp == 3:
            rgba = bytearray(width * 4)
            for x in range(width):
                rgba[x * 4 : x * 4 + 3] = line[x * 3 : x * 3 + 3]
                rgba[x * 4 + 3] = 255
            rows.append(rgba)
        else:
            rows.append(line)
    return width, height, rows


def write_png(width: int, height: int, rows: list[bytearray]) -> bytes:
    def chunk(kind: bytes, body: bytes) -> bytes:
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    raw = b"".join(b"\x00" + bytes(r) for r in rows)
    return (
        SIGNATURE
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def fit(width: int, height: int, rows: list[bytearray], size: int) -> list[bytearray]:
    """`rows` scaled to fit `size` by `size`, centred on a transparent square."""
    scale = min(size / width, size / height)
    nw, nh = max(1, round(width * scale)), max(1, round(height * scale))
    out = [bytearray(size * 4) for _ in range(size)]
    ox, oy = (size - nw) // 2, (size - nh) // 2
    for dy in range(nh):
        y0, y1 = dy * height / nh, (dy + 1) * height / nh
        for dx in range(nw):
            x0, x1 = dx * width / nw, (dx + 1) * width / nw
            r = g = b = a = weight = 0.0
            for sy in range(int(y0), min(height, int(y1 - 1e-9) + 1)):
                wy = min(y1, sy + 1) - max(y0, sy)
                row = rows[sy]
                for sx in range(int(x0), min(width, int(x1 - 1e-9) + 1)):
                    w = wy * (min(x1, sx + 1) - max(x0, sx))
                    pa = row[sx * 4 + 3] / 255.0
                    r += row[sx * 4] * pa * w
                    g += row[sx * 4 + 1] * pa * w
                    b += row[sx * 4 + 2] * pa * w
                    a += pa * w
                    weight += w
            if weight and a > 0:
                px = out[oy + dy]
                i = (ox + dx) * 4
                px[i] = min(255, round(r / a))
                px[i + 1] = min(255, round(g / a))
                px[i + 2] = min(255, round(b / a))
                px[i + 3] = min(255, round(a / weight * 255))
    return out


def main(argv: list[str]) -> int:
    if len(argv) not in (3, 4):
        print(__doc__, file=sys.stderr)
        return 2
    size = int(argv[3]) if len(argv) == 4 else 256
    width, height, rows = read_png(Path(argv[1]).read_bytes())
    Path(argv[2]).write_bytes(write_png(size, size, fit(width, height, rows, size)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
