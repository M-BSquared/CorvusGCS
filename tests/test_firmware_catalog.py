"""PX4 firmware catalogue: parsing, safety, and the offline fallback.

The catalogue is the one place in this app that reaches out to the internet on
the operator's behalf, and the one place where network data decides what gets
written to a flight controller. So the tests here are mostly about what must
NOT happen: no bootloader images offered as firmware, no download from a host
we did not choose, no blank page when there is no network.
"""
from __future__ import annotations

import json
import pathlib
import threading

import pytest

from corvus.firmware_catalog import (
    ALLOWED_HOSTS,
    FirmwareCatalog,
    _parse_releases,
    asset_url,
    board_label,
    detect_board,
    is_flashable_asset,
)

_BOARDS = [
    {"name": "px4_fmu-v6x_default.px4", "label": "Pixhawk 6X (FMUv6X)"},
    {"name": "px4_fmu-v5_default.px4", "label": "Pixhawk 4 (FMUv5)"},
    {"name": "px4_fmu-v4_default.px4", "label": "Pixracer (FMUv4)"},
    {"name": "cubepilot_cubeorange_default.px4", "label": "Cube Orange"},
    {"name": "cubepilot_cubeorangeplus_default.px4", "label": "Cube Orange+"},
]


# ---------------------------------------------------------------------------
# Board detection
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("description,expected", [
    # PX4 builds its USB product string from the board, so the descriptor the
    # OS already has is the most direct answer to "what is plugged in".
    ("PX4 FMU v6X.x", "px4_fmu-v6x_default.px4"),
    ("PX4 FMU v5.x", "px4_fmu-v5_default.px4"),
    # A board sitting in its bootloader still identifies itself.
    ("PX4 BL FMU v6X.x", "px4_fmu-v6x_default.px4"),
    ("CubeOrange", "cubepilot_cubeorange_default.px4"),
    ("Cube Orange+", "cubepilot_cubeorangeplus_default.px4"),
])
def test_the_usb_descriptor_identifies_the_board(description: str, expected: str) -> None:
    found = detect_board({"description": description, "hwid": ""}, _BOARDS)
    assert found is not None and found["name"] == expected


def test_cube_orange_plus_is_not_read_as_a_plain_cube_orange() -> None:
    """The longer name has to win, or the plus variant gets the wrong image."""
    found = detect_board({"description": "CubeOrangePlus", "hwid": ""}, _BOARDS)
    assert found["name"] == "cubepilot_cubeorangeplus_default.px4"


def test_a_generic_descriptor_falls_back_to_the_usb_ids() -> None:
    found = detect_board(
        {"description": "USB Serial", "hwid": "USB VID:PID=26AC:0016"}, _BOARDS)
    assert found is not None and found["name"] == "px4_fmu-v4_default.px4"
    assert found["source"] == "USB device id"


def test_an_unrecognised_device_detects_nothing_rather_than_guessing() -> None:
    """A wrong suggestion is worse than none: it preselects the wrong image."""
    assert detect_board(
        {"description": "FT232R USB UART", "hwid": "USB VID:PID=0403:6001"}, _BOARDS) is None
    assert detect_board({}, _BOARDS) is None
    assert detect_board({"description": "PX4 FMU v6X.x"}, []) is None


def test_detection_only_ever_names_a_board_from_the_given_list() -> None:
    """The suggestion has to be selectable, or the UI preselects a phantom."""
    boards = [{"name": "px4_fmu-v5_default.px4", "label": "Pixhawk 4 (FMUv5)"}]
    assert detect_board({"description": "PX4 FMU v6X.x"}, boards) is None


def _release(tag: str, assets: list[str], prerelease: bool = False) -> dict:
    return {
        "tag_name": tag,
        "name": tag,
        "prerelease": prerelease,
        "published_at": "2026-01-01T00:00:00Z",
        "assets": [
            {
                "name": name,
                "size": 1024,
                "browser_download_url":
                    f"https://github.com/PX4/PX4-Autopilot/releases/download/{tag}/{name}",
            }
            for name in assets
        ],
    }


# ---------------------------------------------------------------------------
# What may be flashed
# ---------------------------------------------------------------------------

def test_bootloader_images_are_never_offered_as_firmware() -> None:
    """A bootloader flashed through the firmware uploader bricks the board.

    PX4 ships bootloader and cannode images in the same release as the flight
    controller firmware, and their names differ by one suffix. Offering them in
    the same list as the real firmware is a foot-gun with no upside.
    """
    assert is_flashable_asset("px4_fmu-v6x_default.px4")
    assert not is_flashable_asset("px4_fmu-v6x_bootloader.px4")
    assert not is_flashable_asset("px4_fmu-v6x_canbootloader.px4")
    assert not is_flashable_asset("ark_can-flow_cannode.px4")
    assert not is_flashable_asset("px4_fmu-v6x_default.bin")

    releases = _parse_releases([_release("v1.17.0", [
        "px4_fmu-v6x_default.px4",
        "px4_fmu-v6x_bootloader.px4",
        "px4_fmu-v5_default.px4",
    ])])
    names = {b["name"] for b in releases[0]["boards"]}
    assert names == {"px4_fmu-v6x_default.px4", "px4_fmu-v5_default.px4"}


