"use strict";

/**
 * Floating SSH terminal windows (Corvus.termWindows).
 *
 * The behaviour under test is the one the SSH tab could not provide: a session
 * opened from a plugin gets a window of its OWN. Two launcher buttons are two
 * sessions, two windows and two live terminals at the same time — where the
 * shared SSH tab had the second opening replace the first, and sent the
 * operator away from the tab they were working in to see it.
 *
 * The rest is what makes such a window survivable: it can be dragged and sized
 * (at any interface scale), it cannot be pushed somewhere it can no longer be
 * grabbed, closing it leaves the program running, and a session that was
 * REPLACED under its name — which is what a restart does — must not leave the
 * window sitting on a stream that has quietly stopped printing.
 *
 * Plain Node-runnable assertions with a small DOM stub, following
 * tests/test_frontend_plugins.js.
 *
 * Run:
 *   node tests/test_frontend_term_window.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};

// The unscaled viewport the layer reports, and the interface scale <body>
// carries. Both are rewritten by individual tests.
let VIEW = { width: 1400, height: 900 };
let SCALE = 1;

const windowListeners = {};
window.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
window.removeEventListener = (t, cb) => {
  const list = windowListeners[t] || [];
  const i = list.indexOf(cb);
  if (i >= 0) list.splice(i, 1);
};
function fireWindow(type) { (windowListeners[type] || []).slice().forEach((cb) => cb({ type })); }

global.CustomEvent = class CustomEvent {
  constructor(type, o = {}) { this.type = type; this.detail = o.detail; }
};
global.localStorage = { getItem: () => null, setItem: () => {}, removeItem: () => {} };

// ---------------------------------------------------------------------------
// DOM stub. term-window.js builds everything with createElement, measures with
// getBoundingClientRect/offsetWidth (the interface-scale correction), and finds
// gesture targets with contains()/closest().
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "",
    textContent: "",
    children: [],
    style: {},
    dataset: {},
    disabled: false,
    parentNode: null,
    _attrs: {},
    _listeners: {},
    _isEl: true,
  };
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) { const has = e.classList.contains(c); const next = force === undefined ? !has : !!force; if (next) e.classList.add(c); else e.classList.remove(c); return next; },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => { if (c.parentNode) c.parentNode.removeChild(c); c.parentNode = e; e.children.push(c); return c; };
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; return c; };
  Object.defineProperty(e, "firstChild", { get() { return e.children[0] || null; } });
  Object.defineProperty(e, "lastChild", { get() { return e.children[e.children.length - 1] || null; } });
  // The layer is the app's unscaled viewport; every other element is measured
  // from the width/height term-window.js wrote onto it.
  Object.defineProperty(e, "clientWidth", {
    get() { return e.classList.contains("term-layer") ? VIEW.width : (parseFloat(e.style.width) || 0); },
  });
  Object.defineProperty(e, "clientHeight", {
    get() { return e.classList.contains("term-layer") ? VIEW.height : (parseFloat(e.style.height) || 0); },
  });
  Object.defineProperty(e, "offsetWidth", { get() { return e.clientWidth; } });
  Object.defineProperty(e, "offsetHeight", { get() { return e.clientHeight; } });
  // getBoundingClientRect reports SCALED pixels, style.* holds unscaled ones —
  // the whole reason the module divides one by the other.
  e.getBoundingClientRect = () => ({
    left: (parseFloat(e.style.left) || 0) * SCALE,
    top: (parseFloat(e.style.top) || 0) * SCALE,
    width: e.offsetWidth * SCALE,
    height: e.offsetHeight * SCALE,
  });
  e.contains = (node) => {
    if (node === e) return true;
    return e.children.some((c) => c._isEl && c.contains(node));
  };
  e.closest = (sel) => {
    const want = sel.replace(/^\./, "");
    let cur = e;
    while (cur) {
      if (cur.classList && cur.classList.contains(want)) return cur;
      cur = cur.parentNode;
    }
    return null;
  };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (type, cb) => { (e._listeners[type] = e._listeners[type] || []).push(cb); };
  e.removeEventListener = () => {};
  e.setPointerCapture = () => {};
  e.releasePointerCapture = () => {};
  e.querySelector = () => null;
  e.querySelectorAll = () => [];
  return e;
}

const body = makeEl("body");
global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelector: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
  body,
};

/** Fire every listener of `type` on a stub element. */
function fire(el, type, event) {
  (el._listeners[type] || []).slice().forEach((cb) => cb(Object.assign({
    type, button: 0, pointerId: 1, target: el, preventDefault() {},
  }, event)));
}

