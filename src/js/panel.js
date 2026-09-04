"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.panel — the right-hand workspace: MAVLink console, SSH, plugins.

  The console is the operator's direct line to the airframe, so it is built to
  be usable when a lot is happening at once rather than only when the stream is
  quiet:

    filter       substring match over the live stream, applied to lines already
                 on screen as well as new ones
    colour       severity is read off the line itself — critical/error red,
                 warning amber, success green, commands accent — instead of
                 through a set of ALL/INFO/WARN/ERR buttons that hid the
                 context around the line the operator was looking for
    pause        freeze the view while still buffering, so reading a message
                 does not mean losing the next fifty
    copy / save  the visible lines, for a bug report or a flight log
    completion   Tab completes a command; "?" lists them with what they do
    history      persisted across launches, not just across tab switches

  The stream itself is a buffer of records, not just DOM: filtering has to be
  able to reveal a line that was previously hidden, which means the line has to
  still exist somewhere. The DOM is a rendering of that buffer.
*/
Corvus.panel = (function () {
  const HISTORY_KEY = "corvus.console.history";
  const MAX_HISTORY = 50;
  // How many records to retain. The DOM only ever holds the visible subset, so
  // this is the real memory bound and it is generous: an operator scrolling
  // back after an incident wants the whole session, not the last screen.
  const MAX_LINES = 2000;

  /* The command surface, in one place: used by the "?" listing, by Tab
     completion, and by the placeholder. Mirrors what
     CorvusHandler._api_console_command accepts — SHELL_COMMANDS included,
     which are forwarded verbatim to the PX4 NSH shell. */
  const COMMANDS = [
    { name: "arm", args: "", help: "Arm the vehicle (refused unless PX4 is happy)" },
    { name: "disarm", args: "", help: "Disarm the vehicle" },
    { name: "mode", args: "<MODE>", help: "Set flight mode, e.g. mode HOLD" },
    { name: "takeoff", args: "[ALT]", help: "Take off to ALT metres AGL (default 10)" },
    { name: "land", args: "", help: "Land at the current position" },
    { name: "rtl", args: "", help: "Return to launch" },
    { name: "listener", args: "<topic>", help: "Stream a uORB topic, e.g. listener sensor_combined" },
    { name: "top", args: "", help: "PX4 task/CPU listing" },
    { name: "free", args: "", help: "Free memory" },
    { name: "dmesg", args: "", help: "PX4 boot/kernel log" },
    { name: "tasks", args: "", help: "Running tasks" },
    { name: "perf", args: "", help: "Performance counters" },
    { name: "boot_log", args: "", help: "Boot log" },
    { name: "hrt", args: "", help: "High-resolution timer info" },
    { name: "shell", args: "<cmd>", help: "Send a raw NSH command (case preserved)" },
    { name: "help", args: "", help: "Ask the backend what it accepts" },
  ];

  /* Record level -> CSS class on the line. Colour is the ONLY thing that
     distinguishes severity now, so the mapping is explicit and total: the
     bridge's "critical" (STATUSTEXT severity <= 3) shares the red of "error"
     rather than falling through to the default text colour, and an unknown
     level from a future backend renders as a plain line instead of an
     unstyled class name. */
  const LEVEL_CLASS = {
    critical: "error",
    error: "error",
    warning: "warning",
    success: "success",
    info: "info",
    cmd: "cmd",
    shell: "shell",
    nav: "nav",
  };

  let panel, handle, tabs, output, input, sendBtn, clearBtn;
  let pauseBtn, copyBtn, saveBtn, filterInput, countEl, hintEl;
  let jumpBtn, jumpLabel;
  // Missed lines since the operator scrolled away from the live end. Drives
  // the jump chip's label, which is the only reason the count is kept.
  let missed = 0;
  let autoscroll = true;
  let history = [];
  let histIdx = -1;
  let sshContent, futureContent;
  let consoleUnsub = null;
  let sshOutputEl = null;
  let sshInputEl = null;
  let sshConnectedName = null;
  let sshSseSource = null;

  // The console stream as data. `lines` is the retained history; the DOM shows
  // whichever of them pass the current filter.
  let lines = [];
  let filterText = "";
  let paused = false;
  // Records that arrived while paused, held back rather than dropped.
  let pendingLines = [];

  function nowTs() {
    const d = new Date();
    const p = (n) => String(n).padStart(2, "0");
    const ms = String(d.getMilliseconds()).padStart(3, "0");
    return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${ms}`;
  }

  // ---- history persistence ----

  function loadHistory() {
    try {
      const raw = JSON.parse(localStorage.getItem(HISTORY_KEY) || "[]");
      history = Array.isArray(raw) ? raw.filter((s) => typeof s === "string" && s) : [];
    } catch (_e) {
      history = [];
    }
    histIdx = history.length;
  }

  function saveHistory() {
    try {
      localStorage.setItem(HISTORY_KEY, JSON.stringify(history.slice(-MAX_HISTORY)));
    } catch (_e) {}
  }

  // ---- the line buffer ----

  /**
   * Does a record survive the given filter? Pure — the module state is passed
   * in, so the rule can be tested without a DOM.
   *
   * Text only. There is deliberately no severity filter: a level floor hid the
   * INFO lines that explain the error above them, and the operator's question
   * is almost always "what happened around this", not "show me only errors".
   * Severity is carried by colour instead, so nothing ever leaves the stream.
   */
  function matchesFilter(rec, text) {
    if (text && String(rec.text).toLowerCase().indexOf(text) === -1) return false;
    return true;
  }

  /** matchesFilter bound to the module's current filter state. */
  function passesFilter(rec) {
    return matchesFilter(rec, filterText);
  }

  /** CSS class for a record's level; "" for untagged or unknown levels. */
  function levelClass(level) {
    return LEVEL_CLASS[level] || "";
  }

  function lineEl(rec) {
    const line = document.createElement("div");
    line.className = "con-line " + levelClass(rec.level);
    const time = document.createElement("span");
    time.className = "con-time";
    time.textContent = rec.ts;
    const arrow = document.createElement("span");
    arrow.className = "con-arrow";
    arrow.textContent = ">>";
    const msg = document.createElement("span");
    msg.className = "con-msg";
    msg.textContent = rec.text;
    line.appendChild(time);
    line.appendChild(document.createTextNode(" "));
    line.appendChild(arrow);
    line.appendChild(document.createTextNode(" "));
    line.appendChild(msg);
    return line;
  }

  /** Append a record and, if it passes the filter, show it. */
  function addConsoleLine(level, text) {
    const rec = { ts: nowTs(), level: level || "", text: String(text == null ? "" : text) };
    if (paused) {
      // Held, not dropped — the point of pausing is to read without losing
      // what arrives meanwhile.
      pendingLines.push(rec);
      if (pendingLines.length > MAX_LINES) pendingLines.shift();
      updateCount();
      return;
    }
    pushLine(rec);
  }

  function pushLine(rec) {
    lines.push(rec);
    if (lines.length > MAX_LINES) lines.shift();
    if (passesFilter(rec)) {
      output.appendChild(lineEl(rec));
      while (output.children.length > MAX_LINES) output.removeChild(output.firstChild);
      if (autoscroll) output.scrollTop = output.scrollHeight;
      else missed++;
    }
    updateCount();
  }

  /** Re-render the visible set from the buffer. Called when a filter changes. */
  function renderLines() {
    Corvus.ui.clear(output);
    const visible = lines.filter(passesFilter);
    // Only the tail is rendered: the buffer can hold far more than is useful
    // to have in the DOM, and the operator scrolls back through what is shown.
    visible.slice(-MAX_LINES).forEach((rec) => output.appendChild(lineEl(rec)));
    if (autoscroll) output.scrollTop = output.scrollHeight;
    updateCount();
  }

  function updateCount() {
    if (countEl) {
      const visible = lines.filter(passesFilter).length;
      const held = pendingLines.length;
      const parts = [];
      if (filterText) parts.push(`${visible} / ${lines.length}`);
      else parts.push(`${lines.length}`);
      if (held) parts.push(`${held} held`);
      if (paused) parts.push("paused");
      countEl.textContent = parts.join("  ·  ");
      countEl.classList.toggle("is-paused", paused);
    }
    updateJump();
  }

  /**
   * The jump-to-live chip. It replaces what used to be a permanent auto-scroll
   * toggle: a control that had to be on screen at all times to express a state
   * the scroll position already shows. The chip appears only once the operator
   * has scrolled away from the live end, and says how much has arrived since —
   * which the toggle never could.
   */
  function updateJump() {
    if (!jumpBtn) return;
    jumpBtn.hidden = autoscroll;
    if (jumpLabel) {
      jumpLabel.textContent = missed > 0
        ? `${missed} new line${missed === 1 ? "" : "s"}`
        : "Jump to live";
    }
  }

  /** Scroll to the live end and resume following it. */
  function jumpToLive() {
    autoscroll = true;
    missed = 0;
    if (output) output.scrollTop = output.scrollHeight;
    updateJump();
  }

  function setPaused(next) {
    paused = !!next;
    if (!paused) {
      // Flush what arrived while frozen, in order.
      pendingLines.forEach(pushLine);
      pendingLines = [];
    }
    if (pauseBtn) {
      pauseBtn.classList.toggle("active", paused);
      pauseBtn.title = paused ? "Resume the stream" : "Pause the stream";
      pauseBtn.setAttribute("aria-label", pauseBtn.title);
      Corvus.ui.clear(pauseBtn).appendChild(Corvus.ui.icon(paused ? "play" : "pause", 15));
      Corvus.ui.refreshIcons();
    }
    updateCount();
  }

  /** The currently visible lines as plain text — what copy and save both use. */
  function visibleText() {
    return lines.filter(passesFilter)
      .map((r) => `${r.ts}  ${r.level ? "[" + r.level + "] " : ""}${r.text}`)
      .join("\n");
  }

  async function copyVisible() {
    const text = visibleText();
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      addConsoleLine("info", `Copied ${lines.filter(passesFilter).length} lines to the clipboard.`);
    } catch (_e) {
      // Clipboard access is refused in some embeddings; say so rather than
      // failing silently and leaving the operator to wonder.
      addConsoleLine("error", "Clipboard unavailable — use Save instead.");
    }
  }

  function saveLog() {
    const text = visibleText();
    if (!text) return;
    const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
    Corvus.telemetry.requestJson("/api/console/save", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ filename: `corvus-console_${stamp}.log`, text }),
    }).then((res) => {
      addConsoleLine("success", `Console log written to ${res.path}`);
    }).catch((err) => {
      addConsoleLine("error", (err && err.message) || "Could not save the log");
    });
  }

  // ---- command entry ----

  /** Commands whose name starts with the current token. */
  function completionsFor(text) {
    const head = text.split(/\s+/)[0].toLowerCase();
    if (!head) return [];
    return COMMANDS.filter((c) => c.name.startsWith(head) && c.name !== head);
  }

  function showHint(text) {
    if (!hintEl) return;
    hintEl.textContent = text || "";
    hintEl.hidden = !text;
  }

  /** Tab: complete the command when it is unambiguous, otherwise list the
   *  candidates. The shell convention, because that is what this looks like. */
  function completeCommand() {
    const matches = completionsFor(input.value);
    if (!matches.length) { showHint(""); return; }
    if (matches.length === 1) {
      const c = matches[0];
      input.value = c.name + (c.args ? " " : "");
      showHint(c.args ? `${c.name} ${c.args} — ${c.help}` : c.help);
      return;
    }
    showHint(matches.map((c) => c.name).join("  "));
  }

  /** Print the command list into the console itself, where it can be scrolled
   *  back to — unlike a tooltip. */
  function printCommandList() {
    addConsoleLine("info", "Commands:");
    COMMANDS.forEach((c) => {
      const sig = c.args ? `${c.name} ${c.args}` : c.name;
      addConsoleLine("", `  ${sig.padEnd(18)}${c.help}`);
    });
  }

  /** Clear the console — buffer and DOM. Clearing only the DOM would let the
   *  next filter change resurrect every line the operator just dismissed. */
  function clearConsole() {
    lines = [];
    pendingLines = [];
    Corvus.ui.clear(output);
    updateCount();
  }

  /* The console feed comes from the shared, reference-counted bus in
     telemetry.js rather than a private EventSource: the calibration wizard
     needs the same STATUSTEXT stream, and two connections to one endpoint is a
     stream this app cannot spare (six per origin, and telemetry, params, tiles
     and firmware already want theirs). */
  function connectConsoleSSE() {
    if (consoleUnsub) consoleUnsub();
    consoleUnsub = Corvus.telemetry.subscribeConsole((entry) => {
      addConsoleLine(entry.name === "SHELL" ? "shell" : entry.level, entry.text || entry.name);
    });
  }

  async function sendCommand() {
    const v = input.value.trim();
    if (!v) return;
    // "?" is the local shortcut for the command list — no round trip, and it
    // works when the link is down, which is exactly when you need to look
    // something up.
    if (v === "?") {
      input.value = "";
      showHint("");
      printCommandList();
      return;
    }
    history.push(v);
    if (history.length > MAX_HISTORY) history.shift();
    histIdx = history.length;
    saveHistory();
    addConsoleLine("cmd", v);
    input.value = "";
    showHint("");
    try {
      const res = await Corvus.telemetry.sendCommand(v);
      if (res.help) {
        printCommandList();
      } else if (res.error) {
        addConsoleLine("error", res.error);
      } else if (res.shell) {
        addConsoleLine("info", "Shell command sent — output streaming…");
      } else if (res.ok) {
        addConsoleLine("success", `Command sent: ${v}`);
      }
    } catch (err) {
      addConsoleLine("error", `Command failed: ${err.message}`);
    }
  }

  function initConsole() {
    loadHistory();
    setPaused(false);
    addConsoleLine("", "Corvus GCS — MAVLink console.");
    addConsoleLine("", "Listening for live MAVLink messages …");
    addConsoleLine("", "Type \"?\" for the command list, Tab to complete.");
    connectConsoleSSE();
  }

  async function renderSSHCards() {
    sshContent.innerHTML = "";
    let devices = [];
    try {
      const data = await Corvus.telemetry.requestJson("/api/ssh/connections");
      devices = (data && data.connections) || [];
    } catch (_e) {
      const note = document.createElement("div");
      note.className = "ssh-card";
      note.innerHTML = '<div class="page-card-desc">Could not load connections.</div>';
      sshContent.appendChild(note);
    }
    devices.forEach((dev) => {
      const card = document.createElement("div");
      card.className = "ssh-card";
      const connected = !!dev.connected;
      card.innerHTML =
        `<div class="ssh-card-top">
          <div class="ssh-icon" data-lucide="server"></div>
          <div class="ssh-info">
            <span class="ssh-name">${dev.name}</span>
            <span class="ssh-host">${dev.host}${dev.port ? ":" + dev.port : ""}</span>
          </div>
          <span class="ssh-status${connected ? " connected" : ""}"><span class="dot"></span>${connected ? "CONNECTED" : "OFFLINE"}</span>
        </div>`;
      const actions = document.createElement("div");
      actions.className = "ssh-card-actions";
      const btn = Corvus.ui.button({
        variant: "primary",
        shape: "block",
        className: "ssh-connect",
        label: "CONNECT",
        onClick: () => connectSSH(dev.name, dev.host, btn),
      });
      btn.dataset.name = dev.name;
      actions.appendChild(btn);
      const rm = Corvus.ui.iconButton("trash-2", {
        title: `Remove ${dev.name}`,
        ariaLabel: `Remove ${dev.name}`,
      });
      rm.addEventListener("click", async () => {
        if (!confirm(`Remove connection ${dev.name}?`)) return;
        try {
          await fetch("/api/ssh/connections/remove", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ name: dev.name }),
          }).then((r) => r.json());
        } catch (_e) {}
        renderSSHCards();
      });
      actions.appendChild(rm);
      card.appendChild(actions);
      sshContent.appendChild(card);
    });
    sshContent.appendChild(Corvus.ui.button({
      variant: "secondary",
      shape: "block",
      className: "ssh-connect",
      icon: "plus",
      label: "ADD CONNECTION",
      onClick: () => addSSHConnection(),
    }));
    Corvus.ui.refreshIcons();
  }

  // Surface a connect failure inline in the existing SSH card (the card list
  // stays visible) instead of opening a fresh terminal + /api/ssh/stream SSE
  // on a failed connection — which previously leaked an SSE and rendered a
  // misleading "CONNECTED" terminal for a connection that did not succeed.
  // The console error log (addConsoleLine) is kept on the res.error path.
  function showSSHCardError(btn, msg) {
    if (!btn) return;   // no card to surface into — caller logs elsewhere
    btn.textContent = "CONNECT";
    const actions = btn.parentNode;
    if (!actions) return;
    let note = actions.querySelector(".ssh-card-error");
    if (!note) {
      note = document.createElement("span");
      note.className = "ssh-card-error";
      note.style.color = "var(--critical)";
      note.style.fontSize = "11px";
      note.style.marginLeft = "8px";
      actions.appendChild(note);
    }
    note.textContent = msg || "Connection failed";
  }

  async function connectSSH(name, host, btn) {
    if (btn) { btn.textContent = "CONNECTING …"; btn.disabled = true; }
    let res;
    try {
      res = await fetch("/api/ssh/connect", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name }),   // connect by name; backend loads saved creds
      }).then((r) => r.json());

      if (res.ok && res.connected) {
        sshConnectedName = name;
        renderSSHTerminal(name, host);
      } else {
        // Failure: do NOT open a fresh terminal/SSE — surface the error in the
        // existing card so the operator can retry from the connection list.
        addConsoleLine("error", `SSH connect failed: ${res.error || host}`);
        showSSHCardError(btn, res.error || "Connection failed");
      }
    } catch (err) {
      res = { ok: false, error: err.message };
      showSSHCardError(btn, err.message);
    }
    if (btn) btn.disabled = false;
    return res;
  }

  // Hand off to the live terminal view from outside the SSH tab (e.g. the
  // Settings page CONNECT button). Sets the connected-name state and renders
  // the terminal + SSE stream exactly once; does NOT issue a connect POST
  // (the caller already connected by name).
  function showSSHTerminal(name, host) {
    sshConnectedName = name;
    renderSSHTerminal(name, host);
  }

  function renderSSHTerminal(name, host, errorMsg) {
    // Close any prior SSH stream BEFORE opening a new one. Without this,
    // connecting to device B while A is connected (via the card CONNECT, the
    // Add-Connection modal, or showSSHTerminal) overwrites the reference and
    // leaves A's /api/ssh/stream open for the page lifetime — a leaked socket
    // whose output listener still calls appendSSHOutput into the now-different
    // sshOutputEl (cross-session output bleed).
    if (sshSseSource) { try { sshSseSource.close(); } catch (_e) {} sshSseSource = null; }
    sshContent.innerHTML = "";
    const card = document.createElement("div");
    card.className = "ssh-card";
    card.innerHTML =
      `<div class="ssh-card-top">
        <div class="ssh-icon" data-lucide="server"></div>
        <div class="ssh-info">
          <span class="ssh-name">${name}</span>
          <span class="ssh-host">${host}</span>
        </div>
        <span class="ssh-status connected"><span class="dot"></span>CONNECTED</span>
      </div>
      <div class="ssh-term" id="sshTerm"></div>
      <div class="ssh-input-row">
        <span class="t-line"><span class="t-user">corvus@companion</span><span class="t-path">:~$</span>&nbsp;</span>
        <input class="ssh-input" id="sshInput" placeholder="type a command..." autocomplete="off" spellcheck="false" />
      </div>`;
    const discBtn = Corvus.ui.button({
      variant: "secondary",
      shape: "block",
      className: "ssh-connect disconnect",
      label: "DISCONNECT",
    });
    discBtn.addEventListener("click", async () => {
      await fetch("/api/ssh/disconnect", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name }),
      }).then((r) => r.json());
      sshConnectedName = null;
      if (sshSseSource) { sshSseSource.close(); sshSseSource = null; }
      renderSSHCards();
    });
    card.appendChild(discBtn);
    sshContent.appendChild(card);
    Corvus.ui.refreshIcons();

    sshOutputEl = card.querySelector("#sshTerm");
    sshInputEl = card.querySelector("#sshInput");

    if (errorMsg) {
      const l = document.createElement("div");
      l.className = "t-line";
      l.style.color = "var(--critical)";
      l.textContent = errorMsg;
      sshOutputEl.appendChild(l);
    }

    sshSseSource = new EventSource("/api/ssh/stream");
    sshSseSource.addEventListener("output", (e) => {
      try {
        const data = JSON.parse(e.data);
        appendSSHOutput(data.text);
      } catch (err) {}
    });
    sshSseSource.onerror = () => {};

    sshInputEl.addEventListener("keydown", async (e) => {
      if (e.key === "Enter") {
        const v = sshInputEl.value;
        sshInputEl.value = "";
        appendSSHOutput(`${v}\r\n`, "cmd");
        await fetch("/api/ssh/send", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ name, data: v + "\n" }),
        });
      }
    });
    sshInputEl.focus();
  }

  function appendSSHOutput(text, cls) {
    if (!sshOutputEl) return;
    const l = document.createElement("div");
    l.className = "t-line" + (cls ? " t-" + cls : "");
    l.textContent = text;
    sshOutputEl.appendChild(l);
    sshOutputEl.scrollTop = sshOutputEl.scrollHeight;
  }

  // The fields of the add-SSH dialog, in the order they are shown. One list
  // drives both the DOM and the read-back, so a field can never be rendered
  // and then forgotten when the form is submitted.
  const SSH_FIELDS = [
    { key: "name", label: "Name", placeholder: "CORVUS-01" },
    { key: "host", label: "Host", placeholder: "192.168.2.10", mono: true, autofocus: true },
    { key: "port", label: "Port", type: "number", value: "22", mono: true },
    { key: "username", label: "Username", placeholder: "corvus", value: "corvus" },
    { key: "password", label: "Password", type: "password", mono: true,
      placeholder: "optional — use key file" },
    { key: "key_path", label: "Key file", mono: true, placeholder: "/home/user/.ssh/id_rsa" },
  ];

  function addSSHConnection(onSaved) {
    // onSaved: optional () => void, invoked after a successful save so a caller
    // (e.g. the Settings page) can refresh its own list. A click Event passed
    // via addEventListener is not a function, so it is safely ignored.
    const onSavedCb = typeof onSaved === "function" ? onSaved : null;

    const body = document.createDocumentFragment();
    const inputs = {};
    let autofocusEl = null;
    SSH_FIELDS.forEach((f) => {
      const control = Corvus.ui.input({
        id: "sshFld_" + f.key,
        type: f.type || "text",
        value: f.value,
        placeholder: f.placeholder,
        mono: f.mono,
        ariaLabel: f.label,
        autocomplete: false,
      });
      inputs[f.key] = control;
      if (f.autofocus) autofocusEl = control;
      body.appendChild(Corvus.ui.field({ label: f.label, control }));
    });
    const error = Corvus.ui.message();
    body.appendChild(error.el);

    const cancelBtn = Corvus.ui.button({
      variant: "secondary", label: "CANCEL", onClick: () => dialog.close(),
    });
    const connectBtn = Corvus.ui.button({
      variant: "primary", icon: "plug", label: "CONNECT", onClick: submit,
    });

    const dialog = Corvus.ui.modal({
      title: "Add SSH Connection",
      size: "sm",
      body,
      actions: [cancelBtn, connectBtn],
    });
    dialog.open();
    // ui.modal focuses its first control (Name); Host is the field that
    // actually needs filling in, so take focus from there.
    if (autofocusEl && typeof autofocusEl.focus === "function") autofocusEl.focus();

    function value(key) { return (inputs[key].value || "").trim(); }

    async function submit() {
      const name = value("name") || "DEVICE";
      const host = value("host");
      const port = parseInt(inputs.port.value, 10) || 22;
      const username = value("username") || "corvus";
      const password = inputs.password.value || null;
      const key_path = value("key_path") || null;
      if (!host) { error.show("Host is required.", "err"); return; }
      error.hide();

      // 1) Save first so connect-by-name can load the creds from config.
      let saveRes;
      try {
        saveRes = await fetch("/api/ssh/connections", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ name, host, port, username, password, key_path }),
        }).then((r) => r.json());
      } catch (err) {
        error.show((err && err.message) || "Save failed", "err");
        return;
      }
      if (!saveRes || !saveRes.ok) {
        error.show((saveRes && saveRes.error) || "Save failed", "err");
        return;
      }
      if (onSavedCb) onSavedCb();
      dialog.close();

      // 2) Connect by name — the creds are now persisted.
      try {
        const res = await fetch("/api/ssh/connect", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ name }),
        }).then((r) => r.json());
        if (res.ok && res.connected) {
          sshConnectedName = name;
          renderSSHTerminal(name, host);
        } else {
          renderSSHTerminal(name, host, res.error || "Connection failed");
        }
      } catch (err) {
        renderSSHTerminal(name, host, (err && err.message) || "Connection failed");
      }
    }
  }

  function initFuture() {
    // The FUTURE tab is the extension point of Corvus GCS. plugins.js renders
    // the registered-plugin grid (or the placeholder when none registered) and
    // owns the open/close lifecycle. The api object is built inside
    // plugins.init so the plugin contract stays self-contained here.
    Corvus.plugins.init(futureContent, Corvus.telemetry);
  }

  function toggle() {
    const collapsed = panel.classList.toggle("collapsed");
    panel.dataset.state = collapsed ? "closed" : "open";
    handle.title = collapsed ? "Expand panel" : "Collapse panel";
    handle.querySelector("i, svg")?.remove();
    const i = document.createElement("i");
    i.setAttribute("data-lucide", collapsed ? "chevron-left" : "chevron-right");
    handle.appendChild(i);
    Corvus.ui.refreshIcons();
    setTimeout(() => window.dispatchEvent(new Event("resize")), 300);
  }

  function init() {
    panel = document.getElementById("rightPanel");
    handle = document.getElementById("panelHandle");
    tabs = document.getElementById("panelTabs");
    output = document.getElementById("consoleOutput");
    input = document.getElementById("consoleInput");
    sendBtn = document.getElementById("consoleSend");
    clearBtn = document.getElementById("clearConsole");
    jumpBtn = document.getElementById("consoleJump");
    jumpLabel = document.getElementById("consoleJumpLabel");
    pauseBtn = document.getElementById("consolePause");
    copyBtn = document.getElementById("consoleCopy");
    saveBtn = document.getElementById("consoleSave");
    filterInput = document.getElementById("consoleFilter");
    countEl = document.getElementById("consoleCount");
    hintEl = document.getElementById("consoleHint");
    sshContent = document.getElementById("sshContent");
    futureContent = document.getElementById("futureContent");

    handle.addEventListener("click", toggle);

    document.getElementById("addSsh").addEventListener("click", addSSHConnection);

    tabs.addEventListener("click", (e) => {
      const tab = e.target.closest(".tab");
      if (!tab) return;
      tabs.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t === tab));
      const id = tab.dataset.tab;
      document.querySelectorAll(".tab-panel").forEach((p) =>
        p.classList.toggle("active", p.dataset.panel === id));
    });

    sendBtn.addEventListener("click", sendCommand);
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") sendCommand();
      else if (e.key === "Tab") {
        // Shell convention: Tab completes rather than moving focus. The send
        // button is one Shift-Tab away, so nothing becomes unreachable.
        e.preventDefault();
        completeCommand();
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        if (histIdx > 0) { histIdx--; input.value = history[histIdx] || ""; }
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        if (histIdx < history.length - 1) { histIdx++; input.value = history[histIdx] || ""; }
        else { histIdx = history.length; input.value = ""; }
      } else if (e.key === "l" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        clearConsole();
      }
    });
    // Live hint as the command is typed, so the argument shape is visible
    // before pressing Enter rather than after it fails.
    input.addEventListener("input", () => {
      const head = input.value.trim().split(/\s+/)[0].toLowerCase();
      const exact = COMMANDS.find((c) => c.name === head);
      if (exact) showHint(exact.args ? `${exact.name} ${exact.args} — ${exact.help}` : exact.help);
      else showHint("");
    });

    if (filterInput) {
      filterInput.addEventListener("input", () => {
        filterText = filterInput.value.trim().toLowerCase();
        renderLines();
      });
    }
    if (pauseBtn) pauseBtn.addEventListener("click", () => setPaused(!paused));
    if (copyBtn) copyBtn.addEventListener("click", copyVisible);
    if (saveBtn) saveBtn.addEventListener("click", saveLog);

    clearBtn.addEventListener("click", clearConsole);
    if (jumpBtn) jumpBtn.addEventListener("click", jumpToLive);
    output.addEventListener("scroll", () => {
      const atBottom = output.scrollHeight - output.scrollTop - output.clientHeight < 30;
      if (!atBottom && autoscroll) {
        // Scrolling up IS the "stop following" gesture — no toggle needed.
        autoscroll = false;
        missed = 0;
        updateJump();
      } else if (atBottom && !autoscroll) {
        // ...and scrolling back down resumes, so the chip is a shortcut, not
        // the only way back.
        jumpToLive();
      }
    });

    initConsole();
    renderSSHCards();
    initFuture();
    Corvus.ui.refreshIcons();
  }

  return {
    init, toggle, addConsoleLine, addSSHConnection, showSSHTerminal,
    // Exposed for tests: the console's pure pieces, assertable without a DOM.
    matchesFilter,
    levelClass,
    completionsFor,
    COMMANDS,
  };
})();
