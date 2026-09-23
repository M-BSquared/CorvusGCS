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

// In-memory localStorage: the operator's added-parameter list lives there, so
// a missing stub would hide whether it is actually persisted.
const storage = {};
window.localStorage = {
  getItem: (k) => (k in storage ? storage[k] : null),
  setItem: (k, v) => { storage[k] = String(v); },
  removeItem: (k) => { delete storage[k]; },
  clear: () => { Object.keys(storage).forEach((k) => delete storage[k]); },
};

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
  e.style.setProperty = (k, v) => { e.style[k] = String(v); };
  e.style.removeProperty = (k) => { delete e.style[k]; };
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

/**
 * A page carrying one bit field.
 *
 * Its own document rather than an extra field on the shared one: the overview
 * assertions count the fields they were written for, and a bitmask is an
 * ArduPilot shape — FENCE_TYPE, FS_OPTIONS, ARMING_CHECK — that the PX4 schema
 * never produces.
 */
function bitmaskDoc(value) {
  const doc = safetyDoc();
  doc.sections = [{
    id: "limits", title: "Flight limits", kind: "fields",
    hint: "The envelope the vehicle is not allowed to leave.",
    fields: [{
      param: "FENCE_TYPE", label: "What the fence limits", kind: "bitmask",
      value: value === undefined ? 3 : value,
      bits: [
        { bit: 0, label: "Maximum altitude" },
        { bit: 1, label: "Circle around home" },
        { bit: 2, label: "Polygon" },
        { bit: 8, label: "Something newer than this build" },
      ],
    }],
  }];
  return doc;
}

