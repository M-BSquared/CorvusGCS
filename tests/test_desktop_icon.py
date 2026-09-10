"""Rewriting the icons a Linux desktop keeps for an AppImage.

``corvus.desktop_icon`` is stdlib-only on purpose — the image work is handed
in as a renderer — so the whole module is tested headless: no PyQt6, no
display, no AppImage. Every test builds a throwaway ``$HOME`` layout matching
what appimaged / AppImageLauncher actually write.

Two places are corrected: the launcher entry an integrator installed, and the
freedesktop thumbnail the file manager paints on the ``.AppImage`` file. The
property that matters most is the negative one — the rewrite must find the
icons of *this* AppImage and no others.
"""
from __future__ import annotations

import hashlib
import os
import sys

import pytest

from corvus import desktop_icon


# ---- fixtures ---------------------------------------------------------------

def _fake_home(tmp_path):
    """An XDG data home with the dirs an integrator writes into."""
    data = tmp_path / "data"
    (data / "applications").mkdir(parents=True)
    (data / "icons" / "hicolor" / "256x256" / "apps").mkdir(parents=True)
    (data / "icons" / "hicolor" / "128x128" / "apps").mkdir(parents=True)
    return {"XDG_DATA_HOME": str(data)}, data


def _appimage(tmp_path, name="Corvus_GCS-2026.09.09-x86_64.AppImage"):
    path = tmp_path / name
    path.write_bytes(b"not really an AppImage")
    return str(path)


def _entry(data, filename, exec_line, icon="appimagekit_corvus_corvus-gcs"):
    (data / "applications" / filename).write_text(
        "[Desktop Entry]\n"
        "Type=Application\n"
        "Name=Corvus GCS\n"
        f"Exec={exec_line}\n"
        f"Icon={icon}\n",
        encoding="utf-8",
    )


def _installed_icon(data, name, size="256x256", body=b"old-icon"):
    path = data / "icons" / "hicolor" / size / "apps" / f"{name}.png"
    path.write_bytes(body)
    return path


def _renderer(calls):
    """A stand-in for the Qt renderer: records the call, writes a marker."""
    def render(source, dest, size, text):
        calls.append((source, dest, size, dict(text)))
        with open(dest, "wb") as fh:
            fh.write(b"new-icon:" + os.path.basename(source).encode())
    return render


# ---- reading the integrated entry -------------------------------------------

def test_entry_that_launches_this_appimage_yields_its_icon(tmp_path) -> None:
    img = _appimage(tmp_path)
    assert desktop_icon.desktop_entry_icon(
        f"[Desktop Entry]\nExec={img} %U\nIcon=corvus-gcs\n", img) == "corvus-gcs"


def test_quoted_exec_still_matches(tmp_path) -> None:
    """AppImageLauncher quotes the path; a path with a space needs it."""
    img = _appimage(tmp_path, "Corvus GCS.AppImage")
    assert desktop_icon.desktop_entry_icon(
        f'[Desktop Entry]\nExec="{img}" %U\nIcon=corvus-gcs\n', img) == "corvus-gcs"


def test_wrapped_exec_still_matches(tmp_path) -> None:
    """The AppImage is not reliably argv[0], so every token is checked."""
    img = _appimage(tmp_path)
    assert desktop_icon.desktop_entry_icon(
        f"[Desktop Entry]\nExec=env FOO=1 {img}\nIcon=corvus-gcs\n", img) == "corvus-gcs"


def test_another_apps_entry_is_not_ours(tmp_path) -> None:
    """The whole safety of the rewrite rests on this returning None."""
    img = _appimage(tmp_path)
    other = _appimage(tmp_path, "SomethingElse.AppImage")
    assert desktop_icon.desktop_entry_icon(
        f"[Desktop Entry]\nExec={other}\nIcon=something-else\n", img) is None


def test_matching_name_without_a_matching_exec_is_not_ours(tmp_path) -> None:
    """A stale entry for an AppImage that has since moved must be left alone."""
    img = _appimage(tmp_path)
    assert desktop_icon.desktop_entry_icon(
        "[Desktop Entry]\nName=Corvus GCS\nExec=/opt/corvus/old\nIcon=corvus-gcs\n",
        img) is None


