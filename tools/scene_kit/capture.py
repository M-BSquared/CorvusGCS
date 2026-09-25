"""Taking the picture: a scene's recipe, run in the desktop app's own engine.

A recipe (see :mod:`recipe`) is a list of browser steps. Followed by hand it
is an afternoon of copying snippets into a console; followed here it is a
command. The steps run in QtWebEngine, which is what the desktop app renders
with, so a picture shows exactly what an operator sees in Corvus: the same
Chromium, the same WebGL map, the same fonts.

The window is never shown. On macOS and Windows it is a real native window
kept off the screen (``WA_DontShowOnScreen``), because that is the one way
QtWebEngine still composites with the GPU: the ``offscreen`` platform loses
its GL context there and grabs a page with no text on it. The process also
stays out of the Dock while it works.

Each scene gets a fresh off-the-record profile, so nothing one picture seeded
in ``localStorage`` leaks into the next.
"""
from __future__ import annotations

import json
import os
import stat
import sys
import tempfile
import time
from pathlib import Path
from typing import Any
from collections.abc import Callable

from .recipe import Step

REPO_ROOT = Path(__file__).resolve().parents[2]

# Wraps a step's expression so its value, or its promise's value, lands
# somewhere a later call can read: runJavaScript hands back a Promise as an
# empty object rather than waiting for it.
_RUN = (
    "(() => {{ window.__corvusScene = null; let v; "
    "try {{ v = ({expr}); }} catch (e) {{ window.__corvusScene = {{ error: String(e) }}; return; }} "
    "Promise.resolve(v).then("
    "(r) => {{ window.__corvusScene = {{ value: r === undefined ? '' : String(r) }}; }}, "
    "(e) => {{ window.__corvusScene = {{ error: String((e && e.stack) || e) }}; }}); }})()"
)


def qt_problem() -> str | None:
    """Why pictures cannot be taken here, or None when they can."""
    try:
        import PyQt6  # noqa: F401
        from PyQt6 import QtWebEngineWidgets  # noqa: F401
    except ImportError as exc:
        return (f"PyQt6 with QtWebEngine is needed to take pictures ({exc}). It is the "
                "desktop app's own dependency group: pip install --group app")
    return None


def _visible_plugins() -> str | None:
    """A plugin folder Qt can list, when the installed one is flagged hidden.

    Qt skips hidden files when it looks for plugins, and on some macOS setups
    everything under a dot-directory such as ``.venv`` carries the hidden
    flag, set again by the system after it is cleared. Qt then has no platform
    plugin at all, and it does not raise: it prints a line and aborts the
    process. So each plugin is linked into a folder outside the dot-directory
    and Qt is pointed there. The links resolve to the real files, so the
    plugins still find their own Qt libraries, and the installation itself is
    not touched.
    """
    hidden = getattr(stat, "UF_HIDDEN", 0)
    if not hidden:
        return None
    from PyQt6.QtCore import QT_VERSION_STR, QLibraryInfo

    root = Path(QLibraryInfo.path(QLibraryInfo.LibraryPath.PluginsPath))
    platforms = root / "platforms"
    try:
        flagged = any(entry.stat().st_flags & hidden for entry in platforms.iterdir())
    except OSError:
        return None
    if not flagged:
        return None
    links = Path(tempfile.gettempdir()) / f"corvus-scene-qt-plugins-{QT_VERSION_STR}"
    for folder in root.iterdir():
        if not folder.is_dir():
            continue
        (links / folder.name).mkdir(parents=True, exist_ok=True)
        for plugin in folder.iterdir():
            link = links / folder.name / plugin.name
            if not link.is_symlink():
                try:
                    link.symlink_to(plugin)
                except OSError:
                    continue
    return str(links)


