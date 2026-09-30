"""Elevation tiles the cache does not hold, derived from ones it does.

Why this exists
---------------
3D mode drapes the map over a raster-dem source. MapLibre asks for elevation
tiles at the zoom it is drawing, and offline a tile that was never downloaded
is a 404. MapLibre then fills that square from whatever coarser tile it
happens to have loaded, and its neighbour from a different one, so two
adjacent squares of ground come from elevation grids of different resolution.
Their edges do not meet: the terrain shows steps and cracks tens of metres
high along tile borders, exactly over the steep ground where an operator is
looking hardest.

The fix is to never answer a DEM request with a hole when a coarser tile
covering the same ground is on disk. The nearest cached ancestor is decoded,
the requested square is cut out of it and resampled bilinearly to full tile
size, and the result is served as an ordinary tile at the requested zoom.
Every tile MapLibre sees is then at the zoom it asked for, so it can stitch
neighbours together the way it does for real tiles.

What it is not
--------------
Derived tiles are never written to the tile cache. They are an interpolation,
not data: storing one would hide the real tile the next time the laptop is
online. They are served with a short browser cache lifetime for the same
reason.

Encoding
--------
Both RGB height packings MapLibre reads, Terrarium and Mapbox Terrain-RGB,
are linear in the packed 24-bit integer ``R << 16 | G << 8 | B``. Resampling
that integer is therefore resampling the height itself, and this module needs
no knowledge of which packing a source uses.

stdlib only: ``zlib`` for the PNG streams, ``array`` for the pixel grids.
"""
from __future__ import annotations

import math
import struct
import sys
import threading
import zlib
from array import array
from collections import OrderedDict
from collections.abc import Callable

_PNG_MAGIC = b"\x89PNG\r\n\x1a\n"

# Colour type -> bytes per pixel, for the 8-bit, non-interlaced PNGs elevation
# servers publish. Anything else is declined rather than half-decoded: a DEM
# read wrong is a mountain in the wrong place, and a 404 is merely flat.
_CHANNELS = {2: 3, 6: 4}

# A 32-bit unsigned array typecode. "I" is 4 bytes on every platform Corvus
# ships on; "L" is the fallback for one where it is not.
_U32 = "I" if array("I").itemsize == 4 else "L"

# How many decoded tiles to keep. A view derives its tiles from a handful of
# coarse ancestors and reads the edges of the real tiles around them, and
# each decoded 256 px grid is 256 KiB.
DECODED_CACHE_SIZE = 32
# How many derived tiles to keep. The 3D view asks for the same elevation tile
# twice (terrain and hillshade are separate sources), and a pan back over the
# same ground asks again.
DERIVED_CACHE_SIZE = 48


class _Grid:
    """A decoded elevation tile: packed 24-bit heights, row-major."""

    __slots__ = ("width", "height", "values")

    def __init__(self, width: int, height: int, values: array) -> None:
        self.width = width
        self.height = height
        self.values = values


def _paeth(a: int, b: int, c: int) -> int:
    p = a + b - c
    pa = p - a if p > a else a - p
    pb = p - b if p > b else b - p
    pc = p - c if p > c else c - p
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def _unfilter(raw: bytes, width: int, height: int, bpp: int) -> bytes | None:
    """Undo the per-scanline PNG filters. None on a malformed stream."""
    stride = width * bpp
    if len(raw) < height * (stride + 1):
        return None
    out = bytearray(height * stride)
    prev = bytearray(stride)
    pos = 0
    for row in range(height):
        kind = raw[pos]
        line = bytearray(raw[pos + 1:pos + 1 + stride])
        pos += stride + 1
        if kind == 0:
            pass
        elif kind == 1:
            for i in range(bpp, stride):
                line[i] = (line[i] + line[i - bpp]) & 255
        elif kind == 2:
            line = bytearray((a + b) & 255 for a, b in zip(line, prev))
        elif kind == 3:
            for i in range(stride):
                left = line[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + ((left + prev[i]) >> 1)) & 255
        elif kind == 4:
            for i in range(stride):
                if i >= bpp:
                    line[i] = (line[i] + _paeth(line[i - bpp], prev[i], prev[i - bpp])) & 255
                else:
                    line[i] = (line[i] + prev[i]) & 255
        else:
            return None
        out[row * stride:(row + 1) * stride] = line
        prev = line
    return bytes(out)


