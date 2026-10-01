"""The documentation site in ``docs/guide`` stays whole.

The manual is hand-written HTML inside a frame that ``tools/guide.py``
generates, served as it is by GitHub Pages. Nothing builds it on the way out,
so a page renamed without a run of the script, a link to an anchor that was
reworded, or a screenshot that moved is a broken page on the live site with
nothing else to say so.
"""
from __future__ import annotations

import importlib.util
import re
import sys
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
DOCS = ROOT / "docs"
GUIDE = DOCS / "guide"

_spec = importlib.util.spec_from_file_location("corvus_guide_tool", ROOT / "tools" / "guide.py")
guide = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
# Registered before it runs: a dataclass looks its own module up by name.
sys.modules[_spec.name] = guide
_spec.loader.exec_module(guide)


class _Page(HTMLParser):
    """Every id, every local link target, and the prose outside code."""

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.ids: set[str] = set()
        self.refs: list[str] = []
        self.prose: list[str] = []
        self._skip = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if values.get("id"):
            self.ids.add(values["id"] or "")
        for key in ("href", "src"):
            if values.get(key):
                self.refs.append(values[key] or "")
        if values.get("srcset"):
            self.refs += [part.split()[0] for part in (values["srcset"] or "").split(",")]
        if tag in ("code", "pre", "script", "style", "kbd"):
            self._skip += 1

    def handle_endtag(self, tag: str) -> None:
        if tag in ("code", "pre", "script", "style", "kbd") and self._skip:
            self._skip -= 1

    def handle_data(self, data: str) -> None:
        if not self._skip:
            self.prose.append(data)


def _parse(path: Path) -> _Page:
    page = _Page()
    page.feed(path.read_text(encoding="utf-8"))
    return page


def _content(path: Path) -> _Page:
    page = _Page()
    page.feed(guide.content_of(path.read_text(encoding="utf-8")))
    return page


def test_every_page_frame_is_current() -> None:
    assert guide.build(check=True) == [], "run python3 tools/guide.py"


def test_the_sidebar_lists_exactly_the_pages_on_disk() -> None:
    listed = {page.file for page in guide.PAGES}
    on_disk = {path.name for path in GUIDE.glob("*.html")}
    assert listed == on_disk
    assert guide.PAGES[0].file == "index.html"


def test_every_page_starts_with_one_h1() -> None:
    for page in guide.PAGES:
        content = guide.content_of((GUIDE / page.file).read_text(encoding="utf-8"))
        assert content.strip().startswith("<h1>"), page.file
        assert content.count("<h1") == 1, page.file


def test_every_local_link_and_anchor_resolves() -> None:
    ids = {path: _parse(path).ids for path in GUIDE.glob("*.html")}
    broken: list[str] = []
    for path in sorted(GUIDE.glob("*.html")):
        for ref in _parse(path).refs:
            parts = urlsplit(ref)
            if parts.scheme or ref.startswith("//") or ref.startswith("data:"):
                continue
            target = (path.parent / unquote(parts.path)).resolve() if parts.path else path
            if not target.is_file():
                broken.append(f"{path.name}: {ref}")
                continue
            if parts.fragment and target.suffix == ".html":
                known = ids.get(target) if target in ids else _parse(target).ids
                if parts.fragment not in known:
                    broken.append(f"{path.name}: {ref}")
    assert not broken, broken


def test_screenshots_have_alt_text_and_both_sizes() -> None:
    for path in GUIDE.glob("*.html"):
        text = path.read_text(encoding="utf-8")
        for figure in re.findall(r'<figure class="doc-shot">(.*?)</figure>', text, re.S):
            assert re.search(r'alt="[^"]{20,}"', figure), path.name
            full = re.search(r'<a href="\.\./(assets/images/[\w-]+)\.jpg">', figure)
            assert full, path.name
            assert f"../{full.group(1)}-800.jpg 800w" in figure, path.name


def test_the_prose_uses_no_dashes() -> None:
    """AGENTS.md: no em or en dash, and no hyphen standing in for one."""
    offenders: list[str] = []
    for path in GUIDE.glob("*.html"):
        prose = " ".join(_content(path).prose)
        for match in re.finditer(r"[–—]| - ", prose):
            start = max(0, match.start() - 30)
            offenders.append(f"{path.name}: ...{prose[start:match.end() + 30]}...")
    assert not offenders, offenders


def test_the_website_points_at_the_manual() -> None:
    index = (DOCS / "index.html").read_text(encoding="utf-8")
    assert 'href="guide/"' in index or 'href="guide/index.html"' in index
    assert "github.com/M-BSquared/CorvusGCS#readme" not in index
