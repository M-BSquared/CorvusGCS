"use strict";

/**
 * Frontend tests for the Setup -> Battery & Power page (Corvus.setupBattery).
 *
 * Plain Node-runnable assertions (no browser, no test runner), same pattern as
 * tests/test_frontend_safety.js: stub the globals the module touches, require
 * the source, and assert on the rendered DOM, the write spies, and the
 * teardown behaviour.
 *
 * What is pinned here is mostly the page's one structural promise — the pack
 * on the left, everything changeable on the right — and the three places where
 * getting it wrong would be a lie about the aircraft rather than a cosmetic
 * bug: a threshold marker placed by guesswork, an estimator setting that stops
 * being editable at the moment the number matters, and the internal-resistance
 * conversion, where the wrong direction is a 6000x error in the sag correction.
 *
 * Run:
 *   node tests/test_frontend_battery.js
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

/** The editable control (not the row) that writes a vehicle `param`. */
function control(container, param) {
  return findByDataset(container, "param", param)
    .filter((e) => e.tagName === "SELECT" || e.tagName === "INPUT")[0];
}

/** The input of one estimator setting row. */
function setting(container, key) {
  const row = findByDataset(container, "setting", key)[0];
  return row && (row.querySelector("INPUT") || row.querySelector("SELECT"));
}

/** The value span of one readout row. */
function readout(container, key) {
  const row = findByDataset(container, "readout", key)[0];
  return row && findOneByClass(row, "battery-readout-value");
}

// ---------------------------------------------------------------------------
// Fake telemetry. `requestJson` answers /api/battery with the page document and
// /api/config with the merged settings, recording every body so the save path
// can be asserted on. `subscribe` captures the callback rather than firing it,
// so each test drives the live column itself.
// ---------------------------------------------------------------------------
function makeFakeTelemetry(opts = {}) {
  const postCalls = [];
  const requests = [];
  const saves = [];
  let subCb = null;
  let unsubCalls = 0;
  let state = opts.state || { armed: false, connected: true };
  let doc = opts.doc;
  let configured = Object.assign({}, (opts.doc && opts.doc.configured) || {});

  const telemetry = {
    postAction(url, payload) {
      postCalls.push({ url, payload });
      if (url === "/api/params/verify") {
        if (opts.verifyError) return Promise.reject(new Error(opts.verifyError));
        const answer = typeof opts.verify === "function" ? opts.verify(payload) : opts.verify;
        return Promise.resolve(Object.assign({ ok: true }, answer || {}));
      }
      if (opts.writeError) return Promise.reject(new Error(opts.writeError));
      return Promise.resolve({ ok: true });
    },
    requestJson(url, options) {
      requests.push({ url, options });
      if (url === "/api/config") {
        const body = JSON.parse(options.body);
        saves.push(body.battery);
        if (opts.saveError) return Promise.reject(new Error(opts.saveError));
        // The real endpoint merges per key and answers with the whole object.
        configured = Object.assign({}, configured, body.battery);
        return Promise.resolve({ ok: true, config: { battery: configured } });
      }
      if (opts.requestError) return Promise.reject(new Error(opts.requestError));
      return Promise.resolve(doc);
    },
    getState() { return state; },
    subscribe(cb) { subCb = cb; return () => { unsubCalls++; }; },
  };
  return {
    telemetry, postCalls, requests, saves,
    get unsubCalls() { return unsubCalls; },
    getSubCb() { return subCb; },
    setDoc(next) { doc = next; },
    setState(s) { state = s; },
    writes() { return postCalls.filter((c) => c.url === "/api/params/set").map((c) => c.payload); },
    checks() { return postCalls.filter((c) => c.url === "/api/params/verify").map((c) => c.payload); },
  };
}

require("../src/js/ui.js");
require("../src/js/setup-shared.js");
require("../src/js/setup-battery.js");

