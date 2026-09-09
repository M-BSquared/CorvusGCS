"""Version single-source-of-truth regression tests.

The VERSION file is the only hand-authored version string. Everything else
reads it dynamically: ``corvus.version.get_version()`` at import time and
``GET /api/version`` at runtime. These tests pin all three layers and
grep-scan the source tree so a future change cannot smuggle in a hardcoded
literal alongside the canonical one.

The .py/.js/.html/.css scan overlaps deliberately with
``tests/test_integration_review.py`` (which keeps its own narrower check);
this file extends coverage to ``.md`` and asserts the CalVer shape plus the
HTTP endpoint contract that the integration file does not exercise. The
existing tests are left untouched.
"""
from __future__ import annotations

import http.client
import json
import pathlib
import re
import sys

_REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
_TESTS_DIR = pathlib.Path(__file__).resolve().parent
if str(_TESTS_DIR) not in sys.path:
    sys.path.insert(0, str(_TESTS_DIR))

from corvus.version import get_version  # noqa: E402

# CalVer: the product ships a calendar version YYYY.M.P (year.month.patch),
# not semver. The format pin lives here so a stray semver bump is caught.
_CALVER_RE = re.compile(r"^\d{4}\.\d+\.\d+$")

# Files that are allowed to name the version literally. VERSION is the
# canonical source; corvus/version.py reads it; pyproject.toml carries a
# deliberate "0.0.0" tooling placeholder (excluded so it is not flagged);
# Plan.md is a planning doc that may reference target versions.
_CANONICAL_FILES = {
    pathlib.PurePosixPath("VERSION"),
    pathlib.PurePosixPath("corvus/version.py"),
    pathlib.PurePosixPath("pyproject.toml"),
    pathlib.PurePosixPath("Plan.md"),
    # README.md carries the version in its badge on purpose — a real number,
    # rewritten from VERSION by the pre-commit hook. It is checked separately
    # and more strictly by test_readme_version_badge_matches_the_version_file,
    # which also enforces that the number appears nowhere else in the file.
    pathlib.PurePosixPath("README.md"),
}

# Directories never scanned: build artifacts, VCS, caches, agent docs, and
# the node tooling dir. The AppImage at the repo root is binary and has no
# scanned extension, so it is skipped by the suffix filter below regardless.
_SKIP_DIR_PARTS = {
    ".git", "__pycache__", ".pytest_cache", "node_modules",
    "build", ".opencode",
}

_SCANNED_EXTS = {".py", ".js", ".html", ".css", ".md"}


def _version_from_file() -> str:
    return (_REPO_ROOT / "VERSION").read_text(encoding="utf-8").strip()


# ---------------------------------------------------------------------------
# 1. VERSION file shape (CalVer YYYY.M.P, exactly one line)
# ---------------------------------------------------------------------------

def test_version_file_is_calver_single_line() -> None:
    raw = (_REPO_ROOT / "VERSION").read_text(encoding="utf-8")
    # Exactly one line, no trailing blank lines, CalVer-shaped.
    assert raw.endswith("\n"), "VERSION file must end with a single newline"
    lines = raw.splitlines()
    assert len(lines) == 1, f"VERSION must hold one line, got {len(lines)}"
    assert _CALVER_RE.match(lines[0]), (
        f"VERSION {lines[0]!r} is not CalVer YYYY.M.P"
    )


# ---------------------------------------------------------------------------
# 2. corvus.version.get_version() returns exactly the VERSION string
# ---------------------------------------------------------------------------

def test_get_version_equals_version_file() -> None:
    assert get_version() == _version_from_file()


def test_version_module_exposes_version_and_dunder() -> None:
    import corvus.version as v
    assert v.VERSION == _version_from_file()
    assert v.__version__ == _version_from_file()
    assert v.get_version() == _version_from_file()


# ---------------------------------------------------------------------------
# 3. GET /api/version returns the canonical shape with the same string
# ---------------------------------------------------------------------------

