"use strict";

/**
 * Frontend tests for the Home map's overlays (Corvus.map.setOverlay and
 * friends), the map half of a plugin's api.map.
 *
 * The rule that matters is the stacking: a plugin's line is a reference the
 * flight is compared against, so it must lie UNDER the flown track, whenever
 * it is drawn, before or after the style loads. The fake map below keeps its
 * layers in a real ordered list and honours MapLibre's beforeId, so "under"
 * is read off the stack the way MapLibre would paint it.
 *
 * Run:
 *   node tests/test_frontend_map_overlays.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
global.Event = class Event { constructor(type) { this.type = type; } };
window.requestAnimationFrame = () => 1;
window.cancelAnimationFrame = () => {};
window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;
window.addEventListener = () => {};
window.removeEventListener = () => {};
window.dispatchEvent = () => true;
window.matchMedia = (query) => ({
  matches: false, media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});
if (typeof global.performance === "undefined") global.performance = { now: () => Date.now() };
global.fetch = () => Promise.reject(new Error("offline"));
global.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };

function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {},
    type: "", hidden: false, disabled: false, value: "", id: "", title: "",
    tabIndex: 0, _attrs: {}, _listeners: {}, _isEl: true, parentNode: null,
  };
  e.style = { setProperty() {}, getPropertyValue: () => "" };
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) {
      const next = force === undefined ? !e.classList.contains(c) : !!force;
      if (next) e.classList.add(c); else e.classList.remove(c);
      return next;
    },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => { c.parentNode = e; e.children.push(c); return c; };
  e.append = (...n) => n.forEach((x) => e.appendChild(x));
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); return c; };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.removeEventListener = () => {};
  e.focus = () => {};
  e.closest = () => null;
  e.querySelector = () => null;
  e.querySelectorAll = () => [];
  Object.defineProperty(e, "firstChild", { get: () => e.children[0] || null });
  Object.defineProperty(e, "clientWidth", { get: () => 1000 });
  Object.defineProperty(e, "clientHeight", { get: () => 700 });
  return e;
}

global.document = {
  createElement: makeEl,
  createElementNS: makeEl,
  getElementById: () => makeEl("div"),
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
};

// ---------------------------------------------------------------------------
// A fake map whose layer stack is real: addLayer(spec, beforeId) inserts
// below beforeId, exactly as MapLibre does.
// ---------------------------------------------------------------------------
const mapEvents = {};
const layers = [];             // ids, bottom first
const layerSpecs = {};
const layout = {};
const sources = {};
const fitCalls = [];
const markers = [];            // maplibregl.Marker instances on the map
let bearing = 0;

const fakeMap = {
  on: (type, cb) => { (mapEvents[type] = mapEvents[type] || []).push(cb); },
  off() {},
  addControl() {},
  addSource(id, spec) {
    if (sources[id]) throw new Error(`source ${id} already exists`);
    sources[id] = { spec, data: spec.data, setData(d) { this.data = d; } };
  },
  getSource: (id) => sources[id] || null,
  removeSource(id) {
    if (layers.some((l) => layerSpecs[l].source === id)) throw new Error(`source ${id} in use`);
    delete sources[id];
  },
  addLayer(spec, beforeId) {
    if (layerSpecs[spec.id]) throw new Error(`layer ${spec.id} already exists`);
    layerSpecs[spec.id] = spec;
    layout[spec.id] = Object.assign({}, spec.layout || {});
    const i = beforeId ? layers.indexOf(beforeId) : -1;
    if (i < 0) layers.push(spec.id); else layers.splice(i, 0, spec.id);
  },
  getLayer: (id) => layerSpecs[id] || undefined,
  removeLayer(id) {
    const i = layers.indexOf(id);
    if (i >= 0) layers.splice(i, 1);
    delete layerSpecs[id];
    delete layout[id];
  },
  setPaintProperty() {},
  setLayoutProperty(id, k, v) { layout[id][k] = v; },
  fitBounds(bounds, opts) { fitCalls.push({ bounds, opts }); },
  getCanvas: () => makeEl("canvas"),
  getCanvasContainer: () => makeEl("div"),
  getContainer: () => makeEl("div"),
  getPitch: () => 0, getZoom: () => 13, getBearing: () => bearing, isMoving: () => false,
  getCenter: () => ({ lng: 11, lat: 48 }),
  zoomIn() {}, zoomOut() {}, resize() {}, setPixelRatio() {},
  easeTo() {}, jumpTo() {}, setCenter() {},
  project: () => ({ x: 500, y: 350 }),
};

global.maplibregl = {
  // The style's own layers (the base imagery) are the bottom of the stack.
  Map: function (opts) {
    ((opts && opts.style && opts.style.layers) || []).forEach((l) => {
      layerSpecs[l.id] = l;
      layout[l.id] = {};
      layers.push(l.id);
    });
    return fakeMap;
  },
  AttributionControl: function () { return {}; },
  Marker: function (opts) {
    const m = {
      opts: opts || {}, lngLat: null, added: false,
      setLngLat(ll) { m.lngLat = ll; return m; },
      addTo() { m.added = true; markers.push(m); return m; },
      remove() { m.added = false; const i = markers.indexOf(m); if (i >= 0) markers.splice(i, 1); },
      setRotation() { return m; }, getElement: () => (m.opts.element || makeEl("div")),
    };
    return m;
  },
};

Corvus.telemetry = {
  requestJson: () => Promise.reject(new Error("offline")),
  getState: () => ({ connected: false, position: [0, 0] }),
  subscribe() { return () => {}; },
};

require("../src/js/ui.js");
require("../src/js/map.js");
const map = Corvus.map;

const LINE = [[11.0, 48.0], [11.001, 48.001], [11.002, 48.0005]];

// Drawn BEFORE the style loads: kept, not lost.
assert.equal(map.setOverlay("early", LINE, { color: "#22D3EE" }), true);
assert.equal(map.hasOverlay("early"), true);

map.init(makeEl("div"), makeEl("div"));
(mapEvents.load || []).forEach((cb) => cb());
assert.equal(map.isReady(), true, "the fake map must reach the loaded state");

const below = (a, b) => layers.indexOf(a) >= 0 && layers.indexOf(b) >= 0
  && layers.indexOf(a) < layers.indexOf(b);

// ---------------------------------------------------------------------------

function testALineDrawnBeforeLoadIsDrawnOnLoadUnderTheTrack() {
  assert.ok(layers.includes("overlay-early-line"), "drawn once the style loaded");
  assert.ok(below("overlay-early-casing", "overlay-early-line"), "casing under the colour");
  assert.ok(below("overlay-early-line", "path-past-glow"), "under the earlier flights");
  assert.ok(below("overlay-early-line", "path-line"), "under the flight in progress");
  assert.ok(below("base", "overlay-early-casing"), "over the imagery");
}

function testALineDrawnAfterLoadIsStillUnderTheTrack() {
  assert.equal(map.setOverlay("late", LINE, { color: "#A3E635", width: 4 }), true);
  assert.ok(below("overlay-late-line", "path-past-glow"));
  assert.ok(below("overlay-late-line", "path-line"));
  assert.ok(below("overlay-late-line", "waypoints-route"), "under the plan route as well");
  const spec = layerSpecs["overlay-late-line"];
  assert.equal(spec.paint["line-color"], "#A3E635");
  assert.equal(spec.paint["line-width"], 4);
  map.removeOverlay("late");
}

function testDrawingAgainReplacesRatherThanAdds() {
  map.setOverlay("again", LINE, { color: "#22D3EE" });
  map.setOverlay("again", LINE.slice(0, 2), { color: "#D946EF", dashed: true });
  assert.equal(layers.filter((l) => l === "overlay-again-line").length, 1, "one line, not two");
  assert.equal(sources["overlay-again"].data.geometry.coordinates.length, 2, "the new points");
  assert.equal(layerSpecs["overlay-again-line"].paint["line-color"], "#D946EF");
  assert.deepEqual(layerSpecs["overlay-again-line"].paint["line-dasharray"], [2, 1.5]);
  map.setOverlay("again", LINE, { color: "#D946EF" });
  assert.equal(layerSpecs["overlay-again-line"].paint["line-dasharray"], undefined,
    "a dash that is no longer asked for is gone");
  assert.ok(below("overlay-again-line", "path-past-glow"), "still under the track after a redraw");
  map.removeOverlay("again");
}

function testRemoveTakesLayersAndSourceAway() {
  map.setOverlay("gone", LINE);
  assert.equal(map.removeOverlay("gone"), true);
  assert.ok(!layers.some((l) => l.startsWith("overlay-gone")), "no layer left");
  assert.equal(sources["overlay-gone"], undefined, "no source left");
  assert.equal(map.hasOverlay("gone"), false);
  assert.equal(map.removeOverlay("gone"), false, "a second remove has nothing to do");
}

function testHidingKeepsTheLine() {
  map.setOverlay("hide", LINE);
  assert.equal(map.setOverlayVisible("hide", false), true);
  assert.equal(layout["overlay-hide-line"].visibility, "none");
  assert.equal(layout["overlay-hide-casing"].visibility, "none");
  assert.equal(map.hasOverlay("hide"), true);
  map.setOverlayVisible("hide", true);
  assert.equal(layout["overlay-hide-line"].visibility, "visible");
  // A hidden line redrawn stays hidden.
  map.setOverlay("hide", LINE, { visible: false });
  assert.equal(layout["overlay-hide-line"].visibility, "none");
  map.removeOverlay("hide");
  assert.equal(map.setOverlayVisible("hide", true), false, "nothing to show");
}

function testTooFewUsablePointsDrawNothing() {
  assert.equal(map.setOverlay("bad", [[11, 48]]), false, "one point is not a line");
  assert.equal(map.setOverlay("bad", [[11, 48], [NaN, 48], [11, 95]]), false,
    "points off the globe do not count");
  assert.equal(map.setOverlay("bad", "nonsense"), false);
  assert.equal(map.setOverlay("", LINE), false, "an overlay needs a name");
  assert.equal(map.hasOverlay("bad"), false);
  const kept = map.setOverlay("mixed", [[11, 48], ["x", 1], [11.1, 48.1, 500]]);
  assert.equal(kept, true);
  assert.deepEqual(map._overlays().mixed.coords, [[11, 48], [11.1, 48.1]],
    "the unusable point is dropped and the third value ignored");
  map.removeOverlay("mixed");
}

function testSwitchingTheImageryKeepsTheLineOnTop() {
  map.setBaseLayer(map.getBaseLayer());
  assert.ok(layers.includes("base"), "the imagery is back");
  assert.ok(below("base", "overlay-early-casing"), "and under the line, not over it");
}

function testFitFramesThePointsAndStopsFollowing() {
  map.setFollow(true);
  fitCalls.length = 0;
  assert.equal(map.fitCoords(LINE), true);
  assert.equal(fitCalls.length, 1);
  assert.deepEqual(fitCalls[0].bounds, [[11.0, 48.0], [11.002, 48.001]]);
  assert.equal(fitCalls[0].opts.bearing, 0, "the map's own bearing, not MapLibre's north up");
  assert.equal(map.isFollowing(), false, "the aircraft does not pull the view straight back");
  bearing = 37;
  map.fitCoords(LINE);
  assert.equal(fitCalls[1].opts.bearing, 37, "a rotated map stays rotated");
  bearing = 0;
  assert.equal(map.fitCoords([]), false, "nothing to frame");
  map.setFollow(true);
}


const SQUARE = [[11.0, 48.0], [11.01, 48.0], [11.01, 48.01], [11.0, 48.01]];

function testAnAreaIsFilledUnderTheTrack() {
  assert.equal(map.setPolygonOverlay("zone", SQUARE, { color: "#A3E635", fillOpacity: 0.4 }), true);
  assert.ok(layers.includes("overlay-zone-fill"), "a fill layer");
  assert.ok(below("overlay-zone-fill", "overlay-zone-casing"), "the fill under its outline");
  assert.ok(below("overlay-zone-line", "path-past-glow"), "under the flown track");
  assert.ok(below("base", "overlay-zone-fill"), "over the imagery");
  const spec = layerSpecs["overlay-zone-fill"];
  assert.equal(spec.type, "fill");
  assert.equal(spec.paint["fill-color"], "#A3E635");
  assert.equal(spec.paint["fill-opacity"], 0.4);
  const geom = sources["overlay-zone"].data.geometry;
  assert.equal(geom.type, "Polygon");
  assert.equal(geom.coordinates[0].length, 5, "the ring is closed");
  assert.deepEqual(geom.coordinates[0][4], geom.coordinates[0][0]);
  map.setOverlayVisible("zone", false);
  assert.equal(layout["overlay-zone-fill"].visibility, "none", "hidden with the outline");
  map.removeOverlay("zone");
  assert.ok(!layers.some((l) => l.startsWith("overlay-zone")), "every layer goes");
  assert.equal(sources["overlay-zone"], undefined);
}

function testAnAreaClosesItselfAndNeedsThreeCorners() {
  const closed = SQUARE.concat([SQUARE[0]]);
  assert.equal(map.setPolygonOverlay("ring", closed), true);
  assert.equal(map._overlays().ring.coords.length, 4, "a repeated first corner is dropped");
  assert.equal(map.setPolygonOverlay("ring", SQUARE.slice(0, 2)), false, "two corners span nothing");
  assert.equal(map.setPolygonOverlay("ring", [SQUARE[0], SQUARE[1], SQUARE[0]]), false,
    "nor do two corners and the first again");
  map.removeOverlay("ring");
  assert.equal(map.setPolygonOverlay("bare", SQUARE, { width: 0 }), true);
  assert.ok(layers.includes("overlay-bare-fill"));
  assert.ok(!layers.includes("overlay-bare-line"), "width 0 draws no outline");
  map.removeOverlay("bare");
}

function testOneKeyHoldsOneShapeOfAnyKind() {
  map.setPolygonOverlay("shape", SQUARE);
  map.setOverlay("shape", LINE);
  assert.ok(!layers.includes("overlay-shape-fill"), "the area's fill went with it");
  assert.equal(sources["overlay-shape"].data.geometry.type, "LineString");
  map.setTextOverlay("shape", [11, 48], "Here");
  assert.ok(!layers.some((l) => l.startsWith("overlay-shape")), "a label has no layers");
  assert.equal(sources["overlay-shape"], undefined);
  map.removeOverlay("shape");
}

function testTextIsAPlainLabelAtItsPoint() {
  const before = markers.length;
  assert.equal(map.setTextOverlay("t", [11.5, 48.5], "<b>Landing</b>   zone", { color: "#FFFFFF", size: 50 }), true);
  assert.equal(markers.length, before + 1, "one marker");
  const m = markers[markers.length - 1];
  assert.deepEqual(m.lngLat, [11.5, 48.5]);
  const el = m.opts.element;
  assert.ok(el.className.includes("map-overlay-label"));
  const text = el.children.find((c) => c.className === "map-overlay-label-text");
  assert.equal(text.textContent, "<b>Landing</b> zone", "plain text, whitespace collapsed");
  assert.equal(map._overlays().t.size, 32, "size clamped");
  assert.equal(m.opts.anchor, "center", "centred without a dot");

  map.setOverlayVisible("t", false);
  assert.equal(el.hidden, true);
  map.setOverlayVisible("t", true);
  assert.equal(el.hidden, false);

  map.setTextOverlay("t", [11.5, 48.5], "Pad", { dot: true });
  assert.equal(markers.length, before + 1, "redrawn, not added");
  const dotted = markers[markers.length - 1];
  assert.equal(dotted.opts.anchor, "left", "the text beside the dot");
  assert.ok(dotted.opts.element.className.includes("has-dot"));

  assert.equal(map.setTextOverlay("bad", [11, 95], "x"), false, "a point off the globe");
  assert.equal(map.setTextOverlay("bad", [11, 48], "   "), false, "no text");
  assert.equal(map.setTextOverlay("long", [11, 48], "x".repeat(500)), true);
  assert.equal(map._overlays().long.text.length, 200, "long text is cut");
  map.removeOverlay("long");
  map.removeOverlay("t");
  assert.equal(markers.length, before, "remove takes the marker away");
}

const tests = [
  testALineDrawnBeforeLoadIsDrawnOnLoadUnderTheTrack,
  testALineDrawnAfterLoadIsStillUnderTheTrack,
  testDrawingAgainReplacesRatherThanAdds,
  testRemoveTakesLayersAndSourceAway,
  testHidingKeepsTheLine,
  testTooFewUsablePointsDrawNothing,
  testSwitchingTheImageryKeepsTheLineOnTop,
  testFitFramesThePointsAndStopsFollowing,
  testAnAreaIsFilledUnderTheTrack,
  testAnAreaClosesItselfAndNeedsThreeCorners,
  testOneKeyHoldsOneShapeOfAnyKind,
  testTextIsAPlainLabelAtItsPoint,
];

let failed = 0;
for (const t of tests) {
  try {
    t();
    console.log(`ok   - ${t.name}`);
  } catch (err) {
    failed++;
    console.error(`FAIL - ${t.name}`);
    console.error(`      ${err && err.stack ? err.stack.split("\n").join("\n      ") : err}`);
  }
}
if (failed) {
  console.error(`\n${failed}/${tests.length} map overlay test(s) FAILED`);
  process.exit(1);
}
console.log(`\nAll ${tests.length} map overlay tests passed.`);
process.exit(0);
