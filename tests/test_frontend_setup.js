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
  e.appendChild = (c) => { e.children.push(c); return c; };
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); return c; };
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
  addEventListener: () => {},
};

// Helpers ---------------------------------------------------------------------
function flushMicrotasks() { return new Promise((r) => setTimeout(r, 0)); }

function findByClass(root, cls) { return root.querySelectorAll("." + cls); }
function findOneByClass(root, cls) { return root.querySelector("." + cls); }
function findByDataset(root, key, value) {
  return findByClass(root, "*").filter((e) => e.dataset && e.dataset[key] === value);
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
    setState(s) { state = s; },
  };
}

// ---------------------------------------------------------------------------
// Load the module under test (defines window.Corvus.setup). The setup page is
// split across four files: shared helpers, the calibration page, the
// parameters page, and the thin orchestrator. Load them in the same order as
// index.html so dependencies resolve.
// ---------------------------------------------------------------------------
require("../src/js/setup-shared.js");
require("../src/js/setup-calibration.js");
require("../src/js/setup-parameters.js");
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
  assert.equal(tiles.length, 2, "two tiles rendered");
  assert.equal(tiles[0].dataset.view, "calibration", "first tile is Calibration");
  assert.equal(tiles[1].dataset.view, "parameters", "second tile is Parameters");

  // Tile titles are real text nodes.
  const titles = tiles.map((t) => findOneByClass(t, "setup-tile-title").textContent);
  assert.deepEqual(titles, ["Calibration", "Parameters"]);
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
  assert.ok(findOneByClass(container, "calib-grid"), "calibration grid present");

  // Click back → tiles grid restored.
  const back = findOneByClass(container, "setup-back");
  fire(back, "click");
  assert.ok(findOneByClass(container, "setup-tiles"), "tile grid restored after back");
  assert.equal(findByClass(container, "calib-grid").length, 0, "calibration grid gone after back");
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
  assert.equal(fake.unsubCalls, 0, "no unsub before leaving");

  // Click back → teardown runs: unsub once, Plotly.purge exactly 3.
  fire(findOneByClass(container, "setup-back"), "click");
  assert.equal(fake.unsubCalls, 1, "telemetry unsub called exactly once on back");
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
  assert.equal(fake.unsubCalls, 1, "unsub on re-render");
  assert.equal(plot.purgeCalls.length, 3, "purge on re-render");
  assert.ok(findOneByClass(container, "setup-tiles"), "tile grid restored on re-render");
  delete window.Plotly;
}

// ===========================================================================
// PART B — Calibration
// ===========================================================================

async function testCalibrationButtonsMapToTypes() {
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[0], "click");   // Calibration

  const grid = findOneByClass(container, "calib-grid");
  const btns = findByClass(grid, "calib-btn");
  assert.equal(btns.length, 6, "six sensor-calibration buttons");

  const expected = {
    compass: "Compass", gyro: "Gyroscope", accel: "Accelerometer",
    level: "Level Horizon", airspeed: "Airspeed", baro: "Baro",
  };
  for (const btn of btns) {
    const type = btn.dataset.type;
    assert.ok(expected[type], `button type ${type} is one of the known types`);
    assert.equal(findOneByClass(btn, "calib-btn-label").textContent, expected[type]);
  }

  // Clicking "Compass" → POST /api/calibrate {type:"compass"}.
  const compassBtn = btns.find((b) => b.dataset.type === "compass");
  fire(compassBtn, "click");
  await flushMicrotasks();
  const cal = fake.postCalls.find((c) => c.url === "/api/calibrate");
  assert.ok(cal, "POST /api/calibrate was issued");
  assert.deepEqual(cal.payload, { type: "compass" }, "compass maps to {type:'compass'}");

  // Each of the six maps to its own type.
  for (const type of Object.keys(expected)) {
    const btn = btns.find((b) => b.dataset.type === type);
    fake.postCalls.length = 0;
    fire(btn, "click");
    await flushMicrotasks();
    const call = fake.postCalls.find((c) => c.url === "/api/calibrate");
    assert.deepEqual(call.payload, { type }, `button ${type} posts {type:'${type}'}`);
  }
}

async function testCalibrationArmedGating() {
  const fake = makeFakeTelemetry({ state: { armed: false, connected: true, warnings: [] } });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[0], "click");   // Calibration
  const grid = findOneByClass(container, "calib-grid");
  const btns = findByClass(grid, "calib-btn");

  // Initial state disarmed → buttons enabled.
  assert.ok(btns.every((b) => !b.disabled), "buttons enabled while disarmed");

  // Emit armed → all disabled + banner visible.
  fake.getSubCb()({ armed: true, connected: true, warnings: [] });
  assert.ok(btns.every((b) => b.disabled), "buttons disabled while armed");
  const banner = findByClass(container, "setup-armed-banner")[0];
  assert.ok(banner && !banner.hidden, "armed banner shown while armed");

  // Emit disarmed → re-enabled + banner hidden.
  fake.getSubCb()({ armed: false, connected: true, warnings: [] });
  assert.ok(btns.every((b) => !b.disabled), "buttons re-enabled when disarmed");
  assert.ok(banner.hidden, "armed banner hidden when disarmed");
}

async function testCalibrationGuidanceRendersStatustext() {
  const fake = makeFakeTelemetry({ state: { armed: false, warnings: [] } });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  clock = 1000;

  Corvus.setup.render(container);
  fire(findByClass(container, "setup-tile")[0], "click");

  const guidance = findOneByClass(container, "guidance-list");
  // Initially empty prompt.
  assert.ok(findOneByClass(guidance, "guidance-empty"), "empty guidance prompt before STATUSTEXT");

  // Emit warnings (STATUSTEXT) → rendered as guidance lines.
  fake.getSubCb()({
    armed: false, connected: true,
    warnings: [
      { level: "notice", msg: "Rotate around all axes", meta: "step 1" },
      { level: "info", msg: "Compass calibrated", meta: "" },
    ],
  });
  const lines = findByClass(guidance, "guidance-line");
  assert.equal(lines.length, 2, "two guidance lines rendered from warnings");
  assert.equal(findOneByClass(lines[0], "guidance-msg").textContent, "Rotate around all axes");
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
  assert.equal(fake.unsubCalls, 0, "no unsub before teardown");

  // Leave the parameters sub-page (back) → teardown closes SSE, clears poll, unsubs.
  fire(findOneByClass(container, "setup-back"), "click");
  assert.equal(sse.closed, true, "params-progress SSE closed on teardown");
  assert.ok(clearedIds.has(pollId), "params poll timer cleared on teardown");
  assert.equal(fake.unsubCalls, 1, "telemetry unsub called on teardown");
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
  assert.equal(fake.unsubCalls, 1, "unsub on re-render");
  assert.ok(findOneByClass(container, "setup-tiles"), "tiles restored on re-render");
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

  await withReset(testCalibrationButtonsMapToTypes);
  await withReset(testCalibrationArmedGating);
  await withReset(testCalibrationGuidanceRendersStatustext);

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

  // Let any best-effort microtasks drain so the process exits cleanly.
  await flushMicrotasks();
  console.log("frontend setup tests passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
