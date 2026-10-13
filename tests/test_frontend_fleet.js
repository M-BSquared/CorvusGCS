"use strict";

/**
 * Several vehicles in the frontend: js/fleet.js, the hand-over between two
 * vehicles in js/telemetry.js, and the track that follows its vehicle in
 * js/map.js.
 *
 * Run:
 *   node tests/test_frontend_fleet.js
 */

const assert = require("node:assert/strict");

const animationFrames = [];
const eventSources = [];
global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
window.requestAnimationFrame = (cb) => animationFrames.push(cb);
window.cancelAnimationFrame = () => {};
window.addEventListener = () => {};
window.dispatchEvent = () => {};
window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;
window.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
const store = new Map();
window.localStorage = {
  getItem: (k) => (store.has(k) ? store.get(k) : null),
  setItem: (k, v) => store.set(k, String(v)),
  removeItem: (k) => store.delete(k),
};

class FakeEventSource {
  constructor(url) { this.url = url; this.listeners = new Map(); eventSources.push(this); }
  addEventListener(type, cb) { this.listeners.set(type, cb); }
  emit(type, data) { this.listeners.get(type)?.({ data }); }
  close() {}
}
global.EventSource = FakeEventSource;

const elementStub = () => ({
  innerHTML: "", value: "", hidden: false, disabled: false, textContent: "",
  classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } },
  style: {
    props: {},
    setProperty(k, v) { this.props[k] = v; },
    removeProperty(k) { delete this.props[k]; },
  },
  querySelector() { return null; }, querySelectorAll() { return []; },
  appendChild() {}, setAttribute() {}, addEventListener() {},
  dataset: {},
});
global.document = {
  getElementById() { return elementStub(); },
  createElement: () => elementStub(),
  createElementNS: () => elementStub(),
  querySelectorAll: () => [],
  addEventListener() {},
};
if (typeof global.performance === "undefined") global.performance = { now: () => Date.now() };

require("../src/js/ui.js");
require("../src/js/telemetry.js");
require("../src/js/fleet.js");
require("../src/js/anim.js");
require("../src/js/map-overlays.js");
require("../src/js/map-fleet.js");
require("../src/js/map.js");

const fleet = Corvus.fleet;

function flush() {
  animationFrames.splice(0).forEach((cb) => cb(Date.now()));
}

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------
assert.equal(fleet.rgbTriplet("#2F7DF6"), "47 125 246");
assert.equal(fleet.rgbTriplet("blue"), "");
assert.equal(fleet.tint("#000000", 0.5), "#808080");
assert.equal(fleet.tint("#2F7DF6", 0), "#2F7DF6");
assert.equal(fleet.tint("#2F7DF6", 1), "#FFFFFF");

const el = elementStub();
fleet.paintElement(el, "#1FA971");
assert.equal(el.style.props["--vehicle"], "#1FA971");
assert.equal(el.style.props["--vehicle-rgb"], "31 169 113");
fleet.paintElement(el, "");
assert.deepEqual(el.style.props, {}, "no colour hands the marker back to the theme");

// ---------------------------------------------------------------------------
// The list
// ---------------------------------------------------------------------------
assert.equal(fleet.isMulti(), false);
assert.equal(fleet.activeVehicle(), null);

const seen = [];
const off = fleet.subscribe((snap) => seen.push(snap));
fleet._take({
  active: "main:1", active_link: "main",
  links: [{ id: "main", label: "Main link", primary: true, connection: "udp:0.0.0.0:14550", vehicles: 1 }],
  vehicles: [{ id: "main:1", sysid: 1, name: "Vehicle 1", color: "", custom_name: false }],
  palette: ["#2F7DF6"],
});
assert.equal(fleet.isMulti(), false, "one vehicle on one link is not a fleet");
assert.equal(fleet.activeVehicle().id, "main:1");
assert.equal(fleet.colorOf("main:1"), "");

fleet._take({
  active: "main:1",
  links: [{ id: "main" }],
  vehicles: [
    { id: "main:1", sysid: 1, name: "Lead", color: "", custom_name: true },
    { id: "main:2", sysid: 2, name: "Vehicle 2", color: "#2F7DF6", custom_name: false },
  ],
});
assert.equal(fleet.isMulti(), true);
assert.equal(fleet.colorOf("main:2"), "#2F7DF6");
assert.equal(fleet.nameOf("main:1"), "Lead");
assert.equal(fleet.colorOf("nobody"), "");
fleet._take(null);
assert.equal(fleet.get().vehicles.length, 0, "a malformed event leaves an empty list, not a crash");
assert.ok(seen.length >= 3);
off();

