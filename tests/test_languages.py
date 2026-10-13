"""Interface languages: the files, their discovery, the endpoints and the config key.

Covers ``corvus/languages.py`` (what a language file may hold and which folder
wins), ``GET /api/languages`` and ``GET /api/languages/strings``, the
``language`` config key, and the language files that ship in ``src/lang``.
"""
from __future__ import annotations

import importlib.util
import json
from pathlib import Path
from typing import Any

import pytest

from corvus import languages, settings_bundle
from corvus.config import CorvusConfig, _build_config, _config_to_dict
from corvus.server import CorvusHandler

ROOT = Path(__file__).resolve().parent.parent


def _write(folder: Path, stem: str, name: str = "", strings: Any = None, **meta: Any) -> Path:
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / f"{stem}.json"
    body: dict[str, Any] = {"meta": {"name": name or stem, **meta}}
    if strings is not None:
        body["strings"] = strings
    path.write_text(json.dumps(body), encoding="utf-8")
    return path


def _handler(tmp_path, path: str = "/api/languages"):
    handler = object.__new__(CorvusHandler)
    handler.mavlink = None
    handler.store = None
    handler.ssh = None
    handler.config = CorvusConfig()
    handler.config_path = str(tmp_path / "config.json")
    handler.language_user_dir = str(tmp_path / "user")
    handler.language_bundled_dir = str(tmp_path / "bundled")
    handler.path = path
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


# ---------------------------------------------------------------------------
# Codes
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("code", ["en", "de", "fr", "pt-BR", "zh-Hant", "sr-Latn-RS", "fil"])
def test_valid_codes(code):
    assert languages.is_valid_code(code)


@pytest.mark.parametrize("code", ["", "E", "english", "de_DE", "../de", "de/", ".de", "DE", "de-", "de-x", None, 5])
def test_invalid_codes(code):
    assert not languages.is_valid_code(code)


# ---------------------------------------------------------------------------
# A file
# ---------------------------------------------------------------------------

def test_a_file_is_read(tmp_path):
    path = _write(tmp_path, "de", "German", {"Settings": "Einstellungen"}, native_name="Deutsch")
    entry = languages.read_file(path)
    assert entry is not None
    assert entry["code"] == "de"
    assert entry["name"] == "German"
    assert entry["native_name"] == "Deutsch"
    assert entry["strings"] == {"Settings": "Einstellungen"}


def test_the_code_is_the_file_name_not_what_the_file_claims(tmp_path):
    path = _write(tmp_path, "de", "German", {}, code="fr")
    assert languages.read_file(path)["code"] == "de"


def test_the_native_name_defaults_to_the_name(tmp_path):
    entry = languages.read_file(_write(tmp_path, "es", "Spanish", {"A": "B"}))
    assert entry["native_name"] == "Spanish"


def test_a_missing_name_falls_back_to_the_code(tmp_path):
    path = tmp_path / "es.json"
    path.write_text(json.dumps({"strings": {"A": "B"}}), encoding="utf-8")
    assert languages.read_file(path)["name"] == "es"


def test_unusable_entries_are_dropped_but_the_file_stays(tmp_path):
    path = _write(tmp_path, "de", "German", {
        "Good": "Gut",
        "Blank": "",
        "Spaces": "   ",
        "Number": 5,
        "Null": None,
        "x" * (languages.MAX_KEY_CHARS + 1): "too long a key",
        "Long": "y" * (languages.MAX_VALUE_CHARS + 1),
    })
    assert languages.read_file(path)["strings"] == {"Good": "Gut"}


@pytest.mark.parametrize("body", ["not json", "[1, 2]", "42", "null", ""])
def test_a_broken_file_is_skipped_not_fatal(tmp_path, body):
    path = tmp_path / "de.json"
    path.write_text(body, encoding="utf-8")
    assert languages.read_file(path) is None


def test_a_file_with_strings_that_are_not_an_object_has_none(tmp_path):
    path = _write(tmp_path, "de", "German", ["a", "b"])
    assert languages.read_file(path)["strings"] == {}


def test_an_oversized_file_is_skipped(tmp_path, monkeypatch):
    monkeypatch.setattr(languages, "MAX_FILE_BYTES", 40)
    path = _write(tmp_path, "de", "German", {"A long enough key": "A long enough value"})
    assert languages.read_file(path) is None


@pytest.mark.parametrize("name", ["_template.json", "notes.txt", "README.md", "DE.json", "de_AT.json"])
def test_files_that_are_not_language_files_are_ignored(tmp_path, name):
    (tmp_path / name).write_text(json.dumps({"meta": {"name": "x"}, "strings": {"A": "B"}}), encoding="utf-8")
    assert languages.read_file(tmp_path / name) is None


# ---------------------------------------------------------------------------
# Discovery
# ---------------------------------------------------------------------------

