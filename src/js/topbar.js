"use strict";
window.Corvus = window.Corvus || {};

Corvus.topbar = (function () {
  let topBar, warningsPopover, warningsList, notificationLive;
  // block key -> {root, vMain, sub, dot, value}, cached once at first build so
  // updateTopBarValues does not re-query the bar on every telemetry tick.
  let blockEls = {};
  let warningsOpen = false;
  let lastState = null;
  let renderedWarningSignature = "";
  let remoteWarningsInitialized = false;
  let remoteWarningKeys = new Set();
  let localNotificationId = 0;
  const localNotifications = new Map();
  const dismissedNotifications = new Set();
  const commandDedupe = Corvus.notificationDedupe.createTracker({ windowMs: 3000 });

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

  function vehicleFirmware(state) {
    if (!state.connected) return { text: "", cls: "", title: "" };
    if (state.px4_version) {
      const detail = state.px4_version_detail ? ` \u00b7 ${state.px4_version_detail}` : "";
      return { text: state.px4_version + detail, cls: "", title: "" };
    }
    if (state.autopilot) {
      return {
        text: "\u2014",
        cls: "undetected",
        title: `${state.autopilot} firmware version undetected (AUTOPILOT_VERSION not provided by this firmware)`,
      };
    }
    return { text: "", cls: "", title: "" };
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
    const fw = vehicleFirmware(state);

    return [
      { type: "logo" },
      { key: "vehicle", label: "Vehicle", value: vehicleLabel(state), dot: conn, sub: fw.text, subCls: fw.cls, title: fw.title, priority: "high" },
      { key: "mode", label: "Mode", value: modeLabel(state), cls: "accent", align: true, priority: "high" },
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
    // Undotted but aligned blocks (e.g. Mode) get a placeholder occupying the
    // dot slot so their value text starts at the same x as dotted blocks.
    const alignHtml = !b.dot && b.align ? `<span class="tb-dot-placeholder"></span>` : "";
    const iconHtml = b.icon ? `<i data-lucide="${b.icon}" class="tb-icon"></i>` : "";
    const subHtml = b.sub !== undefined ? `<span class="sub${b.subCls ? " " + b.subCls : ""}">${b.sub}</span>` : "";
    const valCls = b.cls ? ` ${b.cls}` : "";
    blk.innerHTML =
      `<span class="tb-label">${iconHtml}${b.label}</span>` +
      `<span class="tb-value${valCls}">${alignHtml}${dotHtml}<span class="v-main"></span>${subHtml}</span>`;
    blk.querySelector(".v-main").textContent = b.value;
    if (b.title) blk.title = b.title;
    return blk;
  }

  let topBarBuilt = false;

  function renderTopBar(state) {
    if (!topBarBuilt) {
      const blkData = blocks(state);
      topBar.innerHTML = "";
      blkData.forEach((b) => topBar.appendChild(renderBlock(b)));
      const spacer = document.createElement("div");
      spacer.className = "tb-spacer";
      topBar.appendChild(spacer);
      if (window.lucide && lucide.createIcons) lucide.createIcons();
      // Cache each value block's sub-element refs once: the bar is built only
      // once per session (topBarBuilt), so these refs stay valid for every
      // later update. If the bar were ever rebuilt, this must re-run — it is
      // inside the (!topBarBuilt) branch on purpose.
      blockEls = {};
      blkData.forEach((b) => {
        if (!b.key || b.type === "warnings") return;
        const root = topBar.querySelector(`[data-block="${b.key}"]`);
        if (!root) return;
        blockEls[b.key] = {
          root,
          vMain: root.querySelector(".v-main"),
          sub: root.querySelector(".sub"),
          dot: root.querySelector(".tb-dot"),
          value: root.querySelector(".tb-value"),
        };
      });
      topBarBuilt = true;
      updateTopBarValues(state);
    } else {
      updateTopBarValues(state);
    }
  }

  function updateTopBarValues(state) {
    const blkData = blocks(state);
    blkData.forEach((b) => {
      if (!b.key || b.type === "warnings") return;
      const cached = blockEls[b.key];
      if (!cached) return;
      const el = cached.root;
      const valEl = cached.vMain;
      if (valEl && valEl.textContent !== b.value) valEl.textContent = b.value;
      const subEl = cached.sub;
      if (subEl && b.sub !== undefined) subEl.textContent = b.sub;
      if (subEl && b.subCls !== undefined) subEl.className = "sub" + (b.subCls ? " " + b.subCls : "");
      if (b.title !== undefined) el.title = b.title || "";
      const dotEl = cached.dot;
      if (dotEl && b.dot) {
        dotEl.className = "tb-dot " + b.dot;
      }
      const valSpan = cached.value;
      if (valSpan) valSpan.className = "tb-value" + (b.cls ? " " + b.cls : "");
    });
    // Warnings block has a distinct structure (badge, not v-main/sub); keep
    // its single per-update query as-is rather than over-caching it.
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

    // Toggle the "clear all" button: disabled when empty, enabled when there
    // are notifications to clear. Guarded so a missing button never breaks the
    // popover render (e.g. if the header markup changes).
    const clearAllBtn = document.getElementById("wpClearAll");
    if (clearAllBtn) {
      const empty = notifications.length === 0;
      clearAllBtn.disabled = empty;
      clearAllBtn.setAttribute("aria-disabled", String(empty));
    }

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

  // Best-effort "clear all": ask the backend to drop its warnings store (the
  // next telemetry push then sends an empty `warnings` array) and clear every
  // local notification + the dismissed-key set. Remote warnings are NOT
  // mutated client-side — they disappear via the next SSE push.
  async function clearAllNotifications() {
    try {
      await Corvus.telemetry.postAction("/api/warnings/clear", {});
    } catch (err) {
      console.error("clear warnings failed:", err);
    }
    localNotifications.forEach((notification) => {
      if (notification.timer) window.clearTimeout(notification.timer);
    });
    localNotifications.clear();
    dismissedNotifications.clear();
    renderedWarningSignature = "";
    refreshNotifications();
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

  function init() {
    topBar = document.getElementById("topBar");
    warningsPopover = document.getElementById("warningsPopover");
    warningsList = document.getElementById("warningsList");
    notificationLive = document.getElementById("notificationLive");

    Corvus.telemetry.subscribe(handleTelemetryState);
    warningsList.setAttribute("role", "list");
    document.getElementById("wpClose").addEventListener("click", () => closeWarnings(true));
    const wpClearAll = document.getElementById("wpClearAll");
    if (wpClearAll) wpClearAll.addEventListener("click", clearAllNotifications);

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
  }

  return {
    init,
    notifyError: showCmdError,
    beginCommand: (key) => commandDedupe.begin(key),
    succeedCommand: (attempt) => commandDedupe.succeeded(attempt),
    failCommand: completeFailedAttempt,
  };
})();
