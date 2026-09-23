"use strict";

/**
 * Frontend tests for the Setup -> Remote ID page (Corvus.setupRemoteId).
 *
 * Plain Node-runnable assertions in the same shape as tests/test_frontend_sik.js:
 * stub the globals the module touches, require the source, and assert on the
 * rendered DOM, the request spies, and the teardown behaviour.
 *
 * What is worth pinning here is narrow, because the rules live in the backend
 * and the option tables arrive with the payload. It is the four decisions the
 * page owns:
 *
 *   1. It renders with no vehicle. The identity is the operator's, filled in
 *      on the ground, and the page that refused to open without an aircraft
 *      would be useless exactly when it is needed.
 *   2. It saves what was typed even when its own live check is unhappy — the
 *      check is about the shape of a filing, and losing a half-typed serial
 *      number while the operator goes to read the label is worse than showing
 *      a warning under it.
 *   3. A patch names one field. POST /api/config merges, so a page that sent
 *      the whole identity per keystroke would resurrect a field the operator
 *      had just cleared from another tab.
 *   4. The identity stays editable while armed and the vehicle's parameters do
 *      not. Those are two different owners, and conflating them either locks an
 *      operator out of their own broadcast or invites a refused write.
 *
 * Run:
 *   node tests/test_frontend_remoteid.js
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

const notifications = [];
window.dispatchEvent = (ev) => { notifications.push(ev); };
window.localStorage = {
  getItem: () => null, setItem: () => {}, removeItem: () => {}, clear: () => {},
};
window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;

// The status poll is the one timer this page owns, so it is recorded rather
// than stubbed away: "destroy() stops it" is an assertion, not an assumption.
const timers = { started: 0, cleared: [] };
window.setInterval = () => { timers.started += 1; return timers.started; };
window.clearInterval = (id) => { timers.cleared.push(id); };

// ---------------------------------------------------------------------------
// Minimal DOM stub (mirrors tests/test_frontend_sik.js).
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
  e.replaceChild = (next, old) => {
    const i = e.children.indexOf(old);
    if (i >= 0) { e.children[i] = next; next.parentNode = e; old.parentNode = null; }
    return old;
  };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (type, cb) => { (e._listeners[type] = e._listeners[type] || []).push(cb); };
  e.removeEventListener = () => {};
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  return e;
}

/** Selector support: tag names, class chains, `[data-x="y"]` and bare `[data-x]`. */
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

function fire(el, type, detail) {
  const listeners = (el && el._listeners && el._listeners[type]) || [];
  const event = Object.assign({ preventDefault() {}, stopPropagation() {} }, detail || {});
  listeners.forEach((cb) => cb(event));
}

function card(root, section) {
  return root.querySelector(`.rid-card[data-section="${section}"]`);
}

/** The editable control for one identity field, by its key. */
function control(root, key) {
  const row = root.querySelector(`.rid-field[data-field="${key}"]`);
  if (!row) return null;
  return row.querySelector(".rid-input") || row.querySelector(".rid-select");
}

