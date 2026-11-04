"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.fleet — every vehicle the station hears, and which one is flown.

  The backend (corvus/fleet.py) pushes the whole list on the shared
  /api/events stream under the "fleet" topic, a few times a second while
  anything in it changes. This module keeps the latest copy, hands it to
  whoever subscribes (the top bar, the map), and owns the Vehicles dialog
  the top bar's Vehicle block opens: names, colours, which vehicle is
  flown, and the extra links a vehicle on a radio of its own needs.

  One vehicle is flown at a time. Everything else in the app (the HUD, the
  flight bar, every setup page) follows that one; the rest are drawn on the
  map in their own colour.

  One link is one vehicle: a second aircraft needs a radio and a link of its
  own. Removing a vehicle closes its link, and it leaves the list and the map.
*/
Corvus.fleet = (function () {
  const EMPTY = {
    active: "", active_link: "main", links: [], vehicles: [], palette: [],
    suggestion: null,
  };
  // A snapshot published just before a removal can still be on its way. The
  // removed vehicle is held off the list this long, so it does not flash back.
  const REMOVED_HOLD_MS = 3000;
  const removedAt = new Map();
  let snapshot = EMPTY;
  const subscribers = new Set();
  let started = false;
  let dialog = null;

  function recentlyRemoved(id) {
    const at = removedAt.get(id);
    if (at === undefined) return false;
    if (Date.now() - at < REMOVED_HOLD_MS) return true;
    removedAt.delete(id);
    return false;
  }

  function normalise(data) {
    if (!data || typeof data !== "object") return EMPTY;
    const vehicles = (Array.isArray(data.vehicles) ? data.vehicles : [])
      .filter((v) => v && typeof v.id === "string" && !recentlyRemoved(v.id));
    return {
      active: typeof data.active === "string" && !recentlyRemoved(data.active) ? data.active : "",
      active_link: typeof data.active_link === "string" ? data.active_link : "main",
      links: Array.isArray(data.links) ? data.links : [],
      vehicles,
      palette: Array.isArray(data.palette) ? data.palette : [],
      suggestion: data.suggestion && typeof data.suggestion === "object" ? data.suggestion : null,
    };
  }

  /* What the vehicles not being flown said, turned into notifications on the
     board (corvus/fleet.py decides which: warnings and worse, and a vehicle
     that went quiet or came back). The first list after a page load is the
     baseline, so a reload does not replay the session's history. */
  let eventSeq = null;

  function announce(events) {
    if (!Array.isArray(events)) return;
    const list = events;
    const top = list.reduce((m, e) => Math.max(m, Number(e.seq) || 0), 0);
    if (eventSeq === null) { eventSeq = top; return; }
    list.forEach((e) => {
      const seq = Number(e.seq) || 0;
      if (seq <= eventSeq) return;
      const who = nameOf(e.vehicle) || String(e.vehicle || "Another vehicle");
      const level = e.level === "critical" || e.level === "warning" ? e.level : "info";
      window.dispatchEvent(new CustomEvent("corvus:notification", {
        detail: { level, message: `${who}: ${e.text}` },
      }));
    });
    eventSeq = Math.max(eventSeq, top);
  }

  function take(data) {
    snapshot = normalise(data);
    announce(data && data.events);
    subscribers.forEach((fn) => {
      try { fn(snapshot); } catch (err) { console.error("fleet subscriber failed:", err); }
    });
    if (dialog) dialog.render();
  }

  function start() {
    if (started) return;
    started = true;
    if (Corvus.events && typeof Corvus.events.subscribe === "function") {
      Corvus.events.subscribe("fleet", take);
    }
    if (Corvus.telemetry && typeof Corvus.telemetry.requestJson === "function") {
      Corvus.telemetry.requestJson("/api/fleet").then(take).catch(() => {});
    }
  }

  function subscribe(fn) {
    if (typeof fn !== "function") return function () {};
    subscribers.add(fn);
    try { fn(snapshot); } catch (err) { console.error("fleet subscriber failed:", err); }
    return () => subscribers.delete(fn);
  }

  function get() { return snapshot; }

  function vehicle(id) {
    if (!id) return null;
    return snapshot.vehicles.find((v) => v.id === id) || null;
  }

  /** The vehicle being flown, or null while none is. */
  function activeVehicle() { return vehicle(snapshot.active); }

  /** More than one thing to choose between: vehicles or links. */
  function isMulti() {
    return snapshot.vehicles.length > 1 || snapshot.links.length > 1;
  }

  /** The colour chosen for *id*, or "" for the theme's own. */
  function colorOf(id) {
    const v = vehicle(id);
    return v && typeof v.color === "string" ? v.color : "";
  }

  /** The name the operator reads for *id*, or "". */
  function nameOf(id) {
    const v = vehicle(id);
    return v ? String(v.name || "") : "";
  }

  async function post(url, payload) {
    const data = await Corvus.telemetry.requestJson(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload || {}),
    });
    if (data && data.fleet) take(data.fleet);
    return data;
  }

  const select = (id) => post("/api/fleet/select", { id });
  const rename = (id, name) => post("/api/fleet/vehicle", { id, name });
  const setColor = (id, color) => post("/api/fleet/vehicle", { id, color });
  const addLink = (connection) => post("/api/fleet/links/add", { connection });
  const removeLink = (id) => post("/api/fleet/links/remove", { id });

  /** Take *id* off the list: its link is closed, and the map forgets it. */
  async function removeVehicle(id, confirmArmed) {
    const data = await post("/api/fleet/vehicle/remove", { id, confirm_armed: !!confirmArmed });
    removedAt.set(id, Date.now());
    if (Corvus.mapFleet && typeof Corvus.mapFleet.forget === "function") Corvus.mapFleet.forget(id);
    return data;
  }

  /* ---------------- colours ---------------- */

  /** "#RRGGBB" -> "r g b", the form the --vehicle-rgb token takes. */
  function rgbTriplet(hex) {
    const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(String(hex || ""));
    if (!m) return "";
    return [m[1], m[2], m[3]].map((h) => parseInt(h, 16)).join(" ");
  }

  /** *hex* mixed toward white by *amount* (0 to 1). Earlier flights' colour. */
  function tint(hex, amount) {
    const rgb = rgbTriplet(hex);
    if (!rgb) return hex;
    const k = Math.max(0, Math.min(1, amount));
    return "#" + rgb.split(" ").map((c) => {
      const v = Math.round(Number(c) + (255 - Number(c)) * k);
      return v.toString(16).padStart(2, "0");
    }).join("").toUpperCase();
  }

  /** "#RRGGBB" for a hue (degrees), saturation and lightness (0 to 1). */
  function hslHex(hue, sat, light) {
    const a = sat * Math.min(light, 1 - light);
    const channel = (n) => {
      const k = (n + hue / 30) % 12;
      const v = light - a * Math.max(-1, Math.min(k - 3, 9 - k, 1));
      return Math.round(v * 255).toString(16).padStart(2, "0");
    };
    return ("#" + channel(0) + channel(8) + channel(4)).toUpperCase();
  }

  /**
   * The tone of the earlier flight at *index*, oldest first. Consecutive
   * flights step round the colour wheel by the golden angle, so neighbours
   * are always far apart however many there are, and the hue is pulled
   * toward white like the plain earlier-flight tone, so it stays quieter
   * than the flight in progress.
   */
  function rainbowTone(index) {
    const hue = (200 + Math.max(0, Math.floor(Number(index) || 0)) * 137.508) % 360;
    return tint(hslHex(hue, 0.8, 0.55), 0.3);
  }

  /** What a vehicle with no colour of its own is drawn in: the theme's. */
  function themeColor() {
    return (Corvus.ui && Corvus.ui.token) ? Corvus.ui.token("--vehicle", "#D62B26") : "#D62B26";
  }

  /** Point an element's --vehicle tokens at *color*, or back at the theme. */
  function paintElement(el, color) {
    if (!el || !el.style) return;
    const rgb = rgbTriplet(color);
    if (rgb) {
      el.style.setProperty("--vehicle", color);
      el.style.setProperty("--vehicle-rgb", rgb);
    } else {
      el.style.removeProperty("--vehicle");
      el.style.removeProperty("--vehicle-rgb");
    }
  }

  /* ---------------- the dialog ---------------- */

  function telemetryState() {
    return (Corvus.telemetry && Corvus.telemetry.getState && Corvus.telemetry.getState()) || {};
  }

  function statusText(v) {
    if (v.stale) return "Lost";
    return v.armed ? "Armed" : "Disarmed";
  }

  function detailText(v) {
    const link = linkOf(v);
    const parts = [`System ${v.sysid}`, v.link_label || "", link && !link.primary ? link.connection : ""];
    if (v.type) parts.push(String(v.type).replace(/_/g, " ").toLowerCase());
    if (v.stack) parts.push(v.stack === "px4" ? "PX4" : v.stack === "ardupilot" ? "ArduPilot" : v.stack);
    return parts.filter(Boolean).join(" · ");
  }

  function readoutText(v) {
    const parts = [];
    if (v.mode_label) parts.push(v.mode_label);
    if (typeof v.battery === "number") parts.push(`${v.battery}%`);
    else if (typeof v.voltage === "number") parts.push(`${v.voltage.toFixed(1)} V`);
    if (typeof v.altitude_rel === "number" && Corvus.units) {
      parts.push(`${Corvus.units.formatLength(v.altitude_rel)} rel`);
    }
    return parts.join(" · ");
  }

  function confirmSwitch(target, onYes) {
    const state = telemetryState();
    const current = activeVehicle();
    if (!current || !state.armed || current.stale) { onYes(); return; }
    const body = document.createElement("div");
    body.className = "fleet-confirm";
    const p1 = document.createElement("p");
    p1.textContent = `${current.name} is armed. It keeps flying in ${current.mode_label || "its current mode"}, ` +
      `and the station stops sending it commands.`;
    const p2 = document.createElement("p");
    p2.textContent = `From now on the flight bar, the joystick and every page control ${target.name}.`;
    body.append(p1, p2);
    let handle = null;
    handle = Corvus.ui.modal({
      title: `Take control of ${target.name}?`,
      body,
      size: "sm",
      actions: [
        Corvus.ui.button({ label: "Cancel", variant: "ghost", onClick: () => handle.close() }),
        Corvus.ui.button({
          label: "Take control", variant: "primary",
          onClick: () => { handle.close(); onYes(); },
        }),
      ],
    });
    handle.open();
  }

  function linkOf(v) {
    return snapshot.links.find((l) => l.id === v.link) || null;
  }

  function confirmRemove(v, onDone) {
    const link = linkOf(v);
    const armed = !!v.armed && !v.stale;
    const body = document.createElement("div");
    body.className = "fleet-confirm";
    const p1 = document.createElement("p");
    if (link && link.primary) {
      p1.textContent = "The main link is disconnected and stays closed. " +
        "It leaves the list and the map. Connect it again on the LINK tab.";
    } else {
      const conn = link ? ` (${link.connection})` : "";
      p1.textContent = `Its link${conn} is closed. It leaves the list and the map. ` +
        "To bring it back, open the link again.";
    }
    body.appendChild(p1);
    if (armed) {
      const p2 = document.createElement("p");
      p2.className = "fleet-confirm-warn";
      p2.textContent = `${v.name} is armed. It keeps flying in ${v.mode_label || "its current mode"}, ` +
        "and the station no longer sees it or sends it anything.";
      body.appendChild(p2);
    }
    let handle = null;
    const removeBtn = Corvus.ui.button({
      label: "Remove", variant: "danger", icon: "trash-2",
      onClick: () => {
        Corvus.ui.setBusy(removeBtn, true);
        removeVehicle(v.id, armed)
          .then(() => { handle.close(); if (onDone) onDone(); })
          .catch((err) => Corvus.ui.toast({ level: "critical", message: err.message }))
          .finally(() => Corvus.ui.setBusy(removeBtn, false));
      },
    });
    handle = Corvus.ui.modal({
      title: `Remove ${v.name}?`,
      body,
      size: "sm",
      actions: [
        Corvus.ui.button({ label: "Cancel", variant: "ghost", onClick: () => handle.close() }),
        removeBtn,
      ],
    });
    handle.open();
  }

  /** The serial device of a "serial:<device>:<baud>" connection, or "". */
  function serialDevice(conn) {
    const text = String(conn || "");
    if (!text.startsWith("serial:")) return "";
    return text.slice(7).replace(/:\d+$/, "");
  }

  function sameDevice(a, b) {
    const norm = (d) => String(d || "").toLowerCase().replace("/dev/tty.", "/dev/cu.");
    return !!a && norm(a) === norm(b);
  }

  /**
   * The links: one row each, the radio auto-connect just saw plugged in, the
   * serial ports no link holds, and a field for anything else. Shared by the
   * Vehicles dialog and Settings, so both say the same thing.
   */
  function buildLinksPanel() {
    const root = document.createElement("div");
    root.className = "fleet-links-panel";
    const linkList = document.createElement("div");
    linkList.className = "fleet-links";

    const offer = document.createElement("div");
    offer.className = "fleet-offer";
    offer.hidden = true;
    const offerText = document.createElement("span");
    offerText.className = "fleet-offer-text";
    const offerBtn = Corvus.ui.button({ label: "Open as another vehicle", size: "sm", variant: "primary" });
    offer.append(offerText, offerBtn);

    const free = document.createElement("div");
    free.className = "fleet-free";
    free.hidden = true;
    const freeHead = document.createElement("div");
    freeHead.className = "fleet-free-head";
    const freeTitle = document.createElement("span");
    freeTitle.textContent = "Serial ports no link uses";
    const freeRefresh = Corvus.ui.iconButton("refresh-cw", {
      title: "Look for ports again", ariaLabel: "Look for ports again", size: 13,
      onClick: () => refreshPorts(),
    });
    freeHead.append(freeTitle, freeRefresh);
    const freeList = document.createElement("div");
    freeList.className = "fleet-free-list";
    free.append(freeHead, freeList);

    const hint = document.createElement("p");
    hint.className = "fleet-hint";
    hint.textContent = "One link is one vehicle. A second vehicle needs a radio of its own " +
      "and a link of its own. Another autopilot heard on a link is ignored.";

    const addRow = document.createElement("div");
    addRow.className = "fleet-add";
    const conn = Corvus.ui.input({
      mono: true, placeholder: "serial:/dev/ttyUSB1:57600 or udpin:0.0.0.0:14560",
      ariaLabel: "Connection for a new link", spellcheck: false, autocomplete: false,
    });
    const listId = "fleetPorts-" + Math.random().toString(36).slice(2, 8);
    const datalist = document.createElement("datalist");
    datalist.id = listId;
    conn.setAttribute("list", listId);
    const error = document.createElement("div");
    error.className = "fleet-error";
    error.hidden = true;

    async function open(connection, btn) {
      error.hidden = true;
      if (btn) Corvus.ui.setBusy(btn, true);
      try {
        await addLink(connection);
        return true;
      } catch (err) {
        error.textContent = err.message || "The link could not be opened.";
        error.hidden = false;
        return false;
      } finally {
        if (btn) Corvus.ui.setBusy(btn, false);
      }
    }

    const addBtn = Corvus.ui.button({
      label: "Open link", icon: "plus", size: "sm",
      onClick: async () => {
        const value = conn.value.trim();
        if (!value) { conn.focus(); return; }
        if (await open(value, addBtn)) conn.value = "";
      },
    });
    conn.addEventListener("keydown", (e) => { if (e.key === "Enter") addBtn.click(); });
    addRow.append(conn, datalist, addBtn);
    root.append(linkList, offer, free, hint, addRow, error);

    let ports = [];
    let lastOffer = "";
    const linkRows = new Map();

    function inUse(device) {
      return snapshot.links.some((l) => sameDevice(serialDevice(l.connection), device));
    }

    function refreshPorts() {
      if (!Corvus.telemetry || !Corvus.telemetry.requestJson) return;
      Corvus.telemetry.requestJson("/api/mavlink/serial-ports").then((res) => {
        ports = res && Array.isArray(res.ports) ? res.ports : [];
        Corvus.ui.clear(datalist);
        ports.forEach((p) => {
          const opt = document.createElement("option");
          opt.value = `serial:${p.device}:57600`;
          opt.label = p.description ? `${p.device} (${p.description})` : p.device;
          datalist.appendChild(opt);
        });
        renderFree();
      }).catch(() => {});
    }

    let freeSig = "";
    function renderFree() {
      const unused = ports.filter((p) => !inUse(p.device));
      const sig = unused.map((p) => p.device).join("|");
      if (sig === freeSig) return;
      freeSig = sig;
      Corvus.ui.clear(freeList);
      free.hidden = unused.length === 0;
      unused.forEach((p) => {
        const row = document.createElement("div");
        row.className = "fleet-free-row";
        const name = document.createElement("span");
        name.className = "fleet-link-conn";
        name.textContent = p.device;
        const desc = document.createElement("span");
        desc.className = "fleet-link-count";
        desc.textContent = p.description || "";
        const btn = Corvus.ui.button({ label: "Open", size: "sm" });
        btn.addEventListener("click", () => open(`serial:${p.device}:57600`, btn));
        row.append(name, desc, btn);
        freeList.appendChild(row);
      });
    }

    function buildLinkRow(link) {
      const row = document.createElement("div");
      row.className = "fleet-link";
      const dot = document.createElement("span");
      dot.className = "status-dot";
      const label = document.createElement("span");
      label.className = "fleet-link-label";
      const connEl = document.createElement("span");
      connEl.className = "fleet-link-conn";
      const count = document.createElement("span");
      count.className = "fleet-link-count";
      const remove = link.primary ? null : Corvus.ui.iconButton("x", {
        title: "Close this link", ariaLabel: "Close this link", size: 14,
        onClick: () => removeLink(link.id).catch((err) =>
          Corvus.ui.toast({ level: "critical", message: err.message })),
      });
      row.append(dot, label, connEl, count);
      if (remove) row.appendChild(remove);
      return { el: row, dot, label, connEl, count };
    }

    function updateLinkRow(entry, link) {
      const level = link.status === "connected" ? "healthy"
        : (link.status === "degraded" || link.status === "connecting" || link.status === "reconnecting")
          ? "warning" : "off";
      entry.dot.setAttribute("data-level", level);
      entry.label.textContent = link.label;
      entry.connEl.textContent = link.connection;
      entry.connEl.title = link.error || link.connection;
      entry.count.textContent = link.vehicles ? "1 vehicle" : "No vehicle yet";
      entry.el.classList.toggle("is-active", link.id === snapshot.active_link);
    }

    function renderOffer() {
      const s = snapshot.suggestion;
      const conn = s && typeof s.connection_string === "string" ? s.connection_string : "";
      const show = !!conn && !inUse(serialDevice(conn));
      offer.hidden = !show;
      if (!show) { lastOffer = ""; return; }
      if (conn === lastOffer) return;
      lastOffer = conn;
      const kind = s.kind === "sik" ? "Telemetry radio" : "Flight controller";
      offerText.textContent = `${kind} on ${s.device || serialDevice(conn)} plugged in just now.`;
      offerBtn.onclick = () => open(conn, offerBtn);
      refreshPorts();
    }

    function render() {
      const seen = new Set();
      snapshot.links.forEach((link, i) => {
        seen.add(link.id);
        let entry = linkRows.get(link.id);
        if (!entry) {
          entry = buildLinkRow(link);
          linkRows.set(link.id, entry);
        }
        if (linkList.children[i] !== entry.el) linkList.insertBefore(entry.el, linkList.children[i] || null);
        updateLinkRow(entry, link);
      });
      linkRows.forEach((entry, id) => {
        if (!seen.has(id)) { entry.el.remove(); linkRows.delete(id); }
      });
      renderOffer();
      renderFree();
      Corvus.ui.refreshIcons();
    }

    refreshPorts();
    render();
    return { el: root, render, refreshPorts };
  }

  function buildDialog(focusId) {
    const root = document.createElement("div");
    root.className = "fleet-dialog";

    const vehHead = document.createElement("div");
    vehHead.className = "fleet-section-head";
    vehHead.textContent = "Vehicles";
    const list = document.createElement("div");
    list.className = "fleet-list";
    list.setAttribute("role", "list");
    const empty = document.createElement("p");
    empty.className = "fleet-empty";
    empty.textContent = "No vehicle heard yet. Connect one on the LINK tab, or open another link below.";

    const linkHead = document.createElement("div");
    linkHead.className = "fleet-section-head";
    linkHead.textContent = "Links";
    const links = buildLinksPanel();
    root.append(vehHead, list, empty, linkHead, links.el);

    const rows = new Map();
    let openPalette = null;

    function swatchPicker(v, row) {
      const strip = document.createElement("div");
      strip.className = "fleet-palette";
      const choices = [""].concat(snapshot.palette || []);
      const current = (v.color || "").toUpperCase();
      choices.forEach((c) => {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "fleet-chip" + (c ? "" : " is-theme") + (c.toUpperCase() === current ? " is-selected" : "");
        b.setAttribute("aria-pressed", String(c.toUpperCase() === current));
        b.title = c ? c : "Theme colour";
        b.setAttribute("aria-label", c ? `Colour ${c}` : "Theme colour");
        b.style.background = c || themeColor();
        b.addEventListener("click", () => {
          setColor(v.id, c).catch((err) => Corvus.ui.toast({ level: "critical", message: err.message }));
          closePalette();
        });
        strip.appendChild(b);
      });
      row.appendChild(strip);
      openPalette = strip;
    }

    function closePalette() {
      if (openPalette && openPalette.parentNode) openPalette.parentNode.removeChild(openPalette);
      openPalette = null;
    }

    function buildRow(v) {
      const row = document.createElement("div");
      row.className = "fleet-row";
      row.setAttribute("role", "listitem");
      row.dataset.vehicle = v.id;

      const swatch = document.createElement("button");
      swatch.type = "button";
      swatch.className = "fleet-swatch";
      swatch.title = "Colour on the map";
      swatch.setAttribute("aria-label", "Colour on the map");
      swatch.addEventListener("click", () => {
        const wasOpen = openPalette && openPalette.parentNode === row;
        closePalette();
        if (!wasOpen) swatchPicker(rows.get(v.id).data, row);
      });

      const main = document.createElement("div");
      main.className = "fleet-main";
      const name = Corvus.ui.input({ className: "fleet-name", ariaLabel: "Vehicle name", spellcheck: false });
      name.maxLength = 40;
      const commit = () => {
        const data = rows.get(v.id).data;
        const typed = name.value.trim();
        const current = data.custom_name ? data.name : "";
        if (typed === current) return;
        rename(v.id, typed).catch((err) => Corvus.ui.toast({ level: "critical", message: err.message }));
      };
      name.addEventListener("change", commit);
      name.addEventListener("keydown", (e) => {
        if (e.key === "Enter") { e.preventDefault(); name.blur(); }
        if (e.key === "Escape") {
          const data = rows.get(v.id).data;
          name.value = data.custom_name ? data.name : "";
        }
      });
      const detail = document.createElement("div");
      detail.className = "fleet-detail";
      const readout = document.createElement("div");
      readout.className = "fleet-readout";
      main.append(name, detail, readout);

      const side = document.createElement("div");
      side.className = "fleet-side";
      const status = document.createElement("span");
      status.className = "fleet-status";
      const locate = Corvus.ui.iconButton("crosshair", {
        title: "Show on the map", ariaLabel: "Show on the map", size: 14,
        onClick: () => {
          const data = rows.get(v.id).data;
          const m = Corvus.map && Corvus.map.getMap && Corvus.map.getMap();
          if (m && Array.isArray(data.position)) {
            if (Corvus.map.setFollow) Corvus.map.setFollow(false);
            m.easeTo({ center: data.position, duration: 700 });
          }
        },
      });
      const take = Corvus.ui.button({ label: "Take control", size: "sm", variant: "primary" });
      take.addEventListener("click", () => {
        const data = rows.get(v.id).data;
        confirmSwitch(data, () => {
          Corvus.ui.setBusy(take, true);
          select(data.id)
            .catch((err) => Corvus.ui.toast({ level: "critical", message: err.message }))
            .finally(() => Corvus.ui.setBusy(take, false));
        });
      });
      const flying = document.createElement("span");
      flying.className = "fleet-flying";
      flying.textContent = "In control";
      const remove = Corvus.ui.iconButton("trash-2", {
        title: "Remove this vehicle", ariaLabel: "Remove this vehicle", size: 14,
        onClick: () => confirmRemove(rows.get(v.id).data),
      });
      remove.classList.add("fleet-remove");
      side.append(status, locate, take, flying, remove);

      row.append(swatch, main, side);
      return { el: row, swatch, name, detail, readout, status, locate, take, flying, data: v };
    }

    function updateRow(entry, v) {
      entry.data = v;
      entry.el.classList.toggle("is-active", !!v.active);
      entry.el.classList.toggle("is-stale", !!v.stale);
      entry.swatch.style.background = v.color || themeColor();
      if (document.activeElement !== entry.name) {
        entry.name.value = v.custom_name ? v.name : "";
        entry.name.placeholder = v.custom_name ? "" : v.name;
      }
      entry.detail.textContent = detailText(v);
      entry.readout.textContent = readoutText(v);
      entry.status.textContent = statusText(v);
      entry.status.dataset.state = v.stale ? "lost" : (v.armed ? "armed" : "idle");
      entry.locate.disabled = !Array.isArray(v.position);
      entry.take.hidden = !!v.active;
      entry.take.disabled = !!v.stale;
      entry.flying.hidden = !v.active;
    }

    function render() {
      const seen = new Set();
      snapshot.vehicles.forEach((v, i) => {
        seen.add(v.id);
        let entry = rows.get(v.id);
        if (!entry) {
          entry = buildRow(v);
          rows.set(v.id, entry);
        }
        if (list.children[i] !== entry.el) list.insertBefore(entry.el, list.children[i] || null);
        updateRow(entry, v);
      });
      rows.forEach((entry, id) => {
        if (!seen.has(id)) { entry.el.remove(); rows.delete(id); }
      });
      empty.hidden = snapshot.vehicles.length > 0;
      if (openPalette && !openPalette.isConnected) openPalette = null;

      links.render();
      Corvus.ui.refreshIcons();
    }

    render();
    if (focusId && rows.has(focusId)) {
      const target = rows.get(focusId).el;
      target.classList.add("is-focus");
      setTimeout(() => {
        if (target.scrollIntoView) target.scrollIntoView({ block: "nearest" });
      }, 0);
    }
    return { el: root, render };
  }

  /** Open the Vehicles dialog, optionally scrolled to one vehicle. */
  function openDialog(focusId) {
    if (dialog) { dialog.close(); dialog = null; }
    const view = buildDialog(focusId);
    const handle = Corvus.ui.modal({
      title: "Vehicles",
      body: view.el,
      size: "lg",
      onClose: () => { if (dialog && dialog.handle === handle) dialog = null; },
    });
    dialog = { handle, render: view.render, close: handle.close };
    handle.open();
    // The modal hands focus to its first input, which here is a vehicle's
    // name: the dialog would open with a caret in it, as if asking for one.
    if (document.activeElement && document.activeElement.classList.contains("fleet-name")) {
      handle.dialog.tabIndex = -1;
      handle.dialog.focus();
    }
    return handle;
  }

  return {
    start, subscribe, get, vehicle, activeVehicle, isMulti, colorOf, nameOf,
    select, rename, setColor, addLink, removeLink, removeVehicle, openDialog,
    // The links list, for Settings: {el, render, refreshPorts}.
    buildLinksPanel,
    rgbTriplet, tint, rainbowTone, paintElement, themeColor,
    // test hook: feed a snapshot as if the stream had delivered it.
    _take: take,
  };
})();
