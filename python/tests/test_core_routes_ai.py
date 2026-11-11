"""The AI and plugins area of the Rust core held to the Python server.

Same helper as ``test_core_routes.py`` (``tools/route_harness.py``), its own
file so the areas that port in parallel do not edit one test module. Skipped
when ``corvus-core`` is not built:

    cargo build -p corvus_core --bin corvus-core
"""
from __future__ import annotations

import json
import os
import pathlib
import sys

import pytest

pytest.importorskip("corvus_link")
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "tools"))

import route_harness as harness  # noqa: E402

pytestmark = pytest.mark.skipif(harness.core_binary() is None, reason="corvus-core is not built")

REPO = pathlib.Path(__file__).resolve().parent.parent.parent
# The two programs keep their bundled folder in different places.
PLUGIN_PATHS = ("bundled_dir",)


@pytest.fixture(scope="module")
def pair():
    # The Rust core reads the plugins' lang folders from here, as Python reads python/plugins.
    os.environ["CORVUS_PLUGINS_DIR"] = str(REPO / "src" / "plugins")
    try:
        seed = harness.standard_seed()
        seed["config"]["plugins"] = {"legacy": {"a": 1}}
        with harness.Pair(seed=seed) as p:
            yield p
    finally:
        os.environ.pop("CORVUS_PLUGINS_DIR", None)


def check(diffs: list[str]) -> None:
    assert not diffs, "\n".join(diffs)


def test_the_plugin_list_is_python_s(pair) -> None:
    check(pair.compare_get("/api/plugins", PLUGIN_PATHS))


def test_the_plugin_languages_are_merged_as_python_merges_them(pair) -> None:
    # The native plugins have since grown texts of their own, so the Rust answer
    # may carry more; every text the web plugins have is the same in both.
    for code in ("en", "de", "fr", "dl"):
        (_, py), (_, rs) = pair.both("GET", f"/api/languages/strings?code={code}")
        assert py["strings"] or code == "en", code
        assert {k: rs["strings"].get(k) for k in py["strings"]} == py["strings"], code
        assert {k: v for k, v in rs.items() if k != "strings"} == {k: v for k, v in py.items() if k != "strings"}


@pytest.mark.parametrize("body", [
    {}, {"id": 5}, {"id": ""}, {"id": "a b", "settings": {}}, {"id": "p", "settings": []},
    {"id": "p", "settings": {}, "replace": "no"},
    {"id": "legacy", "settings": {"b": 2}},
    {"id": "legacy", "settings": {"c": 3}},
    {"id": "p", "settings": {"x": 1, "t": "é", "l": [1, 2], "o": {"k": None}}},
    {"id": "p", "settings": {"y": 2}},
    {"id": "p", "settings": {"only": True}, "replace": True},
    {"id": "p", "settings": {"_options": {"m": 1}}},
    {"id": "p", "settings": {"z": 1}, "replace": True},
])
def test_a_plugin_setting_is_saved_as_python_saves_it(pair, body) -> None:
    check(pair.compare_post("/api/plugins/settings", body))
    check(pair.compare_get("/api/plugins", PLUGIN_PATHS))
    for state in (pair.py.state_dir, pair.rs.state_dir):
        assert state.exists()


def test_the_plugin_files_are_byte_for_byte_python_s(pair) -> None:
    def files(state: pathlib.Path) -> dict[str, str]:
        root = state / "plugins"
        return {str(p.relative_to(root)): p.read_text(encoding="utf-8") for p in sorted(root.rglob("config.json"))}

    assert files(pair.py.state_dir) == files(pair.rs.state_dir) != {}
    cfg = [json.loads((s / "config.json").read_text(encoding="utf-8")) for s in (pair.py.state_dir, pair.rs.state_dir)]
    assert cfg[0].get("plugins") == cfg[1].get("plugins")


