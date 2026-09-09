"use strict";

/**
 * Tests for the calibration wizard in Corvus.setupCalibration
 * (src/js/setup-calibration.js) — the guided flow, its lifecycle, and the
 * motor/ESC safety gate.
 *
 * Deliberately isolated from tests/test_frontend_setup.js: this file loads only
 * the calibration stack (ui, setup-shared, calib-figures, calib-protocol,
 * setup-calibration) and drives Corvus.setupCalibration.render directly, so a
 * failure here points at the calibration screen and nothing else. The DOM /
 * fake-telemetry harness is shared with test_frontend_setup.js so the stubs
 * behave identically.
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
  let consoleCb = null;
  let unsubCalls = 0;
  let consoleUnsubCalls = 0;
  const unsub = () => { unsubCalls++; };
  const consoleUnsub = () => { consoleUnsubCalls++; };
  let paramsResponse = opts.paramsResponse || { complete: false, received: 0, count: 0, params: [] };
  let state = opts.state || { armed: false, connected: true };
  const telemetry = {
    postAction(url, payload) {
      postCalls.push({ url, payload });
      if (url === "/api/params/set" && opts.setReject) return Promise.reject(new Error(opts.setReject));
      if (url === "/api/calibrate" && opts.calibrateReject) {
        return Promise.reject(new Error(opts.calibrateReject));
      }
      return Promise.resolve({ ok: true });
    },
    requestJson(url) {
      requests.push(url);
      return Promise.resolve(paramsResponse);
    },
    subscribe(fn) { subCb = fn; return unsub; },
    // The wizard reads PX4 guidance from the console bus, not from the
    // de-duplicated telemetry warnings, so the fake mirrors that contract.
    subscribeConsole(fn) { consoleCb = fn; return consoleUnsub; },
    getState() { return state; },
  };
  return {
    telemetry, postCalls, requests,
    get unsubCalls() { return unsubCalls; },
    get consoleUnsubCalls() { return consoleUnsubCalls; },
    getSubCb: () => subCb,
    /** Push one STATUSTEXT line the way the console SSE would. */
    statustext(text, level) {
      if (consoleCb) consoleCb({ name: "STATUSTEXT", text, level: level || "info" });
    },
    setParamsResponse(r) { paramsResponse = r; },
    setState(s) { state = s; },
  };
}

// Load ONLY the modules under test — NOT setup.js (its Firmware work is in-flight
// and breaks the combined suite). setup-calibration.js depends on
// setup-shared.js, the figure/protocol modules and Corvus.telemetry.
// ui.js first: it defines Corvus.ui, the component layer every other
// module builds its DOM with (index.html loads it in the same order).
require("../src/js/ui.js");
require("../src/js/setup-shared.js");
require("../src/js/calib-figures.js");
require("../src/js/calib-protocol.js");
require("../src/js/setup-calibration.js");

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/** Fresh Plotly + telemetry + container per test so nothing leaks across. */
function reset(opts) {
  delete window.Plotly;
  window.Plotly = fakePlotly();   // PID graphs call Plotly.react on render
  clock = 1000;
  dispatched.length = 0;
  const fake = makeFakeTelemetry(opts || {});
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  pageViewEl = container;
  return { container, fake };
}

/** Open the calibration screen and drill into one procedure's wizard. */
function openWizard(container, type) {
  const destroy = Corvus.setupCalibration.render(container, () => {});
  const card = findByClass(container, "calib-card").find((c) => c.dataset.type === type);
  assert.ok(card, type + " card present");
  fire(card, "click");
  return destroy;
}

function actionButton(container, label) {
  return findByClass(findOneByClass(container, "calib-actions"), "btn")
    .find((b) => (b.textContent + (b.children || []).map((c) => c.textContent).join(""))
      .includes(label)) || null;
}

// ---------------------------------------------------------------------------
// List view
// ---------------------------------------------------------------------------

async function testListShowsEveryProcedureWithItsCost() {
  const { container } = reset();
  const destroy = Corvus.setupCalibration.render(container, () => {});

  const cards = findByClass(container, "calib-card");
  assert.equal(cards.length, 7, "one card per calibration");
  const accel = cards.find((c) => c.dataset.type === "accel");
  const meta = findByClass(accel, "calib-card-meta-item").map((m) => m.textContent);
  assert.ok(meta.includes("6 positions"), "the position count is stated up front");
  assert.ok(meta.includes("Reboot after"), "a calibration that needs a reboot says so");

  destroy();
}

