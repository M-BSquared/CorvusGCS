"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.sshTerm — a real terminal over the SSH bridge.

  What was here before was a command box: a text input at the bottom, one line
  sent per Enter, output appended as plain divs. That is not a terminal. It
  cannot run anything interactive — no `top`, no `vim`, no `sudo` password
  prompt, no Ctrl-C — it renders escape sequences as garbage, and it has to
  invent a prompt of its own because it never sees the remote one, which is why
  it used to print "corvus@companion" no matter who was logged in.

  This drives xterm.js against the raw shell channel instead. Every keystroke
  goes to the remote pty as it is typed and every byte comes back untouched, so
  the prompt on screen is the machine's own prompt, for the account that
  actually logged in.

  Three things this module owns beyond "wire xterm to fetch":

    ordering    keystrokes are coalesced behind a single in-flight POST. Fired
                off in parallel, they would race and arrive scrambled — and a
                shell that receives "sl" for "ls" is worse than a slow one.
    size        the remote pty is told the terminal's real size, so full-screen
                programs lay out against the window the operator can see.
    replay      the stream reconnecting means the backend replays its buffer,
                so a reconnect resets the screen first rather than printing the
                whole session a second time.

  The transport is one WebSocket (/api/ssh/ws): output in, keystrokes and
  sizes out, in order on one connection. An SSE stream held one of the six
  connections a browser allows per host, shared by every window of the app,
  so four terminals left nothing for anything else (corvus/websocket.py).
  A terminal whose WebSocket never opens (no WebSocket, or a backend without
  the route) uses the SSE stream and the POSTs instead, as it did before. One
  that did open and then lost it reconnects by itself, as an EventSource does.
