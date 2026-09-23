"""Place search for the map — "fly to Manching" — over OSM Nominatim.

The planner needs one thing the map cannot give it: a way to get somewhere by
name. Panning from the last operating site to the next one across a country at
zoom 15 is minutes of dragging, and a coordinate pair typed into a box is only
half an answer — operators know the airfield's name, not its decimals.

This module is the online half of that. The offline half (a coordinate pair,
in any of the notations a briefing writes them in) is parsed in the frontend
and never reaches here, so a laptop with no internet still answers the search
box instantly for the case that matters most in the field.

Offline contract
----------------
Same rules the tile proxy and the update check are under, because the same
laptop runs all three:

* Nothing here touches the network at import or at startup. A request happens
  only when the operator searches.
* A failure is reported as a sentence, never as a 500, and puts the module in
  a short back-off so an offline operator does not spend the socket timeout on
  every keystroke.
* Answers are cached in memory, so re-running the same search — which is what
  paging back through a session looks like — costs nothing.

Nominatim's usage policy is a condition of using it, not a suggestion: at most
one request a second, and a real User-Agent identifying the application. Both
are enforced here rather than trusted to the caller, because the caller is a
UI event and UI events arrive as fast as somebody can type.

Only ``lat``/``lon``/``boundingbox`` are trusted out of the payload, and every
one of them is parsed as a number and range-checked before it leaves this
module. The label is text the frontend sets as ``textContent``; no URL,
markup or coordinate is ever taken from upstream unvalidated.
"""
from __future__ import annotations

import json
import logging
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, NamedTuple

logger = logging.getLogger("corvus.geocode")

SEARCH_URL = "https://nominatim.openstreetmap.org/search"
# Nominatim's published limit is one request per second. The margin is for
# clock jitter, not politeness theatre: a burst that lands at 0.999s gets the
# whole application blocked, and the application is a ground station.
MIN_INTERVAL_S = 1.1
# How long a search may wait for its slot before giving up. Past this the
# operator is typing faster than the service may be asked, and a queue of
# stale searches is worse than a "try again".
MAX_WAIT_S = 3.0
FETCH_TIMEOUT_S = 6.0
# A handful of places is a search result; a thousand is a data export, which
# the usage policy forbids and no operator wants to read.
MAX_RESULTS = 8
DEFAULT_RESULTS = 6
# Nobody is waiting long on this, but a hung socket must not hold a handler
# thread and a huge body must not exhaust memory. Eight places is a few KB.
MAX_PAYLOAD_BYTES = 512 * 1024
# After a failure, answer "offline" immediately rather than spending the
# timeout again. Short enough that plugging in a hotspot is noticed within a
# search or two.
OFFLINE_BACKOFF_S = 20.0
# In-memory only, and small: this is a convenience for the session, not a
# gazetteer. A cached miss counts, so a typo is not retried against the
# service every time it is re-typed.
CACHE_MAX = 64
# How far around the map's centre results are preferred, in degrees. A bias,
# never a filter (`bounded=0`), so "Bremen" typed in Bavaria still finds
# Bremen — it just finds the Bremen nearby first when there is one.
VIEWBOX_DEG = 2.0
# Cache cell for the location bias, in degrees. The cache key has to carry
# `near`, or the bias silently stops working on the second search: "Neustadt"
# looked up at site A would be answered from cache, still ranked around A,
# after the operator has driven to site B — which is exactly the search
# someone running two sites in a day makes. Keying on the raw coordinate
# would make every pixel of pan a cache miss, so the bias is bucketed: one
# degree against a four-degree viewbox keeps the ranking honest while a
# search repeated from the same site still hits.
CACHE_CELL_DEG = 1.0


class GeocodeError(RuntimeError):
    """The search could not be run. The message is shown to the operator."""


def _finite(value: Any) -> float | None:
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    return number if number == number and abs(number) != float("inf") else None


class Near(NamedTuple):
    """A map centre used to bias a search. **Longitude first.**

    A named type rather than a bare pair because this module holds both
    conventions: upstream results come back latitude-first (:class:`Point`),
    the query's viewbox is built longitude-first, and both were plain
    ``tuple[float, float]``. Nothing was wrong — every caller happened to
    agree — but a ground station that flies to the wrong hemisphere because
    somebody read a tuple the other way round is not a hypothetical class of
    failure, and it is exactly the kind of thing a refactor introduces
    silently. Now the order is in the type.

    A plain ``(lon, lat)`` tuple still works everywhere one of these is
    accepted: this unpacks identically, so no caller had to change.
    """
    lon: float
    lat: float


