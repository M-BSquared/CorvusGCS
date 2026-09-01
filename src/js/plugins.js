"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.plugins — minimal plugin registry powering the FUTURE tab.
 *
 * The FUTURE tab is the documented extension point of Corvus GCS. Plugins
 * register here at module load (IIFE side-effect) and the grid renders them as
 * cards; clicking a card swaps the grid for the plugin's container and hands
 * it a lean `api` (telemetry subscribe/getState + the one-shot requestJson /
 * postAction config actions + a reduced-motion probe). Only one plugin is
 * open at a time; opening another first closes the current so destroy() runs
 * exactly once (no listener leaks — apple-design "interruptible by default").
 */
Corvus.plugins = (function () {
  // Insertion-ordered map: id -> spec. A Map preserves registration order so
  // the grid is deterministic regardless of object-key ordering quirks.
  const registry = new Map();

  let rootEl = null;     // the FUTURE content element the grid renders into
  let api = null;        // the shared api object handed to every plugin
  let activeId = null;   // currently-open plugin id (null = grid showing)
  let activeContainer = null;  // the container handed to the active plugin

  function refreshIcons() {
    if (window.lucide && lucide.createIcons) lucide.createIcons();
  }

  /**
   * Register a plugin. Returns true on success, false on rejection.
   * Duplicate or empty ids are rejected (returns false) rather than throwing,
   * so a misbehaving plugin module never breaks app init.
   *
   * @param {string} id   Unique non-empty plugin id.
   * @param {Object} spec {name, icon, description, init, destroy}
   * @returns {boolean}
   */
  function register(id, spec) {
    if (typeof id !== "string" || !id) return false;
    if (!spec || typeof spec !== "object") return false;
    if (registry.has(id)) return false;          // duplicate id — reject silently
    if (typeof spec.init !== "function" || typeof spec.destroy !== "function") return false;
    if (typeof spec.name !== "string" || !spec.name) return false;
    registry.set(id, {
      id,
      name: spec.name,
      icon: spec.icon || "puzzle",
      description: spec.description || "",
      init: spec.init,
      destroy: spec.destroy,
    });
    // If the grid is already on screen, re-render so the new card appears.
    if (rootEl && activeId === null) renderGrid();
    return true;
  }

  /**
   * Remove a plugin. If it is currently active, close it first so destroy()
   * runs and the grid is restored.
   * @param {string} id
   * @returns {boolean}
   */
  function unregister(id) {
    if (!registry.has(id)) return false;
    if (activeId === id) close();
    registry.delete(id);
    if (rootEl && activeId === null) renderGrid();
    return true;
  }

  /** List registered plugins in registration order (id/name/icon/description). */
  function list() {
    const out = [];
    registry.forEach((spec) => {
      out.push({ id: spec.id, name: spec.name, icon: spec.icon, description: spec.description });
    });
    return out;
  }

  /** Currently-active plugin id, or null when the grid is showing. */
  function getActive() { return activeId; }

  /** Static placeholder shown when no plugins are registered yet. */
  function renderEmpty() {
    rootEl.innerHTML =
      '<div class="future-icon" data-lucide="puzzle"></div>' +
      '<div class="future-title">Future Tools / Plugins</div>' +
      '<div class="future-desc">This panel is the extension point of Corvus GCS — additional modules will plug in here.</div>';
    rootEl.classList.add("future-empty");
    refreshIcons();
  }

  /** Render the responsive card grid from the registry. */
  function renderGrid() {
    if (!rootEl) return;
    const plugins = list();
    if (!plugins.length) { renderEmpty(); return; }

    rootEl.classList.remove("future-empty");
    rootEl.innerHTML = "";
    const grid = document.createElement("div");
    grid.className = "plugin-grid";
    plugins.forEach((p) => {
      const card = document.createElement("button");
      card.type = "button";
      card.className = "plugin-card";
      card.dataset.pluginId = p.id;
      card.setAttribute("aria-label", p.name);

      // Build the card with createElement (not innerHTML) so the structure is
      // introspectable and a plugin's name/desc are real text nodes.
      const iconWrap = document.createElement("span");
      iconWrap.className = "plugin-card-icon";
      const iconEl = document.createElement("i");
      iconEl.setAttribute("data-lucide", p.icon);
      iconWrap.appendChild(iconEl);

      const nameEl = document.createElement("span");
      nameEl.className = "plugin-card-name";
      nameEl.textContent = p.name;

      const descEl = document.createElement("span");
      descEl.className = "plugin-card-desc";
      descEl.textContent = p.description;

      card.appendChild(iconWrap);
      card.appendChild(nameEl);
      card.appendChild(descEl);
      card.addEventListener("click", () => open(p.id));
      grid.appendChild(card);
    });
    rootEl.appendChild(grid);
    refreshIcons();
  }

  /** Render the opened-plugin view: back button + header + fresh container. */
  function renderPluginView(spec) {
    rootEl.classList.remove("future-empty");
    rootEl.innerHTML = "";

    const view = document.createElement("div");
    view.className = "plugin-view";

    const back = document.createElement("button");
    back.type = "button";
    back.className = "plugin-back";
    const backIcon = document.createElement("i");
    backIcon.setAttribute("data-lucide", "chevron-left");
    const backLabel = document.createElement("span");
    backLabel.textContent = "Plugins";
    back.appendChild(backIcon);
    back.appendChild(backLabel);
    back.setAttribute("aria-label", "Back to plugins");
    back.addEventListener("click", close);

    const header = document.createElement("div");
    header.className = "plugin-header";
    const hIconWrap = document.createElement("span");
    hIconWrap.className = "plugin-header-icon";
    const hIcon = document.createElement("i");
    hIcon.setAttribute("data-lucide", spec.icon);
    hIconWrap.appendChild(hIcon);
    const hName = document.createElement("span");
    hName.className = "plugin-header-name";
    hName.textContent = spec.name;
    header.appendChild(hIconWrap);
    header.appendChild(hName);

    // Fresh container per open() — never reused, so a plugin's own DOM/state
    // cannot leak across open/close cycles.
    const container = document.createElement("div");
    container.className = "plugin-container";

    view.appendChild(back);
    view.appendChild(header);
    view.appendChild(container);
    rootEl.appendChild(view);
    refreshIcons();
    return container;
  }

  /**
   * Run the active plugin's destroy() and null state WITHOUT touching the DOM.
   * Shared by close() (which then restores the grid) and open() (which then
   * renders the next view) so opening another plugin never double-renders the
   * grid. Passes the same container init got, so destroy can tear down what it
   * built. Never throws.
   */
  function teardownActive() {
    if (activeId === null) return null;
    const spec = registry.get(activeId);
    const container = activeContainer;
    activeId = null;
    activeContainer = null;
    if (spec) {
      try { spec.destroy(container); }
      catch (err) { console.error("plugin destroy failed:", spec.id, err); }
    }
    return container;
  }

  /**
   * Open a plugin by id. If another plugin is already open, close it first
   * (destroy runs exactly once) so only one plugin is ever live.
   * @param {string} id
   * @returns {boolean}
   */
  function open(id) {
    const spec = registry.get(id);
    if (!spec) return false;
    if (activeId !== null) {
      if (activeId === id) return true;     // already open — no-op
      teardownActive();                     // close current first (destroy runs)
    }
    const container = renderPluginView(spec);
    activeId = id;
    activeContainer = container;
    try {
      spec.init(container, api);
    } catch (err) {
      console.error("plugin init failed:", id, err);
    }
    return true;
  }

  /**
   * Close the active plugin: run destroy() (best-effort, never throws, receives
   * its container), drop the view, restore the grid.
   */
  function close() {
    if (activeId === null) return;
    teardownActive();
    if (rootEl) renderGrid();
  }

  /**
   * Build the shared api once and wire the FUTURE content element.
   * Call from panel.initFuture(). Building api here (not in panel.js) keeps
   * the plugin contract self-contained in this module.
   * @param {HTMLElement} futureContentEl
   * @param {Object} [telemetry] Corvus.telemetry (defaults to the live module)
   */
  function init(futureContentEl, telemetry) {
    rootEl = futureContentEl;
    const t = telemetry || (window.Corvus && Corvus.telemetry);
    api = {
      telemetry: t,
      subscribe: t ? t.subscribe.bind(t) : null,
      getState: t ? t.getState.bind(t) : null,
      requestJson: t ? t.requestJson.bind(t) : null,
      postAction: t ? t.postAction.bind(t) : null,
      reducedMotion: () => !!(typeof window !== "undefined" && window.matchMedia
        && window.matchMedia("(prefers-reduced-motion: reduce)").matches),
    };
    renderGrid();
  }

  return { register, unregister, list, init, open, close, getActive };
})();
