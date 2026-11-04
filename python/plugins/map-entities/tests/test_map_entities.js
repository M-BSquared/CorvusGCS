"use strict";

/**
 * Frontend tests for the Map Entities plugin (plugins/map-entities): the
 * geometry that decides whether the aircraft flew through a gate or into a
 * tree's crown, what is saved and loaded, placing by clicks, and that nothing
 * stays on the map or in a handler once an entity is gone.
 *
 * Run:
 *   node plugins/map-entities/tests/test_map_entities.js
 */
const assert = require("node:assert/strict");
const path = require("node:path");

const CORVUS = process.env.CORVUS_ROOT
  ? path.resolve(process.env.CORVUS_ROOT)
  : path.join(__dirname, "..", "..", "..");
const PLUGIN = path.join(__dirname, "..", "map-entities.js");

global.window = global;
global.Corvus = {};

function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(), className: "", children: [],
    dataset: {}, hidden: false, _attrs: {}, _listeners: {}, style: {}, _text: "",
  };
  Object.defineProperty(e, "textContent", {
    get: () => e._text,
    set: (value) => { e._text = String(value); e.children = []; },
  });
  e.classList = {
    add(c) { e.className = (e.className + " " + c).trim(); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle() {},
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => { e.children.push(c); return c; };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.querySelector = () => null;
  e.querySelectorAll = () => [];
  return e;
}
global.document = { createElement: makeEl, createElementNS: (ns, tag) => makeEl(tag) };

let registered = null;
Corvus.plugins = { register(id, spec) { registered = { id, spec }; return true; } };
Corvus.ui = { icon: () => makeEl("i"), refreshIcons() {} };

require(path.join(CORVUS, "src", "js", "map-icons.js"));
require(PLUGIN);
const P = Corvus.pluginMapEntities;
const S = P._state;

// ---------------------------------------------------------------------------
// A recording api: what the plugin drew, what it saved, what it said.
// ---------------------------------------------------------------------------
const drawn = {};
const saved = [];
const notes = [];
const picks = [];
let subscribed = null;
let optionValues = {};
let storedSettings = {};

const api = {
  map: {
    drawLine(key, coords, opts) { drawn[key] = { kind: "line", coords, opts }; return true; },
    drawCircle(key, center, radius, opts) { drawn[key] = { kind: "circle", center, radius, opts }; return true; },
    drawPolygon(key, coords, opts) { drawn[key] = { kind: "polygon", coords, opts }; return true; },
    drawMarker(key, point, opts) { drawn[key] = { kind: "marker", point, opts }; return true; },
    remove(key) { const had = key in drawn; delete drawn[key]; return had; },
    pickPoint(onPick, opts) {
      const pick = { onPick, opts, live: true };
      picks.push(pick);
      return () => { pick.live = false; };
    },
    fit() { return true; },
    iconPaths: (name) => Corvus.mapIcons.get(name),
  },
  saveSettings(patch) { saved.push(JSON.parse(JSON.stringify(patch))); return Promise.resolve(patch); },
  getSettings: () => storedSettings,
  getOptions: () => optionValues,
  notification(level, message) { notes.push({ level, message }); },
  subscribe(cb) { subscribed = cb; return () => {}; },
};

function reset() {
  S.entities = [];
  S.results.clear();
  S.selected = null;
  S.placing = null;
  S.prev = null;
  S.ui = null;
  if (S.undo) clearTimeout(S.undo.timer);
  S.undo = null;
  S.api = api;
  S.options = { labels: true, notify: true, armedOnly: true };
  Object.keys(drawn).forEach((k) => delete drawn[k]);
  saved.length = 0;
  notes.length = 0;
  picks.length = 0;
}

const near = (a, b, eps, msg) => assert.ok(Math.abs(a - b) <= eps, `${msg || "value"}: ${a} is not within ${eps} of ${b}`);

// A point `east` and `north` metres from a base, as [lng, lat].
const BASE = [11.6, 48.1];
const at = (east, north) => [
  BASE[0] + east / (111319.49 * Math.cos(BASE[1] * Math.PI / 180)),
  BASE[1] + north / 111319.49,
];
const sample = (east, north, alt) => { const p = at(east, north); return { lng: p[0], lat: p[1], alt: alt == null ? 0 : alt }; };

// A gate 10 m wide, its posts west and east of the base, opening 2 to 6 m up.
function gateAcross(extra) {
  const a = at(-5, 0);
  const b = at(5, 0);
  return Object.assign({
    id: "g1", kind: "gate", name: "Gate 1", color: "#FFD60A",
    lng: a[0], lat: a[1], lng2: b[0], lat2: b[1], base: 2, height: 4,
  }, extra || {});
}

function zoneAt(kind, radius, extra) {
  return Object.assign({
    id: "z" + Math.random().toString(36).slice(2, 6), kind, name: kind, color: "#fff",
    lng: BASE[0], lat: BASE[1], radius, alert: true, height: 10,
  }, extra || {});
}

// ---------------------------------------------------------------------------

function testItRegistersAsATabbedPluginWithOptions() {
  assert.equal(registered.id, "map-entities");
  const spec = registered.spec;
  assert.equal(spec.tab, true, "it can have a tab of its own");
  assert.equal(typeof spec.start, "function", "it starts at boot, to put the entities back on the map");
  assert.deepEqual(spec.options.map((o) => o.key), ["labels", "notify", "armedOnly"]);
  spec.options.forEach((o) => assert.equal(o.type, "toggle"));
}

function testCoordinatesAreReadTheWayTheyArePasted() {
  assert.deepEqual(P.parseCoordinatePair("48.1234, 11.5678"), { lat: 48.1234, lng: 11.5678 });
  assert.deepEqual(P.parseCoordinatePair("48.1234,11.5678"), { lat: 48.1234, lng: 11.5678 });
  assert.deepEqual(P.parseCoordinatePair("48.1234 11.5678"), { lat: 48.1234, lng: 11.5678 });
  assert.deepEqual(P.parseCoordinatePair("48,1234; 11,5678"), { lat: 48.1234, lng: 11.5678 }, "decimal commas after a semicolon");
  assert.deepEqual(P.parseCoordinatePair("-33.5 -70.25"), { lat: -33.5, lng: -70.25 });
  assert.equal(P.parseCoordinatePair("48.1"), null, "one number is not a pair");
  assert.equal(P.parseCoordinatePair("1 2 3"), null);
  assert.equal(P.parseCoordinatePair("95, 11"), null, "latitude beyond the pole");
  assert.equal(P.parseCoordinatePair("48, 190"), null, "longitude beyond the date line");
  assert.equal(P.parseCoordinatePair("north, east"), null);
  assert.equal(P.parseCoordinatePair(""), null);
  assert.equal(P.parseDegrees("48,5", 90), 48.5);
  assert.equal(P.parseDegrees("", 90), null, "empty is not zero");
  assert.equal(P.parseDegrees("91", 90), null);
}

function testDistancesAndBearingsAreInMetres() {
  near(P.distanceM(at(0, 0), at(30, 40)), 50, 0.01, "3-4-5 in metres");
  near(P.bearingDeg(at(0, 0), at(0, 10)), 0, 0.001, "north");
  near(P.bearingDeg(at(0, 0), at(10, 0)), 90, 0.001, "east");
  near(P.bearingDeg(at(0, 0), at(-10, 0)), 270, 0.001, "west");
}

function testAStepAcrossTheGateCrossesItWhereItShouldBe() {
  const gate = gateAcross();
  const hit = P.gateCrossing(gate, sample(-2, -10, 3), sample(-2, 10, 5));
  assert.ok(hit, "a step over the line crosses it");
  near(hit.u, 0.3, 1e-6, "30% along from post A");
  near(hit.t, 0.5, 1e-6, "halfway along the step");
  near(hit.offset, -2, 1e-3, "2 m west of the centre");
  near(hit.alt, 4, 1e-6, "altitude where it crossed, not where it ended");
  const east = P.gateCrossing(gate, sample(4, 10, 3), sample(4, -10, 3));
  near(east.offset, 4, 1e-3, "either direction counts, offset keeps its sign");
}

function testAStepThatMissesTheGateDoesNotCrossIt() {
  const gate = gateAcross();
  assert.equal(P.gateCrossing(gate, sample(8, -10), sample(8, 10)), null, "past post B");
  assert.equal(P.gateCrossing(gate, sample(-8, -10), sample(-8, 10)), null, "past post A");
  assert.equal(P.gateCrossing(gate, sample(0, -10), sample(0, -2)), null, "stops short");
  assert.equal(P.gateCrossing(gate, sample(-10, 1), sample(10, 1)), null, "runs alongside the line");
  assert.equal(P.gateCrossing(gate, sample(0, 5), sample(0, 5)), null, "standing still");
}

function testACrossingIsCountedOnceWhenASampleLandsOnTheLine() {
  const gate = gateAcross();
  const first = P.gateCrossing(gate, sample(0, -5), sample(0, 0));
  const second = P.gateCrossing(gate, sample(0, 0), sample(0, 5));
  assert.ok(first, "the step that ends on the line crosses it");
  assert.equal(second, null, "the step that starts on it does not, again");
}

function testTheOpeningIsAHeightBand() {
  const gate = gateAcross();
  const verdict = (alt) => P.gateVerdict(gate, { alt });
  assert.deepEqual(verdict(4), { ok: true, reason: "" });
  assert.equal(verdict(2).ok, true, "the lower edge is in");
  assert.equal(verdict(6).ok, true, "the upper edge is in");
  assert.deepEqual(verdict(6.5), { ok: false, reason: "above" });
  assert.deepEqual(verdict(1.5), { ok: false, reason: "below" });
  assert.deepEqual(P.gateVerdict({ base: 0, height: 5 }, { alt: -1 }), { ok: false, reason: "below" });
}

function testAZoneIsTouchedByAStepThatPassesThroughItBetweenSamples() {
  const zone = zoneAt("radar", 20);
  const through = P.zoneContact(zone, sample(-100, 5, 10), sample(100, 5, 30));
  assert.equal(through.touched, true, "in at one side and out the other");
  assert.equal(through.inside, false, "neither end is inside");
  near(through.dist, 5, 0.01, "closest approach");
  near(through.alt, 20, 0.01, "altitude at the closest point");
  assert.equal(P.zoneContact(zone, sample(-100, 30), sample(100, 30)).touched, false, "passes outside");
  const end = P.zoneContact(zone, sample(-100, 0), sample(0, 0));
  assert.equal(end.inside, true, "ends in the middle");
  assert.equal(P.zoneContact(zone, sample(0, 0), sample(0, 0)).touched, true, "standing in it");
}

function testNewEntitiesCarryTheirKindsDefaults() {
  const gate = P.makeEntity("gate", at(0, 0), at(6, 8), []);
  assert.equal(gate.kind, "gate");
  assert.equal(gate.name, "Gate 1");
  assert.equal(gate.height, 5);
  assert.equal(gate.base, 0);
  assert.ok(P.makeEntity("gate", at(0, 0), null, []) === null, "a gate needs both posts");
  const tree = P.makeEntity("tree", at(0, 0), null, [gate]);
  assert.equal(tree.name, "Tree 1", "numbered per kind");
  assert.equal(tree.radius, 3);
  assert.equal(tree.height, 12);
  assert.equal(tree.alert, true);
  const second = P.makeEntity("tree", at(1, 1), null, [tree]);
  assert.equal(second.name, "Tree 2");
  const radar = P.makeEntity("radar", at(0, 0), null, []);
  assert.equal(radar.radius, 500);
  const person = P.makeEntity("person", at(0, 0), null, []);
  assert.equal(person.color, "#1E8E3E", "a person is dark green");
  assert.equal(person.radius, 0);
  assert.equal(person.alert, false);
  assert.equal(P.makeEntity("dragon", at(0, 0), null, []), null, "unknown kind");
  assert.equal(P.makeEntity("tree", [11, 95], null, []), null, "off the globe");
  assert.notEqual(tree.id, second.id);
  P.KINDS.forEach((k) => {
    assert.equal(Corvus.mapIcons.has(k.icon), true, k.id + " names a symbol the map api has");
  });
}

function testWhatIsLoadedIsMadeSafe() {
  const list = P.normalizeList([
    { id: "a", kind: "tree", name: "  Old   oak  ", lng: "11.5", lat: 48.2, radius: 1e9, height: -4, color: "javascript:x", note: "n".repeat(900) },
    { id: "a", kind: "tree", lng: 11.6, lat: 48.3 },
    { kind: "gate", lng: 11.5, lat: 48.2 },
    { kind: "gate", lng: 11.5, lat: 48.2, lng2: 11.51, lat2: 48.2, base: "x", height: 99999 },
    { kind: "nope", lng: 11, lat: 48 },
    { kind: "tree", lng: 11, lat: 95 },
    { kind: "tree", lng: "", lat: "" },
    "text", null, 7,
  ]);
  assert.equal(list.length, 3, "the usable ones, nothing else");
  const [oak, twin, gate] = list;
  assert.equal(oak.name, "Old oak", "plain, whitespace folded");
  assert.equal(oak.radius, 100000, "a radius is held to a sane range");
  assert.equal(oak.height, 0);
  assert.equal(oak.color, "#7BD389", "a colour that is not a colour is the kind's");
  assert.equal(oak.note.length, 300);
  assert.notEqual(twin.id, oak.id, "ids stay unique");
  assert.equal(twin.name, "Tree", "no name, the kind's");
  assert.equal(gate.base, 0, "not a number, the default");
  assert.equal(gate.height, 10000);
  assert.deepEqual(P.normalizeList("not a list"), []);
  const many = [];
  for (let i = 0; i < 700; i++) many.push({ kind: "flag", lng: 11, lat: 48 });
  assert.equal(P.normalizeList(many).length, 500, "capped");
}

function testFlyingThroughAGateIsRecordedOnceAndAnnounced() {
  reset();
  S.entities = [gateAcross()];
  const events = P.processStep(sample(0, -10, 4), sample(0, 10, 4), 1000);
  assert.equal(events.length, 1);
  assert.equal(events[0].type, "pass");
  assert.equal(events[0].level, "info");
  assert.match(events[0].text, /Gate 1/);
  const r = S.results.get("g1");
  assert.equal(r.passes, 1);
  assert.equal(r.log.length, 1);
  assert.equal(r.log[0].ok, true);
  assert.equal(P.processStep(sample(0, 10, 4), sample(0, 20, 4), 2000).length, 0, "moving away is nothing");
}

function testCrossingOutsideTheOpeningIsAMissNotAPass() {
  reset();
  S.entities = [gateAcross()];
  const over = P.processStep(sample(0, -10, 9), sample(0, 10, 9), 1);
  assert.equal(over[0].type, "miss");
  assert.equal(over[0].level, "warning");
  assert.match(over[0].text, /above/);
  const under = P.processStep(sample(0, -10, 0.5), sample(0, 10, 0.5), 2);
  assert.match(under[0].text, /below/);
  const r = S.results.get("g1");
  assert.equal(r.passes, 0);
  assert.equal(r.misses, 2);
  assert.deepEqual(r.log.map((l) => l.reason), ["above", "below"]);
}

function testATreeIsOnlyHitBelowItsTop() {
  reset();
  const tree = zoneAt("tree", 4, { height: 10 });
  S.entities = [tree];
  const high = P.processStep(sample(-20, 0, 25), sample(20, 0, 25), 1);
  assert.equal(high.length, 0, "over the top is clear");
  assert.equal(S.results.get(tree.id).hits, 0);
  const low = P.processStep(sample(-20, 0, 6), sample(20, 0, 6), 2);
  assert.equal(low.length, 1);
  assert.equal(low[0].type, "hit");
  assert.equal(low[0].level, "warning");
  assert.equal(S.results.get(tree.id).hits, 1);
  assert.equal(S.results.get(tree.id).inside, false, "it came out the other side");
}

function testAZoneIsEnteredOnceUntilTheAircraftLeaves() {
  reset();
  const radar = zoneAt("radar", 50);
  S.entities = [radar];
  assert.equal(P.processStep(sample(-200, 0), sample(-100, 0), 1).length, 0, "outside");
  const enter = P.processStep(sample(-100, 0), sample(-20, 0), 2);
  assert.equal(enter.length, 1);
  assert.equal(enter[0].type, "enter");
  assert.equal(S.results.get(radar.id).inside, true);
  assert.equal(P.processStep(sample(-20, 0), sample(20, 0), 3).length, 0, "still inside, no second entry");
  assert.equal(S.results.get(radar.id).entries, 1);
  P.processStep(sample(20, 0), sample(200, 0), 4);
  assert.equal(S.results.get(radar.id).inside, false, "left");
  const again = P.processStep(sample(200, 0), sample(0, 0), 5);
  assert.equal(again.length, 1, "entering again is a second entry");
  assert.equal(S.results.get(radar.id).entries, 2);
}

function testAZoneWithoutAnAlertCountsButStaysQuiet() {
  reset();
  const camera = zoneAt("camera", 50, { alert: false });
  const point = zoneAt("person", 0);
  S.entities = [camera, point];
  const events = P.processStep(sample(-100, 0), sample(0, 0), 1);
  assert.equal(events.length, 0);
  assert.equal(S.results.get(camera.id).entries, 1, "counted all the same");
  assert.equal(S.results.has(point.id), true);
  assert.equal(S.results.get(point.id).entries, 0, "no radius, no area");
}

function stateAt(east, north, extra) {
  const p = at(east, north);
  return Object.assign({ connected: true, armed: true, position: [p[0], p[1]], altitude_agl: 4 }, extra || {});
}

function testOnlyArmedFlightIsMeasuredByDefault() {
  reset();
  S.entities = [gateAcross()];
  P.onState(stateAt(0, -10, { armed: false }));
  P.onState(stateAt(0, 10, { armed: false }));
  assert.equal(S.results.size, 0, "carrying it through on the ground is not a pass");
  assert.equal(S.prev, null);
  P.onState(stateAt(0, -10));
  P.onState(stateAt(0, 10));
  assert.equal(S.results.get("g1").passes, 1);
  assert.equal(notes.length, 1, "and the operator is told");
  assert.equal(notes[0].level, "info");
  S.options.armedOnly = false;
  S.prev = null;
  P.onState(stateAt(0, -10, { armed: false }));
  P.onState(stateAt(0, 10, { armed: false }));
  assert.equal(S.results.get("g1").passes, 2, "unless that is what they asked for");
}

function testBadSamplesAndJumpsProveNothing() {
  reset();
  S.entities = [gateAcross()];
  P.onState({ connected: false, armed: true, position: at(0, -10), altitude_agl: 4 });
  P.onState({ connected: true, armed: true, position: [0, 0], altitude_agl: 4 });
  assert.equal(S.prev, null, "no link, no fix: no sample");
  P.onState(stateAt(0, -300));
  P.onState(stateAt(0, 300));
  assert.equal(S.results.size, 0, "600 m in one step is a lost link, not a flight through the gate");
  P.onState(stateAt(0, 300.1));
  assert.equal(S.results.get("g1"), undefined, "GPS jitter is not a step");
}

function testNotificationsFollowTheOption() {
  reset();
  S.entities = [gateAcross()];
  S.options.notify = false;
  P.onState(stateAt(0, -10));
  P.onState(stateAt(0, 10));
  assert.equal(S.results.get("g1").passes, 1);
  assert.equal(notes.length, 0, "recorded, not announced");
}

function testResultsDriveTheColourOnTheMap() {
  reset();
  const gate = gateAcross();
  S.entities = [gate];
  P.onState(stateAt(0, -10, { altitude_agl: 20 }));
  P.onState(stateAt(0, 10, { altitude_agl: 20 }));
  assert.equal(drawn["g1-l"].opts.color, "#FF9F43", "missed: amber");
  P.onState(stateAt(0, -10));
  P.onState(stateAt(0, 10));
  assert.equal(drawn["g1-l"].opts.color, "#7BD389", "passed: green");
  P._resetResults();
  assert.equal(drawn["g1-l"].opts.color, "#FFD60A", "reset: its own colour again");
}

function testStartingDrawsWhatWasSavedAndWatchesTheAircraft() {
  reset();
  S.started = false;
  storedSettings = { entities: [gateAcross(), zoneAt("radar", 300, { id: "r1" }), { kind: "nope" }] };
  optionValues = { labels: false, notify: false, armedOnly: true };
  P.start(api);
  assert.equal(S.entities.length, 2, "the usable ones came back");
  assert.ok(drawn["g1-l"] && drawn["g1-a"] && drawn["g1-b"] && drawn["g1-m"], "a gate is a line, two posts and a mark");
  assert.ok(drawn["r1-m"] && drawn["r1-c"], "a radar is a mark and its reach");
  assert.equal(drawn["r1-c"].radius, 300);
  assert.equal(drawn["r1-m"].opts.label, "", "names off");
  assert.equal(typeof subscribed, "function", "it watches the telemetry from the start");
  P.start(api);
  assert.equal(S.entities.length, 2, "a second start changes nothing");
  P.optionsChanged({ labels: true, notify: true, armedOnly: false }, api);
  assert.equal(drawn["r1-m"].opts.label, "radar", "names on again, at once");
  assert.equal(S.options.armedOnly, false);
  optionValues = {};
  storedSettings = {};
  S.started = false;
}

function testOnlyTheSelectedEntityCanBeMoved() {
  reset();
  const gate = gateAcross();
  const tree = zoneAt("tree", 3, { id: "t1" });
  S.entities = [gate, tree];
  P._select("t1");
  P._select("g1");
  assert.equal(drawn["g1-a"].opts.draggable, true, "the selected gate's posts move");
  assert.equal(drawn["g1-b"].opts.draggable, true);
  assert.equal(drawn["g1-m"].opts.selected, true);
  assert.equal(drawn["t1-m"].opts.draggable, false, "the others stay where they are");
  P._select("t1");
  assert.equal(drawn["g1-a"].opts.draggable, false, "and give it up when another is picked");
  assert.equal(drawn["t1-m"].opts.draggable, true);
}

function testDraggingAPostMovesTheGateAndSavesIt() {
  reset();
  S.entities = [gateAcross()];
  P._select("g1");
  const before = saved.length;
  const post = drawn["g1-b"];
  post.opts.onDrag([11.7, 48.1]);
  assert.deepEqual(drawn["g1-l"].coords[1], [11.7, 48.1], "the line follows the post as it moves");
  assert.notEqual(S.entities[0].lng2, 11.7, "nothing is stored until it is let go");
  assert.equal(saved.length, before);
  post.opts.onDragEnd([11.7, 48.1]);
  assert.equal(S.entities[0].lng2, 11.7);
  assert.equal(S.entities[0].lat2, 48.1);
  assert.equal(saved.length, before + 1, "saved");
  assert.equal(saved[saved.length - 1].entities[0].lng2, 11.7);
}

function testPlacingAGateTakesTwoClicksAndKeepsGoing() {
  reset();
  P._startPlacing("gate");
  assert.equal(picks.length, 1);
  assert.equal(S.placing.kind, "gate");
  picks[0].onPick(at(-5, 0));
  assert.equal(S.entities.length, 0, "one post is not a gate");
  assert.ok(drawn.pending, "the first post is shown while the second is awaited");
  assert.equal(picks.length, 2, "armed again for the second post");
  picks[1].onPick(at(5, 0));
  assert.equal(S.entities.length, 1);
  const gate = S.entities[0];
  assert.equal(gate.kind, "gate");
  near(P.distanceM([gate.lng, gate.lat], [gate.lng2, gate.lat2]), 10, 0.01, "10 m between the posts");
  assert.equal(drawn.pending, undefined, "the temporary post is gone");
  assert.equal(S.selected, gate.id, "the new gate is selected, to adjust it");
  assert.equal(saved.length, 1, "and saved at once");
  assert.equal(picks.length, 3, "ready for the next gate");
  assert.equal(S.placing.step, 0);
  picks[2].onPick(at(20, 0));
  picks[3].onPick(at(30, 0));
  assert.equal(S.entities.length, 2);
  assert.equal(S.entities[1].name, "Gate 2");
}

function testPlacingTreesIsOneClickEachUntilStopped() {
  reset();
  P._startPlacing("tree");
  picks[0].onPick(at(0, 0));
  picks[1].onPick(at(10, 0));
  picks[2].onPick(at(20, 0));
  assert.equal(S.entities.length, 3);
  assert.deepEqual(S.entities.map((e) => e.name), ["Tree 1", "Tree 2", "Tree 3"]);
  assert.ok(drawn[S.entities[0].id + "-c"], "a crown is drawn");
  assert.equal(drawn[S.entities[0].id + "-c"].radius, 3);
  const live = picks[picks.length - 1];
  P._stopPlacing();
  assert.equal(S.placing, null);
  assert.equal(live.live, false, "the pick is dropped, the map is the operator's again");
}

function testEscapeEndsPlacingAndClearsTheHalfPlacedGate() {
  reset();
  P._startPlacing("gate");
  picks[0].onPick(at(0, 0));
  assert.ok(drawn.pending);
  picks[1].opts.onCancel();
  assert.equal(S.placing, null);
  assert.equal(drawn.pending, undefined);
  assert.equal(S.entities.length, 0);
}

function testPickingAnotherKindReplacesThePlacing() {
  reset();
  P._startPlacing("gate");
  picks[0].onPick(at(0, 0));
  P._startPlacing("person");
  assert.equal(picks[1].live, false, "the first pick is dropped");
  assert.equal(drawn.pending, undefined, "and its half gate with it");
  assert.equal(S.placing.kind, "person");
  assert.equal(S.placing.step, 0);
  picks[picks.length - 1].onPick(at(3, 3));
  assert.equal(S.entities[0].kind, "person");
}

function testMapNotReadyIsSaidNotSilent() {
  reset();
  const pick = api.map.pickPoint;
  api.map.pickPoint = () => null;
  try {
    P._startPlacing("tree");
    assert.equal(S.placing, null);
    assert.equal(notes.length, 1);
    assert.equal(notes[0].level, "warning");
  } finally {
    api.map.pickPoint = pick;
  }
}

function testRemovingTakesEveryPieceOffTheMap() {
  reset();
  const gate = gateAcross();
  const radar = zoneAt("radar", 100, { id: "r1" });
  S.entities = [gate, radar];
  P._select("g1");
  assert.deepEqual(Object.keys(drawn).sort(), ["g1-a", "g1-b", "g1-l", "g1-m"]);
  P._select("r1");
  assert.ok(drawn["r1-c"]);
  assert.equal(P._removeEntity("g1"), true);
  assert.ok(!Object.keys(drawn).some((k) => k.startsWith("g1-")), "the line, both posts and the mark");
  assert.equal(S.results.has("g1"), false);
  assert.equal(saved[saved.length - 1].entities.length, 1, "saved without it");
  assert.equal(P._removeEntity("g1"), false, "already gone");
  P._removeEntity("r1");
  assert.equal(Object.keys(drawn).length, 0, "nothing left on the map");
  assert.equal(S.selected, null);
}

function testAnEntityWithoutARadiusDrawsNoArea() {
  reset();
  const radar = zoneAt("radar", 100, { id: "r1" });
  S.entities = [radar];
  P._select("r1");
  assert.ok(drawn["r1-c"]);
  radar.radius = 0;
  P._select(null);
  P._select("r1");
  assert.equal(drawn["r1-c"], undefined, "no radius, no area");
  assert.ok(drawn["r1-m"], "the mark stays");
}

function testTheEditorFollowsWhatIsAddedAndRemoved() {
  reset();
  const container = makeEl("div");
  S.started = true;
  P.init(container, api);
  assert.equal(S.ui.detail.hidden, true, "nothing selected, no editor");
  P._startPlacing("tree");
  picks[0].onPick(at(0, 0));
  assert.equal(S.ui.detail.hidden, false, "the tree just placed is open in the editor");
  assert.equal(S.ui.detail.children.length, 1);
  const first = S.ui.detail.children[0];
  picks[1].onPick(at(10, 0));
  assert.notEqual(S.ui.detail.children[0], first, "and the next one replaces it, not the one before");
  P._removeEntity(S.selected);
  assert.equal(S.ui.detail.hidden, true, "deleting the selected one closes the editor");
  P.destroy(container);
}

function testEveryRowHasItsOwnTrashThatDeletesAtOnce() {
  reset();
  const container = makeEl("div");
  S.started = true;
  S.entities = [zoneAt("tree", 3, { id: "t1", name: "Tree 1" }), zoneAt("tree", 3, { id: "t2", name: "Tree 2" })];
  P.init(container, api);
  const rows = S.ui.list.children;
  assert.equal(rows.length, 2);
  const tools = rows[0].children[3];
  assert.equal(tools.children.length, 2, "show on the map, and delete");
  tools.children[1]._listeners.click[0]({ stopPropagation() {} });
  assert.deepEqual(S.entities.map((e) => e.id), ["t2"], "one click deletes that row's entity");
  assert.equal(S.ui.list.children.length, 1, "and the row is gone");
  assert.equal(S.ui.undo.hidden, false, "with a way back");
  assert.equal(S.ui.undo.children[1].textContent, "Tree 1", "that says what was deleted");
  P.destroy(container);
}

function testADeleteCanBeTakenBackInPlace() {
  reset();
  S.entities = [zoneAt("tree", 3, { id: "a" }), zoneAt("tree", 3, { id: "b" }), zoneAt("tree", 3, { id: "c" })];
  P._removeEntity("b");
  assert.deepEqual(S.entities.map((e) => e.id), ["a", "c"]);
  assert.equal(P._undoDelete(), true);
  assert.deepEqual(S.entities.map((e) => e.id), ["a", "b", "c"], "back where it was");
  assert.equal(S.selected, "b");
  assert.ok(drawn["b-m"], "and on the map again");
  assert.equal(saved[saved.length - 1].entities.length, 3, "and saved");
  assert.equal(P._undoDelete(), false, "nothing left to take back");
  P._removeAll();
  assert.equal(S.entities.length, 0);
  assert.equal(Object.keys(drawn).length, 0);
  assert.equal(P._undoDelete(), true);
  assert.deepEqual(S.entities.map((e) => e.id), ["a", "b", "c"], "all of them, in order");
  P._removeEntity("a");
  P._removeEntity("c");
  P._undoDelete();
  assert.deepEqual(S.entities.map((e) => e.id), ["b", "c"], "only the last delete comes back");
}

function testASectorIsAnAngleFacingADirection() {
  const radar = zoneAt("radar", 100, { angle: 90, bearing: 90 });
  const east = (d) => [d, 0];
  assert.equal(P.inSector(radar, east(50)), true, "straight ahead");
  assert.equal(P.inSector(radar, [50, 40]), true, "inside the 45 degrees either side");
  assert.equal(P.inSector(radar, [50, 60]), false, "outside them");
  assert.equal(P.inSector(radar, east(150)), false, "beyond the radius");
  assert.equal(P.inSector(radar, [-50, 0]), false, "behind it");
  assert.equal(P.inSector(radar, [0, 0]), true, "the centre is in");
  const north = zoneAt("radar", 100, { angle: 90, bearing: 0 });
  assert.equal(P.inSector(north, [-30, 60]), true, "across the north, wrapping past 0");
  assert.equal(P.inSector(north, [30, 60]), true);
  const west = zoneAt("radar", 100, { angle: 120, bearing: 350 });
  assert.equal(P.inSector(west, [-20, 80]), true, "a direction near 360 wraps");
  assert.equal(P.inSector(zoneAt("radar", 100, { angle: 360 }), [-50, 0]), true, "360 is the full circle");
  assert.equal(P.inSector(zoneAt("tree", 10, { angle: 30, bearing: 90 }), [-5, 0]), true, "a crown is always round");
  assert.equal(P.sectorAngle({ kind: "radar" }), 360, "no angle, no sector");
  assert.equal(P.sectorAngle({ kind: "radar", angle: 0 }), 1);
  assert.equal(P.sectorAngle({ kind: "radar", angle: 999 }), 360);
}

function testASectorRingIsTheCentreAndAnArcOfTheRadius() {
  const radar = zoneAt("radar", 200, { angle: 90, bearing: 90 });
  const ring = P.sectorRing(radar);
  assert.deepEqual(ring[0], [radar.lng, radar.lat], "it starts at the centre");
  assert.ok(ring.length >= 4);
  ring.slice(1).forEach((c) => near(P.distanceM([radar.lng, radar.lat], c), 200, 0.5, "every arc point on the radius"));
  const first = P.bearingDeg([radar.lng, radar.lat], ring[1]);
  const last = P.bearingDeg([radar.lng, radar.lat], ring[ring.length - 1]);
  near(first, 45, 0.01, "the arc starts half the angle before the direction");
  near(last, 135, 0.01, "and ends half after");
}

function testASectorOnlyCatchesTheAircraftInsideIt() {
  reset();
  const radar = zoneAt("radar", 100, { id: "r1", angle: 90, bearing: 90 });
  S.entities = [radar];
  assert.equal(P.processStep(sample(0, -150), sample(0, -60), 1).length, 0, "passing south of it, outside the sector");
  assert.equal(S.results.get("r1").entries, 0);
  const through = P.processStep(sample(-50, -80), sample(120, -80), 2);
  assert.equal(through.length, 0, "through the circle's southern part, not the sector's");
  const hit = P.processStep(sample(20, 0), sample(60, 0), 3);
  assert.equal(hit.length, 1, "into the sector");
  assert.equal(hit[0].type, "enter");
  assert.equal(S.results.get("r1").inside, true);
  const fast = P.zoneContact(radar, sample(-300, 5, 10), sample(300, 5, 30));
  assert.equal(fast.touched, true, "a step that crosses the sector between two samples");
  assert.equal(fast.inside, false);
  near(fast.alt, 20 - 0, 12, "at an altitude along the step");
  const full = P.zoneContact(zoneAt("radar", 100), sample(-300, 5), sample(300, 5));
  assert.equal(full.touched, true, "the full circle takes the exact path");
}

function testASectorIsDrawnAsAPieAndAFullCircleAsACircle() {
  reset();
  const radar = zoneAt("radar", 100, { id: "r1", angle: 120, bearing: 0 });
  S.entities = [radar];
  P._select("r1");
  assert.equal(drawn["r1-c"].kind, "polygon", "a sector is a polygon");
  assert.deepEqual(drawn["r1-c"].coords[0], [radar.lng, radar.lat]);
  radar.angle = 360;
  P._select(null);
  P._select("r1");
  assert.equal(drawn["r1-c"].kind, "circle", "a full circle is a circle again");
  const tree = zoneAt("tree", 4, { id: "t1", angle: 30 });
  S.entities.push(tree);
  P._select("t1");
  assert.equal(drawn["t1-c"].kind, "circle", "a crown ignores an angle");
}

function testSectorsAreSavedAndLoadedSafely() {
  const [radar, tree] = P.normalizeList([
    { kind: "radar", lng: 11, lat: 48, radius: 100, angle: 90, bearing: -90 },
    { kind: "tree", lng: 11, lat: 48, angle: 30, bearing: 10 },
  ]);
  assert.equal(radar.angle, 90);
  assert.equal(radar.bearing, 270, "a direction is held to 0 to 360");
  assert.equal(tree.angle, undefined, "a tree has none");
  const odd = P.normalizeEntity({ kind: "radar", lng: 11, lat: 48, angle: "x", bearing: "y" });
  assert.equal(odd.angle, 360);
  assert.equal(odd.bearing, 0);
  assert.equal(P.normalizeEntity({ kind: "camera", lng: 11, lat: 48, angle: 0 }).angle, 1);
  assert.equal(P.normalizeEntity({ kind: "camera", lng: 11, lat: 48, angle: 5000 }).angle, 360);
  const made = P.makeEntity("radar", at(0, 0), null, []);
  assert.equal(made.angle, 360, "new ones are full circles");
  assert.equal(made.bearing, 0);
  assert.equal(P.makeEntity("tree", at(0, 0), null, []).angle, undefined);
}

function testThePaletteHasTwoMainButtonsAndAnEvenGridOfTheRest() {
  reset();
  const container = makeEl("div");
  S.started = true;
  P.init(container, api);
  const [main, rest] = S.ui.palette.children;
  assert.deepEqual(main.children.map((b) => b._attrs["data-kind"]), ["gate", "tree"], "a row of their own");
  assert.equal(rest.children.length, 12, "the others together");
  const css = require("node:fs").readFileSync(path.join(__dirname, "..", "map-entities.css"), "utf8");
  assert.ok(!/\.ent-kind\.is-main[^}]*span/.test(css), "no button spans columns, which is what left holes");
  const counts = css.split("\n").filter((line) => /\.ent-palette \{/.test(line))
    .map((line) => /grid-template-columns: repeat\((\d+)/.exec(line)).filter(Boolean).map((m) => Number(m[1]));
  counts.forEach((n) => assert.equal(12 % n, 0, n + " columns leave no hole"));
  assert.ok(counts.length >= 4, "a count for every width");
  P.destroy(container);
}

function testTheTabChoiceLivesInSettingsNotInTheView() {
  reset();
  const container = makeEl("div");
  S.started = true;
  P.init(container, api);
  const head = container.children[0].children[1];
  assert.equal(head.children.length, 1, "the Place header holds only its caption");
  assert.equal(registered.spec.tab, true, "the plugin can have a tab: Settings, Plugins, Own tab");
  P.destroy(container);
}

function testTheViewBuildsAndTearsDownCleanly() {
  reset();
  const container = makeEl("div");
  S.started = true;
  P.init(container, api);
  assert.ok(S.ui, "built");
  assert.equal(container.children.length, 1);
  P._startPlacing("tree");
  assert.equal(S.placing.kind, "tree");
  P.destroy(container);
  assert.equal(S.ui, null, "the view's hooks are dropped");
  assert.equal(S.placing, null, "a placing in progress does not outlive the view");
  assert.equal(picks[picks.length - 1].live, false, "and its pick is dropped");
  assert.equal(container.textContent, "");
  assert.doesNotThrow(() => P.destroy(null));
}

const tests = [
  testItRegistersAsATabbedPluginWithOptions,
  testCoordinatesAreReadTheWayTheyArePasted,
  testDistancesAndBearingsAreInMetres,
  testAStepAcrossTheGateCrossesItWhereItShouldBe,
  testAStepThatMissesTheGateDoesNotCrossIt,
  testACrossingIsCountedOnceWhenASampleLandsOnTheLine,
  testTheOpeningIsAHeightBand,
  testAZoneIsTouchedByAStepThatPassesThroughItBetweenSamples,
  testNewEntitiesCarryTheirKindsDefaults,
  testWhatIsLoadedIsMadeSafe,
  testFlyingThroughAGateIsRecordedOnceAndAnnounced,
  testCrossingOutsideTheOpeningIsAMissNotAPass,
  testATreeIsOnlyHitBelowItsTop,
  testAZoneIsEnteredOnceUntilTheAircraftLeaves,
  testAZoneWithoutAnAlertCountsButStaysQuiet,
  testOnlyArmedFlightIsMeasuredByDefault,
  testBadSamplesAndJumpsProveNothing,
  testNotificationsFollowTheOption,
  testResultsDriveTheColourOnTheMap,
  testStartingDrawsWhatWasSavedAndWatchesTheAircraft,
  testOnlyTheSelectedEntityCanBeMoved,
  testDraggingAPostMovesTheGateAndSavesIt,
  testPlacingAGateTakesTwoClicksAndKeepsGoing,
  testPlacingTreesIsOneClickEachUntilStopped,
  testEscapeEndsPlacingAndClearsTheHalfPlacedGate,
  testPickingAnotherKindReplacesThePlacing,
  testMapNotReadyIsSaidNotSilent,
  testRemovingTakesEveryPieceOffTheMap,
  testAnEntityWithoutARadiusDrawsNoArea,
  testTheEditorFollowsWhatIsAddedAndRemoved,
  testThePaletteHasTwoMainButtonsAndAnEvenGridOfTheRest,
  testEveryRowHasItsOwnTrashThatDeletesAtOnce,
  testADeleteCanBeTakenBackInPlace,
  testASectorIsAnAngleFacingADirection,
  testASectorRingIsTheCentreAndAnArcOfTheRadius,
  testASectorOnlyCatchesTheAircraftInsideIt,
  testASectorIsDrawnAsAPieAndAFullCircleAsACircle,
  testSectorsAreSavedAndLoadedSafely,
  testTheTabChoiceLivesInSettingsNotInTheView,
  testTheViewBuildsAndTearsDownCleanly,
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
  console.error(`\n${failed}/${tests.length} map entities test(s) FAILED`);
  process.exit(1);
}
console.log(`\nAll ${tests.length} map entities tests passed.`);
process.exit(0);