def test_api_version_endpoint_shape(server_with_store) -> None:
    server = server_with_store
    expected = _version_from_file()
    conn = http.client.HTTPConnection(
        "127.0.0.1", server.server_address[1], timeout=5,
    )
    try:
        conn.request("GET", "/api/version")
        resp = conn.getresponse()
        body = resp.read()
    finally:
        conn.close()
    assert resp.status == 200
    data = json.loads(body)
    # Exact shape per AGENTS.md: product + version + px4_profile.
    assert set(data) == {"product", "version", "px4_profile"}
    assert data["product"] == "Corvus GCS"
    assert data["version"] == expected, (
        f"/api/version version {data['version']!r} != VERSION {expected!r}"
    )
    assert isinstance(data["px4_profile"], str) and data["px4_profile"]


# ---------------------------------------------------------------------------
# 4. Grep-scan: no hardcoded version literal outside the canonical sources
#    across .py/.js/.html/.css/.md (excludes VERSION, corvus/version.py,
#    pyproject.toml, Plan.md, build/, .git/, __pycache__/).
# ---------------------------------------------------------------------------

def test_no_hardcoded_version_literal_outside_canonical_sources() -> None:
    version = _version_from_file()
    assert version, "VERSION file is empty"
    offenders: list[str] = []
    for path in _REPO_ROOT.rglob("*"):
        if not path.is_file():
            continue
        rel = path.relative_to(_REPO_ROOT)
        if any(part in _SKIP_DIR_PARTS for part in rel.parts):
            continue
        # Compare as posix: _CANONICAL_FILES holds PurePosixPath, and a
        # WindowsPath never equals one, so on Windows every canonical
        # file fell through the skip and README.md was reported.
        if pathlib.PurePosixPath(rel.as_posix()) in _CANONICAL_FILES:
            continue
        if path.suffix.lower() not in _SCANNED_EXTS:
            continue
        try:
            data = path.read_bytes()
        except OSError:
            continue
        # Binary heuristic: skip files with NUL bytes in the first 8 KiB so a
        # stray binary masquerading with a text extension is not decoded.
        if b"\x00" in data[:8192]:
            continue
        text = data.decode("utf-8", errors="ignore")
        if version in text:
            offenders.append(rel.as_posix())
    assert not offenders, (
        f"hardcoded version literal {version!r} found in: {offenders}"
    )


def test_readme_version_badge_matches_the_version_file() -> None:
    """The README shows a real version number, and it must be the real one.

    The old contract was "no version literal in the README" — which the badge
    now deliberately breaks, because a pointer to a file is not what a reader
    wants at the top of a page. So the guarantee moves rather than disappears:
    the number may appear, in the badge, and it has to agree with VERSION. The
    pre-commit hook rewrites it from VERSION on every commit, so a mismatch here
    means the hook did not run (``--no-verify``, or ``core.hooksPath`` unset in
    a fresh clone) and the README is lying about which build it describes.
    """
    version = _version_from_file()
    text = (_REPO_ROOT / "README.md").read_text(encoding="utf-8")

    # The exact marker comment, not just the string: the note further down the
    # README explains the marker by name, so a substring check would still pass
    # after somebody deleted the real one — and the hook would then quietly
    # stop updating the badge with nothing failing.
    assert "<!-- corvus:version-badge -->" in text, (
        "the <!-- corvus:version-badge --> marker comment is gone — the "
        "pre-commit hook locates the badge line by it and silently stops "
        "updating without it"
    )
    assert f"badge/Version-{version}-" in text, (
        f"README version badge does not show {version!r}; run "
        "`git config core.hooksPath .githooks` and commit again"
    )
    assert f'alt="Version {version}"' in text, (
        f"README version badge alt text does not show {version!r} — screen "
        "readers would announce a different version from the image"
    )

    # The number belongs to the badge and nowhere else: two copies in one file
    # is the stale-literal problem the old rule existed to prevent.
    stale = [
        n for n, line in enumerate(text.splitlines(), 1)
        if version in line and "badge/Version-" not in line and 'alt="Version' not in line
    ]
    assert not stale, (
        f"version literal {version!r} also appears outside the badge, on "
        f"line(s) {stale} — the badge is the only place it may be written"
    )


def test_pyproject_carries_only_placeholder_not_real_version() -> None:
    # pyproject.toml is a tooling-only placeholder; the real version never
    # lives here. Assert it carries 0.0.0 and NOT the canonical string.
    version = _version_from_file()
    text = (_REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    assert "0.0.0" in text, "pyproject.toml should carry the 0.0.0 placeholder"
    assert version not in text, (
        f"pyproject.toml must not carry the real version {version!r}"
    )
