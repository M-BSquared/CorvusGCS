"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.calibProtocol — the PX4 calibration protocol, as a pure state machine.
 *
 * PX4 drives a calibration entirely over STATUSTEXT: it says which side it wants
 * next, when to hold still, how far along it is, and whether it finished or gave
 * up. A ground station that only fires MAV_CMD_PREFLIGHT_CALIBRATION and then
 * prints the raw log leaves the operator to decode "[cal] Rotate to a pending
 * side: back" themselves — which is where field calibrations go wrong.
 *
 * So this module owns two things and no DOM:
 *   parseLine(text)     one STATUSTEXT line  -> a typed event (or null)
 *   createSession(type) those events         -> the state the wizard renders
 *
 * Keeping it free of DOM and of the network is what makes the parsing testable
 * against real PX4 transcripts (tests/test_calib_protocol.js) instead of against
 * a rendered page.
 *
 * Message set verified against PX4 v1.16 / v1.17 / v1.18
 * (src/modules/commander/calibration_routines.cpp, accelerometer_calibration.cpp,
 * mag_calibration.cpp, airspeed_calibration.cpp, esc_calibration.cpp). Every
 * pattern is matched loosely — a wording change across firmwares degrades to
 * "message shown verbatim, no state change", never to a wrong instruction.
 */
