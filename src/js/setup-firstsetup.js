"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupFirstSetup: the First Setup sub-page of the Setup page.
 *
 * A guided tour through the pages that already exist, in the order a new
 * controller needs them. Nothing is reimplemented: each step hosts the real
 * page (Firmware, Motors, Calibration, Radio Control, Safety, Battery) in the
 * frame below a short explanation, and the page's own back button is hidden
 * because the wizard owns navigation. The one step with no page of its own,
 * choosing what kind of aircraft this is, is built here on top of
 * POST /api/first-setup/airframe.
 *
 *   firmware     flash PX4 or ArduPilot (a blank board is flashed from its
 *                bootloader, see corvus/flash_service.py)
 *   aircraft     the airframe preset (PX4) or frame class and type (ArduPilot)
 *   frame        motor order and spin, where the controller and GPS sit
 *   calibration  controller rotation, then gyro, accelerometer, compass, level
 *   radio        transmitter, sticks, flight mode switch
 *   safety       failsafes and arming checks
 *   battery      cells, capacity, power module
 *   finish       what is done, what is open
 *
 * What a step shows as done comes from the vehicle where the vehicle knows
 * (airframe, calibrations: GET /api/first-setup) and from the operator having
 * been through it where it does not. Steps can be taken in any order; the
 * stepper is a map, not a gate, because a controller that is half set up by
 * another ground station needs the second half only.
 *
 * Backend contract:
 *   GET  /api/first-setup            {connected, stack, vehicle, transport,
 *                                     facts:{airframe,accel,gyro,mag},
 *                                     reasons, needs_setup, prompt, presets,
 *                                     preset_current, fingerprint}
 *   POST /api/first-setup/airframe   {preset} -> {ok, written, reboot}
 *   POST /api/mavlink/reboot         via Corvus.setupShared.rebootButton
 *
 * Exposes render(container, navigateBack, opts) -> destroy(). The caller
 * (setup.js) owns the view lifecycle. destroy() tears the hosted page down and
 * releases the telemetry subscription.
 */
