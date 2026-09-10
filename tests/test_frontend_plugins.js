"use strict";

/**
 * Frontend tests for the TOOLS-tab plugin registry (Corvus.plugins), the
 * drop-in plugin folder, and the two plugins that ship with Corvus — the
 * Vibration Monitor and the SSH Launcher.
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

global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  head,
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
require("../src/js/ui.js");
require("../src/js/plugins.js");
// Both shipped plugins are *folder* plugins: in the browser their scripts are
// appended by loadInstalled rather than by tags in index.html. Requiring them
// here runs the same files the same way — each registers itself as it loads.
require("../plugins/vibration/vibration.js");
require("../plugins/ssh-launcher/ssh-launcher.js");

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
  // vibration.js registered itself at load; verify it is in the grid.
  const found = Corvus.plugins.list().find((p) => p.id === "vibration");
  assert.ok(found, "vibration plugin registered at module load");
  assert.equal(found.name, "Vibration Monitor");
  assert.equal(found.icon, "activity");
  assert.equal(found.description, "Live PX4 vibration metrics and accelerometer clipping");
}

// ---------------------------------------------------------------------------
// PART C: the drop-in plugin folder (Corvus.plugins.loadInstalled)
// ---------------------------------------------------------------------------

/** Run *fn* with console.error muted — two tests below exercise the loader's
 *  failure paths on purpose, and their logging is the expected behaviour, not
 *  output worth printing on a passing run. */
async function quietly(fn) {
  const real = console.error;
  console.error = () => {};
  try { return await fn(); }
  finally { console.error = real; }
}

/** Re-init the registry against a telemetry stub whose responses are scripted. */
function initWithResponses(responses) {
  const calls = [];
  const telemetry = {
    subscribe() { return () => {}; },
    getState() { return null; },
    requestJson(url, options) {
      calls.push({ url, options });
      const answer = responses[url];
      if (answer === undefined) return Promise.reject(new Error("no stub for " + url));
      return Promise.resolve(typeof answer === "function" ? answer() : answer);
    },
    postAction() { return Promise.resolve({ ok: true }); },
  };
  Corvus.plugins.init(makeEl("div"), telemetry);
  return calls;
}

async function testLoadInstalledAppendsScriptsAndStyles() {
  appendedScripts.length = 0;
  appendedStyles.length = 0;
  initWithResponses({
    "/api/plugins": {
      plugins: [{ id: "demo", scripts: ["demo.js"], styles: ["demo.css"] }],
      settings: {},
    },
  });
  const loaded = await Corvus.plugins.loadInstalled();
  assert.deepEqual(loaded, ["demo"], "loadInstalled reports the ids it loaded");
  assert.deepEqual(appendedScripts, ["/api/plugins/asset/demo/demo.js"]);
  assert.deepEqual(appendedStyles, ["/api/plugins/asset/demo/demo.css"]);
}

async function testLoadInstalledEncodesEachPathSegment() {
  appendedScripts.length = 0;
  initWithResponses({
    "/api/plugins": { plugins: [{ id: "nested", scripts: ["sub dir/a b.js"] }], settings: {} },
  });
  await Corvus.plugins.loadInstalled();
  // Segments are encoded individually so the separators survive — a plugin
  // that puts its script in a subfolder must still resolve.
  assert.deepEqual(appendedScripts, ["/api/plugins/asset/nested/sub%20dir/a%20b.js"]);
}

async function testLoadInstalledSurvivesAFailingPlugin() {
  appendedScripts.length = 0;
  scriptBehaviour = (src) => (src.includes("bad") ? "error" : "load");
  initWithResponses({
    "/api/plugins": {
      plugins: [
        { id: "bad", scripts: ["bad.js"] },
        { id: "fine", scripts: ["fine.js"] },
      ],
      settings: {},
    },
  });
  const loaded = await Corvus.plugins.loadInstalled();
  scriptBehaviour = () => "load";
  // One plugin failing to load costs that plugin only.
  assert.deepEqual(loaded, ["fine"]);
  assert.equal(appendedScripts.length, 2, "both were attempted");
}

