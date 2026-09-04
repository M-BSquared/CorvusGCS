"""PX4 firmware release catalogue and image cache.

Picking a firmware file used to be the operator's problem: find the PX4
release, work out which of the ~40 build targets matches the flight
controller, download the ``.px4``, remember where it landed. This module moves
that into the app — the operator picks a release and a board, and the backend
resolves, downloads and caches the image.

Offline contract
----------------
The field laptop has no internet, and the app must never *need* it. So:

* Nothing here runs at startup. The catalogue is fetched only when the operator
  opens the Firmware page and asks for it.
* Every fetch is written to ``<firmware_dir>/catalog.json`` and every image to
  ``<firmware_dir>/<asset name>``. With no network the catalogue is served from
  that cache and already-downloaded images still flash.
* Downloads are server-side, like map tiles: the browser never talks to
  GitHub. That keeps the one network egress point in the backend, where it is
  bounded by a timeout and a size cap.

Only the release list is trusted from the network. The flash request names a
release and a board, never a URL, and the download target is re-derived here
and checked against :data:`ALLOWED_HOSTS` — so a poisoned cache file cannot
turn a flash into a fetch from somewhere else.
"""
from __future__ import annotations

import json
import logging
import os
import re
import threading
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Callable

from .version import get_version

logger = logging.getLogger("corvus.firmware")

RELEASES_URL = "https://api.github.com/repos/PX4/PX4-Autopilot/releases"
# Download URLs are BUILT from this template, never read out of the catalogue.
# The catalogue is network data cached to disk; deriving the URL from a tag and
# an asset name we validated ourselves means a tampered cache file can at worst
# name a different asset in the same repo, not a different server.
ASSET_URL = "https://github.com/PX4/PX4-Autopilot/releases/download/{tag}/{name}"
# The only hosts a firmware image may be fetched from. Checked on the resolved
# download URL, so a tampered catalogue cache cannot redirect the fetch.
ALLOWED_HOSTS = frozenset({"github.com", "objects.githubusercontent.com",
                           "api.github.com", "release-assets.githubusercontent.com"})
CATALOG_FILE = "catalog.json"
# Releases kept in the catalogue. PX4 has years of tags; the operator wants the
# current one and a couple to roll back to, not the archive.
MAX_RELEASES = 8
FETCH_TIMEOUT_S = 20.0
# A PX4 image is a few MB. The cap stops a wrong URL from filling the disk.
MAX_IMAGE_BYTES = 64 * 1024 * 1024
_CHUNK = 64 * 1024

# Assets that are not flight-controller firmware. A bootloader image flashed
# through the firmware uploader bricks the board until it is recovered over
# SWD, so these never reach the operator's board list.
NON_FIRMWARE_SUFFIXES = ("_bootloader.px4", "_canbootloader.px4", "_cannode.px4")

# Friendly names for the common build targets. Anything not listed falls back
# to the raw asset name, so a new board is never hidden from the operator just
# because this table has not caught up.
BOARD_LABELS: dict[str, str] = {
    "px4_fmu-v2_default": "Pixhawk 1 (FMUv2)",
    "px4_fmu-v3_default": "Pixhawk 2 / Cube (FMUv3)",
    "px4_fmu-v4_default": "Pixracer (FMUv4)",
    "px4_fmu-v4pro_default": "Pixhawk 3 Pro (FMUv4-pro)",
    "px4_fmu-v5_default": "Pixhawk 4 (FMUv5)",
    "px4_fmu-v5x_default": "Pixhawk 5X (FMUv5X)",
    "px4_fmu-v6c_default": "Pixhawk 6C (FMUv6C)",
    "px4_fmu-v6u_default": "Pixhawk 6C mini (FMUv6U)",
    "px4_fmu-v6x_default": "Pixhawk 6X (FMUv6X)",
    "px4_fmu-v6xrt_default": "Pixhawk 6X-RT (FMUv6X-RT)",
    "cubepilot_cubeorange_default": "Cube Orange",
    "cubepilot_cubeorangeplus_default": "Cube Orange+",
    "cubepilot_cubeyellow_default": "Cube Yellow",
    "holybro_kakuteh7_default": "Holybro Kakute H7",
    "holybro_kakuteh7v2_default": "Holybro Kakute H7 v2",
    "modalai_fc-v2_default": "ModalAI Flight Core v2",
    "ark_fmu-v6x_default": "ARK FMU v6X",
    "matek_h743_default": "Matek H743",
    "matek_h743-slim_default": "Matek H743 Slim",
}


