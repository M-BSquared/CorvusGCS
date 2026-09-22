#!/usr/bin/env python3
"""Corvus GCS standalone desktop app wrapper.

Launches the backend HTTP/SSE server in a background thread and opens the
UI in a PyQt6 QtWebEngine window — no browser needed. Handles clean
shutdown of both the server and the web engine.

Usage:
    python3 corvus/app.py [port] [mavlink_connection]

Chromium rendering flags are set to handle environments where GBM/EGL
is not available (falls back to Vulkan on Linux, or to software rendering
anywhere) — see :func:`chromium_flags`.
"""
from __future__ import annotations

import atexit
import logging
import os
import signal
import sys
import threading
import time

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, REPO_ROOT)
WEB_DIR = os.path.join(REPO_ROOT, "src")
def chromium_flags(platform: str = sys.platform) -> list[str]:
    """QtWebEngine's Chromium switches for this platform.

    Pure and parameterised so the platform branch is assertable without the
    platform — these decide whether the window paints at all on a field
    laptop, and "it works on mine" is not a test.

    What is here and why:

    * ``--ignore-gpu-blocklist`` — Chromium blocklists a lot of the embedded
      and virtualised GPUs this app actually runs on. Being on the list is
      not the same as being broken, and the fallbacks below cover it when it
      is.
    * ``--enable-unsafe-swiftshader`` — the software GL fallback. "unsafe" is
      Chromium's word for "not GPU-accelerated", not a security switch. This
      is what keeps MapLibre initialising on a machine with no usable driver:
      a VM, a remote session, an old laptop with a broken EGL stack.
    * ``--enable-webgl`` — MapLibre is WebGL; without it there is no map.
    * Vulkan, on Linux only. macOS has no Vulkan (Qt renders through Metal)
      and Windows goes through ANGLE/D3D, so asking for it there is at best a
      no-op and at worst a wasted GPU init on every launch.
    * ``--disable-gpu-sandbox``, on Linux only. The GPU process sandbox is a
      real boundary, and this frontend is the thing that loads operator-
      supplied plugins out of ``~/.corvus/plugins`` — so it is not a boundary
      to give up everywhere by default. It is here because it is the flag
      that makes the GPU stacks above come up on the Linux systems this app
      is deployed to (container, VM, odd driver, no GBM/EGL). On macOS and
      Windows the sandbox works and there is nothing to buy by dropping it,
      so those platforms keep it.

    Any of this can be replaced outright: the whole list is applied with
    ``setdefault``, so an operator who exports ``QTWEBENGINE_CHROMIUM_FLAGS``
    gets exactly what they asked for and none of the above.

    Deliberately absent: ``--disable-software-rasterizer``, which used to sit
    two lines under ``--enable-unsafe-swiftshader`` and turn it off again —
    SwiftShader *is* the software rasterizer, so the pair asked for opposite
    things and the machine that needed the fallback most is the one that lost
    it. Also absent: ``--force-fieldtrials=GPUHardwareRendering/Default``,
    which names no real Chromium field trial and did nothing.
    """
    flags = ["--ignore-gpu-blocklist"]
    if platform.startswith("linux"):
        flags += ["--enable-features=Vulkan", "--use-vulkan", "--disable-gpu-sandbox"]
    flags += ["--enable-unsafe-swiftshader", "--enable-webgl"]
    return flags


os.environ.setdefault("QTWEBENGINE_CHROMIUM_FLAGS", " ".join(chromium_flags()))

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    datefmt="%H:%M:%S",
)
logger = logging.getLogger("corvus.app")

from corvus import desktop_icon
from corvus.config import default_config_path, load_config
from corvus.instance_lock import (
    ALLOW_MULTI_ENV,
    InstanceLock,
    allow_multi,
    describe_peer,
)
from corvus.paths import corvus_path
from corvus.version import get_version


def _report_startup_failure(text: str, detail: str) -> None:
    """Tell the operator why this launch stopped, in a window they will see.

    A packaged build has no console attached: on macOS a double-clicked .app
    writes its log where nobody looks, and on Windows the .exe is built
    windowed. Without this, refusing to start looks identical to the app
    silently failing to start — the worst possible way to explain a
    deliberate decision. Best-effort: if Qt cannot be brought up at all, the
    log line already emitted by the caller is what is left.
    """
    try:
        from PyQt6.QtWidgets import QApplication, QMessageBox
        # Bound, not discarded: the QApplication has to outlive box.exec().
        # Naming it also gets the dialog a real application name instead of
        # the interpreter's, which is what the title bar and the macOS menu
        # bar read — this window is the only thing the operator will see.
        app = QApplication.instance() or QApplication(sys.argv)
        app.setApplicationName("CORVUS GCS")
        box = QMessageBox()
        box.setIcon(QMessageBox.Icon.Information)
        box.setWindowTitle("CORVUS GCS")
        box.setText(text)
        box.setInformativeText(detail)
        box.exec()
    except Exception:  # noqa: BLE001 - the log line is the fallback
        logger.debug("could not show the startup dialog", exc_info=True)


