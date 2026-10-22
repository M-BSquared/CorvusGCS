"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupTuning — the PID Tuning sub-page of the Setup page.
 *
 * The control cascade, one block per loop on a single page, innermost first —
 * the way PX4 is built and the way QGroundControl presents it:
 *
 *   Rate Controller       the loop that decides how the aircraft feels
 *   Attitude Controller   angle error to rate setpoint
 *   Velocity Controller   the loop behind a drifting hover (multicopter)
 *   Position Controller   position error to velocity setpoint (multicopter)
 *
 * Each block holds the loop's live graphs and gains on the left and its own
 * tune button on the right. The autotune itself is one run on the vehicle: a
 * block that is tuned by it starts that run, a block that is tuned together
 * with another says so on its button, and a block no autotune covers shows a
 * greyed button with the reason. The status card above the blocks carries the
 * run's progress, what the autopilot is doing, and whether a reboot is needed.
 *
 * This page replaces the "PID Tuning" band that used to sit at the bottom of
 * the Calibration page, which was one button and three read-only graphs. That
 * band could not tune anything:
 *
 *   - The button was refused whenever the vehicle was armed. PX4's autotune
 *     runs IN FLIGHT — it injects steps into the rate controller and measures
 *     the response — so the only state Corvus would send the command in was the
 *     one state PX4 rejects it in. No autotune could ever start. The gate is now
 *     the real one (armed, and not sitting on the ground), enforced in the
 *     bridge, and stated here before the operator takes off.
 *   - There was no way to set a gain by hand. Autotune tunes two of the four
 *     loops; nothing tunes the other two, and no autotune result is final
 *     without someone able to nudge it. Every loop is editable here.
 *   - The graphs plotted the response with nothing to compare it against. Each
 *     chart now draws the controller's setpoint beside what the vehicle did,
 *     which is the pair a tuning decision is actually read off.
 *
 * Schema-driven, like Motors and Safety & Sensors: GET /api/tuning returns a
 * *description* built by corvus/tuning_config.py — groups of sections of
 * fields, each field naming the PX4 parameter it writes. Nothing here hardcodes
 * a parameter name, so a firmware that lacks one sends one field fewer and this
 * page renders one field fewer, and the same page serves a multicopter and a
 * fixed wing (AGENTS.md: graceful fallback across PX4 v1.16 / v1.17 / v1.18).
 *
 * Backend contract:
 *   GET  /api/tuning                    {connected,groups,received}
 *   POST /api/params/set {name,value}   write one gain (refused while armed)
 *   POST /api/autotune {axis,enabled}   start / stop the tune
 *   POST /api/tuning/stream {enabled}   raise the setpoint stream while open
 *
 * Gains are written while DISARMED. That is the existing Corvus armed-safety
 * contract (POST /api/params/set is refused while armed, in the bridge as well
 * as here), so the workflow this page supports is land, adjust, fly again —
 * not sliders moved mid-hover. The banner says so rather than letting the
 * operator discover it from a refused write.
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the lifecycle and calls destroy() on back / left-nav re-entry, which
 * releases the telemetry subscription, purges every Plotly graph, and hands
 * the setpoint stream rate back to the firmware.
 */
