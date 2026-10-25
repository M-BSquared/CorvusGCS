"""The MCP server's HTTP side: ``/mcp`` for the AI client, ``/api/mcp/*`` for the UI.

The protocol and every decision about what an agent may do live in
:mod:`corvus.mcp`. This module only moves bytes: it checks that a request may
reach the server at all (this computer, switched on, the right token), reads
the body, and writes the reply. CorvusHandler inherits McpRoutes, and the
@route registry wires the ``/api/mcp`` ones.
"""
from __future__ import annotations

import http.client
import json
import logging
import queue
from typing import Any
from urllib.parse import urlparse

from .. import mcp
from ..http_routes import CLIENT_GONE_ERRORS, route

logger = logging.getLogger("corvus.server")

# One JSON-RPC message is small; a mission plan is the largest thing a tool takes.
MAX_MCP_BODY_BYTES = 1024 * 1024
_LOOPBACK = ("127.0.0.1", "::1", "::ffff:127.0.0.1")


def internal_call(handler_cls: Any, server: Any, method: str, path: str,
                  payload: dict[str, Any] | None = None) -> tuple[int, dict[str, Any]]:
    """Run one Corvus endpoint in-process and return ``(status, body)``.

    This is how an MCP tool reaches the vehicle: through the very handler a
    button in the UI calls, with its validation and its refusals, rather than
    through a second path that would have to be kept in step with it. A
    handler built with ``object.__new__`` is what the unit tests use too; only
    ``_send_json`` is replaced, to catch the reply instead of writing it.
    """
    from ..server import _sanitize

    parsed = urlparse(path)
    table = handler_cls._GET_ROUTES if method == "GET" else handler_cls._POST_ROUTES
    name = table.get(parsed.path)
    if name is None:
        return 404, {"ok": False, "error": f"no endpoint {parsed.path}"}
    sub = object.__new__(handler_cls)
    sub.server = server
    sub.client_address = ("127.0.0.1", 0)
    sub.path = path
    sub.headers = http.client.HTTPMessage()
    reply: dict[str, Any] = {}

    def capture(data: dict, status: int = 200) -> None:
        reply["status"] = status
        reply["body"] = data

    sub._send_json = capture
    if method == "GET":
        getattr(sub, name)()
    elif not sub._fleet_refuses(parsed.path):
        getattr(sub, name)(dict(payload or {}))
    if "body" not in reply:
        return 500, {"ok": False, "error": "the endpoint gave no answer"}
    body = json.loads(json.dumps(_sanitize(reply["body"]), default=str))
    return int(reply["status"]), body if isinstance(body, dict) else {"value": body}