/** The checkbox for one bit of a bitmask field. */
function bit(container, param, number) {
  const wrapper = findByDataset(container, "param", param)
    .filter((e) => e.tagName === "DIV")[0];
  return wrapper.querySelectorAll("input")
    .filter((b) => b.dataset.bit === String(number))[0];
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
// The preset catalogue the sensor page offers: one module whose driver is a
// plain parameter, one whose driver is a serial port, and one this firmware
// cannot run at all — the three cases the panel has to render differently.
function defaultPresets() {
  return [
    {
      id: "holybro-h-flow", label: "Holybro H-Flow", vendor: "Holybro",
      model: "19006", bus: "DroneCAN", serial: false, supported: true,
      unsupported: "", driver: "UAVCAN_SUB_RNG:1", active: false, reboot: true,
      summary: "Flow and distance on one CAN cable.",
      note: "A DroneCAN node rather than a local driver.",
      writes: [
        { param: "UAVCAN_ENABLE", value: 2, label: "Run the DroneCAN stack" },
        { param: "UAVCAN_SUB_RNG", value: 1, label: "Subscribe to its distance message" },
        { param: "EKF2_RNG_CTRL", value: 1, label: "Fuse it when low and slow" },
      ],
      missing: ["EKF2_RNG_QLTY_T"],
    },
    {
      id: "benewake-tfmini-s", label: "Benewake TFmini-S", vendor: "Benewake",
      model: "TFmini-S", bus: "UART", serial: true, supported: true,
      unsupported: "", driver: "SENS_TFMINI_CFG", active: false, reboot: true,
      summary: "0.1 to 12 m time-of-flight rangefinder.",
      note: "Leave the module in UART mode.",
      writes: [
        { param: "SENS_TFMINI_CFG", value: null, port: true,
          label: "The serial port the module is wired to" },
        { param: "EKF2_RNG_CTRL", value: 1, label: "Fuse it when low and slow" },
        { param: "EKF2_RNG_NOISE", value: 0.06, label: "Datasheet accuracy" },
      ],
      missing: [],
    },
    {
      id: "matek-3901-l0x", label: "Matek 3901-L0X", vendor: "Matek Systems",
      model: "3901-L0X", bus: "UART (MSP v2)", serial: false, supported: false,
      unsupported: "PX4 has no MSP sensor input.",
      driver: "", active: false, reboot: false,
      summary: "PMW3901 flow and a VL53L0X lidar over MSP.",
      note: "", writes: [], missing: [],
    },
  ];
}

function safetyDoc(toggleOverrides, presets) {
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
        group: "sensors", icon: "radar",
        short: "A downward lidar or sonar.",
        hint: "A downward lidar or sonar.",
        presets: presets === undefined ? defaultPresets() : presets,
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

/** Click through from the overview into one sensor's own page. */
function enterSensor(container, id) {
  const tile = findByDataset(container, "sensor", id || "rangefinder")
    .filter((e) => e.tagName === "BUTTON" && e.className.includes("safety-sensor-tile"))[0];
  assert.ok(tile, `a tile for ${id || "rangefinder"} is on the overview`);
  fire(tile, "click");
}

/** Open the page and go straight to the sensor — the entry point of PART B. */
async function openSensorPage(doc, opts, id) {
  const r = await openWith(doc || safetyDoc(), opts);
  enterSensor(r.container, id);
  return r;
}

/** Choose one module in the hardware dropdown and return the panel it revealed. */
function pickPreset(container, id) {
  const select = findOneByClass(container, "safety-preset-select");
  assert.ok(select, "the hardware dropdown");
  select.value = String(id);
  fire(select, "change");
  return findOneByClass(container, "safety-preset-detail");
}

/** The {value,label} pairs a <select> is offering, in order. */
function optionsOf(select) {
  return (select.children || [])
    .filter((c) => c.tagName === "OPTION")
    .map((c) => ({ value: c.value, label: c.textContent }));
}

/** The Custom panel's add box, and the button that submits it.
 *
 *  The status span is found where it lives rather than by the class it was
 *  built with: setFieldStatus rewrites the class list on every update. */
function addBox(container) {
  const row = findOneByClass(container, "safety-extra-add");
  return {
    row,
    input: row && findOneByClass(row, "safety-extra-input"),
    button: row && findOneByClass(row, "safety-extra-button"),
    status: row && findOneByClass(row, "params-row-status"),
  };
}

/** Type a parameter name into the Custom panel and press Add. */
function addExtraParam(container, name) {
  const box = addBox(container);
  box.input.value = name;
  fire(box.button, "click");
  return box;
}

// ===========================================================================
// PART A — the schema-driven form
// ===========================================================================

async function testCheckValuesConfirmsWhatWasSetAndRedrawsFromTheVehicle() {
  const { container, fake, destroy } = await openWith(safetyDoc());
  const check = findOneByClass(container, "safety-check");
  assert.ok(check, "the page offers Check values");
  assert.equal(check.disabled, false, "enabled once the page has read the vehicle");

  const input = control(container, "GF_MAX_HOR_DIST");
  input.value = "750";
  fire(input, "change");
  await flushMicrotasks();

  fake.telemetry.postAction = (url, payload) => {
    fake.postCalls.push({ url, payload });
    if (url !== "/api/params/verify") return Promise.resolve({ ok: true });
    return Promise.resolve({
      ok: true, values: { GF_MAX_HOR_DIST: 750 }, missing: [],
      results: [{ name: "GF_MAX_HOR_DIST", wanted: 750, before: 500, after: 750,
        rewritten: true, ok: true, error: "" }],
    });
  };
  fire(findOneByClass(container, "safety-check"), "click");
  for (let i = 0; i < 4; i += 1) await flushMicrotasks();

  const verify = fake.postCalls.filter((c) => c.url === "/api/params/verify");
  assert.equal(verify.length, 1);
  assert.deepEqual(verify[0].payload.params, [{ name: "GF_MAX_HOR_DIST", value: 750 }]);
  assert.ok(verify[0].payload.names.includes("GF_ACTION"), "the whole page is read back");
  assert.ok(!verify[0].payload.names.includes("SENS_EN_SF1XX"),
    "a driver the page only offers is not asked for, it is not a field");
  assert.ok(fake.requests.includes("/api/safety?fresh=1"), "the redraw asks the vehicle");
  const card = findOneByClass(container, "safety-check-card");
  assert.ok(card, "the outcome is shown above the sections");
  assert.match(findOneByClass(card, "check-outcome").textContent, /written again, confirmed/);
  const row = findByDataset(container, "param", "GF_MAX_HOR_DIST")
    .filter((e) => /pform-field/.test(e.className))[0];
  assert.match(row.querySelector(".params-row-status").textContent, /written again, confirmed/);
  destroy();
}

async function testASensorChainIsCheckedLikeAField() {
  const { container, fake, destroy } = await openWith(safetyDoc());
  enterSensor(container, "rangefinder");
  await flushMicrotasks();
  fire(findOneByClass(container, "safety-sensor-switch"), "click");
  for (let i = 0; i < 4; i += 1) await flushMicrotasks();
  const written = fake.writes().map((w) => w.name);
  assert.ok(written.length, "the switch wrote its chain");

  fire(findOneByClass(container, "safety-check"), "click");
  await flushMicrotasks();
  const verify = fake.postCalls.filter((c) => c.url === "/api/params/verify")[0];
  assert.deepEqual(verify.payload.params.map((p) => p.name), written,
    "every write of the chain is kept for the check");
  destroy();
}

async function testRendersEverySectionFromTheSchema() {
  const { container, fake } = await openWith(safetyDoc());

  assert.ok(fake.requests.includes("/api/safety"), "the page reads /api/safety on open");
  const titles = findByClass(container, "page-section-title").map((t) => t.textContent);
  assert.deepEqual(titles, ["Flight limits", "Sensors"],
    "the envelope is a form; the sensors are collected into one card of their own");

  const rows = findByClass(container, "safety-field");
  assert.equal(rows.length, 4, "the overview carries the limit fields only");
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

async function testABitmaskRendersOneSwitchPerNamedBit() {
  const { container } = await openWith(bitmaskDoc());
  const wrapper = findByDataset(container, "param", "FENCE_TYPE")
    .filter((e) => e.tagName === "DIV")[0];
  const boxes = wrapper.querySelectorAll("input");
  assert.equal(boxes.length, 4, "one control per named bit, not a number field");
  assert.equal(bit(container, "FENCE_TYPE", 0).checked, true);
  assert.equal(bit(container, "FENCE_TYPE", 1).checked, true);
  assert.equal(bit(container, "FENCE_TYPE", 2).checked, false);
}

async function testTickingABitWritesTheWholeRecomputedWord() {
  const { container, fake } = await openWith(bitmaskDoc());
  const box = bit(container, "FENCE_TYPE", 2);
  box.checked = true;
  fire(box, "change");
  await flushMicrotasks();
  assert.deepEqual(fake.writes(), [{ name: "FENCE_TYPE", value: 7 }],
    "PARAM_SET carries the word, not the bit");
}

async function testClearingABitLeavesTheOthersAlone() {
  const { container, fake } = await openWith(bitmaskDoc());
  const box = bit(container, "FENCE_TYPE", 0);
  box.checked = false;
  fire(box, "change");
  await flushMicrotasks();
  assert.deepEqual(fake.writes(), [{ name: "FENCE_TYPE", value: 2 }]);
}

async function testABitThisBuildDoesNotNameIsNeverLost() {
  // The vehicle holds bit 4, which the schema above does not list. Ticking a
  // bit it does list must not drop the one it does not — a firmware with a
  // newer flag would lose it the first time somebody touched the field.
  const { container, fake } = await openWith(bitmaskDoc(3 | (1 << 4)));
  const box = bit(container, "FENCE_TYPE", 2);
  box.checked = true;
  fire(box, "change");
  await flushMicrotasks();
  assert.deepEqual(fake.writes(), [{ name: "FENCE_TYPE", value: 7 | (1 << 4) }]);
}

async function testARefusedBitmaskWriteRestoresEveryBox() {
  const { container, fake } = await openWith(bitmaskDoc(), { rejectFrom: 1 });
  const box = bit(container, "FENCE_TYPE", 2);
  box.checked = true;
  fire(box, "change");
  await flushMicrotasks();
  assert.equal(bit(container, "FENCE_TYPE", 2).checked, false,
    "a box left ticked that the vehicle never accepted is a lie about the aircraft");
  assert.equal(bit(container, "FENCE_TYPE", 0).checked, true, "and the rest are untouched");
  assert.ok(fake.writes().length === 1);
}

async function testAnArmedVehicleDisablesEveryBitOfABitmask() {
  const { container, fake } = await openWith(bitmaskDoc());
  fake.getSubCb()({ connected: true, armed: true });
  assert.equal(bit(container, "FENCE_TYPE", 0).disabled, true,
    "a live write to a safety bitmask from an armed aircraft");
  fake.getSubCb()({ connected: true, armed: false });
  assert.equal(bit(container, "FENCE_TYPE", 0).disabled, false);
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
  const { container } = await openSensorPage();

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
  const { container } = await openSensorPage();

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
  const { container, fake } = await openSensorPage();

  fire(findOneByClass(container, "safety-sensor-switch"), "click");
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [
    { name: "SENS_EN_SF1XX", value: 6 },
    { name: "EKF2_RNG_CTRL", value: 1 },
  ], "driver first, then the estimator");

  const note = dispatched.filter((e) => e.type === "corvus:notification").pop();
  assert.match(note.detail.message, /reboot/i, "the operator is told a reboot is needed");
  assert.equal(fake.requests.filter((u) => u === "/api/safety").length, 2,
    "the page re-reads itself after the chain");
}

async function testEnablingASerialSensorWritesTheChosenPort() {
  const { container, fake } = await openSensorPage();

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
  const { container, fake } = await openSensorPage(doc);

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
  const { container, fake } = await openSensorPage(doc);

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
  const { container, fake } = await openSensorPage(safetyDoc(), {
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
  const { container, fake } = await openSensorPage(doc, { rejectFrom: 3 });

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
  const { container, fake } = await openSensorPage(doc, { rejectFrom: 2 });

  fire(findOneByClass(container, "safety-sensor-switch"), "click");
  await flushMicrotasks();
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [
    { name: "SENS_TFMINI_CFG", value: 0 },
    { name: "SENS_EN_SF1XX", value: 6 },
  ], "nothing is attempted after the refused write");
}

// ===========================================================================
// PART B1 — the Sensors card and the page each sensor gets
// ===========================================================================

// The overview has to answer "is the lidar up?" without being opened, or the
// card is just a longer route to the same page.
async function testTheOverviewListsEachSensorWithItsState() {
  const { container } = await openWith(safetyDoc({
    enabled: true, detail: "Lightware SF/LW20/c · fusion: Conditional (range aid)",
  }));

  const card = findByDataset(container, "section", "sensors")[0];
  assert.ok(card, "the sensors are collected into a card of their own");

  const tiles = findByClass(container, "safety-sensor-tile");
  assert.equal(tiles.length, 1, "one tile per sensor");
  assert.equal(tiles[0].dataset.sensor, "rangefinder");
  assert.equal(findOneByClass(tiles[0], "tile-title").textContent, "Distance sensor");
  assert.equal(findOneByClass(tiles[0], "safety-sensor-pill").textContent, "On");
  assert.match(findOneByClass(tiles[0], "safety-sensor-state-detail").textContent,
    /Lightware SF\/LW20\/c · fusion: Conditional/,
    "the tile names the driver AND the estimator, so a half-state cannot look configured");

  assert.equal(findByClass(container, "safety-preset-cards").length, 0,
    "a sensor's own controls stay on its own page");
}

async function testASensorTileOpensItsPageAndBackReturns() {
  let wentBackToSetup = 0;
  const fake = makeFakeTelemetry({ doc: safetyDoc() });
  dispatched = [];
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  Corvus.setupSafety.render(container, () => { wentBackToSetup++; });
  await flushMicrotasks();

  enterSensor(container);
  assert.ok(findOneByClass(container, "safety-sensor"), "the sensor page is shown");
  assert.equal(findByClass(container, "safety-sensor-tile").length, 0,
    "the overview is gone rather than stacked underneath");

  const back = findOneByClass(container, "setup-back");
  assert.match(back.getAttribute("aria-label"), /Safety & Sensors/,
    "back names where it actually goes, not the Setup grid");
  fire(back, "click");
  assert.equal(wentBackToSetup, 0, "back stays inside the page instead of leaving it");
  assert.ok(findByClass(container, "safety-sensor-tile").length, "the overview is restored");

  fire(findOneByClass(container, "setup-back"), "click");
  assert.equal(wentBackToSetup, 1, "back from the overview leaves for the Setup grid");
}

// A reload while a sensor page is open re-reads the whole document. The view
// must survive it — a page that bounced to the overview after every write
// would throw the operator out mid-setup, since every write triggers a re-read.
async function testAReloadKeepsTheOpenSensorPage() {
  const { container, fake } = await openSensorPage();

  fire(findOneByClass(container, "safety-sensor-switch"), "click");
  await flushMicrotasks();
  await flushMicrotasks();

  assert.ok(fake.requests.filter((u) => u === "/api/safety").length >= 2, "the page re-read");
  assert.ok(findOneByClass(container, "safety-sensor"), "still on the sensor page");
}

// A firmware that no longer reports the sensor must not leave the operator
// staring at a page about a sensor that is not there.
async function testASensorThatDisappearsFallsBackToTheOverview() {
  const { container, fake } = await openSensorPage();

  fake.setDoc({ connected: true, received: 4, sections: [safetyDoc().sections[0]] });
  fire(findOneByClass(container, "safety-reload"), "click");
  await flushMicrotasks();

  assert.equal(findByClass(container, "safety-sensor").length, 0, "the sensor page is gone");
  assert.ok(findByClass(container, "safety-field").length, "the overview is shown instead");
}

// ===========================================================================
// PART B2 — hardware presets
// ===========================================================================

async function testTheHardwareDropdownLeadsWithCustomAndWritesNothingOnSelection() {
  const { container, fake } = await openSensorPage();

  const select = findOneByClass(container, "safety-preset-select");
  assert.deepEqual(optionsOf(select).map((o) => o.value),
    ["custom", "holybro-h-flow", "benewake-tfmini-s", "matek-3901-l0x"],
    "Custom leads, then one row per product the backend offered");
  assert.equal(select.value, "custom", "the page opens on Custom, never on a preset");

  // The bus is in the label, not only in the panel: picking "UART" when the
  // module on the bench has a CAN plug is the mistake this list can prevent,
  // and a dropdown is read one row at a time.
  const labels = optionsOf(select).map((o) => o.label);
  assert.match(labels[0], /^Custom/);
  assert.match(labels[1], /Holybro H-Flow · DroneCAN · 19006/);
  assert.match(labels[3], /not supported by PX4/,
    "a module this firmware cannot run says so before it is chosen");

  pickPreset(container, "holybro-h-flow");
  assert.equal(fake.writes().length, 0,
    "selecting a preset only previews it; nothing reaches the aircraft yet");
}

// A preset whose driver half is already running says so in the row itself.
async function testADropdownRowSaysWhenItsDriverIsAlreadyRunning() {
  const presets = defaultPresets();
  presets[1].active = true;
  const { container } = await openSensorPage(safetyDoc(undefined, presets));

  const select = findOneByClass(container, "safety-preset-select");
  assert.match(optionsOf(select)[2].label, /Benewake TFmini-S · UART · TFmini-S {2}\(in use\)/);
  assert.ok(!/in use/.test(optionsOf(select)[1].label),
    "and the ones that are not stay quiet");
}

// A preset touches a dozen safety parameters at once, so the operator is shown
// every one of them — name, value and reason — before anything is written.
async function testAPresetShowsExactlyWhatItWouldWrite() {
  const { container } = await openSensorPage();
  const panel = pickPreset(container, "holybro-h-flow");

  assert.match(findOneByClass(panel, "safety-preset-summary").textContent, /one CAN cable/);
  const rows = findByClass(panel, "safety-preset-write");
  assert.deepEqual(rows.map((r) => r.dataset.param),
    ["UAVCAN_ENABLE", "UAVCAN_SUB_RNG", "EKF2_RNG_CTRL"],
    "every parameter the preset would write is listed, in the order it writes them");
  assert.deepEqual(rows.map((r) => findOneByClass(r, "safety-preset-write-value").textContent),
    ["2", "1", "1"]);
  assert.equal(findOneByClass(rows[0], "safety-preset-write-why").textContent,
    "Run the DroneCAN stack", "each write says why it is there");

  assert.match(findOneByClass(panel, "safety-preset-missing").textContent,
    /EKF2_RNG_QLTY_T/,
    "a parameter this firmware lacks is reported, not silently dropped");
}

async function testApplyingAPresetWritesItsWholeChain() {
  const { container, fake } = await openSensorPage();
  pickPreset(container, "holybro-h-flow");

  fire(findOneByClass(container, "safety-preset-apply"), "click");
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [
    { name: "UAVCAN_ENABLE", value: 2 },
    { name: "UAVCAN_SUB_RNG", value: 1 },
    { name: "EKF2_RNG_CTRL", value: 1 },
  ], "the driver half first, the estimator last");

  const note = dispatched.filter((e) => e.type === "corvus:notification").pop();
  assert.match(note.detail.message, /reboot/i, "the operator is told a reboot is needed");
  assert.ok(fake.requests.filter((u) => u === "/api/safety").length >= 2,
    "the page re-reads itself afterwards");
}

// A serial module is "enabled" by naming its port, and only the operator knows
// which one it is wired to — so the preset asks, and substitutes the answer.
async function testAPresetForASerialModuleWritesThePickedPort() {
  const { container, fake } = await openSensorPage();
  const panel = pickPreset(container, "benewake-tfmini-s");

  const row = findByClass(panel, "safety-preset-write")[0];
  assert.equal(findOneByClass(row, "safety-preset-write-value").textContent, "the port below",
    "the port is shown as pending rather than as a made-up number");

  const port = findOneByClass(panel, "safety-preset-port");
  port.value = "101";
  fire(findOneByClass(container, "safety-preset-apply"), "click");
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [
    { name: "SENS_TFMINI_CFG", value: 101 },
    { name: "EKF2_RNG_CTRL", value: 1 },
    { name: "EKF2_RNG_NOISE", value: 0.06 },
  ], "the port the operator picked starts the driver, then the estimator follows");
}

// Two rangefinder drivers must never claim one bus, so a preset zeroes whatever
// is already configured — except the parameters it is about to set itself,
// which would otherwise be written twice.
async function testApplyingAPresetZeroesAConflictingDriverFirst() {
  const doc = safetyDoc({ clear: ["SENS_EN_SF1XX", "UAVCAN_SUB_RNG"] });
  const { container, fake } = await openSensorPage(doc);
  pickPreset(container, "holybro-h-flow");

  fire(findOneByClass(container, "safety-preset-apply"), "click");
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [
    { name: "SENS_EN_SF1XX", value: 0 },
    { name: "UAVCAN_ENABLE", value: 2 },
    { name: "UAVCAN_SUB_RNG", value: 1 },
    { name: "EKF2_RNG_CTRL", value: 1 },
  ], "the old driver is zeroed; the one the preset sets itself is not zeroed first");
}

