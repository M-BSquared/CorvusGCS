"use strict";

/**
 * Frontend tests for the Setup -> Safety & Sensors page (Corvus.setupSafety).
 *
 * Plain Node-runnable assertions (no browser, no test runner) following the
 * same pattern as tests/test_frontend_setup.js: stub the globals the module
 * touches, require the source, and assert on the rendered DOM, the write spies,
 * and the teardown/no-leak behaviour. A small DOM stub (no HTML parser) is used
 * because the page builds its DOM via createElement, so a children-walking stub
 * suffices.
 *
 * The page is rendered directly rather than through the Setup orchestrator: the
 * sub-page owns its own lifecycle contract (render -> destroy), and testing it
 * on its own keeps these assertions independent of the tile grid.
 *
 * Run:
 *   node tests/test_frontend_safety.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals so the module loads and runs in Node.
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

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

window.matchMedia = (query) => ({
  matches: false, media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});

// Captures corvus:notification events so the write paths can fire them without
// a real event target.
let dispatched = [];
window.dispatchEvent = (event) => { dispatched.push(event); };

window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;
window.setInterval = () => 1;
window.clearInterval = () => {};

// ---------------------------------------------------------------------------
// Minimal DOM stub (mirrors tests/test_frontend_setup.js).
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {}, style: {},
    type: "", hidden: false, disabled: false, value: "", id: "",
    _attrs: {}, _listeners: {}, _isEl: true,
  };
  let _html = "";
  Object.defineProperty(e, "innerHTML", {
    get() { return _html; },
    set(v) { _html = String(v); if (_html === "") e.children.length = 0; },
  });
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) { const has = e.classList.contains(c); const next = force === undefined ? !has : !!force; if (next) e.classList.add(c); else e.classList.remove(c); return next; },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => { c.parentNode = e; e.children.push(c); return c; };
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; return c; };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (type, cb) => { (e._listeners[type] = e._listeners[type] || []).push(cb); };
  e.removeEventListener = () => {};
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  return e;
}

function querySel(children, sel) {
  const out = [];
  const wantTag = sel && sel[0] !== ".";
  const classes = sel ? sel.split(".").filter(Boolean) : [];
  function walk(list) {
    for (const c of list) {
      if (!c || !c._isEl) continue;
      const ok = wantTag ? c.tagName === sel.toUpperCase()
                         : classes.every((cl) => c.className.split(/\s+/).includes(cl));
      if (ok) out.push(c);
      if (c.children) walk(c.children);
    }
  }
  walk(children);
  return out;
}

global.document = {
  createElement: makeEl,
  createElementNS: (_ns, tag) => makeEl(tag),
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
};

// Helpers ---------------------------------------------------------------------
function flushMicrotasks() { return new Promise((r) => setTimeout(r, 0)); }
function findByClass(root, cls) { return root.querySelectorAll("." + cls); }
function findOneByClass(root, cls) { return root.querySelector("." + cls); }

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

function fire(el, type, detail) {
  const listeners = (el && el._listeners && el._listeners[type]) || [];
  const event = Object.assign({ preventDefault() {}, stopPropagation() {} }, detail || {});
  listeners.forEach((cb) => cb(event));
}

/** The editable control (not the row) that writes `param`. */
function control(container, param) {
  return findByDataset(container, "param", param)
    .filter((e) => e.tagName === "SELECT" || e.tagName === "INPUT")[0];
}

