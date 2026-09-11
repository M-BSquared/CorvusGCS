"use strict";

/**
 * Frontend tests for Corvus.ui.enhanceSelect — the app's dropdown built over a
 * native <select>.
 *
 * The point of the component is that it changes nothing for its callers: the
 * <select> stays the state, so `value`, `disabled`, the option list and the
 * "change" event must behave exactly as they did before the enhancement. Most
 * of what follows asserts that contract, plus the parts a native select gets
 * for free and this one has to implement itself — keyboard navigation,
 * one-open-list-at-a-time, and flipping the list above a trigger near the
 * bottom of the screen.
 *
 * Plain Node-runnable assertions (no browser, no test runner), following the
 * DOM-stub pattern of tests/test_frontend_mapmenu.js.
 *
 * Run:
 *   node tests/test_frontend_dropdown.js
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
global.KeyboardEvent = class KeyboardEvent extends global.Event {
  constructor(type, options = {}) { super(type, options); this.key = options.key; }
};

// MutationObserver is the only way the component can see a programmatic
// `select.value = x` or a repopulated option list. The fake records the
// callback so a test can fire it the way the browser would.
const observers = [];
global.MutationObserver = class MutationObserver {
  constructor(cb) { this.cb = cb; observers.push(this); }
  observe(target, opts) { this.target = target; this.opts = opts; }
  disconnect() {}
};
function fireMutations() { observers.forEach((o) => o.cb([], o)); }

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
      return e.children.length
        ? e.children.map((c) => c.textContent).join("")
        : e._text;
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
  e.insertBefore = (c, ref) => {
    if (c.parentNode) c.parentNode.removeChild(c);
    const i = e.children.indexOf(ref);
    e.children.splice(i < 0 ? e.children.length : i, 0, c);
    c.parentNode = e; c.parentElement = e;
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
        if (matches(c, sel)) out.push(c);
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

/* Selector support is deliberately thin — a tag, a class, or a tag carrying
   one quoted attribute are the three shapes ui.js queries with. */
function matches(el, sel) {
  const m = /^([a-zA-Z-]*)(?:\.([\w-]+))?(?:\[([\w-]+)="([^"]*)"\])?$/.exec(sel);
  if (!m || (!m[1] && !m[2] && !m[3])) return false;
  if (m[1] && el.tagName !== m[1].toUpperCase()) return false;
  if (m[2] && !el.className.split(/\s+/).includes(m[2])) return false;
  if (m[3]) {
    // `for` is reflected by the htmlFor property, which is what ui.label sets.
    const got = m[3] === "for" && el.htmlFor != null ? el.htmlFor : el.getAttribute(m[3]);
    if (String(got == null ? "" : got) !== m[4]) return false;
  }
  return true;
}

