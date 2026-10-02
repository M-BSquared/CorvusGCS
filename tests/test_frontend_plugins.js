"use strict";

/**
 * Frontend tests for the PLUGINS-tab plugin registry (Corvus.plugins), the
 * drop-in plugin folder and per-plugin settings.
 *
 * No real plugin is loaded: every plugin here is registered by the test
 * itself, so the suite does not depend on which plugins are installed. Each
 * plugin keeps its own tests in plugins/<id>/tests/.
 *
 * Plain Node-runnable assertions (no browser, no test runner) following the
 * same pattern as tests/test_frontend_link.js and tests/frontend_telemetry.test.js:
 * stub the globals the modules touch, require the source, and assert on the
 * pure functions and the registry lifecycle. A small DOM stub (no HTML
 * parser) is used because the source builds its DOM via createElement, so a
 * children-walking stub suffices.
 *
 * Run:
 *   node tests/test_frontend_plugins.js
 */
const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals so plugins.js and the folder plugins load and run in Node.
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

// The chart modules subscribe to corvus:themechange on window, so the listener
// pair has to exist for the theme-reactive redraw path to be exercised at all.
const windowListeners = {};
window.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
window.removeEventListener = (t, cb) => {
  const list = windowListeners[t] || [];
  const i = list.indexOf(cb);
  if (i >= 0) list.splice(i, 1);
};
// api.notification dispatches corvus:notification for the topbar's board, so
// the stub has to deliver it — without one, the call was silently swallowed by
// the guard around it and the board half of a notification went untested.
window.dispatchEvent = (event) => {
  (windowListeners[event && event.type] || []).forEach((cb) => cb(event));
  return true;
};


global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};

// prefers-reduced-motion toggle (api.reducedMotion reads this).
let reducedMotion = false;
window.matchMedia = (query) => ({
  matches: reducedMotion && String(query).includes("prefers-reduced-motion"),
  media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});

// Controlled clock for the throttle test.
let clock = 1000;
Date.now = () => clock;

// lucide is optional in the source (refreshIcons no-ops when absent). Leave it
// undefined so we also exercise that path.
// window.Plotly is set per-test below.

// ---------------------------------------------------------------------------
// Minimal DOM stub. The source builds structure with createElement + appendChild
// (no innerHTML-based querySelector), so this stub only needs to track
// children, className/classList, textContent, dataset, listeners, and an
// innerHTML string (cleared children when set to "").
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "",
    textContent: "",
    children: [],
    dataset: {},
    style: {},
    type: "",
    hidden: false,
    disabled: false,
    value: "",
    id: "",
    _attrs: {},
    _listeners: {},
    _isEl: true,
  };
  let _html = "";
  Object.defineProperty(e, "innerHTML", {
    get() { return _html; },
    set(v) {
      _html = String(v);
      if (_html === "") e.children.length = 0;   // mirror: clearing HTML drops children
    },
  });
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) { const has = e.classList.contains(c); const next = force === undefined ? !has : !!force; if (next) e.classList.add(c); else e.classList.remove(c); return next; },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  // parentNode is maintained like a real DOM so the standard
  // `node.parentNode.removeChild(node)` removal idiom works under the stub —
  // Corvus.ui.modal.close() uses it to unmount a dialog.
  e.appendChild = (c) => { c.parentNode = e; e.children.push(c); return c; };
  e.append = (...nodes) => { nodes.forEach((n) => e.appendChild(n)); };
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; return c; };
  e.insertBefore = (n, ref) => { const i = ref ? e.children.indexOf(ref) : e.children.length; if (i < 0) e.children.push(n); else e.children.splice(i, 0, n); return n; };
  Object.defineProperty(e, "firstChild", { get() { return e.children[0] || null; } });
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (type, cb) => { (e._listeners[type] = e._listeners[type] || []).push(cb); };
  e.removeEventListener = () => {};
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  return e;
}

function querySel(children, sel) {
  // Supports a single class selector (".cls") or tag name. Walks recursively.
  const out = [];
  const wantTag = sel && sel[0] !== ".";
  const classes = sel ? sel.split(".").filter(Boolean) : [];
  function walk(list) {
    for (const c of list) {
      if (!c || !c._isEl) continue;
      const ok = wantTag ? c.tagName === sel.toUpperCase() : classes.every((cl) => c.className.split(/\s+/).includes(cl));
      if (ok) out.push(c);
      if (c.children) walk(c.children);
    }
  }
  walk(children);
  return out;
}

