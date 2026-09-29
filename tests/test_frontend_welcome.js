"use strict";

/**
 * The first start setup (Corvus.welcome), on its pure seams.
 *
 * What the steps start from (the config where it says something, the screen
 * where it does not), and what Finish posts: exactly the keys the setup owns,
 * so the backend's per-key merge of `ui` and `updates` leaves every other
 * setting where it was. Plus the wiring that keeps the release check from
 * opening on top of the setup, and the plain-prose rule for its strings.
 *
 * Run:
 *   node tests/test_frontend_welcome.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

global.window = global;
global.Corvus = { ui: {} };
window.addEventListener = () => {};
window.dispatchEvent = () => true;
global.CustomEvent = class CustomEvent {
  constructor(type, o = {}) { this.type = type; this.detail = o.detail; }
};
global.Event = class Event { constructor(type) { this.type = type; } };
const store = {};
global.localStorage = {
  getItem: (k) => (k in store ? store[k] : null),
  setItem: (k, v) => { store[k] = String(v); },
  removeItem: (k) => { delete store[k]; },
};
const attrs = {};
const props = {};
global.document = {
  documentElement: {
    setAttribute: (k, v) => { attrs[k] = v; },
    getAttribute: (k) => (k in attrs ? attrs[k] : null),
    style: {
      setProperty: (k, v) => { props[k] = v; },
      getPropertyValue: (k) => (k in props ? props[k] : ""),
    },
    clientWidth: 1280,
  },
};
global.getComputedStyle = () => ({ getPropertyValue: (k) => (k in props ? props[k] : "") });

const SRC = path.join(__dirname, "..", "src");
require(path.join(SRC, "js", "units.js"));
require(path.join(SRC, "js", "sidenav.js"));
require(path.join(SRC, "js", "welcome.js"));

const W = Corvus.welcome;
const METRIC = { length: "m", distance: "km", speed: "ms", temperature: "c" };
const AVIATION = { length: "ft", distance: "nmi", speed: "kn", temperature: "c" };

// ---------------------------------------------------------------------------

function testTheConfigOutranksTheScreen() {
  const c = W.choicesFrom(
    { theme: { name: "blue" }, ui: { scale: 1.1, units: AVIATION, mission_page: true },
      review: { sensitivity: "strict" }, updates: { check: false } },
    { theme: "green", units: METRIC });
  assert.deepEqual(c, {
    theme: "blue", units: AVIATION, sensitivity: "strict", missionPage: true, updates: false,
  });
}

function testTheScreenFillsInWhatTheConfigLeavesOut() {
  const c = W.choicesFrom({}, { theme: "green", units: AVIATION });
  assert.deepEqual(c, {
    theme: "green", units: AVIATION, sensitivity: "normal", missionPage: false, updates: true,
  });
}

function testNothingKnownFallsBackToTheDefaults() {
  const c = W.choicesFrom(null, { theme: "no-such-theme" });
  assert.equal(c.theme, Corvus.theme.DEFAULT);
  assert.deepEqual(c.units, METRIC);
  assert.equal(c.sensitivity, "normal");
  assert.equal(c.updates, true);
}

function testAnUnknownSensitivityFallsBackToNormal() {
  const c = W.choicesFrom({ review: { sensitivity: "paranoid" } }, {});
  assert.equal(c.sensitivity, "normal");
}

function testTheSetupOffersTheSensitivitiesTheSettingsPageOffers() {
  // One list, owned by Settings > Analysis; the ids are the backend's.
  assert.deepEqual(Corvus.sidenav.REVIEW_SENSITIVITIES.map((s) => s.id),
    ["relaxed", "normal", "strict"]);
  const src = fs.readFileSync(path.join(SRC, "js", "welcome.js"), "utf8");
  assert.match(src, /Corvus\.sidenav\.REVIEW_SENSITIVITIES/);
  assert.ok(!/id: "relaxed"/.test(src), "welcome.js keeps its own copy of the list");
}

function testTheSensitivityCarriesTheCaution() {
  const src = fs.readFileSync(path.join(SRC, "js", "welcome.js"), "utf8");
  assert.match(src, /Errors in the analysis cannot be ruled out\./);
}

function testFinishPostsOnlyTheKeysTheSetupOwns() {
  const patch = W.configPatch({
    theme: "pink", units: AVIATION, sensitivity: "relaxed", missionPage: true, updates: false,
  });
  // No `ui.scale`: the interface size is not part of the setup, so Finish must
  // leave whatever size the station already has.
  assert.deepEqual(patch, {
    theme: { name: "pink" },
    ui: { units: AVIATION, mission_page: true },
    review: { sensitivity: "relaxed" },
    updates: { check: false },
  });
  // All four units every time: the backend replaces `ui.units` as one value.
  assert.deepEqual(Object.keys(patch.ui.units).sort(), ["distance", "length", "speed", "temperature"]);
}

async function testNothingOpensWhenTheSetupIsDone() {
  const asked = [];
  Corvus.telemetry = {
    requestJson: (url) => { asked.push(url); return Promise.resolve({ pending: false }); },
  };
  await W.maybeOpen();
  assert.deepEqual(asked, ["/api/welcome"]);
  assert.equal(W.isOpen(), false);
}

async function testAnUnreachableBackendOpensNothingAndStillResolves() {
  Corvus.telemetry = { requestJson: () => Promise.reject(new Error("offline")) };
  await W.maybeOpen();
  assert.equal(W.isOpen(), false);
}

function testTheReleaseCheckWaitsForTheSetup() {
  const app = fs.readFileSync(path.join(SRC, "js", "app.js"), "utf8");
  assert.match(app, /Corvus\.welcome\.maybeOpen\(\)\.then\(\(\) => Corvus\.update\.init\(\)\)/);
  assert.equal(app.match(/Corvus\.update\.init\(\)/g).length, 1,
    "the release check is scheduled somewhere else too");
}

function testTheScriptLoadsAfterWhatItUsesAndBeforeTheApp() {
  const html = fs.readFileSync(path.join(SRC, "index.html"), "utf8");
  const at = (name) => html.indexOf(`src="js/${name}"`);
  assert.ok(at("welcome.js") > 0, "welcome.js is not loaded");
  ["units.js", "ui.js", "telemetry.js", "settings-transfer.js", "sidenav.js"].forEach((dep) => {
    assert.ok(at(dep) >= 0 && at(dep) < at("app.js"), dep);
  });
  assert.ok(at("welcome.js") < at("app.js"));
}

function testItsTextIsPlainProse() {
  const src = fs.readFileSync(path.join(SRC, "js", "welcome.js"), "utf8");
  const strings = Array.from(src.matchAll(/"((?:[^"\\\n]|\\.)*)"/g)).map((m) => m[1]);
  assert.ok(strings.some((s) => s.includes("Set up this station")), "the scan found nothing");
  strings.forEach((s) => assert.ok(!/[–—]| - /.test(s), s));
}

// ---------------------------------------------------------------------------

const tests = [
  testTheConfigOutranksTheScreen,
  testTheScreenFillsInWhatTheConfigLeavesOut,
  testNothingKnownFallsBackToTheDefaults,
  testAnUnknownSensitivityFallsBackToNormal,
  testTheSetupOffersTheSensitivitiesTheSettingsPageOffers,
  testTheSensitivityCarriesTheCaution,
  testFinishPostsOnlyTheKeysTheSetupOwns,
  testNothingOpensWhenTheSetupIsDone,
  testAnUnreachableBackendOpensNothingAndStillResolves,
  testTheReleaseCheckWaitsForTheSetup,
  testTheScriptLoadsAfterWhatItUsesAndBeforeTheApp,
  testItsTextIsPlainProse,
];

(async function run() {
  let failed = 0;
  for (const t of tests) {
    try {
      await t();
      console.log(`ok   ${t.name}`);
    } catch (err) {
      failed += 1;
      console.log(`FAIL ${t.name}\n${err.stack || err}`);
    }
  }
  if (failed) {
    console.log(`${failed} of ${tests.length} failed`);
    process.exit(1);
  }
  console.log(`${tests.length} passed`);
})();