# --------------------------------------------------------------------------
# Board detection
# --------------------------------------------------------------------------
# PX4 builds its USB product string from the board, so the descriptor the OS
# already has is the most direct answer to "what is plugged in". These patterns
# turn it into the token a build target is named after; the token is then
# matched against the release's OWN target list, so a board PX4 added after
# this table was written is still found as long as its descriptor names it.
_FMU_RE = re.compile(r"fmu[\s_-]*v?(\d+[a-z]*)", re.I)
_DESCRIPTOR_TOKENS: tuple[tuple[str, str], ...] = (
    # (substring in the USB descriptor, token that appears in the target name)
    ("cubeorangeplus", "cubeorangeplus"),
    ("cube orange+", "cubeorangeplus"),
    ("cubeorange", "cubeorange"),
    ("cube orange", "cubeorange"),
    ("cubeyellow", "cubeyellow"),
    ("cubeblack", "cubeblack"),
    ("kakuteh7v2", "kakuteh7v2"),
    ("kakuteh7", "kakuteh7"),
    ("pixracer", "fmu-v4"),
    ("durandal", "durandal"),
    ("matek", "matek"),
    ("modalai", "modalai"),
)
# USB VID:PID of boards whose descriptor is too generic to identify them.
# Deliberately short: a wrong entry here silently proposes the wrong firmware,
# and the descriptor route covers the overwhelming majority.
_USB_IDS: dict[tuple[int, int], str] = {
    (0x26AC, 0x0011): "fmu-v2",     # 3DR Pixhawk 1
    (0x26AC, 0x0016): "fmu-v4",     # 3DR Pixracer
}


def _hex_ids(hwid: str) -> tuple[int, int] | None:
    """Pull VID:PID out of a pyserial hwid string ("USB VID:PID=26AC:0011 …")."""
    match = re.search(r"VID:PID=([0-9A-Fa-f]{4}):([0-9A-Fa-f]{4})", str(hwid or ""))
    if not match:
        return None
    return int(match.group(1), 16), int(match.group(2), 16)


def detect_board(hints: dict[str, Any], boards: list[dict[str, Any]]) -> dict[str, Any] | None:
    """Guess which of *boards* is plugged in, from USB/MAVLink identity hints.

    `hints` carries whatever is known: the serial port ``description`` and
    ``hwid`` (USB product string and VID:PID), and the ``vendor_id`` /
    ``product_id`` PX4 reports in AUTOPILOT_VERSION.

    Returns ``{name, label, token, source}`` for the matched board, or None. It
    is a *suggestion*: the caller preselects it and the operator can always
    override, because flashing the wrong image is worse than picking from a
    list. Never raises.
    """
    if not boards:
        return None
    description = str(hints.get("description") or "")
    hwid = str(hints.get("hwid") or "")
    haystack = f"{description} {hwid}".lower().replace("_", "").replace(" ", "")

    def _match(token: str, source: str) -> dict[str, Any] | None:
        """First board whose target name contains *token*, preferring _default."""
        needle = token.lower()
        candidates = [b for b in boards if needle in str(b.get("name", "")).lower()]
        if not candidates:
            return None
        candidates.sort(key=lambda b: (0 if "_default." in str(b.get("name")) else 1,
                                       len(str(b.get("name")))))
        best = candidates[0]
        return {"name": best.get("name"), "label": best.get("label"),
                "token": token, "source": source}

    # 1. An explicit VID:PID, for the descriptors that say nothing useful.
    ids = _hex_ids(hwid)
    if ids is None:
        vendor = hints.get("vendor_id")
        product = hints.get("product_id")
        if isinstance(vendor, int) and isinstance(product, int) and vendor and product:
            ids = (vendor, product)
    if ids is not None and ids in _USB_IDS:
        found = _match(_USB_IDS[ids], "USB device id")
        if found:
            return found

    # 2. A board name in the descriptor.
    for needle, token in _DESCRIPTOR_TOKENS:
        if needle.replace(" ", "") in haystack:
            found = _match(token, "USB descriptor")
            if found:
                return found

    # 3. An FMU generation in the descriptor ("PX4 FMU v6X.x" -> fmu-v6x).
    fmu = _FMU_RE.search(f"{description} {hwid}")
    if fmu:
        found = _match(f"fmu-v{fmu.group(1).lower()}", "USB descriptor")
        if found:
            return found
    return None


def default_firmware_dir() -> str:
    """Where downloaded images and the cached catalogue live."""
    return os.path.expanduser("~/.corvus/firmware")


