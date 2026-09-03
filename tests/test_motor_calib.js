"use strict";

/**
 * Standalone tests for the Motor/ESC calibration feature in
 * Corvus.setupCalibration (src/js/setup-calibration.js).
 *
 * Deliberately isolated from tests/test_frontend_setup.js, whose in-flight
 * Firmware work breaks that suite: this file loads only setup-shared.js +
 * setup-calibration.js and drives Corvus.setupCalibration.render directly.
 * The DOM/fake-telemetry harness is copied verbatim from test_frontend_setup.js
 * so the stubs behave identically.
 *
 * Run:
 *   node tests/test_motor_calib.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals so setup-*.js loads and runs in Node.
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};

// prefers-reduced-motion toggle for the reduced-motion helper.
let reducedMotion = false;
window.matchMedia = (query) => ({
  matches: reducedMotion && String(query).includes("prefers-reduced-motion"),
  media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});

// Controlled clock for the Plotly throttle and the tSec buffer timestamps.
let clock = 1000;
Date.now = () => clock;

// window.dispatchEvent captures corvus:notification events.
const dispatched = [];
window.dispatchEvent = (event) => { dispatched.push(event); };

// Fake EventSource (unused by calibration, kept for harness parity).
const eventSources = [];
class FakeEventSource {
  constructor(url) { this.url = url; this.listeners = new Map(); this.closed = false; eventSources.push(this); }
  addEventListener(type, cb) { this.listeners.set(type, cb); }
  close() { this.closed = true; }
  emit(type, data) { const cb = this.listeners.get(type); if (cb) cb({ data: typeof data === "string" ? data : JSON.stringify(data) }); }
}
global.EventSource = FakeEventSource;

// Capture setInterval callbacks (never auto-fire — keeps tests deterministic).
const intervalCbs = [];
let nextIntervalId = 1;
const clearedIds = new Set();
window.setInterval = (cb) => { const id = nextIntervalId++; intervalCbs.push({ id, cb }); return id; };
window.clearInterval = (id) => { clearedIds.add(id); };
window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;

// ---------------------------------------------------------------------------
// Minimal DOM stub (mirrors tests/test_frontend_plugins.js / test_frontend_setup.js).
// The source builds structure with createElement + appendChild, so this stub
// tracks children, className/classList, textContent, dataset, style, listeners,
// and innerHTML (clears children when set to "").
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
    parentNode: null,
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
  // Maintain parentNode like a real DOM so the implementation's standard
  // `node.parentNode.removeChild(node)` removal idiom works under the stub.
  e.appendChild = (c) => { c.parentNode = e; e.children.push(c); return c; };
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; return c; };
  e.insertBefore = (n, ref) => { const i = ref ? e.children.indexOf(ref) : e.children.length; if (i < 0) e.children.push(n); else e.children.splice(i, 0, n); n.parentNode = e; return n; };
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
  // Single class selector (".cls") or tag name. Walks recursively.
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
/** Find a button in the safety dialog by its visible label. The dialog is a
 *  Corvus.ui.modal, so its buttons carry the shared .btn classes rather than
 *  a per-screen one — the label is what identifies them. Corvus.ui.button
 *  wraps the label in a <span>, and the stub's textContent is per-node, so
 *  the search looks at the button and its children. */
function findModalButton(root, label) {
  const text = (el) => [el.textContent || "", ...(el.children || []).map((c) => c.textContent || "")]
    .map((s) => s.trim()).filter(Boolean);
  return findByClass(root, "btn").find((b) => text(b).includes(label)) || null;
}
/** Fire all listeners of a given type on an element (simulate a click). */
function fire(el, type) {
  const listeners = (el && el._listeners && el._listeners[type]) || [];
  listeners.forEach((cb) => cb({}));
}

// ---------------------------------------------------------------------------
// Fake Plotly and a fake telemetry (mirrors tests/test_frontend_setup.js).
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
 * postAction resolves controllable responses.
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

// Load ONLY the modules under test — NOT setup.js (its Firmware work is in-flight
// and breaks the combined suite). setup-calibration.js depends only on
// setup-shared.js + Corvus.telemetry.
// ui.js first: it defines Corvus.ui, the component layer every other
// module builds its DOM with (index.html loads it in the same order).
require("../src/js/ui.js");
require("../src/js/setup-shared.js");
require("../src/js/setup-calibration.js");

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/** Fresh Plotly + telemetry + container per test so nothing leaks across. */
function reset(opts) {
  delete window.Plotly;
  window.Plotly = fakePlotly();   // POD graphs call Plotly.react on render
  clock = 1000;
  dispatched.length = 0;
  const fake = makeFakeTelemetry(opts || {});
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  return { container, fake };
}

