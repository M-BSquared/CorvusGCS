"use strict";

/**
 * Settings export and import, the browser half (Corvus.settingsTransfer).
 *
 * Asserted on the pure seams: which localStorage keys are settings and which
 * part of an import they belong to, collecting them, applying a file's copy
 * of them, and reading a file. Plus the guard that matters most over time: a
 * module that starts keeping a new setting in localStorage has to register it
 * in BROWSER_KEYS (or say it is not a setting), or an export would quietly
 * leave it behind.
 *
 * Run:
 *   node tests/test_frontend_settings_transfer.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, o = {}) { this.type = type; this.detail = o.detail; }
};

require("./../src/js/settings-transfer.js");

const T = Corvus.settingsTransfer;

/** A Storage with the parts of the Web Storage API the module uses. */
function fakeStorage(entries) {
  const map = new Map(Object.entries(entries || {}));
  return {
    get length() { return map.size; },
    key(i) { return Array.from(map.keys())[i] ?? null; },
    getItem(k) { return map.has(k) ? map.get(k) : null; },
    setItem(k, v) { map.set(k, String(v)); },
    removeItem(k) { map.delete(k); },
    dump() { return Object.fromEntries(map); },
  };
}

// ---------------------------------------------------------------------------

function testTheHudPositionIsWindowLayout() {
  assert.equal(T.sectionOfKey("corvus.hud"), "layout");
  assert.equal(T.sectionOfKey("corvus.joystick"), "layout");
  assert.equal(T.sectionOfKey("corvus.theme"), "interface");
}

function testPrefixKeysMatchTheirWholeFamily() {
  assert.equal(T.sectionOfKey("corvus.analysis.sort.ulog"), "layout");
  assert.equal(T.sectionOfKey("corvus.analysis.sortx"), "");
}

function testHistoryIsNotASetting() {
  T.NOT_SETTINGS.forEach((k) => assert.equal(T.sectionOfKey(k), ""));
  assert.equal(T.sectionOfKey("somebody.else"), "");
}

function testCollectTakesOnlySettings() {
  const store = fakeStorage({
    "corvus.hud": '{"x":12,"y":34}',
    "corvus.console.history": '["arm"]',
    "unrelated": "1",
  });
  assert.deepEqual(T.collectBrowser(store), { "corvus.hud": '{"x":12,"y":34}' });
}

function testApplyReplacesOnlyTheChosenParts() {
  const store = fakeStorage({
    "corvus.hud": '{"x":1}',
    "corvus.joystick": '{"x":5}',
    "corvus.theme": "dark",
    "corvus.console.history": '["arm"]',
  });
  T.applyBrowser(store, { "corvus.hud": '{"x":400,"y":20}', "corvus.theme": "light" }, ["layout"]);
  const after = store.dump();
  assert.equal(after["corvus.hud"], '{"x":400,"y":20}');
  // In a chosen part but not in the file: back to its default, as on the
  // station that wrote the file.
  assert.equal(after["corvus.joystick"], undefined);
  // Parts not chosen, and keys that are not settings, stay as they were.
  assert.equal(after["corvus.theme"], "dark");
  assert.equal(after["corvus.console.history"], '["arm"]');
}

function testApplyIgnoresKeysThatAreNotSettings() {
  const store = fakeStorage({});
  T.applyBrowser(store, { "corvus.console.history": "x", "evil": "y" }, ["layout", "interface"]);
  assert.deepEqual(store.dump(), {});
}

function testApplySurvivesAStorageThatThrows() {
  const broken = { get length() { throw new Error("denied"); } };
  T.applyBrowser(broken, { "corvus.hud": "{}" }, ["layout"]);
  assert.deepEqual(T.collectBrowser(broken), {});
}

function testParseRejectsWhatIsNotASettingsFile() {
  assert.throws(() => T.parseBundle("not json"), /not a Corvus GCS settings file/);
  assert.throws(() => T.parseBundle('{"kind":"x","config":{}}'), /not a Corvus GCS settings file/);
  assert.throws(() => T.parseBundle('{"kind":"corvus-settings"}'), /not a Corvus GCS settings file/);
  const ok = T.parseBundle('{"kind":"corvus-settings","format":1,"config":{}}');
  assert.equal(ok.format, 1);
}

function testEmptyPartsAreReported() {
  assert.deepEqual(T.emptySections({ config: {}, plugins: {}, browser: {} }).sort(),
    ["layout", "plugins"]);
  assert.deepEqual(T.emptySections({
    config: {}, plugins: { demo: {} }, browser: { "corvus.hud": "{}" },
  }), []);
}

function testSectionsMatchTheBackend() {
  const py = fs.readFileSync(
    path.join(__dirname, "..", "corvus", "settings_bundle.py"), "utf8");
  const block = py.slice(py.indexOf("SECTIONS: dict"), py.indexOf("EXCLUDED_KEYS"));
  const backend = Array.from(block.matchAll(/^\s+"([a-z]+)": \(/gm)).map((m) => m[1]).sort();
  assert.deepEqual(T.SECTIONS.map((s) => s.id).sort(), backend);
  T.BROWSER_KEYS.forEach((e) => assert.ok(backend.includes(e.section), e.key));
}

function testEveryStoredKeyIsAccountedFor() {
  // A key a module keeps in localStorage is either exported or deliberately
  // not a setting. Anything else is a setting an export would lose.
  const dir = path.join(__dirname, "..", "src", "js");
  const sources = fs.readdirSync(dir).filter((f) => f.endsWith(".js"))
    .map((f) => fs.readFileSync(path.join(dir, f), "utf8"));
  sources.push(fs.readFileSync(path.join(__dirname, "..", "src", "index.html"), "utf8"));
  const found = new Set();
  const patterns = [
    /KEY(?:_PREFIX)?\s*=\s*"(corvus\.[^"]+)"/g,
    /localStorage\.(?:getItem|setItem|removeItem)\(\s*"(corvus\.[^"]+)"/g,
  ];
  sources.forEach((src) => patterns.forEach((re) => {
    for (const m of src.matchAll(re)) found.add(m[1]);
  }));
  assert.ok(found.has("corvus.hud"), "the scan found nothing; the pattern is broken");
  const known = new Set(T.BROWSER_KEYS.map((e) => e.key).concat(T.NOT_SETTINGS));
  const missing = Array.from(found).filter((k) => !known.has(k));
  assert.deepEqual(missing, [],
    "add these to BROWSER_KEYS or NOT_SETTINGS in src/js/settings-transfer.js");
}

// ---------------------------------------------------------------------------

const tests = [
  testTheHudPositionIsWindowLayout,
  testPrefixKeysMatchTheirWholeFamily,
  testHistoryIsNotASetting,
  testCollectTakesOnlySettings,
  testApplyReplacesOnlyTheChosenParts,
  testApplyIgnoresKeysThatAreNotSettings,
  testApplySurvivesAStorageThatThrows,
  testParseRejectsWhatIsNotASettingsFile,
  testEmptyPartsAreReported,
  testSectionsMatchTheBackend,
  testEveryStoredKeyIsAccountedFor,
];

let failed = 0;
for (const t of tests) {
  try {
    t();
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
