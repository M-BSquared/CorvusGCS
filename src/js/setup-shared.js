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
  // Trace colors are read from the theme at draw time rather than frozen as
  // constants, so the light theme gets its darker, saturated variants instead
  // of the dark theme's glowing ones. Kept as getters because the old constant
  // names are part of this module's surface.
  const chartColor = (k, fallback) => {
    try { return Corvus.ui.chartColors()[k] || fallback; } catch (_e) { return fallback; }
  };

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

  // Icons, the lucide refresh, and the page header/section title come from the
  // shared component layer. Setup used to carry its own copies of all four;
  // these are re-exports so the Setup sub-modules keep their short S.* names.
  // Icons are built with "auto" sizing because Setup's stylesheet already owns
  // their dimensions (.setup-tile .tile-icon svg, .calib-btn svg, ...).
  function icon(name) { return Corvus.ui.icon(name, "auto"); }
  const refreshIcons = Corvus.ui.refreshIcons;
  const pageHeader = Corvus.ui.pageHeader;
  const sectionTitle = Corvus.ui.sectionTitle;

  /**
   * A labelled value row used in the Vehicle Info card.
   * @param {string} label
   * @param {string|number} value
   * @param {string} [dataKey] Optional key written to the value span's
   *   `dataset.infoKey` so callers can look the row up later for live updates
   *   (e.g. the Setup tile grid's telemetry subscription). Omit to leave the
   *   span unmarked — backwards-compatible with the original 2-arg call.
   */
  function infoRow(label, value, dataKey) {
    const r = el("div", "page-row");
    r.appendChild(el("span", "page-row-label", label));
    const v = el("span", "page-row-value", String(value));
    if (dataKey) v.dataset.infoKey = dataKey;
    r.appendChild(v);
    return r;
  }

  /** Back button used at the top of each sub-page (apple-design: same path in
   *  and out — back returns along the entry path).
   *
   *  `label` names where back goes; it defaults to "Setup" because that is
   *  where this helper started, but a sub-page on another screen has to say
   *  its own parent or the button lies about the destination. */
  function backButton(onBack, label) {
    const target = label || "Setup";
    return Corvus.ui.button({
      variant: "ghost",
      size: "sm",
      className: "setup-back",
      icon: "chevron-left",
      label: target,
      ariaLabel: "Back to " + target,
      onClick: onBack,
    });
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
  /** Graph layout, themed. The surfaces, type and grid come from the active
   *  theme via Corvus.ui.plotlyTheme(); only what is specific to these graphs
   *  — margins, axis titles — is set here. Called on every redraw, so a theme
   *  switch takes effect on the next frame. */
  function plotlyLayout(unit) {
    const theme = Corvus.ui.plotlyTheme();
    return Object.assign({}, theme, {
      margin: { l: 44, r: 8, t: 6, b: 26 },
      showlegend: false,
      xaxis: Object.assign({}, theme.xaxis, { title: "Time (s)" }),
      yaxis: Object.assign({}, theme.yaxis, { title: unit }),
    });
  }

  /** Plotly config with reduced-motion zero-duration transitions when requested. */
  function plotlyConfig(reduced) {
    const config = { displayModeBar: false, responsive: true };
    if (reduced) { config.transition = { duration: 0 }; config.frame = { duration: 0 }; }
    return config;
  }

  return {
    MAX_POINTS, REDRAW_MIN_MS,
    get COLOR_RATE() { return chartColor("nav", "#4CC9FF"); },
    get COLOR_ATT() { return chartColor("healthy", "#45D483"); },
    get COLOR_VEL() { return chartColor("nav", "#4CC9FF"); },
    reducedMotion, el, icon, refreshIcons, pageHeader, sectionTitle, infoRow,
    backButton, runConfigAction, plotlyLayout, plotlyConfig,
  };
})();
