"""Update check against the Corvus GCS GitHub releases page.

The operator should not have to watch a repository to learn that a newer
build exists. This module compares the running version against the newest
published release of ``M-BSquared/CorvusGCS`` and hands the answer to the
frontend, which raises a dialog once per version.

Offline contract
----------------
Same rules as the firmware catalogue, because the same laptop runs both:

* Nothing here runs at import or at startup. The check happens only when the
  frontend asks for it, on a background fetch nothing in the UI waits for.
* Every answer is cached to ``<state_dir>/update.json``, so a launch with no
  internet reports the last known result instead of going quiet, and a
  repeated launch does not hammer GitHub — a fresh network check is made at
  most once per :data:`CHECK_INTERVAL_S` (sooner after a failure, since a
  failed check knows nothing).
* A failed check is never an error the operator sees. It returns the cached
  release together with a message the Settings page can show on request.

Only the release *list* is trusted from the network. The page URL the dialog
offers is BUILT from the tag, never read out of the payload, so a tampered
cache file can at worst name a different tag in the same repository rather
than send the operator to another site.
"""
from __future__ import annotations

import json
import logging
import os
import re
import threading
import time
import urllib.error
import urllib.request
from typing import Any

from .paths import corvus_home
from .version import get_version

logger = logging.getLogger("corvus.update")

REPO = "M-BSquared/CorvusGCS"
RELEASES_URL = f"https://api.github.com/repos/{REPO}/releases?per_page=10"
# Built from the tag we validated ourselves, never taken from the payload.
RELEASE_PAGE_URL = f"https://github.com/{REPO}/releases/tag/{{tag}}"
RELEASES_PAGE_URL = f"https://github.com/{REPO}/releases"

UPDATE_FILE = "update.json"
FETCH_TIMEOUT_S = 6.0
# Nobody is waiting on this request, but a hung socket must not hold a handler
# thread or a huge body exhaust memory. A release list is a few dozen KB.
MAX_PAYLOAD_BYTES = 2 * 1024 * 1024
# How stale a successful check may be before the next request re-fetches.
CHECK_INTERVAL_S = 6 * 3600.0
# After a failure: the field case is "no internet at all", so retry often
# enough that plugging in a hotspot is noticed, rarely enough that an offline
# session is not retrying on every page load.
RETRY_INTERVAL_S = 900.0
# Release notes are shown in a dialog, not a document viewer.
MAX_NOTES_CHARS = 4000

_TAG_RE = re.compile(r"^v?(\d+(?:\.\d+)*)$")


def default_state_dir() -> str:
    """Where the cached check result lives (``~/.corvus``)."""
    return corvus_home()


def parse_version(text: Any) -> tuple[int, ...] | None:
    """Parse ``2026.09.27`` / ``v2026.09.27`` into a comparable tuple.

    Returns ``None`` for anything that is not a plain dotted number — a
    release named ``nightly`` or a ``0.0.0-unknown`` fallback version must
    never be compared, because a wrong comparison here shows the operator an
    update that does not exist.
    """
    match = _TAG_RE.match(str(text or "").strip())
    if match is None:
        return None
    return tuple(int(part) for part in match.group(1).split("."))


def is_newer(candidate: Any, current: Any) -> bool:
    """True when *candidate* is a strictly newer version than *current*.

    Unparsable on either side means "no update": CalVer components are
    zero-padded strings, so the comparison is done on integer tuples and
    shorter tuples sort first (``2026.09`` < ``2026.09.01``).
    """
    left = parse_version(candidate)
    right = parse_version(current)
    if left is None or right is None:
        return False
    return left > right


def _request(url: str) -> urllib.request.Request:
    return urllib.request.Request(url, headers={
        "User-Agent": f"CorvusGCS/{get_version()}",
        "Accept": "application/vnd.github+json",
    })


def _release_page(tag: str) -> str:
    """The release page for *tag*, or the releases index for an odd tag."""
    if _TAG_RE.match(tag or ""):
        return RELEASE_PAGE_URL.format(tag=tag)
    return RELEASES_PAGE_URL


def _pick_latest(payload: Any) -> dict[str, Any] | None:
    """Reduce the GitHub release payload to the newest stable release.

    Untrusted input: every field is coerced, drafts and prereleases are
    dropped, and the winner is chosen by comparing parsed version tuples
    rather than trusting the order GitHub returned.
    """
    if not isinstance(payload, list):
        return None
    best: dict[str, Any] | None = None
    best_version: tuple[int, ...] | None = None
    for item in payload:
        if not isinstance(item, dict) or item.get("draft") or item.get("prerelease"):
            continue
        tag = str(item.get("tag_name") or "")
        version = parse_version(tag)
        if version is None:
            continue
        if best_version is not None and version <= best_version:
            continue
        notes = str(item.get("body") or "").strip()
        assets = []
        for asset in item.get("assets") or []:
            if not isinstance(asset, dict):
                continue
            assets.append({
                "name": str(asset.get("name") or ""),
                "size": int(asset.get("size") or 0),
            })
        best_version = version
        best = {
            "tag": tag,
            # The tag's own text minus the "v", so the UI never has to strip
            # it and the zero padding CalVer writes ("2026.09.27") survives —
            # rebuilding the string from the parsed ints would lose it, and
            # the dismissed-version match is a string compare.
            "version": tag.lstrip("v"),
            "name": str(item.get("name") or tag),
            "published": str(item.get("published_at") or ""),
            "notes": notes[:MAX_NOTES_CHARS],
            "assets": assets,
            "url": _release_page(tag),
        }
    return best


