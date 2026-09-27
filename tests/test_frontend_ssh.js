"use strict";

/**
 * How a saved SSH connection is written down (Corvus.panel.sshAddress), and
 * the arrow on its card that opens it in a terminal window of its own
 * (Corvus.panel.openSSHWindow).
 *
 * The account is part of a connection's identity — two saved entries can point
 * at the same machine and differ only in which user they log in as — but the
 * SSH cards used to render the host alone, and the terminal header printed a
 * hardcoded "corvus@companion" for every session. An operator logging in as
 * anyone else was told they were someone they are not.
 *
 * Run:
 *   node tests/test_frontend_ssh.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, o = {}) { this.type = type; this.detail = o.detail; }
};
window.addEventListener = () => {};
window.removeEventListener = () => {};
global.localStorage = { getItem: () => null, setItem: () => {}, removeItem: () => {} };
global.document = {
  createElement: () => ({
    className: "", textContent: "", children: [], dataset: {}, style: {},
    classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } },
    appendChild() {}, setAttribute() {}, addEventListener() {},
  }),
  createTextNode: () => ({}),
  getElementById: () => null,
  querySelector: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
};

require("./../src/js/ui.js");
require("./../src/js/panel.js");

const { sshAddress, sshNeeds } = Corvus.panel;

function testTheAccountIsNamed() {
  assert.equal(
    sshAddress({ name: "CORVUS-01", host: "192.168.2.10", port: 22, username: "schwalby" }),
    "schwalby@192.168.2.10:22",
    "the account that logs in must be visible on the card",
  );
}

function testTwoEntriesDifferingOnlyByAccountReadDifferently() {
  const a = sshAddress({ host: "10.0.0.5", port: 22, username: "pi" });
  const b = sshAddress({ host: "10.0.0.5", port: 22, username: "schwalby" });
  assert.notEqual(a, b, "same host, different user must not render identically");
}

function testMissingPiecesDegradeInsteadOfPrintingUndefined() {
  assert.equal(sshAddress({ host: "10.0.0.5" }), "10.0.0.5");
  assert.equal(sshAddress({ host: "10.0.0.5", username: "pi" }), "pi@10.0.0.5");
  assert.equal(sshAddress({}), "");
}

function testSshNeedsReadsAReplyOrARejectedRequest() {
  const body = { ok: false, error: "Authentication failed.", needs: "credentials", connection: "companion" };
  assert.deepEqual(sshNeeds(body), { name: "companion", needs: "credentials", error: "Authentication failed." });
  const error = new Error("no saved connection named 'x'");
  error.body = { error: error.message, needs: "connection", connection: "x" };
  assert.deepEqual(sshNeeds(error), { name: "x", needs: "connection", error: error.message });
}

function testSshNeedsAsksForNothingElse() {
  assert.equal(sshNeeds(null), null);
  assert.equal(sshNeeds({ ok: false, error: "No answer from 10.0.0.7:22 within 8 s." }), null);
  assert.equal(sshNeeds({ needs: "connection" }), null, "no name, nothing to save it under");
  assert.equal(sshNeeds({ needs: "coffee", connection: "x" }), null, "an unknown need");
  assert.equal(sshNeeds(new Error("plain")), null);
}

/* The arrow on a connection card: the session in a terminal window of its
   own. A card that is not connected connects first; one that fails opens
   nothing and says why on its CONNECT. */
function stubWindows() {
  const opened = [];
  const posts = [];
  Corvus.termWindows = { open: (session, opts) => { opened.push({ session, opts }); return true; } };
  return { opened, posts };
}

function stubConnect(reply, posts) {
  global.fetch = (url, opts) => {
    posts.push({ url, body: JSON.parse(opts.body) });
    return Promise.resolve({ json: () => Promise.resolve(reply) });
  };
}

function fakeButton() {
  return {
    textContent: "CONNECT", disabled: false, parentNode: null,
    classList: { toggle() {}, add() {}, remove() {}, contains() { return false; } },
  };
}

async function testTheArrowConnectsThenOpensAWindow() {
  const { opened, posts } = stubWindows();
  stubConnect({ ok: true, connected: true, host: "192.168.1.99", port: 22, username: "root" }, posts);
  const conn = { name: "ProxmoxTest", host: "192.168.1.99", port: 22, username: "root" };
  const shown = await Corvus.panel.openSSHWindow(conn, false, fakeButton(), fakeButton());
  assert.equal(shown, true);
  assert.deepEqual(posts, [{ url: "/api/ssh/connect", body: { name: "ProxmoxTest" } }],
    "the saved connection, by name: the backend holds the login");
  assert.deepEqual(opened, [{
    session: { name: "ProxmoxTest", title: "ProxmoxTest", host: "192.168.1.99", port: 22, username: "root" },
    opts: { reattach: true },
  }], "a window of its own, taking the new shell if one was left open");
}

