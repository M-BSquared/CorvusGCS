"use strict";

/**
 * Frontend tests for the notification centre (Corvus.topbar).
 *
 * The badge is the one number an operator glances at instead of reading, so
 * every rule that decides what it counts is asserted here:
 *
 *  - unread vs read vs dismissed, and which of the three the badge reports;
 *  - closing the popover is what marks the board read, not opening it, so a
 *    notification that arrives while it is open is still visibly new;
 *  - nothing warning-level or worse is ever removed by a timer — only info
 *    ages out, because the console already holds every line verbatim;
 *  - the lifecycle milestones: a fresh link and an autopilot reboot CLEAR,
 *    a change of the armed state MARKS READ, and a disconnect does neither
 *    (state_store synthesises armed=false on one, and a dropped link is the
 *    last moment at which warnings should go quiet);
 *  - only a critical takes the screen, so PX4's NOTICE-level running
 *    commentary no longer unfolds a popover over the map mid-flight.
 *
 * The bar's STATUS block is asserted here too, because it shares this harness
 * and the same rule: it may never claim more than the vehicle actually said.
 *
 * Run:
 *   node tests/test_frontend_notifications.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, o = {}) { this.type = type; this.detail = o.detail; }
};
const windowListeners = {};
window.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
window.removeEventListener = () => {};
window.setTimeout = (fn) => { fn(); return 0; };
window.clearTimeout = () => {};
global.localStorage = { getItem: () => null, setItem: () => {}, removeItem: () => {} };

// A clock the tests move by hand, so the info TTL can be exercised without
// waiting a minute for it.
let nowMs = 1_700_000_000_000;
const RealDate = Date;
global.Date = class extends RealDate {
  constructor(...args) { super(...(args.length ? args : [nowMs])); }
  static now() { return nowMs; }
};

// ---------------------------------------------------------------------------
// DOM stub. Only what the top bar and the popover actually touch.
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], dataset: {}, style: {},
    hidden: false, disabled: false, id: "", title: "",
    _attrs: {}, _listeners: {}, _isEl: true,
    parentElement: null, parentNode: null,
  };
  let _html = "";
  Object.defineProperty(e, "innerHTML", {
    get() { return _html; },
    set(v) {
      _html = String(v);
      e.children.length = 0;
      parseInto(e, _html);
    },
  });
  Object.defineProperty(e, "firstChild", { get() { return e.children[0] || null; } });
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) { const has = e.classList.contains(c); const next = force === undefined ? !has : !!force; if (next) e.classList.add(c); else e.classList.remove(c); return next; },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => {
    if (c.parentNode && c.parentNode !== e) c.parentNode.removeChild(c);
    c.parentNode = e; c.parentElement = e;
    e.children.push(c);
    return c;
  };
  e.append = (...cs) => cs.forEach((c) => e.appendChild(c));
  e.replaceChildren = (...cs) => { e.children.length = 0; cs.forEach((c) => e.appendChild(c)); };
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; c.parentElement = null; return c; };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.removeAttribute = (k) => { delete e._attrs[k]; };
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.removeEventListener = () => {};
  e.focus = () => {};
  e.querySelector = (sel) => querySel([e], sel).filter((n) => n !== e)[0] || null;
  e.querySelectorAll = (sel) => querySel([e], sel).filter((n) => n !== e);
  e.closest = (sel) => {
    const cls = sel.replace(/^\./, "");
    let n = e;
    while (n) { if (n.className && n.className.split(/\s+/).includes(cls)) return n; n = n.parentElement; }
    return null;
  };
  return e;
}

/* The bar builds its blocks from markup strings, and topbar.js then walks the
   result — `warnEl.parentElement` has to be the .tb-value span, not the block.
   So this nests properly rather than flattening every class it can see. The
   app's markup is small and always well-formed, which is the only reason 20
   lines is enough. */