def test_assets_from_an_unexpected_host_are_dropped() -> None:
    payload = [{
        "tag_name": "v1.17.0", "name": "v1.17.0", "prerelease": False,
        "assets": [
            {"name": "evil.px4", "size": 1,
             "browser_download_url": "https://evil.example/evil.px4"},
            {"name": "px4_fmu-v6x_default.px4", "size": 1,
             "browser_download_url":
                 "https://github.com/PX4/PX4-Autopilot/releases/download/v1.17.0/"
                 "px4_fmu-v6x_default.px4"},
        ],
    }]
    boards = _parse_releases(payload)[0]["boards"]
    assert [b["name"] for b in boards] == ["px4_fmu-v6x_default.px4"]


def test_draft_releases_and_junk_payloads_are_ignored() -> None:
    assert _parse_releases([dict(_release("v1.0.0", ["a_default.px4"]), draft=True)]) == []
    assert _parse_releases([_release("v1.0.0", ["only_bootloader.px4"])]) == []
    assert _parse_releases({"not": "a list"}) == []
    assert _parse_releases([None, 42, "nope"]) == []


def test_named_boards_sort_ahead_of_raw_target_names() -> None:
    """~150 targets per release; the operator's board should not be page three."""
    releases = _parse_releases([_release("v1.17.0", [
        "zzz_unknown-board_default.px4",
        "px4_fmu-v6x_default.px4",
    ])])
    labels = [b["label"] for b in releases[0]["boards"]]
    assert labels[0] == "Pixhawk 6X (FMUv6X)"


def test_board_label_falls_back_to_the_target_name() -> None:
    """A board added to PX4 after this table was written must still be listed."""
    assert board_label("px4_fmu-v6x_default.px4") == "Pixhawk 6X (FMUv6X)"
    assert board_label("brand_new-board_default.px4") == "brand_new-board_default"


# ---------------------------------------------------------------------------
# URL derivation
# ---------------------------------------------------------------------------

def test_download_url_is_built_not_taken_from_the_catalogue(tmp_path: pathlib.Path) -> None:
    """A tampered cache must not be able to redirect a firmware download.

    The stored catalogue carries no URL at all; resolve() rebuilds it from the
    fixed release template, so the worst a poisoned cache can do is name a
    different asset in the PX4 repository.
    """
    catalog = FirmwareCatalog(str(tmp_path))
    stored = _parse_releases([_release("v1.17.0", ["px4_fmu-v6x_default.px4"])])
    assert "url" not in stored[0]["boards"][0]

    # Write a cache that tries to smuggle a URL in.
    poisoned = json.loads(json.dumps({"releases": stored}))
    poisoned["releases"][0]["boards"][0]["url"] = "https://evil.example/pwn.px4"
    (tmp_path / "catalog.json").write_text(json.dumps(poisoned), encoding="utf-8")

    entry = catalog.resolve("v1.17.0", "px4_fmu-v6x_default.px4")
    assert entry is not None
    assert entry["url"] == asset_url("v1.17.0", "px4_fmu-v6x_default.px4")
    assert "evil.example" not in entry["url"]


def test_asset_url_targets_an_allowed_host() -> None:
    from urllib.parse import urlparse
    host = urlparse(asset_url("v1.17.0", "px4_fmu-v6x_default.px4")).hostname
    assert host in ALLOWED_HOSTS


def test_resolve_refuses_a_bootloader_even_if_the_cache_lists_one(
    tmp_path: pathlib.Path,
) -> None:
    catalog = FirmwareCatalog(str(tmp_path))
    (tmp_path / "catalog.json").write_text(json.dumps({"releases": [{
        "tag": "v1.17.0",
        "boards": [{"name": "px4_fmu-v6x_bootloader.px4", "label": "x", "size": 1}],
    }]}), encoding="utf-8")
    assert catalog.resolve("v1.17.0", "px4_fmu-v6x_bootloader.px4") is None


def test_download_refuses_a_host_outside_the_allow_list(tmp_path: pathlib.Path) -> None:
    catalog = FirmwareCatalog(str(tmp_path))
    with pytest.raises(ValueError, match="unexpected host"):
        catalog.download({"name": "x.px4", "url": "https://evil.example/x.px4"})


def test_download_refuses_a_non_firmware_name(tmp_path: pathlib.Path) -> None:
    catalog = FirmwareCatalog(str(tmp_path))
    with pytest.raises(ValueError, match="not a PX4 firmware image"):
        catalog.download({"name": "x.exe", "url": asset_url("v1", "x.exe")})


