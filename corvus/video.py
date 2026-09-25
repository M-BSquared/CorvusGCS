"""Camera video for the floating camera windows: RTSP decoded here, WebRTC passed through.

Two kinds of camera
-------------------
**RTSP** (and the udp://, srt:// and http:// streams ffmpeg reads the same
way). A browser cannot open an ``rtsp://`` address, and the PyQt6 WebEngine
wheels are built without the proprietary codecs, so neither ``<video>`` nor
WebCodecs can play the H.264 or H.265 that drone cameras send. The stream is
decoded on this side: one ffmpeg process per camera writes JPEG frames to a
pipe, and the camera window fetches the newest one.

**WebRTC**, over WHEP (RFC 9725): a media server such as MediaMTX, go2rtc or
Janus on the aircraft or the ground network sends the video straight to the
window, which decodes it itself. Nothing is decoded here. Only the signalling
passes through this module (:meth:`VideoService.whep_offer`), and for one
reason: the camera's password stays in this process. The browser gets an
opaque session token, never a credential and never the session URL it would
need to send one.

Why RTSP frames are fetched one at a time
-----------------------------------------
A browser holds at most six HTTP/1.1 connections per origin, and this app
already spends two of them on telemetry and events (see ``src/js/events.js``).
A ``multipart/x-mixed-replace`` stream would take a third for good, per
camera. :meth:`VideoService.frame` is a long poll instead: it answers as soon
as a frame newer than the one the window already has exists, so a connection
is only ever held for one frame interval.

The same poll is what keeps ffmpeg running. A camera nobody has asked for a
frame in :data:`IDLE_STOP_S` is stopped, so a closed window, a reloaded page
or a crashed browser all end the decoder without a message having to arrive.

What keeps it safe
------------------
* The password never goes on ffmpeg's command line, where every account on
  the computer can read it (``ps``). ffmpeg 5 and later is handed the address
  in an ffconcat list file that only this account can read, and the file is
  deleted once ffmpeg has opened it. ffmpeg 4 has no way to take input options
  from a file; it still works, and the status says the password is visible.
* ffmpeg may open only the protocols the camera's scheme needs. An http://
  stream is an HLS playlist as easily as an MJPEG one, and a playlist can name
  ``file:`` segments; the whitelist is what keeps a camera address from being
  a way to read local files.
* Every network wait ffmpeg does has a timeout, so a camera that stops
  answering ends its ffmpeg instead of leaving it parked on a socket. That is
  also what ends an ffmpeg that outlived a Corvus killed hard.
* WHEP requests follow no redirect, and credentials are only ever sent to the
  origin the operator typed: a session URL the server hands back that points
  anywhere else is not used.

Lifecycle: every process is terminated (``SIGTERM``, then ``SIGKILL`` after
:data:`TERM_TIMEOUT_S`), every thread joined, every WHEP session closed and the
private temp folder removed by :meth:`VideoService.shutdown`, which both
launchers call through ``corvus.server.stop_backend``.

stdlib only.
"""
from __future__ import annotations

import base64
import collections
import itertools
import logging
import os
import re
import secrets
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from typing import Any
from urllib.parse import quote, unquote, urljoin, urlsplit, urlunsplit

from .child_env import child_env

logger = logging.getLogger("corvus.video")

# ---------------------------------------------------------------------------
# Limits and timings
# ---------------------------------------------------------------------------

MAX_STREAMS = 8
MAX_NAME_LEN = 64
MAX_URL_LEN = 2048

KIND_RTSP = "rtsp"
KIND_WEBRTC = "webrtc"
KINDS: tuple[str, ...] = (KIND_RTSP, KIND_WEBRTC)

# What ffmpeg is allowed to open. RTSP is the point; the others cost nothing
# because ffmpeg reads them the same way, and a few cameras and video links
# send them instead. ``file:``, ``concat:`` and friends are deliberately not
# here: this is a camera address, not a way to read local files.
SCHEMES: tuple[str, ...] = ("rtsp", "rtsps", "udp", "srt", "http", "https")
# A WHEP endpoint is an HTTP resource.
WEBRTC_SCHEMES: tuple[str, ...] = ("http", "https")

# The protocols each scheme may reach once ffmpeg has it, and nothing else.
_PROTOCOLS: dict[str, str] = {
    "rtsp": "rtsp,tcp,udp,rtp",
    "rtsps": "rtsps,rtsp,tcp,tls,udp,rtp",
    "udp": "udp,rtp",
    "srt": "srt",
    "http": "http,https,tcp,tls,crypto",
    "https": "http,https,tcp,tls,crypto",
}

TRANSPORT_TCP = "tcp"
TRANSPORT_UDP = "udp"
TRANSPORTS: tuple[str, ...] = (TRANSPORT_TCP, TRANSPORT_UDP)
# TCP by default: it survives the packet loss of a radio link and a NAT, where
# RTSP over UDP shows a grey smear or nothing. UDP is offered for the cameras
# that only do that, and for the lower latency when the link is clean.
DEFAULT_TRANSPORT = TRANSPORT_TCP

# Frames are scaled down to this width. A 4K camera re-encoded to JPEG at full
# size costs a core and a few MB per frame to show in a window that is 640
# pixels wide.
MAX_WIDTH = 1280
# ffmpeg's -q:v for MJPEG: 2 is best, 31 worst. 5 is visually clean at a
# fraction of the size.
JPEG_QUALITY = 5

# No frame request for this long and the decoder is stopped.
IDLE_STOP_S = 5.0
# No new frame for this long and the stream is reported as stalled.
STALL_S = 3.0
# No new frame for this long and the decoder is restarted. A camera that
# rebooted leaves ffmpeg waiting on a socket that will never speak again.
WATCHDOG_S = 10.0
# ffmpeg's own network timeout: shorter than the watchdog, so ffmpeg normally
# gives up first and says why.
SOCKET_TIMEOUT_S = 8.0
# Pauses between restarts of a stream that keeps failing.
RETRY_BACKOFF_S: tuple[float, ...] = (1.0, 2.0, 4.0, 8.0)
# How long one frame request waits for a new frame before answering with the
# state instead.
FRAME_WAIT_S = 2.0
# SIGTERM, then this long, then SIGKILL.
TERM_TIMEOUT_S = 2.0
# A frame larger than this without an end marker is not a frame: the buffer is
# dropped rather than grown without bound.
MAX_FRAME_BYTES = 16 * 1024 * 1024
# The longest ffmpeg error line kept. A line with no end is cut, not buffered.
MAX_ERROR_LINE = 4096
# ffmpeg is looked for again this often while none was found, so installing it
# does not need a restart.
RESOLVE_RETRY_S = 5.0
# The ffconcat list holding the address is removed this long after ffmpeg was
# started, or at the first frame, whichever is first. ffmpeg reads the whole
# list while opening its input.
LIST_KEEP_S = 5.0

