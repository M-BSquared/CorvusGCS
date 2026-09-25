"use strict";

/**
 * The GPS and Battery cards behind the top bar.
 *
 * What is pinned: a card says nothing while disconnected (the popover then
 * refuses to open), a row the vehicle has no answer for is left out rather
 * than printed as zero, jamming says "Not reported" rather than "None" when
 * the receiver cannot tell, and flight time left appears only when there is
 * an honest figure: armed, and either the autopilot's own or Corvus's drain
 * estimate.
 *
 * Run:
 *   node tests/test_frontend_topbar_detail.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
window.addEventListener = () => {};
window.removeEventListener = () => {};
global.localStorage = { getItem: () => null, setItem: () => {}, removeItem: () => {} };
global.document = { documentElement: { setAttribute: () => {}, getAttribute: () => null } };

require("./../src/js/notification_dedupe.js");
require("./../src/js/topbar.js");

const detail = Corvus.topbar.detail;
const row = (d, label) => d.rows.find((r) => r[0] === label);

const GPS = {
  connected: true, gps_fix: "3D_FIX", gps_satellites: 14, gps_hdop: 0.8,
  gps_vdop: 1.3, gps_h_acc: 0.9, gps_v_acc: -1, gps_health: "ok",
  gps_jamming: "", gps_spoofing: "", gps_signal_quality: -1,
};

const BATTERY = {
  connected: true, armed: false, battery_percent: 64, battery_voltage: 22.8,
  battery_current: 12.3, battery_cells: 6, battery_cell_voltages: [],
  battery_source: "autopilot", battery_percent_fc: 64, battery_percent_est: 58,
  battery_consumed_mah: 1234, battery_temperature: null,
  battery_time_remaining: 0, battery_endurance_est: -1,
};

const tests = [];
function test(name, fn) { tests.push([name, fn]); }

test("nothing to say while disconnected", () => {
  assert.equal(detail("gps", { connected: false }), null);
  assert.equal(detail("battery", { connected: false }), null);
});

test("GPS card: satellites, reception, geometry, accuracy", () => {
  const d = detail("gps", GPS);
  assert.deepEqual(row(d, "Fix"), ["Fix", "3D fix", "healthy"]);
  assert.equal(row(d, "Satellites")[1], "14");
  assert.deepEqual(row(d, "Reception"), ["Reception", "Good", "healthy"]);
  assert.equal(row(d, "HDOP")[1], "0.8 (excellent)");
  assert.equal(row(d, "Horizontal accuracy")[1], "0.90 m");
  assert.equal(row(d, "Vertical accuracy"), undefined, "unknown accuracy is left out");
  assert.equal(row(d, "Signal quality"), undefined);
  assert.deepEqual(row(d, "Receiver health"), ["Receiver health", "OK", "healthy"]);
});

test("jamming not reported is said, not dressed up as fine", () => {
  const d = detail("gps", GPS);
  assert.deepEqual(row(d, "Jamming"), ["Jamming", "Not reported", "muted"]);
  assert.ok(d.notes.some((n) => /GNSS integrity/.test(n)));
});

test("jamming detected is critical", () => {
  const d = detail("gps", { ...GPS, gps_jamming: "detected", gps_spoofing: "ok", gps_signal_quality: 6 });
  assert.deepEqual(row(d, "Jamming"), ["Jamming", "Detected", "critical"]);
  assert.deepEqual(row(d, "Spoofing"), ["Spoofing", "None detected", "healthy"]);
  assert.equal(row(d, "Signal quality")[1], "6 of 10");
  assert.equal(d.notes.length, 0);
});

test("no fix reads as no fix", () => {
  const d = detail("gps", { ...GPS, gps_fix: "NO_FIX", gps_satellites: 3, gps_hdop: 99 });
  assert.deepEqual(row(d, "Reception"), ["Reception", "No fix", "critical"]);
  assert.equal(row(d, "HDOP"), undefined);
});

test("battery card: voltage, per cell, current, power, the other reading", () => {
  const d = detail("battery", BATTERY);
  assert.equal(row(d, "Voltage")[1], "22.8 V");
  assert.equal(row(d, "Per cell")[1], "3.80 V (6S)");
  assert.equal(row(d, "Current")[1], "12.3 A");
  assert.equal(row(d, "Power")[1], "280 W");
  assert.equal(row(d, "Consumed")[1], "1234 mAh");
  assert.equal(row(d, "Cell voltage says")[1], "58%");
  assert.equal(row(d, "Temperature"), undefined);
});

test("no flight time on the ground", () => {
  const d = detail("battery", { ...BATTERY, battery_time_remaining: 900 });
  assert.equal(row(d, "Flight time left"), undefined);
});

test("no flight time while armed without an estimate", () => {
  const d = detail("battery", { ...BATTERY, armed: true });
  assert.equal(row(d, "Flight time left"), undefined);
});

test("the autopilot's time left wins over Corvus's", () => {
  const d = detail("battery", { ...BATTERY, armed: true, battery_time_remaining: 725, battery_endurance_est: 300 });
  assert.equal(row(d, "Flight time left")[1], "about 12 min");
  assert.ok(d.notes.some((n) => /autopilot estimates/.test(n)));
});

test("Corvus's drain estimate when the autopilot has none", () => {
  const d = detail("battery", { ...BATTERY, armed: true, battery_endurance_est: 3900 });
  assert.equal(row(d, "Flight time left")[1], "about 1 h 05 min");
  assert.ok(d.notes.some((n) => /drain/.test(n)));
});

test("no dashes in the prose", () => {
  const all = [detail("gps", { ...GPS, gps_jamming: "mitigated" }),
    detail("battery", { ...BATTERY, armed: true, battery_endurance_est: 100,
      battery_cell_voltages: [3.8, 3.82] })];
  all.forEach((d) => {
    const text = d.rows.map((r) => r[0] + " " + r[1]).concat(d.notes).join("\n");
    assert.ok(!/—|–| - /.test(text), text);
  });
});

let failed = 0;
for (const [name, fn] of tests) {
  try { fn(); console.log("ok   " + name); }
  catch (err) { failed += 1; console.log("FAIL " + name + "\n  " + err.message); }
}
if (failed) { console.log(`${failed} failed`); process.exit(1); }
console.log(`${tests.length} passed`);
