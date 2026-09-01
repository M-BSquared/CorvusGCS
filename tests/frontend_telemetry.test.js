"use strict";

const assert = require("node:assert/strict");

const animationFrames = [];
const windowListeners = new Map();
const eventSources = [];

global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) {
    this.type = type;
    this.detail = options.detail;
  }
};
window.requestAnimationFrame = (callback) => animationFrames.push(callback);
window.addEventListener = (type, callback) => {
  const listeners = windowListeners.get(type) || [];
  listeners.push(callback);
  windowListeners.set(type, listeners);
};
window.dispatchEvent = (event) => {
  (windowListeners.get(event.type) || []).forEach((callback) => callback(event));
};

class FakeEventSource {
  constructor(url) {
    this.url = url;
    this.listeners = new Map();
    this.closed = false;
    eventSources.push(this);
  }

  addEventListener(type, callback) {
    this.listeners.set(type, callback);
  }

  emit(type, data = "") {
    this.listeners.get(type)?.({ data });
  }

  close() {
    this.closed = true;
  }
}

global.EventSource = FakeEventSource;
require("../src/js/telemetry.js");

function flushAnimationFrame() {
  const callbacks = animationFrames.splice(0);
  callbacks.forEach((callback) => callback());
}

async function rejectsWith(promise, pattern) {
  await assert.rejects(promise, pattern);
}

async function run() {
  const states = [];
  const notifications = [];
  window.addEventListener("corvus:notification", (event) => notifications.push(event.detail));
  Corvus.telemetry.subscribe((state) => states.push({ ...state }));

  Corvus.telemetry.connect();
  const firstSource = eventSources[0];
  firstSource.emit("state", JSON.stringify({ connected: true, heading: 10, warnings: [] }));
  firstSource.emit("state", JSON.stringify({ connected: true, heading: 20, warnings: [] }));
  assert.equal(states.length, 0, "state bursts should wait for one animation frame");
  flushAnimationFrame();
  assert.equal(states.length, 1);
  assert.equal(states[0].heading, 20, "subscribers should receive the newest state in the frame");

  firstSource.onerror();
  firstSource.onerror();
  flushAnimationFrame();
  assert.equal(notifications.length, 1, "one outage should create one notification");
  assert.equal(states.at(-1).connected, false);

  firstSource.emit("state", JSON.stringify({ connected: true, heading: 30, warnings: [] }));
  flushAnimationFrame();
  firstSource.onerror();
  assert.equal(notifications.length, 2, "a recovered stream may report a later outage");

  Corvus.telemetry.connect();
  const secondSource = eventSources[1];
  assert.equal(firstSource.closed, true);
  secondSource.onerror();
  assert.equal(notifications.length, 3, "a replacement stream should report its own outage");
  firstSource.emit("state", JSON.stringify({ connected: true, heading: 999, warnings: [] }));
  secondSource.emit("state", JSON.stringify({ connected: true, heading: 40, warnings: [] }));
  flushAnimationFrame();
  assert.equal(states.at(-1).heading, 40, "events from a replaced stream must be ignored");

  const originalConsoleError = console.error;
  console.error = () => {};
  secondSource.emit("state", "not json");
  secondSource.emit("state", "still not json");
  assert.equal(notifications.length, 4, "repeated parse failures should be reported once");
  secondSource.emit("state", JSON.stringify({ connected: true, heading: 41, warnings: [] }));
  secondSource.emit("state", "not json again");
  console.error = originalConsoleError;
  assert.equal(notifications.length, 5, "valid data should reset parse-failure reporting");

  global.fetch = async () => ({
    ok: false,
    status: 400,
    json: async () => ({ error: "vehicle rejected command" }),
  });
  await rejectsWith(Corvus.telemetry.postAction("/action", {}), /vehicle rejected command/);

  global.fetch = async () => ({ ok: true, status: 200, json: async () => ({ ok: false }) });
  await rejectsWith(Corvus.telemetry.postAction("/action", {}), /Command rejected by vehicle/);

  global.fetch = async () => { throw new Error("offline"); };
  await rejectsWith(Corvus.telemetry.requestJson("/state"), /Network request failed: offline/);

  global.fetch = async () => ({ ok: true, status: 200, json: async () => { throw new Error("bad json"); } });
  await rejectsWith(Corvus.telemetry.requestJson("/state"), /Invalid response/);

  console.log("frontend telemetry tests passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