async function testMotorButtonRendered() {
  const { container } = reset();
  const destroy = Corvus.setupCalibration.render(container, () => {});

  const grid = findOneByClass(container, "calib-grid");
  const btns = findByClass(grid, "calib-btn");
  assert.equal(btns.length, 7, "seven sensor-calibration buttons incl. motor/ESC");

  const motor = btns.find((b) => b.dataset.type === "motor");
  assert.ok(motor, "motor/ESC button present");
  assert.equal(motor.getAttribute("data-variant"), "danger", "motor button flagged danger");
  assert.equal(findOneByClass(motor, "calib-btn-label").textContent, "Motors (ESC)");

  destroy();
}

async function testMotorClickOpensModalNoPost() {
  const { container, fake } = reset();
  const destroy = Corvus.setupCalibration.render(container, () => {});

  const motor = findByClass(findOneByClass(container, "calib-grid"), "calib-btn")
    .find((b) => b.dataset.type === "motor");
  fire(motor, "click");

  // Modal opened under `page` (reachable via the container walk).
  assert.ok(findOneByClass(container, "modal-overlay"), "motor safety modal opened");
  // Opening the gate must NOT fire the POST — only confirm does.
  assert.equal(fake.postCalls.filter((c) => c.url === "/api/calibrate").length, 0,
    "no POST on direct motor click");

  destroy();
}

async function testMotorConfirmPostsMotorType() {
  const { container, fake } = reset();
  const destroy = Corvus.setupCalibration.render(container, () => {});

  const motor = findByClass(findOneByClass(container, "calib-grid"), "calib-btn")
    .find((b) => b.dataset.type === "motor");
  fire(motor, "click");

  const confirm = findModalButton(container, "Calibrate Motors");
  assert.ok(confirm, "confirm button present in modal");
  fire(confirm, "click");
  await flushMicrotasks();

  // Confirm dismisses the modal then POSTs {type:"motor"}.
  assert.ok(!findOneByClass(container, "modal-overlay"), "modal closed on confirm");
  const call = fake.postCalls.find((c) => c.url === "/api/calibrate");
  assert.ok(call, "POST /api/calibrate issued on confirm");
  assert.deepEqual(call.payload, { type: "motor" }, "confirm posts {type:'motor'}");

  destroy();
}

async function testMotorCancelNoPost() {
  const { container, fake } = reset();
  const destroy = Corvus.setupCalibration.render(container, () => {});

  const motor = findByClass(findOneByClass(container, "calib-grid"), "calib-btn")
    .find((b) => b.dataset.type === "motor");
  fire(motor, "click");

  const cancel = findModalButton(container, "Cancel");
  assert.ok(cancel, "cancel button present in modal");
  fire(cancel, "click");

  assert.ok(!findOneByClass(container, "modal-overlay"), "modal closed on cancel");
  assert.equal(fake.postCalls.filter((c) => c.url === "/api/calibrate").length, 0,
    "no POST on cancel");

  destroy();
}

async function testMotorModalCleanedOnDestroy() {
  const { container, fake } = reset();
  const destroy = Corvus.setupCalibration.render(container, () => {});

  const motor = findByClass(findOneByClass(container, "calib-grid"), "calib-btn")
    .find((b) => b.dataset.type === "motor");
  fire(motor, "click");
  assert.ok(findOneByClass(container, "modal-overlay"), "modal opened before destroy");

  destroy();

  // destroy() must drop an open modal so no DOM subtree leaks on back/re-render.
  assert.ok(!findOneByClass(container, "modal-overlay"), "modal removed on destroy");
  assert.equal(fake.unsubCalls, 1, "telemetry unsubscribed exactly once");
}

async function testMotorButtonArmedGating() {
  const { container, fake } = reset({ state: { armed: false, connected: true, warnings: [] } });
  const destroy = Corvus.setupCalibration.render(container, () => {});

  const motor = findByClass(findOneByClass(container, "calib-grid"), "calib-btn")
    .find((b) => b.dataset.type === "motor");

  assert.ok(!motor.disabled, "motor button enabled while disarmed");
  fake.getSubCb()({ armed: true, connected: true, warnings: [] });
  assert.strictEqual(motor.disabled, true, "motor button disabled while armed");
  fake.getSubCb()({ armed: false, connected: true, warnings: [] });
  assert.ok(!motor.disabled, "motor button re-enabled when disarmed again");

  destroy();
}

async function run() {
  await testMotorButtonRendered();
  await testMotorClickOpensModalNoPost();
  await testMotorConfirmPostsMotorType();
  await testMotorCancelNoPost();
  await testMotorModalCleanedOnDestroy();
  await testMotorButtonArmedGating();
  await flushMicrotasks();
  console.log("motor calib tests passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
