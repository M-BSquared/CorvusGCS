"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.plugins — minimal plugin registry powering the FUTURE tab.
 *
 * The FUTURE tab is the documented extension point of Corvus GCS. Plugins
 * register here at module load (IIFE side-effect) and the grid renders them as
 * cards; clicking a card swaps the grid for the plugin's container and hands
 * it a lean `api`. Only one plugin is open at a time; opening another first
 * closes the current so destroy() runs exactly once (no listener leaks —
 * apple-design "interruptible by default").
 *
 * Plugin `api` contract (built once in init(), handed to every plugin.init):
 *
 *   telemetry {Object|null}       The live Corvus.telemetry module. Plugins may
 *                                 call it directly or via the bound helpers.
 *   subscribe(cb) {() => unsub}   Subscribe to the coalesced telemetry stream.
 *                                 Returns an unsubscribe fn; call it in
 *                                 destroy() to avoid listener leaks.
 *   getState() {() => state}      Synchronous snapshot of the latest state.
 *   requestJson(url)              GET a JSON endpoint; rejects on HTTP/network
 *                                 error or invalid JSON (see telemetry.js).
 *   postAction(url, body)         POST a config action; rejects on a vehicle
 *                                 rejection so callers can surface failures.
 *   reducedMotion() {() => bool}  True when the OS asks for less motion
 *                                 (prefers-reduced-motion: reduce).
 *   console(text, level)          Append a line to the MAVLink console output
 *                                 (panel.addConsoleLine). Best-effort + guarded:
 *                                 a missing/broken panel never crashes a plugin.
 *                                 `level` is one of the existing console classes:
 *                                 "" (plain), "cmd", "success", "warning",
 *                                 "error", "nav", "info". Unrecognised/missing
 *                                 levels default to "" (plain).
 *   notification(level, message)  Surface a local notification in the warnings
 *                                 popover via the corvus:notification window
 *                                 event (the topbar already listens for it).
 *                                 `level` is "info" | "warning" | "critical";
 *                                 default "info". Best-effort + guarded.
 *   modes() {() => Promise<string[]>}
 *                                 The connected firmware's flight-mode list
 *                                 (GET /api/mavlink/modes). Cached for the
 *                                 session (modes rarely change per firmware);
 *                                 resolves to [] on error or when no transport.
 *                                 Lets a plugin offer mode-aware UI without
 *                                 duplicating the fetch.
 *
 * All feedback helpers are frontend-only: they never fork the server.
 */
Corvus.plugins = (function () {
  // Insertion-ordered map: id -> spec. A Map preserves registration order so
  // the grid is deterministic regardless of object-key ordering quirks.
  const registry = new Map();

  let rootEl = null;     // the FUTURE content element the grid renders into
  let api = null;        // the shared api object handed to every plugin
  let activeId = null;   // currently-open plugin id (null = grid showing)
  let activeContainer = null;  // the container handed to the active plugin

  // Levels accepted by api.console(); they map 1:1 to the con-line CSS classes
  // used by the MAVLink console (panel.addConsoleLine). "" is the plain style.
  const CONSOLE_LEVELS = new Set(["", "cmd", "success", "warning", "error", "nav", "info"]);

  // Session cache for api.modes(): the flight-mode list rarely changes per
  // firmware, so a single fetch serves every plugin for the session.
  let modesCache = null;      // settled modes array (null = not yet loaded)
  let modesInFlight = null;   // pending promise, dedupes concurrent callers

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
      '<div class="future-title">Tools & Plugins</div>' +
      '<div class="future-desc">Extensions and tools plug in here.</div>';
    rootEl.classList.add("future-empty");
    Corvus.ui.refreshIcons();
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
      // Same control as a Setup tile, laid out as a column instead of a row —
      // ui.tile builds it from createElement (never innerHTML), so a plugin's
      // name/description stay real text nodes and cannot inject markup.
      const card = Corvus.ui.tile({
        className: "plugin-card",
        icon: p.icon,
        title: p.name,
        desc: p.description,
        ariaLabel: p.name,
        onClick: () => open(p.id),
      });
      card.dataset.pluginId = p.id;
      grid.appendChild(card);
    });
    rootEl.appendChild(grid);
    Corvus.ui.refreshIcons();
  }

  /** Render the opened-plugin view: back button + header + fresh container. */
  function renderPluginView(spec) {
    rootEl.classList.remove("future-empty");
    rootEl.innerHTML = "";

    const view = document.createElement("div");
    view.className = "plugin-view";

    const back = Corvus.ui.button({
      variant: "ghost",
      size: "sm",
      className: "plugin-back",
      icon: "chevron-left",
      label: "Plugins",
      ariaLabel: "Back to plugins",
      onClick: close,
    });

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
    Corvus.ui.refreshIcons();
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
   * Append a line to the MAVLink console (panel.addConsoleLine). Best-effort
   * and guarded: a missing/broken panel never throws into a plugin.
   * @param {*} text      Message text (coerced to string).
   * @param {string} [level] One of CONSOLE_LEVELS; defaults to "" (plain).
   */
  function pluginConsole(text, level) {
    const lv = CONSOLE_LEVELS.has(level) ? level : "";
    try {
      if (window.Corvus && window.Corvus.panel
        && typeof window.Corvus.panel.addConsoleLine === "function") {
        window.Corvus.panel.addConsoleLine(lv, String(text));
      }
    } catch (_e) {
      // A missing/broken console must never crash a plugin.
    }
  }

  /**
   * Surface a local notification in the warnings popover by dispatching the
   * corvus:notification window event the topbar already listens for. Best-effort
   * and guarded: a missing topbar / event target never throws into a plugin.
   * @param {string} [level] "info" | "warning" | "critical"; default "info".
   * @param {*} message     Message text (coerced to string).
   */
  function pluginNotification(level, message) {
    const lv = (level === "warning" || level === "critical") ? level : "info";
    try {
      window.dispatchEvent(new CustomEvent("corvus:notification", {
        detail: { level: lv, message: String(message) },
      }));
    } catch (_e) {
      // No topbar / no event target — never crash a plugin.
    }
  }

  /**
   * Return the connected firmware's flight-mode list (GET /api/mavlink/modes).
   * Cached for the session; concurrent callers share one fetch; a transient
   * error resolves to [] and clears the in-flight promise so a later call
   * retries. Resolves to [] when no requestJson transport is available.
   * @returns {Promise<string[]>}
   */
  function pluginModes() {
    if (modesCache) return Promise.resolve(modesCache);
    if (modesInFlight) return modesInFlight;
    const req = api && api.requestJson;
    if (typeof req !== "function") return Promise.resolve([]);
    modesInFlight = Promise.resolve()
      .then(() => req("/api/mavlink/modes"))
      .then(
        (data) => { modesCache = Array.isArray(data && data.modes) ? data.modes : []; return modesCache; },
        () => []   // best-effort: an error never rejects a plugin's UI
      )
      .then((result) => { modesInFlight = null; return result; });
    return modesInFlight;
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
      console: pluginConsole,
      notification: pluginNotification,
      modes: pluginModes,
    };
    renderGrid();
  }

  return { register, unregister, list, init, open, close, getActive };
})();
