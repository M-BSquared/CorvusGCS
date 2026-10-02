"use strict";

/**
 * Frontend tests for the SSH Launcher plugin (plugins/ssh-launcher): the
 * command line it builds, the saved shelf of buttons, launching in a
 * terminal or in the background, connections, and a clean teardown.
 *
 * Run:
 *   node plugins/ssh-launcher/tests/test_ssh_launcher.js
 */
const assert = require("node:assert/strict");
const path = require("node:path");

// The Corvus checkout whose src/ this plugin runs against: the one around
// plugins/ssh-launcher/, unless CORVUS_ROOT names another (the plugin kept in its own
// repository, say). tools/frontend_tests.js sets it.
const CORVUS = process.env.CORVUS_ROOT
  ? path.resolve(process.env.CORVUS_ROOT)
  : path.join(__dirname, "..", "..", "..");

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

// Controlled clock for the throttle test.
let clock = 1000;
Date.now = () => clock;

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
// Load order mirrors the browser's: plugins.js (a <script> tag in index.html)
// first, then the folder plugins, which register themselves at load.
// ---------------------------------------------------------------------------
// ui.js first: it defines Corvus.ui, the component layer every other
// module builds its DOM with (index.html loads it in the same order).
require(path.join(CORVUS, "src", "js", "ui.js"));
require(path.join(CORVUS, "src", "js", "plugins.js"));
// A folder plugin: in the browser its script is appended by loadInstalled
// rather than by a tag in index.html. Requiring it here runs the same file
// the same way, and it registers itself as it loads.
require("../ssh-launcher.js");

/** Run *fn* with console.error muted — two tests below exercise the loader's
 *  failure paths on purpose, and their logging is the expected behaviour, not
 *  output worth printing on a passing run. */
async function quietly(fn) {
  const real = console.error;
  console.error = () => {};
  try { return await fn(); }
  finally { console.error = real; }
}

/** Every toast currently on screen, as {level, title, message}. */
function toasts() {
  return querySel(body.children, ".ui-toast").map((el) => ({
    level: el.dataset.level,
    title: (querySel(el.children, ".ui-toast-title")[0] || {}).textContent,
    message: (querySel(el.children, ".ui-toast-body")[0] || {}).textContent,
  }));
}

/** Dismiss them all, which also clears the timers they would close on — a
 *  pending 6-second timer per warning would keep Node alive long after the
 *  last assertion. */
function dismissToasts() {
  querySel(body.children, ".ui-toast-close").forEach((btn) => click(btn));
}

// ---------------------------------------------------------------------------
// The SSH Launcher
// ---------------------------------------------------------------------------

function testSshLauncherRegistered() {
  const found = Corvus.plugins.list().find((p) => p.id === "ssh-launcher");
  assert.ok(found, "ssh-launcher registered at load");
  assert.equal(found.name, "SSH Launcher");
  assert.equal(found.icon, "rocket");
}

function testSshLauncherPreviewLine() {
  const preview = Corvus.pluginSshLauncher.previewLine;
  assert.equal(preview({ connection: "companion", directory: "/srv", command: "./run.sh", mode: "background" }),
    "ssh companion 'cd /srv && nohup ./run.sh &'");
  assert.equal(preview({ connection: "companion", directory: "/srv", command: "./run.sh", mode: "terminal" }),
    "ssh companion 'cd /srv && ./run.sh'");
  // Background with no folder still shows the nohup — the earlier version
  // patched the composed string and silently dropped it here.
  assert.equal(preview({ connection: "companion", command: "./run.sh", mode: "background" }),
    "ssh companion 'nohup ./run.sh &'");
  assert.equal(preview({ connection: "companion", command: "uptime" }),
    "ssh companion 'uptime'");
  // Nothing to run yet — the button stays disabled and the box shows a hint.
  assert.equal(preview({ connection: "companion", directory: "/srv", command: "  " }), "");
  assert.equal(preview({}), "");
}

function testSshLauncherRemoteLineQuotesTheFolderOnly() {
  const line = Corvus.pluginSshLauncher.remoteLine;
  assert.equal(line({ directory: "/srv/my mission", command: "./run.sh --fast" }),
    "cd -- '/srv/my mission' && ./run.sh --fast");
  // The injection lands inside the quotes: a folder name, not a second command.
  assert.equal(line({ directory: "/tmp'; rm -rf ~", command: "./run.sh" }),
    "cd -- '/tmp'\\''; rm -rf ~' && ./run.sh");
  assert.equal(line({ command: "uptime" }), "uptime");
  assert.equal(line({ directory: "/srv", command: "  " }), "");
}

function testSshLauncherResultSummary() {
  const summary = Corvus.pluginSshLauncher.resultSummary;
  assert.deepEqual(summary({ ok: true, stdout: "4711\n" }),
    { text: "Started in the background (pid 4711).", kind: "ok" });
  assert.deepEqual(summary({ ok: true, stdout: "" }),
    { text: "Started in the background.", kind: "ok" });
  // A failure shows the first line of stderr — the operator's actual diagnosis.
  assert.deepEqual(
    summary({ ok: false, stderr: "sh: ./run.sh: not found\nmore", error: "exit 127" }),
    { text: "sh: ./run.sh: not found", kind: "err" });
  assert.deepEqual(summary({ ok: false, error: "connection refused" }),
    { text: "connection refused", kind: "err" });
  assert.deepEqual(summary({ ok: false }), { text: "The command failed.", kind: "err" });
}

function testSshLauncherModeMigratesFromDetach() {
  const mode = Corvus.pluginSshLauncher.coerceMode;
  // The boolean this plugin saved before it grew a terminal.
  assert.equal(mode({ detach: true }), "background");
  assert.equal(mode({ detach: false }), "terminal");
  // An explicit mode wins, and anything unrecognised lands on the default.
  assert.equal(mode({ mode: "background", detach: false }), "background");
  assert.equal(mode({ mode: "nonsense" }), "terminal");
  assert.equal(mode({}), "terminal");
}

function testSshLauncherSessionIsKeyedByIdNotLabel() {
  const session = Corvus.pluginSshLauncher.sessionName;
  assert.equal(session({ id: "b123", label: "Start mission" }), "ssh-launcher/b123");
  // Renaming a button must not orphan the session its program runs in.
  assert.equal(session({ id: "b123", label: "Renamed" }), "ssh-launcher/b123");
}

/** Every launcher mounted by a test, so a failing assertion cannot leave one
 *  running: the plugin polls on an interval, and a leaked interval keeps Node
 *  alive long past the failure that caused it. */
const mountedLaunchers = [];
function destroyLaunchers() {
  while (mountedLaunchers.length) {
    try { Corvus.pluginSshLauncher.destroy(mountedLaunchers.pop()); } catch (_e) {}
  }
}

