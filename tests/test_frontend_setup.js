"use strict";

/**
 * Frontend tests for the Setup page (Corvus.setup).
 *
 * Plain Node-runnable assertions (no browser, no test runner) following the
 * same pattern as tests/test_frontend_plugins.js and tests/test_frontend_link.js:
 * stub the globals the module touches, require the source, and assert on the
 * rendered DOM, the config-action spies, and the teardown/no-leak behaviour.
 * A small DOM stub (no HTML parser) is used because setup.js builds its DOM
 * via createElement, so a children-walking stub suffices.
 *
 * Run:
 *   node tests/test_frontend_setup.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals so setup.js loads and runs in Node.
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

// The chart modules subscribe to corvus:themechange on window, so the listener
// pair has to exist for the theme-reactive redraw path to be exercised at all.
const windowListeners = {};
window.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
window.removeEventListener = (t, cb) => {
  const list = windowListeners[t] || [];
  const i = list.indexOf(cb);
  if (i >= 0) list.splice(i, 1);
};


global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};

// prefers-reduced-motion toggle for the reduced-motion test.
let reducedMotion = false;
window.matchMedia = (query) => ({
  matches: reducedMotion && String(query).includes("prefers-reduced-motion"),
  media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});

// Controlled clock for the Plotly throttle and the tSec buffer timestamps.
let clock = 1000;
Date.now = () => clock;

// window.dispatchEvent captures corvus:notification events so the action runners
// can fire them without a real event target.
const dispatched = [];
window.dispatchEvent = (event) => { dispatched.push(event); };

// Fake EventSource so the params-progress SSE can be driven deterministically.
const eventSources = [];
class FakeEventSource {
  constructor(url) { this.url = url; this.listeners = new Map(); this.closed = false; eventSources.push(this); }
  addEventListener(type, cb) { this.listeners.set(type, cb); }
  close() { this.closed = true; }
  // A real EventSource delivers e.data as a STRING (the server's payload), and
  // setup.js does JSON.parse(e.data) — so stringify here to match the wire format.
  emit(type, data) { const cb = this.listeners.get(type); if (cb) cb({ data: typeof data === "string" ? data : JSON.stringify(data) }); }
}
global.EventSource = FakeEventSource;

// Stub fetch for the firmware upload (raw binary POST, not postAction). The
// firmware page POSTs the file body directly via fetch and reads res.json();
// tests drive the response via setFetchResponse. Reset per test with resetFetch.
let fetchCalls = [];
let _fetchResp = { ok: true, json: async () => ({ ok: true, state: "flashing" }) };
global.fetch = (url, opts) => {
  fetchCalls.push({ url, opts });
  return Promise.resolve(_fetchResp);
};
function setFetchResponse(resp) { _fetchResp = resp; }
function resetFetch() {
  fetchCalls = [];
  _fetchResp = { ok: true, json: async () => ({ ok: true, state: "flashing" }) };
}

// AbortController stub — the firmware uploader creates one per upload and
// aborts it on teardown. The signal is a plain object with a no-op listener API.
global.AbortController = class AbortController {
  constructor() { this.signal = { aborted: false, addEventListener() {}, removeEventListener() {} }; }
  abort() { if (this.signal) this.signal.aborted = true; }
};

// Capture setInterval callbacks so the ~1s params poll can be driven manually
// (never auto-fires — keeps the tests deterministic). clearInterval marks cleared.
const intervalCbs = [];
let nextIntervalId = 1;
const clearedIds = new Set();
window.setInterval = (cb) => { const id = nextIntervalId++; intervalCbs.push({ id, cb }); return id; };
window.clearInterval = (id) => { clearedIds.add(id); };
window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;

// ---------------------------------------------------------------------------
// Minimal DOM stub (mirrors tests/test_frontend_plugins.js). The source builds
// structure with createElement + appendChild, so this stub tracks children,
// className/classList, textContent, dataset, style, listeners, and innerHTML
// (clears children when set to "").
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "",
    textContent: "",
    children: [],
    dataset: {},
    style: {},
    type: "",
    hidden: false,
    disabled: false,
    value: "",
    readOnly: false,
    id: "",
    _attrs: {},
    _listeners: {},
    _isEl: true,
  };
  let _html = "";
  Object.defineProperty(e, "innerHTML", {
    get() { return _html; },
    set(v) {
      _html = String(v);
      if (_html === "") e.children.length = 0;   // mirror: clearing HTML drops children
    },
  });
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) { const has = e.classList.contains(c); const next = force === undefined ? !has : !!force; if (next) e.classList.add(c); else e.classList.remove(c); return next; },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  // parentNode is maintained like a real DOM so the standard
  // `node.parentNode.removeChild(node)` removal idiom works under the stub —
  // Corvus.ui.modal.close() uses it to unmount a dialog.
  e.appendChild = (c) => { c.parentNode = e; e.children.push(c); return c; };
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; return c; };
  e.insertBefore = (n, ref) => { const i = ref ? e.children.indexOf(ref) : e.children.length; if (i < 0) e.children.push(n); else e.children.splice(i, 0, n); return n; };
  Object.defineProperty(e, "firstChild", { get() { return e.children[0] || null; } });
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (type, cb) => { (e._listeners[type] = e._listeners[type] || []).push(cb); };
  e.removeEventListener = () => {};
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  return e;
}

function querySel(children, sel) {
  // Supports a single class selector (".cls") or tag name. Walks recursively.
  const out = [];
  const wantTag = sel && sel[0] !== ".";
  const classes = sel ? sel.split(".").filter(Boolean) : [];
  function walk(list) {
    for (const c of list) {
      if (!c || !c._isEl) continue;
      const ok = wantTag ? c.tagName === sel.toUpperCase() : classes.every((cl) => c.className.split(/\s+/).includes(cl));
      if (ok) out.push(c);
      if (c.children) walk(c.children);
    }
  }
  walk(children);
  return out;
}

// pageView element used by openView/backButton via document.getElementById.
let pageViewEl = null;
global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: (id) => (id === "pageView" ? pageViewEl : null),
  querySelectorAll: () => [],
  // Both halves of the listener pair: Corvus.ui.modal registers a document
  // keydown handler while a dialog is mounted and removes it on close, so a
  // stub with only addEventListener makes teardown throw.
  addEventListener: () => {},
  removeEventListener: () => {},
};

// Helpers ---------------------------------------------------------------------
function flushMicrotasks() { return new Promise((r) => setTimeout(r, 0)); }

function findByClass(root, cls) { return root.querySelectorAll("." + cls); }
function findOneByClass(root, cls) { return root.querySelector("." + cls); }
function findByTag(root, tag) { return root.querySelectorAll(tag); }
// Walk every descendant of `root` and return those whose `dataset[key]`
// matches `value`. (Cannot use findByClass(root, "*") — the stub's querySel
// only supports class/tag selectors, and ".*" matches nothing.)
function findByDataset(root, key, value) {
  const out = [];
  function walk(list) {
    for (const e of list) {
      if (!e || !e._isEl) continue;
      if (e.dataset && e.dataset[key] === value) out.push(e);
      if (e.children) walk(e.children);
    }
  }
  walk(root.children || []);
  return out;
}

/** Fire all listeners of a given type on an element (simulate a click/input). */
function fire(el, type) {
  const listeners = (el && el._listeners && el._listeners[type]) || [];
  listeners.forEach((cb) => cb({}));
}