*/
Corvus.sshTerm = (function () {

  // Coalescing window for size changes. Dragging the panel edge fires resize
  // continuously; the remote pty only needs the size it lands on.
  const RESIZE_DEBOUNCE_MS = 120;

  // The smallest size worth telling a shell. A window being laid out measures
  // a few pixels for a moment (a window of its own loads its page before it is
  // shown), and FitAddon's floor is two columns. Sent on, that makes a shell
  // like zsh redraw its prompt two characters wide into the scrollback. No
  // terminal window can really be this small (their minimum is about 40
  // columns), so a size below it is a layout in progress and is not sent.
  const MIN_COLS = 20;
  const MIN_ROWS = 4;

  // Pause before reconnecting a WebSocket that was open and went away,
  // doubled each time up to the maximum.
  const RECONNECT_MS = 500;
  const MAX_RECONNECT_MS = 5000;

  /** The terminal's WebSocket address, or null where there is none to open. */
  function socketUrl(name) {
    const loc = window.location;
    if (typeof window.WebSocket !== "function" || !loc || !loc.host) return null;
    const scheme = loc.protocol === "https:" ? "wss:" : "ws:";
    return `${scheme}//${loc.host}/api/ssh/ws?name=${encodeURIComponent(name)}`;
  }

  /** Whether the vendored xterm bundles are loaded RIGHT NOW.
   *
   *  Synchronous and side-effect free on purpose: xterm is fetched on demand
   *  (see js/lazy.js), and a caller that only wants to know whether there is
   *  anything to tear down must not trigger a 300 KB download on its way
   *  out. Callers that want a terminal call `ensure()` instead. */
  function available() {
    return typeof window.Terminal === "function" &&
      !!(window.FitAddon && window.FitAddon.FitAddon);
  }

  /** The xterm bundles, fetched if this is the first terminal of the session.
   *
   *  Resolves once `available()` is true; rejects if the bundle could not be
   *  fetched, which on an offline laptop with a half-written install is a
   *  real outcome and not an exception. Lives here rather than at the two
   *  call sites (the SSH panel and the floating terminal window) so both get
   *  the same behaviour and the same one-fetch-per-session guarantee. */
  function ensure() {
    if (available()) return Promise.resolve(true);
    if (!Corvus.lazy || typeof Corvus.lazy.terminal !== "function") {
      return Promise.reject(new Error("lazy loader unavailable"));
    }
    return Corvus.lazy.terminal().then(() => {
      if (!available()) throw new Error("xterm loaded but did not define its globals");
      return true;
    });
  }

  /**
   * Put `text` on the clipboard. The asynchronous clipboard first; where it
   * is refused (a QtWebEngine without the permission, an older engine) the
   * copy command on a hidden text field, which such engines still honour
   * during the key press that asked for it. `refocus` gives the keyboard back.
   */
  function copyText(text, refocus) {
    const byCommand = () => {
      if (typeof document === "undefined" || !document.body) return;
      const area = document.createElement("textarea");
      area.value = text;
      area.setAttribute("readonly", "");
      area.style.position = "fixed";
      area.style.left = "-9999px";
      document.body.appendChild(area);
      try { area.select(); document.execCommand("copy"); } catch (_e) {}
      document.body.removeChild(area);
      if (typeof refocus === "function") refocus();
    };
    try {
      if (navigator.clipboard && typeof navigator.clipboard.writeText === "function") {
        navigator.clipboard.writeText(text).catch(byCommand);
        return;
      }
    } catch (_e) {}
    byCommand();
  }

  /* xterm paints into a canvas and takes literal colour strings, so like the
     Plotly charts it cannot reference var(--token) and has to be handed
     resolved values — and re-handed them when the theme changes. */
  function themeFromTokens() {
    const t = Corvus.ui.token;
    const bg = t("--ssh-bg", "#0A0D12");
    const fg = t("--terminal-text", "#C8D0DA");
    return {
      background: bg,
      foreground: fg,
      cursor: t("--accent", "#3DA876"),
      cursorAccent: bg,
      selectionBackground: t("--border", "#2A3038"),
      black: t("--surface-2", "#161C24"),
      red: t("--critical", "#FF514D"),
      green: t("--healthy", "#45D483"),
      yellow: t("--warning", "#F5C842"),
      blue: t("--nav", "#4CC9FF"),
      magenta: "#C48BE0",
      cyan: t("--accent", "#3DA876"),
      white: fg,
      brightBlack: t("--text-3", "#69737F"),
      brightRed: t("--critical", "#FF514D"),
      brightGreen: t("--healthy", "#45D483"),
      brightYellow: t("--warning", "#F5C842"),
      brightBlue: t("--nav", "#4CC9FF"),
      brightMagenta: "#DCA8F0",
      brightCyan: "#7BE0C0",
      brightWhite: t("--text-1", "#E6EAF0"),
    };
  }

  /**
   * Mount a live terminal for one SSH session inside `container`.
   *
   * `conn` is {name, host, port, username}. `opts.onClosed` fires when the
   * remote shell ends, so the caller can retire its CONNECTED badge.
   *
   * Returns a handle: {term, fit, focus, dispose, write}. The caller MUST call
   * dispose() before dropping the container — it closes the SSE stream, the
   * resize observer and the theme subscription, none of which the DOM removal
   * would clean up on its own.
   */
  function create(container, conn, opts) {
    const o = opts || {};
    const name = conn.name;

    const term = new window.Terminal({
      fontFamily: "JetBrains Mono, ui-monospace, monospace",
      fontSize: 12,
      lineHeight: 1.2,
      cursorBlink: true,
      // Deep enough that scrolling back after a long build or a boot log
      // reaches the beginning of it rather than a truncated middle.
      scrollback: 5000,
      theme: themeFromTokens(),
    });
    const fit = new window.FitAddon.FitAddon();
    term.loadAddon(fit);
    term.open(container);
    try { fit.fit(); } catch (_e) {}

    let disposed = false;

    // --- transport: the WebSocket, or SSE and POSTs where it cannot open ---
    let mode = socketUrl(name) ? "ws" : "sse";
    let socket = null;
    let socketOpen = false;
    let everOpened = false;
    let sessionEnded = false;
    let reconnectTimer = null;
    let reconnectMs = RECONNECT_MS;

    function sendSocket(message) {
      if (!socket || !socketOpen) return false;
      try { socket.send(JSON.stringify(message)); return true; } catch (_e) { return false; }
    }

    // --- input: in order, one POST in flight, everything typed meanwhile coalesced ---
    let pending = "";
    let sending = false;
    async function drain() {
      sending = true;
      while (pending && !disposed) {
        const chunk = pending;
        pending = "";
        try {
          await fetch("/api/ssh/send", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ name, data: chunk }),
          });
        } catch (_e) {
          break;   // link is gone; the closed event will explain why
        }
      }
      sending = false;
    }
    /** Hand what was typed to the shell; while a WebSocket connects, it waits. */
    function flushInput() {
      if (!pending || disposed) return;
      if (mode === "ws") {
        if (sendSocket({ type: "input", data: pending })) pending = "";
        return;
      }
      if (!sending) drain();
    }
    const dataSub = term.onData((data) => {
      if (disposed) return;
      pending += data;
      flushInput();
    });

    // Ctrl-C with a selection means copy in every terminal emulator; Ctrl-C
    // without one means SIGINT, which is the whole reason for a real terminal.
    // Only the first is intercepted, so the second still reaches the shell.
    // Ctrl-Shift-V is paste on Windows and Linux terminals; xterm would send
    // it to the shell as ^V, so it is left to the browser, which pastes.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      const copyCombo = (e.metaKey && e.key === "c") ||
        (e.ctrlKey && e.shiftKey && (e.key === "C" || e.key === "c"));
      if (copyCombo && term.hasSelection()) {
        copyText(term.getSelection(), () => term.focus());
        return false;
      }
      if (e.ctrlKey && e.shiftKey && !e.metaKey && (e.key === "V" || e.key === "v")) return false;
      return true;
    });

    // --- size: tell the remote pty what the operator can actually see ---
    let lastCols = 0;
    let lastRows = 0;
    let resizeTimer = null;
    function pushSize() {
      if (disposed) return;
      const cols = term.cols;
      const rows = term.rows;
      if (cols < MIN_COLS || rows < MIN_ROWS) return;
      if (cols === lastCols && rows === lastRows) return;
      if (mode === "ws") {
        // Sent once the socket is open (its onopen calls this again).
        if (!sendSocket({ type: "resize", cols, rows })) return;
      } else {
        fetch("/api/ssh/resize", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ name, cols, rows }),
        }).catch(() => {});
      }
      lastCols = cols;
      lastRows = rows;
    }
    function refit() {
      if (disposed) return;
      try { fit.fit(); } catch (_e) {}
      openStream();
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(pushSize, RESIZE_DEBOUNCE_MS);
    }

    // --- output ---
    // Opened once the terminal has a size worth drawing at, for the same
    // reason as MIN_COLS: the stream starts with the backend's replay of
    // everything the session printed so far, and a prompt written into a
    // terminal two columns wide stays broken after it grows, because the
    // shell positioned it for its own width, not for that one.
    let opens = 0;
    let source = null;
    function openStream() {
      if (source || socket || reconnectTimer || disposed || sessionEnded) return;
      if (term.cols < MIN_COLS || term.rows < MIN_ROWS) return;
      if (mode === "ws") openSocket();
      else openSse();
    }

    function sessionClosed() {
      sessionEnded = true;
      if (typeof o.onClosed === "function") o.onClosed();
    }

    function openSocket() {
      let sock;
      try {
        sock = new window.WebSocket(socketUrl(name));
      } catch (_e) {
        mode = "sse";
        openSse();
        return;
      }
      socket = sock;
      sock.onopen = () => {
        if (disposed || socket !== sock) return;
        socketOpen = true;
        everOpened = true;
        reconnectMs = RECONNECT_MS;
        // A reconnect gets the backend's replay buffer again, as over SSE.
        opens += 1;
        if (opens > 1) term.reset();
        lastCols = 0;
        lastRows = 0;
        pushSize();
        flushInput();
      };
      sock.onmessage = (e) => {
        if (disposed || socket !== sock) return;
        let msg = null;
        try { msg = JSON.parse(e.data); } catch (_e) { return; }
        if (!msg) return;
        if (msg.type === "output" && typeof msg.text === "string") term.write(msg.text);
        else if (msg.type === "closed") sessionClosed();
      };
      sock.onerror = () => {};
      sock.onclose = () => {
        if (socket !== sock) return;
        socket = null;
        socketOpen = false;
        if (disposed || sessionEnded) return;
        if (!everOpened) {
          // Never opened: this backend or this engine has no WebSocket for
          // it. The SSE stream and the POSTs do the same job.
          mode = "sse";
          openSse();
          flushInput();
          lastCols = 0;
          lastRows = 0;
          pushSize();
          return;
        }
        reconnectTimer = setTimeout(() => {
          reconnectTimer = null;
          openStream();
        }, reconnectMs);
        reconnectMs = Math.min(MAX_RECONNECT_MS, reconnectMs * 2);
      };
    }

    function openSse() {
      if (source) return;
      const stream = new EventSource(
        "/api/ssh/stream?name=" + encodeURIComponent(name));
      source = stream;
      stream.onopen = () => {
        // A reconnect gets the backend's replay buffer again. Clearing first
        // makes that a redraw of the session instead of a second copy of it.
        opens += 1;
        if (opens > 1) term.reset();
      };
      stream.addEventListener("output", (e) => {
        if (disposed) return;
        try {
          const data = JSON.parse(e.data);
          if (data && typeof data.text === "string") term.write(data.text);
        } catch (_e) {}
      });
      stream.addEventListener("closed", () => {
        if (disposed) return;
        try { stream.close(); } catch (_e) {}
        sessionClosed();
      });
      stream.onerror = () => {};
    }

    let observer = null;
    if (typeof window.ResizeObserver === "function") {
      observer = new window.ResizeObserver(refit);
      observer.observe(container);
    }
    window.addEventListener("resize", refit);
    // The initial size matters as much as later ones: without it the shell
    // keeps the 80x24 the channel was opened with.
    refit();

    const unTheme = Corvus.ui.onThemeChange(() => {
      if (disposed) return;
      term.options.theme = themeFromTokens();
    });

    function dispose() {
      if (disposed) return;
      disposed = true;
      if (resizeTimer) clearTimeout(resizeTimer);
      window.removeEventListener("resize", refit);
      if (observer) { try { observer.disconnect(); } catch (_e) {} }
      try { unTheme(); } catch (_e) {}
      try { dataSub.dispose(); } catch (_e) {}
      if (source) { try { source.close(); } catch (_e) {} }
      if (reconnectTimer) { clearTimeout(reconnectTimer); reconnectTimer = null; }
      if (socket) {
        const sock = socket;
        socket = null;
        try { sock.close(1000); } catch (_e) {}
      }
      try { term.dispose(); } catch (_e) {}
    }

    return {
      term,
      fit: refit,
      focus() { try { term.focus(); } catch (_e) {} },
      write(text) { if (!disposed) term.write(text); },
      dispose,
    };
  }

  return { available, ensure, create, themeFromTokens, socketUrl, copyText };
})();
