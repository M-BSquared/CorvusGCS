"use strict";

/**
 * Frontend tests for the MAVLink console's pure logic (Corvus.panel).
 *
 * The console is the operator's direct line to the airframe, and its filter is
 * the part that can quietly lie: a severity filter that hides errors while
 * claiming to show warnings, or a substring match that drops the line someone
 * was looking for, is worse than no filter at all. Those rules are pure
 * functions, so they are asserted here without a DOM.
 *
 * Run:
 *   node tests/test_frontend_console.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, o = {}) { this.type = type; this.detail = o.detail; }
};
window.addEventListener = () => {};
window.removeEventListener = () => {};
global.localStorage = { getItem: () => null, setItem: () => {}, removeItem: () => {} };
global.document = {
  createElement: () => ({
    className: "", textContent: "", children: [], dataset: {}, style: {},
    classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } },
    appendChild() {}, setAttribute() {}, addEventListener() {},
  }),
  createTextNode: () => ({}),
  getElementById: () => null,
  querySelector: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
};

require("./../src/js/ui.js");
require("./../src/js/panel.js");

const { matchesFilter, completionsFor, COMMANDS } = Corvus.panel;
const rec = (level, text) => ({ ts: "00:00:00.000", level, text });

// ---------------------------------------------------------------------------
// Severity is a floor, not an equality test
// ---------------------------------------------------------------------------

function testAllShowsEverything() {
  ["", "info", "warning", "error", "cmd", "shell", "success"].forEach((lvl) => {
    assert.ok(matchesFilter(rec(lvl, "anything"), "", "all"),
      `level ${lvl || "(untagged)"} must survive the ALL filter`);
  });
}

function testErrorFilterHidesEverythingBelowIt() {
  assert.ok(matchesFilter(rec("error", "boom"), "", "error"));
  assert.ok(!matchesFilter(rec("warning", "hmm"), "", "error"));
  assert.ok(!matchesFilter(rec("info", "fyi"), "", "error"));
  assert.ok(!matchesFilter(rec("", "banner"), "", "error"));
}

function testWarningFilterKeepsErrors() {
  // The rule that matters: asking for warnings must not hide the errors that
  // followed them, or the filter actively misleads.
  assert.ok(matchesFilter(rec("warning", "hmm"), "", "warning"));
  assert.ok(matchesFilter(rec("error", "boom"), "", "warning"),
    "an error must survive the WARN filter");
  assert.ok(!matchesFilter(rec("info", "fyi"), "", "warning"));
}

function testUntaggedLinesReadAsInfo() {
  // The app's own banners and command echoes carry no level; they must not
  // vanish at the INFO setting.
  assert.ok(matchesFilter(rec("", "Corvus GCS — MAVLink console."), "", "info"));
  assert.ok(matchesFilter(rec("cmd", "arm"), "", "info"));
}

// ---------------------------------------------------------------------------
// Substring matching
// ---------------------------------------------------------------------------

function testSubstringIsCaseInsensitive() {
  // The caller lowercases the needle; the haystack is lowercased here.
  assert.ok(matchesFilter(rec("info", "EKF2 lane switch"), "ekf2", "all"));
  assert.ok(matchesFilter(rec("info", "ekf2 lane switch"), "lane", "all"));
  assert.ok(!matchesFilter(rec("info", "EKF2 lane switch"), "baro", "all"));
}

function testEmptyFilterMatchesEverything() {
  assert.ok(matchesFilter(rec("info", "anything at all"), "", "all"));
}

function testFilterAndSeverityCombine() {
  const r = rec("error", "GPS fix lost");
  assert.ok(matchesFilter(r, "gps", "error"), "both conditions met");
  assert.ok(!matchesFilter(r, "baro", "error"), "text misses");
  assert.ok(!matchesFilter(rec("info", "GPS fix acquired"), "gps", "error"), "level misses");
}

function testNonStringTextDoesNotThrow() {
  // Records come from a JSON stream; a numeric or null text must not take the
  // console down with it.
  assert.doesNotThrow(() => matchesFilter({ level: "info", text: 42 }, "4", "all"));
  assert.doesNotThrow(() => matchesFilter({ level: "info", text: null }, "x", "all"));
  assert.ok(!matchesFilter({ level: "info", text: null }, "x", "all"));
}

// ---------------------------------------------------------------------------
// Tab completion
// ---------------------------------------------------------------------------

function testCompletionMatchesPrefixes() {
  const names = completionsFor("lis").map((c) => c.name);
  assert.deepEqual(names, ["listener"]);
}

function testCompletionCanBeAmbiguous() {
  // Several PX4 shell commands start with the same letters; the caller lists
  // them rather than guessing.
  const names = completionsFor("t").map((c) => c.name);
  assert.ok(names.length > 1, `expected several matches for "t", got ${names}`);
  assert.ok(names.includes("takeoff") && names.includes("top"));
}

function testCompletionExcludesAnExactMatch() {
  // Nothing left to complete — offering "arm" while "arm" is typed is noise.
  assert.deepEqual(completionsFor("arm").map((c) => c.name), []);
}

function testCompletionIgnoresArguments() {
  // Only the command token is completed; "mode HOLD" is already past that.
  assert.deepEqual(completionsFor("mode HOLD").map((c) => c.name), []);
}

function testEmptyInputCompletesNothing() {
  assert.deepEqual(completionsFor(""), []);
  assert.deepEqual(completionsFor("   "), []);
}

// ---------------------------------------------------------------------------
// The command table
// ---------------------------------------------------------------------------

function testEveryCommandIsDocumented() {
  assert.ok(COMMANDS.length > 0);
  COMMANDS.forEach((c) => {
    assert.ok(c.name && typeof c.name === "string", "command needs a name");
    assert.ok(c.help && c.help.length > 5, `${c.name}: needs a real help string`);
    assert.equal(typeof c.args, "string", `${c.name}: args must be a string ("" when none)`);
  });
}

function testCommandTableCoversTheBackendVerbs() {
  // The table drives the "?" listing and Tab completion, so a verb the backend
  // accepts but the table omits is invisible to the operator.
  const names = COMMANDS.map((c) => c.name);
  ["arm", "disarm", "mode", "takeoff", "land", "rtl", "shell", "help",
   "listener", "top", "free", "dmesg", "tasks", "perf", "boot_log", "hrt"]
    .forEach((verb) => assert.ok(names.includes(verb), `command table is missing "${verb}"`));
}

function testCommandNamesAreUnique() {
  const names = COMMANDS.map((c) => c.name);
  assert.equal(new Set(names).size, names.length, "duplicate command in the table");
}

const tests = [
  testAllShowsEverything,
  testErrorFilterHidesEverythingBelowIt,
  testWarningFilterKeepsErrors,
  testUntaggedLinesReadAsInfo,
  testSubstringIsCaseInsensitive,
  testEmptyFilterMatchesEverything,
  testFilterAndSeverityCombine,
  testNonStringTextDoesNotThrow,
  testCompletionMatchesPrefixes,
  testCompletionCanBeAmbiguous,
  testCompletionExcludesAnExactMatch,
  testCompletionIgnoresArguments,
  testEmptyInputCompletesNothing,
  testEveryCommandIsDocumented,
  testCommandTableCoversTheBackendVerbs,
  testCommandNamesAreUnique,
];

let failed = 0;
for (const t of tests) {
  try { t(); console.log("ok   - " + t.name); }
  catch (e) { failed++; console.error("FAIL - " + t.name + "\n      " + (e && e.message)); }
}
if (failed) { console.error(`\n${failed}/${tests.length} console test(s) FAILED`); process.exit(1); }
console.log(`\nAll ${tests.length} console tests passed.`);
