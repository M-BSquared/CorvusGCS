"""Readers for an operator's own elevation grids: GeoTIFF, XYZ and ESRI ASCII.

Survey offices publish digital surface and terrain models in a handful of
shapes, and these three cover nearly all of them: a GeoTIFF (one band of
heights, georeferenced by its GeoKeys), an XYZ text file (one "x y z" line per
grid point, as the German states publish their 1 km tiles) and an ESRI ASCII
grid (a short header, then rows of heights).

Every reader returns the same :class:`Grid`: north-up rows of float32 heights
with NaN where the file has no data, the outer corner of its top-left pixel,
the pixel size, and the EPSG code when the file states one. Nothing else in
Corvus needs to know which format a grid came from.

A file that cannot be read correctly is refused with :class:`DemImportError`
and a sentence an operator can act on, never read approximately. A mistaken
height grid is ground drawn where it is not.

stdlib only: ``struct`` and ``zlib`` for TIFF, plain text for the rest. LZW,
Deflate, PackBits and uncompressed TIFFs are read, in strips or tiles, with or
without a predictor, which is what GDAL and QGIS write by default.
"""
from __future__ import annotations

import math
import re
import struct
import sys
import zlib
from array import array
from dataclasses import dataclass

# A grid is held in memory once while it is converted. 120 million cells is a
# 11 km square at 1 m, 480 MB of float32: as large as a laptop in the field
# should be asked to take in one go.
MAX_CELLS = 120_000_000


class DemImportError(ValueError):
    """A file that is not a usable elevation grid, with the reason."""


@dataclass
class Grid:
    """North-up float32 heights and where they lie.

    ``x0``/``y0`` is the outer (top-left) corner of the top-left pixel in the
    grid's own coordinates; ``dx``/``dy`` are positive pixel sizes, x growing
    east by columns and y shrinking south by rows.
    """

    width: int
    height: int
    x0: float
    y0: float
    dx: float
    dy: float
    values: array
    epsg: int | None = None


def _nan_array(count: int) -> array:
    out = array("f", [math.nan]) * count
    return out


def _check_size(width: int, height: int) -> None:
    if width < 2 or height < 2:
        raise DemImportError("the grid is smaller than 2 by 2 points")
    if width * height > MAX_CELLS:
        raise DemImportError(
            f"the grid has {width * height:,} points, more than the {MAX_CELLS:,} "
            "one file may have. Split it into smaller tiles")


def detect_format(path: str, filename: str = "") -> str:
    """``"tiff"``, ``"asc"`` or ``"xyz"`` from the file's first bytes and name."""
    with open(path, "rb") as fh:
        head = fh.read(512)
    if head[:4] in (b"II*\x00", b"MM\x00*", b"II+\x00", b"MM\x00+"):
        return "tiff"
    text = head.decode("latin-1", "replace").lstrip().lower()
    if text.startswith(("ncols", "nrows")):
        return "asc"
    if filename.lower().endswith((".tif", ".tiff")):
        raise DemImportError("this .tif file is not a TIFF image")
    return "xyz"


def read_grid(path: str, filename: str = "") -> Grid:
    """Read *path* as whichever of the three formats it is."""
    kind = detect_format(path, filename)
    if kind == "tiff":
        return read_geotiff(path)
    if kind == "asc":
        return read_esri_ascii(path)
    return read_xyz(path)


# ---------------------------------------------------------------- GeoTIFF

_TYPE_SIZES = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 6: 1, 7: 1, 8: 2, 9: 4, 10: 8,
               11: 4, 12: 8, 16: 8, 17: 8, 18: 8}
_TYPE_FMT = {1: "B", 3: "H", 4: "I", 6: "b", 8: "h", 9: "i", 11: "f", 12: "d",
             16: "Q", 17: "q", 18: "Q"}