/**
 * Mount the launcher against a stub api and return everything a test needs.
 * `opts` is {saved, run, connect, sessions}: the settings it reads, the
 * /api/ssh/run body, the /api/ssh/connect body, and which sessions are live.
 * `connectOnce` answers the FIRST connect only (a body, or an Error to reject
 * with); `sshSetup` is the api.sshSetup stub, its calls kept in `setups`.
 */
function mountLauncher(opts) {
  const o = opts || {};
  const calls = [];
  // Mutable: the launcher can now create one, and the endpoint answers with
  // the list the new entry is in.
  const connections = o.connections || [
    { name: "companion", host: "10.0.0.7", username: "pilot", port: 22 },
    { name: "ground", host: "10.0.0.2", username: "ops", port: 22 },
  ];
  let sessions = (o.sessions || []).map((name) => ({ name, connected: true }));
  let settings = o.saved || {};
  const terminals = [];
  const termOpts = [];
  const notes = [];
  const setups = [];
  let connectOnce = o.connectOnce;
  const container = makeEl("div");
  mountedLaunchers.push(container);
  const extra = o.sshSetup ? {
    sshSetup: (reply) => { setups.push(reply); return Promise.resolve(o.sshSetup(reply)); },
  } : {};
  Corvus.pluginSshLauncher.init(container, Object.assign(extra, {
    requestJson: (url) => {
      calls.push({ url });
      if (url === "/api/ssh/connections") return Promise.resolve({ connections });
      if (url === "/api/ssh/sessions") return Promise.resolve({ sessions });
      return Promise.reject(new Error("no stub for " + url));
    },
    postJson: (url, body) => {
      calls.push({ url, body });
      // The backend modelled closely enough to matter: a connect makes the
      // session live, and a send only succeeds while it IS live — which is
      // what tells the launcher whether it has to open a shell at all.
      if (url === "/api/ssh/connect" && connectOnce) {
        const first = connectOnce;
        connectOnce = null;
        return first instanceof Error ? Promise.reject(first) : Promise.resolve(first);
      }
      if (url === "/api/ssh/connect") {
        const res = o.connect || { ok: true, connected: true };
        if (res.ok && res.connected && !sessions.some((x) => x.name === body.name)) {
          sessions.push({ name: body.name, connected: true });
        }
        return Promise.resolve(res);
      }
      if (url === "/api/ssh/send") {
        return Promise.resolve({
          ok: sessions.some((x) => x.name === body.name && x.connected),
        });
      }
      if (url === "/api/ssh/disconnect") {
        sessions = sessions.filter((x) => x.name !== body.name);
        return Promise.resolve({ ok: true });
      }
      if (url === "/api/ssh/connections") {
        if (o.upsert && o.upsert.ok === false) return Promise.resolve(o.upsert);
        // The real endpoint answers with the redacted list, the new entry in it.
        connections.push({
          name: body.name, host: body.host, port: body.port, username: body.username,
        });
        return Promise.resolve({ ok: true, connections: connections.map((c) => c) });
      }
      return Promise.resolve(o.run || { ok: true, stdout: "4711", stderr: "", command: "x" });
    },
    getSettings: () => settings,
    saveSettings: (patch) => { settings = Object.assign({}, settings, patch); return Promise.resolve(settings); },
    terminal: (session, opts) => { terminals.push(session); termOpts.push(opts || null); return true; },
    console: () => {},
    notification: (level, message) => { notes.push({ level, message }); },
  }));
  return {
    container, calls, connections, terminals, termOpts, notes, setups,
    notesShown: () => querySel(container.children, ".sshl-note").map((n) => n.textContent),
    savedNow: () => settings,
    setSessions: (names) => { sessions = names.map((name) => ({ name, connected: true })); },
    /** Every /api/ssh/send body, in order. */
    sends: () => calls.filter((c) => c.url === "/api/ssh/send").map((c) => c.body),
    connects: () => calls.filter((c) => c.url === "/api/ssh/connect").map((c) => c.body),
    /** The launch buttons currently on the shelf, in order. */
    shelf: () => querySel(container.children, ".sshl-launch"),
    /** A row's tool button, found by what it announces rather than by index —
     *  the tools differ per row (a terminal button has an arrow, a background
     *  one does not) and positions would make these tests lie. */
    tool: (label) => querySel(container.children, ".icon-btn")
      .find((b) => b.getAttribute("aria-label") === label),
    /** Every .btn with the given visible label. */
    byLabel: (text) => querySel(container.children, ".btn")
      .filter((b) => b.children.some((c) => c.textContent === text)),
    fields: () => querySel(container.children, ".field-input"),
    /** The inline new-connection form's inputs, in the order they are asked. */
    newConnFields: () => {
      const box = querySel(container.children, ".sshl-newconn")[0];
      return box ? querySel(box.children, ".field-input") : [];
    },
    /** Whether the form is showing: the container is always in the card, but
     *  its fields exist only while a connection is being typed in. */
    newConnShowing: () => {
      const box = querySel(container.children, ".sshl-newconn")[0];
      return !!box && !box.hidden && querySel(box.children, ".field-input").length > 0;
    },
    /** One input by what it announces — robust against fields appearing
     *  between it and the top of the form. */
    fieldBy: (aria) => querySel(container.children, ".field-input")
      .find((i) => i.getAttribute("aria-label") === aria),
    select: () => querySel(container.children, ".field-select")[0],
    preview: () => querySel(container.children, ".sshl-preview")[0],
    status: () => querySel(container.children, ".ui-msg")[0],
    dots: () => querySel(container.children, ".sshl-dot"),
    /** The warning triangles on the shelf, each with what its popover says —
     *  a failure now belongs to the row that failed, not to a bar under the
     *  whole shelf. */
    failures: () => querySel(container.children, ".sshl-error").map((btn) => ({
      btn,
      ariaLabel: btn.getAttribute("aria-label"),
      text: (btn.corvusPopover ? btn.corvusPopover.el.children : [])
        .filter((c) => c.className.split(/\s+/).includes("ui-popover-text"))
        .map((c) => c.textContent).join("\n"),
    })),
  };
}

/** Fire an element's first click listener. */
function click(el) { el._listeners.click[0](); }

/** Pick an option in a <select> built by Corvus.ui.select. */
function changeTo(el, value) {
  el.value = value;
  (el._listeners.change || []).forEach((cb) => cb());
}

/** Type into an input built by Corvus.ui.input (fires its "input" listener). */
function typeInto(el, value) {
  el.value = value;
  (el._listeners.input || []).forEach((cb) => cb());
}

const TERMINAL_BUTTON = {
  id: "a", label: "Start mission", connection: "companion",
  directory: "/srv", command: "./run.sh", mode: "terminal",
};
const BACKGROUND_BUTTON = {
  id: "b", label: "Record logs", connection: "ground",
  directory: "", command: "./record.sh", mode: "background",
};

