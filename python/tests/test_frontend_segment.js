"use strict";

/**
 * Frontend tests for Corvus.ui.segment — segmented control component.
 *
 * Plain Node-runnable assertions (no browser, no test runner).
 *
 * Run:
 *   node tests/test_frontend_segment.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

// Browser-ish globals so ui.js loads in Node.
global.window = global;
global.Corvus = {};

global.Event = class Event {
  constructor(type) { this.type = type; }
};

function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {},
    type: "", hidden: false, disabled: false, value: "", id: "",
    tabIndex: 0,
    _attrs: {}, _listeners: {}, _props: {}, _isEl: true,
  };
  e.style = {
    setProperty(k, v) { e._props[k] = String(v); },
    getPropertyValue(k) { return k in e._props ? e._props[k] : ""; },
  };
  e.classList = {
    add(...cls) {
      cls.forEach((c) => {
        if (!e.className.split(/\s+/).includes(c)) {
          e.className = (e.className + " " + c).trim();
        }
      });
    },
    remove(...cls) {
      e.className = e.className.split(/\s+/).filter((c) => !cls.includes(c)).join(" ");
    },
    contains(c) {
      return e.className.split(/\s+/).includes(c);
    },
  };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.hasAttribute = (k) => k in e._attrs;
  e.removeAttribute = (k) => { delete e._attrs[k]; };
  e.appendChild = (child) => { e.children.push(child); return child; };
  e.removeChild = (child) => {
    const i = e.children.indexOf(child);
    if (i >= 0) e.children.splice(i, 1);
    return child;
  };
  e.addEventListener = (type, fn) => {
    e._listeners[type] = e._listeners[type] || [];
    e._listeners[type].push(fn);
  };
  e.click = () => {
    (e._listeners.click || []).forEach((fn) => fn({ type: "click" }));
  };
  return e;
}

global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ textContent: String(t), children: [] }),
  querySelectorAll: () => [],
};

// Load ui.js
require(path.join(__dirname, "../src/js/ui.js"));

function testSegmentStructureAndSelection() {
  const changes = [];
  const seg = Corvus.ui.segment({
    ariaLabel: "Connection type",
    value: "net",
    options: [
      { value: "serial", label: "Serial", icon: "cable" },
      { value: "net", label: "UDP / TCP", icon: "network" },
    ],
    onChange: (v) => changes.push(v),
  });

  assert.equal(seg.el.getAttribute("role"), "radiogroup");
  assert.equal(seg.el.getAttribute("aria-label"), "Connection type");
  assert.ok(seg.el.className.includes("ui-segment"));

  const buttons = seg.el.children;
  assert.equal(buttons.length, 2);

  assert.equal(buttons[0].dataset.value, "serial");
  assert.equal(buttons[0].getAttribute("aria-checked"), "false");
  assert.equal(buttons[0].getAttribute("aria-selected"), "false");
  assert.equal(buttons[0].tabIndex, -1);

  assert.equal(buttons[1].dataset.value, "net");
  assert.equal(buttons[1].getAttribute("aria-checked"), "true");
  assert.equal(buttons[1].getAttribute("aria-selected"), "true");
  assert.equal(buttons[1].tabIndex, 0);

  assert.equal(seg.getValue(), "net");

  // Click serial button
  buttons[0].click();
  assert.equal(seg.getValue(), "serial");
  assert.equal(buttons[0].getAttribute("aria-checked"), "true");
  assert.equal(buttons[1].getAttribute("aria-checked"), "false");
  assert.deepEqual(changes, ["serial"]);

  // Set programmatically
  seg.setValue("net");
  assert.equal(seg.getValue(), "net");
  assert.equal(buttons[1].getAttribute("aria-checked"), "true");
}

function testSegmentKeyboardNavigation() {
  const changes = [];
  const seg = Corvus.ui.segment({
    value: "ping",
    options: [
      { value: "ping", label: "Ping" },
      { value: "ssh", label: "SSH" },
    ],
    onChange: (v) => changes.push(v),
  });

  const buttons = seg.el.children;
  const keydown = (btn, key) => {
    let prevented = false;
    (btn._listeners.keydown || []).forEach((fn) => fn({
      key,
      preventDefault: () => { prevented = true; },
    }));
    return prevented;
  };

  // ArrowRight moves from ping to ssh
  assert.ok(keydown(buttons[0], "ArrowRight"));
  assert.equal(seg.getValue(), "ssh");
  assert.deepEqual(changes, ["ssh"]);

  // ArrowRight wraps from ssh to ping
  assert.ok(keydown(buttons[1], "ArrowRight"));
  assert.equal(seg.getValue(), "ping");
  assert.deepEqual(changes, ["ssh", "ping"]);

  // ArrowLeft wraps from ping to ssh
  assert.ok(keydown(buttons[0], "ArrowLeft"));
  assert.equal(seg.getValue(), "ssh");
}

function run() {
  testSegmentStructureAndSelection();
  testSegmentKeyboardNavigation();
  console.log("all Corvus.ui.segment tests passed");
}

run();
