"""Tests for the GitHub-release update check (corvus/update_check.py) and its
``/api/update*`` endpoints in corvus/server.py.

Two things drive the coverage here:

* **Nothing may reach the network.** Every test stubs ``urlopen`` or the
  checker itself, and every cache lives under ``tmp_path``. A test that
  silently hit GitHub would pass on a laptop and fail in the offline CI this
  project is built for.
* **False positives are the real failure mode.** An update notice for a
  version that does not exist, or one that keeps re-appearing after the
  operator dismissed it, is worse than no notice — so the version comparison,
  the draft/prerelease filter, and the "skipped" path are pinned hard.

Handler-level helpers construct a ``CorvusHandler`` via ``object.__new__`` and
stub ``_send_json``, mirroring tests/test_server_config.py.
"""
from __future__ import annotations

import json
import urllib.error
from typing import Any

import pytest

from corvus.config import CorvusConfig, _build_config, load_config
from corvus.update_check import (
    RELEASES_PAGE_URL,
    UpdateChecker,
    _pick_latest,
    is_newer,
    parse_version,
)
from corvus.version import get_version


# ---------------------------------------------------------------------------
# Version parsing / comparison
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("text,expected", [
    ("2000.09.27", (2000, 9, 27)),
    ("v2000.09.27", (2000, 9, 27)),
    ("  v2000.10.02  ", (2000, 10, 2)),
    ("1.16", (1, 16)),
    ("", None),
    (None, None),
    ("nightly", None),
    ("v2000.09.27-rc1", None),
    ("0.0.0-unknown", None),
    ("2000.09.x", None),
])
def test_parse_version(text: Any, expected: tuple | None) -> None:
    assert parse_version(text) == expected


@pytest.mark.parametrize("candidate,current,expected", [
    ("2000.10.02", "2000.09.29", True),
    ("2001.01.01", "2000.12.31", True),
    ("2000.09.30", "2000.09.29", True),
    # Equal is not newer: a machine on the current release must stay quiet.
    ("2000.09.29", "2000.09.29", False),
    ("2000.09.27", "2000.09.29", False),
    ("1999.12.31", "2000.01.01", False),
    # Zero padding is cosmetic; the comparison is on integers.
    ("2000.9.30", "2000.09.29", True),
    # Unparsable on either side means "no update", never a guess.
    ("nightly", "2000.09.29", False),
    ("2000.10.02", "0.0.0-unknown", False),
    ("", "2000.09.29", False),
])
def test_is_newer(candidate: str, current: str, expected: bool) -> None:
    assert is_newer(candidate, current) is expected


def test_is_newer_against_the_running_version() -> None:
    """The version the app actually reports must parse, or nothing ever works."""
    assert parse_version(get_version()) is not None


# ---------------------------------------------------------------------------
# Release payload reduction
# ---------------------------------------------------------------------------

def _release(tag: str, **kw: Any) -> dict[str, Any]:
    base = {
        "tag_name": tag,
        "name": f"Corvus GCS {tag}",
        "draft": False,
        "prerelease": False,
        "published_at": "2026-10-02T09:00:00Z",
        "body": "notes",
        "assets": [],
    }
    base.update(kw)
    return base


def test_pick_latest_takes_the_newest_stable_release() -> None:
    latest = _pick_latest([
        _release("v2000.09.27"),
        _release("v2000.10.02"),
        _release("v2000.09.30"),
    ])
    assert latest is not None
    assert latest["version"] == "2000.10.02"


def test_pick_latest_ignores_the_order_github_returned() -> None:
    """The newest release wins on version, not on its position in the list."""
    latest = _pick_latest([_release("v2000.09.01"), _release("v2000.11.09")])
    assert latest["version"] == "2000.11.09"


def test_pick_latest_skips_drafts_and_prereleases() -> None:
    latest = _pick_latest([
        _release("v2001.01.01", draft=True),
        _release("v2000.12.01", prerelease=True),
        _release("v2000.10.02"),
    ])
    assert latest["version"] == "2000.10.02"


def test_pick_latest_skips_unparsable_tags() -> None:
    latest = _pick_latest([_release("nightly"), _release("v2000.10.02")])
    assert latest["version"] == "2000.10.02"


