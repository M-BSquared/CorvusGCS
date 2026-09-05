"use strict";

/**
 * Tests for Corvus.analysis (src/js/logs.js) — the flight-log section of
 * the Analysis page.
 *
 * What matters here is the queue and the folder: downloads are sequential
 * because MAVLink has one log session per vehicle, and the folder is chosen
 * once and persisted because re-answering "where to?" after every flight is
 * the thing this page exists to remove.
 *
 * Run:
 *   node tests/test_frontend_analysis.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};

const windowListeners = {};
window.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
window.removeEventListener = () => {};
global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
const dispatched = [];
window.dispatchEvent = (event) => { dispatched.push(event); };
window.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });

// Timers are captured, never auto-fired: the page polls, and a test that races
// a real timer is a test that fails on a slow machine. Node's own setTimeout is
// captured FIRST — window IS global here, so overriding it would also take away
// the timer this file's own flush() runs on.
const realSetTimeout = global.setTimeout;
const timeouts = [];
let nextTimeoutId = 1;
const clearedTimeouts = new Set();
window.setTimeout = (cb, ms) => { const id = nextTimeoutId++; timeouts.push({ id, cb, ms }); return id; };
window.clearTimeout = (id) => { clearedTimeouts.add(id); };
window.setInterval = () => 1;
window.clearInterval = () => {};

// --- minimal DOM stub -------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {}, style: {},
    type: "", hidden: false, disabled: false, value: "", checked: false,
    placeholder: "", id: "", _attrs: {}, _listeners: {}, _isEl: true, parentNode: null,
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
  Object.defineProperty(e, "firstChild", { get() { return e.children[0] || null; } });
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.removeEventListener = () => {};
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  return e;
}
function querySel(children, sel) {
  const out = [];
  const wantTag = sel && sel[0] !== ".";
  const classes = sel ? sel.split(".").filter(Boolean) : [];
  (function walk(list) {
    for (const c of list) {
      if (!c || !c._isEl) continue;
      const ok = wantTag ? c.tagName === sel.toUpperCase()
        : classes.every((cl) => c.className.split(/\s+/).includes(cl));
      if (ok) out.push(c);
      if (c.children) walk(c.children);
    }
  })(children);
  return out;
}
window.Plotly = {
  reactCalls: [], purgeCalls: [],
  react(gd, data, layout, config) { this.reactCalls.push({ gd, data, layout, config }); },
  purge(gd) { this.purgeCalls.push(gd); },
};

global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {}, removeEventListener: () => {},
};

function flush() { return new Promise((r) => realSetTimeout(r, 0)); }
function findByClass(root, cls) { return root.querySelectorAll("." + cls); }
function findOneByClass(root, cls) { return root.querySelector("." + cls); }
/** Dispatch, with an event real enough for a handler that stops it: the row
 *  shortcuts live inside a <label> and have to suppress its default toggle. */
function fire(el, type) {
  const event = {
    defaultPrevented: false, propagationStopped: false,
    preventDefault() { event.defaultPrevented = true; },
    stopPropagation() { event.propagationStopped = true; },
  };
  ((el && el._listeners && el._listeners[type]) || []).forEach((cb) => cb(event));
  return event;
}
/** Open one of the two log tiles, the way the Setup page's tiles work. */
function openTile(container, view) {
  const tile = container.querySelectorAll(".logs-tile").find((t) => t.dataset.view === view);
  assert.ok(tile, "tile " + view + " present");
  fire(tile, "click");
  return tile;
}

/** A button inside the open confirm dialog. */
function modalButton(root, label) {
  const overlay = root.querySelector(".modal-overlay");
  return overlay ? buttonByLabel(overlay, label) : null;
}

function buttonByLabel(root, label) {
  const text = (el) => [el.textContent || "", ...(el.children || []).map((c) => c.textContent || "")]
    .join(" ");
  return findByClass(root, "btn").find((b) => text(b).includes(label)) || null;
}

