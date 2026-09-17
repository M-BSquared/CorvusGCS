"use strict";

/**
 * Frontend tests for Corvus.ui.menu / Corvus.ui.menuItem — the one dropdown
 * surface every list in the app opens on.
 *
 * The component was extracted from enhanceSelect(), so the select's own
 * contract is still covered by tests/test_frontend_dropdown.js and the map's
 * by tests/test_frontend_mapmenu.js. What is asserted here is what belongs to
 * the component itself and is shared by all three: the two ways a surface can
 * be anchored, the rows it builds, the keyboard, and every path that takes it
 * back down.
 *
 * Plain Node-runnable assertions (no browser, no test runner), following the
 * DOM-stub pattern of tests/test_frontend_dropdown.js.
 *
 * Run:
 *   node tests/test_frontend_menu.js
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

// ---------------------------------------------------------------------------
// DOM stub. Tracks children, listeners, focus and a settable box, which is all
// the component touches.
// ---------------------------------------------------------------------------
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
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.removeEventListener = (t, cb) => {
    const list = e._listeners[t] || [];
    const i = list.indexOf(cb);
    if (i >= 0) list.splice(i, 1);
  };
  e.fire = (type, ev) => (e._listeners[type] || []).slice().forEach((cb) => cb(Object.assign({
    preventDefault() {}, stopPropagation() {}, target: e,
  }, ev)));
  e.focus = () => { global.document.activeElement = e; };
  e.getBoundingClientRect = () => ({
    top: e._box.top, left: e._box.left,
    width: e._box.width, height: e._box.height,
    bottom: e._box.top + e._box.height, right: e._box.left + e._box.width,
  });
  e.querySelector = () => null;
  e.querySelectorAll = () => [];
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
global.document = {
  body,
  activeElement: null,
  createElement: makeEl,
  querySelectorAll: () => [],
  querySelector: () => null,
  contains: (node) => {
    let n = node;
    while (n) { if (n === body) return true; n = n.parentNode; }
    return false;
  },
  addEventListener: (t, cb) => { (documentListeners[t] = documentListeners[t] || []).push(cb); },
  removeEventListener: (t, cb) => {
    const list = documentListeners[t] || [];
    const i = list.indexOf(cb);
    if (i >= 0) list.splice(i, 1);
  },
};
const windowListeners = {};
global.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
global.removeEventListener = (t, cb) => {
  const list = windowListeners[t] || [];
  const i = list.indexOf(cb);
  if (i >= 0) list.splice(i, 1);
};
function fireWindow(type, ev) {
  (windowListeners[type] || []).slice().forEach((cb) => cb(Object.assign({ type }, ev)));
}
function fireDocument(type, ev) {
  (documentListeners[type] || []).slice().forEach((cb) => cb(Object.assign({
    preventDefault() {}, stopPropagation() {},
  }, ev)));
}
function listenerCount(type) { return (documentListeners[type] || []).length; }

require("../src/js/ui.js");
const ui = Corvus.ui;

/** A menu of plain rows, plus the trigger it hangs off. */
function build(opts, labels) {
  const trigger = makeEl("button");
  trigger._box = { top: 100, left: 200, width: 130, height: 30 };
  body.appendChild(trigger);
  const picked = [];
  const handle = ui.menu(Object.assign({
    render: (el) => (labels || ["Alpha", "Bravo", "Charlie"]).map((label, i) => {
      const row = ui.menuItem({
        label: typeof label === "string" ? label : label.label,
        note: label.note,
        disabled: !!label.disabled,
        active: !!label.active,
        value: i,
        onSelect: (v) => picked.push(v),
      });
      el.appendChild(row);
      return row;
    }),
  }, opts || {}));
  handle.el._box = { top: 0, left: 0, width: 150, height: 180 };
  return { trigger, handle, picked };
}

