"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupBattery — the Battery & Power sub-page of the Setup page.
 *
 * Everything about the pack in one place: what is plugged in, how the autopilot
 * measures it, the levels its failsafe reacts to, and — separately — how Corvus
 * itself reads the remaining charge.
 *
 * Two columns, and the split is the point. The left column is the *aircraft*:
 * a battery drawn with the cells it actually has, filled to what is left, with
 * the failsafe levels marked on it and every live number underneath. The right
 * column is everything that can be *changed*. An operator reading a pack and an
 * operator configuring one are doing different jobs, and the reading has to stay
 * on screen while the configuring happens — a cell count typed on the right
 * shows up in the drawing on the left immediately, which is the only way to
 * tell a correct one from a plausible one.
 *
 * Schema-driven like Motors and Safety & Sensors: GET /api/battery returns a
 * *description* built by corvus/battery_config.py (or corvus/ardupilot_battery.py
 * — the page cannot tell which), sections of fields, each field naming the
 * parameter it writes. Nothing here hardcodes a parameter name, so a firmware
 * that lacks one renders one field fewer.
 *
 * The estimator card is the one thing on this page that is *not* a vehicle
 * setting. PX4 publishes a remaining percentage that is a coulomb count seeded
 * by a guess; ArduPilot publishes none at all unless a capacity is configured.
 * Corvus reads the cell voltage against a discharge curve instead and shows
 * both answers, and which one the interface flies by is the operator's choice.
 * Those settings live in the config file, are written through POST /api/config,
 * and stay editable while the vehicle is armed — refusing them would be the
 * ground station refusing to let somebody fix its own display in the one
 * situation where the number matters most.
 *
 * Backend contract:
 *   GET  /api/battery                {connected,sections,pack,settings,configured,chemistries}
 *   POST /api/params/set {name,val}  write one vehicle field (refused while armed)
 *   POST /api/config {battery:{...}} save the estimator settings
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the lifecycle and calls destroy() on back / left-nav re-entry, which
 * releases the telemetry subscription and disowns any in-flight fetch.
 */
