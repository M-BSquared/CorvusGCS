"use strict";

/**
 * Frontend tests for map follow mode (Corvus.map) — the dead-zone camera.
 *
 * Two layers, because they fail for different reasons:
 *
 *   PART A — the rule as pure geometry (Corvus.map._followAction). No map, no
 *   telemetry: given a vehicle at (px, py) in a viewport, does the camera
 *   hold, ease, or cut? This is where the box maths is pinned.
 *
 *   PART B — the wiring, driven through a fake maplibregl map exactly as
 *   tests/test_frontend_mapmenu.js does. What matters here is restraint: the
 *   map must NOT move while the aircraft is inside the box, and must NOT move
 *   at all once the operator has dragged the view somewhere else.
 *
 * The fake's `project` is a real camera — panning it changes where a
 * coordinate lands — so "the map followed" is observed the way the operator
 * would see it, not by trusting a call count alone.
 *
 * Run:
 *   node tests/test_frontend_map_follow.js
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

// A rAF that hands back an id and never calls back: the marker's easing loop
// is registered but never advances, so nothing keeps the process alive and
// nothing races the assertions. Follow aims at the TARGET, not the eased
// marker position, so holding the loop still leaves the behaviour under test.
let rafId = 0;
window.requestAnimationFrame = () => ++rafId;
window.cancelAnimationFrame = () => {};
window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;
window.addEventListener = () => {};
window.removeEventListener = () => {};
window.dispatchEvent = () => true;

let reducedMotion = false;
window.matchMedia = (query) => ({
  matches: reducedMotion && String(query).includes("prefers-reduced-motion"),
  media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});
if (typeof global.performance === "undefined") global.performance = { now: () => Date.now() };
global.fetch = () => Promise.reject(new Error("offline"));
global.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };

// ---------------------------------------------------------------------------
// DOM stub (mirrors tests/test_frontend_mapmenu.js, plus [data-act] selectors
// because the control rail addresses its buttons that way).
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {},
    type: "", hidden: false, disabled: false, value: "", id: "", title: "",
    tabIndex: 0, _attrs: {}, _listeners: {}, _isEl: true,
    parentNode: null, parentElement: null,
    _box: { width: 0, height: 0 },
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
  e.focus = () => {};
  e.fire = (type, ev) => (e._listeners[type] || []).slice().forEach((cb) => cb(Object.assign({
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

function query(children, sel) {
  const out = [];
  const attr = sel.match(/\[data-act="([^"]+)"\]/);
  const base = sel.replace(/\[data-act="[^"]+"\]/, "");
  const wantTag = base && base[0] !== ".";
  const classes = base.split(".").filter(Boolean);
  (function walk(list) {
    list.forEach((c) => {
      if (!c || !c._isEl) return;
      const own = c.className.split(/\s+/).filter(Boolean);
      let ok = base ? (wantTag ? c.tagName === base.toUpperCase()
        : classes.every((cl) => own.includes(cl))) : true;
      if (ok && attr && c.dataset.act !== attr[1]) ok = false;
      if (ok) out.push(c);
      walk(c.children);
    });
  })(children);
  return out;
}

global.document = {
  createElement: makeEl,
  createElementNS: makeEl,
  getElementById: () => makeEl("div"),
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
};

// ---------------------------------------------------------------------------
// Fake maplibregl. `project` is a real camera: 100 000 px per degree around
// whatever the current centre is, so a pan genuinely changes where a
// coordinate lands on screen.
// ---------------------------------------------------------------------------
const VIEW_W = 1000;
const VIEW_H = 700;
const SCALE = 100000;          // px per degree
const HOME = [11.0, 48.5];     // [lng, lat]

const mapEvents = {};
let camera = { lng: HOME[0], lat: HOME[1] };
let easeCalls = [];
let setCenterCalls = [];
/** Pending follow pans: MapLibre fires moveend when the ease lands, and the
 *  module clears its in-flight guard there — so the fake makes that explicit
 *  rather than settling instantly and hiding the guard from the tests. */
let pendingMove = false;

