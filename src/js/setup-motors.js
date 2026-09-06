"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupMotors — the Motors sub-page of the Setup page.
 *
 * The centrepiece is a drawing of the actual airframe: every motor at its real
 * distance from the centre of gravity, spinning the way its parameters say it
 * spins. That is not decoration. The question this page exists to answer is
 * physical — "which pin is the front-left motor on?" — and a list of parameter
 * names cannot answer it. So the operator clicks the motor they are looking at
 * and assigns, positions, or spins *that* one.
 *
 * The page is schema-driven: GET /api/motors returns a description built by
 * corvus/motor_config.py — the motors with their positions and wiring, the
 * output catalogue, and generic field sections for geometry and protocol. Every
 * field names the PX4 parameter it writes, so nothing here hardcodes a
 * parameter name and a firmware that lacks one simply sends one field fewer
 * (AGENTS.md: graceful fallback across PX4 v1.16 / v1.17 / v1.18).
 *
 * Airframe, deliberately split in two:
 *   - CA_AIRFRAME, the geometry class the control allocator solves for, is
 *     editable here: it is what makes "Motor 3" mean anything at all.
 *   - SYS_AUTOSTART, the airframe *preset*, is shown as read-only context. It
 *     rewrites a whole configuration bundle and needs a reboot; that is a
 *     different act from wiring motors and stays in the parameter editor.
 *
 * The motor test spins real hardware. It is gated three ways: PX4 refuses it
 * while armed, the bridge bounds the duration so the *vehicle* stops the motor
 * even if the link dies, and this page will not enable the button until the
 * operator confirms the propellers are off. That confirmation is deliberately
 * not remembered — it is re-asked every time the page opens, because the one
 * time it is stale is the time someone put the props back on.
 *
 * Backend contract:
 *   GET  /api/motors                        {connected,motors,outputs,banks,sections}
 *   POST /api/motors/assign {motor,bank,pin} wire a motor to an output pin
 *   POST /api/motors/test {motor,throttle,duration}
 *   POST /api/motors/test/stop
 *   POST /api/params/set {name,value}        write one field (refused while armed)
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the lifecycle and calls destroy() on back / left-nav re-entry, which
 * releases the telemetry subscription, cancels the test countdown, stops any
 * running motor, and disowns an in-flight fetch.
 */
