"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.pluginVibration — Vibration Monitor plugin for the FUTURE tab.
 *
 * Plots PX4 vibration metrics in a Plotly graph. PX4 repurposes the three
 * VIBRATION fields (verified against PX4 source): vibration_x = gyro delta-
 * angle coning, vibration_y = gyro high-frequency, vibration_z = accel high-
 * frequency vibration. The accel-HF trace (red) is the main mechanical-health
 * indicator — higher = more mechanical vibration.
 *
 * Design (lean + on-demand streaming, matching the backend contract):
 *  - On open, POST /api/vibration/stream {enabled:true, rate_hz:10} asks PX4
 *    for ~10 Hz VIBRATION (PX4 defaults to 0.1 Hz). This is a one-shot config
 *    action, NOT telemetry polling — vibration data still flows through the
 *    existing telemetry SSE via api.subscribe. If the request fails we show a
 *    non-fatal banner and keep working at whatever rate PX4 sends.
 *  - The buffer only appends a point when a vibration value actually CHANGED
 *    since the last appended point, so unrelated telemetry snapshots (attitude
 *    etc. trigger them too) don't flood the buffer with duplicates.
 *  - Plotly.react (not newPlot) is throttled to ~10 Hz for efficient redraws.
 *  - destroy() unsubscribes, Plotly.purge()s the chart, and best-effort
 *    restores PX4's default VIBRATION rate (POST enabled:false) — no leaks.
 *
 * Reduced motion: when the OS asks for less motion, Plotly transitions are
 * zeroed (static react updates) — apple-design requirement.
 */
