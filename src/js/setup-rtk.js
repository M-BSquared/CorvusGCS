"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupRtk — the RTK GPS sub-page of the Setup page.
 *
 * RTK turns a GPS fix that is accurate to metres into one accurate to
 * centimetres, and it does it by a route that is worth being explicit about on
 * screen, because every failure mode is somewhere along it: a second receiver
 * standing still works out how wrong it is, the ground station carries those
 * corrections down the MAVLink link, and the aircraft's receiver subtracts
 * them. Three parties, two of which the operator can see and one of which they
 * cannot — so the page shows all three, and never reports "RTK" as a single
 * light that is on or off.
 *
 * Why the page is mostly a status display
 * ---------------------------------------
 * There is normally nothing to configure. A base plugged into this computer is
 * found, surveyed and streaming without the page being opened at all, which is
 * the behaviour QGroundControl established and the reason RTK gets used in the
 * field rather than admired in a settings dialog. So the settings are below the
 * fold, in the two shapes that actually differ between operators: how careful
 * the survey has to be, and whether the corrections come off a cable at all or
 * out of an NTRIP caster.
 *
 * The survey is the part that needs explaining, and it is why the progress bar
 * carries a sentence rather than only a percentage. A survey is finished when
 * *both* its minimum duration has passed and its accuracy limit has been met,
 * and an operator watching a bar that has been at 30% for four minutes needs to
 * know it is the sky holding them up and not the clock.
 *
 * Backend contract:
 *   GET  /api/rtk/status    the whole state, polled while the page is open
 *   POST /api/rtk/settings  {enabled,source,mode,survey_*,fixed,ntrip} -> {status}
 *   POST /api/rtk/restart   survey again from zero
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the lifecycle: destroy() stops the poll and marks the page gone, so a
 * request already in flight never writes to a DOM this page no longer owns.
 */
