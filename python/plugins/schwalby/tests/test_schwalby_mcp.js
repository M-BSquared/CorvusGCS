"use strict";

/**
 * Schwalby's tools for an AI client over Corvus' MCP server: its two
 * switches, buttons it may add (here only, marked, waiting for the operator),
 * buttons it may change (its own only), and presses with the shelf closed.
 *
 * Run:
 *   node plugins/schwalby/tests/test_schwalby_mcp.js
 */
const assert = require("node:assert/strict");
const path = require("node:path");

const CORVUS = process.env.CORVUS_ROOT
  ? path.resolve(process.env.CORVUS_ROOT)
  : path.join(__dirname, "..", "..", "..");

global.window = global;
global.Corvus = {};
window.addEventListener = () => {};
window.removeEventListener = () => {};
window.dispatchEvent = () => true;
global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
window.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });

function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(), className: "", textContent: "",
    children: [], dataset: {}, style: { setProperty() {} }, hidden: false, _attrs: {}, _isEl: true,
  };
  e.classList = {
    add(c) { if (!e.className.split(" ").includes(c)) e.className = (e.className + " " + c).trim(); },
    remove(c) { e.className = e.className.split(" ").filter((x) => x !== c).join(" "); },
    toggle() {}, contains: (c) => e.className.split(" ").includes(c),
  };
  e.appendChild = (c) => { e.children.push(c); return c; };
  e.append = (...n) => n.forEach(e.appendChild);
  e.insertBefore = (n) => { e.children.unshift(n); return n; };
  e.removeChild = (c) => c;
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); };
  e.getAttribute = (k) => e._attrs[k] || null;
  e.addEventListener = () => {};
  e.removeEventListener = () => {};
  e.querySelector = () => null;
  e.querySelectorAll = () => [];
  Object.defineProperty(e, "innerHTML", { get: () => "", set() { e.children.length = 0; } });
  return e;
}
global.document = {
  createElement: makeEl, createTextNode: (t) => ({ textContent: String(t) }),
  createDocumentFragment: () => makeEl("fragment"),
  getElementById: () => null, querySelectorAll: () => [], addEventListener() {},
  head: makeEl("head"), body: makeEl("body"),
};

require(path.join(CORVUS, "src", "js", "ui.js"));
require(path.join(CORVUS, "src", "js", "plugins.js"));
Corvus.plugins.init(makeEl("div"), {
  subscribe: () => () => {}, getState: () => ({}),
  requestJson: () => Promise.resolve({}), postAction: () => Promise.resolve({ ok: true }),
});

const tests = [];
function test(name, fn) { tests.push([name, fn]); }

// ---------------------------------------------------------------------------
// Schwalby
// ---------------------------------------------------------------------------

require(path.join(__dirname, "..", "schwalby.js"));
const S = Corvus.pluginSchwalby;

function fakeApi(options, settings) {
  const state = { settings: JSON.parse(JSON.stringify(settings || {})), posts: [], lines: [] };
  return {
    state,
    getOptions: () => options,
    getSettings: () => JSON.parse(JSON.stringify(state.settings)),
    saveSettings: (next) => { state.settings = JSON.parse(JSON.stringify(next)); return Promise.resolve(next); },
    postJson: (url, body) => {
      state.posts.push([url, body]);
      if (url === "/api/local/run") return Promise.resolve({ ok: true, pid: 42, command: body.command });
      return Promise.resolve({ ok: true, connected: true });
    },
    console: (text) => state.lines.push(text),
    notification: () => {},
  };
}
const toolNames = (api) => S.mcpTools(api).map((t) => t.name);
const tool = (api, name) => S.mcpTools(api).find((t) => t.name === name);

test("Schwalby offers nothing until one of its switches is on", () => {
  assert.deepEqual(toolNames(fakeApi({})), []);
  assert.deepEqual(toolNames(fakeApi({ mcp_run: true })), ["list_buttons", "press_button"]);
  assert.deepEqual(toolNames(fakeApi({ mcp_edit: true })),
    ["list_buttons", "add_button", "update_button", "remove_button"]);
  assert.deepEqual(S.OPTIONS.map((o) => [o.key, o.default]), [["mcp_edit", false], ["mcp_run", false]]);
});