def test_english_is_there_with_no_folders_at_all(tmp_path):
    rows = languages.discover(str(tmp_path / "nope"), str(tmp_path / "nada"))
    assert [r["code"] for r in rows] == ["en"]
    assert rows[0]["native_name"] == "English"
    assert rows[0]["count"] == 0


def test_languages_are_listed_english_first_then_by_name(tmp_path):
    _write(tmp_path / "bundled", "fr", "French", {"A": "B"})
    _write(tmp_path / "bundled", "de", "German", {"A": "B", "C": "D"})
    _write(tmp_path / "bundled", "en", "English")
    rows = languages.discover(None, str(tmp_path / "bundled"))
    assert [r["code"] for r in rows] == ["en", "fr", "de"]
    assert {r["code"]: r["count"] for r in rows} == {"en": 0, "fr": 1, "de": 2}
    assert all("strings" not in r for r in rows)


def test_a_user_file_adds_a_language(tmp_path):
    _write(tmp_path / "bundled", "de", "German", {"A": "B"})
    _write(tmp_path / "user", "es", "Spanish", {"A": "B"}, native_name="Español")
    rows = {r["code"]: r for r in languages.discover(str(tmp_path / "user"), str(tmp_path / "bundled"))}
    assert set(rows) == {"en", "de", "es"}
    assert rows["es"]["source"] == "user"
    assert rows["de"]["source"] == "bundled"


def test_a_user_file_replaces_the_shipped_one_with_its_code(tmp_path):
    _write(tmp_path / "bundled", "de", "German", {"Settings": "Einstellungen"})
    _write(tmp_path / "user", "de", "German", {"Settings": "Optionen"})
    entry = languages.load("de", str(tmp_path / "user"), str(tmp_path / "bundled"))
    assert entry["strings"] == {"Settings": "Optionen"}
    assert entry["source"] == "user"


def test_a_broken_user_file_does_not_take_the_shipped_language_with_it(tmp_path):
    _write(tmp_path / "bundled", "de", "German", {"Settings": "Einstellungen"})
    (tmp_path / "user").mkdir()
    (tmp_path / "user" / "de.json").write_text("{broken", encoding="utf-8")
    entry = languages.load("de", str(tmp_path / "user"), str(tmp_path / "bundled"))
    assert entry["strings"] == {"Settings": "Einstellungen"}


def test_load_answers_none_for_an_unknown_or_unsafe_code(tmp_path):
    assert languages.load("xx", str(tmp_path), str(tmp_path)) is None
    assert languages.load("../etc", str(tmp_path), str(tmp_path)) is None


def test_english_loads_with_no_strings_even_without_a_file(tmp_path):
    entry = languages.load("en", str(tmp_path), str(tmp_path))
    assert entry["strings"] == {}


def test_the_user_folder_is_created_with_a_readme(tmp_path, monkeypatch):
    monkeypatch.setattr(languages, "user_languages_dir", lambda: str(tmp_path / "languages"))
    assert languages.ensure_user_languages_dir() == str(tmp_path / "languages")
    readme = (tmp_path / "languages" / "README.md").read_text(encoding="utf-8")
    assert "strings" in readme
    # A second call keeps what is there.
    (tmp_path / "languages" / "README.md").write_text("mine", encoding="utf-8")
    languages.ensure_user_languages_dir()
    assert (tmp_path / "languages" / "README.md").read_text(encoding="utf-8") == "mine"


# ---------------------------------------------------------------------------
# Endpoints
# ---------------------------------------------------------------------------

def test_the_list_endpoint(tmp_path):
    _write(tmp_path / "bundled", "de", "German", {"A": "B"}, native_name="Deutsch")
    handler, responses = _handler(tmp_path)
    handler._api_languages()
    body, status = responses[0]
    assert status == 200
    assert [r["code"] for r in body["languages"]] == ["en", "de"]
    assert body["default"] == "en"
    assert body["user_dir"] == str(tmp_path / "user")


def test_the_strings_endpoint(tmp_path):
    _write(tmp_path / "bundled", "de", "German", {"Settings": "Einstellungen"}, native_name="Deutsch")
    handler, responses = _handler(tmp_path, "/api/languages/strings?code=de")
    handler._api_language_strings()
    body, status = responses[0]
    assert status == 200
    assert body["code"] == "de"
    assert body["native_name"] == "Deutsch"
    assert body["strings"] == {"Settings": "Einstellungen"}


def test_the_strings_endpoint_answers_english_with_nothing(tmp_path):
    handler, responses = _handler(tmp_path, "/api/languages/strings?code=en")
    handler._api_language_strings()
    body, status = responses[0]
    assert status == 200
    assert body["strings"] == {}


@pytest.mark.parametrize("query", ["", "?code=", "?code=../de", "?code=DE", "?code=de%2F"])
def test_the_strings_endpoint_refuses_a_bad_code(tmp_path, query):
    handler, responses = _handler(tmp_path, "/api/languages/strings" + query)
    handler._api_language_strings()
    assert responses[0][1] == 400