function parseInto(root, html) {
  const stack = [root];
  const token = /<(\/?)([a-zA-Z][\w-]*)((?:\s+[\w-]+="[^"]*")*)\s*(\/?)>|([^<]+)/g;
  let m;
  while ((m = token.exec(html)) !== null) {
    const [, closing, tag, attrs, selfClose, text] = m;
    if (text !== undefined) {
      const parent = stack[stack.length - 1];
      if (text.trim()) parent.textContent = (parent.textContent || "") + text;
      continue;
    }
    if (closing) { if (stack.length > 1) stack.pop(); continue; }
    const el = makeEl(tag);
    (attrs.match(/[\w-]+="[^"]*"/g) || []).forEach((pair) => {
      const eq = pair.indexOf("=");
      el.setAttribute(pair.slice(0, eq), pair.slice(eq + 2, -1));
    });
    stack[stack.length - 1].appendChild(el);
    if (!selfClose && tag !== "i" + "mg") stack.push(el);
  }
}

/* Supports the selector shapes topbar.js uses: ".a.b", ".a .b" and
   "[data-block=\"x\"]". Anything else would be a silent false negative, so it
   throws instead. */
function querySel(roots, sel) {
  const parts = sel.trim().split(/\s+/);
  let current = roots;
  parts.forEach((part) => {
    const attr = part.match(/^\[data-block="([^"]+)"\]$/);
    const next = [];
    const walk = (node) => {
      const ok = attr
        ? node.dataset && node.dataset.block === attr[1]
        : part.split(".").filter(Boolean).every((c) => node.className.split(/\s+/).includes(c));
      if (ok) next.push(node);
      (node.children || []).forEach((child) => { if (child._isEl) walk(child); });
    };
    if (!attr && part[0] !== ".") throw new Error(`unsupported selector: ${sel}`);
    current.forEach(walk);
    current = next;
  });
  return current;
}

const byId = {};
global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: (id) => byId[id] || null,
  querySelector: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
  activeElement: null,
};

// Telemetry stub: the tests push snapshots by hand and record cleared warnings.
let stateSubscriber = null;
const posted = [];
let state = {};
global.Corvus.telemetry = {
  subscribe: (fn) => { stateSubscriber = fn; },
  getState: () => state,
  postAction: (url, body) => { posted.push({ url, body }); return Promise.resolve({ ok: true }); },
};

require("./../src/js/ui.js");
require("./../src/js/notification_dedupe.js");
require("./../src/js/topbar.js");

// ---------------------------------------------------------------------------

const BASE = {
  connected: true, armed: false, boot_ms: 1000, warnings: [],
  vehicle_type: "QUAD", autopilot: "PX4", mode: "HOLD",
  battery_percent: 80, battery_voltage: 16.2, gps_fix: "3D_FIX", gps_hdop: 0.9,
  altitude_amsl: 500, groundspeed: 0, vspeed: 0,
};

function warning(level, msg, meta = "10:00:00") { return { level, msg, meta }; }

/**
 * Build the page the top bar expects and hand back the parts under test.
 *
 * The bar itself is built exactly once by topbar.js (topBarBuilt) and its
 * child references are cached, so it is created once here too and reused; only
 * the popover is rebuilt per test, which is also what keeps init()'s listeners
 * from stacking on the close and clear-all buttons.
 */
byId.topBar = makeEl("header");
byId.topBar.id = "topBar";

function mount() {
  posted.length = 0;
  nowMs = 1_700_000_000_000;
  ["warningsPopover", "warningsList", "notificationLive",
   "wpClose", "wpClearAll", "warningsTitle"].forEach((id) => {
    byId[id] = makeEl("div");
    byId[id].id = id;
  });
  byId.warningsPopover.hidden = true;

  Corvus.topbar.init();
  return {
    // The badge is a pill of two parts: the count is its own span, the level
    // is an attribute on the pill around it (topbar.js renderBlock).
    badge: () => byId.topBar.querySelector(".tb-warn-count"),
    badgeLevel: () => byId.topBar.querySelector(".tb-warn-pill").dataset.level,
    items: () => byId.warningsList.querySelectorAll(".wp-item"),
    open: () => !byId.warningsPopover.hidden,
    title: () => byId.warningsTitle.textContent,
  };
}

/** Push a telemetry snapshot, as the SSE stream would. */
function push(patch) {
  state = { ...BASE, ...patch };
  stateSubscriber(state);
  return state;
}

/**
 * Start a test from a settled, connected, disarmed vehicle with a clean board.
 *
 * topbar.js is a singleton: its read/dismissed sets and previous-snapshot
 * fields outlive a mount. Driving a disconnect -> connect here leans on the
 * fresh-link milestone to wipe all of it, which is both the isolation this
 * needs and a small extra assertion that the milestone works.
 */