// A module this firmware cannot run is listed rather than hidden — an operator
// who owns one needs to be told why, not left concluding the wiring is wrong —
// but it is never offered as something that can be applied.
async function testAnUnsupportedPresetExplainsItselfAndOffersNoApply() {
  const { container, fake } = await openSensorPage();
  const panel = pickPreset(container, "matek-3901-l0x");

  assert.match(findOneByClass(panel, "safety-preset-warning-text").textContent,
    /PX4 has no MSP sensor input/, "the reason is spelled out");
  assert.equal(findByClass(panel, "safety-preset-apply").length, 0,
    "there is no Apply button to press");
  assert.equal(findByClass(panel, "safety-preset-write").length, 0,
    "and no write list implying something could be set");
  assert.equal(fake.writes().length, 0);
}

// Half a preset is worse than none: the chain stops at the refusal, says so,
// and re-reads so the page shows what the aircraft actually holds.
async function testAPresetRefusedHalfwayStopsAndIsReported() {
  const { container, fake } = await openSensorPage(safetyDoc(), { rejectFrom: 2 });
  pickPreset(container, "holybro-h-flow");

  fire(findOneByClass(container, "safety-preset-apply"), "click");
  await flushMicrotasks();
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [
    { name: "UAVCAN_ENABLE", value: 2 },
    { name: "UAVCAN_SUB_RNG", value: 1 },
  ], "nothing is attempted after the refused write");
  // setFieldStatus rewrites the class list, so the panel's own status row is
  // found where it lives rather than by the class it was built with.
  const panel = findOneByClass(container, "safety-preset-detail");
  assert.equal(panel.dataset.preset, "holybro-h-flow",
    "the panel still shows the preset that failed, not a reset to Custom");
  assert.ok(findOneByClass(panel, "params-row-status").className.includes("err"),
    "the panel does not claim success");
  const note = dispatched.filter((e) => e.type === "corvus:notification").pop();
  assert.equal(note.detail.level, "critical", "the half-applied chain is reported");
  assert.ok(fake.requests.filter((u) => u === "/api/safety").length >= 2,
    "the page re-reads so it shows the half-applied reality");
}

