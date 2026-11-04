"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.firstSetup: noticing a controller nobody has set up yet.
 *
 * Two things put a controller in front of the operator that needs the first
 * setup, and this module watches for both:
 *
 *   a fresh board on a live link   GET /api/first-setup says the airframe is
 *                                  unset or no sensor was ever calibrated, and
 *                                  the link is a cable or a radio (a simulator
 *                                  is never prompted, see corvus/first_setup.py)
 *   a board with no firmware       the backend publishes `link_bootloader` when
 *                                  a flight controller shows up in its USB
 *                                  bootloader; it speaks no MAVLink, so it can
 *                                  never be a link
 *
 * Either one opens a dialog that offers the wizard (src/js/setup-firstsetup.js)
 * and never starts it unasked: the operator may be holding a controller they
 * only wanted to read a parameter from. "Not now" lasts until the next
 * connection; "Do not ask again" is remembered for that controller only, keyed
 * by the fingerprint the backend builds from the board's USB identity, so the
 * next fresh board is still offered the wizard. Finishing the wizard counts as
 * not asking again.
 *
 * What is remembered lives in localStorage, a convenience of this browser
 * profile: losing it costs one more dialog, never a wrong answer.
 *
 * Nothing here polls. The checks hang off telemetry transitions (connected,
 * the bootloader port) and run once each, after a settle delay.
 */
