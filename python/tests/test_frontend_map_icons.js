"use strict";

/**
 * Frontend tests for Corvus.mapIcons, the named symbols a plugin puts on the
 * Home map: every symbol is plain path data on the 24 grid, names are unique
 * and safe, and a name resolves to a copy nobody can corrupt.
 *
 * Run:
 *   node tests/test_frontend_map_icons.js
 */
const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
require("../src/js/map-icons.js");
const icons = Corvus.mapIcons;

const PATH_RE = /^[MmLlHhVvCcSsQqTtAaZz0-9\s.,eE+-]+$/;

function testEverySymbolIsPlainPathData() {
  const list = icons.list();
  assert.ok(list.length >= 14, "a useful set");
  list.forEach(({ name, label }) => {
    assert.match(name, icons.NAME_RE, `${name} is a safe name`);
    assert.ok(label && label === label.trim(), `${name} has a label`);
    const paths = icons.get(name);
    assert.ok(paths.length > 0 && paths.length <= 12, `${name} has a few paths`);
    paths.forEach((d) => {
      assert.ok(d.length <= 600, `${name}: a path of sane length`);
      assert.match(d, PATH_RE, `${name}: path data only`);
      assert.match(d, /^[Mm]/, `${name}: every path starts with a move`);
    });
  });
}

function testNamesAreUniqueAndOrdered() {
  const names = icons.list().map((i) => i.name);
  assert.equal(new Set(names).size, names.length, "no name twice");
  assert.equal(names[0], "gate", "the order they were defined in");
}

function testAnUnknownOrUnsafeNameIsNull() {
  assert.equal(icons.get("no-such-icon"), null);
  assert.equal(icons.get(""), null);
  assert.equal(icons.get(null), null);
  assert.equal(icons.get({ toString: () => "tree" }), null, "only a real string is a name");
  assert.equal(icons.has("tree"), true);
  assert.equal(icons.has("__proto__"), false);
  assert.equal(icons.has("constructor"), false);
}

function testAGetIsACopy() {
  const first = icons.get("tree");
  first.push("M0 0h99");
  first[0] = "<script>";
  const again = icons.get("tree");
  assert.ok(!again.includes("M0 0h99"));
  assert.match(again[0], PATH_RE, "the stored symbol is untouched");
  const listed = icons.list();
  listed[0].label = "changed";
  assert.notEqual(icons.list()[0].label, "changed", "and so is the list");
}

const tests = [
  testEverySymbolIsPlainPathData,
  testNamesAreUniqueAndOrdered,
  testAnUnknownOrUnsafeNameIsNull,
  testAGetIsACopy,
];

let failed = 0;
for (const t of tests) {
  try {
    t();
    console.log(`ok   - ${t.name}`);
  } catch (err) {
    failed++;
    console.error(`FAIL - ${t.name}`);
    console.error(`      ${err && err.stack ? err.stack.split("\n").join("\n      ") : err}`);
  }
}
if (failed) {
  console.error(`\n${failed}/${tests.length} map icon test(s) FAILED`);
  process.exit(1);
}
console.log(`\nAll ${tests.length} map icon tests passed.`);
process.exit(0);
