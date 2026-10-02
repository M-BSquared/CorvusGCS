"use strict";

/**
 * Frontend tests for the Vibration Monitor plugin (plugins/vibration): the
 * sample buffer, the redraw throttle, Plotly loaded on demand, reduced
 * motion, and a clean teardown.
 *
 * Run:
 *   node plugins/vibration/tests/test_vibration.js
 */
const assert = require("node:assert/strict");
const path = require("node:path");

// The Corvus checkout whose src/ this plugin runs against: the one around
// plugins/vibration/, unless CORVUS_ROOT names another (the plugin kept in its own
// repository, say). tools/frontend_tests.js sets it.
const CORVUS = process.env.CORVUS_ROOT
  ? path.resolve(process.env.CORVUS_ROOT)
  : path.join(__dirname, "..", "..", "..");

// ---------------------------------------------------------------------------
// Browser-ish globals so plugins.js and the folder plugins load and run in Node.
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
// api.notification dispatches corvus:notification for the topbar's board, so
// the stub has to deliver it — without one, the call was silently swallowed by
// the guard around it and the board half of a notification went untested.
window.dispatchEvent = (event) => {
  (windowListeners[event && event.type] || []).forEach((cb) => cb(event));
  return true;
};


global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};

// prefers-reduced-motion toggle (api.reducedMotion reads this).
let reducedMotion = false;
window.matchMedia = (query) => ({
  matches: reducedMotion && String(query).includes("prefers-reduced-motion"),
  media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});

// Controlled clock for the throttle test.
let clock = 1000;
Date.now = () => clock;

// lucide is optional in the source (refreshIcons no-ops when absent). Leave it
// undefined so we also exercise that path.
// window.Plotly is set per-test below.

// ---------------------------------------------------------------------------
// Minimal DOM stub. The source builds structure with createElement + appendChild
// (no innerHTML-based querySelector), so this stub only needs to track
// children, className/classList, textContent, dataset, listeners, and an
// innerHTML string (cleared children when set to "").
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
  e.append = (...nodes) => { nodes.forEach((n) => e.appendChild(n)); };
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

// <head>, plus the loader's script/link elements. Corvus.plugins.loadInstalled
// appends a <script> and waits for its onload, which no stub can produce by
// actually fetching — so appending one here fires the callback the appended
// element was given, and `scriptBehaviour` decides whether that is a load or an
// error. `appendedScripts` is what the assertions read.
const appendedScripts = [];
const appendedStyles = [];
let scriptBehaviour = () => "load";

const head = makeEl("head");
head.appendChild = (el) => {
  el.parentNode = head;
  head.children.push(el);
  if (el.tagName === "SCRIPT") {
    appendedScripts.push(el.src);
    const outcome = scriptBehaviour(el.src);
    setTimeout(() => {
      if (outcome === "error") { if (el.onerror) el.onerror(new Error("failed")); return; }
      if (typeof outcome === "function") outcome(el.src);
      if (el.onload) el.onload();
    }, 0);
  } else if (el.tagName === "LINK") {
    appendedStyles.push(el.href);
  }
  return el;
};

// The toast stack mounts here, so the push half of api.notification is real
// in these tests rather than swallowed by the guard around it.
const body = makeEl("body");

global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  head,
  body,
};

// The launcher asks before removing a button that is running something.
window.confirm = () => true;

// Flush the microtask queue (for the plugin's best-effort postAction promises).
function flushMicrotasks() { return new Promise((r) => setTimeout(r, 0)); }

// ---------------------------------------------------------------------------
// Load order mirrors the browser's: plugins.js (a <script> tag in index.html)
// first, then the folder plugins, which register themselves at load.
// ---------------------------------------------------------------------------
// ui.js first: it defines Corvus.ui, the component layer every other
// module builds its DOM with (index.html loads it in the same order).
require(path.join(CORVUS, "src", "js", "ui.js"));
require(path.join(CORVUS, "src", "js", "plugins.js"));
// A folder plugin: in the browser its script is appended by loadInstalled
// rather than by a tag in index.html. Requiring it here runs the same file
// the same way, and it registers itself as it loads.
require("../vibration.js");

// ---------------------------------------------------------------------------
// The Vibration Monitor
// ---------------------------------------------------------------------------

function fakePlotly() {
  return {
    reactCalls: [],
    purgeCalls: [],
    react(gd, data, layout, config) { this.reactCalls.push({ gd, data, layout, config }); },
    purge(gd) { this.purgeCalls.push(gd); },
  };
}

