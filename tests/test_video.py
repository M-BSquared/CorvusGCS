"""Camera video: addresses, the ffmpeg command, frame splitting and the decoder lifecycle.

ffmpeg is faked with a process object backed by real OS pipes, so the reader
threads block and wake exactly as they do against the real binary, and the
lifecycle assertions (one decoder per camera, stopped when nobody watches,
terminated and joined at shutdown) are about real threads.

The live pipeline was checked by hand against ffmpeg 8.1 with an H.264 test
source; nothing here needs ffmpeg installed.
"""
from __future__ import annotations

import io
import os
import stat
import struct
import subprocess
import threading
import time
import urllib.error
from typing import Any

import pytest

from conftest import posix_permissions
from corvus import video
from corvus.config import CorvusConfig, _build_config, _config_to_dict, to_public_dict


def jpeg(width: int = 64, height: int = 48, fill: bytes = b"\x11\x22") -> bytes:
    """A minimal JPEG: SOI, a SOF0 header with the size, some data, EOI."""
    sof = b"\xff\xc0" + struct.pack(">HBHHB", 11, 8, height, width, 1) + b"\x01\x11\x00"
    return b"\xff\xd8" + b"\xff\xe0\x00\x04ab" + sof + fill * 8 + b"\xff\xd9"


# ---------------------------------------------------------------------------
# Addresses and credentials
# ---------------------------------------------------------------------------

def test_credentials_are_taken_out_of_the_address_and_put_back_intact() -> None:
    url = "rtsp://admin:p%40ss%3Aw%2Frd@192.168.1.10:554/stream1?x=1"
    clean, user, password = video.split_credentials(url)
    assert clean == "rtsp://192.168.1.10:554/stream1?x=1"
    assert (user, password) == ("admin", "p@ss:w/rd")
    assert video.split_credentials(video.with_credentials(clean, user, password)) == (clean, user, password)


def test_an_unencoded_at_sign_in_the_password_still_splits_at_the_host() -> None:
    clean, user, password = video.split_credentials("rtsp://admin:p@ss@cam.local/main")
    assert (clean, user, password) == ("rtsp://cam.local/main", "admin", "p@ss")


def test_the_window_title_shows_where_the_camera_is_and_nothing_secret() -> None:
    assert video.display_address("rtsp://u:secret@192.168.144.25:8554/main.264") == "192.168.144.25:8554/main.264"


@pytest.mark.parametrize("url", [
    "rtsp://192.168.144.25:8554/main.264",
    "rtsps://cam.example/live",
    "udp://@:5600",
    "srt://10.0.0.2:9000",
    "http://10.0.0.3/mjpeg",
])
def test_camera_addresses_ffmpeg_can_open_are_accepted(url: str) -> None:
    assert video.url_problem(url) == ""


@pytest.mark.parametrize("url", [
    "",
    "file:///etc/passwd",
    "concat:/etc/passwd|/etc/hosts",
    "-i /etc/passwd",
    "rtsp://cam /main",
    "rtsp:///main",
    "udp://",
    "rtsp://cam:notaport/main",
])
def test_anything_that_is_not_a_camera_address_is_refused(url: str) -> None:
    assert video.url_problem(url), url


def test_a_password_is_scrubbed_from_what_ffmpeg_says() -> None:
    assert video.redact("401 for rtsp://admin:s3cr%20t@h/", "s3cr t") == "401 for rtsp://admin:***@h/"


# ---------------------------------------------------------------------------
# Settings
# ---------------------------------------------------------------------------

def test_settings_keep_good_cameras_and_drop_broken_ones() -> None:
    out = video.settings({"streams": [
        {"id": "a", "name": "Gimbal", "url": "rtsp://admin:pw@10.0.0.2/main"},
        {"id": "a", "name": "duplicate id", "url": "rtsp://10.0.0.3/x"},
        {"id": "b", "url": "file:///etc/passwd"},
        "not a camera",
        {"id": "c", "url": "rtsp://10.0.0.4/x", "password": "orphan", "transport": "bogus"},
    ], "ffmpeg": 7})
    assert [s["id"] for s in out["streams"]] == ["a", "c"]
    gimbal = out["streams"][0]
    assert gimbal["url"] == "rtsp://10.0.0.2/main", "credentials never stay in the stored address"
    assert (gimbal["username"], gimbal["password"]) == ("admin", "pw")
    other = out["streams"][1]
    assert other["password"] == "", "a password without a user cannot be sent, so it is not kept"
    assert other["transport"] == video.DEFAULT_TRANSPORT
    assert other["name"] == "10.0.0.4/x", "an unnamed camera is called by its address"
    assert out["ffmpeg"] == ""


def test_the_camera_list_is_bounded() -> None:
    raw = {"streams": [{"id": f"s{i}", "url": f"rtsp://10.0.0.{i}/x"} for i in range(20)]}
    assert len(video.settings(raw)["streams"]) == video.MAX_STREAMS


