"use strict";

/**
 * Frontend tests for the virtual joystick (Corvus.joystick).
 *
 * Plain Node-runnable assertions in the style of tests/test_frontend_hud_panel.js:
 * stub the globals, require the source, drive the sticks through pointer
 * events, and assert on what reaches POST /api/mavlink/manual.
 *
 * This is the one control in the app whose output moves an aircraft, so the
 * assertions are about the properties that would be unsafe to get wrong:
 *  - both surfaces are OFF until told otherwise, and nothing sends before init();
 *  - the axis mapping and the hover detent (thrust neutral is 0.5, not 0);
 *  - a diagonal drag is clamped to the circle, so travel never exceeds 1;
 *  - the arrow keys move pitch and roll ONLY, at half stick, and sum with the
 *    sticks into one virtual stick rather than exceeding it;
 *  - the WASD keys move thrust and yaw ONLY, share the arrow keys' panel, and
 *    release thrust to the hover detent rather than to zero;
 *  - a keypress is ignored while the caret is in a field, so typing a
 *    connection string cannot fly the aircraft;
 *  - releasing a stick or key, disabling a surface, or losing window focus all
 *    spring back to centre;
 *  - the stream keeps running at neutral (a gap reads as RC loss to PX4) but
 *    never overlaps itself and never fires while the link is down;
 *  - the pad drags by its grip bar only, clamps into the map, and persists.
 *
 * Run:
 *   node tests/test_frontend_joystick.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
window.matchMedia = (q) => ({ matches: false, media: q, addEventListener() {}, removeEventListener() {} });

const windowListeners = {};
window.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
window.removeEventListener = () => {};

// localStorage stub with a peek hatch for the persistence assertions.
const store = new Map();
global.localStorage = {
  getItem: (k) => (store.has(k) ? store.get(k) : null),
  setItem: (k, v) => store.set(k, String(v)),
  removeItem: (k) => store.delete(k),
};
const POS_KEY = "corvus.joystick";

/* The pad's resting geometry inside the 1000x700 host at (70, 60): CSS puts it
   16px in from the bottom-left corner. Grabbing it at exactly (PAD_X, PAD_Y)
   gives a zero grab offset, so the dragged position is the pointer position
   minus the host origin and the arithmetic in the assertions stays readable. */
const PAD_W = 300, PAD_H = 180;
const PAD_X = 70 + 16;                    // host left + inset
const PAD_Y = 60 + 700 - 16 - PAD_H;      // host top + height - inset - pad height

// Timers are driven by hand: the module streams on setTimeout, and a test
// that waited on the real clock would be both slow and flaky.
let timers = [];
let nextTimerId = 1;
window.setTimeout = (fn, ms) => { const id = nextTimerId++; timers.push({ id, fn, ms }); return id; };
window.clearTimeout = (id) => { timers = timers.filter((t) => t.id !== id); };

/** Run every pending timer once, in the order it was scheduled. */
function flushTimers() {
  const due = timers;
  timers = [];
  due.forEach((t) => t.fn());
  return due.length;
}
function pendingDelay() { return timers.length ? timers[0].ms : null; }

function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {}, style: {},
    hidden: false, disabled: false, id: "", title: "", tabIndex: 0,
    _attrs: {}, _listeners: {}, _isEl: true,
    _rect: { left: 0, top: 0, width: 100, height: 100 },
    // The drag maths mixes rects (scaled px) with offset/client sizes
    // (unscaled px); both have to exist for the clamp to be exercised.
    offsetWidth: 100, offsetHeight: 100, clientWidth: 1000, clientHeight: 700,
    parentElement: null, parentNode: null,
  };
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) { const has = e.classList.contains(c); const next = force === undefined ? !has : !!force; if (next) e.classList.add(c); else e.classList.remove(c); return next; },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  Object.defineProperty(e, "firstChild", { get() { return e.children[0] || null; } });
  e.appendChild = (c) => {
    if (c.parentNode && c.parentNode !== e) c.parentNode.removeChild(c);
    c.parentNode = e; c.parentElement = e;
    e.children.push(c);
    return c;
  };
  e.append = (...cs) => cs.forEach((c) => e.appendChild(c));
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; return c; };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.removeEventListener = () => {};
  e.setPointerCapture = () => {};
  e.releasePointerCapture = () => {};
  e.getBoundingClientRect = () => e._rect;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  return e;
}
function querySel(children, sel) {
  const out = [];
  const classes = sel.split(".").filter(Boolean);
  (function walk(list) {
    for (const c of list) {
      if (!c || !c._isEl) continue;
      if (classes.every((cl) => c.className.split(/\s+/).includes(cl))) out.push(c);
      if (c.children) walk(c.children);
    }
  })(children);
  return out;
}
global.document = {
  createElement: makeEl,
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
};

// Telemetry stub: records every POST and hands back a promise the test
// resolves, so "a frame is still in flight" is a state the test can hold.
const posts = [];
let stateSubscriber = null;
let pendingResolve = null;
global.Corvus.telemetry = {
  subscribe: (fn) => { stateSubscriber = fn; },
  requestJson: (url, options) => {
    posts.push({ url, body: JSON.parse(options.body) });
    return new Promise((resolve, reject) => { pendingResolve = { resolve, reject }; });
  },
};

// The real component layer, not a stub: the pad builds its grip and its four
// keys through Corvus.ui.icon, so a mismatch there should fail here.
require("./../src/js/ui.js");
require("./../src/js/joystick.js");