function testSshLauncherNormalizesASavedShelf() {
  const normalize = Corvus.pluginSshLauncher.normalizeButtons;
  const list = normalize({
    buttons: [
      { id: "a", label: "Mission", connection: "companion", directory: "/srv", command: "./run.sh", mode: "background" },
      { command: "uptime" },
      { label: "no command" },          // dropped: nothing to run
      "not an object",                   // dropped
    ],
  });
  assert.equal(list.length, 2);
  assert.deepEqual(list[0], {
    id: "a", label: "Mission", connection: "companion",
    directory: "/srv", command: "./run.sh", mode: "background", restart: false,
  });
  // An unnamed button falls back to its command, and defaults to a terminal.
  assert.equal(list[1].label, "uptime");
  assert.equal(list[1].mode, "terminal");
  assert.ok(list[1].id, "a saved entry with no id gets one");
}

function testSshLauncherMigratesTheOldSingleCommandShape() {
  // The shape this plugin saved before it grew a shelf. An operator who
  // configured it then must find their command as the first button.
  const list = Corvus.pluginSshLauncher.normalizeButtons({
    connection: "companion", directory: "/srv", command: "./start.sh", detach: true,
  });
  assert.equal(list.length, 1);
  assert.equal(list[0].command, "./start.sh");
  assert.equal(list[0].connection, "companion");
  assert.equal(list[0].label, "./start.sh");
  // detach:true was "start it with nohup and let it run" — that is background.
  assert.equal(list[0].mode, "background");
}

function testSshLauncherNormalizeIsEmptyForNothingSaved() {
  const normalize = Corvus.pluginSshLauncher.normalizeButtons;
  assert.deepEqual(normalize(undefined), []);
  assert.deepEqual(normalize({}), []);
  assert.deepEqual(normalize({ buttons: [] }), []);
}

function testSshLauncherCapsTheShelf() {
  const many = [];
  for (let i = 0; i < Corvus.pluginSshLauncher.MAX_BUTTONS + 5; i++) many.push({ command: "c" + i });
  const list = Corvus.pluginSshLauncher.normalizeButtons({ buttons: many });
  assert.equal(list.length, Corvus.pluginSshLauncher.MAX_BUTTONS);
}

async function testSshLauncherRendersOneButtonPerSavedEntry() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON, BACKGROUND_BUTTON] } });
  await flushMicrotasks();
  const shelf = h.shelf();
  assert.equal(shelf.length, 2, "one launch button per saved entry");
  assert.equal(shelf[0].getAttribute("aria-label"), "Launch Start mission");
  assert.equal(shelf[1].getAttribute("aria-label"), "Launch Record logs");
  // The composed line is the tooltip: the label rarely says what actually runs.
  assert.equal(shelf[0].title, "ssh companion 'cd /srv && ./run.sh'");
  assert.equal(shelf[1].title, "ssh ground 'nohup ./record.sh &'");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherTerminalButtonOpensASessionAndTypesTheLine() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON] } });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  await flushMicrotasks();

  const connect = h.calls.find((c) => c.url === "/api/ssh/connect");
  assert.deepEqual(connect.body, { name: "ssh-launcher/a", from: "companion" },
    "with nothing running, the button opens the session it needs");
  assert.deepEqual(h.sends()[h.sends().length - 1],
    { name: "ssh-launcher/a", data: "cd -- '/srv' && ./run.sh\n" },
    "and types the line into it");
  assert.ok(!h.calls.some((c) => c.url === "/api/ssh/run"),
    "a terminal button never goes through the one-shot run endpoint");
  Corvus.pluginSshLauncher.destroy(h.container);
}

/* The heart of it: a key on a shelf pressed twice means "run it again". It
   runs again in the SAME shell — one run under the last, one scrollback — and
   the session is never thrown away and rebuilt, which used to kill whatever
   was still running and hand back a blank screen. */
async function testSshLauncherPressingItAgainRunsInTheSameSession() {
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON] },
    sessions: ["ssh-launcher/a"],
  });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();

  assert.deepEqual(h.connects(), [],
    "a live session is never replaced — that would stop what is running");
  assert.deepEqual(h.sends(), [{ name: "ssh-launcher/a", data: "cd -- '/srv' && ./run.sh\n" }],
    "the line simply goes to the shell that is already there");

  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  assert.equal(h.sends().length, 2, "and again on the next press");
  assert.deepEqual(h.connects(), []);
  assert.match(h.status().textContent, /sent again/,
    "the message says it went to the terminal that was already open");
  Corvus.pluginSshLauncher.destroy(h.container);
}

function testSshLauncherRestartsOnlyAProgramInATerminal() {
  const L = Corvus.pluginSshLauncher;
  assert.equal(L.restarts({ restart: true, mode: "terminal" }), true);
  assert.equal(L.restarts({ restart: true, mode: "background" }), false,
    "a background run has no terminal to press Ctrl-C into");
  assert.equal(L.restarts({ mode: "terminal" }), false, "off unless asked for");
  assert.equal(L.coerceButton({ command: "x", restart: "yes" }).restart, false,
    "only a real true turns it on");
  assert.equal(L.coerceButton({ command: "x", restart: true }).restart, true);
}

/* Restart on: a press on a running program sends Ctrl-C into its shell,
   waits for the prompt, and only then types the line again. */
async function testSshLauncherRestartSendsCtrlCFirst() {
  const L = Corvus.pluginSshLauncher;
  const line = { name: "ssh-launcher/a", data: "cd -- '/srv' && ./run.sh\n" };
  const ctrlC = { name: "ssh-launcher/a", data: "\x03" };
  const h = mountLauncher({ saved: { buttons: [Object.assign({}, TERMINAL_BUTTON, { restart: true })] } });
  await flushMicrotasks();
  assert.match(h.shelf()[0].getAttribute("aria-label"), /^Launch /);

  // Nothing running yet: Ctrl-C finds no shell, so it is an ordinary first press.
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  await flushMicrotasks();
  assert.deepEqual(h.sends(), [ctrlC, line]);
  assert.equal(h.connects().length, 1);
  assert.equal(h.shelf()[0].getAttribute("aria-label"), "Restart Start mission");
  assert.match(h.shelf()[0].title, /Ctrl-C/);

  click(h.shelf()[0]);
  await flushMicrotasks();
  assert.deepEqual(h.sends().slice(2), [ctrlC],
    "the line waits until the shell has had time to take the prompt back");
  await new Promise((r) => setTimeout(r, L.RESTART_GRACE_MS + 50));
  await flushMicrotasks();
  assert.deepEqual(h.sends().slice(2), [ctrlC, line], "then it is typed into the same shell");
  assert.equal(h.connects().length, 1, "no second session");
  assert.match(h.status().textContent, /stopped with Ctrl-C and started again/);
  L.destroy(h.container);
}

