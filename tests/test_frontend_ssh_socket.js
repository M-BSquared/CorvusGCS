"use strict";

/**
 * Corvus.sshTerm over its WebSocket (src/js/ssh-term.js, /api/ssh/ws).
 *
 * Every terminal on SSE held one of the six connections a browser allows per
 * host, shared by every window of the app; four terminals and nothing else
 * loaded. These tests pin what replaced it: one socket per terminal, input
 * and sizes on it in order, a reconnect after a drop that redraws instead of
 * printing twice, the end of the session reported once, and the old SSE path
 * taken only when the socket never opened.
 *
 * Run:
 *   node tests/test_frontend_ssh_socket.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
window.addEventListener = () => {};
window.removeEventListener = () => {};
Object.defineProperty(global, "location", {
  value: { protocol: "http:", host: "localhost:8000" }, configurable: true, writable: true,
});

let measured = { cols: 80, rows: 24 };
const terms = [];
class Terminal {
  constructor() {
    this.cols = 80; this.rows = 24; this.options = {}; this.written = ""; this.resets = 0;
    this.dataCb = null; terms.push(this);
  }
  loadAddon(addon) { addon.term = this; }
  open() {}
  onData(cb) { this.dataCb = cb; return { dispose() {} }; }
  attachCustomKeyEventHandler(fn) { this.keyHandler = fn; }
  hasSelection() { return !!this.selection; }
  getSelection() { return this.selection || ""; }
  write(text) { this.written += text; }
  reset() { this.resets += 1; this.written = ""; }
  focus() {}
  dispose() {}
  type(text) { this.dataCb(text); }
}
window.Terminal = Terminal;
window.FitAddon = {
  FitAddon: class { fit() { this.term.cols = measured.cols; this.term.rows = measured.rows; } },
};

const posts = [];
global.fetch = (url, init) => {
  posts.push({ url, body: init && init.body ? JSON.parse(init.body) : null });
  return Promise.resolve({ ok: true, json: () => Promise.resolve({ ok: true }) });
};
const streams = [];
global.EventSource = class {
  constructor(url) { this.url = url; this.closed = false; streams.push(this); }
  addEventListener() {}
  close() { this.closed = true; }
};
const sockets = [];
class FakeSocket {
  constructor(url) {
    this.url = url; this.sent = []; this.closed = false; this.closeCode = null;
    this.onopen = null; this.onmessage = null; this.onclose = null; this.onerror = null;
    sockets.push(this);
  }
  send(text) { this.sent.push(JSON.parse(text)); }
  close(code) { this.closed = true; this.closeCode = code; }
  // What the network does:
  open() { this.onopen(); }
  receive(msg) { this.onmessage({ data: JSON.stringify(msg) }); }
  drop() { if (this.onclose) this.onclose({}); }
}
window.WebSocket = FakeSocket;
window.ResizeObserver = class { constructor() {} observe() {} disconnect() {} };

Corvus.ui = { token: (_n, fallback) => fallback, onThemeChange: () => () => {} };
require("../src/js/ssh-term.js");

const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const DEBOUNCE = 200;

function reset() {
  sockets.length = 0; streams.length = 0; posts.length = 0; terms.length = 0;
  measured = { cols: 96, rows: 30 };
}

async function testOneSocketCarriesOutputInputAndSize() {
  reset();
  const handle = Corvus.sshTerm.create({}, { name: "schwalby/l" }, {});
  assert.equal(sockets.length, 1);
  assert.equal(sockets[0].url, "ws://localhost:8000/api/ssh/ws?name=schwalby%2Fl");
  assert.equal(streams.length, 0, "no SSE stream beside it");
  const term = terms[0];
  term.type("l");
  term.type("s");
  assert.deepEqual(sockets[0].sent, [], "typed before it opened: kept, not lost");
  sockets[0].open();
  assert.deepEqual(sockets[0].sent, [
    { type: "resize", cols: 96, rows: 30 },
    { type: "input", data: "ls" },
  ]);
  sockets[0].receive({ type: "output", text: "banner\r\n$ " });
  assert.equal(term.written, "banner\r\n$ ");
  term.type("\r");
  assert.deepEqual(sockets[0].sent[2], { type: "input", data: "\r" });
  measured = { cols: 120, rows: 40 };
  handle.fit();
  await wait(DEBOUNCE);
  assert.deepEqual(sockets[0].sent[3], { type: "resize", cols: 120, rows: 40 });
  assert.deepEqual(posts, [], "nothing over HTTP requests: they are what ran out");
  handle.dispose();
  assert.equal(sockets[0].closed, true);
  assert.equal(sockets[0].closeCode, 1000);
}

async function testADroppedSocketReconnectsAndRedrawsInsteadOfPrintingTwice() {
  reset();
  const handle = Corvus.sshTerm.create({}, { name: "companion" }, {});
  const term = terms[0];
  sockets[0].open();
  sockets[0].receive({ type: "output", text: "old screen" });
  sockets[0].drop();
  assert.equal(sockets.length, 1, "not at once: a backend restarting needs a moment");
  term.type("x");
  await wait(700);
  assert.equal(sockets.length, 2, "it came back by itself");
  sockets[1].open();
  assert.equal(term.resets, 1, "the replay redraws the session rather than repeating it");
  assert.deepEqual(sockets[1].sent.slice(-1)[0], { type: "input", data: "x" },
    "what was typed during the gap is not lost");
  assert.deepEqual(sockets[1].sent[0], { type: "resize", cols: 96, rows: 30 },
    "and the size is told again to what may be a new shell");
  handle.dispose();
}

async function testTheEndOfTheSessionIsReportedOnceAndNotReconnected() {
  reset();
  let closed = 0;
  const handle = Corvus.sshTerm.create({}, { name: "gone" }, { onClosed: () => { closed += 1; } });
  sockets[0].open();
  sockets[0].receive({ type: "closed", name: "gone" });
  sockets[0].drop();
  await wait(700);
  assert.equal(closed, 1);
  assert.equal(sockets.length, 1, "an ended session is not dialled again");
  handle.fit();
  assert.equal(sockets.length, 1, "not by a refit either");
  handle.dispose();
}

async function testASocketThatNeverOpensFallsBackToTheStreamAndPosts() {
  reset();
  const handle = Corvus.sshTerm.create({}, { name: "old-backend" }, {});
  const term = terms[0];
  term.type("pwd\r");
  sockets[0].drop();
  assert.equal(streams.length, 1, "the SSE stream takes over");
  assert.equal(streams[0].url, "/api/ssh/stream?name=old-backend");
  await wait(20);
  assert.deepEqual(posts.find((p) => p.url === "/api/ssh/send").body, { name: "old-backend", data: "pwd\r" },
    "and what was typed while it tried is sent");
  assert.ok(posts.some((p) => p.url === "/api/ssh/resize"), "with the size");
  await wait(700);
  assert.equal(sockets.length, 1, "no second socket for a backend that has none");
  handle.dispose();
  assert.equal(streams[0].closed, true);
}

async function testWithoutAPageAddressTheStreamIsUsed() {
  reset();
  const saved = global.location;
  global.location = undefined;
  try {
    assert.equal(Corvus.sshTerm.socketUrl("x"), null);
    const handle = Corvus.sshTerm.create({}, { name: "x" }, {});
    assert.equal(sockets.length, 0);
    assert.equal(streams.length, 1);
    handle.dispose();
  } finally {
    global.location = saved;
  }
  assert.equal(Corvus.sshTerm.socketUrl("a b"), "ws://localhost:8000/api/ssh/ws?name=a%20b");
  global.location = { protocol: "https:", host: "gcs.local" };
  assert.equal(Corvus.sshTerm.socketUrl("a"), "wss://gcs.local/api/ssh/ws?name=a");
  global.location = saved;
}

async function testCopyAndPasteKeysDoWhatATerminalDoes() {
  reset();
  const copied = [];
  const saved = global.navigator;
  Object.defineProperty(global, "navigator", {
    value: { clipboard: { writeText: (t) => { copied.push(t); return Promise.resolve(); } } },
    configurable: true, writable: true,
  });
  try {
    const handle = Corvus.sshTerm.create({}, { name: "keys" }, {});
    const term = terms[0];
    const key = (k, mods) => term.keyHandler(Object.assign({ type: "keydown", key: k }, mods));
    term.selection = "uname -a";
    assert.equal(key("C", { ctrlKey: true, shiftKey: true }), false, "Ctrl-Shift-C with a selection copies");
    assert.equal(key("c", { metaKey: true }), false, "and so does Cmd-C");
    assert.deepEqual(copied, ["uname -a", "uname -a"]);
    term.selection = "";
    assert.equal(key("c", { ctrlKey: true }), true, "Ctrl-C without one is SIGINT, for the shell");
    assert.equal(key("V", { ctrlKey: true, shiftKey: true }), false,
      "Ctrl-Shift-V is left to the browser, which pastes, instead of reaching the shell as ^V");
    assert.equal(key("v", { ctrlKey: true }), true, "Ctrl-V stays the shell's");
    handle.dispose();
  } finally {
    Object.defineProperty(global, "navigator", { value: saved, configurable: true, writable: true });
  }
}

(async () => {
  const tests = [
    testOneSocketCarriesOutputInputAndSize,
    testADroppedSocketReconnectsAndRedrawsInsteadOfPrintingTwice,
    testTheEndOfTheSessionIsReportedOnceAndNotReconnected,
    testASocketThatNeverOpensFallsBackToTheStreamAndPosts,
    testWithoutAPageAddressTheStreamIsUsed,
    testCopyAndPasteKeysDoWhatATerminalDoes,
  ];
  for (const t of tests) {
    await t();
    console.log(`  ok  ${t.name}`);
  }
  console.log(`All ${tests.length} terminal socket tests passed.`);
})().catch((err) => {
  console.error(err);
  process.exit(1);
});