// ---------------------------------------------------------------------------

/* The module is a singleton, so every test has to hand it back in a clean
   state: switched off, no scheduled tick, and — the one that bites — no frame
   still counted as outstanding from the previous test's unresolved POST. */
async function reset() {
  Corvus.joystick.setEnabled(false);
  Corvus.joystick.setKeysEnabled(false);
  Corvus.joystick.setWasdEnabled(false);
  if (pendingResolve) { pendingResolve.resolve({ ok: true }); pendingResolve = null; }
  await new Promise((r) => setImmediate(r));
  posts.length = 0;
  timers = [];
  store.clear();
}

/** Mount a fresh pad inside a map-sized host and report the parts the tests
 *  drive. `sticks` / `keys` / `wasd` switch the three surfaces on after init,
 *  which is the order the real app uses (build hidden, then apply the
 *  config). */
async function mount({ connected = true, sticks = true, keys = false, wasd = false } = {}) {
  await reset();
  const host = makeEl("div");                 // stands in for .map-view
  // clientWidth === rect width means an interface scale of 1; the scaled case
  // is covered by testDragTracksTheCursorAtAScaledInterfaceSize.
  host.clientWidth = 1000; host.clientHeight = 700;
  host.offsetWidth = 1000; host.offsetHeight = 700;
  host._rect = { left: 70, top: 60, width: 1000, height: 700 };
  const pad = makeEl("div");
  pad.offsetWidth = PAD_W; pad.offsetHeight = PAD_H;
  pad._rect = { left: PAD_X, top: PAD_Y, width: PAD_W, height: PAD_H };
  host.appendChild(pad);
  Corvus.joystick.init(pad);
  const bases = pad.querySelectorAll("js-base");
  bases.forEach((b) => { b._rect = { left: 0, top: 0, width: 100, height: 100 }; });
  if (stateSubscriber) stateSubscriber({ connected });
  if (sticks) Corvus.joystick.setEnabled(true);
  if (keys) Corvus.joystick.setKeysEnabled(true);
  if (wasd) Corvus.joystick.setWasdEnabled(true);
  return {
    host, pad,
    left: bases[0], right: bases[1],
    axes: pad.querySelector("js-axes"),
    grip: pad.querySelector("js-grip"),
    collapse: pad.querySelector("js-collapse"),
    surfaces: pad.querySelector("js-surfaces"),
    keysPanel: pad.querySelector("js-keys"),
    arrows: pad.querySelector("js-cluster-arrows"),
    wasd: pad.querySelector("js-cluster-wasd"),
    // Takes the key's own class suffix: "up"/"left"/... for the arrows,
    // "w"/"a"/"s"/"d" for WASD.
    key: (dir) => pad.querySelector("js-key-" + dir),
  };
}

/** Dispatch a keyboard event at the window listeners the module installed. */
function fireKey(type, key, target) {
  let defaulted = true;
  (windowListeners[type] || []).forEach((cb) => cb({
    key,
    target: target || { tagName: "BODY" },
    preventDefault() { defaulted = false; },
  }));
  return { defaultPrevented: !defaulted };
}

function fire(el, type, ev) {
  (el._listeners[type] || []).forEach((cb) => cb(Object.assign({
    preventDefault() {}, pointerId: 1,
  }, ev)));
}
/* Bases are 100x100 at the origin, so the centre is (50, 50) and one unit of
   travel is 50px. Screen y grows downward; the module inverts it. */
function drag(base, dx, dy) {
  fire(base, "pointerdown", { clientX: 50, clientY: 50 });
  fire(base, "pointermove", { clientX: 50 + dx * 50, clientY: 50 - dy * 50 });
}
function release(base) { fire(base, "pointerup", { clientX: 50, clientY: 50 }); }

/** Settle the pending POST so the next tick is not skipped as overlapping. */
function settle() {
  if (pendingResolve) { pendingResolve.resolve({ ok: true }); pendingResolve = null; }
  return new Promise((r) => setImmediate(r));
}

function lastFrame() { return posts[posts.length - 1].body; }

// ===========================================================================

async function testEnablingBeforeInitDoesNotStream() {
  // Runs FIRST, before any mount: the /api/config read races the DOM wiring,
  // and an invisible control the operator cannot centre must never become an
  // input source. Once init() has run there is no un-initialising it.
  await reset();
  Corvus.joystick.setEnabled(true);
  Corvus.joystick.setKeysEnabled(true);
  assert.equal(posts.length, 0, "no frames without a pad");
  assert.equal(timers.length, 0, "no stream without a pad");
  Corvus.joystick.setEnabled(false);
  Corvus.joystick.setKeysEnabled(false);
}

async function testPadStartsHiddenAndSilent() {
  const { pad } = await mount({ sticks: false });
  assert.equal(pad.hidden, true, "pad hidden until a surface is switched on");
  assert.equal(Corvus.joystick.isEnabled(), false);
  assert.equal(Corvus.joystick.isKeysEnabled(), false);
  assert.equal(posts.length, 0, "nothing streamed while off");
  assert.equal(timers.length, 0, "no stream scheduled while off");
}

async function testEnablingShowsThePadAndStreamsNeutral() {
  const { pad } = await mount({ sticks: false });
  Corvus.joystick.setEnabled(true);

  assert.equal(pad.hidden, false, "pad shown");
  assert.equal(posts.length, 1, "first frame sent immediately");
  assert.equal(posts[0].url, "/api/mavlink/manual");
  // Neutral is the hover detent, NOT zero thrust.
  assert.deepEqual(lastFrame(), { x: 0, y: 0, z: 0.5, r: 0 });
}

