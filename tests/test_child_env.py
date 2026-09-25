"""What a program Corvus starts, but that is not part of Corvus, inherits.

The launchers of the packaged builds point PYTHONHOME, PYTHONPATH, the library
path and the Qt paths into the bundle. :mod:`corvus.child_env` takes exactly
those entries out again, and nothing the operator had set. The layouts below
are the real ones: build-appimage.sh's AppRun, build-macos-app.sh's launcher,
and a run from a checkout.
"""
from __future__ import annotations

import os

from corvus import child_env as ce
from corvus.child_env import QPA_DEFAULT_ENV, bundle_dirs, child_env


def _p(*parts: str) -> str:
    """An absolute path on this platform, whatever it runs the tests on."""
    return os.path.abspath(os.path.join(os.sep, *parts))


def _join(*paths: str) -> str:
    return os.pathsep.join(paths)


MOUNT = _p("tmp", ".mount_Corvus1")
APP = _p("Applications", "Corvus GCS.app")
APP_ROOT = os.path.join(APP, "Contents", "Resources", "app")
APP_PY = os.path.join(APP, "Contents", "Resources", "python")
CHECKOUT = _p("home", "pilot", "CorvusGCS")


def test_the_appimage_launcher_variables_do_not_reach_a_child():
    site = os.path.join(MOUNT, "usr", "lib", "python3.12", "site-packages")
    qt = os.path.join(site, "PyQt6", "Qt6")
    ros = _p("opt", "ros", "jazzy", "lib")
    env = {
        "HOME": _p("home", "pilot"),
        "PATH": _join(_p("usr", "bin"), _p("bin")),
        "PYTHONHOME": os.path.join(MOUNT, "usr"),
        "PYTHONPATH": _join(MOUNT, site),
        "LD_LIBRARY_PATH": _join(os.path.join(qt, "lib"), os.path.join(MOUNT, "usr", "lib"), ros),
        "QT_PLUGIN_PATH": os.path.join(qt, "plugins"),
        "QT_QPA_PLATFORM_PLUGIN_PATH": os.path.join(qt, "plugins"),
        "QTWEBENGINE_RESOURCES_PATH": os.path.join(qt, "resources"),
        "QTWEBENGINE_CHROMIUM_FLAGS": "--no-sandbox --disable-gpu-sandbox",
        "APPDIR": MOUNT,
        "APPIMAGE": _p("home", "pilot", "Corvus_GCS-x86_64.AppImage"),
        "ARGV0": "Corvus_GCS-x86_64.AppImage",
        "OWD": _p("home", "pilot"),
    }
    out = child_env(env, root=MOUNT, prefixes=(os.path.join(MOUNT, "usr"),), meipass="")
    assert out == {
        "HOME": env["HOME"],
        "PATH": env["PATH"],
        # The operator's own entry survives; the bundle's two are gone.
        "LD_LIBRARY_PATH": ros,
    }


def test_the_macos_launcher_variables_do_not_reach_a_child():
    site = os.path.join(APP_PY, "lib", "python3.12", "site-packages")
    env = {
        "HOME": _p("Users", "pilot"),
        "PYTHONHOME": APP_PY,
        "PYTHONPATH": _join(APP_ROOT, site),
        "PYTHONDONTWRITEBYTECODE": "1",
        "QTWEBENGINE_CHROMIUM_FLAGS": "--enable-webgl",
        "__PYVENV_LAUNCHER__": os.path.join(APP_PY, "bin", "python3"),
    }
    out = child_env(env, root=APP_ROOT, prefixes=(APP_PY,), meipass="")
    assert out == {"HOME": env["HOME"], "PYTHONDONTWRITEBYTECODE": "1"}


def test_a_run_from_a_checkout_keeps_what_the_operator_set():
    env = {
        "PYTHONPATH": _p("home", "pilot", "tools"),
        "LD_LIBRARY_PATH": _p("usr", "local", "lib"),
        "QT_PLUGIN_PATH": _p("usr", "lib", "qt6", "plugins"),
        "PATH": _p("usr", "bin"),
    }
    assert child_env(env, root=CHECKOUT, prefixes=(_p("usr"),), meipass="") == env


def test_pythonhome_is_dropped_only_when_it_is_this_interpreters():
    base = {"PATH": _p("usr", "bin")}
    ours = child_env(dict(base, PYTHONHOME=_p("opt", "py312")), root=CHECKOUT,
                     prefixes=(_p("opt", "py312"),), meipass="")
    theirs = child_env(dict(base, PYTHONHOME=_p("opt", "other")), root=CHECKOUT,
                       prefixes=(_p("opt", "py312"),), meipass="")
    assert "PYTHONHOME" not in ours
    assert theirs["PYTHONHOME"] == _p("opt", "other")


def test_the_qt_platform_is_dropped_only_when_corvus_chose_it():
    chosen = {"QT_QPA_PLATFORM": "xcb;wayland", QPA_DEFAULT_ENV: "xcb;wayland"}
    assert child_env(chosen, root=CHECKOUT, prefixes=(), meipass="") == {}
    operator = {"QT_QPA_PLATFORM": "wayland", QPA_DEFAULT_ENV: "xcb;wayland"}
    assert child_env(operator, root=CHECKOUT, prefixes=(), meipass="") == {"QT_QPA_PLATFORM": "wayland"}


def test_the_windows_bootloader_variables_do_not_reach_a_child():
    internal = _p("Program Files", "Corvus GCS", "_internal")
    env = {
        "_PYI_APPLICATION_HOME_DIR": internal,
        "_PYI_PARENT_PROCESS_LEVEL": "1",
        "_MEIPASS2": internal,
        "QT_PLUGIN_PATH": os.path.join(internal, "PyQt6", "Qt6", "plugins"),
        "SystemRoot": _p("Windows"),
    }
    out = child_env(env, root=internal, prefixes=(), meipass=internal)
    assert out == {"SystemRoot": env["SystemRoot"]}


def test_an_appdir_that_is_not_ours_is_left_alone():
    """Corvus started from inside another AppImage's terminal: that one is not ours."""
    other = _p("tmp", ".mount_Other")
    env = {"APPDIR": other, "APPIMAGE": _p("home", "x.AppImage"),
           "LD_LIBRARY_PATH": os.path.join(other, "usr", "lib")}
    assert child_env(env, root=CHECKOUT, prefixes=(), meipass="") == env


def test_the_bundle_is_found_from_the_app_folder():
    assert os.path.normcase(APP) in bundle_dirs({}, APP_ROOT, "")
    assert MOUNT in bundle_dirs({"APPDIR": MOUNT}, MOUNT, "")
    assert bundle_dirs({}, CHECKOUT, "") == [CHECKOUT]


def test_the_callers_mapping_is_not_changed():
    env = {"PYTHONHOME": APP_PY, "QTWEBENGINE_CHROMIUM_FLAGS": "x"}
    before = dict(env)
    child_env(env, root=APP_ROOT, prefixes=(), meipass="")
    assert env == before


def test_the_default_reads_this_process():
    out = child_env()
    assert isinstance(out, dict)
    assert "QTWEBENGINE_CHROMIUM_FLAGS" not in out
    assert out.get("PATH") == os.environ.get("PATH")


def test_the_app_root_is_the_folder_holding_the_package():
    assert os.path.isfile(os.path.join(ce.app_root(), "corvus", "child_env.py"))