class McpRoutes:
    mcp: Any = None

    # ---- shared checks -------------------------------------------------

    def _mcp_from_here(self) -> bool:
        client = getattr(self, "client_address", None)
        host = str(client[0]) if client else ""
        return host in _LOOPBACK or host.startswith("127.")

    def _mcp_reply(self, status: int, payload: Any, headers: dict[str, str] | None = None) -> None:
        body = b"" if payload is None else json.dumps(payload).encode("utf-8")
        self.send_response(status)
        if payload is not None:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        if body:
            self.wfile.write(body)

    def _mcp_admitted(self) -> bool:
        """This computer, switched on, and the token: or a reply saying which is missing."""
        svc = self.mcp
        if svc is None:
            self._mcp_reply(503, mcp._jsonrpc_error(None, -32000, "MCP is not available"))
            return False
        if not self._mcp_from_here():
            self._mcp_reply(403, mcp._jsonrpc_error(
                None, -32000, "The Corvus MCP server only answers this computer."))
            return False
        if not svc.enabled():
            self._mcp_reply(403, mcp._jsonrpc_error(
                None, -32000, f"The MCP server is switched off in Corvus GCS ({mcp.WHERE})."))
            return False
        if not svc.authorized(self.headers.get("Authorization")):
            self._mcp_reply(401, mcp._jsonrpc_error(
                None, -32001, f"Missing or wrong token. Copy it from {mcp.WHERE}."),
                {"WWW-Authenticate": 'Bearer realm="corvus-gcs"'})
            return False
        return True

    # ---- /mcp, the protocol ------------------------------------------------

    def _mcp_post(self) -> None:
        if not self._mcp_admitted():
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except (TypeError, ValueError):
            length = -1
        if length < 0 or length > MAX_MCP_BODY_BYTES:
            self._mcp_reply(413 if length > 0 else 400,
                            mcp._jsonrpc_error(None, mcp.JSONRPC_INVALID_REQUEST,
                                               "bad or oversized body"))
            return
        raw = self.rfile.read(length) if length else b""
        try:
            message = json.loads(raw)
        except (json.JSONDecodeError, ValueError, UnicodeDecodeError):
            self._mcp_reply(400, mcp._jsonrpc_error(None, mcp.JSONRPC_PARSE_ERROR, "invalid JSON"))
            return
        status, payload, headers = self.mcp.handle_post(
            message, self.headers.get("Mcp-Session-Id"))
        self._mcp_reply(status, payload, headers)

    def _mcp_get(self) -> None:
        """The server's event stream: tell a client its tool list changed."""
        if not self._mcp_admitted():
            return
        session_id = self.headers.get("Mcp-Session-Id")
        accept = self.headers.get("Accept", "") or ""
        stream = self.mcp.open_stream(session_id) if "text/event-stream" in accept else None
        if stream is None:
            self._mcp_reply(405, None, {"Allow": "POST, DELETE"})
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.end_headers()
        try:
            while True:
                try:
                    note = stream.get(timeout=15)
                except queue.Empty:
                    if self._sse_stopping():
                        break
                    self.wfile.write(b": ping\n\n")
                    self.wfile.flush()
                    continue
                if note is None:
                    break
                self.wfile.write(b"event: message\ndata: " + json.dumps(note).encode() + b"\n\n")
                self.wfile.flush()
        except CLIENT_GONE_ERRORS:
            pass
        finally:
            self.mcp.close_stream(session_id, stream)

    def _mcp_delete(self) -> None:
        if not self._mcp_admitted():
            return
        ended = self.mcp.end_session(self.headers.get("Mcp-Session-Id"))
        self._mcp_reply(200 if ended else 404, None)

    # ---- /api/mcp, the settings card and the window ---------------------

    def _mcp_service(self) -> Any:
        if self.mcp is None:
            self._send_json({"ok": False, "error": "MCP is not available"}, 503)
        return self.mcp

    def _mcp_url(self) -> str:
        server = getattr(self, "server", None)
        address = getattr(server, "server_address", None)
        port = address[1] if address else 8000
        return f"http://127.0.0.1:{port}/mcp"

    def _mcp_local_only(self) -> bool:
        if self._mcp_from_here():
            return True
        self._send_json({"ok": False, "error":
                         "The MCP server can only be set up on the computer Corvus runs on."}, 403)
        return False

    @route("GET", "/api/mcp")
    def _api_mcp(self) -> None:
        """Everything the MCP card shows: settings, catalog, plugins, status, how to connect.

        The token goes to this computer only: the URL it unlocks is loopback,
        so nobody elsewhere has a use for it.
        """
        svc = self._mcp_service()
        if svc is None:
            return
        local = self._mcp_from_here()
        self._send_json({
            "settings": svc.settings(),
            "catalog": mcp.tool_catalog(),
            "plugins": svc.plugin_summary(),
            "status": svc.status(),
            "url": self._mcp_url(),
            "token": svc.token() if local else "",
            "local": local,
            "control_max_s": mcp.CONTROL_MAX_S,
            "confirm_timeout_s": mcp.CONFIRM_TIMEOUT_S,
        })

    @route("POST", "/api/mcp/settings")
    def _api_mcp_settings(self, payload: dict) -> None:
        svc = self._mcp_service()
        if svc is None or not self._mcp_local_only():
            return
        try:
            settings = svc.update_settings(payload)
        except ValueError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        except OSError as exc:
            self._send_json({"ok": False, "error": f"not saved: {exc}"}, 500)
            return
        self._send_json({"ok": True, "settings": settings, "status": svc.status()})

    @route("POST", "/api/mcp/token")
    def _api_mcp_token(self, payload: dict) -> None:
        del payload
        svc = self._mcp_service()
        if svc is None or not self._mcp_local_only():
            return
        self._send_json({"ok": True, "token": svc.regenerate_token()})

    @route("GET", "/api/mcp/status")
    def _api_mcp_status(self) -> None:
        svc = self._mcp_service()
        if svc is not None:
            self._send_json(svc.status())

    @route("POST", "/api/mcp/stop")
    def _api_mcp_stop(self, payload: dict) -> None:
        """The flight indicator's Stop: take every permission that changes anything away."""
        del payload
        svc = self._mcp_service()
        if svc is None:
            return
        try:
            settings = svc.stop()
        except OSError as exc:
            self._send_json({"ok": False, "error": f"not saved: {exc}"}, 500)
            return
        self._send_json({"ok": True, "settings": settings, "status": svc.status()})

    @route("POST", "/api/mcp/confirm")
    def _api_mcp_confirm(self, payload: dict) -> None:
        svc = self._mcp_service()
        if svc is None:
            return
        if not isinstance(payload.get("allow"), bool):
            self._send_json({"ok": False, "error": "allow must be true or false"}, 400)
            return
        if not svc.answer(payload.get("id"), payload["allow"]):
            self._send_json({"ok": False, "error": "nothing is waiting under that id"}, 404)
            return
        self._send_json({"ok": True})

    @route("POST", "/api/mcp/window")
    def _api_mcp_window(self, payload: dict) -> None:
        """A Corvus window is open, and these are the plugin tools it can run."""
        svc = self._mcp_service()
        if svc is None:
            return
        try:
            result = svc.window_heartbeat(payload.get("id"), payload.get("tools"))
        except ValueError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        self._send_json({"ok": True, **result})

    @route("POST", "/api/mcp/result")
    def _api_mcp_result(self, payload: dict) -> None:
        """A window's answer to a plugin tool call."""
        svc = self._mcp_service()
        if svc is None:
            return
        if not svc.call_result(payload.get("id"), payload.get("ok"),
                               payload.get("result"), payload.get("error")):
            self._send_json({"ok": False, "error": "no call is waiting under that id"}, 404)
            return
        self._send_json({"ok": True})

    def _bind_mcp(self, buf: Any, job_id: str | None = None) -> tuple[Any, Any]:
        """The ``mcp`` event topic: status, confirmations, plugin calls, log lines."""
        del job_id
        svc = self.mcp
        if svc is None:
            return (lambda: None), None
        listener = buf.put_fifo
        svc.add_listener(listener)
        return (lambda: svc.remove_listener(listener)), dict(svc.status(), type="status")
