#!/usr/bin/env python3
"""Corvus GCS standalone desktop app wrapper.

Launches the backend HTTP/SSE server in a background thread and opens the
UI in a PySide6 QtWebEngine window — no browser needed. Handles clean
shutdown of both the server and the web engine.

Usage:
    python3 corvus/app.py [port] [mavlink_connection]

Chromium rendering flags are set so the map draws on any GPU stack, with
software rendering as the fallback anywhere — see :func:`chromium_flags`.
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
    * ``--disable-pinch`` — a trackpad pinch or Ctrl + wheel would zoom the
      whole page like a website and push the sidebar off screen. The size of
      the interface is the scale setting; the map zooms through its own wheel
      and pinch handling, which this switch does not touch.
    * ``--disable-gpu-sandbox``, on Linux only. The GPU process sandbox is a
      real boundary, and this frontend is the thing that loads operator-
      supplied plugins out of ``~/.corvus/plugins`` — so it is not a boundary
      to give up everywhere by default. It is here because it is the flag
      that makes the GPU stack come up on the Linux systems this app is
      deployed to (container, VM, odd driver, no GBM/EGL). On macOS and
      Windows the sandbox works and there is nothing to buy by dropping it,
      so those platforms keep it.

    Any of this can be replaced outright: an operator who exports
    ``QTWEBENGINE_CHROMIUM_FLAGS`` gets exactly what they asked for and none
    of the above (see :func:`chromium_flags_value`).

    Deliberately absent: Vulkan (``--use-vulkan``,
    ``--enable-features=Vulkan``), which Linux used to be given. Chromium's
    Vulkan compositor is experimental on Linux, and with a WebGL canvas that
    redraws every frame it flickers: the 3D map on a Linux Mint laptop,
    whenever the aircraft was flying. Chromium's default GL path draws the
    same map, and SwiftShader still catches a machine where GL will not come
    up. An operator who wants Vulkan back sets ``QTWEBENGINE_CHROMIUM_FLAGS``.
    Also absent: ``--disable-software-rasterizer``, which used to sit
    two lines under ``--enable-unsafe-swiftshader`` and turn it off again —
    SwiftShader *is* the software rasterizer, so the pair asked for opposite
    things and the machine that needed the fallback most is the one that lost
    it. Also absent: ``--force-fieldtrials=GPUHardwareRendering/Default``,
    which names no real Chromium field trial and did nothing.
    """
    flags = ["--ignore-gpu-blocklist"]
    if platform.startswith("linux"):
        flags += ["--disable-gpu-sandbox"]
    flags += ["--enable-unsafe-swiftshader", "--enable-webgl", "--disable-pinch"]
    return flags


# What a packaged launcher adds for its own sandboxing (the AppImage cannot use
# Chromium's setuid sandbox). Kept apart from QTWEBENGINE_CHROMIUM_FLAGS so it
# never stands in for the operator's own choice.
LAUNCHER_FLAGS_ENV = "CORVUS_LAUNCHER_CHROMIUM_FLAGS"


def chromium_flags_value(env, platform: str = sys.platform) -> str:
    """The ``QTWEBENGINE_CHROMIUM_FLAGS`` this run starts Chromium with.

    The operator's own value if they set one, else :func:`chromium_flags`,
    then whatever the launcher put in :data:`LAUNCHER_FLAGS_ENV`. The AppImage
    launcher used to export its sandbox switches as
    ``QTWEBENGINE_CHROMIUM_FLAGS`` itself, which looked to this module like an
    operator's choice: every packaged run then lost the GPU blocklist override
    and the software GL fallback, and on a machine without a GPU driver
    MapLibre found WebGL blocklisted and drew no map.
    """
    own = env.get("QTWEBENGINE_CHROMIUM_FLAGS", "").strip() or " ".join(chromium_flags(platform))
    extra = env.get(LAUNCHER_FLAGS_ENV, "").strip()
    return f"{own} {extra}" if extra else own


os.environ["QTWEBENGINE_CHROMIUM_FLAGS"] = chromium_flags_value(os.environ)

# pymavlink loads the dialect this names, and the packaged builds ship only
# "all" (tools/bundle_trim.py). Another one left in the shell by MAVProxy
# would send pymavlink to regenerate it inside a read-only bundle.
os.environ["MAVLINK_DIALECT"] = "all"


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