def test_the_strings_endpoint_404s_a_language_that_is_not_installed(tmp_path):
    handler, responses = _handler(tmp_path, "/api/languages/strings?code=xx")
    handler._api_language_strings()
    assert responses[0][1] == 404


def test_the_routes_are_registered():
    assert CorvusHandler._GET_ROUTES["/api/languages"] == "_api_languages"
    assert CorvusHandler._GET_ROUTES["/api/languages/strings"] == "_api_language_strings"


# ---------------------------------------------------------------------------
# The config key
# ---------------------------------------------------------------------------

def test_the_language_survives_a_config_round_trip():
    cfg = _build_config({"language": "de"})
    assert cfg.language == "de"
    assert _config_to_dict(cfg)["language"] == "de"
    assert _build_config(_config_to_dict(cfg)).language == "de"


def test_no_language_is_left_out_of_the_file():
    assert "language" not in _config_to_dict(_build_config({}))


@pytest.mark.parametrize("raw", ["", "DE", "de_DE", "../de", 5, None, ["de"], {"name": "de"}])
def test_a_bad_language_in_the_file_is_dropped(raw):
    assert _build_config({"language": raw}).language is None


def test_the_language_is_part_of_the_interface_settings_section():
    assert settings_bundle.section_of("language") == "interface"


def test_post_config_sets_and_clears_the_language(tmp_path):
    handler, _ = _handler(tmp_path)
    public, error = handler._apply_config_partial({"language": "fr"})
    assert error is None
    assert public["language"] == "fr"
    assert handler.config.language == "fr"
    public, error = handler._apply_config_partial({"language": ""})
    assert error is None
    assert "language" not in public
    assert handler.config.language is None


@pytest.mark.parametrize("value", [5, ["de"], "de_DE", "../x", {"name": "de"}])
def test_post_config_refuses_a_bad_language(tmp_path, value):
    handler, _ = _handler(tmp_path)
    public, error = handler._apply_config_partial({"language": value})
    assert public is None
    assert "language" in error


def test_another_setting_keeps_the_language(tmp_path):
    handler, _ = _handler(tmp_path)
    handler._apply_config_partial({"language": "de"})
    public, error = handler._apply_config_partial({"theme": {"name": "green"}})
    assert error is None
    assert public["language"] == "de"


def test_a_language_not_installed_here_is_still_accepted(tmp_path):
    handler, _ = _handler(tmp_path)
    public, error = handler._apply_config_partial({"language": "pt-BR"})
    assert error is None
    assert public["language"] == "pt-BR"


# ---------------------------------------------------------------------------
# What ships
# ---------------------------------------------------------------------------

def _tool():
    spec = importlib.util.spec_from_file_location("i18n_tool", ROOT / "tools" / "i18n.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_the_three_languages_ship():
    rows = {r["code"]: r for r in languages.discover(None, languages.bundled_languages_dir())}
    assert {"en", "de", "fr"} <= set(rows)
    assert rows["de"]["native_name"] == "Deutsch"
    assert rows["fr"]["native_name"] == "Français"
    assert rows["en"]["count"] == 0
    assert rows["de"]["count"] > 500
    assert rows["fr"]["count"] > 500


def test_every_shipped_file_passes_the_checker():
    tool = _tool()
    union: set[str] = set()
    for path in tool.files():
        union |= set(tool.load_raw(path)["strings"])
    for path in tool.files():
        errors = [m for m in tool.problems(path, union) if not m.startswith("note:")]
        assert errors == [], f"{path.name}: {errors[:5]}"


def test_the_shipped_languages_translate_the_same_texts():
    de = languages.load("de", None, languages.bundled_languages_dir())["strings"]
    fr = languages.load("fr", None, languages.bundled_languages_dir())["strings"]
    assert set(de) == set(fr)


@pytest.mark.parametrize("code", ["de", "fr"])
def test_the_core_screens_are_translated(code):
    strings = languages.load(code, None, languages.bundled_languages_dir())["strings"]
    for text in ("Settings", "Language", "Takeoff", "TAKEOFF", "LAND", "RETURN", "Welcome to Corvus GCS",
                 "Set up this station", "Skip for now", "Look", "Units", "Cancel", "Save", "Close"):
        assert strings.get(text), f"{code}: {text!r} has no translation"


def test_the_language_folder_ships_a_readme_the_loader_ignores():
    assert (Path(languages.bundled_languages_dir()) / "README.md").is_file()
    codes = {r["code"] for r in languages.discover(None, languages.bundled_languages_dir())}
    assert "README" not in codes


def test_user_facing_translations_use_no_dashes():
    for code in ("de", "fr"):
        strings = languages.load(code, None, languages.bundled_languages_dir())["strings"]
        for key, value in strings.items():
            assert "—" not in value and "–" not in value, (code, key)
            assert " - " not in value, (code, key)
