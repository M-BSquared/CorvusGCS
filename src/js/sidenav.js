"use strict";
window.Corvus = window.Corvus || {};

Corvus.sidenav = (function () {
  let leftNav, mapView, pageView;
  let activeNav = "home";

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
    activeNav = navId;
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

  function renderSettingsPage(container) {
    container.appendChild(pageHeader("Settings", "Application configuration"));
    const s = document.createElement("div");
    s.className = "page-section";
    s.appendChild(sectionTitle("Connection"));
    const card = document.createElement("div");
    card.className = "page-card";
    card.appendChild(row("MAVLink", "udp:127.0.0.1:14540"));
    card.appendChild(row("HTTP Port", "8000"));
    s.appendChild(card);

    const s2 = document.createElement("div");
    s2.className = "page-section";
    s2.appendChild(sectionTitle("About"));
    const card2 = document.createElement("div");
    card2.className = "page-card";
    Corvus.telemetry.requestJson("/api/version").then((v) => {
      card2.appendChild(row("Product", v.product || "Corvus GCS"));
      card2.appendChild(row("Version", v.version || "—"));
      card2.appendChild(row("PX4 Profile", v.px4_profile || "—"));
    }).catch(() => card2.appendChild(row("Version", "Unavailable")));
    s2.appendChild(card2);
    // appendChild takes a single node; s2 (the About/version card) was being
    // dropped by the old two-arg call.
    container.appendChild(s);
    container.appendChild(s2);
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
