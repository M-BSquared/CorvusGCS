"use strict";

/**
 * Frontend test for src/js/map.js — the tile-source catalogue.
 *
 * Plain Node-runnable assertions (no browser, no test runner), using the same
 * pattern as tests/test_frontend_link.js: stub the browser globals the module
 * touches at LOAD time, require the source, and assert on the exposed surface.
 * init() is never called, so maplibregl is never needed.
 *
 * `corvus/tile_sources.TILE_SOURCES` is the single source of truth for
 * ids + labels + attribution. The frontend used to keep a hand-mirrored copy
 * of it (documented dual-maintenance); it now HYDRATES the catalogue from
 * GET /api/tiles/sources at init. What this test pins is the consequence of
 * that change: before the fetch resolves the module holds exactly one
 * bootstrap entry — enough to paint an attributed map offline — and the
 * bootstrap entry itself still matches the Python registry, since it is the
 * one source declared in two places.
 *
 * Run:
 *   node tests/test_frontend_map.js
 */

const assert = require("node:assert/strict");
const path = require("node:path");
const fs = require("node:fs");

// ---------------------------------------------------------------------------
// Minimal browser-ish globals so map.js loads in Node (init() is never called,
// so maplibregl is never referenced at load time). Mirrors test_frontend_link.js.
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
  matches: false,
  media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});

if (typeof global.performance === "undefined") global.performance = { now: () => Date.now() };

const elementStub = () => ({
  innerHTML: "", value: "", hidden: false, disabled: false,
  classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } },
  querySelector() { return null; }, querySelectorAll() { return []; },
  appendChild() {}, setAttribute() {}, addEventListener() {},
  dataset: {}, style: {},
});
global.document = {
  getElementById() { return elementStub(); },
  createElement: () => elementStub(),
  createElementNS: () => elementStub(),
  querySelectorAll: () => [],
  addEventListener() {},
};

// Load the map module. Defines Corvus.map (with the _sources test hook).
// ui.js first: it defines Corvus.ui, the component layer every other
// module builds its DOM with (index.html loads it in the same order).
require("../src/js/ui.js");
require("../src/js/map.js");

const map = Corvus.map;
assert.ok(map, "Corvus.map must be defined after requiring map.js");
assert.strictEqual(typeof map._sources, "function", "Corvus.map must expose the _sources test hook");
assert.strictEqual(typeof map._bootstrap, "function", "Corvus.map must expose the _bootstrap test hook");
assert.strictEqual(typeof map.setBaseLayer, "function", "Corvus.map must expose setBaseLayer for the settings picker");

// ---------------------------------------------------------------------------
// The authoritative Python registry, read from disk so the one entry the
// frontend still declares (the bootstrap layer) cannot drift away from it.
// ---------------------------------------------------------------------------
const tileSourcesPy = fs.readFileSync(
  path.join(__dirname, "..", "corvus", "tile_sources.py"),
  "utf-8",
);

// Extract the TILE_SOURCES dict literal the registry declares. A light touch:
// pull each id's label/attribution via regex so we don't eval Python.
function registryEntry(id) {
  const block = tileSourcesPy.split(`"${id}":`)[1] || "";
  const labelMatch = block.match(/"label":\s*"([^"]*)"/);
  const attrMatch = block.match(/"attribution":\s*"([^"]*)"/);
  return {
    label: labelMatch ? labelMatch[1] : null,
    attribution: attrMatch ? attrMatch[1] : null,
  };
}

// The single layer map.js declares itself, so it can paint an attributed
// first frame before /api/tiles/sources resolves.
const BOOTSTRAP_ID = "satellite";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

function testCatalogueStartsWithOnlyTheBootstrapEntry() {
  const cat = map._sources();
  assert.ok(cat, "_sources() must return the catalogue object");
  const ids = Object.keys(cat);
  assert.deepStrictEqual(
    ids, [BOOTSTRAP_ID],
    `before /api/tiles/sources resolves the catalogue must hold only the ` +
    `bootstrap entry, got: ${ids}`,
  );
}

