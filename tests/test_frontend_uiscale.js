"use strict";

/**
 * The interface scale and the two coordinate spaces it creates.
 *
 * Corvus.scale sizes the whole app with a CSS `zoom` on <body>. That is one
 * token write and it reaches everything — but it splits the DOM's geometry in
 * two, and the halves look identical:
 *
 *   SCALED    getBoundingClientRect(), pointer clientX/clientY,
 *             window.innerWidth / innerHeight
 *   UNSCALED  style.left / top / maxHeight, offsetWidth / Height,
 *             clientWidth / Height, scrollHeight
 *
 * Anything that measures in one and writes in the other is off by the scale.
 * At 100% the two spaces coincide, so every such mistake is invisible until
 * an operator raises the interface size — which is how the map's layer menu
 * came to open 366px the wrong side of its own button at 150%, and the mode
 * dropdown to drop half a screen below the control it belongs to.
 *
 * The contract asserted here is a single sentence: at any scale, a surface
 * lands in the SAME place relative to its trigger, measured in the app's own
 * pixels. Every case below is run at 1.0 and again at a zoom, and the two
 * results have to agree.
 *
 * Plain Node-runnable assertions (no browser, no test runner), same pattern as
 * the other tests/test_frontend_*.js files.
 *
 * Run:
 *   node tests/test_frontend_uiscale.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};
global.Event = class Event {
  constructor(type, options = {}) { this.type = type; this.bubbles = !!options.bubbles; }
};
global.MutationObserver = class MutationObserver {
  constructor(cb) { this.cb = cb; }
  observe() {}
  disconnect() {}
};

// The window never changes size here; only the zoom inside it does. That is
// the whole point — the operator raises the interface size on the machine
// they already have.
const WINDOW_W = 1280;
const WINDOW_H = 720;
global.innerWidth = WINDOW_W;
global.innerHeight = WINDOW_H;

// ---------------------------------------------------------------------------
// DOM stub that models `zoom`.
// ---------------------------------------------------------------------------
// _box is the element's box in UNSCALED pixels — the space a stylesheet and
// style.left are written in. getBoundingClientRect multiplies by the zoom, as
// the browser does; offsetWidth/clientWidth/scrollHeight do not, as the
// browser also does. `zoom` below is the live scale, and <body> carries it.
let zoom = 1;

function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    nodeType: 1,
    className: "", children: [], dataset: {}, style: {},
    type: "", hidden: false, disabled: false, value: "", id: "", title: "",
    tabIndex: 0, _attrs: {}, _listeners: {}, _text: "", _isEl: true,
    parentNode: null, parentElement: null,
    _box: { top: 0, left: 0, width: 0, height: 0 },
  };
  Object.defineProperty(e, "textContent", {
    get() {
      return e.children.length ? e.children.map((c) => c.textContent).join("") : e._text;
    },
    set(v) { e._text = String(v == null ? "" : v); e.children = []; },
  });
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) {
      const next = force === undefined ? !e.classList.contains(c) : !!force;
      if (next) e.classList.add(c); else e.classList.remove(c);
      return next;
    },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => {
    if (c.parentNode) c.parentNode.removeChild(c);
    c.parentNode = e; c.parentElement = e;
    e.children.push(c);
    return c;
  };
  e.removeChild = (c) => {
    const i = e.children.indexOf(c);
    if (i >= 0) e.children.splice(i, 1);
    c.parentNode = null; c.parentElement = null;
    return c;
  };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.removeAttribute = (k) => { delete e._attrs[k]; };
  e.hasAttribute = (k) => k in e._attrs;
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.removeEventListener = (t, cb) => {
    const list = e._listeners[t] || [];
    const i = list.indexOf(cb);
    if (i >= 0) list.splice(i, 1);
  };
  e.dispatchEvent = (ev) => {
    (e._listeners[ev.type] || []).slice().forEach((cb) => cb(Object.assign(ev, {
      preventDefault() {}, stopPropagation() {}, target: e,
    })));
    return true;
  };
  e.fire = (type, ev) => e.dispatchEvent(Object.assign(new global.Event(type), ev || {}));
  e.focus = () => { global.document.activeElement = e; };
  // THE stub that matters: a client rect is in scaled pixels.
  e.getBoundingClientRect = () => ({
    top: e._box.top * zoom, left: e._box.left * zoom,
    width: e._box.width * zoom, height: e._box.height * zoom,
    bottom: (e._box.top + e._box.height) * zoom,
    right: (e._box.left + e._box.width) * zoom,
  });
  e.querySelectorAll = (sel) => {
    const out = [];
    (function walk(list) {
      list.forEach((c) => {
        if (!c || !c._isEl) return;
        if (String(c.className).split(/\s+/).includes(String(sel).replace(/^\./, ""))) out.push(c);
        walk(c.children);
      });
    })(e.children);
    return out;
  };
  e.querySelector = (sel) => e.querySelectorAll(sel)[0] || null;
  // ...and these are not.
  Object.defineProperty(e, "offsetWidth", { get: () => e._box.width });
  Object.defineProperty(e, "offsetHeight", { get: () => e._box.height });
  Object.defineProperty(e, "scrollHeight", { get: () => e._box.height });
  Object.defineProperty(e, "clientWidth", { get: () => e._box.width });
  Object.defineProperty(e, "clientHeight", { get: () => e._box.height });
  Object.defineProperty(e, "firstChild", { get: () => e.children[0] || null });
  return e;
}

const documentListeners = {};
const body = makeEl("body");
// <body> is the probe Corvus.ui.uiScale() measures, and it fills the window.
// Its unscaled width is therefore the window divided by the zoom, which is
// exactly the relationship the helper reads back out.
Object.defineProperty(body, "_box", {
  get: () => ({ top: 0, left: 0, width: WINDOW_W / zoom, height: WINDOW_H / zoom }),
});

global.document = {
  body,
  activeElement: null,
  createElement: makeEl,
  getElementById: () => null,
  querySelectorAll: (sel) => body.querySelectorAll(sel),
  querySelector: (sel) => body.querySelector(sel),
  addEventListener: (t, cb) => { (documentListeners[t] = documentListeners[t] || []).push(cb); },
  removeEventListener: (t, cb) => {
    const list = documentListeners[t] || [];
    const i = list.indexOf(cb);
    if (i >= 0) list.splice(i, 1);
  },
};
global.addEventListener = () => {};
global.removeEventListener = () => {};

require("../src/js/ui.js");
const ui = Corvus.ui;

const px = (v) => Math.round(parseFloat(v));

// ---------------------------------------------------------------------------
// uiScale() — the measurement everything else rests on
// ---------------------------------------------------------------------------
zoom = 1;
assert.equal(ui.uiScale(), 1, "at 100% the two spaces coincide");
zoom = 1.5;
assert.equal(ui.uiScale(), 1.5, "the ratio of <body>'s two boxes IS the zoom");
zoom = 0.8;
assert.equal(ui.uiScale(), 0.8, "and it reads a shrunk interface as readily");
zoom = 1;

// A document with nothing to measure answers 1 rather than throwing: a
// surface placed at 100% is wrong by nothing.
const realBody = global.document.body;
global.document.body = makeEl("body");            // a body with a zero box
assert.equal(ui.uiScale(), 1, "an unmeasurable document is treated as unscaled");
global.document.body = realBody;

// ---------------------------------------------------------------------------
// The contract: a dropdown lands in the same place at every scale
// ---------------------------------------------------------------------------
/** Open a menu under a trigger at *scale*, and report where it landed. */
function dropdownAt(scale, opts, triggerBox) {
  zoom = scale;
  const trigger = makeEl("button");
  trigger._box = Object.assign({ top: 100, left: 200, width: 130, height: 30 }, triggerBox || {});
  document.body.appendChild(trigger);

  const handle = ui.menu(Object.assign({
    render(surface) {
      return [1, 2, 3].map((n) => {
        const row = ui.menuItem({ label: "Row " + n, onSelect() {} });
        surface.appendChild(row);
        return row;
      });
    },
  }, opts || {}));
  handle.el._box = { top: 0, left: 0, width: 150, height: 120 };
  handle.open({ el: trigger });

  const landed = {
    left: px(handle.el.style.left),
    top: px(handle.el.style.top),
    minWidth: handle.el.style.minWidth ? px(handle.el.style.minWidth) : null,
    placement: handle.el.dataset.placement,
  };
  landed.maxHeight = px(handle.el.style.maxHeight);
  handle.close(false);
  document.body.removeChild(trigger);
  zoom = 1;
  return landed;
}

