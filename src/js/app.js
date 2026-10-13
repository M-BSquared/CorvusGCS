"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.app — frontend orchestrator.

  Module load order (defined by the <script> tags in src/index.html):
    maplibre-gl, lucide, plotly-basic, i18n, ui, telemetry, notification_dedupe,
    topbar, map, instruments, panel, link, plugins,
    setup-shared, setup-calibration, setup-parameters, setup, sidenav,
    joystick, checklist, checklist-editor, tiles, update, app (this file).

  Contract: every Corvus.<module> exposes a no-arg init() (some take a few
  DOM roots) and owns a narrow public API; nothing imports another module's
  internals. app.init() orchestrates them in dependency order — tiles.init
  runs AFTER map.init so the tiles panel can read Corvus.map.getMap(). The
  version is never hardcoded here; it is read from GET /api/version.
*/
Corvus.app = (function () {
  /* The metres a typed altitude stands for, or null while the box holds
     nothing usable. `toSi` converts from the display unit. Clamped to the
     slider's range so a typed 500 asks for the top of it rather than a height
     the backend refuses, and rounded to the centimetre so 100 ft is not sent
     as 30.480000000000004. */
  function altitudeFromEntry(text, toSi, min, max) {
    const typed = String(text).trim();
    if (typed === "") return null;
    const value = Number(typed);
    if (!isFinite(value)) return null;
    const metres = Math.min(max, Math.max(min, toSi(value)));
    return Math.round(metres * 100) / 100;
  }

  /* Whether the box holds a number that stands as typed: not empty, not
     unreadable, and inside the range the box itself shows. `validity` is the
     input's ValidityState. Step is deliberately not asked about, since
     7.5 m is a fine altitude. */
  function entryIsClear(text, validity) {
    if (String(text).trim() === "") return false;
    return !(validity.badInput || validity.rangeUnderflow || validity.rangeOverflow);
  }

  /* The takeoff slider is logarithmic: a fixed distance along it is the same
     percentage of height, so the low heights, where a metre matters, get the
     room and 100 m is not a hair's breadth from 120. Its own number is a
     position from 0 to `span`; the metres come from here.

     A height is snapped to a whole metre up to 50 m and to 5 m above it,
     because a thumb cannot tell 83 from 84 m and nobody wants to. */
  const ALTITUDE_COARSE_FROM_M = 50;
  const ALTITUDE_COARSE_STEP_M = 5;

  function altitudeToPosition(metres, min, max, span) {
    const m = Math.min(max, Math.max(min, metres));
    return Math.round(span * Math.log(m / min) / Math.log(max / min));
  }

  function positionToAltitude(position, min, max, span) {
    const raw = min * Math.pow(max / min, position / span);
    const step = raw >= ALTITUDE_COARSE_FROM_M ? ALTITUDE_COARSE_STEP_M : 1;
    return Math.min(max, Math.max(min, Math.round(raw / step) * step));
  }

  /* One arrow-key press: to the next whole metre (or 5 m above 50) in that
     direction, from wherever the height is, including a typed 30.48. The
     slider's own keys would move a thousandth of the track, which at 10 m is
     less than the snapping, so the key would do nothing. */
  function nudgeAltitude(metres, direction, min, max) {
    const up = direction > 0;
    const coarse = up ? metres >= ALTITUDE_COARSE_FROM_M : metres > ALTITUDE_COARSE_FROM_M;
    const step = coarse ? ALTITUDE_COARSE_STEP_M : 1;
    const next = up
      ? (Math.floor(metres / step + 1e-9) + 1) * step
      : (Math.ceil(metres / step - 1e-9) - 1) * step;
    return Math.min(max, Math.max(min, next));
  }

  /* Where the vehicle is, for offering only the one of TAKEOFF and LAND that
     applies: "air", "ground", or "" when that is not known (or nothing is
     connected), which greys neither. Read like _is_airborne in
     corvus/mavlink_flight.py: the vehicle's own landed state first, and its
     height above home only when it publishes none.

     The two answers are not equally firm, on purpose. "ground" needs a fact
     (disarmed, or the autopilot saying ON_GROUND), because it is what greys
     LAND, and LAND must never be withheld from an aircraft that is flying on
     the strength of a height guess. "air" may come from the guess: the worst
     it does is grey TAKEOFF for a vehicle hovering low. */
  const AIRBORNE_LANDED_STATES = [2, 3, 4];   // IN_AIR, TAKEOFF, LANDING
  const LANDED_ON_GROUND = 1;
  const AIRBORNE_FALLBACK_M = 1.5;

  function flightPhase(s) {
    if (!s || !s.connected) return "";
    const landed = Number(s.landed_state);
    if (AIRBORNE_LANDED_STATES.indexOf(landed) !== -1) return "air";
    if (!s.armed || landed === LANDED_ON_GROUND) return "ground";
    return Number(s.altitude_agl) > AIRBORNE_FALLBACK_M ? "air" : "";
  }

  // Idempotency guard for the flight-mode selector: only repopulate when the
  // mode list returned by the backend actually changes (or once per connect).
  let loadedModesSignature = "";

  function initFlightActions() {
    const btnArm = document.getElementById("btnArm");
    const btnTakeoff = document.getElementById("btnTakeoff");
    const btnLand = document.getElementById("btnLand");
    const btnRTL = document.getElementById("btnRTL");

    /* In the air TAKEOFF is greyed, on the ground LAND is (see flightPhase).
       A button with a command in flight is left alone: setBusy holds it
       disabled until the answer, and clearing that would allow a second
       press. */
    function showFlightPhase(s) {
      const phase = flightPhase(s);
      offer(btnTakeoff, phase !== "air", "Takeoff", "Takeoff: already in the air");
      offer(btnLand, phase !== "ground", "Land", "Land: on the ground");
      if (phase === "air") {
        const panel = document.getElementById("takeoffPanel");
        if (!panel.hidden) panel.hidden = true;
      }
    }

    /* Runs on every telemetry frame; only a real change is written, since an
       unchanged one still costs the bar a style pass. */
    function offer(button, on, title, why) {
      if (button.classList.contains("is-busy")) return;
      if (button.disabled !== !on) button.disabled = !on;
      const tip = on ? title : why;
      if (button.title !== tip) button.title = tip;
    }
    /* The mode picker is a plain <select> and stays one: the app's dropdown
       is put over every select in the page by ui.watchSelects() (called from
       init below), and it keeps the element as the state, so everything here
       still reads modeSel.value and its "change" event. */
    const modeSel = document.getElementById("modeSelector");

    /* Corvus is tuned for aircraft. A rover or boat keeps ARM, RETURN, PLAN
       and the mode picker, and loses what has no meaning on the ground:
       TAKEOFF, LAND and every altitude. Worded from the telemetry snapshot so
       the bar changes on the heartbeat that names the vehicle. */
    const isGround = () => !!(Corvus.capabilities && Corvus.capabilities.isGround());
    let shownGround = null;
    function adaptToVehicle(ground) {
      if (ground === shownGround) return;
      shownGround = ground;
      btnTakeoff.hidden = ground;
      btnLand.hidden = ground;
      if (ground) {
        document.getElementById("takeoffPanel").hidden = true;
      }
      const btnPlanEl = document.getElementById("btnPlan");
      btnPlanEl.title = ground ? "Plan drive-to-points route" : "Plan fly-to-points mission";
      modeSel.title = ground ? "Drive Mode" : "Flight Mode";
      const planTitle = document.getElementById("planTitle");
      if (planTitle) planTitle.textContent = ground ? "DRIVE TO POINTS" : "FLY TO POINTS";
      const altRow = document.getElementById("planAltRow");
      if (altRow) altRow.hidden = ground;
      const flyLabel = document.getElementById("planFlyLabel");
      if (flyLabel) flyLabel.textContent = ground ? "DRIVE" : "FLY";
      btnRTL.title = ground ? "Return to start" : "Return to Launch";
    }

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

    // The two flight cards each open under their own button and never at the
    // same time. Laid out from the buttons' layout offsets, not their
    // bounding rects: a hovered or pressed button carries a transform, and a
    // rect would follow it.
    const FLIGHT_CARDS = [
      { panel: "takeoffPanel", button: "btnTakeoff" },
      { panel: "planPanel", button: "btnPlan" },
    ];

    function anchorFlightCard(card) {
      const panel = document.getElementById(card.panel);
      const button = document.getElementById(card.button);
      if (!panel || !button || panel.hidden) return;
      const host = panel.offsetParent;
      const at = Corvus.ui.offsetWithin(button, host);
      const maxLeft = host ? Math.max(0, host.clientWidth - panel.offsetWidth - 14) : at.left;
      panel.style.left = `${Math.max(0, Math.min(at.left, maxLeft))}px`;
      panel.style.top = `${at.top + button.offsetHeight + 16}px`;
    }

    function openFlightCard(id) {
      FLIGHT_CARDS.forEach((card) => {
        const panel = document.getElementById(card.panel);
        if (!panel) return;
        if (card.panel === id) {
          panel.hidden = false;
          anchorFlightCard(card);
        } else {
          panel.hidden = true;
        }
      });
    }

    function reanchorFlightCards() {
      FLIGHT_CARDS.forEach(anchorFlightCard);
    }
    window.addEventListener("resize", reanchorFlightCards);
    const flightBarEl = document.getElementById("flightActions");
    if (flightBarEl && typeof ResizeObserver === "function") {
      new ResizeObserver(reanchorFlightCards).observe(flightBarEl);
    }

    const takeoffSlider = document.getElementById("takeoffAlt");
    const takeoffInput = document.getElementById("takeoffAltInput");
    const takeoffUnit = document.getElementById("takeoffAltUnit");

    btnTakeoff.addEventListener("click", () => {
      const panel = document.getElementById("takeoffPanel");
      if (panel.hidden) {
        openFlightCard("takeoffPanel");
        takeoffInput.focus();
        takeoffInput.select();
      } else {
        panel.hidden = true;
      }
    });

    // The altitude lives here in metres, which is what the bridge is sent; the
    // slider and the box are two views of it, the box in the display unit. The
    // slider is read only when it is moved, never back from what was set on it:
    // its position is rounded, and a typed 100 ft (30.48 m) must stay 30.48.
    const takeoffMin = Number(takeoffSlider.dataset.minM);
    const takeoffMax = Number(takeoffSlider.dataset.maxM);
    const takeoffSpan = Number(takeoffSlider.max);
    const sliderPosition = (metres) => altitudeToPosition(metres, takeoffMin, takeoffMax, takeoffSpan);
    let takeoffAlt = 10;
    const showTakeoffAlt = () => {
      const shown = Corvus.units.formatLength(takeoffAlt, { bare: true });
      takeoffSlider.value = sliderPosition(takeoffAlt);
      takeoffSlider.setAttribute("aria-valuetext", `${shown} ${Corvus.units.lengthSymbol()}`);
      takeoffInput.min = Corvus.units.formatLength(takeoffMin, { bare: true });
      takeoffInput.max = Corvus.units.formatLength(takeoffMax, { bare: true });
      takeoffInput.value = shown;
      takeoffUnit.textContent = Corvus.units.lengthSymbol();
    };
    takeoffSlider.addEventListener("input", () => {
      takeoffAlt = positionToAltitude(Number(takeoffSlider.value), takeoffMin, takeoffMax, takeoffSpan);
      showTakeoffAlt();
    });
    takeoffSlider.addEventListener("keydown", (event) => {
      const direction = { ArrowRight: 1, ArrowUp: 1, ArrowLeft: -1, ArrowDown: -1 }[event.key];
      if (!direction) return;
      event.preventDefault();
      takeoffAlt = nudgeAltitude(takeoffAlt, direction, takeoffMin, takeoffMax);
      showTakeoffAlt();
    });
    // Taken per keystroke so the slider and CONFIRM follow at once, but the
    // box is left alone while typing: rewriting it would turn a half-typed "4"
    // on the way to "45" into something else under the cursor. It is tidied
    // when the box is left, which also restores an emptied box.
    takeoffInput.addEventListener("input", () => {
      const metres = altitudeFromEntry(takeoffInput.value, Corvus.units.lengthToSi, takeoffMin, takeoffMax);
      if (metres == null) return;
      takeoffAlt = metres;
      takeoffSlider.value = sliderPosition(metres);
    });
    takeoffInput.addEventListener("change", showTakeoffAlt);
    // Enter is CONFIRM TAKEOFF, through the button so it takes the same path
    // (and stays dead while a command is in flight). Only on a number that
    // stands as typed: from an empty or out-of-range box it just tidies the
    // box, so a "500" never becomes a 120 m takeoff the operator has not seen.
    takeoffInput.addEventListener("keydown", (event) => {
      if (event.key !== "Enter" || event.repeat) return;
      event.preventDefault();
      if (!entryIsClear(takeoffInput.value, takeoffInput.validity)) {
        showTakeoffAlt();
        return;
      }
      document.getElementById("takeoffConfirm").click();
    });
    window.addEventListener("corvus:unitschange", showTakeoffAlt);
    showTakeoffAlt();

    document.getElementById("takeoffClose").addEventListener("click", () => {
      document.getElementById("takeoffPanel").hidden = true;
    });

    document.getElementById("takeoffConfirm").addEventListener("click", async () => {
      const alt = takeoffAlt;
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
        Corvus.topbar.notifyError(error.message || "Return rejected", attempt);
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
      const blocked = !(s.connected && s.armed && p.length >= 1);
      if (planFly.disabled !== blocked) planFly.disabled = blocked;
    }

    function refreshPlanCount(pts) {
      const p = pts || getPlanPoints();
      const n = p.length;
      planCount.textContent = `${n} POINT${n === 1 ? "" : "S"}`;
      planCount.classList.toggle("zero", n === 0);
      refreshPlanFly(p);
    }

    function setPlanPanelVisible(visible) {
      if (visible) openFlightCard("planPanel");
      else planPanel.hidden = true;
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

    // The slider itself stays in whole metres, which is what the bridge is
    // sent; only its caption follows the display unit.
    const showPlanAlt = () => {
      planAltValue.textContent = `${Corvus.units.formatLength(planAlt.value)} AGL`;
    };
    planAlt.addEventListener("input", showPlanAlt);
    window.addEventListener("corvus:unitschange", showPlanAlt);
    showPlanAlt();

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
      const points = pts.map((p) => (isGround()
        ? { lat: p.lat, lon: p.lon }
        : { lat: p.lat, lon: p.lon, alt_agl: altAgl }));
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
        Corvus.topbar.notifyError(error.message || (isGround() ? "Drive to points rejected" : "Fly to points rejected"), attempt);
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

    // --- Map context menu: act on a clicked position ---
    // Corvus.map owns the menu surface (where it opens, how it tracks the
    // ground point, when it closes); the rows are registered here because they
    // are flight commands and everything a flight command needs — the topbar's
    // attempt tracking, the notification path, the live armed/connected gate —
    // already lives in this module.
    //
    // Both rows are gated the same way the buttons above them are, and both say
    // why when they are unavailable. A disabled row that explains itself is the
    // point: an operator who clicks the map on a parked aircraft learns what is
    // missing instead of finding the menu mysteriously inert.
    if (Corvus.map && typeof Corvus.map.setContextActions === "function") {
      const state = () => Corvus.telemetry.getState() || {};

      // Altitude comes from the PLAN panel's slider rather than a constant of
      // its own: it is the app's one "how high should it fly" control, the
      // operator can change it, and the menu shows the value it will use so it
      // is never a hidden parameter.
      function planAltAgl() {
        const alt = parseFloat(planAlt.value);
        return Math.min(50, Math.max(1, isFinite(alt) ? alt : 10));
      }

      async function runMapCommand(name, url, body, failText) {
        const attempt = Corvus.topbar.beginCommand(name);
        try {
          await Corvus.telemetry.postAction(url, body);
          Corvus.topbar.succeedCommand(attempt);
        } catch (error) {
          Corvus.topbar.failCommand(attempt);
          Corvus.topbar.notifyError(error.message || failText, attempt);
        }
      }

      Corvus.map.setContextActions([
        {
          id: "goto",
          label: () => (isGround() ? "Drive to this point" : "Fly to this point"),
          icon: "navigation",
          // The same gate as the PLAN panel's FLY, for the same reason: the
          // bridge arms and starts a mission, and dispatching that against a
          // parked aircraft earns an autopilot rejection the operator then has
          // to clear.
          enabled: () => {
            const s = state();
            return !!(s.connected && s.armed);
          },
          note: (_point, enabled) => {
            if (!enabled) {
              return state().connected ? "Arm the vehicle first" : "Not connected";
            }
            return isGround() ? "" : `${Corvus.units.formatLength(planAltAgl())} AGL`;
          },
          run: (point) => runMapCommand(
            "gotopoints", "/api/mavlink/gotopoints",
            { points: [isGround()
              ? { lat: point.lat, lon: point.lng }
              : { lat: point.lat, lon: point.lng, alt_agl: planAltAgl() }] },
            isGround() ? "Drive to point rejected" : "Fly to point rejected",
          ),
        },
        {
          id: "sethome",
          label: "Set home here",
          icon: "house",
          // No armed gate: relocating home is how RTL gets redirected in
          // flight, so refusing it in the air would remove the case it is most
          // needed for. It only needs a vehicle to talk to.
          enabled: () => !!state().connected,
          note: (_point, enabled) =>
            (enabled ? "Moves the return target" : "Not connected"),
          run: (point) => runMapCommand(
            "sethome", "/api/mavlink/sethome",
            { lat: point.lat, lon: point.lng },
            "Set home rejected",
          ),
        },
      ]);
    }

    Corvus.telemetry.requestJson("/api/mavlink/modes").then((data) => {
      refreshModesFromData(data);
    }).catch((error) => Corvus.topbar.notifyError(error.message || "Could not load flight modes"));

    Corvus.telemetry.subscribe((s) => {
      btnArm.classList.toggle("armed", !!s?.armed);
      const span = btnArm.querySelector("span");
      const armCaption = s?.armed ? "DISARM" : "ARM";
      if (span && span.textContent !== armCaption) span.textContent = armCaption;
      adaptToVehicle(Corvus.capabilities ? Corvus.capabilities.isGround(s) : false);
      showFlightPhase(s);
      showVehicleMode(modeSel, s?.mode || "", s?.mode_label || "");
      // PLAN mirrors the other flight buttons: only actionable against a
      // connected vehicle. On disconnect, also exit planning so the UI never
      // strands the operator in a click-to-add mode they can no longer commit.
      if (btnPlan.disabled !== !s?.connected) btnPlan.disabled = !s?.connected;
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

  /**
   * Show the vehicle's mode in the picker.
   *
   * Setting .value alone left the themed dropdown's label behind: it only
   * follows a "change" event or a DOM mutation, and a programmatic value is
   * neither, so the flight bar read SELECT MODE while the vehicle held. A mode
   * the list does not carry (one the firmware reports but cannot be commanded
   * into) gets a disabled row of its own rather than a blank picker, named
   * by `label`, the word the top bar shows for it.
   */
  function showVehicleMode(modeSel, mode, label) {
    if (!modeSel) return;
    const options = Array.from(modeSel.options || []);
    let reported = options.find((o) => o.dataset && o.dataset.reported === "true");
    const offered = options.some((o) => o.value === mode && o !== reported);
    if (mode && !offered) {
      if (!reported) {
        reported = document.createElement("option");
        reported.dataset.reported = "true";
        reported.disabled = true;
        modeSel.appendChild(reported);
      }
      if (reported.value !== mode) reported.value = mode;
      const text = label || mode;
      if (reported.textContent !== text) reported.textContent = text;
    } else if (reported) {
      reported.remove();
    }
    if (modeSel.value !== mode) modeSel.value = mode;
    if (modeSel.corvusSelect) modeSel.corvusSelect.refresh();
  }

  /**
   * Fill the picker from GET /api/mavlink/modes. Each row's value is the name
   * a mode change sends; its text is the word the backend's dialect gives it
   * ("POSITION" for PX4's POSCTL), the same one the top bar shows.
   */
  function refreshModesFromData(data) {
    const modes = (data && data.modes) || [];
    const labels = (data && data.labels) || {};
    const sig = JSON.stringify([modes, labels]);
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
      opt.textContent = labels[m] || m;
      modeSel.appendChild(opt);
    });
    // Restore the selection if the firmware still offers it.
    if (current && Array.from(modeSel.options).some((o) => o.value === current)) {
      modeSel.value = current;
    }
  }

  function init() {
    // Apply the cached theme and interface size synchronously (no FOUC, and no
    // relayout in front of the operator) before the map/UI paint, then let the
    // backend config override both as the authoritative source. The inline
    // script in index.html has usually done this already; repeating it here
    // also covers the case where that script could not read storage.
    Corvus.theme.applySaved();
    Corvus.scale.applySaved();
    // The language last used on this machine, before the config answers. The
    // page is held back by index.html until it is in, so nothing paints in the
    // wrong one first.
    Corvus.i18n.init();
    Corvus.ui.guardSelection();
    Corvus.telemetry.requestJson("/api/config").then((res) => {
      const cfg = (res && res.config) || {};
      // The config is the authority, like for the theme. Not while the first
      // start setup is open: it picks the language itself, from the browser.
      if (!(Corvus.welcome && Corvus.welcome.isOpen())) Corvus.i18n.use(Corvus.i18n.fromConfig(cfg));
      const name = Corvus.theme.fromConfig(cfg);
      if (name) Corvus.theme.setTheme(name);
      const scale = Corvus.scale.fromConfig(cfg);
      if (scale) Corvus.scale.setScale(scale);
      const units = Corvus.units.fromConfig(cfg);
      if (units) Corvus.units.set(units);
      // Off unless the config says otherwise: a control that can move the
      // aircraft is opt-in, and an unreachable backend must leave both off
      // rather than guess from a cached value.
      Corvus.joystick.setEnabled(!!(cfg.controls && cfg.controls.virtual_joystick));
      Corvus.joystick.setGamepadEnabled(!!(cfg.controls && cfg.controls.gamepad));
      Corvus.joystick.setKeysEnabled(!!(cfg.controls && cfg.controls.arrow_keys));
      Corvus.joystick.setWasdEnabled(!!(cfg.controls && cfg.controls.wasd_keys));
      // How much stick a held key is worth. Unlike the switches this is not a
      // decision to fly or not, so an absent key is the module's default
      // rather than "off" — setKeyGain() clamps and falls back on its own.
      Corvus.joystick.setKeyGain(cfg.controls && cfg.controls.key_gain);
      // Optional operator branding in the top bar; absent by default, and the
      // top bar keeps the value until it builds itself on the first state.
      Corvus.topbar.setCompanyLogo((cfg.branding && cfg.branding.logo) || "");
      // The caption status dots. Off unless the config says otherwise, like
      // the control switches above: the backend is the authority, and an
      // absent key is the default (no dots), not the cached value.
      Corvus.topbar.setStatusDots(!!(cfg.ui && cfg.ui.topbar_status_dots));
      // The ALTITUDE block's reference. AMSL unless the config asks for the
      // altitude above home.
      Corvus.topbar.setAltitudeRef(cfg.ui && cfg.ui.topbar_altitude);
      // The severity marks on notifications. Off unless the config says
      // otherwise, like the control switches above: the icon and its colour
      // already carry the level, so an absent key leaves the marks off rather
      // than drawing a rule nobody asked for.
      Corvus.topbar.setNotificationMarks(!!(cfg.ui && cfg.ui.notification_marks));
      // The Home flight bar giving up its captions EARLY, against a share of
      // the map rather than against the room it has. Off unless the config
      // asks, like the control switches above: what the bar does by default is
      // the planner's own rule — full size until the row will not fit — and a
      // config that has never been asked must not shrink a bar nobody asked to
      // shrink.
      Corvus.map.setFlightBarShrink(!!(cfg.ui && cfg.ui.flight_bar_shrink));
      // The flown track. Earlier flights get their own colour unless the
      // config turns that off, and the track the last session left is kept
      // unless the config asks for it to go on every restart.
      Corvus.map.setTrackOptions({
        earlierFlights: !(cfg.ui && cfg.ui.track_earlier_flights === false),
        clearOnRestart: !!(cfg.ui && cfg.ui.track_clear_on_restart),
      }, true);
      // The flight compass is north up unless the config locks it nose up.
      Corvus.instruments.setNoseUp(!!(cfg.ui && cfg.ui.compass_nose_up));
      // The flight HUD is on the Home map unless the config takes it off.
      Corvus.hudPanel.setShown(!(cfg.ui && cfg.ui.flight_hud === false));
      // The optional Mission entry in the left rail. Off unless the config
      // asks for it, and asked for here rather than inside sidenav.init()
      // because the rail is built before this fetch can land — the rail
      // re-renders itself when the answer arrives.
      Corvus.sidenav.setMissionEnabled(!!(cfg.ui && cfg.ui.mission_page));
      // Where camera and terminal windows open in the desktop app. Off unless
      // the config asks: by default each is a window of its own at once.
      if (Corvus.popouts) Corvus.popouts.setInApp(!!(cfg.ui && cfg.ui.windows_in_app));
      // Terminal windows are frosted glass unless the config asks for solid
      // ones. Where nothing behind them can be blurred they are solid anyway.
      if (Corvus.termWindows) Corvus.termWindows.setFrosted(!(cfg.ui && cfg.ui.solid_terminals));
      // The preflight checklist and its Home window. Off unless the config
      // asks for it, like the Mission planner.
      Corvus.checklist.fromConfig(cfg);
      // The side panel's CONSOLE and SSH tabs are there unless the config
      // takes them away, and a plugin gets a tab of its own only when the
      // config allows it. Plugins that load later are sorted by the same rule.
      Corvus.panel.setTabHidden("console", !!(cfg.ui && cfg.ui.console_tab === false));
      Corvus.panel.setTabHidden("ssh", !!(cfg.ui && cfg.ui.ssh_tab === false));
      Corvus.plugins.setTabsAllowed(!!(cfg.ui && cfg.ui.plugin_tabs));
    }).catch(() => {});

    /* Before any module builds its DOM: this replaces the operating system's
       popup on every <select> in the app with the themed dropdown, both the
       ones already in index.html and the ones the setup screens, modals and
       plugins create later. */
    Corvus.ui.watchSelects();

    Corvus.topbar.init();
    Corvus.sidenav.init();
    Corvus.map.init(document.getElementById("map"), document.getElementById("mapControls"));
    // AFTER map.init: the stored geofence is drawn on that map as an overlay.
    if (Corvus.geofence) Corvus.geofence.start();
    Corvus.instruments.init(document.getElementById("flightOverlay"));
    // AFTER instruments.init: the panel re-parents the built instruments into
    // its collapsible body, so they have to exist first.
    Corvus.hudPanel.init(document.getElementById("flightOverlay"));
    Corvus.panel.init();
    // AFTER panel.init: it is panel.initFuture that calls Corvus.plugins.init,
    // and the loader wants the plugin api (and its root element) in place. The
    // fetch is not awaited — plugins register as their scripts arrive and the
    // grid re-renders itself, so boot never waits on the plugin folder.
    Corvus.plugins.loadInstalled();
    Corvus.link.init();
    initFlightActions();
    // The offline-map panel is a modal now (it mounts itself to <body>), so
    // only the trigger is wired here — there is no anchored popover element.
    Corvus.tiles.init(document.getElementById("tilesTrigger"));
    // Builds the pad hidden; the /api/config read above decides which of its
    // surfaces are shown, and Settings toggles them live from there on.
    Corvus.joystick.init(document.getElementById("joystickPad"));
    // Built hidden too; the /api/config read above decides whether it shows.
    Corvus.checklist.init(document.getElementById("checklistPanel"));

    Corvus.telemetry.connect();
    // Every vehicle the station hears, on the shared event stream. Started
    // with the telemetry so the stream's topic set settles at boot.
    if (Corvus.fleet) Corvus.fleet.start();
    Corvus.ui.refreshIcons();

    Corvus.telemetry.requestJson("/api/version").then((v) => {
      document.title = `CORVUS GCS v${v.version}`;
    }).catch(() => {});

    // The first start setup, on a station that has none yet. The background
    // release check (a dialog only when GitHub has a newer version, silent
    // offline) is scheduled once the setup is closed, so its notice never
    // lands on top of it. maybeOpen never rejects.
    Corvus.welcome.maybeOpen().then(() => Corvus.update.init());

    // A controller nobody has set up yet (or one with no firmware) is offered
    // the First Setup wizard. Watches telemetry transitions, never polls.
    Corvus.firstSetup.init();

    let userToggled = false;
    document.getElementById("panelHandle").addEventListener("click", () => {
      userToggled = true;
      if (Corvus.panel && typeof Corvus.panel.setUserToggled === "function") {
        Corvus.panel.setUserToggled(true);
      }
    });
    window.addEventListener("resize", () => {
      if (userToggled || (Corvus.panel && typeof Corvus.panel.isUserToggled === "function" && Corvus.panel.isUserToggled())) return;
      const panel = document.getElementById("rightPanel");
      const collapsed = panel.classList.contains("collapsed");
      // body.clientWidth, not innerWidth: the thresholds are in the same
      // (scaled) pixels the panel and the nav rail are sized in, so raising
      // the interface size collapses the panel at the point the layout
      // actually gets cramped rather than at a fixed window width.
      const width = document.body.clientWidth || window.innerWidth;
      if (width < 960 && !collapsed) Corvus.panel.toggle(false);
      else if (width >= 1280 && collapsed) Corvus.panel.toggle(false);
    });
    window.dispatchEvent(new Event("resize"));
  }

  return {
    init,
    refreshModes,
    refreshModesFromData,
    showVehicleMode,
    flightPhase,
    altitudeFromEntry,
    entryIsClear,
    altitudeToPosition,
    positionToAltitude,
    nudgeAltitude,
    notifyError: (msg, attempt) => Corvus.topbar.notifyError(msg, attempt),
  };
})();

document.addEventListener("DOMContentLoaded", Corvus.app.init);
