"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.ui — reusable UI component helpers.
  Pure factory functions returning DOM elements. No module state, no side
  effects on existing modules. tiles.js and app.js already use button /
  iconButton / icon / setBusy; the duplicate icon() factories in topbar.js /
  sidenav.js / map.js and the hand-rolled button markup elsewhere are still
  migrating over. Version is never referenced here — the HUD/About read it
  from GET /api/version, never from a JS literal.
*/
Corvus.ui = (function () {
  /* Icon size mapped to .btn data-size so JS-created icons and the CSS in
     components.css agree (both 15 / 13 / 17 for md / sm / lg). */
  const BTN_ICON_PX = { sm: 13, md: 15, lg: 17 };

  /*
    Build a Lucide placeholder <i data-lucide="name"> with width/height set
    inline. Lucide's createIcons() swaps it for an <svg> once it is in the
    DOM, so callers must run lucide.createIcons() after appending. Default
    size 16 matches the standalone icon usage across the app.
  */
  function icon(name, sizePx) {
    const size = sizePx == null ? 16 : sizePx;
    const el = document.createElement("i");
    el.setAttribute("data-lucide", name);
    el.style.width = size + "px";
    el.style.height = size + "px";
    return el;
  }

  /*
    Small square icon button reusing the existing .icon-btn style from
    main.css (not restyled here). variant "active" toggles .icon-btn.active.
  */
  function iconButton(name, opts) {
    const o = opts || {};
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "icon-btn";
    if (o.title) btn.title = o.title;
    if (o.variant === "active") btn.classList.add("active");
    btn.appendChild(icon(name, o.size || 15));
    return btn;
  }

  /*
    Build a <button class="btn"> on the components.css system.
    data-variant defaults to "secondary", data-size to "md"; data-shape is
    set only when provided. An optional icon is prepended as a .btn-icon
    sized to match data-size; the label is wrapped in a <span>. busy/disabled
    map to the .is-busy class / .disabled property. onClick is attached as
    a click listener.
  */
  function button(opts) {
    const o = opts || {};
    const size = o.size || "md";

    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "btn";
    btn.setAttribute("data-variant", o.variant || "secondary");
    btn.setAttribute("data-size", size);
    if (o.shape) btn.setAttribute("data-shape", o.shape);

    if (o.icon) {
      const ic = icon(o.icon, BTN_ICON_PX[size] != null ? BTN_ICON_PX[size] : 15);
      ic.classList.add("btn-icon");
      btn.appendChild(ic);
    }
    if (o.label != null && o.label !== "") {
      const span = document.createElement("span");
      span.textContent = String(o.label);
      btn.appendChild(span);
    }

    if (o.busy) btn.classList.add("is-busy");
    if (o.disabled) btn.disabled = true;
    if (typeof o.onClick === "function") btn.addEventListener("click", o.onClick);
    return btn;
  }

  /*
    Unified status indicator. Convention: class "status-dot" + data-level
    ("healthy" | "warning" | "critical" | "off" | "nav"). The matching CSS
    in components.css also accepts the legacy bare class names for migration.
  */
  function statusDot(level) {
    const span = document.createElement("span");
    span.className = "status-dot";
    if (level) span.setAttribute("data-level", level);
    return span;
  }

  /*
    Generic surface card (.page-card-like): optional title row (with optional
    icon), a body slot (string -> .page-card-desc, or a DOM node appended
    directly), and an optional actions row (single node or array). Kept
    generic on purpose; call sites migrate to it later.
  */
  function card(opts) {
    const o = opts || {};
    const el = document.createElement("div");
    el.className = "page-card";

    if (o.title || o.icon) {
      const head = document.createElement("div");
      head.className = "page-card-title";
      head.style.display = "flex";
      head.style.alignItems = "center";
      head.style.gap = "8px";
      if (o.icon) head.appendChild(icon(o.icon, 15));
      if (o.title) head.appendChild(document.createTextNode(String(o.title)));
      el.appendChild(head);
    }

    if (o.body != null && o.body !== "") {
      if (typeof o.body === "string") {
        const desc = document.createElement("div");
        desc.className = "page-card-desc";
        desc.textContent = o.body;
        el.appendChild(desc);
      } else {
        el.appendChild(o.body);
      }
    }

    if (o.actions) {
      const row = document.createElement("div");
      row.style.display = "flex";
      row.style.flexWrap = "wrap";
      row.style.gap = "8px";
      row.style.marginTop = "12px";
      if (Array.isArray(o.actions)) {
        o.actions.forEach((a) => { if (a) row.appendChild(a); });
      } else {
        row.appendChild(o.actions);
      }
      el.appendChild(row);
    }

    return el;
  }

  /* Toggle the ACK-wait / in-flight state on an existing button: .is-busy
     class plus the disabled property so it dims, shows the pulse ring, and
     refuses further clicks. Pure DOM, no state. */
  function setBusy(btn, busy) {
    if (!btn) return;
    const on = !!busy;
    btn.classList.toggle("is-busy", on);
    btn.disabled = on;
  }

  /* Toggle the active/filled toggle state (e.g. PLAN engaged). */
  function setActive(btn, active) {
    if (!btn) return;
    btn.classList.toggle("is-active", !!active);
  }

  return {
    icon,
    iconButton,
    button,
    statusDot,
    card,
    setBusy,
    setActive,
  };
})();
