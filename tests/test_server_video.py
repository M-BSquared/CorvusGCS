"""The /api/video/* endpoints: the HTTP layer over the camera decoders.

The routes own three things: turning a form into a stored camera, keeping the
camera password out of every response, and handing one frame at a time to a
window. The decoder is faked where it would start a process; the settings path
runs through the real config and the real ``corvus.video.settings``.

The password rule is the one with teeth, for the reason the SSH and NTRIP ones
are: the editor is drawn from a response that must never contain it, so a
save without a password has to keep the stored one, or every rename would
lock the operator out of their camera.
"""
from __future__ import annotations

import io
from typing import Any

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus import video  # noqa: E402
from corvus.config import CorvusConfig, load_config  # noqa: E402
from corvus.server import CorvusHandler, stop_backend  # noqa: E402


class FakeVideo:
    """Service stand-in: settings go through for real, frames are canned."""

    def __init__(self, frame_result: dict[str, Any] | None = None) -> None:
        self.applied: list[dict[str, Any]] = []
        self.frame_result = frame_result or {"seq": 7, "jpeg": b"\xff\xd8jpeg\xff\xd9"}
        self.frame_calls: list[tuple[str, int]] = []
        self.shutdowns = 0
        self._settings = video.settings(None)

    def apply_settings(self, stored: dict[str, Any]) -> None:
        self.applied.append(stored)
        self._settings = video.settings(stored)

    def status(self) -> dict[str, Any]:
        return {"available": True, "reason": "",
                "streams": [video.public_stream(s) for s in self._settings["streams"]]}

    def frame(self, stream_id: str, after: int, timeout: float = 0.0, stopping: Any = None) -> dict:
        self.frame_calls.append((stream_id, after))
        if stream_id == "rtc":
            raise video.WrongKind("This camera plays over WebRTC.")
        if stream_id != "cam":
            raise KeyError(stream_id)
        return self.frame_result

    def whep_offer(self, stream_id: str, sdp: str) -> dict:
        self.offers = getattr(self, "offers", []) + [(stream_id, sdp)]
        if stream_id == "cam":
            raise video.WrongKind("This camera is decoded by ffmpeg, not played over WebRTC.")
        if stream_id != "rtc":
            raise KeyError(stream_id)
        if sdp == "refused":
            return {"ok": False, "status": 502, "error": "The camera server refused the user or password."}
        return {"ok": True, "sdp": "v=0 answer", "session": "tok"}

    def whep_close(self, token: str) -> bool:
        self.closed = getattr(self, "closed", []) + [token]
        return token == "tok"

    def shutdown(self) -> None:
        self.shutdowns += 1


def _handler(service: Any, tmp_path: Any, config: Any = None):
    handler = object.__new__(CorvusHandler)
    handler.video = service  # type: ignore[assignment]
    handler.config = config if config is not None else CorvusConfig()
    handler.config_path = str(tmp_path / "config.json")
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def _add(handler: Any, responses: list, **body: Any) -> tuple[dict, int]:
    handler._api_video_streams_upsert(body)
    return responses[-1]


# ---------------------------------------------------------------------------
# Adding and editing cameras
# ---------------------------------------------------------------------------

def test_a_camera_is_stored_with_its_credentials_taken_out_of_the_address(tmp_path: Any) -> None:
    svc = FakeVideo()
    handler, responses = _handler(svc, tmp_path)
    payload, status = _add(handler, responses, name="Gimbal",
                           url="rtsp://admin:hunter2@192.168.144.25:8554/main.264")
    assert status == 200 and payload["ok"] is True and payload["id"]
    stored = handler.config.video["streams"][0]
    assert stored["url"] == "rtsp://192.168.144.25:8554/main.264"
    assert (stored["username"], stored["password"]) == ("admin", "hunter2")
    assert svc.applied, "the running service is told at once"
    assert "hunter2" not in repr(payload), "the answer to a save is a response like any other"
    on_disk = load_config(handler.config_path)
    assert on_disk.video["streams"][0]["password"] == "hunter2"


def test_saving_without_a_password_keeps_the_stored_one(tmp_path: Any) -> None:
    handler, responses = _handler(FakeVideo(), tmp_path)
    payload, _ = _add(handler, responses, name="Nose", url="rtsp://10.0.0.2/main",
                      username="pilot", password="hunter2")
    cam_id = payload["id"]
    _add(handler, responses, id=cam_id, name="Nose camera", url="rtsp://10.0.0.2/main",
         username="pilot")
    stored = handler.config.video["streams"][0]
    assert stored["name"] == "Nose camera"
    assert stored["password"] == "hunter2", "a rename must not lock the operator out"

    _add(handler, responses, id=cam_id, name="Nose camera", url="rtsp://10.0.0.2/main",
         username="pilot", password="")
    assert handler.config.video["streams"][0]["password"] == "", "a sent empty one clears it"
    assert len(handler.config.video["streams"]) == 1, "an edit replaces, it does not append"


