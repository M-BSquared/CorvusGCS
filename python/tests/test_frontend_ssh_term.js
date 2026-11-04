"use strict";

/**
 * Frontend tests for Corvus.sshTerm's size handling (src/js/ssh-term.js):
 * which terminal sizes reach the shell through POST /api/ssh/resize, and when
 * the output stream (which starts with the session's replay) is opened.
 *
 * xterm, its FitAddon, fetch, EventSource and ResizeObserver are stubbed; the
 * test decides what the fit measures and reads back what was posted.
 *
 * Run:
 *   node tests/test_frontend_ssh_term.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
window.addEventListener = () => {};
window.removeEventListener = () => {};

// What the next fit measures, as a window being laid out would.
let measured = { cols: 80, rows: 24 };

class Terminal {
  constructor() {
    this.cols = 80; this.rows = 24; this.options = {};
    // xterm's hidden input, which holds the keyboard while the terminal does.
    const listeners = {};
    this.textarea = {
      listeners,
      addEventListener: (type, cb) => { listeners[type] = cb; },
      removeEventListener: (type) => { delete listeners[type]; },
    };
  }
  loadAddon(addon) { addon.term = this; }
  open() {}
  onData() { return { dispose() {} }; }
  attachCustomKeyEventHandler() {}
  write() {}
  reset() {}
  focus() {}
  dispose() {}
}
window.Terminal = Terminal;
window.FitAddon = {
  FitAddon: class {
    fit() { this.term.cols = measured.cols; this.term.rows = measured.rows; }
  },
};

const posted = [];
global.fetch = (url, init) => {
  if (url === "/api/ssh/resize") posted.push(JSON.parse(init.body));
  return Promise.resolve({ ok: true, json: () => Promise.resolve({ ok: true }) });
};
const streams = [];
global.EventSource = class {
  constructor(url) {
    this.url = url; this.onopen = null; this.onerror = null; this.closed = false;
    this.listeners = {};
    streams.push(this);
  }
  addEventListener(type, cb) { this.listeners[type] = cb; }
  close() { this.closed = true; }
};
let observed = null;
window.ResizeObserver = class {
  constructor(cb) { observed = cb; }
  observe() {}
  disconnect() {}
};

Corvus.ui = { token: (_name, fallback) => fallback, onThemeChange: () => () => {} };
require("../src/js/ssh-term.js");

const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const DEBOUNCE = 200;

async function testATransientTinySizeNeverReachesTheShell() {
  posted.length = 0;
  streams.length = 0;
  // The window's page is laid out before the window is shown: a few pixels.
  measured = { cols: 2, rows: 1 };
  const handle = Corvus.sshTerm.create({}, { name: "schwalby/l" }, {});
  await wait(DEBOUNCE);
  assert.deepEqual(posted, [], "two columns is a layout in progress, not a size");
  assert.equal(streams.length, 0,
    "and no output yet: the replay drawn two columns wide would stay broken");

  measured = { cols: 11, rows: 2 };
  observed();
  await wait(DEBOUNCE);
  assert.deepEqual(posted, [], "and so is a sliver of a window");

  // Shown: the real size goes through, once.
  measured = { cols: 96, rows: 30 };
  observed();
  await wait(DEBOUNCE);
  assert.deepEqual(posted, [{ name: "schwalby/l", cols: 96, rows: 30 }]);
  assert.equal(streams.length, 1, "the output starts once the terminal can show it");
  assert.equal(streams[0].url, "/api/ssh/stream?name=schwalby%2Fl");
  observed();
  await wait(DEBOUNCE);
  assert.equal(posted.length, 1, "the same size is not sent twice");

  // A real change still goes through, down to the smallest window there is.
  measured = { cols: 40, rows: 11 };
  handle.fit();
  await wait(DEBOUNCE);
  assert.deepEqual(posted[1], { name: "schwalby/l", cols: 40, rows: 11 });
  assert.equal(streams.length, 1, "one stream, however often it is refitted");
  handle.dispose();
  assert.equal(streams[0].closed, true);
}

async function testATerminalThatIsNotLaidOutYetStillStreams() {
  // A hidden panel measures nothing at all; the fit then keeps xterm's own
  // 80x24, which is a size to draw at, so the output starts at once as before.
  streams.length = 0;
  measured = { cols: 80, rows: 24 };
  const handle = Corvus.sshTerm.create({}, { name: "companion" }, {});
  assert.equal(streams.length, 1);
  handle.dispose();
}

async function testDisposedBeforeItWasEverShownOpensNothing() {
  streams.length = 0;
  measured = { cols: 2, rows: 1 };
  const handle = Corvus.sshTerm.create({}, { name: "schwalby/x" }, {});
  handle.dispose();
  measured = { cols: 96, rows: 30 };
  observed();
  await wait(DEBOUNCE);
  assert.equal(streams.length, 0);
}

/* A session that ends is said to every page: the SSH tab's card must not go
   on saying CONNECTED because the session ended in a window of its own. */
