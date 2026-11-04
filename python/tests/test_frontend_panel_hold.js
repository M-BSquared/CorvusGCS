"use strict";

/**
 * Frontend tests for Corvus.map.holdThroughPanel: a map beside the right
 * panel is resized once per slide, not on every frame of it, and the picture
 * stays anchored on its left edge while that happens. Under a tilted camera
 * the vanishing point is held instead of panning, so the picture never shears.
 *
 * The fake map stands in for MapLibre's own ResizeObserver: `settle()` is the
 * observer firing, and it resizes the canvas only when the container's width
 * actually changed, which is the property the hold relies on.
 *
 * Run:
 *   node tests/test_frontend_panel_hold.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
window.requestAnimationFrame = () => 0;
window.cancelAnimationFrame = () => {};
window.addEventListener = () => {};
window.removeEventListener = () => {};
window.dispatchEvent = () => true;
window.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
global.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
global.fetch = () => Promise.reject(new Error("offline"));
global.document = {
  createElement: () => ({ style: {}, classList: { add() {}, remove() {} }, appendChild() {} }),
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
};

// The panel's side of the contract: a motion feed the test drives by hand.
const motion = new Set();
Corvus.panel = {
  onMotion(fn) { motion.add(fn); return () => motion.delete(fn); },
};
const emit = (phase, growth) => motion.forEach((fn) => fn({ phase, growth }));

require("../src/js/anim.js");
require("../src/js/map-overlays.js");
require("../src/js/map.js");

function fakeMap(naturalWidth) {
  const listeners = {};
  const container = {
    natural: naturalWidth,
    style: { width: "", right: "" },
    get clientWidth() { return this.style.width ? parseFloat(this.style.width) : this.natural; },
  };
  const canvas = { clientWidth: naturalWidth };
  const m = {
    resizes: 0,
    pans: [],
    on(type, cb) { (listeners[type] = listeners[type] || []).push(cb); },
    off(type, cb) { listeners[type] = (listeners[type] || []).filter((x) => x !== cb); },
    getContainer: () => container,
    getCanvas: () => canvas,
    panBy(offset, opts) { m.pans.push({ offset, opts }); },
    /** MapLibre's observer: a resize only when the box really changed. */
    settle() {
      if (container.clientWidth === canvas.clientWidth) return;
      canvas.clientWidth = container.clientWidth;
      m.resizes += 1;
      (listeners.resize || []).forEach((cb) => cb());
    },
    listenerCount: (type) => (listeners[type] || []).length,
  };
  return { m, container, canvas, listeners };
}

function testOpeningResizesOnceAtTheEnd() {
  const { m, container } = fakeMap(1000);
  const off = Corvus.map.holdThroughPanel(m);
  emit("start", -356);
  // The slide: the column narrows frame by frame under a pinned container.
  for (let w = 1000; w >= 644; w -= 40) { container.natural = w; m.settle(); }
  assert.equal(m.resizes, 0, "no resize while the panel moves");
  container.natural = 644;
  emit("end", 0);
  m.settle();
  assert.equal(m.resizes, 1, "exactly one resize, after the slide");
  assert.deepEqual(m.pans[0].offset, [-178, 0], "picture anchored on the left edge");
  assert.deepEqual(m.pans[0].opts, { animate: false });
  off();
}

function testClosingResizesOnceAtTheStart() {
  const { m, container } = fakeMap(644);
  const off = Corvus.map.holdThroughPanel(m);
  emit("start", 356);
  assert.equal(container.style.width, "1000px", "pinned at the width it is heading for");
  m.settle();
  assert.equal(m.resizes, 1, "the one resize happens up front, under the panel");
  for (let w = 644; w <= 1000; w += 40) { container.natural = w; m.settle(); }
  container.natural = 1000;
  emit("end", 0);
  m.settle();
  assert.equal(m.resizes, 1, "released at the same width: nothing more to rebuild");
  assert.deepEqual(m.pans.map((p) => p.offset), [[178, 0]]);
  assert.equal(container.style.width, "", "the pin is gone afterwards");
  assert.equal(container.style.right, "");
  off();
}

