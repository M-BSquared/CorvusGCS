"use strict";
window.Corvus = window.Corvus || {};

Corvus.instruments = (function () {
  const NS = "http://www.w3.org/2000/svg";
  let compassCard, compassArrow, compassValue, adiInner, adiRollScale, attitudeValue, ftGrid;
  // key -> value span, cached at build time so updateFlightTelemetry does not
  // re-query the grid on every telemetry tick (≤30 Hz after backend coalescing).
  const ftCells = {};
  const PPD = 2.0;

  // Interpolation state. TARGET = latest telemetry; DISPLAYED = eased value the
  // SVG transforms are rendered from. Driven by the single shared Corvus.anim
  // loop so the compass/attitude stay fluid between 5–10 Hz radio samples.
  let insInit = false;
  const insTarget = { heading: 0, pitch: 0, roll: 0 };
  const insDisplay = { heading: 0, pitch: 0, roll: 0 };
  let insAnimator = null;
  const HDG_TAU = 0.18;
  const PR_TAU = 0.16;
  const HDG_EPS = 0.05;
  const PR_EPS = 0.05;

  function el(name, attrs, text) {
    const e = document.createElementNS(NS, name);
    if (attrs) for (const k in attrs) e.setAttribute(k, attrs[k]);
    if (text != null) e.textContent = text;
    return e;
  }

  function buildCompass() {
    const card = compassCard;
    card.innerHTML = "";

    for (let deg = 0; deg < 360; deg += 5) {
      const rad = ((deg - 90) * Math.PI) / 180;
      const isMajor = deg % 30 === 0;
      const isMedium = deg % 10 === 0;
      const r1 = isMajor ? 72 : isMedium ? 78 : 82;
      const r2 = 88;

      card.appendChild(
        el("line", {
          x1: (Math.cos(rad) * r1).toFixed(1),
          y1: (Math.sin(rad) * r1).toFixed(1),
          x2: (Math.cos(rad) * r2).toFixed(1),
          y2: (Math.sin(rad) * r2).toFixed(1),
          stroke: isMajor ? "#38bdf8" : "rgba(255, 255, 255, 0.35)",
          "stroke-width": isMajor ? "1.8" : "1",
          "stroke-linecap": "round",
        })
      );
    }

    const cardinals = [
      { d: 0, t: "N" },
      { d: 90, t: "E" },
      { d: 180, t: "S" },
      { d: 270, t: "W" },
    ];
    cardinals.forEach(({ d, t }) => {
      const rad = ((d - 90) * Math.PI) / 180;
      const x = Math.cos(rad) * 58;
      const y = Math.sin(rad) * 58;

      card.appendChild(
        el("text", {
          x: x.toFixed(1),
          y: y.toFixed(1),
          fill: t === "N" ? "#38bdf8" : "#f8fafc",
          "font-size": "11",
          "font-weight": "700",
          "text-anchor": "middle",
          "dominant-baseline": "central",
        }, t)
      );
    });

    for (const d of [30, 60, 120, 150, 210, 240, 300, 330]) {
      const rad = ((d - 90) * Math.PI) / 180;
      const x = Math.cos(rad) * 60;
      const y = Math.sin(rad) * 60;

      card.appendChild(
        el("text", {
          x: x.toFixed(1),
          y: y.toFixed(1),
          fill: "rgba(248, 250, 252, 0.45)",
          "font-size": "8",
          "font-weight": "600",
          "text-anchor": "middle",
          "dominant-baseline": "central",
        }, String(d / 10).padStart(2, "0"))
      );
    }
  }

  function buildCompassArrow() {
    compassArrow.innerHTML = "";
    compassArrow.appendChild(el("polygon", {
      points: "0,-42 -10,-18 10,-18",
      class: "arrow-body",
    }));
    compassArrow.appendChild(el("polygon", {
      points: "0,42 -6,18 6,18",
      class: "arrow-tail",
    }));
    compassArrow.appendChild(el("line", {
      x1: 0, y1: -18, x2: 0, y2: 18,
      stroke: "rgba(255, 81, 77, 0.4)", "stroke-width": "3.5",
    }));
  }

  function buildAttitude() {
    const inner = adiInner;
    const R = 800;

    const skyGrad = el("linearGradient", { id: "adi-sky", x1: "0", y1: "0", x2: "0", y2: "1" });
    skyGrad.appendChild(el("stop", { offset: "0%", "stop-color": "#0D1B2A" }));
    skyGrad.appendChild(el("stop", { offset: "50%", "stop-color": "#1B4965" }));
    skyGrad.appendChild(el("stop", { offset: "100%", "stop-color": "#2D7FB0" }));
    inner.appendChild(skyGrad);

    const grdGrad = el("linearGradient", { id: "adi-ground", x1: "0", y1: "0", x2: "0", y2: "1" });
    grdGrad.appendChild(el("stop", { offset: "0%", "stop-color": "#1B5E3F" }));
    grdGrad.appendChild(el("stop", { offset: "50%", "stop-color": "#0E3D2A" }));
    grdGrad.appendChild(el("stop", { offset: "100%", "stop-color": "#06231A" }));
    inner.appendChild(grdGrad);

    inner.appendChild(el("rect", { x: -R, y: -R, width: 2 * R, height: R, fill: "url(#adi-sky)" }));
    inner.appendChild(el("rect", { x: -R, y: 0, width: 2 * R, height: R, fill: "url(#adi-ground)" }));

    inner.appendChild(el("line", {
      x1: -R, y1: 0, x2: R, y2: 0,
      stroke: "#FFFFFF", "stroke-width": 2, opacity: "0.9",
    }));

    for (const p of [5, 10, 15, 20, 25, 30, -5, -10, -15, -20, -25, -30]) {
      const y = -p * PPD;
      const isMajor = p % 10 === 0;
      const isBig = p % 30 === 0;
      const w = isBig ? 30 : (isMajor ? 18 : 8);

      inner.appendChild(el("line", {
        x1: -w, y1: y, x2: w, y2: y,
        stroke: "#FFFFFF", "stroke-width": isBig ? 1.8 : (isMajor ? 1.2 : 0.8),
        opacity: isBig ? "0.8" : (isMajor ? "0.5" : "0.3"),
      }));

      if (isMajor && p !== 0) {
        for (const [lx, anchor] of [[w + 5, "start"], [-(w + 5), "end"]]) {
          inner.appendChild(el("text", {
            x: lx, y: y - 2, fill: "rgba(255,255,255,0.6)", "font-size": "7",
            "font-weight": "500", "text-anchor": anchor, "dominant-baseline": "central",
          }, String(Math.abs(p))));
        }
      }
    }

    const scale = adiRollScale;
    for (const r of [-60, -45, -30, -20, -10, 0, 10, 20, 30, 45, 60]) {
      const rad = (r - 90) * Math.PI / 180;
      const major = r % 30 === 0;
      const r1 = major ? 76 : 82;
      const r2 = 88;
      scale.appendChild(el("line", {
        x1: (Math.cos(rad) * r1).toFixed(1), y1: (Math.sin(rad) * r1).toFixed(1),
        x2: (Math.cos(rad) * r2).toFixed(1), y2: (Math.sin(rad) * r2).toFixed(1),
        class: "roll-tick" + (major ? " major" : ""),
      }));
    }
    scale.appendChild(el("polygon", { points: "0,-90 -4,-82 4,-82", class: "roll-pointer" }));
  }

  function updateAttitude(pitch, roll) {
    // Clamps are applied to the DISPLAYED value so the instrument never
    // over-travels its scale while easing.
    const pPitch = Math.max(-30, Math.min(30, pitch));
    const pRoll = Math.max(-60, Math.min(60, roll));
    adiInner.setAttribute("transform", `rotate(${-pRoll}) translate(0 ${(pPitch * PPD).toFixed(1)})`);
    adiRollScale.setAttribute("transform", `rotate(${-pRoll})`);
    if (attitudeValue) {
      const ps = pitch >= 0 ? "+" : "";
      const rs = roll >= 0 ? "+" : "";
      attitudeValue.textContent = `P ${ps}${pitch.toFixed(1)}\u00B0  R ${rs}${roll.toFixed(1)}\u00B0`;
    }
  }

  /** Render the SVG transforms from the interpolated DISPLAYED values. */
  function renderInstruments() {
    const h = Math.round(Corvus.anim.normAngle(insDisplay.heading)) % 360;
    if (compassArrow) compassArrow.setAttribute("transform", `rotate(${h})`);
    if (compassValue) compassValue.textContent = String(h).padStart(3, "0") + "\u00B0";
    const cw = document.getElementById("compassWindow");
    if (cw) cw.textContent = String(h).padStart(3, "0");
    updateAttitude(insDisplay.pitch, insDisplay.roll);
  }

  /** Set the TARGET from telemetry; snap (reduced-motion) or wake the loop. */
  function setInstrumentsTarget(heading, pitch, roll) {
    if (heading == null || isNaN(heading)) heading = 0;
    if (pitch == null || isNaN(pitch)) pitch = 0;
    if (roll == null || isNaN(roll)) roll = 0;
    insTarget.heading = heading;
    insTarget.pitch = pitch;
    insTarget.roll = roll;
    if (!insInit) {
      insInit = true;
      insDisplay.heading = heading;
      insDisplay.pitch = pitch;
      insDisplay.roll = roll;
      renderInstruments();
      return;
    }
    if (Corvus.anim.reducedMotion()) {
      insDisplay.heading = heading;
      insDisplay.pitch = pitch;
      insDisplay.roll = roll;
      renderInstruments();
    } else {
      Corvus.anim.wake();
    }
  }

  function buildFlightTelemetry() {
    const cells = [
      { label: "ALT AMSL", key: "altAmsl", cls: "" },
      { label: "AGL", key: "agl", cls: "" },
      { label: "GS", key: "gs", cls: "nav" },
      { label: "V/S", key: "vs", cls: "" },
      { label: "HDG", key: "hdg", cls: "nav" },
      { label: "SAT", key: "sats", cls: "" },
    ];
    ftGrid.innerHTML = "";
    for (const k in ftCells) delete ftCells[k];   // refresh on rebuild
    cells.forEach((c) => {
      const cell = document.createElement("div");
      cell.className = "ft-cell";
      cell.innerHTML = `<span class="ft-label">${c.label}</span><span class="ft-value ${c.cls}" data-ft="${c.key}">\u2014</span>`;
      ftGrid.appendChild(cell);
    });
    // Cache the value span for each key once, here, so the per-tick update is a
    // plain map lookup + textContent set instead of 6 querySelector calls.
    cells.forEach((c) => { ftCells[c.key] = ftGrid.querySelector(`[data-ft="${c.key}"]`); });
  }

  function updateFlightTelemetry(s) {
    const set = (k, v) => {
      const n = ftCells[k];
      if (n) n.textContent = v;
    };
    if (!s.connected) {
      ["altAmsl", "agl", "gs", "vs", "hdg", "sats"].forEach((k) => set(k, "\u2014"));
      return;
    }
    set("altAmsl", `${Math.round(s.altitude_amsl)} m`);
    set("agl", `${Math.round(s.altitude_agl)} m`);
    set("gs", `${s.groundspeed.toFixed(1)} m/s`);
    set("vs", `${s.vspeed >= 0 ? "+" : ""}${s.vspeed.toFixed(1)} m/s`);
    set("hdg", `${String(Math.round(s.heading)).padStart(3, "0")}\u00B0`);
    set("sats", `${s.gps_satellites}`);
  }

  function init(overlayEl) {
    compassCard = document.getElementById("compassCard");
    compassArrow = document.getElementById("compassArrow");
    compassValue = document.getElementById("compassValue");
    adiInner = document.getElementById("adiInner");
    adiRollScale = document.getElementById("adiRollScale");
    attitudeValue = document.getElementById("attitudeValue");
    ftGrid = document.getElementById("flightTelemetry");
    buildCompass();
    buildCompassArrow();
    buildAttitude();
    buildFlightTelemetry();

    // Register with the single shared rAF loop. Each frame eases DISPLAYED
    // toward TARGET and re-renders; returns false once settled so the loop
    // can self-cancel when the vehicle is idle.
    insAnimator = {
      step: function (dt) {
        if (!insInit) return false;
        const nh = Corvus.anim.approachAngle(insDisplay.heading, insTarget.heading, dt, HDG_TAU);
        const np = Corvus.anim.approach(insDisplay.pitch, insTarget.pitch, dt, PR_TAU);
        const nr = Corvus.anim.approach(insDisplay.roll, insTarget.roll, dt, PR_TAU);
        const done =
          Corvus.anim.settledAngle(nh, insTarget.heading, HDG_EPS) &&
          Math.abs(np - insTarget.pitch) < PR_EPS &&
          Math.abs(nr - insTarget.roll) < PR_EPS;
        insDisplay.heading = done ? insTarget.heading : nh;
        insDisplay.pitch = done ? insTarget.pitch : np;
        insDisplay.roll = done ? insTarget.roll : nr;
        renderInstruments();
        return !done;
      },
    };
    Corvus.anim.add(insAnimator);

    Corvus.telemetry.subscribe((s) => {
      // Compass arrow + attitude SVG interpolate; the numeric flight-telemetry
      // grid stays a direct (tabular) readout of the latest sample.
      setInstrumentsTarget(s.heading, s.pitch, s.roll);
      updateFlightTelemetry(s);
    });
  }

  return { init };
})();