# WebRTC signalling.
WHEP_TIMEOUT_S = 10.0
WHEP_CLOSE_TIMEOUT_S = 3.0
MAX_SDP_BYTES = 256 * 1024
MAX_WHEP_SESSIONS = 16

STATE_IDLE = "idle"
STATE_STARTING = "starting"
STATE_LIVE = "live"
STATE_STALLED = "stalled"
STATE_ERROR = "error"
STATE_UNAVAILABLE = "unavailable"

# Where a package manager puts ffmpeg when the PATH a desktop app inherits
# does not include it (a .app started from Finder sees /usr/bin:/bin only).
_COMMON_FFMPEG_PATHS: tuple[str, ...] = (
    "/opt/homebrew/bin/ffmpeg",
    "/usr/local/bin/ffmpeg",
    "/usr/bin/ffmpeg",
    "/snap/bin/ffmpeg",
    "/opt/local/bin/ffmpeg",
)


def windows_ffmpeg_paths(env: dict[str, str] | None = None) -> list[str]:
    """Where winget, Scoop and Chocolatey put ffmpeg on Windows.

    Each of them adds its folder to the PATH, but only for programs started
    after the install: a Corvus that was already open, or one started from a
    shortcut that kept the old environment, would not find it there.
    """
    env = os.environ if env is None else env
    out = []
    local = env.get("LOCALAPPDATA", "")
    if local:
        out.append(os.path.join(local, "Microsoft", "WinGet", "Links", "ffmpeg.exe"))
    profile = env.get("USERPROFILE", "")
    if profile:
        out.append(os.path.join(profile, "scoop", "shims", "ffmpeg.exe"))
    data = env.get("ProgramData", "")
    if data:
        out.append(os.path.join(data, "chocolatey", "bin", "ffmpeg.exe"))
    return out

FFMPEG_ENV = "CORVUS_FFMPEG"

# Frame numbers are drawn from one counter for the whole process, so a camera
# rebuilt under the same id (its address was edited) still numbers its first
# frame above anything a window open on it has already seen.
_FRAME_SEQ = itertools.count(1)


class WrongKind(ValueError):
    """The camera exists, but is not the kind the request is for."""


# ---------------------------------------------------------------------------
# Addresses and credentials
# ---------------------------------------------------------------------------

def split_credentials(url: str) -> tuple[str, str, str]:
    """``(address, username, password)`` with any ``user:pass@`` taken out.

    Cameras are usually given as ``rtsp://admin:secret@192.168.1.10/stream``.
    The credentials are stored apart from the address so the address can be
    shown and the password never has to be.
    """
    text = str(url or "").strip()
    try:
        parts = urlsplit(text)
    except ValueError:
        return text, "", ""
    if "@" not in parts.netloc:
        return text, "", ""
    userinfo, _, hostport = parts.netloc.rpartition("@")
    user, _, password = userinfo.partition(":")
    clean = urlunsplit((parts.scheme, hostport, parts.path, parts.query, parts.fragment))
    return clean, unquote(user), unquote(password)


def with_credentials(url: str, username: str, password: str) -> str:
    """The address ffmpeg opens: *url* with the credentials put back in."""
    if not username:
        return url
    parts = urlsplit(url)
    userinfo = quote(username, safe="")
    if password:
        userinfo += ":" + quote(password, safe="")
    return urlunsplit((parts.scheme, f"{userinfo}@{parts.netloc}",
                       parts.path, parts.query, parts.fragment))


def display_address(url: str) -> str:
    """Host, port and path, for a window title bar: no scheme, no credentials."""
    clean, _, _ = split_credentials(url)
    try:
        parts = urlsplit(clean)
    except ValueError:
        return clean
    out = parts.netloc + parts.path
    if parts.query:
        out += "?" + parts.query
    return out or clean


def url_problem(url: str, kind: str = KIND_RTSP) -> str:
    """Why *url* cannot be a camera address of this *kind*, or ``""`` when it can."""
    text = str(url or "").strip()
    webrtc = kind == KIND_WEBRTC
    example = ("http://192.168.144.25:8889/cam/whep" if webrtc
               else "rtsp://192.168.144.25:8554/main.264")
    if not text:
        return f"Enter the camera's address, for example {example}."
    if len(text) > MAX_URL_LEN:
        return "The address is too long."
    if any(ch.isspace() or ord(ch) < 32 or ord(ch) == 127 for ch in text):
        return "The address cannot contain spaces."
    try:
        parts = urlsplit(text)
        port = parts.port
    except ValueError:
        return "The port in the address is not a number."
    scheme = parts.scheme.lower()
    if webrtc:
        if scheme not in WEBRTC_SCHEMES:
            return f"A WebRTC camera is its WHEP address, for example {example}."
        if not parts.hostname:
            return "The address has no host."
        return ""
    if scheme not in SCHEMES:
        return "The address has to start with rtsp://, or udp://, srt:// or http:// for other streams."
    # udp:// may name only a port to listen on (udp://@:5600), every other
    # scheme has to say where the camera is.
    if scheme != "udp" and not parts.hostname:
        return "The address has no host."
    if scheme == "udp" and not parts.hostname and port is None:
        return "A udp:// address needs at least a port, for example udp://@:5600."
    return ""


def redact(text: str, secret: str) -> str:
    """*text* with every appearance of *secret* replaced, for logs and messages."""
    if not secret:
        return text
    out = text.replace(secret, "***")
    quoted = quote(secret, safe="")
    return out.replace(quoted, "***") if quoted != secret else out


# The errors an operator actually meets, in words that say what to check.
_ERROR_HINTS: tuple[tuple[str, str], ...] = (
    ("401", "The camera refused the user or password."),
    ("403", "The camera refused access."),
    ("404", "The camera has no stream at that path."),
    ("Connection refused", "Nothing answered at that address and port."),
    ("timed out", "The camera did not answer."),
    ("No route to host", "That address cannot be reached from this computer."),
    ("Network is unreachable", "That address cannot be reached from this computer."),
    ("Name or service not known", "That host name is unknown."),
    ("nodename nor servname", "That host name is unknown."),
    # ffmpeg reports an RTSP camera that went silent the same way as one that
    # sent garbage, so this says only what is certain.
    ("Invalid data found", "No readable video came from that address."),
)

# ffmpeg's "[tcp @ 0x7f...] " prefix names an internal object, not anything an
# operator can act on.
_FFMPEG_PREFIX = re.compile(r"^(\[[^\]]*@ 0x[0-9a-fA-F]+\]\s*)+")
# Python's "[Errno 61] " is a number for the platform, not for the operator.
_ERRNO = re.compile(r"\[Errno -?\d+\]\s*")


