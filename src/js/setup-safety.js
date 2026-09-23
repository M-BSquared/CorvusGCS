"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupSafety — the Safety & Sensors sub-page of the Setup page.
 *
 * Two halves that belong together: the envelope a flight is allowed to use
 * (maximum distance and height, the return-to-launch profile, the failsafe
 * action for every loss the autopilot can detect, the battery levels that
 * trigger them) and the sensors those limits lean on — a downward distance
 * sensor and an optical-flow camera.
 *
 * Like the Motors page this is schema-driven: GET /api/safety returns a
 * *description* built by corvus/safety_config.py — sections of fields, each
 * field naming the PX4 parameter it writes. Nothing here hardcodes a parameter
 * name, so a firmware that lacks one sends one field fewer and this page renders
 * one field fewer (AGENTS.md: graceful fallback across PX4 v1.16 / v1.17 / v1.18).
 *
 * The page has two views. The *overview* is the flight envelope plus a Sensors
 * card: one tile per sensor, each saying whether it is up and what is feeding
 * it. The *sensor view* is one sensor on its own page — the switch that brings
 * it up, the hardware presets, and every parameter it owns. That split exists
 * because the two are different jobs: the envelope is read and adjusted before
 * a flight, a sensor is set up once when it is fitted, and interleaving thirty
 * rangefinder parameters with the geofence made the page a wall.
 *
 * A sensor is not a form, it is a switch. Bringing a ground lidar up is two
 * separate acts in PX4 — start the driver, then tell the estimator to fuse what
 * it produces — and doing only one of them is why a rangefinder can read
 * perfectly and change nothing. So the backend sends the ordered write list for
 * both halves and the switch performs the whole chain in one press.
 *
 * A *preset* is the same idea one level up: an operator does not own a
 * "SENS_TFMINI_CFG", they own a TFmini-S. A preset names a real product and
 * carries the whole chain for it plus the numbers off its datasheet — the
 * height band the flow camera tracks in, the noise the lidar's accuracy implies
 * — so fitting a known module is one press instead of eleven parameter edits.
 * Custom writes nothing on its own and leaves every field below to the operator.
 *
 * Backend contract:
 *   GET  /api/safety                 {connected,sections,received}
 *   POST /api/params/set {name,val}  write one field (refused while armed)
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the lifecycle and calls destroy() on back / left-nav re-entry, which
 * releases the telemetry subscription and disowns any in-flight fetch.
 */