const fakeMap = {
  on: (type, cb) => { (mapEvents[type] = mapEvents[type] || []).push(cb); },
  off: (type, cb) => {
    const list = mapEvents[type] || [];
    const i = list.indexOf(cb);
    if (i >= 0) list.splice(i, 1);
  },
  addControl() {}, addLayer() {}, addSource() {}, getSource: () => null,
  getLayer: () => null, setPaintProperty() {},
  getCanvas: () => makeEl("canvas"),
  getCanvasContainer: () => makeEl("div"),
  getContainer: () => mapEl,
  getPitch: () => 0, zoomIn() {}, zoomOut() {}, resize() {}, setPixelRatio() {},
  easeTo(opts) {
    easeCalls.push(opts);
    if (opts && opts.center) { camera = { lng: opts.center[0], lat: opts.center[1] }; }
    pendingMove = true;
  },
  setCenter(c) {
    setCenterCalls.push(c);
    camera = { lng: c[0], lat: c[1] };
    pendingMove = true;
  },
  project: (lngLat) => ({
    x: Math.round((lngLat[0] - camera.lng) * SCALE) + VIEW_W / 2,
    y: Math.round((camera.lat - lngLat[1]) * SCALE) + VIEW_H / 2,
  }),
};

global.maplibregl = {
  Map: function () { return fakeMap; },
  AttributionControl: function () { return {}; },
  Marker: function () {
    return {
      setLngLat() { return this; }, addTo() { return this; }, remove() {},
      setRotation() { return this; }, getElement: () => makeEl("div"),
    };
  },
};

function fireMap(type, payload) {
  (mapEvents[type] || []).slice().forEach((cb) => cb(payload));
}
/** Land any pan the map started, the way MapLibre does when an ease finishes. */
function settleMove() {
  if (!pendingMove) return;
  pendingMove = false;
  fireMap("moveend", {});
}

// Telemetry: init() subscribes, and centerOnVehicle reads getState().
let telemetryCb = null;
let liveState = { connected: false, position: [0, 0], heading: 0 };
Corvus.telemetry = {
  requestJson: () => Promise.reject(new Error("offline")),
  getState: () => liveState,
  subscribe(fn) { telemetryCb = fn; return () => { telemetryCb = null; }; },
};

require("../src/js/ui.js");
require("../src/js/map.js");

const map = Corvus.map;

const mapEl = makeEl("div");
mapEl.id = "map";
mapEl._box = { width: VIEW_W, height: VIEW_H };
const controlsEl = makeEl("div");
map.init(mapEl, controlsEl, makeEl("div"));
fireMap("load");
assert.equal(map.isReady(), true, "the fake map must reach the loaded state");

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Push one telemetry sample at a lng/lat offset (in degrees) from HOME. */
function fly(dLng, dLat) {
  liveState = {
    connected: true,
    position: [HOME[0] + dLng, HOME[1] + dLat],
    heading: 0,
  };
  telemetryCb(liveState);
}

function resetCamera() {
  camera = { lng: HOME[0], lat: HOME[1] };
  easeCalls = [];
  setCenterCalls = [];
  pendingMove = false;
  reducedMotion = false;
  map.setFollow(true);
}

/** Where the vehicle currently sits on screen, in CSS pixels. */
function vehicleScreenX() { return fakeMap.project(liveState.position).x; }

const followBtn = () => controlsEl.querySelector('[data-act="center"]');

// The dead zone for a 1000x700 viewport, derived the same way the module does:
// 40% of each axis, floored at 140 px. So x holds within +/-200 px of centre
// and y within +/-140 px.
const HALF_X = 200;
const HALF_Y = 140;

// ===========================================================================
// PART A — the rule, as pure geometry
// ===========================================================================

const action = map._followAction;

function testCentreHolds() {
  assert.equal(action(500, 350, 1000, 700), "hold",
    "a vehicle dead centre does not move the camera");
}

function testInsideTheBoxHolds() {
  assert.equal(action(500 + HALF_X - 1, 350, 1000, 700), "hold",
    "just inside the box: still no pan");
  assert.equal(action(500, 350 + HALF_Y - 1, 1000, 700), "hold");
  assert.equal(action(500 - HALF_X, 350 - HALF_Y, 1000, 700), "hold",
    "the box edge itself is inside — the pan needs a real crossing");
}

function testLeavingTheBoxEases() {
  assert.equal(action(500 + HALF_X + 1, 350, 1000, 700), "ease",
    "one pixel past the horizontal edge starts the pan");
  assert.equal(action(500, 350 + HALF_Y + 1, 1000, 700), "ease",
    "and the vertical edge behaves the same");
  assert.equal(action(0, 350, 1000, 700), "ease",
    "at the viewport edge it is still an ease, not a cut");
}

