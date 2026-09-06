"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.topbar — the vehicle status bar and the notification centre behind it.

  Notifications come from two places and are treated as one list:

    remote   PX4 STATUSTEXT, merged into the backend warning store and pushed
             down with every telemetry snapshot. The store is the owner; the
             UI can only hide them locally or ask the backend to drop the lot.
    local    a command this GCS dispatched and the vehicle refused. Created
             here, deduplicated against the remote failure that usually
             follows (see Corvus.notificationDedupe).

  Each notification is in one of three states, and the distinction is what
  keeps the badge honest:

    unread     never been looked at — this is what the badge counts
    read       still on the board, dimmed; the operator has seen it
    dismissed  gone from the list (per item, or via "clear all")

  Nothing warning-level or worse is ever deleted by a timer. Acknowledging a
  warning is the operator's call; a notification centre that quietly drops the
  one thing that mattered is worse than one that shows too much. What DOES age
  out is info — PX4's routine chatter, which is already in the CONSOLE tab
  verbatim and only inflates the badge here.

  Read is set at two moments, both of them "you have had your chance to look":
  closing the popover, and any change of the armed state — arming means every
  preflight complaint on the board is moot, disarming means the flight's
  messages are history.

  Two events go further and clear the board outright, because everything on it
  belongs to a session that has ended: a fresh link, and an autopilot reboot.
  See applyLifecycleMilestones for why a disconnect is neither.