function testBootstrapEntryMatchesTheRegistry() {
  const reg = registryEntry(BOOTSTRAP_ID);
  const boot = map._bootstrap();
  assert.ok(reg.label, `registry entry for ${BOOTSTRAP_ID} has no label (test parse bug?)`);
  assert.strictEqual(boot.id, BOOTSTRAP_ID, "bootstrap id must be the registry's default layer");
  assert.strictEqual(
    boot.label, reg.label,
    `bootstrap label (${boot.label}) != registry (${reg.label})`,
  );
  assert.strictEqual(
    boot.attribution, reg.attribution,
    `bootstrap attribution != registry attribution for ${BOOTSTRAP_ID}`,
  );
  assert.ok(
    typeof boot.maxzoom === "number" && boot.maxzoom > 0,
    "bootstrap entry must carry a usable maxzoom",
  );
}

function testBootstrapEntryHasNonEmptyAttribution() {
  // Offline, with the backend unreachable, this is the only credit string the
  // map has — it must never be blank.
  const attr = map._bootstrap().attribution;
  assert.ok(
    typeof attr === "string" && attr.trim().length > 0,
    `bootstrap attribution must be a non-empty string (got ${JSON.stringify(attr)})`,
  );
}

function testSetBaseLayerRejectsUnknownSources() {
  // A persisted base_layer from a newer build must not blank the map: an id
  // outside the catalogue is ignored and the active layer is left alone.
  const before = map.getBaseLayer();
  map.setBaseLayer("not-a-real-source");
  assert.strictEqual(
    map.getBaseLayer(), before,
    "setBaseLayer must ignore an id that is not in the catalogue",
  );
}

function testSourceTextDoesNotMirrorTheRegistry() {
  // The catalogue is fetched; a re-introduced hardcoded table would silently
  // desync from corvus/tile_sources.py again.
  const src = fs.readFileSync(path.join(__dirname, "..", "src", "js", "map.js"), "utf-8");
  assert.ok(
    src.includes("/api/tiles/sources"),
    "map.js must fetch the source catalogue from /api/tiles/sources",
  );
  for (const id of ["osm", "topo", "hybrid", "streets"]) {
    assert.ok(
      !src.includes(`"${id}"`),
      `map.js hardcodes the source id "${id}"; the catalogue is fetched`,
    );
  }
}

function testSourceTextEnablesAttributionControl() {
  // The map must turn MapLibre's attribution control ON so the credit renders.
  const src = fs.readFileSync(path.join(__dirname, "..", "src", "js", "map.js"), "utf-8");
  assert.ok(
    src.includes("attributionControl: true"),
    "map.js must set attributionControl: true on the MapLibre constructor",
  );
  assert.ok(
    !src.includes("unpkg.com/maplibre"),
    "map.js must not reference the maplibre CDN",
  );
  assert.ok(
    !src.includes("demotiles.maplibre.org"),
    "map.js must not reference remote glyphs",
  );
}

// ---------------------------------------------------------------------------
// _planRouteCoords: plan-route coordinate computation.
//
// The dashed plan line must start at the drone's position, so a single
// waypoint still renders (drone -> wp1). The hook reads live
// Corvus.telemetry.getState() and, by default, the module-internal waypoints.
// In Node the internal array can only be filled via addWaypoint (which needs
// a map), so each case passes the waypoint list explicitly and stubs
// Corvus.telemetry right before the call — the hook reads it live, so swapping
// states per-case is fine.
// ---------------------------------------------------------------------------
function setTelemetryState(state) {
  global.Corvus.telemetry = { getState: () => state };
}
function clearTelemetry() { delete global.Corvus.telemetry; }

function testPlanRouteHookExists() {
  assert.strictEqual(
    typeof map._planRouteCoords, "function",
    "Corvus.map must expose the _planRouteCoords test hook",
  );
}

