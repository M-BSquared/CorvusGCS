"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.theme — the predefined color themes.

  A theme is a complete palette defined in css/themes.css under a
  `[data-theme="<id>"]` selector; selecting one is a single attribute write on
  <html>. There is deliberately no runtime color math here: the CSS is the
  source of truth for what a theme looks like, and this module only decides
  WHICH one is active. That is what replaced the v1 accent picker, where the
  only themeable value was --accent and every other color stayed dark.

  THEMES mirrors the blocks in css/themes.css and must stay in step with them
  (the ids are the contract). `swatch` is the preview shown in the picker:
  accent, panel surface, page background — the three colors that actually tell
  the themes apart at a glance.

  "light-orange" is the default and the one theme with no block of its own —
  it is what bare `:root` carries in css/themes.css, so it is also what an
  unknown id degrades to. Changing DEFAULT changes only what a fresh install
  gets: an operator who has already picked a theme has it in localStorage and
  in the backend config, and both outrank this.

  Persistence is two-layered on purpose: localStorage so the choice can be
  applied before first paint (see the inline script in index.html) and the
  backend config so it survives a cache clear and follows the operator's
  profile. localStorage is written first and never blocks on the network.
*/
Corvus.theme = (function () {
  const KEY = "corvus.theme";
  const DEFAULT = "light-orange";

  const THEMES = [
    { id: "light-orange", label: "Light Orange", desc: "Default",       swatch: ["#C2540A", "#FFFFFF", "#F4F6F8"] },
    { id: "light",        label: "Light",        desc: "White & black", swatch: ["#1B1F26", "#F1F3F6", "#FFFFFF"] },
    { id: "green",        label: "Green",        desc: "Dark",          swatch: ["#3DA876", "#171D25", "#0B0E12"] },
    { id: "blue",         label: "Blue",         desc: "Cool",          swatch: ["#3B9EFF", "#171D25", "#0B0E12"] },
    { id: "pink",         label: "Pink",         desc: "Magenta",       swatch: ["#F0509B", "#171D25", "#0B0E12"] },
    { id: "orange",       label: "Orange",       desc: "Amber",         swatch: ["#F58A2B", "#171D25", "#0B0E12"] },
  ];

  const IDS = THEMES.map((t) => t.id);

  /** True when *id* names a theme css/themes.css actually defines. */
  function isKnown(id) { return IDS.indexOf(id) !== -1; }

  /**
   * Apply *id* to the document and cache it. An unknown id (a config from a
   * newer build, a corrupted storage value) falls back to the default rather
   * than leaving the app on a half-applied palette. Returns the id applied.
   */
  function setTheme(id) {
    const v = isKnown(id) ? id : DEFAULT;
    try { document.documentElement.setAttribute("data-theme", v); } catch (_e) {}
    try { localStorage.setItem(KEY, v); } catch (_e) {}
    // Everything styled in CSS restyles itself the moment the attribute
    // changes. Plotly does not: it draws into its own surface from color
    // strings resolved at build time, so the charts have to be told. This is
    // the only reason a theme change is an event at all.
    try {
      window.dispatchEvent(new CustomEvent("corvus:themechange", { detail: { theme: v } }));
    } catch (_e) {}
    return v;
  }

  /** Apply the locally cached theme (called before the config fetch lands). */
  function applySaved() {
    let v = DEFAULT;
    try { v = localStorage.getItem(KEY) || DEFAULT; } catch (_e) {}
    return setTheme(v);
  }

  /**
   * Resolve the theme a backend config asks for. `theme.name` is what the
   * settings page writes today; `theme.accent` is the legacy v1 hex from the
   * old accent picker, kept readable so an existing ~/.corvus/config.json is
   * not a hard error — its nearest predefined theme is used when it matches
   * one, and the default otherwise. Returns null when the config names
   * nothing, so the caller can leave the cached theme alone.
   */
  function fromConfig(cfg) {
    const theme = cfg && cfg.theme;
    if (!theme) return null;
    if (isKnown(theme.name)) return theme.name;
    if (typeof theme.accent === "string") {
      const hex = theme.accent.trim().toLowerCase();
      const match = THEMES.find((t) => t.swatch[0].toLowerCase() === hex);
      return match ? match.id : DEFAULT;
    }
    return null;
  }

  return { setTheme, applySaved, isKnown, fromConfig, THEMES, IDS, DEFAULT };
})();

