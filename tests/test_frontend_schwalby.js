"use strict";

/**
 * Frontend tests for the Schwalby plugin (plugins/schwalby): the launcher
 * shelf whose buttons run on this computer or over SSH, picked per button
 * with its Local / SSH segment.
 *
 * Plain Node-runnable assertions with the same small DOM stub as
 * tests/test_frontend_plugins.js: the plugin is mounted against a stub api
 * that models the backend closely enough to matter (a connect makes a session
 * live, a send only lands in a live one), and every call it makes is
 * recorded.
 *
 * Run:
 *   node tests/test_frontend_schwalby.js
 */

const assert = require("node:assert/strict");

// ---------------------------------------------------------------------------
// Browser-ish globals so plugins.js and the folder plugins load and run in Node.
// ---------------------------------------------------------------------------
global.window = global;
global.Corvus = {};

// The chart modules subscribe to corvus:themechange on window, so the listener
// pair has to exist for the theme-reactive redraw path to be exercised at all.
const windowListeners = {};
window.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
window.removeEventListener = (t, cb) => {
  const list = windowListeners[t] || [];
  const i = list.indexOf(cb);
  if (i >= 0) list.splice(i, 1);
};
// api.notification dispatches corvus:notification for the topbar's board, so
// the stub has to deliver it — without one, the call was silently swallowed by
// the guard around it and the board half of a notification went untested.
window.dispatchEvent = (event) => {
  (windowListeners[event && event.type] || []).forEach((cb) => cb(event));
  return true;
};


global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};

// prefers-reduced-motion toggle (api.reducedMotion reads this).
let reducedMotion = false;
window.matchMedia = (query) => ({
  matches: reducedMotion && String(query).includes("prefers-reduced-motion"),
  media: query,
  addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
});


// lucide is optional in the source (refreshIcons no-ops when absent). Leave it
// undefined so we also exercise that path.
// window.Plotly is set per-test below.

// ---------------------------------------------------------------------------
// Minimal DOM stub. The source builds structure with createElement + appendChild
// (no innerHTML-based querySelector), so this stub only needs to track
// children, className/classList, textContent, dataset, listeners, and an
// innerHTML string (cleared children when set to "").
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "",
    textContent: "",
    children: [],
    dataset: {},
    style: {},
    type: "",
    hidden: false,
    disabled: false,
    value: "",
    id: "",
    _attrs: {},
    _listeners: {},
    _isEl: true,
  };
  let _html = "";
  Object.defineProperty(e, "innerHTML", {
    get() { return _html; },
    set(v) {
      _html = String(v);
      if (_html === "") e.children.length = 0;   // mirror: clearing HTML drops children
    },
  });
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) { const has = e.classList.contains(c); const next = force === undefined ? !has : !!force; if (next) e.classList.add(c); else e.classList.remove(c); return next; },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  // parentNode is maintained like a real DOM so the standard
  // `node.parentNode.removeChild(node)` removal idiom works under the stub —
  // Corvus.ui.modal.close() uses it to unmount a dialog.
  e.appendChild = (c) => { c.parentNode = e; e.children.push(c); return c; };
  e.append = (...nodes) => { nodes.forEach((n) => e.appendChild(n)); };
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; return c; };
  e.insertBefore = (n, ref) => { const i = ref ? e.children.indexOf(ref) : e.children.length; if (i < 0) e.children.push(n); else e.children.splice(i, 0, n); return n; };
  Object.defineProperty(e, "firstChild", { get() { return e.children[0] || null; } });
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.addEventListener = (type, cb) => { (e._listeners[type] = e._listeners[type] || []).push(cb); };
  e.removeEventListener = () => {};
  e.querySelector = (sel) => querySel(e.children, sel)[0] || null;
  e.querySelectorAll = (sel) => querySel(e.children, sel);
  return e;
}

function querySel(children, sel) {
  // Supports a single class selector (".cls") or tag name. Walks recursively.
  const out = [];
  const wantTag = sel && sel[0] !== ".";
  const classes = sel ? sel.split(".").filter(Boolean) : [];
  function walk(list) {
    for (const c of list) {
      if (!c || !c._isEl) continue;
      const ok = wantTag ? c.tagName === sel.toUpperCase() : classes.every((cl) => c.className.split(/\s+/).includes(cl));
      if (ok) out.push(c);
      if (c.children) walk(c.children);
    }
  }
  walk(children);
  return out;
}

// <head>, plus the loader's script/link elements. Corvus.plugins.loadInstalled
// appends a <script> and waits for its onload, which no stub can produce by
// actually fetching — so appending one here fires the callback the appended
// element was given, and `scriptBehaviour` decides whether that is a load or an
// error. `appendedScripts` is what the assertions read.
const appendedScripts = [];
const appendedStyles = [];
let scriptBehaviour = () => "load";

