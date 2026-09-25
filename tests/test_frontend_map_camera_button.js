"use strict";

/**
 * The map rail's camera button (Corvus.map, wireCameraButton).
 *
 * It exists only while a camera is set up: hidden with its divider when there
 * is none, a plain open/close for one camera, and a list beside the rail when
 * there are several, one row per camera plus "Open all" / "Close all".
 *
 * The map harness is tests/test_frontend_map_follow.js's; Corvus.videoWindows
 * is a fake that records what the button asked of it.
 *
 * Run:
 *   node tests/test_frontend_map_camera_button.js
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
  const group = sel.match(/\[data-group="([^"]+)"\]/);
  const base = sel.replace(/\[data-(act|group)="[^"]+"\]/, "");
  const wantTag = base && base[0] !== ".";
  const classes = base.split(".").filter(Boolean);
  (function walk(list) {
    list.forEach((c) => {
      if (!c || !c._isEl) return;
      const own = c.className.split(/\s+/).filter(Boolean);
      let ok = base ? (wantTag ? c.tagName === base.toUpperCase()
        : classes.every((cl) => own.includes(cl))) : true;
      if (ok && attr && c.dataset.act !== attr[1]) ok = false;
      if (ok && group && c.dataset.group !== group[1]) ok = false;
      if (ok) out.push(c);
      walk(c.children);
    });
  })(children);
  return out;
}

global.document = {
  body: makeEl("body"),
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


// ---------------------------------------------------------------------------
// A fake Corvus.videoWindows: the camera list, which windows are open, and
// what the button asked for.
// ---------------------------------------------------------------------------
const calls = [];
let list = [];
let openIds = new Set();
const cameraFns = [];
const changeFns = [];
const changed = () => changeFns.forEach((fn) => fn(openIds.size));
Corvus.videoWindows = {
  cameras: () => list.map((c) => Object.assign({}, c)),
  onCameras(fn) { cameraFns.push(fn); return () => {}; },
  onChange(fn) { changeFns.push(fn); return () => {}; },
  loadCameras() { calls.push("load"); return Promise.resolve(this.cameras()); },
  has: (id) => openIds.has(id),
  count: () => openIds.size,
  open(cam) { calls.push("open:" + cam.id); openIds.add(cam.id); changed(); return true; },
  toggle(cam) {
    calls.push("toggle:" + cam.id);
    if (openIds.has(cam.id)) openIds.delete(cam.id); else openIds.add(cam.id);
    changed();
  },
  closeAll() { calls.push("closeAll"); openIds = new Set(); changed(); },
  toggleAll() { calls.push("toggleAll"); return Promise.resolve(0); },
};
function setList(next) {
  list = next;
  cameraFns.forEach((fn) => fn(Corvus.videoWindows.cameras()));
}

require("../src/js/ui.js");
require("../src/js/map.js");

const mapEl = makeEl("div");
mapEl.id = "map";
mapEl._box = { width: VIEW_W, height: VIEW_H };
const controlsEl = makeEl("div");
Corvus.map.init(mapEl, controlsEl, makeEl("div"));
const atStart = { calls: calls.slice(), hidden: controlsEl.querySelector('[data-act="video"]').hidden };

const btn = () => controlsEl.querySelector('[data-act="video"]');
const divider = () => controlsEl.querySelector('.mc-divider[data-group="video"]');
const press = () => controlsEl.fire("click", { target: btn() });
const surface = () => document.body.children.find((c) => c.classList.contains("camera-popover")) || null;
const rowsOf = (el) => {
  const out = [];
  (function walk(n) { n.children.forEach((c) => { if (c.classList.contains("option-item")) out.push(c); else walk(c); }); })(el);
  return out;
};
const labelOf = (row) => {
  let text = "";
  (function walk(n) { n.children.forEach((c) => { if (c.classList.contains("option-item-label")) text = c.textContent; else walk(c); }); })(row);
  return text;
};
const reset = () => {
  if (surface()) press();
  openIds = new Set();
  calls.length = 0;
};

const CAM_A = { id: "a", name: "Gimbal", address: "10.0.0.2/main", kind: "rtsp" };
const CAM_B = { id: "b", name: "Belly", address: "10.0.0.3/main", kind: "webrtc" };

// ---------------------------------------------------------------------------

function testTheButtonStartsHiddenAndAsksForTheCameraList() {
  assert.ok(btn(), "the rail still carries the camera button");
  assert.ok(divider(), "and the divider that belongs to it");
  assert.equal(atStart.hidden, true, "no button flashes up before the list is read");
  assert.deepEqual(atStart.calls, ["load"]);
}

function testWithoutACameraTheButtonIsHidden() {
  reset();
  setList([]);
  assert.equal(btn().hidden, true);
  assert.equal(divider().hidden, true, "no divider left over the empty space");
}

function testOneCameraIsOpenedAndClosedByThePress() {
  reset();
  setList([CAM_A]);
  assert.equal(btn().hidden, false);
  assert.equal(divider().hidden, false);
  assert.match(btn().title, /Gimbal/);
  press();
  assert.deepEqual(calls, ["toggle:a"]);
  assert.equal(surface(), null, "one camera needs no list");
  assert.ok(btn().classList.contains("active"), "lit while its window is open");
  press();
  assert.deepEqual(calls, ["toggle:a", "toggle:a"]);
  assert.equal(btn().classList.contains("active"), false);
}

function testSeveralCamerasOpenAList() {
  reset();
  setList([CAM_A, CAM_B]);
  press();
  const el = surface();
  assert.ok(el, "several cameras open a list beside the rail");
  const rows = rowsOf(el);
  assert.deepEqual(rows.map(labelOf), ["Gimbal", "Belly", "Open all"]);
  assert.equal(btn().getAttribute("aria-expanded"), "true");
  assert.deepEqual(calls, [], "opening the list opens no camera");
}

function testARowOpensItsCameraAndTheListStaysOpen() {
  reset();
  setList([CAM_A, CAM_B]);
  press();
  rowsOf(surface())[1].fire("click");
  assert.deepEqual(calls, ["toggle:b"]);
  const el = surface();
  assert.ok(el, "the list stays, so a second camera can be picked");
  const rows = rowsOf(el);
  assert.equal(rows[0].classList.contains("active"), false);
  assert.equal(rows[1].classList.contains("active"), true, "the open camera is marked");
  assert.equal(rows[1].getAttribute("aria-checked"), "true");
  assert.equal(labelOf(rows[2]), "Close all");
}

function testTheLastRowOpensOrClosesThemAll() {
  reset();
  setList([CAM_A, CAM_B]);
  press();
  rowsOf(surface())[2].fire("click");
  assert.deepEqual(calls, ["open:a", "open:b"]);
  assert.equal(labelOf(rowsOf(surface())[2]), "Close all");
  rowsOf(surface())[2].fire("click");
  assert.deepEqual(calls.slice(2), ["closeAll"]);
}

function testTheListGoesWhenOnlyOneCameraIsLeft() {
  reset();
  setList([CAM_A, CAM_B]);
  press();
  assert.ok(surface());
  setList([CAM_A]);
  assert.equal(surface(), null);
  setList([]);
  assert.equal(btn().hidden, true);
}

function testNoDashesInTheButtonsText() {
  setList([CAM_A, CAM_B]);
  press();
  const text = rowsOf(surface()).map(labelOf).join(" ") + " " + btn().title;
  assert.doesNotMatch(text, /[\u2013\u2014]| - /);
  press();
}

const tests = [
  testTheButtonStartsHiddenAndAsksForTheCameraList,
  testWithoutACameraTheButtonIsHidden,
  testOneCameraIsOpenedAndClosedByThePress,
  testSeveralCamerasOpenAList,
  testARowOpensItsCameraAndTheListStaysOpen,
  testTheLastRowOpensOrClosesThemAll,
  testTheListGoesWhenOnlyOneCameraIsLeft,
  testNoDashesInTheButtonsText,
];

for (const t of tests) {
  t();
  console.log(`  ok  ${t.name}`);
}
console.log(`\nAll ${tests.length} map camera button tests passed.`);