// --- fake telemetry ---------------------------------------------------------
const REVIEW = {
  ok: true,
  summary: { name: "log_001.ulg", duration_s: 412.5, airframe: 4001,
             sw: "abcdef0123", hw: "PX4_FMU_V6X", dropouts: 2, dropout_ms: 40,
             truncated: false, topics: [] },
  findings: [
    { level: "critical", text: "Accelerometer clipping occurred (12 samples)." },
    { level: "warning", text: "Magnetometer innovations reached 1.40." },
  ],
  plots: [
    { id: "actuators", title: "Motor outputs", unit: "µs", note: "…",
      group: "Control",
      series: [{ name: "Output 1", x: [0, 1], y: [1000, 1500] }] },
    { id: "ekf", title: "EKF innovation test ratios", unit: "ratio", threshold: 1.0,
      group: "Estimator",
      series: [{ name: "Magnetometer", x: [0, 1], y: [0.2, 1.4] }] },
  ],
  messages: [
    { t: 1, level: "error", text: "Preflight Fail: Compass" },
    { t: 2, level: "info", text: "Armed" },
  ],
  groups: ["Control", "Estimator"],
  modes: [
    { mode: "Manual", state: 0, start: 0, end: 4 },
    { mode: "Position", state: 2, start: 4, end: 10 },
  ],
  armed: [{ start: 1, end: 10 }],
};

function makeFakeTelemetry(status) {
  const postCalls = [];
  const requests = [];
  let current = status;
  let subCb = null;
  let unsubCalls = 0;
  let reviewResponse = REVIEW;
  return {
    setReview(r) { reviewResponse = r; },
    telemetry: {
      requestJson(url, options) {
        requests.push({ url, options });
        if (url === "/api/logs/status") return Promise.resolve(current);
        if (String(url).startsWith("/api/logs/review")) {
          return reviewResponse instanceof Error
            ? Promise.reject(reviewResponse)
            : Promise.resolve(reviewResponse);
        }
        return Promise.resolve({ ok: true });
      },
      postAction(url, payload) {
        postCalls.push({ url, payload });
        if (url === "/api/logs/dir") return Promise.resolve({ ok: true, dir: payload.dir });
        return Promise.resolve({ ok: true });
      },
      subscribe(fn) { subCb = fn; return () => { unsubCalls++; }; },
      getState() { return { connected: true, armed: false }; },
    },
    postCalls, requests,
    get unsubCalls() { return unsubCalls; },
    setStatus(s) { current = s; },
    getSubCb: () => subCb,
  };
}

const STATUS = {
  state: "idle", message: "3 log(s) on the vehicle", percent: 0,
  current: null, queued: [], completed: [],
  logs: [
    { id: 2, size: 2048, utc: 1700000000, downloaded: false, file: "" },
    { id: 1, size: 30720, utc: 1699996400, downloaded: true,
      file: "/home/pilot/.corvus/flightlogs/log_001.ulg",
      file_name: "log_001.ulg" },
    { id: 0, size: 10240, utc: 1699992800, downloaded: false, file: "" },
  ],
  saved: [{ name: "log_001.ulg", id: 1, size: 30720, path: "/home/pilot/x.ulg" }],
  tlogs: [{ name: "20260101-120000.tlog", size: 4096, path: "/home/p/.corvus/logs/x.tlog" }],
  dir: "/home/pilot/.corvus/flightlogs",
  connected: true,
};

require("../src/js/ui.js");
require("../src/js/setup-shared.js");
require("../src/js/analysis.js");

function reset(status) {
  dispatched.length = 0;
  timeouts.length = 0;
  const fake = makeFakeTelemetry(status || JSON.parse(JSON.stringify(STATUS)));
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  return { container, fake };
}

// ---------------------------------------------------------------------------

async function testFolderIsShownAndPersisted() {
  const { container, fake } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();

  const input = findOneByClass(container, "logs-dir-input");
  assert.equal(input.value, "/home/pilot/.corvus/flightlogs",
    "the configured folder is filled in from the backend");

  input.value = "/media/usb/logs";
  fire(input, "input");
  fire(buttonByLabel(container, "Save"), "click");
  await flush();

  const call = fake.postCalls.find((c) => c.url === "/api/logs/dir");
  assert.ok(call, "POST /api/logs/dir issued");
  assert.deepEqual(call.payload, { dir: "/media/usb/logs" },
    "the folder is persisted, not just used for this download");
  destroy();
}

async function testAnOperatorEditIsNotOverwrittenByAPoll() {
  const { container, fake } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();

  const input = findOneByClass(container, "logs-dir-input");
  input.value = "/media/usb/logs";
  fire(input, "input");
  // A poll lands while the operator is still typing the path.
  fake.setStatus(Object.assign({}, STATUS, { dir: "/home/pilot/.corvus/flightlogs" }));
  await Corvus.telemetry.requestJson("/api/logs/status");
  await flush();
  assert.equal(input.value, "/media/usb/logs", "the typed path survives a poll");
  destroy();
}