def test_the_config_file_keeps_the_cameras_and_no_response_carries_a_password() -> None:
    cfg = _build_config({"video": {"streams": [
        {"id": "a", "name": "Nose", "url": "rtsp://pilot:hunter2@10.0.0.2/main"}]}})
    assert cfg.video["streams"][0]["password"] == "hunter2"
    on_disk = _config_to_dict(cfg)
    assert on_disk["video"]["streams"][0]["password"] == "hunter2", "the file is the secret store"
    public = to_public_dict(cfg)
    stream = public["video"]["streams"][0]
    assert "password" not in stream
    assert stream["has_password"] is True
    assert "hunter2" not in repr(public)
    assert cfg.video is not public["video"], "redaction must not touch the live config"
    assert CorvusConfig().video is None


# ---------------------------------------------------------------------------
# The ffmpeg command
# ---------------------------------------------------------------------------

def test_the_ffmpeg_command_decodes_to_jpeg_on_stdout() -> None:
    cmd = video.ffmpeg_command("/usr/bin/ffmpeg", "rtsp://10.0.0.2/main", "udp", version="4.4.2")
    assert cmd[0] == "/usr/bin/ffmpeg"
    assert cmd[cmd.index("-rtsp_transport") + 1] == "udp"
    assert cmd[cmd.index("-i") + 1] == "rtsp://10.0.0.2/main"
    assert cmd[cmd.index("-f", cmd.index("-i")) + 1] == "mjpeg"
    assert cmd[-1] == "pipe:1"
    assert "-nostdin" in cmd, "ffmpeg must never read the terminal Corvus was started from"


def test_rtsp_over_tls_is_always_tcp_and_other_schemes_get_no_rtsp_option() -> None:
    tls = dict(video.input_options("rtsps://cam/live", "udp"))
    assert tls["rtsp_transport"] == "tcp"
    assert "rtsp_transport" not in dict(video.input_options("udp://@:5600", "tcp")), (
        "ffmpeg refuses an option no input consumes"
    )


def test_the_password_never_goes_on_the_command_line() -> None:
    url = "rtsp://admin:s3cret@10.0.0.2/main"
    cmd = video.ffmpeg_command("ffmpeg", url, "tcp", list_path="/private/x.ffconcat", version="8.1.2")
    assert not any("s3cret" in arg for arg in cmd), "every account on the computer can read argv"
    assert cmd[cmd.index("-i") + 1] == "/private/x.ffconcat"
    assert cmd[cmd.index("-protocol_whitelist") + 1] == "file", (
        "the outer ffmpeg reads the list and nothing else"
    )
    listing = video.concat_list(url, "tcp", "8.1.2")
    assert "file 'rtsp://admin:s3cret@10.0.0.2/main'" in listing
    assert "option protocol_whitelist 'rtsp,tcp,udp,rtp'" in listing


def test_only_ffmpeg_5_and_later_can_take_the_address_from_a_file() -> None:
    assert video.hides_password("8.1.2") and video.hides_password("n7.1")
    assert video.hides_password("N-112345-gabcdef"), "a git build is newer than any release"
    assert not video.hides_password("4.4.2-0ubuntu0.22.04.1")


def test_a_quote_in_the_address_cannot_break_out_of_the_list_line() -> None:
    listing = video.concat_list("http://cam/a'b", "tcp", "8.1")
    assert "file 'http://cam/a'\\''b'" in listing


@pytest.mark.parametrize("url, whitelist", [
    ("rtsp://h/x", "rtsp,tcp,udp,rtp"),
    ("udp://@:5600", "udp,rtp"),
    ("http://h/stream.m3u8", "http,https,tcp,tls,crypto"),
])
def test_a_camera_can_reach_only_its_own_protocols_never_a_local_file(url: str, whitelist: str) -> None:
    opts = dict(video.input_options(url))
    assert opts["protocol_whitelist"] == whitelist
    assert "file" not in opts["protocol_whitelist"].split(",")


def test_every_network_wait_has_a_timeout_under_the_name_ffmpeg_uses() -> None:
    assert dict(video.input_options("rtsp://h/x", version="8.1"))["timeout"] == "8000000"
    old = dict(video.input_options("rtsp://h/x", version="4.4.2"))
    assert "stimeout" in old and "timeout" not in old, (
        "ffmpeg 4's rtsp timeout means listen for a connection, not give up on one"
    )
    assert "timeout" in dict(video.input_options("udp://@:5600"))
    assert "rw_timeout" in dict(video.input_options("https://h/x"))


def test_ffmpeg_errors_are_explained_in_words() -> None:
    assert video.explain_error("method DESCRIBE failed: 401 Unauthorized").startswith(
        "The camera refused the user or password.")
    assert video.explain_error("Connection refused").startswith("Nothing answered")
    assert video.explain_error("something new") == "something new"


