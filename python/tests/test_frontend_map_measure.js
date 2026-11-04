"use strict";

/**
 * Frontend test for src/js/map-measure.js, the distance tool on the Home map.
 *
 * A wrong distance is a quiet failure: the label still draws, it just reads
 * something other than the ground between the two points. And a tool that
 * leaves its click handler on after it is switched off turns every later click
 * on the map into a measurement. Both are checked here against a fake map.
 *
 * Run:
 *   node tests/test_frontend_map_measure.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

global.window = global;
global.Corvus = {};

const keyHandlers = new Set();
global.document = {
  createElement: () => ({
    className: "", textContent: "", isConnected: false,
    append() {}, appendChild() {},
  }),
  addEventListener(type, fn) { if (type === "keydown") keyHandlers.add(fn); },
  removeEventListener(type, fn) { if (type === "keydown") keyHandlers.delete(fn); },
};

global.maplibregl = {
  Marker: class {
    constructor(opts) { this.el = opts.element; this.lngLat = null; this.added = false; }
    setLngLat(p) { this.lngLat = p; return this; }
    getElement() { return this.el; }
    addTo() { this.added = true; this.el.isConnected = true; return this; }
    remove() { this.added = false; this.el.isConnected = false; return this; }
  },
};

require("../src/js/map-measure.js");
const measure = Corvus.mapMeasure;

function fakeMap() {
  const handlers = {};
  const sources = {};
  const layers = [];
  const canvas = { style: {} };
  return {
    handlers, sources, layers, canvas,
    isStyleLoaded: () => true,
    getCanvas: () => canvas,
    getSource: (id) => sources[id],
    getLayer: (id) => layers.find((l) => l.id === id),
    addSource(id) { sources[id] = { data: null, setData(d) { this.data = d; } }; },
    removeSource(id) { delete sources[id]; },
    addLayer(l) { layers.push(l); },
    removeLayer(id) { layers.splice(layers.findIndex((l) => l.id === id), 1); },
    on(type, fn) { (handlers[type] = handlers[type] || new Set()).add(fn); },
    off(type, fn) { if (handlers[type]) handlers[type].delete(fn); },
    fire(type, lng, lat) {
      const e = { lngLat: { lng, lat }, preventDefault() {} };
      [...(handlers[type] || [])].forEach((fn) => fn(e));
    },
  };
}

function fresh() {
  measure.detach();
  keyHandlers.clear();
  const map = fakeMap();
  const changes = [];
  measure.attach(map, (on) => changes.push(on));
  return { map, changes };
}

function lineOf(map) {
  const f = map.sources.measure.data.features.find((x) => x.geometry.type === "LineString");
  return f ? f.geometry.coordinates : null;
}

function testDistanceMatchesAKnownSeparation() {
  // One degree of latitude is 111.2 km on the sphere this uses.
  const d = measure._distanceM([11, 48], [11, 49]);
  assert.ok(Math.abs(d - 111195) < 100, `got ${d}`);
  assert.equal(measure._distanceM([11.6, 48.1], [11.6, 48.1]), 0);
  // A degree of longitude is shorter by the cosine of the latitude.
  const e = measure._distanceM([11, 60], [12, 60]);
  assert.ok(Math.abs(e - 55597) < 100, `got ${e}`);
}

function testBearingIsClockwiseFromNorth() {
  assert.equal(Math.round(measure._bearingDeg([11, 48], [11, 49])), 0);
  assert.equal(Math.round(measure._bearingDeg([11, 48], [12, 48])), 90);
  assert.equal(Math.round(measure._bearingDeg([11, 48], [11, 47])), 180);
  assert.equal(Math.round(measure._bearingDeg([11, 48], [10, 48])), 270);
}

function testMidpointSitsBetweenTheEnds() {
  const m = measure._midpoint([11, 48], [11, 50]);
  assert.ok(Math.abs(m[0] - 11) < 1e-9 && Math.abs(m[1] - 49) < 0.01, JSON.stringify(m));
}

function testShortDistancesKeepADecimalWithoutUnitsLoaded() {
  assert.equal(measure._formatDistance(42), "42 m");
}

function testTwoClicksMakeALineAndAThirdStartsOver() {
  const { map } = fresh();
  measure.setActive(true);
  map.fire("click", 11, 48);
  assert.deepEqual(measure.points(), [[11, 48]]);
  map.fire("mousemove", 11.01, 48);
  assert.deepEqual(lineOf(map), [[11, 48], [11.01, 48]], "the line follows the pointer");
  map.fire("click", 11.02, 48);
  assert.deepEqual(lineOf(map), [[11, 48], [11.02, 48]]);
  map.fire("mousemove", 11.5, 48);
  assert.deepEqual(lineOf(map), [[11, 48], [11.02, 48]], "a finished line ignores the pointer");
  map.fire("click", 12, 49);
  assert.deepEqual(measure.points(), [[12, 49]]);
}

function testRightClickClearsThenLeaves() {
  const { map, changes } = fresh();
  measure.setActive(true);
  map.fire("click", 11, 48);
  map.fire("contextmenu", 11, 48);
  assert.deepEqual(measure.points(), []);
  assert.equal(measure.isActive(), true, "the first right click only clears the line");
  map.fire("contextmenu", 11, 48);
  assert.equal(measure.isActive(), false);
  assert.deepEqual(changes, [true, false]);
}

function testEscapeDoesTheSame() {
  const { map } = fresh();
  measure.setActive(true);
  map.fire("click", 11, 48);
  [...keyHandlers].forEach((fn) => fn({ key: "Escape" }));
  assert.deepEqual(measure.points(), []);
  assert.equal(measure.isActive(), true);
  [...keyHandlers].forEach((fn) => fn({ key: "Escape" }));
  assert.equal(measure.isActive(), false);
}

function testTurningItOffLeavesNoHandlerBehind() {
  const { map } = fresh();
  measure.setActive(true);
  assert.equal(map.canvas.style.cursor, "crosshair");
  map.fire("click", 11, 48);
  measure.setActive(false);
  assert.equal(map.canvas.style.cursor, "");
  assert.equal(keyHandlers.size, 0, "the key listener must go with the tool");
  ["click", "mousemove", "contextmenu"].forEach((type) => {
    assert.equal(map.handlers[type].size, 0, `${type} handler left on the map`);
  });
  assert.deepEqual(measure.points(), []);
  map.fire("click", 12, 49);
  assert.deepEqual(measure.points(), []);
}

function testTheToolWaitsForAStyleRatherThanDrawingOnNothing() {
  const { map } = fresh();
  map.isStyleLoaded = () => false;
  assert.equal(measure.setActive(true), false);
  assert.equal(measure.isActive(), false);
  assert.equal(map.handlers.click, undefined);
}

function testDetachRemovesTheLayers() {
  const { map } = fresh();
  measure.setActive(true);
  assert.equal(map.layers.length, 3);
  measure.detach();
  assert.equal(map.layers.length, 0);
  assert.equal(map.sources.measure, undefined);
}

function read(rel) {
  return fs.readFileSync(path.join(__dirname, "..", "src", rel), "utf8");
}

function testTheMapStandsDownWhileMeasuring() {
  const src = read("js/map.js");
  assert.match(src, /function onMapClick\(e\) \{\s*if \(waypointMode \|\| measuring\(\)/,
    "the context menu must not open on a measuring click");
  assert.match(src, /function onMapRightClick\(e\) \{\s*if \(waypointMode \|\| measuring\(\)/);
  assert.match(src, /data-act="measure"|id: "measure"/, "the rail needs the ruler button");
}

function testTheScriptIsLoadedBeforeTheMapThatMountsIt() {
  const html = read("index.html");
  const at = html.indexOf('src="js/map-measure.js"');
  assert.ok(at > 0, "index.html must load js/map-measure.js");
  assert.ok(at < html.indexOf('src="js/map.js"'), "map-measure.js must load before map.js");
}

function testTheSettingIsWiredBothWays() {
  assert.match(read("js/sidenav.js"), /measure_tool/, "Settings must write measure_tool");
  assert.match(read("js/app.js"), /measure_tool/, "app.js must read measure_tool at start");
}

const tests = [
  testDistanceMatchesAKnownSeparation,
  testBearingIsClockwiseFromNorth,
  testMidpointSitsBetweenTheEnds,
  testShortDistancesKeepADecimalWithoutUnitsLoaded,
  testTwoClicksMakeALineAndAThirdStartsOver,
  testRightClickClearsThenLeaves,
  testEscapeDoesTheSame,
  testTurningItOffLeavesNoHandlerBehind,
  testTheToolWaitsForAStyleRatherThanDrawingOnNothing,
  testDetachRemovesTheLayers,
  testTheMapStandsDownWhileMeasuring,
  testTheScriptIsLoadedBeforeTheMapThatMountsIt,
  testTheSettingIsWiredBothWays,
];

let failures = 0;
tests.forEach((test) => {
  try {
    test();
    console.log(`ok - ${test.name}`);
  } catch (error) {
    failures += 1;
    console.error(`FAIL - ${test.name}\n  ${error.message}`);
  }
});
console.log(`\n${tests.length - failures}/${tests.length} passed`);
process.exit(failures ? 1 : 0);