// ---------------------------------------------------------------------------
// Fake telemetry with spies. `subscribe` captures the callback (does NOT
// auto-fire — tests drive it via getSubCb) and returns an unsub spy.
// ---------------------------------------------------------------------------
function makeFakeTelemetry(opts = {}) {
  const postCalls = [];
  const requests = [];
  let subCb = null;
  let unsubCalls = 0;
  let state = opts.state || { armed: false, connected: true };
  let doc = opts.doc;
  // A write may be refused from the Nth call onward, so a chain that fails
  // halfway can be exercised (the vehicle accepting the driver and refusing
  // the estimator is a real failure mode, not a hypothetical one).
  const rejectFrom = opts.rejectFrom == null ? Infinity : opts.rejectFrom;
  const telemetry = {
    postAction(url, payload) {
      postCalls.push({ url, payload });
      if (postCalls.length >= rejectFrom) {
        return Promise.reject(new Error(opts.rejectMessage || "cannot set parameter while armed"));
      }
      return Promise.resolve({ ok: true });
    },
    requestJson(url) {
      requests.push(url);
      if (opts.requestError) return Promise.reject(new Error(opts.requestError));
      return Promise.resolve(doc);
    },
    getState() { return state; },
    subscribe(cb) { subCb = cb; return () => { unsubCalls++; }; },
  };
  return {
    telemetry, postCalls, requests,
    get unsubCalls() { return unsubCalls; },
    getSubCb() { return subCb; },
    setDoc(next) { doc = next; },
    setState(s) { state = s; },
    writes() { return postCalls.filter((c) => c.url === "/api/params/set").map((c) => c.payload); },
  };
}

// ---------------------------------------------------------------------------
// Load the module under test. ui.js first: it defines Corvus.ui, the component
// layer setup-shared.js reads at definition time (same order as index.html).
// ---------------------------------------------------------------------------
require("../src/js/ui.js");
require("../src/js/setup-shared.js");
require("../src/js/setup-safety.js");

// ---------------------------------------------------------------------------
// A representative /api/safety payload: one plain form section with both field
// kinds, and one sensor toggle carrying a bus driver, a serial driver and the
// external option — i.e. one instance of everything the page knows how to draw.
// ---------------------------------------------------------------------------
function safetyDoc(toggleOverrides) {
  return {
    connected: true,
    received: 14,
    sections: [
      {
        id: "limits", title: "Flight limits", kind: "fields",
        hint: "The envelope the vehicle is not allowed to leave.",
        fields: [
          { param: "GF_MAX_HOR_DIST", label: "Maximum distance", kind: "number",
            value: 500, unit: "m", step: 1,
            hint: "Horizontal distance from home the vehicle may reach." },
          { param: "GF_MAX_VER_DIST", label: "Maximum height", kind: "number",
            value: 120, unit: "m", step: 1 },
          { param: "BAT_LOW_THR", label: "Low threshold", kind: "number",
            value: 0.15, step: 0.01, min: 0, max: 1 },
          { param: "GF_ACTION", label: "Action at the limit", kind: "enum", value: 2,
            options: [{ value: 0, label: "None" }, { value: 2, label: "Hold" },
                      { value: 3, label: "Return" }] },
        ],
      },
      {
        id: "rangefinder", title: "Distance sensor", kind: "toggle",
        hint: "A downward lidar or sonar.",
        toggle: Object.assign({
          label: "Distance sensor",
          enabled: false,
          detail: "No driver enabled · fusion: Disabled",
          drivers: [
            { id: "SENS_EN_SF1XX:6", label: "Lightware SF/LW20/c",
              param: "SENS_EN_SF1XX", value: 6, serial: false },
            { id: "SENS_TFMINI_CFG", label: "Benewake TFmini (serial)",
              param: "SENS_TFMINI_CFG", value: null, serial: true },
            { id: "external", label: "External / MAVLink",
              param: null, value: null, serial: false },
          ],
          selected: "SENS_EN_SF1XX:6",
          ports: [{ value: 102, label: "TELEM 2" }, { value: 101, label: "TELEM 1" }],
          port: null,
          enable: [{ param: "EKF2_RNG_CTRL", value: 1 }],
          disable: [{ param: "EKF2_RNG_CTRL", value: 0 }],
          clear: [],
          reboot: true,
        }, toggleOverrides || {}),
        fields: [
          { param: "EKF2_RNG_A_HMAX", label: "Range aid maximum height", kind: "number",
            value: 7, unit: "m" },
        ],
      },
    ],
  };
}

/** Render the page with a canned /api/safety payload. */
async function open(fake) {
  dispatched = [];
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  const destroy = Corvus.setupSafety.render(container, () => {});
  await flushMicrotasks();
  return { container, destroy };
}

function openWith(doc, opts) {
  const fake = makeFakeTelemetry(Object.assign({ doc }, opts || {}));
  return open(fake).then((r) => Object.assign(r, { fake }));
}

