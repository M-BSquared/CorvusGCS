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
    forms       label, field, select, input, checkbox
    layout      card, section, sectionTitle, pageHeader, row, empty, actions
    pickers     optionCards, optionList, navItem
    feedback    progress, message, setBusy, setActive
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
    /* Extra classes layer a screen-specific modifier (.calib-btn, .setup-back)
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
     flex child that shares a row with its siblings (the zoom min/max pair).
     The caption becomes a real <label for> when the control carries an id, so
     clicking it focuses the control; without an id it degrades to a plain
     <span> and the control's own aria-label does the labelling. */
  function field(opts) {
    const o = opts || {};
    const el = document.createElement("div");
    el.className = "field" + (o.inline ? " field-inline" : "");
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
      /* Move focus into the dialog so the keyboard is not left behind on the
         page underneath. */
      const first = dialog.querySelector(
        "input:not([disabled]), select:not([disabled]), textarea:not([disabled]), .btn:not([disabled])");
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
    input,
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
    setBusy,
    setActive,
    // charts
    token,
    plotlyTheme,
    chartColors,
    onThemeChange,
    // helpers
    clear,
    refreshIcons,
  };
})();