class Camera:
    """One QtWebEngine view, kept off screen, driven step by step."""

    def __init__(self, hidpi: bool = False, verbose: bool = False,
                 log: Callable[[str], None] | None = None) -> None:
        self.hidpi = hidpi
        self.verbose = verbose
        self.log = log or (lambda text: print(text, flush=True))
        # The desktop app's own Chromium switches: the ones that keep WebGL,
        # and so the map, alive on the machines it runs on.
        sys.path.insert(0, str(REPO_ROOT))
        from corvus.app import chromium_flags

        os.environ.setdefault("QTWEBENGINE_CHROMIUM_FLAGS", " ".join(chromium_flags()))
        # A background tool, not an application: no Dock icon, no menu bar.
        os.environ.setdefault("QT_MAC_DISABLE_FOREGROUND_APPLICATION_TRANSFORM", "1")

        plugins = _visible_plugins()
        if plugins:
            os.environ["QT_PLUGIN_PATH"] = os.pathsep.join(
                p for p in (plugins, os.environ.get("QT_PLUGIN_PATH", "")) if p)

        from PyQt6.QtCore import QEventLoop, Qt, QTimer, QUrl
        from PyQt6.QtWebEngineCore import QWebEnginePage, QWebEngineProfile
        from PyQt6.QtWebEngineWidgets import QWebEngineView
        from PyQt6.QtWidgets import QApplication

        self._Qt, self._QTimer, self._QEventLoop, self._QUrl = Qt, QTimer, QEventLoop, QUrl
        self._QWebEnginePage, self._QWebEngineProfile = QWebEnginePage, QWebEngineProfile
        self._QWebEngineView = QWebEngineView
        self.app = QApplication.instance() or QApplication([sys.argv[0], "--scene-camera"])
        self.view: Any = None
        self.page: Any = None
        self.profile: Any = None
        self._loaded = False
        self._load_ok = True
        self.zoom = 1.0

    # -- the event loop ----------------------------------------------------

    def spin(self, seconds: float) -> None:
        """Let Qt run for *seconds*: pages load, timers fire, the map draws.

        In short slices, so Python gets control back often enough for Ctrl-C
        to stop a fifty-second settle rather than wait it out.
        """
        remaining = max(0.001, seconds)
        while remaining > 0:
            chunk = min(0.2, remaining)
            loop = self._QEventLoop()
            self._QTimer.singleShot(max(1, int(chunk * 1000)), loop.quit)
            loop.exec()
            remaining -= chunk

    def _until(self, done: Callable[[], bool], timeout: float, step: float = 0.1) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if done():
                return True
            self.spin(step)
        return done()

    # -- one scene ---------------------------------------------------------

    def open(self, width: int, height: int) -> None:
        self.close()
        camera = self

        class _Page(self._QWebEnginePage):
            def javaScriptConsoleMessage(self, level, message, line, source):  # noqa: N802
                if camera.verbose:
                    camera.log(f"console    [{int(level.value)}] {message} ({source}:{line})")

        self.profile = self._QWebEngineProfile()          # off the record
        self.page = _Page(self.profile)
        self.page.loadFinished.connect(self._on_load)
        self.zoom = self._zoom_for(width, height)
        view = self._QWebEngineView()
        view.setPage(self.page)
        view.setAttribute(self._Qt.WidgetAttribute.WA_DontShowOnScreen, True)
        view.setWindowFlags(self._Qt.WindowType.FramelessWindowHint)
        view.resize(round(width * self.zoom), round(height * self.zoom))
        view.show()
        self.page.setZoomFactor(self.zoom)
        self.view = view
        self.spin(0.2)

    def _zoom_for(self, width: int, height: int) -> float:
        """The page zoom that fits the viewport on this screen, sharply.

        The window, hidden or not, is clamped to the screen: a 1000 px tall
        viewport on a 956 px laptop screen comes out 968 px tall with the rest
        of the picture black. So the window is made smaller and the page is
        zoomed out to fill the same CSS viewport. On a 2x display a zoom of
        one half does that exactly, one physical pixel per CSS pixel, which is
        a picture identical to the page at 1x on a large screen.
        """
        screen = self.app.primaryScreen()
        if screen is None:
            return 1.0
        ratio = screen.devicePixelRatio() or 1.0
        geometry = screen.geometry()
        fit = min(1.0, (geometry.width() - 8) / width, (geometry.height() - 8) / height)
        if not self.hidpi and ratio >= 2.0 and 1.0 / ratio <= fit:
            return 1.0 / ratio
        return fit

    def _on_load(self, ok: bool) -> None:
        self._loaded = True
        self._load_ok = bool(ok)

    def close(self) -> None:
        view, page, profile = self.view, self.page, self.profile
        self.view = self.page = self.profile = None
        if view is not None:
            view.hide()
            view.setPage(None)
            view.deleteLater()
        if page is not None:
            page.deleteLater()
        self.spin(0.1)
        if profile is not None:
            profile.deleteLater()
            self.spin(0.1)

    def navigate(self, url: str, timeout: float = 30.0) -> None:
        self._loaded = False
        self.page.load(self._QUrl(url))
        self.page.setZoomFactor(self.zoom)
        if not self._until(lambda: self._loaded, timeout):
            raise RuntimeError(f"{url} did not load within {timeout:.0f} s")
        if not self._load_ok:
            raise RuntimeError(f"{url} failed to load")
        self.page.setZoomFactor(self.zoom)

    def evaluate(self, expression: str, timeout: float = 60.0) -> str:
        """Run one step's JavaScript and wait for its value, promise or not."""
        self.page.runJavaScript(_RUN.format(expr=expression))
        box: dict[str, Any] = {}

        def poll() -> bool:
            if "result" in box:
                return True
            self.page.runJavaScript("JSON.stringify(window.__corvusScene)",
                                    lambda value: box.__setitem__("raw", value))
            raw = box.pop("raw", None)
            if raw and raw != "null":
                box["result"] = json.loads(raw)
            return "result" in box

        if not self._until(poll, timeout, step=0.15):
            raise RuntimeError("a recipe step did not finish in time")
        result = box["result"]
        if "error" in result:
            raise RuntimeError(f"a recipe step failed: {result['error']}")
        return str(result.get("value", ""))

    def screenshot(self, target: Path, size: tuple[int, int]) -> Path:
        """Grab the page and save it; the suffix picks the format."""
        pixmap = self.view.grab()
        if not self.hidpi and (pixmap.width(), pixmap.height()) != size:
            pixmap = pixmap.scaled(
                size[0], size[1], self._Qt.AspectRatioMode.IgnoreAspectRatio,
                self._Qt.TransformationMode.SmoothTransformation)
            pixmap.setDevicePixelRatio(1.0)
        target.parent.mkdir(parents=True, exist_ok=True)
        suffix = target.suffix.lower().lstrip(".")
        fmt = "JPEG" if suffix in ("jpg", "jpeg") else suffix.upper() or "PNG"
        quality = 90 if fmt == "JPEG" else -1
        if not pixmap.save(str(target), fmt, quality):
            raise RuntimeError(f"could not write {target}")
        return target

    # -- a whole recipe ----------------------------------------------------

    def run(self, steps: list[Step], target: Path) -> Path:
        """Follow *steps* in order and save the picture to *target*."""
        size = (1600, 1000)
        for index, step in enumerate(steps, 1):
            if step.action == "resize":
                wanted = json.loads(step.payload)
                size = (int(wanted["width"]), int(wanted["height"]))
                self.open(*size)
            elif step.action == "navigate":
                self.navigate(step.payload)
            elif step.action == "evaluate":
                answer = self.evaluate(step.payload)
                self.log(f"step {index:2d}    {answer}")
            elif step.action == "wait":
                seconds = float(step.payload or 0)
                self.log(f"step {index:2d}    waiting {seconds:.0f} s")
                self.spin(seconds)
            elif step.action == "screenshot":
                saved = self.screenshot(target, size)
                self.log(f"picture    {saved}")
        return target
