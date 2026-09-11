"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupCalibration — the Calibration sub-page of the Setup page.
 *
 * Two views behind one entry point:
 *
 *   list    the seven calibrations as cards (what each one is for, how long it
 *           takes, how many positions it needs).
 *   wizard  one calibration, guided: the aircraft drawn in the attitude PX4 is
 *           asking for, the six positions tracked as they complete, a live
 *           progress bar, the PX4 transcript, and an abort that actually stops
 *           the calibration on the vehicle.
 *
 * The wizard exists because the previous screen was a grid of buttons and a log:
 * it fired MAV_CMD_PREFLIGHT_CALIBRATION and left the operator to translate
 * "[cal] Rotate to a pending side: back" into a physical action. That is where
 * field calibrations fail. The instruction is now a picture of the aircraft in
 * the required attitude (Corvus.calibFigures), driven by a parser for PX4's own
 * guidance (Corvus.calibProtocol).
 *
 * Backend contract (verified against PX4 v1.16 / v1.17 / v1.18):
 *   POST /api/calibrate {type}    start a sensor calibration (refused while armed)
 *   POST /api/calibrate/cancel    abort the running calibration
 *   GET  /api/console/stream      ordered STATUSTEXT, via Corvus.telemetry.subscribeConsole
 *
 * The wizard reads STATUSTEXT from the console stream, not from the telemetry
 * `warnings` array: the state store de-duplicates warnings by message text, so
 * "[cal] Hold still, measuring down side" appearing a second time refreshes a
 * row in place instead of arriving as a new event. Ordered, repeated guidance is
 * exactly what a calibration is made of.
 *
 * PID tuning is NOT here. It used to be a band at the bottom of this page —
 * one autotune button and three read-only graphs — which is how a calibration
 * screen ended up owning the control loops. It now has its own sub-page
 * (setup-tuning.js): the autotune runs in flight, every controller is editable
 * by hand, and the graphs draw the setpoint beside the response. A calibration
 * page and a tuning page share nothing but the vehicle.
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the view lifecycle: it calls destroy() on back / left-nav re-entry so no
 * telemetry subscription, console subscription, animation frame, Plotly graph or
 * buffer leaks.
 */
