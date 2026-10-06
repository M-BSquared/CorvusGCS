"""Video streams.

Route handlers moved out of corvus/server.py. CorvusHandler inherits
VideoRoutes, and the @route registry wires them exactly as before.
"""
from __future__ import annotations

import logging
from typing import Any
from urllib.parse import parse_qs, urlparse
from .. import video
from ..http_routes import CLIENT_GONE_ERRORS, _config_write_lock, route

logger = logging.getLogger("corvus.server")


class VideoRoutes:
    # ---- Camera video ----
    def _video_status(self) -> dict[str, Any]:
        """The Video page's whole state, or an unavailable one without a service."""
        if self.video is None:
            return {
                "available": False,
                "reason": "Video is unavailable in this build.",
                "ffmpeg": {"path": "", "source": "", "version": "", "configured": ""},
                "streams": [
                    dict(video.public_stream(s), state=video.STATE_UNAVAILABLE, message="")
                    for s in video.settings(getattr(self._live_config(), "video", None))["streams"]
                ],
                "max_streams": video.MAX_STREAMS,
                "schemes": list(video.SCHEMES),
                "transports": list(video.TRANSPORTS),
            }
        return self.video.status()

    @route("GET", "/api/video/status")
    def _api_video_status(self) -> None:
        """Every camera with its state, and whether there is an ffmpeg to decode with.

        Always 200 so the page can poll it. Never carries a password.
        """
        try:
            self._send_json(self._video_status())
        except Exception as exc:  # noqa: BLE001 - a status read must never 500
            logger.exception("video status failed")
            self._send_json({"ok": False, "error": f"video status failed: {exc}"}, 500)

    @route("GET", "/api/video/frame")
    def _api_video_frame(self) -> None:
        """The next frame of one camera, as ``image/jpeg``, or its state as JSON.

        ``?id=`` names the camera and ``?after=`` the last frame number the
        window has. The request waits up to ``video.FRAME_WAIT_S`` for a newer
        frame, so a window asking in a loop gets every frame as it is decoded
        while holding a connection for no longer than one frame interval (see
        :mod:`corvus.video` for why this is not one endless stream). With no
        new frame in time the answer is ``{"state", "message", "seq"}``.

        Asking is also what keeps the camera's ffmpeg running: it is started by
        the first request and stopped a few seconds after the last one. That
        makes this the one GET with a side effect, so it gets the check the
        POSTs do: a page on another site cannot start a decoder from an <img>.
        """
        headers = getattr(self, "headers", None)
        if headers is not None and str(headers.get("Sec-Fetch-Site", "")).lower() == "cross-site":
            self._send_json({"error": "cross-site request forbidden"}, 403)
            return
        params = parse_qs(urlparse(self.path).query)
        stream_id = (params.get("id") or [""])[0]
        try:
            after = int((params.get("after") or ["0"])[0])
        except ValueError:
            after = 0
        if self.video is None:
            self._send_json({"state": video.STATE_UNAVAILABLE, "seq": 0,
                             "message": "Video is unavailable in this build."}, 503)
            return
        try:
            result = self.video.frame(stream_id, after, stopping=self._sse_stopping)
        except KeyError:
            self._send_json({"error": "no such camera"}, 404)
            return
        except video.WrongKind as exc:
            self._send_json({"error": str(exc)}, 409)
            return
        jpeg = result.get("jpeg")
        if not jpeg:
            self._send_json(result)
            return
        self.send_response(200)
        self.send_header("Content-Type", "image/jpeg")
        self.send_header("Content-Length", str(len(jpeg)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Video-Seq", str(result["seq"]))
        self._send_cors()
        self.end_headers()
        try:
            self.wfile.write(jpeg)
        except CLIENT_GONE_ERRORS:
            pass

    def _save_video(self, streams: list[dict[str, Any]], ffmpeg: str) -> dict[str, Any]:
        """Store the camera list and persist it; return what was stored.

        Called with ``_config_write_lock`` held, so two saves cannot interleave
        their read-modify-write of the list. The caller hands the result to
        the service once the lock is released.
        """
        cfg = self._live_config()
        cfg.video = video.settings({"streams": streams, "ffmpeg": ffmpeg})
        self._save_live_config()
        return cfg.video

    @route("POST", "/api/video/streams")
    def _api_video_streams_upsert(self, payload: dict) -> None:
        """Add a camera, or replace the one named by ``id``. Does not open it.

        ``kind`` is ``"rtsp"`` (decoded here by ffmpeg, the default) or
        ``"webrtc"`` (a WHEP address the window plays itself).

        ``password`` follows the rule the SSH connections use, because the
        status never sends it to the browser: an omitted key keeps the stored
        one, a sent one (including ``""``) replaces it. With one exception: a
        camera moved to another host does not take its password along, because
        that would send one camera's password to a different machine. The
        answer then carries a ``warning`` saying so. Credentials typed into the
        address itself (``rtsp://user:pass@host/…``) are taken out of it and
        stored apart, so the address can be shown and the password cannot.
        """
        name = payload.get("name", "")
        url = payload.get("url", "")
        username = payload.get("username", "")
        if not isinstance(name, str) or not isinstance(url, str) or not isinstance(username, str):
            self._send_json({"ok": False, "error": "name, url and username must be strings"}, 400)
            return
        if len(name.strip()) > video.MAX_NAME_LEN:
            self._send_json({"ok": False, "error": "The name is too long."}, 400)
            return
        kind = payload.get("kind", video.KIND_RTSP)
        if kind not in video.KINDS:
            self._send_json({"ok": False, "error": "kind must be rtsp or webrtc"}, 400)
            return
        clean_url, url_user, url_password = video.split_credentials(url)
        problem = video.url_problem(clean_url, kind)
        if problem:
            self._send_json({"ok": False, "error": problem}, 400)
            return
        transport = payload.get("transport", video.DEFAULT_TRANSPORT)
        if transport not in video.TRANSPORTS:
            self._send_json({"ok": False, "error": "transport must be tcp or udp"}, 400)
            return
        stream_id = payload.get("id")
        if not isinstance(stream_id, str):
            stream_id = ""

        with _config_write_lock:
            current = video.settings(getattr(self._live_config(), "video", None))
            streams = current["streams"]
            existing = next((s for s in streams if s["id"] == stream_id), None) if stream_id else None
            if stream_id and existing is None:
                self._send_json({"ok": False, "error": "That camera is no longer saved."}, 404)
                return
            if existing is None and len(streams) >= video.MAX_STREAMS:
                self._send_json({"ok": False,
                                 "error": f"At most {video.MAX_STREAMS} cameras can be saved."}, 400)
                return
            warning = ""
            if "password" in payload:
                password = payload["password"] if isinstance(payload["password"], str) else ""
            else:
                password = existing.get("password", "") if existing else ""
                if password and not video.same_origin(existing["url"], clean_url):
                    password = ""
                    warning = ("The camera moved to another host, so its password was not "
                               "kept. Enter it again if the new one needs it.")
            if url_user:
                username, password = url_user, url_password or password
            entry = video.coerce_stream({
                "id": existing["id"] if existing else video.new_stream_id(),
                "name": name,
                "url": clean_url,
                "username": username.strip(),
                "password": password,
                "transport": transport,
                "kind": kind,
            })
            if entry is None:
                self._send_json({"ok": False, "error": "That camera cannot be saved."}, 400)
                return
            if existing is not None:
                streams = [entry if s["id"] == entry["id"] else s for s in streams]
            else:
                streams.append(entry)
            stored = self._save_video(streams, current["ffmpeg"])
        self._apply_video(stored)
        answer = {"ok": True, "id": entry["id"], "status": self._video_status()}
        if warning:
            answer["warning"] = warning
        self._send_json(answer)

    @route("POST", "/api/video/streams/remove")
    def _api_video_streams_remove(self, payload: dict) -> None:
        """Remove a camera by ``id``; its decoder is stopped. Idempotent."""
        stream_id = payload.get("id")
        if not isinstance(stream_id, str) or not stream_id:
            self._send_json({"ok": False, "error": "id must be a non-empty string"}, 400)
            return
        with _config_write_lock:
            current = video.settings(getattr(self._live_config(), "video", None))
            streams = [s for s in current["streams"] if s["id"] != stream_id]
            stored = self._save_video(streams, current["ffmpeg"])
        self._apply_video(stored)
        self._send_json({"ok": True, "status": self._video_status()})

    @route("POST", "/api/video/webrtc/offer")
    def _api_video_webrtc_offer(self, payload: dict) -> None:
        """Pass a camera window's WebRTC offer to the camera's WHEP server.

        ``{id, sdp}`` in; ``{ok, sdp, session}`` out, where ``sdp`` is the
        server's answer and ``session`` an opaque token for
        ``/api/video/webrtc/close``. The request is made from here rather than
        from the window so the camera's password never reaches the browser,
        and the answer never names the session URL it would be sent to.
        """
        stream_id = payload.get("id")
        sdp = payload.get("sdp")
        if not isinstance(stream_id, str) or not stream_id or not isinstance(sdp, str):
            self._send_json({"ok": False, "error": "id and sdp must be strings"}, 400)
            return
        if self.video is None:
            self._send_json({"ok": False, "error": "Video is unavailable in this build."}, 503)
            return
        try:
            result = self.video.whep_offer(stream_id, sdp)
        except KeyError:
            self._send_json({"ok": False, "error": "no such camera"}, 404)
            return
        except video.WrongKind as exc:
            self._send_json({"ok": False, "error": str(exc)}, 409)
            return
        status = result.pop("status", 200) if not result.get("ok") else 200
        self._send_json(result, status)

    @route("POST", "/api/video/webrtc/close")
    def _api_video_webrtc_close(self, payload: dict) -> None:
        """End a WebRTC session a camera window opened. Idempotent."""
        token = payload.get("session")
        if not isinstance(token, str) or not token:
            self._send_json({"ok": False, "error": "session must be a non-empty string"}, 400)
            return
        closed = self.video.whep_close(token) if self.video is not None else False
        self._send_json({"ok": True, "closed": closed})

    @route("POST", "/api/video/settings")
    def _api_video_settings(self, payload: dict) -> None:
        """Set the ffmpeg to decode with. ``""`` means find one by itself.

        A path that is not an executable file is refused rather than stored,
        so the page never has to explain a setting that was saved broken.
        """
        path = payload.get("ffmpeg", "")
        if not isinstance(path, str):
            self._send_json({"ok": False, "error": "ffmpeg must be a string"}, 400)
            return
        path = path.strip()
        if path:
            resolved, _ = video.resolve_ffmpeg(path)
            if not resolved:
                self._send_json({"ok": False,
                                 "error": "There is no program at that path that can be run."}, 400)
                return
        with _config_write_lock:
            current = video.settings(getattr(self._live_config(), "video", None))
            stored = self._save_video(current["streams"], path)
        self._apply_video(stored)
        self._send_json({"ok": True, "status": self._video_status()})
