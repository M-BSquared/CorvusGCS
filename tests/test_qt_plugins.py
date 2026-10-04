"""Qt plugins macOS has flagged hidden, loaded through links (corvus.qt_plugins).

Under ``.venv`` in a checkout a sync agent manages every file ends up flagged
hidden, and Qt then finds no platform plugin and aborts. The links are made
headless here: a plugin folder of plain files, with the flag stood in for, plus
one test with a real flag where the file system has them.
"""
from __future__ import annotations

import os
import re
import stat
import sys
from pathlib import Path

import pytest

from corvus import qt_plugins
from corvus.qt_plugins import flagged_hidden, links_dir, visible_plugin_dir

ROOT = Path(__file__).resolve().parents[1]


def _plugins(tmp_path: Path) -> Path:
    root = tmp_path / ".venv" / "PySide6" / "Qt" / "plugins"
    for rel in ("platforms/libqcocoa.dylib", "platforms/libqminimal.dylib",
                "styles/libqmacstyle.dylib", "imageformats/libqjpeg.dylib"):
        (root / rel).parent.mkdir(parents=True, exist_ok=True)
        (root / rel).write_bytes(b"plugin " + rel.encode())
    return root


def _all_hidden(_path: Path) -> bool:
    return True


def _listing(links: Path) -> dict[str, str]:
    # Windows reads a link back with the \\?\ long path prefix it was not made with.
    return {p.relative_to(links).as_posix(): os.readlink(p).removeprefix("\\\\?\\")
            for p in links.rglob("*") if p.is_symlink()}


def test_nothing_happens_while_qt_can_see_its_plugins(tmp_path):
    root = _plugins(tmp_path)
    assert visible_plugin_dir(root, base=tmp_path / "tmp", hidden=lambda _p: False) is None
    assert not (tmp_path / "tmp").exists()


def test_nothing_happens_without_platform_plugins(tmp_path):
    assert visible_plugin_dir(tmp_path / "nowhere", base=tmp_path, hidden=_all_hidden) is None


def test_hidden_plugins_are_linked_folder_by_folder(tmp_path):
    root = _plugins(tmp_path)
    links = Path(visible_plugin_dir(root, base=tmp_path / "tmp", hidden=_all_hidden))
    assert links == links_dir(root, tmp_path / "tmp")
    assert _listing(links) == {
        "imageformats/libqjpeg.dylib": str(root / "imageformats" / "libqjpeg.dylib"),
        "platforms/libqcocoa.dylib": str(root / "platforms" / "libqcocoa.dylib"),
        "platforms/libqminimal.dylib": str(root / "platforms" / "libqminimal.dylib"),
        "styles/libqmacstyle.dylib": str(root / "styles" / "libqmacstyle.dylib"),
    }
    assert (links / "platforms" / "libqcocoa.dylib").read_bytes() == b"plugin platforms/libqcocoa.dylib"


def test_a_second_run_keeps_the_same_links(tmp_path):
    root = _plugins(tmp_path)
    first = visible_plugin_dir(root, base=tmp_path / "tmp", hidden=_all_hidden)
    before = _listing(Path(first))
    second = visible_plugin_dir(root, base=tmp_path / "tmp", hidden=_all_hidden)
    assert second == first
    assert _listing(Path(second)) == before


def test_stale_links_are_brought_up_to_date(tmp_path):
    """A plugin gone, a link pointing elsewhere, a category dropped: all corrected."""
    root = _plugins(tmp_path)
    links = Path(visible_plugin_dir(root, base=tmp_path / "tmp", hidden=_all_hidden))
    elsewhere = tmp_path / "other" / "libqcocoa.dylib"
    elsewhere.parent.mkdir()
    elsewhere.write_bytes(b"another Qt")
    (links / "platforms" / "libqcocoa.dylib").unlink()
    (links / "platforms" / "libqcocoa.dylib").symlink_to(elsewhere)
    (root / "platforms" / "libqminimal.dylib").unlink()
    for plugin in (root / "imageformats").iterdir():
        plugin.unlink()
    (root / "imageformats").rmdir()

    visible_plugin_dir(root, base=tmp_path / "tmp", hidden=_all_hidden)
    assert _listing(links) == {
        "platforms/libqcocoa.dylib": str(root / "platforms" / "libqcocoa.dylib"),
        "styles/libqmacstyle.dylib": str(root / "styles" / "libqmacstyle.dylib"),
    }
    assert not (links / "imageformats").exists()
    assert not [p for p in links.rglob(".*")], "no half-made link left behind"


def test_each_installation_gets_its_own_links(tmp_path):
    """Two venvs with the same Qt must not share links: one may be deleted."""
    one = _plugins(tmp_path / "one")
    two = _plugins(tmp_path / "two")
    assert links_dir(one, tmp_path) != links_dir(two, tmp_path)


def test_a_file_without_flags_is_not_hidden(tmp_path):
    plain = tmp_path / "libqcocoa.dylib"
    plain.write_bytes(b"x")
    assert flagged_hidden(plain) is False
    assert flagged_hidden(tmp_path / "missing") is False


@pytest.mark.skipif(not hasattr(os, "chflags") or not getattr(stat, "UF_HIDDEN", 0),
                    reason="the file system has no hidden flag")
def test_a_real_hidden_flag_is_read(tmp_path):
    root = _plugins(tmp_path)
    cocoa = root / "platforms" / "libqcocoa.dylib"
    try:
        os.chflags(cocoa, stat.UF_HIDDEN)
    except OSError:
        pytest.skip("this file system does not keep the flag")
    assert flagged_hidden(cocoa)
    links = Path(visible_plugin_dir(root, base=tmp_path / "tmp"))
    link = links / "platforms" / "libqcocoa.dylib"
    assert not os.lstat(link).st_flags & stat.UF_HIDDEN, "the link itself must not be flagged"


def test_the_links_never_reach_a_child_process():
    """A library path, not QT_PLUGIN_PATH: corvus.child_env never sees it."""
    source = (ROOT / "corvus" / "app.py").read_text(encoding="utf-8")
    body = source.split("def use_visible_qt_plugins", 1)[1].split("\ndef ", 1)[0]
    assert "QCoreApplication.addLibraryPath(links)" in body
    assert "QT_PLUGIN_PATH" not in body.replace("``QT_PLUGIN_PATH``", "")


def test_every_qapplication_the_app_makes_comes_after_the_links():
    """Qt loads its platform plugin in the QApplication constructor."""
    source = (ROOT / "corvus" / "app.py").read_text(encoding="utf-8")
    made = [m.start() for m in re.finditer(r"QApplication\((?:sys\.argv\)|qt_argv\(sys\.argv)", source)]
    assert len(made) == 2, "the startup failure dialog and the main window"
    for at in made:
        before = source[:at]
        function = before[before.rindex("\ndef "):]
        assert "use_visible_qt_plugins()" in function, function[:80]


def test_the_module_stays_stdlib_only():
    assert "PySide6" not in Path(qt_plugins.__file__).read_text(encoding="utf-8").split('"""', 2)[2]
    assert "corvus.qt_plugins" in sys.modules
