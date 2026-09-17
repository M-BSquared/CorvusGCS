"use strict";

/**
 * The top bar builds its blocks with telemetry in them, so it may not build
 * them out of a markup string.
 *
 * The Vehicle block's sub-line is `state.px4_version` — a string the AUTOPILOT
 * sent. It used to be interpolated into `blk.innerHTML`, which means the value
 * the aircraft reported was parsed as HTML by the ground station. It is built
 * from three integers today, so nothing could actually be injected through it;
 * that is a property of the current parser, not of the bar, and it is one
 * refactor away from not being true.
 *
 * This file drives the BUILD path specifically. topbar.js builds the bar once
 * per page (topBarBuilt) and updates it by textContent afterwards, so the
 * first snapshot is the only one that ever reaches the markup — which is also
 * the snapshot a page loaded against an already-connected vehicle gets.
 *
 * Run:
 *   node tests/test_frontend_topbar_escaping.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, o = {}) { this.type = type; this.detail = o.detail; }
};
window.addEventListener = () => {};
window.removeEventListener = () => {};
window.setTimeout = (fn) => { fn(); return 0; };
window.clearTimeout = () => {};
global.localStorage = { getItem: () => null, setItem: () => {}, removeItem: () => {} };

// ---------------------------------------------------------------------------
// DOM stub. Deliberately a PARSING one: innerHTML turns markup into child
// elements, so a value that reaches it as markup shows up here as structure
// rather than as text — which is exactly the failure being guarded against.
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {}, style: {},
    hidden: false, id: "", title: "", _attrs: {}, _listeners: {}, _isEl: true,
    parentElement: null, parentNode: null,
  };
  let _html = "";
  Object.defineProperty(e, "innerHTML", {
    get() { return _html; },
    set(v) { _html = String(v); e.children.length = 0; parseInto(e, _html); },
  });
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) { const has = e.classList.contains(c); const next = force === undefined ? !has : !!force; if (next) e.classList.add(c); else e.classList.remove(c); return next; },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => {
    if (c.parentNode && c.parentNode !== e) c.parentNode.removeChild(c);
    c.parentNode = e; c.parentElement = e;
    e.children.push(c);
    return c;
  };
  e.append = (...cs) => cs.forEach((c) => e.appendChild(c));
  e.replaceChildren = (...cs) => { e.children.length = 0; cs.forEach((c) => e.appendChild(c)); };
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; c.parentElement = null; return c; };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.removeAttribute = (k) => { delete e._attrs[k]; };
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.removeEventListener = () => {};
  e.focus = () => {};
  e.querySelector = (sel) => querySel([e], sel).filter((n) => n !== e)[0] || null;
  e.querySelectorAll = (sel) => querySel([e], sel).filter((n) => n !== e);
  e.closest = () => null;
  return e;
}

function parseInto(root, html) {
  const stack = [root];
  const token = /<(\/?)([a-zA-Z][\w-]*)((?:\s+[\w-]+="[^"]*")*)\s*(\/?)>|([^<]+)/g;
  let m;
  while ((m = token.exec(html)) !== null) {
    const [, closing, tag, attrs, selfClose, text] = m;
    if (text !== undefined) {
      const parent = stack[stack.length - 1];
      if (text.trim()) parent.textContent = (parent.textContent || "") + text;
      continue;
    }
    if (closing) { if (stack.length > 1) stack.pop(); continue; }
    const el = makeEl(tag);
    (attrs.match(/[\w-]+="[^"]*"/g) || []).forEach((pair) => {
      const eq = pair.indexOf("=");
      el.setAttribute(pair.slice(0, eq), pair.slice(eq + 2, -1));
    });
    stack[stack.length - 1].appendChild(el);
    // <img> never has a closing tag; pushing it would swallow its siblings.
    if (!selfClose && el.tagName !== "IMG") stack.push(el);
  }
}

function querySel(roots, sel) {
  const parts = sel.trim().split(/\s+/);
  let current = roots;
  parts.forEach((part) => {
    const attr = part.match(/^\[data-block="([^"]+)"\]$/);
    const next = [];
    const walk = (node) => {
      const ok = attr
        ? node.dataset && node.dataset.block === attr[1]
        : part.split(".").filter(Boolean).every((c) => node.className.split(/\s+/).includes(c));
      if (ok) next.push(node);
      (node.children || []).forEach((child) => { if (child._isEl) walk(child); });
    };
    current.forEach(walk);
    current = next;
  });
  return current;
}

const byId = {};
global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: (id) => byId[id] || null,
  querySelector: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
  activeElement: null,
};

let stateSubscriber = null;
let state = {};
global.Corvus.telemetry = {
  subscribe: (fn) => { stateSubscriber = fn; },
  getState: () => state,
  postAction: () => Promise.resolve({ ok: true }),
};

require("./../src/js/ui.js");
require("./../src/js/notification_dedupe.js");
require("./../src/js/topbar.js");

["topBar", "warningsPopover", "warningsList", "notificationLive",
 "wpClose", "wpClearAll", "warningsTitle"].forEach((id) => {
  byId[id] = makeEl("div");
  byId[id].id = id;
});
byId.warningsPopover.hidden = true;

// A firmware string that is markup. Nothing sends this today; the point is
// that the bar's answer must not depend on nothing sending it.
const HOSTILE = '<img src="x" onerror="alert(1)">v1.16.0';

Corvus.topbar.init();
stateSubscriber({
  connected: true, armed: false, boot_ms: 1000, warnings: [],
  vehicle_type: "QUAD", autopilot: "PX4", mode: "HOLD",
  battery_percent: 80, battery_voltage: 16.2,
  gps_fix: "3D_FIX", gps_hdop: 0.9,
  altitude_amsl: 500, groundspeed: 0, vspeed: 0,
  px4_version: HOSTILE, px4_version_detail: "abc123",
});

const tests = [];

function test(name, fn) { tests.push([name, fn]); }

const vehicleBlock = () => byId.topBar.querySelector('[data-block="vehicle"]');

test("the firmware line is text, not markup", () => {
  const sub = vehicleBlock().querySelector(".sub");
  assert.ok(sub, "the Vehicle block still has its sub-line");
  assert.equal(sub.textContent, HOSTILE, "kept verbatim, as a string");
  assert.equal(
    sub.children.filter((c) => c._isEl).length, 0,
    "the firmware string was parsed into elements — it reached innerHTML",
  );
});

test("the Vehicle block holds no element the firmware string put there", () => {
  // Scoped to the block that carries the value. The bar has one legitimate
  // <img> of its own — the operator's company logo (renderCompanyLogo) — and
  // a blanket "no <img> anywhere" would be asserting that away instead.
  const walk = (node) => (node.children || []).reduce(
    (found, child) => found.concat(child._isEl ? [child].concat(walk(child)) : []),
    [],
  );
  const inside = walk(vehicleBlock());
  assert.equal(
    inside.filter((el) => el.tagName === "IMG").length, 0,
    "an <img> was built out of telemetry",
  );
  // Only the spans the block is made of: label, value, v-main, sub.
  assert.deepEqual(
    [...new Set(inside.map((el) => el.tagName))], ["SPAN"],
    "the Vehicle block grew a tag it does not build itself",
  );
});

test("the blocks the bar is actually for still render", () => {
  assert.equal(
    byId.topBar.querySelector('[data-block="battery"]').querySelector(".v-main").textContent,
    "16.2 V",
  );
  assert.equal(
    byId.topBar.querySelector('[data-block="altitude"]').querySelector(".sub").textContent,
    "m AMSL",
  );
  assert.ok(
    byId.topBar.querySelector('[data-block="gps"]').querySelector(".tb-dot"),
    "the status dot rides with the label",
  );
});

let failed = 0;
for (const [name, fn] of tests) {
  try { fn(); console.log("ok   - " + name); }
  catch (e) { failed++; console.error("FAIL - " + name + "\n      " + (e && e.message)); }
}
if (failed) { console.error(`\n${failed}/${tests.length} topbar escaping test(s) FAILED`); process.exit(1); }
console.log(`\nAll ${tests.length} topbar escaping tests passed.`);
