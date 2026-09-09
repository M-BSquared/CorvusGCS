"use strict";

/**
 * Frontend tests for the update notice (Corvus.update).
 *
 * Same shape as tests/test_frontend_setup.js: stub the browser globals the
 * module touches, require the real source (ui.js first, for the modal it
 * builds its dialog from), and assert on the rendered DOM and the requests it
 * makes. No browser, no test runner.
 *
 * What matters here is when the dialog is NOT raised — a ground station must
 * not throw a modal over a flying aircraft, over a version the operator
 * dismissed, or because the field laptop has no internet.
 *
 * Run:
 *   node tests/test_frontend_update.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

window.addEventListener = () => {};
window.removeEventListener = () => {};
window.dispatchEvent = () => {};

// setTimeout is captured, never auto-fired: init() schedules the background
// check and the tests decide when it runs.
const timeouts = [];
window.setTimeout = (cb, ms) => { timeouts.push({ cb, ms }); return timeouts.length; };
window.clearTimeout = () => {};
function runTimeouts() {
  const pending = timeouts.splice(0, timeouts.length);
  pending.forEach((t) => t.cb());
}

// ---------------------------------------------------------------------------
// Minimal DOM stub (mirrors tests/test_frontend_setup.js).
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "",
    textContent: "",
    children: [],
    dataset: {},
    style: {
      _props: {},
      setProperty(k, v) { this._props[k] = String(v); },
      getPropertyValue(k) { return this._props[k] || ""; },
      removeProperty(k) { delete this._props[k]; },
    },
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
  e.append = (...cs) => cs.forEach((c) => e.appendChild(c));
  e.removeChild = (c) => {
    const i = e.children.indexOf(c);
    if (i >= 0) e.children.splice(i, 1);
    c.parentNode = null;
    return c;
  };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (type, cb) => { (e._listeners[type] = e._listeners[type] || []).push(cb); };
  e.removeEventListener = () => {};
  e.select = () => {};
  e.focus = () => {};
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  return e;
}

function querySel(children, sel) {
  const out = [];
  const first = String(sel).split(",")[0].trim();
  const wantTag = first && first[0] !== ".";
  const classes = first ? first.split(".").filter(Boolean) : [];
  function walk(list) {
    for (const c of list) {
      if (!c || !c._isEl) continue;
      const ok = wantTag
        ? c.tagName === first.toUpperCase()
        : classes.every((cl) => c.className.split(/\s+/).includes(cl));
      if (ok) out.push(c);
      if (c.children) walk(c.children);
    }
  }
  walk(children);
  return out;
}

// documentFragment behaves as a plain container: the module appends it into
// the modal body, and the stub's appendChild keeps it as one node — so the
// assertions walk through it exactly as they would a real subtree.
const body = makeEl("body");
global.document = {
  body,
  createElement: makeEl,
  createDocumentFragment: () => makeEl("fragment"),
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
  execCommand: () => true,
};

// Node defines `navigator` as a getter-only global, so it is replaced rather
// than assigned. Left without a clipboard API by default, which exercises the
// execCommand fallback — the path QtWebEngine takes without clipboard
// permission. A test that wants the modern API sets navigator.clipboard.
Object.defineProperty(global, "navigator", {
  value: {}, writable: true, configurable: true,
});

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------
function flush() { return new Promise((r) => setImmediate(r)); }

function findByClass(root, cls) { return root.querySelectorAll("." + cls); }

function textOf(el) {
  if (!el) return "";
  let out = String(el.textContent || "");
  (el.children || []).forEach((c) => { out += textOf(c); });
  return out;
}

function buttonByLabel(root, text) {
  return findByClass(root, "btn").filter((b) => textOf(b).includes(text))[0];
}

function fire(el, type, detail) {
  const listeners = (el && el._listeners && el._listeners[type]) || [];
  const event = Object.assign({ preventDefault() {}, stopPropagation() {} }, detail || {});
  listeners.forEach((cb) => cb(event));
}

/** The mounted modal overlay, or null when no dialog is open. */
function openDialog() {
  return findByClass(body, "modal-overlay")[0] || null;
}

function clearBody() { body.children.length = 0; }

const NEWER = {
  enabled: true,
  skipped: "",
  current: "2000.09.29",
  latest: "2000.10.02",
  update_available: true,
  name: "Corvus GCS v2000.10.02",
  url: "https://github.com/M-BSquared/CorvusGCS/releases/tag/v2000.10.02",
  published: "2026-10-02T09:00:00Z",
  notes: "* Faster map\n* Fixed a thing",
  assets: [],
  checked_at: 1,
  error: "",
};

