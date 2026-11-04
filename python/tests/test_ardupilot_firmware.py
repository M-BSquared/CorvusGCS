"""The ArduPilot half of the firmware catalogue.

PX4's builds come off GitHub's release API; ArduPilot's come off a directory
listing on its own server. That is untrusted HTML being turned into path
segments, so the parsing is where the care goes: a board name is substituted
into a URL *and* into a cache filename, and anything that is not a plain
directory token has to be dropped rather than escaped.

The other half of the contract is the one the PX4 side already holds to: the
download URL is *built* from a template and never read out of the page or the
cache, so a rewritten index cannot redirect a flash.
"""
from __future__ import annotations

import io
import json
import pathlib
import urllib.error
import urllib.parse

import pytest

from corvus import ardupilot_firmware as af
from corvus.firmware_catalog import FirmwareCatalog

INDEX = """
<html><head><title>Index of /Copter/stable</title></head><body>
<h1>Index of /Copter/stable</h1>
<a href="../">Parent Directory</a>
<a href="CubeOrange/">CubeOrange/</a>
<a href="Pixhawk1/">Pixhawk1/</a>
<a href="MatekF405-Wing/">MatekF405-Wing/</a>
<a href="arducopter.apj">arducopter.apj</a>
<a href="/Copter/beta/">beta</a>
<a href="https://example.invalid/evil/">evil</a>
<a href="../../etc/">up</a>
</body></html>
"""


# ---------------------------------------------------------------------------
# Parsing an index
# ---------------------------------------------------------------------------

def test_only_plain_directory_names_become_boards() -> None:
    assert af.parse_index(INDEX) == ["CubeOrange", "MatekF405-Wing", "Pixhawk1"]


def test_an_absolute_or_traversing_link_is_never_a_board() -> None:
    """The one that matters: these tokens go into a URL and into a filename."""
    boards = af.parse_index(INDEX)
    assert not any("/" in b for b in boards)
    assert "evil" not in boards
    assert ".." not in boards


def test_a_listing_that_is_not_one_yields_nothing_rather_than_raising() -> None:
    assert af.parse_index("") == []
    assert af.parse_index("not html at all") == []
    assert af.parse_index("<a href='%2e%2e/'>x</a>") == []


# ---------------------------------------------------------------------------
# URLs and names
# ---------------------------------------------------------------------------

def test_the_download_url_is_built_from_the_template() -> None:
    assert af.asset_url("Copter", "stable", "CubeOrange") == (
        "https://firmware.ardupilot.org/Copter/stable/CubeOrange/arducopter.apj")
    assert af.asset_url("Plane", "beta", "Pixhawk1").endswith("arduplane.apj")


def test_the_cache_name_keeps_builds_of_different_boards_apart() -> None:
    """ArduPilot names every build after its vehicle, whatever board it is for,
    so the path is the only thing that distinguishes them — and the cache is
    flat."""
    a = af.asset_name("Copter", "stable", "CubeOrange")
    b = af.asset_name("Copter", "stable", "Pixhawk1")
    c = af.asset_name("Copter", "beta", "CubeOrange")
    assert len({a, b, c}) == 3
    assert a.endswith(".apj")


@pytest.mark.parametrize("tag", [
    "ardupilot:Copter/stable",
    "ardupilot:Plane/beta",
    "ardupilot:Sub/stable",
])
def test_a_known_tag_parses(tag: str) -> None:
    assert af.parse_tag(tag) is not None


@pytest.mark.parametrize("tag", [
    "v1.17.0",                      # a PX4 tag
    "ardupilot:Copter/latest",      # a channel that is deliberately not offered
    "ardupilot:Rocket/stable",      # a vehicle that does not exist
    "ardupilot:../../etc/stable",
    "ardupilot:",
    "",
])
def test_an_unknown_tag_is_rejected_rather_than_pieced_together(tag: str) -> None:
    """The tag becomes two path segments, so it is validated against the tables
    rather than merely split."""
    assert af.parse_tag(tag) is None


def test_the_host_rule_admits_one_host(monkeypatch: pytest.MonkeyPatch) -> None:
    assert af.host_allowed("https://firmware.ardupilot.org/Copter/stable/x/y.apj")
    assert not af.host_allowed("https://firmware.ardupilot.org.evil.test/x.apj")
    assert not af.host_allowed("https://github.com/x.apj")
    assert not af.host_allowed("not a url")


