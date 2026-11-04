"use strict";

/**
 * Text selection: the stylesheet turns it off for the app chrome and back on
 * for the places an operator copies from, and Corvus.ui.guardSelection() clears
 * a leftover highlight on a press outside those.
 *
 * Run:
 *   node tests/test_frontend_selection.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const css = fs.readFileSync(path.join(__dirname, "..", "src", "css", "main.css"), "utf8");

function testBodyIsNotSelectable() {
  assert.match(css, /\nbody \{ -webkit-user-select: none; user-select: none; \}/);
}

function testCopyTargetsStaySelectable() {
  const block = css.match(/input, textarea[^{]*\{[^}]*user-select: text/);
  assert.ok(block, "a rule turns selection back on");
  for (const sel of ["input", "textarea", "pre", ".console-output", ".xterm", ".selectable"]) {
    assert.ok(block[0].includes(sel), sel + " stays selectable");
  }
}

function testAPressOutsideClearsAHighlight() {
  let handler = null;
  let cleared = 0;
  let collapsed = false;
  global.window = global;
  global.Corvus = {};
  global.getSelection = () => ({ get isCollapsed() { return collapsed; }, removeAllRanges() { cleared++; } });
  global.document = { addEventListener: (type, fn) => { if (type === "pointerdown") handler = fn; } };
  require("../src/js/ui.js");
  Corvus.ui.guardSelection();
  assert.ok(handler, "listener registered");

  handler({ target: { closest: () => null } });
  assert.equal(cleared, 1, "cleared on a press outside");

  handler({ target: { closest: () => ({}) } });
  assert.equal(cleared, 1, "kept on a press inside a selectable region");

  collapsed = true;
  handler({ target: { closest: () => null } });
  assert.equal(cleared, 1, "nothing to clear when collapsed");
}

testBodyIsNotSelectable();
testCopyTargetsStaySelectable();
testAPressOutsideClearsAHighlight();
console.log("selection tests passed");