// ---------------------------------------------------------------------------
// Fake Plotly (mirrors tests/test_frontend_plugins.js) and a fake telemetry.
// ---------------------------------------------------------------------------
function fakePlotly() {
  return {
    reactCalls: [],
    purgeCalls: [],
    react(gd, data, layout, config) { this.reactCalls.push({ gd, data, layout, config }); },
    purge(gd) { this.purgeCalls.push(gd); },
  };
}

/**
 * Build a fake Corvus.telemetry with spies. `subscribe` captures the callback
 * (does NOT auto-fire — tests drive it via getSubCb) and returns an unsub spy.
 * postAction/requestJson resolve controllable responses.
 */
function makeFakeTelemetry(opts = {}) {
  const postCalls = [];
  const requests = [];
  let subCb = null;
  let unsubCalls = 0;
  const unsub = () => { unsubCalls++; };
  let paramsResponse = opts.paramsResponse || { complete: false, received: 0, count: 0, params: [] };
  const urlResponses = Object.assign({}, opts.urlResponses);
  let state = opts.state || { armed: false, connected: true };
  const telemetry = {
    postAction(url, payload) {
      postCalls.push({ url, payload });
      // /api/params/set may be rejected (e.g. "cannot set while armed") without
      // affecting the download/autotune/calibrate actions.
      if (url === "/api/params/set" && opts.setReject) return Promise.reject(new Error(opts.setReject));
      return Promise.resolve({ ok: true });
    },
    requestJson(url) {
      requests.push(url);
      // Per-URL overrides: the firmware page fetches both a status and a
      // catalogue, and one canned response for every URL cannot express that.
      for (const prefix of Object.keys(urlResponses)) {
        if (String(url).startsWith(prefix)) {
          const r = urlResponses[prefix];
          return r instanceof Error ? Promise.reject(r) : Promise.resolve(r);
        }
      }
      return Promise.resolve(paramsResponse);
    },
    subscribe(fn) { subCb = fn; return unsub; },
    getState() { return state; },
  };
  return {
    telemetry, postCalls, requests,
    get unsubCalls() { return unsubCalls; },
    getSubCb: () => subCb,
    setParamsResponse(r) { paramsResponse = r; },
    setResponse(prefix, r) { urlResponses[prefix] = r; },
    setState(s) { state = s; },
  };
}

// ---------------------------------------------------------------------------
// Load the module under test (defines window.Corvus.setup). The setup page is
// split across four files: shared helpers, the calibration page, the
// parameters page, and the thin orchestrator. Load them in the same order as
// index.html so dependencies resolve.
// ---------------------------------------------------------------------------
// ui.js first: it defines Corvus.ui, the component layer every other
// module builds its DOM with (index.html loads it in the same order).
require("../src/js/ui.js");
require("../src/js/setup-shared.js");
require("../src/js/calib-figures.js");
require("../src/js/calib-protocol.js");
require("../src/js/setup-calibration.js");
require("../src/js/setup-parameters.js");
require("../src/js/setup-firmware.js");
require("../src/js/setup.js");

// ===========================================================================
// PART A — Tile grid
// ===========================================================================

async function testTileGridRendersTwoTiles() {
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);

  const tiles = findByClass(container, "setup-tile");
  assert.equal(tiles.length, 3, "three tiles rendered");
  assert.equal(tiles[0].dataset.view, "calibration", "first tile is Calibration");
  assert.equal(tiles[1].dataset.view, "parameters", "second tile is Parameters");
  assert.equal(tiles[2].dataset.view, "firmware", "third tile is Firmware");

  // Tile titles are real text nodes. Setup tiles are Corvus.ui.tile instances
  // (shared with the Plugins grid), so the title carries the component's
  // .tile-title class; .setup-tile is now only the layout modifier.
  const titles = tiles.map((t) => findOneByClass(t, "tile-title").textContent);
  assert.deepEqual(titles, ["Calibration", "Parameters", "Firmware"]);
}

async function testClickTileSwapsToSubPageAndBackReturns() {
  window.Plotly = fakePlotly();
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  const tiles = findByClass(container, "setup-tile");
  // Click Calibration → sub-page rendered (back button + calib grid present).
  fire(tiles[0], "click");
  assert.ok(findOneByClass(container, "setup-back"), "back button present on calibration sub-page");
  assert.ok(findOneByClass(container, "calib-cards"), "calibration card list present");

  // Click back → tiles grid restored.
  const back = findOneByClass(container, "setup-back");
  fire(back, "click");
  assert.ok(findOneByClass(container, "setup-tiles"), "tile grid restored after back");
  assert.equal(findByClass(container, "calib-cards").length, 0, "calibration list gone after back");
  delete window.Plotly;
}

async function testTeardownRunsOnSwap() {
  const plot = fakePlotly();
  window.Plotly = plot;
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[0], "click");   // open calibration
  assert.equal(plot.reactCalls.length, 3, "three graphs initialised");
  assert.equal(fake.unsubCalls, 1, "grid unsub fired on openView");

  // Click back → teardown runs: grid + calibration unsub, Plotly.purge exactly 3.
  fire(findOneByClass(container, "setup-back"), "click");
  assert.equal(fake.unsubCalls, 2, "grid + calibration unsub on back");
  assert.equal(plot.purgeCalls.length, 3, "Plotly.purge called exactly once per graph on back");
  delete window.Plotly;
}

