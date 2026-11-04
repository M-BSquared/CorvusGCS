"use strict";

/**
 * The Logs page's export.
 *
 * The Logs page used to be read-only, and what it showed it scraped back out
 * of #consoleOutput — the DOM the CONSOLE tab renders THROUGH ITS FILTER. Now
 * that the page writes the transcript to a file, that would have been a quiet
 * data loss: a filter typed into a tab the operator has since left would
 * shorten the exported file, with nothing on the page saying so. So the
 * export reads Corvus.panel's buffer instead, and asks for all of it.
 *
 * Asserted here on the pure seam (Corvus.panel.transcript): records in, text
 * out, no buffer and no DOM. The page wiring around it — the button, the busy
 * state, the written path under it — is DOM work covered by hand.
 *
 * Run:
 *   node tests/test_frontend_logs_export.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

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

const { transcript } = Corvus.panel;

const RECORDS = [
  { ts: "00:00:01.000", level: "info", text: "EKF2 lane switch" },
  { ts: "00:00:02.000", level: "error", text: "GPS fix lost" },
  { ts: "00:00:03.000", level: "", text: "Corvus GCS — MAVLink console." },
];

// ---------------------------------------------------------------------------
// An export is the whole buffer
// ---------------------------------------------------------------------------

function testAllIgnoresTheFilter() {
  // The bug this page was one edit away from: exporting what the CONSOLE tab
  // happens to be showing rather than what the link actually said.
  const text = transcript(RECORDS, { all: true, filter: "gps" });
  assert.equal(text.split("\n").length, RECORDS.length);
  assert.ok(text.includes("EKF2 lane switch"));
  assert.ok(text.includes("Corvus GCS"));
}

function testFilterStillAppliesWithoutAll() {
  // Copy and Save in the CONSOLE tab still mean "what is on screen".
  const text = transcript(RECORDS, { filter: "gps" });
  assert.equal(text, "00:00:02.000  [error] GPS fix lost");
}

function testNoFilterIsEveryLine() {
  assert.equal(transcript(RECORDS, {}).split("\n").length, RECORDS.length);
  assert.equal(transcript(RECORDS).split("\n").length, RECORDS.length);
}

// ---------------------------------------------------------------------------
// What a line looks like in the file
// ---------------------------------------------------------------------------

function testLineCarriesTimestampAndLevel() {
  // Someone reconstructing a flight from the file needs the severity that
  // colour carries on screen — the file has no colour.
  const lines = transcript(RECORDS, { all: true }).split("\n");
  assert.equal(lines[0], "00:00:01.000  [info] EKF2 lane switch");
  assert.equal(lines[1], "00:00:02.000  [error] GPS fix lost");
}

function testUntaggedLineHasNoEmptyBrackets() {
  const lines = transcript(RECORDS, { all: true }).split("\n");
  assert.equal(lines[2], "00:00:03.000  Corvus GCS — MAVLink console.");
}

function testEmptyBufferIsEmptyText() {
  // What the page's disabled Export button is gated on, and what the export
  // itself refuses rather than writing a file with nothing in it.
  assert.equal(transcript([], { all: true }), "");
  assert.equal(transcript(null, { all: true }), "");
}

// ---------------------------------------------------------------------------
// The page reads the buffer, not the DOM
// ---------------------------------------------------------------------------

function testLogsPageDoesNotScrapeTheConsoleDom() {
  // A regression guard with teeth: re-scraping #consoleOutput would pass every
  // assertion above and still export a filtered, 30-line-deep transcript,
  // because the DOM holds only what the filter let through.
  const src = fs.readFileSync(
    path.join(__dirname, "..", "src", "js", "sidenav.js"), "utf8");
  const page = src.slice(src.indexOf("function renderLogsPage"));
  assert.ok(!page.slice(0, page.indexOf("function exportLogFile")).includes("consoleOutput"),
    "the Logs page must read Corvus.panel's buffer, not #consoleOutput");
  assert.ok(src.includes("consoleText({ all: true })"),
    "the Logs page must ask for the whole buffer");
}

const tests = [
  testAllIgnoresTheFilter,
  testFilterStillAppliesWithoutAll,
  testNoFilterIsEveryLine,
  testLineCarriesTimestampAndLevel,
  testUntaggedLineHasNoEmptyBrackets,
  testEmptyBufferIsEmptyText,
  testLogsPageDoesNotScrapeTheConsoleDom,
];

let failed = 0;
for (const t of tests) {
  try { t(); console.log("ok   - " + t.name); }
  catch (e) { failed++; console.error("FAIL - " + t.name + "\n      " + (e && e.message)); }
}
if (failed) { console.error(`\n${failed}/${tests.length} logs-export test(s) FAILED`); process.exit(1); }
console.log(`\nAll ${tests.length} logs-export tests passed.`);