def test_pick_latest_keeps_calver_zero_padding() -> None:
    """"2000.09.27", not "2000.9.27" — the dismissal match is a string compare."""
    latest = _pick_latest([_release("v2000.09.07")])
    assert latest["version"] == "2000.09.07"


def test_pick_latest_returns_none_for_junk() -> None:
    assert _pick_latest(None) is None
    assert _pick_latest({"message": "rate limited"}) is None
    assert _pick_latest([]) is None
    assert _pick_latest(["not a dict", 7]) is None


def test_release_url_is_derived_not_taken_from_the_payload() -> None:
    """A poisoned cache/payload can never redirect the operator off GitHub."""
    latest = _pick_latest([_release(
        "v2000.10.02",
        html_url="https://evil.example/pwn",
        url="https://evil.example/pwn",
    )])
    assert latest["url"] == \
        "https://github.com/M-BSquared/CorvusGCS/releases/tag/v2000.10.02"
    assert "evil.example" not in json.dumps(latest)


def test_notes_are_truncated() -> None:
    from corvus.update_check import MAX_NOTES_CHARS
    latest = _pick_latest([_release("v2000.10.02", body="x" * (MAX_NOTES_CHARS + 500))])
    assert len(latest["notes"]) == MAX_NOTES_CHARS


def test_assets_are_coerced_not_trusted() -> None:
    latest = _pick_latest([_release("v2000.10.02", assets=[
        {"name": "app.dmg", "size": "12"},
        "not a dict",
        {"name": None, "size": None},
    ])])
    assert latest["assets"] == [
        {"name": "app.dmg", "size": 12},
        {"name": "", "size": 0},
    ]


# ---------------------------------------------------------------------------
# UpdateChecker: cache + offline behaviour
# ---------------------------------------------------------------------------

class _FakeResponse:
    def __init__(self, payload: Any) -> None:
        self._body = json.dumps(payload).encode("utf-8")

    def read(self, size: int | None = None) -> bytes:
        return self._body if size is None else self._body[:size]

    def __enter__(self) -> "_FakeResponse":
        return self

    def __exit__(self, *_args: Any) -> bool:
        return False


@pytest.fixture
def no_network(monkeypatch: pytest.MonkeyPatch):
    """Make every urlopen a controlled fake; fail loudly on an unstubbed call."""
    calls: list[str] = []

    def _install(payload: Any = None, error: Exception | None = None):
        def fake_urlopen(request, timeout=None):  # noqa: ANN001
            calls.append(getattr(request, "full_url", str(request)))
            if error is not None:
                raise error
            return _FakeResponse(payload)
        monkeypatch.setattr("corvus.update_check.urllib.request.urlopen", fake_urlopen)
        return calls

    _install(error=AssertionError("unstubbed network access"))
    _install.calls = calls  # type: ignore[attr-defined]
    return _install


def test_check_fetches_stores_and_reports(tmp_path, no_network) -> None:
    calls = no_network([_release("v2000.10.02")])
    checker = UpdateChecker(state_dir=str(tmp_path))

    status = checker.check()

    assert len(calls) == 1
    assert status["latest"] == "2000.10.02"
    assert status["error"] == ""
    assert status["current"] == get_version()
    assert (tmp_path / "update.json").is_file()


def test_check_reuses_the_cache_instead_of_refetching(tmp_path, no_network) -> None:
    calls = no_network([_release("v2000.10.02")])
    checker = UpdateChecker(state_dir=str(tmp_path))

    checker.check()
    checker.check()
    checker.check()

    assert len(calls) == 1, "a fresh cache must not be re-fetched"


def test_refresh_forces_a_fetch(tmp_path, no_network) -> None:
    calls = no_network([_release("v2000.10.02")])
    checker = UpdateChecker(state_dir=str(tmp_path))

    checker.check()
    checker.check(force=True)

    assert len(calls) == 2


def test_stale_cache_is_refetched(tmp_path, no_network) -> None:
    from corvus import update_check
    calls = no_network([_release("v2000.10.02")])
    checker = UpdateChecker(state_dir=str(tmp_path))

    checker.check()
    cached = json.loads((tmp_path / "update.json").read_text())
    cached["checked_at"] -= update_check.CHECK_INTERVAL_S + 1
    (tmp_path / "update.json").write_text(json.dumps(cached))

    checker.check()
    assert len(calls) == 2


