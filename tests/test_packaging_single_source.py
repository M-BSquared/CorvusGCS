"""The dependency list, the Python version, and where builds land.

These are not unit tests of any module. They are regression tests for a class
of failure that only shows up in a shipped artifact, weeks later, on someone
else's machine — the kind the suite cannot otherwise see.

The dependency list used to exist in five places in three formats: a conda
environment.yml's pip: block (with floors), both CI pipelines (``pip install
pymavlink pyserial paramiko``, no floors), build-appimage.sh (no floors),
build-macos-app.sh (a regex that screen-scraped environment.yml) and
build-windows.ps1 (a hardcoded array). The Linux artifact was the one that
mattered: built with no version floors at all, it could be produced against a
pymavlink older than the ``>=2.4`` the code requires, and nothing would catch
it until the field. A first consolidation still left four files — three
requirements*.txt and environment.yml — beside pyproject.toml; now
pyproject.toml's ``[dependency-groups]`` is the only one, and every installer
reads it with ``pip install --group``.

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
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
PYPROJECT = ROOT / "pyproject.toml"
GITHUB_CI = ROOT / ".github" / "workflows" / "build.yml"
GITLAB_CI = ROOT / ".gitlab-ci.yml"

# Everything that installs Python packages, and the group each one installs.
INSTALLERS = {
    ROOT / "run.sh": ['--group "$REPO_DIR/pyproject.toml:dev"'],
    ROOT / "build-appimage.sh": ['--group "$REPO_DIR/pyproject.toml:app"'],
    ROOT / "build-macos-app.sh": ['--group "$REPO_DIR/pyproject.toml:app"'],
    ROOT / "build-windows.ps1": [
        '"--group", "${Pyproject}:app"',
        '--group "${Pyproject}:package-windows"',
    ],
    GITHUB_CI: ["pip install --group test", "pip install --group lint"],
    GITLAB_CI: ["pip install --group test", "pip install --group lint"],
}

# The packages the application needs at runtime. Adding one here without
# adding it to pyproject.toml fails, which is the point.
EXPECTED_RUNTIME = {
    "pymavlink", "paramiko", "pyserial", "PyQt6", "PyQt6-WebEngine",
}
QT = {"PyQt6", "PyQt6-WebEngine"}


def _groups() -> dict:
    return tomllib.loads(PYPROJECT.read_text(encoding="utf-8"))["dependency-groups"]


def _group(name: str) -> dict[str, str]:
    """{package: specifier} of one dependency group, following include-group."""
    out: dict[str, str] = {}
    for item in _groups()[name]:
        if isinstance(item, dict):
            out.update(_group(item["include-group"]))
            continue
        package = re.split(r"[<>=!~\[;\s]", item, maxsplit=1)[0].strip()
        out[package] = item
    return out


def test_pyproject_is_the_only_dependency_file() -> None:
    """One list. A second file is how the floors went missing last time."""
    strays = [
        path.name
        for pattern in ("requirements*.txt", "requirements*.in", "environment*.yml",
                        "environment*.yaml", "setup.py", "setup.cfg", "Pipfile",
                        "Pipfile.lock", "poetry.lock", "uv.lock")
        for path in ROOT.glob(pattern)
    ]
    assert not strays, (
        f"{strays} restate the dependency list; it lives in pyproject.toml's "
        "[dependency-groups] and nowhere else"
    )


def test_every_runtime_dependency_states_a_floor() -> None:
    """A package with no floor is a package the build may get wrong."""
    app = _group("app")
    assert set(app) == EXPECTED_RUNTIME, (
        f"the app group lists {sorted(app)}, expected {sorted(EXPECTED_RUNTIME)}"
    )
    for name, spec in app.items():
        assert re.search(r"[><=~]=", spec), (
            f"{name} has no version floor in pyproject.toml; the AppImage "
            "used to be built this way and could ship against a pymavlink "
            "older than the code needs"
        )


def test_the_headless_group_is_the_app_group_without_qt() -> None:
    """CI runs the suite without Qt on purpose; the floors still apply."""
    app = _group("app")
    headless = _group("headless")
    assert set(app) - set(headless) == QT
    for name, spec in headless.items():
        assert app[name] == spec, f"{name}: headless and app disagree on the floor"


def test_the_ci_groups_do_not_drag_qt_into_a_test_job() -> None:
    """~100 MB of Chromium in every test job, to import none of it."""
    test = _group("test")
    lint = _group("lint")
    assert not QT & set(test), "the test group must stay headless"
    assert set(_group("headless")) <= set(test)
    assert "pytest" in test
    assert set(lint) == {"ruff"}, "the lint job needs ruff and nothing else"


def test_the_dev_venv_has_everything_the_other_groups_have() -> None:
    """run.sh's venv runs the app, the suite and the linter."""
    dev = set(_group("dev"))
    for name in ("app", "test", "lint"):
        assert set(_group(name)) <= dev, f"dev is missing part of {name}"
    assert not {"pyinstaller", "Pillow"} & dev, "build tools stay out of the dev venv"


