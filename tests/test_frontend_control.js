"use strict";

/**
 * Frontend tests for the Setup -> Radio Control page (Corvus.setupControl).
 *
 * Plain Node-runnable assertions (no browser, no test runner) following the
 * same pattern as tests/test_frontend_safety.js: stub the globals the module
 * touches, require the source, and assert on the rendered DOM, the write
 * spies, and the teardown/no-leak behaviour.
 *
 * The calibration wizard gets most of the attention here for the reason it
 * exists: PX4 has no autopilot-side RC calibration, so what this page measures
 * IS the calibration, and a wizard that mis-measures a sweep writes endpoints
 * the vehicle will happily fly with. Every assertion about the wizard is
 * ultimately about that — which channel it decided a stick was on, which way it
 * decided the stick moves, and what it refuses to send.
 *
 * Run:
 *   node tests/test_frontend_control.js
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

let dispatched = [];
window.dispatchEvent = (event) => { dispatched.push(event); };

window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;
window.setInterval = () => 1;
window.clearInterval = () => {};

// The Detect timeout is measured on the wall clock, so the clock is ours.
let clock = 1000;
const realNow = Date.now;
Date.now = () => clock;

// ---------------------------------------------------------------------------
// Minimal DOM stub (mirrors tests/test_frontend_safety.js, plus dispatchEvent —
// the Detect button re-uses the control's own change handler rather than
// duplicating the write path, so the stub has to be able to deliver one).
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {}, style: {},
    type: "", hidden: false, disabled: false, value: "", id: "",
    _attrs: {}, _listeners: {}, _isEl: true,
  };
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
  e.dispatchEvent = (ev) => {
    ((e._listeners[ev && ev.type]) || []).forEach((cb) => cb(ev));
    return true;
  };
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  return e;
}

function querySel(children, sel) {
  const out = [];
  const wantTag = sel && sel[0] !== ".";
  const classes = sel ? sel.split(".").filter(Boolean) : [];
  function walk(list) {
    for (const c of list) {
      if (!c || !c._isEl) continue;
      const ok = wantTag ? c.tagName === sel.toUpperCase()
                         : classes.every((cl) => c.className.split(/\s+/).includes(cl));
      if (ok) out.push(c);
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
function flushMicrotasks() { return new Promise((r) => realNow && setTimeout(r, 0)); }
function findByClass(root, cls) { return root.querySelectorAll("." + cls); }
function findOneByClass(root, cls) { return root.querySelector("." + cls); }

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

/** The editable control (not the row) that writes `param`. */
function control(container, param) {
  return findByDataset(container, "param", param)
    .filter((e) => e.tagName === "SELECT" || e.tagName === "INPUT")[0];
}

/** A Corvus.ui.button by its visible label (the label lives in a child span). */
function buttonByLabel(root, label) {
  return root.querySelectorAll("button").find((btn) => (btn.children || [])
    .some((c) => c._isEl && c.textContent === label));
}

/** Text of every element with `cls`, in document order. */
function textsOf(root, cls) {
  return findByClass(root, cls).map((e) => e.textContent);
}

