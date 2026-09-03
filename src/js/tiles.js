"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.tiles — offline-map download panel (Task D5).

  Contract:
    init(triggerEl, popoverEl) — wire the floating trigger + popover. Called
      from app.init AFTER Corvus.map.init so the map is available.
    Reads the current map view bounds from Corvus.map.getMap().getBounds()
    (a MapLibre LngLatBounds: .getWest/.getSouth/.getEast/.getNorth), prefills
    minzoom = current zoom, maxzoom = current zoom + 4 (capped at the source
    cap), and estimates the slippy-map tile count + disk size client-side.
    POST /api/tiles/download starts a job; an EventSource streams progress from
    /api/tiles/progress?id=<job_id>; POST /api/tiles/cancel aborts it.

  Version is never referenced here. Every control is a .btn built via
  Corvus.ui; the popover reuses the shared .glass material and .corvus-enter
  entrance defined in components.css.

  NOTE on the source cap: /api/tiles/sources returns the real upstream
  raster cap under "maxzoom" (19) and the CACHED tile range under
  "cached_minzoom"/"cached_maxzoom" (null when the cache is empty). The
  duplicate-key bug that previously shadowed "maxzoom" has been fixed in the
  backend. SOURCE_ZOOM_CAP stays as a safe fallback matching that upstream
  raster cap (19), shared by every configured ArcGIS source; it is a
  tile-source characteristic, not a version literal, and still clamps the
  zoom inputs in refreshFromMap/onSourceOrZoomChange.
