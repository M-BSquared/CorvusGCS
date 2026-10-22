"use strict";

/**
 * The GPS and Battery cards behind the top bar.
 *
 * What is pinned: a card says nothing while disconnected (the popover then
 * refuses to open), a row the vehicle has no answer for is left out rather
 * than printed as zero (jamming and spoofing included, when the receiver
 * cannot tell), and flight time left appears only when there is
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

require("./../src/js/units.js");
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

test("jamming and spoofing are left out when the receiver does not report them", () => {
  const d = detail("gps", GPS);
  assert.equal(row(d, "Jamming"), undefined);
  assert.equal(row(d, "Spoofing"), undefined);
  assert.equal(d.notes.length, 0);
});

test("only the integrity verdict the receiver gives is shown", () => {
  const d = detail("gps", { ...GPS, gps_jamming: "mitigated" });
  assert.deepEqual(row(d, "Jamming"), ["Jamming", "Detected, mitigated", "warning"]);
  assert.equal(row(d, "Spoofing"), undefined);
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
  assert.equal(d.notes.length, 0);
});

test("Corvus's drain estimate when the autopilot has none", () => {
  const d = detail("battery", { ...BATTERY, armed: true, battery_endurance_est: 3900 });
  assert.equal(row(d, "Flight time left")[1], "about 1 h 05 min");
  assert.equal(d.notes.length, 0);
});

const ALTITUDE = {
  connected: true, altitude_amsl: 512.4, altitude_agl: 37.2, gps_altitude: 509.8,
};

test("nothing to say about altitude while disconnected", () => {
  assert.equal(detail("altitude", { connected: false }), null);
});

test("Altitude card: AMSL, above home and the receiver's own altitude, no prose", () => {
  const d = detail("altitude", ALTITUDE);
  assert.equal(d.title, "Altitude");
  assert.equal(row(d, "AMSL")[1], "512 m");
  assert.equal(row(d, "Above home")[1], "37 m");
  assert.equal(row(d, "GPS")[1], "510 m");
  assert.equal(d.notes.length, 0);
});

test("Altitude card leaves GPS out without a 3D fix", () => {
  for (const none of [null, undefined, "x"]) {
    assert.equal(row(detail("altitude", { ...ALTITUDE, gps_altitude: none }), "GPS"), undefined);
  }
  assert.equal(row(detail("altitude", { ...ALTITUDE, gps_altitude: 0 }), "GPS")[1], "0 m");
});

test("AGL is the height over the ground under the vehicle, never below zero", () => {
  Corvus.topbar.setGroundElevation(0, 480);
  assert.equal(row(detail("altitude", ALTITUDE), "AGL")[1], "32 m");
  Corvus.topbar.setGroundElevation(0, 530);
  assert.equal(row(detail("altitude", ALTITUDE), "AGL")[1], "0 m");
  assert.equal(row(detail("altitude", ALTITUDE), "Above home")[1], "37 m");
});

test("AGL counts from the take-off ground when it is known, not from the autopilot's AMSL", () => {
  Corvus.topbar.setGroundElevation(0, 520, 500);
  assert.equal(row(detail("altitude", ALTITUDE), "AGL")[1], "17 m");
  Corvus.topbar.setGroundElevation(0, 480, 500);
  assert.equal(row(detail("altitude", ALTITUDE), "AGL")[1], "57 m");
  Corvus.topbar.setGroundElevation(0, null);
});

test("AGL is left out without ground data or for another vehicle", () => {
  Corvus.topbar.setGroundElevation(0, null);
  assert.equal(row(detail("altitude", ALTITUDE), "AGL"), undefined);
  Corvus.topbar.setGroundElevation(4, 480);
  assert.equal(row(detail("altitude", ALTITUDE), "AGL"), undefined);
  Corvus.topbar.setGroundElevation(0, null);
});

test("DEM pixels decode in both encodings", () => {
  assert.equal(Corvus.topbar.decodeElevation(128, 0, 0, "terrarium"), 0);
  assert.equal(Corvus.topbar.decodeElevation(129, 244, 0, "terrarium"), 500);
  assert.equal(Math.round(Corvus.topbar.decodeElevation(1, 134, 160, "mapbox")), 0);
});

test("the tile under a point is the slippy-map one", () => {
  const t = Corvus.topbar.groundTileAt(0, 0, 1);
  assert.deepEqual([t.x, t.y], [1, 1]);
  assert.equal(Corvus.topbar.groundTileAt(-180, 0, 0).fx, 0);
});

test("the distance sensor row exists only when Settings asks for it", () => {
  const withSensor = { ...ALTITUDE, rangefinder_distance: 4.27 };
  assert.equal(row(detail("altitude", withSensor), "Distance sensor"), undefined);
  Corvus.topbar.setRangefinder(true);
  assert.equal(row(detail("altitude", withSensor), "Distance sensor")[1], "4.27 m");
  assert.equal(row(detail("altitude", { ...withSensor, rangefinder_distance: 23.4 }),
    "Distance sensor")[1], "23.4 m");
  assert.deepEqual(row(detail("altitude", { ...ALTITUDE, rangefinder_distance: null }), "Distance sensor"),
    ["Distance sensor", "no reading", "muted"]);
  Corvus.topbar.setRangefinder(false);
  assert.equal(row(detail("altitude", withSensor), "Distance sensor"), undefined);
});

const FLIGHT = { connected: true, armed: false, vehicle_id: 1, boot_ms: 1000, groundspeed: 0 };
const feed = (patch) => { const s = { ...FLIGHT, ...patch }; Corvus.topbar.trackTopSpeed(s); return s; };
const top = (s) => row(detail("groundspeed", s), "Top speed")[1];

test("groundspeed card: nothing while disconnected", () => {
  assert.equal(detail("groundspeed", { connected: false }), null);
});

test("groundspeed card keeps the highest speed of the flight", () => {
  feed({});
  feed({ armed: true, groundspeed: 4 });
  feed({ armed: true, groundspeed: 12.5 });
  const s = feed({ armed: true, groundspeed: 7 });
  assert.equal(top(s), Corvus.units.formatSpeed(12.5));
});

test("groundspeed card still shows the top speed after landing", () => {
  const s = feed({ armed: false, groundspeed: 0 });
  assert.equal(top(s), Corvus.units.formatSpeed(12.5));
});

test("arming again starts a new flight", () => {
  feed({ armed: true, groundspeed: 3 });
  const s = feed({ armed: true, groundspeed: 2 });
  assert.equal(top(s), Corvus.units.formatSpeed(3));
});

test("a link that drops and returns mid-flight keeps the top speed", () => {
  feed({ armed: true, groundspeed: 9 });
  feed({ connected: false, armed: false, groundspeed: 0 });
  const s = feed({ armed: true, groundspeed: 1 });
  assert.equal(top(s), Corvus.units.formatSpeed(9));
});

test("an autopilot reboot starts over", () => {
  const s = feed({ armed: false, boot_ms: 50, groundspeed: 0 });
  assert.equal(top(s), Corvus.units.formatSpeed(0));
});

test("each vehicle keeps its own top speed", () => {
  feed({ vehicle_id: 2, armed: true, groundspeed: 5 });
  feed({ vehicle_id: 3, armed: true, groundspeed: 20 });
  assert.equal(top(feed({ vehicle_id: 2, armed: true, groundspeed: 1 })), Corvus.units.formatSpeed(5));
});

test("groundspeed card is one row with no note under it", () => {
  const d = detail("groundspeed", feed({ vehicle_id: 11 }));
  assert.equal(d.rows.length, 1);
  assert.equal(d.notes.length, 0);
});

test("no dashes in the prose", () => {
  const all = [detail("groundspeed", feed({ vehicle_id: 9 })),
detail("gps", { ...GPS, gps_jamming: "mitigated" }),
    detail("altitude", ALTITUDE),
    detail("battery", { ...BATTERY, armed: true, battery_endurance_est: 100,
      battery_cell_voltages: [3.8, 3.82] })];
  all.forEach((d) => {
    const text = d.rows.map((r) => r[0] + " " + r[1]).concat(d.notes).join("\n");
    assert.ok(!/—|–| - /.test(text), text);
  });
});

const climbRows = (s) => {
  const d = detail("vspeed", s);
  return [row(d, "Top climb rate")[1], row(d, "Top sink rate")[1]];
};

test("vertical speed card: nothing while disconnected", () => {
  assert.equal(detail("vspeed", { connected: false }), null);
});

test("vertical speed card keeps the best climb and the worst sink of the flight", () => {
  feed({ vehicle_id: 21, armed: false, vspeed: 0 });
  feed({ vehicle_id: 21, armed: true, vspeed: 2 });
  feed({ vehicle_id: 21, armed: true, vspeed: 5.5 });
  feed({ vehicle_id: 21, armed: true, vspeed: -3 });
  const s = feed({ vehicle_id: 21, armed: true, vspeed: -1 });
  const [climb, sink] = climbRows(s);
  assert.equal(climb, Corvus.units.formatSpeed(5.5));
  assert.equal(sink, Corvus.units.formatSpeed(3));
});

test("a new arming starts the climb and sink records over", () => {
  feed({ vehicle_id: 22, armed: true, vspeed: 4 });
  feed({ vehicle_id: 22, armed: false, vspeed: 0 });
  const s = feed({ vehicle_id: 22, armed: true, vspeed: 1 });
  assert.equal(climbRows(s)[0], Corvus.units.formatSpeed(1));
  assert.equal(climbRows(s)[1], Corvus.units.formatSpeed(0));
});

let failed = 0;
for (const [name, fn] of tests) {
  try { fn(); console.log("ok   " + name); }
  catch (err) { failed += 1; console.log("FAIL " + name + "\n  " + err.message); }
}
if (failed) { console.log(`${failed} failed`); process.exit(1); }
console.log(`${tests.length} passed`);
