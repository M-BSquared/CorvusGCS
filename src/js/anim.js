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
 * damped ease-out — no overshoot). Heading uses the
 * shortest angular path so 350° ↔ 10° animates through 0°, not back through 180°.
 *
 * prefers-reduced-motion: animators snap directly to the target and never wake
 * the loop, satisfying the reduced-motion contract.
 *
 * An animator that sets `map: true` moves something drawn on the map, and
 * steps at most at the map rate (Settings, Appearance, Map: 60 Hz unless the
 * operator picks 30). A display frame that comes sooner passes it over
 * without touching the DOM, so Chromium, and in the desktop app Qt after it,
 * have nothing new of it to composite: with an aircraft flying in 3D, those
 * frames were most of what the app cost in CPU. Every other animator, the
 * flight instruments among them, steps on every display frame as before.
 */
Corvus.anim = (function () {
  const animators = new Set();
  let rafId = null;
  let lastTime = 0;
  const MAP_RATES = [30, 60];
  const DEFAULT_MAP_RATE = 60;
  // A display frame is never exactly on time; without the slack, frames 16.6
  // and 16.8 ms apart would make 30 Hz step on every third frame instead of
  // every second.
  const FRAME_SLACK_MS = 2;
  let mapRate = DEFAULT_MAP_RATE;
  let mapInterval = 1000 / mapRate - FRAME_SLACK_MS;
  // Per map animator: when it last stepped, and whether it was still moving.
  const mapLast = new Map();

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
      let stepDt = dt;
      if (a.map) {
        const last = mapLast.get(a);
        const since = last ? t - last.t : -1;
        if (since >= 0 && since < mapInterval) {
          if (last.moving) alive = true;
          continue;
        }
        if (since >= 0) stepDt = Math.min(since / 1000, 0.05);
      }
      let stillMoving = false;
      try { stillMoving = a.step(stepDt, t); } catch (_) { animators.delete(a); mapLast.delete(a); }
      if (a.map) mapLast.set(a, { t, moving: stillMoving });
      if (stillMoving) alive = true;
    }
    if (alive) rafId = raf(frame);
    else lastTime = 0;
  }

  function add(animator) { animators.add(animator); return animator; }
  function remove(animator) { animators.delete(animator); mapLast.delete(animator); }
  /** Start the loop if it is not already running. Idempotent. */
  function wake() { if (rafId == null) { lastTime = 0; rafId = raf(frame); } }
  /** Cap the map's animators at *hz* steps a second: 30 or 60, anything
   *  else is 60. Returns the rate in force. */
  function setMapRate(hz) {
    mapRate = MAP_RATES.includes(Number(hz)) ? Number(hz) : DEFAULT_MAP_RATE;
    mapInterval = 1000 / mapRate - FRAME_SLACK_MS;
    return mapRate;
  }
  function getMapRate() { return mapRate; }
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
    add, remove, wake, reducedMotion, setMapRate, mapRate: getMapRate, MAP_RATES, DEFAULT_MAP_RATE,
    lerp, approach, normAngle, shortestDelta, approachAngle, settled, settledAngle,
    // test hooks
    _animators: () => animators,
    _rafId: () => rafId,
    _reset: () => { if (rafId != null) cancelRaf(rafId); rafId = null; lastTime = 0; animators.clear(); mapLast.clear(); },
  };
})();
