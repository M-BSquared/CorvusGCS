"use strict";

/**
 * Frontend tests for the drawn transmitter (Corvus.rcTransmitter).
 *
 * Same shape as tests/test_frontend_control.js: stub the globals, require the
 * source, assert on the rendered tree. No browser, no runner.
 *
 * What is worth testing here is not that a rectangle appeared. It is the
 * arithmetic between a pulse width and a picture, because every one of those
 * steps is a place the drawing can confidently show the wrong thing:
 *
 *   - which slot a three-position switch lands in (a boundary off by one band
 *     lights the middle position for a switch that is fully up)
 *   - which way a reversed channel is drawn (the widget follows the hand, so
 *     a reversed channel MUST be inverted or it contradicts the thumb that
 *     moved it)
 *   - which gimbal axis a stick role sits on in each of the four stick modes
 *     (mode 2 is not universal, and a wrong mode points the calibration's
 *     arrow at the wrong thumb)
 *   - that Learn binds the channel that actually moved and not the one that
 *     jittered
 *
 * Run:
 *   node tests/test_frontend_rc_transmitter.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals.
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

window.addEventListener = () => {};
window.removeEventListener = () => {};
window.dispatchEvent = () => {};
global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
window.matchMedia = (query) => ({
  matches: false, media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});

// The Learn timeout is measured on the wall clock, so the clock is ours.
let clock = 1000;
Date.now = () => clock;

// An in-memory localStorage: the bindings are the operator's own model of
// their handset and the module must survive both a missing store and a
// corrupted one.
function makeStorage() {
  const data = {};
  return {
    data,
    getItem: (k) => (k in data ? data[k] : null),
    setItem: (k, v) => { data[k] = String(v); },
    removeItem: (k) => { delete data[k]; },
  };
}
let storage = makeStorage();
Object.defineProperty(window, "localStorage", {
  configurable: true,
  get() { return storage; },
});

// ---------------------------------------------------------------------------
// Minimal DOM stub (mirrors tests/test_frontend_control.js).
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {}, style: {},
    type: "", hidden: false, disabled: false, value: "", id: "",
    _attrs: {}, _listeners: {}, _isEl: true,
  };
  let _html = "";
  Object.defineProperty(e, "innerHTML", {
    get() { return _html; },
    set(v) { _html = String(v); if (_html === "") e.children.length = 0; },
  });
  e.classList = {
    add(c) {
      const s = e.className.split(/\s+/).filter(Boolean);
      if (!s.includes(c)) s.push(c);
      e.className = s.join(" ");
    },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    contains(c) { return e.className.split(/\s+/).includes(c); },
    toggle(c, force) {
      const has = e.classList.contains(c);
      const next = force === undefined ? !has : !!force;
      if (next) e.classList.add(c); else e.classList.remove(c);
      return next;
    },
  };
  e.appendChild = (c) => { c.parentNode = e; e.children.push(c); return c; };
  e.removeChild = (c) => {
    const i = e.children.indexOf(c);
    if (i >= 0) e.children.splice(i, 1);
    c.parentNode = null;
    return c;
  };
  e.insertBefore = (c, ref) => {
    const i = e.children.indexOf(ref);
    c.parentNode = e;
    if (i < 0) e.children.push(c); else e.children.splice(i, 0, c);
    return c;
  };
  e.setAttribute = (k, v) => {
    e._attrs[k] = String(v);
    if (k === "class") e.className = String(v);
    if (k.indexOf("data-") === 0) e.dataset[k.slice(5)] = String(v);
  };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (type, cb) => { (e._listeners[type] = e._listeners[type] || []).push(cb); };
  e.removeEventListener = () => {};
  e.dispatchEvent = (ev) => {
    ((e._listeners[ev && ev.type]) || []).forEach((cb) => cb(ev));
    return true;
  };
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  return e;
}

function querySel(children, sel) {
  const out = [];
  const wantTag = sel && sel[0] !== ".";
  const classes = sel ? sel.split(".").filter(Boolean) : [];
  function walk(list) {
    for (const c of list) {
      if (!c || !c._isEl) continue;
      const ok = wantTag ? c.tagName === sel.toUpperCase()
        : classes.every((cl) => c.className.split(/\s+/).includes(cl));
      if (ok) out.push(c);
      if (c.children) walk(c.children);
    }
  }
  walk(children);
  return out;
}

global.document = {
  createElement: makeEl,
  createElementNS: (_ns, tag) => makeEl(tag),
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
};

// ---------------------------------------------------------------------------
// Fake telemetry — only what the widget's Learn touches.
// ---------------------------------------------------------------------------
let telemetryState = { rc_live: false, rc_channels: [] };
Corvus.telemetry = {
  getState() { return telemetryState; },
  postAction() { return Promise.resolve({}); },
  requestJson() { return Promise.resolve({}); },
};

require("../src/js/ui.js");
require("../src/js/rc-transmitter.js");

const TX = Corvus.rcTransmitter;

// Helpers --------------------------------------------------------------------

/** Every element in `root` carrying `data-control` for `id`. A bindable
 *  control has up to two: the shape on the drawing, and its margin callout. */