async function testEachSurfaceIsShownOnlyWhenItsOwnSwitchIsOn() {
  const m = await mount({ sticks: true, keys: false });
  const sticksEl = m.pad.querySelector("js-sticks");
  const keysEl = m.pad.querySelector("js-keys");
  assert.equal(sticksEl.hidden, false, "sticks shown");
  assert.equal(keysEl.hidden, true, "keys hidden");

  Corvus.joystick.setKeysEnabled(true);
  Corvus.joystick.setEnabled(false);
  assert.equal(sticksEl.hidden, true, "sticks hidden");
  assert.equal(keysEl.hidden, false, "keys shown");
  assert.equal(m.pad.hidden, false, "pad stays up for the keys alone");

  Corvus.joystick.setKeysEnabled(false);
  assert.equal(m.pad.hidden, true, "pad goes away when the last surface does");
}

async function testStickAxesMapToTheTransmitterFrame() {
  const { left, right } = await mount();

  // One axis at a time: a corner drag is clamped to the circle, so no single
  // axis can read 1 while another is off centre (see the diagonal test).
  const cases = [
    { stick: left, dx: -1, dy: 0, axis: "r", want: -1, what: "left stick X is yaw" },
    { stick: left, dx: 0, dy: 1, axis: "z", want: 1, what: "left stick up is full thrust" },
    { stick: right, dx: 1, dy: 0, axis: "y", want: 1, what: "right stick X is roll" },
    { stick: right, dx: 0, dy: -1, axis: "x", want: -1, what: "right stick down is pitch back" },
  ];
  for (const c of cases) {
    await settle();
    drag(c.stick, c.dx, c.dy);
    flushTimers();
    assert.equal(lastFrame()[c.axis], c.want, c.what);
    await settle();
    release(c.stick);
    flushTimers();
  }
}

async function testThrustRunsZeroToOneWithCentreAtHalf() {
  const { left } = await mount();
  await settle();

  drag(left, 0, -1);
  flushTimers();
  assert.equal(lastFrame().z, 0, "stick fully down is zero thrust");

  await settle();
  release(left);
  flushTimers();
  assert.equal(lastFrame().z, 0.5, "released stick returns to the hover detent");
}

async function testDiagonalTravelIsClampedToTheCircle() {
  const { right } = await mount();
  await settle();

  drag(right, 2, 2);        // way outside the base, corner-wise
  flushTimers();

  const f = lastFrame();
  assert.ok(Math.hypot(f.x, f.y) <= 1.0001, `travel ${Math.hypot(f.x, f.y)} exceeds full deflection`);
  assert.ok(f.x > 0.6 && f.y > 0.6, "still reads as a full diagonal");
}

async function testSmallJitterInsideTheDeadzoneReadsAsCentre() {
  const { right } = await mount();
  await settle();

  drag(right, 0.03, -0.02);
  flushTimers();

  assert.equal(lastFrame().x, 0);
  assert.equal(lastFrame().y, 0);
}

async function testLosingWindowFocusCentresAHeldStick() {
  const { right } = await mount();
  await settle();
  drag(right, 1, 1);

  (windowListeners.blur || []).forEach((cb) => cb());
  flushTimers();

  assert.deepEqual(lastFrame(), { x: 0, y: 0, z: 0.5, r: 0 });
}

async function testDisablingStopsTheStreamAndCentresTheSticks() {
  const { pad, left, right } = await mount();
  await settle();
  drag(left, 1, 1);

  Corvus.joystick.setEnabled(false);

  assert.equal(pad.hidden, true, "pad hidden again");
  assert.equal(timers.length, 0, "stream stopped");
  assert.equal(right.parentElement.className.includes("active"), false);
  const before = posts.length;
  flushTimers();
  assert.equal(posts.length, before, "nothing sent after the switch went off");
}

async function testNeutralStreamsSlowerThanAHeldStick() {
  const { right } = await mount();
  const idle = pendingDelay();
  await settle();

  drag(right, 1, 0);
  flushTimers();
  const active = pendingDelay();

  assert.ok(active < idle, `held stick (${active}ms) must stream faster than neutral (${idle}ms)`);
  // Both rates stay well inside PX4's default 0.5 s RC-loss window.
  assert.ok(idle <= 250, `idle rate ${idle}ms is too close to the RC-loss timeout`);
}

async function testAFrameIsNeverSentWhileThePreviousOneIsOutstanding() {
  await mount();
  assert.equal(posts.length, 1);

  flushTimers();     // previous POST still unresolved
  flushTimers();
  assert.equal(posts.length, 1, "ticks skipped rather than queued");

  await settle();
  flushTimers();
  assert.equal(posts.length, 2, "streaming resumes once the frame lands");
}

async function testAFailedFrameDoesNotStopTheStream() {
  await mount();
  pendingResolve.reject(new Error("Request failed (503)"));
  pendingResolve = null;
  await new Promise((r) => setImmediate(r));

  flushTimers();
  assert.equal(posts.length, 2, "stream continued after a dropped frame");
}

