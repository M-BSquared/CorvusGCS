"use strict";

/**
 * Frontend tests for the Setup -> RTK GPS page (Corvus.setupRtk).
 *
 * Plain Node-runnable assertions in the same shape as tests/test_frontend_sik.js:
 * stub the globals the module touches, require the source, and assert on the
 * rendered DOM, the request spies, and the teardown behaviour.
 *
 * This page shows a chain with three links in it — a base station, a radio
 * link, an aircraft — and almost every real RTK complaint is one of them being
 * broken while the other two are fine. So what is asserted here is that the
 * page never collapses that into one verdict: a base that is surveying, a base
 * that is streaming into a dead link, and a base that is working are three
 * different screens.
 *
 * The second thing asserted is the NTRIP password, which is the only secret
 * this page can hold. The status endpoint never sends it, so an empty box must
 * mean "keep the stored one" rather than "clear it" — a round trip that blanked
 * it on every save would be a page that silently breaks its own connection.
 *
 * Run:
 *   node tests/test_frontend_rtk.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals so the module loads and runs in Node.
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

const windowListeners = {};
window.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
window.removeEventListener = (t, cb) => {
  const list = windowListeners[t] || [];
  const i = list.indexOf(cb);
  if (i >= 0) list.splice(i, 1);
};

global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};

window.matchMedia = (query) => ({
  matches: false, media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});

window.dispatchEvent = () => {};
window.localStorage = {
  getItem: () => null, setItem: () => {}, removeItem: () => {}, clear: () => {},
};
window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;
window.setInterval = () => 1;
window.clearInterval = () => {};

// ---------------------------------------------------------------------------
// Minimal DOM stub (mirrors tests/test_frontend_safety.js).
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {}, style: {},
    type: "", hidden: false, disabled: false, value: "", id: "",
    _attrs: {}, _listeners: {}, _isEl: true,
  };
  e.style.setProperty = (k, v) => { e.style[k] = String(v); };
  e.style.removeProperty = (k) => { delete e.style[k]; };
  let _html = "";
  Object.defineProperty(e, "innerHTML", {
    get() { return _html; },
    set(v) { _html = String(v); if (_html === "") e.children.length = 0; },
  });
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) { const has = e.classList.contains(c); const next = force === undefined ? !has : !!force; if (next) e.classList.add(c); else e.classList.remove(c); return next; },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => { c.parentNode = e; e.children.push(c); return c; };
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; return c; };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (type, cb) => { (e._listeners[type] = e._listeners[type] || []).push(cb); };
  e.removeEventListener = () => {};
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  return e;
}

/**
 * Selector support: tag names, class chains, `[data-x="y"]` and bare `[data-x]`.
 *
 * The bare form is not an extra: the page gates its controls with
 * `querySelectorAll("[data-needs-gate]")`, so a stub that could not parse it
 * would report every control as still enabled while armed — the one assertion
 * on this page that most needs to be true. Anything richer than this is not
 * worth a parser here.
 */
function querySel(children, sel) {
  const raw = String(sel || "");
  const attrs = [];
  const base = raw.replace(/\[([\w-]+)(?:=["']?([^\]"']*)["']?)?\]/g, (_m, key, value) => {
    attrs.push([key, value]);
    return "";
  });
  const wantTag = base && base[0] !== ".";
  const classes = base ? base.split(".").filter(Boolean) : [];
  const out = [];
  function matches(el) {
    if (wantTag && el.tagName !== base.toUpperCase()) return false;
    if (!wantTag && !classes.every((c) => el.className.split(/\s+/).includes(c))) return false;
    return attrs.every(([key, value]) => {
      const camel = key.startsWith("data-")
        ? key.slice(5).replace(/-([a-z])/g, (_m, c) => c.toUpperCase())
        : null;
      const actual = camel !== null ? el.dataset[camel] : el.getAttribute(key);
      if (value === undefined) return actual !== undefined && actual !== null;
      return String(actual) === value;
    });
  }
  function walk(list) {
    for (const c of list) {
      if (!c || !c._isEl) continue;
      if (matches(c)) out.push(c);
      if (c.children) walk(c.children);
    }
  }
  walk(children);
  return out;
}

global.document = {
  createElement: makeEl,
  createElementNS: (_ns, tag) => makeEl(tag),
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
};

// Helpers ---------------------------------------------------------------------
function flush() { return new Promise((r) => setTimeout(r, 0)); }