// ---------------------------------------------------------------------------
// menuItem: one row, three shapes
// ---------------------------------------------------------------------------
{
  const plain = ui.menuItem({ label: "Satellite", dot: true, value: "sat", role: "option", active: true });
  assert.equal(plain.className, "option-item active", "the row IS the app's option row");
  assert.equal(plain.getAttribute("role"), "option");
  assert.equal(plain.getAttribute("aria-selected"), "true", "a listbox row says whether it is the one chosen");
  assert.equal(plain.dataset.value, "sat");
  assert.deepEqual(plain.children.map((c) => c.className), ["option-item-dot", "option-item-label"]);
  assert.equal(plain.textContent, "Satellite");

  const action = ui.menuItem({ label: "Fly to this point", note: "Arm the vehicle first", icon: "navigation", disabled: true });
  assert.equal(action.getAttribute("role"), "menuitem", "an action row is a menuitem by default");
  assert.equal(action.getAttribute("aria-selected"), null, "…and is not a selection");
  assert.equal(action.disabled, true);
  assert.equal(action.children[0].tagName, "I", "the icon leads");
  assert.equal(action.children[0].getAttribute("data-lucide"), "navigation");
  const text = action.children[1];
  assert.equal(text.className, "option-item-text", "then the label/note column");
  assert.deepEqual(text.children.map((c) => c.className), ["option-item-label", "option-item-note"]);
  assert.equal(text.children[1].textContent, "Arm the vehicle first",
    "a row that cannot be used says why, rather than going quietly inert");

  // A disabled row is inert even if something dispatches a click at it.
  let ran = 0;
  const off = ui.menuItem({ label: "X", disabled: true, onSelect: () => { ran++; } });
  off.fire("click");
  assert.equal(ran, 0);
}

// ---------------------------------------------------------------------------
// An element anchor: the list drops out of its trigger, onto the body
// ---------------------------------------------------------------------------
{
  const { trigger, handle } = build();
  assert.equal(handle.el.className, "ui-menu glass");
  assert.equal(handle.isOpen(), false);

  handle.open({ el: trigger });
  assert.equal(handle.isOpen(), true);
  assert.equal(handle.el.parentNode, document.body, "mounted away from the pane that would clip it");
  assert.equal(handle.el.dataset.anchor, "element");
  assert.equal(handle.el.style.top, "136px", "6px under the trigger's bottom edge");
  assert.equal(handle.el.style.left, "200px");
  assert.equal(handle.el.style.minWidth, "130px", "never narrower than the control it belongs to");
  assert.equal(document.activeElement, handle.rows()[0], "the keyboard starts on the first usable row");

  handle.close(true);
  assert.equal(handle.el.parentNode, null, "closing takes the surface back off the body");
  assert.equal(document.activeElement, trigger, "and hands the keyboard back to the trigger");
  trigger.parentNode.removeChild(trigger);
}

// ---------------------------------------------------------------------------
// side: "left" — a menu opened from a rail against the edge of the window
// ---------------------------------------------------------------------------
{
  const { trigger, handle } = build({ side: "left", matchAnchorWidth: false });
  trigger._box = { top: 220, left: 1230, width: 34, height: 34 };   // the map rail
  handle.open({ el: trigger });
  assert.equal(handle.el.style.left, "1074px", "beside the rail (1230 - 6 - 150), not under it");
  assert.equal(handle.el.style.top, "220px", "top-aligned with the button it belongs to");
  assert.equal(handle.el.style.minWidth, undefined,
    "matchAnchorWidth:false — a 34px rail button must not size a list of names");
  assert.equal(handle.el.dataset.placement, "left");
  handle.close(false);

  // Hard against the LEFT edge instead: it flips to the other side rather
  // than opening off screen.
  trigger._box = { top: 220, left: 8, width: 34, height: 34 };
  handle.open({ el: trigger });
  assert.equal(handle.el.style.left, "48px", "flipped to the right of the trigger (8 + 34 + 6)");
  assert.equal(handle.el.dataset.placement, "right");
  handle.close(false);
  trigger.parentNode.removeChild(trigger);
}