function testPlanRouteEmptyWhenNoWaypoints() {
  setTelemetryState({ connected: true, position: [8.5, 47.3] });
  assert.deepStrictEqual(map._planRouteCoords([]), [],
    "0 waypoints must yield []");
}

function testPlanRoutePrependsVehicleForSingleWaypoint() {
  setTelemetryState({ connected: true, position: [8.5, 47.3] });
  assert.deepStrictEqual(
    map._planRouteCoords([{ lat: 47.4, lon: 8.6 }]),
    [[8.5, 47.3], [8.6, 47.4]],
    "1 waypoint + connected vehicle must prepend the vehicle [lng, lat]",
  );
}

function testPlanRouteNoVehicleOneWaypointCollapsesToEmpty() {
  setTelemetryState({ connected: false, position: [0, 0] });
  assert.deepStrictEqual(
    map._planRouteCoords([{ lat: 47.4, lon: 8.6 }]),
    [],
    "1 waypoint + no vehicle fix must collapse to [] (>=2-or-empty invariant)",
  );
}

function testPlanRouteNoVehicleTwoWaypointsRendersLine() {
  setTelemetryState({ connected: false, position: [0, 0] });
  assert.deepStrictEqual(
    map._planRouteCoords([{ lat: 47.4, lon: 8.6 }, { lat: 47.5, lon: 8.7 }]),
    [[8.6, 47.4], [8.7, 47.5]],
    "2 waypoints + no vehicle must fall back to waypoint coords only",
  );
}

function testPlanRoutePrependsVehicleForMultipleWaypoints() {
  setTelemetryState({ connected: true, position: [8.5, 47.3] });
  assert.deepStrictEqual(
    map._planRouteCoords([{ lat: 47.4, lon: 8.6 }, { lat: 47.5, lon: 8.7 }]),
    [[8.5, 47.3], [8.6, 47.4], [8.7, 47.5]],
    ">=1 waypoint + connected vehicle must prepend vehicle then list wps",
  );
}

function testPlanRouteZeroPositionTreatedAsNoFix() {
  // connected:true but position [0,0] means no GPS fix -> no vehicle prefix.
  setTelemetryState({ connected: true, position: [0, 0] });
  assert.deepStrictEqual(
    map._planRouteCoords([{ lat: 47.4, lon: 8.6 }, { lat: 47.5, lon: 8.7 }]),
    [[8.6, 47.4], [8.7, 47.5]],
    "connected but [0,0] (no fix) must not prepend a vehicle prefix",
  );
}

function testPlanRouteMissingTelemetryDegradesToWaypointsOnly() {
  // Corvus.telemetry undefined entirely -> defensive null -> waypoints only.
  clearTelemetry();
  assert.deepStrictEqual(
    map._planRouteCoords([{ lat: 47.4, lon: 8.6 }, { lat: 47.5, lon: 8.7 }]),
    [[8.6, 47.4], [8.7, 47.5]],
    "missing Corvus.telemetry must degrade to waypoint coords only",
  );
}

const tests = [
  testCatalogueStartsWithOnlyTheBootstrapEntry,
  testBootstrapEntryMatchesTheRegistry,
  testBootstrapEntryHasNonEmptyAttribution,
  testSetBaseLayerRejectsUnknownSources,
  testSourceTextDoesNotMirrorTheRegistry,
  testSourceTextEnablesAttributionControl,
  testPlanRouteHookExists,
  testPlanRouteEmptyWhenNoWaypoints,
  testPlanRoutePrependsVehicleForSingleWaypoint,
  testPlanRouteNoVehicleOneWaypointCollapsesToEmpty,
  testPlanRouteNoVehicleTwoWaypointsRendersLine,
  testPlanRoutePrependsVehicleForMultipleWaypoints,
  testPlanRouteZeroPositionTreatedAsNoFix,
  testPlanRouteMissingTelemetryDegradesToWaypointsOnly,
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
  console.error(`\n${failed}/${tests.length} frontend map test(s) FAILED`);
  process.exit(1);
}
console.log(`\nAll ${tests.length} frontend map tests passed.`);
