"use strict";
window.Corvus = window.Corvus || {};

Corvus.app = (function () {
  let topBar, leftNav, warningsPopover, warningsList, notificationLive, mapView, pageView;
  let activeNav = "home";
  let warningsOpen = false;
  let lastState = null;
  let renderedWarningSignature = "";
  let remoteWarningsInitialized = false;
  let remoteWarningKeys = new Set();
  let localNotificationId = 0;
  const localNotifications = new Map();
  const dismissedNotifications = new Set();
  const commandDedupe = Corvus.notificationDedupe.createTracker({ windowMs: 3000 });
  // Idempotency guard for the flight-mode selector: only repopulate when the
  // mode list returned by the backend actually changes (or once per connect).
  let loadedModesSignature = "";

  const NAV = [
    { id: "setup", label: "SETUP", icon: "sliders-horizontal" },
    { id: "logs", label: "LOGS", icon: "file-text" },
    { id: "analysis", label: "ANALYSIS", icon: "bar-chart-3" },
  ];

  function icon(name) {
    const i = document.createElement("i");
    i.setAttribute("data-lucide", name);
    return i;
  }

  function vehicleLabel(state) {
    if (!state.connected) return "DISCONNECTED";
    const type = state.vehicle_type || "UNKNOWN";
    const ap = state.autopilot || "";
    return ap ? `${ap} ${type}` : type;
  }

  function modeLabel(state) {
    if (!state.connected) return "—";
    if (state.armed && !state.mode) return "ARMED";
    return state.mode || "STANDBY";
  }

  function notificationKey(notification) {
    if (notification.localId) return `local:${notification.localId}`;
    return `remote:${notification.level || "info"}:${notification.msg || ""}:${notification.meta || ""}`;
  }

  function visibleNotifications(state) {
    const remote = (state?.warnings || []).map((warning) => ({ ...warning, source: "remote" }));
    const activeRemoteKeys = new Set(remote.map(notificationKey));
    dismissedNotifications.forEach((key) => {
      if (key.startsWith("remote:") && !activeRemoteKeys.has(key)) dismissedNotifications.delete(key);
    });
    return remote.concat(Array.from(localNotifications.values()))
      .filter((notification) => !dismissedNotifications.has(notificationKey(notification)));
  }

  function blocks(state) {
    const conn = state.connected ? "healthy" : "off";
    const armed = state.armed ? "ARMED" : "DISARMED";
    const armedCls = state.armed ? "healthy" : "off";
    const gpsCls = state.connected && state.gps_fix && state.gps_fix !== "NO_GPS" && state.gps_fix !== "NO_FIX"
      ? "healthy" : "off";
    const battPct = state.battery_percent || 0;
    const battCls = !state.connected ? "off" : battPct > 25 ? "healthy" : (battPct > 12 ? "warning" : "critical");
    const notifications = visibleNotifications(state);
    const warnCount = notifications.length;
    const hasCritical = notifications.some((w) => w.level === "critical" || w.level === "error");
    const warnLevel = hasCritical ? "critical" : (warnCount > 0 ? "warning" : "healthy");
    const gpsSub = state.connected ? `(${state.gps_hdop > 0 && state.gps_hdop < 99 ? state.gps_hdop.toFixed(1) : "—"})` : "";

    return [
      { type: "logo" },
      { key: "vehicle", label: "Vehicle", value: vehicleLabel(state), dot: conn, priority: "high" },
      { key: "mode", label: "Mode", value: modeLabel(state), cls: "accent", priority: "high" },
      { key: "armed", label: "Armed", value: armed, cls: armedCls, dot: armedCls, priority: "high" },
      { key: "gps", label: "GPS", value: state.connected ? (state.gps_fix || "NO GPS") : "—", sub: gpsSub, cls: gpsCls, dot: gpsCls, priority: "high" },
      { key: "battery", label: "Battery", value: state.connected ? `${state.battery_voltage.toFixed(1)} V` : "—", sub: state.connected ? `${battPct}%` : "", cls: battCls, dot: battCls, priority: "high" },
      { key: "altitude", label: "Altitude", value: state.connected ? `${Math.round(state.altitude_amsl)}` : "—", sub: "m AMSL", priority: "mid" },
      { key: "groundspeed", label: "Groundspeed", value: state.connected ? `${state.groundspeed.toFixed(1)}` : "—", sub: "m/s", priority: "mid" },
      { key: "vspeed", label: "Vertical speed", value: state.connected ? `${state.vspeed >= 0 ? "+" : ""}${state.vspeed.toFixed(1)}` : "—", sub: "m/s", priority: "mid" },
      { key: "time", label: "Time", value: state.time || "—", priority: "mid" },
      { key: "warnings", type: "warnings", label: "Warnings", value: warnCount, level: warnLevel, priority: "high" },
    ];
  }

  function renderLogo() {
    const w = document.createElement("div");
    w.className = "tb-logo";
    const mark = document.createElement("div");
    mark.className = "tb-logo-mark";
    const img = document.createElement("img");
    img.src = "assets/CorvusGCS_logo.png";
    img.alt = "CORVUS GCS";
    mark.appendChild(img);
    const txt = document.createElement("div");
    txt.className = "tb-logo-text";
    txt.innerHTML = '<span class="tb-logo-name">CORVUS</span><span class="tb-logo-sub">GROUND CONTROL</span>';
    w.appendChild(mark);
    w.appendChild(txt);
    return w;
  }

  function renderBlock(b) {
    if (b.type === "logo") return renderLogo();
    if (b.type === "warnings") {
      const blk = document.createElement("button");
      blk.type = "button";
      blk.className = "tb-block warnings clickable";
      blk.dataset.priority = b.priority;
      blk.dataset.block = b.key;
      blk.setAttribute("aria-controls", "warningsPopover");
      blk.setAttribute("aria-expanded", "false");
      blk.setAttribute("aria-haspopup", "dialog");
      blk.innerHTML =
        `<span class="tb-label"><i data-lucide="triangle-alert" class="tb-icon"></i>${b.label}</span>` +
        `<span class="tb-value"><span class="tb-warn-badge"></span></span>`;
      blk.addEventListener("click", toggleWarnings);
      return blk;
    }
    const blk = document.createElement("div");
    blk.className = "tb-block";
    blk.dataset.priority = b.priority;
    blk.dataset.block = b.key;
    const dotHtml = b.dot ? `<span class="tb-dot ${b.dot}"></span>` : "";
    const iconHtml = b.icon ? `<i data-lucide="${b.icon}" class="tb-icon"></i>` : "";
    const subHtml = b.sub !== undefined ? `<span class="sub">${b.sub}</span>` : "";
    const valCls = b.cls ? ` ${b.cls}` : "";
    blk.innerHTML =
      `<span class="tb-label">${iconHtml}${b.label}</span>` +
      `<span class="tb-value${valCls}">${dotHtml}<span class="v-main"></span>${subHtml}</span>`;
    blk.querySelector(".v-main").textContent = b.value;
    return blk;
  }

  let topBarBuilt = false;

  function renderTopBar(state) {
    if (!topBarBuilt) {
      topBar.innerHTML = "";
      blocks(state).forEach((b) => topBar.appendChild(renderBlock(b)));
      const spacer = document.createElement("div");
      spacer.className = "tb-spacer";
      topBar.appendChild(spacer);
      if (window.lucide && lucide.createIcons) lucide.createIcons();
      topBarBuilt = true;
      updateTopBarValues(state);
    } else {
      updateTopBarValues(state);
    }
  }

  function updateTopBarValues(state) {
    const blkData = blocks(state);
    blkData.forEach((b) => {
      const el = b.key ? topBar.querySelector(`[data-block="${b.key}"]`) : null;
      if (!el || b.type === "warnings") return;
      const valEl = el.querySelector(".v-main");
      if (valEl && valEl.textContent !== b.value) valEl.textContent = b.value;
      const subEl = el.querySelector(".sub");
      if (subEl && b.sub !== undefined) subEl.textContent = b.sub;
      const dotEl = el.querySelector(".tb-dot");
      if (dotEl && b.dot) {
        dotEl.className = "tb-dot " + b.dot;
      }
      const valSpan = el.querySelector(".tb-value");
      if (valSpan) valSpan.className = "tb-value" + (b.cls ? " " + b.cls : "");
    });
    const warnEl = topBar.querySelector(".tb-block.warnings .tb-warn-badge");
    if (warnEl) {
      const notifications = visibleNotifications(state);
      const warnCount = notifications.length;
      const hasCritical = notifications.some((w) => w.level === "critical" || w.level === "error");
      const level = hasCritical ? "critical" : (warnCount > 0 ? "warning" : "healthy");
      warnEl.textContent = String(warnCount);
      warnEl.style.background = level === "critical" ? "#FF514D" : level === "warning" ? "#F5C842" : "#45D483";
      const valSpan = warnEl.parentElement;
      if (valSpan) valSpan.className = `tb-value ${level}`;
      const warningButton = warnEl.closest(".tb-block.warnings");
      if (warningButton) warningButton.setAttribute("aria-label", `${warnCount} notification${warnCount === 1 ? "" : "s"}`);
    }
    if (warningsOpen) renderWarningsPopover(state);
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
    container.appendChild(pageHeader("Setup", "Vehicle configuration and calibration"));
    const s = document.createElement("div");
    s.className = "page-section";
    s.appendChild(sectionTitle("Vehicle Info"));
    const state = Corvus.telemetry.getState() || {};
    const card = document.createElement("div");
    card.className = "page-card";
    card.appendChild(row("Autopilot", state.autopilot || "—"));
    card.appendChild(row("Vehicle Type", state.vehicle_type || "—"));
    card.appendChild(row("PX4 Version", state.px4_version || "—"));
    card.appendChild(row("Connected", state.connected ? "Yes" : "No"));
    card.appendChild(row("Armed", state.armed ? "Yes" : "No"));
    s.appendChild(card);
    container.appendChild(s);
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
    container.appendChild(s, s2);
  }

  function renderPlaceholder(container) {
    container.appendChild(pageHeader("Page", "Not yet implemented"));
  }

  function renderWarningsPopover(state, force = false) {
    const notifications = visibleNotifications(state);
    const signature = JSON.stringify(notifications.map((item) => [
      notificationKey(item), item.level, item.msg, item.meta,
    ]));
    if (!force && signature === renderedWarningSignature) return;

    const scrollTop = warningsList.scrollTop;
    const focusedKey = document.activeElement?.closest?.(".wp-item")?.dataset.notificationKey || "";
    renderedWarningSignature = signature;
    warningsList.replaceChildren();
    document.getElementById("warningsTitle").textContent =
      `${notifications.length} Notification${notifications.length === 1 ? "" : "s"}`;

    if (!notifications.length) {
      const empty = document.createElement("div");
      empty.className = "wp-empty";
      empty.textContent = "No notifications.";
      warningsList.appendChild(empty);
    } else {
      notifications.forEach((notification) => {
        const level = notification.level === "error" ? "critical" : (notification.level || "info");
        const item = document.createElement("div");
        item.className = "wp-item";
        item.setAttribute("role", "listitem");
        item.dataset.notificationKey = notificationKey(notification);

        const itemIcon = document.createElement("div");
        itemIcon.className = `wp-icon ${level}`;
        itemIcon.setAttribute("aria-hidden", "true");
        const iconEl = icon(level === "critical" ? "octagon-alert" : level === "warning" ? "triangle-alert" : "info");
        itemIcon.appendChild(iconEl);

        const body = document.createElement("div");
        body.className = "wp-body";
        const message = document.createElement("span");
        message.className = "wp-msg";
        message.textContent = notification.msg || "Unknown notification";
        const meta = document.createElement("span");
        meta.className = "wp-meta";
        meta.textContent = notification.meta || "";
        body.append(message, meta);

        const dismiss = document.createElement("button");
        dismiss.type = "button";
        dismiss.className = "wp-dismiss";
        dismiss.setAttribute("aria-label", `Dismiss ${message.textContent}`);
        dismiss.title = "Dismiss";
        dismiss.appendChild(icon("x"));
        dismiss.addEventListener("click", () => dismissNotification(notificationKey(notification)));
        item.append(itemIcon, body, dismiss);
        warningsList.appendChild(item);
      });
    }
    warningsList.scrollTop = Math.min(scrollTop, warningsList.scrollHeight);
    if (window.lucide && lucide.createIcons) lucide.createIcons({ attrs: { "aria-hidden": "true" } });
    if (focusedKey) {
      const focusedItem = Array.from(warningsList.querySelectorAll(".wp-item"))
        .find((item) => item.dataset.notificationKey === focusedKey);
      (focusedItem?.querySelector(".wp-dismiss") || document.getElementById("wpClose")).focus();
    }
  }

  function setWarningsOpen(open, focusClose = false) {
    warningsOpen = open;
    warningsPopover.hidden = !open;
    const trigger = topBar.querySelector(".tb-block.warnings");
    if (trigger) trigger.setAttribute("aria-expanded", String(open));
    if (open) {
      renderWarningsPopover(lastState || Corvus.telemetry.getState() || {}, true);
      if (focusClose) document.getElementById("wpClose").focus();
    }
  }

  function toggleWarnings() {
    setWarningsOpen(!warningsOpen, !warningsOpen);
  }

  function closeWarnings(restoreFocus = false) {
    setWarningsOpen(false);
    if (restoreFocus) topBar.querySelector(".tb-block.warnings")?.focus();
  }

  function announceNotification(message) {
    notificationLive.textContent = "";
    window.setTimeout(() => { notificationLive.textContent = message; }, 0);
  }

  function refreshNotifications() {
    if (!lastState) return;
    updateTopBarValues(lastState);
  }

  function dismissNotification(key) {
    const dismissButtons = Array.from(warningsList.querySelectorAll(".wp-dismiss"));
    const focusedIndex = dismissButtons.indexOf(document.activeElement);
    if (key.startsWith("local:")) {
      const id = Number(key.slice("local:".length));
      const notification = localNotifications.get(id);
      if (notification?.timer) window.clearTimeout(notification.timer);
      localNotifications.delete(id);
    } else {
      dismissedNotifications.add(key);
    }
    renderedWarningSignature = "";
    refreshNotifications();
    if (focusedIndex >= 0) {
      const remaining = warningsList.querySelectorAll(".wp-dismiss");
      const next = remaining[Math.min(focusedIndex, remaining.length - 1)];
      (next || document.getElementById("wpClose")).focus();
    }
  }

  function removeLocalNotification(id) {
    const notification = localNotifications.get(id);
    if (!notification) return false;
    if (notification.timer) window.clearTimeout(notification.timer);
    localNotifications.delete(id);
    renderedWarningSignature = "";
    return true;
  }

  function addLocalNotification(level, message, lifetimeMs = 8000) {
    const id = ++localNotificationId;
    const notification = {
      localId: id,
      source: "local",
      level,
      msg: String(message || "Command failed"),
      meta: new Date().toLocaleTimeString(),
      timer: null,
    };
    notification.timer = window.setTimeout(() => {
      const focusedItem = document.activeElement?.closest?.(".wp-item");
      const restoreFocus = focusedItem?.dataset.notificationKey === `local:${id}`;
      if (!localNotifications.delete(id)) return;
      renderedWarningSignature = "";
      refreshNotifications();
      if (restoreFocus) document.getElementById("wpClose").focus();
    }, lifetimeMs);
    localNotifications.set(id, notification);
    announceNotification(`${level === "critical" ? "Error" : "Warning"}: ${notification.msg}`);
    setWarningsOpen(true);
    refreshNotifications();
    return id;
  }

  function showCmdError(message, attempt = null) {
    if (!commandDedupe.shouldAddLocal(attempt)) return;
    const notificationId = addLocalNotification("critical", message);
    commandDedupe.attachLocal(attempt, notificationId);
  }

  function completeFailedAttempt(attempt) {
    commandDedupe.failed(attempt);
    window.setTimeout(() => commandDedupe.remove(attempt), commandDedupe.windowMs + 50);
  }

  function syncRemoteWarnings(state) {
    const remote = state?.warnings || [];
    const nextKeys = new Set(remote.map(notificationKey));
    if (!remoteWarningsInitialized) {
      remoteWarningsInitialized = true;
      remoteWarningKeys = nextKeys;
      return;
    }
    const added = remote.filter((warning) => !remoteWarningKeys.has(notificationKey(warning)));
    remoteWarningKeys = nextKeys;
    const duplicateRemoteKeys = new Set();
    added.forEach((warning) => {
      const attempt = commandDedupe.matchRemote(warning);
      if (!attempt?.localNotificationId) return;
      if (removeLocalNotification(attempt.localNotificationId)) duplicateRemoteKeys.add(notificationKey(warning));
    });
    const actionable = added.filter((warning) =>
      !duplicateRemoteKeys.has(notificationKey(warning)) &&
      (warning.level === "critical" || warning.level === "error" || warning.level === "warning"));
    if (!actionable.length) return;
    const newest = actionable[actionable.length - 1];
    announceNotification(`${newest.level === "warning" ? "Warning" : "Error"}: ${newest.msg}`);
    setWarningsOpen(true);
  }

  function handleTelemetryState(state) {
    lastState = state;
    syncRemoteWarnings(state);
    renderTopBar(state);
  }

  function initFlightActions() {
    const btnArm = document.getElementById("btnArm");
    const btnTakeoff = document.getElementById("btnTakeoff");
    const btnLand = document.getElementById("btnLand");
    const btnRTL = document.getElementById("btnRTL");
    const modeSel = document.getElementById("modeSelector");

    btnArm.addEventListener("click", async () => {
      const s = Corvus.telemetry.getState();
      const willArm = !s?.armed;
      const attempt = commandDedupe.begin(willArm ? "arm" : "disarm");
      btnArm.disabled = true;
      try {
        await Corvus.telemetry.postAction("/api/mavlink/arm", { arm: willArm });
        commandDedupe.succeeded(attempt);
        btnArm.classList.toggle("armed", willArm);
      } catch (error) {
        completeFailedAttempt(attempt);
        showCmdError(error.message || (willArm ? "Arm failed" : "Disarm failed"), attempt);
      } finally {
        btnArm.disabled = false;
      }
    });

    btnTakeoff.addEventListener("click", () => {
      const panel = document.getElementById("takeoffPanel");
      panel.hidden = !panel.hidden;
    });

    const takeoffSlider = document.getElementById("takeoffAlt");
    const takeoffAltValue = document.getElementById("takeoffAltValue");
    takeoffSlider.addEventListener("input", () => {
      takeoffAltValue.textContent = takeoffSlider.value + " m";
    });

    document.getElementById("takeoffClose").addEventListener("click", () => {
      document.getElementById("takeoffPanel").hidden = true;
    });

    document.getElementById("takeoffConfirm").addEventListener("click", async () => {
      const alt = parseFloat(takeoffSlider.value);
      const attempt = commandDedupe.begin(["takeoff", "arm"]);
      document.getElementById("takeoffPanel").hidden = true;
      const btn = document.getElementById("takeoffConfirm");
      btn.disabled = true;
      try {
        await Corvus.telemetry.postAction("/api/mavlink/takeoff", { altitude: alt });
        commandDedupe.succeeded(attempt);
      } catch (error) {
        completeFailedAttempt(attempt);
        showCmdError(error.message || "Takeoff rejected by autopilot", attempt);
      } finally {
        btn.disabled = false;
      }
    });

    btnLand.addEventListener("click", async () => {
      const attempt = commandDedupe.begin("land");
      btnLand.disabled = true;
      try {
        await Corvus.telemetry.postAction("/api/mavlink/land", {});
        commandDedupe.succeeded(attempt);
      } catch (error) {
        completeFailedAttempt(attempt);
        showCmdError(error.message || "Land rejected", attempt);
      } finally {
        btnLand.disabled = false;
      }
    });

    btnRTL.addEventListener("click", async () => {
      const attempt = commandDedupe.begin("rtl");
      btnRTL.disabled = true;
      try {
        await Corvus.telemetry.postAction("/api/mavlink/rtl", {});
        commandDedupe.succeeded(attempt);
      } catch (error) {
        completeFailedAttempt(attempt);
        showCmdError(error.message || "RTL rejected", attempt);
      } finally {
        btnRTL.disabled = false;
      }
    });

    modeSel.addEventListener("change", async () => {
      const mode = modeSel.value;
      if (!mode) return;
      const attempt = commandDedupe.begin("mode");
      modeSel.disabled = true;
      try {
        await Corvus.telemetry.postAction("/api/mavlink/mode", { mode });
        commandDedupe.succeeded(attempt);
      } catch (error) {
        completeFailedAttempt(attempt);
        showCmdError(error.message || `Mode ${mode} rejected`, attempt);
      } finally {
        modeSel.disabled = false;
      }
    });

    Corvus.telemetry.requestJson("/api/mavlink/modes").then((data) => {
      refreshModesFromData(data);
    }).catch((error) => showCmdError(error.message || "Could not load flight modes"));

    Corvus.telemetry.subscribe((s) => {
      btnArm.classList.toggle("armed", !!s?.armed);
      const span = btnArm.querySelector("span");
      if (span) span.textContent = s?.armed ? "DISARM" : "ARM";
      const currentMode = s?.mode || "";
      modeSel.value = currentMode;
    });
  }

  /**
   * Repopulate the flight-mode selector from GET /api/mavlink/modes.
   * Idempotent: only re-renders when the mode list signature changes, so a
   * re-fetch triggered on every connect does not thrash the selector or lose
   * the operator's current selection. Exposed for Corvus.link to call after a
   * transition to "connected" so the operator sees exactly the modes the
   * connected firmware supports (PX4 target awareness).
   */
  function refreshModes() {
    return Corvus.telemetry.requestJson("/api/mavlink/modes")
      .then(refreshModesFromData)
      .catch(() => { /* best-effort: keep the existing selector as-is */ });
  }

  function refreshModesFromData(data) {
    const modes = (data && data.modes) || [];
    const sig = modes.join(",");
    if (sig === loadedModesSignature) return;   // idempotent
    loadedModesSignature = sig;
    const modeSel = document.getElementById("modeSelector");
    if (!modeSel) return;
    const current = modeSel.value;
    // Keep only the placeholder option; drop previously loaded modes.
    Array.from(modeSel.querySelectorAll("option:not([value=''])")).forEach((o) => o.remove());
    modes.forEach((m) => {
      const opt = document.createElement("option");
      opt.value = m;
      opt.textContent = m;
      modeSel.appendChild(opt);
    });
    // Restore the selection if the firmware still offers it.
    if (current && Array.from(modeSel.options).some((o) => o.value === current)) {
      modeSel.value = current;
    }
  }

  function init() {
    topBar = document.getElementById("topBar");
    leftNav = document.getElementById("leftNav");
    warningsPopover = document.getElementById("warningsPopover");
    warningsList = document.getElementById("warningsList");
    notificationLive = document.getElementById("notificationLive");
    mapView = document.getElementById("mapView");
    pageView = document.getElementById("pageView");

    renderLeftNav();
    Corvus.telemetry.subscribe(handleTelemetryState);
    warningsList.setAttribute("role", "list");
    document.getElementById("wpClose").addEventListener("click", () => closeWarnings(true));

    document.addEventListener("click", (e) => {
      if (warningsOpen && !e.target.closest(".warnings-popover") && !e.target.closest(".tb-block.warnings")) {
        closeWarnings();
      }
    });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && warningsOpen) {
        event.preventDefault();
        closeWarnings(true);
      }
    });
    window.addEventListener("corvus:notification", (event) => {
      const detail = event.detail || {};
      addLocalNotification(detail.level || "critical", detail.message || "Application error");
    });

    Corvus.map.init(document.getElementById("map"), document.getElementById("mapControls"),
      document.getElementById("layersPopover"));
    Corvus.instruments.init(document.getElementById("flightOverlay"));
    Corvus.panel.init();
    Corvus.link.init();
    initFlightActions();

    Corvus.telemetry.connect();
    if (window.lucide && lucide.createIcons) lucide.createIcons();

    Corvus.telemetry.requestJson("/api/version").then((v) => {
      document.title = `CORVUS GCS v${v.version}`;
    }).catch(() => {});

    let userToggled = false;
    document.getElementById("panelHandle").addEventListener("click", () => { userToggled = true; });
    window.addEventListener("resize", () => {
      if (userToggled) return;
      const panel = document.getElementById("rightPanel");
      const collapsed = panel.classList.contains("collapsed");
      if (window.innerWidth < 960 && !collapsed) Corvus.panel.toggle();
      else if (window.innerWidth >= 1280 && collapsed) Corvus.panel.toggle();
    });
    window.dispatchEvent(new Event("resize"));
  }

  return { init, notifyError: showCmdError, refreshModes };
})();

document.addEventListener("DOMContentLoaded", Corvus.app.init);