// ===========================================================================
// PART A — the schema-driven form
// ===========================================================================

async function testRendersEverySectionFromTheSchema() {
  const { container, fake } = await openWith(safetyDoc());

  assert.ok(fake.requests.includes("/api/safety"), "the page reads /api/safety on open");
  const titles = findByClass(container, "page-section-title").map((t) => t.textContent);
  assert.deepEqual(titles, ["Flight limits", "Distance sensor"], "one card per section");

  const rows = findByClass(container, "safety-field");
  assert.equal(rows.length, 5, "four limit fields plus the rangefinder setting");
  assert.equal(findOneByClass(rows[0], "safety-field-label").textContent, "Maximum distance");
  assert.equal(findOneByClass(rows[0], "safety-unit").textContent, "m");

  assert.equal(control(container, "GF_MAX_HOR_DIST").value, "500",
    "the maximum distance is prefilled from the vehicle");
  assert.equal(control(container, "GF_ACTION").value, "2",
    "the geofence action reflects the value the vehicle holds");
  assert.ok(findOneByClass(container, "params-actions-status").className.includes("ok"),
    "the status line reports the read");
}

// The form layout lives in one CSS block, .pform-*, shared with the Motors
// page; each element carries that base class plus this page's modifier. Losing
// the base class would strip the layout while every other assertion here still
// passed, so the pairing is pinned.
async function testEveryFormPartCarriesTheSharedBaseClass() {
  const { container } = await openWith(safetyDoc());

  const pairs = [
    ["pform-grid", "safety-grid"],
    ["pform-field", "safety-field"],
    ["pform-field-label", "safety-field-label"],
    ["pform-field-control", "safety-field-control"],
    ["pform-unit", "safety-unit"],
    ["pform-select", "safety-select"],
    ["pform-input", "safety-input"],
    ["pform-field-hint", "safety-field-hint"],
  ];
  for (const [base, modifier] of pairs) {
    const found = findByClass(container, base);
    assert.ok(found.length, `${base} is rendered`);
    assert.ok(found.every((e) => e.className.split(/\s+/).includes(modifier)),
      `every .${base} also carries .${modifier}`);
  }
  // The hint keeps the generic .field-hint typography on top of both.
  assert.ok(findOneByClass(container, "pform-field-hint").className.includes("field-hint"));
}

async function testDisconnectedRendersAnExplanationNotAnError() {
  const { container } = await openWith({ connected: false, sections: [], received: 0 });

  assert.equal(findByClass(container, "safety-field").length, 0, "no editable fields");
  assert.ok(findOneByClass(container, "params-desc"), "an explanation is shown instead");
  assert.ok(findOneByClass(container, "params-actions-status").className.includes("err"),
    "the status line reports the failure");
}

async function testAFailedReadIsReportedNotThrown() {
  const fake = makeFakeTelemetry({ doc: safetyDoc(), requestError: "link died" });
  const { container } = await open(fake);
  const status = findOneByClass(container, "params-actions-status");
  assert.ok(status.className.includes("err"), "the read failure is shown");
  assert.match(status.textContent, /link died/);
}

async function testAFieldWriteGoesThroughTheParameterEndpoint() {
  const { container, fake } = await openWith(safetyDoc());

  const input = control(container, "GF_MAX_VER_DIST");
  input.value = "80";
  fire(input, "change");
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [{ name: "GF_MAX_VER_DIST", value: 80 }],
    "the field writes the parameter it names, as a number");
}

async function testANumberFieldRejectsGarbageBeforeItReachesTheAircraft() {
  const { container, fake } = await openWith(safetyDoc());

  const input = control(container, "GF_MAX_HOR_DIST");
  input.value = "far";
  fire(input, "change");
  await flushMicrotasks();

  assert.equal(fake.writes().length, 0, "a non-numeric limit is never written to the aircraft");
  assert.ok(input.className.includes("invalid"), "the field is marked invalid");

  input.value = "250";
  fire(input, "change");
  await flushMicrotasks();
  assert.deepEqual(fake.writes(), [{ name: "GF_MAX_HOR_DIST", value: 250 }]);
}