test("a button the AI client adds runs here, carries the mark, and waits", async () => {
  const api = fakeApi({ mcp_edit: true, mcp_run: true }, { companion: { name: "Pi" } });
  const res = await tool(api, "add_button").handler(
    { label: "Logger", command: "rec.sh", target: "ssh", connection: "pi" });
  const b = api.state.settings.buttons[0];
  assert.equal(res.id, b.id);
  assert.equal(b.target, "local");
  assert.equal(b.connection, "");
  assert.equal(b.by_mcp, true);
  assert.equal(b.pending, true);
  assert.deepEqual(api.state.settings.companion, { name: "Pi" }, "the companion is kept");
  await assert.rejects(Promise.resolve().then(() => tool(api, "press_button").handler({ id: b.id })),
    /not allowed this button yet/);
});

test("only the AI client's own buttons can be changed, and a new command waits again", async () => {
  const mine = { id: "m1", label: "Mine", command: "a", target: "local", by_mcp: true };
  const theirs = { id: "o1", label: "Video", command: "gst", target: "ssh", connection: "pi" };
  const api = fakeApi({ mcp_edit: true }, { buttons: [mine, theirs] });
  await assert.rejects(Promise.resolve().then(() => tool(api, "update_button").handler({ id: "o1", label: "x" })),
    /operator's/);
  await assert.rejects(Promise.resolve().then(() => tool(api, "remove_button").handler({ id: "o1" })),
    /operator's/);
  await tool(api, "update_button").handler({ id: "m1", label: "Renamed" });
  let m = api.state.settings.buttons.find((b) => b.id === "m1");
  assert.equal(m.label, "Renamed");
  assert.equal(m.pending, undefined, "a new label runs nothing new");
  await tool(api, "update_button").handler({ id: "m1", command: "rm -rf ~" });
  m = api.state.settings.buttons.find((b) => b.id === "m1");
  assert.equal(m.pending, true);
  await tool(api, "remove_button").handler({ id: "m1" });
  assert.deepEqual(api.state.settings.buttons.map((b) => b.id), ["o1"]);
});

test("pressing an allowed button with the shelf closed uses the shelf's own endpoints", async () => {
  const api = fakeApi({ mcp_run: true }, {
    buttons: [{ id: "b1", label: "Rec", command: "rec.sh", directory: "~/logs", target: "local",
      mode: "background" }],
  });
  const out = await tool(api, "press_button").handler({ id: "b1" });
  assert.equal(out.ok, true);
  assert.deepEqual(api.state.posts[0], ["/api/local/run", { directory: "~/logs", command: "rec.sh" }]);
  const listed = await tool(api, "list_buttons").handler({});
  assert.equal(listed.buttons[0].runs_on, "this computer");
  assert.equal(listed.you_may_add, false);
});

test("a terminal button pressed headless opens its own session when none is up", async () => {
  const api = fakeApi({ mcp_run: true }, {
    buttons: [{ id: "t1", label: "Mission", command: "./fly", target: "local", mode: "terminal" }],
  });
  const sends = [];
  api.postJson = (url, body) => {
    api.state.posts.push([url, body]);
    if (url === "/api/ssh/send") {
      sends.push(body);
      return Promise.resolve({ ok: sends.length > 1 });
    }
    return Promise.resolve({ ok: true, connected: true });
  };
  const out = await tool(api, "press_button").handler({ id: "t1" });
  assert.equal(out.ok, true);
  assert.deepEqual(api.state.posts.map((p) => p[0]),
    ["/api/ssh/send", "/api/local/connect", "/api/ssh/send"]);
  assert.equal(api.state.posts[1][1].name, "schwalby/t1");
});

test("the AI marks survive only on a button the AI client added", () => {
  assert.equal(S.coerceButton({ command: "x", pending: true }).pending, undefined);
  const b = S.coerceButton({ command: "x", by_mcp: true, pending: true });
  assert.equal(b.by_mcp, true);
  assert.equal(b.pending, true);
  assert.equal("by_mcp" in S.coerceButton({ command: "x" }), false);
});

test("Schwalby's switches are its plugin options and its tools reach the registry", () => {
  const listed = Corvus.plugins.mcpTools().filter((t) => t.plugin === "schwalby");
  assert.deepEqual(listed, [], "both switches start off");
  assert.ok(Corvus.plugins.hasOptions("schwalby"));
});

(async () => {
  let failed = 0;
  for (const [name, fn] of tests) {
    try {
      await fn();
      console.log(`ok   - ${name}`);
    } catch (err) {
      failed += 1;
      console.log(`FAIL - ${name}\n${err && err.stack}`);
    }
  }
  if (failed) {
    console.log(`\n${failed} of ${tests.length} Schwalby MCP tests failed.`);
    process.exit(1);
  }
  console.log(`\nAll ${tests.length} Schwalby MCP tests passed.`);
  process.exit(0);
})();
