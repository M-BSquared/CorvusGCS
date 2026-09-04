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
// Flown track: distance decimation and reboot detection.
//
// Two rules the operator depends on and neither is visible until it is wrong:
//  - a hovering aircraft must not fill the track buffer with the same point,
//    or a long sortie silently eats its own beginning;
//  - the track must survive a LINK drop and only be discarded on an actual
//    vehicle reboot, or a radio glitch erases a flight that is still airborne.
// ---------------------------------------------------------------------------

function testTrackRecordsTheFirstFix() {
  map._resetTrackState();
  assert.equal(map._recordTrackPoint([8.5, 47.3]), true, "first fix starts the track");
  assert.equal(map.getTrack().length, 1);
}

function testTrackIgnoresNullIsland() {
  // [0,0] is the state store's "no fix yet" default, not a position.
  map._resetTrackState();
  assert.equal(map._recordTrackPoint([0, 0]), false, "[0,0] is not a fix");
  assert.equal(map.getTrack().length, 0);
}

function testTrackIgnoresNonFiniteAndMalformedPositions() {
  map._resetTrackState();
  [[NaN, 47.3], [8.5, Infinity], [], [8.5], null, undefined].forEach((pos) => {
    assert.equal(map._recordTrackPoint(pos), false, `rejects ${JSON.stringify(pos)}`);
  });
  assert.equal(map.getTrack().length, 0);
}

function testHoveringDoesNotGrowTheTrack() {
  map._resetTrackState();
  map._recordTrackPoint([8.5, 47.3]);
  // 200 samples of a stationary aircraft. GNSS jitter OSCILLATES around a
  // point rather than drifting away from it, so the samples alternate sign —
  // a monotonic ramp would be real movement and ought to be recorded.
  for (let i = 0; i < 200; i++) {
    const j = (i % 2 ? 1 : -1) * 6e-7;   // ~7 cm, well inside the threshold
    map._recordTrackPoint([8.5 + j, 47.3 + j]);
  }
  assert.equal(map.getTrack().length, 1,
    "a hover must contribute one point, not one per sample");
}

function testSlowDriftIsStillRecorded() {
  // The threshold rejects jitter, not slow flight: a steady crawl past 2 m
  // must still lay down track, or a slow approach would leave a gap.
  map._resetTrackState();
  map._recordTrackPoint([8.5, 47.3]);
  for (let i = 1; i <= 60; i++) map._recordTrackPoint([8.5, 47.3 + i * 5e-6]);  // ~0.55 m/sample
  const n = map.getTrack().length;
  assert.ok(n > 5 && n < 61,
    `slow drift should be decimated but recorded, got ${n} points from 60 samples`);
}

function testMovingGrowsTheTrack() {
  map._resetTrackState();
  map._recordTrackPoint([8.5, 47.3]);
  // ~0.0001 deg of latitude is ~11 m — comfortably past the 2 m threshold.
  for (let i = 1; i <= 5; i++) map._recordTrackPoint([8.5, 47.3 + i * 0.0001]);
  assert.equal(map.getTrack().length, 6, "each real move adds a point");
}

function testTrackThresholdIsAboutTwoMetres() {
  // Just under and just over, on latitude where 1e-5 deg ~ 1.11 m.
  map._resetTrackState();
  map._recordTrackPoint([8.5, 47.3]);
  assert.equal(map._recordTrackPoint([8.5, 47.3 + 0.0000135]), false, "~1.5 m is below the threshold");
  assert.equal(map._recordTrackPoint([8.5, 47.3 + 0.000027]), true, "~3 m is above it");
}

function testClearTrackEmptiesIt() {
  map._resetTrackState();
  map._recordTrackPoint([8.5, 47.3]);
  map._recordTrackPoint([8.5, 47.31]);
  assert.ok(map.getTrack().length >= 2);
  map.clearTrack();
  assert.deepEqual(map.getTrack(), [], "clearTrack discards every point");
}

function testGetTrackReturnsACopy() {
  map._resetTrackState();
  map._recordTrackPoint([8.5, 47.3]);
  const snapshot = map.getTrack();
  snapshot[0][0] = 999;
  snapshot.push([1, 1]);
  assert.equal(map.getTrack()[0][0], 8.5, "caller cannot mutate the live track");
  assert.equal(map.getTrack().length, 1);
}

function testRebootIsDetectedWhenUptimeGoesBackwards() {
  map._resetTrackState();
  // First reading establishes a baseline — it is not itself a reboot, or the
  // track would be wiped the moment the app connects.
  assert.equal(map._checkForReboot(500000), false, "the first uptime seen is not a reboot");
  assert.equal(map._checkForReboot(501000), false, "uptime climbing is normal");
  assert.equal(map._checkForReboot(1200), true, "uptime restarting near zero is a reboot");
  assert.equal(map._checkForReboot(2400), false, "and the new session then climbs normally");
}

function testMissingUptimeIsNotAReboot() {
  // Vehicles that never send SYSTEM_TIME, and the 0 default before the first
  // message, must not be read as a reboot on every single sample.
  map._resetTrackState();
  [undefined, null, 0, -1, "12345", NaN].forEach((v) => {
    assert.equal(map._checkForReboot(v), false, `uptime ${JSON.stringify(v)} is not a reboot`);
  });
}

function testLinkDropDoesNotLookLikeAReboot() {
  // A dropped link stops the samples; when they resume the uptime has moved
  // FORWARD, so the flight in progress keeps its track.
  map._resetTrackState();
  map._checkForReboot(300000);
  assert.equal(map._checkForReboot(345000), false,
    "a gap in telemetry must not discard the track");
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
  testTrackRecordsTheFirstFix,
  testTrackIgnoresNullIsland,
  testTrackIgnoresNonFiniteAndMalformedPositions,
  testHoveringDoesNotGrowTheTrack,
  testSlowDriftIsStillRecorded,
  testMovingGrowsTheTrack,
  testTrackThresholdIsAboutTwoMetres,
  testClearTrackEmptiesIt,
  testGetTrackReturnsACopy,
  testRebootIsDetectedWhenUptimeGoesBackwards,
  testMissingUptimeIsNotAReboot,
  testLinkDropDoesNotLookLikeAReboot,
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