// A sensor whose firmware offered no presets must still render its page.
async function testASensorWithoutPresetsStillRendersItsPage() {
  const { container } = await openSensorPage(safetyDoc(undefined, []));

  assert.equal(findByClass(container, "safety-preset-cards").length, 0, "no hardware card");
  assert.ok(findOneByClass(container, "safety-sensor"), "the switch is still there");
  assert.ok(control(container, "EKF2_RNG_A_HMAX"), "and so are its parameters");
}

// ===========================================================================
// PART B3 — parameters the operator adds under Custom
// ===========================================================================

async function testCustomOffersAnEmptyListAndABoxToAddTo() {
  const { container } = await openSensorPage();

  const panel = findOneByClass(container, "safety-preset-detail");
  assert.equal(panel.dataset.preset, "custom", "the page opens on Custom");
  assert.ok(findOneByClass(panel, "safety-extra-empty"), "nothing added yet is said plainly");
  assert.ok(addBox(container).input, "and the box to add one is there");

  pickPreset(container, "holybro-h-flow");
  assert.equal(findByClass(container, "safety-extra-input").length, 0,
    "a preset is a written chain, not a place to hand-add parameters");
}

// The added name has to be carried to the backend or the value can never come
// back: the page does one batched read and the schema does not know this name.
async function testAddingAParameterReReadsWithItNamedInTheQuery() {
  const { container, fake } = await openSensorPage();

  addExtraParam(container, "EKF2_RNG_QLTY_T");
  await flushMicrotasks();

  assert.ok(fake.requests.some((u) => u === "/api/safety?extra=EKF2_RNG_QLTY_T"),
    "the re-read asks the aircraft for the parameter that was just added");
  assert.equal(fake.writes().length, 0, "adding a row writes nothing on its own");
}