// ---------------------------------------------------------------------------
// What the other vehicles said becomes a notification, once, and a page load
// does not replay the session's history
// ---------------------------------------------------------------------------
const told = [];
const realDispatch = window.dispatchEvent;
window.dispatchEvent = (event) => {
  if (event.type === "corvus:notification") told.push(event.detail);
};
const withEvents = (events) => ({
  active: "main:1", links: [{ id: "main" }, { id: "link-2" }],
  vehicles: [
    { id: "main:1", sysid: 1, name: "Lead" },
    { id: "link-2:1", sysid: 1, name: "Scout" },
  ],
  events,
});
fleet._take(withEvents([{ seq: 4, vehicle: "link-2:1", level: "warning", text: "old news" }]));
assert.deepEqual(told, [], "the first list is the baseline");
fleet._take(withEvents([
  { seq: 4, vehicle: "link-2:1", level: "warning", text: "old news" },
  { seq: 5, vehicle: "link-2:1", level: "critical", text: "Failsafe enabled" },
]));
assert.deepEqual(told, [{ level: "critical", message: "Scout: Failsafe enabled" }]);
fleet._take(withEvents([{ seq: 5, vehicle: "link-2:1", level: "critical", text: "Failsafe enabled" }]));
assert.equal(told.length, 1, "told once");
window.dispatchEvent = realDispatch;

// ---------------------------------------------------------------------------
// The hand-over: a switch between two connected vehicles reads as a
// disconnect, then a connect, to every panel
// ---------------------------------------------------------------------------
Corvus.telemetry.connect();
const source = eventSources[eventSources.length - 1];
const states = [];
Corvus.telemetry.subscribe((s) => states.push({ id: s.vehicle_id, connected: s.connected, armed: s.armed }));
flush();
states.length = 0;

source.emit("state", JSON.stringify({ vehicle_id: "main:1", connected: true, armed: true, position: [11.6, 48.0] }));
flush();
source.emit("state", JSON.stringify({ vehicle_id: "main:1", connected: true, armed: true, position: [11.6, 48.0] }));
flush();
assert.deepEqual(states.map((s) => s.connected), [true, true], "the same vehicle hands nothing over");

states.length = 0;
source.emit("state", JSON.stringify({ vehicle_id: "main:2", connected: true, armed: false, position: [11.7, 48.1] }));
flush();
assert.deepEqual(states, [
  { id: "main:1", connected: false, armed: false },
  { id: "main:2", connected: true, armed: false },
]);

source.emit("state", JSON.stringify({ vehicle_id: "main:3", connected: false }));
flush();
states.length = 0;
source.emit("state", JSON.stringify({ vehicle_id: "main:4", connected: true }));
flush();
assert.deepEqual(states, [{ id: "main:4", connected: true, armed: undefined }],
  "a vehicle that was not connected needs no gap before the next one");

// ---------------------------------------------------------------------------
// The track follows its vehicle
// ---------------------------------------------------------------------------
const map = Corvus.map;
map._resetTrackState();
map._noteVehicle({ vehicle_id: "main:1" });
assert.equal(map.trackOwner(), "main:1", "the first vehicle takes the track as it is");
map._updateTrack({ position: [11.60, 48.00], armed: true, boot_ms: 1000 });
map._updateTrack({ position: [11.601, 48.0], armed: true, boot_ms: 2000 });
assert.equal(map.getTrack().length, 2);

Corvus.mapFleet._restore();
assert.equal(map._noteVehicle({ vehicle_id: "main:2" }), true);
assert.equal(map.trackOwner(), "main:2");
assert.equal(map.getTrack().length, 0, "the new vehicle starts from its own track");
assert.equal(Corvus.mapFleet._tracks().get("main:1").current.length, 2, "the old one's went with it");

// A booted-later vehicle is not a reboot of the last one, and arming it is
// its own takeoff, not the last vehicle's.
map._updateTrack({ position: [11.70, 48.10], armed: false, boot_ms: 10 });
map._updateTrack({ position: [11.701, 48.10], armed: false, boot_ms: 20 });
assert.equal(map.getTrack().length, 2);
assert.equal(map.getEarlierFlights().length, 0);

assert.equal(map._noteVehicle({ vehicle_id: "main:1" }), true);
assert.equal(map.getTrack().length, 2, "switching back brings the first vehicle's track back");
assert.equal(Corvus.mapFleet._tracks().get("main:2").current.length, 2);

map._saveTrack();
const saved = JSON.parse(store.get("corvus.map.track"));
assert.equal(saved.owner, "main:1", "the saved track says whose it is");
map._resetTrackState();
map._restoreTrack();
assert.equal(map.trackOwner(), "main:1");