// ---------------------------------------------------------------------------
// A point anchor: positioned in the host's own pixels, and re-placed on demand
// ---------------------------------------------------------------------------
{
  const host = makeEl("div");
  host._box = { width: 1000, height: 700 };
  const { handle } = build({ host, closeOnScroll: false });
  handle.el._box = { top: 0, left: 0, width: 186, height: 120 };

  handle.open({ x: 300, y: 200 });
  assert.equal(handle.el.parentNode, host, "a point belongs to the container it was measured in");
  assert.equal(handle.el.dataset.anchor, "point");
  assert.equal(handle.el.style.left, "312px", "12px clear of the point");
  assert.equal(handle.el.style.top, "212px");
  assert.equal(handle.el.style.transformOrigin, "left top", "it grows out of the point");
  assert.equal(listenerCount("scroll"), 0,
    "closeOnScroll:false — a host that moves instead of scrolling re-places itself");

  // The anchor moves (a panning map): the same surface follows it.
  handle.place({ x: 180, y: 140 });
  assert.equal(handle.el.style.left, "192px");
  assert.equal(handle.el.style.top, "152px");

  // Near the far corner it flips back inside rather than off the edge.
  handle.place({ x: 950, y: 650 });
  assert.equal(handle.el.style.left, `${950 - 12 - 186}px`);
  assert.equal(handle.el.style.top, `${650 - 12 - 120}px`);
  assert.equal(handle.el.style.transformOrigin, "right bottom");
  handle.close(false);
}

// ---------------------------------------------------------------------------
// A resized window re-places the surface, and nothing else
// ---------------------------------------------------------------------------
{
  const { trigger, handle } = build();
  handle.open({ el: trigger });
  trigger._box = { top: 300, left: 400, width: 130, height: 30 };

  // The browser hands the handler an Event. It is not an anchor, and taking it
  // for one would leave the surface with nothing to measure from or return
  // the keyboard to.
  fireWindow("resize");
  assert.equal(handle.el.style.top, "336px", "the list follows the trigger the resize moved");
  assert.equal(handle.el.style.left, "400px");
  fireDocument("keydown", { key: "Escape" });
  assert.equal(document.activeElement, trigger, "and the trigger is still what it belongs to");

  handle.close(false);
  assert.equal((windowListeners.resize || []).length, 0, "no window listener survives a closed surface");
  trigger.parentNode.removeChild(trigger);
}

// ---------------------------------------------------------------------------
// Choosing a row, and the rows a caller rebuilt underneath an open surface
// ---------------------------------------------------------------------------
{
  const { trigger, handle, picked } = build();
  handle.open({ el: trigger });
  handle.rows()[1].fire("click");
  assert.deepEqual(picked, [1], "the row's own handler runs; closing is the caller's call");
  assert.equal(handle.isOpen(), true);

  handle.rebuild();
  assert.equal(handle.rows().length, 3, "rebuilding re-renders the rows under the open surface");
  assert.equal(document.activeElement, handle.rows()[0], "and puts the keyboard back on a real row");
  handle.close(false);
  trigger.parentNode.removeChild(trigger);
}

// ---------------------------------------------------------------------------
// Keyboard: the rows a native select would give for free
// ---------------------------------------------------------------------------
{
  const { trigger, handle } = build(null, [
    "Alpha", { label: "Bravo", disabled: true }, "Charlie", { label: "Delta", active: true },
  ]);
  handle.open({ el: trigger });
  const rows = handle.rows();
  assert.equal(document.activeElement, rows[3], "an .active row is where the keyboard starts");

  fireDocument("keydown", { key: "ArrowDown" });
  assert.equal(document.activeElement, rows[0], "moving off the end wraps");
  fireDocument("keydown", { key: "ArrowDown" });
  assert.equal(document.activeElement, rows[2], "and steps over a row that cannot be used");
  fireDocument("keydown", { key: "End" });
  assert.equal(document.activeElement, rows[3]);
  fireDocument("keydown", { key: "Home" });
  assert.equal(document.activeElement, rows[0]);
  fireDocument("keydown", { key: "c" });
  assert.equal(document.activeElement, rows[2], "type-ahead jumps by label");

  fireDocument("keydown", { key: "Escape" });
  assert.equal(handle.isOpen(), false);
  assert.equal(document.activeElement, trigger, "Escape returns the keyboard to the trigger");
  trigger.parentNode.removeChild(trigger);
}

