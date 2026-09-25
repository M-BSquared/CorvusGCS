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
  tab underneath stays where it was. The frame is Corvus.floatWindows, the same
  one the camera windows use; the terminal inside it is the same
  Corvus.sshTerm over the same bridge, so everything that is true of the SSH
  tab (a real pty, Ctrl-C, full-screen programs) is true here.

  Two closes, deliberately different:

    ×           puts the window away. The session stays up and the program
                keeps running; opening it again re-attaches and the backend's
                replay redraws the scrollback that was there.
    disconnect  ends the session, which stops what it is running.
*/
Corvus.termWindows = (function () {
  const F = Corvus.floatWindows;
  const KIND = "terminal";

  function keyOf(name) { return "term:" + name; }

  function popouts() {
    return (window.Corvus && Corvus.popouts) || null;
  }

  /**
   * Take a terminal out of the app into a window of its own. The session is
   * not touched: the new window attaches its own stream to it, and the
   * backend's replay redraws the scrollback there.
   */
  function popOut(session, rec) {
    const po = popouts();
    if (!po) return false;
    const ok = po.open(outSpec(session), rec.el.getBoundingClientRect());
    if (ok) F.close(keyOf(session.name));
    return ok;
  }

  /** What a terminal's window out of the app is opened with. */
  function outSpec(session) {
    return {
      key: keyOf(session.name),
      kind: KIND,
      params: {
        name: session.name, title: session.title, host: session.host,
        port: session.port, username: session.username,
      },
    };
  }

  function addressOf(session) {
    const panel = window.Corvus && Corvus.panel;
    if (panel && typeof panel.sshAddress === "function") return panel.sshAddress(session);
    const host = String(session.host || "");
    if (!host) return "";
    const user = session.username ? session.username + "@" : "";
    return user + host + (session.port ? ":" + session.port : "");
  }

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
    F.setStatus(rec, "CONNECTED", "on");

    if (!Corvus.sshTerm) {
      rec.bodyEl.textContent =
        "Terminal component unavailable. The xterm bundle did not load.";
      return;
    }
    // xterm is fetched on first use (js/lazy.js), so the first terminal of
    // the session waits for ~300 KB. Say so rather than showing an empty
    // frame, and come back through this same function once it is here — the
    // isConnected check is the liveness test for a window the operator may
    // well have closed while it was loading.
    if (!Corvus.sshTerm.available()) {
      rec.bodyEl.textContent = "Loading terminal…";
      Corvus.sshTerm.ensure().then(() => {
        if (rec.bodyEl && rec.bodyEl.isConnected) attachTerminal(rec, session, quiet);
      }).catch(() => {
        if (!rec.bodyEl) return;
        rec.bodyEl.textContent =
          "Terminal component unavailable. The xterm bundle did not load.";
      });
      return;
    }
    rec.handle = Corvus.sshTerm.create(rec.bodyEl, session, {
      onClosed() {
        rec.closed = true;
        F.setStatus(rec, "OFFLINE", "");
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
   *        `at` — {left, top, width, height} in the page's pixels: where a
   *        window coming back into the app was let go of.
   * @returns {boolean} whether a window is now showing it
   */
  function open(session, opts) {
    const s = session || {};
    const o = opts || {};
    if (!s.name || typeof s.name !== "string") return false;
    if (!window.Corvus || !Corvus.ui || !F || typeof document === "undefined") return false;

    // A window of its own is told about a new shell whether or not the app
    // has heard from it lately: Chromium slows the timers of a window that
    // has been covered for a while, and its once-a-second "still here" can
    // lapse while it is very much still there. Unheard, it would sit on the
    // old shell's stream, OFFLINE, after the launcher started a new one.
    const po = popouts();
    if (po && o.reattach) po.reattach(keyOf(s.name));
    if (po && po.isOpen(keyOf(s.name))) {
      if (!o.existingOnly) po.focus(keyOf(s.name));
      return true;
    }

    const existing = F.get(keyOf(s.name));
    if (existing) {
      if (o.reattach || existing.closed) attachTerminal(existing, s, !!o.existingOnly);
      if (!o.existingOnly) {
        F.raise(existing);
        if (existing.handle) existing.handle.focus();
      }
      return true;
    }
    if (o.existingOnly) return false;

    // The desktop app's default: a window of its own at once, where the frame
    // would have opened. A window coming back into the app (`at`) is a frame.
    if (po && !o.at && po.opensOutside()
        && po.openNative(outSpec(s), F.placement(keyOf(s.name)))) {
      return true;
    }

    const title = String(s.title || s.name);
    const native = !!(po && po.native());
    const rec = F.open({
      key: keyOf(s.name),
      kind: KIND,
      at: o.at || null,
      title,
      subtitle: addressOf(s),
      icon: "square-terminal",
      noun: "terminal",
      ariaLabel: `Terminal: ${title}`,
      bodyClass: "ssh-term",
      status: "CONNECTED",
      statusTone: "on",
      closeTitle: "Close the window. What is running keeps running",
      onPopOut: po && (!native || !po.canPlace()) ? (r) => popOut(s, r) : null,
      onDragOut: po ? (r, size) => po.dragOut(outSpec(s), size, {
        hide: () => F.hideOut(r),
        gone: () => F.close(keyOf(s.name)),
      }) : null,
      tools: [Corvus.ui.iconButton("power", {
        size: 13,
        className: "icon-btn term-win-disconnect",
        title: "Disconnect. This stops what is running",
        ariaLabel: "Disconnect the session",
        onClick: () => disconnect(s.name),
      })],
      onResize: (r) => { if (r.handle) r.handle.fit(); },
      onFocus: (r) => { if (r.handle) r.handle.focus(); },
      onClose: (r) => {
        if (r.handle) { try { r.handle.dispose(); } catch (_e) {} }
        r.handle = null;
      },
      mount: (r) => {
        r.name = s.name;
        r.handle = null;
        attachTerminal(r, s);
      },
    });
    return !!rec;
  }

  /**
   * Put a window away. The session is left alone — whatever it is running goes
   * on running, and open() re-attaches to it with its scrollback intact.
   * @param {string} name
   * @returns {boolean} whether there was a window to close
   */
  function close(name) {
    return F.close(keyOf(name));
  }

  /** End the session, then close its window. This stops the remote program. */
  function disconnect(name) {
    fetch("/api/ssh/disconnect", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    }).catch(() => {}).then(() => close(name), () => close(name));
  }

  /** Every terminal window, gone. Sessions untouched — see close(). */
  function closeAll() {
    F.keys(KIND).forEach((key) => F.close(key));
  }

  // A popped-out terminal asking to come back into the app.
  if (popouts()) {
    popouts().onDock(KIND, (payload) => {
      if (payload && payload.session) open(payload.session, { at: payload.at || null });
    });
  }

  /** @returns {boolean} whether a window for this session is open. */
  function has(name) { return F.has(keyOf(name)); }

  /** @returns {number} how many terminal windows are open. */
  function count() { return F.count(KIND); }

  return {
    open, close, closeAll, has, count,
    clampRect: F.clampRect, cascadeRect: F.cascadeRect,
    MIN_W: F.MIN_W, MIN_H: F.MIN_H, DEF_W: F.DEF_W, DEF_H: F.DEF_H,
    MARGIN: F.MARGIN, BAR_H: F.BAR_H, KEEP_X: F.KEEP_X, NARROW: F.NARROW,
  };
})();