Corvus.setupRtk = (function () {
  const S = Corvus.setupShared;

  // How often the status is re-read while the page is open. A survey moves
  // once a second and nothing else here moves faster, so this is the rate the
  // page has something new to say — not a rate chosen to look live.
  const POLL_MS = 1000;

  // What each state means in one line. Kept here rather than built from the
  // backend's message so the page always has something to say, including
  // before the first poll has come back.
  const STATE_LABELS = {
    off: "Off",
    searching: "Looking for a base",
    connecting: "Connecting",
    surveying: "Surveying",
    active: "Correcting",
    error: "Problem",
  };

  const STATE_TONE = {
    off: "",
    searching: "pending",
    connecting: "pending",
    surveying: "pending",
    active: "ok",
    error: "err",
  };

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page rtk-page");
    page.appendChild(S.backButton(navigateBack));
    page.appendChild(S.pageHeader(
      "RTK GPS",
      "Centimetre positioning from a base station on this computer, or from an NTRIP caster",
    ));

    let status = null;
    let destroyed = false;
    let pollTimer = null;
    let saving = false;
    // Set while the operator is editing, so a poll landing mid-edit cannot
    // overwrite a half-typed coordinate with the stored one.
    let editing = false;

    // ------------------------------------------------------------------
    // Status card
    // ------------------------------------------------------------------

    const statusCard = S.el("div", "page-card rtk-status-card");
    statusCard.appendChild(S.sectionTitle("Base station"));

    const stateRow = S.el("div", "rtk-state-row");
    const stateBadge = S.el("span", "rtk-state-badge", "…");
    stateBadge.dataset.role = "state";
    const stateText = S.el("span", "rtk-state-text", "Reading the base station…");
    stateText.dataset.role = "message";
    stateRow.appendChild(stateBadge);
    stateRow.appendChild(stateText);
    statusCard.appendChild(stateRow);

    const warningLine = S.el("div", "rtk-warning");
    warningLine.dataset.role = "warning";
    warningLine.hidden = true;
    statusCard.appendChild(warningLine);

    // Survey progress. Hidden unless a survey is actually running or has just
    // finished — a bar sitting at 0% for an NTRIP stream is noise.
    const surveyBlock = S.el("div", "rtk-survey");
    surveyBlock.hidden = true;
    const surveyBar = S.el("div", "rtk-progress");
    const surveyFill = S.el("div", "rtk-progress-fill");
    surveyFill.dataset.role = "progress-fill";
    surveyBar.appendChild(surveyFill);
    surveyBlock.appendChild(surveyBar);
    const surveyText = S.el("div", "rtk-survey-text", "");
    surveyText.dataset.role = "survey-text";
    surveyBlock.appendChild(surveyText);
    statusCard.appendChild(surveyBlock);

    // The three parties, as rows. `dataset.infoKey` on each value so the tests
    // (and any later caller) can find a row without walking the DOM by index.
    const rows = {};
    const rowDefs = [
      ["receiver", "Receiver"],
      ["port", "Connection"],
      ["corrections", "Corrections from the base"],
      ["injected", "Sent to the aircraft"],
      ["fix", "Aircraft GPS"],
    ];
    for (const [key, label] of rowDefs) {
      const row = S.infoRow(label, "—", key);
      rows[key] = row.querySelector(".page-row-value");
      statusCard.appendChild(row);
    }

    const restartBtn = Corvus.ui.button({
      variant: "secondary", size: "sm", icon: "rotate-ccw",
      label: "Survey again",
      onClick: () => doRestart(),
    });
    restartBtn.dataset.action = "restart";
    restartBtn.hidden = true;
    statusCard.appendChild(Corvus.ui.actions(restartBtn));

    page.appendChild(statusCard);

    // ------------------------------------------------------------------
    // Settings card
    // ------------------------------------------------------------------

    const settingsCard = S.el("div", "page-card rtk-settings-card");
    settingsCard.appendChild(S.sectionTitle("Where the corrections come from"));

    const intro = S.el("div", "params-desc");
    intro.textContent =
      "Corvus looks for an RTK receiver on this computer by itself and starts " +
      "correcting as soon as one is plugged in — nothing here needs changing " +
      "for that. The flight controller's own port is never used, so the " +
      "aircraft's telemetry link is never at risk.";
    settingsCard.appendChild(intro);

    const enabledToggle = Corvus.ui.toggle({
      value: true,
      ariaLabel: "RTK corrections",
      onChange: () => { markEdited(); return save(); },
    });
    enabledToggle.el.dataset.role = "enabled";
    settingsCard.appendChild(Corvus.ui.field({
      label: "RTK corrections", control: enabledToggle.el, inline: true,
      hint: "On by default. Turned off, Corvus opens no serial port looking for a base.",
    }));

    const sourceSelect = Corvus.ui.select({
      ariaLabel: "Correction source",
      options: [
        { value: "usb", label: "Base station on this computer" },
        { value: "ntrip", label: "NTRIP caster (needs a network)" },
      ],
      value: "usb",
      onChange: () => { markEdited(); repaintForm(); },
    });
    sourceSelect.dataset.role = "source";
    settingsCard.appendChild(Corvus.ui.field({
      label: "Source", control: sourceSelect,
      hint: "A receiver on a cable needs no network at all. A caster needs one, " +
            "and gives corrections without a second receiver.",
    }));

    // -- the USB base's own settings ------------------------------------

    const usbBlock = S.el("div", "rtk-source-block");
    usbBlock.dataset.role = "usb-block";

    const modeSelect = Corvus.ui.select({
      ariaLabel: "Base position",
      options: [
        { value: "survey", label: "Work it out (survey-in)" },
        { value: "fixed", label: "I know where the base is" },
      ],
      value: "survey",
      onChange: () => { markEdited(); repaintForm(); },
    });
    modeSelect.dataset.role = "mode";
    usbBlock.appendChild(Corvus.ui.field({
      label: "Base position", control: modeSelect,
      hint: "A survey costs a few minutes and is no more accurate than what it " +
            "converges on. A known mark skips both.",
    }));

    const surveyBlockForm = S.el("div", "rtk-sub-block");
    surveyBlockForm.dataset.role = "survey-block";

    const accuracyInput = Corvus.ui.input({
      className: "rtk-input", mono: true, value: "2", autocomplete: false,
      ariaLabel: "Survey accuracy in metres",
    });
    accuracyInput.dataset.role = "survey-accuracy";
    accuracyInput.addEventListener("change", () => { markEdited(); save(); });
    surveyBlockForm.appendChild(Corvus.ui.field({
      label: "Stop at this accuracy (m)", control: accuracyInput,
      hint: "How well the base has to know its own position before it starts " +
            "correcting. Two metres is the usual answer: the aircraft's fix is " +
            "relative to the base, so this is an offset of the whole flight, " +
            "not an error in it.",
    }));

    const durationInput = Corvus.ui.input({
      className: "rtk-input", mono: true, value: "180", autocomplete: false,
      ariaLabel: "Minimum survey duration in seconds",
    });
    durationInput.dataset.role = "survey-duration";
    durationInput.addEventListener("change", () => { markEdited(); save(); });
    surveyBlockForm.appendChild(Corvus.ui.field({
      label: "Survey for at least (s)", control: durationInput,
      hint: "Both this and the accuracy above have to be met before the survey ends.",
    }));
    usbBlock.appendChild(surveyBlockForm);

    const fixedBlock = S.el("div", "rtk-sub-block");
    fixedBlock.dataset.role = "fixed-block";
    fixedBlock.hidden = true;
    const fixedInputs = {};
    const fixedDefs = [
      ["latitude", "Latitude", "°", "Decimal degrees."],
      ["longitude", "Longitude", "°", "Decimal degrees."],
      ["altitude", "Height", "m",
       "Height above the ellipsoid, not above sea level — a survey record " +
       "usually gives one or the other, and in most of Europe they differ by " +
       "tens of metres."],
      ["accuracy", "How well it is known", "m", "The accuracy of the mark itself."],
    ];
    for (const [key, label, unit, hint] of fixedDefs) {
      const input = Corvus.ui.input({
        className: "rtk-input", mono: true, value: "0", autocomplete: false,
        ariaLabel: label,
      });
      input.dataset.role = "fixed-" + key;
      input.addEventListener("change", () => { markEdited(); save(); });
      fixedInputs[key] = input;
      fixedBlock.appendChild(Corvus.ui.field({
        label: unit ? `${label} (${unit})` : label, control: input, hint,
      }));
    }
    usbBlock.appendChild(fixedBlock);
    settingsCard.appendChild(usbBlock);

    // -- NTRIP ----------------------------------------------------------

    const ntripBlock = S.el("div", "rtk-source-block");
    ntripBlock.dataset.role = "ntrip-block";
    ntripBlock.hidden = true;
    const ntripInputs = {};
    const ntripDefs = [
      ["host", "Caster address", "", "The hostname, without http:// and without the mountpoint."],
      ["port", "Port", "", "2101 unless the provider says otherwise."],
      ["mountpoint", "Mountpoint", "", "The stream's name on the caster."],
      ["username", "Username", "", ""],
      ["password", "Password", "password",
       "Stored in the config file on this computer and never sent back to this page."],
    ];
    for (const [key, label, kind, hint] of ntripDefs) {
      const input = Corvus.ui.input({
        className: "rtk-input", value: "", autocomplete: false, ariaLabel: label,
        type: kind === "password" ? "password" : "text",
      });
      input.dataset.role = "ntrip-" + key;
      input.addEventListener("change", () => { markEdited(); save(); });
      ntripInputs[key] = input;
      ntripBlock.appendChild(Corvus.ui.field({ label, control: input, hint }));
    }
    settingsCard.appendChild(ntripBlock);

    const saveStatus = S.el("div", "params-actions-status rtk-save-status", "");
    saveStatus.dataset.role = "save-status";
    settingsCard.appendChild(saveStatus);

    page.appendChild(settingsCard);
    container.appendChild(page);

    // ------------------------------------------------------------------
    // Painting
    // ------------------------------------------------------------------

    /** Show or hide the blocks that belong to the selected source and mode. */
    function repaintForm() {
      const source = sourceSelect.value;
      usbBlock.hidden = source !== "usb";
      ntripBlock.hidden = source !== "ntrip";
      const fixed = modeSelect.value === "fixed";
      surveyBlockForm.hidden = fixed;
      fixedBlock.hidden = !fixed;
    }

    /** Fill the form from the stored settings. Never runs mid-edit. */
    function fillForm(settings) {
      if (!settings || editing) return;
      enabledToggle.setValue(settings.enabled !== false);
      sourceSelect.value = settings.source || "usb";
      modeSelect.value = settings.mode || "survey";
      accuracyInput.value = String(settings.survey_accuracy);
      durationInput.value = String(settings.survey_duration);
      const fixed = settings.fixed || {};
      for (const key of Object.keys(fixedInputs)) {
        fixedInputs[key].value = String(fixed[key] != null ? fixed[key] : 0);
      }
      const ntrip = settings.ntrip || {};
      for (const key of Object.keys(ntripInputs)) {
        if (key === "password") {
          // The real one never reaches the browser. An empty box with a
          // placeholder says "one is stored" without pretending to show it,
          // and an empty box on save means "keep it".
          ntripInputs[key].value = "";
          ntripInputs[key].placeholder = ntrip.has_password ? "stored" : "";
          continue;
        }
        ntripInputs[key].value = String(ntrip[key] != null ? ntrip[key] : "");
      }
      repaintForm();
    }

    function paint(data) {
      status = data;
      const state = data.state || "off";
      stateBadge.textContent = STATE_LABELS[state] || state;
      stateBadge.className = "rtk-state-badge " + (STATE_TONE[state] || "");
      stateText.textContent = data.message || "";

      const warning = data.error || data.warning || "";
      warningLine.textContent = warning;
      warningLine.hidden = !warning;
      warningLine.className = "rtk-warning" + (data.error ? " err" : "");

      // Survey
      const survey = data.survey;
      const surveying = state === "surveying" || (survey && survey.active);
      surveyBlock.hidden = !(surveying || (survey && survey.valid));
      if (!surveyBlock.hidden) {
        const pct = Math.max(0, Math.min(100, Number(data.survey_progress) || 0));
        surveyFill.style.width = pct + "%";
        surveyText.textContent = surveySentence(data, pct);
      }

      rows.receiver.textContent = data.receiver_label
        || (data.state === "active" && !data.receiver
          ? "a base that configured itself" : "—");
      rows.port.textContent = data.device
        ? (data.baud ? `${data.device} at ${data.baud} baud` : data.device)
        : "—";
      rows.corrections.textContent = describeCorrections(data);
      rows.injected.textContent = describeInjected(data);
      const vehicle = data.vehicle || {};
      rows.fix.textContent = vehicle.fix
        ? `${friendlyFix(vehicle.fix)} · ${vehicle.satellites || 0} satellites`
        : "—";

      restartBtn.hidden = !(data.source === "usb" && data.receiver
        && (state === "active" || state === "surveying"));
      fillForm(data.settings);
    }

    /** The sentence under the progress bar: what is still holding it up. */
    function surveySentence(data, pct) {
      const survey = data.survey || {};
      const target = data.survey_target || {};
      if (survey.valid && !survey.active) {
        return `Survey complete — the base knows its position to ${fmt(survey.accuracy)} m.`;
      }
      const parts = [`${pct}%`];
      parts.push(`${Math.round(survey.duration || 0)} s of ${target.duration || 0} s`);
      if (survey.accuracy) {
        parts.push(`accurate to ${fmt(survey.accuracy)} m, needs ${fmt(target.accuracy)} m`);
      }
      return parts.join(" · ");
    }

    function describeCorrections(data) {
      if (!data.frames) return "none yet";
      const types = Object.keys(data.messages || {}).sort().join(", ");
      const age = data.source_age;
      const stale = age != null && age > 5 ? ` · last ${Math.round(age)} s ago` : "";
      return `${data.frames} messages${types ? " (" + types + ")" : ""}${stale}`;
    }

    function describeInjected(data) {
      const injected = data.injected || {};
      if (!data.link_ready) return "waiting for the aircraft link";
      if (!injected.messages) return "none yet";
      const dropped = injected.dropped
        ? ` · ${injected.dropped} could not be sent` : "";
      return `${injected.messages} messages, ${formatBytes(injected.bytes)}${dropped}`;
    }

    // ------------------------------------------------------------------
    // Actions
    // ------------------------------------------------------------------

    function markEdited() {
      editing = true;
    }

    function setSaveStatus(cls, text) {
      saveStatus.className = "params-actions-status rtk-save-status" + (cls ? " " + cls : "");
      saveStatus.textContent = text || "";
    }

    /** Send the whole form. The backend replaces the block wholesale. */
    async function save() {
      if (saving || destroyed) return;
      saving = true;
      setSaveStatus("pending", "saving");
      const body = {
        enabled: enabledToggle.getValue(),
        source: sourceSelect.value,
        mode: modeSelect.value,
        survey_accuracy: Number(accuracyInput.value),
        survey_duration: Number(durationInput.value),
        fixed: {
          latitude: Number(fixedInputs.latitude.value),
          longitude: Number(fixedInputs.longitude.value),
          altitude: Number(fixedInputs.altitude.value),
          accuracy: Number(fixedInputs.accuracy.value),
        },
        ntrip: {
          host: ntripInputs.host.value.trim(),
          port: Number(ntripInputs.port.value) || 2101,
          mountpoint: ntripInputs.mountpoint.value.trim(),
          username: ntripInputs.username.value.trim(),
          // Empty means "keep the stored one" — see fillForm.
          password: ntripInputs.password.value,
        },
      };
      try {
        const result = await Corvus.telemetry.postAction("/api/rtk/settings", body);
        if (destroyed) return;
        setSaveStatus("ok", result && result.warning ? result.warning : "saved");
        editing = false;
        if (result && result.status) paint(result.status);
      } catch (err) {
        if (destroyed) return;
        const message = (err && err.message) || "could not save";
        setSaveStatus("err", message);
        S.notify("critical", "Could not save the RTK settings: " + message);
      } finally {
        saving = false;
      }
    }

    async function doRestart() {
      if (destroyed) return;
      restartBtn.disabled = true;
      setSaveStatus("pending", "restarting the survey");
      try {
        const result = await Corvus.telemetry.postAction("/api/rtk/restart", {});
        if (destroyed) return;
        setSaveStatus("", "");
        if (result && result.status) paint(result.status);
      } catch (err) {
        if (destroyed) return;
        setSaveStatus("err", (err && err.message) || "could not restart");
      } finally {
        if (!destroyed) restartBtn.disabled = false;
      }
    }

    async function refresh() {
      try {
        const data = await Corvus.telemetry.requestJson("/api/rtk/status");
        if (destroyed) return;
        paint(data);
      } catch (err) {
        if (destroyed) return;
        stateBadge.textContent = STATE_LABELS.error;
        stateBadge.className = "rtk-state-badge err";
        stateText.textContent = "Could not read the RTK status.";
      }
    }

    // ------------------------------------------------------------------
    // Lifecycle
    // ------------------------------------------------------------------

    refresh();
    pollTimer = setInterval(refresh, POLL_MS);

    function destroy() {
      destroyed = true;
      if (pollTimer !== null) {
        clearInterval(pollTimer);
        pollTimer = null;
      }
    }

    S.refreshIcons();
    return destroy;
  }

  /** Trim float noise without losing a centimetre. */
  function fmt(value) {
    const n = Number(value);
    if (!isFinite(n)) return "—";
    return String(Math.round(n * 100) / 100);
  }

  function formatBytes(bytes) {
    const n = Number(bytes) || 0;
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} kB`;
    return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  }

  /**
   * The autopilot's fix name, in words.
   *
   * RTK_FIXED and RTK_FLOAT are the two that matter and the difference between
   * them is the whole point of the page: "float" is a receiver that has the
   * corrections and has not yet resolved the carrier ambiguity, which is
   * decimetres; "fixed" is the centimetre answer.
   */
  function friendlyFix(fix) {
    const names = {
      RTK_FIXED: "RTK fixed (centimetres)",
      RTK_FLOAT: "RTK float (decimetres)",
      "3D_FIX": "3D fix",
      DGPS: "DGPS",
      NO_FIX: "no fix",
      NO_GPS: "no GPS",
    };
    return names[fix] || fix;
  }

  return { render };
})();