/** The four numbers that say WHERE a surface went. maxHeight is deliberately
 *  not among them: it is how much window is left below the trigger, and a
 *  bigger interface genuinely has less of it — the same answer a shorter
 *  window gives at 100%. */
function placement(landed) {
  return {
    left: landed.left, top: landed.top,
    minWidth: landed.minWidth, placement: landed.placement,
  };
}

// Below the trigger. The surface is placed in the app's own pixels, so the
// numbers written must not move when the interface is resized — the zoom is
// what makes them bigger on screen, and doing it twice is the bug.
{
  const at100 = dropdownAt(1);
  assert.deepEqual(placement(at100), {
    left: 200, top: 136, minWidth: 130, placement: "bottom",
  }, "100%: 6px under the trigger, left edges aligned");

  for (const scale of [0.8, 0.9, 1.1, 1.25, 1.5]) {
    assert.deepEqual(placement(dropdownAt(scale)), placement(at100),
      `at ${scale * 100}% the list opens exactly where it does at 100%`);
  }

  // The cap on a long list is the other half of the same sum: the room below
  // the trigger, in the app's pixels. It shrinks as the interface grows,
  // which is what makes a list scroll instead of running off the screen.
  for (const scale of [0.8, 1, 1.25, 1.5]) {
    const room = WINDOW_H / scale - 130 - 6 - 8;   // trigger bottom, GAP, EDGE
    assert.equal(dropdownAt(scale).maxHeight, Math.round(room),
      `at ${scale * 100}% the list is capped to the room it has`);
  }
}