# ---------------------------------------------------------------------------
# Frames
# ---------------------------------------------------------------------------

def test_frames_are_cut_whole_however_the_pipe_splits_them() -> None:
    frames = [jpeg(10, 10), jpeg(20, 10, b"\xff\x00"), jpeg(30, 10)]
    stream = b"junk" + b"".join(frames)
    whole = video.JpegSplitter().feed(stream)
    assert whole == frames
    byte_by_byte = video.JpegSplitter()
    got: list[bytes] = []
    for i in range(len(stream)):
        got += byte_by_byte.feed(stream[i:i + 1])
    assert got == frames


def test_a_runaway_frame_is_dropped_rather_than_grown_without_bound() -> None:
    splitter = video.JpegSplitter(max_bytes=64)
    assert splitter.feed(b"\xff\xd8" + b"\x00" * 200) == []
    assert splitter.feed(jpeg()) == [jpeg()], "and the next whole frame still comes through"


def test_the_picture_size_is_read_from_the_frame_header() -> None:
    assert video.jpeg_size(jpeg(1280, 720)) == (1280, 720)
    assert video.jpeg_size(b"\xff\xd8\xff\xd9") is None


# ---------------------------------------------------------------------------
# Finding ffmpeg
# ---------------------------------------------------------------------------

def _executable(tmp_path: Any, name: str = "ffmpeg") -> str:
    path = tmp_path / name
    path.write_text("#!/bin/sh\nexit 0\n")
    path.chmod(0o755)
    return str(path)


@pytest.mark.skipif(os.name == "nt", reason="POSIX executable bit")
def test_a_configured_ffmpeg_is_used_and_a_missing_one_is_not_replaced(tmp_path: Any) -> None:
    exe = _executable(tmp_path)
    assert video.resolve_ffmpeg(exe) == (exe, "configured")
    assert video.resolve_ffmpeg(str(tmp_path / "nope")) == ("", ""), (
        "the operator asked for that binary; quietly using another would hide the typo"
    )


@pytest.mark.skipif(os.name == "nt", reason="POSIX executable bit")
def test_the_environment_can_name_ffmpeg(tmp_path: Any, monkeypatch: pytest.MonkeyPatch) -> None:
    exe = _executable(tmp_path)
    monkeypatch.setenv(video.FFMPEG_ENV, exe)
    assert video.resolve_ffmpeg("") == (exe, "environment")


# ---------------------------------------------------------------------------
# The decoder lifecycle, against a fake ffmpeg on real pipes
# ---------------------------------------------------------------------------

class FakeFfmpeg:
    """A process with real stdout/stderr pipes, writing frames until stopped."""

    def __init__(self, argv: list[str], *, frames: int, interval: float,
                 stderr: bytes, exit_code: int | None) -> None:
        self.argv = argv
        self.pid = 4242
        self.returncode: int | None = None
        self.terminated = 0
        self.killed = 0
        out_r, self._out_w = os.pipe()
        err_r, self._err_w = os.pipe()
        self.stdout = os.fdopen(out_r, "rb", buffering=0)
        self.stderr = os.fdopen(err_r, "rb", buffering=0)
        self._done = threading.Event()
        self._frames, self._interval = frames, interval
        self._stderr, self._exit_code = stderr, exit_code
        threading.Thread(target=self._run, daemon=True).start()

    def _run(self) -> None:
        try:
            for _ in range(self._frames):
                if self._done.is_set():
                    return
                os.write(self._out_w, jpeg(1280, 720))
                time.sleep(self._interval)
            if self._stderr:
                os.write(self._err_w, self._stderr)
            if self._exit_code is not None:
                self._finish(self._exit_code)
        except OSError:
            pass

    def _finish(self, code: int) -> None:
        if self._done.is_set():
            return
        self.returncode = code
        self._done.set()
        for fd in (self._out_w, self._err_w):
            try:
                os.close(fd)
            except OSError:
                pass

    def poll(self) -> int | None:
        return self.returncode

    def terminate(self) -> None:
        self.terminated += 1
        self._finish(-15)

    def kill(self) -> None:
        self.killed += 1
        self._finish(-9)

    def wait(self, timeout: float | None = None) -> int:
        if not self._done.wait(timeout):
            raise subprocess.TimeoutExpired(self.argv, timeout)
        return self.returncode  # type: ignore[return-value]