const head = makeEl("head");
head.appendChild = (el) => {
  el.parentNode = head;
  head.children.push(el);
  if (el.tagName === "SCRIPT") {
    appendedScripts.push(el.src);
    const outcome = scriptBehaviour(el.src);
    setTimeout(() => {
      if (outcome === "error") { if (el.onerror) el.onerror(new Error("failed")); return; }
      if (typeof outcome === "function") outcome(el.src);
      if (el.onload) el.onload();
    }, 0);
  } else if (el.tagName === "LINK") {
    appendedStyles.push(el.href);
  }
  return el;
};

// The toast stack mounts here, so the push half of api.notification is real
// in these tests rather than swallowed by the guard around it.
const body = makeEl("body");

global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  head,
  body,
};

// The launcher asks before removing a button that is running something.
window.confirm = () => true;

// Flush the microtask queue (for the plugin's best-effort postAction promises).
function flushMicrotasks() { return new Promise((r) => setTimeout(r, 0)); }

// ---------------------------------------------------------------------------
// Load order mirrors the browser's: ui.js and plugins.js are <script> tags in
// index.html; the plugin is a folder plugin and registers itself as it loads.
// ---------------------------------------------------------------------------
require("../src/js/ui.js");
require("../src/js/plugins.js");
require("../plugins/schwalby/schwalby.js");

const S = Corvus.pluginSchwalby;

/** Fire an element's first click listener. */
function click(el) { el._listeners.click[0](); }

/** Pick an option in a <select> built by Corvus.ui.select. */
function changeTo(el, value) {
  el.value = value;
  (el._listeners.change || []).forEach((cb) => cb());
}

/** Type into an input built by Corvus.ui.input. */
function typeInto(el, value) {
  el.value = value;
  (el._listeners.input || []).forEach((cb) => cb());
}

function dismissToasts() {
  querySel(body.children, ".ui-toast-close").forEach((btn) => click(btn));
}

const mounted = [];

/**
 * Mount Schwalby against a stub api. `opts`: saved (its settings), sessions
 * (live session names), connections, connect / localConnect (the connect
 * answers), run / localRun (the background answers), localStatus (an object,
 * or a promise for one), holdDisconnect (keep /api/ssh/disconnect pending
 * until releaseDisconnect()).
 */