Corvus.calibProtocol = (function () {
  /* PX4's orientation vocabulary -> our attitude names. PX4 names the side that
     faces DOWN, which is why "up" means upside down and "down" means level. */
  const SIDE_TO_POSE = {
    down: "level",
    up: "upside_down",
    front: "nose_down",
    back: "tail_down",
    left: "left",
    right: "right",
  };

  const SIDE_WORDS = Object.keys(SIDE_TO_POSE).join("|");

  function poseFor(word) {
    return SIDE_TO_POSE[String(word || "").toLowerCase()] || null;
  }

  /**
   * Parse one STATUSTEXT line into a calibration event.
   * @param {string} raw
   * @returns {{kind:string}|null} null when the line is not calibration traffic.
   */
  function parseLine(raw) {
    const text = String(raw == null ? "" : raw).trim();
    if (!text) return null;
    const low = text.toLowerCase();

    // Terminal outcomes first: they must win over any position wording that
    // happens to share the line.
    let m = low.match(/calibration\s+(?:was\s+)?cancell?ed/);
    if (m) return { kind: "cancelled", text };
    // ESC calibration names itself in front of the verb, so it is matched
    // before the generic "calibration done" pattern swallows it.
    if (/esc calibration finished/.test(low)) return { kind: "done", sensor: "motor", text };
    m = low.match(/calibration\s+(?:done|finished|successful)\b[: ]*\s*(\w+)?/);
    if (m) return { kind: "done", sensor: m[1] || "", text };
    m = low.match(/calibration\s+(?:failed|aborted)\b[: ]*\s*(\w+)?/);
    if (m) return { kind: "failed", sensor: m[1] || "", text };

    m = low.match(/calibration\s+started[: ]*\s*(?:\d+\s+)?(\w+)?/);
    if (m) return { kind: "started", sensor: m[1] || "", text };

    m = low.match(/progress\s*<?\s*(\d{1,3})/);
    if (m) return { kind: "progress", value: Math.max(0, Math.min(100, Number(m[1]))), text };

    // Per-side traffic. Ordered most specific first so "left side done" is not
    // read as a request to rotate to the left side.
    m = low.match(new RegExp("(" + SIDE_WORDS + ")\\s+side\\s+already\\s+completed"));
    if (m) return { kind: "side_completed", pose: poseFor(m[1]), text };

    m = low.match(new RegExp("not enough measurements for\\s+(" + SIDE_WORDS + ")\\s+side"));
    if (m) return { kind: "side_failed", pose: poseFor(m[1]), text };

    m = low.match(new RegExp("(" + SIDE_WORDS + ")\\s+side\\s+done"));
    if (m) return { kind: "side_done", pose: poseFor(m[1]), text };

    m = low.match(new RegExp("hold still,?\\s+measuring\\s+(" + SIDE_WORDS + ")\\s+side"));
    if (m) return { kind: "measure", pose: poseFor(m[1]), text };

    m = low.match(new RegExp("continue rotation for\\s+(" + SIDE_WORDS + ")(?:\\s+side)?\\s*(\\d+)?"));
    if (m) {
      return {
        kind: "rotate_around", pose: poseFor(m[1]),
        seconds: m[2] ? Number(m[2]) : null, text,
      };
    }

    m = low.match(new RegExp("(" + SIDE_WORDS + ")\\s+orientation detected"));
    if (m) return { kind: "detected", pose: poseFor(m[1]), text };

    m = low.match(/rotate to a pending side[: ]+(.+)$/);
    if (m) {
      const poses = m[1].split(/[,;]/).map((w) => poseFor(w.trim())).filter(Boolean);
      return { kind: "pending", poses, text };
    }

    m = low.match(new RegExp("rotate to\\s+(?:the\\s+)?(" + SIDE_WORDS + ")\\s+side"));
    if (m) return { kind: "rotate_to", pose: poseFor(m[1]), text };

    if (/rotate vehicle around the detected orientation/.test(low)) {
      return { kind: "rotate_around", pose: null, seconds: null, text };
    }
    if (/rotate to a different side|rotate the vehicle/.test(low)) {
      return { kind: "pending", poses: [], text };
    }
    if (/hold still|hold the vehicle still|keep the vehicle still/.test(low)) {
      return { kind: "hold", text };
    }

    // Sensor-specific operator prompts that carry an action.
    // Word boundary, not a bare substring: "disconnect battery" contains
    // "connect battery", and telling the operator to plug the battery in when
    // PX4 said to unplug it is the worst thing this parser could do.
    if (/disconnect (?:the )?battery/.test(low)) return { kind: "prompt", action: "battery_off", text };
    if (/\bconnect (?:the )?battery/.test(low)) return { kind: "prompt", action: "battery_on", text };
    if (/blow into/.test(low)) return { kind: "prompt", action: "blow", text };
    if (/(keep wind away|not measuring wind|shield.*wind)/.test(low)) {
      return { kind: "prompt", action: "shield", text };
    }

    if (low.indexOf("[cal]") === 0 || /^\[cal\]/.test(low)) return { kind: "note", text };
    return null;
  }

  /* ------------------------------------------------------------------ */
  /* Procedures                                                          */
  /* ------------------------------------------------------------------ */

  /**
   * One procedure per calibration type. `poses` drives the position strip and
   * therefore the figures; an empty list means the aircraft never moves.
   * `reboot: true` marks the calibrations PX4 only applies after a restart.
   */
  const PROCEDURES = {
    accel: {
      type: "accel", label: "Accelerometer", icon: "move-3d",
      summary: "Teaches PX4 where level is, on all three axes.",
      duration: "2–3 min", danger: false, reboot: true,
      poses: ["level", "left", "right", "nose_down", "tail_down", "upside_down"],
      startPose: "level", spin: false,
      brief: "PX4 asks for six positions in its own order. Hold each one still "
        + "until it says the side is done, then move to the next one it names.",
      prep: [
        "Work on a flat, stable surface with room to turn the aircraft over.",
        "Hold each position steady — a wobble makes PX4 discard the side.",
        "Move between positions briskly; PX4 only measures while you are still.",
      ],
    },
    compass: {
      type: "compass", label: "Compass", icon: "compass",
      summary: "Maps the magnetometer's iron distortion.",
      duration: "3–5 min", danger: false, reboot: true,
      poses: ["level", "left", "right", "nose_down", "tail_down", "upside_down"],
      startPose: "level", spin: true,
      brief: "Same six positions as the accelerometer, but in each one you keep "
        + "turning the aircraft around the vertical axis until PX4 is satisfied.",
      prep: [
        "Go outside, away from steel, cars, reinforced concrete and power lines.",
        "Take off your watch and phone — they carry magnets.",
        "Turn smoothly, about one rotation every four seconds.",
      ],
    },
    level: {
      type: "level", label: "Level Horizon", icon: "separator-horizontal",
      summary: "Trims the attitude so a level aircraft reads zero.",
      duration: "< 30 s", danger: false, reboot: false,
      poses: [], startPose: "level", spin: false,
      brief: "Put the aircraft down in its true flight attitude and leave it alone.",
      prep: [
        "Set it down exactly as it sits in level flight.",
        "Use a spirit level if the surface is not truly flat.",
        "Do not touch it while the measurement runs.",
      ],
    },
    gyro: {
      type: "gyro", label: "Gyroscope", icon: "rotate-3d",
      summary: "Zeroes the rate gyro bias.",
      duration: "< 30 s", danger: false, reboot: false,
      poses: [], startPose: "level", spin: false,
      brief: "Stand the aircraft still. Any movement at all invalidates it.",
      prep: [
        "Place it on a surface that does not vibrate.",
        "Keep hands off until PX4 reports it is done.",
      ],
    },
    baro: {
      type: "baro", label: "Barometer", icon: "gauge",
      summary: "Re-zeroes the pressure sensor.",
      duration: "< 30 s", danger: false, reboot: false,
      poses: [], startPose: "level", spin: false,
      brief: "Leave the aircraft still, out of wind and away from propwash.",
      prep: [
        "Indoors or in still air — a gust shifts the reading.",
        "Do not cover or blow across the sensor port.",
      ],
    },
    airspeed: {
      type: "airspeed", label: "Airspeed", icon: "wind",
      summary: "Zeroes the differential pressure sensor.",
      duration: "< 1 min", danger: false, reboot: false,
      poses: [], startPose: "level", spin: false, marker: "nose",
      brief: "Shield the pitot tube from any airflow, then blow into it once when "
        + "PX4 asks — without touching it.",
      prep: [
        "Cover the pitot tube from wind; do not block the opening.",
        "When asked, blow into the front of the tube from a short distance.",
        "Never touch the tube — the tip bends and the calibration is worthless.",
      ],
    },
    motor: {
      type: "motor", label: "Motors / ESC", icon: "fan",
      summary: "Teaches the ESCs the throttle end points.",
      duration: "1–2 min", danger: true, reboot: false,
      poses: [], startPose: "level", spin: false,
      brief: "The ESCs learn maximum and minimum throttle. Motors will spin.",
      prep: [
        "Remove ALL propellers. This is not optional.",
        "Disconnect the flight battery before you start.",
        "Reconnect the battery only when PX4 asks you to.",
      ],
    },
  };

  const ORDER = ["accel", "compass", "level", "gyro", "baro", "airspeed", "motor"];

  /* ------------------------------------------------------------------ */
  /* Session                                                             */
  /* ------------------------------------------------------------------ */

  const ACTION_TEXT = {
    battery_on: "Connect the flight battery now.",
    battery_off: "Disconnect the flight battery.",
    blow: "Blow into the front of the pitot tube — do not touch it.",
    shield: "Shield the pitot tube from wind.",
  };

  /**
   * A running calibration, fed one STATUSTEXT line at a time.
   *
   * The session is the single source of truth for what the wizard shows: the
   * headline instruction, the attitude the figure holds, the per-side progress
   * and the terminal outcome. It is deliberately tolerant — an unrecognised
   * `[cal]` line still lands in the transcript and refreshes the watchdog, it
   * just does not move the state machine.
   *
   * @param {string} type key into PROCEDURES
   * @returns {{getState:function, begin:function, ingest:function,
   *            finish:function, reset:function}}
   */
  function createSession(type) {
    const proc = PROCEDURES[type];
    if (!proc) throw new Error("unknown calibration type: " + type);

    let state;
    reset();

    function reset() {
      const sides = {};
      proc.poses.forEach((p) => { sides[p] = "pending"; });
      state = {
        type: proc.type,
        phase: "idle",            // idle | starting | running | done | failed | cancelled
        headline: proc.brief,
        detail: "",
        action: null,
        progress: null,
        pose: proc.startPose,
        spin: false,
        marker: proc.marker || null,
        sides,
        lastEventAt: 0,
        seenVehicleMessage: false,
      };
    }

    /** Called once the vehicle has ACKed the start command. */
    function begin(now) {
      state.phase = "starting";
      state.headline = "Waiting for the autopilot to start the calibration…";
      state.detail = "";
      state.progress = null;
      state.lastEventAt = now || 0;
      state.seenVehicleMessage = false;
    }

    function markSide(pose, value) {
      if (pose && pose in state.sides) state.sides[pose] = value;
    }

    /** Anything not yet done becomes the active one; keeps the strip honest. */
    function activate(pose) {
      if (!pose) return;
      Object.keys(state.sides).forEach((p) => {
        if (state.sides[p] === "active") state.sides[p] = "pending";
      });
      if (pose in state.sides && state.sides[pose] !== "done") state.sides[pose] = "active";
      state.pose = pose;
    }

    /**
     * Feed one STATUSTEXT line.
     * @param {string} text
     * @param {number} [now] monotonic-ish timestamp for the stall watchdog
     * @returns {boolean} true when the visible state changed
     */
    function ingest(text, now) {
      const ev = parseLine(text);
      if (!ev) return false;
      state.lastEventAt = now || 0;
      state.seenVehicleMessage = true;
      if (state.phase === "idle" || state.phase === "starting") state.phase = "running";

      switch (ev.kind) {
        case "started":
          state.headline = "Calibration running";
          state.detail = "";
          state.progress = 0;
          break;
        case "progress":
          state.progress = ev.value;
          break;
        case "measure":
          activate(ev.pose);
          state.spin = false;
          state.headline = "Hold still — measuring";
          state.detail = Corvus.calibFigures.poseLabel(ev.pose);
          break;
        case "rotate_around":
          if (ev.pose) activate(ev.pose);
          state.spin = true;
          state.headline = "Keep rotating";
          state.detail = ev.seconds
            ? "Around the vertical axis — about " + ev.seconds + " s to go"
            : "Turn the aircraft around the vertical axis";
          break;
        case "detected":
          activate(ev.pose);
          state.spin = !!proc.spin;
          state.headline = Corvus.calibFigures.poseLabel(ev.pose) + " detected";
          state.detail = proc.spin ? "Now rotate around the vertical axis" : "Hold it there";
          break;
        case "rotate_to":
          activate(ev.pose);
          state.spin = false;
          state.headline = "Rotate to " + Corvus.calibFigures.poseLabel(ev.pose).toLowerCase();
          state.detail = Corvus.calibFigures.poseHint(ev.pose);
          break;
        case "side_done": {
          markSide(ev.pose, "done");
          state.spin = false;
          state.headline = Corvus.calibFigures.poseLabel(ev.pose) + " done";
          // Move the figure straight on to the next position that is still
          // outstanding. Leaving it on the side that just finished shows the
          // operator the one thing they no longer have to do; PX4's own
          // "rotate to a pending side" line may be seconds away, or absent.
          const next = proc.poses.find((p) => state.sides[p] === "pending");
          if (next) {
            activate(next);
            state.detail = "Next: " + Corvus.calibFigures.poseLabel(next).toLowerCase()
              + " — " + Corvus.calibFigures.poseHint(next);
          } else {
            state.detail = "Rotate to a position that is still pending.";
          }
          break;
        }
        case "side_completed":
          markSide(ev.pose, "done");
          state.detail = Corvus.calibFigures.poseLabel(ev.pose) + " was already recorded.";
          break;
        case "side_failed":
          markSide(ev.pose, "failed");
          state.headline = "Not enough measurements";
          state.detail = "Hold " + Corvus.calibFigures.poseLabel(ev.pose).toLowerCase()
            + " more steadily and try that position again.";
          break;
        case "pending": {
          state.spin = false;
          const next = (ev.poses || []).find((p) => state.sides[p] !== "done") || null;
          if (next) activate(next);
          state.headline = "Rotate to the next position";
          state.detail = (ev.poses && ev.poses.length)
            ? "Still pending: " + ev.poses.map(Corvus.calibFigures.poseLabel).join(", ")
            : "Move to a position that is not done yet.";
          break;
        }
        case "hold":
          state.spin = false;
          state.headline = "Hold still";
          state.detail = "";
          break;
        case "prompt":
          state.action = ev.action;
          state.headline = ACTION_TEXT[ev.action] || ev.text;
          state.detail = "";
          break;
        case "done":
          state.phase = "done";
          state.spin = false;
          state.progress = 100;
          Object.keys(state.sides).forEach((p) => { state.sides[p] = "done"; });
          state.headline = "Calibration complete";
          state.detail = proc.reboot
            ? "Reboot the autopilot for the new values to take effect."
            : "The new values are active.";
          break;
        case "failed":
          state.phase = "failed";
          state.spin = false;
          state.headline = "Calibration failed";
          state.detail = ev.text;
          break;
        case "cancelled":
          state.phase = "cancelled";
          state.spin = false;
          state.headline = "Calibration cancelled";
          state.detail = "";
          break;
        default:
          state.detail = ev.text;
          break;
      }
      return true;
    }

    /** Terminal state the operator (not PX4) caused. */
    function finish(phase, headline, detail) {
      state.phase = phase;
      state.spin = false;
      state.headline = headline;
      state.detail = detail || "";
    }

    return {
      procedure: proc,
      getState: () => state,
      begin, ingest, finish, reset,
    };
  }

  return { PROCEDURES, ORDER, SIDE_TO_POSE, poseFor, parseLine, createSession };
})();
