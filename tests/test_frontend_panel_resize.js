"use strict";

/**
 * The right panel can be dragged a little wider or narrower. The clamp is the
 * part that can go wrong: too wide and the map has no room left, too narrow
 * and the tab strip and forms break.
 *
 * Run:
 *   node tests/test_frontend_panel_resize.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
window.addEventListener = () => {};
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

const { clampPanelWidth } = Corvus.panel;

assert.equal(clampPanelWidth(380, 1600), 380);
assert.equal(clampPanelWidth(100, 1600), 320, "never below the minimum");
assert.equal(clampPanelWidth(900, 1600), 560, "never above the maximum");
assert.equal(clampPanelWidth(900, 800), 480, "never more than 60% of a small window");
assert.equal(clampPanelWidth(900, 400), 320, "the minimum wins in a tiny window");
assert.equal(clampPanelWidth("abc", 1600), 320, "garbage falls back to the minimum");

console.log("panel resize: ok");
