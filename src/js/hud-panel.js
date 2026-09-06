"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.hudPanel — window behaviour for the flight HUD on the Home map.

  The HUD used to be nailed to the map's bottom-right corner, where it covers
  exactly the ground the operator is flying over as often as not. It is a small
  window now, with four affordances and deliberately no title bar:

    drag      grab the panel anywhere that is not a button and move it
    pin       lock the position so a stray drag over the map cannot shift it —
              the one that matters in the field, where the panel sits under a
              thumb on a trackpad while the aircraft is airborne
    compact   same readouts, smaller: instruments shrink and the telemetry grid
              tightens, for when the map matters more than the numbers
    readouts  fold away ONLY the ALT AMSL / AGL / GS / V/S / HDG / SAT grid and
              keep the two dials, for the operator flying off the compass and
              horizon with the numbers already on the top bar
    collapse  everything folds away and the panel becomes a small control pill

  No chrome by default. A title bar would cost a permanent strip of the map to
  say "FLIGHT", which the compass and horizon underneath it already say. The
  three controls sit over the panel's top-right corner and stay invisible until
  a pointer is over the panel or a control has keyboard focus — so the resting
  state is instruments on a map, and the controls appear when reached for.
  Collapsed is the exception: with the readouts hidden the controls are all
  that is left, so there they stay visible.

  Kept separate from instruments.js on purpose: that module renders the compass,
  the attitude indicator and the telemetry cells, and knows nothing about where
  the panel sits. This one moves the box and never touches its contents.

  Position and state are persisted in localStorage, because a panel the
  operator has to re-arrange on every launch is worse than one that never moved.
  Nothing here is required for flight: every failure path leaves the panel at
  its default corner rather than throwing.
