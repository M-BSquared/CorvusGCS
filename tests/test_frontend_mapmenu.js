"use strict";

/**
 * Frontend tests for the map context menu (click a position -> act on it).
 *
 * Plain Node-runnable assertions (no browser, no test runner), following the
 * same pattern as tests/test_frontend_scale.js: stub the browser globals the
 * module touches, require the source, and assert on the rendered DOM and the
 * registered actions.
 *
 * Unlike tests/test_frontend_map.js — which never calls init() and so needs no
 * maplibregl — these tests DO call it, because the whole point of the menu is
 * the wiring init() puts in place: which map events open it, and which
 * container it is positioned in. maplibregl is faked just far enough for that.
 *
 * Run:
 *   node tests/test_frontend_mapmenu.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
global.Event = class Event { constructor(type) { this.type = type; } };
global.KeyboardEvent = class KeyboardEvent {
  constructor(type, options = {}) { this.type = type; this.key = options.key; }
};

window.requestAnimationFrame = (cb) => setTimeout(() => cb(Date.now()), 16);
window.cancelAnimationFrame = (id) => clearTimeout(id);
window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;
window.addEventListener = () => {};
window.removeEventListener = () => {};
window.dispatchEvent = () => true;
window.matchMedia = (query) => ({
  matches: false, media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});
if (typeof global.performance === "undefined") global.performance = { now: () => Date.now() };
global.fetch = () => Promise.reject(new Error("offline"));
global.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };

// ---------------------------------------------------------------------------
// DOM stub. The menu is built with createElement + appendChild and measured
// with offsetWidth/clientWidth, so the stub tracks children and carries a
// settable box for the positioning maths.
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {},
    type: "", hidden: false, disabled: false, value: "", id: "", title: "",
    tabIndex: 0, _attrs: {}, _listeners: {}, _isEl: true,
    parentNode: null, parentElement: null,
    // Box the positioning maths reads. Menus get a real size below; every
    // other element is 0x0, which nothing under test measures.
    _box: { width: 0, height: 0 },
    focused: false,
  };
  e.style = {
    _props: {},
    setProperty(k, v) { e.style._props[k] = String(v); },
    getPropertyValue(k) { return e.style._props[k] || ""; },
  };
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) {
      const has = e.classList.contains(c);
      const next = force === undefined ? !has : !!force;
      if (next) e.classList.add(c); else e.classList.remove(c);
      return next;
    },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => {
    if (c.parentNode && c.parentNode !== e) c.parentNode.removeChild(c);
    c.parentNode = e; c.parentElement = e;
    e.children.push(c);
    return c;
  };
  e.append = (...nodes) => { nodes.forEach((n) => e.appendChild(n)); };
  e.removeChild = (c) => {
    const i = e.children.indexOf(c);
    if (i >= 0) e.children.splice(i, 1);
    c.parentNode = null; c.parentElement = null;
    return c;
  };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.removeEventListener = () => {};
  e.focus = () => { e.focused = true; };
  e.fire = (type, ev) => (e._listeners[type] || []).forEach((cb) => cb(Object.assign({
    preventDefault() {}, stopPropagation() {}, target: e,
  }, ev)));
  e.closest = (sel) => {
    const cls = sel.replace(/^\./, "");
    let n = e;
    while (n) { if (n.className && n.className.split(/\s+/).includes(cls)) return n; n = n.parentElement; }
    return null;
  };
  e.querySelectorAll = (sel) => query(e.children, sel);
  e.querySelector = (sel) => query(e.children, sel)[0] || null;
  Object.defineProperty(e, "offsetWidth", { get: () => e._box.width });
  Object.defineProperty(e, "offsetHeight", { get: () => e._box.height });
  Object.defineProperty(e, "clientWidth", { get: () => e._box.width });
  Object.defineProperty(e, "clientHeight", { get: () => e._box.height });
  Object.defineProperty(e, "firstChild", { get: () => e.children[0] || null });
  return e;
}

/** Supports ".cls", ".a.b", "tag" and the one attribute form the menu uses. */
function query(children, sel) {
  const out = [];
  const notDisabled = sel.includes(":not([disabled])");
  const attr = sel.match(/\[data-action="([^"]+)"\]/);
  const base = sel.replace(/:not\(\[disabled\]\)/, "").replace(/\[data-action="[^"]+"\]/, "");
  const wantTag = base && base[0] !== ".";
  const classes = base.split(".").filter(Boolean);
  (function walk(list) {
    list.forEach((c) => {
      if (!c || !c._isEl) return;
      const own = c.className.split(/\s+/).filter(Boolean);
      let ok = wantTag ? c.tagName === base.toUpperCase() : classes.every((cl) => own.includes(cl));
      if (ok && notDisabled && c.disabled) ok = false;
      if (ok && attr && c.dataset.action !== attr[1]) ok = false;
      if (ok) out.push(c);
      walk(c.children);
    });
  })(children);
  return out;
}

const documentListeners = {};
global.document = {
  createElement: makeEl,
  createElementNS: makeEl,
  getElementById: () => makeEl("div"),
  querySelectorAll: () => [],
  addEventListener: (t, cb) => { (documentListeners[t] = documentListeners[t] || []).push(cb); },
  removeEventListener: (t, cb) => {
    const list = documentListeners[t] || [];
    const i = list.indexOf(cb);
    if (i >= 0) list.splice(i, 1);
  },
};
function fireDocument(type, ev) {
  (documentListeners[type] || []).slice().forEach((cb) => cb(Object.assign({
    preventDefault() {}, stopPropagation() {},
  }, ev)));
}

// ---------------------------------------------------------------------------
// maplibregl fake — only what init() and the menu touch. `project` is a plain
// linear transform so the positioning maths has something predictable to work
// against; panning is modelled by shifting its origin.
// ---------------------------------------------------------------------------
const mapEvents = {};
let projectOrigin = { x: 0, y: 0 };
const fakeMap = {
  on: (type, cb) => { (mapEvents[type] = mapEvents[type] || []).push(cb); },
  off: (type, cb) => {
    const list = mapEvents[type] || [];
    const i = list.indexOf(cb);
    if (i >= 0) list.splice(i, 1);
  },
  addControl() {}, addLayer() {}, addSource() {}, getSource: () => null,
  getLayer: () => null, setPaintProperty() {}, getCanvas: () => makeEl("canvas"),
  getCanvasContainer: () => makeEl("div"),
  getPitch: () => 0, easeTo() {}, zoomIn() {}, zoomOut() {}, resize() {},
  setPixelRatio() {},
  // 100 000 px per degree, offset by the pan. Rounded because the point of
  // these fixtures is a readable pixel, not float noise from the fake.
  project: (lngLat) => ({
    x: Math.round((lngLat[0] - 11) * 100000) + projectOrigin.x,
    y: Math.round((48.5 - lngLat[1]) * 100000) + projectOrigin.y,
  }),
};
global.maplibregl = {
  Map: function () { return fakeMap; },
  AttributionControl: function () { return {}; },
  Marker: function () {
    return { setLngLat() { return this; }, addTo() { return this; }, remove() {},
             setRotation() { return this; }, getElement: () => makeEl("div") };
  },
};

function fireMap(type, payload) {
  (mapEvents[type] || []).slice().forEach((cb) => cb(payload));
}

// init() hydrates the tile catalogue through Corvus.telemetry and reads the
// saved base layer from /api/config. Both fail closed here — the map is
// offline-safe by design — which is exactly the state these tests want.
Corvus.telemetry = {
  requestJson: () => Promise.reject(new Error("offline")),
  getState: () => null,
  subscribe() {},
};

require("../src/js/ui.js");
require("../src/js/map.js");

const map = Corvus.map;

// ---------------------------------------------------------------------------
// Wire up a map, exactly as index.html + app.js do.
// ---------------------------------------------------------------------------
const mapEl = makeEl("div");
mapEl.id = "map";
mapEl._box = { width: 1000, height: 700 };
map.init(mapEl, makeEl("div"), makeEl("div"));
// Planning mode is refused until the style has loaded (`started`), so the
// fake has to reach that state or the planning-vs-menu tests would pass
// vacuously. Everything the load handler builds is a no-op against the fakes.
fireMap("load");
assert.equal(map.isReady(), true, "the fake map must reach the loaded state");

/** The live menu element, or null. */
function menuEl() { return mapEl.querySelector(".map-context-menu"); }
function pinEl() { return mapEl.querySelector(".map-context-pin"); }
function rows() { return mapEl.querySelectorAll(".map-context-item"); }
function rowText(btn) {
  return {
    label: btn.querySelector(".map-context-label").textContent,
    note: (btn.querySelector(".map-context-note") || {}).textContent,
    disabled: btn.disabled,
  };
}

/** Register a recording action set and return the call log. */
function withActions(overrides) {
  const calls = [];
  map.setContextActions([Object.assign({
    id: "goto",
    label: "Fly to this point",
    icon: "navigation",
    enabled: () => true,
    note: () => "10 m AGL",
    run: (p) => calls.push(p),
  }, overrides || {})]);
  return calls;
}

/** Give the open menu a measurable size, then re-run the positioning. */
function sizeMenu(w, h) {
  const m = menuEl();
  m._box = { width: w, height: h };
  fireMap("move");
  return m;
}

function reset() {
  map.closeContextMenu();
  map.setWaypointMode(false);
  projectOrigin = { x: 0, y: 0 };
}

// ---------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------
function testMapClickOpensTheMenuAtTheClickedPoint() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.003, lat: 48.497 } });
  assert.ok(menuEl(), "a left-click on the map opens the menu");
  assert.ok(pinEl(), "and pins the point it refers to");
  assert.deepEqual(map._contextPoint(), { lng: 11.003, lat: 48.497 });
}

