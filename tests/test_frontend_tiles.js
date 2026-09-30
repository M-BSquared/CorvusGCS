"use strict";

/**
 * Frontend tests for the offline-map dialog (Corvus.tiles).
 *
 * Corvus.ui is stubbed rather than loaded: what is under test is the download
 * flow in tiles.js (which job it follows, what it cancels, what it tells the
 * operator), not the widgets it is drawn with.
 *
 * Run:
 *   node tests/test_frontend_tiles.js
 */

const assert = require("node:assert/strict");
const path = require("node:path");

global.window = global;
global.Corvus = {};

// ---------------------------------------------------------------------------
// Minimal DOM
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "",
    textContent: "",
    children: [],
    hidden: false,
    disabled: false,
    value: "",
    _listeners: {},
  };
  e.classList = {
    add(c) { if (!e.classList.contains(c)) e.className = (e.className + " " + c).trim(); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x && x !== c).join(" "); },
    toggle(c, force) {
      const next = force === undefined ? !e.classList.contains(c) : !!force;
      if (next) e.classList.add(c); else e.classList.remove(c);
      return next;
    },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => { e.children.push(c); return c; };
  e.addEventListener = (type, fn) => { (e._listeners[type] = e._listeners[type] || []).push(fn); };
  e.dispatch = (type) => (e._listeners[type] || []).forEach((fn) => fn({ target: e }));
  return e;
}

global.document = {
  createElement: makeEl,
  createDocumentFragment: () => makeEl("fragment"),
};

// ---------------------------------------------------------------------------
// Corvus.ui stub: records what it builds so the tests can reach the controls.
// ---------------------------------------------------------------------------
const built = {};
let message = null;
let progress = null;

Corvus.ui = {
  select(o) { const s = makeEl("select"); built[o.id] = s; if (o.onChange) s.addEventListener("change", o.onChange); return s; },
  setOptions(sel, options, selected) { sel.options = options; sel.value = selected; },
  input(o) { const i = makeEl("input"); built[o.id] = i; return i; },
  field(o) { const f = makeEl("div"); f.appendChild(o.control); return f; },
  label(text) { const l = makeEl("span"); l.textContent = text; return l; },
  iconButton() { return makeEl("button"); },
  toggle(o) { let v = !!o.value; return { el: makeEl("input"), getValue: () => v, setValue: (x) => { v = !!x; } }; },
  progress(o) {
    progress = { el: makeEl("div"), done: 0, total: 0 };
    progress.el.hidden = !!(o && o.hidden);
    progress.set = (d, t) => { progress.done = d; progress.total = t; };
    progress.reset = () => progress.set(0, 0);
    return progress;
  },
  message() {
    message = { el: makeEl("div"), text: "", kind: null };
    message.el.hidden = true;
    message.show = (text, kind) => { message.el.hidden = false; message.text = text; message.kind = kind || null; };
    message.hide = () => { message.el.hidden = true; message.text = ""; message.kind = null; };
    return message;
  },
  button(o) { const b = makeEl("button"); b.disabled = !!o.disabled; b.click = () => o.onClick(); built[o.label] = b; return b; },
  modal(o) { return { open() {}, close() { o.onClose(); } }; },
  setBusy(btn, busy) { btn.classList.toggle("is-busy", !!busy); btn.disabled = !!busy; },
  clear(host) { host.children = []; },
  refreshIcons() {},
};

// ---------------------------------------------------------------------------
// Backend, event stream and map stubs
// ---------------------------------------------------------------------------
const requests = [];
let mapTilerKeySet = false;
let downloadReply = { job_id: "img", terrain_job_id: "dem", name: "Area" };

Corvus.telemetry = {
  requestJson(url, opts) {
    const body = opts && opts.body ? JSON.parse(opts.body) : null;
    requests.push({ url, body });
    if (url === "/api/tiles/sources") {
      return Promise.resolve({
        sources: [
          { id: "satellite", label: "Satellite", provider: "esri", maxzoom: 19 },
          { id: "maptiler_satellite", label: "Satellite", provider: "maptiler", maxzoom: 20 },
          { id: "mapbox_satellite", label: "Satellite", provider: "mapbox", maxzoom: 20 },
          { id: "google_satellite", label: "Satellite", provider: "google", maxzoom: 20 },
        ],
        providers: [
          { id: "esri", label: "Esri" },
          { id: "maptiler", label: "MapTiler", token_required: true, token_set: mapTilerKeySet },
          { id: "mapbox", label: "Mapbox", token_required: true, token_set: false },
          { id: "google", label: "Google", token_required: false, token_set: false },
        ],
        terrain: [{ id: "terrain", label: "Elevation", maxzoom: 15 }],
        max_tiles_per_job: 50000,
      });
    }
    if (url === "/api/tiles/regions") return Promise.resolve({ regions: [] });
    if (url === "/api/tiles/download") return Promise.resolve(downloadReply);
    if (url === "/api/tiles/cancel") return Promise.resolve({ ok: true });
    return Promise.reject(new Error("unexpected " + url));
  },
};

