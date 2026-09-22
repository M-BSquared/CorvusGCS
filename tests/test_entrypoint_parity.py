"""Parity tests for the two launchers' service wiring.

``corvus.app.start_backend`` (the packaged desktop app) and
``corvus.server.create_server`` (browser mode) build the same backend twice.
Every time one of them grows a service the other does not, the packaged build
ships a page that works in development and is dead in the ``.app``: it has
already happened to the flight-log service (an Analysis page that could list
no ULog) and to the firmware catalogue (a Firmware page that listed no PX4
release, because the desktop app built ``FlashService`` without one).

The per-service tests cover what each service does. Nothing pinned *which
entry point builds it*, which is the half that keeps breaking — so this file
pins the shared builders, and asserts both launchers go through them.

Stdlib-only at module scope: the AST checks read the two source files and need
neither pymavlink nor paramiko, so they still run in a minimal environment.
"""
from __future__ import annotations

import ast
import json
import os
import pathlib
from unittest.mock import MagicMock

import pytest

_CORVUS = pathlib.Path(__file__).resolve().parent.parent / "corvus"


def _function_def(source_path: pathlib.Path, name: str) -> ast.FunctionDef:
    """The top-level ``def name`` in *source_path*, parsed."""
    tree = ast.parse(source_path.read_text(encoding="utf-8"))
    for node in tree.body:
        if isinstance(node, ast.FunctionDef) and node.name == name:
            return node
    raise AssertionError(f"{source_path.name} has no top-level def {name}")


def _called_names(func: ast.FunctionDef) -> set[str]:
    """Every plain-name call inside *func* (``foo(...)``, not ``x.foo(...)``)."""
    return {
        node.func.id for node in ast.walk(func)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
    }


# ---------------------------------------------------------------------------
# 1. There is one backend, and the desktop app does not build a second one
# ---------------------------------------------------------------------------

# Every service create_server wires, and the class neither launcher may
# construct itself. A service that gets a builder belongs here.
_SHARED_SERVICES = [
    ("_build_flash_service", "FlashService"),
    ("_build_sik_service", "SikService"),
    ("_build_update_checker", "UpdateChecker"),
    ("_build_log_service", "LogService"),
    ("_build_geocoder", "Geocoder"),
]

# What building a backend looks like. None of it belongs in start_backend.
_BACKEND_CONSTRUCTION = {
    "VehicleStateStore", "MavlinkBridge", "SshBridge", "bind_server",
    "load_config", "apply_startup_connection", "build_autoconnect_session",
    "start_autoconnect_watcher", "ensure_user_plugins_dir",
}


def test_the_desktop_app_delegates_to_create_server():
    """``start_backend`` builds no backend of its own.

    It used to build the whole thing by hand — the same store, bridge, ssh,
    config, tile resources, flash, SiK, logs, forwarder and update checker as
    browser mode, each assigned twice — and that copy fell behind twice. The
    packaged app shipped an Analysis page that could list no ULog, and a
    Firmware page that listed no PX4 release. No test failed either time,
    because the code was correct in the mode the tests ran in.

    So the rule is not "keep the copy in step" any more, it is "there is no
    copy". What the desktop app may still do is RUN the backend — a thread of
    its own, because Qt owns the main one.
    """
    called = _called_names(_function_def(_CORVUS / "app.py", "start_backend"))
    assert "create_server" in called, (
        "start_backend must get its backend from create_server, not build one"
    )
    forbidden = sorted(
        name for name in called
        if name.startswith("_build_") or name in _BACKEND_CONSTRUCTION
    )
    assert not forbidden, (
        f"start_backend builds {forbidden} itself; that is the divergence "
        "delegating to create_server exists to make impossible"
    )


