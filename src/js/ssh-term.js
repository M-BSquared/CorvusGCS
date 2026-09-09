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
*/
Corvus.sshTerm = (function () {

  // Coalescing window for size changes. Dragging the panel edge fires resize
  // continuously; the remote pty only needs the size it lands on.
  const RESIZE_DEBOUNCE_MS = 120;

  /** Whether the vendored xterm bundles actually loaded. */
  function available() {
    return typeof window.Terminal === "function" &&
      !!(window.FitAddon && window.FitAddon.FitAddon);
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

    // --- input: one POST in flight, everything typed meanwhile coalesced ---
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
    const dataSub = term.onData((data) => {
      if (disposed) return;
      pending += data;
      if (!sending) drain();
    });

    // Ctrl-C with a selection means copy in every terminal emulator; Ctrl-C
    // without one means SIGINT, which is the whole reason for a real terminal.
    // Only the first is intercepted, so the second still reaches the shell.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      const copyCombo = (e.metaKey && e.key === "c") ||
        (e.ctrlKey && e.shiftKey && (e.key === "C" || e.key === "c"));
      if (copyCombo && term.hasSelection()) {
        try { navigator.clipboard.writeText(term.getSelection()); } catch (_e) {}
        return false;
      }
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
      if (cols === lastCols && rows === lastRows) return;
      lastCols = cols;
      lastRows = rows;
      fetch("/api/ssh/resize", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name, cols, rows }),
      }).catch(() => {});
    }
    function refit() {
      if (disposed) return;
      try { fit.fit(); } catch (_e) {}
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(pushSize, RESIZE_DEBOUNCE_MS);
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

    // --- output ---
    let opens = 0;
    const source = new EventSource(
      "/api/ssh/stream?name=" + encodeURIComponent(name));
    source.onopen = () => {
      // A reconnect gets the backend's replay buffer again. Clearing first
      // makes that a redraw of the session instead of a second copy of it.
      opens += 1;
      if (opens > 1) term.reset();
    };
    source.addEventListener("output", (e) => {
      if (disposed) return;
      try {
        const data = JSON.parse(e.data);
        if (data && typeof data.text === "string") term.write(data.text);
      } catch (_e) {}
    });
    source.addEventListener("closed", () => {
      if (disposed) return;
      try { source.close(); } catch (_e) {}
      if (typeof o.onClosed === "function") o.onClosed();
    });
    source.onerror = () => {};

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
      try { source.close(); } catch (_e) {}
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

  return { available, create, themeFromTokens };
})();