*/
Corvus.tiles = (function () {
  const AVG_TILE_KB = 15;     // rough raster-tile size for the disk estimate
  const ZOOM_SPAN = 4;        // default maxzoom = current zoom + this
  const SOURCE_ZOOM_CAP = 19; // upstream cap for the configured ArcGIS sources
  const HARD_ZOOM_FLOOR = 0;
  const HARD_ZOOM_CEIL = 22;  // server rejects z > 22 on the serve path

  let triggerEl = null;
  let popoverEl = null;

  let sources = [];           // last /api/tiles/sources result
  let captured = null;        // {w,s,e,n} captured from the map at open/recapture
  let es = null;              // active EventSource for the running job
  let jobId = null;           // id of the running job (for cancel)
  let dom = {};               // populated by buildPopover

  // ---- slippy-map tile math (Web Mercator / XYZ) ----
  function lonToX(lon, z) { return Math.floor(((lon + 180) / 360) * Math.pow(2, z)); }
  function latToY(lat, z) {
    const r = (lat * Math.PI) / 180;
    return Math.floor(((1 - Math.log(Math.tan(r) + 1 / Math.cos(r)) / Math.PI) / 2) * Math.pow(2, z));
  }

  /** Total tile count for a bbox across [minzoom..maxzoom] (slippy-map formula). */
  function estimateTileCount(w, s, e, n, minzoom, maxzoom) {
    let count = 0;
    const lo = Math.min(minzoom, maxzoom);
    const hi = Math.max(minzoom, maxzoom);
    for (let z = lo; z <= hi; z++) {
      const xMin = Math.min(lonToX(w, z), lonToX(e, z));
      const xMax = Math.max(lonToX(w, z), lonToX(e, z));
      const yMin = Math.min(latToY(n, z), latToY(s, z));
      const yMax = Math.max(latToY(n, z), latToY(s, z));
      count += (xMax - xMin + 1) * (yMax - yMin + 1);
    }
    return count;
  }

  function fmtCount(n) {
    if (n >= 1e6) return (n / 1e6).toFixed(1) + "M";
    if (n >= 1e3) return (n / 1e3).toFixed(1) + "k";
    return String(n);
  }

  function fmtSize(kb) {
    if (kb >= 1e6) return (kb / 1e6).toFixed(1) + " GB";
    if (kb >= 1e3) return (kb / 1e3).toFixed(0) + " MB";
    return Math.round(kb) + " KB";
  }

  function el(tag, cls) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    return e;
  }

  /** True if the MapLibre map is ready for a bounds read. */
  function mapReady() {
    return !!(Corvus.map && typeof Corvus.map.getMap === "function"
      && Corvus.map.getMap() && typeof Corvus.map.getMap().getBounds === "function");
  }

  /** Read + store the current view bounds; returns null if the map isn't ready. */
  function captureBounds() {
    if (!mapReady()) return null;
    const b = Corvus.map.getMap().getBounds();
    const z = Corvus.map.getMap().getZoom();
    captured = {
      w: b.getWest(), s: b.getSouth(), e: b.getEast(), n: b.getNorth(),
      zoom: Math.floor(z),
    };
    return captured;
  }

  // ---- popover DOM (built once; this module owns it) ----
  function buildPopover() {
    popoverEl.innerHTML = "";

    const header = el("div", "tiles-header");
    const title = el("span", "tiles-title"); title.textContent = "OFFLINE MAP";
    const closeBtn = Corvus.ui.iconButton("x", { title: "Close" });
    closeBtn.id = "tilesClose";
    header.appendChild(title); header.appendChild(closeBtn);
    popoverEl.appendChild(header);

    const body = el("div", "tiles-body");

    // source picker
    const srcField = el("div", "tiles-field");
    srcField.appendChild(makeLabel("Source"));
    const srcSelect = el("select", "tiles-select"); srcSelect.id = "tilesSource";
    srcField.appendChild(srcSelect);
    body.appendChild(srcField);

    // captured bounds + recapture
    const bndField = el("div", "tiles-field");
    const bndHead = el("div", "tiles-bounds-head");
    bndHead.appendChild(makeLabel("Region (current view)"));
    const recapture = Corvus.ui.iconButton("locate-fixed", { title: "Recapture current view" });
    recapture.id = "tilesRecapture";
    bndHead.appendChild(recapture);
    bndField.appendChild(bndHead);
    const bndValue = el("span", "tiles-bounds"); bndValue.id = "tilesBounds";
    bndField.appendChild(bndValue);
    body.appendChild(bndField);

    // zoom inputs
    const zoomRow = el("div", "tiles-zoom-row");
    zoomRow.appendChild(makeZoomField("Min zoom", "tilesMinZoom"));
    zoomRow.appendChild(makeZoomField("Max zoom", "tilesMaxZoom"));
    body.appendChild(zoomRow);

    // estimate
    const est = el("div", "tiles-estimate");
    est.appendChild(makeLabel("Estimate"));
    const estWrap = el("div", "tiles-estimate-val");
    const estVal = el("span", "tiles-estimate-value"); estVal.id = "tilesEstimate";
    const estSub = el("span", "tiles-estimate-sub"); estSub.id = "tilesEstimateSub";
    estWrap.appendChild(estVal); estWrap.appendChild(estSub);
    est.appendChild(estWrap);
    body.appendChild(est);

    // actions
    const actions = el("div", "tiles-actions");
    const dlBtn = Corvus.ui.button({ variant: "primary", icon: "download", label: "Download" });
    dlBtn.id = "tilesDownload";
    const cancelBtn = Corvus.ui.button({ variant: "danger", icon: "x", label: "Cancel", disabled: true });
    cancelBtn.id = "tilesCancel";
    actions.appendChild(dlBtn); actions.appendChild(cancelBtn);
    body.appendChild(actions);

    // progress (hidden until a job starts)
    const progress = el("div", "tiles-progress"); progress.id = "tilesProgress"; progress.hidden = true;
    const bar = el("div", "progress-bar");
    const fill = el("div", "progress-bar-fill"); fill.id = "tilesBarFill";
    bar.appendChild(fill); progress.appendChild(bar);
    const progressText = el("div", "tiles-progress-text"); progressText.id = "tilesProgressText";
    progressText.textContent = "0 / 0 (0%)";
    progress.appendChild(progressText);
    body.appendChild(progress);

    // status message
    const msg = el("div", "tiles-msg"); msg.id = "tilesMsg"; msg.hidden = true;
    body.appendChild(msg);

    // cached stats
    body.appendChild(makeLabel("Cached", "tiles-stats-title"));
    const stats = el("div", "tiles-stats"); stats.id = "tilesStats";
    body.appendChild(stats);

    popoverEl.appendChild(body);

    dom = {
      srcSelect, bndValue, recapture, dlBtn, cancelBtn,
      progress, fill, progressText, msg, stats,
      minZoom: document.getElementById("tilesMinZoom"),
      maxZoom: document.getElementById("tilesMaxZoom"),
      estVal, estSub,
    };

    closeBtn.addEventListener("click", close);
    dom.recapture.addEventListener("click", refreshFromMap);
    dom.srcSelect.addEventListener("change", onSourceOrZoomChange);
    dom.minZoom.addEventListener("change", onSourceOrZoomChange);
    dom.maxZoom.addEventListener("change", onSourceOrZoomChange);
    dom.minZoom.addEventListener("input", updateEstimate);
    dom.maxZoom.addEventListener("input", updateEstimate);
    dom.dlBtn.addEventListener("click", startDownload);
    dom.cancelBtn.addEventListener("click", cancelDownload);
  }

  function makeLabel(text, id) {
    const s = el("span", "tiles-field-label");
    s.textContent = text;
    if (id) s.id = id;
    return s;
  }

  function makeZoomField(label, id) {
    const f = el("div", "tiles-field tiles-field-inline");
    f.appendChild(makeLabel(label));
    const input = el("input", "tiles-input");
    input.type = "number";
    input.id = id;
    input.min = HARD_ZOOM_FLOOR;
    input.max = HARD_ZOOM_CEIL;
    input.step = 1;
    f.appendChild(input);
    return f;
  }

  // ---- source list + cached stats ----
  function loadSources() {
    return Corvus.telemetry.requestJson("/api/tiles/sources")
      .then((data) => {
        sources = (data && data.sources) || [];
        renderSourceSelect();
        renderStats();
      })
      .catch(() => {
        sources = [];
        renderSourceSelect();
        renderStats();
      });
  }

  function renderSourceSelect() {
    const sel = dom.srcSelect;
    const prev = sel.value;
    sel.innerHTML = "";
    if (!sources.length) {
      const opt = el("option", ""); opt.value = ""; opt.textContent = "No sources";
      sel.appendChild(opt);
      sel.disabled = true;
      return;
    }
    sel.disabled = false;
    sources.forEach((s) => {
      const opt = el("option", "");
      opt.value = s.id;
      opt.textContent = s.label || s.id;
      sel.appendChild(opt);
    });
    if (prev && sources.some((s) => s.id === prev)) sel.value = prev;
    else sel.value = sources[0].id;
  }

  function renderStats() {
    const host = dom.stats;
    host.innerHTML = "";
    if (!sources.length) {
      const empty = el("div", "tiles-stat-empty"); empty.textContent = "No cache info";
      host.appendChild(empty);
      return;
    }
    sources.forEach((s) => {
      const row = el("div", "tiles-stat-row");
      const left = el("div", "tiles-stat-left");
      const name = el("span", "tiles-stat-name"); name.textContent = s.label || s.id;
      const meta = el("span", "tiles-stat-meta");
      const cnt = s.cached_count || 0;
      // Cached range now lives in cached_minzoom/cached_maxzoom (null when the
      // cache is empty); minzoom/maxzoom are the source floor/cap and are
      // always numbers, so they must NOT be used for the cached-range display.
      const hasRange = typeof s.cached_minzoom === "number" && typeof s.cached_maxzoom === "number";
      meta.textContent = cnt > 0
        ? `${fmtCount(cnt)} tiles` + (hasRange ? ` · z${s.cached_minzoom}–${s.cached_maxzoom}` : "")
        : "not cached";
      left.appendChild(name); left.appendChild(meta);
      // Clear-cache endpoint does not exist yet: disabled with an explanatory
      // tooltip so the row still reads as actionable-in-future (apple-design §6).
      const clearBtn = Corvus.ui.button({ variant: "ghost", size: "sm", icon: "trash-2", disabled: true });
      clearBtn.title = "Clearing not yet supported";
      clearBtn.setAttribute("aria-label", "Clear cache (not yet supported)");
      row.appendChild(left); row.appendChild(clearBtn);
      host.appendChild(row);
    });
  }

  // ---- open / close / refresh ----
  function open() {
    refreshFromMap();
    popoverEl.hidden = false;
    if (triggerEl) triggerEl.classList.add("active");
    loadSources();   // refresh cached_count (may have changed since last open)
  }

  function close() {
    popoverEl.hidden = true;
    if (triggerEl) triggerEl.classList.remove("active");
    // A running job keeps streaming in the background; only tear down the UI
    // surface. The EventSource is closed when the job ends or on re-open.
  }

  function toggle() { popoverEl.hidden ? open() : close(); }

  function refreshFromMap() {
    const b = captureBounds();
    if (!b) {
      dom.bndValue.textContent = "Map not ready";
      dom.dlBtn.disabled = true;
      dom.estVal.textContent = "—";
      dom.estSub.textContent = "";
      return;
    }
    dom.bndValue.textContent =
      `W ${b.w.toFixed(2)}  S ${b.s.toFixed(2)}  E ${b.e.toFixed(2)}  N ${b.n.toFixed(2)}`;
    const cap = SOURCE_ZOOM_CAP;
    const minZ = Math.max(HARD_ZOOM_FLOOR, Math.min(b.zoom, cap));
    const maxZ = Math.min(b.zoom + ZOOM_SPAN, cap);
    dom.minZoom.value = minZ;
    dom.minZoom.max = cap;
    dom.maxZoom.value = maxZ;
    dom.maxZoom.max = cap;
    dom.dlBtn.disabled = false;
    updateEstimate();
  }

  function onSourceOrZoomChange() {
    // Clamp min <= max within the source cap; keep the estimate in sync.
    const cap = SOURCE_ZOOM_CAP;
    let lo = clampInt(dom.minZoom.value, HARD_ZOOM_FLOOR, cap);
    let hi = clampInt(dom.maxZoom.value, HARD_ZOOM_FLOOR, cap);
    if (lo > hi) { const t = lo; lo = hi; hi = t; }
    dom.minZoom.value = lo;
    dom.maxZoom.value = hi;
    updateEstimate();
  }

  function clampInt(v, lo, hi) {
    const n = parseInt(v, 10);
    if (!isFinite(n)) return lo;
    return Math.max(lo, Math.min(hi, n));
  }

  function updateEstimate() {
    if (!captured) return;
    const lo = clampInt(dom.minZoom.value, HARD_ZOOM_FLOOR, SOURCE_ZOOM_CAP);
    const hi = clampInt(dom.maxZoom.value, HARD_ZOOM_FLOOR, SOURCE_ZOOM_CAP);
    const count = estimateTileCount(captured.w, captured.s, captured.e, captured.n, lo, hi);
    dom.estVal.textContent = `≈ ${fmtCount(count)} tiles`;
    dom.estSub.textContent = `~ ${fmtSize(count * AVG_TILE_KB)}`;
    if (count > 100000) showMsg("Very large region — consider a smaller zoom range.", "warn");
    else if (!dom.dlBtn.classList.contains("is-busy")) hideMsg();
  }

  // ---- download flow ----
  function startDownload() {
    if (!captured) return;
    const source = dom.srcSelect.value;
    const minzoom = clampInt(dom.minZoom.value, HARD_ZOOM_FLOOR, SOURCE_ZOOM_CAP);
    const maxzoom = clampInt(dom.maxZoom.value, HARD_ZOOM_FLOOR, SOURCE_ZOOM_CAP);
    if (!source || minzoom > maxzoom) return;

    Corvus.ui.setBusy(dom.dlBtn, true);
    dom.cancelBtn.disabled = false;
    dom.progress.hidden = false;
    dom.fill.style.width = "0%";
    dom.progressText.textContent = "0 / 0 (0%)";
    hideMsg();
    closeEventSource();

    Corvus.telemetry.requestJson("/api/tiles/download", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        source,
        bounds: { w: captured.w, s: captured.s, e: captured.e, n: captured.n },
        minzoom, maxzoom,
      }),
    }).then((data) => {
      const id = data && data.job_id;
      if (!id) throw new Error("No job id returned");
      jobId = id;
      openProgress(id);
    }).catch((err) => {
      failStart(err && err.message ? err.message : "Download request failed");
    });
  }

  function openProgress(id) {
    closeEventSource();
    const url = `/api/tiles/progress?id=${encodeURIComponent(id)}`;
    es = new EventSource(url);
    es.addEventListener("progress", (ev) => {
      let data;
      try { data = JSON.parse(ev.data); } catch (_) { return; }
      onProgress(data);
    });
    es.addEventListener("error", () => {
      // EventSource auto-reconnects; on a hard failure the progress event will
      // carry state "failed". If the stream dies without a terminal event,
      // surface a soft error so the operator is not left looking at 0%.
      if (es && es.readyState === EventSource.CLOSED) failStart("Progress stream closed");
    });
  }

  function onProgress(data) {
    if (!data) return;
    const done = data.done || 0;
    const total = data.total || 0;
    const pct = total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0;
    dom.fill.style.width = pct + "%";
    dom.progressText.textContent = `${done} / ${total} (${pct}%)`;
    const state = data.state;
    if (state === "done") finishOk(data);
    else if (state === "failed") finishFail(data);
    else if (state === "cancelled") finishCancelled(data);
  }

  function finishOk() {
    closeEventSource();
    Corvus.ui.setBusy(dom.dlBtn, false);
    dom.cancelBtn.disabled = true;
    showMsg("Download complete. Tiles cached for offline use.", "ok");
    loadSources();   // refresh cached_count
  }

  function finishFail(data) {
    closeEventSource();
    Corvus.ui.setBusy(dom.dlBtn, false);
    dom.cancelBtn.disabled = true;
    showMsg(`Download failed: ${(data && data.error) || "unknown error"}`, "err");
  }

  function finishCancelled() {
    closeEventSource();
    Corvus.ui.setBusy(dom.dlBtn, false);
    dom.cancelBtn.disabled = true;
    showMsg("Download cancelled.", "warn");
  }

  function failStart(message) {
    closeEventSource();
    Corvus.ui.setBusy(dom.dlBtn, false);
    dom.cancelBtn.disabled = true;
    dom.progress.hidden = true;
    showMsg(message, "err");
  }

  function cancelDownload() {
    if (!jobId) return;
    Corvus.ui.setBusy(dom.cancelBtn, true);
    Corvus.telemetry.requestJson("/api/tiles/cancel", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id: jobId }),
    }).catch(() => { /* best-effort; the progress stream reports the outcome */ })
      .finally(() => { Corvus.ui.setBusy(dom.cancelBtn, false); });
  }

  function closeEventSource() {
    if (es) { try { es.close(); } catch (_) {} es = null; }
  }

  function showMsg(text, kind) {
    dom.msg.hidden = false;
    dom.msg.textContent = text;
    dom.msg.className = "tiles-msg" + (kind ? " " + kind : "");
  }

  function hideMsg() { dom.msg.hidden = true; dom.msg.textContent = ""; }

  // ---- outside-click + Escape (consistent with the layers popover) ----
  function onDocClick(e) {
    if (popoverEl.hidden) return;
    if (e.target.closest(".tiles-trigger") || e.target.closest(".tiles-popover")) return;
    close();
  }

  function onDocKey(e) {
    if (e.key === "Escape" && !popoverEl.hidden) close();
  }

  function init(trigger, popover) {
    triggerEl = trigger;
    popoverEl = popover;
    if (!triggerEl || !popoverEl) return;
    buildPopover();
    triggerEl.addEventListener("click", () => toggle());
    document.addEventListener("click", onDocClick);
    document.addEventListener("keydown", onDocKey);
  }

  return {
    init,
    // exposed for testability / reuse
    estimateTileCount,
    loadSources,
  };
})();