function nodesFor(root, id) {
  const out = [];
  function walk(list) {
    for (const e of list) {
      if (!e || !e._isEl) continue;
      if (e.dataset && e.dataset.control === id) out.push(e);
      if (e.children) walk(e.children);
    }
  }
  walk([root]);
  return out;
}

/** The first node for a control — the drawn shape where it has one. */
function controlNode(root, id) {
  return nodesFor(root, id)[0] || null;
}

/** The position a drawn control is showing, or -1 when it is showing none.
 *  Read off the rendered node rather than from the widget's own state, so the
 *  assertion is about what an operator can see. */
function positionOf(root, id) {
  const group = controlNode(root, id);
  if (!group) return -1;
  const value = group.dataset.position;
  return value === "" || value == null ? -1 : Number(value);
}

/** The callout group for a control — the margin label and its leader line. */
function calloutNode(root, id) {
  return nodesFor(root, id).find(
    (e) => e.className && e.className.indexOf("rc-tx-callout") >= 0) || null;
}

/** The live reading shown in a control's margin label. */
function calloutValue(root, id) {
  const callout = calloutNode(root, id);
  if (!callout) return "";
  const value = callout.children.find(
    (c) => c.className && c.className.indexOf("rc-tx-callout-value") >= 0);
  return value ? value.textContent : "";
}

function frame(channels, extra) {
  return Object.assign({ rc_live: true, rc_channels: channels }, extra || {});
}

/** A widget with channel 5 bound to SA, already fed a calibration for it. */
function withSA(opts) {
  storage = makeStorage();
  storage.setItem("corvus.rc.transmitter.bindings.v2",
    JSON.stringify({ SA: { channel: 5 } }));
  const widget = TX.create(Object.assign({ interactive: true }, opts || {}));
  widget.setChannelInfo({ 5: { min: 1000, max: 2000, reversed: false } });
  return widget;
}

// ===========================================================================
// PART A — the arithmetic
// ===========================================================================

function testPositionBandsSplitEvenly() {
  // Two positions: the boundary is the middle, nowhere else.
  assert.equal(TX.positionIndex(0, 2), 0);
  assert.equal(TX.positionIndex(0.49, 2), 0);
  assert.equal(TX.positionIndex(0.51, 2), 1);
  assert.equal(TX.positionIndex(1, 2), 1);

  // Three: equal thirds, and the extremes land in the extreme slots rather
  // than one short of them.
  assert.equal(TX.positionIndex(0, 3), 0);
  assert.equal(TX.positionIndex(0.5, 3), 1);
  assert.equal(TX.positionIndex(1, 3), 2);
  assert.equal(TX.positionIndex(0.34, 3), 1);
  assert.equal(TX.positionIndex(0.32, 3), 0);

  // Out-of-range input is clamped, never wrapped: a pulse past the measured
  // endpoint is still the top position.
  assert.equal(TX.positionIndex(1.4, 3), 2);
  assert.equal(TX.positionIndex(-2, 3), 0);
}