async function testReRenderTearsDownActiveSubPage() {
  // Re-entering Setup (render called again) must tear the active sub-page down
  // — the left-nav re-entry case. No leaks.
  const plot = fakePlotly();
  window.Plotly = plot;
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[0], "click");   // open calibration
  assert.equal(plot.reactCalls.length, 3);

  // Re-render (left-nav re-entry) → teardown of the calibration sub-page.
  Corvus.setup.render(container);
  assert.equal(fake.unsubCalls, 2, "unsub on re-render");
  assert.equal(plot.purgeCalls.length, 3, "purge on re-render");
  assert.ok(findOneByClass(container, "setup-tiles"), "tile grid restored on re-render");
  delete window.Plotly;
}

// The Vehicle Info card must update live: PX4 version only arrives a few
// seconds after connect via AUTOPILOT_VERSION, so the initial getState()
// snapshot would otherwise show "—" forever. The grid subscribes once on
// render and updates only the five row values per telemetry push.
async function testVehicleInfoUpdatesLive() {
  const fake = makeFakeTelemetry({
    state: { connected: false, armed: false, autopilot: "", vehicle_type: "", px4_version: "" },
  });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  // Grid subscribed once; fake.getSubCb() returns the grid's callback.

  // Initial: every Vehicle Info row reflects the empty/disconnected snapshot.
  assert.equal(findByDataset(container, "infoKey", "px4_version")[0].textContent, "—",
    "PX4 Version row initially shows —");
  assert.equal(findByDataset(container, "infoKey", "autopilot")[0].textContent, "—",
    "Autopilot row initially shows —");
  assert.equal(findByDataset(container, "infoKey", "connected")[0].textContent, "No",
    "Connected row initially shows No");

  // Emit a telemetry push with a real PX4 version → rows update live.
  fake.getSubCb()({
    connected: true, armed: false,
    autopilot: "PX4", vehicle_type: "Standard", px4_version: "v1.18.0",
  });
  assert.equal(findByDataset(container, "infoKey", "px4_version")[0].textContent, "v1.18.0",
    "PX4 Version row updated to v1.18.0");
  assert.equal(findByDataset(container, "infoKey", "autopilot")[0].textContent, "PX4",
    "Autopilot row updated to PX4");
  assert.equal(findByDataset(container, "infoKey", "vehicle_type")[0].textContent, "Standard",
    "Vehicle Type row updated to Standard");
  assert.equal(findByDataset(container, "infoKey", "connected")[0].textContent, "Yes",
    "Connected row updated to Yes");

  // Re-render: the grid's teardown fires its unsub (no leak), then the grid
  // re-subscribes. fake.unsubCalls increments by exactly 1 for the grid unsub.
  const before = fake.unsubCalls;
  Corvus.setup.render(container);
  assert.equal(fake.unsubCalls, before + 1, "grid unsub called on re-render (no leak)");
}

// ===========================================================================
// PART B — Calibration
// ===========================================================================

async function testCalibrationCardsCoverEveryProcedure() {
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[0], "click");   // Calibration

  const cards = findByClass(findOneByClass(container, "calib-cards"), "calib-card");
  assert.equal(cards.length, 7, "one card per calibration procedure");
  assert.deepEqual(cards.map((c) => c.dataset.type), Corvus.calibProtocol.ORDER,
    "cards follow the documented procedure order");

  // Every card names its procedure and its position count, because "which one
  // do I need" is the question the list has to answer.
  for (const card of cards) {
    const proc = Corvus.calibProtocol.PROCEDURES[card.dataset.type];
    assert.equal(findOneByClass(card, "calib-card-title").textContent, proc.label);
    assert.equal(findOneByClass(card, "calib-card-desc").textContent, proc.summary);
  }
  const motor = cards.find((c) => c.dataset.type === "motor");
  assert.ok(findOneByClass(motor, "calib-card-flag"), "motor card flags the props-off hazard");

  // Opening a card must NOT start anything — the wizard's brief comes first.
  fire(cards[0], "click");
  await flushMicrotasks();
  assert.equal(fake.postCalls.filter((c) => c.url === "/api/calibrate").length, 0,
    "opening a calibration posts nothing");
  assert.ok(findOneByClass(container, "calib-stage"), "wizard stage rendered");
}

async function testCalibrationArmedGating() {
  const fake = makeFakeTelemetry({ state: { armed: false, connected: true, warnings: [] } });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[0], "click");   // Calibration
  const cards = findByClass(findOneByClass(container, "calib-cards"), "calib-card");

  // Initial state disarmed → cards enabled.
  assert.ok(cards.every((c) => !c.disabled), "cards enabled while disarmed");

  // Emit armed → all disabled + banner visible.
  fake.getSubCb()({ armed: true, connected: true, warnings: [] });
  assert.ok(cards.every((c) => c.disabled), "cards disabled while armed");
  const banner = findByClass(container, "setup-armed-banner")[0];
  assert.ok(banner && !banner.hidden, "armed banner shown while armed");

  // Emit disarmed → re-enabled + banner hidden.
  fake.getSubCb()({ armed: false, connected: true, warnings: [] });
  assert.ok(cards.every((c) => !c.disabled), "cards re-enabled when disarmed");
  assert.ok(banner.hidden, "armed banner hidden when disarmed");
}

async function testReadinessStripReflectsLinkAndArmedState() {
  const fake = makeFakeTelemetry({ state: { armed: false, connected: false, warnings: [] } });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[0], "click");

  const chips = findByClass(container, "calib-ready-chip");
  assert.equal(chips.length, 2, "link + armed readiness chips");
  const link = chips.find((c) => c.dataset.key === "link");
  assert.equal(link.dataset.state, "bad", "no link reads as not ready");
  assert.equal(findOneByClass(link, "calib-ready-label").textContent, "No link");

  fake.getSubCb()({ armed: false, connected: true, warnings: [] });
  assert.equal(link.dataset.state, "ok", "link up reads as ready");
  assert.equal(findOneByClass(link, "calib-ready-label").textContent, "Link up");
}

// ===========================================================================
// PART C — Autotune (POD Tuning)
// ===========================================================================