// The battery thresholds are a fraction of a pack. A 15 typed where 0.15 belongs
// would park the low-battery failsafe permanently past its trigger, so the
// schema's bounds are enforced before the write leaves the browser.
async function testAValueOutsideTheSchemaBoundsIsRefused() {
  const { container, fake } = await openWith(safetyDoc());

  const input = control(container, "BAT_LOW_THR");
  input.value = "15";
  fire(input, "change");
  await flushMicrotasks();
  assert.equal(fake.writes().length, 0, "an out-of-range threshold never reaches the aircraft");
  assert.ok(input.className.includes("invalid"), "the field is marked invalid");

  input.value = "-0.1";
  fire(input, "change");
  await flushMicrotasks();
  assert.equal(fake.writes().length, 0, "nor does one below the minimum");

  input.value = "0.2";
  fire(input, "change");
  await flushMicrotasks();
  assert.deepEqual(fake.writes(), [{ name: "BAT_LOW_THR", value: 0.2 }],
    "a value inside the bounds is written");
}

// A refused write must not leave a field claiming an envelope the aircraft is
// not flying in — the control snaps back to the value the vehicle still holds.
async function testARefusedFieldWriteRestoresTheControl() {
  const { container, fake } = await openWith(safetyDoc(), { rejectFrom: 1 });

  const select = control(container, "GF_ACTION");
  select.value = "3";
  fire(select, "change");
  await flushMicrotasks();

  assert.equal(select.value, "2", "the action select snaps back after a refused write");
  const note = dispatched.filter((e) => e.type === "corvus:notification").pop();
  assert.equal(note.detail.level, "critical", "the operator is told the write failed");
}

// ===========================================================================
// PART B — the sensor toggle
// ===========================================================================

async function testTheSensorHeaderShowsTheStateAndThePickers() {
  const { container } = await openWith(safetyDoc());

  const header = findOneByClass(container, "safety-sensor");
  assert.equal(header.dataset.sensor, "rangefinder");
  assert.equal(findOneByClass(header, "safety-sensor-label").textContent, "Distance sensor");
  assert.match(findOneByClass(header, "safety-sensor-detail").textContent,
    /No driver enabled · fusion: Disabled/,
    "the state line names the driver AND the estimator");

  const sw = findOneByClass(container, "safety-sensor-switch");
  assert.equal(sw.getAttribute("aria-checked"), "false", "the switch reflects the sensor state");
  assert.equal(findOneByClass(container, "safety-driver-select").value, "SENS_EN_SF1XX:6");
}

// The port only means something for a serial sensor, so it must not sit there
// implying it is being written for an I2C one.
async function testThePortPickerFollowsTheSelectedDriver() {
  const { container } = await openWith(safetyDoc());

  const port = findOneByClass(container, "safety-port-select");
  assert.ok(port.parentNode.hidden, "hidden while a bus driver is selected");

  const driver = findOneByClass(container, "safety-driver-select");
  driver.value = "SENS_TFMINI_CFG";
  fire(driver, "change");
  assert.equal(port.parentNode.hidden, false, "shown once a serial driver is selected");
}

// The whole point of the page: one press starts the driver AND tells the
// estimator to fuse it. Half of that chain is the classic PX4 trap.
async function testEnablingASensorWritesTheWholeChain() {
  const { container, fake } = await openWith(safetyDoc());

  fire(findOneByClass(container, "safety-sensor-switch"), "click");
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [
    { name: "SENS_EN_SF1XX", value: 6 },
    { name: "EKF2_RNG_CTRL", value: 1 },
  ], "driver first, then the estimator");

  const note = dispatched.filter((e) => e.type === "corvus:notification").pop();
  assert.match(note.detail.message, /reboot/, "the operator is told a reboot is needed");
  assert.equal(fake.requests.filter((u) => u === "/api/safety").length, 2,
    "the page re-reads itself after the chain");
}

async function testEnablingASerialSensorWritesTheChosenPort() {
  const { container, fake } = await openWith(safetyDoc());

  const driver = findOneByClass(container, "safety-driver-select");
  driver.value = "SENS_TFMINI_CFG";
  fire(driver, "change");
  const port = findOneByClass(container, "safety-port-select");
  port.value = "101";

  fire(findOneByClass(container, "safety-sensor-switch"), "click");
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [
    { name: "SENS_TFMINI_CFG", value: 101 },
    { name: "EKF2_RNG_CTRL", value: 1 },
  ], "a serial rangefinder is enabled by naming its port");
}