// ---------------------------------------------------------------------------
// Fake telemetry with spies. `subscribe` captures the callback (does NOT
// auto-fire — tests drive it via push()) and returns an unsub spy.
// ---------------------------------------------------------------------------
function makeFakeTelemetry(opts = {}) {
  const postCalls = [];
  const requests = [];
  let subCb = null;
  let unsubCalls = 0;
  let state = opts.state || { armed: false, connected: true, rc_live: false, rc_channels: [] };
  let doc = opts.doc;
  const rejectUrls = opts.rejectUrls || {};
  const telemetry = {
    postAction(url, payload) {
      postCalls.push({ url, payload });
      if (rejectUrls[url]) return Promise.reject(new Error(rejectUrls[url]));
      return Promise.resolve({ ok: true, writes: (payload && payload.channels) || [] });
    },
    requestJson(url) {
      requests.push(url);
      if (opts.requestError) return Promise.reject(new Error(opts.requestError));
      return Promise.resolve(doc);
    },
    getState() { return state; },
    subscribe(cb) { subCb = cb; return () => { unsubCalls++; }; },
  };
  return {
    telemetry, postCalls, requests,
    get unsubCalls() { return unsubCalls; },
    setDoc(next) { doc = next; },
    setState(s) { state = s; },
    /** Deliver one telemetry frame, as the SSE push would. */
    push(next) {
      state = Object.assign({}, state, next);
      if (subCb) subCb(state);
    },
    writes() { return postCalls.filter((c) => c.url === "/api/params/set").map((c) => c.payload); },
    streams() { return postCalls.filter((c) => c.url === "/api/rc/stream").map((c) => c.payload); },
    calibrations() { return postCalls.filter((c) => c.url === "/api/rc/calibrate").map((c) => c.payload); },
  };
}

// ---------------------------------------------------------------------------
// Load the module under test. ui.js first (same order as index.html).
// ---------------------------------------------------------------------------
require("../src/js/ui.js");
require("../src/js/setup-shared.js");
require("../src/js/setup-control.js");

// ---------------------------------------------------------------------------
// A representative /api/rc payload: one plain form section, the stick channels,
// the mode switch with its slots, and the calibration table — i.e. one instance
// of everything the page knows how to draw.
// ---------------------------------------------------------------------------
function channelOptions(count) {
  const options = [{ value: 0, label: "Unassigned" }];
  for (let n = 1; n <= count; n += 1) options.push({ value: n, label: "Channel " + n });
  return options;
}

function mapField(param, label, value) {
  return { param, label, kind: "enum", value, role: "channel", options: channelOptions(8) };
}

function rcDoc(overrides) {
  const doc = {
    connected: true,
    received: 30,
    channel_limit: 8,
    assignments: {
      RC_MAP_ROLL: 1, RC_MAP_PITCH: 2, RC_MAP_THROTTLE: 3, RC_MAP_YAW: 4,
      RC_MAP_FLTMODE: 5, RC_MAP_KILL_SW: 6,
    },
    sections: [
      {
        id: "input", title: "Input and failsafe", kind: "fields",
        hint: "What the vehicle accepts from a transmitter.",
        fields: [
          { param: "COM_RC_IN_MODE", label: "Accepted input", kind: "enum", value: 0,
            options: [{ value: 0, label: "RC transmitter only" },
                      { value: 1, label: "Joystick only" }] },
          { param: "COM_RC_LOSS_T", label: "RC loss timeout", kind: "number",
            value: 0.5, unit: "s" },
        ],
      },
      {
        id: "sticks", title: "Stick channels", kind: "fields", hint: "",
        fields: [
          mapField("RC_MAP_ROLL", "Roll", 1),
          mapField("RC_MAP_PITCH", "Pitch", 2),
          mapField("RC_MAP_THROTTLE", "Throttle", 3),
          mapField("RC_MAP_YAW", "Yaw", 4),
        ],
      },
      {
        id: "modes", title: "Flight mode switch", kind: "fields", hint: "",
        fields: [
          mapField("RC_MAP_FLTMODE", "Flight mode channel", 5),
          { param: "COM_FLTMODE1", label: "Position 1", kind: "enum", value: 0, role: "mode",
            options: [{ value: -1, label: "Unassigned" }, { value: 0, label: "Manual" },
                      { value: 2, label: "Position" }] },
          { param: "COM_FLTMODE2", label: "Position 2", kind: "enum", value: 2, role: "mode",
            options: [{ value: -1, label: "Unassigned" }, { value: 0, label: "Manual" },
                      { value: 2, label: "Position" }] },
          { param: "COM_FLTMODE3", label: "Position 3", kind: "enum", value: -1, role: "mode",
            options: [{ value: -1, label: "Unassigned" }, { value: 0, label: "Manual" },
                      { value: 2, label: "Position" }] },
          { param: "COM_FLTMODE4", label: "Position 4", kind: "enum", value: -1, role: "mode",
            options: [{ value: -1, label: "Unassigned" }] },
          { param: "COM_FLTMODE5", label: "Position 5", kind: "enum", value: -1, role: "mode",
            options: [{ value: -1, label: "Unassigned" }] },
          { param: "COM_FLTMODE6", label: "Position 6", kind: "enum", value: -1, role: "mode",
            options: [{ value: -1, label: "Unassigned" }] },
        ],
      },
      {
        id: "switches", title: "Switches", kind: "fields", hint: "",
        fields: [mapField("RC_MAP_KILL_SW", "Kill switch", 6)],
      },
      {
        id: "channels", title: "Channel calibration", kind: "channels", hint: "",
        rows: [
          { channel: 1, min: 1100, max: 1900, trim: 1500, dz: 10, reversed: false,
            has_rev: true, travel: 800, calibrated: true,
            params: { min: "RC1_MIN", max: "RC1_MAX", trim: "RC1_TRIM",
                      dz: "RC1_DZ", rev: "RC1_REV" } },
          { channel: 2, min: 1500, max: 1520, trim: 1510, dz: 10, reversed: true,
            has_rev: true, travel: 20, calibrated: false,
            params: { min: "RC2_MIN", max: "RC2_MAX", trim: "RC2_TRIM",
                      dz: "RC2_DZ", rev: "RC2_REV" } },
        ],
      },
    ],
  };
  return Object.assign(doc, overrides || {});
}