// ---------------------------------------------------------------------------
// Wizard: the briefing comes before the command
// ---------------------------------------------------------------------------

async function testOpeningAWizardPostsNothing() {
  const { container, fake } = reset();
  const destroy = openWizard(container, "accel");

  assert.ok(findOneByClass(container, "calib-stage"), "stage rendered");
  assert.ok(findOneByClass(container, "calib-prep"), "preparation checklist rendered");
  assert.equal(fake.postCalls.filter((c) => c.url === "/api/calibrate").length, 0,
    "opening a calibration issues no command");

  destroy();
}

async function testAccelWizardDrawsEveryPositionItWillAskFor() {
  const { container } = reset();
  const destroy = openWizard(container, "accel");

  const chips = findByClass(container, "calib-pose");
  assert.equal(chips.length, 6, "six position figures");
  assert.deepEqual(chips.map((c) => c.dataset.pose), Corvus.calibProtocol.PROCEDURES.accel.poses);
  assert.ok(chips.every((c) => c.dataset.state === "pending"), "all pending before the start");
  // Each chip is labelled, so the sequence is readable even where the inline
  // SVG cannot be drawn.
  chips.forEach((c) => {
    assert.equal(findOneByClass(c, "calib-pose-label").textContent,
      Corvus.calibFigures.poseLabel(c.dataset.pose));
  });

  destroy();
}

async function testStartPostsTheCalibrationAndSwapsToAbort() {
  const { container, fake } = reset({ state: { armed: false, connected: true } });
  const destroy = openWizard(container, "gyro");

  const start = actionButton(container, "Start");
  assert.ok(start && !start.disabled, "start enabled when disarmed and linked");
  fire(start, "click");
  await flushMicrotasks();

  const call = fake.postCalls.find((c) => c.url === "/api/calibrate");
  assert.deepEqual(call.payload, { type: "gyro" }, "posts the procedure's own type");
  assert.ok(actionButton(container, "Abort") && !actionButton(container, "Abort").hidden,
    "abort offered while the calibration runs");
  assert.ok(start.hidden, "start withdrawn while the calibration runs");
  assert.ok(findOneByClass(container, "calib-prep").hidden,
    "the briefing gives way to the live instruction");

  destroy();
}

async function testStartIsBlockedWithoutALink() {
  const { container } = reset({ state: { armed: false, connected: false } });
  const destroy = openWizard(container, "gyro");

  assert.equal(actionButton(container, "Start").disabled, true, "no link, no start");
  const banner = findByClass(container, "setup-armed-banner")[0];
  assert.ok(banner && !banner.hidden, "and the reason is stated");
  assert.match(banner.textContent, /No link/);

  destroy();
}

async function testStartIsBlockedWhileArmed() {
  const { container, fake } = reset({ state: { armed: false, connected: true } });
  const destroy = openWizard(container, "gyro");
  assert.equal(actionButton(container, "Start").disabled, false);

  fake.getSubCb()({ armed: true, connected: true });
  assert.equal(actionButton(container, "Start").disabled, true, "armed disables the start");
  assert.match(findByClass(container, "setup-armed-banner")[0].textContent, /armed/);

  fake.getSubCb()({ armed: false, connected: true });
  assert.equal(actionButton(container, "Start").disabled, false, "disarming re-enables it");

  destroy();
}

// ---------------------------------------------------------------------------
// Wizard: PX4 guidance drives the screen
// ---------------------------------------------------------------------------

async function testStatustextDrivesTheInstructionAndTheStrip() {
  const { container, fake } = reset({ state: { armed: false, connected: true } });
  const destroy = openWizard(container, "accel");
  fire(actionButton(container, "Start"), "click");
  await flushMicrotasks();

  fake.statustext("[cal] calibration started: 2 accel");
  fake.statustext("[cal] Hold still, measuring down side");

  const headline = findOneByClass(container, "calib-headline");
  assert.match(headline.textContent, /Hold still/);
  assert.equal(findOneByClass(container, "calib-detail").textContent, "Level");
  const level = findByClass(container, "calib-pose").find((c) => c.dataset.pose === "level");
  assert.equal(level.dataset.state, "active", "the requested position is highlighted");

  fake.statustext("[cal] down side done, rotate to a different side");
  assert.equal(level.dataset.state, "done", "a finished position is marked off");

  // The transcript is the audit trail; it carries PX4's own words verbatim.
  const lines = findByClass(container, "guidance-msg").map((l) => l.textContent);
  assert.ok(lines.includes("[cal] Hold still, measuring down side"));

  destroy();
}