# ---------------------------------------------------------------------------
# Inside the catalogue
# ---------------------------------------------------------------------------

def _seed(tmp_path: pathlib.Path) -> None:
    releases = [{
        "tag": af.release_tag("Copter", "stable"),
        "name": "ArduCopter — stable",
        "prerelease": False,
        "vendor": "ardupilot",
        "boards": [{
            "name": af.asset_name("Copter", "stable", "CubeOrange"),
            "board": "CubeOrange",
            "label": "CubeOrange",
            "size": 0,
            # Deliberately wrong, to prove it is never used.
            "url": "https://example.invalid/evil.apj",
        }],
    }]
    (tmp_path / "catalog-ardupilot.json").write_text(
        json.dumps({"releases": releases}), encoding="utf-8")


def test_resolving_an_ardupilot_board_rebuilds_its_url(tmp_path: pathlib.Path) -> None:
    """The cache is network data written to disk. A tampered file must at worst
    name a different board on the same server."""
    _seed(tmp_path)
    catalog = FirmwareCatalog(str(tmp_path))
    entry = catalog.resolve(af.release_tag("Copter", "stable"),
                            af.asset_name("Copter", "stable", "CubeOrange"))
    assert entry is not None
    assert entry["url"] == af.asset_url("Copter", "stable", "CubeOrange")


def test_a_tampered_board_name_resolves_to_nothing(tmp_path: pathlib.Path) -> None:
    _seed(tmp_path)
    path = tmp_path / "catalog-ardupilot.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    data["releases"][0]["boards"][0]["board"] = "../../../etc"
    path.write_text(json.dumps(data), encoding="utf-8")
    catalog = FirmwareCatalog(str(tmp_path))
    assert catalog.resolve(af.release_tag("Copter", "stable"),
                           af.asset_name("Copter", "stable", "CubeOrange")) is None


def test_a_px4_tag_is_never_routed_to_the_ardupilot_source(
    tmp_path: pathlib.Path,
) -> None:
    _seed(tmp_path)
    catalog = FirmwareCatalog(str(tmp_path))
    assert catalog.resolve("v1.17.0", "px4_fmu-v6x_default.px4") is None


def test_an_apj_may_only_be_fetched_from_ardupilots_own_server(
    tmp_path: pathlib.Path,
) -> None:
    catalog = FirmwareCatalog(str(tmp_path))
    with pytest.raises(ValueError, match="unexpected host"):
        catalog.download({"name": "arducopter-stable-CubeOrange.apj",
                          "url": "https://github.com/x/arducopter.apj"})


def test_a_px4_image_may_not_be_fetched_from_the_ardupilot_server(
    tmp_path: pathlib.Path,
) -> None:
    """The two rules are checked together on purpose: neither source may be
    used to pull the other's file type from somewhere it was never published."""
    catalog = FirmwareCatalog(str(tmp_path))
    with pytest.raises(ValueError, match="unexpected host"):
        catalog.download({
            "name": "px4_fmu-v6x_default.px4",
            "url": "https://firmware.ardupilot.org/Copter/stable/x/y.px4",
        })


def test_a_cached_apj_is_listed_as_flashable(tmp_path: pathlib.Path) -> None:
    (tmp_path / "arducopter-stable-CubeOrange.apj").write_bytes(b"{}")
    (tmp_path / "notes.txt").write_bytes(b"x")
    catalog = FirmwareCatalog(str(tmp_path))
    names = [e["name"] for e in catalog.list_cached_images()]
    assert names == ["arducopter-stable-CubeOrange.apj"]