function makeSelect(pairs) {
  const sel = makeEl("select");
  Object.defineProperty(sel, "options", {
    get: () => sel.children.filter((c) => c.tagName === "OPTION"),
  });
  (pairs || []).forEach(([value, text]) => {
    const o = makeEl("option");
    o.value = value;
    o.textContent = text;
    sel.appendChild(o);
  });
  sel.value = sel.options.length ? sel.options[0].value : "";
  return sel;
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

require("../src/js/ui.js");
const ui = Corvus.ui;

/** A mounted, enhanced select with the PX4-shaped placeholder + modes. */
function mount(pairs, opts) {
  const host = makeEl("div");
  document.body.appendChild(host);
  const sel = makeSelect(pairs || [["", "SELECT MODE"], ["MANUAL", "MANUAL"], ["MISSION", "MISSION"], ["RTL", "RTL"]]);
  sel.value = (opts && opts.value) != null ? opts.value : "";
  sel.className = "field-select mode-selector";
  sel.title = "Flight Mode";
  host.appendChild(sel);
  const h = ui.enhanceSelect(sel);
  // A trigger with a real box, so the placement maths has something to read.
  h.trigger._box = { top: 100, left: 200, width: 130, height: 30 };
  h.menu._box = { top: 0, left: 0, width: 150, height: 180 };
  return { host, sel, h };
}
function rows(h) { return h.menu.querySelectorAll(".option-item"); }
function labelOf(h) { return h.trigger.textContent; }

// ---------------------------------------------------------------------------
// The select stays the state; the trigger only mirrors it
// ---------------------------------------------------------------------------
{
  const { host, sel, h } = mount();
  assert.equal(sel.className.includes("ui-select-native"), true, "the native select is taken out of the layout");
  assert.equal(sel.getAttribute("aria-hidden"), "true");
  assert.equal(sel.tabIndex, -1, "the hidden select must not be a tab stop");
  // The trigger wears the select's own classes, so every rule already written
  // for .field-select / .mode-selector styles it unchanged.
  assert.equal(h.trigger.className, "field-select mode-selector ui-select-trigger");
  assert.equal(h.trigger.getAttribute("role"), "combobox");
  assert.equal(h.trigger.getAttribute("aria-haspopup"), "listbox");
  assert.equal(h.trigger.getAttribute("aria-expanded"), "false");
  assert.equal(h.trigger.getAttribute("aria-label"), "Flight Mode", "the select's label carries over");
  assert.equal(labelOf(h), "SELECT MODE");
  assert.equal(h.trigger.dataset.placeholder, "true", "the empty value is a prompt, not a choice");
  // The wrapper takes the select's place in its parent, with the trigger in it.
  assert.equal(host.children.length, 1);
  assert.equal(host.children[0].className, "ui-select");
  assert.equal(ui.enhanceSelect(sel), h, "enhancing twice returns the same handle");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// Opening builds one row per option, marking the current one
// ---------------------------------------------------------------------------
{
  const { host, h } = mount(null, { value: "MISSION" });
  assert.equal(labelOf(h), "MISSION");
  assert.equal(h.trigger.dataset.placeholder, "false");
  assert.equal(h.isOpen(), false);
  h.open();
  assert.equal(h.isOpen(), true);
  assert.equal(h.trigger.getAttribute("aria-expanded"), "true");
  assert.equal(h.menu.parentNode, document.body, "the list is mounted on the body, not in the flight bar");
  assert.equal(h.menu.getAttribute("role"), "listbox");
  const items = rows(h);
  assert.deepEqual(items.map((b) => b.textContent), ["SELECT MODE", "MANUAL", "MISSION", "RTL"]);
  assert.deepEqual(items.map((b) => b.getAttribute("aria-selected")), ["false", "false", "true", "false"]);
  assert.equal(items[2].classList.contains("active"), true);
  assert.equal(document.activeElement, items[2], "the keyboard starts on the current mode");
  h.close();
  assert.equal(h.menu.parentNode, null, "closing takes the list back off the body");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// Picking a row is a native selection: value + one "change", and none when the
// value did not actually move
// ---------------------------------------------------------------------------
{
  const { host, sel, h } = mount();
  let fired = 0;
  let seen = null;
  sel.addEventListener("change", () => { fired++; seen = sel.value; });

  h.open();
  rows(h)[1].fire("click");
  assert.equal(sel.value, "MANUAL");
  assert.equal(fired, 1);
  assert.equal(seen, "MANUAL", "the handler reads the new value off the select");
  assert.equal(labelOf(h), "MANUAL");
  assert.equal(h.isOpen(), false, "picking closes the list");
  assert.equal(document.activeElement, h.trigger, "and hands the keyboard back to the trigger");

  h.open();
  rows(h)[1].fire("click");
  assert.equal(fired, 1, "re-picking the current value fires no change, exactly as a <select> does");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// Keyboard: open, move, type-ahead, escape
// ---------------------------------------------------------------------------
{
  const { host, h } = mount();
  h.trigger.fire("keydown", { key: "ArrowDown" });
  assert.equal(h.isOpen(), true, "ArrowDown opens the list from the trigger");
  const items = rows(h);

  fireDocument("keydown", { key: "ArrowDown" });
  assert.equal(document.activeElement, items[1]);
  fireDocument("keydown", { key: "End" });
  assert.equal(document.activeElement, items[3]);
  fireDocument("keydown", { key: "ArrowDown" });
  assert.equal(document.activeElement, items[0], "moving off the end wraps");
  fireDocument("keydown", { key: "ArrowUp" });
  assert.equal(document.activeElement, items[3]);
  fireDocument("keydown", { key: "Home" });
  assert.equal(document.activeElement, items[0]);
  // Type-ahead: PX4's mode names are distinct, so a letter jumps like it does
  // in a native select.
  fireDocument("keydown", { key: "m" });
  assert.equal(document.activeElement, items[1], "'m' jumps to MANUAL");
  fireDocument("keydown", { key: "i" });
  assert.equal(document.activeElement, items[2], "'mi' continues to MISSION");

  fireDocument("keydown", { key: "Escape" });
  assert.equal(h.isOpen(), false);
  assert.equal(document.activeElement, h.trigger, "Escape returns the keyboard to the trigger");

  h.open();
  fireDocument("keydown", { key: "Tab" });
  assert.equal(h.isOpen(), false, "Tab closes the list and lets focus leave");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// A press outside closes; a press inside does not
// ---------------------------------------------------------------------------
{
  const { host, h } = mount();
  h.open();
  fireDocument("pointerdown", { target: rows(h)[1] });
  assert.equal(h.isOpen(), true, "a press on a row is not an outside press");
  fireDocument("pointerdown", { target: h.trigger });
  assert.equal(h.isOpen(), true, "nor is a press on the trigger, which toggles on click");
  fireDocument("pointerdown", { target: document.body });
  assert.equal(h.isOpen(), false);
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// Scrolling the pane behind the list moves the trigger out from under it
// ---------------------------------------------------------------------------
{
  const { host, h } = mount();
  h.open();
  fireDocument("scroll", { target: h.menu });
  assert.equal(h.isOpen(), true, "scrolling the list itself is not a reason to close it");
  fireDocument("scroll", { target: document.body });
  assert.equal(h.isOpen(), false, "scrolling anything else is");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// One list at a time — two open lists would both be listening on the document
// ---------------------------------------------------------------------------
{
  const a = mount();
  const b = mount();
  a.h.open();
  b.h.open();
  assert.equal(a.h.isOpen(), false, "opening a second dropdown closes the first");
  assert.equal(b.h.isOpen(), true);
  b.h.close();
  a.host.parentNode.removeChild(a.host);
  b.host.parentNode.removeChild(b.host);
}

// ---------------------------------------------------------------------------
// disabled: app.js sets it on the select while PX4 acknowledges a mode change
// ---------------------------------------------------------------------------
{
  const { host, sel, h } = mount();
  sel.disabled = true;
  fireMutations();
  assert.equal(h.trigger.disabled, true, "the trigger dims with the select it stands for");
  h.open();
  assert.equal(h.isOpen(), false, "a disabled control does not open");
  sel.disabled = false;
  fireMutations();
  assert.equal(h.trigger.disabled, false);
  h.open();
  assert.equal(h.isOpen(), true);
  // An open list must not survive the control being disabled underneath it.
  sel.disabled = true;
  fireMutations();
  assert.equal(h.isOpen(), false);
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// A repopulated mode list (refreshModesFromData) reaches the trigger
// ---------------------------------------------------------------------------
{
  const { host, sel, h } = mount();
  sel.options.filter((o) => o.value !== "").forEach((o) => sel.removeChild(o));
  ["ACRO", "OFFBOARD"].forEach((m) => {
    const o = makeEl("option");
    o.value = m;
    o.textContent = m;
    sel.appendChild(o);
  });
  sel.value = "OFFBOARD";          // the restore app.js does after repopulating
  fireMutations();
  assert.equal(labelOf(h), "OFFBOARD");
  h.open();
  assert.deepEqual(rows(h).map((b) => b.textContent), ["SELECT MODE", "ACRO", "OFFBOARD"]);
  h.close();
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// Placement: under the trigger, flipped above it when the room below is gone
// ---------------------------------------------------------------------------
{
  const { host, h } = mount();
  h.open();
  assert.equal(h.menu.style.top, "136px", "6px under the trigger's bottom edge");
  assert.equal(h.menu.style.left, "200px", "left-aligned with the trigger");
  assert.equal(h.menu.style.minWidth, "130px", "never narrower than the control it belongs to");
  h.close();

  // Same control, sitting near the bottom of the window: 720 - 690 leaves no
  // room for a 180px list, so it opens upwards instead of off-screen.
  h.trigger._box = { top: 660, left: 200, width: 130, height: 30 };
  h.open();
  assert.equal(h.menu.style.top, "474px", "flipped above the trigger (660 - 6 - 180)");
  h.close();

  // And it never runs off the right edge.
  h.trigger._box = { top: 100, left: 1200, width: 130, height: 30 };
  h.open();
  assert.equal(h.menu.style.left, "1122px", "clamped to 8px inside the viewport (1280 - 150 - 8)");
  h.close();
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// enhanceSelects: every plain select in a subtree, and only those
// ---------------------------------------------------------------------------
{
  const host = makeEl("div");
  document.body.appendChild(host);

  const plain = makeSelect([["time-desc", "Newest first"], ["time-asc", "Oldest first"]]);
  plain.className = "field-select logs-sort-select";
  const listbox = makeSelect([["a", "A"], ["b", "B"]]);
  listbox.multiple = true;
  const sized = makeSelect([["a", "A"], ["b", "B"]]);
  sized.size = 4;
  const optedOut = makeSelect([["a", "A"]]);
  optedOut.setAttribute("data-native-select", "");

  const nested = makeEl("div");
  [plain, listbox, sized].forEach((el) => host.appendChild(el));
  host.appendChild(nested);
  nested.appendChild(optedOut);

  ui.enhanceSelects(host);
  assert.ok(plain.corvusSelect, "a plain select anywhere in the subtree gets the app's dropdown");
  assert.equal(plain.corvusSelect.trigger.className,
    "field-select logs-sort-select ui-select-trigger",
    "and the trigger wears its classes, so .logs-sort .field-select still sizes it");
  assert.equal(listbox.corvusSelect, undefined, "a multiple list box is not a dropdown");
  assert.equal(sized.corvusSelect, undefined, "neither is a size>1 list box");
  assert.equal(optedOut.corvusSelect, undefined, "data-native-select keeps the platform control");

  const first = plain.corvusSelect;
  ui.enhanceSelects(host);
  assert.equal(plain.corvusSelect, first, "a second pass over the same subtree changes nothing");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// The field caption follows the control it labels
// ---------------------------------------------------------------------------
{
  const field = makeEl("div");
  document.body.appendChild(field);
  const caption = makeEl("label");
  caption.htmlFor = "tilesSource";
  const sel = makeSelect([["osm", "OpenStreetMap"]]);
  sel.id = "tilesSource";
  field.appendChild(caption);
  field.appendChild(sel);

  const h = ui.enhanceSelect(sel);
  assert.equal(sel.id, "tilesSource", "the id stays on the state — modules look it up by that");
  assert.equal(h.trigger.id, "tilesSource-trigger");
  assert.equal(caption.htmlFor, "tilesSource-trigger",
    "clicking the caption must reach the control that is actually visible");
  field.parentNode.removeChild(field);
}

// The caption is not always the control's sibling: index.html's serial-port
// field puts the select one level deeper, in the row it shares with the
// refresh button. An id is unique, so the search starts at the tree's root.
{
  const linkField = makeEl("div");
  document.body.appendChild(linkField);
  const caption = makeEl("label");
  caption.htmlFor = "linkSerialPort";
  const row = makeEl("div");
  const sel = makeSelect([["", "Select serial port…"]]);
  sel.id = "linkSerialPort";
  linkField.appendChild(caption);
  linkField.appendChild(row);
  row.appendChild(sel);
  row.appendChild(makeEl("button"));

  const h = ui.enhanceSelect(sel);
  assert.equal(caption.htmlFor, "linkSerialPort-trigger",
    "a caption a level above the control still follows it");
  assert.equal(h.trigger.id, "linkSerialPort-trigger");
  linkField.parentNode.removeChild(linkField);
}

// ---------------------------------------------------------------------------
// watchSelects: the page as it loads, and everything built after it
// ---------------------------------------------------------------------------
{
  const host = makeEl("div");
  document.body.appendChild(host);
  const early = makeSelect([["115200", "115200"]]);   // an index.html select
  early.className = "field-select link-select";
  host.appendChild(early);

  ui.watchSelects();
  assert.ok(early.corvusSelect, "the boot pass reaches what the HTML already shipped");

  const watcher = observers.filter((o) => o.target === document.body).pop();
  assert.ok(watcher, "and a watcher is left on the body for what comes later");
  assert.equal(watcher.opts.subtree, true);

  // A setup screen rendering its card minutes after boot.
  const card = makeEl("div");
  const late = makeSelect([["all", "All messages"], ["error", "Errors only"]]);
  card.appendChild(late);
  host.appendChild(card);
  watcher.cb([{ addedNodes: [card], removedNodes: [] }], watcher);
  assert.ok(late.corvusSelect, "a select built long after boot is enhanced too");

  // The wrapper the enhancement itself inserts comes back through the observer.
  watcher.cb([{ addedNodes: [late.parentNode], removedNodes: [] }], watcher);
  assert.ok(late.corvusSelect, "re-entry through the observer is a no-op, not a second dropdown");

  // A select appended straight into a mounted parent arrives on its own.
  const direct = makeSelect([["a", "A"]]);
  host.appendChild(direct);
  watcher.cb([{ addedNodes: [direct], removedNodes: [] }], watcher);
  assert.ok(direct.corvusSelect, "…as does one added with no wrapper around it");

  ui.watchSelects();
  assert.equal(observers.filter((o) => o.target === document.body).length, 1,
    "calling it again does not stack a second watcher on the body");
  host.parentNode.removeChild(host);
}

// ---------------------------------------------------------------------------
// A re-render under an open list takes the list with it
// ---------------------------------------------------------------------------
{
  const { host, h } = mount();
  h.open();
  assert.equal(h.isOpen(), true);
  assert.equal(h.menu.parentNode, document.body);

  // The screen rebuilds. The menu is on <body>, not inside what was removed,
  // so nothing but this would take it down.
  host.parentNode.removeChild(host);
  const watcher = observers.filter((o) => o.target === document.body).pop();
  watcher.cb([{ addedNodes: [], removedNodes: [host] }], watcher);
  assert.equal(h.isOpen(), false, "the list goes with the trigger it belonged to");
  assert.equal(h.menu.parentNode, null, "and is taken off the body");
}

console.log("test_frontend_dropdown.js: all assertions passed");