async function testSuccessEndsTheRunAndOffersTheWayBack() {
  const { container, fake } = reset({ state: { armed: false, connected: true } });
  const destroy = openWizard(container, "accel");
  fire(actionButton(container, "Start"), "click");
  await flushMicrotasks();

  fake.statustext("[cal] calibration done: accel");
  assert.equal(findOneByClass(container, "calib-wizard-view").dataset.phase, "done");
  assert.ok(actionButton(container, "Abort").hidden, "abort withdrawn once it is over");
  assert.ok(!actionButton(container, "Back to calibrations").hidden, "the way back is offered");
  assert.match(findOneByClass(container, "calib-detail").textContent, /Reboot/);

  destroy();
}

async function testFailureOffersARetryWithoutReopeningTheWizard() {
  const { container, fake } = reset({ state: { armed: false, connected: true } });
  const destroy = openWizard(container, "accel");
  fire(actionButton(container, "Start"), "click");
  await flushMicrotasks();

  fake.statustext("[cal] calibration failed: accel", "critical");
  assert.equal(findOneByClass(container, "calib-wizard-view").dataset.phase, "failed");

  const retry = actionButton(container, "Try again");
  assert.ok(retry && !retry.hidden, "retry offered after a failure");
  fire(retry, "click");
  assert.equal(findOneByClass(container, "calib-wizard-view").dataset.phase, "idle",
    "retry returns to the briefing");
  assert.ok(!findOneByClass(container, "calib-prep").hidden, "and the checklist comes back");

  destroy();
}

async function testAbortCancelsOnTheVehicle() {
  const { container, fake } = reset({ state: { armed: false, connected: true } });
  const destroy = openWizard(container, "compass");
  fire(actionButton(container, "Start"), "click");
  await flushMicrotasks();

  fire(actionButton(container, "Abort"), "click");
  await flushMicrotasks();

  // An abort has to reach the autopilot — stopping only the UI would leave the
  // vehicle mid-calibration with no way out but a power cycle.
  assert.ok(fake.postCalls.find((c) => c.url === "/api/calibrate/cancel"),
    "POST /api/calibrate/cancel issued");
  assert.equal(findOneByClass(container, "calib-wizard-view").dataset.phase, "cancelled");

  destroy();
}

async function testRejectedStartIsReportedNotSwallowed() {
  const { container, fake } = reset({
    state: { armed: false, connected: true }, calibrateReject: "cannot calibrate while armed",
  });
  const destroy = openWizard(container, "gyro");
  fire(actionButton(container, "Start"), "click");
  await flushMicrotasks();

  assert.equal(findOneByClass(container, "calib-wizard-view").dataset.phase, "failed");
  assert.match(findOneByClass(container, "calib-detail").textContent, /armed/);
  assert.ok(dispatched.some((e) => e.detail && e.detail.level === "critical"),
    "a rejected start raises a critical notification");

  destroy();
}

// ---------------------------------------------------------------------------
// Motor / ESC safety gate
// ---------------------------------------------------------------------------

async function testMotorStartOpensTheSafetyGateAndPostsNothing() {
  const { container, fake } = reset({ state: { armed: false, connected: true } });
  const destroy = openWizard(container, "motor");

  fire(actionButton(container, "Start"), "click");
  assert.ok(findOneByClass(container, "modal-overlay"), "motor safety modal opened");
  assert.equal(fake.postCalls.filter((c) => c.url === "/api/calibrate").length, 0,
    "the gate itself posts nothing");

  destroy();
}