/** Render the page with a canned /api/rc payload. */
async function openWith(doc, opts) {
  dispatched = [];
  clock = 1000;
  const fake = makeFakeTelemetry(Object.assign({ doc }, opts || {}));
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  const destroy = Corvus.setupControl.render(container, () => {});
  await flushMicrotasks();
  return { container, destroy, fake };
}

/** A telemetry frame carrying channel pulses. */
function frame(channels, extra) {
  return Object.assign({
    armed: false, connected: true, rc_live: true,
    rc_channels: channels, rc_channel_count: channels.length, rc_rssi: 82,
  }, extra || {});
}

// ===========================================================================
// PART A — the overview
// ===========================================================================

async function testReadsTheSchemaAndRaisesTheChannelRate() {
  const { container, fake } = await openWith(rcDoc());

  assert.ok(fake.requests.includes("/api/rc"), "the page reads /api/rc on open");
  assert.deepEqual(fake.streams(), [{ enabled: true, rate_hz: 20 }],
    "opening the page asks for a rate a sweep can actually be measured at");

  const titles = textsOf(container, "page-section-title");
  assert.deepEqual(titles, ["Live channels", "Input and failsafe", "Stick channels",
    "Flight mode switch", "Switches", "Channel calibration"]);
}

async function testChannelBarsFollowTheTelemetry() {
  const { container, fake } = await openWith(rcDoc());

  assert.equal(findOneByClass(container, "rc-monitor-state").dataset.state, "bad",
    "no frame has arrived yet, so the monitor says so");

  fake.push(frame([1100, 1500, 1900, 1000, 1500, 2000, 1234, 1500]));

  const rows = findByClass(container, "rc-channel");
  assert.equal(rows.length, 8, "one bar per delivered channel");
  assert.equal(findOneByClass(rows[0], "rc-channel-value").textContent, "1100 µs");
  assert.equal(findOneByClass(rows[2], "rc-channel-fill").style.width, "83.3%",
    "1900 us sits five-sixths up the 900-2100 scale");
  assert.equal(findOneByClass(container, "rc-monitor-state").dataset.state, "ok");
  assert.ok(findOneByClass(container, "rc-monitor-state-label").textContent.includes("82%"),
    "the RSSI the receiver reported is shown");
}

