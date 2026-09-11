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
 *  - RECENT CONNECTIONS, persisted, at the top of the panel. Retyping
 *    udp:0.0.0.0:14540 on every launch is the kind of friction that gets a
 *    laptop closed. The newest entry doubles as the panel's memory: the card
 *    reopens on the kind, port and baud that last worked.
 *  - PRESETS for the endpoints PX4 actually publishes, so the common cases are
 *    a click rather than a remembered string.
 *  - PORT AUTO-REFRESH when the tab is opened, because a radio plugged in
 *    after launch used to require finding the refresh button. A port that was
 *    there last time and is not there now stays on the list, named as absent:
 *    dropping it silently answers "where did my radio go?" with an empty box.
 *
 * Serial and network share ONE card and ONE Connect button. Two cards with two
 * primary buttons read as two links that could both be open, and there is only
 * ever one — the branch not in use was permanent dead weight above the fold.
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
     Kept short on purpose — a preset list nobody reads is just noise.

     Each carries its own one-line `note`. The connection string alone says
     which port, never which of these four situations the operator is in, and
     picking the wrong one fails as a silent absence of telemetry rather than
     as an error. The note is the difference between reading the list and
     guessing at it. */
  const PRESETS = [
    {
      label: "SITL / GCS UDP",
      conn: "udp:0.0.0.0:14550",
      note: "Waits on the usual ground-station port. Start here — SITL and most telemetry radios send to it by default.",
    },
    // 14540 is PX4's onboard link — the one MAVSDK and MAVROS bind. Listed
    // second, and labelled for what actually lives there, because taking it
    // silently costs a companion process on the same machine its telemetry.
    {
      label: "PX4 onboard UDP",
      conn: "udp:0.0.0.0:14540",
      note: "PX4's companion-computer port. MAVROS and MAVSDK bind it too — taking it can leave them without telemetry.",
    },
    // A mavlink-router UdpEndpoint in Server mode binds and waits for the
    // station to speak first, so this one dials out rather than listening.
    {
      // Named for the program, not the mode: "(server mode)" wrapped the row
      // onto a second line, and the note says it anyway.
      label: "mavlink-router",
      conn: "udpout:127.0.0.1:14550",
      note: "Dials out instead of waiting, because a router in server mode holds the port and expects the station to speak first.",
    },
    {
      label: "TCP (SITL)",
      conn: "tcp:127.0.0.1:5760",
      note: "Connects to a simulator on this machine over TCP — the port jMAVSim and Gazebo serve.",
    },
  ];

  // DOM refs (cached in init)
  let fwdEnabled, fwdPort, fwdAllow, fwdWarning, fwdError, fwdStats, fwdHint;
  let fwdInFlight = false;
  let serialSelect, baudSelect, refreshBtn, connectBtn, reconnectBtn;
  let customInput, portHint;
  let kindSerialBtn, kindNetBtn, serialFields, netFields;
  let statusDot, statusLabel, statusConn, statusError;
  let disconnectBtn, qualityEl, recentHost, presetHost;

  // Local state
  let inFlight = false;          // a connect POST is currently pending
  let lastStatus = "";           // last seen link_status — for transition detection
  let lastSentConnection = "";   // most recently sent connection string (for Reconnect)
  let recent = [];               // recently used connection strings, newest first
  let kind = "serial";           // which branch of the card is showing
  let wantDevice = "";           // serial port to re-select once the list arrives
  let missingDevices = [];       // devices offered but not currently enumerated

  /** Pure: build a serial connection string `serial:<device>:<baud>`. */
  function buildConnectionString(device, baud) {
    if (!device) return "";
    return `serial:${device}:${baud}`;
  }

  /** Pure: split a connection string back into the fields that built it.
   *  A serial string is cut at the LAST colon: `COM3` has none and a POSIX
   *  device is a path, so neither end may swallow the baud. */
  function parseConnection(conn) {
    const s = String(conn || "").trim();
    if (!s) return null;
    const m = /^serial:(.+):(\d+)$/.exec(s);
    if (m) return { kind: "serial", device: m[1], baud: m[2], conn: s };
    return { kind: "net", device: "", baud: "", conn: s };
  }

  /** Pure: how a stored connection reads in the recent list. The raw string is
   *  what gets sent, but `serial:/dev/tty.usbserial-0001:57600` is a poor
   *  thing to scan a list of — the parts are what the operator recognises. */
  function describeConnection(conn) {
    const p = parseConnection(conn);
    if (!p) return { icon: "plug", label: "", detail: "" };
    if (p.kind === "serial") {
      return { icon: "cable", label: p.device, detail: `${p.baud} baud` };
    }
    const t = /^(udpout|udpin|udp|tcpout|tcpin|tcp):(.+)$/i.exec(p.conn);
    if (t) return { icon: "network", label: t[2], detail: t[1].toLowerCase() };
    return { icon: "network", label: p.conn, detail: "" };
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
      const d = describeConnection(conn);
      const go = document.createElement("button");
      go.type = "button";
      go.className = "link-recent-conn";
      go.appendChild(Corvus.ui.icon(d.icon, 13));
      const label = document.createElement("span");
      label.className = "link-recent-label";
      label.textContent = d.label;
      go.appendChild(label);
      if (d.detail) {
        const detail = document.createElement("span");
        detail.className = "link-recent-detail";
        detail.textContent = d.detail;
        go.appendChild(detail);
      }
      // The raw string is still what is sent, so it is still what the title
      // shows — the row is a nicer rendering of it, not a different thing.
      go.title = `Connect to ${conn}`;
      // Fill the card as well as connecting: the operator should be able to
      // see, after the click, exactly which port and baud is being opened.
      go.addEventListener("click", () => { applyConnection(conn); doConnect(conn); });
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

  /* ---- which branch of the card is showing ---- */

  /** Show one branch and hide the other. The Connect button belongs to both. */
  function setKind(next) {
    kind = next === "net" ? "net" : "serial";
    const isNet = kind === "net";
    if (serialFields) serialFields.hidden = isNet;
    if (netFields) netFields.hidden = !isNet;
    if (kindSerialBtn) kindSerialBtn.setAttribute("aria-selected", String(!isNet));
    if (kindNetBtn) kindNetBtn.setAttribute("aria-selected", String(isNet));
  }

  /** Point the card at a connection string: the right branch, and its fields
   *  filled in. Used to restore the last link on launch and to show what a
   *  recent entry actually is when it is clicked. */
  function applyConnection(conn) {
    const p = parseConnection(conn);
    if (!p) return;
    if (p.kind === "serial") {
      wantDevice = p.device;
      if (serialSelect) {
        // The port list arrives asynchronously; select it now if it is already
        // there, and refreshPorts() will honour wantDevice when it is not.
        serialSelect.value = p.device;
      }
      // Only a baud the dropdown actually offers — assigning an absent value
      // to a <select> clears it, which would silently connect at 57600.
      if (baudSelect && p.baud && baudSelect.querySelector(`option[value="${p.baud}"]`)) {
        baudSelect.value = p.baud;
      }
      setKind("serial");
    } else {
      if (customInput) customInput.value = p.conn;
      setKind("net");
    }
  }

  /* One row per preset: name, what it is for, and the string it fills in.
     They were bare ghost buttons, which put four unexplained phrases under
     the field and left the Connect button below them looking like part of the
     same list. Rows read as the pick-one control they are. */
  function renderPresets() {
    if (!presetHost) return;
    Corvus.ui.clear(presetHost);
    presetHost.appendChild(Corvus.ui.label("Common endpoints"));
    const list = document.createElement("div");
    list.className = "link-preset-list";
    PRESETS.forEach((p) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "link-preset";
      b.title = `${p.conn} — ${p.note}`;

      const head = document.createElement("span");
      head.className = "link-preset-head";
      const name = document.createElement("span");
      name.className = "link-preset-name";
      name.textContent = p.label;
      const conn = document.createElement("span");
      conn.className = "link-preset-conn";
      conn.textContent = p.conn;
      head.appendChild(name);
      head.appendChild(conn);

      const note = document.createElement("span");
      note.className = "link-preset-note";
      note.textContent = p.note;

      b.appendChild(head);
      b.appendChild(note);
      b.addEventListener("click", () => {
        // Fill the field rather than connecting outright: a preset is a
        // starting point the operator may want to edit (a different host,
        // a different port) before committing.
        customInput.value = p.conn;
        customInput.focus();
      });
      list.appendChild(b);
    });
    presetHost.appendChild(list);
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
    const current = serialSelect.value || wantDevice;
    try {
      const data = await Corvus.telemetry.requestJson("/api/mavlink/serial-ports");
      const ports = (data && data.ports) || [];
      const opts = [{ value: "", text: placeholder ? placeholder.textContent : "Select serial port\u2026" }];
      ports.forEach((p) => {
        opts.push({ value: p.device, text: portOptionText(p) });
      });
      // The port the operator last flew stays on the list even when it is not
      // enumerated, named as absent. Dropping it answers "where did my radio
      // go?" with an empty dropdown; keeping it answers with the cable.
      missingDevices = [];
      if (current && !ports.some((prt) => prt.device === current)) {
        missingDevices = [current];
        opts.push({ value: current, text: `${current}  \u2014  not connected` });
      }
      setSelectOptions(serialSelect, opts);
      // Auto-select the first port when exactly one real port is available
      // and the operator hasn't chosen one (common case: a single radio plugged in).
      if (ports.length === 1 && !current) {
        serialSelect.value = ports[0].device;
      } else if (current) {
        serialSelect.value = current;
      }
      paintPortHint();
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
      missingDevices = [];
      paintPortHint();
      showError(err && err.message ? err.message : "Could not list serial ports");
    }
  }

  /** Pure enough: the sentence under the port dropdown for an absent device. */
  function missingPortHint(device) {
    return `${device} is not connected. Plug it in, then press refresh.`;
  }

  /** The operator picking a port by hand replaces the remembered one — including
   *  picking the placeholder, which is how an absent port is dismissed. */
  function onPortChange() {
    wantDevice = serialSelect ? serialSelect.value : "";
    paintPortHint();
  }

  function paintPortHint() {
    if (!portHint) return;
    const device = serialSelect ? serialSelect.value : "";
    const absent = !!device && missingDevices.indexOf(device) >= 0;
    portHint.textContent = absent ? missingPortHint(device) : "";
    portHint.hidden = !absent;
  }

  function showError(text) {
    if (!statusError) return;
    if (text) {
      statusError.textContent = String(text);
      // The line is clamped to two rows so a long backend message cannot grow
      // the card; the title keeps the whole of it reachable.
      statusError.title = String(text);
      statusError.hidden = false;
    } else {
      statusError.textContent = "";
      statusError.title = "";
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

  /** The one Connect button, dispatched to whichever branch is showing. */
  function connect() {
    if (kind === "net") connectCustom();
    else connectSerial();
  }

  function connectSerial() {
    const device = serialSelect.value;
    const baud = baudSelect.value || "57600";
    if (!device) {
      showError("Select a serial port first.");
      return;
    }
    // Refusing here rather than letting the backend fail on open(): the cause
    // is a cable, and the operator should read that instead of an errno.
    if (missingDevices.indexOf(device) >= 0) {
      showError(missingPortHint(device));
      return;
    }
    showError("");
    wantDevice = device;
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
    if (connectBtn) connectBtn.disabled = busy;
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
      // Ellipsised on one line — a serial path is long enough to wrap three
      // times — with the full string on hover.
      statusConn.title = conn;
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
    if (connectBtn) {
      connectBtn.disabled = busy || status === "connected";
      const span = connectBtn.querySelector("span");
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
    connectBtn = document.getElementById("linkConnect");
    reconnectBtn = document.getElementById("linkReconnect");
    customInput = document.getElementById("linkCustomConn");
    portHint = document.getElementById("linkPortHint");
    kindSerialBtn = document.getElementById("linkKindSerial");
    kindNetBtn = document.getElementById("linkKindNet");
    serialFields = document.getElementById("linkSerialFields");
    netFields = document.getElementById("linkNetFields");
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
    // The panel opens on the link that last worked — kind, port and baud. Only
    // connections the backend accepted are remembered (see rememberRecent), so
    // this restores a setting that is known to have flown, not a typo.
    setKind("serial");
    if (recent.length) applyConnection(recent[0]);

    if (disconnectBtn) disconnectBtn.addEventListener("click", disconnect);
    if (refreshBtn) refreshBtn.addEventListener("click", refreshPorts);
    if (connectBtn) connectBtn.addEventListener("click", connect);
    if (reconnectBtn) reconnectBtn.addEventListener("click", reconnect);
    if (kindSerialBtn) kindSerialBtn.addEventListener("click", () => setKind("serial"));
    if (kindNetBtn) kindNetBtn.addEventListener("click", () => setKind("net"));
    if (serialSelect) serialSelect.addEventListener("change", onPortChange);

    // Custom connect on Enter.
    if (customInput) {
      customInput.addEventListener("keydown", (e) => {
        if (e.key === "Enter") { e.preventDefault(); connectCustom(); }
      });
    }

    fwdEnabled = document.getElementById("fwdEnabled");
    fwdPort = document.getElementById("fwdPort");
    fwdAllow = document.getElementById("fwdAllowCommands");
    fwdWarning = document.getElementById("fwdCommandWarning");
    fwdError = document.getElementById("fwdError");
    fwdStats = document.getElementById("fwdStats");
    fwdHint = document.getElementById("fwdHint");
    if (fwdEnabled) {
      fwdEnabled.addEventListener("change", saveForwarding);
      fwdAllow.addEventListener("change", saveForwarding);
      fwdPort.addEventListener("change", () => {
        if (fwdEnabled.checked) saveForwarding();
      });
    }

    // One-shot config fetches (NOT live-telemetry polling — allowed).
    refreshPorts();
    loadForwarding();

    // Re-enumerate when the LINK tab is opened. A radio plugged in after
    // launch used to need the operator to find the refresh button; this is
    // still event-driven, not a poll.
    const linkTab = document.querySelector('.panel-tabs .tab[data-tab="link"]');
    if (linkTab) {
      linkTab.addEventListener("click", refreshPorts);
      linkTab.addEventListener("click", loadForwarding);
    }

    // Status comes from the telemetry SSE stream.
    Corvus.telemetry.subscribe(updateFromState);
    // Seed the status row from the current state immediately.
    updateFromState(Corvus.telemetry.getState() || {});
  }

  /* ---------------- sharing the link with a second station ----------------
     One serial port, one program: running QGroundControl next to Corvus used
     to mean closing one of them. Corvus keeps the link and mirrors it over
     UDP. The commanding switch is separate and stays off until an operator
     turns it on — two stations that can both arm one aircraft is a decision
     only the operator can make, so it is never implied by the first switch. */

  /** Pure: the sentence under the port field for a given forwarding status.
   *
   *  Two addresses matter and they are not the same one. `host:port` is where
   *  the other station listens — the UDP link QGroundControl opens by itself —
   *  and Corvus mirrors there from the first frame, so nothing has to be
   *  configured on that end. `listen_host:listen_port` is Corvus' own socket,
   *  which is only worth naming when a station has to dial in to it. */
  function forwardingHint(status) {
    const s = status || {};
    const where = `${s.host || "127.0.0.1"}:${s.port || 14550}`;
    if (!s.running) {
      return `Mirrors this link to ${where} — the UDP link QGroundControl`
        + " opens on its own.";
    }
    // Both stations transmitting under one MAVLink system id makes the
    // autopilot report packet loss that is not happening, so it is said where
    // the operator is already looking rather than only in the log.
    if (s.sysid_conflict) {
      return "The other station is using Corvus' MAVLink system ID (254). Give"
        + " it its own — in QGroundControl, Application Settings → MAVLink →"
        + " Ground Station system ID.";
    }
    const peers = (s.peers || []).length;
    const back = s.listen_port
      ? ` Corvus answers on ${s.listen_host || "127.0.0.1"}:${s.listen_port}.`
      : "";
    if (!peers) {
      return `Mirroring to ${where} — nothing has answered yet.` + back;
    }
    return `${peers} station(s) connected; mirroring to ${where}.` + back;
  }

  function paintForwarding(status) {
    if (!fwdEnabled) return;
    const s = status || {};
    fwdEnabled.checked = !!s.running;
    fwdAllow.checked = !!s.allow_commands;
    if (document.activeElement !== fwdPort && s.port) fwdPort.value = s.port;
    fwdWarning.hidden = !s.allow_commands;
    fwdHint.textContent = forwardingHint(s);
    // A notice is the forwarder saying it is running but not quite where it
    // was asked to — a busy listen port it fell back from. Shown, because a
    // station configured to dial into the old one needs to know; not styled
    // as an error, because nothing failed.
    fwdError.textContent = s.error || s.notice || "";
    fwdError.hidden = !(s.error || s.notice);
    fwdError.classList.toggle("link-status-notice", !s.error && !!s.notice);
    const sent = s.frames_sent || 0;
    const injected = s.frames_injected || 0;
    fwdStats.hidden = !s.running || !(sent || injected);
    fwdStats.textContent = `${sent} frame(s) mirrored`
      + (s.allow_commands ? `, ${injected} received from the other station` : "");
  }

  async function loadForwarding() {
    if (!fwdEnabled) return;
    try {
      paintForwarding(await Corvus.telemetry.requestJson("/api/forwarding"));
    } catch (err) {
      console.error("forwarding status failed:", err);
    }
  }

  async function saveForwarding() {
    if (!fwdEnabled || fwdInFlight) return;
    fwdInFlight = true;
    const port = parseInt(fwdPort.value, 10);
    const payload = {
      enabled: fwdEnabled.checked,
      allow_commands: fwdAllow.checked,
    };
    if (port > 0 && port < 65536) payload.port = port;
    // Repaint the warning immediately: the operator has just ticked the box
    // that lets another station arm the aircraft, and the consequence should
    // not wait on a round trip.
    fwdWarning.hidden = !fwdAllow.checked;
    try {
      const res = await Corvus.telemetry.postAction("/api/forwarding", payload);
      if (res && res.status) paintForwarding(res.status);
      if (res && res.error) { fwdError.textContent = res.error; fwdError.hidden = false; }
    } catch (err) {
      fwdError.textContent = String((err && err.message) || err);
      fwdError.hidden = false;
    } finally {
      fwdInFlight = false;
      loadForwarding();
    }
  }

  return {
    init,
    // Exposed for unit tests:
    buildConnectionString,
    parseConnection,
    describeConnection,
    portOptionText,
    statusInfo,
    qualityInfo,
    forwardingHint,
    PRESETS,
  };
})();
