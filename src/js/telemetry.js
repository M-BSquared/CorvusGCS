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
      // The two answers to "how much is left", and which one battery_percent
      // above is carrying. -1 means nobody has said — an autopilot that
      // publishes no estimate is not a flat pack. See corvus/battery.py.
      battery_percent_fc: -1,
      battery_percent_est: -1,
      battery_source: "autopilot",
      battery_cells: 0,
      battery_cell_voltage: 0,
      battery_cell_voltages: [],
      battery_consumed_mah: 0,
      battery_temperature: null,
      battery_time_remaining: 0,
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
      // Guarded exactly like publishConsoleEntry below. Set.forEach does not
      // catch, so one throwing subscriber used to abort the loop and leave
      // every panel registered after it frozen on its last value — for this
      // frame and every frame after, because the same one throws every time.
      // Stale instruments that still look live is the worst failure a ground
      // station has; a broken panel must cost only itself.
      subscribers.forEach((fn) => {
        try { fn(state); } catch (err) { console.error("telemetry subscriber failed:", err); }
      });
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
    // The shared /api/events stream (js/events.js), not a connection of its
    // own. The reference counting below still decides who is delivered to; the
    // connection is one of three the whole app now holds instead of six.
    const offMessage = Corvus.events.subscribe("console", (entry) => {
      if (generation !== consoleGeneration) return;
      if (!entry || entry.name === "ping") return;
      if (consoleInterrupted) {
        consoleInterrupted = false;
        publishConsoleEntry({
          name: "GCS", level: "success", text: "Console stream reconnected.",
        });
      }
      publishConsoleEntry(entry);
    });
    const offError = Corvus.events.subscribe("error", () => {
      if (generation !== consoleGeneration || consoleInterrupted) return;
      consoleInterrupted = true;
      publishConsoleEntry({
        name: "GCS", level: "warning",
        text: "Console stream interrupted; reconnecting…",
      });
    });
    consoleSource = function () { offMessage(); offError(); };
  }

  function closeConsoleStream() {
    if (!consoleSource) return;
    consoleGeneration++;
    consoleSource();
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
    // The immediate first call is guarded too: a plugin that throws on its
    // own seed frame would otherwise raise inside whoever called subscribe(),
    // taking down that caller's setup instead of just its own.
    if (state) {
      try { fn(state); } catch (err) { console.error("telemetry subscriber failed:", err); }
    }
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

  /* Close every stream the moment the page goes away.
     The browser tears an EventSource down on unload by itself, so this is not
     a leak fix — it is a timing one. The socket is only released when the OS
     gets round to it, and this app runs telemetry, the shared /api/events
     stream and an SSH shell against a six-per-origin HTTP/1.1 cap: on a
     reload the new page can find the budget still held by the old one's
     connections. pagehide rather than beforeunload, because beforeunload does
     not fire on a mobile/background tab teardown and pagehide does. */
  function closeStreams() {
    connectionGeneration++;
    if (eventSource) { try { eventSource.close(); } catch (_e) {} eventSource = null; }
    closeConsoleStream();
    // The shared stream outlives any one consumer's unsubscribe by design, so
    // it is closed here explicitly rather than by the refcount.
    if (window.Corvus && Corvus.events) { try { Corvus.events.stop(); } catch (_e) {} }
  }

  if (typeof window !== "undefined" && window.addEventListener) {
    window.addEventListener("pagehide", closeStreams);
  }

  return {
    connect, getState, subscribe, subscribeConsole, requestJson, postAction,
    sendCommand, arm, setMode, connectMavlink,
  };
})();