// ---------------------------------------------------------------------------
// A representative /api/battery payload: the PX4 shape, with the thresholds
// stated as fractions of a pack.
// ---------------------------------------------------------------------------
function batteryDoc(overrides) {
  return Object.assign({
    connected: true,
    received: 12,
    sections: [
      {
        id: "pack", title: "Pack", kind: "fields",
        hint: "What is plugged in.",
        fields: [
          { param: "BAT1_N_CELLS", label: "Cells in series", kind: "number",
            value: 6, unit: "S", step: 1, min: 0, max: 24 },
          { param: "BAT1_V_EMPTY", label: "Empty cell voltage", kind: "number",
            value: 3.6, unit: "V", step: 0.01, min: 0, max: 5 },
        ],
      },
      {
        id: "sensing", title: "Measurement", kind: "fields",
        fields: [
          { param: "BAT1_SOURCE", label: "Measured by", kind: "enum", value: 0,
            options: [{ value: 0, label: "Power module" }, { value: 2, label: "ESCs" }] },
        ],
      },
    ],
    pack: {
      cells: 6, cells_param: "BAT1_N_CELLS",
      capacity_mah: 16000, capacity_param: "BAT1_CAPACITY",
      full_cell: 4.05, empty_cell: 3.6, load_drop: 0.3,
      resistance_ohm: 0.03, source: "Power module",
      thresholds: [
        { id: "low", label: "Low", param: "BAT_LOW_THR", percent: 15 },
        { id: "crit", label: "Critical", param: "BAT_CRIT_THR", percent: 7 },
      ],
    },
    settings: {
      estimate: false, chemistry: "lipo", cells: 0, full_cell: 4.2,
      empty_cell: 3.3, resistance: 0, capacity_mah: 0,
    },
    configured: {},
    chemistries: [
      { value: "lipo", label: "LiPo", full: 4.2, empty: 3.3, nominal: 3.7 },
      { value: "lifepo4", label: "LiFePO4", full: 3.65, empty: 2.8, nominal: 3.2 },
    ],
  }, overrides || {});
}

/** The ArduPilot shape: absolute volts instead of fractions, no cell count. */
function ardupilotDoc() {
  const doc = batteryDoc({ stack: "ardupilot" });
  doc.pack = Object.assign({}, doc.pack, {
    cells: 0, cells_param: "", full_cell: 0, empty_cell: 0, resistance_ohm: 0,
    thresholds: [
      { id: "low", label: "Low", param: "BATT_LOW_VOLT", volts: 21.0 },
      { id: "crt", label: "Critical", param: "BATT_CRT_VOLT", volts: 19.8 },
    ],
  });
  return doc;
}

function liveState(overrides) {
  return Object.assign({
    connected: true, armed: false,
    battery_percent: 62, battery_voltage: 23.4, battery_current: 18.2,
    battery_percent_fc: 87, battery_percent_est: 62,
    battery_source: "estimate", battery_cells: 6, battery_cell_voltage: 3.9,
    battery_cell_voltages: [], battery_consumed_mah: 4200,
    battery_temperature: 24.5, battery_time_remaining: 615,
  }, overrides || {});
}

/** Render the page with a document already loaded. */
async function mount(opts = {}) {
  const fake = makeFakeTelemetry(Object.assign({ doc: batteryDoc() }, opts));
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  const destroy = Corvus.setupBattery.render(container, () => {});
  await flushMicrotasks();
  return { fake, container, destroy };
}

// ===========================================================================
// Structure
// ===========================================================================

async function testTheLeftColumnIsThePackAndTheRightIsEverythingChangeable() {
  const { container } = await mount();

  const live = findOneByClass(container, "battery-live");
  const config = findOneByClass(container, "battery-config");
  assert.ok(live, "the pack column is rendered");
  assert.ok(config, "the settings column is rendered");
  assert.ok(findOneByClass(live, "battery-figure"), "the drawing is in the left column");
  assert.ok(findOneByClass(live, "battery-readout"), "the numbers are under the drawing");
  assert.ok(findOneByClass(config, "battery-estimator-card"),
    "the estimator is in the right column");
  assert.deepEqual(
    findByDataset(config, "section", "pack").length
      + findByDataset(config, "section", "sensing").length,
    2, "every section the backend sent has a card on the right");
  assert.equal(findByClass(live, "battery-estimator-card").length, 0,
    "nothing changeable is in the pack column");
}

async function testTheVehiclesOwnSectionsAreRenderedFromTheDescription() {
  const { container } = await mount();
  assert.ok(control(container, "BAT1_N_CELLS"), "a number field from the schema");
  const source = control(container, "BAT1_SOURCE");
  assert.equal(source.tagName, "SELECT", "an enum field renders as a picker");
}