def test_a_cache_from_the_future_is_refetched(tmp_path, no_network) -> None:
    """A clock that moved backwards must not pin the check as fresh forever."""
    import time
    calls = no_network([_release("v2000.10.02")])
    checker = UpdateChecker(state_dir=str(tmp_path))

    (tmp_path / "update.json").write_text(json.dumps({
        "checked_at": time.time() + 10_000, "latest": None, "error": "",
    }))
    checker.check()
    assert len(calls) == 1


def test_offline_check_never_raises_and_keeps_the_cached_release(tmp_path, no_network) -> None:
    """The field case: the check fails on every launch and that is not a fault."""
    no_network([_release("v2000.10.02")])
    checker = UpdateChecker(state_dir=str(tmp_path))
    first = checker.check()
    assert first["latest"] == "2000.10.02"

    no_network(error=urllib.error.URLError("no route to host"))
    status = checker.check(force=True)

    assert status["latest"] == "2000.10.02", "the last known release survives"
    assert status["error"], "and the failure is reported, not hidden"


def test_a_failed_check_backs_off_instead_of_retrying_every_call(tmp_path, no_network) -> None:
    calls = no_network(error=urllib.error.URLError("offline"))
    checker = UpdateChecker(state_dir=str(tmp_path))

    checker.check()
    checker.check()
    checker.check()

    assert len(calls) == 1, "a failed attempt is cached too, so it backs off"


def test_a_failed_check_retries_sooner_than_a_successful_one(tmp_path, no_network) -> None:
    from corvus import update_check
    calls = no_network(error=urllib.error.URLError("offline"))
    checker = UpdateChecker(state_dir=str(tmp_path))
    checker.check()

    cached = json.loads((tmp_path / "update.json").read_text())
    cached["checked_at"] -= update_check.RETRY_INTERVAL_S + 1
    (tmp_path / "update.json").write_text(json.dumps(cached))

    checker.check()
    assert len(calls) == 2


def test_an_oversized_payload_is_refused(tmp_path, monkeypatch) -> None:
    from corvus import update_check

    class _Huge:
        def read(self, size=None):  # noqa: ANN001
            return b"x" * (update_check.MAX_PAYLOAD_BYTES + 1)

        def __enter__(self): return self
        def __exit__(self, *_a): return False

    monkeypatch.setattr("corvus.update_check.urllib.request.urlopen",
                        lambda *a, **k: _Huge())
    latest, error = UpdateChecker(state_dir=str(tmp_path)).fetch()
    assert latest is None
    assert error


def test_a_corrupt_cache_file_is_survivable(tmp_path, no_network) -> None:
    (tmp_path / "update.json").write_text("{ not json")
    no_network([_release("v2000.10.02")])
    status = UpdateChecker(state_dir=str(tmp_path)).check()
    assert status["latest"] == "2000.10.02"


def test_cached_status_touches_no_network(tmp_path, no_network) -> None:
    calls = no_network(error=AssertionError("cached_status must not fetch"))
    status = UpdateChecker(state_dir=str(tmp_path)).cached_status()
    assert calls == []
    assert status["update_available"] is False
    assert status["url"] == RELEASES_PAGE_URL


# ---------------------------------------------------------------------------
# Config: the updates section
# ---------------------------------------------------------------------------

def test_config_keeps_a_valid_updates_section() -> None:
    cfg = _build_config({"updates": {"check": False, "skipped": "v2000.10.02"}})
    assert cfg.updates == {"check": False, "skipped": "2000.10.02"}


def test_config_rejects_a_non_boolean_check() -> None:
    """A hand-edited "false" must read as "not set", never as an accidental on."""
    assert _build_config({"updates": {"check": "false"}}).updates is None


def test_config_drops_a_skipped_value_that_is_not_a_version() -> None:
    """Otherwise a junk value could silence updates permanently."""
    assert _build_config({"updates": {"skipped": "everything"}}).updates is None


def test_config_updates_section_round_trips(tmp_path) -> None:
    from corvus.config import save_config
    path = str(tmp_path / "config.json")
    save_config(CorvusConfig(updates={"check": True, "skipped": "2000.10.02"}), path)
    assert load_config(path).updates == {"check": True, "skipped": "2000.10.02"}


