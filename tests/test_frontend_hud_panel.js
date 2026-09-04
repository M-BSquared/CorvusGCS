"use strict";

/**
 * Frontend tests for the movable flight HUD (Corvus.hudPanel).
 *
 * Plain Node-runnable assertions following tests/test_frontend_setup.js: stub
 * the globals, require the source, drive the panel, assert on the DOM and the
 * persisted state.
 *
 * What matters here is that the panel never loses the operator's instruments
 * and never becomes unreachable:
 *  - init() RE-PARENTS the existing instrument nodes rather than rebuilding
 *    them, which is what lets it be layered on top of instruments.js;
 *  - drag/pin/compact/collapse round-trip through localStorage;
 *  - a pinned panel refuses to move;
 *  - a drag toward infinity is clamped so part of the panel stays on screen.
 *
 * Run:
 *   node tests/test_frontend_hud_panel.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
window.matchMedia = (q) => ({ matches: false, media: q, addEventListener() {}, removeEventListener() {} });
// The panel re-clamps its position on window resize, so the listener pair has
// to exist. Handlers are captured so the resize path can be driven directly.
const windowListeners = {};
window.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
window.removeEventListener = () => {};

// localStorage stub with a peek hatch for the persistence assertions.
const store = new Map();
global.localStorage = {
  getItem: (k) => (store.has(k) ? store.get(k) : null),
  setItem: (k, v) => store.set(k, String(v)),
  removeItem: (k) => store.delete(k),
};

// ---------------------------------------------------------------------------
// DOM stub. Elements carry a settable rect so the clamp maths — the only part
// of this module that reads layout — can be exercised without a layout engine.
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {}, style: {},
    hidden: false, disabled: false, id: "", title: "",
    _attrs: {}, _listeners: {}, _isEl: true, _rect: { left: 0, top: 0, width: 320, height: 380 },
    parentElement: null, parentNode: null,
  };
  let _html = "";
  Object.defineProperty(e, "innerHTML", {
    get() { return _html; },
    set(v) { _html = String(v); if (_html === "") e.children.length = 0; },
  });
  Object.defineProperty(e, "firstChild", { get() { return e.children[0] || null; } });
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) { const has = e.classList.contains(c); const next = force === undefined ? !has : !!force; if (next) e.classList.add(c); else e.classList.remove(c); return next; },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  // appendChild MOVES a node, as the real DOM does: hudPanel.init() re-parents
  // the instruments with `while (panel.firstChild) body.appendChild(...)`, and
  // a stub that merely copies makes that loop run forever.
  e.appendChild = (c) => {
    if (c.parentNode && c.parentNode !== e) c.parentNode.removeChild(c);
    c.parentNode = e; c.parentElement = e;
    e.children.push(c);
    return c;
  };
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; c.parentElement = null; return c; };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.removeEventListener = () => {};
  e.setPointerCapture = () => {};
  e.releasePointerCapture = () => {};
  e.getBoundingClientRect = () => e._rect;
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  e.closest = (sel) => {
    const cls = sel.replace(/^\./, "");
    let n = e;
    while (n) { if (n.className && n.className.split(/\s+/).includes(cls)) return n; n = n.parentElement; }
    return null;
  };
  return e;
}
function querySel(children, sel) {
  const out = [];
  const wantTag = sel && sel[0] !== ".";
  const classes = sel ? sel.split(".").filter(Boolean) : [];
  (function walk(list) {
    for (const c of list) {
      if (!c || !c._isEl) continue;
      const ok = wantTag ? c.tagName === sel.toUpperCase() : classes.every((cl) => c.className.split(/\s+/).includes(cl));
      if (ok) out.push(c);
      if (c.children) walk(c.children);
    }
  })(children);
  return out;
}
global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
};

require("./../src/js/ui.js");
require("./../src/js/hud-panel.js");

const KEY = "corvus.hud";

/** Build a fresh map host + overlay carrying two stand-in instrument nodes. */
function mount() {
  store.clear();
  const host = makeEl("div");
  host.className = "map-view";
  host._rect = { left: 70, top: 60, width: 1000, height: 700 };
  const panel = makeEl("div");
  panel.className = "flight-overlay glass";
  panel._rect = { left: 700, top: 380, width: 320, height: 380 };
  const instruments = makeEl("div"); instruments.className = "instruments";
  const telemetry = makeEl("div"); telemetry.className = "flight-telemetry";
  panel.appendChild(instruments);
  panel.appendChild(telemetry);
  host.appendChild(panel);
  return { host, panel, instruments, telemetry };
}

function fire(el, type, ev) {
  (el._listeners[type] || []).forEach((cb) => cb(Object.assign({
    preventDefault() {}, stopPropagation() {}, target: el, button: 0, pointerId: 1,
  }, ev)));
}
function head(panel) { return panel.querySelector(".hud-head"); }
function actionBtn(panel, label) {
  return panel.querySelectorAll(".icon-btn").find((b) => b.getAttribute("aria-label") === label);
}
function persisted() { return JSON.parse(store.get(KEY) || "{}"); }

