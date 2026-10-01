"""ArduPilot firmware releases, as the firmware catalogue's second source.

PX4 publishes its builds as GitHub release assets, which is what
:mod:`corvus.firmware_catalog` was written against: one JSON request lists every
release and every ``.px4`` in it. ArduPilot publishes nothing of the sort. Its
builds live on its own server under a path that *is* the release —

    https://firmware.ardupilot.org/<Vehicle>/<channel>/<board>/<binary>.apj

— and the only complete index of it is ``manifest.json.gz``: about 2 MB on the
wire, but some 80 MB of JSON once unpacked, every byte of which would have to be
parsed to fill a dropdown on a field laptop.

So this module reads the directory listing for each vehicle and channel, which
is one small request per pair and gives exactly what the picker needs: the board
names. The server links each board directory by its absolute path and without
a trailing slash (``href="/Copter/stable/CubeOrange"``), so a board is a link
directly under the listing's own path and nothing else. The URL of the image
itself is then *built* from the template above, never taken from the page that
was parsed — same rule as the PX4 side, and the reason a tampered cache or a
rewritten index cannot redirect a flash.

Two things in a listing are not what they look like. A traditional helicopter
is a Copter build in a ``<board>-heli`` directory whose image is
``arducopter-heli.apj``, so it is offered as a vehicle of its own rather than as
a second copy of every board. And a Linux autopilot, the simulator and the AVR
boards have directories too, but no ``.apj`` in them; they are dropped rather
than offered as a download that can only 404.

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
from typing import Any, NamedTuple

from . import autopilot
from .version import get_version

logger = logging.getLogger("corvus.firmware")

BASE_URL = "https://firmware.ardupilot.org"
# The one host an ``.apj`` may be fetched from, checked on the resolved URL.
ALLOWED_HOSTS = frozenset({"firmware.ardupilot.org"})

# Release tags carry this prefix so the catalogue can route a flash request back
# to the source that produced it without a second lookup.
TAG_PREFIX = "ardupilot:"


class Vehicle(NamedTuple):
    """One firmware an operator can pick, and where the server keeps it."""

    key: str        # the tag's vehicle half, and what ``suggested.vehicle`` names
    directory: str  # the server directory its builds are listed under
    suffix: str     # the board directory suffix that marks its builds ("" = none)
    binary: str     # the ``.apj`` stem
    label: str      # operator-facing name


# The vehicles Corvus flies. AntennaTracker and Blimp are left out deliberately:
# they are not aircraft a ground station like this is used with, and each one
# costs a network request.
VEHICLES: list[Vehicle] = [
    Vehicle("Copter", "Copter", "", "arducopter", "ArduCopter"),
    Vehicle("Heli", "Copter", "-heli", "arducopter-heli", "ArduCopter Heli"),
    Vehicle("Plane", "Plane", "", "arduplane", "ArduPlane"),
    Vehicle("Rover", "Rover", "", "ardurover", "Rover"),
    Vehicle("Sub", "Sub", "", "ardusub", "ArduSub"),
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
# A link under the listing's path that names a file rather than a directory.
_FILE_SUFFIXES = (".apj", ".abin", ".elf", ".hex", ".bin", ".px4",
                  ".txt", ".json", ".gz", ".html", ".xml")

# Board directories whose builds are not a USB bootloader image: Linux
# autopilots (the firmware is an executable copied onto the companion OS), the
# SITL simulator and Qualcomm's QURT, and the AVR-era APM1/APM2, whose ``.hex``
# needs a different programmer altogether. They sit in the listing next to the
# rest, but their directories hold no ``.apj``.
_NOT_FLASHABLE = frozenset({
    "aero", "bbbmini", "bhat", "blue", "canzero", "dark", "disco", "edge",
    "erleboard", "erlebrain2", "linux", "modalai-voxl2", "navigator",
    "navigator64", "navio", "navio2", "obal", "ocpoc_zynq", "pilotpi",
    "pocket", "pocket2", "pxf", "pxfmini", "qurt", "t3-gem-o1", "vnav",
})
_NOT_FLASHABLE_PREFIXES = ("sitl", "apm1", "apm2", "bebop")

# A build that runs a simulated airframe on the real board. It is published next
# to the flight build for development, and flashed onto an aircraft it flies a
# simulation instead of the aircraft, so the picker treats it as a developer
# build: listed, never offered by default.
_SIM_ON_HARDWARE = "-simonhardware"

FETCH_TIMEOUT_S = 12.0
# Each listing is a page of links. 4 MB is far above any real one and stops a
# redirected or hostile response from being read without bound.
MAX_INDEX_BYTES = 4 * 1024 * 1024


def _vehicle(key: str) -> Vehicle | None:
    for vehicle in VEHICLES:
        if vehicle.key == key:
            return vehicle
    return None


def vehicle_binary(vehicle: str) -> str:
    """The ``.apj`` stem for one vehicle, or "" if unknown."""
    found = _vehicle(vehicle)
    return found.binary if found else ""


def vehicle_for_mav_type(mav_type: Any) -> str:
    """The firmware a vehicle of this MAV_TYPE runs, or "" when none fits.

    What the Firmware page opens on when an aircraft is connected, so that
    re-flashing a plane does not start from the copter build. A helicopter is
    told apart here because its firmware is a separate image.
    """
    try:
        if int(mav_type) == 4:      # MAV_TYPE_HELICOPTER
            return "Heli"
    except (TypeError, ValueError):
        return ""
    return {
        autopilot.VEHICLE_COPTER: "Copter",
        autopilot.VEHICLE_PLANE: "Plane",
        autopilot.VEHICLE_ROVER: "Rover",
        autopilot.VEHICLE_SUB: "Sub",
    }.get(autopilot.vehicle_class(mav_type), "")


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
    if _vehicle(vehicle) is None:
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
    found = _vehicle(vehicle)
    directory = found.directory if found else vehicle
    suffix = found.suffix if found else ""
    return (
        f"{BASE_URL}/{quote(directory, safe='')}/{quote(channel, safe='')}"
        f"/{quote(board + suffix, safe='')}/{vehicle_binary(vehicle)}.apj"
    )


def index_url(directory: str, channel: str) -> str:
    quote = urllib.parse.quote
    return f"{BASE_URL}/{quote(directory, safe='')}/{quote(channel, safe='')}/"


def is_flashable_asset(name: str) -> bool:
    """True for an ArduPilot firmware image."""
    return str(name or "").endswith(".apj")


def is_flashable_board(board: str) -> bool:
    """False for a board directory that holds no ``.apj`` to flash."""
    token = str(board or "").lower()
    for vehicle in VEHICLES:
        if vehicle.suffix and token.endswith(vehicle.suffix):
            token = token[:-len(vehicle.suffix)]
            break
    return not (token in _NOT_FLASHABLE or token.startswith(_NOT_FLASHABLE_PREFIXES))


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


def parse_index(html: str, base: str = "") -> list[str]:
    """Board directory names out of one firmware.ardupilot.org listing.

    *base* is the listing's own path (``/Copter/stable/``). The server links
    every board by its absolute path under it, without a trailing slash; a
    plain relative ``Board/`` is accepted too. Anything else on the page (the
    parent directory, the site navigation, another channel) is not a board.

    Untrusted input, and treated as such: every candidate must match
    :data:`_BOARD_RE` whole, so a name carrying a slash, a scheme or an escape
    never reaches the URL template. Sorted, because an operator scanning for
    their board wants alphabetical rather than whatever the filesystem returned.
    """
    prefix = base if not base or base.endswith("/") else base + "/"
    names: list[str] = []
    seen: set[str] = set()
    for match in re.finditer(r'href=["\']([^"\'>]+)["\']', str(html or ""), re.I):
        href = urllib.parse.unquote(match.group(1)).strip()
        try:
            parsed = urllib.parse.urlparse(href)
        except ValueError:
            continue
        if parsed.scheme or parsed.netloc:
            if (parsed.hostname or "").lower() not in ALLOWED_HOSTS:
                continue
            href = parsed.path
        if prefix and href.startswith(prefix):
            token = href[len(prefix):]
            token = token[:-1] if token.endswith("/") else token
            if token.lower().endswith(_FILE_SUFFIXES):
                continue
        elif href.endswith("/") and not href.startswith("/"):
            token = href[:-1]
        else:
            continue
        if "/" in token or token.lower() in _NOT_BOARDS:
            continue
        if not _BOARD_RE.match(token) or token in seen:
            continue
        seen.add(token)
        names.append(token)
    return sorted(names, key=str.lower)


def vehicle_boards(vehicle: Vehicle, listing: list[str]) -> list[str]:
    """One vehicle's flashable boards out of its directory's listing.

    Copter and Heli share a directory; a heli build is the ``-heli`` directory
    with the suffix taken off, so the same board has the same name under both
    and a board chosen under one is still selected under the other.
    """
    siblings = [v.suffix for v in VEHICLES
                if v.directory == vehicle.directory and v.suffix]
    out: list[str] = []
    for name in listing:
        if not is_flashable_board(name):
            continue
        if vehicle.suffix:
            if not name.endswith(vehicle.suffix):
                continue
            board = name[:-len(vehicle.suffix)]
            if board and _BOARD_RE.match(board):
                out.append(board)
        elif not any(name.endswith(s) for s in siblings):
            out.append(name)
    return out


def describe_board(board: str) -> dict[str, Any]:
    """Split a board directory name into the parts a human reads it by.

    Mirrors :func:`corvus.firmware_catalog.describe_board` so the picker can
    group both stacks' boards the same way. ArduPilot has no naming convention
    anywhere near as regular as PX4's ``<vendor>_<board>_<variant>``, so the
    vendor is inferred from a prefix where there is a recognisable one and the
    board is left whole otherwise — presentation, never a filter. ``board``
    stays the directory name: it is what the download URL is built from.
    """
    name = str(board or "")
    lower = name.lower()
    vendor = ""
    for prefix, label in _VENDOR_PREFIXES:
        if lower.startswith(prefix):
            vendor = label
            break
    variant = "default"
    title = name
    if lower.endswith(_SIM_ON_HARDWARE):
        variant = name[-len(_SIM_ON_HARDWARE) + 1:]
        title = name[:-len(_SIM_ON_HARDWARE)]
    return {
        "vendor": vendor or "Other",
        "board": name,
        "variant": variant,
        "peripheral": False,
        "title": title,
    }


# Prefixes that reliably name a vendor. Longest first, so "CubeOrange" is not
# claimed by a shorter prefix that happens to overlap.
_VENDOR_PREFIXES: list[tuple[str, str]] = sorted(
    [
        ("3drcontrol", "3DR"),
        ("aerofox", "AeroFox"),
        ("airbot", "Airbot"),
        ("airlink", "Sky-Drones"),
        ("aocoda", "Aocoda-RC"),
        ("ark", "ARK Electronics"),
        ("atomrc", "AtomRC"),
        ("beast", "Airbot"),
        ("betafpv", "BETAFPV"),
        ("blitz", "iFlight"),
        ("brotherhobby", "Brother Hobby"),
        ("corvon", "Corvon"),
        ("crazyflie", "Bitcraze"),
        ("cuav", "CUAV"),
        ("cube", "CubePilot"),
        ("dakefpv", "DAKEFPV"),
        ("drotek", "Drotek"),
        ("durandal", "Holybro"),
        ("f4by", "Swift-Flyer"),
        ("flyingmoon", "FlyingMoon"),
        ("flywoo", "Flywoo"),
        ("fmuv", "Pixhawk (generic FMU)"),
        ("foxeer", "Foxeer"),
        ("geprc", "GEPRC"),
        ("heewing", "HEEWING"),
        ("here4", "CubePilot"),
        ("holybro", "Holybro"),
        ("iflight", "iFlight"),
        ("jhem", "JHEMCU"),
        ("kakute", "Holybro"),
        ("lumenier", "Lumenier"),
        ("mamba", "Diatone"),
        ("mate", "Matek Systems"),
        ("matek", "Matek Systems"),
        ("micoair", "MicoAir"),
        ("mindpx", "AirMind"),
        ("mini-pix", "Radiolink"),
        ("modalai", "ModalAI"),
        ("mro", "mRo"),
        ("narinfc", "NarinFC"),
        ("nora", "CUAV"),
        ("omnibus", "Omnibus"),
        ("orqa", "Orqa"),
        ("ph4-mini", "Holybro"),
        ("pix32", "Holybro"),
        ("pixhawk", "Pixhawk"),
        ("pixpilot", "PixPilot"),
        ("pixracer", "mRo"),
        ("px4", "Pixhawk"),
        ("qiotek", "QioTek"),
        ("radiolink", "Radiolink"),
        ("revo", "OpenPilot"),
        ("sdmodel", "SDMODEL"),
        ("siyi", "SIYI"),
        ("sky-drones", "Sky-Drones"),
        ("skydroid", "SkyDroid"),
        ("skystars", "Skystars"),
        ("skyviper", "SkyViper"),
        ("sparky2", "TauLabs"),
        ("spedix", "SPEDIX"),
        ("speedybee", "SpeedyBee"),
        ("spracing", "SPRacing"),
        ("succex", "iFlight"),
        ("tbs", "TBS"),
        ("thepeach", "ThePeach"),
        ("tmotor", "T-Motor"),
        ("v5", "CUAV"),
        ("vrbrain", "Virtual Robotix"),
        ("vrcore", "Virtual Robotix"),
        ("vrubrain", "Virtual Robotix"),
        ("vuav", "VUAV"),
        ("x-mav", "X-MAV"),
        ("yjuav", "YJUAV"),
        ("zeroone", "ZeroOne"),
    ],
    key=lambda pair: -len(pair[0]),
)


# --------------------------------------------------------------------------
# Board detection
# --------------------------------------------------------------------------
# PX4's USB descriptor names the FMU generation rather than the product. When a
# board still runs PX4 and is being moved to ArduPilot, this is the ArduPilot
# build for the same generation.
_FMU_RE = re.compile(r"fmu[\s_-]*v?(\d+[a-z]*)", re.I)
_FMU_BOARDS: dict[str, str] = {
    "fmu-v2": "fmuv2",
    "fmu-v3": "fmuv3",
    "fmu-v4": "Pixracer",
    "fmu-v5": "Pixhawk4",
    "fmu-v5x": "Pixhawk5X",
    "fmu-v6c": "Pixhawk6C",
    "fmu-v6x": "Pixhawk6X",
}
# Shorter board names are too likely to turn up inside an unrelated descriptor.
_MIN_EMBEDDED_NAME = 5


def _norm(text: Any) -> str:
    """A board name as a descriptor might spell it: case, spaces and dashes gone."""
    return re.sub(r"[^a-z0-9]", "", str(text or "").lower().replace("+", "plus"))


def detect_board(hints: dict[str, Any], boards: list[dict[str, Any]]) -> dict[str, Any] | None:
    """Guess which of *boards* is plugged in, from the USB descriptor.

    ArduPilot sets its USB product string to the board's own name, so a board
    running ArduPilot says which build it takes in so many words. A board still
    on PX4 names its FMU generation instead, which maps onto one ArduPilot
    board per generation.

    Returns ``{name, key, label, token, source}`` for a board in *boards*, or
    None. A suggestion only: the caller preselects it and the operator can
    always override. Never raises.
    """
    description = str(hints.get("description") or "")
    if not boards or not description.strip():
        return None
    by_key: dict[str, dict[str, Any]] = {}
    for entry in boards:
        key = _norm(entry.get("board"))
        if key and key not in by_key:
            by_key[key] = entry

    def found(key: str) -> dict[str, Any]:
        entry = by_key[key]
        board = str(entry.get("board") or "")
        return {"name": entry.get("name"), "key": board,
                "label": entry.get("title") or board, "token": board,
                "source": "USB descriptor"}

    whole = _norm(description)
    for candidate in [whole] + [_norm(word) for word in description.split()]:
        if candidate in by_key:
            return found(candidate)

    fmu = _FMU_RE.search(description)
    if fmu:
        target = _norm(_FMU_BOARDS.get(f"fmu-v{fmu.group(1).lower()}", ""))
        if target in by_key:
            return found(target)

    embedded = [key for key in by_key if len(key) >= _MIN_EMBEDDED_NAME and key in whole]
    if embedded:
        return found(max(embedded, key=len))
    return None


def _request(url: str) -> urllib.request.Request:
    return urllib.request.Request(url, headers={
        "User-Agent": f"CorvusGCS/{get_version()}",
        "Accept": "text/html",
    })


def _fetch_listing(directory: str, channel: str) -> list[str] | None:
    """The board directories of one listing; None when it cannot be read."""
    url = index_url(directory, channel)
    try:
        with urllib.request.urlopen(_request(url), timeout=FETCH_TIMEOUT_S) as resp:
            raw = resp.read(MAX_INDEX_BYTES + 1)
    except (urllib.error.URLError, TimeoutError, OSError, ValueError) as exc:
        logger.info("ArduPilot index %s failed: %s", url, exc)
        return None
    if len(raw) > MAX_INDEX_BYTES:
        logger.info("ArduPilot index %s is implausibly large", url)
        return None
    base = urllib.parse.urlparse(url).path
    return parse_index(raw.decode("utf-8", errors="replace"), base=base)


def fetch_releases() -> tuple[list[dict[str, Any]], str]:
    """Every vehicle/channel pair with its board list. ``(releases, error)``.

    One request per directory and channel, run in parallel: eight sequential
    requests at a twelve-second timeout is over a minute of a page waiting on a
    refresh the operator asked for. Copter and Heli share one listing. A pair
    whose listing cannot be read contributes nothing and does not fail the
    rest — the usual field case is no network at all, and then the caller falls
    back to its own on-disk cache.

    A server that answers but yields no boards is reported as an error rather
    than returned as an empty list: that is what a changed listing format looks
    like, and an empty list would read as "ArduPilot has no firmware".

    The shape is the one :mod:`corvus.firmware_catalog` publishes, so the
    Firmware page renders both stacks with the same code.
    """
    pairs = [(directory, channel)
             for directory in dict.fromkeys(v.directory for v in VEHICLES)
             for channel, _label, _pre in CHANNELS]
    listings: dict[tuple[str, str], list[str] | None] = {}
    with ThreadPoolExecutor(max_workers=len(pairs)) as pool:
        futures = {pool.submit(_fetch_listing, d, c): (d, c) for d, c in pairs}
        for future, pair in futures.items():
            try:
                listings[pair] = future.result()
            except Exception:  # noqa: BLE001 - one listing must not fail the set
                logger.debug("ArduPilot listing raised", exc_info=True)
                listings[pair] = None

    # Stable first, then the vehicles in table order, which is also the order
    # an operator scans for theirs.
    releases: list[dict[str, Any]] = []
    for channel, channel_label, prerelease in CHANNELS:
        for vehicle in VEHICLES:
            boards = vehicle_boards(vehicle, listings.get((vehicle.directory, channel)) or [])
            if not boards:
                continue
            releases.append({
                "tag": release_tag(vehicle.key, channel),
                "name": f"{vehicle.label} ({channel_label})",
                "prerelease": prerelease,
                "vendor": "ardupilot",
                "vehicle": vehicle.key,
                "published": "",
                "boards": [
                    {
                        "name": asset_name(vehicle.key, channel, board),
                        "board": board,
                        "label": board,
                        "size": 0,
                        "url": asset_url(vehicle.key, channel, board),
                    }
                    for board in boards
                ],
            })
    if releases:
        return releases, ""
    if all(listing is None for listing in listings.values()):
        return [], "could not reach the ArduPilot firmware server"
    return [], "the ArduPilot firmware server answered, but no boards could be read from it"


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
            if not _BOARD_RE.match(board) or not is_flashable_board(board):
                return None
            return dict(
                entry,
                tag=release_tag_,
                url=asset_url(vehicle, channel, board),
            )
    return None
