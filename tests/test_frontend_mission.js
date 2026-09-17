"use strict";

/**
 * Frontend test for src/js/mission.js — the Mission planner's arithmetic and
 * its contract with the Python side.
 *
 * Two kinds of assertion live here.
 *
 * The first is the CONTRACT. The editor restates corvus/mission.py's item
 * types, field names and bounds so it can refuse an impossible value while the
 * operator is still dragging rather than after a round trip. That restatement
 * is a second copy, and a second copy drifts — so the Python module is read off
 * disk and compared, and a bound changed on one side fails here rather than in
 * a 400 the operator sees at the field.
 *
 * The second is the GEOMETRY. Distances, the station list the profile is drawn
 * from, terrain clearance and the pixel <-> altitude conversion the drag is
 * built on are all pure functions, and all of them are wrong in ways nothing
 * throws over: a route half the length it should be still draws a chart. They
 * are the reason those functions are exported as test hooks.
 *
 * Plain Node-runnable assertions (no browser, no test runner), same pattern as
 * the other tests/test_frontend_*.js files: stub the browser globals the module
 * touches at LOAD time, require the source, assert on the exposed surface.
 * render() is never called, so maplibregl and Plotly are never needed.
 *
 * Run:
 *   node tests/test_frontend_mission.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

// ---------------------------------------------------------------------------
// Browser-ish globals, enough for the module to define itself.
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
window.addEventListener = () => {};
window.removeEventListener = () => {};
window.dispatchEvent = () => {};
window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;
global.localStorage = { getItem: () => null, setItem: () => {}, removeItem: () => {} };
global.matchMedia = (query) => ({
  matches: false, media: query,
  addEventListener() {}, removeEventListener() {},
});

const elementStub = () => ({
  innerHTML: "", value: "", hidden: false, disabled: false, textContent: "",
  classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } },
  querySelector() { return null; }, querySelectorAll() { return []; },
  append() {}, appendChild() {}, removeChild() {}, remove() {},
  setAttribute() {}, getAttribute() { return null; }, addEventListener() {},
  removeEventListener() {}, getBoundingClientRect() {
    return { x: 0, y: 0, left: 0, top: 0, width: 0, height: 0 };
  },
  dataset: {}, style: {},
});
global.document = {
  getElementById() { return null; },
  createElement: () => elementStub(),
  createElementNS: () => elementStub(),
  createTextNode: () => elementStub(),
  querySelector() { return null; },
  querySelectorAll: () => [],
  addEventListener() {}, removeEventListener() {},
  documentElement: { setAttribute() {}, style: { setProperty() {} } },
  body: elementStub(),
};

require("../src/js/ui.js");
require("../src/js/mission.js");

const mission = Corvus.mission;
assert.ok(mission, "Corvus.mission must be defined after requiring mission.js");

// ---------------------------------------------------------------------------
// The Python model, read off disk. Regex rather than a parser: these are
// literal assignments in a module that exists to be one table.
// ---------------------------------------------------------------------------
const missionPy = fs.readFileSync(
  path.join(__dirname, "..", "corvus", "mission.py"), "utf-8");

function pyNumber(name) {
  const match = missionPy.match(new RegExp(`^${name}\\s*=\\s*(-?[\\d.]+)`, "m"));
  assert.ok(match, `corvus/mission.py must declare ${name}`);
  return Number(match[1]);
}

/** The item type names ITEM_SPECS declares, in declaration order. */
function pyItemTypes() {
  const block = missionPy.split("ITEM_SPECS: dict[str, dict[str, Any]] = {")[1];
  assert.ok(block, "corvus/mission.py must declare ITEM_SPECS");
  const body = block.split("\nITEM_TYPES")[0];
  // Top-level keys only: they are the ones indented by exactly four spaces.
  return Array.from(body.matchAll(/^ {4}"([a-z_]+)":\s*\{/gm)).map((m) => m[1]);
}

/** The field names one item type's `params` table declares. */
function pyItemParams(type) {
  const block = missionPy.split(`    "${type}": {`)[1] || "";
  const params = block.split('"params": {')[1] || "";
  const body = params.split("\n    },")[0];
  return Array.from(body.matchAll(/^ {12}"([a-z_]+)":/gm)).map((m) => m[1]);
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

function testItemTypesMatchThePythonModel() {
  assert.deepStrictEqual(
    Object.keys(mission._types).sort(), pyItemTypes().sort(),
    "the editor's item palette and corvus/mission.py's ITEM_SPECS must name " +
    "the same types — an extra one here is a plan the backend refuses",
  );
}

function testEachTypesFieldNamesMatchThePythonModel() {
  Object.keys(mission._types).forEach((type) => {
    assert.deepStrictEqual(
      Object.keys(mission._types[type].params).sort(), pyItemParams(type).sort(),
      `the ${type} item's editable fields must match corvus/mission.py`,
    );
  });
}

function testBoundsMatchThePythonModel() {
  const bounds = mission._bounds;
  const pairs = [
    ["ALT_MIN_M", "MISSION_ALT_MIN_M"],
    ["ALT_MAX_M", "MISSION_ALT_MAX_M"],
    ["RADIUS_MIN_M", "MISSION_RADIUS_MIN_M"],
    ["RADIUS_MAX_M", "MISSION_RADIUS_MAX_M"],
    ["HOLD_MAX_S", "MISSION_HOLD_MAX_S"],
    ["TURNS_MAX", "MISSION_TURNS_MAX"],
    ["SPEED_MIN_MS", "MISSION_SPEED_MIN_MS"],
    ["SPEED_MAX_MS", "MISSION_SPEED_MAX_MS"],
    ["MAX_ITEMS", "MISSION_MAX_ITEMS"],
  ];
  pairs.forEach(([js, py]) => {
    assert.strictEqual(
      bounds[js], pyNumber(py),
      `${js} in mission.js must equal ${py} in corvus/mission.py — the ` +
      `editor clamps against its copy, so a drift means the operator is ` +
      `stopped at the wrong number or not stopped at all`,
    );
  });
}

function testTheOrbitTypesAreTheOnesWithARadius() {
  const withRadius = Object.keys(mission._types)
    .filter((type) => "radius" in mission._types[type].params);
  assert.deepStrictEqual(withRadius.sort(), ["loiter_time", "loiter_turns"],
    "only an orbit has a ring on the map to size");
}

function testEveryOrbitCanBeFlownEitherWayRound() {
  ["loiter_turns", "loiter_time"].forEach((type) => {
    const rule = mission._types[type].params.direction;
    assert.ok(rule && rule.choices, `${type} must offer a direction to pick`);
    assert.deepStrictEqual(rule.choices.map((c) => c.value), [1, -1]);
    assert.strictEqual(rule.def, 1, "clockwise unless asked otherwise");
  });
}

function testDirectionIsSnappedToOneOfTwoThingsOnLoad() {
  // Mirrors the same snap in corvus/mission.py: a 0 from a hand-edited file
  // would otherwise fly out as a radius of zero once the sign is applied.
  mission.setPlan({ items: [
    { type: "loiter_turns", lat: 48, lon: 11, alt: 30, radius: 80, direction: 0 },
    { type: "loiter_time", lat: 48, lon: 11, alt: 30, radius: 80, direction: -0.3 },
  ] });
  assert.deepStrictEqual(
    mission.getPlan().items.map((i) => i.direction), [1, -1]);
}

function testOnlyPositionlessTypeIsTheReturn() {
  const positionless = Object.keys(mission._types)
    .filter((type) => !mission._types[type].position);
  assert.deepStrictEqual(positionless, ["rtl"],
    "only a return names no place on the map");
}

// ---------------------------------------------------------------------------
// Shared with the Home tab, rather than forked from it
//
// Three controls on this page are the Home map's, and the point of them being
// the Home map's is that they cannot drift. Each was a second, thinner copy
// once, and each looked close enough in a screenshot to survive review while
// behaving differently: a layer list with no service headings and no
// persistence, and a tool rail that was a column of unlabelled icons beside a
// screen whose primary controls are a labelled row.
//
// Source-text assertions, like tests/test_frontend_css_boundary.js: what they
// pin is invisible at runtime, because a fork works — it is just a second
// thing to keep in step.
// ---------------------------------------------------------------------------
const missionJs = fs.readFileSync(
  path.join(__dirname, "..", "src", "js", "mission.js"), "utf-8");
const mapJs = fs.readFileSync(
  path.join(__dirname, "..", "src", "js", "map.js"), "utf-8");

function testTheLayerSwitcherIsTheHomeMapsOwn() {
  assert.ok(mapJs.includes("createLayerMenu,"),
    "map.js must export createLayerMenu for a second map's rail");
  assert.ok(missionJs.includes("Corvus.map.createLayerMenu("),
    "the Mission rail must build its switcher through map.js");
  assert.ok(!missionJs.includes('className: "layers-popover"'),
    "a second layers popover defined here is a second one to keep in step " +
    "with the Home rail's — build it through Corvus.map.createLayerMenu");
}

function testTheLayerCatalogueIsFetchedOnlyOnce() {
  assert.ok(mapJs.includes("layerSpec: specFor"),
    "map.js must expose the hydrated catalogue's descriptors");
  assert.ok(missionJs.includes("Corvus.map.layerSpec("),
    "the Mission map must read layer descriptors from that one catalogue " +
    "rather than keeping a second copy of corvus/tile_sources.py");
}

function testTheToolBarIsTheHomeFlightBar() {
  assert.ok(missionJs.includes('"flight-actions glass mission-tools"'),
    "the tool bar must be built on .flight-actions, the Home tab's own");
  assert.ok(missionJs.includes('button.className = "fa-btn"'),
    "and its buttons on .fa-btn, so they are the same size and shape");
  assert.ok(missionJs.includes('caption.textContent = entry.label'),
    "every tool carries a caption under its icon, like ARM / TAKEOFF / LAND");
}

function testEveryToolHasACaptionToShow() {
  // The bar renders icon over `entry.label`, so a tool missing one is a blank
  // button — which is exactly what the vertical icon rail this replaced was.
  const tools = mission._tools.filter((entry) => entry.id.indexOf("divider") !== 0);
  assert.ok(tools.length >= 8, `expected the full tool palette, got ${tools.length}`);
  tools.forEach((entry) => {
    assert.ok(entry.label && /^[A-Z]+$/.test(entry.label),
      `tool "${entry.id}" needs an upper-case caption, like the Home bar's`);
    assert.ok(entry.icon, `tool "${entry.id}" needs an icon`);
    assert.ok(entry.title, `tool "${entry.id}" needs a tooltip for the narrow layout`);
  });
}

function testEveryPlaceableToolNamesARealItemType() {
  mission._tools
    .filter((entry) => entry.id.indexOf("divider") !== 0 && entry.id !== "select"
      && entry.id !== "home")
    .forEach((entry) => {
      assert.ok(mission._types[entry.id],
        `the "${entry.id}" tool places an item type that does not exist`);
    });
}

// ---------------------------------------------------------------------------
// Markers are positioned by MapLibre, not by us
//
// MapLibre puts `position: absolute` on the element you hand it (its own
// .maplibregl-marker rule) and writes the ground point into a transform.
// main.css loads after the vendor sheet at the same specificity, so ONE
// `position: relative` here silently wins — and every marker falls back into
// normal flow, stacked one row below the last, sitting that far off the route
// it belongs to. It reads as a projection bug and is a cascade one, which is
// why it is worth a test rather than a comment.
// ---------------------------------------------------------------------------
const mainCss = fs.readFileSync(
  path.join(__dirname, "..", "src", "css", "main.css"), "utf-8");

/** Every rule block in *css* whose selector list names one of *classes* as a
 *  whole class (so `.wp-marker-num` does not match `.wp-marker`). */
function rulesNaming(css, classes) {
  const stripped = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const named = new RegExp(
    `(^|[\\s>+~,])\\.(${classes.join("|")})(?![\\w-])`);
  const out = [];
  let match;
  const blocks = /([^{}]+)\{([^{}]*)\}/g;
  while ((match = blocks.exec(stripped)) !== null) {
    const selector = match[1].trim();
    if (named.test(selector)) out.push({ selector, body: match[2] });
  }
  return out;
}

/** Rule blocks at the TOP level of *css* — never the ones nested inside an
 *  `@media`, which rulesNaming cannot tell apart from an unconditional rule
 *  and which is the whole point when the question is "at what width". */
function topLevelRules(css) {
  const stripped = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const out = [];
  let depth = 0;
  let start = 0;
  let head = "";
  for (let i = 0; i < stripped.length; i += 1) {
    const ch = stripped[i];
    if (ch === "{") {
      if (depth === 0) { head = stripped.slice(start, i).trim(); start = i + 1; }
      depth += 1;
    } else if (ch === "}") {
      depth -= 1;
      if (depth === 0) {
        // An at-rule's body is a block of rules, not declarations; the
        // declarations inside it belong to a width, not to every width.
        if (head && head[0] !== "@") out.push({ selector: head, body: stripped.slice(start, i) });
        start = i + 1;
      }
    }
  }
  return out;
}

const tilesJs = fs.readFileSync(
  path.join(__dirname, "..", "src", "js", "tiles.js"), "utf-8");

function testTheToolBarIsOneRowLikeTheHomeBar() {
  // The two bars were already the same glass, radius, padding, gap and 72px
  // buttons. The ONE difference was that this one wrapped — 127px tall against
  // the Home bar's 68px — and a control set that stacks does not read as the
  // control set it copies.
  const wraps = topLevelRules(mainCss)
    .filter((rule) => /\.mission-tools(?![\w-])/.test(rule.selector))
    .map((rule) => (rule.body.match(/flex-wrap:\s*([\w-]+)/) || [])[1])
    .filter(Boolean);
  assert.ok(wraps.length > 0, "the tool bar must state how it wraps");
  wraps.forEach((value) => assert.strictEqual(value, "nowrap",
    "the planner's tool bar must stay on one row, as the Home flight bar does"));

  assert.ok(/toolsEl\.scrollWidth > room/.test(missionJs),
    "and whether the captions fit must be MEASURED against the bar's own box. " +
    "A viewport media query asks the wrong question: the bar's room is the map " +
    "column, which changes when the right panel opens without the window moving.");
  assert.ok(/classList\.remove\("is-tight", "is-compact"\)/.test(missionJs),
    "the measurement must start from the full bar, or one that has shrunk once " +
    "can never grow back");
}

function testOnlyAMultirotorHidesTheLoiterRadius() {
  /* A loiter radius is the circle an aircraft that cannot hover flies in order
     to stay at a point. Hiding it from an airframe that DOES fly it would take
     away the only control over how tight that circle is. */
  const was = Corvus.telemetry;
  const asVehicle = (state) => {
    Corvus.telemetry = { getState: () => state };
    return mission._vehicleHovers();
  };
  try {
    assert.strictEqual(asVehicle({ connected: true, vehicle_type: "QUADROTOR" }), true);
    assert.strictEqual(asVehicle({ connected: true, vehicle_type: "HEXAROTOR" }), true);
    assert.strictEqual(asVehicle({ connected: true, vehicle_type: "FIXED_WING" }), false,
      "a fixed wing cannot hold a point without circling it");
    assert.strictEqual(asVehicle({ connected: true, vehicle_type: "VTOL_QUADROTOR" }), false,
      "a VTOL may hold in either mode, so the field stays");
    assert.strictEqual(asVehicle({ connected: true, vehicle_type: "GENERIC" }), false,
      "an airframe that has not said what it is is not an argument for hiding it");
    assert.strictEqual(asVehicle({ connected: false, vehicle_type: "QUADROTOR" }), false,
      "and a plan drawn with nothing connected keeps every field the file carries");
  } finally {
    Corvus.telemetry = was;
  }
}

function testEveryHoveringTypeIsARotorcraft() {
  // The names are MAV_TYPE_MAP's in corvus/mavlink_bridge.py; one that drifts
  // would silently stop matching and the field would come back.
  const bridge = fs.readFileSync(
    path.join(__dirname, "..", "corvus", "mavlink_bridge.py"), "utf-8");
  const known = Array.from(bridge.matchAll(/\d+:\s*"([A-Z_]+)"/g)).map((m) => m[1]);
  mission._hoverTypes.forEach((type) => {
    assert.ok(known.indexOf(type) >= 0,
      `"${type}" is not a MAV_TYPE the bridge ever puts in the state store, so ` +
      `the rule that reads it can never fire`);
  });
  assert.strictEqual(mission._hoverTypes.indexOf("FIXED_WING"), -1);
}

function testAHiddenRadiusIsStillSavedAndUploaded() {
  // The airframe decides what is SHOWN, never what the plan carries: the same
  // mission opened with a fixed wing connected must have its circle back.
  mission.setPlan({
    items: [{
      type: "loiter_time", lat: 48, lon: 11, alt: 40,
      seconds: 30, radius: 120, direction: -1,
    }],
  });
  const item = mission.getPlan().items[0];
  assert.strictEqual(item.radius, 120, "the radius stays in the plan");
  assert.strictEqual(item.direction, -1, "and so does the direction");
}

function testTheOfflineControlsSitWhereTheHomeMapsDo() {
  // Not "look like": the SAME classes, so the download square and the trash
  // land on the same pixels as the Home map's and cannot drift from them.
  assert.ok(/className:\s*"tiles-trigger"/.test(missionJs),
    "the planner's offline-download button must be .tiles-trigger — the Home " +
    "map's own control, top right beside the zoom rail");
  assert.ok(/className:\s*"track-clear mission-plan-clear"/.test(missionJs),
    "and its clear-the-route button must be .track-clear — the Home map's " +
    "own, bottom left above the attribution");

  // A mission-only rule that re-positioned either of them would put the same
  // control in two places across two screens, which is the whole thing this
  // pair of classes exists to prevent.
  topLevelRules(mainCss)
    .filter((rule) => /\.mission-plan-clear(?![\w-])/.test(rule.selector)
      || /\.mission-map-wrap\s+\.tiles-trigger/.test(rule.selector))
    .forEach((rule) => {
      assert.ok(!/(^|[;\s])(top|right|bottom|left|position)\s*:/.test(rule.body),
        `"${rule.selector}" moves a control that is meant to be in exactly ` +
        `the place the Home map puts it.`);
    });
}

function testTheZoomRailCarriesNeitherOfThem() {
  const ids = mission._mapControls.map((entry) => entry.id);
  assert.ok(ids.indexOf("regions") >= 0,
    "the rail shows the downloaded areas, as the Home rail does");
  ["download", "clear"].forEach((id) => {
    assert.strictEqual(ids.indexOf(id), -1,
      `"${id}" belongs in its own corner, not in the rail — that is where an ` +
      `operator who has used the Home map will look for it`);
  });
}

function testTheOfflineDialogFollowsWhicheverMapIsShowing() {
  assert.ok(/function useMap\(/.test(tilesJs) && /useMap,/.test(tilesJs),
    "Corvus.tiles must take a map to read its bounds from; the planner owns a " +
    "second one and a download framed on the Home map's view would be the " +
    "wrong ground entirely");
  assert.ok(!/Corvus\.map\.getMap\(\)/.test(tilesJs),
    "and it must not reach for the Home map directly any more");
  // The rectangles are a fact about the cache, not about one map, so they go
  // to every map that can draw them.
  assert.ok(/eachHost\(\(owner\) => owner\.setRegions\(regions\)\)/.test(tilesJs),
    "a downloaded area must be drawn on BOTH maps at once — the Home map must " +
    "not have to be reopened to learn what the planner just fetched");
}

function testTheDownloadedAreasSurviveALayerSwitch() {
  // The Home map had exactly this bug: re-inserting the imagery above the
  // region overlay made the downloaded areas vanish on the one action an
  // operator takes while deciding what still needs downloading.
  assert.ok(/map\.addLayer\(\{ id: "base", type: "raster", source: "base" \}, firstOverlayLayer\(\)\)/
    .test(missionJs),
    "the base raster must be inserted below the FIRST overlay, not below a " +
    "single named one");
  const overlays = missionJs.split("const OVERLAY_LAYERS = [")[1].split("]")[0];
  assert.ok(/mission-regions-fill/.test(overlays),
    "and the downloaded-area layers must be in that list");
}

function testTheToolBarKeepsTheHomeFlightBarsSize() {
  const rules = topLevelRules(mainCss);
  const base = rules.find((rule) => /(^|,)\s*\.fa-btn\s*$/.test(rule.selector));
  assert.ok(base, ".fa-btn must be findable in main.css");
  const baseWidth = Number((base.body.match(/min-width:\s*(\d+)px/) || [])[1]);
  assert.ok(baseWidth > 0, ".fa-btn must quote a min-width");

  rules
    .filter((rule) => /\.mission-tools\s+\.fa-btn(?![\w-])/.test(rule.selector))
    .forEach((rule) => {
      const narrowed = rule.body.match(/min-width:\s*(\d+)px/);
      if (!narrowed) return;
      assert.ok(Number(narrowed[1]) >= baseWidth,
        `"${rule.selector}" narrows the planner's buttons to ${narrowed[1]}px ` +
        `where the Home flight bar's are ${baseWidth}px. The bar IS that bar; ` +
        `a smaller copy of it reads as a lesser control set. Narrowing belongs ` +
        `in the media query that also drops the captions.`);
    });
}

function testTheMarksAreSizedRatherThanTransformed() {
  // MapLibre writes its own transform onto every marker element INLINE, so a
  // `transform` here is dead on arrival — which is exactly what the selected
  // state's scale() was until the size took over.
  rulesNaming(mainCss, ["mission-point", "mission-home", "mission-radius-handle"])
    .forEach((rule) => {
      assert.ok(!/(^|[;\s])transform\s*:/.test(rule.body),
        `"${rule.selector}" sets transform on a MapLibre marker element, ` +
        `which MapLibre overwrites inline. Size the element instead.`);
    });
  const scaled = rulesNaming(mainCss, ["mission-point"])
    .filter((rule) => /--mission-point-scale/.test(rule.body));
  assert.ok(scaled.length > 0,
    "the marks must read --mission-point-scale, which mission.js writes from " +
    "the camera's zoom");
  assert.ok(missionJs.includes('setProperty("--mission-point-scale"'),
    "and mission.js must be the one publishing it");
}

function testNothingOverridesMapLibresMarkerPositioning() {
  // The elements handed to maplibregl.Marker on the Mission page, plus the two
  // Home-tab classes they share.
  const markers = ["mission-point", "mission-home", "mission-radius-handle",
                   "wp-marker", "home-marker"];
  const rules = rulesNaming(mainCss, markers);
  assert.ok(rules.length > 0, "the marker rules must be findable in main.css");
  rules.forEach((rule) => {
    assert.ok(!/(^|[;\s])position\s*:/.test(rule.body),
      `"${rule.selector}" sets position on a MapLibre marker element. ` +
      `MapLibre owns that property — overriding it drops the marker into ` +
      `normal flow and it stops sitting on its own coordinate.`);
  });
}

function testTheBadgeStillAnchorsToItsMarker() {
  // It may — and must — be absolute: MapLibre's own `position: absolute` on
  // the marker makes it the containing block, so the badge needs no help.
  const badge = rulesNaming(mainCss, ["mission-point-badge"])
    .find((rule) => /position\s*:\s*absolute/.test(rule.body));
  assert.ok(badge, "the type badge must be absolutely positioned on the ring");
}

function testTheMarksGrowAsTheCameraPullsBack() {
  const near = mission._pointScaleFor(17);
  const far = mission._pointScaleFor(9);
  assert.strictEqual(near, 1, "close in, a mark is its quoted size");
  assert.ok(far > 1.2, `pulled back, it must be noticeably bigger, got ${far}`);
  assert.strictEqual(mission._pointScaleFor(21), near,
    "past the near end the size holds rather than shrinking away");
  assert.strictEqual(mission._pointScaleFor(3), far,
    "and past the far end it holds rather than growing without bound");
  const middle = mission._pointScaleFor(13);
  assert.ok(middle > near && middle < far, "and it moves monotonically between");
  assert.strictEqual(mission._pointScaleFor(undefined), 1,
    "a camera that has not reported a zoom must not put NaN into a stylesheet");
}

function testTheRouteIsDrawnWiderTheFurtherOutTheCameraIs() {
  const expression = mission._widthByZoom(3);
  assert.strictEqual(expression[0], "interpolate",
    "the width must be a MapLibre zoom expression, evaluated per frame");
  const [, , , zoomFar, widthFar, zoomNear, widthNear] = expression;
  assert.ok(zoomFar < zoomNear, "the stops must run outward-in");
  assert.strictEqual(widthNear, 3, "the quoted width is what is drawn close in");
  assert.ok(widthFar > widthNear,
    `a route pulled back must be drawn wider, got ${widthFar} vs ${widthNear}`);
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

function testDistanceMatchesAKnownSeparation() {
  // One degree of latitude is ~111.2 km.
  const metres = mission._distanceM({ lat: 48, lon: 11 }, { lat: 49, lon: 11 });
  assert.ok(Math.abs(metres - 111195) < 200,
    `a degree of latitude must measure ~111 km, got ${metres}`);
}

function testDistanceOfAPointToItselfIsZero() {
  assert.strictEqual(mission._distanceM({ lat: 48, lon: 11 }, { lat: 48, lon: 11 }), 0);
}

const HOME = { lat: 48, lon: 11 };
const ROUTE = [
  { id: 1, type: "takeoff", lat: 48, lon: 11, alt: 20 },
  { id: 2, type: "waypoint", lat: 48.01, lon: 11, alt: 50 },
  { id: 3, type: "rtl" },
];

function testStationsStartAtHomeAndAccumulateDistance() {
  const stations = mission._stations(ROUTE, HOME);
  assert.deepStrictEqual(stations.map((s) => s.type),
    ["home", "takeoff", "waypoint", "rtl"],
    "the profile starts at home, or the climb-out is invisible");
  assert.strictEqual(stations[0].distance, 0);
  assert.strictEqual(stations[0].alt, 0, "home is the zero of this frame");
  // Home and the takeoff are the same point, so the first leg is zero long.
  assert.strictEqual(stations[1].distance, 0);
  assert.ok(stations[2].distance > 1000, "one hundredth of a degree is ~1.1 km");
}

function testAReturnIsDrawnBackAtHome() {
  const stations = mission._stations(ROUTE, HOME);
  const back = stations[stations.length - 1];
  assert.strictEqual(back.lat, HOME.lat);
  assert.strictEqual(back.lon, HOME.lon);
  assert.ok(back.distance > stations[2].distance,
    "coming home is flown distance, not a free teleport");
}

function testAReturnWithNoHomeIsSkippedRatherThanDrawnAtZero() {
  const stations = mission._stations([{ id: 1, type: "rtl" }], null);
  assert.deepStrictEqual(stations, [],
    "an RTL with nowhere to return to must not put a station at 0,0");
}

function testStationsWithoutAHomeStillDrawTheItems() {
  const stations = mission._stations(ROUTE.slice(0, 2), null);
  assert.deepStrictEqual(stations.map((s) => s.type), ["takeoff", "waypoint"]);
  assert.strictEqual(stations[0].distance, 0);
}

function testRouteLengthAddsTheOrbitCircumference() {
  const orbit = [{ id: 1, type: "loiter_turns", lat: 48, lon: 11, alt: 30, turns: 2, radius: 100 }];
  const length = mission._routeLength(orbit, HOME);
  // Every point is on top of home, so the orbits are the whole length.
  assert.ok(Math.abs(length - 2 * 2 * Math.PI * 100) < 1e-6,
    `two 100 m orbits must measure ~1257 m, got ${length}`);
}

function testRouteDurationIncludesHoldsAndTimedLoiters() {
  const items = [
    { id: 1, type: "waypoint", lat: 48, lon: 11, alt: 30, hold: 30 },
    { id: 2, type: "loiter_time", lat: 48, lon: 11, alt: 30, seconds: 60, radius: 50 },
  ];
  // No ground distance at all — every point is home — so the duration IS the
  // waiting, which a distance-only estimate would report as zero.
  const seconds = mission._routeDuration(items, HOME, 10);
  assert.ok(Math.abs(seconds - 90) < 1e-6,
    `a 30 s hold plus a 60 s loiter is 90 s of flight, got ${seconds}`);
}

function testCircleRingClosesAndIsRoughlyTheAskedRadius() {
  const ring = mission._circleRing({ lat: 48, lon: 11 }, 100, 32);
  assert.strictEqual(ring.length, 33, "a closed ring repeats its first point");
  assert.deepStrictEqual(ring[0], ring[ring.length - 1]);
  const east = { lat: ring[0][1], lon: ring[0][0] };
  const metres = mission._distanceM({ lat: 48, lon: 11 }, east);
  assert.ok(Math.abs(metres - 100) < 5,
    `the ring must sit ~100 m from its centre, got ${metres}`);
}

// ---------------------------------------------------------------------------
// Terrain and clearance
// ---------------------------------------------------------------------------

function testTheRingStartsDueEastOfItsCentre() {
  // The radius grip is put on the ring's FIRST vertex, so where that vertex
  // is decides where the grip appears. Due east keeps it clear of the number
  // in the marker above it and gives the drag a horizontal axis.
  const centre = { lat: 48, lon: 11 };
  const [lon, lat] = mission._circleRing(centre, 200, 64)[0];
  assert.ok(Math.abs(lat - centre.lat) < 1e-9, "same latitude as the centre");
  assert.ok(lon > centre.lon, "and to the east of it");
}

function testTerrariumDecodingMatchesTheKnownPacking() {
  // The format's own zero point: (32768, 0, 0) is sea level.
  assert.strictEqual(mission._decodeTerrarium(128, 0, 0), 0);
  assert.strictEqual(mission._decodeTerrarium(128, 100, 0), 100);
  assert.ok(mission._decodeTerrarium(127, 0, 0) < 0, "below sea level reads negative");
}

const GROUND = { distances: [0, 1000, 2000], elevations: [0, 100, 50] };

function testInterpolationReadsBetweenSamples() {
  assert.strictEqual(mission._interpolateAt(GROUND, 500), 50);
  assert.strictEqual(mission._interpolateAt(GROUND, 1500), 75);
}

function testInterpolationClampsOutsideTheSampledRange() {
  assert.strictEqual(mission._interpolateAt(GROUND, -100), 0);
  assert.strictEqual(mission._interpolateAt(GROUND, 99999), 50);
}

function testClearanceIsTheGapBetweenPlanAndGround() {
  const points = [{ id: 1, type: "waypoint", distance: 1000, alt: 130 }];
  assert.deepStrictEqual(mission._clearances(points, GROUND), [30]);
}

function testStationsMeantToBeOnTheGroundHaveNoClearance() {
  const points = [
    { id: "home", type: "home", distance: 0, alt: 0 },
    { id: 1, type: "land", distance: 1000, alt: 0 },
    { id: 2, type: "rtl", distance: 2000, alt: 0 },
  ];
  assert.deepStrictEqual(
    mission._clearances(points, GROUND), [null, null, null],
    "a landing's clearance is zero by design; counting it would put 0 m in " +
    "the summary for every correct plan",
  );
}

function testClearanceIsUnknownWithoutTerrain() {
  const points = [{ id: 1, type: "waypoint", distance: 1000, alt: 130 }];
  assert.deepStrictEqual(mission._clearances(points, null), [null]);
}

// ---------------------------------------------------------------------------
// The drag: pixels <-> altitude
// ---------------------------------------------------------------------------

const GEOM = {
  left: 50, top: 10, plotWidth: 900, plotHeight: 200,
  xRange: [0, 2], yRange: [0, 100],
};

function testPixelConversionPutsTheTopOfTheRangeAtTheTop() {
  assert.deepStrictEqual(mission._toPixel(GEOM, 0, 100), { px: 50, py: 10 });
  assert.deepStrictEqual(mission._toPixel(GEOM, 2, 0), { px: 950, py: 210 });
}

function testAltitudeConversionIsThePixelConversionInverted() {
  [0, 25, 50, 100].forEach((alt) => {
    const { py } = mission._toPixel(GEOM, 1, alt);
    assert.ok(Math.abs(mission._toAltitude(GEOM, py) - alt) < 1e-9,
      `a pixel turned back into an altitude must give ${alt}`);
  });
}

function testDraggingOffTheChartIsClampedToWhatIsDrawn() {
  assert.strictEqual(mission._toAltitude(GEOM, -500), 100, "above the frame");
  assert.strictEqual(mission._toAltitude(GEOM, 5000), 0, "below the frame");
}

function testTheDrawnRangeAlwaysContainsEveryPointWithRoom() {
  const points = [
    { id: 1, type: "takeoff", distance: 0, alt: 0 },
    { id: 2, type: "waypoint", distance: 100, alt: 80 },
  ];
  const [low, high] = mission._profileRange(points, GROUND);
  assert.ok(low < 0, "the range leaves room under the lowest point");
  assert.ok(high > 100, "and over the highest ground sample");
}

function testClampingRejectsJunkRatherThanProducingNaN() {
  assert.strictEqual(mission._clampNumber("abc", 0, 10, 5), 5);
  assert.strictEqual(mission._clampNumber(undefined, 0, 10, 5), 5);
  assert.strictEqual(mission._clampNumber(99, 0, 10, 5), 10);
  assert.strictEqual(mission._clampNumber(-99, 0, 10, 5), 0);
}

// ---------------------------------------------------------------------------
// The plan, round-tripped
// ---------------------------------------------------------------------------

function testSetPlanAndGetPlanRoundTripWhatTheBackendValidates() {
  mission.setPlan({
    version: 1,
    name: "Ridge run",
    home: { lat: 48, lon: 11 },
    items: [
      { type: "takeoff", lat: 48, lon: 11, alt: 25 },
      { type: "loiter_turns", lat: 48.01, lon: 11.01, alt: 40, turns: 3, radius: 80 },
      { type: "rtl" },
    ],
    speed: 9,
  });
  const plan = mission.getPlan();

  assert.strictEqual(plan.name, "Ridge run");
  assert.strictEqual(plan.speed, 9);
  assert.deepStrictEqual(plan.home, { lat: 48, lon: 11 });
  assert.deepStrictEqual(plan.items.map((i) => i.type), ["takeoff", "loiter_turns", "rtl"]);
  assert.strictEqual(plan.items[1].turns, 3);
  assert.strictEqual(plan.items[1].radius, 80);
  assert.ok(!("lat" in plan.items[2]), "a return carries no position");
}

function testAnOutOfRangeAltitudeIsClampedOnLoadRatherThanRefused() {
  // A plan file from a build with wider bounds must still open — with an
  // altitude the editor can represent, not with one it would upload.
  mission.setPlan({ items: [{ type: "waypoint", lat: 48, lon: 11, alt: 99999 }] });
  assert.strictEqual(mission.getPlan().items[0].alt, mission._bounds.ALT_MAX_M);
}

function testAnUnknownItemTypeIsDroppedOnLoad() {
  mission.setPlan({ items: [
    { type: "teleport", lat: 48, lon: 11, alt: 30 },
    { type: "waypoint", lat: 48, lon: 11, alt: 30 },
  ] });
  assert.deepStrictEqual(mission.getPlan().items.map((i) => i.type), ["waypoint"]);
}

function testAPlanThatCannotBeFlownInOrderSaysSo() {
  // PX4 flies the list literally — it refuses none of this, it simply does it.
  mission.setPlan({
    home: { lat: 48, lon: 11 },
    items: [
      { type: "waypoint", lat: 48, lon: 11, alt: 40 },
      { type: "takeoff", lat: 48.001, lon: 11, alt: 30 },
      { type: "land", lat: 48.002, lon: 11, alt: 0 },
      { type: "waypoint", lat: 48.003, lon: 11, alt: 40 },
    ],
  });
  const said = mission._problems().join(" | ");
  assert.match(said, /takeoff is not the first item/,
    "a takeoff in the middle is a climb the aircraft flies to first");
  assert.match(said, /after the land will not be flown/,
    "items past the end of the mission are never reached");
}

function testTwoTakeoffsAreCalledOut() {
  mission.setPlan({
    home: { lat: 48, lon: 11 },
    items: [
      { type: "takeoff", lat: 48, lon: 11, alt: 30 },
      { type: "takeoff", lat: 48.001, lon: 11, alt: 30 },
    ],
  });
  assert.match(mission._problems().join(" | "), /2 takeoffs/);
}

function testAPlanInOrderIsNotComplainedAbout() {
  mission.setPlan({
    home: { lat: 48, lon: 11 },
    items: [
      { type: "takeoff", lat: 48, lon: 11, alt: 30 },
      { type: "waypoint", lat: 48.001, lon: 11, alt: 40 },
      { type: "land", lat: 48.002, lon: 11, alt: 0 },
    ],
  });
  assert.deepStrictEqual(mission._problems(), [],
    "a warning on a correct plan is a warning nobody reads on a wrong one");
}

function testALandingLoadsOnTheGroundWhateverTheFileSays() {
  mission.setPlan({ items: [{ type: "land", lat: 48, lon: 11, alt: 40 }] });
  assert.strictEqual(mission.getPlan().items[0].alt, 0);
}

// ---------------------------------------------------------------------------

const tests = [
  testItemTypesMatchThePythonModel,
  testEachTypesFieldNamesMatchThePythonModel,
  testBoundsMatchThePythonModel,
  testTheOrbitTypesAreTheOnesWithARadius,
  testEveryOrbitCanBeFlownEitherWayRound,
  testDirectionIsSnappedToOneOfTwoThingsOnLoad,
  testOnlyPositionlessTypeIsTheReturn,
  testTheLayerSwitcherIsTheHomeMapsOwn,
  testTheLayerCatalogueIsFetchedOnlyOnce,
  testTheToolBarIsTheHomeFlightBar,
  testEveryToolHasACaptionToShow,
  testEveryPlaceableToolNamesARealItemType,
  testTheToolBarIsOneRowLikeTheHomeBar,
  testOnlyAMultirotorHidesTheLoiterRadius,
  testEveryHoveringTypeIsARotorcraft,
  testAHiddenRadiusIsStillSavedAndUploaded,
  testTheOfflineControlsSitWhereTheHomeMapsDo,
  testTheZoomRailCarriesNeitherOfThem,
  testTheOfflineDialogFollowsWhicheverMapIsShowing,
  testTheDownloadedAreasSurviveALayerSwitch,
  testTheToolBarKeepsTheHomeFlightBarsSize,
  testTheMarksAreSizedRatherThanTransformed,
  testNothingOverridesMapLibresMarkerPositioning,
  testTheBadgeStillAnchorsToItsMarker,
  testTheMarksGrowAsTheCameraPullsBack,
  testTheRouteIsDrawnWiderTheFurtherOutTheCameraIs,
  testDistanceMatchesAKnownSeparation,
  testDistanceOfAPointToItselfIsZero,
  testStationsStartAtHomeAndAccumulateDistance,
  testAReturnIsDrawnBackAtHome,
  testAReturnWithNoHomeIsSkippedRatherThanDrawnAtZero,
  testStationsWithoutAHomeStillDrawTheItems,
  testRouteLengthAddsTheOrbitCircumference,
  testRouteDurationIncludesHoldsAndTimedLoiters,
  testCircleRingClosesAndIsRoughlyTheAskedRadius,
  testTheRingStartsDueEastOfItsCentre,
  testTerrariumDecodingMatchesTheKnownPacking,
  testInterpolationReadsBetweenSamples,
  testInterpolationClampsOutsideTheSampledRange,
  testClearanceIsTheGapBetweenPlanAndGround,
  testStationsMeantToBeOnTheGroundHaveNoClearance,
  testClearanceIsUnknownWithoutTerrain,
  testPixelConversionPutsTheTopOfTheRangeAtTheTop,
  testAltitudeConversionIsThePixelConversionInverted,
  testDraggingOffTheChartIsClampedToWhatIsDrawn,
  testTheDrawnRangeAlwaysContainsEveryPointWithRoom,
  testClampingRejectsJunkRatherThanProducingNaN,
  testSetPlanAndGetPlanRoundTripWhatTheBackendValidates,
  testAnOutOfRangeAltitudeIsClampedOnLoadRatherThanRefused,
  testAnUnknownItemTypeIsDroppedOnLoad,
  testALandingLoadsOnTheGroundWhateverTheFileSays,
  testAPlanThatCannotBeFlownInOrderSaysSo,
  testTwoTakeoffsAreCalledOut,
  testAPlanInOrderIsNotComplainedAbout,
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
  console.error(`\n${failed}/${tests.length} frontend mission test(s) FAILED`);
  process.exit(1);
}
console.log(`\nAll ${tests.length} frontend mission tests passed.`);
