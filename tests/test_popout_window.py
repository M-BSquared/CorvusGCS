"""The rules a popped-out camera or terminal window is held to in the desktop app.

A floating frame can only move inside the app's window. Its pop-out button
calls window.open, and corvus/app.py turns that into a native window the
operator can put on any screen. These are the parts of that which do not need
Qt: what such a window may show, where it may send a link instead, and where
it may open.
"""
from __future__ import annotations

import pytest

import corvus.app as app


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


@pytest.mark.parametrize("name,expected", [
    ("xcb", {"place": True, "pin": True}),
    ("cocoa", {"place": True, "pin": True}),
    ("windows", {"place": True, "pin": True}),
    ("wayland", {"place": False, "pin": False}),
    ("wayland-egl", {"place": False, "pin": False}),
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
