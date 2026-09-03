"use strict";
window.Corvus = window.Corvus || {};

Corvus.panel = (function () {
  let panel, handle, tabs, output, input, sendBtn, clearBtn, autoBtn;
  let autoscroll = true;
  let history = [];
  let histIdx = -1;
  let sshContent, futureContent;
  let consoleSource = null;
  let sshOutputEl = null;
  let sshInputEl = null;
  let sshConnectedName = null;
  let sshSseSource = null;

  function nowTs() {
    const d = new Date();
    const p = (n) => String(n).padStart(2, "0");
    const ms = String(d.getMilliseconds()).padStart(3, "0");
    return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${ms}`;
  }

  function addConsoleLine(level, text) {
    const line = document.createElement("div");
    line.className = "con-line " + (level || "");
    line.innerHTML = `<span class="con-time">${nowTs()}</span> ` +
      `<span class="con-arrow">&gt;&gt;</span> <span class="con-msg"></span>`;
    line.querySelector(".con-msg").textContent = text;
    output.appendChild(line);
    if (output.children.length > 800) output.removeChild(output.firstChild);
    if (autoscroll) output.scrollTop = output.scrollHeight;
  }

  function connectConsoleSSE() {
    if (consoleSource) consoleSource.close();
    consoleSource = new EventSource("/api/console/stream");
    consoleSource.addEventListener("message", (e) => {
      try {
        const entry = JSON.parse(e.data);
        if (entry.name === "ping") return;
        addConsoleLine(entry.name === "SHELL" ? "shell" : entry.level, entry.text || entry.name);
      } catch (err) {
        console.error("console SSE parse:", err);
      }
    });
    consoleSource.onerror = () => {};
  }

  async function sendCommand() {
    const v = input.value.trim();
    if (!v) return;
    history.push(v);
    histIdx = history.length;
    addConsoleLine("cmd", v);
    input.value = "";
    try {
      const res = await Corvus.telemetry.sendCommand(v);
      if (res.help) {
        addConsoleLine("", "Available: arm, disarm, mode <MODE>, takeoff [ALT], land, rtl, listener <topic>, top, free, dmesg, shell <cmd>, help");
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
    addConsoleLine("", "Corvus GCS — MAVLink console.");
    addConsoleLine("", "Listening for live MAVLink messages …");
    addConsoleLine("", "Type \"help\" for available commands.");
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
    autoBtn = document.getElementById("consoleAutoscroll");
    sshContent = document.getElementById("sshContent");
    futureContent = document.getElementById("futureContent");
    autoBtn.classList.add("active");

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
      else if (e.key === "ArrowUp") {
        e.preventDefault();
        if (histIdx > 0) { histIdx--; input.value = history[histIdx] || ""; }
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        if (histIdx < history.length - 1) { histIdx++; input.value = history[histIdx] || ""; }
        else { histIdx = history.length; input.value = ""; }
      }
    });

    clearBtn.addEventListener("click", () => { output.innerHTML = ""; });
    autoBtn.addEventListener("click", () => {
      autoscroll = !autoscroll;
      autoBtn.classList.toggle("active", autoscroll);
      if (autoscroll) output.scrollTop = output.scrollHeight;
    });
    output.addEventListener("scroll", () => {
      const atBottom = output.scrollHeight - output.scrollTop - output.clientHeight < 30;
      if (!atBottom && autoscroll) { autoscroll = false; autoBtn.classList.remove("active"); }
    });

    initConsole();
    renderSSHCards();
    initFuture();
    Corvus.ui.refreshIcons();
  }

  return { init, toggle, addConsoleLine, addSSHConnection, showSSHTerminal };
})();
