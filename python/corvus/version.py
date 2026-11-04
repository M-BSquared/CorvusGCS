"""Version module — single source of truth for the Corvus GCS version.

Reads the repository-root ``VERSION`` file (or the one beside ``corvus/`` in a
bundle) at import time. No other module
may contain a literal version string; it imports from here.
"""
from __future__ import annotations

import pathlib

# In a bundle VERSION sits beside corvus/; in the repository it is one level
# above python/, which is where the one copy lives.
_HERE = pathlib.Path(__file__).resolve().parent.parent
_VERSION_FILE = next((d / "VERSION" for d in (_HERE, _HERE.parent) if (d / "VERSION").is_file()),
                     _HERE / "VERSION")


def _read_version() -> str:
    try:
        return _VERSION_FILE.read_text().strip()
    except OSError:
        return "0.0.0-unknown"


VERSION = _read_version()
__version__ = VERSION


def get_version() -> str:
    """Return the running Corvus GCS version string."""
    return VERSION