async function testLoadInstalledSurvivesADeadEndpoint() {
  initWithResponses({});   // /api/plugins rejects
  const loaded = await Corvus.plugins.loadInstalled();
  assert.deepEqual(loaded, [], "a failed discovery resolves empty, never rejects");
}

async function testLoadInstalledDoesNotLoadTheSamePluginTwice() {
  appendedScripts.length = 0;
  initWithResponses({
    "/api/plugins": { plugins: [{ id: "once", scripts: ["once.js"] }], settings: {} },
  });
  await Corvus.plugins.loadInstalled();
  const second = await Corvus.plugins.loadInstalled();
  assert.deepEqual(second, [], "an already-loaded plugin is not appended again");
  assert.equal(appendedScripts.length, 1);
}

async function testInstalledPluginRegistersOnLoad() {
  // The whole point of the mechanism: the appended script's own register()
  // call puts the plugin in the grid.
  scriptBehaviour = () => () => {
    Corvus.plugins.register("t-installed", {
      name: "Installed", icon: "puzzle", description: "arrived via the folder",
      init() {}, destroy() {},
    });
  };
  initWithResponses({
    "/api/plugins": { plugins: [{ id: "t-installed", scripts: ["t-installed.js"] }], settings: {} },
  });
  await Corvus.plugins.loadInstalled();
  scriptBehaviour = () => "load";
  const found = Corvus.plugins.list().find((p) => p.id === "t-installed");
  assert.ok(found, "a plugin that registers as its script runs appears in the grid");
  assert.equal(found.description, "arrived via the folder");
}

// ---------------------------------------------------------------------------
// PART D: per-plugin settings
// ---------------------------------------------------------------------------

async function testSettingsAreSeededAndScopedPerPlugin() {
  const calls = initWithResponses({
    "/api/plugins": {
      plugins: [],
      settings: { "t-settings": { directory: "/srv" }, other: { x: 1 } },
    },
    "/api/plugins/settings": () => ({ ok: true, settings: { directory: "/srv", command: "./run" } }),
  });
  await Corvus.plugins.loadInstalled();

  let seen = null;
  Corvus.plugins.register("t-settings", {
    name: "Settings", icon: "i", description: "d",
    init(_el, api) { seen = api; }, destroy() {},
  });
  Corvus.plugins.open("t-settings");

  assert.deepEqual(seen.getSettings(), { directory: "/srv" },
    "a plugin sees its own saved settings, seeded before its init ran");

  const merged = await seen.saveSettings({ command: "./run" });
  assert.deepEqual(merged, { directory: "/srv", command: "./run" });
  const post = calls.find((c) => c.url === "/api/plugins/settings");
  const body = JSON.parse(post.options.body);
  assert.equal(body.id, "t-settings", "saveSettings is bound to the plugin it was handed to");
  assert.equal(body.replace, false, "a plain save merges");
  assert.deepEqual(seen.getSettings(), { directory: "/srv", command: "./run" },
    "the local copy tracks what the backend returned");
  Corvus.plugins.close();
}

async function testSaveSettingsCanReplace() {
  const calls = initWithResponses({
    "/api/plugins": { plugins: [], settings: { "t-replace": { old: 1, keep: 2 } } },
    "/api/plugins/settings": () => ({ ok: true, settings: { keep: 3 } }),
  });
  await Corvus.plugins.loadInstalled();
  let api = null;
  Corvus.plugins.register("t-replace", {
    name: "Replace", icon: "i", description: "d",
    init(_el, a) { api = a; }, destroy() {},
  });
  Corvus.plugins.open("t-replace");
  const saved = await api.saveSettings({ keep: 3 }, true);
  assert.equal(JSON.parse(calls.find((c) => c.url === "/api/plugins/settings").options.body).replace, true);
  // The retired key is gone from the local copy too, not just on disk.
  assert.deepEqual(saved, { keep: 3 });
  assert.deepEqual(api.getSettings(), { keep: 3 });
  Corvus.plugins.close();
}