async function testAChannelTheReceiverStopsDeliveringIsRemoved() {
  const { container, fake } = await openWith(rcDoc());
  fake.push(frame([1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500]));
  assert.equal(findByClass(container, "rc-channel").length, 8);

  fake.push(frame([1500, 1500, 1500, 1500]));
  assert.equal(findByClass(container, "rc-channel").length, 4,
    "a frozen bar reads as a live stick, so it is removed rather than kept");
}

async function testEachBarSaysWhatItsChannelIsBoundTo() {
  const { container, fake } = await openWith(rcDoc());
  fake.push(frame([1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500]));

  const notes = findByClass(container, "rc-channel")
    .map((r) => findOneByClass(r, "rc-channel-note").textContent);
  assert.deepEqual(notes.slice(0, 6),
    ["Roll", "Pitch", "Throttle", "Yaw", "Flight mode channel", "Kill switch"]);
  assert.equal(notes[6], "", "an unbound channel says nothing rather than guessing");
}

async function testAChannelBoundTwiceIsCalledOut() {
  const doc = rcDoc();
  doc.assignments.RC_MAP_KILL_SW = 5;   // sharing the mode switch's channel
  doc.sections[3].fields[0].value = 5;
  const { container, fake } = await openWith(doc);
  fake.push(frame([1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500]));

  const warn = findOneByClass(container, "rc-conflict");
  assert.ok(warn, "a doubled-up channel is surfaced, not left for the flight");
  assert.ok(warn.children.some((c) => c.textContent
    && c.textContent.startsWith("Channel 5 is bound to more than one action")));

  const note = findByClass(container, "rc-channel")[4];
  assert.equal(findOneByClass(note, "rc-channel-note").textContent,
    "Flight mode channel, Kill switch", "the bar names both actions, not the last one");
}

async function testTwoDoubledChannelsReadAsASentence() {
  const doc = rcDoc();
  doc.assignments.RC_MAP_KILL_SW = 5;
  doc.sections[3].fields[0].value = 5;
  doc.assignments.RC_MAP_RETURN_SW = 1;
  doc.sections[3].fields.push(mapField("RC_MAP_RETURN_SW", "Return switch", 1));
  const { container } = await openWith(doc);

  const warn = findOneByClass(container, "rc-conflict");
  assert.ok(warn.children.some((c) => c.textContent
    && c.textContent.startsWith("Channels 1 and 5 are each bound to more than one action")),
    "two doubled-up channels are one readable sentence, not a list glued to a verb");
}

async function testTheModeStripLightsTheLivePosition() {
  const { container, fake } = await openWith(rcDoc());

  const slots = findByClass(container, "rc-mode-slot");
  assert.equal(slots.length, 6, "six positions");
  assert.deepEqual(textsOf(container, "rc-mode-slot-name").slice(0, 3),
    ["Manual", "Position", "Unassigned"], "each slot shows the mode it selects");

  // Mode channel (5) at its low end -> position 1; at its high end -> 6.
  fake.push(frame([1500, 1500, 1500, 1500, 900, 1500, 1500, 1500]));
  assert.equal(slots[0].dataset.active, "1");
  assert.equal(slots[5].dataset.active, "");

  fake.push(frame([1500, 1500, 1500, 1500, 2100, 1500, 1500, 1500]));
  assert.equal(slots[5].dataset.active, "1");
  assert.equal(slots[0].dataset.active, "");
}

async function testTheCalibrationTableFlagsAnUncalibratedChannel() {
  const { container, fake } = await openWith(rcDoc());

  const rows = findByClass(container, "rc-cal-row").filter((r) => r.dataset.channel);
  assert.equal(rows.length, 2);
  assert.equal(rows[0].dataset.uncalibrated, undefined);
  assert.equal(rows[1].dataset.uncalibrated, "1",
    "20 us of travel is not a calibrated channel, whatever the operator believes");

  assert.equal(control(container, "RC1_MIN").value, "1100");
  assert.equal(control(container, "RC2_REV").value, "-1",
    "the direction is read from the sign the vehicle holds");

  fake.push(frame([1234, 1500, 1500, 1500, 1500, 1500, 1500, 1500]));
  assert.equal(findOneByClass(rows[0], "rc-cal-now").textContent, "1234 µs",
    "the live pulse sits beside the endpoint it should have produced");
}