def _report_already_running(detail: str) -> None:
    """The one-instance refusal, as a window."""
    _report_startup_failure(
        "CORVUS GCS is already running.",
        f"{detail}\n\nBring the existing window forward, or set "
        f"{ALLOW_MULTI_ENV}=1 to run a second instance.",
    )


def _report_bad_port(reason: str) -> None:
    """A mistyped port is an operator error, so it gets an operator's answer."""
    _report_startup_failure(
        "CORVUS GCS could not start.",
        f"{reason}\n\nusage: corvus-gcs [PORT] [MAVLINK_CONNECTION]",
    )


def parse_port_arg(argv: list[str], default: int) -> tuple[int, str]:
    """Resolve the optional PORT argument; return ``(port, error)``.

    Split out of :func:`main` so the rule can be tested without bringing up
    Qt, and so the answer to a mistyped port is decided in one place rather
    than by whichever ``int()`` happened to run first.
    """
    if len(argv) <= 1:
        return default, ""
    raw = argv[1]
    try:
        port = int(raw)
    except (TypeError, ValueError):
        return default, f"invalid port {raw!r}"
    if not 1 <= port <= 65535:
        return default, f"port {port} is out of range (1-65535)"
    return port, ""


def webengine_profile_dir(multi: bool) -> str:
    """Directory QtWebEngine keeps its cache and local storage in.

    Chromium takes an exclusive lock on a profile directory. Two Corvus
    processes sharing one — which is what the *default* profile is — leaves
    the second with a profile it cannot write: local storage silently stops
    persisting, and on some builds the render process fails to come up at all
    and the window paints blank. The operator sees an empty application with
    no error anywhere.

    So the profile is named explicitly. A normal single-instance run always
    gets ``default``, which is what keeps the map's cached assets across
    restarts. A run that opted into multiple instances gets its own
    pid-suffixed directory, so the opt-out is actually usable instead of
    trading one silent failure for another.
    """
    name = f"instance-{os.getpid()}" if multi else "default"
    return corvus_path("webengine", name)


def find_free_port(preferred: int = 8000) -> int:
    """Return *preferred* if free, otherwise the next free port.

    Kept for callers outside this module. The launch path no longer uses it:
    the answer is stale the moment it is returned (the probe socket is closed
    before the server binds), so two windows opened together could both be
    told 8000 was free and one would then die on ``Address already in use``
    inside the server constructor. ``server.bind_server`` claims the port for
    real instead, and moving on to the next one is just its retry.
    """
    import socket
    for port in range(preferred, preferred + 20):
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
                s.bind(("", port))
                return port
        except OSError:
            continue
    return preferred


