"""Settings export and import.

Route handlers moved out of corvus/server.py. CorvusHandler inherits
SettingsRoutes, and the @route registry wires them exactly as before.
"""
from __future__ import annotations

import logging
import dataclasses
import json
import os
import tempfile

from pathlib import Path
from typing import Any
from .. import plugin_config, plugin_files, rtk, settings_bundle, video
from ..config import CorvusConfig, _config_to_dict, default_config_path, save_config, to_public_dict
from ..http_input import _safe_filename
from ..http_paths import _default_settings_filename, _settings_export_dir
from ..http_routes import _config_write_lock, route
from ..plugin_registry import discover as discover_plugins, is_valid_id as is_valid_plugin_id

logger = logging.getLogger("corvus.server")

# The plugins one export may carry together, before base64. Keeps a file with
# the logo and every plugin inside MAX_SETTINGS_BODY_BYTES once encoded.
MAX_EXPORT_PLUGIN_BYTES = 16 * 1024 * 1024

# Config keys read once at startup (the listening socket, the tile cache, the
# tlog recorder, the firmware catalog), so a settings import that changes one
# says a restart is needed rather than pretending it took effect.
_IMPORT_RESTART_KEYS: tuple[str, ...] = ("http_port", "tile_cache_dir", "tlog_dir", "firmware_dir")


