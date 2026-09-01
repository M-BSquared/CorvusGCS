"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setup — the Setup page (left-nav "SETUP").
 *
 * Thin orchestrator: renders the tile grid (Calibration, Parameters) plus a
 * compact Vehicle Info card, and routes to the sub-pages. The actual page
 * content lives in its own file:
 *   - setup-calibration.js  (Corvus.setupCalibration)
 *   - setup-parameters.js   (Corvus.setupParameters)
 * Shared helpers live in setup-shared.js (Corvus.setupShared).
 *
 * Lifecycle: each sub-page render returns a `destroy()` that the orchestrator
 * stores as `activeDestroy`. `render()` always tears the active view down first
 * (idempotent, no leaks on left-nav re-entry), then shows the tile grid. The
 * back button in each sub-page calls its own destroy() then asks the orchestrator
 * to return to the grid. apple-design materials/motion; reduced-motion is
 * handled per sub-page (Plotly transitions zeroed).
 */
Corvus.setup = (function () {
  const S = Corvus.setupShared;

  // Active view of the Setup page: "tiles" (the grid) | "calibration" | "parameters".
  let activeView = "tiles";

  // Teardown handle for the currently-rendered view. `render` calls this before
  // building anything new, so re-entering Setup via the left-nav is always clean.
  let activeDestroy = null;

  /**
   * Public entry: render the Setup page into `container`. Always tears down any
   * prior view first (idempotent, no leaks on left-nav re-entry), then shows
   * the tile grid. Sub-pages swap the container content and register their own
   * teardown via `activeDestroy`.
   * @param {HTMLElement} container
   */
  function render(container) {
    teardown();
    activeView = "tiles";
    renderTiles(container);
  }

  /** Run the active view's teardown if present, then clear it. Idempotent. */
  function teardown() {
    if (typeof activeDestroy === "function") {
      try { activeDestroy(); } catch (err) { console.error("setup teardown failed:", err); }
    }
    activeDestroy = null;
  }

  function renderTiles(container) {
    container.innerHTML = "";
    container.appendChild(S.pageHeader("Setup", "Vehicle configuration and calibration"));

    // Compact Vehicle Info card stays at the top of the grid so the operator
    // still sees autopilot / version / connection at a glance (kept lean).
    const state = (Corvus.telemetry && Corvus.telemetry.getState()) || {};
    const infoCard = S.el("div", "page-card setup-vehicle-info");
    infoCard.appendChild(S.sectionTitle("Vehicle Info"));
    infoCard.appendChild(S.infoRow("Autopilot", state.autopilot || "—"));
    infoCard.appendChild(S.infoRow("Vehicle Type", state.vehicle_type || "—"));
    infoCard.appendChild(S.infoRow("PX4 Version", state.px4_version || "—"));
    infoCard.appendChild(S.infoRow("Connected", state.connected ? "Yes" : "No"));
    infoCard.appendChild(S.infoRow("Armed", state.armed ? "Yes" : "No"));
    container.appendChild(infoCard);

    // Tile grid: Calibration + Parameters. Keyboard-focusable buttons so the
    // whole tile is reachable and announces as a control.
    const grid = S.el("div", "setup-tiles");
    grid.appendChild(makeTile("calibration", "sliders-horizontal", "Calibration",
      "Sensor calibration, level/airspeed, and POD autotune with live graphs."));
    grid.appendChild(makeTile("parameters", "list", "Parameters",
      "Download all PX4 parameters on demand, then edit any value."));
    container.appendChild(grid);
    S.refreshIcons();
  }

  /**
   * Build one setup tile. apple-design: lift on hover, scale on press, focus
   * ring, spring-eased motion (handled in CSS via transform/opacity only).
   */
  function makeTile(viewId, iconId, title, desc) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "setup-tile";
    btn.dataset.view = viewId;
    btn.setAttribute("aria-label", title);

    const iconWrap = S.el("span", "setup-tile-icon");
    iconWrap.appendChild(S.icon(iconId));

    const body = S.el("div", "setup-tile-body");
    body.appendChild(S.el("span", "setup-tile-title", title));
    body.appendChild(S.el("span", "setup-tile-desc", desc));

    btn.appendChild(iconWrap);
    btn.appendChild(body);
    btn.appendChild(S.icon("chevron-right"));
    btn.addEventListener("click", () => openView(viewId));
    return btn;
  }

  /** Swap the container to a sub-page. Tears the grid down first. */
  function openView(viewId) {
    if (viewId !== "calibration" && viewId !== "parameters") return;
    teardown();
    activeView = viewId;
    const container = document.getElementById("pageView");
    if (!container) return;
    container.innerHTML = "";
    // The sub-page's back button calls its own destroy() then navigateBack(),
    // which tears down (already done) and re-renders the grid.
    const navigateBack = () => {
      teardown();
      activeView = "tiles";
      const c = document.getElementById("pageView");
      if (c) renderTiles(c);
      S.refreshIcons();
    };
    if (viewId === "calibration") {
      activeDestroy = Corvus.setupCalibration.render(container, navigateBack);
    } else {
      activeDestroy = Corvus.setupParameters.render(container, navigateBack);
    }
    S.refreshIcons();
  }

  return { render };
})();