Corvus.setupSafety = (function () {
  const S = Corvus.setupShared;

  // The schema-driven form machinery lives in setupShared, shared with the
  // Motors page: the control registry, the armed gate, the write path and its
  // restore-on-refusal. These are aliases, not wrappers — one copy of the write
  // path is the point.
  const { registerControl, dropControls, applyArmed, setFieldStatus, notify } = S;

  // The preset card that is not a product: "I will set this up myself".
  const CUSTOM_PRESET = "custom";

  function render(container, navigateBack) {
    // `controls` holds one recheck() per editable control so an armed
    // transition can re-gate the whole page without walking the DOM. `view`
    // is the only piece of navigation state: "overview", or one sensor id.
    const state = {
      container, navigateBack,
      doc: null, view: null,
      armed: false, loading: false, destroyed: false, controls: [],
      status: { cls: "", text: "" },
      // Which preset each sensor page is showing, and the result of the last
      // attempt to apply it. Both outlive a repaint: every write re-reads the
      // vehicle, so a panel that reset itself would erase the refusal it was
      // meant to report at the exact moment it mattered.
      preset: {}, presetStatus: { cls: "", text: "" },
      // The parameters the operator added under Custom, per sensor. Read from
      // this browser profile once, so a page reload does not lose the row
      // somebody added because their airframe needs it.
      extra: readExtra(),
      // Check values (setupShared): what the operator set until a check
      // confirms it, and the last check's outcome until the next one.
      wanted: {}, check: null, checking: false,
    };

    const cur = Corvus.telemetry && Corvus.telemetry.getState();
    state.armed = !!(cur && cur.armed);

    paint(state);

    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      state.unsub = Corvus.telemetry.subscribe((s) => applyArmed(state, !!(s && s.armed)));
    }

    load(state);

    return function destroy() {
      state.destroyed = true;
      if (state.unsub) { try { state.unsub(); } catch (_e) {} state.unsub = null; }
    };
  }

  // -------------------------------------------------------------------------
  // Page chrome and view routing
  // -------------------------------------------------------------------------

  /** The section the sensor view is showing, or null when it is the overview. */
  function currentSection(state) {
    if (!state.view) return null;
    const sections = (state.doc && state.doc.sections) || [];
    return sections.find((s) => String(s.id) === String(state.view)) || null;
  }

  /**
   * Rebuild the whole page from `state.doc` and `state.view`.
   *
   * Everything is rebuilt, including the actions bar, because the two views do
   * not share a header — the back button in the sensor view has to say
   * "Safety & Sensors" or it lies about where it goes. The page-level status
   * text is carried in `state.status` and re-applied, so a rebuild never
   * silently drops the result of the last read.
   */
  function paint(state) {
    if (state.destroyed) return;
    state.container.innerHTML = "";
    state.controls = [];

    // A sensor that disappeared between reads (a firmware downgrade, a
    // disconnect) must not leave the operator on a page about nothing.
    let section = currentSection(state);
    if (state.view && state.doc && !section) { state.view = null; section = null; }

    const page = S.el("div", "setup-page");
    if (section) {
      page.appendChild(S.backButton(() => openOverview(state), "Safety & Sensors"));
      page.appendChild(S.pageHeader(section.title || "Sensor", section.hint || ""));
    } else {
      page.appendChild(S.backButton(state.navigateBack));
      page.appendChild(S.pageHeader("Safety & Sensors",
        "Flight limits, failsafe actions, and the sensors they rely on"));
    }

    const actions = S.el("div", "params-actions");
    const reloadBtn = Corvus.ui.button({
      variant: "primary", size: "sm", icon: "refresh-cw", label: "Reload",
      className: "safety-reload",
    });
    reloadBtn.disabled = state.loading || state.checking;
    reloadBtn.addEventListener("click", () => {
      state.check = null;
      load(state);
    });
    const checkBtn = S.checkButton(state, CHECK);
    const actionsStatus = S.el("div", "params-actions-status");
    actions.appendChild(reloadBtn);
    actions.appendChild(checkBtn);
    actions.appendChild(actionsStatus);
    page.appendChild(actions);

    const banner = S.el("div", "params-banner");
    banner.hidden = true;
    banner.textContent = "Safety configuration is read-only while armed";
    page.appendChild(banner);

    const host = S.el("div", "page-section safety-sections");
    page.appendChild(host);
    state.container.appendChild(page);

    state.reloadBtn = reloadBtn;
    state.actionsStatus = actionsStatus;
    state.banner = banner;
    state.host = host;
    S.setActionsStatus(actionsStatus, state.status.cls, state.status.text);

    if (state.check) host.appendChild(S.checkCard(state.check, "safety"));
    if (section) renderSensorView(state, section);
    else renderOverview(state);

    applyArmed(state, state.armed);
    S.refreshIcons();
  }

  function openSensor(state, id) {
    state.view = String(id);
    paint(state);
  }

  function openOverview(state) {
    state.view = null;
    paint(state);
  }

  /** The read, carrying whatever parameters the operator added by name. */
  function safetyUrl(state, fresh) {
    const query = [];
    const extra = allExtraNames(state);
    if (extra.length) query.push("extra=" + encodeURIComponent(extra.join(",")));
    if (fresh) query.push("fresh=1");
    return "/api/safety" + (query.length ? "?" + query.join("&") : "");
  }

  // Check values: the shared read back, redrawn by a fresh load().
  const CHECK = {
    prefix: "safety",
    reload: (state) => load(state, true),
    setStatus: (state, cls, text) => setStatus(state, cls, text),
  };

  /**
   * Fetch the configuration and rebuild the active view from the response.
   * `fresh` reads every value from the vehicle rather than the cache.
   */
  function load(state, fresh) {
    if (state.loading) return Promise.resolve();
    state.loading = true;
    if (state.reloadBtn) state.reloadBtn.disabled = true;
    if (state.checkBtn) state.checkBtn.disabled = true;
    setStatus(state, "pending", "Reading safety configuration…");
    return Corvus.telemetry.requestJson(safetyUrl(state, fresh)).then((doc) => {
      if (state.destroyed) return;
      state.doc = doc || {};
      pruneExtra(state);
      if (!doc || !doc.connected) {
        state.status = { cls: "err", text: (doc && doc.error) || "No safety configuration received" };
      } else {
        state.status = { cls: "ok", text: `${doc.received} parameters read` };
      }
    }).catch((err) => {
      if (state.destroyed) return;
      state.doc = {};
      state.status = {
        cls: "err", text: (err && err.message) || "Could not read safety configuration",
      };
    }).finally(() => {
      if (state.destroyed) return;
      state.loading = false;
      paint(state);
    });
  }

  // -------------------------------------------------------------------------
  // Overview
  // -------------------------------------------------------------------------

  function renderOverview(state) {
    // Nothing has been read yet: the status line above already says so, and an
    // explanation of what a disconnected vehicle looks like would be a claim
    // the page cannot make until the read comes back.
    if (state.doc === null) return;

    const sections = Array.isArray(state.doc.sections) ? state.doc.sections : [];
    if (!sections.length) {
      const card = S.el("div", "page-card safety-card");
      card.appendChild(S.sectionTitle("Safety & Sensors"));
      card.appendChild(S.el("div", "params-desc",
        "Connect to a vehicle to read its safety limits, failsafe actions and sensor "
        + "configuration. The page shows only what the connected firmware actually reports."));
      state.host.appendChild(card);
      return;
    }

    const sensors = sections.filter(isSensorSection);
    sections.filter((s) => !isSensorSection(s)).forEach((section) => {
      const card = S.el("div", "page-card safety-card");
      card.dataset.section = section.id || "";
      card.appendChild(S.sectionTitle(section.title || ""));
      if (section.hint) card.appendChild(S.el("div", "field-hint", section.hint));
      const fields = section.fields || [];
      if (fields.length) card.appendChild(fieldGrid(state, fields));
      state.host.appendChild(card);
    });

    if (sensors.length) state.host.appendChild(sensorTiles(state, sensors));
  }

  /** A toggle section is a sensor; a plain form section is part of the envelope. */
  function isSensorSection(section) {
    return !!section && (section.group === "sensors" || section.kind === "toggle");
  }

  /**
   * The Sensors card: one tile per sensor, each carrying the two facts that
   * decide whether the operator needs to open it — is it on, and what is
   * feeding it.
   */
  function sensorTiles(state, sections) {
    const card = S.el("div", "page-card safety-card safety-sensors-card");
    card.dataset.section = "sensors";
    card.appendChild(S.sectionTitle("Sensors"));
    card.appendChild(S.el("div", "field-hint",
      "Each sensor has its own page: pick the module you actually fitted and its whole "
      + "setup is written for you, or set every parameter by hand."));

    const grid = S.el("div", "safety-sensor-tiles");
    sections.forEach((section) => {
      const t = section.toggle || {};
      const tile = Corvus.ui.tile({
        className: "safety-sensor-tile",
        icon: section.icon || "radar",
        title: section.title || "",
        desc: section.short || "",
        chevron: true,
        ariaLabel: (section.title || "Sensor") + " settings",
        onClick: () => openSensor(state, section.id),
      });
      tile.dataset.sensor = section.id || "";

      const body = tile.querySelector(".tile-body");
      const stateRow = S.el("span", "safety-sensor-state");
      const on = !!t.enabled;
      stateRow.appendChild(S.el("span",
        "safety-sensor-pill " + (on ? "on" : "off"), on ? "On" : "Off"));
      if (t.detail) stateRow.appendChild(S.el("span", "safety-sensor-state-detail", t.detail));
      if (body) body.appendChild(stateRow); else tile.appendChild(stateRow);

      grid.appendChild(tile);
    });
    card.appendChild(grid);
    return card;
  }

  // -------------------------------------------------------------------------
  // One sensor, on its own page
  // -------------------------------------------------------------------------

  function renderSensorView(state, section) {
    const status = S.el("div", "page-card safety-card safety-sensor-card");
    status.dataset.section = section.id || "";
    status.appendChild(S.sectionTitle("Status"));
    if (section.kind === "toggle" && section.toggle) {
      status.appendChild(sensorHeader(state, section));
    }
    state.host.appendChild(status);

    if ((section.presets || []).length) {
      state.host.appendChild(presetCard(state, section));
    }

    const fields = section.fields || [];
    if (fields.length) {
      const card = S.el("div", "page-card safety-card safety-settings-card");
      card.appendChild(S.sectionTitle("Parameters"));
      card.appendChild(S.el("div", "field-hint",
        "Every field writes its parameter to the vehicle as soon as you leave it. "
        + "A preset above fills these in; changing one afterwards is expected."));
      card.appendChild(fieldGrid(state, fields));
      state.host.appendChild(card);
    }
  }

  /**
   * The switch that brings one sensor up or takes it down, plus the two pickers
   * that say *which* sensor: the driver, and — for a serial rangefinder, whose
   * "enable" is the port it is wired to — the port.
   *
   * The pickers are live even while the switch is off: choosing a sensor writes
   * nothing until the operator turns it on. Changing one while it is already on
   * re-runs the whole chain, so switching from a TFmini to a Lightware is also
   * one gesture rather than four parameter edits.
   */
  function sensorHeader(state, section) {
    const t = section.toggle;
    const wrap = S.el("div", "safety-sensor");
    wrap.dataset.sensor = section.id || "";

    const main = S.el("div", "safety-sensor-main");
    const text = S.el("div", "safety-sensor-text");
    text.appendChild(S.el("span", "safety-sensor-label", t.label || section.title || ""));
    const detail = S.el("span", "safety-sensor-detail", t.detail || "");
    text.appendChild(detail);
    main.appendChild(text);

    const status = S.el("span", "params-row-status safety-sensor-status", "");

    // opts is kept so the armed re-gate can flip `disabled` and repaint: the
    // component reads that flag on every paint rather than only at build time.
    const opts = {
      className: "safety-sensor-switch",
      value: !!t.enabled,
      ariaLabel: (t.label || "Sensor") + " enabled",
      onChange: (next) => applySensor(state, section, next, status),
    };
    const sw = Corvus.ui.toggle(opts);
    sw.el.dataset.sensor = section.id || "";
    main.appendChild(sw.el);
    wrap.appendChild(main);

    const picker = S.el("div", "safety-sensor-picker");
    let driverSelect = null;
    let portSelect = null;
    let portCell = null;

    const drivers = t.drivers || [];
    if (drivers.length) {
      driverSelect = Corvus.ui.select({
        className: "safety-select safety-driver-select",
        ariaLabel: (t.label || "Sensor") + " model",
        options: drivers.map((d) => ({ value: d.id, label: d.label })),
        value: t.selected || (drivers[0] && drivers[0].id),
      });
      driverSelect.dataset.sensor = section.id || "";
      picker.appendChild(labelled("Sensor", driverSelect));
    }
    if (Array.isArray(t.ports) && t.ports.length) {
      portSelect = Corvus.ui.select({
        className: "safety-select safety-port-select",
        ariaLabel: (t.label || "Sensor") + " serial port",
        options: t.ports.map((p) => ({ value: p.value, label: p.label })),
        value: t.port != null ? t.port : t.ports[0].value,
      });
      portSelect.dataset.sensor = section.id || "";
      portCell = labelled("Serial port", portSelect);
      picker.appendChild(portCell);
    }
    picker.appendChild(status);
    wrap.appendChild(picker);

    const ctx = { section, sw, opts, driverSelect, portSelect, portCell, status, detail };
    section._ctx = ctx;

    // The port picker only means anything for a serial driver, so it is hidden
    // for an I2C sensor rather than sitting there implying it is being written.
    function syncPortVisibility() {
      if (!portCell) return;
      const driver = pickedDriver(ctx);
      portCell.hidden = !(driver && driver.serial);
    }
    syncPortVisibility();

    const reapply = () => {
      syncPortVisibility();
      if (!sw.getValue()) return;
      applySensor(state, section, true, status).catch(() => {});
    };
    if (driverSelect) driverSelect.addEventListener("change", reapply);
    if (portSelect) portSelect.addEventListener("change", reapply);

    // One registry entry for the whole band: the switch and both pickers are
    // gated together, so they re-check together.
    registerControl(state, sw.el, () => {
      opts.disabled = state.armed;
      sw.setValue(sw.getValue());
      if (driverSelect) driverSelect.disabled = state.armed;
      if (portSelect) portSelect.disabled = state.armed;
    });
    return wrap;
  }

  /** A small caption above a picker, so the two selects are not two bare boxes. */
  function labelled(text, control) {
    const cell = S.el("div", "safety-sensor-field");
    cell.appendChild(S.el("span", "safety-sensor-field-label", text));
    cell.appendChild(control);
    return cell;
  }

  /** The driver entry the model picker currently names. */
  function pickedDriver(ctx) {
    const drivers = (ctx.section.toggle && ctx.section.toggle.drivers) || [];
    if (!ctx.driverSelect) return drivers[0] || null;
    return drivers.find((d) => String(d.id) === String(ctx.driverSelect.value)) || null;
  }

  // -------------------------------------------------------------------------
  // Presets
  // -------------------------------------------------------------------------

  /**
   * The hardware picker: one dropdown listing every module this build knows,
   * with Custom at the top.
   *
   * Picking one only *previews* it — the writes it would make are listed in
   * full, with the reason for each, before anything reaches the aircraft. That
   * is deliberate: a preset touches a dozen parameters at once, and a control
   * that quietly rewrote twelve safety parameters on a stray click would be the
   * most dangerous thing on this page.
   */
  function presetCard(state, section) {
    const presets = section.presets || [];
    const card = S.el("div", "page-card safety-card safety-preset-card");
    card.appendChild(S.sectionTitle("Hardware"));
    card.appendChild(S.el("div", "field-hint",
      "Pick the module you actually fitted and Corvus writes its whole setup: driver, "
      + "estimator, and the numbers from its datasheet. Custom writes nothing: the "
      + "parameters below are yours, and you can add any others this airframe needs."));

    const options = [{ value: CUSTOM_PRESET, label: "Custom: set the parameters yourself" }]
      .concat(presets.map((p) => ({ value: p.id, label: presetOptionLabel(p) })));

    const chosen = presetChoice(state, section);
    const detail = S.el("div", "safety-preset-detail");
    const picker = Corvus.ui.select({
      className: "safety-select safety-preset-select",
      ariaLabel: (section.title || "Sensor") + " hardware",
      options,
      value: chosen,
    });
    picker.dataset.sensor = section.id || "";
    picker.addEventListener("change", () => {
      state.preset[String(section.id)] = String(picker.value);
      state.presetStatus = { cls: "", text: "" };
      showPreset(state, section, detail, picker.value);
    });
    registerControl(state, picker);

    card.appendChild(labelled("Module", picker));
    card.appendChild(detail);
    showPreset(state, section, detail, chosen);
    return card;
  }

  /**
   * One line of dropdown text for a preset.
   *
   * The bus belongs in the label, not only in the panel below: picking "UART"
   * when the module on the bench has a CAN plug is the mistake this list can
   * actually prevent, and a dropdown is read one row at a time. A module this
   * firmware cannot run says so here too, so it is not chosen and then refused.
   *
   * A preset whose driver half is already running is marked "in use" — the
   * whole Benewake family shares one parameter, so that says "a module like
   * this is configured", never "this is the one you fitted".
   */
  function presetOptionLabel(preset) {
    const parts = [preset.bus, preset.model].filter(Boolean).join(" · ");
    let label = preset.label + (parts ? " · " + parts : "");
    if (!preset.supported) label += "  (not supported by PX4)";
    else if (preset.active) label += "  (in use)";
    return label;
  }

  /** The preset this sensor page is showing, defaulting to Custom.
   *
   *  A remembered id that the backend no longer offers (a firmware change, a
   *  different vehicle) falls back to Custom rather than to a blank panel.
   */
  function presetChoice(state, section) {
    const want = state.preset[String(section.id)];
    if (!want || want === CUSTOM_PRESET) return CUSTOM_PRESET;
    const known = (section.presets || []).some((p) => String(p.id) === String(want));
    return known ? want : CUSTOM_PRESET;
  }

  /** Render the panel below the dropdown for whichever module is selected. */
  function showPreset(state, section, host, id) {
    host.innerHTML = "";
    dropControls(state, host);
    host.dataset.preset = String(id);
    if (String(id) === CUSTOM_PRESET) {
      renderExtraParams(state, section, host);
      return;
    }

    const preset = (section.presets || []).find((p) => String(p.id) === String(id));
    if (!preset) return;

    if (preset.summary) host.appendChild(S.el("p", "safety-preset-summary", preset.summary));
    if (preset.note) host.appendChild(S.el("p", "safety-preset-note", preset.note));

    if (!preset.supported) {
      const warn = S.el("div", "safety-preset-warning");
      warn.appendChild(S.icon("triangle-alert"));
      warn.appendChild(S.el("span", "safety-preset-warning-text", preset.unsupported
        || "This module cannot be configured from here."));
      host.appendChild(warn);
      return;
    }

    // A serial module is "enabled" by naming its port, and only the operator
    // knows which one it is wired to — so the preset asks before it can write.
    let portSelect = null;
    if (preset.serial) {
      const ports = (section.toggle && section.toggle.ports) || [];
      portSelect = Corvus.ui.select({
        className: "safety-select safety-preset-port",
        ariaLabel: preset.label + " serial port",
        options: ports.map((p) => ({ value: p.value, label: p.label })),
        value: (section.toggle && section.toggle.port != null)
          ? section.toggle.port
          : (ports[0] && ports[0].value),
      });
      host.appendChild(labelled("Serial port", portSelect));
      registerControl(state, portSelect);
    }

    host.appendChild(writeTable(preset, portSelect));

    if (preset.missing && preset.missing.length) {
      const note = S.el("div", "safety-preset-missing",
        "This firmware does not carry " + preset.missing.join(", ")
        + ". Those are skipped, the rest is written.");
      host.appendChild(note);
    }

    const bar = S.el("div", "safety-preset-actions");
    const status = S.el("span", "params-row-status safety-preset-status", "");
    const apply = Corvus.ui.button({
      variant: "primary", size: "sm", icon: "check-check",
      label: "Apply " + preset.label,
      className: "safety-preset-apply",
      onClick: () => applyPreset(state, section, preset, portSelect, status).catch(() => {}),
    });
    apply.dataset.preset = preset.id;
    registerControl(state, apply);
    bar.appendChild(apply);
    bar.appendChild(status);
    host.appendChild(bar);
    setFieldStatus(status, state.presetStatus.cls, state.presetStatus.text);
    S.refreshIcons();
  }

  /** Exactly what the preset will write, parameter by parameter, and why. */
  function writeTable(preset, portSelect) {
    const list = S.el("div", "safety-preset-writes");
    (preset.writes || []).forEach((w) => {
      const row = S.el("div", "safety-preset-write");
      row.dataset.param = w.param || "";
      row.appendChild(S.el("span", "safety-preset-write-param", w.param || ""));
      const value = S.el("span", "safety-preset-write-value",
        w.port ? "the port below" : S.formatNumber(w.value));
      if (w.port) value.classList.add("pending");
      row.appendChild(value);
      row.appendChild(S.el("span", "safety-preset-write-why", w.label || ""));
      list.appendChild(row);
    });
    if (portSelect) list.dataset.serial = "true";
    return list;
  }

  // -------------------------------------------------------------------------
  // Parameters the operator adds themselves
  // -------------------------------------------------------------------------
  //
  // Under Custom, the page stops pretending it knows every setting this
  // airframe needs. A parameter named here is read in the same batch as the
  // rest and edited through the same write path — the only difference is who
  // chose the name. The list is kept per browser profile, like the RC binding
  // map, because it describes how *this* operator works on *their* airframe and
  // not something the aircraft can be asked about.

  const EXTRA_KEY = "corvus.safety.extra";

  // Matches corvus/safety_config.py: a PX4 parameter id is at most 16 bytes on
  // the wire, so a longer name can never be answered and is a typo, not a
  // parameter. The cap is the backend's, restated here so the refusal happens
  // where the operator is typing rather than silently at the far end.
  const EXTRA_NAME_RE = /^[A-Z][A-Z0-9_]{0,15}$/;
  const EXTRA_LIMIT = 24;

  function readExtra() {
    try {
      const raw = JSON.parse(window.localStorage.getItem(EXTRA_KEY) || "{}");
      if (!raw || typeof raw !== "object") return {};
      const out = {};
      Object.keys(raw).forEach((k) => {
        if (Array.isArray(raw[k])) out[k] = raw[k].filter((n) => EXTRA_NAME_RE.test(String(n)));
      });
      return out;
    } catch (_e) { return {}; }
  }

  function writeExtra(map) {
    try { window.localStorage.setItem(EXTRA_KEY, JSON.stringify(map || {})); } catch (_e) {}
  }

  function extraNames(state, sectionId) {
    const list = state.extra[String(sectionId)];
    return Array.isArray(list) ? list : [];
  }

  /** Every added name across every sensor, which is what the read asks for. */
  function allExtraNames(state) {
    const seen = [];
    Object.keys(state.extra || {}).forEach((sid) => {
      extraNames(state, sid).forEach((n) => { if (seen.indexOf(n) < 0) seen.push(n); });
    });
    return seen.slice(0, EXTRA_LIMIT);
  }

  /** The values the last read returned for the added names, by parameter. */
  function extraValues(state) {
    const out = {};
    ((state.doc && state.doc.extra) || []).forEach((f) => { out[String(f.param)] = f; });
    return out;
  }

  /** Every parameter this page already renders as a field of its own. */
  function renderedParams(state) {
    const out = {};
    ((state.doc && state.doc.sections) || []).forEach((section) => {
      (section.fields || []).forEach((f) => { out[String(f.param)] = section.title || ""; });
    });
    return out;
  }

  /**
   * Drop added names that turned out to be fields the page draws anyway.
   *
   * Self-healing rather than cosmetic: the backend refuses to return such a
   * name (two controls over one parameter can disagree until the next read),
   * so without this the row would sit there forever claiming the firmware does
   * not have a parameter that is visible two cards further down.
   */
  function pruneExtra(state) {
    const rendered = renderedParams(state);
    let changed = false;
    Object.keys(state.extra).forEach((sid) => {
      const kept = extraNames(state, sid).filter((n) => !(n in rendered));
      if (kept.length !== extraNames(state, sid).length) {
        changed = true;
        if (kept.length) state.extra[sid] = kept; else delete state.extra[sid];
      }
    });
    if (changed) writeExtra(state.extra);
  }

  /** The Custom panel: the operator's own parameters, and the box that adds one. */
  function renderExtraParams(state, section, host) {
    const sid = String(section.id);
    host.appendChild(S.el("p", "safety-preset-note",
      "Nothing is written until you change a field. If this airframe needs a parameter "
      + "the page does not show, name it below and it joins the form, read with "
      + "everything else, written the same way."));

    const values = extraValues(state);
    const list = S.el("div", "safety-extra-list");
    extraNames(state, sid).forEach((name) => {
      list.appendChild(extraRow(state, section, name, values[name]));
    });
    if (!list.children.length) {
      list.appendChild(S.el("div", "safety-extra-empty", "No extra parameters yet."));
    }
    host.appendChild(list);

    const add = S.el("div", "safety-extra-add");
    const field = Corvus.ui.input({
      className: "safety-extra-input",
      mono: true,
      placeholder: "EKF2_RNG_QLTY_T",
      ariaLabel: "Parameter to add",
      autocomplete: false,
      spellcheck: false,
    });
    const status = S.el("span", "params-row-status safety-extra-status", "");
    const submit = () => addExtra(state, section, field, status);
    field.addEventListener("keydown", (e) => {
      if (e && e.key === "Enter") { if (e.preventDefault) e.preventDefault(); submit(); }
    });
    const button = Corvus.ui.button({
      variant: "secondary", size: "sm", icon: "plus", label: "Add",
      className: "safety-extra-button",
      onClick: submit,
    });
    registerControl(state, field);
    registerControl(state, button);
    add.appendChild(labelled("Add a parameter", field));
    add.appendChild(button);
    add.appendChild(status);
    host.appendChild(add);
    S.refreshIcons();
  }

  /**
   * One added parameter: the same control every other field on this page uses,
   * plus the button that takes it away again.
   *
   * A name the vehicle never answered for keeps its row instead of vanishing.
   * Silently dropping it would leave the operator unable to tell a typo from a
   * parameter their PX4 version simply does not have — and unable to remove it.
   */
  function extraRow(state, section, name, field) {
    const row = S.el("div", S.pformClass("safety", "field") + " safety-extra-field");
    row.dataset.param = name;
    row.appendChild(S.el("span", S.pformClass("safety", "field-label"), name));

    const cell = S.el("div", S.pformClass("safety", "field-control"));
    if (field && field.present) {
      const built = S.paramControl(state, field, { prefix: "safety" });
      cell.appendChild(built.el);
      cell.appendChild(built.status);
    } else {
      cell.appendChild(S.el("span", "safety-extra-absent",
        "this firmware does not have it"));
    }
    const remove = Corvus.ui.iconButton("x", {
      className: "icon-btn safety-extra-remove",
      title: "Remove " + name,
      onClick: () => removeExtra(state, section, name),
    });
    remove.dataset.param = name;
    registerControl(state, remove);
    cell.appendChild(remove);
    row.appendChild(cell);
    return row;
  }

  /**
   * Why this name cannot be added, or "" when it can.
   *
   * Every refusal is answered where it is typed. A parameter that is already a
   * field on this page is the one worth naming precisely: it is not missing, it
   * is somewhere the operator has not scrolled to.
   */
  function extraProblem(state, sectionId, name) {
    if (!name) return "type a parameter name";
    if (!EXTRA_NAME_RE.test(name)) {
      return "not a parameter name: letters, digits and _, up to 16 characters";
    }
    if (extraNames(state, sectionId).indexOf(name) >= 0) return "already added";
    const rendered = renderedParams(state);
    if (name in rendered) return `already on this page, under ${rendered[name]}`;
    if (allExtraNames(state).length >= EXTRA_LIMIT) {
      return `at most ${EXTRA_LIMIT} extra parameters`;
    }
    return "";
  }

  /** Add one parameter to this sensor's list and read its value. */
  function addExtra(state, section, field, status) {
    const sid = String(section.id);
    const name = String(field.value || "").trim().toUpperCase();
    const problem = extraProblem(state, sid, name);
    if (problem) {
      field.classList.add("invalid");
      setFieldStatus(status, "err", problem);
      return;
    }
    field.classList.remove("invalid");
    field.value = "";
    setFieldStatus(status, "", "");
    state.extra[sid] = extraNames(state, sid).concat([name]);
    writeExtra(state.extra);
    // The value has to come from the aircraft, so the page re-reads rather than
    // drawing an empty box the operator could mistake for a real zero.
    load(state);
  }

  function removeExtra(state, section, name) {
    const sid = String(section.id);
    const kept = extraNames(state, sid).filter((n) => n !== name);
    if (kept.length) state.extra[sid] = kept; else delete state.extra[sid];
    writeExtra(state.extra);
    paint(state);
  }

  /**
   * Write one preset's whole chain.
   *
   * Same ordering rule as the switch, for the same reason: a driver that is
   * being replaced is zeroed before the new one is started, so two rangefinder
   * drivers never claim one bus, and the estimator is told last. The section's
   * own `clear` list supplies that first step — minus anything the preset is
   * about to set itself, which would otherwise be written twice.
   */
  async function applyPreset(state, section, preset, portSelect, status) {
    if (state.armed) throw new Error("cannot apply a preset while armed");
    if (!preset.supported) throw new Error(preset.unsupported || "not supported");

    const t = section.toggle || {};
    const own = (preset.writes || []).map((w) => w.param);
    const writes = [];
    (t.clear || []).forEach((param) => {
      if (own.indexOf(param) < 0) writes.push({ name: param, value: 0 });
    });
    for (const w of preset.writes || []) {
      const value = w.port ? Number(portSelect && portSelect.value) : Number(w.value);
      if (!isFinite(value)) {
        setPresetStatus(state, status, "err", "pick the serial port first");
        throw new Error("no serial port selected");
      }
      writes.push({ name: w.param, value });
    }
    if (!writes.length) {
      setPresetStatus(state, status, "", "");
      return;
    }

    setPresetStatus(state, status, "pending", "applying…");
    try {
      await runWrites(state, writes);
    } catch (err) {
      const msg = (err && err.message) || "write failed";
      setPresetStatus(state, status, "err", msg);
      notify("critical", `Could not apply ${preset.label}: ${msg}`);
      load(state);
      throw err;
    }
    setPresetStatus(state, status, "ok", "applied");
    notify("info", preset.reboot
      ? `${preset.label} written. Reboot the autopilot for the driver to start`
      : `${preset.label} written`);
    load(state);
  }

  /**
   * Run the whole enable/disable chain for one sensor.
   *
   * Ordered on purpose: a driver that is being replaced is switched off before
   * the new one is switched on, so two rangefinder drivers never claim the same
   * bus at once. The estimator half goes last on enable and follows the driver
   * on disable.
   *
   * Rejects on the first refused write. The switch component snaps back on a
   * rejection, so a refusal never leaves an "on" switch over a sensor that is
   * still off.
   */
  async function applySensor(state, section, on, status) {
    if (state.armed) throw new Error("cannot change the sensor while armed");
    const t = section.toggle || {};
    const ctx = section._ctx || {};
    const driver = on ? pickedDriver(ctx) : null;

    const writes = [];
    (t.clear || []).forEach((param) => {
      if (driver && driver.param === param) return;
      writes.push({ name: param, value: 0 });
    });
    if (driver && driver.param) {
      const value = driver.serial
        ? Number(ctx.portSelect && ctx.portSelect.value)
        : Number(driver.value);
      if (isFinite(value)) writes.push({ name: driver.param, value });
      // A DroneCAN sensor is a subscription, and a subscription is silent while
      // the CAN stack is off — the driver carries that second write with it.
      (driver.extra || []).forEach((w) => writes.push({ name: w.param, value: Number(w.value) }));
    }
    const tail = on ? (t.enable || []) : (t.disable || []);
    tail.forEach((w) => writes.push({ name: w.param, value: Number(w.value) }));

    if (!writes.length) {
      setFieldStatus(status, "", "");
      return;
    }

    setFieldStatus(status, "pending", on ? "enabling…" : "disabling…");
    try {
      await runWrites(state, writes);
    } catch (err) {
      const msg = (err && err.message) || "write failed";
      setFieldStatus(status, "err", msg);
      notify("critical", `Could not configure ${t.label}: ${msg}`);
      // Re-read so the page shows the half-applied reality rather than the
      // configuration the operator asked for.
      load(state);
      throw err;
    }
    setFieldStatus(status, "ok", on ? "enabled" : "disabled");
    if (t.reboot) {
      notify("info",
        `${t.label} written. Reboot the autopilot for the driver to start`);
    }
    // The chain changes which fields the schema emits, so the page re-reads
    // itself instead of showing a stale layout.
    load(state);
  }

  /**
   * One write at a time, stopping at the first refusal. Each is kept as
   * wanted, like a field's, so Check values can confirm the whole chain.
   */
  async function runWrites(state, writes) {
    for (const write of writes) {
      state.wanted[write.name] = write.value;
      await Corvus.telemetry.postAction("/api/params/set", write);
    }
  }

  /** A plain form section: one labelled control per field. */
  function fieldGrid(state, fields) {
    return S.paramFieldGrid(state, fields, { prefix: "safety" });
  }

  /** Preset status, remembered so the repaint after the write keeps showing it. */
  function setPresetStatus(state, status, cls, text) {
    state.presetStatus = { cls, text };
    setFieldStatus(status, cls, text);
  }

  function setStatus(state, cls, text) {
    state.status = { cls, text };
    S.setActionsStatus(state.actionsStatus, cls, text);
  }

  return { render };
})();
