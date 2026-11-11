#!/usr/bin/env python3
"""Dump what the Python AI and plugins area answers into fixtures for the Rust core.

    python tools/dump_ai_cases.py checklists   -> src/core/tests/data/ai_checklists.json
    python tools/dump_ai_cases.py all --check  # fail when a file is stale

Same idea as ``dump_core_cases.py``, kept in its own file so the areas that
port in parallel do not edit one script. Run it from ``python/`` with an
interpreter that has corvus_link installed.
"""
from __future__ import annotations

import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
PYTHON_ROOT = HERE.parent
DATA = PYTHON_ROOT.parent / "src" / "core" / "tests" / "data"
sys.path.insert(0, str(PYTHON_ROOT))

STALE: list[str] = []
CHECK = False


def write(name: str, data: object) -> None:
    DATA.mkdir(parents=True, exist_ok=True)
    path = DATA / name
    text = json.dumps(data, indent=2, sort_keys=False, ensure_ascii=False) + "\n"
    if CHECK:
        if not path.is_file() or path.read_text(encoding="utf-8") != text:
            STALE.append(name)
        return
    path.write_text(text, encoding="utf-8")
    print(f"wrote {path}")


def checklists() -> None:
    from corvus import checklists as C

    long_name = "n" * 100
    long_text = "t" * 300
    inputs: list[object] = [
        None, 5, "x", [], {}, {"enabled": True}, {"enabled": "false"}, {"enabled": 1},
        {"home_window": False, "enabled": False}, {"active": "abc"}, {"active": "a b"},
        {"active": "abc\n"}, {"active": "x" * 40}, {"active": "x" * 41}, {"active": ""},
        {"lists": []}, {"lists": "no"}, {"lists": [5, None, "x"]},
        {"lists": [{}]}, {"lists": [{"id": "a"}, {"id": "a"}, {"id": "a"}]},
        {"lists": [{"id": "list2"}, {}, {}]},
        {"lists": [{"id": "bad id", "name": "  Name \n two  "}]},
        {"lists": [{"id": "ok\n", "name": long_name}]},
        {"lists": [{"name": 5, "items": "x"}]},
        {"lists": [{"items": ["a", {"text": "b", "heading": True}, {"text": "c", "heading": "yes"},
                              {"text": "  "}, {"text": long_text}, 7, None, {"heading": True},
                              "x\ty\u0000z  w", {"text": "é" * 250}]}]},
        {"lists": [{"id": f"l{i}"} for i in range(40)]},
        {"lists": [{"items": [f"i{i}" for i in range(130)]}]},
        {"enabled": True, "home_window": True, "active": "l1", "lists": [
            {"id": "l1", "name": "Preflight", "items": ["Battery", {"text": "Props", "heading": False}]}],
         "extra": 1},
        {"lists": [{"id": "d", "items": ["a b", "tab\there", "  lead", "trail  "]}]},
    ]
    cases = [{"in": value, "out": C.coerce(value)} for value in inputs]
    write("ai_checklists.json", cases)


def _materialize(root: pathlib.Path, tree: dict) -> None:
    for rel, content in tree.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, dict):
            path.write_bytes(b"x" * int(content["repeat"]))
        else:
            path.write_text(content, encoding="utf-8", newline="")


def _manifest(**kw: object) -> str:
    return json.dumps(kw)