class Point(NamedTuple):
    """A place found upstream. **Latitude first**, the way Nominatim sends it."""
    lat: float
    lon: float


def _coordinate(lat: Any, lon: Any) -> Point | None:
    """A :class:`Point` from upstream, or None if it is not one."""
    latitude, longitude = _finite(lat), _finite(lon)
    if latitude is None or longitude is None:
        return None
    if not (-90.0 <= latitude <= 90.0) or not (-180.0 <= longitude <= 180.0):
        return None
    return Point(latitude, longitude)


def _bounds(raw: Any) -> dict[str, float] | None:
    """Nominatim's ``boundingbox`` as the {w,s,e,n} the frontend fits to.

    Upstream orders it [south, north, west, east] as *strings*. A box that is
    not four parseable numbers in range is dropped rather than repaired: the
    caller falls back to centring on the point, which is always right, where a
    half-read box would frame the wrong ground.
    """
    if not isinstance(raw, (list, tuple)) or len(raw) != 4:
        return None
    south, north, west, east = (_finite(v) for v in raw)
    if None in (south, north, west, east):
        return None
    if not (-90.0 <= south <= north <= 90.0):
        return None
    if not (-180.0 <= west <= 180.0) or not (-180.0 <= east <= 180.0):
        return None
    if east < west:  # a box across the antimeridian; fitBounds cannot use it
        return None
    return {"w": west, "s": south, "e": east, "n": north}


def _label(entry: dict[str, Any]) -> str:
    """What the row says. Upstream text, length-capped, never markup."""
    text = entry.get("display_name") or entry.get("name") or ""
    if not isinstance(text, str):
        return ""
    text = " ".join(text.split())
    return text[:200]


def _kind(entry: dict[str, Any]) -> str:
    """A one-word sort of place ("airport", "town"), for the row's caption."""
    for key in ("type", "category", "class"):
        value = entry.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip().replace("_", " ")[:40]
    return ""


def parse_results(payload: Any, limit: int = DEFAULT_RESULTS) -> list[dict[str, Any]]:
    """Upstream JSON -> the rows the frontend draws. Never raises.

    Pure, and exported for it: this is where a malformed or hostile payload
    has to fail safely, and that is assertable without a network.
    """
    if not isinstance(payload, list):
        return []
    results: list[dict[str, Any]] = []
    for entry in payload:
        if not isinstance(entry, dict):
            continue
        point = _coordinate(entry.get("lat"), entry.get("lon"))
        label = _label(entry)
        if point is None or not label:
            continue
        results.append({
            "label": label,
            "lat": point.lat,
            "lon": point.lon,
            "bounds": _bounds(entry.get("boundingbox")),
            "kind": _kind(entry),
        })
        if len(results) >= limit:
            break
    return results


def cache_cell(near: Near | tuple[float, float] | None) -> tuple[int, int] | None:
    """The bias bucket a ``near`` falls in, for the cache key. Pure.

    ``None`` for an unbiased search, so a biased and an unbiased lookup of the
    same text never share an entry.
    """
    if near is None:
        return None
    centre = Near(*near)
    return (round(centre.lon / CACHE_CELL_DEG), round(centre.lat / CACHE_CELL_DEG))


def build_query(query: str, limit: int, near: Near | tuple[float, float] | None) -> str:
    """The upstream query string. Pure, so the bias is assertable offline.

    ``near`` is longitude-first — see :class:`Near`.
    """
    params = {
        "q": query,
        "format": "jsonv2",
        "limit": str(limit),
        "addressdetails": "0",
    }
    if near is not None:
        centre = Near(*near)
        west = max(-180.0, centre.lon - VIEWBOX_DEG)
        east = min(180.0, centre.lon + VIEWBOX_DEG)
        north = min(90.0, centre.lat + VIEWBOX_DEG)
        south = max(-90.0, centre.lat - VIEWBOX_DEG)
        params["viewbox"] = f"{west:.5f},{north:.5f},{east:.5f},{south:.5f}"
        params["bounded"] = "0"
    return urllib.parse.urlencode(params)