function mount(opts) {
  const o = opts || {};
  const calls = [];
  const connections = o.connections || [
    { name: "companion", host: "10.0.0.7", username: "pilot", port: 22 },
    { name: "ground", host: "10.0.0.2", username: "ops", port: 22 },
  ];
  let sessions = (o.sessions || []).map((name) => ({ name, connected: true }));
  let settings = o.saved || {};
  let releaseDisconnect = () => {};
  const terminals = [];
  const termOpts = [];
  const notes = [];
  const consoleLines = [];
  const container = makeEl("div");
  mounted.push(container);
  S.init(container, {
    requestJson: (url) => {
      calls.push({ url });
      if (url === "/api/ssh/connections") return Promise.resolve({ connections });
      if (url === "/api/ssh/sessions") return Promise.resolve({ sessions });
      if (url === "/api/local/status") {
        return Promise.resolve(o.localStatus || { terminal: true, reason: "", background: true, running: 0 });
      }
      return Promise.reject(new Error("no stub for " + url));
    },
    postJson: (url, body) => {
      calls.push({ url, body });
      if (url === "/api/ssh/connect" || url === "/api/local/connect") {
        const res = (url === "/api/local/connect" ? o.localConnect : o.connect)
          || { ok: true, connected: true };
        if (res.ok && res.connected && !sessions.some((x) => x.name === body.name)) {
          sessions.push({ name: body.name, connected: true });
        }
        return Promise.resolve(res);
      }
      if (url === "/api/ssh/send") {
        return Promise.resolve({ ok: sessions.some((x) => x.name === body.name && x.connected) });
      }
      if (url === "/api/ssh/disconnect") {
        const drop = () => { sessions = sessions.filter((x) => x.name !== body.name); return { ok: true }; };
        if (!o.holdDisconnect) return Promise.resolve(drop());
        return new Promise((resolve) => { releaseDisconnect = () => resolve(drop()); });
      }
      if (url === "/api/ssh/connections") {
        connections.push({ name: body.name, host: body.host, port: body.port, username: body.username });
        return Promise.resolve({ ok: true, connections: connections.map((c) => c) });
      }
      if (url === "/api/local/run") {
        return Promise.resolve(o.localRun || { ok: true, pid: 4242, command: body.command });
      }
      if (url === "/api/ssh/run") {
        return Promise.resolve(o.run || { ok: true, stdout: "4711", stderr: "", command: body.command });
      }
      return Promise.reject(new Error("no stub for " + url));
    },
    getSettings: () => settings,
    saveSettings: (patch) => { settings = Object.assign({}, settings, patch); return Promise.resolve(settings); },
    terminal: (session, topts) => { terminals.push(session); termOpts.push(topts || null); return true; },
    console: (line, level) => { consoleLines.push({ line, level }); },
    notification: (level, message) => { notes.push({ level, message }); },
  });
  const all = (sel) => querySel(container.children, sel);
  return {
    container, calls, terminals, termOpts, notes, consoleLines,
    releaseDisconnect: () => releaseDisconnect(),
    savedNow: () => settings,
    urls: () => calls.map((c) => c.url),
    sends: () => calls.filter((c) => c.url === "/api/ssh/send").map((c) => c.body),
    shelf: () => all(".schw-launch"),
    tool: (label) => all(".icon-btn").find((b) => b.getAttribute("aria-label") === label),
    byLabel: (text) => all(".btn").filter((b) => b.children.some((c) => c.textContent === text)),
    fieldBy: (aria) => all(".field-input").find((i) => i.getAttribute("aria-label") === aria),
    select: () => all(".field-select")[0],
    segments: () => all(".schw-seg-opt"),
    where: () => all(".schw-where").map((c) => c.textContent),
    sshFields: () => all(".schw-ssh-fields")[0],
    newConnShowing: () => {
      const box = all(".schw-newconn")[0];
      return !!box && !box.hidden && querySel(box.children, ".field-input").length > 0;
    },
    terminalSwitch: () => all(".ui-toggle")[0],
    hints: () => all(".field-hint").map((c) => c.textContent),
    infos: () => all(".ui-info").map((btn) => (btn.corvusPopover ? btn.corvusPopover.el.children : [])
      .filter((c) => c.className.split(/\s+/).includes("ui-popover-text"))
      .map((c) => c.textContent).join("\n")),
    preview: () => all(".schw-preview")[0],
    status: () => all(".ui-msg")[0],
    failures: () => all(".schw-error").map((btn) => ({
      btn,
      text: (btn.corvusPopover ? btn.corvusPopover.el.children : [])
        .filter((c) => c.className.split(/\s+/).includes("ui-popover-text"))
        .map((c) => c.textContent).join("\n"),
    })),
  };
}

/** Click a segment of the Local / SSH control by its label. */
function pickTarget(h, label) {
  const seg = h.segments().find((b) => b.children.some((c) => c.textContent === label));
  assert.ok(seg, `a ${label} segment`);
  click(seg);
}

const LOCAL_BUTTON = {
  id: "l", label: "Ground logger", target: "local",
  directory: "~/logs", command: "./log.sh", mode: "terminal",
};
const LOCAL_BACKGROUND = {
  id: "k", label: "Tile server", target: "local",
  directory: "", command: "./serve.sh", mode: "background",
};
const SSH_BUTTON = {
  id: "s", label: "Start mission", target: "ssh", connection: "companion",
  directory: "/srv", command: "./run.sh", mode: "terminal",
};
const SSH_BACKGROUND = {
  id: "r", label: "Record", target: "ssh", connection: "ground",
  directory: "", command: "./record.sh", mode: "background",
};

// ---------------------------------------------------------------------------
// The pure parts
// ---------------------------------------------------------------------------

function testRegistered() {
  const found = Corvus.plugins.list().find((p) => p.id === "schwalby");
  assert.ok(found, "schwalby registered at load");
  assert.equal(found.name, "Schwalby");
  assert.equal(found.icon, "bird");
}

function testSessionsAreItsOwn() {
  assert.equal(S.sessionName({ id: "b1", label: "x" }), "schwalby/b1",
    "keyed by id, and never one of the SSH Launcher's");
}

function testTargetOfASavedButton() {
  assert.equal(S.coerceTarget({ target: "local" }), "local");
  assert.equal(S.coerceTarget({ target: "ssh" }), "ssh");
  // Copied over from the SSH Launcher: a connection means it ran over SSH.
  assert.equal(S.coerceTarget({ connection: "companion" }), "ssh");
  assert.equal(S.coerceTarget({}), "local");
  assert.equal(S.coerceTarget({ target: "nonsense" }), "local");
}

