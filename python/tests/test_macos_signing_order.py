"""The macOS bundle is sealed after it is final, and checked.

A release once opened on the Mac that built it and was refused as "damaged"
on every Mac that downloaded it, because a seal was broken by a file written
after signing. Nothing on the build machine asks Gatekeeper, so nothing there
noticed. A download carries com.apple.quarantine, Gatekeeper verifies the
seal, and a broken one leaves the operator no way to open the app at all.
The bundle is now one executable with its resources, so there are no nested
seals: the .app is signed once, after the plist and icon are written.

Read as text: this suite cannot run codesign.
"""
from __future__ import annotations

import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
SCRIPT = (ROOT.parent / "build-macos-app.sh").read_text(encoding="utf-8")
SIGN = 'codesign --force --sign "$CODESIGN_IDENTITY" "$APP"'


def test_the_bundle_is_signed_once_after_it_is_final() -> None:
    assert SCRIPT.count("codesign --force") == 1
    sign = SCRIPT.index(SIGN)
    assert SCRIPT.index('cat > "$APP/Contents/Info.plist"') < sign
    assert SCRIPT.index('-o "$RES/corvus-gcs.icns"') < sign
    assert SCRIPT.index('cp -RX "$REPO_DIR/assets/licenses"') < sign


def test_the_signature_is_verified_as_gatekeeper_would() -> None:
    """Once on the .app, once on the copy inside the .dmg, both after signing."""
    check = "codesign --verify --deep --strict"
    sign = SCRIPT.index(SIGN)
    first = SCRIPT.index(check)
    assert first > sign
    dmg = SCRIPT.index("hdiutil create")
    assert SCRIPT.index(check, dmg) > dmg
    assert 'hdiutil attach -nobrowse -readonly' in SCRIPT


def test_a_failed_dmg_is_retried_and_says_why() -> None:
    """hdiutil's reason went only to build/hdiutil.log, which CI does not keep.

    A release failed on "ERROR: hdiutil failed" and nothing else, on a runner
    where hdiutil create fails intermittently.
    """
    create = SCRIPT.index("if hdiutil create")
    loop = SCRIPT.rindex("for attempt in", 0, create)
    assert create - loop < 400, "hdiutil create is not inside the retry loop"
    failure = SCRIPT.index('echo "ERROR: hdiutil failed', create)
    assert 'tail -n 20 "$BUILD_DIR/hdiutil.log"' in SCRIPT[failure:failure + 300]