class Launcher:
    """Stands in for subprocess.Popen and remembers every process it started."""

    def __init__(self, frames: int = 10_000, interval: float = 0.02,
                 stderr: bytes = b"", exit_code: int | None = None) -> None:
        self.procs: list[FakeFfmpeg] = []
        self.kw = dict(frames=frames, interval=interval, stderr=stderr, exit_code=exit_code)

    def __call__(self, argv: list[str], **_kwargs: Any) -> FakeFfmpeg:
        proc = FakeFfmpeg(argv, **self.kw)  # type: ignore[arg-type]
        proc.list_path = ""
        proc.list_text = ""
        proc.list_mode = 0
        if "concat" in argv:
            # Read at spawn, as ffmpeg does: the file is gone soon after.
            path = argv[argv.index("-i") + 1]
            proc.list_path = path
            proc.list_text = open(path, encoding="utf-8").read()
            proc.list_mode = stat.S_IMODE(os.stat(path).st_mode)
        self.procs.append(proc)
        return proc


class Clock:
    def __init__(self) -> None:
        self.offset = 0.0

    def __call__(self) -> float:
        return time.monotonic() + self.offset


def _service(launcher: Launcher, clock: Any = time.monotonic, streams: Any = None,
             ffmpeg: str = "/fake/ffmpeg", version: str = "8.1", urlopen: Any = None) -> video.VideoService:
    extra = {"urlopen": urlopen} if urlopen is not None else {}
    return video.VideoService(
        {"streams": streams or [
            {"id": "cam", "name": "Nose", "url": "rtsp://admin:s3cret@10.0.0.2/main"}]},
        popen=launcher, clock=clock,
        resolver=lambda _configured: (ffmpeg, "path") if ffmpeg else ("", ""),
        version=lambda _path: version,
        **extra,
    )


def _video_threads() -> list[str]:
    return [t.name for t in threading.enumerate() if t.name.startswith("corvus-video")]