async function testAFieldWriteGoesThroughTheParameterEndpoint() {
  const { container, fake } = await openWith(rcDoc());

  const input = control(container, "COM_RC_LOSS_T");
  input.value = "1.5";
  fire(input, "change");
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [{ name: "COM_RC_LOSS_T", value: 1.5 }]);
}

async function testARefusedWriteRestoresTheControl() {
  const { container, fake } = await openWith(rcDoc(), {
    rejectUrls: { "/api/params/set": "cannot set parameter while armed" },
  });

  const input = control(container, "COM_RC_LOSS_T");
  input.value = "1.5";
  fire(input, "change");
  await flushMicrotasks();

  assert.equal(input.value, "0.5",
    "a control showing a value the vehicle refused is a lie about the aircraft");
  assert.ok(dispatched.some((e) => e.detail && /COM_RC_LOSS_T/.test(e.detail.message)));
}

async function testArmedGatingDisablesEveryControl() {
  const { container, fake } = await openWith(rcDoc());
  fake.push(frame([1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500], { armed: true }));

  assert.equal(findOneByClass(container, "params-banner").hidden, false);
  assert.equal(control(container, "RC_MAP_ROLL").disabled, true);
  assert.equal(control(container, "RC1_MIN").disabled, true);
  assert.equal(buttonByLabel(container, "Calibrate radio").disabled, true);
}

async function testTeardownReleasesTheSubscriptionAndTheStream() {
  const { destroy, fake } = await openWith(rcDoc());
  destroy();
  await flushMicrotasks();

  assert.equal(fake.unsubCalls, 1, "the telemetry subscription is released");
  assert.deepEqual(fake.streams(), [
    { enabled: true, rate_hz: 20 },
    { enabled: false, rate_hz: 20 },
  ], "the rate is handed back to the firmware rather than left raised");
}

async function testDisconnectedRendersAnExplanationNotAnError() {
  const { container } = await openWith({ connected: false, sections: [], received: 0 });
  assert.equal(findByClass(container, "rc-field").length, 0);
  assert.ok(findOneByClass(container, "params-desc"), "an explanation, not an error banner");
  assert.ok(findOneByClass(container, "params-actions-status").className.includes("err"));
}

// ===========================================================================
// PART B — Detect
// ===========================================================================

async function testDetectBindsTheChannelThatMoved() {
  const { container, fake } = await openWith(rcDoc());
  fake.push(frame([1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500]));

  const row = findByDataset(container, "param", "RC_MAP_KILL_SW")
    .find((e) => e.className.includes("rc-field"));
  fire(findOneByClass(row, "rc-detect"), "click");

  // Channel 7 sweeps; everything else jitters by a few microseconds.
  fake.push(frame([1502, 1499, 1500, 1501, 1500, 1498, 1900, 1500]));
  await flushMicrotasks();

  assert.deepEqual(fake.writes(), [{ name: "RC_MAP_KILL_SW", value: 7 }],
    "the channel that actually moved is the one bound");
  assert.equal(control(container, "RC_MAP_KILL_SW").value, "7");
}

async function testDetectIgnoresReceiverJitter() {
  const { container, fake } = await openWith(rcDoc());
  fake.push(frame([1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500]));

  const row = findByDataset(container, "param", "RC_MAP_KILL_SW")
    .find((e) => e.className.includes("rc-field"));
  fire(findOneByClass(row, "rc-detect"), "click");

  fake.push(frame([1508, 1494, 1503, 1497, 1500, 1502, 1499, 1501]));
  await flushMicrotasks();
  assert.deepEqual(fake.writes(), [], "a few microseconds of noise binds nothing");

  clock += 9000;
  fake.push(frame([1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500]));
  assert.equal(findOneByClass(row, "params-row-status").textContent, "nothing moved",
    "a detection that never resolved says so instead of waiting forever");
}

