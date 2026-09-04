"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.hudPanel — window behaviour for the flight HUD on the Home map.

  The HUD used to be nailed to the map's bottom-right corner, where it covers
  exactly the ground the operator is flying over as often as not. This module
  gives it a title bar and four affordances:

    drag      grab the title bar and put it anywhere over the map
    pin       lock the position so a stray drag over the map cannot move it —
              the one that matters in the field, where the panel sits under a
              thumb on a trackpad while the aircraft is airborne
    compact   same readouts, smaller: instruments shrink and the telemetry grid
              tightens, for when the map matters more than the numbers
    collapse  title bar only

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

  let panelEl = null;
  let bodyEl = null;
  let headEl = null;
  let pinBtn = null;
  let sizeBtn = null;
  let collapseBtn = null;

  const DEFAULTS = { x: null, y: null, pinned: false, compact: false, collapsed: false };
  let state = Object.assign({}, DEFAULTS);
  let drag = null;   // {pointerId, dx, dy} while a drag is in flight

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
   * Constrain (x, y) so at least MIN_VISIBLE px of the panel stays inside the
   * map on every edge. Applied on drag, on window resize, and when a stored
   * position is restored into a smaller window than it was saved from.
   */
  function clamp(x, y) {
    const host = boundsEl();
    if (!host) return { x, y };
    const hb = host.getBoundingClientRect();
    const pb = panelEl.getBoundingClientRect();
    const maxX = Math.max(0, hb.width - MIN_VISIBLE);
    const maxY = Math.max(0, hb.height - MIN_VISIBLE);
    return {
      x: Math.min(Math.max(x, MIN_VISIBLE - pb.width), maxX),
      y: Math.min(Math.max(y, 0), maxY),   // never above the top edge
    };
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
    if (bodyEl) bodyEl.hidden = state.collapsed;

    syncButton(pinBtn, state.pinned, state.pinned ? "pin-off" : "pin",
      state.pinned ? "Unlock position" : "Lock position");
    syncButton(sizeBtn, state.compact, state.compact ? "maximize-2" : "minimize-2",
      state.compact ? "Full size" : "Compact size");
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
    drag = {
      pointerId: e.pointerId,
      dx: e.clientX - pb.left,
      dy: e.clientY - pb.top,
      hostLeft: hb.left,
      hostTop: hb.top,
    };
    // Capture so the drag survives the pointer leaving the header — including
    // over the map canvas, which would otherwise swallow the move events.
    try { headEl.setPointerCapture(e.pointerId); } catch (_e) {}
    panelEl.classList.add("is-dragging");
    e.preventDefault();
  }

  function onPointerMove(e) {
    if (!drag || e.pointerId !== drag.pointerId) return;
    state.x = e.clientX - drag.hostLeft - drag.dx;
    state.y = e.clientY - drag.hostTop - drag.dy;
    applyPosition();
  }

  function onPointerUp(e) {
    if (!drag || e.pointerId !== drag.pointerId) return;
    try { headEl.releasePointerCapture(drag.pointerId); } catch (_e) {}
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

  function buildHeader() {
    const head = document.createElement("div");
    head.className = "hud-head";

    const grip = Corvus.ui.icon("grip-horizontal", 13);
    grip.classList.add("hud-grip");
    head.appendChild(grip);

    const title = document.createElement("span");
    title.className = "hud-title";
    title.textContent = "FLIGHT";
    head.appendChild(title);

    const actions = document.createElement("div");
    actions.className = "hud-actions";
    pinBtn = Corvus.ui.iconButton("pin", { size: 13, onClick: togglePin });
    sizeBtn = Corvus.ui.iconButton("minimize-2", { size: 13, onClick: toggleCompact });
    collapseBtn = Corvus.ui.iconButton("chevron-down", { size: 13, onClick: toggleCollapsed });
    actions.appendChild(pinBtn);
    actions.appendChild(sizeBtn);
    actions.appendChild(collapseBtn);
    head.appendChild(actions);

    head.addEventListener("pointerdown", onPointerDown);
    head.addEventListener("pointermove", onPointerMove);
    head.addEventListener("pointerup", onPointerUp);
    head.addEventListener("pointercancel", onPointerUp);
    head.addEventListener("dblclick", (e) => {
      if (e.target.closest(".icon-btn")) return;
      resetPosition();
    });
    return head;
  }

  function togglePin() { state.pinned = !state.pinned; applyState(); save(); }
  function toggleCompact() { state.compact = !state.compact; applyState(); save(); }
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

    headEl = buildHeader();
    panelEl.appendChild(headEl);
    panelEl.appendChild(bodyEl);

    applyState();

    // A window that shrinks must not strand the panel outside the map. The
    // previous handler is dropped first so a re-init cannot stack listeners.
    window.removeEventListener("resize", applyPosition);
    window.addEventListener("resize", applyPosition);
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
