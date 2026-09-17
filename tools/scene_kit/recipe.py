"""Turning a scene into the exact browser steps that photograph it.

The state a screenshot needs is in two places, not one. The aircraft's half
arrives over MAVLink and the backend's half is in a config file — both are the
runner's problem. But the interface's own half lives in the browser: which
theme, how large, where the instrument panel sits, which switch on the drawn
transmitter is bound to which channel. That is ``localStorage``, per browser
profile, and nothing on the Python side can write it.

So a scene emits a *recipe*: a numbered list of steps, each one a single call
an agent (or a person) makes against the browser. The steps are deliberately
boring — navigate, run this snippet, click that, wait, shoot — because a recipe
that needs judgement is a recipe that produces a different picture every time.

The JavaScript here is written to be pasted whole into one evaluation. It is
idempotent: running it twice leaves the same state, which matters because the
most common way to lose a screenshot is to re-run half a recipe.
"""
from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

from .library import Scene

# The drawn transmitter's controls (src/js/rc-transmitter.js LAYOUT) against
# the channels the scene's parameter table maps. This binding is the browser's
# knowledge, not the vehicle's — PX4 knows channel 7 arms, not that channel 7
# is the toggle above your left thumb — which is exactly why it is seeded here.
RC_BINDINGS: dict[str, dict[str, int]] = {
    "SA": {"channel": 5, "positions": 3},     # flight mode
    "SB": {"channel": 9, "positions": 3},
    "SF": {"channel": 8, "positions": 2},     # kill
    "SD": {"channel": 7, "positions": 2},     # arm
    "SC": {"channel": 10, "positions": 2},
    "SH": {"channel": 6, "positions": 2},     # return
    "S1": {"channel": 11},
    "S2": {"channel": 12},
}

# Left-nav ids (src/js/sidenav.js) and Setup tile ids (src/js/setup.js).
NAV_IDS = ("home", "setup", "analysis", "logs", "settings")
SETUP_VIEWS = ("calibration", "control", "tuning", "motors", "safety",
               "parameters", "firmware")


@dataclass
class Step:
    """One action, and the tool an agent would use for it."""

    action: str          # resize | navigate | evaluate | wait | screenshot | note
    detail: str
    payload: str = ""

    def as_dict(self) -> dict[str, Any]:
        out = {"action": self.action, "detail": self.detail}
        if self.payload:
            out["payload"] = self.payload
        return out


def bootstrap_js(scene: Scene) -> str:
    """The snippet that puts the browser's half of the scene in place.

    Theme and interface size are also in the backend config, and the config
    wins on load — this sets them anyway so the page is right *before* the
    first config fetch lands, which is the difference between a screenshot and
    a screenshot with a flash of the default theme in it.
    """
    state = {
        "corvus.theme": scene.theme,
        "corvus.scale": str(scene.scale),
        "corvus.hud": json.dumps(scene.hud),
        "corvus.topbarDots": "1" if scene.topbar_dots else "0",
        "corvus.rc.transmitter.bindings.v2": json.dumps(RC_BINDINGS),
        "corvus.rc.transmitter.mode.v1": "2",
    }
    return (
        "(() => { const s = " + json.dumps(state) + "; "
        "for (const k of Object.keys(s)) { try { localStorage.setItem(k, s[k]); } "
        "catch (e) {} } return Object.keys(s).length + ' keys set'; })()"
    )


def navigate_js(scene: Scene) -> str:
    """Click the scene's page — and its Setup sub-page — into view.

    Corvus has no URL routing: the left nav and the Setup tiles are buttons, so
    getting to a page means pressing them in order, with a pause for the page
    to render and its first fetch to land.
    """
    page = scene.page if scene.page in NAV_IDS else "home"
    lines = [
        "const q = (s) => document.querySelector(s);",
        "const w = (ms) => new Promise((r) => setTimeout(r, ms));",
        f"const nav = q('[data-nav=\"{page}\"]'); if (nav) nav.click();",
        "await w(500);",
    ]
    if page == "setup" and scene.view in SETUP_VIEWS:
        lines += [
            f"const tile = q('[data-view=\"{scene.view}\"]'); if (tile) tile.click();",
            "await w(900);",
        ]
    panel = "collapsed" if scene.workspace == "collapsed" else "open"
    lines += [
        # The handle is clicked rather than Corvus.panel.toggle() called: the
        # click is what marks the collapse as the operator's, and app.js only
        # leaves a panel alone across a window resize once somebody has touched
        # it. Calling toggle() directly gets silently undone by the next resize,
        # which is a screenshot with the workspace back open in it.
        "const rp = q('#rightPanel');",
        "const isCollapsed = rp && rp.classList.contains('collapsed');",
        f"const want = {json.dumps(panel == 'collapsed')};",
        "const handle = q('#panelHandle');",
        "if (rp && handle && isCollapsed !== want) handle.click();",
        "await w(350);",
    ]
    if panel == "open" and scene.tab:
        lines += [
            f"const tab = q('.panel-tabs [data-tab=\"{scene.tab}\"]'); "
            "if (tab) tab.click();",
            "await w(300);",
        ]
    lines.append("return (q('#pageView') && !q('#pageView').hidden) "
                 "? 'page: ' + document.body.dataset.page : 'map view';")
    body = " ".join(lines)
    return "(async () => { " + body + " })()"


