"""The desktop wrapper's Dock / taskbar icon selection.

``corvus.app`` imports cleanly without PyQt6 (its Qt imports live inside
``main()``), so the two helpers that decide which cut of the mark the app
hands the operating system are tested headless — no QApplication, no display.

The switch is icon-only by contract: it must not read, or reach, anything
else in the config.
"""
from __future__ import annotations

import os

import corvus.app as app
from corvus.config import CorvusConfig


def test_default_config_uses_the_normal_mark() -> None:
    """No ``ui`` key at all — the shipped white artwork, as before the switch."""
    assert app.app_icon_inverted(CorvusConfig()) is False


def test_inverted_when_the_operator_asked_for_it() -> None:
    assert app.app_icon_inverted(CorvusConfig(ui={"inverted_app_icon": True})) is True


def test_other_ui_keys_do_not_select_the_icon() -> None:
    """The interface size says nothing about the icon."""
    assert app.app_icon_inverted(CorvusConfig(ui={"scale": 1.25})) is False


def test_non_bool_value_is_not_a_yes() -> None:
    """A hand-edited string must not read as on."""
    assert app.app_icon_inverted(CorvusConfig(ui={"inverted_app_icon": "true"})) is False


def test_missing_config_object_is_not_a_crash() -> None:
    """The icon sync runs on a timer and must survive a config-less server."""
    assert app.app_icon_inverted(None) is False


def test_both_icon_files_ship_beside_the_app() -> None:
    """Both cuts are real files under ``assets/``, so neither path is a dead end.

    They travel with every artifact (the layout contract keeps ``assets/`` a
    sibling of ``corvus/``), which is what lets the switch work in a bundle.
    """
    normal = app.app_icon_path(False)
    inverted = app.app_icon_path(True)
    assert os.path.basename(normal) == "CorvusGCS_logo.png"
    assert os.path.basename(inverted) == "CorvusGCS_logo_inverted.png"
    assert normal != inverted
    assert os.path.isfile(normal)
    assert os.path.isfile(inverted)
