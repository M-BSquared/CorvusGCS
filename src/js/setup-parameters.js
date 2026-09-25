"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupParameters — the Parameters sub-page of the Setup page.
 *
 * Lazy, on-demand parameter editor. The operator clicks "Download Parameters"
 * to fetch the full set; until then Corvus only streams what is needed to fly
 * (lean). A set already downloaded on this link is shown straight away. Once
 * the editor is open, the vehicle's defaults (and on PX4 its descriptions,
 * units and ranges) are read in the background, the way QGroundControl reads
 * them, and each row shows its default and whether it differs from it.
 *
 * Editing: a changed value is an unsaved draft until it is applied, per row
 * (Apply, or Enter) or all at once from the bar under the list. Drafts survive
 * filtering and redraws. Every write is POST /api/params/set, refused while
 * armed.
 *
 * Files: Export writes QGroundControl's .params (PX4), Mission Planner's
 * .param (ArduPilot) or Corvus JSON; Import reads all three, shows what would
 * change against the loaded set, and writes only that.
 *
 * Backend contract:
 *   POST /api/params/download          start full param download (one-shot)
 *   GET  /api/params                   {state,count,received,complete,params}
 *                                     LEAN: params=[] until complete is true
 *   "params" topic on /api/events      progress {state,count,received}
 *   POST /api/params/set {name,value}  write one param (refused while armed)
 *   POST /api/params/upload {params}   write a list on a worker thread
 *   GET  /api/params/upload/result     that upload's tally
 *   POST /api/params/metadata          start reading the defaults
 *   GET  /api/params/metadata          {state,error,received,size,params?}
 *   GET  /api/params/export/target     {dir,filename,format,formats}
 *   POST /api/params/export            {dir,filename,format,params}
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the view lifecycle and calls destroy() on back / left-nav re-entry so
 * the params stream, the ~1 s status polls and the telemetry subscription are
 * all released.
 */
