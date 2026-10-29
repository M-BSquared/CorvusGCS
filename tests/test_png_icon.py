"""tools/png_icon.py: the icon the Linux and Windows builds make without ImageMagick."""
from __future__ import annotations

import struct
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "tools"))
import png_icon  # noqa: E402

LOGO = Path(__file__).resolve().parent.parent / "assets" / "CorvusGCS_logo.png"


def _solid(width: int, height: int, rgba: tuple[int, int, int, int]) -> bytes:
    rows = [bytearray(bytes(rgba) * width) for _ in range(height)]
    return png_icon.write_png(width, height, rows)


def test_a_wide_image_is_centred_on_a_transparent_square() -> None:
    w, h, rows = png_icon.read_png(_solid(40, 20, (200, 10, 10, 255)))
    out = png_icon.fit(w, h, rows, 32)
    assert len(out) == 32 and all(len(r) == 128 for r in out)
    assert out[0][3] == 0, "the top is empty: the image is half as tall as wide"
    assert out[16][64:68] == bytearray([200, 10, 10, 255])


def test_the_logo_becomes_a_256_square(tmp_path: Path) -> None:
    out = tmp_path / "icon.png"
    subprocess.run([sys.executable, str(png_icon.__file__), str(LOGO), str(out)], check=True)
    data = out.read_bytes()
    assert data.startswith(png_icon.SIGNATURE)
    assert struct.unpack(">II", data[16:24]) == (256, 256)
    w, h, rows = png_icon.read_png(data)
    assert any(r[i + 3] for r in rows for i in range(0, len(r), 4)), "something is drawn"


def test_other_formats_are_refused() -> None:
    try:
        png_icon.read_png(b"GIF89a")
    except ValueError:
        return
    raise AssertionError("a GIF was read as a PNG")