function testFarAwayCuts() {
  // Further than one viewport from the centre: an ease across that distance
  // is a smear, so the camera cuts instead. This is the reconnect case.
  assert.equal(action(500 + 1001, 350, 1000, 700), "jump");
  assert.equal(action(500, 350 + 701, 1000, 700), "jump");
  assert.equal(action(500 + 999, 350, 1000, 700), "ease",
    "just inside the cut threshold still eases");
}

function testDegenerateInputHolds() {
  // An unprojectable point (behind the horizon in 3D mode) or a container
  // with no size must never move the map: acting on a coordinate we do not
  // trust is worse than not acting.
  assert.equal(action(NaN, 350, 1000, 700), "hold");
  assert.equal(action(500, Infinity, 1000, 700), "hold");
  assert.equal(action(500, 350, 0, 0), "hold");
  assert.equal(action(500, 350, 1000, -700), "hold");
}

function testSmallViewportGetsAFloor() {
  // 40% of 200 px is 80 px, which would re-centre on every twitch — the
  // 140 px floor applies instead, so the box is +/-70.
  assert.equal(action(100 + 69, 100, 200, 200), "hold");
  assert.equal(action(100 + 71, 100, 200, 200), "ease");
}

function testBoxNeverFillsTheViewport() {
  // 40% of 150 px is 60, under the 140 floor — but 140 is 93% of the viewport,
  // which would leave no edge to cross. The 80% ceiling keeps the box
  // escapable: +/-60 px.
  assert.equal(action(75 + 55, 75, 150, 150), "hold");
  assert.equal(action(75 + 65, 75, 150, 150), "ease",
    "the box must stay escapable or the map silently stops following");
}

// ===========================================================================
// PART B — the wiring
// ===========================================================================

function testFollowIsOnByDefault() {
  assert.equal(map.isFollowing(), true, "the map follows the aircraft by default");
  assert.ok(followBtn(), "the control rail carries the crosshair button");
  assert.ok(followBtn().classList.contains("active"),
    "and it is lit while following");
}

function testFirstFixPlacesTheMap() {
  resetCamera();
  fly(0.001, 0.001);
  assert.equal(easeCalls.length, 1, "the first real fix places the map");
  assert.equal(easeCalls[0].zoom, 16, "at the working zoom");
  settleMove();
}

function testMovingInsideTheBoxDoesNotMoveTheMap() {
  resetCamera();
  // +0.0015 deg = 150 px, comfortably inside the +/-200 px dead zone.
  fly(0.0015, 0);
  assert.equal(easeCalls.length, 0, "no pan while the aircraft is inside the box");
  assert.equal(setCenterCalls.length, 0);
  assert.equal(vehicleScreenX(), 500 + 150,
    "the marker really did move across the screen — the test is not vacuous");
}

function testLeavingTheBoxPansTheMap() {
  resetCamera();
  fly(0.0025, 0);   // 250 px right of centre: past the 200 px edge
  assert.equal(easeCalls.length, 1, "crossing the edge pans the map");
  assert.deepEqual(easeCalls[0].center, liveState.position,
    "and pans to where the aircraft actually is");
  settleMove();
  assert.equal(vehicleScreenX(), 500,
    "which puts it back in the middle, with a full box to move in again");
}

function testAPanInFlightIsNotQueuedTwice() {
  resetCamera();
  fly(0.0025, 0);
  assert.equal(easeCalls.length, 1);
  // More samples land while the ease is still running (10 Hz telemetry, a
  // 700 ms pan). Without the in-flight guard each would queue another pan and
  // the map would judder for the whole flight.
  fly(0.0026, 0);
  fly(0.0027, 0);
  assert.equal(easeCalls.length, 1, "samples during the pan do not queue more pans");
  settleMove();
  fly(0.005, 0);
  assert.equal(easeCalls.length, 2, "but the next crossing after it lands does pan");
  settleMove();
}

/** A move the operator started, as MapLibre reports it. */
function userMove(type) { return { originalEvent: { type } }; }

function testDraggingTheMapStopsTheFollowing() {
  resetCamera();
  fireMap("movestart", userMove("mousemove"));
  assert.equal(map.isFollowing(), false,
    "dragging the map is the operator looking somewhere else");
  assert.equal(followBtn().classList.contains("active"), false,
    "and the crosshair goes dark to say so");

  fly(0.005, 0);   // far outside the box
  assert.equal(easeCalls.length, 0, "the aircraft no longer drags the view back");
  assert.equal(setCenterCalls.length, 0);
}