def _plugin_tree() -> dict:
    return {
        "bundled/alpha/plugin.json": _manifest(id="alpha", name="Alpha", order=20, icon="bolt",
                                               description=" first ", version="1.0.0", author="A",
                                               scripts=["alpha.js"], styles=["alpha.css"]),
        "bundled/alpha/alpha.js": "a", "bundled/alpha/alpha.css": "a",
        "bundled/beta/plugin.json": _manifest(name="Beta", order=20),
        "bundled/beta/beta.js": "b",
        "bundled/gamma/plugin.json": "{not json",
        "bundled/gamma/gamma.js": "g",
        "bundled/delta/plugin.json": _manifest(id="bad id", scripts=["d.js"]),
        "bundled/delta/d.js": "d",
        "bundled/eps/plugin.json": _manifest(id="eps", scripts=["missing.js"]),
        "bundled/zeta/plugin.json": _manifest(
            id="zeta", name="  ", order=5.7, icon="", scripts=[
                "./a.js", "a.js", "../outside.js", "/abs.js", "sub/b.js", "c.exe", "sub\\b.js", 5, "",
                "sub/../a.js", ".dot.js", "./sub/b.js"],
            styles=["s.css", "nope.css"]),
        "bundled/zeta/a.js": "a", "bundled/zeta/s.css": "s", "bundled/zeta/sub/b.js": "b",
        "bundled/zeta/c.exe": "c", "bundled/zeta/.dot.js": "d", "bundled/outside.js": "o",
        "bundled/.hid/plugin.json": _manifest(id="hid", scripts=["h.js"]), "bundled/.hid/h.js": "h",
        "bundled/stray.txt": "not a folder",
        "bundled/eta/plugin.json": _manifest(id="eta", name="eta", order=True, scripts=["eta.js"]),
        "bundled/eta/eta.js": "e",
        "bundled/theta/plugin.json": _manifest(id="theta-id", name="Theta", order="3", scripts=["x.js"]),
        "bundled/theta/x.js": "t",
        "bundled/iota/plugin.json": "[1, 2]", "bundled/iota/iota.js": "i",
        "bundled/\u00fcber/plugin.json": _manifest(name="\u00dcber", scripts=["u.js"]),
        "bundled/\u00fcber/u.js": "u",
        "user/alpha/plugin.json": _manifest(id="alpha", name="Alpha (mine)", order=1, scripts=["mine.js"]),
        "user/alpha/mine.js": "m", "user/alpha/config.json": "{}",
        "user/Omega/plugin.json": _manifest(name="omega", order=20, scripts=["o.js"]),
        "user/Omega/o.js": "o",
        "user/B/plugin.json": _manifest(name="b", order=20, scripts=["b.js"]), "user/B/b.js": "b",
        "user/only-config/config.json": "{}",
        "user/.staged/plugin.json": _manifest(id="staged", scripts=["s.js"]), "user/.staged/s.js": "s",
    }


def plugins() -> None:
    import tempfile

    from corvus import plugin_registry as R

    tree = _plugin_tree()
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        _materialize(root, tree)
        rel = lambda d: str(pathlib.Path(d).relative_to(root))  # noqa: E731
        found = R.discover(str(root / "user"), str(root / "bundled"))
        discovered = [dict(m, dir=rel(m["dir"])) for m in found]
        public = R.public_list(str(root / "user"), str(root / "bundled"))
        dirs = {i: (rel(R.find_dir(i, str(root / "user"), str(root / "bundled"))) if R.find_dir(
            i, str(root / "user"), str(root / "bundled")) else None)
            for i in ["alpha", "zeta", "beta", "gamma", "hid", "staged", "theta-id", "theta", "../x",
                      "", "nope", "Omega", "alpha\n"]}
        alpha = root / "bundled" / "zeta"
        asset_inputs = ["a.js", "./a.js", "sub/b.js", "sub\\b.js", "../outside.js", "/abs.js", "C:/x.js",
                        "c.exe", "s.css", "sub", "", "nope.js", ".dot.js", "sub/../a.js", "a.js/", "a\u0000.js",
                        "A.JS", "x:y"]
        assets = []
        for a in asset_inputs:
            got = R.resolve_asset(str(alpha), a)
            assets.append([a, None if got is None else str(got.relative_to(alpha.resolve()))])
        ids = ["a", "a.b-c_d", "A" * 64, "A" * 65, "", ".a", "-a", "a b", "a/b", "a\n", "a\n\n", "\u00e9"]
        bundled_only = [dict(m, dir=rel(m["dir"])) for m in R.discover(str(root / "none"), str(root / "bundled"))]
    write("ai_plugins.json", {
        "tree": tree, "discover": discovered, "public": public, "find_dir": dirs,
        "assets": assets, "ids": [[i, R.is_valid_id(i)] for i in ids], "bundled_only": bundled_only,
        "readme": R._README,
    })

    bundled_real = R.public_list(str(pathlib.Path(tempfile.gettempdir()) / "corvus-no-such-user-dir"))
    write("ai_plugins_bundled.json", bundled_real)


