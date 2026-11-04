"use strict";

/**
 * Frontend tests for the preflight checklist (Corvus.checklist) and the
 * helpers of its editor dialog (Corvus.checklistEditor).
 *
 *  - off by default, and the standard list stands in until the operator has
 *    lists of their own; an empty list of lists stays empty;
 *  - ticks are keyed by text and occurrence, so reordering keeps them and
 *    rewording clears only that item, and they persist in localStorage;
 *  - ticks of deleted lists and items are pruned;
 *  - the window shows only while the feature AND its Home window are on;
 *  - arming with items open warns once, on the transition only;
 *  - the editor saves a draft in place or appends it, dropping empty items.
 *
 * Run:
 *   node tests/test_frontend_checklist.js
 */

const assert = require("node:assert/strict");
const path = require("node:path");

global.window = global;
global.Corvus = {};
const store = new Map();
global.localStorage = {
  getItem: (k) => (store.has(k) ? store.get(k) : null),
  setItem: (k, v) => store.set(k, String(v)),
  removeItem: (k) => store.delete(k),
};
const notifications = [];
global.CustomEvent = class { constructor(type, o) { this.type = type; this.detail = (o || {}).detail; } };
const windowListeners = {};
window.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
window.dispatchEvent = (e) => { if (e.type === "corvus:notification") notifications.push(e.detail); };

function makeEl(tag) {
  const el = {
    tagName: tag, children: [], parentElement: null, hidden: false, style: {}, dataset: {},
    attrs: {}, textContent: "", className: "", listeners: {},
    classList: {
      set: new Set(),
      add(c) { this.set.add(c); }, remove(c) { this.set.delete(c); },
      toggle(c, on) { if (on) this.set.add(c); else this.set.delete(c); },
      contains(c) { return this.set.has(c); },
    },
    get firstChild() { return this.children[0] || null; },
    appendChild(c) { this.children.push(c); c.parentElement = this; return c; },
    append(...cs) { cs.forEach((c) => this.appendChild(c)); },
    removeChild(c) { this.children = this.children.filter((x) => x !== c); return c; },
    setAttribute(k, v) { this.attrs[k] = String(v); },
    addEventListener(t, cb) { (this.listeners[t] = this.listeners[t] || []).push(cb); },
    contains() { return false; },
    querySelectorAll() { return []; },
    getBoundingClientRect() { return { left: 0, top: 0, width: 0, height: 0 }; },
    clientWidth: 1000, clientHeight: 700, offsetWidth: 272, offsetHeight: 300,
  };
  return el;
}
global.document = { createElement: makeEl, createTextNode: (t) => ({ textContent: t }), activeElement: null, getElementById: () => null };

/** Every descendant, depth first. */
function walk(el, out = []) {
  (el.children || []).forEach((c) => { out.push(c); walk(c, out); });
  return out;
}

const posts = [];
let telemetrySub = null;
Corvus.telemetry = {
  subscribe(fn) { telemetrySub = fn; return () => {}; },
  requestJson(url, opts) {
    const body = JSON.parse(opts.body);
    posts.push(body);
    return Promise.resolve({ config: { checklists: Object.assign({}, lastBlock, body.checklists) } });
  },
};
let lastBlock = {};
Corvus.ui = {
  icon: () => makeEl("i"),
  iconButton: (_n, o) => { const b = makeEl("button"); b.className = (o && o.className) || "icon-btn"; if (o && o.onClick) b.addEventListener("click", o.onClick); return b; },
  select: () => makeEl("select"),
  clear: (el) => { el.children = []; return el; },
  refreshIcons: () => {},
};

require(path.join(__dirname, "..", "src", "js", "checklist.js"));
require(path.join(__dirname, "..", "src", "js", "checklist-editor.js"));
const CL = Corvus.checklist;
const ED = Corvus.checklistEditor;

// ---- defaults ------------------------------------------------------------
assert.equal(CL.isEnabled(), false, "off until the config says otherwise");
assert.equal(CL.isHomeWindow(), true, "the window is on once the feature is");
assert.equal(CL.lists().length, 1);
assert.equal(CL.lists()[0].id, "standard", "the standard list stands in");
assert.equal(CL.hasOwnLists(), false);
CL.fromConfig({ checklists: { lists: [] } });
assert.deepEqual(CL.lists(), [], "an empty list of lists is kept empty");
assert.equal(CL.activeList(), null);