async function testGetSettingsIsAnIsolatedCopy() {
  initWithResponses({
    "/api/plugins": { plugins: [], settings: { "t-copy": { a: 1 } } },
  });
  await Corvus.plugins.loadInstalled();
  let api = null;
  Corvus.plugins.register("t-copy", {
    name: "Copy", icon: "i", description: "d",
    init(_el, a) { api = a; }, destroy() {},
  });
  Corvus.plugins.open("t-copy");
  const first = api.getSettings();
  first.a = 99;
  assert.deepEqual(api.getSettings(), { a: 1 },
    "mutating the returned object cannot corrupt the store");
  Corvus.plugins.close();
}

async function testPostJsonResolvesOnAFailureBody() {
  initWithResponses({
    "/api/thing": () => ({ ok: false, error: "no", stderr: "the detail" }),
  });
  let api = null;
  Corvus.plugins.register("t-postjson", {
    name: "PostJson", icon: "i", description: "d",
    init(_el, a) { api = a; }, destroy() {},
  });
  Corvus.plugins.open("t-postjson");
  // postAction would reject here and throw the stderr away; postJson does not.
  const res = await api.postJson("/api/thing", { x: 1 });
  assert.equal(res.stderr, "the detail");
  Corvus.plugins.close();
}

// ---------------------------------------------------------------------------
// PART E: the bundled SSH Launcher plugin
// ---------------------------------------------------------------------------

function testSshLauncherRegistered() {
  const found = Corvus.plugins.list().find((p) => p.id === "ssh-launcher");
  assert.ok(found, "ssh-launcher registered at load");
  assert.equal(found.name, "SSH Launcher");
  assert.equal(found.icon, "rocket");
}

function testSshLauncherPreviewLine() {
  const preview = Corvus.pluginSshLauncher.previewLine;
  assert.equal(preview({ connection: "companion", directory: "/srv", command: "./run.sh", mode: "background" }),
    "ssh companion 'cd /srv && nohup ./run.sh &'");
  assert.equal(preview({ connection: "companion", directory: "/srv", command: "./run.sh", mode: "terminal" }),
    "ssh companion 'cd /srv && ./run.sh'");
  // Background with no folder still shows the nohup — the earlier version
  // patched the composed string and silently dropped it here.
  assert.equal(preview({ connection: "companion", command: "./run.sh", mode: "background" }),
    "ssh companion 'nohup ./run.sh &'");
  assert.equal(preview({ connection: "companion", command: "uptime" }),
    "ssh companion 'uptime'");
  // Nothing to run yet — the button stays disabled and the box shows a hint.
  assert.equal(preview({ connection: "companion", directory: "/srv", command: "  " }), "");
  assert.equal(preview({}), "");
}

function testSshLauncherRemoteLineQuotesTheFolderOnly() {
  const line = Corvus.pluginSshLauncher.remoteLine;
  assert.equal(line({ directory: "/srv/my mission", command: "./run.sh --fast" }),
    "cd -- '/srv/my mission' && ./run.sh --fast");
  // The injection lands inside the quotes: a folder name, not a second command.
  assert.equal(line({ directory: "/tmp'; rm -rf ~", command: "./run.sh" }),
    "cd -- '/tmp'\\''; rm -rf ~' && ./run.sh");
  assert.equal(line({ command: "uptime" }), "uptime");
  assert.equal(line({ directory: "/srv", command: "  " }), "");
}

function testSshLauncherResultSummary() {
  const summary = Corvus.pluginSshLauncher.resultSummary;
  assert.deepEqual(summary({ ok: true, stdout: "4711\n" }),
    { text: "Started in the background (pid 4711).", kind: "ok" });
  assert.deepEqual(summary({ ok: true, stdout: "" }),
    { text: "Started in the background.", kind: "ok" });
  // A failure shows the first line of stderr — the operator's actual diagnosis.
  assert.deepEqual(
    summary({ ok: false, stderr: "sh: ./run.sh: not found\nmore", error: "exit 127" }),
    { text: "sh: ./run.sh: not found", kind: "err" });
  assert.deepEqual(summary({ ok: false, error: "connection refused" }),
    { text: "connection refused", kind: "err" });
  assert.deepEqual(summary({ ok: false }), { text: "The command failed.", kind: "err" });
}

