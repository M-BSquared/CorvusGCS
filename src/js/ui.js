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
    layout      card, section, sectionTitle, pageHeader, row, empty, actions
    pickers     optionCards, optionList, navItem
    feedback    progress, message, toast, setBusy, setActive
    charts      token, plotlyTheme, chartColors, onThemeChange, attachZoomHint
    helpers     clear, refreshIcons

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
     labelling. */
  function field(opts) {
    const o = opts || {};
    const el = document.createElement("div");
    el.className = "field" + (o.inline ? " field-inline" : "") +
                   (o.className ? " " + o.className : "");
    if (o.label) el.appendChild(label(o.label, { htmlFor: o.control && o.control.id }));
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


  /* The one piece of module state in this file, and it earns it: a dropdown's
     list is mounted on <body> and listens on the document, so knowing which
     one is open is not something any single instance can answer. */
  let openDropdown = null;

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

     The list is mounted on <body> and positioned fixed. Inside the flight bar
     it would be clipped by the bar and stacked under the map chrome; on
     <body> it can also flip above the trigger when the viewport runs out
     below, which a 15-mode PX4 list needs on a laptop screen. */
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

    /* The list IS .option-list / .option-item — the same rows as the map's
       layer switcher — so the dropdown reads as a member of the family
       instead of a second, nearly-identical menu style. */
    const menu = document.createElement("div");
    menu.className = "ui-select-menu option-list glass";
    menu.setAttribute("role", "listbox");
    if (name) menu.setAttribute("aria-label", name);

    let items = [];
    let opened = false;
    let cursor = -1;
    let typed = "";
    let typedAt = 0;

    function options() {
      const list = sel.options || sel.querySelectorAll("option");
      return Array.prototype.slice.call(list || []);
    }
    function textOf(opt) {
      const t = opt.textContent;
      return String(t == null || t === "" ? opt.value : t);
    }
    /* Walk parents rather than call contains(): the same check works for a
       node that is no longer in the document. */
    function within(node, root) {
      let n = node;
      while (n) {
        if (n === root) return true;
        n = n.parentNode;
      }
      return false;
    }

    function syncTrigger() {
      const chosen = options().filter((o) => String(o.value) === String(sel.value))[0];
      labelEl.textContent = chosen ? textOf(chosen) : "";
      /* An empty value is the placeholder row ("SELECT MODE"), which is a
         prompt and not a choice, so it is dimmed like one. */
      trigger.dataset.placeholder = !chosen || String(chosen.value) === "" ? "true" : "false";
      trigger.disabled = !!sel.disabled;
      if (sel.disabled) closeMenu(false);
    }

    function buildItems() {
      clear(menu);
      cursor = -1;
      items = options().map((o, i) => {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "option-item";
        b.setAttribute("role", "option");
        b.dataset.value = String(o.value);
        const on = String(o.value) === String(sel.value);
        if (on) {
          b.classList.add("active");
          cursor = i;
        }
        b.setAttribute("aria-selected", on ? "true" : "false");
        if (o.disabled) b.disabled = true;
        const dot = document.createElement("span");
        dot.className = "option-item-dot";
        b.appendChild(dot);
        const text = document.createElement("span");
        text.className = "option-item-label";
        text.textContent = textOf(o);
        b.appendChild(text);
        b.addEventListener("click", () => choose(o.value));
        menu.appendChild(b);
        return b;
      });
      return items;
    }

    function choose(value) {
      const changed = String(sel.value) !== String(value);
      closeMenu(true);
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

    /* Fixed position, measured from the trigger: below it by default, flipped
       above when the list would not fit, and capped to the space it has so a
       long list scrolls instead of running off the screen. */
    function place() {
      if (!opened || typeof trigger.getBoundingClientRect !== "function") return;
      const r = trigger.getBoundingClientRect();
      const vh = window.innerHeight || 800;
      const vw = window.innerWidth || 1200;
      const GAP = 6;
      const EDGE = 8;
      const below = vh - r.bottom - GAP - EDGE;
      const above = r.top - GAP - EDGE;
      const wanted = menu.scrollHeight || menu.offsetHeight || 0;
      const up = below < Math.min(wanted, 180) && above > below;
      menu.style.minWidth = Math.round(r.width) + "px";
      menu.style.maxHeight = Math.max(120, Math.round(up ? above : below)) + "px";
      const h = menu.offsetHeight || wanted;
      const w = menu.offsetWidth || r.width;
      menu.style.top = Math.round(up ? Math.max(EDGE, r.top - GAP - h) : r.bottom + GAP) + "px";
      menu.style.left = Math.round(Math.max(EDGE, Math.min(r.left, vw - w - EDGE))) + "px";
    }

    function openMenu() {
      if (opened || sel.disabled) return;
      /* One list at a time, like a native select: a second dropdown opened
         from the keyboard would otherwise sit under the first and swallow its
         keys, since both listen on the document. */
      if (openDropdown && openDropdown !== handle) openDropdown.close();
      buildItems();
      if (!items.length) return;
      document.body.appendChild(menu);
      opened = true;
      openDropdown = handle;
      trigger.setAttribute("aria-expanded", "true");
      place();
      document.addEventListener("pointerdown", onOutside, true);
      document.addEventListener("keydown", onKey, true);
      document.addEventListener("scroll", onScroll, true);
      window.addEventListener("resize", place);
      focusItem(cursor >= 0 ? cursor : nextEnabled(-1, 1));
    }

    function closeMenu(refocus) {
      if (!opened) return;
      opened = false;
      typed = "";
      trigger.setAttribute("aria-expanded", "false");
      document.removeEventListener("pointerdown", onOutside, true);
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", place);
      if (openDropdown === handle) openDropdown = null;
      if (menu.parentNode) menu.parentNode.removeChild(menu);
      if (refocus && typeof trigger.focus === "function") trigger.focus();
    }

    function nextEnabled(from, step) {
      if (!items.length) return -1;
      for (let n = 1; n <= items.length; n++) {
        const i = (from + step * n + items.length * items.length) % items.length;
        if (!items[i].disabled) return i;
      }
      return -1;
    }

    function focusItem(i) {
      if (i < 0 || i >= items.length) return;
      cursor = i;
      const b = items[i];
      if (typeof b.focus === "function") b.focus();
      if (typeof b.scrollIntoView === "function") b.scrollIntoView({ block: "nearest" });
    }

    /* The list is positioned once, from the trigger's box. Scrolling the pane
       the control sits in would leave it hanging in the wrong place, so it
       closes instead — scrolling the list itself excepted. */
    function onScroll(e) {
      if (e && within(e.target, menu)) return;
      closeMenu(false);
    }

    function onOutside(e) {
      const t = e && e.target;
      if (within(t, menu) || within(t, wrap)) return;
      closeMenu(false);
    }

    /* Captured on the document: the map and the modals listen for Escape and
       arrow keys too, and a key aimed at an open dropdown must not reach
       them. Registered only while the list is open. */
    function onKey(e) {
      const key = e.key;
      if (key === "Escape") {
        stop(e);
        closeMenu(true);
        return;
      }
      if (key === "Tab") {
        closeMenu(false);
        return;
      }
      if (key === "ArrowDown") { stop(e); focusItem(nextEnabled(cursor, 1)); return; }
      if (key === "ArrowUp") { stop(e); focusItem(nextEnabled(cursor, -1)); return; }
      if (key === "Home") { stop(e); focusItem(nextEnabled(-1, 1)); return; }
      if (key === "End") { stop(e); focusItem(nextEnabled(items.length, -1)); return; }
      /* Type-ahead: PX4's mode list is long and its names are distinct, so a
         letter should jump the way it does in a native select. */
      if (key && key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
        const now = Date.now();
        typed = (now - typedAt < 800 ? typed : "") + key.toLowerCase();
        typedAt = now;
        const hit = items.findIndex((b, i) => !b.disabled && i !== cursor &&
          (b.textContent || "").toLowerCase().indexOf(typed) === 0);
        const wrapHit = hit >= 0 ? hit : items.findIndex((b) => !b.disabled &&
          (b.textContent || "").toLowerCase().indexOf(typed) === 0);
        if (wrapHit >= 0) { stop(e); focusItem(wrapHit); }
      }
    }

    function stop(e) {
      if (typeof e.preventDefault === "function") e.preventDefault();
      if (typeof e.stopPropagation === "function") e.stopPropagation();
    }

    trigger.addEventListener("click", () => {
      if (opened) closeMenu(true); else openMenu();
    });
    trigger.addEventListener("keydown", (e) => {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        stop(e);
        openMenu();
      }
    });
    /* Anything that changes the select — a repopulated mode list, a restored
       selection, the disabled flag app.js sets while PX4 acknowledges — has
       to reach the trigger, and none of it is an event we could listen for. */
    sel.addEventListener("change", syncTrigger);
    if (typeof MutationObserver === "function") {
      const mo = new MutationObserver(() => {
        syncTrigger();
        if (!opened) return;
        /* Rebuilding drops the row the keyboard was on, so put it back on the
           current selection rather than letting focus fall to the body. */
        buildItems();
        place();
        focusItem(cursor >= 0 ? cursor : nextEnabled(-1, 1));
      });
      mo.observe(sel, { childList: true, subtree: true, attributes: true, attributeFilter: ["disabled"] });
    }

    syncTrigger();

    const handle = {
      el: wrap, trigger, menu, select: sel,
      open: openMenu,
      close: () => closeMenu(false),
      refresh: syncTrigger,
      isOpen: () => opened,
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
    const root = open.trigger;
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
    Toast — a transient, self-dismissing notice in the app's own design
    language (glass surface, level icon and colour, same palette as the
    notification centre), for messages that are not vehicle/flight state and
    so have no business sitting in that board (a UI hint like "double-click
    to zoom back out"). Any part of the app can call this; it mounts its own
    stack on <body> the first time it is used.

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
    const rect = popover.getBoundingClientRect();
    stack.style.top = rect.height > 0 ? Math.round(rect.bottom + 10) + "px" : "";
  }
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

    const head = document.createElement("div");
    head.className = "ui-toast-head";
    const iconWrap = document.createElement("div");
    iconWrap.className = "ui-toast-icon " + level;
    iconWrap.appendChild(icon(TOAST_ICON[level], 15));
    const title = document.createElement("span");
    title.className = "ui-toast-title";
    title.textContent = o.title || TOAST_TITLE[level];
    const closeBtn = document.createElement("button");
    closeBtn.type = "button";
    closeBtn.className = "ui-toast-close";
    closeBtn.setAttribute("aria-label", "Dismiss");
    closeBtn.appendChild(icon("x", 12));
    head.append(iconWrap, title, closeBtn);

    const body = document.createElement("div");
    body.className = "ui-toast-body";
    body.textContent = o.message == null ? "" : String(o.message);
    el.append(head, body);

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
    // feedback
    progress,
    message,
    toast,
    setBusy,
    setActive,
    // charts
    token,
    plotlyTheme,
    chartColors,
    onThemeChange,
    attachZoomHint,
    // helpers
    clear,
    refreshIcons,
  };
})();
