"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.calibProtocol — the calibration protocol, as a pure state machine.
 *
 * Both supported flight stacks drive a calibration over STATUSTEXT: they say
 * which side they want next, when to hold still, how far along they are, and
 * whether they finished or gave up. A ground station that only fires
 * MAV_CMD_PREFLIGHT_CALIBRATION and then prints the raw log leaves the operator
 * to decode "[cal] Rotate to a pending side: back" themselves — which is where
 * field calibrations go wrong.
 *
 * So this module owns two things and no DOM:
 *   parseLine(text)     one STATUSTEXT line  -> a typed event (or null)
 *   createSession(type) those events         -> the state the wizard renders
 *
 * Keeping it free of DOM and of the network is what makes the parsing testable
 * against real transcripts (tests/test_calib_protocol.js) instead of against a
 * rendered page.
 *
 * The one structural difference between the stacks
 * ------------------------------------------------
 * PX4 *detects* each accelerometer position: hold the aircraft still and it
 * decides the side is done. ArduPilot does not. It prints "Place vehicle level
 * and press any key." and then waits — indefinitely — for
 * MAV_CMD_ACCELCAL_VEHICLE_POS naming the position it just asked for. A wizard
 * that only listens shows an ArduPilot operator a screen that says "place
 * vehicle level" and never changes, which is exactly what Corvus used to do.
 *
 * That is the `place` event below: it carries the pose to draw *and* the token
 * the backend needs for POST /api/calibrate/position, so the wizard can put a
 * confirm button under the figure.
 *
 * Message sets verified against PX4 v1.16 / v1.17 / v1.18
 * (calibration_routines.cpp, accelerometer_calibration.cpp, mag_calibration.cpp,
 * airspeed_calibration.cpp, esc_calibration.cpp) and ArduPilot 4.3-4.6
 * (AP_AccelCal.cpp, AP_Compass_Calibration.cpp, AP_InertialSensor.cpp). Every
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

  /* ArduPilot describes the ATTITUDE rather than the side facing down, and it
     asks for the position in words rather than detecting it. Left of the arrow
     is the wording from AP_AccelCal's "Place vehicle %s and press any key.";
     right of it is our pose name. */
  const PLACE_PHRASES = [
    [/place vehicle level/, "level"],
    [/place vehicle on its left side/, "left"],
    [/place vehicle on its right side/, "right"],
    [/place vehicle nose down/, "nose_down"],
    [/place vehicle nose up/, "tail_down"],
    [/place vehicle on its back/, "upside_down"],
  ];

  /* pose -> the token POST /api/calibrate/position takes, which the backend
     turns into MAV_CMD_ACCELCAL_VEHICLE_POS's param1. Kept here because it is
     the same mapping the phrases above encode, read the other way. */
  const POSE_TO_POSITION = {
    level: "level",
    left: "left",
    right: "right",
    nose_down: "nosedown",
    tail_down: "noseup",
    upside_down: "back",
  };

  function positionFor(pose) {
    return POSE_TO_POSITION[pose] || null;
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
    // ArduPilot's own outcome wording. Checked before the generic patterns
    // because "Calibration FAILED" carries no sensor name and would otherwise
    // fall through to the note branch.
    if (/calibration successful/.test(low)) {
      return { kind: "done", sensor: "", text };
    }
    if (/calibration (?:failed|unsuccessful)/.test(low)) {
      return { kind: "failed", sensor: "", text };
    }
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

    // ArduPilot's placement prompt. It is the one message in either protocol
    // that needs an answer: the calibration does not advance until the ground
    // station confirms the position, so this event carries the token that
    // confirmation is sent with.
    for (let i = 0; i < PLACE_PHRASES.length; i += 1) {
      if (PLACE_PHRASES[i][0].test(low)) {
        const pose = PLACE_PHRASES[i][1];
        return { kind: "place", pose, position: positionFor(pose), text };
      }
    }

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

    // ArduPilot narrates the short calibrations without the [cal] prefix, so
    // they would otherwise never reach the transcript or refresh the stall
    // watchdog — which is what makes a working calibration look hung.
    if (/calibrating (?:gyros|barometer|compass)/.test(low)) {
      return { kind: "started", sensor: "", text };
    }
    if (/(?:gyro|barometer|compass)[^.]*calibration complete/.test(low)) {
      return { kind: "done", sensor: "", text };
    }
    if (/(?:rotate|turn) (?:the )?vehicle (?:around|about)/.test(low)) {
      return { kind: "rotate_around", pose: null, seconds: null, text };
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
   *
   * The copy is short on purpose. The operator reads it with an aircraft in
   * both hands, so the figure and the icons carry the instruction and the words
   * only name it. `prep` items are `{icon, text}`: a glance at the icon row has
   * to be enough.
   */
  const PROCEDURES = {
    accel: {
      type: "accel", label: "Accelerometer", icon: "move-3d",
      summary: "Teaches the autopilot where level is, on all three axes.",
      duration: "2 to 3 min", danger: false, reboot: true,
      poses: ["level", "left", "right", "nose_down", "tail_down", "upside_down"],
      startPose: "level", spin: false,
      brief: "Six positions. Hold each one still until it is ticked off.",
      prep: [
        { icon: "separator-horizontal", text: "Flat, stable surface" },
        { icon: "hand", text: "Hold each side still" },
        { icon: "zap", text: "Turn briskly between sides" },
      ],
    },
    compass: {
      type: "compass", label: "Compass", icon: "compass",
      summary: "Maps the magnetometer's iron distortion.",
      duration: "3 to 5 min", danger: false, reboot: true,
      poses: ["level", "left", "right", "nose_down", "tail_down", "upside_down"],
      startPose: "level", spin: true,
      brief: "Six positions. Turn slowly in each one until it is ticked off.",
      prep: [
        { icon: "trees", text: "Outside, away from metal" },
        { icon: "magnet", text: "No watch or phone on you" },
        { icon: "refresh-cw", text: "One turn every 4 s" },
      ],
    },
    level: {
      type: "level", label: "Level Horizon", icon: "separator-horizontal",
      summary: "Trims the attitude so a level aircraft reads zero.",
      duration: "< 30 s", danger: false, reboot: false,
      poses: [], startPose: "level", spin: false,
      brief: "Set it down level and let go.",
      prep: [
        { icon: "separator-horizontal", text: "Level flight attitude" },
        { icon: "hand", text: "Hands off" },
      ],
    },
    gyro: {
      type: "gyro", label: "Gyroscope", icon: "rotate-3d",
      summary: "Zeroes the rate gyro bias.",
      duration: "< 30 s", danger: false, reboot: false,
      poses: [], startPose: "level", spin: false,
      brief: "Keep it perfectly still.",
      prep: [
        { icon: "vibrate", text: "No vibration" },
        { icon: "hand", text: "Hands off" },
      ],
    },
    baro: {
      type: "baro", label: "Barometer", icon: "gauge",
      summary: "Re-zeroes the pressure sensor.",
      duration: "< 30 s", danger: false, reboot: false,
      poses: [], startPose: "level", spin: false,
      brief: "Keep it still, out of wind.",
      prep: [
        { icon: "wind", text: "Still air, no propwash" },
        { icon: "hand", text: "Sensor port uncovered" },
      ],
    },
    airspeed: {
      type: "airspeed", label: "Airspeed", icon: "wind",
      summary: "Zeroes the differential pressure sensor.",
      duration: "< 1 min", danger: false, reboot: false,
      poses: [], startPose: "level", spin: false, marker: "nose",
      brief: "Shield the pitot. Blow into it when asked.",
      prep: [
        { icon: "shield", text: "Shield the pitot from wind" },
        { icon: "wind", text: "Blow into it when asked" },
        { icon: "ban", text: "Never touch the tube" },
      ],
    },
    motor: {
      type: "motor", label: "Motors / ESC", icon: "fan",
      summary: "Teaches the ESCs the throttle end points.",
      duration: "1 to 2 min", danger: true, reboot: false,
      poses: [], startPose: "level", spin: false,
      brief: "Props off. Motors will spin.",
      prep: [
        { icon: "fan", text: "ALL propellers removed" },
        { icon: "unplug", text: "Battery disconnected" },
        { icon: "plug-zap", text: "Connect only when asked" },
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
    blow: "Blow into the pitot tube.",
    shield: "Shield the pitot tube.",
  };

  /**
   * What the operator has to do right now, as one word the wizard turns into
   * an icon (and the figure into a motion). Kept apart from the headline so
   * the picture never depends on how a sentence is worded.
   */
  const CUES = [
    "idle", "wait", "rotate", "hold", "spin", "warn",
    "battery_on", "battery_off", "blow", "shield",
    "done", "failed", "cancelled",
  ];

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
        // Where the aircraft physically is, as far as the autopilot has told
        // us: the last side it detected, measured or finished. The figure
        // animates from here to `pose`, so the operator sees the turn to make
        // rather than only where it ends. Starts level, which is how an
        // aircraft sits on the bench.
        from: proc.startPose,
        cue: "idle",
        spin: false,
        marker: proc.marker || null,
        // Set only while the autopilot is waiting to be TOLD the aircraft is in
        // the position it asked for — ArduPilot's accelerometer calibration and
        // nothing else. `{pose, position}`: the attitude to draw, and the token
        // POST /api/calibrate/position takes. Cleared by the next event, so a
        // confirm button can never outlive the prompt that produced it.
        confirm: null,
        sides,
        lastEventAt: 0,
        seenVehicleMessage: false,
      };
    }

    /**
     * Called as the start command goes out, and again once the vehicle has
     * ACKed it. The first `[cal]` lines often arrive over SSE before the ACK
     * does (UDP SITL above all), and a fast baro or level calibration can
     * even have finished by then, so a session the autopilot has already
     * spoken in is left where it is.
     * @returns {boolean} true when the session moved to (or stayed) "starting"
     */
    function begin(now) {
      if (state.seenVehicleMessage) return false;
      if (state.phase !== "idle" && state.phase !== "starting") return false;
      state.phase = "starting";
      state.cue = "wait";
      state.headline = "Waiting for the autopilot…";
      state.detail = "";
      state.progress = null;
      state.confirm = null;
      state.lastEventAt = now || 0;
      return true;
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

    function rotateTo(pose) {
      return pose
        ? "Rotate to " + Corvus.calibFigures.poseLabel(pose).toLowerCase()
        : "Rotate to an open side";
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
      // Any new word from the autopilot supersedes an outstanding placement
      // prompt. The `place` branch below sets it again; everything else clears
      // it, so the confirm button cannot linger past the question it answers.
      state.confirm = null;

      const F = Corvus.calibFigures;
      switch (ev.kind) {
        case "started":
          state.cue = "wait";
          state.headline = "Calibration running";
          state.detail = "";
          state.progress = 0;
          break;
        case "progress":
          state.progress = ev.value;
          break;
        case "measure":
          activate(ev.pose);
          if (ev.pose) state.from = ev.pose;
          state.spin = false;
          state.cue = "hold";
          state.headline = "Hold still";
          state.detail = F.poseLabel(ev.pose);
          break;
        case "rotate_around":
          if (ev.pose) { activate(ev.pose); state.from = ev.pose; }
          state.spin = true;
          state.cue = "spin";
          state.headline = "Keep rotating";
          state.detail = ev.seconds ? "About " + ev.seconds + " s left" : "";
          break;
        case "detected":
          activate(ev.pose);
          if (ev.pose) state.from = ev.pose;
          state.spin = !!proc.spin;
          state.cue = proc.spin ? "spin" : "hold";
          state.headline = proc.spin ? "Keep rotating" : "Hold still";
          state.detail = F.poseLabel(ev.pose);
          break;
        case "rotate_to":
          activate(ev.pose);
          state.spin = false;
          state.cue = "rotate";
          state.headline = rotateTo(ev.pose);
          state.detail = F.poseHint(ev.pose);
          break;
        case "side_done": {
          markSide(ev.pose, "done");
          if (ev.pose) state.from = ev.pose;
          state.spin = false;
          state.cue = "rotate";
          // Move the figure straight on to the next position that is still
          // outstanding. Leaving it on the side that just finished shows the
          // operator the one thing they no longer have to do; PX4's own
          // "rotate to a pending side" line may be seconds away, or absent.
          const next = proc.poses.find((p) => state.sides[p] === "pending");
          if (next) {
            activate(next);
            state.headline = rotateTo(next);
            state.detail = F.poseHint(next);
          } else {
            state.headline = rotateTo(null);
            state.detail = "";
          }
          break;
        }
        case "place":
          activate(ev.pose);
          state.spin = false;
          state.cue = "rotate";
          state.headline = rotateTo(ev.pose);
          state.detail = F.poseHint(ev.pose) + " Then confirm.";
          state.confirm = ev.position
            ? { pose: ev.pose, position: ev.position }
            : null;
          break;
        case "side_completed":
          markSide(ev.pose, "done");
          state.detail = F.poseLabel(ev.pose) + " already done.";
          break;
        case "side_failed":
          markSide(ev.pose, "failed");
          state.cue = "warn";
          state.headline = "Not enough measurements";
          state.detail = "Hold " + F.poseLabel(ev.pose).toLowerCase() + " more steadily.";
          break;
        case "pending": {
          state.spin = false;
          state.cue = "rotate";
          const next = (ev.poses || []).find((p) => state.sides[p] !== "done") || null;
          if (next) activate(next);
          state.headline = rotateTo(next);
          state.detail = next ? F.poseHint(next) : "";
          break;
        }
        case "hold":
          state.spin = false;
          state.cue = "hold";
          state.headline = "Hold still";
          state.detail = "";
          break;
        case "prompt":
          state.action = ev.action;
          state.cue = ev.action;
          state.headline = ACTION_TEXT[ev.action] || ev.text;
          state.detail = "";
          break;
        case "done":
          state.phase = "done";
          state.spin = false;
          state.cue = "done";
          state.progress = 100;
          Object.keys(state.sides).forEach((p) => { state.sides[p] = "done"; });
          state.headline = "Calibration complete";
          state.detail = proc.reboot ? "Reboot to apply." : "New values active.";
          break;
        case "failed":
          state.phase = "failed";
          state.spin = false;
          state.cue = "failed";
          state.headline = "Calibration failed";
          state.detail = ev.text;
          break;
        case "cancelled":
          state.phase = "cancelled";
          state.spin = false;
          state.cue = "cancelled";
          state.headline = "Calibration cancelled";
          state.detail = "";
          break;
        default:
          state.detail = ev.text;
          break;
      }
      return true;
    }

    /** Terminal state the operator, not the autopilot, caused. */
    function finish(phase, headline, detail) {
      state.phase = phase;
      state.cue = phase;
      state.spin = false;
      state.confirm = null;
      state.headline = headline;
      state.detail = detail || "";
    }

    /**
     * The operator answered a placement prompt.
     *
     * Marks the side as measured and clears the prompt straight away rather
     * than waiting for the autopilot's next line: ArduPilot measures for a
     * second or two before it says anything, and a confirm button that stays
     * live through that window gets pressed twice.
     */
    function confirmPlacement() {
      if (!state.confirm) return false;
      markSide(state.confirm.pose, "done");
      state.from = state.confirm.pose;
      state.confirm = null;
      state.cue = "hold";
      state.headline = "Hold still";
      state.detail = "";
      return true;
    }

    return {
      procedure: proc,
      getState: () => state,
      begin, ingest, finish, reset, confirmPlacement,
    };
  }

  return {
    PROCEDURES, ORDER, SIDE_TO_POSE, POSE_TO_POSITION, CUES,
    poseFor, positionFor, parseLine, createSession,
  };
})();
