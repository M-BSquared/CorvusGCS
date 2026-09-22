"""Tests for corvus/geocode.py and the /api/geocode endpoint.

Hermetic and offline: every network call is a monkeypatched stand-in for
``urllib.request.urlopen``. Nothing here reaches Nominatim, which is both a
test requirement and the usage policy — a suite that geocoded on every run
would be exactly the bulk traffic the service forbids.

What is asserted is what goes wrong quietly:

* a hostile or malformed payload must not reach the map as coordinates,
* the one-request-per-second rule must be enforced by the module rather than
  trusted to the caller (the caller is a keystroke),
* an offline laptop must be told so once and then answered instantly, rather
  than spending the socket timeout on every search,
* and the endpoint must never turn any of that into a 500.
"""
from __future__ import annotations

import http.client
import io
import json
import threading
import urllib.error
from typing import Any

import pytest

from corvus import geocode
from corvus.geocode import Geocoder, GeocodeError
from corvus.server import CorvusHandler, CorvusServer


# ---------------------------------------------------------------------------
# parse_results — where a bad payload has to fail safely
# ---------------------------------------------------------------------------

def test_parse_results_keeps_a_well_formed_place() -> None:
    rows = geocode.parse_results([{
        "display_name": "Manching, Bavaria, Germany",
        "lat": "48.7167", "lon": "11.5000",
        "boundingbox": ["48.69", "48.74", "11.46", "11.55"],
        "type": "town",
    }])
    assert rows == [{
        "label": "Manching, Bavaria, Germany",
        "lat": 48.7167, "lon": 11.5,
        "bounds": {"w": 11.46, "s": 48.69, "e": 11.55, "n": 48.74},
        "kind": "town",
    }]


@pytest.mark.parametrize("entry", [
    {"display_name": "nowhere", "lat": "abc", "lon": "1"},
    {"display_name": "nowhere", "lat": "91", "lon": "1"},
    {"display_name": "nowhere", "lat": "1", "lon": "181"},
    {"display_name": "nowhere", "lat": None, "lon": None},
    {"display_name": "", "lat": "1", "lon": "1"},
    {"lat": "1", "lon": "1"},
    "not a dict",
])
def test_parse_results_drops_anything_that_is_not_a_place(entry: Any) -> None:
    assert geocode.parse_results([entry]) == []


def test_parse_results_ignores_a_payload_that_is_not_a_list() -> None:
    assert geocode.parse_results({"results": []}) == []
    assert geocode.parse_results(None) == []


@pytest.mark.parametrize("box", [
    ["48.69", "48.74", "11.46"],                 # three of four
    ["48.74", "48.69", "11.46", "11.55"],        # north before south
    ["48.69", "48.74", "11.55", "11.46"],        # east before west
    ["48.69", "48.74", "11.46", "junk"],
    "48.69,48.74,11.46,11.55",
])
def test_a_box_that_cannot_be_read_is_dropped_not_repaired(box: Any) -> None:
    """The point is always right; a half-read box would frame other ground."""
    rows = geocode.parse_results([{
        "display_name": "somewhere", "lat": "48.7", "lon": "11.5",
        "boundingbox": box,
    }])
    assert len(rows) == 1
    assert rows[0]["bounds"] is None


def test_the_label_is_capped_and_never_carries_markup_through() -> None:
    rows = geocode.parse_results([{
        "display_name": "<img src=x onerror=alert(1)> " + "x" * 400,
        "lat": "1", "lon": "1",
    }])
    # Not escaped here on purpose — the frontend sets it as textContent. What
    # this guards is the length, which is the part a renderer cannot fix.
    assert len(rows[0]["label"]) == 200


def test_parse_results_honours_the_limit() -> None:
    payload = [{"display_name": f"p{i}", "lat": "1", "lon": "1"} for i in range(20)]
    assert len(geocode.parse_results(payload, limit=3)) == 3


# ---------------------------------------------------------------------------
# build_query — the bias is a preference, never a filter
# ---------------------------------------------------------------------------

def test_build_query_without_a_centre_asks_for_no_viewbox() -> None:
    query = geocode.build_query("manching", 5, None)
    assert "viewbox" not in query and "bounded" not in query
    assert "q=manching" in query and "limit=5" in query


def test_build_query_biases_toward_the_map_but_never_bounds_it() -> None:
    query = geocode.build_query("bremen", 5, (11.5, 48.7))
    assert "bounded=0" in query, "a bias that filters would hide the right answer"
    assert "viewbox=" in query


def test_the_viewbox_is_clamped_to_the_world_near_a_pole() -> None:
    query = geocode.build_query("x", 5, (179.5, 89.5))
    box = [float(v) for v in query.split("viewbox=")[1].split("&")[0]
           .replace("%2C", ",").split(",")]
    west, north, east, south = box
    assert -180.0 <= west <= east <= 180.0
    assert -90.0 <= south <= north <= 90.0