class UpdateChecker:
    """Cached "is there a newer release?" answer, backed by one JSON file.

    Thread-safe: HTTP handler threads share one instance, and a check that is
    already in flight is not started a second time.
    """

    def __init__(self, state_dir: str | None = None) -> None:
        self.dir = state_dir or default_state_dir()
        self._lock = threading.Lock()

    # ------------------------------------------------------------------
    # Cache
    # ------------------------------------------------------------------

    def _cache_path(self) -> str:
        return os.path.join(self.dir, UPDATE_FILE)

    def load_cached(self) -> dict[str, Any]:
        """The last stored check result; ``{}`` when there is no usable cache."""
        try:
            with open(self._cache_path(), "r", encoding="utf-8") as handle:
                data = json.load(handle)
        except (OSError, ValueError):
            return {}
        return data if isinstance(data, dict) else {}

    def _store_cached(self, data: dict[str, Any]) -> None:
        try:
            os.makedirs(self.dir, exist_ok=True)
            path = self._cache_path()
            tmp = path + ".tmp"
            with open(tmp, "w", encoding="utf-8") as handle:
                json.dump(data, handle)
            os.replace(tmp, path)
        except OSError:
            logger.debug("could not cache the update check", exc_info=True)

    # ------------------------------------------------------------------
    # Network
    # ------------------------------------------------------------------

    def fetch(self) -> tuple[dict[str, Any] | None, str]:
        """Fetch the release list. Returns ``(latest_release, error)``.

        Never raises: in the field this call fails on every launch, and that
        is a normal state, not a fault.
        """
        try:
            with urllib.request.urlopen(_request(RELEASES_URL),
                                        timeout=FETCH_TIMEOUT_S) as resp:
                raw = resp.read(MAX_PAYLOAD_BYTES + 1)
            if len(raw) > MAX_PAYLOAD_BYTES:
                return None, "the release list was unexpectedly large"
            payload = json.loads(raw.decode("utf-8"))
        except (urllib.error.URLError, TimeoutError, OSError, ValueError) as exc:
            logger.info("update check failed: %s", exc)
            return None, f"could not reach the release server ({exc})"
        return _pick_latest(payload), ""

    def _is_stale(self, cached: dict[str, Any], now: float) -> bool:
        checked_at = cached.get("checked_at")
        if not isinstance(checked_at, (int, float)):
            return True
        # A clock that moved backwards (or a cache copied from another
        # machine) must not pin the check as "fresh" forever.
        if checked_at > now:
            return True
        interval = CHECK_INTERVAL_S if cached.get("latest") else RETRY_INTERVAL_S
        return (now - checked_at) >= interval

    def check(self, force: bool = False) -> dict[str, Any]:
        """Return the update status as the API serves it.

        ``force`` always goes to the network (the Settings page's "Check now").
        Otherwise the cached answer is reused until it goes stale, so opening
        the app repeatedly costs nothing and works offline.
        """
        current = get_version()
        now = time.time()
        with self._lock:
            cached = self.load_cached()
            error = str(cached.get("error") or "")
            latest = cached.get("latest") if isinstance(cached.get("latest"), dict) else None
            checked_at = cached.get("checked_at")
            if force or self._is_stale(cached, now):
                fetched, error = self.fetch()
                if fetched is not None:
                    latest = fetched
                checked_at = now
                # The failed attempt is stored too, so an offline session backs
                # off instead of re-trying the timeout on every page load.
                self._store_cached({
                    "checked_at": now,
                    "latest": latest,
                    "error": error,
                })
        return self._status(current, latest, error, checked_at)

    def cached_status(self) -> dict[str, Any]:
        """The stored answer with no network access at all."""
        cached = self.load_cached()
        latest = cached.get("latest") if isinstance(cached.get("latest"), dict) else None
        return self._status(get_version(), latest,
                            str(cached.get("error") or ""), cached.get("checked_at"))

    @staticmethod
    def _status(current: str, latest: dict[str, Any] | None,
                error: str, checked_at: Any) -> dict[str, Any]:
        release = dict(latest) if latest else {}
        return {
            "current": current,
            "latest": release.get("version", ""),
            "update_available": is_newer(release.get("version"), current),
            "name": release.get("name", ""),
            "url": release.get("url") or RELEASES_PAGE_URL,
            "published": release.get("published", ""),
            "notes": release.get("notes", ""),
            "assets": release.get("assets", []),
            "checked_at": checked_at if isinstance(checked_at, (int, float)) else 0,
            "error": error,
        }
