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
 *   postJson(url, body)           POST that resolves with the parsed body even
 *                                 when it carries {ok:false} — for endpoints
 *                                 whose failure detail (stderr, an exit
 *                                 status) is the thing worth showing. Use
 *                                 postAction instead when a rejection should
 *                                 simply reject.
 *   terminal(session)             Show the live terminal for an SSH session the
 *                                 plugin opened (POST /api/ssh/connect), in a
 *                                 floating window of its own — one per
 *                                 session, so a plugin may hold several at
 *                                 once and the operator keeps the tab they
 *                                 were on. `session` is
 *                                 {name, title, host, port, username}: `name`
 *                                 is the session key, `title` what the window
 *                                 header reads. Calling it again for the same
 *                                 session raises that window. Second argument,
 *                                 both optional: {reattach: true} right after
 *                                 (re)connecting, so an open window takes the
 *                                 new shell instead of a dead stream;
 *                                 {existingOnly: true} to repair a window that
 *                                 is already open WITHOUT opening, raising or
 *                                 focusing one — what a plugin does on an
 *                                 action the operator did not ask to watch.
 *                                 Closing the window leaves the session
 *                                 running; the window's disconnect button ends
 *                                 it. Returns true when a terminal is showing
 *                                 it. Best-effort + guarded, like console().
 *   getSettings() {() => Object}  This plugin's saved settings, from the
 *                                 config file. {} when it has never saved any.
 *   saveSettings(patch, replace)  Merge `patch` into them and persist
 *                                 (POST /api/plugins/settings). Resolves with
 *                                 the saved object. Pass `replace` true to
 *                                 store `patch` as the whole settings object
 *                                 instead — the only way to drop a key, since
 *                                 a merge can only add. Plain UI state only —
 *                                 the config file is not a secret store, so a
 *                                 plugin names a saved SSH connection rather
 *                                 than keeping a password.
 *
 * getSettings/saveSettings are bound to the plugin they were handed to, so a
 * plugin cannot read or overwrite another's settings by accident.
 *
 * All feedback helpers are frontend-only: they never fork the server.
 *
 * ---------------------------------------------------------------------------
 * Installed plugins (the drop-in folder)
 * ---------------------------------------------------------------------------
 * Beyond the plugins that ship as <script> tags in index.html, `loadInstalled`
 * fetches GET /api/plugins and appends each installed plugin's styles and
 * scripts at boot. A plugin is a folder — bundled in the application's own
 * `plugins/`, or dropped by the operator into `~/.corvus/plugins` — holding a
 * `plugin.json` manifest and the files it names; see corvus/plugin_registry.py
 * for the manifest, and the Plugins section of Settings for the button that
 * opens the folder.
 *
 * Loading is deliberately late and forgiving: a plugin that fails to fetch or
 * throws while loading costs that card and nothing else, and because register()
 * re-renders the grid, a plugin arriving after the FUTURE tab is already open
 * simply appears. Nothing is hot-reloaded — a newly dropped-in plugin shows up
 * on the next start, which is the contract the Settings hint states.
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

  // Saved per-plugin settings, id -> object. Seeded from GET /api/plugins at
  // boot and kept in step with every saveSettings, so getSettings can answer
  // synchronously while a plugin is building its form.
  let settingsStore = Object.create(null);

  // Ids whose scripts loadInstalled has already appended, so a second call
  // (a reload of the FUTURE tab, a retry) cannot load the same plugin twice
  // and have its register() rejected as a duplicate.
  const loadedInstalled = new Set();

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
      spec.init(container, apiFor(id));
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
   * POST `body` to `url` and resolve with the parsed response body, whatever
   * it says. Unlike postAction this does NOT reject on {ok:false}: some
   * endpoints put the interesting part of a failure (a remote command's
   * stderr, its exit status) in the body, and rejecting would throw it away.
   * A network/parse error still rejects, because then there is no body.
   * @param {string} url
   * @param {Object} body
   * @returns {Promise<Object>}
   */
  function pluginPostJson(url, body) {
    const req = api && api.requestJson;
    if (typeof req !== "function") return Promise.reject(new Error("no transport"));
    return req(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body || {}),
    });
  }

  /** This plugin's saved settings; always an object, never null. */
  function getSettingsFor(id) {
    const saved = settingsStore[id];
    return saved && typeof saved === "object" ? Object.assign({}, saved) : {};
  }

  /**
   * Merge `patch` into a plugin's saved settings and persist them. The local
   * copy is updated from the server's answer, so a rejected key never lingers
   * in the UI as if it had been saved.
   *
   * With `replace` the object is stored as given instead of merged, which is
   * how a plugin that owns its whole settings object drops a key it no longer
   * writes — a merge can only ever add.
   *
   * @param {string} id
   * @param {Object} patch
   * @param {boolean} [replace]
   * @returns {Promise<Object>} the saved settings
   */
  function saveSettingsFor(id, patch, replace) {
    if (!patch || typeof patch !== "object") return Promise.resolve(getSettingsFor(id));
    return pluginPostJson("/api/plugins/settings", { id, settings: patch, replace: !!replace })
      .then((res) => {
        const saved = (res && res.settings)
          || (replace ? Object.assign({}, patch) : Object.assign(getSettingsFor(id), patch));
        settingsStore[id] = saved;
        return Object.assign({}, saved);
      });
  }

  /**
   * The shared api with getSettings/saveSettings bound to one plugin. Built
   * per open() on top of the shared object (prototype delegation, not a copy)
   * so every plugin keeps seeing the same telemetry/console helpers and a
   * later addition to `api` needs no change here.
   * @param {string} id
   * @returns {Object}
   */
  function apiFor(id) {
    const scoped = Object.create(api);
    scoped.getSettings = () => getSettingsFor(id);
    scoped.saveSettings = (patch, replace) => saveSettingsFor(id, patch, replace);
    return scoped;
  }

  /**
   * Show the live terminal for an SSH session the plugin opened.
   *
   * Every session gets a floating window of its own (Corvus.termWindows), so a
   * plugin with several sessions has several terminals side by side and the
   * operator stays on the tab they were working in. Pointing them all at the
   * panel's single SSH tab — which is what this did — meant four independent
   * programs sharing one terminal, each opening replacing the last, and the
   * shelf that started them left behind.
   *
   * Calling this again for a session that already has a window raises and
   * focuses that window instead of opening a second one.
   *
   * Best-effort and guarded: a missing window layer falls back to the SSH tab,
   * and anything throwing returns false rather than throwing into the plugin.
   *
   * @param {Object} session {name, title, host, port, username}
   * @param {Object} [opts] {reattach} — pass true right after (re)connecting
   *        the session, so a window already open on it takes the new shell
   *        rather than a stream the replaced session left behind.
   * @returns {boolean} whether a terminal is now showing it
   */
  function pluginTerminal(session, opts) {
    if (!session || typeof session.name !== "string" || !session.name) return false;
    try {
      const windows = window.Corvus && window.Corvus.termWindows;
      if (windows && typeof windows.open === "function") return windows.open(session, opts);
      // The panel's tab is the older, single-terminal path; still better than
      // nothing if term-window.js did not load.
      const panel = window.Corvus && window.Corvus.panel;
      if (!panel || typeof panel.showSSHTerminal !== "function") return false;
      panel.showSSHTerminal(session);
      if (typeof panel.showTab === "function") panel.showTab("ssh");
      return true;
    } catch (_e) {
      return false;      // a broken panel must never crash a plugin
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
      terminal: pluginTerminal,
      postJson: pluginPostJson,
      // Overwritten per plugin by apiFor(); present here so the shape is the
      // same object whether a plugin was handed the scoped api or reached the
      // shared one, and so a plugin calling them outside init() gets an empty
      // answer rather than a TypeError.
      getSettings: () => ({}),
      saveSettings: () => Promise.resolve({}),
    };
    renderGrid();
  }

  /**
   * Load the plugins installed as folders (GET /api/plugins) by appending
   * their styles and scripts to the document. Each plugin registers itself as
   * its script runs, exactly as a built-in one does.
   *
   * Resolves when every plugin has been attempted — never rejects. One plugin
   * that 404s, throws on load, or was already loaded costs that plugin only:
   * the FUTURE tab is an extension point, and a bad extension must not be able
   * to take the tab (or the boot) down with it.
   *
   * @returns {Promise<string[]>} the ids that were loaded this call
   */
  function loadInstalled() {
    const req = api && api.requestJson
      ? api.requestJson
      : (window.Corvus && Corvus.telemetry && Corvus.telemetry.requestJson);
    if (typeof req !== "function") return Promise.resolve([]);
    return Promise.resolve()
      .then(() => req("/api/plugins"))
      .then((data) => {
        // Seed the settings store before any plugin script runs, so the first
        // getSettings() inside a plugin's init already has the saved values.
        if (data && data.settings && typeof data.settings === "object") {
          settingsStore = Object.assign(Object.create(null), data.settings);
        }
        const plugins = (data && Array.isArray(data.plugins)) ? data.plugins : [];
        const pending = [];
        plugins.forEach((p) => {
          if (!p || typeof p.id !== "string" || !p.id) return;
          if (loadedInstalled.has(p.id)) return;
          loadedInstalled.add(p.id);
          (Array.isArray(p.styles) ? p.styles : []).forEach((href) => {
            appendStyle(assetUrl(p.id, href));
          });
          // Sequential per plugin: a manifest listing several scripts means the
          // later ones may build on the earlier ones, and <script> tags appended
          // together carry no such guarantee.
          let chain = Promise.resolve();
          (Array.isArray(p.scripts) ? p.scripts : []).forEach((src) => {
            chain = chain.then(() => appendScript(assetUrl(p.id, src)));
          });
          pending.push(chain.then(() => p.id, (err) => {
            console.error("plugin failed to load:", p.id, err);
            return null;
          }));
        });
        return Promise.all(pending).then((ids) => ids.filter((id) => id !== null));
      })
      .catch((err) => {
        console.error("plugin discovery failed:", err);
        return [];
      });
  }

  /** URL of one file inside a plugin's folder (each segment encoded, "/" kept). */
  function assetUrl(id, rel) {
    const path = String(rel).split("/").map(encodeURIComponent).join("/");
    return `/api/plugins/asset/${encodeURIComponent(id)}/${path}`;
  }

  /** Append a stylesheet link. Fire-and-forget: a missing CSS is cosmetic. */
  function appendStyle(href) {
    const link = document.createElement("link");
    link.rel = "stylesheet";
    link.href = href;
    document.head.appendChild(link);
  }

  /** Append a script tag; resolves on load, rejects on error. */
  function appendScript(src) {
    return new Promise((resolve, reject) => {
      const el = document.createElement("script");
      el.src = src;
      // Default (async=false for an inserted script with src) preserves the
      // chain's order; the promise is what the caller actually sequences on.
      el.onload = () => resolve();
      el.onerror = () => reject(new Error(`could not load ${src}`));
      document.head.appendChild(el);
    });
  }

  return { register, unregister, list, init, open, close, getActive, loadInstalled };
})();
