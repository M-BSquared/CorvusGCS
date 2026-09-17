"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.rcTransmitter — a drawn transmitter for the Radio Control page.
 *
 * The channel bars answer "is channel 7 moving". They do not answer the
 * question the operator actually has, which is physical: "is THAT switch the
 * kill switch, and is it in the position I think it is." Between the handset
 * in their hands and a column of microsecond values there is a translation
 * step that is done from memory, and a kill switch bound to the wrong toggle
 * is a mistake that only shows up once.
 *
 * So this draws the handset, in the idiom the manuals use: a technical line
 * drawing of a twin-stick radio — two gimbals with their trims, three shoulder
 * switches per side, two knobs, screen, menu pad and scroll wheel — with every
 * bindable control named by a leader line out to the margin. That is the
 * manuals' convention because it works: the drawing stays uncluttered and the
 * words sit where there is room for them.
 *
 * The drawing then MOVES, from the live RC_CHANNELS stream:
 *
 *   show      what the calibration wizard is asking for right now — the
 *             gimbal to move and the direction to move it in, drawn rather
 *             than described in a sentence about "the left stick"
 *   test      whether a control is even reaching the vehicle: flip it and
 *             watch the lever swing to the position it is in and light. No
 *             arming, no props, no parameter writes.
 *   assign    bind a drawn control to the channel it sends on (learned by
 *             moving it), then give that channel a PX4 function.
 *
 * A switch is drawn as a real toggle — a mounting nut with a bat handle that
 * leans down, level or up — rather than as an abstract indicator, because that
 * is the shape the operator is looking at on the bench. The gimbals carry a
 * thumbstick each — a dished, knurled cap on a shaft — which travels across
 * its well exactly as the thumb travels: a cap up and to the right is a stick
 * held up and to the right, with nothing to decode.
 *
 * What this is NOT: an accurate drawing of any particular radio, and not a
 * claim about which controls your handset has. It is a *model* of the common
 * six-switch, two-knob layout, held as data in LAYOUT below, and every control
 * in it is inert until a channel is bound to it. A radio with four switches
 * simply leaves the other two unbound and grey.
 *
 * Which physical control sends on which channel is knowledge the vehicle does
 * not have — PX4 knows channel 7 is the kill switch, not that channel 7 is the
 * toggle above your left thumb. That mapping is therefore the ground station's
 * and is learned the only way it can be: the operator presses Learn and moves
 * the control, and the channel that moved is the answer (the same measurement
 * the page's Detect button and the calibration wizard are built on). It is
 * kept in localStorage per browser profile, because it describes the handset
 * on the bench and not the aircraft — swapping vehicles must not lose it, and
 * swapping transmitters must not carry it over silently, which is what the
 * Forget button in the header is for.
 *
 * Stick mode. Mode 2 puts throttle on the left gimbal; modes 1, 3 and 4 do not.
 * Since the stick bindings are taken from RC_MAP_THROTTLE and friends rather
 * than learned, the mode is what decides which drawn gimbal axis those land
 * on, and a drawing in the wrong mode would ask the operator to move the wrong
 * thumb. It is a header control, persisted next to the bindings.
 *
 * Direction. The drawing follows the OPERATOR'S HAND, not the pulse: a channel
 * the vehicle knows is reversed (RC<n>_REV = -1) is inverted here, because
 * reversed is precisely the statement "this transmitter's pulse falls when the
 * stick goes the way PX4 calls positive". The channel bars below the drawing
 * stay raw, so both the hand and the wire are on screen at once.
 *
 * Exposes:
 *   LAYOUT, MODES                  the drawn model and the four stick modes
 *   axisRoles(mode)                gimbal axis id -> stick role, for one mode
 *   positionIndex(fraction, n)     a normalised value -> switch position
 *   leverAngle(index, n)           a switch position -> the handle's lean
 *   loadBindings() / saveBindings(map) / loadMode() / saveMode(mode)
 *   create(opts) -> {el, update, setStickChannels, setChannelInfo, setMode,
 *                    setPrompt, channelMap, forgetAll, select, refresh,
 *                    destroy}
 *
 * Degrades to a labelled list where `createElementNS` is unavailable (the
 * DOM-stub test harness), following Corvus.calibFigures: the page must never
 * depend on SVG existing.
 */
Corvus.rcTransmitter = (function () {
  const SVG_NS = "http://www.w3.org/2000/svg";

  /** localStorage keys.
   *
   *  v2: the drawn model moved to the six-switch shoulder layout (SA-SD plus
   *  the two two-position outer switches SF and SH), which changes what some
   *  of the v1 ids MEAN — the old "SF" was a momentary button on the top edge.
   *  A binding read back under a name that now points somewhere else is a kill
   *  switch drawn on the wrong toggle, so the key is versioned and v1 is
   *  simply not read. Re-learning a switch is one press and one flip. */
  const BIND_KEY = "corvus.rc.transmitter.bindings.v2";
  const MODE_KEY = "corvus.rc.transmitter.mode.v1";

  /** Display scale when a channel has no trustworthy calibration of its own.
   *  Same bounds setup-control.js draws its bars against. */
  const PWM_MIN = 900;
  const PWM_MAX = 2100;

  /** How far a control must move for Learn to call it the one that moved, and
   *  how long it waits. Mirrors the page's own Detect for the same reason:
   *  a three-position switch's smallest step is ~400 us and a channel sitting
   *  still jitters by a handful. */
  const LEARN_TRAVEL_US = 250;
  const LEARN_TIMEOUT_MS = 8000;

  /* ================================================================== */
  /* The drawn model                                                     */
  /* ================================================================== */

  /**
   * Geometry and identity of the whole drawing, in one table.
   *
   * Coordinates are in the SVG's own 880x620 viewBox, which is wider than the
   * handset because the outer columns are the label margins. Kept as data
   * because the alternative — geometry inlined at each draw call — is what
   * makes a layout impossible to correct later without re-reading the
   * renderer.
   *
   * `kind` decides both how a control is drawn and how a pulse is read off it:
   *   axis    a gimbal axis; continuous, drawn as the thumbstick's travel
   *   switch  n discrete positions; drawn as a bat handle leaning to one
   *   knob    a continuous rotary; drawn as a pointer angle
   *
   * `callout` is where its name and live reading sit in the margin, and which
   * point on the drawing the leader line runs to. The leader always ends on
   * something that does NOT move — a switch's mounting nut, not its handle —
   * because a leader line that twitches with the stream is one nobody can
   * follow.
   */
  const LAYOUT = {
    view: { w: 880, h: 650 },

    /* --- the shell ---------------------------------------------------- */
    body: "M 356 150 L 524 150 Q 552 150 574 163 L 690 219 Q 728 237 728 272 "
      + "L 728 500 Q 728 536 700 554 L 636 592 Q 614 604 586 604 L 294 604 "
      + "Q 266 604 244 592 L 180 554 Q 152 536 152 500 L 152 272 "
      + "Q 152 237 190 219 L 306 163 Q 328 150 356 150 Z",
    /** The antenna stands up rather than folding across the case. A folded
     *  blade is the more characteristic silhouette, but it lies across the
     *  whole top of the drawing — which is exactly where the knobs' leader
     *  lines run, and a label you cannot follow to its control is worse than
     *  a less characteristic antenna. */
    antenna: {
      mast: { x: 414, y: 72, w: 32, h: 56, rx: 11 },
      hinge: { x: 408, y: 116, w: 44, h: 38, rx: 9 },
    },
    brand: { x: 440, y: 196, label: "CORVUS" },
    chin: { x: 386, y: 604, w: 108, h: 18, rx: 7 },

    gimbals: [
      { id: "left", cx: 306, cy: 372, r: 84 },
      { id: "right", cx: 574, cy: 372, r: 84 },
    ],

    /** Trim tabs: drawn only. A trim shifts the channel its gimbal already
     *  sends on rather than carrying one of its own, so there is nothing here
     *  to bind and nothing to light. */
    trims: [
      { x: 418, y: 320, w: 22, h: 104, axis: "v" },
      { x: 462, y: 320, w: 22, h: 104, axis: "v" },
      { x: 246, y: 468, w: 120, h: 22, axis: "h" },
      { x: 514, y: 468, w: 120, h: 22, axis: "h" },
    ],
    modeText: { x: 440, y: 486 },

    screen: { x: 356, y: 506, w: 168, h: 84 },

    /** The handset's own user-interface controls. Drawn because a radio
     *  without them does not look like a radio, and explicitly NOT bindable:
     *  they run the transmitter's menus and never reach a channel, so offering
     *  to bind one would be offering a mapping that can never fire. */
    nav: { cx: 284, cy: 548, r: 36, labels: ["PAGE", "MENU", "EXIT"] },
    wheel: { cx: 596, cy: 548, r: 36, label: "ENTER" },

    /* --- the bindable controls ----------------------------------------
       The switch nuts descend across each shoulder rather than sitting in a
       row: the handles all swing through the same arc, so a row would put each
       one's "down" position on top of its neighbour's nut. The stagger is what
       keeps six switches legible at every position at once. */
    controls: [
      // Gimbal axes. Each gets its own callout because each is its own
      // channel; the gimbal itself would be ambiguous between the two.
      { id: "left_y", kind: "axis", gimbal: "left", axis: "y",
        label: "Left stick \u2195",
        callout: { side: "left", y: 370, target: [222, 372] } },
      { id: "left_x", kind: "axis", gimbal: "left", axis: "x",
        label: "Left stick \u2194",
        callout: { side: "left", y: 458, target: [306, 456] } },
      { id: "right_y", kind: "axis", gimbal: "right", axis: "y",
        label: "Right stick \u2195",
        callout: { side: "right", y: 370, target: [658, 372] } },
      { id: "right_x", kind: "axis", gimbal: "right", axis: "x",
        label: "Right stick \u2194",
        callout: { side: "right", y: 458, target: [574, 456] } },

      // Shoulder switches, inboard to outboard. The outer one on each side is
      // the two-position one, which is the usual arrangement on handsets in
      // this class; every count is editable per control, because this is a
      // model of a radio and not a specification of one.
      { id: "SA", kind: "switch", positions: 3, side: "left",
        cx: 334, cy: 214, lever: 44, label: "SA",
        callout: { side: "left", y: 126, target: [334, 214] } },
      { id: "SB", kind: "switch", positions: 3, side: "left",
        cx: 270, cy: 240, lever: 44, label: "SB",
        callout: { side: "left", y: 200, target: [270, 240] } },
      { id: "SF", kind: "switch", positions: 2, side: "left",
        cx: 200, cy: 272, lever: 44, label: "SF",
        callout: { side: "left", y: 276, target: [200, 272] } },
      { id: "SD", kind: "switch", positions: 3, side: "right",
        cx: 546, cy: 214, lever: 44, label: "SD",
        callout: { side: "right", y: 126, target: [546, 214] } },
      { id: "SC", kind: "switch", positions: 3, side: "right",
        cx: 610, cy: 240, lever: 44, label: "SC",
        callout: { side: "right", y: 200, target: [610, 240] } },
      { id: "SH", kind: "switch", positions: 2, side: "right",
        cx: 680, cy: 272, lever: 44, label: "SH",
        callout: { side: "right", y: 276, target: [680, 272] } },

      // Knobs, inboard under the brand plate. Their labels hang over the top
      // of the drawing rather than joining a side column: from the margin the
      // leader would have to cross three switch handles to reach them.
      { id: "S1", kind: "knob", cx: 382, cy: 252, r: 21, label: "S1",
        callout: { side: "top-left", y: 80, target: [382, 252] } },
      { id: "S2", kind: "knob", cx: 498, cy: 252, r: 21, label: "S2",
        callout: { side: "top-right", y: 80, target: [498, 252] } },
    ],
  };

  /** Where a callout's text sits, where its leader leaves it, and where its
   *  hit box starts. A `stub` of null is a label above the drawing, whose
   *  leader drops straight out of the text instead of running along a rule. */
  const CALLOUT = {
    left: { text: 136, stub: 176, anchor: "end", box: 12 },
    right: { text: 744, stub: 704, anchor: "start", box: 740 },
    "top-left": { text: 300, stub: null, anchor: "middle", box: 236 },
    "top-right": { text: 580, stub: null, anchor: "middle", box: 516 },
  };

  /** Fast lookup for a control by id. */
  const CONTROL_BY_ID = {};
  LAYOUT.controls.forEach((c) => { CONTROL_BY_ID[c.id] = c; });

  /**
   * The four stick modes, as gimbal-axis -> stick role.
   *
   * These are the standard RC modes and the reason the drawing needs to know
   * which one is in use at all: the four RC_MAP_* stick parameters say which
   * CHANNEL carries throttle, never which thumb does.
   */
  const MODES = {
    1: { left_y: "pitch", left_x: "yaw", right_y: "throttle", right_x: "roll" },
    2: { left_y: "throttle", left_x: "yaw", right_y: "pitch", right_x: "roll" },
    3: { left_y: "pitch", left_x: "roll", right_y: "throttle", right_x: "yaw" },
    4: { left_y: "throttle", left_x: "roll", right_y: "pitch", right_x: "yaw" },
  };

  const DEFAULT_MODE = 2;

  /** The PX4 parameter each stick role is mapped by. */
  const ROLE_PARAM = {
    throttle: "RC_MAP_THROTTLE",
    roll: "RC_MAP_ROLL",
    pitch: "RC_MAP_PITCH",
    yaw: "RC_MAP_YAW",
  };

  const ROLE_LABEL = {
    throttle: "Throttle", roll: "Roll", pitch: "Pitch", yaw: "Yaw",
  };

  /** Axis roles for one mode, defaulting to mode 2 for an unknown value. */
  function axisRoles(mode) {
    return MODES[Number(mode)] || MODES[DEFAULT_MODE];
  }

  /* ================================================================== */
  /* Pure mapping: a pulse to something drawable                         */
  /* ================================================================== */

  /** Normalised 0..1 position of a pulse between two endpoints. */
  function fractionOf(pulse, min, max) {
    const lo = Number(min) || PWM_MIN;
    const hi = Number(max) || PWM_MAX;
    const span = hi - lo;
    if (!(span > 0)) return 0.5;
    return Math.max(0, Math.min(1, (Number(pulse) - lo) / span));
  }

  /**
   * Which of `positions` discrete slots a normalised value sits in.
   *
   * Equal bands, lowest slot first, so slot 0 is the position the operator
   * calls "down" and the last is "up". Deliberately not PX4's six-slot mode
   * arithmetic: that one has margins tuned for selecting a flight mode from a
   * possibly uncalibrated channel, and applying it to a two-position toggle
   * would put the boundary somewhere other than the middle.
   */
  function positionIndex(fraction, positions) {
    const n = Math.max(2, Math.min(6, Math.round(Number(positions) || 2)));
    const f = Math.max(0, Math.min(1, Number(fraction) || 0));
    return Math.max(0, Math.min(n - 1, Math.floor(f * n)));
  }

  /** Human name for a switch position, given how many it has. */
  function positionLabel(index, positions) {
    if (positions <= 2) return index === 0 ? "Down" : "Up";
    if (positions === 3) return ["Down", "Middle", "Up"][index] || String(index + 1);
    return "Position " + (index + 1);
  }

  /**
   * How far above horizontal a bat handle leans, in degrees, for one position.
   *
   * The whole sweep sits in the upper quadrant, because that is where a real
   * switch's handle lives: it stands up out of the case and leans outboard as
   * it comes down, rather than lying flat across the shoulder. A sweep that
   * started below horizontal drew six handles sticking out sideways, which
   * looked like six levers and not like a radio.
   *
   * Sixty degrees of swing is still plenty to tell three positions apart at a
   * glance — the tip moves most of the handle's own length between them.
   */
  const LEVER_LOW = 28;
  const LEVER_HIGH = 90;

  /** Radius of the thumbstick cap. Sized against the 84-unit gimbal well so
   *  the cap is unmistakably a thumb-sized control rather than a marker, and
   *  still leaves a throw worth watching at full deflection. */
  const STICK_R = 27;

  function leverAngle(index, positions) {
    const n = Math.max(2, Math.round(Number(positions) || 2));
    const i = Math.max(0, Math.min(n - 1, Number(index) || 0));
    return LEVER_LOW + (LEVER_HIGH - LEVER_LOW) * (i / (n - 1));
  }

  /* ================================================================== */
  /* Binding store                                                       */
  /* ================================================================== */

  /**
   * Bindings as {controlId: {channel, positions}}.
   *
   * `positions` is stored beside the channel rather than read from LAYOUT
   * because it is the one thing about a control that the drawn model can get
   * wrong for a real handset — a two-position SB is common — and correcting it
   * has to survive a reload.
   */
  function loadBindings() {
    let raw = null;
    try { raw = window.localStorage.getItem(BIND_KEY); } catch (_e) { return {}; }
    if (!raw) return {};
    let parsed = null;
    try { parsed = JSON.parse(raw); } catch (_e) { return {}; }
    if (!parsed || typeof parsed !== "object") return {};
    const clean = {};
    Object.keys(parsed).forEach((id) => {
      if (!CONTROL_BY_ID[id]) return;
      const entry = parsed[id] || {};
      const channel = Number(entry.channel);
      if (!(channel >= 1)) return;
      clean[id] = { channel: Math.round(channel) };
      const positions = Number(entry.positions);
      if (positions >= 2) clean[id].positions = Math.round(positions);
    });
    return clean;
  }

  function saveBindings(map) {
    try { window.localStorage.setItem(BIND_KEY, JSON.stringify(map || {})); }
    catch (_e) { /* private mode, a full quota — the drawing still works */ }
  }

  function loadMode() {
    let raw = null;
    try { raw = window.localStorage.getItem(MODE_KEY); } catch (_e) { return DEFAULT_MODE; }
    const mode = Number(raw);
    return MODES[mode] ? mode : DEFAULT_MODE;
  }

  function saveMode(mode) {
    try { window.localStorage.setItem(MODE_KEY, String(mode)); } catch (_e) {}
  }

  /* ================================================================== */
  /* SVG helpers                                                         */
  /* ================================================================== */

  function svgSupported() {
    return typeof document !== "undefined"
      && typeof document.createElementNS === "function";
  }

  function node(name, attrs) {
    const element = document.createElementNS(SVG_NS, name);
    if (attrs) {
      Object.keys(attrs).forEach((key) => {
        element.setAttribute(key, String(attrs[key]));
      });
    }
    return element;
  }

  function label(cls, x, y, anchor, content) {
    const t = node("text", { class: cls, x: x, y: y, "text-anchor": anchor });
    t.textContent = content == null ? "" : String(content);
    return t;
  }

  function el(tag, cls, content) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (content !== undefined) e.textContent = content;
    return e;
  }

  /* ================================================================== */
  /* The widget                                                          */
  /* ================================================================== */

  /**
   * Build the transmitter.
   *
   * opts:
   *   interactive  whether controls can be selected and bound (the overview
   *                sets this; the calibration wizard does not, because a page
   *                that is measuring endpoints must not also be writing
   *                mappings underneath the measurement)
   *   onSelect(controlId|null)          selection changed
   *   onAssign(controlId, binding|null) a Learn finished, or a Forget cleared
   *   renderFunctions(channel, controlId, armed) -> node|null
   *                what this channel does on the vehicle, shown in the
   *                inspector under a learned control
   */
  function create(opts) {
    const o = opts || {};
    const interactive = o.interactive !== false;

    const root = el("div", "rc-tx");
    root.dataset.interactive = interactive ? "1" : "";

    let bindings = loadBindings();
    let mode = loadMode();
    /** Per-channel calibration, as {channel: {min, max, reversed}} — handed in
     *  by the page from what the vehicle reports. Absent for a channel that
     *  was never calibrated, which is exactly when the display scale is used
     *  instead. */
    let channelInfo = {};
    /** {role: channel} taken from the vehicle's stick mapping. */
    let stickChannels = {};
    let channels = [];
    let live = false;
    // PX4 refuses a parameter write while armed, so the function picker under a
    // control has to be rebuilt when that flips — a picker left enabled over an
    // armed vehicle is the widget lying about what it can do.
    let armed = false;
    // The last frame, kept so a repaint can be forced between frames — see
    // setPrompt: a step change must redraw now, not at the next RC_CHANNELS.
    let lastState = null;
    let selectedId = null;
    let prompt = { controls: [], gimbal: null, direction: null, swept: [] };
    let learn = null;
    // Written by renderInspector; the Learn handlers report into it.
    let learnStatusEl = null;
    let inspectorKey = "";
    let liveValue = null;

    /* ---------------- the drawing ---------------- */

    const figure = el("div", "rc-tx-figure");
    root.appendChild(figure);

    // Per-control handles for the update loop: the pieces whose attributes
    // change every frame, looked up by control id rather than queried. At
    // 20 Hz a querySelector per control per frame is the whole frame budget.
    const parts = {};
    const gimbalParts = {};
    let screenParts = null;
    let modeLabel = null;

    if (svgSupported()) {
      figure.appendChild(buildSvg());
    } else {
      figure.appendChild(buildFallback());
    }

    /* ---------------- shell ---------------- */

    function buildSvg() {
      const s = node("svg", {
        viewBox: "0 0 " + LAYOUT.view.w + " " + LAYOUT.view.h,
        class: "rc-tx-svg",
        role: "img",
        "aria-label": "Transmitter, live control positions",
      });

      const mast = LAYOUT.antenna.mast;
      s.appendChild(node("rect", {
        class: "rc-tx-antenna", x: mast.x, y: mast.y,
        width: mast.w, height: mast.h, rx: mast.rx,
      }));
      const hinge = LAYOUT.antenna.hinge;
      s.appendChild(node("rect", {
        class: "rc-tx-antenna-hinge", x: hinge.x, y: hinge.y,
        width: hinge.w, height: hinge.h, rx: hinge.rx,
      }));
      s.appendChild(node("rect", {
        class: "rc-tx-chin", x: LAYOUT.chin.x, y: LAYOUT.chin.y,
        width: LAYOUT.chin.w, height: LAYOUT.chin.h, rx: LAYOUT.chin.rx,
      }));
      s.appendChild(node("path", { class: "rc-tx-body", d: LAYOUT.body }));
      s.appendChild(label("rc-tx-brand", LAYOUT.brand.x, LAYOUT.brand.y,
        "middle", LAYOUT.brand.label));

      buildScreen(s);
      buildChrome(s);
      LAYOUT.trims.forEach((trim) => buildTrim(s, trim));
      modeLabel = label("rc-tx-case-text", LAYOUT.modeText.x, LAYOUT.modeText.y,
        "middle", "MODE " + mode);
      s.appendChild(modeLabel);

      LAYOUT.gimbals.forEach((g) => buildGimbal(s, g));

      // Controls after the shell so nothing paints over a lit lever, and the
      // callouts after those so a leader line is never hidden by the case.
      LAYOUT.controls.forEach((control) => {
        const part = control.kind === "axis" ? {}
          : control.kind === "knob" ? buildKnob(s, control)
            : buildSwitch(s, control);
        part.kind = control.kind;
        part.nodes = part.group ? [part.group] : [];
        parts[control.id] = part;
      });
      LAYOUT.controls.forEach((control) => {
        const callout = buildCallout(s, control);
        parts[control.id].callout = callout;
        parts[control.id].nodes.push(callout.group);
      });

      return s;
    }

    function buildScreen(s) {
      const sc = LAYOUT.screen;
      s.appendChild(node("rect", {
        class: "rc-tx-screen-bezel", x: sc.x - 7, y: sc.y - 7,
        width: sc.w + 14, height: sc.h + 14, rx: 10,
      }));
      s.appendChild(node("rect", {
        class: "rc-tx-screen", x: sc.x, y: sc.y, width: sc.w, height: sc.h, rx: 4,
      }));
      s.appendChild(label("rc-tx-screen-head", sc.x + sc.w / 2, sc.y + 24,
        "middle", "CORVUS GCS"));
      const line = label("rc-tx-screen-line", sc.x + sc.w / 2, sc.y + 54,
        "middle", "NO SIGNAL");
      s.appendChild(line);
      const sub = label("rc-tx-screen-sub", sc.x + sc.w / 2, sc.y + 74, "middle", "");
      s.appendChild(sub);
      screenParts = { line, sub };
    }

    /** Menu pad and scroll wheel: inert, and drawn so it is obvious they are.
     *  These run the handset's own menus and never reach a channel. */
    function buildChrome(s) {
      const nav = LAYOUT.nav;
      s.appendChild(node("circle", {
        class: "rc-tx-nav", cx: nav.cx, cy: nav.cy, r: nav.r,
      }));
      s.appendChild(node("circle", {
        class: "rc-tx-nav-hub", cx: nav.cx, cy: nav.cy, r: 11,
      }));
      s.appendChild(label("rc-tx-chrome-label", nav.cx, nav.cy - 21, "middle",
        nav.labels[0]));
      s.appendChild(label("rc-tx-chrome-label", nav.cx, nav.cy + 29, "middle",
        nav.labels[2]));

      const wheel = LAYOUT.wheel;
      s.appendChild(node("circle", {
        class: "rc-tx-nav", cx: wheel.cx, cy: wheel.cy, r: wheel.r,
      }));
      // Knurling, so the wheel reads as a wheel and not a second menu pad.
      for (let i = 0; i < 24; i += 1) {
        const a = (i / 24) * Math.PI * 2;
        s.appendChild(node("line", {
          class: "rc-tx-knurl",
          x1: wheel.cx + Math.cos(a) * (wheel.r - 10),
          y1: wheel.cy + Math.sin(a) * (wheel.r - 10),
          x2: wheel.cx + Math.cos(a) * (wheel.r - 2),
          y2: wheel.cy + Math.sin(a) * (wheel.r - 2),
        }));
      }
      s.appendChild(node("circle", {
        class: "rc-tx-nav-hub", cx: wheel.cx, cy: wheel.cy, r: 17,
      }));
    }

    function buildTrim(s, trim) {
      s.appendChild(node("rect", {
        class: "rc-tx-trim", x: trim.x, y: trim.y,
        width: trim.w, height: trim.h, rx: Math.min(trim.w, trim.h) / 2,
      }));
      const cx = trim.x + trim.w / 2;
      const cy = trim.y + trim.h / 2;
      if (trim.axis === "v") {
        s.appendChild(label("rc-tx-trim-mark", cx, trim.y + 13, "middle", "▲"));
        s.appendChild(label("rc-tx-trim-mark", cx, trim.y + trim.h - 4, "middle", "▼"));
        s.appendChild(node("rect", {
          class: "rc-tx-trim-thumb", x: trim.x + 3, y: cy - 7,
          width: trim.w - 6, height: 14, rx: 4,
        }));
      } else {
        s.appendChild(label("rc-tx-trim-mark", trim.x + 10, cy + 5, "middle", "◀"));
        s.appendChild(label("rc-tx-trim-mark", trim.x + trim.w - 10, cy + 5, "middle", "▶"));
        s.appendChild(node("rect", {
          class: "rc-tx-trim-thumb", x: cx - 7, y: trim.y + 3,
          width: 14, height: trim.h - 6, rx: 4,
        }));
      }
    }

    /* ---------------- gimbals ---------------- */

    /**
     * A gimbal: the boot ring it sits in, and the thumbstick standing in it.
     *
     * The stick is drawn as the thing the thumb actually touches — a dished,
     * knurled cap on a shaft — and the whole assembly translates across the
     * well as the two channels move. That is the one drawing of a stick that
     * needs no key: a cap up and to the right IS a stick held up and to the
     * right, where an abstract marker in a circle has to be decoded first.
     *
     * The shaft is drawn from the well's centre to the cap, so how far the
     * stick is deflected reads as a length as well as a position — at the
     * centre there is no shaft at all, which is exactly the state the
     * calibration's "centre the sticks" step is asking the operator to reach.
     */
    function buildGimbal(s, g) {
      const group = node("g", { class: "rc-tx-gimbal" });
      group.appendChild(node("circle", {
        class: "rc-tx-gimbal-boot", cx: g.cx, cy: g.cy, r: g.r,
      }));
      group.appendChild(node("circle", {
        class: "rc-tx-gimbal-well", cx: g.cx, cy: g.cy, r: g.r - 13,
      }));
      // The ring of ticks is what tells the eye where centre is once the cap
      // has moved off it.
      for (let i = 0; i < 40; i += 1) {
        const a = (i / 40) * Math.PI * 2;
        group.appendChild(node("line", {
          class: "rc-tx-gimbal-tick",
          x1: g.cx + Math.cos(a) * (g.r - 22),
          y1: g.cy + Math.sin(a) * (g.r - 22),
          x2: g.cx + Math.cos(a) * (g.r - 15),
          y2: g.cy + Math.sin(a) * (g.r - 15),
        }));
      }
      group.appendChild(node("circle", {
        class: "rc-tx-gimbal-centre", cx: g.cx, cy: g.cy, r: 4,
      }));

      const arrow = node("path", { class: "rc-tx-arrow", d: "", opacity: 0 });
      group.appendChild(arrow);

      const shaft = node("line", {
        class: "rc-tx-stick-shaft", x1: g.cx, y1: g.cy, x2: g.cx, y2: g.cy,
      });
      group.appendChild(shaft);

      // The cap rides in its own group so one transform moves every piece of
      // it — rim, dish and knurling — instead of eighteen attribute writes a
      // frame.
      const cap = node("g", { class: "rc-tx-stick" });
      cap.appendChild(node("circle", {
        class: "rc-tx-stick-rim", cx: g.cx, cy: g.cy, r: STICK_R,
      }));
      for (let i = 0; i < 20; i += 1) {
        const a = (i / 20) * Math.PI * 2;
        cap.appendChild(node("line", {
          class: "rc-tx-stick-knurl",
          x1: g.cx + Math.cos(a) * (STICK_R - 7),
          y1: g.cy + Math.sin(a) * (STICK_R - 7),
          x2: g.cx + Math.cos(a) * (STICK_R - 1.5),
          y2: g.cy + Math.sin(a) * (STICK_R - 1.5),
        }));
      }
      cap.appendChild(node("circle", {
        class: "rc-tx-stick-dish", cx: g.cx, cy: g.cy, r: STICK_R - 9,
      }));
      group.appendChild(cap);

      s.appendChild(group);
      gimbalParts[g.id] = { group, cap, shaft, arrow, geom: g };
    }

    /* ---------------- switches and knobs ---------------- */

    /**
     * A toggle: a mounting nut and a bat handle that leans to its position.
     *
     * The handle is drawn pointing outboard at rest and rotated about the nut,
     * so "up" on the drawing is up on the handset — the operator compares two
     * pictures rather than decoding an indicator.
     */
    function buildSwitch(s, control) {
      const group = node("g", { class: "rc-tx-control rc-tx-switch" });
      group.setAttribute("data-control", control.id);

      group.appendChild(node("circle", {
        class: "rc-tx-nut", cx: control.cx, cy: control.cy, r: 12,
      }));
      group.appendChild(node("circle", {
        class: "rc-tx-nut-hub", cx: control.cx, cy: control.cy, r: 5,
      }));

      const out = control.side === "left" ? -1 : 1;
      const lever = node("g", { class: "rc-tx-lever" });
      lever.appendChild(node("line", {
        class: "rc-tx-lever-stem",
        x1: control.cx, y1: control.cy,
        x2: control.cx + out * control.lever, y2: control.cy,
      }));
      lever.appendChild(node("circle", {
        class: "rc-tx-lever-ball",
        cx: control.cx + out * control.lever, cy: control.cy, r: 9,
      }));
      group.appendChild(lever);

      s.appendChild(group);
      attach(group, control.id);
      return { group, lever, out };
    }

    /** A rotary: a capped body with a pointer swept across KNOB_SPAN. */
    function buildKnob(s, control) {
      const group = node("g", { class: "rc-tx-control rc-tx-knob" });
      group.setAttribute("data-control", control.id);
      group.appendChild(node("circle", {
        class: "rc-tx-knob-body", cx: control.cx, cy: control.cy, r: control.r,
      }));
      group.appendChild(node("circle", {
        class: "rc-tx-knob-cap", cx: control.cx, cy: control.cy, r: control.r - 7,
      }));
      const pointer = node("line", {
        class: "rc-tx-knob-pointer",
        x1: control.cx, y1: control.cy - 3,
        x2: control.cx, y2: control.cy - control.r + 3,
      });
      group.appendChild(pointer);
      s.appendChild(group);
      attach(group, control.id);
      return { group, pointer };
    }

    /* ---------------- callouts ---------------- */

    /**
     * One label in the margin, its leader line, and the dot where the line
     * lands on the drawing.
     *
     * The label is a click target in its own right. On a drawing this dense a
     * two-line label in clear space is an easier thing to hit than an
     * twelve-pixel mounting nut, and it is the only way to reach a stick AXIS
     * at all — the gimbal itself cannot say which of its two channels a click
     * meant.
     */
    function buildCallout(s, control) {
      const spec = control.callout;
      const side = CALLOUT[spec.side];
      const group = node("g", { class: "rc-tx-callout" });
      group.setAttribute("data-control", control.id);

      const tail = spec.target[0] + "," + spec.target[1];
      group.appendChild(node("polyline", {
        class: "rc-tx-leader",
        points: side.stub == null
          ? side.text + "," + (spec.y + 22) + " " + tail
          : side.text + "," + spec.y + " " + side.stub + "," + spec.y + " " + tail,
      }));
      group.appendChild(node("circle", {
        class: "rc-tx-leader-dot", cx: spec.target[0], cy: spec.target[1], r: 3.5,
      }));

      group.appendChild(label("rc-tx-callout-name", side.text, spec.y - 8,
        side.anchor, control.label));
      const value = label("rc-tx-callout-value", side.text, spec.y + 13,
        side.anchor, "—");
      group.appendChild(value);

      // The hit box, drawn last and transparent: `fill: none` would take no
      // pointer events at all, which is the whole point of having it.
      group.appendChild(node("rect", {
        class: "rc-tx-callout-hit", x: side.box, y: spec.y - 27,
        width: 128, height: 46,
      }));

      s.appendChild(group);
      attach(group, control.id);
      return { group, value };
    }

    /** No-SVG fallback: the same controls as a labelled list, so the wizard's
     *  prompt and the bound channels are still readable. */
    function buildFallback() {
      const list = el("div", "rc-tx-fallback");
      LAYOUT.controls.forEach((control) => {
        const row = el("div", "rc-tx-fallback-row");
        row.dataset.control = control.id;
        row.appendChild(el("span", "rc-tx-fallback-label", control.label));
        const value = el("span", "rc-tx-fallback-value", "—");
        row.appendChild(value);
        list.appendChild(row);
        parts[control.id] = {
          kind: control.kind, fallback: true, value, nodes: [row],
        };
        attach(row, control.id);
      });
      return list;
    }

    /**
     * Make one drawn control selectable.
     *
     * Reachable from the keyboard as well as the pointer: this is the surface
     * that binds a kill switch, and a control that can only be reached by
     * clicking a shape in an SVG is one an operator on a trackpad in a field
     * cannot reach at all.
     */
    function attach(element, controlId) {
      if (!interactive || !element.addEventListener) return;
      const control = CONTROL_BY_ID[controlId];
      if (element.setAttribute) {
        element.setAttribute("tabindex", "0");
        element.setAttribute("role", "button");
        element.setAttribute("aria-label", (control && control.label) || controlId);
      }
      element.addEventListener("click", () => { select(controlId); });
      element.addEventListener("keydown", (event) => {
        const key = event && event.key;
        if (key !== "Enter" && key !== " " && key !== "Spacebar") return;
        if (event.preventDefault) event.preventDefault();
        select(controlId);
      });
    }

    /* ---------------- the inspector ---------------- */

    const inspector = el("div", "rc-tx-inspector");
    inspector.hidden = true;
    if (interactive) root.appendChild(inspector);

    const legend = el("div", "rc-tx-legend");
    if (interactive) {
      legend.appendChild(el("span", "rc-tx-legend-text",
        "Click a control — on the drawing or on its label — to see the channel "
        + "it sends on, or to teach it one."));
      root.appendChild(legend);
    }

    /* ================================================================ */
    /* State in, pixels out                                             */
    /* ================================================================ */

    /** The channel a control is on, or 0. Stick axes come from the vehicle's
     *  own RC_MAP_* through the mode; everything else is learned. */
    function channelOf(controlId) {
      const control = CONTROL_BY_ID[controlId];
      if (!control) return 0;
      if (control.kind === "axis") {
        const role = axisRoles(mode)[controlId];
        const mapped = role ? Number(stickChannels[role]) : 0;
        if (mapped >= 1) return mapped;
      }
      const bound = bindings[controlId];
      return bound ? Number(bound.channel) || 0 : 0;
    }

    function positionsOf(controlId) {
      const control = CONTROL_BY_ID[controlId] || {};
      const bound = bindings[controlId];
      if (bound && bound.positions >= 2) return bound.positions;
      return control.positions || 2;
    }

    /**
     * A control's normalised 0..1 travel, or null when it has no channel or
     * no signal.
     *
     * Scaled against the channel's own calibrated endpoints where the vehicle
     * has them, because that is what makes a three-position switch land in the
     * middle of its middle position rather than wherever 900-2100 happens to
     * put it. Reversed channels are inverted here — see the file docstring:
     * this drawing follows the hand.
     */
    function travelOf(controlId) {
      const channel = channelOf(controlId);
      if (!channel || !live) return null;
      const pulse = Number(channels[channel - 1]) || 0;
      if (pulse <= 0) return null;
      const info = channelInfo[channel] || {};
      let f = fractionOf(pulse, info.min, info.max);
      if (info.reversed) f = 1 - f;
      return f;
    }

    function update(state) {
      lastState = state || null;
      channels = (state && state.rc_channels) || [];
      live = !!(state && state.rc_live) && channels.length > 0;
      const nextArmed = !!(state && state.armed);
      if (nextArmed !== armed) { armed = nextArmed; inspectorKey = ""; }

      if (screenParts) {
        const rssi = Number(state && state.rc_rssi);
        screenParts.line.textContent = live ? channels.length + " CH" : "NO SIGNAL";
        screenParts.sub.textContent = live && rssi >= 0 ? "RSSI " + rssi + "%" : "";
      }
      if (modeLabel) modeLabel.textContent = "MODE " + mode;

      LAYOUT.controls.forEach((control) => {
        const part = parts[control.id];
        if (!part) return;
        const travel = travelOf(control.id);
        const channel = channelOf(control.id);
        const index = travel == null ? -1
          : positionIndex(travel, positionsOf(control.id));

        part.nodes.forEach((n) => {
          if (!n.dataset) return;
          n.dataset.bound = channel ? "1" : "";
          n.dataset.signal = travel == null ? "" : "1";
          n.dataset.selected = selectedId === control.id ? "1" : "";
          n.dataset.prompted = prompt.controls.indexOf(control.id) >= 0 ? "1" : "";
          n.dataset.swept = prompt.swept.indexOf(control.id) >= 0 ? "1" : "";
          n.dataset.position = index >= 0 ? String(index) : "";
          n.dataset.on = onState(control, travel, index) ? "1" : "";
        });

        const reading = describe(control.id, travel);
        if (part.fallback) { part.value.textContent = reading; return; }
        if (part.callout && part.callout.value.textContent !== reading) {
          part.callout.value.textContent = reading;
        }

        if (control.kind === "switch") paintSwitch(control, part, index);
        else if (control.kind === "knob") paintKnob(control, part, travel);
      });

      paintGimbals();
      if (selectedId) renderInspector();
      if (learn) learn.sample(state);
    }

    /** Whether a control reads as "doing something" rather than at rest. */
    function onState(control, travel, index) {
      if (travel == null) return false;
      if (control.kind === "switch") return index > 0;
      // A knob is continuous, so "on" is meaningless; what is useful is
      // whether it has left its centre, which is what a tuning knob does.
      if (control.kind === "knob") return Math.abs(travel - 0.5) > 0.1;
      return false;
    }

    function paintSwitch(control, part, index) {
      // No signal parks the handle level and dimmed rather than at a position
      // it might not be in: a lever frozen where it was last seen is a switch
      // that looks like it is still being held.
      const angle = index < 0
        ? (LEVER_LOW + LEVER_HIGH) / 2
        : leverAngle(index, positionsOf(control.id));
      // The handle is drawn lying outboard and rotated up to its angle. SVG
      // rotates clockwise, so a left-hand handle lifts on a positive angle and
      // a right-hand one on a negative.
      const applied = part.out < 0 ? angle : -angle;
      part.lever.setAttribute("transform",
        "rotate(" + applied.toFixed(1) + " " + control.cx + " " + control.cy + ")");
    }

    /** Knob sweep, in degrees either side of straight up. */
    const KNOB_SPAN = 140;

    function paintKnob(control, part, travel) {
      const f = travel == null ? 0.5 : travel;
      const angle = (f * 2 - 1) * KNOB_SPAN;
      part.pointer.setAttribute(
        "transform",
        "rotate(" + angle.toFixed(1) + " " + control.cx + " " + control.cy + ")",
      );
    }

    function paintGimbals() {
      LAYOUT.gimbals.forEach((g) => {
        const part = gimbalParts[g.id];
        if (!part) return;
        const fx = travelOf(g.id + "_x");
        const fy = travelOf(g.id + "_y");
        const reach = g.r - STICK_R - 8;
        const dx = fx == null ? 0 : (fx * 2 - 1) * reach;
        const dy = fy == null ? 0 : -(fy * 2 - 1) * reach;
        part.cap.setAttribute("transform",
          "translate(" + dx.toFixed(1) + " " + dy.toFixed(1) + ")");
        part.shaft.setAttribute("x2", (g.cx + dx).toFixed(1));
        part.shaft.setAttribute("y2", (g.cy + dy).toFixed(1));
        part.group.dataset.signal = (fx == null && fy == null) ? "" : "1";
        part.group.dataset.prompted =
          (prompt.controls.indexOf(g.id + "_x") >= 0
            || prompt.controls.indexOf(g.id + "_y") >= 0) ? "1" : "";
        paintArrow(g, part);
      });
    }

    /**
     * The wizard's "move it THIS way" arrow.
     *
     * Drawn on the gimbal the prompt names, pointing the way the operator is
     * being asked to push. This is the part of the widget the calibration
     * actually needs: "push the pitch stick FORWARD (nose down)" is a sentence
     * that has to be parsed; an arrow on the right stick is not.
     */
    function paintArrow(g, part) {
      const dir = prompt.direction;
      if (!(prompt.gimbal === g.id && dir)) {
        part.arrow.setAttribute("opacity", "0");
        return;
      }
      const step = { up: [0, -1], down: [0, 1], left: [-1, 0], right: [1, 0] }[dir];
      if (!step) { part.arrow.setAttribute("opacity", "0"); return; }

      // Drawn INSIDE the well, from the centre out to the rim. Outside it there
      // is a screen, a trim row and six switches to collide with, and an arrow
      // that has to be hunted for is one the operator reads the sentence
      // instead of.
      const reach = g.r - 22;
      const tipX = g.cx + step[0] * reach;
      const tipY = g.cy + step[1] * reach;
      const barb = 15;
      const baseX = tipX - step[0] * barb;
      const baseY = tipY - step[1] * barb;
      const perpX = step[1] * barb;
      const perpY = step[0] * barb;
      part.arrow.setAttribute("d",
        "M " + g.cx + " " + g.cy + " L " + tipX + " " + tipY
        + " M " + (baseX - perpX) + " " + (baseY - perpY)
        + " L " + tipX + " " + tipY
        + " L " + (baseX + perpX) + " " + (baseY + perpY));
      part.arrow.setAttribute("opacity", "1");
    }

    /** One line of text for a control, used by the callouts, the fallback and
     *  the inspector. */
    function describe(controlId, travel) {
      const channel = channelOf(controlId);
      if (!channel) return "not bound";
      if (travel == null) return "CH" + channel + " · no signal";
      const control = CONTROL_BY_ID[controlId];
      if (control.kind === "switch") {
        const count = positionsOf(controlId);
        return "CH" + channel + " · "
          + positionLabel(positionIndex(travel, count), count);
      }
      return "CH" + channel + " · " + Math.round(travel * 100) + "%";
    }

    /* ================================================================ */
    /* Selection and the inspector                                      */
    /* ================================================================ */

    function select(controlId) {
      if (!interactive) return;
      selectedId = selectedId === controlId ? null : controlId;
      cancelLearn("cancelled");
      inspector.hidden = !selectedId;
      legend.hidden = !!selectedId;
      if (selectedId) renderInspector();
      Object.keys(parts).forEach((id) => {
        (parts[id].nodes || []).forEach((n) => {
          if (n.dataset) n.dataset.selected = id === selectedId ? "1" : "";
        });
      });
      if (typeof o.onSelect === "function") o.onSelect(selectedId);
    }

    /**
     * The inspector is rebuilt wholesale only when the selection, a binding or
     * the armed flag changes; the live parts of it are written in place from
     * every frame. A card rebuilt at 20 Hz would drop the dropdown the
     * operator has open.
     */
    function renderInspector() {
      const controlId = selectedId;
      if (!controlId) return;
      const control = CONTROL_BY_ID[controlId];
      const channel = channelOf(controlId);
      const key = [controlId, channel, positionsOf(controlId),
        armed ? "armed" : "", learn ? "learning" : ""].join(":");
      if (key === inspectorKey) {
        if (liveValue) liveValue.textContent = describe(controlId, travelOf(controlId));
        return;
      }
      inspectorKey = key;
      inspector.innerHTML = "";
      learnStatusEl = null;

      const head = el("div", "rc-tx-inspector-head");
      head.appendChild(el("span", "rc-tx-inspector-title", control.label));
      liveValue = el("span", "rc-tx-inspector-value",
        describe(controlId, travelOf(controlId)));
      head.appendChild(liveValue);
      inspector.appendChild(head);

      const body = el("div", "rc-tx-inspector-body");
      inspector.appendChild(body);

      if (control.kind === "axis") {
        // A stick axis is not learned: the vehicle already says which channel
        // carries throttle, and the drawn axis it lands on is the stick mode.
        // Offering Learn here would let the two disagree.
        const role = axisRoles(mode)[controlId];
        body.appendChild(el("div", "rc-tx-inspector-note",
          channel
            ? ROLE_LABEL[role] + " is on channel " + channel + " ("
              + ROLE_PARAM[role] + "). Change it in Stick channels below, or "
              + "re-run the calibration."
            : ROLE_LABEL[role] + " is unassigned — " + ROLE_PARAM[role]
              + " is 0. Run the radio calibration, or set it in Stick "
              + "channels below."));
        return;
      }

      const row = el("div", "rc-tx-inspector-row");
      const learnBtn = Corvus.ui.button({
        variant: learn ? "secondary" : "primary", size: "sm",
        icon: learn ? "x" : "crosshair",
        label: learn ? "Cancel" : (channel ? "Re-learn channel" : "Learn channel"),
        onClick: () => { if (learn) cancelLearn("cancelled"); else startLearn(controlId); },
      });
      row.appendChild(learnBtn);

      if (control.kind === "switch") {
        const posSelect = Corvus.ui.select({
          className: "rc-tx-pos-select",
          ariaLabel: control.label + " positions",
          options: [{ value: 2, label: "2 positions" },
            { value: 3, label: "3 positions" }, { value: 6, label: "6 positions" }],
          value: positionsOf(controlId),
          onChange: (value) => {
            const next = Number(value);
            if (!(next >= 2)) return;
            const bound = bindings[controlId] || {};
            bindings[controlId] = { channel: bound.channel || 0, positions: next };
            if (!bindings[controlId].channel) delete bindings[controlId].channel;
            saveBindings(bindings);
            inspectorKey = "";
            renderInspector();
            update(lastState);
          },
        });
        row.appendChild(posSelect);
      }

      if (channel) {
        const clear = Corvus.ui.button({
          variant: "ghost", size: "sm", icon: "trash-2", label: "Forget",
          onClick: () => {
            delete bindings[controlId];
            saveBindings(bindings);
            inspectorKey = "";
            renderInspector();
            update(lastState);
            if (typeof o.onAssign === "function") o.onAssign(controlId, null);
          },
        });
        row.appendChild(clear);
      }

      const learnStatus = el("span", "params-row-status", learn ? "move it now…" : "");
      if (learn) learnStatus.classList.add("pending");
      row.appendChild(learnStatus);
      body.appendChild(row);
      learnStatusEl = learnStatus;

      // What this control's channel does on the vehicle, and how to change it.
      if (channel && typeof o.renderFunctions === "function") {
        const fns = o.renderFunctions(channel, controlId, armed);
        if (fns) body.appendChild(fns);
      } else if (!channel) {
        body.appendChild(el("div", "rc-tx-inspector-note",
          "Press Learn, then move this control through its full travel. The "
          + "channel that moves is the one it sends on — once that is known, "
          + "you can give the channel a function."));
      }

      if (typeof Corvus.ui.refreshIcons === "function") Corvus.ui.refreshIcons();
    }

    /**
     * Learn: bind the drawn control to the channel that moves next.
     *
     * Same measurement as the page's Detect — baseline at the press, winner is
     * the largest travel past LEARN_TRAVEL_US — because they answer the same
     * question and two different thresholds would disagree on the same flip of
     * the same switch.
     */
    function startLearn(controlId) {
      if (learn) return;
      const snapshot = (Corvus.telemetry && Corvus.telemetry.getState()) || {};
      const baseline = ((snapshot.rc_channels) || []).slice();
      if (!snapshot.rc_live || !baseline.length) {
        inspectorKey = "";
        renderInspector();
        if (learnStatusEl) {
          learnStatusEl.className = "params-row-status err";
          learnStatusEl.textContent = "no RC signal";
        }
        return;
      }
      const deadline = Date.now() + LEARN_TIMEOUT_MS;
      learn = {
        controlId,
        sample(state) {
          const now = (state && state.rc_channels) || [];
          let best = 0;
          let bestTravel = 0;
          for (let i = 0; i < now.length && i < baseline.length; i += 1) {
            const moved = Math.abs((Number(now[i]) || 0) - (Number(baseline[i]) || 0));
            if (moved > bestTravel) { bestTravel = moved; best = i + 1; }
          }
          if (bestTravel >= LEARN_TRAVEL_US) { finishLearn(best); return; }
          if (Date.now() > deadline) cancelLearn("nothing moved");
        },
      };
      inspectorKey = "";
      renderInspector();
    }

    function finishLearn(channel) {
      const controlId = learn && learn.controlId;
      learn = null;
      if (!controlId) return;
      const existing = bindings[controlId] || {};
      const binding = { channel };
      if (existing.positions >= 2) binding.positions = existing.positions;
      bindings[controlId] = binding;
      saveBindings(bindings);
      inspectorKey = "";
      renderInspector();
      if (learnStatusEl) {
        learnStatusEl.className = "params-row-status ok";
        learnStatusEl.textContent = "channel " + channel;
      }
      if (typeof o.onAssign === "function") o.onAssign(controlId, binding);
    }

    function cancelLearn(reason) {
      if (!learn) return;
      learn = null;
      inspectorKey = "";
      if (selectedId) renderInspector();
      if (learnStatusEl && reason) {
        learnStatusEl.className = "params-row-status err";
        learnStatusEl.textContent = reason;
      }
    }

    /* ================================================================ */
    /* Public surface                                                   */
    /* ================================================================ */

    return {
      el: root,
      update,

      /** Channels of the four stick roles, from the vehicle's RC_MAP_*. */
      setStickChannels(map) {
        stickChannels = map || {};
        inspectorKey = "";
      },

      /** Per-channel {min, max, reversed} from the vehicle's calibration. */
      setChannelInfo(info) { channelInfo = info || {}; },

      setMode(next) {
        mode = MODES[Number(next)] ? Number(next) : DEFAULT_MODE;
        saveMode(mode);
        inspectorKey = "";
        update(lastState);
      },
      getMode() { return mode; },

      /**
       * What the page wants pointed at.
       * {controls: [ids], gimbal: id|null, direction: "up"|..., swept: [ids]}
       */
      setPrompt(next) {
        const p = next || {};
        prompt = {
          controls: p.controls || [],
          gimbal: p.gimbal || null,
          direction: p.direction || null,
          swept: p.swept || [],
        };
        // Repaint immediately rather than at the next frame. A wizard step
        // changes on a button press, and between presses the stream can be
        // anything from 20 Hz to nothing at all — a drawing still showing the
        // previous step's instruction is worse than one showing none.
        update(lastState);
      },

      /** Drop every learned binding (a new transmitter, not a new vehicle). */
      forgetAll() {
        bindings = {};
        saveBindings(bindings);
        inspectorKey = "";
        if (selectedId) renderInspector();
        update(lastState);
      },

      bindings() { return bindings; },

      /** {controlId: channel} for every control that has one — how the page
       *  turns a set of swept CHANNELS back into the controls to light up. */
      channelMap() {
        const map = {};
        LAYOUT.controls.forEach((control) => {
          const channel = channelOf(control.id);
          if (channel) map[control.id] = channel;
        });
        return map;
      },

      selected() { return selectedId; },
      select,
      /** Re-render the inspector after the page wrote a mapping. */
      refresh() { inspectorKey = ""; if (selectedId) renderInspector(); },

      destroy() {
        cancelLearn("");
        selectedId = null;
        learnStatusEl = null;
      },
    };
  }

  return {
    LAYOUT, MODES, DEFAULT_MODE, ROLE_PARAM, ROLE_LABEL,
    axisRoles, positionIndex, positionLabel, fractionOf, leverAngle,
    loadBindings, saveBindings, loadMode, saveMode,
    create,
  };
})();