def board_label(asset_name: str) -> str:
    """Operator-facing name for a ``<target>.px4`` asset."""
    stem = asset_name[:-4] if asset_name.endswith(".px4") else asset_name
    return BOARD_LABELS.get(stem, stem)


def is_flashable_asset(name: str) -> bool:
    """True for a `.px4` image that is flight-controller firmware."""
    return name.endswith(".px4") and not name.endswith(NON_FIRMWARE_SUFFIXES)


def asset_url(tag: str, name: str) -> str:
    """The download URL for one release asset, built from the fixed template."""
    return ASSET_URL.format(
        tag=urllib.parse.quote(str(tag), safe=""),
        name=urllib.parse.quote(str(name), safe=""),
    )


def _safe_name(name: str) -> str:
    """A filename that cannot escape the cache directory."""
    return re.sub(r"[^A-Za-z0-9._-]+", "_", os.path.basename(str(name or "")))[:120]


def _host_allowed(url: str) -> bool:
    try:
        host = (urllib.parse.urlparse(url).hostname or "").lower()
    except ValueError:
        return False
    return host in ALLOWED_HOSTS


def _request(url: str) -> urllib.request.Request:
    return urllib.request.Request(url, headers={
        "User-Agent": f"CorvusGCS/{get_version()}",
        "Accept": "application/vnd.github+json",
    })


class FirmwareCatalog:
    """Release list + image cache, backed by a directory on disk.

    Thread-safe for the one thing that matters: a download runs on the flash
    worker thread while HTTP handlers read the catalogue.
    """

    def __init__(self, firmware_dir: str | None = None) -> None:
        self.dir = firmware_dir or default_firmware_dir()
        self._lock = threading.Lock()

    # ------------------------------------------------------------------
    # Catalogue
    # ------------------------------------------------------------------

    def _catalog_path(self) -> str:
        return os.path.join(self.dir, CATALOG_FILE)

    def load_cached(self) -> list[dict[str, Any]]:
        """Releases from the on-disk cache; [] when there is no usable cache."""
        try:
            with open(self._catalog_path(), "r", encoding="utf-8") as handle:
                data = json.load(handle)
        except (OSError, ValueError):
            return []
        releases = data.get("releases") if isinstance(data, dict) else None
        return releases if isinstance(releases, list) else []

    def _store_cached(self, releases: list[dict[str, Any]]) -> None:
        try:
            os.makedirs(self.dir, exist_ok=True)
            path = self._catalog_path()
            tmp = path + ".tmp"
            with open(tmp, "w", encoding="utf-8") as handle:
                json.dump({"releases": releases}, handle)
            os.replace(tmp, path)
        except OSError:
            logger.debug("could not cache the firmware catalogue", exc_info=True)

    def fetch(self) -> tuple[list[dict[str, Any]], str]:
        """Fetch the release list from GitHub. Returns ``(releases, error)``.

        On any network failure the cached copy is returned together with the
        error, so the page can say "showing what is cached" rather than going
        blank — which is the normal state in the field.
        """
        try:
            with urllib.request.urlopen(_request(RELEASES_URL), timeout=FETCH_TIMEOUT_S) as resp:
                payload = json.loads(resp.read().decode("utf-8"))
        except (urllib.error.URLError, TimeoutError, OSError, ValueError) as exc:
            logger.info("firmware catalogue fetch failed: %s", exc)
            return self.load_cached(), f"could not reach the PX4 release server ({exc})"
        releases = _parse_releases(payload)
        if releases:
            self._store_cached(releases)
        return releases, ""

    def catalog(self, refresh: bool = False) -> dict[str, Any]:
        """The catalogue as the API serves it.

        ``refresh`` forces a network fetch; without it the cache is used when it
        has anything, so opening the page offline is instant and silent.
        """
        error = ""
        releases = [] if refresh else self.load_cached()
        if refresh or not releases:
            releases, error = self.fetch()
        cached = self.list_cached_images()
        cached_names = {entry["name"] for entry in cached}
        for release in releases:
            for board in release.get("boards", []):
                board["cached"] = board.get("name") in cached_names
        return {
            "releases": releases,
            "cached": cached,
            "error": error,
            "dir": self.dir,
        }

    # ------------------------------------------------------------------
    # Images
    # ------------------------------------------------------------------

    def list_cached_images(self) -> list[dict[str, Any]]:
        """Images already on disk — what can be flashed with no network."""
        out: list[dict[str, Any]] = []
        try:
            names = sorted(os.listdir(self.dir))
        except OSError:
            return out
        for name in names:
            if not name.endswith(".px4"):
                continue
            path = os.path.join(self.dir, name)
            try:
                size = os.path.getsize(path)
            except OSError:
                continue
            out.append({"name": name, "size": size, "label": board_label(name)})
        return out

    def resolve(self, release_tag: str, board_name: str) -> dict[str, Any] | None:
        """Find one board's asset in the catalogue. None when it is not there.

        The flash request names a release and a board; the URL is looked up
        here rather than accepted from the client.
        """
        for release in self.load_cached():
            if str(release.get("tag")) != str(release_tag):
                continue
            for board in release.get("boards", []):
                name = str(board.get("name"))
                if name != str(board_name) or not is_flashable_asset(name):
                    continue
                tag = str(release.get("tag") or "")
                return dict(board, tag=tag, url=asset_url(tag, name))
        return None

    def cached_path(self, name: str) -> str | None:
        """Path of an already-downloaded image, or None."""
        path = os.path.join(self.dir, _safe_name(name))
        return path if os.path.isfile(path) else None

    def download(
        self,
        entry: dict[str, Any],
        on_progress: Callable[[int, int], None] | None = None,
        cancel: threading.Event | None = None,
    ) -> bytes:
        """Download one image, cache it, and return its bytes.

        Raises ``ValueError`` with an operator-readable message on refusal or
        failure — the caller turns that into the flash status message.
        """
        name = _safe_name(entry.get("name", ""))
        if not name.endswith(".px4"):
            raise ValueError("not a PX4 firmware image")
        url = str(entry.get("url", ""))
        if not _host_allowed(url):
            raise ValueError("firmware download refused: unexpected host")

        cached = self.cached_path(name)
        if cached:
            try:
                with open(cached, "rb") as handle:
                    return handle.read()
            except OSError:
                pass   # fall through and re-download

        expected = int(entry.get("size") or 0)
        buf = bytearray()
        try:
            with urllib.request.urlopen(_request(url), timeout=FETCH_TIMEOUT_S) as resp:
                total = expected or int(resp.headers.get("Content-Length") or 0)
                while True:
                    if cancel is not None and cancel.is_set():
                        raise ValueError("download cancelled")
                    chunk = resp.read(_CHUNK)
                    if not chunk:
                        break
                    buf.extend(chunk)
                    if len(buf) > MAX_IMAGE_BYTES:
                        raise ValueError("firmware image is implausibly large")
                    if on_progress is not None:
                        on_progress(len(buf), total)
        except ValueError:
            raise
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            raise ValueError(f"firmware download failed ({exc})") from exc
        if not buf:
            raise ValueError("firmware download returned an empty file")

        with self._lock:
            try:
                os.makedirs(self.dir, exist_ok=True)
                path = os.path.join(self.dir, name)
                tmp = path + ".part"
                with open(tmp, "wb") as handle:
                    handle.write(buf)
                os.replace(tmp, path)
            except OSError:
                # A full or read-only disk must not fail the flash — the bytes
                # are already in hand; caching is the optimisation, not the job.
                logger.info("could not cache %s", name, exc_info=True)
        return bytes(buf)


