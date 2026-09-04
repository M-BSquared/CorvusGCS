"use strict";

/**
 * Tests for Corvus.calibProtocol (src/js/calib-protocol.js) and the attitude
 * vocabulary in Corvus.calibFigures (src/js/calib-figures.js).
 *
 * This is the layer the calibration wizard trusts to turn PX4's own words into
 * an instruction, so it is tested against real PX4 transcripts rather than
 * against a rendered page. The transcripts below are the messages PX4 v1.16 /
 * v1.17 / v1.18 emit from calibration_routines.cpp, accelerometer_calibration.cpp,
 * mag_calibration.cpp, airspeed_calibration.cpp and esc_calibration.cpp.
 *
 * Run:
 *   node tests/test_calib_protocol.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
global.document = {
  createElement: () => ({ dataset: {}, className: "", textContent: "", appendChild() {} }),
};

require("../src/js/calib-figures.js");
require("../src/js/calib-protocol.js");

const P = Corvus.calibProtocol;
const F = Corvus.calibFigures;

// ---------------------------------------------------------------------------
// PX4's orientation vocabulary
// ---------------------------------------------------------------------------

function testSideWordsMapToTheAttitudeTheyMean() {
  // PX4 names the side that faces DOWN. Getting "up" wrong here would tell the
  // operator to hold the aircraft the exact opposite way up, so it is pinned.
  assert.equal(P.poseFor("down"), "level");
  assert.equal(P.poseFor("up"), "upside_down");
  assert.equal(P.poseFor("front"), "nose_down");
  assert.equal(P.poseFor("back"), "tail_down");
  assert.equal(P.poseFor("left"), "left");
  assert.equal(P.poseFor("right"), "right");
  assert.equal(P.poseFor("DOWN"), "level", "case-insensitive");
  assert.equal(P.poseFor("sideways"), null, "unknown word is not guessed at");

  // Every mapped attitude must be one the figure renderer can actually draw.
  Object.values(P.SIDE_TO_POSE).forEach((pose) => {
    assert.ok(F.POSES.includes(pose), pose + " is a drawable attitude");
    assert.ok(F.poseLabel(pose), pose + " has an operator-facing label");
  });
}

// ---------------------------------------------------------------------------
// parseLine
// ---------------------------------------------------------------------------

function testParsesRealPx4CalibrationLines() {
  const cases = [
    ["[cal] calibration started: 2 accel", { kind: "started", sensor: "accel" }],
    ["[cal] progress <20>", { kind: "progress", value: 20 }],
    ["[cal] progress 100", { kind: "progress", value: 100 }],
    ["[cal] Hold still, measuring down side", { kind: "measure", pose: "level" }],
    ["[cal] down side done, rotate to a different side", { kind: "side_done", pose: "level" }],
    ["[cal] left side already completed", { kind: "side_completed", pose: "left" }],
    ["[cal] ERROR: Not enough measurements for back side",
      { kind: "side_failed", pose: "tail_down" }],
    ["[cal] up orientation detected", { kind: "detected", pose: "upside_down" }],
    ["[cal] Continue rotation for front side 15 s",
      { kind: "rotate_around", pose: "nose_down", seconds: 15 }],
    ["[cal] Rotate vehicle around the detected orientation",
      { kind: "rotate_around", pose: null }],
    ["[cal] calibration done: accel", { kind: "done" }],
    ["[cal] calibration failed: gyro", { kind: "failed" }],
    ["[cal] calibration cancelled", { kind: "cancelled" }],
    ["[cal] Connect the battery now", { kind: "prompt", action: "battery_on" }],
    ["[cal] Disconnect battery and try again", { kind: "prompt", action: "battery_off" }],
    ["[cal] Blow into front of pitot without touching", { kind: "prompt", action: "blow" }],
    ["[cal] Keep wind away from sensor", { kind: "prompt", action: "shield" }],
    ["[cal] ESC calibration finished", { kind: "done", sensor: "motor" }],
  ];
  for (const [line, expected] of cases) {
    const got = P.parseLine(line);
    assert.ok(got, "parsed: " + line);
    for (const key of Object.keys(expected)) {
      assert.equal(got[key], expected[key], line + " -> " + key);
    }
  }
}

function testPendingSideListIsParsedInOrder() {
  const ev = P.parseLine("[cal] Rotate to a pending side: up, left, right");
  assert.equal(ev.kind, "pending");
  assert.deepEqual(ev.poses, ["upside_down", "left", "right"]);
}

function testCompletionWinsOverPositionWording() {
  // "down side done" contains "down side", which the measure/rotate patterns
  // also match — the specific event has to win or a finished side would be
  // re-announced as the next one to hold.
  assert.equal(P.parseLine("[cal] down side done, rotate to a different side").kind, "side_done");
  // Likewise a terminal outcome must not be read as a rotation request.
  assert.equal(P.parseLine("[cal] calibration done: mag").kind, "done");
}

function testNonCalibrationTrafficIsIgnored() {
  ["ARMED by RC", "Preflight Fail: Compass Sensor 0 missing", "", null, undefined]
    .forEach((line) => assert.equal(P.parseLine(line), null, "ignored: " + line));
}

// ---------------------------------------------------------------------------
// Procedures
// ---------------------------------------------------------------------------

function testEveryProcedureIsCompleteAndDrawable() {
  assert.deepEqual(P.ORDER.slice().sort(), Object.keys(P.PROCEDURES).sort(),
    "ORDER lists every procedure exactly once");
  for (const type of P.ORDER) {
    const proc = P.PROCEDURES[type];
    assert.equal(proc.type, type);
    assert.ok(proc.label && proc.summary && proc.brief, type + " has operator-facing copy");
    assert.ok(proc.prep.length >= 2, type + " states its preconditions");
    assert.ok(F.POSES.includes(proc.startPose), type + " starts from a drawable attitude");
    proc.poses.forEach((p) => assert.ok(F.POSES.includes(p), type + "/" + p + " is drawable"));
  }
  assert.equal(P.PROCEDURES.motor.danger, true, "motor/ESC is flagged dangerous");
  assert.equal(P.PROCEDURES.accel.poses.length, 6, "accelerometer needs six positions");
  assert.equal(P.PROCEDURES.compass.spin, true, "compass calibration is a rotation");
}

// ---------------------------------------------------------------------------
// Session state machine
// ---------------------------------------------------------------------------

function feed(session, lines, t) {
  let clock = t || 1000;
  lines.forEach((line) => { clock += 1000; session.ingest(line, clock); });
  return clock;
}

function testAccelerometerSessionTracksEachSide() {
  const s = P.createSession("accel");
  assert.equal(s.getState().phase, "idle");
  s.begin(1000);
  assert.equal(s.getState().phase, "starting");

  feed(s, [
    "[cal] calibration started: 2 accel",
    "[cal] progress <17>",
    "[cal] Hold still, measuring down side",
  ]);
  let st = s.getState();
  assert.equal(st.phase, "running");
  assert.equal(st.progress, 17);
  assert.equal(st.pose, "level", "figure holds the attitude PX4 asked for");
  assert.equal(st.sides.level, "active");
  assert.match(st.headline, /Hold still/);

  feed(s, ["[cal] down side done, rotate to a different side"]);
  st = s.getState();
  assert.equal(st.sides.level, "done");
  // A finished side must not stay on the figure — the operator needs the next
  // thing to do, not the thing they just did.
  assert.notEqual(st.pose, "level", "the figure moves on to the next pending side");
  assert.equal(st.sides[st.pose], "active");

  feed(s, ["[cal] Rotate to a pending side: up, left"]);
  st = s.getState();
  assert.equal(st.pose, "upside_down", "the first still-pending side becomes the target");
  assert.equal(st.sides.upside_down, "active");

  feed(s, ["[cal] ERROR: Not enough measurements for back side"]);
  st = s.getState();
  assert.equal(st.sides.tail_down, "failed");
  assert.match(st.detail, /steadily/);

  feed(s, ["[cal] calibration done: accel"]);
  st = s.getState();
  assert.equal(st.phase, "done");
  assert.equal(st.progress, 100);
  assert.ok(Object.values(st.sides).every((v) => v === "done"), "success completes every side");
  assert.match(st.detail, /Reboot/, "accelerometer calibration needs a reboot to take effect");
}

function testPendingListNeverReactivatesAFinishedSide() {
  const s = P.createSession("accel");
  s.begin(1000);
  feed(s, [
    "[cal] Hold still, measuring down side",
    "[cal] down side done, rotate to a different side",
    // PX4 re-lists a side it has already finished; the strip must not undo it.
    "[cal] Rotate to a pending side: down, left",
  ]);
  const st = s.getState();
  assert.equal(st.sides.level, "done", "a finished side stays finished");
  assert.equal(st.pose, "left", "the next unfinished side is targeted instead");
}

function testCompassSessionSpinsOnRotationRequests() {
  const s = P.createSession("compass");
  s.begin(1000);
  feed(s, ["[cal] down orientation detected"]);
  let st = s.getState();
  assert.equal(st.pose, "level");
  assert.equal(st.spin, true, "the compass figure rotates once a side is detected");

  feed(s, ["[cal] Continue rotation for down side 20 s"]);
  assert.match(s.getState().detail, /20 s/);

  // Measuring is a hold, not a turn — the animation has to stop saying "spin".
  feed(s, ["[cal] Hold still, measuring left side"]);
  st = s.getState();
  assert.equal(st.spin, false);
  assert.equal(st.pose, "left");
}

function testOperatorPromptsBecomeActions() {
  const s = P.createSession("motor");
  s.begin(1000);
  feed(s, ["[cal] Connect the battery now"]);
  const st = s.getState();
  assert.equal(st.action, "battery_on");
  assert.match(st.headline, /Connect the flight battery/);
}

function testFailureAndCancellationAreTerminal() {
  const failed = P.createSession("gyro");
  failed.begin(1000);
  feed(failed, ["[cal] calibration failed: gyro"]);
  assert.equal(failed.getState().phase, "failed");

  const cancelled = P.createSession("gyro");
  cancelled.begin(1000);
  feed(cancelled, ["[cal] calibration cancelled"]);
  assert.equal(cancelled.getState().phase, "cancelled");

  // finish() is the operator-caused terminal state (a rejected start, a failed
  // abort) and must not need a vehicle message to take effect.
  const local = P.createSession("gyro");
  local.finish("failed", "Could not start", "not connected");
  assert.equal(local.getState().phase, "failed");
  assert.equal(local.getState().headline, "Could not start");
}

function testResetReturnsToTheBriefing() {
  const s = P.createSession("accel");
  s.begin(1000);
  feed(s, ["[cal] Hold still, measuring down side", "[cal] calibration failed: accel"]);
  s.reset();
  const st = s.getState();
  assert.equal(st.phase, "idle");
  assert.equal(st.progress, null);
  assert.equal(st.headline, P.PROCEDURES.accel.brief);
  assert.ok(Object.values(st.sides).every((v) => v === "pending"), "every side back to pending");
}

function testUnknownCalibrationTypeIsRejected() {
  assert.throws(() => P.createSession("nonexistent"), /unknown calibration type/);
}

function testUnrecognisedCalLineRefreshesTheWatchdogWithoutMisleading() {
  // A wording change in a future PX4 must not produce a wrong instruction; it
  // lands in the transcript and keeps the stall watchdog quiet, nothing more.
  const s = P.createSession("gyro");
  s.begin(1000);
  const changed = s.ingest("[cal] some future wording nobody has seen", 5000);
  const st = s.getState();
  assert.equal(changed, true, "the line still counts as vehicle activity");
  assert.equal(st.lastEventAt, 5000);
  assert.equal(st.phase, "running");
  assert.equal(st.detail, "[cal] some future wording nobody has seen");
}

const tests = [
  testSideWordsMapToTheAttitudeTheyMean,
  testParsesRealPx4CalibrationLines,
  testPendingSideListIsParsedInOrder,
  testCompletionWinsOverPositionWording,
  testNonCalibrationTrafficIsIgnored,
  testEveryProcedureIsCompleteAndDrawable,
  testAccelerometerSessionTracksEachSide,
  testPendingListNeverReactivatesAFinishedSide,
  testCompassSessionSpinsOnRotationRequests,
  testOperatorPromptsBecomeActions,
  testFailureAndCancellationAreTerminal,
  testResetReturnsToTheBriefing,
  testUnknownCalibrationTypeIsRejected,
  testUnrecognisedCalLineRefreshesTheWatchdogWithoutMisleading,
];

let failures = 0;
for (const t of tests) {
  try {
    t();
    console.log("ok   - " + t.name);
  } catch (err) {
    failures += 1;
    console.error("FAIL - " + t.name + "\n     " + err.message);
  }
}
if (failures) {
  console.error("\n" + failures + " calibration protocol test(s) failed.");
  process.exitCode = 1;
} else {
  console.log("\nAll " + tests.length + " calibration protocol tests passed.");
}