async function testAPackWithSixCellsIsDrawnWithSixLabelledCells() {
  const { container } = await mount();
  const labels = findByClass(container, "battery-cell-label");
  assert.equal(labels.length, 6, "one voltage label per cell");
  assert.equal(findByClass(container, "battery-divider").length, 5,
    "five dividers between six cells");
}

async function testALargePackDropsTheLabelsRatherThanSmearingThem() {
  const doc = batteryDoc();
  doc.pack.cells = 12;
  const { container, fake } = await mount({ doc });
  fake.getSubCb()(liveState({ battery_cells: 12 }));

  assert.equal(findByClass(container, "battery-divider").length, 11,
    "the cells are still drawn");
  assert.equal(findByClass(container, "battery-cell-label").length, 0,
    "twelve legible labels do not fit, so none are claimed");
}

async function testAPackWhoseCellCountIsUnknownIsDrawnWithoutDividers() {
  const { container, fake } = await mount({ doc: ardupilotDoc() });
  fake.getSubCb()(liveState({ battery_cells: 0 }));

  assert.equal(findByClass(container, "battery-divider").length, 0,
    "a guessed number of cells would be a claim the pack voltage cannot support");
}

// ===========================================================================
// The failsafe levels on the drawing
// ===========================================================================

async function testTheFailsafeLevelsAreMarkedOnThePack() {
  const { container } = await mount();
  const marks = findByClass(container, "battery-threshold");
  assert.equal(marks.length, 2, "low and critical are both drawn");
  assert.ok(marks.some((m) => m.className.includes("low")),
    "the low mark is styled as the warning it is");
  const labels = findByClass(container, "battery-threshold-label")
    .map((l) => l.textContent);
  assert.deepEqual(labels, ["Low", "Critical"]);
}

async function testAnArdupilotLevelInVoltsIsPlacedOnlyOnceTheCellsAreKnown() {
  /* 21.0 V is a position on this scale only when the cell count is known. A
     marker placed by guesswork on the one drawing an operator uses to decide
     whether to land would be worse than no marker at all. */
  const { container, fake } = await mount({ doc: ardupilotDoc() });
  fake.getSubCb()(liveState({ battery_cells: 0 }));
  assert.equal(findByClass(container, "battery-threshold").length, 0,
    "no cell count, so no marker");

  fake.getSubCb()(liveState({ battery_cells: 6 }));
  const marks = findByClass(container, "battery-threshold");
  assert.equal(marks.length, 2, "with six cells the volts become a position");
}

// ===========================================================================
// The live numbers
// ===========================================================================

async function testTheReadoutCarriesWhatTheAircraftIsActuallyDoing() {
  const { container, fake } = await mount();
  fake.getSubCb()(liveState());

  assert.equal(readout(container, "remaining").textContent, "62%");
  assert.equal(readout(container, "voltage").textContent, "23.4 V");
  assert.equal(readout(container, "current").textContent, "18.2 A");
  assert.equal(readout(container, "power").textContent, "426 W");
  assert.equal(readout(container, "consumed").textContent, "4200 mAh");
  assert.equal(readout(container, "capacity").textContent, "16000 mAh");
  assert.equal(readout(container, "temperature").textContent, "24.5 °C");
  assert.equal(readout(container, "endurance").textContent, "10 min 15 s");
}

async function testTheOtherAnswerIsAlwaysOnScreenAndAlwaysNamed() {
  /* The whole argument for computing both is that they can be seen disagreeing
     without switching a setting back and forth. */
  const { container, fake } = await mount();
  const row = findByDataset(container, "readout", "fc")[0];
  const label = findOneByClass(row, "battery-readout-label");

  fake.getSubCb()(liveState({ battery_source: "estimate" }));
  assert.equal(label.textContent, "Autopilot says");
  assert.equal(readout(container, "fc").textContent, "87%");

  fake.getSubCb()(liveState({ battery_source: "autopilot", battery_percent: 87 }));
  assert.equal(label.textContent, "Cell voltage says");
  assert.equal(readout(container, "fc").textContent, "62%");
}

