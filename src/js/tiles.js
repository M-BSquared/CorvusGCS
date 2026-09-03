"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.tiles — offline-map download dialog.

  Contract:
    init(triggerEl) — wire the floating trigger on the map. Called from
      app.init AFTER Corvus.map.init so the map is available.
    Reads the current map view bounds from Corvus.map.getMap().getBounds()
    (a MapLibre LngLatBounds: .getWest/.getSouth/.getEast/.getNorth), prefills
    minzoom = current zoom, maxzoom = current zoom + 4 (capped at the source
    cap), and estimates the slippy-map tile count + disk size client-side.
    POST /api/tiles/download starts a job; an EventSource streams progress from
    /api/tiles/progress?id=<job_id>; POST /api/tiles/cancel aborts it.

  Why a dialog and not a popover: this panel used to be anchored to the map's
  top-right corner, where the flight-action bar, the HUD, the layer switcher
  and the right panel all took turns covering it. It is a modal now
  (Corvus.ui.modal) — centred, above every other surface, dismissed with
  Escape or a click on the scrim. A download in flight is NOT tied to the
  dialog: closing it leaves the job running, and re-opening re-attaches to the
  live progress stream.

  Named regions: every download is recorded in the source's .mbtiles as a
  named area (see corvus/tile_cache.py), so "what do I have offline?" has an
  answer beyond a tile count. The dialog lists them and the map draws them;
  each row can be renamed, framed on the map, or deleted.

  Every control is built with Corvus.ui (button / select / field / progress /
  message); the dialog reuses the shared .modal material. Version is never
  referenced here.

  Zoom caps are PER SOURCE. /api/tiles/sources reports each source's real
  upstream cap under "maxzoom" (19 for the ArcGIS/OSM/Bing layers, 20 for
  Google satellite) and its CACHED range under "cached_minzoom" /
  "cached_maxzoom" (null when the cache is empty). capFor() reads the fetched
  value and only falls back to the constant before the list has loaded.
