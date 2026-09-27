"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.welcome: the first start setup.

  Opens once, on a station that had no config file when Corvus started
  (GET /api/welcome, see corvus/server.py). Its first page has three ways out,
  each one click: set the station up in three short steps (look, units,
  workspace), take the settings from a file exported on another station, or
  skip straight to the map with the defaults.

  Every choice previews live, through the same modules the Settings page
  uses, and nothing is written until Finish: the config is posted once, then
  POST /api/welcome marks the setup done and writes the config file, so the
  next start goes straight to the map. Skip puts back whatever was showing
  when the setup opened and marks it done as well. An import marks it done on
  the backend, and the page reloads from the imported settings.

  app.js waits for this before it schedules the release check, so an update
  notice never lands on top of the setup.
*/
Corvus.welcome = (function () {
  const STEPS = [
    { id: "look", label: "Look" },
    { id: "units", label: "Units" },
    { id: "workspace", label: "Workspace" },
  ];

  const UNIT_ROWS = [
    { q: "length", label: "Altitude and length" },
    { q: "distance", label: "Distance" },
    { q: "speed", label: "Speed" },
    { q: "temperature", label: "Temperature" },
  ];

  let open = false;

  /**
   * What the steps start from: the config where it says something, and what
   * is on screen where it does not. *live* is {theme, units}. Pure.
   */
  function choicesFrom(cfg, live) {
    const c = cfg || {};
    const l = live || {};
    const ui = c.ui || {};
    return {
      theme: Corvus.theme.fromConfig(c) || (Corvus.theme.isKnown(l.theme) ? l.theme : Corvus.theme.DEFAULT),
      units: Corvus.units.fromConfig(c) || Object.assign({}, l.units || Corvus.units.DEFAULTS),
      missionPage: !!ui.mission_page,
      updates: !(c.updates && c.updates.check === false),
    };
  }

  /**
   * The POST /api/config body for *choices*. `ui` and `updates` are merged
   * per key by the backend, so this sets exactly these and leaves the rest,
   * the interface size included: that one is under Settings > Appearance.
   * Pure.
   */
  function configPatch(choices) {
    return {
      theme: { name: choices.theme },
      ui: {
        units: Object.assign({}, choices.units),
        mission_page: !!choices.missionPage,
      },
      updates: { check: !!choices.updates },
    };
  }

  function liveNow() {
    let theme = "";
    try { theme = document.documentElement.getAttribute("data-theme") || ""; } catch (_e) {}
    return { theme, units: Corvus.units.get() };
  }

  /** Put the look back to *c* (theme and units), for Skip. */
  function restore(c) {
    Corvus.theme.setTheme(c.theme);
    Corvus.units.set(c.units);
  }

  function markDone() {
    return Corvus.telemetry.requestJson("/api/welcome", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: "{}",
    });
  }

  function notify(level, message) {
    window.dispatchEvent(new CustomEvent("corvus:notification", { detail: { level, message } }));
  }

  /**
   * Open the setup when this station has not had it yet. Resolves once it is
   * closed, or at once when there is nothing to do or the backend cannot
   * say. Never rejects.
   */
  async function maybeOpen() {
    let state;
    try {
      state = await Corvus.telemetry.requestJson("/api/welcome");
    } catch (_e) {
      return;
    }
    if (!state || !state.pending) return;
    let cfg = {};
    try {
      const res = await Corvus.telemetry.requestJson("/api/config");
      cfg = (res && res.config) || {};
    } catch (_e) { /* the defaults stand in */ }
    try {
      await show(cfg);
    } catch (_e) {
      open = false;
    }
  }

  /** Build and open the setup. Resolves when it closes. */
  function show(cfg) {
    if (open) return Promise.resolve();
    open = true;
    return new Promise((resolve) => {
      const initial = choicesFrom(cfg, liveNow());
      const choices = JSON.parse(JSON.stringify(initial));
      let step = -1;

      const backBtn = Corvus.ui.button({
        variant: "secondary", icon: "arrow-left", label: "Back", onClick: () => go(step - 1),
      });
      const nextBtn = Corvus.ui.button({
        variant: "primary", label: "Next", onClick: () => next(),
      });
      const dialog = Corvus.ui.modal({
        title: "Welcome to Corvus GCS",
        size: "lg",
        body: document.createDocumentFragment(),
        actions: [backBtn, nextBtn],
        dismissable: false,
        onClose: () => { open = false; resolve(); },
      });
      dialog.dialog.classList.add("welcome");
      dialog.dialog.tabIndex = -1;
      const titleEl = dialog.dialog.querySelector(".modal-title");
      const footer = dialog.dialog.querySelector(".modal-actions");
      const msg = Corvus.ui.message();

      function go(n) {
        step = Math.max(-1, Math.min(n, STEPS.length - 1));
        msg.hide();
        Corvus.ui.clear(dialog.body);
        dialog.dialog.classList.toggle("is-start", step < 0);
        if (step < 0) {
          titleEl.textContent = "Welcome to Corvus GCS";
          footer.style.display = "none";
          dialog.body.appendChild(startPage());
        } else {
          titleEl.textContent = "Set up Corvus GCS";
          footer.style.display = "";
          dialog.body.appendChild(stepper());
          dialog.body.appendChild(pages[STEPS[step].id]());
          dialog.body.appendChild(msg.el);
          const last = step === STEPS.length - 1;
          const label = nextBtn.querySelector("span");
          if (label) label.textContent = last ? "Finish" : "Next";
        }
        Corvus.ui.refreshIcons();
        dialog.body.scrollTop = 0;
        // The first page takes focus on the dialog itself, so it opens with no
        // ring on a card; Tab still walks the three cards in order.
        const first = step < 0 ? dialog.dialog : dialog.body.querySelector(
          ".option-card.selected, .ui-toggle, .ui-select-trigger, input");
        if (first && typeof first.focus === "function") first.focus({ preventScroll: true });
      }

      function next() {
        if (step < STEPS.length - 1) go(step + 1);
        else finish();
      }

      function close() {
        dialog.close();
      }

      // ---- first page: three ways out ---------------------------------------

      function startPage() {
        const wrap = document.createElement("div");
        wrap.className = "welcome-start";

        const hero = document.createElement("div");
        hero.className = "welcome-hero";
        const mark = document.createElement("span");
        mark.className = "welcome-mark";
        mark.setAttribute("aria-hidden", "true");
        hero.appendChild(mark);
        const heading = document.createElement("h2");
        heading.className = "welcome-heading";
        heading.textContent = "Welcome to Corvus GCS";
        hero.appendChild(heading);
        const lede = document.createElement("p");
        lede.className = "welcome-lede";
        lede.textContent = "This station has not been set up yet. Make it yours in " +
          "about a minute, or bring the settings over from another station.";
        hero.appendChild(lede);
        wrap.appendChild(hero);

        const primary = Corvus.ui.tile({
          className: "welcome-card welcome-card-primary",
          icon: "sliders-horizontal",
          title: "Set up this station",
          desc: "Colour theme, units and workspace.",
          onClick: () => go(0),
        });
        const arrow = document.createElement("span");
        arrow.className = "welcome-card-go";
        arrow.setAttribute("aria-hidden", "true");
        arrow.appendChild(Corvus.ui.icon("arrow-right", "auto"));
        primary.appendChild(arrow);
        wrap.appendChild(primary);

        const more = document.createElement("div");
        more.className = "welcome-more";
        if (Corvus.settingsTransfer) {
          more.appendChild(Corvus.ui.tile({
            className: "welcome-card",
            icon: "upload",
            title: "Import settings",
            desc: "From another station.",
            chevron: true,
            onClick: () => Corvus.settingsTransfer.pickFile({ onImported: close }),
          }));
        }
        more.appendChild(Corvus.ui.tile({
          className: "welcome-card",
          icon: "skip-forward",
          title: "Skip for now",
          desc: "Start with the defaults.",
          chevron: true,
          onClick: () => skip(),
        }));
        wrap.appendChild(more);
        return wrap;
      }

      function stepper() {
        const list = document.createElement("ol");
        list.className = "welcome-steps";
        list.setAttribute("aria-label", `Step ${step + 1} of ${STEPS.length}`);
        STEPS.forEach((s, i) => {
          const item = document.createElement("li");
          item.className = "welcome-step" + (i === step ? " active" : "") + (i < step ? " done" : "");
          if (i === step) item.setAttribute("aria-current", "step");
          const num = document.createElement("span");
          num.className = "welcome-step-num";
          num.textContent = String(i + 1);
          item.appendChild(num);
          const text = document.createElement("span");
          text.textContent = s.label;
          item.appendChild(text);
          list.appendChild(item);
        });
        return list;
      }

      // ---- the three steps --------------------------------------------------

      const pages = {
        look() {
          const wrap = document.createDocumentFragment();
          const themes = Corvus.ui.optionCards({
            ariaLabel: "Colour theme",
            columns: 3,
            value: choices.theme,
            options: Corvus.theme.THEMES,
            onChange: (id) => { choices.theme = Corvus.theme.setTheme(id); },
          });
          wrap.appendChild(Corvus.ui.field({
            label: "Colour theme",
            control: themes.el,
            hint: "The interface size (text, icons and panels) can be adjusted " +
                  "later in Settings under Appearance.",
          }));
          return wrap;
        },

        units() {
          const U = Corvus.units;
          const wrap = document.createDocumentFragment();
          const selects = {};

          function sync() {
            choices.units = U.get();
            presets.setValue(U.systemOf(choices.units) || "");
            Object.keys(selects).forEach((q) => {
              const sel = selects[q];
              sel.value = choices.units[q];
              if (sel.corvusSelect) sel.corvusSelect.refresh();
            });
          }

          const presets = Corvus.ui.optionCards({
            ariaLabel: "Unit system",
            columns: 3,
            value: U.systemOf(choices.units) || "",
            options: U.SYSTEMS.map((s) => ({ id: s.id, label: s.label, desc: s.desc })),
            onChange: (id) => { U.setSystem(id); sync(); },
          });
          wrap.appendChild(Corvus.ui.field({
            label: "Unit system",
            control: presets.el,
            hint: "Sets the four below at once. You can still change any one of them.",
          }));

          const grid = document.createElement("div");
          grid.className = "welcome-units";
          UNIT_ROWS.forEach((r) => {
            const sel = Corvus.ui.select({
              id: "welcomeUnits_" + r.q,
              ariaLabel: r.label,
              value: choices.units[r.q],
              options: U.QUANTITIES[r.q].map((u) => ({ value: u.id, label: u.label })),
              onChange: (v) => {
                const patch = {};
                patch[r.q] = v;
                U.set(patch);
                sync();
              },
            });
            selects[r.q] = sel;
            grid.appendChild(Corvus.ui.field({ label: r.label, control: sel }));
          });
          wrap.appendChild(grid);

          const note = document.createElement("div");
          note.className = "field-hint";
          note.textContent = "Display only. Parameters, mission files and everything " +
            "sent to the aircraft stay in the units the autopilot uses.";
          wrap.appendChild(note);
          return wrap;
        },

        workspace() {
          const wrap = document.createDocumentFragment();
          const mission = Corvus.ui.toggle({
            id: "welcomeMissionPage",
            value: choices.missionPage,
            ariaLabel: "Mission planner",
            onChange: (on) => { choices.missionPage = on; },
          });
          wrap.appendChild(Corvus.ui.field({
            label: "Mission planner",
            control: mission.el,
            className: "field-switch",
            hint: "Adds MISSION to the left rail: plan takeoff, waypoints and landing " +
                  "on a map, then upload. Leave it off for a station flown by hand.",
          }));
          const updates = Corvus.ui.toggle({
            id: "welcomeUpdates",
            value: choices.updates,
            ariaLabel: "Check for updates",
            onChange: (on) => { choices.updates = on; },
          });
          wrap.appendChild(Corvus.ui.field({
            label: "Check for updates",
            control: updates.el,
            className: "field-switch",
            hint: "At each start, asks GitHub whether a newer release exists. Nothing " +
                  "is downloaded and nothing about this machine is sent. Without " +
                  "internet it stays silent.",
          }));
          const outro = document.createElement("div");
          outro.className = "page-card-desc welcome-outro";
          outro.textContent = "Connections, the map service, the virtual joystick and " +
            "everything on these pages are under Settings, whenever you need them.";
          wrap.appendChild(outro);
          return wrap;
        },
      };

      // ---- the ways out ----------------------------------------------------

      async function finish() {
        msg.hide();
        Corvus.ui.setBusy(nextBtn, true);
        backBtn.disabled = true;
        try {
          await Corvus.telemetry.requestJson("/api/config", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(configPatch(choices)),
          });
          await markDone();
        } catch (err) {
          msg.show((err && err.message) || "The settings could not be saved.", "err");
          Corvus.ui.setBusy(nextBtn, false);
          backBtn.disabled = false;
          return;
        }
        if (Corvus.sidenav && Corvus.sidenav.setMissionEnabled) {
          Corvus.sidenav.setMissionEnabled(choices.missionPage);
        }
        close();
        notify("info", "Corvus GCS is set up. Everything can be changed under Settings.");
      }

      function skip() {
        restore(initial);
        // Best effort: when the backend cannot take it, the setup simply
        // offers itself again on the next start.
        markDone().catch(() => {});
        close();
      }

      dialog.open();
      go(-1);
    });
  }

  return {
    maybeOpen,
    show,
    isOpen: () => open,
    STEPS,
    // Exported for the test suite.
    choicesFrom,
    configPatch,
  };
})();
