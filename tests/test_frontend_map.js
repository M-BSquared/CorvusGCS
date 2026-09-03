"use strict";

/**
 * Frontend test for src/js/map.js — the tile-source mirror (TILE).
 *
 * Plain Node-runnable assertions (no browser, no test runner), using the same
 * pattern as tests/test_frontend_link.js: stub the browser globals the module
 * touches at LOAD time, require the source, and assert on the exposed surface.
 * init() is never called, so maplibregl is never needed.
 *
 * The Python registry `corvus/tile_sources.TILE_SOURCES` is the single source of
 * truth for ids+labels+attribution; the frontend `TILE` is a hand-mirrored copy
 * (documented dual-maintenance in map.js). This test asserts the mirror stays
 * in sync: the same 5 ids, each with a non-empty `attribution`, and the
 * `attributionControl: true` flag so MapLibre actually renders the credit.
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

// Load the map module. Defines Corvus.map (with the _TILE test hook).
require("../src/js/map.js");

const map = Corvus.map;
assert.ok(map, "Corvus.map must be defined after requiring map.js");
assert.strictEqual(typeof map._TILE, "function", "Corvus.map must expose the _TILE test hook");

// ---------------------------------------------------------------------------
// The authoritative Python registry, read from disk so this test fails loud if
// the frontend mirror desyncs from it. Kept in sync by the dual-maintenance
// contract documented in map.js.
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

const REGISTRY_IDS = ["satellite", "streets", "hybrid", "topo", "osm"];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

function testTileHasExactlyFiveEntries() {
  const tile = map._TILE();
  assert.ok(tile, "_TILE() must return the tile mirror object");
  const ids = Object.keys(tile);
  assert.strictEqual(ids.length, 5, `TILE must have exactly 5 entries, got ${ids.length}: ${ids}`);
  for (const id of REGISTRY_IDS) {
    assert.ok(id in tile, `TILE missing entry for ${id}`);
  }
}

function testTileLabelsMatchRegistry() {
  const tile = map._TILE();
  for (const id of REGISTRY_IDS) {
    const reg = registryEntry(id);
    assert.ok(reg.label, `registry entry for ${id} has no label (test parse bug?)`);
    assert.strictEqual(
      tile[id].label, reg.label,
      `TILE[${id}].label (${tile[id].label}) != registry (${reg.label})`,
    );
  }
}

function testEveryTileEntryHasNonEmptyAttribution() {
  const tile = map._TILE();
  for (const id of Object.keys(tile)) {
    const attr = tile[id].attribution;
    assert.ok(
      typeof attr === "string" && attr.trim().length > 0,
      `TILE[${id}].attribution must be a non-empty string (got ${JSON.stringify(attr)})`,
    );
  }
}

function testTileAttributionsMatchRegistry() {
  const tile = map._TILE();
  for (const id of REGISTRY_IDS) {
    const reg = registryEntry(id);
    assert.ok(reg.attribution, `registry entry for ${id} has no attribution (test parse bug?)`);
    assert.strictEqual(
      tile[id].attribution, reg.attribution,
      `TILE[${id}].attribution != registry attribution for ${id}`,
    );
  }
}

function testOsmAttributionIsCorrect() {
  const tile = map._TILE();
  assert.strictEqual(
    tile.osm.attribution, "© OpenStreetMap contributors",
    "OSM attribution must be the legally-required credit string",
  );
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

const tests = [
  testTileHasExactlyFiveEntries,
  testTileLabelsMatchRegistry,
  testEveryTileEntryHasNonEmptyAttribution,
  testTileAttributionsMatchRegistry,
  testOsmAttributionIsCorrect,
  testSourceTextEnablesAttributionControl,
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
