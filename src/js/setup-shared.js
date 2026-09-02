"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupShared — shared helpers and constants for the Setup page modules.
 *
 * Kept stateless (no DOM ownership, no subscriptions) so the calibration and
 * parameters sub-pages can import these without coupling to each other. This
 * is the common layer extracted out of the former setup.js monolith so each
 * page lives in its own file.
 */
Corvus.setupShared = (function () {
  // Rolling-window cap and ~10 Hz redraw throttle for the live Plotly graphs,
  // matching the vibration plugin's proven pattern (capped memory, rAF-friendly).
  const MAX_POINTS = 600;
  const REDRAW_MIN_MS = 100;

  // Semantic palette reused from the app CSS variables (kept in sync here so
  // the Plotly dark theme matches the HUD). Roll rate + horizontal velocity use
  // nav blue (#4CC9FF); roll attitude uses healthy green (#45D483).
  const COLOR_RATE = "#4CC9FF";
  const COLOR_ATT = "#45D483";
  const COLOR_VEL = "#4CC9FF";

  /** True when the OS asks for less motion (read per sub-page build). */
  function reducedMotion() {
    return !!(typeof window !== "undefined" && window.matchMedia
      && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
  }

  /** Tiny helper: build an element with a class and optional text. */
  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  /** Build a lucide <i data-lucide="name"> icon element. */
  function icon(name) {
    const i = document.createElement("i");
    i.setAttribute("data-lucide", name);
    return i;
  }

  /** Refresh lucide icons if the library is present (no-op otherwise). */
  function refreshIcons() {
    if (window.lucide && lucide.createIcons) lucide.createIcons();
  }

  /** Page header shared by the grid and both sub-pages. */
  function pageHeader(title, subtitle) {
    const h = el("div", "page-header");
    h.appendChild(el("div", "page-title", title));
    h.appendChild(el("div", "page-subtitle", subtitle));
    return h;
  }

  /** Section title (the underlined label used inside pages). */
  function sectionTitle(text) {
    return el("div", "page-section-title", text);
  }

  /** A labelled value row used in the Vehicle Info card. */
  function infoRow(label, value) {
    const r = el("div", "page-row");
    r.appendChild(el("span", "page-row-label", label));
    r.appendChild(el("span", "page-row-value", String(value)));
    return r;
  }

  /** Back button used at the top of each sub-page (apple-design: same path in
   *  and out — back returns along the entry path). */
  function backButton(onBack) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "btn setup-back";
    b.setAttribute("data-variant", "ghost");
    b.setAttribute("data-size", "sm");
    b.appendChild(icon("chevron-left"));
    b.appendChild(el("span", null, "Setup"));
    b.setAttribute("aria-label", "Back to Setup");
    b.addEventListener("click", onBack);
    return b;
  }

  /**
   * Shared one-shot config action: disable siblings while in flight, show a
   * spinner on the clicked button, then a green confirmation or the error
   * message via the existing notification system + a local status line.
   * `siblings` is the explicit button list (avoids parentElement walks so the
   * action is robust and the lifecycle is testable).
   */
  async function runConfigAction(siblings, btn, url, payload, busyText, okText) {
    const status = btn.querySelector(".calib-btn-status");
    siblings.forEach((b) => { b.disabled = true; });
    btn.classList.add("busy");
    if (status) { status.textContent = busyText; status.className = "calib-btn-status busy"; }
    try {
      await Corvus.telemetry.postAction(url, payload);
      if (status) { status.textContent = okText; status.className = "calib-btn-status ok"; }
      window.dispatchEvent(new CustomEvent("corvus:notification",
        { detail: { level: "info", message: okText } }));
    } catch (err) {
      const msg = (err && err.message) || "Action failed";
      if (status) { status.textContent = msg; status.className = "calib-btn-status err"; }
      window.dispatchEvent(new CustomEvent("corvus:notification",
        { detail: { level: "critical", message: msg } }));
    } finally {
      btn.classList.remove("busy");
      // Re-gate from the live armed state; if disarmed, re-enable so the
      // operator can run the next calibration immediately.
      const s = Corvus.telemetry && Corvus.telemetry.getState();
      const armed = !!(s && s.armed);
      siblings.forEach((b) => { b.disabled = armed; });
    }
  }

  /** A dark Plotly layout for a single-trace live graph (shared by all graphs). */
  function plotlyLayout(unit) {
    return {
      paper_bgcolor: "#0d1117",
      plot_bgcolor: "#0d1117",
      font: { color: "#A5ADB8", family: "JetBrains Mono, monospace", size: 10 },
      margin: { l: 44, r: 8, t: 6, b: 26 },
      showlegend: false,
      xaxis: { title: "Time (s)", gridcolor: "rgba(255,255,255,0.06)", zerolinecolor: "rgba(255,255,255,0.08)", tickfont: { size: 9 } },
      yaxis: { title: unit, gridcolor: "rgba(255,255,255,0.06)", zerolinecolor: "rgba(255,255,255,0.08)", tickfont: { size: 9 } },
    };
  }

  /** Plotly config with reduced-motion zero-duration transitions when requested. */
  function plotlyConfig(reduced) {
    const config = { displayModeBar: false, responsive: true };
    if (reduced) { config.transition = { duration: 0 }; config.frame = { duration: 0 }; }
    return config;
  }

  return {
    MAX_POINTS, REDRAW_MIN_MS, COLOR_RATE, COLOR_ATT, COLOR_VEL,
    reducedMotion, el, icon, refreshIcons, pageHeader, sectionTitle, infoRow,
    backButton, runConfigAction, plotlyLayout, plotlyConfig,
  };
})();
