"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.link — LINK tab connection manager.
 *
 * Lets the operator connect a Holybro SiK Telemetry Radio V3 (or any UDP/TCP
 * endpoint) from the UI. Serial ports come from GET /api/mavlink/serial-ports,
 * connections go via POST /api/mavlink/connect (re-using
 * Corvus.telemetry.connectMavlink). Status reflects the link_status /
 * link_connection / link_error fields pushed by the backend over SSE — the
 * frontend never polls live telemetry.
 *
 * On a transition to "connected" the flight-mode selector is refreshed from
 * GET /api/mavlink/modes (via Corvus.app.refreshModes) so the operator only
 * sees modes the connected firmware supports — and only when the list
 * actually changes (idempotent).
 */
Corvus.link = (function () {
  // DOM refs (cached in init)
  let serialSelect, baudSelect, refreshBtn, serialConnectBtn, reconnectBtn;
  let customInput, customConnectBtn;
  let statusDot, statusLabel, statusConn, statusError;

  // Local state
  let inFlight = false;          // a connect POST is currently pending
  let lastStatus = "";           // last seen link_status — for transition detection
  let lastSentConnection = "";   // most recently sent connection string (for Reconnect)

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
  };
})();