function findByDataset(root, key, value) {
  const out = [];
  function walk(list) {
    for (const e of list) {
      if (!e || !e._isEl) continue;
      if (e.dataset && e.dataset[key] === value) out.push(e);
      if (e.children) walk(e.children);
    }
  }
  walk(root.children || []);
  return out;
}

function fire(el, type, detail) {
  const listeners = (el && el._listeners && el._listeners[type]) || [];
  const event = Object.assign({ preventDefault() {}, stopPropagation() {} }, detail || {});
  listeners.forEach((cb) => cb(event));
}

function action(root, name) { return findByDataset(root, "action", name)[0]; }

/** The editable control for one register in one column. */
function control(root, side, name) {
  const field = root.querySelector(`.sik-field[data-side="${side}"][data-name="${name}"]`);
  return field ? field.querySelector("[data-field]") : null;
}

function allText(root) {
  const parts = [];
  function walk(list) {
    for (const e of list) {
      if (!e || !e._isEl) continue;
      if (e.textContent) parts.push(e.textContent);
      if (e.children) walk(e.children);
    }
  }
  walk(root.children || []);
  return parts.join(" | ");
}

// ---------------------------------------------------------------------------
// Backend payloads
// ---------------------------------------------------------------------------

function defaultSettings(over = {}) {
  return Object.assign({
    enabled: true,
    source: "usb",
    device: "",
    baud: 0,
    mode: "survey",
    survey_accuracy: 2.0,
    survey_duration: 180,
    msm7: false,
    fixed: { latitude: 0, longitude: 0, altitude: 0, accuracy: 0.1 },
    ntrip: { host: "", port: 2101, mountpoint: "", username: "", password: "",
             has_password: false },
  }, over);
}

function statusPayload(over = {}) {
  return Object.assign({
    enabled: true,
    state: "active",
    message: "The survey is complete and corrections are flowing",
    error: "", warning: "",
    source: "usb", mode: "survey",
    device: "/dev/ttyACM9", baud: 38400,
    receiver: { model: "ZED-F9P", protocol: 27.11, software: "EXT CORE 1.00" },
    receiver_label: "u-blox ZED-F9P · EXT CORE 1.00 · protocol 27.11",
    survey: { duration: 200, accuracy: 1.2, observations: 900, valid: true, active: false },
    survey_progress: 100,
    survey_target: { accuracy: 2.0, duration: 180 },
    frames: 412, frame_bytes: 40000, crc_errors: 0,
    source_age: 0.4, uptime: 240,
    messages: { 1005: 40, 1074: 200, 1084: 172 },
    injected: { bytes: 39000, messages: 400, dropped: 0, age: 0.4 },
    link_ready: true,
    vehicle: { fix: "RTK_FIXED", satellites: 24, hdop: 0.6 },
    ports: [{ device: "/dev/ttyACM9", description: "u-blox GNSS receiver", hwid: "" }],
    settings: defaultSettings(),
    defaults: defaultSettings(),
  }, over);
}

function makeFakeTelemetry() {
  const gets = [];
  const posts = [];
  const replies = {};
  const rejects = {};
  let state = { armed: false };
  const telemetry = {
    requestJson(url) {
      gets.push(url);
      if (rejects[url]) return Promise.reject(new Error(rejects[url]));
      return Promise.resolve(replies[url] !== undefined ? replies[url] : statusPayload());
    },
    postAction(url, payload) {
      posts.push({ url, payload });
      if (rejects[url]) return Promise.reject(new Error(rejects[url]));
      return Promise.resolve(replies[url] !== undefined ? replies[url] : { ok: true });
    },
    getState() { return state; },
    subscribe() { return () => {}; },
  };
  return {
    telemetry, gets, posts,
    setReply(url, value) { replies[url] = value; },
    setReject(url, message) { rejects[url] = message; },
  };
}

// ---------------------------------------------------------------------------
// Load the module under test, in index.html's order.
// ---------------------------------------------------------------------------
require("../src/js/ui.js");
require("../src/js/setup-shared.js");
require("../src/js/setup-rtk.js");

async function mount(status) {
  const fake = makeFakeTelemetry();
  if (status !== undefined) fake.setReply("/api/rtk/status", status);
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  const destroy = Corvus.setupRtk.render(container, () => {});
  await flush();
  return { fake, container, destroy };
}

function role(root, name) { return findByDataset(root, "role", name)[0]; }
function rowValue(root, key) { return findByDataset(root, "infoKey", key)[0]; }

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
const tests = [];
function test(name, fn) { tests.push([name, fn]); }