def _wait_until(cond: Any, timeout: float = 3.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if cond():
            return True
        time.sleep(0.02)
    return cond()


def test_the_first_frame_request_starts_one_decoder_and_frames_follow_in_order() -> None:
    launcher = Launcher()
    svc = _service(launcher)
    try:
        first = svc.frame("cam", 0, timeout=3)
        assert first.get("jpeg", b"").startswith(b"\xff\xd8")
        second = svc.frame("cam", first["seq"], timeout=3)
        assert second["seq"] > first["seq"], "a window never gets the same frame twice"
        assert len(launcher.procs) == 1, "one camera, one ffmpeg, however often it is asked"
        proc = launcher.procs[0]
        assert not any("s3cret" in a for a in proc.argv), "never on the command line"
        assert "file 'rtsp://admin:s3cret@10.0.0.2/main'" in proc.list_text, (
            "the list file is the one place the password goes"
        )
        assert _wait_until(lambda: not os.path.exists(proc.list_path)), (
            "the list is removed once ffmpeg has opened it"
        )
        status = svc.status()["streams"][0]
        assert status["state"] == video.STATE_LIVE
        assert (status["width"], status["height"]) == (1280, 720)
        assert "password" not in status and "s3cret" not in repr(svc.status())
    finally:
        svc.shutdown()


def test_shutdown_terminates_ffmpeg_and_joins_every_thread() -> None:
    launcher = Launcher()
    svc = _service(launcher)
    assert "jpeg" in svc.frame("cam", 0, timeout=3)
    svc.shutdown()
    proc = launcher.procs[0]
    assert proc.terminated >= 1 and proc.poll() is not None
    assert _wait_until(lambda: not _video_threads()), _video_threads()
    svc.shutdown()   # idempotent


def test_a_camera_nobody_watches_is_stopped() -> None:
    launcher = Launcher()
    clock = Clock()
    svc = _service(launcher, clock=clock)
    try:
        assert "jpeg" in svc.frame("cam", 0, timeout=3)
        clock.offset += video.IDLE_STOP_S + 1
        assert _wait_until(lambda: launcher.procs[0].poll() is not None), (
            "a closed window stops asking, and that alone has to end the decoder"
        )
        assert _wait_until(lambda: svc.status()["streams"][0]["state"] == video.STATE_IDLE)
        assert _wait_until(lambda: not _video_threads())
        # The picture from before the stop is not what the camera shows now.
        result = svc.frame("cam", 0, timeout=0.05)
        assert "jpeg" not in result or len(launcher.procs) == 2
    finally:
        svc.shutdown()


def test_an_ffmpeg_that_fails_reports_why_without_the_password() -> None:
    launcher = Launcher(frames=0, stderr=b"rtsp://admin:s3cret@10.0.0.2/main: Server returned 401 Unauthorized\n",
                        exit_code=1)
    svc = _service(launcher)
    try:
        result = svc.frame("cam", 0, timeout=1.0)
        assert "jpeg" not in result
        assert _wait_until(lambda: svc.status()["streams"][0]["state"] == video.STATE_ERROR)
        message = svc.status()["streams"][0]["message"]
        assert "401 Unauthorized" in message
        assert "s3cret" not in message
    finally:
        svc.shutdown()


def test_a_camera_that_goes_silent_is_restarted_by_the_watchdog() -> None:
    launcher = Launcher(frames=0)
    clock = Clock()
    svc = _service(launcher, clock=clock)
    try:
        svc.frame("cam", 0, timeout=0.1)
        assert _wait_until(lambda: launcher.procs)
        clock.offset += video.WATCHDOG_S + 1
        svc.frame("cam", 0, timeout=0.1)   # still watched
        assert _wait_until(lambda: launcher.procs[0].terminated >= 1), (
            "an ffmpeg waiting on a camera that rebooted would otherwise wait forever"
        )
        assert _wait_until(lambda: "No picture" in svc.status()["streams"][0]["message"]
                           or len(launcher.procs) > 1)
    finally:
        svc.shutdown()


def test_without_ffmpeg_video_says_so_and_starts_nothing() -> None:
    launcher = Launcher()
    svc = _service(launcher, ffmpeg="")
    try:
        result = svc.frame("cam", 0, timeout=0.1)
        assert result["state"] == video.STATE_UNAVAILABLE
        assert "ffmpeg" in result["message"]
        status = svc.status()
        assert status["available"] is False and status["reason"]
        assert launcher.procs == []
    finally:
        svc.shutdown()


def test_an_unknown_camera_is_a_key_error() -> None:
    svc = _service(Launcher())
    with pytest.raises(KeyError):
        svc.frame("nope", 0, timeout=0.01)
    svc.shutdown()


def test_renaming_a_camera_keeps_its_decoder_and_readdressing_it_replaces_it() -> None:
    launcher = Launcher()
    svc = _service(launcher)
    try:
        assert "jpeg" in svc.frame("cam", 0, timeout=3)
        base = svc.settings()["streams"][0]
        svc.apply_settings({"streams": [dict(base, name="Renamed")]})
        assert launcher.procs[0].poll() is None, "a new name must not blank the window"
        assert svc.status()["streams"][0]["name"] == "Renamed"

        svc.apply_settings({"streams": [dict(base, url="rtsp://10.0.0.9/other")]})
        assert _wait_until(lambda: launcher.procs[0].poll() is not None), "the old address is closed"
        after = svc.frame("cam", 0, timeout=3)
        assert "jpeg" in after and len(launcher.procs) == 2
        assert "file 'rtsp://admin:s3cret@10.0.0.9/other'" in launcher.procs[1].list_text
    finally:
        svc.shutdown()
    assert _wait_until(lambda: not _video_threads()), "retired decoders are joined at shutdown too"


def test_removing_a_camera_stops_its_decoder() -> None:
    launcher = Launcher()
    svc = _service(launcher)
    try:
        assert "jpeg" in svc.frame("cam", 0, timeout=3)
        svc.apply_settings({"streams": []})
        assert _wait_until(lambda: launcher.procs[0].poll() is not None)
        assert svc.status()["streams"] == []
    finally:
        svc.shutdown()


def test_shutdown_leaves_no_list_file_or_folder_behind() -> None:
    launcher = Launcher()
    svc = _service(launcher)
    assert "jpeg" in svc.frame("cam", 0, timeout=3)
    folder = os.path.dirname(launcher.procs[0].list_path)
    svc.shutdown()
    assert not os.path.exists(folder)


@posix_permissions
def test_the_list_file_and_its_folder_are_this_accounts_alone() -> None:
    """The list file carries the camera password. On Windows the folder is
    private through its access list (``mkdtemp``, Python 3.12.4 and later),
    which ``st_mode`` does not show."""
    launcher = Launcher()
    svc = _service(launcher)
    try:
        assert "jpeg" in svc.frame("cam", 0, timeout=3)
        proc = launcher.procs[0]
        assert proc.list_mode == 0o600, "only this account can read the password"
        assert stat.S_IMODE(os.stat(os.path.dirname(proc.list_path)).st_mode) == 0o700
    finally:
        svc.shutdown()


def test_an_old_ffmpeg_still_works_and_the_status_says_the_password_shows() -> None:
    launcher = Launcher()
    svc = _service(launcher, version="4.4.2")
    try:
        assert "jpeg" in svc.frame("cam", 0, timeout=3)
        argv = launcher.procs[0].argv
        assert argv[argv.index("-i") + 1] == "rtsp://admin:s3cret@10.0.0.2/main"
        assert "-stimeout" in argv
        assert svc.status()["ffmpeg"]["hides_password"] is False
    finally:
        svc.shutdown()


def test_an_endless_error_line_is_cut_not_buffered() -> None:
    launcher = Launcher(frames=0, stderr=b"x" * 200_000, exit_code=1)
    svc = _service(launcher)
    try:
        svc.frame("cam", 0, timeout=0.5)
        assert _wait_until(lambda: svc.status()["streams"][0]["state"] == video.STATE_ERROR)
        assert len(svc.status()["streams"][0]["message"]) <= video.MAX_ERROR_LINE + 100
    finally:
        svc.shutdown()


def test_the_specific_error_is_shown_rather_than_ffmpegs_generic_last_line() -> None:
    stderr = (b"[tcp @ 0x1] Connection to tcp://10.0.0.2:554 failed: Connection refused\n"
              b"Error opening input files: Connection refused\n")
    launcher = Launcher(frames=0, stderr=stderr, exit_code=1)
    svc = _service(launcher)
    try:
        svc.frame("cam", 0, timeout=0.5)
        assert _wait_until(lambda: svc.status()["streams"][0]["state"] == video.STATE_ERROR)
        message = svc.status()["streams"][0]["message"]
        assert message.startswith("Nothing answered at that address and port.")
        assert "tcp://10.0.0.2:554" in message
    finally:
        svc.shutdown()


# ---------------------------------------------------------------------------
# WebRTC (WHEP) signalling
# ---------------------------------------------------------------------------

OFFER = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\n"
ANSWER = b"v=0\r\no=- 2 2 IN IP4 10.0.0.2\r\ns=-\r\n"


class FakeResponse:
    def __init__(self, status: int = 201, body: bytes = ANSWER, location: str = "") -> None:
        self.status = status
        self.body = body
        self.headers = {"Location": location} if location else {}

    def read(self, n: int = -1) -> bytes:
        return self.body if n < 0 else self.body[:n]

    def close(self) -> None:
        pass

    def __enter__(self) -> FakeResponse:
        return self

    def __exit__(self, *exc: Any) -> None:
        pass


class FakeWhep:
    """Stands in for urlopen: answers offers, records every request."""

    def __init__(self, *answers: Any) -> None:
        self.answers = list(answers)
        self.requests: list[dict[str, Any]] = []

    def __call__(self, request: Any, timeout: float) -> Any:
        self.requests.append({
            "method": request.get_method(), "url": request.full_url,
            "auth": request.get_header("Authorization"),
            "type": request.get_header("Content-type"), "data": request.data,
        })
        if request.get_method() == "DELETE":
            return FakeResponse(200, b"")
        answer = self.answers.pop(0) if self.answers else FakeResponse(location="/cam/whep/s1")
        if isinstance(answer, Exception):
            raise answer
        return answer


WEBRTC = [{"id": "rtc", "name": "Gimbal", "kind": "webrtc",
           "url": "http://pilot:hunter2@10.0.0.2:8889/cam/whep"}]


def test_a_webrtc_camera_needs_a_whep_address_and_keeps_a_token_without_a_user() -> None:
    assert video.url_problem("rtsp://h/x", video.KIND_WEBRTC)
    assert video.url_problem("http://10.0.0.2:8889/cam/whep", video.KIND_WEBRTC) == ""
    token = video.coerce_stream({"kind": "webrtc", "url": "http://h/cam/whep", "password": "jwt"})
    assert token["password"] == "jwt", "a bearer token has no user"
    rtsp = video.coerce_stream({"url": "rtsp://h/x", "password": "orphan"})
    assert rtsp["kind"] == "rtsp" and rtsp["password"] == ""


def test_the_offer_goes_to_the_camera_with_its_credentials_and_the_window_gets_a_token() -> None:
    whep = FakeWhep()
    svc = _service(Launcher(), streams=WEBRTC, urlopen=whep)
    try:
        res = svc.whep_offer("rtc", OFFER)
        assert res["ok"] is True and res["sdp"] == ANSWER.decode()
        req = whep.requests[0]
        assert req["method"] == "POST" and req["url"] == "http://10.0.0.2:8889/cam/whep"
        assert req["type"] == "application/sdp" and req["data"] == OFFER.encode()
        assert req["auth"].startswith("Basic "), "the credentials travel in a header, not the URL"
        assert "hunter2" not in repr(res), "the window gets a token, never a credential"
        assert "/cam/whep/s1" not in repr(res), "nor the session URL it would need to send one"

        assert svc.whep_close(res["session"]) is True
        delete = whep.requests[-1]
        assert delete["method"] == "DELETE"
        assert delete["url"] == "http://10.0.0.2:8889/cam/whep/s1", "a relative Location is resolved"
        assert delete["auth"] == req["auth"]
        assert svc.whep_close(res["session"]) is False, "a session closes once"
    finally:
        svc.shutdown()


def test_a_token_alone_is_sent_as_bearer() -> None:
    whep = FakeWhep()
    svc = _service(Launcher(), urlopen=whep, streams=[
        {"id": "rtc", "kind": "webrtc", "url": "https://cam.local/x/whep", "password": "jwt-token"}])
    try:
        assert svc.whep_offer("rtc", OFFER)["ok"]
        assert whep.requests[0]["auth"] == "Bearer jwt-token"
    finally:
        svc.shutdown()


def test_credentials_are_never_sent_to_another_origin() -> None:
    whep = FakeWhep(FakeResponse(location="http://evil.example/steal"))
    svc = _service(Launcher(), streams=WEBRTC, urlopen=whep)
    try:
        res = svc.whep_offer("rtc", OFFER)
        assert res["ok"]
        svc.whep_close(res["session"])
        assert [r["method"] for r in whep.requests] == ["POST"], (
            "a session URL on another host is not DELETEd with this camera's password"
        )
    finally:
        svc.shutdown()


def test_a_refused_offer_says_why_without_the_password() -> None:
    refused = urllib.error.HTTPError("http://10.0.0.2:8889/cam/whep", 401, "Unauthorized", {},
                                     io.BytesIO(b'{"error":"authentication failed: hunter2"}'))
    svc = _service(Launcher(), streams=WEBRTC, urlopen=FakeWhep(refused))
    try:
        res = svc.whep_offer("rtc", OFFER)
        assert res["ok"] is False and res["status"] == 502
        assert res["error"].startswith("The camera server refused the user or password.")
        assert "hunter2" not in res["error"]
    finally:
        svc.shutdown()


def test_a_redirect_is_refused_rather_than_followed() -> None:
    moved = urllib.error.HTTPError("http://10.0.0.2:8889/cam/whep", 307, "Moved", {}, io.BytesIO(b""))
    svc = _service(Launcher(), streams=WEBRTC, urlopen=FakeWhep(moved))
    try:
        assert "redirected" in svc.whep_offer("rtc", OFFER)["error"]
    finally:
        svc.shutdown()


def test_an_answer_that_is_not_sdp_is_refused() -> None:
    svc = _service(Launcher(), streams=WEBRTC, urlopen=FakeWhep(FakeResponse(body=b"<html>login</html>")))
    try:
        assert svc.whep_offer("rtc", OFFER)["ok"] is False
        assert svc.whep_offer("rtc", "not sdp")["status"] == 400
    finally:
        svc.shutdown()


def test_the_kinds_do_not_cross() -> None:
    svc = _service(Launcher(), streams=WEBRTC + [{"id": "cam", "url": "rtsp://10.0.0.3/x"}],
                   urlopen=FakeWhep())
    try:
        with pytest.raises(video.WrongKind):
            svc.frame("rtc", 0, timeout=0.01)
        with pytest.raises(video.WrongKind):
            svc.whep_offer("cam", OFFER)
        with pytest.raises(KeyError):
            svc.whep_offer("nope", OFFER)
    finally:
        svc.shutdown()


def test_sessions_are_bounded_and_closed_with_their_camera_and_at_shutdown() -> None:
    whep = FakeWhep()
    svc = _service(Launcher(), streams=WEBRTC, urlopen=whep)
    for _ in range(video.MAX_WHEP_SESSIONS + 2):
        assert svc.whep_offer("rtc", OFFER)["ok"]
    deletes = [r for r in whep.requests if r["method"] == "DELETE"]
    assert len(deletes) == 2, "the oldest sessions are closed, not kept forever"

    svc.apply_settings({"streams": [dict(svc.settings()["streams"][0], url="http://10.0.0.9:8889/o/whep")]})
    deletes = [r for r in whep.requests if r["method"] == "DELETE"]
    assert len(deletes) == 2 + video.MAX_WHEP_SESSIONS, "a re-addressed camera ends its sessions"

    assert svc.whep_offer("rtc", OFFER)["ok"]
    svc.shutdown()
    assert whep.requests[-1]["method"] == "DELETE", "and shutdown ends what is left"
    assert svc.whep_offer("rtc", OFFER)["ok"] is False


# ---------------------------------------------------------------------------
# WHEP against a real HTTP server on loopback: the real opener, real sockets
# ---------------------------------------------------------------------------

def _whep_server(handler_body: Any):
    import http.server

    seen: list[dict[str, Any]] = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args: Any) -> None:
            pass

        def _record(self) -> None:
            length = int(self.headers.get("Content-Length") or 0)
            seen.append({"method": self.command, "path": self.path,
                         "auth": self.headers.get("Authorization"),
                         "type": self.headers.get("Content-Type"),
                         "body": self.rfile.read(length) if length else b""})

        def do_POST(self) -> None:  # noqa: N802 - http.server's name
            self._record()
            handler_body(self)

        def do_DELETE(self) -> None:  # noqa: N802 - http.server's name
            self._record()
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, seen