async function testTheLandingViewIsTwoTilesWithCounts() {
  const { container } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();

  const tiles = findByClass(container, "logs-tile");
  assert.equal(tiles.length, 3, "one tile per log kind, plus Flight Review");
  assert.deepEqual(tiles.map((t) => t.dataset.view), ["ulog", "tlog", "review"]);
  assert.match(findOneByClass(tiles[2], "tile-desc").textContent,
    /1 downloaded log\(s\) ready to review/);
  // The landing view answers "is there anything to fetch?" without opening
  // either sub-page.
  assert.match(findOneByClass(tiles[0], "tile-desc").textContent, /3 log\(s\) on the vehicle/);
  // What is already saved is stated up front — it decides whether opening the
  // tile is a chore or a no-op.
  assert.match(findOneByClass(tiles[0], "tile-desc").textContent, /1 already in the folder/);
  assert.match(findOneByClass(tiles[1], "tile-desc").textContent, /1 session\(s\)/);
  // Neither list is built until its tile is opened.
  assert.equal(findByClass(container, "logs-row").length, 0);
  destroy();
}

async function testTilesSayWhenThereIsNoLink() {
  const { container } = reset(Object.assign({}, STATUS, { connected: false, logs: [] }));
  const destroy = Corvus.analysis.render(container);
  await flush();
  const tile = findByClass(container, "logs-tile")[0];
  assert.match(findOneByClass(tile, "tile-desc").textContent, /Connect to the vehicle/);
  destroy();
}

async function testOpeningATileReplacesThePageRatherThanUnfolding() {
  const { container } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();

  const landing = findOneByClass(container, "logs-landing");
  const body = findOneByClass(container, "logs-body");
  assert.ok(!landing.hidden && body.hidden, "landing view before a tile is opened");

  openTile(container, "ulog");
  // A full view swap, not a card unfolding underneath: telemetry, the folder
  // row and the tiles all go away so the sub-page owns the screen.
  assert.ok(landing.hidden, "the landing view is replaced, not pushed down");
  assert.ok(!body.hidden, "the sub-page owns the screen");
  destroy();
}

async function testAlreadyDownloadedLogsAreMarked() {
  const { container } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");

  const rows = findByClass(container, "logs-row").filter((r) => r.dataset.id !== undefined);
  const byId = Object.fromEntries(rows.map((r) => [r.dataset.id, r]));
  assert.ok(findOneByClass(byId["1"], "logs-row-tag"), "the saved log is tagged");
  assert.equal(findOneByClass(byId["2"], "logs-row-tag"), null, "the missing one is not");
  assert.match(findOneByClass(byId["1"], "logs-row-meta").textContent, /log_001\.ulg/,
    "and says where it already is");
  destroy();
}

async function testSelectMissingSkipsWhatIsAlreadyInTheFolder() {
  const { container, fake } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");

  fire(buttonByLabel(container, "Select missing"), "click");
  assert.match(findOneByClass(container, "logs-selection").textContent, /2 selected/);
  fire(buttonByLabel(container, "Download selected"), "click");
  await flush();

  const call = fake.postCalls.find((c) => c.url === "/api/logs/download");
  // The common case after a flight: fetch what is not already here, without
  // pulling the ones on disk back over the link.
  assert.deepEqual(call.payload.ids.slice().sort(), [0, 2]);
  destroy();
}

async function testBackReturnsToTheTiles() {
  const { container } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();

  openTile(container, "ulog");
  assert.ok(findOneByClass(container, "logs-list"), "sub-page open");
  fire(findOneByClass(container, "setup-back"), "click");
  assert.equal(findByClass(container, "logs-tile").length, 3, "back at the tiles");
  assert.ok(!findOneByClass(container, "logs-landing").hidden, "landing view restored");
  destroy();
}

async function testVehicleLogsAreListedNewestFirst() {
  const { container } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");

  const rows = findByClass(container, "logs-row").filter((r) => r.dataset.id !== undefined);
  assert.equal(rows.length, 3, "one row per on-board log");
  assert.deepEqual(rows.map((r) => r.dataset.id), ["2", "1", "0"]);
  assert.match(findOneByClass(rows[1], "logs-row-meta").textContent, /30 KB/,
    "size is stated — it decides whether this is a 10-second or 10-minute job");
  destroy();
}