test("a working base reports every link in the chain, not one verdict", async () => {
  const { container } = await mount();

  assert.equal(role(container, "state").textContent, "Correcting");
  assert.ok(role(container, "state").className.includes("ok"));
  assert.match(rowValue(container, "receiver").textContent, /ZED-F9P/);
  assert.match(rowValue(container, "port").textContent, /ttyACM9 at 38400 baud/);
  assert.match(rowValue(container, "corrections").textContent, /412 messages/);
  assert.match(rowValue(container, "injected").textContent, /400 messages/);
  assert.match(rowValue(container, "fix").textContent, /RTK fixed/);
  assert.match(rowValue(container, "fix").textContent, /24 satellites/);
});

test("a base streaming into a dead link says so instead of looking healthy", async () => {
  /* The failure this row exists for: the base is perfect, the page is green,
     and nothing is reaching the aircraft because the radio is not up. */
  const { container } = await mount(statusPayload({
    link_ready: false,
    injected: { bytes: 0, messages: 0, dropped: 0, age: null },
  }));

  assert.match(rowValue(container, "injected").textContent, /waiting for the aircraft link/);
  assert.match(rowValue(container, "corrections").textContent, /412 messages/,
    "the base is still producing, and the page has to keep saying so");
});

test("a running survey shows what is still holding it up", async () => {
  /* A bar that has been at 30% for four minutes is only readable if it says
     whether it is the clock or the sky. */
  const { container } = await mount(statusPayload({
    state: "surveying",
    message: "Surveying: the base is learning where it is",
    survey: { duration: 45, accuracy: 6.5, observations: 45, valid: false, active: true },
    survey_progress: 25,
  }));

  assert.equal(role(container, "state").textContent, "Surveying");
  const text = role(container, "survey-text").textContent;
  assert.match(text, /25%/);
  assert.match(text, /45 s of 180 s/);
  assert.match(text, /needs 2 m/);
  assert.equal(role(container, "progress-fill").style.width, "25%");
});

test("a finished survey reports the accuracy it converged on", async () => {
  const { container } = await mount();
  assert.match(role(container, "survey-text").textContent,
    /Survey complete: the base knows its position to 1.2 m/);
});

test("nothing found yet is a state, not an error", async () => {
  const { container } = await mount(statusPayload({
    state: "searching",
    message: "Looking for a base station. Plug an RTK GNSS receiver into this computer",
    receiver: null, receiver_label: "", device: "", baud: 0,
    survey: null, survey_progress: 0, frames: 0,
    injected: { bytes: 0, messages: 0, dropped: 0, age: null },
    vehicle: { fix: "3D_FIX", satellites: 11, hdop: 1.2 },
  }));

  assert.equal(role(container, "state").textContent, "Looking for a base");
  assert.ok(!role(container, "state").className.includes("err"),
    "no base plugged in is not a fault");
  assert.equal(role(container, "warning").hidden, true);
  assert.equal(role(container, "survey-text").parentNode.hidden, true,
    "an empty progress bar for a survey that is not running is noise");
});

test("an error is shown as an error", async () => {
  const { container } = await mount(statusPayload({
    state: "error",
    message: "/dev/ttyACM9 produced no corrections",
    error: "/dev/ttyACM9 produced no corrections. Check that the base station has power",
  }));

  assert.ok(role(container, "state").className.includes("err"));
  const warning = role(container, "warning");
  assert.equal(warning.hidden, false);
  assert.match(warning.textContent, /has power/);
});

test("the survey form is what is shown for a survey, and the coordinates for a fixed base", async () => {
  const { container } = await mount();
  assert.equal(role(container, "survey-block").hidden, false);
  assert.equal(role(container, "fixed-block").hidden, true);
  assert.equal(role(container, "ntrip-block").hidden, true);

  const mode = role(container, "mode");
  mode.value = "fixed";
  fire(mode, "change");
  assert.equal(role(container, "survey-block").hidden, true);
  assert.equal(role(container, "fixed-block").hidden, false);
});

test("choosing NTRIP swaps the whole block rather than adding to it", async () => {
  const { container } = await mount();
  const source = role(container, "source");
  source.value = "ntrip";
  fire(source, "change");

  assert.equal(role(container, "usb-block").hidden, true);
  assert.equal(role(container, "ntrip-block").hidden, false);
});