_COMPRESSION_NAMES = {6: "old JPEG", 7: "JPEG", 34887: "LERC", 34925: "LZMA",
                      50000: "Zstandard", 50001: "WebP", 34712: "JPEG 2000"}


def _lzw_decode(data: bytes) -> bytes:
    """TIFF LZW (MSB first, early change), as libtiff writes it."""
    out = bytearray()
    table: list[bytes] = [bytes([i]) for i in range(256)] + [b"", b""]
    bits = 9
    prev: bytes | None = None
    pos = 0
    total = len(data) * 8
    while pos + bits <= total:
        byte = pos >> 3
        chunk = int.from_bytes(data[byte:byte + 3].ljust(3, b"\x00"), "big")
        code = (chunk >> (24 - (pos & 7) - bits)) & ((1 << bits) - 1)
        pos += bits
        if code == 256:
            table = table[:258]
            bits = 9
            prev = None
            continue
        if code == 257:
            break
        if prev is None:
            entry = table[code]
            out += entry
            prev = entry
            continue
        if code < len(table):
            entry = table[code]
            table.append(prev + entry[:1])
        elif code == len(table):
            entry = prev + prev[:1]
            table.append(entry)
        else:
            raise DemImportError("the TIFF's LZW data is damaged")
        out += entry
        prev = entry
        size = len(table)
        if size >= 511 and bits == 9:
            bits = 10
        elif size >= 1023 and bits == 10:
            bits = 11
        elif size >= 2047 and bits == 11:
            bits = 12
    return bytes(out)


def _packbits_decode(data: bytes) -> bytes:
    out = bytearray()
    i = 0
    n = len(data)
    while i < n:
        c = data[i]
        i += 1
        if c < 128:
            out += data[i:i + c + 1]
            i += c + 1
        elif c > 128:
            out += data[i:i + 1] * (257 - c)
            i += 1
    return bytes(out)


class _Tiff:
    def __init__(self, path: str) -> None:
        self.fh = open(path, "rb")
        try:
            self._open()
        except BaseException:
            self.fh.close()
            raise

    def _open(self) -> None:
        head = self.fh.read(16)
        if head[:2] == b"II":
            self.bo = "<"
        elif head[:2] == b"MM":
            self.bo = ">"
        else:
            raise DemImportError("not a TIFF file")
        (magic,) = struct.unpack(self.bo + "H", head[2:4])
        self.big = magic == 43
        if magic not in (42, 43):
            raise DemImportError("not a TIFF file")
        if self.big:
            (first,) = struct.unpack(self.bo + "Q", head[8:16])
        else:
            (first,) = struct.unpack(self.bo + "I", head[4:8])
        self.tags = self._read_ifd(first)

    def close(self) -> None:
        self.fh.close()

    def _read_ifd(self, offset: int) -> dict[int, tuple]:
        fh, bo = self.fh, self.bo
        fh.seek(offset)
        if self.big:
            (count,) = struct.unpack(bo + "Q", fh.read(8))
            entry_size, inline = 20, 8
        else:
            (count,) = struct.unpack(bo + "H", fh.read(2))
            entry_size, inline = 12, 4
        if count > 4096:
            raise DemImportError("the TIFF header is damaged")
        raw = fh.read(count * entry_size)
        tags: dict[int, tuple] = {}
        for i in range(count):
            e = raw[i * entry_size:(i + 1) * entry_size]
            if self.big:
                tag, typ, n = struct.unpack(bo + "HHQ", e[:12])
                ref = e[12:20]
            else:
                tag, typ, n = struct.unpack(bo + "HHI", e[:8])
                ref = e[8:12]
            size = _TYPE_SIZES.get(typ)
            if size is None:
                continue
            total = size * n
            if total <= inline:
                blob = ref[:total]
            else:
                (where,) = struct.unpack(bo + ("Q" if self.big else "I"), ref)
                here = fh.tell()
                fh.seek(where)
                blob = fh.read(total)
                fh.seek(here)
            if typ == 2:
                tags[tag] = (blob.split(b"\x00", 1)[0].decode("latin-1", "replace"),)
            elif typ in (5, 10):
                fmt = "I" if typ == 5 else "i"
                pairs = struct.unpack(bo + fmt * (2 * n), blob)
                tags[tag] = tuple(pairs[k] / pairs[k + 1] if pairs[k + 1] else 0.0
                                  for k in range(0, len(pairs), 2))
            elif typ == 7:
                tags[tag] = tuple(blob)
            else:
                tags[tag] = struct.unpack(bo + _TYPE_FMT[typ] * n, blob)
        return tags

    def get(self, tag: int, default=None):
        value = self.tags.get(tag)
        return value if value is not None else default

    def one(self, tag: int, default=None):
        value = self.tags.get(tag)
        return value[0] if value else default

    def read_at(self, offset: int, size: int) -> bytes:
        self.fh.seek(offset)
        return self.fh.read(size)