async function testNothingIsSentWhileTheLinkIsDown() {
  const { axes } = await mount({ connected: false });

  assert.equal(posts.length, 0, "no frames without a vehicle");
  assert.ok(timers.length > 0, "but the stream stays scheduled for the reconnect");
  assert.equal(axes.textContent, "NO LINK");

  stateSubscriber({ connected: true });
  flushTimers();
  assert.equal(posts.length, 1, "frames resume when the link comes back");
}

// --- arrow keys ------------------------------------------------------------

async function testArrowKeysMovePitchAndRollOnly() {
  await mount({ sticks: false, keys: true });

  const cases = [
    { key: "ArrowUp", axis: "x", want: 0.5, what: "up is pitch forward" },
    { key: "ArrowDown", axis: "x", want: -0.5, what: "down is pitch back" },
    { key: "ArrowRight", axis: "y", want: 0.5, what: "right is roll right" },
    { key: "ArrowLeft", axis: "y", want: -0.5, what: "left is roll left" },
  ];
  for (const c of cases) {
    await settle();
    fireKey("keydown", c.key);
    flushTimers();
    const f = lastFrame();
    assert.equal(f[c.axis], c.want, c.what);
    // Thrust and yaw are NOT reachable from the keyboard.
    assert.equal(f.z, 0.5, "thrust stays at the hover detent");
    assert.equal(f.r, 0, "yaw stays centred");
    await settle();
    fireKey("keyup", c.key);
    flushTimers();
    assert.equal(lastFrame()[c.axis], 0, "key released returns to centre");
  }
}

async function testArrowKeysDeflectHalfStickNotFull() {
  await mount({ sticks: false, keys: true });
  await settle();
  fireKey("keydown", "ArrowUp");
  flushTimers();
  assert.equal(lastFrame().x, 0.5, "a key has no travel to meter, so it is not full authority");
}

async function testAKeyPressIsSwallowedSoThePageDoesNotScroll() {
  await mount({ sticks: false, keys: true });
  assert.equal(fireKey("keydown", "ArrowUp").defaultPrevented, true);
  fireKey("keyup", "ArrowUp");
  // A key we do not own is left entirely alone.
  assert.equal(fireKey("keydown", "PageDown").defaultPrevented, false);
}

async function testArrowKeysAreIgnoredWhileTheCaretIsInAField() {
  await mount({ sticks: false, keys: true });
  await settle();

  const r = fireKey("keydown", "ArrowUp", { tagName: "INPUT" });
  flushTimers();

  assert.equal(r.defaultPrevented, false, "the field keeps its own arrow keys");
  assert.equal(lastFrame().x, 0, "typing a connection string cannot fly the aircraft");
}

async function testArrowKeysDoNothingWhileTheirSwitchIsOff() {
  await mount({ sticks: true, keys: false });
  await settle();

  fireKey("keydown", "ArrowUp");
  flushTimers();

  assert.equal(lastFrame().x, 0, "the sticks surface alone must not answer the keyboard");
  fireKey("keyup", "ArrowUp");
}

async function testTheOnScreenKeyLightsUpUnderARealKeypress() {
  const { key } = await mount({ sticks: false, keys: true });
  const up = key("up");

  fireKey("keydown", "ArrowUp");
  assert.ok(up.className.includes("active"), "the cluster shows what the keyboard is doing");
  fireKey("keyup", "ArrowUp");
  assert.ok(!up.className.includes("active"));
}

async function testAnOnScreenKeyFliesTheSameAxisAsItsKeyboardKey() {
  const { key } = await mount({ sticks: false, keys: true });
  await settle();
  const right = key("right");

  fire(right, "pointerdown", {});
  flushTimers();
  assert.equal(lastFrame().y, 0.5, "holding the on-screen key rolls right");

  await settle();
  fire(right, "pointerup", {});
  flushTimers();
  assert.equal(lastFrame().y, 0, "releasing it centres");
}

async function testOppositeKeysCancelRatherThanFight() {
  await mount({ sticks: false, keys: true });
  await settle();

  fireKey("keydown", "ArrowUp");
  fireKey("keydown", "ArrowDown");
  flushTimers();

  assert.equal(lastFrame().x, 0);
  fireKey("keyup", "ArrowUp"); fireKey("keyup", "ArrowDown");
}

async function testKeysAndSticksSumIntoOneVirtualStick() {
  const { right } = await mount({ sticks: true, keys: true });
  await settle();

  drag(right, 0, 1);                 // right stick full forward
  fireKey("keydown", "ArrowUp");     // and the key pushes the same way
  flushTimers();

  const f = lastFrame();
  assert.ok(f.x <= 1.0001, `pitch ${f.x} exceeded full deflection`);
  assert.equal(f.x, 1, "two ways to move one stick, clamped at the stop");
  fireKey("keyup", "ArrowUp");
  release(right);
}

async function testLosingWindowFocusReleasesAHeldKey() {
  const { key } = await mount({ sticks: false, keys: true });
  await settle();
  fireKey("keydown", "ArrowLeft");

  (windowListeners.blur || []).forEach((cb) => cb());
  flushTimers();

  assert.equal(lastFrame().y, 0);
  assert.ok(!key("left").className.includes("active"));
}

// --- moving the pad --------------------------------------------------------

async function testTheGripDragsThePadAndPersistsThePosition() {
  const { pad, grip } = await mount();

  fire(grip, "pointerdown", { clientX: PAD_X, clientY: PAD_Y, button: 0 });
  fire(pad, "pointermove", { clientX: 400, clientY: 300 });
  fire(pad, "pointerup", { clientX: 400, clientY: 300 });

  assert.ok(pad.className.includes("is-placed"), "anchoring handed over to left/top");
  assert.equal(pad.style.left, "330px");   // 400 - 70 (host left) - 0 (grab offset)
  assert.equal(pad.style.top, "240px");
  assert.deepEqual(JSON.parse(store.get(POS_KEY)), { x: 330, y: 240, collapsed: false });
}