def missing_display(env, platform: str = sys.platform) -> str:
    """Why no window can open here, or ``""`` when one can.

    On Linux, Qt aborts the process when it finds neither an X11 nor a Wayland
    display, after printing a line about plugins that sends the operator
    looking in the wrong place. A start over SSH or from a service is the
    usual cause, and this says so plainly. An operator who chose a platform
    (``offscreen``, ``eglfs``, ``vnc``) is trusted with it.
    """
    if not platform.startswith("linux") or env.get("QT_QPA_PLATFORM"):
        return ""
    if env.get("DISPLAY") or env.get("WAYLAND_DISPLAY"):
        return ""
    return ("no display to open a window on: neither DISPLAY nor WAYLAND_DISPLAY "
            "is set. Start CORVUS GCS from the desktop session, or run serve.py "
            "and open the interface in a browser.")


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


POPOUT_MOVE_QUIET_MS = 300


def stays_on_top(pinned: bool, app_active: bool, moving: bool = False) -> bool:
    """Whether a pop-out window is raised above every other window right now.

    The pin keeps a window above the Corvus window, not above everything: a
    camera floating over another program's windows is in the way there. So the
    window is held on top only while Corvus is the active application; with
    another program in front it is an ordinary window its windows can cover,
    and it is on top again the moment Corvus is.

    ``moving``: the window is being dragged. Some window managers (Muffin on
    Cinnamon) report the application as inactive for the length of a move; the
    window would drop behind the Corvus window and be gone until it is let go
    of. A window under the pointer is not one another program covers, so it
    stays where it is.
    """
    return bool(pinned) and (bool(app_active) or bool(moving))


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


# Qt 6.9 gave this bit a second name, ExpandedClientAreaHint, which PySide6
# exports from 6.12 on; older ones know it only as
# MaximizeUsingFullscreenGeometryHint. On macOS it runs the window's content
# under the title bar, and NoTitleBarBackgroundHint makes that bar see-through.
QT_EXPANDED_CLIENT_AREA = 0x00400000
QT_NO_TITLE_BAR_BACKGROUND = 0x00800000
QT_FRAMELESS = 0x00000800
# The width the traffic lights take at the left of a macOS title bar, gap to
# the page's bar included, in points. The same from macOS 15 to 26.
MAC_TITLE_CONTROLS_W = 72
# A macOS title bar's height, for as long as Qt has not measured it yet.
MAC_TITLE_BAR_H = 28


def system_title_bar(cfg) -> bool:
    """Whether windows of their own get the system's title bar: the default.

    Settings, "Corvus title bar", turns it off for the bar the page draws
    itself. Read from the live config object the HTTP handlers mutate in
    place, like the app icon, so the switch reaches open windows without a
    restart. Anything but a genuine ``True`` there is the system's bar.
    """
    ui = getattr(cfg, "ui", None)
    return not (isinstance(ui, dict) and ui.get("corvus_title_bar") is True)


# A window's own answer to "which title bar" (``popout_title_bar``): the
# system's, the Corvus bar, or, for anything else, whatever Settings says.
TITLE_BAR_CHOICES = ("system", "corvus")


def popout_title_bar(choice: str, system_by_default: bool) -> bool:
    """Whether one window of its own gets the system's title bar.

    *choice* is the window's own setting, given by what it shows: a plugin's
    terminal can have its own in the plugin's options (``bar`` in the
    window's address, then ``PopoutBridge.setTitleBar``). ``"system"`` and
    ``"corvus"`` decide; anything else follows Settings, *system_by_default*.
    """
    if choice == "system":
        return True
    if choice == "corvus":
        return False
    return bool(system_by_default)


def popout_bar_choice(url: str) -> str:
    """The ``bar`` a popout.html address asks for, or ``""`` for Settings'."""
    try:
        values = parse_qs(urlsplit(url).query).get("bar") or [""]
    except ValueError:
        return ""
    return values[0] if values[0] in TITLE_BAR_CHOICES else ""


