"""The operator's preflight checklists: what is kept, and its bounds.

A checklist is the operator's own text. Corvus does not interpret an item and
never acts on one: ticking an item off happens in the browser and is not
stored here, because it belongs to one flight rather than to the station.

Stored under the ``checklists`` key of the application config::

    {
      "enabled": false,        # the feature at all (Settings > Pages)
      "home_window": true,     # the checklist window on the Home map
      "active": "<list id>",   # the list that window shows
      "lists": [
        {"id": "...", "name": "...",
         "items": [{"text": "...", "heading": false}, ...]},
      ],
    }

Every key is optional and kept independently. ``lists`` absent means the
operator has never written one and the frontend offers its standard list;
``lists`` present and empty means they deleted every list, which is kept as
such rather than bringing the standard one back.

stdlib only.
"""
from __future__ import annotations

import re
from typing import Any

MAX_LISTS = 32
MAX_ITEMS = 120
MAX_NAME = 80
MAX_TEXT = 200

_ID_RE = re.compile(r"^[A-Za-z0-9_-]{1,40}$")


def _clean_text(value: Any, limit: int) -> str:
    """One line of text, trimmed and cut to *limit*; "" for anything else.

    Newlines and tabs become spaces: an item is one row in a window, and a
    control character pasted from elsewhere must not break that row.
    """
    if not isinstance(value, str):
        return ""
    text = re.sub(r"[\x00-\x1f\x7f]+", " ", value)
    text = re.sub(r" {2,}", " ", text).strip()
    return text[:limit].rstrip()


def _coerce_item(raw: Any) -> dict[str, Any] | None:
    """One item or section heading, or None when it has no text."""
    if isinstance(raw, str):
        raw = {"text": raw}
    if not isinstance(raw, dict):
        return None
    text = _clean_text(raw.get("text"), MAX_TEXT)
    if not text:
        return None
    return {"text": text, "heading": raw.get("heading") is True}


def _coerce_list(raw: Any, index: int, taken: set[str]) -> dict[str, Any] | None:
    """One checklist, with a unique id; None when it is not an object.

    A missing, malformed or repeated id is replaced rather than the list
    dropped: the id only ties the Home window's choice and its ticks to the
    list, and losing the operator's text over it would be the worse outcome.
    """
    if not isinstance(raw, dict):
        return None
    list_id = raw.get("id")
    if not isinstance(list_id, str) or not _ID_RE.match(list_id) or list_id in taken:
        list_id = f"list{index + 1}"
        suffix = 1
        while list_id in taken:
            suffix += 1
            list_id = f"list{index + 1}_{suffix}"
    taken.add(list_id)
    name = _clean_text(raw.get("name"), MAX_NAME) or f"Checklist {index + 1}"
    items_raw = raw.get("items")
    items: list[dict[str, Any]] = []
    if isinstance(items_raw, list):
        for entry in items_raw:
            item = _coerce_item(entry)
            if item is not None:
                items.append(item)
            if len(items) >= MAX_ITEMS:
                break
    return {"id": list_id, "name": name, "items": items}


def coerce(raw: Any) -> dict[str, Any] | None:
    """Keep the known checklist keys with bounded values; None when none are left.

    Booleans are genuine booleans only, like the other switches in the
    config: ``"false"`` in a hand-edited file must not read as on.
    """
    if not isinstance(raw, dict):
        return None
    out: dict[str, Any] = {}
    for key in ("enabled", "home_window"):
        if isinstance(raw.get(key), bool):
            out[key] = raw[key]
    lists_raw = raw.get("lists")
    if isinstance(lists_raw, list):
        taken: set[str] = set()
        lists: list[dict[str, Any]] = []
        for index, entry in enumerate(lists_raw[:MAX_LISTS]):
            coerced = _coerce_list(entry, index, taken)
            if coerced is not None:
                lists.append(coerced)
        out["lists"] = lists
    active = raw.get("active")
    if isinstance(active, str) and _ID_RE.match(active):
        out["active"] = active
    return out or None


def copy(block: dict[str, Any]) -> dict[str, Any]:
    """A copy deep enough that the caller cannot mutate the stored lists."""
    out = dict(block)
    if isinstance(block.get("lists"), list):
        out["lists"] = [
            {**entry, "items": [dict(item) for item in entry.get("items", [])]}
            for entry in block["lists"]
        ]
    return out
