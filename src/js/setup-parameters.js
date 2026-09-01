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

    const section = S.el("div", "page-section");
    const card = S.el("div", "page-card params-card");
    section.appendChild(card);
    page.appendChild(section);
    container.appendChild(page);

    // Local param cache + the progress SSE/poll handles, captured here so
    // teardown can close everything.
    const state = {
      params: [],          // the editable list (only once complete)
      eventSource: null,   // the /api/params/progress SSE
      pollTimer: null,     // the ~1 s GET /api/params fallback
      unsub: null,          // telemetry subscription (armed gating)
      rendered: false,
    };

    // Phase 1: show the Download button (do NOT auto-download — lean + on-demand).
    renderDownloadPrompt(card, state);

    // Subscribe to telemetry so the editor's armed banner gates live. The
    // download prompt has no banner; the editor applies the state on render and
    // on every armed transition while it is open.
    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      state.unsub = Corvus.telemetry.subscribe((s) => {
        if (card._editorView) applyArmedToEditor(card._editorView, !!(s && s.armed));
      });
    }

    function destroy() {
      if (state.unsub) { try { state.unsub(); } catch (_e) {} state.unsub = null; }
      if (state.eventSource) { try { state.eventSource.close(); } catch (_e) {} state.eventSource = null; }
      if (state.pollTimer) { try { window.clearInterval(state.pollTimer); } catch (_e) {} state.pollTimer = null; }
    }

    return destroy;
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

    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "params-download-btn";
    btn.appendChild(S.icon("download"));
    btn.appendChild(S.el("span", null, "Download Parameters"));
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

    // Primary: SSE progress stream.
    try {
      state.eventSource = new EventSource("/api/params/progress");
      state.eventSource.addEventListener("progress", (e) => {
        try {
          const d = JSON.parse(e.data);
          setProgress(d.received || 0, d.count || 0);
          if (d.state === "complete") finishDownload(card, state);
        } catch (_err) { /* keep the poll fallback as the safety net */ }
      });
      state.eventSource.addEventListener("ping", () => {});
      state.eventSource.onerror = () => { /* poll fallback covers this */ };
    } catch (_err) { /* no EventSource — poll fallback covers this */ }

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
    if (state.eventSource) { try { state.eventSource.close(); } catch (_e) {} state.eventSource = null; }
    if (state.pollTimer) { try { window.clearInterval(state.pollTimer); } catch (_e) {} state.pollTimer = null; }
    if (state.rendered) return;   // guard against SSE + poll both firing

    Corvus.telemetry.requestJson("/api/params").then((d) => {
      if (!d || !d.complete || !Array.isArray(d.params) || !d.params.length) {
        // Not actually complete yet (race) — re-open the progress view.
        openProgressView(card, state);
        return;
      }
      state.params = d.params.slice().sort(byName);
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
    search.className = "params-search";
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
        `Showing ${CAP} of ${filtered.length} — refine the search to see more.`));
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
    valueInput.className = "params-value";
    valueInput.value = String(param.value);
    valueInput.inputMode = "decimal";
    valueInput.setAttribute("aria-label", `Value for ${param.name}`);
    row.appendChild(valueInput);

    row.appendChild(S.el("span", "params-type", param.type ? String(param.type) : ""));

    const applyBtn = document.createElement("button");
    applyBtn.type = "button";
    applyBtn.className = "params-apply";
    applyBtn.textContent = "Apply";
    applyBtn.disabled = true;   // only enabled when the value changed & is valid
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
