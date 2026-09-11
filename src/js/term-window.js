"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.termWindows — free-floating SSH terminals, one per session.

  The panel's SSH tab is a single terminal: opening a session there replaces
  whatever was in it, and getting to it means leaving the tab you were on. That
  is the right shape for "connect to the companion computer and look around",
  and the wrong one for a plugin that starts four programs — the SSH Launcher's
  buttons each own a session, and sending the operator to one shared tab made
  four independent programs share one window and hid the shelf that started
  them.

  So a session opened from a plugin gets a window of its own: it floats over
  the app, it can be dragged and resized, several can be open at once, and the
  tab underneath stays where it was. The window is only a frame — the terminal
  inside it is the same Corvus.sshTerm over the same bridge, so everything that
  is true of the SSH tab (a real pty, Ctrl-C, full-screen programs) is true
  here.

  Two closes, deliberately different:

    ×           puts the window away. The session stays up and the program
                keeps running; opening it again re-attaches and the backend's
                replay redraws the scrollback that was there.
    disconnect  ends the session, which stops what it is running.

  Geometry is remembered per session for as long as the page lives, so a window
  put in a corner comes back to that corner.

  Everything is measured in UNSCALED pixels: <body> carries the interface-scale
  zoom, so getBoundingClientRect reports scaled pixels while style.left writes
  unscaled ones. Same correction as hud-panel.js, for the same reason.