async function testSshLauncherRestartSwitchIsSavedAndOnlyForATerminal() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON] } });
  await flushMicrotasks();
  click(h.tool("Edit Start mission"));
  const switches = querySel(h.container.children, ".field-switch");
  const toggles = querySel(h.container.children, ".ui-toggle");
  assert.equal(toggles[1].getAttribute("aria-label"), "Restart on press");
  assert.equal(toggles[1].getAttribute("aria-checked"), "false");
  assert.equal(switches[1].hidden, false);

  click(toggles[0]);                       // run in the background
  assert.equal(switches[1].hidden, true, "no restart without a terminal");
  click(toggles[0]);                       // back to a terminal
  assert.equal(switches[1].hidden, false);

  click(toggles[1]);
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();
  assert.equal(h.savedNow().buttons[0].restart, true);
  Corvus.pluginSshLauncher.destroy(h.container);
}

/* A shell that has since ended leaves the button pressable: the send comes
   back ok:false, and only then is a new session opened. */
async function testSshLauncherReopensASessionThatHasEnded() {
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON] },
    sessions: ["ssh-launcher/a"],
  });
  await flushMicrotasks();
  h.setSessions([]);                      // the remote shell exited
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  await flushMicrotasks();

  assert.deepEqual(h.connects(), [{ name: "ssh-launcher/a", from: "companion" }],
    "the session is opened again");
  assert.equal(h.sends().length, 2, "the probe that failed, then the real one");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherTerminalButtonReportsAFailedConnect() {
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON] },
    connect: { ok: false, connected: false, error: "Authentication failed" },
  });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();

  // The reason sits on the row that failed, in the popover of its own warning
  // triangle — not in a bar under a shelf that may be eight rows long.
  const failures = h.failures();
  assert.equal(failures.length, 1);
  assert.equal(failures[0].text, "Authentication failed");
  assert.equal(failures[0].ariaLabel, "Why Start mission did not run");
  // And it is pushed at an operator who has already turned back to the
  // aircraft: api.notification is a toast plus a line on the board.
  assert.deepEqual(h.notes, [
    { level: "warning", message: "Start mission: Authentication failed" },
  ]);
  // One send: the probe that discovered there was no session. Nothing is typed
  // after the connect failed — there is nothing on the far end to type into.
  assert.equal(h.sends().length, 1);
  assert.equal(h.dots().length, 0, "and the row does not claim to be running");
  Corvus.pluginSshLauncher.destroy(h.container);
}

function testSshLauncherMissingConnections() {
  const L = Corvus.pluginSshLauncher;
  const buttons = L.normalizeButtons({ buttons: [TERMINAL_BUTTON, BACKGROUND_BUTTON,
    Object.assign({}, TERMINAL_BUTTON, { id: "c" })] });
  assert.deepEqual(L.missingConnections(buttons, []), ["companion", "ground"]);
  assert.deepEqual(L.missingConnections(buttons, [{ name: "ground" }]), ["companion"]);
  assert.deepEqual(L.missingConnections(buttons, [{ name: "companion" }, { name: "ground" }]), []);
}

async function testSshLauncherACopiedShelfSaysWhatIsMissing() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON] }, connections: [] });
  await flushMicrotasks();
  assert.match(h.notesShown().join("\n"), /Not set up on this computer yet: \u201Ccompanion\u201D/);
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherSetsUpAMissingConnectionAndPressesAgain() {
  const error = new Error("no saved connection named 'companion'");
  error.status = 400;
  error.body = { error: error.message, needs: "connection", connection: "companion" };
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON] },
    connections: [],
    connectOnce: error,
    sshSetup: () => true,
  });
  await flushMicrotasks();
  click(h.shelf()[0]);
  for (let i = 0; i < 6; i++) await flushMicrotasks();
  assert.equal(h.setups.length, 1);
  assert.equal(h.setups[0].connection, "companion");
  assert.equal(h.connects().length, 2, "pressed again once it was set up");
  assert.equal(h.failures().length, 0);
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherAskedLoginThatIsCancelledStaysAFailure() {
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON] },
    connectOnce: { ok: false, connected: false, error: "Authentication failed",
      needs: "credentials", connection: "companion" },
    sshSetup: () => false,
  });
  await flushMicrotasks();
  click(h.shelf()[0]);
  for (let i = 0; i < 6; i++) await flushMicrotasks();
  assert.equal(h.setups.length, 1);
  assert.equal(h.connects().length, 1);
  assert.equal(h.failures()[0].text, "Authentication failed");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherWarningSitsOnTheToolsCentreLine() {
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON] },
    connect: { ok: false, connected: false, error: "Authentication failed" },
  });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();

  // The triangle is one of the row's tools, so its glyph is the pencil's size.
  // Lucide carries the inline size onto the <svg>, so this is the size drawn.
  const glyph = h.failures()[0].btn.children[0];
  const pencil = h.tool("Edit Start mission").children[0];
  assert.equal(glyph.style.width, pencil.style.width);
  assert.equal(glyph.style.height, pencil.style.height);
  Corvus.pluginSshLauncher.destroy(h.container);

  // A 16px hint beside 28px buttons was left at the top of the row by the
  // default stretch; the row has to centre its tools, and the triangle has to
  // take their box.
  const css = require("node:fs").readFileSync(
    require("node:path").join(__dirname, "..", "ssh-launcher.css"), "utf8");
  const block = (sel) => {
    const m = css.match(new RegExp(sel.replace(/[.[\]]/g, "\\$&") + "\\s*\\{([^}]*)\\}"));
    return m ? m[1] : "";
  };
  assert.match(block(".sshl-row-tools"), /align-items:\s*center/);
  assert.match(block(".ui-info.sshl-error"), /width:\s*28px/);
  assert.match(block(".ui-info.sshl-error"), /height:\s*28px/);
}

async function testSshLauncherAFailureGoesWhenTheButtonWorksAgain() {
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON] },
    connect: { ok: false, connected: false, error: "Authentication failed" },
  });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  assert.equal(h.failures().length, 1);

  // Someone brought the machine back: the session is up, so the line goes
  // straight into it and the press succeeds.
  h.setSessions(["ssh-launcher/a"]);
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();

  assert.equal(h.failures().length, 0,
    "the triangle answered the previous press, not this one");
  assert.equal(h.notes.length, 1, "and nothing new was pushed at the operator");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherBackgroundButtonUsesTheRunEndpoint() {
  const h = mountLauncher({ saved: { buttons: [BACKGROUND_BUTTON] } });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();

  const run = h.calls.find((c) => c.url === "/api/ssh/run");
  assert.deepEqual(run.body, {
    name: "ground", directory: "", command: "./record.sh", detach: true,
  }, "the saved connection is named, never a password");
  assert.ok(!h.calls.some((c) => c.url === "/api/ssh/connect"),
    "a background button opens no session to leave behind");
  Corvus.pluginSshLauncher.destroy(h.container);
}

/* The arrow before the first press: the operator wants the terminal, and the
   connection behind it, without starting the program yet. */