function settle(ui) {
  push({ connected: false, armed: false, warnings: [] });
  push({ connected: true, armed: false, warnings: [] });
  posted.length = 0;
  if (ui.open()) clickClose();
}

/** The STATUS block's rendered value, colour class and dot class. */
function status() {
  const root = byId.topBar.querySelector('[data-block="armed"]');
  return {
    text: root.querySelector(".v-main").textContent,
    cls: root.querySelector(".tb-value").className,
    dot: root.querySelector(".tb-dot").className,
    tone: (root.dataset && root.dataset.tone) || "",
    title: root.title || "",
  };
}

function clickClose() { byId.wpClose._listeners.click.forEach((cb) => cb()); }
function clickClearAll() { byId.wpClearAll._listeners.click.forEach((cb) => cb()); }
function openPopover() {
  byId.topBar.querySelector(".tb-block.warnings")._listeners.click.forEach((cb) => cb());
}

// ===========================================================================

function testInformationalNotificationsDoNotPaintTheBadgeAmber() {
  const ui = mount();
  settle(ui);

  // A normal flight produces a steady trickle of these. The badge used to be a
  // two-way split — anything unread that was not critical became "warning" —
  // so the bar spent the whole flight looking like something was wrong. An
  // operator who learns that amber means nothing has been taught to ignore the
  // one colour that has to keep working.
  push({ warnings: [warning("info", "Mode accepted: MISSION"),
                    warning("info", "Takeoff target accepted")] });
  assert.equal(ui.badge().textContent, "2");
  assert.equal(ui.badgeLevel(), "info", "informational is its own level");

  // A real warning among them still wins.
  push({ warnings: [warning("info", "Mode accepted: MISSION"),
                    warning("warning", "Preflight Fail: compass")] });
  assert.equal(ui.badgeLevel(), "warning");

  // And a critical outranks the warning.
  push({ warnings: [warning("info", "Mode accepted: MISSION"),
                    warning("warning", "Preflight Fail: compass"),
                    warning("critical", "Arm failed: DENIED")] });
  assert.equal(ui.badgeLevel(), "critical");
}

function testAnEmptyBoardIsTheGoodColourNotAQuietBad() {
  const ui = mount();
  settle(ui);
  push({ warnings: [] });
  assert.equal(ui.badge().textContent, "0");
  assert.equal(ui.badgeLevel(), "healthy",
    "nothing wrong should look like nothing wrong");
}

function testTheStatusBlockCarriesAToneNotJustAColour() {
  const ui = mount();
  settle(ui);

  // Colour alone was carrying every one of these states, which asks the
  // operator to interpret a hue mid-flight. The three worth recognising at a
  // glance get a tone the stylesheet turns into a tinted pill.
  push({ armed: false, prearm_ok: true, landed_state: 1 });
  assert.equal(status().tone, "ready");

  push({ armed: true, prearm_ok: true, landed_state: 1, altitude_agl: 0 });
  assert.equal(status().tone, "armed");

  push({ armed: true, prearm_ok: true, landed_state: 2, altitude_agl: 20 });
  assert.equal(status().tone, "flying");

  // NOT READY deliberately gets no pill: it is the absence of a clearance,
  // not an active state, and giving it the same weight is how a bar ends up
  // amber from end to end.
  push({ armed: false, prearm_ok: false, landed_state: 1 });
  assert.equal(status().tone, "notready");
}

function testBadgeCountsUnreadAndGoesQuietOnceRead() {
  const ui = mount();
  settle(ui);

  push({ warnings: [warning("warning", "Low battery"), warning("warning", "GPS jamming")] });
  assert.equal(ui.badge().textContent, "2", "both unread");
  assert.equal(ui.badgeLevel(), "warning");

  openPopover();
  assert.equal(ui.badge().textContent, "2", "opening alone does not mark them read");
  clickClose();

  // Read, not gone: the board still says how many are on it, in a colour that
  // is no longer asking for anything.
  assert.equal(ui.badge().textContent, "2", "total once everything is read");
  assert.equal(ui.badgeLevel(), "read");
}

