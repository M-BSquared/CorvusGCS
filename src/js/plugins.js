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
 *   notification(level, message)  Say something to the operator, in both the
 *                                 places it belongs: a toast over whatever
 *                                 they are looking at, and a line on the
 *                                 notification board (the corvus:notification
 *                                 window event the topbar listens for), which
 *                                 is where it stays once the toast has gone.
 *                                 The toast is titled with the plugin's name
 *                                 and the board line is prefixed with it, so
 *                                 a plugin writes its own message and nothing
 *                                 else. `level` is "info" | "warning" |
 *                                 "critical"; default "info" — a "critical"
 *                                 toast stays up until it is dismissed.
 *                                 Best-effort + guarded.
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
 *                                 Where the window opens is the operator's
 *                                 setting, not the plugin's: in the desktop
 *                                 app a window of its own by default (with a
 *                                 pin that keeps it above Corvus), or a frame
 *                                 inside the app that can be dragged out;
 *                                 see js/popout.js. The plugin calls this the
 *                                 same way either way.
 *   sshSetup(reply) {(reply) => Promise<boolean>}
 *                                 For a plugin that runs things over a saved
 *                                 SSH connection it only knows by name. Pass
 *                                 it the reply of a failed /api/ssh/connect or
 *                                 /api/ssh/run (or the Error a rejected
 *                                 request threw; it carries the body as
 *                                 `.body`). When the reply says this computer
 *                                 lacks that connection or its login was
 *                                 refused (`needs`), the operator is asked for
 *                                 host, user and password once, they are saved
 *                                 under the same name, and this resolves true:
 *                                 try again. Anything else resolves false
 *                                 without asking. This is what makes a plugin
 *                                 config copied from another computer work
 *                                 after one prompt. Best-effort + guarded.
 *   getSettings() {() => Object}  This plugin's saved settings, from its own
 *                                 config file (<plugin folder>/<id>/config.json,
 *                                 apart from the application's config, so it
 *                                 can be copied to another machine on its
 *                                 own). {} when it has never saved any.
 *   saveSettings(patch, replace)  Merge `patch` into them and persist
 *                                 (POST /api/plugins/settings). Resolves with
 *                                 the saved object. Pass `replace` true to
 *                                 store `patch` as the whole settings object
 *                                 instead — the only way to drop a key, since
 *                                 a merge can only add. Plain UI state only —
 *                                 that file is not a secret store, so a
 *                                 plugin names a saved SSH connection rather
 *                                 than keeping a password.
 *
 *   map {Object}                  Shapes and text on the Home map. Lines and
 *                                 areas are always drawn UNDER the flown track,
 *                                 so where the aircraft has been stays on top;
 *                                 text sits over it. Every key is the plugin's
 *                                 own: two plugins can both draw "path" without
 *                                 touching each other's, and one key holds one
 *                                 shape of any kind (drawing it again replaces
 *                                 it, whatever it was before).
 *     colors                      [{id, label, color}]: the colours that read
 *                                 against imagery and cannot be mistaken for
 *                                 the track (red) or the plan route (amber).
 *                                 Offer these rather than a free colour picker.
 *     drawLine(key, coords, opts) Draw or replace a line. `coords` is
 *                                 [[lng, lat], ...] (a third value is ignored);
 *                                 `opts` {color, width, opacity, dashed,
 *                                 visible}. False for fewer than two usable
 *                                 points.
 *     drawPolygon(key, coords, opts)
 *                                 Draw or replace an area: the points are its
 *                                 corners, in order, and the surface between
 *                                 them is filled. The ring closes itself.
 *                                 `opts` {color, fillOpacity (default 0.25),
 *                                 width (outline, default 2, 0 for none),
 *                                 opacity, dashed, visible}. False for fewer
 *                                 than three usable corners.
 *     drawCircle(key, center, radius, opts)
 *                                 An area that is a circle of `radius` metres
 *                                 around [lng, lat]; `opts` as drawPolygon.
 *     drawText(key, point, text, opts)
 *                                 Draw or replace a text label at [lng, lat].
 *                                 Plain text, never markup, at most 200
 *                                 characters. `opts` {color, size (px, 9 to
 *                                 32, default 12), dot (a point marker with
 *                                 the text beside it), opacity, visible}.
 *     remove(key)                 Take a shape off the map.
 *     setVisible(key, on)         Hide or show a shape without forgetting it.
 *     has(key)                    Whether this plugin has a shape under `key`.
 *     fit(coords)                 Bring the Home page forward and frame these
 *                                 [lng, lat] points (stops following the
 *                                 aircraft, as a drag of the map would).
 *                                 Every draw works before the map has loaded
 *                                 (it is drawn once it has) and answers true
 *                                 when drawn. A shape outlives the plugin's
 *                                 view: closing the card does not remove it,
 *                                 so a plugin removes what it drew when the
 *                                 operator asks.
 *
 *   getOptions() {() => Object}   The values of the options the plugin
 *                                 declared (see `options` below), each one
 *                                 its saved value or its default.
 *
 * getSettings/saveSettings are bound to the plugin they were handed to, so a
 * plugin cannot read or overwrite another's settings by accident, and
 * notification is bound the same way so a message carries the name of the
 * plugin that sent it without that plugin having to write it out. map and
 * getOptions are bound the same way.
 *
 * All feedback helpers are frontend-only: they never fork the server.
 *
 * ---------------------------------------------------------------------------
 * The spec a plugin registers
 * ---------------------------------------------------------------------------
 *   name, icon, description       The card (and the tab caption).
 *   init(containerEl, api)        Build the view into the container.
 *   destroy(containerEl)          Tear it down again.
 *   start(api)       optional     Runs once per session as soon as the plugin
 *                                 is registered and the api exists, whether or
 *                                 not the operator ever opens it. For what has
 *                                 to be there from the start: a line on the
 *                                 map restored from saved settings. Keep it
 *                                 light; it has no container and no destroy.
 *   tab: true        optional     The plugin can live in a tab of its own in
 *                                 the side panel, before PLUGINS, instead of a
 *                                 card under it. Its settings in Settings >
 *                                 Plugins then carry an "Own tab" switch, and
 *                                 the operator decides; until they have, the
 *                                 old global ui.plugin_tabs decides (off by
 *                                 default). So a plugin has to work as both. A
 *                                 tab is built the first time it is shown and
 *                                 kept while the operator moves between tabs;
 *                                 destroy runs when the tab goes away (the
 *                                 switch turned off, or the plugin
 *                                 unregistered).
 *   options          optional     Settings the operator sets in a dialog that
 *                                 opens from the plugin's gear in Settings >
 *                                 Plugins, rendered by Corvus so every plugin's
 *                                 look the same. An array of
 *                                 {key, label, type, default, hint, ...}:
 *                                   type "toggle"  a switch, value true/false
 *                                   type "select"  `choices` [{value, label}]
 *                                   type "text"    `placeholder`
 *                                   type "number"  `min`, `max`, `step`
 *                                 `key` is a short identifier ("tab" is taken).
 *                                 The values are saved in the plugin's own
 *                                 config.json, under "_options", where a
 *                                 saveSettings never touches them; read them
 *                                 with api.getOptions().
 *   optionsChanged(options, api)  optional
 *                                 Runs after the operator changed an option,
 *                                 with every value, so an open view or a
 *                                 drawn shape can follow at once.
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
 * simply appears. `reload` (Settings > Plugins, Reload plugins) unloads every
 * installed plugin and loads the folders again, so a plugin dropped in, edited
 * or removed takes effect without restarting Corvus.
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

  // Ids whose start(api) has run. Once per session, never again on a second
  // init() or a re-render.
  const startedIds = new Set();

  // The <script> and <link> elements loadInstalled appended, id -> [el], so a
  // reload can take a plugin's files out of the document before loading them
  // again.
  const installedEls = new Map();

  // Bumped by every reload and put on the asset URLs, so the browser fetches
  // a file that changed on disk instead of answering from its cache.
  let loadRound = 0;

  // Where a plugin's operator-set options live inside its config.json: next
  // to whatever the plugin saves itself, under a key the plugin never sees
  // (getSettings leaves it out, and the backend keeps it through a
  // saveSettings with replace).
  const OPTIONS_KEY = "_options";
  // The one option Corvus adds by itself, for a plugin that can have a tab.
  const TAB_OPTION = "tab";
  const OPTION_TYPES = new Set(["toggle", "select", "text", "number"]);
  const OPTION_KEY_RE = /^[A-Za-z][A-Za-z0-9_]{0,31}$/;

  // Whether Settings lets a plugin that asks for one have a tab of its own
  // (ui.plugin_tabs). Off until app.js reads the config: a plugin is a card
  // unless the operator has said otherwise.
  let tabsAllowed = false;

  // Plugins showing as a tab right now: id -> {section, container, live}.
  // `live` is whether init has run on that container.
  const tabbed = new Map();

  // The overlay keys each plugin has drawn, id -> Set, so unregister can take
  // a plugin's shapes off the map with it.
  const ownedOverlays = new Map();

  // The colours a plugin's line may be drawn in (api.map.colors). Literal
  // values, not theme tokens, for the reason the buildings give in map.js:
  // a line is read against imagery, not against the interface. None of them
  // is red or amber, which are the flown track and the plan route.
  const LINE_COLORS = Object.freeze([
    { id: "cyan", label: "Cyan", color: "#22D3EE" },
    { id: "lime", label: "Lime", color: "#A3E635" },
    { id: "magenta", label: "Magenta", color: "#D946EF" },
    { id: "blue", label: "Blue", color: "#3B82F6" },
    { id: "white", label: "White", color: "#FFFFFF" },
  ].map(Object.freeze));

  /**
   * Register a plugin. Returns true on success, false on rejection.
   * Duplicate or empty ids are rejected (returns false) rather than throwing,
   * so a misbehaving plugin module never breaks app init.
   *
   * @param {string} id   Unique non-empty plugin id.
   * @param {Object} spec {name, icon, description, init, destroy, start?, tab?,
   *                       options?, optionsChanged?}
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
      start: typeof spec.start === "function" ? spec.start : null,
      tab: spec.tab === true,
      options: cleanOptionSpecs(spec.options),
      optionsChanged: typeof spec.optionsChanged === "function" ? spec.optionsChanged : null,
    });
    runStart(id);
    // A tab if it asks for one and may have one; otherwise the grid, if it is
    // on screen, re-renders so the new card appears.
    syncTabs();
    return true;
  }

  /**
   * Remove a plugin. If it is currently active, close it first so destroy()
   * runs and the grid is restored. Its tab and its lines on the map go too.
   * @param {string} id
   * @returns {boolean}
   */
  function unregister(id) {
    if (!registry.has(id)) return false;
    if (activeId === id) close();
    if (tabbed.has(id)) unmountTab(id);
    removeOverlaysOf(id);
    registry.delete(id);
    startedIds.delete(id);
    if (rootEl && activeId === null) renderGrid();
    return true;
  }

  /**
   * List registered plugins in registration order. `tab` is whether the
   * plugin can have a tab of its own, `inTab` whether it has one right now,
   * `hasOptions` whether it has anything to set in Settings.
   */
  function list() {
    const out = [];
    registry.forEach((spec) => {
      out.push({
        id: spec.id, name: spec.name, icon: spec.icon, description: spec.description,
        tab: spec.tab, inTab: tabbed.has(spec.id), hasOptions: hasOptions(spec.id),
      });
    });
    return out;
  }

  /** Run a plugin's start(api) once, as soon as both it and the api exist. */
  function runStart(id) {
    const spec = registry.get(id);
    if (!spec || !spec.start || !api || startedIds.has(id)) return;
    startedIds.add(id);
    try {
      spec.start(apiFor(id));
    } catch (err) {
      console.error("plugin start failed:", id, err);
    }
  }

  // ---- plugin tabs ------------------------------------------------------

  /** The panel tab id for a plugin. Prefixed so no plugin id can collide
   *  with one of the panel's own tabs ("ssh", "console", ...). */
  function tabIdFor(id) { return "plugin-" + id; }

  function panelTabs() {
    const panel = window.Corvus && window.Corvus.panel;
    return panel && typeof panel.addTab === "function" && typeof panel.removeTab === "function"
      ? panel : null;
  }

  /**
   * The old global switch (ui.plugin_tabs), now only the default for a
   * plugin whose own "Own tab" option the operator has not set. Plugins move
   * between their tab and their card at once: a tab that goes away runs its
   * plugin's destroy, and the card is back in the grid.
   * @param {boolean} on
   * @returns {boolean} the setting now in force
   */
  function setTabsAllowed(on) {
    tabsAllowed = !!on;
    syncTabs();
    return tabsAllowed;
  }

  /** Give every plugin the tab it should have, and take away the rest. */
  function syncTabs() {
    const panel = panelTabs();
    registry.forEach((spec, id) => {
      const want = !!(panel && spec.tab && wantsTab(id));
      if (want && !tabbed.has(id)) mountTab(spec, panel);
      else if (!want && tabbed.has(id)) unmountTab(id);
    });
    if (rootEl && activeId === null) renderGrid();
  }

  function mountTab(spec, panel) {
    // Open as a card right now: it is closed first, so one plugin never runs
    // twice, and the grid it leaves is redrawn without it.
    if (activeId === spec.id) {
      teardownActive();
      if (rootEl) renderGrid();
    }
    const entry = { section: null, container: null, live: false };
    tabbed.set(spec.id, entry);
    let section = null;
    try {
      section = panel.addTab({
        id: tabIdFor(spec.id),
        label: spec.name,
        title: spec.name,
        onShow: () => startTab(spec.id),
      });
    } catch (err) {
      console.error("plugin tab failed:", spec.id, err);
    }
    if (!section) { tabbed.delete(spec.id); return; }

    // The same header the SSH and PLUGINS tabs open with, then the plugin's
    // own container. Fresh for every mount, like a card's.
    const header = document.createElement("div");
    header.className = "panel-section-header";
    const title = document.createElement("span");
    title.className = "panel-section-title";
    title.textContent = spec.name;
    header.appendChild(title);
    const container = document.createElement("div");
    container.className = "plugin-container plugin-tab-container";
    section.appendChild(header);
    section.appendChild(container);
    entry.section = section;
    entry.container = container;
  }

  /** The tab was brought forward: build the plugin's view the first time. */
  function startTab(id) {
    const entry = tabbed.get(id);
    const spec = registry.get(id);
    if (!entry || !spec || entry.live || !entry.container) return;
    entry.live = true;
    try {
      spec.init(entry.container, apiFor(id));
    } catch (err) {
      console.error("plugin init failed:", id, err);
    }
    refreshIcons();
  }

  /** Render the Lucide icons a plugin's view asked for with data-lucide. A
   *  plugin builds its buttons with Corvus.ui and should not have to know
   *  that they need this afterwards. */
  function refreshIcons() {
    try {
      if (window.Corvus && Corvus.ui && typeof Corvus.ui.refreshIcons === "function") {
        Corvus.ui.refreshIcons();
      }
    } catch (_e) { /* cosmetic */ }
  }

  function unmountTab(id) {
    const entry = tabbed.get(id);
    tabbed.delete(id);
    if (!entry) return;
    const spec = registry.get(id);
    if (entry.live && spec) {
      try { spec.destroy(entry.container); }
      catch (err) { console.error("plugin destroy failed:", id, err); }
    }
    const panel = panelTabs();
    if (panel) {
      try { panel.removeTab(tabIdFor(id)); }
      catch (err) { console.error("plugin tab removal failed:", id, err); }
    }
  }

  // ---- shapes and text on the map (api.map) -------------------------------

  /** The map's overlay id for one of a plugin's keys. Plugin ids are already
   *  narrow (see plugin_registry.py); the key is narrowed the same way. */
  function overlayIdFor(id, key) {
    return "plugin-" + id + "-" + String(key).replace(/[^A-Za-z0-9_.-]/g, "_");
  }

  function liveMap() {
    const m = window.Corvus && window.Corvus.map;
    return m && typeof m.setOverlay === "function" ? m : null;
  }

  const EARTH_RADIUS_M = 6371008.8;

  /**
   * The corners of a circle of `radius` metres around [lng, lat], as a
   * polygon ring (not closed). Spherical, which is far closer than anything
   * a plugin's circle is drawn to show. Empty for an unusable centre or
   * radius. Pure.
   * @param {[number, number]} center
   * @param {number} radius metres, up to 1000 km
   * @param {number} [segments] corners, 8 to 256 (default 72)
   * @returns {Array<[number, number]>}
   */
  function circleRing(center, radius, segments) {
    if (!Array.isArray(center) || center.length < 2) return [];
    const lng = Number(center[0]);
    const lat = Number(center[1]);
    const r = Number(radius);
    if (!isFinite(lng) || !isFinite(lat) || Math.abs(lat) > 90 || Math.abs(lng) > 180) return [];
    if (!isFinite(r) || r <= 0 || r > 1e6) return [];
    const n = Math.min(Math.max(Math.round(Number(segments) || 72), 8), 256);
    const rad = Math.PI / 180;
    const phi1 = lat * rad;
    const lambda1 = lng * rad;
    const delta = r / EARTH_RADIUS_M;
    const out = [];
    for (let i = 0; i < n; i++) {
      const theta = (2 * Math.PI * i) / n;
      const phi2 = Math.asin(Math.sin(phi1) * Math.cos(delta)
        + Math.cos(phi1) * Math.sin(delta) * Math.cos(theta));
      const lambda2 = lambda1 + Math.atan2(
        Math.sin(theta) * Math.sin(delta) * Math.cos(phi1),
        Math.cos(delta) - Math.sin(phi1) * Math.sin(phi2));
      let outLng = lambda2 / rad;
      if (outLng > 180) outLng -= 360;
      else if (outLng < -180) outLng += 360;
      out.push([outLng, Math.max(-90, Math.min(90, phi2 / rad))]);
    }
    return out;
  }

  function removeOverlaysOf(id) {
    const keys = ownedOverlays.get(id);
    ownedOverlays.delete(id);
    const m = liveMap();
    if (!keys || !m) return;
    keys.forEach((key) => {
      try { m.removeOverlay(overlayIdFor(id, key)); } catch (_e) { /* best effort */ }
    });
  }

  /**
   * api.map for one plugin. Every call is guarded: a missing or broken map
   * answers false, never throws into the plugin.
   * @param {string} id
   * @returns {Object}
   */
  function mapApiFor(id) {
    const validKey = (key) => typeof key === "string" && key.length > 0 && key.length <= 64;
    const owned = () => {
      if (!ownedOverlays.has(id)) ownedOverlays.set(id, new Set());
      return ownedOverlays.get(id);
    };
    // One guarded draw for every kind: the map method by name, so a map that
    // lacks it (an older one, a test stub) answers false like a missing map.
    const draw = (method, key, args) => {
      const m = liveMap();
      if (!m || !validKey(key) || typeof m[method] !== "function") return false;
      try {
        const drawn = m[method].apply(m, [overlayIdFor(id, key)].concat(args));
        if (drawn) owned().add(key);
        return !!drawn;
      } catch (_e) {
        return false;
      }
    };
    return {
      colors: LINE_COLORS.map((c) => Object.assign({}, c)),
      drawLine(key, coords, opts) {
        return draw("setOverlay", key, [coords, opts]);
      },
      drawPolygon(key, coords, opts) {
        return draw("setPolygonOverlay", key, [coords, opts]);
      },
      drawCircle(key, center, radius, opts) {
        const ring = circleRing(center, radius, opts && opts.segments);
        if (!ring.length) return false;
        return draw("setPolygonOverlay", key, [ring, opts]);
      },
      drawText(key, point, text, opts) {
        return draw("setTextOverlay", key, [point, text, opts]);
      },
      remove(key) {
        const m = liveMap();
        if (!m || !validKey(key)) return false;
        owned().delete(key);
        try { return !!m.removeOverlay(overlayIdFor(id, key)); } catch (_e) { return false; }
      },
      setVisible(key, on) {
        const m = liveMap();
        if (!m || !validKey(key)) return false;
        try { return !!m.setOverlayVisible(overlayIdFor(id, key), on); } catch (_e) { return false; }
      },
      has(key) {
        const m = liveMap();
        if (!m || !validKey(key) || typeof m.hasOverlay !== "function") return false;
        try { return !!m.hasOverlay(overlayIdFor(id, key)); } catch (_e) { return false; }
      },
      fit(coords) {
        const m = liveMap();
        if (!m || typeof m.fitCoords !== "function") return false;
        try {
          // The Home page first: framing a map that is not on screen shows
          // the operator nothing.
          const nav = window.Corvus && window.Corvus.sidenav;
          if (nav && typeof nav.switchTo === "function"
              && typeof nav.current === "function" && nav.current() !== "home") {
            nav.switchTo("home");
          }
          return !!m.fitCoords(coords);
        } catch (_e) {
          return false;
        }
      },
    };
  }

  /** Currently-active plugin id, or null when the grid is showing. */
  function getActive() { return activeId; }

  /** Static placeholder shown when there is no card to show. `allInTabs` is
   *  the case where there are plugins, each in a tab of its own. */
  function renderEmpty(allInTabs) {
    rootEl.innerHTML =
      '<div class="future-icon" data-lucide="puzzle"></div>' +
      '<div class="future-title">Tools & Plugins</div>' +
      (allInTabs
        ? '<div class="future-desc">Every installed plugin has a tab of its own.</div>'
        : '<div class="future-desc">Extensions and tools plug in here.</div>');
    rootEl.classList.add("future-empty");
    Corvus.ui.refreshIcons();
  }

  /** Render the responsive card grid from the registry. A plugin with a tab
   *  of its own has no card: one way in, not two. */
  function renderGrid() {
    if (!rootEl) return;
    const plugins = list().filter((p) => !p.inTab);
    if (!plugins.length) { renderEmpty(registry.size > 0); return; }

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
    // A plugin with a tab opens there.
    if (tabbed.has(id)) {
      const panel = panelTabs();
      return !!(panel && panel.showTab(tabIdFor(id)));
    }
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
    refreshIcons();
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
   * Tell the operator something, in both the places it belongs: a toast over
   * whatever they are looking at, and a line on the notification board (the
   * corvus:notification window event the topbar listens for).
   *
   * The board alone was not enough, which is what this fixes. A plugin's
   * warning is nearly always the answer to something the operator just
   * pressed — a launch button whose terminal would not open — and the board
   * answers it with a badge in a corner that says nothing until it is opened.
   * Only a critical unfolds the popover by itself, so everything below that
   * landed silently. The toast is the answer arriving where the question was
   * asked; the board is where it stays afterwards.
   *
   * The plugin's name is the toast's title and the board line's prefix, so a
   * plugin writes only what it has to say and the operator still learns who
   * said it. `id` is null for the shared, unbound api (see apiFor).
   *
   * Best-effort and guarded on both halves, separately: a missing toast layer
   * must not cost the board line, and neither may throw into a plugin.
   *
   * @param {string|null} id  the plugin this was called on behalf of
   * @param {string} [level] "info" | "warning" | "critical"; default "info".
   * @param {*} message     Message text (coerced to string).
   */
  function pluginNotification(id, level, message) {
    const lv = (level === "warning" || level === "critical") ? level : "info";
    const spec = id ? registry.get(id) : null;
    const name = spec ? spec.name : "";
    const text = String(message);
    try {
      window.dispatchEvent(new CustomEvent("corvus:notification", {
        detail: { level: lv, message: name ? `${name}: ${text}` : text },
      }));
    } catch (_e) {
      // No topbar / no event target — never crash a plugin.
    }
    try {
      if (window.Corvus && Corvus.ui && typeof Corvus.ui.toast === "function") {
        // Undefined, not "": toast() falls back to the level's own title
        // ("Warning" / "Error") when a plugin has no name to put there.
        Corvus.ui.toast({ level: lv, title: name || undefined, message: text });
      }
    } catch (_e) {
      // A missing/broken toast layer must never crash a plugin either.
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
    if (!saved || typeof saved !== "object") return {};
    const out = Object.assign({}, saved);
    delete out[OPTIONS_KEY];
    return out;
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
    // The options are the operator's, set in Settings; a plugin writing its
    // own state can neither set nor clear them.
    const own = Object.assign({}, patch);
    delete own[OPTIONS_KEY];
    return pluginPostJson("/api/plugins/settings", { id, settings: own, replace: !!replace })
      .then((res) => {
        const options = savedOptions(id);
        const saved = (res && res.settings)
          || Object.assign(replace ? {} : getSettingsFor(id), own,
            Object.keys(options).length ? { [OPTIONS_KEY]: options } : {});
        settingsStore[id] = saved;
        return getSettingsFor(id);
      });
  }

  // ---- options the operator sets (Settings > Plugins, the gear) ----------

  /**
   * Keep the declared options that can be rendered and stored: a usable,
   * unique key that is not "tab", a known type, a label, and for a select at
   * least one choice. The default is coerced to the type, so a plugin that
   * declares {type: "number", default: "5"} still reads a number. Pure.
   * @param {*} raw  the spec's `options`
   * @returns {Array<Object>}
   */
  function cleanOptionSpecs(raw) {
    if (!Array.isArray(raw)) return [];
    const seen = new Set([TAB_OPTION]);
    const out = [];
    raw.forEach((o) => {
      if (!o || typeof o !== "object") return;
      const key = typeof o.key === "string" ? o.key : "";
      if (!OPTION_KEY_RE.test(key) || seen.has(key)) return;
      const type = OPTION_TYPES.has(o.type) ? o.type : null;
      if (!type) return;
      const opt = {
        key, type,
        label: typeof o.label === "string" && o.label.trim() ? o.label.trim() : key,
        hint: typeof o.hint === "string" ? o.hint.trim() : "",
      };
      if (type === "select") {
        opt.choices = (Array.isArray(o.choices) ? o.choices : []).map((c) => (
          c && typeof c === "object"
            ? { value: String(c.value), label: String(c.label == null ? c.value : c.label) }
            : { value: String(c), label: String(c) }
        ));
        if (!opt.choices.length) return;
      }
      if (type === "number") {
        ["min", "max", "step"].forEach((k) => {
          const v = Number(o[k]);
          if (o[k] != null && isFinite(v)) opt[k] = v;
        });
      }
      if (type === "text" && typeof o.placeholder === "string") opt.placeholder = o.placeholder;
      opt.default = coerceOption(opt, o.default);
      if (opt.default === undefined) opt.default = emptyOption(opt);
      seen.add(key);
      out.push(opt);
    });
    return out;
  }

  /** The value an option has when nothing at all was given. */
  function emptyOption(opt) {
    if (opt.type === "toggle") return false;
    if (opt.type === "select") return opt.choices[0].value;
    if (opt.type === "number") return opt.min != null ? opt.min : 0;
    return "";
  }

  /**
   * `value` as this option's type, or undefined when it cannot be one: a
   * select value that is not a choice, a number that is not a number. A
   * number is clamped to min and max. Pure.
   */
  function coerceOption(opt, value) {
    if (value === undefined || value === null) return undefined;
    if (opt.type === "toggle") return typeof value === "boolean" ? value : undefined;
    if (opt.type === "select") {
      const v = String(value);
      return opt.choices.some((c) => c.value === v) ? v : undefined;
    }
    if (opt.type === "number") {
      if (typeof value === "string" && !value.trim()) return undefined;
      let n = Number(value);
      if (!isFinite(n)) return undefined;
      if (opt.min != null) n = Math.max(n, opt.min);
      if (opt.max != null) n = Math.min(n, opt.max);
      return n;
    }
    return String(value).slice(0, 1000);
  }

  /** The operator's saved option values for a plugin, as stored. */
  function savedOptions(id) {
    const saved = settingsStore[id];
    const opts = saved && typeof saved === "object" ? saved[OPTIONS_KEY] : null;
    return opts && typeof opts === "object" && !Array.isArray(opts) ? opts : {};
  }

  /** Whether a plugin that can have a tab should have one now: the
   *  operator's own choice for it, else the old global switch. */
  function wantsTab(id) {
    const v = savedOptions(id)[TAB_OPTION];
    return typeof v === "boolean" ? v : tabsAllowed;
  }

  /** Whether a plugin has anything to set: its own options or a tab. */
  function hasOptions(id) {
    const spec = registry.get(id);
    return !!(spec && (spec.tab || spec.options.length));
  }

  /**
   * Every option of a plugin as the Settings dialog renders it: the schema
   * (the tab switch first, when the plugin can have a tab) and the current
   * values. Null for a plugin that is not registered.
   * @param {string} id
   * @returns {{schema: Array<Object>, values: Object}|null}
   */
  function optionsOf(id) {
    const spec = registry.get(id);
    if (!spec) return null;
    const schema = spec.options.map((o) => Object.assign({}, o));
    if (spec.tab) {
      schema.unshift({
        key: TAB_OPTION, type: "toggle", label: "Own tab", default: false,
        hint: "A tab of its own in the side panel, before PLUGINS, instead of " +
              "a card under PLUGINS.",
      });
    }
    return { schema, values: valuesFor(id) };
  }

  /** The current value of every option of a plugin. */
  function valuesFor(id) {
    const spec = registry.get(id);
    if (!spec) return {};
    const saved = savedOptions(id);
    const out = {};
    spec.options.forEach((o) => {
      const v = coerceOption(o, saved[o.key]);
      out[o.key] = v === undefined ? o.default : v;
    });
    if (spec.tab) out[TAB_OPTION] = wantsTab(id);
    return out;
  }

  /**
   * Set one option of a plugin and persist it. The change is in force at
   * once (a tab appears or goes, the plugin's optionsChanged runs) and is
   * taken back if the save fails, so the operator never looks at a value
   * that was not kept.
   * @param {string} id
   * @param {string} key
   * @param {*} value
   * @returns {Promise<Object>} every value after the change
   */
  function setOption(id, key, value) {
    const spec = registry.get(id);
    if (!spec) return Promise.reject(new Error("No such plugin."));
    const opt = key === TAB_OPTION && spec.tab
      ? { key, type: "toggle" }
      : spec.options.find((o) => o.key === key);
    if (!opt) return Promise.reject(new Error(`No option ${key}.`));
    const next = coerceOption(opt, value);
    if (next === undefined) return Promise.reject(new Error(`Not a valid value for ${opt.label || key}.`));

    const before = Object.assign({}, savedOptions(id));
    const after = Object.assign({}, before, { [key]: next });
    const apply = (opts) => {
      settingsStore[id] = Object.assign({}, settingsStore[id] || {}, { [OPTIONS_KEY]: opts });
      if (key === TAB_OPTION) syncTabs();
      else notifyOptions(id);
    };
    apply(after);
    return pluginPostJson("/api/plugins/settings", { id, settings: { [OPTIONS_KEY]: after } })
      .then((res) => {
        if (!res || res.ok === false || res.error) {
          throw new Error((res && res.error) || "Could not save the setting.");
        }
        return valuesFor(id);
      })
      .catch((err) => {
        apply(before);
        throw err;
      });
  }

  /** Tell a plugin its options changed. Guarded: a throwing hook costs the
   *  plugin, not the Settings page. */
  function notifyOptions(id) {
    const spec = registry.get(id);
    if (!spec || !spec.optionsChanged || !api) return;
    try {
      spec.optionsChanged(valuesFor(id), apiFor(id));
    } catch (err) {
      console.error("plugin optionsChanged failed:", id, err);
    }
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
    scoped.notification = (level, message) => pluginNotification(id, level, message);
    scoped.map = mapApiFor(id);
    scoped.getOptions = () => valuesFor(id);
    return scoped;
  }

  /**
   * Show the live terminal for an SSH session the plugin opened.
   *
   * Every session gets a window of its own (Corvus.termWindows: a native
   * window or a frame inside the app, as the operator has set it), so a
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
   * Set up the SSH connection a failed connect/run reply names, then resolve
   * whether the plugin should try again. See the api contract above.
   *
   * @param {Object|Error} reply the reply body, or the Error a request threw
   * @returns {Promise<boolean>} true once the connection was saved
   */
  function pluginSshSetup(reply) {
    try {
      const panel = window.Corvus && window.Corvus.panel;
      if (!panel || typeof panel.setupSSHConnection !== "function") return Promise.resolve(false);
      return Promise.resolve(panel.setupSSHConnection(reply)).catch(() => false);
    } catch (_e) {
      return Promise.resolve(false);
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
      // Overwritten per plugin by apiFor() so the message carries a name; the
      // unbound one still works, it just speaks anonymously.
      notification: (level, message) => pluginNotification(null, level, message),
      modes: pluginModes,
      terminal: pluginTerminal,
      postJson: pluginPostJson,
      sshSetup: pluginSshSetup,
      // Overwritten per plugin by apiFor(); present here so the shape is the
      // same object whether a plugin was handed the scoped api or reached the
      // shared one, and so a plugin calling them outside init() gets an empty
      // answer rather than a TypeError.
      getSettings: () => ({}),
      saveSettings: () => Promise.resolve({}),
      getOptions: () => ({}),
      // Overwritten per plugin by apiFor(), which binds the keys to the
      // plugin; the shared one draws nothing.
      map: {
        colors: LINE_COLORS.map((c) => Object.assign({}, c)),
        drawLine: () => false,
        drawPolygon: () => false,
        drawCircle: () => false,
        drawText: () => false,
        remove: () => false,
        setVisible: () => false,
        has: () => false,
        fit: () => false,
      },
    };
    // Plugins that registered before the api existed start now.
    registry.forEach((_spec, id) => runStart(id));
    syncTabs();
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
          const els = [];
          installedEls.set(p.id, els);
          (Array.isArray(p.styles) ? p.styles : []).forEach((href) => {
            els.push(appendStyle(assetUrl(p.id, href)));
          });
          // Sequential per plugin: a manifest listing several scripts means the
          // later ones may build on the earlier ones, and <script> tags appended
          // together carry no such guarantee.
          let chain = Promise.resolve();
          (Array.isArray(p.scripts) ? p.scripts : []).forEach((src) => {
            chain = chain.then(() => appendScript(assetUrl(p.id, src), els));
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

  /**
   * Unload every installed plugin and load the plugin folders again, so a
   * plugin that was dropped in, edited or deleted takes effect without a
   * restart. Each one goes the way unregister takes it (its view or tab
   * destroyed, its shapes off the map), its files leave the document, and
   * then discovery runs as at boot: start hooks run again, a saved shape is
   * redrawn. Never rejects.
   *
   * A plugin's own globals (Corvus.pluginX) are simply replaced when its
   * script runs again; that is why every plugin script is one IIFE.
   *
   * @returns {Promise<string[]>} the ids loaded
   */
  function reload() {
    Array.from(loadedInstalled).forEach((id) => {
      unregister(id);
      (installedEls.get(id) || []).forEach((el) => {
        try { if (el && el.parentNode) el.parentNode.removeChild(el); } catch (_e) { /* gone */ }
      });
    });
    loadedInstalled.clear();
    installedEls.clear();
    loadRound += 1;
    return loadInstalled().then((ids) => {
      if (rootEl && activeId === null) renderGrid();
      return ids;
    });
  }

  /** URL of one file inside a plugin's folder (each segment encoded, "/" kept).
   *  After a reload it carries the round, so nothing comes from the cache. */
  function assetUrl(id, rel) {
    const path = String(rel).split("/").map(encodeURIComponent).join("/");
    const fresh = loadRound ? `?r=${loadRound}` : "";
    return `/api/plugins/asset/${encodeURIComponent(id)}/${path}${fresh}`;
  }

  /** Append a stylesheet link. Fire-and-forget: a missing CSS is cosmetic. */
  function appendStyle(href) {
    const link = document.createElement("link");
    link.rel = "stylesheet";
    link.href = href;
    document.head.appendChild(link);
    return link;
  }

  /** Append a script tag; resolves on load, rejects on error. The element is
   *  pushed onto `els` so a reload can remove it. */
  function appendScript(src, els) {
    return new Promise((resolve, reject) => {
      const el = document.createElement("script");
      if (els) els.push(el);
      // Set, not left to the default: an inserted script is async unless told
      // otherwise, and runs whenever it happens to finish downloading. Ordered,
      // the plugins register in the order discovery listed them, which is
      // their manifest `order`, so the grid does not reshuffle between starts.
      el.async = false;
      el.src = src;
      el.onload = () => resolve();
      el.onerror = () => reject(new Error(`could not load ${src}`));
      document.head.appendChild(el);
    });
  }

  return {
    register, unregister, list, init, open, close, getActive, loadInstalled, reload,
    setTabsAllowed, tabsAllowed: () => tabsAllowed,
    hasOptions, options: optionsOf, setOption,
    LINE_COLORS, circleRing,
  };
})();