function testNormalize() {
  const list = S.normalizeButtons({
    buttons: [
      { id: "a", label: "Here", target: "local", connection: "companion", directory: "~/x", command: "./x" },
      { id: "b", connection: "ground", command: "./y", detach: true },
      { label: "no command" },
      "not an object",
    ],
  });
  assert.deepEqual(list, [
    { id: "a", label: "Here", target: "local", connection: "", directory: "~/x", command: "./x", mode: "terminal" },
    { id: "b", label: "./y", target: "ssh", connection: "ground", directory: "", command: "./y", mode: "background" },
  ], "a button here names no connection; the SSH Launcher's detach still means background");
  assert.deepEqual(S.normalizeButtons(undefined), []);
  const many = [];
  for (let i = 0; i < S.MAX_BUTTONS + 3; i++) many.push({ command: "c" + i });
  assert.equal(S.normalizeButtons({ buttons: many }).length, S.MAX_BUTTONS);
}

function testTerminalLineKeepsHomeOutsideTheQuotes() {
  const line = S.terminalLine;
  assert.equal(line({ directory: "/srv/my mission", command: "./run.sh --fast" }),
    "cd -- '/srv/my mission' && ./run.sh --fast");
  // ~ only means home outside the quotes; quoted it is a folder named "~".
  assert.equal(line({ directory: "~/my logs", command: "./log.sh" }), "cd -- ~/'my logs' && ./log.sh");
  assert.equal(line({ directory: "~", command: "ls" }), "cd -- ~ && ls");
  assert.equal(line({ directory: "~/", command: "ls" }), "cd -- ~/ && ls");
  assert.equal(line({ directory: "~other", command: "ls" }), "cd -- '~other' && ls");
  assert.equal(line({ directory: "/tmp'; rm -rf ~", command: "ls" }), "cd -- '/tmp'\\''; rm -rf ~' && ls",
    "an injection stays a folder name");
  assert.equal(line({ directory: "~/a'; rm -rf ~", command: "ls" }), "cd -- ~/'a'\\''; rm -rf ~' && ls");
  assert.equal(line({ command: "uptime" }), "uptime");
  assert.equal(line({ directory: "/srv", command: "  " }), "");
}

function testPreviewLine() {
  const preview = S.previewLine;
  assert.equal(preview({ target: "local", directory: "~/logs", command: "./log.sh", mode: "terminal" }),
    "cd ~/logs && ./log.sh", "no ssh in front of a program that runs here");
  assert.equal(preview({ target: "local", command: "./serve.sh", mode: "background" }), "./serve.sh &",
    "here a background program is Corvus' own child, not a nohup'd orphan");
  assert.equal(preview({ target: "ssh", connection: "companion", directory: "/srv", command: "./run.sh" }),
    "ssh companion 'cd /srv && ./run.sh'");
  assert.equal(preview({ target: "ssh", connection: "companion", command: "./x", mode: "background" }),
    "ssh companion 'nohup ./x &'");
  assert.equal(preview({ command: " " }), "");
}

function testSummaries() {
  assert.deepEqual(S.localSummary({ ok: true, pid: 4242 }),
    { text: "Started in the background (pid 4242). It stops when Corvus closes.", kind: "ok" });
  assert.deepEqual(S.localSummary({ ok: true, exited: true, code: 0, output: "done" }),
    { text: "Ran and finished.", kind: "ok" });
  assert.deepEqual(S.localSummary({ ok: false, exited: true, code: 127, output: "zsh: command not found: ./x\nmore" }),
    { text: "zsh: command not found: ./x", kind: "err" }, "in the program's own words");
  assert.deepEqual(S.localSummary({ ok: false, exited: true, code: 3, output: "" }),
    { text: "It ended at once with exit code 3.", kind: "err" });
  assert.deepEqual(S.localSummary({ ok: false, error: "There is no folder ~/nope on this computer." }),
    { text: "There is no folder ~/nope on this computer.", kind: "err" });
  assert.deepEqual(S.sshSummary({ ok: true, stdout: "4711\n" }),
    { text: "Started in the background (pid 4711).", kind: "ok" });
  assert.deepEqual(S.sshSummary({ ok: false, stderr: "sh: ./x: not found\nmore" }),
    { text: "sh: ./x: not found", kind: "err" });
}

function testNewConnectionHelpers() {
  assert.equal(S.derivedName({ host: " 10.0.0.7 ", username: "pilot" }), "pilot@10.0.0.7");
  assert.equal(S.newConnectionError({ host: "" }, []), "Name the host to connect to.");
  assert.match(S.newConnectionError({ host: "h", port: "70000" }, []), /between 1 and 65535/);
  assert.match(S.newConnectionError({ host: "h", username: "u" }, ["u@h"]), /already saved/);
  assert.deepEqual(S.newConnectionBody({ host: "h", port: "", username: "u", password: " pw " }),
    { name: "u@h", host: "h", port: 22, username: "u", password: " pw ", key_path: "" });
}

