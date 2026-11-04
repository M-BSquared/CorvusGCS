"use strict";

/**
 * Frontend tests for the MAVLink console's pure logic (Corvus.panel).
 *
 * The console is the operator's direct line to the airframe, and its filter is
 * the part that can quietly lie: a substring match that drops the line someone
 * was looking for is worse than no filter at all. There is no longer a
 * severity filter — every line reaches the stream and colour carries the
 * level — so the level->class mapping is asserted here too: it is now the only
 * thing that tells an error from an info line. Both rules are pure functions,
 * so they are checked without a DOM.
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

const { matchesFilter, levelClass, completionsFor, COMMANDS } = Corvus.panel;
const rec = (level, text) => ({ ts: "00:00:00.000", level, text });

// ---------------------------------------------------------------------------
// No severity filter: every level reaches the stream
// ---------------------------------------------------------------------------

function testEveryLevelSurvivesTheFilter() {
  // The whole point of dropping the ALL/INFO/WARN/ERR chips: no level is ever
  // withheld, so the context around an error is still on screen.
  ["", "info", "warning", "error", "critical", "cmd", "shell", "success", "nav"]
    .forEach((lvl) => {
      assert.ok(matchesFilter(rec(lvl, "anything"), ""),
        `level ${lvl || "(untagged)"} must reach the stream`);
    });
}

function testSeverityIsNotAFilterArgument() {
  // A leftover third argument from the old severity filter must not resurrect
  // level filtering by accident.
  assert.ok(matchesFilter(rec("info", "fyi"), "", "error"));
  assert.ok(matchesFilter(rec("", "banner"), "", "error"));
}

// ---------------------------------------------------------------------------
// Colour is the level: the mapping is the whole distinction now
// ---------------------------------------------------------------------------

function testCriticalSharesTheErrorColour() {
  // The bridge sends "critical" for STATUSTEXT severity <= 3. Without this
  // mapping an emergency renders in the default text colour — the one case
  // that must never look ordinary.
  assert.equal(levelClass("critical"), "error");
  assert.equal(levelClass("error"), "error");
}

function testEveryBackendLevelHasAClass() {
  // Levels the bridge actually publishes (see corvus/mavlink_bridge.py) plus
  // the two the frontend tags itself.
  ["critical", "error", "warning", "success", "info", "cmd", "shell"]
    .forEach((lvl) => {
      assert.ok(levelClass(lvl), `level "${lvl}" must map to a CSS class`);
    });
}

function testUnknownAndUntaggedLevelsRenderPlain() {
  // An unknown level from a future backend must not leak a raw class name
  // into the DOM; untagged app banners stay plain.
  assert.equal(levelClass(""), "");
  assert.equal(levelClass(undefined), "");
  assert.equal(levelClass("emergency-mk2"), "");
}

// ---------------------------------------------------------------------------
// Substring matching
// ---------------------------------------------------------------------------

function testSubstringIsCaseInsensitive() {
  // The caller lowercases the needle; the haystack is lowercased here.
  assert.ok(matchesFilter(rec("info", "EKF2 lane switch"), "ekf2"));
  assert.ok(matchesFilter(rec("info", "ekf2 lane switch"), "lane"));
  assert.ok(!matchesFilter(rec("info", "EKF2 lane switch"), "baro"));
}

function testEmptyFilterMatchesEverything() {
  assert.ok(matchesFilter(rec("info", "anything at all"), ""));
}

function testTextFilterIgnoresLevel() {
  // The substring is the only thing that hides a line, whatever its severity.
  assert.ok(matchesFilter(rec("error", "GPS fix lost"), "gps"));
  assert.ok(!matchesFilter(rec("error", "GPS fix lost"), "baro"));
  assert.ok(matchesFilter(rec("info", "GPS fix acquired"), "gps"));
}

function testNonStringTextDoesNotThrow() {
  // Records come from a JSON stream; a numeric or null text must not take the
  // console down with it.
  assert.doesNotThrow(() => matchesFilter({ level: "info", text: 42 }, "4"));
  assert.doesNotThrow(() => matchesFilter({ level: "info", text: null }, "x"));
  assert.ok(!matchesFilter({ level: "info", text: null }, "x"));
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
  testEveryLevelSurvivesTheFilter,
  testSeverityIsNotAFilterArgument,
  testCriticalSharesTheErrorColour,
  testEveryBackendLevelHasAClass,
  testUnknownAndUntaggedLevelsRenderPlain,
  testSubstringIsCaseInsensitive,
  testEmptyFilterMatchesEverything,
  testTextFilterIgnoresLevel,
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
