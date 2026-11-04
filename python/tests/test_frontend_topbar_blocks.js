"use strict";

/**
 * Altitude, groundspeed and climb rate can be taken off the top bar (Settings,
 * Top bar). Every one is shown unless hidden, the choice is written on <html>
 * for the CSS and cached for the next first frame, and a name the bar does not
 * know is ignored.
 *
 * Run:
 *   node tests/test_frontend_topbar_blocks.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
window.addEventListener = () => {};
window.removeEventListener = () => {};
const stored = {};
global.localStorage = {
  getItem: (k) => (k in stored ? stored[k] : null),
  setItem: (k, v) => { stored[k] = String(v); },
  removeItem: (k) => { delete stored[k]; },
};
const attrs = {};
global.document = {
  documentElement: {
    setAttribute: (k, v) => { attrs[k] = String(v); },
    getAttribute: (k) => (k in attrs ? attrs[k] : null),
    removeAttribute: (k) => { delete attrs[k]; },
  },
};

require("./../src/js/units.js");
require("./../src/js/notification_dedupe.js");
require("./../src/js/topbar.js");

const tb = Corvus.topbar;
const tests = [];
function test(name, fn) { tests.push([name, fn]); }

test("everything is shown by default", () => {
  ["altitude", "groundspeed", "vspeed"].forEach((k) => assert.equal(tb.blockShown(k), true, k));
  assert.equal(attrs["data-topbar-hidden"], undefined);
});

test("hiding one leaves the others and writes the attribute", () => {
  assert.equal(tb.setBlockShown("groundspeed", false), false);
  assert.equal(tb.blockShown("groundspeed"), false);
  assert.equal(tb.blockShown("altitude"), true);
  assert.equal(attrs["data-topbar-hidden"], "groundspeed");
  assert.equal(stored["corvus.topbarHidden"], "groundspeed");
});

test("several hidden at once, in a fixed order", () => {
  tb.setBlockShown("vspeed", false);
  tb.setBlockShown("altitude", false);
  assert.equal(attrs["data-topbar-hidden"], "altitude groundspeed vspeed");
});

test("showing them again clears the attribute", () => {
  ["altitude", "groundspeed", "vspeed"].forEach((k) => tb.setBlockShown(k, true));
  assert.equal(attrs["data-topbar-hidden"], undefined);
  assert.equal(stored["corvus.topbarHidden"], "");
});

test("a block the bar cannot hide is ignored", () => {
  assert.equal(tb.setBlockShown("battery", false), true);
  assert.equal(tb.blockShown("battery"), true);
  assert.equal(attrs["data-topbar-hidden"], undefined);
});

let failed = 0;
for (const [name, fn] of tests) {
  try { fn(); console.log("ok   " + name); }
  catch (err) { failed += 1; console.log("FAIL " + name + "\n  " + err.message); }
}
if (failed) { console.log(`${failed} failed`); process.exit(1); }
console.log(`${tests.length} passed`);
