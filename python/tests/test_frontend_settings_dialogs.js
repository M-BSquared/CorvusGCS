"use strict";

/**
 * The export and import dialogs of Corvus.settingsTransfer, built against a
 * stand-in Corvus.ui and asserted on what they send.
 *
 * The pure helpers have their own test (test_frontend_settings_transfer.js).
 * This one covers the wiring those cannot: that a dialog builds at all (the
 * part picker once reported a change before the dialog had its buttons), that
 * the parts and plugin halves switched off stay out of the request, that a
 * part the file does not carry cannot be chosen, and that the warning about
 * plugin code follows the switches.
 *
 * Run:
 *   node tests/test_frontend_settings_dialogs.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
window.dispatchEvent = () => true;
global.CustomEvent = class CustomEvent {
  constructor(type, o = {}) { this.type = type; this.detail = o.detail; }
};

function el(tag) {
  return {
    tag, className: "", textContent: "", children: [], hidden: false, disabled: false,
    title: "", style: {}, attrs: {},
    appendChild(c) { this.children.push(c); return c; },
    setAttribute(k, v) { this.attrs[k] = v; },
  };
}
global.document = {
  createElement: el,
  createDocumentFragment: () => el("#fragment"),
  body: el("body"),
};

const store = new Map();
global.localStorage = {
  get length() { return store.size; },
  key: (i) => Array.from(store.keys())[i] ?? null,
  getItem: (k) => (store.has(k) ? store.get(k) : null),
  setItem: (k, v) => { store.set(k, String(v)); },
  removeItem: (k) => { store.delete(k); },
};

let toggles;
let buttons;
let messages;
let dialogs;

function resetUi() {
  toggles = [];
  buttons = [];
  messages = [];
  dialogs = [];
}

Corvus.ui = {
  toggle(o) {
    let v = !!o.value;
    const t = {
      el: el("button"),
      label: o.ariaLabel,
      disabled: !!o.disabled,
      getValue: () => v,
      setValue: (n) => { v = !!n; },
      click() {
        if (t.disabled) return;
        v = !v;
        if (o.onChange) o.onChange(v);
      },
    };
    toggles.push(t);
    return t;
  },
  field(o) {
    const f = el("field");
    f.label = o.label;
    f.hint = o.hint;
    if (o.control) f.appendChild(o.control);
    return f;
  },
  label(text) { const l = el("span"); l.textContent = text; return l; },
  actions(children) { const a = el("actions"); children.forEach((c) => a.appendChild(c)); return a; },
  button(o) {
    const b = el("button");
    b.label = o.label;
    b.onClick = o.onClick;
    buttons.push(b);
    return b;
  },
  input(o) { const i = el("input"); i.value = o.value || ""; return i; },
  message() {
    const m = {
      el: el("msg"), text: "", kind: "",
      show(t, k) { m.text = t; m.kind = k; m.el.hidden = false; },
      hide() { m.text = ""; m.kind = ""; m.el.hidden = true; },
    };
    messages.push(m);
    return m;
  },
  modal(o) {
    const d = { title: o.title, body: o.body, closed: false };
    return {
      open() { dialogs.push(d); },
      close() { d.closed = true; },
    };
  },
  setBusy() {},
};

const requests = [];
const LISTING = {
  plugins: [
    { id: "ssh-launcher", name: "SSH Launcher", source: "bundled" },
    { id: "vibration", name: "Vibration", source: "bundled" },
    { id: "field-notes", name: "Field notes", source: "user" },
  ],
  settings: { "ssh-launcher": { connection: "companion" }, "field-notes": { crew: "A" } },
};
Corvus.telemetry = {
  getState: () => ({ armed: false }),
  requestJson(url, opts) {
    requests.push({ url, body: opts && opts.body ? JSON.parse(opts.body) : null });
    if (url === "/api/plugins") return Promise.resolve(LISTING);
    if (url === "/api/settings/export/target") return Promise.resolve({ dir: "/tmp", filename: "x.json" });
    if (url === "/api/settings/export") return Promise.resolve({ ok: true, path: "/tmp/x.json" });
    if (url === "/api/settings/import") return Promise.resolve({ ok: true, plugins_installed: 1 });
    return Promise.reject(new Error(`unexpected ${url}`));
  },
};

require("./../src/js/settings-transfer.js");
const T = Corvus.settingsTransfer;

const toggle = (label) => toggles.find((t) => t.label === label);
const button = (label) => buttons.find((b) => b.label === label);
const lastBody = (url) => requests.filter((r) => r.url === url).pop().body;
const flush = () => new Promise((r) => setImmediate(r));

// ---------------------------------------------------------------------------

async function testTheExportDialogBuildsWithEveryPartOn() {
  resetUi();
  await T.openExport();
  assert.equal(dialogs.length, 1);
  assert.equal(button("Save").disabled, false);
  T.SECTIONS.forEach((s) => assert.equal(toggle(s.label).getValue(), true, s.id));
  // The bundled plugin with nothing saved has no row; the others have their halves.
  assert.equal(toggle("Vibration: Settings"), undefined);
  assert.equal(toggle("SSH Launcher: Files"), undefined);
  assert.equal(toggle("SSH Launcher: Settings").getValue(), true);
  assert.equal(toggle("Field notes: Files").getValue(), true);
}

async function testNoneLeavesNothingToSaveAndAllBringsItBack() {
  resetUi();
  await T.openExport();
  button("None").onClick();
  assert.equal(button("Save").disabled, true);
  assert.equal(toggle("Field notes: Files").getValue(), false);
  button("All").onClick();
  assert.equal(button("Save").disabled, false);
  assert.equal(toggle("Field notes: Files").getValue(), true);
}

async function testWhatIsSwitchedOffStaysOutOfTheExport() {
  resetUi();
  store.clear();
  localStorage.setItem("corvus.theme", "green");
  localStorage.setItem("corvus.link.recent", "[]");
  await T.openExport();
  toggle("Connections").click();
  toggle("Field notes: Files").click();
  await button("Save").onClick();
  const body = lastBody("/api/settings/export");
  assert.deepEqual(body.sections, ["interface", "layout", "map", "vehicle", "folders", "plugins"]);
  assert.deepEqual(body.plugin_settings, ["ssh-launcher", "field-notes"]);
  assert.deepEqual(body.plugin_files, []);
  assert.deepEqual(body.browser, { "corvus.theme": "green" });
  assert.equal(body.include_secrets, false);
}

async function testPluginsSwitchedOffTakeNoPluginAtAll() {
  resetUi();
  await T.openExport();
  toggle("Plugins").click();
  await button("Save").onClick();
  const body = lastBody("/api/settings/export");
  assert.ok(!body.sections.includes("plugins"));
  assert.deepEqual(body.plugin_settings, []);
  assert.deepEqual(body.plugin_files, []);
}

const FILE = {
  kind: "corvus-settings",
  format: 2,
  sections: ["interface", "plugins"],
  config: { theme: { name: "green" } },
  plugins: { "field-notes": { crew: "A" } },
  plugin_files: { mine: { "plugin.json": Buffer.from('{"name":"Mine"}').toString("base64") } },
  browser: { "corvus.theme": "green" },
};

async function testAPartTheFileLacksCannotBeImported() {
  resetUi();
  await T.openImport(FILE, "station.json");
  assert.equal(toggle("Connections").disabled, true);
  assert.equal(toggle("Connections").getValue(), false);
  assert.equal(toggle("Interface and controls").getValue(), true);
  assert.equal(toggle("Mine: Settings"), undefined);
  assert.equal(toggle("Mine: Files").getValue(), true);
}

async function testTheCodeWarningFollowsThePluginFiles() {
  resetUi();
  await T.openImport(FILE, "station.json");
  const code = messages.find((m) => /Plugin files are code/.test(m.text));
  assert.ok(code, "no warning while plugin files are switched on");
  toggle("Mine: Files").click();
  assert.equal(code.text, "");
  toggle("Mine: Files").click();
  assert.match(code.text, /Plugin files are code/);
}

async function testImportSendsTheChoiceAndReportsBack() {
  resetUi();
  store.clear();
  let reported = null;
  await T.openImport(FILE, "station.json", { onImported: (res) => { reported = res; } });
  toggle("Field notes: Settings").click();
  await button("Import").onClick();
  await flush();
  const body = lastBody("/api/settings/import");
  assert.deepEqual(body.sections, ["interface", "plugins"]);
  assert.deepEqual(body.plugin_settings, []);
  assert.deepEqual(body.plugin_files, ["mine"]);
  assert.deepEqual(reported, { ok: true, plugins_installed: 1 });
  assert.equal(localStorage.getItem("corvus.theme"), "green");
  assert.equal(dialogs[0].closed, true);
  assert.equal(dialogs[1].title, "Import settings");
}

// ---------------------------------------------------------------------------

const tests = [
  testTheExportDialogBuildsWithEveryPartOn,
  testNoneLeavesNothingToSaveAndAllBringsItBack,
  testWhatIsSwitchedOffStaysOutOfTheExport,
  testPluginsSwitchedOffTakeNoPluginAtAll,
  testAPartTheFileLacksCannotBeImported,
  testTheCodeWarningFollowsThePluginFiles,
  testImportSendsTheChoiceAndReportsBack,
];

(async function run() {
  let failed = 0;
  for (const t of tests) {
    try {
      await t();
      console.log(`ok   ${t.name}`);
    } catch (err) {
      failed += 1;
      console.log(`FAIL ${t.name}\n${err.stack || err}`);
    }
  }
  if (failed) {
    console.log(`${failed} of ${tests.length} failed`);
    process.exit(1);
  }
  console.log(`${tests.length} passed`);
})();