function testRightClickOpensTheSameMenu() {
  reset();
  withActions();
  fireMap("contextmenu", { lngLat: { lng: 11.004, lat: 48.496 }, preventDefault() {} });
  assert.ok(menuEl(), "right-click is the other gesture operators reach for");
  assert.deepEqual(map._contextPoint(), { lng: 11.004, lat: 48.496 });
}

function testMenuShowsTheCoordinatesItActsOn() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.6405, lat: 48.0861 } });
  assert.equal(mapEl.querySelector(".map-context-coords").textContent, "48.086100, 11.640500");
}

function testNoActionsMeansNoMenu() {
  reset();
  map.setContextActions([]);
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  assert.equal(menuEl(), null, "a map with nothing to offer opens nothing");
}

function testPlanningModeOwnsTheClick() {
  reset();
  withActions();
  map.setWaypointMode(true);
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  assert.equal(menuEl(), null, "in planning mode a click places a waypoint, not a menu");
  fireMap("contextmenu", { lngLat: { lng: 11.0, lat: 48.5 }, preventDefault() {} });
  assert.equal(menuEl(), null, "and right-click removes the last one");
  map.setWaypointMode(false);
}

function testEnteringPlanningClosesAnOpenMenu() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  assert.ok(menuEl());
  map.setWaypointMode(true);
  assert.equal(menuEl(), null, "the menu would be acting on a point planning has taken over");
  assert.equal(pinEl(), null, "and its pin goes with it");
  map.setWaypointMode(false);
}