class Geocoder:
    """One place search service for the process.

    Holds no thread, socket or file handle between calls — every request is a
    single ``urlopen`` with a timeout — so there is nothing for the shutdown
    path to tear down. The lock and the cache are plain memory.
    """

    def __init__(self, user_agent: str, url: str = SEARCH_URL,
                 timeout: float = FETCH_TIMEOUT_S) -> None:
        self.url = url
        self.user_agent = user_agent
        self.timeout = timeout
        self._lock = threading.Lock()
        self._next_at = 0.0
        self._offline_until = 0.0
        self._cache: dict[tuple[str, int], list[dict[str, Any]]] = {}

    # -- rate limit ----------------------------------------------------
    def _reserve(self) -> float:
        """Claim the next request slot; returns how long to wait for it.

        A wait past the ceiling claims NOTHING. The caller refuses that search,
        and a refused search that had consumed its slot anyway would push the
        queue further out for the next one — which is how a burst of typing
        turns into a search box that stays broken after the typing stops.
        """
        with self._lock:
            now = time.monotonic()
            wait = max(0.0, self._next_at - now)
            if wait > MAX_WAIT_S:
                return wait
            self._next_at = max(now, self._next_at) + MIN_INTERVAL_S
            return wait

    def _offline(self) -> bool:
        with self._lock:
            return time.monotonic() < self._offline_until

    def _back_off(self, seconds: float = OFFLINE_BACKOFF_S) -> None:
        with self._lock:
            self._offline_until = time.monotonic() + seconds

    def _clear_back_off(self) -> None:
        with self._lock:
            self._offline_until = 0.0

    # -- cache ---------------------------------------------------------
    def _cached(self, key: tuple) -> list[dict[str, Any]] | None:
        with self._lock:
            hit = self._cache.get(key)
            return [dict(row) for row in hit] if hit is not None else None

    def _remember(self, key: tuple, rows: list[dict[str, Any]]) -> None:
        with self._lock:
            if len(self._cache) >= CACHE_MAX:
                # Oldest insertion first; dicts preserve it and this is a
                # session convenience, not a cache that needs an LRU.
                self._cache.pop(next(iter(self._cache)), None)
            self._cache[key] = [dict(row) for row in rows]

    # -- the search ----------------------------------------------------
    def search(self, query: str, limit: int = DEFAULT_RESULTS,
               near: Near | tuple[float, float] | None = None) -> list[dict[str, Any]]:
        """Places matching ``query``, nearest ``near`` first where it is given.

        An empty list means the search ran and found nothing. A
        :class:`GeocodeError` means it could not run at all — no network, the
        service refused, or the operator is typing faster than it may be
        asked. The two are different answers and the UI says different things.
        """
        text = " ".join(str(query or "").split())
        if not text:
            return []
        limit = max(1, min(MAX_RESULTS, int(limit or DEFAULT_RESULTS)))
        # `near` belongs in the key: it changes the query, so it changes the
        # answer. See CACHE_CELL_DEG for why it is bucketed and not exact.
        key = (text.casefold(), limit, cache_cell(near))
        cached = self._cached(key)
        if cached is not None:
            return cached
        if self._offline():
            raise GeocodeError("No connection to the place-search service.")
        wait = self._reserve()
        if wait > MAX_WAIT_S:
            raise GeocodeError("Place search is busy. Try that again.")
        if wait > 0:
            time.sleep(wait)
        payload = self._fetch(build_query(text, limit, near))
        rows = parse_results(payload, limit)
        self._remember(key, rows)
        return rows

    def _fetch(self, query_string: str) -> Any:
        request = urllib.request.Request(
            f"{self.url}?{query_string}",
            headers={"User-Agent": self.user_agent, "Accept": "application/json"},
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                raw = response.read(MAX_PAYLOAD_BYTES + 1)
        except urllib.error.HTTPError as exc:
            # 429 is the service saying "you broke the one-per-second rule".
            # Backing off is the only correct response to it, and a longer one
            # than an offline laptop needs.
            logger.debug("geocode HTTP %s", exc.code)
            self._back_off(60.0 if exc.code in (429, 503) else OFFLINE_BACKOFF_S)
            raise GeocodeError("The place-search service refused the request.") from None
        except Exception:  # noqa: BLE001 - offline field use must never 500
            logger.debug("geocode request failed", exc_info=True)
            self._back_off()
            raise GeocodeError("No connection to the place-search service.") from None
        if not raw or len(raw) > MAX_PAYLOAD_BYTES:
            self._back_off()
            raise GeocodeError("The place-search service sent an unusable answer.")
        try:
            payload = json.loads(raw.decode("utf-8", "replace"))
        except ValueError:
            self._back_off()
            raise GeocodeError("The place-search service sent an unusable answer.") from None
        self._clear_back_off()
        return payload
