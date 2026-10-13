"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.popoutPage — what runs in popout.html: one camera or one terminal,
  filling a window of its own.

  The content is built by the same code as in the app's floating frames
  (Corvus.videoWindows.attach, Corvus.sshTerm.create), under the same title
  bar, so a window out of the app is the frame it came from with the edges of
  the app's window taken away.

  In the desktop app the window has the system's title bar and buttons by
  default, and this bar keeps the rest (see goNative). With Settings, "Corvus
  title bar", on, the window is frameless (corvus/app.py) and this page is all
  of it: the bar moves it, the corner grip sizes it, the bar's own buttons
  maximize and close it, exactly as in the app, and one more that a frame
  inside the app does not need: the pin, which keeps the window above the
  Corvus window. Only above Corvus: with another program in front, its windows
  can cover this one as they would any other (corvus/app.py, stays_on_top). A window that came OUT of the
  app (its address says "dock=1") can go back: its bar has the button for it,
  and letting go of it over the app puts it back in as a frame, where it was
  let go of. One that opened as a window of its own stays one. In a browser
  the pop-up has the browser's own frame, and the bar offers the way back.

  Everything the page needs comes from its address (see Corvus.popouts.url):
  a camera's id, name, address and kind, a terminal's session, or the plugin
  whose window it is (kind "plugin:<id>", see pluginWindow). No
  credential is in it; the backend holds those, as it does for the frames.