// ===========================================================================

function testInitReparentsInstrumentsInsteadOfRebuilding() {
  const { panel, instruments, telemetry } = mount();
  Corvus.hudPanel.init(panel);

  const body = panel.querySelector(".hud-body");
  assert.ok(body, "collapsible body created");
  // The SAME nodes, moved — instruments.js holds references to them and keeps
  // rendering into them after this module runs.
  assert.equal(body.children[0], instruments, "instruments node re-parented, not recreated");
  assert.equal(body.children[1], telemetry, "telemetry node re-parented, not recreated");
  assert.ok(head(panel), "title bar created");
  assert.equal(panel.querySelector(".hud-title").textContent, "FLIGHT");
}

function testHeaderExposesThreeControls() {
  const { panel } = mount();
  Corvus.hudPanel.init(panel);
  ["Lock position", "Compact size", "Collapse"].forEach((label) => {
    assert.ok(actionBtn(panel, label), `header has a "${label}" control`);
  });
}

function testCollapseHidesTheBodyAndRoundTrips() {
  const { panel } = mount();
  Corvus.hudPanel.init(panel);
  const body = panel.querySelector(".hud-body");

  fire(actionBtn(panel, "Collapse"), "click");
  assert.equal(body.hidden, true, "body hidden when collapsed");
  assert.ok(panel.classList.contains("is-collapsed"));
  assert.equal(persisted().collapsed, true, "collapsed state persisted");

  fire(actionBtn(panel, "Expand"), "click");
  assert.equal(body.hidden, false, "body shown again");
  assert.equal(persisted().collapsed, false);
}

function testCompactRoundTrips() {
  const { panel } = mount();
  Corvus.hudPanel.init(panel);
  fire(actionBtn(panel, "Compact size"), "click");
  assert.ok(panel.classList.contains("is-compact"));
  assert.equal(persisted().compact, true);
  fire(actionBtn(panel, "Full size"), "click");
  assert.ok(!panel.classList.contains("is-compact"));
  assert.equal(persisted().compact, false);
}

function testDragMovesThePanelAndPersistsThePosition() {
  const { panel } = mount();
  Corvus.hudPanel.init(panel);
  const h = head(panel);

  // Grab at (740, 390) — 40px into a panel whose top-left is (700, 380).
  fire(h, "pointerdown", { clientX: 740, clientY: 390 });
  fire(h, "pointermove", { clientX: 540, clientY: 290 });
  fire(h, "pointerup", { clientX: 540, clientY: 290 });

  // Host origin is (70, 60), so the panel's host-relative position is
  // (540-40-70, 290-10-60) = (430, 220).
  assert.equal(panel.style.left, "430px");
  assert.equal(panel.style.top, "220px");
  assert.ok(panel.classList.contains("is-placed"), "switches to left/top anchoring once moved");
  assert.deepEqual([persisted().x, persisted().y], [430, 220], "position persisted");
}

function testPinnedPanelRefusesToMove() {
  const { panel } = mount();
  Corvus.hudPanel.init(panel);
  fire(actionBtn(panel, "Lock position"), "click");
  assert.ok(panel.classList.contains("is-pinned"));
  assert.equal(persisted().pinned, true);

  const h = head(panel);
  fire(h, "pointerdown", { clientX: 740, clientY: 390 });
  fire(h, "pointermove", { clientX: 200, clientY: 200 });
  fire(h, "pointerup", { clientX: 200, clientY: 200 });
  assert.equal(panel.style.left, "", "a pinned panel does not move");
  assert.ok(!panel.classList.contains("is-placed"));
}

function testHeaderButtonsDoNotStartADrag() {
  const { panel } = mount();
  Corvus.hudPanel.init(panel);
  const h = head(panel);
  // A pointerdown that originates on a control must not begin dragging, or
  // every click on pin/compact/collapse would nudge the panel.
  fire(h, "pointerdown", { clientX: 740, clientY: 390, target: actionBtn(panel, "Collapse") });
  fire(h, "pointermove", { clientX: 500, clientY: 200 });
  assert.equal(panel.style.left, "", "no drag started from a header button");
}