function testEveryStickModePlacesFourDistinctRoles() {
  Object.keys(TX.MODES).forEach((mode) => {
    const roles = TX.axisRoles(mode);
    const ids = Object.keys(roles).sort();
    assert.deepEqual(ids, ["left_x", "left_y", "right_x", "right_y"],
      "mode " + mode + " names all four gimbal axes");
    const named = Object.keys(roles).map((id) => roles[id]).sort();
    assert.deepEqual(named, ["pitch", "roll", "throttle", "yaw"],
      "mode " + mode + " places each stick role exactly once");
  });

  // The two that matter in the field, stated explicitly rather than derived:
  // mode 2 is throttle-left, mode 1 is throttle-right.
  assert.equal(TX.axisRoles(2).left_y, "throttle");
  assert.equal(TX.axisRoles(1).right_y, "throttle");
  // Throttle and pitch are always on a vertical axis, roll and yaw on a
  // horizontal one — which is what lets the wizard's arrow direction be a
  // property of the ROLE and not of the mode.
  Object.keys(TX.MODES).forEach((mode) => {
    const roles = TX.axisRoles(mode);
    ["throttle", "pitch"].forEach((role) => {
      const axis = Object.keys(roles).find((id) => roles[id] === role);
      assert.ok(axis.endsWith("_y"), role + " is vertical in mode " + mode);
    });
    ["roll", "yaw"].forEach((role) => {
      const axis = Object.keys(roles).find((id) => roles[id] === role);
      assert.ok(axis.endsWith("_x"), role + " is horizontal in mode " + mode);
    });
  });

  // An unknown mode degrades to 2 rather than throwing or drawing nothing.
  assert.deepEqual(TX.axisRoles(9), TX.MODES[2]);
}

function testBindingsRoundTripAndRejectJunk() {
  storage = makeStorage();
  assert.deepEqual(TX.loadBindings(), {}, "nothing stored is no bindings");

  TX.saveBindings({ SA: { channel: 5, positions: 3 } });
  assert.deepEqual(TX.loadBindings(), { SA: { channel: 5, positions: 3 } });

  // A stored entry for a control this layout no longer draws, a channel of 0,
  // and outright garbage are all dropped rather than carried into the widget.
  storage.setItem("corvus.rc.transmitter.bindings.v2", JSON.stringify({
    SA: { channel: 7 },
    SZ: { channel: 3 },
    SB: { channel: 0 },
    SC: "nonsense",
  }));
  assert.deepEqual(TX.loadBindings(), { SA: { channel: 7 } });

  storage.setItem("corvus.rc.transmitter.bindings.v2", "{not json");
  assert.deepEqual(TX.loadBindings(), {}, "a corrupt store is an empty one");
}

function testModePersistsAndDefaultsToTwo() {
  storage = makeStorage();
  assert.equal(TX.loadMode(), 2, "mode 2 is the default");
  TX.saveMode(1);
  assert.equal(TX.loadMode(), 1);
  storage.setItem("corvus.rc.transmitter.mode.v1", "17");
  assert.equal(TX.loadMode(), 2, "a mode that does not exist falls back to 2");
}

// ===========================================================================
// PART B — the drawing follows the stream
// ===========================================================================

function testAnUnboundControlStaysDark() {
  storage = makeStorage();
  const widget = TX.create({ interactive: true });
  widget.update(frame([1500, 1500, 1500, 1500, 1900]));

  const sa = controlNode(widget.el, "SA");
  assert.equal(sa.dataset.bound, "", "SA has no channel, so it is not bound");
  assert.equal(sa.dataset.on, "", "and nothing lights, however the channels move");
  assert.equal(positionOf(widget.el, "SA"), -1);
}

function testASwitchLightsThePositionItIsIn() {
  const widget = withSA();

  widget.update(frame([1500, 1500, 1500, 1500, 1000]));
  assert.equal(positionOf(widget.el, "SA"), 0, "the low pulse is the down position");
  assert.equal(controlNode(widget.el, "SA").dataset.on, "",
    "down is the resting position and does not read as ON");

  widget.update(frame([1500, 1500, 1500, 1500, 1500]));
  assert.equal(positionOf(widget.el, "SA"), 1, "mid travel is the middle position");
  assert.equal(controlNode(widget.el, "SA").dataset.on, "1");

  widget.update(frame([1500, 1500, 1500, 1500, 2000]));
  assert.equal(positionOf(widget.el, "SA"), 2, "full travel is the up position");
  assert.equal(controlNode(widget.el, "SA").dataset.on, "1");
}

function testASwitchWithoutASignalLightsNothing() {
  const widget = withSA();
  widget.update(frame([1500, 1500, 1500, 1500, 2000]));
  assert.equal(positionOf(widget.el, "SA"), 2);

  // The transmitter is switched off: the channels stop arriving. A picture
  // frozen at the last position read is a switch that looks like it is still
  // being held.
  widget.update({ rc_live: false, rc_channels: [] });
  assert.equal(positionOf(widget.el, "SA"), -1, "no signal shows no position");
  assert.equal(controlNode(widget.el, "SA").dataset.signal, "");
  assert.equal(calloutValue(widget.el, "SA"), "CH5 · no signal",
    "and the label says so rather than going blank");
}

