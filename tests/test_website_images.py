"""The project website's pictures are all there, in both sizes.

``docs/index.html`` loads each screenshot as an 800 px copy and opens the
1600 px original in its lightbox, and the hero swaps between a dark and a
light picture with the page's theme. Each of those is a file name typed into
HTML, so a renamed or forgotten picture is a broken image on the live site
and nothing else would say so.
"""
from __future__ import annotations

import re
from pathlib import Path

DOCS = Path(__file__).resolve().parents[1] / "docs"
INDEX = (DOCS / "index.html").read_text(encoding="utf-8")


def _jpeg_size(path: Path) -> tuple[int, int]:
    """(width, height) from a JPEG's start-of-frame marker."""
    data = path.read_bytes()
    i = 2
    while i < len(data):
        marker, length = data[i + 1], int.from_bytes(data[i + 2:i + 4], "big")
        if 0xC0 <= marker <= 0xCF and marker not in (0xC4, 0xC8, 0xCC):
            return int.from_bytes(data[i + 7:i + 9], "big"), int.from_bytes(data[i + 5:i + 7], "big")
        i += 2 + length
    raise AssertionError(f"{path.name}: no frame header")


def test_every_referenced_image_exists() -> None:
    refs = set(re.findall(r'assets/images/[\w.-]+\.(?:jpg|png|svg)', INDEX))
    assert refs
    missing = sorted(r for r in refs if not (DOCS / r).is_file())
    assert not missing, f"the page links pictures that are not in docs/: {missing}"


def test_every_screenshot_has_both_sizes() -> None:
    fulls = re.findall(r'data-full="(assets/images/[\w-]+)\.jpg"', INDEX)
    assert len(fulls) == len(set(fulls)), "a screenshot appears twice in the gallery"
    for base in fulls:
        assert _jpeg_size(DOCS / f"{base}.jpg") == (1600, 1000), base
        assert _jpeg_size(DOCS / f"{base}-800.jpg") == (800, 500), base
        assert f'srcset="{base}-800.jpg 800w, {base}.jpg 1600w"' in INDEX, base


def test_the_hero_has_a_picture_for_each_theme() -> None:
    hero = re.search(r'<img id="hero-image"[^>]*>', INDEX, re.S)
    assert hero, "the hero image lost its id, and with it the theme swap"
    for theme in ("dark", "light"):
        base = re.search(rf'data-{theme}="(assets/images/[\w-]+)"', hero.group(0))
        assert base, f"no {theme} hero picture"
        for suffix in (".jpg", "-800.jpg"):
            assert (DOCS / f"{base.group(1)}{suffix}").is_file(), base.group(1) + suffix
    script = (DOCS / "script.js").read_text(encoding="utf-8")
    assert "getElementById('hero-image')" in script


def test_every_screenshot_has_a_caption_and_alt_text() -> None:
    figures = re.findall(r'<figure class="shot[^"]*">(.*?)</figure>', INDEX, re.S)
    assert figures
    for figure in figures:
        assert re.search(r'alt="[^"]{20,}"', figure), figure[:120]
        assert re.search(r"<figcaption><strong>[^<]+</strong> [^<]+</figcaption>", figure), figure[:120]