class SettingsRoutes:
    # ---- Settings export and import (corvus/settings_bundle.py) ----
    @route("GET", "/api/settings/export/target")
    def _api_settings_export_target(self) -> None:
        """Where a settings export would be written, and a filename. Always 200."""
        self._send_json({"dir": _settings_export_dir(),
                         "filename": _default_settings_filename()})

    @route("POST", "/api/settings/export")
    def _api_settings_export(self, payload: dict) -> None:
        """Write the chosen settings of this station to one file and return its path.

        Written here rather than as a browser download for the reason parameter
        exports are (see ``_api_params_export``). It is also what lets the file
        carry the secrets when ``include_secrets`` asks for them: they go from
        the config straight to disk and never through an HTTP response.
        ``browser`` is the interface state the page keeps in localStorage,
        carried through untouched.

        ``sections`` picks the parts (every one when absent). With
        ``plugins`` among them, ``plugin_settings`` names the plugins whose
        settings go in (every one when absent) and ``plugin_files`` the
        plugins that go in themselves (none when absent); only a plugin from
        the operator's folder can, since a bundled one ships with Corvus.
        """
        include_secrets = payload.get("include_secrets", False)
        if not isinstance(include_secrets, bool):
            self._send_json({"ok": False, "error": "include_secrets must be true or false"}, 400)
            return
        browser = payload.get("browser", {})
        if not isinstance(browser, dict):
            self._send_json({"ok": False, "error": "browser must be an object"}, 400)
            return
        try:
            sections = settings_bundle.normalize_sections(payload.get("sections"))
        except ValueError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        if not sections:
            self._send_json({"ok": False, "error": "Choose at least one part to export."}, 400)
            return
        files_by_plugin, error = self._export_plugin_files(payload.get("plugin_files", []),
                                                           "plugins" in sections)
        if error is not None:
            self._send_json({"ok": False, "error": error}, 400)
            return
        raw_dir = payload.get("dir")
        target_dir = raw_dir if isinstance(raw_dir, str) and raw_dir.strip() else \
            _settings_export_dir()
        target_dir = os.path.expanduser(target_dir.strip())
        filename = _safe_filename(payload.get("filename"), _default_settings_filename(), ".json")

        with _config_write_lock:
            cfg = self._live_config()
            config = _config_to_dict(cfg)
            plugins = self._plugin_settings_all()
        try:
            chosen_settings = settings_bundle.normalize_ids(
                payload.get("plugin_settings"), plugins, "plugin_settings")
        except ValueError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        plugins = {k: v for k, v in plugins.items() if k in chosen_settings}
        logo = None
        if isinstance(cfg.branding, dict) and cfg.branding.get("logo"):
            try:
                logo = self._logo_path().read_bytes() or None
            except OSError:
                logo = None
        bundle = settings_bundle.build(config, sections=sections, plugins=plugins,
                                       plugin_files=files_by_plugin, logo=logo,
                                       browser=browser, include_secrets=include_secrets)
        try:
            text = json.dumps(bundle, indent=2, ensure_ascii=False) + "\n"
            os.makedirs(target_dir, exist_ok=True)
            path = os.path.join(target_dir, filename)
            # mkstemp creates the file 0o600, which the replace keeps: with the
            # secrets in it, this file is as sensitive as the config itself.
            fd, tmp_name = tempfile.mkstemp(prefix=filename + ".", suffix=".tmp", dir=target_dir)
            try:
                with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as f:
                    f.write(text)
                os.replace(tmp_name, path)
            except BaseException:
                try:
                    os.unlink(tmp_name)
                except OSError:
                    pass
                raise
        except OSError as exc:
            self._send_json(
                {"ok": False, "error": f"could not write to {target_dir}: {exc.strerror or exc}"},
                400)
            return
        except Exception as exc:  # noqa: BLE001 - an export must never 500 unexplained
            logger.exception("settings export failed")
            self._send_json({"ok": False, "error": f"export failed: {exc}"}, 500)
            return
        logger.info("exported settings (%s) to %s (secrets %s, %d plugin(s) with their files)",
                    ", ".join(bundle["sections"]), path,
                    "included" if include_secrets else "left out", len(files_by_plugin))
        self._send_json({"ok": True, "path": path, "dir": target_dir,
                         "filename": filename, "secrets": include_secrets,
                         "sections": bundle["sections"]})

    def _export_plugin_files(self, raw: Any, wanted: bool
                             ) -> tuple[dict[str, dict[str, bytes]], str | None]:
        """The files of each plugin named in *raw*, or an error sentence.

        Only plugins discovered in the operator's folder: a bundled one ships
        with Corvus, and a name that is not installed has no files to take.
        """
        if raw is None or not wanted:
            return {}, None
        if not isinstance(raw, list) or not all(isinstance(p, str) for p in raw):
            return {}, "plugin_files must be a list of plugin ids"
        if not raw:
            return {}, None
        user_dir, bundled_dir = self._plugin_roots()
        installed = {m["id"]: m for m in discover_plugins(user_dir, bundled_dir)}
        out: dict[str, dict[str, bytes]] = {}
        total = 0
        for plugin_id in dict.fromkeys(raw):
            manifest = installed.get(plugin_id)
            if manifest is None or manifest.get("source") != "user":
                return {}, f"The plugin {plugin_id} is not in the plugin folder, so its files cannot be exported."
            try:
                files = plugin_files.collect(manifest["dir"])
            except ValueError as exc:
                return {}, (f"The plugin {manifest['name']} is {exc}, too much for a settings file. "
                            "Copy its folder instead.")
            except OSError as exc:
                return {}, f"The plugin {manifest['name']} could not be read: {exc.strerror or exc}"
            total += sum(len(b) for b in files.values())
            if total > MAX_EXPORT_PLUGIN_BYTES:
                return {}, ("The chosen plugins are larger than "
                            f"{MAX_EXPORT_PLUGIN_BYTES // (1024 * 1024)} MB together. "
                            "Take fewer of them, or copy their folders instead.")
            out[plugin_id] = files
        return out, None

    @route("POST", "/api/settings/import")
    def _api_settings_import(self, payload: dict) -> None:
        """Apply a settings file written by ``POST /api/settings/export``.

        ``bundle`` is the parsed file and ``sections`` the section ids to take
        from it (every one the file carries when absent); a section the file
        does not carry is never taken from it. With ``plugins`` among them,
        ``plugin_settings`` and ``plugin_files`` name the plugins whose
        settings, and which plugins themselves, are taken (every one in the
        file when absent). Refused while armed: an import can restart the
        forwarder, the RTK corrections and the cameras, replace the Remote ID
        the aircraft broadcasts, and install plugin code. Everything is
        checked before anything is written. What runs from the config is
        applied now; the few settings read only at startup come back in
        ``restart`` so the page can say so. The browser block is the page's to
        apply. A successful import also counts as the first start setup.
        """
        if self.store is not None and self.store.get_snapshot().get("armed"):
            self._send_json({"ok": False,
                             "error": "Settings cannot be imported while the vehicle is armed."},
                            409)
            return
        try:
            bundle = settings_bundle.parse(payload.get("bundle"))
            sections = settings_bundle.normalize_sections(payload.get("sections"))
            take_settings = settings_bundle.normalize_ids(
                payload.get("plugin_settings"), bundle["plugins"], "plugin_settings")
            take_files = settings_bundle.normalize_ids(
                payload.get("plugin_files"), bundle["plugin_files"], "plugin_files")
        except ValueError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        sections &= set(bundle["sections"])
        if "plugins" not in sections:
            take_settings, take_files = set(), set()
        if not sections:
            self._send_json({"ok": False, "error": "Choose at least one part to import."}, 400)
            return

        from ..config import _build_config
        warnings: list[str] = []
        plugins_written = 0
        plugins_installed = 0
        with _config_write_lock:
            cfg = self._live_config()
            before = _config_to_dict(cfg)
            merged = settings_bundle.merge(before, bundle["config"], sections,
                                           secrets=bundle["secrets"])
            logo = bundle["logo"] if "interface" in sections else None
            if "interface" in sections and logo is None:
                # A logo name with no picture behind it would draw a broken
                # image in the top bar.
                branding = merged.get("branding")
                if isinstance(branding, dict):
                    branding = {k: v for k, v in branding.items() if k != "logo"}
                    if branding:
                        merged["branding"] = branding
                    else:
                        merged.pop("branding", None)
            new_cfg = _build_config(merged)

            if "interface" in sections:
                try:
                    if logo is not None:
                        self._store_logo(logo)
                    else:
                        self._logo_path().unlink(missing_ok=True)
                except OSError as exc:
                    logger.error("settings import: company logo not stored: %s", exc)
                    warnings.append("The company logo could not be stored.")
                    new_cfg.branding = cfg.branding

            new_cfg.plugins = cfg.plugins
            if "plugins" in sections:
                # Files before settings: an install keeps the settings the
                # installed copy had, and the file's own settings, when
                # chosen, then replace them.
                for plugin_id in sorted(take_files):
                    try:
                        plugin_files.install(plugin_id, bundle["plugin_files"][plugin_id],
                                             self.plugin_user_dir)
                    except (OSError, ValueError) as exc:
                        logger.error("settings import: plugin %s not installed: %s",
                                     plugin_id, exc)
                        warnings.append(f"The plugin {plugin_id} could not be installed.")
                        continue
                    plugins_installed += 1
                legacy = dict(cfg.plugins) if isinstance(cfg.plugins, dict) else {}
                for plugin_id, settings in bundle["plugins"].items():
                    if plugin_id not in take_settings or not is_valid_plugin_id(plugin_id):
                        continue
                    try:
                        plugin_config.save(plugin_id, settings, self.plugin_user_dir)
                    except (OSError, ValueError) as exc:
                        logger.error("settings import: plugin %s not stored: %s", plugin_id, exc)
                        warnings.append(f"The settings of plugin {plugin_id} could not be stored.")
                        continue
                    plugins_written += 1
                    legacy.pop(plugin_id, None)
                new_cfg.plugins = legacy or None

            # In place, like _apply_config_partial: every handler shares this object.
            for field in dataclasses.fields(cfg):
                setattr(cfg, field.name, getattr(new_cfg, field.name))
            after = _config_to_dict(cfg)
            try:
                save_config(cfg, self.config_path or default_config_path())
            except OSError as exc:
                logger.error("settings import: config not saved: %s", exc)
                warnings.append("Applied for this session but not saved.")
            if before.get("forwarding") != after.get("forwarding"):
                self._restart_forwarder()
            self._refresh_autoconnect_session(cfg)
            self._refresh_battery_settings(cfg)
            self._refresh_remote_id(cfg)

        if before.get("video") != after.get("video"):
            self._apply_video(video.settings(cfg.video))
        if before.get("rtk") != after.get("rtk") and self.rtk is not None:
            try:
                self.rtk.apply_settings(rtk.settings(cfg.rtk))
            except Exception:  # noqa: BLE001 - the rest of the import stands
                logger.exception("settings import: RTK settings not applied")
                warnings.append("The RTK settings are stored but could not be applied.")
        restart = [key for key in _IMPORT_RESTART_KEYS if before.get(key) != after.get(key)]
        self._finish_first_start()
        logger.info("imported settings (%s) from a file written by %s",
                    ", ".join(sorted(sections)), bundle["version"] or "an unknown version")
        self._send_json({
            "ok": True,
            "config": to_public_dict(cfg),
            "sections": sorted(sections),
            "plugins": plugins_written,
            "plugins_installed": plugins_installed,
            "restart": restart,
            "warnings": warnings,
        })

    @route("POST", "/api/settings/reset")
    def _api_settings_reset(self, payload: dict) -> None:
        """Put the station back to factory settings and reopen the first start setup.

        The config goes back to its defaults and its file is removed, the
        company logo and every plugin's saved settings are deleted, and the
        first start setup is pending again, for this session and, with no
        config file on disk, for the next start too. Plugins the operator
        installed stay installed: they are software, not settings. Refused
        while armed, for the reasons an import is. The browser half, the
        interface state in localStorage, is the page's to clear.
        """
        if self.store is not None and self.store.get_snapshot().get("armed"):
            self._send_json({"ok": False,
                             "error": "Settings cannot be reset while the vehicle is armed."},
                            409)
            return
        warnings: list[str] = []
        with _config_write_lock:
            cfg = self._live_config()
            before = _config_to_dict(cfg)
            fresh = CorvusConfig()
            for field in dataclasses.fields(cfg):
                setattr(cfg, field.name, getattr(fresh, field.name))
            after = _config_to_dict(cfg)
            try:
                Path(self.config_path or default_config_path()).unlink(missing_ok=True)
            except OSError as exc:
                logger.error("settings reset: config file not removed: %s", exc)
                warnings.append("The config file could not be removed. "
                                "The next start may skip the setup.")
            try:
                self._logo_path().unlink(missing_ok=True)
            except OSError as exc:
                logger.error("settings reset: company logo not removed: %s", exc)
                warnings.append("The company logo could not be removed.")
            for plugin_id in plugin_config.load_all(self.plugin_user_dir):
                path = plugin_config.config_path(plugin_id, self.plugin_user_dir)
                if path is None:
                    continue
                try:
                    os.unlink(path)
                except FileNotFoundError:
                    pass
                except OSError as exc:
                    logger.error("settings reset: plugin %s settings not removed: %s",
                                 plugin_id, exc)
                    warnings.append(f"The settings of plugin {plugin_id} could not be removed.")
            if before.get("forwarding") != after.get("forwarding"):
                self._restart_forwarder()
            self._refresh_autoconnect_session(cfg)
            self._refresh_battery_settings(cfg)
            self._refresh_remote_id(cfg)

        if before.get("video") != after.get("video"):
            self._apply_video(video.settings(cfg.video))
        if before.get("rtk") != after.get("rtk") and self.rtk is not None:
            try:
                self.rtk.apply_settings(rtk.settings(cfg.rtk))
            except Exception:  # noqa: BLE001 - the rest of the reset stands
                logger.exception("settings reset: RTK settings not applied")
                warnings.append("The RTK settings are reset but could not be applied.")
        restart = [key for key in _IMPORT_RESTART_KEYS if before.get(key) != after.get(key)]
        state = self.first_start
        if state is not None:
            state["pending"] = True
        logger.info("settings reset to factory defaults")
        self._send_json({"ok": True, "config": to_public_dict(cfg),
                         "restart": restart, "warnings": warnings})

    def _store_logo(self, raw: bytes) -> None:
        """Write *raw* as the company logo, atomically. Raises OSError."""
        path = self._logo_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp_name = tempfile.mkstemp(prefix=path.name + ".", suffix=".tmp",
                                        dir=str(path.parent))
        try:
            with os.fdopen(fd, "wb") as f:
                f.write(raw)
            os.replace(tmp_name, path)
        except OSError:
            try:
                os.unlink(tmp_name)
            except OSError:
                pass
            raise