async function testAnAutopilotThatReportsNothingSaysSoRatherThanZero() {
  const { container, fake } = await mount();
  fake.getSubCb()(liveState({ battery_source: "estimate", battery_percent_fc: -1 }));
  assert.equal(readout(container, "fc").textContent, "no estimate");
}

async function testAPackThatReportsItsOwnCellsIsBelievedOverTheAverage() {
  const { container, fake } = await mount();
  fake.getSubCb()(liveState({
    battery_cell_voltages: [3.91, 3.88, 3.90, 3.92, 3.89, 3.72],
  }));

  const labels = findByClass(container, "battery-cell-label").map((l) => l.textContent);
  assert.equal(labels[0], "3.91 V");
  assert.equal(labels[5], "3.72 V", "the cell that is dragging the pack down");
  assert.match(readout(container, "cell").textContent, /200 mV apart/,
    "the spread is what a per-cell report is actually for");
}

async function testADisconnectedVehicleShowsNoNumbersRatherThanZeroes() {
  const { container, fake } = await mount();
  fake.getSubCb()(liveState({ connected: false }));

  assert.equal(readout(container, "voltage").textContent, "—");
  assert.equal(readout(container, "remaining").textContent, "—");
  const fill = findOneByClass(container, "battery-fill");
  assert.ok(fill.className.includes("off"), "an empty-looking pack would be a lie");
}

// ===========================================================================
// The estimator settings
// ===========================================================================

async function testTurningTheEstimateOnSavesItThroughTheConfigEndpoint() {
  const { container, fake } = await mount();
  const toggle = findOneByClass(container, "battery-estimate-toggle");
  fire(toggle, "click");
  await flushMicrotasks();

  assert.deepEqual(fake.saves, [{ estimate: true }],
    "one key, so the merge leaves the rest of the settings alone");
}

async function testASettingLeftBlankMeansWorkItOut() {
  const doc = batteryDoc({ configured: { cells: 6 } });
  const { container, fake } = await mount({ doc });
  const input = setting(container, "cells");
  assert.equal(input.value, "6", "a pinned count is shown as pinned");

  input.value = "";
  fire(input, "change");
  await flushMicrotasks();
  assert.deepEqual(fake.saves, [{ cells: 0 }], "0 is how 'work it out' is stored");
}

async function testAnAutoSettingShowsWhatItResolvedToWithoutClaimingItWasTyped() {
  /* A form pre-filled with 4.2 cannot say whether the operator pinned it or the
     chemistry did, and the difference decides what happens when they switch to
     LiFePO4 later. */
  const { container, fake } = await mount();
  fake.getSubCb()(liveState({ battery_cells: 6 }));

  assert.equal(setting(container, "full_cell").value, "");
  assert.equal(setting(container, "full_cell")._attrs.placeholder || "", "");
  assert.match(setting(container, "full_cell").placeholder, /4\.2 V/);
}

async function testASettingsExplanationIsBehindAHintIconNotUnderTheRow() {
  /* A paragraph under each of five rows made the estimator read as prose and
     pushed the rows a screen apart. */
  const { container } = await mount();
  const row = findByDataset(container, "setting", "cells")[0];
  assert.equal(findByClass(row, "pform-field-hint").length, 0, "nothing printed under the row");
  const hint = findOneByClass(row, "ui-info");
  assert.ok(hint && hint.corvusPopover, "a hint icon beside the label");
  const text = hint.corvusPopover.el.children
    .filter((c) => c.className.split(/\s+/).includes("ui-popover-text"))
    .map((c) => c.textContent).join("");
  assert.match(text, /19\.8 V/);
}

async function testAValueOutsideItsBoundsIsRefusedInTheRowItWasTypedIn() {
  const { container, fake } = await mount();
  const input = setting(container, "cells");
  input.value = "40";
  fire(input, "change");
  await flushMicrotasks();

  assert.deepEqual(fake.saves, [], "nothing was saved");
  assert.ok(input.className.includes("invalid"));
  const status = findByDataset(container, "setting", "cells")[0]
    .querySelector(".params-row-status");
  assert.match(status.textContent, /above 24/);
}

