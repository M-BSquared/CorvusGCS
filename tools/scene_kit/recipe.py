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

# Left-nav ids (src/js/sidenav.js), Setup tile ids (src/js/setup.js) and the
# Analysis page's sub-pages (src/js/analysis.js).
NAV_IDS = ("home", "mission", "setup", "analysis", "logs", "settings")
SETUP_VIEWS = ("calibration", "control", "tuning", "motors", "safety", "battery",
               "sik", "rtk", "remoteid", "parameters", "firmware", "video")
ANALYSIS_VIEWS = ("ulog", "tlog", "review")

# The SSH connections an ``ssh`` scene saves, by the name the card shows. The
# first opens in the SSH tab, the second in a terminal window over the map.
SSH_TAB = "Companion computer"
SSH_WINDOW = "Payload computer"
# Where that window sits: the upper left of the map, which the sortie's track
# leaves empty.
SSH_WINDOW_AT = {"left": 86, "top": 150, "width": 620, "height": 318}


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
    if page == "analysis" and scene.view in ANALYSIS_VIEWS:
        lines += [
            f"const tile = q('.logs-tile[data-view=\"{scene.view}\"]'); "
            "if (tile) tile.click();",
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
        if scene.tab == "future" and scene.plugin:
            lines += [
                "const card = [...document.querySelectorAll('button')].find("
                f"(b) => (b.textContent || '').includes({json.dumps(scene.plugin)})); "
                "if (card) card.click();",
                "await w(500);",
            ]
    lines.append("const active = q('.nav-item.active'); "
                 "return 'on ' + ((active && active.dataset.nav) || '?') + "
                 "(rp && rp.classList.contains('collapsed') ? ', workspace collapsed' : '');")
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


def mission_js(scene: Scene) -> str:
    """Put the scene's plan on the mission planner and frame it.

    ``setPlan`` is the page's own entry for a whole plan, the one Open uses;
    the fit button is then pressed rather than the camera moved from here,
    because it is the framing an operator gets and the one the page is laid
    out around. Nothing is uploaded.
    """
    plan = json.dumps(scene.mission())
    return (
        "(async () => { const w = (ms) => new Promise((r) => setTimeout(r, ms)); "
        "for (let i = 0; i < 40 && !(window.Corvus && Corvus.mission "
        "&& document.querySelector('.mission-map-controls .mc-btn[data-act=\"fit\"]')); i++) "
        "await w(250); "
        "if (!(window.Corvus && Corvus.mission)) return 'no mission planner'; "
        f"Corvus.mission.setPlan({plan}); await w(600); "
        "const fit = document.querySelector('.mission-map-controls .mc-btn[data-act=\"fit\"]'); "
        "if (fit) fit.click(); await w(900); "
        # The fit leaves the easternmost point's name under the map's own
        # control rail. A small wheel step out gives it room; the zoom buttons
        # step a whole level, which halves the plan.
        "const canvas = document.querySelector('.mission-map canvas'); "
        "if (canvas) { const r = canvas.getBoundingClientRect(); "
        "canvas.dispatchEvent(new WheelEvent('wheel', { deltaY: 70, deltaMode: 0, "
        "clientX: r.left + r.width * 0.42, clientY: r.top + r.height * 0.5, "
        "bubbles: true, cancelable: true })); await w(900); } "
        "return 'plan: ' + Corvus.mission.getPlan().items.length + ' items'; })()"
    )


def ssh_js() -> str:
    """Connect both saved sessions: one in the SSH tab, one in a window.

    The tab's card is clicked, the way an operator connects; the second
    session is connected through the same endpoint the Settings page uses and
    handed to the terminal windows, which is the path a plugin's terminal
    takes.
    """
    at = json.dumps(SSH_WINDOW_AT)
    return (
        "(async () => { const w = (ms) => new Promise((r) => setTimeout(r, ms)); "
        "let btn = null; "
        "for (let i = 0; i < 40 && !btn; i++) { "
        f"btn = document.querySelector('.ssh-connect[data-name={json.dumps(SSH_TAB)}]'); "
        "if (!btn) await w(250); } "
        "if (!btn) return 'no connection card'; btn.click(); "
        "for (let i = 0; i < 60 && !document.querySelector('.ssh-terminal-card .xterm'); i++) "
        "await w(250); "
        "const conns = await fetch('/api/ssh/connections').then((r) => r.json()); "
        f"const other = (conns.connections || []).find((c) => c.name === {json.dumps(SSH_WINDOW)}); "
        "if (!other) return 'tab connected, no second connection'; "
        "const res = await fetch('/api/ssh/connect', { method: 'POST', "
        "headers: { 'Content-Type': 'application/json' }, "
        "body: JSON.stringify({ name: other.name }) }).then((r) => r.json()); "
        "if (!res.connected) return 'second connect failed: ' + (res.error || '?'); "
        "Corvus.termWindows.open({ name: other.name, title: other.name, host: other.host, "
        f"port: other.port, username: other.username }}, {{ at: {at} }}); "
        "await w(1500); return 'two terminals'; })()"
    )


def review_js(focus: str = "") -> str:
    """Open the newest downloaded log in Flight Review, and frame what matters.

    Without *focus* the review's own head is framed: the summary, the flight
    modes, the findings and the ground track over the imagery. With it, the
    plot of that title is brought to the top, with whatever follows it
    underneath: the plots are drawn as they come into view, so the step waits
    for the one it scrolled to.
    """
    head = (
        "(async () => { const w = (ms) => new Promise((r) => setTimeout(r, ms)); "
        "let pick = null; "
        "for (let i = 0; i < 60; i++) { "
        "pick = document.querySelector('.review-pick select'); "
        "if (pick && !pick.disabled && pick.options.length && pick.options[0].value) break; "
        "await w(250); } "
        "if (!pick || pick.disabled) return 'no downloaded log to review'; "
        # The picker already holds the newest log; it is a styled select, and
        # a click on it would open its list over the page.
        "const open = [...document.querySelectorAll('.review-pick button')]"
        ".find((b) => /^\\s*review\\s*$/i.test(b.textContent || '') && !b.disabled); "
        "if (!open) return 'no review button'; open.click(); "
        "for (let i = 0; i < 80 && !document.querySelector('.review-out .js-plotly-plot'); i++) "
        "await w(250); "
    )
    if focus:
        return head + (
            "const pv = document.getElementById('pageView'); "
            "const card = [...document.querySelectorAll('.review-out .page-card')].find((c) => "
            f"(c.textContent || '').trim().toLowerCase().startsWith({json.dumps(focus.lower())})); "
            f"if (!card || !pv) return 'no plot called ' + {json.dumps(focus)}; "
            "pv.scrollTop += card.getBoundingClientRect().top - pv.getBoundingClientRect().top - 16; "
            "for (let i = 0; i < 40 && !card.querySelector('.main-svg'); i++) await w(250); "
            f"await w(2000); return 'reviewing ' + pick.value + ' at ' + {json.dumps(focus)}; }})()"
        )
    return head + (
        # The ground track over the imagery: the same curve over the same field
        # the map pictures show. Then the review is scrolled up so its summary,
        # the modes and the track share the frame instead of the file picker.
        "const sat = [...document.querySelectorAll('.review-view-opt')]"
        ".find((b) => /satellite/i.test(b.textContent || '')); "
        "if (sat) sat.click(); "
        "const out = document.querySelector('.review-out'); "
        "if (out) out.scrollIntoView({ block: 'start' }); "
        "await w(600); "
        "const host = document.querySelector('.review-track-map'); "
        "const tm = host && host.trackMap; "
        "if (tm) await new Promise((done) => { const t = setTimeout(done, 8000); "
        "if (tm.loaded() && tm.areTilesLoaded()) { clearTimeout(t); done(); return; } "
        "tm.once('idle', () => { clearTimeout(t); done(); }); }); "
        "await w(1500); return 'reviewing ' + pick.value + (sat ? ' on satellite' : ''); })()"
    )


def hud_controls_js() -> str:
    """Show the instrument panel's own controls: lock, compact and collapse.

    The app shows them only while the pointer is over the panel, and a picture
    has no pointer, so the panel would read as one with no controls at all.
    A style on the page for the shot, the same kind of staging as taking the
    logo out; nothing in the app is changed.
    """
    return (
        "(() => { let s = document.getElementById('scene-hud-controls'); "
        "if (!s) { s = document.createElement('style'); s.id = 'scene-hud-controls'; "
        "s.textContent = '.flight-overlay .hud-actions { opacity: 1 !important; "
        "transition: none !important; }'; document.head.appendChild(s); } "
        "const n = document.querySelectorAll('.flight-overlay .hud-actions .icon-btn').length; "
        "return n ? n + ' panel controls shown' : 'no instrument panel'; })()"
    )


def hide_logo_js() -> str:
    """Take the operator's company logo out of the top bar for the picture.

    A sandboxed scene has no logo configured, so this only matters for a
    Corvus that is already running (``--no-backend``), where the logo is the
    operator's own setting. It is removed from the page, not from the
    configuration: the picture must not carry it, and the setting is not the
    recipe's to change.
    """
    return (
        "(() => { const logos = [...document.querySelectorAll('.tb-company-logo')]; "
        "const shown = logos.filter((el) => !el.hidden && el.getAttribute('src')).length; "
        "logos.forEach((el) => el.remove()); "
        "return shown ? 'company logo removed' : 'no company logo'; })()"
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
        "(async () => { if (!window.Corvus || !Corvus.map || !Corvus.map.isReady()) "
        "return 'map not ready'; Corvus.map.setFollow(false); "
        "const m = Corvus.map.getMap(); "
        f"m.fitBounds([[{west:.6f}, {south:.6f}], [{east:.6f}, {north:.6f}]], "
        "{ padding: 90, duration: 0 }); "
        # The imagery at the new zoom arrives tile by tile; a picture taken
        # before the map goes idle has grey squares in it.
        "await new Promise((done) => { const t = setTimeout(done, 8000); "
        "m.once('idle', () => { clearTimeout(t); done(); }); m.triggerRepaint(); }); "
        "return 'framed at zoom ' + m.getZoom().toFixed(2); })()"
    )


def param_filter_js(mode: str) -> str:
    """Press one of the parameter editor's All / Modified / Unsaved filters.

    The Modified filter stays disabled until the firmware's metadata (its
    defaults) has arrived, so this waits for the button to come alive rather
    than clicking a dead one.
    """
    return (
        "(async () => { const w = (ms) => new Promise((r) => setTimeout(r, ms)); "
        f"const sel = '.params-filter-opt[data-mode={json.dumps(mode)}]'; "
        "for (let t = 0; t < 15000; t += 200) { const b = document.querySelector(sel); "
        "if (b && !b.disabled) { b.click(); await w(400); "
        "const n = document.querySelector('.params-count'); "
        f"return {json.dumps(mode)} + ' filter on' + (n ? ', ' + n.textContent : ''); }} "
        "await w(200); } return 'filter not available'; })()"
    )


def calibration_hold_js() -> str:
    """Wait for the calibration figure to rest, solid, at the side to turn to.

    The large figure demonstrates the turn in a loop: faint where the aircraft
    is, then the turn, then a hold at the target drawn solid. A picture taken
    at a random moment is faint half the time, which reads as "nothing asked
    for". So this waits out any hold already under way and returns as the next
    one begins, leaving the shot the whole hold to land in.
    """
    return (
        "(async () => { const w = (ms) => new Promise((r) => setTimeout(r, ms)); "
        "const body = () => document.querySelector("
        "'svg.calib-figure:not(.calib-figure-compact) .calib-figure-body'); "
        "const solid = () => { const b = body(); const o = b && b.getAttribute('opacity'); "
        "return !o || parseFloat(o) >= 0.999; }; "
        "if (!body()) return 'no calibration figure'; let t = 0; "
        "while (solid() && t < 4000) { await w(40); t += 40; } "
        "while (!solid() && t < 8000) { await w(40); t += 40; } "
        "return solid() ? 'figure solid at its target' : 'figure never settled'; })()"
    )


def three_d_js(scene: Scene) -> str:
    """Put the framed map into the scene's 3D mode, turned to its bearing.

    Through ``set3DMode``, the door the rail's 3D button uses, so the picture
    is the 3D an operator gets. The bearing and zoom wait for the tilt to
    land: set during the tilt's ease, the ease would take them straight back.
    """
    return (
        "(async () => { if (!window.Corvus || !Corvus.map || !Corvus.map.isReady()) "
        "return 'map not ready'; const m = Corvus.map.getMap(); "
        "const idle = (ms) => new Promise((done) => { const t = setTimeout(done, ms); "
        "m.once('idle', () => { clearTimeout(t); done(); }); m.triggerRepaint(); }); "
        f"const mode = Corvus.map.set3DMode({json.dumps(scene.map_3d)}); "
        "await new Promise((r) => setTimeout(r, 2000)); "
        f"m.jumpTo({{ bearing: {scene.map_bearing:.1f}, zoom: m.getZoom() + {scene.map_zoom:.2f} }}); "
        "await idle(15000); "
        "return mode + ' at pitch ' + m.getPitch().toFixed(0) + ', bearing ' + "
        "m.getBearing().toFixed(0) + (Corvus.map.hasTerrain() ? ', terrain' : ', no terrain'); })()"
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
    if scene.mission_plan and scene.page == "mission":
        out.append(Step("evaluate",
                        "Put the sortie on the mission planner as a plan, and "
                        "frame it with the page's own fit button.",
                        mission_js(scene)))
    if scene.ssh and scene.tab == "ssh":
        out.append(Step("evaluate",
                        f"Connect {SSH_TAB!r} in the SSH tab and open "
                        f"{SSH_WINDOW!r} in a terminal window; both type a few "
                        "commands on login.",
                        ssh_js()))
    if scene.page == "analysis" and scene.view == "review":
        out.append(Step("evaluate",
                        "Open the newest downloaded log in Flight Review and "
                        "wait for its plots.",
                        review_js(scene.review_focus)))
    out.append(Step("wait", f"Let the scene build: {scene.settle:.0f} s. "
                            + (scene.notes or ""), f"{scene.settle:.0f}"))
    if scene.page == "home":
        out.append(Step("evaluate",
                        "Frame the map on the flight: following off, fitted to "
                        "the track and the home point, and wait for the imagery.",
                        frame_js(scene)))
        if scene.map_3d != "off":
            out.append(Step("evaluate",
                            f"Switch the map to 3D ({scene.map_3d}) and turn it "
                            f"to a bearing of {scene.map_bearing:.0f} degrees.",
                            three_d_js(scene)))
        if scene.hud_controls:
            out.append(Step("evaluate",
                            "Show the instrument panel's lock, compact and "
                            "collapse controls, which the app shows only under "
                            "the pointer.",
                            hud_controls_js()))
    if scene.view == "parameters" and scene.param_filter != "all":
        out.append(Step("evaluate",
                        f"Press the editor's {scene.param_filter} filter once the "
                        "firmware defaults have arrived.",
                        param_filter_js(scene.param_filter)))
    if scene.calibration:
        out.append(Step("evaluate",
                        "Wait for the demonstrated turn to rest, solid, at the "
                        "side the aircraft is asked for.",
                        calibration_hold_js()))
    out.append(Step("evaluate",
                    "Take any company logo out of the top bar. The pictures "
                    "never carry one.",
                    hide_logo_js()))
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