function testAReversedChannelIsDrawnAsTheHandMovedIt() {
  const widget = withSA();
  widget.setChannelInfo({ 5: { min: 1000, max: 2000, reversed: true } });

  // RC5_REV = -1 says this channel's pulse FALLS as the control goes the way
  // PX4 calls positive. The drawing follows the hand, so the low pulse has to
  // be drawn as the up position — otherwise the picture contradicts the thumb.
  widget.update(frame([1500, 1500, 1500, 1500, 1000]));
  assert.equal(positionOf(widget.el, "SA"), 2);
  widget.update(frame([1500, 1500, 1500, 1500, 2000]));
  assert.equal(positionOf(widget.el, "SA"), 0);
}

function testAnUncalibratedChannelStillReadsOffTheDisplayScale() {
  storage = makeStorage();
  storage.setItem("corvus.rc.transmitter.bindings.v2",
    JSON.stringify({ SH: { channel: 6 } }));
  const widget = TX.create({ interactive: true });
  // No setChannelInfo at all: the vehicle has never calibrated channel 6.
  widget.update(frame([0, 0, 0, 0, 0, 2000]));
  assert.equal(positionOf(widget.el, "SH"), 1,
    "a two-position switch at the top of the display scale is still up");
  widget.update(frame([0, 0, 0, 0, 0, 1000]));
  assert.equal(positionOf(widget.el, "SH"), 0);
}

function testStickAxesComeFromTheVehicleMappingAndTheMode() {
  storage = makeStorage();
  const widget = TX.create({ interactive: true });
  widget.setMode(2);
  widget.setStickChannels({ throttle: 3, roll: 1, pitch: 2, yaw: 4 });
  widget.update(frame([1500, 1500, 1500, 1500]));

  let map = widget.channelMap();
  assert.equal(map.left_y, 3, "mode 2 puts throttle on the left stick's vertical axis");
  assert.equal(map.right_x, 1, "and roll on the right stick's horizontal one");

  // Same vehicle, a mode-1 transmitter: the channels did not move, the thumbs
  // did. Getting this wrong points the calibration arrow at the wrong stick.
  widget.setMode(1);
  map = widget.channelMap();
  assert.equal(map.right_y, 3, "mode 1 puts throttle on the right stick");
  assert.equal(map.left_y, 2, "and pitch on the left one");
}

function testTheHandsetsOwnMenuButtonsAreNotBindable() {
  // PAGE / MENU / EXIT and the scroll wheel run the transmitter's menus and
  // never reach a channel. Offering to bind one would offer a mapping that
  // can never fire.
  const ids = TX.LAYOUT.controls.map((c) => c.id);
  TX.LAYOUT.nav.labels.concat([TX.LAYOUT.wheel.label]).forEach((name) => {
    assert.ok(ids.indexOf(name) < 0, name + " is drawn but not bindable");
  });
  assert.ok(TX.LAYOUT.nav.labels.indexOf("MENU") >= 0,
    "it is still drawn, or the thing does not look like a radio");
}

function testEveryBindableControlHasACalloutAndNoTwoShareARow() {
  // The callout is the label in the margin. It is the only click target a
  // stick axis has, and on a drawing this dense it is the practical one for
  // everything else, so a control without one is a control out of reach.
  const seen = {};
  TX.LAYOUT.controls.forEach((control) => {
    const spec = control.callout;
    assert.ok(spec, control.id + " has a callout");
    assert.ok(["left", "right", "top-left", "top-right"].indexOf(spec.side) >= 0,
      control.id + " sits in a margin the drawing actually has");
    const key = spec.side + ":" + spec.y;
    assert.ok(!seen[key], "nothing else is drawn at " + key + " (" + control.id + ")");
    seen[key] = control.id;
    assert.ok(Array.isArray(spec.target) && spec.target.length === 2,
      control.id + " leader line lands somewhere");
  });
}

