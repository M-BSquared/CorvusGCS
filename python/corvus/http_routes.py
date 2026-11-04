"""The pieces every route module shares: the @route registry, the lock that
serialises config writes, and the errors that mean a client went away.

Moved out of corvus/server.py, which re-exports them.
"""
from __future__ import annotations

import threading


# Route registry: the @route decorator tags handler methods while the
# CorvusHandler class body executes; the pending entries are wired onto the
# class-level _GET_ROUTES/_POST_ROUTES tables after the class is defined. The
# dispatch methods then resolve path -> method name by lookup instead of an
# if/elif chain, so adding an endpoint is just "decorate a method".
_pending_routes: list[tuple[str, str, str]] = []


def route(http_method: str, path: str):
    """Register the decorated handler as the route for *path*."""
    def decorator(func):
        _pending_routes.append((http_method, path, func.__name__))
        return func
    return decorator


# One writer at a time for the live CorvusConfig object.
#
# The config is a single mutable dataclass shared by every handler thread, and
# the endpoints that change it all do read-modify-write: read the current
# dict, merge the payload, assign the fields back, then serialize the whole
# object to disk. Two of those interleaving is not hypothetical — a settings
# page with two browser tabs open, or a double-clicked toggle, is enough. The
# damage is real: ``_api_config_apply`` assigns seventeen fields one at a
# time, so a save running between the third and the fourth writes a file that
# is half the old config and half the new one; and the SSH list is mutated in
# place by add (append) while remove rebuilds it, so an add and a remove that
# overlap lose an entry outright.
#
# An RLock (not a Lock) because a guarded endpoint calls other guarded helpers
# — ``_save_live_config`` is itself taken under the same lock by its callers.
# Module-level, so it covers every handler thread of every server in the
# process, which is what "one config file" means.
_config_write_lock = threading.RLock()


# Every way a browser tab closing reaches an SSE writer.
#
# The handlers used to catch only BrokenPipeError and ConnectionResetError,
# which is the complete list on Linux and macOS and an incomplete one on
# Windows: closing a tab there surfaces as ConnectionAbortedError (WSAECONNAB
# ORTED, 10053), and a socket torn down under a blocked write raises a bare
# OSError with WSAENOTSOCK (10038). Those escaped the handler, so socketserver
# printed a full traceback per closed tab — noise that buries a real fault in
# the log during a flight. TimeoutError covers a write that stalls on a wedged
# client. All of them mean exactly one thing: stop writing, drop the listener,
# let the thread go. OSError is the base of the first four, so the tuple is
# really "OSError" — spelled out because the specific names are the point.
CLIENT_GONE_ERRORS = (
    BrokenPipeError,
    ConnectionResetError,
    ConnectionAbortedError,
    TimeoutError,
    OSError,
)
