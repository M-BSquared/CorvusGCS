"use strict";

/**
 * Frontend tests for Corvus.ui.popover / Corvus.ui.infoHint — the hint icon
 * and the glass sheet it opens.
 *
 * The component exists so a row can stop printing its own explanation: the
 * text has to be reachable by pointer AND by keyboard, and it must never
 * leave a listener or a node on the body behind when it closes.
 *
 * The two ways in are the point of most of what follows. A hover is a glance
 * — it waits for the pointer to mean it, it is transparent to the pointer
 * while it is up, and it goes when the pointer goes. A press (click, tap,
 * keyboard focus) is a request to read — immediate, pinned, pointer-capturing
 * and dismissed only deliberately. The old sheet caught its own pointerenter
 * and cancelled the close it had just scheduled, which is how brushing a hint
 * left one hanging on screen.
 *
 * Plus the placement maths — the sheet flips above an anchor near the bottom
 * of the screen the way the dropdown's list does.
 *
 * Plain Node-runnable assertions (no browser, no test runner), following the
 * DOM-stub pattern of tests/test_frontend_dropdown.js.
 *
 * Run:
 *   node tests/test_frontend_popover.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};
global.innerWidth = 1280;
global.innerHeight = 720;

global.Event = class Event {
  constructor(type, options = {}) { this.type = type; this.bubbles = !!options.bubbles; }
};

global.MutationObserver = class MutationObserver {
  constructor(cb) { this.cb = cb; }
  observe() {}
  disconnect() {}
};

// The hover-intent and hide grace periods are real timers; tests drive them
// by hand.  runTimers() fires everything due, in the order it was scheduled.
const timers = [];
let timerSeq = 0;
global.setTimeout = (cb, ms) => { const id = ++timerSeq; timers.push({ id, cb, ms }); return id; };
global.clearTimeout = (id) => {
  const i = timers.findIndex((t) => t.id === id);
  if (i >= 0) timers.splice(i, 1);
};
function runTimers() {
  const due = timers.splice(0, timers.length);
  due.forEach((t) => t.cb());
}

// ---------------------------------------------------------------------------
// DOM stub — children, listeners, focus, attributes and a settable box.
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    nodeType: 1,
    className: "", children: [], dataset: {}, style: {},
    type: "", hidden: false, disabled: false, value: "", id: "", title: "",
    tabIndex: 0, _attrs: {}, _listeners: {}, _text: "",
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
    contains(c) { return e.className.split(/\s+/).includes(c); },
    toggle(c, force) {
      const next = force === undefined ? !e.classList.contains(c) : !!force;
      if (next) e.classList.add(c); else e.classList.remove(c);
      return next;
    },
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
  e.getBoundingClientRect = () => ({
    top: e._box.top, left: e._box.left,
    width: e._box.width, height: e._box.height,
    bottom: e._box.top + e._box.height, right: e._box.left + e._box.width,
  });
  e.querySelectorAll = (sel) => {
    const out = [];
    (function walk(list) {
      list.forEach((c) => {
        if (String(c.className).split(/\s+/).includes(String(sel).replace(/^\./, ""))) out.push(c);
        walk(c.children);
      });
    })(e.children);
    return out;
  };
  e.querySelector = (sel) => e.querySelectorAll(sel)[0] || null;
  Object.defineProperty(e, "offsetWidth", { get: () => e._box.width });
  Object.defineProperty(e, "offsetHeight", { get: () => e._box.height });
  Object.defineProperty(e, "scrollHeight", { get: () => e._box.height });
  Object.defineProperty(e, "firstChild", { get: () => e.children[0] || null });
  return e;
}

const documentListeners = {};
const windowListeners = {};
const body = makeEl("body");
global.document = {
  body,
  activeElement: null,
  createElement: makeEl,
  querySelectorAll: (sel) => body.querySelectorAll(sel),
  querySelector: (sel) => body.querySelector(sel),
  addEventListener: (t, cb) => { (documentListeners[t] = documentListeners[t] || []).push(cb); },
  removeEventListener: (t, cb) => {
    const list = documentListeners[t] || [];
    const i = list.indexOf(cb);
    if (i >= 0) list.splice(i, 1);
  },
};
global.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
global.removeEventListener = (t, cb) => {
  const list = windowListeners[t] || [];
  const i = list.indexOf(cb);
  if (i >= 0) list.splice(i, 1);
};
function fireDocument(type, ev) {
  (documentListeners[type] || []).slice().forEach((cb) => cb(Object.assign({
    preventDefault() {}, stopPropagation() {},
  }, ev)));
}
function documentCount(type) { return (documentListeners[type] || []).length; }

require("../src/js/ui.js");
const ui = Corvus.ui;

/** A hint button mounted in a host, with boxes the placement maths can read. */
function mount(opts) {
  const host = makeEl("div");
  document.body.appendChild(host);
  const btn = ui.infoHint(Object.assign({
    title: "SITL / GCS UDP",
    text: "udp:0.0.0.0:14550\nWaits on the usual ground-station port.",
  }, opts || {}));
  host.appendChild(btn);
  btn._box = { top: 300, left: 400, width: 16, height: 16 };
  const pop = btn.corvusPopover;
  pop.el._box = { top: 0, left: 0, width: 260, height: 80 };
  return { host, btn, pop };
}

