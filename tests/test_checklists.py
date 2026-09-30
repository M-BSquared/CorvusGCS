"""Preflight checklists: the bounds on what is kept, and how it is written.

The lists are the operator's own text, stored in the ``checklists`` block of
the application config (``corvus/checklists.py``). What matters here is that a
hand-edited or hostile file cannot produce anything the Home window cannot
draw, and that the three places that write the block (the Settings switch,
the Home window's list picker, the editor) never erase each other's keys.
"""
from __future__ import annotations

from typing import Any

from corvus import checklists, settings_bundle
from corvus.config import CorvusConfig, _build_config, _config_to_dict, load_config
from corvus.server import CorvusHandler


def _handler(config: CorvusConfig, config_path: str) -> tuple[CorvusHandler, list]:
    handler = object.__new__(CorvusHandler)
    handler.config = config
    handler.config_path = config_path
    handler.ssh = None
    handler.mavlink = None
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda payload, status=200: responses.append((payload, status))
    return handler, responses


def test_absent_or_malformed_block_is_none() -> None:
    assert checklists.coerce(None) is None
    assert checklists.coerce("on") is None
    assert checklists.coerce({"enabled": "true"}) is None


def test_switches_are_genuine_booleans_only() -> None:
    assert checklists.coerce({"enabled": True, "home_window": False}) == {
        "enabled": True, "home_window": False}
    assert checklists.coerce({"enabled": 1, "home_window": "false"}) is None


def test_items_are_trimmed_single_lines_and_empty_ones_dropped() -> None:
    out = checklists.coerce({"lists": [{"id": "a", "name": "  Quad  ", "items": [
        {"text": "  Props\ntight  "},
        {"text": "   "},
        "Battery strapped",
        {"text": "Before takeoff", "heading": True},
        {"text": "Odd", "heading": "yes"},
        42,
    ]}]})
    assert out == {"lists": [{"id": "a", "name": "Quad", "items": [
        {"text": "Props tight", "heading": False},
        {"text": "Battery strapped", "heading": False},
        {"text": "Before takeoff", "heading": True},
        {"text": "Odd", "heading": False},
    ]}]}


def test_lengths_and_counts_are_bounded() -> None:
    raw = {"lists": [{"id": f"l{i}", "name": "n" * 500,
                      "items": [{"text": "t" * 999}] * (checklists.MAX_ITEMS + 10)}
                     for i in range(checklists.MAX_LISTS + 5)]}
    out = checklists.coerce(raw)
    assert out is not None
    assert len(out["lists"]) == checklists.MAX_LISTS
    first = out["lists"][0]
    assert len(first["name"]) == checklists.MAX_NAME
    assert len(first["items"]) == checklists.MAX_ITEMS
    assert len(first["items"][0]["text"]) == checklists.MAX_TEXT


def test_bad_or_repeated_ids_are_replaced_not_dropped() -> None:
    out = checklists.coerce({"lists": [
        {"id": "same", "name": "A", "items": ["x"]},
        {"id": "same", "name": "B", "items": ["y"]},
        {"id": "../etc", "name": "C", "items": ["z"]},
        {"name": "", "items": []},
    ]})
    assert out is not None
    ids = [entry["id"] for entry in out["lists"]]
    assert len(set(ids)) == 4
    assert ids[0] == "same"
    assert [entry["name"] for entry in out["lists"]] == ["A", "B", "C", "Checklist 4"]


def test_an_empty_list_of_lists_is_kept_as_such() -> None:
    """Deleting every list must not bring the standard one back."""
    assert checklists.coerce({"lists": []}) == {"lists": []}


def test_config_round_trip_and_copy_is_deep() -> None:
    cfg = _build_config({"checklists": {"enabled": True, "active": "a",
                                        "lists": [{"id": "a", "name": "A", "items": ["x"]}]}})
    out = _config_to_dict(cfg)
    out["checklists"]["lists"][0]["items"][0]["text"] = "mutated"
    assert cfg.checklists["lists"][0]["items"][0]["text"] == "x"
    assert _build_config(_config_to_dict(cfg)).checklists == cfg.checklists


def test_checklists_travel_with_the_vehicle_section() -> None:
    assert settings_bundle.section_of("checklists") == "vehicle"


def test_post_merges_per_key(tmp_path) -> None:
    """The Settings switch, the Home picker and the editor each write one key;
    none may erase another's."""
    cfg_path = str(tmp_path / "config.json")
    handler, responses = _handler(CorvusConfig(), cfg_path)
    lists: list[dict[str, Any]] = [{"id": "a", "name": "A", "items": [{"text": "x"}]},
                                   {"id": "b", "name": "B", "items": [{"text": "y"}]}]
    handler._api_config_update({"checklists": {"lists": lists}})
    handler._api_config_update({"checklists": {"enabled": True}})
    handler._api_config_update({"checklists": {"active": "b"}})
    payload, status = responses[-1]
    assert status == 200
    block = payload["config"]["checklists"]
    assert block["enabled"] is True
    assert block["active"] == "b"
    assert [entry["id"] for entry in block["lists"]] == ["a", "b"]
    assert load_config(cfg_path).checklists == block

    handler._api_config_update({"checklists": {"lists": lists[:1]}})
    assert [entry["id"] for entry in responses[-1][0]["config"]["checklists"]["lists"]] == ["a"]


def test_post_rejects_a_non_object(tmp_path) -> None:
    handler, responses = _handler(CorvusConfig(), str(tmp_path / "config.json"))
    handler._api_config_update({"checklists": ["a"]})
    payload, status = responses[-1]
    assert status == 400
    assert "checklists" in payload["error"]