def popout_frame(system: bool, platform_name: str) -> dict:
    """How a camera's or terminal's window of its own is framed.

    By default the operating system draws its own frame and its own close,
    minimize and maximize buttons, and the page keeps what the system bar has
    no place for: the name, the status, the pin and the power button. With
    Settings, "Corvus title bar", it is frameless and the page draws all of
    it (``native`` false).

    On macOS the page runs on under the system bar (``overlay``): the traffic
    lights sit at the left of the page's own bar, which keeps ``inset``
    points clear for them, and the name follows them. Elsewhere the page
    starts below the system bar, which carries the name as the window title;
    the page's bar becomes a strip under it, the status at the left and the
    buttons at the right, below the system's own (``controls``).

    ``flags`` are the Qt window flags beyond ``Qt.Window``, and
    ``translucent`` whether the window is see-through around the page: a
    frameless window for its rounded corners, a macOS one for the blur behind
    a frosted terminal. A Windows or Linux frame is never see-through, as a
    framed window with a transparent client area draws black there.
    """
    if not system:
        return {"native": False, "overlay": False, "controls": "", "inset": 0,
                "flags": QT_FRAMELESS, "translucent": True}
    if str(platform_name or "").lower() == "cocoa":
        return {"native": True, "overlay": True, "controls": "left",
                "inset": MAC_TITLE_CONTROLS_W,
                "flags": QT_EXPANDED_CLIENT_AREA | QT_NO_TITLE_BAR_BACKGROUND,
                "translucent": True}
    return {"native": True, "overlay": False, "controls": "right", "inset": 0,
            "flags": 0, "translucent": False}


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
        QEvent, QFile, QIODevice, QObject, QPoint, QRect, QSize, Qt, QTimer, QUrl, Slot,
    )
    from PySide6.QtGui import QCursor, QGuiApplication
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

    no_display = missing_display(os.environ)
    if no_display:
        logger.error("%s", no_display)
        lock.release()
        return 1

    # Set before the QApplication, because Windows binds a window to whatever
    # the AppUserModelID was when the window was created.
    set_windows_app_id(f"corvus.gcs.{get_version()}")
    use_visible_qt_plugins()
    # Before the QApplication, so the first window already carries the id a
    # Linux dock matches against the desktop entry's StartupWMClass.
    app_id = desktop_id()
    QGuiApplication.setDesktopFileName(app_id)
    # Before the backend: Qt aborts the process when it cannot load a platform
    # plugin, and an abort runs no atexit handler. Up to here nothing is open
    # that the kernel does not close itself.
    app = QApplication(qt_argv(sys.argv, app_id))
    app.setApplicationName("CORVUS GCS")
    app.setApplicationDisplayName("CORVUS GCS")
    app.setApplicationVersion(get_version())

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

    window = QMainWindow()
    window.setWindowTitle(f"CORVUS GCS v{get_version()}")
    window.resize(1440, 1024)
    window.setMinimumSize(960, 600)

    central = QWidget()
    layout = QVBoxLayout(central)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(0)

    def configure_view(view) -> None:
        # No native menu (Reload, Save page, View page source): none of it
        # belongs in a ground station. The page still gets its contextmenu
        # events, so the map's own right-click menu is unaffected.
        try:
            view.setContextMenuPolicy(Qt.ContextMenuPolicy.NoContextMenu)
        except Exception:  # noqa: BLE001 - a stray menu beats no window
            logger.exception("could not turn off the native context menu")
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
    # lets the operator put anywhere, another screen included. By default it
    # has the system's title bar and buttons, and the page's bar keeps the
    # rest (popout_frame). With Settings, "Corvus title bar", it is frameless
    # and transparent instead: the page draws the very same frame the app
    # does, bar, buttons and grip, so the frame does not change as it crosses
    # the edge. The page moves its window through PopoutBridge either way.
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
        want = stays_on_top(holder.pinned, active, holder.moving)
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

    platform_name = QGuiApplication.platformName()
    # Settings, "Corvus title bar": what the windows of their own were last
    # framed with, so the timer below only reframes them when it changes.
    frame_pref = {"system": system_title_bar(cfg)}

    class PopoutWindow(QWidget):
        """The native window a camera or terminal gets. Remembers where it was."""

        def __init__(self):
            super().__init__(window, Qt.WindowType.Window | Qt.WindowType.FramelessWindowHint)
            self.key = ""
            self.normal = None      # the geometry before a maximize
            self.pinned = False     # the pin in its bar (see apply_pin)
            self.moving = False     # moved a moment ago (see moveEvent)
            self.want = None        # where it opens, re-applied as it is shown
            self.settled = QTimer(self)
            self.settled.setSingleShot(True)
            self.settled.setInterval(POPOUT_MOVE_QUIET_MS)
            self.settled.timeout.connect(self._stopped_moving)
            self.blur = 0           # the blur behind it (MacBlur), while frosted
            self.frame = popout_frame(False, platform_name)
            self.page = None        # its PopoutPage, to tell it the frame changed
            self.bar = ""           # its own title bar choice (popout_title_bar)
            # Under a macOS title bar the page draws its own bar, so the page
            # fills the window rather than starting below the system's bar.
            self.setAttribute(Qt.WidgetAttribute.WA_ContentsMarginsRespectsSafeArea, False)

        def hide_title(self) -> None:
            """Under a macOS title bar the page's bar carries the name; the
            system's would be drawn over it. Qt shows it again whenever the
            window is shown or retitled, so this follows both."""
            if not self.frame["overlay"] or mac is None:
                return
            try:
                mac.hide_title(int(self.winId()))
            except Exception:  # noqa: BLE001 - a doubled name beats no window
                logger.debug("could not hide a window's own title", exc_info=True)

        def moveEvent(self, event):  # noqa: N802 - Qt's name
            super().moveEvent(event)
            if self.pinned:
                self.moving = True
                self.settled.start()

        def _stopped_moving(self) -> None:
            """No move for a moment: the pin goes by the app's state again."""
            self.moving = False
            apply_pin(self)

        def showEvent(self, event):  # noqa: N802 - Qt's name
            super().showEvent(event)
            self.hide_title()

        def changeEvent(self, event):  # noqa: N802 - Qt's name
            super().changeEvent(event)
            if event.type() == QEvent.Type.WindowTitleChange:
                self.hide_title()

        def unfrost(self) -> None:
            if self.blur and mac is not None:
                try:
                    mac.clear(self.blur)
                except Exception:  # noqa: BLE001 - the window goes either way
                    logger.debug("could not take a window's blur away", exc_info=True)
            self.blur = 0

        def closeEvent(self, event):  # noqa: N802 - Qt's name
            if self.key:
                popout_last[self.key] = QRect(
                    self.normal or (self.normalGeometry() if self.isMaximized() else self.geometry()))
                # The system's own close button closes the window without its
                # page saying so first (js/popout-page.js, closeWindow): the
                # app is told here instead, or it would count the window as
                # open until its heartbeat goes stale.
                try:
                    web.page().runJavaScript(
                        "window.Corvus && Corvus.popouts && Corvus.popouts.gone && "
                        f"Corvus.popouts.gone({json.dumps(self.key)});")
                except RuntimeError:     # the app's page is already gone
                    pass
            self.unfrost()
            super().closeEvent(event)

    def dress(holder, system: bool) -> None:
        """Frame a window of its own with the system's title bar, or with none.

        Changing a window's frame makes Qt build its native window anew, so
        the blur behind it goes first (the page asks for it again once it
        hears of the new frame), and a pin is carried over. A window that is
        showing is shown again where its content was.
        """
        frame = popout_frame(system, platform_name)
        if frame == holder.frame and holder.windowHandle() is not None:
            return
        shown = holder.isVisible()
        geometry = holder.geometry()
        holder.unfrost()
        holder.frame = frame
        flags = Qt.WindowType.Window | Qt.WindowType(frame["flags"])
        flags |= holder.windowFlags() & Qt.WindowType.WindowStaysOnTopHint
        holder.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground, frame["translucent"])
        holder.setWindowFlags(flags)
        if shown:
            holder.setGeometry(geometry)
            holder.show()
            holder.raise_()
        if holder.page is not None:
            holder.page.runJavaScript(
                "window.dispatchEvent(new CustomEvent('corvus:native-frame'));")

    def sync_frames() -> None:
        """Settings, "Corvus title bar", turned on or off: every window follows."""
        want = system_title_bar(getattr(server, "config", None))
        if want == frame_pref["system"]:
            return
        frame_pref["system"] = want
        for holder in list(popout_windows.values()):
            try:
                dress(holder, popout_title_bar(holder.bar, want))
            except RuntimeError:     # a window deleted a moment ago
                pass

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
        measured from the global cursor (``QCursor.pos``) rather than from the
        page's ``screenX``: Chromium derives that from the window position it
        last heard of, which trails a window that is being moved, and the
        feedback made the window judder. The page's deltas are only the
        fallback. On Windows a move follows the
        pointer in Windows' own pixels instead (:class:`Win32Windows`), and on
        Wayland the page hands the gesture to the compositor (``systemMove``,
        ``systemResize``): see :func:`window_support`.
        """

        def __init__(self, holder):
            super().__init__(holder)
            self._holder = holder
            self._start = None
            self._cursor0 = None    # global cursor when the gesture began
            self._last = None       # last position or size applied, to skip repeats
            self._grab = None       # Windows: (hwnd, dx, dy), pointer to window, in its pixels

        def _delta(self, dx: int, dy: int) -> tuple[int, int]:
            """The cursor's travel since ``dragStart``, or the page's if Qt has none."""
            if self._cursor0 is None:
                return dx, dy
            now = QCursor.pos()
            return now.x() - self._cursor0.x(), now.y() - self._cursor0.y()

        @Slot()
        def dragStart(self) -> None:
            self._start = self._holder.geometry()
            self._cursor0 = QCursor.pos()
            self._last = None
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
            dx, dy = self._delta(dx, dy)
            target = (self._start.x() + dx, self._start.y() + dy)
            if target != self._last:
                self._last = target
                self._holder.move(*target)

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
                dw, dh = self._delta(dw, dh)
                size = (max(POPOUT_MIN_W, self._start.width() + dw),
                        max(POPOUT_MIN_H, self._start.height() + dh))
                if size != self._last:
                    self._last = size
                    self._holder.resize(*size)

        @Slot(int, int, result=str)
        def dragEnd(self, x: int, y: int) -> str:
            """End a gesture let go of at (x, y) in this window's page.

            Says whether that was over the app, and where the window's top left
            is in the app's page, so it can go back in right there. The point
            is the page's own, mapped here: Chromium's screen coordinates in
            QtWebEngine leave out the window's title bar.
            """
            self._start = None
            self._cursor0 = None
            self._last = None
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

        @Slot(result=str)
        def frame(self) -> str:
            """How this window is framed (:func:`popout_frame`), for its page.

            ``top`` is the height of the system's title bar over an
            ``overlay`` page, which its own bar matches so the system's
            buttons sit in the middle of it.
            """
            frame = self._holder.frame
            top = 0
            if frame["overlay"]:
                handle = self._holder.windowHandle()
                top = handle.safeAreaMargins().top() if handle is not None else 0
                top = top or MAC_TITLE_BAR_H
            return json.dumps({"native": frame["native"], "overlay": frame["overlay"],
                               "controls": frame["controls"], "inset": frame["inset"],
                               "top": top})

        @Slot(str)
        def setTitleBar(self, choice: str) -> None:
            """This window's own title bar: "system", "corvus", or "" for Settings'."""
            holder = self._holder
            holder.bar = choice if choice in TITLE_BAR_CHOICES else ""
            dress(holder, popout_title_bar(holder.bar, frame_pref["system"]))

        @Slot(result=bool)
        def toggleMaximize(self) -> bool:
            if self._holder.frame["native"]:
                # In the system's frame the system maximizes it, and its own
                # button may undo that, so nothing is kept here to go stale.
                if self._holder.isMaximized():
                    self._holder.showNormal()
                    return False
                self._holder.showMaximized()
                return True
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
        # A window not yet shown loses the size it was given to the web view's
        # own (100 x 30), so it is given again as it is shown.
        if holder.want is not None:
            holder.setGeometry(holder.want)
            holder.want = None
        if not holder.isVisible():
            holder.show()
            holder.raise_()

    def open_popout(page_profile, bar: str = ""):
        holder = PopoutWindow()
        holder.bar = bar
        holder.setAttribute(Qt.WidgetAttribute.WA_DeleteOnClose, True)
        # Frameless, the page draws the frame, rounded corners included, and
        # around them the window has to be see-through, or the corners would
        # be square again. With the system's title bar, see popout_frame.
        dress(holder, popout_title_bar(bar, frame_pref["system"]))
        holder.setWindowTitle("CORVUS GCS")
        holder.setMinimumSize(POPOUT_MIN_W, POPOUT_MIN_H)
        holder.resize(640, 400)
        box = QVBoxLayout(holder)
        box.setContentsMargins(0, 0, 0, 0)
        box.setSpacing(0)
        view = QWebEngineView(holder)
        page = PopoutPage(page_profile, view, holder)
        holder.page = page
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
        page = open_popout(web.page().profile(), popout_bar_choice(url))
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
            where = popout_geometry(
                (rect.x(), rect.y(), rect.width(), rect.height()), screen_area_at(rect.center()))
            holder.setGeometry(*where)
            holder.want = QRect(*where)

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
            holder.want = holder.geometry()
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
    timer.timeout.connect(sync_frames)

    window.show()
    logger.info("CORVUS GCS v%s: standalone window ready", get_version())
    ret = app.exec()
    shutdown()  # belt-and-suspenders if aboutToQuit did not fire
    return ret


if __name__ == "__main__":
    sys.exit(main())
