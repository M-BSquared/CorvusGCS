"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupControl — the Radio Control sub-page of the Setup page.
 *
 * Everything about the thing in the operator's hands: which transmitter the
 * vehicle listens to, what each channel carries, what every switch does, and
 * the calibration that makes a stick's travel mean the same to PX4 as it does
 * to the pilot.
 *
 * Two views behind one entry point, the same shape as the Calibration page:
 *
 *   overview  the live channel monitor at the top — the one thing on this page
 *             that answers "is the radio even talking" — then the schema-driven
 *             configuration: input mode and RC-loss action, stick channels, the
 *             flight-mode switch with its six slots, the remaining switches,
 *             the AUX passthroughs, and the per-channel calibration table.
 *   wizard    the calibration itself, step by step.
 *
 * Why the calibration is a wizard here and not a button
 * ----------------------------------------------------
 * PX4 has no RC calibration. Every other calibration on the Setup page is a
 * MAV_CMD the autopilot runs and narrates over STATUSTEXT; this one does not
 * exist on the vehicle at all. The whole procedure belongs to the ground
 * station: watch RC_CHANNELS while the operator sweeps every control, then
 * write the endpoints that were seen into RC<n>_MIN / MAX / TRIM / REV. So the
 * wizard is not a nicer front end for a vehicle-side process — it *is* the
 * process, and the measurement it takes is the only evidence the written
 * numbers are based on.
 *
 * Stick direction convention (this is the part that is easy to get backwards):
 * PX4 expects each mapped channel's pulse width to RISE as the stick moves in
 * the direction named below, and RC<n>_REV flips a transmitter that does the
 * opposite.
 *
 *   throttle  rises towards full power
 *   roll      rises to the right
 *   yaw       rises to the right
 *   pitch     rises with the stick pushed FORWARD (nose down)
 *
 * Pitch is the odd one only if you expect "up is more"; PX4 negates the pitch
 * channel on its way into the attitude setpoint, so forward-is-more is what the
 * firmware is built around. The wizard names the direction it wants in words
 * and shows the live bar while it asks, so the operator can see the channel
 * answer rather than trust this comment.
 *
 * Backend contract:
 *   GET  /api/rc                     {connected,sections,assignments,channel_limit}
 *   POST /api/rc/stream {enabled,rate_hz}   raise RC_CHANNELS while open
 *   POST /api/rc/calibrate {channels,count,mapping}  write a measurement
 *   POST /api/params/set {name,value}       edit one field (refused while armed)
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the lifecycle and calls destroy() on back / left-nav re-entry, which
 * releases the telemetry subscription, disowns any in-flight fetch, and hands
 * the RC stream rate back to the firmware.
 */