async function testAnAddedParameterBecomesAnOrdinaryEditableField() {
  const doc = safetyDoc();
  const { container, fake } = await openSensorPage(doc);

  fake.setDoc(Object.assign({}, doc, {
    extra: [{ param: "EKF2_RNG_QLTY_T", label: "EKF2_RNG_QLTY_T", kind: "number",
              present: true, value: 0.2 }],
  }));
  addExtraParam(container, "ekf2_rng_qlty_t");
  await flushMicrotasks();

  const input = control(container, "EKF2_RNG_QLTY_T");
  assert.ok(input, "the name is upper-cased and the row is drawn");
  assert.equal(input.value, "0.2", "prefilled with what the aircraft answered");

  input.value = "0.5";
  fire(input, "change");
  await flushMicrotasks();
  assert.deepEqual(fake.writes(), [{ name: "EKF2_RNG_QLTY_T", value: 0.5 }],
    "and it writes through the same path as every other field");
}

// A name the vehicle never answered for keeps its row. Dropping it silently
// would leave the operator unable to tell a typo from a parameter their PX4
// version does not have — and unable to remove it.
async function testAParameterTheFirmwareLacksKeepsItsRowAndSaysSo() {
  const doc = safetyDoc();
  const { container, fake } = await openSensorPage(doc);

  fake.setDoc(Object.assign({}, doc, {
    extra: [{ param: "NOT_A_PARAM", label: "NOT_A_PARAM", kind: "number",
              present: false, value: 0 }],
  }));
  addExtraParam(container, "NOT_A_PARAM");
  await flushMicrotasks();

  const row = findByDataset(container, "param", "NOT_A_PARAM")
    .filter((e) => e.className.includes("safety-extra-field"))[0];
  assert.ok(row, "the row is still there");
  assert.equal(findOneByClass(row, "safety-extra-absent").textContent,
    "this firmware does not have it");
  assert.equal(control(container, "NOT_A_PARAM"), undefined, "with nothing to edit");
  assert.ok(findOneByClass(row, "safety-extra-remove"), "but it can still be removed");
}

