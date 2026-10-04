"""The app icon: which cut of the mark is wanted, how it is drawn, and the
names a Linux desktop matches the running window by.

The decisions are plain functions, tested headless. Only the drawing needs
Qt, and its imports stay inside the functions that draw, so importing this
module never loads PySide6.
"""
from __future__ import annotations

import logging
import os
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

logger = logging.getLogger("corvus.app")


def app_icon_inverted(cfg) -> bool:
    """Whether the operator asked for the inverted cut of the mark.

    Reads the live config object the HTTP handlers mutate in place, so the
    Settings switch reaches the Dock / taskbar without a restart. Anything
    other than a genuine ``True`` means the normal (white artwork) mark.
    """
    ui = getattr(cfg, "ui", None)
    return isinstance(ui, dict) and ui.get("inverted_app_icon") is True


def app_icon_backplate(cfg) -> bool:
    """Whether the mark should be drawn on a filled rounded square.

    The shipped artwork is a bare silhouette on transparency, which is what
    makes it vanish against a dock of its own colour. A backplate gives it
    the contrast a platform icon is normally expected to carry on its own,
    and unlike the inversion it works whichever way the dock is shaded.

    Independent of :func:`app_icon_inverted`: the inversion picks the mark,
    this picks whether it gets a ground. Anything other than a genuine
    ``True`` means the bare mark, as before the switch existed.
    """
    ui = getattr(cfg, "ui", None)
    return isinstance(ui, dict) and ui.get("app_icon_backplate") is True


def desktop_id(env=None) -> str:
    """The name a Linux desktop knows this app by.

    It is the window's app id on Wayland, the WM_CLASS instance on X11, and
    what the ``StartupWMClass`` of a desktop entry has to say for a dock to
    give the window that entry's icon. The AppImage's entry says
    ``corvus-gcs``; a source checkout gets its own name, so a dev entry
    written by ``run.sh --desktop-entry`` never claims an installed
    AppImage's windows, or the other way round.
    """
    env = os.environ if env is None else env
    return "corvus-gcs" if env.get("APPIMAGE") else "corvus-gcs-dev"


def qt_argv(argv: list[str], app_id: str, platform: str = sys.platform) -> list[str]:
    """argv for the QApplication.

    On Linux ``-name`` sets the X11 WM_CLASS instance, which Qt otherwise
    takes from ``argv[0]``: ``app.py``, which matches no desktop entry. Qt's
    xcb plugin consumes the pair; under Wayland it is left over and ignored.
    """
    if not platform.startswith("linux"):
        return list(argv)
    return [*argv, "-name", app_id]


def ships_own_icon(platform: str = sys.platform, frozen: bool | None = None,
                   here: str = __file__) -> bool:
    """Whether the OS already shows a packaged icon for this process.

    True for the macOS ``.app`` (its ``.icns``) and the frozen Windows build
    (the ``.ico`` in the executable): there the default icon is left alone,
    so the bundle's own artwork is what the Dock and taskbar show. False on
    Linux, where only the window icon reaches an X11 taskbar, and for any
    source checkout, which has no packaged icon at all.
    """
    if frozen is None:
        frozen = bool(getattr(sys, "frozen", False))
    if platform == "darwin":
        return ".app/Contents/" in here
    if platform == "win32":
        return frozen
    return False


def app_icon_path(inverted: bool) -> str:
    """Absolute path of the PNG the Dock / taskbar icon is built from."""
    name = "CorvusGCS_logo_inverted.png" if inverted else "CorvusGCS_logo.png"
    return os.path.join(REPO_ROOT, "assets", name)


# Backplate geometry, as fractions of the icon's edge. The corner radius sits
# where every platform's icon grid rounds to, and the mark is inset so its
# wingtips keep clear of the corners instead of being clipped by them.
_PLATE_RADIUS = 0.225
_PLATE_INSET = 0.78
# The plate carries the contrast, so it takes the side of the theme the mark
# does not: the white mark gets the dark ground (--bg of the dark theme in
# src/css/themes.css), the black one gets white.
_PLATE_DARK = "#0B0E12"
_PLATE_LIGHT = "#FFFFFF"
# Size the icon is rasterised at when nothing else asks for one. Large enough
# that macOS and GNOME downsample rather than upscale it.
_ICON_RENDER_SIZE = 512


