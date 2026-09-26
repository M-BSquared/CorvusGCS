"""The project website's copies of the pictures, made from the scenes' own.

The README shows each picture as its scene writes it. The website
(``docs/index.html``) shows the same pictures as JPEGs in two sizes: the
1600 px original its lightbox opens, and an 800 px copy the page loads first.
Made by hand, those copies drift from the README's; made here, they are one
command after ``shoot``.

A scene names its website picture with :attr:`Scene.web`. A scene whose asset
already lives in the website's folder (a picture only the website shows) gets
its 800 px copy and keeps its original untouched, so running this twice does
not re-encode a JPEG over itself.

The link preview other sites show for the page (``og:image``) is made here
too, from the dark flight picture: the logo, one line, and the window.
"""
from __future__ import annotations

import base64
import json
import os
import sys
import tempfile
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import Any

from .library import Scene

REPO_ROOT = Path(__file__).resolve().parents[2]
WEB_DIR = REPO_ROOT / "docs" / "assets" / "images"
FULL = (1600, 1000)
SMALL = (800, 500)
# What the website's pictures have always been encoded at: close to the
# originals by eye, about a third of their weight.
QUALITY_FULL = 82
QUALITY_SMALL = 80

SOCIAL = WEB_DIR / "social-preview.jpg"
SOCIAL_SIZE = (1200, 630)
SOCIAL_SCENE = "dark"

# The site's own palette (docs/style.css, dark theme) and its font, so the
# preview reads as the page it links to.
_SOCIAL_HTML = """<!doctype html><html><head><meta charset="utf-8"><style>
@font-face { font-family: Inter; src: url(@FONT@) format('woff2'); font-weight: 100 900; }
html, body { margin: 0; width: 1200px; height: 630px; overflow: hidden; }
body { background: #0B0E12; font-family: Inter, sans-serif; position: relative; }
.grid { position: absolute; inset: 0;
  background-image: linear-gradient(rgba(255,255,255,.035) 1px, transparent 1px),
                    linear-gradient(90deg, rgba(255,255,255,.035) 1px, transparent 1px);
  background-size: 48px 48px;
  mask-image: radial-gradient(ellipse at 30% 0%, #000 0%, transparent 75%); }
.glow { position: absolute; left: 520px; top: 120px; width: 900px; height: 600px;
  background: radial-gradient(closest-side, rgba(59,158,255,.16), transparent); }
.head { position: absolute; left: 104px; top: 30px; right: 104px; height: 110px;
  display: flex; align-items: center; justify-content: space-between; }
.head img { height: 96px; margin-left: -8px; }
.tag { color: #A5ADB8; font-size: 21px; line-height: 1.35; text-align: right; font-weight: 500; }
.tag b { color: #F2F4F7; font-weight: 600; }
.win { position: absolute; left: 104px; top: 156px; width: 992px; border-radius: 12px 12px 0 0;
  overflow: hidden; border: 1px solid rgba(59,158,255,.5); border-bottom: 0;
  box-shadow: 0 30px 80px rgba(0,0,0,.55); background: #11161D; }
.bar { height: 26px; display: flex; gap: 7px; align-items: center; padding: 0 12px;
  background: #171D25; border-bottom: 1px solid rgba(255,255,255,.06); }
.bar i { width: 9px; height: 9px; border-radius: 50%; background: #3A4452; display: block; }
.win img { display: block; width: 992px; }
</style></head><body>
<div class="grid"></div><div class="glow"></div>
<div class="head"><img src="@LOGO@" alt="">
<div class="tag"><b>Ground control for PX4 and ArduPilot</b><br>Offline first. macOS, Linux and Windows.</div></div>
<div class="win"><div class="bar"><i></i><i></i><i></i></div><img src="@SHOT@" alt=""></div>
</body></html>"""

_app: Any = None


def targets(scene: Scene) -> tuple[Path, Path]:
    """The full-size and 800 px website files for *scene*."""
    if not scene.web:
        raise ValueError(f"scene {scene.id!r} has no website picture")
    return WEB_DIR / f"{scene.web}.jpg", WEB_DIR / f"{scene.web}-800.jpg"


def _data_uri(path: Path, mime: str) -> str:
    return f"data:{mime};base64," + base64.b64encode(path.read_bytes()).decode()


