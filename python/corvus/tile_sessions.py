"""Licensed tile APIs: turning an operator's key into a tile URL template.

MapTiler and Mapbox put the key straight into an XYZ template. Google's Map
Tiles API and Bing's REST API do not work that way: Google wants a session
created first (``createSession``), and Bing hands out the tile URL in an
imagery metadata answer. This module makes that one call, turns the answer
into an ordinary template for :func:`corvus.tile_sources.build_tile_url`, and
keeps it until it runs out, so a pan costs one call per layer rather than one
per tile.

The template it returns still carries ``{k}`` where the API wants the key on
every tile (Google), so the key is substituted at fetch time like any other
and :func:`corvus.tile_sources.redact_url` keeps it out of the logs. The
session value is percent-encoded into the template, so nothing the upstream
sends back can add URL structure.

No threads and no open sockets: every call is a ``urlopen`` with a timeout,
made on the thread that asked. Nothing to tear down on shutdown.

stdlib only.
"""
from __future__ import annotations

import json
import logging
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Any

from . import tile_sources
from .version import get_version

logger = logging.getLogger(__name__)

_TIMEOUT_S = 10.0
_MAX_BODY = 256 * 1024
# How long a failed resolve is remembered, so a pan over a bad key (or with no
# network) does not repeat the call once per tile.
_RETRY_AFTER_REFUSED_S = 60.0
_RETRY_AFTER_OFFLINE_S = 15.0
# A session is dropped this long before the upstream says it ends, so no tile
# is ever requested with one that expired in flight.
_EXPIRY_MARGIN_S = 600.0
# Bing's metadata answer carries no expiry; the tile URL in it stays valid for
# far longer than this, which only bounds how stale its imagery version gets.
_BING_TTL_S = 12 * 3600.0
_GOOGLE_DEFAULT_TTL_S = 12 * 3600.0
_BING_HOST_SUFFIXES = (".virtualearth.net", ".bing.com")


class TileSessionError(Exception):
    """A licensed API would not hand out a tile template.

    ``offline`` separates "could not reach it" (counts against the shared
    upstream breaker, like any failed fetch) from "it answered no" (a wrong
    key or an API not enabled, which says nothing about the network).
    """

    def __init__(self, message: str, *, offline: bool) -> None:
        super().__init__(message)
        self.offline = offline


_lock = threading.Lock()
# (source_id, key) -> (template or None, valid_until, error or None)
_cache: dict[tuple[str, str], tuple[str | None, float, TileSessionError | None]] = {}
_key_locks: dict[tuple[str, str], threading.Lock] = {}


def resolve(source_id: str, token: str) -> str:
    """A tile template for *source_id* on the licensed API, using *token*.

    Cached per source and key until the upstream's session runs out. One
    caller per source and key makes the call; the rest wait for its answer
    rather than each opening a session of their own.

    Raises :class:`TileSessionError` when the API refuses or cannot be reached,
    and ``ValueError`` when *source_id* has no licensed API.
    """
    block = tile_sources.licensed(source_id)
    if block is None:
        raise ValueError(f"{source_id!r} has no licensed API")
    cache_key = (source_id, token)
    with _lock:
        key_lock = _key_locks.setdefault(cache_key, threading.Lock())
    with key_lock:
        now = time.monotonic()
        with _lock:
            hit = _cache.get(cache_key)
        if hit is not None and now < hit[1]:
            template, _until, error = hit
            if template is not None:
                return template
            assert error is not None
            raise error
        try:
            template, ttl = _fetch(block, token)
        except TileSessionError as exc:
            retry = _RETRY_AFTER_OFFLINE_S if exc.offline else _RETRY_AFTER_REFUSED_S
            with _lock:
                _cache[cache_key] = (None, now + retry, exc)
            raise
        with _lock:
            _cache[cache_key] = (template, now + ttl, None)
        return template


def invalidate(source_id: str, token: str) -> None:
    """Forget the template for *source_id* and *token*.

    Called when a tile made from it is refused, so the next one opens a fresh
    session instead of repeating the refusal until the old one would have
    expired.
    """
    with _lock:
        _cache.pop((source_id, token), None)


def clear() -> None:
    """Forget every template. A replaced key needs none of this, since the
    cache is keyed by the key itself; it exists for tests."""
    with _lock:
        _cache.clear()


def _fetch(block: dict[str, Any], token: str) -> tuple[str, float]:
    api = block.get("api")
    if api == "google_tiles":
        return _google_session(block, token)
    if api == "bing_rest":
        return _bing_metadata(block, token)
    raise TileSessionError(f"unknown licensed API {api!r}", offline=False)


