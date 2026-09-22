"use strict";

/**
 * Frontend tests for the interface scale (Corvus.scale), the stepped slider
 * that drives it (Corvus.ui.slider), and the default color theme.
 *
 * Plain Node-runnable assertions (no browser, no test runner) following the
 * same pattern as tests/test_frontend_setup.js: stub the globals the modules
 * touch, require the source, assert on the rendered DOM and the callbacks.
 *
 * Run:
 *   node tests/test_frontend_scale.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals so ui.js and sidenav.js load in Node.
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

const dispatched = [];
window.dispatchEvent = (event) => { dispatched.push(event); return true; };
window.addEventListener = () => {};
window.removeEventListener = () => {};

global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
global.Event = class Event {
  constructor(type) { this.type = type; }
};

// localStorage stub with a switchable failure mode: the modules must survive a
// browser that refuses storage (private mode) without leaving the app unstyled.
let storage = {};
let storageThrows = false;
global.localStorage = {
  getItem(k) { if (storageThrows) throw new Error("denied"); return k in storage ? storage[k] : null; },
  setItem(k, v) { if (storageThrows) throw new Error("denied"); storage[k] = String(v); },
  removeItem(k) { delete storage[k]; },
};

// ---------------------------------------------------------------------------
// Minimal DOM stub. Both modules build structure with createElement +
// appendChild/append, so a children-walking stub suffices.
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {},
    type: "", hidden: false, disabled: false, value: "", id: "",
    tabIndex: 0, min: "", max: "", step: "",
    _attrs: {}, _listeners: {}, _props: {}, _isEl: true,
  };
  e.style = {
    setProperty(k, v) { e._props[k] = String(v); },
    getPropertyValue(k) { return k in e._props ? e._props[k] : ""; },
  };
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) {
      const has = e.classList.contains(c);
      const next = force === undefined ? !has : !!force;
      if (next) e.classList.add(c); else e.classList.remove(c);
      return next;
    },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => { c.parentNode = e; e.children.push(c); return c; };
  e.append = (...nodes) => { nodes.forEach((n) => e.appendChild(n)); };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (type, cb) => { (e._listeners[type] = e._listeners[type] || []).push(cb); };
  e.removeEventListener = () => {};
  e.fire = (type) => (e._listeners[type] || []).forEach((cb) => cb({ target: e }));
  e.querySelectorAll = (sel) => {
    const classes = sel.split(".").filter(Boolean);
    const out = [];
    (function walk(list) {
      list.forEach((c) => {
        if (!c || !c._isEl) return;
        if (classes.every((cl) => c.className.split(/\s+/).includes(cl))) out.push(c);
        walk(c.children);
      });
    })(e.children);
    return out;
  };
  e.querySelector = (sel) => e.querySelectorAll(sel)[0] || null;
  return e;
}

const docEl = makeEl("html");
global.document = {
  documentElement: docEl,
  createElement: makeEl,
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
};

// Corvus.scale.get() reads the token back off the document.
global.getComputedStyle = (el) => ({
  getPropertyValue: (k) => (el && el.style ? el.style.getPropertyValue(k) : ""),
  colorScheme: "",
});

require("../src/js/ui.js");
require("../src/js/sidenav.js");

const scale = Corvus.scale;
const theme = Corvus.theme;

function reset() {
  storage = {};
  storageThrows = false;
  dispatched.length = 0;
  docEl._props = {};
  docEl.style.setProperty = (k, v) => { docEl._props[k] = String(v); };
  docEl.style.getPropertyValue = (k) => (k in docEl._props ? docEl._props[k] : "");
}

// ---------------------------------------------------------------------------
// Corvus.theme — the default is the white/orange theme, not a dark one
// ---------------------------------------------------------------------------
reset();
assert.equal(theme.DEFAULT, "light-orange", "the light/orange theme is the default");
assert.ok(theme.isKnown("light-orange"), "the default must be a known theme id");
assert.equal(theme.THEMES[0].id, "light-orange", "the default is offered first in the picker");
assert.deepEqual(theme.IDS.slice().sort(),
  ["blue", "green", "light", "light-orange", "orange", "pink"],
  "every theme block in css/themes.css is offered, and no id that has none");

// An unknown id degrades to the default rather than a half-applied palette.
reset();
assert.equal(theme.setTheme("no-such-theme"), "light-orange");
assert.equal(docEl.getAttribute("data-theme"), "light-orange");

// With nothing stored, a fresh install lands on the default.
reset();
assert.equal(theme.applySaved(), "light-orange");

// A theme the operator already picked outranks the new default.
reset();
storage["corvus.theme"] = "green";
assert.equal(theme.applySaved(), "green", "an existing choice survives the default change");

// ---------------------------------------------------------------------------
// Corvus.scale — normalize
// ---------------------------------------------------------------------------
reset();
assert.equal(scale.DEFAULT, 1);
assert.equal(scale.normalize(1.25), 1.25);
assert.equal(scale.normalize("1.1"), 1.1, "a stored string is a number");
assert.equal(scale.normalize(99), scale.MAX, "an absurd value is clamped, not rejected");
assert.equal(scale.normalize(0.01), scale.MIN);
assert.equal(scale.normalize(0), scale.DEFAULT, "zero would paint nothing");
assert.equal(scale.normalize(-2), scale.DEFAULT);
assert.equal(scale.normalize("nonsense"), scale.DEFAULT);
assert.equal(scale.normalize(null), scale.DEFAULT);
assert.equal(scale.normalize(undefined), scale.DEFAULT);
assert.equal(scale.normalize(NaN), scale.DEFAULT);
assert.equal(scale.normalize(Infinity), scale.DEFAULT,
  "Infinity is garbage input, not a very large size");

// Every offered step survives normalize untouched — the slider must be able to
// reach all of them.
scale.STEPS.forEach((s) => {
  assert.equal(scale.normalize(s.value), s.value, `step ${s.label} is reachable`);
});
assert.equal(scale.STEPS[0].value, scale.MIN);
assert.equal(scale.STEPS[scale.STEPS.length - 1].value, scale.MAX);
assert.ok(scale.STEPS.some((s) => s.value === 1), "100% is one of the steps");

// ---------------------------------------------------------------------------
// Corvus.scale — setScale writes the token, caches, and announces
// ---------------------------------------------------------------------------
reset();
assert.equal(scale.setScale(1.25), 1.25);
assert.equal(docEl.style.getPropertyValue("--ui-scale"), "1.25",
  "the whole feature is this one token write");
assert.equal(storage["corvus.scale"], "1.25");
assert.ok(dispatched.some((e) => e.type === "corvus:scalechange" && e.detail.scale === 1.25),
  "the map and the HUD panel are told");
assert.ok(dispatched.some((e) => e.type === "resize"),
  "anything already reacting to a viewport change needs no new listener");

// A clamped value is what gets stored, so the next launch reads a sane one.
reset();
assert.equal(scale.setScale(50), scale.MAX);
assert.equal(storage["corvus.scale"], String(scale.MAX));

// Storage refusing to answer must not stop the scale being applied.
reset();
storageThrows = true;
assert.equal(scale.setScale(1.1), 1.1);
assert.equal(docEl.style.getPropertyValue("--ui-scale"), "1.1");

// ---------------------------------------------------------------------------
// Corvus.scale — applySaved / get / fromConfig
// ---------------------------------------------------------------------------
reset();
assert.equal(scale.applySaved(), 1, "nothing stored -> 100%");

reset();
storage["corvus.scale"] = "0.9";
assert.equal(scale.applySaved(), 0.9);
assert.equal(scale.get(), 0.9, "get() reads back what was applied");

reset();
storage["corvus.scale"] = "garbage";
assert.equal(scale.applySaved(), 1, "a corrupted cache falls back to 100%");

reset();
assert.equal(scale.fromConfig(null), null, "no config names nothing");
assert.equal(scale.fromConfig({}), null);
assert.equal(scale.fromConfig({ ui: {} }), null, "leave the cached value alone");
assert.equal(scale.fromConfig({ ui: { scale: 1.1 } }), 1.1);
assert.equal(scale.fromConfig({ ui: { scale: "1.25" } }), 1.25);
assert.equal(scale.fromConfig({ ui: { scale: 40 } }), scale.MAX, "clamped, never rejected");
assert.equal(scale.fromConfig({ ui: { scale: "nope" } }), null);
assert.equal(scale.fromConfig({ ui: { scale: 0 } }), null);

// ---------------------------------------------------------------------------
// Corvus.ui.slider — structure
// ---------------------------------------------------------------------------
function buildSlider(overrides) {
  const calls = { input: [], change: [] };
  const handle = Corvus.ui.slider(Object.assign({
    ariaLabel: "Interface size",
    value: 1,
    steps: Corvus.scale.STEPS,
    onInput: (v) => calls.input.push(v),
    onChange: (v) => calls.change.push(v),
  }, overrides || {}));
  const range = handle.el.querySelector(".ui-slider-input");
  return { handle, calls, range,
    ticks: handle.el.querySelectorAll(".ui-slider-tick"),
    labels: handle.el.querySelectorAll(".ui-slider-step") };
}

reset();
let s = buildSlider();
assert.equal(s.ticks.length, Corvus.scale.STEPS.length, "one step indicator per step");
assert.equal(s.labels.length, Corvus.scale.STEPS.length, "one label per step indicator");
assert.deepEqual(s.labels.map((b) => b.textContent), ["80%", "90%", "100%", "110%", "125%", "150%"]);
assert.equal(s.range.min, "0");
assert.equal(s.range.max, String(Corvus.scale.STEPS.length - 1));
assert.equal(s.range.step, "1", "the control is an index, never a free number");
assert.equal(s.range.getAttribute("aria-label"), "Interface size");

// Ticks and labels are placed with the same 0..1 position, so a dot sits under
// its own label and under the thumb at that step.
assert.equal(s.ticks[0].style.getPropertyValue("--tick-pos"), "0");
assert.equal(s.ticks[s.ticks.length - 1].style.getPropertyValue("--tick-pos"), "1");
s.ticks.forEach((d, i) => {
  assert.equal(d.style.getPropertyValue("--tick-pos"),
    s.labels[i].style.getPropertyValue("--tick-pos"));
});

// The current step is marked, and only it.
assert.equal(s.range.value, "2");
assert.equal(s.handle.el.style.getPropertyValue("--slider-pos"), "0.4");
assert.deepEqual(s.labels.map((b) => b.classList.contains("is-current")),
  [false, false, true, false, false, false]);
assert.equal(s.range.getAttribute("aria-valuetext"), "100%");
// The ticks the fill has reached are re-coloured so they stay visible on it.
assert.deepEqual(s.ticks.map((d) => d.classList.contains("is-passed")),
  [true, true, true, false, false, false]);

// ---------------------------------------------------------------------------
// Corvus.ui.slider — callbacks: preview while dragging, persist on release
// ---------------------------------------------------------------------------
reset();
s = buildSlider();
s.range.value = "4";
s.range.fire("input");
assert.deepEqual(s.calls.input, [1.25], "every crossed step previews");
assert.deepEqual(s.calls.change, [], "a drag in progress persists nothing");
assert.equal(s.handle.getValue(), 1.25);

s.range.fire("change");
assert.deepEqual(s.calls.change, [1.25], "releasing persists once");

// An input event that lands on the step already shown is not a change.
s.calls.input.length = 0;
s.range.value = "4";
s.range.fire("input");
assert.deepEqual(s.calls.input, [], "no callback for a move that changed nothing");

// A label click is a complete gesture: preview and persist, in that order.
reset();
s = buildSlider();
s.labels[0].fire("click");
assert.deepEqual(s.calls.input, [0.8]);
assert.deepEqual(s.calls.change, [0.8]);
assert.equal(s.range.value, "0");
assert.equal(s.labels[0].getAttribute("aria-current"), "true");
assert.equal(s.labels[2].getAttribute("aria-current"), "false");

// Clicking the step already selected does nothing at all.
s.calls.input.length = 0;
s.calls.change.length = 0;
s.labels[0].fire("click");
assert.deepEqual(s.calls.input, []);
assert.deepEqual(s.calls.change, []);

// ---------------------------------------------------------------------------
// Corvus.ui.slider — value resolution
// ---------------------------------------------------------------------------
// A persisted value between steps (an older build's step list, a hand-edited
// config) shows the nearest step rather than falling off the control.
reset();
s = buildSlider({ value: 1.2 });
assert.equal(s.range.value, "4", "1.2 is nearest 125%");
assert.equal(s.handle.getValue(), 1.25);

reset();
s = buildSlider({ value: 0.87 });
assert.equal(s.handle.getValue(), 0.9);

// setValue repaints without firing callbacks — it reflects state, it is not a
// second way for the operator to change it.
reset();
s = buildSlider();
s.handle.setValue(1.5);
assert.equal(s.handle.getValue(), 1.5);
assert.equal(s.range.value, "5");
assert.equal(s.handle.el.style.getPropertyValue("--slider-pos"), "1");
assert.deepEqual(s.calls.input, []);
assert.deepEqual(s.calls.change, []);

// A slider with no steps at all must not throw (an empty registry, a bad call).
reset();
assert.doesNotThrow(() => Corvus.ui.slider({ steps: [] }));

// ---------------------------------------------------------------------------
// Corvus.scale — the responsive breakpoints
// ---------------------------------------------------------------------------
// The width media queries in css/main.css are selectors guarded by this
// attribute, because `zoom` is invisible to a media query: at 150% on a
// 1440px window the app has 960px and @media still reads 1440, so the narrow
// layout never arrived and the flight bar ran out over the map's chrome.
function tokensAt(windowWidth, scaleValue) {
  docEl.clientWidth = windowWidth;
  scale.setScale(scaleValue);
  return (docEl.getAttribute("data-vw") || "").split(" ").filter(Boolean);
}

reset();
assert.deepEqual(tokensAt(1440, 1), ["min-760"],
  "a full-size window at 100% is the widest layout");
assert.deepEqual(tokensAt(1440, 1.5), ["max-1279", "min-760"],
  "the SAME window at 150% has 960px to lay out in, and reflows to it");
assert.deepEqual(tokensAt(960, 1), ["max-1279", "min-760"],
  "which is exactly a 960px window at 100% — the two must agree");

// Every step the slider offers turns the breakpoints over at the width it
// actually has, not at the window's.
reset();
docEl.clientWidth = 1280;
scale.STEPS.forEach((step) => {
  const effective = 1280 / step.value;
  const tokens = tokensAt(1280, step.value);
  assert.equal(tokens.includes("max-1279"), effective <= 1279,
    `at ${step.label} a 1280px window lays out in ${Math.round(effective)}px`);
  assert.equal(tokens.includes("min-760"), effective >= 760, `min-760 at ${step.label}`);
});

// The narrow end still resolves: nothing about the ladder is scale-specific.
reset();
assert.deepEqual(tokensAt(720, 1), ["max-1279", "max-959", "max-900", "max-720"]);
assert.deepEqual(tokensAt(1080, 1.5), ["max-1279", "max-959", "max-900", "max-720"],
  "1080 at 150% is 720 at 100%");

// A document that reports no width must not throw or leave the attribute
// stale — the app has to paint something.
reset();
docEl.clientWidth = 0;
assert.doesNotThrow(() => scale.setScale(1));
assert.equal(typeof docEl.getAttribute("data-vw"), "string");

// ---------------------------------------------------------------------------
// The stylesheets: nothing may reintroduce a window-measuring rule
// ---------------------------------------------------------------------------
// Both of these are silent failures at runtime — the app simply keeps the
// wrong layout at any scale but 100% — so they are asserted against the
// source instead.
const fs = require("node:fs");
const path = require("node:path");
const cssDir = path.join(__dirname, "..", "src", "css");
const sheets = ["themes.css", "components.css", "main.css"];
const css = Object.fromEntries(
  sheets.map((f) => [f, fs.readFileSync(path.join(cssDir, f), "utf8")])
);
const withoutComments = (text) => text.replace(/\/\*[\s\S]*?\*\//g, "");

// 1. No width media query. It would measure the window and fire at the wrong
//    interface size — which is the bug this attribute exists to fix.
for (const [name, text] of Object.entries(css)) {
  const found = withoutComments(text).match(/@media[^{]*\((?:max|min)-(?:width|device-width)\s*:/g) || [];
  assert.deepEqual(found, [],
    `${name}: a width media query cannot see the interface scale — guard the rule ` +
    "with :where(:root[data-vw~=\"…\"]) instead (see RESPONSIVE in main.css)");
}

// 2. No bare vw/vh unit. `zoom` does not reach them either: 44vw inside the
//    zoomed body is laid out as 44% of the window and then painted 1.5x
//    wider than that. --vw / --vh in themes.css are the corrected ones.
const VIEWPORT_UNIT = /(?:^|[\s(,:*/+-])\d*\.?\d+(vw|vh|vmin|vmax|dvw|dvh|svw|svh|lvw|lvh)\b/g;
for (const [name, text] of Object.entries(css)) {
  const body = withoutComments(text)
    // the two token definitions ARE the corrected units
    .replace(/--vw:[^;]+;/, "").replace(/--vh:[^;]+;/, "");
  const found = [...body.matchAll(VIEWPORT_UNIT)].map((m) => m[0].trim());
  assert.deepEqual(found, [],
    `${name}: use calc(N * var(--vw)) / var(--vh) — a bare viewport unit ignores the ` +
    "interface scale and overflows the window at anything above 100%");
}

// 3. Every data-vw token a stylesheet asks for is one Corvus.scale can write.
//    A typo here disables a responsive rule and nothing says so.
const known = new Set(scale.BREAKPOINTS.map((b) => b.token));
assert.ok(known.size >= 5, "the breakpoint ladder is the source of truth");
for (const [name, text] of Object.entries(css)) {
  for (const m of text.matchAll(/data-vw~="([^"]+)"/g)) {
    assert.ok(known.has(m[1]),
      `${name}: data-vw~="${m[1]}" is not a token Corvus.scale writes ` +
      `(have: ${[...known].join(", ")})`);
  }
}

// 4. And every token the ladder writes is actually used, so a breakpoint
//    cannot quietly outlive the rules it was added for.
const usedTokens = new Set();
for (const text of Object.values(css)) {
  for (const m of text.matchAll(/data-vw~="([^"]+)"/g)) usedTokens.add(m[1]);
}
assert.deepEqual([...known].filter((t) => !usedTokens.has(t)), [],
  "a breakpoint with no rule behind it is dead weight on every resize");

console.log("test_frontend_scale.js: all assertions passed");
