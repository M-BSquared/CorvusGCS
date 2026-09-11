"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupTuning — the PID Tuning sub-page of the Setup page.
 *
 * The control cascade, one tab per loop, innermost first — the way PX4 is
 * built and the way QGroundControl presents it:
 *
 *   Rate Controller       the loop that decides how the aircraft feels
 *   Attitude Controller   angle error to rate setpoint
 *   Velocity Controller   the loop behind a drifting hover (multicopter)
 *   Position Controller   position error to velocity setpoint (multicopter)
 *   Autotune              PX4's own tune: preconditions, settings, and the run
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

  const AUTOTUNE_PHASE_TEXT = {
    "": "Idle",
    running: "Tuning",
    done: "Complete",
    failed: "Failed",
  };

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page tune-page");
    page.appendChild(S.backButton(navigateBack));
    page.appendChild(S.pageHeader("PID Tuning",
      "The control cascade, one loop at a time — by hand or by autotune"));

    // Readiness strip: what each action needs, stated before it is attempted.
    // Autotune and a gain edit want opposite states, which is exactly why both
    // are shown rather than one "ready" light.
    const ready = S.el("div", "calib-ready tune-ready");
    const linkChip = readyChip("link", "Link");
    const flightChip = readyChip("flight", "Disarmed");
    ready.appendChild(linkChip.el);
    ready.appendChild(flightChip.el);
    page.appendChild(ready);

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
    banner.textContent =
      "Gains are read-only while armed — land and disarm to change them. "
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
      groups: [], activeId: null,
      charts: null, autotune: null,
      snapshot: {},
      linkChip, flightChip,
      reduced: S.reducedMotion(),
      startMs: Date.now(),
    };

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

    reloadBtn.addEventListener("click", () => load(state));
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

  function load(state) {
    if (state.loading) return Promise.resolve();
    state.loading = true;
    state.reloadBtn.disabled = true;
    S.setActionsStatus(state.actionsStatus, "pending", "Reading tuning parameters…");
    return Corvus.telemetry.requestJson("/api/tuning").then((doc) => {
      if (state.destroyed) return;
      renderGroups(state, doc || {});
      if (!doc || !doc.connected) {
        S.setActionsStatus(state.actionsStatus, "err",
          (doc && doc.error) || "No tuning parameters received");
      } else {
        S.setActionsStatus(state.actionsStatus, "ok", `${doc.received} parameters read`);
      }
    }).catch((err) => {
      if (state.destroyed) return;
      renderGroups(state, {});
      S.setActionsStatus(state.actionsStatus, "err",
        (err && err.message) || "Could not read the tuning parameters");
    }).finally(() => {
      if (state.destroyed) return;
      state.loading = false;
      state.reloadBtn.disabled = false;
    });
  }

  function renderGroups(state, doc) {
    purgeCharts(state);
    state.groups = Array.isArray(doc.groups) ? doc.groups : [];
    state.controls = [];
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

    // Keep the operator on the tab they were reading across a reload; a reload
    // that silently jumps back to Roll loses the axis they were working on.
    if (!state.groups.some((g) => g.id === state.activeId)) {
      state.activeId = state.groups[0].id;
    }

    state.groups.forEach((group) => {
      const tab = document.createElement("button");
      tab.type = "button";
      tab.className = "tune-tab";
      tab.dataset.group = group.id;
      tab.setAttribute("role", "tab");
      tab.textContent = group.title || group.id;
      tab.addEventListener("click", () => selectGroup(state, group.id));
      state.tabs.appendChild(tab);
    });

    selectGroup(state, state.activeId);
  }

  function selectGroup(state, groupId) {
    const group = state.groups.find((g) => g.id === groupId);
    if (!group) return;
    state.activeId = groupId;
    purgeCharts(state);
    state.controls = [];
    state.autotune = null;
    state.host.innerHTML = "";

    Array.prototype.forEach.call(state.tabs.children, (tab) => {
      const active = tab.dataset.group === groupId;
      tab.classList.toggle("active", active);
      tab.setAttribute("aria-selected", active ? "true" : "false");
    });

    if (group.hint) state.host.appendChild(S.el("div", "tune-hint", group.hint));
    if (group.kind === "autotune") state.host.appendChild(buildAutotune(state, group));
    if (group.charts && group.charts.length) {
      state.host.appendChild(buildCharts(state, group));
    }

    (group.sections || []).forEach((section) => {
      const card = S.el("div", "page-card tune-card");
      card.dataset.section = section.id || "";
      card.appendChild(S.sectionTitle(section.title || ""));
      card.appendChild(S.paramFieldGrid(state, section.fields || [], { prefix: "tune" }));
      state.host.appendChild(card);
    });

    applyArmed(state, state.armed);
    if (state.autotune) state.autotune.paint();
    S.refreshIcons();
    // Plotly sizes to the element, which has only just been laid out.
    redrawAll(state);
  }

  /* ================================================================== */
  /* Charts: setpoint against response                                   */
  /* ================================================================== */

  function buildCharts(state, group) {
    const wrap = S.el("div", "tune-charts");
    const config = S.plotlyConfig(state.reduced);
    const charts = (group.charts || []).map((spec) => {
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
      return {
        spec, chart, live, dead: false,
        buf: { t: [], sp: [], actual: [] },
      };
    });

    state.charts = { charts, config, lastRedraw: 0, unsubTheme: null };
    // Plotly holds the colors it was handed, so a theme switch has to force a
    // redraw. Released in purgeCharts along with the graphs themselves.
    state.charts.unsubTheme = Corvus.ui.onThemeChange(() => redrawAll(state));
    return wrap;
  }

  function traces(state, entry) {
    const palette = Corvus.ui.chartColors();
    const out = [{
      x: entry.buf.t, y: entry.buf.actual, mode: "lines", name: "response",
      line: { color: palette.nav || "#4CC9FF", width: 1.6 },
    }];
    // The setpoint is drawn only when the vehicle is actually sending one. A
    // dashed line pinned at zero would read as "the controller is commanding
    // nothing", which is a different and much more alarming claim than "this
    // firmware is not streaming its setpoint".
    if (entry.spec.setpoint && state.snapshot.setpoints_live) {
      out.push({
        x: entry.buf.t, y: entry.buf.sp, mode: "lines", name: "setpoint",
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
    if (typeof window === "undefined" || typeof window.Plotly === "undefined") return;
    if (!state.charts || entry.dead) return;
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
    // Marked before the purge, not after: a redraw queued for this frame must
    // not reach a graph that is about to be torn out from under it.
    state.charts.charts.forEach((entry) => { entry.dead = true; });
    if (typeof window !== "undefined" && window.Plotly) {
      state.charts.charts.forEach((entry) => {
        try { window.Plotly.purge(entry.chart); } catch (_e) {}
      });
    }
    state.charts.charts.forEach((entry) => { entry.buf = { t: [], sp: [], actual: [] }; });
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

  function buildAutotune(state, group) {
    const card = S.el("div", "page-card tune-autotune");

    const head = S.el("div", "tune-autotune-head");
    head.appendChild(S.el("span", "tune-autotune-label", group.label || "Autotune"));
    const phase = S.el("span", "calib-phase tune-autotune-phase");
    phase.dataset.phase = "";
    head.appendChild(phase);
    card.appendChild(head);

    // The preconditions, before the aircraft leaves the ground rather than
    // after PX4 has refused with a result code that names none of them.
    const steps = S.el("ol", "tune-steps");
    (group.steps || []).forEach((line) => {
      const item = document.createElement("li");
      item.className = "tune-step";
      item.textContent = line;
      steps.appendChild(item);
    });
    card.appendChild(steps);

    const moduleWarning = S.el("div", "params-banner tune-module-warning");
    moduleWarning.hidden = group.enabled !== false;
    moduleWarning.textContent =
      "The autotune module is switched off on this autopilot. Turn it on below, "
      + "then reboot the autopilot before starting a tune.";
    card.appendChild(moduleWarning);

    const bar = S.el("div", "calib-progress tune-progress");
    const fill = S.el("div", "calib-progress-fill");
    bar.appendChild(fill);
    bar.hidden = true;
    card.appendChild(bar);

    const gate = S.el("div", "params-banner setup-armed-banner tune-gate");
    gate.hidden = true;
    card.appendChild(gate);

    const startBtn = Corvus.ui.button({
      variant: "primary", icon: "play", label: "Start autotune",
      onClick: () => runAutotune(state, true),
    });
    const stopBtn = Corvus.ui.button({
      variant: "danger", icon: "octagon-x", label: "Stop autotune",
      onClick: () => runAutotune(state, false),
    });
    const row = Corvus.ui.actions([startBtn, stopBtn]);
    row.classList.add("tune-autotune-actions");
    card.appendChild(row);

    const ctx = { phase, bar, fill, gate, startBtn, stopBtn, moduleWarning, busy: false };
    ctx.paint = () => paintAutotune(state, ctx);
    state.autotune = ctx;

    // One registry entry for the pair: they are gated together, on the
    // opposite condition to every other control on the page.
    registerControl(state, startBtn, ctx.paint);
    return card;
  }

  function paintAutotune(state, ctx) {
    const phase = String(state.snapshot.autotune_state || "");
    const running = phase === "running";
    const progress = Number(state.snapshot.autotune_progress) || 0;
    const flying = isFlying(state);

    ctx.phase.dataset.phase = phase || "idle";
    ctx.phase.textContent = AUTOTUNE_PHASE_TEXT[phase] || phase;

    ctx.bar.hidden = !running && phase !== "done";
    ctx.fill.style.width = Math.max(0, Math.min(100, running ? progress : 100)) + "%";
    ctx.bar.dataset.phase = phase;

    ctx.startBtn.hidden = running;
    ctx.startBtn.disabled = ctx.busy || !flying;
    ctx.stopBtn.hidden = !running;
    ctx.stopBtn.disabled = ctx.busy || !state.connected;

    // The gate names the missing precondition. "Cannot autotune" would tell an
    // operator standing in a field nothing they can act on.
    let text = "";
    if (!state.connected) {
      text = "No link to the vehicle — connect before autotuning.";
    } else if (!state.armed) {
      text = "The autotune runs in flight. Arm the vehicle and take off first.";
    } else if (state.landed === 1) {
      text = "The vehicle is still on the ground. Hold a stable hover, then start the tune.";
    }
    ctx.gate.hidden = !text || running;
    ctx.gate.textContent = text;
  }

  async function runAutotune(state, enable) {
    const ctx = state.autotune;
    if (!ctx || ctx.busy) return;
    if (enable && !isFlying(state)) return;
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
      // A finished tune writes new gains to the vehicle; the fields on the
      // other tabs would otherwise still show the ones it replaced.
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