Corvus.setupMotors = (function () {
  const S = Corvus.setupShared;
  const SVG_NS = "http://www.w3.org/2000/svg";

  // The schema-driven form machinery lives in setupShared, shared with the
  // Safety & Sensors page: the control registry, the armed gate, the write path
  // and its restore-on-refusal. These are aliases, not wrappers — one copy of
  // the write path is the point.
  const { registerControl, dropControls, recheckAll, applyArmed,
          setFieldStatus, notify, formatNumber } = S;

  // PX4's default moment coefficient magnitude. Only used when the vehicle
  // reports CA_ROTORn_KM as exactly 0: the sign of zero carries no direction,
  // so a CW/CCW choice would otherwise write zero back and change nothing.
  const DEFAULT_KM = 0.05;

  // Diagram geometry, in viewBox units.
  const VIEW = 340;
  const CENTER = VIEW / 2;
  const ARM_PX = 118;        // screen radius of the outermost motor
  const HUB_PX = 20;         // motor disc radius
  // Below this the geometry has no meaningful extent (all motors at the
  // origin), so positions are laid out evenly and the ring is drawn dashed —
  // an honest "no positions set" rather than four discs stacked on the hub.
  const MIN_SPAN_M = 1e-4;

  // Bench identification throttles. Deliberately stops at half: this is "which
  // motor is this?", not a power test, and the ESC calibration procedure is the
  // separate tool for the full range.
  const THROTTLE_STEPS = [5, 10, 15, 20, 30, 40, 50];
  const DEFAULT_THROTTLE = 15;
  const TEST_DURATION_S = 2;

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page");
    page.appendChild(S.backButton(navigateBack));
    page.appendChild(S.pageHeader("Motors", "Airframe layout, motor assignment, and output protocol"));

    // Actions bar lives outside the content host so Reload + status survive
    // every re-render of the cards below.
    const actions = S.el("div", "params-actions");
    const reloadBtn = Corvus.ui.button({
      variant: "primary", size: "sm", icon: "refresh-cw", label: "Reload",
    });
    const actionsStatus = S.el("div", "params-actions-status");
    actions.appendChild(reloadBtn);
    actions.appendChild(actionsStatus);
    page.appendChild(actions);

    const banner = S.el("div", "params-banner");
    banner.hidden = true;
    banner.textContent = "Motor configuration is read-only while armed";
    page.appendChild(banner);

    const host = S.el("div", "page-section motors-sections");
    page.appendChild(host);
    container.appendChild(page);

    // `controls` holds one recheck() per editable control so an armed
    // transition re-gates the whole page without walking the DOM.
    const state = {
      host, banner, reloadBtn, actionsStatus,
      armed: false, loading: false, destroyed: false,
      controls: [], doc: null,
      selected: 1,             // 1-based motor the detail panel is showing
      propsOff: false,         // the propellers-removed acknowledgement
      throttle: DEFAULT_THROTTLE,
      testing: 0,              // motor currently spinning, 0 = none
      testTimer: null,
      nodes: {},               // motor number -> diagram node, for live marking
    };

    const cur = Corvus.telemetry && Corvus.telemetry.getState();
    applyArmed(state, !!(cur && cur.armed));

    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      state.unsub = Corvus.telemetry.subscribe((s) => applyArmed(state, !!(s && s.armed)));
    }

    reloadBtn.addEventListener("click", () => load(state));
    load(state);

    return function destroy() {
      state.destroyed = true;
      if (state.unsub) { try { state.unsub(); } catch (_e) {} state.unsub = null; }
      clearTestTimer(state);
      // Leaving the page must not leave a motor turning. The vehicle's own
      // bounded timeout is the backstop; this is the immediate stop.
      if (state.testing) {
        state.testing = 0;
        try { Corvus.telemetry.postAction("/api/motors/test/stop", {}); } catch (_e) {}
      }
    };
  }

  /** Fetch the configuration and rebuild every card from the response. */
  function load(state) {
    if (state.loading) return;
    state.loading = true;
    state.reloadBtn.disabled = true;
    setStatus(state, "pending", "Reading motor configuration…");
    Corvus.telemetry.requestJson("/api/motors").then((doc) => {
      if (state.destroyed) return;
      state.doc = doc || {};
      renderCards(state);
      if (!doc || !doc.connected) {
        setStatus(state, "err", (doc && doc.error) || "No motor configuration received");
      } else {
        const n = (doc.motors || []).length;
        setStatus(state, "ok", `${n} motor${n === 1 ? "" : "s"} · ${doc.received} parameters read`);
      }
    }).catch((err) => {
      if (state.destroyed) return;
      state.doc = {};
      renderCards(state);
      setStatus(state, "err", (err && err.message) || "Could not read motor configuration");
    }).finally(() => {
      if (state.destroyed) return;
      state.loading = false;
      state.reloadBtn.disabled = false;
    });
  }

  function renderCards(state) {
    const doc = state.doc || {};
    const motors = doc.motors || [];
    state.host.innerHTML = "";
    state.controls = [];
    state.nodes = {};

    if (!motors.length && !(doc.sections || []).length) {
      const card = S.el("div", "page-card motors-card");
      card.appendChild(S.sectionTitle("Motors"));
      card.appendChild(S.el("div", "params-desc",
        "Connect to a vehicle to read its motor configuration. The page shows only "
        + "what the connected firmware actually reports."));
      state.host.appendChild(card);
      S.refreshIcons();
      return;
    }

    // Keep the selection valid across a reload that changed the motor count.
    if (!motors.some((m) => m.number === state.selected)) {
      state.selected = motors.length ? motors[0].number : 0;
    }

    if (motors.length) {
      state.host.appendChild(airframeCard(state, doc, motors));
      state.host.appendChild(motorCard(state, doc, motors));
      state.host.appendChild(testCard(state, motors));
    }
    (doc.sections || []).forEach((section) => {
      const card = S.el("div", "page-card motors-card");
      card.appendChild(S.sectionTitle(section.title || ""));
      if (section.hint) card.appendChild(S.el("div", "field-hint", section.hint));
      card.appendChild(fieldGrid(state, section.fields || []));
      state.host.appendChild(card);
    });

    applyArmed(state, state.armed);
    S.refreshIcons();
  }

  // -------------------------------------------------------------------------
  // The airframe picture
  // -------------------------------------------------------------------------

  function airframeCard(state, doc, motors) {
    const card = S.el("div", "page-card motors-card motors-airframe-card");
    card.appendChild(S.sectionTitle("Airframe"));

    // The airframe class and the motor count live here, beside the drawing they
    // change, rather than in a separate form further down the page: picking
    // "Fixed wing" redraws this picture, so the control belongs next to it.
    if ((doc.geometry || []).length) {
      card.appendChild(fieldGrid(state, doc.geometry));
    }

    if (doc.airframe_preset != null) {
      const caption = S.el("div", "field-hint",
        `Airframe preset SYS_AUTOSTART ${doc.airframe_preset}. The preset rewrites a whole `
        + "configuration and needs a reboot, so it is changed in Parameters, not here.");
      card.appendChild(caption);
    }

    const span = motorSpan(motors);
    card.appendChild(diagram(state, motors, span, doc.airframe_family || "multirotor"));

    const legend = S.el("div", "motors-legend");
    legend.appendChild(S.el("span", "motors-legend-item",
      span >= MIN_SPAN_M
        // Millimetres is the finest anyone measures an arm to; the raw float
        // carries a dozen digits of noise that say nothing.
        ? `Outer ring ${(Math.round(span * 1000) / 1000)} m from the centre of gravity`
        : "No motor positions set — motors shown evenly spaced"));
    // PX4 stores no wingspan, hull length or rotor diameter, so the body behind
    // the motors is a schematic of the airframe *class*. Only the motors are
    // measured, and saying so is the difference between a diagram and a lie.
    if ((doc.airframe_family || "multirotor") !== "multirotor") {
      legend.appendChild(S.el("span", "motors-legend-item",
        `${doc.airframe_label || "Airframe"} outline is schematic — motor positions are to scale`));
    }
    legend.appendChild(S.el("span", "motors-legend-item",
      "Click a motor to select it"));
    card.appendChild(legend);

    // Add / remove motors. These write CA_ROTOR_COUNT, which changes which
    // fields exist at all, so both reload the page afterwards.
    const count = motors.length;
    const max = doc.max_motors || 12;
    const rowEl = S.el("div", "motors-count-actions");
    const addBtn = Corvus.ui.button({
      variant: "secondary", size: "sm", icon: "plus", label: "Add motor",
    });
    const removeBtn = Corvus.ui.button({
      variant: "ghost", size: "sm", icon: "minus", label: "Remove last",
    });
    rowEl.appendChild(addBtn);
    rowEl.appendChild(removeBtn);
    rowEl.appendChild(S.el("span", "motors-count-note", `${count} of ${max} motors`));
    card.appendChild(rowEl);

    const countStatus = S.el("span", "params-row-status", "");
    card.appendChild(countStatus);

    addBtn.addEventListener("click", () =>
      setMotorCount(state, count + 1, addBtn, countStatus));
    removeBtn.addEventListener("click", () =>
      setMotorCount(state, count - 1, removeBtn, countStatus));

    registerControl(state, addBtn, () => { addBtn.disabled = state.armed || count >= max; });
    registerControl(state, removeBtn, () => { removeBtn.disabled = state.armed || count <= 1; });
    return card;
  }

  /** Distance of the furthest motor from the centre of gravity, in metres. */
  function motorSpan(motors) {
    return motors.reduce(
      (max, m) => Math.max(max, Math.hypot(Number(m.x) || 0, Number(m.y) || 0)), 0);
  }

  /**
   * Screen position of a motor, in viewBox units.
   *
   * PX4 body frame is X forward, Y right, Z down, and the view is from above:
   * X maps up the screen, Y maps right. When the geometry carries no positions
   * at all the motors are spread evenly instead, so they stay distinguishable
   * and clickable — the dashed ring says that is what happened.
   */
  function motorPoint(motor, index, total, span) {
    if (span < MIN_SPAN_M) {
      const angle = (index / Math.max(1, total)) * Math.PI * 2 - Math.PI / 2;
      return { x: CENTER + Math.cos(angle) * ARM_PX, y: CENTER + Math.sin(angle) * ARM_PX };
    }
    const scale = ARM_PX / span;
    return {
      x: CENTER + (Number(motor.y) || 0) * scale,
      y: CENTER - (Number(motor.x) || 0) * scale,
    };
  }

  function diagram(state, motors, span, family) {
    const host = S.el("div", "motors-diagram");
    if (typeof document.createElementNS !== "function") {
      // No SVG (a browser without it): fall back to a plain row of motor
      // buttons so selecting and assigning still work.
      motors.forEach((m, i) => host.appendChild(motorButton(state, m, i, motors.length, span)));
      return host;
    }

    const svg = node("svg", {
      viewBox: `0 0 ${VIEW} ${VIEW}`,
      class: "motors-svg",
      role: "group",
      "aria-label": "Airframe layout",
    });
    svg.dataset.family = family;

    // Reference ring at the outermost motor radius, plus the body axes.
    svg.appendChild(node("circle", {
      cx: CENTER, cy: CENTER, r: ARM_PX, class: "motors-ring",
      "stroke-dasharray": span < MIN_SPAN_M ? "5 6" : "none",
    }));
    svg.appendChild(node("line", {
      x1: CENTER, y1: CENTER - ARM_PX, x2: CENTER, y2: CENTER + ARM_PX, class: "motors-axis",
    }));
    svg.appendChild(node("line", {
      x1: CENTER - ARM_PX, y1: CENTER, x2: CENTER + ARM_PX, y2: CENTER, class: "motors-axis",
    }));

    // Nose marker: without it "front-left" is a guess.
    svg.appendChild(node("polygon", {
      points: `${CENTER},${CENTER - ARM_PX - 22} ${CENTER - 9},${CENTER - ARM_PX - 6} ${CENTER + 9},${CENTER - ARM_PX - 6}`,
      class: "motors-nose",
    }));
    const fwd = node("text", {
      x: CENTER, y: CENTER - ARM_PX - 28, class: "motors-axis-label", "text-anchor": "middle",
    });
    fwd.textContent = "FWD";
    svg.appendChild(fwd);

    // The body, then the motors on top of it.
    svg.appendChild(silhouette(family, motors, span));
    motors.forEach((m, i) => {
      svg.appendChild(motorNode(state, m, motorPoint(m, i, motors.length, span)));
    });

    host.appendChild(svg);
    return host;
  }

  /**
   * The airframe body behind the motors, chosen by the vehicle's CA_AIRFRAME
   * class.
   *
   * A multirotor's arms are *derived*: they run from the hub to each motor, so
   * they carry real information. Every other body is a schematic — PX4 stores no
   * wingspan, hull length or rotor diameter, so a wing drawn to a made-up size
   * would be a lie dressed as data. They are therefore drawn at a fixed
   * proportion of the view and labelled as schematic in the legend, while the
   * motors on top stay at their measured positions.
   */
  function silhouette(family, motors, span) {
    const g = node("g", { class: "motors-body" });
    g.dataset.family = family;
    if (family === "wing") { wingBody(g); }
    else if (family === "vtol") { wingBody(g); liftArms(g, motors, span); }
    else if (family === "rover") { roverBody(g); }
    else if (family === "helicopter") { helicopterBody(g); }
    else { multirotorArms(g, motors, span); }
    g.appendChild(node("circle", { cx: CENTER, cy: CENTER, r: 9, class: "motors-hub" }));
    return g;
  }

  /** Arms from the hub to every motor — the multirotor's real structure. */
  function multirotorArms(g, motors, span) {
    motors.forEach((m, i) => {
      const p = motorPoint(m, i, motors.length, span);
      g.appendChild(node("line", {
        x1: CENTER, y1: CENTER, x2: p.x, y2: p.y, class: "motors-arm",
      }));
    });
  }

  /** A quadplane's booms: arms to the lift rotors only, never to the pusher. */
  function liftArms(g, motors, span) {
    motors.forEach((m, i) => {
      if (m.thrust === "horizontal") return;
      const p = motorPoint(m, i, motors.length, span);
      g.appendChild(node("line", {
        x1: CENTER, y1: CENTER, x2: p.x, y2: p.y, class: "motors-arm",
      }));
    });
  }

  /** Fuselage, main wing, tailplane and fin, seen from above. */
  function wingBody(g) {
    const halfSpan = 128;
    const wingY = CENTER - 12;
    g.appendChild(node("polygon", {
      class: "motors-shape",
      points: pts([
        [CENTER - halfSpan, wingY + 26], [CENTER - halfSpan + 16, wingY - 6],
        [CENTER - 14, wingY - 14], [CENTER + 14, wingY - 14],
        [CENTER + halfSpan - 16, wingY - 6], [CENTER + halfSpan, wingY + 26],
        [CENTER + halfSpan - 10, wingY + 32], [CENTER - halfSpan + 10, wingY + 32],
      ]),
    }));
    const tailY = CENTER + 96;
    g.appendChild(node("polygon", {
      class: "motors-shape",
      points: pts([
        [CENTER - 56, tailY + 14], [CENTER - 48, tailY - 2], [CENTER + 48, tailY - 2],
        [CENTER + 56, tailY + 14], [CENTER + 48, tailY + 18], [CENTER - 48, tailY + 18],
      ]),
    }));
    // Fuselage last so it reads as sitting on top of the wing.
    g.appendChild(node("polygon", {
      class: "motors-shape motors-shape-body",
      points: pts([
        [CENTER, CENTER - 118], [CENTER + 11, CENTER - 92], [CENTER + 13, CENTER + 60],
        [CENTER + 7, tailY + 22], [CENTER - 7, tailY + 22],
        [CENTER - 13, CENTER + 60], [CENTER - 11, CENTER - 92],
      ]),
    }));
  }

  /** Chassis and four wheels, seen from above. */
  function roverBody(g) {
    g.appendChild(node("rect", {
      class: "motors-shape motors-shape-body",
      x: CENTER - 52, y: CENTER - 96, width: 104, height: 192, rx: 18,
    }));
    [[-1, -1], [1, -1], [-1, 1], [1, 1]].forEach(([sx, sy]) => {
      g.appendChild(node("rect", {
        class: "motors-shape",
        x: CENTER + sx * 68 - 11, y: CENTER + sy * 62 - 22,
        width: 22, height: 44, rx: 7,
      }));
    });
  }

  /** Main rotor disc, pod and tail boom, seen from above. */
  function helicopterBody(g) {
    g.appendChild(node("circle", {
      class: "motors-shape motors-rotor-disc",
      cx: CENTER, cy: CENTER - 18, r: 96,
    }));
    g.appendChild(node("polygon", {
      class: "motors-shape motors-shape-body",
      points: pts([
        [CENTER, CENTER - 86], [CENTER + 22, CENTER - 44], [CENTER + 20, CENTER + 20],
        [CENTER + 7, CENTER + 30], [CENTER + 7, CENTER + 104], [CENTER - 7, CENTER + 104],
        [CENTER - 7, CENTER + 30], [CENTER - 20, CENTER + 20], [CENTER - 22, CENTER - 44],
      ]),
    }));
    g.appendChild(node("circle", {
      class: "motors-shape motors-rotor-disc",
      cx: CENTER + 20, cy: CENTER + 104, r: 24,
    }));
  }

  function pts(list) {
    return list.map(([x, y]) => `${round(x)},${round(y)}`).join(" ");
  }

  /** One clickable motor on the diagram. */
  function motorNode(state, motor, point) {
    const g = node("g", {
      class: "motors-node",
      tabindex: "0",
      role: "button",
      "aria-label": motorAria(motor),
    });
    g.dataset.motor = String(motor.number);

    g.appendChild(node("circle", {
      cx: point.x, cy: point.y, r: HUB_PX, class: "motors-node-disc",
    }));

    if (motor.spin) {
      const arc = spinArc(point.x, point.y, HUB_PX + 7, motor.spin === "CCW");
      g.appendChild(node("path", { d: arc.d, class: "motors-node-spin" }));
      g.appendChild(node("polygon", { points: arc.head, class: "motors-node-spin-head" }));
    }

    // A rotor that does not thrust straight up — a VTOL's pusher, a plane's
    // tractor, a tailsitter's whole set — gets an arrow along the direction it
    // actually pushes. Without it a pusher is drawn identically to a lift rotor
    // and the picture claims the aircraft hovers on it.
    const thrust = thrustArrow(motor, point);
    if (thrust) g.appendChild(node("polygon", { points: thrust, class: "motors-node-thrust" }));

    const number = node("text", {
      x: point.x, y: point.y + 5, class: "motors-node-number", "text-anchor": "middle",
    });
    number.textContent = String(motor.number);
    g.appendChild(number);

    const output = node("text", {
      x: point.x, y: point.y + HUB_PX + 21, class: "motors-node-output", "text-anchor": "middle",
    });
    output.textContent = motor.output ? motor.output.label : "unassigned";
    g.appendChild(output);

    const select = () => selectMotor(state, motor.number);
    g.addEventListener("click", select);
    g.addEventListener("keydown", (e) => {
      const key = e && e.key;
      if (key === "Enter" || key === " " || key === "Spacebar") {
        if (e.preventDefault) e.preventDefault();
        select();
      }
    });

    state.nodes[motor.number] = g;
    paintNode(state, motor.number);
    return g;
  }

  /** The no-SVG fallback: the same selection affordance as a plain button. */
  function motorButton(state, motor, index, total, span) {
    const btn = Corvus.ui.button({
      variant: "secondary", size: "sm",
      className: "motors-node-fallback",
      label: `${motor.label} · ${motor.output ? motor.output.label : "unassigned"}`,
      ariaLabel: motorAria(motor),
      onClick: () => selectMotor(state, motor.number),
    });
    btn.dataset.motor = String(motor.number);
    state.nodes[motor.number] = btn;
    paintNode(state, motor.number);
    return btn;
  }

  function motorAria(motor) {
    const parts = [motor.label];
    parts.push(motor.output ? `on ${motor.output.label}` : "not assigned");
    if (Math.abs(motor.x) > 1e-6 || Math.abs(motor.y) > 1e-6) {
      parts.push(`${formatNumber(motor.x)} metres forward, ${formatNumber(motor.y)} metres right`);
    }
    if (motor.spin) parts.push(motor.spin === "CW" ? "clockwise" : "counter-clockwise");
    if (motor.thrust === "horizontal") parts.push("thrusts forward");
    return parts.join(", ");
  }

  /**
   * An arc with an arrowhead showing which way the propeller turns, seen from
   * above. Screen Y points down, so SVG's sweep-flag 1 draws visually clockwise
   * and 0 draws counter-clockwise; the arrowhead sits on the end tangent.
   */
  function spinArc(cx, cy, r, ccw) {
    const sweep = 250;
    const startDeg = ccw ? 40 : 140;
    const endDeg = ccw ? startDeg - sweep : startDeg + sweep;
    const p0 = onCircle(cx, cy, r, startDeg);
    const p1 = onCircle(cx, cy, r, endDeg);
    const d = `M ${round(p0.x)} ${round(p0.y)} A ${r} ${r} 0 ${sweep > 180 ? 1 : 0} `
      + `${ccw ? 0 : 1} ${round(p1.x)} ${round(p1.y)}`;
    const tangent = endDeg + (ccw ? -90 : 90);
    return { d, head: triangle(p1, tangent, 6) };
  }

  /**
   * The thrust direction of a non-lift rotor, projected onto the top-down view.
   *
   * Returns null for a lift rotor (its thrust points out of the screen, which
   * cannot be drawn as an in-plane arrow) and for a firmware that reports no
   * axis at all — in both cases the plain disc is the honest drawing.
   */
  function thrustArrow(motor, point) {
    const axis = motor.axis;
    if (!axis || motor.thrust !== "horizontal") return null;
    // Body X forward maps up the screen, body Y right maps right.
    const dx = Number(axis.y) || 0;
    const dy = -(Number(axis.x) || 0);
    const length = Math.hypot(dx, dy);
    if (length < 0.2) return null;
    const deg = Math.atan2(dy / length, dx / length) * 180 / Math.PI;
    const tip = onCircle(point.x, point.y, HUB_PX + 16, deg);
    return triangle(tip, deg, 7);
  }

  function onCircle(cx, cy, r, deg) {
    const rad = deg * Math.PI / 180;
    return { x: cx + Math.cos(rad) * r, y: cy + Math.sin(rad) * r };
  }

  /** An arrowhead of half-width `size` pointing along `deg` from `tip`. */
  function triangle(tip, deg, size) {
    const rad = deg * Math.PI / 180;
    const back = { x: tip.x - Math.cos(rad) * size * 2, y: tip.y - Math.sin(rad) * size * 2 };
    const nx = -Math.sin(rad) * size;
    const ny = Math.cos(rad) * size;
    return [
      `${round(tip.x)},${round(tip.y)}`,
      `${round(back.x + nx)},${round(back.y + ny)}`,
      `${round(back.x - nx)},${round(back.y - ny)}`,
    ].join(" ");
  }

  function round(n) { return Math.round(n * 100) / 100; }

  function node(name, attrs) {
    const el = document.createElementNS(SVG_NS, name);
    if (attrs) {
      Object.keys(attrs).forEach((k) => el.setAttribute(k, String(attrs[k])));
    }
    if (!el.dataset) el.dataset = {};
    return el;
  }

  /** Apply the selected / assigned / testing classes to one motor's node. */
  function paintNode(state, number) {
    const el = state.nodes[number];
    if (!el) return;
    const motor = (state.doc && state.doc.motors || []).find((m) => m.number === number);
    el.classList.toggle("selected", state.selected === number);
    el.classList.toggle("testing", state.testing === number);
    el.classList.toggle("unassigned", !!motor && !motor.output);
  }

  function selectMotor(state, number) {
    if (state.selected === number) return;
    const previous = state.selected;
    state.selected = number;
    paintNode(state, previous);
    paintNode(state, number);
    renderMotorPanel(state);
    renderTestPanel(state);
  }

  // -------------------------------------------------------------------------
  // The selected motor
  // -------------------------------------------------------------------------

  function motorCard(state, doc, motors) {
    const card = S.el("div", "page-card motors-card");
    card.appendChild(S.sectionTitle("Selected motor"));
    const body = S.el("div", "motors-panel");
    card.appendChild(body);
    state.motorPanel = body;
    renderMotorPanel(state);
    return card;
  }

  function selectedMotor(state) {
    return ((state.doc && state.doc.motors) || [])
      .find((m) => m.number === state.selected) || null;
  }

  function renderMotorPanel(state) {
    const body = state.motorPanel;
    if (!body) return;
    body.innerHTML = "";
    dropControls(state, body);

    const motor = selectedMotor(state);
    if (!motor) {
      body.appendChild(S.el("div", "field-hint", "Select a motor in the diagram."));
      return;
    }

    const head = S.el("div", "motors-panel-head");
    head.appendChild(S.el("span", "motors-panel-title", motor.label));
    head.appendChild(S.el("span", "motors-panel-sub",
      motor.output ? `wired to ${motor.output.label}` : "not wired to an output"));
    body.appendChild(head);

    body.appendChild(assignmentRow(state, motor));
    body.appendChild(fieldGrid(state, motor.fields || []));
    if (!(motor.fields || []).length) {
      body.appendChild(S.el("div", "field-hint",
        "This firmware reports no position parameters for this motor."));
    }
    S.refreshIcons();
  }

  /**
   * Bank + pin, the two halves of "where is this motor plugged in".
   *
   * PX4 stores the reverse mapping, so the write is done by the backend
   * (POST /api/motors/assign): it clears the pin the motor is leaving, swaps
   * with another motor if the target is taken, and refuses rather than move a
   * servo off its pin.
   */
  function assignmentRow(state, motor) {
    const doc = state.doc || {};
    const banks = doc.banks || [];
    const row = S.el("div", "motors-assign");
    const status = S.el("span", "params-row-status", "");

    const bankSelect = Corvus.ui.select({
      className: "motors-select",
      ariaLabel: `Output bank for ${motor.label}`,
      options: [{ value: "", label: "Unassigned" }].concat(
        banks.map((b) => ({ value: b.id, label: b.label }))),
      value: motor.output ? motor.output.bank : "",
    });
    const pinSelect = Corvus.ui.select({
      className: "motors-select",
      ariaLabel: `Output pin for ${motor.label}`,
      options: pinOptions(doc, motor, motor.output ? motor.output.bank : ""),
      value: motor.output ? String(motor.output.pin) : "",
    });

    row.appendChild(Corvus.ui.field({ label: "Output bank", control: bankSelect }));
    row.appendChild(Corvus.ui.field({ label: "Output pin", control: pinSelect }));
    row.appendChild(status);

    bankSelect.addEventListener("change", () => {
      const bank = bankSelect.value;
      Corvus.ui.setOptions(pinSelect, pinOptions(doc, motor, bank), "");
      if (!bank) {
        assign(state, motor, null, status);
      } else {
        setFieldStatus(status, "", "");
      }
    });
    pinSelect.addEventListener("change", () => {
      const pin = Number(pinSelect.value);
      if (!bankSelect.value || !isFinite(pin) || !pin) return;
      assign(state, motor, { bank: bankSelect.value, pin }, status);
    });

    registerControl(state, bankSelect);
    registerControl(state, pinSelect);
    return row;
  }

  /** Pins of `bank`, each annotated with what it currently drives. */
  function pinOptions(doc, motor, bank) {
    const options = [{ value: "", label: bank ? "Choose a pin…" : "—" }];
    (doc.outputs || []).forEach((out) => {
      if (out.bank !== bank) return;
      const mine = out.motor === motor.number;
      const busy = !mine && out.function !== "Disabled";
      options.push({
        value: String(out.pin),
        label: busy ? `${out.label} — ${out.function}` : out.label,
      });
    });
    return options;
  }

  async function assign(state, motor, target, status) {
    setFieldStatus(status, "pending", "assigning");
    const body = target === null
      ? { motor: motor.number, output: null }
      : { motor: motor.number, bank: target.bank, pin: target.pin };
    try {
      await Corvus.telemetry.postAction("/api/motors/assign", body);
      setFieldStatus(status, "ok", "assigned");
      // The whole output table moved (a swap touches two motors), so re-read
      // rather than patch a guess into the diagram.
      load(state);
    } catch (err) {
      const msg = (err && err.message) || "assignment failed";
      setFieldStatus(status, "err", msg);
      notify("critical", `Could not assign ${motor.label}: ${msg}`);
      // Put the controls back on what the vehicle still holds.
      renderMotorPanel(state);
    }
  }

  // -------------------------------------------------------------------------
  // Motor test
  // -------------------------------------------------------------------------

  function testCard(state, motors) {
    const card = S.el("div", "page-card motors-card motors-test-card");
    card.appendChild(S.sectionTitle("Motor test"));

    // The warning is a permanent part of the card, not a dialog that can be
    // dismissed and forgotten: a spinning propeller is the hazard on this page.
    const warn = S.el("div", "motors-danger");
    warn.appendChild(S.el("strong", "motors-danger-title", "Remove all propellers first."));
    warn.appendChild(S.el("span", "motors-danger-text",
      "This spins the selected motor at the throttle below. A propeller on a motor that "
      + "starts unexpectedly will cut. Test on a bench, with the aircraft held down, and "
      + "never with props fitted."));
    card.appendChild(warn);

    const body = S.el("div", "motors-test");
    card.appendChild(body);
    state.testPanel = body;
    renderTestPanel(state);
    return card;
  }

  function renderTestPanel(state) {
    const body = state.testPanel;
    if (!body) return;
    body.innerHTML = "";
    dropControls(state, body);

    const motor = selectedMotor(state);

    const ack = Corvus.ui.toggle({
      value: state.propsOff,
      ariaLabel: "Propellers are removed",
      onChange: (next) => {
        state.propsOff = !!next;
        recheckAll(state);
      },
    });
    body.appendChild(Corvus.ui.field({
      label: "Propellers are removed", control: ack.el, className: "field-switch",
    }));

    const throttle = Corvus.ui.slider({
      value: state.throttle,
      steps: THROTTLE_STEPS.map((v) => ({ value: v, label: `${v}%` })),
      ariaLabel: "Test throttle",
      onChange: (v) => { state.throttle = Number(v); },
    });
    body.appendChild(Corvus.ui.field({ label: "Throttle", control: throttle.el }));

    const status = S.el("div", "params-row-status", "");
    const row = S.el("div", "motors-test-actions");
    const spinBtn = Corvus.ui.button({
      variant: "primary", size: "sm", icon: "play",
      label: motor ? `Spin ${motor.label}` : "Spin motor",
    });
    const stopBtn = Corvus.ui.button({
      variant: "secondary", size: "sm", icon: "octagon-x", label: "Stop all",
    });
    row.appendChild(spinBtn);
    row.appendChild(stopBtn);
    row.appendChild(status);
    body.appendChild(row);

    spinBtn.addEventListener("click", () => spinMotor(state, status));
    stopBtn.addEventListener("click", () => stopMotors(state, status));

    // The spin button needs all three: a motor selected, the propellers
    // acknowledged, and a disarmed vehicle. Stop is never gated — refusing to
    // stop a motor would be a safety regression.
    registerControl(state, spinBtn, () => {
      spinBtn.disabled = state.armed || !state.propsOff || !selectedMotor(state) || !!state.testing;
    });
    registerControl(state, ack.el, () => { ack.el.disabled = state.armed; });
    state.testStatus = status;
    S.refreshIcons();
  }

  async function spinMotor(state, status) {
    const motor = selectedMotor(state);
    if (!motor || !state.propsOff || state.armed) return;
    state.testing = motor.number;
    paintNode(state, motor.number);
    recheckAll(state);
    setFieldStatus(status, "pending",
      `${motor.label} spinning at ${state.throttle}% for ${TEST_DURATION_S}s`);
    try {
      await Corvus.telemetry.postAction("/api/motors/test", {
        motor: motor.number, throttle: state.throttle, duration: TEST_DURATION_S,
      });
      clearTestTimer(state);
      // The vehicle stops the motor itself when its timeout expires; this only
      // returns the UI to its resting state at the same moment.
      state.testTimer = window.setTimeout(() => {
        state.testTimer = null;
        endTest(state, "ok", `${motor.label} test finished`);
      }, TEST_DURATION_S * 1000);
    } catch (err) {
      const msg = (err && err.message) || "motor test failed";
      endTest(state, "err", msg);
      notify("critical", `Could not test ${motor.label}: ${msg}`);
    }
  }

  async function stopMotors(state, status) {
    clearTestTimer(state);
    setFieldStatus(status, "pending", "stopping");
    try {
      await Corvus.telemetry.postAction("/api/motors/test/stop", {});
      endTest(state, "ok", "motors stopped");
    } catch (err) {
      endTest(state, "err", (err && err.message) || "stop failed");
    }
  }

  function endTest(state, cls, text) {
    const was = state.testing;
    state.testing = 0;
    if (was) paintNode(state, was);
    setFieldStatus(state.testStatus, cls, text);
    recheckAll(state);
  }

  function clearTestTimer(state) {
    if (state.testTimer) {
      try { window.clearTimeout(state.testTimer); } catch (_e) {}
      state.testTimer = null;
    }
  }

  // -------------------------------------------------------------------------
  // Generic fields
  // -------------------------------------------------------------------------

  /**
   * A plain form: one labelled control per field, built by the shared
   * schema-driven form layer.
   *
   * The two page-specific parts are handed to it here. `signFallback` is the
   * magnitude a CW/CCW choice falls back to when the vehicle reports
   * CA_ROTORn_KM as exactly 0, whose sign says nothing; `onApplied` is what a
   * confirmed write means to this page — a moved motor changes the drawing, and
   * the airframe class or motor count changes which fields exist at all.
   */
  function fieldGrid(state, fields) {
    return S.paramFieldGrid(state, fields, {
      prefix: "motors",
      signFallback: DEFAULT_KM,
      onApplied: (field) => {
        if (field.reload || String(field.param || "").indexOf("CA_ROTOR") === 0) load(state);
      },
    });
  }

  /** Write CA_ROTOR_COUNT from the Add / Remove buttons, then re-read. */
  async function setMotorCount(state, count, btn, status) {
    if (state.armed) return;
    btn.disabled = true;
    setFieldStatus(status, "pending", `setting motor count to ${count}`);
    try {
      await Corvus.telemetry.postAction("/api/params/set",
        { name: "CA_ROTOR_COUNT", value: count });
      setFieldStatus(status, "ok", `motor count ${count}`);
      load(state);
    } catch (err) {
      const msg = (err && err.message) || "write failed";
      setFieldStatus(status, "err", msg);
      notify("critical", `Could not change the motor count: ${msg}`);
      recheckAll(state);
    }
  }

  /** The page-level status line above the cards. */
  function setStatus(state, cls, text) {
    S.setActionsStatus(state.actionsStatus, cls, text);
  }

  return { render };
})();