async function testAutotuneButtonsMapToAxes() {
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[0], "click");   // Calibration

  const controls = findOneByClass(container, "autotune-controls");
  const btns = findByClass(controls, "autotune-btn");
  assert.equal(btns.length, 4, "four autotune buttons");

  const axes = btns.map((b) => b.dataset.axis).sort();
  assert.deepEqual(axes, ["all", "pitch", "roll", "yaw"].sort(), "axes are roll/pitch/yaw/all");

  // No velocity-controller autotune button exists (PX4 accuracy note).
  assert.ok(!axes.includes("velocity"), "no 'velocity' autotune button (PX4 has none)");

  // Each button posts /api/autotune with its axis.
  for (const axis of ["roll", "pitch", "yaw", "all"]) {
    const btn = btns.find((b) => b.dataset.axis === axis);
    fake.postCalls.length = 0;
    fire(btn, "click");
    await flushMicrotasks();
    const call = fake.postCalls.find((c) => c.url === "/api/autotune");
    assert.ok(call, `POST /api/autotune issued for ${axis}`);
    assert.deepEqual(call.payload, { axis }, `${axis} posts {axis:'${axis}'}`);
  }
}

async function testAutotuneArmedGating() {
  const fake = makeFakeTelemetry({ state: { armed: false, warnings: [] } });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[0], "click");
  const btns = findByClass(findOneByClass(container, "autotune-controls"), "autotune-btn");

  assert.ok(btns.every((b) => !b.disabled), "autotune enabled while disarmed");
  fake.getSubCb()({ armed: true, connected: true, warnings: [] });
  assert.ok(btns.every((b) => b.disabled), "autotune disabled while armed");
  fake.getSubCb()({ armed: false, connected: true, warnings: [] });
  assert.ok(btns.every((b) => !b.disabled), "autotune re-enabled when disarmed");
}

// ===========================================================================
// PART F — Plotly autotune graphs (buffer + react + purge)
// ===========================================================================

async function testAutotuneGraphsReactAndBuffer() {
  const plot = fakePlotly();
  window.Plotly = plot;
  const fake = makeFakeTelemetry({ state: { armed: false, warnings: [] } });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[0], "click");   // Calibration

  // Three graphs → three initial Plotly.react calls (empty figures).
  assert.equal(plot.reactCalls.length, 3, "three initial react calls");
  const charts = findByClass(container, "autotune-graph");
  assert.equal(charts.length, 3, "three graph containers in the DOM");

  // Drive telemetry with rollspeed/roll/groundspeed; the buffer appends one
  // point per graph and the throttled redraw fires after the window elapses.
  const cb = fake.getSubCb();
  // First frame within the throttle window → buffer appends, no redraw yet.
  cb({ armed: false, connected: true, warnings: [], rollspeed: 1.5, roll: 2.5, groundspeed: 3.5 });
  assert.equal(plot.reactCalls.length, 3, "no redraw within the throttle window");

  // Advance past the throttle window → one redraw per graph.
  clock = 1000 + 150;
  cb({ armed: false, connected: true, warnings: [], rollspeed: 1.5, roll: 2.5, groundspeed: 3.5 });
  assert.equal(plot.reactCalls.length, 6, "one redraw per graph after the throttle window");

  // The three graphs' redraw data carries the appended telemetry values.
  // The last three react calls correspond to roll rate / roll attitude / h-vel.
  const r3 = plot.reactCalls[3].data[0].y;
  const r4 = plot.reactCalls[4].data[0].y;
  const r5 = plot.reactCalls[5].data[0].y;
  assert.ok(r3.includes(1.5), "roll rate buffer has rollspeed value");
  assert.ok(r4.includes(2.5), "roll attitude buffer has roll value");
  assert.ok(r5.includes(3.5), "horizontal velocity buffer has groundspeed value");

  // Teardown purges each graph exactly once.
  fire(findOneByClass(container, "setup-back"), "click");
  assert.equal(plot.purgeCalls.length, 3, "Plotly.purge exactly once per graph on teardown");
  delete window.Plotly;
}

async function testReducedMotionZeroDurationTransition() {
  reducedMotion = true;
  const plot = fakePlotly();
  window.Plotly = plot;
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[0], "click");

  assert.ok(plot.reactCalls.length >= 3, "graphs initialised under reduced motion");
  for (const call of plot.reactCalls) {
    assert.ok(call.config && call.config.transition && call.config.transition.duration === 0,
      "reduced-motion config has transition.duration === 0");
  }
  fire(findOneByClass(container, "setup-back"), "click");
  delete window.Plotly;
  reducedMotion = false;
}

// ===========================================================================
// PART D — Parameters (download flow + armed gating)
// ===========================================================================

async function testParametersDoesNotAutoDownload() {
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[1], "click");   // Parameters

  // Download button present, no editor yet, no download POST issued.
  assert.ok(findOneByClass(container, "params-download-btn"), "Download button present");
  assert.equal(findByClass(container, "params-table").length, 0, "editor not rendered before download");
  assert.equal(fake.postCalls.filter((c) => c.url === "/api/params/download").length, 0,
    "no auto-download on opening the parameters page");
}

async function testParametersDownloadFlowGating() {
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;
  clearedIds.clear();

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[1], "click");   // Parameters
  fire(findOneByClass(container, "params-download-btn"), "click");
  await flushMicrotasks();   // postAction → openProgressView

  // Download POST issued; progress view opened (SSE + poll created).
  assert.equal(fake.postCalls.filter((c) => c.url === "/api/params/download").length, 1, "download POST issued");
  assert.ok(findOneByClass(container, "progress-bar"), "progress bar shown");
  assert.equal(eventSources.length, 1, "progress SSE created");
  assert.ok(intervalCbs.length >= 1, "poll timer created");

  const sse = eventSources[0];

  // Feed complete:false via the poll fallback → progress updates, editor NOT shown.
  fake.setParamsResponse({ complete: false, received: 5, count: 10, state: "downloading", params: [] });
  intervalCbs[0].cb();   // manual poll tick
  await flushMicrotasks();
  assert.equal(findByClass(container, "params-table").length, 0, "editor NOT shown while complete:false");
  const fill = findOneByClass(container, "progress-bar-fill");
  assert.equal(fill.style.width, "50%", "progress bar reflects received/count (5/10)");

  // Feed complete via the SSE → finishDownload → editor shown with the right rows.
  fake.setParamsResponse({
    complete: true, received: 2, count: 2, state: "complete",
    params: [
      { name: "FW_ACRO_LIM", value: 1, type: 9 },
      { name: "MC_ROLL_P", value: 6.5, type: 9 },
    ],
  });
  sse.emit("progress", { state: "complete", received: 2, count: 2 });
  await flushMicrotasks();   // finishDownload → requestJson → renderEditor
  await flushMicrotasks();

  assert.ok(findOneByClass(container, "params-table"), "editor rendered when complete:true");
  const rows = findByClass(container, "params-row");
  assert.equal(rows.length, 2, "two parameter rows rendered");
  // Rows are sorted by name.
  assert.equal(findOneByClass(rows[0], "params-name").textContent, "FW_ACRO_LIM");
  assert.equal(findOneByClass(rows[1], "params-name").textContent, "MC_ROLL_P");
  // Values populated from the params array.
  assert.equal(findOneByClass(rows[0], "params-value").value, "1");
  assert.equal(findOneByClass(rows[1], "params-value").value, "6.5");
}