async function testADragIsClampedSoThePadStaysReachable() {
  const { pad, grip } = await mount();

  fire(grip, "pointerdown", { clientX: PAD_X, clientY: PAD_Y, button: 0 });
  fire(pad, "pointermove", { clientX: -9000, clientY: -9000 });
  fire(pad, "pointerup", {});

  // MIN_VISIBLE (64) - pad width (300) is as far left as it may go, and the
  // top edge is a hard stop.
  assert.equal(pad.style.left, "-236px");
  assert.equal(pad.style.top, "0px");
}

async function testTheSticksAreNotADragHandle() {
  const { pad, right } = await mount();

  fire(right, "pointerdown", { clientX: 50, clientY: 50, button: 0 });
  fire(pad, "pointermove", { clientX: 400, clientY: 300 });
  fire(pad, "pointerup", {});

  assert.ok(!pad.className.includes("is-placed"),
    "a drag that started on a stick must fly, not move the window");
}

async function testDoubleClickingTheGripSendsThePadHome() {
  const { pad, grip } = await mount();
  fire(grip, "pointerdown", { clientX: PAD_X, clientY: PAD_Y, button: 0 });
  fire(pad, "pointermove", { clientX: 400, clientY: 300 });
  fire(pad, "pointerup", {});

  fire(grip, "dblclick", {});

  assert.ok(!pad.className.includes("is-placed"));
  assert.equal(pad.style.left, "");
  assert.deepEqual(JSON.parse(store.get(POS_KEY)), { x: null, y: null, collapsed: false });
}

async function testAStoredPositionIsRestoredOnTheNextLaunch() {
  await reset();
  store.set(POS_KEY, JSON.stringify({ x: 210, y: 120 }));
  const host = makeEl("div");
  host.clientWidth = 1000; host.clientHeight = 700;
  host.offsetWidth = 1000; host.offsetHeight = 700;
  host._rect = { left: 70, top: 60, width: 1000, height: 700 };
  const pad = makeEl("div");
  pad.offsetWidth = PAD_W; pad.offsetHeight = PAD_H;
  host.appendChild(pad);
  Corvus.joystick.init(pad);
  Corvus.joystick.setEnabled(true);

  assert.equal(pad.style.left, "210px");
  assert.equal(pad.style.top, "120px");
}

async function testACorruptStoredPositionFallsBackToTheDefaultCorner() {
  await reset();
  store.set(POS_KEY, "{not json");
  const pad = makeEl("div");
  makeEl("div").appendChild(pad);
  Corvus.joystick.init(pad);

  assert.ok(!pad.style.left, "no stored position to apply");
  assert.ok(!pad.className.includes("is-placed"));
}

async function testDragTracksTheCursorAtAScaledInterfaceSize() {
  // The interface-size control puts a CSS zoom on <body>: rects and pointer
  // coordinates come back scaled while style.left is written unscaled. Without
  // the correction the pad runs away from the cursor.
  const { host, pad, grip } = await mount();
  host._rect = { left: 140, top: 120, width: 2000, height: 1400 };   // 2x
  pad._rect = { left: 2 * PAD_X, top: 2 * PAD_Y, width: 2 * PAD_W, height: 2 * PAD_H };

  fire(grip, "pointerdown", { clientX: 2 * PAD_X, clientY: 2 * PAD_Y, button: 0 });
  fire(pad, "pointermove", { clientX: 800, clientY: 600 });
  fire(pad, "pointerup", {});

  // (800 - 140) / 2 = 330 unscaled px from the host's left edge.
  assert.equal(pad.style.left, "330px");
  assert.equal(pad.style.top, "240px");
}

// --- collapsing ------------------------------------------------------------

async function testCollapsingFoldsTheSurfacesAndRoundTrips() {
  const m = await mount({ sticks: false, keys: true });
  assert.equal(m.surfaces.hidden, false, "surfaces up to begin with");

  fire(m.collapse, "click");
  assert.equal(m.surfaces.hidden, true, "surfaces folded away");
  assert.ok(m.pad.className.includes("is-collapsed"));
  assert.equal(m.pad.hidden, false, "the grip bar stays — this is not 'off'");
  assert.equal(JSON.parse(store.get(POS_KEY)).collapsed, true, "collapsed persisted");

  fire(m.collapse, "click");
  assert.equal(m.surfaces.hidden, false, "surfaces back");
  assert.ok(!m.pad.className.includes("is-collapsed"));
  assert.equal(JSON.parse(store.get(POS_KEY)).collapsed, false);
}

