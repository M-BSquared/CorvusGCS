"""The interface must load nothing from the internet.

The whole product premise is a laptop in a field with no connection. That fails
in a way nobody notices during development — where the CDN always answers — and
then shows up as missing icons and wrong type on the one machine that matters.
So the contract is pinned here rather than left to review:

* ``src/index.html`` references no external origin.
* Every asset it does reference exists on disk.
* The vendored bundles are the real libraries, not a saved error page.
* Fonts and icons in particular are vendored, since those were the last two
  CDN dependencies and are the easiest to reintroduce by habit.

Map TILES are the deliberate exception: they originate online, but the browser
never fetches them — they go through the backend, which caches them into the
offline MBTiles store (see tests/test_tile_regions.py).
"""
from __future__ import annotations

import pathlib
import re

import pytest

_REPO = pathlib.Path(__file__).resolve().parent.parent
_SRC = _REPO / "src"
_INDEX = _SRC / "index.html"
_VENDOR = _SRC / "vendor"


def _strip_comments(text: str) -> str:
    """Drop /* block */, // line and <!-- html --> comments.

    Crude on purpose — it only has to stop documentation examples from being
    read as real icon usage, and it is applied to source this repo owns.
    """
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    text = re.sub(r"<!--.*?-->", "", text, flags=re.S)
    return re.sub(r"^\s*//.*$", "", text, flags=re.M)


def _index_text() -> str:
    return _INDEX.read_text(encoding="utf-8")


# ---------------------------------------------------------------------------
# 1. No external origins
# ---------------------------------------------------------------------------

def test_index_html_references_no_external_origin() -> None:
    """Nothing in the page may point at another host — no CDN, no font service."""
    offenders = re.findall(r'(?:src|href)\s*=\s*"((?:https?:)?//[^"]*)"', _index_text())
    assert not offenders, f"index.html loads external resources: {offenders}"


@pytest.mark.parametrize("needle", [
    "fonts.googleapis.com",
    "fonts.gstatic.com",
    "unpkg.com",
    "cdn.jsdelivr.net",
    "cdnjs.cloudflare.com",
])
def test_index_html_names_no_known_cdn(needle: str) -> None:
    assert needle not in _index_text(), f"index.html still references {needle}"


def test_frontend_sources_reference_no_external_origin() -> None:
    """Same rule for the CSS and JS the page pulls in.

    The tile UPSTREAMS live in corvus/tile_sources.py (backend-only, never
    fetched by the browser), so nothing under src/css or src/js — vendored
    bundles aside — has any business naming a remote host.
    """
    offenders: list[str] = []
    for path in list((_SRC / "css").rglob("*.css")) + list((_SRC / "js").rglob("*.js")):
        for match in re.findall(r"https?://[^\s\"')]+", path.read_text(encoding="utf-8")):
            # A bare mention in prose (a credits URL, a doc link in a comment)
            # is not a load; only url()/import/fetch targets would be.
            if re.search(rf"(?:url\(|src\s*=|import\s+)['\"]?{re.escape(match)}", 
                         path.read_text(encoding="utf-8")):
                offenders.append(f"{path.relative_to(_REPO)}: {match}")
    assert not offenders, f"frontend sources load from the network: {offenders}"


# ---------------------------------------------------------------------------
# 2. Everything referenced exists on disk
# ---------------------------------------------------------------------------

def test_every_asset_index_html_references_exists() -> None:
    refs = re.findall(r'(?:src|href)\s*=\s*"([^"#][^"]*)"', _index_text())
    missing = [r for r in refs if not (_SRC / r).is_file()]
    assert not missing, f"index.html references files that are not on disk: {missing}"


def test_font_css_payloads_exist() -> None:
    """Every url() in the vendored font CSS resolves to a real .woff2."""
    css = (_VENDOR / "fonts.css").read_text(encoding="utf-8")
    urls = re.findall(r"url\(([^)]+)\)", css)
    assert urls, "fonts.css declares no font files"
    for u in urls:
        rel = u.strip("'\"")
        assert not rel.startswith("http"), f"fonts.css still points at a remote host: {rel}"
        assert (_VENDOR / rel).is_file(), f"missing font payload: {rel}"


# ---------------------------------------------------------------------------
# 3. The vendored bundles are genuine
# ---------------------------------------------------------------------------

def test_vendored_lucide_is_a_real_bundle() -> None:
    js = (_VENDOR / "lucide.min.js").read_text(encoding="utf-8")
    assert "lucide" in js[:400].lower(), "lucide bundle missing its license header"
    assert "createIcons" in js, "lucide bundle does not export createIcons"
    # Pinned, not @latest: a floating version is not reproducible, and the
    # bundle is checked in precisely so the build never reaches the network.
    assert re.search(r"lucide v\d+\.\d+\.\d+", js[:400]), "lucide bundle has no version in its header"


def test_vendored_lucide_defines_every_icon_the_ui_asks_for() -> None:
    """A missing glyph renders as nothing, and nothing about that is loud.

    Scans the source for icon names and checks each against the bundle's own
    export list, so adding an icon to the UI without it existing fails here
    rather than silently leaving a blank square in the field.
    """
    names: set[str] = set()
    for path in list((_SRC / "js").rglob("*.js")) + [_INDEX]:
        # Comments are stripped first: ui.js documents its icon() helper with a
        # literal `<i data-lucide="name">`, which is prose, not a glyph request.
        text = _strip_comments(path.read_text(encoding="utf-8"))
        names |= set(re.findall(r'data-lucide="([a-z0-9-]+)"', text))
        names |= set(re.findall(r'\bicon:\s*"([a-z0-9-]+)"', text))
        names |= set(re.findall(r'\b(?:icon|iconButton)\(\s*"([a-z0-9-]+)"', text))
    assert names, "found no icon names to check (scan is broken)"

    bundle = (_VENDOR / "lucide.min.js").read_text(encoding="utf-8")
    missing = []
    for n in sorted(names):
        pascal = "".join(w.capitalize() for w in n.split("-"))
        if pascal not in bundle:
            missing.append(n)
    assert not missing, f"icons used by the UI are absent from the vendored bundle: {missing}"


def test_vendored_font_files_are_woff2() -> None:
    files = sorted((_VENDOR / "fonts").glob("*.woff2"))
    assert files, "no vendored font payloads"
    for f in files:
        # wOF2 magic — guards against a saved HTML error page.
        assert f.read_bytes()[:4] == b"wOF2", f"{f.name} is not a woff2 file"


def test_font_css_declares_both_families() -> None:
    css = (_VENDOR / "fonts.css").read_text(encoding="utf-8")
    for family in ("Inter", "JetBrains Mono"):
        assert f"font-family: '{family}'" in css, f"fonts.css does not declare {family}"


def test_font_faces_cover_the_weights_the_ui_uses() -> None:
    """Both families are variable fonts declared with a weight RANGE.

    A single-weight declaration pointing at a variable file renders every
    weight at the default instance — the bug this guards against is subtle
    (headings simply stop looking bold), so pin the ranges.
    """
    css = (_VENDOR / "fonts.css").read_text(encoding="utf-8")
    ranges = re.findall(r"font-weight:\s*(\d+)\s+(\d+);", css)
    assert ranges, "fonts.css declares no weight ranges — variable axis not exposed"
    for lo, hi in ranges:
        assert int(lo) <= 400 and int(hi) >= 600, f"weight range {lo}-{hi} misses the UI's 400-600"