async function testParametersArmedGatingReadOnly() {
  const fake = makeFakeTelemetry({ state: { armed: false } });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[1], "click");
  fire(findOneByClass(container, "params-download-btn"), "click");
  await flushMicrotasks();

  // Complete the download → editor rendered.
  fake.setParamsResponse({
    complete: true, received: 1, count: 1, state: "complete",
    params: [{ name: "MC_ROLL_P", value: 6.5, type: 9 }],
  });
  eventSources[0].emit("progress", { state: "complete", received: 1, count: 1 });
  await flushMicrotasks();
  await flushMicrotasks();

  const inputs = findByClass(container, "params-value");
  const applies = findByClass(container, "params-apply");
  const banner = findByClass(container, "params-banner")[0];
  assert.ok(inputs.length >= 1, "editor value inputs present");

  // Disarmed → editable.
  assert.ok(inputs.every((i) => !i.readOnly), "value inputs editable while disarmed");
  assert.ok(banner.hidden, "armed banner hidden while disarmed");

  // Emit armed → inputs read-only + banner shown + apply disabled.
  fake.getSubCb()({ armed: true });
  assert.ok(inputs.every((i) => i.readOnly), "value inputs read-only while armed");
  assert.ok(!banner.hidden, "armed banner shown while armed");
  assert.ok(applies.every((b) => b.disabled), "apply buttons disabled while armed");

  // Disarm → editable again.
  fake.getSubCb()({ armed: false });
  assert.ok(inputs.every((i) => !i.readOnly), "value inputs editable again after disarm");
  assert.ok(banner.hidden, "armed banner hidden after disarm");
}

// ===========================================================================
// PART E — Param set (apply + non-numeric rejection)
// ===========================================================================

async function testParamSetAppliesValidValue() {
  const fake = makeFakeTelemetry({ state: { armed: false } });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[1], "click");
  fire(findOneByClass(container, "params-download-btn"), "click");
  await flushMicrotasks();

  fake.setParamsResponse({
    complete: true, received: 1, count: 1, state: "complete",
    params: [{ name: "MC_ROLL_P", value: 6.5, type: 9 }],
  });
  eventSources[0].emit("progress", { state: "complete", received: 1, count: 1 });
  await flushMicrotasks();
  await flushMicrotasks();

  const row = findByClass(container, "params-row")[0];
  const input = findOneByClass(row, "params-value");
  const apply = findOneByClass(row, "params-apply");
  const status = findOneByClass(row, "params-row-status");

  // Initially: value matches original → Apply disabled.
  assert.equal(apply.disabled, true, "Apply disabled when value unchanged");

  // Change to a valid number → Apply enabled.
  input.value = "8";
  fire(input, "input");
  assert.equal(apply.disabled, false, "Apply enabled for a valid changed value");
  assert.equal(input.classList.contains("invalid"), false, "no invalid state for numeric value");

  // Apply → POST /api/params/set {name, value:8}.
  fire(apply, "click");
  await flushMicrotasks();
  const setCall = fake.postCalls.find((c) => c.url === "/api/params/set");
  assert.ok(setCall, "POST /api/params/set issued on Apply");
  assert.deepEqual(setCall.payload, { name: "MC_ROLL_P", value: 8 }, "set payload is {name, value:number}");
  assert.equal(status.textContent, "saved", "row shows saved status");
  assert.equal(apply.disabled, true, "Apply disabled again after a successful save (value matches)");
}

async function testParamSetRejectsNonNumeric() {
  const fake = makeFakeTelemetry({ state: { armed: false } });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[1], "click");
  fire(findOneByClass(container, "params-download-btn"), "click");
  await flushMicrotasks();

  fake.setParamsResponse({
    complete: true, received: 1, count: 1, state: "complete",
    params: [{ name: "MC_ROLL_P", value: 6.5, type: 9 }],
  });
  eventSources[0].emit("progress", { state: "complete", received: 1, count: 1 });
  await flushMicrotasks();
  await flushMicrotasks();

  const row = findByClass(container, "params-row")[0];
  const input = findOneByClass(row, "params-value");
  const apply = findOneByClass(row, "params-apply");

  // Enter a non-numeric value → invalid state + Apply disabled.
  input.value = "abc";
  fire(input, "input");
  assert.equal(input.classList.contains("invalid"), true, "non-numeric value flagged invalid");
  assert.equal(apply.disabled, true, "Apply disabled for non-numeric value");

  // Clicking Apply anyway → no set POST (client-side rejection).
  const before = fake.postCalls.filter((c) => c.url === "/api/params/set").length;
  fire(apply, "click");
  await flushMicrotasks();
  const after = fake.postCalls.filter((c) => c.url === "/api/params/set").length;
  assert.equal(after, before, "no /api/params/set POST for a non-numeric value");
}

async function testParamSetFailureShowsError() {
  const fake = makeFakeTelemetry({ state: { armed: false }, setReject: "cannot set parameter while armed" });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[1], "click");
  fire(findOneByClass(container, "params-download-btn"), "click");
  await flushMicrotasks();

  fake.setParamsResponse({
    complete: true, received: 1, count: 1, state: "complete",
    params: [{ name: "MC_ROLL_P", value: 6.5, type: 9 }],
  });
  eventSources[0].emit("progress", { state: "complete", received: 1, count: 1 });
  await flushMicrotasks();
  await flushMicrotasks();

  const row = findByClass(container, "params-row")[0];
  const input = findOneByClass(row, "params-value");
  const apply = findOneByClass(row, "params-apply");
  const status = findOneByClass(row, "params-row-status");

  input.value = "8";
  fire(input, "input");
  fire(apply, "click");
  await flushMicrotasks();
  assert.equal(status.textContent, "cannot set parameter while armed", "row shows the backend error");
  assert.equal(status.classList.contains("err"), true, "error status flagged");
  assert.equal(apply.disabled, false, "Apply re-enabled so the operator can retry");
}

// ===========================================================================
// PART D/H — Teardown / no-leak (params SSE + poll + telemetry unsub)
// ===========================================================================

