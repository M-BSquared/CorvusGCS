"use strict";

/**
 * Frontend test for 3D terrain wiring in src/js/map.js: which of the terrain,
 * the shading, the globe and the buildings are in the style for each mode
 * and zoom, and that turning 3D off leaves none of them behind.
 *
 * The rules pinned here are the ones that made the terrain look wrong:
 *
 *   * the relief is part of EVERY 3D mode, not only the one with buildings:
 *     a tilted map over flat ground is the picture that makes an operator
 *     misjudge a ridge;
 *   * the relief stays on under the globe. It used to be switched off below
 *     zoom 12, which is exactly where whole valleys fit on screen;
 *   * the terrain carries a hillshade from its own source, under every
 *     overlay.
 *
 * Driven through a stateful fake maplibregl map, the way
 * tests/test_frontend_map_follow.js drives follow mode.
 *
 * Run:
 *   node tests/test_frontend_map_terrain.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
global.Event = class Event { constructor(type) { this.type = type; } };

let rafId = 0;
window.requestAnimationFrame = () => ++rafId;
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
    tabIndex: 0, _attrs: {}, _listeners: {}, _isEl: true,
    parentNode: null, parentElement: null,
    _box: { width: 1000, height: 700 },
  };
  e.style = {
    _props: {},
    setProperty(k, v) { e.style._props[k] = String(v); },
    getPropertyValue(k) { return e.style._props[k] || ""; },
  };
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) {
      const has = e.classList.contains(c);
      const next = force === undefined ? !has : !!force;
      if (next) e.classList.add(c); else e.classList.remove(c);
      return next;
    },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => {
    if (c.parentNode && c.parentNode !== e) c.parentNode.removeChild(c);
    c.parentNode = e; c.parentElement = e;
    e.children.push(c);
    return c;
  };
  e.append = (...nodes) => { nodes.forEach((n) => e.appendChild(n)); };
  e.insertBefore = (c) => e.appendChild(c);
  e.removeChild = (c) => {
    const i = e.children.indexOf(c);
    if (i >= 0) e.children.splice(i, 1);
    c.parentNode = null; c.parentElement = null;
    return c;
  };
  e.remove = () => { if (e.parentNode) e.parentNode.removeChild(e); };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.removeEventListener = () => {};
  e.focus = () => {};
  e.contains = () => false;
  e.getBoundingClientRect = () => ({ left: 0, top: 0, right: 1000, bottom: 700, width: 1000, height: 700 });
  e.closest = () => null;
  e.querySelectorAll = (sel) => query(e.children, sel);
  e.querySelector = (sel) => query(e.children, sel)[0] || null;
  Object.defineProperty(e, "offsetWidth", { get: () => e._box.width });
  Object.defineProperty(e, "offsetHeight", { get: () => e._box.height });
  Object.defineProperty(e, "clientWidth", { get: () => e._box.width });
  Object.defineProperty(e, "clientHeight", { get: () => e._box.height });
  Object.defineProperty(e, "firstChild", { get: () => e.children[0] || null });
  return e;
}

function query(children, sel) {
  const out = [];
  const attr = sel.match(/\[data-act="([^"]+)"\]/);
  const base = sel.replace(/\[data-act="[^"]+"\]/, "");
  const wantTag = base && base[0] !== ".";
  const classes = base.split(".").filter(Boolean);
  (function walk(list) {
    list.forEach((c) => {
      if (!c || !c._isEl) return;
      const own = c.className.split(/\s+/).filter(Boolean);
      let ok = base ? (wantTag ? c.tagName === base.toUpperCase()
        : classes.every((cl) => own.includes(cl))) : true;
      if (ok && attr && c.dataset.act !== attr[1]) ok = false;
      if (ok) out.push(c);
      walk(c.children);
    });
  })(children);
  return out;
}

global.document = {
  createElement: makeEl,
  createElementNS: makeEl,
  getElementById: () => makeEl("div"),
  querySelector: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
  documentElement: makeEl("html"),
  body: makeEl("body"),
};
global.getComputedStyle = () => ({ getPropertyValue: () => "" });

// ---------------------------------------------------------------------------
// A fake map that keeps the style: sources, layers (in order), terrain,
// projection. Enough of MapLibre's surface for 3D mode to build and tear
// down what it builds.
// ---------------------------------------------------------------------------
const mapEvents = {};
const style = { sources: new Map(), layers: [], terrain: null, projection: "mercator" };
let zoom = 13;
let pitch = 0;
// Where the view is. Moved by a test that needs building cells no earlier
// test has already filled.
let centre = [11.0, 47.4];

const mapEl = makeEl("div");

const fakeMap = {
  on: (type, cb) => { (mapEvents[type] = mapEvents[type] || []).push(cb); },
  off: (type, cb) => {
    const list = mapEvents[type] || [];
    const i = list.indexOf(cb);
    if (i >= 0) list.splice(i, 1);
  },
  once() {},
  addControl() {},
  addSource(id, spec) {
    if (style.sources.has(id)) throw new Error(`source ${id} already exists`);
    style.sources.set(id, spec);
  },
  removeSource(id) {
    if (style.terrain && style.terrain.source === id) {
      throw new Error(`source ${id} is used by the terrain`);
    }
    if (style.layers.some((l) => l.source === id)) {
      throw new Error(`source ${id} is used by a layer`);
    }
    style.sources.delete(id);
  },
  getSource: (id) => (style.sources.has(id)
    ? { id, setData(data) { painted[id] = data; } } : undefined),
  addLayer(layer, before) {
    const at = before ? style.layers.findIndex((l) => l.id === before) : -1;
    if (at >= 0) style.layers.splice(at, 0, layer); else style.layers.push(layer);
  },
  removeLayer(id) { style.layers = style.layers.filter((l) => l.id !== id); },
  getLayer: (id) => style.layers.find((l) => l.id === id),
  setPaintProperty() {},
  setTerrain(spec) {
    if (spec && !style.sources.has(spec.source)) throw new Error("no such source");
    style.terrain = spec || null;
  },
  getTerrain: () => style.terrain,
  setProjection(p) { style.projection = p.type; },
  getProjection: () => ({ type: style.projection }),
  setSky() {},
  getCanvas: () => makeEl("canvas"),
  getCanvasContainer: () => makeEl("div"),
  getContainer: () => mapEl,
  getCenter: () => ({ lng: centre[0], lat: centre[1] }),
  getBearing: () => 0,
  getPitch: () => pitch,
  getZoom: () => zoom,
  getMaxPitch: () => 85,
  isMoving: () => false,
  isEasing: () => false,
  zoomIn() {}, zoomOut() {}, resize() {}, setPixelRatio() {}, triggerRepaint() {},
  queryTerrainElevation: () => (style.terrain ? 1500 : null),
  easeTo(opts) { if (opts && opts.pitch !== undefined) pitch = opts.pitch; },
  jumpTo(opts) {
    if (opts && opts.pitch !== undefined) pitch = opts.pitch;
    if (opts && opts.zoom !== undefined) zoom = opts.zoom;
  },
  project: () => ({ x: 500, y: 350 }),
  // A small view around the centre: one or two building cells.
  getBounds: () => ({
    getWest: () => centre[0] - 0.001, getSouth: () => centre[1] - 0.001,
    getEast: () => centre[0] + 0.001, getNorth: () => centre[1] + 0.001,
  }),
};
const painted = {};

global.maplibregl = {
  Map: function () { return fakeMap; },
  AttributionControl: function () { return {}; },
  Marker: function () {
    return {
      setLngLat() { return this; }, addTo() { return this; }, remove() {},
      setRotation() { return this; }, getElement: () => makeEl("div"),
    };
  },
  MercatorCoordinate: { fromLngLat: () => ({ x: 0.5, y: 0.5, z: 0 }) },
};

// What the backend answers for a building cell; tests swap it.
let buildingAnswer = () => Promise.reject(new Error("offline"));
const buildingAsks = [];
Corvus.telemetry = {
  requestJson: (url) => {
    if (String(url).startsWith("/api/buildings/")) {
      buildingAsks.push(url);
      return buildingAnswer(url);
    }
    return Promise.reject(new Error("offline"));
  },
  postAction: () => Promise.resolve({}),
  getState: () => ({ connected: false, position: [0, 0], heading: 0 }),
  subscribe() { return () => {}; },
};
Corvus.units = { formatLength: (m) => `${m} m` };

require("../src/js/ui.js");
require("../src/js/anim.js");
require("../src/js/map-overlays.js");
require("../src/js/map.js");

const map = Corvus.map;
map.init(mapEl, makeEl("div"), makeEl("div"));
(mapEvents.load || []).forEach((cb) => cb());
assert.equal(map.isReady(), true, "the fake map must reach the loaded state");

const DEM = { id: "terrain", encoding: "terrarium", maxzoom: 15, attribution: "test" };

/** Let the elevation probe (a resolved promise without an Image) run out. */
const settle = () => new Promise((resolve) => setImmediate(resolve));

