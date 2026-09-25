"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.popouts — floating windows taken out of the app into windows of their
  own.

  A Corvus.floatWindows frame lives inside the page, so it can go anywhere in
  the app's window and nowhere else. That is right for a camera over the map
  and wrong for one on a second monitor. So:

    desktop app  By default a camera or terminal opens straight away in a
                 native window of its own (corvus/app.py, MainBridge):
                 frameless, drawing the very same frame, and free to go
                 anywhere. With Settings, "Open inside the Corvus window" on
                 (setInApp), it opens as a frame in the app instead, and
                 dragging it past the edge carries it on outside as that
                 native window; let go of it over the app again and it is a
                 frame in the app again.
    browser      A browser has no frameless windows: the frame always opens
                 in the app, and gets a button that opens its content in a
                 pop-up (window.open).

  The popped-out window is its own page (popout.html) with its own stream: a
  camera fetches its own frames or opens its own WebRTC connection, a terminal
  attaches its own SSE stream to the same SSH session. Nothing is moved between
  documents, so nothing can be left half in one and half in the other.

  The two sides talk over a BroadcastChannel, which every page of this origin
  in the same browser profile can use, and which survives the main page being
  reloaded:

    state   popout -> app, once a second: "I am open, and this is my state".
            The heartbeat is also how the app knows a pop-out is still there
            after a reload, when it has no window handle any more.
    closed  popout -> app, as it goes.
    dock    popout -> app: "put me back"; the app opens the frame again.
            Only a window that came out of the app offers it (the "dock"
            parameter of its address); one that opened on its own stays one.
    close   app -> popout: close yourself (the map's camera button).
    focus   app -> popout: come to the front.
    reattach app -> popout: the SSH session behind you was replaced.
*/
Corvus.popouts = (function () {
  const CHANNEL = "corvus-popouts";
  const HEARTBEAT_MS = 1000;
  // A pop-out that has not been heard from in this long is gone. Generous,
  // because a page that is loading has not started its heartbeat yet.
  const STALE_MS = 5000;
  const SWEEP_MS = 2000;

  /** key -> {kind, lastSeen, state, win} */
  const known = new Map();
  const listeners = new Set();
  const dockHandlers = new Map();
  const pageHandlers = new Map();
  let channel = null;
  let sweepTimer = null;
  // Settings, "Open inside the Corvus window". Off by default.
  let inApp = false;

  function now() { return Date.now(); }

  function chan() {
    if (channel || typeof window.BroadcastChannel !== "function") return channel;
    channel = new window.BroadcastChannel(CHANNEL);
    // Node has BroadcastChannel too (the test suite); there it must not keep
    // the process alive.
    if (typeof channel.unref === "function") channel.unref();
    channel.onmessage = (e) => receive(e && e.data);
    return channel;
  }

  function post(msg) {
    const c = chan();
    if (!c) return;
    try { c.postMessage(msg); } catch (_e) {}
  }

  function emit() {
    listeners.forEach((fn) => { try { fn(); } catch (_e) {} });
  }

  function receive(msg) {
    if (!msg || typeof msg !== "object" || typeof msg.key !== "string") return;
    // The page side: messages addressed to the pop-out this page is.
    const own = pageHandlers.get(msg.key);
    if (own && (msg.type === "close" || msg.type === "focus" || msg.type === "reattach")) {
      own(msg);
      return;
    }
    // The app side.
    if (msg.type === "state") {
      const waiter = shownWaiters.get(msg.key);
      if (waiter) {
        shownWaiters.delete(msg.key);
        try { waiter(); } catch (_e) {}
      }
      const had = known.has(msg.key);
      const entry = known.get(msg.key) || { kind: msg.kind, win: null };
      entry.kind = msg.kind || entry.kind;
      entry.lastSeen = now();
      entry.state = msg.state || null;
      known.set(msg.key, entry);
      if (!had) { emit(); startSweep(); }
    } else if (msg.type === "closed") {
      if (known.delete(msg.key)) emit();
    } else if (msg.type === "dock") {
      known.delete(msg.key);
      const fn = dockHandlers.get(msg.kind);
      if (typeof fn === "function") {
        try { fn(msg.payload || {}); } catch (_e) {}
      }
      emit();
    }
  }

  /** Drop pop-outs that stopped answering, and tell the listeners. */
  function sweep() {
    let changed = false;
    known.forEach((entry, key) => {
      const closed = entry.win && entry.win.closed === true;
      if (closed || now() - entry.lastSeen > STALE_MS) {
        known.delete(key);
        changed = true;
      }
    });
    if (changed) emit();
    if (!known.size && sweepTimer) { clearInterval(sweepTimer); sweepTimer = null; }
  }

  function startSweep() {
    if (!sweepTimer) sweepTimer = setInterval(sweep, SWEEP_MS);
  }

  /** The popout.html query for one frame. */
  function query(spec) {
    const params = new URLSearchParams();
    Object.keys(spec.params || {}).forEach((k) => {
      const v = spec.params[k];
      if (v !== undefined && v !== null && v !== "") params.set(k, String(v));
    });
    params.set("key", spec.key);
    params.set("kind", spec.kind);
    return params.toString();
  }

  /** The popout.html address for one frame. Exported for the tests. */
  function url(spec) {
    return "popout.html?" + query(spec);
  }

  // The keys the desktop app accepts for a native window (corvus/app.py,
  // _POPOUT_KEY_RE). A key it would refuse is kept in the app instead of
  // being sent off to a window that never opens.
  const KEY_RE = /^(video|term):[A-Za-z0-9_./@:-]{1,96}$/;

  /** Whether `key` can name a native window. */
  function keyOk(key) { return typeof key === "string" && KEY_RE.test(key); }

  /** The same frame, marked as one that may go back into the app. */
  function dockable(spec) {
    return Object.assign({}, spec, { params: Object.assign({}, spec.params, { dock: "1" }) });
  }

  // ---- the desktop app's native windows ---------------------------------------

  /** The desktop app's window bridge (see corvus/app.py), or null in a browser. */
  function native() {
    return (typeof window !== "undefined" && window.corvusNative) || null;
  }

  /** Settings, "Open inside the Corvus window". */
  function setInApp(on) { inApp = !!on; }

  /**
   * Whether this app may place its windows itself and knows where the pointer
   * is (corvus/app.py, window_support). Not on Wayland: there a frame cannot
   * follow the pointer out of the app, so it stays in and offers the button
   * that opens its window instead.
   */
  function canPlace() {
    const s = typeof window !== "undefined" ? window.corvusNativeSupport : null;
    return !(s && s.place === false);
  }

  /** Whether a camera or terminal opens as a window of its own straight away. */
  function opensOutside() {
    const n = native();
    return !inApp && !!(n && typeof n.openWindow === "function");
  }

  /**
   * Open a frame's content straight away in a native window of its own.
   * `rect` is where the frame would have opened in the app, in the page's
   * pixels; the desktop app puts the window there, or where this key's window
   * was last closed.
   * @returns {boolean} whether it was asked for
   */
  function openNative(spec, rect) {
    if (!spec || !keyOk(spec.key) || !rect) return false;
    if (!call("openWindow", spec.key, query(spec), Math.round(rect.left), Math.round(rect.top),
      Math.round(rect.width), Math.round(rect.height))) return false;
    known.set(spec.key, { kind: spec.kind, lastSeen: now(), state: null, win: null });
    startSweep();
    emit();
    return true;
  }

  function call(name, ...args) {
    const n = native();
    if (!n || typeof n[name] !== "function") return false;
    try { n[name](...args); return true; } catch (_e) { return false; }
  }

  /**
   * Hand a frame's drag to a native window once the pointer has left the app.
   *
   * Returns the controller Corvus.floatWindows drives for the rest of the
   * drag, or null when there is no native window to go to (a browser):
   *
   *   move(left, top)  where the frame's top left is now, in the page's own
   *                    pixels (the pointer's clientX/clientY less the grab)
   *   end(inside)      the pointer was let go; `inside` is whether that was
   *                    over the app, in which case the frame stays in the app
   *
   * @param {Object} spec {key, kind, params}, as for open()
   * @param {{width, height}} size the frame's size in the page's pixels
   * @param {Object} hooks {hide()} the frame, once its window is showing;
   *        {gone()} the frame, once the drag ended outside the app
   */
  function dragOut(frameSpec, size, hooks) {
    if (!native() || !canPlace() || !frameSpec || !keyOk(frameSpec.key)) return null;
    const spec = dockable(frameSpec);
    const h = hooks || {};
    let started = false;
    let done = false;
    return {
      move(left, top) {
        const l = Math.round(left);
        const t = Math.round(top);
        if (started) {
          call("moveWindow", spec.key, l, t);
          return;
        }
        started = true;
        call("detachWindow", spec.key, query(spec), l, t,
          Math.round(size.width), Math.round(size.height));
        known.set(spec.key, { kind: spec.kind, lastSeen: now(), state: null, win: null });
        startSweep();
        // The frame stays up until its window has drawn, so the picture is
        // never simply gone halfway across the edge.
        whenShown(spec.key, () => { if (!done && typeof h.hide === "function") h.hide(); });
        emit();
      },
      end(inside) {
        done = true;
        if (!started) return;
        if (inside) {
          call("closeWindow", spec.key);
          known.delete(spec.key);
          emit();
        } else {
          call("settleWindow", spec.key);
          if (typeof h.gone === "function") h.gone();
        }
      },
    };
  }

  const shownWaiters = new Map();

  /** Call `fn` once the window for `key` has sent its first heartbeat. */
  function whenShown(key, fn) {
    shownWaiters.set(key, fn);
  }

  /**
   * window.open features that put the new window where the frame was, in
   * screen coordinates. Exported for the tests.
   * @param {{left, top, width, height}} rect the frame, in viewport pixels
   * @param {Object} win the window the frame is in (screenX, outer/inner sizes)
   */
  function features(rect, win) {
    const r = rect || {};
    const w = win || {};
    // The page's own origin on the screen: the window's, plus its chrome.
    const chromeX = Math.max(0, (Number(w.outerWidth) || 0) - (Number(w.innerWidth) || 0));
    const chromeY = Math.max(0, (Number(w.outerHeight) || 0) - (Number(w.innerHeight) || 0));
    const left = Math.round((Number(w.screenX) || 0) + chromeX / 2 + (Number(r.left) || 0));
    const top = Math.round((Number(w.screenY) || 0) + chromeY + (Number(r.top) || 0));
    const width = Math.max(320, Math.round(Number(r.width) || 640));
    const height = Math.max(200, Math.round(Number(r.height) || 400));
    return `popup=yes,width=${width},height=${height},left=${left},top=${top}`;
  }

  // ---- the app side ----------------------------------------------------------

  /**
   * Open a frame's content in a window of its own.
   * @param {Object} spec {key, kind, params}
   * @param {Object} rect where the frame is, so the window opens on top of it
   * @returns {boolean} whether a window was opened
   */
  function open(spec, rect) {
    if (!spec || !spec.key) return false;
    chan();
    // The desktop app: its own frameless window, the same as opening outside.
    const n = native();
    if (n && typeof n.openWindow === "function") return openNative(dockable(spec), rect);
    if (typeof window.open !== "function") return false;
    let win = null;
    try {
      win = window.open(url(dockable(spec)), "corvus-" + spec.key.replace(/[^A-Za-z0-9_-]/g, "-"),
        features(rect, window));
    } catch (_e) {
      win = null;
    }
    if (!win) return false;
    known.set(spec.key, { kind: spec.kind, lastSeen: now(), state: null, win });
    startSweep();
    emit();
    return true;
  }

  /** @returns {boolean} whether this frame is out in a window of its own */
  function isOpen(key) {
    const entry = known.get(key);
    if (!entry) return false;
    if ((entry.win && entry.win.closed === true) || now() - entry.lastSeen > STALE_MS) {
      known.delete(key);
      return false;
    }
    return true;
  }

  /** Bring a popped-out window to the front. */
  function focus(key) {
    const entry = known.get(key);
    if (entry && entry.win) { try { entry.win.focus(); } catch (_e) {} }
    post({ type: "focus", key });
  }

  /** Close one popped-out window. The native one directly as well, in case
   *  its page has not loaded far enough to hear the message. */
  function close(key) {
    post({ type: "close", key });
    call("closeWindow", key);
    if (known.delete(key)) emit();
  }

  /** Close every popped-out window of one kind ("video", "terminal"). */
  function closeKind(kind) {
    keys(kind).forEach((key) => close(key));
  }

  /** Tell a popped-out terminal that the session behind it is a new shell. */
  function reattach(key) {
    post({ type: "reattach", key });
  }

  function keys(kind) {
    const out = [];
    Array.from(known.keys()).forEach((key) => {
      if (isOpen(key) && (!kind || known.get(key).kind === kind)) out.push(key);
    });
    return out;
  }

  function count(kind) { return keys(kind).length; }

  /** The last state a popped-out window reported, or null. */
  function stateOf(key) {
    return isOpen(key) ? (known.get(key).state || null) : null;
  }

  /** What to do when a popped-out window of `kind` asks to be put back. */
  function onDock(kind, fn) { dockHandlers.set(kind, fn); chan(); }

  /** Called whenever the set of popped-out windows changes. */
  function onChange(fn) {
    if (typeof fn !== "function") return () => {};
    listeners.add(fn);
    chan();
    return () => listeners.delete(fn);
  }

  // ---- the pop-out side --------------------------------------------------------

  /**
   * Run in popout.html: announce this window, keep announcing it, and do what
   * the app asks. Returns {dock(payload), stop()}.
   * @param {string} key
   * @param {string} kind
   * @param {Object} hooks {state() -> Object, onClose(), onFocus(), onReattach()}
   */
  function announce(key, kind, hooks) {
    const h = hooks || {};
    chan();
    pageHandlers.set(key, (msg) => {
      if (msg.type === "close" && typeof h.onClose === "function") h.onClose();
      if (msg.type === "focus" && typeof h.onFocus === "function") h.onFocus();
      if (msg.type === "reattach" && typeof h.onReattach === "function") h.onReattach();
    });
    const beat = () => post({
      type: "state", key, kind, state: typeof h.state === "function" ? h.state() : null,
    });
    beat();
    const timer = setInterval(beat, HEARTBEAT_MS);
    let done = false;
    function stop() {
      if (done) return;
      done = true;
      clearInterval(timer);
      pageHandlers.delete(key);
      post({ type: "closed", key, kind });
    }
    return {
      stop,
      dock(payload) {
        if (done) return;
        done = true;
        clearInterval(timer);
        pageHandlers.delete(key);
        post({ type: "dock", key, kind, payload });
      },
    };
  }

  return {
    open, isOpen, focus, close, closeKind, reattach, keys, count, stateOf,
    onDock, onChange, announce, url, features, native, dragOut,
    setInApp, opensOutside, openNative, keyOk, canPlace,
    STALE_MS, HEARTBEAT_MS,
  };
})();