// Aligned to the trigger's right edge — the other axis of the same sum.
{
  const at100 = dropdownAt(1, { align: "end" });
  for (const scale of [0.8, 1.25, 1.5]) {
    assert.deepEqual(placement(dropdownAt(scale, { align: "end" })), placement(at100),
      `align:end survives ${scale * 100}%`);
  }
}

// side:"left" — the map rail's popovers, measured on the horizontal axis.
//
// The rail is against the right edge of the app, and where that edge IS is
// the thing the scale moves: at 125% a 1280px window ends at 1024 of the
// app's own pixels. So the fixture follows the edge, as the stylesheet does,
// and what has to hold across scales is the gap, not an absolute x.
{
  function railAt(scale) {
    const inset = 14;
    const edge = WINDOW_W / scale;
    const box = { top: 220, left: edge - inset - 34, width: 34, height: 34 };
    const landed = dropdownAt(scale, { side: "left", matchAnchorWidth: false }, box);
    return {
      placement: landed.placement,
      gap: Math.round(box.left - (landed.left + 150)),  // rail edge to surface edge
      top: landed.top - box.top,
    };
  }
  const at100 = railAt(1);
  assert.deepEqual(at100, { placement: "left", gap: 6, top: 0 },
    "100%: beside the rail by the default gap, top-aligned with its button");
  for (const scale of [0.8, 1.25, 1.5]) {
    assert.deepEqual(railAt(scale), at100,
      `the rail's popover holds its gap and its alignment at ${scale * 100}%`);
  }

  // Hard against the LEFT edge instead: it flips rather than opening off
  // screen, and it makes that decision from the room the app has.
  for (const scale of [1, 1.25, 1.5]) {
    const landed = dropdownAt(scale, { side: "left", matchAnchorWidth: false },
      { top: 220, left: 8, width: 34, height: 34 });
    assert.equal(landed.placement, "right", `flipped at ${scale * 100}%`);
    assert.equal(landed.left, 48, "8 + 34 + 6, in the app's own pixels");
  }
}

