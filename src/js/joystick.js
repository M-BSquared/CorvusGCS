"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.joystick — the on-screen manual-control pad over the map.

  Three input surfaces, each with its own switch in Settings > Appearance >
  Controls, all feeding ONE MANUAL_CONTROL stream:

    Virtual joystick   two spring-return sticks — full four-axis control
    Arrow keys         a four-key cluster, and the keyboard's own arrow keys —
                       forward / back / left / right only
    WASD keys          a second four-key cluster in the SAME panel, and the
                       keyboard's own W/A/S/D — thrust and yaw only

  The two key clusters are the two sticks split in half, and they share one
  panel for that reason: WASD is the left stick (thrust and yaw), the arrows
  are the right one (pitch and roll). Either alone is half a transmitter;
  together they are the whole one, without a pointer.

  All three are off by default, and all three are INPUT SOURCES and nothing
  else: they arm nothing, change no mode, and cannot override a failsafe.
  Whether the autopilot acts on them at all is the vehicle's decision
  (COM_RC_IN_MODE 1 or 3, and a mode that flies from the sticks) — the GCS
  deliberately does not set that parameter on the operator's behalf.

  Axis mapping is the transmitter's, because that is what PX4 receives:

    left  stick   X -> r  yaw    (-1 CCW .. +1 CW)
                  Y -> z  thrust (0 down .. 1 up, 0.5 = the centre detent
                                  PX4 expects from a spring-return stick)
    right stick   X -> y  roll   (-1 left .. +1 right)
                  Y -> x  pitch  (-1 back .. +1 forward)
    arrow keys    up/down  -> x  pitch      left/right -> y  roll
    WASD keys     W/S      -> z  thrust    A/D        -> r  yaw

  Thrust is the one axis a key cannot simply spring back on: releasing W parks
  it at the hover detent, not at zero, exactly as letting go of the left stick
  does. That is what makes a digital thrust key safe enough to offer at all —
  it climbs while held and hovers when let go, and it is why WASD is its own
  switch rather than something the arrow keys imply.

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
  module owns one specific element and adds pin/compact, neither of which
  belongs on a control surface — so the drag maths is mirrored here instead,
  including the interface-scale correction (see pointerScale).

  The grip bar also collapses the pad to the bar alone, which is persisted with
  the position. Collapsed, the pad STOPS BEING AN INPUT SOURCE: the surfaces
  are released and both the on-screen keys and the keyboard's own arrow and
  WASD keys stand down, for the same reason nothing streams without a pad at
  all — a control the operator cannot see is a control they cannot centre. The
  stream itself keeps running at neutral, because a gap in it is what PX4 reads
  as RC loss; a collapsed pad is a transmitter set down, not one switched off.
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
  // Margin kept between the pad and the map edge when the MAP moves under it,
  // rather than the pad over the map. See contain().
  const EDGE = 8;

  // Neutral frame: sticks centred, thrust at the hover detent.
  const NEUTRAL = { x: 0, y: 0, z: 0.5, r: 0 };

  /* Keyboard key -> the on-screen key it holds down, and which surface owns
     it. Screen "up" is forward. WASD is stored lower-case and looked up that
     way, so Shift and Caps Lock fly the aircraft exactly as the bare key
     does. */
  const KEY_DIRS = {
    ArrowUp: { dir: "up", surface: "keys" },
    ArrowDown: { dir: "down", surface: "keys" },
    ArrowLeft: { dir: "left", surface: "keys" },
    ArrowRight: { dir: "right", surface: "keys" },
    w: { dir: "thrustUp", surface: "wasd" },
    s: { dir: "thrustDown", surface: "wasd" },
    a: { dir: "yawLeft", surface: "wasd" },
    d: { dir: "yawRight", surface: "wasd" },
  };

  /** The entry for a keyboard event's key, or undefined. */
  function keyEntry(key) {
    if (typeof key !== "string") return undefined;
    return KEY_DIRS[key] || KEY_DIRS[key.toLowerCase()];
  }

  let pad = null;
  let sticksEl = null;
  let keysEl = null;
  let arrowClusterEl = null;
  let wasdClusterEl = null;
  let surfacesEl = null;
  let gripEl = null;
  let statusEl = null;
  let collapseBtn = null;
  let showSticks = false;
  let showKeys = false;
  let showWasd = false;
  let collapsed = false;
  let timer = null;
  let inFlight = false;
  let connected = false;
  let boundGlobals = false;
  const sticks = [];
  const keyButtons = {};        // dir -> button element
  const held = {};              // dir -> true while pressed

  let position = { x: null, y: null };
  let drag = null;
  // The map's size at the last reflow, so a change in it can be told from a
  // re-render. See onHostResize.
  let lastHost = { w: 0, h: 0 };
  let hostObserver = null;

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
  /* key clusters                                                        */
  /* ------------------------------------------------------------------ */

  /* The arrow cluster and the WASD cluster, each laid out as its own keys sit
     on a keyboard — an inverted T — so a cluster reads as "these keys" rather
     than as four unrelated buttons. The arrows carry direction icons and the
     WASD keys carry their letters, which is the shortest way to say which
     physical key lights up which button. Each is a real <button>, so it is
     reachable by tab and fires on Space/Enter as well as under a finger. */
  const CLUSTERS = {
    wasd: {
      className: "js-keys-cluster js-cluster-wasd",
      ariaLabel: "WASD control: thrust up, thrust down, yaw left and yaw right",
      caption: "THR · YAW",
      keys: [
        { dir: "thrustUp", cls: "w", letter: "W", label: "Thrust up" },
        { dir: "yawLeft", cls: "a", letter: "A", label: "Yaw left" },
        { dir: "thrustDown", cls: "s", letter: "S", label: "Thrust down" },
        { dir: "yawRight", cls: "d", letter: "D", label: "Yaw right" },
      ],
    },
    keys: {
      className: "js-keys-cluster js-cluster-arrows",
      ariaLabel: "Arrow key control: forward, back, left and right",
      caption: "ARROW KEYS",
      keys: [
        { dir: "up", cls: "up", icon: "arrow-up", label: "Forward" },
        { dir: "left", cls: "left", icon: "arrow-left", label: "Left" },
        { dir: "down", cls: "down", icon: "arrow-down", label: "Back" },
        { dir: "right", cls: "right", icon: "arrow-right", label: "Right" },
      ],
    },
  };

  function buildCluster(spec) {
    const wrap = document.createElement("div");
    wrap.className = spec.className;

    const grid = document.createElement("div");
    grid.className = "js-keys-grid";
    grid.setAttribute("role", "group");
    grid.setAttribute("aria-label", spec.ariaLabel);

    spec.keys.forEach((key) => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "js-key js-key-" + key.cls;
      btn.title = key.label;
      btn.setAttribute("aria-label", key.label);
      if (key.icon) {
        btn.appendChild(Corvus.ui.icon(key.icon, 15));
      } else {
        btn.classList.add("js-key-letter");
        btn.textContent = key.letter;
      }

      btn.addEventListener("pointerdown", (event) => {
        try { btn.setPointerCapture(event.pointerId); } catch (_e) {}
        setHeld(key.dir, true);
        event.preventDefault();
      });
      ["pointerup", "pointercancel"].forEach((name) => {
        btn.addEventListener(name, () => setHeld(key.dir, false));
      });
      // Space/Enter on a focused key: the browser fires click, not a
      // pointer pair, so pulse the axis for one frame rather than leaving
      // the key stuck down with no matching release.
      btn.addEventListener("click", () => {
        if (held[key.dir]) return;
        setHeld(key.dir, true);
        window.setTimeout(() => setHeld(key.dir, false), Math.round(1000 / ACTIVE_HZ) * 2);
      });

      keyButtons[key.dir] = btn;
      grid.appendChild(btn);
    });

    const caption = document.createElement("span");
    caption.className = "js-caption";
    caption.textContent = spec.caption;

    wrap.append(grid, caption);
    return wrap;
  }

  /* Both clusters live in ONE panel, switched on independently. They are the
     two halves of the stick pair, so a pad showing both reads as a single
     keyboard-sized transmitter rather than as two unrelated key pads; WASD
     comes first for the same reason the throttle stick is the left one. */
  function buildKeys() {
    const wrap = document.createElement("div");
    wrap.className = "js-keys";
    wasdClusterEl = buildCluster(CLUSTERS.wasd);
    arrowClusterEl = buildCluster(CLUSTERS.keys);
    wrap.append(wasdClusterEl, arrowClusterEl);
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

  function surfaceOn(name) { return name === "wasd" ? showWasd : showKeys; }

  /* The keyboard drives the same buttons its cluster does, so the on-screen
     keys light up under a real keypress. Ignored while that key's own surface
     is off — W is an ordinary letter until the WASD switch is on — while the
     map view is not the visible page, and while the caret is in a field: an
     operator typing a connection string must not be flying. */
  function keyboardTarget(event) {
    if (collapsed) return null;
    const entry = keyEntry(event.key);
    if (!entry || !surfaceOn(entry.surface)) return null;
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
    const entry = keyEntry(event.key);
    // Released unconditionally: a key that went down while the surface was on
    // must still come up if the switch flipped mid-press.
    if (entry) setHeld(entry.dir, false);
  }

  /* ------------------------------------------------------------------ */
  /* the frame                                                           */
  /* ------------------------------------------------------------------ */

  /** The four MAVLink axes for the current state of every surface. */
  function frame() {
    const left = (showSticks && sticks[0]) ? sticks[0].value : { x: 0, y: 0 };
    const right = (showSticks && sticks[1]) ? sticks[1].value : { x: 0, y: 0 };

    // Both halves are worked in STICK units — thrust included, where 0 is the
    // hover detent and ±1 the stops — so the keys sum with their stick and
    // clamp to the circle the same way on either side. Thrust is converted to
    // MAVLink's 0..1 once, at the bottom.
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

    let thrust = left.y;
    let yaw = left.x;
    if (showWasd) {
      if (held.thrustUp) thrust += KEY_DEFLECTION;
      if (held.thrustDown) thrust -= KEY_DEFLECTION;
      if (held.yawRight) yaw += KEY_DEFLECTION;
      if (held.yawLeft) yaw -= KEY_DEFLECTION;
      const c = clampToCircle(yaw, thrust);
      yaw = c.x; thrust = c.y;
    }

    return {
      x: round(pitch),                       // pitch, forward positive
      y: round(roll),                        // roll, right positive
      z: round(0.5 + thrust * 0.5),          // thrust, 0.5 = hover detent
      r: round(yaw),                         // yaw, clockwise positive
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

  /* Only the axes the visible surfaces can actually move. Four of them is a
     wider strip than one key cluster underneath it, so a keys-only pad used to
     be a narrow control under a bar twice its width. The arrows drive pitch
     and roll, WASD drives thrust and yaw, and the sticks drive all four — so
     the readout names exactly the half, or the whole, that is switched on. */
  function paintStatus(f) {
    if (!statusEl) return;
    let text;
    if (!connected) {
      text = "NO LINK";
    } else {
      const parts = [];
      if (showSticks || showKeys) parts.push(`P ${fmt(f.x)}`, `R ${fmt(f.y)}`);
      if (showSticks || showWasd) parts.push(`T ${f.z.toFixed(2)}`, `Y ${fmt(f.r)}`);
      text = parts.join("  ");
    }
    statusEl.textContent = text;
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

  function streaming() { return showSticks || showKeys || showWasd; }

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

  /** The map's size in the pad's own (unscaled) pixels. */
  function hostSize() {
    const host = boundsEl();
    return host ? { w: host.clientWidth, h: host.clientHeight } : { w: 0, h: 0 };
  }

  /**
   * Keep the WHOLE pad on the map, with a small margin — stricter than
   * clampPosition(), which deliberately lets a deliberate drag leave only a
   * strip showing. Used when the map resizes under a pad the operator is not
   * touching. An axis the pad is simply too large for falls back to the loose
   * clamp.
   */
  function contain(x, y) {
    const host = boundsEl();
    if (!host) return { x, y };
    const loose = clampPosition(x, y);
    const maxX = host.clientWidth - pad.offsetWidth - EDGE;
    const maxY = host.clientHeight - pad.offsetHeight - EDGE;
    return {
      x: maxX >= EDGE ? Math.min(Math.max(x, EDGE), maxX) : loose.x,
      y: maxY >= EDGE ? Math.min(Math.max(y, EDGE), maxY) : loose.y,
    };
  }

  /**
   * The map changed size — nearly always because the right utility panel was
   * slid open or shut, and otherwise because the window was resized.
   *
   * A pad that has been dragged is positioned from the map's TOP-LEFT, so a
   * map that narrows from the right leaves it exactly where it was: behind the
   * utility panel, which outranks it in the stacking order and simply covers
   * it. A control surface that disappears under a sidebar is worse than a
   * readout doing the same, so a pad sitting nearer the right edge travels
   * with that edge (and one nearer the left stays put), and the whole pad is
   * then contained rather than merely clamped.
   */
  function onHostResize() {
    if (!pad) return;
    const size = hostSize();
    // A hidden map — the operator is on Setup, Options or any other page —
    // reports 0x0, and a box with no size says nothing about where a panel
    // belongs. Containing against it collapses every coordinate to the origin,
    // which is how a carefully arranged pad used to come back to the
    // top-left corner after a round trip through another page. So there is
    // nothing to reflow against and nothing is touched — `lastHost` least of
    // all, because the next real resize still needs the last real size to tell
    // which edge the pad was travelling with.
    if (!size.w || !size.h) return;
    // Showing the map again after a page visit is not a resize: the box came
    // back the size it left. Reflowing anyway would re-`contain()` a pad the
    // operator had deliberately parked overhanging the edge, nudging it a few
    // pixels on every single visit to Setup and back.
    if (size.w === lastHost.w && size.h === lastHost.h) {
      applyPosition();
      return;
    }
    if (position.x !== null && lastHost.w && size.w && size.w !== lastHost.w) {
      const nearRight = (lastHost.w - (position.x + pad.offsetWidth)) < position.x;
      if (nearRight) position.x += size.w - lastHost.w;
    }
    if (position.y !== null && lastHost.h && size.h && size.h !== lastHost.h) {
      const nearBottom = (lastHost.h - (position.y + pad.offsetHeight)) < position.y;
      if (nearBottom) position.y += size.h - lastHost.h;
    }
    lastHost = size;
    if (position.x !== null && position.y !== null) {
      const c = contain(position.x, position.y);
      position.x = c.x; position.y = c.y;
    }
    applyPosition();
  }

  function loadPosition() {
    position = { x: null, y: null };
    collapsed = false;
    try {
      const saved = JSON.parse(localStorage.getItem(POS_KEY) || "null");
      if (!saved || typeof saved !== "object") return;
      if (typeof saved.x === "number") position.x = saved.x;
      if (typeof saved.y === "number") position.y = saved.y;
      collapsed = !!saved.collapsed;
    } catch (_e) { /* unreadable storage -> the default corner, expanded */ }
  }

  function savePosition() {
    try {
      localStorage.setItem(POS_KEY, JSON.stringify(
        { x: position.x, y: position.y, collapsed }));
    } catch (_e) {}
  }

  /** Send the pad back to its default corner. */
  function resetPosition() {
    position = { x: null, y: null };
    applyPosition();
    savePosition();
  }

  /** True when the event started on one of the grip's own controls. Written
   *  defensively because `closest` is the one DOM method the test stub omits. */
  function onGripButton(event) {
    const el = event && event.target;
    return !!(el && typeof el.closest === "function" && el.closest(".icon-btn"));
  }

  /* The grip bar is the only drag handle. The sticks and the keys are the
     whole point of the pad, so a drag that started on them would fly the
     aircraft while moving the window. */
  function wireDrag() {
    gripEl.addEventListener("pointerdown", (event) => {
      if (event.button !== 0) return;
      // The collapse control lives on the grip; its click must not also drag.
      if (onGripButton(event)) return;
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

    gripEl.addEventListener("dblclick", (event) => {
      if (onGripButton(event)) return;
      resetPosition();
    });
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

  function setWasdEnabled(on) {
    if (!!on === showWasd) return;
    showWasd = !!on;
    if (pad) apply();
  }

  function apply() {
    if (sticksEl) sticksEl.hidden = !showSticks || collapsed;
    // The panel is up while EITHER cluster is on; each cluster answers only to
    // its own switch, which is what puts WASD and the arrows side by side in
    // one window instead of giving each a pad of its own.
    if (keysEl) keysEl.hidden = (!showKeys && !showWasd) || collapsed;
    if (arrowClusterEl) arrowClusterEl.hidden = !showKeys;
    if (wasdClusterEl) wasdClusterEl.hidden = !showWasd;
    if (surfacesEl) surfacesEl.hidden = collapsed;
    pad.classList.toggle("is-collapsed", collapsed);
    pad.hidden = !streaming();
    if (collapseBtn) {
      const label = collapsed ? "Expand the control pad" : "Collapse the control pad";
      collapseBtn.title = label;
      collapseBtn.setAttribute("aria-label", label);
      collapseBtn.setAttribute("aria-expanded", collapsed ? "false" : "true");
      Corvus.ui.clear(collapseBtn).appendChild(
        Corvus.ui.icon(collapsed ? "chevron-up" : "chevron-down", 13));
      Corvus.ui.refreshIcons();
    }
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

  function toggleCollapsed() {
    collapsed = !collapsed;
    savePosition();
    apply();
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
    collapseBtn = Corvus.ui.iconButton("chevron-down", {
      size: 13,
      className: "icon-btn js-collapse",
      title: "Collapse the control pad",
      onClick: toggleCollapsed,
    });
    gripEl.appendChild(collapseBtn);

    const surfaces = document.createElement("div");
    surfaces.className = "js-surfaces";
    surfacesEl = surfaces;

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
    lastHost = hostSize();

    // One set of global listeners for the pad, not one per surface, so a
    // rebuild cannot stack them. Blur matters most: a window that loses focus
    // must not leave a stick or a key pinned with the stream still sending it.
    if (!boundGlobals) {
      window.addEventListener("blur", releaseAll);
      window.addEventListener("resize", onHostResize);
      window.addEventListener("keydown", onKeyDown);
      window.addEventListener("keyup", onKeyUp);
      boundGlobals = true;
    }
    // The right utility panel animates its width over 280ms and fires no event
    // of its own, so watch the map box directly: the pad then travels WITH the
    // sidebar instead of jumping once the transition has finished. The window
    // listener above stays as the fallback where ResizeObserver is missing.
    if (hostObserver) { hostObserver.disconnect(); hostObserver = null; }
    if (typeof ResizeObserver === "function" && boundsEl()) {
      hostObserver = new ResizeObserver(onHostResize);
      hostObserver.observe(boundsEl());
    }
    Corvus.ui.refreshIcons();
    apply();
  }

  return {
    init,
    setEnabled,
    setKeysEnabled,
    setWasdEnabled,
    isEnabled: () => showSticks,
    isKeysEnabled: () => showKeys,
    isWasdEnabled: () => showWasd,
    resetPosition,
  };
})();
