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
  // Whether the small coloured state dots ride along with the block captions.
  // Off unless the operator asks for them: every block that carries a dot also
  // paints its own value with the same state (a red "0.0 V", a grey "NO GPS"),
  // so on a normal bar the dot is the same sentence said twice. Operators who
  // scan the bar by colour alone can turn them back on in Settings.
  // Cached in localStorage so the bar is right before the config lands, and
  // written as an attribute on <html> because the hiding itself is CSS — see
  // the .tb-dot rules in main.css.
  const DOTS_KEY = "corvus.topbarDots";
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

  /* The bar names the airframe the way an operator does, not the way MAVLINK
     enumerates it: MAV_TYPE_QUADROTOR reads "Quadcopter". Only the display
     changes — the raw enum name stays in telemetry state, where the parameter
     export and the setup page rely on it. */
  const VEHICLE_TYPE_LABELS = {
    GENERIC: "Generic",
    FIXED_WING: "Fixed Wing",
    QUADROTOR: "Quadcopter",
    COAXIAL: "Coaxial Helicopter",
    HELICOPTER: "Helicopter",
    AIRSHIP: "Airship",
    FREE_BALLOON: "Balloon",
    ROCKET: "Rocket",
    GROUNDED_ROVER: "Rover",
    SURFACE_BOAT: "Boat",
    SUBMARINE: "Submarine",
    HEXAROTOR: "Hexacopter",
    OCTOROTOR: "Octocopter",
    TRIROTOR: "Tricopter",
    VTOL_DUOROTOR: "VTOL Duorotor",
    VTOL_QUADROTOR: "VTOL Quadrotor",
    VTOL_TILTROTOR: "VTOL Tiltrotor",
  };

  /* Anything outside the table — a frame this build predates — is titled from
     its enum name rather than shouted: "TYPE_23" stays "Type 23". */
  function vehicleTypeLabel(raw) {
    if (!raw) return "Unknown";
    if (VEHICLE_TYPE_LABELS[raw]) return VEHICLE_TYPE_LABELS[raw];
    return raw.toLowerCase().split("_").filter(Boolean)
      .map((w) => w.charAt(0).toUpperCase() + w.slice(1)).join(" ");
  }

  function vehicleLabel(state) {
    if (!state.connected) return "DISCONNECTED";
    const type = vehicleTypeLabel(state.vehicle_type);
    const ap = state.autopilot || "";
    return ap ? `${ap} ${type}` : type;
  }

  function vehicleFirmware(state) {
    if (!state.connected) return { text: "", cls: "", title: "" };
    /* Release version only. The git hash the firmware also reports identifies
       the exact build, which matters when filing a bug and never in flight, so
       it lives in the tooltip instead of the bar. */
    if (state.px4_version) {
      const detail = state.px4_version_detail
        ? `Build ${state.px4_version_detail}` : "";
      return { text: state.px4_version, cls: "", title: detail };
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

  /* MAV_LANDED_STATE, as EXTENDED_SYS_STATE reports it. */
  const LANDED_UNDEFINED = 0, LANDED_ON_GROUND = 1;
  const LANDED_IN_AIR = 2, LANDED_TAKEOFF = 3, LANDED_LANDING = 4;
  /* Height above home that counts as airborne when the firmware does not
     publish EXTENDED_SYS_STATE. Deliberately well clear of the noise on a
     stationary vehicle's altitude estimate. */
  const AIRBORNE_FALLBACK_M = 1.5;

  /** Is the aircraft off the ground? The vehicle's answer where there is one. */
  function isAirborne(state) {
    const landed = state.landed_state;
    if (landed === LANDED_IN_AIR || landed === LANDED_TAKEOFF || landed === LANDED_LANDING) return true;
    if (landed === LANDED_ON_GROUND) return false;
    // UNDEFINED, or a firmware that never sends it: fall back to height.
    return Number(state.altitude_agl || 0) > AIRBORNE_FALLBACK_M;
  }

  /**
   * The readiness block: what the operator actually needs from the bar, which
   * is not "is the switch on" (the ARM button on the flight page already says
   * that) but "can this thing fly, and is it flying".
   *
   *   FLYING     armed and off the ground. This used to read ARMED for the
   *              whole flight, which is the least informative moment to be
   *              told about a switch: of course it is armed, it is in the air.
   *   ARMED      armed, still on the ground. The one moment the word earns its
   *              place — the propellers are live and the aircraft is within
   *              reach of somebody — so it keeps it, and it is the only state
   *              here drawn in the attention colour.
   *   READY      the autopilot's own preflight check passes: arming would be
   *              accepted right now
   *   NOT READY  the autopilot refuses to arm; the reason is a PX4
   *              "Preflight Fail" STATUSTEXT, so it is already in the
   *              notification centre next door
   *   STANDBY    firmware that does not publish MAV_SYS_STATUS_PREARM_CHECK,
   *              or nothing received yet. We know the switch is off and
   *              nothing more, so we claim nothing more — READY here would be
   *              a clearance the vehicle never gave.
   *   —          no link
   */
  function readiness(state) {
    if (!state.connected) return { value: "—", cls: "off", tone: "none", title: "No link to a vehicle" };
    if (state.armed) {
      if (isAirborne(state)) {
        return { value: "FLYING", cls: "nav", tone: "flying", title: "Airborne — motors are live" };
      }
      return {
        value: "ARMED",
        cls: "armed",
        tone: "armed",
        title: "Armed on the ground — propellers are live",
      };
    }
    if (state.prearm_ok === true) {
      return {
        value: "READY",
        cls: "healthy",
        tone: "ready",
        title: "Preflight checks pass — the vehicle would accept an arm command",
      };
    }
    if (state.prearm_ok === false) {
      return {
        value: "NOT READY",
        cls: "warning",
        tone: "notready",
        title: "The autopilot is refusing to arm — see the notifications for the failing check",
      };
    }
    return {
      value: "STANDBY",
      cls: "off",
      tone: "none",
      title: "Disarmed. This firmware does not report its preflight-check state, so readiness is unknown.",
    };
  }

  function modeLabel(state) {
    if (!state.connected) return "—";
    if (state.armed && !state.mode) return isAirborne(state) ? "FLYING" : "ARMED";
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
   *
   * "Worst" is now actually worst. This used to be a two-way split — anything
   * unread that was not critical was painted "warning" — so a single
   * informational line ("Takeoff target accepted", "Mode accepted: MISSION")
   * turned the badge amber. A normal flight produces a steady trickle of
   * those, which meant the bar spent the flight looking like something was
   * wrong, and an operator who learns that amber means nothing has been taught
   * to ignore the one colour that has to keep working.
   */
  function notificationSummary(state) {
    const items = visibleNotifications(state);
    const unread = items.filter((item) => !readNotifications.has(notificationKey(item)));
    let level = "healthy";
    if (unread.length) {
      const levels = unread.map(notificationLevel);
      if (levels.includes("critical")) level = "critical";
      else if (levels.includes("warning")) level = "warning";
      else level = "info";
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
    const conn = state.connected ? "healthy" : "critical";
    const ready = readiness(state);
    const gpsCls = state.connected && state.gps_fix && state.gps_fix !== "NO_GPS" && state.gps_fix !== "NO_FIX"
      ? "healthy" : "off";
    const battPct = state.battery_percent || 0;
    const battCls = !state.connected ? "off" : battPct > 25 ? "healthy" : (battPct > 12 ? "warning" : "critical");
    const notifications = notificationSummary(state);
    const gpsSub = state.connected ? `(${state.gps_hdop > 0 && state.gps_hdop < 99 ? state.gps_hdop.toFixed(1) : "—"})` : "";
    const fw = vehicleFirmware(state);

    return [
      { type: "logo" },
      { key: "vehicle", label: "Vehicle", value: vehicleLabel(state), cls: state.connected ? "" : "critical", dot: conn, sub: fw.text, subCls: fw.cls, title: fw.title, priority: "high" },
      { key: "mode", label: "Mode", value: modeLabel(state), cls: "accent", priority: "high" },
      { key: "armed", label: "Status", value: ready.value, cls: ready.cls, dot: ready.cls, tone: ready.tone, title: ready.title, priority: "high" },
      { key: "gps", label: "GPS", value: state.connected ? (state.gps_fix || "NO GPS") : "—", sub: gpsSub, cls: gpsCls, dot: gpsCls, priority: "high" },
      { key: "battery", label: "Battery", value: state.connected ? `${state.battery_voltage.toFixed(1)} V` : "—", sub: state.connected ? `${battPct}%` : "", cls: battCls, dot: battCls, priority: "high" },
      { key: "altitude", label: "Altitude", value: state.connected ? `${Math.round(state.altitude_amsl)}` : "—", sub: "m AMSL", priority: "mid" },
      { key: "groundspeed", label: "Groundspeed", value: state.connected ? `${state.groundspeed.toFixed(1)}` : "—", sub: "m/s", priority: "mid" },
      { key: "vspeed", label: "Vertical speed", value: state.connected ? `${state.vspeed >= 0 ? "+" : ""}${state.vspeed.toFixed(1)}` : "—", sub: "m/s", priority: "mid" },
      { key: "warnings", type: "warnings", label: "Notifications", value: notifications.count, level: notifications.level, priority: "high" },
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

  /**
   * Show or hide the caption status dots. Returns the state applied, so a
   * caller can undo itself with the return value rather than its own copy.
   */
  function setStatusDots(on) {
    const v = !!on;
    try { document.documentElement.setAttribute("data-topbar-dots", v ? "on" : "off"); } catch (_e) {}
    try { localStorage.setItem(DOTS_KEY, v ? "1" : "0"); } catch (_e) {}
    return v;
  }

  /** Whether the dots are currently shown. */
  function statusDots() {
    try { return document.documentElement.getAttribute("data-topbar-dots") === "on"; }
    catch (_e) { return false; }
  }

  /** Apply the locally cached choice (called before the config fetch lands).
   *  Absent storage means off — the default is no dots, not "whatever the
   *  last machine did". */
  function applySavedStatusDots() {
    let v = false;
    try { v = localStorage.getItem(DOTS_KEY) === "1"; } catch (_e) {}
    return setStatusDots(v);
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
        `<span class="tb-label">${b.label}</span>` +
        `<span class="tb-value"><span class="tb-warn-pill">` +
        `<span class="tb-warn-count"></span>` +
        `<i data-lucide="message-square" class="tb-icon tb-warn-icon" aria-hidden="true"></i>` +
        `</span></span>`;
      blk.addEventListener("click", toggleWarnings);
      return blk;
    }
    const blk = document.createElement("div");
    blk.className = "tb-block";
    blk.dataset.priority = b.priority;
    blk.dataset.block = b.key;
    // The status dot rides with the LABEL, not the value. In front of the
    // value it read as a bullet, it sat immediately beside a value already
    // painted the same colour (a red dot against a red "0.0 V" is one signal
    // said twice), and — because only some blocks have one — it indented
    // those values 12px past the undotted ones. That indent is why an
    // invisible placeholder used to exist for Mode; with the dot up in the
    // quiet uppercase caption, every value in the bar starts at the same x
    // on its own and the placeholder is gone.
    const dotHtml = b.dot ? `<span class="tb-dot ${b.dot}"></span>` : "";
    const iconHtml = b.icon ? `<i data-lucide="${b.icon}" class="tb-icon"></i>` : "";
    const subHtml = b.sub !== undefined ? `<span class="sub${b.subCls ? " " + b.subCls : ""}">${b.sub}</span>` : "";
    const valCls = b.cls ? ` ${b.cls}` : "";
    blk.innerHTML =
      `<span class="tb-label">${iconHtml}${b.label}${dotHtml}</span>` +
      `<span class="tb-value${valCls}"><span class="v-main"></span>${subHtml}</span>`;
    blk.querySelector(".v-main").textContent = b.value;
    // The tone is what lets a block say something with more than a text
    // colour — see the [data-tone] rules in main.css. Colour alone was doing
    // all the work here, and "ready" and "armed" are exactly the two states an
    // operator should be able to read without stopping to interpret a hue.
    if (b.tone && b.tone !== "none") blk.dataset.tone = b.tone;
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
      if (b.tone !== undefined) {
        if (b.tone && b.tone !== "none") el.dataset.tone = b.tone;
        else delete el.dataset.tone;
      }
      const dotEl = cached.dot;
      if (dotEl && b.dot) {
        dotEl.className = "tb-dot " + b.dot;
      }
      const valSpan = cached.value;
      if (valSpan) valSpan.className = "tb-value" + (b.cls ? " " + b.cls : "");
    });
    // Warnings block has a distinct structure (pill, not v-main/sub); keep
    // its single per-update query as-is rather than over-caching it.
    const warnPill = topBar.querySelector(".tb-block.warnings .tb-warn-pill");
    if (warnPill) {
      const { items, unread, count, level } = notificationSummary(state);
      const countEl = warnPill.querySelector(".tb-warn-count");
      if (countEl) countEl.textContent = String(count);
      // The pill colour is a token, set from CSS off this attribute. It used
      // to be three hex literals written straight onto style.background, which
      // meant the one element in the bar that never changed with the theme was
      // the one shouting loudest.
      warnPill.dataset.level = level;
      const warningButton = warnPill.closest(".tb-block.warnings");
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

    // Before the first telemetry state, so the bar is never built showing
    // dots the operator has turned off. The backend config overrides this the
    // moment it lands (app.js), exactly as it does for theme and scale.
    applySavedStatusDots();

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
    setStatusDots,
    statusDots,
    notifyError: showCmdError,
    beginCommand: (key) => commandDedupe.begin(key),
    succeedCommand: (attempt) => commandDedupe.succeeded(attempt),
    failCommand: completeFailedAttempt,
  };
})();
