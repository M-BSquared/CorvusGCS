"""Where the HTTP layer reads and writes the operator's files: parameter and
settings exports, missions.

Moved out of corvus/server.py, which re-exports them.
"""
from __future__ import annotations

import os
import socket
import time
from typing import Any

from . import mission
from .http_input import _slugify
from .paths import corvus_path


def _params_export_dir(cfg: Any) -> str:
    """Directory exported parameter files are written to.

    The operator's ``params_dir`` when set, otherwise ``~/.corvus/params`` —
    the same "empty string means the built-in default" convention the tile
    cache and tlog directories use.
    """
    configured = getattr(cfg, "params_dir", "") or ""
    if configured.strip():
        return os.path.expanduser(configured.strip())
    return corvus_path("params")


def _settings_export_dir() -> str:
    """Where a settings export goes by default: Downloads, else the home folder.

    Not under ``~/.corvus`` like the other exports: this file is made to be
    carried to another machine, and a hidden folder is where it gets lost.
    """
    home = os.path.expanduser("~")
    downloads = os.path.join(home, "Downloads")
    return downloads if os.path.isdir(downloads) else home


def _default_settings_filename() -> str:
    """``corvus-settings_<host>_<YYYY-MM-DD_HH-MM>.json``: which station, and when."""
    stamp = time.strftime("%Y-%m-%d_%H-%M")
    try:
        host = _slugify(socket.gethostname().split(".")[0])
    except OSError:
        host = ""
    return f"corvus-settings_{host}_{stamp}.json" if host else f"corvus-settings_{stamp}.json"


def _missions_dir(cfg: Any) -> str:
    """Directory saved mission plans are read from and written to.

    Same convention as the parameter export folder above: the operator's
    ``missions_dir`` when set, ``~/.corvus/missions`` otherwise.
    """
    configured = getattr(cfg, "missions_dir", "") or ""
    return mission.missions_dir(os.path.expanduser(configured.strip()))