def _request_json(url: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
    data = json.dumps(body).encode("utf-8") if body is not None else None
    headers = {"User-Agent": f"CorvusGCS/{get_version()}"}
    if data is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers,
                                 method="POST" if data is not None else "GET")
    try:
        with urllib.request.urlopen(req, timeout=_TIMEOUT_S) as resp:
            raw = resp.read(_MAX_BODY + 1)
    except urllib.error.HTTPError as exc:
        # Redacted: the URL carries the key, and this message is logged.
        logger.info("licensed tile API refused (%s): %s",
                    exc.code, tile_sources.redact_url(url))
        raise TileSessionError(
            f"the map service refused the key (HTTP {exc.code})", offline=False) from None
    except (urllib.error.URLError, TimeoutError, OSError):
        raise TileSessionError("the map service could not be reached", offline=True) from None
    if len(raw) > _MAX_BODY:
        raise TileSessionError("the map service answer was too large", offline=False)
    try:
        parsed = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        raise TileSessionError("the map service answer was not JSON", offline=False) from None
    if not isinstance(parsed, dict):
        raise TileSessionError("the map service answer was not an object", offline=False)
    return parsed


def _google_session(block: dict[str, Any], token: str) -> tuple[str, float]:
    """Open a Map Tiles API session for one layer.

    Terrain needs the roadmap layer on top (the API refuses it alone); hybrid
    is satellite with that same layer, which is what the registry says.
    """
    body: dict[str, Any] = {
        "mapType": block["map_type"],
        "language": "en-US",
        "region": "US",
    }
    if block.get("layer_types"):
        body["layerTypes"] = list(block["layer_types"])
    url = tile_sources.build_tile_url(tile_sources.GOOGLE_TILES_SESSION_URL, 0, 0, 0, token)
    answer = _request_json(url, body)
    session = answer.get("session")
    if not isinstance(session, str) or not session:
        raise TileSessionError("the map service opened no session", offline=False)
    ttl = _GOOGLE_DEFAULT_TTL_S
    try:
        expiry = float(answer.get("expiry"))
    except (TypeError, ValueError):
        expiry = 0.0
    if expiry > 0:
        ttl = max(0.0, min(ttl, expiry - time.time() - _EXPIRY_MARGIN_S))
        if ttl <= 0:
            raise TileSessionError("the map service opened an expired session",
                                   offline=False)
    template = tile_sources.GOOGLE_TILES_URL.replace(
        "{session}", urllib.parse.quote(session, safe=""))
    return template, ttl


def _bing_metadata(block: dict[str, Any], token: str) -> tuple[str, float]:
    """Read the tile URL for one imagery set from Bing's metadata API.

    The answer's ``imageUrl`` uses Bing's own placeholders; they are mapped
    onto this project's ``{s}`` and ``{q}`` so the template is filled the same
    way as every other. Anything that is not an https URL on a Microsoft host,
    or that carries a placeholder this code does not know, is refused rather
    than fetched.
    """
    url = tile_sources.build_tile_url(
        tile_sources.BING_METADATA_URL.replace(
            "{imagery_set}", urllib.parse.quote(block["imagery_set"], safe="")),
        0, 0, 0, token)
    answer = _request_json(url)
    try:
        resource = answer["resourceSets"][0]["resources"][0]
        image_url = resource["imageUrl"]
        subdomains = resource.get("imageUrlSubdomains") or []
    except (KeyError, IndexError, TypeError):
        raise TileSessionError("the map service sent no tile address", offline=False) from None
    if not isinstance(image_url, str) or not isinstance(subdomains, list):
        raise TileSessionError("the map service sent no tile address", offline=False)

    if subdomains == [f"t{i}" for i in range(4)]:
        subdomain = "t{s}"
    elif subdomains and isinstance(subdomains[0], str) and subdomains[0].isalnum():
        subdomain = subdomains[0]
    elif "{subdomain}" in image_url:
        raise TileSessionError("the map service sent no tile servers", offline=False)
    else:
        subdomain = ""
    template = (image_url
                .replace("{subdomain}", subdomain)
                .replace("{quadkey}", "{q}")
                .replace("{culture}", "en-US"))

    parts = urllib.parse.urlsplit(template.replace("{s}", "0").replace("{q}", "0"))
    host = parts.hostname or ""
    if (parts.scheme != "https"
            or not host.endswith(_BING_HOST_SUFFIXES)
            or "{" in parts.geturl() or "}" in parts.geturl()
            or "{q}" not in template):
        raise TileSessionError("the map service sent a tile address Corvus will not use",
                               offline=False)
    return template, _BING_TTL_S