function testFollowingDoesNotMakeThePictureJump() {
  // The Home map follows by default; the picture must hold there exactly as
  // it does on the planner, or the map jumps by half the panel.
  const { m, container } = fakeMap(1000);
  const off = Corvus.map.holdThroughPanel(m, { following: () => true });
  emit("start", -356);
  container.natural = 644;
  emit("end", 0);
  m.settle();
  assert.equal(m.resizes, 1);
  assert.deepEqual(m.pans.map((p) => p.offset), [[-178, 0]]);
  off();
}

function testOnTheGlobeThePanIsMeasuredAndCorrected() {
  // A pan on the globe is not a pure shift: model one that only gets 88% of
  // the way, and require the anchor back at its old pixel anyway.
  const { m, container, canvas } = fakeMap(644);
  let anchorX = 322;                       // the old middle, on screen
  m.getCenter = () => ({ anchor: true });
  m.project = () => ({ x: anchorX, y: 300 });
  m.panBy = (offset, opts) => { m.pans.push({ offset, opts }); anchorX -= offset[0] * 0.88; };
  const settle = m.settle;
  m.settle = () => {
    const before = canvas.clientWidth;
    if (container.clientWidth !== before) anchorX += (container.clientWidth - before) / 2;
    settle();
  };
  const off = Corvus.map.holdThroughPanel(m);
  emit("start", 356);
  m.settle();
  assert.ok(Math.abs(anchorX - 322) < 0.5, `anchor back at 322, got ${anchorX}`);
  assert.ok(m.pans.length > 1 && m.pans.length <= 4, "corrected in a few steps");
  assert.ok(m.pans.every((p) => p.opts && p.opts.animate === false), "never animated");
  container.natural = 1000;
  emit("end", 0);
  off();
}

function testAnUnrelatedResizeLaterIsNotAnchored() {
  const { m, container } = fakeMap(644);
  const off = Corvus.map.holdThroughPanel(m);
  emit("start", 356);
  m.settle();
  container.natural = 1000;
  emit("end", 0);
  m.settle();
  return new Promise((resolve) => setTimeout(() => {
    container.natural = 900;   // the window, much later
    m.settle();
    assert.equal(m.pans.length, 1, "only the slide's own resize was anchored");
    off();
    resolve();
  }, 300));
}

/** A tilted map: padding moves the vanishing point. */
function tiltedMap(naturalWidth) {
  const f = fakeMap(naturalWidth);
  const { m } = f;
  let padding = { top: 0, bottom: 0, left: 0, right: 0 };
  m.eases = [];
  m.getPitch = () => 60;
  m.getPadding = () => Object.assign({}, padding);
  m.setPadding = (p) => { padding = Object.assign({}, p); };
  m.easeTo = (opts) => { m.eases.push(opts); };
  /** Where the vanishing point is drawn, from the left edge. */
  m.vanishingX = (width) => padding.left + (width - padding.left - padding.right) / 2;
  m.moveEnd = () => (f.listeners.moveend || []).forEach((cb) => cb());
  return f;
}

const tick = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function testTiltedOpeningDoesNotMoveThePicture() {
  const { m, container } = tiltedMap(1000);
  const off = Corvus.map.holdThroughPanel(m);
  emit("start", -356);
  container.natural = 644;
  emit("end", 0);
  m.settle();
  assert.equal(m.resizes, 1);
  assert.equal(m.pans.length, 0, "no pan: a pan shears a tilted picture");
  assert.equal(m.eases.length, 0, "and no glide: the picture stays put");
  assert.equal(m.vanishingX(644), 500, "vanishing point still on its old pixel");
  assert.deepEqual(Corvus.map.panelRestPadding(m), { top: 0, bottom: 0, left: 0, right: 0 },
    "the next camera move takes it back");
  off();
}

