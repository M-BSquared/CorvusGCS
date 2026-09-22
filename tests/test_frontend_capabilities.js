"use strict";

/**
 * Tests for Corvus.capabilities (src/js/capabilities.js) and the bitmask
 * control in Corvus.setupShared.
 *
 * Corvus speaks PX4 and ArduPilot, and they do not offer the same feature set.
 * Every difference is a control on some page, and a control that is offered and
 * then refused is worse than one that is not there — the operator is standing
 * in a field wondering what they did wrong.
 *
 * Two rules matter more than the rest and are the reason this file exists:
 *
 *   The cache is keyed by STACK, not by session. The whole point of the
 *   document is that reconnecting to a different aircraft changes the answer.
 *
 *   A failure degrades to "allowed". A page that hides its own controls
 *   whenever the network hiccups is a worse failure than one that offers a
 *   control the vehicle refuses with a message.
 *
 * Run:
 *   node tests/test_frontend_capabilities.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};

let snapshot = { autopilot_stack: "" };
let requests = [];
let answer = null;      // null = reject

Corvus.telemetry = {
  getState: () => snapshot,
  async requestJson(url) {
    requests.push(url);
    if (answer === null) throw new Error("Network request failed");
    return answer;
  },
};

require("../src/js/capabilities.js");
const C = Corvus.capabilities;

function reset(stack, doc) {
  snapshot = { autopilot_stack: stack };
  requests = [];
  answer = doc;
  C.reset();
}

const ARDUPILOT = {
  stack: "ardupilot", label: "ArduPilot", shell: false, esc_calibration: false,
  accel_cal_prompted: true, autotune: "mode", log_suffix: ".bin",
  calibrations: ["accel", "compass", "gyro"],
};

async function testTheDocumentIsFetchedOncePerStack() {
  reset("ardupilot", ARDUPILOT);
  const a = await C.get();
  const b = await C.get();
  assert.equal(requests.length, 1, "a second ask must not be a second request");
  assert.equal(a, b);
  assert.equal(a.shell, false);
}

async function testConcurrentAsksShareOneRequest() {
  reset("ardupilot", ARDUPILOT);
  const [a, b, c] = await Promise.all([C.get(), C.get(), C.get()]);
  assert.equal(requests.length, 1);
  assert.equal(a, b);
  assert.equal(b, c);
}

async function testADifferentAircraftIsADifferentAnswer() {
  reset("ardupilot", ARDUPILOT);
  await C.get();
  snapshot = { autopilot_stack: "px4" };
  answer = { stack: "px4", shell: true, calibrations: ["motor"] };
  const px4 = await C.get();
  assert.equal(requests.length, 2, "the stack changed, so the document must be re-read");
  assert.equal(px4.shell, true);
}

async function testAFailedFetchDegradesToAllowed() {
  reset("px4", null);
  const caps = await C.get();
  assert.equal(caps.shell, true, "a network hiccup must not hide the console");
  assert.equal(caps.stack, "px4", "the stack we asked for is still reported");
}

async function testAnUnansweredCalibrationListNeverGatesAnything() {
  reset("px4", null);
  assert.equal(C.hasCalibration("motor"), true, "nothing fetched yet = nothing gated");
  await C.get();
  assert.equal(C.hasCalibration("motor"), true, "an empty list is not a denial");
}

async function testAnAnsweredListGatesWhatIsNotInIt() {
  reset("ardupilot", ARDUPILOT);
  await C.get();
  assert.equal(C.hasCalibration("compass"), true);
  assert.equal(C.hasCalibration("motor"), false, "ArduPilot has no MAVLink ESC cal");
}

function testPeekNeverFetches() {
  reset("ardupilot", ARDUPILOT);
  assert.equal(C.peek(), null);
  assert.equal(requests.length, 0);
}

const tests = [
  testTheDocumentIsFetchedOncePerStack,
  testConcurrentAsksShareOneRequest,
  testADifferentAircraftIsADifferentAnswer,
  testAFailedFetchDegradesToAllowed,
  testAnUnansweredCalibrationListNeverGatesAnything,
  testAnAnsweredListGatesWhatIsNotInIt,
  testPeekNeverFetches,
];

(async () => {
  let failures = 0;
  for (const t of tests) {
    try {
      await t();
      console.log("ok   - " + t.name);
    } catch (err) {
      failures += 1;
      console.error("FAIL - " + t.name + "\n     " + err.message);
    }
  }
  if (failures) {
    console.error("\n" + failures + " capability test(s) failed.");
    process.exitCode = 1;
  } else {
    console.log("\nAll " + tests.length + " capability tests passed.");
  }
})();