/*
  Corvus.scale — the interface size.

  One number multiplied over every used length below <body> via the --ui-scale
  token and the `zoom` rule in css/main.css. It is a token write and nothing
  else: no component knows about it, exactly as no component knows which theme
  is active, which is why it reaches text, icons (whose px sizes JS writes
  inline, out of reach of any font-size lever), bar heights and hairlines
  alike.

  STEPS is a short ordered list rather than a continuous range because the
  choice is coarse — an operator picks "a bit bigger", not 113%. The values are
  the contract with the settings slider and with the backend config
  (`ui.scale`); the labels are only what the step indicators read.

  Persistence mirrors Corvus.theme: localStorage first, so the size is applied
  before first paint and the app never resizes itself in front of the operator,
  and the backend config second, so it survives a cache clear.

  Two things do have to be told when it changes, and they are why this is an
  event: geometry read back out of the DOM in JS is in scaled pixels while
  style writes are in unscaled ones (hud-panel.js), and MapLibre sizes its
  drawing buffer from the container's unscaled size, which would leave the map
  soft when the UI grows and needlessly oversampled when it shrinks
  (map.js). A plain `resize` is dispatched alongside so anything that already
  reacts to a viewport change — the right panel's auto-collapse, MapLibre's
  own observer, Plotly — needs no new listener.
*/
Corvus.scale = (function () {
  const KEY = "corvus.scale";
  const DEFAULT = 1;

  const STEPS = [
    { value: 0.8,  label: "80%" },
    { value: 0.9,  label: "90%" },
    { value: 1,    label: "100%" },
    { value: 1.1,  label: "110%" },
    { value: 1.25, label: "125%" },
    { value: 1.5,  label: "150%" },
  ];

  const MIN = STEPS[0].value;
  const MAX = STEPS[STEPS.length - 1].value;

  /** Clamp *v* into the offered range, or the default when it is not a
   *  number at all. Never snaps to a step: a config written by a build with
   *  a different STEPS list stays honoured, and the slider shows the nearest
   *  step for it. */
  function normalize(v) {
    const n = Number(v);
    if (!isFinite(n) || n <= 0) return DEFAULT;
    return Math.min(Math.max(n, MIN), MAX);
  }

  /** Apply *v* to the document and cache it. Returns the value applied. */
  function setScale(v) {
    const n = normalize(v);
    try { document.documentElement.style.setProperty("--ui-scale", String(n)); } catch (_e) {}
    try { localStorage.setItem(KEY, String(n)); } catch (_e) {}
    try {
      window.dispatchEvent(new CustomEvent("corvus:scalechange", { detail: { scale: n } }));
      window.dispatchEvent(new Event("resize"));
    } catch (_e) {}
    return n;
  }

  /** Apply the locally cached scale (called before the config fetch lands). */
  function applySaved() {
    let v = DEFAULT;
    try { v = localStorage.getItem(KEY) || DEFAULT; } catch (_e) {}
    return setScale(v);
  }

  /** The scale the current document is at, whatever set it. */
  function get() {
    try {
      return normalize(getComputedStyle(document.documentElement).getPropertyValue("--ui-scale"));
    } catch (_e) {
      return DEFAULT;
    }
  }

  /**
   * Resolve the scale a backend config asks for, or null when it names none
   * (so the caller leaves the cached value alone). A non-numeric or
   * out-of-range value is clamped rather than rejected — a config must never
   * be able to leave the UI at an unusable size, and must never fail to load.
   */
  function fromConfig(cfg) {
    const ui = cfg && cfg.ui;
    if (!ui || ui.scale == null) return null;
    const n = Number(ui.scale);
    if (!isFinite(n) || n <= 0) return null;
    return normalize(n);
  }

  return { setScale, applySaved, get, normalize, fromConfig, STEPS, DEFAULT, MIN, MAX };
})();