async function testACollapsedPadStopsFlyingButKeepsTheStreamAlive() {
  const m = await mount({ sticks: false, keys: true });
  posts.length = 0;

  fire(m.collapse, "click");
  // A control the operator cannot see is one they cannot centre, so a
  // collapsed pad is not an input source — the same rule that keeps a hidden
  // pad silent.
  const swallowed = fireKey("keydown", "ArrowUp");
  assert.equal(swallowed.defaultPrevented, false, "the key is not even claimed");
  assert.equal(m.key("up").className.includes("active"), false, "no key held");

  await settle();
  flushTimers();
  const last = posts[posts.length - 1];
  assert.ok(last, "frames keep going out while collapsed");
  // Neutral, not silence: a gap in MANUAL_CONTROL is what PX4 reads as RC loss.
  assert.deepEqual(
    [last.body.x, last.body.y, last.body.z, last.body.r], [0, 0, 0.5, 0],
    "a collapsed pad streams a released transmitter, not nothing",
  );

  fire(m.collapse, "click");
  fireKey("keydown", "ArrowUp");
  assert.ok(m.key("up").className.includes("active"), "keys fly again once expanded");
  fireKey("keyup", "ArrowUp");
}

async function testACollapsedPadIsRestoredOnTheNextLaunch() {
  await reset();
  store.set(POS_KEY, JSON.stringify({ x: null, y: null, collapsed: true }));
  const host = makeEl("div");
  host.clientWidth = 1000; host.clientHeight = 700;
  host._rect = { left: 70, top: 60, width: 1000, height: 700 };
  const pad = makeEl("div");
  pad.offsetWidth = PAD_W; pad.offsetHeight = PAD_H;
  host.appendChild(pad);
  Corvus.joystick.init(pad);
  Corvus.joystick.setKeysEnabled(true);

  assert.ok(pad.className.includes("is-collapsed"), "collapsed restored");
  assert.equal(pad.querySelector("js-surfaces").hidden, true);
  Corvus.joystick.setKeysEnabled(false);
}

async function testTheCollapseControlIsNotADragHandle() {
  const m = await mount();
  // The control sits ON the grip, which is the drag surface; a click on it
  // must not also shove the pad across the map.
  fire(m.grip, "pointerdown", {
    clientX: PAD_X, clientY: PAD_Y, button: 0,
    target: { closest: (sel) => (sel === ".icon-btn" ? m.collapse : null) },
  });
  fire(m.pad, "pointermove", { clientX: 600, clientY: 400 });
  fire(m.pad, "pointerup", {});
  assert.equal(m.pad.style.left, "", "no drag started from the collapse control");
}

// --- WASD keys -------------------------------------------------------------

async function testWasdKeysMoveThrustAndYawOnly() {
  await mount({ sticks: false, keys: false, wasd: true });

  // Half stick on the throttle axis is 0.25 either side of the 0.5 detent.
  const cases = [
    { key: "w", axis: "z", want: 0.75, what: "W is thrust up" },
    { key: "s", axis: "z", want: 0.25, what: "S is thrust down" },
    { key: "d", axis: "r", want: 0.5, what: "D is yaw clockwise" },
    { key: "a", axis: "r", want: -0.5, what: "A is yaw counter-clockwise" },
  ];
  for (const c of cases) {
    await settle();
    fireKey("keydown", c.key);
    flushTimers();
    const f = lastFrame();
    assert.equal(f[c.axis], c.want, c.what);
    // Pitch and roll are NOT reachable from WASD.
    assert.equal(f.x, 0, "pitch stays centred");
    assert.equal(f.y, 0, "roll stays centred");
    await settle();
    fireKey("keyup", c.key);
    flushTimers();
  }
}

async function testReleasingAThrustKeyReturnsToTheHoverDetent() {
  await mount({ sticks: false, keys: false, wasd: true });
  await settle();
  fireKey("keydown", "w");
  flushTimers();
  assert.equal(lastFrame().z, 0.75);

  await settle();
  fireKey("keyup", "w");
  flushTimers();
  // The one release that is not a return to zero: letting go of the throttle
  // parks the aircraft at hover, exactly as letting go of the stick does.
  assert.equal(lastFrame().z, 0.5, "thrust springs back to the detent, not to zero");
}

async function testWasdIsShiftAndCapsLockProof() {
  await mount({ sticks: false, keys: false, wasd: true });
  await settle();
  fireKey("keydown", "W");
  flushTimers();
  assert.equal(lastFrame().z, 0.75, "a capital W flies the same axis as a lower-case one");
  fireKey("keyup", "W");
}

async function testWasdDoesNothingWhileItsSwitchIsOff() {
  await mount({ sticks: false, keys: true, wasd: false });
  await settle();

  // W is an ordinary letter until the WASD switch is on: the arrow-key
  // surface alone must neither claim it nor fly on it.
  const r = fireKey("keydown", "w");
  flushTimers();
  assert.equal(r.defaultPrevented, false, "the key is not even claimed");
  assert.equal(lastFrame().z, 0.5, "thrust stays at the hover detent");
  fireKey("keyup", "w");
}

async function testWasdIsIgnoredWhileTheCaretIsInAField() {
  await mount({ sticks: false, keys: false, wasd: true });
  await settle();

  const r = fireKey("keydown", "w", { tagName: "INPUT" });
  flushTimers();

  assert.equal(r.defaultPrevented, false, "typing a 'w' types a 'w'");
  assert.equal(lastFrame().z, 0.5, "naming an SSH host cannot fly the aircraft");
}

