"""ArduPilot firmware releases, as the firmware catalogue's second source.

PX4 publishes its builds as GitHub release assets, which is what
:mod:`corvus.firmware_catalog` was written against: one JSON request lists every
release and every ``.px4`` in it. ArduPilot publishes nothing of the sort. Its
builds live on its own server under a path that *is* the release —

    https://firmware.ardupilot.org/<Vehicle>/<channel>/<board>/<binary>.apj

— and the only complete index of it is a ~50 MB ``manifest.json.gz``, which is
not something to download onto a field laptop to fill a dropdown.

So this module reads the directory listing for each vehicle and channel, which
is one small request per pair and gives exactly what the picker needs: the board
names. The URL of the image itself is then *built* from the template above,
never taken from the page that was parsed — same rule as the PX4 side, and the
reason a tampered cache or a rewritten index cannot redirect a flash.

The file format is not a problem. ``.apj`` and ``.px4`` are the same container —
a JSON object whose ``image`` field is base64'd, zlib-compressed firmware — so
:mod:`corvus.firmware_uploader` already flashes an ArduPilot build with no
change at all. Only *finding* it was missing.
"""
from __future__ import annotations

import logging
import re
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from typing import Any

from .version import get_version

logger = logging.getLogger("corvus.firmware")

BASE_URL = "https://firmware.ardupilot.org"
# The one host an ``.apj`` may be fetched from, checked on the resolved URL.
ALLOWED_HOSTS = frozenset({"firmware.ardupilot.org"})

# Release tags carry this prefix so the catalogue can route a flash request back
# to the source that produced it without a second lookup.
TAG_PREFIX = "ardupilot:"

# The vehicles Corvus flies, and the binary each one's build is named after.
# AntennaTracker and Blimp are left out deliberately: they are not aircraft a
# ground station like this is used with, and each one costs a network request.
VEHICLES: list[tuple[str, str, str]] = [
    # (directory, binary stem, operator-facing label)
    ("Copter", "arducopter", "ArduCopter"),
    ("Plane", "arduplane", "ArduPlane"),
    ("Rover", "ardurover", "Rover"),
    ("Sub", "ardusub", "ArduSub"),
]

# stable is what an operator should fly; beta is what they test. `latest` is the
# per-commit build and is not offered — flashing an untested master onto an
# aircraft is not something a ground station should make a two-click operation.
CHANNELS: list[tuple[str, str, bool]] = [
    # (directory, label, prerelease)
    ("stable", "stable", False),
    ("beta", "beta", True),
]

# A board directory name. Deliberately strict: this token is substituted into a
# URL and into a cache filename, so anything outside it is dropped rather than
# escaped.
_BOARD_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$")
# Directory entries that are not boards.
_NOT_BOARDS = frozenset({
    "..", ".", "parent directory", "tools", "logs",
})

FETCH_TIMEOUT_S = 12.0
# Each listing is a page of links. 4 MB is far above any real one and stops a
# redirected or hostile response from being read without bound.
MAX_INDEX_BYTES = 4 * 1024 * 1024


def vehicle_binary(vehicle: str) -> str:
    """The ``.apj`` stem for one vehicle directory, or "" if unknown."""
    for directory, stem, _label in VEHICLES:
        if directory == vehicle:
            return stem
    return ""


def release_tag(vehicle: str, channel: str) -> str:
    """The catalogue tag for one vehicle/channel pair."""
    return f"{TAG_PREFIX}{vehicle}/{channel}"


def parse_tag(tag: str) -> tuple[str, str] | None:
    """``ardupilot:Copter/stable`` -> ``("Copter", "stable")``, or None.

    Validated against the tables above rather than merely parsed: the two halves
    become path segments, and only a name this module already knows may.
    """
    text = str(tag or "")
    if not text.startswith(TAG_PREFIX):
        return None
    rest = text[len(TAG_PREFIX):]
    vehicle, _sep, channel = rest.partition("/")
    if not any(vehicle == v for v, _s, _l in VEHICLES):
        return None
    if not any(channel == c for c, _l, _p in CHANNELS):
        return None
    return vehicle, channel