def decode(data: bytes) -> _Grid | None:
    """Decode an RGB(A) elevation PNG into packed heights, or None.

    Only 8-bit, non-interlaced truecolour images are accepted, which is what
    every public elevation tile service serves. Alpha, when present, is
    ignored: the height is in the colour channels alone.
    """
    if not data or data[:8] != _PNG_MAGIC:
        return None
    width = height = channels = 0
    idat: list[bytes] = []
    pos = 8
    try:
        while pos + 8 <= len(data):
            (length,) = struct.unpack(">I", data[pos:pos + 4])
            kind = data[pos + 4:pos + 8]
            body = data[pos + 8:pos + 8 + length]
            pos += 12 + length
            if kind == b"IHDR":
                width, height, depth, colour, _comp, _flt, interlace = struct.unpack(
                    ">IIBBBBB", body)
                if depth != 8 or interlace != 0 or colour not in _CHANNELS:
                    return None
                channels = _CHANNELS[colour]
            elif kind == b"IDAT":
                idat.append(body)
            elif kind == b"IEND":
                break
        if not (width and height and channels and idat):
            return None
        pixels = _unfilter(zlib.decompress(b"".join(idat)), width, height, channels)
    except (struct.error, zlib.error, ValueError):
        return None
    if pixels is None:
        return None
    count = width * height
    packed = bytearray(4 * count)
    packed[1::4] = pixels[0::channels]
    packed[2::4] = pixels[1::channels]
    packed[3::4] = pixels[2::channels]
    values = array(_U32)
    values.frombytes(bytes(packed))
    if sys.byteorder == "little":
        values.byteswap()
    return _Grid(width, height, values)


def _chunk(kind: bytes, body: bytes) -> bytes:
    return (struct.pack(">I", len(body)) + kind + body
            + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF))


def encode(width: int, height: int, values: array) -> bytes:
    """Encode packed 24-bit heights as an 8-bit RGB PNG."""
    words = array(_U32, values)
    if sys.byteorder == "little":
        words.byteswap()
    packed = words.tobytes()
    count = width * height
    rgb = bytearray(3 * count)
    rgb[0::3] = packed[1::4]
    rgb[1::3] = packed[2::4]
    rgb[2::3] = packed[3::4]
    stride = 3 * width
    raw = b"".join(b"\x00" + bytes(rgb[r * stride:(r + 1) * stride]) for r in range(height))
    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (_PNG_MAGIC + _chunk(b"IHDR", header)
            + _chunk(b"IDAT", zlib.compress(raw, 6)) + _chunk(b"IEND", b""))


def _axis(size: int, scale: int, offset: int,
          out_size: int | None = None) -> list[tuple[int, int, float]]:
    """Source index pair and weight for every output pixel along one axis.

    Output pixel centres are mapped into the ancestor's pixel grid, so the
    sampling is continuous across the border between two tiles cut from the
    same ancestor: no seam between siblings. At the ancestor's own edge the
    nearest row is held, which is what MapLibre does with a real tile's
    border until the neighbour arrives. *out_size* differs from *size* only
    when the output is a different pixel size (a 256 px tile re-cut at 512).
    """
    count = size if out_size is None else out_size
    out = []
    last = size - 1
    for i in range(count):
        s = (offset * count + i + 0.5) / scale * (size / count) - 0.5
        s = min(max(s, 0.0), float(last))
        lo = int(math.floor(s))
        hi = lo + 1 if lo < last else lo
        out.append((lo, hi, s - lo))
    return out