async function testParametersTeardownClosesSseAndPoll() {
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;
  clearedIds.clear();

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[1], "click");   // Parameters
  fire(findOneByClass(container, "params-download-btn"), "click");
  await flushMicrotasks();

  assert.equal(eventSources.length, 1, "SSE open during download");
  const sse = eventSources[0];
  const pollId = intervalCbs[0].id;
  assert.equal(sse.closed, false, "SSE open before teardown");
  assert.equal(fake.unsubCalls, 1, "grid unsub fired on openView");

  // Leave the parameters sub-page (back) → teardown closes SSE, clears poll, unsubs.
  fire(findOneByClass(container, "setup-back"), "click");
  assert.equal(sse.closed, true, "params-progress SSE closed on teardown");
  assert.ok(clearedIds.has(pollId), "params poll timer cleared on teardown");
  assert.equal(fake.unsubCalls, 2, "grid + params unsub on back");
  assert.ok(findOneByClass(container, "setup-tiles"), "tile grid restored after back");
}

async function testParametersTeardownOnReRender() {
  // Re-rendering Setup while the params sub-page is open must tear it down too.
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[1], "click");
  fire(findOneByClass(container, "params-download-btn"), "click");
  await flushMicrotasks();
  const sse = eventSources[0];
  assert.equal(sse.closed, false);

  Corvus.setup.render(container);   // left-nav re-entry
  assert.equal(sse.closed, true, "SSE closed on re-render");
  assert.equal(fake.unsubCalls, 2, "unsub on re-render");
  assert.ok(findOneByClass(container, "setup-tiles"), "tiles restored on re-render");
}

// ===========================================================================
// PART G — Firmware sub-page (Corvus.setupFirmware)
// ===========================================================================
//
// The firmware page flashes PX4 firmware over a direct USB connection only.
// It renders a Connection card (transport + the USB-only gate banner + an
// armed banner) and a Firmware File card (file input + Upload + progress +
// flash log). The backend gate (can_flash) drives the UI; live telemetry's
// armed flag is overlaid. Upload POSTs the raw binary via fetch and opens a
// /api/firmware/progress SSE; terminal events fire corvus:notifications.

/** Render the firmware sub-page directly with a controllable status payload. */
async function renderFirmware(opts = {}) {
  const status = opts.status || {
    can_flash: true, transport: "usb", state: "idle",
    device: "/dev/ttyACM0", armed: false, progress: 0, message: "",
  };
  const fake = makeFakeTelemetry({ state: opts.telemetryState || { armed: false, connected: true } });
  fake.setParamsResponse(status);
  if (opts.catalog !== undefined) fake.setResponse("/api/firmware/catalog", opts.catalog);
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  resetFetch();
  eventSources.length = 0;
  const navigateBack = opts.navigateBack || (() => {});
  const destroy = Corvus.setupFirmware.render(container, navigateBack);
  await flushMicrotasks(); // refreshStatus().then(applyStatus) resolves
  return { container, destroy, fake, navigateBack };
}

/** Switch the firmware page between the catalogue and the local-file source. */
function selectFirmwareSource(container, id) {
  const btn = findByClass(container, "firmware-source-btn")
    .find((b) => b.dataset.source === id);
  assert.ok(btn, "firmware source button " + id + " present");
  fire(btn, "click");
  return btn;
}

/** Select a file in the firmware file input and return the chosen file name. */
function selectFirmwareFile(container, name) {
  selectFirmwareSource(container, "file");
  const fileInput = findOneByClass(container, "firmware-file-input");
  fileInput.files = [{ name: name || "px4_fmu-v5.px4", size: 1024 }];
  fire(fileInput, "change");
  return fileInput;
}

/** Upload (open the progress SSE). Assumes a file is selected and the gate is open. */
async function performUpload(container) {
  resetFetch();
  eventSources.length = 0;
  fire(findByClass(container, "params-download-btn")[0], "click");
  await flushMicrotasks();
  await flushMicrotasks();
  await flushMicrotasks();
  return eventSources[eventSources.length - 1];
}

async function testFirmwareSubPageRendersCardsAndControls() {
  const { container } = await renderFirmware();
  // Back button.
  assert.ok(findOneByClass(container, "setup-back"), "back button rendered");
  // Connection + Firmware File section titles.
  const titleTexts = findByClass(container, "page-section-title").map((t) => t.textContent);
  assert.ok(titleTexts.includes("Connection"), "Connection card present");
  assert.ok(titleTexts.includes("Firmware"), "Firmware card present");
  // File input + Upload button.
  assert.ok(findOneByClass(container, "firmware-file-input"), "file input rendered");
  const uploadBtn = findByClass(container, "params-download-btn")[0];
  assert.ok(uploadBtn, "Upload (Flash Firmware) button rendered");
  // Upload starts disabled (no file selected yet, even with the gate open).
  assert.equal(uploadBtn.disabled, true, "Upload disabled until a file is selected");
}

async function testFirmwareUsbGateDisablesUploadAndShowsBanner() {
  const { container } = await renderFirmware({
    status: { can_flash: false, transport: "sik", state: "idle",
      device: "/dev/ttyUSB0", armed: false, progress: 0, message: "" },
  });
  const banners = findByClass(container, "params-banner");
  const gateBanner = banners[0];
  assert.ok(!gateBanner.hidden, "gate banner shown when not can_flash");
  assert.ok(/USB/.test(gateBanner.textContent), "gate banner mentions USB");
  const uploadBtn = findByClass(container, "params-download-btn")[0];
  assert.equal(uploadBtn.disabled, true, "Upload disabled over a non-USB link");
}

async function testFirmwareUsbAllowedHidesBannerAndEnablesUploadAfterFile() {
  const { container } = await renderFirmware({
    status: { can_flash: true, transport: "usb", state: "idle",
      device: "/dev/ttyACM0", armed: false, progress: 0, message: "" },
  });
  const banners = findByClass(container, "params-banner");
  const gateBanner = banners[0];
  assert.ok(gateBanner.hidden, "gate banner hidden over direct USB");
  const uploadBtn = findByClass(container, "params-download-btn")[0];
  assert.equal(uploadBtn.disabled, true, "Upload disabled before a file is selected");
  selectFirmwareFile(container);
  assert.equal(uploadBtn.disabled, false, "Upload enabled after selecting a file over USB");
}