@pytest.mark.parametrize("body, fragment", [
    ({"url": "file:///etc/passwd"}, "rtsp://"),
    ({"url": ""}, "address"),
    ({"url": "rtsp://10.0.0.2/x", "transport": "quic"}, "transport"),
    ({"url": "rtsp://10.0.0.2/x", "name": "x" * 200}, "too long"),
    ({"url": 5}, "strings"),
])
def test_a_camera_that_cannot_work_is_refused_before_it_is_stored(
        tmp_path: Any, body: dict, fragment: str) -> None:
    handler, responses = _handler(FakeVideo(), tmp_path)
    payload, status = _add(handler, responses, **body)
    assert status == 400 and payload["ok"] is False
    assert fragment in payload["error"]
    assert handler.config.video is None


def test_editing_a_camera_that_was_removed_meanwhile_is_a_404(tmp_path: Any) -> None:
    handler, responses = _handler(FakeVideo(), tmp_path)
    payload, status = _add(handler, responses, id="gone", url="rtsp://10.0.0.2/x")
    assert status == 404
    assert handler.config.video is None, "and it is not quietly added as a new one"


def test_the_camera_list_is_capped(tmp_path: Any) -> None:
    handler, responses = _handler(FakeVideo(), tmp_path)
    for i in range(video.MAX_STREAMS):
        assert _add(handler, responses, url=f"rtsp://10.0.0.{i}/x")[1] == 200
    payload, status = _add(handler, responses, url="rtsp://10.0.0.99/x")
    assert status == 400 and str(video.MAX_STREAMS) in payload["error"]


def test_removing_a_camera(tmp_path: Any) -> None:
    svc = FakeVideo()
    handler, responses = _handler(svc, tmp_path)
    cam_id = _add(handler, responses, url="rtsp://10.0.0.2/x")[0]["id"]
    handler._api_video_streams_remove({"id": cam_id})
    assert responses[-1][1] == 200
    assert handler.config.video["streams"] == []
    assert svc.applied[-1]["streams"] == []
    handler._api_video_streams_remove({"id": cam_id})
    assert responses[-1][1] == 200, "removing twice is not an error"


# ---------------------------------------------------------------------------
# Status and the decoder path
# ---------------------------------------------------------------------------

def test_the_status_never_carries_a_password(tmp_path: Any) -> None:
    svc = FakeVideo()
    handler, responses = _handler(svc, tmp_path)
    _add(handler, responses, url="rtsp://admin:hunter2@10.0.0.2/x")
    handler._api_video_status()
    payload, status = responses[-1]
    assert status == 200
    assert payload["streams"][0]["has_password"] is True
    assert "hunter2" not in repr(payload)


def test_without_a_service_the_status_still_renders_and_says_why(tmp_path: Any) -> None:
    cfg = CorvusConfig(video=video.settings({"streams": [{"id": "a", "url": "rtsp://u:pw@h/x"}]}))
    handler, responses = _handler(None, tmp_path, config=cfg)
    handler._api_video_status()
    payload, status = responses[-1]
    assert status == 200 and payload["available"] is False and payload["reason"]
    assert payload["streams"][0]["state"] == video.STATE_UNAVAILABLE
    assert "pw" not in repr(payload["streams"])


def test_a_decoder_path_that_is_not_a_program_is_refused(tmp_path: Any) -> None:
    handler, responses = _handler(FakeVideo(), tmp_path)
    handler._api_video_settings({"ffmpeg": str(tmp_path / "missing")})
    assert responses[-1][1] == 400
    assert handler.config.video is None
    handler._api_video_settings({"ffmpeg": ""})
    assert responses[-1][1] == 200
    assert handler.config.video["ffmpeg"] == ""


# ---------------------------------------------------------------------------
# Frames
# ---------------------------------------------------------------------------

def _frame_handler(service: Any, tmp_path: Any, query: str):
    handler, responses = _handler(service, tmp_path)
    handler.path = "/api/video/frame?" + query
    handler.wfile = io.BytesIO()
    sent: dict[str, Any] = {"headers": {}}
    handler.send_response = lambda code, *a: sent.__setitem__("status", code)  # type: ignore[method-assign]
    handler.send_header = lambda k, v: sent["headers"].__setitem__(k, v)  # type: ignore[method-assign]
    handler.end_headers = lambda: None  # type: ignore[method-assign]
    handler._send_cors = lambda: None  # type: ignore[method-assign]
    return handler, responses, sent


def test_a_frame_is_sent_as_a_jpeg_with_its_number(tmp_path: Any) -> None:
    svc = FakeVideo()
    handler, responses, sent = _frame_handler(svc, tmp_path, "id=cam&after=3")
    handler._api_video_frame()
    assert svc.frame_calls == [("cam", 3)]
    assert sent["status"] == 200
    assert sent["headers"]["Content-Type"] == "image/jpeg"
    assert sent["headers"]["X-Video-Seq"] == "7"
    assert sent["headers"]["Cache-Control"] == "no-store"
    assert handler.wfile.getvalue() == b"\xff\xd8jpeg\xff\xd9"
    assert responses == []


