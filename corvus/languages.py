"""Translation files for the Corvus interface.

A language is one JSON file named after its code (``de.json``, ``fr.json``,
``pt-BR.json``). Two folders are scanned, in this order:

1. **bundled**: ``<repo>/src/lang``, the languages that ship with Corvus. Inside
   a packaged artifact this lives under ``src/`` and is replaced wholesale by an
   update.
2. **user**: ``~/.corvus/languages``, where an operator drops a file to add a
   language without touching the install. It survives updates. A user file with
   the code of a bundled one wins, which is how a shipped translation is
   corrected locally.

The file format is deliberately flat so that anybody can write one::

    {
      "meta": {"name": "German", "native_name": "Deutsch"},
      "strings": {
        "Settings": "Einstellungen",
        "Settings of {name}": "Einstellungen von {name}"
      }
    }

The English text of the interface is the key, so nothing in the code carries an
id and an untranslated string simply stays English. A key with ``{name}`` in it
matches any text in that place, and the same ``{name}`` in the value puts the
matched text back. English itself is built in: ``en.json`` only has to carry the
name, and a missing one changes nothing.

The scan happens on request, not at import, and never raises. A file that is
too large, is not JSON, or carries no usable strings is skipped with a warning.
A field laptop must start with a broken translation on disk, minus that
language, never not at all.

A plugin brings its own translations the way it brings its own tests:
``<plugin>/lang/<code>.json``, in the same format (``meta`` is optional there).
Those files are merged into the language of the same code when it is loaded,
from the bundled and the operator's plugin folders. An entry in the main file
wins over a plugin's entry for the same English text, so a plugin can never
change what the application itself says.

Nothing here touches the network. stdlib only.
"""
from __future__ import annotations

import json
import logging
import os
import pathlib
import re
from typing import Any

from .paths import corvus_path
from .plugin_registry import bundled_plugins_dir, user_plugins_dir

logger = logging.getLogger("corvus.languages")

DEFAULT_CODE = "en"

# A primary language subtag, then at most two more (region, script). The code
# names a file and ends up in a URL, so it is deliberately narrow: no
# separators, no leading dot, nothing that could walk out of the folder.
_CODE_RE = re.compile(r"^[a-z]{2,3}(?:-[A-Za-z0-9]{2,8}){0,2}$")

MAX_FILE_BYTES = 2 * 1024 * 1024
MAX_KEY_CHARS = 600
MAX_VALUE_CHARS = 1200
MAX_NAME_CHARS = 60

_README = """# Corvus GCS languages

Drop one file per language into this directory, then press Reload languages in
Settings > Appearance > Language (or restart Corvus).

    languages/
      es.json
      pt-BR.json

The file name is the language code. `meta` names the language and `strings`
maps the English text of the interface to its translation:

    {
      "meta": {"name": "Spanish", "native_name": "Espa\\u00f1ol"},
      "strings": {
        "Settings": "Ajustes",
        "Settings of {name}": "Ajustes de {name}"
      }
    }

The English text is the key, copied exactly, so an English text that is not
listed stays English. A key with `{name}` in it matches any text in that place;
the same `{name}` in the value puts the matched text back.

A file here with the code of a language that ships with Corvus replaces it.

`python3 tools/i18n.py new <code> <name> <native name>` in a Corvus checkout
writes a file with every known text in it, ready to translate.
"""


def bundled_languages_dir() -> str:
    """The language folder that ships with Corvus: ``<repo>/src/lang``.

    Resolved the way ``corvus/server.py`` resolves ``src/``: as a sibling of the
    ``corvus`` package, so it points inside the artifact when Corvus runs from
    one and at the repository when it runs from a checkout.
    """
    return str(pathlib.Path(__file__).resolve().parent.parent / "src" / "lang")


def user_languages_dir() -> str:
    """The operator's language folder: ``~/.corvus/languages``."""
    return corvus_path("languages")


def ensure_user_languages_dir() -> str:
    """Create ``~/.corvus/languages`` (with its README) and return the path.

    Idempotent, and never raises: a read-only home directory costs the folder,
    not the application. The README is only written when absent.
    """
    path = user_languages_dir()
    try:
        os.makedirs(path, exist_ok=True)
        readme = os.path.join(path, "README.md")
        if not os.path.exists(readme):
            with open(readme, "w", encoding="utf-8") as fh:
                fh.write(_README)
    except OSError as exc:
        logger.warning("could not create language folder %s: %s", path, exc)
    return path


def is_valid_code(code: Any) -> bool:
    """Whether *code* is safe to use as a file name and in a URL."""
    return isinstance(code, str) and bool(_CODE_RE.match(code))


def _text(raw: Any, limit: int) -> str:
    return raw.strip()[:limit] if isinstance(raw, str) else ""


def _clean_strings(raw: Any) -> dict[str, str]:
    """The usable entries of a ``strings`` block.

    A non-string key or value, an empty one (an untranslated entry in a
    template), and one over the length caps are dropped rather than failing the
    file: one bad line must not cost a whole language.
    """
    if not isinstance(raw, dict):
        return {}
    out: dict[str, str] = {}
    for key, value in raw.items():
        if not isinstance(key, str) or not isinstance(value, str):
            continue
        if not key.strip() or not value.strip():
            continue
        if len(key) > MAX_KEY_CHARS or len(value) > MAX_VALUE_CHARS:
            continue
        out[key] = value
    return out