def test_a_plugin_folder_serves_its_files_as_python_serves_them(pair) -> None:
    for state in (pair.py.state_dir, pair.rs.state_dir):
        d = state / "plugins" / "mine"
        (d / "sub").mkdir(parents=True, exist_ok=True)
        (d / "plugin.json").write_text('{"name": "Mine", "order": 3, "styles": ["m.css"]}', encoding="utf-8")
        (d / "mine.js").write_text("console.log(1)\n", encoding="utf-8")
        (d / "m.css").write_text("a{}\n", encoding="utf-8")
        (d / "sub" / "x.json").write_text("{}", encoding="utf-8")
        (d / "secret.py").write_text("x", encoding="utf-8")
    check(pair.compare_get("/api/plugins", PLUGIN_PATHS))
    for path in ("mine/mine.js", "mine/m.css", "mine/sub/x.json", "mine/secret.py", "mine/nope.js",
                 "mine/../mine/mine.js", "nobody/x.js", "bad%20id/x.js", "mine/sub", "mine/.."):
        py, rs = pair.both("GET", f"/api/plugins/asset/{path}")
        assert py[0] == rs[0], path
        assert py[1] == rs[1], path


# ---- settings export, import, reset and the company logo ----

PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 64


def _post_raw(url: str, data: bytes) -> tuple[int, object]:
    import urllib.error
    import urllib.request

    req = urllib.request.Request(url, data=data, method="POST",
                                 headers={"Content-Type": "application/octet-stream"})
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            status, raw = resp.status, resp.read()
    except urllib.error.HTTPError as exc:
        status, raw = exc.code, exc.read()
    try:
        return status, json.loads(raw)
    except ValueError:
        return status, raw


@pytest.fixture(scope="module")
def sp():
    seed = harness.standard_seed()
    seed["config"].update({
        "mcp": {"enabled": True},
        "checklists": {"enabled": True, "lists": [{"id": "a", "name": "Pre", "items": ["Props"]}]},
    })
    with harness.Pair(seed=seed) as p:
        for state in (p.py.state_dir, p.rs.state_dir):
            d = state / "plugins" / "mine"
            d.mkdir(parents=True)
            (d / "plugin.json").write_text('{"name": "Mine", "scripts": ["mine.js"]}', encoding="utf-8")
            (d / "mine.js").write_text("x\n", encoding="utf-8")
            (d / "config.json").write_text('{"k": 1}\n', encoding="utf-8")
        yield p


def _export_file(pair, side: str, body: dict) -> tuple[int, object, dict | None]:
    server = pair.py if side == "py" else pair.rs
    target = server.state_dir.parent / "exports"
    status, answer = harness.http("POST", server.url + "/api/settings/export",
                                  dict(body, dir=str(target), filename="out.json"))
    written = None
    if status == 200:
        written = json.loads((target / "out.json").read_text(encoding="utf-8"))
    return status, answer, written


def test_the_export_target_has_the_same_shape(sp) -> None:
    (_, py), (_, rs) = sp.both("GET", "/api/settings/export/target")
    assert set(py) == set(rs) == {"dir", "filename"}
    assert py["filename"].startswith("corvus-settings_") and rs["filename"].startswith("corvus-settings_")


@pytest.mark.parametrize("body", [
    {}, {"include_secrets": True}, {"include_secrets": "yes"}, {"browser": []}, {"sections": "map"},
    {"sections": []}, {"sections": ["nope"]}, {"sections": ["interface", "map"], "browser": {"corvus.x": "1", "y": "2", "corvus.z": 3}},
    {"sections": ["plugins"], "plugin_files": ["mine"], "plugin_settings": ["mine"]},
    {"sections": ["plugins"], "plugin_files": ["nobody"]}, {"sections": ["plugins"], "plugin_files": "mine"},
    {"sections": ["plugins"], "plugin_settings": "mine"}, {"sections": ["vehicle", "connections"], "include_secrets": True},
    {"sections": ["interface"], "plugin_files": ["mine"]},
])
def test_an_export_answers_and_writes_as_python_does(sp, body) -> None:
    (ps, pa, pw), (rs, ra, rw) = _export_file(sp, "py", body), _export_file(sp, "rs", body)
    assert ps == rs, (pa, ra)
    py_norm = json.loads(json.dumps(pa).replace(str(sp.py.state_dir.parent), "<root>"))
    rs_norm = json.loads(json.dumps(ra).replace(str(sp.rs.state_dir.parent), "<root>"))
    check(harness.diff_json(py_norm, rs_norm))
    if pw is not None:
        for w in (pw, rw):
            w.pop("exported_at")
        check(harness.diff_json(pw, rw))