async function testTiltedClosingThenOpeningCancelsOut() {
  const { m, container } = tiltedMap(644);
  const off = Corvus.map.holdThroughPanel(m);
  emit("start", 356);
  m.settle();
  assert.equal(m.pans.length, 0);
  assert.equal(m.vanishingX(1000), 322, "held on its pixel across the resize");
  container.natural = 1000;
  emit("end", 0);
  await tick(300);
  emit("start", -356);
  container.natural = 644;
  emit("end", 0);
  m.settle();
  assert.deepEqual(m.getPadding(), { top: 0, bottom: 0, left: 0, right: 0 },
    "back where it started, so nothing is left to restore");
  m.moveEnd();
  assert.equal(Corvus.map.panelRestPadding(m), null);
  off();
}

async function testACameraMoveThatRestoresThePaddingClearsTheRest() {
  const { m, container } = tiltedMap(644);
  const off = Corvus.map.holdThroughPanel(m);
  emit("start", 356);
  m.settle();
  container.natural = 1000;
  emit("end", 0);
  await tick(300);
  assert.ok(Corvus.map.panelRestPadding(m), "an offset is owed");
  m.moveEnd();
  assert.ok(Corvus.map.panelRestPadding(m), "a move that did not restore it keeps it owed");
  m.setPadding(Corvus.map.panelRestPadding(m));
  m.moveEnd();
  assert.equal(Corvus.map.panelRestPadding(m), null);
  off();
}

function testFlatMapStillPans() {
  const { m, container } = tiltedMap(1000);
  m.getPitch = () => 0;
  const off = Corvus.map.holdThroughPanel(m);
  emit("start", -356);
  container.natural = 644;
  emit("end", 0);
  m.settle();
  assert.equal(m.eases.length, 0, "a flat map keeps its left edge exactly");
  assert.deepEqual(m.pans.map((p) => p.offset), [[-178, 0]]);
  off();
}

function testAHiddenMapIsLeftAlone() {
  const { m, container } = fakeMap(0);
  const off = Corvus.map.holdThroughPanel(m);
  emit("start", 356);
  assert.equal(container.style.width, "", "a map with no size gets no pin");
  emit("end", 0);
  off();
}

function testUnsubscribeLetsGo() {
  const { m, container } = fakeMap(1000);
  const off = Corvus.map.holdThroughPanel(m);
  emit("start", -356);
  off();
  assert.equal(container.style.width, "", "pin released on unsubscribe");
  assert.equal(m.listenerCount("resize"), 0, "resize listener removed");
  const before = motion.size;
  emit("start", -356);
  assert.equal(container.style.width, "", "no longer listening");
  assert.equal(motion.size, before);
}

function testWithoutAPanelItIsANoop() {
  const saved = Corvus.panel;
  Corvus.panel = undefined;
  const { m } = fakeMap(1000);
  const off = Corvus.map.holdThroughPanel(m);
  assert.equal(typeof off, "function");
  assert.equal(m.listenerCount("resize"), 0);
  off();
  Corvus.panel = saved;
}

(async () => {
  const tests = [
    testOpeningResizesOnceAtTheEnd,
    testClosingResizesOnceAtTheStart,
    testFollowingDoesNotMakeThePictureJump,
    testOnTheGlobeThePanIsMeasuredAndCorrected,
    testAnUnrelatedResizeLaterIsNotAnchored,
    testTiltedOpeningDoesNotMoveThePicture,
    testTiltedClosingThenOpeningCancelsOut,
    testACameraMoveThatRestoresThePaddingClearsTheRest,
    testFlatMapStillPans,
    testAHiddenMapIsLeftAlone,
    testUnsubscribeLetsGo,
    testWithoutAPanelItIsANoop,
  ];
  for (const t of tests) {
    await t();
    console.log(`  ok  ${t.name}`);
  }
  console.log(`\nAll ${tests.length} panel-hold tests passed.`);
})().catch((err) => { console.error(err); process.exit(1); });