async function testAConnectedCardOpensItsWindowWithoutConnectingAgain() {
  const { opened, posts } = stubWindows();
  stubConnect({ ok: true, connected: true }, posts);
  const conn = { name: "ProxmoxTest", host: "192.168.1.99", port: 22, username: "root" };
  await Corvus.panel.openSSHWindow(conn, true, fakeButton(), fakeButton());
  assert.deepEqual(posts, [], "the session is up: no second connect");
  assert.equal(opened.length, 1);
  assert.deepEqual(opened[0].opts, { reattach: false });
}

async function testAFailedConnectOpensNothingAndSaysWhy() {
  const { opened, posts } = stubWindows();
  stubConnect({ ok: false, connected: false, error: "No answer from 192.168.1.99:22 within 8 s." }, posts);
  const connectBtn = fakeButton();
  const arrow = fakeButton();
  const shown = await Corvus.panel.openSSHWindow(
    { name: "ProxmoxTest", host: "192.168.1.99" }, false, arrow, connectBtn);
  assert.equal(shown, false);
  assert.deepEqual(opened, [], "no window onto a shell that is not there");
  assert.equal(connectBtn.textContent, "CONNECT", "the card's own button is where it is said");
  assert.equal(arrow.disabled, false, "and the arrow can be pressed again");
}

/* The card has no bin any more: deleting a saved connection is in its editor,
   beside Save, and it asks first. Built against a stand-in Corvus.ui, which
   is all the dialog needs to be asserted on. */
function standInDialogUi() {
  const real = Corvus.ui;
  const built = { buttons: {}, closed: 0, errors: [] };
  const control = () => ({ value: "", focus() {} });
  Corvus.ui = Object.assign({}, real, {
    input: (o) => Object.assign(control(), { value: o.value || "" }),
    field: () => ({}),
    message: () => ({ el: {}, show: (t) => built.errors.push(t), hide() {} }),
    button: (o) => { const b = { opts: o, disabled: false }; built.buttons[o.label] = b; return b; },
    modal: (o) => { built.actions = o.actions; return { open() {}, close() { built.closed += 1; } }; },
    setBusy: (b, on) => { if (b) b.disabled = !!on; },
  });
  return { built, restore: () => { Corvus.ui = real; } };
}

async function testDeletingAConnectionIsInItsEditorAndAsksFirst() {
  const { built, restore } = standInDialogUi();
  const posts = [];
  const ended = [];
  const saved = [];
  const asked = [];
  global.fetch = (url, opts) => {
    posts.push({ url, body: JSON.parse(opts.body) });
    return Promise.resolve({ json: () => Promise.resolve({ ok: true, connections: [] }) });
  };
  global.confirm = (text) => { asked.push(text); return asked.length > 1; };
  Corvus.sshTerm = { sessionEnded: (name) => ended.push(name) };
  document.createDocumentFragment = () => ({ appendChild() {} });
  try {
    Corvus.panel.editSSHConnection(
      { name: "Proxmox", host: "192.168.1.99", port: 22, username: "root", connected: true },
      () => saved.push(true));
    assert.deepEqual(built.actions.map((b) => b.opts.label), ["CANCEL", "DELETE", "SAVE"],
      "Delete sits between the two ways out, not beside CONNECT");
    assert.equal(built.buttons.DELETE.opts.variant, "danger");

    // Answered no: nothing happens.
    await built.buttons.DELETE.opts.onClick();
    assert.match(asked[0], /Delete the connection Proxmox\?/);
    assert.match(asked[0], /session is open and will be closed/, "a live session says so");
    assert.doesNotMatch(asked[0], /[\u2013\u2014]/, "no dashes in operator text");
    assert.deepEqual(posts, []);
    assert.equal(built.closed, 0);

    // Answered yes.
    await built.buttons.DELETE.opts.onClick();
    assert.deepEqual(posts, [{ url: "/api/ssh/connections/remove", body: { name: "Proxmox" } }]);
    assert.deepEqual(ended, ["Proxmox"], "its terminal windows and the cards hear the session is over");
    assert.equal(saved.length, 1, "the list it was opened from is drawn again");
    assert.equal(built.closed, 1);
  } finally {
    restore();
    delete global.confirm;
    delete Corvus.sshTerm;
  }
}

const tests = [
  testDeletingAConnectionIsInItsEditorAndAsksFirst,
  testTheArrowConnectsThenOpensAWindow,
  testAConnectedCardOpensItsWindowWithoutConnectingAgain,
  testAFailedConnectOpensNothingAndSaysWhy,
  testSshNeedsReadsAReplyOrARejectedRequest,
  testSshNeedsAsksForNothingElse,
  testTheAccountIsNamed,
  testTwoEntriesDifferingOnlyByAccountReadDifferently,
  testMissingPiecesDegradeInsteadOfPrintingUndefined,
];

(async () => {
  let failed = 0;
  for (const t of tests) {
    try {
      await t();
      console.log(`  ok  ${t.name}`);
    } catch (err) {
      failed += 1;
      console.error(`  FAIL ${t.name}: ${err.message}`);
    }
  }
  console.log(failed ? `${failed} failing` : `${tests.length} passing`);
  process.exit(failed ? 1 : 0);
})();