function testReopeningReplacesTheOpenMenu() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  fireMap("click", { lngLat: { lng: 11.1, lat: 48.4 } });
  assert.equal(rows().length, 1, "one menu at a time, not two stacked");
  assert.equal(mapEl.querySelectorAll(".map-context-pin").length, 1);
  assert.deepEqual(map._contextPoint(), { lng: 11.1, lat: 48.4 });
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------
function testRowsRenderLabelAndNote() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  assert.deepEqual(rowText(rows()[0]),
    { label: "Fly to this point", note: "10 m AGL", disabled: false });
}

function testEnabledIsReEvaluatedOnEveryOpen() {
  reset();
  let armed = false;
  withActions({ enabled: () => armed, note: (_p, ok) => (ok ? "10 m AGL" : "Arm the vehicle first") });

  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  assert.deepEqual(rowText(rows()[0]),
    { label: "Fly to this point", note: "Arm the vehicle first", disabled: true },
    "a row that cannot be used says why, rather than going quietly inert");

  armed = true;
  map.closeContextMenu();
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  assert.deepEqual(rowText(rows()[0]),
    { label: "Fly to this point", note: "10 m AGL", disabled: false },
    "the same row reflects live state the next time the menu opens");
}

function testChoosingARowRunsItWithThePointAndCloses() {
  reset();
  const calls = withActions();
  fireMap("click", { lngLat: { lng: 11.6405, lat: 48.0861 } });
  rows()[0].fire("click");
  assert.deepEqual(calls, [{ lng: 11.6405, lat: 48.0861 }],
    "the action receives the point the menu was opened on");
  assert.equal(menuEl(), null, "and the menu closes rather than inviting a second click");
  assert.equal(pinEl(), null);
}

