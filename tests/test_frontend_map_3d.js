"use strict";

/**
 * Frontend test for the 3D half of src/js/map.js — the maths that decides
 * WHERE the aircraft is drawn.
 *
 * Same pattern as tests/test_frontend_map.js: stub the browser globals the
 * module touches at LOAD time, require the source, and assert on the exposed
 * surface. init() is never called, so there is no MapLibre and no WebGL; what
 * is under test here is arithmetic, which is exactly the part that has no
 * business needing a GPU to check.
 *
 * Why these three:
 *
 *   _transformVec4   — a column-major 4x4 times a vec4. Transposing this by
 *                      accident produces a plausible-looking wrong position,
 *                      the kind of bug that is only visible as "the drone is
 *                      in the wrong place" during a flight.
 *   _projectAltitude — clip space to screen pixels, including the rule that a
 *                      point behind the camera has NO screen position. Without
 *                      that guard the division by a negative w places the
 *                      aircraft mirrored in front of the viewer.
 *   _vehicleDrawAltitude — the preference order that decides which altitude
 *                      the aircraft is drawn at, and its fallbacks. This is
 *                      the difference between an aircraft at its real height
 *                      and one buried in a hill.
 *
 * Run:
 *   node tests/test_frontend_map_3d.js
 */

const assert = require("node:assert/strict");
const path = require("node:path");

// ---------------------------------------------------------------------------
// Minimal browser-ish globals (mirrors tests/test_frontend_map.js)
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};

window.requestAnimationFrame = (cb) => setTimeout(() => cb(Date.now()), 16);
window.cancelAnimationFrame = (id) => clearTimeout(id);
window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;
window.matchMedia = (query) => ({
  matches: false, media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});
if (typeof global.performance === "undefined") global.performance = { now: () => Date.now() };

const elementStub = () => ({
  innerHTML: "", value: "", hidden: false, disabled: false,
  classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } },
  querySelector() { return null; }, querySelectorAll() { return []; },
  appendChild() {}, setAttribute() {}, addEventListener() {},
  dataset: {}, style: {}, firstChild: null,
});
global.document = {
  getElementById() { return elementStub(); },
  createElement() { return elementStub(); },
  createElementNS() { return elementStub(); },
  querySelector() { return null; },
  addEventListener() {}, removeEventListener() {},
  body: elementStub(),
  documentElement: elementStub(),
};
window.document = global.document;

// MercatorCoordinate is the one piece of MapLibre the projection maths calls.
// Reimplemented here from the Web Mercator definition rather than stubbed to a
// constant, so the test exercises the real coordinate the map would hand it.
const EARTH_CIRCUMFERENCE = 40075016.686;
global.maplibregl = {
  MercatorCoordinate: {
    fromLngLat(lngLat, altitude = 0) {
      const lat = lngLat.lat;
      const x = (180 + lngLat.lng) / 360;
      const y = (180 - (180 / Math.PI)
        * Math.log(Math.tan(Math.PI / 4 + (lat * Math.PI) / 360))) / 360;
      const z = altitude / (EARTH_CIRCUMFERENCE * Math.cos((lat * Math.PI) / 180));
      return { x, y, z };
    },
  },
};

require(path.join(__dirname, "..", "src", "js", "map.js"));
const map = Corvus.map;

let passed = 0;
function check(name, fn) { fn(); passed++; console.log("  ok  " + name); }

console.log("map.js — 3D projection and altitude");

// ---------------------------------------------------------------------------
// 1. transformVec4: column-major, the convention MapLibre hands out
// ---------------------------------------------------------------------------

check("identity leaves a vector alone", () => {
  const identity = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
  assert.deepEqual(map._transformVec4(identity, [2, 3, 4, 1]), [2, 3, 4, 1]);
});

check("the translation column is read as a column, not a row", () => {
  // Column-major: the translation lives in elements 12..14. Reading the matrix
  // transposed would put these numbers in the scale positions instead, which
  // still produces a number — just the wrong one.
  const translate = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 5, 6, 7, 1];
  assert.deepEqual(map._transformVec4(translate, [1, 1, 1, 1]), [6, 7, 8, 1]);
});

check("a perspective row produces a w that is not 1", () => {
  // Element 11 is m[2][3] in column-major: the term that makes w depend on z,
  // which is what gives distant things a smaller screen size.
  const perspective = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, -1, 0, 0, 0, 1];
  assert.deepEqual(map._transformVec4(perspective, [0, 0, 2, 1]), [0, 0, 2, -1]);
});

