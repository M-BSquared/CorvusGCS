"""The dependency list, the Python version, and where builds land.

These are not unit tests of any module. They are regression tests for a class
of failure that only shows up in a shipped artifact, weeks later, on someone
else's machine — the kind the suite cannot otherwise see.

The dependency list used to exist in five places in three formats: a conda
environment.yml's pip: block (with floors), two CI pipelines (``pip install
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
GITHUB_CI = ROOT.parent / ".github" / "workflows" / "build.yml"

# Everything that installs Python packages, and the group each one installs.
INSTALLERS = {
    ROOT.parent / "run.sh": ['--group "$REPO_DIR/python/pyproject.toml:dev"'],
    ROOT.parent / "build-appimage.sh": ['--group "$REPO_DIR/python/pyproject.toml:headless"'],
    ROOT.parent / "build-macos-app.sh": ['--group "$REPO_DIR/python/pyproject.toml:headless"'],
    ROOT.parent / "build-windows.ps1": ['"--group", "${Pyproject}:headless"'],
    GITHUB_CI: ["pip install --group test", "pip install --group lint"],
}

# The packages the tests and tools need at runtime. Adding one here without
# adding it to pyproject.toml fails, which is the point.
EXPECTED_RUNTIME = {"paramiko", "pyserial"}
QT = {"PySide6-Essentials", "PySide6-Addons", "PySide6-WebEngine"}


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
    """A package with no floor is a package a test run may get wrong."""
    headless = _group("headless")
    assert set(headless) == EXPECTED_RUNTIME, (
        f"the headless group lists {sorted(headless)}, expected {sorted(EXPECTED_RUNTIME)}"
    )
    for name, spec in headless.items():
        assert re.search(r"[><=~]=", spec), f"{name} has no version floor in pyproject.toml"


def test_the_pipeline_checks_the_rust_side() -> None:
    """Formatting, clippy and the Rust unit tests run on every push."""
    text = GITHUB_CI.read_text(encoding="utf-8")
    for needle in ("cargo fmt --check", "cargo clippy --workspace --all-targets --locked -- -D warnings", "cargo test --workspace --locked"):
        assert needle in text, needle


def test_pymavlink_is_a_test_reference_only() -> None:
    assert "pymavlink" not in _group("headless")
    assert "pymavlink" in _group("test")
    for path in (ROOT / "corvus").rglob("*.py"):
        for line in path.read_text(encoding="utf-8").splitlines():
            stripped = line.strip()
            assert not stripped.startswith(("from pymavlink", "import pymavlink")), (
                f"{path.relative_to(ROOT)} imports pymavlink; the app uses corvus_link"
            )


def test_the_ci_groups_do_not_drag_qt_into_a_test_job() -> None:
    """~100 MB of Chromium in every test job, to import none of it."""
    test = _group("test")
    lint = _group("lint")
    assert not QT & set(test), "no test needs Qt"
    assert set(_group("headless")) <= set(test)
    assert "pytest" in test
    assert set(lint) == {"ruff"}, "the lint job needs ruff and nothing else"


def test_the_dev_venv_has_everything_the_other_groups_have() -> None:
    """run.sh's venv runs the suite and the linter."""
    dev = set(_group("dev"))
    for name in ("test", "lint"):
        assert set(_group(name)) <= dev, f"dev is missing part of {name}"
    assert not {"pyinstaller", "Pillow"} & dev, "build tools stay out of the dev venv"


def test_no_installer_hardcodes_the_dependency_list() -> None:
    """Every one of them reads the file instead of restating it."""
    packages = ("pymavlink", "paramiko", "pyserial", "PySide6", "PyQt6", "pytest", "ruff",
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


def test_one_python_version_across_the_toolchain() -> None:
    """Tested, developed and shipped must be the same interpreter."""
    project = tomllib.loads(PYPROJECT.read_text(encoding="utf-8"))["project"]
    assert project["requires-python"] == ">=3.12"

    run_sh = (ROOT.parent / "run.sh").read_text(encoding="utf-8")
    assert "sys.version_info < (3, 12)" in run_sh

    text = GITHUB_CI.read_text(encoding="utf-8")
    versions = set(re.findall(r"python[-:]?(?:version:)?\s*['\"]?3\.(\d+)", text))
    assert versions <= {"12"}, f"{GITHUB_CI.name} names Python 3.{sorted(versions)}"


def test_setup_python_caches_on_pyproject() -> None:
    """setup-python's pip cache keys on **/requirements.txt unless told
    otherwise, and fails the step outright when it finds none."""
    text = GITHUB_CI.read_text(encoding="utf-8")
    caches = text.count("cache: pip")
    assert caches, "no pip cache configured"
    assert text.count("cache-dependency-path: python/pyproject.toml") == caches


def test_the_dev_venv_stays_out_of_git() -> None:
    gitignore = (ROOT.parent / ".gitignore").read_text(encoding="utf-8").splitlines()
    assert "/.venv/" in gitignore


def test_the_pipeline_runs_a_linter() -> None:
    """It did not, which is how six dead locals sat in the tree — including
    an AppUserModelID computed and thrown away, leaving the Windows taskbar
    showing the launcher's icon instead of Corvus's."""
    text = GITHUB_CI.read_text(encoding="utf-8")
    assert "ruff check" in text, f"{GITHUB_CI.name} runs no linter"


def test_github_is_the_only_ci() -> None:
    """The GitLab pipeline was retired. A second pipeline is a second place
    for the job list, the apt set and the build distro to drift."""
    assert not (ROOT.parent / ".gitlab-ci.yml").exists()


def test_builds_write_into_dist_not_the_repo_root() -> None:
    """Two finished .dmg files, 600 MB between them, sat in the checkout."""
    checks = {
        "build-appimage.sh": 'OUTPUT="$REPO_DIR/dist/',
        "build-macos-app.sh": 'DMG="$DIST_DIR/',
        "build-windows.ps1": '"dist\\Corvus_GCS-',
    }
    for name, expected in checks.items():
        text = (ROOT.parent / name).read_text(encoding="utf-8")
        assert expected in text, f"{name} does not write its artifact into dist/"

    gitignore = (ROOT.parent / ".gitignore").read_text(encoding="utf-8")
    assert "/dist/" in gitignore