def _resample_float(grid: _Grid, dz: int, sub_x: int, sub_y: int,
                    out_width: int | None = None,
                    out_height: int | None = None) -> list[float]:
    """resample() before rounding, so corrections can be added first."""
    scale = 1 << dz
    width, height, values = grid.width, grid.height, grid.values
    cols = _axis(width, scale, sub_x, out_width)
    rows = _axis(height, scale, sub_y, out_height)
    horizontal: dict[int, list[float]] = {}

    def along(row: int) -> list[float]:
        line = horizontal.get(row)
        if line is None:
            base = row * width
            src = values[base:base + width]
            line = [src[lo] + (src[hi] - src[lo]) * f for lo, hi, f in cols]
            horizontal[row] = line
        return line

    out: list[float] = []
    for lo, hi, f in rows:
        a = along(lo)
        if hi == lo or f == 0.0:
            out.extend(a)
            continue
        b = along(hi)
        out.extend(p + (q - p) * f for p, q in zip(a, b))
    return out


def _pack(values: list[float]) -> array:
    top = (1 << 24) - 1
    return array(_U32, (min(top, max(0, int(v + 0.5))) for v in values))


def resample(grid: _Grid, dz: int, sub_x: int, sub_y: int) -> array:
    """The square *dz* zooms below *grid* at (*sub_x*, *sub_y*), full size.

    *sub_x* / *sub_y* are the child's column and row inside the ancestor,
    in ``0 .. 2**dz - 1``. Bilinear, on the packed heights, rounded to the
    nearest packed unit (1/256 m for Terrarium).
    """
    return _pack(_resample_float(grid, dz, sub_x, sub_y))


def _cut_edge(grid: _Grid, dz: int, sub_x: int, sub_y: int, side: str) -> list[float]:
    """One edge of the square _resample_float would cut, without the rest."""
    scale = 1 << dz
    width, values = grid.width, grid.values
    cols = _axis(width, scale, sub_x)
    rows = _axis(grid.height, scale, sub_y)
    if side in ("w", "e"):
        col = cols[0] if side == "w" else cols[-1]
        points = [(row, col) for row in rows]
    else:
        row = rows[0] if side == "n" else rows[-1]
        points = [(row, col) for col in cols]
    out = []
    for (rlo, rhi, fy), (clo, chi, fx) in points:
        a, b = values[rlo * width + clo], values[rlo * width + chi]
        c, d = values[rhi * width + clo], values[rhi * width + chi]
        upper = a + (b - a) * fx
        lower = c + (d - c) * fx
        out.append(upper + (lower - upper) * fy)
    return out


def _edge(values, width: int, height: int, side: str) -> list[float]:
    """One edge of a full grid, in order along it."""
    if side == "w":
        return [values[r * width] for r in range(height)]
    if side == "e":
        return [values[r * width + width - 1] for r in range(height)]
    if side == "n":
        return [values[i] for i in range(width)]
    return [values[(height - 1) * width + i] for i in range(width)]


def _blend_edges(values: list[float], width: int, height: int,
                 deltas: dict[str, list[float]]) -> list[float]:
    """Add a correction that moves each edge by its delta and fades across.

    Each side's correction is a linear ramp over the whole tile: all of it at
    that edge, none of it at the opposite one. The ramps are summed. A tile
    between a real neighbour and one of its own kind therefore meets the real
    one and leaves the other alone, and a slope replaces the step.

    Why not something that pins every edge exactly (a Coons patch): the
    derived neighbours above and below are bent by their OWN real
    neighbours, which this tile cannot see. Pinning to their unbent edges
    squeezes the whole correction into a strip along the real edge, which is
    the wall again, only thinner. A sum of full-width ramps gives each shared
    edge the same correction from both sides, up to the height difference
    between two adjacent pixels.
    """
    zero_v = [0.0] * height
    zero_u = [0.0] * width
    left = deltas.get("w", zero_v)
    right = deltas.get("e", zero_v)
    top = deltas.get("n", zero_u)
    bottom = deltas.get("s", zero_u)
    us = [i / (width - 1) for i in range(width)]
    out: list[float] = []
    for j in range(height):
        v = j / (height - 1)
        a = left[j]
        span = right[j] - a
        iv = 1 - v
        row = values[j * width:(j + 1) * width]
        out.extend(h + a + span * u + t * iv + b * v
                   for h, u, t, b in zip(row, us, top, bottom))
    return out


