"""Where Corvus keeps its state, spelled the way the host spells paths.

Every default location is a subdirectory of ``~/.corvus``. This was written as
``os.path.expanduser("~/.corvus/logs")``, which is correct on POSIX and only
half-correct on Windows: ``expanduser`` replaces the tilde and leaves the rest
alone, so the result came out as ``C:\\Users\\pilot/.corvus/logs`` — mixed
separators. Windows opens that file happily, but it is what the operator sees
in the Analysis page's folder line and in every error message, and half of a
path in the wrong slashes reads like a bug in the application.

One place to build them, so the answer is native everywhere.
"""
from __future__ import annotations

import os

# The directory name itself stays dotted on every platform. A leading dot means
# nothing to Windows Explorer, but the alternative is a different location per
# platform, and an operator moving a config between two machines is better
# served by one answer than by a tidier one.
CORVUS_DIR_NAME = ".corvus"


def corvus_home() -> str:
    """Absolute path of ``~/.corvus``, with the host's own separators."""
    return os.path.join(os.path.expanduser("~"), CORVUS_DIR_NAME)


def corvus_path(*parts: str) -> str:
    """``~/.corvus`` joined with *parts* — ``corvus_path("logs")`` and so on."""
    return os.path.join(corvus_home(), *parts)