async function testAnEndedSessionIsToldToEveryPage() {
  const here = [];
  const stop = Corvus.sshTerm.onSessionEnded((name) => here.push(name));
  const elsewhere = [];
  const otherPage = new BroadcastChannel("corvus-ssh");
  otherPage.onmessage = (e) => elsewhere.push(e.data);
  try {
    Corvus.sshTerm.sessionEnded("ProxmoxTest");
    await wait(20);
    assert.deepEqual(here, ["ProxmoxTest"], "this page hears it at once");
    assert.deepEqual(elsewhere, [{ type: "ended", name: "ProxmoxTest" }], "and so do the others");

    // And the other way round: a window of its own saying so reaches this page.
    otherPage.postMessage({ type: "ended", name: "companion" });
    await wait(20);
    assert.deepEqual(here, ["ProxmoxTest", "companion"]);
  } finally {
    stop();
    otherPage.close();
  }
}

async function testAShellThatEndsSaysSoOnce() {
  streams.length = 0;
  measured = { cols: 80, rows: 24 };
  const heard = [];
  const stop = Corvus.sshTerm.onSessionEnded((name) => heard.push(name));
  let closedCalls = 0;
  const handle = Corvus.sshTerm.create({}, { name: "companion" }, { onClosed: () => { closedCalls += 1; } });
  try {
    streams[0].listeners.closed();
    streams[0].listeners.closed();
    assert.equal(closedCalls, 1);
    assert.deepEqual(heard, ["companion"], "the end is told once, whoever was watching it");
  } finally {
    stop();
    handle.dispose();
  }
}

/* The SSH tab and a window of its own can show one session, whose pty has
   one size. The terminal taking the keyboard sends its size again, so the
   shell lays out for the one being typed in. */
async function testTheTerminalBeingTypedInSetsTheSize() {
  posted.length = 0;
  streams.length = 0;
  measured = { cols: 38, rows: 30 };
  const side = Corvus.sshTerm.create({}, { name: "ProxmoxTest" }, {});
  await wait(DEBOUNCE);
  assert.deepEqual(posted, [{ name: "ProxmoxTest", cols: 38, rows: 30 }], "the side panel, narrow");

  // The window of its own (another page) set 96 columns meanwhile; back in
  // the side panel, the same 38 columns are sent again rather than skipped.
  const keyboard = side.term.textarea;
  keyboard.listeners.focus();
  assert.deepEqual(posted[1], { name: "ProxmoxTest", cols: 38, rows: 30 });

  side.dispose();
  assert.equal(keyboard.listeners.focus, undefined, "and a disposed terminal lets go of it");
}

async function run() {
  await testATransientTinySizeNeverReachesTheShell();
  await testATerminalThatIsNotLaidOutYetStillStreams();
  await testDisposedBeforeItWasEverShownOpensNothing();
  await testAnEndedSessionIsToldToEveryPage();
  await testAShellThatEndsSaysSoOnce();
  await testTheTerminalBeingTypedInSetsTheSize();
  console.log("frontend ssh-term tests passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