*/
Corvus.popoutPage = (function () {
  const F = Corvus.floatWindows;

  /** The app's theme, followed when it changes in the main window. */
  function followTheme() {
    window.addEventListener("storage", (e) => {
      if (e.key !== "corvus.theme") return;
      if (e.newValue) document.documentElement.setAttribute("data-theme", e.newValue);
      else document.documentElement.removeAttribute("data-theme");
      window.dispatchEvent(new CustomEvent("corvus:themechange", { detail: { theme: e.newValue } }));
    });
  }

  /**
   * The bar and the body, with the frame's own classes, filling the window.
   * Returns a record Corvus.floatWindows.setStatus can write the pill of.
   */
  function frame(root, opts) {
    const ui = Corvus.ui;
    const el = document.createElement("div");
    el.className = "term-win popout-win" + (opts.className ? " " + opts.className : "");
    const bar = document.createElement("div");
    bar.className = "term-win-bar";
    const icon = document.createElement("span");
    icon.className = "term-win-icon";
    icon.appendChild(ui.icon(opts.icon, 14));
    bar.appendChild(icon);
    const info = document.createElement("div");
    info.className = "term-win-info";
    const nameEl = document.createElement("span");
    nameEl.className = "term-win-name";
    nameEl.textContent = opts.title;
    const hostEl = document.createElement("span");
    hostEl.className = "term-win-host";
    hostEl.textContent = opts.subtitle || "";
    info.appendChild(nameEl);
    info.appendChild(hostEl);
    bar.appendChild(info);
    const status = document.createElement("span");
    status.className = "ssh-status";
    const dot = document.createElement("span");
    dot.className = "dot";
    status.appendChild(dot);
    status.appendChild(document.createTextNode(""));
    bar.appendChild(status);
    const tools = document.createElement("div");
    tools.className = "term-win-tools";
    (opts.tools || []).forEach((t) => tools.appendChild(t));
    bar.appendChild(tools);
    const body = document.createElement("div");
    body.className = "term-win-body" + (opts.bodyClass ? " " + opts.bodyClass : "");
    el.appendChild(bar);
    el.appendChild(body);
    root.appendChild(el);
    document.title = `${opts.title} (Corvus GCS)`;
    const rec = { el, barEl: bar, toolsEl: tools, bodyEl: body, statusEl: status };
    ui.refreshIcons();
    return rec;
  }

  /** The desktop app's bridge to this window, or null in a browser. */
  function bridge() {
    return (window.corvusNative && typeof window.corvusNative.dragStart === "function")
      ? window.corvusNative : null;
  }

  /** Run `fn` with the bridge once it is connected, if it ever is. */
  function whenNative(fn) {
    if (bridge()) { fn(bridge()); return; }
    window.addEventListener("corvus:native-ready", () => { if (bridge()) fn(bridge()); }, { once: true });
  }

  /**
   * Make the page the whole window: the bar moves it, the grip sizes it, the
   * bar's buttons pin, maximize and close it. The moves are the pointer's own
   * screen deltas, handed to corvus/app.py, which moves the native window.
   *
   * @param {Object} rec the frame
   * @param {Object} n the bridge
   * @param {Object} opts {noun, onClose(), dockBtn, onDrop({left, top, width, height}),
   *        onMaximize(isMax)}
   *        `dockBtn` and `onDrop` only for a window that may go back in
   */
  function goNative(rec, n, opts) {
    const ui = Corvus.ui;
    if (document.documentElement) document.documentElement.classList.add("native-window");
    let maximized = false;
    // What this platform lets the window do (corvus/app.py, window_support).
    // On Wayland the compositor moves and sizes it, and there is no pin.
    const support = window.corvusNativeSupport || {};
    const place = support.place !== false;
    const canPin = support.pin !== false && typeof n.setPinned === "function";

    const maxBtn = ui.iconButton("maximize-2", {
      size: 13,
      title: "Maximize",
      ariaLabel: `Maximize the ${opts.noun}`,
      onClick: () => toggleMax(),
    });
    const closeBtn = ui.iconButton("x", {
      size: 13,
      title: opts.closeTitle || "Close the window",
      ariaLabel: `Close the ${opts.noun} window`,
      onClick: () => opts.onClose(),
    });
    // Kept above the Corvus window: a camera beside the map would otherwise go
    // behind it at the first click there.
    let pinned = false;
    const pinBtn = canPin ? ui.iconButton("pin", {
      size: 13,
      ariaLabel: `Keep the ${opts.noun} above the Corvus window`,
      onClick: () => n.setPinned(!pinned, paintPin),
    }) : null;
    function paintPin(on) {
      pinned = !!on;
      if (!pinBtn) return;
      pinBtn.classList.toggle("active", pinned);
      pinBtn.setAttribute("aria-pressed", pinned ? "true" : "false");
      pinBtn.title = pinned
        ? "Kept above the Corvus window. Click to stop"
        : "Keep above the Corvus window. Other programs can still cover it";
    }
    paintPin(false);
    if (pinBtn && typeof n.isPinned === "function") n.isPinned(paintPin);

    // The app's order: the way back into it first (when there is one), then
    // the pin, maximize, the content's own buttons, and × last.
    const kids = Array.from(rec.toolsEl.children);
    const before = kids[opts.dockBtn ? kids.indexOf(opts.dockBtn) + 1 : 0];
    [pinBtn, maxBtn].forEach((btn) => {
      if (!btn) return;
      if (before && typeof rec.toolsEl.insertBefore === "function") rec.toolsEl.insertBefore(btn, before);
      else rec.toolsEl.appendChild(btn);
    });
    rec.toolsEl.appendChild(closeBtn);
    const grip = document.createElement("div");
    grip.className = "term-win-grip";
    grip.setAttribute("aria-hidden", "true");
    rec.el.appendChild(grip);
    // What the system's own frame does instead, when Settings asks for it.
    [maxBtn, closeBtn, grip].forEach((el) => el.classList.add("popout-own-frame"));
    ui.refreshIcons();

    // The default, unless Settings, "Corvus title bar", is on (corvus/app.py,
    // popout_frame): the system's frame closes, minimizes, maximizes and sizes the window, and
    // this bar keeps the name, the status, the pin and the power button. On
    // macOS the page runs on under the system's bar, and this one matches its
    // height and keeps clear of the traffic lights.
    let frame = { native: false };
    function setFrame(f) {
      frame = (f && typeof f === "object") ? f : { native: false };
      const root = document.documentElement;
      const overlay = !!(frame.native && frame.overlay);
      const k = ui.uiScale();
      if (root) {
        root.classList.toggle("system-frame", !!frame.native);
        root.classList.toggle("system-frame-overlay", overlay);
        if (root.style && typeof root.style.setProperty === "function") {
          root.style.setProperty("--system-bar-h", overlay ? (Number(frame.top) || 28) / k + "px" : "");
          root.style.setProperty("--system-controls-w", overlay ? (Number(frame.inset) || 0) / k + "px" : "");
        }
      }
      if (frame.native && maximized) paintMax(false);
      if (typeof opts.onFrame === "function") opts.onFrame(frame);
    }
    function readFrame() {
      if (typeof n.frame !== "function") return;
      n.frame((answer) => {
        let f = null;
        try { f = JSON.parse(answer); } catch (_e) { f = null; }
        setFrame(f);
      });
    }
    window.addEventListener("corvus:native-frame", readFrame);
    readFrame();

    function paintMax(isMax) {
      maximized = !!isMax;
      rec.el.classList.toggle("is-max", maximized);
      maxBtn.title = maximized ? "Restore" : "Maximize";
      maxBtn.setAttribute("aria-label", `${maximized ? "Restore" : "Maximize"} the ${opts.noun}`);
      ui.clear(maxBtn).appendChild(ui.icon(maximized ? "minimize-2" : "maximize-2", 13));
      ui.refreshIcons();
    }

    function toggleMax() {
      n.toggleMaximize((isMax) => {
        // In the system's frame its own button can undo a maximize without
        // this page hearing of it, so the page keeps no state that would
        // then hold the bar still.
        if (frame.native) {
          if (typeof opts.onMaximize === "function") opts.onMaximize(!!isMax);
          return;
        }
        paintMax(isMax);
        if (typeof opts.onMaximize === "function") opts.onMaximize(maximized);
      });
    }

    let drag = null;
    rec.el.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      const onGrip = grip.contains(e.target);
      const onBar = rec.barEl.contains(e.target) && !e.target.closest(".icon-btn");
      if (!onGrip && !onBar) return;           // presses in the body are the content's
      if (maximized && !onGrip) return;        // a maximized window has nowhere to go
      if (!place) {
        // The compositor takes the pointer from here; there is no drop to
        // watch for, and no going back into the app by letting go over it.
        const hand = onGrip ? n.systemResize : n.systemMove;
        if (typeof hand === "function") hand.call(n);
        e.preventDefault();
        return;
      }
      drag = {
        id: e.pointerId, mode: onGrip ? "size" : "move",
        sx: e.screenX, sy: e.screenY, lx: e.clientX, ly: e.clientY, moved: false,
      };
      n.dragStart();
      try { rec.el.setPointerCapture(e.pointerId); } catch (_e) {}
      rec.el.classList.add("is-dragging");
      e.preventDefault();
    });
    rec.el.addEventListener("pointermove", (e) => {
      if (!drag || e.pointerId !== drag.id) return;
      const dx = Math.round(e.screenX - drag.sx);
      const dy = Math.round(e.screenY - drag.sy);
      if (!dx && !dy && !drag.moved) return;
      drag.moved = true;
      drag.lx = e.clientX;
      drag.ly = e.clientY;
      if (drag.mode === "move") n.dragTo(dx, dy);
      else n.resizeTo(dx, dy);
    });
    const end = (e) => {
      if (!drag || (e && e.pointerId !== drag.id)) return;
      const was = drag;
      drag = null;
      try { rec.el.releasePointerCapture(was.id); } catch (_e) {}
      rec.el.classList.remove("is-dragging");
      const x = Math.round(e && e.type === "pointerup" ? e.clientX : was.lx);
      const y = Math.round(e && e.type === "pointerup" ? e.clientY : was.ly);
      n.dragEnd(x, y, (answer) => {
        if (was.mode !== "move" || !was.moved || typeof opts.onDrop !== "function") return;
        let d = null;
        try { d = JSON.parse(answer); } catch (_e) { d = null; }
        // Let go of over the app: back in, as a frame, right there.
        if (d && d.inside) opts.onDrop({ left: d.left, top: d.top, width: d.width, height: d.height });
      });
    };
    rec.el.addEventListener("pointerup", end);
    rec.el.addEventListener("pointercancel", end);
    rec.barEl.addEventListener("dblclick", (e) => {
      if (e.target.closest(".icon-btn")) return;
      toggleMax();
    });
    return { toggleMax, pinBtn, setFrame, frame: () => frame };
  }

  // ---- frosted glass ------------------------------------------------------------

  /** Where the app keeps whether terminals are frosted (js/term-window.js):
   *  "0" when Settings, "Solid terminals", is on. */
  const FROSTED_KEY = "corvus.frostedTerminals";

  /** Frosted unless the app said solid: frosted glass is the default. */
  function frostedWanted() {
    try {
      return !window.localStorage || window.localStorage.getItem(FROSTED_KEY) !== "0";
    } catch (_e) {
      return true;
    }
  }

  function reducedTransparency() {
    try {
      return typeof window.matchMedia === "function"
        && window.matchMedia("(prefers-reduced-transparency: reduce)").matches;
    } catch (_e) {
      return false;
    }
  }

  /** Whether the theme's glass is dark, for the blur's own light or dark tint. */
  function darkGlass() {
    const rgb = String(Corvus.ui.token("--glass-rgb", "255 255 255")).split(/[\s,]+/).map(Number);
    if (rgb.length < 3 || rgb.some((v) => !isFinite(v))) return false;
    return 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2] < 128;
  }

  /** The frame's corner radius in the window's own points (the interface
   *  scale is a zoom on <body>), so the blur has the frame's corners. */
  function cornerRadius(rec) {
    let px = 0;
    try { px = parseFloat(getComputedStyle(rec.el).borderTopLeftRadius) || 0; } catch (_e) { px = 0; }
    return px * Corvus.ui.uiScale();
  }

  /**
   * Frost a terminal's window of its own, unless Settings asks for solid ones.
   *
   * The page cannot do it alone: backdrop-filter only sees the page, and
   * behind this page is the desktop. So the desktop app puts the operating
   * system's blur behind the window (corvus/app.py, PopoutBridge.setFrosted),
   * and the page turns see-through only once it has said the blur is there.
   * Where there is none, the terminal stays solid: text over a sharp desktop
   * is not readable.
   *
   * Followed live: the switch, the theme (light or dark glass) and maximize
   * (square corners at the edges of the screen).
   *
   * @returns {{refresh: Function}|null}
   */
  function followFrosted(rec, n) {
    const root = document.documentElement;
    if (typeof n.setFrosted !== "function" || !root) return null;
    let asked = 0;
    function refresh() {
      const want = frostedWanted() && !reducedTransparency();
      const ticket = ++asked;
      // Solid before the blur goes, see-through only after it came: either
      // way round, the desktop never shows through sharp.
      if (!want) root.classList.remove("frosted-terminals");
      n.setFrosted(want, darkGlass(), cornerRadius(rec), (ok) => {
        if (ticket === asked) root.classList.toggle("frosted-terminals", !!(want && ok));
      });
    }
    window.addEventListener("storage", (e) => { if (e.key === FROSTED_KEY) refresh(); });
    window.addEventListener("corvus:themechange", refresh);
    refresh();
    return { refresh };
  }

  function dockButton(noun, onClick) {
    return Corvus.ui.iconButton("square-arrow-down-left", {
      size: 13,
      title: "Put it back into the Corvus window",
      ariaLabel: `Put the ${noun} back into the Corvus window`,
      onClick,
    });
  }

  /** This page's link to the app (Corvus.popouts.announce), once made. */
  let activeLink = null;

  function closeWindow() {
    // Said before the window goes: the desktop app deletes the page without a
    // pagehide, and an app still counting this window would, when asked to
    // open the camera again, bring forward a window that is no longer there.
    if (activeLink) activeLink.stop();
    const n = bridge();
    if (n && typeof n.closeWindow === "function") { n.closeWindow(); return; }
    try { window.close(); } catch (_e) {}
  }

  function raiseWindow() {
    const n = bridge();
    if (n && typeof n.raiseWindow === "function") { n.raiseWindow(); return; }
    try { window.focus(); } catch (_e) {}
  }

  // ---- a camera ---------------------------------------------------------------

  function camera(root, key, params) {
    const stream = {
      id: params.get("id") || "",
      name: params.get("name") || "",
      address: params.get("address") || "",
      kind: params.get("stream") === "webrtc" ? "webrtc" : "rtsp",
    };
    let player = null;
    let link = null;
    const dockBtn = params.get("dock") === "1" ? dockButton("camera", () => {
      if (link) link.dock({ stream });
      if (player) player.stop();
      closeWindow();
    }) : null;
    const rec = frame(root, {
      icon: "video",
      title: stream.name || stream.address || "Camera",
      subtitle: stream.address,
      className: "term-win--video",
      bodyClass: "video-win-body",
      tools: dockBtn ? [dockBtn] : [],
    });
    player = Corvus.videoWindows.attach(rec.bodyEl, stream,
      (text, tone, message) => F.setStatus(rec, text, tone, message));
    link = activeLink = Corvus.popouts.announce(key, "video", {
      state: () => player.snapshot(),
      onClose: () => { player.stop(); closeWindow(); },
      onFocus: raiseWindow,
    });
    whenNative((n) => goNative(rec, n, {
      noun: "camera",
      closeTitle: "Close the camera window",
      onClose: () => { player.stop(); closeWindow(); },
      dockBtn,
      onDrop: dockBtn ? (at) => {
        link.dock({ stream, at });
        player.stop();
        closeWindow();
      } : null,
    }));
    window.addEventListener("pagehide", () => { player.stop(); link.stop(); });
    return { rec, player, link };
  }

  // ---- a terminal ---------------------------------------------------------------

  function terminal(root, key, params) {
    const session = {
      name: params.get("name") || "",
      title: params.get("title") || "",
      host: params.get("host") || "",
      port: params.get("port") || "",
      username: params.get("username") || "",
    };
    const user = session.username ? session.username + "@" : "";
    const address = session.host ? user + session.host + (session.port ? ":" + session.port : "") : "";
    let handle = null;
    let closed = false;
    let link = null;
    const dockBtn = params.get("dock") === "1" ? dockButton("terminal", () => {
      if (link) link.dock({ session });
      dispose();
      closeWindow();
    }) : null;

    const rec = frame(root, {
      icon: "square-terminal",
      title: session.title || session.name,
      subtitle: address,
      className: "term-win--terminal",
      bodyClass: "ssh-term",
      tools: [
        dockBtn,
        Corvus.ui.iconButton("power", {
          size: 13,
          className: "icon-btn term-win-disconnect",
          title: "Disconnect. This stops what is running",
          ariaLabel: "Disconnect the session",
          onClick: () => {
            // Said before the window goes, as closeWindow says it is closing:
            // the app's SSH tab still shows the session as connected otherwise.
            const done = () => {
              if (typeof Corvus.sshTerm.sessionEnded === "function") {
                Corvus.sshTerm.sessionEnded(session.name);
              }
              closeWindow();
            };
            fetch("/api/ssh/disconnect", {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ name: session.name }),
            }).catch(() => {}).then(done, done);
          },
        }),
      ].filter(Boolean),
    });

    function dispose() {
      if (handle) { try { handle.dispose(); } catch (_e) {} }
      handle = null;
    }

    /** A stream to the session; `quiet` when it is only being repaired. */
    function attach(quiet) {
      dispose();
      closed = false;
      Corvus.ui.clear(rec.bodyEl);
      F.setStatus(rec, "CONNECTED", "on");
      if (!Corvus.sshTerm.available()) {
        rec.bodyEl.textContent = "Loading terminal…";
        Corvus.sshTerm.ensure().then(() => attach(quiet)).catch(() => {
          rec.bodyEl.textContent = "Terminal component unavailable. The xterm bundle did not load.";
        });
        return;
      }
      handle = Corvus.sshTerm.create(rec.bodyEl, session, {
        onClosed() {
          closed = true;
          F.setStatus(rec, "OFFLINE", "");
        },
      });
      handle.fit();
      // A repair is the launcher starting a new shell, not the operator
      // asking to type here: the keyboard stays where it is.
      if (!quiet) handle.focus();
    }

    attach();
    link = activeLink = Corvus.popouts.announce(key, "terminal", {
      state: () => ({ state: closed ? "offline" : "connected" }),
      onClose: () => { dispose(); closeWindow(); },
      onFocus: () => { raiseWindow(); if (handle) handle.focus(); },
      onReattach: () => attach(true),
    });
    let frost = null;
    whenNative((n) => {
      goNative(rec, n, {
        noun: "terminal",
        closeTitle: "Close the window. What is running keeps running",
        onClose: () => { dispose(); closeWindow(); },
        dockBtn,
        onDrop: dockBtn ? (at) => {
          link.dock({ session, at });
          dispose();
          closeWindow();
        } : null,
        onMaximize: () => { if (frost) frost.refresh(); },
        // A new frame is a new native window: the blur behind the old one
        // went with it.
        onFrame: () => { if (frost) frost.refresh(); },
      });
      frost = followFrosted(rec, n);
    });
    window.addEventListener("pagehide", () => { dispose(); link.stop(); });
    return { rec, link };
  }

  // ---- a plugin's window ----------------------------------------------------------

  /** The plugins this page has loaded, by id, as they registered. */
  const pluginSpecs = new Map();

  /**
   * This page has no plugin registry (js/plugins.js); a plugin's script calls
   * Corvus.plugins.register as it runs, so a stand-in keeps what it says.
   */
  function pluginShim() {
    if (Corvus.plugins && typeof Corvus.plugins.register === "function") return;
    Corvus.plugins = {
      register(id, spec) {
        if (typeof id === "string" && id && spec && typeof spec === "object") pluginSpecs.set(id, spec);
      },
    };
  }

  function pluginAsset(id, rel) {
    const path = String(rel).split("/").map(encodeURIComponent).join("/");
    return `/api/plugins/asset/${encodeURIComponent(id)}/${path}`;
  }

  function appendScript(src) {
    return new Promise((resolve, reject) => {
      const el = document.createElement("script");
      el.async = false;
      el.src = src;
      el.onload = () => resolve();
      el.onerror = () => reject(new Error(`could not load ${src}`));
      document.head.appendChild(el);
    });
  }

  /** A plugin's registration, its styles and scripts loaded into this page. */
  function loadPlugin(id) {
    pluginShim();
    if (pluginSpecs.has(id)) return Promise.resolve(pluginSpecs.get(id));
    return fetch("/api/plugins")
      .then((r) => r.json())
      .then((data) => {
        const list = data && Array.isArray(data.plugins) ? data.plugins : [];
        const p = list.find((x) => x && x.id === id);
        if (!p) return null;
        (Array.isArray(p.styles) ? p.styles : []).forEach((href) => {
          const link = document.createElement("link");
          link.rel = "stylesheet";
          link.href = pluginAsset(id, href);
          document.head.appendChild(link);
        });
        let chain = Promise.resolve();
        (Array.isArray(p.scripts) ? p.scripts : []).forEach((src) => {
          chain = chain.then(() => appendScript(pluginAsset(id, src)));
        });
        return chain.then(() => pluginSpecs.get(id) || null);
      });
  }

  /**
   * A plugin's window of its own (kind "plugin:<id>"): the plugin's script is
   * loaded here, and its `popout(ctx)` fills the frame. Everything else, the
   * bar, the pin, the title bar, docking and closing, is this page's, as for
   * a terminal. What the plugin gets:
   *
   *   ctx.params        the window's address (what the app opened it with)
   *   ctx.frame(opts)   the frame: {title, subtitle, icon, className,
   *                     bodyClass, noun, closeTitle, tools}. Returns
   *                     {body, setStatus(text, tone, title)}. Once.
   *   ctx.onClose(fn)   run as the window goes, however it goes
   *   ctx.setTitleBar(choice)  this window's own title bar: "system",
   *                     "corvus", or "" for Settings' (corvus/app.py)
   *   ctx.close()       close the window
   */
  function pluginWindow(root, key, params, id) {
    const kind = "plugin:" + id;
    const closers = [];
    let rec = null;
    let opts = {};
    let link = null;
    let frost = null;
    let finished = false;

    function finish() {
      if (finished) return;
      finished = true;
      closers.splice(0).forEach((fn) => { try { fn(); } catch (_e) { /* keep going */ } });
    }
    function done() { finish(); closeWindow(); }

    const dockBtn = params.get("dock") === "1" ? dockButton("window", () => {
      if (link) link.dock({});
      done();
    }) : null;

    const ctx = {
      params,
      frame(o) {
        if (rec) return { body: rec.bodyEl, setStatus: (t, tone, title) => F.setStatus(rec, t, tone, title) };
        opts = o || {};
        rec = frame(root, {
          icon: opts.icon || "app-window",
          title: opts.title || params.get("title") || id,
          subtitle: opts.subtitle != null ? opts.subtitle : (params.get("subtitle") || ""),
          className: opts.className || "",
          bodyClass: opts.bodyClass || "",
          tools: [dockBtn].concat(Array.isArray(opts.tools) ? opts.tools : []).filter(Boolean),
        });
        return { body: rec.bodyEl, setStatus: (t, tone, title) => F.setStatus(rec, t, tone, title) };
      },
      onClose(fn) { if (typeof fn === "function") closers.push(fn); },
      setTitleBar(choice) {
        const n = bridge();
        if (n && typeof n.setTitleBar === "function") n.setTitleBar(String(choice || ""));
      },
      close: done,
    };

    return loadPlugin(id).then((spec) => {
      if (!spec || typeof spec.popout !== "function") {
        root.textContent = "This plugin has no window to show here.";
        return null;
      }
      spec.popout(ctx);
      if (!rec) ctx.frame({});
      link = activeLink = Corvus.popouts.announce(key, kind, {
        state: () => null,
        onClose: done,
        onFocus: raiseWindow,
      });
      whenNative((n) => {
        goNative(rec, n, {
          noun: opts.noun || "window",
          closeTitle: opts.closeTitle,
          onClose: done,
          dockBtn,
          onDrop: dockBtn ? (at) => { link.dock({ at }); done(); } : null,
          onMaximize: () => { if (frost) frost.refresh(); },
          onFrame: () => { if (frost) frost.refresh(); },
        });
        if (rec.el.classList.contains("term-win--terminal")) frost = followFrosted(rec, n);
      });
      window.addEventListener("pagehide", () => { finish(); if (link) link.stop(); });
      return { rec, link };
    }).catch((err) => {
      console.error("plugin window failed:", id, err);
      root.textContent = "This plugin's window could not be loaded.";
      return null;
    });
  }

  /** Build the page from its address. Exported for the tests. */
  function boot(root, search) {
    const params = new URLSearchParams(search || "");
    const kind = params.get("kind");
    const key = params.get("key") || "";
    followTheme();
    if (kind === "video" && params.get("id")) return camera(root, key, params);
    if (kind === "terminal" && params.get("name")) return terminal(root, key, params);
    if (kind && kind.indexOf("plugin:") === 0 && kind.length > 7) {
      return pluginWindow(root, key, params, kind.slice(7));
    }
    root.textContent = "There is nothing to show in this window.";
    return null;
  }

  if (typeof document !== "undefined" && document.getElementById("popout")) {
    boot(document.getElementById("popout"), window.location.search);
  }

  return { boot, goNative, followFrosted, darkGlass, FROSTED_KEY, pluginSpecs };
})();