Corvus.firstSetup = (function () {
  const STORE_KEY = "corvus.firstSetup.v1";
  /* A fresh board's parameters are still arriving for a moment after the
     heartbeat, and a flash briefly shows the bootloader port too. */
  const CONNECT_SETTLE_MS = 3500;
  const BOOTLOADER_SETTLE_MS = 4000;
  const RETRY_MS = 4000;
  const MODAL_RETRY_MS = 5000;

  /* ------------------------------ memory ------------------------------ */

  function readStore() {
    try {
      const raw = JSON.parse(window.localStorage.getItem(STORE_KEY) || "{}");
      return { quiet: Array.isArray(raw.quiet) ? raw.quiet.map(String) : [] };
    } catch (_e) {
      return { quiet: [] };
    }
  }

  function writeStore(store) {
    try { window.localStorage.setItem(STORE_KEY, JSON.stringify(store)); } catch (_e) { /* optional */ }
  }

  /** Stop offering the wizard for this controller. */
  function silence(fingerprint) {
    if (!fingerprint) return;
    const store = readStore();
    if (store.quiet.indexOf(fingerprint) < 0) store.quiet.push(fingerprint);
    store.quiet = store.quiet.slice(-50);
    writeStore(store);
  }

  function isSilenced(fingerprint) {
    return !!fingerprint && readStore().quiet.indexOf(fingerprint) >= 0;
  }

  /* ------------------------------- pure -------------------------------- */

  /** Whether a GET /api/first-setup payload earns an unprompted dialog. Pure. */
  function shouldPrompt(payload, silenced) {
    const p = payload || {};
    if (!p.connected || !p.prompt) return false;
    return !silenced;
  }

  /** The device of a published `link_bootloader`, or "". Pure. */
  function bootloaderDevice(state) {
    const b = state && state.link_bootloader;
    return b && b.device ? String(b.device) : "";
  }

  /** The dialog's reasons for a fresh controller. Pure. */
  function freshReasons(payload) {
    const list = (payload && payload.reasons) || [];
    return list.length ? list : ["This controller looks like it has not been set up."];
  }

  /* ------------------------------ detector ----------------------------- */

  let started = false;
  let unsub = null;
  let lastConnected = false;
  let lastBootloader = "";
  let connectTimer = null;
  let bootTimer = null;
  let shownFor = "";          // fingerprint already offered since it connected
  let shownBootloader = "";   // bootloader port already offered since it appeared
  let dialog = null;
  let latest = {};

  function modalOpen() {
    return !!document.querySelector(".modal-overlay");
  }

  function wizardOpen() {
    return !!(Corvus.setupFirstSetup && Corvus.setupFirstSetup.isOpen());
  }

  function clearTimers() {
    if (connectTimer) { window.clearTimeout(connectTimer); connectTimer = null; }
    if (bootTimer) { window.clearTimeout(bootTimer); bootTimer = null; }
  }

  function onState(state) {
    latest = state || {};
    const connected = !!latest.connected;
    const waiting = bootloaderDevice(latest);

    if (connected && !lastConnected) {
      if (connectTimer) window.clearTimeout(connectTimer);
      connectTimer = window.setTimeout(() => checkFresh(true), CONNECT_SETTLE_MS);
    } else if (!connected && lastConnected) {
      if (connectTimer) { window.clearTimeout(connectTimer); connectTimer = null; }
      shownFor = "";
    }
    lastConnected = connected;

    if (waiting && waiting !== lastBootloader) {
      if (bootTimer) window.clearTimeout(bootTimer);
      bootTimer = window.setTimeout(() => checkBootloader(waiting), BOOTLOADER_SETTLE_MS);
    } else if (!waiting) {
      if (bootTimer) { window.clearTimeout(bootTimer); bootTimer = null; }
      shownBootloader = "";
    }
    lastBootloader = waiting;
  }

  function checkFresh(mayRetry) {
    connectTimer = null;
    if (!lastConnected || latest.armed || wizardOpen()) return;
    if (modalOpen()) {
      connectTimer = window.setTimeout(() => checkFresh(mayRetry), MODAL_RETRY_MS);
      return;
    }
    Corvus.telemetry.requestJson("/api/first-setup").then((payload) => {
      if (!lastConnected) return;
      if (!payload || !payload.connected) {
        /* The parameters were not there yet. Once more, then give up quietly. */
        if (mayRetry) connectTimer = window.setTimeout(() => checkFresh(false), RETRY_MS);
        return;
      }
      const fp = String(payload.fingerprint || "");
      if (fp && fp === shownFor) return;
      if (!shouldPrompt(payload, isSilenced(fp))) return;
      shownFor = fp;
      offerFresh(payload);
    }).catch(() => { /* a failed read is no reason to interrupt anyone */ });
  }

  function checkBootloader(device) {
    bootTimer = null;
    if (bootloaderDevice(latest) !== device || latest.connected || wizardOpen()) return;
    if (device === shownBootloader) return;
    if (modalOpen()) {
      bootTimer = window.setTimeout(() => checkBootloader(device), MODAL_RETRY_MS);
      return;
    }
    /* A flash in progress reboots its own board into the bootloader. That is
       not a new controller, and the Firmware page is already open for it. */
    Corvus.telemetry.requestJson("/api/firmware/status").then((status) => {
      if (bootloaderDevice(latest) !== device) return;
      const state = status && status.state;
      if (state === "downloading" || state === "flashing") return;
      shownBootloader = device;
      offerBlank(device);
    }).catch(() => {});
  }

  /* ------------------------------- dialog ------------------------------ */

  function closeDialog() {
    if (dialog) { const d = dialog; dialog = null; d.close(); }
  }

  function offer(opts) {
    closeDialog();
    const body = document.createElement("div");
    body.className = "fs-offer";
    const lede = document.createElement("p");
    lede.className = "fs-offer-lede";
    lede.textContent = opts.lede;
    body.appendChild(lede);
    if (opts.reasons && opts.reasons.length) {
      const list = document.createElement("ul");
      list.className = "fs-offer-reasons";
      opts.reasons.forEach((r) => {
        const li = document.createElement("li");
        li.textContent = r;
        list.appendChild(li);
      });
      body.appendChild(list);
    }
    const note = document.createElement("p");
    note.className = "fs-offer-note";
    note.textContent = opts.note;
    body.appendChild(note);

    const actions = [
      Corvus.ui.button({ variant: "ghost", label: "Not now", onClick: closeDialog }),
    ];
    if (opts.fingerprint) {
      actions.push(Corvus.ui.button({
        variant: "secondary", label: "Do not ask again",
        onClick: () => { silence(opts.fingerprint); closeDialog(); },
      }));
    }
    actions.push(Corvus.ui.button({
      variant: "primary", icon: "wand-sparkles", label: "Start setup",
      onClick: () => { closeDialog(); open({ step: opts.step }); },
    }));
    dialog = Corvus.ui.modal({
      title: opts.title, body, actions,
      onClose: () => { dialog = null; },
    });
    dialog.open();
  }

  function offerFresh(payload) {
    offer({
      title: "New controller detected",
      lede: "This flight controller has not been set up yet.",
      reasons: freshReasons(payload),
      note: "The first setup takes you through the aircraft type, the frame and motors, "
        + "the sensor calibration and the safety settings, one page after the other.",
      fingerprint: String(payload.fingerprint || ""),
      step: "",
    });
  }

  function offerBlank(device) {
    offer({
      title: "Controller without firmware",
      lede: "A flight controller is waiting in its bootloader on " + device + ".",
      reasons: ["It has no firmware to talk to, so Corvus cannot connect to it yet."],
      note: "The first setup starts by flashing PX4 or ArduPilot onto it, then goes on "
        + "with the aircraft type and the calibration.",
      fingerprint: "",
      step: "firmware",
    });
  }

  /* ------------------------------- public ------------------------------ */

  /**
   * Open the wizard on the Setup page. *opts.step* names the step to start on;
   * left out, the wizard picks the first one that still needs doing.
   */
  function open(opts) {
    closeDialog();
    if (Corvus.sidenav && typeof Corvus.sidenav.switchTo === "function") {
      Corvus.sidenav.switchTo("setup");
    }
    if (Corvus.setup && typeof Corvus.setup.openView === "function") {
      Corvus.setup.openView("firstsetup", opts || {});
    }
  }

  function init() {
    if (started) return;
    started = true;
    if (!Corvus.telemetry || typeof Corvus.telemetry.subscribe !== "function") return;
    unsub = Corvus.telemetry.subscribe(onState);
    const now = typeof Corvus.telemetry.getState === "function" ? Corvus.telemetry.getState() : null;
    if (now) onState(now);
  }

  function destroy() {
    clearTimers();
    closeDialog();
    if (typeof unsub === "function") { try { unsub(); } catch (_e) { /* gone */ } }
    unsub = null;
    started = false;
    lastConnected = false;
    lastBootloader = "";
    shownFor = "";
    shownBootloader = "";
  }

  return {
    init, destroy, open, silence, isSilenced,
    shouldPrompt, bootloaderDevice, freshReasons,
  };
})();
