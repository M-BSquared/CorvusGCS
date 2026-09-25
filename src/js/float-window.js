"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.floatWindows — the floating window frame the SSH terminals and the
  camera views share.

  A frame floats over the whole app, is dragged by its title bar, sized from
  its corner grip, maximized from the bar (or a double-click on it), and put
  away with ×. Several can be open at once, and the tab underneath stays where
  it was. What is INSIDE a frame is its owner's business: term-window.js puts
  an xterm there, video-window.js a camera picture.

  One module rather than one copy per kind, because the two must look and
  behave as one thing. They share one layer too, so clicking a camera brings
  it above a terminal and back, and one cascade, so the camera that opens
  after a terminal does not land exactly on it.

  Geometry is remembered per window key for as long as the page lives, so a
  window put in a corner comes back to that corner.

  Everything is measured in UNSCALED pixels: <body> carries the interface-scale
  zoom, so getBoundingClientRect reports scaled pixels while style.left writes
  unscaled ones. Same correction as hud-panel.js, for the same reason.
*/
Corvus.floatWindows = (function () {
  const MIN_W = 320;
  const MIN_H = 170;
  const DEF_W = 640;
  const DEF_H = 400;
  // Kept clear of the viewport edges when a window is maximized or clamped.
  const MARGIN = 12;
  const BAR_H = 34;
  // How much of a window must stay reachable when it is dragged off an edge.
  // Less than this and the title bar — the only way to drag it back — is gone.
  const KEEP_X = 140;
  const CASCADE = 26;
  // Below this width there is nowhere to place a floating window that is not
  // on top of everything anyway, so it opens maximized instead.
  const NARROW = 720;

  /** key -> the open window's record. One window per key, always. */
  const windows = new Map();
  /** key -> the rect it was last at, so a reopened window comes back home. */
  const geometry = new Map();

  let layerEl = null;
  let opened = 0;          // only feeds the cascade, never decremented
  let resizeWired = false;

  /**
   * The rect a window with no remembered geometry opens at: down the right of
   * the viewport, each one a step below and left of the last so a second
   * window never lands exactly on the first.
   *
   * `topInset` keeps the first one clear of the telemetry bar — a window
   * covering the altitude and battery readouts is the one place on this
   * screen a window must not open by itself.
   *
   * Pure, and exported for the test suite.
   *
   * @param {number} index how many windows have been opened before this one
   * @param {{width: number, height: number}} view unscaled viewport
   * @param {number} [topInset] chrome at the top of the screen to stay below
   * @param {{w: number, h: number}} [size] the size to open at, DEF_W x DEF_H by default
   * @returns {{x: number, y: number, w: number, h: number}}
   */
  function cascadeRect(index, view, topInset, size) {
    const top = Math.max(0, Number(topInset) || 0);
    const want = size || {};
    const w = Math.min(Number(want.w) || DEF_W, Math.max(MIN_W, view.width - 2 * MARGIN));
    const h = Math.min(Number(want.h) || DEF_H, Math.max(MIN_H, view.height - top - 2 * MARGIN));
    const step = (index % 6) * CASCADE;
    return clampRect({
      x: view.width - w - MARGIN - step,
      y: top + MARGIN + step,
      w, h,
    }, view);
  }

  /**
   * Constrain a rect to a viewport it has to stay usable in: never larger than
   * the viewport, never smaller than a window can be read at, and never so
   * far out that the title bar cannot be grabbed to bring it back.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} rect {x, y, w, h}
   * @param {{width: number, height: number}} view unscaled viewport
   * @returns {{x: number, y: number, w: number, h: number}}
   */
  function clampRect(rect, view) {
    const r = rect || {};
    const vw = Math.max(1, Number(view && view.width) || 0);
    const vh = Math.max(1, Number(view && view.height) || 0);
    const w = Math.max(Math.min(Number(r.w) || DEF_W, vw), Math.min(MIN_W, vw));
    const h = Math.max(Math.min(Number(r.h) || DEF_H, vh), Math.min(MIN_H, vh));
    const keep = Math.min(KEEP_X, w);
    const x = Math.min(Math.max(Number(r.x) || 0, keep - w), Math.max(0, vw - keep));
    // The bar stays on screen at both ends: above the top edge it is gone for
    // good, below the bottom edge there is nothing left to grab.
    const y = Math.min(Math.max(Number(r.y) || 0, 0), Math.max(0, vh - BAR_H));
    return { x, y, w, h };
  }

  /** The maximized rect: the viewport, less the margin. */
  function fullRect(view) {
    return {
      x: MARGIN,
      y: MARGIN,
      w: Math.max(MIN_W, view.width - 2 * MARGIN),
      h: Math.max(MIN_H, view.height - 2 * MARGIN),
    };
  }

  /**
   * The layer every window lives in: fixed, full-viewport, and transparent to
   * the pointer so the app underneath keeps working everywhere a window is
   * not. Created on the first open and kept afterwards.
   */
  function ensureLayer() {
    if (layerEl && layerEl.parentNode) return layerEl;
    layerEl = document.createElement("div");
    layerEl.className = "term-layer";
    document.body.appendChild(layerEl);
    if (!resizeWired) {
      window.addEventListener("resize", onViewportResize);
      resizeWired = true;
    }
    return layerEl;
  }

  /** The unscaled viewport, measured on the layer (offset* ignores zoom). */
  function viewport() {
    const el = layerEl;
    const w = (el && el.clientWidth) || window.innerWidth || 1024;
    const h = (el && el.clientHeight) || window.innerHeight || 720;
    return { width: w, height: h };
  }

  /** The app chrome a self-placed window opens below: the telemetry bar. */
  function topInset() {
    const raw = (Corvus.ui && typeof Corvus.ui.token === "function")
      ? Corvus.ui.token("--top-h", "") : "";
    const px = parseFloat(raw);
    return isFinite(px) && px > 0 ? px : 0;
  }

  /** The interface scale, for a position given in the page's own pixels. */
  function uiScale() {
    const k = (Corvus.ui && typeof Corvus.ui.uiScale === "function") ? Corvus.ui.uiScale() : 1;
    return (isFinite(k) && k > 0) ? k : 1;
  }

  /** Scaled px per unscaled px, so pointer deltas land where the cursor is. */
  function pointerScale(el) {
    if (!el || typeof el.getBoundingClientRect !== "function") return 1;
    const rect = el.getBoundingClientRect();
    return (rect.width && el.offsetWidth) ? (rect.width / el.offsetWidth) : 1;
  }

  /** Write a record's rect to its element and let the content re-fit. */
  function applyRect(rec) {
    rec.el.style.left = rec.rect.x + "px";
    rec.el.style.top = rec.rect.y + "px";
    rec.el.style.width = rec.rect.w + "px";
    rec.el.style.height = rec.rect.h + "px";
    if (!rec.maximized) geometry.set(rec.key, Object.assign({}, rec.rect));
    if (typeof rec.onResize === "function") rec.onResize(rec);
  }

  /** Bring a window to the front. Stacking is DOM order — no z-index race. */
  function raise(rec) {
    if (rec && rec.el.parentNode === layerEl && layerEl.lastChild !== rec.el) {
      layerEl.appendChild(rec.el);
    }
  }

  function focusContent(rec) {
    if (typeof rec.onFocus === "function") rec.onFocus(rec);
  }

  // ---- the frame ---------------------------------------------------------

  function buildFrame(rec, spec) {
    const ui = Corvus.ui;
    const el = document.createElement("div");
    el.className = "term-win" + (spec.className ? " " + spec.className : "");
    el.setAttribute("role", "dialog");
    el.setAttribute("aria-label", spec.ariaLabel || rec.title);

    const bar = document.createElement("div");
    bar.className = "term-win-bar";

    const icon = document.createElement("span");
    icon.className = "term-win-icon";
    icon.appendChild(ui.icon(spec.icon || "app-window", 14));
    bar.appendChild(icon);

    const info = document.createElement("div");
    info.className = "term-win-info";
    const nameEl = document.createElement("span");
    nameEl.className = "term-win-name";
    nameEl.textContent = rec.title;
    const hostEl = document.createElement("span");
    hostEl.className = "term-win-host";
    hostEl.textContent = String(spec.subtitle || "");
    info.appendChild(nameEl);
    info.appendChild(hostEl);
    bar.appendChild(info);
    rec.nameEl = nameEl;
    rec.hostEl = hostEl;

    const status = document.createElement("span");
    status.className = "ssh-status";
    const dot = document.createElement("span");
    dot.className = "dot";
    status.appendChild(dot);
    status.appendChild(document.createTextNode(""));
    bar.appendChild(status);
    rec.statusEl = status;
    setStatus(rec, spec.status || "", spec.statusTone || "");

    const tools = document.createElement("div");
    tools.className = "term-win-tools";
    if (typeof spec.onPopOut === "function") {
      // The frame can go anywhere in the app's window and nowhere else; this
      // is the way past that edge, onto another screen (see js/popout.js).
      tools.appendChild(ui.iconButton("square-arrow-out-up-right", {
        size: 13,
        title: "Open in a window of its own, which can go anywhere on the screen",
        ariaLabel: `Open the ${rec.noun} in its own window`,
        onClick: () => spec.onPopOut(rec),
      }));
    }
    rec.maxBtn = ui.iconButton("maximize-2", {
      size: 13,
      title: "Maximize",
      ariaLabel: `Maximize the ${rec.noun}`,
      onClick: () => toggleMax(rec),
    });
    tools.appendChild(rec.maxBtn);
    (spec.tools || []).forEach((t) => { if (t) tools.appendChild(t); });
    tools.appendChild(ui.iconButton("x", {
      size: 13,
      title: spec.closeTitle || "Close the window",
      ariaLabel: `Close the ${rec.noun} window`,
      onClick: () => close(rec.key),
    }));
    bar.appendChild(tools);
    el.appendChild(bar);
    rec.barEl = bar;

    const body = document.createElement("div");
    body.className = "term-win-body" + (spec.bodyClass ? " " + spec.bodyClass : "");
    el.appendChild(body);
    rec.bodyEl = body;

    const grip = document.createElement("div");
    grip.className = "term-win-grip";
    grip.setAttribute("aria-hidden", "true");
    el.appendChild(grip);
    rec.gripEl = grip;

    rec.el = el;
    return el;
  }

  /**
   * The pill in the title bar. `tone` is "on" (the green dot: connected,
   * live), "wait" (amber: connecting) or "" (grey: offline).
   * @param {Object} rec
   * @param {string} text
   * @param {string} [tone]
   * @param {string} [title] tooltip, for the reason behind an offline pill
   */
  function setStatus(rec, text, tone, title) {
    const el = rec && rec.statusEl;
    if (!el) return;
    el.classList.toggle("connected", tone === "on");
    el.classList.toggle("pending", tone === "wait");
    el.lastChild.textContent = String(text || "");
    el.hidden = !text;
    if (title) el.title = title;
    else if (el.removeAttribute) el.removeAttribute("title");
  }

  // ---- moving and sizing -------------------------------------------------

  /** Whether a pointer event is outside the app's window. */
  function outsideViewport(e) {
    const w = Number(window.innerWidth);
    const h = Number(window.innerHeight);
    if (!(w > 0) || !(h > 0)) return false;
    return e.clientX < 0 || e.clientY < 0 || e.clientX >= w || e.clientY >= h;
  }

  /**
   * One pointer gesture drives both: the title bar moves the window, the
   * corner grip sizes it. Pointer capture on the window itself, so a fast drag
   * over the map canvas or an xterm does not lose the pointer.
   *
   * A move that carries the pointer out of the app is handed to
   * `rec.onDragOut` (js/popout.js): in the desktop app the frame goes on in a
   * native window of its own from there, following the pointer, and stays in
   * the app if it is let go of over the app again.
   */
  function wireGestures(rec) {
    let drag = null;

    rec.el.addEventListener("pointerdown", (e) => {
      raise(rec);
      if (e.button !== 0) return;
      const onGrip = rec.gripEl.contains(e.target);
      const onBar = rec.barEl.contains(e.target) && !e.target.closest(".icon-btn");
      if (!onGrip && !onBar) return;          // presses in the body are the content's
      if (rec.maximized && !onGrip) return;   // a maximized window has nowhere to go
      const k = pointerScale(rec.el);
      const box = rec.el.getBoundingClientRect();
      drag = {
        id: e.pointerId,
        mode: onGrip ? "size" : "move",
        px: e.clientX / k,
        py: e.clientY / k,
        rect: Object.assign({}, rec.rect),
        // Where the frame was grabbed, in the page's own pixels, so a window
        // outside the app keeps the pointer on the same spot of its bar.
        grabX: e.clientX - box.left,
        grabY: e.clientY - box.top,
        width: box.width,
        height: box.height,
        out: null,
      };
      if (drag.mode === "size" && rec.maximized) restore(rec, true);
      try { rec.el.setPointerCapture(e.pointerId); } catch (_e) {}
      rec.el.classList.add("is-dragging");
      e.preventDefault();
    });

    rec.el.addEventListener("pointermove", (e) => {
      if (!drag || e.pointerId !== drag.id) return;
      if (drag.mode === "move" && !drag.out && typeof rec.onDragOut === "function"
          && outsideViewport(e)) {
        drag.out = rec.onDragOut(rec, { width: drag.width, height: drag.height }) || null;
      }
      if (drag.out) drag.out.move(e.clientX - drag.grabX, e.clientY - drag.grabY);
      const k = pointerScale(rec.el);
      const dx = e.clientX / k - drag.px;
      const dy = e.clientY / k - drag.py;
      const view = viewport();
      if (drag.mode === "move") {
        rec.rect = clampRect({
          x: drag.rect.x + dx, y: drag.rect.y + dy, w: drag.rect.w, h: drag.rect.h,
        }, view);
      } else {
        rec.rect = clampRect({
          x: drag.rect.x, y: drag.rect.y,
          w: drag.rect.w + dx, h: drag.rect.h + dy,
        }, view);
      }
      applyRect(rec);
    });

    const end = (e) => {
      if (!drag || (e && e.pointerId !== drag.id)) return;
      try { rec.el.releasePointerCapture(drag.id); } catch (_e) {}
      const out = drag.out;
      const grab = drag;
      drag = null;
      rec.el.classList.remove("is-dragging");
      if (out) {
        const inside = !!(e && e.type === "pointerup" && !outsideViewport(e));
        out.end(inside);
        if (!inside) return;       // the window outside the app has it now
        // Let go over the app: the frame stays, where the pointer left it.
        rec.el.classList.remove("is-out");
        const k = pointerScale(rec.el);
        rec.rect = clampRect({
          x: (e.clientX - grab.grabX) / k, y: (e.clientY - grab.grabY) / k,
          w: rec.rect.w, h: rec.rect.h,
        }, viewport());
        applyRect(rec);
      }
      if (typeof rec.onResize === "function") rec.onResize(rec);
    };
    rec.el.addEventListener("pointerup", end);
    rec.el.addEventListener("pointercancel", end);

    // Double-clicking the bar is maximize everywhere else; it is here too.
    rec.barEl.addEventListener("dblclick", (e) => {
      if (e.target.closest(".icon-btn")) return;
      toggleMax(rec);
    });
  }

  function syncMaxButton(rec) {
    const label = rec.maximized ? "Restore" : "Maximize";
    rec.maxBtn.title = label;
    rec.maxBtn.setAttribute("aria-label", `${label} the ${rec.noun}`);
    // Lucide has already swapped the <i> for an <svg>, so the glyph is
    // replaced and re-rendered rather than mutated (same as hud-panel.js).
    Corvus.ui.clear(rec.maxBtn)
      .appendChild(Corvus.ui.icon(rec.maximized ? "minimize-2" : "maximize-2", 13));
    Corvus.ui.refreshIcons();
  }

  function toggleMax(rec) {
    if (rec.maximized) restore(rec);
    else {
      rec.prevRect = Object.assign({}, rec.rect);
      rec.maximized = true;
      rec.el.classList.add("is-max");
      rec.rect = fullRect(viewport());
      applyRect(rec);
      syncMaxButton(rec);
    }
    focusContent(rec);
  }

  /** Back to the rect the window had before it was maximized. */
  function restore(rec, keepFocus) {
    if (!rec.maximized) return;
    rec.maximized = false;
    rec.el.classList.remove("is-max");
    rec.rect = clampRect(rec.prevRect || rec.rect, viewport());
    applyRect(rec);
    syncMaxButton(rec);
    if (!keepFocus) focusContent(rec);
  }

  /** A smaller window must not leave its windows stranded off screen. */
  function onViewportResize() {
    const view = viewport();
    windows.forEach((rec) => {
      rec.rect = rec.maximized ? fullRect(view) : clampRect(rec.rect, view);
      applyRect(rec);
    });
  }

  // ---- open / close ------------------------------------------------------

  /**
   * Open a new window frame. Returns the existing record instead when the key
   * already has a window — the caller decides whether that means raising it.
   *
   * @param {Object} spec
   *   key          unique across every kind, e.g. "term:<session>"
   *   kind         what the window shows ("terminal", "video"), for count/keys
   *   title        the bar's name
   *   subtitle     the smaller text beside it (host, camera address)
   *   icon         Lucide name for the bar
   *   noun         "terminal" | "camera": the maximize/close labels read
   *                "Maximize the <noun>"
   *   ariaLabel    the dialog's label
   *   className    a modifier on the frame (term-win--video)
   *   bodyClass    a class for the body the content goes into
   *   status, statusTone  the pill's first text and tone (see setStatus)
   *   tools        extra bar buttons, placed between maximize and ×
   *   onPopOut(rec)  when given, a bar button that takes the content out into
   *                a window of its own (js/popout.js)
   *   onDragOut(rec, size)  called once when a move carries the pointer out
   *                of the app; returns the controller that takes the drag from
   *                there (js/popout.js dragOut), or null to keep the frame in
   *   at           {left, top, width, height} in the page's pixels: open
   *                there, rather than where this key was last or cascaded
   *   closeTitle   the × tooltip
   *   size         {w, h} to open at when there is no remembered rect
   *   onResize(rec)  the frame changed size: re-fit the content
   *   onFocus(rec)   the frame wants the keyboard in its content
   *   onClose(rec)   the frame is going away: release what the content holds
   *   mount(rec)     fill rec.bodyEl, called once the frame is laid out
   * @returns {Object|null} the window's record
   */
  function open(spec) {
    const s = spec || {};
    if (!s.key || typeof s.key !== "string") return null;
    if (!window.Corvus || !Corvus.ui || typeof document === "undefined") return null;
    const existing = windows.get(s.key);
    if (existing) return existing;

    ensureLayer();
    const rec = {
      key: s.key,
      kind: String(s.kind || ""),
      title: String(s.title || s.key),
      noun: String(s.noun || "window"),
      maximized: false,
      prevRect: null,
      onResize: s.onResize,
      onFocus: s.onFocus,
      onClose: s.onClose,
      onDragOut: s.onDragOut,
    };
    buildFrame(rec, s);
    wireGestures(rec);

    const view = viewport();
    const remembered = geometry.get(s.key);
    if (s.at) {
      // A window coming back into the app lands where it was let go of. The
      // position is in the page's pixels; the frame is placed in unscaled ones.
      const k = uiScale();
      rec.rect = clampRect({
        x: s.at.left / k, y: s.at.top / k, w: s.at.width / k, h: s.at.height / k,
      }, view);
    } else {
      rec.rect = remembered
        ? clampRect(remembered, view)
        : cascadeRect(opened, view, topInset(), s.size);
    }
    opened += 1;

    layerEl.appendChild(rec.el);
    windows.set(s.key, rec);
    applyRect(rec);
    Corvus.ui.refreshIcons();
    if (typeof s.mount === "function") s.mount(rec);

    // A narrow window has nowhere to put a 640px frame, so it starts filled.
    if (view.width < NARROW) toggleMax(rec);
    return rec;
  }

  /**
   * Put a window away. What its content does about that (the SSH session
   * stays up, the camera decoder stops once nobody asks for frames) is the
   * content's onClose.
   * @param {string} key
   * @returns {boolean} whether there was a window to close
   */
  function close(key) {
    const rec = windows.get(key);
    if (!rec) return false;
    windows.delete(key);
    if (typeof rec.onClose === "function") {
      try { rec.onClose(rec); } catch (_e) {}
    }
    if (rec.el.parentNode) rec.el.parentNode.removeChild(rec.el);
    return true;
  }

  /** @returns {Object|null} the open window for this key */
  function get(key) { return windows.get(key) || null; }

  /** @returns {boolean} whether a window for this key is open. */
  function has(key) { return windows.has(key); }

  /** @returns {string[]} the keys of every open window of one kind */
  function keys(kind) {
    const out = [];
    windows.forEach((rec, key) => { if (!kind || rec.kind === kind) out.push(key); });
    return out;
  }

  /** @returns {number} how many windows of one kind (or of any) are open. */
  function count(kind) { return keys(kind).length; }

  /**
   * Where a frame for `key` would open in the app, in the page's own pixels:
   * where it was last, or the next step of the cascade. A camera or terminal
   * that opens as a window of its own straight away opens there, so it comes
   * up where the operator is used to finding it.
   * @param {string} key
   * @param {{w: number, h: number}} [size]
   * @returns {{left, top, width, height}}
   */
  function placement(key, size) {
    ensureLayer();
    const view = viewport();
    const remembered = geometry.get(key);
    const r = remembered ? clampRect(remembered, view) : cascadeRect(opened, view, topInset(), size);
    if (!remembered) opened += 1;
    const k = uiScale();
    return { left: r.x * k, top: r.y * k, width: r.w * k, height: r.h * k };
  }

  /** Hide a frame whose content has gone on in a window outside the app. */
  function hideOut(rec) {
    if (rec && rec.el) rec.el.classList.add("is-out");
  }

  return {
    open, close, get, has, keys, count, raise, setStatus, toggleMax, hideOut, placement,
    clampRect, cascadeRect,
    MIN_W, MIN_H, DEF_W, DEF_H, MARGIN, BAR_H, KEEP_X, NARROW,
  };
})();
