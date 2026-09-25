"use strict";

/**
 * Frontend test for src/js/units.js — the display units.
 *
 * A wrong factor here is a wrong number on the HUD with nothing to say so:
 * an altitude read in feet that is really metres is off by a factor of three.
 * The metric default also has to print exactly what the app printed before
 * the setting existed, so the place search and the mission summary keep
 * their wording on a station that never opens the Units card.
 *
 * Run:
 *   node tests/test_frontend_units.js
 */

const assert = require("node:assert/strict");

const store = {};
global.window = global;
global.Corvus = {};
global.localStorage = {
  getItem: (k) => (k in store ? store[k] : null),
  setItem: (k, v) => { store[k] = String(v); },
};
const events = [];
global.CustomEvent = function (type, init) { this.type = type; this.detail = init && init.detail; };
global.dispatchEvent = (e) => { events.push(e); return true; };

require("../src/js/units.js");
const U = Corvus.units;

function testTheDefaultIsMetricAndPrintsWhatTheAppAlwaysPrinted() {
  assert.deepEqual(U.get(), { length: "m", distance: "km", speed: "ms", temperature: "c" });
  assert.equal(U.systemOf(), "metric");
  assert.equal(U.formatLength(120.4), "120 m");
  assert.equal(U.formatSpeed(-1.25, { signed: true }), "-1.3 m/s");
  assert.equal(U.formatSpeed(2, { signed: true }), "+2.0 m/s");
  assert.equal(U.formatTemperature(25), "25.0 °C");
  // The place search's wording, unchanged.
  assert.equal(U.formatDistance(0, { coarse: true }), "0 m");
  assert.equal(U.formatDistance(342, { coarse: true }), "340 m");
  assert.equal(U.formatDistance(1420, { coarse: true }), "1.4 km");
  assert.equal(U.formatDistance(48200, { coarse: true }), "48 km");
  // The mission summary's.
  assert.equal(U.formatDistance(640), "640 m");
  assert.equal(U.formatDistance(2345), "2.35 km");
}

function testImperialConvertsEveryQuantity() {
  U.setSystem("imperial");
  assert.equal(U.systemOf(), "imperial");
  assert.equal(U.formatLength(100), "328 ft");
  assert.equal(U.formatSpeed(10), "22.4 mph");
  assert.equal(U.formatTemperature(0), "32.0 °F");
  assert.equal(U.formatTemperature(100), "212.0 °F");
  assert.equal(U.formatDistance(1609.344), "1.00 mi");
  // A short hop is feet, not a fraction of a mile.
  assert.equal(U.formatDistance(100), "328 ft");
}

function testConversionsRoundTrip() {
  U.set({ length: "ft", speed: "kn", distance: "nmi" });
  assert.ok(Math.abs(U.lengthToSi(U.length(123.4)) - 123.4) < 1e-9);
  assert.ok(Math.abs(U.speedToSi(U.speed(7.5)) - 7.5) < 1e-9);
  assert.ok(Math.abs(U.distanceToSi(U.distance(5000)) - 5000) < 1e-9);
  assert.ok(Math.abs(U.speed(1852 / 3600) - 1) < 1e-12, "one knot is 1852 m per hour");
  assert.equal(U.systemOf(), null, "feet with nautical miles and knots but Fahrenheit is no preset");
  U.set({ temperature: "c" });
  assert.equal(U.systemOf(), "aviation");
}

function testAChangeIsPersistedAndAnnounced() {
  events.length = 0;
  U.set({ speed: "kmh" });
  assert.equal(JSON.parse(store["corvus.units"]).speed, "kmh");
  assert.equal(events.length, 1);
  assert.equal(events[0].type, "corvus:unitschange");
  assert.equal(events[0].detail.speed, "kmh");
  assert.equal(U.formatSpeed(10), "36.0 km/h");
}

function testUnknownValuesFallBackPerQuantity() {
  assert.deepEqual(U.normalize({ length: "furlong", speed: "kn", temperature: 3 }),
                   { length: "m", distance: "km", speed: "kn", temperature: "c" });
  assert.equal(U.fromConfig({}), null);
  assert.equal(U.fromConfig({ ui: { scale: 1 } }), null);
  assert.deepEqual(U.fromConfig({ ui: { units: { length: "ft" } } }),
                   { length: "ft", distance: "km", speed: "ms", temperature: "c" });
  store["corvus.units"] = "{not json";
  assert.deepEqual(U.applySaved(), U.DEFAULTS);
}

function testNonNumbersPrintNothing() {
  U.setSystem("metric");
  assert.equal(U.formatLength(NaN), "");
  assert.equal(U.formatSpeed(undefined), "");
  assert.equal(U.formatTemperature(NaN), "");
  assert.equal(U.formatDistance(Infinity), "");
}

const tests = [
  testTheDefaultIsMetricAndPrintsWhatTheAppAlwaysPrinted,
  testImperialConvertsEveryQuantity,
  testConversionsRoundTrip,
  testAChangeIsPersistedAndAnnounced,
  testUnknownValuesFallBackPerQuantity,
  testNonNumbersPrintNothing,
];
let failed = 0;
for (const t of tests) {
  try { t(); console.log(`ok   - ${t.name}`); }
  catch (e) { failed += 1; console.log(`FAIL - ${t.name}\n      ${e.message}`); }
}
if (failed) { console.log(`${failed} failed`); process.exit(1); }