function makeVibApi(opts = {}) {
  // Capture the telemetry callback the plugin registers, and expose an unsub
  // spy, so tests can drive the subscriber and assert on teardown.
  const captured = { cb: null, unsubCalls: 0 };
  const unsub = opts.unsub || (() => { captured.unsubCalls++; });
  const subscribe = opts.subscribe || ((fn) => { captured.cb = fn; return unsub; });
  return {
    api: captured,
    telemetry: { subscribe, getState() { return null; }, requestJson() { return Promise.resolve({}); }, postAction: opts.postAction || (() => Promise.resolve({ ok: true })) },
    subscribe,
    getState() { return null; },
    requestJson() { return Promise.resolve({}); },
    postAction: opts.postAction || (() => Promise.resolve({ ok: true })),
    reducedMotion: opts.reducedMotion || (() => false),
  };
}

function testUpdateBufferOnlyOnChange() {
  const buf = { t: [], vx: [], vy: [], vz: [] };
  const s1 = { vibration_x: 0.1, vibration_y: 0.2, vibration_z: 0.3 };
  // First point always appends (buffer empty).
  assert.equal(Corvus.pluginVibration.updateBuffer(buf, s1, 0), true);
  assert.equal(buf.vx.length, 1);
  // Identical vibration values → no append.
  assert.equal(Corvus.pluginVibration.updateBuffer(buf, s1, 1), false);
  assert.equal(buf.vx.length, 1, "identical vibration does not grow the buffer");
  // Changed vibration_z → grows by exactly one.
  const s2 = { vibration_x: 0.1, vibration_y: 0.2, vibration_z: 0.9 };
  assert.equal(Corvus.pluginVibration.updateBuffer(buf, s2, 2), true);
  assert.equal(buf.vx.length, 2, "changed vibration grows the buffer by one");
  assert.equal(buf.vz[1], 0.9);
  assert.equal(buf.t[1], 2);
}

function testUpdateBufferCapsMemory() {
  const buf = { t: [], vx: [], vy: [], vz: [] };
  const max = Corvus.pluginVibration.MAX_POINTS;
  for (let i = 0; i < max + 50; i++) {
    Corvus.pluginVibration.updateBuffer(buf, { vibration_x: i, vibration_y: 0, vibration_z: 0 }, i);
  }
  assert.equal(buf.vx.length, max, "buffer capped at MAX_POINTS");
  assert.equal(buf.vx[0], 50, "oldest points dropped (FIFO)");
}

function testThrottleRedrawsAtMostOncePerWindow() {
  const plot = fakePlotly();
  window.Plotly = plot;
  const api = makeVibApi();
  const container = makeEl("div");

  clock = 1000;
  Corvus.pluginVibration.init(container, api);
  // init performs one initial redraw.
  assert.equal(plot.reactCalls.length, 1, "initial redraw on init");

  const cb = api.api.cb;
  assert.ok(typeof cb === "function", "subscribe captured the telemetry callback");

  // Feed many rapid updates at the SAME clock (within the throttle window).
  for (let i = 0; i < 50; i++) {
    cb({ vibration_x: i, vibration_y: 0, vibration_z: 0, clipping_0: 0, clipping_1: 0, clipping_2: 0 });
  }
  assert.equal(plot.reactCalls.length, 1, "no redraw within the ~100ms window despite many updates");

  // Advance past the throttle window and feed one update → exactly one redraw.
  clock = 1000 + Corvus.pluginVibration.REDRAW_MIN_MS;
  cb({ vibration_x: 99, vibration_y: 0, vibration_z: 0, clipping_0: 0, clipping_1: 0, clipping_2: 0 });
  assert.equal(plot.reactCalls.length, 2, "one redraw after the throttle window elapses");
  // Plotly.react skips data it has already seen, so each draw needs new arrays.
  const [first, second] = plot.reactCalls;
  assert.notStrictEqual(second.data[0].x, first.data[0].x, "each redraw hands Plotly a fresh x array");
  assert.notStrictEqual(second.data[2].y, first.data[2].y, "each redraw hands Plotly a fresh y array");
  assert.equal(first.data[0].x.length, 0, "an earlier draw's arrays are not mutated afterwards");
  assert.ok(second.data[0].x.length > 0, "the new draw carries the buffered points");

  // Many more rapid updates at the new clock → no further redraw.
  for (let i = 0; i < 50; i++) {
    cb({ vibration_x: 100 + i, vibration_y: 0, vibration_z: 0, clipping_0: 0, clipping_1: 0, clipping_2: 0 });
  }
  assert.equal(plot.reactCalls.length, 2, "at most one redraw per throttle window");

  Corvus.pluginVibration.destroy(container);
  delete window.Plotly;
}