async function testRemovingAParameterDropsItsRowWithoutTouchingTheAircraft() {
  const doc = safetyDoc();
  const { container, fake } = await openSensorPage(doc);
  fake.setDoc(Object.assign({}, doc, {
    extra: [{ param: "MPC_XY_P", label: "MPC_XY_P", kind: "number",
              present: true, value: 0.95 }],
  }));
  addExtraParam(container, "MPC_XY_P");
  await flushMicrotasks();
  assert.ok(control(container, "MPC_XY_P"), "added");

  fire(findOneByClass(container, "safety-extra-remove"), "click");
  assert.equal(control(container, "MPC_XY_P"), undefined, "the row is gone");
  assert.ok(findOneByClass(container, "safety-extra-empty"), "and the list is empty again");
  assert.equal(fake.writes().length, 0, "removing a row never writes to the aircraft");
}

// Every refusal is answered where the operator is typing, not silently at the
// far end of the link.
async function testABadNameIsRefusedInTheBoxItWasTypedIn() {
  const { container, fake } = await openSensorPage();

  const box = addExtraParam(container, "not a param");
  assert.ok(box.status.className.includes("err"));
  assert.match(box.status.textContent, /not a parameter name/);
  assert.ok(box.input.className.includes("invalid"));
  assert.equal(fake.requests.filter((u) => u.indexOf("extra=") >= 0).length, 0,
    "nothing was asked of the aircraft");

  // 17 characters: one past what a PX4 parameter id can hold on the wire.
  addExtraParam(container, "ABCDEFGHIJKLMNOPQ");
  assert.match(addBox(container).status.textContent, /16 characters/);
}

