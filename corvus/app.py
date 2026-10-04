#!/usr/bin/env python3
"""Corvus GCS standalone desktop app wrapper.

Launches the backend HTTP/SSE server in a background thread and opens the
UI in a PySide6 QtWebEngine window — no browser needed. Handles clean
shutdown of both the server and the web engine.

Usage:
    python3 corvus/app.py [port] [mavlink_connection]

Chromium rendering flags are set to handle environments where GBM/EGL
is not available (falls back to Vulkan on Linux, or to software rendering
anywhere) — see :func:`chromium_flags`.
"""
from __future__ import annotations

import atexit
import json
import logging
import re
import os
import signal
import sys
import threading
import time
from urllib.parse import parse_qs, urlsplit

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


def qpa_platform(env, platform: str = sys.platform) -> str:
    """The Qt platform to ask for, or ``""`` to leave Qt its own choice.

    On a Linux desktop running Wayland with XWayland beside it, X11 first:
    ``"xcb;wayland"``. A Wayland client may not place its own windows, read
    where the pointer is on the screen, or keep a window above others, and
    the windows out of the app need all three: dragged by their bar, docked
    back by letting go over the app, pinned above it. Under XWayland they
    work as on any X11 desktop. Wayland stays the fallback in the same value,
    so a machine whose Qt cannot load its X11 plugin still starts (see
    :func:`window_support` for what is then left out). An operator who set
    ``QT_QPA_PLATFORM`` gets exactly that.
    """
    if not platform.startswith("linux") or env.get("QT_QPA_PLATFORM"):
        return ""
    wayland = bool(env.get("WAYLAND_DISPLAY")) or env.get("XDG_SESSION_TYPE", "").lower() == "wayland"
    return "xcb;wayland" if wayland and env.get("DISPLAY") else ""


_QPA = qpa_platform(os.environ)
if _QPA:
    from corvus.child_env import QPA_DEFAULT_ENV

    os.environ["QT_QPA_PLATFORM"] = _QPA
    # So corvus.child_env can keep it from the programs Corvus starts.
    os.environ[QPA_DEFAULT_ENV] = _QPA

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    datefmt="%H:%M:%S",
)
logger = logging.getLogger("corvus.app")

from corvus import desktop_icon
from corvus.app_icon import (
    app_icon_backplate,
    app_icon_inverted,
    app_icon_path,
    app_icon_plate_color,
    build_app_icon,
    desktop_id,
    qt_argv,
    render_app_icon,
    ships_own_icon,
)
from corvus.config import default_config_path, load_config
from corvus.instance_lock import (
    ALLOW_MULTI_ENV,
    InstanceLock,
    allow_multi,
    describe_peer,
)
from corvus.file_manager import open_url
from corvus.native_window import mac_blur, win32_windows
from corvus.paths import corvus_path
from corvus.qt_plugins import visible_plugin_dir
from corvus.version import get_version


def use_visible_qt_plugins() -> str | None:
    """Load Qt's plugins through links when macOS has flagged them hidden.

    Must run before the first QApplication, which is when Qt loads its
    platform plugin. See :mod:`corvus.qt_plugins` for why a run from a checkout
    in a folder a sync agent manages needs it. A library path rather than
    ``QT_PLUGIN_PATH``, so no program Corvus starts inherits the links.
    Returns the folder of links, or ``None`` when Qt can see its plugins.
    """
    from PySide6.QtCore import QCoreApplication, QLibraryInfo

    try:
        links = visible_plugin_dir(QLibraryInfo.path(QLibraryInfo.LibraryPath.PluginsPath))
    except OSError:
        logger.warning("Qt's plugins are flagged hidden and could not be linked; "
                       "Qt may not find its platform plugin", exc_info=True)
        return None
    if links:
        QCoreApplication.addLibraryPath(links)
        logger.info("Qt's plugins are flagged hidden here; loading them through %s", links)
    return links


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
        from PySide6.QtWidgets import QApplication, QMessageBox
        use_visible_qt_plugins()
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