def app_icon_plate_color(inverted: bool) -> str:
    """The backplate colour that puts the chosen cut of the mark in relief."""
    return _PLATE_LIGHT if inverted else _PLATE_DARK


def app_icon_pixmap(source: str, size: int, plate: str):
    """The mark centred on a filled rounded square — a square QPixmap.

    Qt is imported here rather than at module scope so ``corvus.app`` keeps
    importing headless (the icon-selection helpers above are tested without
    PySide6 and without a display).
    """
    from PySide6.QtCore import QRectF, Qt
    from PySide6.QtGui import QColor, QPainter, QPainterPath, QPixmap

    canvas = QPixmap(size, size)
    canvas.fill(QColor(0, 0, 0, 0))
    painter = QPainter(canvas)
    try:
        painter.setRenderHint(QPainter.RenderHint.Antialiasing, True)
        painter.setRenderHint(QPainter.RenderHint.SmoothPixmapTransform, True)
        path = QPainterPath()
        radius = size * _PLATE_RADIUS
        path.addRoundedRect(QRectF(0, 0, size, size), radius, radius)
        painter.fillPath(path, QColor(plate))
        mark = QPixmap(source)
        if not mark.isNull():
            inner = max(1, int(size * _PLATE_INSET))
            mark = mark.scaled(inner, inner, Qt.AspectRatioMode.KeepAspectRatio,
                               Qt.TransformationMode.SmoothTransformation)
            painter.drawPixmap((size - mark.width()) // 2,
                               (size - mark.height()) // 2, mark)
    finally:
        painter.end()                # before the pixmap is handed on, always
    return canvas


def render_app_icon(source: str, dest: str, size, plate, text=None) -> None:
    """Write the app icon to *dest* as a PNG. Raises if it cannot.

    This is the renderer :mod:`corvus.desktop_icon` calls for every file it
    rewrites; *size* is the one the icon theme's directory or the thumbnail
    tier asks for, or ``None`` where nothing has an opinion.

    *text* becomes PNG ``tEXt`` chunks. Empty for a launcher icon; for a file
    thumbnail it carries the ``Thumb::URI`` / ``Thumb::MTime`` pair the
    freedesktop spec requires, which is the difference between a PNG the file
    manager adopts and one it ignores. It goes through QImage because QPixmap
    has no text keys — and the conversion is needed for the save anyway.
    """
    from PySide6.QtCore import Qt
    from PySide6.QtGui import QPixmap

    if plate:
        pixmap = app_icon_pixmap(source, int(size or _ICON_RENDER_SIZE), plate)
    else:
        pixmap = QPixmap(source)
        if pixmap.isNull():
            raise ValueError(f"cannot read {source}")
        if size:
            pixmap = pixmap.scaled(int(size), int(size),
                                   Qt.AspectRatioMode.KeepAspectRatio,
                                   Qt.TransformationMode.SmoothTransformation)
    image = pixmap.toImage()
    for key, value in (text or {}).items():
        image.setText(key, value)
    if not image.save(dest, "PNG"):
        raise OSError(f"cannot write {dest}")


def build_app_icon(inverted: bool, backplate: bool):
    """The QIcon for the Dock / taskbar, or ``None`` if the artwork is gone."""
    from PySide6.QtGui import QIcon

    path = app_icon_path(inverted)
    if not os.path.exists(path):
        logger.warning("app icon %s missing; keeping the current one", path)
        return None
    if not backplate:
        return QIcon(path)
    return QIcon(app_icon_pixmap(path, _ICON_RENDER_SIZE,
                                 app_icon_plate_color(inverted)))