// The other vehicles' tracks: decimated, split at a takeoff, bounded.
const mf = Corvus.mapFleet;
mf.clearTracks();
assert.equal(mf._record("main:5", [11.5, 48.0], false), true);
assert.equal(mf._record("main:5", [11.5, 48.0], false), false, "a hover adds nothing");
mf._record("main:5", [11.501, 48.0], false);
mf._record("main:5", [11.502, 48.0], true);
assert.equal(mf._tracks().get("main:5").past.length, 1, "arming starts the next flight");
assert.equal(mf._record("main:5", [0, 0], true), false, "no fix is not a point");
mf._save();
mf._restore();
assert.equal(mf._tracks().get("main:5").past.length, 1, "the tracks survive a restart");
mf.clearTracks();
assert.equal(store.has("corvus.map.fleetTracks"), false);

// The paths are redrawn when something drawn changed, not on every snapshot:
// in 3D each setData re-renders the line into every terrain tile's texture.
{
  const sets = [];
  const fakeSource = { setData: (d) => sets.push(d) };
  global.maplibregl = global.maplibregl || {
    Marker: function () {
      return { setLngLat() { return this; }, addTo() { return this; }, remove() {},
        setRotation() { return this; } };
    },
  };
  mf.attach({
    addSource() {}, addLayer() {}, getLayer: () => undefined, setPaintProperty() {},
    getSource: () => fakeSource,
  });
  const quiet = { vehicles: [] };
  mf._onSnapshot(quiet);
  const first = sets.length;
  for (let i = 0; i < 5; i++) mf._onSnapshot(quiet);
  assert.equal(sets.length, first, "an unchanged snapshot redraws nothing");
  const moving = (lng) => ({ vehicles: [{ id: "main:7", name: "Seven", color: "#36c",
    position: [lng, 48.0], armed: true, stale: false, heading: 0 }] });
  mf._onSnapshot(moving(11.6));
  mf._onSnapshot(moving(11.601));
  assert.equal(sets.length, first + 2, "a vehicle that moved is drawn where it went");
  mf._onSnapshot(moving(11.601));
  assert.equal(sets.length, first + 2, "a hover is not a redraw");
  mf.clearTracks();
}

// ---------------------------------------------------------------------------
// Removing a vehicle: off the list, off the map, and it does not flash back
// ---------------------------------------------------------------------------
(async () => {
  const two = (extra) => ({
    active: "main:1",
    links: [{ id: "main", primary: true }, { id: "link-2", primary: false, connection: "udp:0.0.0.0:14560" }],
    vehicles: [
      { id: "main:1", sysid: 1, link: "main", name: "Lead", position: [11.6, 48.0] },
      { id: "link-2:1", sysid: 1, link: "link-2", name: "Scout", position: [11.7, 48.1], ...extra },
    ],
  });
  const posted = [];
  const realRequest = Corvus.telemetry.requestJson;
  Corvus.telemetry.requestJson = async (url, opts) => {
    posted.push({ url, body: JSON.parse(opts.body) });
    const after = two();
    after.vehicles = after.vehicles.slice(0, 1);
    after.links = after.links.slice(0, 1);
    return { ok: true, fleet: after };
  };
  try {
    mf.clearTracks();
    fleet._take(two());
    mf._onSnapshot(fleet.get());
    mf._onSnapshot(fleet.get());
    assert.ok(mf._tracks().has("link-2:1"));

    await fleet.removeVehicle("link-2:1", true);
    assert.deepEqual(posted[0], { url: "/api/fleet/vehicle/remove",
      body: { id: "link-2:1", confirm_armed: true } });
    assert.deepEqual(fleet.get().vehicles.map((v) => v.id), ["main:1"]);
    assert.equal(mf._tracks().has("link-2:1"), false, "its track went with it");

    // A snapshot published just before the removal arrives late.
    fleet._take(two());
    assert.deepEqual(fleet.get().vehicles.map((v) => v.id), ["main:1"], "no flash back");
    mf._onSnapshot(fleet.get());
    mf.stashTrack("link-2:1", { current: [[1, 1], [2, 2]], past: [] });
    assert.equal(mf._tracks().has("link-2:1"), false, "a late hand-over is dropped too");

    // Heard again once the hold is over (the operator opened its link again).
    const realNow = Date.now;
    Date.now = () => realNow() + 10000;
    try {
      fleet._take(two());
      assert.deepEqual(fleet.get().vehicles.map((v) => v.id), ["main:1", "link-2:1"]);
      mf._onSnapshot(fleet.get());
      mf._onSnapshot(fleet.get());
      assert.ok(mf._tracks().has("link-2:1"), "back on the map once heard again");
    } finally {
      Date.now = realNow;
    }
  } finally {
    Corvus.telemetry.requestJson = realRequest;
    mf.clearTracks();
  }
  console.log("test_frontend_fleet.js: all assertions passed");
})().catch((err) => { console.error(err); process.exit(1); });
