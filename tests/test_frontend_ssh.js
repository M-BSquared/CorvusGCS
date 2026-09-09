"use strict";

/**
 * How a saved SSH connection is written down (Corvus.panel.sshAddress).
 *
 * The account is part of a connection's identity — two saved entries can point
 * at the same machine and differ only in which user they log in as — but the
 * SSH cards used to render the host alone, and the terminal header printed a
 * hardcoded "corvus@companion" for every session. An operator logging in as
 * anyone else was told they were someone they are not.
 *
 * Run:
 *   node tests/test_frontend_ssh.js
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

const { sshAddress } = Corvus.panel;

function testTheAccountIsNamed() {
  assert.equal(
    sshAddress({ name: "CORVUS-01", host: "192.168.2.10", port: 22, username: "schwalby" }),
    "schwalby@192.168.2.10:22",
    "the account that logs in must be visible on the card",
  );
}

function testTwoEntriesDifferingOnlyByAccountReadDifferently() {
  const a = sshAddress({ host: "10.0.0.5", port: 22, username: "pi" });
  const b = sshAddress({ host: "10.0.0.5", port: 22, username: "schwalby" });
  assert.notEqual(a, b, "same host, different user must not render identically");
}

function testMissingPiecesDegradeInsteadOfPrintingUndefined() {
  assert.equal(sshAddress({ host: "10.0.0.5" }), "10.0.0.5");
  assert.equal(sshAddress({ host: "10.0.0.5", username: "pi" }), "pi@10.0.0.5");
  assert.equal(sshAddress({}), "");
}

const tests = [
  testTheAccountIsNamed,
  testTwoEntriesDifferingOnlyByAccountReadDifferently,
  testMissingPiecesDegradeInsteadOfPrintingUndefined,
];

let failed = 0;
tests.forEach((t) => {
  try {
    t();
    console.log(`  ok  ${t.name}`);
  } catch (err) {
    failed += 1;
    console.error(`  FAIL ${t.name}: ${err.message}`);
  }
});
console.log(failed ? `${failed} failing` : `${tests.length} passing`);
process.exit(failed ? 1 : 0);