@pytest.mark.parametrize("builder, klass", _SHARED_SERVICES)
def test_create_server_builds_each_service_through_its_helper(builder, klass):
    """Browser mode — the one implementation — goes through the builders."""
    called = _called_names(_function_def(_CORVUS / "server.py", "create_server"))
    assert builder in called, f"create_server does not call {builder}"
    assert klass not in called, (
        f"create_server constructs {klass} directly; that is what {builder} is for"
    )


def test_create_server_calls_every_build_helper_server_defines():
    """No ``_build_*`` helper may exist that the one backend never calls.

    ``_build_tile_downloader`` is excluded: it is internal to
    ``_build_tile_resources``, not something an entry point calls.
    """
    tree = ast.parse((_CORVUS / "server.py").read_text(encoding="utf-8"))
    defined = {
        node.name for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name.startswith("_build_")
    } - {"_build_tile_downloader"}

    called = _called_names(_function_def(_CORVUS / "server.py", "create_server"))
    for helper in sorted(defined):
        assert helper in called, f"create_server does not call {helper}"


def test_the_shutdown_sequence_is_not_duplicated_either():
    """``_stop_all`` was 98 lines, character-identical in two files.

    Nine ordered teardown steps, each with a comment explaining why it must
    come before the next, in two places with nothing enforcing the mirror —
    and the order is load-bearing: the auto-connect watcher restarts the
    bridge, so a tick landing after ``mavlink.stop()`` re-opens the link the
    shutdown just closed.
    """
    for path in (_CORVUS / "app.py", _CORVUS.parent / "serve.py"):
        func = _function_def(path, "_stop_all")
        assert "stop_backend" in _called_names(func), (
            f"{path.name}:_stop_all must delegate to corvus.server.stop_backend"
        )
        # A delegate, not a copy with a call bolted onto the end of it.
        size = len(ast.unparse(func).splitlines())
        assert size < 20, (
            f"{path.name}:_stop_all is {size} lines; it should be a delegate, "
            "not a second copy of the sequence"
        )


# ---------------------------------------------------------------------------
# 2. The shared flash builder: catalogue present, and never fatal
# ---------------------------------------------------------------------------

def test_build_flash_service_roots_the_catalogue_at_the_configured_dir(tmp_path):
    pytest.importorskip("pymavlink")
    from corvus.server import _build_flash_service

    cfg = MagicMock()
    cfg.firmware_dir = str(tmp_path / "fw")
    flash = _build_flash_service(MagicMock(), MagicMock(), cfg)

    assert flash is not None
    assert flash.catalog is not None, "packaged builds ship without a catalogue again"
    assert flash.catalog.dir == str(tmp_path / "fw")


def test_build_flash_service_falls_back_to_the_default_firmware_dir(tmp_path):
    """An unset ``firmware_dir`` still gets a catalogue, not ``None``."""
    pytest.importorskip("pymavlink")
    from corvus.firmware_catalog import default_firmware_dir
    from corvus.server import _build_flash_service

    cfg = MagicMock()
    cfg.firmware_dir = ""
    flash = _build_flash_service(MagicMock(), MagicMock(), cfg)

    assert flash is not None and flash.catalog is not None
    assert flash.catalog.dir == default_firmware_dir()


def test_build_flash_service_never_raises_out_of_the_launcher(monkeypatch, caplog):
    """A broken flash service costs the firmware page, never the launch."""
    pytest.importorskip("pymavlink")
    import corvus.flash_service as flash_service
    from corvus.server import _build_flash_service

    def _boom(*args, **kwargs):
        raise RuntimeError("no serial stack here")

    monkeypatch.setattr(flash_service, "FlashService", _boom)
    cfg = MagicMock()
    cfg.firmware_dir = ""

    assert _build_flash_service(MagicMock(), MagicMock(), cfg) is None
    assert "flash service unavailable" in caplog.text


# ---------------------------------------------------------------------------
# 3. Browser mode honours the operator's tile_cache_dir
# ---------------------------------------------------------------------------