// ---------------------------------------------------------------------------
// The terminal itself is Corvus.sshTerm's job and has its own coverage; here it
// is a spy, so "did this window attach a NEW stream" is answerable.
// ---------------------------------------------------------------------------
const terms = [];
Corvus.sshTerm = {
  available: () => sshTermAvailable,
  create(container, conn, opts) {
    const handle = {
      conn, container, opts,
      disposed: false, fits: 0, focused: 0,
      fit() { handle.fits += 1; },
      focus() { handle.focused += 1; },
      write() {},
      dispose() { handle.disposed = true; },
    };
    terms.push(handle);
    return handle;
  },
};
let sshTermAvailable = true;

// POST /api/ssh/disconnect is the only network the module does.
const posts = [];
global.fetch = (url, init) => {
  posts.push({ url, body: JSON.parse((init && init.body) || "{}") });
  return Promise.resolve({ json: () => Promise.resolve({ ok: true }) });
};

require("../src/js/ui.js");
require("../src/js/term-window.js");

const tw = Corvus.termWindows;

function flush() { return new Promise((r) => setTimeout(r, 0)); }

/** The layer, and the window frames in it (back to front). */
function layer() { return body.children.find((c) => c.classList.contains("term-layer")); }
function frames() { return layer() ? layer().children : []; }
function frameOf(name) {
  return frames().find((f) => f.getAttribute("aria-label") &&
    f.getAttribute("aria-label").endsWith(name));
}
function partOf(frame, cls) {
  const walk = (el) => {
    for (const c of el.children) {
      if (c._isEl && c.classList.contains(cls)) return c;
      if (c._isEl) { const hit = walk(c); if (hit) return hit; }
    }
    return null;
  };
  return walk(frame);
}
function toolOf(frame, ariaLabel) {
  const bar = partOf(frame, "term-win-bar");
  const walk = (el) => {
    for (const c of el.children) {
      if (c._isEl && c.getAttribute("aria-label") === ariaLabel) return c;
      if (c._isEl) { const hit = walk(c); if (hit) return hit; }
    }
    return null;
  };
  return walk(bar);
}
function rectOf(frame) {
  return {
    x: parseFloat(frame.style.left), y: parseFloat(frame.style.top),
    w: parseFloat(frame.style.width), h: parseFloat(frame.style.height),
  };
}

/** Between tests: every window gone, every spy empty, the viewport back. */
function reset() {
  tw.closeAll();
  terms.length = 0;
  posts.length = 0;
  VIEW = { width: 1400, height: 900 };
  SCALE = 1;
  sshTermAvailable = true;
}

const SESSION_A = { name: "ssh-launcher/a", title: "Start mission", host: "10.0.0.7", port: 22, username: "pilot" };
const SESSION_B = { name: "ssh-launcher/b", title: "Video", host: "10.0.0.2", port: 22, username: "ops" };

// ---------------------------------------------------------------------------
// Geometry — pure, so asserted without a window
// ---------------------------------------------------------------------------

function testAWindowDraggedOffAnEdgeStaysGrabbable() {
  const view = { width: 1200, height: 800 };
  const far = tw.clampRect({ x: -5000, y: 40, w: 640, h: 400 }, view);
  assert.ok(far.x + far.w >= tw.KEEP_X,
    "enough of the window stays on screen to grab its bar and drag it back");
  const right = tw.clampRect({ x: 5000, y: 40, w: 640, h: 400 }, view);
  assert.ok(right.x <= view.width - tw.KEEP_X, "same on the other side");
}

function testTheTitleBarIsNeverPushedOffTheTopOrBottom() {
  const view = { width: 1200, height: 800 };
  assert.equal(tw.clampRect({ x: 10, y: -400, w: 640, h: 400 }, view).y, 0,
    "above the top edge the bar is gone for good, so it is not allowed there");
  const low = tw.clampRect({ x: 10, y: 5000, w: 640, h: 400 }, view);
  assert.ok(low.y <= view.height - tw.BAR_H, "and the bar stays inside the bottom edge");
}

function testAWindowIsNeverBiggerThanTheViewportOrTooSmallToRead() {
  const small = tw.clampRect({ x: 0, y: 0, w: 5000, h: 5000 }, { width: 500, height: 400 });
  assert.equal(small.w, 500);
  assert.equal(small.h, 400);
  const tiny = tw.clampRect({ x: 0, y: 0, w: 10, h: 10 }, { width: 1200, height: 800 });
  assert.equal(tiny.w, tw.MIN_W);
  assert.equal(tiny.h, tw.MIN_H);
}

