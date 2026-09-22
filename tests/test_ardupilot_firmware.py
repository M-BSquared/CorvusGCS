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

import json
import pathlib

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