def test_config_without_an_updates_section_still_loads(tmp_path) -> None:
    path = tmp_path / "config.json"
    path.write_text(json.dumps({"http_port": 8100}))
    cfg = load_config(str(path))
    assert cfg.updates is None
    assert cfg.http_port == 8100


# ---------------------------------------------------------------------------
# HTTP endpoints
# ---------------------------------------------------------------------------

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus.server import CorvusHandler  # noqa: E402


class _StubChecker:
    """Records how it was called; returns a fixed status."""

    def __init__(self, status: dict[str, Any] | None = None) -> None:
        self.status = status or {
            "current": "2000.09.29", "latest": "2000.10.02",
            "update_available": True, "name": "n",
            "url": "https://github.com/M-BSquared/CorvusGCS/releases/tag/v2000.10.02",
            "published": "", "notes": "", "assets": [], "checked_at": 1, "error": "",
        }
        self.check_calls: list[bool] = []
        self.cached_calls = 0

    def check(self, force: bool = False) -> dict[str, Any]:
        self.check_calls.append(force)
        return dict(self.status)

    def cached_status(self) -> dict[str, Any]:
        self.cached_calls += 1
        return dict(self.status)


def _handler(*, updates: Any = None, config: CorvusConfig | None = None,
             config_path: str | None = None, path: str = "/api/update"):
    handler = object.__new__(CorvusHandler)
    handler.updates = updates
    handler.config = config
    handler.config_path = config_path
    handler.path = path
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def test_api_update_reports_an_available_update(tmp_path) -> None:
    checker = _StubChecker()
    handler, responses = _handler(
        updates=checker, config=CorvusConfig(),
        config_path=str(tmp_path / "config.json"))

    handler._api_update()

    payload, status = responses[0]
    assert status == 200
    assert payload["update_available"] is True
    assert payload["latest"] == "2000.10.02"
    assert payload["enabled"] is True
    assert payload["skipped"] == ""
    assert checker.check_calls == [False], "no refresh unless asked"


def test_api_update_honours_the_refresh_query(tmp_path) -> None:
    checker = _StubChecker()
    handler, _ = _handler(
        updates=checker, config=CorvusConfig(),
        config_path=str(tmp_path / "config.json"), path="/api/update?refresh=1")
    handler._api_update()
    assert checker.check_calls == [True]


def test_api_update_off_never_touches_the_network(tmp_path) -> None:
    checker = _StubChecker()
    handler, responses = _handler(
        updates=checker, config=CorvusConfig(updates={"check": False}),
        config_path=str(tmp_path / "config.json"))

    handler._api_update()

    payload, _ = responses[0]
    assert payload["enabled"] is False
    assert payload["update_available"] is False, "a disabled check can raise no dialog"
    assert checker.check_calls == [], "and reaches for nothing"
    assert checker.cached_calls == 1


def test_api_update_echoes_the_skipped_version(tmp_path) -> None:
    handler, responses = _handler(
        updates=_StubChecker(),
        config=CorvusConfig(updates={"skipped": "2000.10.02"}),
        config_path=str(tmp_path / "config.json"))
    handler._api_update()
    assert responses[0][0]["skipped"] == "2000.10.02"


def test_api_update_without_a_checker_still_answers(tmp_path) -> None:
    handler, responses = _handler(
        updates=None, config=CorvusConfig(),
        config_path=str(tmp_path / "config.json"))
    handler._api_update()
    payload, status = responses[0]
    assert status == 200
    assert payload["update_available"] is False
    assert payload["error"]


def test_api_update_never_500s_on_a_broken_checker(tmp_path) -> None:
    class _Broken:
        def check(self, force: bool = False):
            raise RuntimeError("boom")

    handler, responses = _handler(
        updates=_Broken(), config=CorvusConfig(),
        config_path=str(tmp_path / "config.json"))
    handler._api_update()
    payload, status = responses[0]
    assert status == 200
    assert payload["update_available"] is False


def test_api_update_skip_persists_the_version(tmp_path) -> None:
    path = str(tmp_path / "config.json")
    handler, responses = _handler(
        updates=_StubChecker(), config=CorvusConfig(), config_path=path)

    handler._api_update_skip({"version": "2000.10.02"})

    payload, status = responses[0]
    assert status == 200 and payload["ok"] is True
    assert payload["skipped"] == "2000.10.02"
    assert load_config(path).updates == {"skipped": "2000.10.02"}