test("a survey setting is saved as the number that was typed", async () => {
  const { fake, container } = await mount();
  const accuracy = role(container, "survey-accuracy");
  accuracy.value = "0.5";
  fire(accuracy, "change");
  await flush();

  const post = fake.posts.find((p) => p.url === "/api/rtk/settings");
  assert.ok(post, "the change was never sent");
  assert.equal(post.payload.survey_accuracy, 0.5);
  assert.equal(post.payload.survey_duration, 180);
  assert.equal(post.payload.source, "usb");
});

test("an empty NTRIP password box means keep the stored one", async () => {
  /* The status endpoint never sends the real password, so a form round trip
     that posted the empty box as the new value would clear it on every save. */
  const { fake, container } = await mount(statusPayload({
    source: "ntrip",
    settings: defaultSettings({
      source: "ntrip",
      ntrip: { host: "caster.example", port: 2101, mountpoint: "MSM4",
               username: "me", password: "", has_password: true },
    }),
  }));

  const password = role(container, "ntrip-password");
  assert.equal(password.value, "", "the real password must never reach the browser");
  assert.equal(password.placeholder, "stored",
    "an operator has to be able to tell a stored password from no password");

  fire(role(container, "ntrip-host"), "change");
  await flush();
  const post = fake.posts.find((p) => p.url === "/api/rtk/settings");
  assert.equal(post.payload.ntrip.password, "",
    "empty is the signal the backend reads as 'keep the stored one'");
  assert.equal(post.payload.ntrip.host, "caster.example");
});

test("turning RTK off is a save like any other", async () => {
  const { fake, container } = await mount();
  const toggle = role(container, "enabled");
  fire(toggle, "click");
  await flush();

  const post = fake.posts.find((p) => p.url === "/api/rtk/settings");
  assert.equal(post.payload.enabled, false);
});

test("survey again is offered only when there is a survey to redo", async () => {
  const { container } = await mount();
  assert.equal(findByDataset(container, "action", "restart")[0].hidden, false);

  const ntrip = await mount(statusPayload({
    source: "ntrip", receiver: null, receiver_label: "", survey: null,
  }));
  assert.equal(findByDataset(ntrip.container, "action", "restart")[0].hidden, true,
    "an NTRIP caster has no survey of its own to restart");
});

test("survey again asks the backend rather than the receiver", async () => {
  const { fake, container } = await mount();
  fire(findByDataset(container, "action", "restart")[0], "click");
  await flush();

  assert.ok(fake.posts.some((p) => p.url === "/api/rtk/restart"));
});

test("a status read that fails does not leave the page claiming to be correcting", async () => {
  const fake = makeFakeTelemetry();
  fake.setReject("/api/rtk/status", "network down");
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  const destroy = Corvus.setupRtk.render(container, () => {});
  await flush();

  assert.equal(role(container, "state").textContent, "Problem");
  assert.match(role(container, "message").textContent, /Could not read/);
  destroy();
});

test("destroy stops the poll", async () => {
  /* The page polls once a second for as long as it is open; one that kept
     polling after the operator left it would do so for the rest of the
     session, against a DOM it no longer owns. */
  const intervals = [];
  const clears = [];
  const realSet = window.setInterval;
  const realClear = window.clearInterval;
  window.setInterval = (fn, ms) => { intervals.push(ms); return 42; };
  window.clearInterval = (id) => { clears.push(id); };
  global.setInterval = window.setInterval;
  global.clearInterval = window.clearInterval;
  try {
    const { destroy } = await mount();
    assert.deepEqual(intervals, [1000]);
    destroy();
    assert.deepEqual(clears, [42]);
    destroy();
    assert.deepEqual(clears, [42], "a second destroy must not clear a timer twice");
  } finally {
    window.setInterval = realSet;
    window.clearInterval = realClear;
    global.setInterval = realSet;
    global.clearInterval = realClear;
  }
});

test("a reply landing after destroy never touches the page again", async () => {
  const { container, destroy } = await mount();
  const before = role(container, "state").textContent;
  destroy();
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  await flush();
  await flush();
  assert.equal(role(container, "state").textContent, before);
});

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------
(async function run() {
  for (const [name, fn] of tests) {
    try {
      await fn();
    } catch (err) {
      console.error(`FAIL: ${name}`);
      console.error(err);
      process.exit(1);
    }
  }
  console.log(`frontend RTK tests passed (${tests.length} assertions groups)`);
})();