// ---------------------------------------------------------------------------
// Dismissal: outside press, scroll, and no listener left behind
// ---------------------------------------------------------------------------
{
  const { trigger, handle } = build();
  const before = { key: listenerCount("keydown"), press: listenerCount("pointerdown") };
  handle.open({ el: trigger });
  assert.equal(listenerCount("keydown"), before.key + 1);

  fireDocument("pointerdown", { target: handle.rows()[0] });
  assert.equal(handle.isOpen(), true, "a press on a row is not an outside press");
  fireDocument("pointerdown", { target: trigger });
  assert.equal(handle.isOpen(), true, "nor is one on the trigger, which toggles on click");
  fireDocument("scroll", { target: handle.el });
  assert.equal(handle.isOpen(), true, "scrolling the list itself is not a reason to close it");

  fireDocument("scroll", { target: document.body });
  assert.equal(handle.isOpen(), false, "scrolling the pane under it is");
  assert.equal(listenerCount("keydown"), before.key, "no listener survives a closed surface");
  assert.equal(listenerCount("pointerdown"), before.press);

  handle.open({ el: trigger });
  fireDocument("pointerdown", { target: document.body });
  assert.equal(handle.isOpen(), false);
  trigger.parentNode.removeChild(trigger);
}

// ---------------------------------------------------------------------------
// One surface at a time, across every kind of menu in the app
// ---------------------------------------------------------------------------
{
  const a = build();
  const host = makeEl("div");
  host._box = { width: 800, height: 600 };
  const b = build({ host, closeOnScroll: false });
  b.handle.el._box = { top: 0, left: 0, width: 150, height: 100 };

  a.handle.open({ el: a.trigger });
  b.handle.open({ x: 100, y: 100 });
  assert.equal(a.handle.isOpen(), false,
    "the map's menu closing the flight bar's list is the point: both listen on the document");
  assert.equal(b.handle.isOpen(), true);
  b.handle.close(false);
  a.trigger.parentNode.removeChild(a.trigger);
  b.trigger.parentNode.removeChild(b.trigger);
}

// ---------------------------------------------------------------------------
// Nothing to offer is nothing to open
// ---------------------------------------------------------------------------
{
  const trigger = makeEl("button");
  body.appendChild(trigger);
  const handle = ui.menu({ render: () => [] });
  assert.equal(handle.open({ el: trigger }), null);
  assert.equal(handle.isOpen(), false, "an empty surface is a rectangle the operator has to dismiss for nothing");
  assert.equal(handle.el.parentNode, null);
  // A close with nothing open is a no-op, not a throw: every teardown path
  // calls it, several of them twice.
  assert.doesNotThrow(() => handle.close(false));
  trigger.parentNode.removeChild(trigger);
}

// ---------------------------------------------------------------------------
// onOpen / onClose fire once per visit, whichever path closed it
// ---------------------------------------------------------------------------
{
  const opens = [];
  const { trigger, handle } = build({
    onOpen: () => opens.push("open"),
    onClose: () => opens.push("close"),
  });
  handle.open({ el: trigger });
  fireDocument("keydown", { key: "Escape" });
  handle.open({ el: trigger });
  handle.close(false);
  handle.close(false);
  assert.deepEqual(opens, ["open", "close", "open", "close"],
    "the trigger's own pressed state can be driven from these, so a second close must not fire one");
  trigger.parentNode.removeChild(trigger);
}

console.log("test_frontend_menu.js: all assertions passed");