async function testSelectingSeveralQueuesThemInOneRequest() {
  const { container, fake } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");

  const boxes = findByClass(container, "logs-check");
  boxes[0].checked = true; fire(boxes[0], "change");
  boxes[2].checked = true; fire(boxes[2], "change");
  assert.match(findOneByClass(container, "logs-selection").textContent, /2 selected/);

  fire(buttonByLabel(container, "Download selected"), "click");
  await flush();

  const call = fake.postCalls.find((c) => c.url === "/api/logs/download");
  assert.ok(call, "POST /api/logs/download issued");
  // One request carrying every id: the backend owns the ordering, because the
  // MAVLink log session is single and the queue must be serialised there.
  assert.deepEqual(call.payload.ids.slice().sort(), [0, 2]);
  destroy();
}

async function testSelectAllAndClear() {
  const { container } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");

  fire(buttonByLabel(container, "Select all"), "click");
  assert.match(findOneByClass(container, "logs-selection").textContent, /3 selected/);
  fire(buttonByLabel(container, "Clear"), "click");
  assert.equal(findOneByClass(container, "logs-selection").textContent, "");
  destroy();
}

async function testDownloadIsBlockedWithoutASelectionOrALink() {
  const { container } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");
  const btn = buttonByLabel(container, "Download selected");
  assert.equal(btn.disabled, true, "nothing selected, nothing to download");
  destroy();

  const offline = reset(Object.assign({}, STATUS, { connected: false, logs: [] }));
  const destroy2 = Corvus.analysis.render(offline.container);
  await flush();
  openTile(offline.container, "ulog");
  assert.equal(buttonByLabel(offline.container, "Read from vehicle").disabled, true,
    "cannot read logs off a vehicle that is not there");
  const banner = findOneByClass(offline.container, "params-banner");
  assert.ok(banner && !banner.hidden, "and the reason is stated");
  destroy2();
}

async function testARunningQueueShowsWhichLogIsCurrentAndOffersCancel() {
  const { container } = reset(Object.assign({}, STATUS, {
    state: "downloading", percent: 40, current: 1, queued: [0],
    message: "log_001 — 12/30 KB (+1 queued)",
    completed: [{ id: 2, ok: true, detail: "/home/pilot/x.ulg" }],
  }));
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");

  const rows = findByClass(container, "logs-row").filter((r) => r.dataset.id !== undefined);
  const byId = Object.fromEntries(rows.map((r) => [r.dataset.id, r.dataset.state]));
  assert.equal(byId["1"], "active", "the log being fetched is marked");
  assert.equal(byId["2"], "done", "a finished one is marked too");
  assert.ok(!findOneByClass(container, "calib-progress").hidden, "progress shown");
  assert.ok(!buttonByLabel(container, "Cancel").hidden, "a long queue is cancellable");
  assert.equal(buttonByLabel(container, "Download selected").disabled, true,
    "no second job while one runs");
  destroy();
}

async function testLocalTlogsAreListedSeparately() {
  const { container } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "tlog");
  const staticRows = findByClass(container, "logs-row-static");
  assert.equal(staticRows.length, 1, "the recorded tlog is listed");
  assert.match(findOneByClass(staticRows[0], "logs-row-title").textContent, /\.tlog$/);
  destroy();
}

async function testEraseIsGatedBehindAConfirmThatNamesTheLoss() {
  const { container, fake } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");

  fire(buttonByLabel(container, "Erase all on vehicle"), "click");
  const overlay = findOneByClass(container, "modal-overlay");
  assert.ok(overlay, "the erase confirm opened");
  const text = findByClass(overlay, "motor-calib-warning-line")
    .map((l) => l.textContent).join(" ");
  // The two facts that decide the answer: it takes everything, and how much of
  // that is not saved anywhere else.
  assert.match(text, /ALL 3 log\(s\)/);
  assert.match(text, /no way to delete a single log/);
  assert.match(text, /2 of them are NOT in your download folder/);
  // Opening the gate must not erase anything.
  assert.equal(fake.postCalls.filter((c) => c.url === "/api/logs/erase").length, 0);
  destroy();
}

