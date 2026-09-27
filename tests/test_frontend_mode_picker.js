"use strict";

/**
 * The flight bar's mode picker shows the mode the vehicle reports.
 *
 * It is a <select> under the app's themed dropdown, and the dropdown's label
 * follows only a "change" event or a DOM mutation. Telemetry sets the value
 * programmatically, which is neither, so the bar used to read SELECT MODE
 * while the vehicle held in LOITER.
 *
 * Run:
 *   node tests/test_frontend_mode_picker.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
global.document = {
  addEventListener: () => {},
  createElement: (tag) => makeOption(tag),
};

function makeOption(tag, value = "", text = "") {
  const o = {
    tagName: String(tag || "option").toUpperCase(),
    value, textContent: text, disabled: false, dataset: {}, parent: null,
    remove() { if (o.parent) o.parent._remove(o); },
  };
  return o;
}

function makeSelect(values) {
  const sel = {
    options: [],
    refreshed: 0,
    _value: "",
    get value() { return this._value; },
    // A native select takes only a value one of its options carries.
    set value(v) {
      this._value = this.options.some((o) => o.value === v) ? v : "";
    },
    appendChild(o) { o.parent = sel; sel.options.push(o); return o; },
    _remove(o) { sel.options = sel.options.filter((x) => x !== o); o.parent = null; },
  };
  sel.corvusSelect = { refresh: () => { sel.refreshed += 1; } };
  sel.appendChild(makeOption("option", "", "SELECT MODE"));
  values.forEach((v) => sel.appendChild(makeOption("option", v, v)));
  return sel;
}

require("./../src/js/app.js");
const { showVehicleMode } = Corvus.app;

function testTheLabelFollowsTheVehicle() {
  const sel = makeSelect(["POSCTL", "LOITER", "MISSION"]);
  showVehicleMode(sel, "LOITER");
  assert.equal(sel.value, "LOITER");
  assert.equal(sel.refreshed, 1, "the dropdown's label is refreshed, not left behind");
  showVehicleMode(sel, "MISSION");
  assert.equal(sel.value, "MISSION");
  assert.equal(sel.refreshed, 2);
}

function testAModeTheListLacksIsShownButCannotBePicked() {
  const sel = makeSelect(["POSCTL", "LOITER"]);
  showVehicleMode(sel, "AUTO_VTOL_TAKEOFF");
  assert.equal(sel.value, "AUTO_VTOL_TAKEOFF", "not a blank picker");
  const row = sel.options.find((o) => o.value === "AUTO_VTOL_TAKEOFF");
  assert.equal(row.disabled, true, "reported, not offered");
  assert.equal(sel.options.filter((o) => o.dataset.reported === "true").length, 1);

  showVehicleMode(sel, "OTHER_MODE");
  assert.equal(sel.options.filter((o) => o.dataset.reported === "true").length, 1,
    "one reported row, reused");

  showVehicleMode(sel, "LOITER");
  assert.equal(sel.value, "LOITER");
  assert.equal(sel.options.some((o) => o.dataset.reported === "true"), false,
    "the reported row goes once the mode is one the list offers");
}

function testNoModeIsThePlaceholder() {
  const sel = makeSelect(["LOITER"]);
  showVehicleMode(sel, "LOITER");
  showVehicleMode(sel, "");
  assert.equal(sel.value, "");
}

const tests = [
  testTheLabelFollowsTheVehicle,
  testAModeTheListLacksIsShownButCannotBePicked,
  testNoModeIsThePlaceholder,
];
let failed = 0;
for (const t of tests) {
  try {
    t();
    console.log(`ok   - ${t.name}`);
  } catch (err) {
    failed += 1;
    console.log(`FAIL - ${t.name}\n      ${err.message}`);
  }
}
if (failed) {
  console.log(`\n${failed}/${tests.length} mode picker test(s) FAILED`);
  process.exit(1);
}
console.log(`\nAll ${tests.length} mode picker tests passed.`);