// ---- pure helpers ---------------------------------------------------------
const items = [
  { text: "Before", heading: true },
  { text: "Props", heading: false },
  { text: "Props", heading: false },
  { text: "GPS", heading: false },
];
const keys = CL.itemKeys(items);
assert.equal(keys[0], null, "a heading is not tickable");
assert.notEqual(keys[1], keys[2], "two items that read the same stay two");
assert.deepEqual(CL.progress({ items }, [keys[1], "gone"]), { done: 1, total: 3 });
assert.deepEqual(CL.normalize({ enabled: "yes", home_window: false }),
  { enabled: false, homeWindow: false, active: "", lists: null });
const id = CL.newId([{ id: "a" }]);
assert.match(id, /^cl[a-z0-9]+$/);

// ---- ticks ----------------------------------------------------------------
const listA = { id: "a", name: "A", items };
lastBlock = { enabled: true, lists: [listA] };
CL.fromConfig({ checklists: lastBlock });
const k = CL.itemKeys(CL.lists()[0].items);
CL.toggleItem("a", k[3]);
assert.deepEqual(CL.tickedOf("a"), [k[3]]);
assert.ok(store.get("corvus.checklist.ticks").includes("GPS"), "ticks persist in localStorage");

// Reorder: GPS moves to the top, and stays ticked.
lastBlock = { enabled: true, lists: [{ id: "a", name: "A", items: [items[3], items[0], items[1], items[2]] }] };
CL.fromConfig({ checklists: lastBlock });
assert.deepEqual(CL.progress(CL.lists()[0], CL.tickedOf("a")), { done: 1, total: 3 });
// Reword it: that tick goes, and is pruned from storage.
lastBlock = { enabled: true, lists: [{ id: "a", name: "A", items: [{ text: "GNSS", heading: false }] }] };
CL.fromConfig({ checklists: lastBlock });
assert.deepEqual(CL.tickedOf("a"), []);
CL.resetList("a");

// ---- the window -----------------------------------------------------------
const panel = makeEl("div");
const host = makeEl("div");
host.appendChild(panel);
CL.init(panel);
assert.equal(panel.hidden, false, "enabled with the Home window on: shown");
CL.setEnabled(false);
assert.equal(panel.hidden, true, "the feature off hides the window");
CL.setEnabled(true);
const itemRows = walk(panel).filter((e) => e.className && e.className.split(" ").includes("checklist-item"));
assert.equal(itemRows.length, 1);
itemRows[0].listeners.click[0]();
assert.equal(CL.tickedOf("a").length, 1, "a click on a row ticks it");
CL.resetList("a");

// The × puts the window away and persists that, without turning the feature off.
const closeBtn = walk(panel).filter((e) => e.tagName === "button" && e.className.includes("checklist-bar-btn")).pop();
closeBtn.listeners.click[0]();
assert.equal(panel.hidden, true);
assert.deepEqual(posts[posts.length - 1], { checklists: { home_window: false } });
assert.equal(CL.isEnabled(), true);

// ---- arming with items open ----------------------------------------------
telemetrySub({ connected: true, armed: false });
telemetrySub({ connected: true, armed: true });
assert.equal(notifications.length, 1, "arming with the list open warns");
assert.equal(notifications[0].level, "warning");
assert.ok(!/[–—]/.test(notifications[0].message), "no dashes in operator text");
telemetrySub({ connected: true, armed: true });
assert.equal(notifications.length, 1, "once per arming, not per frame");
telemetrySub({ connected: false, armed: false });
telemetrySub({ connected: true, armed: true });
assert.equal(notifications.length, 1, "a station that connects to an armed aircraft says nothing");

// ---- editor helpers ---------------------------------------------------------
const before = [{ id: "a", name: "A", items: [] }, { id: "b", name: "B", items: [] }];
const saved = ED.withDraft(before, { id: "b", name: " B2 ", items: [{ text: " x " }, { text: "  " }] });
assert.deepEqual(saved.map((l) => l.name), ["A", "B2"], "saved in place");
assert.deepEqual(saved[1].items, [{ text: "x", heading: false }], "empty items dropped");
assert.equal(ED.withDraft(before, { id: "c", name: "C", items: [] }).length, 3, "a new list is appended");
assert.ok(ED.draftProblem({ name: "", items: [{ text: "x" }] }));
assert.ok(ED.draftProblem({ name: "N", items: [{ text: "S", heading: true }] }), "a section alone is not a list");
assert.equal(ED.draftProblem({ name: "N", items: [{ text: "x" }] }), "");

setTimeout(() => console.log("test_frontend_checklist: all assertions passed"), 0);
