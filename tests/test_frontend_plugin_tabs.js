"use strict";

/**
 * Frontend tests for what a plugin may do outside its card:
 *
 *   PART A  the side panel's tabs (Corvus.panel): a tab added before PLUGINS,
 *           removed again, and CONSOLE / SSH switched off by Settings.
 *   PART B  the registry (Corvus.plugins): start(api) once per session, a tab
 *           of its own only while Settings allows it (and a card otherwise),
 *           and api.map, whose keys are the plugin's own.
 *
 * The real panel.js runs against a small DOM stub with a selector engine
 * that understands what the panel asks for (".panel-tabs .tab[data-tab=x]"),
 * so the tab strip is asserted as the operator would see it: in order, with
 * PLUGINS last.
 *
 * Run:
 *   node tests/test_frontend_plugin_tabs.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
global.Event = class Event { constructor(type) { this.type = type; } };
window.addEventListener = () => {};
window.removeEventListener = () => {};
window.dispatchEvent = () => true;
window.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
global.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };

// ---------------------------------------------------------------------------
// DOM stub with descendant selectors, classes, tags and [attr="value"].
// ---------------------------------------------------------------------------
function camel(name) { return name.replace(/-([a-z])/g, (_m, c) => c.toUpperCase()); }

function attrOf(el, name) {
  if (name.startsWith("data-")) return el.dataset[camel(name.slice(5))];
  if (name in el._attrs) return el._attrs[name];
  return el[name] == null ? undefined : String(el[name]);
}

function parseCompound(part) {
  const out = { tag: null, classes: [], attrs: [] };
  const re = /(\.[\w-]+)|(\[([\w-]+)(?:="([^"]*)")?\])|(#[\w-]+)|([\w-]+)/g;
  let m;
  while ((m = re.exec(part))) {
    if (m[1]) out.classes.push(m[1].slice(1));
    else if (m[2]) out.attrs.push({ name: m[3], value: m[4] });
    else if (m[5]) out.attrs.push({ name: "id", value: m[5].slice(1) });
    else if (m[6]) out.tag = m[6].toUpperCase();
  }
  return out;
}

function matchesCompound(el, c) {
  if (!el || !el._isEl) return false;
  if (c.tag && el.tagName !== c.tag) return false;
  const own = el.className.split(/\s+/).filter(Boolean);
  if (!c.classes.every((x) => own.includes(x))) return false;
  return c.attrs.every((a) => {
    const v = attrOf(el, a.name);
    return a.value === undefined ? v !== undefined : v === a.value;
  });
}

function matches(el, selector, scope) {
  const parts = selector.trim().split(/\s+/).map(parseCompound);
  if (!matchesCompound(el, parts[parts.length - 1])) return false;
  let node = el.parentNode;
  for (let i = parts.length - 2; i >= 0; i--) {
    while (node && node !== scope && !matchesCompound(node, parts[i])) node = node.parentNode;
    if (!node || node === scope) return false;
    node = node.parentNode;
  }
  return true;
}

function queryAll(root, selector) {
  const out = [];
  (function walk(list) {
    list.forEach((c) => {
      if (!c || !c._isEl) return;
      if (matches(c, selector, root)) out.push(c);
      walk(c.children);
    });
  })(root.children);
  return out;
}

function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", children: [], dataset: {},
    type: "", hidden: false, disabled: false, value: "", id: "", title: "",
    tabIndex: 0, scrollTop: 0, scrollHeight: 0, clientHeight: 0,
    _attrs: {}, _listeners: {}, _isEl: true, parentNode: null, _text: "",
  };
  Object.defineProperty(e, "textContent", {
    get() { return e._text + e.children.map((c) => c.textContent || "").join(""); },
    set(v) { e._text = String(v); e.children.length = 0; },
  });
  let html = "";
  Object.defineProperty(e, "innerHTML", {
    get() { return html; },
    // Markup is not parsed, but whatever was there before is replaced.
    set(v) { html = String(v); e.children.length = 0; },
  });
  e.style = { setProperty() {}, getPropertyValue: () => "" };
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
  const detach = (n) => { if (n.parentNode) n.parentNode.removeChild(n); };
  e.appendChild = (c) => { if (c._isEl) detach(c); c.parentNode = e; e.children.push(c); return c; };
  e.append = (...n) => n.forEach((x) => e.appendChild(x));
  e.insertBefore = (n, ref) => {
    if (n._isEl) detach(n);
    const i = ref ? e.children.indexOf(ref) : -1;
    if (i < 0) e.children.push(n); else e.children.splice(i, 0, n);
    n.parentNode = e;
    return n;
  };
  e.removeChild = (c) => {
    const i = e.children.indexOf(c);
    if (i >= 0) e.children.splice(i, 1);
    c.parentNode = null;
    return c;
  };
  e.remove = () => detach(e);
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.removeAttribute = (k) => { delete e._attrs[k]; };
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.removeEventListener = () => {};
  e.click = () => (e._listeners.click || []).slice().forEach((cb) => cb({ target: e, preventDefault() {} }));
  e.focus = () => {};
  e.closest = (sel) => { let n = e; while (n) { if (matches(n, sel, null)) return n; n = n.parentNode; } return null; };
  e.querySelector = (sel) => queryAll(e, sel)[0] || null;
  e.querySelectorAll = (sel) => queryAll(e, sel);
  Object.defineProperty(e, "firstChild", { get: () => e.children[0] || null });
  return e;
}

// The panel as index.html builds it, reduced to what the tabs touch.
const body = makeEl("body");
const byId = {};
function el(tag, opts, parent) {
  const n = makeEl(tag);
  if (opts.id) { n.id = opts.id; byId[opts.id] = n; }
  if (opts.className) n.className = opts.className;
  Object.assign(n.dataset, opts.dataset || {});
  if (opts.text) n.textContent = opts.text;
  if (parent) parent.appendChild(n);
  return n;
}
const rightPanel = el("aside", { id: "rightPanel", className: "right-panel" }, body);
el("button", { id: "panelHandle", className: "panel-handle" }, rightPanel);
const panelBody = el("div", { id: "panelBody", className: "panel-body" }, rightPanel);
const tabsEl = el("div", { id: "panelTabs", className: "panel-tabs" }, panelBody);
[["link", "LINK"], ["console", "CONSOLE"], ["ssh", "SSH"], ["future", "PLUGINS"]].forEach(([id, text], i) => {
  el("button", { className: "tab" + (i === 0 ? " active" : ""), dataset: { tab: id }, text }, tabsEl);
});
const panelContent = el("div", { className: "panel-content" }, panelBody);
["link", "console", "ssh", "future"].forEach((id, i) => {
  el("section", { className: "tab-panel" + (i === 0 ? " active" : ""), dataset: { panel: id } }, panelContent);
});
const sections = () => queryAll(panelContent, ".tab-panel");
const section = (id) => sections().find((s) => s.dataset.panel === id);
el("div", { id: "sshContent" }, section("ssh"));
el("div", { id: "futureContent" }, section("future"));
el("div", { id: "consoleOutput" }, section("console"));

// Scripts a plugin "folder" serves: the URL without its query -> the code
// that runs when the <script> loads. Appended scripts load on the next tick.
const fakeScripts = {};
const head = makeEl("head");
const headAppend = head.appendChild;
head.appendChild = (c) => {
  headAppend(c);
  if (c.tagName === "SCRIPT") {
    setTimeout(() => {
      const run = fakeScripts[String(c.src).split("?")[0]];
      if (!run) { if (c.onerror) c.onerror(); return; }
      run();
      if (c.onload) c.onload();
    }, 0);
  }
  return c;
};

global.document = {
  createElement: makeEl,
  createElementNS: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  createDocumentFragment: () => makeEl("fragment"),
  getElementById: (id) => byId[id] || (byId[id] = makeEl("div")),
  querySelector: (sel) => queryAll(body, sel)[0] || null,
  querySelectorAll: (sel) => queryAll(body, sel),
  addEventListener() {},
  removeEventListener() {},
  head,
  body,
};

// Telemetry the panel's console and SSH list ask for.
// requestJson goes through a swappable handler, because the plugin api binds
// the function itself once, at init.
const posts = [];
const defaultRequest = (url, opts) => {
  if (opts && opts.method === "POST") posts.push({ url, body: JSON.parse(opts.body) });
  return Promise.resolve({ connections: [] });
};
let requestHandler = defaultRequest;
Corvus.telemetry = {
  subscribe() { return () => {}; },
  subscribeConsole() { return () => {}; },
  getState() { return { connected: false, position: [0, 0] }; },
  requestJson(url, opts) { return requestHandler(url, opts); },
  postAction() { return Promise.resolve({ ok: true }); },
};

// The map half of api.map: recorded, not drawn.
const mapCalls = [];
const overlays = {};
Corvus.map = {
  setOverlay(id, coords, opts) {
    mapCalls.push(["set", id]);
    if (!Array.isArray(coords) || coords.length < 2) return false;
    overlays[id] = { coords, opts };
    return true;
  },
  setPolygonOverlay(id, coords, opts) {
    mapCalls.push(["polygon", id]);
    if (!Array.isArray(coords) || coords.length < 3) return false;
    overlays[id] = { coords, opts, kind: "polygon" };
    return true;
  },
  setTextOverlay(id, point, text, opts) {
    mapCalls.push(["text", id]);
    if (!Array.isArray(point) || !text) return false;
    overlays[id] = { coords: [point], text, opts, kind: "text" };
    return true;
  },
  setButtonOverlay(id, point, text, opts) {
    mapCalls.push(["button", id]);
    if (!Array.isArray(point) || !text) return false;
    overlays[id] = { coords: [point], text, opts, kind: "button" };
    return true;
  },
  removeOverlay(id) { mapCalls.push(["remove", id]); const had = !!overlays[id]; delete overlays[id]; return had; },
  setOverlayVisible(id, on) { mapCalls.push(["visible", id, on]); return !!overlays[id]; },
  hasOverlay: (id) => !!overlays[id],
  fitCoords(coords) { mapCalls.push(["fit", coords.length]); return coords.length > 0; },
};

let page = "settings";
Corvus.sidenav = {
  current: () => page,
  switchTo(p) { page = p; },
};

require("../src/js/ui.js");
require("../src/js/plugins.js");
require("../src/js/panel.js");

const panel = Corvus.panel;
const plugins = Corvus.plugins;
panel.init();   // also runs plugins.init on the PLUGINS content

const tabOrder = () => queryAll(tabsEl, ".tab").map((t) => t.dataset.tab);
const visibleTabs = () => queryAll(tabsEl, ".tab").filter((t) => !t.hidden).map((t) => t.dataset.tab);
const activeTab = () => (queryAll(tabsEl, ".tab").find((t) => t.classList.contains("active")) || {}).dataset;
const gridIds = () => queryAll(byId.futureContent, ".plugin-card").map((c) => c.dataset.pluginId);

function spyPlugin(extra) {
  const spy = {
    inits: [], destroys: [], starts: [],
    spec: Object.assign({
      name: "Spy", icon: "i", description: "d",
      init(c, api) { spy.inits.push({ c, api }); },
      destroy(c) { spy.destroys.push(c); },
    }, extra || {}),
  };
  return spy;
}

// ===========================================================================
// PART A: the panel's tabs
// ===========================================================================

function testAnAddedTabSitsBeforePlugins() {
  const sec = panel.addTab({ id: "x-one", label: "One" });
  assert.ok(sec, "addTab returns the section the tab shows");
  assert.deepEqual(tabOrder(), ["link", "console", "ssh", "x-one", "future"]);
  panel.addTab({ id: "x-two", label: "Two" });
  assert.deepEqual(tabOrder(), ["link", "console", "ssh", "x-one", "x-two", "future"],
    "PLUGINS stays the last tab");
  assert.equal(panel.addTab({ id: "x-one", label: "Again" }), sec, "the same id is the same tab");
  assert.equal(tabOrder().filter((t) => t === "x-one").length, 1);
  panel.removeTab("x-one");
  panel.removeTab("x-two");
  assert.deepEqual(tabOrder(), ["link", "console", "ssh", "future"]);
}

function testShowingATabRunsItsHookEveryTime() {
  let shown = 0;
  panel.addTab({ id: "x-hook", label: "Hook", onShow: () => { shown++; } });
  assert.equal(panel.showTab("x-hook"), true);
  assert.equal(activeTab().tab, "x-hook");
  assert.ok(section("x-hook").classList.contains("active"));
  panel.showTab("link");
  panel.showTab("x-hook");
  assert.equal(shown, 2);
  // Removing the tab that is showing hands over to the first one left.
  panel.removeTab("x-hook");
  assert.equal(activeTab().tab, "link");
  assert.equal(section("x-hook"), undefined, "its section is gone too");
}

function testConsoleAndSshCanBeSwitchedOff() {
  panel.showTab("ssh");
  assert.equal(panel.setTabHidden("ssh", true), true);
  assert.deepEqual(visibleTabs(), ["link", "console", "future"]);
  assert.equal(activeTab().tab, "link", "the hidden tab was showing: LINK takes over");
  assert.equal(panel.isTabShown("ssh"), false);
  assert.equal(panel.showTab("ssh"), false, "a hidden tab cannot be brought forward");
  assert.equal(activeTab().tab, "link");
  panel.setTabHidden("console", true);
  assert.deepEqual(visibleTabs(), ["link", "future"]);
  panel.setTabHidden("ssh", false);
  panel.setTabHidden("console", false);
  assert.deepEqual(visibleTabs(), ["link", "console", "ssh", "future"]);
  assert.equal(panel.isTabShown("ssh"), true);
}

function testLinkAndPluginsCannotBeSwitchedOff() {
  assert.equal(panel.setTabHidden("link", true), false);
  assert.equal(panel.setTabHidden("future", true), false);
  assert.deepEqual(visibleTabs(), ["link", "console", "ssh", "future"]);
}

function testPanelToggleCollapsesAndExpandsCleanly() {
  const p = byId.rightPanel;
  const h = byId.panelHandle;
  assert.equal(p.classList.contains("collapsed"), false);
  assert.equal(p.dataset.state, "open");

  // Non-user toggle (responsive layout auto-collapse)
  panel.setUserToggled(false);
  panel.toggle(false);
  assert.equal(p.classList.contains("collapsed"), true);
  assert.equal(p.dataset.state, "closed");
  assert.equal(h.title, "Expand panel");
  assert.equal(h.getAttribute("aria-expanded"), "false");
  assert.equal(panel.isUserToggled(), false, "toggle(false) does not mark userToggled");

  // User toggle (expanding)
  panel.toggle(true);
  assert.equal(p.classList.contains("collapsed"), false);
  assert.equal(p.dataset.state, "open");
  assert.equal(h.title, "Collapse panel");
  assert.equal(h.getAttribute("aria-expanded"), "true");
  assert.equal(panel.isUserToggled(), true, "toggle(true) marks userToggled");

  // User toggle (collapsing)
  panel.toggle();
  assert.equal(p.classList.contains("collapsed"), true);
  assert.equal(p.dataset.state, "closed");
  assert.equal(h.title, "Expand panel");
  assert.equal(h.getAttribute("aria-expanded"), "false");
  assert.equal(panel.isUserToggled(), true);

  // Restore open state
  panel.toggle(true);
  assert.equal(p.classList.contains("collapsed"), false);
  assert.equal(p.classList.contains("is-animating"), true, "is-animating set during transition");
}

// ===========================================================================
// PART B: the registry
// ===========================================================================

function testStartRunsOnceAtRegistration() {
  const spy = spyPlugin({ start(api) { spy.starts.push(api); } });
  plugins.register("t-start", spy.spec);
  assert.equal(spy.starts.length, 1, "start runs as soon as the plugin is registered");
  assert.equal(spy.inits.length, 0, "without anyone opening it");
  assert.equal(typeof spy.starts[0].getSettings, "function", "with the plugin's own api");
  assert.equal(typeof spy.starts[0].map.drawLine, "function");
  plugins.init(byId.futureContent, Corvus.telemetry);
  assert.equal(spy.starts.length, 1, "and never again in the session");
  plugins.unregister("t-start");
}

function testAThrowingStartCostsOnlyThatPlugin() {
  const ok = plugins.register("t-throws", spyPlugin({ start() { throw new Error("boom"); } }).spec);
  assert.equal(ok, true, "registered all the same");
  assert.ok(gridIds().includes("t-throws"), "and on the grid");
  plugins.unregister("t-throws");
}

function testATabIsOnlyGrantedWhenAllowed() {
  const spy = spyPlugin({ name: "Shelf", tab: true });
  plugins.register("t-shelf", spy.spec);
  assert.equal(plugins.tabsAllowed(), false, "off by default");
  assert.ok(gridIds().includes("t-shelf"), "a card while tabs are not allowed");
  assert.ok(!tabOrder().includes("plugin-t-shelf"));

  plugins.setTabsAllowed(true);
  assert.deepEqual(tabOrder(), ["link", "console", "ssh", "plugin-t-shelf", "future"],
    "its tab sits before PLUGINS");
  const tab = queryAll(tabsEl, ".tab").find((t) => t.dataset.tab === "plugin-t-shelf");
  assert.equal(tab.textContent, "Shelf", "captioned with the plugin's name");
  assert.ok(!gridIds().includes("t-shelf"), "no card as well: one way in");
  const listed = plugins.list().find((p) => p.id === "t-shelf");
  assert.equal(listed.tab, true);
  assert.equal(listed.inTab, true);
  assert.equal(spy.inits.length, 0, "built the first time it is shown, not before");

  panel.showTab("plugin-t-shelf");
  assert.equal(spy.inits.length, 1);
  const container = spy.inits[0].c;
  assert.ok(container.classList.contains("plugin-container"));
  assert.equal(typeof spy.inits[0].api.saveSettings, "function", "with its own api");
  panel.showTab("link");
  panel.showTab("plugin-t-shelf");
  assert.equal(spy.inits.length, 1, "kept while the operator moves between tabs");
  assert.equal(spy.destroys.length, 0);

  // open() on a plugin with a tab brings the tab forward.
  panel.showTab("link");
  assert.equal(plugins.open("t-shelf"), true);
  assert.equal(activeTab().tab, "plugin-t-shelf");
  assert.equal(plugins.getActive(), null, "nothing opened as a card");

  plugins.setTabsAllowed(false);
  assert.equal(spy.destroys.length, 1, "taking the tab away runs destroy");
  assert.equal(spy.destroys[0], container, "on the container init got");
  assert.ok(!tabOrder().includes("plugin-t-shelf"));
  assert.equal(activeTab().tab, "link", "the panel moves off the tab that went");
  assert.ok(gridIds().includes("t-shelf"), "and the card is back");
  plugins.unregister("t-shelf");
}

function testAPluginThatDoesNotAskStaysACard() {
  plugins.setTabsAllowed(true);
  plugins.register("t-card", spyPlugin({ name: "Card" }).spec);
  assert.ok(gridIds().includes("t-card"));
  assert.ok(!tabOrder().includes("plugin-t-card"));
  plugins.setTabsAllowed(false);
  plugins.unregister("t-card");
}

function testAnOpenCardMovesIntoItsTabCleanly() {
  const spy = spyPlugin({ name: "Mover", tab: true });
  plugins.register("t-move", spy.spec);
  plugins.open("t-move");
  assert.equal(plugins.getActive(), "t-move");
  plugins.setTabsAllowed(true);
  assert.equal(spy.destroys.length, 1, "the card is closed first, so it never runs twice");
  assert.equal(plugins.getActive(), null);
  assert.ok(tabOrder().includes("plugin-t-move"));
  plugins.unregister("t-move");
  assert.ok(!tabOrder().includes("plugin-t-move"), "unregister takes the tab away");
  plugins.setTabsAllowed(false);
}

function testUnregisteringATabbedPluginDestroysItsView() {
  const spy = spyPlugin({ name: "Gone", tab: true });
  plugins.setTabsAllowed(true);
  plugins.register("t-gone", spy.spec);
  panel.showTab("plugin-t-gone");
  plugins.unregister("t-gone");
  assert.equal(spy.destroys.length, 1);
  assert.equal(activeTab().tab, "link");
  plugins.setTabsAllowed(false);
}

function testWhenEveryPluginHasATabTheGridSaysSo() {
  const existing = plugins.list().map((p) => p.id);
  existing.forEach((id) => plugins.unregister(id));
  plugins.register("t-only", spyPlugin({ name: "Only", tab: true }).spec);
  plugins.setTabsAllowed(true);
  assert.match(byId.futureContent.innerHTML, /Every installed plugin has a tab of its own/);
  plugins.setTabsAllowed(false);
  plugins.unregister("t-only");
}

function testMapKeysBelongToThePlugin() {
  let apiA = null;
  let apiB = null;
  plugins.register("t-a", spyPlugin({ name: "A", start(api) { apiA = api; } }).spec);
  plugins.register("t-b", spyPlugin({ name: "B", start(api) { apiB = api; } }).spec);
  const line = [[11, 48], [11.1, 48.1]];
  assert.equal(apiA.map.drawLine("path", line, { color: "#2BC4E4" }), true);
  assert.equal(apiB.map.drawLine("path", line), true);
  assert.ok(overlays["plugin-t-a-path"], "namespaced by plugin");
  assert.ok(overlays["plugin-t-b-path"], "so both plugins keep their own");
  assert.equal(apiA.map.has("path"), true);
  assert.equal(apiA.map.drawLine("one", [[11, 48]]), false, "one point draws nothing");
  assert.equal(apiA.map.drawLine("", line), false, "a key is needed");
  assert.equal(apiA.map.setVisible("path", false), true);
  assert.equal(apiA.map.remove("path"), true);
  assert.equal(apiA.map.has("path"), false);
  assert.ok(overlays["plugin-t-b-path"], "B's line untouched by A's remove");

  // A strange key cannot reach outside the plugin's namespace.
  apiA.map.drawLine("../t-b-path", line);
  assert.ok(overlays["plugin-t-a-.._t-b-path"]);
  assert.equal(Object.keys(overlays).filter((k) => k.startsWith("plugin-t-b")).length, 1);

  // Unregistering a plugin takes its lines with it.
  plugins.unregister("t-b");
  assert.equal(overlays["plugin-t-b-path"], undefined);
  plugins.unregister("t-a");
  assert.equal(overlays["plugin-t-a-.._t-b-path"], undefined);
}

function testMapColoursAvoidTheTrackAndThePlan() {
  let api = null;
  plugins.register("t-colours", spyPlugin({ start(a) { api = a; } }).spec);
  const colours = api.map.colors;
  assert.ok(colours.length >= 3, "a choice of a few");
  const ids = new Set();
  colours.forEach((c) => {
    assert.match(c.color, /^#[0-9A-F]{6}$/i);
    assert.ok(c.label && c.id);
    ids.add(c.id);
    const r = parseInt(c.color.slice(1, 3), 16);
    const g = parseInt(c.color.slice(3, 5), 16);
    const b = parseInt(c.color.slice(5, 7), 16);
    const redOrAmber = r > 180 && b < 90;
    assert.ok(!redOrAmber, `${c.label} would read as the track or the plan route`);
  });
  assert.equal(ids.size, colours.length, "ids are unique");
  colours[0].color = "#000000";
  assert.notEqual(plugins.LINE_COLORS[0].color, "#000000", "a copy, not the palette itself");
  let other = null;
  plugins.register("t-colours-2", spyPlugin({ start(a) { other = a; } }).spec);
  assert.notEqual(other.map.colors[0].color, "#000000", "another plugin sees the real palette");
  plugins.unregister("t-colours-2");
  plugins.unregister("t-colours");
}

function testFitBringsHomeForward() {
  let api = null;
  plugins.register("t-fit", spyPlugin({ start(a) { api = a; } }).spec);
  page = "settings";
  mapCalls.length = 0;
  assert.equal(api.map.fit([[11, 48], [11.1, 48.1]]), true);
  assert.equal(page, "home", "the Home page comes forward");
  assert.deepEqual(mapCalls, [["fit", 2]]);
  plugins.unregister("t-fit");
}

function testApiMapWithoutAMapAnswersFalse() {
  let api = null;
  plugins.register("t-nomap", spyPlugin({ start(a) { api = a; } }).spec);
  const saved = Corvus.map;
  Corvus.map = undefined;
  try {
    assert.equal(api.map.drawLine("x", [[1, 2], [3, 4]]), false);
    assert.equal(api.map.remove("x"), false);
    assert.equal(api.map.fit([[1, 2]]), false);
  } finally {
    Corvus.map = saved;
  }
  plugins.unregister("t-nomap");
}


function testPolygonCircleAndTextAreThePluginsOwn() {
  let api = null;
  plugins.register("t-shapes", spyPlugin({ start(a) { api = a; } }).spec);
  const square = [[11, 48], [11.1, 48], [11.1, 48.1], [11, 48.1]];
  assert.equal(api.map.drawPolygon("zone", square, { fillOpacity: 0.3 }), true);
  assert.equal(overlays["plugin-t-shapes-zone"].kind, "polygon");
  assert.equal(api.map.drawPolygon("thin", square.slice(0, 2)), false);
  assert.equal(api.map.drawCircle("ring", [11, 48], 100), true);
  const ring = overlays["plugin-t-shapes-ring"].coords;
  assert.equal(ring.length, 72, "a smooth ring by default");
  const R = 6371008.8;
  const rad = Math.PI / 180;
  ring.forEach(([lng, lat]) => {
    const dLat = (lat - 48) * rad;
    const dLng = (lng - 11) * rad;
    const a = Math.sin(dLat / 2) ** 2 + Math.cos(48 * rad) * Math.cos(lat * rad) * Math.sin(dLng / 2) ** 2;
    const d = 2 * R * Math.asin(Math.sqrt(a));
    assert.ok(Math.abs(d - 100) < 0.01, `every corner 100 m out, not ${d}`);
  });
  assert.equal(api.map.drawCircle("ring", [11, 48], -5), false, "no negative radius");
  assert.equal(api.map.drawCircle("ring", [11, 95], 5), false, "no centre off the globe");
  assert.equal(api.map.drawText("label", [11, 48], "Pad", { dot: true }), true);
  assert.equal(overlays["plugin-t-shapes-label"].text, "Pad");
  assert.equal(api.map.has("label"), true);
  let clicks = 0;
  assert.equal(api.map.drawButton("go", [11, 48], "Go", { onClick() { clicks++; } }), true);
  assert.equal(overlays["plugin-t-shapes-go"].kind, "button");
  overlays["plugin-t-shapes-go"].opts.onClick();
  assert.equal(clicks, 1, "the plugin's handler is called");
  assert.equal(api.map.drawButton("boom", [11, 48], "Boom", { onClick() { throw new Error("x"); } }), true);
  assert.doesNotThrow(() => overlays["plugin-t-shapes-boom"].opts.onClick(), "a throwing handler stays in the plugin");
  assert.equal(api.map.drawButton("none", [11, 48], ""), false, "no label, no button");
  plugins.unregister("t-shapes");
  assert.ok(!Object.keys(overlays).some((k) => k.startsWith("plugin-t-shapes")),
    "every shape goes with the plugin, whatever its kind");
}

async function testOptionsAreDeclaredReadAndSaved() {
  let api = null;
  const changes = [];
  plugins.setTabsAllowed(false);
  plugins.register("t-opt", spyPlugin({
    name: "Opt", tab: true,
    start(a) { api = a; },
    options: [
      { key: "labels", label: "Show labels", type: "toggle", default: true },
      { key: "units", label: "Units", type: "select", default: "m",
        choices: [{ value: "m", label: "Metres" }, { value: "ft", label: "Feet" }] },
      { key: "radius", label: "Radius", type: "number", default: "5", min: 1, max: 10 },
      { key: "tab", label: "Mine", type: "toggle" },
      { key: "labels", label: "Again", type: "toggle" },
      { key: "colour", label: "Colour", type: "rainbow" },
      { key: "2bad", label: "Bad key", type: "text" },
    ],
    optionsChanged(values) { changes.push(values); },
  }).spec);
  assert.equal(plugins.hasOptions("t-opt"), true);
  const opts = plugins.options("t-opt");
  assert.deepEqual(opts.schema.map((o) => o.key), ["tab", "labels", "units", "radius"],
    "the tab switch first, then the plugin's own; the unusable ones are dropped");
  assert.deepEqual(opts.values, { tab: false, labels: true, units: "m", radius: 5 });
  assert.deepEqual(api.getOptions(), opts.values, "the plugin reads the same values");

  posts.length = 0;
  const values = await plugins.setOption("t-opt", "radius", "50");
  assert.equal(values.radius, 10, "clamped to max");
  assert.deepEqual(posts[0], { url: "/api/plugins/settings",
    body: { id: "t-opt", settings: { _options: { radius: 10 } } } });
  assert.equal(changes.length, 1, "optionsChanged runs");
  assert.equal(changes[0].radius, 10);
  assert.equal(api.getOptions().radius, 10);
  assert.equal("_options" in api.getSettings(), false, "the plugin's own settings never show them");

  await assert.rejects(plugins.setOption("t-opt", "units", "parsec"), /valid/);
  await assert.rejects(plugins.setOption("t-opt", "nope", 1), /No option/);

  // The tab switch moves it out of the grid and into a tab, without a hook.
  assert.ok(gridIds().includes("t-opt"));
  await plugins.setOption("t-opt", "tab", true);
  assert.ok(tabOrder().includes("plugin-t-opt"), "its own tab");
  assert.ok(!gridIds().includes("t-opt"));
  assert.equal(changes.length, 1, "the tab is Corvus's business, not the plugin's");

  // A save that fails puts the tab back where it was.
  requestHandler = () => Promise.reject(new Error("disk full"));
  await assert.rejects(plugins.setOption("t-opt", "tab", false), /disk full/);
  assert.ok(tabOrder().includes("plugin-t-opt"), "still a tab: the change was not kept");
  assert.equal(plugins.options("t-opt").values.tab, true);
  requestHandler = defaultRequest;

  // The plugin writing its own state cannot set or clear the options.
  posts.length = 0;
  await api.saveSettings({ a: 1, _options: { tab: false } }, true);
  assert.deepEqual(posts[0].body.settings, { a: 1 });
  assert.equal(api.getOptions().tab, true);
  assert.deepEqual(api.getSettings(), { a: 1 });
  plugins.unregister("t-opt");
}

async function testTheOperatorsTabChoiceBeatsTheOldSwitch() {
  plugins.register("t-own", spyPlugin({ name: "Own", tab: true }).spec);
  await plugins.setOption("t-own", "tab", false);
  plugins.setTabsAllowed(true);
  assert.ok(!tabOrder().includes("plugin-t-own"), "switched off for this one, so a card");
  await plugins.setOption("t-own", "tab", true);
  plugins.setTabsAllowed(false);
  assert.ok(tabOrder().includes("plugin-t-own"), "switched on for this one, so a tab");
  plugins.unregister("t-own");
  plugins.register("t-plain", spyPlugin({ name: "Plain" }).spec);
  assert.equal(plugins.hasOptions("t-plain"), false, "nothing to set, no gear");
  assert.equal(plugins.options("t-plain").schema.length, 0);
  plugins.unregister("t-plain");
}

async function testReloadLoadsThePluginFoldersAgain() {
  let version = 1;
  let starts = 0;
  let destroyed = 0;
  fakeScripts["/api/plugins/asset/t-rl/t-rl.js"] = () => plugins.register("t-rl", {
    name: "Reload v" + version,
    init() {},
    destroy() { destroyed++; },
    start() { starts++; },
  });
  let onDisk = [{ id: "t-rl", scripts: ["t-rl.js"], styles: ["t-rl.css"] }];
  requestHandler = (url, opts) => (url === "/api/plugins"
    ? Promise.resolve({ plugins: onDisk, settings: {} })
    : defaultRequest(url, opts));
  const files = () => head.children.filter((c) => /\/t-rl\//.test(c.src || c.href || ""));
  try {
    assert.deepEqual(await plugins.loadInstalled(), ["t-rl"]);
    assert.equal(plugins.list().find((p) => p.id === "t-rl").name, "Reload v1");
    assert.equal(starts, 1);
    plugins.open("t-rl");

    version = 2;
    assert.deepEqual(await plugins.reload(), ["t-rl"]);
    assert.equal(plugins.list().find((p) => p.id === "t-rl").name, "Reload v2", "the new script ran");
    assert.equal(destroyed, 1, "the open view was torn down first");
    assert.equal(starts, 2, "start runs again");
    assert.equal(files().length, 2, "one script and one stylesheet, not two of each");
    assert.ok(files().every((f) => /\?r=\d+$/.test(f.src || f.href)), "fetched past the cache");

    onDisk = [];
    assert.deepEqual(await plugins.reload(), []);
    assert.equal(plugins.list().some((p) => p.id === "t-rl"), false, "a deleted plugin goes");
    assert.equal(files().length, 0, "and its files leave the document");
  } finally {
    requestHandler = defaultRequest;
    delete fakeScripts["/api/plugins/asset/t-rl/t-rl.js"];
  }
}

const tests = [
  testAnAddedTabSitsBeforePlugins,
  testShowingATabRunsItsHookEveryTime,
  testConsoleAndSshCanBeSwitchedOff,
  testLinkAndPluginsCannotBeSwitchedOff,
  testPanelToggleCollapsesAndExpandsCleanly,
  testStartRunsOnceAtRegistration,
  testAThrowingStartCostsOnlyThatPlugin,
  testATabIsOnlyGrantedWhenAllowed,
  testAPluginThatDoesNotAskStaysACard,
  testAnOpenCardMovesIntoItsTabCleanly,
  testUnregisteringATabbedPluginDestroysItsView,
  testWhenEveryPluginHasATabTheGridSaysSo,
  testMapKeysBelongToThePlugin,
  testMapColoursAvoidTheTrackAndThePlan,
  testFitBringsHomeForward,
  testApiMapWithoutAMapAnswersFalse,
  testPolygonCircleAndTextAreThePluginsOwn,
  testOptionsAreDeclaredReadAndSaved,
  testTheOperatorsTabChoiceBeatsTheOldSwitch,
  testReloadLoadsThePluginFoldersAgain,
];

// The start hook that throws logs through console.error; keep the run quiet.
const realError = console.error;
let failed = 0;
(async () => {
for (const t of tests) {
  console.error = () => {};
  try {
    await t();
    console.error = realError;
    console.log(`ok   - ${t.name}`);
  } catch (err) {
    console.error = realError;
    failed++;
    console.error(`FAIL - ${t.name}`);
    console.error(`      ${err && err.stack ? err.stack.split("\n").join("\n      ") : err}`);
  }
}
if (failed) {
  console.error(`\n${failed}/${tests.length} plugin tab test(s) FAILED`);
  process.exit(1);
}
console.log(`\nAll ${tests.length} plugin tab tests passed.`);
process.exit(0);
})();