Corvus.pluginVibration = (function () {
  const MAX_POINTS = 600;          // ~60 s at 10 Hz — capped memory
  const REDRAW_MIN_MS = 100;       // throttle Plotly.react to ~10 Hz

  // Trace colours reuse the app's semantic palette: nav blue (info), warning
  // yellow, critical red. The accel-HF trace (red) is the health indicator.
  const COLORS = { x: "#4CC9FF", y: "#F5C842", z: "#FF514D" };

  /**
   * Pure buffer-update: append only when a vibration value changed since the
   * last appended point. Exposed (via the module's updateBuffer) so the test
   * suite can drive it directly without a DOM/Plotly.
   *
   * @param {Object} buf   {t:[],vx:[],vy:[],vz:[]}
   * @param {Object} s      telemetry state (vibration_x/y/z present)
   * @param {number} tSec   relative seconds for the new point
   * @returns {boolean} true iff a point was appended
   */
  function updateBuffer(buf, s, tSec) {
    if (!buf || !s) return false;
    const vx = Number(s.vibration_x) || 0;
    const vy = Number(s.vibration_y) || 0;
    const vz = Number(s.vibration_z) || 0;
    const n = buf.vx.length;
    if (n > 0) {
      const lastX = buf.vx[n - 1];
      const lastY = buf.vy[n - 1];
      const lastZ = buf.vz[n - 1];
      // Unchanged since last append → skip (dedupe unrelated snapshots).
      if (lastX === vx && lastY === vy && lastZ === vz) return false;
    }
    buf.t.push(tSec);
    buf.vx.push(vx);
    buf.vy.push(vy);
    buf.vz.push(vz);
    // Cap memory: drop the oldest point past the window.
    if (buf.t.length > MAX_POINTS) {
      buf.t.shift(); buf.vx.shift(); buf.vy.shift(); buf.vz.shift();
    }
    return true;
  }

  function init(containerEl, api) {
    const reduced = api && typeof api.reducedMotion === "function" && api.reducedMotion();

    // Plotly not vendored → clear fallback message, do NOT subscribe. destroy
    // stays safe to call (guards below).
    if (typeof window === "undefined" || typeof window.Plotly === "undefined") {
      containerEl.innerHTML = '<div class="vib-warning">Plotly not available (vendor/plotly-basic.min.js missing)</div>';
      return;
    }

    // Ask PX4 for high-rate VIBRATION. One-shot config action, not polling.
    // Failure is non-fatal: keep working at PX4's default rate.
    let warn = null;
    if (api && typeof api.postAction === "function") {
      Promise.resolve()
        .then(() => api.postAction("/api/vibration/stream", { enabled: true, rate_hz: 10 }))
        .catch(() => {
          warn = document.createElement("div");
          warn.className = "vib-warning";
          warn.textContent = "Could not enable high-rate vibration stream — showing PX4 default-rate data";
          containerEl.insertBefore(warn, containerEl.firstChild);
        });
    }

    // Build the UI with createElement and keep direct refs to the chart and
    // stats elements (no querySelector) so teardown is robust and the layout
    // is introspectable.
    containerEl.innerHTML = "";

    const header = document.createElement("div");
    header.className = "vib-header";
    const title = document.createElement("span");
    title.className = "vib-title";
    title.textContent = "Vibration Monitor";
    header.appendChild(title);

    const legend = document.createElement("div");
    legend.className = "vib-legend";
    const legItems = [
      { cls: "vib-leg-x", color: COLORS.x, label: "Gyro coning (x)" },
      { cls: "vib-leg-y", color: COLORS.y, label: "Gyro HF vibration (y)" },
      { cls: "vib-leg-z", color: COLORS.z, label: "Accel HF vibration (z)" },
    ];
    legItems.forEach((li) => {
      const leg = document.createElement("span");
      leg.className = "vib-leg " + li.cls;
      const dot = document.createElement("span");
      dot.className = "vib-dot";
      dot.style.background = li.color;
      leg.appendChild(dot);
      leg.appendChild(document.createTextNode(li.label));
      legend.appendChild(leg);
    });

    const note = document.createElement("div");
    note.className = "vib-note";
    note.textContent =
      "PX4 repurposes these fields: vibration_x = gyro delta-angle coning, " +
      "vibration_y = gyro high-frequency, vibration_z = accel high-frequency vibration. " +
      "Higher accel-HF (red) = more mechanical vibration.";

    const chartDiv = document.createElement("div");
    chartDiv.className = "vib-chart";

    const statsEl = document.createElement("div");
    statsEl.className = "vib-stats";

    containerEl.appendChild(header);
    containerEl.appendChild(legend);
    containerEl.appendChild(note);
    containerEl.appendChild(chartDiv);
    containerEl.appendChild(statsEl);

    const startMs = Date.now();
    const buf = { t: [], vx: [], vy: [], vz: [] };
    let lastRedraw = 0;

    function buildTraces() {
      return [
        { x: buf.t, y: buf.vx, mode: "lines", name: "Gyro coning", line: { color: COLORS.x, width: 1.5 } },
        { x: buf.t, y: buf.vy, mode: "lines", name: "Gyro HF", line: { color: COLORS.y, width: 1.5 } },
        { x: buf.t, y: buf.vz, mode: "lines", name: "Accel HF", line: { color: COLORS.z, width: 1.5 } },
      ];
    }

    function buildLayout() {
      return {
        paper_bgcolor: "#0d1117",
        plot_bgcolor: "#0d1117",
        font: { color: "#A5ADB8", family: "JetBrains Mono, monospace", size: 10 },
        margin: { l: 44, r: 8, t: 6, b: 26 },
        showlegend: true,
        legend: { x: 1, y: 1, xanchor: "right", yanchor: "top", bgcolor: "rgba(13,17,23,0)" },
        xaxis: { title: "Time (s)", gridcolor: "rgba(255,255,255,0.06)", zerolinecolor: "rgba(255,255,255,0.08)", tickfont: { size: 9 } },
        yaxis: { title: "Vibration metric (unitless)", gridcolor: "rgba(255,255,255,0.06)", zerolinecolor: "rgba(255,255,255,0.08)", tickfont: { size: 9 } },
      };
    }

    // Reduced motion → zero-duration Plotly transitions (static react updates),
    // per apple-design: no animated redraws when the OS asks for less motion.
    const config = { displayModeBar: false, responsive: true };
    if (reduced) {
      config.transition = { duration: 0 };
      config.frame = { duration: 0 };
    }

    function redraw() {
      lastRedraw = Date.now();
      try {
        window.Plotly.react(chartDiv, buildTraces(), buildLayout(), config);
      } catch (err) {
        console.error("vibration Plotly.react failed:", err);
      }
    }

    function updateStats(s) {
      const c0 = Number(s.clipping_0) || 0;
      const c1 = Number(s.clipping_1) || 0;
      const c2 = Number(s.clipping_2) || 0;
      const vx = Number(s.vibration_x) || 0;
      const vy = Number(s.vibration_y) || 0;
      const vz = Number(s.vibration_z) || 0;
      const fmt = (n) => n.toFixed(4);
      statsEl.innerHTML =
        '<span class="vib-stat"><span class="vib-stat-k">x</span> ' + fmt(vx) + '</span>' +
        '<span class="vib-stat"><span class="vib-stat-k">y</span> ' + fmt(vy) + '</span>' +
        '<span class="vib-stat"><span class="vib-stat-k">z</span> ' + fmt(vz) + '</span>' +
        '<span class="vib-stat"><span class="vib-stat-k">Clipping</span> ' + c0 + '/' + c1 + '/' + c2 + '</span>';
    }

    function onTelemetry(s) {
      if (!s) return;
      const tSec = (Date.now() - startMs) / 1000;
      updateBuffer(buf, s, tSec);
      updateStats(s);
      // Throttle Plotly.react to ~10 Hz even on rapid updates. The telemetry
      // callback already runs on a rAF (telemetry.js notifySubscribers), so a
      // direct call here stays on the compositor clock (apple-design).
      if (Date.now() - lastRedraw >= REDRAW_MIN_MS) redraw();
    }

    // Initial empty figure so the chart has a layout before first data.
    redraw();

    let unsub = null;
    if (api && typeof api.subscribe === "function") {
      unsub = api.subscribe(onTelemetry);
    }

    // Capture everything destroy needs on the container so destroy is a pure
    // teardown with no external references.
    containerEl._vibDestroy = function () {
      if (unsub) { try { unsub(); } catch (_e) {} unsub = null; }
      try { window.Plotly.purge(chartDiv); } catch (_e) {}
      // Best-effort restore of PX4 default rate (lean). Never blocks destroy.
      if (api && typeof api.postAction === "function") {
        Promise.resolve()
          .then(() => api.postAction("/api/vibration/stream", { enabled: false, rate_hz: 10 }))
          .catch(() => {});
      }
      buf.t = null; buf.vx = null; buf.vy = null; buf.vz = null;
      containerEl._vibDestroy = null;
    };
  }

  function destroy(containerEl) {
    // Guard every teardown so destroy never throws — the registry relies on it.
    if (!containerEl) return;
    if (typeof containerEl._vibDestroy === "function") {
      containerEl._vibDestroy();
    }
  }

  // Expose updateBuffer for the test suite (pure function, no DOM/Plotly).
  return { init, destroy, updateBuffer, MAX_POINTS, REDRAW_MIN_MS };
})();

// Register at module load so the plugin appears in the grid before app init.
if (window.Corvus && Corvus.plugins && typeof Corvus.plugins.register === "function") {
  Corvus.plugins.register("vibration", {
    name: "Vibration Monitor",
    icon: "activity",
    description: "Live PX4 vibration metrics and accelerometer clipping",
    init: function (containerEl, api) { Corvus.pluginVibration.init(containerEl, api); },
    destroy: function (containerEl) { Corvus.pluginVibration.destroy(containerEl); },
  });
}