const FAKE_CATALOG = {
  dir: "/home/pilot/.corvus/firmware",
  error: "",
  cached: [{ name: "px4_fmu-v6x_default.px4", size: 2048, label: "Pixhawk 6X (FMUv6X)" }],
  releases: [
    {
      tag: "v1.18.0-beta1", name: "v1.18.0-beta1", prerelease: true, boards: [
        { name: "px4_fmu-v6x_default.px4", label: "Pixhawk 6X (FMUv6X)", size: 2048, cached: false },
      ],
    },
    {
      tag: "v1.17.0", name: "v1.17.0", prerelease: false, boards: [
        { name: "px4_fmu-v6x_default.px4", label: "Pixhawk 6X (FMUv6X)", size: 2048, cached: true },
        { name: "cubepilot_cubeorange_default.px4", label: "Cube Orange", size: 2048, cached: false },
      ],
    },
  ],
};

function firmwareSelects(container) {
  const selects = findByTag(container, "select");
  return { release: selects[0], board: selects[1] };
}

async function testFirmwareCatalogDefaultsToTheNewestStableRelease() {
  const { container } = await renderFirmware({ catalog: FAKE_CATALOG });
  await flushMicrotasks();

  const { release, board } = firmwareSelects(container);
  // A pre-release is a deliberate choice. Landing on one by not choosing is
  // how an operator flashes beta firmware onto an aircraft by accident.
  assert.equal(release.value, "v1.17.0", "newest stable release preselected");
  assert.ok(board.children.length >= 2, "the release's boards are listed");
  // Cached images flash with no network; the list has to say which ones those are.
  const cached = board.children.find((o) => o.value === "px4_fmu-v6x_default.px4");
  assert.match(cached.textContent, /downloaded/, "a cached image is marked");
}

async function testFirmwareBoardFilterNarrowsTheList() {
  const { container } = await renderFirmware({ catalog: FAKE_CATALOG });
  await flushMicrotasks();

  const { board } = firmwareSelects(container);
  assert.equal(board.children.length, 2, "both boards before filtering");
  // PX4 ships ~150 targets per release, so the filter is the only way the
  // list is usable at all.
  const filter = findByClass(container, "field-input")
    .find((i) => i.placeholder === "Filter boards…");
  filter.value = "cube";
  fire(filter, "input");
  assert.equal(board.children.length, 1, "filter narrows the board list");
  assert.equal(board.children[0].value, "cubepilot_cubeorange_default.px4");
}

async function testFirmwareFlashPostsReleaseAndBoardNotAUrl() {
  const { container, fake } = await renderFirmware({ catalog: FAKE_CATALOG });
  await flushMicrotasks();

  const uploadBtn = findByClass(container, "params-download-btn")[0];
  assert.equal(uploadBtn.disabled, false, "flash enabled once a board is selected");
  fire(uploadBtn, "click");
  await flushMicrotasks();

  const call = fake.postCalls.find((c) => c.url === "/api/firmware/flash");
  assert.ok(call, "POST /api/firmware/flash issued");
  assert.deepEqual(call.payload, {
    release: "v1.17.0", board: "px4_fmu-v6x_default.px4",
  }, "the request names a release and a board");
  // The browser must never choose what gets fetched — resolving the download
  // target is the backend's job.
  assert.ok(!("url" in call.payload), "no download URL comes from the frontend");
}

async function testFirmwareCatalogOfflineKeepsThePageUsable() {
  const { container } = await renderFirmware({
    catalog: { releases: [], cached: [], error: "could not reach the PX4 release server", dir: "" },
  });
  await flushMicrotasks();

  const note = findOneByClass(container, "firmware-catalog-note");
  assert.ok(note && /release/i.test(note.textContent), "the reason is stated");
  // With no catalogue the local-file path must still be reachable — that is
  // the whole offline story for this page.
  selectFirmwareSource(container, "file");
  assert.ok(!findOneByClass(container, "firmware-file-row").hidden,
    "the local file picker is still available offline");
}

async function testDownloadingCountsAsBusy() {
  const { container } = await renderFirmware({
    catalog: FAKE_CATALOG,
    status: { can_flash: false, transport: "usb", state: "downloading",
      device: "/dev/ttyACM0", armed: false, progress: 42, message: "Downloading…" },
  });
  await flushMicrotasks();

  const uploadBtn = findByClass(container, "params-download-btn")[0];
  assert.equal(uploadBtn.disabled, true, "no second job while a download runs");
  // A download is the longer half of the job and must be cancellable.
  assert.ok(!findOneByClass(container, "firmware-cancel").hidden,
    "cancel offered while downloading");
}

async function testFirmwareArmedGateDisablesUploadAndShowsBanner() {
  const { container } = await renderFirmware({
    status: { can_flash: false, transport: "usb", state: "idle",
      device: "/dev/ttyACM0", armed: true, progress: 0, message: "" },
    telemetryState: { armed: true, connected: true },
  });
  const banners = findByClass(container, "params-banner");
  const armedBanner = banners[1];
  assert.ok(!armedBanner.hidden, "armed banner shown while armed");
  assert.ok(/arm/i.test(armedBanner.textContent), "armed banner mentions arm");
  const uploadBtn = findByClass(container, "params-download-btn")[0];
  assert.equal(uploadBtn.disabled, true, "Upload disabled while armed");
}

async function testFirmwareUploadCallsFetchAndOpensSse() {
  const { container } = await renderFirmware();
  selectFirmwareFile(container);
  await performUpload(container);
  assert.ok(fetchCalls.length >= 1, "fetch called on Upload");
  const call = fetchCalls[0];
  assert.ok(call.url.indexOf("/api/firmware/upload") === 0, "fetch targets the upload endpoint");
  assert.equal(call.opts.method, "POST", "upload is a POST");
  assert.ok(eventSources.length >= 1, "progress SSE opened after upload");
  assert.ok(
    eventSources[eventSources.length - 1].url.indexOf("/api/firmware/progress") === 0,
    "SSE opened on the progress endpoint",
  );
}

async function testFirmwareProgressSseUpdatesBar() {
  const { container } = await renderFirmware();
  selectFirmwareFile(container);
  const sse = await performUpload(container);
  assert.ok(sse, "SSE open after upload");
  const fill = findOneByClass(container, "progress-bar-fill");
  sse.emit("progress", { state: "programming", percent: 42, message: "Programming…" });
  assert.equal(fill.style.width, "42%", "progress bar fill width set from the SSE percent");
}