function testEachNewWindowLandsBesideTheLastOne() {
  const view = { width: 1400, height: 900 };
  const first = tw.cascadeRect(0, view);
  const second = tw.cascadeRect(1, view);
  assert.notDeepEqual(first, second, "a second window must not hide the first exactly");
  const cramped = tw.cascadeRect(0, { width: 420, height: 320 });
  assert.ok(cramped.w <= 420 && cramped.h <= 320, "and it still fits a small screen");
  // A window that places itself must not land on the telemetry bar: altitude
  // and battery are the last readouts allowed to disappear behind one.
  const below = tw.cascadeRect(0, view, 54);
  assert.ok(below.y >= 54, "the first window opens under the top bar, not over it");
  assert.ok(below.y + below.h <= view.height, "and still fits under it");
}

// ---------------------------------------------------------------------------
// One window per session
// ---------------------------------------------------------------------------

function testTwoSessionsGetTwoWindowsAndTwoLiveTerminals() {
  reset();
  assert.equal(tw.open(SESSION_A), true);
  assert.equal(tw.open(SESSION_B), true);
  assert.equal(tw.count(), 2, "two sessions, two windows — not one reused");
  assert.equal(frames().length, 2);
  assert.equal(terms.length, 2, "and two live terminals");
  assert.equal(terms[0].disposed, false, "opening the second must not tear down the first");
  assert.deepEqual(terms.map((t) => t.conn.name), [SESSION_A.name, SESSION_B.name]);
  assert.notDeepEqual(rectOf(frames()[0]), rectOf(frames()[1]));
}

function testOpeningTheSameSessionAgainRaisesItInsteadOfDuplicating() {
  reset();
  tw.open(SESSION_A);
  tw.open(SESSION_B);
  assert.equal(frames()[frames().length - 1], frameOf(SESSION_B.title), "B is in front");

  assert.equal(tw.open(SESSION_A), true);
  assert.equal(tw.count(), 2, "no second window for a session that already has one");
  assert.equal(terms.length, 2, "and no second stream for it");
  assert.equal(frames()[frames().length - 1], frameOf(SESSION_A.title),
    "asking for it again brings it to the front");
  assert.equal(terms[0].focused >= 2, true, "and puts the keyboard back in it");
}

/* The regression: a restart connects a NEW shell under the same session name.
   The window's stream is still subscribed to the session object that was
   replaced, and because the name resolves to something connected the backend
   never tells it the shell ended — so it shows a terminal that will never
   print again. Re-attaching is the only way the window can find that out. */
function testARestartedSessionGetsAFreshStream() {
  reset();
  tw.open(SESSION_A);
  const first = terms[0];
  tw.open(SESSION_A, { reattach: true });
  assert.equal(first.disposed, true, "the stream of the replaced session is dropped");
  assert.equal(terms.length, 2, "and a new one takes the new shell");
  assert.equal(tw.count(), 1, "still one window — the frame was kept, only the stream changed");
}

/* A launch is not a request to be shown a terminal — the arrow beside the
   button is. So the launcher repairs a window that is already open and opens
   none, without raising it or taking the keyboard. */
function testExistingOnlyRepairsAnOpenWindowAndOpensNoneOtherwise() {
  reset();
  assert.equal(tw.open(SESSION_A, { existingOnly: true }), false,
    "with no window open, nothing is opened");
  assert.equal(tw.count(), 0);
  assert.equal(terms.length, 0);

  tw.open(SESSION_A);
  tw.open(SESSION_B);
  const focusedBefore = terms[0].focused;
  assert.equal(frames()[frames().length - 1], frameOf(SESSION_B.title), "B is in front");

  assert.equal(tw.open(SESSION_A, { reattach: true, existingOnly: true }), true);
  assert.equal(terms.length, 3, "the open window took the new shell");
  assert.equal(tw.count(), 2, "and no window was added");
  assert.equal(frames()[frames().length - 1], frameOf(SESSION_B.title),
    "the repaired window is not thrown in front of the one being read");
  assert.equal(terms[2].focused, 0, "and does not take the keyboard");
  assert.equal(terms[0].focused, focusedBefore);
}