def test_action_group_icon_is_not_the_app_icon(tmp_path) -> None:
    """Parsing stops at the first action group, whose Icon is the action's."""
    img = _appimage(tmp_path)
    text = (f"[Desktop Entry]\nExec={img}\nIcon=corvus-gcs\n"
            "\n[Desktop Action New]\nName=New\nIcon=document-new\n")
    assert desktop_icon.desktop_entry_icon(text, img) == "corvus-gcs"


def test_localised_icon_key_is_ignored(tmp_path) -> None:
    img = _appimage(tmp_path)
    text = f"[Desktop Entry]\nExec={img}\nIcon[de]=raabe\nIcon=corvus-gcs\n"
    assert desktop_icon.desktop_entry_icon(text, img) == "corvus-gcs"


def test_commented_out_exec_does_not_claim_the_entry(tmp_path) -> None:
    img = _appimage(tmp_path)
    text = f"[Desktop Entry]\n#Exec={img}\nExec=/usr/bin/other\nIcon=corvus-gcs\n"
    assert desktop_icon.desktop_entry_icon(text, img) is None


def test_integrated_names_scan_only_the_user_applications_dir(tmp_path) -> None:
    env, data = _fake_home(tmp_path)
    img = _appimage(tmp_path)
    _entry(data, "appimagekit_corvus.desktop", f"{img} %U", icon="corvus-mark")
    _entry(data, "unrelated.desktop", "/usr/bin/gedit", icon="gedit")
    assert desktop_icon.integrated_icon_names(img, env) == ["corvus-mark"]


def test_two_entries_naming_one_icon_report_it_once(tmp_path) -> None:
    env, data = _fake_home(tmp_path)
    img = _appimage(tmp_path)
    _entry(data, "a.desktop", str(img), icon="corvus-mark")
    _entry(data, "b.desktop", f'"{img}" %U', icon="corvus-mark")
    assert desktop_icon.integrated_icon_names(img, env) == ["corvus-mark"]


def test_missing_applications_dir_is_not_an_error(tmp_path) -> None:
    env = {"XDG_DATA_HOME": str(tmp_path / "nothing-here")}
    assert desktop_icon.integrated_icon_names(_appimage(tmp_path), env) == []


# ---- locating the installed files -------------------------------------------

def test_every_installed_size_is_found(tmp_path) -> None:
    env, data = _fake_home(tmp_path)
    _installed_icon(data, "corvus-mark", "256x256")
    _installed_icon(data, "corvus-mark", "128x128")
    found = desktop_icon.icon_files(["corvus-mark"], env)
    assert len(found) == 2
    assert {os.path.basename(p) for p in found} == {"corvus-mark.png"}


def test_absolute_icon_key_is_taken_as_the_file(tmp_path) -> None:
    env, data = _fake_home(tmp_path)
    installed = _installed_icon(data, "corvus-mark")
    assert desktop_icon.icon_files([str(installed)], env) == [str(installed)]


def test_svg_targets_are_left_alone(tmp_path) -> None:
    """Overwriting an .svg with PNG bytes would break the icon outright."""
    env, data = _fake_home(tmp_path)
    scalable = data / "icons" / "hicolor" / "scalable" / "apps"
    scalable.mkdir(parents=True)
    (scalable / "corvus-mark.svg").write_text("<svg/>", encoding="utf-8")
    assert desktop_icon.icon_files(["corvus-mark"], env) == []


def test_a_name_nobody_installed_finds_nothing(tmp_path) -> None:
    env, _data = _fake_home(tmp_path)
    assert desktop_icon.icon_files(["corvus-mark"], env) == []


@pytest.mark.parametrize("path,expected", [
    ("/h/.local/share/icons/hicolor/256x256/apps/x.png", 256),
    ("/h/.local/share/icons/hicolor/48x48/apps/x.png", 48),
    ("/h/.local/share/icons/hicolor/scalable/apps/x.png", None),
    ("/h/.icons/x.png", None),
])
def test_nominal_size_comes_from_the_theme_directory(path, expected) -> None:
    assert desktop_icon.nominal_size(path) == expected