def social_card(source: Path, log: Callable[[str], None] = print) -> Path:
    """The page's link preview: *source* in a window, under the logo.

    Rendered by the same off-screen QtWebEngine the pictures are taken with,
    from a page that carries everything inline, so nothing is fetched.
    """
    from .capture import Camera
    from .recipe import Step

    mime = "image/png" if source.suffix.lower() == ".png" else "image/jpeg"
    html = (_SOCIAL_HTML
            .replace("@FONT@", _data_uri(REPO_ROOT / "docs" / "assets" / "fonts"
                                         / "Inter-latin.woff2", "font/woff2"))
            .replace("@LOGO@", _data_uri(REPO_ROOT / "assets" / "CorvusGCS.png", "image/png"))
            .replace("@SHOT@", _data_uri(source, mime)))
    camera = Camera(log=lambda _text: None)
    with tempfile.TemporaryDirectory() as tmp:
        page = Path(tmp) / "social.html"
        page.write_text(html, encoding="utf-8")
        width, height = SOCIAL_SIZE
        try:
            camera.run([
                Step("resize", "", json.dumps({"width": width, "height": height})),
                Step("navigate", "", page.as_uri()),
                Step("wait", "", "1.5"),
                Step("screenshot", ""),
            ], SOCIAL)
        finally:
            camera.close()
    log(f"web        link preview          -> {SOCIAL.relative_to(REPO_ROOT)}")
    return SOCIAL


def _application() -> None:
    """Qt's image plugins, and the application they need, before any QImage.

    Without an application QImage reads no JPEG at all, and the plugins sit
    under ``.venv``, which macOS may flag hidden (see capture).
    """
    global _app
    from .capture import _visible_plugins

    plugins = _visible_plugins()
    if plugins:
        os.environ["QT_PLUGIN_PATH"] = os.pathsep.join(
            p for p in (plugins, os.environ.get("QT_PLUGIN_PATH", "")) if p)
    from PyQt6.QtGui import QGuiApplication

    if QGuiApplication.instance() is None:
        os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
        _app = QGuiApplication([sys.argv[0]])


def publish(scenes: Iterable[Scene], source_dir: Path | None = None,
            log: Callable[[str], None] = print) -> list[Path]:
    """Write the website's two sizes for every scene that has a website picture.

    *source_dir* reads the pictures from a ``shoot --out`` folder instead of
    the scenes' assets, so a batch can be looked at before it is published.
    The link preview is remade when the dark flight picture is among them.
    """
    scenes = list(scenes)

    def source_of(scene: Scene) -> Path:
        asset = REPO_ROOT / scene.asset
        return source_dir / asset.name if source_dir else asset

    written: list[Path] = []
    # First: the preview needs QtWebEngine's widget application, and a plain
    # image application made before it would leave the camera without one.
    for scene in scenes:
        if scene.id == SOCIAL_SCENE:
            written.append(social_card(source_of(scene), log))
    _application()
    from PyQt6.QtCore import Qt
    from PyQt6.QtGui import QImage, QImageWriter

    def scaled(image: QImage, size: tuple[int, int]) -> QImage:
        return image.scaled(size[0], size[1], Qt.AspectRatioMode.IgnoreAspectRatio,
                            Qt.TransformationMode.SmoothTransformation)

    def write(image: QImage, path: Path, quality: int) -> None:
        writer = QImageWriter(str(path), b"jpeg")
        writer.setQuality(quality)
        writer.setOptimizedWrite(True)
        writer.setProgressiveScanWrite(True)
        if not writer.write(image):
            raise RuntimeError(f"could not write {path}: {writer.errorString()}")

    WEB_DIR.mkdir(parents=True, exist_ok=True)
    for scene in scenes:
        if not scene.web:
            continue
        source = source_of(scene)
        full, small = targets(scene)
        image = QImage(str(source))
        if image.isNull():
            raise RuntimeError(f"{scene.id}: cannot read {source}")
        image = image.convertToFormat(QImage.Format.Format_RGB32)
        if (image.width(), image.height()) != FULL:
            image = scaled(image, FULL)
        if full.resolve() != source.resolve():
            write(image, full, QUALITY_FULL)
            written.append(full)
        write(scaled(image, SMALL), small, QUALITY_SMALL)
        written.append(small)
        log(f"web        {scene.id:21} -> {full.relative_to(REPO_ROOT)} (+ 800 px)")
    return written
