"""The host's own launchers: the file manager for a folder, the browser for a page.

Both start a program that is not Corvus, so both hand it the operator's
environment rather than the packaged build's (see corvus/child_env.py).
"""
from __future__ import annotations

from typing import Any

import pytest

from corvus import file_manager


class _Spawn:
    def __init__(self) -> None:
        self.calls: list[tuple[list[str], dict[str, Any]]] = []

    def popen(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        return object()

    def run(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        return file_manager.subprocess.CompletedProcess(argv, 0, b"", b"")


@pytest.fixture
def spawn(monkeypatch):
    s = _Spawn()
    monkeypatch.setattr(file_manager.subprocess, "Popen", s.popen)
    monkeypatch.setattr(file_manager.subprocess, "run", s.run)
    monkeypatch.setenv("QTWEBENGINE_CHROMIUM_FLAGS", "--no-sandbox")
    return s


def test_linux_opens_a_page_with_xdg_open_and_the_operators_environment(spawn):
    assert file_manager.open_url("https://example.org/release", platform="linux") is True
    argv, kwargs = spawn.calls[0]
    assert argv == ["xdg-open", "https://example.org/release"]
    assert "QTWEBENGINE_CHROMIUM_FLAGS" not in kwargs["env"]
    assert kwargs["start_new_session"] is True


@pytest.mark.parametrize("url", ["file:///etc/passwd", "javascript:alert(1)", "smb://host/share", ""])
def test_only_web_pages_are_opened(spawn, url):
    assert file_manager.open_url(url, platform="linux") is False
    assert spawn.calls == []


def test_elsewhere_the_browser_module_opens_it(spawn, monkeypatch):
    import webbrowser
    opened: list[str] = []
    monkeypatch.setattr(webbrowser, "open", lambda url: opened.append(url) or True)
    assert file_manager.open_url("http://127.0.0.1:8000/", platform="darwin") is True
    assert opened == ["http://127.0.0.1:8000/"]
    assert spawn.calls == []


def test_linux_falls_back_to_the_browser_module_without_xdg_open(monkeypatch):
    import webbrowser

    def missing(argv, **_kwargs):
        raise FileNotFoundError(argv[0])

    monkeypatch.setattr(file_manager.subprocess, "Popen", missing)
    monkeypatch.setattr(webbrowser, "open", lambda url: True)
    assert file_manager.open_url("https://example.org", platform="linux") is True


def test_a_folder_is_opened_with_the_operators_environment(spawn, tmp_path):
    ok, error = file_manager.open_folder(str(tmp_path))
    assert (ok, error) == (True, "")
    _argv, kwargs = spawn.calls[0]
    assert "QTWEBENGINE_CHROMIUM_FLAGS" not in kwargs["env"]
