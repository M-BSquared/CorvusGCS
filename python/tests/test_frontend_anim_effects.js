"use strict";

/**
 * Frontend tests for the one-shot UI effects in Corvus.anim (enter, leave,
 * pop, shake, flash, stagger).
 *
 * What matters is the contract, not the keyframes: an effect replaces its own
 * previous run instead of stacking, leave() always hands over exactly once,
 * and with no Web Animations API or under reduced motion nothing animates
 * and leave() hands over at once. None of them wakes the shared rAF loop.
 *
 * Run:
 *   node tests/test_frontend_anim_effects.js
 */

const assert = require("node:assert/strict");
const path = require("node:path");

global.window = global;
global.Corvus = {};
let reduced = false;
window.matchMedia = (q) => ({ matches: reduced && q.includes("reduce") });
window.requestAnimationFrame = () => 1;
window.cancelAnimationFrame = () => {};

class FakeAnimation {
  constructor(keyframes, opts) {
    this.keyframes = keyframes;
    this.opts = opts;
    this.state = "running";
    this.listeners = {};
  }
  addEventListener(type, fn) { (this.listeners[type] = this.listeners[type] || []).push(fn); }
  emit(type) { (this.listeners[type] || []).forEach((fn) => fn()); }
  cancel() { if (this.state !== "running") return; this.state = "cancelled"; this.emit("cancel"); }
  finish() { if (this.state !== "running") return; this.state = "finished"; this.emit("finish"); }
}

function makeEl() {
  const el = { style: {}, runs: [] };
  el.animate = (kf, opts) => { const a = new FakeAnimation(kf, opts); el.runs.push(a); return a; };
  return el;
}

require(path.join(__dirname, "..", "src", "js", "anim.js"));
const A = Corvus.anim;

let passed = 0;
function test(name, fn) {
  reduced = false;
  fn();
  passed += 1;
  console.log("ok -", name);
}

test("every effect is exported", () => {
  ["enter", "leave", "pop", "shake", "flash", "stagger"].forEach((k) =>
    assert.equal(typeof A[k], "function", k));
});

test("a second run of the same kind cancels the first", () => {
  const el = makeEl();
  const first = A.pop(el);
  const second = A.pop(el);
  assert.equal(first.state, "cancelled");
  assert.equal(second.state, "running");
});

test("different kinds on one element do not cancel each other", () => {
  const el = makeEl();
  const ring = A.flash(el, "err");
  const wiggle = A.shake(el);
  assert.equal(ring.state, "running");
  assert.equal(wiggle.state, "running");
});

test("only transform and opacity move, flash's ring aside", () => {
  const el = makeEl();
  [A.enter(el), A.pop(el), A.shake(el), A.leave(el, () => {})].forEach((a) => {
    a.keyframes.forEach((f) => Object.keys(f).forEach((k) => {
      if (k === "offset") return;
      assert.ok(["opacity", "transform", "transformOrigin"].includes(k), `unexpected ${k}`);
    }));
  });
  A.flash(el, "ok").keyframes.forEach((f) =>
    Object.keys(f).forEach((k) => assert.ok(k === "boxShadow" || k === "offset", k)));
});

test("leave() hands over once, after the animation ends", () => {
  const el = makeEl();
  let calls = 0;
  const a = A.leave(el, () => { calls += 1; });
  assert.equal(calls, 0);
  assert.equal(el.style.pointerEvents, "none");
  a.finish();
  a.emit("finish");
  assert.equal(calls, 1);
});

test("leave() hands over when it is cut short", () => {
  const el = makeEl();
  let calls = 0;
  A.leave(el, () => { calls += 1; });
  A.leave(el, () => { calls += 1; });
  assert.equal(calls, 1, "the replaced run still handed over");
});

test("without element.animate nothing runs and leave() is synchronous", () => {
  const el = { style: {} };
  assert.equal(A.pop(el), null);
  assert.equal(A.enter(el), null);
  let done = false;
  assert.equal(A.leave(el, () => { done = true; }), null);
  assert.equal(done, true);
  assert.equal(A.pop(null), null);
});

test("reduced motion: nothing animates, leave() is synchronous", () => {
  reduced = true;
  const el = makeEl();
  assert.equal(A.flash(el, "ok"), null);
  assert.equal(A.stagger([el, makeEl()]).filter(Boolean).length, 0);
  let done = false;
  A.leave(el, () => { done = true; });
  assert.equal(done, true);
  assert.equal(el.runs.length, 0);
});

test("stagger caps the delay so a long list is never slow", () => {
  const nodes = Array.from({ length: 30 }, makeEl);
  const runs = A.stagger(nodes);
  const delays = runs.map((a) => a.opts.delay);
  assert.equal(delays[0], 0);
  assert.ok(delays[1] > 0);
  assert.equal(delays[29], delays[28], "items past the cap share the last delay");
  assert.ok(delays[29] <= 120, `last delay ${delays[29]} ms`);
  assert.equal(runs[1].opts.fill, "backwards", "a delayed item stays hidden until its turn");
});

test("entering content is visible from the first frame", () => {
  const a = Corvus.anim.enter(makeEl());
  assert.ok(a.keyframes[0].opacity > 0, "never starts fully transparent");
  assert.ok(a.opts.duration <= 160, `${a.opts.duration} ms`);
});

test("leaving is quicker than entering", () => {
  const el = makeEl();
  const out = A.leave(el, () => {});
  const into = A.enter(makeEl());
  assert.ok(out.opts.duration < into.opts.duration);
});

test("flipping through pages fast skips the entrance", () => {
  const el = makeEl();
  assert.ok(A.enter(el), "the first entrance plays");
  assert.equal(A.enter(el), null, "a second one right after snaps in");
  assert.ok(A.enter(makeEl()), "another element is not affected");
  const group = {};
  assert.ok(A.enter(makeEl(), { group }), "the first panel of a tab row plays");
  assert.equal(A.enter(makeEl(), { group }), null, "the next panel right after snaps in");
});

test("a hidden window animates nothing", () => {
  global.document = { hidden: true };
  try {
    assert.equal(A.pop(makeEl()), null);
    let done = false;
    A.leave(makeEl(), () => { done = true; });
    assert.equal(done, true);
  } finally {
    delete global.document;
  }
});

test("effects never wake the shared animation loop", () => {
  A._reset();
  const el = makeEl();
  A.enter(el); A.pop(el); A.shake(el); A.flash(el); A.stagger([makeEl()]);
  assert.equal(A._rafId(), null);
  assert.equal(A._animators().size, 0);
});

console.log(`\n${passed} passed`);
