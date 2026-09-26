"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.settingsTransfer: Export and Import on the Settings page.

  One file carries the whole station. The backend puts in its config, every
  plugin's config and the company logo (POST /api/settings/export, see
  corvus/settings_bundle.py); this page adds the interface state it keeps in
  localStorage, listed in BROWSER_KEYS: where the flight HUD and the virtual
  joystick sit, the RC transmitter bindings, the extra safety parameters,
  which link cards are folded.

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
    { key: "corvus.link.cards", section: "layout" },
    { key: "corvus.map.lastCamera", section: "layout" },
    { key: "corvus.analysis.sort.", section: "layout" },
    { key: "corvus.link.recent", section: "connections" },
    { key: "corvus.safety.extra", section: "vehicle" },
  ];
  const NOT_SETTINGS = ["corvus.console.history", "corvus.mission.last"];

  /* The parts an import is chosen by. The ids match SECTIONS in
     corvus/settings_bundle.py. */
  const SECTIONS = [
    { id: "interface", label: "Interface and controls",
      hint: "Theme, size, units, top bar, joystick and keys, RC transmitter, company logo." },
    { id: "layout", label: "Window layout",
      hint: "Where the flight HUD and the virtual joystick sit, and which cards are folded." },
    { id: "map", label: "Map", hint: "Map service, base layer, 3D mode and map keys." },
    { id: "connections", label: "Connections",
      hint: "Default link, auto connect, forwarding to a second station, SSH connections." },
    { id: "vehicle", label: "Vehicle",
      hint: "Battery estimate, Remote ID, RTK base, cameras, extra safety parameters." },
    { id: "folders", label: "Folders and port",
      hint: "Where Corvus keeps tiles, logs, parameters, firmware and missions. " +
            "Paths from another machine may not exist on this one." },
    { id: "plugins", label: "Plugins", hint: "The saved settings of every plugin." },
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

  /** Every setting this page keeps in *storage*, as {key: string}. */
  function collectBrowser(storage) {
    const out = {};
    try {
      storageKeys(storage).forEach((k) => {
        if (!sectionOfKey(k)) return;
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

  // ---- export ------------------------------------------------------------

  async function openExport() {
    let target = {};
    try {
      target = await Corvus.telemetry.requestJson("/api/settings/export/target");
    } catch (_e) { /* the placeholders stand in */ }

    const nameInput = Corvus.ui.input({
      id: "settingsExportName",
      ariaLabel: "File name",
      value: target.filename || "",
      placeholder: "corvus-settings.json",
      mono: true,
      autocomplete: false,
    });
    const dirInput = Corvus.ui.input({
      id: "settingsExportDir",
      ariaLabel: "Folder",
      value: target.dir || "",
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
    summary.textContent = "Every setting of this station in one file: the interface, " +
      "the window layout, the map, connections, vehicle settings, folders and plugins.";
    body.appendChild(summary);
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

    async function save() {
      msg.hide();
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
            browser: store ? collectBrowser(store) : {},
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

  function pickFile() {
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
        openImport(parseBundle(text), file.name || "the file");
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

  /** The parts that have nothing to import in *bundle*. Pure. */
  function emptySections(bundle) {
    const empty = [];
    const plugins = bundle && bundle.plugins;
    if (!plugins || typeof plugins !== "object" || !Object.keys(plugins).length) {
      empty.push("plugins");
    }
    const browser = (bundle && bundle.browser) || {};
    if (!Object.keys(browser).some((k) => sectionOfKey(k) === "layout")) empty.push("layout");
    return empty;
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

  function openImport(bundle, fileName) {
    const empty = new Set(emptySections(bundle));
    const toggles = new Map();

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

    const parts = document.createElement("div");
    parts.className = "settings-transfer-parts";
    body.appendChild(parts);

    SECTIONS.forEach((s) => {
      const none = empty.has(s.id);
      const t = Corvus.ui.toggle({
        value: !none,
        disabled: none,
        ariaLabel: s.label,
        onChange: () => refresh(),
      });
      toggles.set(s.id, t);
      parts.appendChild(Corvus.ui.field({
        label: s.label,
        control: t.el,
        className: "field-switch",
        hint: none ? "Nothing of this in the file." : s.hint,
      }));
    });

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
    // Wide, so each part's hint fits on one line and seven of them fit on a
    // laptop screen without the dialog scrolling.
    const dialog = Corvus.ui.modal({
      title: "Import settings",
      size: "lg",
      body,
      actions: [cancelBtn, importBtn],
    });
    dialog.open();

    function chosen() {
      return SECTIONS.map((s) => s.id).filter((id) => toggles.get(id).getValue());
    }
    function refresh() {
      importBtn.disabled = !chosen().length;
    }

    async function run() {
      msg.hide();
      const sections = chosen();
      if (!sections.length) return;
      if (armedNow()) {
        msg.show("Settings cannot be imported while the vehicle is armed.", "err");
        return;
      }
      Corvus.ui.setBusy(importBtn, true);
      let res;
      try {
        res = await Corvus.telemetry.requestJson("/api/settings/import", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ bundle, sections }),
        });
      } catch (err) {
        msg.show((err && err.message) || "Import failed", "err");
        Corvus.ui.setBusy(importBtn, false);
        return;
      }
      const store = localStore();
      if (store) applyBrowser(store, bundle.browser, sections);
      dialog.close();
      showResult(res || {});
    }
  }

  /** What the import did, and the reload that makes it take effect. */
  function showResult(res) {
    const lines = ["Settings imported."];
    const restart = (res.restart || []).map((k) => LABELS[k] || k);
    if (restart.length) {
      lines.push(`Restart Corvus GCS for the new ${restart.join(", ")}.`);
    }
    (res.warnings || []).forEach((w) => lines.push(w));
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
      title: "Import settings",
      size: "sm",
      body,
      actions: [reloadBtn],
      dismissable: false,
    });
    done.open();
  }

  // ---- Settings page -----------------------------------------------------

  /** The "Export and import" section of the Settings page. */
  function section() {
    const card = Corvus.ui.card({});
    card.classList.add("settings-transfer");
    const desc = document.createElement("div");
    desc.className = "page-card-desc";
    desc.textContent = "Save every setting of this station to one file, or load one " +
      "saved here or on another station. The file includes the window layout, " +
      "such as where the flight HUD sits.";
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
    BROWSER_KEYS,
    NOT_SETTINGS,
    SECTIONS,
    // Exported for the test suite.
    sectionOfKey,
    collectBrowser,
    applyBrowser,
    parseBundle,
    emptySections,
  };
})();
