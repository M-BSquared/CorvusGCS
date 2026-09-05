"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.joystick — the on-screen manual-control pad over the map.

  Two input surfaces, each with its own switch in Settings > Appearance >
  Controls, both feeding ONE MANUAL_CONTROL stream:

    Virtual joystick   two spring-return sticks — full four-axis control
    Arrow keys         a four-key cluster, and the keyboard's own arrow keys —
                       forward / back / left / right only

  Both are off by default, and both are INPUT SOURCES and nothing else: they
  arm nothing, change no mode, and cannot override a failsafe. Whether the
  autopilot acts on them at all is the vehicle's decision (COM_RC_IN_MODE 1 or
  3, and a mode that flies from the sticks) — the GCS deliberately does not set
  that parameter on the operator's behalf.

  Axis mapping is the transmitter's, because that is what PX4 receives:

    left  stick   X -> r  yaw    (-1 CCW .. +1 CW)
                  Y -> z  thrust (0 down .. 1 up, 0.5 = the centre detent
                                  PX4 expects from a spring-return stick)
    right stick   X -> y  roll   (-1 left .. +1 right)
                  Y -> x  pitch  (-1 back .. +1 forward)
    arrow keys    up/down  -> x  pitch      left/right -> y  roll
                  and nothing else: a key is on or off, and a digital hold on
                  thrust or yaw is a worse idea than not offering it.

  The keys deflect to KEY_DEFLECTION rather than to the stop. A key has no
  travel to meter, so full authority on a keypress would make the mildest
  correction the largest one available; the sticks are there when full
  deflection is wanted. Keys and sticks SUM into the same virtual stick — they
  are two ways to move one control, not two controls — and the result is
  clamped to the circle, so pushing both at once cannot exceed full travel.

  Everything springs back to centre on release, which parks the aircraft at
  hover in the modes this is useful in (Position / Altitude) rather than at
  zero thrust.

  Streaming, not event-driven: PX4 treats a gap in MANUAL_CONTROL as RC loss,
  so frames go out on a timer for as long as a surface is on screen and the
  link is up — neutral frames included. The rate drops while every axis sits at
  centre because each frame is one HTTP round trip on a server that does not
  keep connections alive; the idle rate is still far inside PX4's RC-loss
  window (COM_RC_LOSS_T, 0.5 s by default). One in-flight frame at a time: a
  tick that finds the previous POST unfinished skips rather than queues, so a
  slow link degrades the stream's rate instead of its latency.

  Known limitation: the stream is a browser timer, and browsers throttle those
  in a window that is not visible. Backgrounding the GCS while flying from the
  pad therefore slows the stream, exactly as putting the transmitter down
  would. Nothing here can prevent that; the aircraft's own RC-loss failsafe is
  what covers it.

  The pad is a movable window over the map, like the flight HUD: drag it by its
  grip bar, double-click that bar to send it back to its corner, and the
  position is persisted. It deliberately does NOT reuse Corvus.hudPanel — that
  module owns one specific element and adds pin/compact/collapse, none of which
  belong on a control surface — so the drag maths is mirrored here instead,
  including the interface-scale correction (see pointerScale).
