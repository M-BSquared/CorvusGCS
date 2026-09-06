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
 * A sensor section is not a form, it is a switch. Bringing a ground lidar up is
 * two separate acts in PX4 — start the driver, then tell the estimator to fuse
 * what it produces — and doing only one of them is why a rangefinder can read
 * perfectly and change nothing. So the backend sends the ordered write list for
 * both halves and the switch performs the whole chain in one press; the settings
 * that stay genuinely per-airframe (mounting offset, height limits, quality
 * gates) remain ordinary fields underneath.
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
  const { registerControl, applyArmed, setFieldStatus, notify } = S;

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page");
    page.appendChild(S.backButton(navigateBack));
    page.appendChild(S.pageHeader("Safety & Sensors",
      "Flight limits, failsafe actions, and the sensors they rely on"));

    // Actions bar lives outside the content host so Reload + status survive
    // every re-render of the sections below.
    const actions = S.el("div", "params-actions");
    const reloadBtn = Corvus.ui.button({
      variant: "primary", size: "sm", icon: "refresh-cw", label: "Reload",
    });
    const actionsStatus = S.el("div", "params-actions-status");
    actions.appendChild(reloadBtn);
    actions.appendChild(actionsStatus);
    page.appendChild(actions);

    const banner = S.el("div", "params-banner");
    banner.hidden = true;
    banner.textContent = "Safety configuration is read-only while armed";
    page.appendChild(banner);

    const host = S.el("div", "page-section safety-sections");
    page.appendChild(host);
    container.appendChild(page);

    // `controls` holds one recheck() per editable control so an armed
    // transition can re-gate the whole page without walking the DOM.
    const state = {
      host, banner, reloadBtn, actionsStatus,
      armed: false, loading: false, destroyed: false, controls: [],
    };

    const cur = Corvus.telemetry && Corvus.telemetry.getState();
    applyArmed(state, !!(cur && cur.armed));

    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      state.unsub = Corvus.telemetry.subscribe((s) => applyArmed(state, !!(s && s.armed)));
    }

    reloadBtn.addEventListener("click", () => load(state));
    load(state);

    return function destroy() {
      state.destroyed = true;
      if (state.unsub) { try { state.unsub(); } catch (_e) {} state.unsub = null; }
    };
  }

  /** Fetch the configuration and rebuild every section from the response. */
  function load(state) {
    if (state.loading) return Promise.resolve();
    state.loading = true;
    state.reloadBtn.disabled = true;
    setStatus(state, "pending", "Reading safety configuration…");
    return Corvus.telemetry.requestJson("/api/safety").then((doc) => {
      if (state.destroyed) return;
      renderSections(state, doc || {});
      if (!doc || !doc.connected) {
        setStatus(state, "err", (doc && doc.error) || "No safety configuration received");
      } else {
        setStatus(state, "ok", `${doc.received} parameters read`);
      }
    }).catch((err) => {
      if (state.destroyed) return;
      renderSections(state, {});
      setStatus(state, "err", (err && err.message) || "Could not read safety configuration");
    }).finally(() => {
      if (state.destroyed) return;
      state.loading = false;
      state.reloadBtn.disabled = false;
    });
  }

  function renderSections(state, doc) {
    state.host.innerHTML = "";
    state.controls = [];

    const sections = Array.isArray(doc.sections) ? doc.sections : [];
    if (!sections.length) {
      const card = S.el("div", "page-card safety-card");
      card.appendChild(S.sectionTitle("Safety & Sensors"));
      card.appendChild(S.el("div", "params-desc",
        "Connect to a vehicle to read its safety limits, failsafe actions and sensor "
        + "configuration. The page shows only what the connected firmware actually reports."));
      state.host.appendChild(card);
      S.refreshIcons();
      return;
    }

    sections.forEach((section) => {
      const card = S.el("div", "page-card safety-card");
      card.dataset.section = section.id || "";
      card.appendChild(S.sectionTitle(section.title || ""));
      if (section.hint) card.appendChild(S.el("div", "field-hint", section.hint));
      if (section.kind === "toggle" && section.toggle) {
        card.appendChild(sensorHeader(state, section));
      }
      const fields = section.fields || [];
      if (fields.length) card.appendChild(fieldGrid(state, fields));
      state.host.appendChild(card);
    });

    applyArmed(state, state.armed);
    S.refreshIcons();
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
    }
    const tail = on ? (t.enable || []) : (t.disable || []);
    tail.forEach((w) => writes.push({ name: w.param, value: Number(w.value) }));

    if (!writes.length) {
      setFieldStatus(status, "", "");
      return;
    }

    setFieldStatus(status, "pending", on ? "enabling…" : "disabling…");
    try {
      for (const write of writes) {
        await Corvus.telemetry.postAction("/api/params/set", write);
      }
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
        `${t.label} written — reboot the autopilot for the driver to start`);
    }
    // The chain changes which fields the schema emits, so the page re-reads
    // itself instead of showing a stale layout.
    load(state);
  }

  /** A plain form section: one labelled control per field. */
  function fieldGrid(state, fields) {
    return S.paramFieldGrid(state, fields, { prefix: "safety" });
  }

  function setStatus(state, cls, text) {
    S.setActionsStatus(state.actionsStatus, cls, text);
  }

  return { render };
})();