// ---------------------------------------------------------------------------
// 2. projectAltitude: clip space -> screen pixels
// ---------------------------------------------------------------------------

/** A matrix that maps mercator (x, y) straight onto NDC, with no perspective:
 *  ndc = (x, y) * 2 - 1, so the maths is checkable by hand. */
function flatMatrix() {
  return [2, 0, 0, 0, 0, 2, 0, 0, 0, 0, 1, 0, -1, -1, 0, 1];
}

check("the origin of the visible square lands in the middle of the screen", () => {
  // Mercator (0.5, 0.5) is lng 0, lat 0 -> ndc (0, 0) -> the screen centre.
  const at = map._projectAltitude(flatMatrix(), 0, 0, 0, 800, 600);
  assert.ok(at, "a point in front of the camera must project");
  assert.ok(Math.abs(at.x - 400) < 1e-6, `x was ${at.x}`);
  assert.ok(Math.abs(at.y - 300) < 1e-6, `y was ${at.y}`);
});

check("screen y is flipped relative to clip y", () => {
  // Clip space has +y up; a screen has +y down. Getting this backwards draws
  // the aircraft BELOW its own shadow, which reads as it being underground.
  //
  // Asserted on clip y directly rather than on a latitude: Web Mercator's own
  // y already runs southward, so a "point further north" would be testing two
  // flips at once and pass with either one wrong.
  const constantClipY = (ndcY) => {
    const m = new Array(16).fill(0);
    m[15] = 1;        // w = 1
    m[13] = ndcY;     // clip y comes entirely from the translation column
    return m;
  };
  const up = map._projectAltitude(constantClipY(0.5), 0, 0, 0, 800, 600);
  const down = map._projectAltitude(constantClipY(-0.5), 0, 0, 0, 800, 600);
  assert.ok(Math.abs(up.y - 150) < 1e-6, `clip y +0.5 should be 150, was ${up.y}`);
  assert.ok(Math.abs(down.y - 450) < 1e-6, `clip y -0.5 should be 450, was ${down.y}`);
});

check("a point behind the camera has no screen position", () => {
  // w <= 0. The naive division would place it mirrored in FRONT of the
  // viewer — an aircraft drawn on the wrong side of the operator.
  const behind = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -1];
  assert.equal(map._projectAltitude(behind, 0, 0, 0, 800, 600), null);
});

check("a missing matrix is not a crash", () => {
  assert.equal(map._projectAltitude(null, 0, 0, 0, 800, 600), null);
});

check("a non-finite altitude is treated as ground, not as NaN pixels", () => {
  const at = map._projectAltitude(flatMatrix(), 0, 0, NaN, 800, 600);
  assert.ok(at && isFinite(at.x) && isFinite(at.y));
});

// ---------------------------------------------------------------------------
// 3. vehicleDrawAltitude: which altitude the aircraft is drawn at
// ---------------------------------------------------------------------------
//
// Without a map there is no terrain, so terrainElevation() returns null for
// every lookup — which is exactly the no-DEM branch of the preference order,
// and the one that has to keep working when a region was never downloaded.

check("with no DEM, the autopilot's own AMSL is used", () => {
  const altitude = map._vehicleDrawAltitude({
    altitude_amsl: 713, altitude_agl: 140,
    home: [11.39, 47.26], position: [11.39, 47.27],
  });
  assert.equal(altitude, 713);
});

check("with no DEM and no AMSL, the relative altitude keeps it off the ground", () => {
  const altitude = map._vehicleDrawAltitude({
    altitude_amsl: 0, altitude_agl: 55,
    home: [11.39, 47.26], position: [11.39, 47.27],
  });
  assert.equal(altitude, 55);
});

check("a vehicle that reports nothing is drawn on the ground, not at NaN", () => {
  assert.equal(map._vehicleDrawAltitude({}), 0);
  assert.equal(map._vehicleDrawAltitude(null), 0);
});

check("a [0,0] home is not treated as a position", () => {
  // The state store's "no fix yet" default. Reading it as a real home would
  // anchor the aircraft's height to the Gulf of Guinea.
  const altitude = map._vehicleDrawAltitude({
    altitude_amsl: 0, altitude_agl: 30, home: [0, 0], position: [0, 0],
  });
  assert.equal(altitude, 30);
});

check("a negative relative altitude is kept, not clamped away", () => {
  // Flying below the launch point is normal off a ridge, and hiding it would
  // put the aircraft above terrain it is actually below.
  assert.equal(map._vehicleDrawAltitude({ altitude_amsl: 0, altitude_agl: -20 }), -20);
});