*/
Corvus.termWindows = (function () {
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

  /** name -> the open window's record. One window per session, always. */
  const windows = new Map();
  /** name -> the rect it was last at, so a reopened window comes back home. */
  const geometry = new Map();

  let layerEl = null;
  let opened = 0;          // only feeds the cascade, never decremented
  let resizeWired = false;

  /**
   * The rect a window with no remembered geometry opens at: down the right of
   * the viewport, each one a step below and left of the last so a second
   * window never lands exactly on the first.
   *
   * `topInset` keeps the first one clear of the telemetry bar — a terminal
   * covering the altitude and battery readouts is the one place on this screen
   * a window must not open by itself.
   *
   * Pure, and exported for the test suite.
   *
   * @param {number} index how many windows have been opened before this one
   * @param {{width: number, height: number}} view unscaled viewport
   * @param {number} [topInset] chrome at the top of the screen to stay below
   * @returns {{x: number, y: number, w: number, h: number}}
   */
  function cascadeRect(index, view, topInset) {
    const top = Math.max(0, Number(topInset) || 0);
    const w = Math.min(DEF_W, Math.max(MIN_W, view.width - 2 * MARGIN));
    const h = Math.min(DEF_H, Math.max(MIN_H, view.height - top - 2 * MARGIN));
    const step = (index % 6) * CASCADE;
    return clampRect({
      x: view.width - w - MARGIN - step,
      y: top + MARGIN + step,
      w, h,
    }, view);
  }

  /**
   * Constrain a rect to a viewport it has to stay usable in: never larger than
   * the viewport, never smaller than a terminal can be read at, and never so
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

  /** Scaled px per unscaled px, so pointer deltas land where the cursor is. */
  function pointerScale(el) {
    if (!el || typeof el.getBoundingClientRect !== "function") return 1;
    const rect = el.getBoundingClientRect();
    return (rect.width && el.offsetWidth) ? (rect.width / el.offsetWidth) : 1;
  }

  /** Write a record's rect to its element and re-fit the terminal to it. */
  function applyRect(rec) {
    rec.el.style.left = rec.rect.x + "px";
    rec.el.style.top = rec.rect.y + "px";
    rec.el.style.width = rec.rect.w + "px";
    rec.el.style.height = rec.rect.h + "px";
    if (!rec.maximized) geometry.set(rec.name, Object.assign({}, rec.rect));
    if (rec.handle) rec.handle.fit();
  }

  /** Bring a window to the front. Stacking is DOM order — no z-index race. */
  function raise(rec) {
    if (rec.el.parentNode === layerEl && layerEl.lastChild !== rec.el) {
      layerEl.appendChild(rec.el);
    }
  }

  function addressOf(session) {
    const panel = window.Corvus && Corvus.panel;
    if (panel && typeof panel.sshAddress === "function") return panel.sshAddress(session);
    const host = String(session.host || "");
    if (!host) return "";
    const user = session.username ? session.username + "@" : "";
    return user + host + (session.port ? ":" + session.port : "");
  }

  // ---- the frame ---------------------------------------------------------

  function buildFrame(rec, session) {
    const ui = Corvus.ui;
    const el = document.createElement("div");
    el.className = "term-win";
    el.setAttribute("role", "dialog");
    el.setAttribute("aria-label", `Terminal — ${rec.title}`);

    const bar = document.createElement("div");
    bar.className = "term-win-bar";

    const icon = document.createElement("span");
    icon.className = "term-win-icon";
    icon.appendChild(ui.icon("square-terminal", 14));
    bar.appendChild(icon);

    const info = document.createElement("div");
    info.className = "term-win-info";
    const nameEl = document.createElement("span");
    nameEl.className = "term-win-name";
    nameEl.textContent = rec.title;
    const hostEl = document.createElement("span");
    hostEl.className = "term-win-host";
    hostEl.textContent = addressOf(session);
    info.appendChild(nameEl);
    info.appendChild(hostEl);
    bar.appendChild(info);

    const status = document.createElement("span");
    status.className = "ssh-status connected";
    const dot = document.createElement("span");
    dot.className = "dot";
    status.appendChild(dot);
    status.appendChild(document.createTextNode("CONNECTED"));
    bar.appendChild(status);
    rec.statusEl = status;

    const tools = document.createElement("div");
    tools.className = "term-win-tools";
    rec.maxBtn = ui.iconButton("maximize-2", {
      size: 13,
      title: "Maximize",
      ariaLabel: "Maximize the terminal",
      onClick: () => toggleMax(rec),
    });
    tools.appendChild(rec.maxBtn);
    tools.appendChild(ui.iconButton("power", {
      size: 13,
      className: "icon-btn term-win-disconnect",
      title: "Disconnect — this stops what is running",
      ariaLabel: "Disconnect the session",
      onClick: () => disconnect(rec),
    }));
    tools.appendChild(ui.iconButton("x", {
      size: 13,
      title: "Close the window — what is running keeps running",
      ariaLabel: "Close the terminal window",
      onClick: () => close(rec.name),
    }));
    bar.appendChild(tools);
    el.appendChild(bar);
    rec.barEl = bar;

    const body = document.createElement("div");
    body.className = "term-win-body ssh-term";
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

  // ---- moving and sizing -------------------------------------------------

  /**
   * One pointer gesture drives both: the title bar moves the window, the
   * corner grip sizes it. Pointer capture on the window itself, so a fast drag
   * over the map canvas or an xterm does not lose the pointer.
   */
  function wireGestures(rec) {
    let drag = null;

    rec.el.addEventListener("pointerdown", (e) => {
      raise(rec);
      if (e.button !== 0) return;
      const onGrip = rec.gripEl.contains(e.target);
      const onBar = rec.barEl.contains(e.target) && !e.target.closest(".icon-btn");
      if (!onGrip && !onBar) return;          // clicks in the terminal are the shell's
      if (rec.maximized && !onGrip) return;   // a maximized window has nowhere to go
      const k = pointerScale(rec.el);
      drag = {
        id: e.pointerId,
        mode: onGrip ? "size" : "move",
        px: e.clientX / k,
        py: e.clientY / k,
        rect: Object.assign({}, rec.rect),
      };
      if (drag.mode === "size" && rec.maximized) restore(rec, true);
      try { rec.el.setPointerCapture(e.pointerId); } catch (_e) {}
      rec.el.classList.add("is-dragging");
      e.preventDefault();
    });

    rec.el.addEventListener("pointermove", (e) => {
      if (!drag || e.pointerId !== drag.id) return;
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
      drag = null;
      rec.el.classList.remove("is-dragging");
      if (rec.handle) rec.handle.fit();
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
    rec.maxBtn.setAttribute("aria-label",
      rec.maximized ? "Restore the terminal" : "Maximize the terminal");
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
    if (rec.handle) rec.handle.focus();
  }

  /** Back to the rect the window had before it was maximized. */
  function restore(rec, keepFocus) {
    if (!rec.maximized) return;
    rec.maximized = false;
    rec.el.classList.remove("is-max");
    rec.rect = clampRect(rec.prevRect || rec.rect, viewport());
    applyRect(rec);
    syncMaxButton(rec);
    if (!keepFocus && rec.handle) rec.handle.focus();
  }

  /** A smaller window must not leave its terminals stranded off screen. */
  function onViewportResize() {
    const view = viewport();
    windows.forEach((rec) => {
      rec.rect = rec.maximized ? fullRect(view) : clampRect(rec.rect, view);
      applyRect(rec);
    });
  }

  // ---- open / close ------------------------------------------------------

  /**
   * Attach (or re-attach) the live terminal inside a window's frame.
   *
   * Re-attaching matters because a session can be REPLACED under its name: the
   * launcher's restart connects a second shell as the same session, and the
   * old stream — subscribed to the old session object, while the name still
   * resolves to a connected one — goes quiet without ever being told it ended.
   * A window left on that stream shows a terminal that will never print again.
   * Tearing it down and opening a fresh stream gets the new shell, with the
   * backend's replay redrawing what it has already printed.
   */
  function attachTerminal(rec, session, quiet) {
    if (rec.handle) { try { rec.handle.dispose(); } catch (_e) {} }
    rec.handle = null;
    rec.closed = false;
    Corvus.ui.clear(rec.bodyEl);
    rec.statusEl.classList.add("connected");
    rec.statusEl.lastChild.textContent = "CONNECTED";

    if (!Corvus.sshTerm || !Corvus.sshTerm.available()) {
      rec.bodyEl.textContent =
        "Terminal component unavailable — the xterm bundle did not load.";
      return;
    }
    rec.handle = Corvus.sshTerm.create(rec.bodyEl, session, {
      onClosed() {
        rec.closed = true;
        rec.statusEl.classList.remove("connected");
        rec.statusEl.lastChild.textContent = "OFFLINE";
      },
    });
    // The frame is only measurable once it is laid out; without this the pty
    // keeps the 80x24 the channel was opened with.
    rec.handle.fit();
    // `quiet` is a window being repaired in the background — the operator is
    // pressing a launch button, not asking to type in here.
    if (!quiet) rec.handle.focus();
  }

  /**
   * Open (or raise) the floating terminal for one already-connected session.
   *
   * Does NOT connect: the caller owns the session, exactly as with the SSH
   * tab's showSSHTerminal.
   *
   * @param {Object} session {name, title, host, port, username}
   * @param {Object} [opts] {reattach, existingOnly}
   *        `reattach` — true when the caller has just (re)connected this
   *        session, so an open window drops its old stream and takes the new
   *        shell instead of showing a dead one.
   *        `existingOnly` — repair a window that is already open and open
   *        none. A launch is not a request to be shown a terminal; the arrow
   *        beside the button is. Returns false when there is no window.
   * @returns {boolean} whether a window is now showing it
   */
  function open(session, opts) {
    const s = session || {};
    const o = opts || {};
    if (!s.name || typeof s.name !== "string") return false;
    if (!window.Corvus || !Corvus.ui || typeof document === "undefined") return false;

    const existing = windows.get(s.name);
    if (existing) {
      if (o.reattach || existing.closed) attachTerminal(existing, s, !!o.existingOnly);
      if (!o.existingOnly) {
        raise(existing);
        if (existing.handle) existing.handle.focus();
      }
      return true;
    }
    if (o.existingOnly) return false;

    ensureLayer();
    const rec = {
      name: s.name,
      title: String(s.title || s.name),
      maximized: false,
      prevRect: null,
      handle: null,
    };
    buildFrame(rec, s);
    wireGestures(rec);

    const view = viewport();
    const remembered = geometry.get(s.name);
    rec.rect = remembered
      ? clampRect(remembered, view)
      : cascadeRect(opened, view, topInset());
    opened += 1;

    layerEl.appendChild(rec.el);
    windows.set(s.name, rec);
    applyRect(rec);
    Corvus.ui.refreshIcons();
    attachTerminal(rec, s);

    // A narrow window has nowhere to put a 640px frame, so it starts filled.
    if (view.width < NARROW) toggleMax(rec);
    return true;
  }

  /**
   * Put a window away. The session is left alone — whatever it is running goes
   * on running, and open() re-attaches to it with its scrollback intact.
   * @param {string} name
   * @returns {boolean} whether there was a window to close
   */
  function close(name) {
    const rec = windows.get(name);
    if (!rec) return false;
    windows.delete(name);
    if (rec.handle) { try { rec.handle.dispose(); } catch (_e) {} }
    if (rec.el.parentNode) rec.el.parentNode.removeChild(rec.el);
    return true;
  }

  /** End the session, then close its window. This stops the remote program. */
  function disconnect(rec) {
    const name = rec.name;
    fetch("/api/ssh/disconnect", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    }).catch(() => {}).then(() => close(name), () => close(name));
  }

  /** Every window, gone. Sessions untouched — see close(). */
  function closeAll() {
    Array.from(windows.keys()).forEach(close);
  }

  /** @returns {boolean} whether a window for this session is open. */
  function has(name) { return windows.has(name); }

  /** @returns {number} how many terminal windows are open. */
  function count() { return windows.size; }

  return {
    open, close, closeAll, has, count,
    clampRect, cascadeRect,
    MIN_W, MIN_H, DEF_W, DEF_H, MARGIN, BAR_H, KEEP_X, NARROW,
  };
})();