def test_whep_over_real_http_answers_resolves_the_session_and_deletes_it() -> None:
    def answer(h: Any) -> None:
        h.send_response(201)
        h.send_header("Content-Type", "application/sdp")
        h.send_header("Location", "/cam/whep/session-7")
        h.send_header("Content-Length", str(len(ANSWER)))
        h.end_headers()
        h.wfile.write(ANSWER)

    server, seen = _whep_server(answer)
    port = server.server_address[1]
    svc = video.VideoService({"streams": [{"id": "rtc", "kind": "webrtc",
                                           "url": f"http://pilot:hunter2@127.0.0.1:{port}/cam/whep"}]})
    try:
        res = svc.whep_offer("rtc", OFFER)
        assert res["ok"] and res["sdp"] == ANSWER.decode()
        assert seen[0]["path"] == "/cam/whep" and seen[0]["type"] == "application/sdp"
        assert seen[0]["auth"].startswith("Basic ") and seen[0]["body"] == OFFER.encode()
        assert svc.whep_close(res["session"])
        assert seen[-1]["method"] == "DELETE" and seen[-1]["path"] == "/cam/whep/session-7"
        assert seen[-1]["auth"] == seen[0]["auth"]
    finally:
        svc.shutdown()
        server.shutdown()
        server.server_close()


