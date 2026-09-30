"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.tiles — offline-map download dialog.

  Contract:
    init(triggerEl) — wire the floating trigger on the map. Called from
      app.init AFTER Corvus.map.init so the map is available.
    useMap(adapter) — which map the dialog is reading and framing on. The
      Mission planner owns a SECOND MapLibre map and hands its own adapter
      over while that page is up; null goes back to the Home map. See
      "Two maps, one cache" below.
    Reads the current map view bounds from the active map's getBounds()
    (a MapLibre LngLatBounds: .getWest/.getSouth/.getEast/.getNorth), prefills
    minzoom = current zoom, maxzoom = current zoom + 4 (capped at the source
    cap), and estimates the slippy-map tile count + disk size client-side.
    POST /api/tiles/download starts a job; an EventSource streams progress from
    /api/tiles/progress?id=<job_id>; POST /api/tiles/cancel aborts it.

  TWO MAPS, ONE CACHE. The tiles live in the backend's .mbtiles store and are
  served to both maps through the same /api/tiles/<source>/z/x/y.png proxy, so
  an area downloaded while planning a mission is an area the Home map already
  has — there is no second copy and nothing to sync. What IS per map is the
  view: which bounds a download is framed on, and which map a region row
  frames. That is the only thing `useMap` switches. The drawn rectangles go to
  BOTH maps, because a download made on one is a fact about the other.

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
  const BIG_JOB_TILES = 20000;  // above this, warn before the operator commits
  // The server's per-job cap, until /api/tiles/sources says otherwise.
  const FALLBACK_MAX_TILES = 50000;
  const TERRAIN_PHASE_MSG = "Imagery done. Downloading elevation data…";

  let triggerEl = null;
  // Whichever button opened the dialog, so the pressed state goes back on the
  // right one: the Home map's trigger and the Mission map's are two buttons
  // for one dialog.
  let activeTrigger = null;
  // The map the dialog reads its bounds from and frames regions on. Null is
  // the Home map, which is the case for everything except the Mission page.
  let host = null;

  let sources = [];    // last /api/tiles/sources result
  let providers = [];  // service grouping from the same response
  // Elevation sources from the same response. Not offered in the layer
  // picker — a DEM is not something you look at — but a download can carry
  // one alongside the imagery, which is what makes 3D terrain work offline.
  let terrainSources = [];
  let regions = [];    // last /api/tiles/regions result
  let captured = null; // {w,s,e,n,zoom} captured from the map at open/recapture
  let es = null;       // unsubscribe from the shared stream, while a job runs
  let jobId = null;    // id of the imagery job (for cancel / re-attach)
  // The elevation job the backend starts alongside the imagery, or null. It
  // is followed once the imagery is done and cancelled together with it.
  let terrainJobId = null;
  // The second elevation job, for the model 3D is drawing from when that is
  // not the free one, or null. Followed and cancelled like the first.
  let terrainSourceJobId = null;
  // The free elevation model every download carries, from the catalogue.
  let defaultTerrainId = null;
  // Building blocks the backend queued for this download, fetched in the
  // background after the tile jobs; said in the completion message.
  let buildingBlocks = 0;
  let followId = null; // the job whose progress is on screen
  let jobState = null; // "running" until both jobs have ended, then the outcome
  let results = {};    // job id -> terminal progress snapshot
  let maxTilesPerJob = FALLBACK_MAX_TILES;
  let dialog = null;   // Corvus.ui.modal handle while the dialog is open
  let dom = {};        // populated by buildDialog

  // Web Mercator's latitude limit, the edge of every tile grid.
  const MERCATOR_LAT = 85.05112878;

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
    const dem = terrainSources.find((d) => d.id === id);
    // An elevation area is labelled for what it is rather than by a service
    // name: "Elevation" is what the operator chose when they ticked the box,
    // and the model's name tells two of them over the same ground apart.
    if (dem) return dem.label ? `Elevation · ${dem.label}` : "Elevation";
    return sourceLabel(sources.find((s) => s.id === id)) || id || "";
  }

  /** The DEM a download always carries (the backend's DEFAULT_TERRAIN), or
   *  null when none is served. */
  function terrainSpec() {
    return terrainSources.find((d) => d.id === defaultTerrainId) || terrainSources[0] || null;
  }

  /** Tiles of one elevation job over the captured area, zoomed like the
   *  backend's _start_terrain_companion: from zoom 0 (or as low as the
   *  per-job cap allows) up to the DEM's last level it has everywhere. */
  function demCount(dem, lo, hi) {
    const cap = Number.isFinite(dem.sparse_above) ? dem.sparse_above : dem.maxzoom;
    const top = Math.min(hi, dem.maxzoom, cap);
    let low = 0;
    while (low < Math.min(lo, top) && estimateTileCount(
      captured.w, captured.s, captured.e, captured.n, low, top) > maxTilesPerJob) {
      low += 1;
    }
    return estimateTileCount(captured.w, captured.s, captured.e, captured.n, low, top);
  }

  /** Every job of the current download, in the order they are followed. */
  function jobChain() {
    return [jobId, terrainJobId, terrainSourceJobId].filter(Boolean);
  }

  /** The elevation model 3D is drawing from right now, as the catalogue
   *  describes it, or null. The download carries it along with the free one. */
  function chosenTerrain() {
    const spec = Corvus.map && typeof Corvus.map.getTerrainSpec === "function"
      ? Corvus.map.getTerrainSpec() : null;
    const id = spec && spec.id;
    return (id && terrainSources.find((d) => d.id === id)) || null;
  }

  /** True when the operator has asked for elevation with this download. */
  function terrainWanted() {
    return !!(terrainSpec() && dom.terrainToggle && dom.terrainToggle.getValue());
  }

  /**
   * Show the elevation option only when the backend actually serves a DEM.
   * A tickbox for a capability that is not there is worse than no tickbox:
   * it promises offline terrain the field laptop would not have.
   */
  function renderTerrainOption() {
    if (!dom.terrainField) return;
    dom.terrainField.hidden = !terrainSpec();
    updateEstimate();
  }

  /** Whichever map the operator is looking at. */
  function mapHost() { return host || Corvus.map; }

  /** Every map that can draw the downloaded areas — not just the active one:
   *  a region downloaded while planning is one the Home map has too, and it
   *  must not have to be reopened to find that out. */
  function eachHost(fn) {
    const hosts = [];
    if (Corvus.map && typeof Corvus.map.setRegions === "function") hosts.push(Corvus.map);
    if (host && host !== Corvus.map && typeof host.setRegions === "function") hosts.push(host);
    hosts.forEach(fn);
  }

  /** Point the dialog at a map. `null` returns it to the Home map, which is
   *  what a page owning its own map must do on teardown — the adapter it
   *  handed over is dead the moment its map is removed. */
  function useMap(adapter) {
    host = (adapter && typeof adapter.getMap === "function") ? adapter : null;
  }

  /** True if the MapLibre map is ready for a bounds read. */
  function mapReady() {
    const owner = mapHost();
    return !!(owner && typeof owner.getMap === "function"
      && owner.getMap() && typeof owner.getMap().getBounds === "function");
  }

  /**
   * A view's bounds cut to one world, as {w,s,e,n}.
   *
   * MapLibre reports the view as drawn, world copies included: zoomed out, or
   * panned across the date line, a longitude can be -250 or 312, and the
   * backend refuses anything outside [-180, 180]. The view is shifted onto
   * the world copy its centre is in and cut at that copy's edges, and the
   * latitudes at Web Mercator's, beyond which there are no tiles.
   */
  function oneWorld(w, s, e, n) {
    if (!(e - w < 360)) return { w: -180, s: clampLat(s), e: 180, n: clampLat(n) };
    const shift = Math.round((w + e) / 2 / 360) * 360;
    return {
      w: Math.max(-180, w - shift), s: clampLat(s),
      e: Math.min(180, e - shift), n: clampLat(n),
    };
  }

  function clampLat(lat) { return Math.max(-MERCATOR_LAT, Math.min(MERCATOR_LAT, lat)); }

  /** Read + store the current view bounds; returns null if the map isn't ready. */
  function captureBounds() {
    if (!mapReady()) return null;
    const b = mapHost().getMap().getBounds();
    const z = mapHost().getMap().getZoom();
    const box = oneWorld(b.getWest(), b.getSouth(), b.getEast(), b.getNorth());
    if (![box.w, box.s, box.e, box.n].every(isFinite)) return null;
    captured = Object.assign(box, { zoom: Math.floor(z) });
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
      hint: "Optional. Defaults to the area's centre coordinates.",
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

    // Elevation. On by default: a pre-downloaded area without heights is flat
    // the moment the laptop leaves the network, and an operator who downloads
    // an area before driving out is exactly the operator who will be in 3D
    // with no way to fetch the missing DEM.
    const terrainToggle = Corvus.ui.toggle({
      value: true,
      ariaLabel: "Include elevation data",
      onChange: updateEstimate,
    });
    const terrainField = Corvus.ui.field({
      label: "Elevation (3D terrain)",
      control: terrainToggle.el,
      className: "field-switch",
      hint: "Downloads the height model for the same area, so 3D mode keeps its relief offline.",
    });
    terrainField.hidden = true;   // until the catalogue says a DEM is served
    body.appendChild(terrainField);

    // Buildings for 3D. On by default for the same reason as elevation: an
    // area flown offline has only the buildings someone happened to look at
    // online, which is some of them, never all.
    const buildingsToggle = Corvus.ui.toggle({
      value: true,
      ariaLabel: "Include buildings",
    });
    body.appendChild(Corvus.ui.field({
      label: "Buildings (3D)",
      control: buildingsToggle.el,
      className: "field-switch",
      hint: "Fetches the OpenStreetMap buildings of the area in the background, "
        + "nearest the centre first, so 3D shows them offline.",
    }));

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
      srcSelect, nameInput, bndValue, dlBtn, cancelBtn, terrainToggle, terrainField,
      buildingsToggle,
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
        terrainSources = (data && data.terrain) || [];
        defaultTerrainId = (data && data.default_terrain) || null;
        maxTilesPerJob = (data && data.max_tiles_per_job) || FALLBACK_MAX_TILES;
        renderSourceSelect();
        renderTerrainOption();
      })
      .catch(() => {
        sources = [];
        providers = [];
        terrainSources = [];
        renderSourceSelect();
        renderTerrainOption();
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
    // A service that serves nothing without a key (MapTiler, Mapbox) is only
    // offered once one is stored: without it every tile would be a 401, and
    // the server refuses the job anyway.
    const usable = sources.filter((s) => {
      const p = providers.find((x) => x.id === s.provider);
      return !(p && p.token_required && !p.token_set);
    });
    const options = usable.map((s) => ({ value: s.id, label: sourceLabel(s) }));
    // Keep an explicit choice the operator already made; otherwise follow the
    // map. Falls back to the first source when neither resolves.
    const prev = sel.value;
    const known = (id) => !!id && usable.some((s) => s.id === id);
    const owner = mapHost();
    const mapLayer = (owner && typeof owner.getBaseLayer === "function")
      ? owner.getBaseLayer()
      : null;
    Corvus.ui.setOptions(sel, options,
      known(prev) ? prev : (known(mapLayer) ? mapLayer : usable[0].id));
  }

  // ---- downloaded areas ----
  function loadRegions() {
    return Corvus.telemetry.requestJson("/api/tiles/regions")
      .then((data) => {
        regions = (data && data.regions) || [];
        renderRegions();
        // Every map overlay reads the same list, so they are refreshed
        // together and can never disagree about what is cached.
        eachHost((owner) => owner.setRegions(regions));
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
      const owner = mapHost();
      if (owner && typeof owner.fitBounds === "function") {
        owner.fitBounds(region.bounds);
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
    parts.push(`z${region.minzoom} to ${region.maxzoom}`);
    parts.push(region.state === "running"
      ? "downloading…"
      : `${fmtCount(region.tile_count || 0)} tiles`);
    if (region.state === "cancelled" || region.state === "failed") parts.push("incomplete");
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
      showMsg(`Deleted "${region.name}": ${fmtCount(freed)} tiles removed.`, "ok");
      return loadRegions();
    }).catch((err) => showMsg((err && err.message) || "Delete failed", "err"));
  }

  // ---- open / close / refresh ----
  function open(trigger) {
    if (dialog) return;
    activeTrigger = trigger || triggerEl;
    dialog = buildDialog();
    dialog.open();
    if (activeTrigger) activeTrigger.classList.add("active");

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
    if (followId && jobState === "running") {
      Corvus.ui.setBusy(dom.dlBtn, true);
      dom.cancelBtn.disabled = false;
      dom.progress.el.hidden = false;
      if (followId !== jobId) showMsg(TERRAIN_PHASE_MSG);
      openProgress(followId);
    }
  }

  function close() {
    if (dialog) dialog.close();   // onClose does the teardown
  }

  /** Called by the modal for every close path (button, scrim, Escape). */
  function onDialogClosed() {
    dialog = null;
    dom = {};
    if (activeTrigger) activeTrigger.classList.remove("active");
    activeTrigger = null;
    // A running job keeps streaming in the background; only the UI surface is
    // torn down. The stream is closed here and re-opened on the next open()
    // so it is not writing into detached nodes in the meantime.
    closeEventSource();
  }

  function toggle() { dialog ? close() : open(); }

  /** Is the dialog up? Read by a second trigger that has to decide whether its
   *  press means open or close. */
  function isOpen() { return !!dialog; }

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

  /** Tile count of the imagery job for the dialog's current area and zooms. */
  function imageryCount() {
    const cap = currentCap();
    const lo = clampInt(dom.minZoom.value, HARD_ZOOM_FLOOR, cap);
    const hi = clampInt(dom.maxZoom.value, HARD_ZOOM_FLOOR, cap);
    return estimateTileCount(captured.w, captured.s, captured.e, captured.n, lo, hi);
  }

  function updateEstimate() {
    if (!captured || !dom.estVal) return;
    const cap = currentCap();
    const lo = clampInt(dom.minZoom.value, HARD_ZOOM_FLOOR, cap);
    const hi = clampInt(dom.maxZoom.value, HARD_ZOOM_FLOOR, cap);
    const imagery = imageryCount();
    let count = imagery;
    // The elevation job is a second download over the same ground, capped at
    // the DEM's own maxzoom — so it has to be in the number the operator
    // decides on, not a surprise on their disk afterwards. It starts at zoom
    // 0 whatever the imagery starts at (the backend's
    // _start_terrain_companion), because 3D draws distant ground from coarse
    // elevation and derives missing fine tiles from it.
    const dem = terrainWanted() ? terrainSpec() : null;
    if (dem) {
      count += demCount(dem, lo, hi);
      // The model 3D is drawing from rides along too when it is another one.
      const chosen = chosenTerrain();
      if (chosen && chosen.id !== dem.id) count += demCount(chosen, lo, hi);
    }
    dom.estVal.textContent = `≈ ${fmtCount(count)} tiles`;
    dom.estSub.textContent = `~ ${fmtSize(count * AVG_TILE_KB)}`;
    // While a job runs the message line and the button belong to it.
    if (dom.dlBtn.classList.contains("is-busy")) return;
    // The server refuses an imagery job over its cap. Said here, before the
    // press, with the two settings that bring it down; the elevation job
    // starts only as low as the cap allows, so only the imagery count decides.
    const tooBig = imagery > maxTilesPerJob;
    dom.dlBtn.disabled = tooBig;
    if (tooBig) {
      showMsg(`Too many tiles for one download: ${fmtCount(imagery)}, the limit is ` +
        `${fmtCount(maxTilesPerJob)}. Choose a smaller area or a lower maximum zoom.`, "err");
    } else if (count > BIG_JOB_TILES) {
      showMsg("Very large region. Consider a smaller zoom range.", "warn");
    } else {
      hideMsg();
    }
  }

  // ---- download flow ----
  function startDownload() {
    if (!captured) return;
    const source = dom.srcSelect.value;
    const cap = capFor(source);
    const minzoom = clampInt(dom.minZoom.value, HARD_ZOOM_FLOOR, cap);
    const maxzoom = clampInt(dom.maxZoom.value, HARD_ZOOM_FLOOR, cap);
    if (!source || minzoom > maxzoom) return;
    if (imageryCount() > maxTilesPerJob) { updateEstimate(); return; }

    jobId = null;
    terrainJobId = null;
    terrainSourceJobId = null;
    followId = null;
    results = {};
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
        terrain: terrainWanted(),
        terrain_source: terrainWanted() && chosenTerrain() ? chosenTerrain().id : undefined,
        buildings: !!(dom.buildingsToggle && dom.buildingsToggle.getValue()),
      }),
    }).then((data) => {
      const id = data && data.job_id;
      if (!id) throw new Error("No job id returned");
      jobId = id;
      terrainJobId = (data && data.terrain_job_id) || null;
      terrainSourceJobId = (data && data.terrain_source_job_id) || null;
      buildingBlocks = Number(data && data.building_blocks) || 0;
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
    dropSubscriptions();
    followId = id;
    // The shared /api/events stream (js/events.js): a browser allows six
    // connections per origin, and this page is precisely the one that wants
    // the rest of them for tiles. `followJob` points the "tiles" topic at this
    // download — it is the one part of the stream's URL that changes, so a new
    // job does reconnect, which costs nothing because tile progress coalesces.
    Corvus.events.followJob(id);
    const offProgress = Corvus.events.subscribe("tiles", onProgress);
    const offError = Corvus.events.subscribe("error", () => {
      // The stream reconnects on its own; on a hard failure the progress event
      // carries state "failed". This is only for a stream that dies without a
      // terminal event, so the operator is not left looking at 0 %.
      if (jobState === "running") failStart("Progress stream closed");
    });
    es = function () { offProgress(); offError(); };
  }

  function onProgress(data) {
    if (!data) return;
    // One stream follows one job at a time; a late event for the job this
    // dialog has moved on from must not move the bar.
    if (data.job_id && data.job_id !== followId) return;
    if (dom.progress) dom.progress.set(data.done || 0, data.total || 0);
    const state = data.state;
    if (state !== "done" && state !== "failed" && state !== "cancelled") return;
    results[data.job_id || followId] = data;
    // The elevation jobs run alongside the imagery. The download is not
    // finished while the heights 3D mode needs offline are still coming in,
    // so each is followed in turn rather than left running unseen.
    const chain = jobChain();
    const next = chain[chain.indexOf(followId) + 1];
    if (state !== "cancelled" && chain.includes(followId) && next) {
      if (dom.progress) dom.progress.reset();
      showMsg(TERRAIN_PHASE_MSG);
      openProgress(next);
      loadRegions();
      return;
    }
    finishJobs();
  }

  /** Report the outcome of the imagery job and its elevation companions. */
  function finishJobs() {
    closeEventSource();
    resetButtons();
    const ended = jobChain().map((id) => results[id]).filter(Boolean);
    const imagery = results[jobId] || {};
    // A job "done" with failed tiles still has holes: the operator must hear
    // that before driving out, not find it in the field.
    const missing = ended.reduce((n, r) => n + (Number(r.failed) || 0), 0);
    if (ended.some((r) => r.state === "cancelled")) {
      jobState = "cancelled";
      showMsg("Download cancelled. Tiles fetched so far stay cached.", "warn");
    } else if (imagery.state === "failed") {
      jobState = "failed";
      showMsg(`Download failed: ${imagery.error || "unknown error"}`, "err");
    } else if (missing > 0) {
      jobState = "done";
      showMsg(`Download finished, but ${fmtCount(missing)} tiles could not be fetched. ` +
        "Start the same download again to fill the gaps.", "warn");
    } else {
      jobState = "done";
      showMsg("Download complete. Tiles cached for offline use." + (buildingBlocks
        ? ` Buildings for ${fmtCount(buildingBlocks)} blocks are still being fetched `
          + "in the background; leave Corvus running and online for a few minutes."
        : ""), "ok");
    }
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
    // Every job: cancelling only the imagery left the elevation downloads
    // running in the background after the operator had said stop.
    const ids = jobChain();
    Promise.all(ids.map((id) => Corvus.telemetry.requestJson("/api/tiles/cancel", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id }),
    }).catch(() => { /* best-effort; the progress stream reports the outcome */ })))
      .finally(() => {
        if (!dom.cancelBtn) return;
        Corvus.ui.setBusy(dom.cancelBtn, false);
        dom.cancelBtn.disabled = jobState !== "running";
      });
  }

  function dropSubscriptions() {
    // `es` is an unsubscribe from the shared stream, not a socket.
    if (es) { try { es(); } catch (_) {} es = null; }
  }

  function closeEventSource() {
    // The topic itself stays on the connection (see js/events.js on sticky
    // topics); dropping the job is what stops the server following it.
    dropSubscriptions();
    if (Corvus.events) Corvus.events.followJob(null);
  }

  function showMsg(text, kind) { if (dom.msg) dom.msg.show(text, kind); }
  function hideMsg() { if (dom.msg) dom.msg.hide(); }

  function init(trigger) {
    triggerEl = trigger;
    if (!triggerEl) return;
    // This trigger belongs to the Home map, so pressing it means that map —
    // whatever page last handed an adapter over and, in the failure case,
    // forgot to hand it back.
    triggerEl.addEventListener("click", () => { useMap(null); toggle(); });
  }

  return {
    init,
    useMap,
    open,
    close,
    isOpen,
    // exposed for testability / reuse
    estimateTileCount,
    loadSources,
    loadRegions,
  };
})();