function testTheHandleLeansFurtherForEveryPositionUp() {
  // Position is carried by the angle of the handle, so the angles have to be
  // monotonic: a three-position switch whose middle leant further than its top
  // would still light, and still be wrong.
  const three = [0, 1, 2].map((i) => TX.leverAngle(i, 3));
  assert.ok(three[0] < three[1] && three[1] < three[2], "down < middle < up");
  const two = [0, 1].map((i) => TX.leverAngle(i, 2));
  assert.deepEqual([two[0], two[1]], [three[0], three[2]],
    "a two-position switch uses the same extremes, so the drawings agree");
  // The whole sweep stays in the upper quadrant — a handle that lay flat
  // across the shoulder stopped reading as a switch on a radio — and the
  // swing between the extremes is wide enough to tell apart at a glance.
  assert.ok(two[0] > 0 && two[1] <= 90, "every position stands up out of the case");
  assert.ok(two[1] - two[0] >= 45, "with a visible swing between down and up");
}

// ===========================================================================
// PART C — selection, learning, prompting
// ===========================================================================

function testSelectingAControlOpensItsInspector() {
  const widget = withSA();
  widget.update(frame([1500, 1500, 1500, 1500, 2000]));

  const picked = [];
  const w2 = TX.create({ interactive: true, onSelect: (id) => picked.push(id) });
  w2.update(frame([1500]));
  const sa = controlNode(w2.el, "SA");
  sa.dispatchEvent({ type: "click" });
  assert.deepEqual(picked, ["SA"]);
  assert.equal(w2.selected(), "SA");

  // Clicking the same control again closes it — the inspector is a detail
  // view, not a mode you have to escape from.
  sa.dispatchEvent({ type: "click" });
  assert.equal(w2.selected(), null);
}

function testAStickAxisIsReachedThroughItsMarginLabel() {
  // A gimbal carries two channels and cannot say which one a click on it
  // meant, so the axes are selected from their margin labels. If that stops
  // working there is no way to inspect a stick axis at all.
  storage = makeStorage();
  const widget = TX.create({ interactive: true });
  widget.setStickChannels({ throttle: 3, roll: 1, pitch: 2, yaw: 4 });
  widget.update(frame([1500, 1500, 1500, 1500]));

  assert.equal(controlNode(widget.el, "left_y"), calloutNode(widget.el, "left_y"),
    "an axis has no shape of its own on the drawing — only a label");
  calloutNode(widget.el, "left_y").dispatchEvent({ type: "click" });
  assert.equal(widget.selected(), "left_y");
  assert.equal(calloutValue(widget.el, "left_y"), "CH3 · 50%",
    "and the label carries the live reading, not just the name");
}

function testANonInteractiveWidgetNeverSelects() {
  // The calibration wizard builds it this way: a measurement must not be able
  // to write mappings underneath itself.
  storage = makeStorage();
  const widget = TX.create({ interactive: false });
  widget.update(frame([1500]));
  const sa = controlNode(widget.el, "SA");
  sa.dispatchEvent({ type: "click" });
  assert.equal(widget.selected(), null);
}

function testLearnBindsTheChannelThatMoved() {
  storage = makeStorage();
  const widget = TX.create({ interactive: true });
  telemetryState = frame([1500, 1500, 1500, 1500, 1500, 1500]);
  widget.update(telemetryState);
  widget.select("SB");

  const learn = buttonByLabel(widget.el, "Learn channel");
  assert.ok(learn, "an unbound control offers Learn");
  learn.dispatchEvent({ type: "click" });

  // Channel 4 twitches; channel 6 is flipped. The winner is the one that
  // actually travelled, not the first one that moved at all.
  widget.update(frame([1500, 1500, 1500, 1508, 1500, 2000]));
  assert.deepEqual(widget.bindings().SB, { channel: 6 });
  assert.deepEqual(TX.loadBindings().SB, { channel: 6 },
    "and it survives a reload, because it describes the handset");
}

function testLearnIgnoresJitterAndGivesUp() {
  storage = makeStorage();
  clock = 1000;
  const widget = TX.create({ interactive: true });
  telemetryState = frame([1500, 1500, 1500]);
  widget.update(telemetryState);
  widget.select("SC");
  buttonByLabel(widget.el, "Learn channel").dispatchEvent({ type: "click" });

  widget.update(frame([1504, 1497, 1503]));
  assert.equal(widget.bindings().SC, undefined,
    "a receiver sitting still never wins a Learn");

  clock += 9000;
  widget.update(frame([1504, 1497, 1503]));
  assert.equal(widget.bindings().SC, undefined);
  assert.ok(buttonByLabel(widget.el, "Learn channel"),
    "and the button is back, rather than stuck reading Cancel");
}