*/
Corvus.tiles = (function () {
  const AVG_TILE_KB = 15;       // rough raster-tile size for the disk estimate
  const ZOOM_SPAN = 4;          // default maxzoom = current zoom + this
  const FALLBACK_ZOOM_CAP = 19; // used only until /api/tiles/sources responds
  const HARD_ZOOM_FLOOR = 0;
  const HARD_ZOOM_CEIL = 22;    // server rejects z > 22 on the serve path
  const BIG_JOB_TILES = 100000; // above this, warn before the operator commits

  let triggerEl = null;

  let sources = [];    // last /api/tiles/sources result
  let providers = [];  // service grouping from the same response
  let regions = [];    // last /api/tiles/regions result
  let captured = null; // {w,s,e,n,zoom} captured from the map at open/recapture
  let es = null;       // active EventSource for the running job
  let jobId = null;    // id of the running job (for cancel / re-attach)
  let jobState = null; // last known state of that job
  let dialog = null;   // Corvus.ui.modal handle while the dialog is open
  let dom = {};        // populated by buildDialog

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

  /** Upstream zoom cap for a source, from the fetched catalogue. Falls back to
   *  the conservative constant before the list has loaded, so the clamping
   *  math below never sees `undefined`. */
  function capFor(sourceId) {
    const s = sources.find((x) => x.id === sourceId);
    return (s && typeof s.maxzoom === "number") ? s.maxzoom : FALLBACK_ZOOM_CAP;
  }

  /** Cap for whatever the picker currently shows. */
  function currentCap() {
    return capFor(dom.srcSelect ? dom.srcSelect.value : null);
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

  /** ISO timestamp -> a short local date, or "" when unparseable. */
  function fmtDate(iso) {
    if (!iso) return "";
    const d = new Date(iso);
    return isNaN(d.getTime()) ? "" : d.toLocaleDateString();
  }

  function el(tag, cls) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    return e;
  }

  /** Provider label for *id*, or the raw id when the grouping is unavailable. */
  function providerLabel(id) {
    const p = providers.find((x) => x.id === id);
    return (p && p.label) || id || "";
  }

  /**
   * "Service · Layer" for a source. A bare "Satellite" is ambiguous now that
   * four services offer one. Where a service's only layer carries the service's
   * own name (OpenStreetMap), the prefix is dropped rather than stuttered.
   */
  function sourceLabel(s) {
    if (!s) return "";
    const layer = s.label || s.id;
    if (!providers.length) return layer;
    const prov = providerLabel(s.provider);
    return prov === layer ? layer : `${prov} · ${layer}`;
  }

  function sourceLabelById(id) {
    return sourceLabel(sources.find((s) => s.id === id)) || id || "";
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

  // ---- dialog DOM (rebuilt on every open; this module owns it) ----
  function buildDialog() {
    const body = document.createDocumentFragment();

    // Source picker. Changing it re-clamps the zoom range to that source's own
    // cap, which is why the handler is onSourceOrZoomChange, not updateEstimate.
    const srcSelect = Corvus.ui.select({
      id: "tilesSource",
      ariaLabel: "Tile source",
      onChange: onSourceOrZoomChange,
    });
    body.appendChild(Corvus.ui.field({ label: "Source", control: srcSelect }));

    // Name for this area. Optional — the backend falls back to the centre
    // coordinates — but naming it here is the difference between a list the
    // operator can act on and a list of numbers.
    const nameInput = Corvus.ui.input({
      id: "tilesRegionName",
      ariaLabel: "Name for this area",
      placeholder: "e.g. Landing site north",
      autocomplete: false,
    });
    body.appendChild(Corvus.ui.field({
      label: "Area name",
      control: nameInput,
      hint: "Optional — defaults to the area's centre coordinates.",
    }));

    // Captured bounds + recapture
    const bndField = el("div", "field");
    const bndHead = el("div", "tiles-bounds-head");
    bndHead.appendChild(Corvus.ui.label("Region (current view)"));
    bndHead.appendChild(Corvus.ui.iconButton("locate-fixed", {
      title: "Recapture current view",
      onClick: refreshFromMap,
    }));
    bndField.appendChild(bndHead);
    const bndValue = el("span", "tiles-bounds");
    bndField.appendChild(bndValue);
    body.appendChild(bndField);

    // Zoom range
    const minZoom = makeZoomInput("tilesMinZoom", "Minimum zoom");
    const maxZoom = makeZoomInput("tilesMaxZoom", "Maximum zoom");
    const zoomRow = el("div", "tiles-zoom-row");
    zoomRow.appendChild(Corvus.ui.field({ label: "Min zoom", control: minZoom, inline: true }));
    zoomRow.appendChild(Corvus.ui.field({ label: "Max zoom", control: maxZoom, inline: true }));
    body.appendChild(zoomRow);

    // Estimate
    const est = el("div", "tiles-estimate");
    est.appendChild(Corvus.ui.label("Estimate"));
    const estWrap = el("div", "tiles-estimate-val");
    const estVal = el("span", "tiles-estimate-value");
    const estSub = el("span", "tiles-estimate-sub");
    estWrap.appendChild(estVal); estWrap.appendChild(estSub);
    est.appendChild(estWrap);
    body.appendChild(est);

    // Progress + status message (both hidden until a job starts / reports)
    const progress = Corvus.ui.progress({ hidden: true });
    body.appendChild(progress.el);
    const msg = Corvus.ui.message();
    body.appendChild(msg.el);

    // Downloaded areas
    const regionsHead = el("div", "tiles-regions-head");
    regionsHead.appendChild(Corvus.ui.label("Downloaded areas"));
    const regionsCount = el("span", "tiles-regions-count");
    regionsHead.appendChild(regionsCount);
    body.appendChild(regionsHead);
    const regionList = el("div", "tiles-regions");
    body.appendChild(regionList);

    const dlBtn = Corvus.ui.button({
      variant: "primary", icon: "download", label: "Download", onClick: startDownload,
    });
    const cancelBtn = Corvus.ui.button({
      variant: "danger", icon: "x", label: "Cancel", disabled: true, onClick: cancelDownload,
    });

    dom = {
      srcSelect, nameInput, bndValue, dlBtn, cancelBtn,
      progress, msg, regionList, regionsCount, minZoom, maxZoom, estVal, estSub,
    };

    minZoom.addEventListener("change", onSourceOrZoomChange);
    maxZoom.addEventListener("change", onSourceOrZoomChange);
    minZoom.addEventListener("input", updateEstimate);
    maxZoom.addEventListener("input", updateEstimate);

    return Corvus.ui.modal({
      title: "Offline map",
      size: "lg",
      body,
      actions: [dlBtn, cancelBtn],
      onClose: onDialogClosed,
    });
  }

  function makeZoomInput(id, ariaLabel) {
    return Corvus.ui.input({
      id,
      type: "number",
      ariaLabel,
      mono: true,
      min: HARD_ZOOM_FLOOR,
      max: HARD_ZOOM_CEIL,
      step: 1,
    });
  }

  // ---- source list + cached stats ----
  function loadSources() {
    return Corvus.telemetry.requestJson("/api/tiles/sources")
      .then((data) => {
        sources = (data && data.sources) || [];
        providers = (data && data.providers) || [];
        renderSourceSelect();
      })
      .catch(() => {
        sources = [];
        providers = [];
        renderSourceSelect();
      });
  }

  /**
   * Fill the source picker. Options are ordered by service (the registry's own
   * order) and labelled "Service · Layer". The initial selection follows the
   * base layer currently on the map, so opening the dialog pre-downloads what
   * the operator is actually looking at — that is how the map service chosen
   * in Settings > Appearance reaches the downloader.
   */
  function renderSourceSelect() {
    const sel = dom.srcSelect;
    if (!sel) return;
    if (!sources.length) {
      Corvus.ui.setOptions(sel, [{ value: "", label: "No sources" }]);
      sel.disabled = true;
      return;
    }
    sel.disabled = false;
    const options = sources.map((s) => ({ value: s.id, label: sourceLabel(s) }));
    // Keep an explicit choice the operator already made; otherwise follow the
    // map. Falls back to the first source when neither resolves.
    const prev = sel.value;
    const known = (id) => !!id && sources.some((s) => s.id === id);
    const mapLayer = (Corvus.map && typeof Corvus.map.getBaseLayer === "function")
      ? Corvus.map.getBaseLayer()
      : null;
    Corvus.ui.setOptions(sel, options,
      known(prev) ? prev : (known(mapLayer) ? mapLayer : sources[0].id));
  }

  // ---- downloaded areas ----
  function loadRegions() {
    return Corvus.telemetry.requestJson("/api/tiles/regions")
      .then((data) => {
        regions = (data && data.regions) || [];
        renderRegions();
        // The map overlay reads the same list, so both are refreshed together
        // and can never disagree about what is cached.
        if (Corvus.map && typeof Corvus.map.setRegions === "function") {
          Corvus.map.setRegions(regions);
        }
        return regions;
      })
      .catch(() => {
        regions = [];
        renderRegions();
        return regions;
      });
  }

  function renderRegions() {
    const host = dom.regionList;
    if (!host) return;
    Corvus.ui.clear(host);
    if (dom.regionsCount) {
      dom.regionsCount.textContent = regions.length
        ? `${regions.length} area${regions.length === 1 ? "" : "s"}`
        : "";
    }
    if (!regions.length) {
      const empty = el("div", "tiles-stat-empty");
      empty.textContent = "Nothing downloaded yet. Frame an area on the map and download it.";
      host.appendChild(empty);
      return;
    }
    regions.forEach((r) => host.appendChild(regionRow(r)));
    Corvus.ui.refreshIcons();
  }

  function regionRow(region) {
    const row = el("div", "tiles-region-row" + (region.state === "running" ? " downloading" : ""));

    // The whole identity block is the "show me this area" affordance — a
    // separate locate button next to a name that does nothing would be worse.
    const locate = el("button", "tiles-region-main");
    locate.type = "button";
    locate.title = "Show this area on the map";
    const name = el("span", "tiles-region-name");
    name.textContent = region.name || "Region";
    const meta = el("span", "tiles-region-meta");
    meta.textContent = regionMeta(region);
    locate.appendChild(name);
    locate.appendChild(meta);
    locate.addEventListener("click", () => {
      if (Corvus.map && typeof Corvus.map.fitBounds === "function") {
        Corvus.map.fitBounds(region.bounds);
      }
      close();
    });
    row.appendChild(locate);

    const actions = el("div", "tiles-region-actions");
    actions.appendChild(Corvus.ui.iconButton("pencil", {
      title: "Rename this area",
      ariaLabel: `Rename ${region.name}`,
      onClick: () => renameRegion(region),
    }));
    actions.appendChild(Corvus.ui.iconButton("trash-2", {
      title: "Delete this area and its tiles",
      ariaLabel: `Delete ${region.name}`,
      onClick: () => deleteRegion(region),
    }));
    row.appendChild(actions);
    return row;
  }

  function regionMeta(region) {
    const parts = [sourceLabelById(region.source)];
    parts.push(`z${region.minzoom}–${region.maxzoom}`);
    parts.push(region.state === "running"
      ? "downloading…"
      : `${fmtCount(region.tile_count || 0)} tiles`);
    const date = fmtDate(region.created_at);
    if (date) parts.push(date);
    return parts.filter(Boolean).join(" · ");
  }

  function renameRegion(region) {
    if (typeof window.prompt !== "function") return;
    const next = window.prompt("Name for this area", region.name || "");
    if (next === null) return;                 // cancelled
    const name = next.trim();
    if (!name || name === region.name) return;
    Corvus.telemetry.requestJson("/api/tiles/regions/rename", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ source: region.source, id: region.id, name }),
    }).then(loadRegions)
      .catch((err) => showMsg((err && err.message) || "Rename failed", "err"));
  }

  function deleteRegion(region) {
    // Deleting frees disk, so it is worth a confirm. The backend keeps any
    // tile another region still covers, which is what the wording promises.
    if (typeof window.confirm === "function" &&
        !window.confirm(
          `Delete "${region.name}" and its cached tiles?\n\n` +
          "Tiles shared with another downloaded area are kept.")) {
      return;
    }
    Corvus.telemetry.requestJson("/api/tiles/regions/remove", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ source: region.source, id: region.id, delete_tiles: true }),
    }).then((res) => {
      const freed = (res && res.removed_tiles) || 0;
      showMsg(`Deleted "${region.name}" — ${fmtCount(freed)} tiles removed.`, "ok");
      return loadRegions();
    }).catch((err) => showMsg((err && err.message) || "Delete failed", "err"));
  }

  // ---- open / close / refresh ----
  function open() {
    if (dialog) return;
    dialog = buildDialog();
    dialog.open();
    if (triggerEl) triggerEl.classList.add("active");

    refreshFromMap();
    renderRegions();
    // Refresh the catalogue on every open: the base layer may have changed
    // since last time, and cached counts move as jobs finish. Re-clamp the
    // zoom range afterwards, because the source that just became selected may
    // have a different cap than the one refreshFromMap assumed.
    loadSources().then(() => onSourceOrZoomChange());
    loadRegions();
    // A job started before the dialog was last closed is still running in the
    // background; re-attach so its progress reappears instead of looking lost.
    if (jobId && jobState !== "done" && jobState !== "failed" && jobState !== "cancelled") {
      Corvus.ui.setBusy(dom.dlBtn, true);
      dom.cancelBtn.disabled = false;
      dom.progress.el.hidden = false;
      openProgress(jobId);
    }
  }

  function close() {
    if (dialog) dialog.close();   // onClose does the teardown
  }

  /** Called by the modal for every close path (button, scrim, Escape). */
  function onDialogClosed() {
    dialog = null;
    dom = {};
    if (triggerEl) triggerEl.classList.remove("active");
    // A running job keeps streaming in the background; only the UI surface is
    // torn down. The stream is closed here and re-opened on the next open()
    // so it is not writing into detached nodes in the meantime.
    closeEventSource();
  }

  function toggle() { dialog ? close() : open(); }

  function refreshFromMap() {
    if (!dom.bndValue) return;
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
    const cap = currentCap();
    dom.minZoom.value = Math.max(HARD_ZOOM_FLOOR, Math.min(b.zoom, cap));
    dom.minZoom.max = cap;
    dom.maxZoom.value = Math.min(b.zoom + ZOOM_SPAN, cap);
    dom.maxZoom.max = cap;
    dom.dlBtn.disabled = false;
    updateEstimate();
  }

  function onSourceOrZoomChange() {
    if (!dom.minZoom) return;
    // Clamp min <= max within the SELECTED source's cap and re-publish that cap
    // on the inputs. Both matter: the cap changes when the source changes (and
    // when the catalogue first loads, since refreshFromMap ran before it), so
    // updating only the values would leave the spinners offering zooms the
    // upstream cannot serve.
    const cap = currentCap();
    let lo = clampInt(dom.minZoom.value, HARD_ZOOM_FLOOR, cap);
    let hi = clampInt(dom.maxZoom.value, HARD_ZOOM_FLOOR, cap);
    if (lo > hi) { const t = lo; lo = hi; hi = t; }
    dom.minZoom.value = lo;
    dom.maxZoom.value = hi;
    dom.minZoom.max = cap;
    dom.maxZoom.max = cap;
    updateEstimate();
  }

  function clampInt(v, lo, hi) {
    const n = parseInt(v, 10);
    if (!isFinite(n)) return lo;
    return Math.max(lo, Math.min(hi, n));
  }

  function updateEstimate() {
    if (!captured || !dom.estVal) return;
    const cap = currentCap();
    const lo = clampInt(dom.minZoom.value, HARD_ZOOM_FLOOR, cap);
    const hi = clampInt(dom.maxZoom.value, HARD_ZOOM_FLOOR, cap);
    const count = estimateTileCount(captured.w, captured.s, captured.e, captured.n, lo, hi);
    dom.estVal.textContent = `≈ ${fmtCount(count)} tiles`;
    dom.estSub.textContent = `~ ${fmtSize(count * AVG_TILE_KB)}`;
    if (count > BIG_JOB_TILES) showMsg("Very large region — consider a smaller zoom range.", "warn");
    else if (!dom.dlBtn.classList.contains("is-busy")) hideMsg();
  }

  // ---- download flow ----
  function startDownload() {
    if (!captured) return;
    const source = dom.srcSelect.value;
    const cap = capFor(source);
    const minzoom = clampInt(dom.minZoom.value, HARD_ZOOM_FLOOR, cap);
    const maxzoom = clampInt(dom.maxZoom.value, HARD_ZOOM_FLOOR, cap);
    if (!source || minzoom > maxzoom) return;

    Corvus.ui.setBusy(dom.dlBtn, true);
    dom.cancelBtn.disabled = false;
    dom.progress.el.hidden = false;
    dom.progress.reset();
    hideMsg();
    closeEventSource();

    Corvus.telemetry.requestJson("/api/tiles/download", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        source,
        name: dom.nameInput.value.trim(),
        bounds: { w: captured.w, s: captured.s, e: captured.e, n: captured.n },
        minzoom, maxzoom,
      }),
    }).then((data) => {
      const id = data && data.job_id;
      if (!id) throw new Error("No job id returned");
      jobId = id;
      jobState = "running";
      openProgress(id);
      // The area is recorded the moment the job exists, so it appears on the
      // map as "downloading" rather than only once it finishes.
      loadRegions();
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
    jobState = data.state || jobState;
    if (dom.progress) dom.progress.set(data.done || 0, data.total || 0);
    if (data.state === "done") finishOk();
    else if (data.state === "failed") finishFail(data);
    else if (data.state === "cancelled") finishCancelled();
  }

  function finishOk() {
    closeEventSource();
    resetButtons();
    showMsg("Download complete. Tiles cached for offline use.", "ok");
    loadRegions();
  }

  function finishFail(data) {
    closeEventSource();
    resetButtons();
    showMsg(`Download failed: ${(data && data.error) || "unknown error"}`, "err");
    loadRegions();
  }

  function finishCancelled() {
    closeEventSource();
    resetButtons();
    showMsg("Download cancelled. Tiles fetched so far stay cached.", "warn");
    loadRegions();
  }

  function failStart(message) {
    closeEventSource();
    jobState = "failed";
    resetButtons();
    if (dom.progress) dom.progress.el.hidden = true;
    showMsg(message, "err");
  }

  /** Return the action row to its idle state. No-op once the dialog is gone. */
  function resetButtons() {
    if (!dom.dlBtn) return;
    Corvus.ui.setBusy(dom.dlBtn, false);
    dom.cancelBtn.disabled = true;
  }

  function cancelDownload() {
    if (!jobId) return;
    Corvus.ui.setBusy(dom.cancelBtn, true);
    Corvus.telemetry.requestJson("/api/tiles/cancel", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id: jobId }),
    }).catch(() => { /* best-effort; the progress stream reports the outcome */ })
      .finally(() => { if (dom.cancelBtn) Corvus.ui.setBusy(dom.cancelBtn, false); });
  }

  function closeEventSource() {
    if (es) { try { es.close(); } catch (_) {} es = null; }
  }

  function showMsg(text, kind) { if (dom.msg) dom.msg.show(text, kind); }
  function hideMsg() { if (dom.msg) dom.msg.hide(); }

  function init(trigger) {
    triggerEl = trigger;
    if (!triggerEl) return;
    triggerEl.addEventListener("click", () => toggle());
  }

  return {
    init,
    open,
    close,
    // exposed for testability / reuse
    estimateTileCount,
    loadSources,
    loadRegions,
  };
})();