*/
Corvus.hudPanel = (function () {
  const KEY = "corvus.hud";
  // Keep at least this much of the panel on screen when clamping, so it can
  // never be dragged (or resized) entirely out of reach.
  const MIN_VISIBLE = 64;
  // Margin kept between the panel and the map edge when the MAP moves under
  // it, rather than the panel over the map. See contain().
  const EDGE = 8;

  let panelEl = null;
  let bodyEl = null;
  let telemetryEl = null;
  let actionsEl = null;
  let pinBtn = null;
  let sizeBtn = null;
  let readoutsBtn = null;
  let collapseBtn = null;

  const DEFAULTS = {
    x: null, y: null, pinned: false, compact: false, collapsed: false,
    readouts: true,
  };
  let state = Object.assign({}, DEFAULTS);
  let drag = null;   // {pointerId, dx, dy} while a drag is in flight
  // The map's size at the last reflow, so a change in it can be told from a
  // re-render. See onHostResize.
  let lastHost = { w: 0, h: 0 };
  let hostObserver = null;

  // ---- persistence -------------------------------------------------------

  function load() {
    // Always start from the defaults, then overlay what was stored. Returning
    // early on empty/corrupt storage would leave whatever the previous init
    // left behind — which is wrong the moment init runs twice, and is exactly
    // what made this module untestable.
    state = Object.assign({}, DEFAULTS);
    try {
      const raw = localStorage.getItem(KEY);
      if (!raw) return;
      const saved = JSON.parse(raw);
      if (!saved || typeof saved !== "object") return;
      if (typeof saved.x === "number") state.x = saved.x;
      if (typeof saved.y === "number") state.y = saved.y;
      state.pinned = !!saved.pinned;
      state.compact = !!saved.compact;
      state.collapsed = !!saved.collapsed;
      // Absent means "shown": a panel saved before this control existed had
      // its readouts up, and must not come back with them folded away.
      state.readouts = saved.readouts !== false;
    } catch (_e) { /* unreadable storage -> the defaults set above */ }
  }

  function save() {
    try { localStorage.setItem(KEY, JSON.stringify(state)); } catch (_e) {}
  }

  // ---- geometry ----------------------------------------------------------

  /** The area the panel may occupy: the map view, not the whole window. */
  function boundsEl() {
    return panelEl && panelEl.parentElement ? panelEl.parentElement : null;
  }

  /**
   * The factor between the pixels the pointer reports and the pixels this
   * panel is positioned in.
   *
   * The interface-scale control puts a CSS `zoom` on <body>, so
   * getBoundingClientRect and pointer clientX/Y come back in SCALED pixels
   * while style.left/top are written in UNSCALED ones. Everything below that
   * mixes the two has to divide by this; offsetWidth/clientWidth are already
   * unscaled and need no correction, which is why clamp() uses them. Returns
   * 1 at 100%, and on any element the browser reports no box for.
   */
  function pointerScale(el) {
    if (!el) return 1;
    const rect = el.getBoundingClientRect();
    return (rect.width && el.offsetWidth) ? (rect.width / el.offsetWidth) : 1;
  }

  /**
   * Constrain (x, y) so at least MIN_VISIBLE px of the panel stays inside the
   * map on every edge. Applied on drag, on window resize, and when a stored
   * position is restored into a smaller window than it was saved from.
   */
  function clamp(x, y) {
    const host = boundsEl();
    if (!host) return { x, y };
    const maxX = Math.max(0, host.clientWidth - MIN_VISIBLE);
    const maxY = Math.max(0, host.clientHeight - MIN_VISIBLE);
    return {
      x: Math.min(Math.max(x, MIN_VISIBLE - panelEl.offsetWidth), maxX),
      y: Math.min(Math.max(y, 0), maxY),   // never above the top edge
    };
  }

  /** The map's size in its own (unscaled) pixels. */
  function hostSize() {
    const host = boundsEl();
    return host ? { w: host.clientWidth, h: host.clientHeight } : { w: 0, h: 0 };
  }

  /**
   * Keep the WHOLE panel on the map, with a small margin — stricter than
   * clamp(), which deliberately lets a deliberate drag leave only a strip
   * showing. Used when the map resizes under a panel the operator is not
   * touching: they did not ask for it to be half off screen, so it is not.
   * An axis the panel is simply too large for falls back to clamp().
   */
  function contain(x, y) {
    const host = boundsEl();
    if (!host) return { x, y };
    const loose = clamp(x, y);
    const maxX = host.clientWidth - panelEl.offsetWidth - EDGE;
    const maxY = host.clientHeight - panelEl.offsetHeight - EDGE;
    return {
      x: maxX >= EDGE ? Math.min(Math.max(x, EDGE), maxX) : loose.x,
      y: maxY >= EDGE ? Math.min(Math.max(y, EDGE), maxY) : loose.y,
    };
  }

  /**
   * The map changed size — nearly always because the right utility panel was
   * slid open or shut, and otherwise because the window was resized.
   *
   * A panel that has been dragged is positioned from the map's TOP-LEFT, so a
   * map that narrows from the right leaves it exactly where it was: behind the
   * utility panel, which outranks it in the stacking order and simply covers
   * it. So a panel sitting nearer the right edge travels with that edge, and
   * one sitting nearer the left stays put — which is what "the window kept its
   * place" means for either half of the map. The whole panel is then contained
   * rather than merely clamped, because this move is not the operator's doing.
   */
  function onHostResize() {
    if (!panelEl) return;
    const size = hostSize();
    // A hidden map — the operator is on Setup, Options or any other page —
    // reports 0x0, and a box with no size says nothing about where a panel
    // belongs. Containing against it collapses every coordinate to the origin,
    // which is how a carefully arranged panel used to come back to the
    // top-left corner after a round trip through another page. So there is
    // nothing to reflow against and nothing is touched — `lastHost` least of
    // all, because the next real resize still needs the last real size to tell
    // which edge the panel was travelling with.
    if (!size.w || !size.h) return;
    // Showing the map again after a page visit is not a resize: the box came
    // back the size it left. Reflowing anyway would re-`contain()` a panel the
    // operator had deliberately parked overhanging the edge, nudging it a few
    // pixels on every single visit to Setup and back.
    if (size.w === lastHost.w && size.h === lastHost.h) {
      applyPosition();
      return;
    }
    if (state.x !== null && lastHost.w && size.w && size.w !== lastHost.w) {
      const nearRight = (lastHost.w - (state.x + panelEl.offsetWidth)) < state.x;
      if (nearRight) state.x += size.w - lastHost.w;
    }
    if (state.y !== null && lastHost.h && size.h && size.h !== lastHost.h) {
      const nearBottom = (lastHost.h - (state.y + panelEl.offsetHeight)) < state.y;
      if (nearBottom) state.y += size.h - lastHost.h;
    }
    lastHost = size;
    if (state.x !== null && state.y !== null) {
      const c = contain(state.x, state.y);
      state.x = c.x; state.y = c.y;
    }
    applyPosition();
  }

  /** Write the current position to the element. A null position means "leave
   *  it at the CSS default corner" — the panel has never been moved. */
  function applyPosition() {
    if (!panelEl) return;
    if (state.x === null || state.y === null) {
      panelEl.style.left = "";
      panelEl.style.top = "";
      panelEl.classList.remove("is-placed");
      return;
    }
    const c = clamp(state.x, state.y);
    state.x = c.x; state.y = c.y;
    // .is-placed drops the CSS right/bottom anchoring so left/top can win.
    panelEl.classList.add("is-placed");
    panelEl.style.left = c.x + "px";
    panelEl.style.top = c.y + "px";
  }

  function applyState() {
    if (!panelEl) return;
    panelEl.classList.toggle("is-pinned", state.pinned);
    panelEl.classList.toggle("is-compact", state.compact);
    panelEl.classList.toggle("is-collapsed", state.collapsed);
    panelEl.classList.toggle("is-readouts-off", !state.readouts);
    if (bodyEl) bodyEl.hidden = state.collapsed;
    // `hidden` as well as the class, because .flight-telemetry sets its own
    // `display: grid` and a class rule would beat the UA's [hidden] on its
    // own — the same trap .hud-body and the joystick surfaces sit in.
    if (telemetryEl) telemetryEl.hidden = !state.readouts;

    syncButton(pinBtn, state.pinned, state.pinned ? "pin-off" : "pin",
      state.pinned ? "Unlock position" : "Lock position");
    syncButton(sizeBtn, state.compact, state.compact ? "maximize-2" : "minimize-2",
      state.compact ? "Full size" : "Compact size");
    // A double chevron, so it does not read as a second copy of the panel's
    // own collapse: this one folds a section, that one folds the window.
    syncButton(readoutsBtn, !state.readouts,
      state.readouts ? "chevrons-down-up" : "chevrons-up-down",
      state.readouts ? "Hide the readouts" : "Show the readouts");
    syncButton(collapseBtn, false, state.collapsed ? "chevron-up" : "chevron-down",
      state.collapsed ? "Expand" : "Collapse");
    Corvus.ui.refreshIcons();
    applyPosition();
  }

  /** Repoint an icon button at a different glyph and re-label it. Lucide has
   *  already replaced the <i> with an <svg>, so the old node is swapped out
   *  rather than mutated. */
  function syncButton(btn, active, iconName, label) {
    if (!btn) return;
    btn.classList.toggle("active", !!active);
    btn.title = label;
    btn.setAttribute("aria-label", label);
    btn.setAttribute("aria-pressed", active ? "true" : "false");
    Corvus.ui.clear(btn).appendChild(Corvus.ui.icon(iconName, 13));
  }

  // ---- dragging ----------------------------------------------------------

  function onPointerDown(e) {
    // Left button only, never while pinned, and never from one of the header
    // buttons (their click must not start a drag).
    if (state.pinned || e.button !== 0) return;
    if (e.target.closest(".icon-btn")) return;
    const host = boundsEl();
    if (!host) return;

    const hb = host.getBoundingClientRect();
    const pb = panelEl.getBoundingClientRect();
    const k = pointerScale(host);
    drag = {
      pointerId: e.pointerId,
      // Grab offset in the panel's own (unscaled) pixels, so the point under
      // the cursor stays under the cursor at any interface size.
      dx: (e.clientX - pb.left) / k,
      dy: (e.clientY - pb.top) / k,
      hostLeft: hb.left,
      hostTop: hb.top,
      scale: k,
    };
    // Capture so the drag survives the pointer leaving the header — including
    // over the map canvas, which would otherwise swallow the move events.
    try { panelEl.setPointerCapture(e.pointerId); } catch (_e) {}
    panelEl.classList.add("is-dragging");
    e.preventDefault();
  }

  function onPointerMove(e) {
    if (!drag || e.pointerId !== drag.pointerId) return;
    state.x = (e.clientX - drag.hostLeft) / drag.scale - drag.dx;
    state.y = (e.clientY - drag.hostTop) / drag.scale - drag.dy;
    applyPosition();
  }

  function onPointerUp(e) {
    if (!drag || e.pointerId !== drag.pointerId) return;
    try { panelEl.releasePointerCapture(drag.pointerId); } catch (_e) {}
    drag = null;
    panelEl.classList.remove("is-dragging");
    save();
  }

  /** Double-click the title bar to send the panel home. The only way back to
   *  the default corner once it has been moved. */
  function resetPosition() {
    state.x = null;
    state.y = null;
    applyPosition();
    save();
  }

  // ---- header ------------------------------------------------------------

  /**
   * The three controls, as a cluster that floats over the panel's top-right
   * corner. No title bar: CSS keeps it invisible until the pointer is over the
   * panel or a control takes keyboard focus.
   */
  function buildActions() {
    const actions = document.createElement("div");
    actions.className = "hud-actions";
    pinBtn = Corvus.ui.iconButton("pin", { size: 13, onClick: togglePin });
    sizeBtn = Corvus.ui.iconButton("minimize-2", { size: 13, onClick: toggleCompact });
    readoutsBtn = Corvus.ui.iconButton("chevrons-down-up", {
      size: 13, className: "icon-btn hud-readouts", onClick: toggleReadouts,
    });
    collapseBtn = Corvus.ui.iconButton("chevron-down", { size: 13, onClick: toggleCollapsed });
    actions.appendChild(pinBtn);
    actions.appendChild(sizeBtn);
    actions.appendChild(readoutsBtn);
    actions.appendChild(collapseBtn);
    return actions;
  }

  /** Make the whole panel the drag surface. With no title bar there is no
   *  dedicated handle, and the panel has no interactive content of its own —
   *  only the control buttons, which onPointerDown excludes. */
  function wireDragSurface() {
    panelEl.addEventListener("pointerdown", onPointerDown);
    panelEl.addEventListener("pointermove", onPointerMove);
    panelEl.addEventListener("pointerup", onPointerUp);
    panelEl.addEventListener("pointercancel", onPointerUp);
    panelEl.addEventListener("dblclick", (e) => {
      if (e.target.closest(".icon-btn")) return;
      resetPosition();
    });
  }

  function togglePin() { state.pinned = !state.pinned; applyState(); save(); }
  function toggleCompact() { state.compact = !state.compact; applyState(); save(); }
  function toggleReadouts() { state.readouts = !state.readouts; applyState(); save(); }
  function toggleCollapsed() { state.collapsed = !state.collapsed; applyState(); save(); }

  // ---- lifecycle ---------------------------------------------------------

  /**
   * Wrap the existing HUD contents in a draggable panel. Called from app.init
   * AFTER Corvus.instruments.init, so the instruments are already built and
   * simply get re-parented into the body — their element references stay
   * valid, which is why this can be layered on without touching that module.
   */
  function init(overlayEl) {
    panelEl = overlayEl;
    if (!panelEl) return;
    load();

    bodyEl = document.createElement("div");
    bodyEl.className = "hud-body";
    // Move the instruments + telemetry grid into the collapsible body. The
    // nodes are moved, not recreated, so instruments.js keeps its references.
    while (panelEl.firstChild) bodyEl.appendChild(panelEl.firstChild);
    // The numeric grid, so the readouts control can fold it on its own. Null
    // is a normal outcome — instruments.js may not have built it yet.
    telemetryEl = bodyEl.querySelector(".flight-telemetry");

    actionsEl = buildActions();
    panelEl.appendChild(actionsEl);
    panelEl.appendChild(bodyEl);
    // The panel names itself for assistive tech now that no visible title
    // bar does. It is a supplementary readout, not a landmark.
    panelEl.setAttribute("role", "group");
    panelEl.setAttribute("aria-label", "Flight instruments");
    wireDragSurface();

    lastHost = hostSize();
    applyState();

    // A map that shrinks must not strand the panel outside it — or, when the
    // right utility panel slides open, behind it. The previous handler is
    // dropped first so a re-init cannot stack listeners.
    window.removeEventListener("resize", onHostResize);
    window.addEventListener("resize", onHostResize);
    // The utility panel animates its width over 280ms and fires no event of
    // its own, so watch the map box directly: the panel then travels WITH the
    // sidebar instead of jumping once the transition has finished. The window
    // listener above stays as the fallback where ResizeObserver is missing.
    if (hostObserver) { hostObserver.disconnect(); hostObserver = null; }
    if (typeof ResizeObserver === "function" && boundsEl()) {
      hostObserver = new ResizeObserver(onHostResize);
      hostObserver.observe(boundsEl());
    }
  }

  return {
    init,
    resetPosition,
    // test hooks: the persisted state and the clamp, both pure enough to
    // assert on without a layout engine.
    _state: () => Object.assign({}, state),
    _setState: (s) => { state = Object.assign(state, s); applyState(); },
  };
})();
