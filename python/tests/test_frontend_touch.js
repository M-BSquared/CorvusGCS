"use strict";

/**
 * Frontend tests for tablet mode (Corvus.touch, src/js/touch.js) and the
 * stylesheet that answers it (src/css/touch.css).
 *
 * Plain Node-runnable assertions (no browser, no test runner), same pattern as
 * the other tests/test_frontend_*.js files: stub the globals the module
 * touches, require the source, drive it and look at what it did.
 *
 * Run:
 *   node tests/test_frontend_touch.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const SRC = path.join(__dirname, "..", "src");

// ---------------------------------------------------------------------------
// Browser-ish globals.
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

const listeners = { document: {}, window: {} };
const dispatched = [];
window.addEventListener = (type, cb) => { (listeners.window[type] = listeners.window[type] || []).push(cb); };
window.removeEventListener = () => {};
window.dispatchEvent = (event) => { dispatched.push(event.type); return true; };

global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
global.Event = class Event {
  constructor(type) { this.type = type; }
};
global.MouseEvent = class MouseEvent {
  constructor(type, init = {}) { Object.assign(this, init); this.type = type; this.defaultPrevented = false; }
  preventDefault() { this.defaultPrevented = true; }
  stopImmediatePropagation() { this.stopped = true; }
};

let storage = {};
global.localStorage = {
  getItem(k) { return k in storage ? storage[k] : null; },
  setItem(k, v) { storage[k] = String(v); },
  removeItem(k) { delete storage[k]; },
};

let coarse = false;
window.matchMedia = (query) => ({ matches: query === "(pointer: coarse)" ? coarse : false });
Object.defineProperty(global, "navigator", {
  value: { maxTouchPoints: 0, vibrate: () => true },
  configurable: true,
});

// Timers run when the test says so, so a long press is a step, not a sleep.
let clock = 0;
let timers = [];
global.setTimeout = (fn, ms) => { const t = { fn, at: clock + ms }; timers.push(t); return t; };
global.clearTimeout = (t) => { timers = timers.filter((x) => x !== t); };
global.performance = { now: () => clock };
function advance(ms) {
  clock += ms;
  const due = timers.filter((t) => t.at <= clock);
  timers = timers.filter((t) => t.at > clock);
  due.forEach((t) => t.fn());
}

function el(tag, classes, parent) {
  const e = {
    nodeType: 1, tagName: tag.toUpperCase(), parentNode: parent || null,
    isConnected: true, received: [],
    classList: { contains: (c) => (classes || []).includes(c) },
    dispatchEvent(event) {
      e.received.push(event);
      (listeners.window[event.type] || []).forEach((cb) => cb(Object.assign(event, { target: e })));
      return true;
    },
    closest(sel) {
      const cls = sel.replace(/^\./, "");
      let n = e;
      while (n) { if (n.classList && n.classList.contains(cls)) return n; n = n.parentNode; }
      return null;
    },
  };
  return e;
}

const attrs = {};
const viewport = { content: "width=device-width, initial-scale=1.0" };
viewport.getAttribute = () => viewport.content;
viewport.setAttribute = (_k, v) => { viewport.content = v; };
let pointAt = null;
global.document = {
  documentElement: {
    setAttribute(k, v) { attrs[k] = String(v); },
    removeAttribute(k) { delete attrs[k]; },
    getAttribute(k) { return k in attrs ? attrs[k] : null; },
  },
  querySelector: (sel) => (sel.includes("viewport") ? viewport : null),
  addEventListener(type, cb) { (listeners.document[type] = listeners.document[type] || []).push(cb); },
  elementFromPoint: () => pointAt,
  fullscreenEnabled: false,
};

const fire = (where, type, event) =>
  (listeners[where][type] || []).forEach((cb) => cb(event));
const touch = (type, target, id = 1, x = 100, y = 100) =>
  fire("document", type, { type, pointerType: "touch", pointerId: id, target, clientX: x, clientY: y });

// The modules touch.js tells about the mode, recorded.
const tabCalls = [];
let railRefreshes = 0;
Corvus.panel = { setTabHidden: (id, hidden, reason) => { tabCalls.push([id, hidden, reason]); return true; } };
Corvus.sidenav = { refreshRail: () => { railRefreshes += 1; } };

require(path.join(SRC, "js", "touch.js"));
const T = Corvus.touch;

// ---------------------------------------------------------------------------
// The switch.
// ---------------------------------------------------------------------------
function testAFingerStartsInTabletModeAndAMouseDoesNot() {
  storage = {};
  coarse = true;
  assert.equal(T.applySaved(), true);
  assert.equal(attrs["data-touch"], "on");
  assert.equal(storage["corvus.touch"], undefined, "a detected start is not a choice");
  coarse = false;
  assert.equal(T.applySaved(), false);
  assert.equal(attrs["data-touch"], undefined);
}

function testTheAndroidAppStartsInTabletModeOnAnyScreen() {
  storage = {};
  coarse = false;
  Object.defineProperty(global, "navigator", {
    value: { maxTouchPoints: 0, userAgent: "Mozilla/5.0 (Linux; Android 14) CorvusAndroid/2026.10.01" },
    configurable: true,
  });
  try {
    assert.equal(T.applySaved(), true);
    assert.equal(storage["corvus.touch"], undefined, "a detected start is not a choice");
    storage = { "corvus.touch": "0" };
    assert.equal(T.applySaved(), false, "the operator can still switch it off");
    const html = fs.readFileSync(path.join(SRC, "index.html"), "utf8");
    assert.ok(html.includes("CorvusAndroid"), "the first paint makes the same default");
  } finally {
    Object.defineProperty(global, "navigator", {
      value: { maxTouchPoints: 0, vibrate: () => true },
      configurable: true,
    });
  }
}

function testTheOperatorsChoiceOutranksTheScreen() {
  coarse = true;
  storage = { "corvus.touch": "0" };
  assert.equal(T.applySaved(), false);
  coarse = false;
  storage = { "corvus.touch": "1" };
  assert.equal(T.applySaved(), true);
}

function testTheSwitchIsKeptOnThisDeviceAndToldAround() {
  storage = {};
  tabCalls.length = 0;
  dispatched.length = 0;
  const before = railRefreshes;
  T.setEnabled(true);
  assert.equal(storage["corvus.touch"], "1");
  assert.deepEqual(tabCalls.at(-1), ["ssh", true, "touch"], "the SSH tab goes, for tablet mode's own reason");
  assert.ok(railRefreshes > before, "the rail is rebuilt for the fullscreen entry");
  assert.ok(dispatched.includes("corvus:touchchange"));
  assert.ok(dispatched.includes("resize"), "everything that measures the layout is told");
  assert.match(viewport.content, /maximum-scale=1\.0/, "iOS must not zoom in on a focused field");

  T.setEnabled(false);
  assert.equal(storage["corvus.touch"], "0");
  assert.deepEqual(tabCalls.at(-1), ["ssh", false, "touch"]);
  assert.equal(viewport.content, "width=device-width, initial-scale=1.0");
  assert.equal(attrs["data-touch"], undefined);
}

// ---------------------------------------------------------------------------
// Long press.
// ---------------------------------------------------------------------------
function menusOn(target) {
  return target.received.filter((e) => e.type === "contextmenu" && e.corvusLongPress);
}

function testAHeldFingerIsARightClick() {
  T.setEnabled(true, false);
  const button = el("button", ["mc-btn"]);
  touch("pointerdown", button, 1, 40, 50);
  advance(T.LONG_PRESS_MS - 1);
  assert.equal(menusOn(button).length, 0, "not before the press is long");
  advance(1);
  const [menu] = menusOn(button);
  assert.ok(menu, "the held button gets a contextmenu");
  assert.equal(menu.clientX, 40);
  assert.equal(menu.clientY, 50);
  assert.equal(menu.button, 2);
  touch("pointerup", button);

  const click = new MouseEvent("click");
  fire("window", "click", click);
  assert.equal(click.defaultPrevented, true, "lifting the finger is not also a tap");
  const next = new MouseEvent("click");
  fire("window", "click", next);
  assert.equal(next.defaultPrevented, false, "only that one click");
}

function testMovingOrASecondFingerIsNotALongPress() {
  const button = el("button");
  touch("pointerdown", button, 1, 10, 10);
  touch("pointermove", button, 1, 30, 10);
  advance(T.LONG_PRESS_MS);
  assert.equal(menusOn(button).length, 0, "a drag is not a press");
  touch("pointerup", button, 1);

  touch("pointerdown", button, 1);
  touch("pointerdown", button, 2);
  advance(T.LONG_PRESS_MS);
  assert.equal(menusOn(button).length, 0, "two fingers are a pinch");
  touch("pointerup", button, 1);
  touch("pointerup", button, 2);
}

function testATapIsStillATap() {
  const button = el("button");
  touch("pointerdown", button);
  advance(100);
  touch("pointerup", button);
  const click = new MouseEvent("click");
  fire("window", "click", click);
  assert.equal(click.defaultPrevented, false);
}

function testFieldsAndThePadKeepTheirOwnPress() {
  const input = el("input");
  touch("pointerdown", input);
  advance(T.LONG_PRESS_MS);
  assert.equal(menusOn(input).length, 0, "select, copy and paste stay the platform's");
  touch("pointerup", input);

  const pad = el("div", ["joystick-pad"]);
  const stick = el("div", ["js-stick"], pad);
  touch("pointerdown", stick);
  advance(T.LONG_PRESS_MS * 4);
  assert.equal(menusOn(stick).length, 0, "a stick held still is flying");
  touch("pointerup", stick);
}

function testTheBareMapIsLeftToMapLibre() {
  const canvas = el("canvas", ["maplibregl-canvas"]);
  touch("pointerdown", canvas);
  advance(T.LONG_PRESS_MS);
  assert.equal(menusOn(canvas).length, 0, "MapLibre fires the map's own at 500 ms");
  touch("pointerup", canvas);
  const click = new MouseEvent("click");
  fire("window", "click", click);
  assert.equal(click.defaultPrevented, true, "but the held finger is still no tap");
}

function testAndroidsOwnMenuIsNotASecondOne() {
  const button = el("button");
  touch("pointerdown", button);
  advance(T.LONG_PRESS_MS);
  const native = new MouseEvent("contextmenu");
  native.target = button;
  fire("window", "contextmenu", native);
  assert.equal(native.stopped, true, "the platform's menu after ours is dropped");
  touch("pointerup", button);
  advance(2000);

  // And the other way round: the platform first cancels ours.
  const other = el("button");
  touch("pointerdown", other);
  const first = new MouseEvent("contextmenu");
  first.target = other;
  fire("window", "contextmenu", first);
  assert.notEqual(first.stopped, true);
  advance(T.LONG_PRESS_MS);
  assert.equal(menusOn(other).length, 0);
  touch("pointerup", other);
  advance(2000);
}

function testAHeldPointOnTheMapIsLeftAlone() {
  // A finger resting on a point before it drags it must not delete it.
  const marker = el("div", ["maplibregl-marker"]);
  const dot = el("span", [], marker);
  touch("pointerdown", dot);
  advance(T.LONG_PRESS_MS * 2);
  touch("pointerup", dot);
  assert.equal(menusOn(dot).length, 0, "nothing is sent to the point");
  const android = new MouseEvent("contextmenu");
  android.target = dot;
  fire("window", "contextmenu", android);
  assert.equal(android.stopped, true, "and Android's own menu on it is dropped");

  pointAt = dot;
  assert.equal(T.onMarker({ originalEvent: { clientX: 5, clientY: 5 } }), true,
    "MapLibre's long press over a point does not take the newest point away either");
  pointAt = el("canvas", ["maplibregl-canvas"]);
  assert.equal(T.onMarker({ originalEvent: { clientX: 5, clientY: 5 } }), false,
    "on the bare map it does");
  advance(2000);
}

function testAWindowIsPickedUpOnlyAfterAHold() {
  T.setEnabled(true, false);
  const hud = el("div", ["flight-overlay"]);
  let lifted = 0;
  const down = { type: "pointerdown", pointerType: "touch", pointerId: 3, target: hud, clientX: 50, clientY: 50 };
  assert.equal(T.holdToMove(down, () => { lifted += 1; }), true, "a finger waits");
  touch("pointerdown", hud, 3, 50, 50);
  advance(T.HOLD_TO_MOVE_MS - 1);
  assert.equal(lifted, 0);
  advance(1);
  assert.equal(lifted, 1, "held still, it lifts");
  advance(T.LONG_PRESS_MS);
  assert.equal(menusOn(hud).length, 0, "a lifted window is not also long pressed");
  touch("pointerup", hud, 3, 50, 50);

  // A swipe across it, or a tap, never lifts it.
  T.holdToMove(Object.assign({}, down, { pointerId: 4 }), () => { lifted += 1; });
  touch("pointerdown", hud, 4, 50, 50);
  touch("pointermove", hud, 4, 90, 50);
  advance(T.HOLD_TO_MOVE_MS * 2);
  touch("pointerup", hud, 4, 90, 50);
  T.holdToMove(Object.assign({}, down, { pointerId: 5 }), () => { lifted += 1; });
  touch("pointerdown", hud, 5, 50, 50);
  touch("pointerup", hud, 5, 50, 50);
  advance(T.HOLD_TO_MOVE_MS * 2);
  assert.equal(lifted, 1);

  assert.equal(T.holdToMove(Object.assign({}, down, { pointerType: "mouse" }), () => {}), false,
    "a mouse drags at once, as it always has");
  T.setEnabled(false, false);
  assert.equal(T.holdToMove(down, () => {}), false, "and so does a finger outside tablet mode");
  T.setEnabled(true, false);
  advance(2000);
}

function testADraggedWindowIsNotALongPress() {
  const grip = el("div", ["js-grip"]);
  touch("pointerdown", grip, 6);
  T.claim(6);
  advance(T.LONG_PRESS_MS * 2);
  touch("pointerup", grip, 6);
  assert.equal(menusOn(grip).length, 0);
  advance(2000);
}

function dblclicksOn(target) {
  return target.received.filter((e) => e.type === "dblclick");
}

function testADoubleTapIsADoubleClick() {
  const bar = el("div", ["js-grip"], el("div", ["joystick-pad"]));
  touch("pointerdown", bar, 1, 20, 20);
  touch("pointerup", bar, 1, 20, 20);
  advance(100);
  touch("pointerdown", bar, 1, 22, 21);
  touch("pointerup", bar, 1, 22, 21);
  advance(50);
  assert.equal(dblclicksOn(bar).length, 1, "the pad's grip is sent home by a double tap");
  assert.equal(dblclicksOn(bar)[0].corvusDoubleTap, true);

  // Too slow, or too far apart, is two taps.
  advance(1000);
  touch("pointerdown", bar, 1, 20, 20);
  touch("pointerup", bar, 1, 20, 20);
  advance(T.DOUBLE_TAP_MS + 10);
  touch("pointerdown", bar, 1, 20, 20);
  touch("pointerup", bar, 1, 20, 20);
  advance(50);
  assert.equal(dblclicksOn(bar).length, 1);
  advance(1000);
}

function testTheBrowsersOwnDoubleClickIsNotDoubled() {
  const bar = el("div", ["term-win-bar"]);
  touch("pointerdown", bar);
  touch("pointerup", bar);
  advance(80);
  touch("pointerdown", bar);
  touch("pointerup", bar);
  const native = new MouseEvent("dblclick");
  fire("window", "dblclick", native);
  advance(50);
  assert.equal(dblclicksOn(bar).length, 0, "Android sent its own; none is added");
  advance(1000);

  touch("pointerdown", bar);
  touch("pointerup", bar);
  advance(80);
  touch("pointerdown", bar);
  touch("pointerup", bar);
  advance(50);
  assert.equal(dblclicksOn(bar).length, 1);
  const late = new MouseEvent("dblclick");
  fire("window", "dblclick", late);
  assert.equal(late.stopped, true, "a browser's late one for the same taps is dropped");
  advance(1000);
}

function testTheMapKeepsItsOwnDoubleTap() {
  const map = el("div", ["maplibregl-map"]);
  const canvas = el("canvas", ["maplibregl-canvas"], map);
  for (let i = 0; i < 2; i += 1) {
    touch("pointerdown", canvas);
    touch("pointerup", canvas);
    advance(80);
  }
  advance(50);
  assert.equal(dblclicksOn(canvas).length, 0, "MapLibre zooms on it; a dblclick would zoom twice");
  advance(1000);
}

function testHintsNameTheFingersGesture() {
  T.setEnabled(true, false);
  assert.equal(T.pick("Right click undoes it.", "Hold to undo it."), "Hold to undo it.");
  T.setEnabled(false, false);
  assert.equal(T.pick("Right click undoes it.", "Hold to undo it."), "Right click undoes it.");
  T.setEnabled(true, false);
}

function testNothingHappensWithTheModeOff() {
  T.setEnabled(false, false);
  const button = el("button");
  touch("pointerdown", button);
  advance(T.LONG_PRESS_MS * 2);
  touch("pointerup", button);
  assert.equal(menusOn(button).length, 0);
  assert.equal(T.onMarker({ originalEvent: { clientX: 1, clientY: 1 } }), false);
  const pinch = { defaultPrevented: false, preventDefault() { this.defaultPrevented = true; } };
  fire("document", "gesturestart", pinch);
  assert.equal(pinch.defaultPrevented, false, "a laptop's page zoom is not this module's");
  T.setEnabled(true, false);
  fire("document", "gesturestart", pinch);
  assert.equal(pinch.defaultPrevented, true, "in tablet mode a pinch never zooms the page");
}

// ---------------------------------------------------------------------------
// The stylesheet and the page.
// ---------------------------------------------------------------------------
const stripComments = (css) => css.replace(/\/\*[\s\S]*?\*\//g, "");

function topLevelSelectors(css) {
  const out = [];
  let depth = 0;
  let head = "";
  for (const ch of stripComments(css)) {
    if (ch === "{") { if (depth === 0) out.push(head.trim()); depth += 1; head = ""; }
    else if (ch === "}") { depth = Math.max(0, depth - 1); head = ""; }
    else if (depth === 0) head += ch;
  }
  return out.filter(Boolean);
}

function testEveryTouchRuleIsGuarded() {
  const css = fs.readFileSync(path.join(SRC, "css", "touch.css"), "utf8");
  const loose = topLevelSelectors(css)
    .flatMap((h) => h.split(","))
    .map((s) => s.trim())
    .filter((s) => !s.startsWith(":root[data-touch]") && !s.startsWith(":root:not([data-touch])"));
  assert.deepEqual(loose, [], "touch.css must change nothing outside tablet mode");

  const components = stripComments(fs.readFileSync(path.join(SRC, "css", "components.css"), "utf8"));
  assert.ok(components.includes(":root[data-touch] .btn"), "the shared controls grow too");
}

function testZoomButtonsAndTheResizeEdgeAreGone() {
  const css = stripComments(fs.readFileSync(path.join(SRC, "css", "touch.css"), "utf8"));
  assert.match(css, /\.mc-btn\[data-act="in"\][\s\S]*?\.mc-btn\[data-act="out"\]\s*\{\s*display:\s*none/,
    "two fingers zoom the map; the + and - buttons go");
  assert.match(css, /\.panel-grip\s*\{\s*display:\s*none/);
  assert.match(css, /\[data-touch-hide\]\s*\{\s*display:\s*none/);
}

function testThePagePaintsInTheModeItWillRunIn() {
  const html = fs.readFileSync(path.join(SRC, "index.html"), "utf8");
  assert.ok(html.includes(`localStorage.getItem("${T.KEY}")`), "the inline script reads the same key");
  assert.ok(html.includes('"(pointer: coarse)"'), "and makes the same default");
  assert.ok(html.includes('setAttribute("data-touch", "on")'));
  assert.ok(html.indexOf('src="js/touch.js"') > html.indexOf('src="js/panel.js"'));
}

const tests = [
  testAFingerStartsInTabletModeAndAMouseDoesNot,
  testTheAndroidAppStartsInTabletModeOnAnyScreen,
  testTheOperatorsChoiceOutranksTheScreen,
  testTheSwitchIsKeptOnThisDeviceAndToldAround,
  testAHeldFingerIsARightClick,
  testMovingOrASecondFingerIsNotALongPress,
  testATapIsStillATap,
  testFieldsAndThePadKeepTheirOwnPress,
  testTheBareMapIsLeftToMapLibre,
  testAndroidsOwnMenuIsNotASecondOne,
  testAHeldPointOnTheMapIsLeftAlone,
  testAWindowIsPickedUpOnlyAfterAHold,
  testADraggedWindowIsNotALongPress,
  testADoubleTapIsADoubleClick,
  testTheBrowsersOwnDoubleClickIsNotDoubled,
  testTheMapKeepsItsOwnDoubleTap,
  testHintsNameTheFingersGesture,
  testNothingHappensWithTheModeOff,
  testEveryTouchRuleIsGuarded,
  testZoomButtonsAndTheResizeEdgeAreGone,
  testThePagePaintsInTheModeItWillRunIn,
];

let failed = 0;
for (const t of tests) {
  try {
    t();
    console.log(`ok   - ${t.name}`);
  } catch (err) {
    failed += 1;
    console.log(`FAIL - ${t.name}\n${err.stack}`);
  }
}
if (failed) {
  console.log(`\n${failed} of ${tests.length} tablet mode tests failed.`);
  process.exit(1);
}
console.log(`\nAll ${tests.length} tablet mode tests passed.`);