Corvus.setupControl = (function () {
  const S = Corvus.setupShared;
  const { registerControl, applyArmed, notify } = S;

  /** RC_CHANNELS rate asked for while this page is open. A stick swept through
   *  its travel in half a second is three samples at PX4's default 5 Hz, and
   *  the endpoint written would then be whatever those three happened to
   *  catch. 20 Hz costs ~1 kB/s and turns the sweep into ten times as much
   *  evidence. */
  const STREAM_HZ = 20;

  /** PX4 stores channel pulses in microseconds; these are the bounds a real
   *  receiver stays inside, and they set the scale of the channel bars. */
  const PWM_MIN = 900;
  const PWM_MAX = 2100;

  /** Mirrors corvus/rc_config.MIN_TRAVEL_US — a channel that moved less than
   *  this was not actually swept, whatever the operator believes. Duplicated
   *  rather than fetched because the wizard has to gate its own Next button
   *  before the backend ever sees the measurement; the backend still refuses
   *  independently, which is what makes this a hint and not the rule. */
  const MIN_TRAVEL_US = 200;

  /** How far a channel must move for "Detect" to call it the one that moved.
   *  A three-position switch's smallest step is ~400 us; receiver noise on a
   *  channel sitting still is a handful. */
  const DETECT_TRAVEL_US = 250;
  const DETECT_TIMEOUT_MS = 8000;

  /** The four sticks, in the order the wizard asks for them, each with the
   *  direction PX4 expects the pulse to rise in (see the file docstring). */
  const STICKS = [
    { id: "throttle", param: "RC_MAP_THROTTLE", label: "Throttle",
      action: "Push the throttle stick all the way UP and hold it." },
    { id: "roll", param: "RC_MAP_ROLL", label: "Roll",
      action: "Move the roll stick all the way RIGHT and hold it." },
    { id: "pitch", param: "RC_MAP_PITCH", label: "Pitch",
      action: "Push the pitch stick all the way FORWARD (nose down) and hold it." },
    { id: "yaw", param: "RC_MAP_YAW", label: "Yaw",
      action: "Move the yaw stick all the way RIGHT and hold it." },
  ];

  /* ================================================================== */
  /* Entry point                                                         */
  /* ================================================================== */

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page");
    container.appendChild(page);

    // Last telemetry snapshot, shared by both views; the subscription is owned
    // here so switching views never re-subscribes and never misses a frame
    // between a teardown and the next mount.
    let snapshot = { armed: false, connected: false, rc_channels: [] };
    let active = null;   // { el, destroy, onTelemetry } of the mounted view

    function mount(view) {
      if (active) {
        try { active.destroy(); } catch (err) { console.error("control view teardown:", err); }
        if (active.el && active.el.parentNode) active.el.parentNode.removeChild(active.el);
      }
      active = view;
      page.appendChild(view.el);
      if (typeof view.onTelemetry === "function") view.onTelemetry(snapshot);
      S.refreshIcons();
    }

    // The schema is loaded once by the overview and handed to the wizard, so
    // the wizard knows which channels the sticks are already on and can offer
    // the existing mapping as the starting point of its own.
    const openOverview = () => mount(buildOverview(navigateBack, openWizard));
    function openWizard(doc) {
      mount(buildWizard(doc, () => openOverview()));
    }
    openOverview();

    let unsub = null;
    function onTelemetry(s) {
      if (!s) return;
      snapshot = s;
      if (active && typeof active.onTelemetry === "function") active.onTelemetry(s);
    }
    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      unsub = Corvus.telemetry.subscribe(onTelemetry);
      const cur = Corvus.telemetry.getState();
      if (cur) onTelemetry(cur);
    }

    // Raise the channel rate for as long as the page is open. Best-effort in
    // both directions: a firmware that refuses simply streams at its default,
    // which makes the bars coarser and nothing worse.
    setStream(true);

    return function destroy() {
      if (unsub) { try { unsub(); } catch (_e) {} unsub = null; }
      if (active) {
        try { active.destroy(); } catch (err) { console.error("control teardown:", err); }
        active = null;
      }
      setStream(false);
    };
  }

  function setStream(enabled) {
    if (!Corvus.telemetry || typeof Corvus.telemetry.postAction !== "function") return;
    Corvus.telemetry.postAction("/api/rc/stream", {
      enabled: !!enabled, rate_hz: STREAM_HZ,
    }).catch(() => {});
  }

  /* ================================================================== */
  /* Shared: the live channel monitor                                    */
  /* ================================================================== */

  /** Normalised 0..1 position of a pulse inside the display scale. */
  function pwmFraction(value) {
    const v = Number(value) || 0;
    if (v <= 0) return 0;
    return Math.max(0, Math.min(1, (v - PWM_MIN) / (PWM_MAX - PWM_MIN)));
  }

  /**
   * The channel bars. Built once and updated in place from every telemetry
   * push — a page that rebuilt eighteen rows at 20 Hz would spend its frame
   * budget on createElement and drop the very motion the operator is watching
   * for.
   *
   * Returns {el, update(state), rows} where `rows` is keyed by channel number
   * so a caller (the wizard, the Detect button) can highlight one.
   */
  function channelMonitor(opts) {
    const o = opts || {};
    const el = S.el("div", "page-card rc-monitor");

    const head = S.el("div", "rc-monitor-head");
    head.appendChild(S.sectionTitle(o.title || "Channels"));
    const badge = S.el("div", "rc-monitor-state");
    const badgeDot = S.el("span", "rc-monitor-dot");
    const badgeText = S.el("span", "rc-monitor-state-label", "No signal");
    badge.appendChild(badgeDot);
    badge.appendChild(badgeText);
    head.appendChild(badge);
    el.appendChild(head);

    const list = S.el("div", "rc-channels");
    el.appendChild(list);

    const empty = S.el("div", "rc-monitor-empty",
      "No channel data. Switch the transmitter on and bind the receiver — "
      + "the bars appear as soon as RC_CHANNELS arrives.");
    el.appendChild(empty);

    const rows = {};
    function rowFor(channel) {
      if (rows[channel]) return rows[channel];
      const row = S.el("div", "rc-channel");
      row.dataset.channel = String(channel);
      row.appendChild(S.el("span", "rc-channel-label", "CH " + channel));
      const track = S.el("div", "rc-channel-track");
      const fill = S.el("div", "rc-channel-fill");
      track.appendChild(fill);
      // The centre mark is what makes a trim visible at a glance: a stick that
      // rests off-centre is a trim that will be written as a centre.
      track.appendChild(S.el("div", "rc-channel-centre"));
      row.appendChild(track);
      const value = S.el("span", "rc-channel-value", "—");
      row.appendChild(value);
      const note = S.el("span", "rc-channel-note", "");
      row.appendChild(note);
      list.appendChild(row);
      rows[channel] = { row, fill, value, note };
      return rows[channel];
    }

    /** Which action each channel is bound to, so a bar says what it does.
     *  Also the place a double-booked channel becomes visible. */
    let labels = {};
    function setLabels(next) {
      labels = next || {};
      Object.keys(rows).forEach((channel) => {
        const text = labels[channel] || "";
        rows[channel].note.textContent = text;
        rows[channel].row.dataset.bound = text ? "1" : "";
      });
    }

    function update(state) {
      const channels = (state && state.rc_channels) || [];
      const live = !!(state && state.rc_live) && channels.length > 0;
      empty.hidden = live;
      list.hidden = !live;

      badge.dataset.state = live ? "ok" : "bad";
      if (live) {
        const rssi = Number(state.rc_rssi);
        badgeText.textContent = rssi >= 0
          ? channels.length + " channels · RSSI " + rssi + "%"
          : channels.length + " channels";
      } else {
        badgeText.textContent = "No signal";
      }

      for (let i = 0; i < channels.length; i += 1) {
        const entry = rowFor(i + 1);
        const pulse = Number(channels[i]) || 0;
        entry.fill.style.width = (pwmFraction(pulse) * 100).toFixed(1) + "%";
        entry.value.textContent = pulse > 0 ? pulse + " µs" : "—";
        const text = labels[String(i + 1)] || "";
        if (entry.note.textContent !== text) {
          entry.note.textContent = text;
          entry.row.dataset.bound = text ? "1" : "";
        }
      }
      // Channels the receiver stopped delivering are removed rather than left
      // frozen at their last value, which would read as a live stick.
      Object.keys(rows).forEach((key) => {
        if (Number(key) > channels.length) {
          const entry = rows[key];
          if (entry.row.parentNode) entry.row.parentNode.removeChild(entry.row);
          delete rows[key];
        }
      });
    }

    function highlight(channel) {
      Object.keys(rows).forEach((key) => {
        rows[key].row.dataset.active = (String(channel) === key) ? "1" : "";
      });
    }

    update(null);
    return { el, update, setLabels, highlight, rows };
  }

  /* ================================================================== */
  /* Overview view                                                       */
  /* ================================================================== */

  function buildOverview(navigateBack, openWizard) {
    const el = S.el("div", "rc-overview-view");
    el.appendChild(S.backButton(navigateBack));
    el.appendChild(S.pageHeader("Radio Control",
      "Transmitter calibration, channel assignment and switch mapping"));

    const actions = S.el("div", "params-actions");
    const calibrateBtn = Corvus.ui.button({
      variant: "primary", size: "sm", icon: "radio", label: "Calibrate radio",
    });
    const reloadBtn = Corvus.ui.button({
      variant: "secondary", size: "sm", icon: "refresh-cw", label: "Reload",
    });
    const actionsStatus = S.el("div", "params-actions-status");
    actions.appendChild(calibrateBtn);
    actions.appendChild(reloadBtn);
    actions.appendChild(actionsStatus);
    el.appendChild(actions);

    const banner = S.el("div", "params-banner");
    banner.hidden = true;
    banner.textContent = "Radio configuration is read-only while armed";
    el.appendChild(banner);

    const monitor = channelMonitor({ title: "Live channels" });
    el.appendChild(monitor.el);

    const host = S.el("div", "page-section rc-sections");
    el.appendChild(host);

    // `controls` holds one recheck() per editable control so an armed
    // transition re-gates the whole page without walking the DOM. Same
    // contract the Motors and Safety pages hand to setupShared.
    const state = {
      host, banner, actionsStatus, monitor,
      armed: false, loading: false, destroyed: false, controls: [],
      doc: null, detect: null,
      // Live-updating pieces of the rendered sections, refreshed from the
      // telemetry push rather than rebuilt: the mode-slot highlight and the
      // per-channel "current" readout in the calibration table.
      modeSlots: null, channelRows: {},
    };

    calibrateBtn.addEventListener("click", () => {
      if (state.armed) return;
      openWizard(state.doc);
    });
    reloadBtn.addEventListener("click", () => load(state));
    registerControl(state, calibrateBtn);
    registerControl(state, reloadBtn, () => { reloadBtn.disabled = state.loading; });

    load(state);

    return {
      el,
      onTelemetry(s) {
        applyArmed(state, !!(s && s.armed));
        state.monitor.update(s);
        paintLive(state, s);
        if (state.detect) state.detect.sample(s);
      },
      destroy() {
        state.destroyed = true;
        cancelDetect(state, "cancelled");
      },
    };
  }

  function setStatus(state, cls, text) {
    S.setActionsStatus(state.actionsStatus, cls, text);
  }

  function load(state) {
    if (state.loading) return Promise.resolve();
    state.loading = true;
    S.recheckAll(state);
    setStatus(state, "pending", "Reading radio configuration…");
    return Corvus.telemetry.requestJson("/api/rc").then((doc) => {
      if (state.destroyed) return;
      state.doc = doc || {};
      renderSections(state, state.doc);
      if (!doc || !doc.connected) {
        setStatus(state, "err", (doc && doc.error) || "No radio configuration received");
      } else {
        setStatus(state, "ok", `${doc.received} parameters read`);
      }
    }).catch((err) => {
      if (state.destroyed) return;
      state.doc = null;
      renderSections(state, {});
      setStatus(state, "err", (err && err.message) || "Could not read radio configuration");
    }).finally(() => {
      if (state.destroyed) return;
      state.loading = false;
      S.recheckAll(state);
    });
  }

  /**
   * Which action every channel is bound to, as {channel: "Roll, AUX 1"}.
   *
   * Joined rather than overwritten so a channel bound to two things reads as
   * both. PX4 permits that and it is a real way to lose an aircraft: a kill
   * switch sharing a channel with the mode switch fires on a mode change.
   */
  function assignmentLabels(doc) {
    const labels = {};
    const assignments = (doc && doc.assignments) || {};
    const titles = {};
    ((doc && doc.sections) || []).forEach((section) => {
      (section.fields || []).forEach((field) => {
        if (field.role === "channel") titles[field.param] = field.label;
      });
    });
    Object.keys(assignments).forEach((param) => {
      const channel = Number(assignments[param]);
      if (!channel) return;
      const name = titles[param] || param;
      labels[String(channel)] = labels[String(channel)]
        ? labels[String(channel)] + ", " + name
        : name;
    });
    return labels;
  }

  function renderSections(state, doc) {
    state.host.innerHTML = "";
    S.dropControls(state, state.host);
    state.modeSlots = null;
    state.channelRows = {};

    state.monitor.setLabels(assignmentLabels(doc));

    const sections = Array.isArray(doc.sections) ? doc.sections : [];
    if (!sections.length) {
      const card = S.el("div", "page-card rc-card");
      card.appendChild(S.sectionTitle("Radio Control"));
      card.appendChild(S.el("div", "params-desc",
        "Connect to a vehicle to read its transmitter configuration. The page "
        + "shows only what the connected firmware actually reports, so a mapping "
        + "a firmware does not have is one row fewer rather than an error."));
      state.host.appendChild(card);
      S.refreshIcons();
      return;
    }

    const duplicates = duplicateChannels(doc);

    sections.forEach((section) => {
      const card = S.el("div", "page-card rc-card");
      card.dataset.section = section.id || "";
      card.appendChild(S.sectionTitle(section.title || ""));
      if (section.hint) card.appendChild(S.el("div", "field-hint", section.hint));

      if (section.kind === "channels") {
        card.appendChild(channelTable(state, section));
      } else {
        card.appendChild(fieldGrid(state, section, duplicates));
      }
      state.host.appendChild(card);
    });

    if (duplicates.length) {
      const warn = S.el("div", "rc-conflict");
      warn.appendChild(S.icon("triangle-alert"));
      const many = duplicates.length > 1;
      const list = many
        ? duplicates.slice(0, -1).join(", ") + " and " + duplicates[duplicates.length - 1]
        : String(duplicates[0]);
      warn.appendChild(S.el("span", null,
        (many ? "Channels " + list + " are each bound" : "Channel " + list + " is bound")
        + " to more than one action. PX4 allows it; in the air it means one "
        + "control moves two things."));
      state.host.appendChild(warn);
    }

    S.recheckAll(state);
    S.refreshIcons();
  }

  /** Channels carrying more than one RC_MAP_* assignment. */
  function duplicateChannels(doc) {
    const counts = {};
    const assignments = (doc && doc.assignments) || {};
    Object.keys(assignments).forEach((param) => {
      const channel = Number(assignments[param]);
      if (channel) counts[channel] = (counts[channel] || 0) + 1;
    });
    return Object.keys(counts)
      .filter((channel) => counts[channel] > 1)
      .map(Number)
      .sort((a, b) => a - b);
  }

  /**
   * A section's fields, with a Detect button beside every channel picker.
   *
   * The plain rows come from the shared schema-driven form; only the Detect
   * column and the mode-slot strip are this page's own, because only this page
   * has a live channel stream to identify a control from.
   */
  function fieldGrid(state, section, duplicates) {
    const wrap = S.el("div", "rc-field-wrap");
    const grid = S.el("div", "pform-grid rc-grid");

    (section.fields || []).forEach((field) => {
      const row = S.el("div", "pform-field rc-field");
      row.dataset.param = field.param || "";
      row.appendChild(S.el("span", "pform-field-label rc-field-label",
        field.label || field.param || ""));

      const cell = S.el("div", "pform-field-control rc-field-control");
      const built = S.paramControl(state, field, {
        prefix: "rc",
        onApplied: () => {
          // A mapping write changes what every other row means — the channel
          // it freed, the channel it took, whether anything is now doubled up.
          // Re-reading is the only way the page stays true to the vehicle.
          if (field.role === "channel") load(state);
        },
      });
      cell.appendChild(built.el);
      if (field.unit) cell.appendChild(S.el("span", "pform-unit", field.unit));

      if (field.role === "channel") {
        const detectBtn = Corvus.ui.button({
          variant: "ghost", size: "sm", icon: "crosshair", label: "Detect",
          ariaLabel: "Detect the channel for " + (field.label || field.param),
          onClick: () => startDetect(state, field, built.el, built.status),
        });
        detectBtn.classList.add("rc-detect");
        registerControl(state, detectBtn, () => {
          detectBtn.disabled = state.armed || !!state.detect;
        });
        cell.appendChild(detectBtn);
      }
      cell.appendChild(built.status);
      row.appendChild(cell);

      if (field.hint) {
        row.appendChild(S.el("span", "field-hint pform-field-hint", field.hint));
      }
      const channel = Number(field.value);
      if (field.role === "channel" && channel && duplicates.indexOf(channel) >= 0) {
        row.dataset.conflict = "1";
      }
      grid.appendChild(row);
    });
    wrap.appendChild(grid);

    // The mode section gets a live strip: which of the six positions the
    // switch is in right now, read off the channel it is mapped to. It is the
    // difference between believing the slots are in the right order and seeing
    // it (PX4's own slot arithmetic, mirrored in modeSlotIndex).
    if (section.id === "modes") {
      const strip = modeStrip(state, section);
      if (strip) wrap.appendChild(strip);
    }
    return wrap;
  }

  /* ---------------- flight mode slot strip ---------------- */

  /**
   * PX4's own mode-slot arithmetic, from RCUpdate: the mode channel's
   * normalised -1..+1 value is divided into six equal bands with a small
   * margin at each end. Mirrored rather than approximated so the strip agrees
   * with what the autopilot will actually select.
   */
  function modeSlotIndex(normalized) {
    const SLOT_MIN = -1.0 - 0.05;
    const SLOT_MAX = 1.0 + 0.05;
    const slot = Math.floor(((normalized - SLOT_MIN) / (SLOT_MAX - SLOT_MIN)) * 6);
    return Math.max(0, Math.min(5, slot));
  }

  function modeStrip(state, section) {
    const slotFields = (section.fields || []).filter((f) => f.role === "mode");
    if (!slotFields.length) return null;
    const mapField = (section.fields || []).find((f) => f.param === "RC_MAP_FLTMODE");

    const strip = S.el("div", "rc-modes");
    const cells = slotFields.map((field, index) => {
      const cell = S.el("div", "rc-mode-slot");
      cell.dataset.slot = String(index + 1);
      cell.appendChild(S.el("span", "rc-mode-slot-index", String(index + 1)));
      const name = S.el("span", "rc-mode-slot-name", labelFor(field));
      cell.appendChild(name);
      strip.appendChild(cell);
      return { cell, name, field };
    });
    const note = S.el("div", "field-hint rc-modes-note",
      mapField && Number(mapField.value)
        ? "The highlighted position is where the switch is right now."
        : "Assign a flight mode channel above to see which position the switch is in.");

    const wrap = S.el("div", "rc-modes-wrap");
    wrap.appendChild(strip);
    wrap.appendChild(note);

    state.modeSlots = {
      cells,
      channel: mapField ? Number(mapField.value) || 0 : 0,
      refreshLabels() {
        cells.forEach((c) => { c.name.textContent = labelFor(c.field); });
      },
    };
    return wrap;
  }

  function labelFor(field) {
    const value = Number(field.value);
    const option = (field.options || []).find((o) => Number(o.value) === value);
    return option ? option.label : String(field.value);
  }

  /* ---------------- channel calibration table ---------------- */

  function channelTable(state, section) {
    const table = S.el("div", "rc-cal-table");
    const head = S.el("div", "rc-cal-row rc-cal-head");
    ["Channel", "Now", "Min", "Centre", "Max", "Reversed"].forEach((title) => {
      head.appendChild(S.el("span", "rc-cal-cell", title));
    });
    table.appendChild(head);

    (section.rows || []).forEach((entry) => {
      const row = S.el("div", "rc-cal-row");
      row.dataset.channel = String(entry.channel);
      if (!entry.calibrated) row.dataset.uncalibrated = "1";

      const name = S.el("span", "rc-cal-cell rc-cal-name", "CH " + entry.channel);
      if (!entry.calibrated) {
        name.appendChild(S.el("span", "rc-cal-flag", "not calibrated"));
      }
      row.appendChild(name);

      const now = S.el("span", "rc-cal-cell rc-cal-now", "—");
      row.appendChild(now);

      ["min", "trim", "max"].forEach((key) => {
        const cell = S.el("span", "rc-cal-cell");
        const param = entry.params && entry.params[key];
        if (!param) {
          cell.appendChild(S.el("span", "rc-cal-missing", "—"));
        } else {
          const built = S.paramControl(state, {
            param, label: "Channel " + entry.channel + " " + key,
            kind: "number", value: entry[key], step: 1, min: 500, max: 2500,
          }, { prefix: "rc" });
          built.el.classList.add("rc-cal-input");
          cell.appendChild(built.el);
          cell.appendChild(built.status);
        }
        row.appendChild(cell);
      });

      const revCell = S.el("span", "rc-cal-cell");
      if (entry.has_rev && entry.params && entry.params.rev) {
        const built = S.paramControl(state, {
          param: entry.params.rev,
          label: "Channel " + entry.channel + " direction",
          kind: "sign", value: entry.reversed ? -1 : 1,
          options: [{ value: 1, label: "Normal" }, { value: -1, label: "Reversed" }],
        }, { prefix: "rc", signFallback: 1 });
        built.el.classList.add("rc-cal-rev");
        revCell.appendChild(built.el);
        revCell.appendChild(built.status);
      } else {
        revCell.appendChild(S.el("span", "rc-cal-missing", "—"));
      }
      row.appendChild(revCell);

      table.appendChild(row);
      state.channelRows[entry.channel] = { row, now };
    });
    return table;
  }

  /** Per-frame refresh of everything in the rendered sections that is live. */
  function paintLive(state, s) {
    const channels = (s && s.rc_channels) || [];

    Object.keys(state.channelRows).forEach((key) => {
      const pulse = Number(channels[Number(key) - 1]) || 0;
      const cell = state.channelRows[key].now;
      const text = pulse > 0 ? pulse + " µs" : "—";
      if (cell.textContent !== text) cell.textContent = text;
    });

    const slots = state.modeSlots;
    if (!slots) return;
    const pulse = slots.channel ? Number(channels[slots.channel - 1]) || 0 : 0;
    let activeIndex = -1;
    if (pulse > 0) {
      // Normalise against the display scale rather than the channel's own
      // endpoints: an uncalibrated channel has no trustworthy endpoints, and a
      // strip that silently uses the wrong ones is worse than one that is
      // openly approximate.
      activeIndex = modeSlotIndex(pwmFraction(pulse) * 2 - 1);
    }
    slots.cells.forEach((c, index) => {
      c.cell.dataset.active = index === activeIndex ? "1" : "";
    });
  }

  /* ---------------- Detect: bind a control by moving it ---------------- */

  /**
   * Watch the live channels and pick the one that moves.
   *
   * This is the answer to "which channel is that switch on", which is
   * otherwise a guess followed by a test flight. The baseline is the frame at
   * the moment Detect was pressed; the winner is the channel whose travel from
   * that baseline is both largest and past DETECT_TRAVEL_US, so a receiver's
   * few-microsecond jitter never wins.
   */
  function startDetect(state, field, control, status) {
    if (state.detect || state.armed) return;
    const snapshot = (Corvus.telemetry && Corvus.telemetry.getState()) || {};
    const baseline = ((snapshot.rc_channels) || []).slice();
    if (!snapshot.rc_live || !baseline.length) {
      S.setFieldStatus(status, "err", "no RC signal");
      return;
    }

    const deadline = Date.now() + DETECT_TIMEOUT_MS;
    S.setFieldStatus(status, "pending", "move it now…");
    state.detect = {
      field, control, status, baseline,
      sample(s) {
        const channels = (s && s.rc_channels) || [];
        let best = 0;
        let bestTravel = 0;
        for (let i = 0; i < channels.length && i < baseline.length; i += 1) {
          const travel = Math.abs((Number(channels[i]) || 0) - (Number(baseline[i]) || 0));
          if (travel > bestTravel) { bestTravel = travel; best = i + 1; }
        }
        if (bestTravel >= DETECT_TRAVEL_US) {
          finishDetect(state, best);
          return;
        }
        if (Date.now() > deadline) cancelDetect(state, "nothing moved");
      },
    };
    S.recheckAll(state);
  }

  function finishDetect(state, channel) {
    const detect = state.detect;
    if (!detect) return;
    state.detect = null;
    S.recheckAll(state);
    detect.control.value = String(channel);
    S.setFieldStatus(detect.status, "ok", "channel " + channel);
    // Dispatched rather than called directly so the write goes through the one
    // path every other edit on this page uses, refusal handling included.
    detect.control.dispatchEvent(changeEvent());
  }

  /** A "change" event, or a plain stand-in where Event does not exist —
   *  the same fallback ui.js's own select uses. */
  function changeEvent() {
    if (typeof Event === "function") return new Event("change", { bubbles: true });
    return { type: "change", bubbles: true };
  }

  function cancelDetect(state, reason) {
    const detect = state.detect;
    if (!detect) return;
    state.detect = null;
    if (!state.destroyed) {
      S.setFieldStatus(detect.status, "err", reason || "cancelled");
      S.recheckAll(state);
    }
  }

  /* ================================================================== */
  /* Calibration wizard                                                  */
  /* ================================================================== */

  const STEPS = ["intro", "centre", "sweep", "sticks", "review"];

  function buildWizard(doc, navigateBack) {
    const el = S.el("div", "rc-wizard-view");
    el.appendChild(Corvus.ui.button({
      variant: "ghost", size: "sm", className: "setup-back",
      icon: "chevron-left", label: "Radio Control",
      ariaLabel: "Back to Radio Control", onClick: () => navigateBack(),
    }));
    el.appendChild(S.pageHeader("Radio Calibration",
      "Measure what the transmitter sends, then write it to the vehicle"));

    const banner = S.el("div", "params-banner setup-armed-banner");
    banner.hidden = true;
    el.appendChild(banner);

    // --- the measurement --------------------------------------------------
    // `travel` is the only thing this wizard produces: per channel, the lowest
    // and highest pulse ever seen during the sweep, the pulse seen while the
    // sticks were centred, and — for the four sticks — which way the channel
    // moved when the operator was asked for a named direction.
    const measurement = {
      centre: {},        // channel -> pulse at rest
      min: {},           // channel -> lowest pulse seen
      max: {},           // channel -> highest pulse seen
      sticks: {},        // stick id -> {channel, reversed}
      count: 0,
    };

    let step = "intro";
    let vehicle = { armed: false, connected: false, rc_live: false };
    let latest = [];
    let stickIndex = 0;
    let busy = false;
    let paintedKey = "";

    const stage = S.el("div", "page-card rc-stage");
    const stepChip = S.el("div", "rc-step");
    stage.appendChild(stepChip);
    const headline = S.el("div", "rc-headline");
    headline.setAttribute("role", "status");
    headline.setAttribute("aria-live", "polite");
    stage.appendChild(headline);
    const detail = S.el("div", "rc-detail");
    stage.appendChild(detail);
    const hint = S.el("div", "rc-hint");
    hint.hidden = true;
    stage.appendChild(hint);
    el.appendChild(stage);

    const monitor = channelMonitor({ title: "Live channels" });
    el.appendChild(monitor.el);

    const review = S.el("div", "page-card rc-review");
    review.hidden = true;
    el.appendChild(review);

    const backBtn = Corvus.ui.button({
      variant: "secondary", icon: "chevron-left", label: "Back",
      onClick: () => goBack(),
    });
    const nextBtn = Corvus.ui.button({
      variant: "primary", icon: "chevron-right", label: "Next",
      onClick: () => goNext(),
    });
    const applyBtn = Corvus.ui.button({
      variant: "primary", icon: "check", label: "Write to vehicle",
      onClick: () => apply(),
    });
    const restartBtn = Corvus.ui.button({
      variant: "secondary", icon: "rotate-cw", label: "Start over",
      onClick: () => restart(),
    });
    const actions = Corvus.ui.actions([backBtn, nextBtn, applyBtn, restartBtn]);
    actions.classList.add("rc-actions");
    el.appendChild(actions);

    // The status element carries only the shared class: setActionsStatus
    // rewrites className wholesale, so a modifier put on it would survive
    // exactly until the first message. The spacing lives on the wrapper.
    const statusRow = S.el("div", "rc-status");
    const status = S.el("div", "params-actions-status");
    statusRow.appendChild(status);
    el.appendChild(statusRow);

    /* ---------------- steps ---------------- */

    function activeChannels() {
      return latest.map((v, i) => ({ channel: i + 1, value: Number(v) || 0 }))
        .filter((c) => c.value > 0);
    }

    /** Channels whose sweep is wide enough to be a real measurement. */
    function sweptChannels() {
      return Object.keys(measurement.max)
        .map(Number)
        .filter((channel) => (measurement.max[channel] - measurement.min[channel])
          >= MIN_TRAVEL_US)
        .sort((a, b) => a - b);
    }

    function paint() {
      const index = STEPS.indexOf(step);
      stepChip.textContent = "Step " + (index + 1) + " of " + STEPS.length;
      stepChip.dataset.step = step;

      const blocked = vehicle.armed || !vehicle.connected;
      banner.hidden = !blocked;
      banner.textContent = vehicle.armed
        ? "Cannot calibrate the radio while armed — disarm first."
        : "No link to the vehicle — connect before calibrating.";

      review.hidden = step !== "review";
      hint.hidden = true;
      monitor.highlight(null);

      if (step === "intro") {
        headline.textContent = "Before you start";
        detail.textContent = "Propellers off. Battery disconnected or the vehicle "
          + "on a bench. Transmitter switched on and the receiver bound — the "
          + "channel bars below must be moving when you wiggle a stick. Every "
          + "endpoint this wizard writes comes from what it sees there.";
      } else if (step === "centre") {
        headline.textContent = "Centre the sticks, throttle fully down";
        detail.textContent = "Let the sticks rest at their centre and pull the "
          + "throttle all the way down. Leave the switches wherever they normally "
          + "sit. This is the resting position written as each channel's centre.";
      } else if (step === "sweep") {
        const swept = sweptChannels();
        headline.textContent = "Move everything through its full travel";
        detail.textContent = "Sweep every stick to all four corners and flip every "
          + "switch and knob end to end, slowly. The bars follow; the numbers "
          + "beside them are the widest travel seen so far.";
        hint.hidden = false;
        hint.textContent = swept.length
          ? swept.length + " channel" + (swept.length === 1 ? "" : "s")
            + " swept far enough so far: CH " + swept.join(", CH ")
          : "No channel has moved far enough yet.";
      } else if (step === "sticks") {
        const stick = STICKS[stickIndex];
        headline.textContent = stick.label + ": " + stick.action;
        detail.textContent = "Hold it there and press Next. The channel that moved "
          + "is the one this axis is bound to, and the direction it moved decides "
          + "whether PX4 has to reverse it.";
        const known = measurement.sticks[stick.id];
        hint.hidden = false;
        hint.textContent = known
          ? "Measured: channel " + known.channel
            + (known.reversed ? " (reversed)" : " (normal)")
          : "Nothing measured for this axis yet.";
        if (known) monitor.highlight(known.channel);
      } else {
        headline.textContent = "Review the measurement";
        detail.textContent = "Nothing has been written yet. Check the endpoints "
          + "below, then write them to the vehicle.";
        renderReview();
      }

      backBtn.hidden = step === "intro";
      backBtn.disabled = busy;
      nextBtn.hidden = step === "review";
      nextBtn.disabled = busy || blocked || !canAdvance();
      applyBtn.hidden = step !== "review";
      applyBtn.disabled = busy || blocked || !reviewRows().length;
      restartBtn.hidden = step === "intro";
      restartBtn.disabled = busy;

      el.dataset.step = step;
      // Lucide rewrites every placeholder in the document, so it runs on a
      // step change and not on the 20 Hz telemetry repaint that keeps the
      // sweep counters moving.
      const key = step + ":" + stickIndex;
      if (key !== paintedKey) {
        paintedKey = key;
        S.refreshIcons();
      }
    }

    /** Whether the current step has enough evidence to move on. */
    function canAdvance() {
      if (!vehicle.rc_live) return false;
      // Every step but the review gates on the live frame: the operator can
      // only be holding a stick somewhere if channels are arriving at all.
      // (The centre step captures its snapshot on the way OUT, so gating it on
      // that snapshot would leave Next disabled forever.)
      if (step === "intro" || step === "centre") return activeChannels().length > 0;
      if (step === "sweep") return sweptChannels().length >= 4;
      if (step === "sticks") {
        // The stick has to have moved from where it rested, or there is
        // nothing to bind it to and nothing to take a direction from.
        return !!detectStick();
      }
      return true;
    }

    /**
     * The channel the current stick prompt moved, and which way.
     *
     * Measured against the centre captured in step 2 rather than against the
     * previous frame: the operator is asked to move a stick and HOLD it, so the
     * frame-to-frame delta is zero exactly when the answer is available.
     */
    function detectStick() {
      let best = null;
      let bestTravel = 0;
      for (let i = 0; i < latest.length; i += 1) {
        const channel = i + 1;
        const centre = measurement.centre[channel];
        if (centre == null) continue;
        const value = Number(latest[i]) || 0;
        if (!value) continue;
        const travel = Math.abs(value - centre);
        if (travel > bestTravel) {
          bestTravel = travel;
          best = { channel, reversed: value < centre };
        }
      }
      return bestTravel >= DETECT_TRAVEL_US ? best : null;
    }

    function goNext() {
      if (step === "intro") {
        step = "centre";
      } else if (step === "centre") {
        captureCentre();
        step = "sweep";
      } else if (step === "sweep") {
        step = "sticks";
        stickIndex = 0;
      } else if (step === "sticks") {
        const found = detectStick();
        if (found) measurement.sticks[STICKS[stickIndex].id] = found;
        if (stickIndex < STICKS.length - 1) stickIndex += 1;
        else step = "review";
      }
      paint();
    }

    function goBack() {
      if (step === "sticks" && stickIndex > 0) { stickIndex -= 1; paint(); return; }
      const index = STEPS.indexOf(step);
      step = STEPS[Math.max(0, index - 1)];
      if (step === "sticks") stickIndex = STICKS.length - 1;
      paint();
    }

    function restart() {
      measurement.centre = {};
      measurement.min = {};
      measurement.max = {};
      measurement.sticks = {};
      measurement.count = 0;
      stickIndex = 0;
      step = "intro";
      S.setActionsStatus(status, "", "");
      paint();
    }

    /** Snapshot the resting position of every channel the receiver delivers. */
    function captureCentre() {
      measurement.centre = {};
      measurement.min = {};
      measurement.max = {};
      activeChannels().forEach((entry) => {
        measurement.centre[entry.channel] = entry.value;
        measurement.min[entry.channel] = entry.value;
        measurement.max[entry.channel] = entry.value;
      });
      measurement.count = activeChannels().length;
    }

    /** Widen each channel's measured travel with the frame that just arrived. */
    function accumulate() {
      if (step !== "sweep" && step !== "sticks") return;
      activeChannels().forEach((entry) => {
        const channel = entry.channel;
        if (measurement.min[channel] == null) {
          measurement.min[channel] = entry.value;
          measurement.max[channel] = entry.value;
          return;
        }
        if (entry.value < measurement.min[channel]) measurement.min[channel] = entry.value;
        if (entry.value > measurement.max[channel]) measurement.max[channel] = entry.value;
      });
    }

    /* ---------------- review + write ---------------- */

    /**
     * The rows that will be written: every channel swept far enough, with its
     * centre clamped into its own measured travel.
     *
     * The clamp is not cosmetic. A throttle held down through the sweep ends
     * with a centre equal to its minimum, which is correct; a channel whose
     * rest position was captured before the operator touched it can end up a
     * microsecond outside the travel that was measured afterwards, and the
     * backend refuses that outright.
     */
    function reviewRows() {
      const stickChannels = {};
      Object.keys(measurement.sticks).forEach((id) => {
        stickChannels[measurement.sticks[id].channel] = measurement.sticks[id];
      });
      return sweptChannels().map((channel) => {
        const min = measurement.min[channel];
        const max = measurement.max[channel];
        const centre = measurement.centre[channel] == null
          ? Math.round((min + max) / 2)
          : Math.max(min, Math.min(max, measurement.centre[channel]));
        const stick = stickChannels[channel];
        return {
          channel, min, max, trim: centre,
          reversed: stick ? stick.reversed : false,
          set_reverse: !!stick,
          role: stick ? stickLabel(channel) : "",
        };
      });
    }

    function stickLabel(channel) {
      const names = STICKS
        .filter((s) => measurement.sticks[s.id]
          && measurement.sticks[s.id].channel === channel)
        .map((s) => s.label);
      return names.join(", ");
    }

    function renderReview() {
      Corvus.ui.clear(review);
      review.appendChild(S.sectionTitle("What will be written"));
      const rows = reviewRows();
      if (!rows.length) {
        review.appendChild(S.el("div", "params-desc",
          "No channel was swept far enough to write. Start over and move every "
          + "stick and switch through its full travel."));
        return;
      }

      // A stick bound to two axes, or two axes bound to one channel, is a
      // measurement mistake — almost always a stick that was not moved when it
      // was asked for. Saying so here beats writing it and flying it.
      const bound = {};
      let clash = "";
      STICKS.forEach((stick) => {
        const found = measurement.sticks[stick.id];
        if (!found) return;
        if (bound[found.channel]) clash = "Channel " + found.channel + " was measured "
          + "for both " + bound[found.channel] + " and " + stick.label
          + " — one of those sticks did not move when it was asked to.";
        bound[found.channel] = stick.label;
      });
      const missing = STICKS.filter((s) => !measurement.sticks[s.id]).map((s) => s.label);

      const table = S.el("div", "rc-cal-table");
      const head = S.el("div", "rc-cal-row rc-cal-head");
      ["Channel", "Bound to", "Min", "Centre", "Max", "Direction"].forEach((title) => {
        head.appendChild(S.el("span", "rc-cal-cell", title));
      });
      table.appendChild(head);
      rows.forEach((entry) => {
        const row = S.el("div", "rc-cal-row");
        row.dataset.channel = String(entry.channel);
        row.appendChild(S.el("span", "rc-cal-cell rc-cal-name", "CH " + entry.channel));
        row.appendChild(S.el("span", "rc-cal-cell", entry.role || "—"));
        row.appendChild(S.el("span", "rc-cal-cell rc-cal-num", String(entry.min)));
        row.appendChild(S.el("span", "rc-cal-cell rc-cal-num", String(entry.trim)));
        row.appendChild(S.el("span", "rc-cal-cell rc-cal-num", String(entry.max)));
        row.appendChild(S.el("span", "rc-cal-cell",
          entry.set_reverse ? (entry.reversed ? "Reversed" : "Normal") : "unchanged"));
        table.appendChild(row);
      });
      review.appendChild(table);

      if (missing.length) {
        review.appendChild(S.el("div", "rc-review-warn",
          "No channel was measured for: " + missing.join(", ")
          + ". Those axes keep whatever mapping the vehicle already has."));
      }
      if (clash) review.appendChild(S.el("div", "rc-review-warn", clash));
    }

    async function apply() {
      if (busy) return;
      const rows = reviewRows();
      if (!rows.length) return;
      const mapping = {};
      STICKS.forEach((stick) => {
        const found = measurement.sticks[stick.id];
        if (found) mapping[stick.param] = found.channel;
      });

      busy = true;
      paint();
      S.setActionsStatus(status, "pending", "Writing the calibration…");
      try {
        const result = await Corvus.telemetry.postAction("/api/rc/calibrate", {
          channels: rows.map((r) => ({
            channel: r.channel, min: r.min, max: r.max, trim: r.trim,
            reversed: r.reversed, set_reverse: r.set_reverse,
          })),
          count: Math.max(...rows.map((r) => r.channel)),
          mapping,
        });
        const written = (result && result.writes && result.writes.length) || 0;
        S.setActionsStatus(status, "ok", written + " parameters written");
        notify("info", "Radio calibration written to the vehicle");
        navigateBack();
      } catch (err) {
        const msg = (err && err.message) || "The vehicle rejected the calibration";
        S.setActionsStatus(status, "err", msg);
        notify("critical", "Radio calibration failed: " + msg);
      } finally {
        busy = false;
        paint();
      }
    }

    paint();

    return {
      el,
      onTelemetry(s) {
        const next = {
          armed: !!(s && s.armed),
          connected: !!(s && s.connected),
          rc_live: !!(s && s.rc_live),
        };
        // A change to what gates the wizard — arming, a dropped link, the
        // transmitter going quiet — has to repaint on ANY step. The review step
        // is the one that matters: it carries the write button, and leaving it
        // enabled over a vehicle that just armed is the page lying about what
        // it is about to be allowed to do.
        const gateChanged = next.armed !== vehicle.armed
          || next.connected !== vehicle.connected
          || next.rc_live !== vehicle.rc_live;
        vehicle = next;
        latest = (s && s.rc_channels) || [];
        monitor.update(s);
        accumulate();
        // Otherwise every step but the review is repainted per frame: each of
        // them gates its Next button on what is arriving right now, and a
        // button whose state was decided on entry is wrong the moment the
        // transmitter is switched on. The review is left alone because
        // repainting it rebuilds its table under the operator's cursor.
        if (gateChanged || step !== "review") paint();
      },
      destroy() {},
    };
  }

  return { render };
})();
