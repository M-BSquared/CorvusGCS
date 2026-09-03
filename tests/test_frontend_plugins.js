"use strict";

/**
 * Frontend tests for the TOOLS-tab plugin registry (Corvus.plugins) and the
 * Vibration Monitor plugin (Corvus.pluginVibration).
 *
 * Plain Node-runnable assertions (no browser, no test runner) following the
 * same pattern as tests/test_frontend_link.js and tests/frontend_telemetry.test.js:
 * stub the globals the modules touch, require the source, and assert on the
 * pure functions and the registry/plugin lifecycle. A small DOM stub (no HTML
 * parser) is used because the source builds its DOM via createElement, so a
 * children-walking stub suffices.
 *
 * Run:
 *   node tests/test_frontend_plugins.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals so plugins.js / plugin-vibration.js load and run in Node.
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

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

global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
};

// Flush the microtask queue (for the plugin's best-effort postAction promises).
function flushMicrotasks() { return new Promise((r) => setTimeout(r, 0)); }

// ---------------------------------------------------------------------------
// Load order mirrors src/index.html for the new modules: plugins.js first, then
// plugin-vibration.js (which registers itself at load).
// ---------------------------------------------------------------------------
require("../src/js/plugins.js");
require("../src/js/plugin-vibration.js");

// ---------------------------------------------------------------------------
// PART A: plugin registry
// ---------------------------------------------------------------------------

function makeSpyPlugin(id, name) {
  const spy = {
    id,
    initCalls: 0,
    initArgs: [],
    destroyCalls: 0,
    destroyArgs: [],
    init(containerEl, api) { spy.initCalls++; spy.initArgs.push({ containerEl, api }); },
    destroy(containerEl) { spy.destroyCalls++; spy.destroyArgs.push(containerEl); },
  };
  return spy;
}

function testRegisterAndList() {
  const ok = Corvus.plugins.register("t-list", {
    name: "Lister", icon: "star", description: "lists things",
    init() {}, destroy() {},
  });
  assert.equal(ok, true, "register returns true for a new id");
  const found = Corvus.plugins.list().find((p) => p.id === "t-list");
  assert.ok(found, "registered plugin appears in list()");
  assert.equal(found.name, "Lister");
  assert.equal(found.icon, "star");
  assert.equal(found.description, "lists things");
}

function testDuplicateRegisterRejected() {
  // Duplicate id is rejected by returning false (documented contract), not by
  // throwing, so a misbehaving plugin module never breaks app init.
  const first = Corvus.plugins.register("t-dup", { name: "A", icon: "i", description: "d", init() {}, destroy() {} });
  const second = Corvus.plugins.register("t-dup", { name: "B", icon: "i", description: "d", init() {}, destroy() {} });
  assert.equal(first, true);
  assert.equal(second, false, "duplicate id is rejected (returns false)");
  const entries = Corvus.plugins.list().filter((p) => p.id === "t-dup");
  assert.equal(entries.length, 1, "only the first registration is kept");
  assert.equal(entries[0].name, "A");
}

function testOpenCloseLifecycle() {
  const root = makeEl("div");
  const telemetry = { subscribe() { return () => {}; }, getState() { return null; }, requestJson() { return Promise.resolve({}); }, postAction() { return Promise.resolve({ ok: true }); } };
  Corvus.plugins.init(root, telemetry);

  const spy = makeSpyPlugin("t-lifecycle", "Lifecycle");
  Corvus.plugins.register("t-lifecycle", { name: spy.id, icon: "i", description: "d", init: spy.init, destroy: spy.destroy });

  assert.equal(Corvus.plugins.getActive(), null, "nothing active before open");
  assert.equal(Corvus.plugins.open("t-lifecycle"), true);
  assert.equal(spy.initCalls, 1, "init called exactly once on open");
  assert.ok(spy.initArgs[0].containerEl, "init received a container element");
  assert.equal(spy.initArgs[0].containerEl.className, "plugin-container");
  assert.ok(spy.initArgs[0].api && typeof spy.initArgs[0].api.subscribe === "function", "init received the api");
  assert.equal(Corvus.plugins.getActive(), "t-lifecycle");
  assert.equal(root.children[0].className, "plugin-view", "grid replaced by plugin view on open");

  Corvus.plugins.close();
  assert.equal(spy.destroyCalls, 1, "destroy called exactly once on close");
  assert.equal(spy.destroyArgs[0], spy.initArgs[0].containerEl, "destroy received the same container init got");
  assert.equal(Corvus.plugins.getActive(), null, "nothing active after close");
  assert.equal(root.children[0].className, "plugin-grid", "grid restored on close (container removed)");
}

function testOpenSecondClosesFirst() {
  const root = makeEl("div");
  const telemetry = { subscribe() { return () => {}; }, getState() { return null; }, requestJson() { return Promise.resolve({}); }, postAction() { return Promise.resolve({ ok: true }); } };
  Corvus.plugins.init(root, telemetry);

  const a = makeSpyPlugin("t-a", "A");
  const b = makeSpyPlugin("t-b", "B");
  Corvus.plugins.register("t-a", { name: "A", icon: "i", description: "d", init: a.init, destroy: a.destroy });
  Corvus.plugins.register("t-b", { name: "B", icon: "i", description: "d", init: b.init, destroy: b.destroy });

  Corvus.plugins.open("t-a");
  assert.equal(a.initCalls, 1);
  assert.equal(Corvus.plugins.getActive(), "t-a");

  Corvus.plugins.open("t-b");
  assert.equal(a.destroyCalls, 1, "first plugin destroyed when second opens");
  assert.equal(b.initCalls, 1, "second plugin initialised");
  assert.equal(Corvus.plugins.getActive(), "t-b");
  assert.equal(a.initCalls, 1, "first plugin init not called again");

  Corvus.plugins.close();
  assert.equal(b.destroyCalls, 1);
  assert.equal(Corvus.plugins.getActive(), null);
}

function testOpenUnknownIdIsNoOp() {
  assert.equal(Corvus.plugins.open("does-not-exist"), false);
  assert.equal(Corvus.plugins.getActive(), null);
}

// ---------------------------------------------------------------------------
// PART B: vibration plugin
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

function testRegisteredVibrationPlugin() {
  // plugin-vibration.js registered itself at load; verify it is in the grid.
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
  testRegisterAndList();
  testDuplicateRegisterRejected();
  testOpenCloseLifecycle();
  testOpenSecondClosesFirst();
  testOpenUnknownIdIsNoOp();

  testUpdateBufferOnlyOnChange();
  testUpdateBufferCapsMemory();
  testThrottleRedrawsAtMostOncePerWindow();
  testDestroyPurgesAndUnsubscribes();
  testReducedMotionZeroDurationTransition();
  testNonReducedMotionHasNoZeroTransition();
  testPlotlyMissingFallbackDoesNotSubscribe();
  testRegisteredVibrationPlugin();

  // Let the best-effort postAction microtasks (from destroy) drain so the
  // process exits cleanly with no pending unhandled work.
  await flushMicrotasks();

  console.log("frontend plugin tests passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