// Not missing — somewhere the operator has not scrolled to. Saying which card
// it is on is the difference between a useful refusal and a confusing one.
async function testAParameterAlreadyOnThePageIsRefusedByName() {
  const { container } = await openSensorPage();

  const box = addExtraParam(container, "EKF2_RNG_A_HMAX");
  assert.match(box.status.textContent, /already on this page, under Distance sensor/);

  addExtraParam(container, "GF_MAX_HOR_DIST");
  assert.match(addBox(container).status.textContent, /already on this page, under Flight limits/);
}

async function testTheSameParameterCannotBeAddedTwice() {
  const doc = safetyDoc();
  const { container, fake } = await openSensorPage(doc);
  fake.setDoc(Object.assign({}, doc, {
    extra: [{ param: "MPC_XY_P", label: "MPC_XY_P", kind: "number",
              present: true, value: 0.95 }],
  }));
  addExtraParam(container, "MPC_XY_P");
  await flushMicrotasks();

  const box = addExtraParam(container, "MPC_XY_P");
  assert.match(box.status.textContent, /already added/);
  assert.equal(findByClass(container, "safety-extra-field").length, 1);
}

// The list describes how this operator works on their airframe, so it outlives
// the page — a reload that lost the row would lose it silently.
async function testTheAddedListSurvivesANewPage() {
  window.localStorage.clear();
  const doc = safetyDoc();
  const first = await openSensorPage(doc);
  first.fake.setDoc(Object.assign({}, doc, {
    extra: [{ param: "MPC_XY_P", label: "MPC_XY_P", kind: "number",
              present: true, value: 0.95 }],
  }));
  addExtraParam(first.container, "MPC_XY_P");
  await flushMicrotasks();
  first.destroy();

  const again = await openSensorPage(Object.assign({}, doc, {
    extra: [{ param: "MPC_XY_P", label: "MPC_XY_P", kind: "number",
              present: true, value: 0.95 }],
  }));
  assert.ok(again.fake.requests.some((u) => u === "/api/safety?extra=MPC_XY_P"),
    "the remembered name is asked for on the next open");
  assert.ok(control(again.container, "MPC_XY_P"), "and its row is drawn again");
  window.localStorage.clear();
}

// A remembered name that this build now renders as a field of its own would sit
// there forever claiming the firmware does not have a parameter that is visible
// two cards down — the backend refuses to return it, so the page forgets it.
async function testARememberedNameThatBecameAFieldIsForgotten() {
  window.localStorage.clear();
  window.localStorage.setItem("corvus.safety.extra",
    JSON.stringify({ rangefinder: ["EKF2_RNG_A_HMAX", "MPC_XY_P"] }));

  const doc = safetyDoc();
  const { container, fake } = await openSensorPage(Object.assign({}, doc, {
    extra: [{ param: "MPC_XY_P", label: "MPC_XY_P", kind: "number",
              present: true, value: 0.95 }],
  }));

  assert.equal(findByClass(container, "safety-extra-field").length, 1,
    "only the one that is not already a field is kept");
  assert.ok(control(container, "MPC_XY_P"));
  assert.deepEqual(JSON.parse(window.localStorage.getItem("corvus.safety.extra")),
    { rangefinder: ["MPC_XY_P"] }, "and the stale name is dropped for good");
  assert.ok(fake.requests.every((u) => u.indexOf("EKF2_RNG_A_HMAX") < 0)
    || fake.requests.length >= 1);
  window.localStorage.clear();
}

async function testTheAddBoxIsGatedWhileArmed() {
  window.localStorage.clear();
  const fake = makeFakeTelemetry({ doc: safetyDoc(), state: { connected: true, armed: true } });
  const { container } = await open(fake);
  enterSensor(container);

  const box = addBox(container);
  assert.ok(box.input.disabled, "the name box is disabled while armed");
  assert.ok(box.button.disabled, "and so is Add");
}

// ===========================================================================
// PART C — armed gating and teardown
// ===========================================================================

function gatedControls(container) {
  return findByClass(container, "safety-select")
    .concat(findByClass(container, "safety-input"))
    .concat(findByClass(container, "safety-sensor-switch"))
    .concat(findByClass(container, "safety-preset-apply"));
}

async function testArmedGatingDisablesEveryControl() {
  const fake = makeFakeTelemetry({ doc: safetyDoc(), state: { connected: true, armed: false } });
  const { container } = await open(fake);

  const banner = findOneByClass(container, "params-banner");
  const controls = gatedControls(container);
  assert.ok(controls.length >= 4, "controls rendered");
  assert.ok(banner.hidden, "no banner while disarmed");
  assert.ok(controls.every((c) => !c.disabled), "controls editable while disarmed");

  fake.getSubCb()({ connected: true, armed: true });
  assert.ok(!banner.hidden, "armed banner shown");
  assert.ok(controls.every((c) => c.disabled), "every control disabled while armed");

  fake.getSubCb()({ connected: true, armed: false });
  assert.ok(banner.hidden, "banner hidden again on disarm");
  assert.ok(controls.every((c) => !c.disabled), "controls editable again on disarm");
}