// <head>, plus the loader's script/link elements. Corvus.plugins.loadInstalled
// appends a <script> and waits for its onload, which no stub can produce by
// actually fetching — so appending one here fires the callback the appended
// element was given, and `scriptBehaviour` decides whether that is a load or an
// error. `appendedScripts` is what the assertions read.
const appendedScripts = [];
const appendedStyles = [];
let scriptBehaviour = () => "load";

const head = makeEl("head");
head.appendChild = (el) => {
  el.parentNode = head;
  head.children.push(el);
  if (el.tagName === "SCRIPT") {
    appendedScripts.push(el.src);
    const outcome = scriptBehaviour(el.src);
    setTimeout(() => {
      if (outcome === "error") { if (el.onerror) el.onerror(new Error("failed")); return; }
      if (typeof outcome === "function") outcome(el.src);
      if (el.onload) el.onload();
    }, 0);
  } else if (el.tagName === "LINK") {
    appendedStyles.push(el.href);
  }
  return el;
};

// The toast stack mounts here, so the push half of api.notification is real
// in these tests rather than swallowed by the guard around it.
const body = makeEl("body");

global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  head,
  body,
};

// The launcher asks before removing a button that is running something.
window.confirm = () => true;

// Flush the microtask queue (for the plugin's best-effort postAction promises).
function flushMicrotasks() { return new Promise((r) => setTimeout(r, 0)); }

// ---------------------------------------------------------------------------
// Load order mirrors the browser's: plugins.js (a <script> tag in index.html)
// first, then the folder plugins, which register themselves at load.
// ---------------------------------------------------------------------------
// ui.js first: it defines Corvus.ui, the component layer every other
// module builds its DOM with (index.html loads it in the same order).
require("../src/js/ui.js");
require("../src/js/plugins.js");

// ---------------------------------------------------------------------------
// PART A: plugin registry
// ---------------------------------------------------------------------------

function makeSpyPlugin(id, name) {
  const spy = {
    id,
    initCalls: 0,
    initArgs: [],
    destroyCalls: 0,
    destroyArgs: [],
    init(containerEl, api) { spy.initCalls++; spy.initArgs.push({ containerEl, api }); },
    destroy(containerEl) { spy.destroyCalls++; spy.destroyArgs.push(containerEl); },
  };
  return spy;
}

function testRegisterAndList() {
  const ok = Corvus.plugins.register("t-list", {
    name: "Lister", icon: "star", description: "lists things",
    init() {}, destroy() {},
  });
  assert.equal(ok, true, "register returns true for a new id");
  const found = Corvus.plugins.list().find((p) => p.id === "t-list");
  assert.ok(found, "registered plugin appears in list()");
  assert.equal(found.name, "Lister");
  assert.equal(found.icon, "star");
  assert.equal(found.description, "lists things");
}

function testDuplicateRegisterRejected() {
  // Duplicate id is rejected by returning false (documented contract), not by
  // throwing, so a misbehaving plugin module never breaks app init.
  const first = Corvus.plugins.register("t-dup", { name: "A", icon: "i", description: "d", init() {}, destroy() {} });
  const second = Corvus.plugins.register("t-dup", { name: "B", icon: "i", description: "d", init() {}, destroy() {} });
  assert.equal(first, true);
  assert.equal(second, false, "duplicate id is rejected (returns false)");
  const entries = Corvus.plugins.list().filter((p) => p.id === "t-dup");
  assert.equal(entries.length, 1, "only the first registration is kept");
  assert.equal(entries[0].name, "A");
}

function testOpenCloseLifecycle() {
  const root = makeEl("div");
  const telemetry = { subscribe() { return () => {}; }, getState() { return null; }, requestJson() { return Promise.resolve({}); }, postAction() { return Promise.resolve({ ok: true }); } };
  Corvus.plugins.init(root, telemetry);

  const spy = makeSpyPlugin("t-lifecycle", "Lifecycle");
  Corvus.plugins.register("t-lifecycle", { name: spy.id, icon: "i", description: "d", init: spy.init, destroy: spy.destroy });

  assert.equal(Corvus.plugins.getActive(), null, "nothing active before open");
  assert.equal(Corvus.plugins.open("t-lifecycle"), true);
  assert.equal(spy.initCalls, 1, "init called exactly once on open");
  assert.ok(spy.initArgs[0].containerEl, "init received a container element");
  assert.equal(spy.initArgs[0].containerEl.className, "plugin-container");
  assert.ok(spy.initArgs[0].api && typeof spy.initArgs[0].api.subscribe === "function", "init received the api");
  assert.equal(Corvus.plugins.getActive(), "t-lifecycle");
  assert.equal(root.children[0].className, "plugin-view", "grid replaced by plugin view on open");

  Corvus.plugins.close();
  assert.equal(spy.destroyCalls, 1, "destroy called exactly once on close");
  assert.equal(spy.destroyArgs[0], spy.initArgs[0].containerEl, "destroy received the same container init got");
  assert.equal(Corvus.plugins.getActive(), null, "nothing active after close");
  assert.equal(root.children[0].className, "plugin-grid", "grid restored on close (container removed)");
}