function testAShellThatEndsMarksTheWindowOfflineAndTheNextOpenReconnects() {
  reset();
  tw.open(SESSION_A);
  const frame = frameOf(SESSION_A.title);
  const status = partOf(frame, "ssh-status");
  assert.equal(status.lastChild.textContent, "CONNECTED");

  terms[0].opts.onClosed();
  assert.equal(status.lastChild.textContent, "OFFLINE",
    "the header stops claiming a session that has ended");
  assert.equal(status.classList.contains("connected"), false);

  tw.open(SESSION_A);
  assert.equal(terms.length, 2, "a window whose shell ended re-attaches when it is opened again");
  assert.equal(status.lastChild.textContent, "CONNECTED");
}

// ---------------------------------------------------------------------------
// Closing, disconnecting, coming back
// ---------------------------------------------------------------------------

function testClosingTheWindowLeavesTheProgramRunning() {
  reset();
  tw.open(SESSION_A);
  assert.equal(tw.close(SESSION_A.name), true);
  assert.equal(tw.count(), 0);
  assert.equal(frames().length, 0, "the frame is unmounted");
  assert.equal(terms[0].disposed, true, "and its SSE stream closed");
  assert.equal(posts.length, 0,
    "but nothing was disconnected — the program on the far end keeps running");
  assert.equal(tw.close(SESSION_A.name), false, "closing twice is a no-op");
}

async function testDisconnectEndsTheSessionAndTakesTheWindowWithIt() {
  reset();
  tw.open(SESSION_A);
  const frame = frameOf(SESSION_A.title);
  fire(toolOf(frame, "Disconnect the session"), "click");
  await flush();
  assert.deepEqual(posts, [{ url: "/api/ssh/disconnect", body: { name: SESSION_A.name } }]);
  assert.equal(tw.count(), 0, "the window goes with the session it was showing");
}

function testAReopenedWindowComesBackWhereItWasPut() {
  reset();
  tw.open(SESSION_A);
  const frame = frameOf(SESSION_A.title);
  const bar = partOf(frame, "term-win-bar");
  fire(frame, "pointerdown", { target: bar, clientX: 500, clientY: 500 });
  fire(frame, "pointermove", { clientX: 300, clientY: 620 });
  fire(frame, "pointerup", {});
  const moved = rectOf(frame);

  tw.close(SESSION_A.name);
  tw.open(SESSION_A);
  assert.deepEqual(rectOf(frameOf(SESSION_A.title)), moved,
    "a window put in a corner is there again when it is fetched back");
}

// ---------------------------------------------------------------------------
// Moving and sizing
// ---------------------------------------------------------------------------

function testDraggingTheBarMovesTheWindowByThePointerDelta() {
  reset();
  tw.open(SESSION_A);
  const frame = frameOf(SESSION_A.title);
  const before = rectOf(frame);
  const bar = partOf(frame, "term-win-bar");
  fire(frame, "pointerdown", { target: bar, clientX: 400, clientY: 300 });
  fire(frame, "pointermove", { clientX: 340, clientY: 380 });
  fire(frame, "pointerup", {});
  const after = rectOf(frame);
  assert.equal(after.x, before.x - 60);
  assert.equal(after.y, before.y + 80);
  assert.equal(after.w, before.w, "moving is not sizing");
}

/* <body> carries the interface-scale zoom, so a 100px pointer move at scale 2
   is 50 unscaled pixels — the number style.left is written in. Uncorrected,
   the window ran away from the cursor at any scale but 1. */
function testDraggingIsCorrectedForTheInterfaceScale() {
  reset();
  SCALE = 2;
  tw.open(SESSION_A);
  const frame = frameOf(SESSION_A.title);
  const before = rectOf(frame);
  const bar = partOf(frame, "term-win-bar");
  fire(frame, "pointerdown", { target: bar, clientX: 600, clientY: 400 });
  fire(frame, "pointermove", { clientX: 400, clientY: 400 });
  fire(frame, "pointerup", {});
  assert.equal(rectOf(frame).x, before.x - 100, "200 scaled pixels are 100 unscaled ones");
  SCALE = 1;
}