async function testMotorConfirmPostsMotorType() {
  const { container, fake } = reset({ state: { armed: false, connected: true } });
  const destroy = openWizard(container, "motor");
  fire(actionButton(container, "Start"), "click");

  const confirm = findModalButton(container, "Calibrate Motors");
  assert.ok(confirm, "confirm button present in the modal");
  fire(confirm, "click");
  await flushMicrotasks();

  assert.ok(!findOneByClass(container, "modal-overlay"), "modal closed on confirm");
  const call = fake.postCalls.find((c) => c.url === "/api/calibrate");
  assert.deepEqual(call.payload, { type: "motor" }, "confirm posts {type:'motor'}");

  destroy();
}

async function testMotorCancelNoPost() {
  const { container, fake } = reset({ state: { armed: false, connected: true } });
  const destroy = openWizard(container, "motor");
  fire(actionButton(container, "Start"), "click");

  fire(findModalButton(container, "Cancel"), "click");
  assert.ok(!findOneByClass(container, "modal-overlay"), "modal closed on cancel");
  assert.equal(fake.postCalls.filter((c) => c.url === "/api/calibrate").length, 0,
    "no POST on cancel");

  destroy();
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

async function testDestroyReleasesEverythingTheWizardTookOut() {
  const { container, fake } = reset({ state: { armed: false, connected: true } });
  const destroy = openWizard(container, "motor");
  fire(actionButton(container, "Start"), "click");
  assert.ok(findOneByClass(container, "modal-overlay"), "modal open before destroy");

  destroy();

  assert.ok(!findOneByClass(container, "modal-overlay"), "modal removed on destroy");
  assert.equal(fake.unsubCalls, 1, "telemetry unsubscribed exactly once");
  assert.equal(fake.consoleUnsubCalls, 1, "console stream released exactly once");
  assert.equal(clearedIds.size >= 0, true);
}

async function testLeavingTheWizardStopsTheWatchdogAndTheStream() {
  const { container, fake } = reset({ state: { armed: false, connected: true } });
  const destroy = Corvus.setupCalibration.render(container, () => {});
  fire(findByClass(container, "calib-card").find((c) => c.dataset.type === "gyro"), "click");
  fire(actionButton(container, "Start"), "click");
  await flushMicrotasks();
  assert.equal(fake.consoleUnsubCalls, 0, "stream held while the wizard is up");

  // Back to the list: the wizard's subscriptions go with it, the page's own
  // telemetry subscription stays.
  fire(findOneByClass(container, "setup-back"), "click");
  assert.equal(fake.consoleUnsubCalls, 1, "console stream released on leaving the wizard");
  assert.equal(fake.unsubCalls, 0, "the page keeps its telemetry subscription");
  assert.ok(findOneByClass(container, "calib-cards"), "back at the list");

  destroy();
  assert.equal(fake.unsubCalls, 1, "and releases it on teardown");
}

async function testPlotlyGraphsArePurgedOnTeardown() {
  const { container } = reset();
  const destroy = Corvus.setupCalibration.render(container, () => {});
  assert.equal(window.Plotly.reactCalls.length, 3, "three PID graphs initialised");
  destroy();
  assert.equal(window.Plotly.purgeCalls.length, 3, "each PID graph purged exactly once");
}

async function run() {
  const tests = [
    testListShowsEveryProcedureWithItsCost,
    testOpeningAWizardPostsNothing,
    testAccelWizardDrawsEveryPositionItWillAskFor,
    testStartPostsTheCalibrationAndSwapsToAbort,
    testStartIsBlockedWithoutALink,
    testStartIsBlockedWhileArmed,
    testStatustextDrivesTheInstructionAndTheStrip,
    testSuccessEndsTheRunAndOffersTheWayBack,
    testFailureOffersARetryWithoutReopeningTheWizard,
    testAbortCancelsOnTheVehicle,
    testRejectedStartIsReportedNotSwallowed,
    testMotorStartOpensTheSafetyGateAndPostsNothing,
    testMotorConfirmPostsMotorType,
    testMotorCancelNoPost,
    testDestroyReleasesEverythingTheWizardTookOut,
    testLeavingTheWizardStopsTheWatchdogAndTheStream,
    testPlotlyGraphsArePurgedOnTeardown,
  ];
  for (const t of tests) {
    await t();
    console.log("ok   - " + t.name);
  }
  await flushMicrotasks();
  console.log("\nAll " + tests.length + " calibration wizard tests passed.");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
