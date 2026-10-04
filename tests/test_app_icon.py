"""The desktop wrapper's Dock / taskbar icon selection.

``corvus.app_icon`` imports cleanly without PySide6 (its Qt imports are all
function-local), so the helpers that decide how the app draws the mark it
hands the operating system are tested headless — no QApplication, no display.
The painting itself needs Qt and is therefore not exercised here; what is
exercised is every decision that feeds it.

The switches are icon-only by contract: they must not read, or reach,
anything else in the config.
"""
from __future__ import annotations

import os

import corvus.app_icon as icon
from corvus.config import CorvusConfig


def test_default_config_uses_the_normal_mark() -> None:
    """No ``ui`` key at all — the shipped white artwork, as before the switch."""
    assert icon.app_icon_inverted(CorvusConfig()) is False


def test_inverted_when_the_operator_asked_for_it() -> None:
    assert icon.app_icon_inverted(CorvusConfig(ui={"inverted_app_icon": True})) is True


def test_other_ui_keys_do_not_select_the_icon() -> None:
    """The interface size says nothing about the icon."""
    assert icon.app_icon_inverted(CorvusConfig(ui={"scale": 1.25})) is False


def test_non_bool_value_is_not_a_yes() -> None:
    """A hand-edited string must not read as on."""
    assert icon.app_icon_inverted(CorvusConfig(ui={"inverted_app_icon": "true"})) is False


def test_missing_config_object_is_not_a_crash() -> None:
    """The icon sync runs on a timer and must survive a config-less server."""
    assert icon.app_icon_inverted(None) is False


def test_backplate_is_off_by_default() -> None:
    """A config without the key keeps the bare silhouette the builds ship."""
    assert icon.app_icon_backplate(CorvusConfig()) is False


def test_backplate_when_the_operator_asked_for_it() -> None:
    assert icon.app_icon_backplate(CorvusConfig(ui={"app_icon_backplate": True})) is True


def test_backplate_needs_a_real_bool() -> None:
    assert icon.app_icon_backplate(CorvusConfig(ui={"app_icon_backplate": "yes"})) is False


def test_backplate_survives_a_config_less_server() -> None:
    assert icon.app_icon_backplate(None) is False


def test_the_two_switches_are_independent() -> None:
    """Inversion picks the mark, the backplate gives it a ground."""
    cfg = CorvusConfig(ui={"app_icon_backplate": True})
    assert icon.app_icon_backplate(cfg) is True
    assert icon.app_icon_inverted(cfg) is False

    cfg = CorvusConfig(ui={"inverted_app_icon": True})
    assert icon.app_icon_inverted(cfg) is True
    assert icon.app_icon_backplate(cfg) is False


def test_the_plate_takes_the_side_the_mark_does_not() -> None:
    """The plate exists for contrast, so it is never the mark's own shade."""
    assert icon.app_icon_plate_color(False) == icon._PLATE_DARK    # white mark
    assert icon.app_icon_plate_color(True) == icon._PLATE_LIGHT    # black mark
    assert icon._PLATE_DARK != icon._PLATE_LIGHT


def test_both_icon_files_ship_beside_the_app() -> None:
    """Both cuts are real files under ``assets/``, so neither path is a dead end.

    They travel with every artifact (the layout contract keeps ``assets/`` a
    sibling of ``corvus/``), which is what lets the switch work in a bundle.
    """
    normal = icon.app_icon_path(False)
    inverted = icon.app_icon_path(True)
    assert os.path.basename(normal) == "CorvusGCS_logo.png"
    assert os.path.basename(inverted) == "CorvusGCS_logo_inverted.png"
    assert normal != inverted
    assert os.path.isfile(normal)
    assert os.path.isfile(inverted)


def test_an_appimage_answers_to_its_desktop_entry() -> None:
    """build-appimage.sh's entry says StartupWMClass=corvus-gcs."""
    assert icon.desktop_id({"APPIMAGE": "/opt/Corvus_GCS.AppImage"}) == "corvus-gcs"


def test_a_source_checkout_has_a_name_of_its_own() -> None:
    """So a dev entry and an installed AppImage never claim each other's windows."""
    assert icon.desktop_id({}) == "corvus-gcs-dev"


def test_the_appimage_entry_and_the_app_agree_on_the_id() -> None:
    """The window id and the entry's StartupWMClass are one name, or no dock matches."""
    script = os.path.join(icon.REPO_ROOT, "build-appimage.sh")
    with open(script, encoding="utf-8") as fh:
        text = fh.read()
    wanted = icon.desktop_id({"APPIMAGE": "x"})
    assert f"StartupWMClass={wanted}\n" in text
    assert f"Icon={wanted}\n" in text


def test_linux_names_the_x11_instance() -> None:
    argv = icon.qt_argv(["corvus/icon.py", "8000"], "corvus-gcs-dev", "linux")
    assert argv == ["corvus/icon.py", "8000", "-name", "corvus-gcs-dev"]


def test_other_platforms_get_argv_unchanged() -> None:
    for platform in ("darwin", "win32"):
        argv = ["icon.py"]
        out = icon.qt_argv(argv, "corvus-gcs", platform)
        assert out == ["icon.py"]
        assert out is not argv


def test_packaged_icons_are_left_to_the_platform() -> None:
    assert icon.ships_own_icon("darwin", False, "/A/Corvus GCS.app/Contents/Resources/corvus/icon.py")
    assert icon.ships_own_icon("win32", True, r"C:\Corvus GCS\_internal\corvus\icon.py")


def test_qt_sets_the_icon_everywhere_else() -> None:
    """Linux needs the window icon even packaged; a checkout has nothing to show."""
    assert not icon.ships_own_icon("linux", False, "/tmp/.mount_x/corvus/icon.py")
    assert not icon.ships_own_icon("darwin", False, "/Users/me/Developer/CorvusGCS/corvus/icon.py")
    assert not icon.ships_own_icon("win32", False, r"C:\src\CorvusGCS\corvus\icon.py")


def test_the_dev_entry_and_the_app_agree_on_the_id() -> None:
    """run.sh --desktop-entry writes the id a source checkout's window carries."""
    with open(os.path.join(icon.REPO_ROOT, "run.sh"), encoding="utf-8") as fh:
        text = fh.read()
    assert f'"StartupWMClass={icon.desktop_id({})}"' in text