async function testDetectWithoutASignalRefusesRatherThanGuessing() {
  const { container, fake } = await openWith(rcDoc());

  const row = findByDataset(container, "param", "RC_MAP_KILL_SW")
    .find((e) => e.className.includes("rc-field"));
  fire(findOneByClass(row, "rc-detect"), "click");
  await flushMicrotasks();

  assert.equal(findOneByClass(row, "params-row-status").textContent, "no RC signal");
  assert.deepEqual(fake.writes(), []);
}

// ===========================================================================
// PART C — the calibration wizard
// ===========================================================================

/** Open the page, deliver a first frame, and enter the wizard. */
async function openWizard(doc) {
  const opened = await openWith(doc || rcDoc());
  opened.fake.push(frame([1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500]));
  fire(buttonByLabel(opened.container, "Calibrate radio"), "click");
  await flushMicrotasks();
  return opened;
}

/** Click Next — refusing to do it while the wizard has it disabled.
 *  A browser will not deliver a click to a disabled button and the DOM stub
 *  will, so without this check a step that can never be advanced past reads as
 *  a passing test. */
function next(container) {
  const btn = buttonByLabel(container, "Next");
  assert.equal(btn.disabled, false,
    "Next is disabled — this step cannot actually be advanced past");
  fire(btn, "click");
}

/** Same, for the one button that writes to the vehicle. */
function writeToVehicle(container) {
  const btn = buttonByLabel(container, "Write to vehicle");
  assert.equal(btn.disabled, false, "the write button is disabled");
  fire(btn, "click");
}

/** Every stick centred, throttle down — where the transmitter rests. */
const REST = [1500, 1500, 1000, 1500, 1500, 1500, 1500, 1500];

/** Drive one stick prompt: hold the named channel at `value`, then Next.
 *  Everything else stays where it rests, which is what the operator's hands
 *  actually do — resetting the throttle to mid-stick between prompts would
 *  make it the largest mover on every one of them. */
function holdAndAdvance(fake, container, channel, value) {
  const channels = REST.slice();
  channels[channel - 1] = value;
  fake.push(frame(channels));
  next(container);
}

/** Walk the wizard from the centre step through all four stick prompts. */
async function runFullSweep(fake, container) {
  // Centre -> sweep: the resting position is captured on the way out.
  fake.push(frame(REST));
  next(container);
  // Sweep: everything through its travel, then on once four have moved.
  fake.push(frame([1100, 1100, 1000, 1100, 1000, 1000, 1500, 1500]));
  fake.push(frame([1900, 1900, 2000, 1900, 2000, 2000, 1500, 1500]));
  next(container);
  // The four sticks, in the order the wizard asks for them.
  holdAndAdvance(fake, container, 3, 2000);   // throttle up   -> channel 3, normal
  holdAndAdvance(fake, container, 1, 1900);   // roll right    -> channel 1, normal
  holdAndAdvance(fake, container, 2, 1100);   // pitch forward -> channel 2, reversed
  holdAndAdvance(fake, container, 4, 1900);   // yaw right     -> channel 4, normal
}

async function testTheWizardWalksTheStepsAndMeasuresEachStick() {
  const { container, fake } = await openWizard();

  assert.ok(findOneByClass(container, "rc-headline").textContent.includes("Before you start"));
  next(container);
  assert.ok(findOneByClass(container, "rc-headline").textContent.includes("Centre the sticks"));

  await runFullSweep(fake, container);

  assert.ok(findOneByClass(container, "rc-headline").textContent.includes("Review"));
  const rows = findByClass(container, "rc-cal-row").filter((r) => r.dataset.channel);
  const byChannel = {};
  rows.forEach((r) => {
    byChannel[r.dataset.channel] = findByClass(r, "rc-cal-cell").map((c) => c.textContent);
  });
  assert.equal(byChannel["1"][1], "Roll");
  assert.equal(byChannel["2"][1], "Pitch");
  assert.equal(byChannel["1"][5], "Normal");
  assert.equal(byChannel["2"][5], "Reversed",
    "a pitch channel that fell when the stick went forward is reversed");
  assert.equal(byChannel["7"], undefined, "a channel nobody swept is not written");
}

