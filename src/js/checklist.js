"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.checklist — the operator's preflight checklists, and the window on
  the Home map they are ticked off in.

  Off by default. Settings > Appearance > Preflight checklist turns the
  feature on, and its Edit button opens the dialog the lists are written in
  (js/checklist-editor.js). The Home map then carries this window. The window
  can be put away on its own with its × and brought back from the same
  Settings card, without turning the feature off.

  What a list SAYS lives in the backend config (`checklists`, see
  corvus/checklists.py), so it survives a reinstall and travels with a
  settings export. What has been TICKED lives in localStorage only: it
  belongs to one flight on this station, not to the setup. An item is ticked
  by its text (and which occurrence of that text it is), not by its position,
  so reordering a list keeps the ticks and rewording an item clears only that
  item.

  Nothing here acts on the aircraft. Arming with items still open is allowed
  and only reported, once, as a warning: the checklist is the operator's
  memory aid, and a ground station that refuses to arm over a list it cannot
  verify would be blocking on the wrong thing.

  The window is movable like the joystick pad and the flight HUD: dragged by
  its bar, double-click the bar to send it home, position and fold persisted.
  The drag maths mirrors js/joystick.js on purpose (see its header for why
  neither reuses Corvus.hudPanel), including the interface-scale correction.
*/
Corvus.checklist = (function () {
  const POS_KEY = "corvus.checklist";
  const TICKS_KEY = "corvus.checklist.ticks";
  const MIN_VISIBLE = 64;
  const EDGE = 8;

  /* The list a station has before its operator writes one. Generic enough to
     hold for PX4 and ArduPilot alike: it names things to look at, never a
     parameter or a mode, which differ between the two. */
  const STANDARD = Object.freeze({
    id: "standard",
    name: "Standard preflight",
    items: Object.freeze([
      { text: "Propellers and battery secure", heading: false },
      { text: "Weather, site and airspace checked", heading: false },
      { text: "GPS fix and home position set", heading: false },
      { text: "Failsafe and return altitude checked", heading: false },
      { text: "Area clear of people", heading: false },
    ].map(Object.freeze)),
  });

  let enabled = false;
  let homeWindow = true;
  let activeId = "";
  // null until the operator has saved lists of their own: the standard list
  // is offered then. An empty array is a deliberate "no lists".
  let storedLists = null;
  let ticks = {};

  let panel = null;
  let gripEl = null;
  let titleEl = null;
  let countEl = null;
  let bodyEl = null;
  let collapseBtn = null;
  let collapsed = false;
  let position = { x: null, y: null };
  let drag = null;
  let lastHost = { w: 0, h: 0 };
  let hostObserver = null;
  let boundGlobals = false;
  let wasArmed = null;
  const listeners = new Set();

  /* ------------------------------------------------------------------ */
  /* pure helpers                                                        */
  /* ------------------------------------------------------------------ */

  function cloneList(list) {
    return {
      id: String(list.id),
      name: String(list.name || ""),
      items: (list.items || []).map((it) => ({ text: String(it.text || ""), heading: it.heading === true })),
    };
  }

  /**
   * The config block as this module uses it, with every default filled in.
   * Pure, and exported for the test suite.
   *
   * @param {Object} block cfg.checklists, or anything
   * @returns {{enabled: boolean, homeWindow: boolean, active: string, lists: Array|null}}
   */
  function normalize(block) {
    const b = (block && typeof block === "object") ? block : {};
    return {
      enabled: b.enabled === true,
      homeWindow: b.home_window !== false,
      active: typeof b.active === "string" ? b.active : "",
      lists: Array.isArray(b.lists) ? b.lists.filter((l) => l && typeof l === "object").map(cloneList) : null,
    };
  }

  /**
   * The tick key of every item, null for a heading. The text plus which
   * occurrence of it this is, so two items that read the same stay two.
   * Pure, and exported for the test suite.
   *
   * @param {Array} items
   * @returns {Array<string|null>}
   */
  function itemKeys(items) {
    const seen = {};
    return (items || []).map((it) => {
      if (!it || it.heading) return null;
      const text = String(it.text || "");
      seen[text] = (seen[text] || 0) + 1;
      return text + "\u0000" + seen[text];
    });
  }

  /**
   * How far through a list the operator is. Pure, and exported for the tests.
   *
   * @param {Object} list
   * @param {Array<string>} ticked the list's ticked keys
   * @returns {{done: number, total: number}}
   */
  function progress(list, ticked) {
    const have = new Set(ticked || []);
    const keys = itemKeys(list && list.items).filter((k) => k !== null);
    return { done: keys.filter((k) => have.has(k)).length, total: keys.length };
  }

  /**
   * An id no list in `lists` has yet.
   * @param {Array} lists
   * @returns {string}
   */
  function newId(lists) {
    const taken = new Set((lists || []).map((l) => l.id));
    for (;;) {
      const id = "cl" + Math.random().toString(36).slice(2, 10);
      if (!taken.has(id)) return id;
    }
  }

  /* ------------------------------------------------------------------ */
  /* state                                                               */
  /* ------------------------------------------------------------------ */

  /** The lists in force: the operator's, or the standard one. */
  function lists() {
    return storedLists === null ? [cloneList(STANDARD)] : storedLists.map(cloneList);
  }

  /** The list the Home window shows: the chosen one, else the first. */
  function activeList() {
    const all = lists();
    return all.find((l) => l.id === activeId) || all[0] || null;
  }

  function loadTicks() {
    ticks = {};
    try {
      const saved = JSON.parse(localStorage.getItem(TICKS_KEY) || "null");
      if (!saved || typeof saved !== "object") return;
      Object.keys(saved).forEach((id) => {
        if (Array.isArray(saved[id])) ticks[id] = saved[id].filter((k) => typeof k === "string");
      });
    } catch (_e) { /* unreadable storage -> nothing ticked */ }
  }

  function saveTicks() {
    try { localStorage.setItem(TICKS_KEY, JSON.stringify(ticks)); } catch (_e) {}
  }

  /** Drop ticks of lists and items that no longer exist, so storage does not
   *  grow with every list ever written. */
  function pruneTicks() {
    const next = {};
    lists().forEach((l) => {
      const keys = new Set(itemKeys(l.items));
      const kept = (ticks[l.id] || []).filter((k) => keys.has(k));
      if (kept.length) next[l.id] = kept;
    });
    ticks = next;
    saveTicks();
  }

  function tickedOf(listId) {
    return ticks[listId] || [];
  }

  /** Tick or untick one item of a list. */
  function toggleItem(listId, key) {
    const cur = new Set(tickedOf(listId));
    if (cur.has(key)) cur.delete(key); else cur.add(key);
    ticks[listId] = Array.from(cur);
    saveTicks();
    changed();
  }

  /** Untick every item of a list, for the next flight. */
  function resetList(listId) {
    delete ticks[listId];
    saveTicks();
    changed();
  }

  function changed() {
    paint();
    listeners.forEach((fn) => {
      try { fn(); } catch (err) { console.error("checklist listener failed:", err); }
    });
  }

  /** Be told when the lists, the choice or the ticks change. Returns the unsubscribe. */
  function onChange(fn) {
    listeners.add(fn);
    return () => listeners.delete(fn);
  }

  /** Take the `checklists` block of a config the backend answered with. */
  function fromConfig(cfg) {
    const n = normalize(cfg && cfg.checklists);
    enabled = n.enabled;
    homeWindow = n.homeWindow;
    activeId = n.active;
    storedLists = n.lists;
    pruneTicks();
    changed();
  }

  /**
   * Write part of the block and take back what the backend stored, which is
   * the coerced version: an item cut to length is shown cut.
   * @param {Object} patch
   * @returns {Promise}
   */
  function persist(patch) {
    return Corvus.telemetry.requestJson("/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ checklists: patch }),
    }).then((res) => {
      if (res && res.config) fromConfig(res.config);
      return res;
    });
  }

  /** Turn the whole feature on or off, live. Persisting is the caller's. */
  function setEnabled(on) {
    enabled = !!on;
    changed();
  }

  /** Show or put away the Home window, and persist it. */
  function setHomeWindow(on) {
    const before = homeWindow;
    homeWindow = !!on;
    changed();
    return persist({ home_window: homeWindow }).catch((error) => {
      homeWindow = before;
      changed();
      throw error;
    });
  }

  /** Choose the list the Home window shows, and persist it. */
  function setActive(id) {
    const before = activeId;
    activeId = String(id || "");
    changed();
    return persist({ active: activeId }).catch((error) => {
      activeId = before;
      changed();
      throw error;
    });
  }

  /** Replace every list, as the Setup editor does on Save. */
  function saveLists(next) {
    return persist({ lists: (next || []).map(cloneList) });
  }

  /* ------------------------------------------------------------------ */
  /* the window                                                          */
  /* ------------------------------------------------------------------ */

  function paint() {
    if (!panel) return;
    const show = enabled && homeWindow;
    panel.hidden = !show;
    if (!show) return;
    const ui = Corvus.ui;
    const list = activeList();
    const all = lists();
    const p = list ? progress(list, tickedOf(list.id)) : { done: 0, total: 0 };
    const complete = p.total > 0 && p.done === p.total;

    titleEl.textContent = list ? list.name : "Preflight checklist";
    countEl.textContent = p.total ? `${p.done}/${p.total}` : "";
    countEl.classList.toggle("is-complete", complete);
    panel.classList.toggle("is-complete", complete);
    panel.classList.toggle("is-collapsed", collapsed);

    const label = collapsed ? "Expand the checklist" : "Collapse the checklist";
    collapseBtn.title = label;
    collapseBtn.setAttribute("aria-label", label);
    collapseBtn.setAttribute("aria-expanded", collapsed ? "false" : "true");
    ui.clear(collapseBtn).appendChild(ui.icon(collapsed ? "chevron-down" : "chevron-up", 13));

    // The rows are rebuilt on every change, so the one that had keyboard focus
    // is found again by its key rather than lost to <body>.
    const focused = document.activeElement;
    const focusKey = focused && focused.dataset && bodyEl.contains && bodyEl.contains(focused)
      ? focused.dataset.key : null;
    ui.clear(bodyEl);
    bodyEl.hidden = collapsed;
    if (!collapsed) {
      if (all.length > 1) {
        const picker = ui.select({
          className: "checklist-picker",
          ariaLabel: "Checklist",
          options: all.map((l) => ({ value: l.id, label: l.name })),
          value: list ? list.id : "",
          onChange: (v) => { setActive(v).catch(() => {}); },
        });
        bodyEl.appendChild(picker);
      }
      if (!list || !list.items.length) {
        const empty = document.createElement("div");
        empty.className = "checklist-empty";
        empty.textContent = list
          ? "This checklist has no items yet. Add them in Settings, Preflight checklist."
          : "No checklist yet. Write one in Settings, Preflight checklist.";
        bodyEl.appendChild(empty);
      } else {
        bodyEl.appendChild(buildItems(list));
        if (complete) {
          const done = document.createElement("div");
          done.className = "checklist-done";
          done.appendChild(ui.icon("circle-check", 14));
          done.appendChild(document.createTextNode("All items checked"));
          bodyEl.appendChild(done);
        }
      }
    }
    if (focusKey && bodyEl.querySelectorAll) {
      Array.from(bodyEl.querySelectorAll(".checklist-item"))
        .filter((el) => el.dataset.key === focusKey)
        .forEach((el) => el.focus());
    }
    ui.refreshIcons();
    applyPosition();
  }

  function buildItems(list) {
    const ui = Corvus.ui;
    const keys = itemKeys(list.items);
    const have = new Set(tickedOf(list.id));
    const ol = document.createElement("div");
    ol.className = "checklist-items";
    ol.setAttribute("role", "group");
    ol.setAttribute("aria-label", list.name);
    list.items.forEach((it, i) => {
      if (it.heading) {
        const h = document.createElement("div");
        h.className = "checklist-heading";
        h.textContent = it.text;
        ol.appendChild(h);
        return;
      }
      const key = keys[i];
      const on = have.has(key);
      const row = document.createElement("button");
      row.type = "button";
      row.className = "checklist-item" + (on ? " is-checked" : "");
      row.setAttribute("role", "checkbox");
      row.setAttribute("aria-checked", on ? "true" : "false");
      row.dataset.key = key;
      const box = document.createElement("span");
      box.className = "checklist-box";
      if (on) box.appendChild(ui.icon("check", 12));
      const text = document.createElement("span");
      text.className = "checklist-text";
      text.textContent = it.text;
      row.append(box, text);
      row.addEventListener("click", () => toggleItem(list.id, key));
      ol.appendChild(row);
    });
    return ol;
  }

  /* ---- position, mirrored from js/joystick.js ---------------------- */

  function boundsEl() {
    return panel && panel.parentElement ? panel.parentElement : null;
  }

  function pointerScale(el) {
    if (!el) return 1;
    const rect = el.getBoundingClientRect();
    return (rect.width && el.offsetWidth) ? (rect.width / el.offsetWidth) : 1;
  }

  function clampPosition(x, y) {
    const host = boundsEl();
    if (!host) return { x, y };
    const maxX = Math.max(0, host.clientWidth - MIN_VISIBLE);
    const maxY = Math.max(0, host.clientHeight - MIN_VISIBLE);
    return {
      x: Math.min(Math.max(x, MIN_VISIBLE - panel.offsetWidth), maxX),
      y: Math.min(Math.max(y, 0), maxY),
    };
  }

  function applyPosition() {
    if (!panel) return;
    if (position.x === null || position.y === null) {
      // Unmoved, it rests under the flight bar. The bar's height depends on
      // the window and on how far it has stepped down (js/map.js fitBar), so
      // it is measured rather than guessed: a fixed offset put the window
      // over ARM and TAKEOFF at some sizes.
      const room = document.getElementById("flightBarRoom");
      const below = room && room.offsetHeight ? room.offsetTop + room.offsetHeight + 10 : 0;
      panel.style.left = "";
      panel.style.top = below ? below + "px" : "";
      panel.style.maxHeight = below ? `calc(100% - ${below + 16}px)` : "";
      panel.classList.remove("is-placed");
      return;
    }
    panel.style.maxHeight = "";
    const c = clampPosition(position.x, position.y);
    position.x = c.x; position.y = c.y;
    panel.classList.add("is-placed");
    panel.style.left = c.x + "px";
    panel.style.top = c.y + "px";
  }

  function hostSize() {
    const host = boundsEl();
    return host ? { w: host.clientWidth, h: host.clientHeight } : { w: 0, h: 0 };
  }

  function contain(x, y) {
    const host = boundsEl();
    if (!host) return { x, y };
    const loose = clampPosition(x, y);
    const maxX = host.clientWidth - panel.offsetWidth - EDGE;
    const maxY = host.clientHeight - panel.offsetHeight - EDGE;
    return {
      x: maxX >= EDGE ? Math.min(Math.max(x, EDGE), maxX) : loose.x,
      y: maxY >= EDGE ? Math.min(Math.max(y, EDGE), maxY) : loose.y,
    };
  }

  /* The map changed size, usually the right panel sliding. Same rules as the
     joystick pad: a hidden map says nothing, and a window nearer the right
     edge travels with it rather than disappearing under the panel. */
  function onHostResize() {
    if (!panel) return;
    const size = hostSize();
    if (!size.w || !size.h) return;
    if (size.w === lastHost.w && size.h === lastHost.h) {
      applyPosition();
      return;
    }
    if (position.x !== null && lastHost.w && size.w !== lastHost.w) {
      const nearRight = (lastHost.w - (position.x + panel.offsetWidth)) < position.x;
      if (nearRight) position.x += size.w - lastHost.w;
    }
    if (position.y !== null && lastHost.h && size.h !== lastHost.h) {
      const nearBottom = (lastHost.h - (position.y + panel.offsetHeight)) < position.y;
      if (nearBottom) position.y += size.h - lastHost.h;
    }
    lastHost = size;
    if (position.x !== null && position.y !== null) {
      const c = contain(position.x, position.y);
      position.x = c.x; position.y = c.y;
    }
    applyPosition();
  }

  function loadPosition() {
    position = { x: null, y: null };
    collapsed = false;
    try {
      const saved = JSON.parse(localStorage.getItem(POS_KEY) || "null");
      if (!saved || typeof saved !== "object") return;
      if (typeof saved.x === "number") position.x = saved.x;
      if (typeof saved.y === "number") position.y = saved.y;
      collapsed = !!saved.collapsed;
    } catch (_e) { /* unreadable storage -> the default corner, expanded */ }
  }

  function savePosition() {
    try {
      localStorage.setItem(POS_KEY, JSON.stringify({ x: position.x, y: position.y, collapsed }));
    } catch (_e) {}
  }

  function resetPosition() {
    position = { x: null, y: null };
    applyPosition();
    savePosition();
  }

  function onBarControl(event) {
    const el = event && event.target;
    return !!(el && typeof el.closest === "function" && el.closest("button, select"));
  }

  function wireDrag() {
    gripEl.addEventListener("pointerdown", (event) => {
      if (event.button !== 0 || onBarControl(event)) return;
      const host = boundsEl();
      if (!host) return;
      const hb = host.getBoundingClientRect();
      const pb = panel.getBoundingClientRect();
      const k = pointerScale(host);
      drag = {
        pointerId: event.pointerId,
        dx: (event.clientX - pb.left) / k,
        dy: (event.clientY - pb.top) / k,
        hostLeft: hb.left,
        hostTop: hb.top,
        scale: k,
      };
      try { panel.setPointerCapture(event.pointerId); } catch (_e) {}
      panel.classList.add("is-dragging");
      event.preventDefault();
    });
    panel.addEventListener("pointermove", (event) => {
      if (!drag || event.pointerId !== drag.pointerId) return;
      position.x = (event.clientX - drag.hostLeft) / drag.scale - drag.dx;
      position.y = (event.clientY - drag.hostTop) / drag.scale - drag.dy;
      applyPosition();
    });
    ["pointerup", "pointercancel"].forEach((name) => {
      panel.addEventListener(name, (event) => {
        if (!drag || event.pointerId !== drag.pointerId) return;
        try { panel.releasePointerCapture(drag.pointerId); } catch (_e) {}
        drag = null;
        panel.classList.remove("is-dragging");
        savePosition();
      });
    });
    gripEl.addEventListener("dblclick", (event) => {
      if (onBarControl(event)) return;
      resetPosition();
    });
  }

  function toggleCollapsed() {
    collapsed = !collapsed;
    savePosition();
    paint();
  }

  /* Arming with the list unfinished is reported once per arming, never
     prevented. Only the transition counts, so a GCS opened on an aircraft
     already in the air says nothing. */
  function onTelemetry(s) {
    const armed = !!(s && s.armed);
    if (wasArmed === false && armed && enabled) {
      const list = activeList();
      const p = list ? progress(list, tickedOf(list.id)) : { done: 0, total: 0 };
      if (p.total && p.done < p.total) {
        window.dispatchEvent(new CustomEvent("corvus:notification", {
          detail: {
            level: "warning",
            message: `Armed with the preflight checklist open: ${p.done} of ${p.total} items checked.`,
          },
        }));
      }
    }
    wasArmed = s && s.connected ? armed : null;
  }

  function init(root) {
    panel = root;
    if (!panel) return;
    const ui = Corvus.ui;
    panel.hidden = true;
    while (panel.firstChild) panel.removeChild(panel.firstChild);

    gripEl = document.createElement("div");
    gripEl.className = "checklist-bar";
    gripEl.title = "Drag to move, double-click to reset";
    gripEl.appendChild(ui.icon("clipboard-check", 14));
    titleEl = document.createElement("span");
    titleEl.className = "checklist-title";
    countEl = document.createElement("span");
    countEl.className = "checklist-count";
    const resetBtn = ui.iconButton("rotate-ccw", {
      size: 13,
      className: "icon-btn checklist-bar-btn",
      title: "Uncheck every item for the next flight",
      onClick: () => { const l = activeList(); if (l) resetList(l.id); },
    });
    collapseBtn = ui.iconButton("chevron-up", {
      size: 13,
      className: "icon-btn checklist-bar-btn",
      title: "Collapse the checklist",
      onClick: toggleCollapsed,
    });
    const closeBtn = ui.iconButton("x", {
      size: 13,
      className: "icon-btn checklist-bar-btn",
      title: "Hide the checklist window. Settings, Preflight checklist shows it again.",
      onClick: () => { setHomeWindow(false).catch(() => {}); },
    });
    gripEl.append(titleEl, countEl, resetBtn, collapseBtn, closeBtn);

    bodyEl = document.createElement("div");
    bodyEl.className = "checklist-body";
    panel.append(gripEl, bodyEl);

    loadPosition();
    wireDrag();
    lastHost = hostSize();
    if (!boundGlobals) {
      window.addEventListener("resize", onHostResize);
      Corvus.telemetry.subscribe(onTelemetry);
      boundGlobals = true;
    }
    if (hostObserver) { hostObserver.disconnect(); hostObserver = null; }
    if (typeof ResizeObserver === "function" && boundsEl()) {
      hostObserver = new ResizeObserver(onHostResize);
      hostObserver.observe(boundsEl());
    }
    paint();
  }

  // Before anything can call fromConfig: its prune writes the ticks back.
  loadTicks();

  return {
    init,
    fromConfig,
    persist,
    setEnabled,
    setHomeWindow,
    setActive,
    saveLists,
    toggleItem,
    resetList,
    onChange,
    lists,
    activeList,
    tickedOf,
    isEnabled: () => enabled,
    isHomeWindow: () => homeWindow,
    hasOwnLists: () => storedLists !== null,
    resetPosition,
    normalize,
    itemKeys,
    progress,
    newId,
    STANDARD,
  };
})();
