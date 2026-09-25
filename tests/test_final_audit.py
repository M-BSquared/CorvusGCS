"""Final-audit regression tests: server route completeness.

The route-registry refactor in corvus/server.py moves every API endpoint
behind a ``@route`` decorator dispatched through ``CorvusHandler._GET_ROUTES``
/ ``_POST_ROUTES``. The per-endpoint tests cover each route individually, but
nothing pins the *full* set, so a route silently dropped (or accidentally
added) during a future refactor would slip through. This file encodes the
authoritative contract: the exact GET/POST route sets the integrated change
ships with, so any deviation fails the suite.

Scope is intentionally narrow and stdlib-only. Version single-source, armed
safety, shutdown, and per-endpoint behaviour are covered by their own test
files; this one only guards the route table.
"""
from __future__ import annotations

import pytest

# corvus.server imports mavlink_bridge (pymavlink) and ssh_bridge (paramiko)
# at module load; guard the import so collection never fails in a minimal env.
pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")
from corvus.server import CorvusHandler, _TILE_PATH_RE  # noqa: E402


# The authoritative route set shipped by this integrated change. Every entry
# is a @route(...) declaration in corvus/server.py. A dropped route (refactor
# regression) OR an unregistered new route both fail the equality check, which
# is the point: a deliberate route addition must update this contract too.
_EXPECTED_GET = {
    "/api/version",
    "/api/state",
    "/api/ssh/sessions",
    "/api/ssh/connections",
    "/api/ssh/stream",
    # A terminal's output, keystrokes and size on one WebSocket, which is not
    # one of the six connections a browser allows per host.
    "/api/ssh/ws",
    "/api/plugins",
    "/api/mavlink/auto",
    "/api/mavlink/modes",
    # What the connected flight stack can do, so the UI hides a control the
    # vehicle would refuse rather than offering one that fails on press.
    "/api/mavlink/capabilities",
    "/api/mavlink/serial-ports",
    "/api/motors",
    # The pack, how the autopilot measures it, and Corvus's own estimator
    # settings — every BAT_/BATT_ parameter lives behind this one.
    "/api/battery",
    "/api/params",
    # The identity this station broadcasts for the aircraft — Basic ID,
    # Operator ID, Self ID, EU classification — plus the vehicle's own DID_/
    # COM_ARM_ODID parameters and what the live broadcast is doing.
    "/api/remoteid",
    # Polled while the page is open, so it touches no MAVLink: a parameter read
    # on a reconnecting link can sit behind the bridge's operation lock, and
    # polled ones pile up against the browser's six-connection limit.
    "/api/remoteid/status",
    "/api/rc",
    "/api/safety",
    "/api/tuning",
    "/api/params/progress",
    "/api/params/upload/result",
    "/api/params/export/target",
    "/api/params/metadata",
    "/api/params/metadata/cache",
    "/api/telemetry",
    "/api/console/stream",
    # The multiplexed stream: console, params, firmware and tiles down one
    # connection, because a browser allows six per origin and the map wants
    # that budget for tiles. The single-topic routes above and below stay as
    # the published per-topic form.
    "/api/events",
    "/api/tiles/sources",
    "/api/tiles/jobs",
    "/api/tiles/progress",
    "/api/tiles/regions",
    "/api/geocode",
    "/api/mission/plans",
    "/api/config",
    "/api/firmware/status",
    "/api/firmware/catalog",
    "/api/sik/status",
    "/api/rtk/status",
    # Camera video: the page's state, and one frame per request (a long poll,
    # so a camera never holds one of the browser's six connections for good).
    "/api/video/status",
    "/api/video/frame",
    # Whether a terminal on this computer is possible here (a launcher's local buttons).
    "/api/local/status",
    "/api/logs/status",
    "/api/logs/review",
    "/api/logs/tlog-review",
    "/api/forwarding",
    "/api/firmware/progress",
    "/api/branding/logo",
    "/api/update",
}

