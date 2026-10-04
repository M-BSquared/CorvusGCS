"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.scale — the interface size.

  One number multiplied over every used length below <body> via the --ui-scale
  token and the `zoom` rule in css/main.css. It is a token write and nothing
  else: no component knows about it, exactly as no component knows which theme
  is active, which is why it reaches text, icons (whose px sizes JS writes
  inline, out of reach of any font-size lever), bar heights and hairlines
  alike.

  STEPS is a short ordered list rather than a continuous range because the
  choice is coarse — an operator picks "a bit bigger", not 113%. The values are
  the contract with the settings slider and with the backend config
  (`ui.scale`); the labels are only what the step indicators read.

  Persistence mirrors Corvus.theme: localStorage first, so the size is applied
  before first paint and the app never resizes itself in front of the operator,
  and the backend config second, so it survives a cache clear.

  Two things do have to be told when it changes, and they are why this is an
  event: geometry read back out of the DOM in JS is in scaled pixels while
  style writes are in unscaled ones (hud-panel.js), and MapLibre sizes its
  drawing buffer from the container's unscaled size, which would leave the map
  soft when the UI grows and needlessly oversampled when it shrinks
  (map.js). A plain `resize` is dispatched alongside so anything that already
  reacts to a viewport change — the right panel's auto-collapse, MapLibre's
  own observer, Plotly — needs no new listener.

  It also owns the responsive breakpoints, because it is the only thing that
  knows the difference between the window and the room the app has. See
  BREAKPOINTS below.
*/
Corvus.scale = (function () {
  const KEY = "corvus.scale";
  const DEFAULT = 1;

  const STEPS = [
    { value: 0.8,  label: "80%" },
    { value: 0.9,  label: "90%" },
    { value: 1,    label: "100%" },
    { value: 1.1,  label: "110%" },
    { value: 1.25, label: "125%" },
    { value: 1.5,  label: "150%" },
  ];

  const MIN = STEPS[0].value;
  const MAX = STEPS[STEPS.length - 1].value;

  /** Clamp *v* into the offered range, or the default when it is not a
   *  number at all. Never snaps to a step: a config written by a build with
   *  a different STEPS list stays honoured, and the slider shows the nearest
   *  step for it. */
  function normalize(v) {
    const n = Number(v);
    if (!isFinite(n) || n <= 0) return DEFAULT;
    return Math.min(Math.max(n, MIN), MAX);
  }

  /*
    BREAKPOINTS — the responsive rules' condition, in the app's own pixels.

    `zoom` is invisible to a media query: it measures the window, which stays
    1440px wide however small the app's own pixels have become inside it. So
    the width media queries in css/main.css became selectors guarded by this
    attribute, and this is what writes it.

    Each token names the media query it replaces, so a rule and its condition
    still read the same way ("max-720" is `(max-width: 720px)`). A token is
    present when the EFFECTIVE viewport — the window divided by the scale —
    satisfies it, which at 100% is the window itself and therefore exactly
    what the media queries did.

    documentElement.clientWidth rather than innerWidth: that is the width a
    media query would have measured, scrollbar excluded, and matching it is
    the whole point.
  */
  const BREAKPOINTS = [
    { token: "max-1279", test: (w) => w <= 1279 },
    { token: "max-959",  test: (w) => w <= 959 },
    { token: "max-900",  test: (w) => w <= 900 },
    { token: "max-720",  test: (w) => w <= 720 },
    { token: "max-600",  test: (w) => w <= 600 },
    { token: "min-760",  test: (w) => w >= 760 },
  ];

  /** The viewport in the pixels the app lays out in: the window, unzoomed. */
  function effectiveWidth(n) {
    const root = document.documentElement;
    const px = (root && root.clientWidth) || window.innerWidth || 0;
    const scale = n > 0 ? n : 1;
    return px / scale;
  }

  /** Write the breakpoint tokens for scale *n*. Guarded like every other
   *  document write here: a browser that refuses the attribute leaves the
   *  widest layout, never an unstyled one. */
  function applyBreakpoints(n) {
    try {
      const width = effectiveWidth(n);
      const tokens = BREAKPOINTS.filter((b) => b.test(width)).map((b) => b.token);
      document.documentElement.setAttribute("data-vw", tokens.join(" "));
    } catch (_e) {}
  }

  /** Apply *v* to the document and cache it. Returns the value applied. */
  function setScale(v) {
    const n = normalize(v);
    try { document.documentElement.style.setProperty("--ui-scale", String(n)); } catch (_e) {}
    // Before the events, not after: a listener that measures the layout must
    // find the breakpoints already settled, or it measures the widest one.
    applyBreakpoints(n);
    try { localStorage.setItem(KEY, String(n)); } catch (_e) {}
    try {
      window.dispatchEvent(new CustomEvent("corvus:scalechange", { detail: { scale: n } }));
      window.dispatchEvent(new Event("resize"));
    } catch (_e) {}
    return n;
  }

  /* A window resize moves the same line without the scale changing. Bound at
     load rather than from init(), because the attribute is part of the
     layout and must not wait for a page to be opened. */
  try {
    window.addEventListener("resize", () => { applyBreakpoints(get()); });
  } catch (_e) {}

  /** Apply the locally cached scale (called before the config fetch lands). */
  function applySaved() {
    let v = DEFAULT;
    try { v = localStorage.getItem(KEY) || DEFAULT; } catch (_e) {}
    return setScale(v);
  }

  /** The scale the current document is at, whatever set it. */
  function get() {
    try {
      return normalize(getComputedStyle(document.documentElement).getPropertyValue("--ui-scale"));
    } catch (_e) {
      return DEFAULT;
    }
  }

  /**
   * Resolve the scale a backend config asks for, or null when it names none
   * (so the caller leaves the cached value alone). A non-numeric or
   * out-of-range value is clamped rather than rejected — a config must never
   * be able to leave the UI at an unusable size, and must never fail to load.
   */
  function fromConfig(cfg) {
    const ui = cfg && cfg.ui;
    if (!ui || ui.scale == null) return null;
    const n = Number(ui.scale);
    if (!isFinite(n) || n <= 0) return null;
    return normalize(n);
  }

  return {
    setScale, applySaved, get, normalize, fromConfig,
    STEPS, BREAKPOINTS, DEFAULT, MIN, MAX,
  };
})();