# ---------------------------------------------------------------------------
# The service: rate limit, cache, back-off
# ---------------------------------------------------------------------------

class FakeResponse(io.BytesIO):
    def __enter__(self) -> FakeResponse:
        return self

    def __exit__(self, *exc: Any) -> bool:
        self.close()
        return False


def _payload(*names: str) -> bytes:
    return json.dumps([
        {"display_name": n, "lat": "48.0", "lon": "11.0", "type": "town"}
        for n in names
    ]).encode()


@pytest.fixture
def patched_urlopen(monkeypatch):
    """Records every request and answers with whatever the test queued."""
    calls: list[str] = []
    answers: list[Any] = []

    def fake_urlopen(request, timeout=None):  # noqa: ANN001
        calls.append(request.full_url)
        answer = answers.pop(0) if answers else _payload("Somewhere")
        if isinstance(answer, Exception):
            raise answer
        return FakeResponse(answer)

    monkeypatch.setattr(geocode.urllib.request, "urlopen", fake_urlopen)
    return calls, answers


def test_a_search_returns_places_and_sends_the_user_agent(patched_urlopen, monkeypatch) -> None:
    calls, answers = patched_urlopen
    seen: dict[str, str] = {}

    def capture(request, timeout=None):  # noqa: ANN001
        seen.update(request.headers)
        return FakeResponse(_payload("Manching"))

    monkeypatch.setattr(geocode.urllib.request, "urlopen", capture)
    rows = Geocoder(user_agent="CorvusGCS/test").search("manching")
    assert [r["label"] for r in rows] == ["Manching"]
    # Nominatim's policy is explicit that an anonymous client may be blocked.
    assert seen.get("User-agent") == "CorvusGCS/test"


def test_an_empty_query_never_reaches_the_network(patched_urlopen) -> None:
    calls, _ = patched_urlopen
    assert Geocoder(user_agent="t").search("   ") == []
    assert calls == []


def test_the_same_search_twice_costs_one_request(patched_urlopen) -> None:
    calls, _ = patched_urlopen
    coder = Geocoder(user_agent="t")
    first = coder.search("manching")
    second = coder.search("  MANCHING  ")   # same place, same cache entry
    assert first == second
    assert len(calls) == 1


def test_a_search_from_a_different_site_is_not_answered_from_cache(patched_urlopen) -> None:
    """The bias is part of the query, so it is part of the cache key.

    Two sites a country apart: the second search must reach the service with
    its own viewbox, not come back ranked around the first one.
    """
    calls, _ = patched_urlopen
    coder = Geocoder(user_agent="t")
    coder.search("neustadt", near=(11.5, 48.8))    # site A, Bavaria
    coder.search("neustadt", near=(8.8, 53.1))     # site B, Bremen
    assert len(calls) == 2, "the second site was answered with the first site's ranking"
    assert "viewbox" in calls[0] and "viewbox" in calls[1]
    assert calls[0] != calls[1], "both searches went upstream with the same viewbox"


def test_a_search_repeated_from_the_same_site_still_costs_one_request(patched_urlopen) -> None:
    """Bucketed, not exact: nudging the map must not empty the cache."""
    calls, _ = patched_urlopen
    coder = Geocoder(user_agent="t")
    coder.search("manching", near=(11.50, 48.72))
    coder.search("manching", near=(11.51, 48.73))
    assert len(calls) == 1


def test_a_biased_and_an_unbiased_search_are_different_entries(patched_urlopen) -> None:
    calls, _ = patched_urlopen
    coder = Geocoder(user_agent="t")
    coder.search("manching")
    coder.search("manching", near=(11.5, 48.7))
    assert len(calls) == 2


def test_cache_cell_buckets_a_coordinate_and_passes_none_through() -> None:
    from corvus.geocode import cache_cell

    assert cache_cell(None) is None
    assert cache_cell((11.4, 48.6)) == cache_cell((11.3, 48.7))
    assert cache_cell((11.4, 48.6)) != cache_cell((8.8, 53.1))


def test_the_cache_hands_back_copies_not_its_own_rows(patched_urlopen) -> None:
    coder = Geocoder(user_agent="t")
    rows = coder.search("manching")
    rows[0]["label"] = "tampered"
    assert coder.search("manching")[0]["label"] != "tampered"


def test_requests_are_spaced_at_the_published_rate(patched_urlopen, monkeypatch) -> None:
    """One per second is a condition of use, not a courtesy."""
    slept: list[float] = []
    monkeypatch.setattr(geocode.time, "sleep", lambda s: slept.append(s))
    coder = Geocoder(user_agent="t")
    coder.search("one")
    coder.search("two")
    assert slept and slept[0] >= geocode.MIN_INTERVAL_S * 0.9