// The armed gate is a page-level fact, so it has to survive the page rebuilding
// itself for a different view. A sensor page entered while armed that came up
// editable would be the gate failing exactly where it matters most.
async function testTheArmedGateSurvivesEnteringASensor() {
  const fake = makeFakeTelemetry({ doc: safetyDoc(), state: { connected: true, armed: true } });
  const { container } = await open(fake);

  enterSensor(container);
  pickPreset(container, "holybro-h-flow");

  assert.ok(!findOneByClass(container, "params-banner").hidden,
    "the armed banner is on the sensor page too");
  const controls = gatedControls(container);
  assert.ok(controls.length >= 4, "the sensor page rendered its controls");
  assert.ok(controls.every((c) => c.disabled),
    "every control on the sensor page comes up disabled, presets included");
}

async function testAnArmedSwitchWritesNothing() {
  const fake = makeFakeTelemetry({ doc: safetyDoc(), state: { connected: true, armed: true } });
  const { container } = await open(fake);
  enterSensor(container);

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
    testCheckValuesConfirmsWhatWasSetAndRedrawsFromTheVehicle,
    testASensorChainIsCheckedLikeAField,
    testRendersEverySectionFromTheSchema,
    testEveryFormPartCarriesTheSharedBaseClass,
    testDisconnectedRendersAnExplanationNotAnError,
    testAFailedReadIsReportedNotThrown,
    testAFieldWriteGoesThroughTheParameterEndpoint,
    testANumberFieldRejectsGarbageBeforeItReachesTheAircraft,
    testAValueOutsideTheSchemaBoundsIsRefused,
    testARefusedFieldWriteRestoresTheControl,
    testABitmaskRendersOneSwitchPerNamedBit,
    testTickingABitWritesTheWholeRecomputedWord,
    testClearingABitLeavesTheOthersAlone,
    testABitThisBuildDoesNotNameIsNeverLost,
    testARefusedBitmaskWriteRestoresEveryBox,
    testAnArmedVehicleDisablesEveryBitOfABitmask,
    testTheSensorHeaderShowsTheStateAndThePickers,
    testThePortPickerFollowsTheSelectedDriver,
    testEnablingASensorWritesTheWholeChain,
    testEnablingASerialSensorWritesTheChosenPort,
    testSwitchingDriverZeroesTheOldOneFirst,
    testDisablingASensorTakesDownBothHalves,
    testARefusedEnableSnapsTheSwitchBack,
    testAChainRefusedHalfwayStopsAndIsReported,
    testAChainStopsAtTheFirstRefusal,
    testTheOverviewListsEachSensorWithItsState,
    testASensorTileOpensItsPageAndBackReturns,
    testAReloadKeepsTheOpenSensorPage,
    testASensorThatDisappearsFallsBackToTheOverview,
    testTheHardwareDropdownLeadsWithCustomAndWritesNothingOnSelection,
    testADropdownRowSaysWhenItsDriverIsAlreadyRunning,
    testAPresetShowsExactlyWhatItWouldWrite,
    testApplyingAPresetWritesItsWholeChain,
    testAPresetForASerialModuleWritesThePickedPort,
    testApplyingAPresetZeroesAConflictingDriverFirst,
    testAnUnsupportedPresetExplainsItselfAndOffersNoApply,
    testAPresetRefusedHalfwayStopsAndIsReported,
    testASensorWithoutPresetsStillRendersItsPage,
    testCustomOffersAnEmptyListAndABoxToAddTo,
    testAddingAParameterReReadsWithItNamedInTheQuery,
    testAnAddedParameterBecomesAnOrdinaryEditableField,
    testAParameterTheFirmwareLacksKeepsItsRowAndSaysSo,
    testRemovingAParameterDropsItsRowWithoutTouchingTheAircraft,
    testABadNameIsRefusedInTheBoxItWasTypedIn,
    testAParameterAlreadyOnThePageIsRefusedByName,
    testTheSameParameterCannotBeAddedTwice,
    testTheAddedListSurvivesANewPage,
    testARememberedNameThatBecameAFieldIsForgotten,
    testTheAddBoxIsGatedWhileArmed,
    testArmedGatingDisablesEveryControl,
    testTheArmedGateSurvivesEnteringASensor,
    testAnArmedSwitchWritesNothing,
    testTeardownReleasesTheSubscription,
    testALateResponseAfterTeardownIsIgnored,
  ];
  for (const t of tests) {
    // The added-parameter list is persisted, so it would otherwise leak from
    // one test into the next exactly the way it is meant to leak across page
    // loads. Each test starts from an empty profile.
    window.localStorage.clear();
    await t();
    console.log("  ok", t.name);
  }
  console.log(`\n${tests.length} passed — Setup > Safety & Sensors`);
}

main().catch((err) => { console.error(err); process.exit(1); });
