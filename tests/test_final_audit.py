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
    "/api/plugins",
    "/api/mavlink/modes",
    "/api/mavlink/serial-ports",
    "/api/motors",
    "/api/params",
    "/api/rc",
    "/api/safety",
    "/api/tuning",
    "/api/params/progress",
    "/api/params/upload/result",
    "/api/params/export/target",
    "/api/telemetry",
    "/api/console/stream",
    "/api/tiles/sources",
    "/api/tiles/jobs",
    "/api/tiles/progress",
    "/api/tiles/regions",
    "/api/config",
    "/api/firmware/status",
    "/api/firmware/catalog",
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
    "/api/mavlink/gotopoints",
    "/api/mavlink/sethome",
    "/api/mavlink/manual",
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
    "/api/params/export",
    "/api/calibrate",
    "/api/calibrate/cancel",
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
    "/api/tiles/regions/rename",
    "/api/tiles/regions/remove",
    "/api/firmware/cancel",
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