// Two rangefinder drivers must never claim the same bus at once, so the one
// being replaced is switched off before the new one is switched on.
async function testSwitchingDriverZeroesTheOldOneFirst() {
  const doc = safetyDoc({
    enabled: true, selected: "SENS_TFMINI_CFG", port: 102,
    clear: ["SENS_TFMINI_CFG"],
    detail: "Benewake TFmini (serial) · fusion: Conditional (range aid)",
  });
  const { container, fake } = await openWith(doc);

  const driver = findOneByClass(container, "safety-driver-select");
  driver.value = "SENS_EN_SF1XX:6";
  fire(driver, "change");
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [
    { name: "SENS_TFMINI_CFG", value: 0 },
    { name: "SENS_EN_SF1XX", value: 6 },
    { name: "EKF2_RNG_CTRL", value: 1 },
  ], "old driver off, new driver on, estimator last");
}

async function testDisablingASensorTakesDownBothHalves() {
  const doc = safetyDoc({
    enabled: true, selected: "SENS_EN_SF1XX:6", clear: ["SENS_EN_SF1XX"],
    detail: "Lightware SF/LW20/c · fusion: Conditional (range aid)",
  });
  const { container, fake } = await openWith(doc);

  const sw = findOneByClass(container, "safety-sensor-switch");
  assert.equal(sw.getAttribute("aria-checked"), "true", "the switch starts on");
  fire(sw, "click");
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [
    { name: "SENS_EN_SF1XX", value: 0 },
    { name: "EKF2_RNG_CTRL", value: 0 },
  ], "driver off, then the estimator");
}

// A refused write must never leave an "on" switch over a sensor that is still
// off — the operator would fly believing the lidar was live.
async function testARefusedEnableSnapsTheSwitchBack() {
  const { container, fake } = await openWith(safetyDoc(), {
    rejectFrom: 1, rejectMessage: "parameter write failed",
  });

  const sw = findOneByClass(container, "safety-sensor-switch");
  fire(sw, "click");
  await flushMicrotasks();
  await flushMicrotasks();

  assert.equal(sw.getAttribute("aria-checked"), "false", "the switch returns to off");
  const note = dispatched.filter((e) => e.type === "corvus:notification").pop();
  assert.equal(note.detail.level, "critical", "the operator is told it failed");
  assert.ok(fake.requests.filter((u) => u === "/api/safety").length >= 2,
    "the page re-reads so it shows the half-applied reality");
}

// A chain refused halfway is the worst case: the old driver is off, the new one
// is on, and the estimator was never told. The page must stop at the refusal and
// must not claim success.
async function testAChainRefusedHalfwayStopsAndIsReported() {
  // A stale serial driver still configured, so the chain is three writes long
  // and a refusal in the middle is observable.
  const doc = safetyDoc({ clear: ["SENS_TFMINI_CFG"] });
  const { container, fake } = await openWith(doc, { rejectFrom: 3 });

  const sw = findOneByClass(container, "safety-sensor-switch");
  fire(sw, "click");
  await flushMicrotasks();
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [
    { name: "SENS_TFMINI_CFG", value: 0 },
    { name: "SENS_EN_SF1XX", value: 6 },
    { name: "EKF2_RNG_CTRL", value: 1 },
  ], "the estimator write is the one that was refused");
  assert.equal(sw.getAttribute("aria-checked"), "false", "the switch does not claim success");

  const note = dispatched.filter((e) => e.type === "corvus:notification").pop();
  assert.equal(note.detail.level, "critical", "the half-applied chain is reported");
}

// The write after a refused one is never attempted — a chain that kept going
// would leave a configuration nobody asked for.
async function testAChainStopsAtTheFirstRefusal() {
  const doc = safetyDoc({ clear: ["SENS_TFMINI_CFG"] });
  const { container, fake } = await openWith(doc, { rejectFrom: 2 });

  fire(findOneByClass(container, "safety-sensor-switch"), "click");
  await flushMicrotasks();
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [
    { name: "SENS_TFMINI_CFG", value: 0 },
    { name: "SENS_EN_SF1XX", value: 6 },
  ], "nothing is attempted after the refused write");
}

