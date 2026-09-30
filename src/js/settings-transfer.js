"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.settingsTransfer: Export and Import on the Settings page.

  One file carries the station, or the parts of it the operator picks. The
  backend puts in its config, the plugins' configs, the operator's own plugins
  and the company logo (POST /api/settings/export, see
  corvus/settings_bundle.py); this page adds the interface state it keeps in
  localStorage, listed in BROWSER_KEYS: where the flight HUD and the virtual
  joystick sit, the RC transmitter bindings, the extra safety parameters,
  which link cards are folded.

  Both dialogs pick by part, and under Plugins by plugin, with two halves each:
  its settings, and its files (the plugin itself). Only a plugin installed in
  the operator's folder has files to take; the ones that ship with Corvus GCS
  are on every station already.

  The backend writes the file, for the reason parameter exports are written
  there: QtWebEngine drops a browser download. Import reads the file here,
  shows what it holds, and posts it back with the parts the operator chose.
  The browser half is applied only once the backend has accepted the rest,
  and the page then reloads so every module starts from the new state rather
  than each one being taught to re-read its own.
*/
Corvus.settingsTransfer = (function () {
  /* Every localStorage key that is a setting, with the part of an import it
     belongs to. A key ending in "." is a prefix. Keys that are history rather
     than settings sit in NOT_SETTINGS, so the test suite can tell a key that
     was forgotten here from one that was left out on purpose. */
  const BROWSER_KEYS = [
    { key: "corvus.theme", section: "interface" },
    { key: "corvus.scale", section: "interface" },
    { key: "corvus.units", section: "interface" },
    { key: "corvus.topbarDots", section: "interface" },
    { key: "corvus.notificationMarks", section: "interface" },
    { key: "corvus.rc.transmitter.bindings.v2", section: "interface" },
    { key: "corvus.rc.transmitter.mode.v1", section: "interface" },
    { key: "corvus.hud", section: "layout" },
    { key: "corvus.joystick", section: "layout" },
    { key: "corvus.checklist", section: "layout" },
    { key: "corvus.link.cards", section: "layout" },
    { key: "corvus.map.lastCamera", section: "layout" },
    { key: "corvus.analysis.sort.", section: "layout" },
    { key: "corvus.link.recent", section: "connections" },
    { key: "corvus.safety.extra", section: "vehicle" },
  ];
  // corvus.frostedTerminals mirrors the config's ui.solid_terminals for
  // the windows of their own (js/term-window.js), rewritten on every start;
  // the setting itself travels with the config.
  // corvus.checklist.ticks is what was checked for the flight at hand, not
  // how the station is set up. corvus.map.track is the flown track itself.
  const NOT_SETTINGS = ["corvus.console.history", "corvus.mission.last", "corvus.frostedTerminals",
    "corvus.checklist.ticks", "corvus.map.track"];

  /* The parts an import is chosen by. The ids match SECTIONS in
     corvus/settings_bundle.py. */
  const SECTIONS = [
    { id: "interface", label: "Interface and controls",
      hint: "Theme, size, units, top bar, joystick and keys, RC transmitter, company logo, " +
            "flight review sensitivity." },
    { id: "layout", label: "Window layout",
      hint: "Where the flight HUD, the virtual joystick and the checklist sit, and which cards are folded." },
    { id: "map", label: "Map", hint: "Map service, base layer, 3D mode and map keys." },
    { id: "connections", label: "Connections",
      hint: "Default link, auto connect, forwarding to a second station, SSH connections." },
    { id: "vehicle", label: "Vehicle",
      hint: "Battery estimate, Remote ID, RTK base, cameras, extra safety parameters, preflight checklists." },
    { id: "folders", label: "Folders and port",
      hint: "Where Corvus keeps tiles, logs, parameters, firmware and missions. " +
            "Paths from another machine may not exist on this one." },
    { id: "plugins", label: "Plugins",
      hint: "Each plugin's saved settings, and the plugins you installed yourself." },
  ];

  const LABELS = {
    http_port: "HTTP port",
    tile_cache_dir: "tile cache folder",
    tlog_dir: "telemetry log folder",
    firmware_dir: "firmware folder",
  };

  // ---- browser half ------------------------------------------------------

  /** The part a localStorage key belongs to, or "" when it is not a setting. */
  function sectionOfKey(key) {
    const k = String(key || "");
    const hit = BROWSER_KEYS.find((e) =>
      e.key.endsWith(".") ? k.startsWith(e.key) : k === e.key);
    return hit ? hit.section : "";
  }

  function storageKeys(storage) {
    const keys = [];
    for (let i = 0; i < storage.length; i += 1) {
      const k = storage.key(i);
      if (k) keys.push(k);
    }
    return keys;
  }

  /** Every setting this page keeps in *storage*, as {key: string}; only those
   *  of *sections* when it is given. */
  function collectBrowser(storage, sections) {
    const out = {};
    const only = sections ? new Set(sections) : null;
    try {
      storageKeys(storage).forEach((k) => {
        const section = sectionOfKey(k);
        if (!section || (only && !only.has(section))) return;
        const v = storage.getItem(k);
        if (typeof v === "string") out[k] = v;
      });
    } catch (_e) { /* storage unavailable: the backend half still exports */ }
    return out;
  }

  /**
   * Make the chosen parts of *storage* what *browser* says.
   *
   * A key of a chosen part that the file does not have is removed, so it
   * falls back to its default the way it had on the station that wrote the
   * file. Parts not chosen are not touched.
   */
  function applyBrowser(storage, browser, sections) {
    const chosen = new Set(sections || []);
    const incoming = browser && typeof browser === "object" ? browser : {};
    try {
      storageKeys(storage).forEach((k) => {
        if (chosen.has(sectionOfKey(k)) && !Object.prototype.hasOwnProperty.call(incoming, k)) {
          storage.removeItem(k);
        }
      });
      Object.keys(incoming).forEach((k) => {
        if (chosen.has(sectionOfKey(k)) && typeof incoming[k] === "string") {
          storage.setItem(k, incoming[k]);
        }
      });
    } catch (_e) { /* storage unavailable: the backend half is already applied */ }
  }

  function localStore() {
    try { return window.localStorage; } catch (_e) { return null; }
  }

  // ---- what a file carries -----------------------------------------------

  /** The parts *bundle* carries. A format 1 file carries every part. Pure. */
  function sectionsInFile(bundle) {
    const all = SECTIONS.map((s) => s.id);
    if (!bundle || !(bundle.format >= 2) || !Array.isArray(bundle.sections)) return all;
    return all.filter((id) => bundle.sections.indexOf(id) !== -1);
  }

  /** The parts that have nothing to import in *bundle*. Pure. */
  function emptySections(bundle) {
    const present = new Set(sectionsInFile(bundle));
    const empty = SECTIONS.map((s) => s.id).filter((id) => !present.has(id));
    const plugins = (bundle && bundle.plugins) || {};
    const files = (bundle && bundle.plugin_files) || {};
    if (present.has("plugins") && !Object.keys(plugins).length && !Object.keys(files).length) {
      empty.push("plugins");
    }
    const browser = (bundle && bundle.browser) || {};
    if (present.has("layout") && !Object.keys(browser).some((k) => sectionOfKey(k) === "layout")) {
      empty.push("layout");
    }
    return empty;
  }

  function isObject(v) { return !!v && typeof v === "object" && !Array.isArray(v); }

  /** The name in a plugin's base64 plugin.json, or "" when there is none. Pure. */
  function manifestName(files) {
    const raw = isObject(files) ? files["plugin.json"] : null;
    if (typeof raw !== "string") return "";
    try {
      const bytes = Uint8Array.from(atob(raw), (c) => c.charCodeAt(0));
      const manifest = JSON.parse(new TextDecoder().decode(bytes));
      return (manifest && typeof manifest.name === "string" && manifest.name.trim()) || "";
    } catch (_e) {
      return "";
    }
  }

  function installedById(listing) {
    const out = {};
    ((listing && listing.plugins) || []).forEach((p) => { if (p && p.id) out[p.id] = p; });
    return out;
  }

  /**
   * The plugins an export can take, from GET /api/plugins: one row per
   * installed plugin with something to take, then one per saved config whose
   * plugin is not installed. `settings` and `files` say which halves exist;
   * `why` is the word shown in place of a half that does not. Pure.
   */
  function exportPlugins(listing) {
    const saved = (listing && isObject(listing.settings)) ? listing.settings : {};
    const rows = ((listing && listing.plugins) || []).filter((p) => p && p.id).map((p) => {
      const user = p.source === "user";
      return {
        id: p.id,
        name: p.name || p.id,
        note: user ? "Installed in your plugin folder" : "Ships with Corvus GCS",
        settings: Object.prototype.hasOwnProperty.call(saved, p.id),
        files: user,
        why: { settings: "No settings", files: "Built in" },
      };
    }).filter((r) => r.settings || r.files);
    const seen = new Set(rows.map((r) => r.id));
    Object.keys(saved).sort().forEach((id) => {
      if (seen.has(id)) return;
      rows.push({
        id, name: id, note: "Not installed here", settings: true, files: false,
        why: { settings: "", files: "Not installed" },
      });
    });
    return rows;
  }

  /** The plugins in *bundle*, described against what *listing* has installed. Pure. */
  function importPlugins(bundle, listing) {
    const saved = isObject(bundle && bundle.plugins) ? bundle.plugins : {};
    const files = isObject(bundle && bundle.plugin_files) ? bundle.plugin_files : {};
    const here = installedById(listing);
    const ids = Array.from(new Set(Object.keys(files).concat(Object.keys(saved)))).sort();
    return ids.map((id) => {
      const mine = here[id];
      const hasFiles = Object.prototype.hasOwnProperty.call(files, id);
      let note;
      if (hasFiles) {
        if (!mine) note = "New on this station";
        else if (mine.source === "user") note = "Replaces the copy installed here";
        else note = "Takes the place of the copy that ships with Corvus GCS";
      } else if (!mine) {
        note = "Not installed here. The settings wait for it.";
      } else {
        note = mine.source === "user" ? "Installed here" : "Ships with Corvus GCS";
      }
      return {
        id,
        name: manifestName(files[id]) || (mine && mine.name) || id,
        note,
        settings: Object.prototype.hasOwnProperty.call(saved, id),
        files: hasFiles,
        why: { settings: "Not in file", files: "Not in file" },
      };
    });
  }

  // ---- the part picker both dialogs share -------------------------------

  /**
   * One switch per part, and under Plugins a row per plugin with a switch
   * for its settings and one for its files. A half the plugin does not have
   * shows why in place of its switch, rather than a switch that cannot move.
   *
   *   spec.unavailable  {part id: hint} for parts that cannot be chosen
   *   spec.plugins      rows from exportPlugins() or importPlugins()
   *   spec.onChange     called after any switch moves
   *
   * Returns {el, head, chosen(), pluginChoice(), anyFiles()}.
   */
  function partsPicker(spec) {
    const unavailable = spec.unavailable || {};
    const plugins = spec.plugins || [];
    const toggles = new Map();
    const halves = [];
    let pluginList = null;

    const el = document.createElement("div");
    el.className = "settings-transfer-parts";

    function showPluginList() {
      if (!pluginList) return;
      const t = toggles.get("plugins");
      pluginList.hidden = !(t && t.getValue());
    }

    function changed() {
      showPluginList();
      if (typeof spec.onChange === "function") spec.onChange();
    }

    function setAll(on) {
      toggles.forEach((t, id) => { if (!unavailable[id]) t.setValue(on); });
      halves.forEach((h) => h.toggle.setValue(on));
      changed();
    }

    SECTIONS.forEach((s) => {
      const off = unavailable[s.id];
      const t = Corvus.ui.toggle({
        value: !off,
        disabled: !!off,
        ariaLabel: s.label,
        onChange: () => changed(),
      });
      toggles.set(s.id, t);
      el.appendChild(Corvus.ui.field({
        label: s.label,
        control: t.el,
        className: "field-switch",
        hint: off || s.hint,
      }));
      if (s.id === "plugins" && !off && plugins.length) {
        pluginList = document.createElement("div");
        pluginList.className = "settings-transfer-plugins";
        plugins.forEach((p) => pluginList.appendChild(pluginRow(p)));
        el.appendChild(pluginList);
      }
    });

    function pluginRow(p) {
      const row = document.createElement("div");
      row.className = "settings-transfer-plugin";
      const text = document.createElement("div");
      text.className = "settings-transfer-plugin-text";
      const name = document.createElement("span");
      name.className = "settings-transfer-plugin-name";
      name.textContent = p.name;
      text.appendChild(name);
      const note = document.createElement("span");
      note.className = "field-hint";
      note.textContent = p.note || "";
      text.appendChild(note);
      row.appendChild(text);

      [["settings", "Settings"], ["files", "Files"]].forEach(([kind, caption]) => {
        if (!p[kind]) {
          const none = document.createElement("span");
          none.className = "settings-transfer-opt is-unavailable";
          none.textContent = (p.why && p.why[kind]) || "";
          row.appendChild(none);
          return;
        }
        const t = Corvus.ui.toggle({
          value: true,
          ariaLabel: `${p.name}: ${caption}`,
          onChange: () => changed(),
        });
        // A label, so the caption works the switch as well.
        const opt = document.createElement("label");
        opt.className = "settings-transfer-opt";
        const cap = document.createElement("span");
        cap.textContent = caption;
        opt.appendChild(cap);
        opt.appendChild(t.el);
        row.appendChild(opt);
        halves.push({ id: p.id, kind, toggle: t });
      });
      return row;
    }

    // "All" and "None" above the list, beside its caption.
    const head = document.createElement("div");
    head.className = "settings-transfer-head";
    head.appendChild(Corvus.ui.label(spec.caption || "Parts"));
    head.appendChild(Corvus.ui.actions([
      Corvus.ui.button({ variant: "ghost", size: "sm", label: "All", onClick: () => setAll(true) }),
      Corvus.ui.button({ variant: "ghost", size: "sm", label: "None", onClick: () => setAll(false) }),
    ]));

    function chosen() {
      return SECTIONS.map((s) => s.id).filter((id) => !unavailable[id] && toggles.get(id).getValue());
    }

    function pluginChoice() {
      const on = toggles.get("plugins");
      const out = { settings: [], files: [] };
      if (!on || unavailable.plugins || !on.getValue()) return out;
      halves.forEach((h) => { if (h.toggle.getValue()) out[h.kind].push(h.id); });
      return out;
    }

    // Not changed(): the dialog building this has no buttons yet to refresh.
    showPluginList();
    return {
      el, head, chosen, pluginChoice, setAll,
      anyFiles: () => pluginChoice().files.length > 0,
    };
  }

  // ---- export ------------------------------------------------------------

  async function openExport() {
    const [target, listing] = await Promise.all([
      Corvus.telemetry.requestJson("/api/settings/export/target").catch(() => ({})),
      Corvus.telemetry.requestJson("/api/plugins").catch(() => ({})),
    ]);
    const plugins = exportPlugins(listing || {});

    const nameInput = Corvus.ui.input({
      id: "settingsExportName",
      ariaLabel: "File name",
      value: (target && target.filename) || "",
      placeholder: "corvus-settings.json",
      mono: true,
      autocomplete: false,
    });
    const dirInput = Corvus.ui.input({
      id: "settingsExportDir",
      ariaLabel: "Folder",
      value: (target && target.dir) || "",
      placeholder: "~/Downloads",
      mono: true,
      autocomplete: false,
      spellcheck: false,
    });
    const secrets = Corvus.ui.toggle({
      ariaLabel: "Include passwords and map keys",
    });

    const body = document.createDocumentFragment();
    const summary = document.createElement("div");
    summary.className = "page-card-desc";
    summary.textContent = "Choose what goes into the file. Everything is switched on, " +
      "which is the whole station: a backup, or the setup for the next laptop.";
    body.appendChild(summary);

    const picker = partsPicker({
      caption: "What goes into the file",
      unavailable: plugins.length ? {} : { plugins: "No plugin has anything to export." },
      plugins,
      onChange: () => refresh(),
    });
    body.appendChild(picker.head);
    body.appendChild(picker.el);

    body.appendChild(Corvus.ui.field({ label: "File name", control: nameInput }));
    body.appendChild(Corvus.ui.field({
      label: "Folder",
      control: dirInput,
      hint: "On the machine running Corvus. Created if it does not exist.",
    }));
    body.appendChild(Corvus.ui.field({
      label: "Include passwords and map keys",
      control: secrets.el,
      className: "field-switch",
      hint: "SSH and camera passwords, the NTRIP password and map service keys. " +
            "Leave off for a file you pass on.",
    }));
    const msg = Corvus.ui.message();
    body.appendChild(msg.el);

    const cancelBtn = Corvus.ui.button({
      variant: "secondary", label: "Cancel", onClick: () => dialog.close(),
    });
    const saveBtn = Corvus.ui.button({
      variant: "primary", icon: "download", label: "Save", onClick: save,
    });
    // As wide as the import dialog, and wide enough for the whole default
    // file name, which carries the machine's name and the time.
    const dialog = Corvus.ui.modal({
      title: "Export settings",
      size: "lg",
      body,
      actions: [cancelBtn, saveBtn],
    });
    dialog.open();
    refresh();

    function refresh() {
      saveBtn.disabled = !picker.chosen().length;
    }

    async function save() {
      msg.hide();
      const sections = picker.chosen();
      if (!sections.length) return;
      const choice = picker.pluginChoice();
      Corvus.ui.setBusy(saveBtn, true);
      try {
        const store = localStore();
        const res = await Corvus.telemetry.requestJson("/api/settings/export", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            filename: nameInput.value.trim(),
            dir: dirInput.value.trim(),
            include_secrets: secrets.getValue(),
            browser: store ? collectBrowser(store, sections) : {},
            sections,
            plugin_settings: choice.settings,
            plugin_files: choice.files,
          }),
        });
        dialog.close();
        notify("info", `Settings exported to ${res.path}`);
      } catch (err) {
        msg.show((err && err.message) || "Export failed", "err");
      } finally {
        Corvus.ui.setBusy(saveBtn, false);
      }
    }
  }

  // ---- import ------------------------------------------------------------

  /**
   * Ask for a settings file and open the import dialog for it.
   * *opts* is handed to openImport (see there).
   */
  function pickFile(opts) {
    const input = document.createElement("input");
    input.type = "file";
    input.setAttribute("accept", ".json,application/json");
    input.hidden = true;
    document.body.appendChild(input);
    input.addEventListener("change", () => {
      input.remove();
      const file = input.files && input.files[0];
      if (!file) return;
      readFileText(file).then((text) => {
        openImport(parseBundle(text), file.name || "the file", opts);
      }).catch((err) => {
        notify("critical", (err && err.message) || "Could not read the settings file.");
      });
    });
    input.click();
  }

  function readFileText(file) {
    if (typeof file.text === "function") return file.text();
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result || ""));
      reader.onerror = () => reject(new Error("Could not read the settings file."));
      reader.readAsText(file);
    });
  }

  /** The parsed file, or an Error an operator can act on. Pure. */
  function parseBundle(text) {
    let data;
    try { data = JSON.parse(String(text || "")); } catch (_e) {
      throw new Error("This is not a Corvus GCS settings file.");
    }
    if (!data || typeof data !== "object" || data.kind !== "corvus-settings" ||
        !data.config || typeof data.config !== "object") {
      throw new Error("This is not a Corvus GCS settings file.");
    }
    return data;
  }

  function describeSource(bundle, fileName) {
    const parts = [];
    if (bundle.version) parts.push(`Corvus GCS ${bundle.version}`);
    if (bundle.exported_at) {
      const when = new Date(bundle.exported_at);
      if (!isNaN(when.getTime())) parts.push(`exported ${when.toLocaleString()}`);
    }
    return parts.length ? `${fileName}, from ${parts.join(", ")}.` : `${fileName}.`;
  }

  /**
   * The import dialog for a parsed *bundle*.
   *
   * opts.onImported(res) runs once the backend has taken the file, before the
   * page offers to reload; the first start setup uses it to close itself.
   */
  async function openImport(bundle, fileName, opts) {
    const o = opts || {};
    let listing = {};
    try {
      listing = await Corvus.telemetry.requestJson("/api/plugins");
    } catch (_e) { /* the rows then say less about what is installed here */ }
    const plugins = importPlugins(bundle, listing);
    const present = new Set(sectionsInFile(bundle));
    const unavailable = {};
    emptySections(bundle).forEach((id) => {
      unavailable[id] = present.has(id) ? "Nothing of this in the file." : "Not in this file.";
    });

    const body = document.createDocumentFragment();
    // What the file is and whether it carries passwords, together above the
    // parts: both describe the file, not the choice.
    const about = document.createElement("div");
    about.className = "settings-transfer-file";
    const source = document.createElement("div");
    source.className = "page-card-desc";
    source.textContent = describeSource(bundle, fileName);
    about.appendChild(source);
    const note = document.createElement("div");
    note.className = "page-card-desc";
    note.textContent = bundle.secrets
      ? "The file carries passwords and map keys, and they replace the ones stored here."
      : "The file carries no passwords or map keys. The ones stored here are kept " +
        "wherever the connection, camera or map service is still the same.";
    about.appendChild(note);
    body.appendChild(about);

    const picker = partsPicker({
      caption: "What to take from the file",
      unavailable,
      plugins,
      onChange: () => refresh(),
    });
    body.appendChild(picker.head);
    body.appendChild(picker.el);

    const code = Corvus.ui.message();
    body.appendChild(code.el);
    const warn = Corvus.ui.message();
    warn.show("The parts switched on replace the settings of this station. " +
              "The interface reloads afterwards.", "warn");
    body.appendChild(warn.el);
    const msg = Corvus.ui.message();
    body.appendChild(msg.el);

    const cancelBtn = Corvus.ui.button({
      variant: "secondary", label: "Cancel", onClick: () => dialog.close(),
    });
    const importBtn = Corvus.ui.button({
      variant: "primary", icon: "upload", label: "Import", onClick: run,
    });
    const dialog = Corvus.ui.modal({
      title: "Import settings",
      size: "lg",
      body,
      actions: [cancelBtn, importBtn],
    });
    dialog.open();
    refresh();

    function refresh() {
      importBtn.disabled = !picker.chosen().length;
      if (picker.anyFiles()) {
        code.show("Plugin files are code. It runs inside Corvus GCS and can reach the " +
                  "vehicle like the interface can. Install plugins only from a file you trust.",
                  "warn");
      } else {
        code.hide();
      }
    }

    async function run() {
      msg.hide();
      const sections = picker.chosen();
      if (!sections.length) return;
      if (armedNow()) {
        msg.show("Settings cannot be imported while the vehicle is armed.", "err");
        return;
      }
      const choice = picker.pluginChoice();
      Corvus.ui.setBusy(importBtn, true);
      let res;
      try {
        res = await Corvus.telemetry.requestJson("/api/settings/import", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            bundle,
            sections,
            plugin_settings: choice.settings,
            plugin_files: choice.files,
          }),
        });
      } catch (err) {
        msg.show((err && err.message) || "Import failed", "err");
        Corvus.ui.setBusy(importBtn, false);
        return;
      }
      const store = localStore();
      if (store) applyBrowser(store, bundle.browser, sections);
      dialog.close();
      if (typeof o.onImported === "function") {
        try { o.onImported(res || {}); } catch (_e) { /* the result still shows */ }
      }
      showResult(res || {});
    }
  }

  /** What the import did, and the reload that makes it take effect. */
  function showResult(res) {
    const lines = ["Settings imported."];
    const installed = Number(res.plugins_installed) || 0;
    if (installed) {
      lines.push(installed === 1 ? "1 plugin installed." : `${installed} plugins installed.`);
    }
    const restart = (res.restart || []).map((k) => LABELS[k] || k);
    if (restart.length) {
      lines.push(`Restart Corvus GCS for the new ${restart.join(", ")}.`);
    }
    (res.warnings || []).forEach((w) => lines.push(w));
    showNotice("Import settings", lines);
  }

  /** A dialog of *lines* whose one way out is the reload that applies them. */
  function showNotice(title, lines) {
    const body = document.createDocumentFragment();
    lines.forEach((text) => {
      const p = document.createElement("div");
      p.className = "page-card-desc";
      p.textContent = text;
      body.appendChild(p);
    });
    const reloadBtn = Corvus.ui.button({
      variant: "primary", icon: "refresh-cw", label: "Reload interface",
      onClick: () => window.location.reload(),
    });
    const done = Corvus.ui.modal({
      title,
      size: "sm",
      body,
      actions: [reloadBtn],
      dismissable: false,
    });
    done.open();
  }

  // ---- Factory reset -----------------------------------------------------

  /** Remove every key of this page from *storage*: settings and history alike. */
  function clearBrowser(storage) {
    try {
      storageKeys(storage).forEach((k) => {
        if (k.startsWith("corvus.")) storage.removeItem(k);
      });
    } catch (_e) { /* storage unavailable: the backend half is already reset */ }
  }

  /** Ask, then put the station back to factory settings and reload into the setup. */
  function openReset() {
    const body = document.createDocumentFragment();
    const desc = document.createElement("div");
    desc.className = "page-card-desc";
    desc.textContent = "Every setting of this station goes back to its default: the " +
      "interface, the layout, the map, the connections, the vehicle settings, the " +
      "folders, the company logo and the settings of every plugin. Plugins you " +
      "installed yourself stay installed. Missions, logs, parameter files and map " +
      "tiles are not touched.";
    body.appendChild(desc);
    const warn = Corvus.ui.message();
    warn.show("This cannot be undone. Export your settings first if you may want " +
              "them back. The interface then reloads into the first start setup.", "warn");
    body.appendChild(warn.el);
    const msg = Corvus.ui.message();
    body.appendChild(msg.el);

    const cancelBtn = Corvus.ui.button({
      variant: "secondary", label: "Cancel", onClick: () => dialog.close(),
    });
    const resetBtn = Corvus.ui.button({
      variant: "danger", icon: "rotate-ccw", label: "Reset", onClick: run,
    });
    const dialog = Corvus.ui.modal({
      title: "Reset to factory settings",
      size: "sm",
      body,
      actions: [cancelBtn, resetBtn],
    });
    dialog.open();

    async function run() {
      msg.hide();
      if (armedNow()) {
        msg.show("Settings cannot be reset while the vehicle is armed.", "err");
        return;
      }
      Corvus.ui.setBusy(resetBtn, true);
      let res;
      try {
        res = await Corvus.telemetry.requestJson("/api/settings/reset", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: "{}",
        });
      } catch (err) {
        msg.show((err && err.message) || "Reset failed", "err");
        Corvus.ui.setBusy(resetBtn, false);
        return;
      }
      const store = localStore();
      if (store) clearBrowser(store);
      dialog.close();
      const lines = [];
      const restart = ((res && res.restart) || []).map((k) => LABELS[k] || k);
      if (restart.length) lines.push(`Restart Corvus GCS for the default ${restart.join(", ")}.`);
      ((res && res.warnings) || []).forEach((w) => lines.push(w));
      if (!lines.length) {
        window.location.reload();
        return;
      }
      showNotice("Reset to factory settings", ["Settings reset."].concat(lines));
    }
  }

  // ---- Settings page -----------------------------------------------------

  /** The "Export and import" section of the Settings page. */
  function section() {
    const card = Corvus.ui.card({});
    card.classList.add("settings-transfer");
    const desc = document.createElement("div");
    desc.className = "page-card-desc";
    desc.textContent = "Save the settings of this station to one file, or load one " +
      "saved here or on another station. You choose the parts, down to each " +
      "plugin, which can travel with its settings or on its own.";
    card.appendChild(desc);
    card.appendChild(Corvus.ui.actions([
      Corvus.ui.button({
        variant: "secondary", icon: "download", label: "Export settings",
        onClick: () => { openExport(); },
      }),
      Corvus.ui.button({
        variant: "secondary", icon: "upload", label: "Import settings",
        onClick: () => pickFile(),
      }),
      Corvus.ui.button({
        variant: "danger", icon: "rotate-ccw", label: "Reset to factory settings",
        className: "settings-transfer-reset",
        onClick: () => openReset(),
      }),
    ]));
    return Corvus.ui.section({ title: "Export and import", body: card });
  }

  function notify(level, message) {
    window.dispatchEvent(new CustomEvent("corvus:notification", { detail: { level, message } }));
  }

  function armedNow() {
    const s = Corvus.telemetry && Corvus.telemetry.getState && Corvus.telemetry.getState();
    return !!(s && s.armed);
  }

  return {
    section,
    openExport,
    openImport,
    pickFile,
    openReset,
    BROWSER_KEYS,
    NOT_SETTINGS,
    SECTIONS,
    // Exported for the test suite.
    sectionOfKey,
    collectBrowser,
    applyBrowser,
    clearBrowser,
    parseBundle,
    sectionsInFile,
    emptySections,
    manifestName,
    exportPlugins,
    importPlugins,
  };
})();