// ---------------------------------------------------------------------------
// The hint is a labelled button carrying an icon, and nothing is on screen yet
// ---------------------------------------------------------------------------
{
  const { host, btn, pop } = mount();
  assert.equal(btn.tagName, "BUTTON");
  assert.equal(btn.type, "button", "a button in a form must not submit it");
  assert.equal(btn.className, "ui-info");
  assert.equal(btn.getAttribute("aria-label"), "More information about SITL / GCS UDP",
    "an icon-only button needs a name a screen reader can read out");
  assert.equal(btn.title, "", "no native tooltip: it would cover the popover that replaced it");
  const glyph = btn.children[0];
  assert.equal(glyph.getAttribute("data-lucide"), "info", "the circled i");
  assert.equal(pop.isOpen(), false);
  assert.equal(pop.el.parentNode, null, "nothing is mounted until it is asked for");
  // The sheet is the dropdown's material, not a second popover style.
  assert.equal(pop.el.className, "ui-popover glass");
  assert.equal(pop.el.getAttribute("role"), "tooltip");
  // Title, then one paragraph per line of the text.
  assert.deepEqual(pop.el.children.map((c) => c.className),
    ["ui-popover-title", "ui-popover-text", "ui-popover-text"]);
  assert.equal(pop.el.children[0].textContent, "SITL / GCS UDP");
  assert.equal(pop.el.children[2].textContent, "Waits on the usual ground-station port.");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// Hover waits for intent, opens on the body and describes the anchor; leaving
// closes it again
// ---------------------------------------------------------------------------
{
  const { host, btn, pop } = mount();
  const before = documentCount("keydown");
  btn.fire("pointerenter");
  assert.equal(pop.isOpen(), false,
    "a pointer on its way past a column of hints must not set off every one");
  runTimers();
  assert.equal(pop.isOpen(), true);
  assert.equal(pop.isPinned(), false, "a glance is not a request to read");
  assert.equal(pop.el.dataset.pinned, "false");
  assert.equal(pop.el.style.pointerEvents, "none",
    "the sheet cannot be allowed to keep itself open under the pointer");
  assert.equal(pop.el.parentNode, document.body,
    "mounted on the body, so a scrolling card cannot clip it");
  assert.equal(btn.getAttribute("aria-describedby"), pop.el.id);
  assert.equal(btn.dataset.popoverOpen, "true", "the trigger shows it is the one that is open");

  btn.fire("pointerleave");
  assert.equal(pop.isOpen(), true, "a hair of grace, so the icon's edge cannot flicker it");
  runTimers();
  assert.equal(pop.isOpen(), false);
  assert.equal(pop.el.parentNode, null, "closing takes the sheet back off the body");
  assert.equal(btn.getAttribute("aria-describedby"), null,
    "a description pointing at a node that is gone is one nothing can follow");
  assert.equal(btn.dataset.popoverOpen, undefined);
  assert.equal(documentCount("keydown"), before, "and leaves no document listener behind");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// A pointer that passes over the anchor and keeps going never opens it at all
// ---------------------------------------------------------------------------
{
  const { host, btn, pop } = mount();
  btn.fire("pointerenter");
  btn.fire("pointerleave");
  runTimers();
  assert.equal(pop.isOpen(), false, "the intent was withdrawn before it was honoured");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// A hovered sheet leaves with the pointer even though it lies right where the
// pointer left — this is what used to hold it on screen
// ---------------------------------------------------------------------------
{
  const { host, btn, pop } = mount();
  btn.fire("pointerenter");
  runTimers();
  btn.fire("pointerleave");
  // In the browser an unpinned sheet is pointer-transparent, so this event
  // cannot reach it. Firing it by hand proves the close does not depend on
  // that: nothing the sheet hears keeps an unpinned sheet alive.
  pop.el.fire("pointerenter");
  runTimers();
  assert.equal(pop.isOpen(), false, "a glance ends when the pointer moves on");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// A pinned sheet is the opposite: it takes the pointer, survives the gap, and
// waits to be dismissed
// ---------------------------------------------------------------------------
{
  const { host, btn, pop } = mount();
  btn.fire("click");
  assert.equal(pop.isPinned(), true);
  assert.equal(pop.el.dataset.pinned, "true");
  assert.equal(pop.el.style.pointerEvents, "", "pinned text has to be selectable");

  btn.fire("pointerleave");
  runTimers();
  assert.equal(pop.isOpen(), true, "something asked for this one to stay");

  pop.el.fire("pointerenter");
  pop.el.fire("pointerleave");
  runTimers();
  assert.equal(pop.isOpen(), true, "and it is dismissed deliberately, not by leaving");
  fireDocument("pointerdown", { target: makeEl("div") });
  assert.equal(pop.isOpen(), false);
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// Reaching for a hint that is already showing keeps it, rather than toggling
// the sheet out from under the hand that went for it
// ---------------------------------------------------------------------------
{
  const { host, btn, pop } = mount();
  btn.fire("pointerenter");
  runTimers();
  assert.equal(pop.isPinned(), false);
  // A real press focuses the button before the click arrives.
  btn.fire("pointerdown");
  btn.fire("focus");
  btn.fire("click");
  assert.equal(pop.isOpen(), true, "the press promoted the glance instead of ending it");
  assert.equal(pop.isPinned(), true);
  btn.fire("click");
  assert.equal(pop.isOpen(), false, "a second press closes it");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// Keyboard: focus opens, Escape closes and hands the focus back, blur closes
// ---------------------------------------------------------------------------
{
  const { host, btn, pop } = mount();
  assert.equal(btn.tabIndex, 0, "the hint is reachable without a pointer");
  btn.fire("focus");
  assert.equal(pop.isOpen(), true, "the explanation exists for the keyboard too");
  assert.equal(pop.isPinned(), true, "arriving by keyboard is a request to read it");
  fireDocument("keydown", { key: "Escape" });
  assert.equal(pop.isOpen(), false);
  assert.equal(document.activeElement, btn, "Escape hands the keyboard back to the trigger");

  btn.fire("focus");
  btn.fire("blur");
  assert.equal(pop.isOpen(), false, "tabbing away closes it with no grace period");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// A tap toggles it — a touch screen has no hover to open it with
// ---------------------------------------------------------------------------
{
  const { host, btn, pop } = mount();
  btn.fire("click");
  assert.equal(pop.isOpen(), true, "opened at once: a tap has no hover to wait for");
  assert.equal(pop.isPinned(), true);
  btn.fire("click");
  assert.equal(pop.isOpen(), false);
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// One at a time, and a click elsewhere or a scroll dismisses it
// ---------------------------------------------------------------------------
{
  const a = mount({ title: "First" });
  const b = mount({ title: "Second" });
  a.btn.fire("pointerenter");
  runTimers();
  b.btn.fire("pointerenter");
  runTimers();
  assert.equal(a.pop.isOpen(), false, "two sheets would overlap each other's text");
  assert.equal(b.pop.isOpen(), true);

  fireDocument("pointerdown", { target: makeEl("div") });
  assert.equal(b.pop.isOpen(), false, "a click anywhere else dismisses it");

  b.pop.show();
  fireDocument("scroll", { target: makeEl("div") });
  assert.equal(b.pop.isOpen(), false,
    "it is placed once from the anchor's box, so a scroll would leave it hanging");

  // Scrolling the sheet's own content is not the pane moving underneath it.
  b.pop.show();
  fireDocument("scroll", { target: b.pop.el.children[0] });
  assert.equal(b.pop.isOpen(), true);
  b.pop.hide();

  a.host.parentNode.removeChild(a.host);
  b.host.parentNode.removeChild(b.host);
}

// ---------------------------------------------------------------------------
// Placement: under the anchor, centred and clamped — flipped above when the
// space below runs out
// ---------------------------------------------------------------------------
{
  const { host, btn, pop } = mount();
  pop.show();
  assert.equal(pop.el.dataset.placement, "bottom");
  assert.equal(pop.el.style.top, "324px", "8px under the anchor's bottom edge");
  // Centred on a 16px anchor at x=400: 408 - 130.
  assert.equal(pop.el.style.left, "278px");
  pop.hide();

  // Near the bottom of a 720px viewport there is no room below.
  btn._box = { top: 680, left: 400, width: 16, height: 16 };
  pop.show();
  assert.equal(pop.el.dataset.placement, "top");
  assert.equal(pop.el.style.top, "592px", "680 - 8 gap - 80 tall");
  pop.hide();

  // A hint at the right edge is pulled back inside rather than off screen.
  btn._box = { top: 300, left: 1270, width: 16, height: 16 };
  pop.show();
  assert.equal(pop.el.style.left, String(1280 - 260 - 8) + "px");
  pop.hide();
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// setContent replaces the sheet, and an empty one never paints
// ---------------------------------------------------------------------------
{
  const { host, btn, pop } = mount();
  pop.setContent({ title: "Serial port", text: "No port is connected." });
  pop.show();
  assert.deepEqual(pop.el.children.map((c) => c.textContent),
    ["Serial port", "No port is connected."]);

  pop.setContent({});
  pop.hide();
  pop.show();
  assert.equal(pop.isOpen(), false, "nothing to say is no reason to paint an empty sheet");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// popover() attaches to any anchor, once, and destroy() releases it
// ---------------------------------------------------------------------------
{
  const anchor = makeEl("span");
  document.body.appendChild(anchor);
  anchor._box = { top: 100, left: 100, width: 40, height: 14 };
  const p = ui.popover(anchor, { text: "Link quality over the last minute." });
  p.el._box = { top: 0, left: 0, width: 200, height: 40 };
  assert.equal(ui.popover(anchor), p, "attaching twice returns the same handle");
  p.show();
  assert.equal(p.isOpen(), true);
  p.destroy();
  assert.equal(p.isOpen(), false);
  assert.equal(p.el.parentNode, null);
  assert.equal(anchor.corvusPopover, null, "the anchor is released");
  document.body.removeChild(anchor);
}

console.log("test_frontend_popover.js: all assertions passed");