# Packed 24-bit value to metres, per RGB height packing:
# height = packed * scale + offset.
ENCODINGS: dict[str, tuple[float, float]] = {
    "terrarium": (1 / 256, -32768.0),
    "mapbox": (0.1, -10000.0),
}


def transcode(png: bytes, from_encoding: str, to_encoding: str, size: int) -> bytes | None:
    """The same ground, in another height packing and at another pixel size.

    How a keyed service's elevation tile is stood in for by the free one when
    the keyed tile cannot be had: the free tile is re-packed into the keyed
    source's encoding and re-cut to its tile size, because MapLibre requires
    both to be uniform within one source (it refuses to stitch neighbours of
    different sizes). None when either packing is unknown or *png* unreadable.
    """
    if from_encoding not in ENCODINGS or to_encoding not in ENCODINGS or size < 2:
        return None
    grid = decode(png)
    if grid is None:
        return None
    values = _resample_float(grid, 0, 0, 0, size, size)
    scale_in, offset_in = ENCODINGS[from_encoding]
    scale_out, offset_out = ENCODINGS[to_encoding]
    if (scale_in, offset_in) != (scale_out, offset_out):
        factor = scale_in / scale_out
        shift = (offset_in - offset_out) / scale_out
        values = [v * factor + shift for v in values]
    return encode(size, size, _pack(values))


def overzoom(ancestor_png: bytes, dz: int, sub_x: int, sub_y: int) -> bytes | None:
    """One-shot: derive a child tile from an ancestor PNG. None if unreadable."""
    if dz < 1 or not (0 <= sub_x < (1 << dz)) or not (0 <= sub_y < (1 << dz)):
        return None
    grid = decode(ancestor_png)
    if grid is None:
        return None
    return encode(grid.width, grid.height, resample(grid, dz, sub_x, sub_y))


# Each side of a tile: the neighbour's offset, and which of ITS sides is ours.
_SIDES = {"w": (-1, 0, "e"), "e": (1, 0, "w"), "n": (0, -1, "s"), "s": (0, 1, "n")}

_GetTile = Callable[[int, int, int], "bytes | None"]