function testANotificationArrivingWhileOpenStaysNew() {
  const ui = mount();
  settle(ui);
  push({ warnings: [warning("warning", "Low battery")] });
  openPopover();
  clickClose();
  assert.equal(ui.badgeLevel(), "read");

  openPopover();
  push({ warnings: [warning("warning", "Low battery"), warning("warning", "Compass drift")] });
  assert.equal(ui.badge().textContent, "1", "the new one is unread while on screen");
  assert.equal(ui.badgeLevel(), "warning");
  clickClose();
  assert.equal(ui.badgeLevel(), "read");
}

function testReadItemsStayInTheListDimmedRatherThanDisappearing() {
  const ui = mount();
  settle(ui);
  push({ warnings: [warning("critical", "Preflight fail: Compass not calibrated")] });
  openPopover();
  assert.equal(ui.items().length, 1);
  assert.ok(!ui.items()[0].classList.contains("is-read"), "unread on arrival");
  assert.equal(ui.items()[0].dataset.level, "critical");
  clickClose();

  openPopover();
  assert.equal(ui.items().length, 1, "still on the board");
  assert.ok(ui.items()[0].classList.contains("is-read"), "now dimmed as read");
  clickClose();
}

function testOnlyACriticalTakesTheScreen() {
  const ui = mount();
  settle(ui);

  // PX4 emits NOTICE-level lines through a whole normal flight; a popover for
  // each of them is what trained operators to dismiss the centre unread.
  push({ warnings: [warning("warning", "Takeoff detected")] });
  assert.equal(ui.open(), false, "a warning raises the badge and nothing else");
  assert.equal(ui.badge().textContent, "1");

  push({ warnings: [warning("warning", "Takeoff detected"), warning("critical", "Battery critical")] });
  assert.equal(ui.open(), true, "a critical opens the centre");
  clickClose();
}

function testInfoAgesOutButAWarningNeverDoes() {
  const ui = mount();
  settle(ui);
  push({ warnings: [warning("info", "Home position set"), warning("warning", "Low wind margin")] });
  assert.equal(ui.badge().textContent, "2");

  nowMs += 61_000;                       // past the info TTL
  push({ warnings: [warning("info", "Home position set"), warning("warning", "Low wind margin")] });
  assert.equal(ui.badge().textContent, "1", "the info line aged out");

  openPopover();
  assert.equal(ui.items().length, 1);
  assert.equal(ui.items()[0].dataset.level, "warning",
    "what survives is the one nobody has acted on");
  clickClose();
}

function testArmingMarksTheBoardReadWithoutDeletingIt() {
  const ui = mount();
  settle(ui);
  push({ warnings: [warning("critical", "Arming denied: GPS fix required")] });
  assert.equal(ui.open(), true);
  clickClose();
  push({ warnings: [warning("critical", "Arming denied: GPS fix required"), warning("warning", "Compass drift")] });
  assert.equal(ui.badge().textContent, "1", "the second one is unread");

  // The vehicle armed, so every preflight complaint on the board was answered.
  push({ armed: true, warnings: [warning("critical", "Arming denied: GPS fix required"), warning("warning", "Compass drift")] });
  assert.equal(ui.badgeLevel(), "read", "arming marks the board read");
  assert.equal(ui.badge().textContent, "2", "and keeps both — read is not deleted");
  assert.deepEqual(posted, [], "nothing was cleared at the backend");
}

function testADisconnectDoesNotQuietenTheBoard() {
  const ui = mount();
  settle(ui);
  push({ armed: true, warnings: [] });

  const seen = [warning("critical", "Engine failure")];
  push({ armed: true, warnings: seen });
  clickClose();                                   // acknowledged in the air
  const board = seen.concat(warning("critical", "Link degraded"));
  push({ armed: true, warnings: board });
  assert.equal(ui.badge().textContent, "1", "one unread while still airborne");

  // state_store.set_disconnected synthesises armed=false. That must not read
  // as "the flight ended, stand down": a dropped link is the moment warnings
  // matter most, so neither milestone may fire on it.
  push({ connected: false, armed: false, warnings: board });
  assert.deepEqual(posted, [], "a disconnect clears nothing");
  assert.equal(ui.badge().textContent, "1", "and marks nothing read");
  assert.equal(ui.badgeLevel(), "critical", "still shouting");
  openPopover();
  assert.equal(ui.items().length, 2, "both are still on the board");
  clickClose();
}