async function testWasdSharesTheArrowKeyPanel() {
  const m = await mount({ sticks: false, keys: true, wasd: true });
  // The point of the feature: ONE window, two clusters, not a second pad.
  assert.ok(m.wasd, "the WASD cluster exists");
  assert.ok(m.arrows, "the arrow cluster exists");
  assert.equal(m.wasd.parentElement, m.keysPanel, "WASD sits in the key panel");
  assert.equal(m.arrows.parentElement, m.keysPanel, "so do the arrows");
  assert.equal(m.keysPanel.hidden, false);
  assert.equal(m.wasd.hidden, false);
  assert.equal(m.arrows.hidden, false);

  Corvus.joystick.setKeysEnabled(false);
  assert.equal(m.keysPanel.hidden, false, "the panel stays up for WASD alone");
  assert.equal(m.arrows.hidden, true, "each cluster answers to its own switch");
  assert.equal(m.wasd.hidden, false);

  Corvus.joystick.setWasdEnabled(false);
  assert.equal(m.keysPanel.hidden, true, "panel goes away with its last cluster");
  assert.equal(m.pad.hidden, true, "and so does the pad");
}

async function testAnOnScreenWasdKeyFliesTheSameAxisAsItsKeyboardKey() {
  const m = await mount({ sticks: false, keys: false, wasd: true });
  await settle();
  const w = m.key("w");

  fire(w, "pointerdown", {});
  flushTimers();
  assert.equal(lastFrame().z, 0.75, "holding the on-screen W climbs");
  assert.ok(w.className.includes("active"));

  await settle();
  fire(w, "pointerup", {});
  flushTimers();
  assert.equal(lastFrame().z, 0.5, "releasing it hovers");
}

async function testWasdAndTheLeftStickSumIntoOneVirtualStick() {
  const { left } = await mount({ sticks: true, wasd: true });
  await settle();

  drag(left, 0, 1);                // left stick full up
  fireKey("keydown", "w");         // and the key pushes the same way
  flushTimers();

  const f = lastFrame();
  assert.equal(f.z, 1, "two ways to move one stick, clamped at the stop");
  fireKey("keyup", "w");
  release(left);
}

async function testLosingWindowFocusReleasesAHeldWasdKey() {
  await mount({ sticks: false, keys: false, wasd: true });
  await settle();
  fireKey("keydown", "w");

  (windowListeners.blur || []).forEach((cb) => cb());
  flushTimers();

  // A window that loses focus must not leave the throttle pinned open with
  // the stream still sending it.
  assert.equal(lastFrame().z, 0.5);
}

async function testACollapsedPadStandsDownWasdToo() {
  const m = await mount({ sticks: false, keys: false, wasd: true });
  fire(m.collapse, "click");

  const swallowed = fireKey("keydown", "w");
  assert.equal(swallowed.defaultPrevented, false, "the key is not even claimed");
  assert.equal(m.key("w").className.includes("active"), false, "no key held");

  await settle();
  flushTimers();
  assert.equal(posts[posts.length - 1].body.z, 0.5,
    "a collapsed pad streams a released transmitter, not a held throttle");
  fire(m.collapse, "click");
}

// --- the axis readout ------------------------------------------------------

async function testTheAxisReadoutOnlyNamesTheAxesTheSurfacesCanMove() {
  // The arrows drive pitch and roll and nothing else. Naming thrust and yaw as
  // well made the grip bar twice the width of the key cluster under it.
  const keysOnly = await mount({ sticks: false, keys: true });
  assert.equal(keysOnly.axes.textContent, "P +0.00  R +0.00");

  // WASD is the other half, so it names the other two.
  const wasdOnly = await mount({ sticks: false, wasd: true });
  assert.equal(wasdOnly.axes.textContent, "T 0.50  Y +0.00");

  const bothClusters = await mount({ sticks: false, keys: true, wasd: true });
  assert.equal(bothClusters.axes.textContent, "P +0.00  R +0.00  T 0.50  Y +0.00",
    "the two clusters together are a whole transmitter");

  const withSticks = await mount({ sticks: true, keys: true });
  assert.equal(withSticks.axes.textContent, "P +0.00  R +0.00  T 0.50  Y +0.00",
    "all four axes once a stick can move them");
}

// Leaving Home hides the map view, so it reports 0x0. Reflowing against a box
// with no size collapsed every coordinate to the origin, which is how a pad the
// operator had carefully placed came back sitting in the top-left corner after
// a round trip through Setup or Options. A control surface is worse to lose
// than a readout: the operator reaches for a stick that is no longer there.
async function testAHiddenMapLeavesThePadWhereItWas() {
  const { host, pad, grip } = await mount();
  fire(grip, "pointerdown", { clientX: PAD_X, clientY: PAD_Y, button: 0 });
  fire(pad, "pointermove", { clientX: 400, clientY: 300 });
  fire(pad, "pointerup", {});
  const placed = { left: pad.style.left, top: pad.style.top };
  assert.ok(parseInt(placed.left, 10) > 0 && parseInt(placed.top, 10) > 0,
    "the pad starts away from the corner");

  // Navigate to Setup: display:none makes every box zero.
  host.clientWidth = 0; host.clientHeight = 0;
  (windowListeners.resize || []).forEach((cb) => cb());
  assert.deepEqual({ left: pad.style.left, top: pad.style.top }, placed,
    "a hidden map moves nothing");

  // ...and back to Home.
  host.clientWidth = 1000; host.clientHeight = 700;
  (windowListeners.resize || []).forEach((cb) => cb());
  assert.deepEqual({ left: pad.style.left, top: pad.style.top }, placed,
    "the pad is still where the operator left it");
}