async function testChangingTheChemistryIsSavedByName() {
  const { container, fake } = await mount();
  const select = setting(container, "chemistry");
  select.value = "lifepo4";
  fire(select, "change");
  await flushMicrotasks();

  assert.deepEqual(fake.saves, [{ chemistry: "lifepo4" }]);
}

async function testTakingThePackFromTheVehicleConvertsTheResistancePerCell() {
  /* BAT1_R_INTERNAL is ohms for the whole pack; the field is milliohms per
     cell. The wrong direction here is a 6000x error in the sag correction. */
  const { container, fake } = await mount();
  fire(findOneByClass(container, "battery-copy"), "click");
  await flushMicrotasks();

  assert.deepEqual(fake.saves, [{
    cells: 6, full_cell: 4.05, empty_cell: 3.6, resistance: 5,
  }], "0.03 ohm over six cells is 5 mOhm a cell");
}

async function testAVehicleWithNoPackToCopyOffersNoButtonToPressIt() {
  const { container } = await mount({ doc: ardupilotDoc() });
  assert.equal(findOneByClass(container, "battery-copy").disabled, true,
    "ArduPilot has none of these parameters, so there is nothing to copy");
}

async function testARefusedSaveIsReportedAndNotShownAsApplied() {
  const { container, fake } = await mount({ saveError: "config is read-only" });
  const input = setting(container, "cells");
  input.value = "6";
  fire(input, "change");
  await flushMicrotasks();

  const status = findByDataset(container, "setting", "cells")[0]
    .querySelector(".params-row-status");
  assert.match(status.textContent, /read-only/);
  assert.ok(dispatched.some((e) => e.detail && /Could not save/.test(e.detail.message)),
    "and the operator is told, not only the row");
  assert.deepEqual(fake.writes(), [], "nothing reached the aircraft");
}

// ===========================================================================
// Armed
// ===========================================================================

async function testArmedGatingStopsTheVehiclesOwnParametersOnly() {
  /* The estimator is Corvus's own display setting. Refusing it while armed
     would be the ground station refusing to let somebody fix its own number in
     the one situation where the number matters most. */
  const { container, fake } = await mount();
  fake.getSubCb()(liveState({ armed: true }));

  assert.equal(control(container, "BAT1_N_CELLS").disabled, true,
    "the vehicle parameter is refused while armed");
  assert.equal(setting(container, "cells").disabled, false,
    "the estimator setting is not");
  assert.equal(findOneByClass(container, "battery-estimate-toggle").disabled, false);
  assert.equal(findOneByClass(container, "params-banner").hidden, false,
    "and the banner says which half is gated");
}

async function testAVehicleFieldWritesThroughTheSharedParameterEndpoint() {
  const { container, fake } = await mount();
  const input = control(container, "BAT1_N_CELLS");
  input.value = "12";
  fire(input, "change");
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [{ name: "BAT1_N_CELLS", value: 12 }]);
}


// ===========================================================================
// Check values
// ===========================================================================

/** A verify answer in the backend's shape, for the names in `rows`. */
function verifyAnswer(rows, values) {
  const failed = rows.filter((r) => !r.ok).length;
  return {
    results: rows, values: values || {}, missing: [],
    confirmed: rows.length - failed, failed, all_confirmed: failed === 0,
  };
}

function checkButton(container) { return findOneByClass(container, "battery-check"); }

async function clickCheck(container) {
  fire(checkButton(container), "click");
  await flushMicrotasks();
  await flushMicrotasks();
}

async function testTheCheckSendsWhatWasSetAndReadsBackTheWholePage() {
  const { container, fake } = await mount({ verify: verifyAnswer([]) });
  const input = control(container, "BAT1_N_CELLS");
  input.value = "12";
  fire(input, "change");
  await flushMicrotasks();

  await clickCheck(container);

  assert.deepEqual(fake.checks(), [{
    params: [{ name: "BAT1_N_CELLS", value: 12 }],
    names: ["BAT1_N_CELLS", "BAT1_V_EMPTY", "BAT1_SOURCE"],
  }]);
}