async function testKeepingTheLogsPostsNothing() {
  const { container, fake } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");

  fire(buttonByLabel(container, "Erase all on vehicle"), "click");
  fire(modalButton(container, "Keep the logs"), "click");
  assert.equal(findOneByClass(container, "modal-overlay"), null, "dialog closed");
  assert.equal(fake.postCalls.filter((c) => c.url === "/api/logs/erase").length, 0,
    "nothing erased");
  destroy();
}

async function testConfirmingErasePostsTheErase() {
  const { container, fake } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");

  fire(buttonByLabel(container, "Erase all on vehicle"), "click");
  fire(modalButton(container, "Erase anyway"), "click");
  await flush();

  assert.ok(fake.postCalls.find((c) => c.url === "/api/logs/erase"),
    "POST /api/logs/erase issued");
  assert.equal(findOneByClass(container, "modal-overlay"), null, "dialog closed");
  destroy();
}

async function testEraseWordingChangesWhenEverythingIsSaved() {
  const saved = JSON.parse(JSON.stringify(STATUS));
  saved.logs.forEach((l) => { l.downloaded = true; l.file = "/home/pilot/x.ulg"; });
  const { container } = reset(saved);
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");

  fire(buttonByLabel(container, "Erase all on vehicle"), "click");
  const overlay = findOneByClass(container, "modal-overlay");
  const text = findByClass(overlay, "motor-calib-warning-line")
    .map((l) => l.textContent).join(" ");
  assert.match(text, /All of them are already in your download folder/);
  // Nothing is at risk, so the button is not phrased as an override.
  assert.ok(modalButton(container, "Erase all logs"), "plain confirm wording");
  destroy();
}

async function testEraseIsBlockedWithNoLogsOrNoLink() {
  const empty = reset(Object.assign({}, STATUS, { logs: [], saved: [] }));
  const destroy = Corvus.analysis.render(empty.container);
  await flush();
  openTile(empty.container, "ulog");
  assert.equal(buttonByLabel(empty.container, "Erase all on vehicle").disabled, true,
    "nothing to erase");
  destroy();
}

async function testAnOpenEraseDialogIsDroppedOnTeardown() {
  const { container } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");
  fire(buttonByLabel(container, "Erase all on vehicle"), "click");
  assert.ok(findOneByClass(container, "modal-overlay"), "dialog open before teardown");

  destroy();
  assert.equal(findOneByClass(container, "modal-overlay"), null,
    "no dialog left behind on a page that is gone");
}

async function testFlightReviewListsDownloadedLogsAndPlotsOne() {
  const { container } = reset();
  window.Plotly.reactCalls.length = 0;
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "review");

  const select = container.querySelectorAll("select")[0];
  assert.equal(select.disabled, false, "the downloaded log is selectable");
  assert.equal(select.children[0].value, "log_001.ulg");

  fire(buttonByLabel(container, "Review"), "click");
  await flush();
  await flush();

  // Summary, then findings, then the plots, then what the aircraft itself said.
  assert.match(findOneByClass(container, "review-fact-value").textContent, /412.5 s/);
  const findings = findByClass(container, "review-finding");
  assert.deepEqual(findings.map((f) => f.dataset.level), ["critical", "warning"]);
  assert.equal(findByClass(container, "review-plot").length, 2, "one host per plot");
  assert.equal(window.Plotly.reactCalls.length, 2, "each plot drawn once");
  // The rejection line is the whole point of the EKF plot.
  const ekf = window.Plotly.reactCalls[1];
  // Found by shape, not by index: mode bands share the shapes array, and an
  // index-based check breaks the moment anything else is drawn.
  const line = (ekf.layout.shapes || []).find((sh) => sh.type === "line");
  assert.ok(line && line.y0 === 1.0, "the 1.0 threshold is drawn");
  const msgs = findByClass(container, "review-messages")[0];
  assert.equal(findByClass(msgs, "guidance-line").length, 2, "flight messages listed");
  destroy();
}

async function testADownloadedRowLinksStraightIntoItsReview() {
  const { container, fake } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");

  const shortcuts = findByClass(container, "logs-row-review");
  assert.equal(shortcuts.length, 1,
    "only the row that is actually in the folder can be reviewed");

  fire(shortcuts[0], "click");
  await flush();
  await flush();

  // Straight to the plots: the point of the shortcut is not having to pick the
  // file out of a list a second time.
  const asked = fake.requests.filter((r) => String(r.url).indexOf("/api/logs/review") === 0);
  assert.equal(asked.length, 1);
  assert.match(asked[0].url, /file=log_001\.ulg/);
  assert.equal(findByClass(container, "review-plot").length, 2, "the review is drawn");
  destroy();
}