def calibration_js(sensor: str) -> str:
    """Open one calibration's card and press Start.

    Two clicks, not one: the page lists the seven calibrations as cards
    (``[data-type]``), and the Start button only exists inside the one that is
    open. The ESC calibration adds a confirmation — it spins motors — so the
    snippet answers that too, and says which button it pressed.
    """
    return (
        "(async () => { const q = (s) => document.querySelector(s); "
        "const w = (ms) => new Promise((r) => setTimeout(r, ms)); "
        f"const card = q('[data-type=\"{sensor}\"]'); "
        "if (!card) return 'no card for this sensor'; card.click(); await w(700); "
        "const start = [...document.querySelectorAll('button')]"
        ".find((x) => /^start /i.test((x.textContent || '').trim())); "
        "if (!start) return 'card open, no start button (armed? no link?)'; "
        "start.click(); await w(500); "
        "const go = [...document.querySelectorAll('button')]"
        ".find((x) => /continue|understood|spin/i.test(x.textContent || '')); "
        "if (go) { go.click(); return 'started (confirmed)'; } "
        "return 'started'; })()"
    )


def frame_js(scene: Scene) -> str:
    """Frame the map on the flight, and stop it drifting before the shot.

    Follow mode keeps the aircraft centred, which is right for flying and wrong
    for a photograph: a survey photographed from over the aircraft shows one leg
    and a lot of field. So following is turned off and the map is fitted to the
    scene's own bounds — the track, the home point and a margin — which is a
    frame that does not depend on where in the pattern the aircraft happens to
    be when the shutter goes.
    """
    west, south, east, north = scene.bounds()
    return (
        "(() => { if (!window.Corvus || !Corvus.map || !Corvus.map.isReady()) "
        "return 'map not ready'; Corvus.map.setFollow(false); "
        "const m = Corvus.map.getMap(); "
        f"m.fitBounds([[{west:.6f}, {south:.6f}], [{east:.6f}, {north:.6f}]], "
        "{ padding: 90, duration: 0 }); "
        "return 'framed at zoom ' + m.getZoom().toFixed(2); })()"
    )


def steps(scene: Scene, url: str) -> list[Step]:
    """The whole recipe, in order."""
    width, height = scene.viewport
    out = [
        Step("resize", f"Set the viewport to {width}x{height} — the README's "
                       "images are 16:10 and the layout is designed around it.",
             json.dumps({"width": width, "height": height})),
        Step("navigate", f"Open {url}", url),
        Step("evaluate", "Seed the browser's half of the scene (theme, interface "
                         "size, instrument-panel placement, transmitter bindings).",
             bootstrap_js(scene)),
        Step("navigate", "Reload so the seeded state is applied from the first "
                         "paint, with no flash of the default theme.", url),
        Step("wait", "Let the link come up and the first telemetry land.", "3"),
    ]
    if scene.page != "home" or scene.workspace != "open" or scene.tab != "link":
        out.append(Step("evaluate", "Navigate to the scene's page and set the "
                                    "workspace the way the picture wants it.",
                        navigate_js(scene)))
    if scene.view == "parameters":
        out.append(Step("evaluate",
                        "Start the parameter download — the editor is empty "
                        "until it completes.",
                        "(() => { const b = [...document.querySelectorAll('button')]"
                        ".find((x) => /download/i.test(x.textContent || '')); "
                        "if (b) { b.click(); return 'download started'; } "
                        "return 'download button not found'; })()"))
    if scene.calibration:
        out.append(Step("evaluate",
                        f"Open the {scene.calibration} card and start it. The "
                        "vehicle runs PX4's own transcript and stops where the "
                        "scene says, so the wizard holds still for the picture.",
                        calibration_js(scene.calibration)))
    out.append(Step("wait", f"Let the scene build: {scene.settle:.0f} s. "
                            + (scene.notes or ""), f"{scene.settle:.0f}"))
    if scene.page == "home":
        out.append(Step("evaluate",
                        "Frame the map on the flight — following off, fitted to "
                        "the track and the home point.",
                        frame_js(scene)))
    out.append(Step("screenshot", f"Shoot, and save it as {scene.asset}."))
    return out


def as_text(scene: Scene, url: str) -> str:
    """The recipe as something a person can follow at a terminal."""
    lines = [
        f"scene      {scene.id}",
        f"picture    {scene.asset}",
        f"title      {scene.title}",
        f"theme      {scene.theme}   viewport {scene.viewport[0]}x{scene.viewport[1]}",
        f"page       {scene.page}" + (f" / {scene.view}" if scene.view else ""),
        "",
    ]
    if scene.caption:
        lines += ["caption (the README's words for this picture):",
                  "  " + scene.caption, ""]
    lines.append("steps:")
    for i, step in enumerate(steps(scene, url), 1):
        lines.append(f"  {i}. [{step.action}] {step.detail}")
        if step.payload and step.action in ("evaluate",):
            lines.append("      " + step.payload)
        elif step.payload and step.action in ("navigate", "resize", "wait"):
            lines.append("      " + step.payload)
    return "\n".join(lines)


def as_json(scene: Scene, url: str) -> dict[str, Any]:
    """The recipe as something an agent can consume without parsing prose."""
    return {
        "scene": scene.id,
        "asset": scene.asset,
        "title": scene.title,
        "caption": scene.caption,
        "url": url,
        "viewport": {"width": scene.viewport[0], "height": scene.viewport[1]},
        "theme": scene.theme,
        "page": scene.page,
        "view": scene.view,
        "workspace": scene.workspace,
        "notes": scene.notes,
        "steps": [s.as_dict() for s in steps(scene, url)],
    }