Corvus.setupCalibration = (function () {
  const S = Corvus.setupShared;
  const P = Corvus.calibProtocol;
  const F = Corvus.calibFigures;

  /** No PX4 word within this long of a successful start means something is
   *  wrong with the link, not with the operator. */
  const START_TIMEOUT_MS = 8000;
  /** PX4 going quiet mid-calibration usually means it is waiting for a position
   *  the operator has not realised it wants. */
  const STALL_TIMEOUT_MS = 45000;
  const TRANSCRIPT_MAX = 60;

  function now() { return Date.now(); }

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page");
    container.appendChild(page);

    // Last telemetry snapshot, shared by both views; the subscription itself is
    // owned here so switching views never re-subscribes. Views receive the whole
    // snapshot, not a digest of it — re-reading getState() instead would
    // silently drop the pushed values.
    let snapshot = { armed: false, connected: false };
    let active = null;          // { el, destroy, onTelemetry } of the mounted view

    function mount(view) {
      if (active) {
        try { active.destroy(); } catch (err) { console.error("calibration view teardown:", err); }
        if (active.el && active.el.parentNode) active.el.parentNode.removeChild(active.el);
      }
      active = view;
      page.appendChild(view.el);
      if (typeof view.onTelemetry === "function") view.onTelemetry(snapshot);
      S.refreshIcons();
    }

    const openWizard = (type) => mount(buildWizard(type, () => mount(buildList(openWizard, navigateBack))));
    mount(buildList(openWizard, navigateBack));

    // --- shared telemetry subscription -----------------------------------
    let unsub = null;
    function onTelemetry(s) {
      if (!s) return;
      snapshot = s;
      if (active && typeof active.onTelemetry === "function") active.onTelemetry(s);
    }
    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      unsub = Corvus.telemetry.subscribe(onTelemetry);
      const cur = Corvus.telemetry.getState();
      if (cur) onTelemetry(cur);
    }

    /** Teardown returned to setup.js. Idempotent and guarded. */
    return function destroy() {
      if (unsub) { try { unsub(); } catch (_e) {} unsub = null; }
      if (active) {
        try { active.destroy(); } catch (err) { console.error("calibration teardown:", err); }
        active = null;
      }
    };
  }

  /* ================================================================== */
  /* List view                                                           */
  /* ================================================================== */

  function buildList(openWizard, navigateBack) {
    const el = S.el("div", "calib-list-view");
    el.appendChild(S.backButton(navigateBack));
    el.appendChild(S.pageHeader("Calibration", "Guided sensor calibration"));

    // Readiness strip: the two preconditions every calibration shares, stated
    // before the operator picks one rather than as a rejection afterwards.
    const ready = S.el("div", "calib-ready");
    const linkChip = readyChip("link", "Link");
    const armChip = readyChip("armed", "Disarmed");
    ready.appendChild(linkChip.el);
    ready.appendChild(armChip.el);
    el.appendChild(ready);

    const sensorSection = S.el("div", "page-section");
    sensorSection.appendChild(S.sectionTitle("Sensor Calibration"));
    const cards = S.el("div", "calib-cards");
    const cardEls = P.ORDER.map((type) => {
      const proc = P.PROCEDURES[type];
      const card = document.createElement("button");
      card.type = "button";
      card.className = "calib-card" + (proc.danger ? " calib-card-danger" : "");
      card.dataset.type = type;

      const iconBox = S.el("div", "calib-card-icon");
      iconBox.appendChild(S.icon(proc.icon));
      card.appendChild(iconBox);

      const body = S.el("div", "calib-card-body");
      const head = S.el("div", "calib-card-head");
      head.appendChild(S.el("span", "calib-card-title", proc.label));
      if (proc.danger) head.appendChild(S.el("span", "calib-card-flag", "Props off"));
      body.appendChild(head);
      body.appendChild(S.el("div", "calib-card-desc", proc.summary));
      const meta = S.el("div", "calib-card-meta");
      meta.appendChild(S.el("span", "calib-card-meta-item",
        proc.poses.length ? proc.poses.length + " positions" : "Stays still"));
      meta.appendChild(S.el("span", "calib-card-meta-item", proc.duration));
      if (proc.reboot) meta.appendChild(S.el("span", "calib-card-meta-item", "Reboot after"));
      body.appendChild(meta);
      card.appendChild(body);
      card.appendChild(S.icon("chevron-right"));

      card.addEventListener("click", () => { if (!card.disabled) openWizard(type); });
      cards.appendChild(card);
      return card;
    });
    sensorSection.appendChild(cards);
    el.appendChild(sensorSection);

    const armedBanner = S.el("div", "params-banner setup-armed-banner");
    armedBanner.hidden = true;
    armedBanner.textContent = "Cannot calibrate while armed — disarm first.";
    sensorSection.appendChild(armedBanner);

    return {
      el,
      onTelemetry(s) {
        const armed = !!s.armed;
        const connected = !!s.connected;
        linkChip.set(connected, connected ? "Link up" : "No link");
        armChip.set(!armed, armed ? "Armed" : "Disarmed");
        cardEls.forEach((c) => { c.disabled = armed; });
        armedBanner.hidden = !armed;
      },
      destroy() {},
    };
  }

  function readyChip(key, label) {
    const el = S.el("div", "calib-ready-chip");
    el.dataset.key = key;
    const dot = S.el("span", "calib-ready-dot");
    const text = S.el("span", "calib-ready-label", label);
    el.appendChild(dot);
    el.appendChild(text);
    return {
      el,
      set(ok, next) {
        el.dataset.state = ok ? "ok" : "bad";
        if (next !== undefined) text.textContent = next;
      },
    };
  }

  /* ================================================================== */
  /* Wizard view                                                         */
  /* ================================================================== */

  function buildWizard(type, navigateBack) {
    const session = P.createSession(type);
    const proc = session.procedure;
    const reduced = S.reducedMotion();

    const el = S.el("div", "calib-wizard-view");
    el.appendChild(Corvus.ui.button({
      variant: "ghost", size: "sm", className: "setup-back",
      icon: "chevron-left", label: "Calibration",
      ariaLabel: "Back to the calibration list", onClick: () => navigateBack(),
    }));
    el.appendChild(S.pageHeader(proc.label + " Calibration", proc.summary));

    const banner = S.el("div", "params-banner setup-armed-banner");
    banner.hidden = true;
    el.appendChild(banner);

    // --- stage: the figure and the instruction it illustrates ------------
    const stage = S.el("div", "page-card calib-stage");
    const figureHost = S.el("div", "calib-stage-figure");
    stage.appendChild(figureHost);
    const figure = F.create(figureHost, {
      pose: proc.startPose, spin: false, marker: proc.marker || null, reduced,
    });

    const text = S.el("div", "calib-stage-text");
    const phaseChip = S.el("div", "calib-phase");
    phaseChip.dataset.phase = "idle";
    text.appendChild(phaseChip);
    const headline = S.el("div", "calib-headline");
    headline.setAttribute("role", "status");
    headline.setAttribute("aria-live", "polite");
    text.appendChild(headline);
    const detail = S.el("div", "calib-detail");
    text.appendChild(detail);
    const bar = S.el("div", "calib-progress");
    const barFill = S.el("div", "calib-progress-fill");
    bar.appendChild(barFill);
    bar.hidden = true;
    text.appendChild(bar);
    const watchdog = S.el("div", "calib-watchdog");
    watchdog.hidden = true;
    text.appendChild(watchdog);
    stage.appendChild(text);
    el.appendChild(stage);

    // --- position strip ---------------------------------------------------
    // Every position PX4 will ask for, drawn, with its own state. Before the
    // start it doubles as a preview: clicking one shows it on the big figure,
    // so the whole sequence can be studied before the aircraft is picked up.
    const poseFigures = [];
    let poseStrip = null;
    if (proc.poses.length) {
      poseStrip = S.el("div", "calib-poses");
      proc.poses.forEach((pose) => {
        const chip = document.createElement("button");
        chip.type = "button";
        chip.className = "calib-pose";
        chip.dataset.pose = pose;
        chip.dataset.state = "pending";
        const host = S.el("div", "calib-pose-figure");
        chip.appendChild(host);
        chip.appendChild(S.el("span", "calib-pose-label", F.poseLabel(pose)));
        const mark = S.el("span", "calib-pose-mark");
        chip.appendChild(mark);
        chip.addEventListener("click", () => {
          if (session.getState().phase === "idle") figure.set({ pose, spin: false });
        });
        poseStrip.appendChild(chip);
        poseFigures.push({ pose, chip, fig: F.create(host, { compact: true, pose, reduced }) });
      });
      el.appendChild(poseStrip);
    }

    // --- preparation checklist (idle only) --------------------------------
    const prep = S.el("div", "page-card calib-prep");
    const prepTitle = S.el("div", "guidance-title");
    prepTitle.appendChild(S.icon(proc.danger ? "triangle-alert" : "list-checks"));
    prepTitle.appendChild(S.el("span", null, "Before you start"));
    prep.appendChild(prepTitle);
    const prepList = S.el("ul", "calib-prep-list");
    proc.prep.forEach((line) => {
      const item = document.createElement("li");
      item.className = "calib-prep-item";
      item.textContent = line;
      prepList.appendChild(item);
    });
    prep.appendChild(prepList);
    if (proc.danger) prep.classList.add("calib-prep-danger");
    el.appendChild(prep);

    // --- live PX4 transcript ---------------------------------------------
    const logCard = S.el("div", "page-card calib-log-card");
    const logTitle = S.el("div", "guidance-title");
    logTitle.appendChild(S.icon("info"));
    logTitle.appendChild(S.el("span", null, "Autopilot messages (live STATUSTEXT)"));
    logCard.appendChild(logTitle);
    const guidanceList = S.el("div", "guidance-list");
    guidanceList.setAttribute("role", "log");
    guidanceList.setAttribute("aria-live", "polite");
    guidanceList.appendChild(S.el("div", "guidance-empty", "Nothing from the autopilot yet."));
    logCard.appendChild(guidanceList);
    el.appendChild(logCard);

    // --- actions ----------------------------------------------------------
    const startBtn = Corvus.ui.button({
      variant: proc.danger ? "danger" : "primary",
      icon: "play", label: "Start " + proc.label.toLowerCase() + " calibration",
      onClick: onStart,
    });
    const abortBtn = Corvus.ui.button({
      variant: "danger", icon: "octagon-x", label: "Abort calibration", onClick: onAbort,
    });
    const retryBtn = Corvus.ui.button({
      variant: "secondary", icon: "rotate-cw", label: "Try again", onClick: onRetry,
    });
    const doneBtn = Corvus.ui.button({
      variant: "secondary", icon: "chevron-left", label: "Back to calibrations",
      onClick: () => navigateBack(),
    });
    const actions = Corvus.ui.actions([startBtn, abortBtn, retryBtn, doneBtn]);
    actions.classList.add("calib-actions");
    el.appendChild(actions);

    /* Modal gate for the one calibration that spins motors. Mounted on `el`
       (not document.body) so tearing the view down takes the dialog with it —
       position:fixed covers the viewport regardless of parent. */
    let modal = null;
    function closeModal() {
      if (!modal) return;
      modal.close();
      modal = null;
    }

    let vehicle = { armed: false, connected: false };
    let busy = false;
    let logCount = 0;
    let watchdogTimer = null;

    /* ---------------- rendering ---------------- */

    const PHASE_TEXT = {
      idle: "Ready", starting: "Starting", running: "In progress",
      done: "Complete", failed: "Failed", cancelled: "Cancelled",
    };

    function paint() {
      const st = session.getState();
      const running = st.phase === "starting" || st.phase === "running";
      const terminal = st.phase === "done" || st.phase === "failed" || st.phase === "cancelled";

      phaseChip.dataset.phase = st.phase;
      phaseChip.textContent = PHASE_TEXT[st.phase] || st.phase;
      headline.textContent = st.headline;
      detail.textContent = st.detail;
      figure.set({ pose: st.pose, spin: st.spin, marker: st.marker });

      bar.hidden = st.progress == null;
      if (st.progress != null) {
        barFill.style.width = Math.max(0, Math.min(100, st.progress)) + "%";
        bar.dataset.phase = st.phase;
      }

      poseFigures.forEach((p) => {
        const state = st.sides[p.pose] || "pending";
        p.chip.dataset.state = state;
        p.chip.disabled = running && state === "done";
      });

      prep.hidden = st.phase !== "idle";
      if (poseStrip) poseStrip.hidden = false;

      const blocked = vehicle.armed || !vehicle.connected;
      startBtn.hidden = running || terminal;
      startBtn.disabled = busy || blocked;
      abortBtn.hidden = !running;
      abortBtn.disabled = busy;
      retryBtn.hidden = st.phase !== "failed" && st.phase !== "cancelled";
      retryBtn.disabled = busy || blocked;
      doneBtn.hidden = !terminal;

      banner.hidden = !(vehicle.armed || !vehicle.connected) || running;
      banner.textContent = vehicle.armed
        ? "Cannot calibrate while armed — disarm first."
        : "No link to the vehicle — connect before calibrating.";
      el.dataset.phase = st.phase;
    }

    function appendLog(entryText, level) {
      if (logCount === 0) Corvus.ui.clear(guidanceList);
      const line = S.el("div", "guidance-line" + (level ? " " + level : ""));
      line.appendChild(S.el("span", "guidance-level", level || "info"));
      line.appendChild(S.el("span", "guidance-msg", entryText));
      guidanceList.appendChild(line);
      logCount += 1;
      // Cap the transcript so a long calibration cannot grow the DOM without
      // bound; the tail is the part that matters anyway.
      while (guidanceList.children.length > TRANSCRIPT_MAX && guidanceList.firstChild) {
        guidanceList.removeChild(guidanceList.firstChild);
      }
      guidanceList.scrollTop = guidanceList.scrollHeight;
    }

    /* ---------------- watchdog ---------------- */

    function checkWatchdog() {
      const st = session.getState();
      if (st.phase !== "starting" && st.phase !== "running") {
        watchdog.hidden = true;
        return;
      }
      const quiet = now() - st.lastEventAt;
      if (!st.seenVehicleMessage && quiet > START_TIMEOUT_MS) {
        watchdog.hidden = false;
        watchdog.textContent = "The autopilot accepted the command but has not reported "
          + "anything yet. Check the link, or abort and try again.";
      } else if (st.seenVehicleMessage && quiet > STALL_TIMEOUT_MS) {
        watchdog.hidden = false;
        watchdog.textContent = "No word from the autopilot for "
          + Math.round(quiet / 1000) + " s. It is probably still waiting for a "
          + "position — or abort and start over.";
      } else {
        watchdog.hidden = true;
      }
    }

    function startWatchdog() {
      stopWatchdog();
      if (typeof window === "undefined" || typeof window.setInterval !== "function") return;
      watchdogTimer = window.setInterval(checkWatchdog, 1000);
    }
    function stopWatchdog() {
      if (watchdogTimer && typeof window.clearInterval === "function") {
        window.clearInterval(watchdogTimer);
      }
      watchdogTimer = null;
      watchdog.hidden = true;
    }

    /* ---------------- console stream ---------------- */

    let unsubConsole = null;
    if (Corvus.telemetry && typeof Corvus.telemetry.subscribeConsole === "function") {
      unsubConsole = Corvus.telemetry.subscribeConsole((entry) => {
        if (!entry) return;
        const name = entry.name || "";
        if (name !== "STATUSTEXT" && name !== "CALIBRATE") return;
        const body = entry.text || "";
        const wasTerminal = isTerminal(session.getState().phase);
        const changed = session.ingest(body, now());
        if (changed || /\[cal\]/i.test(body)) appendLog(body, entry.level || "info");
        if (changed) {
          paint();
          const st = session.getState();
          if (!wasTerminal && isTerminal(st.phase)) onTerminal(st);
        }
      });
    }

    function isTerminal(phase) {
      return phase === "done" || phase === "failed" || phase === "cancelled";
    }

    function onTerminal(st) {
      stopWatchdog();
      busy = false;
      notify(st.phase === "done" ? "info" : "warning",
        proc.label + " calibration " + (st.phase === "done" ? "complete" : st.phase));
      paint();
    }

    function notify(level, message) {
      window.dispatchEvent(new CustomEvent("corvus:notification", { detail: { level, message } }));
    }

    /* ---------------- actions ---------------- */

    function onStart() {
      if (busy) return;
      if (proc.danger && !modal) { openSafetyModal(); return; }
      launch();
    }

    async function launch() {
      busy = true;
      session.reset();
      session.begin(now());
      logCount = 0;
      Corvus.ui.clear(guidanceList);
      guidanceList.appendChild(S.el("div", "guidance-empty", "Waiting for the autopilot…"));
      paint();
      try {
        await Corvus.telemetry.postAction("/api/calibrate", { type: proc.type });
        session.begin(now());
        startWatchdog();
      } catch (err) {
        session.finish("failed", "Could not start the calibration",
          (err && err.message) || "The vehicle rejected the command.");
        notify("critical", (err && err.message) || "Calibration could not be started");
      } finally {
        busy = false;
        paint();
      }
    }

    async function onAbort() {
      if (busy) return;
      busy = true;
      paint();
      try {
        await Corvus.telemetry.postAction("/api/calibrate/cancel", {});
        session.finish("cancelled", "Calibration cancelled", "The vehicle stopped the calibration.");
      } catch (err) {
        // The abort itself failing is worth saying out loud: the vehicle may
        // still be mid-calibration, and pretending otherwise is worse.
        session.finish("failed", "Abort failed",
          (err && err.message) || "The vehicle did not confirm the abort.");
        notify("critical", (err && err.message) || "Could not abort the calibration");
      } finally {
        busy = false;
        stopWatchdog();
        paint();
      }
    }

    function onRetry() {
      session.reset();
      logCount = 0;
      Corvus.ui.clear(guidanceList);
      guidanceList.appendChild(S.el("div", "guidance-empty", "Nothing from the autopilot yet."));
      paint();
    }

    function openSafetyModal() {
      const body = S.el("div", "motor-calib-warning");
      proc.prep.forEach((line) => body.appendChild(S.el("div", "motor-calib-warning-line", line)));
      body.appendChild(S.el("div", "motor-calib-warning-line",
        "The ESCs are powered by re-plugging the battery AFTER you press Calibrate, "
        + "when PX4 instructs you to."));

      const cancel = Corvus.ui.button({ variant: "secondary", label: "Cancel", onClick: closeModal });
      const confirm = Corvus.ui.button({
        variant: "danger", label: "Calibrate Motors",
        onClick: () => { closeModal(); launch(); },
      });
      modal = Corvus.ui.modal({
        title: "Motor / ESC Calibration",
        size: "sm", body, actions: [cancel, confirm],
        mount: el, onClose: () => { modal = null; },
      });
      modal.open();
    }

    paint();

    return {
      el,
      onTelemetry(s) {
        vehicle = { armed: !!s.armed, connected: !!s.connected };
        // An arm event mid-calibration is PX4's problem, not ours; the banner
        // and the disabled start button are the whole gate here.
        paint();
      },
      destroy() {
        closeModal();
        stopWatchdog();
        if (unsubConsole) { try { unsubConsole(); } catch (_e) {} unsubConsole = null; }
        figure.destroy();
        poseFigures.forEach((p) => { try { p.fig.destroy(); } catch (_e) {} });
        poseFigures.length = 0;
      },
    };
  }

  return { render };
})();
