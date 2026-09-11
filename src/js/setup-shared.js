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
    else el.value = formatNumber(field.value);
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
    return grid;
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
  };
})();