def test_non_square_size_directory_is_not_a_size() -> None:
    """``16x9`` is not a thing the icon spec produces; do not guess at it."""
    assert desktop_icon.nominal_size("/h/icons/hicolor/16x9/apps/x.png") is None


# ---- the rewrite ------------------------------------------------------------

def test_rewrite_replaces_every_installed_size(tmp_path) -> None:
    env, data = _fake_home(tmp_path)
    img = _appimage(tmp_path)
    _entry(data, "corvus.desktop", f"{img} %U", icon="corvus-mark")
    big = _installed_icon(data, "corvus-mark", "256x256")
    small = _installed_icon(data, "corvus-mark", "128x128")
    source = tmp_path / "CorvusGCS_logo_inverted.png"
    source.write_bytes(b"source")

    calls: list = []
    written = desktop_icon.sync_integrated_icon(
        str(source), _renderer(calls), appimage=img, env=env)

    assert sorted(written) == sorted([str(big), str(small)])
    assert big.read_bytes() == b"new-icon:CorvusGCS_logo_inverted.png"
    assert all(text == {} for _s, _d, _size, text in calls)
    # Each target is rendered at the size its own directory promises.
    assert sorted(size for _s, _d, size, _t in calls) == [128, 256]


def test_rewrite_leaves_a_stranger_untouched(tmp_path) -> None:
    env, data = _fake_home(tmp_path)
    img = _appimage(tmp_path)
    _entry(data, "corvus.desktop", str(img), icon="corvus-mark")
    _installed_icon(data, "corvus-mark")
    stranger = _installed_icon(data, "gedit", body=b"gedit-icon")
    source = tmp_path / "CorvusGCS_logo.png"
    source.write_bytes(b"source")

    desktop_icon.sync_integrated_icon(str(source), _renderer([]),
                                      appimage=img, env=env)
    assert stranger.read_bytes() == b"gedit-icon"


def test_a_failing_render_leaves_the_old_icon_in_place(tmp_path) -> None:
    """A half-written PNG in the launcher is worse than a stale one."""
    env, data = _fake_home(tmp_path)
    img = _appimage(tmp_path)
    _entry(data, "corvus.desktop", str(img), icon="corvus-mark")
    installed = _installed_icon(data, "corvus-mark", body=b"old-icon")
    source = tmp_path / "CorvusGCS_logo.png"
    source.write_bytes(b"source")

    def boom(_source, _dest, _size, _text):
        raise RuntimeError("no Qt here")

    assert desktop_icon.sync_integrated_icon(
        str(source), boom, appimage=img, env=env) == []
    assert installed.read_bytes() == b"old-icon"
    # …and nothing temporary is left behind next to it.
    leftovers = [n for n in os.listdir(installed.parent) if n.startswith(".corvus-icon-")]
    assert leftovers == []


def test_missing_artwork_is_a_no_op(tmp_path) -> None:
    env, data = _fake_home(tmp_path)
    img = _appimage(tmp_path)
    _entry(data, "corvus.desktop", str(img), icon="corvus-mark")
    installed = _installed_icon(data, "corvus-mark", body=b"old-icon")

    assert desktop_icon.sync_integrated_icon(
        str(tmp_path / "gone.png"), _renderer([]), appimage=img, env=env) == []
    assert installed.read_bytes() == b"old-icon"


def test_nothing_integrated_is_a_no_op(tmp_path) -> None:
    env, _data = _fake_home(tmp_path)
    calls: list = []
    assert desktop_icon.sync_integrated_icon(
        __file__, _renderer(calls), appimage=_appimage(tmp_path), env=env) == []
    assert calls == []


# ---- the platform gate ------------------------------------------------------

def test_no_appimage_env_means_no_launcher_to_correct() -> None:
    assert desktop_icon.appimage_path({}) is None


def test_appimage_env_pointing_nowhere_is_ignored(tmp_path) -> None:
    env = {"APPIMAGE": str(tmp_path / "not-here.AppImage")}
    assert desktop_icon.appimage_path(env) is None


@pytest.mark.skipif(sys.platform.startswith("linux"),
                    reason="the gate only bites off Linux")
def test_other_platforms_never_report_an_appimage(tmp_path) -> None:
    """macOS keeps its signed .icns and Windows its embedded .ico."""
    env = {"APPIMAGE": _appimage(tmp_path)}
    assert desktop_icon.appimage_path(env) is None


