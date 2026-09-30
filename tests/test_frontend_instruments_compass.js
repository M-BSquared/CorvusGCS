"use strict";

/**
 * Frontend tests for the flight compass lock (Corvus.instruments.setNoseUp).
 *
 * North up (the default): the card is fixed and the needle carries the
 * heading. Nose up: the card turns by minus the heading, the needle stays at
 * the top, and every label turns back so it stays upright.
 *
 * Run:
 *   node tests/test_frontend_instruments_compass.js
 */

const assert = require("node:assert/strict");
const path = require("node:path");

global.window = global;
global.Corvus = {};

function makeEl(tag) {
  const attrs = {};
  const e = {
    tagName: String(tag).toUpperCase(), children: [], textContent: "", innerHTML: "",
    setAttribute: (k, v) => { attrs[k] = String(v); },
    getAttribute: (k) => (k in attrs ? attrs[k] : null),
    removeAttribute: (k) => { delete attrs[k]; },
    appendChild: (c) => { e.children.push(c); return c; },
    querySelector: () => makeEl("span"),
    attrs,
  };
  Object.defineProperty(e, "innerHTML", {
    get: () => "", set: () => { e.children.length = 0; },
  });
  return e;
}

const ids = {};
global.document = {
  getElementById: (id) => (ids[id] = ids[id] || makeEl(id === "compassValue" ? "span" : "g")),
  createElementNS: (_ns, name) => makeEl(name),
  createElement: (name) => makeEl(name),
};

let subscriber = null;
Corvus.telemetry = { subscribe: (cb) => { subscriber = cb; } };
Corvus.anim = {
  add() {}, wake() {},
  reducedMotion: () => true,
  normAngle: (a) => ((a % 360) + 360) % 360,
};
Corvus.units = { formatLength: String, formatSpeed: String };

require(path.join(__dirname, "..", "src", "js", "instruments.js"));
const ins = Corvus.instruments;

// Chosen before init, as app.js may do when the config lands first.
ins.setNoseUp(false);
ins.init(null);

const card = ids.compassCard;
const arrow = ids.compassArrow;
const labels = card.children.filter((c) => c.tagName === "TEXT");
assert.equal(labels.length, 12, "four cardinals and eight digits");

function fly(heading) {
  subscriber({ connected: false, heading, pitch: 0, roll: 0 });
}

// North up: needle turns, card stays.
fly(90);
assert.equal(arrow.getAttribute("transform"), "rotate(90)");
assert.equal(card.getAttribute("transform"), "rotate(0)");
assert.ok(labels.every((t) => t.getAttribute("transform") === null));
assert.equal(ids.compassValue.textContent, "090°");

// Nose up: card turns under a fixed needle, labels turn back about themselves.
ins.setNoseUp(true);
assert.equal(ins.noseUp(), true);
assert.equal(arrow.getAttribute("transform"), "rotate(0)");
assert.equal(card.getAttribute("transform"), "rotate(-90)");
const n = labels.find((t) => t.textContent === "N");
assert.equal(n.getAttribute("transform"),
  `rotate(90 ${n.getAttribute("x")} ${n.getAttribute("y")})`);

fly(270);
assert.equal(card.getAttribute("transform"), "rotate(-270)");
assert.equal(arrow.getAttribute("transform"), "rotate(0)");
assert.equal(ids.compassValue.textContent, "270°", "the readout is unchanged by the lock");

// And back to north up, with the label turns cleared.
ins.setNoseUp(false);
assert.equal(card.getAttribute("transform"), "rotate(0)");
assert.equal(arrow.getAttribute("transform"), "rotate(270)");
assert.ok(labels.every((t) => t.getAttribute("transform") === null));

console.log("test_frontend_instruments_compass: ok");
