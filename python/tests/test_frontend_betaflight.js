"use strict";

/**
 * Frontend tests for the Setup -> Betaflight page (Corvus.setupBetaflight):
 * the two pure functions that decide what the page says.
 *
 * Run: node tests/test_frontend_betaflight.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
require("../src/js/setup-betaflight.js");

const B = Corvus.setupBetaflight;

function testPortLabelMarksTheLinkPort() {
  assert.equal(B.portLabel({ device: "/dev/ttyACM0", description: "STM32 Virtual ComPort", link: false }),
    "/dev/ttyACM0 (STM32 Virtual ComPort)");
  assert.equal(B.portLabel({ device: "/dev/ttyUSB0", description: "CP2102", link: true }),
    "/dev/ttyUSB0 (MAVLink link, CP2102)");
  assert.equal(B.portLabel({ device: "COM5" }), "COM5");
  assert.equal(B.portLabel(null), "");
}

function testSummaryRowsReadTheReply() {
  const rows = B.summaryRows({
    variant_name: "Betaflight", version: "4.5.1", api: "1.46",
    board: "S405", target: "SPEEDYF4", name: "racer",
    build: { date: "Oct 28 2026", time: "12:00:00" },
    sensors: ["gyro", "accelerometer"], cycle_time_us: 125, cpu_load: 12,
    pid_profile: 2, pid_profile_count: 4, rate_profile: 1,
    battery: { cells: 4, voltage: 16.2, current: 2.5 },
  });
  assert.deepEqual(Object.fromEntries(rows), {
    "Firmware": "Betaflight 4.5.1",
    "MSP API": "1.46",
    "Board": "S405, SPEEDYF4",
    "Built": "Oct 28 2026 12:00:00",
    "Craft name": "racer",
    "Sensors": "gyro, accelerometer",
    "Loop time": "125 µs (8000 Hz)",
    "CPU load": "12 %",
    "PID profile": "2 of 4",
    "Rate profile": "1",
    "Battery": "4S, 16.20 V, 2.5 A",
  });
}

function testSummaryRowsLeaveOutWhatWasNotAnswered() {
  const rows = B.summaryRows({ variant_name: "Betaflight", version: "4.5.1", sensors: [] });
  assert.deepEqual(rows, [["Firmware", "Betaflight 4.5.1"], ["Sensors", "none"]]);
  assert.deepEqual(B.summaryRows(null), []);
}

testPortLabelMarksTheLinkPort();
testSummaryRowsReadTheReply();
testSummaryRowsLeaveOutWhatWasNotAnswered();
console.log("betaflight page tests passed");
