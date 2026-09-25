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
    ["POINT_NAME_MAX", "MISSION_POINT_NAME_MAX"],
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
  assert.ok(tools.length >= 7, `expected the full tool palette, got ${tools.length}`);
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

/** The declarations of every top-level rule whose selector is exactly this. */
function rulesFor(selector) {
  return topLevelRules(mainCss)
    .filter((rule) => rule.selector.split(",").map((part) => part.trim()).includes(selector))
    .map((rule) => rule.body);
}

function testAHiddenVehicleBoxOrIssueListTakesNoRoom() {
  // Both are display: flex, which outranks the UA's [hidden] rule. The
  // vehicle box has a border and padding, so hidden it still drew as an empty
  // bar between the summary and the item list.
  for (const box of [".mission-vehicle", ".mission-issues"]) {
    assert.ok(rulesFor(`${box}[hidden]`).some((body) => /display:\s*none/.test(body)),
      `${box}[hidden] must restate display: none`);
  }
}

function testTheListScrollsWithTheSidebarAboveIt() {
  // The list used to be the only part of the sidebar that scrolled, and with
  // the summary above it and the point editor below it showed three rows.
  // Now name, summary and list scroll as one box and the list takes its full
  // height inside it.
  assert.ok(rulesFor(".mission-side-scroll").some((body) => /overflow-y:\s*auto/.test(body)),
    "the sidebar's upper part must be one scrolling box");
  rulesFor(".mission-list").forEach((body) => assert.ok(!/overflow/.test(body),
    "on a wide screen the list must not scroll on its own inside that box"));
  assert.ok(/sideScrollEl\.appendChild\(listEl\)/.test(missionJs),
    "the list must live inside the scrolling box");
  assert.ok(/sideScrollEl\.appendChild\(summaryEl\)/.test(missionJs),
    "and so must the summary above it");
  assert.ok(/side\.appendChild\(detailEl\)/.test(missionJs),
    "the point editor stays outside it, pinned above the buttons");
  // A row dragged against the end of what is visible has to scroll the box
  // that actually scrolls; the list's own scrollTop no longer moves.
  assert.ok(/\[listEl, sideScrollEl\]/.test(missionJs),
    "the row drag must scroll the sidebar when the list itself cannot scroll");
}

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

  // The narrowing itself is Corvus.ui.fitBar, which the Home flight bar is
  // also put through — one rule for two bars that have to behave alike,
  // rather than two copies that agree until one of them is edited.
  const uiJs = fs.readFileSync(
    path.join(__dirname, "..", "src", "js", "ui.js"), "utf-8");
  assert.ok(/Corvus\.ui\.fitBar\(toolsEl, toolsEl\.parentNode\.clientWidth\)/.test(missionJs),
    "the planner's bar must be narrowed by the shared helper, against the box " +
    "its parent gives it");
  assert.ok(/bar\.scrollWidth <= space/.test(uiJs),
    "and whether the captions fit must be MEASURED against the bar's own box. " +
    "A viewport media query asks the wrong question: the bar's room is the map " +
    "column, which changes when the right panel opens without the window moving.");
  assert.ok(/classList\.remove\("is-tight", "is-compact"\)/.test(uiJs),
    "the measurement must start from the full bar, or one that has shrunk once " +
    "can never grow back");
}

function testBothBarsAreMeasuredAgainstARoomAndNotThemselves() {
  /* The reservation of the map's right-hand controls belongs to the box the
     bar SITS IN, on both screens: .mission-topleft for the planner,
     .map-topleft for the Home map. It used to be a max-width on the Home bar
     itself, which reads like the same rule and is not — a capped bar cannot
     keep its buttons. It squashes inside the cap, so the same caption is 8px
     narrower there than on the planner, and when even the icon step will not
     fit, its glass ends while the last buttons carry on past the edge.

     A room measures; a bar keeps its size and steps down; and when even the
     last step will not fit the room, it floats over the map's chrome in one
     piece — which it may, outranking it (--z-map-panel over --z-map-chrome). */
  const declaring = (selector) => {
    const rules = topLevelRules(mainCss)
      .filter((rule) => rule.selector.trim() === selector);
    assert.equal(rules.length, 1, `${selector} must be declared exactly once`);
    return rules[0].body;
  };
  const reservation = /width:\s*calc\(100% - var\(--map-rail-inset\) - var\(--map-trigger-clear\)\)/;
  for (const room of [".map-topleft", ".mission-topleft"]) {
    const body = declaring(room);
    assert.match(body, reservation,
      `${room} must be the room that stops where the map's controls begin`);
    assert.ok(!/max-width:\s*calc/.test(body),
      `${room} must carry a WIDTH — a max-width collapses to the bar and measures nothing`);
    // Nothing is drawn in the room, and it is as wide as the map whether the
    // bar fills it or not — so a drag beside the bar has to reach the map.
    assert.match(body, /pointer-events:\s*none/,
      `${room} must not take presses of its own`);
  }
  // And neither bar may cap itself: its room is the cap, and it has to be,
  // since that is the box the fit is measured against.
  for (const bar of [".flight-actions", ".mission-tools"]) {
    assert.match(declaring(bar), /max-width:\s*none/,
      `${bar} must leave the width to its room`);
  }
}