def test_an_export_to_a_folder_that_cannot_be_made_is_refused_alike(sp) -> None:
    (ps, pa), (rs, ra) = sp.both("POST", "/api/settings/export", {"dir": "/proc/nope/x", "filename": "a.json"})
    assert ps == rs == 400
    assert pa["error"].split(":")[0] == ra["error"].split(":")[0]


def _bundle(sp, body: dict) -> dict:
    status, _, written = _export_file(sp, "py", body)
    assert status == 200
    return written


@pytest.mark.parametrize("name,body,take", [
    ("everything with secrets", {"include_secrets": True}, {}),
    ("without secrets over the stored ones", {}, {}),
    ("only the map", {}, {"sections": ["map"]}),
    ("plugins", {"sections": ["plugins"], "plugin_files": ["mine"], "plugin_settings": ["mine"]}, {}),
    ("plugin settings only", {"sections": ["plugins"], "plugin_settings": ["mine"]}, {"plugin_files": []}),
    ("a section the file lacks", {"sections": ["map"]}, {"sections": ["interface"]}),
    ("nothing chosen", {}, {"sections": []}),
    ("bad sections", {}, {"sections": "x"}),
    ("bad plugin list", {"sections": ["plugins"]}, {"plugin_settings": "x"}),
])
def test_an_import_is_answered_and_applied_as_python_does(sp, name, body, take) -> None:
    bundle = _bundle(sp, body)
    sp.both("POST", "/api/config", {"theme": {"name": "blue"}, "ui": {"scale": 1.5}})
    request = dict(take, bundle=bundle)
    check(sp.compare_post("/api/settings/import", request))
    check(sp.compare_get("/api/config"))
    check(sp.compare_get("/api/plugins", PLUGIN_PATHS))


@pytest.mark.parametrize("bundle", [
    None, 5, {}, {"kind": "corvus-settings"}, {"kind": "corvus-settings", "format": 99, "config": {}},
    {"kind": "corvus-settings", "format": 2, "config": {}},
    {"kind": "corvus-settings", "format": 1, "config": {"theme": {"name": "red"}}},
    {"kind": "corvus-settings", "format": 2, "sections": ["interface"], "config": {}, "logo": "!!"},
    {"kind": "corvus-settings", "format": 2, "sections": ["plugins"], "config": {},
     "plugins": {}, "plugin_files": {"x": {"plugin.json": "e30="}, "y": {"../a.js": "eA=="}}},
])
def test_a_bad_import_is_refused_with_python_s_words(sp, bundle) -> None:
    check(sp.compare_post("/api/settings/import", {"bundle": bundle}))


def test_a_logo_comes_and_goes_as_python_has_it(sp) -> None:
    for side in (sp.py, sp.rs):
        status, got = harness.http("GET", side.url + "/api/branding/logo")
        assert status == 404 and got == {"error": "no company logo"}
    answers = []
    for side in (sp.py, sp.rs):
        answers.append([
            _post_raw(side.url + "/api/branding/logo?name=" + "a%5Cb%2Fc.png", b""),
            _post_raw(side.url + "/api/branding/logo?name=x.png", b"GIF89a"),
            _post_raw(side.url + "/api/branding/logo?name=%2Fdir%2Fmy%20mark.png", PNG),
        ])
    assert answers[0][0] == answers[1][0]
    assert answers[0][1] == answers[1][1]
    check(harness.diff_json(answers[0][2][1], answers[1][2][1]))
    assert answers[0][2][0] == answers[1][2][0] == 200
    for side in (sp.py, sp.rs):
        import urllib.request
        with urllib.request.urlopen(side.url + "/api/branding/logo") as resp:
            assert (resp.status, resp.headers["Content-Type"], resp.headers["Cache-Control"], resp.read()) == (
                200, "image/png", "no-store", PNG)
    check(sp.compare_get("/api/config"))
    a = sp.both("POST", "/api/settings/export", {"sections": ["interface"], "dir": str(sp.py.state_dir.parent), "filename": "l.json"})
    assert a[0][0] == a[1][0] == 200
    check(sp.compare_post("/api/branding/logo/remove", {}))
    check(sp.compare_post("/api/branding/logo/remove", {}))
    check(sp.compare_get("/api/config"))
    for side in (sp.py, sp.rs):
        assert harness.http("GET", side.url + "/api/branding/logo")[0] == 404