@pytest.mark.skipif(not sys.platform.startswith("linux"),
                    reason="only Linux resolves $APPIMAGE")
def test_linux_resolves_the_appimage_env(tmp_path) -> None:
    img = _appimage(tmp_path)
    assert desktop_icon.appimage_path({"APPIMAGE": img}) == os.path.realpath(img)


def test_sync_without_an_appimage_touches_nothing(tmp_path) -> None:
    """The default detection path: a source checkout has no launcher entry."""
    env, data = _fake_home(tmp_path)
    env["APPIMAGE"] = ""
    installed = _installed_icon(data, "corvus-mark", body=b"old-icon")
    assert desktop_icon.sync_integrated_icon(__file__, _renderer([]), env=env) == []
    assert installed.read_bytes() == b"old-icon"


# ---- the thumbnail a file manager paints on the .AppImage file ---------------

def _thumb_home(tmp_path):
    cache = tmp_path / "cache"
    cache.mkdir()
    return {"XDG_CACHE_HOME": str(cache)}, cache


def test_thumbnail_name_is_the_md5_of_the_file_uri(tmp_path) -> None:
    """The spec keys a thumbnail on the hash of the URI, nothing else."""
    img = _appimage(tmp_path)
    expected = hashlib.md5(("file://" + img).encode("utf-8")).hexdigest() + ".png"
    assert desktop_icon.thumbnail_name(img) == expected


def test_file_uri_escapes_the_way_glib_does() -> None:
    """A hash off by one byte is a thumbnail the desktop never finds.

    GLib keeps the sub-delimiters plus ``:@/`` unescaped in a path and
    percent-encodes the rest, which is where Python's own default differs.
    """
    assert desktop_icon.file_uri("/tmp/a b") == "file:///tmp/a%20b"
    assert desktop_icon.file_uri("/tmp/a+b,c;d=e") == "file:///tmp/a+b,c;d=e"
    assert desktop_icon.file_uri("/tmp/a#b") == "file:///tmp/a%23b"
    assert desktop_icon.file_uri("/tmp/Bö.AppImage") == "file:///tmp/B%C3%B6.AppImage"


def test_the_two_standard_tiers_are_always_written(tmp_path) -> None:
    env, cache = _thumb_home(tmp_path)
    targets = desktop_icon.thumbnail_targets(_appimage(tmp_path), env)
    tiers = {os.path.basename(os.path.dirname(p)): size for p, size in targets}
    assert tiers == {"normal": 128, "large": 256}
    assert all(p.startswith(str(cache)) for p, _size in targets)


def test_a_larger_tier_is_written_only_where_it_already_exists(tmp_path) -> None:
    """A HiDPI desktop reads x-large first; a stale entry there would win."""
    env, cache = _thumb_home(tmp_path)
    (cache / "thumbnails" / "x-large").mkdir(parents=True)
    targets = desktop_icon.thumbnail_targets(_appimage(tmp_path), env)
    tiers = {os.path.basename(os.path.dirname(p)): size for p, size in targets}
    assert tiers == {"normal": 128, "large": 256, "x-large": 512}


def test_thumbnail_carries_the_keys_the_spec_validates(tmp_path) -> None:
    env, _cache = _thumb_home(tmp_path)
    img = _appimage(tmp_path)
    source = tmp_path / "CorvusGCS_logo.png"
    source.write_bytes(b"source")

    calls: list = []
    written = desktop_icon.sync_appimage_thumbnail(
        str(source), _renderer(calls), appimage=img, env=env)

    assert len(written) == 2
    for _s, _d, _size, text in calls:
        assert text["Thumb::URI"] == desktop_icon.file_uri(img)
        assert text["Thumb::MTime"] == str(int(os.path.getmtime(img)))


def test_thumbnail_mtime_is_the_appimage_not_the_artwork(tmp_path) -> None:
    """MTime must describe the file being previewed, or it reads as stale."""
    env, _cache = _thumb_home(tmp_path)
    img = _appimage(tmp_path)
    os.utime(img, (1_600_000_000, 1_600_000_000))
    source = tmp_path / "CorvusGCS_logo.png"
    source.write_bytes(b"source")

    calls: list = []
    desktop_icon.sync_appimage_thumbnail(str(source), _renderer(calls),
                                         appimage=img, env=env)
    assert {t["Thumb::MTime"] for _s, _d, _size, t in calls} == {"1600000000"}