async function testTheShortcutDoesNotAlsoTickTheRow() {
  /* The row is a <label> wrapping its checkbox: a click that is not stopped
     would queue the log for download on the way to reading it. */
  const { container } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");
  const event = fire(findByClass(container, "logs-row-review")[0], "click");
  assert.ok(event.defaultPrevented, "the label's default toggle is suppressed");
  assert.ok(event.propagationStopped, "the row does not also see the click");
  await flush();
  destroy();
}

async function testAVanishedFileSaysSoInsteadOfPlottingNothing() {
  const status = JSON.parse(JSON.stringify(STATUS));
  const { container, fake } = reset(status);
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");
  // Deleted from the folder after the row was drawn; the next poll sees it.
  status.saved = [];
  fake.setStatus(status);
  timeouts.splice(0).forEach((t) => t.cb());
  await flush();
  fire(findByClass(container, "logs-row-review")[0], "click");
  await flush();
  await flush();
  assert.equal(fake.requests.filter(
    (r) => String(r.url).indexOf("/api/logs/review") === 0).length, 0,
    "no review is requested for a file that is gone");
  assert.match(findOneByClass(container, "logs-status").textContent,
    /no longer in the folder/);
  destroy();
}

async function testLeavingMidReadDoesNotPaintTheAnswerLater() {
  /* A long flight takes a moment to parse. Leaving before it lands is normal;
     the answer arriving into a page the operator already walked away from is
     not, and it would draw Plotly graphs nothing ever purges. */
  const { container, fake } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "ulog");

  let release = null;
  const slow = new Promise((r) => { release = r; });
  const original = fake.telemetry.requestJson;
  fake.telemetry.requestJson = (url, opts) => (
    String(url).indexOf("/api/logs/review") === 0
      ? slow.then(() => JSON.parse(JSON.stringify(REVIEW)))
      : original(url, opts));

  window.Plotly.reactCalls.length = 0;
  fire(findByClass(container, "logs-row-review")[0], "click");
  await flush();
  assert.equal(findByClass(container, "review-loading").length, 1,
    "the wait is shown, not a blank page");

  // Back, then into Flight Review by hand — a fresh view of the same kind, so
  // "am I still on a review page?" is not enough to tell the answers apart.
  fire(buttonByLabel(container, "Analysis"), "click");
  await flush();
  openTile(container, "review");
  await flush();
  release();
  await flush();
  await flush();

  assert.equal(findByClass(container, "review-plot").length, 0,
    "nothing is drawn into the page that was left");
  assert.equal(window.Plotly.reactCalls.length, 0, "and no graph is created");
  destroy();
}

async function testFlightReviewWithNoDownloadedLogsSaysSo() {
  const { container } = reset(Object.assign({}, STATUS, { saved: [] }));
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "review");
  const select = container.querySelectorAll("select")[0];
  assert.equal(select.disabled, true, "nothing to review");
  assert.match(select.children[0].textContent, /No downloaded logs/);
  destroy();
}

async function testAnUnreadableLogReportsTheReasonAndKeepsThePage() {
  const { container, fake } = reset();
  fake.setReview(new Error("not a ULog file"));
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "review");

  fire(buttonByLabel(container, "Review"), "click");
  await flush();
  await flush();

  const note = findByClass(container, "logs-status")[0];
  assert.match(note.textContent, /not a ULog file/);
  assert.ok(note.className.includes("err"));
  // Still usable: pick another file and try again.
  assert.equal(buttonByLabel(container, "Review").disabled, false);
  destroy();
}

async function testPlotlyGraphsArePurgedOnLeavingTheReview() {
  const { container } = reset();
  window.Plotly.purgeCalls.length = 0;
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "review");
  fire(buttonByLabel(container, "Review"), "click");
  await flush();
  await flush();

  // Plotly holds a canvas and listeners per graph; dropping the DOM alone
  // leaks both, and this page can draw a dozen graphs per log.
  fire(findOneByClass(container, "setup-back"), "click");
  assert.equal(window.Plotly.purgeCalls.length, 2, "every graph purged on leaving");
  destroy();
}