function testAFreshLinkClearsTheBoard() {
  const ui = mount();
  settle(ui);
  push({ warnings: [warning("critical", "Preflight fail: Accel calibration")] });
  clickClose();
  push({ connected: false, warnings: [warning("critical", "Preflight fail: Accel calibration")] });
  posted.length = 0;

  // Reconnecting is a new session, possibly a different aircraft. The console
  // still holds every line, so clearing here loses nothing.
  push({ connected: true, warnings: [warning("critical", "Preflight fail: Accel calibration")] });
  assert.deepEqual(posted.map((p) => p.url), ["/api/warnings/clear"]);
  assert.equal(ui.badge().textContent, "0", "board empty straight away, not a round trip later");
  assert.equal(ui.badgeLevel(), "healthy");
}

function testAnAutopilotRebootClearsTheBoard() {
  const ui = mount();
  settle(ui);
  push({ boot_ms: 90_000, warnings: [warning("warning", "Old session warning")] });
  clickClose();
  posted.length = 0;

  // boot_ms running backwards is how the map spots a reboot too.
  push({ boot_ms: 400, warnings: [warning("warning", "Old session warning")] });
  assert.deepEqual(posted.map((p) => p.url), ["/api/warnings/clear"]);
  assert.equal(ui.badge().textContent, "0");
}

function testTheFirstSnapshotTriggersNoMilestone() {
  // A page opened against a vehicle that is already connected and armed has
  // observed no transition, and must not wipe a board nobody has seen.
  const ui = mount();
  push({ connected: true, armed: true, warnings: [warning("critical", "Held from before this page loaded")] });
  assert.deepEqual(posted, [], "nothing cleared on the very first snapshot");
  assert.equal(ui.badge().textContent, "1", "and it is unread, because this UI has not shown it");
  clickClose();
}

function testDismissingRemovesOneAndClearAllEmptiesTheBoardImmediately() {
  const ui = mount();
  settle(ui);
  push({ warnings: [warning("warning", "A"), warning("warning", "B")] });
  openPopover();
  assert.equal(ui.items().length, 2);

  ui.items()[0].querySelector(".wp-dismiss")._listeners.click.forEach((cb) => cb());
  assert.equal(ui.items().length, 1, "dismissed one");
  assert.equal(ui.items()[0].querySelector(".wp-msg").textContent, "B");

  clickClearAll();
  assert.deepEqual(posted.map((p) => p.url), ["/api/warnings/clear"]);
  // The store's next push is what really removes them, but the button has to
  // do something the moment it is pressed.
  assert.equal(ui.badge().textContent, "0", "hidden without waiting for the round trip");
  clickClose();
}

function testARejectedCommandIsNotDeletedByATimer() {
  const ui = mount();
  settle(ui);
  Corvus.topbar.notifyError("Takeoff rejected: not armed");
  assert.equal(ui.open(), true, "a command the operator is waiting on takes the screen");
  clickClose();

  // It used to remove itself after eight seconds — the one thing a rejected
  // command must not do.
  nowMs += 600_000;
  push({ warnings: [] });
  assert.equal(ui.badge().textContent, "1", "still on the board ten minutes later");
  openPopover();
  assert.equal(ui.items()[0].querySelector(".wp-msg").textContent, "Takeoff rejected: not armed");
  clickClose();
}

function testTheTitleSaysHowManyAreNew() {
  const ui = mount();
  settle(ui);
  push({ warnings: [warning("warning", "A"), warning("warning", "B")] });
  openPopover();
  assert.equal(ui.title(), "2 Notifications · 2 new");
  clickClose();
  openPopover();
  assert.equal(ui.title(), "2 Notifications", "no count once nothing is new");
  clickClose();
}

// --- the STATUS block ------------------------------------------------------

function testStatusReportsTheAutopilotsOwnPreflightVerdict() {
  const ui = mount();
  settle(ui);

  push({ armed: false, prearm_ok: true });
  assert.equal(status().text, "READY", "the vehicle says it would arm");
  assert.ok(status().cls.includes("healthy"));
  assert.ok(status().dot.includes("healthy"));

  push({ armed: false, prearm_ok: false });
  assert.equal(status().text, "NOT READY", "the vehicle is refusing to arm");
  assert.ok(status().cls.includes("warning"), "a refused preflight is not a neutral state");
}

