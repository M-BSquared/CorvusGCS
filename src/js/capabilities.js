"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.capabilities — what the connected flight stack can actually do.
 *
 * Corvus speaks PX4 and ArduPilot, and they do not offer the same feature set:
 * ArduPilot has no MAVLink shell, calibrates its ESCs through a parameter
 * rather than a command, runs its autotune as a flight mode, and waits to be
 * told each accelerometer position. Every one of those is a control on some
 * page, and a control that is offered and then refused is worse than one that
 * is not there — the operator is standing in a field wondering what they did
 * wrong.
 *
 * So the backend publishes the answer at GET /api/mavlink/capabilities and this
 * module caches it. It is cached per *stack*, not for the session: the whole
 * point is that reconnecting to a different aircraft changes the answer, and
 * `autopilot_stack` on the telemetry snapshot is what says so.
 *
 *   await Corvus.capabilities.get()  -> the document, fetched at most once per stack
 *   Corvus.capabilities.peek()       -> what is cached right now, or null
 *   Corvus.capabilities.stack()      -> "px4" | "ardupilot" | "generic" | ""
 *
 * Everything degrades to "allowed": a fetch that fails returns the permissive
 * default, because a page that hides its own controls whenever the network
 * hiccups is a worse failure than one that offers a control the vehicle
 * refuses with a message.
 */
Corvus.capabilities = (function () {
  const DEFAULT = {
    stack: "",
    label: "",
    shell: true,
    esc_calibration: true,
    accel_cal_prompted: false,
    autotune: "command",
    autotune_mode: "",
    log_format: "ulog",
    log_suffix: ".ulg",
    firmware_vendor: "",
    guided_mode: "",
    mission_mode: "MISSION",
    takeoff_frame: "amsl",
    calibrations: [],
    modes: [],
    vehicle_type: 0,
  };

  let cached = null;       // the last document we successfully fetched
  let cachedStack = null;  // the stack it was fetched for
  let inflight = null;

  /** The stack the telemetry snapshot currently reports, or "". */
  function stack() {
    try {
      const snapshot = Corvus.telemetry && Corvus.telemetry.getState
        ? Corvus.telemetry.getState() : null;
      return (snapshot && snapshot.autopilot_stack) || "";
    } catch (_err) {
      return "";
    }
  }

  function peek() {
    return cached;
  }

  /**
   * The capability document for the stack currently connected.
   *
   * Re-fetched when the stack changes and not otherwise, so a page that asks
   * on every render costs one request per aircraft rather than one per paint.
   */
  async function get() {
    const now = stack();
    if (cached && cachedStack === now) return cached;
    if (inflight && cachedStack === now) return inflight;
    cachedStack = now;
    inflight = (async () => {
      try {
        const data = await Corvus.telemetry.requestJson("/api/mavlink/capabilities");
        cached = Object.assign({}, DEFAULT, data || {});
      } catch (_err) {
        // Permissive, deliberately. See the module note.
        cached = Object.assign({}, DEFAULT, { stack: now });
      } finally {
        inflight = null;
      }
      return cached;
    })();
    return inflight;
  }

  /** Forget what is cached — for tests, and for an explicit reconnect. */
  function reset() {
    cached = null;
    cachedStack = null;
    inflight = null;
  }

  /**
   * Does this stack offer *name* as a calibration?
   *
   * Unknown (nothing fetched yet, or a backend that did not list them) is
   * "yes": see the module note on degrading to allowed.
   */
  function hasCalibration(name) {
    if (!cached || !Array.isArray(cached.calibrations) || !cached.calibrations.length) {
      return true;
    }
    return cached.calibrations.indexOf(name) !== -1;
  }

  return { DEFAULT, get, peek, reset, stack, hasCalibration };
})();
