"""The rules a popped-out camera or terminal window is held to in the desktop app.

A floating frame can only move inside the app's window. Its pop-out button
calls window.open, and corvus/app.py turns that into a native window the
operator can put on any screen. These are the parts of that which do not need
Qt: what such a window may show, where it may send a link instead, and where
it may open.
"""
from __future__ import annotations

import sys

import pytest

import corvus.app as app
import corvus.native_window as native_window


@pytest.mark.parametrize("url", [
    "about:blank",
    "http://localhost:8000/popout.html?kind=video&id=v1",
    "http://127.0.0.1:8000/popout.html",
])
def test_a_pop_out_shows_corvus_own_pages(url: str) -> None:
    assert app.popout_url_allowed(url, 8000)


@pytest.mark.parametrize("url", [
    "http://localhost:8001/popout.html",      # another Corvus, or anything on another port
    "https://localhost:8000/popout.html",
    "http://example.com/",
    "http://localhost.example.com:8000/",
    "file:///etc/passwd",
    "javascript:alert(1)",
    "http://localhost:notaport/",
])
def test_a_pop_out_shows_nothing_else(url: str) -> None:
    assert not app.popout_url_allowed(url, 8000)


def test_only_web_pages_go_to_the_system_browser() -> None:
    assert app.external_url("https://px4.io/")
    assert app.external_url("http://10.0.0.7/")
    assert not app.external_url("file:///Users/pilot/.corvus/config.json")
    assert not app.external_url("javascript:alert(1)")
    assert not app.external_url("smb://share/x")


def test_a_pop_out_opens_where_its_frame_was() -> None:
    screen = (0, 25, 1440, 875)
    assert app.popout_geometry((868, 105, 560, 349), screen) == (868, 105, 560, 349)


def test_a_pop_out_never_opens_where_it_cannot_be_grabbed() -> None:
    screen = (0, 25, 1440, 875)
    x, y, w, h = app.popout_geometry((5000, -300, 560, 349), screen)
    assert (x + w, y) == (1440, 25), "pulled back onto the screen, below the menu bar"
    assert app.popout_geometry((10, 40, 9000, 9000), screen)[2:] == (1440, 875), (
        "never bigger than the screen"
    )
    assert app.popout_geometry((10, 40, 20, 20), screen)[2:] == (
        app.POPOUT_MIN_W, app.POPOUT_MIN_H), "never too small to read"


def test_a_second_screen_to_the_left_is_a_screen_like_any_other() -> None:
    left = (-1920, 0, 1920, 1080)
    assert app.popout_geometry((-1500, 200, 640, 400), left) == (-1500, 200, 640, 400)


# ---------------------------------------------------------------------------
# The bridge between a page and its native window
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("key", ["video:v1a2b3c4", "term:ssh-launcher/b1x", "term:pilot@10.0.0.7"])
def test_frame_keys_name_a_window(key: str) -> None:
    assert app.popout_key_valid(key)


@pytest.mark.parametrize("key", ["", "video:", "map:1", "video:a b", "video:" + "x" * 200, "video:<x>", None])
def test_anything_else_names_none(key) -> None:
    assert not app.popout_key_valid(key)


def test_a_window_is_keyed_by_the_frame_its_address_names() -> None:
    url = "http://localhost:8000/popout.html?id=v1&key=video%3Av1&kind=video"
    assert app.popout_key(url) == "video:v1"
    assert app.popout_key("http://localhost:8000/popout.html?key=evil%3Ax") == ""
    assert app.popout_key("http://localhost:8000/") == ""


def test_the_bootstrap_never_touches_the_document() -> None:
    """It runs at document creation, before there is an <html> element.

    It did touch it once (``document.documentElement.classList``), threw, and
    never connected the bridge, so no frame could leave the app: the kind of
    failure that looks like the feature silently not existing.
    """
    js = app.NATIVE_BOOTSTRAP_JS
    assert "document." not in js
    assert "qt.webChannelTransport" in js and "corvus:native-ready" in js