function testDragIsClampedSoThePanelStaysReachable() {
  const { panel } = mount();
  Corvus.hudPanel.init(panel);
  const h = head(panel);

  // Drag far past the top-left corner of the map.
  fire(h, "pointerdown", { clientX: 740, clientY: 390 });
  fire(h, "pointermove", { clientX: -5000, clientY: -5000 });
  fire(h, "pointerup", { clientX: -5000, clientY: -5000 });

  const x = parseInt(panel.style.left, 10);
  const y = parseInt(panel.style.top, 10);
  // 64px of a 320px-wide panel must remain inside the host: x >= 64 - 320.
  assert.equal(x, 64 - 320, "left edge clamped so a strip stays on screen");
  assert.equal(y, 0, "never dragged above the top of the map");

  // ...and equally far the other way.
  fire(h, "pointerdown", { clientX: -5000 + 40, clientY: 10 });
  fire(h, "pointermove", { clientX: 9000, clientY: 9000 });
  fire(h, "pointerup", { clientX: 9000, clientY: 9000 });
  assert.ok(parseInt(panel.style.left, 10) <= 1000 - 64, "right edge clamped inside the host");
  assert.ok(parseInt(panel.style.top, 10) <= 700 - 64, "bottom edge clamped inside the host");
}

function testDoubleClickReturnsThePanelToItsDefaultCorner() {
  const { panel } = mount();
  Corvus.hudPanel.init(panel);
  const h = head(panel);
  fire(h, "pointerdown", { clientX: 740, clientY: 390 });
  fire(h, "pointermove", { clientX: 400, clientY: 200 });
  fire(h, "pointerup", { clientX: 400, clientY: 200 });
  assert.ok(panel.classList.contains("is-placed"));

  fire(h, "dblclick", {});
  assert.equal(panel.style.left, "", "position cleared");
  assert.ok(!panel.classList.contains("is-placed"), "back to the CSS default corner");
  assert.equal(persisted().x, null, "cleared position persisted");
}

function testStateIsRestoredOnTheNextLaunch() {
  const first = mount();
  Corvus.hudPanel.init(first.panel);
  fire(actionBtn(first.panel, "Compact size"), "click");
  fire(actionBtn(first.panel, "Lock position"), "click");
  fire(actionBtn(first.panel, "Collapse"), "click");

  // A second mount reads the same localStorage — the next app launch.
  const host = makeEl("div");
  host._rect = { left: 70, top: 60, width: 1000, height: 700 };
  const panel = makeEl("div");
  panel._rect = { left: 700, top: 380, width: 320, height: 380 };
  panel.appendChild(makeEl("div"));
  host.appendChild(panel);
  Corvus.hudPanel.init(panel);

  assert.ok(panel.classList.contains("is-compact"), "compact restored");
  assert.ok(panel.classList.contains("is-pinned"), "pinned restored");
  assert.ok(panel.classList.contains("is-collapsed"), "collapsed restored");
  assert.equal(panel.querySelector(".hud-body").hidden, true);
}

function testCorruptStoredStateFallsBackToDefaults() {
  store.clear();
  store.set(KEY, "{ not json");
  const { panel } = mount();
  store.set(KEY, "{ not json");   // mount() clears the store
  Corvus.hudPanel.init(panel);
  const s = Corvus.hudPanel._state();
  assert.deepEqual(
    [s.pinned, s.compact, s.collapsed, s.x, s.y],
    [false, false, false, null, null],
    "unreadable storage leaves the panel at its defaults instead of throwing",
  );
}

function testShrinkingTheWindowPullsThePanelBackIntoView() {
  const { host, panel } = mount();
  Corvus.hudPanel.init(panel);
  const h = head(panel);
  fire(h, "pointerdown", { clientX: 740, clientY: 390 });
  fire(h, "pointermove", { clientX: 1000, clientY: 700 });
  fire(h, "pointerup", { clientX: 1000, clientY: 700 });
  const before = parseInt(panel.style.left, 10);

  // The map area shrinks (right panel opened, window resized). The panel must
  // be pulled back rather than stranded outside the visible map.
  host._rect = { left: 70, top: 60, width: 400, height: 300 };
  (windowListeners.resize || []).forEach((cb) => cb());

  const after = parseInt(panel.style.left, 10);
  assert.ok(after < before, "panel pulled back when the map shrank");
  assert.ok(after <= 400 - 64, "still inside the smaller map");
}

const tests = [
  testInitReparentsInstrumentsInsteadOfRebuilding,
  testHeaderExposesThreeControls,
  testCollapseHidesTheBodyAndRoundTrips,
  testCompactRoundTrips,
  testDragMovesThePanelAndPersistsThePosition,
  testPinnedPanelRefusesToMove,
  testHeaderButtonsDoNotStartADrag,
  testDragIsClampedSoThePanelStaysReachable,
  testDoubleClickReturnsThePanelToItsDefaultCorner,
  testStateIsRestoredOnTheNextLaunch,
  testCorruptStoredStateFallsBackToDefaults,
  testShrinkingTheWindowPullsThePanelBackIntoView,
];

let failed = 0;
for (const t of tests) {
  try { t(); console.log("ok   - " + t.name); }
  catch (e) { failed++; console.error("FAIL - " + t.name + "\n      " + (e && e.message)); }
}
if (failed) { console.error(`\n${failed}/${tests.length} HUD panel test(s) FAILED`); process.exit(1); }
console.log(`\nAll ${tests.length} HUD panel tests passed.`);
