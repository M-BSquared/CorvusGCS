"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupParameters — the Parameters sub-page of the Setup page.
 *
 * Lazy, on-demand PX4 parameter editor. The operator must click "Download
 * Parameters" to fetch the full set; until then Corvus only streams what's
 * needed to fly (lean). The editor only appears once the download is complete
 * (the backend returns params=[] until then). Each row writes via
 * POST /api/params/set (refused while armed).
 *
 * Backend contract (verified against PX4 v1.18):
 *   POST /api/params/download        start full param download (one-shot)
 *   GET  /api/params                 {state,count,received,complete,params}
 *                                   LEAN: params=[] until complete is true
 *   GET  /api/params/progress        SSE: progress {state,count,received}
 *   POST /api/params/set {name,val}  write one param (refused while armed)
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the view lifecycle and calls destroy() on back / left-nav re-entry so
 * the params SSE, the ~1s config-status poll, and the telemetry subscription
 * are all released.
 */
Corvus.setupParameters = (function () {
  const S = Corvus.setupShared;

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page");
    // The back button only asks the orchestrator to navigate back; the
    // orchestrator's teardown() is the single place that calls destroy().
    page.appendChild(S.backButton(navigateBack));
    page.appendChild(S.pageHeader("Parameters", "Download all PX4 parameters, then edit"));

    // Actions bar (Export / Import + status). Lives in `page`, sibling of the
    // `section`, so it stays visible across every phase — the editor's `card`
    // wipes its innerHTML per phase and would otherwise drop these buttons.
    const actions = S.el("div", "params-actions");
    const exportBtn = makeActionButton("download", "Export");
    const importBtn = makeActionButton("upload", "Import");
    // SYS_AUTOSTART, SENS_EN_*, SER_* and the like are read at boot. After a
    // reboot the rows on screen are the old boot's, so the page goes back to
    // the download prompt rather than keep showing them.
    const rebootBtn = S.rebootButton({ size: "sm", mount: page, onRebooted: () => {
      if (state.destroyed) return;
      state.params = [];
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

    // Local param cache + the progress SSE/poll handles, captured here so
    // teardown can close everything. The export/import buttons are hoisted onto
    // `state` because finishDownload/openUploadProgress are module-level helpers
    // that cannot see this closure.
    const state = {
      params: [],          // the editable list (only once complete)
      eventSource: null,   // the ONE /api/params/progress SSE (see watchProgress)
      progressSubs: null,  // who is watching it; the stream closes with the last
      pollTimer: null,     // the ~1 s GET /api/params fallback
      unsub: null,          // telemetry subscription (armed gating)
      rendered: false,
      exportBtn,           // enabled once the full param set is loaded
      importBtn,           // armed-gated; disabled during upload
      actionsStatus,
      downloadUnwatch: null,     // release the download view's watch
      uploadUnwatch: null,       // release an import's watch
      uploading: false,
      card,                // the editor redraws into it after an import
      destroyed: false,
    };

    // Phase 1: show the Download button (do NOT auto-download — lean + on-demand).
    renderDownloadPrompt(card, state);

    // Subscribe to telemetry so the editor's armed banner gates live. The
    // download prompt has no banner; the editor applies the state on render and
    // on every armed transition while it is open. The same callback also gates
    // the Import button (uploads are refused while armed).
    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      state.unsub = Corvus.telemetry.subscribe((s) => {
        if (card._editorView) applyArmedToEditor(card._editorView, !!(s && s.armed));
        if (!state.uploading) importBtn.disabled = !!(s && s.armed);
        rebootBtn.disabled = !!(s && s.armed);
      });
    }

    // Export: serialise the loaded set to a Corvus param file (JSON) and download
    // it. No version literal — the metadata version comes from GET /api/version.
    exportBtn.addEventListener("click", () => exportParams(state));
    // Import: pick a file, validate, confirm, POST to the drone, follow the SSE.
    importBtn.addEventListener("click", () => importParams(state));

    function destroy() {
      state.destroyed = true;
      if (state.unsub) { try { state.unsub(); } catch (_e) {} state.unsub = null; }
      state.downloadUnwatch = null;
      state.uploadUnwatch = null;
      closeProgressStream(state);
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

  /**
   * Export the loaded parameter set to a Corvus param file (JSON).
   *
   * Opens a dialog with the filename and the folder, both prefilled from
   * GET /api/params/export/target, then POSTs to /api/params/export which
   * writes the file and reports the full path back.
   *
   * The file is written by the BACKEND, not pulled as a browser download.
   * The desktop build runs this UI inside QtWebEngine, which drops an
   * `<a download>` unless the host app implements a download handler — so the
   * previous export silently produced no file there at all. Writing it
   * server-side behaves identically in the desktop app and in a browser, lands
   * it in a folder the operator chose, and can say exactly where it went.
   */
  async function exportParams(state) {
    if (!state.params || !state.params.length) return;

    let target = {};
    try {
      target = await Corvus.telemetry.requestJson("/api/params/export/target");
    } catch (_e) { /* offline/unsupported: the dialog falls back to placeholders */ }

    const nameInput = Corvus.ui.input({
      id: "paramsExportName",
      ariaLabel: "File name",
      value: target.filename || "",
      placeholder: "corvus-params.json",
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

    const body = document.createDocumentFragment();
    body.appendChild(S.el("div", "params-desc",
      `${state.params.length} parameters will be written as a Corvus parameter ` +
      "file. The same file can be re-imported with Import."));
    body.appendChild(Corvus.ui.field({ label: "File name", control: nameInput }));
    body.appendChild(Corvus.ui.field({
      label: "Folder",
      control: dirInput,
      hint: "On the machine running Corvus. Created if it does not exist.",
    }));
    const msg = Corvus.ui.message();
    body.appendChild(msg.el);

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
      Corvus.ui.setBusy(saveBtn, true);
      try {
        const res = await Corvus.telemetry.requestJson("/api/params/export", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            filename: nameInput.value.trim(),
            dir: dirInput.value.trim(),
            params: state.params.map((p) => ({ name: p.name, value: p.value, type: p.type })),
          }),
        });
        dialog.close();
        // The saved path is the useful part of the confirmation — it is what
        // the operator needs to find the file afterwards.
        setStatus(state, "ok", `Saved ${res.param_count} parameters to ${res.path}`);
        window.dispatchEvent(new CustomEvent("corvus:notification",
          { detail: { level: "info", message: `Parameters exported to ${res.path}` } }));
      } catch (err) {
        msg.show((err && err.message) || "Export failed", "err");
      } finally {
        Corvus.ui.setBusy(saveBtn, false);
      }
    }
  }

  /**
   * Import: open a file picker, read + validate a Corvus param file, confirm
   * with the operator, POST the params to the drone, then follow the upload
   * progress SSE to completion.
   */
  function importParams(state) {
    if (state.uploading) return;
    const input = document.createElement("input");
    input.type = "file";
    input.setAttribute("accept", ".json,application/json");
    document.body.appendChild(input);
    input.addEventListener("change", () => {
      input.remove();
      onImportFile(state, input).catch((err) => {
        const msg = (err && err.message) || "Import failed";
        setStatus(state, "err", msg);
        window.dispatchEvent(new CustomEvent("corvus:notification",
          { detail: { level: "critical", message: msg } }));
        const s = Corvus.telemetry && Corvus.telemetry.getState();
        state.importBtn.disabled = !!(s && s.armed);
      });
    });
    input.click();
  }

  async function onImportFile(state, input) {
    const file = input.files && input.files[0];
    if (!file) return;
    const text = await readFileText(file);
    let doc;
    try {
      doc = JSON.parse(text);
    } catch (_e) {
      throw new Error("File is not valid JSON");
    }
    if (!doc || !Array.isArray(doc.params) || !doc.params.length) {
      throw new Error("Parameter file has no parameters");
    }
    const clean = [];
    for (const p of doc.params) {
      if (!p || typeof p.name !== "string" || !p.name.length || typeof p.value !== "number") {
        throw new Error("Parameter file has an invalid entry");
      }
      clean.push({ name: p.name, value: p.value });
    }
    if (typeof window.confirm === "function" &&
        !window.confirm(`Apply ${clean.length} parameters to the drone?`)) return;
    try {
      await Corvus.telemetry.postAction("/api/params/upload", { params: clean });
      openUploadProgress(state, clean.length);
    } catch (err) {
      const msg = (err && err.message) || "Upload failed";
      setStatus(state, "err", msg);
      window.dispatchEvent(new CustomEvent("corvus:notification",
        { detail: { level: "critical", message: msg } }));
      const s = Corvus.telemetry && Corvus.telemetry.getState();
      state.importBtn.disabled = !!(s && s.armed);
    }
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
   * Upload progress view: the /api/params/progress SSE emits `{state,count,
   * received}`. During an upload the backend sets state to "uploading" then
   * "upload_complete"; we close the SSE and fetch the final result on the
   * latter. No poll fallback — the final result is GET /api/params/upload/result.
   */
  function openUploadProgress(state, total) {
    state.uploading = true;
    state.importBtn.disabled = true;
    setStatus(state, "pending", `Uploading 0 / ${total} parameters…`);
    state.uploadUnwatch = watchProgress(state, (d) => {
      if (d.state === "upload_complete") {
        closeUploadSse(state);
        finishUpload(state);
      } else {
        setStatus(state, "pending",
          `Uploading ${d.received || 0} / ${d.count || 0} parameters…`);
      }
    });
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
  }

  /** Fetch the final upload result and summarise written/failed counts. */
  async function finishUpload(state) {
    let r;
    try {
      r = await Corvus.telemetry.requestJson("/api/params/upload/result");
    } catch (err) {
      const msg = (err && err.message) || "Upload result unavailable";
      setStatus(state, "err", msg);
      window.dispatchEvent(new CustomEvent("corvus:notification",
        { detail: { level: "critical", message: msg } }));
      state.uploading = false;
      const s = Corvus.telemetry && Corvus.telemetry.getState();
      state.importBtn.disabled = !!(s && s.armed);
      return;
    }
    const written = (r && r.written) || 0;
    const failed = (r && r.failed) || 0;
    const errors = (r && Array.isArray(r.errors)) ? r.errors : [];
    if (failed === 0) {
      setStatus(state, "ok", `Uploaded ${written} parameters`);
      window.dispatchEvent(new CustomEvent("corvus:notification",
        { detail: { level: "info", message: `Uploaded ${written} parameters` } }));
    } else {
      const cls = written > 0 ? "ok" : "err";
      let text = `Uploaded ${written}, ${failed} failed`;
      if (errors.length) text += `: ${errors.slice(0, 3).map((e) => e.name).join(", ")}`;
      setStatus(state, cls, text);
      window.dispatchEvent(new CustomEvent("corvus:notification",
        { detail: { level: "critical", message: text } }));
    }
    state.uploading = false;
    const s = Corvus.telemetry && Corvus.telemetry.getState();
    state.importBtn.disabled = !!(s && s.armed);
    if (state.rendered) refreshEditor(state);
  }

  /**
   * Redraw an open editor from the vehicle's set after an import.
   *
   * The confirmed writes went into the backend's copy, so the rows on screen
   * are the values from before the upload, and an Export now would write
   * those. The filter the operator typed survives the redraw.
   */
  function refreshEditor(state) {
    Corvus.telemetry.requestJson("/api/params").then((d) => {
      if (state.destroyed || !d || !d.complete || !Array.isArray(d.params)) return;
      const view = state.card && state.card._editorView;
      const filter = view ? view.search.value : "";
      state.params = d.params.slice().sort(byName);
      renderEditor(state.card, state);
      const fresh = state.card._editorView;
      if (filter && fresh) {
        fresh.search.value = filter;
        fresh.filter = filter.trim().toLowerCase();
        renderRows(state, fresh);
      }
    }).catch(() => { /* the rows stay as they were; Download reads them again */ });
  }

  /** Apply the status class + text to the actions status line. */
  function setStatus(state, cls, text) {
    if (!state.actionsStatus) return;
    state.actionsStatus.className = "params-actions-status" + (cls ? " " + cls : "");
    state.actionsStatus.textContent = text || "";
  }

  /** Phase 1: the Download-on-demand prompt + explanation (lean philosophy). */
  function renderDownloadPrompt(card, state) {
    card.innerHTML = "";
    card.appendChild(S.sectionTitle("Parameter Set"));

    const desc = S.el("div", "params-desc");
    desc.textContent =
      "Downloading all PX4 parameters. Until then, Corvus only streams what's needed to fly (lean). " +
      "You must wait for the full set before editing.";
    card.appendChild(desc);

    const btn = Corvus.ui.button({
      variant: "primary",
      className: "params-download-btn",
      icon: "download",
      label: "Download Parameters",
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
    status.hidden = false;
    status.className = "params-status";
    status.textContent = "Requesting parameter list…";

    Corvus.telemetry.postAction("/api/params/download", {})
      .then(() => openProgressView(card, state))
      .catch((err) => {
        // 503 (disconnected) or other error — keep the Download button usable.
        btn.disabled = false;
        status.className = "params-status err";
        status.textContent = (err && err.message) || "Could not start download";
      });
  }

  /**
   * Progress view: SSE primary + ~1 s GET poll fallback (a config-status poll,
   * not telemetry). The operator MUST wait until `complete` before the editor
   * appears (per the lean contract: params is [] until complete).
   */
  function openProgressView(card, state) {
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

    // Primary: the shared SSE progress stream.
    state.downloadUnwatch = watchProgress(state, (d) => {
      setProgress(d.received || 0, d.count || 0);
      if (d.state === "complete") finishDownload(card, state);
    });

    // Safety-net poll of the config status (NOT telemetry). ~1 s cadence.
    state.pollTimer = window.setInterval(() => {
      Corvus.telemetry.requestJson("/api/params").then((d) => {
        setProgress(d.received || 0, d.count || 0);
        if (d.complete) finishDownload(card, state);
      }).catch(() => { /* transient; the SSE/poll retry on the next tick */ });
    }, 1000);
  }

  /** Phase 3: download complete -> fetch the full set and render the editor. */
  function finishDownload(card, state) {
    if (state.downloadUnwatch) { state.downloadUnwatch(); state.downloadUnwatch = null; }
    if (state.pollTimer) { try { window.clearInterval(state.pollTimer); } catch (_e) {} state.pollTimer = null; }
    if (state.rendered) return;   // guard against SSE + poll both firing

    Corvus.telemetry.requestJson("/api/params").then((d) => {
      if (!d || !d.complete || !Array.isArray(d.params) || !d.params.length) {
        // Not actually complete yet (race) — re-open the progress view.
        openProgressView(card, state);
        return;
      }
      state.params = d.params.slice().sort(byName);
      if (state.exportBtn) state.exportBtn.disabled = false;   // params loaded → Export usable
      state.rendered = true;
      renderEditor(card, state);
    }).catch((err) => {
      card.innerHTML = "";
      card.appendChild(S.sectionTitle("Parameters"));
      const status = S.el("div", "params-status err");
      status.textContent = (err && err.message) || "Could not load parameters";
      card.appendChild(status);
    });
  }

  function byName(a, b) {
    return (a.name || "").localeCompare(b.name || "");
  }

  /** Phase 4: the searchable, sortable, per-row-apply editor. */
  function renderEditor(card, state) {
    card.innerHTML = "";
    card.appendChild(S.sectionTitle(`Parameters (${state.params.length})`));

    // Armed banner (read-only inputs while armed).
    const banner = S.el("div", "params-banner");
    banner.hidden = true;
    banner.textContent = "Parameter editing disabled while armed";
    card.appendChild(banner);

    // Toolbar: search input + count.
    const toolbar = S.el("div", "params-toolbar");
    const search = document.createElement("input");
    search.type = "text";
    search.className = "field-input params-search";
    search.placeholder = "Filter by names…";
    search.setAttribute("aria-label", "Filter parameters by name");
    const count = S.el("span", "params-count", "");
    toolbar.appendChild(search);
    toolbar.appendChild(count);
    card.appendChild(toolbar);

    // The list host (rows are rendered into it on filter/render).
    const listHost = S.el("div", "params-table");
    listHost.setAttribute("role", "list");
    card.appendChild(listHost);

    const view = { banner, search, count, listHost, filter: "", armed: false, rows: [] };
    card._editorView = view;
    search.addEventListener("input", () => {
      view.filter = search.value.trim().toLowerCase();
      renderRows(state, view);
    });
    renderRows(state, view);

    // Apply the current armed state to the banner + inputs.
    const cur = Corvus.telemetry && Corvus.telemetry.getState();
    applyArmedToEditor(view, !!(cur && cur.armed));
  }

  /**
   * Render only the filtered + visible slice (cap ~200 visible rows so a 1000+
   * param set never builds 1000+ DOM rows at once). Search narrows the set;
   * otherwise the first 200 are shown with a "showing N of M" note.
   */
  function renderRows(state, view) {
    const filtered = state.params.filter((p) =>
      !view.filter || (p.name || "").toLowerCase().includes(view.filter));
    view.listHost.innerHTML = "";
    view.rows = [];

    const CAP = 200;
    filtered.slice(0, CAP).forEach((p) => {
      const row = makeRow(state, view, p);
      view.listHost.appendChild(row);
      view.rows.push(row._recheck);
    });
    if (filtered.length > CAP) {
      view.listHost.appendChild(S.el("div", "params-more",
        `Showing ${CAP} of ${filtered.length}. Refine the search to see more.`));
    }
    view.count.textContent = `${filtered.length} parameter${filtered.length === 1 ? "" : "s"}`;
  }

  /** One editable row: name (mono), value input, type, Apply button, status. */
  function makeRow(state, view, param) {
    const row = document.createElement("div");
    row.className = "params-row";
    row.setAttribute("role", "listitem");
    row.dataset.name = param.name || "";

    row.appendChild(S.el("span", "params-name", param.name || ""));

    const valueInput = document.createElement("input");
    valueInput.type = "text";
    valueInput.className = "field-input field-input-mono params-value";
    valueInput.value = String(param.value);
    valueInput.inputMode = "decimal";
    valueInput.setAttribute("aria-label", `Value for ${param.name}`);
    row.appendChild(valueInput);

    row.appendChild(S.el("span", "params-type", param.type ? String(param.type) : ""));

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

    // recheck: recompute the Apply disabled + invalid state from the live armed
    // flag, the numeric validity, and whether the value changed. Stored on the
    // row so an armed transition can refresh every row without DOM walks.
    function recheck() {
      const valid = isNumeric(valueInput.value);
      const changed = String(valueInput.value) !== String(param.value);
      valueInput.classList.toggle("invalid", !valid);
      valueInput.readOnly = !!view.armed;
      applyBtn.disabled = view.armed ? true : !(valid && changed);
    }
    row._recheck = recheck;

    valueInput.addEventListener("input", recheck);
    applyBtn.addEventListener("click", () => applyParam(param, valueInput, applyBtn, status));

    recheck();
    return row;
  }

  function isNumeric(v) {
    if (v === null || v === undefined || String(v).trim() === "") return false;
    const n = Number(v);
    return isFinite(n);
  }

  /** POST /api/params/set {name, value}; show per-row pending/saved/failed. */
  async function applyParam(param, input, btn, status) {
    if (!isNumeric(input.value)) {
      input.classList.add("invalid");
      btn.disabled = true;
      return;
    }
    const value = Number(input.value);
    btn.disabled = true;
    status.textContent = "saving";
    status.className = "params-row-status pending";
    try {
      await Corvus.telemetry.postAction("/api/params/set", { name: param.name, value });
      param.value = value;     // update the local cache so the row is no longer "changed"
      input.classList.remove("invalid");
      status.textContent = "saved";
      status.className = "params-row-status ok";
      // Apply disabled again until the value diverges once more.
      btn.disabled = !(String(input.value) !== String(param.value));
    } catch (err) {
      const msg = (err && err.message) || "set failed";
      status.textContent = msg;
      status.className = "params-row-status err";
      btn.disabled = false;   // let the operator retry / fix the value
      window.dispatchEvent(new CustomEvent("corvus:notification",
        { detail: { level: "critical", message: `Could not set ${param.name}: ${msg}` } }));
    }
  }

  /** Toggle the read-only + banner state on the whole editor (armed gating). */
  function applyArmedToEditor(view, armed) {
    if (!view) return;
    view.armed = !!armed;
    if (view.banner) view.banner.hidden = !armed;
    if (view.rows) view.rows.forEach((recheck) => {
      try { recheck(); } catch (_e) {}
    });
  }

  return { render };
})();