function testLearnWithoutASignalRefusesRatherThanGuessing() {
  storage = makeStorage();
  const widget = TX.create({ interactive: true });
  telemetryState = { rc_live: false, rc_channels: [] };
  widget.update(telemetryState);
  widget.select("SD");
  buttonByLabel(widget.el, "Learn channel").dispatchEvent({ type: "click" });
  assert.equal(widget.bindings().SD, undefined);
  assert.ok(widget.el.querySelectorAll(".params-row-status")
    .some((s) => s.textContent === "no RC signal"));
}

function testForgettingDropsEveryLearnedControl() {
  const widget = withSA();
  widget.update(frame([1500, 1500, 1500, 1500, 2000]));
  assert.equal(positionOf(widget.el, "SA"), 2);

  widget.forgetAll();
  widget.update(frame([1500, 1500, 1500, 1500, 2000]));
  assert.deepEqual(widget.bindings(), {});
  assert.deepEqual(TX.loadBindings(), {});
  assert.equal(positionOf(widget.el, "SA"), -1, "a forgotten control goes dark");
}

function testThePromptMarksWhatToMoveAndWhatIsDone() {
  const widget = withSA();
  widget.setStickChannels({ throttle: 3, roll: 1, pitch: 2, yaw: 4 });
  widget.setMode(2);

  widget.setPrompt({ controls: ["left_y"], gimbal: "left", direction: "up" });
  widget.update(frame([1500, 1500, 1500, 1500, 1500]));
  assert.equal(controlNode(widget.el, "left_y").dataset.prompted, "1");
  assert.equal(controlNode(widget.el, "right_y").dataset.prompted, "");

  // The sweep step's second signal: which controls have already been moved
  // far enough, so what is left is visible rather than counted.
  widget.setPrompt({ controls: ["SA", "left_y"], swept: ["SA"] });
  widget.update(frame([1500, 1500, 1500, 1500, 1500]));
  assert.equal(controlNode(widget.el, "SA").dataset.swept, "1");
  assert.equal(controlNode(widget.el, "left_y").dataset.swept, "");
  assert.equal(controlNode(widget.el, "left_y").dataset.prompted, "1");
}

function testTheFunctionPanelIsOnlyOfferedForALearnedChannel() {
  storage = makeStorage();
  const calls = [];
  const widget = TX.create({
    interactive: true,
    renderFunctions: (channel, controlId, armed) => {
      calls.push({ channel, controlId, armed });
      const node = document.createElement("div");
      node.className = "fn-panel";
      return node;
    },
  });
  widget.update(frame([1500, 1500, 1500, 1500, 1500]));

  widget.select("SB");
  assert.deepEqual(calls, [], "an unbound control has no channel to give a function to");

  telemetryState = frame([1500, 1500, 1500, 1500, 1500]);
  buttonByLabel(widget.el, "Learn channel").dispatchEvent({ type: "click" });
  widget.update(frame([1500, 1500, 2000, 1500, 1500]));
  assert.deepEqual(calls, [{ channel: 3, controlId: "SB", armed: false }]);
  assert.ok(widget.el.querySelector(".fn-panel"));

  // Armed is handed through, because PX4 refuses the write and a picker left
  // enabled over an armed vehicle is the widget lying about what it can do.
  widget.update(frame([1500, 1500, 2000, 1500, 1500], { armed: true }));
  assert.equal(calls[calls.length - 1].armed, true);
}