def test_typing_faster_than_the_service_may_be_asked_is_refused_not_queued(
    patched_urlopen, monkeypatch,
) -> None:
    monkeypatch.setattr(geocode.time, "sleep", lambda s: None)
    coder = Geocoder(user_agent="t")
    ceiling = int(geocode.MAX_WAIT_S / geocode.MIN_INTERVAL_S) + 2
    refused = 0
    for i in range(ceiling + 3):
        try:
            coder.search(f"query {i}")
        except GeocodeError:
            refused += 1
    assert refused, "a burst faster than one per second must be refused"
    # A refused search claimed no slot, so the queue does not keep growing:
    # once the burst stops, the box works again on the next tick.
    assert refused <= 3 + 1


def test_being_offline_is_a_sentence_and_then_an_instant_answer(patched_urlopen, monkeypatch) -> None:
    calls, answers = patched_urlopen
    monkeypatch.setattr(geocode.time, "sleep", lambda s: None)
    answers.append(urllib.error.URLError("no route to host"))
    coder = Geocoder(user_agent="t")
    with pytest.raises(GeocodeError) as first:
        coder.search("manching")
    assert "connection" in str(first.value).lower()
    with pytest.raises(GeocodeError):
        coder.search("ingolstadt")
    # The second search spent no socket at all — that is the whole point of
    # the back-off on a laptop with no internet.
    assert len(calls) == 1


def test_a_rate_limit_answer_backs_off_rather_than_retrying(patched_urlopen, monkeypatch) -> None:
    calls, answers = patched_urlopen
    monkeypatch.setattr(geocode.time, "sleep", lambda s: None)
    answers.append(urllib.error.HTTPError("u", 429, "Too Many Requests", {}, None))
    coder = Geocoder(user_agent="t")
    with pytest.raises(GeocodeError):
        coder.search("manching")
    with pytest.raises(GeocodeError):
        coder.search("ingolstadt")
    assert len(calls) == 1


def test_a_body_that_is_not_json_is_an_error_not_a_crash(patched_urlopen, monkeypatch) -> None:
    _, answers = patched_urlopen
    monkeypatch.setattr(geocode.time, "sleep", lambda s: None)
    answers.append(b"<html>503</html>")
    with pytest.raises(GeocodeError):
        Geocoder(user_agent="t").search("manching")


def test_an_oversized_body_is_refused(patched_urlopen, monkeypatch) -> None:
    _, answers = patched_urlopen
    monkeypatch.setattr(geocode.time, "sleep", lambda s: None)
    answers.append(b"[" + b"0," * geocode.MAX_PAYLOAD_BYTES)
    with pytest.raises(GeocodeError):
        Geocoder(user_agent="t").search("manching")


def test_a_recovered_connection_clears_the_back_off(patched_urlopen, monkeypatch) -> None:
    calls, answers = patched_urlopen
    monkeypatch.setattr(geocode.time, "sleep", lambda s: None)
    coder = Geocoder(user_agent="t")
    coder.search("first")                 # succeeds, clears any back-off
    answers.append(_payload("Later"))
    assert coder.search("second")[0]["label"] == "Later"
    assert len(calls) == 2


def test_the_geocoder_holds_nothing_that_needs_tearing_down() -> None:
    """The shutdown-path invariant: no thread, no socket, no file handle."""
    before = threading.active_count()
    coder = Geocoder(user_agent="t")
    assert threading.active_count() == before
    assert not hasattr(coder, "shutdown")


# ---------------------------------------------------------------------------
# The endpoint
# ---------------------------------------------------------------------------

class StubGeocoder:
    def __init__(self, rows=None, error: str | None = None) -> None:
        self.rows = rows or []
        self.error = error
        self.calls: list[tuple] = []

    def search(self, query, limit=6, near=None):  # noqa: ANN001
        self.calls.append((query, limit, near))
        if self.error is not None:
            raise GeocodeError(self.error)
        return self.rows


@pytest.fixture
def geocode_server():
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    saved = (CorvusHandler.store, CorvusHandler.mavlink,
             CorvusHandler.ssh, CorvusHandler.geocoder)
    CorvusHandler.store = None
    CorvusHandler.mavlink = None
    CorvusHandler.ssh = None
    stub = StubGeocoder()
    CorvusHandler.geocoder = stub
    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    thread = threading.Thread(
        target=server.serve_forever, name="corvus-test-geocode", daemon=True,
        kwargs={"poll_interval": 0.05},
    )
    thread.start()
    try:
        yield server, stub
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        (CorvusHandler.store, CorvusHandler.mavlink,
         CorvusHandler.ssh, CorvusHandler.geocoder) = saved


