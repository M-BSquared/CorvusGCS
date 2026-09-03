"use strict";

/**
 * Frontend tests for the Parameters Export/Import actions
 * (Corvus.setupParameters), focused on the new actions bar.
 *
 * Plain Node-runnable assertions (no browser, no test runner) following the
 * same pattern as tests/test_frontend_setup.js: stub the globals the module
 * touches, require the source, and assert on the rendered DOM, the export
 * blob capture, the import upload SSE flow, and the armed gating / teardown.
 *
 * Run:
 *   node tests/test_frontend_params_export.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals so setup-parameters.js (and setup-shared.js) load and
// run in Node. Mirrors tests/test_frontend_setup.js.
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};

let reducedMotion = false;
window.matchMedia = (query) => ({
  matches: reducedMotion && String(query).includes("prefers-reduced-motion"),
  media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});

let clock = 1000;
Date.now = () => clock;
// new Date().toISOString() must work — keep the native Date with a fixed now().
const realToISOString = Date.prototype.toISOString;

// window.dispatchEvent captures corvus:notification events.
const dispatched = [];
window.dispatchEvent = (event) => { dispatched.push(event); };

// Fake EventSource so the params-progress SSE can be driven deterministically.
const eventSources = [];
class FakeEventSource {
  constructor(url) { this.url = url; this.listeners = new Map(); this.closed = false; eventSources.push(this); }
  addEventListener(type, cb) { this.listeners.set(type, cb); }
  close() { this.closed = true; }
  emit(type, data) {
    const cb = this.listeners.get(type);
    if (cb) cb({ data: typeof data === "string" ? data : JSON.stringify(data) });
  }
}
global.EventSource = FakeEventSource;

// Controlled setInterval (never auto-fires) so the ~1s params poll is driven
// manually and the tests stay deterministic.
const intervalCbs = [];
let nextIntervalId = 1;
const clearedIds = new Set();
window.setInterval = (cb) => { const id = nextIntervalId++; intervalCbs.push({ id, cb }); return id; };
window.clearInterval = (id) => { clearedIds.add(id); };
window.setTimeout = global.setTimeout;
window.clearTimeout = global.clearTimeout;

// ---------------------------------------------------------------------------
// Minimal DOM stub (mirrors tests/test_frontend_setup.js). The source builds
// structure with createElement + appendChild, so this stub tracks children,
// className/classList, textContent, dataset, style, listeners, and innerHTML
// (clears children when set to "").
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
    readOnly: false,
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
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; return c; };
  e.remove = () => { /* no-op: detached in the stub */ };
  e.insertBefore = (n, ref) => { const i = ref ? e.children.indexOf(ref) : e.children.length; if (i < 0) e.children.push(n); else e.children.splice(i, 0, n); return n; };
  Object.defineProperty(e, "firstChild", { get() { return e.children[0] || null; } });
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (type, cb) => { (e._listeners[type] = e._listeners[type] || []).push(cb); };
  e.removeEventListener = () => {};
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  e.click = () => fire(e, "click");
  return e;
}