def plugin_config() -> None:
    import tempfile

    from corvus import plugin_config as PC

    ops: list[list] = [
        ["load", "a"], ["save", "a", {"x": 1, "y": [1, 2, {"z": "\u00e9\u0001"}], "n": None, "f": 1.5, "e": {}}],
        ["load", "a"], ["update", "a", {"x": 2, "k": True}, False], ["load", "a"],
        ["update", "a", {"only": 1}, True], ["load", "a"],
        ["update", "a", {"_options": {"m": 1}, "q": 1}, False],
        ["update", "a", {"fresh": 1}, True], ["load", "a"],
        ["update", "a", {"_options": {"m": 2}}, True], ["load", "a"],
        ["update", "b", {"n": 1}, False], ["update", "c", {}, False], ["update", "c", {"k": 1}, False],
        ["save", "bad id", {"a": 1}], ["save", "..", {}], ["load", "../a"], ["update", "", {}, False],
        ["raw", "d", "{not json"], ["load", "d"], ["update", "d", {"k": 1}, False],
        ["raw", "e", "[1, 2]"], ["load", "e"],
        ["raw", "f", "{\"keep\": 1}"], ["load", "f"],
        ["mkdir", "emptyfolder"], ["mkdir", ".hidden"], ["raw", ".hidden", "{}"],
        ["load_all"],
        ["migrate", {"a": {"legacy": 1}, "g": {"legacy": 2}, "h": 5, "bad id": {"x": 1}, "d": {"l": 1}}],
        ["load", "g"], ["load", "h"], ["load_all"],
        ["migrate", None], ["migrate", {}], ["migrate", []],
    ]
    out = []
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        for op in ops:
            kind = op[0]
            result: object
            try:
                if kind == "load":
                    result = PC.load(op[1], str(root))
                elif kind == "load_all":
                    result = PC.load_all(str(root))
                elif kind == "save":
                    PC.save(op[1], op[2], str(root))
                    result = "ok"
                elif kind == "update":
                    result = PC.update(op[1], op[2], replace=op[3], user_dir=str(root))
                elif kind == "raw":
                    (root / op[1]).mkdir(parents=True, exist_ok=True)
                    (root / op[1] / "config.json").write_text(op[2], encoding="utf-8")
                    result = "ok"
                elif kind == "mkdir":
                    (root / op[1]).mkdir(parents=True, exist_ok=True)
                    result = "ok"
                else:
                    result = PC.migrate(op[1], str(root))
            except ValueError as exc:
                result = {"error": str(exc)}
            out.append({"op": op, "result": result})
        files = {}
        for path in sorted(root.rglob("config.json")):
            files[str(path.relative_to(root))] = path.read_text(encoding="utf-8")
    write("ai_plugin_config.json", {"ops": out, "files": files})