def _parse_releases(payload: Any) -> list[dict[str, Any]]:
    """Reduce the GitHub release payload to what the UI needs.

    Untrusted input: every field is coerced, and only ``.px4`` assets on an
    allowed host survive.
    """
    if not isinstance(payload, list):
        return []
    out: list[dict[str, Any]] = []
    for item in payload:
        if not isinstance(item, dict) or item.get("draft"):
            continue
        boards = []
        for asset in item.get("assets") or []:
            if not isinstance(asset, dict):
                continue
            name = str(asset.get("name") or "")
            url = str(asset.get("browser_download_url") or "")
            if not is_flashable_asset(name) or not _host_allowed(url):
                continue
            boards.append({
                "name": name,
                "label": board_label(name),
                "size": int(asset.get("size") or 0),
                # No URL: it is derived from tag + name at flash time.
            })
        if not boards:
            continue
        # Named boards first — the operator's own hardware is almost always one
        # of those, and PX4 ships ~150 targets per release.
        boards.sort(key=lambda b: (
            b["label"] == b["name"][:-4],
            b["label"].lower(),
        ))
        out.append({
            "tag": str(item.get("tag_name") or ""),
            "name": str(item.get("name") or item.get("tag_name") or ""),
            "prerelease": bool(item.get("prerelease")),
            "published": str(item.get("published_at") or ""),
            "boards": boards,
        })
        if len(out) >= MAX_RELEASES:
            break
    return out
