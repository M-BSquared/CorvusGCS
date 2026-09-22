"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.ui — the reusable UI component layer.

  Every control in the app is built here, not hand-rolled at the call site.
  Each export is a pure factory: it returns a detached DOM node (or a small
  {el, ...methods} handle for the stateful ones) and holds no module state, so
  a factory can be called from any module, in any order, and the result is the
  caller's to own. The matching CSS lives in css/components.css and reads only
  tokens from css/themes.css, which is what lets one theme switch restyle every
  control at once.

  Grouping:
    primitives  icon, iconButton, button, statusDot, badge
    forms       label, field, select, setOptions, enhanceSelect, input, toggle
                (every <select> becomes the app's dropdown — see watchSelects)
    menus       menu, menuItem — the one dropdown surface and its rows, for
                every list that opens over something (the select's own list,
                the map's context menu, the map's layer switcher)
    layout      card, section, sectionTitle, pageHeader, row, empty, actions
    pickers     optionCards, optionList, navItem
    overlays    modal, popover, infoHint
    feedback    progress, message, toast, setBusy, setActive
    charts      token, plotlyTheme, chartColors, onThemeChange, attachZoomHint
    helpers     clear, refreshIcons, uiScale (the interface-scale correction
                every geometry read has to make — see its comment)

  Version is never referenced here — the HUD/About read it from
  GET /api/version, never from a JS literal.
*/
Corvus.ui = (function () {
  /* Icon size mapped to .btn data-size so JS-created icons and the CSS in
     components.css agree (both 15 / 13 / 17 for md / sm / lg). */
  const BTN_ICON_PX = { sm: 13, md: 15, lg: 17 };

  /*
    Build a Lucide placeholder <i data-lucide="name"> with width/height set
    inline. Lucide's createIcons() swaps it for an <svg> once it is in the
    DOM, so callers must run refreshIcons() after appending. Default size 16
    matches the standalone icon usage across the app.

    Pass "auto" to skip the inline size entirely, for the icons whose
    dimensions a stylesheet already owns — an inline style would win over that
    rule and silently resize them.
  */
  function icon(name, sizePx) {
    const el = document.createElement("i");
    el.setAttribute("data-lucide", name);
    if (sizePx !== "auto") {
      const size = sizePx == null ? 16 : sizePx;
      el.style.width = size + "px";
      el.style.height = size + "px";
    }
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
    /* `className` swaps the base class for a bespoke icon-button surface that
       already exists in main.css (.mc-btn for the map controls, .nav-item for
       the left rail) so those call sites can share this factory instead of
       hand-rolling the same four lines. Default stays .icon-btn. */
    btn.className = o.className || "icon-btn";
    if (o.title) btn.title = o.title;
    /* An icon-only button has no text for a screen reader; fall back to the
       tooltip so it is never announced as an unlabelled button. */
    btn.setAttribute("aria-label", o.ariaLabel || o.title || name);
    if (o.id) btn.id = o.id;
    if (o.variant === "active") btn.classList.add("active");
    if (o.disabled) btn.disabled = true;
    btn.appendChild(icon(name, o.size || 15));
    if (typeof o.onClick === "function") btn.addEventListener("click", o.onClick);
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
    if (o.id) btn.id = o.id;
    if (o.title) btn.title = o.title;
    if (o.ariaLabel) btn.setAttribute("aria-label", o.ariaLabel);
    /* Extra classes layer a screen-specific modifier (.setup-back, .tune-tab)
       on top of the shared .btn foundation rather than replacing it. */
    if (o.className) btn.className += " " + o.className;
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

  /* ===================== forms ===================== */

  /* Field caption. `for` wires it to a control id so clicking the caption
     focuses the control; without one it degrades to a plain <span> rather
     than an orphaned <label> that reads as clickable but does nothing. */
  function label(text, opts) {
    const o = opts || {};
    const el = document.createElement(o.htmlFor ? "label" : "span");
    el.className = "field-label" + (o.className ? " " + o.className : "");
    if (o.htmlFor) el.htmlFor = o.htmlFor;
    el.textContent = String(text == null ? "" : text);
    return el;
  }

  /* Caption + control + optional hint, stacked. `inline` makes the field a
     flex child that shares a row with its siblings (the zoom min/max pair);
     `className` adds a layout modifier (.field-switch puts caption and control
     on one line). The caption becomes a real <label for> when the control
     carries an id, so clicking it focuses the control; without an id it
     degrades to a plain <span> and the control's own aria-label does the
     labelling.

     `hint` and `info` are the same sentence in two places: printed under the
     control, or folded into a hint icon beside the caption. A paragraph that
     explains a policy rather than the next keystroke belongs in `info` — left
     on the page it outweighs the setting it describes and pushes whatever
     follows out of sight. `info` takes the text, or an infoHint options object
     for a caller that wants its own title or placement. */
  function field(opts) {
    const o = opts || {};
    const el = document.createElement("div");
    el.className = "field" + (o.inline ? " field-inline" : "") +
                   (o.className ? " " + o.className : "");
    if (o.label) {
      const caption = label(o.label, { htmlFor: o.control && o.control.id });
      if (o.info) {
        /* Caption and icon share a wrapper so they stay one caption where the
           field is a row (.field-switch): as separate children the icon would
           be a third flex item and drift across to the control's side. */
        const capRow = document.createElement("div");
        capRow.className = "field-label-row";
        capRow.appendChild(caption);
        capRow.appendChild(infoHint(
          typeof o.info === "string" ? { title: o.label, text: o.info } : o.info
        ));
        el.appendChild(capRow);
      } else {
        el.appendChild(caption);
      }
    }
    if (o.control) el.appendChild(o.control);
    if (o.hint) {
      const hint = document.createElement("span");
      hint.className = "field-hint";
      hint.textContent = String(o.hint);
      el.appendChild(hint);
    }
    return el;
  }

  /* Native <select> on the shared field styling. `options` accepts either
     plain strings or {value, label, disabled} objects, so a caller with an
     API payload does not have to reshape it first. Returns the element; read
     .value as usual. */
  function select(opts) {
    const o = opts || {};
    const el = document.createElement("select");
    el.className = "field-select" + (o.className ? " " + o.className : "");
    if (o.id) el.id = o.id;
    if (o.ariaLabel) el.setAttribute("aria-label", o.ariaLabel);
    if (o.title) el.title = o.title;
    setOptions(el, o.options, o.value);
    if (o.disabled) el.disabled = true;
    if (typeof o.onChange === "function") {
      el.addEventListener("change", () => o.onChange(el.value, el));
    }
    return el;
  }

  /* Replace a <select>'s options, preserving the current selection when it
     still exists. Split out of select() because every list that reloads from
     the backend (serial ports, tile sources) needs exactly this and used to
     re-implement the "remember prev, restore if still present" dance. */
  function setOptions(el, options, value) {
    if (!el) return;
    const prev = value != null ? value : el.value;
    el.innerHTML = "";
    /* The selection is restored from the values we just appended rather than
       from `el.options`, which exists only on a real <select> — reading it made
       this helper throw anywhere the element was a stand-in for one. */
    const values = [];
    (options || []).forEach((raw) => {
      const item = (raw && typeof raw === "object") ? raw : { value: raw, label: raw };
      const opt = document.createElement("option");
      opt.value = String(item.value == null ? "" : item.value);
      opt.textContent = String(item.label == null ? item.value : item.label);
      if (item.disabled) opt.disabled = true;
      values.push(opt.value);
      el.appendChild(opt);
    });
    if (prev != null && values.indexOf(String(prev)) >= 0) {
      el.value = String(prev);
    } else if (values.length) {
      el.value = values[0];
    }
  }


  /* The one piece of module state in this file, and it earns it: a menu's
     surface is mounted away from its trigger and listens on the document, so
     knowing which one is open is not something any single instance can
     answer. */
  let openDropdown = null;

  /* Walk parents rather than call contains(): the same check works for a node
     that is no longer in the document, which is exactly the case a dismissal
     handler runs in. */
  function withinNode(node, root) {
    let n = node;
    while (n) {
      if (n === root) return true;
      n = n.parentNode;
    }
    return false;
  }

  function stopEvent(e) {
    if (e && typeof e.preventDefault === "function") e.preventDefault();
    if (e && typeof e.stopPropagation === "function") e.stopPropagation();
  }

  /* ===== menuItem — one row of the app's dropdown =====
     The row the flight bar's mode list, the map's layer switcher and the
     map's context menu all draw: a leading state dot OR an icon, a label, and
     an optional second line saying what the row will do or why it cannot be
     used. Everything about how it looks is .option-item in components.css, so
     a row is the same row wherever it is opened from.

     opts: {label, note, icon, dot, value, active, disabled, role, className,
            onSelect}
       dot       the state dot of a value picker (a chooser, not an action)
       icon      a Lucide name, for an action row; callers must refreshIcons()
       role      "option" inside a listbox, "menuitem" (the default) in a menu
       note      a second, quieter line under the label */
  function menuItem(opts) {
    const o = opts || {};
    const b = document.createElement("button");
    b.type = "button";
    b.className = "option-item" + (o.className ? " " + o.className : "");
    b.setAttribute("role", o.role || "menuitem");
    if (o.value != null) b.dataset.value = String(o.value);
    if (o.active) b.classList.add("active");
    if (o.role === "option") b.setAttribute("aria-selected", o.active ? "true" : "false");
    if (o.disabled) b.disabled = true;

    if (o.dot) {
      const dot = document.createElement("span");
      dot.className = "option-item-dot";
      b.appendChild(dot);
    }
    if (o.icon) b.appendChild(icon(o.icon, "auto"));

    const labelEl = document.createElement("span");
    labelEl.className = "option-item-label";
    labelEl.textContent = String(o.label == null ? "" : o.label);
    /* The note only earns the extra column box when there is one: a single
       line stays a single span, which is what every list of plain choices in
       the app is. */
    if (o.note) {
      const text = document.createElement("span");
      text.className = "option-item-text";
      text.appendChild(labelEl);
      const note = document.createElement("span");
      note.className = "option-item-note";
      note.textContent = String(o.note);
      text.appendChild(note);
      b.appendChild(text);
    } else {
      b.appendChild(labelEl);
    }

    if (typeof o.onSelect === "function") {
      b.addEventListener("click", () => {
        if (b.disabled) return;
        o.onSelect(o.value, b);
      });
    }
    return b;
  }

  /* ===== menu — the app's dropdown, as a surface anything can open =====
     The dropdown the app wears — glass surface, .option-item rows, one open
     list at a time, Escape and outside-press dismissal, roving focus and
     type-ahead — was written inside enhanceSelect() for the flight bar's mode
     picker. Every other list that opens over something then grew its own
     copy: the map's context menu carried its own positioning, its own
     document listeners and its own row styling, and the layer switcher had no
     dismissal beyond a click elsewhere and a top offset hard-coded to the
     rail button it was meant to hang under.

     This is that behaviour, once. The component owns the surface, where it
     goes, how it is dismissed and how the keyboard moves through it; the
     caller owns what is in it — render() fills the cleared surface and hands
     back the rows the keyboard should walk (menuItem() builds them).

     opts:
       className         extra classes on the surface
       role              "listbox" for a value picker, "menu" for actions
       ariaLabel
       host              the element (or () => element) the surface mounts in.
                         The default <body> is what keeps a list out of the
                         pane that would clip it; a surface positioned from a
                         point passes the container those pixels belong to.
       render(el)        fills the cleared surface, returns the row elements
       side              "bottom" (default) or "left" of an element anchor
       gap               pixels between the surface and its trigger (default 6),
                         or a function returning them, re-read on each place()
       matchAnchorWidth  false for a menu wider than its trigger (a rail icon)
       closeOnScroll     default true. False for a surface inside a host that
                         does not scroll and repositions itself (the map).
       autofocus         default true — opening moves the keyboard onto the
                         current row. False for a surface opened by hover,
                         where taking the focus would be a decision the
                         operator did not make.
       typeahead         default true
       onOpen() / onClose()

     open(anchor) takes {el} — a trigger the list drops out of — or {x, y}, a
     point in the host's own pixels, which is what a menu anchored to a place
     on the map needs. Both are re-measured by place(), so a caller whose
     anchor moves (a panning map) calls place() with the new one. */
  function menu(opts) {
    const o = opts || {};

    const el = document.createElement("div");
    el.className = "ui-menu glass" + (o.className ? " " + o.className : "");
    el.setAttribute("role", o.role || "menu");
    if (o.ariaLabel) el.setAttribute("aria-label", o.ariaLabel);

    let rows = [];
    let opened = false;
    let cursor = -1;
    let anchor = null;
    let hostEl = null;
    let typed = "";
    let typedAt = 0;

    function resolveHost() {
      const h = typeof o.host === "function" ? o.host() : o.host;
      if (h) return h;
      return typeof document !== "undefined" ? document.body : null;
    }

    /* The row the keyboard starts on: the current choice if the list has one,
       the first usable row otherwise. A value picker marks its selection
       .active, an action menu marks nothing — so one rule covers both. */
    function build() {
      clear(el);
      const built = typeof o.render === "function" ? o.render(el) : [];
      rows = Array.prototype.slice.call(built || []);
      cursor = -1;
      rows.forEach((b, i) => {
        if (cursor < 0 && b && b.classList && b.classList.contains("active")) cursor = i;
      });
      return rows;
    }

    /* Fixed, measured from the trigger: below it by default, flipped above
       when the list would not fit, and capped to the space it has so a long
       list scrolls instead of running off the screen. "left" is the same
       thing on the other axis, for a menu opened from a rail against the edge
       of the window.

       Every length below is UNSCALED — the space style.left writes in. The
       trigger's rect and the window's size are both read in scaled pixels and
       divided back into it (see uiScale), while el.offsetWidth/scrollHeight
       are already unscaled and are used as they come. Without that division
       the surface was placed at its scaled coordinates and the zoom scaled
       them AGAIN: at 150% the map's layer menu opened 366px the wrong side of
       its button. GAP and EDGE stay unscaled on purpose, so the gap under a
       dropdown grows with the interface exactly like every other padding. */
    function placeByElement(trigger) {
      if (typeof trigger.getBoundingClientRect !== "function") return;
      const k = uiScale();
      const r = unscaledRect(trigger, k);
      const vh = (window.innerHeight || 800) / k;
      const vw = (window.innerWidth || 1200) / k;
      /* How far the surface stands off its trigger. The default suits a list
         dropped under a control inside a panel, where the two read as one
         thing. A surface that hangs BESIDE a floating rail is a separate
         object next to another separate object, and at six pixels the two
         glass edges look stuck together rather than adjacent.

         A function is re-read on every placement, which is what a caller
         needs when the distance depends on something measured — the map
         rail's popovers clear the RAIL, not the button inside it, and only
         the live layout knows how far apart those two are. */
      const GAP = typeof o.gap === "function" ? Number(o.gap()) || 0
        : (typeof o.gap === "number" ? o.gap : 6);
      const EDGE = 8;
      if (o.matchAnchorWidth !== false) el.style.minWidth = Math.round(r.width) + "px";

      if (o.side === "left") {
        const w = el.offsetWidth || 0;
        const h = el.offsetHeight || el.scrollHeight || 0;
        el.style.maxHeight = Math.max(120, Math.round(vh - 2 * EDGE)) + "px";
        const flip = r.left - GAP - w < EDGE && vw - r.right - GAP - w >= EDGE;
        const left = flip ? r.right + GAP : r.left - GAP - w;
        el.style.left = Math.round(Math.max(EDGE, Math.min(left, vw - w - EDGE))) + "px";
        el.style.top = Math.round(Math.max(EDGE, Math.min(r.top, vh - h - EDGE))) + "px";
        el.dataset.placement = flip ? "right" : "left";
        return;
      }

      const below = vh - r.bottom - GAP - EDGE;
      const above = r.top - GAP - EDGE;
      const wanted = el.scrollHeight || el.offsetHeight || 0;
      const up = below < Math.min(wanted, 180) && above > below;
      el.style.maxHeight = Math.max(120, Math.round(up ? above : below)) + "px";
      const h = el.offsetHeight || wanted;
      const w = el.offsetWidth || r.width;
      const wantLeft = o.align === "end" ? r.right - w : r.left;
      el.style.top = Math.round(up ? Math.max(EDGE, r.top - GAP - h) : r.bottom + GAP) + "px";
      el.style.left = Math.round(Math.max(EDGE, Math.min(wantLeft, vw - w - EDGE))) + "px";
      el.dataset.placement = up ? "top" : "bottom";
    }

    /* Positioned in the host's own pixels, next to a point inside it: down and
       right of it by default, flipped on whichever axis would overflow, and
       clamped back inside when the surface is too large to fit on either side
       (a small window, a large interface scale) rather than pushed off the
       edge. It grows out of the corner nearest the point. */
    function placeByPoint(x, y) {
      if (!hostEl) return;
      const hostW = hostEl.clientWidth;
      const hostH = hostEl.clientHeight;
      const w = el.offsetWidth;
      const h = el.offsetHeight;
      const GAP = 12;
      const EDGE = 4;
      const flipX = x + GAP + w > hostW && x - GAP - w >= 0;
      const flipY = y + GAP + h > hostH && y - GAP - h >= 0;
      let left = flipX ? x - GAP - w : x + GAP;
      let top = flipY ? y - GAP - h : y + GAP;
      left = Math.max(EDGE, Math.min(left, hostW - w - EDGE));
      top = Math.max(EDGE, Math.min(top, hostH - h - EDGE));
      el.style.left = `${left}px`;
      el.style.top = `${top}px`;
      el.style.transformOrigin =
        `${flipX ? "right" : "left"} ${flipY ? "bottom" : "top"}`;
    }

    /* Re-measure and reposition. `next` replaces the anchor, which is how a
       menu anchored to a ground point follows it while the map pans. An
       anchor that does not parse leaves the current one alone — place() is
       also the window's resize handler, and that hands it an Event. */
    function place(next) {
      const moved = normalizeAnchor(next);
      if (moved) anchor = moved;
      if (!opened || !anchor) return;
      if (anchor.el) placeByElement(anchor.el);
      else placeByPoint(anchor.x, anchor.y);
    }
    function onResize() { place(); }

    function normalizeAnchor(at) {
      if (!at) return null;
      if (at.el) return { el: at.el };
      if (typeof at.x === "number" && typeof at.y === "number") {
        return { x: at.x, y: at.y };
      }
      /* A bare element is the common case written the short way. */
      if (typeof at.getBoundingClientRect === "function" || at.nodeType === 1) {
        return { el: at };
      }
      return null;
    }

    function open(at) {
      if (opened) close(false);
      const next = normalizeAnchor(at);
      const host = resolveHost();
      if (!next || !host) return null;
      anchor = next;
      hostEl = host;
      build();
      /* Nothing to offer is nothing to open — an empty surface is a floating
         rectangle the operator has to dismiss for no reason. */
      if (!rows.length) return null;

      /* One list at a time, like a native select: a second menu opened from
         the keyboard would otherwise sit under the first and swallow its
         keys, since both listen on the document. */
      if (openDropdown && openDropdown !== handle) openDropdown.close();

      hostEl.appendChild(el);
      opened = true;
      openDropdown = handle;
      el.dataset.anchor = anchor.el ? "element" : "point";
      /* Lucide swaps the <i> placeholders for <svg> in place, so this has to
         run BEFORE the first measurement — an unswapped placeholder has no
         width and the surface would be positioned from the wrong size. */
      refreshIcons();
      place();
      document.addEventListener("pointerdown", onOutside, true);
      document.addEventListener("keydown", onKey, true);
      if (o.closeOnScroll !== false) document.addEventListener("scroll", onScroll, true);
      if (typeof window !== "undefined" && window.addEventListener) {
        window.addEventListener("resize", onResize);
      }
      if (typeof o.onOpen === "function") o.onOpen();
      /* A surface the operator opened by POINTING at something must not take
         the focus off whatever they were using — a hover is not a decision to
         move the keyboard. The arrow keys still reach into it (onKey is on the
         document while it is open), so it stays operable either way. */
      if (o.autofocus !== false) focusRow(cursor >= 0 ? cursor : nextEnabled(-1, 1));
      return handle;
    }

    function close(refocus) {
      if (!opened) return;
      opened = false;
      typed = "";
      document.removeEventListener("pointerdown", onOutside, true);
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("scroll", onScroll, true);
      if (typeof window !== "undefined" && window.removeEventListener) {
        window.removeEventListener("resize", onResize);
      }
      if (openDropdown === handle) openDropdown = null;
      if (el.parentNode) el.parentNode.removeChild(el);
      const back = refocus && anchor && anchor.el;
      if (typeof o.onClose === "function") o.onClose();
      if (back && typeof anchor.el.focus === "function") anchor.el.focus();
    }

    /* Rebuild the rows under an open surface — a repopulated mode list, a row
       whose availability changed. Dropping the rows drops the one the
       keyboard was on, so it goes back to the current choice rather than
       letting focus fall to the body. */
    function rebuild() {
      if (!opened) return;
      build();
      place();
      if (o.autofocus !== false) focusRow(cursor >= 0 ? cursor : nextEnabled(-1, 1));
    }

    function nextEnabled(from, step) {
      if (!rows.length) return -1;
      for (let n = 1; n <= rows.length; n++) {
        const i = (from + step * n + rows.length * rows.length) % rows.length;
        if (!rows[i].disabled) return i;
      }
      return -1;
    }

    function focusRow(i) {
      if (i < 0 || i >= rows.length) return;
      cursor = i;
      const b = rows[i];
      if (typeof b.focus === "function") b.focus();
      if (typeof b.scrollIntoView === "function") b.scrollIntoView({ block: "nearest" });
    }

    /* The surface is positioned once, from its anchor. Scrolling the pane the
       anchor sits in would leave it hanging in the wrong place, so it closes
       instead — scrolling the surface itself excepted. */
    function onScroll(e) {
      if (e && withinNode(e.target, el)) return;
      close(false);
    }

    /* A press on the trigger is not an outside press: the trigger toggles on
       click, and closing here first would make it reopen immediately. */
    function onOutside(e) {
      const t = e && e.target;
      if (withinNode(t, el)) return;
      if (anchor && anchor.el && withinNode(t, anchor.el)) return;
      close(false);
    }

    /* Captured on the document: the map and the modals listen for Escape and
       arrow keys too, and a key aimed at an open menu must not reach them.
       Registered only while the surface is open. */
    function onKey(e) {
      const key = e.key;
      if (key === "Escape") {
        stopEvent(e);
        close(true);
        return;
      }
      if (key === "Tab") {
        close(false);
        return;
      }
      if (key === "ArrowDown") { stopEvent(e); focusRow(nextEnabled(cursor, 1)); return; }
      if (key === "ArrowUp") { stopEvent(e); focusRow(nextEnabled(cursor, -1)); return; }
      if (key === "Home") { stopEvent(e); focusRow(nextEnabled(-1, 1)); return; }
      if (key === "End") { stopEvent(e); focusRow(nextEnabled(rows.length, -1)); return; }
      /* Type-ahead: PX4's mode list is long and its names are distinct, so a
         letter should jump the way it does in a native select. */
      if (o.typeahead === false) return;
      if (key && key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
        const now = Date.now();
        typed = (now - typedAt < 800 ? typed : "") + key.toLowerCase();
        typedAt = now;
        const starts = (b) => (b.textContent || "").toLowerCase().indexOf(typed) === 0;
        const hit = rows.findIndex((b, i) => !b.disabled && i !== cursor && starts(b));
        const wrapHit = hit >= 0 ? hit : rows.findIndex((b) => !b.disabled && starts(b));
        if (wrapHit >= 0) { stopEvent(e); focusRow(wrapHit); }
      }
    }

    const handle = {
      el,
      open,
      close,
      place,
      rebuild,
      isOpen: () => opened,
      rows: () => rows.slice(),
      /* What the surface belongs to, for the watcher that takes a menu down
         when the screen under it is re-rendered away. */
      anchorNode: () => (anchor && anchor.el) || hostEl,
    };
    return handle;
  }

  /* The <label for> that names a select, or null. Searched from the tree's
     root rather than from the select's own parent: the caption and the
     control are siblings in ui.field(), but index.html's serial-port field
     puts the select one level deeper, inside the .link-row it shares with the
     refresh button — a parent-only lookup missed exactly that one. An id is
     unique, so the root is the right and only scope to search, and taking it
     from getRootNode() means a subtree that is not mounted yet works too. */
  function labelOwner(sel) {
    let root = typeof sel.getRootNode === "function" ? sel.getRootNode() : null;
    if (!root) {
      root = sel;
      while (root.parentNode) root = root.parentNode;
    }
    if (!root || typeof root.querySelector !== "function") return null;
    return root.querySelector('label[for="' + sel.id + '"]');
  }

  /* ===== enhanceSelect — the app's own dropdown, over a native <select> =====
     A <select>'s open popup is the one control the design system cannot
     reach: the option list is drawn by the operating system, so it keeps the
     platform's white sheet, radius and font whatever theme is on — the flight
     bar's mode picker was a chrome-grey list dropped on a glass bar.

     This replaces the popup, and only the popup. The <select> stays in the DOM
     and stays the state (value, options, disabled, the "change" event), so
     every caller keeps talking to a plain select: nothing downstream changes,
     and a browser without the enhancement still gets a working control.

     The list itself is a ui.menu — the same surface, rows, dismissal and
     keyboard as the map's context menu and the layer switcher; this function
     is only the part that is specific to standing in for a <select>. */
  function enhanceSelect(sel) {
    if (!sel) return null;
    if (sel.corvusSelect) return sel.corvusSelect;
    const parent = sel.parentNode;
    if (!parent) return null;

    /* The trigger inherits the select's own classes (.field-select,
       .mode-selector, …) so every width, font and colour rule already written
       for the control applies to it unchanged. Read them before the native
       element is marked hidden. */
    const inherited = sel.className;

    const wrap = document.createElement("div");
    wrap.className = "ui-select";
    parent.insertBefore(wrap, sel);
    wrap.appendChild(sel);
    sel.classList.add("ui-select-native");
    sel.setAttribute("aria-hidden", "true");
    sel.tabIndex = -1;

    const trigger = document.createElement("button");
    trigger.type = "button";
    trigger.className = (inherited ? inherited + " " : "") + "ui-select-trigger";
    trigger.setAttribute("role", "combobox");
    trigger.setAttribute("aria-haspopup", "listbox");
    trigger.setAttribute("aria-expanded", "false");
    const name = sel.getAttribute("aria-label") || sel.title || "";
    if (name) {
      trigger.setAttribute("aria-label", name);
      trigger.title = name;
    }
    /* A <label for> written against the select now points at a hidden
       element: clicking it would open nothing, and the trigger would have no
       accessible name. The association moves to the trigger, which takes an
       id derived from the select's rather than the id itself — several
       modules still look the state up with getElementById(sel.id). */
    const owner = sel.id ? labelOwner(sel) : null;
    if (owner) {
      trigger.id = sel.id + "-trigger";
      owner.htmlFor = trigger.id;
    }
    const labelEl = document.createElement("span");
    labelEl.className = "ui-select-label";
    trigger.appendChild(labelEl);
    wrap.appendChild(trigger);

    function options() {
      const list = sel.options || sel.querySelectorAll("option");
      return Array.prototype.slice.call(list || []);
    }
    function textOf(opt) {
      const t = opt.textContent;
      return String(t == null || t === "" ? opt.value : t);
    }

    function syncTrigger() {
      const chosen = options().filter((o) => String(o.value) === String(sel.value))[0];
      labelEl.textContent = chosen ? textOf(chosen) : "";
      /* An empty value is the placeholder row ("SELECT MODE"), which is a
         prompt and not a choice, so it is dimmed like one. */
      trigger.dataset.placeholder = !chosen || String(chosen.value) === "" ? "true" : "false";
      trigger.disabled = !!sel.disabled;
      if (sel.disabled) list.close(false);
    }

    /* The rows ARE .option-item — the same rows as the map's layer switcher —
       so the list reads as a member of the family instead of a second,
       nearly-identical menu style. */
    function buildRows(surface) {
      return options().map((o) => {
        const row = menuItem({
          role: "option",
          dot: true,
          value: o.value,
          label: textOf(o),
          active: String(o.value) === String(sel.value),
          disabled: !!o.disabled,
          onSelect: () => choose(o.value),
        });
        surface.appendChild(row);
        return row;
      });
    }

    function choose(value) {
      const changed = String(sel.value) !== String(value);
      list.close(true);
      sel.value = String(value);
      syncTrigger();
      /* Native selects fire "change" only when the value actually moved;
         keeping that contract is what lets a caller swap a <select> for this
         without re-reading its own handler. */
      if (changed && typeof sel.dispatchEvent === "function") {
        sel.dispatchEvent(makeChangeEvent());
      }
    }

    function makeChangeEvent() {
      if (typeof Event === "function") return new Event("change", { bubbles: true });
      return { type: "change", bubbles: true };
    }

    const list = menu({
      className: "ui-select-menu option-list",
      role: "listbox",
      ariaLabel: name,
      render: buildRows,
      onOpen: () => trigger.setAttribute("aria-expanded", "true"),
      onClose: () => trigger.setAttribute("aria-expanded", "false"),
    });

    function openList() {
      if (sel.disabled) return;
      list.open({ el: trigger });
    }

    trigger.addEventListener("click", () => {
      if (list.isOpen()) list.close(true); else openList();
    });
    trigger.addEventListener("keydown", (e) => {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        stopEvent(e);
        openList();
      }
    });
    /* Anything that changes the select — a repopulated mode list, a restored
       selection, the disabled flag app.js sets while PX4 acknowledges — has
       to reach the trigger, and none of it is an event we could listen for. */
    sel.addEventListener("change", syncTrigger);
    if (typeof MutationObserver === "function") {
      const mo = new MutationObserver(() => {
        syncTrigger();
        list.rebuild();
      });
      mo.observe(sel, { childList: true, subtree: true, attributes: true, attributeFilter: ["disabled"] });
    }

    syncTrigger();

    const handle = {
      el: wrap, trigger, menu: list.el, select: sel,
      open: openList,
      close: () => list.close(false),
      refresh: syncTrigger,
      isOpen: list.isOpen,
    };
    sel.corvusSelect = handle;
    return handle;
  }

  /* ===== enhanceSelects — the dropdown, adopted by the whole UI =====
     enhanceSelect() was written for one control, the flight bar's mode
     picker, and only that control ever called it. Every other <select> kept
     the operating system's popup, so a single screen could show both: the
     logs toolbar's "Sort" picker dropped a white platform list out of a glass
     bar styled in the theme's colours.

     The fix is not fifteen extra call sites (and a plugin's select left
     behind anyway) but applying the enhancement where selects appear — once
     over the document, then to whatever is added to it afterwards. A caller
     keeps writing Corvus.ui.select() or plain <select> markup and gets the
     app's dropdown without knowing this exists.

     Two selects are left native: a list box (`multiple`, or size > 1), which
     this dropdown does not model, and anything marked data-native-select,
     for the case where the platform control is the right answer. */
  function enhanceSelects(root) {
    const scope = root || (typeof document !== "undefined" ? document : null);
    if (!scope || typeof scope.querySelectorAll !== "function") return;
    const found = scope.querySelectorAll("select");
    Array.prototype.forEach.call(found || [], enhanceIfPlain);
  }

  function enhanceIfPlain(sel) {
    if (!sel || sel.corvusSelect) return;
    if (sel.multiple || (sel.size && sel.size > 1)) return;
    if (typeof sel.hasAttribute === "function" && sel.hasAttribute("data-native-select")) return;
    enhanceSelect(sel);
  }

  let selectWatcher = null;

  /* Enhance what is on the page now, and keep enhancing: every setup screen,
     modal and plugin builds its controls long after boot, and several rebuild
     them on each render. The observer costs one pass over the added subtree;
     enhanceIfPlain() is idempotent, so the wrapper enhanceSelect() itself
     inserts comes back through here and is ignored.

     Enhancement is a microtask behind the insertion, which is one frame in
     which the native control can paint. Calling enhanceSelects() directly
     after building a control avoids even that, and nothing needs to. */
  function watchSelects() {
    enhanceSelects(document);
    if (selectWatcher || typeof MutationObserver !== "function") return;
    const body = document.body;
    if (!body) return;
    selectWatcher = new MutationObserver((records) => {
      records.forEach((rec) => {
        Array.prototype.forEach.call(rec.addedNodes || [], (node) => {
          if (!node || node.nodeType !== 1) return;
          if (node.tagName === "SELECT") enhanceIfPlain(node);
          else enhanceSelects(node);
        });
        /* A re-render that replaces the control while its list is open would
           otherwise leave the list on <body>, hanging over a trigger that no
           longer exists — the menu is not inside the subtree that was
           removed, so nothing else would take it down. */
        if (openDropdown && (rec.removedNodes || []).length) closeOrphanedDropdown();
      });
    });
    selectWatcher.observe(body, { childList: true, subtree: true });
  }

  function closeOrphanedDropdown() {
    const open = openDropdown;
    if (!open) return;
    const root = open.anchorNode();
    if (typeof document.contains === "function" ? document.contains(root) : true) return;
    open.close();
  }

  /* Text/number input on the shared field styling. `mono` switches to the
     tabular monospace face used for numeric and connection-string fields. */
  function input(opts) {
    const o = opts || {};
    const el = document.createElement("input");
    el.type = o.type || "text";
    el.className = "field-input" + (o.mono ? " field-input-mono" : "") +
                   (o.className ? " " + o.className : "");
    if (o.id) el.id = o.id;
    if (o.value != null) el.value = String(o.value);
    if (o.placeholder) el.placeholder = o.placeholder;
    if (o.ariaLabel) el.setAttribute("aria-label", o.ariaLabel);
    if (o.title) el.title = o.title;
    if (o.min != null) el.min = o.min;
    if (o.max != null) el.max = o.max;
    if (o.step != null) el.step = o.step;
    if (o.disabled) el.disabled = true;
    if (o.autocomplete === false) el.autocomplete = "off";
    if (o.spellcheck === false) el.spellcheck = false;
    if (typeof o.onInput === "function") el.addEventListener("input", () => o.onInput(el.value, el));
    if (typeof o.onChange === "function") el.addEventListener("change", () => o.onChange(el.value, el));
    return el;
  }

  /* On/off switch: a real <button role="switch"> rather than a styled
     checkbox, so it keeps keyboard and screen-reader semantics without a
     hidden input to keep in sync. `onChange` receives the NEW state and may
     return a promise; while that promise is pending the switch is disabled
     and shows the state it is moving to, and a rejection snaps it back — an
     operator must never be left looking at an "on" switch that the backend
     refused. Returns {el, setValue, getValue}. */
  function toggle(opts) {
    const o = opts || {};
    let value = !!o.value;
    let busy = false;

    const el = document.createElement("button");
    el.type = "button";
    el.className = "ui-toggle" + (o.className ? " " + o.className : "");
    el.setAttribute("role", "switch");
    if (o.id) el.id = o.id;
    if (o.ariaLabel) el.setAttribute("aria-label", o.ariaLabel);
    if (o.title) el.title = o.title;

    const track = document.createElement("span");
    track.className = "ui-toggle-track";
    const knob = document.createElement("span");
    knob.className = "ui-toggle-knob";
    track.appendChild(knob);
    el.appendChild(track);

    function paint() {
      el.classList.toggle("on", value);
      el.setAttribute("aria-checked", value ? "true" : "false");
      el.disabled = !!o.disabled || busy;
    }

    function setValue(next) {
      value = !!next;
      paint();
    }

    el.addEventListener("click", () => {
      if (el.disabled) return;
      const next = !value;
      const previous = value;
      setValue(next);
      if (typeof o.onChange !== "function") return;
      const result = o.onChange(next);
      if (!result || typeof result.then !== "function") return;
      busy = true;
      paint();
      result.then(
        () => { busy = false; paint(); },
        () => { busy = false; setValue(previous); },
      );
    });

    paint();
    return { el, setValue, getValue: () => value };
  }

  /* Stepped slider with step indicators: a rail, a dot at every step, and a
     clickable label under each dot. `steps` is an ordered array of
     {value, label} — the control is an index into it, never a free number, so
     a caller gets back one of the values it supplied and nothing between them.

     Two callbacks because dragging and committing are different events:
     `onInput` fires on every step the thumb crosses (live preview) and
     `onChange` only when the operator lets go (persist). A label click fires
     both, in that order.

     Returns {el, setValue, getValue}. */
  function slider(opts) {
    const o = opts || {};
    const steps = (o.steps || []).map((raw) =>
      (raw && typeof raw === "object") ? raw : { value: raw, label: String(raw) });
    const last = Math.max(0, steps.length - 1);

    /* Nearest step to *v*, so a persisted value from another build (or a
       hand-edited config) lands on a real step instead of nowhere. */
    function indexOf(v) {
      const n = Number(v);
      if (!steps.length) return 0;
      if (!isFinite(n)) return 0;
      let best = 0;
      let bestGap = Infinity;
      steps.forEach((st, i) => {
        const gap = Math.abs(Number(st.value) - n);
        if (gap < bestGap) { bestGap = gap; best = i; }
      });
      return best;
    }

    let index = indexOf(o.value);

    const el = document.createElement("div");
    el.className = "ui-slider" + (o.className ? " " + o.className : "");

    const row = document.createElement("div");
    row.className = "ui-slider-row";

    const rail = document.createElement("div");
    rail.className = "ui-slider-rail";
    const fill = document.createElement("div");
    fill.className = "ui-slider-fill";
    const ticks = document.createElement("div");
    ticks.className = "ui-slider-ticks";
    ticks.setAttribute("aria-hidden", "true");

    const range = document.createElement("input");
    range.type = "range";
    range.className = "ui-slider-input";
    range.min = "0";
    range.max = String(last);
    range.step = "1";
    if (o.id) range.id = o.id;
    if (o.ariaLabel) range.setAttribute("aria-label", o.ariaLabel);
    if (o.disabled) range.disabled = true;

    const scale = document.createElement("div");
    scale.className = "ui-slider-scale";

    const tickEls = [];
    const stepEls = [];
    steps.forEach((st, i) => {
      const pos = last ? i / last : 0;

      const dot = document.createElement("span");
      dot.className = "ui-slider-tick";
      dot.style.setProperty("--tick-pos", String(pos));
      ticks.appendChild(dot);
      tickEls.push(dot);

      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "ui-slider-step";
      btn.style.setProperty("--tick-pos", String(pos));
      btn.textContent = String(st.label == null ? st.value : st.label);
      /* The range input already announces the step; a second tab stop per
         step would make the control N+1 stops for no added reach. */
      btn.tabIndex = -1;
      btn.addEventListener("click", () => {
        if (range.disabled || i === index) return;
        apply(i);
        emit("onInput");
        emit("onChange");
      });
      scale.appendChild(btn);
      stepEls.push(btn);
    });

    row.append(rail, fill, ticks, range);
    el.append(row, scale);

    function current() { return steps[index] || { value: o.value, label: "" }; }

    function emit(name) {
      const fn = o[name];
      if (typeof fn === "function") fn(current().value, current());
    }

    /* Paint index *i* onto every part of the control. Separated from the
       event handlers so setValue() and a drag go through the same path. */
    function apply(i) {
      index = Math.min(Math.max(i, 0), last);
      const pos = last ? index / last : 0;
      range.value = String(index);
      el.style.setProperty("--slider-pos", String(pos));
      range.setAttribute("aria-valuetext", String(current().label || current().value));
      tickEls.forEach((d, n) => d.classList.toggle("is-passed", n <= index));
      stepEls.forEach((b, n) => {
        const on = n === index;
        b.classList.toggle("is-current", on);
        b.setAttribute("aria-current", on ? "true" : "false");
      });
    }

    range.addEventListener("input", () => {
      const next = Number(range.value);
      if (next === index) return;
      apply(next);
      emit("onInput");
    });
    /* Commit on release. Keyboard arrows fire input+change together, so a
       key press persists immediately; a drag persists once, at the end. */
    range.addEventListener("change", () => {
      apply(Number(range.value));
      emit("onChange");
    });

    apply(index);

    return {
      el,
      setValue: (v) => apply(indexOf(v)),
      getValue: () => current().value,
    };
  }

  /* ===================== layout ===================== */

  /* Page title + one-line subtitle. Text is set through textContent, never
     innerHTML, so a value that came from the backend cannot inject markup. */
  function pageHeader(title, subtitle) {
    const el = document.createElement("div");
    el.className = "page-header";
    const t = document.createElement("div");
    t.className = "page-title";
    t.textContent = String(title == null ? "" : title);
    el.appendChild(t);
    if (subtitle) {
      const s = document.createElement("div");
      s.className = "page-subtitle";
      s.textContent = String(subtitle);
      el.appendChild(s);
    }
    return el;
  }

  function sectionTitle(text) {
    const el = document.createElement("div");
    el.className = "page-section-title";
    el.textContent = String(text == null ? "" : text);
    return el;
  }

  /* A titled page section. `body` is appended as-is when it is a node; when
     it is a string it becomes a card with that description, which is the
     shape every "nothing here yet" section in the app wanted. */
  function section(opts) {
    const o = opts || {};
    const el = document.createElement("div");
    el.className = "page-section";
    if (o.title) el.appendChild(sectionTitle(o.title));
    const body = (typeof o.body === "string") ? card({ body: o.body }) : o.body;
    if (body) el.appendChild(body);
    return el;
  }

  /* Key/value row. Returns the row; the value node is reachable through
     .querySelector(".page-row-value") for live updates. */
  function row(labelText, value) {
    const el = document.createElement("div");
    el.className = "page-row";
    const l = document.createElement("span");
    l.className = "page-row-label";
    l.textContent = String(labelText == null ? "" : labelText);
    const v = document.createElement("span");
    v.className = "page-row-value";
    v.textContent = String(value == null ? "" : value);
    el.appendChild(l);
    el.appendChild(v);
    return el;
  }

  /* Muted placeholder line for an empty list / unavailable data. */
  function empty(text) {
    const el = document.createElement("div");
    el.className = "page-card-desc";
    el.textContent = String(text == null ? "" : text);
    return el;
  }

  /* Horizontal button group. Nullish entries are skipped so a caller can
     write [saveBtn, canDelete && deleteBtn] without filtering first. */
  function actions(children, opts) {
    const o = opts || {};
    const el = document.createElement("div");
    el.className = "ui-actions" + (o.stretch ? " ui-actions-stretch" : "");
    (Array.isArray(children) ? children : [children]).forEach((c) => {
      if (c) el.appendChild(c);
    });
    return el;
  }

  /* ===================== pickers ===================== */

  /*
    Grid of selectable cards — the picker behind the theme chooser and the map
    service chooser. Each option is {id, label, desc, swatch}; `swatch` is an
    array of CSS colors rendered as a stacked preview chip (a theme's palette)
    or a single color string. Exactly one card carries .selected at a time and
    the group is a real radiogroup, so arrow keys and screen readers work.
    Returns {el, setValue, getValue}.
  */
  function optionCards(opts) {
    const o = opts || {};
    const el = document.createElement("div");
    el.className = "option-cards";
    el.setAttribute("role", "radiogroup");
    if (o.ariaLabel) el.setAttribute("aria-label", o.ariaLabel);
    if (o.columns) el.style.setProperty("--option-columns", String(o.columns));

    let value = o.value;
    const cards = [];

    (o.options || []).forEach((op) => {
      const c = document.createElement("button");
      c.type = "button";
      c.className = "option-card";
      c.dataset.value = op.id;
      c.setAttribute("role", "radio");

      if (op.swatch) {
        const sw = document.createElement("span");
        sw.className = "option-card-swatch";
        const colors = Array.isArray(op.swatch) ? op.swatch : [op.swatch];
        colors.forEach((color) => {
          const chip = document.createElement("span");
          chip.className = "option-card-chip";
          chip.style.background = color;
          sw.appendChild(chip);
        });
        c.appendChild(sw);
      } else if (op.icon) {
        const ic = icon(op.icon, 16);
        ic.classList.add("option-card-icon");
        c.appendChild(ic);
      }

      const text = document.createElement("span");
      text.className = "option-card-text";
      const name = document.createElement("span");
      name.className = "option-card-label";
      name.textContent = String(op.label == null ? op.id : op.label);
      text.appendChild(name);
      if (op.desc) {
        const d = document.createElement("span");
        d.className = "option-card-desc";
        d.textContent = String(op.desc);
        text.appendChild(d);
      }
      c.appendChild(text);

      c.addEventListener("click", () => {
        setValue(op.id);
        if (typeof o.onChange === "function") o.onChange(op.id);
      });
      cards.push(c);
      el.appendChild(c);
    });

    function setValue(next) {
      value = next;
      cards.forEach((c) => {
        const on = c.dataset.value === String(next);
        c.classList.toggle("selected", on);
        c.setAttribute("aria-checked", on ? "true" : "false");
        /* Roving tabindex: only the checked radio is tabbable, so the group
           is one stop in the tab order rather than N. */
        c.tabIndex = on ? 0 : -1;
      });
      /* Nothing matched (e.g. a persisted id that no longer exists) — leave
         the first card reachable so the group is not a tab dead end. */
      if (!cards.some((c) => c.classList.contains("selected")) && cards.length) {
        cards[0].tabIndex = 0;
      }
    }
    setValue(value);

    return { el, setValue, getValue: () => value };
  }

  /*
    Vertical list of single-line choices with a leading state dot — the map
    layer switcher's shape. Same contract as optionCards: exactly one active,
    returns {el, setValue, getValue}.
  */
  function optionList(opts) {
    const o = opts || {};
    const el = document.createElement("div");
    el.className = "option-list";
    el.setAttribute("role", "radiogroup");
    if (o.ariaLabel) el.setAttribute("aria-label", o.ariaLabel);

    let value = o.value;
    const items = [];

    (o.options || []).forEach((op) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "option-item";
      b.dataset.value = op.id;
      b.setAttribute("role", "radio");
      const dot = document.createElement("span");
      dot.className = "option-item-dot";
      b.appendChild(dot);
      const text = document.createElement("span");
      text.className = "option-item-label";
      text.textContent = String(op.label == null ? op.id : op.label);
      b.appendChild(text);
      b.addEventListener("click", () => {
        setValue(op.id);
        if (typeof o.onChange === "function") o.onChange(op.id);
      });
      items.push(b);
      el.appendChild(b);
    });

    function setValue(next) {
      value = next;
      items.forEach((b) => {
        const on = b.dataset.value === String(next);
        b.classList.toggle("active", on);
        b.setAttribute("aria-checked", on ? "true" : "false");
        b.tabIndex = on ? 0 : -1;
      });
      if (!items.some((b) => b.classList.contains("active")) && items.length) {
        items[0].tabIndex = 0;
      }
    }
    setValue(value);

    return { el, setValue, getValue: () => value };
  }

  /*
    Large clickable tile: icon chip, title, optional description, optional
    trailing chevron. The Setup grid and the Plugins grid are the same control
    laid out differently (row vs column), so they share this factory and the
    .tile interaction recipe in components.css; `className` adds the layout
    modifier (.setup-tile / .plugin-card) on top.
  */
  function tile(opts) {
    const o = opts || {};
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "tile" + (o.className ? " " + o.className : "");
    if (o.ariaLabel || o.title) btn.setAttribute("aria-label", o.ariaLabel || o.title);

    if (o.icon) {
      const wrap = document.createElement("span");
      wrap.className = "tile-icon";
      /* "auto" so the per-layout icon size in components.css/main.css wins —
         the two grids use different icon sizes. */
      wrap.appendChild(icon(o.icon, "auto"));
      btn.appendChild(wrap);
    }

    const body = document.createElement("div");
    body.className = "tile-body";
    if (o.title) {
      const h = document.createElement("span");
      h.className = "tile-title";
      h.textContent = String(o.title);
      body.appendChild(h);
    }
    if (o.desc) {
      const d = document.createElement("span");
      d.className = "tile-desc";
      d.textContent = String(o.desc);
      body.appendChild(d);
    }
    btn.appendChild(body);

    if (o.chevron) btn.appendChild(icon("chevron-right", "auto"));
    if (typeof o.onClick === "function") btn.addEventListener("click", o.onClick);
    return btn;
  }

  /* Left-rail navigation button (icon over label). */
  function navItem(opts) {
    const o = opts || {};
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "nav-item" + (o.active ? " active" : "");
    btn.dataset.nav = o.id;
    if (o.title) btn.title = o.title;
    btn.appendChild(icon(o.icon));
    const lbl = document.createElement("span");
    lbl.className = "nav-label";
    lbl.textContent = String(o.label == null ? "" : o.label);
    btn.appendChild(lbl);
    if (typeof o.onClick === "function") btn.addEventListener("click", o.onClick);
    return btn;
  }

  /* ===================== overlays ===================== */

  /*
    Modal dialog: dimmed scrim + centred glass panel. One implementation for
    every dialog in the app (SSH add, motor-calibration confirm, offline map,
    parameter export), which previously each hand-rolled their own overlay,
    scrim, close button and Escape handling.

    opts: {title, body, actions, size, onClose, dismissable, mount}
      body       node (or string -> a muted paragraph)
      actions    node/array appended to a footer row; omit for a bodiless dialog
      size       "sm" | "md" | "lg" — dialog width
      onClose    called after the dialog is removed, for any reason
      dismissable  false pins the dialog open: no backdrop click, no Escape,
                 no close button. For a confirm the operator must answer.
      mount      parent to append to (default document.body). The overlay is
                 position:fixed, so it covers the viewport whichever parent it
                 hangs from; a screen that tears its own subtree down passes
                 its container so the dialog goes with it.

    Returns {el, body, close}. The dialog is NOT mounted — call open() on the
    handle, so the caller controls when it appears.
  */
  function modal(opts) {
    const o = opts || {};
    const dismissable = o.dismissable !== false;

    const overlay = document.createElement("div");
    overlay.className = "modal-overlay";

    const dialog = document.createElement("div");
    dialog.className = "modal";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-modal", "true");
    if (o.size) dialog.setAttribute("data-size", o.size);
    overlay.appendChild(dialog);

    const head = document.createElement("div");
    head.className = "modal-header";
    const title = document.createElement("span");
    title.className = "modal-title";
    title.textContent = String(o.title == null ? "" : o.title);
    /* Name the dialog by its own title rather than a duplicated aria-label. */
    const titleId = "modal-title-" + Math.random().toString(36).slice(2, 9);
    title.id = titleId;
    dialog.setAttribute("aria-labelledby", titleId);
    head.appendChild(title);
    if (dismissable) {
      head.appendChild(iconButton("x", { title: "Close", onClick: () => close() }));
    }
    dialog.appendChild(head);

    const bodyEl = document.createElement("div");
    bodyEl.className = "modal-body";
    if (typeof o.body === "string") bodyEl.appendChild(empty(o.body));
    else if (o.body) bodyEl.appendChild(o.body);
    dialog.appendChild(bodyEl);

    if (o.actions) {
      const footer = actions(o.actions, { stretch: true });
      footer.classList.add("modal-actions");
      dialog.appendChild(footer);
    }

    let closed = false;
    function close() {
      if (closed) return;
      closed = true;
      document.removeEventListener("keydown", onKey, true);
      if (overlay.parentNode) overlay.parentNode.removeChild(overlay);
      if (typeof o.onClose === "function") o.onClose();
    }

    /* Captured on the document so Escape closes the dialog no matter where
       focus sits — a click on the map behind it would otherwise steal the
       key. Registered only while the dialog is mounted. */
    function onKey(e) {
      if (e.key === "Escape" && dismissable) {
        e.preventDefault();
        e.stopPropagation();
        close();
      }
    }

    if (dismissable) {
      overlay.addEventListener("click", (e) => { if (e.target === overlay) close(); });
    }

    function open() {
      if (closed) return null;
      (o.mount || document.body).appendChild(overlay);
      document.addEventListener("keydown", onKey, true);
      refreshIcons();
      /* Ahead of the focus call, and ahead of the observer that would get to
         it a microtask later: a select still wearing the platform popup here
         is the control the next line would hand the keyboard to, moments
         before it is taken out of the layout. */
      enhanceSelects(dialog);
      /* Move focus into the dialog so the keyboard is not left behind on the
         page underneath. An enhanced select is display:none and cannot take
         focus — its trigger, the next node along, is what stands in for it. */
      const first = dialog.querySelector(
        "input:not([disabled]), select:not([disabled]):not(.ui-select-native), " +
        "textarea:not([disabled]), .ui-select-trigger:not([disabled]), .btn:not([disabled])");
      if (first && typeof first.focus === "function") first.focus();
      return overlay;
    }

    return { el: overlay, dialog, body: bodyEl, open, close };
  }

  /* ===== popover — the explainer that only appears when it is asked for =====
     A screen full of rows each carrying two lines of prose is a screen the
     operator has to read before they can scan it: the LINK tab's four preset
     endpoints filled a whole card because every one of them explained itself
     out loud, permanently. The explanation still has to be there — a preset
     nobody can tell apart from the next one is a button that produces a 400 —
     so it moves off the row and behind a hint the pointer or the keyboard
     opens.

     This is the generic half: it attaches to ANY anchor (a hint icon, a field
     caption, a status chip) and owns only the surface and when it is on
     screen. The anchor keeps being whatever it was.

     The surface is .ui-popover.glass — the same translucent material as the
     dropdown list, deliberately, because both are transient sheets that drop
     out of a control. Like the dropdown it is mounted on <body> and placed
     fixed from the anchor's box, since a popover inside a scrolling card
     would be clipped by it, and flips above the anchor when the space below
     runs out.

     There are two ways in and they are not the same promise. A HOVER is a
     glance: the pointer has to rest on the anchor before the sheet appears,
     the sheet is transparent to the pointer while it is up, and it leaves as
     soon as the pointer does. That transparency is the whole fix for a sheet
     that used to outstay its welcome — it hung under the anchor exactly where
     the pointer left, caught its own enter event and cancelled the close it
     had just scheduled, so brushing a hint held a sheet open until the
     operator hunted for somewhere else to point. A PRESS — a click, a tap, a
     keyboard focus — is a request to read: it opens at once, pins the sheet
     so the pointer can enter it and select the text, and closes only on
     Escape, a click outside, or a second press.

     opts: {title, text, body, className, maxWidth, placement, delay, openDelay}
       title      bold first line; omit for a single paragraph
       text       the explanation; \n splits it into paragraphs
       body       node appended after the text, for a caller with real markup
       placement  "bottom" (default) | "top" — a preference, not a promise:
                  both flip when the viewport says so
       openDelay  ms the pointer must rest on the anchor before a hover opens
                  it (default 120), so a pointer crossing a column of hints
                  on its way somewhere else does not set off every one of
                  them. A press ignores it.
       delay      ms a hover-opened sheet lingers after the pointer leaves
                  (default 80) — enough that the edge of the icon does not
                  flicker, short enough that it is gone before it is noticed.

     Returns {el, anchor, show, hide, toggle, isOpen, isPinned, setContent,
     destroy}. show(true) pins; show() does not.
  */
  let openPopover = null;

  function popover(anchor, opts) {
    if (!anchor) return null;
    if (anchor.corvusPopover) return anchor.corvusPopover;
    const o = opts || {};
    const grace = o.delay == null ? 80 : o.delay;
    const openDelay = o.openDelay == null ? 120 : o.openDelay;

    const el = document.createElement("div");
    el.className = "ui-popover glass" + (o.className ? " " + o.className : "");
    el.setAttribute("role", "tooltip");
    el.id = "ui-popover-" + Math.random().toString(36).slice(2, 9);
    if (o.maxWidth) el.style.maxWidth = o.maxWidth;

    let opened = false;
    let pinned = false;
    let timer = null;
    let openTimer = null;
    /* A pointer press focuses the button before the click lands, and that
       focus must not count as the keyboard arriving. */
    let fromPointer = false;

    /* Replaces the whole content, so a caller whose explanation depends on
       live state (a port that is taken, a firmware that is absent) can keep
       one popover rather than build a new one per change. */
    function setContent(next) {
      const c = next || {};
      clear(el);
      if (c.title) {
        const t = document.createElement("div");
        t.className = "ui-popover-title";
        t.textContent = String(c.title);
        el.appendChild(t);
      }
      if (c.text) {
        String(c.text).split("\n").forEach((line) => {
          if (!line) return;
          const p = document.createElement("p");
          p.className = "ui-popover-text";
          p.textContent = line;
          el.appendChild(p);
        });
      }
      if (c.body) el.appendChild(c.body);
      if (opened) place();
    }

    /* Fixed, measured from the anchor: centred under it, flipped above when
       the space below cannot hold it, and clamped to the viewport so a hint
       at the right edge of a card does not push the sheet off screen.

       In UNSCALED pixels throughout, for the reason placeByElement above
       spells out: the anchor's rect and the window's size arrive scaled and
       are divided back, el.offsetWidth/Height are already unscaled. */
    function place() {
      if (!opened || typeof anchor.getBoundingClientRect !== "function") return;
      const k = uiScale();
      const r = unscaledRect(anchor, k);
      const vw = (window.innerWidth || 1200) / k;
      const vh = (window.innerHeight || 800) / k;
      const GAP = 8;
      const EDGE = 8;
      const h = el.offsetHeight || el.scrollHeight || 0;
      const w = el.offsetWidth || 0;
      const below = vh - r.bottom - GAP - EDGE;
      const above = r.top - GAP - EDGE;
      const wantTop = o.placement === "top";
      const up = wantTop ? (above >= h || above >= below) : (below < h && above > below);
      el.dataset.placement = up ? "top" : "bottom";
      el.style.top = Math.round(up
        ? Math.max(EDGE, r.top - GAP - h)
        : Math.max(EDGE, Math.min(r.bottom + GAP, vh - EDGE - h))) + "px";
      const centred = r.left + r.width / 2 - w / 2;
      el.style.left = Math.round(Math.max(EDGE, Math.min(centred, vw - w - EDGE))) + "px";
    }

    /* pin: the opening was deliberate (a click, a tap, the keyboard), so the
       sheet stays until it is dismissed and accepts the pointer. A hover
       opening does neither. */
    function show(pin) {
      cancelOpen();
      cancelHide();
      if (anchor.disabled) return;
      if (opened) { if (pin) setPinned(true); return; }
      /* Nothing to say is not a reason to paint an empty sheet. */
      if (!el.children.length) return;
      /* One at a time, like the dropdown: two popovers open from neighbouring
         rows would overlap each other's text and both listen on the
         document. */
      if (openPopover && openPopover !== handle) openPopover.hide();
      if (openDropdown) openDropdown.close();
      document.body.appendChild(el);
      opened = true;
      openPopover = handle;
      /* Described-by only while it is on screen: a reference to a node that
         is not in the document is one a screen reader cannot follow. */
      anchor.setAttribute("aria-describedby", el.id);
      if (anchor.dataset) anchor.dataset.popoverOpen = "true";
      setPinned(!!pin);
      place();
      document.addEventListener("keydown", onKey, true);
      document.addEventListener("scroll", onScroll, true);
      document.addEventListener("pointerdown", onOutside, true);
      window.addEventListener("resize", place);
      refreshIcons();
    }

    /* The flag is on the element, not only in this closure, because the
       stylesheet is what actually takes the pointer away from an unpinned
       sheet; the inline style is the belt to that braces, for a caller who
       loads the component without the stylesheet. */
    function setPinned(next) {
      pinned = !!next;
      el.dataset.pinned = pinned ? "true" : "false";
      el.style.pointerEvents = pinned ? "" : "none";
    }

    function hide() {
      cancelOpen();
      cancelHide();
      if (!opened) return;
      opened = false;
      setPinned(false);
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("scroll", onScroll, true);
      document.removeEventListener("pointerdown", onOutside, true);
      window.removeEventListener("resize", place);
      if (openPopover === handle) openPopover = null;
      if (el.parentNode) el.parentNode.removeChild(el);
      if (typeof anchor.removeAttribute === "function") anchor.removeAttribute("aria-describedby");
      if (anchor.dataset) delete anchor.dataset.popoverOpen;
    }

    /* A hover follows the pointer out. The short grace is only so the seam
       between the icon and its own hover background cannot flicker the sheet;
       a pinned sheet ignores the pointer leaving entirely, because something
       asked for it to stay. */
    function hideSoon() {
      cancelHide();
      if (!opened || pinned) return;
      if (!grace) { hide(); return; }
      timer = setTimeout(() => { timer = null; hide(); }, grace);
    }
    function cancelHide() {
      if (timer == null) return;
      clearTimeout(timer);
      timer = null;
    }

    /* Hover intent: the pointer has to mean it. Without this, a pointer
       travelling down a column of hinted rows opens every sheet on the way
       past. */
    function showSoon() {
      cancelHide();
      if (opened || openTimer != null) return;
      if (!openDelay) { show(false); return; }
      openTimer = setTimeout(() => { openTimer = null; show(false); }, openDelay);
    }
    function cancelOpen() {
      if (openTimer == null) return;
      clearTimeout(openTimer);
      openTimer = null;
    }

    function onScroll(e) {
      if (e && within(e.target, el)) return;
      hide();
    }
    function onOutside(e) {
      const t = e && e.target;
      if (within(t, el) || within(t, anchor)) return;
      hide();
    }
    /* Captured, like the dropdown's: Escape belongs to the topmost transient
       surface, and the map and the modals listen for it too. */
    function onKey(e) {
      if (!e || e.key !== "Escape") return;
      if (typeof e.preventDefault === "function") e.preventDefault();
      if (typeof e.stopPropagation === "function") e.stopPropagation();
      hide();
      if (typeof anchor.focus === "function") anchor.focus();
    }

    function within(node, root) {
      let n = node;
      while (n) {
        if (n === root) return true;
        n = n.parentNode;
      }
      return false;
    }

    anchor.addEventListener("pointerenter", showSoon);
    anchor.addEventListener("pointerleave", () => { cancelOpen(); hideSoon(); });
    anchor.addEventListener("pointerdown", () => { fromPointer = true; });
    /* Focus opens it too, or the explanation exists only for people using a
       pointer — but only the keyboard's focus does, since the pointer's own
       focus is the front half of a click the next listener answers. */
    anchor.addEventListener("focus", () => { if (!fromPointer) show(true); });
    anchor.addEventListener("blur", () => { fromPointer = false; hide(); });
    /* A touch screen has no hover at all, so the tap has to do it. A press on
       a sheet the pointer had already opened pins it rather than closing it:
       the operator reaching for a hint that is already showing wants to keep
       it, not dismiss it. */
    anchor.addEventListener("click", (e) => {
      if (typeof e.preventDefault === "function") e.preventDefault();
      fromPointer = false;
      if (opened && pinned) hide(); else show(true);
    });
    /* Only a pinned sheet should receive these at all — the unpinned one is
       transparent to the pointer — but a pinned one still needs the gap
       between anchor and sheet to be crossable. The guard is what stops the
       sheet reprieving itself: without it, a sheet lying under the pointer's
       exit path cancels its own close, and that is exactly the lingering the
       transparency was added to end. */
    el.addEventListener("pointerenter", () => { if (pinned) cancelHide(); });
    el.addEventListener("pointerleave", hideSoon);

    setContent({ title: o.title, text: o.text, body: o.body });

    const handle = {
      el, anchor,
      show, hide, setContent,
      toggle: () => { if (opened) hide(); else show(true); },
      isOpen: () => opened,
      isPinned: () => pinned,
      destroy: () => { hide(); anchor.corvusPopover = null; },
    };
    anchor.corvusPopover = handle;
    return handle;
  }

  /*
    infoHint — the circled "i" and its popover, as one call.

    The shape every caller wanted from popover(): a small round icon button
    that sits next to a label and holds the sentence that would otherwise be
    printed under it. Returns the BUTTON (append it wherever the hint
    belongs); the popover handle hangs off it as .corvusPopover, for the rare
    caller that has to drive it.

    opts: title, text, body, placement, delay (see popover), plus
      ariaLabel  what a screen reader announces for the button itself
                 (default: "More information about <title>")
      size       icon size in px, default 13 — a hint must not out-shout the
                 label it annotates
      className  extra classes on the button
  */
  function infoHint(opts) {
    const o = opts || {};
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "ui-info" + (o.className ? " " + o.className : "");
    /* Not in the tab order by default: on a list of hinted rows every hint
       would otherwise sit between two rows and double the tab stops. The
       content is reachable from the row itself, whose title carries it. */
    btn.tabIndex = o.focusable === false ? -1 : 0;
    const name = o.ariaLabel || (o.title ? "More information about " + o.title : "More information");
    btn.setAttribute("aria-label", name);
    /* No native title: it would put the platform's own yellow tooltip on top
       of the popover that replaced it. */
    btn.appendChild(icon(o.icon || "info", o.size == null ? 13 : o.size));
    popover(btn, o);
    return btn;
  }

  /* ===================== feedback ===================== */

  /*
    Determinate progress bar with a "done / total (pct)" caption. Returns
    {el, set, reset}; `set(done, total)` clamps the percentage so a backend
    that over-reports cannot push the fill past 100%.
  */
  function progress(opts) {
    const o = opts || {};
    const el = document.createElement("div");
    el.className = "ui-progress";
    if (o.hidden) el.hidden = true;
    const bar = document.createElement("div");
    bar.className = "progress-bar";
    const fill = document.createElement("div");
    fill.className = "progress-bar-fill";
    bar.appendChild(fill);
    el.appendChild(bar);
    const text = document.createElement("div");
    text.className = "ui-progress-text";
    el.appendChild(text);

    function set(done, total) {
      const d = Number(done) || 0;
      const t = Number(total) || 0;
      const pct = t > 0 ? Math.max(0, Math.min(100, Math.round((d / t) * 100))) : 0;
      fill.style.width = pct + "%";
      text.textContent = `${d} / ${t} (${pct}%)`;
      return pct;
    }
    function reset() { set(0, 0); }
    reset();

    return { el, fill, text, set, reset };
  }

  /*
    Toast — a transient, self-dismissing notice built to the same anatomy as a
    row on the notification board (glass surface, leading column with the
    level icon and its severity mark, then the text, then a dismiss), for
    messages that are not vehicle/flight state and so have no business sitting
    in that board (a UI hint like "double-click to zoom back out"). Any part of
    the app can call this; it mounts its own stack on <body> the first time it
    is used.

    opts: {level: "info" | "warning" | "critical", title, message, duration}
      level     picks the icon, accent colour and default title/duration.
      title     overrides the default ("Info" / "Warning" / "Error").
      duration  ms before it auto-dismisses; 0 pins it open (the default for
                "critical" — an error is the operator's to dismiss). Omit to
                use the level's default.

    Returns {el, close}.
  */
  let toastStack = null;
  function toastHost() {
    if (toastStack && toastStack.isConnected) return toastStack;
    toastStack = document.createElement("div");
    toastStack.className = "ui-toast-stack";
    document.body.appendChild(toastStack);
    return toastStack;
  }

  /* The notification popover and the toast stack share a corner by default.
     If the popover happens to be open when a toast fires, drop the stack
     below it instead of drawing over it — an operator who opened the board
     to read a warning should not have a hint toast blot it out. */
  function repositionToastStack(stack) {
    const popover = document.getElementById("warningsPopover");
    if (!popover || popover.hidden || typeof popover.getBoundingClientRect !== "function") {
      stack.style.top = "";
      return;
    }
    /* The board's rect comes back scaled and this `top` is written unscaled,
       so at 150% an undivided bottom edge dropped the stack half a screen
       below the board it was meant to clear. */
    const rect = unscaledRect(popover, uiScale());
    stack.style.top = rect.height > 0 ? Math.round(rect.bottom + 10) + "px" : "";
  }
  /* The level's icon, default title and default lifetime. The icons are the
     same three the notification board picks from (src/js/topbar.js) — a
     warning may not be a triangle in one place and something else in the
     other. */
  const TOAST_ICON = { info: "info", warning: "triangle-alert", critical: "octagon-alert" };
  const TOAST_TITLE = { info: "Info", warning: "Warning", critical: "Error" };
  const TOAST_DURATION = { info: 4500, warning: 6000, critical: 0 };

  function toast(opts) {
    const o = opts || {};
    const level = (o.level === "warning" || o.level === "critical") ? o.level : "info";

    const el = document.createElement("div");
    el.className = "ui-toast glass corvus-enter";
    el.dataset.level = level;
    el.setAttribute("role", level === "critical" ? "alert" : "status");

    /* Built as a board row is built — icon, text, dismiss as three children of
       the card — rather than as a header strip with the message hanging under
       it. That is what lets the severity mark be the same rule in both places:
       it is drawn on the card and threaded through an icon centred on the
       card, which only works if the icon is the card's own first column. */
    const iconWrap = document.createElement("div");
    iconWrap.className = "ui-toast-icon " + level;
    iconWrap.setAttribute("aria-hidden", "true");
    iconWrap.appendChild(icon(TOAST_ICON[level], 16));

    const text = document.createElement("div");
    text.className = "ui-toast-text";
    const title = document.createElement("span");
    title.className = "ui-toast-title";
    title.textContent = o.title || TOAST_TITLE[level];
    const body = document.createElement("div");
    body.className = "ui-toast-body";
    body.textContent = o.message == null ? "" : String(o.message);
    text.append(title, body);

    const closeBtn = document.createElement("button");
    closeBtn.type = "button";
    closeBtn.className = "ui-toast-close";
    closeBtn.setAttribute("aria-label", "Dismiss");
    closeBtn.appendChild(icon("x", 13));

    el.append(iconWrap, text, closeBtn);

    const host = toastHost();
    repositionToastStack(host);
    host.appendChild(el);
    refreshIcons();

    let closed = false;
    let timer = null;
    function close() {
      if (closed) return;
      closed = true;
      if (timer != null) window.clearTimeout(timer);
      el.classList.add("is-leaving");
      window.setTimeout(() => { if (el.parentNode) el.parentNode.removeChild(el); }, 160);
    }
    closeBtn.addEventListener("click", close);

    const duration = o.duration != null ? o.duration : TOAST_DURATION[level];
    if (duration > 0) timer = window.setTimeout(close, duration);

    return { el, close };
  }

  /*
    Inline status message with ok / warn / err severities. Returns
    {el, show, hide}; hidden until show() is called so a caller can append it
    unconditionally at build time.
  */
  function message(opts) {
    const o = opts || {};
    const el = document.createElement("div");
    el.className = "ui-msg" + (o.className ? " " + o.className : "");
    el.hidden = true;
    const base = el.className;

    function show(text, kind) {
      el.hidden = false;
      el.textContent = String(text == null ? "" : text);
      el.className = base + (kind ? " " + kind : "");
    }
    function hide() {
      el.hidden = true;
      el.textContent = "";
      el.className = base;
    }
    return { el, show, hide };
  }

  /* ===================== charts ===================== */

  /**
   * Read a design token off the document root.
   *
   * Plotly draws into a canvas/SVG it owns and takes colors as literal
   * strings, so it is the one place in the app that cannot simply reference
   * var(--token) — the values have to be resolved and handed over. That makes
   * this the bridge between themes.css and every chart.
   */
  function token(name, fallback) {
    try {
      const v = getComputedStyle(document.documentElement).getPropertyValue(name);
      return (v && v.trim()) || fallback;
    } catch (_e) {
      return fallback;
    }
  }

  /**
   * The themed half of a Plotly layout: surfaces, type, grid and axis colors,
   * all resolved from the active theme. Callers merge their own margins,
   * titles and axis ranges on top.
   *
   * Fallbacks are the dark-theme values, so a chart drawn before the
   * stylesheet has applied still looks deliberate rather than Plotly-default
   * white-on-white.
   */
  function plotlyTheme() {
    const surface = token("--surface-1", "#11161D");
    const text = token("--text-2", "#A5ADB8");
    const muted = token("--text-3", "#69737F");
    const grid = token("--border-soft", "#20262E");
    const axis = token("--border", "#2A3038");
    return {
      paper_bgcolor: surface,
      plot_bgcolor: surface,
      font: { color: text, family: "JetBrains Mono, monospace", size: 10 },
      xaxis: {
        gridcolor: grid, zerolinecolor: axis, linecolor: axis,
        tickfont: { size: 9, color: muted },
      },
      yaxis: {
        gridcolor: grid, zerolinecolor: axis, linecolor: axis,
        tickfont: { size: 9, color: muted },
      },
    };
  }

  /** Series colors for charts, from the semantic palette. Each theme retunes
   *  these, so a chart drawn in the light theme uses its darker, saturated
   *  variants rather than the dark theme's glowing ones. */
  function chartColors() {
    return {
      nav: token("--nav", "#4CC9FF"),
      healthy: token("--healthy", "#45D483"),
      warning: token("--warning", "#F5C842"),
      critical: token("--critical", "#FF514D"),
      accent: token("--accent", "#3DA876"),
    };
  }

  /**
   * Subscribe to theme changes. Charts hold resolved color strings, so unlike
   * everything else in the app they do not restyle themselves when the theme
   * attribute flips — they have to be told to redraw. Returns an unsubscribe.
   */
  function onThemeChange(fn) {
    if (typeof fn !== "function") return function () {};
    // Guarded like every other helper here: a plugin (the documented extension
    // point) may run somewhere without a full window, and a missing theme
    // listener must degrade to "charts keep their current colors", never throw.
    if (typeof window === "undefined" || typeof window.addEventListener !== "function") {
      return function () {};
    }
    const handler = () => fn();
    window.addEventListener("corvus:themechange", handler);
    return function () {
      if (typeof window.removeEventListener === "function") {
        window.removeEventListener("corvus:themechange", handler);
      }
    };
  }

  /* Plotly's own built-in zoom hint (its "notifier" toast) is unstyled and
     positions itself off the chart, over whatever else happens to be on the
     page — plotlyConfig() turns it off (showTips: false) everywhere. This is
     its replacement: the app's own toast, shown once per session the first
     time an operator box-zooms any chart, on whichever host asks for it.
     Attaching twice on the same host (a live chart redraws on every sample)
     is a no-op, tracked by identity rather than a flag on the element so a
     plugin's plain object host cannot collide with it. */
  const zoomHintHosts = (typeof WeakSet === "function") ? new WeakSet() : null;
  let zoomHintShown = false;
  function attachZoomHint(host) {
    if (!host || typeof host.on !== "function") return;
    if (zoomHintHosts) {
      if (zoomHintHosts.has(host)) return;
      zoomHintHosts.add(host);
    }
    host.on("plotly_relayout", (ev) => {
      if (zoomHintShown || !ev) return;
      const zoomedIn = Object.keys(ev).some((k) => /^[xy]axis\d*\.range\[/.test(k));
      if (!zoomedIn) return;
      zoomHintShown = true;
      toast({ level: "info", message: "Double-click to zoom back out" });
    });
  }

  /* ===================== helpers ===================== */

  /*
    uiScale() — scaled pixels per unscaled pixel.

    The interface-size control (Settings -> Appearance) puts a CSS `zoom` on
    <body>, and that splits the DOM's geometry into two coordinate spaces that
    look alike and are not:

      SCALED    getBoundingClientRect(), pointer clientX/clientY,
                window.innerWidth / innerHeight — the real window's pixels.
      UNSCALED  style.left / top / width / maxHeight, offsetWidth / Height,
                clientWidth / Height, scrollHeight — the pixels everything
                below <body> is laid out in, which the zoom then multiplies.

    A measurement taken in one and written into the other is multiplied (or
    divided) by the scale a second time, which is what put the map's layer
    menu a third of a screen away from the button that opened it at 150%.
    Anything that mixes them divides by this; anything staying inside one
    space needs it not at all.

    Measured, never read from the --ui-scale token: the ratio of an element's
    border box in the two spaces IS the cumulative zoom above it, so this
    stays right if the token is renamed, unset, or the browser ignores `zoom`
    altogether. <body> is the probe because it is where the zoom is declared
    and it always has a box; its size also makes the rounding in offsetWidth
    (an integer) negligible. Returns 1 when there is nothing to measure, which
    is the answer at 100% anyway.
  */
  function uiScale() {
    if (typeof document === "undefined" || !document.body) return 1;
    const body = document.body;
    if (typeof body.getBoundingClientRect !== "function") return 1;
    const unscaled = body.offsetWidth;
    if (!(unscaled > 0)) return 1;
    const rect = body.getBoundingClientRect();
    const k = rect && rect.width ? rect.width / unscaled : 1;
    return (isFinite(k) && k > 0) ? k : 1;
  }

  /* A client rect divided back into the unscaled space `style.left` writes
     in. The six numbers, not the DOMRect's own toJSON, because a caller reads
     r.bottom and r.right as often as r.top and r.left. */
  function unscaledRect(el, k) {
    const r = el.getBoundingClientRect();
    return {
      top: r.top / k, left: r.left / k,
      bottom: r.bottom / k, right: r.right / k,
      width: r.width / k, height: r.height / k,
    };
  }

  /* Empty a container. Faster than innerHTML="" for large lists and, unlike
     it, cannot be handed markup by accident. */
  function clear(el) {
    if (!el) return el;
    while (el.firstChild) el.removeChild(el.firstChild);
    return el;
  }

  /* Swap every <i data-lucide> placeholder in the document for its SVG. Must
     run after appending anything built with icon(). `opts` is forwarded to
     Lucide (e.g. {attrs: {"aria-hidden": "true"}} for decorative icons).
     Safe to call when Lucide has not loaded (offline first paint) — it simply
     does nothing. */
  function refreshIcons(opts) {
    if (typeof window !== "undefined" && window.lucide && window.lucide.createIcons) {
      window.lucide.createIcons(opts);
    }
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

  /* Keep a .flight-actions bar on ONE ROW inside the room it actually has.

     Both bars in the application are this bar — the Home tab's flight actions
     and the planner's tools — and both sit on a map column whose width is not
     the window's: opening the right-hand panel narrows it without the window
     moving a pixel, which is why the test is a measurement and never a
     viewport media query.

     Two steps down, in .flight-actions.is-tight / .is-compact: the shared
     button width goes first, the captions only if that is still not enough.
     Both are cleared before measuring, because a bar that has already been
     narrowed measures narrow and would never widen again once it had shrunk
     once.

     `room` is the width to fit into; without one the bar's own client width is
     used, which is what a bar capped by a max-width is already limited to. */
  function fitBar(bar, room) {
    if (!bar || !bar.classList) return;
    bar.classList.remove("is-tight", "is-compact");
    const space = room == null ? bar.clientWidth : room;
    if (!(space > 0) || bar.scrollWidth <= space) return;
    bar.classList.add("is-tight");
    if (bar.scrollWidth > space) bar.classList.add("is-compact");
  }

  return {
    // primitives
    icon,
    iconButton,
    button,
    statusDot,
    // forms
    label,
    field,
    select,
    setOptions,
    enhanceSelect,
    enhanceSelects,
    watchSelects,
    menu,
    menuItem,
    input,
    slider,
    toggle,
    // layout
    card,
    section,
    sectionTitle,
    pageHeader,
    row,
    empty,
    actions,
    // pickers
    optionCards,
    optionList,
    navItem,
    tile,
    // overlays
    modal,
    popover,
    infoHint,
    // feedback
    progress,
    message,
    toast,
    setBusy,
    setActive,
    fitBar,
    // charts
    token,
    plotlyTheme,
    chartColors,
    onThemeChange,
    attachZoomHint,
    // helpers
    clear,
    refreshIcons,
    uiScale,
  };
})();