// ---------------------------------------------------------------------------
// 4. The building cell grid: bounded, nearest first
// ---------------------------------------------------------------------------
//
// A pitched camera sees to the horizon, and MapLibre's bounds say so — so the
// grid walk has to be bounded by the BUDGET, not by the bounds. Enumerating
// the rectangle first would build a five-figure array on every pan, on the
// frame budget of a field laptop.

const box = (w, s, e, n) => ({ w, s, e, n });

check("nonsense in, nothing out", () => {
  assert.deepEqual(map._buildingCellsIn(null, [11, 47]), []);
  assert.deepEqual(map._buildingCellsIn(box(1, 40, 21, 55), null), []);
  assert.deepEqual(map._buildingCellsIn(box(NaN, 40, 21, 55), [11, 47]), []);
});

check("a horizon-wide view is capped at the cell budget", () => {
  // Twenty degrees of longitude is thousands of z15 columns. Enumerating the
  // rectangle first would materialise every one of them to then keep 24.
  const limits = map._buildingLimits();
  const cells = map._buildingCellsIn(box(1, 40, 21, 55), [11, 47]);
  assert.ok(cells.length <= limits.maxCells,
    `got ${cells.length} cells, budget is ${limits.maxCells}`);
  assert.ok(cells.length > 0, "a view over land must still ask for something");
});

check("cells come back nearest-first, which is what the feature cap relies on", () => {
  const cells = map._buildingCellsIn(box(1, 40, 21, 55), [11, 47]);
  const at = (key) => key.split("/").slice(1).map(Number);
  const [cx, cy] = at(cells[0]);
  let previous = 0;
  for (const key of cells) {
    const [x, y] = at(key);
    // Chebyshev distance: the walk goes out in square rings, so it is that
    // distance — not Euclidean — that must never decrease.
    const ring = Math.max(Math.abs(x - cx), Math.abs(y - cy));
    assert.ok(ring >= previous, `ring went backwards at ${key}`);
    previous = ring;
  }
});

check("the first cell is the one the centre is in", () => {
  // paintBuildings fills its budget from the front, so "first" has to mean
  // "under the aircraft", not "top-left of the bounds".
  const centre = [11.4, 47.27];
  const cells = map._buildingCellsIn(box(11.3, 47.2, 11.5, 47.35), centre);
  const alone = map._buildingCellsIn(box(centre[0], centre[1], centre[0], centre[1]), centre);
  assert.equal(cells[0], alone[0]);
});

check("every cell is asked for once", () => {
  const cells = map._buildingCellsIn(box(11.3, 47.2, 11.5, 47.35), [11.4, 47.27]);
  assert.equal(new Set(cells).size, cells.length);
});

check("a small view is covered completely, not merely sampled", () => {
  // Under the budget, the cap must not be silently dropping ground.
  const cells = map._buildingCellsIn(box(11.39, 47.26, 11.41, 47.28), [11.4, 47.27]);
  const xs = cells.map((k) => Number(k.split("/")[1]));
  const ys = cells.map((k) => Number(k.split("/")[2]));
  const span = (a) => Math.max(...a) - Math.min(...a) + 1;
  assert.equal(cells.length, span(xs) * span(ys), "the covered rectangle has holes");
});

check("cells are addressed at the zoom the backend answers", () => {
  const limits = map._buildingLimits();
  const cells = map._buildingCellsIn(box(11.3, 47.2, 11.5, 47.35), [11.4, 47.27]);
  // The backend serves exactly one zoom, because its cache is keyed by it.
  assert.ok(cells.every((k) => Number(k.split("/")[0]) === limits.cellZoom));
});

check("an inside-out viewport asks for nothing", () => {
  assert.deepEqual(map._buildingCellsIn(box(20, 40, 1, 55), [11, 47]), []);
});

// ---------------------------------------------------------------------------
// 5. The mode's own surface
// ---------------------------------------------------------------------------

check("3D exposes a state that starts off", () => {
  assert.equal(typeof map.set3D, "function");
  assert.equal(map.is3D(), false);
  assert.equal(map.hasTerrain(), false);
});

check("set3D without a map is a no-op, not a crash", () => {
  // The rail is built before the style loads, so the button exists before
  // there is anything for it to act on.
  assert.equal(map.set3D(true), false);
});

console.log(`\n${passed} assertions passed`);
