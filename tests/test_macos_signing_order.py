"""The macOS bundle is sealed inside out, after it is final, and checked.

The CI release opened on the Mac that built it and was refused as "damaged"
on every Mac that downloaded it. Two nested seals in the shipped .dmg were broken:
the Python.app stub was signed and then had its Info.plist rewritten and an
icon added, and Python.framework kept the Python Software Foundation's
signature, which seals a _CodeSignature/CodeResources the build never copied.
Nothing on the build machine asks Gatekeeper, so nothing there noticed. A
download carries com.apple.quarantine, Gatekeeper verifies every nested seal,
and a broken one leaves the operator no way to open the app at all.

Read as text: this suite cannot run codesign.
"""
from __future__ import annotations

import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
SCRIPT = (ROOT / "build-macos-app.sh").read_text(encoding="utf-8")
SIGN_LOOP = 'for target in "$FW_DST/Resources/Python.app" "$FW_DST" "$APP"; do'


def test_the_bundle_is_signed_inside_out() -> None:
    """Stub, then the framework that holds it, then the .app that holds both."""
    assert SIGN_LOOP in SCRIPT


def test_the_stub_is_not_sealed_before_it_is_final() -> None:
    assert 'sign "$STUB"' not in SCRIPT
    loop = SCRIPT.index(SIGN_LOOP)
    plist_edit = SCRIPT.index('"$FW_DST/Resources/Python.app/Contents/Info.plist"')
    icon_copy = SCRIPT.index("Python.app/Contents/Resources/corvus-gcs.icns")
    assert plist_edit < loop
    assert icon_copy < loop


def test_the_signature_is_verified_as_gatekeeper_would() -> None:
    """Once on the .app, once on the copy inside the .dmg, both after signing."""
    check = "codesign --verify --deep --strict"
    loop = SCRIPT.index(SIGN_LOOP)
    first = SCRIPT.index(check)
    assert first > loop
    dmg = SCRIPT.index("hdiutil create")
    assert SCRIPT.index(check, dmg) > dmg
    assert 'hdiutil attach -nobrowse -readonly' in SCRIPT