def test_the_catalogue_stays_offline_when_a_cache_can_answer(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Opening the Firmware page in the field must not need the internet — and
    ArduPilot's list costs one request per vehicle and channel, so it must not
    stall the page on eight timeouts either."""
    _seed(tmp_path)
    # A PX4 cache too, so the PX4 half has no reason of its own to go out and
    # this test is about the ArduPilot half rather than about both at once.
    (tmp_path / "catalog.json").write_text(
        json.dumps({"releases": [{
            "tag": "v1.17.0", "name": "v1.17.0", "prerelease": False,
            "published": "", "boards": [{
                "name": "px4_fmu-v6x_default.px4", "label": "Pixhawk 6X", "size": 1,
            }],
        }]}), encoding="utf-8")

    def explode(*_args, **_kwargs):
        raise AssertionError("the catalogue went to the network without being asked")

    monkeypatch.setattr("corvus.ardupilot_firmware.urllib.request.urlopen", explode)
    monkeypatch.setattr("corvus.firmware_catalog.urllib.request.urlopen", explode)
    data = FirmwareCatalog(str(tmp_path)).catalog(refresh=False)
    assert [r["tag"] for r in data["releases"]] == [
        "v1.17.0", af.release_tag("Copter", "stable")]
    assert data["error"] == ""


def test_a_vendor_prefix_groups_a_board_without_filtering_it() -> None:
    assert af.describe_board("CubeOrangePlus")["vendor"] == "CubePilot"
    assert af.describe_board("MatekH743")["vendor"] == "Matek Systems"
    # A board nobody has a prefix for is still listed, under Other.
    assert af.describe_board("SomeNewBoard")["vendor"] == "Other"
    assert af.describe_board("SomeNewBoard")["board"] == "SomeNewBoard"


# ---------------------------------------------------------------------------
# The listing as firmware.ardupilot.org actually serves it
# ---------------------------------------------------------------------------
# Every board is linked by its absolute path and without a trailing slash, next
# to the site navigation and the parent directory. The fixture above is an
# Apache-style listing; this one is the real shape, and parsing only the first
# is how the ArduPilot half of the catalogue came back empty from every refresh.

REAL_INDEX = """
<html><head><title>ArduPilot firmware : /Copter/stable</title></head>
<a href="/"><img src="https://firmware.ardupilot.org/Tools/Logos/x.png"></a>
<a href="http://www.ardupilot.org">Home</a> |
<a href="http://firmware.ardupilot.org/">Firmware</a>
<a href="http://ardupilot.org/dev/docs/pre-built-binaries.html">guide</a>
<table>
<tr><td><img src="/icons/back.gif"></td><td><a href="/Copter"><b>Parent Directory</B> </a></td></tr>
<tr><td><img src="/icons/folder.gif"></td><td><a href="/Copter/stable/CubeBlack+">CubeBlack+</A></td></tr>
<tr><td><img src="/icons/folder.gif"></td><td><a href="/Copter/stable/CubeOrange">CubeOrange</A></td></tr>
<tr><td><img src="/icons/folder.gif"></td><td><a href="/Copter/stable/CubeOrange-heli">CubeOrange-heli</A></td></tr>
<tr><td><img src="/icons/folder.gif"></td><td><a href="/Copter/stable/CubeOrange-SimOnHardWare">CubeOrange-SimOnHardWare</A></td></tr>
<tr><td><img src="/icons/folder.gif"></td><td><a href="/Copter/stable/MatekH743-bdshot">MatekH743-bdshot</A></td></tr>
<tr><td><img src="/icons/folder.gif"></td><td><a href="/Copter/stable/Pixhawk6X">Pixhawk6X</A></td></tr>
<tr><td><img src="/icons/folder.gif"></td><td><a href="/Copter/stable/Pixhawk6X-heli">Pixhawk6X-heli</A></td></tr>
<tr><td><img src="/icons/folder.gif"></td><td><a href="/Copter/stable/SITL_x86_64_linux_gnu">SITL_x86_64_linux_gnu</A></td></tr>
<tr><td><img src="/icons/folder.gif"></td><td><a href="/Copter/stable/apm2-quad">apm2-quad</A></td></tr>
<tr><td><img src="/icons/folder.gif"></td><td><a href="/Copter/stable/navio2">navio2</A></td></tr>
<tr><td><img src="/icons/folder.gif"></td><td><a href="/Copter/stable/navio2-heli">navio2-heli</A></td></tr>
<tr><td><img src="/icons/folder.gif"></td><td><a href="/Copter/stable/fmuv3">fmuv3</A></td></tr>
<tr><td><a href="/Copter/beta/Pixhawk1">elsewhere</A></td></tr>
<tr><td><a href="/Copter/stable/CubeOrange/arducopter.apj">a file</A></td></tr>
</table></html>
"""


def test_the_real_listing_yields_its_boards() -> None:
    assert af.parse_index(REAL_INDEX, base="/Copter/stable/") == [
        "apm2-quad", "CubeBlack+", "CubeOrange", "CubeOrange-heli",
        "CubeOrange-SimOnHardWare", "fmuv3", "MatekH743-bdshot", "navio2",
        "navio2-heli", "Pixhawk6X", "Pixhawk6X-heli", "SITL_x86_64_linux_gnu",
    ]


def test_only_links_under_the_listings_own_path_are_boards() -> None:
    """The parent directory, the navigation, another channel and a file one
    level down are all links on the same page, and none of them is a board."""
    boards = af.parse_index(REAL_INDEX, base="/Copter/stable/")
    assert "Copter" not in boards
    assert "Pixhawk1" not in boards
    assert not any("/" in b or b.endswith(".apj") for b in boards)


def _copter(name: str) -> af.Vehicle:
    return next(v for v in af.VEHICLES if v.key == name)


def test_a_helicopter_is_its_own_firmware_not_a_second_copy_of_every_board() -> None:
    listing = af.parse_index(REAL_INDEX, base="/Copter/stable/")
    copter = af.vehicle_boards(_copter("Copter"), listing)
    heli = af.vehicle_boards(_copter("Heli"), listing)
    assert not any(b.endswith("-heli") for b in copter)
    # Same name under both, so a board picked under one is found under the other.
    assert heli == ["CubeOrange", "Pixhawk6X"]
    assert af.asset_url("Heli", "stable", "CubeOrange") == (
        "https://firmware.ardupilot.org/Copter/stable/CubeOrange-heli/arducopter-heli.apj")
    assert af.asset_name("Heli", "stable", "CubeOrange") != af.asset_name(
        "Copter", "stable", "CubeOrange")


def test_boards_with_no_apj_are_not_offered() -> None:
    """Linux autopilots, the simulator and the AVR boards have directories on
    the server but no image a USB bootloader takes. Offering one ends in a 404
    after the operator has already committed to the flash."""
    listing = af.parse_index(REAL_INDEX, base="/Copter/stable/")
    copter = af.vehicle_boards(_copter("Copter"), listing)
    for gone in ("navio2", "SITL_x86_64_linux_gnu", "apm2-quad"):
        assert gone not in copter
    assert "navio2" not in af.vehicle_boards(_copter("Heli"), listing)
    # Exact names, not prefixes: a board that merely starts like one stays.
    assert af.is_flashable_board("AEROFOX-H7")
    assert af.is_flashable_board("GreenSightUltraBlue")
    assert not af.is_flashable_board("aero")


def test_a_plus_in_a_board_name_is_escaped_in_the_url() -> None:
    assert af.asset_url("Copter", "stable", "CubeBlack+") == (
        "https://firmware.ardupilot.org/Copter/stable/CubeBlack%2B/arducopter.apj")


def test_a_sim_on_hardware_build_is_a_developer_build() -> None:
    """It flies a simulation instead of the aircraft it is flashed onto."""
    described = af.describe_board("CubeOrange-SimOnHardWare")
    assert described["variant"] == "SimOnHardWare"
    assert described["title"] == "CubeOrange"
    # The directory name stays whole: the URL is built from it.
    assert described["board"] == "CubeOrange-SimOnHardWare"
    assert af.describe_board("MatekH743-bdshot")["variant"] == "default"


class _Response(io.BytesIO):
    def __enter__(self) -> _Response:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()


def _serve(pages: dict[str, str]):
    def urlopen(request, timeout=None):  # noqa: ARG001 - urlopen's signature
        url = request.full_url
        if url not in pages:
            raise urllib.error.URLError("no route")
        path = urllib.parse.urlparse(url).path
        return _Response(pages[url].replace("/Copter/stable/", path).encode())
    return urlopen


def test_fetching_builds_one_release_per_vehicle_and_channel(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    pages = {af.index_url(d, c): REAL_INDEX
             for d in ("Copter", "Plane", "Rover", "Sub") for c in ("stable", "beta")}
    monkeypatch.setattr(af.urllib.request, "urlopen", _serve(pages))
    releases, error = af.fetch_releases()
    assert error == ""
    tags = [r["tag"] for r in releases]
    assert tags[:5] == [af.release_tag(v, "stable")
                        for v in ("Copter", "Heli", "Plane", "Rover", "Sub")]
    assert all(r["prerelease"] for r in releases[5:])
    heli = next(r for r in releases if r["tag"] == af.release_tag("Heli", "stable"))
    assert heli["vehicle"] == "Heli"
    assert heli["name"] == "ArduCopter Heli (stable)"
    assert [b["board"] for b in heli["boards"]] == ["CubeOrange", "Pixhawk6X"]
    plane = next(r for r in releases if r["tag"] == af.release_tag("Plane", "beta"))
    assert plane["boards"][0]["url"].startswith("https://firmware.ardupilot.org/Plane/beta/")


def test_a_server_that_answers_with_no_boards_is_an_error_not_an_empty_list(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """This is what a changed listing format looks like. Reported as nothing
    at all, it read as "ArduPilot publishes no firmware" and hid the bug."""
    pages = {af.index_url(d, c): "<html>moved</html>"
             for d in ("Copter", "Plane", "Rover", "Sub") for c in ("stable", "beta")}
    monkeypatch.setattr(af.urllib.request, "urlopen", _serve(pages))
    releases, error = af.fetch_releases()
    assert releases == []
    assert "no boards" in error


def test_an_unreachable_server_says_so(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(af.urllib.request, "urlopen", _serve({}))
    releases, error = af.fetch_releases()
    assert releases == []
    assert "could not reach" in error


def test_a_tampered_cache_cannot_aim_a_flash_at_a_board_with_no_image(
    tmp_path: pathlib.Path,
) -> None:
    _seed(tmp_path)
    path = tmp_path / "catalog-ardupilot.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    data["releases"][0]["boards"][0]["board"] = "navio2"
    path.write_text(json.dumps(data), encoding="utf-8")
    catalog = FirmwareCatalog(str(tmp_path))
    assert catalog.resolve(af.release_tag("Copter", "stable"),
                           af.asset_name("Copter", "stable", "CubeOrange")) is None


# ---------------------------------------------------------------------------
# Which board, and which vehicle
# ---------------------------------------------------------------------------

_AP_BOARDS = [
    {"name": af.asset_name("Copter", "stable", b), "board": b, "title": b}
    for b in ("CubeOrange", "CubeOrangePlus", "Pixhawk6X", "MatekH743", "fmuv3")
]


@pytest.mark.parametrize("description,expected", [
    # ArduPilot's USB product string is the board's own name.
    ("CubeOrange", "CubeOrange"),
    ("CubeOrangePlus", "CubeOrangePlus"),
    ("MatekH743", "MatekH743"),
    # A board still on PX4 names its FMU generation instead.
    ("PX4 FMU v6X.x", "Pixhawk6X"),
    ("PX4 BL FMU v3.x", "fmuv3"),
    # PX4's spelling of the product, on a board moving to ArduPilot.
    ("Cube Orange+", "CubeOrangePlus"),
])
def test_the_usb_descriptor_names_the_ardupilot_board(description: str, expected: str) -> None:
    found = af.detect_board({"description": description, "hwid": ""}, _AP_BOARDS)
    assert found is not None and found["key"] == expected
    assert found["name"] == af.asset_name("Copter", "stable", expected)


def test_an_unrecognised_descriptor_suggests_no_ardupilot_board() -> None:
    assert af.detect_board({"description": "FT232R USB UART"}, _AP_BOARDS) is None
    assert af.detect_board({"description": ""}, _AP_BOARDS) is None
    assert af.detect_board({"description": "CubeOrange"}, []) is None


@pytest.mark.parametrize("mav_type,expected", [
    (2, "Copter"),     # QUADROTOR
    (13, "Copter"),    # HEXAROTOR
    (4, "Heli"),       # HELICOPTER: a separate image
    (1, "Plane"),      # FIXED_WING
    (20, "Plane"),     # VTOL_TAILSITTER_QUADROTOR: QuadPlane is ArduPlane
    (10, "Rover"),     # GROUND_ROVER
    (11, "Rover"),     # SURFACE_BOAT
    (12, "Sub"),       # SUBMARINE
    (0, ""),           # no heartbeat yet
    ("x", ""),
])
def test_the_connected_airframe_names_its_firmware(mav_type: object, expected: str) -> None:
    assert af.vehicle_for_mav_type(mav_type) == expected