def test_create_server_honours_configured_tile_cache_dir(tmp_path, monkeypatch):
    """``config.tile_cache_dir`` beats the default in browser mode too.

    The desktop app has always honoured it. While ``create_server`` did not,
    an operator who pointed the cache at an external drive got it in one mode
    and not the other, and the two modes then read and wrote different
    ``.mbtiles`` files — an area downloaded in one was missing in the other.
    """
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    import corvus.server as srv

    configured = tmp_path / "external-ssd"
    fallback = tmp_path / "default"
    fallback.mkdir()
    monkeypatch.delenv("CORVUS_TILE_CACHE_DIR", raising=False)
    monkeypatch.setattr(srv, "default_cache_dir", lambda: str(fallback))

    cfg_path = tmp_path / "config.json"
    cfg_path.write_text(
        json.dumps({
            "mavlink_connection": "udp:127.0.0.1:9999",
            "tile_cache_dir": str(configured),
        }),
        encoding="utf-8",
    )

    # Record the dir each builder is handed, then build for real, so the test
    # pins both the argument and where the files end up.
    captured: dict[str, str] = {}
    real_tiles = srv._build_tile_resources
    real_buildings = srv._build_building_service

    def _spy_tiles(cache_dir):
        captured["tiles"] = cache_dir
        return real_tiles(cache_dir)

    def _spy_buildings(cache_dir):
        captured["buildings"] = cache_dir
        return real_buildings(cache_dir)

    monkeypatch.setattr(srv, "_build_tile_resources", _spy_tiles)
    monkeypatch.setattr(srv, "_build_building_service", _spy_buildings)
    # No link, no receive thread: create_server starts the bridge, and this
    # test is about where the tile caches land.
    from corvus.mavlink_bridge import MavlinkBridge
    monkeypatch.setattr(MavlinkBridge, "start", lambda self: None)

    server = srv.create_server(port=0, config_path=str(cfg_path))
    try:
        assert captured["tiles"] == str(configured)
        assert captured["buildings"] == str(configured)
        # And it is where the caches actually landed.
        assert list(configured.glob("*.mbtiles")), "no cache written to the configured dir"
        assert not list(fallback.glob("*.mbtiles")), "caches went to the default dir anyway"
    finally:
        try:
            server.mavlink.stop()
        except Exception:
            pass
        try:
            server.server_close()
        except Exception:
            pass


# ---------------------------------------------------------------------------
# 4. The shared tile-cache-dir resolver
# ---------------------------------------------------------------------------

def test_tile_cache_dir_expands_a_tilde(monkeypatch):
    """``~/tiles`` is a home-relative path, not a directory named ``~``."""
    pytest.importorskip("pymavlink")
    from corvus.server import _tile_cache_dir

    cfg = MagicMock()
    cfg.tile_cache_dir = "~/corvus-tiles"
    resolved = _tile_cache_dir(cfg)

    assert not resolved.startswith("~")
    assert resolved == str(pathlib.Path.home() / "corvus-tiles")


def test_tile_cache_dir_falls_back_to_the_default(monkeypatch, tmp_path):
    pytest.importorskip("pymavlink")
    import corvus.server as srv

    monkeypatch.setattr(srv, "default_cache_dir", lambda: str(tmp_path))
    cfg = MagicMock()
    cfg.tile_cache_dir = "   "
    assert srv._tile_cache_dir(cfg) == str(tmp_path)


def test_default_cache_dir_expands_a_tilde_in_the_env_override(monkeypatch):
    """The env hook serve.py/app.py set carries whatever was typed."""
    from corvus.tile_cache import default_cache_dir

    monkeypatch.setenv("CORVUS_TILE_CACHE_DIR", "~/corvus-tiles")
    assert default_cache_dir() == str(pathlib.Path.home() / "corvus-tiles")


