"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.app — frontend orchestrator.

  Module load order (defined by the <script> tags in src/index.html):
    maplibre-gl, lucide, plotly-basic, ui, telemetry, notification_dedupe,
    topbar, map, instruments, panel, link, plugins, plugin-vibration,
    setup-shared, setup-calibration, setup-parameters, setup, sidenav,
    tiles, app (this file).

  Contract: every Corvus.<module> exposes a no-arg init() (some take a few
  DOM roots) and owns a narrow public API; nothing imports another module's
  internals. app.init() orchestrates them in dependency order — tiles.init
  runs AFTER map.init so the tiles panel can read Corvus.map.getMap(). The
  version is never hardcoded here; it is read from GET /api/version.
*/
Corvus.app = (function () {
  // Idempotency guard for the flight-mode selector: only repopulate when the
  // mode list returned by the backend actually changes (or once per connect).
  let loadedModesSignature = "";

  function initFlightActions() {
    const btnArm = document.getElementById("btnArm");
    const btnTakeoff = document.getElementById("btnTakeoff");
    const btnLand = document.getElementById("btnLand");
    const btnRTL = document.getElementById("btnRTL");
    const modeSel = document.getElementById("modeSelector");

    btnArm.addEventListener("click", async () => {
      const s = Corvus.telemetry.getState();
      const willArm = !s?.armed;
      const attempt = Corvus.topbar.beginCommand(willArm ? "arm" : "disarm");
      Corvus.ui.setBusy(btnArm, true);
      try {
        await Corvus.telemetry.postAction("/api/mavlink/arm", { arm: willArm });
        Corvus.topbar.succeedCommand(attempt);
        btnArm.classList.toggle("armed", willArm);
      } catch (error) {
        Corvus.topbar.failCommand(attempt);
        Corvus.topbar.notifyError(error.message || (willArm ? "Arm failed" : "Disarm failed"), attempt);
      } finally {
        Corvus.ui.setBusy(btnArm, false);
      }
    });

    btnTakeoff.addEventListener("click", () => {
      const panel = document.getElementById("takeoffPanel");
      panel.hidden = !panel.hidden;
    });

    const takeoffSlider = document.getElementById("takeoffAlt");
    const takeoffAltValue = document.getElementById("takeoffAltValue");
    takeoffSlider.addEventListener("input", () => {
      takeoffAltValue.textContent = takeoffSlider.value + " m";
    });

    document.getElementById("takeoffClose").addEventListener("click", () => {
      document.getElementById("takeoffPanel").hidden = true;
    });

    document.getElementById("takeoffConfirm").addEventListener("click", async () => {
      const alt = parseFloat(takeoffSlider.value);
      const attempt = Corvus.topbar.beginCommand(["takeoff", "arm"]);
      document.getElementById("takeoffPanel").hidden = true;
      const btn = document.getElementById("takeoffConfirm");
      Corvus.ui.setBusy(btn, true);
      try {
        await Corvus.telemetry.postAction("/api/mavlink/takeoff", { altitude: alt });
        Corvus.topbar.succeedCommand(attempt);
      } catch (error) {
        Corvus.topbar.failCommand(attempt);
        Corvus.topbar.notifyError(error.message || "Takeoff rejected by autopilot", attempt);
      } finally {
        Corvus.ui.setBusy(btn, false);
      }
    });

    btnLand.addEventListener("click", async () => {
      const attempt = Corvus.topbar.beginCommand("land");
      Corvus.ui.setBusy(btnLand, true);
      try {
        await Corvus.telemetry.postAction("/api/mavlink/land", {});
        Corvus.topbar.succeedCommand(attempt);
      } catch (error) {
        Corvus.topbar.failCommand(attempt);
        Corvus.topbar.notifyError(error.message || "Land rejected", attempt);
      } finally {
        Corvus.ui.setBusy(btnLand, false);
      }
    });

    btnRTL.addEventListener("click", async () => {
      const attempt = Corvus.topbar.beginCommand("rtl");
      Corvus.ui.setBusy(btnRTL, true);
      try {
        await Corvus.telemetry.postAction("/api/mavlink/rtl", {});
        Corvus.topbar.succeedCommand(attempt);
      } catch (error) {
        Corvus.topbar.failCommand(attempt);
        Corvus.topbar.notifyError(error.message || "RTL rejected", attempt);
      } finally {
        Corvus.ui.setBusy(btnRTL, false);
      }
    });

    modeSel.addEventListener("change", async () => {
      const mode = modeSel.value;
      if (!mode) return;
      const attempt = Corvus.topbar.beginCommand("mode");
      modeSel.disabled = true;
      try {
        await Corvus.telemetry.postAction("/api/mavlink/mode", { mode });
        Corvus.topbar.succeedCommand(attempt);
      } catch (error) {
        Corvus.topbar.failCommand(attempt);
        Corvus.topbar.notifyError(error.message || `Mode ${mode} rejected`, attempt);
      } finally {
        modeSel.disabled = false;
      }
    });

    // --- PLAN: fly-to-points ("Punktabflug") mission planning ---
    // Contracts (owned by the map + backend agents):
    //   Corvus.map.setWaypointMode(bool), .getWaypoints(): [{lat,lon}],
    //   .clearWaypoints(), .onWaypointsUpdate(cb)
    //   POST /api/mavlink/gotopoints { points:[{lat,lon,alt_agl}] }, alt_agl in [1,50].
    const btnPlan = document.getElementById("btnPlan");
    const planPanel = document.getElementById("planPanel");
    const planAlt = document.getElementById("planAlt");
    const planAltValue = document.getElementById("planAltValue");
    const planCount = document.getElementById("planCount");
    const planFly = document.getElementById("planFly");
    const planClear = document.getElementById("planClear");

    // The waypoint API is shipped by the map agent in parallel; guard once so a
    // not-yet-merged map module degrades gracefully (button disabled) instead
    // of crashing the flight-actions bar.
    const mapWaypointApi = !!(Corvus.map
      && typeof Corvus.map.setWaypointMode === "function"
      && typeof Corvus.map.getWaypoints === "function"
      && typeof Corvus.map.clearWaypoints === "function"
      && typeof Corvus.map.onWaypointsUpdate === "function");

    function getPlanPoints() {
      return mapWaypointApi ? Corvus.map.getWaypoints() : [];
    }

    // Safer MVP gate: FLY requires connected + armed + >=1 waypoint. The bridge
    // arms/starts the mission; refusing to dispatch against a parked aircraft
    // avoids an autopilot rejection the operator would have to clear.
    function refreshPlanFly(pts) {
      const s = Corvus.telemetry.getState() || {};
      const p = pts || getPlanPoints();
      planFly.disabled = !(s.connected && s.armed && p.length >= 1);
    }

    function refreshPlanCount(pts) {
      const p = pts || getPlanPoints();
      const n = p.length;
      planCount.textContent = `${n} POINT${n === 1 ? "" : "S"}`;
      planCount.classList.toggle("zero", n === 0);
      refreshPlanFly(p);
    }

    function setPlanPanelVisible(visible) {
      planPanel.hidden = !visible;
    }

    function setPlanMode(enabled) {
      if (!mapWaypointApi) return;
      btnPlan.classList.toggle("active", enabled);
      Corvus.map.setWaypointMode(enabled);
      if (enabled) setPlanPanelVisible(true);
    }

    btnPlan.addEventListener("click", () => {
      if (btnPlan.disabled) return;
      setPlanMode(!btnPlan.classList.contains("active"));
    });

    // Less destructive close: keep waypoints, just hide the panel.
    document.getElementById("planClose").addEventListener("click", () => {
      setPlanPanelVisible(false);
    });

    planAlt.addEventListener("input", () => {
      planAltValue.textContent = planAlt.value + " m AGL";
    });

    planClear.addEventListener("click", () => {
      if (mapWaypointApi) Corvus.map.clearWaypoints();
    });

    planFly.addEventListener("click", async () => {
      if (!mapWaypointApi) return;
      const pts = Corvus.map.getWaypoints();
      if (!pts.length) return;
      const alt = parseFloat(planAlt.value);
      // alt_agl is bounded [1, 50] by the slider; clamp defensively for safety.
      const altAgl = Math.min(50, Math.max(1, isFinite(alt) ? alt : 10));
      const points = pts.map((p) => ({ lat: p.lat, lon: p.lon, alt_agl: altAgl }));
      const attempt = Corvus.topbar.beginCommand("gotopoints");
      Corvus.ui.setBusy(planFly, true);
      try {
        await Corvus.telemetry.postAction("/api/mavlink/gotopoints", { points });
        Corvus.topbar.succeedCommand(attempt);
        // Mission accepted: exit planning cleanly and drop the plan.
        Corvus.map.setWaypointMode(false);
        Corvus.map.clearWaypoints();
        btnPlan.classList.remove("active");
        setPlanPanelVisible(false);
      } catch (error) {
        Corvus.topbar.failCommand(attempt);
        Corvus.topbar.notifyError(error.message || "Fly to points rejected", attempt);
      } finally {
        Corvus.ui.setBusy(planFly, false);
        refreshPlanFly(); // re-gate from live state (keep the plan so the operator can retry)
      }
    });

    if (mapWaypointApi) {
      Corvus.map.onWaypointsUpdate((pts) => {
        refreshPlanCount(pts);
        if (pts.length > 0 && btnPlan.classList.contains("active")) {
          setPlanPanelVisible(true); // auto-show on first waypoint while planning
        }
        if (pts.length === 0) planFly.disabled = true;
      });
    }

    Corvus.telemetry.requestJson("/api/mavlink/modes").then((data) => {
      refreshModesFromData(data);
    }).catch((error) => Corvus.topbar.notifyError(error.message || "Could not load flight modes"));

    Corvus.telemetry.subscribe((s) => {
      btnArm.classList.toggle("armed", !!s?.armed);
      const span = btnArm.querySelector("span");
      if (span) span.textContent = s?.armed ? "DISARM" : "ARM";
      const currentMode = s?.mode || "";
      modeSel.value = currentMode;
      // PLAN mirrors the other flight buttons: only actionable against a
      // connected vehicle. On disconnect, also exit planning so the UI never
      // strands the operator in a click-to-add mode they can no longer commit.
      btnPlan.disabled = !s?.connected;
      if (!s?.connected && btnPlan.classList.contains("active")) setPlanMode(false);
      refreshPlanFly();
    });
  }

  /**
   * Repopulate the flight-mode selector from GET /api/mavlink/modes.
   * Idempotent: only re-renders when the mode list signature changes, so a
   * re-fetch triggered on every connect does not thrash the selector or lose
   * the operator's current selection. Exposed for Corvus.link to call after a
   * transition to "connected" so the operator sees exactly the modes the
   * connected firmware supports (PX4 target awareness).
   */
  function refreshModes() {
    return Corvus.telemetry.requestJson("/api/mavlink/modes")
      .then(refreshModesFromData)
      .catch(() => { /* best-effort: keep the existing selector as-is */ });
  }

  function refreshModesFromData(data) {
    const modes = (data && data.modes) || [];
    const sig = modes.join(",");
    if (sig === loadedModesSignature) return;   // idempotent
    loadedModesSignature = sig;
    const modeSel = document.getElementById("modeSelector");
    if (!modeSel) return;
    const current = modeSel.value;
    // Keep only the placeholder option; drop previously loaded modes.
    Array.from(modeSel.querySelectorAll("option:not([value=''])")).forEach((o) => o.remove());
    modes.forEach((m) => {
      const opt = document.createElement("option");
      opt.value = m;
      opt.textContent = m;
      modeSel.appendChild(opt);
    });
    // Restore the selection if the firmware still offers it.
    if (current && Array.from(modeSel.options).some((o) => o.value === current)) {
      modeSel.value = current;
    }
  }

  function init() {
    // Apply the cached accent synchronously (no FOUC) before the map/UI paint,
    // then let the backend config override it as the authoritative source.
    Corvus.theme.applySaved();
    Corvus.telemetry.requestJson("/api/config").then((res) => {
      const accent = res && res.config && res.config.theme && res.config.theme.accent;
      if (accent) Corvus.theme.setAccent(accent);
    }).catch(() => {});

    Corvus.topbar.init();
    Corvus.sidenav.init();
    Corvus.map.init(document.getElementById("map"), document.getElementById("mapControls"),
      document.getElementById("layersPopover"));
    Corvus.instruments.init(document.getElementById("flightOverlay"));
    Corvus.panel.init();
    Corvus.link.init();
    initFlightActions();
    Corvus.tiles.init(document.getElementById("tilesTrigger"),
      document.getElementById("tilesPopover"));

    Corvus.telemetry.connect();
    if (window.lucide && lucide.createIcons) lucide.createIcons();

    Corvus.telemetry.requestJson("/api/version").then((v) => {
      document.title = `CORVUS GCS v${v.version}`;
    }).catch(() => {});

    let userToggled = false;
    document.getElementById("panelHandle").addEventListener("click", () => { userToggled = true; });
    window.addEventListener("resize", () => {
      if (userToggled) return;
      const panel = document.getElementById("rightPanel");
      const collapsed = panel.classList.contains("collapsed");
      if (window.innerWidth < 960 && !collapsed) Corvus.panel.toggle();
      else if (window.innerWidth >= 1280 && collapsed) Corvus.panel.toggle();
    });
    window.dispatchEvent(new Event("resize"));
  }

  return {
    init,
    refreshModes,
    notifyError: (msg, attempt) => Corvus.topbar.notifyError(msg, attempt),
  };
})();

document.addEventListener("DOMContentLoaded", Corvus.app.init);