def test_no_new_frame_in_time_answers_with_the_state(tmp_path: Any) -> None:
    svc = FakeVideo({"state": "starting", "message": "Connecting to the camera.", "seq": 0})
    handler, responses, _ = _frame_handler(svc, tmp_path, "id=cam&after=nonsense")
    handler._api_video_frame()
    assert svc.frame_calls == [("cam", 0)], "a garbled frame number starts from the beginning"
    assert responses == [({"state": "starting", "message": "Connecting to the camera.", "seq": 0}, 200)]


def test_an_unknown_camera_is_a_404(tmp_path: Any) -> None:
    handler, responses, _ = _frame_handler(FakeVideo(), tmp_path, "id=other")
    handler._api_video_frame()
    assert responses[-1][1] == 404


# ---------------------------------------------------------------------------
# Lifecycle
# ---------------------------------------------------------------------------

def test_the_backend_teardown_stops_the_decoders() -> None:
    svc = FakeVideo()

    class Server:
        video = svc
        stopped = False

        def shutdown(self) -> None:
            self.stopped = True

        def server_close(self) -> None:
            pass

    server = Server()
    stop_backend(server)
    assert svc.shutdowns >= 1
    assert server.stopped


# ---------------------------------------------------------------------------
# WebRTC, and the hardening around both kinds
# ---------------------------------------------------------------------------

def test_a_webrtc_camera_is_stored_with_its_kind(tmp_path: Any) -> None:
    handler, responses = _handler(FakeVideo(), tmp_path)
    payload, status = _add(handler, responses, kind="webrtc", name="Gimbal",
                           url="http://pilot:hunter2@10.0.0.2:8889/cam/whep")
    assert status == 200
    stored = handler.config.video["streams"][0]
    assert (stored["kind"], stored["url"]) == ("webrtc", "http://10.0.0.2:8889/cam/whep")
    assert "hunter2" not in repr(payload)
    payload, status = _add(handler, responses, kind="webrtc", url="rtsp://10.0.0.2/x")
    assert status == 400 and "WHEP" in payload["error"]
    payload, status = _add(handler, responses, kind="hls", url="http://10.0.0.2/x")
    assert status == 400


def test_a_camera_moved_to_another_host_does_not_take_its_password_along(tmp_path: Any) -> None:
    handler, responses = _handler(FakeVideo(), tmp_path)
    cam_id = _add(handler, responses, url="rtsp://admin:hunter2@10.0.0.2/main")[0]["id"]
    payload, _ = _add(handler, responses, id=cam_id, url="rtsp://10.0.0.2:554/other", username="admin")
    assert handler.config.video["streams"][0]["password"] == "hunter2", "same camera, new path: kept"
    assert "warning" not in payload
    payload, _ = _add(handler, responses, id=cam_id, url="rtsp://10.9.9.9/main", username="admin")
    assert handler.config.video["streams"][0]["password"] == "", (
        "one camera's password must not be sent to a different machine"
    )
    assert "password was not kept" in payload["warning"]


def test_the_offer_is_passed_through_and_the_window_gets_only_a_token(tmp_path: Any) -> None:
    svc = FakeVideo()
    handler, responses = _handler(svc, tmp_path)
    handler._api_video_webrtc_offer({"id": "rtc", "sdp": "v=0 offer"})
    assert responses[-1] == ({"ok": True, "sdp": "v=0 answer", "session": "tok"}, 200)
    handler._api_video_webrtc_offer({"id": "rtc", "sdp": "refused"})
    payload, status = responses[-1]
    assert status == 502 and payload["ok"] is False and "status" not in payload
    handler._api_video_webrtc_offer({"id": "cam", "sdp": "v=0"})
    assert responses[-1][1] == 409
    handler._api_video_webrtc_offer({"id": "nope", "sdp": "v=0"})
    assert responses[-1][1] == 404
    handler._api_video_webrtc_offer({"id": "rtc"})
    assert responses[-1][1] == 400
    handler._api_video_webrtc_close({"session": "tok"})
    assert responses[-1] == ({"ok": True, "closed": True}, 200)
    assert svc.closed == ["tok"]


def test_a_webrtc_camera_has_no_frames_here(tmp_path: Any) -> None:
    handler, responses, _ = _frame_handler(FakeVideo(), tmp_path, "id=rtc")
    handler._api_video_frame()
    assert responses[-1][1] == 409


def test_a_page_on_another_site_cannot_start_a_decoder(tmp_path: Any) -> None:
    svc = FakeVideo()
    handler, responses, _ = _frame_handler(svc, tmp_path, "id=cam")
    handler.headers = {"Sec-Fetch-Site": "cross-site"}
    handler._api_video_frame()
    assert responses[-1][1] == 403
    assert svc.frame_calls == [], "refused before the decoder is asked for anything"