function testOpenSecondClosesFirst() {
  const root = makeEl("div");
  const telemetry = { subscribe() { return () => {}; }, getState() { return null; }, requestJson() { return Promise.resolve({}); }, postAction() { return Promise.resolve({ ok: true }); } };
  Corvus.plugins.init(root, telemetry);

  const a = makeSpyPlugin("t-a", "A");
  const b = makeSpyPlugin("t-b", "B");
  Corvus.plugins.register("t-a", { name: "A", icon: "i", description: "d", init: a.init, destroy: a.destroy });
  Corvus.plugins.register("t-b", { name: "B", icon: "i", description: "d", init: b.init, destroy: b.destroy });

  Corvus.plugins.open("t-a");
  assert.equal(a.initCalls, 1);
  assert.equal(Corvus.plugins.getActive(), "t-a");

  Corvus.plugins.open("t-b");
  assert.equal(a.destroyCalls, 1, "first plugin destroyed when second opens");
  assert.equal(b.initCalls, 1, "second plugin initialised");
  assert.equal(Corvus.plugins.getActive(), "t-b");
  assert.equal(a.initCalls, 1, "first plugin init not called again");

  Corvus.plugins.close();
  assert.equal(b.destroyCalls, 1);
  assert.equal(Corvus.plugins.getActive(), null);
}

function testOpenUnknownIdIsNoOp() {
  assert.equal(Corvus.plugins.open("does-not-exist"), false);
  assert.equal(Corvus.plugins.getActive(), null);
}

// ---------------------------------------------------------------------------
// PART C: the drop-in plugin folder (Corvus.plugins.loadInstalled)
// ---------------------------------------------------------------------------

/** Run *fn* with console.error muted — two tests below exercise the loader's
 *  failure paths on purpose, and their logging is the expected behaviour, not
 *  output worth printing on a passing run. */
async function quietly(fn) {
  const real = console.error;
  console.error = () => {};
  try { return await fn(); }
  finally { console.error = real; }
}

/** Re-init the registry against a telemetry stub whose responses are scripted. */
function initWithResponses(responses) {
  const calls = [];
  const telemetry = {
    subscribe() { return () => {}; },
    getState() { return null; },
    requestJson(url, options) {
      calls.push({ url, options });
      const answer = responses[url];
      if (answer === undefined) return Promise.reject(new Error("no stub for " + url));
      return Promise.resolve(typeof answer === "function" ? answer() : answer);
    },
    postAction() { return Promise.resolve({ ok: true }); },
  };
  Corvus.plugins.init(makeEl("div"), telemetry);
  return calls;
}

async function testLoadInstalledAppendsScriptsAndStyles() {
  appendedScripts.length = 0;
  appendedStyles.length = 0;
  initWithResponses({
    "/api/plugins": {
      plugins: [{ id: "demo", scripts: ["demo.js"], styles: ["demo.css"] }],
      settings: {},
    },
  });
  const loaded = await Corvus.plugins.loadInstalled();
  assert.deepEqual(loaded, ["demo"], "loadInstalled reports the ids it loaded");
  assert.deepEqual(appendedScripts, ["/api/plugins/asset/demo/demo.js"]);
  assert.deepEqual(appendedStyles, ["/api/plugins/asset/demo/demo.css"]);
  // An inserted script is async by default and would run whenever its download
  // finished, so the grid order would follow the network, not the manifests.
  const tag = head.children.filter((c) => c.tagName === "SCRIPT").pop();
  assert.equal(tag.async, false, "plugin scripts run in the order discovery listed them");
}

async function testLoadInstalledEncodesEachPathSegment() {
  appendedScripts.length = 0;
  initWithResponses({
    "/api/plugins": { plugins: [{ id: "nested", scripts: ["sub dir/a b.js"] }], settings: {} },
  });
  await Corvus.plugins.loadInstalled();
  // Segments are encoded individually so the separators survive — a plugin
  // that puts its script in a subfolder must still resolve.
  assert.deepEqual(appendedScripts, ["/api/plugins/asset/nested/sub%20dir/a%20b.js"]);
}