def _geokeys(tiff: _Tiff) -> dict[int, object]:
    directory = tiff.get(34735)
    if not directory or len(directory) < 4:
        return {}
    doubles = tiff.get(34736, ())
    ascii_params = (tiff.get(34737) or ("",))[0]
    keys: dict[int, object] = {}
    count = directory[3]
    for i in range(count):
        base = 4 + 4 * i
        if base + 4 > len(directory):
            break
        key, location, n, value = directory[base:base + 4]
        if location == 0:
            keys[key] = value
        elif location == 34736:
            keys[key] = doubles[value:value + n]
        elif location == 34737:
            keys[key] = ascii_params[value:value + n].rstrip("|")
    return keys


def read_geotiff(path: str) -> Grid:
    """The first band of a single-image GeoTIFF."""
    tiff = _Tiff(path)
    try:
        return _read_geotiff(tiff)
    finally:
        tiff.close()


def _read_geotiff(tiff: _Tiff) -> Grid:
    width = tiff.one(256)
    height = tiff.one(257)
    if not width or not height:
        raise DemImportError("the TIFF has no image size")
    _check_size(width, height)
    bits = tiff.get(258, (1,))[0]
    samples = tiff.one(277, 1)
    fmt = tiff.get(339, (1,))[0]
    compression = tiff.one(259, 1)
    predictor = tiff.one(317, 1)
    planar = tiff.one(284, 1)
    if compression not in (1, 5, 8, 32946, 32773):
        name = _COMPRESSION_NAMES.get(compression, f"method {compression}")
        raise DemImportError(
            f"the TIFF is compressed with {name}, which Corvus cannot read. "
            "Save it again with LZW or Deflate compression (QGIS: Export, Save As)")
    code = {(3, 32): "f", (3, 64): "d", (1, 8): "B", (1, 16): "H", (1, 32): "I",
            (2, 8): "b", (2, 16): "h", (2, 32): "i"}.get((fmt, bits))
    if code is None:
        raise DemImportError(
            f"the TIFF stores {bits} bit samples of format {fmt}, which is not an "
            "elevation grid Corvus can read")
    if code in ("I", "i") and array(code).itemsize != 4:
        code = {"I": "L", "i": "l"}[code]
    size = bits // 8

    # Georeferencing
    keys = _geokeys(tiff)
    scale = tiff.get(33550)
    tie = tiff.get(33922)
    matrix = tiff.get(34264)
    flip = False
    if scale and tie and len(tie) >= 6:
        dx, dy = float(scale[0]), float(scale[1])
        i, j, _k, x, y, _z = (float(v) for v in tie[:6])
        if dy < 0:
            dy = -dy
            flip = True
        x0 = x - i * dx
        y0 = y + j * dy if not flip else y - j * dy
    elif matrix and len(matrix) >= 16:
        # Row-major 4x4: x = a*i + b*j + d, y = e*i + f*j + h.
        a, b, d = float(matrix[0]), float(matrix[1]), float(matrix[3])
        e, f, h = float(matrix[4]), float(matrix[5]), float(matrix[7])
        if abs(b) > 1e-9 or abs(e) > 1e-9:
            raise DemImportError("the TIFF is rotated, which Corvus cannot place on the map")
        dx, x0, dy, y0 = a, d, -f, h
        if dy < 0:
            dy = -dy
            flip = True
    else:
        raise DemImportError(
            "the TIFF has no georeferencing. Use a GeoTIFF, or the XYZ file it came with")
    if not (dx > 0 and dy > 0):
        raise DemImportError("the TIFF's pixel size is not positive")
    if keys.get(1025) == 2:
        # PixelIsPoint: the tie point is the centre of the first pixel.
        x0 -= dx / 2.0
        y0 += dy / 2.0 if not flip else -dy / 2.0

    epsg = None
    model = keys.get(1024)
    projected = keys.get(3072)
    geographic = keys.get(2048)
    if isinstance(projected, int) and 0 < projected < 32767:
        epsg = projected
    elif model == 2 and isinstance(geographic, int) and 0 < geographic < 32767:
        epsg = geographic

    nodata = None
    raw_nodata = tiff.get(42113)
    if raw_nodata:
        try:
            nodata = float(str(raw_nodata[0]).strip())
        except ValueError:
            nodata = None

    # Pixel data, in strips or tiles
    tiled = tiff.get(322) is not None
    if tiled:
        tw, th = tiff.one(322), tiff.one(323)
        offsets, counts = tiff.get(324), tiff.get(325)
    else:
        tw, th = width, tiff.one(278, height) or height
        th = min(th, height)
        offsets, counts = tiff.get(273), tiff.get(279)
    if not offsets or not counts or not tw or not th:
        raise DemImportError("the TIFF's image data is missing")
    across = -(-width // tw)
    down = -(-height // th)
    chunk_samples = samples if planar == 1 else 1
    chunk_count = across * down
    if len(offsets) < chunk_count:
        raise DemImportError("the TIFF's image data is incomplete")

    out = _nan_array(width * height)
    swap = (tiff.bo == "<") != (sys.byteorder == "little")
    stride = tw * chunk_samples
    for index in range(chunk_count):
        blob = tiff.read_at(offsets[index], counts[index])
        if compression == 5:
            blob = _lzw_decode(blob)
        elif compression in (8, 32946):
            try:
                blob = zlib.decompress(blob)
            except zlib.error as exc:
                raise DemImportError("the TIFF's Deflate data is damaged") from exc
        elif compression == 32773:
            blob = _packbits_decode(blob)
        col0 = (index % across) * tw
        row0 = (index // across) * th
        rows = th if tiled else min(th, height - row0)
        need = rows * stride * size
        if len(blob) < need:
            blob = blob + b"\x00" * (need - len(blob))
        if predictor == 3:
            blob = _undo_float_predictor(blob, rows, tw, chunk_samples, size, tiff.bo)
            values = array(code)
            values.frombytes(blob[:need])
            if swap:
                values.byteswap()
        else:
            values = array(code)
            values.frombytes(blob[:need])
            if swap:
                values.byteswap()
            if predictor == 2:
                _undo_horizontal(values, rows, tw, chunk_samples, code)
        span = min(tw, width - col0)
        for r in range(rows):
            dest_row = row0 + r
            if dest_row >= height:
                break
            src = r * stride
            line = values[src:src + span * chunk_samples:chunk_samples]
            dest = dest_row * width + col0
            out[dest:dest + span] = array("f", line)

    if nodata is not None or code in ("f", "d"):
        _mask_nodata(out, nodata)
    if flip:
        out = _flip_rows(out, width, height)
    return Grid(width, height, x0, y0, dx, dy, out, epsg)


def _undo_horizontal(values: array, rows: int, width: int, samples: int, code: str) -> None:
    """Predictor 2, in place: each sample is stored as the difference to its left."""
    mask = {"B": 0xFF, "b": 0xFF, "H": 0xFFFF, "h": 0xFFFF}.get(code)
    if code in ("f", "d"):
        raise DemImportError("the TIFF uses predictor 2 on floating point data")
    stride = width * samples
    signed = code in ("b", "h", "i", "l")
    bits = values.itemsize * 8
    top = 1 << bits
    half = top >> 1
    mask = top - 1 if mask is None else mask
    for r in range(rows):
        base = r * stride
        for i in range(base + samples, base + stride):
            v = (values[i] + values[i - samples]) & mask
            if signed and v >= half:
                v -= top
            values[i] = v


def _undo_float_predictor(blob: bytes, rows: int, width: int, samples: int,
                          size: int, bo: str) -> bytes:
    """Predictor 3: byte-wise differencing over byte planes, most significant first."""
    stride = width * samples
    row_bytes = stride * size
    out = bytearray(len(blob))
    for r in range(rows):
        row = bytearray(blob[r * row_bytes:(r + 1) * row_bytes])
        for i in range(samples, row_bytes):
            row[i] = (row[i] + row[i - samples]) & 0xFF
        restored = bytearray(row_bytes)
        for k in range(size):
            plane = row[k * stride:(k + 1) * stride]
            pos = size - 1 - k if bo == "<" else k
            restored[pos::size] = plane
        out[r * row_bytes:(r + 1) * row_bytes] = restored
    return bytes(out)


def _mask_nodata(values: array, nodata: float | None) -> None:
    """NaN for the file's no-data value and for anything that cannot be ground."""
    for i, v in enumerate(values):
        if v != v:
            continue
        if (nodata is not None and (v == nodata or abs(v - nodata) < 1e-3)) \
                or v < -12000.0 or v > 10000.0:
            values[i] = math.nan


def _flip_rows(values: array, width: int, height: int) -> array:
    out = array("f")
    for r in range(height - 1, -1, -1):
        out.extend(values[r * width:(r + 1) * width])
    return out


# ---------------------------------------------------------------- ESRI ASCII

def read_esri_ascii(path: str) -> Grid:
    """An ESRI ASCII grid (``.asc``): a short header, then rows north to south."""
    header: dict[str, float] = {}
    with open(path, encoding="latin-1") as fh:
        line = ""
        for line in fh:
            parts = line.split()
            if len(parts) == 2 and re.match(r"^[A-Za-z_]+$", parts[0]):
                try:
                    header[parts[0].lower()] = float(parts[1])
                except ValueError as exc:
                    raise DemImportError(f"the header line {line.strip()!r} is unreadable") from exc
                continue
            break
        try:
            width = int(header["ncols"])
            height = int(header["nrows"])
        except KeyError as exc:
            raise DemImportError("the ASCII grid has no ncols or nrows") from exc
        _check_size(width, height)
        dx = header.get("cellsize", header.get("dx"))
        dy = header.get("cellsize", header.get("dy"))
        if not dx or not dy or dx <= 0 or dy <= 0:
            raise DemImportError("the ASCII grid has no cell size")
        if "xllcorner" in header:
            x0 = header["xllcorner"]
        elif "xllcenter" in header:
            x0 = header["xllcenter"] - dx / 2.0
        else:
            raise DemImportError("the ASCII grid has no xllcorner")
        if "yllcorner" in header:
            ybottom = header["yllcorner"]
        elif "yllcenter" in header:
            ybottom = header["yllcenter"] - dy / 2.0
        else:
            raise DemImportError("the ASCII grid has no yllcorner")
        nodata = header.get("nodata_value")
        values = array("f")
        pending = line.split()
        count = width * height
        while True:
            for token in pending:
                try:
                    values.append(float(token))
                except ValueError as exc:
                    raise DemImportError(f"{token!r} in the ASCII grid is not a number") from exc
            if len(values) >= count:
                break
            nxt = fh.readline()
            if not nxt:
                break
            pending = nxt.split()
    if len(values) < count:
        raise DemImportError(
            f"the ASCII grid ends after {len(values):,} of its {count:,} values")
    del values[count:]
    _mask_nodata(values, nodata)
    return Grid(width, height, x0, ybottom + height * dy, dx, dy, values, None)


# ---------------------------------------------------------------- XYZ

_SPLIT = re.compile(r"[\s,;]+")


def _xyz_rows(path: str):
    """(x, y, z) per data line; header and comment lines are skipped."""
    with open(path, encoding="latin-1") as fh:
        for line in fh:
            parts = _SPLIT.split(line.strip())
            if len(parts) < 3:
                continue
            try:
                yield float(parts[0]), float(parts[1]), float(parts[2])
            except ValueError:
                continue


def _spacing(values: set[float]) -> float:
    ordered = sorted(values)
    best = math.inf
    for a, b in zip(ordered, ordered[1:]):
        step = b - a
        if 1e-9 < step < best:
            best = step
    return best


def read_xyz(path: str) -> Grid:
    """An XYZ point list on a regular grid, one ``x y z`` per line, any order.

    Two passes over the file so the memory needed is the grid's, not the
    point list's: the first finds the extent and the spacing, the second puts
    every height in its cell. Points that are not on the grid the spacing
    implies are a different kind of file (a scattered point cloud) and are
    refused, not rasterised.
    """
    xmin = ymin = math.inf
    xmax = ymax = -math.inf
    xs: set[float] = set()
    ys: set[float] = set()
    points = 0
    for x, y, _z in _xyz_rows(path):
        points += 1
        if x < xmin:
            xmin = x
        if x > xmax:
            xmax = x
        if y < ymin:
            ymin = y
        if y > ymax:
            ymax = y
        if len(xs) < 200_000:
            xs.add(round(x, 6))
        if len(ys) < 200_000:
            ys.add(round(y, 6))
    if points < 4:
        raise DemImportError("the file holds no x y z lines")
    dx = _spacing(xs)
    dy = _spacing(ys)
    if not (math.isfinite(dx) and math.isfinite(dy)):
        raise DemImportError("the points do not form a grid (all on one line)")
    width = int(round((xmax - xmin) / dx)) + 1
    height = int(round((ymax - ymin) / dy)) + 1
    _check_size(width, height)
    if points < 0.25 * width * height:
        raise DemImportError(
            "the points are not on a regular grid. Corvus reads gridded XYZ "
            "files, not scattered point clouds")
    values = _nan_array(width * height)
    off_grid = 0
    for x, y, z in _xyz_rows(path):
        fc = (x - xmin) / dx
        fr = (ymax - y) / dy
        c = int(round(fc))
        r = int(round(fr))
        if abs(fc - c) > 0.1 or abs(fr - r) > 0.1:
            off_grid += 1
            continue
        if -12000.0 < z < 10000.0:
            values[r * width + c] = z
    if off_grid > points * 0.01:
        raise DemImportError(
            "the points are not on a regular grid. Corvus reads gridded XYZ "
            "files, not scattered point clouds")
    return Grid(width, height, xmin - dx / 2.0, ymax + dy / 2.0, dx, dy, values, None)


def guess_epsg(grid: Grid) -> int | None:
    """EPSG 4326 for a grid whose coordinates can only be degrees, else None."""
    x1 = grid.x0 + grid.width * grid.dx
    y1 = grid.y0 - grid.height * grid.dy
    if -180.5 <= grid.x0 <= 180.5 and -180.5 <= x1 <= 180.5 \
            and -90.5 <= y1 <= 90.5 and -90.5 <= grid.y0 <= 90.5 and grid.dx < 1.0:
        return 4326
    return None
