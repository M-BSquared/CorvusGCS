"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.mcp: this window's half of the MCP server (corvus/mcp.py).

  The server runs in the backend, but three of its jobs need a person or a
  plugin, and both are in this window:

  * ASKING. A tool the operator set to "ask first" waits for an answer. The
    question arrives on the `mcp` event topic and is a dialog here: what the
    AI client wants, in its own words, Allow or Refuse. It closes by itself
    when the backend gives up waiting, or when another window answered.
  * PLUGIN TOOLS. A plugin adds tools in the browser (Corvus.plugins,
    mcpTools). This window tells the backend every 10 s that it is open and
    which tools it can run, so a plugin tool exists exactly while a window
    does. A call arrives as an event, runs the plugin's handler, and the
    answer goes back on POST /api/mcp/result.
  * THE INDICATOR. While an AI client is connected and may change something,
    a pill sits on the map saying so, and it is stronger while the client
    flies the vehicle directly. Its Stop takes every permission that changes
    anything away at once and puts a vehicle under direct control in hold.

  What the server says it did goes to the console, and what it refused to the
  notification board as well, for the operator who had their eyes on the
  aircraft.
*/
Corvus.mcp = (function () {
  const HEARTBEAT_MS = 10000;
  const ANNOUNCE_DEBOUNCE_MS = 300;

  const windowId = "w" + Math.random().toString(36).slice(2, 12);
  let status = null;
  let started = false;
  let heartbeatTimer = null;
  let announceTimer = null;
  let badge = null;
  const listeners = new Set();
  const dialogs = new Map();      // confirmation id -> modal handle

  function req() {
    return window.Corvus && Corvus.telemetry && Corvus.telemetry.requestJson;
  }

  function post(url, body) {
    const request = req();
    if (typeof request !== "function") return Promise.reject(new Error("no transport"));
    return request(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body || {}),
    });
  }

  // ---- plugin tools ----------------------------------------------------

  /** The tool descriptions to announce: every plugin's, without the handlers. */
  function toolList() {
    const plugins = window.Corvus && Corvus.plugins;
    if (!plugins || typeof plugins.mcpTools !== "function") return [];
    try {
      return plugins.mcpTools();
    } catch (err) {
      console.error("mcp: plugin tool list failed:", err);
      return [];
    }
  }

  function announce() {
    clearTimeout(announceTimer);
    announceTimer = null;
    if (!status || !status.enabled) return Promise.resolve(null);
    return post("/api/mcp/window", { id: windowId, tools: toolList() }).catch(() => null);
  }

  /** A plugin's tools changed: tell the backend soon, once. */
  function toolsChanged() {
    if (announceTimer) return;
    announceTimer = setTimeout(announce, ANNOUNCE_DEBOUNCE_MS);
  }

  function runCall(event) {
    if (event.window !== windowId) return;
    const plugins = window.Corvus && Corvus.plugins;
    const reply = (ok, result, error) => post("/api/mcp/result", {
      id: event.id, ok, result: result === undefined ? null : result, error: error || null,
    }).catch(() => {});
    if (!plugins || typeof plugins.runMcpTool !== "function") {
      reply(false, null, "Plugins are not loaded in this window.");
      return;
    }
    Promise.resolve()
      .then(() => plugins.runMcpTool(event.plugin, event.tool, event.args || {}))
      .then((result) => reply(true, result, null),
        (err) => reply(false, null, (err && err.message) || String(err)));
  }

  // ---- asking the operator ------------------------------------------------

  function openConfirm(event) {
    if (dialogs.has(event.id) || !Corvus.ui || typeof Corvus.ui.modal !== "function") return;
    const ui = Corvus.ui;
    const body = document.createElement("div");
    body.className = "mcp-confirm";
    const who = document.createElement("p");
    who.className = "mcp-confirm-who";
    who.textContent = `${event.client || "An AI client"} asks to:`;
    const what = document.createElement("p");
    what.className = "mcp-confirm-what";
    what.textContent = event.summary || event.tool;
    const why = document.createElement("p");
    why.className = "mcp-confirm-why";
    why.textContent = `Permission: ${event.permission_label || event.permission}. ` +
      "Nothing is sent until you allow it.";
    const left = document.createElement("p");
    left.className = "mcp-confirm-left";
    body.append(who, what, why, left);

    let answered = false;
    const answer = (allow) => {
      if (answered) return;
      answered = true;
      post("/api/mcp/confirm", { id: event.id, allow }).catch(() => {});
      handle.close();
    };
    const handle = ui.modal({
      title: "AI client request",
      body,
      size: "sm",
      dismissable: false,
      actions: [
        ui.button({ label: "Refuse", icon: "x", onClick: () => answer(false) }),
        ui.button({ label: "Allow", icon: "check", variant: "primary", onClick: () => answer(true) }),
      ],
      onClose: () => {
        clearInterval(tick);
        dialogs.delete(event.id);
      },
    });
    const deadline = Date.now() + (Number(event.expires_in_s) || 0) * 1000;
    const paint = () => {
      const s = Math.max(0, Math.round((deadline - Date.now()) / 1000));
      left.textContent = `Refused by itself in ${s} s.`;
      if (s <= 0) handle.close();
    };
    const tick = setInterval(paint, 1000);
    paint();
    dialogs.set(event.id, handle);
    handle.open();
  }

  function closeConfirm(id) {
    const handle = dialogs.get(id);
    if (handle) handle.close();
  }

  // ---- the indicator ------------------------------------------------------

  /** Pure: what the pill says for a status, or null for no pill. */
  function badgeState(s) {
    if (!s || !s.enabled) return null;
    if (s.control) {
      const what = s.control.kind === "manual" ? "sticks" : "velocity";
      return { level: "control", text: `AI client flying (${what})` };
    }
    if (s.pending && s.pending.length) return { level: "ask", text: "AI client is asking" };
    if (s.active && s.writes) return { level: "on", text: "AI client connected" };
    return null;
  }

  function paintBadge() {
    const look = badgeState(status);
    if (!look) {
      if (badge) badge.hidden = true;
      return;
    }
    if (!badge) {
      const host = document.getElementById("mapView");
      if (!host) return;
      badge = document.createElement("div");
      badge.className = "mcp-badge";
      badge.setAttribute("role", "status");
      const dot = document.createElement("span");
      dot.className = "mcp-badge-dot";
      const label = document.createElement("span");
      label.className = "mcp-badge-text";
      const stop = Corvus.ui.button({
        label: "Stop", icon: "octagon-x", size: "sm", variant: "danger",
        title: "Take every permission that changes the vehicle away from the AI client, " +
               "and hold a vehicle it flies directly",
        onClick: () => stopNow(stop),
      });
      stop.classList.add("mcp-badge-stop");
      badge.append(dot, label, stop);
      host.appendChild(badge);
      Corvus.ui.refreshIcons();
    }
    badge.hidden = false;
    badge.dataset.level = look.level;
    badge.querySelector(".mcp-badge-text").textContent = look.text;
  }

  function stopNow(btn) {
    if (btn) Corvus.ui.setBusy(btn, true);
    return post("/api/mcp/stop", {})
      .then((res) => { if (res && res.status) setStatus(res.status); })
      .catch((err) => {
        notify("critical", `Stop failed: ${(err && err.message) || err}`);
      })
      .finally(() => { if (btn) Corvus.ui.setBusy(btn, false); });
  }

  // ---- what the server says ----------------------------------------------

  function consoleLine(level, text) {
    try {
      if (Corvus.panel && typeof Corvus.panel.addConsoleLine === "function") {
        Corvus.panel.addConsoleLine(level, text);
      }
    } catch (_e) { /* a console that is not there costs only the line */ }
  }

  function notify(level, message) {
    try {
      window.dispatchEvent(new CustomEvent("corvus:notification", { detail: { level, message } }));
    } catch (_e) { /* no board */ }
  }

  function setStatus(next) {
    const wasEnabled = !!(status && status.enabled);
    status = next || null;
    paintBadge();
    (status && status.pending || []).forEach(openConfirm);
    if (status && status.enabled && !wasEnabled) announce();
    listeners.forEach((fn) => { try { fn(status); } catch (err) { console.error(err); } });
  }

  function onEvent(event) {
    if (!event || typeof event !== "object") return;
    if (event.type === "status") setStatus(event);
    else if (event.type === "confirm") openConfirm(event);
    else if (event.type === "confirm_done") closeConfirm(event.id);
    else if (event.type === "call") runCall(event);
    else if (event.type === "log") {
      const level = ["cmd", "info", "warning", "error", "success"].includes(event.level)
        ? event.level : "";
      consoleLine(level, String(event.text || ""));
      // A warning, never a critical: a refused call is news, not an emergency
      // that pins itself over the panel until it is dismissed.
      if (event.notify) notify("warning", String(event.text || ""));
    }
  }

  // ---- lifecycle ----------------------------------------------------------

  function init() {
    if (started) return;
    started = true;
    if (Corvus.events && typeof Corvus.events.subscribe === "function") {
      Corvus.events.subscribe("mcp", onEvent);
    }
    heartbeatTimer = setInterval(announce, HEARTBEAT_MS);
    window.addEventListener("pagehide", () => clearInterval(heartbeatTimer));
  }

  /**
   * Hear every status the server sends: the settings card keeps its client
   * list and switches current with it. Called at once with the last one known.
   * @param {function(Object|null):void} fn
   * @returns {function():void} unsubscribe
   */
  function subscribe(fn) {
    if (typeof fn !== "function") return () => {};
    listeners.add(fn);
    try { fn(status); } catch (err) { console.error(err); }
    return () => listeners.delete(fn);
  }

  return {
    init, subscribe, toolsChanged, announce, stop: stopNow, setStatus,
    status: () => status, windowId: () => windowId,
    badgeState,
  };
})();
