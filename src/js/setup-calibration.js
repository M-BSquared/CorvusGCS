"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupCalibration — the Calibration sub-page of the Setup page.
 *
 * Sensor calibration buttons (compass/gyro/accel/level/airspeed/baro) with
 * live STATUSTEXT guidance, plus the POD Tuning subsection (PX4 axis-based
 * autotune: roll/pitch/yaw/all) and three live Plotly graphs of the controller
 * states being tuned (roll rate, roll attitude, horizontal velocity).
 *
 * Backend contract (verified against PX4 v1.18):
 *   POST /api/calibrate {type}   sensor calibration (refused while armed)
 *   POST /api/autotune {axis}    PX4 autotune (refused while armed)
 *
 * PX4 accuracy note: PX4 has NO velocity-controller autotune. The three live
 * graphs are views of the controller states, NOT autotune targets. There is
 * intentionally no "Velocity" autotune button.
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the view lifecycle: it calls destroy() on back / left-nav re-entry so no
 * telemetry subscription, Plotly graph, or buffer leaks.
 */
Corvus.setupCalibration = (function () {
  const S = Corvus.setupShared;

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page");
    // The back button only asks the orchestrator to navigate back; the
    // orchestrator's teardown() is the single place that calls destroy().
    page.appendChild(S.backButton(navigateBack));
    page.appendChild(S.pageHeader("Calibration", "Sensor calibration and POD tuning"));

    // --- Sensor calibration buttons ----------------------------------------
    const sensorSection = S.el("div", "page-section");
    sensorSection.appendChild(S.sectionTitle("Sensor Calibration"));
    const sensorCard = S.el("div", "page-card setup-sensor-card");

    const calibTypes = [
      { type: "compass", label: "Compass", icon: "compass" },
      { type: "gyro", label: "Gyroscope", icon: "rotate-3d" },
      { type: "accel", label: "Accelerometer", icon: "move-3d" },
      { type: "level", label: "Level Horizon", icon: "separator-horizontal" },
      { type: "airspeed", label: "Airspeed", icon: "wind" },
      { type: "baro", label: "Baro", icon: "gauge" },
      // ESC/motor calibration via MAV_CMD_PREFLIGHT_CALIBRATION param7=1.0.
      // "fan" is lucide's propeller glyph (verified in the bundled set).
      { type: "motor", label: "Motors (ESC)", icon: "fan" },
    ];

    const calibGrid = S.el("div", "calib-grid");
    const calibBtns = calibTypes.map((c) => {
      // Motor/ESC calibration spins motors at max PWM — flag it as a danger
      // tile so the operator sees a red button, not a benign secondary one.
      // The motor button is also gated by a safety-confirm modal (props-off +
      // battery procedure); every other calibration runs directly.
      const b = Corvus.ui.button({
        variant: c.type === "motor" ? "danger" : "secondary",
        className: "calib-btn",
        icon: c.icon,
        onClick: () => {
          if (c.type === "motor") runMotorCalibration();
          else runCalibration(c.type);
        },
      });
      b.dataset.type = c.type;
      b.appendChild(S.el("span", "calib-btn-label", c.label));
      b.appendChild(S.el("span", "calib-btn-status", ""));
      calibGrid.appendChild(b);
      return b;
    });
    sensorCard.appendChild(calibGrid);

    const armedBanner = S.el("div", "params-banner setup-armed-banner");
    armedBanner.hidden = true;
    armedBanner.textContent = "Cannot calibrate while armed — disarm first.";
    sensorCard.appendChild(armedBanner);

    const guidanceTitle = S.el("div", "guidance-title");
    guidanceTitle.appendChild(S.icon("info"));
    guidanceTitle.appendChild(S.el("span", null, "Guidance (live STATUSTEXT)"));
    sensorCard.appendChild(guidanceTitle);

    const guidanceList = S.el("div", "guidance-list");
    guidanceList.setAttribute("role", "log");
    guidanceList.setAttribute("aria-live", "polite");
    sensorCard.appendChild(guidanceList);

    sensorSection.appendChild(sensorCard);
    page.appendChild(sensorSection);

    // --- POD Tuning (autotune + live graphs) -------------------------------
    const podSection = S.el("div", "page-section pod-section");
    podSection.appendChild(S.sectionTitle("POD Tuning"));

    const podNote = S.el("div", "pod-note");
    podNote.textContent =
      "PX4 autotune tunes the rate + attitude controllers together. Refused while armed. " +
      "Follow STATUSTEXT guidance during tuning. (PX4 has no velocity-controller autotune; " +
      "the graphs below are live telemetry views, not autotune targets.)";
    podSection.appendChild(podNote);

    const autotuneControls = S.el("div", "autotune-controls");
    const axisTypes = [
      { axis: "roll", label: "Roll" },
      { axis: "pitch", label: "Pitch" },
      { axis: "yaw", label: "Yaw" },
      { axis: "all", label: "All" },
    ];
    const autotuneBtns = axisTypes.map((a) => {
      const b = Corvus.ui.button({
        variant: "secondary",
        className: "calib-btn autotune-btn",
        onClick: () => runAutotune(a.axis),
      });
      b.dataset.axis = a.axis;
      b.appendChild(S.el("span", "calib-btn-label", a.label));
      b.appendChild(S.el("span", "calib-btn-status", ""));
      autotuneControls.appendChild(b);
      return b;
    });
    podSection.appendChild(autotuneControls);

    const autotuneBanner = S.el("div", "params-banner setup-armed-banner");
    autotuneBanner.hidden = true;
    autotuneBanner.textContent = "Cannot autotune while armed — disarm first.";
    podSection.appendChild(autotuneBanner);

    // Three live Plotly graphs: roll rate, roll attitude, horizontal velocity.
    const graphsHost = S.el("div", "autotune-graphs");
    const graphSpecs = [
      { id: "rollrate", title: "Roll Rate", field: "rollspeed", unit: "deg/s", color: S.COLOR_RATE },
      { id: "rollatt", title: "Roll Attitude", field: "roll", unit: "deg", color: S.COLOR_ATT },
      { id: "hvel", title: "Horizontal Velocity", field: "groundspeed", unit: "m/s", color: S.COLOR_VEL },
    ];
    const graphs = graphSpecs.map((g) => {
      const host = S.el("div", "autotune-graph-host");
      const head = S.el("div", "autotune-graph-head");
      head.appendChild(S.el("span", "autotune-graph-title", g.title));
      const live = S.el("span", "autotune-live");
      live.appendChild(S.el("span", "autotune-live-dot"));
      live.appendChild(S.el("span", null, "live"));
      head.appendChild(live);
      host.appendChild(head);
      const chart = S.el("div", "autotune-graph");
      chart.dataset.graph = g.id;
      host.appendChild(chart);
      graphsHost.appendChild(host);
      return { spec: g, chart };
    });
    podSection.appendChild(graphsHost);
    page.appendChild(podSection);

    container.appendChild(page);

    // --- Wire telemetry: armed gating + guidance + graph buffers ----------
    const reduced = S.reducedMotion();
    const startMs = Date.now();
    const bufs = graphs.map(() => ({ t: [], y: [] }));
    let lastRedraw = 0;

    function buildTrace(g, buf) {
      return [{ x: buf.t, y: buf.y, mode: "lines", line: { color: g.spec.color, width: 1.5 } }];
    }
    const config = S.plotlyConfig(reduced);

    function redrawGraph(idx) {
      const g = graphs[idx];
      if (typeof window === "undefined" || typeof window.Plotly === "undefined") return;
      try { window.Plotly.react(g.chart, buildTrace(g, bufs[idx]), S.plotlyLayout(g.spec.unit), config); }
      catch (err) { console.error("setup Plotly.react failed:", err); }
    }

    function appendPoint(idx, value, tSec) {
      const buf = bufs[idx];
      const y = Number(value);
      if (!isFinite(y)) return;
      buf.t.push(tSec);
      buf.y.push(y);
      if (buf.t.length > S.MAX_POINTS) { buf.t.shift(); buf.y.shift(); }
    }

    // Initial empty figures so the graphs have a layout before first data.
    graphs.forEach((_, i) => redrawGraph(i));
    lastRedraw = Date.now();

    // Track the last guidance signature so we only re-render on change.
    let lastGuidanceSig = "";

    function renderGuidance(host, warnings) {
      const sig = JSON.stringify(warnings);
      if (sig === lastGuidanceSig) return;
      lastGuidanceSig = sig;
      host.innerHTML = "";
      if (!warnings.length) {
        host.appendChild(S.el("div", "guidance-empty", "Waiting for STATUSTEXT guidance…"));
        return;
      }
      // Render the latest ~50 so a long calibration doesn't grow unbounded.
      warnings.slice(-50).forEach((w) => {
        const line = S.el("div", "guidance-line" + (w.level ? " " + w.level : ""));
        line.appendChild(S.el("span", "guidance-level", w.level || "info"));
        line.appendChild(S.el("span", "guidance-msg", w.msg || ""));
        if (w.meta) line.appendChild(S.el("span", "guidance-meta", w.meta));
        host.appendChild(line);
      });
      host.scrollTop = host.scrollHeight;
    }

    function onTelemetry(s) {
      if (!s) return;
      // Armed gating for calibration + autotune buttons.
      const armed = !!s.armed;
      calibBtns.forEach((b) => { b.disabled = armed; });
      autotuneBtns.forEach((b) => { b.disabled = armed; });
      armedBanner.hidden = !armed;
      autotuneBanner.hidden = !armed;

      // Guidance: render the latest STATUSTEXT entries (warnings shape {level,msg,meta}).
      renderGuidance(guidanceList, s.warnings || []);

      // Append a point to each graph buffer and throttle the redraw to ~10 Hz.
      const tSec = (Date.now() - startMs) / 1000;
      graphs.forEach((g, i) => appendPoint(i, s[g.spec.field], tSec));
      if (Date.now() - lastRedraw >= S.REDRAW_MIN_MS) {
        lastRedraw = Date.now();
        graphs.forEach((_, i) => redrawGraph(i));
      }
    }

    // Subscribe to telemetry (the SSE is the live-data source — no polling).
    let unsub = null;
    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      unsub = Corvus.telemetry.subscribe(onTelemetry);
      const cur = Corvus.telemetry.getState();
      if (cur) onTelemetry(cur);
    }

    // Tracks the open motor-safety modal so destroy() can remove it on
    // back/re-render (no leaked DOM subtree if the operator leaves mid-gate).
    let motorModal = null;

    // Modal close + Escape handler live at render scope so destroy() can reuse
    // closeMotorModal() (DRY). Escape handling and listener cleanup now live
    // in Corvus.ui.modal, which registers its keydown listener only while the
    // dialog is mounted and removes it on every close path — the leak this
    // screen used to have (a back-navigation with the dialog open left a
    // keydown listener on document) is structurally gone.
    // Idempotent: a no-op when the dialog was never opened or already closed.
    function closeMotorModal() {
      if (!motorModal) return;
      motorModal.close();   // onClose nulls motorModal
      motorModal = null;
    }

    async function runCalibration(type) {
      await S.runConfigAction(calibBtns, findBtn(calibBtns, "type", type),
        "/api/calibrate", { type }, "Calibrating…", "Calibrated");
    }

    async function runAutotune(axis) {
      await S.runConfigAction(autotuneBtns, findBtn(autotuneBtns, "axis", axis),
        "/api/autotune", { axis }, "Tuning…", "Autotune started");
    }

    // Motor/ESC calibration is dangerous (PX4 spins motors at max PWM). Gate
    // it behind a safety-confirm modal so the operator must acknowledge the
    // prop-off + battery procedure before the POST goes out. The modal is
    // appended to `page` (NOT document.body) so the DOM-stub test harness —
    // which has no document.body and only walks the container subtree — can
    // reach it; position:fixed still covers the viewport regardless of parent.
    function runMotorCalibration() {
      if (motorModal) return;   // already open — ignore re-clicks

      const body = S.el("div", "motor-calib-warning");
      [
        "Motors will spin at maximum PWM. Remove ALL propellers before continuing.",
        "Disconnect the flight battery now. The ESCs are powered by re-plugging the battery AFTER you press Calibrate, when PX4 instructs you to.",
        "Follow the live STATUSTEXT guidance below the sensor buttons during calibration.",
      ].forEach((line) => body.appendChild(S.el("div", "motor-calib-warning-line", line)));

      const cancel = Corvus.ui.button({
        variant: "secondary", label: "Cancel", onClick: closeMotorModal,
      });
      const confirm = Corvus.ui.button({
        variant: "danger", label: "Calibrate Motors",
        onClick: () => {
          closeMotorModal();
          const motorBtn = findBtn(calibBtns, "type", "motor");
          S.runConfigAction(calibBtns, motorBtn, "/api/calibrate",
            { type: "motor" }, "Calibrating…", "Calibration started");
        },
      });

      // Mounted on `page`, NOT document.body, so tearing the sub-page down
      // takes the dialog with it (and so the DOM-stub test harness, which has
      // no document.body and only walks the container subtree, can reach it).
      // The overlay is position:fixed either way.
      motorModal = Corvus.ui.modal({
        title: "Motor / ESC Calibration",
        size: "sm",
        body,
        actions: [cancel, confirm],
        mount: page,
        onClose: () => { motorModal = null; },
      });
      motorModal.open();
    }

    function findBtn(list, key, val) {
      return list.find((b) => b.dataset[key] === val) || list[0];
    }

    // Teardown returned to the caller (setup.js). Idempotent + guarded.
    function destroy() {
      // Release the motor-safety modal AND its document keydown listener via
      // closeMotorModal() (DRY). Idempotent: no-op when the modal was never
      // opened, and safe to call again after it already ran (motorModal null).
      closeMotorModal();
      if (unsub) { try { unsub(); } catch (_e) {} unsub = null; }
      if (typeof window !== "undefined" && typeof window.Plotly !== "undefined" && window.Plotly) {
        graphs.forEach((g) => {
          try { window.Plotly.purge(g.chart); } catch (_e) {}
        });
      }
      bufs.forEach((b) => { b.t = null; b.y = null; });
    }

    return destroy;
  }

  return { render };
})();
