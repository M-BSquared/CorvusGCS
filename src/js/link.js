"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.link — LINK tab connection manager.
 *
 * Connects a Holybro SiK Telemetry Radio V3 (or any UDP/TCP endpoint) from the
 * UI. Serial ports come from GET /api/mavlink/serial-ports, connections go via
 * POST /api/mavlink/connect and POST /api/mavlink/disconnect. Status reflects
 * the link_status / link_connection / link_error / link_quality fields pushed
 * by the backend over SSE — the frontend never polls live telemetry.
 *
 * Beyond connecting, the panel covers what a field session actually needs:
 *
 *  - DISCONNECT. Previously the only way out of a link was into another one,
 *    so freeing the radio (to hand the aircraft to another GCS, swap a cable,
 *    or stop a reconnect loop hammering a port that moved) meant quitting.
 *  - LINK QUALITY, from the same SSE field the top bar uses. "Connected" alone
 *    does not tell you whether the link is worth flying on.
 *  - RECENT CONNECTIONS, persisted. Retyping udp:0.0.0.0:14540 on every launch
 *    is the kind of friction that gets a laptop closed.
 *  - PRESETS for the endpoints PX4 actually publishes, so the common cases are
 *    a click rather than a remembered string.
 *  - PORT AUTO-REFRESH when the tab is opened, because a radio plugged in
 *    after launch used to require finding the refresh button.
 *
 * On a transition to "connected" the flight-mode selector is refreshed from
 * GET /api/mavlink/modes (via Corvus.app.refreshModes) so the operator only
 * sees modes the connected firmware supports — and only when the list actually
 * changes (idempotent).
 */