def plugin_files() -> None:
    import tempfile

    from corvus import plugin_files as PF

    paths = ["a.js", "sub/a.js", "a.JS", "config.json", "sub/config.json", "a.exe", "a", ".a.js", "sub/.a.js",
             "../a.js", "sub/../a.js", "./a.js", "/a.js", "C:/a.js", "a:b.js", "a\\b.js", "a b .js",
             "a.js ", "a.js.", "con.js", "CON", "nul.txt", "com1.json", "lpt9.md", "COM0.js", "aux.d.js",
             "a|b.js", "a?.js", "a*.js", "a\"b.js", "a<b.js", "a\tb.js", "", "x" * 241, "/".join(["d"] * 8) + "/a.js",
             "/".join(["d"] * 9) + "/a.js", "d//a.js", "d/a.js/", "img/Logo.PNG", "lang/de.json", "a.tar.gz",
             ".js", "sub/.js", "a.mjs", "a.map", "style.css", "t.csv", "n.md", "n.txt", "f.woff2", "\u00e9.js"]
    cleaned = [[p, PF.clean_path(p)] for p in paths]
    tree = {
        "p/plugin.json": "{}", "p/z.js": "z", "p/a.js": "a", "p/config.json": "{}", "p/sub/b.css": "b",
        "p/sub/deep/c.json": "c", "p/.hidden.js": "h", "p/.git/x.js": "g", "p/__pycache__/m.js": "m",
        "p/bad.exe": "b", "p/sub/.h.js": "h", "p/B.js": "B", "p/a b.js": "ab", "p/\u00e9.js": "e",
        "q/plugin.json": "{}", "q/big.txt": {"repeat": PF.MAX_PLUGIN_BYTES + 1},
        "r/plugin.json": "{}", **{f"r/f{i:03}.js": "x" for i in range(PF.MAX_FILES)},
        "s/plugin.json": "{}", "s/big.txt": {"repeat": PF.MAX_PLUGIN_BYTES},
    }
    collected = {}
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        _materialize(root, tree)
        for name in ("p", "q", "r", "s"):
            try:
                got = PF.collect(root / name)
                collected[name] = {"files": [[k, v.decode("utf-8")] for k, v in got.items()]
                                   if name == "p" else sorted(got)[:3] + [len(got)]}
            except ValueError as exc:
                collected[name] = {"error": str(exc)}
    checks = []
    bad_sets = [
        ("empty", {}), ("none", None), ("manifest only", {"plugin.json": b"{}"}),
        ("no manifest", {"a.js": b"a"}), ("unclean", {"plugin.json": b"{}", "../a.js": b"a"}),
        ("config", {"plugin.json": b"{}", "config.json": b"{}"}),
        ("backslash", {"plugin.json": b"{}", "a\\b.js": b"a"}),
        ("not bytes", {"plugin.json": "text"}),
        ("big", {"plugin.json": b"{}", "a.txt": b"x" * (PF.MAX_PLUGIN_BYTES + 1)}),
        ("just fits", {"plugin.json": b"x" * PF.MAX_PLUGIN_BYTES}),
        ("many", {"plugin.json": b"{}", **{f"f{i}.js": b"x" for i in range(PF.MAX_FILES)}}),
        ("just many", {"plugin.json": b"{}", **{f"f{i}.js": b"x" for i in range(PF.MAX_FILES - 1)}}),
    ]
    for label, files in bad_sets:
        try:
            got = PF.check(files)
            checks.append([label, {"ok": sorted(got)}])
        except ValueError as exc:
            checks.append([label, {"error": str(exc)}])
    installs = []
    with tempfile.TemporaryDirectory() as tmp:
        user = pathlib.Path(tmp) / "plugins"

        def listing() -> dict:
            return {str(p.relative_to(user)): p.read_text(encoding="utf-8")
                    for p in sorted(user.rglob("*")) if p.is_file()}

        steps = [
            ("bad id", "bad id", {"plugin.json": b"{}"}),
            ("no files", "x", {}),
            ("first", "x", {"plugin.json": b"{\"v\": 1}", "x.js": b"one", "sub/y.css": b"y"}),
            ("again", "x", {"plugin.json": b"{\"v\": 2}", "x.js": b"two"}),
        ]
        for label, pid, files in steps:
            if label == "again":
                (user / "x" / "config.json").write_text("{\"saved\": 1}\n", encoding="utf-8")
            try:
                PF.install(pid, files, str(user))
                installs.append({"step": label, "ok": True, "files": listing()})
            except ValueError as exc:
                installs.append({"step": label, "error": str(exc), "files": listing() if user.exists() else {}})
    write("ai_plugin_files.json", {"clean": cleaned, "tree": tree, "collected": collected, "checks": checks,
                                   "installs": installs, "limits": [PF.MAX_PLUGIN_BYTES, PF.MAX_FILES]})


def _releases() -> dict:
    def rel(tag, **kw):
        return dict({"tag_name": tag, "name": f"Release {tag}", "published_at": "2026-11-01T10:00:00Z",
                     "body": "  notes \u00e9 \n", "assets": [{"name": "a.zip", "size": 12}, {"name": 5, "size": "7"},
                                                          {"size": None}, "x"]}, **kw)
    return {
        "mixed": [rel("v2026.10.01"), rel("v2026.11.03"), rel("v2026.11.02", prerelease=True),
                  rel("v2026.12.01", draft=True), rel("nightly"), rel("v2026.9.99"), 5, None,
                  {"tag_name": "2026.11.03"}, rel("v2026.11.03\n")],
        "newest_listed_last": [rel("v2026.01.01"), rel("v2027.01.01"), rel("v2026.06.01")],
        "older": [rel("v2020.01.01")],
        "long_notes": [rel("v2030.01.01", body="n" * 5000)],
        "empty": [], "none_stable": [rel("v2030.01.01", prerelease=True)], "not_a_list": {"a": 1},
        "odd_fields": [{"tag_name": "vv2030.1", "name": None, "published_at": 5, "body": None, "assets": None}],
    }


