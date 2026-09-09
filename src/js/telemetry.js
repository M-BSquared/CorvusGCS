"use strict";
window.Corvus = window.Corvus || {};

Corvus.telemetry = (function () {
  const subscribers = new Set();
  let state = null;
  let eventSource = null;
  let notifyScheduled = false;
  let connectionGeneration = 0;
  let streamErrorReported = false;
  let parseErrorReported = false;

  function initState() {
    state = {
      connected: false,
      armed: false,
      // Autopilot preflight verdict: true = would arm, false = refusing,
      // null = this firmware does not report it. See topbar readiness().
      prearm_ok: null,
      vehicle_type: "",
      autopilot: "",
      mode: "",
      px4_version: "",
      position: [0, 0],
      altitude_amsl: 0,
      altitude_agl: 0,
      heading: 0,
      groundspeed: 0,
      airspeed: 0,
      vspeed: 0,
      pitch: 0,
      roll: 0,
      yaw: 0,
      battery_percent: 0,
      battery_voltage: 0,
      battery_current: 0,
      gps_fix: "",
      gps_satellites: 0,
      gps_hdop: 99,
      uplink: 0,
      time: "",
      warnings: [],
      home: [0, 0],
      mission: [],
    };
  }

  function notifySubscribers() {
    if (notifyScheduled) return;
    notifyScheduled = true;
    const schedule = window.requestAnimationFrame || ((callback) => window.setTimeout(callback, 0));
    schedule(() => {
      notifyScheduled = false;
      subscribers.forEach((fn) => fn(state));
    });
  }

  function publishTransportError(message) {
    window.dispatchEvent(new CustomEvent("corvus:notification", {
      detail: { level: "critical", message },
    }));
  }

  function connect() {
    initState();
    notifySubscribers();
    if (eventSource) eventSource.close();
    const generation = ++connectionGeneration;
    streamErrorReported = false;
    parseErrorReported = false;
    eventSource = new EventSource("/api/telemetry");
    eventSource.addEventListener("state", (e) => {
      if (generation !== connectionGeneration) return;
      try {
        state = JSON.parse(e.data);
        streamErrorReported = false;
        parseErrorReported = false;
        notifySubscribers();
      } catch (err) {
        console.error("telemetry parse error:", err);
        if (!parseErrorReported) {
          parseErrorReported = true;
          publishTransportError("Invalid telemetry data received");
        }
      }
    });
    eventSource.addEventListener("ping", () => {});
    eventSource.onerror = () => {
      if (generation !== connectionGeneration) return;
      if (state.connected !== false) {
        state.connected = false;
        notifySubscribers();
      }
      if (!streamErrorReported) {
        streamErrorReported = true;
        publishTransportError("Telemetry connection interrupted; reconnecting");
      }
    };
  }

  /* ---------------- console / STATUSTEXT bus ----------------
     One EventSource for /api/console/stream, shared by every consumer and
     reference-counted, because the browser caps concurrent HTTP/1.1 streams per
     origin at six and this app already runs telemetry, params, tiles and
     firmware streams alongside it. The MAVLink console panel and the
     calibration wizard both need the same ordered STATUSTEXT feed; a second
     connection for the second consumer is exactly the kind of thing that
     silently stalls a stream in the field. */
  const consoleSubs = new Set();
  let consoleSource = null;
  let consoleGeneration = 0;
  let consoleInterrupted = false;

  function publishConsoleEntry(entry) {
    consoleSubs.forEach((fn) => {
      try { fn(entry); } catch (err) { console.error("console subscriber failed:", err); }
    });
  }

  function openConsoleStream() {
    if (consoleSource) return;
    const generation = ++consoleGeneration;
    consoleInterrupted = false;
    consoleSource = new EventSource("/api/console/stream");
    consoleSource.addEventListener("open", () => {
      if (generation !== consoleGeneration) return;
      if (consoleInterrupted) {
        publishConsoleEntry({
          name: "GCS", level: "success", text: "Console stream reconnected.",
        });
        consoleInterrupted = false;
      }
    });
    consoleSource.addEventListener("message", (e) => {
      if (generation !== consoleGeneration) return;
      let entry;
      try {
        entry = JSON.parse(e.data);
      } catch (err) {
        console.error("console SSE parse:", err);
        return;
      }
      if (!entry || entry.name === "ping") return;
      publishConsoleEntry(entry);
    });
    consoleSource.onerror = () => {
      if (generation !== consoleGeneration || consoleInterrupted) return;
      consoleInterrupted = true;
      publishConsoleEntry({
        name: "GCS", level: "warning",
        text: "Console stream interrupted; reconnecting…",
      });
    };
  }

  function closeConsoleStream() {
    if (!consoleSource) return;
    consoleGeneration++;
    consoleSource.close();
    consoleSource = null;
    consoleInterrupted = false;
  }

  /**
   * Subscribe to console entries `{ts, name, text, level}`. The stream opens on
   * the first subscriber and closes when the last one unsubscribes, so a
   * transient consumer (the calibration wizard) leaves no socket behind.
   * @param {function(Object):void} fn
   * @returns {function():void} unsubscribe — idempotent.
   */
  function subscribeConsole(fn) {
    if (typeof fn !== "function") return function () {};
    consoleSubs.add(fn);
    openConsoleStream();
    let released = false;
    return function () {
      if (released) return;
      released = true;
      consoleSubs.delete(fn);
      if (consoleSubs.size === 0) closeConsoleStream();
    };
  }

  function getState() { return state; }

  function subscribe(fn) {
    subscribers.add(fn);
    if (state) fn(state);
    return () => subscribers.delete(fn);
  }

  async function requestJson(url, options = {}) {
    let response;
    try {
      response = await fetch(url, options);
    } catch (error) {
      throw new Error(`Network request failed: ${error.message}`);
    }
    let data;
    try {
      data = await response.json();
    } catch (_error) {
      throw new Error(`Invalid response from ${url}`);
    }
    if (!response.ok) throw new Error(data.error || `Request failed (${response.status})`);
    return data;
  }

  async function postAction(url, payload) {
    const data = await requestJson(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
    if (data.ok !== true) throw new Error(data.error || "Command rejected by vehicle");
    return data;
  }

  async function sendCommand(command) {
    return requestJson("/api/console/command", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ command }),
    });
  }

  async function arm(armTrue = true) {
    return postAction("/api/mavlink/arm", { arm: armTrue });
  }

  async function setMode(mode) {
    return postAction("/api/mavlink/mode", { mode });
  }

  async function connectMavlink(conn) {
    return postAction("/api/mavlink/connect", { connection: conn });
  }

  return {
    connect, getState, subscribe, subscribeConsole, requestJson, postAction,
    sendCommand, arm, setMode, connectMavlink,
  };
})();
