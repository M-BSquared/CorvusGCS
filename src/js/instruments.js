"use strict";
window.Corvus = window.Corvus || {};

Corvus.instruments = (function () {
  const NS = "http://www.w3.org/2000/svg";
  let compassCard, compassArrow, compassValue, adiInner, adiRollArc, adiRollScale, attitudeValue, ftGrid;
  // key -> value span, cached at build time so updateFlightTelemetry does not
  // re-query the grid on every telemetry tick (≤30 Hz after backend coalescing).
  const ftCells = {};
  const PPD = 1.7;

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

    // A face disc under the tick band, so the ticks read as a raised ring
    // around a dial rather than as marks floating on the bezel fill.
    card.appendChild(el("circle", { cx: 0, cy: 0, r: 74, class: "compass-face" }));

    // Three tick weights, not two: 5deg hairlines for texture, 10deg for
    // counting, 30deg for the labelled bearings — and the four cardinals
    // heavier still, so N/E/S/W are findable without reading the letters.
    for (let deg = 0; deg < 360; deg += 5) {
      const rad = ((deg - 90) * Math.PI) / 180;
      const isCardinal = deg % 90 === 0;
      const isMajor = deg % 30 === 0;
      const isMedium = deg % 10 === 0;
      const r1 = isMajor ? 75 : isMedium ? 79 : 82;
      const r2 = 86;
      const cls = isCardinal ? " cardinal" : isMajor ? " major" : isMedium ? " medium" : "";

      card.appendChild(
        el("line", {
          x1: (Math.cos(rad) * r1).toFixed(1),
          y1: (Math.sin(rad) * r1).toFixed(1),
          x2: (Math.cos(rad) * r2).toFixed(1),
          y2: (Math.sin(rad) * r2).toFixed(1),
          class: "compass-tick" + cls,
          "stroke-linecap": "round",
        })
      );
    }

    // Cardinals and the eight 30deg numbers share one radius. Mixing the two
    // rings — as this did — made the numbers look like a second, unrelated
    // scale; on one ring they are plainly the same scale at two weights.
    const LABEL_R = 60;
    const cardinals = [
      { d: 0, t: "N" },
      { d: 90, t: "E" },
      { d: 180, t: "S" },
      { d: 270, t: "W" },
    ];
    cardinals.forEach(({ d, t }) => {
      const rad = ((d - 90) * Math.PI) / 180;
      card.appendChild(
        el("text", {
          x: (Math.cos(rad) * LABEL_R).toFixed(1),
          y: (Math.sin(rad) * LABEL_R).toFixed(1),
          class: "compass-label" + (t === "N" ? " north" : ""),
          "text-anchor": "middle",
          "dominant-baseline": "central",
        }, t)
      );
    });

    for (const d of [30, 60, 120, 150, 210, 240, 300, 330]) {
      const rad = ((d - 90) * Math.PI) / 180;
      card.appendChild(
        el("text", {
          x: (Math.cos(rad) * LABEL_R).toFixed(1),
          y: (Math.sin(rad) * LABEL_R).toFixed(1),
          class: "compass-digit",
          "text-anchor": "middle",
          "dominant-baseline": "central",
        }, String(d / 10).padStart(2, "0"))
      );
    }
  }

  function buildCompassArrow() {
    compassArrow.innerHTML = "";
    // A waisted needle rather than the old pair of triangles: one shape whose
    // north half is obviously the pointing end. The tip stops at r=50, inside
    // the label ring, so the needle never sits on top of a bearing it is
    // supposed to be indicating.
    compassArrow.appendChild(el("path", {
      d: "M 0 43 L 5 9 L 0 3 L -5 9 Z",
      class: "needle-south",
    }));
    compassArrow.appendChild(el("path", {
      d: "M 0 -50 L 7 -9 L 0 -3 L -7 -9 Z",
      class: "needle-north",
    }));
  }

  function buildAttitude() {
    const inner = adiInner;
    inner.innerHTML = "";
    const R = 900;

    // Two flat halves. This carried a pair of linear gradients and a vignette
    // circle; the shading was doing no work the horizon line was not already
    // doing, and it cost the dial its flatness.
    inner.appendChild(el("rect", { x: -R, y: -R, width: 2 * R, height: R, class: "adi-sky" }));
    inner.appendChild(el("rect", { x: -R, y: 0, width: 2 * R, height: R, class: "adi-ground" }));

    inner.appendChild(el("line", { x1: -R, y1: 0, x2: R, y2: 0, class: "adi-horizon" }));

    // Ten-degree steps only, one weight, one label per side. The 5deg
    // half-ticks and the three-weight hierarchy that used to be here were
    // reading as precision this instrument does not have: it is a glance
    // check on attitude, and the numeric pitch/roll is printed underneath
    // for anyone who actually needs a figure.
    for (const p of [10, 20, 30, -10, -20, -30]) {
      const y = -p * PPD;
      const w = 22;

      inner.appendChild(el("line", {
        x1: -w, y1: y, x2: w, y2: y, class: "pitch-tick", "stroke-linecap": "round",
      }));

      for (const [lx, anchor] of [[w + 5, "start"], [-(w + 5), "end"]]) {
        inner.appendChild(el("text", {
          x: lx, y: y, class: "pitch-label",
          "text-anchor": anchor, "dominant-baseline": "central",
        }, String(Math.abs(p))));
      }
    }

    // Level, half-bank, full-bank, on the ring rather than on the sky — the
    // same band, radii and weights the compass uses for its 30deg ticks, so
    // the two dials read as one pair of instruments. Eleven ticks invited the
    // operator to read a bank angle off an arc that is 11px tall on the real
    // panel; three say the one thing the arc is good for — how far past level
    // the aircraft is.
    adiRollArc.innerHTML = "";
    for (const r of [-60, -30, 0, 30, 60]) {
      const rad = (r - 90) * Math.PI / 180;
      const r1 = r === 0 ? 75 : 79;
      const r2 = 86;
      adiRollArc.appendChild(el("line", {
        x1: (Math.cos(rad) * r1).toFixed(1), y1: (Math.sin(rad) * r1).toFixed(1),
        x2: (Math.cos(rad) * r2).toFixed(1), y2: (Math.sin(rad) * r2).toFixed(1),
        class: "roll-tick" + (r === 0 ? " zero" : ""),
        "stroke-linecap": "round",
      }));
    }

    // Sky pointer: the only part that banks, so the gap between it and the 0
    // tick IS the roll angle. Its apex meets the ball edge, which is what ties
    // the moving half of the instrument to the fixed scale above it.
    adiRollScale.innerHTML = "";
    adiRollScale.appendChild(el("polygon", { points: "0,-73 -5,-64 5,-64", class: "roll-pointer" }));
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
    adiRollArc = document.getElementById("adiRollArc");
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