// A trigger near the bottom flips the list above itself. The decision is made
// from the room BELOW, and that room is the window divided by the scale — at
// 150% a control 600px down is already past the fold, so the flip has to
// happen there and not only at 100%.
{
  const low = { top: 640, left: 200, width: 130, height: 30 };
  assert.equal(dropdownAt(1, {}, low).placement, "top",
    "100%: 50px of room under a 120px list is not enough");
  assert.equal(dropdownAt(1.5, {}, low).placement, "top",
    "150%: the same control, the same answer");

  // And a trigger with room below keeps the list below at every scale.
  const high = { top: 40, left: 200, width: 130, height: 30 };
  for (const scale of [1, 1.25, 1.5]) {
    assert.equal(dropdownAt(scale, {}, high).placement, "bottom",
      `a control at the top of the window drops its list downwards at ${scale * 100}%`);
  }
}

// The window's far edge is the window's, not the window's times the scale: a
// list against the right edge is clamped to the room the app actually has.
{
  const edge = { top: 100, left: 1180, width: 90, height: 30 };
  const at100 = dropdownAt(1, {}, edge);
  assert.equal(at100.left, WINDOW_W - 150 - 8, "clamped 8px clear of the right edge");
  assert.equal(dropdownAt(1.5, {}, edge).left, Math.round(WINDOW_W / 1.5) - 150 - 8,
    "at 150% the edge is 853px in, because that is where the window now ends");
  assert.ok(dropdownAt(1.5, {}, edge).left + 150 <= WINDOW_W / 1.5,
    "and the surface is inside it, not hanging off");
}

// ---------------------------------------------------------------------------
// The same contract for the popover
// ---------------------------------------------------------------------------
function popoverAt(scale, anchorBox) {
  zoom = scale;
  const anchor = makeEl("button");
  anchor._box = Object.assign({ top: 200, left: 400, width: 20, height: 20 }, anchorBox || {});
  document.body.appendChild(anchor);
  const pop = ui.popover(anchor, { title: "Why", text: "Because." });
  pop.el._box = { top: 0, left: 0, width: 260, height: 90 };
  pop.show(true);
  const landed = {
    left: px(pop.el.style.left),
    top: px(pop.el.style.top),
    placement: pop.el.dataset.placement,
  };
  pop.hide();
  document.body.removeChild(anchor);
  zoom = 1;
  return landed;
}

{
  const at100 = popoverAt(1);
  assert.deepEqual(at100, { left: 280, top: 228, placement: "bottom" },
    "100%: centred on the anchor, 8px under it");
  for (const scale of [0.8, 1.1, 1.25, 1.5]) {
    assert.deepEqual(popoverAt(scale), at100,
      `the hint sheet stays on its anchor at ${scale * 100}%`);
  }

  // Near the bottom it flips above, at every scale — the room below is the
  // window's, measured in the app's pixels.
  const low = { top: 660, left: 400, width: 20, height: 20 };
  assert.equal(popoverAt(1, low).placement, "top");
  assert.equal(popoverAt(1.5, low).placement, "top");
}

// ---------------------------------------------------------------------------
// A point anchor is already unscaled, and must be left alone
// ---------------------------------------------------------------------------
// The map's context menu is positioned from map.project(), which reports the
// container's own pixels — the same space style.left writes in. Dividing that
// by the scale would be the original bug with its sign flipped.
{
  const host = makeEl("div");
  host._box = { top: 0, left: 0, width: 1000, height: 700 };
  document.body.appendChild(host);

  function pointAt(scale) {
    zoom = scale;
    const handle = ui.menu({
      host,
      closeOnScroll: false,
      render(surface) {
        const row = ui.menuItem({ label: "Fly to", onSelect() {} });
        surface.appendChild(row);
        return [row];
      },
    });
    handle.el._box = { top: 0, left: 0, width: 186, height: 120 };
    handle.open({ x: 300, y: 200 });
    const landed = { left: px(handle.el.style.left), top: px(handle.el.style.top) };
    handle.close(false);
    zoom = 1;
    return landed;
  }

  assert.deepEqual(pointAt(1), { left: 312, top: 212 }, "12px clear of the point");
  for (const scale of [0.8, 1.25, 1.5]) {
    assert.deepEqual(pointAt(scale), { left: 312, top: 212 },
      `a ground point is already unscaled and stays put at ${scale * 100}%`);
  }
  document.body.removeChild(host);
}

console.log("test_frontend_uiscale.js: all assertions passed");