function testSegment() {
  const changes = [];
  const seg = S.segment({
    ariaLabel: "Where it runs",
    value: "local",
    options: [{ value: "local", label: "Local", icon: "laptop" }, { value: "ssh", label: "SSH", icon: "server" }],
    onChange: (v) => changes.push(v),
  });
  const [local, ssh] = seg.el.children;
  assert.equal(seg.el.getAttribute("role"), "radiogroup");
  assert.equal(seg.el.getAttribute("aria-label"), "Where it runs");
  assert.equal(local.getAttribute("role"), "radio");
  assert.equal(local.getAttribute("aria-checked"), "true");
  assert.equal(local.tabIndex, 0, "only the chosen segment is a Tab stop");
  assert.equal(ssh.tabIndex, -1);
  click(local);
  assert.deepEqual(changes, [], "the value already chosen is no change");
  click(ssh);
  assert.deepEqual(changes, ["ssh"]);
  assert.equal(ssh.getAttribute("aria-checked"), "true");
  assert.equal(local.getAttribute("aria-checked"), "false");
  let prevented = false;
  ssh._listeners.keydown[0]({ key: "ArrowRight", preventDefault: () => { prevented = true; } });
  assert.equal(seg.getValue(), "local", "the arrow keys move the choice, and wrap");
  assert.ok(prevented);
  local._listeners.keydown[0]({ key: "Tab", preventDefault() { throw new Error("Tab is not the segment's"); } });
  assert.equal(seg.getValue(), "local");
}