def test_every_installer_reads_its_group_from_pyproject() -> None:
    for path, needles in INSTALLERS.items():
        text = path.read_text(encoding="utf-8")
        for needle in needles:
            assert needle in text, f"{path.name} does not install {needle!r}"


def test_no_installer_hardcodes_the_dependency_list() -> None:
    """Every one of them reads the file instead of restating it."""
    packages = ("pymavlink", "paramiko", "pyserial", "PyQt6", "pytest", "ruff",
                "pyinstaller", "Pillow")
    for path in INSTALLERS:
        for line in path.read_text(encoding="utf-8").splitlines():
            stripped = line.strip()
            # A mention in a comment is fine; an install line is the problem.
            if stripped.startswith("#") or stripped.startswith("//"):
                continue
            if "pip install" not in stripped and "Deps" not in stripped:
                continue
            for package in packages:
                if re.search(rf"\b{re.escape(package)}\b", stripped, re.IGNORECASE):
                    raise AssertionError(
                        f"{path.name} names {package} in an install line: "
                        f"{stripped!r}. The list lives in pyproject.toml."
                    )


def test_the_windows_build_tools_are_installed_where_they_are_used() -> None:
    """Pillow went into CI's host interpreter; the icon step runs the venv's.

    So the Windows release artifact never carried the mark. The build tools
    are a pyproject group installed into the same build venv that renders the
    .ico and runs PyInstaller.
    """
    tools = _group("package-windows")
    assert {"pyinstaller", "Pillow"} <= set(tools)
    ps1 = (ROOT / "build-windows.ps1").read_text(encoding="utf-8")
    assert "& $VenvPy -c $IcoScript" in ps1
    assert "Pillow" not in GITHUB_CI.read_text(encoding="utf-8").split("windows-app:")[1]


def test_the_macos_build_no_longer_parses_yaml_with_a_regex() -> None:
    """It screen-scraped environment.yml's pip: block to stay in sync."""
    text = (ROOT / "build-macos-app.sh").read_text(encoding="utf-8")
    assert "RUNTIME_DEPS" not in text
    assert 'pip install "${DEPS_ARGS[@]}"' in text


def test_a_release_lock_still_wins_over_the_floors() -> None:
    """requirements.lock is generated, not a second list — and it is what
    makes a rebuild of a tag resolve to the versions that tag shipped with."""
    for name in ("build-appimage.sh", "build-macos-app.sh", "build-windows.ps1"):
        text = (ROOT / name).read_text(encoding="utf-8")
        assert "CORVUS_REQUIREMENTS" in text, name
        assert "requirements.lock" in text, name


def test_one_python_version_across_the_toolchain() -> None:
    """Tested, developed and shipped must be the same interpreter."""
    project = tomllib.loads(PYPROJECT.read_text(encoding="utf-8"))["project"]
    assert project["requires-python"] == ">=3.12"

    run_sh = (ROOT / "run.sh").read_text(encoding="utf-8")
    assert "sys.version_info < (3, 12)" in run_sh

    for ci in (GITHUB_CI, GITLAB_CI):
        text = ci.read_text(encoding="utf-8")
        versions = set(re.findall(r"python[-:]?(?:version:)?\s*['\"]?3\.(\d+)", text))
        assert versions <= {"12"}, f"{ci.name} names Python 3.{sorted(versions)}"


def test_setup_python_caches_on_pyproject() -> None:
    """setup-python's pip cache keys on **/requirements.txt unless told
    otherwise, and fails the step outright when it finds none."""
    text = GITHUB_CI.read_text(encoding="utf-8")
    caches = text.count("cache: pip")
    assert caches, "no pip cache configured"
    assert text.count("cache-dependency-path: pyproject.toml") == caches


def test_the_dev_venv_stays_out_of_git() -> None:
    gitignore = (ROOT / ".gitignore").read_text(encoding="utf-8").splitlines()
    assert "/.venv/" in gitignore


def test_both_pipelines_run_a_linter() -> None:
    """Neither did, which is how six dead locals sat in the tree — including
    an AppUserModelID computed and thrown away, leaving the Windows taskbar
    showing the launcher's icon instead of Corvus's."""
    for ci in (GITHUB_CI, GITLAB_CI):
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
