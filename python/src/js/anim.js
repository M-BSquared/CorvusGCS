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
 * steps at most at the map rate (Settings, Map, Map animation: 60 Hz unless the
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
          // Kept alive even when it had settled: a wake() since may have given
          // it a new target, which would otherwise wait for the next wake.
          alive = true;
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

  // ---- one-shot UI effects (Web Animations API) ----
  // None of these touch the loop above: the browser runs them, on the
  // compositor where it can (transform and opacity only, flash's ring is the
  // one paint), and nothing is left running once they end. A second call of
  // the same kind on the same element replaces the first, so a burst of
  // telemetry or clicks never stacks animations. Without element.animate (an
  // old engine, the Node tests), under reduced motion or in a hidden window
  // they do nothing and leave() hands over at once.
  //
  // They are tuned to make the app feel faster, never slower. New content is
  // on screen in the first frame and only settles into place; the curve does
  // most of its travel in the first third; leaving is quicker than arriving,
  // because the operator has already moved on; and an operator flipping
  // through pages faster than ENTER_BURST_MS gets no entrance at all.
  const EASE = "cubic-bezier(0.2, 0.8, 0.2, 1)";
  const EASE_OUT = "cubic-bezier(0.16, 1, 0.3, 1)";
  const running = new WeakMap();
  const lastEnter = new WeakMap();
  const ENTER_BURST_MS = 300;
  const STAGGER_STEP_MS = 16;
  const STAGGER_MAX = 6;

  function play(el, kind, keyframes, opts) {
    if (!el || typeof el.animate !== "function" || reducedMotion()) return null;
    if (typeof document !== "undefined" && document.hidden) return null;
    let byKind = running.get(el);
    if (!byKind) { byKind = {}; running.set(el, byKind); }
    if (byKind[kind]) byKind[kind].cancel();
    let a;
    try { a = el.animate(keyframes, Object.assign({ easing: EASE }, opts)); } catch (_) { return null; }
    byKind[kind] = a;
    const clear = () => { if (byKind[kind] === a) delete byKind[kind]; };
    a.addEventListener("finish", clear);
    a.addEventListener("cancel", clear);
    return a;
  }

  /** Settle in from a short rise, already mostly visible.
   *  opts: {y = 3 px, duration = 140, delay = 0, group}. *group* is any
   *  object the burst rule counts by instead of the element, so the panels
   *  of one tab row are one burst. */
  function enter(el, opts) {
    const o = opts || {};
    if (!el) return null;
    const t = now();
    const key = o.group || el;
    const prev = lastEnter.get(key);
    lastEnter.set(key, t);
    if (!o.delay && prev != null && t - prev < ENTER_BURST_MS) return null;
    const y = o.y == null ? 3 : o.y;
    const from = { opacity: 0.25 };
    const to = { opacity: 1 };
    if (y) { from.transform = `translateY(${y}px)`; to.transform = "none"; }
    return play(el, "enter", [from, to], {
      duration: o.duration || 140, delay: o.delay || 0, easing: EASE_OUT,
      fill: o.delay ? "backwards" : "none",
    });
  }

  /** Fade out, then call *done* exactly once (at once when nothing animates).
   *  The element stops taking the pointer while it goes. */
  function leave(el, done, opts) {
    const o = opts || {};
    let called = false;
    const finish = () => { if (called) return; called = true; if (typeof done === "function") done(); };
    const y = o.y == null ? 2 : o.y;
    const a = play(el, "leave", [
      { opacity: 1, transform: "none" },
      { opacity: 0, transform: `translateY(${y}px) scale(${o.scale || 0.985})` },
    ], { duration: o.duration || 90, easing: "ease-in", fill: "forwards" });
    if (!a) { finish(); return null; }
    el.style.pointerEvents = "none";
    a.addEventListener("finish", finish);
    a.addEventListener("cancel", finish);
    return a;
  }

  /** A short swell that says "this value just changed". */
  function pop(el, opts) {
    const o = opts || {};
    const origin = o.origin || "center";
    return play(el, "pop", [
      { transform: "scale(1)", transformOrigin: origin },
      { transform: `scale(${o.scale || 1.06})`, transformOrigin: origin, offset: 0.35 },
      { transform: "scale(1)", transformOrigin: origin },
    ], { duration: o.duration || 220, easing: EASE_OUT });
  }

  /** A small sideways shake for input the vehicle or the form refused. */
  function shake(el) {
    return play(el, "shake", [
      { transform: "translateX(0)" },
      { transform: "translateX(-4px)", offset: 0.2 },
      { transform: "translateX(4px)", offset: 0.45 },
      { transform: "translateX(-2px)", offset: 0.7 },
      { transform: "translateX(0)" },
    ], { duration: 280, easing: "ease-out" });
  }

  const FLASH_TONES = { ok: "var(--healthy)", warn: "var(--warning)", err: "var(--critical)", accent: "var(--accent-bright)" };
  /** A ring that blooms out of the element and fades: a write confirmed
   *  ("ok"), refused ("err"), or "warn" / "accent". */
  function flash(el, tone) {
    const color = FLASH_TONES[tone] || FLASH_TONES.ok;
    return play(el, "flash", [
      { boxShadow: "0 0 0 0 transparent" },
      { boxShadow: `0 0 0 2px ${color}`, offset: 0.2 },
      { boxShadow: "0 0 0 6px transparent" },
    ], { duration: 520, easing: "ease-out" });
  }

  /** enter() each node a beat after the one before, the first STAGGER_MAX of
   *  them; the rest arrive with the last, so the whole cascade is over in
   *  about a tenth of a second however long the list. */
  function stagger(nodes, opts) {
    const o = opts || {};
    const step = o.step || STAGGER_STEP_MS;
    const list = Array.from(nodes || []);
    return list.map((n, i) => enter(n, {
      y: o.y == null ? 4 : o.y, duration: o.duration, delay: Math.min(i, STAGGER_MAX) * step,
    }));
  }

  return {
    add, remove, wake, reducedMotion, setMapRate, mapRate: getMapRate, MAP_RATES, DEFAULT_MAP_RATE,
    lerp, approach, normAngle, shortestDelta, approachAngle, settled, settledAngle,
    enter, leave, pop, shake, flash, stagger,
    // test hooks
    _animators: () => animators,
    _rafId: () => rafId,
    _reset: () => { if (rafId != null) cancelRaf(rafId); rafId = null; lastTime = 0; animators.clear(); mapLast.clear(); },
  };
})();