function testTheTwoBarsButtonsAreTheSameSize() {
  /* The bars are one control set on two screens, so a button has to be the
     same object on both. It was not: the Home bar sits in a shrink-to-fit
     absolute box and its TAKEOFF — the one caption wider than the shared 72px
     — was squashed back to 72 by flex, wearing its word into its own padding,
     while the planner's got the 80 it asks for. A squash is a third narrowing
     step, silent and uneven, on top of the two the bars declare. */
  const base = topLevelRules(mainCss)
    .filter((rule) => /^\.fa-btn$/.test(rule.selector.trim()));
  assert.equal(base.length, 1, ".fa-btn must be declared exactly once");
  assert.match(base[0].body, /flex-shrink:\s*0/,
    "a flight-bar button must keep the width its own word needs, on both maps");

  /* There is no fourth step and no squash anywhere: what a bar does when even
     icons will not fit its room is float over the map's chrome whole, the way
     the planner's always has. A bar that gives up pixels instead comes apart —
     uneven buttons first, then a glass surface that ends before its own last
     button. */
  const shrinkable = topLevelRules(mainCss)
    .filter((rule) => /\.fa-btn/.test(rule.selector))
    .filter((rule) => /flex-shrink:\s*[1-9]/.test(rule.body))
    .map((rule) => rule.selector.trim());
  assert.deepEqual(shrinkable, [],
    "no step may hand the buttons back their flex-shrink:\n  " + shrinkable.join("\n  "));
}