def test_whep_over_real_http_does_not_follow_a_redirect_with_the_password() -> None:
    def redirect(h: Any) -> None:
        h.send_response(307)
        h.send_header("Location", "http://127.0.0.1:9/elsewhere")
        h.send_header("Content-Length", "0")
        h.end_headers()

    server, seen = _whep_server(redirect)
    port = server.server_address[1]
    svc = video.VideoService({"streams": [{"id": "rtc", "kind": "webrtc",
                                           "url": f"http://127.0.0.1:{port}/cam/whep", "password": "tok"}]})
    try:
        res = svc.whep_offer("rtc", OFFER)
        assert res["ok"] is False and "redirected" in res["error"]
        assert len(seen) == 1, "the request went to the configured server and nowhere else"
    finally:
        svc.shutdown()
        server.shutdown()
        server.server_close()


def test_a_whep_server_that_is_not_there_is_reported_not_raised() -> None:
    svc = video.VideoService({"streams": [{"id": "rtc", "kind": "webrtc",
                                           "url": "http://127.0.0.1:9/cam/whep"}]})
    try:
        res = svc.whep_offer("rtc", OFFER)
        assert res["ok"] is False and res["status"] == 502
        assert "did not answer" in res["error"] or "Nothing answered" in res["error"]
    finally:
        svc.shutdown()