function testDisabledRowDoesNothing() {
  reset();
  const calls = withActions({ enabled: () => false });
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  rows()[0].fire("click");
  assert.deepEqual(calls, [], "no command leaves the client from a disabled row");
  assert.ok(menuEl(), "and the menu stays put");
}

function testAThrowingActionDoesNotBreakTheMenu() {
  reset();
  map.setContextActions([
    { id: "boom", label: "Boom", run: () => { throw new Error("nope"); } },
  ]);
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  assert.doesNotThrow(() => rows()[0].fire("click"));
  assert.equal(menuEl(), null, "it still closed — the click was a real choice");
}

function testFirstUsableRowTakesFocus() {
  reset();
  map.setContextActions([
    { id: "a", label: "A", enabled: () => false, run() {} },
    { id: "b", label: "B", enabled: () => true, run() {} },
  ]);
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  const [a, b] = rows();
  assert.equal(a.focused, false);
  assert.equal(b.focused, true, "the menu is keyboard-operable the moment it opens");
}

// ---------------------------------------------------------------------------
// Closing
// ---------------------------------------------------------------------------
function testEscapeCloses() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  fireDocument("keydown", { key: "Escape" });
  assert.equal(menuEl(), null);
}

function testAnUnrelatedKeyDoesNotClose() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  fireDocument("keydown", { key: "a" });
  assert.ok(menuEl());
}

function testPointerDownOutsideCloses() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  fireDocument("pointerdown", { target: makeEl("div") });
  assert.equal(menuEl(), null);
}

function testPointerDownInsideTheMenuDoesNotClose() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  fireDocument("pointerdown", { target: rows()[0] });
  assert.ok(menuEl(), "clicking a row must not dismiss the menu before the row runs");
}

function testClosingUnhooksTheDocumentListeners() {
  reset();
  withActions();
  const before = (documentListeners.keydown || []).length;
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  assert.equal((documentListeners.keydown || []).length, before + 1);
  map.closeContextMenu();
  assert.equal((documentListeners.keydown || []).length, before,
    "no listener survives a closed menu");
  assert.equal((documentListeners.pointerdown || []).length, 0);
}

function testCloseIsIdempotent() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  map.closeContextMenu();
  assert.doesNotThrow(() => map.closeContextMenu());
  assert.equal(map._contextPoint(), null);
}

function testClearingTheActionsClosesAnOpenMenu() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.0, lat: 48.5 } });
  map.setContextActions([]);
  assert.equal(menuEl(), null);
}

// ---------------------------------------------------------------------------
// Positioning — the menu is anchored to a ground point, not to a pixel
// ---------------------------------------------------------------------------
function testMenuSitsBelowRightOfThePinWithRoomToSpare() {
  reset();
  withActions();
  // project() puts this at (300, 200) in the 1000x700 container.
  fireMap("click", { lngLat: { lng: 11.003, lat: 48.498 } });
  const m = sizeMenu(186, 120);
  assert.equal(pinEl().style.left, "300px");
  assert.equal(pinEl().style.top, "200px");
  assert.equal(m.style.left, "312px", "12px clear of the pin");
  assert.equal(m.style.top, "212px");
  assert.equal(m.style.transformOrigin, "left top", "it grows out of the pin");
}