def _get(server: CorvusServer, path: str) -> tuple[int, dict]:
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    conn.request("GET", path)
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, json.loads(body)


def test_endpoint_returns_the_rows_it_was_given(geocode_server) -> None:
    server, stub = geocode_server
    stub.rows = [{"label": "Manching", "lat": 48.7, "lon": 11.5,
                  "bounds": None, "kind": "town"}]
    status, data = _get(server, "/api/geocode?q=manching")
    assert status == 200
    assert data["ok"] is True
    assert data["results"][0]["label"] == "Manching"
    assert stub.calls[0][0] == "manching"


def test_endpoint_passes_the_map_centre_through_as_a_bias(geocode_server) -> None:
    server, stub = geocode_server
    _get(server, "/api/geocode?q=bremen&near=11.5,48.7&limit=3")
    assert stub.calls[0][1] == 3
    assert stub.calls[0][2] == (11.5, 48.7)


@pytest.mark.parametrize("near", ["junk", "11.5", "999,48.7", "11.5,91", "nan,nan", ""])
def test_an_unusable_centre_is_dropped_rather_than_failing_the_search(
    geocode_server, near: str,
) -> None:
    server, stub = geocode_server
    status, data = _get(server, f"/api/geocode?q=bremen&near={near}")
    assert status == 200 and data["ok"] is True
    assert stub.calls[0][2] is None


def test_a_search_that_cannot_run_is_a_sentence_not_a_status_code(geocode_server) -> None:
    server, stub = geocode_server
    stub.error = "No connection to the place-search service."
    status, data = _get(server, "/api/geocode?q=manching")
    # 200 on purpose: "found nothing" and "could not look" are both ordinary
    # in the field and the box says something different for each.
    assert status == 200
    assert data["ok"] is False and data["results"] == []
    assert "connection" in data["error"].lower()


def test_a_missing_query_is_the_one_400(geocode_server) -> None:
    server, _ = geocode_server
    status, data = _get(server, "/api/geocode?q=%20%20")
    assert status == 400 and data["ok"] is False


def test_a_service_that_was_never_built_says_so_without_500(geocode_server) -> None:
    server, _ = geocode_server
    saved = CorvusHandler.geocoder
    CorvusHandler.geocoder = None
    try:
        status, data = _get(server, "/api/geocode?q=manching")
    finally:
        CorvusHandler.geocoder = saved
    assert status == 200
    assert data["ok"] is False and data["results"] == []


def test_an_exploding_geocoder_never_500s_the_app(geocode_server) -> None:
    server, stub = geocode_server

    class Boom:
        def search(self, *a, **kw):  # noqa: ANN001, ANN002, ANN003
            raise RuntimeError("upstream went sideways")

    saved = CorvusHandler.geocoder
    CorvusHandler.geocoder = Boom()
    try:
        status, data = _get(server, "/api/geocode?q=manching")
    finally:
        CorvusHandler.geocoder = saved
    assert status == 200
    assert data["ok"] is False and data["results"] == []


def test_the_two_coordinate_orders_are_named_not_remembered() -> None:
    """Both conventions live in this module; the types say which is which.

    A search bias is longitude-first (the viewbox the query carries), a
    result is latitude-first (the way Nominatim sends it). They used to be
    the same anonymous ``tuple[float, float]``.
    """
    from corvus.geocode import Near, Point, build_query

    near = Near(11.5, 48.8)          # Bavaria: lon 11.5, lat 48.8
    assert near.lon == 11.5 and near.lat == 48.8
    assert Point(48.8, 11.5).lat == 48.8

    # A plain tuple in the old order still means the same thing, so nothing
    # that passed one had to change.
    assert build_query("x", 5, (11.5, 48.8)) == build_query("x", 5, near)

    # And the viewbox really is built around the longitude, not the latitude:
    # west/east straddle 11.5, north/south straddle 48.8.
    query = build_query("x", 5, near)
    viewbox = query.split("viewbox=")[1].split("&")[0].replace("%2C", ",")
    west, north, east, south = (float(v) for v in viewbox.split(","))
    assert west < 11.5 < east
    assert south < 48.8 < north


def test_the_http_layer_hands_down_a_named_near() -> None:
    """``?near=<lon>,<lat>`` is parsed into the type, not a bare pair."""
    pytest.importorskip("pymavlink")
    from corvus.geocode import Near
    from corvus.server import _parse_near

    parsed = _parse_near("11.5,48.8")
    assert isinstance(parsed, Near)
    assert (parsed.lon, parsed.lat) == (11.5, 48.8)
    assert _parse_near("nonsense") is None
