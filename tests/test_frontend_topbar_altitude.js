"use strict";

/**
 * The top bar's ALTITUDE block reads AMSL by default and can be switched to
 * the altitude relative to home (Settings, Top bar). The switch must redraw
 * the live bar at once, survive a reload through localStorage, and treat any
 * value it does not know as AMSL.
 *
 * Run:
 *   node tests/test_frontend_topbar_altitude.js
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
const stored = {};
global.localStorage = {
  getItem: (k) => (k in stored ? stored[k] : null),
  setItem: (k, v) => { stored[k] = String(v); },
  removeItem: (k) => { delete stored[k]; },
};

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
require("./../src/js/units.js");
require("./../src/js/notification_dedupe.js");
require("./../src/js/topbar.js");

["topBar", "warningsPopover", "warningsList", "notificationLive",
 "wpClose", "wpClearAll", "warningsTitle"].forEach((id) => {
  byId[id] = makeEl("div");
  byId[id].id = id;
});
byId.warningsPopover.hidden = true;

Corvus.topbar.init();
stateSubscriber({
  connected: true, armed: false, boot_ms: 1000, warnings: [],
  vehicle_type: "QUAD", autopilot: "PX4", mode: "HOLD",
  battery_percent: 80, battery_voltage: 16.2,
  gps_fix: "3D_FIX", gps_hdop: 0.9,
  altitude_amsl: 512, altitude_agl: 37, groundspeed: 0, vspeed: 0,
  px4_version: "v1.16.0",
});

const altBlock = () => byId.topBar.querySelector('[data-block="altitude"]');
const altValue = () => altBlock().querySelector(".v-main").textContent;
const altSub = () => altBlock().querySelector(".sub").textContent;

const tests = [];
function test(name, fn) { tests.push([name, fn]); }

test("AMSL is the default", () => {
  assert.equal(Corvus.topbar.altitudeRef(), "amsl");
  assert.equal(altValue(), "512");
  assert.equal(altSub(), "m AMSL");
});

test("relative shows the altitude above home and says so", () => {
  assert.equal(Corvus.topbar.setAltitudeRef("relative"), "relative");
  assert.equal(altValue(), "37", "the live bar redraws without a new frame");
  assert.equal(altSub(), "m REL");
  assert.equal(altBlock().title, "Altitude relative to home");
  assert.equal(stored["corvus.topbarAltitude"], "relative");
});

test("the next frame keeps the chosen reference", () => {
  stateSubscriber(Object.assign({}, state, {
    connected: true, battery_voltage: 16.2, battery_percent: 80, warnings: [],
    altitude_amsl: 520, altitude_agl: 45,
  }));
  assert.equal(altValue(), "45");
});

test("an unknown reference falls back to AMSL", () => {
  for (const bad of [undefined, null, "agl", "RELATIVE", 1]) {
    Corvus.topbar.setAltitudeRef("relative");
    assert.equal(Corvus.topbar.setAltitudeRef(bad), "amsl", String(bad));
  }
  assert.equal(altSub(), "m AMSL");
  assert.equal(altBlock().title, "Altitude above mean sea level");
});

test("the caption follows the length unit", () => {
  Corvus.units.set({ length: "ft" });
  Corvus.topbar.setAltitudeRef("relative");
  assert.equal(altSub(), "ft REL");
  Corvus.units.set({ length: "m" });
  Corvus.topbar.setAltitudeRef("amsl");
});

let failed = 0;
for (const [name, fn] of tests) {
  try { fn(); console.log("ok   - " + name); }
  catch (e) { failed++; console.error("FAIL - " + name + "\n      " + (e && e.message)); }
}
if (failed) { console.error(`\n${failed}/${tests.length} topbar altitude test(s) FAILED`); process.exit(1); }
console.log(`\nAll ${tests.length} topbar altitude tests passed.`);