const CURRENT = Object.assign({}, NEWER, {
  latest: "2000.09.29", update_available: false, notes: "",
});

/**
 * Fake telemetry: one response per URL prefix, plus a settable armed state.
 * Records every request so the tests can assert what was (and was not) asked.
 */
function makeFakeTelemetry(status) {
  const requests = [];
  const posts = [];
  let state = { armed: false };
  let subCb = null;
  let response = status;
  let openResponse = { ok: true, url: NEWER.url };

  Corvus.telemetry = {
    requestJson(url, options) {
      requests.push({ url, options });
      if (String(url).startsWith("/api/update/open")) {
        return openResponse instanceof Error
          ? Promise.reject(openResponse) : Promise.resolve(openResponse);
      }
      return response instanceof Error
        ? Promise.reject(response) : Promise.resolve(response);
    },
    postAction(url, payload) {
      posts.push({ url, payload });
      return Promise.resolve({ ok: true });
    },
    getState() { return state; },
    subscribe(fn) { subCb = fn; return () => { subCb = null; }; },
  };

  return {
    requests, posts,
    setState(s) { state = s; },
    setResponse(r) { response = r; },
    setOpenResponse(r) { openResponse = r; },
    emitState(s) { state = s; if (subCb) subCb(s); },
    hasSubscriber: () => subCb !== null,
  };
}

// ---------------------------------------------------------------------------
// Load the module under test. ui.js first — it defines Corvus.ui, the
// component layer the dialog is built from (same order as index.html).
// ---------------------------------------------------------------------------
require("../src/js/ui.js");
require("../src/js/update.js");

// ===========================================================================
// Tests
// ===========================================================================

async function testDialogShownForNewerVersion() {
  clearBody();
  const fake = makeFakeTelemetry(NEWER);

  await Corvus.update.check();
  await flush();

  const dialog = openDialog();
  assert.ok(dialog, "a newer release raises the dialog");
  const text = textOf(dialog);
  assert.ok(text.includes("2000.09.29"), "the installed version is shown");
  assert.ok(text.includes("2000.10.02"), "the available version is shown");
  assert.ok(text.includes("Faster map"), "the release notes are shown");
  assert.ok(text.includes(NEWER.url), "the release URL is readable, not only linked");

  // The check must not carry a refresh: the automatic path uses the backend's
  // cache so a page reload never costs a network round trip.
  assert.equal(fake.requests[0].url, "/api/update", "automatic check does not force a refresh");

  Corvus.update.close();
  assert.equal(openDialog(), null, "close() unmounts the dialog");
}

async function testNoDialogWhenUpToDate() {
  clearBody();
  makeFakeTelemetry(CURRENT);

  const status = await Corvus.update.check();
  await flush();

  assert.equal(openDialog(), null, "no dialog when the running version is current");
  assert.equal(status.update_available, false);
}

async function testSkippedVersionIsSilent() {
  clearBody();
  const skipped = Object.assign({}, NEWER, { skipped: "2000.10.02" });
  makeFakeTelemetry(skipped);

  await Corvus.update.check();
  await flush();
  assert.equal(openDialog(), null, "a dismissed version never prompts again");

  // …but an explicit "Check now" still shows it, otherwise the operator has no
  // way back to a release they skipped earlier.
  await Corvus.update.check({ manual: true, refresh: true });
  await flush();
  assert.ok(openDialog(), "a manual check overrides the dismissal");
  Corvus.update.close();
}

async function testDisabledCheckNeverPrompts() {
  clearBody();
  makeFakeTelemetry(Object.assign({}, NEWER, { enabled: false, update_available: false }));

  await Corvus.update.check();
  await flush();
  assert.equal(openDialog(), null, "the switched-off check raises nothing");
}

async function testArmedVehicleDefersTheDialog() {
  clearBody();
  const fake = makeFakeTelemetry(NEWER);
  fake.setState({ armed: true });

  await Corvus.update.check();
  await flush();

  assert.equal(openDialog(), null, "no dialog over an armed vehicle");
  assert.ok(fake.hasSubscriber(), "the notice waits on a telemetry subscription");

  // Still armed: the notice stays held.
  fake.emitState({ armed: true });
  await flush();
  assert.equal(openDialog(), null, "still nothing while armed");

  fake.emitState({ armed: false });
  await flush();
  assert.ok(openDialog(), "the dialog appears once the vehicle disarms");
  assert.equal(fake.hasSubscriber(), false, "the subscription is dropped after it fires");
  Corvus.update.close();
}