async function testTheWizardWritesWhatItMeasured() {
  const { container, fake } = await openWizard();
  next(container);
  await runFullSweep(fake, container);

  writeToVehicle(container);
  await flushMicrotasks();

  const posted = fake.calibrations();
  assert.equal(posted.length, 1, "one write, carrying the whole measurement");
  const payload = posted[0];
  assert.deepEqual(payload.mapping, {
    RC_MAP_THROTTLE: 3, RC_MAP_ROLL: 1, RC_MAP_PITCH: 2, RC_MAP_YAW: 4,
  });
  const channel2 = payload.channels.find((c) => c.channel === 2);
  assert.deepEqual(channel2, {
    channel: 2, min: 1100, max: 1900, trim: 1500, reversed: true, set_reverse: true,
  });
  const channel5 = payload.channels.find((c) => c.channel === 5);
  assert.equal(channel5.set_reverse, false,
    "the mode switch has no measured direction, so its RC5_REV is left alone");
  assert.equal(payload.count, 6, "the highest channel that was actually swept");
}

async function testTheThrottleCentreIsClampedIntoItsOwnTravel() {
  const { container, fake } = await openWizard();
  next(container);
  await runFullSweep(fake, container);

  writeToVehicle(container);
  await flushMicrotasks();

  const throttle = fake.calibrations()[0].channels.find((c) => c.channel === 3);
  assert.equal(throttle.trim, 1000,
    "the throttle rested at its minimum, and that is its centre");
  assert.ok(throttle.trim >= throttle.min && throttle.trim <= throttle.max);
}

async function testTheWizardWillNotAdvanceWithoutASweep() {
  const { container, fake } = await openWizard();
  next(container);                       // -> centre
  fake.push(frame([1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500]));
  next(container);                       // -> sweep
  assert.ok(findOneByClass(container, "rc-headline").textContent.includes("full travel"));

  // Two channels twitch; the other six never move.
  fake.push(frame([1400, 1600, 1500, 1500, 1500, 1500, 1500, 1500]));
  assert.equal(buttonByLabel(container, "Next").disabled, true,
    "four sticks have to have travelled before the sweep counts");

  fake.push(frame([1100, 1900, 1000, 1100, 1500, 1500, 1500, 1500]));
  fake.push(frame([1900, 1100, 2000, 1900, 1500, 1500, 1500, 1500]));
  assert.equal(buttonByLabel(container, "Next").disabled, false);
}

async function testTheWizardWillNotAdvancePastAStickThatDidNotMove() {
  const { container, fake } = await openWizard();
  next(container);
  fake.push(frame(REST));
  next(container);
  fake.push(frame([1100, 1100, 1000, 1100, 1500, 1500, 1500, 1500]));
  fake.push(frame([1900, 1900, 2000, 1900, 1500, 1500, 1500, 1500]));
  next(container);

  // Back at rest: nothing is being held, so nothing has been measured.
  fake.push(frame(REST));
  assert.ok(findOneByClass(container, "rc-headline").textContent.includes("Throttle"));
  assert.equal(buttonByLabel(container, "Next").disabled, true,
    "a stick the operator did not move must not be bound to whatever drifted");
}

async function testAnArmedVehicleStopsTheWizardBeforeItWrites() {
  const { container, fake } = await openWizard();
  next(container);
  await runFullSweep(fake, container);

  fake.push(frame(REST, { armed: true }));
  assert.equal(buttonByLabel(container, "Write to vehicle").disabled, true);
  assert.equal(findOneByClass(container, "params-banner").hidden, false);
}