def read_file(path: str | os.PathLike[str], source: str = "bundled") -> dict[str, Any] | None:
    """One language file, validated, or None when it cannot be used.

    The code is the file name; a ``code`` inside ``meta`` is not trusted, so a
    copied file cannot claim to be another language.
    """
    p = pathlib.Path(path)
    code = p.stem
    if p.suffix.lower() != ".json" or not is_valid_code(code):
        return None
    try:
        if p.stat().st_size > MAX_FILE_BYTES:
            logger.warning("language file %s is larger than %d bytes; skipped", p, MAX_FILE_BYTES)
            return None
        data = json.loads(p.read_text(encoding="utf-8"))
    except (OSError, ValueError, RecursionError) as exc:
        logger.warning("language file %s unreadable; skipped: %s", p, exc)
        return None
    if not isinstance(data, dict):
        logger.warning("language file %s is not a JSON object; skipped", p)
        return None
    meta = data.get("meta") if isinstance(data.get("meta"), dict) else {}
    name = _text(meta.get("name"), MAX_NAME_CHARS) or code
    return {
        "code": code,
        "name": name,
        "native_name": _text(meta.get("native_name"), MAX_NAME_CHARS) or name,
        "source": source,
        "last": meta.get("last") is True,
        "strings": _clean_strings(data.get("strings")),
    }


def _scan(directory: str | None, source: str) -> dict[str, dict[str, Any]]:
    found: dict[str, dict[str, Any]] = {}
    if not directory:
        return found
    try:
        names = sorted(os.listdir(directory))
    except OSError:
        return found
    for name in names:
        entry = read_file(os.path.join(directory, name), source)
        if entry is not None:
            found[entry["code"]] = entry
    return found


def plugin_language_roots() -> list[str]:
    """The plugin folders whose ``lang/`` files are merged: bundled, then user."""
    return [bundled_plugins_dir(), user_plugins_dir()]


def _plugin_strings(code: str, roots: Any) -> dict[str, str]:
    """The strings every plugin under *roots* has for *code*.

    A later root wins over an earlier one, which is how a user plugin replaces
    a bundled plugin of the same id. A missing, oversized or broken file is
    skipped: a plugin's bad translation costs that plugin its text, never the
    language.
    """
    out: dict[str, str] = {}
    for root in roots or ():
        try:
            folders = sorted(os.listdir(root))
        except OSError:
            continue
        for folder in folders:
            path = pathlib.Path(root) / folder / "lang" / f"{code}.json"
            try:
                if not path.is_file() or path.stat().st_size > MAX_FILE_BYTES:
                    continue
                data = json.loads(path.read_text(encoding="utf-8"))
            except (OSError, ValueError, RecursionError) as exc:
                logger.warning("plugin language file %s unreadable; skipped: %s", path, exc)
                continue
            if isinstance(data, dict):
                out.update(_clean_strings(data.get("strings")))
    return out


def _all(user_dir: str | None, bundled_dir: str | None) -> dict[str, dict[str, Any]]:
    found = _scan(bundled_dir, "bundled")
    found.update(_scan(user_dir, "user"))
    if DEFAULT_CODE not in found:
        found[DEFAULT_CODE] = {"code": DEFAULT_CODE, "name": "English",
                               "native_name": "English", "source": "builtin", "strings": {}}
    return found


def discover(user_dir: str | None = None, bundled_dir: str | None = None) -> list[dict[str, Any]]:
    """The installed languages, English first, then by name; a file whose
    ``meta`` has ``"last": true`` (a joke or experimental language) sorts to the end.

    Each entry carries ``code``, ``name``, ``native_name``, ``source``
    (``builtin``, ``bundled`` or ``user``) and ``count``, the number of texts
    it translates. The strings themselves are fetched per language.
    """
    found = _all(user_dir, bundled_dir)
    rows = [{**{k: v for k, v in e.items() if k not in ("strings", "last")}, "count": len(e["strings"])}
            for e in found.values()]
    rows.sort(key=lambda r: (r["code"] != DEFAULT_CODE, found[r["code"]].get("last", False),
                             r["name"].lower(), r["code"]))
    return rows


def load(code: str, user_dir: str | None = None,
         bundled_dir: str | None = None, plugin_dirs: Any = ()) -> dict[str, Any] | None:
    """The language *code* with its strings, or None when it is not installed.

    *plugin_dirs* are plugin folders (see :func:`plugin_language_roots`) whose
    ``<plugin>/lang/<code>.json`` strings are merged in under the language's
    own. Empty by default, so a caller that does not ask gets the language file
    alone.
    """
    if not is_valid_code(code):
        return None
    entry = _all(user_dir, bundled_dir).get(code)
    if entry is None:
        return None
    extra = _plugin_strings(code, plugin_dirs)
    if not extra:
        return entry
    return {**entry, "strings": {**extra, **entry["strings"]}}
