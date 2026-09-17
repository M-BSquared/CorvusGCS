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
// Metres above SEA LEVEL — the same datum the DEM, the autopilot's AMSL and
// MapLibre 5's own rendering all use, which is why none of these need a
// conversion. (MapLibre 4 drew relative to the terrain under the map centre,
// and getting that wrong put the aircraft hundreds of pixels off.)
//
// Without a map there is no terrain at all, which is the FLAT-WORLD branch:
// the ground MapLibre draws is the sea-level plane, the shadow sits on it,
// and the leader line between them is what the operator reads as height. So
// the answer there is height above GROUND — an AMSL would draw a 700 m line
// under an aircraft whose own label says 140 m. It is also the branch a field
// laptop lands on whenever the elevation for an area was never downloaded.

check("with no DEM the aircraft is drawn at its height above the ground", () => {
  // The flat plane IS the ground, so the relative altitude is the height
  // above it. Drawing the AMSL instead made the leader line disagree with the
  // number printed beside it.
  const altitude = map._vehicleDrawAltitude({
    altitude_amsl: 713, altitude_agl: 140,
    home: [11.39, 47.26], position: [11.39, 47.27],
  });
  assert.equal(altitude, 140);
});

check("with no DEM and no relative altitude, AMSL is better than nothing", () => {
  const altitude = map._vehicleDrawAltitude({
    altitude_amsl: 713, position: [11.39, 47.27],
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
// 5. Globe or terrain: the handover rule
// ---------------------------------------------------------------------------
//
// The two cannot both be on. MapLibre 5.24 answers queryTerrainElevation with
// 0 under the globe projection, so a terrain-enabled globe tells the camera
// the ground is at sea level — and over a 600 m valley floor that is a camera
// underground and a map that renders nothing. The split is also simply right:
// from orbit a 600 m hill is under a pixel, and a globe at street level is a
// flat map with extra maths.

check("far out is the globe, close in is terrain", () => {
  const { max } = map._globeZooms();
  assert.equal(map._wantsGlobe(2, false), true, "the world view is the globe");
  assert.equal(map._wantsGlobe(max + 4, false), false, "the field view is terrain");
});

check("the handover has hysteresis, so a zoom on the threshold does not flap", () => {
  const { max, band } = map._globeZooms();
  // Sitting exactly on the nominal threshold: whatever is showing stays.
  assert.equal(map._wantsGlobe(max, true), true, "globe stays globe at the line");
  assert.equal(map._wantsGlobe(max, false), false, "terrain stays terrain at the line");
  // And the band is real in both directions.
  assert.equal(map._wantsGlobe(max + band - 0.01, true), true);
  assert.equal(map._wantsGlobe(max + band + 0.01, true), false);
  assert.equal(map._wantsGlobe(max - band - 0.01, false), true);
  assert.equal(map._wantsGlobe(max - band + 0.01, false), false);
});

check("a zoom that is not a number changes nothing", () => {
  // A transform mid-change is not a reason to tear down the world.
  assert.equal(map._wantsGlobe(NaN, true), true);
  assert.equal(map._wantsGlobe(NaN, false), false);
  assert.equal(map._wantsGlobe(undefined, true), true);
});

// ---------------------------------------------------------------------------
// 6. The elevation pixel that keeps the map from going white
// ---------------------------------------------------------------------------
//
// MapLibre's camera focuses on sea level until told otherwise, and it will
// only report the ground height once it can SEE the ground. Attach terrain
// over a 600 m valley and the camera is underground: nothing renders, and
// nothing can tell it why — the map sits white until the operator happens to
// zoom out far enough to clear the mountain.
//
// map.js breaks that circle by decoding one pixel of one elevation tile
// itself, before terrain is attached. This is that decode. A wrong formula
// here puts the camera in the wrong place, which is the same white map with
// a different cause, so the packings are pinned against their definitions.

check("terrarium decodes to metres above sea level", () => {
  map._setTerrainSpec({ encoding: "terrarium", maxzoom: 15 });
  // height = R * 256 + G + B / 256 - 32768, so the zero point is R=128.
  assert.equal(map._decodeDemPixel(128, 0, 0), 0, "R=128 is sea level");
  assert.equal(map._decodeDemPixel(128, 100, 0), 100);
  assert.equal(map._decodeDemPixel(129, 0, 0), 256);
  assert.equal(map._decodeDemPixel(128, 0, 128), 0.5, "the blue channel is the fraction");
  // Below sea level has to come back negative, not wrap.
  assert.equal(map._decodeDemPixel(127, 156, 0), -100);
});

check("a real summit decodes to a plausible height", () => {
  map._setTerrainSpec({ encoding: "terrarium", maxzoom: 15 });
  // Innsbruck's valley floor, the case that produced the white map.
  const metres = map._decodeDemPixel(130, 62, 0);
  assert.ok(metres > 570 && metres < 580, `expected ~574 m, got ${metres}`);
});

check("mapbox packing is decoded by its own formula", () => {
  // A second DEM would arrive with a different encoding, and silently reading
  // it as terrarium gives heights that are wrong by kilometres.
  map._setTerrainSpec({ encoding: "mapbox", maxzoom: 15 });
  assert.ok(Math.abs(map._decodeDemPixel(1, 134, 160) - 0) < 0.2, "mapbox zero point");
  assert.ok(Math.abs(map._decodeDemPixel(1, 134, 170) - 1) < 0.2);
});

check("an unknown encoding falls back to terrarium rather than to NaN", () => {
  map._setTerrainSpec({ encoding: undefined, maxzoom: 15 });
  assert.equal(map._decodeDemPixel(128, 0, 0), 0);
  map._setTerrainSpec(null);
  assert.equal(map._decodeDemPixel(128, 0, 0), 0);
});

// ---------------------------------------------------------------------------
// 7. What a terrain reading means
// ---------------------------------------------------------------------------
//
// MapLibre answers queryTerrainElevation with 0 — not null — for a point
// whose DEM tile is not loaded. Every point outside the current view is such
// a point, and so is every point at all for the first second or two after
// terrain is attached. Taken as sea level it is the difference between an
// aircraft drawn 150 m above an alpine valley and one drawn 1.5 km inside the
// mountain, and the usual casualty is the HOME point: a few kilometres into a
// sortie its tile is no longer loaded, and the height the whole preference
// order is anchored to silently becomes zero.

check("a literal zero is not an answer about the ground", () => {
  assert.equal(map._readTerrainValue(0), null);
  assert.equal(map._readTerrainValue(-0), null);
});

check("a real height is passed straight through, sign and all", () => {
  assert.equal(map._readTerrainValue(2962), 2962);
  // Below sea level is real ground: the Dead Sea, a polder, a dry lake bed.
  assert.equal(map._readTerrainValue(-413), -413);
  assert.equal(map._readTerrainValue(0.5), 0.5);
});

check("nothing, and nonsense, are both unknown", () => {
  assert.equal(map._readTerrainValue(null), null);
  assert.equal(map._readTerrainValue(undefined), null);
  assert.equal(map._readTerrainValue(NaN), null);
  assert.equal(map._readTerrainValue(Infinity), null);
});

// ---------------------------------------------------------------------------
// 8. Aiming the camera at an aircraft that is not on the ground
// ---------------------------------------------------------------------------
//
// In 3D the marker is drawn at altitude, well above the ground point the
// camera can actually be told to centre on. The offset between the two is
// measured on screen, in one frame, and handed to easeTo — which is what
// makes it need no model of the pitch, the zoom or the terrain. It replaced
// un-projecting the marker's own position, which worked low and failed high:
// a marker above the horizon un-projects to nowhere useful, the code fell
// back to the ground point, and follow mode quietly stopped following.

check("no airborne marker means no offset", () => {
  assert.deepEqual(map._airborneOffset(null, { x: 1, y: 2 }, 800, 600), [0, 0]);
  assert.deepEqual(map._airborneOffset({ x: 1, y: 2 }, null, 800, 600), [0, 0]);
});

check("the offset is the marker's own rise above its shadow", () => {
  // Shadow at the centre, aircraft 200 px above it: the ground has to sit 200
  // px BELOW the middle for the aircraft to land in it.
  assert.deepEqual(
    map._airborneOffset({ x: 400, y: 100 }, { x: 400, y: 300 }, 800, 600),
    [0, 200]);
});

check("a rotated camera offsets sideways too", () => {
  assert.deepEqual(
    map._airborneOffset({ x: 330, y: 100 }, { x: 400, y: 300 }, 800, 600),
    [70, 200]);
});

check("an aircraft on its own shadow asks for nothing", () => {
  assert.deepEqual(
    map._airborneOffset({ x: 400, y: 300 }, { x: 400, y: 300 }, 800, 600),
    [0, 0]);
});

check("a marker a whole viewport up does not throw the ground off screen", () => {
  // Perspective can separate the two by more than the viewport. Past the
  // clamp the marker is allowed to sit high; the ground the aircraft is
  // flying over is worth keeping on screen too.
  const [, dy] = map._airborneOffset({ x: 400, y: -900 }, { x: 400, y: 300 }, 800, 600);
  assert.ok(dy > 0 && dy <= 600 * 0.35 + 1e-9, `clamped, was ${dy}`);
});

check("a degenerate viewport asks for nothing rather than for NaN", () => {
  assert.deepEqual(
    map._airborneOffset({ x: 400, y: 100 }, { x: 400, y: NaN }, 800, 600), [0, 0]);
  assert.deepEqual(
    map._airborneOffset({ x: 400, y: 100 }, { x: 400, y: 300 }, 0, 0), [0, 0]);
});

// ---------------------------------------------------------------------------
// 9. What the base imagery has to stay underneath
// ---------------------------------------------------------------------------

check("every overlay is above the base imagery, regions first", () => {
  const overlays = map._overlayLayers();
  const at = (id) => overlays.indexOf(id);
  assert.ok(at("offline-regions-fill") === 0,
    "the downloaded-area rectangles are added first, so they are the floor " +
    "the imagery must stay under — a list starting at the track re-inserted " +
    "the imagery on top of them and switching map service erased them");
  assert.ok(at("offline-regions-line") < at("buildings-3d"));
  assert.ok(at("buildings-3d") < at("path-glow"), "a building never hides the track");
  assert.ok(at("path-line") < at("waypoints-route"));
  assert.ok(overlays.every((id) => typeof id === "string" && id.length));
});

// ---------------------------------------------------------------------------
// 10. The mode's own surface
// ---------------------------------------------------------------------------

check("3D exposes a state that starts off", () => {
  assert.equal(typeof map.set3D, "function");
  assert.equal(map.is3D(), false);
  assert.equal(map.hasTerrain(), false);
  assert.equal(map.get3DMode(), "off");
});

// ---------------------------------------------------------------------------
// 11. The three modes
// ---------------------------------------------------------------------------
//
// "simple" is the camera tilt and the sky, over flat ground: no elevation
// tiles, no building requests, no globe. "full" is the tilt over ground that
// is really shaped, with buildings on it. The split is not cosmetic — the
// second one downloads, and a field laptop on a radio link is exactly where
// that is worth being able to decline.

check("only the two 3D modes are modes; anything else is flat", () => {
  assert.equal(map._normaliseThreeDMode("simple"), "simple");
  assert.equal(map._normaliseThreeDMode("full"), "full");
  assert.equal(map._normaliseThreeDMode("off"), "off");
  // A config file written by a newer build, a typo, a hand-edited JSON: the
  // answer has to be a flat map, never a half-built 3D one.
  assert.equal(map._normaliseThreeDMode("photoreal"), "off");
  assert.equal(map._normaliseThreeDMode(""), "off");
  assert.equal(map._normaliseThreeDMode(undefined), "off");
  assert.equal(map._normaliseThreeDMode(null), "off");
  assert.equal(map._normaliseThreeDMode(true), "off");
  assert.equal(map._normaliseThreeDMode({ mode: "full" }), "off");
});

check("the chosen 3D is remembered while 3D is off", () => {
  // The rail's button turns the mode the operator last picked back on. Asking
  // for "off" must not throw that choice away, or every operator who prefers
  // the cheap mode gets handed the expensive one on their next press.
  assert.equal(map._threeDDetail(), "full", "terrain & buildings is the default");
  map.set3DMode("simple");
  assert.equal(map._threeDDetail(), "simple");
  map.set3DMode("off");
  assert.equal(map._threeDDetail(), "simple", "off is not a third preference");
  map.set3DMode("full");
  assert.equal(map._threeDDetail(), "full");
  // A mode nobody recognises is "off", so it must not be remembered either.
  map.set3DMode("simple");
  map.set3DMode("photoreal");
  assert.equal(map._threeDDetail(), "simple");
});

check("the panel's switch chooses WHICH 3D, never whether", () => {
  // The rail button owns on/off. Flipping the switch while 3D is off records
  // the choice and leaves the map alone — a map that tilts because a pointer
  // wandered onto a switch is a map that moved on its own.
  map.set3DMode("off");
  map.set3DDetail("full");
  assert.equal(map._threeDDetail(), "full");
  assert.equal(map.is3D(), false, "the switch does not turn 3D on");
  map.set3DDetail("simple");
  assert.equal(map._threeDDetail(), "simple");
  assert.equal(map.is3D(), false);
  // "off" is not a detail: the switch has two positions and neither is off.
  map.set3DDetail("off");
  assert.equal(map._threeDDetail(), "simple", "off is not one of the two");
  map.set3DDetail("nonsense");
  assert.equal(map._threeDDetail(), "simple");
});

check("choosing a mode without a map is a no-op, not a crash", () => {
  // The rail is built before the style loads, so the menu can be opened and
  // a row pressed before there is anything to apply it to.
  assert.equal(map.set3DMode("full"), "off");
  assert.equal(map.is3D(), false);
});

check("set3D without a map is a no-op, not a crash", () => {
  // The rail is built before the style loads, so the button exists before
  // there is anything for it to act on.
  assert.equal(map.set3D(true), false);
});

console.log(`\n${passed} assertions passed`);