function testMenuFlipsBackInsideNearTheBottomRightCorner() {
  reset();
  withActions();
  // (950, 650) — a menu placed down-right would run off both edges.
  fireMap("click", { lngLat: { lng: 11.0095, lat: 48.4935 } });
  const m = sizeMenu(186, 120);
  assert.equal(m.style.left, `${950 - 12 - 186}px`, "flipped to the left of the pin");
  assert.equal(m.style.top, `${650 - 12 - 120}px`, "and above it");
  assert.equal(m.style.transformOrigin, "right bottom");
}

function testMenuIsClampedToTheMapRatherThanFlippedWhenItBarelyFits() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.0095, lat: 48.4935 } });   // (950, 650)
  // Too wide to sit clear of the pin on either side, but it does fit the map:
  // it slides back inside on the preferred (right/down) side instead of
  // flipping to a side it would not fit on either.
  const m = sizeMenu(980, 690);
  assert.equal(m.style.left, `${1000 - 980 - 4}px`);
  assert.equal(m.style.top, `${700 - 690 - 4}px`);
}

function testMenuLargerThanTheMapIsPinnedInsideNotPushedOff() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.0095, lat: 48.4935 } });   // (950, 650)
  // A tiny window, or a large interface scale: there is no position that fits.
  // The top-left corner has to win, or the menu's own rows scroll off screen.
  const m = sizeMenu(1200, 900);
  assert.equal(m.style.left, "4px", "never off the left edge");
  assert.equal(m.style.top, "4px", "never off the top edge");
}

function testMenuTracksItsGroundPointWhenTheMapPans() {
  reset();
  withActions();
  fireMap("click", { lngLat: { lng: 11.003, lat: 48.498 } });
  sizeMenu(186, 120);
  const point = map._contextPoint();

  projectOrigin = { x: -120, y: -60 };   // the operator pans the map
  fireMap("move");

  assert.equal(pinEl().style.left, "180px", "the pin stays on its ground point");
  assert.equal(pinEl().style.top, "140px");
  assert.equal(menuEl().style.left, "192px", "and the menu follows it");
  assert.deepEqual(map._contextPoint(), point, "the point itself never moves");
}

const tests = [
  testMapClickOpensTheMenuAtTheClickedPoint,
  testRightClickOpensTheSameMenu,
  testMenuShowsTheCoordinatesItActsOn,
  testNoActionsMeansNoMenu,
  testPlanningModeOwnsTheClick,
  testEnteringPlanningClosesAnOpenMenu,
  testReopeningReplacesTheOpenMenu,
  testRowsRenderLabelAndNote,
  testEnabledIsReEvaluatedOnEveryOpen,
  testChoosingARowRunsItWithThePointAndCloses,
  testDisabledRowDoesNothing,
  testAThrowingActionDoesNotBreakTheMenu,
  testFirstUsableRowTakesFocus,
  testEscapeCloses,
  testAnUnrelatedKeyDoesNotClose,
  testPointerDownOutsideCloses,
  testPointerDownInsideTheMenuDoesNotClose,
  testClosingUnhooksTheDocumentListeners,
  testCloseIsIdempotent,
  testClearingTheActionsClosesAnOpenMenu,
  testMenuSitsBelowRightOfThePinWithRoomToSpare,
  testMenuFlipsBackInsideNearTheBottomRightCorner,
  testMenuIsClampedToTheMapRatherThanFlippedWhenItBarelyFits,
  testMenuLargerThanTheMapIsPinnedInsideNotPushedOff,
  testMenuTracksItsGroundPointWhenTheMapPans,
];

let failed = 0;
for (const t of tests) {
  try {
    t();
    console.log(`ok   - ${t.name}`);
  } catch (err) {
    failed++;
    console.error(`FAIL - ${t.name}`);
    console.error(`      ${err && err.stack ? err.stack.split("\n").join("\n      ") : err}`);
  }
}

if (failed) {
  console.error(`\n${failed}/${tests.length} map context-menu test(s) FAILED`);
  process.exit(1);
}
console.log(`\nAll ${tests.length} map context-menu tests passed.`);
