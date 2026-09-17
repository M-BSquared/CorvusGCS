"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupSik — the Telemetry Radio sub-page of the Setup page.
 *
 * Configures a SiK telemetry radio pair: the settings Mission Planner exposes
 * on its "Initial Setup | Optional Hardware | SiK Radio" screen, in the idiom
 * the rest of Corvus's setup pages use.
 *
 * Why this page does not look like the other setup pages under the hood
 * ---------------------------------------------------------------------
 * Every other page here edits the autopilot, so a change is one PARAM_SET that
 * the vehicle acknowledges and that takes effect on the spot. A radio is not on
 * that path: it is the modem the telemetry passes through, it keeps its own
 * EEPROM, and reaching it means taking its serial port away from the MAVLink
 * bridge and talking AT commands down it. So this page has no live edits and no
 * per-field apply. It has Load, which takes a snapshot, and Save, which writes
 * a batch and reboots the radios into it — because every write costs a reboot,
 * and a page that rebooted the link on each keystroke would be unusable.
 *
 * Two radios, side by side
 * ------------------------
 * A telemetry link is a pair, and eight of its settings have to be identical at
 * both ends or the two radios cannot hear each other at all. That is the reason
 * for the two columns rather than a preference for symmetry: the failure this
 * page exists to fix — a link that will not come up — is usually one of those
 * eight having drifted, and it is invisible unless both ends are on screen at
 * once. The backend reads the far radio through the near one (``RT`` in place
 * of ``AT``, relayed over the air), flags the ones that disagree, and the Copy
 * button resolves them in the only safe direction: outward, to the radio on the
 * aircraft, before the one on the cable is touched.
 *
 * Backend contract:
 *   GET  /api/sik/status  {ports[],link_device,link_baud,armed,busy,
 *                          can_configure,blocked_reason,schema}
 *   POST /api/sik/load    {device,baud?,remote?} -> {local,remote,mismatches,
 *                                                    link,remote_reachable}
 *   POST /api/sik/save    {device,baud?,local?,remote?} -> {applied,warnings}
 *   POST /api/sik/reset   {device,baud?,target} -> {target}
 *
 * All three actions are POST including the read, because loading settings takes
 * the port from the bridge and puts a radio into command mode — a side effect on
 * the aircraft's telemetry link, not a fetch.
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the lifecycle: destroy() releases the telemetry subscription and marks
 * the page gone, so a session's late answer never writes to a DOM this page no
 * longer owns.
 */