async function testPlotsAreSectionedWithJumpChips() {
  const { container } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "review");
  fire(buttonByLabel(container, "Review"), "click");
  await flush();
  await flush();

  assert.deepEqual(findByClass(container, "review-group-title").map((e) => e.textContent),
    ["Control", "Estimator"], "one heading per group, in the backend's order");
  assert.equal(findByClass(container, "review-nav-chip").length, 2, "a jump chip each");
  destroy();
}

async function testFlightModesAreShownAsAStripAndDrawnBehindEveryTimePlot() {
  const { container } = reset();
  window.Plotly.reactCalls.length = 0;
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "review");
  fire(buttonByLabel(container, "Review"), "click");
  await flush();
  await flush();

  // The strip is the key as much as the timeline.
  assert.equal(findByClass(container, "review-mode-seg").length, 2, "one segment per span");
  assert.deepEqual(findByClass(container, "review-mode-chip").map((c) =>
    c.children[1].textContent), ["Manual", "Position"]);

  // And the same bands sit behind every time plot: the same oscillation means
  // different things in Position and in Manual.
  window.Plotly.reactCalls.forEach((call) => {
    const bands = (call.layout.shapes || []).filter((sh) => sh.type === "rect");
    assert.equal(bands.length, 2, "both mode bands drawn");
    assert.equal(bands[0].x0, 0);
    assert.equal(bands[1].x1, 10);
    assert.equal(bands[0].layer, "below", "bands sit behind the traces");
  });
  destroy();
}

async function testTheGroundTrackGetsNoTimeBands() {
  const { container, fake } = reset();
  fake.setReview(Object.assign({}, REVIEW, {
    groups: ["Flight"],
    plots: [{ id: "track", title: "Ground track", unit: "m", group: "Flight",
              xlabel: "East (m)", equal: true,
              series: [{ name: "Path", x: [0, 5], y: [0, 10] }] }],
  }));
  window.Plotly.reactCalls.length = 0;
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "review");
  fire(buttonByLabel(container, "Review"), "click");
  await flush();
  await flush();

  const layout = window.Plotly.reactCalls[0].layout;
  // The track's x axis is metres east, so a time span drawn across it would be
  // meaningless — and its axes must stay equal or the shape is wrong.
  assert.ok(!(layout.shapes || []).length, "no mode bands on a non-time plot");
  assert.equal(layout.xaxis.title, "East (m)");
  assert.equal(layout.yaxis.scaleanchor, "x");
  destroy();
}

async function testTheModeStripNamesSpansAndTotalsTheTime() {
  const { container } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "review");
  fire(buttonByLabel(container, "Review"), "click");
  await flush();
  await flush();

  // Wide spans are named in place; the key covers the slivers either way.
  const labels = findByClass(container, "review-mode-seg-label").map((e) => e.textContent);
  assert.deepEqual(labels, ["Manual", "Position"]);
  // How long each mode was actually flown — the strip alone cannot say that
  // when a mode appears more than once.
  const times = findByClass(container, "review-mode-time").map((e) => e.textContent);
  assert.deepEqual(times, ["4 s", "6 s"]);
  // And a time ruler, so "which mode when" is answerable from the overview.
  const ticks = findByClass(container, "review-mode-tick").map((e) => e.textContent);
  assert.equal(ticks.length, 5);
  assert.equal(ticks[0], "0:00");
  assert.equal(ticks[4], "0:10");
  destroy();
}

async function testSetpointSeriesAreDrawnAsMarkersNotLines() {
  const { container, fake } = reset();
  fake.setReview(Object.assign({}, REVIEW, {
    groups: ["Estimator"],
    plots: [{ id: "alt_sources", title: "Altitude estimate", unit: "m AMSL",
              group: "Estimator",
              series: [
                { name: "Fused estimate", x: [0, 1], y: [10, 11] },
                { name: "Altitude setpoint", x: [0, 1], y: [10, 12], draw: "markers" },
              ] }],
  }));
  window.Plotly.reactCalls.length = 0;
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "review");
  fire(buttonByLabel(container, "Review"), "click");
  await flush();
  await flush();

  const traces = window.Plotly.reactCalls[0].data;
  assert.equal(traces[0].mode, "lines");
  // Setpoints are sparse and stepped; a line through them implies values that
  // were never commanded.
  assert.equal(traces[1].mode, "markers");
  assert.ok(traces[1].marker, "drawn as points");
  destroy();
}