async function testSshLauncherArrowConnectsBeforeTheFirstPress() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON] } });
  await flushMicrotasks();
  const arrow = h.tool("Open the terminal for Start mission");
  assert.ok(arrow, "a terminal button has an arrow");
  assert.equal(arrow.disabled, false, "with nothing running the arrow still works");
  assert.match(arrow.title, /connect/i, "and says it connects");
  assert.equal(h.dots().length, 0, "nothing is running yet");

  click(arrow);
  await flushMicrotasks();
  await flushMicrotasks();
  await flushMicrotasks();

  assert.deepEqual(h.connects(), [{ name: "ssh-launcher/a", from: "companion" }],
    "the button's own session, on its saved connection");
  assert.deepEqual(h.sends(), [], "the program is not started by the arrow");
  assert.equal(h.terminals.length, 1, "its terminal window is opened");
  assert.equal(h.terminals[0].name, "ssh-launcher/a");
  assert.deepEqual(h.termOpts[0], { reattach: true, existingOnly: false },
    "a window left on an ended shell takes the new one");
  assert.equal(h.dots().length, 1, "the row shows the session is up");

  // The button now types into that same shell, and opens no second one.
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  assert.equal(h.connects().length, 1, "no second connect");
  assert.deepEqual(h.sends(), [{
    name: "ssh-launcher/a", data: Corvus.pluginSshLauncher.remoteLine(TERMINAL_BUTTON) + "\n",
  }], "the line went straight into the shell the arrow opened");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherArrowThatCannotConnectSaysWhy() {
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON] },
    connect: { ok: false, connected: false, error: "Authentication failed" },
  });
  await flushMicrotasks();
  click(h.tool("Open the terminal for Start mission"));
  await flushMicrotasks();
  await flushMicrotasks();
  await flushMicrotasks();

  assert.equal(h.terminals.length, 0, "no window onto a shell that is not there");
  assert.equal(h.failures().length, 1, "the row says why");
  assert.match(h.failures()[0].text, /Authentication failed/);
  assert.equal(h.tool("Open the terminal for Start mission").disabled, false,
    "and the arrow can be pressed again");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherBackgroundButtonHasNoArrow() {
  const h = mountLauncher({ saved: { buttons: [BACKGROUND_BUTTON] } });
  await flushMicrotasks();
  assert.equal(h.tool("Open the terminal for Record logs"), undefined,
    "a detached program has no terminal to open");
  Corvus.pluginSshLauncher.destroy(h.container);
}

/* A launch is a launch. Opening the window on every press threw a terminal at
   an operator who asked for a program to start — four buttons on the pad would
   have meant four windows in the way. The arrow is the request to watch. */
async function testSshLauncherLaunchingOpensNoTerminalWindow() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON] } });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  await flushMicrotasks();

  const asked = h.termOpts.filter((o) => !o || !o.existingOnly);
  assert.deepEqual(asked, [], "nothing was opened for the operator to look at");
  // A window that IS open must still be repaired when a new shell took over
  // the session name, so the launch may touch one — quietly, and only that.
  h.termOpts.forEach((o) => assert.equal(o.existingOnly, true,
    "a launch only ever repairs a window that is already there"));
  Corvus.pluginSshLauncher.destroy(h.container);
}

/* Two buttons are two sessions: the whole point of a shelf is that the mission
   script and the video pipeline run side by side, each in a terminal of its
   own when the operator asks to see them. */
async function testSshLauncherTwoButtonsUseTwoSeparateSessions() {
  const second = { id: "b", label: "Video", connection: "ground",
    directory: "/opt/cam", command: "./stream.sh", mode: "terminal" };
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON, second] } });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  await flushMicrotasks();
  click(h.shelf()[1]);
  await flushMicrotasks();
  await flushMicrotasks();
  await flushMicrotasks();

  assert.deepEqual(h.connects().map((b) => b.name),
    ["ssh-launcher/a", "ssh-launcher/b"],
    "each button opens a session of its own");
  assert.deepEqual(h.connects().map((b) => b.from), ["companion", "ground"],
    "each on its own saved connection");
  assert.equal(h.dots().length, 2, "and both rows show they are running");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherArrowOpensTheSessionsTerminal() {
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON] },
    sessions: ["ssh-launcher/a"],
  });
  await flushMicrotasks();
  const arrow = h.tool("Open the terminal for Start mission");
  assert.equal(arrow.disabled, false, "a live session makes the arrow live too");
  assert.equal(h.dots().length, 1, "and the row shows it is running");
  // Pressing it runs the command again in that same session, so it says so
  // rather than promising a second copy — or threatening a restart.
  assert.equal(h.shelf()[0].getAttribute("aria-label"), "Run Start mission again");
  assert.match(h.shelf()[0].title, /already in/);

  click(arrow);
  assert.deepEqual(h.terminals[0], {
    name: "ssh-launcher/a",
    title: "Start mission",
    host: "10.0.0.7",
    port: 22,
    username: "pilot",
  }, "the window gets the session key plus a readable title and the host it is on");
  assert.deepEqual(h.termOpts[0], { reattach: false, existingOnly: false },
    "the arrow opens the window — it is the one thing that does");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherLaunchingMarksTheRowRunning() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON] } });
  await flushMicrotasks();
  assert.equal(h.dots().length, 0);
  click(h.shelf()[0]);
  await flushMicrotasks();
  await flushMicrotasks();
  // No waiting for the next poll: the launch that just succeeded is proof.
  assert.equal(h.dots().length, 1);
  assert.equal(h.tool("Open the terminal for Start mission").disabled, false);
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherAddsAButton() {
  const h = mountLauncher({ saved: { buttons: [] } });
  await flushMicrotasks();
  assert.equal(h.shelf().length, 0);

  click(h.byLabel("Add button")[0]);
  const [labelField, dirField, cmdField] = h.fields();
  typeInto(labelField, "Start mission");
  typeInto(dirField, "/srv/mission");
  typeInto(cmdField, "./run.sh");
  assert.equal(h.preview().textContent, "ssh companion 'cd /srv/mission && ./run.sh'",
    "the editor previews what the new button will run");

  click(h.byLabel("Save")[0]);
  await flushMicrotasks();

  assert.equal(h.shelf().length, 1, "the new button is on the shelf");
  const saved = h.savedNow().buttons;
  assert.equal(saved.length, 1, "and was persisted");
  assert.equal(saved[0].label, "Start mission");
  assert.equal(saved[0].command, "./run.sh");
  assert.equal(saved[0].mode, "terminal", "a new button runs in a terminal by default");
  assert.equal(saved[0].connection, "companion", "and defaults to the first connection");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherCancelDiscardsTheDraft() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON] } });
  await flushMicrotasks();

  click(h.tool("Edit Start mission"));
  typeInto(h.fields()[0], "Renamed");
  click(h.byLabel("Cancel")[0]);
  await flushMicrotasks();

  assert.equal(h.shelf()[0].getAttribute("aria-label"), "Launch Start mission",
    "Cancel really cancels — the shelf still holds the original");
  assert.equal(h.savedNow().buttons[0].label, "Start mission",
    "and nothing was written back over it");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherEditsInPlaceWithoutAddingOne() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON, BACKGROUND_BUTTON] } });
  await flushMicrotasks();

  click(h.tool("Edit Start mission"));
  typeInto(h.fields()[0], "Mission A");
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();

  const saved = h.savedNow().buttons;
  assert.equal(saved.length, 2, "editing replaces, it does not append");
  assert.equal(saved[0].id, "a", "and keeps the entry's id — its session depends on it");
  assert.equal(saved[0].label, "Mission A");
  assert.equal(saved[1].label, "Record logs", "the other button is untouched");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherRemovesAnIdleButtonWithoutAsking() {
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON, BACKGROUND_BUTTON] } });
  await flushMicrotasks();

  let asked = false;
  window.confirm = () => { asked = true; return true; };
  // Removing is in the button's own settings now: there is no trash can on the
  // shelf to mis-tap beside a launch button.
  assert.equal(h.tool("Remove Start mission"), undefined);
  click(h.tool("Edit Start mission"));
  click(h.byLabel("Delete")[0]);
  await flushMicrotasks();

  assert.equal(asked, false, "nothing is running, so nothing is at stake");
  assert.deepEqual(h.savedNow().buttons.map((b) => b.id), ["b"]);
  assert.ok(!h.calls.some((c) => c.url === "/api/ssh/disconnect"));
  Corvus.pluginSshLauncher.destroy(h.container);
}