const layerIds = () => style.layers.map((l) => l.id);
const hasLayer = (id) => layerIds().includes(id);

let passed = 0;
async function check(name, fn) {
  await fn();
  passed++;
  console.log("  ok  " + name);
}

async function reset() {
  map.set3DMode("off");
  await settle();
  zoom = 13;
}

(async () => {
  console.log("map.js — 3D terrain wiring");
  map._setTerrainSpec(DEM);

  await check("the terrain-only 3D still has the relief, shaded", async () => {
    await reset();
    map.set3DMode("simple");
    await settle();
    assert.equal(map.hasTerrain(), true, "3D without buildings is not 3D without terrain");
    assert.deepEqual(style.terrain, { source: "terrain", exaggeration: 1 });
    assert.ok(hasLayer("terrain-hillshade"), "the relief is shaded");
    assert.ok(!hasLayer("buildings-3d"), "no buildings were asked for");
  });

  await check("the shading has its own source, never the terrain's", async () => {
    const shade = style.layers.find((l) => l.id === "terrain-hillshade");
    assert.notEqual(shade.source, "terrain",
      "MapLibre renders the mesh from a coarser DEM and warns when one source serves both");
    const spec = style.sources.get(shade.source);
    assert.equal(spec.type, "raster-dem");
    assert.equal(spec.tileSize, 256, "terrarium tiles are 256 px; 512 would halve every height");
    assert.equal(spec.encoding, "terrarium");
  });

  await check("the shading sits under every overlay", async () => {
    const at = (id) => layerIds().indexOf(id);
    assert.ok(at("terrain-hillshade") >= 0);
    for (const id of ["offline-regions-fill", "path-glow", "path-line"]) {
      if (at(id) >= 0) assert.ok(at("terrain-hillshade") < at(id), `${id} is above the shading`);
    }
  });

  await check("full 3D adds the buildings on top of the same terrain", async () => {
    map.set3DMode("full");
    await settle();
    assert.equal(map.hasTerrain(), true);
    assert.ok(hasLayer("buildings-3d"));
    assert.ok(hasLayer("terrain-hillshade"));
  });

  await check("going back to terrain only takes the buildings, not the ground", async () => {
    map.set3DMode("simple");
    await settle();
    assert.ok(!hasLayer("buildings-3d"));
    assert.equal(map.hasTerrain(), true);
  });

  await check("zoomed out, the globe shows WITH the relief on it", async () => {
    await reset();
    zoom = 9;
    map.set3DMode("full");
    await settle();
    assert.equal(style.projection, "globe");
    assert.equal(map.hasTerrain(), true,
      "zoom 8 to 11 is where whole valleys fit on screen; a smooth ball there is the bug");
    assert.ok(style.terrain);
  });

  await check("zooming in across the handover keeps the terrain attached", async () => {
    const before = style.terrain;
    zoom = 14;
    (mapEvents.moveend || []).forEach((cb) => cb({}));
    await settle();
    assert.equal(style.projection, "mercator");
    assert.equal(style.terrain, before, "the projection changed, the ground did not");
    zoom = 9;
    (mapEvents.moveend || []).forEach((cb) => cb({}));
    await settle();
    assert.equal(style.projection, "globe");
    assert.equal(style.terrain, before);
  });

  await check("the terrain-only mode gets the globe too", async () => {
    await reset();
    zoom = 6;
    map.set3DMode("simple");
    await settle();
    assert.equal(style.projection, "globe");
    assert.equal(map.hasTerrain(), true);
  });

  await check("3D off leaves no terrain, shading, source or globe behind", async () => {
    map.set3DMode("full");
    await settle();
    await reset();
    assert.equal(map.hasTerrain(), false);
    assert.equal(style.terrain, null);
    assert.equal(style.projection, "mercator");
    assert.ok(!hasLayer("terrain-hillshade"));
    assert.ok(!hasLayer("buildings-3d"));
    assert.ok(!style.sources.has("terrain"));
    assert.ok(![...style.sources.keys()].some((id) => id.startsWith("terrain")),
      "the shading's source goes too");
  });

  await check("pressing 3D twice builds the same terrain as pressing it once", async () => {
    map.set3DMode("simple");
    await settle();
    await reset();
    map.set3DMode("simple");
    await settle();
    assert.equal(map.hasTerrain(), true);
    assert.equal(layerIds().filter((id) => id === "terrain-hillshade").length, 1);
  });

  const FREE = { id: "terrain", provider: "terrain", encoding: "terrarium", tile_size: 256,
    maxzoom: 15, token_required: false, token_set: false };
  const MAPTILER = { id: "maptiler_terrain", provider: "maptiler", encoding: "mapbox",
    tile_size: 512, maxzoom: 14, token_required: true, token_set: true };
  const MAPBOX = { id: "mapbox_terrain", provider: "mapbox", encoding: "mapbox",
    tile_size: 256, maxzoom: 15, token_required: true, token_set: false };
  const CATALOGUE = [FREE, MAPTILER, MAPBOX];

  await check("a map service with its own elevation and a key gets that elevation", async () => {
    assert.equal(map._terrainFor(CATALOGUE, "maptiler", "terrain"), MAPTILER);
  });

  await check("without the key, or without a model of its own, it is the free one", async () => {
    assert.equal(map._terrainFor(CATALOGUE, "mapbox", "terrain"), FREE,
      "a keyed DEM without its key is a 401, not terrain");
    assert.equal(map._terrainFor(CATALOGUE, "google", "terrain"), FREE,
      "Google publishes no elevation tiles");
    assert.equal(map._terrainFor(CATALOGUE, "esri", "terrain"), FREE);
    assert.equal(map._terrainFor(CATALOGUE, null, "terrain"), FREE);
    assert.equal(map._terrainFor([], "maptiler", "terrain"), null);
  });

  await check("a 512 px model is declared at 512, to both of its sources", async () => {
    await reset();
    map._setTerrainSpec(MAPTILER);
    map.set3DMode("simple");
    await settle();
    assert.equal(style.sources.get("maptiler_terrain").tileSize, 512,
      "declared at 256 every height would be doubled");
    assert.equal(style.sources.get("maptiler_terrain").encoding, "mapbox");
    assert.equal(style.sources.get("maptiler_terrain-shade").tileSize, 512);
    await reset();
    assert.ok(![...style.sources.keys()].some((id) => id.includes("terrain")),
      "the keyed model's sources go too");
    map._setTerrainSpec(DEM);
  });

  await check("automatic prefers Copernicus where the map service has no model of its own",
    async () => {
      const COPERNICUS = { id: "copernicus", provider: "copernicus", encoding: "terrarium",
        tile_size: 512, maxzoom: 17, token_required: false, token_set: false };
      const all = [FREE, COPERNICUS, MAPTILER, MAPBOX];
      assert.equal(map._terrainFor(all, "google", "terrain", "auto", "copernicus"), COPERNICUS);
      assert.equal(map._terrainFor(all, "maptiler", "terrain", "auto", "copernicus"), MAPTILER,
        "a keyed service's own model still wins, its imagery was made to sit on it");
      assert.equal(map._terrainFor(all, "maptiler", "terrain", "terrain", "copernicus"), FREE,
        "the operator's pick wins over the rule");
      assert.equal(map._terrainFor(all, "esri", "terrain", "mapbox_terrain", "copernicus"),
        COPERNICUS, "a pick that cannot load (no key) falls back to the rule, not to flat");
      assert.equal(map._terrainFor(all, "esri", "terrain", "nonsense", "copernicus"), COPERNICUS);
    });

  // ---- buildings: a slow or absent cell is asked for again, never given up on ----
  const realSetTimeout = window.setTimeout;
  let timers = [];
  const useManualTimers = () => { timers = []; window.setTimeout = (fn) => { timers.push(fn); return timers.length; }; };
  const restoreTimers = () => { window.setTimeout = realSetTimeout; };
  const runTimers = async (rounds) => {
    for (let i = 0; i < rounds && timers.length; i++) {
      const now = timers;
      timers = [];
      now.forEach((fn) => fn());
      await settle();
    }
  };
  const view = () => { (mapEvents.moveend || []).forEach((cb) => cb({})); };

  await check("offline, a cell is asked for again on the next view, not polled or dropped",
    async () => {
      await reset();
      zoom = 16;
      map.set3DMode("full");
      await settle();
      useManualTimers();
      try {
        buildingAnswer = () => Promise.resolve({ features: [], unavailable: true });
        buildingAsks.length = 0;
        view();
        await runTimers(3);
        const first = buildingAsks.length;
        assert.ok(first > 0, "the view asked for its cells");
        await runTimers(5);
        assert.equal(buildingAsks.length, first, "nothing is coming, so nothing is polled");
        view();
        await runTimers(3);
        assert.equal(buildingAsks.length, 2 * first, "the next view asks again");
      } finally {
        restoreTimers();
      }
    });

  await check("a cell that outlasts the retries is picked up by the next view", async () => {
    await reset();
    zoom = 16;
    map.set3DMode("full");
    await settle();
    useManualTimers();
    try {
      buildingAnswer = () => Promise.resolve({ features: [], pending: true });
      buildingAsks.length = 0;
      view();
      await runTimers(60);
      const polled = buildingAsks.length;
      assert.ok(polled > 1, "a pending cell is polled");
      // The backend has it now. Before, the map had kept an empty answer for
      // the cell and a new view never asked: no buildings until a restart.
      const building = { type: "Feature", properties: { height: 9, min_height: 0 },
        geometry: { type: "Polygon", coordinates: [[[11, 47.4], [11.0001, 47.4],
          [11.0001, 47.4001], [11, 47.4]]] } };
      buildingAnswer = () => Promise.resolve({ features: [building] });
      view();
      await runTimers(5);
      assert.ok(buildingAsks.length > polled, "the next view asked again");
      const drawn = painted.buildings && painted.buildings.features;
      assert.ok(drawn && drawn.length >= 1, "and the building is drawn");
    } finally {
      restoreTimers();
      buildingAnswer = () => Promise.reject(new Error("offline"));
    }
  });

  await check("a cell still pending when 3D is left is asked for again when it comes back",
    async () => {
      await reset();
      zoom = 16;
      centre = [11.3, 47.6];
      map.set3DMode("full");
      await settle();
      useManualTimers();
      try {
        buildingAnswer = () => Promise.resolve({ features: [], pending: true });
        buildingAsks.length = 0;
        view();
        await runTimers(2);
        const asked = buildingAsks.length;
        assert.ok(asked > 0, "the view asked for its cells");
        // The operator leaves 3D while Overpass is still busy; the retry
        // fires into a map that no longer wants the cell. It used to keep
        // its empty claim, so coming back never asked again.
        map.set3DMode("off");
        await runTimers(3);
        const building = { type: "Feature", properties: { height: 9, min_height: 0 },
          geometry: { type: "Polygon", coordinates: [[[11.3, 47.6], [11.3001, 47.6],
            [11.3001, 47.6001], [11.3, 47.6]]] } };
        buildingAnswer = () => Promise.resolve({ features: [building] });
        map.set3DMode("full");
        await settle();
        view();
        await runTimers(3);
        assert.ok(buildingAsks.length > asked, "the cells are asked for again");
        const drawn = painted.buildings && painted.buildings.features;
        assert.ok(drawn && drawn.length >= 1, "and the building is drawn");
      } finally {
        restoreTimers();
        centre = [11.0, 47.4];
        buildingAnswer = () => Promise.reject(new Error("offline"));
      }
    });

  await check("without a DEM, 3D is a tilt over flat ground, not an error", async () => {
    await reset();
    map._setTerrainSpec(null);
    map.set3DMode("full");
    await settle();
    assert.equal(map.hasTerrain(), false);
    assert.equal(style.terrain, null);
    assert.ok(!hasLayer("terrain-hillshade"));
    assert.equal(pitch, 60, "the tilt does not wait for a terrain that is not coming");
    await reset();
    map._setTerrainSpec(DEM);
  });

  // ---- the camera's ground height: Corvus holds it, smoothed ----
  // The fake rAF never runs its callback; these tests step the glide by hand.
  const frames = [];
  const withFrames = async (fn) => {
    const realRaf = window.requestAnimationFrame;
    window.requestAnimationFrame = (cb) => { frames.push(cb); return frames.length; };
    try { await fn(); } finally { window.requestAnimationFrame = realRaf; frames.length = 0; }
  };
  const runFrames = (n) => {
    for (let i = 0; i < n && frames.length; i++) frames.shift()();
  };
  const withCamera = async (start, ground, fn) => {
    const writes = [];
    let held = start;
    const realQuery = fakeMap.queryTerrainElevation;
    fakeMap.getCenterElevation = () => held;
    fakeMap.setCenterElevation = (h) => { writes.push(h); held = h; };
    fakeMap.queryTerrainElevation = (at) => (style.terrain ? ground(at) : null);
    try {
      await fn(writes, () => held);
    } finally {
      fakeMap.isMoving = () => false;
      fakeMap.queryTerrainElevation = realQuery;
      delete fakeMap.getCenterElevation;
      delete fakeMap.setCenterElevation;
    }
  };

  await check("with terrain on, the camera's height is Corvus's, not MapLibre's", async () => {
    await reset();
    const clamps = [];
    fakeMap.setCenterClampedToGround = (on) => { clamps.push(on); };
    try {
      map.set3DMode("simple");
      await settle();
      assert.equal(clamps[clamps.length - 1], false,
        "MapLibre's clamp reads one DEM pixel, which over a surface model is a roof");
      await reset();
      assert.equal(clamps[clamps.length - 1], true, "and it gets it back when 3D goes off");
    } finally {
      delete fakeMap.setCenterClampedToGround;
    }
  });

  await check("a follow ease leaves the ground height alone over terrain", async () => {
    await reset();
    const eases = [];
    const realEase = fakeMap.easeTo;
    const realState = Corvus.telemetry.getState;
    fakeMap.easeTo = (opts) => { eases.push(opts); realEase(opts); };
    Corvus.telemetry.getState = () => ({ connected: true, position: [11.01, 47.41], heading: 0 });
    try {
      map.set3DMode("simple");
      await settle();
      eases.length = 0;
      map.centerOnVehicle(true);
      assert.ok(eases.length > 0 && eases.every((o) => o.freezeElevation === true),
        "unfrozen, MapLibre lands every pan on the pixel under the target");
      await reset();
      eases.length = 0;
      map.centerOnVehicle(true);
      assert.ok(eases.length > 0 && eases.every((o) => !o.freezeElevation),
        "without terrain there is no ground height to leave alone");
    } finally {
      fakeMap.easeTo = realEase;
      Corvus.telemetry.getState = realState;
    }
  });

  await check("the camera height is never moved under a drag", async () => {
    await reset();
    map.set3DMode("simple");
    await settle();
    await withCamera(900, () => 1500, async (writes) => {
      fakeMap.isMoving = () => true;
      assert.equal(map._syncTerrainElevation(), false, "mid-drag is not settled, only postponed");
      assert.deepEqual(writes, [], "a drag is never moved under the operator's hand");
    });
  });

  await check("a camera inside the hill is put on the ground at once", async () => {
    await reset();
    zoom = 16;
    map.set3DMode("simple");
    await settle();
    await withCamera(0, () => 1500, async (writes) => {
      assert.equal(map._syncTerrainElevation(), true);
      assert.deepEqual(writes, [1500], "a camera inside the hill is the blank map: no glide");
    });
    zoom = 13;
  });

  await check("a roof under the centre does not move the camera", async () => {
    await reset();
    zoom = 17;
    map.set3DMode("simple");
    await settle();
    // A surface model: the centre is on a 15 m roof, the patch round it is
    // the street at 550 m.
    const c = { lng: centre[0], lat: centre[1] };
    const onCentre = (at) => Math.abs(at.lng - c.lng) < 1e-9 && Math.abs(at.lat - c.lat) < 1e-9;
    await withCamera(550, (at) => (onCentre(at) ? 565 : 550), async (writes) => {
      assert.equal(map._syncTerrainElevation(), true);
      assert.deepEqual(writes, [], "the median of the patch is the street");
    });
    zoom = 13;
  });

  await check("a real change of ground is glided to, not jumped to", async () => {
    await reset();
    zoom = 17;
    map.set3DMode("simple");
    await settle();
    await withFrames(async () => {
      await withCamera(550, () => 580, async (writes, held) => {
        map._syncTerrainElevation();
        assert.ok(writes.length === 1 && writes[0] > 550 && writes[0] < 560,
          `the first frame takes a small step, got ${writes}`);
        runFrames(200);
        assert.equal(held(), 580, "and it arrives");
        for (let i = 1; i < writes.length; i++) {
          assert.ok(writes[i] > writes[i - 1], "steadily, without overshoot");
        }
        const count = writes.length;
        map._syncTerrainElevation();
        runFrames(5);
        assert.equal(writes.length, count, "once there, nothing more is written");
      });
    });
    zoom = 13;
  });

  await check("a glide gives way to a camera move", async () => {
    await reset();
    zoom = 17;
    map.set3DMode("simple");
    await settle();
    await withFrames(async () => {
      await withCamera(550, () => 580, async (writes) => {
        map._syncTerrainElevation();
        const count = writes.length;
        fakeMap.isMoving = () => true;
        runFrames(10);
        assert.equal(writes.length, count, "nothing is written while the operator moves the map");
      });
    });
    zoom = 13;
  });

  await check("the side view starts on the press, not after the elevation probe", async () => {
    await reset();
    const calls = [];
    const realEase = fakeMap.easeTo;
    const realTerrain = fakeMap.setTerrain;
    fakeMap.easeTo = (opts) => { if (opts && opts.pitch !== undefined) calls.push("tilt"); realEase(opts); };
    fakeMap.setTerrain = (spec) => { if (spec) calls.push("terrain"); realTerrain(spec); };
    try {
      map.set3DMode("simple");
      assert.deepEqual(calls, ["tilt"],
        "the tilt is synchronous with the press; the probe can take its whole timeout");
      await settle();
      assert.deepEqual(calls, ["tilt", "terrain"], "and the relief follows");
    } finally {
      fakeMap.easeTo = realEase;
      fakeMap.setTerrain = realTerrain;
    }
  });

  await check("the terrain waits for the tilt to land before it is attached", async () => {
    await reset();
    fakeMap.isEasing = () => true;
    try {
      map.set3DMode("simple");
      await settle();
      assert.equal(style.terrain, null,
        "attaching writes the camera's height, a jumpTo, which would stop the tilt part way");
      fakeMap.isEasing = () => false;
      await new Promise((resolve) => setTimeout(resolve, 120));
      assert.ok(style.terrain, "once the camera is still, the relief comes up");
      assert.equal(pitch, 60);
    } finally {
      fakeMap.isEasing = () => false;
    }
  });

  await check("a camera that never holds still does not keep the terrain away", async () => {
    await reset();
    fakeMap.isMoving = () => true;
    try {
      map.set3DMode("simple");
      await settle();
      assert.equal(style.terrain, null);
      await new Promise((resolve) => setTimeout(resolve, 1700));
      assert.ok(style.terrain, "attached after the wait is up, moving or not");
    } finally {
      fakeMap.isMoving = () => false;
    }
  });

  await check("leaving 3D while the terrain waits leaves no terrain behind", async () => {
    await reset();
    fakeMap.isEasing = () => true;
    try {
      map.set3DMode("simple");
      await settle();
      map.set3DMode("off");
      fakeMap.isEasing = () => false;
      await new Promise((resolve) => setTimeout(resolve, 120));
      assert.equal(style.terrain, null, "the wait is dropped with the mode");
      assert.equal(pitch, 0);
    } finally {
      fakeMap.isEasing = () => false;
    }
  });

  await check("the flown track is redrawn when it shows, not on every sample", async () => {
    await reset();
    map._resetTrackState();
    let draws = 0;
    let drawn = null;
    Object.defineProperty(painted, "vehicle-path", {
      configurable: true,
      get: () => drawn,
      set: (data) => { draws++; drawn = data; },
    });
    const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const at = (m) => ({ armed: true, position: [11.0, 47.4 + m / 111320], gps_h_acc: 0.02 });
    try {
      zoom = 13;   // about 6.5 m per pixel
      map._updateTrack(at(0));
      await wait(130);
      const base = draws;
      for (let m = 1; m <= 8; m++) map._updateTrack(at(m));
      await wait(130);
      assert.equal(draws, base, "eight metres of straight flight is not a pixel at zoom 13");
      zoom = 19;   // about 0.1 m per pixel: every metre shows
      for (let m = 9; m <= 30; m++) map._updateTrack(at(m));
      await wait(130);
      assert.equal(draws, base + 1, "a burst of samples is one redraw, not one each");
      const line = drawn.geometry.coordinates;
      assert.deepEqual(line[line.length - 1], at(30).position, "and it shows the latest");
      // While the operator drags, nothing is redrawn; letting go catches up.
      (mapEvents.movestart || []).forEach((cb) => cb({ originalEvent: { type: "mousedown" } }));
      for (let m = 31; m <= 40; m++) map._updateTrack(at(m));
      await wait(300);
      assert.equal(draws, base + 1, "no redraw under a drag");
      (mapEvents.moveend || []).forEach((cb) => cb({}));
      await wait(300);
      assert.equal(draws, base + 2, "the drag is over and the track catches up");
      assert.deepEqual(drawn.geometry.coordinates.slice(-1)[0], at(40).position);
    } finally {
      delete painted["vehicle-path"];
      map._resetTrackState();
      zoom = 13;
    }
  });

  console.log(`\n${passed} checks passed`);
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
