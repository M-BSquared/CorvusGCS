"use strict";

/**
 * Frontend tests for Corvus.map.holdThroughPanel: a map beside the right
 * panel is resized once per slide, not on every frame of it, and the picture
 * stays anchored on its left edge while that happens.
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
  return { m, container, canvas };
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