// The hidden interlude must not poison the remembered map size either, or the
// pad would stop travelling with the right edge after the first page visit.
async function testEdgeTrackingSurvivesAHiddenMap() {
  const { host, pad, grip } = await mount();
  // Park the pad against the map's right edge.
  fire(grip, "pointerdown", { clientX: PAD_X, clientY: PAD_Y, button: 0 });
  fire(pad, "pointermove", { clientX: 70 + 1000 - PAD_W, clientY: 200 });
  fire(pad, "pointerup", {});
  const parked = parseInt(pad.style.left, 10);

  // Setup and back, at the same size.
  host.clientWidth = 0; host.clientHeight = 0;
  (windowListeners.resize || []).forEach((cb) => cb());
  host.clientWidth = 1000; host.clientHeight = 700;
  (windowListeners.resize || []).forEach((cb) => cb());
  assert.equal(parseInt(pad.style.left, 10), parked,
    "the round trip is a true no-op — not even a containing nudge");

  // Now narrow the map: the pad must still ride the edge in.
  host.clientWidth = 620;
  (windowListeners.resize || []).forEach((cb) => cb());
  assert.ok(parseInt(pad.style.left, 10) < parked,
    "still tracks the right edge after a page visit");
  assert.ok(parseInt(pad.style.left, 10) + PAD_W <= 620,
    "the whole pad is on the narrowed map");
}

async function testShrinkingTheMapPullsThePadBackIntoView() {
  const { host, pad, grip } = await mount();
  fire(grip, "pointerdown", { clientX: PAD_X, clientY: PAD_Y, button: 0 });
  fire(pad, "pointermove", { clientX: 900, clientY: 600 });
  fire(pad, "pointerup", {});
  const before = parseInt(pad.style.left, 10);

  host.clientWidth = 400; host.clientHeight = 300;
  (windowListeners.resize || []).forEach((cb) => cb());

  const after = parseInt(pad.style.left, 10);
  assert.ok(after < before, "pad pulled back when the map shrank");
  assert.ok(after <= 400 - 64, "still inside the smaller map");
}

const tests = [
  testEnablingBeforeInitDoesNotStream,
  testPadStartsHiddenAndSilent,
  testEnablingShowsThePadAndStreamsNeutral,
  testEachSurfaceIsShownOnlyWhenItsOwnSwitchIsOn,
  testStickAxesMapToTheTransmitterFrame,
  testThrustRunsZeroToOneWithCentreAtHalf,
  testDiagonalTravelIsClampedToTheCircle,
  testSmallJitterInsideTheDeadzoneReadsAsCentre,
  testLosingWindowFocusCentresAHeldStick,
  testDisablingStopsTheStreamAndCentresTheSticks,
  testNeutralStreamsSlowerThanAHeldStick,
  testAFrameIsNeverSentWhileThePreviousOneIsOutstanding,
  testAFailedFrameDoesNotStopTheStream,
  testNothingIsSentWhileTheLinkIsDown,
  testArrowKeysMovePitchAndRollOnly,
  testArrowKeysDeflectHalfStickNotFull,
  testAKeyPressIsSwallowedSoThePageDoesNotScroll,
  testArrowKeysAreIgnoredWhileTheCaretIsInAField,
  testArrowKeysDoNothingWhileTheirSwitchIsOff,
  testTheOnScreenKeyLightsUpUnderARealKeypress,
  testAnOnScreenKeyFliesTheSameAxisAsItsKeyboardKey,
  testOppositeKeysCancelRatherThanFight,
  testKeysAndSticksSumIntoOneVirtualStick,
  testLosingWindowFocusReleasesAHeldKey,
  testWasdKeysMoveThrustAndYawOnly,
  testReleasingAThrustKeyReturnsToTheHoverDetent,
  testWasdIsShiftAndCapsLockProof,
  testWasdDoesNothingWhileItsSwitchIsOff,
  testWasdIsIgnoredWhileTheCaretIsInAField,
  testWasdSharesTheArrowKeyPanel,
  testAnOnScreenWasdKeyFliesTheSameAxisAsItsKeyboardKey,
  testWasdAndTheLeftStickSumIntoOneVirtualStick,
  testLosingWindowFocusReleasesAHeldWasdKey,
  testACollapsedPadStandsDownWasdToo,
  testTheGripDragsThePadAndPersistsThePosition,
  testADragIsClampedSoThePadStaysReachable,
  testTheSticksAreNotADragHandle,
  testDoubleClickingTheGripSendsThePadHome,
  testDragTracksTheCursorAtAScaledInterfaceSize,
  testAStoredPositionIsRestoredOnTheNextLaunch,
  testACorruptStoredPositionFallsBackToTheDefaultCorner,
  testShrinkingTheMapPullsThePadBackIntoView,
  testCollapsingFoldsTheSurfacesAndRoundTrips,
  testACollapsedPadStopsFlyingButKeepsTheStreamAlive,
  testACollapsedPadIsRestoredOnTheNextLaunch,
  testTheCollapseControlIsNotADragHandle,
  testTheAxisReadoutOnlyNamesTheAxesTheSurfacesCanMove,
  testAHiddenMapLeavesThePadWhereItWas,
  testEdgeTrackingSurvivesAHiddenMap,
];

(async () => {
  let failed = 0;
  for (const t of tests) {
    try { await t(); console.log("ok   - " + t.name); }
    catch (e) { failed++; console.error("FAIL - " + t.name + "\n      " + (e && e.message)); }
  }
  if (failed) { console.error(`\n${failed}/${tests.length} joystick test(s) FAILED`); process.exit(1); }
  console.log(`\nAll ${tests.length} joystick tests passed.`);
})();
