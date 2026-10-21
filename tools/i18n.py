#!/usr/bin/env python3
"""Check and start interface language files (src/lang/*.json).

    python3 tools/i18n.py check            every language file: valid, consistent
    python3 tools/i18n.py new es Spanish Español
                                            a file with every known text, blank
    python3 tools/i18n.py fmt              rewrite the files sorted and indented

A plugin keeps its own translations in plugins/<id>/lang/<code>.json, in the
same format (meta is optional there). check and fmt cover those files too; a
plugin file is compared with the other languages of the same plugin only.

A language file maps the English text of the interface to its translation (see
corvus/languages.py). There is no source of truth for the list of texts: it is
the union of what the files already carry, which grows as languages are
translated. `new` starts a file from that union, and `check` lists what a
language still lacks against it.

To find texts nobody has translated yet, open the app in the language, use the
pages, and call Corvus.i18n.template() in the browser console: it returns the
texts that were shown in English, each with a blank to fill in.

stdlib only.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from corvus import languages  # noqa: E402

LANG_DIR = Path(languages.bundled_languages_dir())
HOLE = re.compile(r"\{([A-Za-z_][A-Za-z0-9_]*)\}")
# The project writes no dashes in prose: not an em dash, and not an en dash or
# a hyphen standing between words as one.
DASH = re.compile("—|–| - ")


def files(directory: Path = LANG_DIR) -> list[Path]:
    return sorted(p for p in directory.glob("*.json") if languages.is_valid_code(p.stem))


def plugin_files(root: Path = ROOT / "plugins") -> dict[Path, list[Path]]:
    """Every plugin's language files, keyed by its ``lang`` folder."""
    out: dict[Path, list[Path]] = {}
    for folder in sorted(root.glob("*/lang")):
        found = sorted(p for p in folder.glob("*.json") if languages.is_valid_code(p.stem))
        if found:
            out[folder] = found
    return out


def load_raw(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def holes(text: str) -> set[str]:
    return set(HOLE.findall(text))


def problems(path: Path, union: set[str], need_meta: bool = True) -> list[str]:
    """What is wrong with one file; an empty list means it is fine."""
    out: list[str] = []
    try:
        data = load_raw(path)
    except (OSError, ValueError) as exc:
        return [f"not readable: {exc}"]
    if not isinstance(data, dict):
        return ["not a JSON object"]
    meta = data.get("meta")
    if need_meta and (not isinstance(meta, dict) or not str(meta.get("name", "")).strip()):
        out.append("meta.name is missing")
    strings = data.get("strings")
    if not isinstance(strings, dict):
        return out + ["strings is missing"]
    for key, value in strings.items():
        if not isinstance(value, str):
            out.append(f"{key!r}: the translation is not a string")
            continue
        if not value.strip():
            continue
        if holes(key) != holes(value):
            out.append(f"{key!r}: placeholders differ ({sorted(holes(key))} vs {sorted(holes(value))})")
        if DASH.search(value) and not DASH.search(key):
            out.append(f"{key!r}: the translation uses a dash")
        if "\n" in value and "\n" not in key:
            out.append(f"{key!r}: the translation has a line break the English lacks")
    if path.stem != languages.DEFAULT_CODE:
        missing = len(union - set(strings))
        if missing:
            out.append(f"note: {missing} known texts have no translation yet")
    return out


def cmd_check(args: argparse.Namespace) -> int:
    paths = files()
    if not paths:
        print("no language files found in", LANG_DIR)
        return 1
    union: set[str] = set()
    for p in paths:
        try:
            union |= set(load_raw(p).get("strings", {}))
        except (OSError, ValueError, AttributeError):
            pass
    failed = 0
    for p in paths:
        found = problems(p, union)
        errors = [m for m in found if not m.startswith("note:")]
        notes = [m for m in found if m.startswith("note:")]
        status = "FAIL" if errors else "ok"
        print(f"{p.name}: {status}" + (f" ({notes[0][6:]})" if notes else ""))
        for m in errors[:30]:
            print("   ", m)
        if len(errors) > 30:
            print(f"    ... and {len(errors) - 30} more")
        failed += bool(errors)
    for group in plugin_files().values():
        plugin_union: set[str] = set()
        for p in group:
            try:
                plugin_union |= set(load_raw(p).get("strings", {}))
            except (OSError, ValueError, AttributeError):
                pass
        for p in group:
            found = problems(p, plugin_union, need_meta=False)
            errors = [m for m in found if not m.startswith("note:")]
            notes = [m for m in found if m.startswith("note:")]
            status = "FAIL" if errors else "ok"
            print(f"{p.relative_to(ROOT)}: {status}" + (f" ({notes[0][6:]})" if notes else ""))
            for m in errors[:30]:
                print("   ", m)
            failed += bool(errors)
    return 1 if failed else 0


def write(path: Path, name: str | None, native: str | None, strings: dict[str, str],
          last: bool = False) -> None:
    body: dict = {"strings": {k: strings[k] for k in sorted(strings, key=lambda s: (s.lower(), s))}}
    if name is not None:
        meta = {"name": name, "native_name": native, **({"last": True} if last else {})}
        body = {"meta": meta, **body}
    path.write_text(json.dumps(body, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def cmd_new(args: argparse.Namespace) -> int:
    if not languages.is_valid_code(args.code):
        print(f"{args.code!r} is not a language code such as es or pt-BR")
        return 2
    target = LANG_DIR / f"{args.code}.json"
    if target.exists() and not args.force:
        print(f"{target} exists; pass --force to replace it")
        return 2
    union: set[str] = set()
    for p in files():
        try:
            union |= set(load_raw(p).get("strings", {}))
        except (OSError, ValueError, AttributeError):
            pass
    write(target, args.name, args.native, {k: "" for k in union})
    print(f"wrote {target} with {len(union)} texts to translate")
    return 0


def cmd_fmt(args: argparse.Namespace) -> int:
    for p in files():
        data = load_raw(p)
        meta = data.get("meta", {})
        write(p, str(meta.get("name", p.stem)), str(meta.get("native_name", meta.get("name", p.stem))),
              {k: v for k, v in data.get("strings", {}).items() if isinstance(v, str)},
              last=meta.get("last") is True)
        print("formatted", p.name)
    for group in plugin_files().values():
        for p in group:
            data = load_raw(p)
            meta = data.get("meta")
            write(p, str(meta.get("name", p.stem)) if isinstance(meta, dict) else None,
                  str(meta.get("native_name", meta.get("name", p.stem))) if isinstance(meta, dict) else None,
                  {k: v for k, v in data.get("strings", {}).items() if isinstance(v, str)})
            print("formatted", p.relative_to(ROOT))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("check", help="validate every language file").set_defaults(run=cmd_check)
    new = sub.add_parser("new", help="start a language file from every known text")
    new.add_argument("code")
    new.add_argument("name", help="the language in English, for example Spanish")
    new.add_argument("native", help="the language in itself, for example Español")
    new.add_argument("--force", action="store_true")
    new.set_defaults(run=cmd_new)
    sub.add_parser("fmt", help="sort and indent every file").set_defaults(run=cmd_fmt)
    args = parser.parse_args(argv)
    return args.run(args)


if __name__ == "__main__":
    raise SystemExit(main())