function testSshLauncherModeMigratesFromDetach() {
  const mode = Corvus.pluginSshLauncher.coerceMode;
  // The boolean this plugin saved before it grew a terminal.
  assert.equal(mode({ detach: true }), "background");
  assert.equal(mode({ detach: false }), "terminal");
  // An explicit mode wins, and anything unrecognised lands on the default.
  assert.equal(mode({ mode: "background", detach: false }), "background");
  assert.equal(mode({ mode: "nonsense" }), "terminal");
  assert.equal(mode({}), "terminal");
}

function testSshLauncherSessionIsKeyedByIdNotLabel() {
  const session = Corvus.pluginSshLauncher.sessionName;
  assert.equal(session({ id: "b123", label: "Start mission" }), "ssh-launcher/b123");
  // Renaming a button must not orphan the session its program runs in.
  assert.equal(session({ id: "b123", label: "Renamed" }), "ssh-launcher/b123");
}

/** Every launcher mounted by a test, so a failing assertion cannot leave one
 *  running: the plugin polls on an interval, and a leaked interval keeps Node
 *  alive long past the failure that caused it. */
const mountedLaunchers = [];
function destroyLaunchers() {
  while (mountedLaunchers.length) {
    try { Corvus.pluginSshLauncher.destroy(mountedLaunchers.pop()); } catch (_e) {}
  }
}

/**
 * Mount the launcher against a stub api and return everything a test needs.
 * `opts` is {saved, run, connect, sessions}: the settings it reads, the
 * /api/ssh/run body, the /api/ssh/connect body, and which sessions are live.
 */
function mountLauncher(opts) {
  const o = opts || {};
  const calls = [];
  const connections = [
    { name: "companion", host: "10.0.0.7", username: "pilot", port: 22 },
    { name: "ground", host: "10.0.0.2", username: "ops", port: 22 },
  ];
  let sessions = (o.sessions || []).map((name) => ({ name, connected: true }));
  let settings = o.saved || {};
  const terminals = [];
  const container = makeEl("div");
  mountedLaunchers.push(container);
  Corvus.pluginSshLauncher.init(container, {
    requestJson: (url) => {
      calls.push({ url });
      if (url === "/api/ssh/connections") return Promise.resolve({ connections });
      if (url === "/api/ssh/sessions") return Promise.resolve({ sessions });
      return Promise.reject(new Error("no stub for " + url));
    },
    postJson: (url, body) => {
      calls.push({ url, body });
      if (url === "/api/ssh/connect") {
        return Promise.resolve(o.connect || { ok: true, connected: true });
      }
      if (url === "/api/ssh/send" || url === "/api/ssh/disconnect") {
        return Promise.resolve({ ok: true });
      }
      return Promise.resolve(o.run || { ok: true, stdout: "4711", stderr: "", command: "x" });
    },
    getSettings: () => settings,
    saveSettings: (patch) => { settings = Object.assign({}, settings, patch); return Promise.resolve(settings); },
    terminal: (session) => { terminals.push(session); return true; },
    console: () => {},
    notification: () => {},
  });
  return {
    container, calls, connections, terminals,
    savedNow: () => settings,
    setSessions: (names) => { sessions = names.map((name) => ({ name, connected: true })); },
    /** The launch buttons currently on the shelf, in order. */
    shelf: () => querySel(container.children, ".sshl-launch"),
    /** A row's tool button, found by what it announces rather than by index —
     *  the tools differ per row (a terminal button has an arrow, a background
     *  one does not) and positions would make these tests lie. */
    tool: (label) => querySel(container.children, ".icon-btn")
      .find((b) => b.getAttribute("aria-label") === label),
    /** Every .btn with the given visible label. */
    byLabel: (text) => querySel(container.children, ".btn")
      .filter((b) => b.children.some((c) => c.textContent === text)),
    fields: () => querySel(container.children, ".field-input"),
    select: () => querySel(container.children, ".field-select")[0],
    preview: () => querySel(container.children, ".sshl-preview")[0],
    status: () => querySel(container.children, ".ui-msg")[0],
    dots: () => querySel(container.children, ".sshl-dot"),
  };
}