def test_a_pinned_window_is_above_corvus_and_not_above_other_programs() -> None:
    """The pin keeps a camera over the Corvus window, and only while Corvus is
    in front: with another program active it is an ordinary window again."""
    assert app.stays_on_top(pinned=True, app_active=True)
    assert not app.stays_on_top(pinned=True, app_active=False)
    assert not app.stays_on_top(pinned=False, app_active=True)


def test_a_pinned_window_stays_on_top_while_it_is_being_moved() -> None:
    """Muffin (Cinnamon) reports the app inactive for the length of a move; the
    pin must not drop the window behind Corvus for that, or it is invisible
    until it is let go of."""
    assert app.stays_on_top(pinned=True, app_active=False, moving=True)
    assert not app.stays_on_top(pinned=True, app_active=False, moving=False)
    assert not app.stays_on_top(pinned=False, app_active=False, moving=True)


# ---------------------------------------------------------------------------
# The platforms: what a window out of the app can do, and how it is moved
# ---------------------------------------------------------------------------

WAYLAND_DESKTOP = {"WAYLAND_DISPLAY": "wayland-0", "DISPLAY": ":0", "XDG_SESSION_TYPE": "wayland"}


def test_a_wayland_desktop_with_xwayland_runs_the_app_on_x11_first() -> None:
    assert app.qpa_platform(WAYLAND_DESKTOP, "linux") == "xcb;wayland"
    assert app.qpa_platform({"XDG_SESSION_TYPE": "wayland", "DISPLAY": ":1"}, "linux") == "xcb;wayland"


@pytest.mark.parametrize("env,platform", [
    (dict(WAYLAND_DESKTOP, QT_QPA_PLATFORM="wayland"), "linux"),   # the operator's choice wins
    ({"WAYLAND_DISPLAY": "wayland-0"}, "linux"),                    # no XWayland to go to
    ({"DISPLAY": ":0", "XDG_SESSION_TYPE": "x11"}, "linux"),        # already X11
    (WAYLAND_DESKTOP, "darwin"),
    (WAYLAND_DESKTOP, "win32"),
])
def test_everywhere_else_qt_chooses(env, platform) -> None:
    assert app.qpa_platform(env, platform) == ""


def test_a_linux_start_without_any_display_says_so() -> None:
    assert "DISPLAY" in app.missing_display({}, "linux")


@pytest.mark.parametrize("env,platform", [
    ({"DISPLAY": ":0"}, "linux"),
    ({"WAYLAND_DISPLAY": "wayland-0"}, "linux"),
    ({"QT_QPA_PLATFORM": "offscreen"}, "linux"),    # the operator's choice
    ({}, "darwin"),
    ({}, "win32"),
])
def test_a_window_can_open(env, platform) -> None:
    assert app.missing_display(env, platform) == ""


def test_qt_comes_up_before_the_backend_opens_anything() -> None:
    import inspect

    source = inspect.getsource(app.main)
    assert source.index("QApplication(qt_argv(") < source.index("start_backend(port")


@pytest.mark.parametrize("name,expected", [
    ("xcb", {"place": True, "pin": True, "frost": False}),
    ("cocoa", {"place": True, "pin": True, "frost": True}),
    ("windows", {"place": True, "pin": True, "frost": False}),
    ("wayland", {"place": False, "pin": False, "frost": False}),
    ("wayland-egl", {"place": False, "pin": False, "frost": False}),
])
def test_what_a_window_can_do_follows_the_platform(name, expected) -> None:
    assert app.window_support(name) == expected


def test_the_pages_learn_it_before_their_own_scripts_run() -> None:
    line = app.native_support_js({"place": False, "pin": False})
    assert 'window.corvusNativeSupport = Object.freeze({"place": false, "pin": false});' in line
    assert "document" not in line


def test_the_win32_calls_exist_only_on_windows() -> None:
    assert app.win32_windows("darwin") is None
    assert app.win32_windows("linux") is None