*/
Corvus.joystick = (function () {
  const ACTIVE_HZ = 20;      // while an axis is off centre
  const IDLE_HZ = 5;         // holding the link open at neutral
  const DEADZONE = 0.06;     // fraction of stick travel that still reads as centre
  const KEY_DEFLECTION = 0.5;

  const POS_KEY = "corvus.joystick";
  // Keep at least this much of the pad on screen when clamping, so it can
  // never be dragged entirely out of reach.
  const MIN_VISIBLE = 64;

  // Neutral frame: sticks centred, thrust at the hover detent.
  const NEUTRAL = { x: 0, y: 0, z: 0.5, r: 0 };

  // Arrow key -> the axis it moves and which way. Screen "up" is forward.
  const KEY_DIRS = {
    ArrowUp: { dir: "up", axis: "x", sign: 1 },
    ArrowDown: { dir: "down", axis: "x", sign: -1 },
    ArrowLeft: { dir: "left", axis: "y", sign: -1 },
    ArrowRight: { dir: "right", axis: "y", sign: 1 },
  };

  let pad = null;
  let sticksEl = null;
  let keysEl = null;
  let gripEl = null;
  let statusEl = null;
  let showSticks = false;
  let showKeys = false;
  let timer = null;
  let inFlight = false;
  let connected = false;
  let boundGlobals = false;
  const sticks = [];
  const keyButtons = {};        // dir -> button element
  const held = {};              // dir -> true while pressed

  let position = { x: null, y: null };
  let drag = null;

  /* ------------------------------------------------------------------ */
  /* sticks                                                              */
  /* ------------------------------------------------------------------ */

  /* One stick: a circular base, a knob that follows the pointer inside it,
     and a caption. Returns a handle whose .value is the live {x, y} in
     [-1, 1] with y positive UP (screen y is inverted here, once, so no caller
     has to remember to do it). */
  function buildStick(spec) {
    const wrap = document.createElement("div");
    wrap.className = "js-stick";

    const base = document.createElement("div");
    base.className = "js-base";
    base.setAttribute("role", "application");
    base.setAttribute("aria-label", spec.ariaLabel);
    base.tabIndex = -1;

    const cross = document.createElement("span");
    cross.className = "js-cross";
    const knob = document.createElement("span");
    knob.className = "js-knob";
    base.append(cross, knob);

    const caption = document.createElement("span");
    caption.className = "js-caption";
    caption.textContent = spec.caption;

    wrap.append(base, caption);

    const handle = { el: wrap, base, value: { x: 0, y: 0 }, spec };
    let pointerId = null;

    function paint() {
      // The knob travels to the edge of the base minus its own radius; that
      // ratio lives in CSS as --js-travel so the two cannot disagree.
      knob.style.transform =
        `translate(calc(${handle.value.x} * var(--js-travel)), ` +
        `calc(${-handle.value.y} * var(--js-travel)))`;
      wrap.classList.toggle("active", pointerId !== null);
    }

    function track(event) {
      const rect = base.getBoundingClientRect();
      const radius = rect.width / 2;
      if (!radius) return;
      let nx = (event.clientX - (rect.left + radius)) / radius;
      let ny = ((rect.top + radius) - event.clientY) / radius;
      const clamped = clampToCircle(nx, ny);
      handle.value = {
        x: applyDeadzone(clamped.x),
        y: applyDeadzone(clamped.y),
      };
      paint();
    }

    function release() {
      if (pointerId !== null) {
        try { base.releasePointerCapture(pointerId); } catch (_e) {}
        pointerId = null;
      }
      handle.value = { x: 0, y: 0 };   // spring return
      paint();
    }

    base.addEventListener("pointerdown", (event) => {
      if (pointerId !== null) return;
      pointerId = event.pointerId;
      try { base.setPointerCapture(pointerId); } catch (_e) {}
      track(event);
      event.preventDefault();
    });
    base.addEventListener("pointermove", (event) => {
      if (event.pointerId !== pointerId) return;
      track(event);
    });
    ["pointerup", "pointercancel"].forEach((name) => {
      base.addEventListener(name, (event) => {
        if (event.pointerId !== pointerId) return;
        release();
      });
    });

    handle.release = release;
    paint();
    return handle;
  }

  function applyDeadzone(v) {
    if (Math.abs(v) <= DEADZONE) return 0;
    // Rescale past the deadzone so the stick still reaches full travel.
    const scaled = (Math.abs(v) - DEADZONE) / (1 - DEADZONE);
    return (v < 0 ? -scaled : scaled);
  }

  /* Clamp to the circle, not the square: a stick has no corners, and without
     this a diagonal reads as 1.41 of travel. */
  function clampToCircle(x, y) {
    const mag = Math.hypot(x, y);
    return mag > 1 ? { x: x / mag, y: y / mag } : { x, y };
  }

  /* ------------------------------------------------------------------ */
  /* arrow keys                                                          */
  /* ------------------------------------------------------------------ */

  /* The four-key cluster, laid out as a keyboard's own inverted T so it reads
     as "these keys" rather than as four unrelated buttons. Each is a real
     <button>, so it is reachable by tab and fires on Space/Enter as well as
     under a finger. */
  function buildKeys() {
    const wrap = document.createElement("div");
    wrap.className = "js-keys";

    const grid = document.createElement("div");
    grid.className = "js-keys-grid";
    grid.setAttribute("role", "group");
    grid.setAttribute("aria-label", "Arrow key control: forward, back, left and right");

    [
      { dir: "up", icon: "arrow-up", label: "Forward" },
      { dir: "left", icon: "arrow-left", label: "Left" },
      { dir: "down", icon: "arrow-down", label: "Back" },
      { dir: "right", icon: "arrow-right", label: "Right" },
    ].forEach((spec) => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "js-key js-key-" + spec.dir;
      btn.title = spec.label;
      btn.setAttribute("aria-label", spec.label);
      btn.appendChild(Corvus.ui.icon(spec.icon, 15));

      btn.addEventListener("pointerdown", (event) => {
        try { btn.setPointerCapture(event.pointerId); } catch (_e) {}
        setHeld(spec.dir, true);
        event.preventDefault();
      });
      ["pointerup", "pointercancel"].forEach((name) => {
        btn.addEventListener(name, () => setHeld(spec.dir, false));
      });
      // Space/Enter on a focused key: the browser fires click, not a
      // pointer pair, so pulse the axis for one frame rather than leaving
      // the key stuck down with no matching release.
      btn.addEventListener("click", () => {
        if (held[spec.dir]) return;
        setHeld(spec.dir, true);
        window.setTimeout(() => setHeld(spec.dir, false), Math.round(1000 / ACTIVE_HZ) * 2);
      });

      keyButtons[spec.dir] = btn;
      grid.appendChild(btn);
    });

    const caption = document.createElement("span");
    caption.className = "js-caption";
    caption.textContent = "ARROW KEYS";

    wrap.append(grid, caption);
    return wrap;
  }

  function setHeld(dir, down) {
    if (!!held[dir] === !!down) return;
    held[dir] = !!down;
    const btn = keyButtons[dir];
    if (btn) btn.classList.toggle("active", !!down);
  }

  function releaseKeys() {
    Object.keys(keyButtons).forEach((dir) => setHeld(dir, false));
  }

  /* The keyboard drives the same four keys the cluster does, so the on-screen
     keys light up under a real keypress. Ignored while the arrow-key surface
     is off, while the map view is not the visible page, and while the caret is
     in a field — an operator typing a connection string must not be flying. */
  function keyboardTarget(event) {
    if (!showKeys) return null;
    const entry = KEY_DIRS[event.key];
    if (!entry) return null;
    if (event.altKey || event.ctrlKey || event.metaKey) return null;
    if (pad && pad.hidden) return null;
    const el = event.target;
    if (el && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName || ""))) {
      return null;
    }
    return entry;
  }

  function onKeyDown(event) {
    const entry = keyboardTarget(event);
    if (!entry) return;
    event.preventDefault();          // no page scroll while flying
    setHeld(entry.dir, true);
  }

  function onKeyUp(event) {
    const entry = KEY_DIRS[event.key];
    // Released unconditionally: a key that went down while the surface was on
    // must still come up if the switch flipped mid-press.
    if (entry) setHeld(entry.dir, false);
  }

  /* ------------------------------------------------------------------ */
  /* the frame                                                           */
  /* ------------------------------------------------------------------ */

  /** The four MAVLink axes for the current state of both surfaces. */
  function frame() {
    const left = (showSticks && sticks[0]) ? sticks[0].value : { x: 0, y: 0 };
    const right = (showSticks && sticks[1]) ? sticks[1].value : { x: 0, y: 0 };

    let pitch = right.y;
    let roll = right.x;
    if (showKeys) {
      if (held.up) pitch += KEY_DEFLECTION;
      if (held.down) pitch -= KEY_DEFLECTION;
      if (held.right) roll += KEY_DEFLECTION;
      if (held.left) roll -= KEY_DEFLECTION;
      // Sticks and keys move ONE virtual stick, so the sum is clamped to the
      // circle exactly as the stick itself is.
      const c = clampToCircle(roll, pitch);
      roll = c.x; pitch = c.y;
    }

    return {
      x: round(pitch),                       // pitch, forward positive
      y: round(roll),                        // roll, right positive
      z: round(0.5 + left.y * 0.5),          // thrust, 0.5 = hover detent
      r: round(left.x),                      // yaw, clockwise positive
    };
  }

  /* Axes go on the wire as int16 ticks at 1000 per unit, so anything past the
     third decimal is noise — and rounding here is what lets isNeutral() be an
     equality test instead of an epsilon comparison. */
  function round(v) { return Math.round(v * 1000) / 1000; }

  function isNeutral(f) {
    return f.x === 0 && f.y === 0 && f.r === 0 && f.z === NEUTRAL.z;
  }

  function fmt(v) {
    return (v >= 0 ? "+" : "−") + Math.abs(v).toFixed(2);
  }

  function paintStatus(f) {
    if (!statusEl) return;
    statusEl.textContent = connected
      ? `P ${fmt(f.x)}   R ${fmt(f.y)}   T ${f.z.toFixed(2)}   Y ${fmt(f.r)}`
      : "NO LINK";
    statusEl.classList.toggle("offline", !connected);
  }

  /* ------------------------------------------------------------------ */
  /* the stream                                                          */
  /* ------------------------------------------------------------------ */

  /* One tick of the stream. Sends even at neutral (that is what keeps PX4
     from calling RC loss) but never while a previous frame is outstanding,
     and never while the link is down — a disconnected vehicle would only
     produce a stream of 503s the operator cannot act on. */
  function tick() {
    const f = frame();
    paintStatus(f);
    if (!connected || inFlight) { schedule(f); return; }
    inFlight = true;
    Corvus.telemetry.requestJson("/api/mavlink/manual", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(f),
    }).catch(() => {
      // Deliberately silent: a stick stream that raised a toast per dropped
      // frame would bury every other notification. The status readout going
      // to NO LINK is the operator-visible signal.
    }).then(() => { inFlight = false; });
    schedule(f);
  }

  function schedule(f) {
    if (!streaming()) return;
    const hz = isNeutral(f) ? IDLE_HZ : ACTIVE_HZ;
    timer = window.setTimeout(tick, Math.round(1000 / hz));
  }

  function stopStream() {
    if (timer !== null) { window.clearTimeout(timer); timer = null; }
  }

  function streaming() { return showSticks || showKeys; }

  /* ------------------------------------------------------------------ */
  /* dragging                                                            */
  /* ------------------------------------------------------------------ */

  /** The area the pad may occupy: the map view, not the whole window. */
  function boundsEl() {
    return pad && pad.parentElement ? pad.parentElement : null;
  }

  /**
   * The factor between the pixels the pointer reports and the pixels this pad
   * is positioned in. The interface-scale control puts a CSS `zoom` on
   * <body>, so getBoundingClientRect and pointer clientX/Y come back in SCALED
   * pixels while style.left/top are written in UNSCALED ones. offsetWidth /
   * clientWidth are already unscaled, which is why clamp() uses them.
   */
  function pointerScale(el) {
    if (!el) return 1;
    const rect = el.getBoundingClientRect();
    return (rect.width && el.offsetWidth) ? (rect.width / el.offsetWidth) : 1;
  }

  function clampPosition(x, y) {
    const host = boundsEl();
    if (!host) return { x, y };
    const maxX = Math.max(0, host.clientWidth - MIN_VISIBLE);
    const maxY = Math.max(0, host.clientHeight - MIN_VISIBLE);
    return {
      x: Math.min(Math.max(x, MIN_VISIBLE - pad.offsetWidth), maxX),
      y: Math.min(Math.max(y, 0), maxY),   // never above the top edge
    };
  }

  /** Write the current position. A null position means "leave it at the CSS
   *  default corner" — the pad has never been moved. */
  function applyPosition() {
    if (!pad) return;
    if (position.x === null || position.y === null) {
      pad.style.left = "";
      pad.style.top = "";
      pad.classList.remove("is-placed");
      return;
    }
    const c = clampPosition(position.x, position.y);
    position.x = c.x; position.y = c.y;
    // .is-placed drops the CSS bottom anchoring so left/top can win.
    pad.classList.add("is-placed");
    pad.style.left = c.x + "px";
    pad.style.top = c.y + "px";
  }

  function loadPosition() {
    position = { x: null, y: null };
    try {
      const saved = JSON.parse(localStorage.getItem(POS_KEY) || "null");
      if (!saved || typeof saved !== "object") return;
      if (typeof saved.x === "number") position.x = saved.x;
      if (typeof saved.y === "number") position.y = saved.y;
    } catch (_e) { /* unreadable storage -> the default corner */ }
  }

  function savePosition() {
    try { localStorage.setItem(POS_KEY, JSON.stringify(position)); } catch (_e) {}
  }

  /** Send the pad back to its default corner. */
  function resetPosition() {
    position = { x: null, y: null };
    applyPosition();
    savePosition();
  }

  /* The grip bar is the only drag handle. The sticks and the keys are the
     whole point of the pad, so a drag that started on them would fly the
     aircraft while moving the window. */
  function wireDrag() {
    gripEl.addEventListener("pointerdown", (event) => {
      if (event.button !== 0) return;
      const host = boundsEl();
      if (!host) return;
      const hb = host.getBoundingClientRect();
      const pb = pad.getBoundingClientRect();
      const k = pointerScale(host);
      drag = {
        pointerId: event.pointerId,
        // Grab offset in the pad's own (unscaled) pixels, so the point under
        // the cursor stays under the cursor at any interface size.
        dx: (event.clientX - pb.left) / k,
        dy: (event.clientY - pb.top) / k,
        hostLeft: hb.left,
        hostTop: hb.top,
        scale: k,
      };
      // Capture on the pad so the drag survives the pointer leaving the grip —
      // including over the map canvas, which would otherwise swallow the moves.
      try { pad.setPointerCapture(event.pointerId); } catch (_e) {}
      pad.classList.add("is-dragging");
      event.preventDefault();
    });

    pad.addEventListener("pointermove", (event) => {
      if (!drag || event.pointerId !== drag.pointerId) return;
      position.x = (event.clientX - drag.hostLeft) / drag.scale - drag.dx;
      position.y = (event.clientY - drag.hostTop) / drag.scale - drag.dy;
      applyPosition();
    });
    ["pointerup", "pointercancel"].forEach((name) => {
      pad.addEventListener(name, (event) => {
        if (!drag || event.pointerId !== drag.pointerId) return;
        try { pad.releasePointerCapture(drag.pointerId); } catch (_e) {}
        drag = null;
        pad.classList.remove("is-dragging");
        savePosition();
      });
    });

    gripEl.addEventListener("dblclick", resetPosition);
  }

  /* ------------------------------------------------------------------ */
  /* lifecycle                                                           */
  /* ------------------------------------------------------------------ */

  function releaseAll() {
    sticks.forEach((s) => s.release());
    releaseKeys();
  }

  /* Show or hide each surface and start or stop the stream to match.

     Safe to call before init(): the config fetch that decides the initial
     state races the DOM wiring, so a call that arrives first is remembered and
     applied once the pad exists. Nothing streams without a pad — an invisible
     control the operator cannot centre must never be an input source.

     Leaving the Home tab is NOT a reason to stop: the pad rides inside the map
     view and goes out of sight with it, but cutting the stream mid-flight is
     exactly what PX4 reads as RC loss. It keeps sending centred sticks, which
     is what a released transmitter sends too. */
  function setEnabled(on) {
    if (!!on === showSticks) return;
    showSticks = !!on;
    if (pad) apply();
  }

  function setKeysEnabled(on) {
    if (!!on === showKeys) return;
    showKeys = !!on;
    if (pad) apply();
  }

  function apply() {
    if (sticksEl) sticksEl.hidden = !showSticks;
    if (keysEl) keysEl.hidden = !showKeys;
    pad.hidden = !streaming();
    stopStream();
    releaseAll();
    if (streaming()) {
      // A pad that just changed width must not sit half off the map.
      applyPosition();
      tick();
    } else {
      paintStatus(frame());
    }
  }

  function init(root) {
    pad = root;
    if (!pad) return;
    pad.hidden = true;
    // Idempotent: a second init() rebuilds the pad rather than stacking a
    // second set of surfaces behind the first, which would leave frame()
    // reading controls nobody can see.
    sticks.length = 0;
    Object.keys(keyButtons).forEach((k) => delete keyButtons[k]);
    Object.keys(held).forEach((k) => delete held[k]);
    while (pad.firstChild) pad.removeChild(pad.firstChild);

    // Grip bar: the drag handle, and the only place the live axis values are
    // shown. One row, so the pad reads as a window with a title bar rather
    // than as loose controls scattered over the map.
    gripEl = document.createElement("div");
    gripEl.className = "js-grip";
    gripEl.title = "Drag to move · double-click to reset";
    gripEl.appendChild(Corvus.ui.icon("grip-horizontal", 13));
    statusEl = document.createElement("span");
    statusEl.className = "js-axes";
    gripEl.appendChild(statusEl);

    const surfaces = document.createElement("div");
    surfaces.className = "js-surfaces";

    sticksEl = document.createElement("div");
    sticksEl.className = "js-sticks";
    sticks.push(buildStick({
      caption: "THR · YAW",
      ariaLabel: "Left stick: throttle and yaw",
    }));
    sticks.push(buildStick({
      caption: "PITCH · ROLL",
      ariaLabel: "Right stick: pitch and roll",
    }));
    sticksEl.append(sticks[0].el, sticks[1].el);

    keysEl = buildKeys();
    surfaces.append(sticksEl, keysEl);
    pad.append(gripEl, surfaces);

    Corvus.telemetry.subscribe((s) => {
      connected = !!(s && s.connected);
      if (streaming()) paintStatus(frame());
    });

    loadPosition();
    wireDrag();

    // One set of global listeners for the pad, not one per surface, so a
    // rebuild cannot stack them. Blur matters most: a window that loses focus
    // must not leave a stick or a key pinned with the stream still sending it.
    if (!boundGlobals) {
      window.addEventListener("blur", releaseAll);
      window.addEventListener("resize", applyPosition);
      window.addEventListener("keydown", onKeyDown);
      window.addEventListener("keyup", onKeyUp);
      boundGlobals = true;
    }
    Corvus.ui.refreshIcons();
    apply();
  }

  return {
    init,
    setEnabled,
    setKeysEnabled,
    isEnabled: () => showSticks,
    isKeysEnabled: () => showKeys,
    resetPosition,
  };
})();