_EXPECTED_POST = {
    "/api/console/command",
    "/api/console/save",
    "/api/mavlink/connect",
    "/api/mavlink/disconnect",
    "/api/mavlink/arm",
    "/api/mavlink/mode",
    "/api/mavlink/takeoff",
    "/api/mavlink/land",
    "/api/mavlink/rtl",
    "/api/mavlink/reboot",
    "/api/mavlink/gotopoints",
    "/api/mavlink/sethome",
    "/api/mavlink/manual",
    "/api/mission/upload",
    "/api/mission/start",
    "/api/mission/download",
    "/api/mission/clear",
    "/api/mission/plans/save",
    "/api/mission/plans/load",
    "/api/mission/plans/remove",
    "/api/ssh/connect",
    "/api/ssh/send",
    "/api/ssh/resize",
    "/api/ssh/disconnect",
    "/api/ssh/connections",
    "/api/ssh/connections/remove",
    "/api/ssh/run",
    "/api/plugins/settings",
    "/api/plugins/folder",
    "/api/config",
    "/api/update/skip",
    "/api/update/open",
    "/api/motors/assign",
    "/api/motors/test",
    "/api/motors/test/stop",
    "/api/params/download",
    "/api/params/set",
    "/api/params/upload",
    # Read back what the operator wrote, write again what did not stick.
    "/api/params/verify",
    "/api/params/export",
    "/api/params/metadata",
    "/api/params/metadata/cache/clear",
    "/api/calibrate",
    "/api/calibrate/cancel",
    # ArduPilot waits to be told each accelerometer position; PX4 recognises
    # them itself and refuses this with an explanation. See corvus.autopilot.
    "/api/calibrate/position",
    "/api/firmware/flash",
    "/api/logs/refresh",
    "/api/logs/download",
    "/api/logs/erase",
    "/api/logs/cancel",
    "/api/logs/dir",
    "/api/forwarding",
    "/api/autotune",
    "/api/vibration/stream",
    "/api/rc/calibrate",
    "/api/rc/stream",
    "/api/tuning/stream",
    "/api/tiles/download",
    "/api/tiles/cancel",
    "/api/tiles/token",
    "/api/tiles/regions/rename",
    "/api/tiles/regions/remove",
    "/api/firmware/cancel",
    # SiK radio sessions are POST including the read: loading settings takes the
    # serial port from the bridge and puts a radio into command mode, which is a
    # side effect on the telemetry link and not a fetch.
    "/api/sik/load",
    "/api/sik/save",
    "/api/sik/reset",
    # RTK: both of these restart a correction session, which costs a running
    # survey. Never behind a method a browser may prefetch or retry.
    "/api/rtk/settings",
    "/api/rtk/restart",
    "/api/video/streams",
    "/api/video/streams/remove",
    "/api/video/settings",
    # WebRTC signalling runs through the backend so the camera password stays
    # there; the window only ever holds an opaque session token.
    "/api/video/webrtc/offer",
    "/api/video/webrtc/close",
    # Launcher buttons that run on this computer: a shell in a terminal, or a
    # program in the background. Loopback callers only.
    "/api/local/connect",
    "/api/local/run",
    # One ping from this computer, for Schwalby's companion indicator.
    "/api/local/ping",
    "/api/warnings/clear",
    # The logo UPLOAD is not here on purpose: like /api/firmware/upload it
    # carries a raw octet-stream body and is dispatched ahead of the JSON
    # route table, so it never reaches _POST_ROUTES.
    "/api/branding/logo/remove",
}


def test_get_routes_complete_and_no_extras() -> None:
    """Every expected GET route is registered and none were dropped or added."""
    actual = set(CorvusHandler._GET_ROUTES)
    missing = _EXPECTED_GET - actual
    extra = actual - _EXPECTED_GET
    assert not missing, f"GET routes lost in refactor: {sorted(missing)}"
    assert not extra, f"unexpected GET routes added: {sorted(extra)}"


def test_post_routes_complete_and_no_extras() -> None:
    """Every expected POST route is registered and none were dropped or added."""
    actual = set(CorvusHandler._POST_ROUTES)
    missing = _EXPECTED_POST - actual
    extra = actual - _EXPECTED_POST
    assert not missing, f"POST routes lost in refactor: {sorted(missing)}"
    assert not extra, f"unexpected POST routes added: {sorted(extra)}"


def test_registered_routes_resolve_to_handlers() -> None:
    """No route may point at a handler method that no longer exists."""
    for table, method in (
        (CorvusHandler._GET_ROUTES, "GET"),
        (CorvusHandler._POST_ROUTES, "POST"),
    ):
        for path, name in table.items():
            assert hasattr(CorvusHandler, name), (
                f"{method} {path} -> missing handler {name!r}"
            )


def test_post_handlers_take_the_parsed_body() -> None:
    """Every POST handler is called with the request body; it must accept one.

    ``_handle_api_post`` dispatches as ``getattr(self, name)(payload)``, so a
    handler declared ``def _api_x(self)`` raises TypeError on every request and
    returns 500 — while the two tests above still report the route as present
    and resolving, because it is. ``/api/rtk/restart`` shipped exactly that
    way; this is the check that would have caught it.

    GET handlers are the mirror image: they are called with no argument, so
    they must not *require* one.
    """
    import inspect

    for path, name in CorvusHandler._POST_ROUTES.items():
        signature = inspect.signature(getattr(CorvusHandler, name))
        required = [
            p for p in list(signature.parameters.values())[1:]
            if p.default is inspect.Parameter.empty
            and p.kind in (p.POSITIONAL_ONLY, p.POSITIONAL_OR_KEYWORD)
        ]
        assert len(required) == 1, (
            f"POST {path} -> {name}{signature} cannot be called as "
            f"{name}(payload); the dispatcher passes the parsed body"
        )

    for path, name in CorvusHandler._GET_ROUTES.items():
        signature = inspect.signature(getattr(CorvusHandler, name))
        required = [
            p for p in list(signature.parameters.values())[1:]
            if p.default is inspect.Parameter.empty
            and p.kind in (p.POSITIONAL_ONLY, p.POSITIONAL_OR_KEYWORD)
        ]
        assert not required, (
            f"GET {path} -> {name}{signature} requires an argument the "
            "dispatcher does not pass"
        )


def test_tile_path_param_regex_does_not_shadow_exact_routes() -> None:
    """The .png path-param tile route must not swallow the exact tile routes.

    /api/tiles/{sources,jobs,progress} are exact table entries; the
    path-param regex handles /api/tiles/<src>/<z>/<x>/<y>.png. The dispatcher
    only invokes the regex when the path ends in ``.png``, so an exact route
    must never match the regex (or it could be shadowed if the guard order
    ever changed).
    """
    for exact in ("/api/tiles/sources", "/api/tiles/jobs", "/api/tiles/progress"):
        assert _TILE_PATH_RE.match(exact) is None, (
            f"tile regex shadows exact route {exact!r}"
        )
    # A real tile path matches with the expected capture groups.
    m = _TILE_PATH_RE.match("/api/tiles/satellite/5/3/2.png")
    assert m is not None and m.groups() == ("satellite", "5", "3", "2")