def test_the_desktop_blur_exists_only_on_macos() -> None:
    """A frosted terminal in a window of its own needs the operating system
    to blur what is behind it. Elsewhere there is nothing to load, and the
    window stays solid."""
    assert app.mac_blur("linux") is None
    assert app.mac_blur("win32") is None


@pytest.mark.skipif(sys.platform != "darwin", reason="AppKit is macOS only")
def test_the_desktop_blur_loads_on_macos() -> None:
    blur = app.mac_blur("darwin")
    assert isinstance(blur, native_window.MacBlur)
    blur.clear(0)   # nothing to take away is not an error


def test_by_default_a_window_of_its_own_is_frameless_and_the_page_draws_it() -> None:
    frame = app.popout_frame(False, "cocoa")
    assert frame["native"] is False
    assert frame["flags"] == app.QT_FRAMELESS
    assert frame["translucent"] is True, "see-through around the page's rounded corners"


def test_on_macos_the_page_runs_on_under_the_systems_title_bar() -> None:
    """The traffic lights sit at the left of the Corvus bar, which keeps the
    name, the status, the pin and the power button."""
    frame = app.popout_frame(True, "cocoa")
    assert frame["native"] and frame["overlay"]
    assert frame["controls"] == "left"
    assert frame["inset"] == app.MAC_TITLE_CONTROLS_W
    assert frame["flags"] == app.QT_EXPANDED_CLIENT_AREA | app.QT_NO_TITLE_BAR_BACKGROUND
    assert frame["translucent"] is True, "the blur behind a frosted terminal shows through"


@pytest.mark.parametrize("name", ["windows", "xcb", "wayland"])
def test_elsewhere_the_page_starts_below_the_systems_title_bar(name) -> None:
    frame = app.popout_frame(True, name)
    assert frame["native"] and not frame["overlay"]
    assert frame["controls"] == "right"
    assert frame["flags"] == 0, "an ordinary framed window"
    assert frame["translucent"] is False, "a framed window draws black where it is see-through"


@pytest.mark.skipif(sys.platform != "darwin", reason="Qt's names are checked where Qt runs")
def test_the_expanded_client_area_is_the_bit_qt_knows() -> None:
    from PySide6.QtCore import Qt
    assert app.QT_FRAMELESS == Qt.WindowType.FramelessWindowHint.value
    assert app.QT_NO_TITLE_BAR_BACKGROUND == Qt.WindowType.NoTitleBarBackgroundHint.value
    expanded = getattr(Qt.WindowType, "ExpandedClientAreaHint",
                       Qt.WindowType.MaximizeUsingFullscreenGeometryHint)
    assert app.QT_EXPANDED_CLIENT_AREA == expanded.value


def test_the_system_title_bar_is_the_default_and_the_corvus_bar_is_asked_for() -> None:
    class Cfg:
        ui = None
    cfg = Cfg()
    assert app.system_title_bar(cfg) is True
    assert app.system_title_bar(None) is True
    cfg.ui = {"corvus_title_bar": "true"}
    assert app.system_title_bar(cfg) is True, "a string is not a yes"
    cfg.ui = {"corvus_title_bar": False}
    assert app.system_title_bar(cfg) is True
    cfg.ui = {"corvus_title_bar": True}
    assert app.system_title_bar(cfg) is False


@pytest.mark.parametrize("choice,default,expected", [
    ("system", False, True),
    ("corvus", True, False),
    ("", True, True),
    ("", False, False),
    ("anything", False, False),
])
def test_a_window_may_choose_its_own_title_bar(choice, default, expected) -> None:
    """A plugin's terminal can have its own (the plugin's option); otherwise
    Settings decides."""
    assert app.popout_title_bar(choice, default) is expected


def test_the_choice_comes_with_the_windows_address() -> None:
    base = "http://localhost:8000/popout.html?key=term%3Anuttx-console&kind=plugin%3Anuttx-console"
    assert app.popout_bar_choice(base + "&bar=system") == "system"
    assert app.popout_bar_choice(base + "&bar=corvus") == "corvus"
    assert app.popout_bar_choice(base + "&bar=bogus") == ""
    assert app.popout_bar_choice(base) == ""