Corvus.setupTuning = (function () {
  const S = Corvus.setupShared;
  const { registerControl, applyArmed, notify } = S;

  /** Rate the setpoint messages are asked for while this page is open. Fast
   *  enough that a step response is visible, slow enough to stay polite on a
   *  telemetry radio. */
  const STREAM_HZ = 20;

  /** Written to the vehicle with the autotune module switched on: the module
   *  only starts at boot, so nothing runs until the autopilot restarts. */
  const REBOOT_TEXT_OFF =
    "The autotune module is switched off on this autopilot. Switch it on in "
    + "the autotune settings below, then reboot the autopilot before starting a tune.";
  const REBOOT_TEXT_PENDING =
    "The autotune module was switched on. Reboot the autopilot now: it only "
    + "starts after a restart.";

  const DEFAULT_RUNNING_TEXT =
    "The autopilot is tuning. Keep your hands off the sticks unless the vehicle misbehaves.";
  const DEFAULT_DONE_TEXT =
    "The tune finished. Test the new gains gently before flying normally.";
  const FAILED_TEXT =
    "The tune did not finish. The console has the autopilot's reason. If the module "
    + "was only just switched on, reboot the autopilot and try again.";

  const AUTOTUNE_PHASE_TEXT = {
    "": "Idle",
    running: "Tuning",
    done: "Complete",
    failed: "Failed",
  };

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page tune-page");
    page.appendChild(S.backButton(navigateBack));
    const header = S.pageHeader("PID Tuning",
      "The control cascade, one loop at a time, by hand or by autotune");

    // Readiness strip: what each action needs, stated before it is attempted.
    // Autotune and a gain edit want opposite states, which is exactly why both
    // are shown rather than one "ready" light. It sits with the actions, in
    // the title row.
    const ready = S.el("div", "calib-ready tune-ready");
    const linkChip = readyChip("link", "Link");
    const flightChip = readyChip("flight", "Disarmed");
    ready.appendChild(linkChip.el);
    ready.appendChild(flightChip.el);

    const actions = S.el("div", "params-actions");
    const reloadBtn = Corvus.ui.button({
      variant: "primary", size: "sm", icon: "refresh-cw", label: "Reload",
    });
    const actionsStatus = S.el("div", "params-actions-status");
    // The link chip above already says when there is no vehicle.
    actionsStatus.dataset.linkChip = "1";
    actions.appendChild(ready);
    actions.appendChild(reloadBtn);
    // Placed once the state exists: the button reads it.
    const checkSlot = S.el("span", "tune-check-slot");
    actions.appendChild(checkSlot);
    actions.appendChild(actionsStatus);
    page.appendChild(S.pageHead(header, actions));

    const banner = S.el("div", "params-banner");
    banner.hidden = true;
    banner.textContent =
      "Gains are read-only while armed. Land and disarm to change them. "
      + "The autotune is the opposite: it only runs in flight.";
    page.appendChild(banner);

    const tabs = S.el("div", "tune-tabs");
    tabs.setAttribute("role", "tablist");
    page.appendChild(tabs);

    const host = S.el("div", "page-section tune-panel");
    page.appendChild(host);
    container.appendChild(page);

    // `controls` holds one recheck() per editable control so an armed
    // transition can re-gate the page without walking the DOM. `charts` and
    // `autotune` are the live views the telemetry subscription drives.
    const state = {
      host, tabs, banner, reloadBtn, actionsStatus,
      armed: false, connected: false, landed: 0,
      loading: false, destroyed: false, controls: [],
      groups: [], restartPending: false,
      charts: null, autotune: null, savedBufs: {},
      snapshot: {},
      linkChip, flightChip,
      reduced: S.reducedMotion(),
      startMs: Date.now(),
      // Check values (setupShared): the description it reads the field list
      // from, what the operator set until a check confirms it, and the last
      // check's outcome until the next one.
      doc: null, wanted: {}, check: null, checking: false,
    };
    checkSlot.appendChild(S.checkButton(state, CHECK));

    applyVehicle(state, Corvus.telemetry && Corvus.telemetry.getState());
    paintReady(state);

    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      state.unsub = Corvus.telemetry.subscribe((s) => onTelemetry(state, s));
    }

    // Ask PX4 for the setpoint messages at a tuning rate for as long as this
    // page is open, and hand the rate back in destroy(). A refusal is not
    // fatal: the response traces still draw, the setpoint ones are simply
    // slower or absent, which the charts show honestly.
    setStream(true);

    reloadBtn.addEventListener("click", () => {
      state.check = null;
      load(state);
    });
    load(state);

    function setStream(enabled) {
      if (!Corvus.telemetry || typeof Corvus.telemetry.postAction !== "function") return;
      Corvus.telemetry.postAction("/api/tuning/stream",
        { enabled: enabled, rate_hz: STREAM_HZ }).catch(() => {});
    }

    return function destroy() {
      state.destroyed = true;
      if (state.unsub) { try { state.unsub(); } catch (_e) {} state.unsub = null; }
      setStream(false);
      purgeCharts(state);
    };
  }

  /* ================================================================== */
  /* Readiness                                                           */
  /* ================================================================== */

  function readyChip(key, label) {
    const el = S.el("div", "calib-ready-chip");
    el.dataset.key = key;
    el.appendChild(S.el("span", "calib-ready-dot"));
    const text = S.el("span", "calib-ready-label", label);
    el.appendChild(text);
    return {
      el,
      set(ok, next) {
        el.dataset.state = ok ? "ok" : "bad";
        if (next !== undefined) text.textContent = next;
      },
    };
  }

  /** True when the vehicle is where PX4 will accept an autotune: armed, and
   *  not reported as sitting on the ground. An unknown landed state (0, the
   *  firmware does not publish EXTENDED_SYS_STATE) is not treated as grounded —
   *  the backend applies the same rule, and PX4 remains the authority. */
  function isFlying(state) {
    return state.connected && state.armed && state.landed !== 1;
  }

  function applyVehicle(state, s) {
    state.armed = !!(s && s.armed);
    state.connected = !!(s && s.connected);
    state.landed = Number(s && s.landed_state) || 0;
  }

  /* ================================================================== */
  /* Load + render                                                       */
  /* ================================================================== */

  // Check values: the shared read back, redrawn by a fresh load().
  const CHECK = {
    prefix: "tune",
    reload: (state) => load(state, true),
    setStatus: (state, cls, text) => S.setActionsStatus(state.actionsStatus, cls, text),
  };

  /** `fresh` reads every gain from the vehicle rather than the cache. */
  function load(state, fresh) {
    if (state.loading) return Promise.resolve();
    state.loading = true;
    state.reloadBtn.disabled = true;
    S.recheckButton(state, CHECK);
    S.setActionsStatus(state.actionsStatus, "pending", "Reading tuning parameters…");
    return Corvus.telemetry.requestJson(fresh ? "/api/tuning?fresh=1" : "/api/tuning").then((doc) => {
      if (state.destroyed) return;
      state.doc = doc || {};
      renderGroups(state, doc || {});
      if (!doc || !doc.connected) {
        S.setActionsStatus(state.actionsStatus, "err",
          (doc && doc.error) || "No tuning parameters received");
      } else {
        S.setActionsStatus(state.actionsStatus, "ok", `${doc.received} parameters read`);
      }
    }).catch((err) => {
      if (state.destroyed) return;
      state.doc = {};
      renderGroups(state, {});
      S.setActionsStatus(state.actionsStatus, "err",
        (err && err.message) || "Could not read the tuning parameters");
    }).finally(() => {
      if (state.destroyed) return;
      state.loading = false;
      state.reloadBtn.disabled = false;
      S.recheckButton(state, CHECK);
    });
  }

  function renderGroups(state, doc) {
    purgeCharts(state);
    state.groups = Array.isArray(doc.groups) ? doc.groups : [];
    state.controls = [];
    state.autotune = null;
    Corvus.ui.clear(state.tabs);
    state.host.innerHTML = "";

    if (!state.groups.length) {
      const card = S.el("div", "page-card tune-card");
      card.appendChild(S.sectionTitle("PID Tuning"));
      card.appendChild(S.el("div", "params-desc",
        "Connect to a vehicle to read its controller gains. The page shows only "
        + "the loops the connected firmware actually reports, so a multicopter "
        + "and a fixed wing each get their own."));
      state.host.appendChild(card);
      S.refreshIcons();
      return;
    }

    state.charts = newChartSet(state);
    const autotune = state.groups.find((g) => g.kind === "autotune") || null;
    const loops = state.groups.filter((g) => g.kind !== "autotune");

    if (state.check) state.host.appendChild(S.checkCard(state.check, "tune"));
    if (autotune) buildAutotune(state, autotune).forEach((el) => state.host.appendChild(el));

    loops.forEach((group) => {
      const tab = document.createElement("button");
      tab.type = "button";
      tab.className = "tune-tab";
      tab.dataset.group = group.id;
      tab.textContent = group.title || group.id;
      tab.addEventListener("click", () => jumpToLoop(state, group.id));
      state.tabs.appendChild(tab);
      state.host.appendChild(buildLoop(state, group));
    });
    if (autotune) {
      const settings = buildAutotuneSettings(state, autotune);
      if (settings) state.host.appendChild(settings);
    }

    applyArmed(state, state.armed);
    if (state.autotune) state.autotune.paint();
    S.refreshIcons();
    // Plotly sizes to the element, which has only just been laid out.
    redrawAll(state);
  }

  /** The tab strip is a jump list: every loop is on the page already. */
  function jumpToLoop(state, groupId) {
    Array.prototype.forEach.call(state.tabs.children, (tab) => {
      const active = tab.dataset.group === groupId;
      tab.classList.toggle("active", active);
      tab.setAttribute("aria-current", active ? "true" : "false");
    });
    const block = findLoop(state, groupId);
    if (block && typeof block.scrollIntoView === "function") {
      block.scrollIntoView({ behavior: state.reduced ? "auto" : "smooth", block: "start" });
    }
  }

  function findLoop(state, groupId) {
    return Array.prototype.find.call(state.host.children,
      (child) => child.dataset && child.dataset.loop === groupId) || null;
  }

  /** One controller: its graphs and gains on the left, its tune button on the right. */
  function buildLoop(state, group) {
    const block = S.el("section", "tune-loop");
    block.dataset.loop = group.id;
    block.appendChild(S.el("h3", "tune-loop-title", group.title || group.id));
    if (group.hint) block.appendChild(S.el("div", "tune-hint", group.hint));

    const body = S.el("div", "tune-loop-body");
    const main = S.el("div", "tune-loop-main");
    if (group.charts && group.charts.length) main.appendChild(buildCharts(state, group));
    (group.sections || []).forEach((section) => {
      const card = S.el("div", "page-card tune-card");
      card.dataset.section = section.id || "";
      card.appendChild(S.sectionTitle(section.title || ""));
      card.appendChild(S.paramFieldGrid(state, section.fields || [], { prefix: "tune" }));
      main.appendChild(card);
    });
    body.appendChild(main);
    body.appendChild(buildTuneSide(state, group));
    block.appendChild(body);
    return block;
  }

  /* ================================================================== */
  /* Charts: setpoint against response                                   */
  /* ================================================================== */

  function newChartSet(state) {
    const set = {
      charts: [], config: S.plotlyConfig(state.reduced),
      lastRedraw: 0, unsubTheme: null, observer: null,
    };
    // Plotly holds the colors it was handed, so a theme switch has to force a
    // redraw. Released in purgeCharts along with the graphs themselves.
    set.unsubTheme = Corvus.ui.onThemeChange(() => redrawAll(state));
    // Every loop's graphs are on one page now; a graph scrolled out of view is
    // still buffered but not redrawn, so ten live graphs do not cost ten
    // Plotly passes ten times a second.
    if (typeof window !== "undefined" && typeof window.IntersectionObserver === "function") {
      set.observer = new window.IntersectionObserver((records) => {
        records.forEach((record) => {
          const entry = set.charts.find((c) => c.chart === record.target);
          if (!entry) return;
          entry.visible = !!record.isIntersecting;
          if (entry.visible) redraw(state, entry);
        });
      });
    }
    return set;
  }

  function buildCharts(state, group) {
    const wrap = S.el("div", "tune-charts");
    const set = state.charts;
    (group.charts || []).forEach((spec) => {
      const host = S.el("div", "tune-chart-host");
      const head = S.el("div", "tune-chart-head");
      head.appendChild(S.el("span", "tune-chart-title", spec.title || ""));
      const live = S.el("span", "autotune-live");
      live.appendChild(S.el("span", "autotune-live-dot"));
      live.appendChild(S.el("span", "tune-chart-live-text", "live"));
      head.appendChild(live);
      host.appendChild(head);
      const chart = S.el("div", "tune-chart");
      chart.dataset.chart = spec.id || "";
      host.appendChild(chart);
      wrap.appendChild(host);
      // A reload re-reads the gains and rebuilds the page; the traces of the
      // run that just finished are what the operator wants to look at next,
      // so they are carried over.
      const buf = state.savedBufs[spec.id] || { t: [], sp: [], actual: [] };
      delete state.savedBufs[spec.id];
      const entry = { spec, chart, live, dead: false, visible: true, buf };
      set.charts.push(entry);
      if (set.observer) set.observer.observe(chart);
    });
    return wrap;
  }

  // Plotly.react only redraws data it sees as new, and pushSample mutates the
  // buffer in place, so every draw hands it fresh arrays.
  function traces(state, entry) {
    const palette = Corvus.ui.chartColors();
    const t = entry.buf.t.slice();
    const out = [{
      x: t, y: entry.buf.actual.slice(), mode: "lines", name: "response",
      line: { color: palette.nav || "#4CC9FF", width: 1.6 },
    }];
    // The setpoint is drawn only when the vehicle is actually sending one. A
    // dashed line pinned at zero would read as "the controller is commanding
    // nothing", which is a different and much more alarming claim than "this
    // firmware is not streaming its setpoint".
    if (entry.spec.setpoint && state.snapshot.setpoints_live) {
      out.push({
        x: t, y: entry.buf.sp.slice(), mode: "lines", name: "setpoint",
        line: { color: palette.healthy || "#45D483", width: 1.4, dash: "dot" },
      });
    }
    return out;
  }

  /* Plotly.react does part of its work asynchronously — the auto-margin pass
     resolves a tick or more after the call returns. Switching tabs purges the
     chart and drops the element in between, and that pending pass then throws
     on a graph that no longer exists. The synchronous try/catch cannot see an
     async rejection, so the returned promise is caught too, and a chart whose
     set has already been purged is never redrawn in the first place. */
  function redraw(state, entry) {
    if (typeof window === "undefined") return;
    if (!state.charts || entry.dead || !entry.visible) return;
    // Plotly is fetched on first use (js/lazy.js). This runs at ~10 Hz off
    // live telemetry, so the guard has to be the cheap synchronous one and
    // the re-entry has to re-check `entry.dead`: a tab switch purges the
    // chart and drops the element while the bundle is still in flight.
    if (typeof window.Plotly === "undefined") {
      if (!Corvus.lazy || typeof Corvus.lazy.plotly !== "function") return;
      Corvus.lazy.plotly()
        .then(() => { if (state.charts && !entry.dead) redraw(state, entry); })
        .catch((err) => console.error("tuning charts unavailable:", err));
      return;
    }
    try {
      const pending = window.Plotly.react(entry.chart, traces(state, entry),
        S.plotlyLayout(entry.spec.unit || ""), state.charts.config);
      Corvus.ui.attachZoomHint(entry.chart);
      if (pending && typeof pending.catch === "function") {
        pending.catch(() => {});
      }
    } catch (err) {
      console.error("tuning Plotly.react failed:", err);
    }
  }

  function redrawAll(state) {
    if (!state.charts) return;
    state.charts.lastRedraw = Date.now();
    state.charts.charts.forEach((entry) => redraw(state, entry));
  }

  function purgeCharts(state) {
    if (!state.charts) return;
    if (state.charts.unsubTheme) {
      try { state.charts.unsubTheme(); } catch (_e) {}
    }
    if (state.charts.observer) {
      try { state.charts.observer.disconnect(); } catch (_e) {}
    }
    // Marked before the purge, not after: a redraw queued for this frame must
    // not reach a graph that is about to be torn out from under it.
    state.charts.charts.forEach((entry) => { entry.dead = true; });
    if (typeof window !== "undefined" && window.Plotly) {
      state.charts.charts.forEach((entry) => {
        try { window.Plotly.purge(entry.chart); } catch (_e) {}
      });
    }
    state.charts.charts.forEach((entry) => { state.savedBufs[entry.spec.id] = entry.buf; });
    state.charts = null;
  }

  function pushSample(state, s) {
    if (!state.charts) return;
    const tSec = (Date.now() - state.startMs) / 1000;
    state.charts.charts.forEach((entry) => {
      const actual = Number(s[entry.spec.actual]);
      if (!isFinite(actual)) return;
      const buf = entry.buf;
      buf.t.push(tSec);
      buf.actual.push(actual);
      const sp = entry.spec.setpoint ? Number(s[entry.spec.setpoint]) : NaN;
      buf.sp.push(isFinite(sp) ? sp : null);
      if (buf.t.length > S.MAX_POINTS) {
        buf.t.shift();
        buf.actual.shift();
        buf.sp.shift();
      }
    });
    if (Date.now() - state.charts.lastRedraw >= S.REDRAW_MIN_MS) redrawAll(state);
  }

  /* ================================================================== */
  /* Autotune                                                            */
  /* ================================================================== */

  /**
   * The run, as two cards: a slim strip that stays in view while the operator
   * scrolls from one loop's graphs to the next (progress, what the autopilot is
   * doing, anything blocking the start), and below it the card of what is set
   * before a tune (axes, the steps). Returned as a pair, in page order.
   */
  function buildAutotune(state, group) {
    const strip = S.el("div", "page-card tune-run");
    const card = S.el("div", "page-card tune-autotune");

    const head = S.el("div", "tune-autotune-head");
    head.appendChild(S.el("span", "tune-autotune-label", group.label || "Autotune"));
    const phase = S.el("span", "calib-phase tune-autotune-phase");
    phase.dataset.phase = "";
    head.appendChild(phase);
    strip.appendChild(head);

    // The run, in one place: how far it is, and what the autopilot is doing.
    // The buttons are beside each controller below; this is what they start.
    const barRow = S.el("div", "tune-progress-row");
    const bar = S.el("div", "calib-progress tune-progress");
    const fill = S.el("div", "calib-progress-fill");
    bar.appendChild(fill);
    bar.setAttribute("role", "progressbar");
    bar.setAttribute("aria-valuemin", "0");
    bar.setAttribute("aria-valuemax", "100");
    barRow.appendChild(bar);
    const percent = S.el("span", "tune-progress-percent");
    const elapsed = S.el("span", "tune-progress-elapsed");
    barRow.appendChild(percent);
    barRow.appendChild(elapsed);
    strip.appendChild(barRow);

    const message = S.el("div", "tune-message");
    message.setAttribute("role", "status");
    message.setAttribute("aria-live", "polite");
    strip.appendChild(message);

    // A reboot is asked for in words and offered as a button, so an operator
    // who has just switched the module on is not left to find the command.
    const reboot = S.el("div", "params-banner tune-module-warning");
    reboot.hidden = true;
    const rebootText = S.el("span", "tune-module-warning-text");
    reboot.appendChild(rebootText);
    const rebootBtn = S.rebootButton({
      size: "sm",
      onRebooted: () => { state.restartPending = false; paintSoon(state); },
    });
    reboot.appendChild(rebootBtn);
    strip.appendChild(reboot);

    const gate = S.el("div", "params-banner setup-armed-banner tune-gate");
    gate.hidden = true;
    strip.appendChild(gate);

    const axes = buildAutotuneAxes(state, group.axes);
    if (axes) card.appendChild(axes);

    // The preconditions, before the aircraft leaves the ground rather than
    // after PX4 has refused with a result code that names none of them.
    const details = S.el("details", "tune-steps-box");
    details.appendChild(S.el("summary", "tune-steps-summary", "Before you start"));
    const steps = S.el("ol", "tune-steps");
    (group.steps || []).forEach((line) => {
      const item = document.createElement("li");
      item.className = "tune-step";
      item.textContent = line;
      steps.appendChild(item);
    });
    details.appendChild(steps);
    card.appendChild(details);

    const axesField = group.axes && group.axes.selectable ? group.axes.field : null;
    const ctx = {
      group, phase, bar, fill, percent, elapsed, message, gate, reboot, rebootText,
      rebootBtn, axesField, sides: [], busy: false, runStart: 0,
    };
    ctx.paint = () => paintAutotune(state, ctx);
    state.autotune = ctx;
    return [strip, card];
  }

  /** The autotune's own settings, below the loops: they are written like any
   *  gain, so they sit with the gains rather than with the run. */
  function buildAutotuneSettings(state, group) {
    const sections = group.sections || [];
    if (!sections.length) return null;
    const wrap = S.el("section", "tune-loop tune-loop-settings");
    wrap.dataset.loop = group.id;
    sections.forEach((section) => {
      const card = S.el("div", "page-card tune-card");
      card.dataset.section = section.id || "";
      card.appendChild(S.sectionTitle(section.title || ""));
      card.appendChild(S.paramFieldGrid(state, section.fields || [], {
        prefix: "tune",
        onApplied: (field) => onSettingApplied(state, group, field),
      }));
      wrap.appendChild(card);
    });
    return wrap;
  }

  /** The module is read at boot, so switching it on changes nothing until the
   *  autopilot restarts; the page says so and holds the start back until then. */
  function onSettingApplied(state, group, field) {
    if (group.enable_param && field.param === group.enable_param) {
      if (Number(field.value) === 1) {
        if (!group.enabled) state.restartPending = true;
        group.enabled = true;
      } else {
        group.enabled = false;
        state.restartPending = false;
      }
    }
    paintSoon(state);
  }

  function paintSoon(state) {
    if (state.autotune) state.autotune.paint();
  }

  /**
   * The tune button beside one controller.
   *
   * The autotune is one run on the vehicle, so what a button does depends on
   * how the firmware covers the loop (`group.tune`, from the schema): `run`
   * starts the tune of this loop, `with` starts the run this loop is tuned in
   * and says so, `manual` is a loop no autotune covers, greyed with the reason.
   */
  function buildTuneSide(state, group) {
    const tune = group.tune || { mode: "manual", text: "" };
    const side = S.el("aside", "page-card tune-side");
    side.dataset.loop = group.id;
    side.dataset.mode = tune.mode;
    side.appendChild(S.sectionTitle("Autotune"));

    const status = S.el("div", "tune-side-status");
    side.appendChild(status);

    const ctx = state.autotune;
    const tunable = tune.mode !== "manual" && !!ctx;
    const startBtn = Corvus.ui.button({
      variant: "primary", icon: tunable ? "play" : "ban",
      label: tunable ? "Start autotune" : "No autotune",
      onClick: () => runAutotune(state, true),
    });
    const stopBtn = Corvus.ui.button({
      variant: "danger", icon: "octagon-x", label: "Stop autotune",
      onClick: () => runAutotune(state, false),
    });
    stopBtn.hidden = true;
    const row = Corvus.ui.actions([startBtn, stopBtn]);
    row.classList.add("tune-autotune-actions");
    side.appendChild(row);
    side.appendChild(S.el("div", "tune-side-note", tune.text || ""));

    if (tunable) {
      const entry = { group, tune, status, startBtn, stopBtn };
      ctx.sides.push(entry);
      // Gated on the opposite condition to every gain: in flight, not on the ground.
      registerControl(state, startBtn, ctx.paint);
    } else {
      startBtn.disabled = true;
      status.textContent = "Set by hand";
    }
    return side;
  }

  /**
   * Which axes the next tune covers, next to the button that starts it.
   *
   * A stack that takes a selection gets its bitmask as an ordinary field,
   * written like any other gain (so only while disarmed). One that does not,
   * PX4's multicopter autotune, still gets the row, ticked and greyed with the
   * reason: an operator looking for QGroundControl's per axis checkboxes
   * learns why they are not there rather than that they are missing.
   */
  function buildAutotuneAxes(state, spec) {
    if (!spec) return null;
    if (spec.selectable && spec.field) {
      const grid = S.paramFieldGrid(state, [spec.field], {
        prefix: "tune",
        onApplied: () => paintSoon(state),
      });
      grid.classList.add("tune-axes");
      return grid;
    }
    const grid = S.el("div", "pform-grid tune-grid tune-axes tune-axes-fixed");
    const row = S.el("div", "pform-field tune-field");
    row.appendChild(S.el("span", "pform-field-label tune-field-label",
      spec.label || "Axes to tune"));
    const cell = S.el("div", "pform-field-control tune-field-control");
    const bits = S.el("div", "pform-bits tune-bits");
    (spec.labels || []).forEach((label) => {
      const item = S.el("label", "pform-bit tune-bit");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = true;
      box.disabled = true;
      box.setAttribute("aria-label", `${label} (always tuned)`);
      item.appendChild(box);
      item.appendChild(S.el("span", null, label));
      bits.appendChild(item);
    });
    cell.appendChild(bits);
    row.appendChild(cell);
    if (spec.reason) {
      row.appendChild(S.el("span", "field-hint pform-field-hint tune-field-hint", spec.reason));
    }
    grid.appendChild(row);
    return grid;
  }

  /** True when the axis selection names none of the axes it offers, which
   *  would start a tune that has nothing to tune. */
  function noAxisSelected(field) {
    if (!field) return false;
    const value = Number(field.value) || 0;
    const offered = (field.bits || []).reduce((mask, b) => mask | (1 << b.bit), 0);
    return offered !== 0 && (value & offered) === 0;
  }

  /** "0:42", "12:05". */
  function clock(seconds) {
    const s = Math.max(0, Math.floor(seconds));
    return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  }

  /** The reboot the autotune is waiting for, or "" when none is. */
  function rebootReason(state, ctx) {
    if (state.restartPending) return REBOOT_TEXT_PENDING;
    if (ctx.group.enabled === false && ctx.group.enable_param) return REBOOT_TEXT_OFF;
    return "";
  }

  /** Why the tune cannot start right now, or "" when it can. The gate names the
   *  missing precondition: "cannot autotune" would tell an operator standing in
   *  a field nothing they can act on. */
  function gateText(state, ctx, noAxis) {
    if (!state.connected) return "No link to the vehicle. Connect before autotuning.";
    if (noAxis) {
      // Before the flight checks: the boxes only take a write on the ground,
      // so "take off first" would send the operator up to find this out.
      return state.armed
        ? "No axis is selected. Land and disarm to tick at least one."
        : "No axis is selected. Tick at least one above before taking off.";
    }
    if (!state.armed) return "The autotune runs in flight. Arm the vehicle and take off first.";
    if (state.landed === 1) {
      return "The vehicle is still on the ground. Hold a stable hover, then start the tune.";
    }
    return "";
  }

  function paintAutotune(state, ctx) {
    const phase = String(state.snapshot.autotune_state || "");
    const running = phase === "running";
    const hasProgress = ctx.group.progress !== false;
    const progress = Math.max(0, Math.min(100, Number(state.snapshot.autotune_progress) || 0));
    const flying = isFlying(state);
    const noAxis = noAxisSelected(ctx.axesField);
    const reason = rebootReason(state, ctx);

    if (running && !ctx.runStart) ctx.runStart = Date.now();
    if (!running) ctx.runStart = 0;

    ctx.phase.dataset.phase = phase || "idle";
    ctx.phase.textContent = AUTOTUNE_PHASE_TEXT[phase] || phase;

    // Without a progress figure (ArduPilot narrates in the console) a number
    // would be invented, so the bar says "running" without claiming a share.
    const indeterminate = running && !hasProgress;
    const share = running ? (hasProgress ? progress : 100) : (phase === "done" ? 100 : 0);
    ctx.fill.style.width = share + "%";
    ctx.bar.dataset.phase = phase;
    ctx.bar.classList.toggle("is-indeterminate", indeterminate);
    ctx.bar.setAttribute("aria-valuenow", String(indeterminate ? 0 : share));
    ctx.bar.setAttribute("aria-valuetext", indeterminate ? "Tuning" : share + "%");
    ctx.percent.textContent = running && hasProgress ? progress + "%" : "";
    ctx.elapsed.textContent = running && ctx.runStart
      ? clock((Date.now() - ctx.runStart) / 1000) + " elapsed" : "";

    ctx.reboot.hidden = !reason;
    ctx.rebootText.textContent = reason;
    ctx.rebootBtn.disabled = state.armed || !state.connected;

    const gate = reason ? "" : gateText(state, ctx, noAxis);
    ctx.gate.hidden = !gate || running;
    ctx.gate.textContent = gate;

    let line;
    if (running) line = ctx.group.running_text || DEFAULT_RUNNING_TEXT;
    else if (phase === "done") line = ctx.group.done_text || DEFAULT_DONE_TEXT;
    else if (phase === "failed") line = FAILED_TEXT;
    else line = "No tune is running.";
    ctx.message.textContent = line;
    ctx.message.dataset.phase = phase || "idle";

    const startable = !ctx.busy && flying && !noAxis && !reason;
    ctx.sides.forEach((side) => {
      side.startBtn.hidden = running;
      side.startBtn.disabled = !startable;
      side.stopBtn.hidden = !running;
      side.stopBtn.disabled = ctx.busy || !state.connected;
      let text = "";
      if (running) text = hasProgress ? `Tuning ${progress}%` : "Tuning";
      else if (phase === "done") text = "Complete";
      else if (phase === "failed") text = "Failed";
      side.status.textContent = text;
      side.status.dataset.phase = phase || "idle";
    });
  }

  async function runAutotune(state, enable) {
    const ctx = state.autotune;
    if (!ctx || ctx.busy) return;
    if (enable && (!isFlying(state) || noAxisSelected(ctx.axesField)
        || rebootReason(state, ctx))) return;
    ctx.busy = true;
    ctx.paint();
    try {
      await Corvus.telemetry.postAction("/api/autotune",
        { axis: "all", enabled: enable });
      notify("info", enable ? "Autotune started" : "Autotune stopped");
    } catch (err) {
      notify("critical", (err && err.message)
        || (enable ? "Could not start the autotune" : "Could not stop the autotune"));
    } finally {
      ctx.busy = false;
      ctx.paint();
      // A finished tune writes new gains to the vehicle; the fields would
      // otherwise still show the ones it replaced.
      if (!enable) load(state);
    }
  }

  /* ================================================================== */
  /* Telemetry                                                           */
  /* ================================================================== */

  function onTelemetry(state, s) {
    if (!s || state.destroyed) return;
    const wasRunning = String(state.snapshot.autotune_state || "") === "running";
    state.snapshot = s;
    const wasArmed = state.armed;
    applyVehicle(state, s);

    paintReady(state);
    S.reloadOnLink(state, s, () => load(state));

    // A vehicle that dropped off the link has most likely restarted, which is
    // all the pending reboot was waiting for. A notice that outlived it would
    // keep the start button locked for a module that is already running.
    if (!state.connected) state.restartPending = false;
    if (wasArmed !== state.armed) applyArmed(state, state.armed);
    if (state.autotune) state.autotune.paint();

    // A tune that just finished has written new gains into the vehicle; re-read
    // so the fields show what is actually flying rather than what it replaced.
    const nowState = String(s.autotune_state || "");
    if (wasRunning && nowState && nowState !== "running") {
      notify(nowState === "done" ? "info" : "warning",
        nowState === "done" ? "Autotune complete" : "Autotune did not finish");
      load(state);
    }

    pushSample(state, s);
  }

  /* The two chips report different questions, so they cannot share a verdict:
     the link chip is "is there a vehicle", the flight chip is "is it where the
     autotune needs it". A disarmed vehicle is a perfectly good state for
     editing gains, which is why the flight chip says what it is rather than
     only whether it is good. */
  function paintReady(state) {
    state.linkChip.set(state.connected, state.connected ? "Link up" : "No link");
    state.flightChip.set(isFlying(state), flightLabel(state));
  }

  function flightLabel(state) {
    if (!state.armed) return "Disarmed";
    if (state.landed === 1) return "Armed, on the ground";
    if (state.landed === 2) return "Flying";
    return "Armed";
  }

  return { render };
})();