def test_api_update_skip_leaves_the_check_switch_alone(tmp_path) -> None:
    """Dismissing a version must not also turn the update check off."""
    path = str(tmp_path / "config.json")
    handler, _ = _handler(
        updates=_StubChecker(),
        config=CorvusConfig(updates={"check": True}), config_path=path)

    handler._api_update_skip({"version": "2000.10.02"})

    assert load_config(path).updates == {"check": True, "skipped": "2000.10.02"}


def test_api_update_skip_with_an_empty_version_clears_the_dismissal(tmp_path) -> None:
    path = str(tmp_path / "config.json")
    handler, responses = _handler(
        updates=_StubChecker(),
        config=CorvusConfig(updates={"skipped": "2000.10.02"}), config_path=path)

    handler._api_update_skip({"version": ""})

    assert responses[0][0]["skipped"] == ""
    assert (load_config(path).updates or {}).get("skipped") is None


def test_api_update_skip_ignores_a_junk_version(tmp_path) -> None:
    path = str(tmp_path / "config.json")
    handler, responses = _handler(
        updates=_StubChecker(), config=CorvusConfig(), config_path=path)

    handler._api_update_skip({"version": "../../etc/passwd"})

    assert responses[0][0]["skipped"] == ""


def test_api_update_open_uses_the_derived_url_not_the_payload(tmp_path, monkeypatch) -> None:
    opened: list[str] = []
    import webbrowser
    monkeypatch.setattr(webbrowser, "open", lambda url: opened.append(url) or True)

    checker = _StubChecker()
    handler, responses = _handler(
        updates=checker, config=CorvusConfig(),
        config_path=str(tmp_path / "config.json"))

    handler._api_update_open({"url": "https://evil.example/pwn"})

    assert opened == ["https://github.com/M-BSquared/CorvusGCS/releases/tag/v2000.10.02"]
    assert responses[0][0]["ok"] is True


def test_api_update_open_reports_when_no_browser_opens(tmp_path, monkeypatch) -> None:
    import webbrowser
    monkeypatch.setattr(webbrowser, "open", lambda url: False)

    handler, responses = _handler(
        updates=_StubChecker(), config=CorvusConfig(),
        config_path=str(tmp_path / "config.json"))
    handler._api_update_open({})

    payload, status = responses[0]
    assert status == 200
    assert payload["ok"] is False
    assert payload["url"], "the URL is still returned so the UI can offer a copy"


def test_api_update_open_without_a_checker_is_503(tmp_path) -> None:
    handler, responses = _handler(
        updates=None, config=CorvusConfig(),
        config_path=str(tmp_path / "config.json"))
    handler._api_update_open({})
    assert responses[0][1] == 503


def test_update_routes_are_registered() -> None:
    assert CorvusHandler._GET_ROUTES.get("/api/update") == "_api_update"
    assert CorvusHandler._POST_ROUTES.get("/api/update/skip") == "_api_update_skip"
    assert CorvusHandler._POST_ROUTES.get("/api/update/open") == "_api_update_open"


def test_no_version_literal_in_the_update_module() -> None:
    """The single-source-of-truth rule: the version comes from corvus.version.

    Checked over the AST rather than the raw text so the CalVer *examples* in
    the docstrings (``2026.09.27`` in ``parse_version``) are not mistaken for
    a second place the version lives — a literal only counts when it is a
    value the code can actually return.
    """
    import ast
    import pathlib
    import re

    path = (pathlib.Path(__file__).resolve().parent.parent
            / "corvus" / "update_check.py")
    tree = ast.parse(path.read_text())
    docstrings = {
        id(node.body[0].value)
        for node in ast.walk(tree)
        if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef,
                             ast.AsyncFunctionDef))
        and node.body and isinstance(node.body[0], ast.Expr)
        and isinstance(node.body[0].value, ast.Constant)
        and isinstance(node.body[0].value.value, str)
    }
    calver = re.compile(r"\b\d{4}\.\d{1,2}\.\d{1,3}\b")
    offenders = [
        node.value for node in ast.walk(tree)
        if isinstance(node, ast.Constant) and isinstance(node.value, str)
        and id(node) not in docstrings and calver.search(node.value)
    ]
    assert not offenders, f"version literal(s) in code: {offenders}"
    assert "get_version" in path.read_text()