async function testLoadInstalledSurvivesAFailingPlugin() {
  appendedScripts.length = 0;
  scriptBehaviour = (src) => (src.includes("bad") ? "error" : "load");
  initWithResponses({
    "/api/plugins": {
      plugins: [
        { id: "bad", scripts: ["bad.js"] },
        { id: "fine", scripts: ["fine.js"] },
      ],
      settings: {},
    },
  });
  const loaded = await Corvus.plugins.loadInstalled();
  scriptBehaviour = () => "load";
  // One plugin failing to load costs that plugin only.
  assert.deepEqual(loaded, ["fine"]);
  assert.equal(appendedScripts.length, 2, "both were attempted");
}

async function testLoadInstalledSurvivesADeadEndpoint() {
  initWithResponses({});   // /api/plugins rejects
  const loaded = await Corvus.plugins.loadInstalled();
  assert.deepEqual(loaded, [], "a failed discovery resolves empty, never rejects");
}

async function testLoadInstalledDoesNotLoadTheSamePluginTwice() {
  appendedScripts.length = 0;
  initWithResponses({
    "/api/plugins": { plugins: [{ id: "once", scripts: ["once.js"] }], settings: {} },
  });
  await Corvus.plugins.loadInstalled();
  const second = await Corvus.plugins.loadInstalled();
  assert.deepEqual(second, [], "an already-loaded plugin is not appended again");
  assert.equal(appendedScripts.length, 1);
}

async function testInstalledPluginRegistersOnLoad() {
  // The whole point of the mechanism: the appended script's own register()
  // call puts the plugin in the grid.
  scriptBehaviour = () => () => {
    Corvus.plugins.register("t-installed", {
      name: "Installed", icon: "puzzle", description: "arrived via the folder",
      init() {}, destroy() {},
    });
  };
  initWithResponses({
    "/api/plugins": { plugins: [{ id: "t-installed", scripts: ["t-installed.js"] }], settings: {} },
  });
  await Corvus.plugins.loadInstalled();
  scriptBehaviour = () => "load";
  const found = Corvus.plugins.list().find((p) => p.id === "t-installed");
  assert.ok(found, "a plugin that registers as its script runs appears in the grid");
  assert.equal(found.description, "arrived via the folder");
}

// ---------------------------------------------------------------------------
// PART D: per-plugin settings
// ---------------------------------------------------------------------------

async function testSettingsAreSeededAndScopedPerPlugin() {
  const calls = initWithResponses({
    "/api/plugins": {
      plugins: [],
      settings: { "t-settings": { directory: "/srv" }, other: { x: 1 } },
    },
    "/api/plugins/settings": () => ({ ok: true, settings: { directory: "/srv", command: "./run" } }),
  });
  await Corvus.plugins.loadInstalled();

  let seen = null;
  Corvus.plugins.register("t-settings", {
    name: "Settings", icon: "i", description: "d",
    init(_el, api) { seen = api; }, destroy() {},
  });
  Corvus.plugins.open("t-settings");

  assert.deepEqual(seen.getSettings(), { directory: "/srv" },
    "a plugin sees its own saved settings, seeded before its init ran");

  const merged = await seen.saveSettings({ command: "./run" });
  assert.deepEqual(merged, { directory: "/srv", command: "./run" });
  const post = calls.find((c) => c.url === "/api/plugins/settings");
  const body = JSON.parse(post.options.body);
  assert.equal(body.id, "t-settings", "saveSettings is bound to the plugin it was handed to");
  assert.equal(body.replace, false, "a plain save merges");
  assert.deepEqual(seen.getSettings(), { directory: "/srv", command: "./run" },
    "the local copy tracks what the backend returned");
  Corvus.plugins.close();
}

async function testSaveSettingsCanReplace() {
  const calls = initWithResponses({
    "/api/plugins": { plugins: [], settings: { "t-replace": { old: 1, keep: 2 } } },
    "/api/plugins/settings": () => ({ ok: true, settings: { keep: 3 } }),
  });
  await Corvus.plugins.loadInstalled();
  let api = null;
  Corvus.plugins.register("t-replace", {
    name: "Replace", icon: "i", description: "d",
    init(_el, a) { api = a; }, destroy() {},
  });
  Corvus.plugins.open("t-replace");
  const saved = await api.saveSettings({ keep: 3 }, true);
  assert.equal(JSON.parse(calls.find((c) => c.url === "/api/plugins/settings").options.body).replace, true);
  // The retired key is gone from the local copy too, not just on disk.
  assert.deepEqual(saved, { keep: 3 });
  assert.deepEqual(api.getSettings(), { keep: 3 });
  Corvus.plugins.close();
}