def test_thumbnail_dirs_are_created_private(tmp_path) -> None:
    """A thumbnail can expose a file its owner never shared; 0700 per spec."""
    env, cache = _thumb_home(tmp_path)
    img = _appimage(tmp_path)
    source = tmp_path / "CorvusGCS_logo.png"
    source.write_bytes(b"source")

    written = desktop_icon.sync_appimage_thumbnail(
        str(source), _renderer([]), appimage=img, env=env)

    normal = cache / "thumbnails" / "normal"
    assert normal.is_dir()
    assert oct(normal.stat().st_mode & 0o777) == "0o700"
    assert oct(os.stat(written[0]).st_mode & 0o777) == "0o600"


def test_a_failed_thumbnail_marker_is_cleared(tmp_path) -> None:
    """A leftover fail/ marker makes the file manager ignore our PNG entirely."""
    env, cache = _thumb_home(tmp_path)
    img = _appimage(tmp_path)
    fail = cache / "thumbnails" / "fail" / "gnome-thumbnail-factory"
    fail.mkdir(parents=True)
    marker = fail / desktop_icon.thumbnail_name(img)
    marker.write_bytes(b"")
    source = tmp_path / "CorvusGCS_logo.png"
    source.write_bytes(b"source")

    desktop_icon.sync_appimage_thumbnail(str(source), _renderer([]),
                                         appimage=img, env=env)
    assert not marker.exists()


def test_a_stranger_s_failed_marker_survives(tmp_path) -> None:
    env, cache = _thumb_home(tmp_path)
    img = _appimage(tmp_path)
    fail = cache / "thumbnails" / "fail" / "gnome-thumbnail-factory"
    fail.mkdir(parents=True)
    stranger = fail / "0123456789abcdef0123456789abcdef.png"
    stranger.write_bytes(b"")
    source = tmp_path / "CorvusGCS_logo.png"
    source.write_bytes(b"source")

    desktop_icon.sync_appimage_thumbnail(str(source), _renderer([]),
                                         appimage=img, env=env)
    assert stranger.exists()


def test_a_failing_thumbnail_render_leaves_nothing_behind(tmp_path) -> None:
    env, cache = _thumb_home(tmp_path)
    img = _appimage(tmp_path)
    source = tmp_path / "CorvusGCS_logo.png"
    source.write_bytes(b"source")

    def boom(_source, _dest, _size, _text):
        raise RuntimeError("no Qt here")

    assert desktop_icon.sync_appimage_thumbnail(
        str(source), boom, appimage=img, env=env) == []
    normal = cache / "thumbnails" / "normal"
    assert list(normal.iterdir()) == []


def test_no_appimage_means_no_thumbnail_written(tmp_path) -> None:
    """A source checkout previews nothing; the cache must stay untouched."""
    env, cache = _thumb_home(tmp_path)
    env["APPIMAGE"] = ""
    calls: list = []
    assert desktop_icon.sync_appimage_thumbnail(__file__, _renderer(calls),
                                                env=env) == []
    assert calls == []
    assert not (cache / "thumbnails").exists()


def test_missing_artwork_writes_no_thumbnail(tmp_path) -> None:
    env, cache = _thumb_home(tmp_path)
    img = _appimage(tmp_path)
    assert desktop_icon.sync_appimage_thumbnail(
        str(tmp_path / "gone.png"), _renderer([]), appimage=img, env=env) == []
    assert not (cache / "thumbnails").exists()


def test_an_appimage_that_vanished_is_not_a_crash(tmp_path) -> None:
    """The mtime read is the one call here that can lose its file mid-run."""
    env, _cache = _thumb_home(tmp_path)
    source = tmp_path / "CorvusGCS_logo.png"
    source.write_bytes(b"source")
    assert desktop_icon.sync_appimage_thumbnail(
        str(source), _renderer([]),
        appimage=str(tmp_path / "never-existed.AppImage"), env=env) == []