Corvus.setupFirstSetup = (function () {
  const S = Corvus.setupShared;

  const STEPS = [
    { id: "firmware", label: "Firmware", page: "setupFirmware",
      title: "Install the firmware",
      text: "A new controller usually has no firmware, or not the one you want. Pick PX4 or "
        + "ArduPilot below and flash it over the USB cable. If it already runs the firmware you "
        + "want, continue." },
    { id: "aircraft", label: "Aircraft", custom: true,
      title: "Choose the aircraft type",
      text: "Tell the controller what it is flying. This decides which controllers run and how "
        + "many motors it expects." },
    { id: "frame", label: "Frame and motors", page: "setupMotors",
      title: "Frame, motors and positions",
      text: "Check the motor order and spin direction, and set where the flight controller and "
        + "the GPS sit on the drawing of the aircraft. Test the motors with the propellers "
        + "removed." },
    { id: "calibration", label: "Calibration", page: "setupCalibration",
      title: "Orientation and calibration",
      text: "First set how the controller is turned on the aircraft. Then calibrate the gyro, "
        + "the accelerometer and the compass, and level the horizon. A changed rotation needs "
        + "a reboot and a new calibration." },
    { id: "radio", label: "Radio", page: "setupControl",
      title: "Radio control",
      text: "Bind the transmitter, calibrate the sticks and assign the flight mode switch. "
        + "Skip this if you fly without a transmitter." },
    { id: "safety", label: "Safety", page: "setupSafety",
      title: "Safety and failsafes",
      text: "Set what the aircraft does when the radio or the battery fails, and which checks "
        + "must pass before it arms." },
    { id: "battery", label: "Battery", page: "setupBattery",
      title: "Battery and power",
      text: "Enter the cell count and the capacity so the remaining charge is honest." },
    { id: "finish", label: "Finish", custom: true,
      title: "Ready to fly?",
      text: "Here is what is done and what is still open." },
  ];

  const PAGE_FOR = {
    setupFirmware: () => Corvus.setupFirmware,
    setupMotors: () => Corvus.setupMotors,
    setupCalibration: () => Corvus.setupCalibration,
    setupControl: () => Corvus.setupControl,
    setupSafety: () => Corvus.setupSafety,
    setupBattery: () => Corvus.setupBattery,
  };

  /* What the operator has been through survives leaving the page and coming
     back within the session, so a trip to Parameters in the middle of the
     setup costs nothing. It is not persisted past a reload: a fresh start is
     a fresh look at the controller. */
  const memory = { visited: {}, current: "", open: false };

  /* ------------------------------- pure -------------------------------- */

  const SENSOR_OK = { done: true, not_required: true };

  /** One word for the three sensor facts. Pure. */
  function calibrationState(facts) {
    const f = facts || {};
    const list = [f.accel, f.gyro, f.mag].filter(Boolean);
    if (list.some((v) => v === "todo")) return "todo";
    if (list.length && list.every((v) => SENSOR_OK[v])) return "done";
    return "unknown";
  }

  /**
   * The state of every step: "done", "todo" or "pending". *payload* is the
   * GET /api/first-setup answer (or null), *visited* the steps the operator
   * has been through, *bootloader* whether a board waits in its bootloader.
   * Pure.
   */
  function stepStates(payload, visited, bootloader) {
    const p = payload || {};
    const seen = visited || {};
    const facts = p.facts || {};
    const out = {};
    STEPS.forEach((s) => { out[s.id] = seen[s.id] ? "done" : "pending"; });
    out.firmware = (!p.connected || bootloader) ? "todo" : "done";
    if (facts.airframe === "done") out.aircraft = "done";
    else if (facts.airframe === "todo") out.aircraft = "todo";
    const cal = calibrationState(facts);
    if (cal === "done") out.calibration = "done";
    else if (cal === "todo") out.calibration = "todo";
    out.finish = "pending";
    return out;
  }

  /** The step to start on when nobody named one. Pure. */
  function initialStep(payload, bootloader) {
    const p = payload || {};
    if (bootloader || !p.connected) return "firmware";
    const states = stepStates(p, {}, false);
    if (states.aircraft === "todo") return "aircraft";
    if (states.calibration === "todo") return "calibration";
    return "aircraft";
  }

  function indexOf(id) {
    const i = STEPS.findIndex((s) => s.id === id);
    return i < 0 ? 0 : i;
  }

  /** What was written, in words the operator recognises. Pure. */
  function presetLabel(payload, id) {
    const list = (payload && payload.presets) || [];
    const hit = list.find((p) => p.id === id);
    return hit ? hit.label : "";
  }

  /* ------------------------------ the page ------------------------------ */

  function render(container, navigateBack, opts) {
    const options = opts || {};
    memory.open = true;
    const page = S.el("div", "setup-page fs-page");
    container.appendChild(page);

    let payload = null;
    let snapshot = (Corvus.telemetry && Corvus.telemetry.getState && Corvus.telemetry.getState()) || {};
    let current = STEPS[indexOf(options.step || memory.current || "")].id;
    let hosted = null;          // { el, destroy } of the step being shown
    let destroyed = false;
    let unsub = null;

    page.appendChild(S.backButton(navigateBack));
    page.appendChild(S.pageHeader("First Setup",
      "A new controller, set up step by step"));

    const stepList = S.el("ol", "fs-steps");
    stepList.setAttribute("aria-label", "Setup steps");
    page.appendChild(stepList);

    const bar = S.el("div", "page-card fs-bar");
    const barText = S.el("div", "fs-bar-text");
    const barTitle = S.el("div", "fs-bar-title");
    const barBody = S.el("div", "fs-bar-body");
    barText.append(barTitle, barBody);
    const barActions = S.el("div", "fs-bar-actions");
    const backBtn = Corvus.ui.button({
      variant: "ghost", icon: "chevron-left", label: "Previous", onClick: () => go(-1),
    });
    const skipBtn = Corvus.ui.button({
      variant: "ghost", label: "Skip", onClick: () => go(1, false),
    });
    const nextBtn = Corvus.ui.button({
      variant: "primary", label: "Next", onClick: () => go(1, true),
    });
    barActions.append(backBtn, skipBtn, nextBtn);
    bar.append(barText, barActions);
    page.appendChild(bar);

    const host = S.el("div", "fs-host");
    page.appendChild(host);

    function waiting() { return !!(snapshot && snapshot.link_bootloader && snapshot.link_bootloader.device); }

    function states() { return stepStates(payload, memory.visited, waiting()); }

    function paintSteps() {
      const st = states();
      stepList.innerHTML = "";
      STEPS.forEach((s, i) => {
        const li = S.el("li", "fs-step-item");
        const b = S.el("button", "fs-step");
        b.type = "button";
        b.dataset.step = s.id;
        const state = st[s.id];
        if (s.id === current) { b.classList.add("active"); b.setAttribute("aria-current", "step"); }
        if (state === "done") b.classList.add("done");
        if (state === "todo") b.classList.add("todo");
        const num = S.el("span", "fs-step-num");
        if (state === "done") num.appendChild(S.icon("check"));
        else num.textContent = String(i + 1);
        b.append(num, S.el("span", "fs-step-label", s.label));
        b.addEventListener("click", () => show(s.id));
        li.appendChild(b);
        stepList.appendChild(li);
      });
      S.refreshIcons();
    }

    function paintBar() {
      const step = STEPS[indexOf(current)];
      barTitle.textContent = step.title;
      barBody.textContent = step.text;
      const i = indexOf(current);
      backBtn.disabled = i === 0;
      const last = i === STEPS.length - 1;
      skipBtn.hidden = last;
      nextBtn.hidden = last;
    }

    function dropHosted() {
      if (hosted) {
        try { hosted.destroy(); } catch (err) { console.error("first setup step teardown:", err); }
        hosted = null;
      }
      host.innerHTML = "";
    }

    function show(id) {
      if (destroyed) return;
      current = id;
      memory.current = id;
      dropHosted();
      const step = STEPS[indexOf(id)];
      host.dataset.step = id;
      if (step.page) {
        const mod = PAGE_FOR[step.page] && PAGE_FOR[step.page]();
        if (mod && typeof mod.render === "function") {
          const slot = S.el("div", "fs-slot");
          host.appendChild(slot);
          const destroy = mod.render(slot, () => {});
          hosted = { destroy: typeof destroy === "function" ? destroy : () => {} };
        } else {
          host.appendChild(S.el("div", "field-hint", "This page is not available in this build."));
        }
      } else if (id === "aircraft") {
        hosted = buildAircraft();
        host.appendChild(hosted.el);
      } else {
        hosted = buildFinish();
        host.appendChild(hosted.el);
      }
      paintSteps();
      paintBar();
      S.refreshIcons();
    }

    function go(delta, completed) {
      const step = STEPS[indexOf(current)];
      if (completed) memory.visited[step.id] = true;
      const next = indexOf(current) + delta;
      if (next < 0 || next >= STEPS.length) return;
      show(STEPS[next].id);
      refresh();
    }

    /** Ask the vehicle what it holds now, and redraw what depends on it. */
    function refresh() {
      if (!Corvus.telemetry || typeof Corvus.telemetry.requestJson !== "function") {
        return Promise.resolve(null);
      }
      return Corvus.telemetry.requestJson("/api/first-setup").then((doc) => {
        if (destroyed) return null;
        payload = doc || null;
        paintSteps();
        paintBar();
        if (hosted && typeof hosted.onPayload === "function") hosted.onPayload(payload);
        return payload;
      }).catch(() => null);
    }

    /* ------------------------------ aircraft ------------------------------ */

    function buildAircraft() {
      const el = S.el("div", "page-card fs-aircraft");
      const status = S.el("div", "fs-status");
      const picker = S.el("div", "fs-picker");
      const note = S.el("div", "field-hint fs-note");
      const actions = S.el("div", "fs-actions");
      let chosen = null;
      let cards = null;
      let written = false;
      let rebooting = false;
      let unsubReboot = null;
      el.append(status, picker, note, actions);

      const apply = Corvus.ui.button({
        variant: "primary", icon: "check", label: "Use this aircraft type",
        onClick: () => confirmAndWrite(),
      });
      const reboot = S.rebootButton({
        mount: page,
        onRebooted: () => { rebooting = true; watchReturn(); paintAircraft(); },
      });
      actions.append(apply, reboot);

      function watchReturn() {
        if (unsubReboot || !Corvus.telemetry || !Corvus.telemetry.subscribe) return;
        let wentDown = false;
        unsubReboot = Corvus.telemetry.subscribe((t) => {
          if (!t) return;
          if (!t.connected) { wentDown = true; return; }
          if (!wentDown) return;
          if (typeof unsubReboot === "function") { unsubReboot(); unsubReboot = null; }
          window.setTimeout(() => {
            if (destroyed) return;
            rebooting = false;
            written = false;
            refresh().then(() => paintAircraft());
          }, 2500);
        });
      }

      function paintAircraft() {
        const p = payload || {};
        const armed = !!(snapshot && snapshot.armed);
        const list = p.presets || [];
        status.textContent = "";
        picker.innerHTML = "";
        cards = null;
        const current = p.preset_current;
        if (!p.connected) {
          status.textContent = "No controller is connected yet. Install the firmware in the "
            + "first step, then come back here.";
        } else if (rebooting) {
          status.textContent = "Waiting for the controller to restart. This takes a few seconds.";
        } else if (current != null) {
          const label = presetLabel(p, current);
          status.textContent = "This controller is set to " + (label || "airframe " + current) + ".";
        } else if (p.facts && p.facts.airframe === "todo") {
          status.textContent = "No aircraft type is selected on this controller.";
        } else {
          status.textContent = "Pick the type that matches your aircraft.";
        }
        if (p.connected && !list.length) {
          note.textContent = p.stack === "ardupilot"
            ? "ArduPilot Plane, Rover and Sub pick their type with the firmware you flashed. "
              + "There is nothing to choose here. Continue with the frame and motors."
            : "This firmware offers no presets here. Continue with the next step.";
        } else if (armed) {
          note.textContent = "The aircraft is armed. Disarm it before changing the aircraft type.";
        } else if (written) {
          note.textContent = "Written. The controller applies the new type after a reboot.";
        } else {
          note.textContent = p.stack === "ardupilot"
            ? "Sets the frame class and type. Reboot the controller afterwards."
            : "Sets the airframe and resets the controller's parameters to the defaults of that "
              + "airframe on the next boot. Reboot the controller afterwards.";
        }
        if (list.length) {
          const options = list.map((x) => ({ id: String(x.id), label: x.label, desc: x.group }));
          cards = Corvus.ui.optionCards({
            ariaLabel: "Aircraft type", options, columns: 3,
            value: chosen != null ? String(chosen) : (current != null ? String(current) : undefined),
            onChange: (id) => { chosen = Number(id); paintButtons(); },
          });
          picker.appendChild(cards.el);
        }
        paintButtons();
        S.refreshIcons();
      }

      function paintButtons() {
        const p = payload || {};
        const armed = !!(snapshot && snapshot.armed);
        const has = !!(p.presets && p.presets.length);
        apply.hidden = !has;
        apply.disabled = armed || !p.connected || chosen == null || written || rebooting
          || chosen === p.preset_current;
        reboot.hidden = !written;
        reboot.disabled = armed || rebooting;
      }

      function confirmAndWrite() {
        const p = payload || {};
        if (chosen == null) return;
        if (p.preset_current == null) { write(); return; }
        const body = S.el("div", "reboot-confirm",
          "This controller already has an aircraft type. Changing it to "
          + presetLabel(p, chosen) + " can reset settings you made before. Continue?");
        const m = Corvus.ui.modal({
          title: "Change the aircraft type?", size: "sm", body, mount: page,
          actions: [
            Corvus.ui.button({ variant: "secondary", label: "Cancel", onClick: () => m.close() }),
            Corvus.ui.button({ variant: "primary", label: "Change it",
              onClick: () => { m.close(); write(); } }),
          ],
        });
        m.open();
      }

      function write() {
        apply.classList.add("is-busy");
        Corvus.telemetry.postAction("/api/first-setup/airframe", { preset: chosen }).then((res) => {
          written = true;
          if (res && res.warning) {
            S.notify("warning", "The aircraft type is set, but: " + res.warning);
          } else {
            S.notify("info", "Aircraft type written. Reboot the controller to apply it.");
          }
          paintAircraft();
        }).catch((err) => {
          S.notify("critical", "Could not set the aircraft type: " + ((err && err.message) || "no answer"));
        }).finally(() => { apply.classList.remove("is-busy"); });
      }

      paintAircraft();
      return {
        el,
        onPayload: () => paintAircraft(),
        onTelemetry: () => paintButtons(),
        destroy() {
          if (typeof unsubReboot === "function") { unsubReboot(); unsubReboot = null; }
        },
      };
    }

    /* ------------------------------- finish ------------------------------- */

    function buildFinish() {
      const el = S.el("div", "page-card fs-finish");
      const list = S.el("ul", "fs-summary");
      const hint = S.el("div", "field-hint fs-note");
      const actions = S.el("div", "fs-actions");
      el.append(list, hint, actions);

      const again = Corvus.ui.button({
        variant: "secondary", icon: "refresh-cw", label: "Check again",
        onClick: () => refresh(),
      });
      const done = Corvus.ui.button({
        variant: "primary", icon: "circle-check", label: "Finish setup",
        onClick: finish,
      });
      actions.append(again, done);

      function finish() {
        if (payload && payload.fingerprint && Corvus.firstSetup) {
          Corvus.firstSetup.silence(String(payload.fingerprint));
        }
        Corvus.setupFirstSetup.forget();
        S.notify("info", "First setup finished.");
        navigateBack();
      }

      function paintFinish() {
        const st = states();
        list.innerHTML = "";
        let open = 0;
        STEPS.slice(0, -1).forEach((s) => {
          const li = S.el("li", "fs-summary-row " + st[s.id]);
          const mark = S.el("span", "fs-summary-mark");
          mark.appendChild(S.icon(st[s.id] === "done" ? "circle-check"
            : st[s.id] === "todo" ? "circle-alert" : "circle"));
          const name = S.el("span", "fs-summary-name", s.label);
          const word = S.el("span", "fs-summary-state",
            st[s.id] === "done" ? "Done" : st[s.id] === "todo" ? "Needs attention" : "Not visited");
          const goBtn = Corvus.ui.button({
            variant: "ghost", size: "sm", label: st[s.id] === "done" ? "Review" : "Open",
            onClick: () => show(s.id),
          });
          li.append(mark, name, word, goBtn);
          list.appendChild(li);
          if (st[s.id] !== "done") open += 1;
        });
        hint.textContent = open
          ? open + (open === 1 ? " step is" : " steps are") + " not marked done. You can finish now "
            + "and come back from the Setup page at any time."
          : "Everything is done. Check the aircraft with the preflight checklist before the first flight.";
        S.refreshIcons();
      }

      paintFinish();
      return { el, onPayload: () => paintFinish(), destroy() {} };
    }

    /* ----------------------------- life cycle ----------------------------- */

    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      unsub = Corvus.telemetry.subscribe((t) => {
        if (!t || destroyed) return;
        const armedChanged = !!t.armed !== !!(snapshot && snapshot.armed);
        const bootChanged = waiting() !== !!(t.link_bootloader && t.link_bootloader.device);
        snapshot = t;
        if ((armedChanged || bootChanged) && hosted && typeof hosted.onTelemetry === "function") {
          hosted.onTelemetry(t);
        }
        if (bootChanged) paintSteps();
      });
    }

    if (options.step || memory.current) {
      show(current);
      refresh();
    } else {
      /* Nobody named a step: look at the controller first, so the wizard opens
         where the work is instead of building a page it then throws away. */
      paintSteps();
      paintBar();
      host.appendChild(S.el("div", "field-hint", "Looking at the controller…"));
      refresh().then((doc) => {
        if (destroyed) return;
        show(initialStep(doc, waiting()));
      });
    }

    return function destroy() {
      destroyed = true;
      memory.open = false;
      dropHosted();
      if (typeof unsub === "function") { try { unsub(); } catch (_e) { /* gone */ } }
      unsub = null;
    };
  }

  return {
    render,
    isOpen: () => memory.open,
    forget() { memory.visited = {}; memory.current = ""; },
    STEPS, stepStates, initialStep, calibrationState, presetLabel,
  };
})();
