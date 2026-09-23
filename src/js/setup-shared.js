"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupShared — shared helpers and constants for the Setup page modules.
 *
 * Kept stateless (no DOM ownership, no subscriptions) so the sub-pages can
 * import these without coupling to each other. This is the common layer
 * extracted out of the former setup.js monolith so each page lives in its own
 * file.
 *
 * The second half of the file is the *schema-driven parameter form*: the
 * machinery behind every page that renders a description the backend sent —
 * Motors, Safety & Sensors and PID Tuning — rather than a form somebody typed
 * out. Those helpers take the calling page's `state` object and write to it;
 * the module itself still holds nothing, so "stateless" above is unchanged.
 * The contract is small and every one of those pages already satisfies it:
 *
 *   state.armed         boolean, the live armed flag
 *   state.controls      array, the registry these helpers push to
 *   state.banner        optional element, shown while armed
 *   state.actionsStatus optional element, the page-level status line
 *
 * They live here because the alternative is what this codebase had: two copies
 * of the write path, drifting. A write that silently stops restoring a refused
 * control on one page and not the other is exactly the bug that costs an
 * airframe, so there is one copy.
 */
Corvus.setupShared = (function () {
  // Rolling-window cap and ~10 Hz redraw throttle for the live Plotly graphs,
  // matching the vibration plugin's proven pattern (capped memory, rAF-friendly).
  const MAX_POINTS = 600;
  const REDRAW_MIN_MS = 100;

  // Semantic palette reused from the app CSS variables (kept in sync here so
  // the Plotly dark theme matches the HUD). A measured response is nav blue
  // (#4CC9FF); the setpoint it is read against is healthy green (#45D483).
  // Trace colors are read from the theme at draw time rather than frozen as
  // constants, so the light theme gets its darker, saturated variants instead
  // of the dark theme's glowing ones. Kept as getters because the old constant
  // names are part of this module's surface.
  const chartColor = (k, fallback) => {
    try { return Corvus.ui.chartColors()[k] || fallback; } catch (_e) { return fallback; }
  };

  /** True when the OS asks for less motion (read per sub-page build). */
  function reducedMotion() {
    return !!(typeof window !== "undefined" && window.matchMedia
      && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
  }

  /** Tiny helper: build an element with a class and optional text. */
  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  // Icons, the lucide refresh, and the page header/section title come from the
  // shared component layer. Setup used to carry its own copies of all four;
  // these are re-exports so the Setup sub-modules keep their short S.* names.
  // Icons are built with "auto" sizing because Setup's stylesheet already owns
  // their dimensions (.setup-tile .tile-icon svg, .calib-card svg, ...).
  function icon(name) { return Corvus.ui.icon(name, "auto"); }
  const refreshIcons = Corvus.ui.refreshIcons;
  const pageHeader = Corvus.ui.pageHeader;
  const sectionTitle = Corvus.ui.sectionTitle;

  /**
   * A labelled value row used in the Vehicle Info card.
   * @param {string} label
   * @param {string|number} value
   * @param {string} [dataKey] Optional key written to the value span's
   *   `dataset.infoKey` so callers can look the row up later for live updates
   *   (e.g. the Setup tile grid's telemetry subscription). Omit to leave the
   *   span unmarked — backwards-compatible with the original 2-arg call.
   */
  function infoRow(label, value, dataKey) {
    const r = el("div", "page-row");
    r.appendChild(el("span", "page-row-label", label));
    const v = el("span", "page-row-value", String(value));
    if (dataKey) v.dataset.infoKey = dataKey;
    r.appendChild(v);
    return r;
  }

  /** Back button used at the top of each sub-page (apple-design: same path in
   *  and out — back returns along the entry path).
   *
   *  `label` names where back goes; it defaults to "Setup" because that is
   *  where this helper started, but a sub-page on another screen has to say
   *  its own parent or the button lies about the destination. */
  function backButton(onBack, label) {
    const target = label || "Setup";
    return Corvus.ui.button({
      variant: "ghost",
      size: "sm",
      className: "setup-back",
      icon: "chevron-left",
      label: target,
      ariaLabel: "Back to " + target,
      onClick: onBack,
    });
  }

  /** Graph layout, themed. The surfaces, type and grid come from the active
   *  theme via Corvus.ui.plotlyTheme(); only what is specific to these graphs
   *  — margins, axis titles — is set here. Called on every redraw, so a theme
   *  switch takes effect on the next frame. */
  function plotlyLayout(unit) {
    const theme = Corvus.ui.plotlyTheme();
    return Object.assign({}, theme, {
      margin: { l: 44, r: 8, t: 6, b: 26 },
      showlegend: false,
      xaxis: Object.assign({}, theme.xaxis, { title: "Time (s)" }),
      yaxis: Object.assign({}, theme.yaxis, { title: unit }),
    });
  }

  /** Plotly config with reduced-motion zero-duration transitions when requested.
   *  showTips is off: Plotly's own "double-click to zoom back out" hint is an
   *  unstyled toast it draws itself, positioned over whatever else is on the
   *  page rather than the chart — Corvus.ui.attachZoomHint() replaces it with
   *  the app's own toast on the charts that call it. */
  function plotlyConfig(reduced) {
    const config = { displayModeBar: false, responsive: true, showTips: false };
    if (reduced) { config.transition = { duration: 0 }; config.frame = { duration: 0 }; }
    return config;
  }

  // -------------------------------------------------------------------------
  // Schema-driven parameter forms
  // -------------------------------------------------------------------------

  /**
   * Class list for one part of the form: the shared base plus the page's own
   * modifier, e.g. ("motors", "field") -> "pform-field motors-field".
   *
   * Both survive on the element on purpose. `.pform-*` carries the layout, so
   * it is written once; `.<page>-*` is what each page overrides (its column
   * widths, its select cap) and what the frontend tests address rows by.
   */
  function pformClass(prefix, suffix) {
    const base = "pform-" + suffix;
    return prefix ? base + " " + prefix + "-" + suffix : base;
  }

  /** Fire a notification without every caller rebuilding the CustomEvent. */
  function notify(level, message) {
    window.dispatchEvent(new CustomEvent("corvus:notification", { detail: { level, message } }));
  }

  /**
   * The "Reboot autopilot" button: calibrations and parameters that are read
   * at boot (SYS_AUTOSTART, SENS_EN_*, SER_*, output protocols) take effect
   * only after one. Asks first, because the link drops for a few seconds;
   * the backend refuses while armed, and so should the caller's gate, which
   * owns `disabled`.
   *
   * @param {{mount?: Element, size?: string, onRebooted?: function}} [opts]
   * @returns {HTMLButtonElement}
   */
  function rebootButton(opts) {
    const o = opts || {};
    const btn = Corvus.ui.button({
      variant: "secondary", size: o.size, icon: "power", label: "Reboot autopilot",
      className: "reboot-autopilot",
    });
    let modal = null;
    function close() { if (modal) { const m = modal; modal = null; m.close(); } }
    async function reboot() {
      close();
      btn.classList.add("is-busy");
      try {
        await Corvus.telemetry.postAction("/api/mavlink/reboot", {});
        notify("info", "The autopilot is rebooting. The link returns in a few seconds.");
        if (typeof o.onRebooted === "function") o.onRebooted();
      } catch (err) {
        notify("critical", "Reboot refused: " + ((err && err.message) || "no answer"));
      } finally {
        btn.classList.remove("is-busy");
      }
    }
    btn.addEventListener("click", () => {
      if (btn.disabled || modal) return;
      const body = el("div", "reboot-confirm",
        "The link to the vehicle drops while it restarts, and the parameters "
        + "are read again afterwards. The vehicle must be disarmed.");
      modal = Corvus.ui.modal({
        title: "Reboot the autopilot?", size: "sm", body,
        actions: [
          Corvus.ui.button({ variant: "secondary", label: "Cancel", onClick: close }),
          Corvus.ui.button({ variant: "primary", icon: "power", label: "Reboot", onClick: reboot }),
        ],
        mount: o.mount, onClose: () => { modal = null; },
      });
      modal.open();
    });
    return btn;
  }

  /** Trim float noise so 0.30000000000000004 does not reach the operator. */
  function formatNumber(value) {
    const n = Number(value);
    if (!isFinite(n)) return "";
    if (Number.isInteger(n)) return String(n);
    return String(Math.round(n * 1e6) / 1e6);
  }

  function isNumeric(v) {
    if (v === null || v === undefined || String(v).trim() === "") return false;
    return isFinite(Number(v));
  }

  /**
   * Why a typed value cannot be written, or "" when it can.
   *
   * The schema's bounds are checked here rather than left to the autopilot
   * because the mistake they catch is a plausible one with a real cost: the
   * battery thresholds are a fraction, so a 15 typed where 0.15 belongs would
   * put the low-battery failsafe permanently past its trigger.
   */
  function rangeProblem(field, raw) {
    if (!isNumeric(raw)) return "not a number";
    const value = Number(raw);
    if (field.min != null && value < Number(field.min)) return `below ${field.min}`;
    if (field.max != null && value > Number(field.max)) return `above ${field.max}`;
    return "";
  }

  /** Per-row status text (saving / saved / the refusal). */
  function setFieldStatus(status, cls, text) {
    if (!status) return;
    status.className = "params-row-status" + (cls ? " " + cls : "");
    status.textContent = text || "";
  }

  /** The page-level status line above the cards. */
  function setActionsStatus(status, cls, text) {
    if (!status) return;
    status.className = "params-actions-status" + (cls ? " " + cls : "");
    status.textContent = text || "";
  }

  /**
   * Track one control so an armed transition can re-gate it.
   *
   * `recheck` defaults to the plain "disabled while armed" rule; controls with
   * extra conditions (a motor spin button, the count buttons) pass their own.
   */
  function registerControl(state, el, recheck) {
    const fn = recheck || (() => {
      if (typeof el.disabled === "boolean") el.disabled = state.armed;
    });
    state.controls.push({ el, recheck: fn });
    fn();
  }

  /** Forget the controls inside `host` before its contents are rebuilt. */
  function dropControls(state, host) {
    state.controls = state.controls.filter((c) => !containsNode(host, c.el));
  }

  function containsNode(host, el) {
    let n = el;
    while (n) {
      if (n === host) return true;
      n = n.parentNode || n.parentElement;
    }
    return false;
  }

  function recheckAll(state) {
    state.controls.forEach((c) => { try { c.recheck(); } catch (_e) {} });
  }

  /** Show the armed banner and re-gate every control (PX4 refuses the writes). */
  function applyArmed(state, armed) {
    state.armed = !!armed;
    if (state.banner) state.banner.hidden = !state.armed;
    recheckAll(state);
  }

  /**
   * Snap a control back to the value the vehicle still holds.
   *
   * Called after a refused write. A control left showing DShot600, or a 500 m
   * geofence, that the autopilot never accepted is a lie about the aircraft.
   */
  function restoreControl(field, el) {
    if (field.kind === "sign") el.value = String(Number(field.value) < 0 ? -1 : 1);
    else if (field.kind === "enum") el.value = String(Math.round(Number(field.value)));
    else if (field.kind === "bitmask") repaintBits(field, el);
    else el.value = formatNumber(field.value);
  }

  /**
   * Put a bitmask control back in step with the value the vehicle still holds.
   *
   * The element is the wrapper the checkboxes live in, not an input, which is
   * why this cannot share the `el.value =` path above.
   */
  function repaintBits(field, wrapper) {
    const value = Number(field.value) || 0;
    const boxes = wrapper.querySelectorAll ? wrapper.querySelectorAll("input") : [];
    for (let i = 0; i < boxes.length; i += 1) {
      const bit = Number(boxes[i].dataset.bit);
      boxes[i].checked = (value & (1 << bit)) !== 0;
    }
  }

  /**
   * Write one field through the shared parameter endpoint.
   *
   * `onApplied(field)` runs only after the vehicle confirms, and is where a
   * page says what a successful write means to it — re-reading itself when the
   * write changed which fields exist at all.
   */
  async function applyParam(state, field, el, status, value, onApplied) {
    if (state.armed) return;
    // A page that offers "Check values" keeps what the operator asked for,
    // written or refused, so the check can compare against it and write it
    // again. A refused control snaps back, so the control cannot remember it.
    if (state.wanted) state.wanted[field.param] = value;
    setFieldStatus(status, "pending", "saving");
    try {
      await Corvus.telemetry.postAction("/api/params/set", { name: field.param, value });
      field.value = value;
      setFieldStatus(status, "ok", "saved");
      if (typeof onApplied === "function") onApplied(field);
    } catch (err) {
      const msg = (err && err.message) || "write failed";
      setFieldStatus(status, "err", msg);
      restoreControl(field, el);
      notify("critical", `Could not set ${field.param}: ${msg}`);
    }
  }

  /**
   * Keep the magnitude, take the sign.
   *
   * A "sign" field is one parameter carrying two things — a propeller's spin
   * direction in its sign, a tuned coefficient in its magnitude. Flipping the
   * direction must not quietly rewrite the magnitude to ±1, so `fallback` is
   * used only when the vehicle reports exactly 0, whose sign says nothing.
   */
  function signedValue(current, sign, fallback) {
    const magnitude = Math.abs(Number(current)) || Math.abs(Number(fallback)) || 1;
    return sign < 0 ? -magnitude : magnitude;
  }

  /**
   * Build one editable control for a schema field and register its armed
   * re-gate. Returns {el, status}.
   *
   * "enum" and "sign" render as a select; "number" as a numeric input applied
   * on change (blur/Enter), matching the Parameters editor's per-row apply. A
   * number outside the schema's own bounds never leaves the browser.
   *
   * opts: {prefix, onApplied, signFallback}
   */
  function paramControl(state, field, opts) {
    const o = opts || {};
    const status = el("span", "params-row-status", "");
    let control;

    if (field.kind === "bitmask") {
      /* One parameter, several independent switches. ArduPilot leans on these
         far more than PX4 does — FENCE_TYPE, FS_OPTIONS and ARMING_CHECK are
         all bit fields — and rendering one as a number asks an operator to do
         binary arithmetic on their aircraft's safety settings.

         Every toggle writes the whole recomputed word, because that is what
         PARAM_SET carries; bits the schema does not name are preserved, so a
         firmware with a bit this build has never heard of does not lose it the
         first time somebody ticks a box. */
      control = el("div", pformClass(o.prefix, "bits"));
      const boxes = [];
      (field.bits || []).forEach((bit) => {
        const label = el("label", pformClass(o.prefix, "bit"));
        const box = document.createElement("input");
        box.type = "checkbox";
        box.dataset.bit = String(bit.bit);
        box.checked = ((Number(field.value) || 0) & (1 << bit.bit)) !== 0;
        box.setAttribute("aria-label", `${bit.label} (${field.param} bit ${bit.bit})`);
        label.appendChild(box);
        label.appendChild(el("span", null, bit.label));
        control.appendChild(label);
        boxes.push(box);
        box.addEventListener("change", () => {
          let next = Number(field.value) || 0;
          if (box.checked) next |= (1 << bit.bit);
          else next &= ~(1 << bit.bit);
          // >>> 0 so a bit-31 mask stays a positive number: JavaScript's
          // bitwise operators work on signed 32-bit integers, and a negative
          // value reaches the autopilot as a different mask entirely.
          applyParam(state, field, control, status, next >>> 0, o.onApplied);
        });
      });
      control.dataset.param = field.param || "";
      // The wrapper has no `disabled` of its own, so the armed re-gate has to
      // reach each checkbox: a bitmask left live while the vehicle is armed is
      // a live write to ARMING_CHECK from an armed aircraft.
      registerControl(state, control, () => {
        boxes.forEach((box) => { box.disabled = state.armed; });
      });
      return { el: control, status };
    }

    if (field.kind === "enum" || field.kind === "sign") {
      const value = field.kind === "sign"
        ? (Number(field.value) < 0 ? -1 : 1)
        : Math.round(Number(field.value));
      control = Corvus.ui.select({
        className: pformClass(o.prefix, "select"),
        ariaLabel: `${field.label} (${field.param})`,
        title: field.param,
        options: (field.options || []).map((op) => ({ value: op.value, label: op.label })),
        value: value,
      });
      control.addEventListener("change", () => {
        const picked = Number(control.value);
        const next = field.kind === "sign"
          ? signedValue(field.value, picked, o.signFallback)
          : picked;
        applyParam(state, field, control, status, next, o.onApplied);
      });
    } else {
      control = Corvus.ui.input({
        className: pformClass(o.prefix, "input"),
        mono: true,
        value: formatNumber(field.value),
        ariaLabel: `${field.label} (${field.param})`,
        title: field.param,
        autocomplete: false,
      });
      if (field.step != null) control.step = String(field.step);
      control.addEventListener("change", () => {
        const problem = rangeProblem(field, control.value);
        if (problem) {
          control.classList.add("invalid");
          setFieldStatus(status, "err", problem);
          return;
        }
        control.classList.remove("invalid");
        applyParam(state, field, control, status, Number(control.value), o.onApplied);
      });
    }

    control.dataset.param = field.param || "";
    registerControl(state, control);
    return { el: control, status };
  }

  /** A plain form: one labelled control per field. opts as paramControl. */
  function paramFieldGrid(state, fields, opts) {
    const o = opts || {};
    const grid = el("div", pformClass(o.prefix, "grid"));
    (fields || []).forEach((field) => {
      const row = el("div", pformClass(o.prefix, "field"));
      row.dataset.param = field.param || "";
      row.appendChild(el("span", pformClass(o.prefix, "field-label"),
        field.label || field.param || ""));
      const cell = el("div", pformClass(o.prefix, "field-control"));
      const built = paramControl(state, field, o);
      cell.appendChild(built.el);
      if (field.unit) cell.appendChild(el("span", pformClass(o.prefix, "unit"), field.unit));
      cell.appendChild(built.status);
      row.appendChild(cell);
      if (field.hint) {
        row.appendChild(el("span", "field-hint " + pformClass(o.prefix, "field-hint"),
          field.hint));
      }
      grid.appendChild(row);
    });
    if (state && state.check) markChecked(state.check, grid);
    return grid;
  }

  // -------------------------------------------------------------------------
  // Check values: read back, write again what did not stick, redraw
  // -------------------------------------------------------------------------
  //
  // Every setup page writes a field the moment it changes, and a write can be
  // lost on a radio link or refused by the vehicle. "Check values" is the way
  // to be sure: it reads each field back from the vehicle (never from the
  // backend's cache), writes again whatever the operator set that did not
  // stick, and has the page redraw itself from what the vehicle holds. What
  // the operator asked for lives in `state.wanted` (see applyParam) until a
  // check confirms it, because a refused field snaps back and cannot
  // remember it.
  //
  // A page opts in with `wanted: {}`, `check: null` and `checking: false` on
  // its state, a checkButton in its actions bar, a checkCard at the top of
  // its content while `state.check` is set, and a loader that can read fresh.
  // paramFieldGrid marks each checked field in place.

  // POST /api/params/verify refuses more names than this in one request.
  const CHECK_MAX_NAMES = 200;

  /**
   * Every editable parameter in a page description: any object carrying a
   * `param` and a `kind`, which is what a field is on every page. Option
   * tables, output catalogues and sensor driver lists have no `kind`, so a
   * page that writes those as well adds them itself.
   */
  function fieldParams(doc) {
    const names = [];
    // Pages hang their own bookkeeping off the description under "_" keys
    // (Safety keeps the controls of a sensor panel on `section._ctx`), and a
    // control leads back into the DOM, so those are not walked, and nothing
    // is walked twice.
    const seen = new Set();
    (function walk(node) {
      if (!node || typeof node !== "object" || seen.has(node)) return;
      seen.add(node);
      if (Array.isArray(node)) { node.forEach(walk); return; }
      if (typeof node.param === "string" && node.param && typeof node.kind === "string"
          && names.indexOf(node.param) < 0) {
        names.push(node.param);
      }
      Object.keys(node).forEach((key) => {
        if (key !== "options" && key.charAt(0) !== "_") walk(node[key]);
      });
    })(doc);
    return names;
  }

  function checkNames(state, opts) {
    return typeof opts.names === "function" ? opts.names(state) : fieldParams(state.doc);
  }

  function checkable(state, opts) {
    return !state.loading && !state.checking
      && !!(state.doc && state.doc.connected) && checkNames(state, opts).length > 0;
  }

  /**
   * The "Check values" button.
   *
   * `opts.prefix` names the page for its CSS hook, `opts.reload(state)` must
   * return a promise for a fresh read and redraw, `opts.setStatus(state, cls,
   * text)` writes the page's status line, and `opts.names(state)` may replace
   * the default field walk.
   */
  function checkButton(state, opts) {
    const o = opts || {};
    const btn = Corvus.ui.button({
      variant: "secondary", size: "sm", icon: "list-checks", label: "Check values",
      className: (o.prefix ? o.prefix + "-check " : "") + "check-values",
    });
    btn.title = "Read every value back from the vehicle, write again what did "
      + "not stick, and show what the vehicle holds now";
    btn.disabled = !checkable(state, o);
    btn.addEventListener("click", () => runCheck(state, o));
    state.checkBtn = btn;
    return btn;
  }

  /** Re-gate the button after a read, without a repaint. */
  function recheckButton(state, opts) {
    if (state.checkBtn) state.checkBtn.disabled = !checkable(state, opts || {});
  }

  async function runCheck(state, opts) {
    if (!checkable(state, opts)) return;
    const targets = Object.keys(state.wanted || {})
      .map((name) => ({ name, value: state.wanted[name] }));
    // Capped to what one request may name. A larger page loses nothing: the
    // reload after the check reads every value fresh anyway.
    const names = checkNames(state, opts)
      .slice(0, Math.max(0, CHECK_MAX_NAMES - targets.length));
    state.checking = true;
    if (state.checkBtn) state.checkBtn.disabled = true;
    if (state.reloadBtn) state.reloadBtn.disabled = true;
    opts.setStatus(state, "pending", targets.length
      ? `Checking ${targets.length} ${targets.length === 1 ? "change" : "changes"} on the vehicle…`
      : "Reading every value back from the vehicle…");
    let check;
    try {
      const res = await Corvus.telemetry.postAction(
        "/api/params/verify", { params: targets, names });
      if (state.destroyed) return;
      (res.results || []).forEach((r) => { if (r.ok && state.wanted) delete state.wanted[r.name]; });
      check = summarizeCheck(res);
    } catch (err) {
      if (state.destroyed) return;
      check = { cls: "err", text: (err && err.message) || "The check could not run", rows: [] };
    }
    state.checking = false;
    state.check = check;
    // Redraw from the vehicle, so every field shows what it holds now.
    await opts.reload(state);
    if (state.destroyed) return;
    opts.setStatus(state, check.cls, check.text);
    notify(check.cls === "ok" ? "info" : "warning", check.text);
  }

  function summarizeCheck(res) {
    const rows = Array.isArray(res && res.results) ? res.results : [];
    const read = Object.keys((res && res.values) || {}).length;
    const missing = Array.isArray(res && res.missing) ? res.missing : [];
    const failed = rows.filter((r) => !r.ok);
    const rewritten = rows.filter((r) => r.ok && r.rewritten);
    let cls = "ok";
    let text;
    if (!rows.length) {
      text = `No changes to confirm on this page. ${read} values read back from the vehicle.`;
    } else if (!failed.length) {
      text = rows.length === 1
        ? `${rows[0].name} is on the vehicle.`
        : `All ${rows.length} changes are on the vehicle.`;
      if (rewritten.length) {
        text += rewritten.length === 1 && rows.length === 1
          ? " It had to be written again."
          : ` ${rewritten.length} had to be written again.`;
      }
    } else {
      cls = "err";
      const names = failed.map((r) => r.name).join(", ");
      text = (rows.length === 1
        ? `${names} is not on the vehicle.`
        : `${failed.length} of ${rows.length} changes are not on the vehicle: ${names}.`)
        + " Press Check values again to retry.";
    }
    if (missing.length) {
      cls = "err";
      text += ` No answer for ${missing.join(", ")}.`;
    }
    return { cls, text, rows };
  }

  function formatChecked(value) {
    return value === null || value === undefined ? "no answer" : formatNumber(value);
  }

  function checkOutcome(row) {
    if (row.ok) return row.rewritten ? "written again, confirmed" : "confirmed";
    return row.error || "not applied";
  }

  /** The last check, one row per value the operator set. */
  function checkCard(check, prefix) {
    const p = prefix || "setup";
    const card = el("div", `page-card ${p}-card ${p}-check-card check-card`);
    card.dataset.section = "check";
    card.appendChild(sectionTitle("Check result"));
    card.appendChild(el("div", `${p}-check-summary check-summary ${check.cls || ""}`, check.text));
    if (check.rows && check.rows.length) {
      const table = el("div", `${p}-check-rows check-rows`);
      const head = el("div", `${p}-check-row ${p}-check-head check-row check-head`);
      ["Parameter", "Wanted", "Vehicle now", "Result"].forEach((t) => head.appendChild(el("span", null, t)));
      table.appendChild(head);
      check.rows.forEach((row) => {
        const state = row.ok ? "ok" : "err";
        const line = el("div", `${p}-check-row check-row ${state}`);
        line.dataset.param = row.name;
        line.appendChild(el("span", `${p}-check-param check-param`, row.name));
        line.appendChild(el("span", `${p}-check-value check-value`, formatChecked(row.wanted)));
        line.appendChild(el("span", `${p}-check-value check-value`, formatChecked(row.after)));
        line.appendChild(el("span", `${p}-check-outcome check-outcome`, checkOutcome(row)));
        table.appendChild(line);
      });
      card.appendChild(table);
    }
    return card;
  }

  /** Put each checked value's outcome next to its field, where it was typed. */
  function markChecked(check, grid) {
    const rows = (check && check.rows) || [];
    if (!rows.length || !grid) return;
    const byName = {};
    rows.forEach((r) => { byName[r.name] = r; });
    Array.prototype.forEach.call(grid.children || [], (fieldRow) => {
      const r = fieldRow && fieldRow.dataset && byName[fieldRow.dataset.param];
      if (!r) return;
      const status = fieldRow.querySelector(".params-row-status");
      if (r.ok) setFieldStatus(status, "ok", checkOutcome(r));
      else setFieldStatus(status, "err", `wanted ${formatChecked(r.wanted)}, vehicle holds ${formatChecked(r.after)}`);
    });
  }

  return {
    MAX_POINTS, REDRAW_MIN_MS,
    get COLOR_RATE() { return chartColor("nav", "#4CC9FF"); },
    get COLOR_ATT() { return chartColor("healthy", "#45D483"); },
    get COLOR_VEL() { return chartColor("nav", "#4CC9FF"); },
    reducedMotion, el, icon, refreshIcons, pageHeader, sectionTitle, infoRow,
    backButton, plotlyLayout, plotlyConfig,
    // Schema-driven parameter forms (Motors, Safety & Sensors).
    notify, formatNumber, isNumeric, rangeProblem,
    setFieldStatus, setActionsStatus,
    registerControl, dropControls, recheckAll, applyArmed,
    restoreControl, applyParam, signedValue, paramControl, paramFieldGrid,
    pformClass, rebootButton,
    // Check values, shared by every page that writes fields one at a time.
    fieldParams, checkButton, recheckButton, checkCard, markChecked,
    summarizeCheck, CHECK_MAX_NAMES,
  };
})();
