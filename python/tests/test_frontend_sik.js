"use strict";

/**
 * Frontend tests for the Setup -> Telemetry Radio page (Corvus.setupSik).
 *
 * Plain Node-runnable assertions in the same shape as tests/test_frontend_safety.js:
 * stub the globals the module touches, require the source, and assert on the
 * rendered DOM, the request spies, and the teardown behaviour.
 *
 * What is worth asserting here is narrower than on the other setup pages,
 * because this page deliberately does almost nothing on its own: the register
 * table, the labels, the option lists and every refusal message come from the
 * backend. So these tests are about the three decisions the page *does* own —
 * what it sends, when it refuses to send anything, and whether an edit is
 * visible as an edit before it is written.
 *
 * Run:
 *   node tests/test_frontend_sik.js
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
function statusPayload(over = {}) {
  return Object.assign({
    ports: [
      { device: "/dev/ttyUSB0", description: "FT232R", kind: "sik", is_link: true },
      { device: "/dev/ttyUSB9", description: "spare", kind: "unknown", is_link: false },
    ],
    link_device: "/dev/ttyUSB0", link_baud: 57600,
    transport: "sik", armed: false, busy: false,
    can_configure: true, blocked_reason: "",
    default_baud: 57600, bauds: [57600, 115200, 9600],
    schema: { registers: [], must_match: ["NETID"], bauds: [57600], default_baud: 57600 },
  }, over);
}

function field(name, register, value, over = {}) {
  return Object.assign({
    name, register, value, label: name, kind: "number",
    advanced: false, must_match: false, read_only: false,
    hint: `what ${name} does`,
  }, over);
}

function radio(values = {}) {
  const netid = values.NETID == null ? 25 : values.NETID;
  const air = values.AIR_SPEED == null ? 64 : values.AIR_SPEED;
  return {
    version: values.version || "SiK 2.0 on HM-TRP",
    board: "HM-TRP",
    fields: [
      field("FORMAT", 0, 25, { read_only: true, advanced: true }),
      field("AIR_SPEED", 2, air, {
        kind: "enum", must_match: true, label: "Air data rate",
        options: [{ value: 64, label: "64 kbps" }, { value: 128, label: "128 kbps" }],
      }),
      field("NETID", 3, netid, { must_match: true, label: "Network ID", min: 0, max: 499 }),
      field("TXPOWER", 4, 20, { label: "Transmit power" }),
      field("DUTY_CYCLE", 11, 100, { advanced: true, label: "Duty cycle", unit: "%" }),
    ],
    values: { FORMAT: 25, AIR_SPEED: air, NETID: netid, TXPOWER: 20, DUTY_CYCLE: 100 },
  };
}

function loadPayload(over = {}) {
  return Object.assign({
    ok: true, device: "/dev/ttyUSB0", baud: 57600,
    local: radio(), remote: radio({ NETID: 30 }),
    remote_reachable: true,
    mismatches: [{ name: "NETID", label: "Network ID", local: 25, remote: 30 }],
    link: {
      local_rssi: 208, remote_rssi: 205, local_noise: 41, remote_noise: 38,
      local_dbm: -17.5, remote_dbm: -19.1,
      local_margin_db: 83.5, remote_margin_db: 83.5,
    },
  }, over);
}

// ---------------------------------------------------------------------------
// Fake telemetry, with the request spies the page is asserted through.
// ---------------------------------------------------------------------------
function makeFakeTelemetry(opts = {}) {
  const gets = [];
  const posts = [];
  let subCb = null;
  let unsubCalls = 0;
  let state = opts.state || { armed: false, connected: true };
  const replies = Object.assign({}, opts.replies);
  const rejects = Object.assign({}, opts.rejects);

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
    subscribe(cb) { subCb = cb; return () => { unsubCalls++; }; },
  };
  return {
    telemetry, gets, posts,
    get unsubCalls() { return unsubCalls; },
    getSubCb() { return subCb; },
    setState(s) { state = s; },
    setReply(url, value) { replies[url] = value; },
    setReject(url, message) { rejects[url] = message; },
  };
}

// ---------------------------------------------------------------------------
// Load the module under test, in index.html's order.
// ---------------------------------------------------------------------------
require("../src/js/ui.js");
require("../src/js/setup-shared.js");
require("../src/js/setup-sik.js");

/** Render the page with a loaded radio pair already on screen. */
async function mountLoaded(opts = {}) {
  const fake = makeFakeTelemetry(opts);
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  if (opts.loadReply !== undefined) fake.setReply("/api/sik/load", opts.loadReply);
  else fake.setReply("/api/sik/load", loadPayload());
  const destroy = Corvus.setupSik.render(container, () => {});
  await flush();
  if (opts.skipLoad !== true) {
    fire(action(container, "load"), "click");
    await flush();
    await flush();
  }
  return { fake, container, destroy };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
const tests = [];
function test(name, fn) { tests.push([name, fn]); }

test("the port list preselects the link's own port and says which one it is", async () => {
  const { container } = await mountLoaded({ skipLoad: true });
  const port = findByDataset(container, "role", "port")[0];

  assert.equal(port.value, "/dev/ttyUSB0",
    "the radio an operator wants is normally the one they are already talking to");
  assert.equal(port.disabled, false);
  const labels = port.children.map((o) => o.textContent);
  assert.ok(labels.some((l) => l.includes("/dev/ttyUSB0") && l.includes("live link")),
    "the port that costs a telemetry interruption has to say so before it is used");
  assert.ok(labels.some((l) => l.includes("/dev/ttyUSB9")));
});

test("nothing is shown until a radio has actually been read", async () => {
  const { container } = await mountLoaded({ skipLoad: true });

  assert.equal(container.querySelectorAll(".sik-radio-card").length, 0,
    "a form pre-filled with defaults would invite saving a configuration nobody saw");
});

test("a load sends the chosen port, baud and scope", async () => {
  const { fake, container } = await mountLoaded({ skipLoad: true });
  findByDataset(container, "role", "scope")[0].value = "0";
  fire(action(container, "load"), "click");
  await flush();

  assert.deepEqual(fake.posts[0], {
    url: "/api/sik/load",
    payload: { device: "/dev/ttyUSB0", baud: 57600, remote: false },
  });
});

test("both radios are drawn side by side with their fields", async () => {
  const { container } = await mountLoaded();

  const cards = container.querySelectorAll(".sik-radio-card");
  assert.equal(cards.length, 2, "a pair that disagrees is invisible unless both ends show");
  assert.equal(control(container, "local", "NETID").value, "25");
  assert.equal(control(container, "remote", "NETID").value, "30");
  assert.equal(control(container, "local", "AIR_SPEED").tagName, "SELECT");
  assert.equal(control(container, "local", "NETID").tagName, "INPUT");
});

test("the read-only EEPROM format is shown but cannot be edited", async () => {
  const { container } = await mountLoaded();
  assert.equal(control(container, "local", "FORMAT").disabled, true);
});

test("advanced settings are hidden until asked for", async () => {
  const { container } = await mountLoaded();
  const duty = container.querySelector('.sik-field[data-side="local"][data-name="DUTY_CYCLE"]');

  assert.equal(duty.hidden, true);
  fire(action(container, "advanced"), "click");
  assert.equal(
    container.querySelector('.sik-field[data-side="local"][data-name="DUTY_CYCLE"]').hidden,
    false);
});

test("the mismatch card names the setting and both of its values", async () => {
  const { container } = await mountLoaded();

  const items = container.querySelectorAll(".sik-mismatch-list").length
    ? container.querySelector(".sik-mismatch-list").children : [];
  assert.equal(items.length, 1);
  assert.ok(items[0].textContent.includes("Network ID"));
  assert.ok(items[0].textContent.includes("25") && items[0].textContent.includes("30"));
});

test("a matching pair shows no mismatch card at all", async () => {
  const { container } = await mountLoaded({
    loadReply: loadPayload({ remote: radio(), mismatches: [] }),
  });
  assert.equal(container.querySelectorAll(".sik-mismatch-card").length, 0);
});

test("copying to remote stages the local value without writing anything", async () => {
  const { fake, container } = await mountLoaded();
  const before = fake.posts.length;

  fire(action(container, "copy"), "click");

  assert.equal(control(container, "remote", "NETID").value, "25");
  assert.equal(fake.posts.length, before, "copy stages, it does not write");
  const save = action(container, "save");
  assert.equal(save.dataset.pending, "1");
});

test("save sends only what changed, and the remote alongside it", async () => {
  const { fake, container } = await mountLoaded();
  fire(action(container, "copy"), "click");
  const power = control(container, "local", "TXPOWER");
  power.value = "14";
  fire(power, "change");

  fire(action(container, "save"), "click");
  await flush();

  const save = fake.posts.find((p) => p.url === "/api/sik/save");
  assert.deepEqual(save.payload, {
    device: "/dev/ttyUSB0", baud: 57600,
    remote: { NETID: 25 },
    local: { TXPOWER: 14 },
  });
  assert.ok(!("AIR_SPEED" in save.payload.local),
    "an untouched register must not be rewritten — every write costs a reboot");
});

test("typing the original value back removes it from the batch", async () => {
  const { fake, container } = await mountLoaded();
  const netid = control(container, "local", "NETID");
  netid.value = "42";
  fire(netid, "change");
  assert.equal(action(container, "save").dataset.pending, "1");

  netid.value = "25";
  fire(netid, "change");
  assert.equal(action(container, "save").dataset.pending, "0");

  fire(action(container, "save"), "click");
  await flush();
  assert.equal(fake.posts.filter((p) => p.url === "/api/sik/save").length, 0);
});

test("an edited field is marked as edited before it is written", async () => {
  const { container } = await mountLoaded();
  const netid = control(container, "local", "NETID");
  netid.value = "42";
  fire(netid, "change");

  const wrap = container.querySelector('.sik-field[data-side="local"][data-name="NETID"]');
  assert.ok(wrap.className.includes("sik-dirty"),
    "between staging and saving, the value on screen is not the value in the radio");
});

test("the radios are re-read after a save rather than trusted", async () => {
  const { fake, container } = await mountLoaded();
  const netid = control(container, "local", "NETID");
  netid.value = "42";
  fire(netid, "change");

  fire(action(container, "save"), "click");
  await flush();
  await flush();
  await flush();

  const urls = fake.posts.map((p) => p.url);
  assert.deepEqual(urls, ["/api/sik/load", "/api/sik/save", "/api/sik/load"],
    "the radio rounds air rates and powers up, so what it holds is not what was sent");
});

test("a remote that did not answer is explained, not left blank", async () => {
  const { container } = await mountLoaded({
    loadReply: loadPayload({ remote: null, remote_reachable: false, mismatches: [] }),
  });

  assert.equal(container.querySelectorAll(".sik-radio-unreachable").length, 1);
  assert.ok(allText(container).includes("did not answer"));
});

test("radios on different firmware are called out", async () => {
  const { container } = await mountLoaded({
    loadReply: loadPayload({ remote: radio({ version: "SiK 1.9 on HM-TRP" }), mismatches: [] }),
  });

  assert.ok(allText(container).includes("different firmware"),
    "a pair has to match on firmware version as well as on settings");
});

test("the link report is shown when the radio gave one", async () => {
  const { container } = await mountLoaded();
  const text = allText(container);

  assert.ok(text.includes("208"), "local signal");
  assert.ok(text.includes("83.5"), "fade margin — the number that answers 'will this reach'");
});

test("a backend refusal is shown with the backend's own wording", async () => {
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  fake.setReject("/api/sik/load", "the remote radio is not answering");
  const container = makeEl("div");
  Corvus.setupSik.render(container, () => {});
  await flush();

  fire(action(container, "load"), "click");
  await flush();
  await flush();

  const msg = container.querySelector(".sik-status");
  assert.equal(msg.textContent, "the remote radio is not answering");
  assert.ok(msg.className.includes("err"));
});

test("arming closes the page immediately, without waiting for a poll", async () => {
  const { fake, container } = await mountLoaded();

  fake.setState({ armed: true, connected: true });
  fake.getSubCb()({ armed: true, connected: true });

  assert.equal(action(container, "load").disabled, true);
  assert.equal(action(container, "save").disabled, true);
  assert.equal(action(container, "reset-local").disabled, true);
  const banner = container.querySelector(".params-banner");
  assert.ok(banner.textContent.includes("armed") && banner.hidden === false);
});

test("disarming opens it again", async () => {
  const { fake, container } = await mountLoaded();
  fake.setState({ armed: true });
  fake.getSubCb()({ armed: true });
  fake.setState({ armed: false });
  fake.getSubCb()({ armed: false });

  assert.equal(action(container, "load").disabled, false);
  assert.equal(container.querySelector(".params-banner").hidden, true);
});

test("a backend gate is surfaced even when the vehicle is disarmed", async () => {
  const fake = makeFakeTelemetry({
    replies: {
      "/api/sik/status": statusPayload({
        can_configure: false, blocked_reason: "pyserial is unavailable",
      }),
    },
  });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  Corvus.setupSik.render(container, () => {});
  await flush();

  assert.equal(action(container, "load").disabled, true);
  assert.equal(container.querySelector(".params-banner").textContent,
    "pyserial is unavailable");
});

test("with no serial ports at all there is nothing to press", async () => {
  const fake = makeFakeTelemetry({
    replies: { "/api/sik/status": statusPayload({ ports: [] }) },
  });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  Corvus.setupSik.render(container, () => {});
  await flush();

  const port = findByDataset(container, "role", "port")[0];
  assert.equal(port.disabled, true);
  assert.equal(action(container, "load").disabled, true);
});

test("destroy releases the telemetry subscription", async () => {
  const { fake, destroy } = await mountLoaded();
  assert.equal(fake.unsubCalls, 0);
  destroy();
  assert.equal(fake.unsubCalls, 1);
});

test("a late answer never writes to a page that is gone", async () => {
  const fake = makeFakeTelemetry();
  Corvus.telemetry = fake.telemetry;
  fake.setReply("/api/sik/load", loadPayload());
  const container = makeEl("div");
  const destroy = Corvus.setupSik.render(container, () => {});
  await flush();

  fire(action(container, "load"), "click");
  destroy();
  await flush();
  await flush();

  assert.equal(container.querySelectorAll(".sik-radio-card").length, 0,
    "the session outlived the page; its result must not land in the DOM");
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
  console.log(`\n${tests.length} passed`);
})();
