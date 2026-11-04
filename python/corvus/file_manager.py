"""Show a folder to the operator in the host's own file manager.

Corvus tells the operator about several directories — the plugin folder, the
parameter exports, the flight logs — and a path in a settings field is a poor
way to reach one. This opens the folder the way the platform does: Finder on
macOS, Explorer on Windows, whatever ``xdg-open`` resolves to on Linux.

The one safety property that matters here: the path is passed as an argv
element to a fixed executable, never through a shell, so a folder name cannot
turn into a command. Callers pass a directory Corvus itself decided on, and
this refuses anything that is not an existing directory.

:func:`open_url` hands a web address to the system browser the same way. Both
start the host's launcher with the operator's environment, not the one a
packaged Corvus runs with (:mod:`corvus.child_env`): on a KDE desktop
``xdg-open`` is a Qt program, and with the AppImage's library and plugin paths
it loads Corvus' Qt instead of its own and dies before it opens anything.

stdlib only.
"""
from __future__ import annotations

import logging
import os
import subprocess
import sys
from urllib.parse import urlsplit

from .child_env import child_env

logger = logging.getLogger("corvus.file_manager")

# How long to wait for the launcher to report back. The file manager itself
# keeps running; these commands only hand the path over and exit, so a process
# still alive after this has failed to do that.
_LAUNCH_TIMEOUT_S = 5


def open_folder(path: str) -> tuple[bool, str]:
    """Open *path* in the host file manager; return ``(ok, error)``.

    ``error`` is an empty string on success and an operator-readable sentence
    otherwise. Never raises: a machine with no desktop session (a headless
    server running ``serve.py``) simply has nothing to open, and that is a
    message in the UI, not a failed request.
    """
    if not isinstance(path, str) or not path:
        return False, "no folder given"
    if not os.path.isdir(path):
        return False, f"{path} does not exist"

    if sys.platform == "darwin":
        argv = ["open", path]
    elif os.name == "nt":
        # explorer.exe returns a non-zero exit code even when it opened the
        # window, so this one path is checked by whether it started at all.
        try:
            subprocess.Popen(["explorer", os.path.normpath(path)],
                             close_fds=True, env=child_env())
            return True, ""
        except OSError as exc:
            logger.warning("could not open %s in Explorer: %s", path, exc)
            return False, "could not open the folder"
    else:
        argv = ["xdg-open", path]

    try:
        proc = subprocess.run(
            argv, timeout=_LAUNCH_TIMEOUT_S,
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=False,
            env=child_env(),
        )
    except FileNotFoundError:
        logger.warning("no file manager launcher (%s) on this system", argv[0])
        return False, f"{argv[0]} is not available on this system"
    except (OSError, subprocess.SubprocessError) as exc:
        logger.warning("could not open %s: %s", path, exc)
        return False, "could not open the folder"
    if proc.returncode != 0:
        detail = (proc.stderr or b"").decode("utf-8", errors="replace").strip()
        logger.warning("%s exited %d for %s: %s", argv[0], proc.returncode, path, detail)
        return False, "could not open the folder"
    return True, ""


def open_url(url: str, platform: str = sys.platform) -> bool:
    """Open a web page in the system browser; whether a browser was started.

    http and https only: on every desktop the URL opener is a launcher for far
    more schemes than those, and the scheme decides which program runs.

    Linux goes through ``xdg-open`` with :func:`corvus.child_env.child_env`,
    not through :mod:`webbrowser`, which starts its commands with this
    process' environment and no way to give it another. It is not waited for:
    some browsers keep ``xdg-open`` until their window closes. On macOS the
    browser is started by Launch Services, in the session's environment, and
    on Windows nothing the build sets is read by a browser, so
    :mod:`webbrowser` is right on both.
    """
    try:
        if urlsplit(url).scheme not in ("http", "https"):
            return False
    except ValueError:
        return False
    if platform.startswith("linux"):
        try:
            subprocess.Popen(
                ["xdg-open", url], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL, close_fds=True, start_new_session=True,
                env=child_env(),
            )
            return True
        except OSError as exc:
            logger.info("xdg-open could not open %s: %s", url, exc)
    try:
        import webbrowser
        return bool(webbrowser.open(url))
    except Exception:  # noqa: BLE001 - no browser is an answer, not an error
        logger.info("could not open %s in a browser", url, exc_info=True)
        return False