function testProgressIsClaimedOnlyWhileTheVehicleHoldsThisPlan() {
  const plan = {
    name: "Flown",
    items: [
      { type: "waypoint", lat: 48.0, lon: 11.0, alt: 30 },
      { type: "waypoint", lat: 48.1, lon: 11.1, alt: 30 },
      { type: "rtl" },
    ],
  };
  const frame = (over) => Object.assign({
    connected: true, mission_known: true, mission_revision: 4, mission_total: 3,
    mission_item: 1, mission_reached_item: 0, mission_state: "active",
  }, over || {});
  mission.setPlan(plan);
  mission._onVehicleMission(frame());
  assert.equal(mission._progress(), null, "nothing claimed before this plan is on the vehicle");

  mission._syncWith(4);
  assert.deepEqual(mission._progress(), { item: 1, reached: 0, state: "active" });
  assert.equal(mission._currentIndex(), 1);

  mission._onVehicleMission(frame({ mission_state: "complete" }));
  assert.equal(mission._currentIndex(), -1, "a finished mission has no leg being flown");

  mission._onVehicleMission(frame({ mission_revision: 5, mission_known: false, mission_item: -1 }));
  assert.equal(mission._progress(), null, "another station replaced the mission");

  mission._onVehicleMission(frame({ mission_revision: 6 }));
  mission._syncWith(6);
  assert.ok(mission._progress(), "synced again");
  const edited = JSON.parse(JSON.stringify(plan));
  edited.items[1].alt = 45;
  mission.setPlan(edited);
  mission._onVehicleMission(frame({ mission_revision: 6, mission_item: 2 }));
  assert.equal(mission._progress(), null, "an edited plan is no longer the one being flown");

  mission.setPlan(plan);
  assert.ok(mission._progress(), "the same plan again is the same plan");
  mission._syncWith(undefined);
  assert.equal(mission._progress(), null);
  mission.setPlan({ items: [] });
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

function testTheStartMarkCarriesTheTakeoffsNumber() {
  // The takeoff on the start has no ring of its own, so the start wears its
  // number. Without it the route read H, 2, 3 and point 1 looked missing.
  assert.ok(missionJs.includes("buildHomeMarker(startTakeoff())"),
    "the start mark must be built knowing which takeoff it stands for");
  assert.ok(/mission-home-num[\s\S]{0,200}positionNumber\(items\.indexOf\(takeoff\)\)/.test(missionJs),
    "and label it with that takeoff's number, the one the list shows");
  const badge = rulesNaming(mainCss, ["mission-home-num"])
    .find((rule) => /position\s*:\s*absolute/.test(rule.body));
  assert.ok(badge, "the number must be pinned onto the start mark, not laid out beside it");
}

function testTheSelectionIsDrawnInItsOwnColour() {
  // The themed --nav blue sank into water and shadow on imagery. The selection
  // ring has its own literal, like the route.
  const selected = rulesNaming(mainCss, ["mission-point", "mission-home"])
    .filter((rule) => /\.is-selected/.test(rule.selector) && /box-shadow/.test(rule.body));
  assert.ok(selected.length >= 2, "both the points and the start must have a selected state");
  selected.forEach((rule) => {
    assert.ok(/var\(--plan-select\)/.test(rule.body),
      `"${rule.selector}" must draw the selection in --plan-select`);
    assert.ok(!/--nav\b/.test(rule.body),
      `"${rule.selector}" still borrows the themed --nav blue`);
  });
  // The profile sits on a themed surface, where a white ring would vanish in
  // the light theme, so it takes the theme's own text colour instead.
  assert.ok(missionJs.includes('line: { color: Corvus.ui.token("--text-1", "#12151A"), width: 2.5 }'),
    "the profile's selected ring must follow the theme, not --nav");
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

function testAPinnedSpeedCarriesToThePointsAfterIt() {
  // Mirrors PX4: a DO_CHANGE_SPEED holds until another one replaces it, so a
  // speed set on point 2 is also what points 3 and 4 are flown at.
  const items = [
    { id: 1, type: "waypoint", lat: 48, lon: 11, alt: 30, speed: null },
    { id: 2, type: "waypoint", lat: 48, lon: 11, alt: 30, speed: 4 },
    { id: 3, type: "waypoint", lat: 48, lon: 11, alt: 30, speed: null },
    { id: 4, type: "waypoint", lat: 48, lon: 11, alt: 30, speed: 12 },
  ];
  const speeds = mission._speedByItem(items, 9);
  assert.deepStrictEqual([1, 2, 3, 4].map((id) => speeds.get(id)), [9, 4, 4, 12]);
}

function testAPointWithNoSpeedOfItsOwnInheritsThePlansStartSpeed() {
  const items = [{ id: 1, type: "waypoint", lat: 48, lon: 11, alt: 30, speed: null }];
  assert.strictEqual(mission._speedByItem(items, 7).get(1), 7);
  // Nothing pinned anywhere: the estimate still needs a number, and it is the
  // nominal cruise rather than a division by zero.
  assert.ok(mission._speedByItem(items, null).get(1) > 0);
}

function testASlowLegIsTimedAtItsOwnSpeedRatherThanThePlans() {
  // One kilometre out at 10 m/s, one kilometre on at 5 m/s: 100 s + 200 s.
  // A length-over-speed estimate would call the whole thing 200 s.
  const far = { lat: 48, lon: 11 };
  const items = [
    { id: 1, type: "waypoint", lat: 48.008993, lon: 11, alt: 30, hold: 0 },
    { id: 2, type: "waypoint", lat: 48.017986, lon: 11, alt: 30, hold: 0, speed: 5 },
  ];
  const seconds = mission._routeDuration(items, far, 10);
  assert.ok(Math.abs(seconds - 300) < 5,
    `a 1 km leg at 10 m/s then 1 km at 5 m/s is ~300 s, got ${seconds}`);
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

// The profile is drawn in Plotly's layout pixels, which the interface scale
// leaves alone, and grabbed with a pointer, which it does not. Under-dividing
// put every station further right and lower than it was drawn: at 150% the
// grab radius reached nothing and a height could not be dragged at all.
function testAPointerReachesTheStationItIsOverAtEveryInterfaceScale() {
  // The plot sits 120px into the page; the station is at 1km / 40m.
  const rect100 = { left: 120, top: 60 };
  const station = mission._toPixel(GEOM, 1, 40);

  [0.8, 1, 1.25, 1.5].forEach((k) => {
    // Where the browser actually paints it: everything below <body> is
    // multiplied by the zoom, the element's rect included.
    const rect = { left: rect100.left * k, top: rect100.top * k };
    const clientX = (rect100.left + station.px) * k;
    const clientY = (rect100.top + station.py) * k;

    const at = mission._plotPoint(rect, clientX, clientY, k);
    assert.ok(Math.hypot(at.px - station.px, at.py - station.py) < 0.001,
      `at ${k * 100}% the pointer lands on the station it is over`);
    assert.ok(Math.abs(mission._toAltitude(GEOM, at.py) - 40) < 0.001,
      `and reads back the height that is drawn there at ${k * 100}%`);
  });
}

// The opposite mistake is as wrong as the original: a scale that is divided
// out twice walks the drag the other way.
function testTheScaleIsDividedOutExactlyOnce() {
  const at = mission._plotPoint({ left: 100, top: 50 }, 300, 200, 2);
  assert.deepStrictEqual(at, { px: 100, py: 75 },
    "(300 - 100) / 2 and (200 - 50) / 2, in the plot's own pixels");
  assert.deepStrictEqual(mission._plotPoint({ left: 100, top: 50 }, 300, 200, 1),
    { px: 200, py: 150 }, "and at 100% it is the plain difference");
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
// Dragging a row to a new place in the list
// ---------------------------------------------------------------------------
// The list's order is the order the plan is flown, and the drag that changes
// it answers one question per pointer sample: which slot is the row over? The
// slot is worked out from the boxes of the OTHER rows, so it is a pure
// function and it is asserted here — a row that lands one place off produces
// a plan that uploads cleanly and flies the wrong route.

// Six rows of 24px with the 2px gap the stylesheet puts between them, as they
// are laid out with one of their number lifted out.
const ROWS = (count) => {
  const boxes = [];
  for (let i = 0; i < count; i += 1) boxes.push({ top: i * 26, height: 24 });
  return boxes;
};

function testARowDroppedOverATopHalfGoesAboveThatRow() {
  const others = ROWS(5);
  // Middle of the dragged row just above the middle of the third row (top 52,
  // middle 64): it belongs before it, at index 2.
  assert.strictEqual(mission._dropSlot(others, 63), 2);
  // And just below that middle, after it.
  assert.strictEqual(mission._dropSlot(others, 65), 3);
}

function testARowDraggedPastEitherEndStopsAtTheEnd() {
  const others = ROWS(5);
  assert.strictEqual(mission._dropSlot(others, -400), 0, "above the list");
  assert.strictEqual(mission._dropSlot(others, 4000), others.length,
    "below the last row, which is the last slot there is");
}

function testARowIsAlreadyInItsOwnSlot() {
  // The dragged row is not among the boxes, so the slot it came from is the
  // one it is over while it has not moved: rows 0..4 with the second lifted
  // out leaves boxes at 0, 26, 52, 78, and its own middle is at 26 + 12.
  const others = [
    { top: 0, height: 24 }, { top: 26, height: 24 },
    { top: 52, height: 24 }, { top: 78, height: 24 },
  ];
  assert.strictEqual(mission._dropSlot(others, 38), 1);
}

function testRowsOfUnequalHeightAreMeasuredEachOnItsOwn() {
  // Nothing promises the rows are the same height — a long value wraps — so
  // the crossing point is each row's own middle, not a multiple of one.
  const others = [{ top: 0, height: 60 }, { top: 62, height: 20 }];
  assert.strictEqual(mission._dropSlot(others, 29), 0);
  assert.strictEqual(mission._dropSlot(others, 31), 1);
  assert.strictEqual(mission._dropSlot(others, 73), 2);
}

// ---------------------------------------------------------------------------
// Leaving the page puts it down; it does not throw it away
// ---------------------------------------------------------------------------
// The planner owns a second MapLibre map, and building one costs a GL context,
// a style, a dozen tile requests and a re-sampled terrain — the best part of a
// second of blank map. It used to pay that on every entry, on a page the
// operator flips to and from constantly while drawing a route. Now the page is
// built once and suspended on the way out.
//
// Both halves of that are invisible at runtime: a planner that is destroyed on
// exit still works, only slowly, and a planner that is suspended but keeps its
// listeners still works, only it answers the keyboard from the wrong page. So
// the contract is asserted against the source, the same way the document
// listeners below are.

const missionSource = fs.readFileSync(
  path.join(__dirname, "..", "src", "js", "mission.js"), "utf8");
const sidenavSource = fs.readFileSync(
  path.join(__dirname, "..", "src", "js", "sidenav.js"), "utf8");

/** One function's body, from `function <name>(` to the closing brace in the
 *  first column of its indentation. Crude, and enough: the two functions read
 *  here are plain module-level declarations. */
function bodyOf(source, name) {
  const start = source.indexOf(`function ${name}(`);
  assert.ok(start >= 0, `${name}() not found — has it been renamed?`);
  const end = source.indexOf("\n  }\n", start);
  assert.ok(end > start, `${name}() has no end brace at module indentation`);
  return source.slice(start, end);
}

function testLeavingThePlannerSuspendsItRatherThanRebuildingIt() {
  assert.match(missionSource, /^\s*suspend,$/m,
    "Corvus.mission must expose suspend() — it is what a nav-away calls");
  assert.match(sidenavSource, /Corvus\.mission\.suspend\(\)/,
    "the left-nav exit from Mission must suspend the page, not tear it down");
  // The destroyer is still reachable, for the page leaving the rail.
  assert.match(sidenavSource, /Corvus\.mission\.teardown\(\)/,
    "teardown() must still be called where the planner leaves the app");
}

function testOnlyTeardownLetsGoOfTheMap() {
  const suspendBody = bodyOf(missionSource, "suspend");
  assert.ok(!/map\.remove\(\)/.test(suspendBody),
    "suspend() must keep the map — releasing it is what made re-entry slow");
  assert.ok(!/terrainTiles = new Map\(\)/.test(suspendBody),
    "suspend() must keep the decoded terrain; re-sampling it is the other half");
  const teardownBody = bodyOf(missionSource, "teardown");
  assert.match(teardownBody, /map\.remove\(\)/,
    "teardown() must still release the GL context");
  assert.match(teardownBody, /suspend\(\);/,
    "teardown() goes through suspend() so there is one list of what is let go");
}

function testEveryWindowListenerIsAlsoRemoved() {
  // The same leak as the document listeners, and the one the suspend/resume
  // split makes easy to get wrong: arm() is called on every entry, so a
  // listener disarm() forgets is added again on each one.
  const added = new Set();
  const removed = new Set();
  const pattern = /window\.(add|remove)EventListener\(\s*"([^"]+)"\s*,\s*([A-Za-z0-9_$]+)/g;
  let match;
  while ((match = pattern.exec(missionSource)) !== null) {
    (match[1] === "add" ? added : removed).add(`${match[2]}:${match[3]}`);
  }
  assert.ok(added.size, "no window listeners found — did the pattern stop matching?");
  for (const key of added) {
    assert.ok(removed.has(key),
      `window listener ${key} is added but never removed; every entry adds another`);
  }
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

function testAPointsOwnSpeedRoundTripsAndAnAbsentOneStaysAbsent() {
  // `speed: null` is not something corvus/mission.py accepts — an absent
  // speed is an absent KEY, because that is what "inherit" means on the wire.
  mission.setPlan({
    version: 1,
    name: "Two speeds",
    home: { lat: 48, lon: 11 },
    items: [
      { type: "waypoint", lat: 48, lon: 11, alt: 25, speed: 4 },
      { type: "waypoint", lat: 48.01, lon: 11.01, alt: 25 },
    ],
  });
  const plan = mission.getPlan();

  assert.strictEqual(plan.items[0].speed, 4);
  assert.ok(!("speed" in plan.items[1]),
    "a point that pins no speed must not send one");
}

function testAPointsNameRoundTripsAndAnUnnamedOneSendsNone() {
  mission.setPlan({
    version: 1,
    name: "Named",
    home: { lat: 48, lon: 11 },
    items: [
      { type: "waypoint", lat: 48, lon: 11, alt: 25, name: "Nordhang Süd" },
      { type: "waypoint", lat: 48.01, lon: 11.01, alt: 25 },
      { type: "rtl", name: "Heim" },
    ],
  });
  const plan = mission.getPlan();

  assert.strictEqual(plan.items[0].name, "Nordhang Süd");
  assert.ok(!("name" in plan.items[1]),
    "an unnamed point must not send an empty name");
  assert.strictEqual(plan.items[2].name, "Heim");
  mission.setPlan({ items: [] });
}

function testAPointNameIsCleanedAsTheBackendCleansIt() {
  const clean = mission._cleanPointName;
  const max = mission._bounds.POINT_NAME_MAX;
  assert.strictEqual(clean("  Ridge  "), "Ridge");
  assert.strictEqual(clean("Ridge\nnorth\tside"), "Ridge north side");
  assert.strictEqual(clean("a\u0000b\u001bc d"), "a b c d");
  assert.strictEqual(clean("   "), "");
  assert.strictEqual(clean(42), "");
  assert.strictEqual(clean(null), "");
  // Counted in code points, as Python counts them: cut in UTF-16 units, an
  // emoji at the limit would be split into half a character.
  const emoji = "\u{1F681}".repeat(max + 5);
  assert.strictEqual(Array.from(clean(emoji)).length, max);
  assert.ok(!/[\uD800-\uDBFF]$/.test(clean(emoji)), "no lone surrogate at the cut");
}

function testRenamingAPointDoesNotMakeItADifferentPlanFromTheOneFlown() {
  // A name never reaches the vehicle, so it cannot be what ends "this is the
  // plan being flown". An altitude can.
  const base = {
    name: "Flown",
    items: [
      { type: "waypoint", lat: 48.0, lon: 11.0, alt: 30 },
      { type: "waypoint", lat: 48.1, lon: 11.1, alt: 30 },
    ],
  };
  mission.setPlan(base);
  const key = mission._planKey();
  const named = JSON.parse(JSON.stringify(base));
  named.items[1].name = "Ridge";
  mission.setPlan(named);
  assert.strictEqual(mission._planKey(), key);
  named.items[1].alt = 45;
  mission.setPlan(named);
  assert.notStrictEqual(mission._planKey(), key);
  mission.setPlan({ items: [] });
}

function testAPointNameIsDrawnBesideTheRingAndNotOnIt() {
  // Its own selector: the marker rules are the ones a position or a transform
  // must never appear in, and this label needs both.
  const rules = rulesNaming(mainCss, ["mission-point-name"]);
  assert.ok(rules.some((rule) => /position\s*:\s*absolute/.test(rule.body)),
    "the name must be absolutely positioned beside its marker");
  rules.forEach((rule) => {
    assert.ok(!/\.(mission-point|wp-marker)(?![\w-])/.test(rule.selector),
      `"${rule.selector}" also names the marker element, which would give it ` +
      "the label's position and transform");
  });
  assert.ok(rules.some((rule) => /pointer-events\s*:\s*none/.test(rule.body)),
    "a name over the map must not take the click that places the next point");
}

function testASpeedOutOfRangeIsClampedOnLoadRatherThanRefused() {
  mission.setPlan({
    version: 1,
    name: "Too fast",
    items: [{ type: "waypoint", lat: 48, lon: 11, alt: 25, speed: 999 }],
  });

  assert.strictEqual(mission.getPlan().items[0].speed,
    mission._bounds.SPEED_MAX_MS);
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

// ---------------------------------------------------------------------------
// The order a plan is drawn in: start, route, one ending
// ---------------------------------------------------------------------------

const at = (lat, lon) => ({ lat, lng: lon });
const types = () => mission.getPlan().items.map((i) => i.type);

function testNothingButTheStartCanBePlacedOnAnEmptyPlan() {
  mission.setPlan({ items: [] });
  assert.strictEqual(mission._toolBlocked("home"), null);
  assert.strictEqual(mission._toolBlocked("select"), null);
  for (const id of ["waypoint", "loiter_turns", "loiter_time", "land", "rtl"]) {
    assert.match(String(mission._toolBlocked(id)), /start first/,
      `"${id}" must wait for the start: a landing with no takeoff is not a mission`);
  }
}

function testThereIsNoSeparateTakeoffTool() {
  assert.ok(!mission._tools.some((entry) => entry.id === "takeoff"),
    "START places the takeoff; a second tool for the same place is the confusion");
}

function testTheStartCarriesItsTakeoff() {
  mission.setPlan({ items: [] });
  mission._placeStart(at(48, 11));
  const plan = mission.getPlan();
  assert.deepStrictEqual(plan.home, { lat: 48, lon: 11 });
  assert.deepStrictEqual(types(), ["takeoff"]);
  assert.strictEqual(plan.items[0].lat, 48);
  assert.strictEqual(plan.items[0].lon, 11);

  // Moving the start moves the takeoff, and never adds a second one.
  mission._placeStart(at(48.001, 11.002));
  assert.deepStrictEqual(types(), ["takeoff"]);
  assert.strictEqual(mission.getPlan().items[0].lat, 48.001);
  assert.strictEqual(mission.getPlan().items[0].lon, 11.002);

  mission._removeStart();
  assert.strictEqual(mission.getPlan().home, undefined);
  assert.deepStrictEqual(types(), [], "the takeoff on the start goes with it");
}

function testAStartDoesNotAddATakeoffToAPlanThatHasOneElsewhere() {
  mission.setPlan({
    items: [
      { type: "takeoff", lat: 48.01, lon: 11, alt: 30 },
      { type: "waypoint", lat: 48.02, lon: 11, alt: 30 },
    ],
  });
  mission._placeStart(at(48, 11));
  assert.deepStrictEqual(types(), ["takeoff", "waypoint"]);
  assert.strictEqual(mission.getPlan().items[0].lat, 48.01, "an older file's takeoff stays put");
}

function testOnlyOneEndingAndNewPointsGoInBeforeIt() {
  mission.setPlan({ items: [] });
  mission._placeStart(at(48, 11));
  mission._addItem("waypoint", at(48.001, 11));
  mission._addItem("land", at(48.002, 11));
  assert.match(String(mission._toolBlocked("land")), /already ends with a landing/);
  assert.match(String(mission._toolBlocked("rtl")), /already ends with a landing/);
  assert.strictEqual(mission._toolBlocked("waypoint"), null);

  mission._addItem("waypoint", at(48.003, 11));
  assert.deepStrictEqual(types(), ["takeoff", "waypoint", "waypoint", "land"],
    "a point drawn after the landing is still flown, before it");
  assert.deepStrictEqual(mission._problems(), []);
}

function testTheTakeoffStaysFirstAndTheEndingStaysLast() {
  mission.setPlan({ items: [] });
  mission._placeStart(at(48, 11));
  const a = mission._addItem("waypoint", at(48.001, 11));
  const b = mission._addItem("waypoint", at(48.002, 11));
  const end = mission._addItem("rtl", null);
  const takeoffId = 1;

  mission._moveItemTo(a.id, 0);
  assert.deepStrictEqual(types(), ["takeoff", "waypoint", "waypoint", "rtl"],
    "nothing goes in front of the climb");
  mission._moveItemTo(b.id, 3);
  assert.deepStrictEqual(types(), ["takeoff", "waypoint", "waypoint", "rtl"],
    "nothing goes after the ending");
  mission._moveItemTo(end.id, 1);
  mission._moveItemTo(takeoffId, 2);
  assert.deepStrictEqual(types(), ["takeoff", "waypoint", "waypoint", "rtl"]);

  mission._moveItemTo(a.id, 2);
  const order = mission.getPlan().items.slice(0, 3).map((i) => i.lat);
  assert.deepStrictEqual(order, [48, 48.002, 48.001],
    "the route between them still reorders freely");
}

function testAPlanWithARouteButNoEndingSaysSo() {
  mission.setPlan({
    home: { lat: 48, lon: 11 },
    items: [
      { type: "takeoff", lat: 48, lon: 11, alt: 30 },
      { type: "waypoint", lat: 48.001, lon: 11, alt: 40 },
    ],
  });
  assert.match(mission._problems().join(" | "), /no ending/);
}

function testALandingLoadsOnTheGroundWhateverTheFileSays() {
  mission.setPlan({ items: [{ type: "land", lat: 48, lon: 11, alt: 40 }] });
  assert.strictEqual(mission.getPlan().items[0].alt, 0);
}

// ---------------------------------------------------------------------------
// The reopen rule: a plan is framed only when none of it is on screen.
// ---------------------------------------------------------------------------

function testABoxOverlapIsTrueForAnySharedGround() {
  const view = { w: 11.0, s: 48.0, e: 12.0, n: 49.0 };
  assert.equal(mission._boxesOverlap({ w: 11.4, s: 48.4, e: 11.6, n: 48.6 }, view), true,
    "a plan inside the view overlaps it");
  assert.equal(mission._boxesOverlap({ w: 10.5, s: 47.5, e: 11.2, n: 48.2 }, view), true,
    "a plan half off the edge is still partly visible");
  assert.equal(mission._boxesOverlap({ w: 12.0, s: 49.0, e: 13.0, n: 50.0 }, view), true,
    "touching at a corner counts as visible rather than as a jump");
  assert.equal(mission._boxesOverlap({ w: 2.0, s: 48.0, e: 3.0, n: 49.0 }, view), false,
    "a plan at the last site is not on this screen");
  assert.equal(mission._boxesOverlap(null, view), false);
  assert.equal(mission._boxesOverlap({ w: 1, s: 1, e: 2, n: 2 }, null), false);
}


// ---------------------------------------------------------------------------
// Document-level listeners have to come off with the page.
// ---------------------------------------------------------------------------
// This module registers a capture-phase listener on the DOCUMENT for the
// placement tool's Escape. The map is rendered and torn down every time the
// operator changes page, so a listener that is added and never removed
// accumulates one live handler per visit, each holding this closure and the
// map it captured. (The search box's own listener is asserted where the box
// lives, in tests/test_frontend_map_search.js.)

function testEveryDocumentListenerIsAlsoRemoved() {
  const source = fs.readFileSync(
    path.join(__dirname, "..", "src", "js", "mission.js"), "utf8");
  const added = new Set();
  const removed = new Set();
  const pattern = /document\.(add|remove)EventListener\(\s*"([^"]+)"\s*,\s*([A-Za-z0-9_$]+)/g;
  let match;
  while ((match = pattern.exec(source)) !== null) {
    (match[1] === "add" ? added : removed).add(`${match[2]}:${match[3]}`);
  }
  assert.ok(added.size, "no document listeners found — did the pattern stop matching?");
  for (const key of added) {
    assert.ok(removed.has(key),
      `document listener ${key} is added but never removed; it leaks on teardown`);
  }
}


const tests = [
  testABoxOverlapIsTrueForAnySharedGround,
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
  testAHiddenVehicleBoxOrIssueListTakesNoRoom,
  testTheListScrollsWithTheSidebarAboveIt,
  testBothBarsAreMeasuredAgainstARoomAndNotThemselves,
  testTheTwoBarsButtonsAreTheSameSize,
  testProgressIsClaimedOnlyWhileTheVehicleHoldsThisPlan,
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
  testTheStartMarkCarriesTheTakeoffsNumber,
  testTheSelectionIsDrawnInItsOwnColour,
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
  testAPinnedSpeedCarriesToThePointsAfterIt,
  testAPointWithNoSpeedOfItsOwnInheritsThePlansStartSpeed,
  testASlowLegIsTimedAtItsOwnSpeedRatherThanThePlans,
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
  testAPointerReachesTheStationItIsOverAtEveryInterfaceScale,
  testTheScaleIsDividedOutExactlyOnce,
  testTheDrawnRangeAlwaysContainsEveryPointWithRoom,
  testClampingRejectsJunkRatherThanProducingNaN,
  testARowDroppedOverATopHalfGoesAboveThatRow,
  testARowDraggedPastEitherEndStopsAtTheEnd,
  testARowIsAlreadyInItsOwnSlot,
  testRowsOfUnequalHeightAreMeasuredEachOnItsOwn,
  testSetPlanAndGetPlanRoundTripWhatTheBackendValidates,
  testAPointsOwnSpeedRoundTripsAndAnAbsentOneStaysAbsent,
  testAPointsNameRoundTripsAndAnUnnamedOneSendsNone,
  testAPointNameIsCleanedAsTheBackendCleansIt,
  testRenamingAPointDoesNotMakeItADifferentPlanFromTheOneFlown,
  testAPointNameIsDrawnBesideTheRingAndNotOnIt,
  testASpeedOutOfRangeIsClampedOnLoadRatherThanRefused,
  testAnOutOfRangeAltitudeIsClampedOnLoadRatherThanRefused,
  testAnUnknownItemTypeIsDroppedOnLoad,
  testALandingLoadsOnTheGroundWhateverTheFileSays,
  testAPlanThatCannotBeFlownInOrderSaysSo,
  testTwoTakeoffsAreCalledOut,
  testAPlanInOrderIsNotComplainedAbout,
  testNothingButTheStartCanBePlacedOnAnEmptyPlan,
  testThereIsNoSeparateTakeoffTool,
  testTheStartCarriesItsTakeoff,
  testAStartDoesNotAddATakeoffToAPlanThatHasOneElsewhere,
  testOnlyOneEndingAndNewPointsGoInBeforeIt,
  testTheTakeoffStaysFirstAndTheEndingStaysLast,
  testAPlanWithARouteButNoEndingSaysSo,
  testEveryDocumentListenerIsAlsoRemoved,
  testLeavingThePlannerSuspendsItRatherThanRebuildingIt,
  testOnlyTeardownLetsGoOfTheMap,
  testEveryWindowListenerIsAlsoRemoved,
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