// ===========================================================================
// PART C — armed gating and teardown
// ===========================================================================

async function testArmedGatingDisablesEveryControl() {
  const fake = makeFakeTelemetry({ doc: safetyDoc(), state: { connected: true, armed: false } });
  const { container } = await open(fake);

  const banner = findOneByClass(container, "params-banner");
  const controls = findByClass(container, "safety-select")
    .concat(findByClass(container, "safety-input"))
    .concat(findByClass(container, "safety-sensor-switch"));
  assert.ok(controls.length >= 6, "controls rendered");
  assert.ok(banner.hidden, "no banner while disarmed");
  assert.ok(controls.every((c) => !c.disabled), "controls editable while disarmed");

  fake.getSubCb()({ connected: true, armed: true });
  assert.ok(!banner.hidden, "armed banner shown");
  assert.ok(controls.every((c) => c.disabled), "every control disabled while armed");

  fake.getSubCb()({ connected: true, armed: false });
  assert.ok(banner.hidden, "banner hidden again on disarm");
  assert.ok(controls.every((c) => !c.disabled), "controls editable again on disarm");
}

async function testAnArmedSwitchWritesNothing() {
  const fake = makeFakeTelemetry({ doc: safetyDoc(), state: { connected: true, armed: true } });
  const { container } = await open(fake);

  fire(findOneByClass(container, "safety-sensor-switch"), "click");
  await flushMicrotasks();
  assert.equal(fake.writes().length, 0, "a disabled switch never reaches the vehicle");
}

async function testTeardownReleasesTheSubscription() {
  const fake = makeFakeTelemetry({ doc: safetyDoc() });
  const { destroy } = await open(fake);
  const before = fake.unsubCalls;

  destroy();
  assert.equal(fake.unsubCalls, before + 1, "the telemetry subscription is released");
  destroy();
  assert.equal(fake.unsubCalls, before + 1, "destroy is idempotent — no double unsub");
}

// A response that lands after teardown must not touch a container the page no
// longer owns.
async function testALateResponseAfterTeardownIsIgnored() {
  const fake = makeFakeTelemetry({ doc: safetyDoc() });
  dispatched = [];
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  const destroy = Corvus.setupSafety.render(container, () => {});
  destroy();
  await flushMicrotasks();

  assert.equal(findByClass(container, "safety-field").length, 0,
    "nothing was rendered into the abandoned container");
}

// ---------------------------------------------------------------------------
async function main() {
  const tests = [
    testRendersEverySectionFromTheSchema,
    testEveryFormPartCarriesTheSharedBaseClass,
    testDisconnectedRendersAnExplanationNotAnError,
    testAFailedReadIsReportedNotThrown,
    testAFieldWriteGoesThroughTheParameterEndpoint,
    testANumberFieldRejectsGarbageBeforeItReachesTheAircraft,
    testAValueOutsideTheSchemaBoundsIsRefused,
    testARefusedFieldWriteRestoresTheControl,
    testTheSensorHeaderShowsTheStateAndThePickers,
    testThePortPickerFollowsTheSelectedDriver,
    testEnablingASensorWritesTheWholeChain,
    testEnablingASerialSensorWritesTheChosenPort,
    testSwitchingDriverZeroesTheOldOneFirst,
    testDisablingASensorTakesDownBothHalves,
    testARefusedEnableSnapsTheSwitchBack,
    testAChainRefusedHalfwayStopsAndIsReported,
    testAChainStopsAtTheFirstRefusal,
    testArmedGatingDisablesEveryControl,
    testAnArmedSwitchWritesNothing,
    testTeardownReleasesTheSubscription,
    testALateResponseAfterTeardownIsIgnored,
  ];
  for (const t of tests) {
    await t();
    console.log("  ok", t.name);
  }
  console.log(`\n${tests.length} passed — Setup > Safety & Sensors`);
}

main().catch((err) => { console.error(err); process.exit(1); });