function testDestroyPurgesAndUnsubscribes() {
  const plot = fakePlotly();
  window.Plotly = plot;
  const api = makeVibApi();
  const container = makeEl("div");

  clock = 5000;
  Corvus.pluginVibration.init(container, api);
  assert.equal(plot.purgeCalls.length, 0, "no purge before destroy");
  assert.equal(api.api.unsubCalls, 0, "no unsubscribe before destroy");

  Corvus.pluginVibration.destroy(container);
  assert.equal(plot.purgeCalls.length, 1, "Plotly.purge called exactly once on destroy");
  assert.equal(api.api.unsubCalls, 1, "telemetry unsubscribe called exactly once on destroy");

  // destroy is idempotent — safe to call again (no double purge / unsub).
  Corvus.pluginVibration.destroy(container);
  assert.equal(plot.purgeCalls.length, 1, "destroy is idempotent");
  assert.equal(api.api.unsubCalls, 1);
  delete window.Plotly;
}

function testReducedMotionZeroDurationTransition() {
  reducedMotion = true;
  const plot = fakePlotly();
  window.Plotly = plot;
  const api = makeVibApi({ reducedMotion: () => true });
  const container = makeEl("div");
  Corvus.pluginVibration.init(container, api);

  assert.ok(plot.reactCalls.length >= 1, "at least one react on init");
  const cfg = plot.reactCalls[plot.reactCalls.length - 1].config;
  assert.ok(cfg && cfg.transition && cfg.transition.duration === 0,
    "reduced-motion config includes transition.duration === 0");
  Corvus.pluginVibration.destroy(container);
  delete window.Plotly;
  reducedMotion = false;
}

function testNonReducedMotionHasNoZeroTransition() {
  const plot = fakePlotly();
  window.Plotly = plot;
  const api = makeVibApi({ reducedMotion: () => false });
  const container = makeEl("div");
  Corvus.pluginVibration.init(container, api);
  const cfg = plot.reactCalls[plot.reactCalls.length - 1].config;
  assert.ok(!cfg.transition, "no transition set when reduced motion is off");
  Corvus.pluginVibration.destroy(container);
  delete window.Plotly;
}

function testPlotlyMissingFallbackDoesNotSubscribe() {
  delete window.Plotly;
  const api = makeVibApi();
  const container = makeEl("div");

  Corvus.pluginVibration.init(container, api);
  assert.ok(container.innerHTML.indexOf("Plotly not available") >= 0,
    "fallback message rendered when Plotly is missing");
  assert.equal(api.api.cb, null, "no subscription when Plotly is missing");

  // destroy must be safe even though init bailed early.
  assert.doesNotThrow(() => Corvus.pluginVibration.destroy(container));
}

async function testPlotlyIsLoadedOnDemandBeforeMounting() {
  delete window.Plotly;
  const plot = fakePlotly();
  let loads = 0;
  Corvus.lazy = { plotly: () => { loads += 1; window.Plotly = plot; return Promise.resolve(plot); } };
  const api = makeVibApi();
  const container = makeEl("div");

  Corvus.pluginVibration.init(container, api);
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(loads, 1, "the plugin asks the app to load Plotly");
  assert.ok(container.innerHTML.indexOf("Plotly not available") < 0,
    "no missing-Plotly message once the loader delivered it");
  assert.ok(typeof api.api.cb === "function", "subscribed once Plotly arrived");

  Corvus.pluginVibration.destroy(container);
  delete Corvus.lazy;
  delete window.Plotly;
}

async function testDestroyWhilePlotlyLoadsNeverMounts() {
  delete window.Plotly;
  let release;
  Corvus.lazy = { plotly: () => new Promise((r) => { release = r; }) };
  const api = makeVibApi();
  const container = makeEl("div");

  Corvus.pluginVibration.init(container, api);
  Corvus.pluginVibration.destroy(container);
  window.Plotly = fakePlotly();
  release(window.Plotly);
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(api.api.cb, null, "a plugin closed during the load does not subscribe");

  delete Corvus.lazy;
  delete window.Plotly;
}

function testRegisteredVibrationPlugin() {
  // vibration.js registered itself at load; verify it is in the grid.
  const found = Corvus.plugins.list().find((p) => p.id === "vibration");
  assert.ok(found, "vibration plugin registered at module load");
  assert.equal(found.name, "Vibration Monitor");
  assert.equal(found.icon, "activity");
  assert.equal(found.description, "Live PX4 vibration metrics and accelerometer clipping");
}


// ---------------------------------------------------------------------------
// Run all tests.
// ---------------------------------------------------------------------------
async function run() {
  testUpdateBufferOnlyOnChange();
  testUpdateBufferCapsMemory();
  testThrottleRedrawsAtMostOncePerWindow();
  testDestroyPurgesAndUnsubscribes();
  testReducedMotionZeroDurationTransition();
  testNonReducedMotionHasNoZeroTransition();
  testPlotlyMissingFallbackDoesNotSubscribe();
  await testPlotlyIsLoadedOnDemandBeforeMounting();
  await testDestroyWhilePlotlyLoadsNeverMounts();
  testRegisteredVibrationPlugin();

  await flushMicrotasks();

  console.log("vibration plugin tests passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