def test_a_list_file_ffmpeg_holds_open_is_removed_once_it_exits(monkeypatch) -> None:
    """Windows: a file another process has open cannot be deleted.

    ffmpeg keeps its input list open for as long as it runs, so the removal
    after the first frame fails there. The password must still not outlive
    the run: the file goes as soon as ffmpeg has exited.
    """
    launcher = Launcher()
    svc = _service(launcher)
    real_unlink = os.unlink
    refused: list[str] = []

    def windows_unlink(path: str) -> None:
        running = [p for p in launcher.procs if p.list_path == path and p.poll() is None]
        if running:
            refused.append(path)
            raise PermissionError(32, "The process cannot access the file", path)
        real_unlink(path)

    monkeypatch.setattr(video.os, "unlink", windows_unlink)
    try:
        assert "jpeg" in svc.frame("cam", 0, timeout=3)
        proc = launcher.procs[0]
        assert _wait_until(lambda: len(refused) >= 2), "tried again while ffmpeg ran"
        assert os.path.exists(proc.list_path)
    finally:
        svc.shutdown()
    assert not os.path.exists(proc.list_path), "the password outlived the ffmpeg that needed it"


def test_ffmpeg_does_not_inherit_the_bundles_variables(monkeypatch) -> None:
    captured: dict[str, Any] = {}

    class Capturing(Launcher):
        def __call__(self, argv: list[str], **kwargs: Any) -> FakeFfmpeg:
            captured.update(kwargs)
            return super().__call__(argv, **kwargs)

    monkeypatch.setenv("QTWEBENGINE_CHROMIUM_FLAGS", "--no-sandbox")
    launcher = Capturing()
    svc = _service(launcher)
    try:
        assert "jpeg" in svc.frame("cam", 0, timeout=3)
    finally:
        svc.shutdown()
    assert isinstance(captured.get("env"), dict)
    assert "QTWEBENGINE_CHROMIUM_FLAGS" not in captured["env"]


def test_windows_install_folders_are_searched() -> None:
    env = {"LOCALAPPDATA": "C:\\Users\\p\\AppData\\Local", "USERPROFILE": "C:\\Users\\p",
           "ProgramData": "C:\\ProgramData"}
    paths = video.windows_ffmpeg_paths(env)
    assert any("WinGet" in p for p in paths)
    assert any("scoop" in p for p in paths)
    assert any("chocolatey" in p for p in paths)
    assert all(p.endswith("ffmpeg.exe") for p in paths)
    assert video.windows_ffmpeg_paths({}) == []