def asset_name(vehicle: str, channel: str, board: str) -> str:
    """The cache filename for one build.

    ArduPilot names every build ``arducopter.apj`` whatever board it is for, so
    the path is the only thing that distinguishes them. The cache is flat, so
    the distinguishing parts move into the name — otherwise a Pixhawk build and
    a CubeOrange build would be the same file.
    """
    return f"{vehicle_binary(vehicle)}-{channel}-{board}.apj"


def asset_url(vehicle: str, channel: str, board: str) -> str:
    """The download URL for one build, from the fixed template."""
    quote = urllib.parse.quote
    return (
        f"{BASE_URL}/{quote(vehicle, safe='')}/{quote(channel, safe='')}"
        f"/{quote(board, safe='')}/{vehicle_binary(vehicle)}.apj"
    )


def index_url(vehicle: str, channel: str) -> str:
    quote = urllib.parse.quote
    return f"{BASE_URL}/{quote(vehicle, safe='')}/{quote(channel, safe='')}/"


def is_flashable_asset(name: str) -> bool:
    """True for an ArduPilot firmware image."""
    return str(name or "").endswith(".apj")


def host_allowed(url: str) -> bool:
    """Is *url* an https URL on a host firmware is actually published from?

    The scheme is checked with the host, not left to the caller. A firmware
    image is executed by the flight controller, and ``http://`` on a hostname
    from this allow-list is still a plaintext download that anyone on the path
    can replace — the allow-list would say yes to it while proving nothing.
    """
    try:
        parsed = urllib.parse.urlparse(url)
        host = (parsed.hostname or "").lower()
    except ValueError:
        return False
    return parsed.scheme == "https" and host in ALLOWED_HOSTS


def parse_index(html: str) -> list[str]:
    """Board directory names out of one firmware.ardupilot.org listing.

    Untrusted input, and treated as such: every candidate must match
    :data:`_BOARD_RE` whole, so a name carrying a slash, a scheme or an escape
    never reaches the URL template. Order is the server's, then sorted, because
    an operator scanning for their board wants alphabetical rather than
    whatever the filesystem returned.
    """
    names: list[str] = []
    seen: set[str] = set()
    for match in re.finditer(r'href=["\']([^"\'>]+)["\']', str(html or ""), re.I):
        href = urllib.parse.unquote(match.group(1)).strip()
        if not href.endswith("/"):
            continue
        token = href[:-1]
        # A listing may link parents and siblings absolutely; only a plain
        # relative directory name is a board.
        if "/" in token or token.lower() in _NOT_BOARDS:
            continue
        if not _BOARD_RE.match(token) or token in seen:
            continue
        seen.add(token)
        names.append(token)
    return sorted(names, key=str.lower)


def describe_board(board: str) -> dict[str, str]:
    """Split a board directory name into the parts a human reads it by.

    Mirrors :func:`corvus.firmware_catalog.describe_board` so the picker can
    group both stacks' boards the same way. ArduPilot has no naming convention
    anywhere near as regular as PX4's ``<vendor>_<board>_<variant>``, so the
    vendor is inferred from a prefix where there is a recognisable one and the
    board is left whole otherwise — presentation, never a filter.
    """
    name = str(board or "")
    lower = name.lower()
    vendor = ""
    for prefix, label in _VENDOR_PREFIXES:
        if lower.startswith(prefix):
            vendor = label
            break
    return {
        "vendor": vendor or "Other",
        "board": name,
        "variant": "",
        "peripheral": "",
        "title": name,
    }


# Prefixes that reliably name a vendor. Longest first, so "CubeOrange" is not
# claimed by a shorter prefix that happens to overlap.
_VENDOR_PREFIXES: list[tuple[str, str]] = sorted(
    [
        ("cube", "CubePilot"),
        ("pixhawk", "Pixhawk"),
        ("px4", "Pixhawk"),
        ("fmuv", "Pixhawk (generic FMU)"),
        ("holybro", "Holybro"),
        ("durandal", "Holybro"),
        ("kakute", "Holybro"),
        ("matek", "Matek Systems"),
        ("mate", "Matek Systems"),
        ("mro", "mRo"),
        ("speedybee", "SpeedyBee"),
        ("omnibus", "Omnibus"),
        ("f4by", "Swift-Flyer"),
        ("cuav", "CUAV"),
        ("v5", "CUAV"),
        ("nora", "CUAV"),
        ("sky-drones", "Sky-Drones"),
        ("skyviper", "SkyViper"),
        ("bbbmini", "BeagleBone"),
        ("navio", "Emlid"),
        ("edge", "Emlid"),
        ("obal", "Obal"),
        ("revo", "OpenPilot"),
        ("sitl", "Simulator"),
    ],
    key=lambda pair: -len(pair[0]),
)