async function testOfflineCheckIsSilent() {
  clearBody();
  makeFakeTelemetry(new Error("Network request failed"));

  const status = await Corvus.update.check();
  await flush();

  assert.equal(status, null, "an unreachable backend resolves to null, never throws");
  assert.equal(openDialog(), null, "and shows nothing");

  // A manual check is allowed to fail loudly — the operator asked.
  await assert.rejects(() => Corvus.update.check({ manual: true }),
    "a manual check surfaces the error");
}

async function testSkipButtonPersistsTheVersion() {
  clearBody();
  const fake = makeFakeTelemetry(NEWER);

  await Corvus.update.check();
  await flush();
  const dialog = openDialog();
  fire(buttonByLabel(dialog, "Skip this version"), "click");
  await flush();

  assert.deepEqual(fake.posts[0], {
    url: "/api/update/skip", payload: { version: "2000.10.02" },
  }, "the dismissal is persisted for the exact version shown");
  assert.equal(openDialog(), null, "and the dialog closes");
}

async function testLaterButtonPersistsNothing() {
  clearBody();
  const fake = makeFakeTelemetry(NEWER);

  await Corvus.update.check();
  await flush();
  fire(buttonByLabel(openDialog(), "Later"), "click");
  await flush();

  assert.equal(fake.posts.length, 0, "\"Later\" writes no config");
  assert.equal(openDialog(), null, "but does close the dialog");
}

async function testOpenReleasePageGoesThroughTheBackend() {
  clearBody();
  const fake = makeFakeTelemetry(NEWER);

  await Corvus.update.check();
  await flush();
  const dialog = openDialog();
  fire(buttonByLabel(dialog, "Open release"), "click");
  await flush();

  const opened = fake.requests.filter((r) => r.url === "/api/update/open");
  assert.equal(opened.length, 1, "the release page is opened server-side");
  assert.equal(opened[0].options.method, "POST");
  // No URL in the request: the backend derives it, so the button can never be
  // turned into a redirect out of the app.
  assert.equal(opened[0].options.body, "{}", "the request carries no URL");
  Corvus.update.close();
}

async function testNoBrowserFallsBackToCopy() {
  clearBody();
  const fake = makeFakeTelemetry(NEWER);
  fake.setOpenResponse({ ok: false, error: "no browser available", url: NEWER.url });

  await Corvus.update.check();
  await flush();
  const dialog = openDialog();
  fire(buttonByLabel(dialog, "Open release"), "click");
  await flush();
  await flush();

  const msg = findByClass(dialog, "update-msg")[0];
  assert.ok(msg, "the dialog carries a status line");
  assert.ok(/clipboard/i.test(textOf(msg)),
    `no browser falls back to the clipboard, got: ${textOf(msg)}`);
  Corvus.update.close();
}

async function testInitDefersTheCheck() {
  clearBody();
  const fake = makeFakeTelemetry(NEWER);

  Corvus.update.init();
  assert.equal(fake.requests.length, 0, "init() fires no request of its own");
  assert.ok(timeouts.length >= 1, "it schedules the check instead");
  assert.ok(timeouts[timeouts.length - 1].ms >= 1000,
    "and leaves the UI time to settle first");

  runTimeouts();
  await flush();
  assert.equal(fake.requests[0].url, "/api/update", "the scheduled check then runs");
  Corvus.update.close();
}

// ---------------------------------------------------------------------------

async function main() {
  const tests = [
    testDialogShownForNewerVersion,
    testNoDialogWhenUpToDate,
    testSkippedVersionIsSilent,
    testDisabledCheckNeverPrompts,
    testArmedVehicleDefersTheDialog,
    testOfflineCheckIsSilent,
    testSkipButtonPersistsTheVersion,
    testLaterButtonPersistsNothing,
    testOpenReleasePageGoesThroughTheBackend,
    testNoBrowserFallsBackToCopy,
    testInitDefersTheCheck,
  ];
  for (const t of tests) {
    await t();
    console.log(`  ok  ${t.name}`);
  }
  console.log(`\n${tests.length} update-notice assertions passed`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
