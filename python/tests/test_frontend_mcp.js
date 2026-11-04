"use strict";

/**
 * Frontend tests for the MCP server's browser half: plugin tools
 * (Corvus.plugins mcpTools / runMcpTool), the map indicator's states
 * (Corvus.mcp.badgeState) and the settings card's OpenCode block
 * (Corvus.settingsMcp). Schwalby's tools are tested in its own folder.
 *
 * Plain Node-runnable assertions with a small DOM stub, the pattern of
 * tests/test_frontend_plugins.js.
 *
 * Run:
 *   node tests/test_frontend_mcp.js
 */
const assert = require("node:assert/strict");

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

require("../src/js/ui.js");
require("../src/js/plugins.js");
require("../src/js/mcp.js");
require("../src/js/settings-mcp.js");

const tests = [];
function test(name, fn) { tests.push([name, fn]); }

// ---------------------------------------------------------------------------
// Plugin tools
// ---------------------------------------------------------------------------

let changes = 0;
const realChanged = Corvus.mcp.toolsChanged;
Corvus.mcp.toolsChanged = () => { changes += 1; };

const saved = {};
const telemetry = {
  subscribe: () => () => {}, getState: () => ({}),
  requestJson: (url, opts) => {
    if (url === "/api/plugins/settings") {
      const body = JSON.parse(opts.body);
      saved[body.id] = body.settings;
      return Promise.resolve({ ok: true, settings: body.settings });
    }
    return Promise.resolve({});
  },
  postAction: () => Promise.resolve({ ok: true }),
};
Corvus.plugins.init(makeEl("div"), telemetry);

test("a plugin's tools are announced without their handlers, bad ones dropped", () => {
  const before = changes;
  Corvus.plugins.register("probe", {
    name: "Probe", init() {}, destroy() {},
    mcpTools: (api) => [
      { name: "echo", description: "Say it back", readOnly: true,
        inputSchema: { type: "object", properties: { text: { type: "string" } } },
        handler: (args, a) => ({ said: args.text, same: a === api || typeof a.getSettings === "function" }) },
      { name: "no handler" },
      { name: "bad name!", handler: () => 1 },
      { name: "echo", handler: () => "duplicate" },
    ],
  });
  assert.ok(changes > before, "registering tells the MCP window module");
  const listed = Corvus.plugins.mcpTools().filter((t) => t.plugin === "probe");
  assert.equal(listed.length, 1);
  assert.deepEqual(listed[0], {
    plugin: "probe", plugin_name: "Probe", name: "echo", title: "echo",
    description: "Say it back",
    input_schema: { type: "object", properties: { text: { type: "string" } } },
    confirm: false, read_only: true,
  });
  assert.equal(typeof listed[0].handler, "undefined");
});

test("runMcpTool runs the handler and refuses an unknown tool", async () => {
  const out = await Corvus.plugins.runMcpTool("probe", "echo", { text: "hi" });
  assert.equal(out.said, "hi");
  await assert.rejects(Corvus.plugins.runMcpTool("probe", "nope", {}), /no tool nope/);
  await assert.rejects(Corvus.plugins.runMcpTool("ghost", "echo", {}));
});

test("a handler that throws is the reason the agent is told", async () => {
  Corvus.plugins.register("grumpy", {
    name: "Grumpy", init() {}, destroy() {},
    mcpTools: [{ name: "go", handler: () => { throw new Error("not today"); } }],
  });
  await assert.rejects(Corvus.plugins.runMcpTool("grumpy", "go", {}), /not today/);
});

// ---------------------------------------------------------------------------
// The indicator and the settings card
// ---------------------------------------------------------------------------

test("the map pill shows only what needs the operator's eye", () => {
  const b = Corvus.mcp.badgeState;
  assert.equal(b(null), null);
  assert.equal(b({ enabled: false, control: { kind: "manual" } }), null);
  assert.equal(b({ enabled: true, active: true, writes: false }), null, "a reader is no pill");
  assert.equal(b({ enabled: true, active: true, writes: true }).level, "on");
  assert.equal(b({ enabled: true, pending: [{ id: "x" }] }).level, "ask");
  const flying = b({ enabled: true, active: true, writes: true, control: { kind: "velocity" } });
  assert.equal(flying.level, "control");
  assert.match(flying.text, /flying \(velocity\)/);
});

test("the OpenCode block is the remote MCP shape with the token as a header", () => {
  const cfg = JSON.parse(Corvus.settingsMcp.opencodeConfig("http://127.0.0.1:8000/mcp", "abc"));
  assert.equal(cfg.$schema, "https://opencode.ai/config.json");
  assert.deepEqual(cfg.mcp.corvus, {
    type: "remote", url: "http://127.0.0.1:8000/mcp", enabled: true, oauth: false,
    timeout: 60000, headers: { Authorization: "Bearer abc" },
  });
});

test("the client line says who is connected", () => {
  const line = Corvus.settingsMcp.clientsLine;
  assert.match(line({ enabled: false }), /^Off/);
  assert.match(line({ enabled: true, clients: [] }), /No AI client/);
  assert.equal(line({ enabled: true, clients: [{ name: "opencode", version: "1.2", seen_s: 4 }] }),
    "Connected: opencode 1.2 (4 s ago)");
});

test("the server card counts the permissions that are on and the ones that ask", () => {
  const catalog = [
    { id: "telemetry", writes: false }, { id: "arm", writes: true },
    { id: "flight", writes: true }, { id: "goto", writes: true },
  ];
  const sum = Corvus.settingsMcp.permissionSummary;
  assert.equal(sum({ permissions: { telemetry: true }, confirm: {} }, catalog), "1 of 4 permissions on.");
  assert.equal(sum({
    permissions: { telemetry: true, arm: true, goto: true },
    confirm: { telemetry: true, arm: true, flight: true },
  }, catalog), "3 of 4 permissions on, 1 of them ask you first.");
  assert.equal(sum(null, []), "0 of 0 permissions on.");
});

test("no text the operator reads carries a dash", () => {
  const fs = require("node:fs");
  const path = require("node:path");
  ["src/js/mcp.js", "src/js/settings-mcp.js"].forEach((rel) => {
    const src = fs.readFileSync(path.join(__dirname, "..", rel), "utf8");
    const strings = src.match(/"[^"\n]*"|`[^`\n]*`/g) || [];
    strings.forEach((s) => {
      assert.ok(!/[–—]/.test(s), `${rel}: ${s}`);
      assert.ok(!/\w - \w/.test(s), `${rel}: ${s}`);
    });
  });
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
  Corvus.mcp.toolsChanged = realChanged;
  if (failed) {
    console.log(`\n${failed} of ${tests.length} MCP frontend tests failed.`);
    process.exit(1);
  }
  console.log(`\nAll ${tests.length} MCP frontend tests passed.`);
  process.exit(0);
})();