async function testGetSettingsIsAnIsolatedCopy() {
  initWithResponses({
    "/api/plugins": { plugins: [], settings: { "t-copy": { a: 1 } } },
  });
  await Corvus.plugins.loadInstalled();
  let api = null;
  Corvus.plugins.register("t-copy", {
    name: "Copy", icon: "i", description: "d",
    init(_el, a) { api = a; }, destroy() {},
  });
  Corvus.plugins.open("t-copy");
  const first = api.getSettings();
  first.a = 99;
  assert.deepEqual(api.getSettings(), { a: 1 },
    "mutating the returned object cannot corrupt the store");
  Corvus.plugins.close();
}

async function testPostJsonResolvesOnAFailureBody() {
  initWithResponses({
    "/api/thing": () => ({ ok: false, error: "no", stderr: "the detail" }),
  });
  let api = null;
  Corvus.plugins.register("t-postjson", {
    name: "PostJson", icon: "i", description: "d",
    init(_el, a) { api = a; }, destroy() {},
  });
  Corvus.plugins.open("t-postjson");
  // postAction would reject here and throw the stderr away; postJson does not.
  const res = await api.postJson("/api/thing", { x: 1 });
  assert.equal(res.stderr, "the detail");
  Corvus.plugins.close();
}

/** Every toast currently on screen, as {level, title, message}. */
function toasts() {
  return querySel(body.children, ".ui-toast").map((el) => ({
    level: el.dataset.level,
    title: (querySel(el.children, ".ui-toast-title")[0] || {}).textContent,
    message: (querySel(el.children, ".ui-toast-body")[0] || {}).textContent,
  }));
}

/** Dismiss them all, which also clears the timers they would close on — a
 *  pending 6-second timer per warning would keep Node alive long after the
 *  last assertion. */
function dismissToasts() {
  querySel(body.children, ".ui-toast-close").forEach((btn) => click(btn));
}

async function testNotificationIsPushedAtTheOperatorNotOnlyFiled() {
  initWithResponses({});
  dismissToasts();
  const board = [];
  const onBoard = (event) => board.push(event.detail);
  window.addEventListener("corvus:notification", onBoard);

  let api = null;
  Corvus.plugins.register("t-notify", {
    name: "Notifier", icon: "i", description: "d",
    init(_el, a) { api = a; }, destroy() {},
  });
  Corvus.plugins.open("t-notify");
  api.notification("warning", "It would not start");

  // The board keeps it, as it always did — and now something actually says so
  // on screen, instead of a badge in a corner nobody is looking at.
  assert.deepEqual(board, [
    { level: "warning", message: "Notifier: It would not start" },
  ]);
  assert.deepEqual(toasts(), [
    { level: "warning", title: "Notifier", message: "It would not start" },
  ], "the plugin's name titles the toast, so its message stays its own");

  dismissToasts();
  window.removeEventListener("corvus:notification", onBoard);
  Corvus.plugins.close();
}

/** Fire an element's first click listener. */
function click(el) { el._listeners.click[0](); }

// ---------------------------------------------------------------------------
// Run all tests.
// ---------------------------------------------------------------------------
async function run() {
  testRegisterAndList();
  testDuplicateRegisterRejected();
  testOpenCloseLifecycle();
  testOpenSecondClosesFirst();
  testOpenUnknownIdIsNoOp();
  await testLoadInstalledAppendsScriptsAndStyles();
  await testLoadInstalledEncodesEachPathSegment();
  await quietly(testLoadInstalledSurvivesAFailingPlugin);
  await quietly(testLoadInstalledSurvivesADeadEndpoint);
  await testLoadInstalledDoesNotLoadTheSamePluginTwice();
  await testInstalledPluginRegistersOnLoad();
  await testSettingsAreSeededAndScopedPerPlugin();
  await testSaveSettingsCanReplace();
  await testGetSettingsIsAnIsolatedCopy();
  await testPostJsonResolvesOnAFailureBody();
  await testNotificationIsPushedAtTheOperatorNotOnlyFiled();

  // Close the toasts the failure tests raised, whose auto-dismiss timers would
  // otherwise hold the process open for another six seconds.
  dismissToasts();
  await flushMicrotasks();

  console.log("frontend plugin tests passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
}).finally(() => {
  dismissToasts();      // a toast's auto-dismiss timer must not hold the process open
});