function testTheGripSizesTheWindowAndTheTerminalBodyDoesNeither() {
  reset();
  tw.open(SESSION_A);
  const frame = frameOf(SESSION_A.title);
  const before = rectOf(frame);

  const bodyEl = partOf(frame, "term-win-body");
  fire(frame, "pointerdown", { target: bodyEl, clientX: 500, clientY: 500 });
  fire(frame, "pointermove", { clientX: 300, clientY: 300 });
  fire(frame, "pointerup", {});
  assert.deepEqual(rectOf(frame), before,
    "a press in the terminal belongs to the shell, not to the window frame");

  const grip = partOf(frame, "term-win-grip");
  fire(frame, "pointerdown", { target: grip, clientX: 500, clientY: 500 });
  fire(frame, "pointermove", { clientX: 460, clientY: 540 });
  fire(frame, "pointerup", {});
  const after = rectOf(frame);
  assert.equal(after.w, before.w - 40);
  assert.equal(after.h, before.h + 40);
  assert.equal(after.x, before.x, "sizing from the corner does not move the window");
}

function testMaximizeFillsTheViewportAndRestoreGivesTheWindowBack() {
  reset();
  tw.open(SESSION_A);
  const frame = frameOf(SESSION_A.title);
  const before = rectOf(frame);
  const maxBtn = toolOf(frame, "Maximize the terminal");
  fire(maxBtn, "click");
  const full = rectOf(frame);
  assert.ok(full.w > before.w && full.h > before.h);
  assert.ok(full.w <= VIEW.width && full.h <= VIEW.height, "and never past the screen");
  assert.equal(frame.classList.contains("is-max"), true);

  fire(toolOf(frame, "Restore the terminal"), "click");
  assert.deepEqual(rectOf(frame), before, "restore is exactly where it was");
  assert.equal(frame.classList.contains("is-max"), false);
}

function testAShrinkingWindowPullsTerminalsBackIntoView() {
  reset();
  tw.open(SESSION_A);
  const frame = frameOf(SESSION_A.title);
  VIEW = { width: 600, height: 420 };
  fireWindow("resize");
  const after = rectOf(frame);
  assert.ok(after.x <= VIEW.width - tw.KEEP_X && after.y <= VIEW.height - tw.BAR_H,
    "a window that was off the new viewport is reachable again");
  assert.ok(after.w <= VIEW.width && after.h <= VIEW.height);
}

// ---------------------------------------------------------------------------
// Degrading
// ---------------------------------------------------------------------------

function testASessionWithoutANameIsRefused() {
  reset();
  assert.equal(tw.open(), false);
  assert.equal(tw.open({ title: "nameless" }), false);
  assert.equal(tw.count(), 0);
}

function testAMissingXtermBundleStillGivesAWindowThatSaysSo() {
  reset();
  sshTermAvailable = false;
  assert.equal(tw.open(SESSION_A), true);
  const frame = frameOf(SESSION_A.title);
  assert.match(partOf(frame, "term-win-body").textContent, /xterm bundle/,
    "the frame explains itself rather than showing an empty black box");
  assert.equal(terms.length, 0);
}

const tests = [
  testAWindowDraggedOffAnEdgeStaysGrabbable,
  testTheTitleBarIsNeverPushedOffTheTopOrBottom,
  testAWindowIsNeverBiggerThanTheViewportOrTooSmallToRead,
  testEachNewWindowLandsBesideTheLastOne,
  testTwoSessionsGetTwoWindowsAndTwoLiveTerminals,
  testOpeningTheSameSessionAgainRaisesItInsteadOfDuplicating,
  testARestartedSessionGetsAFreshStream,
  testExistingOnlyRepairsAnOpenWindowAndOpensNoneOtherwise,
  testAShellThatEndsMarksTheWindowOfflineAndTheNextOpenReconnects,
  testClosingTheWindowLeavesTheProgramRunning,
  testDisconnectEndsTheSessionAndTakesTheWindowWithIt,
  testAReopenedWindowComesBackWhereItWasPut,
  testDraggingTheBarMovesTheWindowByThePointerDelta,
  testDraggingIsCorrectedForTheInterfaceScale,
  testTheGripSizesTheWindowAndTheTerminalBodyDoesNeither,
  testMaximizeFillsTheViewportAndRestoreGivesTheWindowBack,
  testAShrinkingWindowPullsTerminalsBackIntoView,
  testASessionWithoutANameIsRefused,
  testAMissingXtermBundleStillGivesAWindowThatSaysSo,
];

(async () => {
  let failed = 0;
  for (const t of tests) {
    try {
      await t();
      console.log(`  ok  ${t.name}`);
    } catch (err) {
      failed += 1;
      console.error(`  FAIL ${t.name}: ${err.message}`);
    }
  }
  reset();
  console.log(failed ? `${failed} failing` : `All ${tests.length} terminal-window tests passed.`);
  process.exit(failed ? 1 : 0);
})();