def start_backend(port: int, mavlink_conn: str | None = None) -> tuple:
    """Start the HTTP/SSE server and MAVLink bridge in-process.

    Returns ``(server, mavlink, ssh)``. The server may be listening on a
    *different* port than *port* — see :func:`corvus.server.bind_server`; read
    it back from ``server.server_address[1]``.

    The backend itself is ``corvus.server.create_server``, unchanged and
    entire. This function used to build one by hand instead: ninety lines
    that constructed the same store, bridge, ssh, config, auto-connect
    session, tile resources, buildings, geocoder, flash, SiK, RTK, logs, forwarder
    and update checker as browser mode, and then assigned each of them twice —
    once onto ``CorvusHandler``, once onto the server.

    It is worth being precise about what that cost, because it was not
    hypothetical. Every service had to be added in four places or it silently
    did not exist in one of the two modes, and twice it was not: the packaged
    app shipped an Analysis page that could not list a single ULog, and a
    Firmware page that listed no PX4 release. Both of those were this
    function's copy falling behind, and neither failed a test — the feature
    simply was not there in the build operators run.

    So there is one backend now, and the desktop app's only remaining job is
    to serve it from a thread of its own: the Qt event loop owns this one.
    """
    from corvus.server import create_server

    server = create_server(port=port, mavlink_conn=mavlink_conn)
    # serve_forever blocks; the desktop app's main thread belongs to Qt.
    # Daemon, because the teardown path (_stop_all -> server.shutdown) is what
    # stops this loop, and a hung request must never keep the process alive
    # after the window is gone.
    backend_thread = threading.Thread(
        target=server.serve_forever, name="corvus-backend", daemon=True,
    )
    backend_thread.start()
    return server, server.mavlink, server.ssh

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
    PyQt6 and without a display).
    """
    from PyQt6.QtCore import QRectF, Qt
    from PyQt6.QtGui import QColor, QPainter, QPainterPath, QPixmap

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
    from PyQt6.QtCore import Qt
    from PyQt6.QtGui import QPixmap

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
    from PyQt6.QtGui import QIcon

    path = app_icon_path(inverted)
    if not os.path.exists(path):
        logger.warning("app icon %s missing; keeping the current one", path)
        return None
    if not backplate:
        return QIcon(path)
    return QIcon(app_icon_pixmap(path, _ICON_RENDER_SIZE,
                                 app_icon_plate_color(inverted)))


def _stop_all(server) -> None:
    """Ordered, exception-safe backend teardown. Never raises.

    The sequence lives in :func:`corvus.server.stop_backend`, shared with
    serve.py. This used to be a character-for-character copy of that file's
    function, with a docstring that said so.
    """
    from corvus.server import stop_backend
    stop_backend(server, logger)

def set_windows_app_id(app_id: str) -> bool:
    """Tell Windows this process is its own application. No-op elsewhere.

    Without an explicit AppUserModelID, Windows groups the window under
    whatever launched it — the host Python, or the packaged launcher — and
    shows *that* icon in the taskbar and the alt-tab list. Which undoes the
    ``.ico`` ``build-windows.ps1`` goes to the trouble of cutting, and undoes
    :mod:`corvus.desktop_icon` on the one platform it does not otherwise
    cover.

    The value was already being computed and thrown away; this is the line
    that was missing. Returns whether the id was actually set, so the caller
    (and a test) can tell "not Windows" from "Windows said no".
    """
    if os.name != "nt":
        return False
    try:
        import ctypes
        ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID(app_id)
    except Exception:  # noqa: BLE001 - a taskbar icon is never worth a failed launch
        logger.debug("could not set the Windows app id", exc_info=True)
        return False
    return True


def main() -> int:
    from PyQt6.QtCore import QUrl
    from PyQt6.QtWebEngineCore import QWebEnginePage, QWebEngineProfile
    from PyQt6.QtWebEngineWidgets import QWebEngineView
    from PyQt6.QtWidgets import QApplication, QMainWindow, QVBoxLayout, QWidget

    cfg = load_config()
    cfg_path = default_config_path()
    # Forward-compat env hooks: the map/mavlink agents' default_cache_dir()/
    # default_log_dir() do not read these env vars yet, but setting them now
    # is harmless and lets those agents honour an operator override later
    # without touching app.py.
    if cfg.tile_cache_dir:
        os.environ["CORVUS_TILE_CACHE_DIR"] = cfg.tile_cache_dir
    if cfg.tlog_dir:
        os.environ["CORVUS_TLOG_DIR"] = cfg.tlog_dir
    logger.info("Config loaded from %s: mavlink=%s http_port=%d "
                "tile_cache_dir=%s tlog_dir=%s",
                cfg_path, cfg.mavlink_connection, cfg.http_port,
                cfg.tile_cache_dir or "(default)", cfg.tlog_dir or "(default)")

    # CLI args override the config file when present. A mistyped port reaches
    # a packaged build with no console attached, so a bare int() would end the
    # launch with a traceback nobody ever sees and no window — indistinguishable
    # from the app simply failing to start. serve.py answers this with a usage
    # line; here the answer has to be a dialog as well.
    port, port_error = parse_port_arg(sys.argv, cfg.http_port)
    if port_error:
        logger.error("%s\nusage: corvus-gcs [PORT] [MAVLINK_CONNECTION]", port_error)
        _report_bad_port(port_error)
        return 2
    # None, not the config value, when no argument was given: the startup
    # resolver treats an explicit argument as the operator's own decision and
    # lets it outrank a flight controller on a cable, so handing it the config
    # file's string would make auto-connect impossible to reach. The file is
    # passed separately, at its own priority.
    mavlink_conn = sys.argv[2] if len(sys.argv) > 2 else None

    # One ground station per machine. Taken BEFORE the serial link, the
    # forwarder's UDP port, the tile database or the config file are touched,
    # because the damage a second instance does is done at the moment it opens
    # them — see corvus/instance_lock.py for why a split serial stream is the
    # one that matters. CORVUS_ALLOW_MULTI=1 is the deliberate way past it.
    lock = InstanceLock()
    is_first = lock.acquire(port=None)
    if not is_first and not allow_multi():
        peer = describe_peer(lock.peer())
        logger.error("CORVUS GCS is %s. Bring that window forward, or set "
                     "%s=1 to run a second one.", peer, ALLOW_MULTI_ENV)
        _report_already_running(peer)
        return 1
    atexit.register(lock.release)
    # True only for a run that is knowingly sharing the machine with another
    # Corvus: the one case that needs its own QtWebEngine profile directory.
    multi_instance = not is_first
    if multi_instance:
        logger.warning("starting a SECOND CORVUS GCS instance (%s=1). The "
                       "serial link, the forwarder port and the config file "
                       "are not shareable — point this one at its own link.",
                       ALLOW_MULTI_ENV)

    logger.info("Starting backend on port %d (MAVLink: %s)", port,
                mavlink_conn or cfg.mavlink_connection)
    try:
        server, mavlink, ssh = start_backend(port, mavlink_conn)
    except OSError as exc:
        # Every port in the search span is taken. Say so instead of dying on a
        # bare traceback with no window ever having appeared.
        logger.error("could not start the backend: %s", exc)
        _report_already_running(f"could not open an HTTP port: {exc}")
        lock.release()
        return 1
    # bind_server may have landed on a different port than the one asked for.
    port = server.server_address[1]
    lock.write_port(port)

    # Set before the QApplication, because Windows binds a window to whatever
    # the AppUserModelID was when the window was created.
    set_windows_app_id(f"corvus.gcs.{get_version()}")
    app = QApplication(sys.argv)
    app.setApplicationName("CORVUS GCS")
    app.setApplicationDisplayName("CORVUS GCS")
    app.setApplicationVersion(get_version())

    window = QMainWindow()
    window.setWindowTitle(f"CORVUS GCS v{get_version()}")
    window.resize(1440, 1024)
    window.setMinimumSize(960, 600)

    central = QWidget()
    layout = QVBoxLayout(central)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(0)

    # A profile of our own rather than Chromium's shared default one, so a
    # second instance (CORVUS_ALLOW_MULTI=1) cannot contend for the profile
    # lock and end up with a blank window. Parented to `window` so Qt's
    # ownership keeps it alive: a profile collected while its page is still
    # open takes the render process down with it.
    profile_dir = webengine_profile_dir(multi_instance)
    try:
        os.makedirs(profile_dir, exist_ok=True)
        profile = QWebEngineProfile("corvus", window)
        profile.setPersistentStoragePath(profile_dir)
        profile.setCachePath(os.path.join(profile_dir, "cache"))
        web = QWebEngineView()
        web.setPage(QWebEnginePage(profile, web))
    except Exception:  # noqa: BLE001 - a window on the default profile beats no window
        logger.exception("could not set up a private web profile at %s; "
                         "falling back to the shared default", profile_dir)
        web = QWebEngineView()
    web.setUrl(QUrl(f"http://localhost:{port}/"))
    layout.addWidget(web)
    window.setCentralWidget(central)

    # Dock / taskbar icon, from the live config. Two cuts of the mark ship in
    # assets/: white artwork for a dark dock, black for a light one; either can
    # additionally be set on a filled backplate, which is the only variant that
    # reads on a dock of *any* shade. Applied here for the first paint and
    # re-checked on the timer below, so flipping either switch in
    # Settings -> Appearance lands without a restart. Nothing else in the app
    # reads this: it is the icon only, not a theme.
    #
    # Seeded with the off state, not None, because that is what the platform
    # already shows: all three build scripts cut the bundle icon
    # (.icns / .ico / .png) from the bare normal mark. An operator who never
    # touches the switches therefore keeps the packaged icon untouched — Qt is
    # only asked for an icon once the config actually asks for a different one.
    applied_icon: list[tuple[bool, bool]] = [(False, False)]
    # The launcher entry a Linux desktop integrator wrote is a file under
    # $HOME, not process state: it outlives the run that set it. So it gets its
    # own seed of None, which makes the first tick reconcile it once against
    # the config — otherwise an operator who turns a switch off would keep last
    # session's icon in the applications grid forever. After that it moves only
    # when the switches do.
    applied_launcher: list = [None]

    def sync_app_icon() -> None:
        cfg_live = getattr(server, "config", None)
        want = (app_icon_inverted(cfg_live), app_icon_backplate(cfg_live))

        if want != applied_icon[0]:
            applied_icon[0] = want      # never retry a missing file every tick
            icon = build_app_icon(*want)
            if icon is not None:
                app.setWindowIcon(icon)  # Dock on macOS, taskbar on Linux
                window.setWindowIcon(icon)

        if want != applied_launcher[0]:
            applied_launcher[0] = want
            inverted, backplate = want
            plate = app_icon_plate_color(inverted) if backplate else None
            source = app_icon_path(inverted)

            def render(src, dst, size, text):
                render_app_icon(src, dst, size, plate, text)

            # Two separate places a Linux desktop keeps a copy of the icon:
            # the launcher entry an integrator installed, and the thumbnail
            # the file manager paints on the .AppImage file. Neither is the
            # read-only .DirIcon inside the bundle, and both are corrected.
            try:
                desktop_icon.sync_integrated_icon(source, render)
            except Exception:           # a cosmetic file, never a reason to die
                logger.exception("launcher icon sync failed")
            try:
                desktop_icon.sync_appimage_thumbnail(source, render)
            except Exception:
                logger.exception("file thumbnail sync failed")

    sync_app_icon()

    import threading as _t
    # Test-and-set under a lock, not Event.is_set()/Event.set(). Four things
    # can call shutdown(): aboutToQuit, the atexit hook, the SIGINT/SIGTERM
    # handlers, and the belt-and-suspenders call after app.exec() returns. The
    # gap between checking the flag and setting it was wide enough for two of
    # them to both get through — and _stop_all() running twice concurrently
    # means two threads closing the same sockets, joining the same threads and
    # calling server_close() on an already-closed socket. The lock closes the
    # gap; the losers return immediately instead of blocking on a teardown
    # they have no reason to wait for.
    shutdown_lock = _t.Lock()
    shutting_down = _t.Event()

    def shutdown() -> None:
        with shutdown_lock:
            if shutting_down.is_set():
                return
            shutting_down.set()

        logger.info("Shutting down — stopping all connections …")
        _stop_all(server)

        # Bounded exit watchdog. The Qt event loop normally returns cleanly
        # and main() then sys.exit(ret); but QtWebEngine child processes
        # and stuck daemon threads can block interpreter teardown. The
        # daemon sleeps 2s and forces os._exit(0) if the main thread is
        # still alive then — the app never hangs on quit. It dies with the
        # process on a clean exit, so the normal path is just sys.exit.
        #
        # Why start AFTER _stop_all: so the watchdog cannot pre-empt the
        # bounded joins inside mavlink.stop() (which flushes the tlog) and
        # cut that flush short on a lossy link. It only guards the post-
        # teardown phase (Qt page deleteLater + the app.exec() return),
        # matching the same fix already applied in serve.py.
        def _watchdog() -> None:
            time.sleep(2.0)
            if threading.main_thread().is_alive():
                logger.warning("clean exit timed out after 2s; forcing os._exit(0)")
                os._exit(0)
        _t.Thread(target=_watchdog, name="exit-watchdog", daemon=True).start()

        try:
            web.page().deleteLater()
        except Exception:
            logger.exception("web page deleteLater failed")
        logger.info("All connections stopped.")

    app.aboutToQuit.connect(shutdown)
    atexit.register(shutdown)

    def quit_app(*_args) -> None:
        app.quit()

    signal.signal(signal.SIGINT, quit_app)
    signal.signal(signal.SIGTERM, quit_app)

    # Timer to process SIGINT promptly (Qt doesn't handle signals natively)
    from PyQt6.QtCore import QTimer
    timer = QTimer()
    timer.start(500)
    timer.timeout.connect(lambda: None)
    # Same tick picks up an app-icon change written by POST /api/config. A
    # dict lookup and a comparison; the QIcon is only rebuilt when the answer
    # actually changed, which is why polling here beats plumbing an event out
    # of the HTTP thread into the Qt loop.
    timer.timeout.connect(sync_app_icon)

    window.show()
    logger.info("CORVUS GCS v%s — standalone window ready", get_version())
    ret = app.exec()
    shutdown()  # belt-and-suspenders if aboutToQuit did not fire
    return ret


if __name__ == "__main__":
    sys.exit(main())
