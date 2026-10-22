"use strict";

/**
 * Frontend tests for the FLIGHT TIME cell of the flight telemetry grid.
 *
 * The clock starts at take-off, stops at touchdown or disarm, keeps showing
 * the last flight until the next take-off, and clears on disconnect.
 *
 * Run:
 *   node tests/test_frontend_instruments_flight_time.js
 */

const assert = require("node:assert/strict");
const path = require("node:path");

global.window = global;
global.Corvus = {};

function makeEl() {
  const e = {
    children: [], textContent: "", className: "",
    setAttribute() {}, getAttribute: () => null, removeAttribute() {},
    appendChild: (c) => { e.children.push(c); return c; },
  };
  let html = "";
  Object.defineProperty(e, "innerHTML", { get: () => html, set: (v) => { html = v; e.children.length = 0; } });
  return e;
}

const cells = {};
const grid = makeEl();
grid.appendChild = (c) => { grid.children.push(c); return c; };
grid.querySelector = (sel) => {
  const key = /data-ft="(\w+)"/.exec(sel)[1];
  return (cells[key] = cells[key] || makeEl());
};
global.document = {
  getElementById: (id) => (id === "flightTelemetry" ? grid : makeEl()),
  createElementNS: () => makeEl(),
  createElement: () => makeEl(),
};

let subscriber = null;
Corvus.telemetry = { subscribe: (cb) => { subscriber = cb; } };
Corvus.anim = { add() {}, wake() {}, reducedMotion: () => true, normAngle: (a) => ((a % 360) + 360) % 360 };
Corvus.units = { formatLength: String, formatSpeed: String };

let clock = 1000000;
Date.now = () => clock;

require(path.join(__dirname, "..", "src", "js", "instruments.js"));
Corvus.instruments.init(null);

const base = { connected: true, armed: false, landed_state: 1, altitude_agl: 0, heading: 0, pitch: 0, roll: 0, gps_satellites: 9 };
const feed = (over, advanceS = 0) => {
  clock += advanceS * 1000;
  subscriber({ ...base, ...over });
  return cells.flightTime.textContent;
};

const tests = [];
const test = (name, fn) => tests.push([name, fn]);

test("zero on the ground, armed or not", () => {
  assert.equal(feed({}), "0:00");
  assert.equal(feed({ armed: true }, 30), "0:00");
});

test("counts from take-off in m:ss", () => {
  assert.equal(feed({ armed: true, landed_state: 3 }, 1), "0:00");
  assert.equal(feed({ armed: true, landed_state: 2 }, 75), "1:15");
});

test("stops at touchdown and holds the last flight", () => {
  assert.equal(feed({ armed: true, landed_state: 1 }, 5), "1:20");
  assert.equal(feed({ armed: false, landed_state: 1 }, 600), "1:20");
});

test("the next take-off starts a new clock, hours as h:mm:ss", () => {
  assert.equal(feed({ armed: true, landed_state: 2 }, 10), "0:00");
  assert.equal(feed({ armed: true, landed_state: 2 }, 3725), "1:02:05");
});

test("a disarm in the air stops it", () => {
  assert.equal(feed({ armed: false, landed_state: 2 }, 10), "1:02:15");
  assert.equal(feed({ armed: false, landed_state: 2 }, 100), "1:02:15");
});

test("without a landed state the height decides", () => {
  assert.equal(feed({ armed: true, landed_state: 0, altitude_agl: 0.3 }, 1), "1:02:15");
  assert.equal(feed({ armed: true, landed_state: 0, altitude_agl: 20 }, 1), "0:00");
  assert.equal(feed({ armed: true, landed_state: 0, altitude_agl: 20 }, 42), "0:42");
});

test("a lost link clears it", () => {
  feed({ connected: false });
  assert.equal(cells.flightTime.textContent, "—");
  assert.equal(feed({}, 1), "0:00");
});

let failed = 0;
for (const [name, fn] of tests) {
  try { fn(); console.log("ok   " + name); } catch (e) { failed++; console.log("FAIL " + name + "\n" + e.message); }
}
process.exit(failed ? 1 : 0);
