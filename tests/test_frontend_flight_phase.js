"use strict";

/**
 * The flight bar offers only the one of TAKEOFF and LAND that applies: in the
 * air TAKEOFF is greyed, on the ground LAND is.
 *
 * The two are not judged alike. LAND is greyed only on a fact (disarmed, or
 * the autopilot saying ON_GROUND), never on the height guess used when a
 * firmware publishes no landed state: withholding LAND from an aircraft that
 * is flying is the one mistake this must not make.
 *
 * Run:
 *   node tests/test_frontend_flight_phase.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
global.document = { addEventListener: () => {}, getElementById: () => null };

require("./../src/js/app.js");
const { flightPhase } = Corvus.app;

const ON_GROUND = 1;
const IN_AIR = 2;
const TAKEOFF = 3;
const LANDING = 4;

function testNothingIsGreyedWithoutAVehicle() {
  assert.equal(flightPhase(null), "");
  assert.equal(flightPhase({ connected: false, armed: true, landed_state: IN_AIR }), "");
}

function testTheAutopilotsLandedStateDecides() {
  for (const landed of [IN_AIR, TAKEOFF, LANDING]) {
    assert.equal(flightPhase({ connected: true, armed: true, landed_state: landed }), "air",
      `landed state ${landed} is off the ground`);
  }
  assert.equal(flightPhase({ connected: true, armed: true, landed_state: ON_GROUND,
    altitude_agl: 40 }), "ground", "the autopilot's own answer beats a height");
}

function testAVehicleClimbingOutCanStillLand() {
  // TAKEOFF in progress: LAND has to be there to abort it.
  assert.equal(flightPhase({ connected: true, armed: true, landed_state: TAKEOFF }), "air");
}

function testADisarmedVehicleIsOnTheGround() {
  assert.equal(flightPhase({ connected: true, armed: false, landed_state: 0 }), "ground");
}

function testTheHeightGuessNeverGreysLand() {
  // No landed state published (0, UNDEFINED): the height above home decides
  // only that it is flying, never that it is not.
  assert.equal(flightPhase({ connected: true, armed: true, landed_state: 0, altitude_agl: 12 }), "air");
  assert.equal(flightPhase({ connected: true, armed: true, landed_state: 0, altitude_agl: 0.4 }), "",
    "armed, low, no landed state: both stay offered");
  assert.equal(flightPhase({ connected: true, armed: true }), "");
}

const tests = [
  testNothingIsGreyedWithoutAVehicle,
  testTheAutopilotsLandedStateDecides,
  testAVehicleClimbingOutCanStillLand,
  testADisarmedVehicleIsOnTheGround,
  testTheHeightGuessNeverGreysLand,
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
  console.log(`\n${failed}/${tests.length} flight phase test(s) FAILED`);
  process.exit(1);
}
console.log(`\nAll ${tests.length} flight phase tests passed.`);
