"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.theme — accent picker v1. The accent is the single runtime-overridable
  theme knob: setAccent(hex) writes the CSS custom property --accent on the
  document root and caches it in localStorage so the next load can apply it
  synchronously (no flicker) before the backend config fetch confirms it.
  The derived --accent-bright/-hover/-dim/-dark tokens stay at their :root
  defaults for now (documented v1 limitation).
*/
Corvus.theme = (function () {
  const KEY = "corvus.accent";
  const DEFAULT = "#3DA876";

  function setAccent(hex) {
    const v = (typeof hex === "string" && hex.trim()) ? hex.trim() : DEFAULT;
    try { document.documentElement.style.setProperty("--accent", v); } catch (_e) {}
    try { localStorage.setItem(KEY, v); } catch (_e) {}
  }

  function applySaved() {
    let v = DEFAULT;
    try { v = localStorage.getItem(KEY) || DEFAULT; } catch (_e) {}
    setAccent(v);
    return v;
  }

  return { setAccent, applySaved, DEFAULT };
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

  function icon(name) {
    const i = document.createElement("i");
    i.setAttribute("data-lucide", name);
    return i;
  }

  function renderLeftNav() {
    leftNav.innerHTML = "";

    const home = document.createElement("button");
    home.className = "nav-item" + (activeNav === "home" ? " active" : "");
    home.dataset.nav = "home";
    home.appendChild(icon("house"));
    const hl = document.createElement("span");
    hl.className = "nav-label";
    hl.textContent = "HOME";
    home.appendChild(hl);
    home.addEventListener("click", () => switchTo("home"));
    leftNav.appendChild(home);

    const divider = document.createElement("div");
    divider.className = "nav-divider";
    leftNav.appendChild(divider);

    NAV.forEach((n) => {
      const item = document.createElement("button");
      item.className = "nav-item" + (n.id === activeNav ? " active" : "");
      item.dataset.nav = n.id;
      item.appendChild(icon(n.icon));
      const lbl = document.createElement("span");
      lbl.className = "nav-label";
      lbl.textContent = n.label;
      item.appendChild(lbl);
      item.addEventListener("click", () => switchTo(n.id));
      leftNav.appendChild(item);
    });

    const spacer = document.createElement("div");
    spacer.className = "nav-spacer";
    leftNav.appendChild(spacer);

    const settings = document.createElement("button");
    settings.className = "nav-item" + (activeNav === "settings" ? " active" : "");
    settings.dataset.nav = "settings";
    settings.title = "Settings";
    settings.appendChild(icon("settings"));
    const sl = document.createElement("span");
    sl.className = "nav-label";
    sl.textContent = "SET";
    settings.appendChild(sl);
    settings.addEventListener("click", () => switchTo("settings"));
    leftNav.appendChild(settings);
    if (window.lucide && lucide.createIcons) lucide.createIcons();
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
    pageView.innerHTML = "";
    fn(pageView);
    if (window.lucide && lucide.createIcons) lucide.createIcons();
  }

  function pageHeader(title, subtitle) {
    const h = document.createElement("div");
    h.className = "page-header";
    h.innerHTML = `<div class="page-title">${title}</div><div class="page-subtitle">${subtitle}</div>`;
    return h;
  }

  function sectionTitle(text) {
    const t = document.createElement("div");
    t.className = "page-section-title";
    t.textContent = text;
    return t;
  }

  function row(label, value) {
    const r = document.createElement("div");
    r.className = "page-row";
    r.innerHTML = `<span class="page-row-label"></span><span class="page-row-value"></span>`;
    r.querySelector(".page-row-label").textContent = label;
    r.querySelector(".page-row-value").textContent = value;
    return r;
  }

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
    const s = document.createElement("div");
    s.className = "page-section";
    s.appendChild(sectionTitle("Recent Console Output"));
    const card = document.createElement("div");
    card.className = "page-card";
    const lines = document.querySelectorAll("#consoleOutput .con-line");
    const recent = Array.from(lines).slice(-30).map((l) => {
      const t = l.querySelector(".con-time")?.textContent || "";
      const m = l.querySelector(".con-msg")?.textContent || "";
      return `${t}  ${m}`;
    });
    if (recent.length === 0) {
      card.innerHTML = '<div class="page-card-desc">No log entries yet.</div>';
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
    s.appendChild(card);
    container.appendChild(s);
  }

  function renderAnalysisPage(container) {
    container.appendChild(pageHeader("Analysis", "Telemetry analysis and flight statistics"));
    const s = document.createElement("div");
    s.className = "page-section";
    s.appendChild(sectionTitle("Current Telemetry"));
    const state = Corvus.telemetry.getState() || {};
    const card = document.createElement("div");
    card.className = "page-card";
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
      card.innerHTML = '<div class="page-card-desc">Vehicle not connected.</div>';
    }
    s.appendChild(card);
    container.appendChild(s);
  }

  // --- Settings page data ---
  // Accent presets for the theme picker v1. Green is the default and matches
  // the :root --accent value in main.css.
  const ACCENT_PRESETS = [
    { name: "Green", hex: "#3DA876" },
    { name: "Blue", hex: "#4CC9FF" },
    { name: "Orange", hex: "#F5A623" },
    { name: "Red", hex: "#FF514D" },
    { name: "Purple", hex: "#A371F7" },
  ];
  // Map base-layer id -> human label. Mirrors the map agent's TILE registry
  // (corvus/tile_sources.py) so the read-only settings row shows the same
  // label the layers popover uses. The actual picker is owned by the map agent.
  const BASE_LAYER_LABELS = {
    satellite: "Satellite",
    streets: "Streets",
    hybrid: "Hybrid",
    topo: "Topographic",
    osm: "OpenStreetMap",
  };

  function normalizeHex(v) {
    if (typeof v !== "string") return null;
    const s = v.trim();
    if (/^#?[0-9a-fA-F]{6}$/.test(s)) return s.startsWith("#") ? s : "#" + s;
    return null;
  }
  function sameHex(a, b) {
    const na = normalizeHex(a), nb = normalizeHex(b);
    return !!na && !!nb && na.toLowerCase() === nb.toLowerCase();
  }

  // POST a partial config update; best-effort (accent is already applied live,
  // a persist failure is non-fatal). Reuses the telemetry transport so HTTP
  // errors reject instead of resolving with a bogus body.
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

  // --- Section A: Appearance (accent / theme picker) ---
  function renderAppearanceSection(container, cfg) {
    const sec = document.createElement("div");
    sec.className = "page-section";
    sec.appendChild(sectionTitle("Appearance"));
    const card = document.createElement("div");
    card.className = "page-card";

    const current = (cfg.theme && cfg.theme.accent) || Corvus.theme.DEFAULT;
    Corvus.theme.setAccent(current);   // authoritative value from config wins

    const curRow = row("Accent", current);
    const curVal = curRow.querySelector(".page-row-value");
    card.appendChild(curRow);

    const swatchRow = document.createElement("div");
    swatchRow.className = "settings-swatches";
    const swatches = [];
    ACCENT_PRESETS.forEach((p) => {
      const sw = document.createElement("button");
      sw.type = "button";
      sw.className = "settings-swatch" + (sameHex(current, p.hex) ? " selected" : "");
      sw.style.background = p.hex;
      sw.title = p.name;
      sw.setAttribute("aria-label", `Accent ${p.name}`);
      sw.dataset.hex = p.hex;
      sw.addEventListener("click", () => selectAccent(p.hex));
      swatchRow.appendChild(sw);
      swatches.push(sw);
    });
    card.appendChild(swatchRow);

    const customRow = document.createElement("div");
    customRow.className = "settings-accent-row";
    const colorInput = document.createElement("input");
    colorInput.type = "color";
    colorInput.className = "settings-color-input";
    colorInput.value = normalizeHex(current) || Corvus.theme.DEFAULT;
    colorInput.title = "Custom accent color";
    colorInput.setAttribute("aria-label", "Custom accent color");
    const hexInput = document.createElement("input");
    hexInput.type = "text";
    hexInput.className = "settings-hex-input";
    hexInput.value = current;
    hexInput.placeholder = "#RRGGBB";
    hexInput.setAttribute("aria-label", "Accent hex value");
    customRow.appendChild(colorInput);
    customRow.appendChild(hexInput);
    card.appendChild(customRow);

    let persistTimer = null;
    function persistDebounced(hex) {
      if (persistTimer) clearTimeout(persistTimer);
      persistTimer = setTimeout(() => postConfig({ theme: { accent: hex } }), 250);
    }
    function persistNow(hex) {
      if (persistTimer) { clearTimeout(persistTimer); persistTimer = null; }
      postConfig({ theme: { accent: hex } });
    }

    function syncSelected(hex) {
      swatches.forEach((sw) => sw.classList.toggle("selected", sameHex(sw.dataset.hex, hex)));
    }
    function selectAccent(hex) {
      Corvus.theme.setAccent(hex);
      curVal.textContent = hex;
      colorInput.value = normalizeHex(hex) || colorInput.value;
      hexInput.value = hex;
      syncSelected(hex);
      persistNow(hex);
    }

    // color input: live-apply on every input (cheap CSS var write), debounce
    // the persist; flush immediately on change (release).
    colorInput.addEventListener("input", () => {
      const v = colorInput.value;
      hexInput.value = v;
      curVal.textContent = v;
      Corvus.theme.setAccent(v);
      syncSelected(v);
      persistDebounced(v);
    });
    colorInput.addEventListener("change", () => persistNow(colorInput.value));

    // hex text input: validate on input, apply + debounce; finalize on change.
    hexInput.addEventListener("input", () => {
      const v = normalizeHex(hexInput.value);
      if (!v) return;
      colorInput.value = v;
      curVal.textContent = v;
      Corvus.theme.setAccent(v);
      syncSelected(v);
      persistDebounced(v);
    });
    hexInput.addEventListener("change", () => {
      const v = normalizeHex(hexInput.value);
      if (v) {
        hexInput.value = v;
        colorInput.value = v;
        curVal.textContent = v;
        Corvus.theme.setAccent(v);
        syncSelected(v);
        persistNow(v);
      }
    });

    sec.appendChild(card);
    container.appendChild(sec);
  }

  // --- Section B: SSH Connections (saved-connection manager) ---
  // `gen` (optional, from renderSettingsPage) gates the async list population
  // so a navigation that repurposes pageView mid-fetch can't append into a
  // detached list. Post-render refreshes (add/connect handlers) pass no gen.
  function renderSSHSection(container, gen) {
    const sec = document.createElement("div");
    sec.className = "page-section";
    sec.appendChild(sectionTitle("SSH Connections"));
    const card = document.createElement("div");
    card.className = "page-card";
    const list = document.createElement("div");
    card.appendChild(list);
    refreshSettingsSSHList(list, gen);
    sec.appendChild(card);
    container.appendChild(sec);
  }

  function refreshSettingsSSHList(list, gen) {
    list.innerHTML = '<div class="page-card-desc">Loading…</div>';
    Corvus.telemetry.requestJson("/api/ssh/connections").then((data) => {
      // Skip if the page was repurposed after this render started. Handler-
      // driven refreshes (add/connect) pass no gen and always refresh.
      if (gen !== undefined && gen !== navGeneration) return;
      const conns = (data && data.connections) || [];
      list.innerHTML = "";
      if (!conns.length) {
        const empty = document.createElement("div");
        empty.className = "page-card-desc";
        empty.textContent = "No saved SSH connections. Click + to add one.";
        list.appendChild(empty);
      } else {
        conns.forEach((c) => list.appendChild(settingsSSHRow(c, list)));
      }
      const addBtn = document.createElement("button");
      addBtn.className = "btn";
      addBtn.setAttribute("data-variant", "secondary");
      addBtn.setAttribute("data-shape", "block");
      addBtn.style.marginTop = "10px";
      addBtn.textContent = "+ ADD CONNECTION";
      // Reuse the SSH tab's add modal; refresh this list once the save lands.
      addBtn.addEventListener("click", () => {
        if (Corvus.panel && typeof Corvus.panel.addSSHConnection === "function") {
          Corvus.panel.addSSHConnection(() => refreshSettingsSSHList(list));
        }
      });
      list.appendChild(addBtn);
      if (window.lucide && lucide.createIcons) lucide.createIcons();
    }).catch(() => {
      if (gen !== undefined && gen !== navGeneration) return;
      list.innerHTML = '<div class="page-card-desc">Could not load SSH connections.</div>';
      if (window.lucide && lucide.createIcons) lucide.createIcons();
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

    const connectBtn = document.createElement("button");
    connectBtn.className = "btn";
    connectBtn.setAttribute("data-variant", "primary");
    connectBtn.setAttribute("data-size", "sm");
    connectBtn.textContent = "CONNECT";
    const note = document.createElement("span");
    note.className = "settings-ssh-note";
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

    const rm = document.createElement("button");
    rm.className = "icon-btn";
    rm.setAttribute("aria-label", `Remove ${c.name}`);
    rm.title = `Remove ${c.name}`;
    const trash = document.createElement("i");
    trash.setAttribute("data-lucide", "trash-2");
    rm.appendChild(trash);
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
    const sec = document.createElement("div");
    sec.className = "page-section";
    sec.appendChild(sectionTitle("Connection"));
    const card = document.createElement("div");
    card.className = "page-card";
    card.appendChild(row("MAVLink", cfg.mavlink_connection || "—"));
    const httpPort = cfg.http_port != null ? String(cfg.http_port) : "—";
    card.appendChild(row("HTTP Port", httpPort));
    sec.appendChild(card);
    container.appendChild(sec);
  }

  // --- Section D: Map (display the persisted base layer; read-only) ---
  function renderMapSection(container, cfg) {
    const sec = document.createElement("div");
    sec.className = "page-section";
    sec.appendChild(sectionTitle("Map"));
    const card = document.createElement("div");
    card.className = "page-card";
    const baseLayer = (cfg.map && cfg.map.base_layer) || "";
    const label = BASE_LAYER_LABELS[baseLayer] || baseLayer || "—";
    card.appendChild(row("Base layer", label));
    sec.appendChild(card);
    container.appendChild(sec);
  }

  // --- Section E: About (existing — reads /api/version, no hardcoded version) ---
  // `gen` (optional, from renderSettingsPage) gates the independent /api/version
  // fetch so it can't append rows to a pageView that has since been repurposed.
  // Undefined gen ⇒ no guard (keeps the helper callable from non-render contexts).
  function renderAboutSection(container, gen) {
    const sec = document.createElement("div");
    sec.className = "page-section";
    sec.appendChild(sectionTitle("About"));
    const card = document.createElement("div");
    card.className = "page-card";
    Corvus.telemetry.requestJson("/api/version").then((v) => {
      if (gen !== undefined && gen !== navGeneration) return;
      card.appendChild(row("Product", v.product || "Corvus GCS"));
      card.appendChild(row("Version", v.version || "—"));
      card.appendChild(row("PX4 Profile", v.px4_profile || "—"));
    }).catch(() => {
      if (gen !== undefined && gen !== navGeneration) return;
      card.appendChild(row("Version", "Unavailable"));
    });
    sec.appendChild(card);
    container.appendChild(sec);
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
      renderAppearanceSection(container, cfg);
      renderSSHSection(container, gen);
      renderConnectionSection(container, cfg);
      renderMapSection(container, cfg);
      renderAboutSection(container, gen);
      if (window.lucide && lucide.createIcons) lucide.createIcons();
    }).catch(() => {
      if (gen !== navGeneration) return;
      const note = document.createElement("div");
      note.className = "page-card";
      note.innerHTML = '<div class="page-card-desc">Settings unavailable. Could not load configuration from the backend.</div>';
      container.appendChild(note);
      renderAboutSection(container, gen);   // About has its own fetch + graceful fallback
      if (window.lucide && lucide.createIcons) lucide.createIcons();
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