async function testARefusedWriteIsStillCheckedSoItCanBeWrittenAgain() {
  /* A refused field snaps back to what the vehicle holds, so the control no
     longer shows what the operator wanted. The check must still know. */
  const { container, fake } = await mount({ writeError: "not confirmed", verify: verifyAnswer([]) });
  const input = control(container, "BAT1_N_CELLS");
  input.value = "12";
  fire(input, "change");
  await flushMicrotasks();
  assert.equal(control(container, "BAT1_N_CELLS").value, "6", "the refused field snapped back");

  await clickCheck(container);

  assert.deepEqual(fake.checks()[0].params, [{ name: "BAT1_N_CELLS", value: 12 }]);
}

async function testAfterTheCheckTheFieldsShowWhatTheVehicleHolds() {
  const doc = batteryDoc();
  const fake = makeFakeTelemetry({
    doc,
    verify: verifyAnswer([{ name: "BAT1_N_CELLS", wanted: 12, before: 6, after: 12,
      rewritten: true, ok: true, error: "" }], { BAT1_N_CELLS: 12 }),
  });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  Corvus.setupBattery.render(container, () => {});
  await flushMicrotasks();
  const input = control(container, "BAT1_N_CELLS");
  input.value = "12";
  fire(input, "change");
  await flushMicrotasks();

  const after = batteryDoc();
  after.sections[0].fields[0].value = 12;
  fake.setDoc(after);
  await clickCheck(container);

  assert.equal(control(container, "BAT1_N_CELLS").value, "12",
    "the page is redrawn from a fresh read, not from what was typed");
  assert.ok(fake.requests.filter((r) => r.url === "/api/battery?fresh=1").length >= 2,
    "the redraw asks the vehicle, not the cache");
  const card = findOneByClass(container, "battery-check-card");
  assert.ok(card, "the outcome is shown");
  assert.match(findOneByClass(card, "battery-check-outcome").textContent, /written again, confirmed/);
  assert.match(findOneByClass(container, "params-actions-status").textContent,
    /BAT1_N_CELLS is on the vehicle\. It had to be written again/);
}

async function testAValueThatDidNotStickIsNamedInItsRowAndKeptForTheNextCheck() {
  let calls = 0;
  const { container, fake } = await mount({
    verify: () => {
      calls += 1;
      return verifyAnswer([{ name: "BAT1_N_CELLS", wanted: 12, before: 6, after: 6,
        rewritten: true, ok: false, error: "the vehicle kept a different value" }],
        { BAT1_N_CELLS: 6 });
    },
  });
  const input = control(container, "BAT1_N_CELLS");
  input.value = "12";
  fire(input, "change");
  await flushMicrotasks();

  await clickCheck(container);

  const row = findByDataset(container, "param", "BAT1_N_CELLS")
    .filter((e) => /pform-field/.test(e.className))[0];
  assert.match(row.querySelector(".params-row-status").textContent, /wanted 12, vehicle holds 6/);
  const status = findOneByClass(container, "params-actions-status");
  assert.match(status.textContent, /BAT1_N_CELLS is not on the vehicle/);
  assert.match(status.className, /err/);

  await clickCheck(container);
  assert.equal(calls, 2);
  assert.deepEqual(fake.checks()[1].params, [{ name: "BAT1_N_CELLS", value: 12 }],
    "an unconfirmed change is checked again next time");
}

async function testAConfirmedChangeIsNotCheckedAgain() {
  const { container, fake } = await mount({
    verify: (payload) => verifyAnswer(payload.params.map((p) => ({
      name: p.name, wanted: p.value, before: p.value, after: p.value,
      rewritten: false, ok: true, error: "" }))),
  });
  const input = control(container, "BAT1_N_CELLS");
  input.value = "12";
  fire(input, "change");
  await flushMicrotasks();

  await clickCheck(container);
  await clickCheck(container);

  assert.deepEqual(fake.checks()[1].params, [], "once confirmed, the change is done");
  assert.match(findOneByClass(container, "params-actions-status").textContent,
    /No changes to confirm/);
}

async function testACheckThatCannotRunSaysSo() {
  const { container } = await mount({ verifyError: "not connected" });
  await clickCheck(container);
  const status = findOneByClass(container, "params-actions-status");
  assert.match(status.textContent, /not connected/);
  assert.match(status.className, /err/);
}

async function testThereIsNothingToCheckWithoutAVehicle() {
  const doc = batteryDoc({ connected: false, sections: [], error: "no heartbeat" });
  const { container } = await mount({ doc });
  assert.equal(checkButton(container).disabled, true);
}