def test_a_reset_puts_the_station_back_as_python_does(sp) -> None:
    check(sp.compare_post("/api/plugins/settings", {"id": "mine", "settings": {"z": 1}}))
    check(sp.compare_post("/api/settings/reset", {}))
    check(sp.compare_get("/api/config"))
    check(sp.compare_get("/api/welcome"))
    check(sp.compare_get("/api/plugins", PLUGIN_PATHS))
    for state in (sp.py.state_dir, sp.rs.state_dir):
        assert not (state / "config.json").exists()
        assert not (state / "plugins" / "mine" / "config.json").exists()
        assert (state / "plugins" / "mine" / "plugin.json").exists(), "an installed plugin stays"


# ---- the update check, from the cache (no network on either side) ----

CACHED = {"checked_at": 1.0, "latest": {"tag": "v2999.01.01", "version": "2999.01.01", "name": "Big", "published": "p",
                                        "notes": "n", "assets": [{"name": "a", "size": 1}], "url": "https://evil.example/"},
          "error": "no route"}


@pytest.fixture(scope="module")
def up():
    seed = harness.standard_seed()
    seed["config"]["updates"] = {"check": False}
    with harness.Pair(seed=seed) as p:
        for state in (p.py.state_dir, p.rs.state_dir):
            (state / "update.json").write_text(json.dumps(CACHED), encoding="utf-8")
        yield p


def test_a_switched_off_update_check_reports_the_cache_only(up) -> None:
    check(up.compare_get("/api/update"))
    check(up.compare_get("/api/update?refresh=1"))


@pytest.mark.parametrize("body", [{"version": "2999.01.01"}, {"version": "  x  "}, {}, {"version": 5}, {"version": ""}])
def test_a_skipped_release_is_remembered_as_python_remembers_it(up, body) -> None:
    check(up.compare_post("/api/update/skip", body))
    check(up.compare_get("/api/update"))


# ---- the MCP server: the settings card's routes and /mcp itself ----

