"use strict";
window.Corvus = window.Corvus || {};

Corvus.panel = (function () {
  let panel, handle, body, tabs, output, input, sendBtn, clearBtn, autoBtn;
  let autoscroll = true;
  let history = [];
  let histIdx = -1;
  let sshContent, futureContent;
  let consoleSource = null;
  let sshOutputEl = null;
  let sshInputEl = null;
  let sshConnectedName = null;
  let sshSseSource = null;

  function refreshIcons() {
    if (window.lucide && lucide.createIcons) lucide.createIcons();
  }

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
        addConsoleLine(entry.level, entry.text || entry.name);
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
        addConsoleLine("", "Available: arm, disarm, mode <MODE>, takeoff [ALT], land, rtl, help");
      } else if (res.error) {
        addConsoleLine("error", res.error);
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

  function renderSSHCards() {
    const devices = [
      { name: "CORVUS-01", host: "192.168.2.10" },
      { name: "CORVUS-GCS", host: "192.168.2.5" },
    ];
    sshContent.innerHTML = "";
    devices.forEach((dev) => {
      const card = document.createElement("div");
      card.className = "ssh-card";
      card.innerHTML =
        `<div class="ssh-card-top">
          <div class="ssh-icon" data-lucide="server"></div>
          <div class="ssh-info">
            <span class="ssh-name">${dev.name}</span>
            <span class="ssh-host">${dev.host}</span>
          </div>
          <span class="ssh-status" data-status="${dev.name}"><span class="dot"></span>OFFLINE</span>
        </div>`;
      const btn = document.createElement("button");
      btn.className = "ssh-connect";
      btn.textContent = "CONNECT";
      btn.dataset.host = dev.host;
      btn.dataset.name = dev.name;
      btn.addEventListener("click", () => connectSSH(dev.name, dev.host, btn));
      card.appendChild(btn);
      sshContent.appendChild(card);
    });
    const addBtn = document.createElement("button");
    addBtn.className = "ssh-connect";
    addBtn.style.background = "var(--surface-2)";
    addBtn.style.color = "var(--text-1)";
    addBtn.style.border = "1px solid var(--border)";
    addBtn.textContent = "+ ADD CONNECTION";
    addBtn.addEventListener("click", addSSHConnection);
    sshContent.appendChild(addBtn);
    refreshIcons();
  }

  async function connectSSH(name, host, btn) {
    btn.textContent = "CONNECTING …";
    btn.disabled = true;
    try {
      const res = await fetch("/api/ssh/connect", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name, host, username: "corvus", port: 22 }),
      }).then((r) => r.json());

      if (res.ok && res.connected) {
        sshConnectedName = name;
        renderSSHTerminal(name, host);
      } else {
        addConsoleLine("error", `SSH connect failed: ${res.error || host}`);
        renderSSHTerminal(name, host, res.error || "Connection failed");
      }
    } catch (err) {
      renderSSHTerminal(name, host, err.message);
    }
    btn.disabled = false;
  }

  function renderSSHTerminal(name, host, errorMsg) {
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
    const discBtn = document.createElement("button");
    discBtn.className = "ssh-connect disconnect";
    discBtn.textContent = "DISCONNECT";
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
    refreshIcons();

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

  function addSSHConnection() {
    const overlay = document.createElement("div");
    overlay.className = "ssh-modal-overlay";
    overlay.innerHTML = `
      <div class="ssh-modal">
        <div class="ssh-modal-header">
          <span class="ssh-modal-title">Add SSH Connection</span>
          <button class="icon-btn" id="sshModalClose"><i data-lucide="x"></i></button>
        </div>
        <div class="ssh-field">
          <label class="ssh-field-label">Name</label>
          <input type="text" class="ssh-field-input" id="sshFldName" placeholder="CORVUS-01" value="" />
        </div>
        <div class="ssh-field">
          <label class="ssh-field-label">Host</label>
          <input type="text" class="ssh-field-input mono" id="sshFldHost" placeholder="192.168.2.10" />
        </div>
        <div class="ssh-field">
          <label class="ssh-field-label">Port</label>
          <input type="number" class="ssh-field-input mono" id="sshFldPort" value="22" />
        </div>
        <div class="ssh-field">
          <label class="ssh-field-label">Username</label>
          <input type="text" class="ssh-field-input" id="sshFldUser" placeholder="corvus" value="corvus" />
        </div>
        <div class="ssh-field">
          <label class="ssh-field-label">Password</label>
          <input type="password" class="ssh-field-input mono" id="sshFldPass" placeholder="••••••••" />
        </div>
        <div class="ssh-modal-actions">
          <button class="ssh-modal-btn secondary" id="sshModalCancel">CANCEL</button>
          <button class="ssh-modal-btn primary" id="sshModalConnect">
            <i data-lucide="plug"></i>
            <span>CONNECT</span>
          </button>
        </div>
      </div>`;
    document.body.appendChild(overlay);
    refreshIcons();

    const close = () => overlay.remove();
    overlay.querySelector("#sshModalClose").addEventListener("click", close);
    overlay.querySelector("#sshModalCancel").addEventListener("click", close);
    overlay.addEventListener("click", (e) => { if (e.target === overlay) close(); });
    overlay.addEventListener("keydown", (e) => { if (e.key === "Escape") close(); });

    overlay.querySelector("#sshModalConnect").addEventListener("click", async () => {
      const name = overlay.querySelector("#sshFldName").value.trim() || "DEVICE";
      const host = overlay.querySelector("#sshFldHost").value.trim();
      const port = parseInt(overlay.querySelector("#sshFldPort").value) || 22;
      const username = overlay.querySelector("#sshFldUser").value.trim() || "corvus";
      const password = overlay.querySelector("#sshFldPass").value || null;
      if (!host) return;
      overlay.remove();
      const res = await fetch("/api/ssh/connect", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name, host, port, username, password }),
      }).then((r) => r.json());
      if (res.ok) renderSSHTerminal(name, host);
      else renderSSHTerminal(name, host, res.error || "Connection failed");
    });

    overlay.querySelector("#sshFldHost").focus();
  }

  function initFuture() {
    futureContent.innerHTML =
      '<div class="future-icon" data-lucide="puzzle"></div>' +
      '<div class="future-title">Future Tools / Plugins</div>' +
      '<div class="future-desc">Hier können zukünftige Funktionen integriert werden.<br><br>' +
      'This panel is the extension point of Corvus GCS — additional modules will plug in here.</div>';
  }

  function toggle() {
    const collapsed = panel.classList.toggle("collapsed");
    panel.dataset.state = collapsed ? "closed" : "open";
    handle.title = collapsed ? "Expand panel" : "Collapse panel";
    handle.querySelector("i, svg")?.remove();
    const i = document.createElement("i");
    i.setAttribute("data-lucide", collapsed ? "chevron-left" : "chevron-right");
    handle.appendChild(i);
    refreshIcons();
    setTimeout(() => window.dispatchEvent(new Event("resize")), 300);
  }

  function init() {
    panel = document.getElementById("rightPanel");
    handle = document.getElementById("panelHandle");
    body = document.getElementById("panelBody");
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
    refreshIcons();
  }

  return { init, toggle, addConsoleLine };
})();
