"use strict";

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
require("../src/js/notification_dedupe.js");

const { actionFromMessage, isFailureMessage, createTracker } = Corvus.notificationDedupe;

assert.equal(actionFromMessage("Disarm failed: DENIED"), "disarm");
assert.equal(actionFromMessage("Arm command rejected by vehicle"), "arm");
assert.equal(actionFromMessage("Take-off failed: no altitude reference"), "takeoff");
assert.equal(actionFromMessage("Return to launch failed"), "rtl");
assert.equal(actionFromMessage("Landing command denied"), "land");
assert.equal(actionFromMessage("Mode change failed"), "mode");
assert.equal(actionFromMessage("Telemetry connection interrupted"), "");
assert.equal(isFailureMessage("Takeoff failed: DENIED"), true);
assert.equal(isFailureMessage("Takeoff target accepted"), false);

let clock = 1000;
const tracker = createTracker({ windowMs: 3000, now: () => clock });

const remoteFirst = tracker.begin("takeoff");
clock += 10000;
assert.equal(tracker.matchRemote({ msg: "Takeoff target accepted" }), null);
assert.equal(
  tracker.matchRemote({ msg: "Takeoff command failed: DENIED" }),
  remoteFirst,
  "an active request should correlate even when the backend takes longer than the dedupe window",
);
tracker.failed(remoteFirst);
assert.equal(tracker.shouldAddLocal(remoteFirst), false, "HTTP error after SSE should not add a duplicate");

const localFirst = tracker.begin("land");
tracker.failed(localFirst);
tracker.attachLocal(localFirst, 17);
clock += 100;
assert.equal(tracker.matchRemote({ msg: "Landing failed: TEMPORARILY_REJECTED" }), localFirst);
assert.equal(localFirst.localNotificationId, 17, "SSE can replace the correlated local notification");

const expired = tracker.begin("rtl");
tracker.failed(expired);
clock += 3001;
assert.equal(tracker.matchRemote({ msg: "RTL command failed: DENIED" }), null);
assert.equal(tracker.shouldAddLocal(expired), true, "events outside the time window stay distinct");

const firstRetry = tracker.begin("arm");
tracker.failed(firstRetry);
assert.equal(tracker.matchRemote({ msg: "Arm failed: DENIED" }), firstRetry);
const secondRetry = tracker.begin("arm");
tracker.failed(secondRetry);
assert.equal(tracker.shouldAddLocal(secondRetry), true, "a later attempt is not hidden by an earlier match");
assert.equal(
  tracker.matchRemote({ msg: "Arm command rejected: DENIED" }),
  secondRetry,
  "a separate backend event correlates to the separate retry",
);

const modeAttempt = tracker.begin("mode");
tracker.failed(modeAttempt);
assert.equal(tracker.matchRemote({ msg: "Takeoff failed: DENIED" }), null, "different actions never merge");

const delayedFirst = tracker.begin("land");
tracker.failed(delayedFirst);
tracker.attachLocal(delayedFirst, 21);
const pendingRetry = tracker.begin("land");
assert.equal(
  tracker.matchRemote({ msg: "Land failed: DENIED" }),
  delayedFirst,
  "a delayed backend event replaces the older local failure before consuming a newer retry",
);
assert.equal(tracker.shouldAddLocal(pendingRetry), true);
assert.equal(tracker.matchRemote({ msg: "Land command failed: DENIED" }), pendingRetry);

console.log("notification dedupe tests passed");