Corvus.link = (function () {
  const RECENT_KEY = "corvus.link.recent";
  const MAX_RECENT = 5;

  /* The endpoints PX4 actually publishes, so the common cases are a click.
     Kept short on purpose — a preset list nobody reads is just noise. */
  const PRESETS = [
    { label: "SITL / onboard UDP", conn: "udp:0.0.0.0:14540" },
    { label: "QGC-style UDP", conn: "udp:0.0.0.0:14550" },
    { label: "TCP (SITL)", conn: "tcp:127.0.0.1:5760" },
  ];

  // DOM refs (cached in init)
  let serialSelect, baudSelect, refreshBtn, serialConnectBtn, reconnectBtn;
  let customInput, customConnectBtn;
  let statusDot, statusLabel, statusConn, statusError;
  let disconnectBtn, qualityEl, recentHost, presetHost;

  // Local state
  let inFlight = false;          // a connect POST is currently pending
  let lastStatus = "";           // last seen link_status — for transition detection
  let lastSentConnection = "";   // most recently sent connection string (for Reconnect)
  let recent = [];               // recently used connection strings, newest first

  /** Pure: build a serial connection string `serial:<device>:<baud>`. */
  function buildConnectionString(device, baud) {
    if (!device) return "";
    return `serial:${device}:${baud}`;
  }

  /** Pure: option text for a serial port — "device  —  description" or device only. */
  function portOptionText(port) {
    const device = (port && port.device) || "";
    const description = (port && port.description) || "";
    return description ? `${device}  \u2014  ${description}` : device;
  }

  /** Pure: map a link_status to a {label, dot} using the semantic palette. */
  function statusInfo(linkStatus) {
    switch (String(linkStatus || "disconnected").toLowerCase()) {
      case "connected":    return { label: "CONNECTED",    dot: "healthy" };
      case "connecting":   return { label: "CONNECTING",   dot: "warning" };
      case "reconnecting": return { label: "RECONNECTING", dot: "warning" };
      case "disconnected":
      default:             return { label: "DISCONNECTED", dot: "off" };
    }
  }

  /** Pure: map a link_quality to a {label, level} for the status row.
   *  "Connected" says the socket is up; this says whether it is worth flying
   *  on, which is the question actually being asked. */
  function qualityInfo(linkQuality) {
    switch (String(linkQuality || "unknown").toLowerCase()) {
      case "good": return { label: "GOOD", level: "healthy" };
      case "fair": return { label: "FAIR", level: "warning" };
      case "poor": return { label: "POOR", level: "critical" };
      case "lost": return { label: "LOST", level: "critical" };
      default:     return { label: "", level: "off" };
    }
  }

  // ---- recent connections ----

  function loadRecent() {
    try {
      const raw = JSON.parse(localStorage.getItem(RECENT_KEY) || "[]");
      recent = Array.isArray(raw) ? raw.filter((s) => typeof s === "string" && s).slice(0, MAX_RECENT) : [];
    } catch (_e) {
      recent = [];
    }
  }

  /** Record a connection string as most-recently-used. Case-sensitive and
   *  exact: `serial:/dev/ttyUSB0:57600` and `...:115200` are different links,
   *  and collapsing them would silently reconnect at the wrong baud. */
  function rememberRecent(conn) {
    if (!conn) return;
    recent = [conn].concat(recent.filter((c) => c !== conn)).slice(0, MAX_RECENT);
    try { localStorage.setItem(RECENT_KEY, JSON.stringify(recent)); } catch (_e) {}
    renderRecent();
  }

  function forgetRecent(conn) {
    recent = recent.filter((c) => c !== conn);
    try { localStorage.setItem(RECENT_KEY, JSON.stringify(recent)); } catch (_e) {}
    renderRecent();
  }

  function renderRecent() {
    if (!recentHost) return;
    Corvus.ui.clear(recentHost);
    if (!recent.length) {
      recentHost.hidden = true;
      return;
    }
    recentHost.hidden = false;
    recentHost.appendChild(Corvus.ui.label("Recent"));
    const list = document.createElement("div");
    list.className = "link-recent-list";
    recent.forEach((conn) => {
      const row = document.createElement("div");
      row.className = "link-recent-row";
      const go = document.createElement("button");
      go.type = "button";
      go.className = "link-recent-conn";
      go.textContent = conn;
      go.title = `Connect to ${conn}`;
      go.addEventListener("click", () => doConnect(conn));
      row.appendChild(go);
      row.appendChild(Corvus.ui.iconButton("x", {
        title: "Forget this connection",
        ariaLabel: `Forget ${conn}`,
        size: 12,
        onClick: () => forgetRecent(conn),
      }));
      list.appendChild(row);
    });
    recentHost.appendChild(list);
    Corvus.ui.refreshIcons();
  }

  function renderPresets() {
    if (!presetHost) return;
    Corvus.ui.clear(presetHost);
    PRESETS.forEach((p) => {
      const b = Corvus.ui.button({
        variant: "ghost",
        size: "sm",
        label: p.label,
        title: p.conn,
        onClick: () => {
          // Fill the field rather than connecting outright: a preset is a
          // starting point the operator may want to edit (a different host,
          // a different port) before committing.
          customInput.value = p.conn;
          customInput.focus();
        },
      });
      b.classList.add("link-preset");
      presetHost.appendChild(b);
    });
  }

  function setSelectOptions(select, opts) {
    select.innerHTML = "";
    opts.forEach((o) => {
      const el = document.createElement("option");
      el.value = o.value;
      el.textContent = o.text;
      if (o.selected) el.selected = true;
      select.appendChild(el);
    });
  }

  /** Fetch /api/mavlink/serial-ports and repopulate the dropdown. */
  async function refreshPorts() {
    if (!serialSelect) return;
    const placeholder = serialSelect.querySelector('option[value=""]');
    const current = serialSelect.value;
    try {
      const data = await Corvus.telemetry.requestJson("/api/mavlink/serial-ports");
      const ports = (data && data.ports) || [];
      const opts = [{ value: "", text: placeholder ? placeholder.textContent : "Select serial port\u2026" }];
      ports.forEach((p) => {
        opts.push({ value: p.device, text: portOptionText(p) });
      });
      setSelectOptions(serialSelect, opts);
      // Auto-select the first port when exactly one real port is available
      // and the operator hasn't chosen one (common case: a single radio plugged in).
      if (ports.length === 1 && !serialSelect.value) {
        serialSelect.value = ports[0].device;
      } else if (current && opts.some((o) => o.value === current)) {
        serialSelect.value = current;
      }
      // Surface a backend enumeration error (e.g. permission denied) without
      // blocking the empty dropdown.
      if (data && data.error) {
        showError(data.error);
      } else {
        showError("");
      }
    } catch (err) {
      // Keep the placeholder; show the error in the status row.
      setSelectOptions(serialSelect, [{ value: "", text: "Select serial port\u2026" }]);
      showError(err && err.message ? err.message : "Could not list serial ports");
    }
  }

  function showError(text) {
    if (!statusError) return;
    if (text) {
      statusError.textContent = String(text);
      statusError.hidden = false;
    } else {
      statusError.textContent = "";
      statusError.hidden = true;
    }
  }

  /** POST a connection string and update local state. */
  async function doConnect(conn) {
    if (!conn) return;
    if (inFlight) return;
    inFlight = true;
    setBusy(true);
    lastSentConnection = conn;
    try {
      await Corvus.telemetry.connectMavlink(conn);
      // Only remembered once the backend accepted it — a string it rejected
      // is not worth offering back on the next launch.
      rememberRecent(conn);
      // The backend starts the bridge and reports status over SSE; the
      // subscriber will update the buttons + status row from link_status.
    } catch (err) {
      showError(err && err.message ? err.message : "Connection failed");
    } finally {
      inFlight = false;
      setBusy(false);
      updateFromState(Corvus.telemetry.getState() || {});
    }
  }

  function connectSerial() {
    const device = serialSelect.value;
    const baud = baudSelect.value || "57600";
    if (!device) {
      showError("Select a serial port first.");
      return;
    }
    showError("");
    doConnect(buildConnectionString(device, baud));
  }

  function connectCustom() {
    const conn = (customInput.value || "").trim();
    if (!conn) {
      showError("Enter a connection string (e.g. udp:0.0.0.0:14540).");
      return;
    }
    showError("");
    doConnect(conn);
  }

  /** Close the link and leave it closed. Frees the radio without quitting. */
  async function disconnect() {
    if (inFlight) return;
    inFlight = true;
    setBusy(true);
    try {
      await Corvus.telemetry.postAction("/api/mavlink/disconnect", {});
      showError("");
    } catch (err) {
      showError(err && err.message ? err.message : "Disconnect failed");
    } finally {
      inFlight = false;
      setBusy(false);
      updateFromState(Corvus.telemetry.getState() || {});
    }
  }

  function reconnect() {
    // Re-POST the connection currently in use (or the last one we sent).
    const s = Corvus.telemetry.getState() || {};
    const conn = s.link_connection || lastSentConnection;
    if (conn) doConnect(conn);
  }

  /** Disable the connect buttons while a request is in flight. */
  function setBusy(busy) {
    if (serialConnectBtn) serialConnectBtn.disabled = busy;
    if (customConnectBtn) customConnectBtn.disabled = busy;
    if (reconnectBtn) reconnectBtn.disabled = busy;
    if (disconnectBtn) disconnectBtn.disabled = busy;
  }

  /** Reflect link_status into the status row + connect button labels. */
  function updateFromState(state) {
    const status = String(state.link_status || "disconnected").toLowerCase();
    const info = statusInfo(status);

    if (statusDot) {
      statusDot.className = "link-status-dot " + info.dot;
    }
    if (statusLabel) statusLabel.textContent = info.label;

    if (statusConn) {
      const conn = state.link_connection || "";
      statusConn.textContent = conn || "No connection";
      statusConn.hidden = !conn;
    }

    // Backend link_error wins over any locally-shown error.
    if (state.link_error) {
      showError(state.link_error);
    } else if (status !== "reconnecting") {
      // Clear a stale error once we're no longer reconnecting, unless a local
      // doConnect error is still showing (inFlight just ended). Keep it simple:
      // only auto-clear when disconnected/connected.
      if (status === "connected" || status === "disconnected") showError("");
    }

    // Button affordances per the task spec:
    //  - connecting  -> disabled, "Connecting\u2026"
    //  - connected   -> disabled "Connected" + enabled "Reconnect"
    //  - otherwise   -> enabled "Connect", Reconnect hidden
    const busy = inFlight || status === "connecting";
    if (serialConnectBtn) {
      serialConnectBtn.disabled = busy || status === "connected";
      const span = serialConnectBtn.querySelector("span");
      if (span) {
        span.textContent = status === "connecting" ? "Connecting\u2026"
          : status === "connected" ? "Connected"
          : "Connect";
      }
    }
    if (customConnectBtn) {
      customConnectBtn.disabled = busy || status === "connected";
      const span = customConnectBtn.querySelector("span");
      if (span) {
        span.textContent = status === "connecting" ? "Connecting\u2026"
          : status === "connected" ? "Connected"
          : "Connect";
      }
    }
    if (reconnectBtn) {
      const showReconnect = status === "connected" && !inFlight;
      reconnectBtn.hidden = !showReconnect;
      reconnectBtn.disabled = !showReconnect;
    }
    // Disconnect is offered whenever there is something to close — including
    // while connecting or reconnecting, which is exactly when an operator
    // wants to stop a retry loop against a port that has moved.
    if (disconnectBtn) {
      const canDisconnect = status !== "disconnected" && !inFlight;
      disconnectBtn.hidden = !canDisconnect;
      disconnectBtn.disabled = !canDisconnect;
    }
    // Link quality only means something on a live link.
    if (qualityEl) {
      const q = status === "connected" ? qualityInfo(state.link_quality) : { label: "", level: "off" };
      qualityEl.textContent = q.label;
      qualityEl.dataset.level = q.level;
      qualityEl.hidden = !q.label;
    }

    // Transition to "connected" -> refresh flight modes (idempotent).
    if (status === "connected" && lastStatus !== "connected") {
      if (Corvus.app && typeof Corvus.app.refreshModes === "function") {
        Corvus.app.refreshModes();
      }
    }
    lastStatus = status;
  }

  function init() {
    serialSelect = document.getElementById("linkSerialPort");
    baudSelect = document.getElementById("linkBaud");
    refreshBtn = document.getElementById("linkRefreshPorts");
    serialConnectBtn = document.getElementById("linkSerialConnect");
    reconnectBtn = document.getElementById("linkReconnect");
    customInput = document.getElementById("linkCustomConn");
    customConnectBtn = document.getElementById("linkCustomConnect");
    statusDot = document.getElementById("linkStatusDot");
    statusLabel = document.getElementById("linkStatusLabel");
    statusConn = document.getElementById("linkStatusConn");
    statusError = document.getElementById("linkStatusError");
    disconnectBtn = document.getElementById("linkDisconnect");
    qualityEl = document.getElementById("linkQuality");
    recentHost = document.getElementById("linkRecent");
    presetHost = document.getElementById("linkPresets");

    loadRecent();
    renderRecent();
    renderPresets();

    if (disconnectBtn) disconnectBtn.addEventListener("click", disconnect);
    if (refreshBtn) refreshBtn.addEventListener("click", refreshPorts);
    if (serialConnectBtn) serialConnectBtn.addEventListener("click", connectSerial);
    if (customConnectBtn) customConnectBtn.addEventListener("click", connectCustom);
    if (reconnectBtn) reconnectBtn.addEventListener("click", reconnect);

    // Custom connect on Enter.
    if (customInput) {
      customInput.addEventListener("keydown", (e) => {
        if (e.key === "Enter") { e.preventDefault(); connectCustom(); }
      });
    }

    // One-shot config fetches (NOT live-telemetry polling — allowed).
    refreshPorts();

    // Re-enumerate when the LINK tab is opened. A radio plugged in after
    // launch used to need the operator to find the refresh button; this is
    // still event-driven, not a poll.
    const linkTab = document.querySelector('.panel-tabs .tab[data-tab="link"]');
    if (linkTab) linkTab.addEventListener("click", refreshPorts);

    // Status comes from the telemetry SSE stream.
    Corvus.telemetry.subscribe(updateFromState);
    // Seed the status row from the current state immediately.
    updateFromState(Corvus.telemetry.getState() || {});
  }

  return {
    init,
    // Exposed for unit tests:
    buildConnectionString,
    portOptionText,
    statusInfo,
    qualityInfo,
    PRESETS,
  };
})();
