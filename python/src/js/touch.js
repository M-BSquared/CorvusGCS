"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.touch — tablet mode.

  One switch (Settings > Interface > Tablet mode) that turns the same app into
  one a finger can fly. It is a document attribute and a handful of gestures,
  not a second layout: `data-touch="on"` on <html> is what css/touch.css and
  the touch section of css/components.css key on, exactly as data-theme is for
  the palette, so no page has to know which mode it is drawn in.

  What the attribute does in CSS: every control grows to a target a finger can
  hit (--touch-target), the controls that only appear on hover are shown, and
  the parts that need a mouse or a keyboard are taken away (anything marked
  `data-touch-hide`, the side panel's resize edge, the SSH terminal tab).

  What this module does on top, because CSS cannot:

    Long press   a held finger is the right click a tablet does not have.
                 iPadOS Safari never sends `contextmenu` for one, so the map's
                 context menu, removing the last planned point, the mission
                 point menus and the 3D and camera panels were unreachable.
                 After LONG_PRESS_MS without moving, a `contextmenu` is
                 dispatched at the finger, and every handler already written
                 for the right button answers it. The bare map is left to
                 MapLibre, which does the same there. Android does send its
                 own; whichever comes first wins and the other is dropped.
    Page zoom    a double tap or a pinch outside the map would zoom the page
                 like a website, and iOS zooms in on any field it focuses.
                 The viewport is held at 1 while the mode is on; the map's own
                 pinch is untouched. The interface scale is still the way to
                 make everything bigger.
    Double tap   the double click that sends a window back to its corner or
                 maximizes a camera, sent where the browser does not send one
                 itself (iPadOS, for most elements). Never on the map, whose
                 double tap is MapLibre's zoom.
    Hold to move a window dragged by its whole surface (the flight HUD) moves
                 only after the finger has been held on it for HOLD_TO_MOVE_MS,
                 so a thumb resting on it or a swipe meant for the map leaves
                 it where it is. See holdToMove().
    Fullscreen   the rail offers it where the browser can do it, because a
                 tab strip and an address bar are a tenth of a tablet.

  Stored on THIS device only (localStorage), never in the backend config.
  A tablet reaches Corvus over the network and shares the laptop's config, so
  a config key would put the laptop into tablet mode the moment the tablet
  switched it on. Until the operator has chosen, a device whose primary
  pointer is a finger (iPad, Android tablet) starts in it and a laptop does
  not; the inline script in index.html makes the same decision before the
  first paint.
*/
Corvus.touch = (function () {
  const KEY = "corvus.touch";
  const ATTR = "data-touch";
  // Just after MapLibre's own 500 ms, so on the map its answer comes first and
  // the point under the finger is still there for onMarker() to find.
  const LONG_PRESS_MS = 550;
  const MOVE_TOLERANCE = 10;
  // How long after a fired long press the browser's own contextmenu is still
  // taken to belong to it, and how long after the finger lifts its click is.
  const AFTERMATH_MS = 800;
  const CLICK_GUARD_MS = 400;
  const HOLD_TO_MOVE_MS = 300;
  // Two taps this close in time and place are a double tap, and a browser's
  // own dblclick this soon after one sent from here is the same gesture.
  const DOUBLE_TAP_MS = 320;
  const DOUBLE_TAP_PX = 24;
  // The first is index.html's own; the inline script there may already have
  // swapped in the second, so the free one is not read back from the page.
  const FREE_VIEWPORT = "width=device-width, initial-scale=1.0";
  const HELD_VIEWPORT = "width=device-width, initial-scale=1.0, maximum-scale=1.0";

  let enabled = false;
  let press = null;
  let firedAt = 0;
  let clickGuardUntil = 0;
  let touchesDown = 0;
  let hold = null;
  let lastTap = null;
  let nativeDblAt = -Infinity;
  let sentDblAt = -Infinity;

  function now() {
    return (typeof performance !== "undefined" && performance.now) ? performance.now() : Date.now();
  }

  /** Whether this device's primary pointer is a finger. */
  function coarsePointer() {
    try {
      return !!(window.matchMedia && window.matchMedia("(pointer: coarse)").matches);
    } catch (_e) {
      return false;
    }
  }

  /** Whether this device has a touch screen at all, primary or not. */
  function hasTouchscreen() {
    if (coarsePointer()) return true;
    try {
      return !!(typeof navigator !== "undefined" && navigator.maxTouchPoints > 0);
    } catch (_e) {
      return false;
    }
  }

  /** The stored choice: true, false, or null while the operator has made none. */
  function stored() {
    try {
      const v = localStorage.getItem(KEY);
      if (v === "1") return true;
      if (v === "0") return false;
    } catch (_e) {}
    return null;
  }

  /** What the device starts in: the stored choice, else a finger means on. */
  function preferred() {
    const s = stored();
    return s === null ? coarsePointer() : s;
  }

  function viewportMeta() {
    try {
      return document.querySelector('meta[name="viewport"]');
    } catch (_e) {
      return null;
    }
  }

  function holdViewport(on) {
    const meta = viewportMeta();
    if (meta) meta.setAttribute("content", on ? HELD_VIEWPORT : FREE_VIEWPORT);
  }

  /**
   * Turn tablet mode on or off for this document. *persist* false applies it
   * without recording a choice (the start, from the stored or detected value).
   * Returns the state applied.
   */
  function setEnabled(on, persist) {
    enabled = !!on;
    try {
      if (enabled) document.documentElement.setAttribute(ATTR, "on");
      else document.documentElement.removeAttribute(ATTR);
    } catch (_e) {}
    holdViewport(enabled);
    if (!enabled) { cancelPress(); cancelHold(); }
    if (persist !== false) {
      try { localStorage.setItem(KEY, enabled ? "1" : "0"); } catch (_e) {}
    }
    sync();
    try {
      window.dispatchEvent(new CustomEvent("corvus:touchchange", { detail: { touch: enabled } }));
      // The rail, the panel and the map chrome all change size with it, and
      // everything that measures the layout already listens for a resize.
      window.dispatchEvent(new Event("resize"));
    } catch (_e) {}
    return enabled;
  }

  /** Apply what this device starts in (called before the modules build). */
  function applySaved() {
    return setEnabled(preferred(), false);
  }

  /** Bring the modules built after applySaved() in line with the mode. */
  function sync() {
    if (Corvus.panel && typeof Corvus.panel.setTabHidden === "function") {
      // A terminal is a keyboard and a mouse selecting text; neither is here.
      Corvus.panel.setTabHidden("ssh", enabled, "touch");
    }
    if (Corvus.sidenav && typeof Corvus.sidenav.refreshRail === "function") {
      Corvus.sidenav.refreshRail();
    }
  }

  // ---- fullscreen ----

  function fullscreenElement() {
    return document.fullscreenElement || document.webkitFullscreenElement || null;
  }

  /** Whether the browser will put the page in fullscreen. The desktop app's
   *  web view says no, and has a window of its own for that anyway. */
  function canFullscreen() {
    try {
      return !!(document.fullscreenEnabled || document.webkitFullscreenEnabled);
    } catch (_e) {
      return false;
    }
  }

  function isFullscreen() {
    return !!fullscreenElement();
  }

  /** Enter or leave fullscreen. Resolves either way: a refusal leaves the page
   *  as it was, which is all there is to do about it. */
  function toggleFullscreen() {
    const root = document.documentElement;
    try {
      if (isFullscreen()) {
        const exit = document.exitFullscreen || document.webkitExitFullscreen;
        return Promise.resolve(exit && exit.call(document)).catch(() => {});
      }
      const enter = root.requestFullscreen || root.webkitRequestFullscreen;
      return Promise.resolve(enter && enter.call(root)).catch(() => {});
    } catch (_e) {
      return Promise.resolve();
    }
  }

  function buzz() {
    try {
      if (typeof navigator !== "undefined" && typeof navigator.vibrate === "function") navigator.vibrate(10);
    } catch (_e) {}
  }

  /** A control that has taken this finger for a gesture of its own (a window
   *  being dragged, a row being reordered) is not also being long pressed. */
  function claim(pointerId) {
    if (press && press.id === pointerId) cancelPress();
  }

  // ---- hold to move ----

  function cancelHold() {
    if (hold && hold.timer) clearTimeout(hold.timer);
    hold = null;
  }

  /**
   * Hold to move, for a window dragged by its whole surface. Call it from the
   * window's pointerdown: it returns false for anything but a finger in
   * tablet mode, and the caller starts its drag at once as it always has.
   * For a finger it returns true and the caller starts nothing; *onLift*
   * runs once the finger has been held still for HOLD_TO_MOVE_MS, and from
   * there the caller's own pointermove moves the window. A finger that moves
   * or lifts first did not mean to move it.
   */
  function holdToMove(e, onLift) {
    if (!enabled || !e || e.pointerType !== "touch") return false;
    cancelHold();
    const id = e.pointerId;
    hold = {
      id, x: e.clientX, y: e.clientY,
      timer: setTimeout(() => {
        hold = null;
        claim(id);
        buzz();
        if (typeof onLift === "function") onLift();
      }, HOLD_TO_MOVE_MS),
    };
    return true;
  }

  // ---- double tap ----

  /** Where a double tap is somebody else's: the map zooms on its own, and a
   *  field selects a word. */
  function ownsDoubleTap(target) {
    if (isField(target)) return true;
    return !!(target && typeof target.closest === "function" && target.closest(".maplibregl-map"));
  }

  function onTapEnd(e, moved) {
    if (moved || ownsDoubleTap(e.target)) { lastTap = null; return; }
    const t = now();
    const prev = lastTap;
    if (prev && t - prev.at < DOUBLE_TAP_MS && prev.target === e.target
        && Math.hypot(e.clientX - prev.x, e.clientY - prev.y) < DOUBLE_TAP_PX) {
      lastTap = null;
      const target = e.target;
      const x = e.clientX;
      const y = e.clientY;
      // After the browser has had its turn: one that sends dblclick itself
      // (Chrome on Android) has done so by then, and gets no second one.
      setTimeout(() => {
        if (now() - nativeDblAt < DOUBLE_TAP_MS) return;
        let event;
        try {
          event = new MouseEvent("dblclick", { bubbles: true, cancelable: true, composed: true, clientX: x, clientY: y, button: 0, detail: 2 });
        } catch (_e) {
          return;
        }
        event.corvusDoubleTap = true;
        sentDblAt = now();
        if (target && typeof target.dispatchEvent === "function") target.dispatchEvent(event);
      }, 30);
      return;
    }
    lastTap = { at: t, x: e.clientX, y: e.clientY, target: e.target };
  }

  /** A browser dblclick is noted, and dropped when this module already sent
   *  one for the same two taps. */
  function onDblClick(e) {
    if (!enabled || e.corvusDoubleTap) return;
    if (now() - sentDblAt < DOUBLE_TAP_MS * 2) {
      e.preventDefault();
      e.stopImmediatePropagation();
      return;
    }
    nativeDblAt = now();
  }

  // ---- long press ----

  /** Fields keep the platform's own presses: select, copy, paste. */
  function isField(target) {
    let n = target;
    while (n && n.nodeType === 1) {
      const tag = n.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
      if (n.isContentEditable) return true;
      if (n.classList && n.classList.contains("xterm")) return true;
      n = n.parentNode;
    }
    return false;
  }

  /** A stick or a key held on the pad is flying, not asking for a menu. */
  function onPad(target) {
    return !!(target && typeof target.closest === "function" && target.closest(".joystick-pad"));
  }

  /** MapLibre turns a held finger on the map itself into the map's own
   *  `contextmenu` (at 500 ms, the same as here), so the canvas needs nothing
   *  dispatched. What sits on the map (a mission point, a fence corner) is
   *  not the canvas, and that is what this module is for. */
  function isMapCanvas(target) {
    return !!(target && target.classList && target.classList.contains("maplibregl-canvas"));
  }

  function inMarker(target) {
    return !!(target && typeof target.closest === "function" && target.closest(".maplibregl-marker"));
  }

  function cancelPress() {
    if (press && press.timer) clearTimeout(press.timer);
    press = null;
  }

  function firePress() {
    const p = press;
    if (!p || !enabled) return;
    p.timer = null;
    p.held = true;
    if (inMarker(p.target)) return;
    buzz();
    if (isMapCanvas(p.target)) return;
    const target = p.target && p.target.isConnected !== false ? p.target : null;
    if (!target || typeof target.dispatchEvent !== "function") return;
    let event;
    try {
      event = new MouseEvent("contextmenu", {
        bubbles: true, cancelable: true, composed: true,
        clientX: p.x, clientY: p.y, screenX: p.sx, screenY: p.sy,
        button: 2, buttons: 0,
      });
    } catch (_e) {
      return;
    }
    event.corvusLongPress = true;
    firedAt = now();
    target.dispatchEvent(event);
  }

  function onPointerDown(e) {
    if (e.pointerType !== "touch") return;
    touchesDown += 1;
    if (touchesDown > 1) { cancelHold(); lastTap = null; }
    // A second finger is a pinch or a two-stick hold, never a long press.
    if (!enabled || touchesDown > 1 || isField(e.target)) { cancelPress(); return; }
    cancelPress();
    press = {
      id: e.pointerId, target: e.target, held: false, moved: false,
      x: e.clientX, y: e.clientY, sx: e.screenX, sy: e.screenY,
      // Tracked on the pad all the same: its grip bar's double tap sends it home.
      timer: onPad(e.target) ? null : setTimeout(firePress, LONG_PRESS_MS),
    };
  }

  function onPointerMove(e) {
    if (hold && e.pointerId === hold.id
        && Math.hypot(e.clientX - hold.x, e.clientY - hold.y) > MOVE_TOLERANCE) cancelHold();
    if (!press || press.held || e.pointerId !== press.id) return;
    if (Math.hypot(e.clientX - press.x, e.clientY - press.y) > MOVE_TOLERANCE) {
      clearTimeout(press.timer);
      press.timer = null;
      press.moved = true;
    }
  }

  function onPointerEnd(e) {
    if (e.pointerType !== "touch") return;
    touchesDown = Math.max(0, touchesDown - 1);
    if (hold && e.pointerId === hold.id) cancelHold();
    if (!press || e.pointerId !== press.id) return;
    // A finger that was held long enough to be a right click is not also a
    // tap when it lifts: on the planner that would place a point straight
    // after the press that took one away.
    if (press.held) clickGuardUntil = now() + CLICK_GUARD_MS;
    if (enabled && e.type === "pointerup" && !press.held) onTapEnd(e, press.moved);
    else lastTap = null;
    cancelPress();
  }

  /** The browser's own contextmenu for a press this module already answered
   *  (Android) is dropped, and one that beats the timer cancels it. So is
   *  Android's on a point on the map: see onMarker(). */
  function onContextMenu(e) {
    if (!enabled || e.corvusLongPress) return;
    if (inMarker(e.target)) {
      e.preventDefault();
      e.stopImmediatePropagation();
      return;
    }
    if (press && !press.held) { cancelPress(); return; }
    if (firedAt && now() - firedAt < AFTERMATH_MS) {
      e.preventDefault();
      e.stopImmediatePropagation();
    }
  }

  /**
   * Whether a map `contextmenu` event was a finger held on a point on the
   * map (a mission point, a fence corner) rather than on the map.
   *
   * A point is dragged, and a finger often rests on it a moment before it
   * moves. With a mouse the right click on a point deletes it; under a finger
   * that pause would delete the point the operator was about to move, so in
   * tablet mode a held point is left alone: nothing is sent to it from here,
   * Android's own contextmenu on it is dropped above, and the map's handlers
   * ask this to ignore MapLibre's long press for the spot under it, which
   * would otherwise take the newest point away instead. A point is removed
   * from the list, whose row tools are always shown in tablet mode.
   */
  function onMarker(mapEvent) {
    if (!enabled) return false;
    const o = mapEvent && mapEvent.originalEvent;
    if (!o || typeof document.elementFromPoint !== "function") return false;
    return inMarker(document.elementFromPoint(o.clientX, o.clientY));
  }

  /** Lifting the finger after a long press is not also a tap. */
  function onClick(e) {
    if (!enabled || !clickGuardUntil) return;
    const guarded = now() < clickGuardUntil;
    clickGuardUntil = 0;
    if (guarded) {
      e.preventDefault();
      e.stopImmediatePropagation();
    }
  }

  /* iOS zooms the page on a two-finger pinch whatever the viewport says. The
     map listens to touch events, not to these, so its pinch still works. */
  function onGesture(e) {
    if (enabled) e.preventDefault();
  }

  function bind() {
    if (typeof document === "undefined" || typeof document.addEventListener !== "function") return;
    const capture = { capture: true, passive: true };
    document.addEventListener("pointerdown", onPointerDown, capture);
    document.addEventListener("pointermove", onPointerMove, capture);
    document.addEventListener("pointerup", onPointerEnd, capture);
    document.addEventListener("pointercancel", onPointerEnd, capture);
    window.addEventListener("contextmenu", onContextMenu, true);
    window.addEventListener("click", onClick, true);
    window.addEventListener("dblclick", onDblClick, true);
    document.addEventListener("gesturestart", onGesture, { passive: false });
    document.addEventListener("fullscreenchange", sync);
    document.addEventListener("webkitfullscreenchange", sync);
  }
  bind();

  return {
    setEnabled, applySaved, sync,
    isEnabled: () => enabled,
    stored, preferred, hasTouchscreen, coarsePointer,
    canFullscreen, isFullscreen, toggleFullscreen,
    onMarker, claim, holdToMove,
    // The wording for a gesture: *mouse* where there is one, *finger* in
    // tablet mode. Both are English source text, translated where shown.
    pick: (mouse, finger) => (enabled ? finger : mouse),
    LONG_PRESS_MS, HOLD_TO_MOVE_MS, DOUBLE_TAP_MS, KEY,
  };
})();
