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
  // The tile credit is a legal requirement, so it must be on the map one way or
  // the other. The constructor flag is off on purpose — the control is added
  // explicitly so it can be placed bottom-left, out of the control rail's
  // corner — which is exactly the substitution this check has to allow without
  // letting the credit be dropped altogether.
  const src = fs.readFileSync(path.join(__dirname, "..", "src", "js", "map.js"), "utf-8");
  assert.ok(
    src.includes("attributionControl: true")
      || /addControl\(\s*new maplibregl\.AttributionControl\(/.test(src),
    "map.js must render the attribution, via the constructor flag or an explicit control",
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

function testTheCreditIsTheImagerysAndNotTheRenderersToo() {
  // MapLibre's own default options carry a customAttribution linking its
  // homepage, so every map showed the tile credit joined to the word MapLibre
  // by a pipe. Only the first of those is a credit for something ON the map;
  // the renderer is credited in Settings > About > Credits, with its version
  // and its licence, which is where a library belongs and where the BSD-3
  // notice actually has to live.
  const src = fs.readFileSync(path.join(__dirname, "..", "src", "js", "map.js"), "utf-8");
  assert.ok(
    /customAttribution:\s*\[\]/.test(src),
    "map.js must pass an empty customAttribution — MapLibre adds its own credit " +
    "whenever the options object is left out entirely",
  );
  assert.ok(
    /addControl\(new maplibregl\.AttributionControl\(ATTRIBUTION_OPTIONS\)/.test(src),
    "and the control must be built with those options rather than bare",
  );
  // Both maps, one argument: the planner reads this back rather than keeping
  // its own copy of it.
  const missionSrc = fs.readFileSync(
    path.join(__dirname, "..", "src", "js", "mission.js"), "utf-8");
  assert.ok(
    /Corvus\.map\.addAttribution\(map\)/.test(missionSrc),
    "the Mission map must take the credit control from map.js",
  );
  // The imagery credit itself is not suppressible from here and must not be:
  // it is a condition of the tiles being on screen at all, and it arrives on
  // each source's own attribution field.
  assert.ok(
    !/customAttribution:\s*\[[^\]]/.test(src),
    "nothing may be smuggled back into the credit from the frontend",
  );
}

// ---------------------------------------------------------------------------
// Centre-on-vehicle: what counts as somewhere to centre.
//
// The crosshair button used to return silently when the link was down, which
// is indistinguishable from a broken button — and it had no [0,0] guard, so a
// connected vehicle that had not yet acquired a fix flew the map to the Gulf
// of Guinea. realFix is the rule both of those now go through.
// ---------------------------------------------------------------------------

function testRealFixAcceptsAGenuinePosition() {
  assert.deepEqual(map._realFix([11.640969, 48.080217]), [11.640969, 48.080217]);
  // Legitimately near zero on one axis only — Greenwich is not null island.
  assert.deepEqual(map._realFix([0, 48.08]), [0, 48.08]);
  assert.deepEqual(map._realFix([11.64, 0]), [11.64, 0]);
}

function testRealFixRejectsTheNoFixDefault() {
  // [0,0] is the state store's "nothing yet" default, not a place.
  assert.equal(map._realFix([0, 0]), null);
}

function testRealFixRejectsMalformedPositions() {
  [null, undefined, [], [11.64], [NaN, 48], [11.64, Infinity], ["a", "b"]]
    .forEach((pos) => {
      assert.equal(map._realFix(pos), null, `rejects ${JSON.stringify(pos)}`);
    });
}

function testRealFixCoercesNumericStrings() {
  // Positions arrive over JSON; a stringified pair must not be discarded.
  assert.deepEqual(map._realFix(["11.64", "48.08"]), [11.64, 48.08]);
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

// ---------------------------------------------------------------------------
// The home marker.
//
// "H" in a ring is a promise about where the aircraft will come back to, so it
// has to be the aircraft's own answer or nothing. The marker is created at
// DEFAULT_CENTER, which is where the map OPENS, not where anything is — and it
// used to be shown from that moment, and its update guard tested only the
// longitude, so a cleared home left the last one on screen for good.
// ---------------------------------------------------------------------------
function fakeMarker() {
  const el = { hidden: false };
  return {
    el,
    lngLat: null,
    getElement() { return el; },
    setLngLat(v) { this.lngLat = v; return this; },
  };
}

function testHomeMarkerHiddenBeforeAnyHomeIsReported() {
  const m = fakeMarker();
  map._updateHome({ home: [0, 0] }, m);
  assert.equal(m.el.hidden, true, "[0,0] is 'nothing yet', not a place");
  assert.equal(m.lngLat, null, "and nothing is placed there");
}

function testHomeMarkerHiddenWithoutTelemetryAtAll() {
  const m = fakeMarker();
  map._updateHome(null, m);
  assert.equal(m.el.hidden, true);
  map._updateHome({}, m);
  assert.equal(m.el.hidden, true);
}

function testHomeMarkerAppearsOnAReportedHome() {
  const m = fakeMarker();
  m.el.hidden = true;
  map._updateHome({ home: [11.6405678, 48.0812345] }, m);
  assert.equal(m.el.hidden, false);
  assert.deepStrictEqual(m.lngLat, [11.6405678, 48.0812345]);
}

function testHomeMarkerGoesAwayWhenHomeIsCleared() {
  // The bridge clears home on every connect, so the previous session's launch
  // point cannot be drawn as this one's. That is only true if the marker
  // actually follows it back to nothing.
  const m = fakeMarker();
  map._updateHome({ home: [11.64, 48.08] }, m);
  assert.equal(m.el.hidden, false);
  map._updateHome({ home: [0, 0] }, m);
  assert.equal(m.el.hidden, true, "a cleared home must not leave its marker");
}

function testHomeMarkerIgnoresAMalformedHome() {
  const m = fakeMarker();
  [{ home: [NaN, 48] }, { home: ["x", "y"] }, { home: [11.6] }].forEach((state) => {
    m.el.hidden = false;
    map._updateHome(state, m);
    assert.equal(m.el.hidden, true, `malformed home: ${JSON.stringify(state.home)}`);
  });
}

// ---------------------------------------------------------------------------
// The view the two maps share.
// ---------------------------------------------------------------------------
// One piece of ground, two maps over it. The rule is that the Mission planner
// opens on the view the operator last AIMED, on whichever of the two they
// aimed it — and that "aimed" means a hand on the map, never the aircraft
// dragging the Home camera along behind it.
//
// Pure bookkeeping, so it is assertable with no MapLibre in the room: without
// a Home map, missionOpenView() can only ever answer from what the planner
// reported, which is exactly the half this checks.

function withFreshSharedView(body) {
  Corvus.map._resetSharedView();
  try { body(); } finally { Corvus.map._resetSharedView(); }
}

function testAPlannerThatWasNeverAimedHasNoViewOfItsOwn() {
  withFreshSharedView(() => {
    // No Home map in Node, and nothing reported by the planner, so there is
    // nothing to open on — the planner falls back to the aircraft itself.
    assert.equal(Corvus.map.missionOpenView(), null);
  });
}

function testThePlannersOwnAimIsWhatItReopensOn() {
  withFreshSharedView(() => {
    Corvus.map.noteMissionView({ center: [11.5, 48.7], zoom: 16.5, bearing: 30 });
    assert.deepEqual(Corvus.map.missionOpenView(),
                     { center: [11.5, 48.7], zoom: 16.5, bearing: 30 });
  });
}

function testAimingTheHomeMapAfterwardsWinsBack() {
  withFreshSharedView(() => {
    Corvus.map.noteMissionView({ center: [11.5, 48.7], zoom: 16, bearing: 0 });
    Corvus.map._noteHomeAim();
    // The operator moved the Home map more recently, so that is the view they
    // last chose — and with no Home map in Node there is nothing to read it
    // from, which is the honest answer rather than a stale one.
    assert.equal(Corvus.map.missionOpenView(), null);
  });
}

function testTheReportedViewIsACopyNotTheStore() {
  withFreshSharedView(() => {
    const centre = [11.5, 48.7];
    Corvus.map.noteMissionView({ center: centre, zoom: 16, bearing: 0 });
    centre[0] = 0;
    assert.equal(Corvus.map.missionOpenView().center[0], 11.5);
    const read = Corvus.map.missionOpenView();
    read.center[1] = 0;
    assert.equal(Corvus.map.missionOpenView().center[1], 48.7);
  });
}

function testAnUnusableViewIsIgnoredRatherThanStored() {
  withFreshSharedView(() => {
    [
      null,
      {},
      { center: [11.5], zoom: 16 },
      { center: ["x", "y"], zoom: 16 },
      { center: [11.5, 48.7] },
      { center: [11.5, 48.7], zoom: NaN },
    ].forEach((bad) => {
      Corvus.map.noteMissionView(bad);
      assert.equal(Corvus.map.missionOpenView(), null,
        `${JSON.stringify(bad)} must not become the view the planner opens on`);
    });
  });
}

function testAMissingBearingIsFlatNotBroken() {
  withFreshSharedView(() => {
    Corvus.map.noteMissionView({ center: [11.5, 48.7], zoom: 14 });
    assert.equal(Corvus.map.missionOpenView().bearing, 0);
  });
}

// ---------------------------------------------------------------------------
// The flight bar's narrowing rule (setFlightBarShrink / _flightBarRoom).
// ---------------------------------------------------------------------------

function testTheFlightBarIsMeasuredAgainstAShareOfTheMap() {
  // What the switch does when it is on: half the map column, so the same two
  // steps fitBar always applies are reached before the row runs out of room.
  try {
    Corvus.map.setFlightBarShrink(true);
    assert.equal(Corvus.map._flightBarRoom(1000), 500);
    assert.equal(Corvus.map._flightBarRoom(640), 320);
    // A map column that measures nothing — a page not on screen, a map with no
    // parent — is NOT a room of zero: that would pin the bar at icons forever.
    assert.equal(Corvus.map._flightBarRoom(0), null);
  } finally {
    Corvus.map.setFlightBarShrink(false);
  }
}

function testTheFlightBarIsFullSizeUntilTheSwitchIsThrown() {
  // The default, and the whole point of it: the room is .map-topleft's — the
  // box beside the map's controls — so the buttons keep their 72px rhythm and
  // their captions until the row will not fit THAT. It is the planner's rule
  // measured against the planner's kind of box. The share is what the Settings
  // switch adds on top.
  assert.equal(Corvus.map._flightBarRoom(1000, 830), 830,
    "a bar nobody asked to shrink is measured against the room it sits in");
  try {
    Corvus.map.setFlightBarShrink(true);
    assert.equal(Corvus.map._flightBarRoom(1000, 830), 500,
      "the switch measures the MAP instead, so the steps come earlier");
    Corvus.map.setFlightBarShrink(false);
    assert.equal(Corvus.map._flightBarRoom(1000, 830), 830);
  } finally {
    Corvus.map.setFlightBarShrink(false);
  }
  // A room that measures nothing — a page off screen — is null, not 0: fitBar
  // then leaves a bar nobody can measure exactly as it is, rather than pinning
  // it at icons forever.
  assert.equal(Corvus.map._flightBarRoom(0, 0), null);
}

function testTheFlightBarShrinkIsOffWhenTheConfigIsSilent() {
  // Read as "is true" in both places that read it, because it is OPT-IN: a
  // config that has never been asked leaves the bar at the size it is meant
  // to be, and only an explicit true trades its labels for map.
  const appJs = fs.readFileSync(
    path.join(__dirname, "..", "src", "js", "app.js"), "utf-8");
  assert.ok(/!!\(cfg\.ui && cfg\.ui\.flight_bar_shrink\)/.test(appJs),
    "app.js must apply the config's answer as \"is true\"");
  assert.ok(!/flight_bar_shrink === false/.test(appJs),
    "and must not read it as \"not false\" — that would shrink an unasked bar");
  const sidenavJs = fs.readFileSync(
    path.join(__dirname, "..", "src", "js", "sidenav.js"), "utf-8");
  assert.ok(/!!\(\(cfg\.ui \|\| \{\}\)\.flight_bar_shrink\)/.test(sidenavJs),
    "the Settings switch must render from the same reading");
  // And the switch must drive the live bar as well as the config, or the
  // operator throws it and watches nothing happen until a reload.
  assert.ok(/Corvus\.map\.setFlightBarShrink\(next\)/.test(sidenavJs),
    "the Settings switch must apply to the live bar immediately");
  assert.ok(/postConfig\(\{ ui: \{ flight_bar_shrink: next \} \}/.test(sidenavJs),
    "and persist the one key it owns");
}

// ---------------------------------------------------------------------------
// The credit toggle. It starts folded on every map, only a click opens it, and
// MapLibre's own opens are undone. The old guard closed real clicks as well,
// because the click reached it through two writers, and the planner had no
// guard at all, so its credit opened by itself.
// ---------------------------------------------------------------------------

function fakeAttrib() {
  const classes = new Set(["maplibregl-ctrl", "maplibregl-ctrl-attrib", "maplibregl-compact"]);
  const attrs = new Map();
  const listeners = [];
  const observers = [];
  const notify = () => observers.forEach((cb) => cb());
  const el = {
    classList: {
      contains: (c) => classes.has(c),
      add: (...cs) => { cs.forEach((c) => classes.add(c)); notify(); },
      remove: (...cs) => { cs.forEach((c) => classes.delete(c)); notify(); },
      toggle: (c, on) => { if (on) classes.add(c); else classes.delete(c); notify(); },
    },
    hasAttribute: (n) => attrs.has(n),
    setAttribute: (n, v) => { attrs.set(n, v); notify(); },
    removeAttribute: (n) => { attrs.delete(n); notify(); },
    addEventListener: (type, cb, capture) => listeners.push({ type, cb, capture }),
    click(onButton) {
      let prevented = false;
      let stopped = false;
      const e = {
        target: { closest: (sel) => (onButton && sel === ".maplibregl-ctrl-attrib-button" ? {} : null) },
        preventDefault: () => { prevented = true; },
        stopImmediatePropagation: () => { stopped = true; },
      };
      listeners.filter((l) => l.type === "click").forEach((l) => l.cb(e));
      return { prevented, stopped };
    },
    get shown() { return classes.has("maplibregl-compact-show") || attrs.has("open"); },
    get openAttr() { return attrs.has("open"); },
    get showClass() { return classes.has("maplibregl-compact-show"); },
  };
  return { el, observers };
}

function withAttrib(fn) {
  const saved = global.MutationObserver;
  const { el, observers } = fakeAttrib();
  let depth = 0;
  global.MutationObserver = class {
    constructor(cb) { this.cb = cb; }
    observe() {
      observers.push(() => {
        // A real observer is batched; a bounded re-entry is close enough and
        // still catches an observer that keeps writing.
        if (depth > 5) throw new Error("the observer never settles");
        depth++; try { this.cb([]); } finally { depth--; }
      });
    }
  };
  try { map._ownAttributionToggle(el); fn(el); } finally { global.MutationObserver = saved; }
}

function testTheCreditStartsFoldedEvenAfterMapLibreOpensIt() {
  withAttrib((el) => {
    assert.equal(el.shown, false, "folded at start");
    // What MapLibre does when the source's attribution text arrives.
    el.setAttribute("open", "");
    el.classList.add("maplibregl-compact", "maplibregl-compact-show");
    assert.equal(el.shown, false, "an open MapLibre makes on its own is undone");
  });
}

function testAClickOpensTheCreditAndItStaysOpen() {
  withAttrib((el) => {
    const r = el.click(true);
    assert.ok(r.prevented, "the native <summary> toggle must not run as a second writer");
    assert.ok(r.stopped, "MapLibre's own toggle must not run as a second writer");
    assert.ok(el.showClass && el.openAttr, "a click on the icon opens the credit");
    el.click(true);
    assert.equal(el.shown, false, "a second click folds it again");
  });
}

function testTheFoldedDiscIsTheButtonButTheOpenCreditsLinksAreNot() {
  withAttrib((el) => {
    el.click(false);
    assert.ok(el.shown, "folded, a click anywhere on the disc opens it");
    const r = el.click(false);
    assert.ok(el.shown && !r.prevented, "open, a click on the credit text is left alone");
  });
}

function testAFoldMapLibreMakesIsAccepted() {
  withAttrib((el) => {
    el.click(true);
    el.classList.remove("maplibregl-compact-show");  // MapLibre folds it on drag
    assert.equal(el.shown, false, "the credit folds and stays folded");
    el.click(true);
    assert.ok(el.shown, "and the next click opens it, not closes it");
  });
}

const tests = [
  testTheFlightBarIsFullSizeUntilTheSwitchIsThrown,
  testTheFlightBarIsMeasuredAgainstAShareOfTheMap,
  testTheFlightBarShrinkIsOffWhenTheConfigIsSilent,
  testAPlannerThatWasNeverAimedHasNoViewOfItsOwn,
  testThePlannersOwnAimIsWhatItReopensOn,
  testAimingTheHomeMapAfterwardsWinsBack,
  testTheReportedViewIsACopyNotTheStore,
  testAnUnusableViewIsIgnoredRatherThanStored,
  testAMissingBearingIsFlatNotBroken,
  testHomeMarkerHiddenBeforeAnyHomeIsReported,
  testHomeMarkerHiddenWithoutTelemetryAtAll,
  testHomeMarkerAppearsOnAReportedHome,
  testHomeMarkerGoesAwayWhenHomeIsCleared,
  testHomeMarkerIgnoresAMalformedHome,
  testCatalogueStartsWithOnlyTheBootstrapEntry,
  testBootstrapEntryMatchesTheRegistry,
  testBootstrapEntryHasNonEmptyAttribution,
  testSetBaseLayerRejectsUnknownSources,
  testSourceTextDoesNotMirrorTheRegistry,
  testSourceTextEnablesAttributionControl,
  testTheCreditIsTheImagerysAndNotTheRenderersToo,
  testRealFixAcceptsAGenuinePosition,
  testRealFixRejectsTheNoFixDefault,
  testRealFixRejectsMalformedPositions,
  testRealFixCoercesNumericStrings,
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
  testTheCreditStartsFoldedEvenAfterMapLibreOpensIt,
  testAClickOpensTheCreditAndItStaysOpen,
  testTheFoldedDiscIsTheButtonButTheOpenCreditsLinksAreNot,
  testAFoldMapLibreMakesIsAccepted,
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