/** Fire an element's first click listener. */
function click(el) { el._listeners.click[0](); }

/** Type into an input built by Corvus.ui.input (fires its "input" listener). */
function typeInto(el, value) {
  el.value = value;
  (el._listeners.input || []).forEach((cb) => cb());
}

const TERMINAL_BUTTON = {
  id: "a", label: "Start mission", connection: "companion",
  directory: "/srv", command: "./run.sh", mode: "terminal",
};
const BACKGROUND_BUTTON = {
  id: "b", label: "Record logs", connection: "ground",
  directory: "", command: "./record.sh", mode: "background",
};

function testSshLauncherNormalizesASavedShelf() {
  const normalize = Corvus.pluginSshLauncher.normalizeButtons;
  const list = normalize({
    buttons: [
      { id: "a", label: "Mission", connection: "companion", directory: "/srv", command: "./run.sh", mode: "background" },
      { command: "uptime" },
      { label: "no command" },          // dropped: nothing to run
      "not an object",                   // dropped
    ],
  });
  assert.equal(list.length, 2);
  assert.deepEqual(list[0], {
    id: "a", label: "Mission", connection: "companion",
    directory: "/srv", command: "./run.sh", mode: "background",
  });
  // An unnamed button falls back to its command, and defaults to a terminal.
  assert.equal(list[1].label, "uptime");
  assert.equal(list[1].mode, "terminal");
  assert.ok(list[1].id, "a saved entry with no id gets one");
}

function testSshLauncherMigratesTheOldSingleCommandShape() {
  // The shape this plugin saved before it grew a shelf. An operator who
  // configured it then must find their command as the first button.
  const list = Corvus.pluginSshLauncher.normalizeButtons({
    connection: "companion", directory: "/srv", command: "./start.sh", detach: true,
  });
  assert.equal(list.length, 1);
  assert.equal(list[0].command, "./start.sh");
  assert.equal(list[0].connection, "companion");
  assert.equal(list[0].label, "./start.sh");
  // detach:true was "start it with nohup and let it run" — that is background.
  assert.equal(list[0].mode, "background");
}

function testSshLauncherNormalizeIsEmptyForNothingSaved() {
  const normalize = Corvus.pluginSshLauncher.normalizeButtons;
  assert.deepEqual(normalize(undefined), []);
  assert.deepEqual(normalize({}), []);
  assert.deepEqual(normalize({ buttons: [] }), []);
}

function testSshLauncherCapsTheShelf() {
  const many = [];
  for (let i = 0; i < Corvus.pluginSshLauncher.MAX_BUTTONS + 5; i++) many.push({ command: "c" + i });
  const list = Corvus.pluginSshLauncher.normalizeButtons({ buttons: many });
  assert.equal(list.length, Corvus.pluginSshLauncher.MAX_BUTTONS);
}