function testStatusNeverInventsReadinessTheVehicleDidNotReport() {
  const ui = mount();
  settle(ui);

  // Firmware that does not publish MAV_SYS_STATUS_PREARM_CHECK. All we know is
  // that the switch is off, and that is all we are allowed to say — a READY
  // here would read as a preflight clearance nobody gave.
  push({ armed: false, prearm_ok: null });
  assert.equal(status().text, "STANDBY");
  assert.ok(status().cls.includes("off"));

  push({ armed: false, prearm_ok: undefined });
  assert.equal(status().text, "STANDBY", "a missing field is unknown, not ready");
}

function testArmedOnTheGroundIsNotTheSameAsFlying() {
  const ui = mount();
  settle(ui);

  // Armed, still on the ground: the one moment the word ARMED earns its place,
  // because the propellers are live and somebody can walk into them.
  push({ armed: true, prearm_ok: false, landed_state: 1, altitude_agl: 0 });
  assert.equal(status().text, "ARMED");
  assert.ok(status().cls.includes("armed"), "its own tone, not the healthy green");

  // Off the ground. Telling the operator the switch is on is the least useful
  // thing the bar could say at this point — of course it is armed, it is flying.
  push({ armed: true, prearm_ok: false, landed_state: 2, altitude_agl: 30 });
  assert.equal(status().text, "FLYING");
  assert.ok(status().cls.includes("nav"), "flying is an active state, not a fault");
}

function testAirborneFallsBackToHeightWhenTheFirmwareIsSilent() {
  const ui = mount();
  settle(ui);

  // Firmware that never sends EXTENDED_SYS_STATE (landed_state stays 0).
  push({ armed: true, landed_state: 0, altitude_agl: 0.2 });
  assert.equal(status().text, "ARMED", "0.2 m is ground noise, not flight");

  push({ armed: true, landed_state: 0, altitude_agl: 25 });
  assert.equal(status().text, "FLYING", "clearly airborne without the message");

  // An explicit ON_GROUND outranks a bad altitude reference.
  push({ armed: true, landed_state: 1, altitude_agl: 25 });
  assert.equal(status().text, "ARMED", "the vehicle's own verdict wins");
}

function testALostLinkOutranksEverything() {
  const ui = mount();
  settle(ui);

  push({ connected: false, armed: true, prearm_ok: true, landed_state: 2 });
  assert.equal(status().text, "\u2014", "nothing is known without a link");
  assert.ok(status().dot.includes("off"));
}

const tests = [
  // First, and only first: it is the one test that needs the module's
  // never-seen-a-snapshot state, which nothing can restore afterwards.
  testTheFirstSnapshotTriggersNoMilestone,
  testBadgeCountsUnreadAndGoesQuietOnceRead,
  testInformationalNotificationsDoNotPaintTheBadgeAmber,
  testAnEmptyBoardIsTheGoodColourNotAQuietBad,
  testTheStatusBlockCarriesAToneNotJustAColour,
  testANotificationArrivingWhileOpenStaysNew,
  testReadItemsStayInTheListDimmedRatherThanDisappearing,
  testOnlyACriticalTakesTheScreen,
  testInfoAgesOutButAWarningNeverDoes,
  testArmingMarksTheBoardReadWithoutDeletingIt,
  testADisconnectDoesNotQuietenTheBoard,
  testAFreshLinkClearsTheBoard,
  testAnAutopilotRebootClearsTheBoard,
  testDismissingRemovesOneAndClearAllEmptiesTheBoardImmediately,
  testARejectedCommandIsNotDeletedByATimer,
  testTheTitleSaysHowManyAreNew,
  testStatusReportsTheAutopilotsOwnPreflightVerdict,
  testStatusNeverInventsReadinessTheVehicleDidNotReport,
  testArmedOnTheGroundIsNotTheSameAsFlying,
  testAirborneFallsBackToHeightWhenTheFirmwareIsSilent,
  testALostLinkOutranksEverything,
];

let failed = 0;
for (const t of tests) {
  try { t(); console.log("ok   - " + t.name); }
  catch (e) { failed++; console.error("FAIL - " + t.name + "\n      " + (e && e.message)); }
}
if (failed) { console.error(`\n${failed}/${tests.length} notification test(s) FAILED`); process.exit(1); }
console.log(`\nAll ${tests.length} notification tests passed.`);