def explain_error(line: str) -> str:
    """ffmpeg's last word, led by a plain sentence when the cause is a known one."""
    text = _ERRNO.sub("", _FFMPEG_PREFIX.sub("", str(line or "").strip()))
    for needle, hint in _ERROR_HINTS:
        if needle.lower() in text.lower():
            return f"{hint} ({text})" if text else hint
    return text


# ---------------------------------------------------------------------------
# Settings
# ---------------------------------------------------------------------------

def new_stream_id() -> str:
    """A short id for a new camera. Never shown; it keys the window and the URL."""
    return "v" + secrets.token_hex(4)


def _clean_str(raw: Any, limit: int) -> str:
    return raw.strip()[:limit] if isinstance(raw, str) else ""


def coerce_stream(raw: Any) -> dict[str, Any] | None:
    """One saved camera as a complete dict, or None to drop it.

    Defensive because it reads a JSON file an operator may have edited: a
    half-written entry costs that camera and not the list.
    """
    if not isinstance(raw, dict):
        return None
    kind = raw.get("kind") if raw.get("kind") in KINDS else KIND_RTSP
    url, url_user, url_password = split_credentials(_clean_str(raw.get("url"), MAX_URL_LEN))
    if url_problem(url, kind):
        return None
    stream_id = raw.get("id")
    if (not isinstance(stream_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,32}", stream_id)):
        stream_id = new_stream_id()
    username = _clean_str(raw.get("username"), 256) or url_user
    # Not stripped: a password may begin or end with a space.
    password = raw.get("password") if isinstance(raw.get("password"), str) else ""
    password = (password or url_password)[:1024]
    transport = raw.get("transport")
    if transport not in TRANSPORTS:
        transport = DEFAULT_TRANSPORT
    return {
        "id": stream_id,
        "name": _clean_str(raw.get("name"), MAX_NAME_LEN) or display_address(url),
        "kind": kind,
        "url": url,
        "username": username,
        # RTSP cannot send a password without a user, so it is not kept. A
        # WHEP server takes one on its own as a bearer token.
        "password": password if (username or kind == KIND_WEBRTC) else "",
        "transport": transport,
    }


def settings(raw: Any) -> dict[str, Any]:
    """The video block of the config, every field bounded.

    ``{"streams": [...], "ffmpeg": ""}``. ``ffmpeg`` is an optional path to
    the binary; empty means "find it" (see :func:`resolve_ffmpeg`).
    """
    src = raw if isinstance(raw, dict) else {}
    streams: list[dict[str, Any]] = []
    seen: set[str] = set()
    for entry in src.get("streams") or []:
        stream = coerce_stream(entry)
        if stream is None or stream["id"] in seen:
            continue
        seen.add(stream["id"])
        streams.append(stream)
        if len(streams) >= MAX_STREAMS:
            break
    return {"streams": streams, "ffmpeg": _clean_str(src.get("ffmpeg"), 1024)}


def public_stream(stream: dict[str, Any]) -> dict[str, Any]:
    """A camera as the browser may see it: no password, only whether there is one."""
    out = {k: v for k, v in stream.items() if k != "password"}
    out.setdefault("kind", KIND_RTSP)
    out["has_password"] = bool(stream.get("password"))
    out["address"] = display_address(stream.get("url", ""))
    return out


# ---------------------------------------------------------------------------
# ffmpeg
# ---------------------------------------------------------------------------

def _is_executable(path: str) -> bool:
    return bool(path) and os.path.isfile(path) and os.access(path, os.X_OK)


def resolve_ffmpeg(configured: str = "") -> tuple[str, str]:
    """``(path, where it came from)``, or ``("", "")`` when there is no ffmpeg.

    In order: the path the operator configured, ``$CORVUS_FFMPEG``, the
    binary the ``imageio-ffmpeg`` package ships, ffmpeg on the ``PATH``, and
    the usual install folders. A configured path that does not exist is not
    silently replaced by another one: the operator asked for that binary.
    """
    configured = str(configured or "").strip()
    if configured:
        path = os.path.expanduser(configured)
        if _is_executable(path):
            return path, "configured"
        # "C:\\ffmpeg\\bin\\ffmpeg" names the program the way cmd would run it.
        if sys.platform == "win32" and not path.lower().endswith(".exe") and _is_executable(path + ".exe"):
            return path + ".exe", "configured"
        return "", ""
    env = os.environ.get(FFMPEG_ENV, "").strip()
    if env and _is_executable(os.path.expanduser(env)):
        return os.path.expanduser(env), "environment"
    try:
        import imageio_ffmpeg  # type: ignore[import-not-found]
        bundled = imageio_ffmpeg.get_ffmpeg_exe()
        if _is_executable(bundled):
            return bundled, "bundled"
    except Exception:  # noqa: BLE001 - absent or broken package: keep looking
        pass
    found = shutil.which("ffmpeg")
    if found:
        return found, "path"
    candidates = windows_ffmpeg_paths() if sys.platform == "win32" else list(_COMMON_FFMPEG_PATHS)
    for candidate in candidates:
        if _is_executable(candidate):
            return candidate, "path"
    return "", ""


def _no_window_flags() -> int:
    """Keep a console window from flashing up for ffmpeg on Windows."""
    return getattr(subprocess, "CREATE_NO_WINDOW", 0) if sys.platform == "win32" else 0


def ffmpeg_version(path: str) -> str:
    """``"7.1"`` from ``ffmpeg -version``, or ``""`` when it cannot be read."""
    try:
        out = subprocess.run(
            [path, "-hide_banner", "-version"],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            timeout=5, check=False, creationflags=_no_window_flags(), env=child_env(),
        ).stdout.decode("utf-8", "replace")
    except (OSError, subprocess.SubprocessError):
        return ""
    first = out.splitlines()[0] if out else ""
    words = first.split()
    if len(words) >= 3 and words[0] == "ffmpeg" and words[1] == "version":
        return words[2]
    return ""


def ffmpeg_major(version: str) -> int | None:
    """The major version from ``"8.1.2"``, ``"n7.1"`` or ``"4.4.2-0ubuntu1"``.

    None for a git build (``"N-112345-g..."``), which is newer than any
    release this module has to tell apart.
    """
    match = re.match(r"n?(\d+)\.\d+", str(version or "").strip())
    return int(match.group(1)) if match else None


def hides_password(version: str) -> bool:
    """Whether this ffmpeg can take the address from a file (ffconcat ``option``, 5.0+)."""
    major = ffmpeg_major(version)
    return major is None or major >= 5


def input_options(url: str, transport: str = DEFAULT_TRANSPORT,
                  version: str = "") -> list[tuple[str, str]]:
    """The options ffmpeg opens one camera with, in order.

    The protocol whitelist, the RTSP transport, a network timeout under the
    name this scheme and this ffmpeg use for it, and the settings that keep
    ffmpeg from buffering frames it could already show: for a pilot a second
    of buffer is a second of flying blind. The probe is cut from five seconds
    to one, which is still past the keyframe interval of any camera worth
    flying with.
    """
    scheme = urlsplit(url).scheme.lower()
    timeout_us = str(int(SOCKET_TIMEOUT_S * 1_000_000))
    opts = [("protocol_whitelist", _PROTOCOLS.get(scheme, "tcp"))]
    if scheme in ("rtsp", "rtsps"):
        # RTSP over TLS has no UDP transport.
        opts.append(("rtsp_transport", TRANSPORT_TCP if scheme == "rtsps" else transport))
        # ffmpeg 4's "timeout" for RTSP means "listen for a camera to connect",
        # the opposite of what is wanted; its socket timeout is "stimeout".
        major = ffmpeg_major(version)
        opts.append(("stimeout" if major is not None and major < 5 else "timeout", timeout_us))
    elif scheme in ("udp", "srt"):
        opts.append(("timeout", timeout_us))
    else:
        opts.append(("rw_timeout", timeout_us))
    opts += [("fflags", "nobuffer"), ("analyzeduration", "1000000"), ("probesize", "1000000")]
    return opts


def _ffconcat_quote(text: str) -> str:
    """Single-quote *text* for an ffconcat line; a quote inside becomes '\\''."""
    return "'" + text.replace("'", "'\\''") + "'"


def concat_list(url: str, transport: str = DEFAULT_TRANSPORT, version: str = "") -> str:
    """The ffconcat file that hands ffmpeg the address, password included."""
    lines = ["ffconcat version 1.0", f"file {_ffconcat_quote(url)}"]
    lines += [f"option {k} {_ffconcat_quote(v)}" for k, v in input_options(url, transport, version)]
    return "\n".join(lines) + "\n"


def _output_args() -> list[str]:
    return [
        "-an", "-sn", "-dn",
        "-vf", f"scale='min({MAX_WIDTH},iw)':-2",
        "-f", "mjpeg", "-q:v", str(JPEG_QUALITY),
        "pipe:1",
    ]


def ffmpeg_command(ffmpeg: str, url: str, transport: str = DEFAULT_TRANSPORT, *,
                   list_path: str = "", version: str = "") -> list[str]:
    """The argv that turns one camera into JPEG frames on stdout.

    With *list_path* the address (and its password) is in that ffconcat file
    and the argv carries only the file's name: the outer ffmpeg may open
    nothing but ``file``, the camera nothing but its own protocols. Without
    it the address goes on the command line, which only an ffmpeg older than
    5 needs (see :func:`hides_password`).
    """
    cmd = [ffmpeg, "-hide_banner", "-nostdin", "-loglevel", "error"]
    if list_path:
        cmd += [
            "-protocol_whitelist", "file",
            "-f", "concat", "-safe", "0",
            "-analyzeduration", "1000000", "-probesize", "1000000",
            "-flags", "low_delay",
            "-i", list_path,
        ]
    else:
        for key, value in input_options(url, transport, version):
            cmd += [f"-{key}", value]
        cmd += ["-flags", "low_delay", "-i", url]
    return cmd + _output_args()


class JpegSplitter:
    """Cut ffmpeg's MJPEG byte stream into whole JPEG images.

    The mjpeg muxer writes the images back to back with nothing between them,
    so an image is everything from a start-of-image marker (``FF D8``) to the
    next end-of-image marker (``FF D9``). Neither can occur inside the
    compressed data, where a literal ``FF`` is always followed by ``00``.
    """

    SOI = b"\xff\xd8"
    EOI = b"\xff\xd9"

    def __init__(self, max_bytes: int = MAX_FRAME_BYTES) -> None:
        self._buf = bytearray()
        self._scan = 0
        self._max = max_bytes

    def feed(self, data: bytes) -> list[bytes]:
        """Add *data*; return every image it completed, oldest first."""
        self._buf += data
        frames: list[bytes] = []
        while True:
            start = self._buf.find(self.SOI)
            if start < 0:
                # Keep a trailing FF: it may be the first half of a marker.
                del self._buf[:max(0, len(self._buf) - 1)]
                self._scan = 0
                break
            if start:
                del self._buf[:start]
                self._scan = 0
            end = self._buf.find(self.EOI, max(2, self._scan))
            if end < 0:
                if len(self._buf) > self._max:
                    self._buf.clear()
                    self._scan = 0
                else:
                    self._scan = max(2, len(self._buf) - 1)
                break
            frames.append(bytes(self._buf[:end + 2]))
            del self._buf[:end + 2]
            self._scan = 0
        return frames


def jpeg_size(data: bytes) -> tuple[int, int] | None:
    """``(width, height)`` from a JPEG's frame header, or None."""
    i = 2
    n = len(data)
    while i + 9 < n:
        if data[i] != 0xFF:
            return None
        marker = data[i + 1]
        if marker in (0xD8, 0x01) or 0xD0 <= marker <= 0xD7:
            i += 2
            continue
        seg = (data[i + 2] << 8) | data[i + 3]
        if marker in (0xC0, 0xC1, 0xC2):
            height = (data[i + 5] << 8) | data[i + 6]
            width = (data[i + 7] << 8) | data[i + 8]
            return width, height
        i += 2 + seg
    return None


def _write_private(path: str, text: str) -> None:
    """Create *path* readable by this account only, and never over another file."""
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)


