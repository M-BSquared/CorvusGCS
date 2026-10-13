"use strict";

/**
 * The takeoff altitude can be typed as well as dragged. What the box holds is
 * in the display unit, what the backend takes is metres between 1 and 120, so
 * the box is read through altitudeFromEntry: converted, clamped to that range,
 * and null while it holds nothing a takeoff could use.
 *
 * Enter in the box confirms the takeoff, but only from a number that stands as
 * typed: entryIsClear is false for an empty box and for one the input itself
 * calls out of range or unreadable, so a typed 500 is corrected first rather
 * than flown as 120.
 *
 * The slider is logarithmic: its own number is a position from 0 to 1000, and
 * altitudeToPosition / positionToAltitude convert, snapping to whole metres up
 * to 50 m and to 5 m above. nudgeAltitude is what an arrow key does, because
 * the slider's own key step would be a thousandth of the track.
 *
 * Run:
 *   node tests/test_frontend_takeoff_entry.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
global.document = { addEventListener: () => {}, getElementById: () => null };

require("./../src/js/app.js");
const { altitudeFromEntry, entryIsClear, altitudeToPosition, positionToAltitude, nudgeAltitude } = Corvus.app;

const MIN = 1;
const MAX = 120;
const SPAN = 1000;
const toAlt = (position) => positionToAltitude(position, MIN, MAX, SPAN);
const toPos = (metres) => altitudeToPosition(metres, MIN, MAX, SPAN);
const nudge = (metres, direction) => nudgeAltitude(metres, direction, MIN, MAX);

const metres = (v) => Number(v);
const feet = (v) => Number(v) / 3.28084;

function testATypedNumberIsTheAltitude() {
  assert.equal(altitudeFromEntry("25", metres, 1, 120), 25);
  assert.equal(altitudeFromEntry(" 7.5 ", metres, 1, 120), 7.5);
}

function testTheDisplayUnitIsConvertedToMetres() {
  assert.equal(altitudeFromEntry("100", feet, 1, 120), 30.48);
}

function testAnAltitudeOutsideTheRangeIsClamped() {
  assert.equal(altitudeFromEntry("500", metres, 1, 120), 120);
  assert.equal(altitudeFromEntry("0", metres, 1, 120), 1);
  assert.equal(altitudeFromEntry("-5", metres, 1, 120), 1);
  assert.equal(altitudeFromEntry("1000", feet, 1, 120), 120, "the cap is in metres, not in the shown unit");
}

function testNothingUsableIsNotAnAltitude() {
  // An emptied box is not a zero: it would clamp to 1 m and move the slider
  // under an operator who is only retyping.
  assert.equal(altitudeFromEntry("", metres, 1, 120), null);
  assert.equal(altitudeFromEntry("   ", metres, 1, 120), null);
  assert.equal(altitudeFromEntry("abc", metres, 1, 120), null);
  assert.equal(altitudeFromEntry("Infinity", metres, 1, 120), null);
}

function testTheSliderSpansTheWholeRange() {
  assert.equal(toAlt(0), MIN);
  assert.equal(toAlt(SPAN), MAX);
  assert.equal(toPos(MIN), 0);
  assert.equal(toPos(MAX), SPAN);
}

function testTheSliderIsLogarithmic() {
  // The geometric middle of 1 and 120 sits halfway along, not 60 m.
  assert.equal(toAlt(SPAN / 2), 11);
  assert.ok(toPos(2) - toPos(1) > toPos(41) - toPos(40),
    "a metre near the ground takes more track than a metre at 40 m");
  assert.ok(toPos(10) > toPos(5) && toPos(5) > toPos(2));
}

function testTheSliderNeverGoesBackwards() {
  let last = 0;
  for (let position = 0; position <= SPAN; position += 1) {
    const metres = toAlt(position);
    assert.ok(metres >= last, `position ${position} gave ${metres} after ${last}`);
    assert.ok(metres >= MIN && metres <= MAX);
    last = metres;
  }
}

function testEveryOfferedHeightSurvivesTheRoundTrip() {
  // The thumb is put where the height is, and read again when it is moved: a
  // height that did not come back would creep by itself.
  const offered = [];
  for (let m = 1; m < 50; m += 1) offered.push(m);
  for (let m = 50; m <= 120; m += 5) offered.push(m);
  for (const m of offered) assert.equal(toAlt(toPos(m)), m, `${m} m`);
}

function testHeightsSnapToWholeMetresAndFiveAbove50() {
  assert.equal(toAlt(toPos(30.2)), 30);
  assert.equal(toAlt(toPos(83)), 85);
  for (let position = 0; position <= SPAN; position += 7) {
    const metres = toAlt(position);
    assert.ok(Number.isInteger(metres), `${metres} is not a whole metre`);
    if (metres >= 50) assert.equal(metres % 5, 0, `${metres} m above 50 is not a multiple of 5`);
  }
}

function testAnArrowKeyMovesOneStep() {
  assert.equal(nudge(10, 1), 11);
  assert.equal(nudge(10, -1), 9);
  assert.equal(nudge(49, 1), 50);
  assert.equal(nudge(50, 1), 55);
  assert.equal(nudge(55, -1), 50);
  assert.equal(nudge(50, -1), 49, "back below 50 the step is a metre again");
}

function testAnArrowKeyFromAnOffGridHeightLandsOnTheGrid() {
  assert.equal(nudge(30.48, 1), 31);
  assert.equal(nudge(30.48, -1), 30);
  assert.equal(nudge(52.3, 1), 55);
  assert.equal(nudge(52.3, -1), 50);
}

function testAnArrowKeyStopsAtTheEnds() {
  assert.equal(nudge(MIN, -1), MIN);
  assert.equal(nudge(MAX, 1), MAX);
  assert.equal(nudge(118, 1), MAX);
}

const OK = { badInput: false, rangeUnderflow: false, rangeOverflow: false };

function testAnEntryThatStandsAsTypedIsClear() {
  assert.equal(entryIsClear("25", OK), true);
  assert.equal(entryIsClear("7.5", OK), true, "a fraction is not a step mismatch worth refusing");
}

function testAnEmptyOrUnreadableEntryIsNotClear() {
  assert.equal(entryIsClear("", OK), false);
  assert.equal(entryIsClear("   ", OK), false);
  assert.equal(entryIsClear("", { ...OK, badInput: true }), false);
}

function testAnEntryOutsideTheRangeIsNotClear() {
  assert.equal(entryIsClear("500", { ...OK, rangeOverflow: true }), false);
  assert.equal(entryIsClear("0", { ...OK, rangeUnderflow: true }), false);
}

const tests = [
  testAnEntryThatStandsAsTypedIsClear,
  testAnEmptyOrUnreadableEntryIsNotClear,
  testAnEntryOutsideTheRangeIsNotClear,
  testTheSliderSpansTheWholeRange,
  testTheSliderIsLogarithmic,
  testTheSliderNeverGoesBackwards,
  testEveryOfferedHeightSurvivesTheRoundTrip,
  testHeightsSnapToWholeMetresAndFiveAbove50,
  testAnArrowKeyMovesOneStep,
  testAnArrowKeyFromAnOffGridHeightLandsOnTheGrid,
  testAnArrowKeyStopsAtTheEnds,
  testATypedNumberIsTheAltitude,
  testTheDisplayUnitIsConvertedToMetres,
  testAnAltitudeOutsideTheRangeIsClamped,
  testNothingUsableIsNotAnAltitude,
];
let failed = 0;
for (const t of tests) {
  try {
    t();
    console.log(`ok   - ${t.name}`);
  } catch (err) {
    failed += 1;
    console.log(`FAIL - ${t.name}\n      ${err.message}`);
  }
}
if (failed) {
  console.log(`\n${failed}/${tests.length} takeoff entry test(s) FAILED`);
  process.exit(1);
}
console.log(`\nAll ${tests.length} takeoff entry tests passed.`);