Corvus.setupParameters = (function () {
  const S = Corvus.setupShared;
  // Rows drawn at once; a 1400 parameter set never builds 1400 DOM rows.
  const CAP = 200;
  const POLL_MS = 1000;
  // MAV_PARAM_TYPE -> the whole numbers it holds. REAL32 (9) is not here.
  const INT_RANGES = {
    1: [0, 255], 2: [-128, 127], 3: [0, 65535], 4: [-32768, 32767],
    5: [0, 4294967295], 6: [-2147483648, 2147483647],
  };
  const TYPE_NAMES = {
    1: "UINT8", 2: "INT8", 3: "UINT16", 4: "INT16", 5: "UINT32", 6: "INT32", 9: "REAL32",
  };
  const FORMATS = [
    { id: "qgc", label: "QGroundControl (.params)", suffix: ".params", kind: "QGroundControl" },
    { id: "mission-planner", label: "Mission Planner (.param)", suffix: ".param", kind: "Mission Planner" },
    { id: "json", label: "Corvus (.json)", suffix: ".json", kind: "Corvus" },
  ];
  const MODES = [
    { id: "all", label: "All", title: "Every parameter" },
    { id: "modified", label: "Modified", title: "Parameters whose value differs from the firmware default" },
    { id: "pending", label: "Unsaved", title: "Values you changed here but have not written yet" },
  ];

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page");
    // The back button only asks the orchestrator to navigate back; the
    // orchestrator's teardown() is the single place that calls destroy().
    page.appendChild(S.backButton(navigateBack));
    page.appendChild(S.pageHeader("Parameters",
      "Every parameter with its default. Change values, load and save parameter files"));

    // Actions bar (Export / Import + status). Lives in `page`, sibling of the
    // `section`, so it stays visible across every phase — the editor's `card`
    // wipes its innerHTML per phase and would otherwise drop these buttons.
    const actions = S.el("div", "params-actions");
    const exportBtn = makeActionButton("download", "Export");
    const importBtn = makeActionButton("upload", "Import");
    exportBtn.title = "Save the parameters to a file";
    importBtn.title = "Write a parameter file (.params, .param or .json) to the vehicle";
    // SYS_AUTOSTART, SENS_EN_*, SER_* and the like are read at boot. After a
    // reboot the rows on screen are the old boot's, so the page goes back to
    // the download prompt rather than keep showing them.
    const rebootBtn = S.rebootButton({ size: "sm", mount: page, onRebooted: () => {
      if (state.destroyed) return;
      stopMetadataPoll(state);
      state.params = [];
      state.byName = new Map();
      state.meta = null;
      state.metaStatus = { state: "idle", error: "" };
      state.rendered = false;
      card._editorView = null;
      exportBtn.disabled = true;
      renderDownloadPrompt(card, state);
    } });
    const actionsStatus = S.el("div", "params-actions-status");
    actions.appendChild(exportBtn);
    actions.appendChild(importBtn);
    actions.appendChild(rebootBtn);
    actions.appendChild(actionsStatus);
    page.appendChild(actions);
    exportBtn.disabled = true;   // enabled once params are loaded
    const cur = Corvus.telemetry && Corvus.telemetry.getState();
    importBtn.disabled = !!(cur && cur.armed);
    rebootBtn.disabled = !!(cur && cur.armed);

    const section = S.el("div", "page-section");
    const card = S.el("div", "page-card params-card");
    section.appendChild(card);
    page.appendChild(section);
    container.appendChild(page);

    // Everything the module-level helpers need, in one place so teardown can
    // close it all.
    const state = {
      params: [],          // the editable list (only once complete), sorted by name
      byName: new Map(),   // name -> the same objects
      meta: null,          // name -> {default, short_desc, units, ...} once read
      metaStatus: { state: "idle", error: "" },
      metaTimer: null,     // the ~1 s GET /api/params/metadata poll while it loads
      pending: new Map(),  // name -> the text typed but not written yet
      rowStatus: new Map(), // name -> {cls, text}; survives a redraw
      search: "",
      mode: "all",
      phase: "prompt",     // prompt | downloading | editor
      eventSource: null,   // the ONE params topic subscription (see watchProgress)
      progressSubs: null,  // who is watching it; the stream closes with the last
      pollTimer: null,     // the ~1 s GET /api/params fallback
      unsub: null,         // telemetry subscription (armed gating)
      rendered: false,
      exportBtn,           // enabled once the full param set is loaded
      importBtn,           // armed-gated; disabled during upload
      actionsStatus,
      downloadUnwatch: null,     // release the download view's watch
      uploadUnwatch: null,       // release an import's watch
      uploadPoll: null,          // the ~1 s upload result poll behind it
      uploading: false,
      uploadFinishing: false,
      card,                // the editor redraws into it after an import
      page,
      destroyed: false,
    };

    // Phase 1: show the Download button (do NOT auto-download — lean + on-demand).
    renderDownloadPrompt(card, state);
    adoptLoadedSet(state);

    // Subscribe to telemetry so the editor's armed banner gates live. The
    // same callback also gates the Import button (uploads are refused while
    // armed).
    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      state.unsub = Corvus.telemetry.subscribe((s) => {
        if (card._editorView) applyArmedToEditor(card._editorView, !!(s && s.armed));
        if (!state.uploading) importBtn.disabled = !!(s && s.armed);
        rebootBtn.disabled = !!(s && s.armed);
      });
    }

    exportBtn.addEventListener("click", () => exportParams(state));
    importBtn.addEventListener("click", () => importParams(state));

    function destroy() {
      state.destroyed = true;
      if (state.unsub) { try { state.unsub(); } catch (_e) {} state.unsub = null; }
      state.downloadUnwatch = null;
      state.uploadUnwatch = null;
      closeUploadSse(state);
      closeProgressStream(state);
      stopMetadataPoll(state);
      if (state.pollTimer) { try { window.clearInterval(state.pollTimer); } catch (_e) {} state.pollTimer = null; }
    }

    return destroy;
  }

  /** Build an Export/Import action button (mirrors renderDownloadPrompt's btn). */
  function makeActionButton(iconName, label) {
    return Corvus.ui.button({
      variant: "primary", size: "sm", icon: iconName, label,
    });
  }

  function notify(level, message) {
    window.dispatchEvent(new CustomEvent("corvus:notification", { detail: { level, message } }));
  }

  function plural(n, word) {
    return `${n} ${word}${n === 1 ? "" : "s"}`;
  }

  function armedNow() {
    const s = Corvus.telemetry && Corvus.telemetry.getState && Corvus.telemetry.getState();
    return !!(s && s.armed);
  }

  // ---------------------------------------------------------------------------
  // Values
  // ---------------------------------------------------------------------------

  function isIntType(type) {
    return Object.prototype.hasOwnProperty.call(INT_RANGES, Number(type));
  }

  /**
   * The text a value is shown as. A float parameter travels as a float32, so
   * 0.1 arrives as 0.10000000149011612; the shortest decimal that is the same
   * float32 is exact and is what a person would have typed.
   */
  function formatValue(value, type) {
    const n = Number(value);
    if (!isFinite(n)) return String(value);
    if (isIntType(type)) return String(Math.round(n));
    const target = Math.fround(n);
    for (let digits = 1; digits <= 9; digits += 1) {
      const text = String(Number(n.toPrecision(digits)));
      if (Math.fround(Number(text)) === target) return text;
    }
    return String(n);
  }

  /** Same value as the vehicle would hold it: whole for integers, float32 otherwise. */
  function valuesEqual(a, b, type) {
    const x = Number(a);
    const y = Number(b);
    if (!isFinite(x) || !isFinite(y)) return false;
    if (isIntType(type)) return Math.round(x) === Math.round(y);
    return Math.fround(x) === Math.fround(y);
  }

  /** Read what the operator typed: {ok, value, error} plus a range warning. */
  function parseDraft(text, param, meta) {
    const t = String(text == null ? "" : text).trim();
    if (!t || !isFinite(Number(t))) return { ok: false, error: "not a number" };
    const value = Number(t);
    const range = INT_RANGES[Number(param.type)];
    if (range) {
      if (!Number.isInteger(value)) return { ok: false, error: "whole numbers only" };
      if (value < range[0] || value > range[1]) {
        return { ok: false, error: `${range[0]} to ${range[1]} only` };
      }
    }
    let warn = "";
    if (meta && ((typeof meta.min === "number" && value < meta.min) ||
                 (typeof meta.max === "number" && value > meta.max))) {
      const lo = typeof meta.min === "number" ? formatValue(meta.min, param.type) : "";
      const hi = typeof meta.max === "number" ? formatValue(meta.max, param.type) : "";
      warn = lo && hi ? `outside ${lo} to ${hi}` : (lo ? `below ${lo}` : `above ${hi}`);
    }
    return { ok: true, value, warn };
  }

  function metaFor(state, name) {
    return (state.meta && state.meta[name]) || null;
  }

  function hasDefault(meta) {
    return !!meta && typeof meta.default === "number";
  }

  function isModified(state, param) {
    const meta = metaFor(state, param.name);
    return hasDefault(meta) && !valuesEqual(param.value, meta.default, param.type);
  }

  function byName(a, b) {
    return (a.name || "").localeCompare(b.name || "");
  }

  // ---------------------------------------------------------------------------
  // Parameter files
  // ---------------------------------------------------------------------------

  /**
   * Parse a parameter file: Corvus JSON, QGroundControl's tab separated
   * .params, or Mission Planner's NAME,VALUE .param. Throws an Error whose
   * message is shown to the operator.
   *
   * A QGroundControl file can hold several components; only the autopilot's
   * rows (component 1, or the file's only component) are taken, and the rest
   * are counted as skipped.
   */
  function parseParamFile(text, fileName) {
    const body = String(text == null ? "" : text).replace(/^﻿/, "").trim();
    const isJson = /\.json$/i.test(fileName || "") || body[0] === "{";
    if (isJson) {
      let doc;
      try { doc = JSON.parse(body); } catch (_e) { throw new Error("File is not valid JSON"); }
      if (!doc || !Array.isArray(doc.params) || !doc.params.length) {
        throw new Error("Parameter file has no parameters");
      }
      const params = [];
      for (const p of doc.params) {
        if (!p || typeof p.name !== "string" || !p.name.length ||
            typeof p.value !== "number" || !isFinite(p.value)) {
          throw new Error("Parameter file has an invalid entry");
        }
        params.push({ name: p.name, value: p.value });
      }
      return { format: "json", params, skipped: 0 };
    }
    if (!body) throw new Error("Parameter file is empty");
    const rows = [];
    let qgc = false;
    body.split(/\r?\n/).forEach((raw, index) => {
      const line = raw.trim();
      if (!line || line[0] === "#") return;
      const cols = line.split("\t").map((c) => c.trim());
      let row;
      if (cols.length >= 5 && /^\d+$/.test(cols[0]) && /^\d+$/.test(cols[1])) {
        qgc = true;
        row = { component: Number(cols[1]), name: cols[2], text: cols[3] };
      } else {
        const parts = line.split(/[\s,]+/).filter(Boolean);
        if (parts.length < 2) throw new Error(`Line ${index + 1} is not a parameter`);
        row = { component: null, name: parts[0], text: parts[1] };
      }
      if (!/^[A-Za-z0-9_]{1,16}$/.test(row.name)) {
        throw new Error(`Line ${index + 1}: "${row.name.slice(0, 20)}" is not a parameter name`);
      }
      const value = Number(row.text);
      if (row.text === "" || !isFinite(value)) {
        throw new Error(`Line ${index + 1}: "${row.text.slice(0, 20)}" is not a number`);
      }
      rows.push({ component: row.component, name: row.name, value });
    });
    if (!rows.length) throw new Error("Parameter file has no parameters");
    let keep = rows;
    if (qgc) {
      const components = rows.map((r) => r.component).filter((c) => c !== null);
      const wanted = components.indexOf(1) >= 0 ? 1 : components[0];
      keep = rows.filter((r) => r.component === null || r.component === wanted);
    }
    const merged = new Map();
    keep.forEach((r) => merged.set(r.name, r.value));
    const params = Array.from(merged, ([name, value]) => ({ name, value }));
    return { format: qgc ? "qgc" : "mission-planner", params, skipped: rows.length - keep.length };
  }

  function formatFromName(fileName) {
    const lower = String(fileName || "").toLowerCase();
    const hit = FORMATS.find((f) => lower.endsWith(f.suffix));
    return hit ? hit.id : "";
  }

  function withSuffix(fileName, formatId) {
    const fmt = FORMATS.find((f) => f.id === formatId);
    const stem = String(fileName || "").replace(/\.(params|param|parm|json)$/i, "");
    return stem ? stem + (fmt ? fmt.suffix : "") : "";
  }

  // ---------------------------------------------------------------------------
  // Export
  // ---------------------------------------------------------------------------

  /**
   * Export the loaded parameter set to a file the operator names.
   *
   * The file is written by the BACKEND, not pulled as a browser download.
   * The desktop build runs this UI inside QtWebEngine, which drops an
   * `<a download>` unless the host app implements a download handler — so a
   * browser download silently produced no file there at all. Writing it
   * server-side behaves identically in the desktop app and in a browser, lands
   * it in a folder the operator chose, and can say exactly where it went.
   */
  async function exportParams(state) {
    if (!state.params || !state.params.length) return;

    let target = {};
    try {
      target = await Corvus.telemetry.requestJson("/api/params/export/target");
    } catch (_e) { /* offline/unsupported: the dialog falls back to placeholders */ }

    let format = target.format || formatFromName(target.filename) || "qgc";
    const nameInput = Corvus.ui.input({
      id: "paramsExportName",
      ariaLabel: "File name",
      value: target.filename || "",
      placeholder: "corvus-params" + (FORMATS.find((f) => f.id === format) || FORMATS[0]).suffix,
      mono: true,
      autocomplete: false,
    });
    const dirInput = Corvus.ui.input({
      id: "paramsExportDir",
      ariaLabel: "Folder",
      value: target.dir || "",
      placeholder: "~/.corvus/params",
      mono: true,
      autocomplete: false,
      spellcheck: false,
    });
    const formatSelect = Corvus.ui.select({
      className: "params-export-format",
      ariaLabel: "Format",
      options: FORMATS.map((f) => ({ value: f.id, label: f.label })),
      value: format,
      onChange: (value) => {
        format = value;
        nameInput.value = withSuffix(nameInput.value, value);
        describe();
      },
    });
    const modifiedCount = state.meta ? state.params.filter((p) => isModified(state, p)).length : 0;
    const onlyModified = Corvus.ui.toggle({
      ariaLabel: "Only parameters that differ from their default",
      disabled: !state.meta,
      onChange: () => describe(),
    });

    const body = document.createDocumentFragment();
    const summary = S.el("div", "params-desc", "");
    body.appendChild(summary);
    body.appendChild(Corvus.ui.field({ label: "File name", control: nameInput }));
    body.appendChild(Corvus.ui.field({ label: "Format", control: formatSelect }));
    body.appendChild(Corvus.ui.field({
      label: "Folder",
      control: dirInput,
      hint: "On the machine running Corvus. Created if it does not exist.",
    }));
    body.appendChild(Corvus.ui.field({
      label: "Only parameters changed from their default",
      control: onlyModified.el,
      className: "field-switch",
      hint: state.meta
        ? `${modifiedCount} of ${state.params.length} parameters differ from their default.`
        : "Needs the defaults from the vehicle, which are not available.",
    }));
    const msg = Corvus.ui.message();
    body.appendChild(msg.el);

    function chosen() {
      return onlyModified.getValue()
        ? state.params.filter((p) => isModified(state, p))
        : state.params;
    }
    function describe() {
      const fmt = FORMATS.find((f) => f.id === format) || FORMATS[0];
      summary.textContent = `${plural(chosen().length, "parameter")} will be written as a ` +
        `${fmt.kind} parameter file. Import reads it back.`;
    }
    describe();

    const cancelBtn = Corvus.ui.button({
      variant: "secondary", label: "Cancel", onClick: () => dialog.close(),
    });
    const saveBtn = Corvus.ui.button({
      variant: "primary", icon: "download", label: "Save", onClick: save,
    });

    const dialog = Corvus.ui.modal({
      title: "Export parameters",
      size: "md",
      body,
      actions: [cancelBtn, saveBtn],
    });
    dialog.open();

    async function save() {
      msg.hide();
      const params = chosen();
      if (!params.length) {
        msg.show("Nothing to export: every parameter is at its default.", "err");
        return;
      }
      Corvus.ui.setBusy(saveBtn, true);
      try {
        const res = await Corvus.telemetry.requestJson("/api/params/export", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            filename: nameInput.value.trim(),
            dir: dirInput.value.trim(),
            format,
            params: params.map((p) => ({ name: p.name, value: p.value, type: p.type })),
          }),
        });
        dialog.close();
        // The saved path is the useful part of the confirmation — it is what
        // the operator needs to find the file afterwards.
        setStatus(state, "ok", `Saved ${res.param_count} parameters to ${res.path}`);
        notify("info", `Parameters exported to ${res.path}`);
      } catch (err) {
        msg.show((err && err.message) || "Export failed", "err");
      } finally {
        Corvus.ui.setBusy(saveBtn, false);
      }
    }
  }

  // ---------------------------------------------------------------------------
  // Import
  // ---------------------------------------------------------------------------

  /**
   * Import: pick a file, parse it, show what it would change, then POST the
   * changed values to the drone and follow the upload progress.
   */
  function importParams(state) {
    if (state.uploading) return;
    const input = document.createElement("input");
    input.type = "file";
    input.setAttribute("accept", ".params,.param,.parm,.txt,.json,application/json,text/plain");
    document.body.appendChild(input);
    input.addEventListener("change", () => {
      input.remove();
      onImportFile(state, input).catch((err) => {
        const msg = (err && err.message) || "Import failed";
        setStatus(state, "err", msg);
        notify("critical", msg);
        state.importBtn.disabled = armedNow();
      });
    });
    input.click();
  }

  async function onImportFile(state, input) {
    const file = input.files && input.files[0];
    if (!file) return;
    const text = await readFileText(file);
    const parsed = parseParamFile(text, file.name);
    openImportPreview(state, parsed, file.name || "the file");
  }

  /** Read a File as text, preferring file.text() with a FileReader fallback. */
  function readFileText(file) {
    if (typeof file.text === "function") return file.text();
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result || ""));
      reader.onerror = () => reject(new Error("Could not read file"));
      reader.readAsText(file);
    });
  }

  /**
   * What the file would do to the vehicle, before anything is written.
   *
   * Against a loaded set only the values that differ are written: a file of
   * 1400 parameters that changes three of them is three writes, not 1400, and
   * a name this firmware does not have is never sent at all. Without a loaded
   * set there is nothing to compare, so every value is written and the dialog
   * says so.
   */
  function openImportPreview(state, parsed, fileName) {
    const loaded = state.phase === "editor" && state.params.length > 0;
    const total = parsed.params.length;
    const changes = [];
    const unknown = [];
    let same = 0;
    if (loaded) {
      parsed.params.forEach((p) => {
        const cur = state.byName.get(p.name);
        if (!cur) unknown.push(p.name);
        else if (valuesEqual(cur.value, p.value, cur.type)) same += 1;
        else changes.push({ name: p.name, from: cur.value, to: p.value, type: cur.type });
      });
    }
    const toWrite = loaded
      ? changes.map((c) => ({ name: c.name, value: c.to }))
      : parsed.params.map((p) => ({ name: p.name, value: p.value }));

    const body = document.createDocumentFragment();
    let text;
    if (loaded) {
      text = `${changes.length} of ${plural(total, "parameter")} in ${fileName} ` +
        `${changes.length === 1 ? "differs" : "differ"} from the vehicle.`;
      if (same) text += ` ${same} already match.`;
      if (unknown.length) {
        text += ` ${plural(unknown.length, "parameter")} ${unknown.length === 1 ? "is" : "are"} ` +
          "not on this vehicle and skipped.";
      }
    } else {
      text = `${plural(total, "parameter")} in ${fileName}. Download the vehicle's parameters ` +
        "first to see which of them change anything; without them every value is written.";
    }
    if (parsed.skipped) {
      text += ` ${plural(parsed.skipped, "row")} for other components ` +
        `${parsed.skipped === 1 ? "is" : "are"} skipped.`;
    }
    body.appendChild(S.el("div", "params-desc", text));

    if (changes.length) {
      const list = S.el("div", "params-import-list");
      changes.slice(0, 300).forEach((c) => {
        const row = S.el("div", "params-import-row");
        row.appendChild(S.el("span", "params-import-name", c.name));
        row.appendChild(S.el("span", "params-import-from", formatValue(c.from, c.type)));
        row.appendChild(S.el("span", "params-import-arrow", "→"));
        row.appendChild(S.el("span", "params-import-to", formatValue(c.to, c.type)));
        list.appendChild(row);
      });
      if (changes.length > 300) {
        list.appendChild(S.el("div", "params-more", `and ${changes.length - 300} more`));
      }
      body.appendChild(list);
      if (changes.some((c) => { const m = metaFor(state, c.name); return m && m.reboot_required; })) {
        body.appendChild(S.el("div", "params-import-note",
          "Some of these take effect only after the autopilot reboots."));
      }
    }
    if (unknown.length) {
      const shown = unknown.slice(0, 8).join(", ");
      body.appendChild(S.el("div", "params-import-note",
        `Not on this vehicle: ${shown}${unknown.length > 8 ? ` and ${unknown.length - 8} more` : ""}.`));
    }

    const cancelBtn = Corvus.ui.button({
      variant: "secondary", label: toWrite.length ? "Cancel" : "Close", onClick: () => dialog.close(),
    });
    const writeBtn = Corvus.ui.button({
      variant: "primary", icon: "upload",
      label: toWrite.length ? `Write ${plural(toWrite.length, "parameter")}` : "Nothing to write",
      disabled: !toWrite.length || armedNow(),
      onClick: () => {
        dialog.close();
        startUpload(state, toWrite);
      },
    });
    const dialog = Corvus.ui.modal({
      title: "Import parameters",
      size: "md",
      body,
      actions: [cancelBtn, writeBtn],
    });
    dialog.open();
  }

  /**
   * Write an imported list, following it to its end.
   *
   * The watch starts BEFORE the request: a short upload is over before the
   * POST's answer is back, and an event nobody is listening for is gone, so
   * watching afterwards left the status at "Uploading" for good. The ~1 s
   * poll of the result covers the one case the watch cannot, a stream that
   * only connects once the upload has already finished.
   */
  async function startUpload(state, params) {
    state.uploading = true;
    state.importBtn.disabled = true;
    const total = params.length;
    setStatus(state, "pending", `Uploading 0 / ${total} parameters…`);
    state.uploadUnwatch = watchProgress(state, (d) => {
      if (d.state === "upload_complete" || d.state === "interrupted") uploadDone(state);
      else if (d.state === "uploading") {
        setStatus(state, "pending", `Uploading ${d.received || 0} / ${d.count || total} parameters…`);
      }
    });
    try {
      await Corvus.telemetry.postAction("/api/params/upload", { params });
    } catch (err) {
      closeUploadSse(state);
      state.uploading = false;
      const msg = (err && err.message) || "Upload failed";
      setStatus(state, "err", msg);
      notify("critical", msg);
      state.importBtn.disabled = armedNow();
      return;
    }
    if (!state.uploading || state.destroyed) return;
    state.uploadPoll = window.setInterval(() => {
      Corvus.telemetry.requestJson("/api/params/upload/result").then((r) => {
        if (r && (r.state === "upload_complete" || r.state === "interrupted")) uploadDone(state);
      }).catch(() => { /* the next tick asks again */ });
    }, POLL_MS);
  }

  /** The upload ended, whichever of the watch and the poll noticed first. */
  function uploadDone(state) {
    if (!state.uploading || state.uploadFinishing) return;
    state.uploadFinishing = true;
    closeUploadSse(state);
    finishUpload(state).finally(() => { state.uploadFinishing = false; });
  }

  /**
   * Parameter progress for this card, shared by everything that watches it.
   *
   * This used to be two EventSource objects on the SAME url — one for the
   * download view, one for an import — and that is a connection the rest of
   * the app cannot have. A browser caps concurrent HTTP/1.1 requests per
   * origin at six, and this application could hold five SSE streams open at
   * once, leaving the map fetching its tiles one at a time at exactly the
   * moment (pre-flight setup, with a download running) the operator is
   * busiest. The refcount below fixed the double connection; js/events.js
   * then put this topic on the shared stream, which fixed the other four.
   *
   * Returns an unsubscribe. The topic stops being delivered here when the
   * last watcher lets go.
   */
  function watchProgress(state, handler) {
    if (!state.progressSubs) state.progressSubs = new Set();
    state.progressSubs.add(handler);
    if (!state.eventSource) {
      // The shared /api/events stream (js/events.js), not an EventSource of
      // this card's own: the connection budget is six per origin and this
      // endpoint is one of four that now travel together. Both consumers here
      // have their own fallback — the download has a ~1 s poll, the upload has
      // GET /api/params/upload/result — so a dropped stream is not an error
      // either of them has to act on.
      state.eventSource = Corvus.events.subscribe("params", (data) => {
        // Guarded per subscriber: one throwing watcher must not stop the
        // other from seeing the event that ends its wait.
        state.progressSubs.forEach((fn) => {
          try { fn(data); } catch (err) {
            console.error("params progress watcher failed:", err);
          }
        });
      });
    }
    return function () {
      if (!state.progressSubs) return;
      state.progressSubs.delete(handler);
      if (!state.progressSubs.size) closeProgressStream(state);
    };
  }

  function closeProgressStream(state) {
    if (state.eventSource) {
      // An unsubscribe from the shared stream, not a socket close: the topic
      // stays on the connection (see js/events.js on why topics are sticky),
      // but nothing is delivered here any more.
      try { state.eventSource(); } catch (_e) {}
      state.eventSource = null;
    }
    if (state.progressSubs) state.progressSubs.clear();
  }

  function closeUploadSse(state) {
    if (state.uploadUnwatch) { state.uploadUnwatch(); state.uploadUnwatch = null; }
    if (state.uploadPoll) { try { window.clearInterval(state.uploadPoll); } catch (_e) {} state.uploadPoll = null; }
  }

  /** Fetch the final upload result and summarise written/failed counts. */
  async function finishUpload(state) {
    let r;
    try {
      r = await Corvus.telemetry.requestJson("/api/params/upload/result");
    } catch (err) {
      const msg = (err && err.message) || "Upload result unavailable";
      setStatus(state, "err", msg);
      notify("critical", msg);
      state.uploading = false;
      state.importBtn.disabled = armedNow();
      return;
    }
    const written = (r && r.written) || 0;
    const failed = (r && r.failed) || 0;
    const errors = (r && Array.isArray(r.errors)) ? r.errors : [];
    if (failed === 0 && !(r && r.state === "interrupted")) {
      setStatus(state, "ok", `Uploaded ${plural(written, "parameter")}`);
      notify("info", `Uploaded ${plural(written, "parameter")}`);
    } else {
      const cls = written > 0 ? "ok" : "err";
      let text = `Uploaded ${written}, ${failed} failed`;
      if (r && r.state === "interrupted") text += ", the link dropped before the end";
      if (errors.length) text += `: ${errors.slice(0, 3).map((e) => e.name).join(", ")}`;
      setStatus(state, cls, text);
      notify("critical", text);
    }
    state.uploading = false;
    state.importBtn.disabled = armedNow();
    if (state.rendered) refreshEditor(state);
  }

  /**
   * Redraw an open editor from the vehicle's set after an import.
   *
   * The confirmed writes went into the backend's copy, so the rows on screen
   * are the values from before the upload, and an Export now would write
   * those. The search, the filter and any unsaved drafts survive the redraw.
   */
  function refreshEditor(state) {
    Corvus.telemetry.requestJson("/api/params").then((d) => {
      if (state.destroyed || !d || !d.complete || !Array.isArray(d.params)) return;
      setParams(state, d.params);
      renderEditor(state.card, state);
    }).catch(() => { /* the rows stay as they were; Download reads them again */ });
  }

  /** Apply the status class + text to the actions status line. */
  function setStatus(state, cls, text) {
    if (!state.actionsStatus) return;
    state.actionsStatus.className = "params-actions-status" + (cls ? " " + cls : "");
    state.actionsStatus.textContent = text || "";
  }

  // ---------------------------------------------------------------------------
  // Download
  // ---------------------------------------------------------------------------

  /**
   * Show a set this link already downloaded, or follow a download that is
   * already running. Neither starts anything: the download stays the
   * operator's decision (lean), but a set the backend holds is no reason to
   * make them ask for it twice.
   */
  function adoptLoadedSet(state) {
    if (!Corvus.telemetry || typeof Corvus.telemetry.requestJson !== "function") return;
    Corvus.telemetry.requestJson("/api/params").then((d) => {
      if (state.destroyed || state.phase !== "prompt" || !d) return;
      if (d.complete && Array.isArray(d.params) && d.params.length) showEditor(state, d.params);
      else if (d.state === "downloading") openProgressView(state.card, state);
    }).catch(() => { /* the prompt stays; Download works as usual */ });
  }

  /** Phase 1: the Download-on-demand prompt, or why the last one fell short. */
  function renderDownloadPrompt(card, state, incomplete) {
    state.phase = "prompt";
    card.innerHTML = "";
    card.appendChild(S.sectionTitle(incomplete ? "Download incomplete" : "Parameter Set"));

    const desc = S.el("div", "params-desc");
    desc.textContent = incomplete
      ? `The vehicle sent ${incomplete.received || 0} of ${incomplete.count || 0} parameters ` +
        "before the link lost the rest. Try again. A cable is more reliable than a radio for this."
      : "Read every parameter from the vehicle to edit them. Until then, Corvus streams only " +
        "what it needs to fly. The editor opens once the whole set has arrived, and the " +
        "defaults are read from the vehicle right after.";
    card.appendChild(desc);

    const btn = Corvus.ui.button({
      variant: "primary",
      className: "params-download-btn",
      icon: "download",
      label: incomplete ? "Download again" : "Download Parameters",
    });
    card.appendChild(btn);

    const status = S.el("div", "params-status");
    status.hidden = true;
    card.appendChild(status);

    btn.addEventListener("click", () => startDownload(card, state, btn, status));
  }

  /** Phase 2: kick off the download and open the progress view. */
  function startDownload(card, state, btn, status) {
    btn.disabled = true;
    state.phase = "downloading";
    if (status) {
      status.hidden = false;
      status.className = "params-status";
      status.textContent = "Requesting parameter list…";
    }

    Corvus.telemetry.postAction("/api/params/download", {})
      .then(() => {
        if (state.destroyed) return;
        state.rendered = false;
        openProgressView(card, state);
      })
      .catch((err) => {
        // 503 (disconnected) or other error — keep the button usable.
        btn.disabled = false;
        const msg = (err && err.message) || "Could not start download";
        if (status) {
          status.className = "params-status err";
          status.textContent = msg;
        } else {
          setStatus(state, "err", msg);
        }
        if (state.phase === "downloading") state.phase = state.rendered ? "editor" : "prompt";
      });
  }

  function stopDownloadWatch(state) {
    if (state.downloadUnwatch) { state.downloadUnwatch(); state.downloadUnwatch = null; }
    if (state.pollTimer) { try { window.clearInterval(state.pollTimer); } catch (_e) {} state.pollTimer = null; }
  }

  /**
   * Progress view: SSE primary + ~1 s GET poll fallback (a config-status poll,
   * not telemetry). The operator MUST wait until `complete` before the editor
   * appears (per the lean contract: params is [] until complete).
   */
  function openProgressView(card, state) {
    stopDownloadWatch(state);
    state.phase = "downloading";
    card._editorView = null;
    card.innerHTML = "";
    card.appendChild(S.sectionTitle("Downloading Parameters"));

    const barHost = S.el("div", "progress-host");
    const bar = S.el("div", "progress-bar");
    const fill = S.el("div", "progress-bar-fill");
    bar.appendChild(fill);
    barHost.appendChild(bar);

    const label = S.el("div", "progress-label", "Waiting for all parameters…");
    barHost.appendChild(label);
    card.appendChild(barHost);

    const status = S.el("div", "params-status");
    card.appendChild(status);

    function setProgress(received, count) {
      const pct = (count && count > 0) ? Math.min(100, Math.round((received / count) * 100)) : 0;
      fill.style.width = pct + "%";
      if (count && count > 0) label.textContent = `${received} / ${count} parameters · ${pct}%`;
      else label.textContent = "Requesting parameter list…";
    }

    function onStatus(d) {
      if (d.complete || d.state === "complete") finishDownload(card, state);
      else if (d.state === "incomplete") downloadIncomplete(card, state, d);
    }

    // Primary: the shared SSE progress stream.
    state.downloadUnwatch = watchProgress(state, (d) => {
      setProgress(d.received || 0, d.count || 0);
      onStatus(d);
    });

    // Safety-net poll of the config status (NOT telemetry). ~1 s cadence.
    state.pollTimer = window.setInterval(() => {
      Corvus.telemetry.requestJson("/api/params").then((d) => {
        setProgress(d.received || 0, d.count || 0);
        onStatus(d);
      }).catch(() => { /* transient; the SSE/poll retry on the next tick */ });
    }, POLL_MS);
  }

  /** The watchdog gave up on a lossy link: say how far it got and offer a retry. */
  function downloadIncomplete(card, state, d) {
    stopDownloadWatch(state);
    if (state.rendered || state.destroyed) return;
    renderDownloadPrompt(card, state, d);
  }

  /** Phase 3: download complete -> fetch the full set and render the editor. */
  function finishDownload(card, state) {
    stopDownloadWatch(state);
    if (state.rendered) return;   // guard against SSE + poll both firing

    Corvus.telemetry.requestJson("/api/params").then((d) => {
      if (state.destroyed || state.rendered) return;
      if (!d || !d.complete || !Array.isArray(d.params) || !d.params.length) {
        // Not actually complete yet (race) — re-open the progress view.
        openProgressView(card, state);
        return;
      }
      showEditor(state, d.params);
    }).catch((err) => {
      card.innerHTML = "";
      card.appendChild(S.sectionTitle("Parameters"));
      const status = S.el("div", "params-status err");
      status.textContent = (err && err.message) || "Could not load parameters";
      card.appendChild(status);
    });
  }

  function setParams(state, params) {
    state.params = params.slice().sort(byName);
    state.byName = new Map(state.params.map((p) => [p.name, p]));
    // A draft that now equals the vehicle's value is no longer a change.
    state.pending.forEach((text, name) => {
      const p = state.byName.get(name);
      const v = p ? parseDraft(text, p, metaFor(state, name)) : null;
      if (!p || (v.ok && valuesEqual(v.value, p.value, p.type))) state.pending.delete(name);
    });
  }

  function showEditor(state, params) {
    setParams(state, params);
    state.rendered = true;
    state.phase = "editor";
    if (state.exportBtn) state.exportBtn.disabled = false;
    renderEditor(state.card, state);
    if (!state.meta) loadMetadata(state);
  }

  // ---------------------------------------------------------------------------
  // Defaults (parameter metadata)
  // ---------------------------------------------------------------------------

  /**
   * Ask the backend to read the defaults from the vehicle, then poll the
   * status until they arrive or it gives up. A config-status poll like the
   * download's fallback, not telemetry, and it stops at the first answer
   * that is not "loading".
   */
  function loadMetadata(state) {
    stopMetadataPoll(state);
    if (!Corvus.telemetry || typeof Corvus.telemetry.postAction !== "function") return;
    state.metaStatus = { state: "loading", error: "", received: 0, size: 0 };
    updateMetaView(state);
    Corvus.telemetry.postAction("/api/params/metadata", {})
      .then((st) => onMetaStatus(state, st))
      .catch((err) => onMetaStatus(state, {
        state: "unavailable", error: (err && err.message) || "no answer",
      }));
  }

  function pollMetadata(state) {
    Corvus.telemetry.requestJson("/api/params/metadata")
      .then((st) => onMetaStatus(state, st))
      .catch(() => { /* transient; the next tick asks again */ });
  }

  function onMetaStatus(state, st) {
    if (state.destroyed || !st) return;
    if (st.state === "ready" && st.params && typeof st.params === "object") {
      stopMetadataPoll(state);
      state.meta = st.params;
      state.metaStatus = { state: "ready", error: "", source: st.source || "" };
      if (state.card._editorView) renderRows(state, state.card._editorView, true);
      updateMetaView(state);
      return;
    }
    if (st.state === "ready" || st.state === "loading") {
      // The start answer carries no metadata; the status poll brings it.
      state.metaStatus = st;
      if (!state.metaTimer) {
        state.metaTimer = window.setInterval(() => pollMetadata(state), POLL_MS);
      }
      if (st.state === "ready") pollMetadata(state);
      updateMetaView(state);
      return;
    }
    stopMetadataPoll(state);
    state.metaStatus = { state: st.error ? "unavailable" : "idle", error: st.error || "" };
    updateMetaView(state);
  }

  function stopMetadataPoll(state) {
    if (state.metaTimer) {
      try { window.clearInterval(state.metaTimer); } catch (_e) {}
      state.metaTimer = null;
    }
  }

  /** The note above the list and the Modified filter follow the metadata state. */
  function updateMetaView(state) {
    const view = state.card && state.card._editorView;
    if (!view) return;
    const st = state.metaStatus || {};
    const note = view.metaNote;
    note.innerHTML = "";
    note.className = "params-meta-note";
    if (st.state === "loading") {
      const pct = st.size > 0 ? ` ${Math.min(100, Math.round((st.received / st.size) * 100))}%` : "";
      note.textContent = `Reading the defaults from the vehicle…${pct}`;
      note.hidden = false;
    } else if (st.state === "unavailable") {
      note.classList.add("warn");
      note.appendChild(S.el("span", "", `Defaults are not available: ${st.error || "no answer"}.`));
      const retry = Corvus.ui.button({
        variant: "secondary", size: "sm", label: "Try again", className: "params-meta-retry",
        onClick: () => loadMetadata(state),
      });
      note.appendChild(retry);
      note.hidden = false;
    } else if (st.state === "ready" && st.source === "cache") {
      // The operator opted into this in Settings, and the warning there is
      // the reason it is said here too: a copy is matched by checksum only.
      note.textContent = "Defaults from the copy kept on this computer, matched to the "
        + "vehicle by checksum. Settings, Files turns this off.";
      note.hidden = false;
    } else {
      note.hidden = true;
    }
    updateCounts(state, view);
  }

  // ---------------------------------------------------------------------------
  // Editor
  // ---------------------------------------------------------------------------

  /** Phase 4: the searchable, filterable editor with drafts and defaults. */
  function renderEditor(card, state) {
    card.innerHTML = "";
    const head = S.el("div", "params-head");
    head.appendChild(S.sectionTitle(`Parameters (${state.params.length})`));
    const reloadBtn = Corvus.ui.button({
      variant: "secondary", size: "sm", icon: "refresh-cw", label: "Reload",
      className: "params-reload", title: "Read every parameter from the vehicle again",
    });
    reloadBtn.addEventListener("click", () => {
      if (state.uploading) return;
      startDownload(card, state, reloadBtn, null);
    });
    head.appendChild(reloadBtn);
    card.appendChild(head);

    // Armed banner (read-only inputs while armed).
    const banner = S.el("div", "params-banner");
    banner.hidden = true;
    banner.textContent = "Parameter editing disabled while armed";
    card.appendChild(banner);

    const metaNote = S.el("div", "params-meta-note");
    metaNote.hidden = true;
    card.appendChild(metaNote);

    // Toolbar: search, the All / Modified / Unsaved filter, the count.
    const toolbar = S.el("div", "params-toolbar");
    const search = document.createElement("input");
    search.type = "text";
    search.className = "field-input params-search";
    search.placeholder = "Search names and descriptions…";
    search.setAttribute("aria-label", "Search parameters");
    search.value = state.search;
    toolbar.appendChild(search);

    const seg = S.el("div", "params-filter");
    seg.setAttribute("role", "tablist");
    seg.setAttribute("aria-label", "Show");
    const segButtons = {};
    MODES.forEach((m) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "params-filter-opt";
      b.dataset.mode = m.id;
      b.title = m.title;
      b.setAttribute("role", "tab");
      b.appendChild(S.el("span", "", m.label));
      const n = S.el("span", "params-filter-count", "");
      b.appendChild(n);
      b._count = n;
      b.addEventListener("click", () => {
        if (b.disabled || state.mode === m.id) return;
        state.mode = m.id;
        renderRows(state, view);
      });
      segButtons[m.id] = b;
      seg.appendChild(b);
    });
    toolbar.appendChild(seg);

    const count = S.el("span", "params-count", "");
    toolbar.appendChild(count);
    card.appendChild(toolbar);

    // The list host (rows are rendered into it on filter/render).
    const listHost = S.el("div", "params-table");
    listHost.setAttribute("role", "list");
    card.appendChild(listHost);

    // Unsaved drafts: written one by one, or dropped, from here.
    const pendingBar = S.el("div", "params-pending-bar");
    pendingBar.hidden = true;
    const pendingText = S.el("span", "params-pending-text", "");
    const discardBtn = Corvus.ui.button({
      variant: "secondary", size: "sm", label: "Discard", className: "params-discard",
    });
    const applyAllBtn = Corvus.ui.button({
      variant: "primary", size: "sm", icon: "check", label: "Apply all", className: "params-apply-all",
    });
    pendingBar.appendChild(pendingText);
    pendingBar.appendChild(discardBtn);
    pendingBar.appendChild(applyAllBtn);
    card.appendChild(pendingBar);

    const view = {
      banner, search, count, listHost, metaNote, segButtons,
      pendingBar, pendingText, discardBtn, applyAllBtn,
      armed: false, handles: new Map(), busy: false,
    };
    card._editorView = view;

    search.addEventListener("input", () => {
      state.search = search.value;
      renderRows(state, view);
    });
    discardBtn.addEventListener("click", () => {
      state.pending.clear();
      state.rowStatus.clear();
      renderRows(state, view);
    });
    applyAllBtn.addEventListener("click", () => applyAll(state, view));

    renderRows(state, view);
    updateMetaView(state);

    // Apply the current armed state to the banner + inputs.
    applyArmedToEditor(view, armedNow());
  }

  function matches(state, param, query) {
    if (state.mode === "modified" && !isModified(state, param)) return false;
    if (state.mode === "pending" && !state.pending.has(param.name)) return false;
    if (!query) return true;
    if ((param.name || "").toLowerCase().includes(query)) return true;
    const meta = metaFor(state, param.name);
    return !!(meta && meta.short_desc && meta.short_desc.toLowerCase().includes(query));
  }

  /**
   * Render only the filtered + visible slice (cap ~200 visible rows so a 1000+
   * param set never builds 1000+ DOM rows at once). Search narrows the set;
   * otherwise the first 200 are shown with a "showing N of M" note.
   *
   * `keepFocus` puts the keyboard back on the row it was in, for a redraw the
   * operator did not ask for (the defaults arriving mid-edit).
   */
  function renderRows(state, view, keepFocus) {
    let focusName = null;
    if (keepFocus && typeof document !== "undefined" && document.activeElement &&
        typeof document.activeElement.closest === "function") {
      const row = document.activeElement.closest(".params-row");
      focusName = row ? row.dataset.name : null;
    }
    if (state.mode === "modified" && !state.meta) state.mode = "all";
    const query = state.search.trim().toLowerCase();
    const filtered = state.params.filter((p) => matches(state, p, query));
    view.listHost.innerHTML = "";
    view.handles = new Map();

    filtered.slice(0, CAP).forEach((p) => {
      const handle = makeRow(state, view, p);
      view.listHost.appendChild(handle.row);
      view.handles.set(p.name, handle);
    });
    if (filtered.length > CAP) {
      view.listHost.appendChild(S.el("div", "params-more",
        `Showing ${CAP} of ${filtered.length}. Refine the search to see more.`));
    } else if (!filtered.length) {
      let text = "No parameter matches the search.";
      if (!query && state.mode === "modified") text = "Every parameter is at its default.";
      if (!query && state.mode === "pending") text = "No unsaved changes.";
      view.listHost.appendChild(S.el("div", "params-more", text));
    }
    view.count.textContent = `${filtered.length} parameter${filtered.length === 1 ? "" : "s"}`;
    updateCounts(state, view);
    updatePendingBar(state, view);
    if (Corvus.ui.refreshIcons) Corvus.ui.refreshIcons();
    if (focusName && view.handles.has(focusName)) {
      const control = view.handles.get(focusName).control;
      if (control && typeof control.focus === "function") control.focus();
    }
  }

  function updateCounts(state, view) {
    const b = view.segButtons;
    if (!b) return;
    const modified = state.meta ? state.params.filter((p) => isModified(state, p)).length : 0;
    b.all._count.textContent = String(state.params.length);
    b.modified._count.textContent = state.meta ? String(modified) : "";
    b.pending._count.textContent = state.pending.size ? String(state.pending.size) : "";
    b.modified.disabled = !state.meta;
    b.modified.title = state.meta ? MODES[1].title
      : (state.metaStatus && state.metaStatus.state === "loading"
        ? "Waiting for the defaults from the vehicle"
        : "Needs the defaults from the vehicle, which are not available");
    Object.keys(b).forEach((id) => b[id].setAttribute("aria-selected", String(state.mode === id)));
  }

  function updatePendingBar(state, view) {
    const n = state.pending.size;
    view.pendingBar.hidden = n === 0;
    view.pendingText.textContent = n ? `${plural(n, "unsaved change")}` : "";
    view.applyAllBtn.disabled = view.armed || view.busy || n === 0;
    view.discardBtn.disabled = view.busy || n === 0;
  }

  /** One editable row: name and description, value, default, Apply, status. */
  function makeRow(state, view, param) {
    const name = param.name || "";
    const meta = metaFor(state, name);
    const row = document.createElement("div");
    row.className = "params-row";
    row.setAttribute("role", "listitem");
    row.dataset.name = name;

    const nameCell = S.el("div", "params-name-cell");
    nameCell.appendChild(S.el("span", "params-name", name));
    if (meta && meta.short_desc) nameCell.appendChild(S.el("span", "params-hint", meta.short_desc));
    if (meta && (meta.long_desc || meta.short_desc)) nameCell.title = meta.long_desc || meta.short_desc;
    row.appendChild(nameCell);

    const valueCell = S.el("div", "params-value-cell");
    const draft = state.pending.has(name) ? state.pending.get(name) : formatValue(param.value, param.type);
    let control;
    if (meta && Array.isArray(meta.values) && meta.values.length) {
      const options = meta.values.map(([v, label]) => ({
        value: formatValue(v, param.type),
        label: label ? `${label} (${formatValue(v, param.type)})` : formatValue(v, param.type),
      }));
      if (!options.some((o) => o.value === draft)) {
        options.push({ value: draft, label: `${draft} (not in the list)` });
      }
      control = Corvus.ui.select({
        className: "params-value params-enum",
        ariaLabel: `Value for ${name}`,
        options, value: draft,
      });
      control.addEventListener("change", () => onDraft(control.value));
    } else {
      control = document.createElement("input");
      control.type = "text";
      control.className = "field-input field-input-mono params-value";
      control.value = draft;
      control.inputMode = "decimal";
      control.setAttribute("aria-label", `Value for ${name}`);
      control.addEventListener("input", () => onDraft(control.value));
    }
    const typeName = TYPE_NAMES[Number(param.type)];
    const bits = meta && Array.isArray(meta.bitmask)
      ? " Bits: " + meta.bitmask.map(([i, label]) => `${i} ${label}`).join(", ") : "";
    control.title = (typeName ? typeName + "." : "") + bits;
    control.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        if (!applyBtn.disabled) apply();
      } else if (e.key === "Escape" && state.pending.has(name)) {
        e.preventDefault();
        setDraft(formatValue(param.value, param.type));
      }
    });
    valueCell.appendChild(control);
    if (meta && meta.units) valueCell.appendChild(S.el("span", "params-unit", meta.units));
    row.appendChild(valueCell);

    const defaultCell = S.el("div", "params-default-cell");
    const defaultEl = S.el("span", "params-default", "");
    defaultCell.appendChild(defaultEl);
    const resetBtn = Corvus.ui.iconButton("rotate-ccw", {
      className: "icon-btn params-reset", size: 13,
      title: hasDefault(meta) ? `Set to the default, ${formatValue(meta.default, param.type)}` : "Set to the default",
      onClick: () => { if (hasDefault(meta)) setDraft(formatValue(meta.default, param.type)); },
    });
    defaultCell.appendChild(resetBtn);
    row.appendChild(defaultCell);

    const applyBtn = Corvus.ui.button({
      variant: "primary",
      size: "sm",
      className: "params-apply",
      label: "Apply",
      disabled: true,   // only enabled when the value changed & is valid
    });
    row.appendChild(applyBtn);

    const status = S.el("span", "params-row-status", "");
    row.appendChild(status);

    function onDraft(text) {
      const v = parseDraft(text, param, meta);
      if (v.ok && valuesEqual(v.value, param.value, param.type)) state.pending.delete(name);
      else state.pending.set(name, String(text));
      state.rowStatus.delete(name);
      refresh();
      updatePendingBar(state, view);
      updateCounts(state, view);
    }

    function setDraft(text) {
      if (control.tagName === "SELECT" &&
          !Array.prototype.some.call(control.children || [], (o) => o.value === text)) {
        const opt = document.createElement("option");
        opt.value = text;
        opt.textContent = `${text} (not in the list)`;
        control.appendChild(opt);
      }
      control.value = text;
      if (control.tagName === "SELECT" && typeof control.dispatchEvent === "function" &&
          typeof Event === "function") {
        control.dispatchEvent(new Event("change"));   // the enhanced dropdown repaints on it
      } else {
        onDraft(text);
      }
    }

    // refresh: recompute every derived state of the row from the live armed
    // flag, the draft's validity, and whether it differs from the vehicle.
    // Stored on the handle so an armed transition or a write can refresh
    // rows without rebuilding them.
    function refresh() {
      const text = String(control.value);
      const v = parseDraft(text, param, meta);
      const changed = !(v.ok && valuesEqual(v.value, param.value, param.type));
      control.classList.toggle("invalid", !v.ok);
      control.classList.toggle("warn", v.ok && !!v.warn);
      if (control.tagName === "SELECT") control.disabled = !!view.armed;
      else control.readOnly = !!view.armed;
      applyBtn.disabled = view.armed || view.busy || !(v.ok && changed);
      row.classList.toggle("is-pending", changed);
      row.classList.toggle("is-modified", isModified(state, param));

      if (hasDefault(meta)) {
        defaultEl.textContent = formatValue(meta.default, param.type);
        defaultEl.title = `Default ${formatValue(meta.default, param.type)}${meta.units ? " " + meta.units : ""}`;
        resetBtn.hidden = v.ok && valuesEqual(v.value, meta.default, param.type);
        resetBtn.disabled = !!view.armed;
      } else {
        defaultEl.textContent = "";
        defaultEl.title = state.meta ? "No default known for this parameter" : "";
        resetBtn.hidden = true;
      }

      const st = state.rowStatus.get(name);
      if (!v.ok) {
        status.textContent = v.error;
        status.className = "params-row-status err";
      } else if (st) {
        status.textContent = st.text;
        status.className = "params-row-status" + (st.cls ? " " + st.cls : "");
      } else if (changed && v.warn) {
        status.textContent = v.warn;
        status.className = "params-row-status warn";
      } else {
        status.textContent = "";
        status.className = "params-row-status";
      }
    }

    async function apply() {
      const v = parseDraft(control.value, param, meta);
      if (!v.ok) {
        refresh();
        applyBtn.disabled = true;
        return;
      }
      applyBtn.disabled = true;
      await writeParam(state, param, v.value);
      const fresh = state.card._editorView;
      if (fresh) {
        const handle = fresh.handles.get(name);
        if (handle) handle.refresh();
        updatePendingBar(state, fresh);
        updateCounts(state, fresh);
      }
    }

    applyBtn.addEventListener("click", apply);

    refresh();
    return { row, control, applyBtn, resetBtn, status, refresh };
  }

  /** POST /api/params/set {name, value}; records the row's saving/saved/failed state. */
  async function writeParam(state, param, value) {
    const name = param.name;
    state.rowStatus.set(name, { cls: "pending", text: "saving" });
    const view = state.card && state.card._editorView;
    const handle = view && view.handles.get(name);
    if (handle) handle.refresh();
    try {
      await Corvus.telemetry.postAction("/api/params/set", { name, value });
      param.value = value;     // update the local cache so the row is no longer "changed"
      state.pending.delete(name);
      const meta = metaFor(state, name);
      state.rowStatus.set(name, {
        cls: "ok", text: meta && meta.reboot_required ? "saved, reboot to apply" : "saved",
      });
      return true;
    } catch (err) {
      const msg = (err && err.message) || "set failed";
      state.rowStatus.set(name, { cls: "err", text: msg });
      notify("critical", `Could not set ${name}: ${msg}`);
      return false;
    }
  }

  /** Write every draft, one confirmed write after the other. */
  async function applyAll(state, view) {
    if (view.busy || view.armed || !state.pending.size) return;
    view.busy = true;
    Corvus.ui.setBusy(view.applyAllBtn, true);
    updatePendingBar(state, view);
    view.handles.forEach((h) => h.refresh());
    let written = 0;
    let failed = 0;
    let reboot = false;
    for (const [name, text] of Array.from(state.pending)) {
      if (state.destroyed || view.armed) break;
      const param = state.byName.get(name);
      if (!param) { state.pending.delete(name); continue; }
      const meta = metaFor(state, name);
      const v = parseDraft(text, param, meta);
      if (!v.ok) {
        state.rowStatus.set(name, { cls: "err", text: v.error });
        failed += 1;
        continue;
      }
      if (await writeParam(state, param, v.value)) {
        written += 1;
        if (meta && meta.reboot_required) reboot = true;
      } else {
        failed += 1;
      }
      const handle = view.handles.get(name);
      if (handle) handle.refresh();
    }
    view.busy = false;
    Corvus.ui.setBusy(view.applyAllBtn, false);
    if (state.destroyed) return;
    let text = `Wrote ${plural(written, "parameter")}`;
    if (failed) text += `, ${failed} failed`;
    if (reboot) text += ". Reboot the autopilot to apply them";
    setStatus(state, failed ? "err" : "ok", text);
    if (state.card._editorView === view) {
      view.handles.forEach((h) => h.refresh());
      updatePendingBar(state, view);
      updateCounts(state, view);
    }
  }

  /** Toggle the read-only + banner state on the whole editor (armed gating). */
  function applyArmedToEditor(view, armed) {
    if (!view) return;
    view.armed = !!armed;
    if (view.banner) view.banner.hidden = !armed;
    if (view.handles) view.handles.forEach((h) => {
      try { h.refresh(); } catch (_e) {}
    });
    if (view.pendingBar) {
      view.applyAllBtn.disabled = view.armed || view.busy || view.pendingBar.hidden;
    }
  }

  return { render, parseParamFile, formatValue };
})();