async function testFirmwareDoneNotifiesInfo() {
  const { container, fake } = await renderFirmware();
  selectFirmwareFile(container);
  const sse = await performUpload(container);
  // The progress handler calls refreshStatus() on a terminal event; route the
  // status fetch to a "done" payload so applyStatus fires the info notification.
  fake.setParamsResponse({
    can_flash: true, transport: "usb", state: "done", device: "/dev/ttyACM0",
    armed: false, progress: 100, message: "Firmware flashed successfully",
  });
  dispatched.length = 0;
  sse.emit("progress", { state: "done", percent: 100, message: "Firmware flashed successfully" });
  await flushMicrotasks();
  await flushMicrotasks();
  const note = dispatched.find((e) => e.type === "corvus:notification");
  assert.ok(note, "a corvus:notification was dispatched on done");
  assert.equal(note.detail.level, "info", "done notifies at info level");
}

async function testFirmwareFailedNotifiesCritical() {
  const { container, fake } = await renderFirmware();
  selectFirmwareFile(container);
  const sse = await performUpload(container);
  fake.setParamsResponse({
    can_flash: false, transport: "usb", state: "failed", device: "/dev/ttyACM0",
    armed: false, progress: 50, message: "firmware CRC mismatch — not booting",
  });
  dispatched.length = 0;
  sse.emit("progress", { state: "failed", percent: 50, message: "CRC mismatch" });
  await flushMicrotasks();
  await flushMicrotasks();
  const note = dispatched.find((e) => e.type === "corvus:notification");
  assert.ok(note, "a corvus:notification was dispatched on failed");
  assert.equal(note.detail.level, "critical", "failed notifies at critical level");
}

async function testFirmwareDestroyClosesSseAndUnsubscribes() {
  const { container, destroy, fake } = await renderFirmware();
  selectFirmwareFile(container);
  const sse = await performUpload(container);
  assert.equal(sse.closed, false, "SSE open before destroy");
  const unsubBefore = fake.unsubCalls;
  destroy();
  assert.equal(sse.closed, true, "SSE closed on destroy");
  assert.equal(fake.unsubCalls, unsubBefore + 1, "telemetry unsub on destroy");
}

async function testFirmwareBackCallsDestroyNoLeak() {
  // End-to-end via the orchestrator: open Firmware from the grid, upload (open
  // the SSE), then click back — the orchestrator's teardown runs the firmware
  // destroy (SSE closed, telemetry unsub) and re-renders the grid. No leak.
  const fake = makeFakeTelemetry({ state: { armed: false, connected: true } });
  fake.setParamsResponse({
    can_flash: true, transport: "usb", state: "idle", device: "/dev/ttyACM0",
    armed: false, progress: 0, message: "",
  });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  resetFetch();
  eventSources.length = 0;
  Corvus.setup.render(container);
  // Open the Firmware sub-page (third tile).
  fire(findByClass(container, "setup-tile")[2], "click");
  await flushMicrotasks();
  selectFirmwareFile(container);
  const sse = await performUpload(container);
  assert.equal(sse.closed, false, "SSE open while on the firmware sub-page");
  const unsubBefore = fake.unsubCalls;
  // Back → orchestrator teardown runs the firmware destroy, then re-renders grid.
  fire(findOneByClass(container, "setup-back"), "click");
  assert.equal(sse.closed, true, "SSE closed on back (destroy ran)");
  assert.equal(fake.unsubCalls, unsubBefore + 1, "firmware telemetry unsub on back");
  assert.ok(findOneByClass(container, "setup-tiles"), "tile grid restored after back");
}

// ===========================================================================
// Run all tests.
// ===========================================================================
/**
 * Tear down any sub-page a previous test left open, with Plotly undefined so a
 * leftover teardown's Plotly.purge is skipped (no purge-spy pollution between
 * tests). The setup module keeps the active sub-page's teardown in module-level
 * state, so this guarantees each test starts at a clean tile grid.
 */
function resetSetup() {
  delete window.Plotly;
  pageViewEl = makeEl("div");
  Corvus.setup.render(pageViewEl);
}

async function withReset(fn) {
  resetSetup();
  return fn();
}

async function run() {
  await withReset(testTileGridRendersTwoTiles);
  await withReset(testClickTileSwapsToSubPageAndBackReturns);
  await withReset(testTeardownRunsOnSwap);
  await withReset(testReRenderTearsDownActiveSubPage);
  await withReset(testVehicleInfoUpdatesLive);

  await withReset(testCalibrationCardsCoverEveryProcedure);
  await withReset(testCalibrationArmedGating);
  await withReset(testReadinessStripReflectsLinkAndArmedState);

  await withReset(testAutotuneButtonsMapToAxes);
  await withReset(testAutotuneArmedGating);

  await withReset(testAutotuneGraphsReactAndBuffer);
  await withReset(testReducedMotionZeroDurationTransition);

  await withReset(testParametersDoesNotAutoDownload);
  await withReset(testParametersDownloadFlowGating);
  await withReset(testParametersArmedGatingReadOnly);

  await withReset(testParamSetAppliesValidValue);
  await withReset(testParamSetRejectsNonNumeric);
  await withReset(testParamSetFailureShowsError);

  await withReset(testParametersTeardownClosesSseAndPoll);
  await withReset(testParametersTeardownOnReRender);

  await withReset(testFirmwareSubPageRendersCardsAndControls);
  await withReset(testFirmwareUsbGateDisablesUploadAndShowsBanner);
  await withReset(testFirmwareUsbAllowedHidesBannerAndEnablesUploadAfterFile);
  await withReset(testFirmwareCatalogDefaultsToTheNewestStableRelease);
  await withReset(testFirmwareBoardFilterNarrowsTheList);
  await withReset(testFirmwareFlashPostsReleaseAndBoardNotAUrl);
  await withReset(testFirmwareCatalogOfflineKeepsThePageUsable);
  await withReset(testDownloadingCountsAsBusy);
  await withReset(testFirmwareArmedGateDisablesUploadAndShowsBanner);
  await withReset(testFirmwareUploadCallsFetchAndOpensSse);
  await withReset(testFirmwareProgressSseUpdatesBar);
  await withReset(testFirmwareDoneNotifiesInfo);
  await withReset(testFirmwareFailedNotifiesCritical);
  await withReset(testFirmwareDestroyClosesSseAndUnsubscribes);
  await withReset(testFirmwareBackCallsDestroyNoLeak);

  // Let any best-effort microtasks drain so the process exits cleanly.
  await flushMicrotasks();
  console.log("frontend setup tests passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