Corvus.sidenav = (function () {
  let leftNav, mapView, pageView;
  // Teardown for the Analysis page's log downloader (poll timer + telemetry
  // subscription). Paired 1:1 with every render of that page.
  let analysisDestroy = null;
  let activeNav = "home";

  // Monotonic navigation generation. Bumped on every switchTo so async work
  // started by a previous page (e.g. Settings' /api/config + /api/version
  // fetches) can capture the generation at issue time and skip mutating
  // pageView after the operator has navigated to a different page.
  let navGeneration = 0;

  const NAV = [
    { id: "setup", label: "SETUP", icon: "sliders-horizontal" },
    { id: "analysis", label: "ANALYSIS", icon: "chart-column" },
    { id: "logs", label: "LOGS", icon: "file-text" },
  ];

  function renderLeftNav() {
    Corvus.ui.clear(leftNav);

    // One list drives the whole rail: HOME and SET differ from the middle
    // entries only by the separator/spacer around them, not by how the
    // button itself is built, so they share ui.navItem like everything else.
    const items = [
      { id: "home", label: "HOME", icon: "house", after: "divider" },
      ...NAV,
      { id: "settings", label: "SET", icon: "settings", title: "Settings", before: "spacer" },
    ];

    items.forEach((n) => {
      if (n.before) leftNav.appendChild(railFiller(n.before));
      leftNav.appendChild(Corvus.ui.navItem({
        id: n.id,
        icon: n.icon,
        label: n.label,
        title: n.title,
        active: n.id === activeNav,
        onClick: () => switchTo(n.id),
      }));
      if (n.after) leftNav.appendChild(railFiller(n.after));
    });
    Corvus.ui.refreshIcons();
  }

  /** Rail filler: "divider" is the hairline under HOME, "spacer" the flexible
   *  gap that pushes SET to the bottom of the rail. */
  function railFiller(kind) {
    const d = document.createElement("div");
    d.className = kind === "spacer" ? "nav-spacer" : "nav-divider";
    return d;
  }

  function switchTo(navId) {
    const prev = activeNav;
    activeNav = navId;
    // Bump on every nav change so in-flight async appends from the previous
    // page can detect they have been superseded and skip writing into the
    // now-different pageView (see renderSettingsPage / renderAboutSection).
    navGeneration++;
    // Tear down the Setup page on any left-nav exit from it — BEFORE pageView
    // is repurposed — so its sub-page SSE / params poll / firmware upload /
    // Plotly graphs / telemetry subscriptions are released. Previously this
    // only happened on Back or re-entry, so leaving Setup via the left-nav
    // leaked those resources. Idempotent: safe if Back already tore down.
    if (prev === "setup" && navId !== "setup" &&
        Corvus.setup && typeof Corvus.setup.teardown === "function") {
      Corvus.setup.teardown();
    }
    // Same for the Analysis page's log downloader, on every left-nav exit.
    if (typeof analysisDestroy === "function") {
      try { analysisDestroy(); } catch (err) { console.error("analysis teardown failed:", err); }
      analysisDestroy = null;
    }
    leftNav.querySelectorAll(".nav-item").forEach((x) =>
      x.classList.toggle("active", x.dataset.nav === navId));

    if (navId === "home") {
      mapView.hidden = false;
      pageView.hidden = true;
    } else {
      mapView.hidden = true;
      pageView.hidden = false;
      renderPage(navId);
    }
  }

  function renderPage(navId) {
    const pages = {
      setup: renderSetupPage,
      logs: renderLogsPage,
      analysis: renderAnalysisPage,
      settings: renderSettingsPage,
    };
    const fn = pages[navId] || renderPlaceholder;
    Corvus.ui.clear(pageView);
    fn(pageView);
    Corvus.ui.refreshIcons();
  }

  // Layout primitives come from the shared component layer — these are local
  // names for them, not local implementations. pageHeader in particular used
  // to build its markup with innerHTML and interpolated arguments; ui.pageHeader
  // sets textContent instead, so a title sourced from the backend can no
  // longer inject markup.
  const pageHeader = Corvus.ui.pageHeader;
  const row = Corvus.ui.row;

  function renderSetupPage(container) {
    // The whole Setup page (tile grid + sub-pages) is owned by Corvus.setup.
    // It tears the active sub-page down on every entry so a left-nav re-entry
    // never leaks telemetry subscriptions, Plotly graphs, or the params SSE.
    if (Corvus.setup && typeof Corvus.setup.render === "function") {
      Corvus.setup.render(container);
    } else {
      container.appendChild(pageHeader("Setup", "Vehicle configuration and calibration"));
    }
  }

  function renderLogsPage(container) {
    container.appendChild(pageHeader("Logs", "Flight logs and MAVLink message history"));
    const card = Corvus.ui.card({});
    const lines = document.querySelectorAll("#consoleOutput .con-line");
    const recent = Array.from(lines).slice(-30).map((l) => {
      const t = l.querySelector(".con-time")?.textContent || "";
      const m = l.querySelector(".con-msg")?.textContent || "";
      return `${t}  ${m}`;
    });
    if (recent.length === 0) {
      card.appendChild(Corvus.ui.empty("No log entries yet."));
    } else {
      const pre = document.createElement("pre");
      pre.style.fontFamily = "var(--mono)";
      pre.style.fontSize = "11px";
      pre.style.color = "var(--text-2)";
      pre.style.lineHeight = "1.5";
      pre.style.whiteSpace = "pre-wrap";
      pre.textContent = recent.join("\n");
      card.appendChild(pre);
    }
    container.appendChild(Corvus.ui.section({ title: "Recent Console Output", body: card }));
  }

  function renderAnalysisPage(container) {
    container.appendChild(pageHeader("Analysis",
      "Flight logs and telemetry analysis"));
    // The whole page body (live telemetry, the download folder, the two log
    // tiles and their sub-pages) is owned by Corvus.analysis, the same way
    // Corvus.setup owns the Setup page. It holds a poll timer and two
    // telemetry subscriptions, so it is torn down on every left-nav exit.
    if (Corvus.analysis && typeof Corvus.analysis.render === "function") {
      analysisDestroy = Corvus.analysis.render(container);
    }
  }

  // --- Settings page data ---

  // POST a partial config update. Best-effort by default: the change is
  // already applied live, so a persist failure is non-fatal for this session
  // and the settings page has nothing useful to say about it. Pass
  // `{strict: true}` where the caller needs to know — a control that must undo
  // itself when the backend refuses. Reuses the telemetry transport so HTTP
  // errors reject instead of resolving with a bogus body.
  function postConfig(body, opts) {
    const request = Corvus.telemetry.requestJson("/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    return (opts && opts.strict) ? request : request.catch(() => {});
  }

  // Activate the SSH tab in the right panel (used after a settings-page CONNECT
  // so the live terminal the panel rendered becomes visible). Mirrors the
  // tab-switch logic in panel.init() without re-firing its click handler.
  function switchToSSHTab() {
    const tab = document.querySelector('.panel-tabs .tab[data-tab="ssh"]');
    const panel = document.querySelector('.tab-panel[data-panel="ssh"]');
    if (tab) {
      document.querySelectorAll(".panel-tabs .tab").forEach((t) =>
        t.classList.toggle("active", t === tab));
    }
    if (panel) {
      document.querySelectorAll(".tab-panel").forEach((p) =>
        p.classList.toggle("active", p === panel));
    }
  }

  // --- Section A: Appearance (color theme, map service, on-screen controls) ---
  // Operator choices that all change what the app looks like and what it puts
  // on screen, so they share one section. Everything here applies immediately
  // and persists in the background — nothing has a Save button, because there
  // is nothing to get wrong and undoing is one more click.
  function renderAppearanceSection(container, cfg, gen) {
    const body = document.createDocumentFragment();
    body.appendChild(companyLogoCard(cfg));
    body.appendChild(appIconCard(cfg));
    body.appendChild(themeCard(cfg));
    body.appendChild(scaleCard(cfg));
    body.appendChild(mapServiceCard(cfg, gen));
    body.appendChild(controlsCard(cfg));
    container.appendChild(Corvus.ui.section({ title: "Appearance", body }));
  }

  // Which optional input controls the map carries: the stick pair, the arrow
  // keys and the WASD keys, each its own switch. The card exists as its own
  // group because controls that can move the aircraft do not belong under
  // "Color theme".
  //
  // Both switches drive the live map immediately and persist in the
  // background, and the persist is what the toggle awaits: a rejected POST
  // snaps the switch back rather than leaving it claiming a state the config
  // does not have. The joystick module is the single owner of what is on
  // screen — this only tells it.
  function controlsCard(cfg) {
    const card = Corvus.ui.card({ title: "Controls" });
    card.appendChild(controlSwitch({
      id: "settingsVirtualJoystick",
      key: "virtual_joystick",
      label: "Virtual joystick",
      apply: "setEnabled",
      value: !!(cfg.controls && cfg.controls.virtual_joystick),
      hint: "Shows a two-stick pad over the map on the Home tab: left stick " +
            "throttle and yaw, right stick pitch and roll. Drag the pad by " +
            "its grip bar to move it; double-click the bar to send it back.",
    }));
    card.appendChild(controlSwitch({
      id: "settingsArrowKeys",
      key: "arrow_keys",
      label: "Arrow keys",
      apply: "setKeysEnabled",
      value: !!(cfg.controls && cfg.controls.arrow_keys),
      hint: "Adds a four-key pad to the same window and lets the keyboard's " +
            "arrow keys fly the aircraft forward, back, left and right — " +
            "pitch and roll only, at half stick.",
    }));
    card.appendChild(controlSwitch({
      id: "settingsWasdKeys",
      key: "wasd_keys",
      label: "WASD keys",
      apply: "setWasdEnabled",
      value: !!(cfg.controls && cfg.controls.wasd_keys),
      hint: "The other half of the transmitter, in the same window beside the " +
            "arrow keys: W and S are thrust, A and D are yaw, also at half " +
            "stick. Releasing a thrust key returns to the hover detent, not " +
            "to zero. The autopilot acts on any of these controls only when " +
            "the vehicle accepts joystick input (COM_RC_IN_MODE 1 or 3) and " +
            "is in a mode that flies from the sticks. All are off by default.",
    }));
    return card;
  }

  // One row of the Controls card. Every switch here does the same three
  // things — tell the joystick module, persist the one key it owns, and undo
  // both if the backend refuses — so they are built rather than repeated.
  function controlSwitch(spec) {
    function live(on) {
      if (Corvus.joystick && typeof Corvus.joystick[spec.apply] === "function") {
        Corvus.joystick[spec.apply](on);
      }
    }
    const sw = Corvus.ui.toggle({
      id: spec.id,
      value: spec.value,
      ariaLabel: spec.label,
      onChange: (on) => {
        live(on);
        // Only this switch's key is sent: the backend merges `controls`
        // wholesale, and posting both would let a stale render of one switch
        // overwrite the other.
        const patch = {};
        patch[spec.key] = on;
        return postConfig({ controls: patch }, { strict: true })
          .catch((error) => {
            live(!on);   // put the map back where the persisted config still says it is
            throw error;
          });
      },
    });
    return Corvus.ui.field({
      label: spec.label,
      control: sw.el,
      className: "field-switch",
      hint: spec.hint,
    });
  }

  // Which cut of the Corvus mark the desktop app hands the operating system
  // for its Dock / taskbar icon: the shipped white artwork, or the inverted
  // black one for a light dock. The desktop wrapper (corvus/app.py) is the
  // only consumer — this switch deliberately touches nothing else, so the
  // color theme, the top-bar mark and the browser tab icon all stay put.
  //
  // Persisted in the backend config rather than localStorage because the
  // process that acts on it is the Python wrapper, not this page. It picks
  // the change up on its own timer, so the icon flips while the app runs.
  function appIconCard(cfg) {
    const card = Corvus.ui.card({ title: "App icon" });
    const sw = Corvus.ui.toggle({
      id: "settingsInvertedAppIcon",
      value: !!(cfg.ui && cfg.ui.inverted_app_icon),
      ariaLabel: "Inverted app icon",
      onChange: (on) => postConfig({ ui: { inverted_app_icon: on } }, { strict: true }),
    });
    card.appendChild(Corvus.ui.field({
      label: "Inverted app icon",
      control: sw.el,
      className: "field-switch",
      hint: "Switches the icon the desktop app shows in the Dock (macOS) or " +
            "the taskbar (Linux) from the white mark to the black one, which " +
            "reads better on a light dock. Applies to the running app within " +
            "a second; the icon the installer put in Finder or the launcher " +
            "keeps whatever the build shipped. Changes nothing else \u2014 not " +
            "the color theme, and not the mark in the top bar.",
    }));
    return card;
  }

  // Optional company logo shown at the far top right, opposite the Corvus
  // mark (which always keeps the left end of the bar). Deliberately empty by
  // default: an operator adds their own PNG (a university or unit crest), and
  // Corvus ships with none. The bytes live in the backend's branding folder,
  // not in the config file — the config only records the display filename,
  // which is also what tells the UI a logo is configured at all.
  function companyLogoCard(cfg) {
    const configured = (cfg.branding && cfg.branding.logo) || "";
    const card = Corvus.ui.card({ title: "Company logo" });

    const preview = document.createElement("div");
    preview.className = "brand-logo-preview";
    const name = document.createElement("span");
    name.className = "brand-logo-name";

    const fileInput = document.createElement("input");
    fileInput.type = "file";
    fileInput.accept = "image/png,.png";
    fileInput.className = "brand-logo-input";
    fileInput.setAttribute("aria-hidden", "true");
    fileInput.tabIndex = -1;

    const chooseBtn = Corvus.ui.button({
      variant: "secondary",
      icon: "image-plus",
      label: "Choose PNG\u2026",
      onClick: () => fileInput.click(),
    });
    const removeBtn = Corvus.ui.button({
      variant: "secondary",
      icon: "trash-2",
      label: "Remove",
      onClick: () => removeLogo(),
    });
    const status = Corvus.ui.message({});

    // Single place that reflects "is a logo set" into the card AND the live
    // top bar, so upload / remove / initial render can never disagree.
    function showLogo(logoName) {
      Corvus.ui.clear(preview);
      if (logoName) {
        const img = document.createElement("img");
        img.alt = logoName;
        img.src = `/api/branding/logo?v=${Date.now()}`;
        preview.appendChild(img);
        name.textContent = logoName;
      } else {
        const empty = document.createElement("span");
        empty.className = "brand-logo-empty";
        empty.textContent = "No company logo";
        preview.appendChild(empty);
        name.textContent = "Corvus mark only";
      }
      removeBtn.disabled = !logoName;
      if (Corvus.topbar && typeof Corvus.topbar.setCompanyLogo === "function") {
        Corvus.topbar.setCompanyLogo(logoName);
      }
    }

    async function uploadLogo(file) {
      // Reject non-PNGs before the round trip; the backend checks the magic
      // bytes too, so a renamed JPEG never reaches the top bar either way.
      if (!/\.png$/i.test(file.name) && file.type !== "image/png") {
        status.show("Choose a PNG file.", "err");
        return;
      }
      status.show("Uploading\u2026");
      Corvus.ui.setBusy(chooseBtn, true);
      try {
        const res = await Corvus.telemetry.requestJson(
          `/api/branding/logo?name=${encodeURIComponent(file.name)}`,
          { method: "POST", headers: { "Content-Type": "application/octet-stream" }, body: file },
        );
        showLogo((res && res.logo) || file.name);
        status.show("Company logo updated.", "ok");
      } catch (error) {
        status.show(error.message || "Could not upload the logo.", "err");
      } finally {
        Corvus.ui.setBusy(chooseBtn, false);
      }
    }

    async function removeLogo() {
      status.show("Removing\u2026");
      Corvus.ui.setBusy(removeBtn, true);
      try {
        await Corvus.telemetry.requestJson("/api/branding/logo/remove", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: "{}",
        });
        showLogo("");
        status.show("Company logo removed.", "ok");
      } catch (error) {
        status.show(error.message || "Could not remove the logo.", "err");
      } finally {
        Corvus.ui.setBusy(removeBtn, false);
      }
    }

    fileInput.addEventListener("change", () => {
      const file = fileInput.files && fileInput.files[0];
      // Reset first: picking the same file twice must still fire a change.
      fileInput.value = "";
      if (file) uploadLogo(file);
    });

    const row = document.createElement("div");
    row.className = "brand-logo-row";
    row.append(preview, name, fileInput);
    card.appendChild(Corvus.ui.field({
      label: "Top-right logo",
      control: row,
      hint: "Optional PNG shown at the top right of the status bar. The Corvus " +
            "mark stays on the left. Transparent artwork works best; max 4 MB. " +
            "None is set by default.",
    }));
    card.appendChild(Corvus.ui.actions([chooseBtn, removeBtn]));
    card.appendChild(status.el);
    showLogo(configured);
    return card;
  }

  // Color theme picker. The config is authoritative over the locally cached
  // theme, so applying it here also corrects a stale localStorage value the
  // pre-paint script in index.html may have used.
  function themeCard(cfg) {
    const current = Corvus.theme.fromConfig(cfg) || Corvus.theme.applySaved();
    Corvus.theme.setTheme(current);

    const picker = Corvus.ui.optionCards({
      ariaLabel: "Color theme",
      columns: 2,
      value: current,
      options: Corvus.theme.THEMES,
      onChange: (id) => {
        Corvus.theme.setTheme(id);
        postConfig({ theme: { name: id } });
      },
    });

    return Corvus.ui.card({
      title: "Color theme",
      body: picker.el,
    });
  }

  // Interface size. Same shape as the theme card — the config is authoritative
  // over the locally cached value, so rendering the page also corrects a stale
  // localStorage scale the pre-paint script in index.html may have used.
  //
  // The slider previews on every step the thumb crosses and only persists on
  // release: the operator is judging the result by looking at it, so waiting
  // for a commit to redraw would make the control unusable, and writing the
  // config on each crossed step would POST five times per drag.
  function scaleCard(cfg) {
    const current = Corvus.scale.fromConfig(cfg) || Corvus.scale.applySaved();
    Corvus.scale.setScale(current);

    const control = Corvus.ui.slider({
      id: "settingsUiScale",
      ariaLabel: "Interface size",
      value: current,
      steps: Corvus.scale.STEPS,
      onInput: (v) => Corvus.scale.setScale(v),
      onChange: (v) => postConfig({ ui: { scale: Corvus.scale.setScale(v) } }),
    });

    const card = Corvus.ui.card({ title: "Interface size" });
    card.appendChild(Corvus.ui.field({
      control: control.el,
      hint: "Scales the whole interface — text, icons, bars and panels — on this " +
            "machine. Larger reads better on a bright field laptop; smaller fits " +
            "more of the map and the engineering panel on screen. 100% is the default.",
    }));
    return card;
  }

  // Map service picker. The source list is fetched rather than mirrored here:
  // corvus/tile_sources.py is the single registry, and /api/tiles/sources
  // reports both the provider grouping and each source's live cache stats.
  // `gen` gates the async render so a navigation away mid-fetch cannot append
  // into a pageView that has since been repurposed (same contract as the SSH
  // and About sections).
  function mapServiceCard(cfg, gen) {
    const card = Corvus.ui.card({ title: "Map service" });
    const desc = Corvus.ui.empty("Loading map services\u2026");
    card.appendChild(desc);

    Corvus.telemetry.requestJson("/api/tiles/sources").then((data) => {
      if (gen !== undefined && gen !== navGeneration) return;
      const providers = (data && data.providers) || [];
      const sources = (data && data.sources) || [];
      if (!providers.length) {
        desc.textContent = "No map services available.";
        return;
      }
      card.removeChild(desc);
      card.appendChild(buildMapServicePicker(providers, sources, cfg, data.default_provider));
      Corvus.ui.refreshIcons();
    }).catch(() => {
      if (gen !== undefined && gen !== navGeneration) return;
      desc.textContent = "Could not load map services.";
    });

    return card;
  }

  function buildMapServicePicker(providers, sources, cfg, fallbackProvider) {
    const wrap = document.createElement("div");
    const byId = {};
    sources.forEach((s) => { byId[s.id] = s; });

    // The persisted base layer is the source of truth for which service is
    // active — the provider key is a convenience mirror, so a config that has
    // one but not the other still resolves. base_layer wins on a conflict
    // because it is what the map actually renders.
    const savedLayer = (cfg.map && cfg.map.base_layer) || "";
    const activeProvider =
      (byId[savedLayer] && byId[savedLayer].provider) ||
      ((cfg.map && cfg.map.provider) || "") ||
      fallbackProvider ||
      (providers[0] && providers[0].id);

    const picker = Corvus.ui.optionCards({
      ariaLabel: "Map service",
      columns: 2,
      value: activeProvider,
      options: providers.map((p) => ({
        id: p.id,
        label: p.label,
        desc: describeProvider(p, byId),
        icon: "map",
      })),
      onChange: (id) => selectProvider(id),
    });
    wrap.appendChild(picker.el);

    // Within the chosen service, which of its layers to show. A service with
    // a single layer (OpenStreetMap) hides this row entirely rather than
    // showing a one-option control the operator cannot act on.
    const layerField = document.createElement("div");
    layerField.className = "settings-map-layer";
    wrap.appendChild(layerField);

    const note = Corvus.ui.empty("");
    note.className = "field-hint";
    wrap.appendChild(note);

    function currentStyleOf(layerId) {
      return byId[layerId] ? byId[layerId].style : null;
    }

    function renderLayerField(providerId, selectedLayer) {
      Corvus.ui.clear(layerField);
      const prov = providers.find((p) => p.id === providerId);
      const ids = (prov && prov.sources) || [];
      if (ids.length < 2) return;
      const sel = Corvus.ui.select({
        id: "settingsMapLayer",
        ariaLabel: "Map layer",
        value: selectedLayer,
        options: ids.map((id) => ({ value: id, label: (byId[id] || {}).label || id })),
        onChange: (id) => applyLayer(id),
      });
      layerField.appendChild(Corvus.ui.field({ label: "Layer", control: sel }));
    }

    function describeCache(layerId) {
      const s = byId[layerId];
      if (!s) return "";
      const n = s.cached_count || 0;
      return n > 0
        ? `${n.toLocaleString()} tiles cached offline \u00b7 up to z${s.maxzoom}`
        : `Nothing cached yet \u00b7 up to z${s.maxzoom}`;
    }

    // Switching service keeps the operator on the equivalent layer where the
    // new service has one (Esri Satellite -> Google Satellite), and falls back
    // to that service's first layer otherwise.
    function selectProvider(providerId) {
      const prov = providers.find((p) => p.id === providerId);
      if (!prov || !prov.sources.length) return;
      const wantStyle = currentStyleOf(activeLayerId);
      const match = prov.sources.find((id) => byId[id] && byId[id].style === wantStyle);
      applyLayer(match || prov.sources[0]);
    }

    function applyLayer(layerId) {
      if (!byId[layerId]) return;
      activeLayerId = layerId;
      const providerId = byId[layerId].provider;
      picker.setValue(providerId);
      renderLayerField(providerId, layerId);
      note.textContent = describeCache(layerId);
      // Apply to the live map immediately, then persist. The map module owns
      // the switch so the layers popover on the Home tab stays in sync.
      if (Corvus.map && typeof Corvus.map.setBaseLayer === "function") {
        Corvus.map.setBaseLayer(layerId);
      }
      postConfig({ map: { base_layer: layerId, provider: providerId } });
    }

    let activeLayerId =
      (byId[savedLayer] && savedLayer) ||
      ((providers.find((p) => p.id === activeProvider) || {}).sources || [])[0];

    renderLayerField(activeProvider, activeLayerId);
    note.textContent = describeCache(activeLayerId);
    return wrap;
  }

  /** One-line summary of what a service offers: its layer count, plus how many
   *  of those layers already have tiles on disk (what matters in the field). */
  function describeProvider(prov, byId) {
    const ids = prov.sources || [];
    const cached = ids.filter((id) => byId[id] && (byId[id].cached_count || 0) > 0).length;
    const layers = ids.length === 1 ? "1 layer" : `${ids.length} layers`;
    return cached > 0 ? `${layers} \u00b7 ${cached} cached` : layers;
  }

  // --- Section B: SSH Connections (saved-connection manager) ---
  // `gen` (optional, from renderSettingsPage) gates the async list population
  // so a navigation that repurposes pageView mid-fetch can't append into a
  // detached list. Post-render refreshes (add/connect handlers) pass no gen.
  function renderSSHSection(container, gen) {
    const list = document.createElement("div");
    refreshSettingsSSHList(list, gen);
    container.appendChild(Corvus.ui.section({
      title: "SSH Connections",
      body: Corvus.ui.card({ body: list }),
    }));
  }

  function refreshSettingsSSHList(list, gen) {
    Corvus.ui.clear(list).appendChild(Corvus.ui.empty("Loading\u2026"));
    Corvus.telemetry.requestJson("/api/ssh/connections").then((data) => {
      // Skip if the page was repurposed after this render started. Handler-
      // driven refreshes (add/connect) pass no gen and always refresh.
      if (gen !== undefined && gen !== navGeneration) return;
      const conns = (data && data.connections) || [];
      Corvus.ui.clear(list);
      if (!conns.length) {
        list.appendChild(Corvus.ui.empty("No saved SSH connections. Click + to add one."));
      } else {
        conns.forEach((c) => list.appendChild(settingsSSHRow(c, list)));
      }
      // Reuse the SSH tab's add modal; refresh this list once the save lands.
      const addBtn = Corvus.ui.button({
        variant: "secondary",
        shape: "block",
        icon: "plus",
        label: "ADD CONNECTION",
        onClick: () => {
          if (Corvus.panel && typeof Corvus.panel.addSSHConnection === "function") {
            Corvus.panel.addSSHConnection(() => refreshSettingsSSHList(list));
          }
        },
      });
      addBtn.style.marginTop = "10px";
      list.appendChild(addBtn);
      Corvus.ui.refreshIcons();
    }).catch(() => {
      if (gen !== undefined && gen !== navGeneration) return;
      Corvus.ui.clear(list).appendChild(Corvus.ui.empty("Could not load SSH connections."));
      Corvus.ui.refreshIcons();
    });
  }

  function settingsSSHRow(c, list) {
    const r = document.createElement("div");
    r.className = "settings-ssh-row";
    const info = document.createElement("span");
    info.className = "settings-ssh-info";
    info.textContent = `${c.name} — ${c.host}:${c.port} (${c.username})`;
    r.appendChild(info);
    const actions = document.createElement("div");
    actions.className = "settings-ssh-actions";

    const note = document.createElement("span");
    note.className = "settings-ssh-note";
    const connectBtn = Corvus.ui.button({
      variant: "primary",
      size: "sm",
      label: "CONNECT",
    });
    connectBtn.addEventListener("click", async () => {
      connectBtn.textContent = "CONNECTING…";
      connectBtn.disabled = true;
      note.textContent = "";
      let res;
      try {
        res = await Corvus.telemetry.requestJson("/api/ssh/connect", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ name: c.name }),   // connect by name
        });
      } catch (err) {
        res = { ok: false, error: err.message };
      }
      if (res && res.ok && res.connected) {
        // Hand off to the panel's live terminal + switch the SSH tab to it.
        // The whole connection goes across, not just the host: the terminal
        // header names the account that logged in, and only the backend's
        // reply knows which one the saved entry resolved to.
        if (Corvus.panel && typeof Corvus.panel.showSSHTerminal === "function") {
          Corvus.panel.showSSHTerminal({
            name: c.name,
            host: res.host || c.host,
            port: res.port || c.port,
            username: res.username || c.username,
          });
        }
        switchToSSHTab();
        connectBtn.textContent = "CONNECTED";
        connectBtn.disabled = false;
      } else {
        connectBtn.textContent = "CONNECT";
        connectBtn.disabled = false;
        note.textContent = (res && res.error) ? res.error : "Connect failed";
        refreshSettingsSSHList(list);   // reflect any connected-state change
      }
    });
    actions.appendChild(connectBtn);
    actions.appendChild(note);

    const rm = Corvus.ui.iconButton("trash-2", {
      title: `Remove ${c.name}`,
      ariaLabel: `Remove ${c.name}`,
    });
    rm.addEventListener("click", async () => {
      if (!confirm(`Remove connection ${c.name}?`)) return;
      try {
        await fetch("/api/ssh/connections/remove", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ name: c.name }),
        }).then((r) => r.json());
      } catch (_e) {}
      refreshSettingsSSHList(list);
    });
    actions.appendChild(rm);

    r.appendChild(actions);
    return r;
  }

  // --- Section C: Connection (read-only, accurate from config) ---
  function renderConnectionSection(container, cfg) {
    const card = Corvus.ui.card({});
    card.appendChild(row("MAVLink", cfg.mavlink_connection || "\u2014"));
    card.appendChild(row("HTTP Port", cfg.http_port != null ? String(cfg.http_port) : "\u2014"));
    container.appendChild(Corvus.ui.section({ title: "Connection", body: card }));
  }

  // --- Section D: Files (where Corvus writes what it saves) ---
  // One editable folder today (parameter exports). The tile cache and tlog
  // directories are also config keys but are read at startup, so changing them
  // here would silently not take effect until a restart — they stay
  // config-file-only until that is handled.
  function renderFilesSection(container, cfg) {
    const card = Corvus.ui.card({});
    const dirInput = Corvus.ui.input({
      id: "settingsParamsDir",
      value: cfg.params_dir || "",
      placeholder: "~/.corvus/params",
      mono: true,
      ariaLabel: "Parameter export folder",
      autocomplete: false,
      spellcheck: false,
      // Persist on blur/Enter rather than per keystroke: a half-typed path is
      // never a folder the operator meant.
      onChange: (value) => postConfig({ params_dir: value.trim() }),
    });
    card.appendChild(Corvus.ui.field({
      label: "Parameter export folder",
      control: dirInput,
      hint: "Where Export writes parameter files, on the machine running Corvus. " +
            "Leave empty for ~/.corvus/params. The export dialog can still override it per file.",
    }));
    container.appendChild(Corvus.ui.section({ title: "Files", body: card }));
  }

  // --- Section E: About (existing — reads /api/version, no hardcoded version) ---
  // `gen` (optional, from renderSettingsPage) gates the independent /api/version
  // fetch so it can't append rows to a pageView that has since been repurposed.
  // Undefined gen ⇒ no guard (keeps the helper callable from non-render contexts).
  function renderAboutSection(container, gen) {
    const card = Corvus.ui.card({});
    const rows = document.createElement("div");
    card.appendChild(rows);
    Corvus.telemetry.requestJson("/api/version").then((v) => {
      if (gen !== undefined && gen !== navGeneration) return;
      rows.appendChild(row("Product", v.product || "Corvus GCS"));
      rows.appendChild(row("Version", v.version || "\u2014"));
      rows.appendChild(row("PX4 Profile", v.px4_profile || "\u2014"));
    }).catch(() => {
      if (gen !== undefined && gen !== navGeneration) return;
      rows.appendChild(row("Version", "Unavailable"));
    });
    // Appended outside the fetch so the button is there even when the backend
    // is unreachable — the credits it opens are static apart from the map
    // attributions, which degrade on their own.
    card.appendChild(Corvus.ui.actions(Corvus.ui.button({
      variant: "secondary",
      icon: "info",
      label: "Credits",
      onClick: () => Corvus.credits.open(),
    })));
    container.appendChild(Corvus.ui.section({ title: "About", body: card }));
    container.appendChild(Corvus.ui.section({
      title: "Updates", body: updatesCard(gen),
    }));
  }

  // Update card: the switch that governs whether Corvus looks at the GitHub
  // releases at all, plus a manual check. Its state comes from GET /api/update
  // rather than the config read the page already did, because the same
  // response carries the last check's result and error — one fetch, one truth.
  function updatesCard(gen) {
    const card = Corvus.ui.card({});
    const status = Corvus.ui.message({});
    const rows = document.createElement("div");
    card.appendChild(rows);

    const sw = Corvus.ui.toggle({
      value: true,
      ariaLabel: "Check for updates",
      onChange: (on) => postConfig({ updates: { check: on } }, { strict: true }),
    });
    card.appendChild(Corvus.ui.field({
      label: "Check for updates",
      control: sw.el,
      className: "field-switch",
      hint: "Compares the running version against the published releases on " +
            "GitHub and shows a notice when a newer one exists. Nothing is " +
            "downloaded and nothing is sent about this machine beyond the " +
            "request itself. The check never runs while the vehicle is armed, " +
            "and with no internet it fails silently — Corvus never needs the " +
            "network to fly.",
    }));

    // "Available" is only worth a row when there is something newer; a machine
    // running the current release should read as current, not as a comparison.
    function paint(data) {
      Corvus.ui.clear(rows);
      rows.appendChild(row("Installed", data.current || "\u2014"));
      if (data.update_available) {
        rows.appendChild(row("Available", data.latest || "\u2014"));
      } else if (data.latest) {
        rows.appendChild(row("Latest release", data.latest));
      }
      sw.setValue(data.enabled !== false);
    }

    const checkBtn = Corvus.ui.button({
      variant: "secondary",
      icon: "refresh-cw",
      label: "Check now",
      onClick: () => {
        Corvus.ui.setBusy(checkBtn, true);
        status.hide();
        // manual: the dialog is raised even for a version the operator
        // skipped, and the error is reported instead of swallowed.
        Corvus.update.check({ refresh: true, manual: true }).then((data) => {
          if (!data) return;
          paint(data);
          if (data.enabled === false) {
            status.show("Update checks are switched off.", "warn");
          } else if (data.error) {
            status.show(data.error, "warn");
          } else if (data.update_available) {
            status.show(`Version ${data.latest} is available.`, "ok");
          } else {
            status.show("Corvus GCS is up to date.", "ok");
          }
        }).catch(() => {
          status.show("Could not reach the release server.", "warn");
        }).finally(() => Corvus.ui.setBusy(checkBtn, false));
      },
    });
    card.appendChild(Corvus.ui.actions(checkBtn));
    card.appendChild(status.el);

    // Cached read: opening Settings must not fire a network check by itself.
    Corvus.telemetry.requestJson("/api/update").then((data) => {
      if (gen !== undefined && gen !== navGeneration) return;
      paint(data);
      if (data.enabled !== false && data.update_available) {
        status.show(`Version ${data.latest} is available.`, "ok");
      }
    }).catch(() => {
      if (gen !== undefined && gen !== navGeneration) return;
      status.show("Update status unavailable.", "warn");
    });

    return card;
  }

  function renderSettingsPage(container) {
    // Capture the navigation generation so the async /api/config + /api/version
    // fetches can skip mutating pageView if the operator navigates away before
    // they resolve (otherwise the resolved promise appends Settings sections
    // onto the now-different page).
    const gen = navGeneration;
    container.appendChild(pageHeader("Settings", "Application configuration"));
    // Build the editable page from the fetched config; the SSH section does
    // its own fetch (it carries the live `connected` status), and About keeps
    // its independent /api/version fetch.
    Corvus.telemetry.requestJson("/api/config").then((res) => {
      if (gen !== navGeneration) return;
      const cfg = (res && res.config) || {};
      renderAppearanceSection(container, cfg, gen);
      renderSSHSection(container, gen);
      renderConnectionSection(container, cfg);
      renderFilesSection(container, cfg);
      renderAboutSection(container, gen);
      Corvus.ui.refreshIcons();
    }).catch(() => {
      if (gen !== navGeneration) return;
      container.appendChild(Corvus.ui.card({
        body: "Settings unavailable. Could not load configuration from the backend.",
      }));
      renderAboutSection(container, gen);   // About has its own fetch + graceful fallback
      Corvus.ui.refreshIcons();
    });
  }

  function renderPlaceholder(container) {
    container.appendChild(pageHeader("Page", "Not yet implemented"));
  }

  function init() {
    leftNav = document.getElementById("leftNav");
    mapView = document.getElementById("mapView");
    pageView = document.getElementById("pageView");
    renderLeftNav();
  }

  return { init };
})();
