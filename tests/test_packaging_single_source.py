"""The runtime dependency list, the Python version, and where builds land.

These are not unit tests of any module. They are regression tests for a class
of failure that only shows up in a shipped artifact, weeks later, on someone
else's machine — the kind the suite cannot otherwise see.

The dependency list used to exist in five places in three formats:
environment.yml's pip: block (with floors), both CI pipelines (``pip install
pymavlink pyserial paramiko``, no floors), build-appimage.sh (no floors),
build-macos-app.sh (a regex that screen-scraped environment.yml) and
build-windows.ps1 (a hardcoded array). The Linux artifact was the one that
mattered: built with no version floors at all, it could be produced against a
pymavlink older than the ``>=2.4`` the code requires, and nothing would catch
it until the field.

The Python version was inconsistent the same way: ``>=3.10`` in
pyproject.toml, 3.11 in environment.yml, 3.12 in both pipelines. So the
tested, the developed and the shipped interpreter could all be different ones,
and "it passes CI" did not mean "it runs on what we ship".

stdlib only, and everything is read off disk as text — these files are
consumed by pip, bash, PowerShell and two CI runners, none of which this
suite can execute.
"""
from __future__ import annotations

import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent

RUNTIME = ROOT / "requirements.txt"
HEADLESS = ROOT / "requirements-headless.txt"
DEV = ROOT / "requirements-dev.txt"

# Everything that installs Corvus's runtime dependencies.
INSTALLERS = [
    ROOT / "environment.yml",
    ROOT / "build-appimage.sh",
    ROOT / "build-macos-app.sh",
    ROOT / "build-windows.ps1",
    ROOT / ".github" / "workflows" / "build.yml",
    ROOT / ".gitlab-ci.yml",
]

# The packages the application needs at runtime. Adding one here without
# adding it to requirements.txt fails, which is the point.
EXPECTED_RUNTIME = {
    "pymavlink", "paramiko", "pyserial", "PyQt6", "PyQt6-WebEngine",
}


def _requirements(path: pathlib.Path) -> dict[str, str]:
    """{package: specifier} from a requirements file, following ``-r``."""
    out: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("-r "):
            out.update(_requirements(path.parent / line[3:].strip()))
            continue
        name = re.split(r"[<>=!~\[]", line, maxsplit=1)[0].strip()
        out[name] = line
    return out


def test_every_runtime_dependency_states_a_floor() -> None:
    """A package with no floor is a package the build may get wrong."""
    reqs = _requirements(RUNTIME)
    assert set(reqs) == EXPECTED_RUNTIME, (
        f"requirements.txt lists {sorted(reqs)}, expected {sorted(EXPECTED_RUNTIME)}"
    )
    for name, spec in reqs.items():
        assert re.search(r"[><=~]=", spec), (
            f"{name} has no version floor in requirements.txt; the AppImage "
            "used to be built this way and could ship against a pymavlink "
            "older than the code needs"
        )


def test_the_headless_list_is_the_runtime_list_without_qt() -> None:
    """CI runs the suite without Qt on purpose; the floors still apply."""
    runtime = set(_requirements(RUNTIME))
    headless = set(_requirements(HEADLESS))
    assert headless < runtime, "requirements-headless.txt must be a subset"
    assert runtime - headless == {"PyQt6", "PyQt6-WebEngine"}


def test_the_dev_list_does_not_drag_qt_into_a_test_job() -> None:
    """~100 MB of Chromium in every test job, to import none of it."""
    dev = set(_requirements(DEV))
    assert "PyQt6" not in dev and "pymavlink" not in dev, (
        "requirements-dev.txt must stay tooling-only so CI can pair it with "
        "requirements-headless.txt"
    )
    assert "pytest" in dev and "ruff" in dev


def test_no_installer_hardcodes_the_dependency_list() -> None:
    """Every one of them reads the file instead of restating it."""
    for path in INSTALLERS:
        text = path.read_text(encoding="utf-8")
        assert "requirements" in text, f"{path.name} installs nothing from requirements*.txt"
        for package in ("pymavlink", "paramiko", "pyserial"):
            # A mention in a comment is fine; an install line is the problem.
            for line in text.splitlines():
                stripped = line.strip()
                if stripped.startswith("#") or stripped.startswith("//"):
                    continue
                if package in stripped and ("pip install" in stripped or "Deps" in stripped):
                    raise AssertionError(
                        f"{path.name} names {package} in an install line: "
                        f"{stripped!r}. The list lives in requirements.txt."
                    )


def test_the_macos_build_no_longer_parses_yaml_with_a_regex() -> None:
    """It screen-scraped environment.yml's pip: block to stay in sync."""
    text = (ROOT / "build-macos-app.sh").read_text(encoding="utf-8")
    assert "RUNTIME_DEPS" not in text
    assert 'pip install -r "$REQUIREMENTS"' in text


def test_one_python_version_across_the_toolchain() -> None:
    """Tested, developed and shipped must be the same interpreter."""
    pyproject = (ROOT / "pyproject.toml").read_text(encoding="utf-8")
    assert 'requires-python = ">=3.12"' in pyproject

    env = (ROOT / "environment.yml").read_text(encoding="utf-8")
    assert "python=3.12" in env

    for ci in (ROOT / ".github" / "workflows" / "build.yml", ROOT / ".gitlab-ci.yml"):
        text = ci.read_text(encoding="utf-8")
        versions = set(re.findall(r"python[-:]?(?:version:)?\s*['\"]?3\.(\d+)", text))
        assert versions <= {"12"}, f"{ci.name} names Python 3.{sorted(versions)}"


def test_both_pipelines_run_a_linter() -> None:
    """Neither did, which is how six dead locals sat in the tree — including
    an AppUserModelID computed and thrown away, leaving the Windows taskbar
    showing the launcher's icon instead of Corvus's."""
    for ci in (ROOT / ".github" / "workflows" / "build.yml", ROOT / ".gitlab-ci.yml"):
        text = ci.read_text(encoding="utf-8")
        assert "ruff check" in text, f"{ci.name} runs no linter"


def test_builds_write_into_dist_not_the_repo_root() -> None:
    """Two finished .dmg files, 600 MB between them, sat in the checkout."""
    checks = {
        "build-appimage.sh": 'OUTPUT="$REPO_DIR/dist/',
        "build-macos-app.sh": 'DMG="$DIST_DIR/',
        "build-windows.ps1": '"dist\\Corvus_GCS-',
    }
    for name, expected in checks.items():
        text = (ROOT / name).read_text(encoding="utf-8")
        assert expected in text, f"{name} does not write its artifact into dist/"

    gitignore = (ROOT / ".gitignore").read_text(encoding="utf-8")
    assert "/dist/" in gitignore