# ---------------------------------------------------------------------------
# Pop-out windows
# ---------------------------------------------------------------------------
#
# A floating camera or terminal frame lives inside the page, so it cannot leave
# the app's window. Its pop-out button calls window.open (src/js/popout.js),
# and these are the rules the native window that makes is held to. Pure, so
# they are tested without Qt.

POPOUT_MIN_W = 320
POPOUT_MIN_H = 200


def popout_url_allowed(url: str, port: int) -> bool:
    """Whether a window the page opened may show *url*: this backend's own pages only.

    A pop-out has no address bar and no business showing the web. Anything
    else it is sent to goes to the system browser instead (see
    :func:`external_url`), or nowhere.
    """
    if url in ("", "about:blank"):
        return True
    try:
        parts = urlsplit(url)
        url_port = parts.port
    except ValueError:
        return False
    return (parts.scheme == "http"
            and (parts.hostname or "") in ("localhost", "127.0.0.1")
            and url_port == port)


# A pop-out's key names the frame it came from ("video:<id>", "term:<session>")
# and travels in its address. Narrow, because it also keys the native window.
_POPOUT_KEY_RE = re.compile(r"^(video|term):[A-Za-z0-9_./@:-]{1,96}$")


def popout_key_valid(key: str) -> bool:
    """Whether *key* can name a pop-out window."""
    return isinstance(key, str) and bool(_POPOUT_KEY_RE.match(key))


def popout_key(url: str) -> str:
    """The key a popout.html address names, or ``""``."""
    try:
        values = parse_qs(urlsplit(url).query).get("key") or [""]
    except ValueError:
        return ""
    return values[0] if popout_key_valid(values[0]) else ""


# Runs in every page of the app's profile before the page's own scripts: it
# connects the page to the Qt object it may ask things of (see PopoutBridge
# and MainBridge in main()). A page without a web channel, such as one in a
# plain browser, has no ``qt`` object and nothing happens. It runs before the
# document has an element, so it must not touch the DOM.
NATIVE_BOOTSTRAP_JS = """
;(function () {
  if (typeof qt === "undefined" || !qt.webChannelTransport || typeof QWebChannel !== "function") return;
  new QWebChannel(qt.webChannelTransport, function (channel) {
    window.corvusNative = channel.objects.corvusNative || null;
    window.dispatchEvent(new CustomEvent("corvus:native-ready"));
  });
})();
"""


def native_support_js(support: dict[str, bool]) -> str:
    """The line of the bootstrap that tells a page what its windows can do.

    Set before any script of the page runs, so a page decides once, and
    synchronously, whether to offer a drag out of the app or a pin (see
    :func:`window_support`).
    """
    return "\n;window.corvusNativeSupport = Object.freeze(" + json.dumps(support) + ");\n"


def external_url(url: str) -> bool:
    """Whether *url* may be handed to the system browser: a web page, nothing local."""
    try:
        return urlsplit(url).scheme in ("http", "https")
    except ValueError:
        return False


def stays_on_top(pinned: bool, app_active: bool) -> bool:
    """Whether a pop-out window is raised above every other window right now.

    The pin keeps a window above the Corvus window, not above everything: a
    camera floating over another program's windows is in the way there. So the
    window is held on top only while Corvus is the active application; with
    another program in front it is an ordinary window its windows can cover,
    and it is on top again the moment Corvus is.
    """
    return bool(pinned) and bool(app_active)