# ---------------------------------------------------------------------------
# 5. QtWebEngine's Chromium switches
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("platform", ["linux", "linux2", "darwin", "win32"])
def test_chromium_flags_never_disable_the_software_fallback(platform):
    """The software rasterizer IS SwiftShader; asking for both was a no-op.

    The pair used to sit two lines apart, and the machine that needed the
    fallback most — no usable GPU driver, so a blank window or a MapLibre
    that refuses to initialise — is the one that lost it.
    """
    from corvus.app import chromium_flags

    flags = chromium_flags(platform)
    assert "--enable-unsafe-swiftshader" in flags
    assert "--disable-software-rasterizer" not in flags


@pytest.mark.parametrize("platform", ["darwin", "win32"])
def test_vulkan_is_not_requested_off_linux(platform):
    """macOS renders through Metal and Windows through ANGLE/D3D."""
    from corvus.app import chromium_flags

    flags = chromium_flags(platform)
    assert not [f for f in flags if "ulkan" in f], flags


@pytest.mark.parametrize("platform", ["linux", "linux2"])
def test_vulkan_is_requested_on_linux(platform):
    from corvus.app import chromium_flags

    flags = chromium_flags(platform)
    assert "--use-vulkan" in flags and "--enable-features=Vulkan" in flags


@pytest.mark.parametrize("platform", ["linux", "darwin", "win32"])
def test_chromium_flags_carry_no_invented_field_trial(platform):
    """--force-fieldtrials=GPUHardwareRendering/Default names nothing real."""
    from corvus.app import chromium_flags

    assert not [f for f in chromium_flags(platform) if f.startswith("--force-fieldtrials")]


@pytest.mark.parametrize("platform", ["linux", "darwin", "win32"])
def test_webgl_stays_on_every_platform(platform):
    """MapLibre is WebGL; without it there is no map at all."""
    from corvus.app import chromium_flags

    assert "--enable-webgl" in chromium_flags(platform)


@pytest.mark.parametrize("platform", ["darwin", "win32"])
def test_the_gpu_sandbox_is_kept_where_it_works(platform):
    """A real boundary, and this frontend loads operator-supplied plugins.

    ``--disable-gpu-sandbox`` was set on every platform and every machine,
    including ones whose drivers are fine. It earns its place only on the
    Linux systems where it is what makes the GPU stack come up at all.
    """
    from corvus.app import chromium_flags

    assert "--disable-gpu-sandbox" not in chromium_flags(platform)


@pytest.mark.parametrize("platform", ["linux", "linux2"])
def test_the_gpu_sandbox_is_still_dropped_where_it_blocks_startup(platform):
    from corvus.app import chromium_flags

    assert "--disable-gpu-sandbox" in chromium_flags(platform)


def test_the_operator_can_replace_the_whole_flag_list(monkeypatch):
    """setdefault, so an exported QTWEBENGINE_CHROMIUM_FLAGS wins outright."""
    import corvus.app as app_module

    source = pathlib.Path(app_module.__file__).read_text(encoding="utf-8")
    assert 'os.environ.setdefault("QTWEBENGINE_CHROMIUM_FLAGS"' in source, (
        "the flags must be a default the operator can override, not an "
        "assignment that silently discards what they set"
    )


def test_the_windows_taskbar_gets_its_own_identity():
    """Without an AppUserModelID, Windows shows the LAUNCHER's icon.

    The id was computed and thrown away. That undoes the .ico
    build-windows.ps1 cuts, and undoes corvus.desktop_icon on the one
    platform it does not otherwise cover.
    """
    from corvus.app import set_windows_app_id

    # Off Windows it is a no-op that reports itself as one, rather than
    # pretending to have set something.
    assert set_windows_app_id("corvus.gcs.test") is (os.name == "nt")

    source = pathlib.Path(_CORVUS / "app.py").read_text(encoding="utf-8")
    assert "SetCurrentProcessExplicitAppUserModelID" in source
    # Before the QApplication: Windows binds a window to whatever the id was
    # when the window was created.
    assert source.index("set_windows_app_id(") < source.index("app = QApplication(sys.argv)")