function rowStatus(root, key) {
  const row = root.querySelector(`.rid-field[data-field="${key}"]`);
  return row ? row.querySelector(".params-row-status") : null;
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
function identity(over = {}) {
  const base = {
    enabled: true, region: "eu",
    basic_id: { id_type: 1, ua_type: 2, uas_id: "ABCD3XYZ" },
    operator_id: { operator_id_type: 0, operator_id: "FIN87astrdge12k8" },
    self_id: { description_type: 0, description: "Survey" },
    system: {
      operator_location_type: 0, operator_latitude: 0, operator_longitude: 0,
      operator_altitude_geo: -1000, classification_type: 0,
      category_eu: 0, class_eu: 0, area_count: 1, area_radius: 0,
      area_ceiling: -1000, area_floor: -1000,
    },
  };
  Object.keys(over).forEach((key) => {
    base[key] = (over[key] && typeof over[key] === "object")
      ? Object.assign({}, base[key], over[key]) : over[key];
  });
  return base;
}

const SCHEMA = {
  id_types: [
    { value: 0, label: "None" },
    { value: 1, label: "Serial number (ANSI/CTA-2063-A)" },
    { value: 2, label: "CAA registration ID" },
  ],
  ua_types: [
    { value: 0, label: "Undeclared" },
    { value: 1, label: "Aeroplane" },
    { value: 2, label: "Helicopter or multirotor" },
  ],
  operator_id_types: [{ value: 0, label: "CAA-issued operator registration" }],
  description_types: [
    { value: 0, label: "Flight description" },
    { value: 1, label: "Emergency" },
  ],
  location_types: [
    { value: 0, label: "Take-off location" },
    { value: 1, label: "Live GNSS of the control station", disabled: true,
      reason: "Corvus has no GNSS receiver of its own" },
    { value: 2, label: "Fixed position" },
  ],
  classification_types: [
    { value: 0, label: "Undeclared" },
    { value: 1, label: "European Union" },
  ],
  categories_eu: [{ value: 0, label: "Undeclared" }, { value: 1, label: "Open" }],
  classes_eu: [{ value: 0, label: "Undeclared" }, { value: 2, label: "Class C1 (under 900 g)" }],
  regions: [
    { value: "eu", label: "European Union (EU 2019/945, EN 4709-002)" },
    { value: "faa", label: "United States (FAA Part 89)" },
  ],
  limits: { uas_id: 20, operator_id: 20, description: 23 },
  altitude_unknown: -1000,
};

function payload(over = {}) {
  return Object.assign({
    connected: true, received: 1,
    identity: identity(), configured: {},
    schema: SCHEMA, findings: [],
    status: { enabled: true, supported: true, broadcasting: true,
              last_sent_age: 0.4, error: "", arm_status: null },
    suggested_ua_type: 0,
    sections: [{
      id: "vehicle", title: "On the aircraft", kind: "fields",
      hint: "PX4 keeps one Remote ID setting of its own.",
      fields: [{
        param: "COM_ARM_ODID", label: "Remote ID arming check", kind: "enum",
        value: 1, options: [{ value: 0, label: "Disabled" },
                            { value: 2, label: "Required to arm" }],
      }],
    }],
  }, over);
}

// ---------------------------------------------------------------------------
// Fake telemetry, with the request spies the page is asserted through.
// ---------------------------------------------------------------------------
function makeFakeTelemetry(opts = {}) {
  const requests = [];
  let subCb = null;
  let unsubCalls = 0;
  let state = opts.state || { armed: false, connected: true, position: [0, 0] };
  const replies = Object.assign({ "/api/remoteid": payload() }, opts.replies);
  // The poll is a projection of the full document, exactly as the backend
  // builds it from the same stored identity.
  const statusReply = () => {
    const full = replies["/api/remoteid"] || {};
    return { identity: full.identity, findings: full.findings, status: full.status };
  };
  const rejects = Object.assign({}, opts.rejects);

  const telemetry = {
    requestJson(url, init) {
      const body = init && init.body ? JSON.parse(init.body) : null;
      requests.push({ url, method: (init && init.method) || "GET", body });
      if (rejects[url]) return Promise.reject(new Error(rejects[url]));
      if (url === "/api/remoteid/status") return Promise.resolve(statusReply());
      if (url === "/api/config") {
        // Mirrors what the backend returns: the merged, resolved identity.
        const merged = mergeDeep(replies["/api/remoteid"].identity, body.remote_id);
        replies["/api/remoteid"] = Object.assign({}, replies["/api/remoteid"],
          { identity: merged });
        return Promise.resolve({ ok: true, config: { remote_id: merged } });
      }
      return Promise.resolve(replies[url] !== undefined ? replies[url] : {});
    },
    postAction(url, body) {
      requests.push({ url, method: "POST", body });
      if (rejects[url]) return Promise.reject(new Error(rejects[url]));
      return Promise.resolve({ ok: true });
    },
    getState() { return state; },
    subscribe(cb) { subCb = cb; return () => { unsubCalls++; }; },
  };
  return {
    telemetry, requests,
    get unsubCalls() { return unsubCalls; },
    getSubCb() { return subCb; },
    setState(s) { state = s; },
    setReply(url, value) { replies[url] = value; },
    setReject(url, message) { rejects[url] = message; },
  };
}

function mergeDeep(base, patch) {
  const out = Object.assign({}, base);
  Object.keys(patch || {}).forEach((key) => {
    if (patch[key] && typeof patch[key] === "object" && !Array.isArray(patch[key])
        && out[key] && typeof out[key] === "object") {
      out[key] = Object.assign({}, out[key], patch[key]);
    } else {
      out[key] = patch[key];
    }
  });
  return out;
}

/** Everything the page POSTed to /api/config, newest last. */
function saves(fake) {
  return fake.requests.filter((r) => r.url === "/api/config").map((r) => r.body);
}

// ---------------------------------------------------------------------------
// Load the module under test, in index.html's order.
// ---------------------------------------------------------------------------
require("../src/js/ui.js");
require("../src/js/setup-shared.js");
require("../src/js/setup-remoteid.js");

async function mount(opts = {}) {
  timers.started = 0;
  timers.cleared.length = 0;
  notifications.length = 0;
  const fake = makeFakeTelemetry(opts);
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  const destroy = Corvus.setupRemoteId.render(container, () => {});
  await flush();
  await flush();
  return { fake, container, destroy };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
const tests = [];
function test(name, fn) { tests.push([name, fn]); }

test("the identity renders with no vehicle connected", async () => {
  const { container } = await mount({
    replies: { "/api/remoteid": payload({
      connected: false, sections: [], received: 0,
      status: { enabled: false, supported: false, broadcasting: false,
                last_sent_age: null, error: "", arm_status: null },
    }) },
  });

  assert.ok(card(container, "basic_id"),
    "the identity is the operator's and is filled in before the aircraft is out of its case");
  assert.equal(control(container, "uas_id").value, "ABCD3XYZ");
  assert.equal(container.querySelectorAll(".rid-vehicle-card").length, 0,
    "no vehicle, no vehicle parameters");
});

test("every identity card is on the page", async () => {
  const { container } = await mount();

  ["broadcast", "status", "checks", "basic_id", "operator_id", "self_id",
   "location", "classification"].forEach((section) => {
    assert.ok(card(container, section), `the ${section} card is present`);
  });
});

test("the master switch saves the broadcast flag on its own", async () => {
  const { fake, container } = await mount({
    replies: { "/api/remoteid": payload({ identity: identity({ enabled: false }) }) },
  });
  fire(container.querySelector(".rid-enable-toggle"), "click");
  await flush();

  assert.deepEqual(saves(fake)[0], { remote_id: { enabled: true } },
    "a patch names one field; the backend merges the rest");
});

test("a field save carries only that field, nested under its card", async () => {
  const { fake, container } = await mount();
  const input = control(container, "description");
  input.value = "Hall 7 survey";
  fire(input, "change");
  await flush();

  assert.deepEqual(saves(fake)[0],
    { remote_id: { self_id: { description: "Hall 7 survey" } } });
});

test("a malformed serial number is called out under the field and still saved", async () => {
  const { fake, container } = await mount();
  const input = control(container, "uas_id");
  input.value = "ABCD9XYZ";
  fire(input, "input");

  assert.match(rowStatus(container, "uas_id").textContent, /9 characters follow it/,
    "the mistake is named while the character is still under the cursor");

  fire(input, "change");
  await flush();
  assert.deepEqual(saves(fake)[0], { remote_id: { basic_id: { uas_id: "ABCD9XYZ" } } },
    "losing a half-typed serial while the operator reads the label is worse");
});

test("a serial number is upper-cased as it is typed", async () => {
  const { container } = await mount();
  const input = control(container, "uas_id");
  input.value = "abcd3xyz";
  fire(input, "input");

  assert.equal(input.value, "ABCD3XYZ",
    "CTA-2063-A is capitals only, and retyping it is not the operator's job");
});

test("the character counter says how much of the field is left", async () => {
  const { container } = await mount();
  const row = container.querySelector('.rid-field[data-field="description"]');

  assert.equal(row.querySelector(".rid-counter").textContent, "6/23");
});

test("the EU class fields appear only once a classification is declared", async () => {
  const undeclared = await mount();
  assert.equal(control(undeclared.container, "class_eu"), null,
    "a class mark under an undeclared scheme is a number nobody can read");

  const declared = await mount({
    replies: { "/api/remoteid": payload({
      identity: identity({ system: { classification_type: 1, category_eu: 1, class_eu: 2 } }),
    }) },
  });
  assert.ok(control(declared.container, "category_eu"));
  assert.ok(control(declared.container, "class_eu"));
  assert.ok(control(declared.container, "area_radius"));
});

test("the coordinates appear only for a fixed operator position", async () => {
  const takeoff = await mount();
  assert.equal(control(takeoff.container, "operator_latitude"), null,
    "on the take-off setting the aircraft supplies the position");

  const fixed = await mount({
    replies: { "/api/remoteid": payload({
      identity: identity({ system: { operator_location_type: 2 } }),
    }) },
  });
  assert.ok(control(fixed.container, "operator_latitude"));
  assert.ok(control(fixed.container, "operator_altitude_geo"));
});

test("an unknown altitude reads as empty rather than as sea level", async () => {
  const { fake, container } = await mount({
    replies: { "/api/remoteid": payload({
      identity: identity({ system: { operator_location_type: 2 } }),
    }) },
  });
  const input = control(container, "operator_altitude_geo");
  assert.equal(input.value, "", "-1000 is the standard's 'not known', not a height");

  input.value = "";
  fire(input, "change");
  await flush();
  assert.deepEqual(saves(fake)[0],
    { remote_id: { system: { operator_altitude_geo: -1000 } } },
    "clearing the field declares it unknown rather than claiming 0 m");
});

test("the position the aircraft reports can be taken in one press", async () => {
  const { fake, container } = await mount({
    state: { armed: false, connected: true, position: [48.0705, 11.6385] },
    replies: { "/api/remoteid": payload({
      identity: identity({ system: { operator_location_type: 2 } }),
    }) },
  });
  fire(container.querySelector(".rid-take-position"), "click");
  await flush();

  assert.deepEqual(saves(fake)[0], {
    remote_id: { system: { operator_latitude: 48.0705, operator_longitude: 11.6385 } },
  });
});

test("the position button is disabled while the aircraft has none", async () => {
  const { container } = await mount({
    state: { armed: false, connected: true, position: [0, 0] },
    replies: { "/api/remoteid": payload({
      identity: identity({ system: { operator_location_type: 2 } }),
    }) },
  });

  assert.equal(container.querySelector(".rid-take-position").disabled, true,
    "0,0 is the store's 'nothing yet' and also a real point in the Atlantic");
});

test("the option the station cannot honour is shown with its reason, not hidden", async () => {
  const { container } = await mount();
  const select = control(container, "operator_location_type");
  const live = select.children.filter((o) => o.value === "1")[0];

  assert.ok(live, "an operator who knows the standard must not think Corvus forgot it");
  assert.equal(live.disabled, true);
  assert.match(live.textContent, /no GNSS receiver/);
});

test("the findings the backend sent are listed with their level", async () => {
  const { container } = await mount({
    replies: { "/api/remoteid": payload({ findings: [
      { level: "error", field: "operator_id", text: "No operator registration number." },
      { level: "warning", field: "ua_type", text: "The aircraft type is undeclared." },
    ] }) },
  });
  const checks = card(container, "checks").querySelectorAll(".rid-check");

  assert.equal(checks.length, 2);
  assert.equal(checks[0].dataset.level, "error");
  assert.equal(checks[1].dataset.level, "warning");
  assert.match(allText(checks[0]), /No operator registration number/);
});

test("a complete filing says so instead of showing an empty list", async () => {
  const { container } = await mount();
  const checks = card(container, "checks");

  assert.equal(checks.querySelectorAll(".rid-check-ok").length, 1);
  assert.match(allText(checks), /asks for is filled in/);
});

test("the live broadcast status names the link, the send and the aircraft", async () => {
  const { container } = await mount({
    replies: { "/api/remoteid": payload({ status: {
      enabled: true, supported: true, broadcasting: false, last_sent_age: null,
      error: "", arm_status: { ok: false, status: 1, error: "no transmitter" },
    } }) },
  });
  const rows = card(container, "status").querySelectorAll(".rid-status-row");

  assert.deepEqual(rows.map((r) => r.dataset.status), ["link", "sending", "arm"]);
  assert.match(allText(rows[1]), /nothing has gone out yet/);
  assert.match(allText(rows[2]), /no transmitter/);
});

test("a vehicle that never reports an arm status is not read as a clearance", async () => {
  const { container } = await mount();
  const arm = card(container, "status").querySelector('[data-status="arm"]');

  assert.match(allText(arm), /has not reported/,
    "PX4 does not send the message at all; silence is not 'good to arm'");
});

test("a MAVLink 1 link says why nothing can be broadcast", async () => {
  const { container } = await mount({
    replies: { "/api/remoteid": payload({ status: {
      enabled: true, supported: false, broadcasting: false, last_sent_age: null,
      error: "this link is MAVLink 1 — the Remote ID messages are MAVLink 2 only",
      arm_status: null,
    } }) },
  });
  const link = card(container, "status").querySelector('[data-status="link"]');

  assert.match(allText(link), /MAVLink 2/);
});

test("the aircraft's own parameters render from the schema the backend sent", async () => {
  const { container } = await mount();
  const vehicle = container.querySelector(".rid-vehicle-card");

  assert.ok(vehicle, "the vehicle half is a schema-driven form like every other page");
  assert.ok(vehicle.querySelector('[data-param="COM_ARM_ODID"]'));
});

test("the vehicle card is the only place a parameter name appears", async () => {
  const { container } = await mount();
  const identityHalf = ["basic_id", "operator_id", "self_id", "location",
                        "classification"].map((s) => card(container, s));

  identityHalf.forEach((cardEl) => {
    assert.equal(cardEl.querySelectorAll("[data-param]").length, 0,
      "a page that thought a serial number was a parameter would offer to "
      + "write a registration number to an autopilot");
  });
});

test("Check values reads back the vehicle's parameters and never the identity", async () => {
  const { fake, container } = await mount({
    replies: { "/api/remoteid?fresh=1": payload() },
  });
  const check = container.querySelector(".rid-check");
  assert.ok(check && !check.disabled, "offered once the vehicle has answered");

  const param = container.querySelector(".rid-vehicle-card")
    .querySelector('.pform-select[data-param="COM_ARM_ODID"]');
  param.value = "2";
  fire(param, "change");
  await flush();

  fake.telemetry.postAction = (url, body) => {
    fake.requests.push({ url, method: "POST", body });
    if (url !== "/api/params/verify") return Promise.resolve({ ok: true });
    return Promise.resolve({
      ok: true, values: { COM_ARM_ODID: 2 }, missing: [],
      results: [{ name: "COM_ARM_ODID", wanted: 2, before: 1, after: 2,
        rewritten: false, ok: true, error: "" }],
    });
  };
  fire(container.querySelector(".rid-check"), "click");
  for (let i = 0; i < 4; i += 1) await flush();

  const verify = fake.requests.filter((r) => r.url === "/api/params/verify")[0];
  assert.deepEqual(verify.body, {
    params: [{ name: "COM_ARM_ODID", value: 2 }], names: ["COM_ARM_ODID"],
  }, "the serial number and the operator ID are Corvus's, not the vehicle's");
  assert.ok(fake.requests.some((r) => r.url === "/api/remoteid?fresh=1"),
    "the redraw asks the vehicle");
  assert.ok(container.querySelector(".rid-check-card"), "the outcome is shown");
});

test("arming freezes the vehicle parameters and leaves the identity alone", async () => {
  const { fake, container } = await mount();
  fake.getSubCb()({ armed: true, connected: true, position: [0, 0] });

  const param = container.querySelector(".rid-vehicle-card")
    .querySelector('.pform-select[data-param="COM_ARM_ODID"]');
  assert.equal(param.disabled, true, "the autopilot refuses these writes while armed");
  assert.equal(control(container, "uas_id").disabled, false,
    "the identity is Corvus's, and a wrong serial on a live aircraft has to be fixable");
  assert.equal(container.querySelector(".params-banner").hidden, false);
});

test("a refused save is reported on the row and as a notification", async () => {
  const { container } = await mount();
  Corvus.telemetry.requestJson = () => Promise.reject(new Error("disk full"));
  const input = control(container, "description");
  input.value = "Hall 7";
  fire(input, "change");
  await flush();

  assert.equal(rowStatus(container, "description").textContent, "disk full");
  assert.ok(notifications.some((e) => /Could not save the Remote ID/.test(e.detail.message)));
});

test("the region only changes what is checked, never what is sent", async () => {
  const { fake, container } = await mount();
  const select = control(container, "region");
  select.value = "faa";
  fire(select, "change");
  await flush();

  assert.deepEqual(saves(fake)[0], { remote_id: { region: "faa" } });
  assert.ok(!saves(fake).some((s) => "basic_id" in s.remote_id),
    "picking a region must not rewrite the identity");
});

test("the poll never re-reads the vehicle's parameters", async () => {
  const { fake, container } = await mount();
  const input = control(container, "description");
  input.value = "Hall 7";
  fire(input, "change");
  await flush();
  await flush();

  const reads = fake.requests.filter((r) => r.url === "/api/remoteid");
  assert.equal(reads.length, 1,
    "a parameter read per save would stack up against the browser's "
    + "six-connection limit and starve the rest of the page");
  assert.ok(fake.requests.some((r) => r.url === "/api/remoteid/status"),
    "the checks are refreshed from the endpoint that touches no MAVLink");
});

test("destroy stops the status poll and releases the subscription", async () => {
  const { fake, destroy } = await mount();
  assert.equal(timers.started, 1, "one poll, not one per card");

  destroy();

  assert.equal(fake.unsubCalls, 1);
  assert.deepEqual(timers.cleared, [1]);
});

test("a late answer never writes to a page that is gone", async () => {
  timers.started = 0;
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  // The shell paints synchronously; what must not land is the *loaded*
  // identity, which arrives a tick later.
  const destroy = Corvus.setupRemoteId.render(container, () => {});
  assert.equal(control(container, "uas_id").value, "");

  destroy();
  await flush();
  await flush();

  assert.equal(control(container, "uas_id").value, "",
    "the fetch outlived the page; its result must not land in the DOM");
});

// ---------------------------------------------------------------------------
(async () => {
  let failed = 0;
  for (const [name, fn] of tests) {
    try {
      await fn();
      console.log(`  ok  ${name}`);
    } catch (err) {
      failed++;
      console.error(`  FAIL ${name}\n       ${err.message}`);
    }
  }
  if (failed) {
    console.error(`\n${failed} of ${tests.length} failed`);
    process.exit(1);
  }
  console.log(`\n${tests.length} passed — Setup > Remote ID`);
})();