def _request(url: str) -> urllib.request.Request:
    return urllib.request.Request(url, headers={
        "User-Agent": f"CorvusGCS/{get_version()}",
        "Accept": "text/html",
    })


def _fetch_boards(vehicle: str, channel: str) -> list[str]:
    """The board list for one vehicle/channel, or [] when it cannot be read."""
    url = index_url(vehicle, channel)
    try:
        with urllib.request.urlopen(_request(url), timeout=FETCH_TIMEOUT_S) as resp:
            raw = resp.read(MAX_INDEX_BYTES + 1)
    except (urllib.error.URLError, TimeoutError, OSError, ValueError) as exc:
        logger.info("ArduPilot index %s failed: %s", url, exc)
        return []
    if len(raw) > MAX_INDEX_BYTES:
        logger.info("ArduPilot index %s is implausibly large", url)
        return []
    return parse_index(raw.decode("utf-8", errors="replace"))


def fetch_releases() -> tuple[list[dict[str, Any]], str]:
    """Every vehicle/channel pair with its board list. ``(releases, error)``.

    One request per pair, run in parallel: eight sequential requests at a
    twelve-second timeout is over a minute of a page waiting on a refresh the
    operator asked for. A pair whose listing cannot be read contributes nothing
    and does not fail the rest — the usual field case is no network at all, and
    then the caller falls back to its own on-disk cache.

    The shape is the one :mod:`corvus.firmware_catalog` publishes, so the
    Firmware page renders both stacks with the same code.
    """
    pairs = [(v, s, vl, c, cl, pre)
             for v, s, vl in VEHICLES
             for c, cl, pre in CHANNELS]
    releases: list[dict[str, Any]] = []
    failures = 0
    with ThreadPoolExecutor(max_workers=len(pairs)) as pool:
        futures = {
            pool.submit(_fetch_boards, vehicle, channel): (vehicle, label, channel,
                                                           channel_label, prerelease)
            for vehicle, _stem, label, channel, channel_label, prerelease in pairs
        }
        for future, meta in futures.items():
            vehicle, label, channel, channel_label, prerelease = meta
            try:
                boards = future.result()
            except Exception:  # noqa: BLE001 - one listing must not fail the set
                logger.debug("ArduPilot listing raised", exc_info=True)
                boards = []
            if not boards:
                failures += 1
                continue
            releases.append({
                "tag": release_tag(vehicle, channel),
                "name": f"{label} ({channel_label})",
                "prerelease": prerelease,
                "vendor": "ardupilot",
                "published": "",
                "boards": [
                    {
                        "name": asset_name(vehicle, channel, board),
                        "board": board,
                        "label": board,
                        "size": 0,
                        "url": asset_url(vehicle, channel, board),
                    }
                    for board in boards
                ],
            })
    releases.sort(key=lambda r: (r["prerelease"], r["name"]))
    if failures and not releases:
        return [], "could not reach the ArduPilot firmware server"
    if failures:
        return releases, ""
    return releases, ""


def resolve(releases: list[dict[str, Any]], release_tag_: str,
            board_name: str) -> dict[str, Any] | None:
    """Find one board's build in a cached release list, re-deriving its URL.

    The URL in the cache is ignored and rebuilt from the tag and the board, for
    the same reason the PX4 side does it: the cache is network data written to
    disk, and a tampered file must at worst name a different board on the same
    server.
    """
    parsed = parse_tag(release_tag_)
    if parsed is None:
        return None
    vehicle, channel = parsed
    for release in releases:
        if str(release.get("tag")) != str(release_tag_):
            continue
        for entry in release.get("boards", []):
            if str(entry.get("name")) != str(board_name):
                continue
            board = str(entry.get("board") or "")
            if not _BOARD_RE.match(board):
                return None
            return dict(
                entry,
                tag=release_tag_,
                url=asset_url(vehicle, channel, board),
            )
    return None