def update_check() -> None:
    import http.server
    import tempfile
    import threading

    from corvus import update_check as U

    versions = ["2026.09.27", "v2026.09.27", " v1.2 ", "nightly", "", None, "1..2", "v", "1.2.3.4", "01.002",
                "2026.09", "2026.09.01", "v2026.11.03\n", "0.0.0-unknown", "1e3", 5, 2026.09]
    parse = [[v, list(U.parse_version(v)) if U.parse_version(v) is not None else None] for v in versions]
    newer = [[a, b, U.is_newer(a, b)] for a, b in [
        ("2026.09.27", "2026.09.26"), ("2026.09", "2026.09.01"), ("2026.09.01", "2026.09"), ("v2", "1.99.99"),
        ("nightly", "1"), ("1", "nightly"), ("1.0", "1.0.0"), ("1.0.0", "1.0"), ("1.0", "1.0")]]
    pages = [[t, U._release_page(t)] for t in ["v2026.09.27", "2026.09", "nightly", "", "v1.2\n", "../x"]]
    releases = _releases()
    picks = [[name, U._pick_latest(p)] for name, p in releases.items()]

    scenarios = []
    for name, payload in releases.items():
        scenarios.append({"name": name, "status": 200, "body": json.dumps(payload)})
    scenarios += [
        {"name": "not json", "status": 200, "body": "<html>"},
        {"name": "server error", "status": 500, "body": "{}"},
        {"name": "too large", "status": 200, "body": json.dumps([{"tag_name": "v1", "body": "x" * (U.MAX_PAYLOAD_BYTES + 10)}])},
    ]
    out = []
    for sc in scenarios:
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self, sc=sc):  # noqa: N802
                body = sc["body"].encode()
                self.send_response(sc["status"])
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *a):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        U.RELEASES_URL = f"http://127.0.0.1:{server.server_address[1]}/releases"
        with tempfile.TemporaryDirectory() as tmp:
            checker = U.UpdateChecker(tmp)
            first = checker.check(force=True)
            cache = json.loads(pathlib.Path(tmp, "update.json").read_text())
            second = checker.check()
            cached = checker.cached_status()
        server.shutdown()
        for d in (first, second, cached):
            d["checked_at"] = d["checked_at"] > 0
        cache["checked_at"] = cache["checked_at"] > 0
        out.append({"scenario": sc if len(sc["body"]) < 3000 else dict(sc, body=None), "first": first,
                    "cache": cache, "second": second, "cached": cached})
    # stale rules
    with tempfile.TemporaryDirectory() as tmp:
        c = U.UpdateChecker(tmp)
        stale = [[v, c._is_stale(v, 1000.0)] for v in [
            {}, {"checked_at": "x"}, {"checked_at": 2000.0, "latest": {"a": 1}}, {"checked_at": 1000.0 - U.CHECK_INTERVAL_S, "latest": {"a": 1}},
            {"checked_at": 1000.0 - U.CHECK_INTERVAL_S + 1, "latest": {"a": 1}}, {"checked_at": 1000.0 - U.RETRY_INTERVAL_S},
            {"checked_at": 1000.0 - U.RETRY_INTERVAL_S + 1}, {"checked_at": 1000.0 - U.RETRY_INTERVAL_S, "latest": {}}]]
    write("ai_update.json", clean_nan({"parse": parse, "newer": newer, "pages": pages, "picks": picks,
                                       "scenarios": out, "stale": stale, "intervals": [U.CHECK_INTERVAL_S, U.RETRY_INTERVAL_S]}))


def clean_nan(x):
    return x


COMMANDS = {"checklists": checklists, "plugins": plugins, "plugin_config": plugin_config,
            "plugin_files": plugin_files, "update": update_check}


def main(argv: list[str]) -> int:
    global CHECK
    CHECK = "--check" in argv
    names = [a for a in argv[1:] if a != "--check"] or ["all"]
    for name in names:
        if name == "all":
            for fn in COMMANDS.values():
                fn()
        elif name in COMMANDS:
            COMMANDS[name]()
        else:
            print(f"unknown command {name}; one of {', '.join(COMMANDS)} or all", file=sys.stderr)
            return 2
    if STALE:
        print("stale: " + ", ".join(STALE) + "; run tools/dump_ai_cases.py", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
