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
 * Above the calibrations sits the flight controller's rotation on the
 * airframe, because every accelerometer and compass calibration is measured
 * through it: set wrong, the wizard asks for "nose down" and the sensor sees
 * a side. Where the flight controller and the GPS *sit* is not here. Those
 * positions are lever arms the estimator uses in flight, no calibration reads
 * them, and they share their origin with the motors, so the Motors page draws
 * and edits them.
 *
 * Backend contract (verified against PX4 v1.16 / v1.17 / v1.18):
 *   GET  /api/mounting            {connected, orientation:{fields,hint}, positions}
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

  /** The session's cue as an icon beside the instruction: the operator's eyes
   *  are on the aircraft, and a symbol reads faster than a sentence. */
  const CUE_ICON = {
    wait: "hourglass",
    rotate: "rotate-3d",
    hold: "hand",
    spin: "refresh-cw",
    warn: "triangle-alert",
    battery_on: "plug-zap",
    battery_off: "unplug",
    blow: "wind",
    shield: "shield",
    done: "circle-check",
    failed: "circle-x",
    cancelled: "ban",
  };

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

    const mounting = buildMounting();
    el.appendChild(mounting.el);

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

      const unsupported = S.el("div", "calib-card-desc calib-card-unsupported", "");
      unsupported.hidden = true;
      body.appendChild(unsupported);

      card.addEventListener("click", () => { if (!card.disabled) openWizard(type); });
      cards.appendChild(card);
      return { type, card, unsupported };
    });
    sensorSection.appendChild(cards);
    el.appendChild(sensorSection);

    const armedBanner = S.el("div", "params-banner setup-armed-banner calib-list-gate");
    armedBanner.hidden = true;
    armedBanner.textContent = "Cannot calibrate while armed. Disarm first.";
    sensorSection.appendChild(armedBanner);

    /* A calibration the connected stack does not run is greyed out with the
       reason on the card, not hidden: an operator looking for "Motors / ESC"
       on an ArduPilot aircraft needs to be told it is done through a parameter
       there, not left to conclude Corvus lost the feature. */
    let dead = false;
    let supported = null;      // null = not answered yet, so nothing is gated
    function applySupport() {
      cardEls.forEach((entry) => {
        const ok = supported === null || supported.indexOf(entry.type) !== -1;
        entry.card.dataset.supported = ok ? "yes" : "no";
        entry.unsupported.hidden = ok;
        if (!ok) {
          entry.unsupported.textContent =
            "Not available on the connected autopilot.";
        }
      });
    }

    function refreshSupport() {
      if (!Corvus.capabilities) return;
      Corvus.capabilities.get().then((caps) => {
        if (dead) return;
        supported = (caps && Array.isArray(caps.calibrations) && caps.calibrations.length)
          ? caps.calibrations : null;
        applySupport();
        paintArmed(lastArmed);
      }).catch(() => {});
    }

    let lastArmed = false;
    function paintArmed(armed) {
      lastArmed = armed;
      cardEls.forEach((entry) => {
        const ok = supported === null || supported.indexOf(entry.type) !== -1;
        entry.card.disabled = armed || !ok;
      });
    }

    refreshSupport();
    let lastStack = Corvus.capabilities ? Corvus.capabilities.stack() : "";
    let lastConnected = null;

    return {
      el,
      onTelemetry(s) {
        const armed = !!s.armed;
        const connected = !!s.connected;
        linkChip.set(connected, connected ? "Link up" : "No link");
        armChip.set(!armed, armed ? "Armed" : "Disarmed");
        paintArmed(armed);
        armedBanner.hidden = !armed;
        mounting.setArmed(armed);
        // A different aircraft is a different feature set, and the capability
        // document is cached per stack — so a change here is the one event
        // that has to re-ask.
        const nextStack = (s && s.autopilot_stack) || "";
        if (nextStack !== lastStack) {
          lastStack = nextStack;
          refreshSupport();
          mounting.load();
        } else if (connected !== lastConnected && lastConnected !== null) {
          mounting.load();
        }
        lastConnected = connected;
      },
      destroy() { dead = true; mounting.destroy(); },
    };
  }

  /**
   * The flight controller's rotation on the airframe, from GET /api/mounting.
   *
   * Schema driven like every setup form: the fields name the parameter they
   * write (SENS_BOARD_ROT on PX4, AHRS_ORIENTATION on ArduPilot), so nothing
   * here knows which stack answered. A rotation is read at boot, so a change
   * offers the reboot, and says which calibrations it has invalidated.
   */
  function buildMounting() {
    const section = S.el("div", "page-section calib-mounting");
    section.appendChild(S.sectionTitle("Flight controller orientation"));
    const card = S.el("div", "page-card calib-mounting-card");
    section.appendChild(card);
    const state = { armed: false, controls: [], wanted: null, doc: null,
                    loading: false, destroyed: false };

    function load() {
      if (state.loading || state.destroyed) return Promise.resolve();
      if (!Corvus.telemetry || typeof Corvus.telemetry.requestJson !== "function") {
        paint();
        return Promise.resolve();
      }
      state.loading = true;
      return Corvus.telemetry.requestJson("/api/mounting").then((doc) => {
        if (state.destroyed) return;
        state.doc = doc || {};
        paint();
      }).catch(() => {
        if (state.destroyed) return;
        state.doc = {};
        paint();
      }).finally(() => { state.loading = false; });
    }

    function paint() {
      card.innerHTML = "";
      state.controls = [];
      const doc = state.doc || {};
      const orientation = doc.orientation;
      if (!orientation || !(orientation.fields || []).length) {
        card.appendChild(S.el("div", "field-hint", doc.connected
          ? "The connected firmware reports no board rotation."
          : "Connect to a vehicle to read how the flight controller is mounted."));
        return;
      }
      if (orientation.hint) card.appendChild(S.el("div", "field-hint", orientation.hint));

      const reboot = S.el("div", "calib-mounting-reboot");
      reboot.hidden = true;
      reboot.appendChild(S.el("span", "field-hint",
        "Reboot for the new rotation to take effect, then calibrate the accelerometer, "
        + "the compass and the level horizon again."));
      const rebootBtn = S.rebootButton({ size: "sm", mount: section,
        onRebooted: () => { reboot.hidden = true; } });
      reboot.appendChild(rebootBtn);

      card.appendChild(S.paramFieldGrid(state, orientation.fields, {
        prefix: "calib",
        onApplied: (field) => { if (field.reboot) reboot.hidden = false; },
      }));
      card.appendChild(reboot);
      S.registerControl(state, rebootBtn);

      card.appendChild(S.el("div", "field-hint calib-mounting-positions",
        "Where the flight controller and the GPS sit is set on the Motors page, on the "
        + "drawing of the airframe. The estimator uses those positions in flight; no "
        + "calibration reads them."));
      S.applyArmed(state, state.armed);
      S.refreshIcons();
    }

    load();
    return {
      el: section,
      load,
      setArmed(armed) { S.applyArmed(state, armed); },
      destroy() { state.destroyed = true; },
    };
  }

  /** One checklist line: an icon the eye finds first, and a few words. */
  function prepItem(entry, tag, cls) {
    const item = S.el(tag, cls);
    item.appendChild(S.icon(entry.icon));
    item.appendChild(S.el("span", null, entry.text));
    return item;
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
    // The title row also carries Start and Abort, top right, in the same
    // slot: only one of them is ever shown, and the way in or out is where
    // the eye goes first, not below the log.
    const head = S.el("div", "calib-head");
    head.appendChild(S.pageHeader(proc.label + " Calibration", proc.summary));
    el.appendChild(head);

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
    const instruction = S.el("div", "calib-instruction");
    const cueBox = S.el("div", "calib-cue");
    cueBox.setAttribute("aria-hidden", "true");
    instruction.appendChild(cueBox);
    const words = S.el("div", "calib-words");
    const headline = S.el("div", "calib-headline");
    headline.setAttribute("role", "status");
    headline.setAttribute("aria-live", "polite");
    words.appendChild(headline);
    const detail = S.el("div", "calib-detail");
    words.appendChild(detail);
    instruction.appendChild(words);
    text.appendChild(instruction);
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
    // Every position the autopilot will ask for, drawn, with its own state.
    // Before the
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
        const ok = S.icon("check");
        ok.classList.add("calib-pose-ok");
        mark.appendChild(ok);
        const bad = S.icon("x");
        bad.classList.add("calib-pose-bad");
        mark.appendChild(bad);
        chip.appendChild(mark);
        chip.addEventListener("click", () => {
          if (session.getState().phase !== "idle") return;
          preview = pose;
          paint();
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
    proc.prep.forEach((entry) => prepList.appendChild(prepItem(entry, "li", "calib-prep-item")));
    prep.appendChild(prepList);
    if (proc.danger) prep.classList.add("calib-prep-danger");
    el.appendChild(prep);

    // --- live autopilot transcript ----------------------------------------
    // Folded away by default: the stage above already says what the
    // autopilot wants, and a scrolling log beside it is text the operator
    // does not need to read. It opens by itself when a calibration fails,
    // where the autopilot's own words are the explanation.
    const logCard = document.createElement("details");
    logCard.className = "page-card calib-log-card";
    const logTitle = document.createElement("summary");
    logTitle.className = "guidance-title calib-log-summary";
    logTitle.appendChild(S.icon("info"));
    logTitle.appendChild(S.el("span", null, "Autopilot messages"));
    const logCountEl = S.el("span", "calib-log-count", "");
    logTitle.appendChild(logCountEl);
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
    /* The step PX4 does not have. ArduPilot prints "Place vehicle level and
       press any key." and then waits for MAV_CMD_ACCELCAL_VEHICLE_POS naming
       that position — forever, if nobody sends it. This is that key. It is
       shown only while the session is actually holding a prompt, so on a PX4
       link it never appears. */
    const confirmBtn = Corvus.ui.button({
      variant: "primary", icon: "check", label: "In position, continue",
      onClick: onConfirmPosition,
    });
    const abortBtn = Corvus.ui.button({
      variant: "danger", icon: "octagon-x", label: "Abort calibration", onClick: onAbort,
    });
    head.appendChild(startBtn);
    head.appendChild(abortBtn);
    const retryBtn = Corvus.ui.button({
      variant: "secondary", icon: "rotate-cw", label: "Try again", onClick: onRetry,
    });
    const doneBtn = Corvus.ui.button({
      variant: "secondary", icon: "chevron-left", label: "Back to calibrations",
      onClick: () => navigateBack(),
    });
    // The accelerometer and compass results are read at boot; the wizard that
    // says so offers the reboot rather than sending the operator to find one.
    const rebootBtn = S.rebootButton({ mount: el });
    const actions = Corvus.ui.actions(
      [confirmBtn, retryBtn, rebootBtn, doneBtn]);
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
    let shownCue = null;
    // The position picked on the strip before the start. Held here rather
    // than set on the figure directly, because every telemetry repaint would
    // otherwise put the figure straight back on the start pose.
    let preview = null;

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
      detail.hidden = !st.detail;
      paintCue(st.cue || "idle");
      if (st.phase !== "idle") preview = null;
      figure.set({
        pose: preview || st.pose, spin: st.spin, marker: st.marker,
        from: st.from, motion: st.cue === "rotate",
      });

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
      confirmBtn.hidden = !(running && st.confirm);
      confirmBtn.disabled = busy;
      if (st.confirm) {
        confirmBtn.textContent = "In position, continue";
      }
      abortBtn.hidden = !running;
      abortBtn.disabled = busy;
      retryBtn.hidden = st.phase !== "failed" && st.phase !== "cancelled";
      retryBtn.disabled = busy || blocked;
      doneBtn.hidden = !terminal;
      rebootBtn.hidden = !(st.phase === "done" && proc.reboot);
      rebootBtn.disabled = busy || blocked;

      banner.hidden = !(vehicle.armed || !vehicle.connected) || running;
      banner.textContent = vehicle.armed
        ? "Cannot calibrate while armed. Disarm first."
        : "No link to the vehicle. Connect before calibrating.";
      if (st.phase === "failed" && el.dataset.phase !== "failed") logCard.open = true;
      el.dataset.phase = st.phase;
    }

    /** Swap the cue icon only when the cue changes: a progress line arrives
     *  every second or so, and each icon refresh walks the document. */
    function paintCue(cue) {
      if (cue === shownCue) return;
      shownCue = cue;
      cueBox.dataset.cue = cue;
      Corvus.ui.clear(cueBox);
      cueBox.appendChild(S.icon(CUE_ICON[cue] || proc.icon));
      S.refreshIcons();
    }

    function appendLog(entryText, level) {
      if (logCount === 0) Corvus.ui.clear(guidanceList);
      const line = S.el("div", "guidance-line" + (level ? " " + level : ""));
      line.appendChild(S.el("span", "guidance-level", level || "info"));
      line.appendChild(S.el("span", "guidance-msg", entryText));
      guidanceList.appendChild(line);
      logCount += 1;
      logCountEl.textContent = String(logCount);
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
        watchdog.textContent = "No reply from the autopilot yet. Check the link, or abort.";
      } else if (st.confirm) {
        // Not a stall: the autopilot is waiting for the operator, and saying
        // "no word for 40 s" here would blame the vehicle for the pause it
        // asked for.
        watchdog.hidden = true;
      } else if (st.seenVehicleMessage && quiet > STALL_TIMEOUT_MS) {
        watchdog.hidden = false;
        watchdog.textContent = "Autopilot silent for " + Math.round(quiet / 1000)
          + " s. It may want the next position.";
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
      clearLog("Waiting for the autopilot…");
      paint();
      try {
        await Corvus.telemetry.postAction("/api/calibrate", { type: proc.type });
        // Restarts the start timer only if the autopilot is still silent: its
        // first lines may have beaten the ACK here, and the whole calibration
        // may already be over.
        session.begin(now());
        if (!isTerminal(session.getState().phase)) startWatchdog();
      } catch (err) {
        const message = (err && err.message) || "The vehicle rejected the command.";
        const phase = session.getState().phase;
        if (phase === "running") {
          // The vehicle is already narrating a calibration, so the command
          // took even though its ACK did not reach us. Its own lines decide
          // how this ends.
          appendLog(message, "warning");
          startWatchdog();
        } else if (!isTerminal(phase)) {
          session.finish("failed", "Could not start the calibration", message);
          notify("critical", (err && err.message) || "Calibration could not be started");
        }
      } finally {
        busy = false;
        paint();
      }
    }

    async function onConfirmPosition() {
      const pending = session.getState().confirm;
      if (!pending || busy) return;
      busy = true;
      paint();
      try {
        await Corvus.telemetry.postAction(
          "/api/calibrate/position", { position: pending.position });
        // Advance the session only once the vehicle has taken the answer.
        // Moving the figure on before that would show the operator the next
        // position while the autopilot was still asking for this one.
        session.confirmPlacement();
        appendLog("Confirmed: " + F.poseLabel(pending.pose), "info");
      } catch (err) {
        const message = (err && err.message) || "The vehicle did not take the position";
        appendLog(message, "critical");
        notify("warning", message);
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
      clearLog("Nothing from the autopilot yet.");
      logCard.open = false;
      paint();
    }

    function clearLog(placeholder) {
      logCount = 0;
      logCountEl.textContent = "";
      Corvus.ui.clear(guidanceList);
      guidanceList.appendChild(S.el("div", "guidance-empty", placeholder));
    }

    function openSafetyModal() {
      const body = S.el("div", "motor-calib-warning");
      proc.prep.forEach((entry) => body.appendChild(
        prepItem(entry, "div", "motor-calib-warning-line")));

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
      S.refreshIcons();
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