*/
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
  // Operator-supplied company logo: the display filename from the backend
  // config, or "" for the default (no company logo at all). Held here because
  // the config lands before the bar is built on the first telemetry state.
  let companyLogo = "";
  const localNotifications = new Map();
  const dismissedNotifications = new Set();
  const readNotifications = new Set();
  // key -> ms first observed, so info can age out. Pruned with the sets above.
  const notificationFirstSeen = new Map();
  // How long a routine info line stays on the board. Warning and critical have
  // no equivalent — see the module comment.
  const INFO_TTL_MS = 60000;
  // Vehicle state at the previous telemetry push, for the transitions that
  // drive the read/clear milestones. null until the first snapshot arrives, so
  // a page opened against an already-flying vehicle triggers nothing.
  let prevConnected = null;
  let prevArmed = null;
  // Autopilot uptime at the previous push; a value that goes backwards is a
  // reboot. 0 until the vehicle reports one.
  let prevBootMs = 0;
  const commandDedupe = Corvus.notificationDedupe.createTracker({ windowMs: 3000 });

  /* Top-bar icons are sized by main.css, so they are built without an inline
     size ("auto") — an inline width/height would override those rules. */
  function icon(name) { return Corvus.ui.icon(name, "auto"); }

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

  /** PX4 calls it "error", the popover draws it as "critical" — one spelling. */
  function notificationLevel(notification) {
    const level = notification?.level || "info";
    return level === "error" ? "critical" : level;
  }

  /**
   * Every notification currently on the board, remote and local, minus the
   * dismissed and the aged-out.
   *
   * Also where the three per-key stores are pruned. A remote warning that has
   * left the backend store can never come back under the same key (its `meta`
   * timestamp is part of the key), so holding "dismissed" or "read" for it
   * forever would be a slow leak — and, worse, would silently suppress a
   * genuinely new warning that happened to reuse the key.
   */
  function visibleNotifications(state) {
    const remote = (state?.warnings || []).map((warning) => ({ ...warning, source: "remote" }));
    const all = remote.concat(Array.from(localNotifications.values()));
    const activeKeys = new Set(all.map(notificationKey));

    [dismissedNotifications, readNotifications].forEach((set) => {
      Array.from(set).forEach((key) => { if (!activeKeys.has(key)) set.delete(key); });
    });
    Array.from(notificationFirstSeen.keys()).forEach((key) => {
      if (!activeKeys.has(key)) notificationFirstSeen.delete(key);
    });

    const now = Date.now();
    return all.filter((notification) => {
      const key = notificationKey(notification);
      if (dismissedNotifications.has(key)) return false;
      if (!notificationFirstSeen.has(key)) notificationFirstSeen.set(key, now);
      // Info only. A warning or a critical stays until somebody acts on it.
      if (notificationLevel(notification) !== "info") return true;
      return now - notificationFirstSeen.get(key) < INFO_TTL_MS;
    });
  }

  /**
   * What the badge has to say, in one place, because the bar and the popover
   * were computing it twice and could disagree.
   *
   * The count is UNREAD while anything is unread, and the total once it has
   * all been seen — so a board of acknowledged warnings still says it is not
   * empty, without pretending they are new. `level` follows the same split:
   * the worst unread level, or "read" for a board that has been looked at.
   */
  function notificationSummary(state) {
    const items = visibleNotifications(state);
    const unread = items.filter((item) => !readNotifications.has(notificationKey(item)));
    let level = "healthy";
    if (unread.length) {
      level = unread.some((item) => notificationLevel(item) === "critical") ? "critical" : "warning";
    } else if (items.length) {
      level = "read";
    }
    return { items, unread, count: unread.length || items.length, level };
  }

  /** Everything on the board has been seen. Never deletes — see the module
   *  comment; this is the "hide as read" half of the contract. */
  function markAllNotificationsRead(state) {
    const items = visibleNotifications(state || lastState || {});
    let changed = false;
    items.forEach((item) => {
      const key = notificationKey(item);
      if (!readNotifications.has(key)) { readNotifications.add(key); changed = true; }
    });
    if (changed) {
      renderedWarningSignature = "";
      refreshNotifications();
    }
    return changed;
  }

  function blocks(state) {
    const conn = state.connected ? "healthy" : "off";
    const armed = state.armed ? "ARMED" : "DISARMED";
    const armedCls = state.armed ? "healthy" : "off";
    const gpsCls = state.connected && state.gps_fix && state.gps_fix !== "NO_GPS" && state.gps_fix !== "NO_FIX"
      ? "healthy" : "off";
    const battPct = state.battery_percent || 0;
    const battCls = !state.connected ? "off" : battPct > 25 ? "healthy" : (battPct > 12 ? "warning" : "critical");
    const notifications = notificationSummary(state);
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
      { key: "warnings", type: "warnings", label: "Warnings", value: notifications.count, level: notifications.level, priority: "high" },
    ];
  }

  // Point the company-logo <img> at the stored PNG, or hide it. Kept separate
  // from renderLogo so a settings change re-skins the live bar without the
  // rebuild that topBarBuilt deliberately prevents. The cache-buster is what
  // makes a replaced logo appear immediately.
  function applyCompanyLogo() {
    const img = topBar && topBar.querySelector(".tb-company-logo");
    if (!img) return;
    if (companyLogo) {
      img.alt = companyLogo;
      img.src = `/api/branding/logo?v=${Date.now()}`;
      img.hidden = false;
    } else {
      img.hidden = true;
      img.removeAttribute("src");
    }
  }

  function setCompanyLogo(name) {
    companyLogo = name || "";
    applyCompanyLogo();
  }

  // Optional operator branding at the far right of the bar. Always built (so
  // applyCompanyLogo has something to point at) but hidden until a logo is
  // configured — no company logo ships by default, and the Corvus mark keeps
  // the left end of the bar either way. A file that fails to decode hides
  // itself rather than leaving a broken-image box in the bar.
  function renderCompanyLogo() {
    const img = document.createElement("img");
    img.className = "tb-company-logo";
    img.hidden = true;
    img.alt = "";
    img.addEventListener("error", () => { img.hidden = true; });
    return img;
  }

  function renderLogo() {
    const w = document.createElement("div");
    w.className = "tb-logo";
    // The mark is a background image driven by the --logo-mark token, not an
    // <img src>: the logo ships as white artwork for dark surfaces and black
    // for light ones, and CSS cannot swap an element's src. role="img" +
    // aria-label keeps it announced exactly as the <img alt> was.
    const mark = document.createElement("div");
    mark.className = "tb-logo-mark";
    mark.setAttribute("role", "img");
    mark.setAttribute("aria-label", "CORVUS GCS");
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
      // After the flex:1 spacer, so operator branding sits hard against the
      // right edge and never competes with the Corvus mark on the left.
      topBar.appendChild(renderCompanyLogo());
      Corvus.ui.refreshIcons();
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
      applyCompanyLogo();
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
      const { items, unread, count, level } = notificationSummary(state);
      warnEl.textContent = String(count);
      // The badge colour is a token, set from CSS off this attribute. It used
      // to be three hex literals written straight onto style.background, which
      // meant the one element in the bar that never changed with the theme was
      // the one shouting loudest.
      warnEl.dataset.level = level;
      const valSpan = warnEl.parentElement;
      if (valSpan) valSpan.className = `tb-value ${level}`;
      const warningButton = warnEl.closest(".tb-block.warnings");
      if (warningButton) {
        const plural = (n) => `${n} notification${n === 1 ? "" : "s"}`;
        warningButton.setAttribute("aria-label", unread.length
          ? `${unread.length} unread of ${plural(items.length)}`
          : plural(items.length));
      }
    }
    if (warningsOpen) renderWarningsPopover(state);
  }

  function renderWarningsPopover(state, force = false) {
    const { items: notifications, unread } = notificationSummary(state);
    const unreadKeys = new Set(unread.map(notificationKey));
    const signature = JSON.stringify(notifications.map((item) => [
      notificationKey(item), item.level, item.msg, item.meta,
      unreadKeys.has(notificationKey(item)),
    ]));
    if (!force && signature === renderedWarningSignature) return;

    const scrollTop = warningsList.scrollTop;
    const focusedKey = document.activeElement?.closest?.(".wp-item")?.dataset.notificationKey || "";
    renderedWarningSignature = signature;
    warningsList.replaceChildren();
    // "5 Notifications · 2 new" rather than a separate pill, so the popover's
    // accessible name carries the unread count too.
    const total = `${notifications.length} Notification${notifications.length === 1 ? "" : "s"}`;
    document.getElementById("warningsTitle").textContent =
      unread.length ? `${total} \u00b7 ${unread.length} new` : total;

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
        const level = notificationLevel(notification);
        const key = notificationKey(notification);
        const isRead = !unreadKeys.has(key);
        const item = document.createElement("div");
        // Read is a dimming, not a removal: the line stays exactly where it
        // was so the board reads as a log rather than a queue that empties
        // itself while the operator is looking at it.
        item.className = isRead ? "wp-item is-read" : "wp-item";
        item.dataset.level = level;
        item.setAttribute("role", "listitem");
        item.dataset.notificationKey = key;

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
    Corvus.ui.refreshIcons({ attrs: { "aria-hidden": "true" } });
    if (focusedKey) {
      const focusedItem = Array.from(warningsList.querySelectorAll(".wp-item"))
        .find((item) => item.dataset.notificationKey === focusedKey);
      (focusedItem?.querySelector(".wp-dismiss") || document.getElementById("wpClose")).focus();
    }
  }

  /* Closing the popover is what marks the board read — not opening it. An
     operator who opens the centre because something just arrived should see
     that thing arrive as new, and anything that lands WHILE they are reading
     stays new too; the badge settles the moment they look away. */
  function setWarningsOpen(open, focusClose = false) {
    const wasOpen = warningsOpen;
    warningsOpen = open;
    warningsPopover.hidden = !open;
    const trigger = topBar.querySelector(".tb-block.warnings");
    if (trigger) trigger.setAttribute("aria-expanded", String(open));
    if (open) {
      renderWarningsPopover(lastState || Corvus.telemetry.getState() || {}, true);
      if (focusClose) document.getElementById("wpClose").focus();
    } else if (wasOpen) {
      markAllNotificationsRead(lastState);
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
      localNotifications.delete(Number(key.slice("local:".length)));
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

  /**
   * Empty the board: drop every local notification and ask the backend to drop
   * its warning store.
   *
   * The remote half is hidden here as well as at the source. The store is the
   * owner and its next push is what really removes them, but waiting a round
   * trip for a button press to do anything is what made "clear all" feel
   * broken; the dismissed keys prune themselves the moment that push lands
   * (see visibleNotifications), so this is a bridge rather than a second
   * source of truth. A failed POST leaves them hidden — the same outcome as
   * dismissing each by hand, which is what the operator asked for.
   */
  async function clearAllNotifications() {
    (lastState?.warnings || []).forEach((warning) =>
      dismissedNotifications.add(notificationKey(warning)));
    localNotifications.clear();
    readNotifications.clear();
    notificationFirstSeen.clear();
    renderedWarningSignature = "";
    refreshNotifications();
    try {
      await Corvus.telemetry.postAction("/api/warnings/clear", {});
    } catch (err) {
      console.error("clear warnings failed:", err);
    }
  }

  function removeLocalNotification(id) {
    if (!localNotifications.delete(id)) return false;
    renderedWarningSignature = "";
    return true;
  }

  /* A local notification is a command this GCS sent and the vehicle refused.
     It used to delete itself after eight seconds, which is the one thing a
     rejected command must not do — an operator who looked away missed it, and
     the board then claimed nothing had gone wrong. It now lives by the same
     rule as everything else: info ages out, warning and critical stay until
     acknowledged (see visibleNotifications). */
  function addLocalNotification(level, message) {
    const id = ++localNotificationId;
    const notification = {
      localId: id,
      source: "local",
      level,
      msg: String(message || "Command failed"),
      meta: new Date().toLocaleTimeString(),
    };
    localNotifications.set(id, notification);
    announceNotification(`${level === "critical" ? "Error" : "Warning"}: ${notification.msg}`);
    // Only a critical takes the screen. A rejected command is one the operator
    // just issued and is waiting on, so it earns the interruption.
    if (notificationLevel(notification) === "critical") setWarningsOpen(true);
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
      notificationLevel(warning) !== "info");
    if (!actionable.length) return;
    const newest = actionable[actionable.length - 1];
    announceNotification(`${notificationLevel(newest) === "warning" ? "Warning" : "Error"}: ${newest.msg}`);
    // Only a critical takes the screen. PX4 emits NOTICE-level lines through
    // the whole of a normal flight ("Takeoff detected", "RTL: land at home"),
    // and a popover unfolding over the map for each of them trained operators
    // to dismiss the centre without reading it. Everything else raises the
    // badge and is announced to assistive tech, which is what a warning is for.
    if (actionable.some((warning) => notificationLevel(warning) === "critical")) {
      setWarningsOpen(true);
    }
  }

  /**
   * The vehicle-state transitions the notification board reacts to.
   *
   * A fresh link, or an autopilot reboot, CLEARS. Whatever is on the board
   * predates it, so it describes a previous session — possibly a different
   * aircraft, certainly a different power cycle. Nothing is lost: the console
   * holds every line verbatim, which is what makes clearing here safe at all.
   * (A reboot is read the way the map reads it: boot_ms running backwards.)
   *
   * A change of the armed state MARKS READ. Arming means every preflight
   * complaint on the board has been answered — the vehicle would not have
   * armed otherwise — and disarming means the flight's messages are history.
   * Read, not cleared: "it armed anyway" is not proof a warning was
   * unimportant, so it stays on the board, just no longer shouting.
   *
   * The arm milestone needs the link up on BOTH sides of the change, because
   * a disconnect synthesises armed=false (state_store.set_disconnected) — and
   * a dropped link is the last moment at which warnings should go quiet.
   *
   * Every milestone is skipped on the first snapshot. A page opened against a
   * vehicle that is already connected and armed has observed no transition,
   * and must not silently wipe a board the operator has not seen yet.
   */
  function applyLifecycleMilestones(state) {
    const connected = !!state?.connected;
    const armed = !!state?.armed;
    const bootMs = Number(state?.boot_ms) || 0;
    const rebooted = prevBootMs > 0 && bootMs > 0 && bootMs < prevBootMs;

    if ((prevConnected === false && connected) || rebooted) {
      clearAllNotifications();
    } else if (prevArmed !== null && prevArmed !== armed && connected && prevConnected) {
      markAllNotificationsRead(state);
    }
    prevConnected = connected;
    prevArmed = armed;
    if (bootMs > 0) prevBootMs = bootMs;
  }

  function handleTelemetryState(state) {
    lastState = state;
    syncRemoteWarnings(state);
    applyLifecycleMilestones(state);
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
    setCompanyLogo,
    notifyError: showCmdError,
    beginCommand: (key) => commandDedupe.begin(key),
    succeedCommand: (attempt) => commandDedupe.succeeded(attempt),
    failCommand: completeFailedAttempt,
  };
})();