async function testSshLauncherRendersOneButtonPerSavedEntry() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON, BACKGROUND_BUTTON] } });
  await flushMicrotasks();
  const shelf = h.shelf();
  assert.equal(shelf.length, 2, "one launch button per saved entry");
  assert.equal(shelf[0].getAttribute("aria-label"), "Launch Start mission");
  assert.equal(shelf[1].getAttribute("aria-label"), "Launch Record logs");
  // The composed line is the tooltip: the label rarely says what actually runs.
  assert.equal(shelf[0].title, "ssh companion 'cd /srv && ./run.sh'");
  assert.equal(shelf[1].title, "ssh ground 'nohup ./record.sh &'");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherTerminalButtonOpensASessionAndTypesTheLine() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON] } });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();

  const connect = h.calls.find((c) => c.url === "/api/ssh/connect");
  assert.deepEqual(connect.body, { name: "ssh-launcher/a", from: "companion" },
    "the session is the button's own, borrowing the saved connection's credentials");
  const send = h.calls.find((c) => c.url === "/api/ssh/send");
  assert.deepEqual(send.body, { name: "ssh-launcher/a", data: "cd -- '/srv' && ./run.sh\n" });
  assert.ok(!h.calls.some((c) => c.url === "/api/ssh/run"),
    "a terminal button never goes through the one-shot run endpoint");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherTerminalButtonReportsAFailedConnect() {
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON] },
    connect: { ok: false, connected: false, error: "Authentication failed" },
  });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();

  assert.equal(h.status().textContent, "Start mission: Authentication failed");
  assert.ok(!h.calls.some((c) => c.url === "/api/ssh/send"),
    "nothing is typed into a session that did not open");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherBackgroundButtonUsesTheRunEndpoint() {
  const h = mountLauncher({ saved: { buttons: [BACKGROUND_BUTTON] } });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();

  const run = h.calls.find((c) => c.url === "/api/ssh/run");
  assert.deepEqual(run.body, {
    name: "ground", directory: "", command: "./record.sh", detach: true,
  }, "the saved connection is named, never a password");
  assert.ok(!h.calls.some((c) => c.url === "/api/ssh/connect"),
    "a background button opens no session to leave behind");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherArrowIsOffUntilSomethingIsRunning() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON] } });
  await flushMicrotasks();
  const arrow = h.tool("Open the terminal for Start mission");
  assert.ok(arrow, "a terminal button has an arrow");
  assert.equal(arrow.disabled, true, "with nothing running the arrow leads nowhere");
  assert.ok(arrow.title.includes("Not running"));
  assert.equal(h.dots().length, 0, "and there is no live dot");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherBackgroundButtonHasNoArrow() {
  const h = mountLauncher({ saved: { buttons: [BACKGROUND_BUTTON] } });
  await flushMicrotasks();
  assert.equal(h.tool("Open the terminal for Record logs"), undefined,
    "a detached program has no terminal to open");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherArrowOpensTheSessionsTerminal() {
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON] },
    sessions: ["ssh-launcher/a"],
  });
  await flushMicrotasks();
  const arrow = h.tool("Open the terminal for Start mission");
  assert.equal(arrow.disabled, false, "a live session makes the arrow live too");
  assert.equal(h.dots().length, 1, "and the row shows it is running");
  // Pressing it restarts rather than promising a second copy.
  assert.equal(h.shelf()[0].getAttribute("aria-label"), "Restart Start mission");

  click(arrow);
  assert.deepEqual(h.terminals[0], {
    name: "ssh-launcher/a",
    title: "Start mission",
    host: "10.0.0.7",
    port: 22,
    username: "pilot",
  }, "the panel gets the session key plus a readable title and the host it is on");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherLaunchingMarksTheRowRunning() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON] } });
  await flushMicrotasks();
  assert.equal(h.dots().length, 0);
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  // No waiting for the next poll: the launch that just succeeded is proof.
  assert.equal(h.dots().length, 1);
  assert.equal(h.tool("Open the terminal for Start mission").disabled, false);
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherAddsAButton() {
  const h = mountLauncher({ saved: { buttons: [] } });
  await flushMicrotasks();
  assert.equal(h.shelf().length, 0);

  click(h.byLabel("Add button")[0]);
  const [labelField, dirField, cmdField] = h.fields();
  typeInto(labelField, "Start mission");
  typeInto(dirField, "/srv/mission");
  typeInto(cmdField, "./run.sh");
  assert.equal(h.preview().textContent, "ssh companion 'cd /srv/mission && ./run.sh'",
    "the editor previews what the new button will run");

  click(h.byLabel("Save")[0]);
  await flushMicrotasks();

  assert.equal(h.shelf().length, 1, "the new button is on the shelf");
  const saved = h.savedNow().buttons;
  assert.equal(saved.length, 1, "and was persisted");
  assert.equal(saved[0].label, "Start mission");
  assert.equal(saved[0].command, "./run.sh");
  assert.equal(saved[0].mode, "terminal", "a new button runs in a terminal by default");
  assert.equal(saved[0].connection, "companion", "and defaults to the first connection");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherCancelDiscardsTheDraft() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON] } });
  await flushMicrotasks();

  click(h.tool("Edit Start mission"));
  typeInto(h.fields()[0], "Renamed");
  click(h.byLabel("Cancel")[0]);
  await flushMicrotasks();

  assert.equal(h.shelf()[0].getAttribute("aria-label"), "Launch Start mission",
    "Cancel really cancels — the shelf still holds the original");
  assert.equal(h.savedNow().buttons[0].label, "Start mission",
    "and nothing was written back over it");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherEditsInPlaceWithoutAddingOne() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON, BACKGROUND_BUTTON] } });
  await flushMicrotasks();

  click(h.tool("Edit Start mission"));
  typeInto(h.fields()[0], "Mission A");
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();

  const saved = h.savedNow().buttons;
  assert.equal(saved.length, 2, "editing replaces, it does not append");
  assert.equal(saved[0].id, "a", "and keeps the entry's id — its session depends on it");
  assert.equal(saved[0].label, "Mission A");
  assert.equal(saved[1].label, "Record logs", "the other button is untouched");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherRemovesAnIdleButtonWithoutAsking() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON, BACKGROUND_BUTTON] } });
  await flushMicrotasks();

  let asked = false;
  window.confirm = () => { asked = true; return true; };
  click(h.tool("Remove Start mission"));
  await flushMicrotasks();

  assert.equal(asked, false, "nothing is running, so nothing is at stake");
  assert.deepEqual(h.savedNow().buttons.map((b) => b.id), ["b"]);
  assert.ok(!h.calls.some((c) => c.url === "/api/ssh/disconnect"));
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherRemovingARunningButtonAsksAndClosesItsTerminal() {
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON] },
    sessions: ["ssh-launcher/a"],
  });
  await flushMicrotasks();

  // Refused: the button, and the program it is running, both stay.
  window.confirm = () => false;
  click(h.tool("Remove Start mission"));
  await flushMicrotasks();
  assert.equal(h.shelf().length, 1, "declining the prompt keeps the button");
  assert.ok(!h.calls.some((c) => c.url === "/api/ssh/disconnect"));

  // Confirmed: the button goes, and so does the session only it could reach.
  window.confirm = () => true;
  click(h.tool("Remove Start mission"));
  await flushMicrotasks();
  assert.equal(h.shelf().length, 0);
  assert.deepEqual(h.calls.find((c) => c.url === "/api/ssh/disconnect").body,
    { name: "ssh-launcher/a" });
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherSaveIsBlockedWithoutACommand() {
  const h = mountLauncher({ saved: { buttons: [] } });
  await flushMicrotasks();
  click(h.byLabel("Add button")[0]);
  const save = h.byLabel("Save")[0];
  assert.equal(save.disabled, true, "a button with nothing to run cannot be saved");
  typeInto(h.fields()[2], "./run.sh");
  assert.equal(save.disabled, false);
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherShowsStderrOfAFailedBackgroundRun() {
  const h = mountLauncher({
    saved: { buttons: [BACKGROUND_BUTTON] },
    run: { ok: false, stdout: "", stderr: "sh: ./record.sh: not found", error: "exit 127" },
  });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();

  const out = querySel(h.container.children, ".sshl-output")[0];
  assert.equal(out.hidden, false, "the failure's output is shown");
  assert.ok(out.textContent.includes("not found"));
  assert.equal(h.status().textContent, "Record logs: sh: ./record.sh: not found",
    "the status line names the button that failed");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherWithoutConnectionsCannotAdd() {
  const container = makeEl("div");
  mountedLaunchers.push(container);
  Corvus.pluginSshLauncher.init(container, {
    requestJson: () => Promise.resolve({ connections: [], sessions: [] }),
    postJson: () => Promise.resolve({ ok: true }),
    getSettings: () => ({}),
    saveSettings: () => Promise.resolve({}),
    terminal: () => true,
    console: () => {},
    notification: () => {},
  });
  await flushMicrotasks();
  const note = querySel(container.children, ".sshl-note")[0];
  assert.ok(note && note.textContent.includes("Settings"),
    "the empty state points at where connections are added");
  Corvus.pluginSshLauncher.destroy(container);
}

async function testSshLauncherDestroyStopsThePollAndLateCallbacks() {
  let resolveConnect;
  let polls = 0;
  const container = makeEl("div");
  mountedLaunchers.push(container);
  Corvus.pluginSshLauncher.init(container, {
    requestJson: (url) => {
      if (url === "/api/ssh/sessions") { polls++; return Promise.resolve({ sessions: [] }); }
      return Promise.resolve({ connections: [{ name: "companion", host: "h", username: "u" }] });
    },
    postJson: () => new Promise((r) => { resolveConnect = r; }),
    getSettings: () => ({ buttons: [{ id: "a", label: "Go", connection: "companion", command: "./run.sh" }] }),
    saveSettings: () => Promise.resolve({}),
    terminal: () => true,
    console: () => {},
    notification: () => {},
  });
  await flushMicrotasks();
  click(querySel(container.children, ".sshl-launch")[0]);
  const pollsAtTeardown = polls;
  Corvus.pluginSshLauncher.destroy(container);

  // The connect answers after the plugin was closed; nothing may be painted
  // into a container the registry has already discarded, and the liveness poll
  // must not outlive it either — an interval nobody clears is a leak.
  resolveConnect({ ok: true, connected: true });
  await flushMicrotasks();
  assert.equal(querySel(container.children, ".ui-msg")[0].hidden, true,
    "a late result does not touch a destroyed plugin");
  await new Promise((r) => setTimeout(r, Corvus.pluginSshLauncher.LIVE_POLL_MS + 60));
  assert.equal(polls, pollsAtTeardown, "the liveness poll stopped with the plugin");
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

  await testLoadInstalledAppendsScriptsAndStyles();
  await testLoadInstalledEncodesEachPathSegment();
  await quietly(testLoadInstalledSurvivesAFailingPlugin);
  await quietly(testLoadInstalledSurvivesADeadEndpoint);
  await testLoadInstalledDoesNotLoadTheSamePluginTwice();
  await testInstalledPluginRegistersOnLoad();

  await testSettingsAreSeededAndScopedPerPlugin();
  await testSaveSettingsCanReplace();
  await testGetSettingsIsAnIsolatedCopy();
  await testPostJsonResolvesOnAFailureBody();

  testSshLauncherRegistered();
  testSshLauncherPreviewLine();
  testSshLauncherRemoteLineQuotesTheFolderOnly();
  testSshLauncherResultSummary();
  testSshLauncherModeMigratesFromDetach();
  testSshLauncherSessionIsKeyedByIdNotLabel();
  testSshLauncherNormalizesASavedShelf();
  testSshLauncherMigratesTheOldSingleCommandShape();
  testSshLauncherNormalizeIsEmptyForNothingSaved();
  testSshLauncherCapsTheShelf();
  await testSshLauncherRendersOneButtonPerSavedEntry();
  await testSshLauncherTerminalButtonOpensASessionAndTypesTheLine();
  await testSshLauncherTerminalButtonReportsAFailedConnect();
  await testSshLauncherBackgroundButtonUsesTheRunEndpoint();
  await testSshLauncherArrowIsOffUntilSomethingIsRunning();
  await testSshLauncherBackgroundButtonHasNoArrow();
  await testSshLauncherArrowOpensTheSessionsTerminal();
  await testSshLauncherLaunchingMarksTheRowRunning();
  await testSshLauncherAddsAButton();
  await testSshLauncherCancelDiscardsTheDraft();
  await testSshLauncherEditsInPlaceWithoutAddingOne();
  await testSshLauncherRemovesAnIdleButtonWithoutAsking();
  await testSshLauncherRemovingARunningButtonAsksAndClosesItsTerminal();
  await testSshLauncherSaveIsBlockedWithoutACommand();
  await testSshLauncherShowsStderrOfAFailedBackgroundRun();
  await testSshLauncherWithoutConnectionsCannotAdd();
  await testSshLauncherDestroyStopsThePollAndLateCallbacks();

  // Let the best-effort postAction microtasks (from destroy) drain so the
  // process exits cleanly with no pending unhandled work.
  await flushMicrotasks();

  console.log("frontend plugin tests passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
}).finally(destroyLaunchers);   // a failed assertion must not leave a poll running
