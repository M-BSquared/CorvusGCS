"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setup — the Setup page (left-nav "SETUP").
 *
 * Thin orchestrator: renders the tile grid (Calibration, Motors, Safety &
 * Sensors, Parameters, Firmware) plus a compact Vehicle Info card, and routes to the sub-pages. The
 * actual page content lives in its own file:
 *   - setup-calibration.js  (Corvus.setupCalibration)
 *   - setup-motors.js       (Corvus.setupMotors)
 *   - setup-safety.js       (Corvus.setupSafety)
 *   - setup-parameters.js   (Corvus.setupParameters)
 *   - setup-firmware.js     (Corvus.setupFirmware)
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

  // Active view of the Setup page: "tiles" (the grid) | "calibration" |
  // "motors" | "safety" | "parameters" | "firmware".
  let activeView = "tiles";

  // Teardown handle for the currently-rendered view. `render` calls this before
  // building anything new, so re-entering Setup via the left-nav is always clean.
  let activeDestroy = null;

  // Teardown handle for the tile grid's own telemetry subscription (the Vehicle
  // Info card updates live — the firmware version arrives a few seconds after connect via
  // AUTOPILOT_VERSION, so the initial getState() snapshot would otherwise show
  // "—" forever). Paired 1:1 with `teardown()`: every subscribe has a matching
  // unsub, so the grid never leaks a listener across re-render / sub-page swaps.
  let gridUnsub = null;

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

  /** Run the active view's teardown if present, then clear it. Also tears down
   *  the tile grid's telemetry subscription. Idempotent. */
  function teardown() {
    if (typeof activeDestroy === "function") {
      try { activeDestroy(); } catch (err) { console.error("setup teardown failed:", err); }
    }
    activeDestroy = null;
    if (typeof gridUnsub === "function") {
      try { gridUnsub(); } catch (err) { console.error("setup grid teardown failed:", err); }
    }
    gridUnsub = null;
  }

  function renderTiles(container) {
    container.innerHTML = "";
    container.appendChild(S.pageHeader("Setup", "Vehicle configuration and calibration"));

    // Compact Vehicle Info card stays at the top of the grid so the operator
    // still sees which autopilot and firmware they are configuring (kept lean).
    const state = (Corvus.telemetry && Corvus.telemetry.getState()) || {};
    const infoCard = S.el("div", "page-card setup-vehicle-info");
    infoCard.appendChild(S.sectionTitle("Vehicle Info"));

    // Build the Vehicle Info rows. `dataKey` is written to each value span's
    // `dataset.infoKey` so tests (and any future caller) can locate a row by
    // key. Capture direct refs to the value spans here so the live telemetry
    // subscription below can update each row in place without a per-tick DOM
    // query.
    //
    // Connection and armed state are deliberately absent: the top bar carries
    // both permanently, on every page. Repeating them here bought nothing and
    // pushed the identity rows the card exists for further down.
    const rowDefs = [
      { key: "autopilot",    label: "Autopilot",    value: state.autopilot    || "—" },
      { key: "vehicle_type", label: "Vehicle Type", value: state.vehicle_type || "—" },
      { key: "px4_version",  label: "Firmware Version", value: state.px4_version || "—" },
    ];
    const rowValues = {};
    for (const def of rowDefs) {
      const row = S.infoRow(def.label, def.value, def.key);
      rowValues[def.key] = row.querySelector(".page-row-value");
      infoCard.appendChild(row);
    }
    container.appendChild(infoCard);

    // Tile grid: Calibration + Motors + Safety & Sensors + Parameters +
    // Firmware. Keyboard-focusable buttons so the whole tile is reachable and
    // announces as a control.
    const grid = S.el("div", "setup-tiles");
    grid.appendChild(makeTile("calibration", "sliders-horizontal", "Calibration",
      "Sensor calibration, level/airspeed, and PID autotune with live graphs."));
    grid.appendChild(makeTile("motors", "fan", "Motors",
      "Airframe geometry, motor assignment and spacing, and the output protocol."));
    grid.appendChild(makeTile("safety", "shield", "Safety & Sensors",
      "Distance and height limits, failsafe actions, rangefinder and optical flow."));
    grid.appendChild(makeTile("parameters", "list", "Parameters",
      "Download all PX4 parameters on demand, then edit any value."));
    grid.appendChild(makeTile("firmware", "cpu", "Firmware",
      "Flash PX4 firmware over a direct USB connection only."));
    container.appendChild(grid);

    // Subscribe to telemetry so the Vehicle Info rows update live. The firmware version
    // only arrives a few seconds after connect via AUTOPILOT_VERSION, so the
    // initial getState() snapshot above would otherwise stay "—" forever. The
    // subscription updates only the row values per push; the tiles and header
    // are static. Stored in `gridUnsub` so `teardown()` can release it.
    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      gridUnsub = Corvus.telemetry.subscribe((s) => {
        if (!s) return;
        const next = {
          autopilot:    s.autopilot    || "—",
          vehicle_type: s.vehicle_type || "—",
          px4_version:  s.px4_version  || "—",
        };
        for (const key of Object.keys(next)) {
          const span = rowValues[key];
          if (span && span.textContent !== next[key]) span.textContent = next[key];
        }
      });
    }

    S.refreshIcons();
  }

  /**
   * Build one setup tile. apple-design: lift on hover, scale on press, focus
   * ring, spring-eased motion (handled in CSS via transform/opacity only).
   */
  function makeTile(viewId, iconId, title, desc) {
    const btn = Corvus.ui.tile({
      className: "setup-tile",
      icon: iconId,
      title,
      desc,
      chevron: true,
      onClick: () => openView(viewId),
    });
    btn.dataset.view = viewId;
    return btn;
  }

  /** Swap the container to a sub-page. Tears the grid down first. */
  function openView(viewId) {
    if (["calibration", "motors", "safety", "parameters", "firmware"].indexOf(viewId) < 0) return;
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
    } else if (viewId === "motors") {
      activeDestroy = Corvus.setupMotors.render(container, navigateBack);
    } else if (viewId === "safety") {
      activeDestroy = Corvus.setupSafety.render(container, navigateBack);
    } else if (viewId === "parameters") {
      activeDestroy = Corvus.setupParameters.render(container, navigateBack);
    } else { // firmware
      activeDestroy = Corvus.setupFirmware.render(container, navigateBack);
    }
    S.refreshIcons();
  }

  // `teardown` is exported alongside `render` so the left-nav (sidenav.js)
  // can release the active sub-page + the grid's telemetry subscription when
  // the operator leaves the Setup page via the left-nav — not only on Back or
  // re-entry. Idempotent: safe to call when nothing is active, and safe to
  // call again after Back already tore down (no double-unsub, no throw).
  return { render, teardown };
})();
