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


COMMANDS = {"checklists": checklists}


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