Corvus.setupBattery = (function () {
  const S = Corvus.setupShared;
  const { applyArmed, setFieldStatus, notify } = S;

  const NS = "http://www.w3.org/2000/svg";

  // Above this many cells the per-cell voltage labels stop being readable and
  // become a grey smear, so the drawing shows the dividers and puts the spread
  // in the readout instead. 14S exists; 14 legible labels across 220px do not.
  const MAX_LABELLED_CELLS = 8;

  // The estimator settings this page owns, and what an unset one means. Kept
  // here as well as in corvus/battery.py because the form has to draw an
  // "auto" field before any answer has come back from the vehicle.
  const SETTING_DEFAULTS = {
    estimate: false, chemistry: "lipo", cells: 0,
    full_cell: 0, empty_cell: 0, resistance: 0, capacity_mah: 0,
  };

  function render(container, navigateBack) {
    const state = {
      container, navigateBack,
      doc: null,
      armed: false, loading: false, destroyed: false, controls: [],
      status: { cls: "", text: "" },
      // What the operator typed (0 = "work it out"), and the last result of
      // saving it. Both outlive a repaint: a refused save has to keep saying so.
      configured: Object.assign({}, SETTING_DEFAULTS),
      settingsStatus: { cls: "", text: "" },
      // Live telemetry, kept so a repaint can redraw the pack without waiting
      // for the next frame off the link.
      live: null,
      gauge: null,
    };

    const cur = Corvus.telemetry && Corvus.telemetry.getState();
    state.armed = !!(cur && cur.armed);
    state.live = cur || null;

    paint(state);

    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      state.unsub = Corvus.telemetry.subscribe((s) => {
        if (state.destroyed || !s) return;
        state.live = s;
        applyArmed(state, !!s.armed);
        applyLive(state);
      });
    }

    load(state);

    return function destroy() {
      state.destroyed = true;
      if (state.unsub) { try { state.unsub(); } catch (_e) {} state.unsub = null; }
    };
  }

  // -------------------------------------------------------------------------
  // Page chrome
  // -------------------------------------------------------------------------

  function paint(state) {
    if (state.destroyed) return;
    state.container.innerHTML = "";
    state.controls = [];
    state.gauge = null;

    const page = S.el("div", "setup-page");
    page.appendChild(S.backButton(state.navigateBack));
    page.appendChild(S.pageHeader("Battery & Power",
      "The pack, how the autopilot measures it, and how much of it is left"));

    const actions = S.el("div", "params-actions");
    const reloadBtn = Corvus.ui.button({
      variant: "primary", size: "sm", icon: "refresh-cw", label: "Reload",
      className: "battery-reload",
    });
    reloadBtn.disabled = state.loading;
    reloadBtn.addEventListener("click", () => load(state));
    const actionsStatus = S.el("div", "params-actions-status");
    actions.appendChild(reloadBtn);
    actions.appendChild(actionsStatus);
    page.appendChild(actions);

    // Only the vehicle's own parameters are refused while armed. The estimator
    // settings below are Corvus's and stay live — see the module comment.
    const banner = S.el("div", "params-banner");
    banner.hidden = true;
    banner.textContent = "Vehicle battery parameters are read-only while armed";
    page.appendChild(banner);

    const layout = S.el("div", "battery-layout");
    const left = S.el("div", "battery-live");
    const right = S.el("div", "battery-config");
    layout.appendChild(left);
    layout.appendChild(right);
    page.appendChild(layout);
    state.container.appendChild(page);

    state.reloadBtn = reloadBtn;
    state.actionsStatus = actionsStatus;
    state.banner = banner;
    state.host = right;
    S.setActionsStatus(actionsStatus, state.status.cls, state.status.text);

    left.appendChild(livePanel(state));
    right.appendChild(estimatorCard(state));
    renderSections(state);

    applyArmed(state, state.armed);
    applyLive(state);
    S.refreshIcons();
  }

  /** Fetch the configuration and rebuild from the response. */
  function load(state) {
    if (state.loading) return Promise.resolve();
    state.loading = true;
    if (state.reloadBtn) state.reloadBtn.disabled = true;
    setStatus(state, "pending", "Reading battery configuration…");
    return Corvus.telemetry.requestJson("/api/battery").then((doc) => {
      if (state.destroyed) return;
      state.doc = doc || {};
      state.configured = Object.assign({}, SETTING_DEFAULTS, (doc && doc.configured) || {});
      if (!doc || !doc.connected) {
        state.status = {
          cls: "err",
          text: (doc && doc.error) || "No battery configuration received",
        };
      } else {
        state.status = { cls: "ok", text: `${doc.received} parameters read` };
      }
    }).catch((err) => {
      if (state.destroyed) return;
      state.doc = {};
      state.status = {
        cls: "err", text: (err && err.message) || "Could not read battery configuration",
      };
    }).finally(() => {
      if (state.destroyed) return;
      state.loading = false;
      paint(state);
    });
  }

  function setStatus(state, cls, text) {
    state.status = { cls, text };
    S.setActionsStatus(state.actionsStatus, cls, text);
  }

  // -------------------------------------------------------------------------
  // Left column: the pack as it is right now
  // -------------------------------------------------------------------------

  /**
   * The drawing and the numbers under it.
   *
   * Built once per repaint; everything that moves is written through refs held
   * in `state.gauge`, so a 30 Hz telemetry push costs a handful of text
   * assignments rather than a rebuilt DOM.
   */
  function livePanel(state) {
    const card = S.el("div", "page-card battery-card battery-gauge-card");
    card.appendChild(S.sectionTitle("Pack"));

    const svg = document.createElementNS(NS, "svg");
    svg.setAttribute("viewBox", "0 0 260 132");
    svg.setAttribute("class", "battery-figure");
    svg.setAttribute("role", "img");
    svg.setAttribute("aria-label", "Battery charge and cell voltages");
    card.appendChild(svg);

    const readout = S.el("div", "battery-readout");
    card.appendChild(readout);

    const rows = [
      ["remaining", "Remaining"],
      ["voltage", "Pack voltage"],
      ["cell", "Per cell"],
      ["current", "Current"],
      ["power", "Power"],
      ["consumed", "Used"],
      ["capacity", "Capacity"],
      ["temperature", "Temperature"],
      ["endurance", "Time left"],
      ["fc", "Autopilot says"],
    ];
    const values = {};
    const labels = {};
    rows.forEach(([key, text]) => {
      const row = S.el("div", "battery-readout-row");
      row.dataset.readout = key;
      const label = S.el("span", "battery-readout-label", text);
      labels[key] = label;
      row.appendChild(label);
      const value = S.el("span", "battery-readout-value", "—");
      values[key] = value;
      row.appendChild(value);
      readout.appendChild(row);
    });

    state.gauge = { svg, values, labels, cells: -1, signature: "" };
    return card;
  }

  /**
   * Redraw the battery body for a given cell count.
   *
   * Only called when the count actually changes. The body is one fill across
   * the whole pack — that is what "remaining" means — and the cells are
   * dividers over it rather than N separate gauges, because a per-compartment
   * fill would be a claim about individual cells that a pack voltage cannot
   * support.
   */
  function drawFigure(state, cells, thresholds) {
    const gauge = state.gauge;
    if (!gauge) return;
    const svg = gauge.svg;
    svg.innerHTML = "";

    const x0 = 8;
    const x1 = 228;
    const y0 = 16;
    const y1 = 86;
    const width = x1 - x0;

    const el = (name, attrs, text) => {
      const node = document.createElementNS(NS, name);
      if (attrs) Object.keys(attrs).forEach((k) => node.setAttribute(k, attrs[k]));
      if (text != null) node.textContent = text;
      return node;
    };

    // Terminal first so the body's stroke draws over its root.
    svg.appendChild(el("rect", {
      x: x1, y: (y0 + y1) / 2 - 12, width: 14, height: 24, rx: 4,
      class: "battery-terminal",
    }));
    svg.appendChild(el("rect", {
      x: x0, y: y0, width, height: y1 - y0, rx: 8, class: "battery-body",
    }));

    // The fill is clipped to the body so its square end never pokes out of the
    // rounded corner at 100%.
    const clipId = "battery-clip-" + Math.random().toString(36).slice(2, 8);
    const defs = el("defs");
    const clip = el("clipPath", { id: clipId });
    clip.appendChild(el("rect", { x: x0, y: y0, width, height: y1 - y0, rx: 8 }));
    defs.appendChild(clip);
    svg.appendChild(defs);

    const fillGroup = el("g", { "clip-path": `url(#${clipId})` });
    const fill = el("rect", {
      x: x0, y: y0, width: 0, height: y1 - y0, class: "battery-fill",
    });
    fillGroup.appendChild(fill);
    svg.appendChild(fillGroup);

    // Cell dividers and their labels. A pack whose cell count is unknown gets
    // no dividers at all rather than a guessed number of them.
    const labels = [];
    const count = cells > 0 ? cells : 0;
    if (count > 1) {
      for (let i = 1; i < count; i += 1) {
        const x = x0 + (width * i) / count;
        svg.appendChild(el("line", {
          x1: x, y1: y0, x2: x, y2: y1, class: "battery-divider",
        }));
      }
    }
    if (count > 0 && count <= MAX_LABELLED_CELLS) {
      for (let i = 0; i < count; i += 1) {
        const x = x0 + (width * (i + 0.5)) / count;
        const text = el("text", {
          x: x.toFixed(1), y: y1 + 16, class: "battery-cell-label",
          "text-anchor": "middle",
        }, "—");
        svg.appendChild(text);
        labels.push(text);
      }
    }

    // The failsafe levels, on the scale they are actually read against. A
    // threshold the backend sent in volts is only drawable once a cell count
    // is known, so it is dropped rather than placed at a guessed position.
    const marks = [];
    (thresholds || []).forEach((threshold) => {
      const percent = thresholdPercent(state, threshold, cells);
      if (percent == null) return;
      const x = x0 + (width * Math.max(0, Math.min(100, percent))) / 100;
      svg.appendChild(el("line", {
        x1: x, y1: y0 - 6, x2: x, y2: y1 + 4,
        class: "battery-threshold " + (threshold.id || ""),
      }));
      svg.appendChild(el("text", {
        x: x.toFixed(1), y: y0 - 9, class: "battery-threshold-label",
        "text-anchor": "middle",
      }, threshold.label || ""));
      marks.push(threshold);
    });

    const percentText = el("text", {
      x: (x0 + width / 2).toFixed(1), y: ((y0 + y1) / 2 + 9).toFixed(1),
      class: "battery-percent-label", "text-anchor": "middle",
    }, "—");
    svg.appendChild(percentText);

    const sourceText = el("text", {
      x: (x0 + width / 2).toFixed(1), y: y1 + 34,
      class: "battery-source-label", "text-anchor": "middle",
    }, "");
    svg.appendChild(sourceText);

    gauge.cells = cells;
    gauge.fill = fill;
    gauge.cellLabels = labels;
    gauge.percentText = percentText;
    gauge.sourceText = sourceText;
    gauge.geometry = { x0, width };
    gauge.marks = marks;
  }

  /**
   * Where a threshold sits on a 0–100 scale, or null when it cannot be placed.
   *
   * PX4 states its levels as a fraction of the pack and they map straight
   * across. ArduPilot states them in volts, which is only a position on this
   * scale once the cell count and the chemistry endpoints are known — and a
   * marker placed by guesswork on the one drawing an operator uses to decide
   * whether to land would be worse than no marker.
   */
  function thresholdPercent(state, threshold, cells) {
    if (threshold == null) return null;
    if (typeof threshold.percent === "number") return threshold.percent;
    if (typeof threshold.volts === "number" && cells > 0) {
      const chem = chemistry(state);
      const full = resolvedNumber(state, "full_cell", chem.full);
      const empty = resolvedNumber(state, "empty_cell", chem.empty);
      if (!(full > empty)) return null;
      const perCell = threshold.volts / cells;
      return ((perCell - empty) / (full - empty)) * 100;
    }
    return null;
  }

  /** Push the latest telemetry into the drawing and the readout. */
  function applyLive(state) {
    const gauge = state.gauge;
    if (!gauge) return;
    const s = state.live || {};
    const pack = (state.doc && state.doc.pack) || {};
    const connected = !!s.connected;

    // The cell count the backend resolved wins; the vehicle's own parameter is
    // the fallback for a pack that has not been measured yet (no link, or a
    // monitor that reports no voltage).
    const cells = Number(s.battery_cells) > 0
      ? Number(s.battery_cells)
      : Number(pack.cells) || 0;
    // Redrawn only when the drawing itself would differ. The readout below is
    // written on every frame; rebuilding twenty SVG nodes at 30 Hz is not.
    const thresholds = Array.isArray(pack.thresholds) ? pack.thresholds : [];
    const signature = cells + "|" + JSON.stringify(thresholds);
    if (signature !== gauge.signature) {
      drawFigure(state, cells, thresholds);
      gauge.signature = signature;
    }

    const percent = connected ? Number(s.battery_percent) || 0 : 0;
    const voltage = connected ? Number(s.battery_voltage) || 0 : 0;
    const current = connected ? Number(s.battery_current) || 0 : 0;
    const estimated = Number(s.battery_percent_est);
    const reported = Number(s.battery_percent_fc);
    const usingEstimate = s.battery_source === "estimate";

    if (gauge.fill) {
      const width = gauge.geometry.width * Math.max(0, Math.min(100, percent)) / 100;
      gauge.fill.setAttribute("width", width.toFixed(1));
      gauge.fill.setAttribute("class", "battery-fill " + level(percent, connected));
    }
    if (gauge.percentText) {
      gauge.percentText.textContent = connected ? `${Math.round(percent)}%` : "—";
    }
    if (gauge.sourceText) {
      gauge.sourceText.textContent = !connected ? ""
        : usingEstimate ? "from cell voltage" : "from the autopilot";
    }

    // Per-cell labels: the pack's own report when it makes one, the pack
    // voltage divided by the count when it does not. The two are not the same
    // claim, so a measured set is marked as such in the readout below.
    const measured = Array.isArray(s.battery_cell_voltages) ? s.battery_cell_voltages : [];
    (gauge.cellLabels || []).forEach((label, index) => {
      if (!connected) { label.textContent = "—"; return; }
      const volts = measured.length > index
        ? measured[index]
        : (cells > 0 ? voltage / cells : 0);
      label.textContent = volts > 0 ? `${volts.toFixed(2)} V` : "—";
    });

    const v = gauge.values;
    const dash = "—";
    v.remaining.textContent = connected ? `${Math.round(percent)}%` : dash;
    v.remaining.className = "battery-readout-value " + level(percent, connected);
    v.voltage.textContent = connected && voltage > 0 ? `${voltage.toFixed(1)} V` : dash;
    v.cell.textContent = cellText(connected, s, voltage, cells, measured);
    v.current.textContent = connected ? `${current.toFixed(1)} A` : dash;
    v.power.textContent = connected && voltage > 0
      ? `${Math.round(voltage * current)} W` : dash;
    v.consumed.textContent = Number(s.battery_consumed_mah) > 0
      ? `${Math.round(s.battery_consumed_mah)} mAh` : dash;
    v.capacity.textContent = Number(pack.capacity_mah) > 0
      ? `${Math.round(pack.capacity_mah)} mAh` : dash;
    v.temperature.textContent = typeof s.battery_temperature === "number"
      ? `${s.battery_temperature.toFixed(1)} °C` : dash;
    v.endurance.textContent = Number(s.battery_time_remaining) > 0
      ? formatDuration(s.battery_time_remaining) : dash;

    // The answer the operator is NOT flying by, named so the two can be
    // compared without switching the setting back and forth.
    gauge.labels.fc.textContent = usingEstimate ? "Autopilot says" : "Cell voltage says";
    if (!connected) {
      v.fc.textContent = dash;
    } else if (usingEstimate) {
      v.fc.textContent = reported >= 0 ? `${Math.round(reported)}%` : "no estimate";
    } else {
      v.fc.textContent = estimated >= 0 ? `${Math.round(estimated)}%` : "no cell count";
    }
  }

  function cellText(connected, s, voltage, cells, measured) {
    if (!connected || cells <= 0) return "—";
    if (measured.length > 1) {
      const low = Math.min.apply(null, measured);
      const high = Math.max.apply(null, measured);
      const spread = (high - low) * 1000;
      return `${low.toFixed(2)}–${high.toFixed(2)} V (${Math.round(spread)} mV apart)`;
    }
    const perCell = Number(s.battery_cell_voltage) > 0
      ? Number(s.battery_cell_voltage)
      : (voltage > 0 ? voltage / cells : 0);
    if (!(perCell > 0)) return "—";
    return `${perCell.toFixed(2)} V × ${cells}S`;
  }

  /** Healthy / warning / critical, matching the top bar's own thresholds. */
  function level(percent, connected) {
    if (!connected) return "off";
    if (percent > 25) return "healthy";
    return percent > 12 ? "warning" : "critical";
  }

  function formatDuration(seconds) {
    const total = Math.max(0, Math.round(Number(seconds) || 0));
    const minutes = Math.floor(total / 60);
    const rest = total % 60;
    if (minutes >= 60) {
      return `${Math.floor(minutes / 60)} h ${minutes % 60} min`;
    }
    return minutes > 0 ? `${minutes} min ${rest} s` : `${rest} s`;
  }

  // -------------------------------------------------------------------------
  // Right column: the estimator, then the vehicle's own parameters
  // -------------------------------------------------------------------------

  function chemistry(state) {
    const list = (state.doc && state.doc.chemistries) || [];
    const wanted = state.configured.chemistry || SETTING_DEFAULTS.chemistry;
    return list.find((c) => c.value === wanted)
      || list[0]
      || { value: wanted, label: wanted, full: 4.2, empty: 3.3, nominal: 3.7 };
  }

  /** A setting's effective value: what the operator pinned, else the default. */
  function resolvedNumber(state, key, fallback) {
    const value = Number(state.configured[key]);
    return value > 0 ? value : fallback;
  }

  function estimatorCard(state) {
    const card = S.el("div", "page-card battery-card battery-estimator-card");
    card.dataset.section = "estimator";
    card.appendChild(S.sectionTitle("How Corvus reads the pack"));
    card.appendChild(S.el("div", "field-hint",
      "The autopilot's remaining figure is a capacity count that starts from a "
      + "guess, so a pack flown, charged to storage and flown again reads full on "
      + "the second take-off. Corvus can read the cell voltage against a discharge "
      + "curve instead. Neither is right in every case — both are shown on the left, "
      + "and this chooses which one the rest of the interface flies by."));

    const head = S.el("div", "battery-estimate-switch");
    const label = S.el("div", "battery-estimate-label");
    label.appendChild(S.el("span", "battery-estimate-title", "Use the cell-voltage estimate"));
    label.appendChild(S.el("span", "battery-estimate-sub",
      "Replaces the percentage in the top bar, the map and the logs."));
    head.appendChild(label);

    const status = S.el("span", "params-row-status");
    const toggle = Corvus.ui.toggle({
      className: "battery-estimate-toggle",
      value: !!state.configured.estimate,
      ariaLabel: "Use the cell-voltage estimate",
      onChange: (next) => saveSettings(state, { estimate: next }, status),
    });
    head.appendChild(toggle.el);
    card.appendChild(head);
    card.appendChild(status);

    const grid = S.el("div", "pform-grid battery-grid");
    grid.appendChild(chemistryRow(state));
    grid.appendChild(numberRow(state, {
      key: "cells", label: "Cells in series", unit: "S", step: 1, min: 0, max: 24,
      hint: "0 works it out from the pack voltage when the battery is plugged "
            + "in, which is the only moment that reading is unambiguous — 19.8 V "
            + "is a fresh 5S and a tired 6S. Pin it if you know it.",
      autoText: () => {
        const cells = Number((state.live || {}).battery_cells) || 0;
        return cells > 0 ? `auto — reading ${cells}S` : "auto";
      },
    }));
    grid.appendChild(numberRow(state, {
      key: "full_cell", label: "Full cell voltage", unit: "V", step: 0.01,
      min: 0, max: 5,
      hint: "Resting volts of one charged cell, at which the estimate reads 100%.",
      autoText: () => `auto — ${chemistry(state).full} V`,
    }));
    grid.appendChild(numberRow(state, {
      key: "empty_cell", label: "Empty cell voltage", unit: "V", step: 0.01,
      min: 0, max: 5,
      hint: "The landing decision, not the cell's datasheet minimum. The estimate "
            + "reads 0% here.",
      autoText: () => `auto — ${chemistry(state).empty} V`,
    }));
    grid.appendChild(numberRow(state, {
      key: "resistance", label: "Internal resistance", unit: "mΩ/cell", step: 0.1,
      min: 0, max: 100,
      hint: "Corrects the voltage back to rest before it is read. A 6S pack "
            + "pulling 60 A through 5 mΩ a cell reads 1.8 V low — thirty points "
            + "of charge, in a climb. 0 leaves the reading uncorrected, which is "
            + "pessimistic rather than wrong.",
      autoText: () => "off",
    }));
    card.appendChild(grid);

    const actions = S.el("div", "battery-estimator-actions");
    const copy = Corvus.ui.button({
      variant: "ghost", size: "sm", icon: "download", label: "Take these from the vehicle",
      className: "battery-copy",
    });
    copy.disabled = !hasVehiclePack(state);
    copy.addEventListener("click", () => copyFromVehicle(state, status));
    actions.appendChild(copy);
    card.appendChild(actions);

    return card;
  }

  /** True when the vehicle actually told us something worth copying. */
  function hasVehiclePack(state) {
    const pack = (state.doc && state.doc.pack) || {};
    return !!(pack.cells || pack.full_cell || pack.empty_cell || pack.resistance_ohm);
  }

  /**
   * Fill the estimator from the autopilot's own pack description.
   *
   * One press rather than four retyped numbers, and on PX4 it is the same pack
   * the autopilot is already reading — which is the point: an estimate built on
   * different endpoints than the failsafe would disagree with it for no reason
   * an operator could see. ArduPilot has none of these parameters, so the
   * button stays disabled there and the numbers are the operator's own.
   */
  function copyFromVehicle(state, status) {
    const pack = (state.doc && state.doc.pack) || {};
    const patch = {};
    if (pack.cells > 0) patch.cells = pack.cells;
    if (pack.full_cell > 0) patch.full_cell = pack.full_cell;
    if (pack.empty_cell > 0) patch.empty_cell = pack.empty_cell;
    // BAT1_R_INTERNAL is ohms for the whole pack; this field is milliohms per
    // cell. Converting in the wrong direction here would be a 6000x error in
    // the sag correction, so it is done once, explicitly.
    if (pack.resistance_ohm > 0 && pack.cells > 0) {
      patch.resistance = Math.round((pack.resistance_ohm / pack.cells) * 1000 * 10) / 10;
    }
    if (!Object.keys(patch).length) {
      setFieldStatus(status, "err", "the vehicle reported no pack to copy");
      return Promise.resolve();
    }
    return saveSettings(state, patch, status);
  }

  function chemistryRow(state) {
    const row = S.el("div", "pform-field battery-field");
    row.dataset.setting = "chemistry";
    row.appendChild(S.el("span", "pform-field-label battery-field-label", "Chemistry"));
    const cell = S.el("div", "pform-field-control battery-field-control");
    const options = ((state.doc && state.doc.chemistries) || []).map((c) => ({
      value: c.value, label: `${c.label} (${c.empty}–${c.full} V)`,
    }));
    const status = S.el("span", "params-row-status");
    const select = Corvus.ui.select({
      className: "pform-select battery-select",
      ariaLabel: "Battery chemistry",
      options: options.length ? options : [{ value: "lipo", label: "LiPo" }],
      value: state.configured.chemistry || SETTING_DEFAULTS.chemistry,
    });
    select.addEventListener("change", () => {
      saveSettings(state, { chemistry: select.value }, status);
    });
    cell.appendChild(select);
    cell.appendChild(status);
    row.appendChild(cell);
    row.appendChild(S.el("span", "field-hint pform-field-hint",
      "Which discharge curve the cell voltage is read against. A LiFePO4 cell at "
      + "3.3 V is nearly full; a LiPo cell at 3.3 V is empty."));
    return row;
  }

  /**
   * One numeric estimator setting.
   *
   * Empty means "work it out", which is why the input is left blank rather than
   * pre-filled with the resolved value: a form showing 4.2 cannot say whether
   * the operator pinned 4.2 or the chemistry did, and the difference decides
   * what happens when they later switch to LiFePO4.
   */
  function numberRow(state, spec) {
    const row = S.el("div", "pform-field battery-field");
    row.dataset.setting = spec.key;
    row.appendChild(S.el("span", "pform-field-label battery-field-label", spec.label));

    const cell = S.el("div", "pform-field-control battery-field-control");
    const status = S.el("span", "params-row-status");
    const current = Number(state.configured[spec.key]) || 0;
    const input = Corvus.ui.input({
      className: "pform-input battery-input",
      mono: true,
      value: current > 0 ? S.formatNumber(current) : "",
      placeholder: spec.autoText ? spec.autoText() : "auto",
      ariaLabel: spec.label,
      autocomplete: false,
    });
    input.step = String(spec.step);
    input.addEventListener("change", () => {
      const raw = String(input.value || "").trim();
      if (raw === "") {
        input.classList.remove("invalid");
        saveSettings(state, { [spec.key]: 0 }, status);
        return;
      }
      const problem = S.rangeProblem({ min: spec.min, max: spec.max }, raw);
      if (problem) {
        input.classList.add("invalid");
        setFieldStatus(status, "err", problem);
        return;
      }
      input.classList.remove("invalid");
      saveSettings(state, { [spec.key]: Number(raw) }, status);
    });
    cell.appendChild(input);
    if (spec.unit) cell.appendChild(S.el("span", "pform-unit", spec.unit));
    cell.appendChild(status);
    row.appendChild(cell);
    if (spec.hint) row.appendChild(S.el("span", "field-hint pform-field-hint", spec.hint));
    return row;
  }

  /**
   * Persist part of the estimator settings.
   *
   * POST /api/config merges per key, so a patch naming one field leaves the
   * rest alone. The backend hands the settings straight to the bridge, so the
   * next telemetry frame already carries the new reading — which is why the
   * left column is redrawn from the response rather than waiting for a reload.
   */
  function saveSettings(state, patch, status) {
    setFieldStatus(status, "pending", "saving");
    return Corvus.telemetry.requestJson("/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ battery: patch }),
    }).then((data) => {
      if (state.destroyed) return;
      const saved = (data && data.config && data.config.battery) || {};
      state.configured = Object.assign({}, SETTING_DEFAULTS, saved);
      setFieldStatus(status, "ok", "saved");
      // The placeholders and the drawing both read from the settings, so the
      // card is rebuilt rather than patched field by field.
      repaintEstimator(state);
      applyLive(state);
    }).catch((err) => {
      if (state.destroyed) return;
      const message = (err && err.message) || "save failed";
      setFieldStatus(status, "err", message);
      notify("critical", `Could not save the battery settings: ${message}`);
    });
  }

  /** Swap the estimator card for a freshly built one, in place. */
  function repaintEstimator(state) {
    if (!state.host) return;
    const old = state.host.querySelector(".battery-estimator-card");
    const next = estimatorCard(state);
    if (old && old.parentNode) {
      S.dropControls(state, old);
      old.parentNode.replaceChild(next, old);
    } else {
      state.host.appendChild(next);
    }
    applyArmed(state, state.armed);
    S.refreshIcons();
  }

  /** The vehicle's own parameters, one card per section the backend sent. */
  function renderSections(state) {
    if (state.doc === null) return;
    const sections = Array.isArray(state.doc.sections) ? state.doc.sections : [];
    if (!sections.length) {
      const card = S.el("div", "page-card battery-card");
      card.dataset.section = "empty";
      card.appendChild(S.sectionTitle("Vehicle settings"));
      card.appendChild(S.el("div", "params-desc",
        "Connect to a vehicle to read the pack it is configured for. The settings "
        + "above are Corvus's own and are saved either way."));
      state.host.appendChild(card);
      return;
    }
    sections.forEach((section) => {
      const card = S.el("div", "page-card battery-card");
      card.dataset.section = section.id || "";
      card.appendChild(S.sectionTitle(section.title || ""));
      if (section.hint) card.appendChild(S.el("div", "field-hint", section.hint));
      const fields = section.fields || [];
      if (fields.length) {
        card.appendChild(S.paramFieldGrid(state, fields, { prefix: "battery" }));
      }
      state.host.appendChild(card);
    });
  }

  return { render };
})();