def _unlink(path: str, quiet: bool = False) -> bool:
    """Remove *path*; whether it is gone now.

    Windows refuses to delete a file another process has open, and ffmpeg
    keeps its input list open for as long as it runs. That is not a failure
    worth a warning while ffmpeg runs (*quiet*): the caller tries again once
    it has exited.
    """
    try:
        os.unlink(path)
    except FileNotFoundError:
        return True
    except OSError as exc:
        if not quiet:
            logger.warning("could not remove %s: %s", path, exc)
        return False
    return True


# ---------------------------------------------------------------------------
# One RTSP camera
# ---------------------------------------------------------------------------

class VideoStream:
    """One camera: an ffmpeg process while somebody is watching, none otherwise.

    A supervisor thread owns the process. It starts it on the first
    :meth:`demand`, restarts it with a backoff when it exits or goes silent,
    and stops it once nobody has asked for a frame in :data:`IDLE_STOP_S`.
    Each supervisor carries a generation number, so a process being torn down
    can never publish a frame or a state into the one that replaced it.
    """

    def __init__(
        self,
        config: dict[str, Any],
        decoder: Callable[[], tuple[str, str]],
        workdir: Callable[[], str],
        *,
        popen: Callable[..., Any] = subprocess.Popen,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self.config = dict(config)
        self._decoder = decoder
        self._workdir = workdir
        self._popen = popen
        self._clock = clock
        self._cond = threading.Condition()
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._gen = 0
        self._runs = 0
        self._last_demand = 0.0
        self._state = STATE_IDLE
        self._message = ""
        self._frame: bytes = b""
        self._seq = 0
        self._frame_at = 0.0
        self._size: tuple[int, int] | None = None
        self._fps = 0.0
        self._proc: Any = None

    # -- the address ffmpeg opens, and what may be said about it ------------

    def _open_url(self) -> str:
        c = self.config
        return with_credentials(c["url"], c.get("username", ""), c.get("password", ""))

    def _scrub(self, text: str, list_path: str = "") -> str:
        out = text.replace(self._open_url(), self.config["url"])
        if list_path:
            out = out.replace(list_path, display_address(self.config["url"]))
        return redact(out, self.config.get("password", ""))

    # -- what the HTTP layer calls ------------------------------------------

    def demand(self) -> None:
        """Somebody wants frames: keep the decoder running, start it if needed."""
        with self._cond:
            self._last_demand = self._clock()
            if self._stop.is_set():
                return
            if self._thread is not None and self._thread.is_alive():
                return
            self._gen += 1
            gen = self._gen
            if self._state in (STATE_IDLE, STATE_UNAVAILABLE):
                self._state, self._message = STATE_STARTING, "Connecting to the camera."
            self._thread = threading.Thread(
                target=self._supervise, args=(gen,),
                name=f"corvus-video-{self.config['id']}", daemon=True)
            self._thread.start()

    def wait_frame(self, after: int, timeout: float,
                   stopping: Callable[[], bool] | None = None) -> tuple[int, bytes] | None:
        """The newest frame once its number is above *after*, or None on timeout."""
        deadline = self._clock() + max(0.0, timeout)
        with self._cond:
            while self._seq <= after or not self._frame:
                if stopping is not None and stopping():
                    return None
                remaining = deadline - self._clock()
                if remaining <= 0 or self._stop.is_set():
                    return None
                self._cond.wait(min(remaining, 0.25))
            return self._seq, self._frame

    def status(self) -> dict[str, Any]:
        """State, the last message, and what the picture looks like."""
        with self._cond:
            now = self._clock()
            state = self._state
            if state == STATE_LIVE and self._frame_at and now - self._frame_at > STALL_S:
                state = STATE_STALLED
            return {
                "state": state,
                "message": self._message if state != STATE_STALLED else
                f"No picture for {int(now - self._frame_at)} s.",
                "seq": self._seq,
                "width": self._size[0] if self._size else 0,
                "height": self._size[1] if self._size else 0,
                "fps": round(self._fps, 1),
                "age": round(now - self._frame_at, 1) if self._frame_at else None,
                "running": self._thread is not None and self._thread.is_alive(),
            }

    def stop(self, join: bool = True) -> None:
        """Stop the decoder for good and, with *join*, wait for its thread."""
        self._stop.set()
        with self._cond:
            proc = self._proc
            thread = self._thread
            self._cond.notify_all()
        if proc is not None:
            _signal_term(proc)
        if join and thread is not None:
            thread.join(TERM_TIMEOUT_S * 2 + 2.0)

    def join(self, timeout: float) -> None:
        """Wait for a stream :meth:`stop` was called on without *join*."""
        thread = self._thread
        if thread is not None:
            thread.join(timeout)

    def alive(self) -> bool:
        """Whether the supervisor thread is still running."""
        thread = self._thread
        return thread is not None and thread.is_alive()

    # -- the supervisor -----------------------------------------------------

    def _set(self, gen: int, state: str, message: str) -> None:
        with self._cond:
            if gen == self._gen:
                self._state, self._message = state, message
                self._cond.notify_all()

    def _publish(self, gen: int, frame: bytes) -> None:
        now = self._clock()
        with self._cond:
            if gen != self._gen:
                return
            if self._frame_at:
                dt = now - self._frame_at
                if dt > 0:
                    rate = 1.0 / dt
                    self._fps = rate if not self._fps else self._fps * 0.9 + rate * 0.1
            if self._size is None or self._seq % 50 == 0:
                self._size = jpeg_size(frame) or self._size
            self._frame = frame
            self._seq = next(_FRAME_SEQ)
            self._frame_at = now
            self._state, self._message = STATE_LIVE, ""
            self._cond.notify_all()

    def _idle(self) -> bool:
        return self._clock() - self._last_demand > IDLE_STOP_S

    def _supervise(self, gen: int) -> None:
        failures = 0
        try:
            while not self._stop.is_set():
                ffmpeg, version = self._decoder()
                if not ffmpeg:
                    self._set(gen, STATE_UNAVAILABLE,
                              "ffmpeg was not found. Video needs it to decode the camera.")
                    return
                ran_live = self._run_once(gen, ffmpeg, version)
                if self._stop.is_set() or self._idle():
                    return
                failures = 0 if ran_live else failures + 1
                pause = RETRY_BACKOFF_S[min(failures, len(RETRY_BACKOFF_S) - 1)]
                if self._stop.wait(pause):
                    return
                if self._idle():
                    return
        except Exception:  # noqa: BLE001 - a supervisor must never take the server down
            logger.exception("video supervisor for %s failed", self.config.get("id"))
            self._set(gen, STATE_ERROR, "The video decoder failed. See the log.")
        finally:
            with self._cond:
                if gen == self._gen:
                    if self._state not in (STATE_ERROR, STATE_UNAVAILABLE):
                        self._state, self._message = STATE_IDLE, ""
                    # A picture from before the decoder stopped is not what the
                    # camera shows now; the next viewer waits for a fresh one.
                    self._frame = b""
                    self._frame_at = 0.0
                    self._fps = 0.0
                    self._cond.notify_all()

    def _command(self, ffmpeg: str, version: str) -> tuple[list[str], str]:
        """The argv for one run, and the list file it reads (``""`` for none)."""
        url = self._open_url()
        transport = self.config.get("transport", DEFAULT_TRANSPORT)
        if not hides_password(version):
            return ffmpeg_command(ffmpeg, url, transport, version=version), ""
        self._runs += 1
        path = os.path.join(self._workdir(), f"{self.config['id']}-{self._runs}.ffconcat")
        _unlink(path)
        _write_private(path, concat_list(url, transport, version))
        return ffmpeg_command(ffmpeg, url, transport, list_path=path, version=version), path

    def _run_once(self, gen: int, ffmpeg: str, version: str) -> bool:
        """One ffmpeg run, start to exit. True when it produced a picture."""
        self._set(gen, STATE_STARTING, "Connecting to the camera.")
        try:
            cmd, list_path = self._command(ffmpeg, version)
        except OSError as exc:
            self._set(gen, STATE_ERROR, f"The camera address could not be handed to ffmpeg: {exc}")
            return False
        try:
            proc = self._popen(
                cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                bufsize=0, creationflags=_no_window_flags(), env=child_env(),
            )
        except OSError as exc:
            if list_path:
                _unlink(list_path)
            self._set(gen, STATE_ERROR, f"ffmpeg could not be started: {exc.strerror or exc}")
            return False
        with self._cond:
            self._proc = proc
        # The list file's name stays scrubbed from messages after the file is gone.
        scrub_path = list_path
        errors: collections.deque[str] = collections.deque(maxlen=20)
        got_frame = threading.Event()
        started = self._clock()

        def read_frames() -> None:
            splitter = JpegSplitter()
            stream = proc.stdout
            read = getattr(stream, "read1", None) or stream.read
            while True:
                try:
                    chunk = read(65536)
                except (OSError, ValueError):
                    return
                if not chunk:
                    return
                frames = splitter.feed(chunk)
                if frames:
                    # Only the newest one matters; older ones are already late.
                    self._publish(gen, frames[-1])
                    got_frame.set()

        def read_errors() -> None:
            while True:
                try:
                    raw = proc.stderr.readline(MAX_ERROR_LINE)
                except (OSError, ValueError):
                    return
                if not raw:
                    return
                line = raw.decode("utf-8", "replace").strip()
                if line:
                    errors.append(self._scrub(line, scrub_path))

        readers = [
            threading.Thread(target=read_frames, name=f"corvus-video-out-{gen}", daemon=True),
            threading.Thread(target=read_errors, name=f"corvus-video-err-{gen}", daemon=True),
        ]
        for t in readers:
            t.start()

        reason = ""
        try:
            while proc.poll() is None:
                if self._stop.wait(0.25):
                    break
                if self._idle():
                    break
                if list_path and (got_frame.is_set() or self._clock() - started > LIST_KEEP_S):
                    # On Windows this fails while ffmpeg holds the file open;
                    # the finally below removes it once ffmpeg has exited.
                    if _unlink(list_path, quiet=True):
                        list_path = ""
                with self._cond:
                    last = self._frame_at if got_frame.is_set() else started
                if self._clock() - last > WATCHDOG_S:
                    reason = (f"No picture for {int(WATCHDOG_S)} s. Reconnecting."
                              if got_frame.is_set() else
                              f"No picture after {int(WATCHDOG_S)} s. Check the address and the network.")
                    logger.info("video %s: %s", self.config.get("id"), reason)
                    break
        finally:
            _terminate(proc)
            for t in readers:
                t.join(TERM_TIMEOUT_S)
            for pipe in (proc.stdout, proc.stderr):
                try:
                    if pipe is not None:
                        pipe.close()
                except OSError:
                    pass
            if list_path:
                _unlink(list_path)
            with self._cond:
                if self._proc is proc:
                    self._proc = None

        if not (self._stop.is_set() or self._idle()):
            detail = reason or self._last_error(errors)
            if not detail:
                detail = ("The camera stopped sending." if got_frame.is_set()
                          else "ffmpeg ended without a picture.")
            self._set(gen, STATE_ERROR, detail)
            logger.info("video %s ended: %s", self.config.get("id"), detail)
        return got_frame.is_set()

    @staticmethod
    def _last_error(errors: collections.deque[str]) -> str:
        """The most telling of ffmpeg's last lines.

        ffmpeg ends with a generic "Error opening input files" after the line
        that says why; the specific one is the one worth showing.
        """
        lines = list(errors)
        for line in reversed(lines):
            if not line.startswith("Error opening input file"):
                return explain_error(line)
        return explain_error(lines[-1]) if lines else ""


def _signal_term(proc: Any) -> None:
    try:
        if proc.poll() is None:
            proc.terminate()
    except OSError:
        pass


def _terminate(proc: Any) -> None:
    """SIGTERM, then SIGKILL after :data:`TERM_TIMEOUT_S`. Never raises."""
    _signal_term(proc)
    try:
        proc.wait(TERM_TIMEOUT_S)
        return
    except subprocess.TimeoutExpired:
        pass
    except OSError:
        return
    try:
        proc.kill()
        proc.wait(TERM_TIMEOUT_S)
    except (OSError, subprocess.TimeoutExpired):
        logger.warning("ffmpeg pid %s did not exit after SIGKILL", getattr(proc, "pid", "?"))


# ---------------------------------------------------------------------------
# WebRTC signalling (WHEP)
# ---------------------------------------------------------------------------

class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """A WHEP answer that redirects is refused, not followed with the credentials."""

    def redirect_request(self, *args: Any, **kwargs: Any) -> None:  # noqa: D401
        return None


_WHEP_OPENER = urllib.request.build_opener(_NoRedirect())


def _default_urlopen(request: urllib.request.Request, timeout: float) -> Any:
    return _WHEP_OPENER.open(request, timeout=timeout)


def auth_header(stream: dict[str, Any]) -> str:
    """``Authorization`` for a WHEP server: Basic with a user, Bearer with a token alone."""
    user = stream.get("username", "")
    password = stream.get("password", "")
    if user:
        token = base64.b64encode(f"{user}:{password}".encode()).decode("ascii")
        return f"Basic {token}"
    return f"Bearer {password}" if password else ""


def _origin(url: str) -> tuple[str, str, int | None]:
    parts = urlsplit(url)
    default = {"http": 80, "https": 443, "rtsp": 554, "rtsps": 322}.get(parts.scheme.lower())
    try:
        port = parts.port or default
    except ValueError:
        port = None
    return parts.scheme.lower(), (parts.hostname or "").lower(), port


def same_origin(a: str, b: str) -> bool:
    """Whether *b* is on the scheme, host and port of *a*."""
    return _origin(a) == _origin(b)


def _whep_error(code: int, body: bytes) -> str:
    text = body.decode("utf-8", "replace").strip()
    # MediaMTX and go2rtc answer errors as JSON {"error": "..."}.
    match = re.search(r'"error"\s*:\s*"([^"]{1,300})"', text)
    detail = match.group(1) if match else re.sub(r"\s+", " ", text)[:200]
    if code in (401, 403):
        lead = "The camera server refused the user or password."
    elif code == 404:
        lead = "The camera server has no stream at that address."
    elif code in (301, 302, 303, 307, 308):
        lead = "The camera server redirected the request, which Corvus does not follow."
    else:
        lead = f"The camera server refused the connection ({code})."
    return f"{lead} ({detail})" if detail else lead


class VideoService:
    """The configured cameras: RTSP decoders, and WebRTC signalling sessions.

    Thread-safe: the HTTP handler threads call every method concurrently.
    """

    def __init__(
        self,
        config: dict[str, Any] | None = None,
        *,
        popen: Callable[..., Any] = subprocess.Popen,
        clock: Callable[[], float] = time.monotonic,
        resolver: Callable[[str], tuple[str, str]] = resolve_ffmpeg,
        version: Callable[[str], str] = ffmpeg_version,
        urlopen: Callable[[urllib.request.Request, float], Any] = _default_urlopen,
    ) -> None:
        self._lock = threading.Lock()
        self._popen = popen
        self._clock = clock
        self._resolver = resolver
        self._version_of = version
        self._urlopen = urlopen
        self._settings = settings(config)
        self._configs: dict[str, dict[str, Any]] = {}
        self._streams: dict[str, VideoStream] = {}
        # Streams replaced or removed while running: stopped at once, joined at
        # shutdown, so a settings change never waits on an ffmpeg exiting.
        self._retired: list[VideoStream] = []
        # token -> {"stream", "location", "auth", "created"}
        self._sessions: dict[str, dict[str, Any]] = {}
        self._tmpdir = ""
        self._ffmpeg = ""
        self._source = ""
        self._version = ""
        self._resolved_at: float | None = None
        self._closed = False
        self._rebuild(self._settings)

    # -- ffmpeg ---------------------------------------------------------------

    def _resolve(self, force: bool = False) -> str:
        with self._lock:
            now = self._clock()
            stale = (self._resolved_at is None or
                     (not self._ffmpeg and now - self._resolved_at > RESOLVE_RETRY_S))
            if not (force or stale):
                return self._ffmpeg
            self._resolved_at = now
            configured = self._settings.get("ffmpeg", "")
        path, source = self._resolver(configured)
        version = self._version_of(path) if path else ""
        with self._lock:
            self._ffmpeg, self._source, self._version = path, source, version
        return path

    def ffmpeg_path(self) -> str:
        """The ffmpeg in use, or ``""``."""
        return self._resolve()

    def _decoder(self) -> tuple[str, str]:
        path = self._resolve()
        with self._lock:
            return path, self._version

    def _workdir(self) -> str:
        """A folder only this account can enter, made on first use."""
        with self._lock:
            if not self._tmpdir or not os.path.isdir(self._tmpdir):
                self._tmpdir = tempfile.mkdtemp(prefix="corvus-video-")
                os.chmod(self._tmpdir, 0o700)
            return self._tmpdir

    # -- settings -------------------------------------------------------------

    def _new_stream(self, cfg: dict[str, Any]) -> VideoStream:
        return VideoStream(cfg, self._decoder, self._workdir, popen=self._popen, clock=self._clock)

    def _rebuild(self, new: dict[str, Any]) -> tuple[list[VideoStream], list[str]]:
        """Swap in *new*; return the decoders to stop and the WHEP sessions to close."""
        retired: list[VideoStream] = []
        keep: dict[str, VideoStream] = {}
        configs = {cfg["id"]: cfg for cfg in new["streams"]}
        for cfg in new["streams"]:
            if cfg.get("kind", KIND_RTSP) != KIND_RTSP:
                continue
            old = self._streams.get(cfg["id"])
            if old is not None and _same_source(old.config, cfg):
                old.config = dict(cfg)
                keep[cfg["id"]] = old
                continue
            if old is not None:
                retired.append(old)
            keep[cfg["id"]] = self._new_stream(cfg)
        retired += [s for sid, s in self._streams.items() if sid not in keep]
        # A WHEP session holds the credentials it was opened with; one whose
        # camera was removed or re-addressed is closed rather than left to the
        # server's own timeout.
        stale = [token for token, s in self._sessions.items()
                 if s["stream"] not in configs
                 or not _same_source(s["config"], configs[s["stream"]])]
        self._streams = keep
        self._configs = configs
        self._settings = new
        return retired, stale

    def apply_settings(self, raw: Any) -> None:
        """Replace the camera list and the ffmpeg path.

        A camera whose address, credentials or transport did not change keeps
        its running decoder, so renaming one does not blank its window.
        """
        new = settings(raw)
        with self._lock:
            if self._closed:
                return
            decoder_changed = new.get("ffmpeg", "") != self._settings.get("ffmpeg", "")
            retired, stale = self._rebuild(new)
            if decoder_changed:
                retired += list(self._streams.values())
                self._streams = {sid: self._new_stream(s.config) for sid, s in self._streams.items()}
            closing = [self._sessions.pop(token) for token in stale]
        for s in retired:
            s.stop(join=False)
        with self._lock:
            self._retired = [s for s in self._retired if s.alive()] + retired
        self._close_sessions(closing)
        if decoder_changed:
            self._resolve(force=True)

    def settings(self) -> dict[str, Any]:
        """The stored settings, passwords included. Never send this to a browser."""
        with self._lock:
            return {"streams": [dict(s) for s in self._settings["streams"]],
                    "ffmpeg": self._settings.get("ffmpeg", "")}

    # -- RTSP frames ----------------------------------------------------------

    def has(self, stream_id: str) -> bool:
        with self._lock:
            return stream_id in self._configs

    def frame(self, stream_id: str, after: int, timeout: float = FRAME_WAIT_S,
              stopping: Callable[[], bool] | None = None) -> dict[str, Any]:
        """The next frame of one camera, or its state when none came in time.

        ``{"jpeg": bytes, "seq": n}`` or ``{"state": ..., "message": ..., "seq": n}``.
        Raises KeyError for a camera that is not configured, and WrongKind for
        a WebRTC one, which never has frames here.
        """
        with self._lock:
            if stream_id not in self._configs:
                raise KeyError(stream_id)
            stream = self._streams.get(stream_id)
            closed = self._closed
        if stream is None:
            raise WrongKind("This camera plays over WebRTC.")
        if closed:
            return {"state": STATE_IDLE, "message": "Corvus is shutting down.", "seq": 0}
        if not self._resolve():
            return {"state": STATE_UNAVAILABLE, "seq": 0,
                    "message": "ffmpeg was not found. Video needs it to decode the camera."}
        stream.demand()
        got = stream.wait_frame(after, timeout, stopping)
        if got is not None:
            return {"seq": got[0], "jpeg": got[1]}
        status = stream.status()
        return {"state": status["state"], "message": status["message"], "seq": status["seq"]}

    # -- WebRTC signalling ------------------------------------------------------

    def whep_offer(self, stream_id: str, sdp: str) -> dict[str, Any]:
        """Hand the window's SDP offer to the camera's WHEP server; return its answer.

        ``{"ok": True, "sdp": answer, "session": token}``, or ``{"ok": False,
        "error": ..., "status": http status to answer with}``. Raises KeyError
        for an unknown camera and WrongKind for an RTSP one.
        """
        with self._lock:
            cfg = self._configs.get(stream_id)
            closed = self._closed
        if cfg is None:
            raise KeyError(stream_id)
        if cfg.get("kind") != KIND_WEBRTC:
            raise WrongKind("This camera is decoded by ffmpeg, not played over WebRTC.")
        if closed:
            return {"ok": False, "status": 503, "error": "Corvus is shutting down."}
        if not isinstance(sdp, str) or not sdp.startswith("v=0") or len(sdp) > MAX_SDP_BYTES:
            return {"ok": False, "status": 400, "error": "That is not an SDP offer."}

        headers = {"Content-Type": "application/sdp", "Accept": "application/sdp"}
        auth = auth_header(cfg)
        if auth:
            headers["Authorization"] = auth
        request = urllib.request.Request(cfg["url"], data=sdp.encode("utf-8"),
                                         headers=headers, method="POST")
        try:
            response = self._urlopen(request, WHEP_TIMEOUT_S)
        except urllib.error.HTTPError as exc:
            body = b""
            try:
                body = exc.read(4096) or b""
            except Exception:  # noqa: BLE001 - the status alone is enough to report
                pass
            return {"ok": False, "status": 502,
                    "error": redact(_whep_error(exc.code, body), cfg.get("password", ""))}
        except (urllib.error.URLError, OSError, ValueError) as exc:
            reason = getattr(exc, "reason", exc)
            return {"ok": False, "status": 502,
                    "error": explain_error(f"The camera server did not answer: {reason}")}
        try:
            with response:
                code = getattr(response, "status", 200)
                body = response.read(MAX_SDP_BYTES + 1)
                location = response.headers.get("Location", "") if response.headers else ""
        except (OSError, ValueError) as exc:
            return {"ok": False, "status": 502, "error": f"The camera server's answer was cut off: {exc}"}
        if code not in (200, 201) or len(body) > MAX_SDP_BYTES:
            return {"ok": False, "status": 502, "error": _whep_error(code, b"")}
        answer = body.decode("utf-8", "replace")
        if not answer.startswith("v=0"):
            return {"ok": False, "status": 502,
                    "error": "The camera server answered with something that is not an SDP answer."}

        resource = urljoin(cfg["url"], location) if location else ""
        if resource and not same_origin(cfg["url"], resource):
            # The credentials go only where the operator sent them. A session
            # somewhere else is left to the server's own timeout.
            logger.warning("WHEP session for %s points at another origin; not used", stream_id)
            resource = ""
        token = secrets.token_urlsafe(16)
        evicted: list[dict[str, Any]] = []
        with self._lock:
            self._sessions[token] = {"stream": stream_id, "config": dict(cfg),
                                     "location": resource, "created": self._clock()}
            while len(self._sessions) > MAX_WHEP_SESSIONS:
                oldest = min(self._sessions, key=lambda t: self._sessions[t]["created"])
                evicted.append(self._sessions.pop(oldest))
        self._close_sessions(evicted)
        return {"ok": True, "sdp": answer, "session": token}

    def whep_close(self, token: str) -> bool:
        """End one WebRTC session on the camera's server. False when it was not open."""
        with self._lock:
            session = self._sessions.pop(token, None) if isinstance(token, str) else None
        if session is None:
            return False
        self._close_sessions([session])
        return True

    def _delete_session(self, session: dict[str, Any]) -> None:
        if not session.get("location"):
            return
        headers = {}
        auth = auth_header(session["config"])
        if auth:
            headers["Authorization"] = auth
        request = urllib.request.Request(session["location"], headers=headers, method="DELETE")
        try:
            response = self._urlopen(request, WHEP_CLOSE_TIMEOUT_S)
            try:
                response.close()
            except Exception:  # noqa: BLE001 - nothing left to do with it
                pass
        except Exception as exc:  # noqa: BLE001 - the server times the session out anyway
            logger.info("closing a WHEP session failed: %s", exc)

    def _close_sessions(self, sessions: list[dict[str, Any]], budget: float = WHEP_CLOSE_TIMEOUT_S) -> None:
        """DELETE every session at once, and wait for them at most *budget* seconds."""
        sessions = [s for s in sessions if s.get("location")]
        if not sessions:
            return
        threads = [threading.Thread(target=self._delete_session, args=(s,),
                                    name="corvus-video-whep-close", daemon=True) for s in sessions]
        for t in threads:
            t.start()
        deadline = time.monotonic() + budget
        for t in threads:
            t.join(max(0.0, deadline - time.monotonic()))

    # -- status -----------------------------------------------------------------

    def status(self) -> dict[str, Any]:
        """Everything the Video page shows, without a single password."""
        path = self._resolve()
        with self._lock:
            configs = [dict(c) for c in self._settings["streams"]]
            decoders = dict(self._streams)
            sessions = collections.Counter(s["stream"] for s in self._sessions.values())
            configured = self._settings.get("ffmpeg", "")
            source, version = self._source, self._version
        out = []
        for cfg in configs:
            entry = public_stream(cfg)
            decoder = decoders.get(cfg["id"])
            if decoder is not None:
                entry.update(decoder.status())
            else:
                entry.update({"state": STATE_IDLE, "message": "", "seq": 0, "width": 0,
                              "height": 0, "fps": 0.0, "age": None, "running": False})
            entry["sessions"] = sessions.get(cfg["id"], 0)
            out.append(entry)
        reason = ""
        if not path:
            reason = (f"No ffmpeg at {configured}." if configured else
                      "ffmpeg was not found. RTSP cameras need it to decode the video. "
                      "Install ffmpeg, or give its path below.")
        return {
            "available": bool(path),
            "reason": reason,
            "ffmpeg": {"path": path, "source": source, "version": version,
                       "configured": configured,
                       "hides_password": bool(path) and hides_password(version)},
            "streams": out,
            "max_streams": MAX_STREAMS,
            "schemes": list(SCHEMES),
            "transports": list(TRANSPORTS),
            "kinds": list(KINDS),
        }

    # -- teardown -----------------------------------------------------------

    def shutdown(self) -> None:
        """Stop every decoder, close every WHEP session, join every thread. Never raises."""
        with self._lock:
            self._closed = True
            streams = list(self._streams.values()) + self._retired
            self._retired = []
            sessions = list(self._sessions.values())
            self._sessions = {}
            tmpdir, self._tmpdir = self._tmpdir, ""
        for s in streams:
            try:
                s.stop(join=False)
            except Exception:  # noqa: BLE001 - teardown never raises
                logger.exception("video stream stop failed")
        try:
            self._close_sessions(sessions, budget=2.0)
        except Exception:  # noqa: BLE001 - teardown never raises
            logger.exception("closing WHEP sessions failed")
        for s in streams:
            try:
                s.join(TERM_TIMEOUT_S * 2 + 2.0)
            except Exception:  # noqa: BLE001 - teardown never raises
                logger.exception("video stream join failed")
        if tmpdir:
            shutil.rmtree(tmpdir, ignore_errors=True)


def _same_source(a: dict[str, Any], b: dict[str, Any]) -> bool:
    return all(a.get(k) == b.get(k) for k in ("kind", "url", "username", "password", "transport"))