Corvus.setupSik = (function () {
  const S = Corvus.setupShared;

  /** The two ends of the link, as the operator thinks of them. */
  const SIDES = [
    { id: "local", title: "This radio" },
    { id: "remote", title: "Remote radio" },
  ];

  /**
   * What the two columns are, said once above both of them.
   *
   * It used to be a sentence inside each card, which is the obvious place for
   * it and the wrong one: the two sentences are not the same length, so they
   * wrap to different numbers of lines and push each column's first field to a
   * different height. That costs exactly the thing the two columns are for —
   * reading a value off one and comparing it with the other on the same line.
   */
  const COLUMNS_INTRO =
    "Left is the radio on the cable in front of you, normally the ground " +
    "station's. Right is the one at the other end of the link, reached over the " +
    "air through the near one — it needs power at both ends and a solid green " +
    "LED on both radios.";

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page sik-page");
    page.appendChild(S.backButton(navigateBack));
    page.appendChild(S.pageHeader("Telemetry Radio",
      "Read and program a SiK radio pair: network ID, air rate, power and band"));

    // Everything the page has read so far. Null until the first successful
    // Load; the settings cards do not exist before then, because there is
    // nothing truthful to put in them — a form pre-filled with defaults would
    // invite the operator to "save" a configuration they never saw.
    let loaded = null;
    // Pending edits per side, keyed by register name. Only these are sent, so
    // a save never rewrites a register the operator did not touch.
    const edits = { local: {}, remote: {} };
    let schema = null;
    let status = null;
    let showAdvanced = false;
    let busy = false;
    let destroyed = false;
    let telemetryUnsub = null;

    // --- Port card --------------------------------------------------------
    const portSection = S.el("div", "page-section");
    const portCard = S.el("div", "page-card");
    portCard.appendChild(S.sectionTitle("Radio port"));

    const portDesc = S.el("div", "params-desc");
    portDesc.textContent =
      "Pick the serial port the radio is on and press Load. While the radio is " +
      "in command mode it is not relaying telemetry, so if this is the port the " +
      "live link runs on, Corvus stops the link for the few seconds a session " +
      "takes and reconnects afterwards.";
    portCard.appendChild(portDesc);

    const gateBanner = S.el("div", "params-banner");
    gateBanner.hidden = true;
    portCard.appendChild(gateBanner);

    const portSelect = Corvus.ui.select({
      ariaLabel: "Radio serial port",
      options: [{ value: "", label: "Looking for serial ports…" }],
      disabled: true,
      onChange: () => { recomputeGate(); },
    });
    portSelect.dataset.role = "port";

    const baudSelect = Corvus.ui.select({
      ariaLabel: "Radio baud rate",
      options: [{ value: "", label: "57600" }],
      disabled: true,
    });
    baudSelect.dataset.role = "baud";

    const remoteToggle = Corvus.ui.select({
      ariaLabel: "Read the remote radio",
      options: [
        { value: "1", label: "Read both radios" },
        { value: "0", label: "This radio only" },
      ],
      value: "1",
    });
    remoteToggle.dataset.role = "scope";

    const portRow = S.el("div", "sik-port-row");
    portRow.appendChild(Corvus.ui.field({
      label: "Serial port", control: portSelect,
      hint: "A radio on a USB cable, or the port the live link already runs on.",
    }));
    portRow.appendChild(Corvus.ui.field({
      label: "Baud rate", control: baudSelect,
      hint: "The speed of this cable, not of the air link. If the radio does " +
            "not answer, Corvus tries the other common rates before giving up.",
    }));
    portRow.appendChild(Corvus.ui.field({
      label: "Scope", control: remoteToggle,
      hint: "Reading the far radio takes a few seconds longer and needs it powered.",
    }));
    portCard.appendChild(portRow);

    const loadBtn = Corvus.ui.button({
      variant: "primary", icon: "download", label: "Load settings",
      onClick: () => doLoad(),
    });
    loadBtn.dataset.action = "load";
    portCard.appendChild(Corvus.ui.actions([loadBtn]));

    const statusMsg = Corvus.ui.message({ className: "sik-status" });
    portCard.appendChild(statusMsg.el);

    portSection.appendChild(portCard);
    page.appendChild(portSection);

    // --- Everything below is built on Load and replaced on every reload ----
    const resultHost = S.el("div", "sik-results");
    page.appendChild(resultHost);

    container.appendChild(page);
    S.refreshIcons();

    // ------------------------------------------------------------------
    // Status, gating
    // ------------------------------------------------------------------

    /**
     * The one place that decides whether the actions are live. Both the polled
     * backend gate and the live telemetry armed flag feed it: the snapshot can
     * be seconds stale, and arming is exactly the event that must disable this
     * page instantly rather than at the next poll.
     */
    function recomputeGate() {
      const armedNow = isArmed();
      const reason = armedNow
        ? "Radio configuration is refused while the vehicle is armed — a radio in " +
          "command mode is not relaying telemetry."
        : (status && !status.can_configure ? (status.blocked_reason || "") : "");
      gateBanner.hidden = !reason;
      gateBanner.textContent = reason;
      const blocked = !!reason || busy || !portSelect.value;
      loadBtn.disabled = blocked;
      resultHost.querySelectorAll("[data-needs-gate]").forEach((el) => {
        el.disabled = blocked;
      });
    }

    function isArmed() {
      const t = Corvus.telemetry && Corvus.telemetry.getState
        ? Corvus.telemetry.getState() : null;
      return !!(t && t.armed);
    }

    function setBusy(next, text) {
      busy = !!next;
      loadBtn.classList.toggle("is-busy", busy);
      if (text) statusMsg.show(text, "");
      recomputeGate();
    }

    async function refreshStatus() {
      try {
        const data = await Corvus.telemetry.requestJson("/api/sik/status");
        if (destroyed) return;
        status = data;
        schema = data.schema || schema;
        fillPorts(data);
        fillBauds(data);
        recomputeGate();
      } catch (err) {
        if (destroyed) return;
        statusMsg.show("Could not read the serial port list.", "err");
      }
    }

    /**
     * Fill the port picker, preselecting the link's own port.
     *
     * That default is the right one nearly always — the radio an operator wants
     * to configure is the one they are already talking to — and it is also the
     * one that costs a telemetry interruption, so the option label says so
     * rather than leaving them to find out.
     */
    function fillPorts(data) {
      const ports = Array.isArray(data.ports) ? data.ports : [];
      if (!ports.length) {
        Corvus.ui.setOptions(portSelect, [{ value: "", label: "No serial ports found" }], "");
        portSelect.disabled = true;
        return;
      }
      const previous = portSelect.value;
      const options = ports.map((p) => {
        const marks = [];
        if (p.is_link) marks.push("live link");
        else if (p.kind === "sik") marks.push("radio");
        if (p.description) marks.push(p.description);
        return {
          value: p.device,
          label: marks.length ? `${p.device} — ${marks.join(", ")}` : p.device,
        };
      });
      const preferred = previous
        || (ports.find((p) => p.is_link) || {}).device
        || (ports.find((p) => p.kind === "sik") || {}).device
        || ports[0].device;
      Corvus.ui.setOptions(portSelect, options, preferred);
      portSelect.disabled = false;
    }

    function fillBauds(data) {
      const bauds = Array.isArray(data.bauds) && data.bauds.length
        ? data.bauds : [57600];
      const preferred = String(
        portSelect.value && data.link_device === portSelect.value && data.link_baud
          ? data.link_baud
          : (data.default_baud || 57600)
      );
      Corvus.ui.setOptions(
        baudSelect,
        bauds.map((b) => ({ value: String(b), label: `${b} baud` })),
        preferred
      );
      baudSelect.disabled = false;
    }

    // ------------------------------------------------------------------
    // Requests
    // ------------------------------------------------------------------

    /**
     * POST one radio action through the shared helper.
     *
     * `postAction` already turns a non-2xx or an `ok:false` body into a thrown
     * Error carrying the backend's own message, which is the whole point: the
     * refusals this page shows — a radio that is not powered, a port something
     * else has open, a vehicle that is armed — are written once, in the
     * service, and are never restated here.
     *
     * There is no cancellation. A session cannot meaningfully be called back
     * once it has started: the radio is already in command mode or already
     * rebooting, and the bridge restart at the end of it has to run whatever
     * this page is doing. What destroy() guarantees instead is that a late
     * answer never touches a DOM this page no longer owns, which is what the
     * `destroyed` flag every caller checks is for.
     */
    function post(path, body) {
      return Corvus.telemetry.postAction(path, body);
    }

    function target() {
      const baud = parseInt(baudSelect.value, 10);
      const body = { device: portSelect.value };
      if (Number.isFinite(baud) && baud > 0) body.baud = baud;
      return body;
    }

    async function doLoad() {
      if (busy || !portSelect.value) return;
      setBusy(true, "Entering command mode — this takes a few seconds…");
      try {
        const body = target();
        body.remote = remoteToggle.value !== "0";
        const data = await post("/api/sik/load", body);
        if (destroyed) return;
        loaded = data;
        edits.local = {};
        edits.remote = {};
        renderResults();
        const notes = [];
        if (data.remote && !data.remote_reachable) notes.push("the remote radio did not answer");
        statusMsg.show(
          data.mismatches && data.mismatches.length
            ? `Settings loaded. ${data.mismatches.length} setting(s) differ between the two radios.`
            : (body.remote && !data.remote_reachable
              ? "This radio's settings loaded; the remote radio did not answer."
              : "Settings loaded."),
          data.mismatches && data.mismatches.length ? "warn" : "ok"
        );
      } catch (err) {
        if (destroyed) return;
        statusMsg.show(err.message || "Could not read the radio.", "err");
      } finally {
        if (!destroyed) setBusy(false);
      }
    }

    async function doSave() {
      if (busy || !loaded) return;
      const body = target();
      if (Object.keys(edits.remote).length) body.remote = { ...edits.remote };
      if (Object.keys(edits.local).length) body.local = { ...edits.local };
      if (!body.local && !body.remote) {
        statusMsg.show("Nothing has been changed.", "");
        return;
      }
      setBusy(true, "Writing settings and rebooting the radios…");
      try {
        const data = await post("/api/sik/save", body);
        if (destroyed) return;
        const warned = (data.warnings || []).join(" ");
        statusMsg.show(warned || "Settings written. Re-reading the radios…", warned ? "warn" : "ok");
        edits.local = {};
        edits.remote = {};
        // Re-read rather than trusting the write: the radio rounds air rates
        // and transmit powers up to the nearest value it supports, so what it
        // now holds is not necessarily what was sent.
        setBusy(false);
        await doLoad();
      } catch (err) {
        if (destroyed) return;
        statusMsg.show(err.message || "Could not write the radio.", "err");
        setBusy(false);
      }
    }

    async function doReset(which) {
      if (busy || !loaded) return;
      const side = which === "remote" ? "remote" : "local";
      const label = side === "remote" ? "the remote radio" : "this radio";
      const ok = await confirmReset(side, label);
      if (!ok || destroyed) return;
      setBusy(true, `Resetting ${label} to factory defaults…`);
      try {
        const body = target();
        body.target = side;
        await post("/api/sik/reset", body);
        if (destroyed) return;
        statusMsg.show(`${label} reset to factory defaults. Re-reading…`, "ok");
        setBusy(false);
        await doLoad();
      } catch (err) {
        if (destroyed) return;
        statusMsg.show(err.message || "Could not reset the radio.", "err");
        setBusy(false);
      }
    }

    /**
     * Confirm a factory reset before it runs.
     *
     * A reset is the one action here that cannot be undone by reading the old
     * values back, because it discards them — and resetting the *remote* radio
     * is worse than that: it lands on the radio bolted to the aircraft, and if
     * its defaults differ from this end's settings the link goes down with the
     * only way back being a cable and the airframe on a bench.
     */
    function confirmReset(side, label) {
      const detail = side === "remote"
        ? "The remote radio will return to its firmware defaults. If those differ " +
          "from this radio's settings the link will drop, and the only way to " +
          "reach it again is a cable to the radio itself."
        : "This radio will return to its firmware defaults, including its baud " +
          "rate. If the remote radio keeps different settings the link will not " +
          "come back until they match again.";
      if (!Corvus.ui || typeof Corvus.ui.modal !== "function") return Promise.resolve(true);
      return new Promise((resolve) => {
        let settled = false;
        const finish = (value) => {
          if (settled) return;
          settled = true;
          resolve(value);
        };
        const box = S.el("div", "motor-calib-warning");
        box.appendChild(S.el("div", "motor-calib-warning-line", detail));
        box.appendChild(S.el("div", "motor-calib-warning-line",
          "Both radios are read again afterwards, so you will see the result " +
          "rather than have to trust it."));
        const cancel = Corvus.ui.button({
          variant: "secondary", label: "Leave it alone",
          onClick: () => { dialog.close(); },
        });
        const confirm = Corvus.ui.button({
          variant: "danger", label: `Reset ${label}`,
          onClick: () => { finish(true); dialog.close(); },
        });
        const dialog = Corvus.ui.modal({
          title: `Reset ${label} to factory defaults`,
          size: "sm", body: box, actions: [cancel, confirm],
          mount: page,
          // Resolves false on every exit that is not the confirm button —
          // Escape, the close cross and the backdrop all land here — so a
          // dismissed dialog can never leave the caller awaiting forever.
          onClose: () => finish(false),
        });
        dialog.open();
      });
    }

    // ------------------------------------------------------------------
    // Rendering the loaded radios
    // ------------------------------------------------------------------

    function renderResults() {
      resultHost.innerHTML = "";
      if (!loaded) return;

      resultHost.appendChild(buildLinkCard());
      const mismatchCard = buildMismatchCard();
      if (mismatchCard) resultHost.appendChild(mismatchCard);

      const intro = S.el("div", "params-desc sik-columns-intro");
      intro.textContent = COLUMNS_INTRO;
      resultHost.appendChild(intro);

      const columns = S.el("div", "sik-columns");
      for (const side of SIDES) {
        const radio = loaded[side.id];
        if (!radio) {
          if (side.id === "remote") columns.appendChild(buildUnreachableCard(side));
          continue;
        }
        columns.appendChild(buildRadioCard(side, radio));
      }
      resultHost.appendChild(columns);
      resultHost.appendChild(buildActionsCard());
      S.refreshIcons();
      recomputeGate();
    }

    /** Firmware, board and — when the radio reported one — the link report. */
    function buildLinkCard() {
      const card = S.el("div", "page-card sik-link-card");
      card.appendChild(S.sectionTitle("Link"));

      const local = loaded.local || {};
      const remote = loaded.remote || null;
      card.appendChild(S.infoRow("This radio", local.version || "—", "local_version"));
      card.appendChild(S.infoRow("Remote radio",
        remote ? (remote.version || "—") : "not answering", "remote_version"));

      // Firmware version is the one thing that must match and is not a
      // register, so a difference is reported here rather than in the
      // mismatch list, which is about settings the operator can change.
      if (remote && local.version && remote.version && local.version !== remote.version) {
        const warn = S.el("div", "params-banner");
        warn.textContent =
          "The two radios are running different firmware. A pair has to match on " +
          "firmware version as well as on settings; update whichever is older.";
        card.appendChild(warn);
      }

      const link = loaded.link;
      if (link) {
        card.appendChild(S.infoRow("Signal (this / remote)",
          `${link.local_rssi} / ${link.remote_rssi}  ` +
          `(${link.local_dbm} / ${link.remote_dbm} dBm)`, "rssi"));
        card.appendChild(S.infoRow("Noise (this / remote)",
          `${link.local_noise} / ${link.remote_noise}`, "noise"));
        // Fade margin is the number that answers "will this reach": the SiK
        // rule of thumb is that range doubles for every 6 dB of it.
        card.appendChild(S.infoRow("Fade margin (this / remote)",
          `${link.local_margin_db} / ${link.remote_margin_db} dB`, "margin"));
      }
      return card;
    }

    /** The disagreement banner, and the one-click way out of it. */
    function buildMismatchCard() {
      const mismatches = (loaded && loaded.mismatches) || [];
      if (!mismatches.length) return null;
      const card = S.el("div", "page-card sik-mismatch-card");
      card.appendChild(S.sectionTitle("The two radios disagree"));

      const desc = S.el("div", "params-desc");
      desc.textContent =
        "These settings have to be identical at both ends or the radios cannot " +
        "hear each other. Copying stages this radio's values into the remote " +
        "column; nothing is written until you press Save.";
      card.appendChild(desc);

      const list = S.el("ul", "sik-mismatch-list");
      for (const item of mismatches) {
        const li = S.el("li");
        li.dataset.name = item.name;
        li.textContent = `${item.label}: ${item.local} here, ${item.remote} on the remote radio`;
        list.appendChild(li);
      }
      card.appendChild(list);

      const copyBtn = Corvus.ui.button({
        variant: "secondary", icon: "copy",
        label: "Copy these to the remote radio",
        onClick: () => copyToRemote(),
      });
      copyBtn.dataset.action = "copy";
      copyBtn.dataset.needsGate = "1";
      card.appendChild(copyBtn);
      return card;
    }

    function buildUnreachableCard(side) {
      const card = S.el("div", "page-card sik-radio-card sik-radio-unreachable");
      card.appendChild(S.sectionTitle(side.title));
      const desc = S.el("div", "params-desc");
      desc.textContent =
        "The remote radio did not answer. It is reached through this one over the " +
        "air, so it needs power at the far end and a pair that still agrees on " +
        "the air settings. A solid green LED on both radios means the link is up.";
      card.appendChild(desc);
      return card;
    }

    /**
     * One column: every field the radio reported, in the order it reported them.
     *
     * The field list comes from the backend rather than from a form written
     * here, which is what lets a radio with registers Corvus has never heard of
     * still be read and written — a newer SiK build, or RFD900-class firmware
     * with a longer table.
     */
    function buildRadioCard(side, radio) {
      const card = S.el("div", "page-card sik-radio-card");
      card.dataset.side = side.id;
      card.appendChild(S.sectionTitle(side.title));

      if (radio.board) card.appendChild(S.infoRow("Board", radio.board, "board"));

      const grid = S.el("div", "sik-field-grid");
      for (const field of radio.fields || []) {
        grid.appendChild(buildField(side.id, field));
      }
      card.appendChild(grid);
      return card;
    }

    function buildField(sideId, spec) {
      const wrap = S.el("div", "sik-field");
      wrap.dataset.name = spec.name;
      wrap.dataset.side = sideId;
      if (spec.advanced) wrap.classList.add("sik-advanced");
      wrap.hidden = !!spec.advanced && !showAdvanced;
      if (spec.must_match) wrap.classList.add("sik-must-match");

      const staged = edits[sideId][spec.name];
      const value = staged === undefined ? spec.value : staged;

      let control;
      if (spec.read_only) {
        control = Corvus.ui.input({
          value: String(value), disabled: true,
          ariaLabel: spec.label,
        });
      } else if (spec.kind === "enum") {
        control = Corvus.ui.select({
          ariaLabel: spec.label,
          options: (spec.options || []).map((o) => ({
            value: String(o.value), label: o.label,
          })),
          value: String(value),
          onChange: (next) => stage(sideId, spec, parseInt(next, 10), wrap),
        });
      } else {
        control = Corvus.ui.input({
          type: "number", value: String(value),
          min: spec.min, max: spec.max, step: spec.step || 1,
          ariaLabel: spec.label,
          onChange: (next) => stage(sideId, spec, parseInt(next, 10), wrap),
        });
      }
      control.dataset.field = spec.name;
      if (!spec.read_only) control.dataset.needsGate = "1";

      const label = spec.unit ? `${spec.label} (${spec.unit})` : spec.label;
      wrap.appendChild(Corvus.ui.field({ label, control, hint: spec.hint }));
      if (spec.must_match) {
        const mark = S.el("span", "sik-match-mark");
        mark.textContent = "must match at both ends";
        wrap.appendChild(mark);
      }
      return wrap;
    }

    /**
     * Record one edit, or drop it when the operator typed the original back.
     *
     * Dropping matters: an unchanged register still in the batch would be
     * written, and every write costs the radio a reboot. It is also what makes
     * "nothing has been changed" an honest message rather than one that depends
     * on whether a field was focused.
     */
    function stage(sideId, spec, value, wrap) {
      if (!Number.isFinite(value)) return;
      if (value === spec.value) delete edits[sideId][spec.name];
      else edits[sideId][spec.name] = value;
      const dirty = Object.prototype.hasOwnProperty.call(edits[sideId], spec.name);
      if (wrap) wrap.classList.toggle("sik-dirty", dirty);
      updateSaveState();
    }

    /** Stage the local values for every must-match register that differs. */
    function copyToRemote() {
      const mismatches = (loaded && loaded.mismatches) || [];
      for (const item of mismatches) {
        edits.remote[item.name] = item.local;
        const field = resultHost.querySelector(
          `.sik-field[data-side="remote"][data-name="${item.name}"]`);
        if (!field) continue;
        const control = field.querySelector("[data-field]");
        if (control) control.value = String(item.local);
        field.classList.add("sik-dirty");
      }
      updateSaveState();
      statusMsg.show(
        "Staged in the remote column. Press Save to write them — the remote radio " +
        "is written first, before this one.", "");
    }

    let saveBtn = null;
    function updateSaveState() {
      if (!saveBtn) return;
      const count = Object.keys(edits.local).length + Object.keys(edits.remote).length;
      saveBtn.dataset.pending = String(count);
      const span = saveBtn.querySelector("span");
      if (span) span.textContent = count ? `Save ${count} change(s)` : "Save";
    }

    function buildActionsCard() {
      const card = S.el("div", "page-card sik-actions-card");

      const advanced = Corvus.ui.button({
        variant: "ghost", size: "sm",
        icon: showAdvanced ? "chevron-up" : "chevron-down",
        label: showAdvanced ? "Hide advanced settings" : "Show advanced settings",
        onClick: () => {
          showAdvanced = !showAdvanced;
          resultHost.querySelectorAll(".sik-advanced").forEach((el) => {
            el.hidden = !showAdvanced;
          });
          renderResults();
        },
      });
      advanced.dataset.action = "advanced";
      card.appendChild(advanced);

      saveBtn = Corvus.ui.button({
        variant: "primary", icon: "save", label: "Save",
        onClick: () => doSave(),
      });
      saveBtn.dataset.action = "save";
      saveBtn.dataset.needsGate = "1";
      card.appendChild(saveBtn);
      updateSaveState();

      for (const side of SIDES) {
        if (!loaded[side.id]) continue;
        const btn = Corvus.ui.button({
          variant: "danger", size: "sm", icon: "rotate-ccw",
          label: side.id === "remote" ? "Reset remote radio" : "Reset this radio",
          onClick: () => doReset(side.id),
        });
        btn.dataset.action = `reset-${side.id}`;
        btn.dataset.needsGate = "1";
        card.appendChild(btn);
      }
      return card;
    }

    // ------------------------------------------------------------------
    // Lifecycle
    // ------------------------------------------------------------------

    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      // Only the armed flag matters here, and only because arming must close
      // the gate immediately rather than at the next status poll.
      let lastArmed = null;
      telemetryUnsub = Corvus.telemetry.subscribe((t) => {
        const armed = !!(t && t.armed);
        if (armed === lastArmed) return;
        lastArmed = armed;
        recomputeGate();
      });
    }

    refreshStatus();

    function destroy() {
      destroyed = true;
      if (typeof telemetryUnsub === "function") {
        try { telemetryUnsub(); } catch (err) { console.error("sik teardown failed:", err); }
      }
      telemetryUnsub = null;
    }

    return destroy;
  }

  return { render };
})();