function testTheDrawingIsSymmetricAboutItsCentreLine() {
  // A handset is symmetric, so a drawing of one that is not reads as a
  // mistake before it reads as anything else — and the two label columns are
  // only level with each other if the things they point at are. Every
  // asymmetry this catches was in the drawing once: the antenna ten units
  // left of the case, the trim pair eleven right of it.
  const centre = TX.LAYOUT.centre;
  const mirror = (x) => 2 * centre - x;
  const near = (a, b, what) => assert.ok(Math.abs(a - b) < 0.51,
    what + " is centred (" + a + " vs " + b + ")");

  const { antenna, brand, chin, screen, modeText, nav, wheel, body } = TX.LAYOUT;
  near(antenna.mast.x + antenna.mast.w / 2, centre, "the antenna mast");
  near(antenna.hinge.x + antenna.hinge.w / 2, centre, "the antenna hinge");
  near(brand.x, centre, "the wordmark");
  near(chin.x + chin.w / 2, centre, "the chin");
  near(screen.x + screen.w / 2, centre, "the screen");
  near(modeText.x, centre, "the mode text");
  near((nav.cx + wheel.cx) / 2, centre, "the menu pad and the wheel");

  // The trims come in pairs — two upright between the gimbals, one under
  // each — and each pair has to straddle the centre line.
  const upright = TX.LAYOUT.trims.filter((t) => t.axis === "v")
    .map((t) => t.x + t.w / 2).sort((a, b) => a - b);
  assert.equal(upright.length, 2, "two upright trims");
  near((upright[0] + upright[1]) / 2, centre, "the upright trim pair");

  // Every x in the shell outline has its mirror image in the same outline.
  const xs = String(body).match(/-?\d+(?:\.\d+)?/g)
    .filter((_n, i) => i % 2 === 0).map(Number);
  xs.forEach((x) => {
    assert.ok(xs.some((other) => Math.abs(other - mirror(x)) < 0.51),
      "the outline's " + x + " has a mirror at " + mirror(x));
  });

  // And every control on one side has a counterpart at its mirror image, in
  // the same row of the opposite margin.
  const by = {};
  TX.LAYOUT.controls.forEach((c) => {
    const x = c.kind === "axis"
      ? TX.LAYOUT.gimbals.find((g) => g.id === c.gimbal).cx
      : c.cx;
    // Keyed by the row as well as the column, because a gimbal's two axes
    // share its centre and are told apart by which margin row they label.
    const key = Math.round(Math.min(x, mirror(x))) + "@" + c.callout.y;
    (by[key] ||= []).push({ c, x });
  });
  Object.keys(by).forEach((key) => {
    const pair = by[key];
    assert.equal(pair.length, 2, "control at " + key + " is one of a pair");
    near(pair[0].x, mirror(pair[1].x), pair[0].c.id + "/" + pair[1].c.id);
    assert.equal(pair[0].c.callout.y, pair[1].c.callout.y,
      pair[0].c.id + " and " + pair[1].c.id + " label on the same row");
    near(pair[0].c.callout.target[0], mirror(pair[1].c.callout.target[0]),
      pair[0].c.id + "'s leader lands opposite " + pair[1].c.id + "'s");
    assert.equal(pair[0].c.callout.target[1], pair[1].c.callout.target[1],
      pair[0].c.id + "'s leader lands level with " + pair[1].c.id + "'s");
  });

}

/** A Corvus.ui.button by its visible label (the label lives in a child span). */
function buttonByLabel(root, label) {
  return root.querySelectorAll("button").find((btn) => (btn.children || [])
    .some((c) => c._isEl && c.textContent === label));
}

// ===========================================================================

function main() {
  const tests = [
    testPositionBandsSplitEvenly,
    testEveryStickModePlacesFourDistinctRoles,
    testBindingsRoundTripAndRejectJunk,
    testModePersistsAndDefaultsToTwo,
    testAnUnboundControlStaysDark,
    testASwitchLightsThePositionItIsIn,
    testASwitchWithoutASignalLightsNothing,
    testAReversedChannelIsDrawnAsTheHandMovedIt,
    testAnUncalibratedChannelStillReadsOffTheDisplayScale,
    testStickAxesComeFromTheVehicleMappingAndTheMode,
    testTheHandsetsOwnMenuButtonsAreNotBindable,
    testEveryBindableControlHasACalloutAndNoTwoShareARow,
    testTheDrawingIsSymmetricAboutItsCentreLine,
    testTheHandleLeansFurtherForEveryPositionUp,
    testSelectingAControlOpensItsInspector,
    testAStickAxisIsReachedThroughItsMarginLabel,
    testANonInteractiveWidgetNeverSelects,
    testLearnBindsTheChannelThatMoved,
    testLearnIgnoresJitterAndGivesUp,
    testLearnWithoutASignalRefusesRatherThanGuessing,
    testForgettingDropsEveryLearnedControl,
    testThePromptMarksWhatToMoveAndWhatIsDone,
    testTheFunctionPanelIsOnlyOfferedForALearnedChannel,
  ];
  tests.forEach((t) => { t(); console.log("  ok " + t.name); });
  console.log("\n" + tests.length + " passed — Setup > Radio Control > transmitter");
}

main();