/* A running row has no pencil, so the editor meets a running session only
   when it was opened before the shelf knew: here, before the first liveness
   answer came back. Delete still asks there, and still closes it. */
async function testSshLauncherRemovingARunningButtonAsksAndClosesItsTerminal() {
  const opts = { saved: { buttons: [TERMINAL_BUTTON] }, sessions: ["ssh-launcher/a"] };
  const h = mountLauncher(opts);
  click(h.tool("Edit Start mission"));
  await flushMicrotasks();

  // Refused: the button, and the program it is running, both stay.
  window.confirm = () => false;
  click(h.byLabel("Delete")[0]);
  await flushMicrotasks();
  assert.equal(h.byLabel("Delete").length, 1,
    "declining the prompt leaves the editor open on the button");
  click(h.byLabel("Cancel")[0]);
  assert.equal(h.shelf().length, 1, "declining the prompt keeps the button");
  assert.ok(!h.calls.some((c) => c.url === "/api/ssh/disconnect"));
  assert.equal(h.tool("Edit Start mission"), undefined, "back on the shelf it is a running row");
  assert.ok(h.tool("Stop Start mission"));
  Corvus.pluginSshLauncher.destroy(h.container);

  // Confirmed: the button goes, and so does the session only it could reach.
  window.confirm = () => true;
  const again = mountLauncher(opts);
  click(again.tool("Edit Start mission"));
  await flushMicrotasks();
  click(again.byLabel("Delete")[0]);
  await flushMicrotasks();
  assert.equal(again.shelf().length, 0);
  assert.deepEqual(again.calls.find((c) => c.url === "/api/ssh/disconnect").body,
    { name: "ssh-launcher/a" });
  Corvus.pluginSshLauncher.destroy(again.container);
}

/* While its session is up the row's pencil is a stop button: Ctrl-C, the
   grace for the program to take it, then the session closed. A background
   button has no session, so it keeps its pencil. */
async function testSshLauncherARunningRowStopsInsteadOfEditing() {
  const L = Corvus.pluginSshLauncher;
  const h = mountLauncher({
    saved: { buttons: [TERMINAL_BUTTON, BACKGROUND_BUTTON] },
    sessions: ["ssh-launcher/a"],
  });
  await flushMicrotasks();
  assert.equal(h.tool("Edit Start mission"), undefined);
  const stopBtn = h.tool("Stop Start mission");
  assert.ok(stopBtn.className.split(/\s+/).includes("sshl-stop"));
  assert.ok(h.tool("Edit Record logs"));
  assert.equal(h.tool("Stop Record logs"), undefined);

  click(stopBtn);
  await flushMicrotasks();
  assert.deepEqual(h.sends(), [{ name: "ssh-launcher/a", data: L.CTRL_C }]);
  assert.ok(!h.calls.some((c) => c.url === "/api/ssh/disconnect"),
    "the hang-up waits for the program to take the Ctrl-C");
  assert.equal(h.dots().length, 0);
  assert.equal(h.tool("Stop Start mission").disabled, true, "shown stopping");

  await new Promise((r) => setTimeout(r, L.RESTART_GRACE_MS + 50));
  await flushMicrotasks();
  assert.deepEqual(h.calls.filter((c) => c.url === "/api/ssh/disconnect").map((c) => c.body),
    [{ name: "ssh-launcher/a" }]);
  assert.ok(h.tool("Edit Start mission"), "stopped, the pencil is back");
  assert.equal(h.tool("Stop Start mission"), undefined);
  assert.match(h.status().textContent || "", /^Start mission: stopped\./);
  Corvus.pluginSshLauncher.destroy(h.container);
}

/* Pressed while it is being stopped, the button waits for the session to be
   closed and starts the program in a new one, never in the shell that is
   being hung up. */