def _mcp_post(server, token: str | None, message, session: str | None = None):
    import urllib.error
    import urllib.request

    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    if session:
        headers["Mcp-Session-Id"] = session
    req = urllib.request.Request(server.url + "/mcp", data=json.dumps(message).encode(), method="POST", headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            status, raw, hdrs = resp.status, resp.read(), dict(resp.headers)
    except urllib.error.HTTPError as exc:
        status, raw, hdrs = exc.code, exc.read(), dict(exc.headers)
    return status, (json.loads(raw) if raw else None), hdrs


@pytest.fixture(scope="module")
def mp():
    with harness.Pair(seed=harness.standard_seed()) as p:
        yield p


MCP_IGNORE = ("token", "url", "at")


def test_the_mcp_card_starts_as_python_has_it(mp) -> None:
    check(mp.compare_get("/api/mcp", MCP_IGNORE))
    check(mp.compare_get("/api/mcp/status", MCP_IGNORE))
    (_, py), (_, rs) = mp.both("GET", "/api/mcp")
    assert len(py["token"]) == len(rs["token"]) == 43


@pytest.mark.parametrize("body", [
    {"enabled": "yes"}, {"permissions": {"nope": True}}, {"permissions": {"arm": 1}}, {"plugins": {"bad id": True}},
    {"confirm": []}, {"enabled": True, "fleet": True}, {"permissions": {"arm": True, "mission": True}, "confirm": {"arm": False}},
    {"plugins": {"demo": False}}, {"permissions": {"arm": False}},
])
def test_a_switch_is_set_as_python_sets_it(mp, body) -> None:
    check(mp.compare_post("/api/mcp/settings", body, MCP_IGNORE))
    check(mp.compare_get("/api/mcp", MCP_IGNORE))
    for state in (mp.py.state_dir, mp.rs.state_dir):
        pass


def test_the_mcp_block_in_the_config_file_is_python_s(mp) -> None:
    texts = [(s / "config.json").read_text(encoding="utf-8") for s in (mp.py.state_dir, mp.rs.state_dir)]
    assert json.loads(texts[0])["mcp"] == json.loads(texts[1])["mcp"]
    assert texts[0] == texts[1]


@pytest.mark.parametrize("body", [{}, {"id": "x"}, {"id": "x", "allow": "yes"}, {"id": "x", "allow": True}])
def test_a_confirmation_nobody_waits_for_is_refused_alike(mp, body) -> None:
    check(mp.compare_post("/api/mcp/confirm", body, MCP_IGNORE))


@pytest.mark.parametrize("body", [{}, {"id": "bad id"}, {"id": "w1", "tools": []},
                                  {"id": "w1", "tools": [{"plugin": "demo", "name": "go", "title": "Go"}]}])
def test_a_window_is_registered_as_python_registers_it(mp, body) -> None:
    check(mp.compare_post("/api/mcp/window", body, MCP_IGNORE))
    check(mp.compare_post("/api/mcp/result", {"id": "none", "ok": True}, MCP_IGNORE))
    check(mp.compare_get("/api/mcp/status", MCP_IGNORE + ("seen_s",)))


def test_the_protocol_over_http_is_answered_as_python_answers_it(mp) -> None:
    tokens = [harness.http("GET", s.url + "/api/mcp")[1]["token"] for s in (mp.py, mp.rs)]
    for body in ({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "T"}}},):
        (ps, pb, ph), (rs, rb, rh) = (_mcp_post(s, t, body) for s, t in zip((mp.py, mp.rs), tokens))
        assert ps == rs == 200
        check(harness.diff_json(pb, rb))
        sessions = (ph["Mcp-Session-Id"], rh["Mcp-Session-Id"])
    for body in ({"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
                 {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "get_vehicle_state"}},
                 {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "arm"}},
                 {"jsonrpc": "2.0", "id": 5, "method": "nope"}, {"jsonrpc": "2.0", "method": "notifications/initialized"},
                 [{"jsonrpc": "2.0", "id": 6, "method": "ping"}], 5):
        (ps, pb, _), (rs, rb, _) = (_mcp_post(s, t, body, sid) for s, t, sid in zip((mp.py, mp.rs), tokens, sessions))
        assert ps == rs, body
        check(harness.diff_json(pb, rb, ("mission_revision", "link_status", "link_connection", "link_error", "link_auto",
                                         "link_quality", "link_suggestion", "link_bootloader", "time", "boot_ms",
                                         "px4_version", "text", "structuredContent")))


def test_the_door_stays_shut_as_python_shuts_it(mp) -> None:
    for token in (None, "wrong"):
        (ps, pb, ph), (rs, rb, rh) = (_mcp_post(s, token, {"jsonrpc": "2.0", "id": 1, "method": "ping"}) for s in (mp.py, mp.rs))
        assert (ps, pb) == (rs, rb)
        assert ph.get("WWW-Authenticate") == rh.get("WWW-Authenticate")
    mp.both("POST", "/api/mcp/settings", {"enabled": False})
    tokens = [harness.http("GET", s.url + "/api/mcp")[1]["token"] for s in (mp.py, mp.rs)]
    (ps, pb, _), (rs, rb, _) = (_mcp_post(s, t, {"jsonrpc": "2.0", "id": 1, "method": "ping"}) for s, t in zip((mp.py, mp.rs), tokens))
    assert (ps, pb) == (rs, rb) and ps == 403


def test_a_stop_takes_the_writes_away_as_python_does(mp) -> None:
    mp.both("POST", "/api/mcp/settings", {"enabled": True, "permissions": {"arm": True, "flight": True}})
    check(mp.compare_post("/api/mcp/stop", {}, MCP_IGNORE))
    check(mp.compare_get("/api/mcp", MCP_IGNORE))


def test_a_new_token_makes_the_old_one_useless_in_both(mp) -> None:
    olds = [harness.http("GET", s.url + "/api/mcp")[1]["token"] for s in (mp.py, mp.rs)]
    for s in (mp.py, mp.rs):
        status, answer = harness.http("POST", s.url + "/api/mcp/token", {})
        assert status == 200 and answer["ok"] is True and len(answer["token"]) == 43
    for s, old in zip((mp.py, mp.rs), olds):
        assert _mcp_post(s, old, {"jsonrpc": "2.0", "id": 1, "method": "ping"})[0] == 401