def window_support(platform_name: str) -> dict[str, bool]:
    """What the windows out of the app can do on this Qt platform.

    ``place``: this process may put a window where it wants and knows where
    the pointer is, which dragging by the bar, following the pointer out of
    the app and docking by letting go over it all rest on. ``pin``: a window
    can be kept above others. Wayland gives a client neither: the compositor
    moves and sizes the window (``startSystemMove``), and there is no pin.
    The pages read this before they offer either (the bootstrap script puts
    it on ``window.corvusNativeSupport``), so a missing feature is left out
    rather than offered and broken.

    ``frost``: the desktop behind a window can be blurred (:class:`MacBlur`),
    which a frosted terminal in a window of its own needs. macOS only. On
    Linux a client has no way to ask for it: Cinnamon (Linux Mint) and GNOME
    (Ubuntu) blur nothing behind another program's window, and only KWin has
    a protocol for it. There, and on Windows, such a window stays solid; the
    frames inside the app are frosted by the page itself everywhere.
    """
    name = str(platform_name or "").lower()
    wayland = name.startswith("wayland")
    return {"place": not wayland, "pin": not wayland, "frost": name == "cocoa"}


def popout_geometry(rect: tuple[int, int, int, int],
                    available: tuple[int, int, int, int]) -> tuple[int, int, int, int]:
    """Where a pop-out opens: the rect the page asked for, kept on the screen.

    *rect* and *available* are ``(x, y, width, height)``; *available* is the
    usable area of the screen the rect is on. The page asks for the spot its
    frame was at, which is on screen by construction, but a window must never
    open somewhere it cannot be grabbed: off an unplugged monitor, under the
    menu bar, or bigger than the screen.
    """
    x, y, w, h = rect
    ax, ay, aw, ah = available
    w = max(min(w, aw), min(POPOUT_MIN_W, aw))
    h = max(min(h, ah), min(POPOUT_MIN_H, ah))
    x = min(max(x, ax), ax + aw - w)
    y = min(max(y, ay), ay + ah - h)
    return x, y, w, h


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
    from PySide6.QtCore import (
        QFile, QIODevice, QObject, QPoint, QRect, QSize, Qt, QTimer, QUrl, Slot,
    )
    from PySide6.QtGui import QGuiApplication
    from PySide6.QtWebChannel import QWebChannel
    from PySide6.QtWebEngineCore import (
        QWebEnginePage, QWebEngineProfile, QWebEngineScript, QWebEngineSettings,
    )
    from PySide6.QtWebEngineWidgets import QWebEngineView
    from PySide6.QtWidgets import QApplication, QMainWindow, QVBoxLayout, QWidget

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
                       "are not shareable. Point this one at its own link.",
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
    use_visible_qt_plugins()
    # Before the QApplication, so the first window already carries the id a
    # Linux dock matches against the desktop entry's StartupWMClass.
    app_id = desktop_id()
    QGuiApplication.setDesktopFileName(app_id)
    app = QApplication(qt_argv(sys.argv, app_id))
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

    def configure_view(view) -> None:
        # A WebRTC camera's picture arrives seconds after the click that opened
        # its window, by which time the click no longer counts as one. The
        # video is muted, and this app plays no other media.
        try:
            view.settings().setAttribute(
                QWebEngineSettings.WebAttribute.PlaybackRequiresUserGesture, False)
        except Exception:  # noqa: BLE001 - a camera that needs a click beats no window
            logger.exception("could not allow camera video to start by itself")
        # Copying: a terminal's selection, a parameter, a log path. QtWebEngine
        # refuses navigator.clipboard.writeText without this, on every
        # platform, and the copy failed without a word. Writing only: reading
        # the clipboard (JavascriptCanPaste) stays off, and a paste from the
        # keyboard needs neither.
        try:
            view.settings().setAttribute(
                QWebEngineSettings.WebAttribute.JavascriptCanAccessClipboard, True)
        except Exception:  # noqa: BLE001 - copying is a convenience
            logger.exception("could not allow copying to the clipboard")

    # ---- windows out of the app ----------------------------------------------
    #
    # A camera or terminal frame dragged past the edge of the app goes on in a
    # native window of its own (src/js/popout.js), which the operating system
    # lets the operator put anywhere, another screen included. The window is
    # frameless and transparent: the page draws the very same frame the app
    # does, bar, buttons and grip, so the frame does not change as it crosses
    # the edge, and there is no second, native title bar with its three
    # buttons. The page moves and sizes its window through PopoutBridge.
    #
    # Parented to the main window so it closes with it and never keeps the app
    # running on its own (only a window without a parent counts as the last
    # one for quitOnLastWindowClosed).
    popout_windows: dict = {}   # key -> window, once its page has named it
    # key -> where its window was when it was last closed, for this run, so a
    # camera opened again comes back to the screen and the spot it was left on.
    popout_last: dict = {}
    # key -> whether its window is pinned above the Corvus window (the pin in
    # its bar), for this run, so a camera opened again is pinned again.
    popout_pinned: dict = {}
    support = window_support(QGuiApplication.platformName())
    win = win32_windows()
    mac = mac_blur() if support["frost"] else None
    if not support["place"]:
        logger.info("Qt platform %s: windows out of the app are moved by the compositor "
                    "and cannot be pinned", QGuiApplication.platformName())

    def set_on_top(holder, on: bool) -> None:
        """Hold a window above every other window, or stop.

        The flag goes to the live native window (QWindow.setFlags) rather than
        through QWidget.setWindowFlags, which would hide the window and build
        it again: a camera would blink out and reconnect for a click on a pin.
        overrideWindowFlags keeps the widget's own record in step, so a window
        not made yet is made with the flag.
        """
        flags = holder.windowFlags()
        if on:
            flags |= Qt.WindowType.WindowStaysOnTopHint
        else:
            flags &= ~Qt.WindowType.WindowStaysOnTopHint
        holder.overrideWindowFlags(flags)
        handle = holder.windowHandle()
        if handle is not None:
            handle.setFlags(flags)

    def app_active() -> bool:
        return QGuiApplication.applicationState() == Qt.ApplicationState.ApplicationActive

    def step_back(holder) -> None:
        """A pinned window as another program comes forward: behind it, not over it.

        Dropping the flag alone moves the window to the top of the ordinary
        windows on Windows and on X11, which is above the program that just
        came forward. macOS keeps the order of the level it leaves.
        """
        if win is not None:
            try:
                win.step_back(int(holder.winId()))
            except Exception:  # noqa: BLE001 - the order is cosmetic
                logger.debug("could not step a window back", exc_info=True)
        elif QGuiApplication.platformName() == "xcb":
            holder.lower()

    def apply_pin(holder) -> None:
        """Put a window at the level its pin and the app's state call for."""
        if not support["pin"]:
            return
        active = app_active()
        want = stays_on_top(holder.pinned, active)
        if want != bool(holder.windowFlags() & Qt.WindowType.WindowStaysOnTopHint):
            set_on_top(holder, want)
            if not want and not active:
                step_back(holder)

    def pin(holder, on: bool) -> None:
        holder.pinned = bool(on) and support["pin"]
        if holder.key:
            popout_pinned[holder.key] = holder.pinned
        apply_pin(holder)

    # Corvus in front: pinned windows above everything of Corvus'. Another
    # program in front: they are ordinary windows it can cover.
    def on_app_state(_state) -> None:
        for holder in list(popout_windows.values()):
            try:
                apply_pin(holder)
            except RuntimeError:     # a window deleted a moment ago
                pass

    app.applicationStateChanged.connect(on_app_state)

    class PopoutWindow(QWidget):
        """The native window a camera or terminal gets. Remembers where it was."""

        def __init__(self):
            super().__init__(window, Qt.WindowType.Window | Qt.WindowType.FramelessWindowHint)
            self.key = ""
            self.normal = None      # the geometry before a maximize
            self.pinned = False     # the pin in its bar (see apply_pin)
            self.blur = 0           # the blur behind it (MacBlur), while frosted

        def unfrost(self) -> None:
            if self.blur and mac is not None:
                try:
                    mac.clear(self.blur)
                except Exception:  # noqa: BLE001 - the window goes either way
                    logger.debug("could not take a window's blur away", exc_info=True)
            self.blur = 0

        def closeEvent(self, event):  # noqa: N802 - Qt's name
            if self.key:
                popout_last[self.key] = QRect(self.normal or self.geometry())
            self.unfrost()
            super().closeEvent(event)

    def screen_area_at(point) -> tuple[int, int, int, int]:
        screen = (QGuiApplication.screenAt(point) or window.screen()
                  or QGuiApplication.primaryScreen())
        area = screen.availableGeometry()
        return area.x(), area.y(), area.width(), area.height()

    def settle(holder) -> None:
        """Keep a window that was let go of where it can be grabbed again."""
        g = holder.geometry()
        now = (g.x(), g.y(), g.width(), g.height())
        fitted = popout_geometry(now, screen_area_at(g.center()))
        if fitted != now:
            holder.setGeometry(*fitted)

    def over_app(point) -> bool:
        """Whether *point* (global) is over the app's page, which is shown."""
        if not window.isVisible() or window.isMinimized():
            return False
        return QRect(web.mapToGlobal(QPoint(0, 0)), web.size()).contains(point)

    def place_popout(holder, rect) -> None:
        x, y, w, h = popout_geometry(
            (rect.x(), rect.y(), rect.width(), rect.height()), screen_area_at(rect.center()))
        holder.setGeometry(x, y, w, h)

    class PopoutBridge(QObject):
        """What a window out of the app may ask of its own window, and nothing else.

        Moves and sizes are relative to where a gesture started (``dragStart``),
        in the pointer's own screen deltas, so they are right on every screen
        whatever the window's position was. On Windows a move follows the
        pointer in Windows' own pixels instead (:class:`Win32Windows`), and on
        Wayland the page hands the gesture to the compositor (``systemMove``,
        ``systemResize``): see :func:`window_support`.
        """

        def __init__(self, holder):
            super().__init__(holder)
            self._holder = holder
            self._start = None
            self._grab = None       # Windows: (hwnd, dx, dy), pointer to window, in its pixels

        @Slot()
        def dragStart(self) -> None:
            self._start = self._holder.geometry()
            self._grab = None
            if win is not None:
                try:
                    hwnd = int(self._holder.winId())
                    cx, cy = win.cursor()
                    ox, oy = win.origin(hwnd)
                    self._grab = (hwnd, cx - ox, cy - oy)
                except Exception:  # noqa: BLE001 - Qt's move is the fallback
                    logger.debug("native drag unavailable", exc_info=True)

        @Slot(int, int)
        def dragTo(self, dx: int, dy: int) -> None:
            if self._start is None or self._holder.normal is not None:
                return
            if self._grab is not None:
                hwnd, gx, gy = self._grab
                try:
                    cx, cy = win.cursor()
                    win.move(hwnd, cx - gx, cy - gy)
                    return
                except Exception:  # noqa: BLE001 - Qt's move is the fallback
                    self._grab = None
            self._holder.move(self._start.x() + dx, self._start.y() + dy)

        @Slot(result=bool)
        def systemMove(self) -> bool:
            """Hand the move to the window system (Wayland: the only way to move)."""
            handle = self._holder.windowHandle()
            return bool(handle is not None and self._holder.normal is None
                        and handle.startSystemMove())

        @Slot(result=bool)
        def systemResize(self) -> bool:
            """Hand a resize from the corner grip to the window system."""
            handle = self._holder.windowHandle()
            return bool(handle is not None
                        and handle.startSystemResize(Qt.Edge.RightEdge | Qt.Edge.BottomEdge))

        @Slot(int, int)
        def resizeTo(self, dw: int, dh: int) -> None:
            if self._start is not None:
                self._holder.normal = None
                self._holder.resize(max(POPOUT_MIN_W, self._start.width() + dw),
                                    max(POPOUT_MIN_H, self._start.height() + dh))

        @Slot(int, int, result=str)
        def dragEnd(self, x: int, y: int) -> str:
            """End a gesture let go of at (x, y) in this window's page.

            Says whether that was over the app, and where the window's top left
            is in the app's page, so it can go back in right there. The point
            is the page's own, mapped here: Chromium's screen coordinates in
            QtWebEngine leave out the window's title bar.
            """
            self._start = None
            self._grab = None
            if self._holder.normal is not None or not support["place"]:
                return json.dumps({"inside": False})
            released = self._holder.mapToGlobal(QPoint(x, y))
            settle(self._holder)
            if not over_app(released):
                return json.dumps({"inside": False})
            origin = web.mapFromGlobal(self._holder.pos())
            return json.dumps({"inside": True, "left": origin.x(), "top": origin.y(),
                               "width": self._holder.width(), "height": self._holder.height()})

        @Slot(result=bool)
        def toggleMaximize(self) -> bool:
            if not support["place"]:
                # Where a window may not place itself, the compositor maximizes it.
                if self._holder.isMaximized():
                    self._holder.normal = None
                    self._holder.showNormal()
                    return False
                self._holder.normal = self._holder.geometry()
                self._holder.showMaximized()
                return True
            if self._holder.normal is not None:
                self._holder.setGeometry(self._holder.normal)
                self._holder.normal = None
                return False
            self._holder.normal = self._holder.geometry()
            self._holder.setGeometry(*screen_area_at(self._holder.geometry().center()))
            return True

        @Slot(bool, result=bool)
        def setPinned(self, on: bool) -> bool:
            """Keep this window above the Corvus window (the pin in its bar)."""
            pin(self._holder, bool(on))
            return self._holder.pinned

        @Slot(result=bool)
        def isPinned(self) -> bool:
            return self._holder.pinned

        @Slot(bool, bool, float, result=bool)
        def setFrosted(self, on: bool, dark: bool, radius: float) -> bool:
            """Blur the desktop behind this window, or stop (frosted terminals).

            Answers whether the blur is there. The page turns see-through only
            when it is: a terminal over a sharp desktop is not readable, so
            where there is no blur the terminal stays solid.
            """
            holder = self._holder
            if not on or mac is None:
                holder.unfrost()
                return False
            try:
                holder.blur = mac.frost(int(holder.winId()), holder.blur, bool(dark),
                                        float(radius), holder.width(), holder.height())
            except Exception:  # noqa: BLE001 - a solid terminal is the fallback
                logger.debug("could not blur behind a window", exc_info=True)
                return False
            return True

        @Slot()
        def raiseWindow(self) -> None:
            self._holder.show()
            self._holder.raise_()
            self._holder.activateWindow()

        @Slot()
        def closeWindow(self) -> None:
            self._holder.close()

    class PopoutPage(QWebEnginePage):
        """A pop-out's page: Corvus' own pages only, never the web."""

        def __init__(self, page_profile, parent, holder):
            super().__init__(page_profile, parent)
            self.holder = holder

        def acceptNavigationRequest(self, url, nav_type, is_main_frame):  # noqa: N802 - Qt's name
            text = url.toString()
            if popout_url_allowed(text, port):
                key = popout_key(text) if is_main_frame else ""
                if key and key not in popout_windows:
                    popout_windows[key] = self.holder
                    self.holder.key = key
                    if popout_pinned.get(key):
                        pin(self.holder, True)
                    self.holder.destroyed.connect(
                        lambda *_: popout_windows.pop(key, None))
                return True
            if is_main_frame:
                if external_url(text):
                    open_url(text)
                QTimer.singleShot(0, self.holder.close)
            return False

        def createWindow(self, _type):  # noqa: N802 - Qt's name
            return open_popout(self.profile())

    def show_popout(holder) -> None:
        if not holder.isVisible():
            holder.show()
            holder.raise_()

    def open_popout(page_profile):
        holder = PopoutWindow()
        holder.setAttribute(Qt.WidgetAttribute.WA_DeleteOnClose, True)
        # The page draws the frame, rounded corners included; around them the
        # window has to be see-through, or the corners would be square again.
        holder.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground, True)
        holder.setWindowTitle("CORVUS GCS")
        holder.setMinimumSize(POPOUT_MIN_W, POPOUT_MIN_H)
        holder.resize(640, 400)
        box = QVBoxLayout(holder)
        box.setContentsMargins(0, 0, 0, 0)
        box.setSpacing(0)
        view = QWebEngineView(holder)
        page = PopoutPage(page_profile, view, holder)
        page.setBackgroundColor(Qt.GlobalColor.transparent)
        channel = QWebChannel(page)
        channel.registerObject("corvusNative", PopoutBridge(holder))
        page.setWebChannel(channel)
        view.setPage(page)
        configure_view(view)
        box.addWidget(view)
        page.titleChanged.connect(holder.setWindowTitle)
        page.windowCloseRequested.connect(holder.close)
        page.geometryChangeRequested.connect(lambda rect: place_popout(holder, rect))
        # Shown once its page has drawn, so it never flashes up empty.
        page.loadFinished.connect(lambda ok: show_popout(holder) if ok else holder.close())
        return page

    def window_for(key: str, query: str):
        """The native window for `key`, made and pointed at its page if there is none."""
        holder = popout_windows.get(key)
        if holder is not None:
            return holder, False
        url = f"http://localhost:{port}/popout.html?{query}"
        if not popout_url_allowed(url, port) or popout_key(url) != key:
            return None, False
        page = open_popout(web.page().profile())
        holder = page.holder
        holder.key = key
        if popout_pinned.get(key):
            pin(holder, True)
        popout_windows[key] = holder
        holder.destroyed.connect(lambda *_: popout_windows.pop(key, None))
        page.setUrl(QUrl(url))
        return holder, True

    class MainBridge(QObject):
        """What the app's page may ask: a window for a frame, moved, closed.

        Coordinates are the page's own (CSS pixels from the top left of the
        page, the same as the pointer's clientX and clientY), so the page
        never has to know where its window is on the screen.
        """

        def __init__(self, parent=None):
            super().__init__(parent)
            # key -> (hwnd, dx, dy): on Windows, where the pointer holds the
            # window being dragged out, in Windows' own pixels (see Win32Windows).
            self._grabs: dict = {}

        def _rect(self, left: int, top: int, width: int, height: int):
            return QRect(web.mapToGlobal(QPoint(left, top)),
                         QSize(max(POPOUT_MIN_W, width), max(POPOUT_MIN_H, height)))

        @Slot(str, str, int, int, int, int)
        def openWindow(self, key: str, query: str, left: int, top: int,
                       width: int, height: int) -> None:
            """A window of its own straight away: where it was last, or where
            the frame would have opened in the app. An open one comes forward."""
            if not popout_key_valid(key) or len(query) > 4096:
                return
            holder, made = window_for(key, query)
            if holder is None:
                return
            if not made:
                # Still loading, it shows itself once drawn (open_popout).
                if holder.isVisible():
                    holder.raise_()
                    holder.activateWindow()
                return
            rect = popout_last.get(key) or self._rect(left, top, width, height)
            holder.setGeometry(*popout_geometry(
                (rect.x(), rect.y(), rect.width(), rect.height()), screen_area_at(rect.center())))

        @Slot(str, str, int, int, int, int)
        def detachWindow(self, key: str, query: str, left: int, top: int,
                         width: int, height: int) -> None:
            """A frame dragged out of the app: its window, exactly where the frame is."""
            if not popout_key_valid(key) or len(query) > 4096:
                return
            holder, _ = window_for(key, query)
            if holder is None:
                return
            holder.setGeometry(self._rect(left, top, width, height))
            self._grabs.pop(key, None)
            if win is not None:
                try:
                    hwnd = int(holder.winId())
                    cx, cy = win.cursor()
                    ox, oy = win.origin(hwnd)
                    self._grabs[key] = (hwnd, cx - ox, cy - oy)
                except Exception:  # noqa: BLE001 - Qt's move is the fallback
                    logger.debug("native drag unavailable", exc_info=True)

        @Slot(str, int, int)
        def moveWindow(self, key: str, left: int, top: int) -> None:
            holder = popout_windows.get(key)
            if holder is None:
                return
            grab = self._grabs.get(key)
            if grab is not None:
                hwnd, gx, gy = grab
                try:
                    cx, cy = win.cursor()
                    win.move(hwnd, cx - gx, cy - gy)
                    return
                except Exception:  # noqa: BLE001 - Qt's move is the fallback
                    self._grabs.pop(key, None)
            holder.move(web.mapToGlobal(QPoint(left, top)))

        @Slot(str)
        def settleWindow(self, key: str) -> None:
            self._grabs.pop(key, None)
            holder = popout_windows.get(key)
            if holder is not None:
                settle(holder)

        @Slot(str)
        def closeWindow(self, key: str) -> None:
            self._grabs.pop(key, None)
            holder = popout_windows.get(key)
            if holder is not None:
                holder.close()

    class MainPage(QWebEnginePage):
        """The app's page. window.open gets a real window, for pop-outs."""

        def createWindow(self, _type):  # noqa: N802 - Qt's name
            return open_popout(self.profile())

    def native_script():
        """qwebchannel.js and the bootstrap above, for every page of a profile."""
        source = QFile(":/qtwebchannel/qwebchannel.js")
        if not source.open(QIODevice.OpenModeFlag.ReadOnly):
            logger.warning("qwebchannel.js not found; windows cannot leave the app")
            return None
        script = QWebEngineScript()
        script.setName("corvus-native")
        script.setSourceCode(bytes(source.readAll()).decode("utf-8")
                             + native_support_js(support) + NATIVE_BOOTSTRAP_JS)
        script.setInjectionPoint(QWebEngineScript.InjectionPoint.DocumentCreation)
        script.setWorldId(QWebEngineScript.ScriptWorldId.MainWorld)
        script.setRunsOnSubFrames(False)
        return script

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
        web.setPage(MainPage(profile, web))
    except Exception:  # noqa: BLE001 - a window on the default profile beats no window
        logger.exception("could not set up a private web profile at %s; "
                         "falling back to the shared default", profile_dir)
        web = QWebEngineView()
        try:
            web.setPage(MainPage(QWebEngineProfile.defaultProfile(), web))
        except Exception:  # noqa: BLE001 - a window without pop-outs beats no window
            logger.exception("pop-out windows unavailable")
    configure_view(web)
    try:
        script = native_script()
        if script is not None:
            web.page().profile().scripts().insert(script)
            main_channel = QWebChannel(web.page())
            main_channel.registerObject("corvusNative", MainBridge(web))
            web.page().setWebChannel(main_channel)
    except Exception:  # noqa: BLE001 - frames that stay inside the app beat no window
        logger.exception("could not connect the page to its windows")
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
    # Where the platform already shows a packaged icon (.icns, .ico), seeded
    # with the off state: the builds cut that icon from the bare normal mark,
    # so Qt is only asked for one once the config asks for a different one.
    # Everywhere else (Linux, a source checkout) nothing would draw an icon
    # unless Qt is handed one, so the first tick always sets it.
    applied_icon: list = [(False, False)] if ships_own_icon() else [None]
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

        logger.info("Shutting down, stopping all connections …")
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
    timer = QTimer()
    timer.start(500)
    timer.timeout.connect(lambda: None)
    # Same tick picks up an app-icon change written by POST /api/config. A
    # dict lookup and a comparison; the QIcon is only rebuilt when the answer
    # actually changed, which is why polling here beats plumbing an event out
    # of the HTTP thread into the Qt loop.
    timer.timeout.connect(sync_app_icon)

    window.show()
    logger.info("CORVUS GCS v%s: standalone window ready", get_version())
    ret = app.exec()
    shutdown()  # belt-and-suspenders if aboutToQuit did not fire
    return ret


if __name__ == "__main__":
    sys.exit(main())