function querySel(children, sel) {
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

// document + document.body (the export anchor and the import input are appended
// to document.body and then removed).
let pageViewEl = null;
const bodyEl = makeEl("body");
global.document = {
  createElement: makeEl,
  // Corvus.ui.modal assembles its body in a fragment. The stub models it as a
  // plain element: appendChild/children behave the same, which is all the
  // module and these assertions need.
  createDocumentFragment: () => makeEl("fragment"),
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: (id) => (id === "pageView" ? pageViewEl : null),
  querySelectorAll: () => [],
  // Both halves of the pair: ui.modal registers a document keydown handler
  // while a dialog is mounted and removes it on close.
  addEventListener: () => {},
  removeEventListener: () => {},
  body: bodyEl,
};

// ---------------------------------------------------------------------------
// The export writes the file through the BACKEND (POST /api/params/export), so
// there is no Blob and no object URL to stub any more — what the tests assert
// is the request the dialog sends and the path it reports back.
// ---------------------------------------------------------------------------

// FileReader fallback (unused when file.text() is defined, but kept for safety).
global.FileReader = class FileReader {
  readAsText() { /* tests use file.text() */ }
};

// window.confirm → true (the import confirmation proceeds).
window.confirm = () => true;

// ---------------------------------------------------------------------------
// Helpers ---------------------------------------------------------------------
function flushMicrotasks() { return new Promise((r) => setTimeout(r, 0)); }

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

function fire(el, type, payload) {
  const listeners = (el && el._listeners && el._listeners[type]) || [];
  listeners.forEach((cb) => cb(payload || {}));
}

// ---------------------------------------------------------------------------
// Fake Corvus.telemetry with spies (mirrors test_frontend_setup.js). The
// requestJson responses are per-URL so /api/version, /api/params, and
// /api/params/upload/result can each be controlled independently.
// ---------------------------------------------------------------------------
function makeFakeTelemetry(opts = {}) {
  const postCalls = [];
  const requests = [];
  const jsonCalls = [];   // requestJson calls that carried an init (i.e. POSTs)
  let subCb = null;
  let unsubCalls = 0;
  const unsub = () => { unsubCalls++; };
  // Default is an obviously-non-real placeholder so the version single-source
  // grep-scan (tests/test_version_single_source.py) does not flag this file for
  // hardcoding the live VERSION literal. Tests that assert a specific version
  // (Test 2) override `versionResponse` explicitly.
  const versionResponse = opts.versionResponse || { product: "Corvus GCS", version: "0.0.0-test" };
  let paramsResponse = opts.paramsResponse || { complete: false, received: 0, count: 0, params: [] };
  const uploadResultResponse = opts.uploadResultResponse || { written: 0, failed: 0, errors: [] };
  // What GET /api/params/export/target reports: where the file would go and
  // the backend-generated default name.
  const exportTargetResponse = opts.exportTargetResponse ||
    { dir: "/home/pilot/.corvus/params", filename: "corvus-params_px4-quadrotor_2026-01-02_03-04.json" };
  let state = opts.state || { armed: false, connected: true };
  const telemetry = {
    postAction(url, payload) {
      postCalls.push({ url, payload });
      if (url === "/api/params/upload" && opts.uploadReject) {
        return Promise.reject(new Error(opts.uploadReject));
      }
      return Promise.resolve({ ok: true, state: "uploading", count: payload && payload.params ? payload.params.length : 0 });
    },
    requestJson(url, init) {
      requests.push(url);
      // The export flow POSTs through requestJson, so the init is captured for
      // assertions (postCalls only covers postAction).
      if (init) jsonCalls.push({ url, init, body: init.body ? JSON.parse(init.body) : null });
      if (url === "/api/version") return Promise.resolve(versionResponse);
      if (url === "/api/params/upload/result") return Promise.resolve(uploadResultResponse);
      if (url === "/api/params/export/target") return Promise.resolve(exportTargetResponse);
      if (url === "/api/params/export") {
        if (opts.exportReject) return Promise.reject(new Error(opts.exportReject));
        const body = init && init.body ? JSON.parse(init.body) : {};
        return Promise.resolve({
          ok: true,
          dir: body.dir,
          filename: body.filename,
          path: `${body.dir}/${body.filename}`,
          param_count: (body.params || []).length,
        });
      }
      return Promise.resolve(paramsResponse);
    },
    subscribe(fn) { subCb = fn; return unsub; },
    getState() { return state; },
  };
  return {
    telemetry, postCalls, requests, jsonCalls,
    get unsubCalls() { return unsubCalls; },
    getSubCb: () => subCb,
    setParamsResponse(r) { paramsResponse = r; },
    setState(s) { state = s; },
    getUploadResultResponse: () => uploadResultResponse,
  };
}

// ---------------------------------------------------------------------------
// Load the module under test (shared helpers + the parameters page). We do
// NOT load setup.js (the orchestrator) — these tests drive setupParameters
// directly so they focus on the export/import actions.
// ---------------------------------------------------------------------------
// ui.js first: it defines Corvus.ui, the component layer every other
// module builds its DOM with (index.html loads it in the same order).
require("../src/js/ui.js");
require("../src/js/setup-shared.js");
require("../src/js/setup-parameters.js");

// ===========================================================================
// Test 1 — render creates an actions bar with Export (disabled) + Import
// (enabled when disarmed) buttons.
// ===========================================================================
async function testActionsBarRendersWithInitialState() {
  const fake = makeFakeTelemetry({ state: { armed: false, connected: true } });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;
  clearedIds.clear();
  dispatched.length = 0;

  const destroy = Corvus.setupParameters.render(container, () => {});
  await flushMicrotasks();

  const actions = findOneByClass(container, "params-actions");
  assert.ok(actions, "params-actions bar present");
  const btns = findByClass(actions, "btn");
  assert.equal(btns.length, 2, "exactly two action buttons (Export + Import)");
  const exportBtn = findByDataset(actions, "lucide", "download")[0]
    || btns.find((b) => b.children.some((c) => c._attrs && c._attrs["data-lucide"] === "download"));
  const importBtn = findByDataset(actions, "lucide", "upload")[0]
    || btns.find((b) => b.children.some((c) => c._attrs && c._attrs["data-lucide"] === "upload"));
  assert.ok(exportBtn, "Export button present (download icon)");
  assert.ok(importBtn, "Import button present (upload icon)");
  assert.equal(exportBtn.disabled, true, "Export disabled before params are loaded");
  assert.equal(importBtn.disabled, false, "Import enabled when disarmed");
  assert.ok(findOneByClass(actions, "params-actions-status"), "actions status line present");

  destroy();
}

// ===========================================================================
// Test 2 — after a complete download, Export becomes enabled; clicking it
// opens the export dialog prefilled from GET /api/params/export/target, and
// Save POSTs the parameter set to /api/params/export and reports the path the
// backend wrote it to.
//
// The file is written server-side, not pulled as a browser download: the
// desktop build runs inside QtWebEngine, which drops an <a download> unless
// the host app implements a download handler, so the old blob export produced
// no file there at all. The version metadata is stamped by the backend for the
// same single-source reason — the frontend must not put one in the payload.
// ===========================================================================
async function testExportOpensDialogAndSavesThroughBackend() {
  const fake = makeFakeTelemetry({
    state: { armed: false, connected: true },
    exportTargetResponse: {
      dir: "/home/pilot/.corvus/params",
      filename: "corvus-params_px4-quadrotor_2026-01-02_03-04.json",
    },
    paramsResponse: {
      complete: true, received: 2, count: 2, state: "complete",
      params: [
        { name: "MC_ROLL_P", value: 6, type: 9 },
        { name: "FW_ACRO_LIM", value: 1, type: 9 },
      ],
    },
  });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;
  clearedIds.clear();
  dispatched.length = 0;

  const destroy = Corvus.setupParameters.render(container, () => {});
  await flushMicrotasks();

  // Drive the download: click the Download Parameters button → openProgressView.
  const downloadBtn = findOneByClass(container, "params-download-btn");
  fire(downloadBtn, "click");
  await flushMicrotasks();   // postAction → openProgressView (SSE + poll created)

  // Complete the download via the SSE progress event.
  const sse = eventSources[0];
  sse.emit("progress", { state: "complete", received: 2, count: 2 });
  await flushMicrotasks();   // finishDownload → requestJson(/api/params)
  await flushMicrotasks();   // renderEditor

  // Export is now enabled.
  const actions = findOneByClass(container, "params-actions");
  const exportBtn = findByDataset(actions, "lucide", "download")[0]
    || findByClass(actions, "btn").find((b) => b.children.some((c) => c._attrs && c._attrs["data-lucide"] === "download"));
  assert.equal(exportBtn.disabled, false, "Export enabled after the full set is loaded");

  // Click Export → asks the backend where the file would go → opens the dialog.
  fire(exportBtn, "click");
  await flushMicrotasks();   // requestJson(/api/params/export/target)
  await flushMicrotasks();   // dialog build + open

  assert.ok(fake.requests.includes("/api/params/export/target"),
    "the dialog asks the backend for the target folder + default filename");

  const dialog = findOneByClass(bodyEl, "modal");
  assert.ok(dialog, "export dialog opened");
  // The DOM stub's selector engine handles classes and tags, not #id, so the
  // shared field controls are found by class and matched on their id property.
  const fieldById = (root, id) => findByClass(root, "field-input").find((e) => e.id === id) || null;
  const nameInput = fieldById(dialog, "paramsExportName");
  const dirInput = fieldById(dialog, "paramsExportDir");
  assert.ok(nameInput && dirInput, "dialog has a filename and a folder field");
  assert.equal(nameInput.value, "corvus-params_px4-quadrotor_2026-01-02_03-04.json",
    "filename prefilled from the backend (readable date + vehicle, not an epoch)");
  assert.equal(dirInput.value, "/home/pilot/.corvus/params",
    "folder prefilled from the backend");

  // The operator retargets the export, then saves.
  nameInput.value = "before-maiden-flight.json";
  dirInput.value = "/mnt/usb/flights";
  const saveBtn = findByClass(findOneByClass(bodyEl, "modal-actions"), "btn")
    .find((b) => b.getAttribute("data-variant") === "primary");
  assert.ok(saveBtn, "dialog has a Save button");
  fire(saveBtn, "click");
  await flushMicrotasks();   // POST /api/params/export
  await flushMicrotasks();   // close + status + notification

  const post = fake.jsonCalls.find((c) => c.url === "/api/params/export");
  assert.ok(post, "POST /api/params/export sent");
  assert.equal(post.init.method, "POST", "export is a POST");
  assert.equal(post.body.filename, "before-maiden-flight.json", "operator filename is sent");
  assert.equal(post.body.dir, "/mnt/usb/flights", "operator folder is sent");
  assert.equal(post.body.params.length, 2, "the full loaded set is sent");
  assert.deepEqual(
    post.body.params.map((p) => p.name).sort(),
    ["FW_ACRO_LIM", "MC_ROLL_P"],
    "params carry name + value + type",
  );
  // The backend stamps product/version/exported_at; a frontend copy would be a
  // second source of truth for the version.
  assert.equal(post.body.version, undefined, "frontend does not stamp a version");
  assert.equal(post.body.product, undefined, "frontend does not stamp product metadata");

  assert.ok(!findOneByClass(bodyEl, "modal"), "dialog closed after a successful save");

  // The saved path is surfaced — it is what the operator needs to find the file.
  const note = dispatched.find((e) => e.type === "corvus:notification"
    && /\/mnt\/usb\/flights\/before-maiden-flight\.json/.test(e.detail.message));
  assert.ok(note, "notification names the full path the file was written to");
  assert.equal(note.detail.level, "info", "export notification is info level");
  const status = findOneByClass(container, "params-actions-status");
  assert.ok(/\/mnt\/usb\/flights\/before-maiden-flight\.json/.test(status.textContent),
    "the actions status line names the saved path");

  destroy();
}

// ===========================================================================
// Test 2b — a failing export keeps the dialog open and shows the reason, so
// the operator can fix the folder and retry instead of losing the export.
// ===========================================================================
async function testExportFailureKeepsDialogOpenWithReason() {
  const fake = makeFakeTelemetry({
    state: { armed: false, connected: true },
    exportReject: "could not write to /mnt/usb/flights: Read-only file system",
    paramsResponse: {
      complete: true, received: 1, count: 1, state: "complete",
      params: [{ name: "MC_ROLL_P", value: 6, type: 9 }],
    },
  });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;
  clearedIds.clear();
  dispatched.length = 0;

  const destroy = Corvus.setupParameters.render(container, () => {});
  await flushMicrotasks();
  fire(findOneByClass(container, "params-download-btn"), "click");
  await flushMicrotasks();
  eventSources[0].emit("progress", { state: "complete", received: 1, count: 1 });
  await flushMicrotasks();
  await flushMicrotasks();

  const actions = findOneByClass(container, "params-actions");
  const exportBtn = findByClass(actions, "btn")
    .find((b) => b.children.some((c) => c._attrs && c._attrs["data-lucide"] === "download"));
  fire(exportBtn, "click");
  await flushMicrotasks();
  await flushMicrotasks();

  const saveBtn = findByClass(findOneByClass(bodyEl, "modal-actions"), "btn")
    .find((b) => b.getAttribute("data-variant") === "primary");
  fire(saveBtn, "click");
  await flushMicrotasks();
  await flushMicrotasks();

  assert.ok(findOneByClass(bodyEl, "modal"), "dialog stays open when the save fails");
  const msg = findOneByClass(bodyEl, "ui-msg");
  assert.ok(msg && /Read-only file system/.test(msg.textContent),
    "the backend's reason is shown in the dialog");
  assert.equal(saveBtn.disabled, false, "Save is usable again so the operator can retry");

  destroy();
}

// ===========================================================================
// Test 3 — Import a valid file → postAction(/api/params/upload) called → SSE
// emits upload_complete → /api/params/upload/result fetched → summary shown.
// ===========================================================================
async function testImportValidFileUploadsAndSummarises() {
  const fake = makeFakeTelemetry({
    state: { armed: false, connected: true },
    uploadResultResponse: { written: 1, failed: 0, errors: [] },
  });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;
  clearedIds.clear();
  dispatched.length = 0;

  const destroy = Corvus.setupParameters.render(container, () => {});
  await flushMicrotasks();

  const actions = findOneByClass(container, "params-actions");
  const importBtn = findByDataset(actions, "lucide", "upload")[0]
    || findByClass(actions, "btn").find((b) => b.children.some((c) => c._attrs && c._attrs["data-lucide"] === "upload"));
  assert.equal(importBtn.disabled, false, "Import enabled when disarmed");

  // Stub the input element that document.createElement returns for type=file:
  // give it a fake file whose text() resolves the import JSON. The module
  // appends the input to document.body and calls input.click(); the change
  // listener is attached synchronously, so we capture it before firing.
  const fakeFile = {
    name: "params.json",
    text() { return Promise.resolve(JSON.stringify({
      product: "Corvus GCS", version: "2026.09.42",
      params: [{ name: "X", value: 1 }],
    })); },
  };
  const origCreate = document.createElement;
  let capturedInput = null;
  document.createElement = function (tag) {
    const el = origCreate(tag);
    if (String(tag).toLowerCase() === "input") {
      el.files = [fakeFile];
      capturedInput = el;
    }
    return el;
  };
  try {
    fire(importBtn, "click");
  } finally {
    document.createElement = origCreate;
  }
  assert.ok(capturedInput, "import created a file input");
  // The module appended the input to document.body then called input.click().
  // The change listener is attached; fire it to simulate a file selection.
  fire(capturedInput, "change");
  await flushMicrotasks();   // readFileText → JSON.parse → confirm → postAction
  await flushMicrotasks();   // openUploadProgress (SSE created)

  const uploadCall = fake.postCalls.find((c) => c.url === "/api/params/upload");
  assert.ok(uploadCall, "POST /api/params/upload issued");
  assert.deepEqual(uploadCall.payload, { params: [{ name: "X", value: 1 }] },
    "upload payload is the cleaned {name,value} list");

  // The upload SSE is the last EventSource created.
  const uploadSse = eventSources[eventSources.length - 1];
  assert.equal(uploadSse.closed, false, "upload SSE open while uploading");
  // Emit upload_complete → close SSE → fetch result → summarise.
  uploadSse.emit("progress", { state: "upload_complete", count: 1, received: 1 });
  await flushMicrotasks();   // finishUpload → requestJson(/api/params/upload/result)
  await flushMicrotasks();

  assert.ok(fake.requests.includes("/api/params/upload/result"),
    "GET /api/params/upload/result fetched after upload_complete");
  assert.equal(uploadSse.closed, true, "upload SSE closed on upload_complete");
  const status = findOneByClass(container, "params-actions-status");
  assert.ok(status.classList.contains("ok"), "summary status has ok class (failed===0)");
  assert.ok(/Uploaded 1 parameters/.test(status.textContent),
    `summary text shows written count: got "${status.textContent}"`);

  destroy();
}

// ===========================================================================
// Test 4 — Import invalid JSON / missing params → notify critical, no
// postAction call.
// ===========================================================================
async function testImportInvalidJsonNotifiesAndDoesNotPost() {
  const fake = makeFakeTelemetry({ state: { armed: false, connected: true } });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;
  clearedIds.clear();
  dispatched.length = 0;

  const destroy = Corvus.setupParameters.render(container, () => {});
  await flushMicrotasks();

  const actions = findOneByClass(container, "params-actions");
  const importBtn = findByDataset(actions, "lucide", "upload")[0]
    || findByClass(actions, "btn").find((b) => b.children.some((c) => c._attrs && c._attrs["data-lucide"] === "upload"));

  // Invalid JSON file.
  const fakeFile = { name: "bad.json", text() { return Promise.resolve("not json {"); } };
  const origCreate = document.createElement;
  let capturedInput = null;
  document.createElement = function (tag) {
    const el = origCreate(tag);
    if (String(tag).toLowerCase() === "input") { el.files = [fakeFile]; capturedInput = el; }
    return el;
  };
  try { fire(importBtn, "click"); } finally { document.createElement = origCreate; }
  fire(capturedInput, "change");
  await flushMicrotasks();
  await flushMicrotasks();

  assert.equal(fake.postCalls.filter((c) => c.url === "/api/params/upload").length, 0,
    "no upload POST for invalid JSON");
  const note = dispatched.find((e) => e.type === "corvus:notification" && /valid JSON/.test(e.detail.message));
  assert.ok(note, "critical notification dispatched for invalid JSON");
  assert.equal(note.detail.level, "critical", "invalid JSON notification is critical");

  // Second case: valid JSON but missing params array.
  dispatched.length = 0;
  const fakeFile2 = { name: "no-params.json", text() { return Promise.resolve(JSON.stringify({ product: "Corvus GCS" })); } };
  const origCreate2 = document.createElement;
  let capturedInput2 = null;
  document.createElement = function (tag) {
    const el = origCreate2(tag);
    if (String(tag).toLowerCase() === "input") { el.files = [fakeFile2]; capturedInput2 = el; }
    return el;
  };
  try { fire(importBtn, "click"); } finally { document.createElement = origCreate2; }
  fire(capturedInput2, "change");
  await flushMicrotasks();
  await flushMicrotasks();
  assert.equal(fake.postCalls.filter((c) => c.url === "/api/params/upload").length, 0,
    "no upload POST for a file with no params array");
  const note2 = dispatched.find((e) => e.type === "corvus:notification" && /no parameters/i.test(e.detail.message));
  assert.ok(note2, "critical notification dispatched for missing params");
  assert.equal(note2.detail.level, "critical", "missing-params notification is critical");

  destroy();
}

// ===========================================================================
// Test 5 — Armed gating: getState() {armed:true} → Import disabled; the
// subscribe callback toggles it.
// ===========================================================================
async function testArmedGatingDisablesImport() {
  const fake = makeFakeTelemetry({ state: { armed: true, connected: true } });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;

  const destroy = Corvus.setupParameters.render(container, () => {});
  await flushMicrotasks();

  const actions = findOneByClass(container, "params-actions");
  const importBtn = findByDataset(actions, "lucide", "upload")[0]
    || findByClass(actions, "btn").find((b) => b.children.some((c) => c._attrs && c._attrs["data-lucide"] === "upload"));
  assert.equal(importBtn.disabled, true, "Import disabled when armed at render time");

  // Disarm via the telemetry subscription callback → Import re-enabled.
  fake.getSubCb()({ armed: false, connected: true });
  assert.equal(importBtn.disabled, false, "Import re-enabled when the subscribe callback reports disarmed");

  // Re-arm via the subscribe callback → Import disabled again.
  fake.getSubCb()({ armed: true, connected: true });
  assert.equal(importBtn.disabled, true, "Import disabled again when the subscribe callback reports armed");

  destroy();
}

// ===========================================================================
// Test 6 — destroy() closes the upload SSE.
// ===========================================================================
async function testDestroyClosesUploadSse() {
  const fake = makeFakeTelemetry({
    state: { armed: false, connected: true },
    uploadResultResponse: { written: 1, failed: 0, errors: [] },
  });
  Corvus.telemetry = fake.telemetry;
  const container = makeEl("div");
  clock = 1000;
  eventSources.length = 0;
  intervalCbs.length = 0;
  clearedIds.clear();
  dispatched.length = 0;

  const destroy = Corvus.setupParameters.render(container, () => {});
  await flushMicrotasks();

  const actions = findOneByClass(container, "params-actions");
  const importBtn = findByDataset(actions, "lucide", "upload")[0]
    || findByClass(actions, "btn").find((b) => b.children.some((c) => c._attrs && c._attrs["data-lucide"] === "upload"));

  // Drive an import far enough to open the upload SSE.
  const fakeFile = { name: "params.json", text() { return Promise.resolve(JSON.stringify({ params: [{ name: "X", value: 1 }] })); } };
  const origCreate = document.createElement;
  let capturedInput = null;
  document.createElement = function (tag) {
    const el = origCreate(tag);
    if (String(tag).toLowerCase() === "input") { el.files = [fakeFile]; capturedInput = el; }
    return el;
  };
  try { fire(importBtn, "click"); } finally { document.createElement = origCreate; }
  fire(capturedInput, "change");
  await flushMicrotasks();
  await flushMicrotasks();   // openUploadProgress created the upload SSE

  // The first EventSource is the download SSE (if any); the upload SSE is the
  // last one created. Identify it and confirm it is open before destroy().
  const uploadSse = eventSources[eventSources.length - 1];
  assert.equal(uploadSse.closed, false, "upload SSE open before destroy");

  destroy();
  assert.equal(uploadSse.closed, true, "upload SSE closed by destroy");
}

// ===========================================================================
// Run all tests.
// ===========================================================================
async function run() {
  await testActionsBarRendersWithInitialState();
  await testExportOpensDialogAndSavesThroughBackend();
  await testExportFailureKeepsDialogOpenWithReason();
  await testImportValidFileUploadsAndSummarises();
  await testImportInvalidJsonNotifiesAndDoesNotPost();
  await testArmedGatingDisablesImport();
  await testDestroyClosesUploadSse();

  // Let any best-effort microtasks drain so the process exits cleanly.
  await flushMicrotasks();
  console.log("all passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
