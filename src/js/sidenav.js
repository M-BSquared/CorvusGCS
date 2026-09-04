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

  Persistence is two-layered on purpose: localStorage so the choice can be
  applied before first paint (see the inline script in index.html) and the
  backend config so it survives a cache clear and follows the operator's
  profile. localStorage is written first and never blocks on the network.
*/
Corvus.theme = (function () {
  const KEY = "corvus.theme";
  const DEFAULT = "green";

  const THEMES = [
    { id: "green",  label: "Green",  desc: "Default",       swatch: ["#3DA876", "#171D25", "#0B0E12"] },
    { id: "blue",   label: "Blue",   desc: "Cool",          swatch: ["#3B9EFF", "#171D25", "#0B0E12"] },
    { id: "pink",   label: "Pink",   desc: "Magenta",       swatch: ["#F0509B", "#171D25", "#0B0E12"] },
    { id: "orange", label: "Orange", desc: "Amber",         swatch: ["#F58A2B", "#171D25", "#0B0E12"] },
    { id: "light",  label: "Light",  desc: "White & black", swatch: ["#1B1F26", "#F1F3F6", "#FFFFFF"] },
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

Corvus.sidenav = (function () {
  let leftNav, mapView, pageView;
  let activeNav = "home";

  // Monotonic navigation generation. Bumped on every switchTo so async work
  // started by a previous page (e.g. Settings' /api/config + /api/version
  // fetches) can capture the generation at issue time and skip mutating
  // pageView after the operator has navigated to a different page.
  let navGeneration = 0;

  const NAV = [
    { id: "setup", label: "SETUP", icon: "sliders-horizontal" },
    { id: "logs", label: "LOGS", icon: "file-text" },
    { id: "analysis", label: "ANALYSIS", icon: "chart-column" },
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
    container.appendChild(pageHeader("Analysis", "Telemetry analysis and flight statistics"));
    const state = Corvus.telemetry.getState() || {};
    const card = Corvus.ui.card({});
    if (state.connected) {
      card.appendChild(row("Altitude AMSL", `${Math.round(state.altitude_amsl)} m`));
      card.appendChild(row("Altitude AGL", `${Math.round(state.altitude_agl)} m`));
      card.appendChild(row("Groundspeed", `${state.groundspeed.toFixed(1)} m/s`));
      card.appendChild(row("Vertical speed", `${state.vspeed.toFixed(1)} m/s`));
      card.appendChild(row("Heading", `${Math.round(state.heading)}°`));
      card.appendChild(row("Pitch", `${state.pitch.toFixed(1)}°`));
      card.appendChild(row("Roll", `${state.roll.toFixed(1)}°`));
      card.appendChild(row("Battery", `${state.battery_voltage.toFixed(1)} V (${state.battery_percent}%)`));
      card.appendChild(row("GPS Fix", `${state.gps_fix} (${state.gps_satellites} sats)`));
    } else {
      card.appendChild(Corvus.ui.empty("Vehicle not connected."));
    }
    container.appendChild(Corvus.ui.section({ title: "Current Telemetry", body: card }));
  }

  // --- Settings page data ---

  // POST a partial config update; best-effort (the change is already applied
  // live, a persist failure is non-fatal for this session). Reuses the
  // telemetry transport so HTTP errors reject instead of resolving with a
  // bogus body.
  function postConfig(body) {
    return Corvus.telemetry.requestJson("/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }).catch(() => {});
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

  // --- Section A: Appearance (color theme + map service) ---
  // Two operator choices that both change how the app looks, so they share one
  // section. Each is a Corvus.ui.optionCards group: same control, different
  // content. Both apply immediately and persist in the background — nothing
  // here has a Save button, because there is nothing to get wrong and undoing
  // is one more click.
  function renderAppearanceSection(container, cfg, gen) {
    const body = document.createDocumentFragment();
    body.appendChild(themeCard(cfg));
    body.appendChild(mapServiceCard(cfg, gen));
    container.appendChild(Corvus.ui.section({ title: "Appearance", body }));
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
        if (Corvus.panel && typeof Corvus.panel.showSSHTerminal === "function") {
          Corvus.panel.showSSHTerminal(c.name, c.host);
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