class DemFallback:
    """Answers a missing elevation tile from the nearest cached ancestor.

    Resampling alone leaves a wall wherever the derived tile meets ground
    known at a different resolution: a real tile next door, or a neighbour
    derived from a coarser ancestor. At the edge of a downloaded area that
    wall is hundreds of metres high, drawn with the imagery smeared down it.
    So each derived tile also reads the edges of its four neighbours and
    bends its own edges to meet them: fully to a real tile, which is not
    going to move, and halfway to a derived one (taken before its own
    bending), which bends the other half by the same rule. The correction
    fades across the tile, which turns the wall into a slope.

    Shared by every handler thread. Holds nothing but two bounded in-memory
    caches, so there is nothing to close on shutdown. The lock guards only the
    caches; decoding and resampling run outside it, so two threads deriving
    different tiles do not wait on each other beyond the GIL.
    """

    def __init__(self, decoded_size: int = DECODED_CACHE_SIZE,
                 derived_size: int = DERIVED_CACHE_SIZE) -> None:
        self._lock = threading.Lock()
        self._decoded: OrderedDict[tuple, _Grid] = OrderedDict()
        self._derived: OrderedDict[tuple, bytes] = OrderedDict()
        self._decoded_size = max(1, int(decoded_size))
        self._derived_size = max(1, int(derived_size))

    def tile(self, source: str, z: int, x: int, y: int, get_tile: _GetTile) -> bytes | None:
        """A PNG for (*z*, *x*, *y*) derived from a cached ancestor, or None.

        *get_tile* reads the cache for one tile of *source*; it may raise, which
        is treated as a miss. None means no ancestor is cached at all, which
        is the one case where the ground genuinely is unknown.
        """
        key = (source, z, x, y)
        with self._lock:
            hit = self._derived.get(key)
            if hit is not None:
                self._derived.move_to_end(key)
                return hit
        found = self._finest(source, z, x, y, get_tile)
        if found is None:
            return None
        grid, dz, sub_x, sub_y = found
        width, height = grid.width, grid.height
        values = _resample_float(grid, dz, sub_x, sub_y)
        count = 1 << z
        deltas: dict[str, list[float]] = {}
        for side, (dx, dy, theirs) in _SIDES.items():
            ny = y + dy
            if not 0 <= ny < count:
                continue
            seen = self._neighbour_edge(source, z, (x + dx) % count, ny, theirs, get_tile)
            if seen is None:
                continue
            line, real = seen
            mine = _edge(values, width, height, side)
            if len(line) != len(mine):
                continue
            share = 1.0 if real else 0.5
            deltas[side] = [(other - own) * share for own, other in zip(mine, line)]
        if deltas:
            values = _blend_edges(values, width, height, deltas)
        blob = encode(width, height, _pack(values))
        with self._lock:
            self._derived[key] = blob
            self._derived.move_to_end(key)
            while len(self._derived) > self._derived_size:
                self._derived.popitem(last=False)
        return blob

    def forget(self, source: str | None = None) -> None:
        """Drop what is remembered, for one source or all of them.

        For when the cache under it changes shape: a download that adds finer
        tiles, or an area deleted from disk.
        """
        with self._lock:
            if source is None:
                self._decoded.clear()
                self._derived.clear()
                return
            for store in (self._decoded, self._derived):
                for key in [k for k in store if k[0] == source]:
                    del store[key]

    def _finest(self, source: str, z: int, x: int, y: int,
                get_tile: _GetTile) -> tuple[_Grid, int, int, int] | None:
        """The nearest cached ancestor of a tile, and where the tile sits in it."""
        for dz in range(1, z + 1):
            grid = self._grid(source, z - dz, x >> dz, y >> dz, get_tile)
            if grid is not None:
                mask = (1 << dz) - 1
                return grid, dz, x & mask, y & mask
        return None

    def _neighbour_edge(self, source: str, z: int, x: int, y: int, side: str,
                        get_tile: _GetTile) -> tuple[list[float], bool] | None:
        """One edge of a tile as it will be served, and whether it is real.

        A derived neighbour's edge is taken before its own blending, which is
        what makes the two halfway corrections land on the same line.
        """
        grid = self._grid(source, z, x, y, get_tile)
        if grid is not None:
            return _edge(grid.values, grid.width, grid.height, side), True
        found = self._finest(source, z, x, y, get_tile)
        if found is None:
            return None
        ancestor, dz, sub_x, sub_y = found
        return _cut_edge(ancestor, dz, sub_x, sub_y, side), False

    def _grid(self, source: str, z: int, x: int, y: int,
              get_tile: _GetTile) -> _Grid | None:
        """A cached tile, decoded, or None."""
        key = (source, z, x, y)
        with self._lock:
            grid = self._decoded.get(key)
            if grid is not None:
                self._decoded.move_to_end(key)
                return grid
        try:
            blob = get_tile(z, x, y)
        except Exception:  # noqa: BLE001 - an unreadable cache is a miss
            blob = None
        if blob is None:
            return None
        grid = decode(blob)
        if grid is None:
            return None
        with self._lock:
            self._decoded[key] = grid
            self._decoded.move_to_end(key)
            while len(self._decoded) > self._decoded_size:
                self._decoded.popitem(last=False)
        return grid
