"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupBetaflight — the Betaflight sub-page of the Setup page.
 *
 * Betaflight is flown with Corvus over MAVLink telemetry, which carries no
 * settings. Its settings travel over MSP on the flight controller's USB port,
 * and this page reads them from there: firmware and board, the sensors it
 * found, the PIDs and rates of the active profiles, the battery. Reading
 * changes nothing on the controller. Changing it is the Betaflight
 * Configurator's job, and the page says so.
 *
 * The port is not the MAVLink link's: that one is a UART with telemetry on it,
 * this one is the USB cable. The link's own port is listed and marked, and the
 * backend refuses it.
 *
 * Backend contract:
 *   GET  /api/betaflight/status   {ports: [{device, description, link}], link_device, default_baud, bauds}
 *   POST /api/betaflight/read     {device, baud?} -> {ok, variant_name, version, api, board, target,
 *                                  name, sensors, cycle_time_us, cpu_load, pid_profile, rate_profile,
 *                                  pids, rates, battery, unanswered} or {ok: false, error}
 *
 * Exposes render(container, navigateBack) -> destroy().
 */
Corvus.setupBetaflight = (function () {
  const S = Corvus.setupShared;

  const AXES = [["roll", "Roll"], ["pitch", "Pitch"], ["yaw", "Yaw"], ["level", "Level"]];

  const RATE_ROWS = [
    ["rc_rate", "RC rate"],
    ["rc_expo", "RC expo"],
    ["roll", "Super rate roll"],
    ["pitch", "Super rate pitch"],
    ["yaw", "Super rate yaw"],
    ["throttle_mid", "Throttle mid"],
    ["throttle_expo", "Throttle expo"],
    ["tpa_rate", "TPA rate"],
    ["tpa_breakpoint", "TPA breakpoint"],
  ];

  /**
   * The text of one port in the picker: its name, what it says it is, and a
   * mark on the one the MAVLink link holds.
   * Pure, and exported for the test suite.
   * @param {Object} port one entry of GET /api/betaflight/status
   * @returns {string}
   */
  function portLabel(port) {
    const p = port || {};
    const marks = [];
    if (p.link) marks.push("MAVLink link");
    if (p.description) marks.push(p.description);
    return marks.length ? `${p.device} (${marks.join(", ")})` : String(p.device || "");
  }

  /**
   * The rows of the summary card, as [label, value] pairs. A part the
   * controller did not answer is left out rather than shown as zero.
   * Pure, and exported for the test suite.
   * @param {Object} r the reply of POST /api/betaflight/read
   * @returns {Array<[string, string]>}
   */
  function summaryRows(r) {
    const d = r || {};
    const rows = [];
    const firmware = [d.variant_name, d.version].filter(Boolean).join(" ");
    if (firmware) rows.push(["Firmware", firmware]);
    if (d.api) rows.push(["MSP API", d.api]);
    const board = [d.board, d.target && d.target !== d.board ? d.target : ""].filter(Boolean).join(", ");
    if (board) rows.push(["Board", board]);
    if (d.build && d.build.date) {
      rows.push(["Built", [d.build.date, d.build.time].filter(Boolean).join(" ")]);
    }
    if (d.name) rows.push(["Craft name", d.name]);
    if (Array.isArray(d.sensors)) rows.push(["Sensors", d.sensors.length ? d.sensors.join(", ") : "none"]);
    if (Number.isFinite(d.cycle_time_us) && d.cycle_time_us > 0) {
      rows.push(["Loop time", `${d.cycle_time_us} µs (${Math.round(1e6 / d.cycle_time_us)} Hz)`]);
    }
    if (Number.isFinite(d.cpu_load)) rows.push(["CPU load", `${d.cpu_load} %`]);
    if (Number.isFinite(d.pid_profile)) {
      const of = Number.isFinite(d.pid_profile_count) && d.pid_profile_count > 0 ? ` of ${d.pid_profile_count}` : "";
      rows.push(["PID profile", `${d.pid_profile}${of}`]);
    }
    if (Number.isFinite(d.rate_profile)) rows.push(["Rate profile", String(d.rate_profile)]);
    if (d.battery) {
      const b = d.battery;
      rows.push(["Battery", `${b.cells}S, ${b.voltage.toFixed(2)} V, ${b.current.toFixed(1)} A`]);
    }
    return rows;
  }

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page betaflight-page");
    page.appendChild(S.backButton(navigateBack));
    page.appendChild(S.pageHeader("Betaflight",
      "Read a Betaflight flight controller over USB: firmware, sensors, PIDs and rates"));

    let status = null;
    let busy = false;
    let destroyed = false;

    const portSection = S.el("div", "page-section");
    const portCard = S.el("div", "page-card betaflight-port-card");
    portCard.appendChild(S.sectionTitle("Flight controller"));

    const desc = S.el("div", "params-desc");
    desc.textContent =
      "Plug the flight controller in over USB and pick its port. This is not the " +
      "telemetry port that carries MAVLink. Reading changes nothing on the " +
      "controller. To change settings, use the Betaflight Configurator.";
    portCard.appendChild(desc);

    const portSelect = Corvus.ui.select({
      ariaLabel: "Flight controller serial port",
      options: [{ value: "", label: "Looking for serial ports…" }],
      disabled: true,
      onChange: () => { recompute(); },
    });
    portSelect.dataset.role = "port";

    const baudSelect = Corvus.ui.select({
      ariaLabel: "Flight controller baud rate",
      options: [{ value: "115200", label: "115200" }],
    });
    baudSelect.dataset.role = "baud";

    const row = S.el("div", "sik-port-row");
    row.appendChild(Corvus.ui.field({
      label: "Serial port", control: portSelect,
      info: "The USB cable of the flight controller. A USB port ignores the baud rate.",
    }));
    row.appendChild(Corvus.ui.field({
      label: "Baud rate", control: baudSelect,
      info: "Only matters on a UART set to MSP. 115200 is the Betaflight default.",
    }));
    portCard.appendChild(row);

    const readBtn = Corvus.ui.button({
      variant: "primary", icon: "download", label: "Read controller",
      onClick: () => doRead(),
    });
    readBtn.dataset.action = "read";
    const refreshBtn = Corvus.ui.button({
      variant: "ghost", icon: "refresh-cw", label: "Refresh ports",
      onClick: () => refreshStatus(),
    });
    portCard.appendChild(Corvus.ui.actions([readBtn, refreshBtn]));

    const message = Corvus.ui.message({ className: "betaflight-status" });
    portCard.appendChild(message.el);
    portSection.appendChild(portCard);
    page.appendChild(portSection);

    const results = S.el("div", "sik-results betaflight-results");
    page.appendChild(results);
    container.appendChild(page);
    S.refreshIcons();

    function linkPortSelected() {
      return !!(status && status.link_device && portSelect.value &&
        (status.ports || []).some((p) => p.device === portSelect.value && p.link));
    }

    function recompute() {
      readBtn.disabled = busy || !portSelect.value || linkPortSelected();
      if (linkPortSelected()) {
        message.show("This port carries the MAVLink link. Pick the flight controller's USB port.", "err");
      } else if (message.el.classList.contains("err") && !busy) {
        message.hide();
      }
    }

    function fillPorts(data) {
      const ports = Array.isArray(data.ports) ? data.ports : [];
      if (!ports.length) {
        Corvus.ui.setOptions(portSelect, [{ value: "", label: "No serial ports found" }], "");
        portSelect.disabled = true;
        return;
      }
      const options = ports.map((p) => ({ value: p.device, label: portLabel(p) }));
      const usb = ports.find((p) => !p.link) || ports[0];
      Corvus.ui.setOptions(portSelect, options, portSelect.value || usb.device);
      portSelect.disabled = false;
    }

    function fillBauds(data) {
      const bauds = Array.isArray(data.bauds) && data.bauds.length ? data.bauds : [data.default_baud || 115200];
      Corvus.ui.setOptions(baudSelect, bauds.map((b) => ({ value: String(b), label: String(b) })),
        baudSelect.value || String(data.default_baud || 115200));
    }

    async function refreshStatus() {
      try {
        const data = await Corvus.telemetry.requestJson("/api/betaflight/status");
        if (destroyed) return;
        status = data;
        fillPorts(data);
        fillBauds(data);
        recompute();
      } catch (_err) {
        if (destroyed) return;
        message.show("Could not read the serial port list.", "err");
      }
    }

    async function doRead() {
      if (busy || !portSelect.value) return;
      busy = true;
      readBtn.classList.add("is-busy");
      message.show("Reading the flight controller…", "");
      recompute();
      try {
        const data = await Corvus.telemetry.postAction("/api/betaflight/read", {
          device: portSelect.value,
          baud: Number(baudSelect.value) || 115200,
        });
        if (destroyed) return;
        message.hide();
        showResult(data);
      } catch (err) {
        if (destroyed) return;
        results.innerHTML = "";
        message.show((err && err.message) || "Could not read the flight controller.", "err");
      } finally {
        busy = false;
        readBtn.classList.remove("is-busy");
        if (!destroyed) recompute();
      }
    }

    function showResult(data) {
      results.innerHTML = "";
      const summary = S.el("div", "page-card");
      summary.appendChild(S.sectionTitle("Controller"));
      for (const [label, value] of summaryRows(data)) summary.appendChild(S.infoRow(label, value));
      if (data.is_betaflight === false) {
        const warn = S.el("div", "params-banner");
        warn.textContent = "This is not Betaflight. The values below are read the way Betaflight reports them.";
        summary.appendChild(warn);
      }
      results.appendChild(summary);

      if (data.pids) {
        const card = S.el("div", "page-card");
        card.appendChild(S.sectionTitle("PIDs"));
        for (const [key, label] of AXES) {
          const v = data.pids[key];
          if (v) card.appendChild(S.infoRow(label, `P ${v.p}   I ${v.i}   D ${v.d}`));
        }
        results.appendChild(card);
      }
      if (data.rates) {
        const card = S.el("div", "page-card");
        card.appendChild(S.sectionTitle("Rates"));
        for (const [key, label] of RATE_ROWS) {
          if (data.rates[key] !== undefined) card.appendChild(S.infoRow(label, String(data.rates[key])));
        }
        results.appendChild(card);
      }
      if (Array.isArray(data.unanswered) && data.unanswered.length) {
        const note = S.el("div", "params-desc");
        note.textContent = "This firmware did not answer every question, so some values are missing.";
        results.appendChild(note);
      }
      S.refreshIcons();
    }

    refreshStatus();

    return function destroy() {
      destroyed = true;
    };
  }

  return { render, portLabel, summaryRows };
})();