async function testARefusedCalibrationIsReportedAndNothingIsClaimed() {
  const doc = rcDoc();
  const { container, fake } = await openWith(doc, {
    rejectUrls: { "/api/rc/calibrate": "channel 3 moved only 40 us" },
  });
  fake.push(frame([1500, 1500, 1500, 1500, 1500, 1500, 1500, 1500]));
  fire(buttonByLabel(container, "Calibrate radio"), "click");
  await flushMicrotasks();
  next(container);
  await runFullSweep(fake, container);

  writeToVehicle(container);
  await flushMicrotasks();

  const line = findOneByClass(findOneByClass(container, "rc-status"), "params-actions-status");
  assert.equal(line.textContent, "channel 3 moved only 40 us");
  assert.ok(line.className.includes("err"), "and it reads as a failure");
  assert.ok(dispatched.some((e) => e.detail && e.detail.level === "critical"),
    "a refused calibration is a notification, not a silent status line");
  assert.ok(findOneByClass(container, "rc-review"), "the wizard stays on the review step");
}

async function testStartOverForgetsTheWholeMeasurement() {
  const { container, fake } = await openWizard();
  next(container);
  await runFullSweep(fake, container);
  assert.ok(findByClass(container, "rc-cal-row").length > 1);

  fire(buttonByLabel(container, "Start over"), "click");
  assert.ok(findOneByClass(container, "rc-headline").textContent.includes("Before you start"));

  next(container);                       // intro -> centre
  fake.push(frame(REST));
  next(container);                       // centre -> sweep
  assert.equal(buttonByLabel(container, "Next").disabled, true,
    "the travel measured before the restart is gone, so the sweep starts over");

  fake.push(frame([1100, 1100, 1000, 1100, 1500, 1500, 1500, 1500]));
  fake.push(frame([1900, 1900, 2000, 1900, 1500, 1500, 1500, 1500]));
  next(container);                       // sweep -> sticks
  assert.ok(findOneByClass(container, "rc-hint").textContent.includes("Nothing measured"),
    "a restart drops the sticks it had already measured");
}

// ---------------------------------------------------------------------------
async function main() {
  const tests = [
    testReadsTheSchemaAndRaisesTheChannelRate,
    testChannelBarsFollowTheTelemetry,
    testAChannelTheReceiverStopsDeliveringIsRemoved,
    testEachBarSaysWhatItsChannelIsBoundTo,
    testAChannelBoundTwiceIsCalledOut,
    testTwoDoubledChannelsReadAsASentence,
    testTheModeStripLightsTheLivePosition,
    testTheCalibrationTableFlagsAnUncalibratedChannel,
    testAFieldWriteGoesThroughTheParameterEndpoint,
    testARefusedWriteRestoresTheControl,
    testArmedGatingDisablesEveryControl,
    testTeardownReleasesTheSubscriptionAndTheStream,
    testDisconnectedRendersAnExplanationNotAnError,
    testDetectBindsTheChannelThatMoved,
    testDetectIgnoresReceiverJitter,
    testDetectWithoutASignalRefusesRatherThanGuessing,
    testTheWizardWalksTheStepsAndMeasuresEachStick,
    testTheWizardWritesWhatItMeasured,
    testTheThrottleCentreIsClampedIntoItsOwnTravel,
    testTheWizardWillNotAdvanceWithoutASweep,
    testTheWizardWillNotAdvancePastAStickThatDidNotMove,
    testAnArmedVehicleStopsTheWizardBeforeItWrites,
    testARefusedCalibrationIsReportedAndNothingIsClaimed,
    testStartOverForgetsTheWholeMeasurement,
  ];
  for (const t of tests) {
    await t();
    console.log("  ok", t.name);
  }
  console.log(`\n${tests.length} passed — Setup > Radio Control`);
}

main().catch((err) => { console.error(err); process.exit(1); });
