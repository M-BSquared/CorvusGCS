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