# ---------------------------------------------------------------------------
# Offline behaviour
# ---------------------------------------------------------------------------

def test_catalog_serves_the_cache_without_touching_the_network(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Opening the Firmware page in the field must not need the internet."""
    catalog = FirmwareCatalog(str(tmp_path))
    stored = _parse_releases([_release("v1.17.0", ["px4_fmu-v6x_default.px4"])])
    (tmp_path / "catalog.json").write_text(json.dumps({"releases": stored}), encoding="utf-8")

    def explode(*_args, **_kwargs):
        raise AssertionError("the catalogue went to the network without being asked")

    monkeypatch.setattr("corvus.firmware_catalog.urllib.request.urlopen", explode)
    data = catalog.catalog(refresh=False)
    assert [r["tag"] for r in data["releases"]] == ["v1.17.0"]
    assert data["error"] == ""


def test_a_failed_refresh_returns_the_cache_and_says_why(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """No network is the normal state in the field, not an error page."""
    catalog = FirmwareCatalog(str(tmp_path))
    stored = _parse_releases([_release("v1.17.0", ["px4_fmu-v6x_default.px4"])])
    (tmp_path / "catalog.json").write_text(json.dumps({"releases": stored}), encoding="utf-8")

    def boom(*_args, **_kwargs):
        raise OSError("no route to host")

    monkeypatch.setattr("corvus.firmware_catalog.urllib.request.urlopen", boom)
    data = catalog.catalog(refresh=True)
    assert [r["tag"] for r in data["releases"]] == ["v1.17.0"], "cache still served"
    assert "could not reach" in data["error"]


def test_missing_cache_is_empty_not_an_exception(tmp_path: pathlib.Path) -> None:
    catalog = FirmwareCatalog(str(tmp_path / "does-not-exist"))
    assert catalog.load_cached() == []
    assert catalog.list_cached_images() == []
    assert catalog.cached_path("px4_fmu-v6x_default.px4") is None


def test_corrupt_cache_is_empty_not_an_exception(tmp_path: pathlib.Path) -> None:
    (tmp_path / "catalog.json").write_text("{not json", encoding="utf-8")
    assert FirmwareCatalog(str(tmp_path)).load_cached() == []


def test_cached_images_are_listed_and_marked_in_the_catalogue(
    tmp_path: pathlib.Path,
) -> None:
    """"Already downloaded" is the difference between a wait and no wait."""
    catalog = FirmwareCatalog(str(tmp_path))
    stored = _parse_releases([_release("v1.17.0", [
        "px4_fmu-v6x_default.px4", "px4_fmu-v5_default.px4",
    ])])
    (tmp_path / "catalog.json").write_text(json.dumps({"releases": stored}), encoding="utf-8")
    (tmp_path / "px4_fmu-v6x_default.px4").write_bytes(b"x" * 16)

    data = catalog.catalog(refresh=False)
    marked = {b["name"]: b["cached"] for b in data["releases"][0]["boards"]}
    assert marked == {"px4_fmu-v6x_default.px4": True, "px4_fmu-v5_default.px4": False}
    assert [c["name"] for c in data["cached"]] == ["px4_fmu-v6x_default.px4"]


def test_a_cached_image_is_returned_without_a_download(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    catalog = FirmwareCatalog(str(tmp_path))
    (tmp_path / "px4_fmu-v6x_default.px4").write_bytes(b"cached-image")

    def explode(*_args, **_kwargs):
        raise AssertionError("re-downloaded an image that was already cached")

    monkeypatch.setattr("corvus.firmware_catalog.urllib.request.urlopen", explode)
    entry = {"name": "px4_fmu-v6x_default.px4",
             "url": asset_url("v1.17.0", "px4_fmu-v6x_default.px4")}
    assert catalog.download(entry) == b"cached-image"


def test_a_cancelled_download_raises_rather_than_flashing_a_partial_image(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    catalog = FirmwareCatalog(str(tmp_path))
    cancel = threading.Event()
    cancel.set()

    class _Resp:
        headers = {"Content-Length": "8"}

        def read(self, _n: int) -> bytes:
            return b"12345678"

        def __enter__(self):
            return self

        def __exit__(self, *_exc) -> None:
            return None

    monkeypatch.setattr("corvus.firmware_catalog.urllib.request.urlopen",
                        lambda *_a, **_k: _Resp())
    entry = {"name": "px4_fmu-v6x_default.px4",
             "url": asset_url("v1.17.0", "px4_fmu-v6x_default.px4")}
    with pytest.raises(ValueError, match="cancelled"):
        catalog.download(entry, cancel=cancel)
    assert not (tmp_path / "px4_fmu-v6x_default.px4").exists(), "nothing partial cached"
