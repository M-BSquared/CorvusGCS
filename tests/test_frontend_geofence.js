"use strict";

/**
 * Frontend tests for the geofence: Corvus.geofence (the stored area and its
 * dashed outline on the Home map) and the pure helpers of the Geofence card
 * (Corvus.setupGeofence).
 *
 * Run:
 *   node tests/test_frontend_geofence.js
 */

const assert = require("node:assert/strict");
const path = require("node:path");

global.window = global;
global.Corvus = {};

const drawn = [];
Corvus.ui = { token: (_name, fallback) => fallback, onThemeChange() {} };
Corvus.map = {
  setPolygonOverlay: (id, coords, opts) => { drawn.push({ op: "set", id, coords, opts }); return true; },
  removeOverlay: (id) => { drawn.push({ op: "remove", id }); return true; },
};
const posted = [];
let answer = null;
Corvus.telemetry = {
  requestJson: async () => answer,
  postAction: async (url, payload) => { posted.push({ url, payload }); return answer; },
};

require(path.join(__dirname, "..", "src", "js", "geofence.js"));
require(path.join(__dirname, "..", "src", "js", "setup-geofence.js"));

const G = Corvus.geofence;
const SG = Corvus.setupGeofence;
const SQUARE = [[11.0, 48.0], [11.01, 48.0], [11.01, 48.01], [11.0, 48.01]];

const tests = [];
function test(name, fn) { tests.push({ name, fn }); }

test("a point inside the square is inside, one beside it is not", () => {
  assert.equal(G.contains(SQUARE, [11.005, 48.005]), true);
  assert.equal(G.contains(SQUARE, [11.02, 48.005]), false);
  assert.equal(G.contains([], [11.005, 48.005]), false);
});

test("a bow tie crosses itself and a square does not", () => {
  assert.equal(G.selfIntersects(SQUARE), false);
  assert.equal(G.selfIntersects([[0, 0], [1, 1], [1, 0], [0, 1]]), true);
});

test("the area of a 0.01 degree square near Munich is about 0.83 km²", () => {
  const m2 = G.areaM2(SQUARE);
  assert.ok(m2 > 0.8e6 && m2 < 0.86e6, String(m2));
  assert.equal(SG.formatArea(m2).endsWith("ha"), true);
});

test("a double click's two corners on one spot count once", () => {
  const pts = [[11, 48], [11.001, 48], [11.001, 48.001], [11.001, 48.001]];
  assert.equal(SG.distinctCorners(pts).length, 3);
});

test("the card names what is wrong with an area", () => {
  assert.match(SG.problemOf(SQUARE.slice(0, 2), 64), /at least 3/);
  assert.match(SG.problemOf([[0, 0], [1, 1], [1, 0], [0, 1]], 64), /cross/);
  assert.match(SG.problemOf(SQUARE, 3), /at most 3/);
  assert.equal(SG.problemOf(SQUARE, 64), "");
});

test("a stored area is drawn on the Home map as a dashed outline", async () => {
  drawn.length = 0;
  answer = { ok: true, polygon: SQUARE, show_on_map: true, on_vehicle: null };
  await G.load();
  const last = drawn[drawn.length - 1];
  assert.equal(last.op, "set");
  assert.equal(last.id, G.OVERLAY_ID);
  assert.deepEqual(last.coords, SQUARE);
  assert.equal(last.opts.dashed, true);
});

test("turning the toggle off takes the outline away", async () => {
  drawn.length = 0;
  posted.length = 0;
  answer = { ok: true, polygon: SQUARE, show_on_map: false };
  await G.setShowOnMap(false);
  assert.deepEqual(posted[0], { url: "/api/geofence/save", payload: { show_on_map: false } });
  assert.deepEqual(drawn[drawn.length - 1], { op: "remove", id: G.OVERLAY_ID });
});

test("no area draws nothing even with the toggle on", async () => {
  drawn.length = 0;
  answer = { ok: true, polygon: [], show_on_map: true };
  await G.savePolygon([]);
  assert.equal(drawn[drawn.length - 1].op, "remove");
});

test("listeners hear every change, and stop when they unsubscribe", async () => {
  const heard = [];
  const off = G.onChange((s) => heard.push(s.polygon.length));
  answer = { ok: true, polygon: SQUARE, show_on_map: true, on_vehicle: true };
  await G.upload();
  off();
  answer = { ok: true, polygon: [], show_on_map: true };
  await G.clearVehicle();
  assert.deepEqual(heard, [4]);
  assert.equal(posted.some((p) => p.url === "/api/geofence/upload"), true);
  assert.equal(posted.some((p) => p.url === "/api/geofence/clear"), true);
});

(async () => {
  let failed = 0;
  for (const t of tests) {
    try {
      await t.fn();
      console.log(`ok   - ${t.name}`);
    } catch (error) {
      failed += 1;
      console.log(`FAIL - ${t.name}\n${error && error.stack}`);
    }
  }
  if (failed) {
    console.log(`\n${failed}/${tests.length} geofence test(s) FAILED`);
    process.exit(1);
  }
  console.log(`\nall ${tests.length} geofence tests passed`);
})();