function testStylesheetIsItsOwn() {
  const fs = require("node:fs");
  const path = require("node:path");
  const dir = path.join(__dirname, "..", "plugins", "schwalby");
  const manifest = JSON.parse(fs.readFileSync(path.join(dir, "plugin.json"), "utf8"));
  assert.deepEqual(manifest.styles, ["schwalby.css"]);
  const css = fs.readFileSync(path.join(dir, "schwalby.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
  // Loaded into the shared document: every rule is about one of its own
  // classes, so it can never restyle the app.
  const selectors = css.replace(/@media[^{]*\{/g, "").split("}").map((r) => r.split("{")[0].trim()).filter(Boolean);
  selectors.forEach((sel) => sel.split(",").forEach((one) => {
    assert.match(one.trim(), /\.schw-/, `"${one.trim()}" styles something that is not Schwalby's`);
  }));
  const block = (sel) => {
    const m = css.match(new RegExp(sel.replace(/[.[\]]/g, "\\$&") + "\\s*\\{([^}]*)\\}"));
    return m ? m[1] : "";
  };
  assert.match(block(".schw-row-tools"), /align-items:\s*center/);
  assert.match(block(".ui-info.schw-error"), /width:\s*28px/);
  // And the script uses only classes its stylesheet (or the app) defines.
  const js = fs.readFileSync(path.join(dir, "schwalby.js"), "utf8");
  for (const m of js.matchAll(/"(schw-[a-z-]+)"/g)) {
    assert.ok(css.includes("." + m[1]), `${m[1]} is used but not styled`);
  }
}

// ---------------------------------------------------------------------------
// The shelf
// ---------------------------------------------------------------------------

async function testLocalTerminalButtonOpensAShellHere() {
  const h = mount({ saved: { buttons: [LOCAL_BUTTON] } });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();

  assert.deepEqual(h.calls.find((c) => c.url === "/api/local/connect").body, { name: "schwalby/l" },
    "a shell on this computer, under the button's own session");
  assert.ok(!h.urls().includes("/api/ssh/connect"), "and no SSH connection at all");
  assert.deepEqual(h.sends(), [
    { name: "schwalby/l", data: "cd -- ~/'logs' && ./log.sh\n" },
    { name: "schwalby/l", data: "cd -- ~/'logs' && ./log.sh\n" },
  ], "tried the live session first, then typed it into the new shell");
  assert.ok(h.termOpts.every((o) => o && o.existingOnly), "a launch opens no window");
  assert.deepEqual(h.consoleLines[0], { line: "schwalby (this computer): cd -- ~/'logs' && ./log.sh", level: "success" });

  // Pressed again: the same shell, no second connect.
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  assert.equal(h.urls().filter((u) => u === "/api/local/connect").length, 1);

  // The arrow opens its window, named for this computer.
  click(h.tool("Open the terminal for Ground logger"));
  assert.deepEqual(h.terminals[h.terminals.length - 1],
    { name: "schwalby/l", title: "Ground logger", host: "this computer" });
  assert.equal(h.termOpts[h.termOpts.length - 1].existingOnly, false);
  S.destroy(h.container);
}

async function testLocalBackgroundButtonRunsHere() {
  const h = mount({ saved: { buttons: [LOCAL_BACKGROUND] } });
  await flushMicrotasks();
  assert.equal(h.tool("Open the terminal for Tile server"), undefined, "nothing to watch, so no arrow");
  click(h.shelf()[0]);
  await flushMicrotasks();
  assert.deepEqual(h.calls.find((c) => c.url === "/api/local/run").body, { directory: "", command: "./serve.sh" });
  assert.ok(!h.urls().includes("/api/ssh/run"));
  assert.match(h.status().textContent || "", /pid 4242\)\. It stops when Corvus closes\./);
  S.destroy(h.container);
}

async function testALocalFailureLandsOnItsRow() {
  const h = mount({
    saved: { buttons: [LOCAL_BACKGROUND] },
    localRun: { ok: false, exited: true, code: 127, output: "zsh: command not found: ./serve.sh", command: "./serve.sh" },
  });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  const [failure] = h.failures();
  assert.ok(failure, "a warning on the row that failed");
  assert.equal(failure.text, "zsh: command not found: ./serve.sh");
  assert.ok(h.notes.some((n) => n.level === "warning" && /Tile server: zsh: command not found/.test(n.message)));
  S.destroy(h.container);
}

async function testARefusedLocalShellSaysWhy() {
  const h = mount({
    saved: { buttons: [LOCAL_BUTTON] },
    localConnect: { ok: false, connected: false, error: "There is no folder ~/logs on this computer." },
  });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  assert.equal(h.failures()[0].text, "There is no folder ~/logs on this computer.");
  S.destroy(h.container);
}

async function testSshButtonsRunOverSsh() {
  const h = mount({ saved: { buttons: [SSH_BUTTON, SSH_BACKGROUND] } });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  assert.deepEqual(h.calls.find((c) => c.url === "/api/ssh/connect").body, { name: "schwalby/s", from: "companion" });
  assert.ok(!h.urls().includes("/api/local/connect"));
  click(h.tool("Open the terminal for Start mission"));
  assert.deepEqual(h.terminals[h.terminals.length - 1],
    { name: "schwalby/s", title: "Start mission", host: "10.0.0.7", port: 22, username: "pilot" });

  click(h.shelf()[1]);
  await flushMicrotasks();
  assert.deepEqual(h.calls.find((c) => c.url === "/api/ssh/run").body,
    { name: "ground", directory: "", command: "./record.sh", detach: true });
  assert.ok(!h.urls().includes("/api/local/run"));
  S.destroy(h.container);
}

async function testRowsSayWhereTheyRun() {
  const h = mount({ saved: { buttons: [LOCAL_BUTTON, SSH_BUTTON] } });
  await flushMicrotasks();
  assert.deepEqual(h.where(), ["Local", "SSH"]);
  S.destroy(h.container);
}

async function testEditorSwitchesBetweenLocalAndSsh() {
  const h = mount({ saved: { buttons: [] } });
  await flushMicrotasks();
  click(h.byLabel("Add button")[0]);

  // An empty shelf starts a button here: Local is the first segment.
  const [localSeg, sshSeg] = h.segments();
  assert.equal(localSeg.getAttribute("aria-checked"), "true");
  assert.equal(sshSeg.getAttribute("aria-checked"), "false");
  assert.equal(h.sshFields().hidden, true, "no connection to pick for this computer");
  typeInto(h.fieldBy("Folder"), "~/tools");
  typeInto(h.fieldBy("Command to start"), "./go.sh");
  assert.equal(h.preview().textContent, "cd ~/tools && ./go.sh");
  assert.equal(h.byLabel("Save")[0].disabled, false, "this computer needs no connection");
  assert.ok(h.infos().some((t) => /stopped when Corvus closes/.test(t)));

  // SSH: the connection is back, and the preview goes through it.
  pickTarget(h, "SSH");
  assert.equal(h.sshFields().hidden, false);
  assert.equal(h.preview().textContent, "ssh companion 'cd ~/tools && ./go.sh'");
  assert.ok(h.infos().some((t) => /survives Corvus closing/.test(t)));

  // Back to Local and saved: no connection is carried along.
  pickTarget(h, "Local");
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();
  let saved = h.savedNow().buttons;
  assert.equal(saved[0].target, "local");
  assert.equal(saved[0].connection, "");
  assert.deepEqual(h.where(), ["Local"]);

  // The next button starts where the last one runs.
  click(h.byLabel("Add button")[0]);
  assert.equal(h.segments()[0].getAttribute("aria-checked"), "true");
  pickTarget(h, "SSH");
  changeTo(h.select(), "ground");
  typeInto(h.fieldBy("Command to start"), "./fly.sh");
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();
  saved = h.savedNow().buttons;
  assert.equal(saved[1].target, "ssh");
  assert.equal(saved[1].connection, "ground");
  assert.deepEqual(h.where(), ["Local", "SSH"]);
  S.destroy(h.container);
}

async function testCancelDiscardsTheDraft() {
  const h = mount({ saved: { buttons: [LOCAL_BUTTON] } });
  await flushMicrotasks();
  click(h.tool("Edit Ground logger"));
  pickTarget(h, "SSH");
  click(h.byLabel("Cancel")[0]);
  assert.deepEqual(h.where(), ["Local"]);
  assert.equal(h.savedNow().buttons[0].target, "local");
  S.destroy(h.container);
}

async function testTypingANewConnectionIsGoneWhenLocal() {
  const h = mount({ saved: { buttons: [] }, connections: [] });
  await flushMicrotasks();
  click(h.byLabel("Add button")[0]);
  assert.equal(h.newConnShowing(), false, "a button here asks for no connection");
  pickTarget(h, "SSH");
  assert.equal(h.newConnShowing(), true, "with none saved, SSH opens on the form");
  typeInto(h.fieldBy("Password"), "hunter2");
  pickTarget(h, "Local");
  assert.equal(h.newConnShowing(), false);
  assert.equal(h.fieldBy("Password"), undefined, "the password field is gone, not hidden");
  S.destroy(h.container);
}

async function testSavesATypedInConnectionBeforeTheButton() {
  const h = mount({ saved: { buttons: [] }, connections: [] });
  await flushMicrotasks();
  click(h.byLabel("Add button")[0]);
  pickTarget(h, "SSH");
  typeInto(h.fieldBy("Host"), "10.0.0.9");
  typeInto(h.fieldBy("User"), "pilot");
  typeInto(h.fieldBy("Password"), "hunter2");
  typeInto(h.fieldBy("Command to start"), "./fly.sh");
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();
  const order = h.urls().filter((u) => u === "/api/ssh/connections");
  assert.equal(order.length, 2, "the list was read, then the new connection saved");
  const saved = h.savedNow().buttons[0];
  assert.equal(saved.connection, "pilot@10.0.0.9");
  assert.ok(!JSON.stringify(h.savedNow()).includes("hunter2"), "the password never lands in the plugin's settings");
  S.destroy(h.container);
}

async function testNoTerminalWhereThereIsNone() {
  const h = mount({
    saved: { buttons: [LOCAL_BUTTON] },
    localStatus: { terminal: false, reason: "Windows offers no terminal here. Run it in the background.", background: true },
  });
  await flushMicrotasks();
  click(h.tool("Edit Ground logger"));
  const sw = h.terminalSwitch();
  assert.equal(sw.disabled, true, "no switch to a terminal that cannot be had");
  assert.equal(sw.getAttribute("aria-checked"), "false");
  assert.ok(h.hints().includes("Windows offers no terminal here. Run it in the background."), "and it says why");
  assert.match(h.preview().textContent, /&$/, "the preview is the background run it will be");

  // Over SSH the terminal is on the far end, so the operator's choice is back.
  pickTarget(h, "SSH");
  assert.equal(h.terminalSwitch().disabled, false);
  assert.equal(h.terminalSwitch().getAttribute("aria-checked"), "true");

  pickTarget(h, "Local");
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();
  assert.equal(h.savedNow().buttons[0].mode, "background");
  S.destroy(h.container);
}

async function testALateStatusRepaintsTheOpenEditor() {
  let answer = null;
  const h = mount({
    saved: { buttons: [LOCAL_BUTTON] },
    localStatus: new Promise((resolve) => { answer = resolve; }),
  });
  await flushMicrotasks();
  click(h.tool("Edit Ground logger"));
  assert.equal(h.terminalSwitch().disabled, false, "unanswered, the operator's choice stands");
  answer({ terminal: false, reason: "No terminal here.", background: true });
  await flushMicrotasks();
  assert.equal(h.terminalSwitch().disabled, true, "the editor that is open takes the answer");
  assert.ok(h.hints().includes("No terminal here."));
  S.destroy(h.container);
}

async function testMovingARunningButtonClosesItsSessionFirst() {
  const h = mount({ saved: { buttons: [SSH_BUTTON] }, sessions: ["schwalby/s"], holdDisconnect: true });
  await flushMicrotasks();

  // Declined: nothing is saved, the editor stays, the program keeps running.
  window.confirm = () => false;
  click(h.tool("Edit Start mission"));
  pickTarget(h, "Local");
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();
  assert.equal(h.byLabel("Save").length, 1, "still editing");
  assert.equal(h.savedNow().buttons[0].target, "ssh");
  assert.ok(!h.urls().includes("/api/ssh/disconnect"));

  // Confirmed: the old session goes, because its next line would still have
  // landed on the companion computer.
  let asked = "";
  window.confirm = (text) => { asked = text; return true; };
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();
  assert.match(asked, /still running in its terminal/);
  assert.deepEqual(h.calls.filter((c) => c.url === "/api/ssh/disconnect").map((c) => c.body), [{ name: "schwalby/s" }]);
  assert.equal(h.savedNow().buttons[0].target, "local");

  // Pressed before the backend has closed it: the line waits, then goes to a
  // new shell here, never to the old one there.
  click(h.shelf()[0]);
  await flushMicrotasks();
  assert.deepEqual(h.sends(), [], "nothing is sent while the old session is closing");
  h.releaseDisconnect();
  await flushMicrotasks();
  await flushMicrotasks();
  assert.deepEqual(h.urls().filter((u) => /disconnect|local\/connect|ssh\/send/.test(u)),
    ["/api/ssh/disconnect", "/api/ssh/send", "/api/local/connect", "/api/ssh/send"]);
  window.confirm = () => true;
  S.destroy(h.container);
}

async function testARenameKeepsTheSession() {
  const h = mount({ saved: { buttons: [SSH_BUTTON] }, sessions: ["schwalby/s"] });
  await flushMicrotasks();
  let asked = 0;
  window.confirm = () => { asked += 1; return true; };
  click(h.tool("Edit Start mission"));
  typeInto(h.fieldBy("Button label"), "Mission A");
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();
  assert.equal(asked, 0);
  assert.ok(!h.urls().includes("/api/ssh/disconnect"));

  // Another connection is a move: asked, then closed.
  click(h.tool("Edit Mission A"));
  changeTo(h.select(), "ground");
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();
  assert.equal(asked, 1);
  assert.deepEqual(h.calls.find((c) => c.url === "/api/ssh/disconnect").body, { name: "schwalby/s" });
  window.confirm = () => true;
  S.destroy(h.container);
}

async function testRemovingARunningButtonClosesItsSession() {
  const h = mount({ saved: { buttons: [LOCAL_BUTTON] }, sessions: ["schwalby/l"] });
  await flushMicrotasks();
  click(h.tool("Edit Ground logger"));
  click(h.byLabel("Delete")[0]);
  await flushMicrotasks();
  assert.equal(h.shelf().length, 0);
  assert.deepEqual(h.calls.find((c) => c.url === "/api/ssh/disconnect").body, { name: "schwalby/l" });
  S.destroy(h.container);
}

async function testDestroyStopsLateCallbacks() {
  const h = mount({ saved: { buttons: [LOCAL_BACKGROUND] } });
  S.destroy(h.container);
  await flushMicrotasks();
  assert.equal(h.container._schwDestroy, null);
  assert.equal(h.shelf().length, 1, "nothing was redrawn after destroy");
}

async function run() {
  testRegistered();
  testSessionsAreItsOwn();
  testTargetOfASavedButton();
  testNormalize();
  testTerminalLineKeepsHomeOutsideTheQuotes();
  testPreviewLine();
  testSummaries();
  testNewConnectionHelpers();
  testSegment();
  testStylesheetIsItsOwn();
  await testLocalTerminalButtonOpensAShellHere();
  await testLocalBackgroundButtonRunsHere();
  await testALocalFailureLandsOnItsRow();
  await testARefusedLocalShellSaysWhy();
  await testSshButtonsRunOverSsh();
  await testRowsSayWhereTheyRun();
  await testEditorSwitchesBetweenLocalAndSsh();
  await testCancelDiscardsTheDraft();
  await testTypingANewConnectionIsGoneWhenLocal();
  await testSavesATypedInConnectionBeforeTheButton();
  await testNoTerminalWhereThereIsNone();
  await testALateStatusRepaintsTheOpenEditor();
  await testMovingARunningButtonClosesItsSessionFirst();
  await testARenameKeepsTheSession();
  await testRemovingARunningButtonClosesItsSession();
  await testDestroyStopsLateCallbacks();
  dismissToasts();
  await flushMicrotasks();
  console.log("frontend schwalby tests passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
}).finally(() => {
  while (mounted.length) {
    try { S.destroy(mounted.pop()); } catch (_e) { /* already gone */ }
  }
  dismissToasts();
});
