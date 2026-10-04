"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.anim — single shared requestAnimationFrame coordinator plus the
 * pure easing helpers used by map.js and instruments.js.
 *
 * Why a shared loop: telemetry over a serial radio arrives at ~5–10 Hz, so the
 * map marker and the flight instruments would visibly stutter if they snapped
 * to each new sample. Both renderers instead keep a TARGET (latest telemetry)
 * and a DISPLAYED value, and a SINGLE rAF loop advances every registered
 * animator one frame at a time. The loop self-cancels once all animators report
 * they have settled (so the field laptop does not spin a permanent 60 FPS loop
 * while the vehicle is idle) and is woken again the moment a new target lands.
 *
 * The easing is exponential approach (a frame-rate-independent, critically
 * damped ease-out — no overshoot, per apple-design defaults). Heading uses the
 * shortest angular path so 350° ↔ 10° animates through 0°, not back through 180°.
 *
 * prefers-reduced-motion: animators snap directly to the target and never wake
 * the loop, satisfying the apple-design reduced-motion contract.
 */
Corvus.anim = (function () {
  const animators = new Set();
  let rafId = null;
  let lastTime = 0;

  const hasRaf = () => typeof window !== "undefined" && typeof window.requestAnimationFrame === "function";
  const raf = (cb) => hasRaf()
    ? window.requestAnimationFrame(cb)
    : window.setTimeout(() => cb(now()), 16);
  const cancelRaf = (id) => {
    if (typeof window !== "undefined" && typeof window.cancelAnimationFrame === "function") {
      window.cancelAnimationFrame(id);
    } else {
      window.clearTimeout(id);
    }
  };
  const now = () => (typeof performance !== "undefined" && performance.now)
    ? performance.now()
    : Date.now();

  function frame(t) {
    rafId = null;
    const dt = lastTime ? Math.min((t - lastTime) / 1000, 0.05) : 0;
    lastTime = t;
    let alive = false;
    for (const a of Array.from(animators)) {
      let stillMoving = false;
      try { stillMoving = a.step(dt, t); } catch (_) { animators.delete(a); }
      if (stillMoving) alive = true;
    }
    if (alive) rafId = raf(frame);
    else lastTime = 0;
  }

  function add(animator) { animators.add(animator); return animator; }
  function remove(animator) { animators.delete(animator); }
  /** Start the loop if it is not already running. Idempotent. */
  function wake() { if (rafId == null) { lastTime = 0; rafId = raf(frame); } }
  function reducedMotion() {
    return !!(typeof window !== "undefined" && window.matchMedia
      && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
  }

  // ---- pure easing helpers (frame-rate independent, no overshoot) ----
  function lerp(a, b, t) { return a + (b - a) * t; }
  function approach(cur, target, dt, tau) {
    const k = 1 - Math.exp(-dt / Math.max(tau, 1e-4));
    return cur + (target - cur) * k;
  }
  function normAngle(a) { return ((a % 360) + 360) % 360; }
  /** Shortest signed delta from cur to target, in (-180, 180]. */
  function shortestDelta(cur, target) {
    return ((target - cur) % 360 + 540) % 360 - 180;
  }
  function approachAngle(cur, target, dt, tau) {
    return cur + shortestDelta(cur, target) * (1 - Math.exp(-dt / Math.max(tau, 1e-4)));
  }
  function settled(a, b, eps) { return Math.abs(a - b) <= eps; }
  function settledAngle(cur, target, eps) { return Math.abs(shortestDelta(cur, target)) <= eps; }

  return {
    add, remove, wake, reducedMotion,
    lerp, approach, normAngle, shortestDelta, approachAngle, settled, settledAngle,
    // test hooks
    _animators: () => animators,
    _rafId: () => rafId,
    _reset: () => { if (rafId != null) cancelRaf(rafId); rafId = null; lastTime = 0; animators.clear(); },
  };
})();