async function testTheModeTimelineGetsNamedAxisLevels() {
  const { container, fake } = reset();
  fake.setReview(Object.assign({}, REVIEW, {
    groups: ["Flight"],
    plots: [{ id: "modes", title: "Flight mode", unit: "", group: "Flight",
              ytick: { vals: [0, 1], labels: ["Manual", "Position"] },
              series: [{ name: "Flight mode", x: [0, 4, 4, 10], y: [0, 0, 1, 1],
                         draw: "lines", shape: "hv" }] }],
  }));
  window.Plotly.reactCalls.length = 0;
  const destroy = Corvus.analysis.render(container);
  await flush();
  openTile(container, "review");
  fire(buttonByLabel(container, "Review"), "click");
  await flush();
  await flush();

  const call = window.Plotly.reactCalls[0];
  assert.deepEqual(call.layout.yaxis.ticktext, ["Manual", "Position"],
    "levels are named, not numbered");
  // A step, not a ramp: a mode change is instantaneous.
  assert.equal(call.data[0].line.shape, "hv");
  destroy();
}

async function testDestroyStopsPollingAndUnsubscribes() {
  const { container, fake } = reset();
  const destroy = Corvus.analysis.render(container);
  await flush();
  const before = fake.requests.length;

  destroy();
  // Fire every scheduled poll; a destroyed page must issue no further request.
  timeouts.forEach((t) => { try { t.cb(); } catch (_e) {} });
  await flush();

  assert.equal(fake.requests.length, before,
    "no polling after teardown — the page is gone, the timer must be too");
  // Two subscriptions: the live Current Telemetry card and the link-state
  // watcher that re-polls when the vehicle comes or goes.
  assert.equal(fake.unsubCalls, 2, "both telemetry subscriptions released");
}

async function run() {
  const tests = [
    testFolderIsShownAndPersisted,
    testAnOperatorEditIsNotOverwrittenByAPoll,
    testTheLandingViewIsTwoTilesWithCounts,
    testTilesSayWhenThereIsNoLink,
    testOpeningATileReplacesThePageRatherThanUnfolding,
    testAlreadyDownloadedLogsAreMarked,
    testSelectMissingSkipsWhatIsAlreadyInTheFolder,
    testBackReturnsToTheTiles,
    testVehicleLogsAreListedNewestFirst,
    testSelectingSeveralQueuesThemInOneRequest,
    testSelectAllAndClear,
    testDownloadIsBlockedWithoutASelectionOrALink,
    testARunningQueueShowsWhichLogIsCurrentAndOffersCancel,
    testLocalTlogsAreListedSeparately,
    testEraseIsGatedBehindAConfirmThatNamesTheLoss,
    testKeepingTheLogsPostsNothing,
    testConfirmingErasePostsTheErase,
    testEraseWordingChangesWhenEverythingIsSaved,
    testEraseIsBlockedWithNoLogsOrNoLink,
    testAnOpenEraseDialogIsDroppedOnTeardown,
    testFlightReviewListsDownloadedLogsAndPlotsOne,
    testADownloadedRowLinksStraightIntoItsReview,
    testTheShortcutDoesNotAlsoTickTheRow,
    testAVanishedFileSaysSoInsteadOfPlottingNothing,
    testLeavingMidReadDoesNotPaintTheAnswerLater,
    testFlightReviewWithNoDownloadedLogsSaysSo,
    testAnUnreadableLogReportsTheReasonAndKeepsThePage,
    testPlotlyGraphsArePurgedOnLeavingTheReview,
    testPlotsAreSectionedWithJumpChips,
    testFlightModesAreShownAsAStripAndDrawnBehindEveryTimePlot,
    testTheGroundTrackGetsNoTimeBands,
    testTheModeStripNamesSpansAndTotalsTheTime,
    testSetpointSeriesAreDrawnAsMarkersNotLines,
    testTheModeTimelineGetsNamedAxisLevels,
    testDestroyStopsPollingAndUnsubscribes,
  ];
  for (const t of tests) {
    await t();
    console.log("ok   - " + t.name);
  }
  console.log("\nAll " + tests.length + " Analysis page tests passed.");
}

run().catch((error) => { console.error(error); process.exitCode = 1; });
