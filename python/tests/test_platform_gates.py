"""Tests that assert POSIX permission bits say so, so Windows skips them.

Not a test of any module: a guard on the suite itself. Windows reports every
file as 0o666 and every folder as 0o777 whatever was asked for (``os.chmod``
sets only the read-only flag), so ``mode == 0o600`` cannot pass there. Written
on macOS or Linux, such a test passes locally and fails only on the
``test-windows`` runner, after the push, and blocks the release. That happened
twice to ``tests/test_video.py`` in one commit.

The rule: a test function that compares anything against an octal mode
literal (``0o600``, ``"0o700"``) is gated. Any of these counts as a gate:

- ``@posix_permissions`` from ``conftest``, the usual one;
- a ``skipif`` decorator, or a module-level mark it names, that tests
  ``os.name`` or ``sys.platform``;
- the comparison sitting in either branch of ``if os.name == "posix":`` (or
  any ``if`` on ``os.name`` / ``sys.platform``), for a test that checks more
  than the mode;
- ``if os.name == "nt": pytest.skip(...)`` in the test's own body.

stdlib only; the test files are read as source, never imported.
"""
from __future__ import annotations

import ast
import pathlib
import re

TESTS = pathlib.Path(__file__).resolve().parent
_MODE_TEXT = re.compile(r"0o[0-7]{3,4}")
_GATE_TEXT = re.compile(r"posix_permissions|os\.name|sys\.platform|win32")


def _mode_literal(node: ast.AST, source: str) -> bool:
    if not isinstance(node, ast.Constant):
        return False
    if isinstance(node.value, str):
        return bool(_MODE_TEXT.fullmatch(node.value))
    if isinstance(node.value, int) and not isinstance(node.value, bool):
        return (ast.get_source_segment(source, node) or "").lower().startswith("0o")
    return False


def _gated_names(tree: ast.Module, source: str) -> set[str]:
    """Module-level names bound to a platform condition, like ``posix_only``."""
    names = set()
    for node in tree.body:
        if isinstance(node, ast.Assign) and _GATE_TEXT.search(ast.get_source_segment(source, node) or ""):
            names.update(t.id for t in node.targets if isinstance(t, ast.Name))
        if isinstance(node, ast.ImportFrom) and node.module == "conftest":
            names.update(a.asname or a.name for a in node.names if a.name == "posix_permissions")
    return names


def _is_gate(expr: ast.AST, source: str, gated: set[str]) -> bool:
    text = ast.get_source_segment(source, expr) or ""
    return bool(_GATE_TEXT.search(text)) or any(
        isinstance(n, ast.Name) and n.id in gated for n in ast.walk(expr))


def _skips_itself(func: ast.AST, source: str, gated: set[str]) -> bool:
    """Whether the test body opens with ``if <platform>: pytest.skip(...)``."""
    return any(
        isinstance(stmt, ast.If) and _is_gate(stmt.test, source, gated)
        and any(isinstance(n, ast.Attribute) and n.attr == "skip" for n in ast.walk(stmt))
        for stmt in func.body)


def _ungated_mode_checks(path: pathlib.Path) -> list[str]:
    source = path.read_text(encoding="utf-8")
    tree = ast.parse(source, filename=str(path))
    gated = _gated_names(tree, source)
    if "pytestmark" in gated:
        return []
    found = []

    def visit(node: ast.AST, guarded: bool, test: str) -> None:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            if node.name.startswith("test"):
                test = node.name
                guarded = (guarded or _skips_itself(node, source, gated)
                           or any(_is_gate(d, source, gated) for d in node.decorator_list))
        elif isinstance(node, ast.ClassDef):
            guarded = guarded or any(_is_gate(d, source, gated) for d in node.decorator_list)
        elif isinstance(node, ast.If) and _is_gate(node.test, source, gated):
            guarded = True
        elif (isinstance(node, ast.Compare) and test and not guarded
              and any(_mode_literal(n, source) for n in ast.walk(node))):
            found.append(f"{path.name}:{node.lineno} in {test}(): "
                         f"{ast.get_source_segment(source, node)}")
        for child in ast.iter_child_nodes(node):
            visit(child, guarded, test)

    visit(tree, False, "")
    return found


def test_every_permission_bit_check_is_skipped_on_windows():
    found = [line for path in sorted(TESTS.glob("test_*.py")) for line in _ungated_mode_checks(path)]
    assert not found, (
        "These compare POSIX permission bits, which Windows does not report, so they fail "
        "only on the test-windows runner. Put the check in a test of its own marked "
        "@posix_permissions (from conftest import posix_permissions), or inside "
        "`if os.name == \"posix\":` when the test checks more than the mode:\n  "
        + "\n  ".join(found)
    )


def test_the_guard_finds_an_ungated_check_and_accepts_every_gate(tmp_path):
    (tmp_path / "test_sample.py").write_text(
        "import os, sys, stat, pytest\n"
        "from conftest import posix_permissions\n"
        "posix_only = pytest.mark.skipif(sys.platform == 'win32', reason='x')\n"
        "def test_bare(p):\n"
        "    assert stat.S_IMODE(os.stat(p).st_mode) == 0o700\n"
        "def test_via_a_fake(proc):\n"
        "    assert proc.list_mode == 0o600\n"
        "def test_as_text(p):\n"
        "    assert oct(p.stat().st_mode & 0o777) == '0o600'\n"
        "@posix_permissions\n"
        "def test_marked(p):\n"
        "    assert p.mode == 0o600\n"
        "@posix_only\n"
        "def test_named_mark(p):\n"
        "    assert p.mode == 0o600\n"
        "@pytest.mark.skipif(os.name == 'nt', reason='x')\n"
        "def test_inline_mark(p):\n"
        "    assert p.mode == 0o600\n"
        "def test_inline_if(p):\n"
        "    if os.name == 'posix':\n"
        "        assert p.mode == 0o600\n"
        "def test_skips_itself(p):\n"
        "    if os.name == 'nt':\n"
        "        pytest.skip('x')\n"
        "    assert p.mode == 0o600\n"
        "def test_not_a_mode(p):\n"
        "    assert p.count == 600\n",
        encoding="utf-8")
    found = _ungated_mode_checks(tmp_path / "test_sample.py")
    assert [line.split(" in ")[1].split("(")[0] for line in found] == [
        "test_bare", "test_via_a_fake", "test_as_text"]
