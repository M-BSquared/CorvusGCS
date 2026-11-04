"use strict";

/**
 * Noticing a controller nobody has set up (Corvus.firstSetup).
 *
 * The detector hangs off telemetry transitions and offers the wizard in a
 * dialog; it never starts it unasked and never asks twice. Covered here:
 * a fresh board on a cable prompts once; a simulator, an armed aircraft, a
 * silenced controller and a configured one do not; a board with no firmware
 * (the backend's `link_bootloader`) is offered the Firmware step, but not
 * while a flash is running; and the plain-prose rule for the strings.
 *
 * Time is driven by hand: setTimeout is replaced with a queue the test runs.
 *
 * Run:
 *   node tests/test_frontend_firstsetup.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

global.window = global;
global.Corvus = {};

const store = {};
global.localStorage = {
  getItem: (k) => (k in store ? store[k] : null),
  setItem: (k, v) => { store[k] = String(v); },
  removeItem: (k) => { delete store[k]; },
};

// A manual clock: timers run when the test says so.
let timers = [];
let nextTimer = 1;
global.setTimeout = (fn, ms) => { const id = nextTimer++; timers.push({ id, fn, ms }); return id; };
global.clearTimeout = (id) => { timers = timers.filter((t) => t.id !== id); };
async function runTimers() {
  const due = timers; timers = [];
  for (const t of due) t.fn();
  await flush();
}
function flush() { return new Promise((r) => setImmediate(r)); }

function el() {
  return { className: "", textContent: "", children: [], appendChild(c) { this.children.push(c); return c; } };
}
let modalOpen = false;
global.document = {
  createElement: () => el(),
  querySelector: () => (modalOpen ? {} : null),
};

const dialogs = [];
Corvus.ui = {
  button: (o) => o,
  modal: (o) => {
    const d = { opts: o, opened: false, closed: false,
      open() { d.opened = true; modalOpen = true; }, close() { d.closed = true; modalOpen = false; } };
    dialogs.push(d);
    return d;
  },
};

let state = {};
let subscriber = null;
const requests = [];
let responses = {};
Corvus.telemetry = {
  subscribe(fn) { subscriber = fn; return () => { subscriber = null; }; },
  getState: () => state,
  requestJson(url) {
    requests.push(url);
    const r = responses[url];
    return r instanceof Error ? Promise.reject(r) : Promise.resolve(r);
  },
};
const switched = [];
const opened = [];
Corvus.sidenav = { switchTo: (id) => switched.push(id) };
Corvus.setup = { openView: (id, opts) => opened.push([id, opts]) };
Corvus.setupFirstSetup = { isOpen: () => false };

require(path.join(__dirname, "..", "src", "js", "first-setup.js"));
const F = Corvus.firstSetup;

const FRESH = {
  connected: true, prompt: true, needs_setup: true, fingerprint: "px4|1|2|a",
  reasons: ["No airframe is selected."],
};

function reset() {
  F.destroy();
  timers = [];
  dialogs.length = 0;
  requests.length = 0;
  opened.length = 0;
  switched.length = 0;
  Object.keys(store).forEach((k) => delete store[k]);
  modalOpen = false;
  state = { connected: false, armed: false, link_bootloader: null };
  responses = { "/api/first-setup": FRESH, "/api/firmware/status": { state: "idle" } };
  F.init();
}

function push(patch) {
  state = Object.assign({}, state, patch);
  subscriber(state);
}

function buttonLabelled(dialog, label) {
  return dialog.opts.actions.find((a) => a.label === label);
}

// ---------------------------------------------------------------------------

function testThePureSeams() {
  assert.equal(F.shouldPrompt(FRESH, false), true);
  assert.equal(F.shouldPrompt(FRESH, true), false, "a silenced controller");
  assert.equal(F.shouldPrompt(Object.assign({}, FRESH, { prompt: false }), false), false, "a simulator");
  assert.equal(F.shouldPrompt(Object.assign({}, FRESH, { connected: false }), false), false);
  assert.equal(F.shouldPrompt(null, false), false);
  assert.equal(F.bootloaderDevice({ link_bootloader: { device: "/dev/ttyACM0" } }), "/dev/ttyACM0");
  assert.equal(F.bootloaderDevice({ link_bootloader: null }), "");
  assert.equal(F.bootloaderDevice(null), "");
  assert.deepEqual(F.freshReasons(FRESH), ["No airframe is selected."]);
  assert.equal(F.freshReasons({}).length, 1, "there is always something to say");
}

function testSilencingIsRememberedPerControllerOnly() {
  reset();
  F.silence("px4|1|2|a");
  assert.equal(F.isSilenced("px4|1|2|a"), true);
  assert.equal(F.isSilenced("px4|1|2|b"), false, "the next board is still offered the wizard");
  assert.equal(F.isSilenced(""), false);
}

async function testAFreshBoardIsOfferedTheWizardOnceItHasSettled() {
  reset();
  push({ connected: true });
  assert.equal(requests.length, 0, "nothing is asked the moment the heartbeat lands");
  await runTimers();

  assert.deepEqual(requests, ["/api/first-setup"]);
  assert.equal(dialogs.length, 1);
  assert.equal(dialogs[0].opened, true);
  assert.equal(dialogs[0].opts.title, "New controller detected");

  push({ connected: true, mode: "STABILIZED" });
  await runTimers();
  assert.equal(dialogs.length, 1, "later pushes on the same link do not ask again");
}

async function testStartingTheWizardOpensTheSetupPage() {
  reset();
  push({ connected: true });
  await runTimers();

  buttonLabelled(dialogs[0], "Start setup").onClick();

  assert.deepEqual(switched, ["setup"]);
  assert.equal(opened.length, 1);
  assert.equal(opened[0][0], "firstsetup");
  assert.equal(dialogs[0].closed, true);
}

async function testNotNowAsksAgainOnTheNextConnectionButDoNotAskAgainDoesNot() {
  reset();
  push({ connected: true });
  await runTimers();
  buttonLabelled(dialogs[0], "Not now").onClick();
  push({ connected: false });
  push({ connected: true });
  await runTimers();
  assert.equal(dialogs.length, 2, "Not now lasts only until the next connection");

  buttonLabelled(dialogs[1], "Do not ask again").onClick();
  assert.equal(F.isSilenced("px4|1|2|a"), true);
  push({ connected: false });
  push({ connected: true });
  await runTimers();
  assert.equal(dialogs.length, 2, "and Do not ask again is final for that controller");
}

async function testASimulatorAnArmedAircraftAndAConfiguredBoardAreLeftAlone() {
  reset();
  responses["/api/first-setup"] = Object.assign({}, FRESH, { prompt: false });
  push({ connected: true });
  await runTimers();
  assert.equal(dialogs.length, 0, "a simulator");

  reset();
  push({ connected: true, armed: true });
  await runTimers();
  assert.equal(requests.length, 0, "an armed aircraft is not interrupted, nor even asked about");

  reset();
  responses["/api/first-setup"] = { connected: true, prompt: false, needs_setup: false, fingerprint: "x" };
  push({ connected: true });
  await runTimers();
  assert.equal(dialogs.length, 0, "a configured board");
}

async function testParametersThatAreNotThereYetAreRetriedOnceThenDropped() {
  reset();
  responses["/api/first-setup"] = { connected: false };
  push({ connected: true });
  await runTimers();
  assert.equal(requests.length, 1);
  await runTimers();
  assert.equal(requests.length, 2, "one more try");
  await runTimers();
  assert.equal(requests.length, 2, "and then it gives up quietly");
  assert.equal(dialogs.length, 0);
}

async function testAFailedReadInterruptsNobody() {
  reset();
  responses["/api/first-setup"] = new Error("offline");
  push({ connected: true });
  await runTimers();
  assert.equal(dialogs.length, 0);
}

async function testALinkThatDropsBeforeTheCheckCancelsIt() {
  reset();
  push({ connected: true });
  push({ connected: false });
  await runTimers();
  assert.equal(requests.length, 0);
}

async function testAnOpenDialogDefersTheOffer() {
  reset();
  modalOpen = true;
  push({ connected: true });
  await runTimers();
  assert.equal(requests.length, 0, "the first start setup, an update notice or anything else is not covered");
  modalOpen = false;
  await runTimers();
  assert.equal(dialogs.length, 1);
}

async function testABoardWithNoFirmwareIsOfferedTheFirmwareStep() {
  reset();
  push({ link_bootloader: { device: "/dev/ttyACM0", description: "PX4 BL FMU v6X.x" } });
  await runTimers();

  assert.deepEqual(requests, ["/api/firmware/status"]);
  assert.equal(dialogs.length, 1);
  assert.equal(dialogs[0].opts.title, "Controller without firmware");
  assert.equal(buttonLabelled(dialogs[0], "Do not ask again"), undefined, "no fingerprint to remember it by");
  buttonLabelled(dialogs[0], "Start setup").onClick();
  assert.deepEqual(opened[0], ["firstsetup", { step: "firmware" }]);
}

async function testTheBootloaderOfAFlashInProgressIsNotANewController() {
  reset();
  responses["/api/firmware/status"] = { state: "flashing" };
  push({ link_bootloader: { device: "/dev/ttyACM0" } });
  await runTimers();
  assert.equal(dialogs.length, 0);
}

async function testABoardThatIsUnpluggedBeforeTheCheckIsForgotten() {
  reset();
  push({ link_bootloader: { device: "/dev/ttyACM0" } });
  push({ link_bootloader: null });
  await runTimers();
  assert.equal(requests.length, 0);
  assert.equal(dialogs.length, 0);
}

async function testTheSameBootloaderIsOfferedOncePerAppearance() {
  reset();
  push({ link_bootloader: { device: "/dev/ttyACM0" } });
  await runTimers();
  push({ link_bootloader: { device: "/dev/ttyACM0" }, time: "t" });
  await runTimers();
  assert.equal(dialogs.length, 1);

  dialogs[0].close();
  push({ link_bootloader: null });
  push({ link_bootloader: { device: "/dev/ttyACM0" } });
  await runTimers();
  assert.equal(dialogs.length, 2, "replugged, so offered again");
}

async function testDestroyReleasesTheSubscriptionAndTheTimers() {
  reset();
  push({ connected: true });
  F.destroy();
  assert.equal(subscriber, null);
  assert.equal(timers.length, 0);
}

function testTheStringsUseNoDashes() {
  // Plain prose, no em or en dash and no spaced hyphen, per AGENTS.md.
  for (const file of ["first-setup.js", "setup-firstsetup.js"]) {
    const src = fs.readFileSync(path.join(__dirname, "..", "src", "js", file), "utf8");
    const literals = src.match(/"(?:[^"\\\n]|\\.)*"/g) || [];
    for (const lit of literals) {
      assert.ok(!/[–—]/.test(lit), `${file}: dash in ${lit}`);
      assert.ok(!/ - /.test(lit), `${file}: spaced hyphen in ${lit}`);
    }
  }
}

async function run() {
  testThePureSeams();
  testSilencingIsRememberedPerControllerOnly();
  await testAFreshBoardIsOfferedTheWizardOnceItHasSettled();
  await testStartingTheWizardOpensTheSetupPage();
  await testNotNowAsksAgainOnTheNextConnectionButDoNotAskAgainDoesNot();
  await testASimulatorAnArmedAircraftAndAConfiguredBoardAreLeftAlone();
  await testParametersThatAreNotThereYetAreRetriedOnceThenDropped();
  await testAFailedReadInterruptsNobody();
  await testALinkThatDropsBeforeTheCheckCancelsIt();
  await testAnOpenDialogDefersTheOffer();
  await testABoardWithNoFirmwareIsOfferedTheFirmwareStep();
  await testTheBootloaderOfAFlashInProgressIsNotANewController();
  await testABoardThatIsUnpluggedBeforeTheCheckIsForgotten();
  await testTheSameBootloaderIsOfferedOncePerAppearance();
  await testDestroyReleasesTheSubscriptionAndTheTimers();
  testTheStringsUseNoDashes();
  console.log("frontend first setup tests passed");
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