// ===========================================================================
// Lifecycle
// ===========================================================================

async function testADisconnectedVehicleStillOffersTheEstimator() {
  const doc = batteryDoc({ connected: false, sections: [], error: "no heartbeat" });
  const { container } = await mount({ doc });

  assert.ok(findOneByClass(container, "battery-estimator-card"),
    "the settings are Corvus's own and are saved either way");
  assert.match(findOneByClass(container, "params-actions-status").textContent,
    /no heartbeat/);
}

async function testTeardownReleasesTheSubscription() {
  const { fake, destroy } = await mount();
  destroy();
  assert.equal(fake.unsubCalls, 1);
}

async function testALateResponseAfterTeardownIsIgnored() {
  const fake = makeFakeTelemetry({ doc: batteryDoc() });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  const destroy = Corvus.setupBattery.render(container, () => {});
  const card = findOneByClass(container, "battery-estimator-card");
  destroy();
  await flushMicrotasks();

  assert.equal(findOneByClass(container, "battery-estimator-card"), card,
    "a response that lands after the page is gone must not rebuild it");
  assert.match(findOneByClass(container, "params-actions-status").textContent,
    /Reading battery configuration/,
    "and must not write its result into a page nobody is looking at");
  assert.equal(findByDataset(container, "section", "pack").length, 0,
    "the document it carried was never rendered");
}

// ---------------------------------------------------------------------------

async function main() {
  const tests = [
    testTheLeftColumnIsThePackAndTheRightIsEverythingChangeable,
    testTheVehiclesOwnSectionsAreRenderedFromTheDescription,
    testAPackWithSixCellsIsDrawnWithSixLabelledCells,
    testALargePackDropsTheLabelsRatherThanSmearingThem,
    testAPackWhoseCellCountIsUnknownIsDrawnWithoutDividers,
    testTheFailsafeLevelsAreMarkedOnThePack,
    testAnArdupilotLevelInVoltsIsPlacedOnlyOnceTheCellsAreKnown,
    testTheReadoutCarriesWhatTheAircraftIsActuallyDoing,
    testTheOtherAnswerIsAlwaysOnScreenAndAlwaysNamed,
    testAnAutopilotThatReportsNothingSaysSoRatherThanZero,
    testAPackThatReportsItsOwnCellsIsBelievedOverTheAverage,
    testADisconnectedVehicleShowsNoNumbersRatherThanZeroes,
    testTurningTheEstimateOnSavesItThroughTheConfigEndpoint,
    testASettingLeftBlankMeansWorkItOut,
    testAnAutoSettingShowsWhatItResolvedToWithoutClaimingItWasTyped,
    testASettingsExplanationIsBehindAHintIconNotUnderTheRow,
    testAValueOutsideItsBoundsIsRefusedInTheRowItWasTypedIn,
    testChangingTheChemistryIsSavedByName,
    testTakingThePackFromTheVehicleConvertsTheResistancePerCell,
    testAVehicleWithNoPackToCopyOffersNoButtonToPressIt,
    testARefusedSaveIsReportedAndNotShownAsApplied,
    testArmedGatingStopsTheVehiclesOwnParametersOnly,
    testAVehicleFieldWritesThroughTheSharedParameterEndpoint,
    testTheCheckSendsWhatWasSetAndReadsBackTheWholePage,
    testARefusedWriteIsStillCheckedSoItCanBeWrittenAgain,
    testAfterTheCheckTheFieldsShowWhatTheVehicleHolds,
    testAValueThatDidNotStickIsNamedInItsRowAndKeptForTheNextCheck,
    testAConfirmedChangeIsNotCheckedAgain,
    testACheckThatCannotRunSaysSo,
    testThereIsNothingToCheckWithoutAVehicle,
    testADisconnectedVehicleStillOffersTheEstimator,
    testTeardownReleasesTheSubscription,
    testALateResponseAfterTeardownIsIgnored,
  ];
  for (const t of tests) {
    dispatched = [];
    await t();
    console.log("  ok", t.name);
  }
  console.log(`\n${tests.length} passed — Setup > Battery & Power`);
}

main().catch((err) => { console.error(err); process.exit(1); });