function testWheelZoomKeepsFollowing() {
  resetCamera();
  // Zooming is something an operator does WHILE watching the aircraft, so the
  // wheel is deliberately let through.
  fireMap("movestart", userMove("wheel"));
  assert.equal(map.isFollowing(), true);
}

function testOurOwnPansDoNotStopTheFollowing() {
  resetCamera();
  // The subtle one: every follow pan and every rail zoom fires movestart too.
  // Programmatic moves carry no originalEvent, and if they were treated as
  // operator input the very first follow pan would switch following off.
  fireMap("movestart", {});
  fireMap("movestart", undefined);
  assert.equal(map.isFollowing(), true);

  fly(0.0025, 0);
  assert.equal(easeCalls.length, 1, "so the map still follows after a pan of its own");
  settleMove();
  fly(0.006, 0);
  assert.equal(easeCalls.length, 2, "and after the one following that");
  settleMove();
}

function testTouchDragAlsoStopsTheFollowing() {
  resetCamera();
  fireMap("movestart", userMove("touchmove"));
  assert.equal(map.isFollowing(), false, "a finger drag counts the same as a mouse drag");
}

function testCentreButtonResumesFollowing() {
  resetCamera();
  fly(0.0005, 0);
  fireMap("movestart", userMove("mousemove"));
  assert.equal(map.isFollowing(), false);

  followBtn().parentNode.fire("click", { target: followBtn() });
  assert.equal(map.isFollowing(), true, "the crosshair button resumes following");
  assert.ok(followBtn().classList.contains("active"), "and lights up again");
  assert.ok(easeCalls.length >= 1, "it also recentres straight away");
  settleMove();

  fly(0.003, 0);
  assert.ok(easeCalls.length >= 2, "and the map tracks the aircraft again after it");
  settleMove();
}

function testSetFollowIsPublic() {
  resetCamera();
  assert.equal(map.setFollow(false), false);
  assert.equal(map.isFollowing(), false);
  fly(0.005, 0);
  assert.equal(easeCalls.length, 0, "off means off");
  map.setFollow(true);
  assert.equal(map.isFollowing(), true);
}

function testReducedMotionSnapsInsteadOfEasing() {
  resetCamera();
  reducedMotion = true;
  fly(0.0025, 0);
  assert.equal(easeCalls.length, 0, "reduced motion never animates the camera");
  assert.equal(setCenterCalls.length, 1, "it cuts instead, like the marker does");
  settleMove();
  reducedMotion = false;
}

function testAFarAwayVehicleIsCutTo() {
  resetCamera();
  fly(0.02, 0);   // 2000 px: more than a viewport from centre
  assert.equal(easeCalls.length, 0, "no smear across two screens");
  assert.equal(setCenterCalls.length, 1, "the camera cuts to it");
  settleMove();
  assert.equal(vehicleScreenX(), 500);
}

function testNoFixDoesNotMoveTheMap() {
  resetCamera();
  // [0,0] is the state store's "no fix yet" default, not a position in the
  // Gulf of Guinea — following it would fly the map off the planet.
  liveState = { connected: true, position: [0, 0], heading: 0 };
  telemetryCb(liveState);
  assert.equal(easeCalls.length, 0);
  assert.equal(setCenterCalls.length, 0);
}

// ---------------------------------------------------------------------------

const tests = [
  testCentreHolds,
  testInsideTheBoxHolds,
  testLeavingTheBoxEases,
  testFarAwayCuts,
  testDegenerateInputHolds,
  testSmallViewportGetsAFloor,
  testBoxNeverFillsTheViewport,
  testFollowIsOnByDefault,
  testFirstFixPlacesTheMap,
  testMovingInsideTheBoxDoesNotMoveTheMap,
  testLeavingTheBoxPansTheMap,
  testAPanInFlightIsNotQueuedTwice,
  testDraggingTheMapStopsTheFollowing,
  testTouchDragAlsoStopsTheFollowing,
  testWheelZoomKeepsFollowing,
  testOurOwnPansDoNotStopTheFollowing,
  testCentreButtonResumesFollowing,
  testSetFollowIsPublic,
  testReducedMotionSnapsInsteadOfEasing,
  testAFarAwayVehicleIsCutTo,
  testNoFixDoesNotMoveTheMap,
];

for (const t of tests) {
  t();
  console.log(`  ok  ${t.name}`);
}
console.log(`\nAll ${tests.length} map-follow tests passed.`);