const followed = [];
const handlers = { tiles: new Set(), error: new Set() };
Corvus.events = {
  followJob(id) { followed.push(id); },
  subscribe(topic, fn) { handlers[topic].add(fn); return () => handlers[topic].delete(fn); },
};
function emit(data) { Array.from(handlers.tiles).forEach((fn) => fn(data)); }

let view = { w: 11.63, s: 48.075, e: 11.645, n: 48.085, zoom: 14 };
Corvus.map = {
  getMap() {
    return {
      getBounds: () => ({
        getWest: () => view.w, getSouth: () => view.s, getEast: () => view.e, getNorth: () => view.n,
      }),
      getZoom: () => view.zoom,
    };
  },
  getBaseLayer: () => "satellite",
  setRegions() {},
};

require(path.join(__dirname, "..", "src", "js", "tiles.js"));

const settle = () => new Promise((resolve) => setImmediate(resolve));

async function openDialog(nextView) {
  if (Corvus.tiles.isOpen()) Corvus.tiles.close();
  view = nextView || { w: 11.63, s: 48.075, e: 11.645, n: 48.085, zoom: 14 };
  requests.length = 0;
  followed.length = 0;
  Corvus.tiles.open();
  await settle();
  await settle();
}

async function startDownload(reply) {
  downloadReply = reply;
  built.Download.click();
  await settle();
  await settle();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
const tests = [];
function test(name, fn) { tests.push({ name, fn }); }

test("the elevation job is followed once the imagery is done", async () => {
  await openDialog();
  await startDownload({ job_id: "img", terrain_job_id: "dem" });
  assert.equal(followed[followed.length - 1], "img");

  emit({ job_id: "img", state: "done", done: 15, total: 15, failed: 0 });
  await settle();
  assert.equal(followed[followed.length - 1], "dem");
  assert.match(message.text, /elevation/i);
  assert.ok(built.Download.classList.contains("is-busy"), "still busy while elevation downloads");

  emit({ job_id: "dem", state: "done", done: 6, total: 6, failed: 0 });
  await settle();
  assert.equal(message.kind, "ok");
  assert.match(message.text, /Download complete/);
  assert.equal(built.Download.classList.contains("is-busy"), false);
});

test("a late imagery event does not finish the elevation phase", async () => {
  await openDialog();
  await startDownload({ job_id: "img", terrain_job_id: "dem" });
  emit({ job_id: "img", state: "done", done: 15, total: 15, failed: 0 });
  await settle();
  emit({ job_id: "img", state: "done", done: 15, total: 15, failed: 0 });
  assert.equal(progress.total, 0, "the bar belongs to the elevation job now");
  assert.ok(built.Download.classList.contains("is-busy"));
  emit({ job_id: "dem", state: "done", done: 6, total: 6, failed: 0 });
  await settle();
});

test("cancel stops the elevation job as well as the imagery", async () => {
  await openDialog();
  await startDownload({ job_id: "img", terrain_job_id: "dem" });
  built.Cancel.click();
  await settle();
  const cancelled = requests.filter((r) => r.url === "/api/tiles/cancel").map((r) => r.body.id);
  assert.deepEqual(cancelled.sort(), ["dem", "img"]);

  emit({ job_id: "img", state: "cancelled", done: 3, total: 15, failed: 0 });
  await settle();
  assert.equal(message.kind, "warn");
  assert.match(message.text, /cancelled/);
  assert.equal(followed[followed.length - 1], null, "stopped following");
});

test("the second elevation job is followed, reported and cancelled too", async () => {
  await openDialog();
  await startDownload({ job_id: "img", terrain_job_id: "dem", terrain_source_job_id: "cop" });
  emit({ job_id: "img", state: "done", done: 15, total: 15, failed: 0 });
  await settle();
  emit({ job_id: "dem", state: "done", done: 6, total: 6, failed: 0 });
  await settle();
  assert.equal(followed[followed.length - 1], "cop", "the chosen model is followed next");
  assert.ok(built.Download.classList.contains("is-busy"), "not finished while it downloads");

  built.Cancel.click();
  await settle();
  const cancelled = requests.filter((r) => r.url === "/api/tiles/cancel").map((r) => r.body.id);
  assert.deepEqual(cancelled.sort(), ["cop", "dem", "img"]);

  emit({ job_id: "cop", state: "done", done: 10, total: 13, failed: 3 });
  await settle();
  assert.equal(message.kind, "warn");
  assert.match(message.text, /3 tiles could not be fetched/);
});

test("a view with world copies is cut to one world before it is sent", async () => {
  // Zoomed out, MapLibre reports the view as drawn: east of the date line
  // here, and past the poles' tile edge. The backend refuses both.
  await openDialog({ w: 27.7, s: -89, e: 312.3, n: 88, zoom: 1 });
  await startDownload({ job_id: "img", terrain_job_id: null });
  let sent = requests.find((r) => r.url === "/api/tiles/download").body.bounds;
  assert.equal(sent.w, 27.7);
  assert.equal(sent.e, 180);
  assert.ok(sent.n < 85.06 && sent.s > -85.06, JSON.stringify(sent));
  emit({ job_id: "img", state: "done", done: 1, total: 1, failed: 0 });
  await settle();

  // Panned onto the next world copy: shifted back, not refused.
  await openDialog({ w: 190, s: 47, e: 200, n: 48, zoom: 8 });
  await startDownload({ job_id: "img2", terrain_job_id: null });
  sent = requests.find((r) => r.url === "/api/tiles/download").body.bounds;
  assert.deepEqual([sent.w, sent.e], [-170, -160]);
  emit({ job_id: "img2", state: "done", done: 1, total: 1, failed: 0 });
  await settle();

  // More than one whole world across is the whole world.
  await openDialog({ w: -300, s: -60, e: 400, n: 60, zoom: 0 });
  await startDownload({ job_id: "img3", terrain_job_id: null });
  sent = requests.find((r) => r.url === "/api/tiles/download").body.bounds;
  assert.deepEqual([sent.w, sent.e], [-180, 180]);
  emit({ job_id: "img3", state: "done", done: 1, total: 1, failed: 0 });
  await settle();
});

test("tiles that could not be fetched are reported, not called complete", async () => {
  await openDialog();
  await startDownload({ job_id: "img", terrain_job_id: "dem" });
  emit({ job_id: "img", state: "done", done: 12, total: 15, failed: 3 });
  await settle();
  emit({ job_id: "dem", state: "done", done: 5, total: 6, failed: 1 });
  await settle();
  assert.equal(message.kind, "warn");
  assert.match(message.text, /4 tiles could not be fetched/);
  assert.doesNotMatch(message.text, /Download complete/);
});

test("an imagery job that fetched nothing is reported as failed", async () => {
  await openDialog();
  await startDownload({ job_id: "img", terrain_job_id: null });
  emit({ job_id: "img", state: "failed", done: 0, total: 15, failed: 15, error: "no tile could be downloaded" });
  await settle();
  assert.equal(message.kind, "err");
  assert.match(message.text, /Download failed: no tile could be downloaded/);
});

test("an area over the per-job cap is refused before the press", async () => {
  await openDialog({ w: -180, s: -85, e: 180, n: 85, zoom: 5 });
  assert.equal(built.Download.disabled, true);
  assert.equal(message.kind, "err");
  assert.match(message.text, /the limit is 50\.0k/);
  built.Download.click();
  await settle();
  assert.equal(requests.filter((r) => r.url === "/api/tiles/download").length, 0);

  // Bringing the max zoom down lifts the refusal.
  built.tilesMaxZoom.value = 6;
  built.tilesMaxZoom.dispatch("change");
  assert.equal(built.Download.disabled, false);
  assert.notEqual(message.kind, "err");
});

test("keyed services are offered only once their key is stored", async () => {
  mapTilerKeySet = false;
  await openDialog();
  let offered = built.tilesSource.options.map((o) => o.value);
  assert.deepEqual(offered, ["satellite", "google_satellite"]);

  mapTilerKeySet = true;
  await openDialog();
  offered = built.tilesSource.options.map((o) => o.value);
  assert.deepEqual(offered, ["satellite", "maptiler_satellite", "google_satellite"]);
  mapTilerKeySet = false;
});

test("no dashes in the operator-facing messages", async () => {
  const src = require("node:fs").readFileSync(
    path.join(__dirname, "..", "src", "js", "tiles.js"), "utf-8");
  const shown = src.match(/showMsg\(([^;]*)\)/g) || [];
  shown.concat(src.match(/TERRAIN_PHASE_MSG = [^;]*/g) || []).forEach((call) => {
    assert.doesNotMatch(call, /—|–| - /, call);
  });
});

(async () => {
  let failed = 0;
  for (const t of tests) {
    try {
      await t.fn();
      console.log(`ok   - ${t.name}`);
    } catch (err) {
      failed++;
      console.log(`FAIL - ${t.name}\n       ${err && err.stack ? err.stack : err}`);
    }
  }
  if (failed) {
    console.log(`\n${failed} of ${tests.length} offline-map dialog tests failed.`);
    process.exit(1);
  }
  console.log(`\nAll ${tests.length} offline-map dialog tests passed.`);
})();