async function testSshLauncherALaunchDuringAStopWaitsForIt() {
  const L = Corvus.pluginSshLauncher;
  const h = mountLauncher({ saved: { buttons: [TERMINAL_BUTTON] }, sessions: ["ssh-launcher/a"] });
  await flushMicrotasks();
  click(h.tool("Stop Start mission"));
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();
  assert.equal(h.sends().length, 1, "only the Ctrl-C so far");

  await new Promise((r) => setTimeout(r, L.RESTART_GRACE_MS + 50));
  await flushMicrotasks();
  await flushMicrotasks();
  const wire = new Set(["/api/ssh/send", "/api/ssh/disconnect", "/api/ssh/connect"]);
  assert.deepEqual(h.calls.filter((c) => wire.has(c.url)).map((c) => c.url), [
    "/api/ssh/send", "/api/ssh/disconnect", "/api/ssh/send", "/api/ssh/connect", "/api/ssh/send",
  ]);
  assert.ok(h.tool("Stop Start mission"), "running again, in its new session");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherSaveIsBlockedWithoutACommand() {
  const h = mountLauncher({ saved: { buttons: [] } });
  await flushMicrotasks();
  click(h.byLabel("Add button")[0]);
  const save = h.byLabel("Save")[0];
  assert.equal(h.byLabel("Delete").length, 0,
    "a button that does not exist yet has nothing to delete — Cancel drops it");
  assert.equal(save.disabled, true, "a button with nothing to run cannot be saved");
  typeInto(h.fields()[2], "./run.sh");
  assert.equal(save.disabled, false);
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherShowsStderrOfAFailedBackgroundRun() {
  const h = mountLauncher({
    saved: { buttons: [BACKGROUND_BUTTON] },
    run: { ok: false, stdout: "", stderr: "sh: ./record.sh: not found", error: "exit 127" },
  });
  await flushMicrotasks();
  click(h.shelf()[0]);
  await flushMicrotasks();

  const out = querySel(h.container.children, ".sshl-output")[0];
  assert.equal(out.hidden, false, "the failure's output is shown");
  assert.ok(out.textContent.includes("not found"));
  assert.deepEqual(h.failures().map((f) => f.text), ["sh: ./record.sh: not found"],
    "the triangle on the row carries the reason");
  assert.deepEqual(h.notes, [
    { level: "warning", message: "Record logs: sh: ./record.sh: not found" },
  ]);
  Corvus.pluginSshLauncher.destroy(h.container);
}

/* A connection typed into the editor, checked without a DOM. */
function testSshLauncherNewConnectionHelpers() {
  const { derivedName, newConnectionError, newConnectionBody } = Corvus.pluginSshLauncher;

  assert.equal(derivedName({ host: "10.0.0.7", username: "pilot" }), "pilot@10.0.0.7",
    "the name writes itself the way the operator would have typed it");
  assert.equal(derivedName({ host: "10.0.0.7" }), "10.0.0.7");
  assert.equal(derivedName({ username: "pilot" }), "", "a user without a host is not a name");

  assert.match(newConnectionError({ host: "" }, []), /host/,
    "a connection with nowhere to go cannot be saved");
  assert.equal(newConnectionError({ host: "10.0.0.7", port: "22" }, []), "");
  assert.match(newConnectionError({ host: "10.0.0.7", port: "70000" }, []), /1 and 65535/);
  assert.match(newConnectionError({ host: "10.0.0.7", port: "ssh" }, []), /1 and 65535/);
  // The one that matters: upserting an existing name would REPLACE that
  // connection's credentials and silently repoint a card the operator trusts.
  assert.match(
    newConnectionError({ host: "10.0.0.7", username: "pilot" }, ["pilot@10.0.0.7"]),
    /already saved/);
  assert.match(newConnectionError({ host: "x", name: "companion" }, ["companion"]), /already saved/);

  assert.deepEqual(
    newConnectionBody({ host: " 10.0.0.7 ", username: " pilot ", password: " s3cret ", port: "" }),
    { name: "pilot@10.0.0.7", host: "10.0.0.7", port: 22, username: "pilot",
      password: " s3cret ", key_path: "" },
    "fields are trimmed, the port defaults to 22 — and the password is left exactly as typed");
}

/* Without a single saved connection the shelf used to be a dead end: the
   editor offered an empty list and the note sent the operator to Settings.
   A shelf is built on the pad as often as at a desk, so the form is right
   there instead. */
async function testSshLauncherWithoutConnectionsOffersToTypeOneIn() {
  const h = mountLauncher({ connections: [] });
  await flushMicrotasks();
  const note = querySel(h.container.children, ".sshl-note")[0];
  assert.ok(note && /New connection/.test(note.textContent),
    "the empty state points at the form, not at a trip to Settings");

  click(h.byLabel("Add button")[0]);
  assert.ok(h.newConnShowing(), "the editor opens straight into the connection form");
  assert.equal(h.select().value, Corvus.pluginSshLauncher.NEW_CONNECTION);
  Corvus.pluginSshLauncher.destroy(h.container);
}

/* A password field that is merely hidden is still a password field in the
   page, so the form only exists while it is the answer. */
async function testSshLauncherPickingASavedConnectionHasNoForm() {
  const h = mountLauncher({ saved: { buttons: [] } });
  await flushMicrotasks();
  click(h.byLabel("Add button")[0]);
  assert.equal(h.newConnShowing(), false, "a saved connection needs no form");
  changeTo(h.select(), Corvus.pluginSshLauncher.NEW_CONNECTION);
  assert.ok(h.newConnShowing(), "and picking New connection… brings one");
  changeTo(h.select(), "companion");
  assert.equal(h.newConnFields().length, 0, "going back takes the fields away again");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherSavesATypedInConnectionBeforeTheButton() {
  const h = mountLauncher({ saved: { buttons: [] } });
  await flushMicrotasks();
  click(h.byLabel("Add button")[0]);
  changeTo(h.select(), Corvus.pluginSshLauncher.NEW_CONNECTION);

  const [host, port, user, password] = h.newConnFields();
  typeInto(host, "10.0.0.9");
  typeInto(user, "pilot");
  typeInto(password, "s3cret");
  typeInto(port, "2222");
  typeInto(h.fieldBy("Command to start"), "./run.sh");
  typeInto(h.fieldBy("Remote folder"), "/srv");

  assert.equal(h.preview().textContent, "ssh pilot@10.0.0.9 'cd /srv && ./run.sh'",
    "the preview names the connection that is about to be created");

  click(h.byLabel("Save")[0]);
  await flushMicrotasks();
  await flushMicrotasks();

  const upsert = h.calls.find((c) => c.url === "/api/ssh/connections" && c.body);
  assert.deepEqual(upsert.body, {
    name: "pilot@10.0.0.9", host: "10.0.0.9", port: 2222, username: "pilot",
    password: "s3cret", key_path: "",
  }, "the credentials go to the backend's store, under a name it can resolve");

  const saved = h.savedNow().buttons;
  assert.equal(saved.length, 1, "the button is on the shelf");
  assert.equal(saved[0].connection, "pilot@10.0.0.9",
    "and carries the NAME — never the password");
  assert.equal(JSON.stringify(saved).indexOf("s3cret"), -1,
    "nothing secret reaches the plugin's settings file");
  assert.equal(h.shelf().length, 1);
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherOffersTheNewConnectionToTheNextButton() {
  const h = mountLauncher({ saved: { buttons: [] } });
  await flushMicrotasks();
  click(h.byLabel("Add button")[0]);
  changeTo(h.select(), Corvus.pluginSshLauncher.NEW_CONNECTION);
  typeInto(h.newConnFields()[0], "10.0.0.9");
  typeInto(h.newConnFields()[2], "pilot");
  typeInto(h.fieldBy("Command to start"), "./run.sh");
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();
  await flushMicrotasks();

  click(h.byLabel("Add button")[0]);
  const values = h.select().children.map((o) => o.value);
  assert.ok(values.includes("pilot@10.0.0.9"),
    "a connection typed in once is simply pickable the next time");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherRefusesAConnectionNameThatIsAlreadySaved() {
  const h = mountLauncher({ saved: { buttons: [] } });
  await flushMicrotasks();
  click(h.byLabel("Add button")[0]);
  changeTo(h.select(), Corvus.pluginSshLauncher.NEW_CONNECTION);
  typeInto(h.newConnFields()[0], "10.9.9.9");
  typeInto(h.fieldBy("Command to start"), "./run.sh");
  // "companion" is already a saved connection; saving over it would repoint it.
  typeInto(h.newConnFields()[5], "companion");

  assert.equal(h.byLabel("Save")[0].disabled, true, "Save refuses the collision");
  assert.match(h.status().textContent, /already saved/,
    "and says why, because a disabled button explains nothing");
  assert.ok(!h.calls.some((c) => c.url === "/api/ssh/connections" && c.body),
    "nothing was written (the GET at init is not a write)");

  typeInto(h.newConnFields()[5], "companion-2");
  assert.equal(h.byLabel("Save")[0].disabled, false, "another name is fine");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherKeepsTheFormWhenTheConnectionCannotBeSaved() {
  const h = mountLauncher({
    saved: { buttons: [] },
    upsert: { ok: false, error: "host must be a non-empty string" },
  });
  await flushMicrotasks();
  click(h.byLabel("Add button")[0]);
  changeTo(h.select(), Corvus.pluginSshLauncher.NEW_CONNECTION);
  typeInto(h.newConnFields()[0], "10.0.0.9");
  typeInto(h.newConnFields()[3], "s3cret");
  typeInto(h.fieldBy("Command to start"), "./run.sh");
  click(h.byLabel("Save")[0]);
  await flushMicrotasks();
  await flushMicrotasks();

  assert.equal(h.status().textContent, "host must be a non-empty string");
  assert.equal((h.savedNow().buttons || []).length, 0,
    "a button that names a connection the backend does not have is not a button");
  assert.ok(h.newConnShowing(), "the form stays up");
  assert.equal(h.newConnFields()[3].value, "s3cret",
    "with what was typed still in it — a typo is fixed, not retyped");
  Corvus.pluginSshLauncher.destroy(h.container);
}

async function testSshLauncherDestroyStopsThePollAndLateCallbacks() {
  let resolveConnect;
  let polls = 0;
  const container = makeEl("div");
  mountedLaunchers.push(container);
  Corvus.pluginSshLauncher.init(container, {
    requestJson: (url) => {
      if (url === "/api/ssh/sessions") { polls++; return Promise.resolve({ sessions: [] }); }
      return Promise.resolve({ connections: [{ name: "companion", host: "h", username: "u" }] });
    },
    postJson: () => new Promise((r) => { resolveConnect = r; }),
    getSettings: () => ({ buttons: [{ id: "a", label: "Go", connection: "companion", command: "./run.sh" }] }),
    saveSettings: () => Promise.resolve({}),
    terminal: () => true,
    console: () => {},
    notification: () => {},
  });
  await flushMicrotasks();
  click(querySel(container.children, ".sshl-launch")[0]);
  const pollsAtTeardown = polls;
  Corvus.pluginSshLauncher.destroy(container);

  // The connect answers after the plugin was closed; nothing may be painted
  // into a container the registry has already discarded, and the liveness poll
  // must not outlive it either — an interval nobody clears is a leak.
  resolveConnect({ ok: true, connected: true });
  await flushMicrotasks();
  assert.equal(querySel(container.children, ".ui-msg")[0].hidden, true,
    "a late result does not touch a destroyed plugin");
  await new Promise((r) => setTimeout(r, Corvus.pluginSshLauncher.LIVE_POLL_MS + 60));
  assert.equal(polls, pollsAtTeardown, "the liveness poll stopped with the plugin");
}


// ---------------------------------------------------------------------------
// Run all tests.
// ---------------------------------------------------------------------------
async function run() {
  testSshLauncherRegistered();
  testSshLauncherPreviewLine();
  testSshLauncherRemoteLineQuotesTheFolderOnly();
  testSshLauncherResultSummary();
  testSshLauncherModeMigratesFromDetach();
  testSshLauncherSessionIsKeyedByIdNotLabel();
  testSshLauncherNormalizesASavedShelf();
  testSshLauncherMigratesTheOldSingleCommandShape();
  testSshLauncherNormalizeIsEmptyForNothingSaved();
  testSshLauncherCapsTheShelf();
  await testSshLauncherRendersOneButtonPerSavedEntry();
  await testSshLauncherTerminalButtonOpensASessionAndTypesTheLine();
  await testSshLauncherTerminalButtonReportsAFailedConnect();
  await testSshLauncherWarningSitsOnTheToolsCentreLine();
  await testSshLauncherAFailureGoesWhenTheButtonWorksAgain();
  await testSshLauncherBackgroundButtonUsesTheRunEndpoint();
  await testSshLauncherArrowConnectsBeforeTheFirstPress();
  await testSshLauncherArrowThatCannotConnectSaysWhy();
  await testSshLauncherBackgroundButtonHasNoArrow();
  await testSshLauncherPressingItAgainRunsInTheSameSession();
  testSshLauncherRestartsOnlyAProgramInATerminal();
  await testSshLauncherRestartSendsCtrlCFirst();
  await testSshLauncherRestartSwitchIsSavedAndOnlyForATerminal();
  await testSshLauncherReopensASessionThatHasEnded();
  await testSshLauncherLaunchingOpensNoTerminalWindow();
  await testSshLauncherTwoButtonsUseTwoSeparateSessions();
  await testSshLauncherArrowOpensTheSessionsTerminal();
  await testSshLauncherLaunchingMarksTheRowRunning();
  await testSshLauncherAddsAButton();
  await testSshLauncherCancelDiscardsTheDraft();
  await testSshLauncherEditsInPlaceWithoutAddingOne();
  await testSshLauncherRemovesAnIdleButtonWithoutAsking();
  await testSshLauncherRemovingARunningButtonAsksAndClosesItsTerminal();
  await testSshLauncherARunningRowStopsInsteadOfEditing();
  await testSshLauncherALaunchDuringAStopWaitsForIt();
  await testSshLauncherSaveIsBlockedWithoutACommand();
  await testSshLauncherShowsStderrOfAFailedBackgroundRun();
  testSshLauncherNewConnectionHelpers();
  await testSshLauncherWithoutConnectionsOffersToTypeOneIn();
  await testSshLauncherPickingASavedConnectionHasNoForm();
  await testSshLauncherSavesATypedInConnectionBeforeTheButton();
  await testSshLauncherOffersTheNewConnectionToTheNextButton();
  await testSshLauncherRefusesAConnectionNameThatIsAlreadySaved();
  await testSshLauncherKeepsTheFormWhenTheConnectionCannotBeSaved();
  await testSshLauncherDestroyStopsThePollAndLateCallbacks();
  testSshLauncherMissingConnections();
  await testSshLauncherACopiedShelfSaysWhatIsMissing();
  await testSshLauncherSetsUpAMissingConnectionAndPressesAgain();
  await testSshLauncherAskedLoginThatIsCancelledStaysAFailure();

  // Let the best-effort postAction microtasks (from destroy) drain so the
  // process exits cleanly with no pending unhandled work, and close the
  // toasts the failure tests raised, whose auto-dismiss timers would otherwise
  // hold the process open for another six seconds.
  dismissToasts();
  await flushMicrotasks();

  console.log("ssh launcher plugin tests passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
}).finally(() => {
  destroyLaunchers();   // a failed assertion must not leave a poll running
  dismissToasts();      // nor a toast's auto-dismiss timer
});
