"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.anim — single shared requestAnimationFrame coordinator plus the
 * pure easing helpers used by map.js and instruments.js.
 *
 * Why a shared loop: telemetry over a serial radio arrives at ~5–10 Hz, so the
 * map marker and the flight instruments would visibly stutter if they snapped
 * to each new sample. Both renderers instead keep a TARGET (latest telemetry)
 * and a DISPLAYED value, and a SINGLE rAF loop advances every registered
 * animator one frame at a time. The loop self-cancels once all animators report
 * they have settled (so the field laptop does not spin a permanent 60 FPS loop
 * while the vehicle is idle) and is woken again the moment a new target lands.
 *
 * The easing is exponential approach (a frame-rate-independent, critically
 * damped ease-out — no overshoot, per apple-design defaults). Heading uses the
 * shortest angular path so 350° ↔ 10° animates through 0°, not back through 180°.
 *
 * prefers-reduced-motion: animators snap directly to the target and never wake
 * the loop, satisfying the apple-design reduced-motion contract.
 */
Corvus.anim = (function () {
  const animators = new Set();
  let rafId = null;
  let lastTime = 0;

  const hasRaf = () => typeof window !== "undefined" && typeof window.requestAnimationFrame === "function";
  const raf = (cb) => hasRaf()
    ? window.requestAnimationFrame(cb)
    : window.setTimeout(() => cb(now()), 16);
  const cancelRaf = (id) => {
    if (typeof window !== "undefined" && typeof window.cancelAnimationFrame === "function") {
      window.cancelAnimationFrame(id);
    } else {
      window.clearTimeout(id);
    }
  };
  const now = () => (typeof performance !== "undefined" && performance.now)
    ? performance.now()
    : Date.now();

  function frame(t) {
    rafId = null;
    const dt = lastTime ? Math.min((t - lastTime) / 1000, 0.05) : 0;
    lastTime = t;
    let alive = false;
    for (const a of Array.from(animators)) {
      let stillMoving = false;
      try { stillMoving = a.step(dt, t); } catch (_) { animators.delete(a); }
      if (stillMoving) alive = true;
    }
    if (alive) rafId = raf(frame);
    else lastTime = 0;
  }

  function add(animator) { animators.add(animator); return animator; }
  function remove(animator) { animators.delete(animator); }
  /** Start the loop if it is not already running. Idempotent. */
  function wake() { if (rafId == null) { lastTime = 0; rafId = raf(frame); } }
  function reducedMotion() {
    return !!(typeof window !== "undefined" && window.matchMedia
      && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
  }

  // ---- pure easing helpers (frame-rate independent, no overshoot) ----
  function lerp(a, b, t) { return a + (b - a) * t; }
  function approach(cur, target, dt, tau) {
    const k = 1 - Math.exp(-dt / Math.max(tau, 1e-4));
    return cur + (target - cur) * k;
  }
  function normAngle(a) { return ((a % 360) + 360) % 360; }
  /** Shortest signed delta from cur to target, in (-180, 180]. */
  function shortestDelta(cur, target) {
    return ((target - cur) % 360 + 540) % 360 - 180;
  }
  function approachAngle(cur, target, dt, tau) {
    return cur + shortestDelta(cur, target) * (1 - Math.exp(-dt / Math.max(tau, 1e-4)));
  }
  function settled(a, b, eps) { return Math.abs(a - b) <= eps; }
  function settledAngle(cur, target, eps) { return Math.abs(shortestDelta(cur, target)) <= eps; }

  return {
    add, remove, wake, reducedMotion,
    lerp, approach, normAngle, shortestDelta, approachAngle, settled, settledAngle,
    // test hooks
    _animators: () => animators,
    _rafId: () => rafId,
    _reset: () => { if (rafId != null) cancelRaf(rafId); rafId = null; lastTime = 0; animators.clear(); },
  };
})();

Corvus.map = (function () {
  let map = null;
  let vehicleMarker = null;
  let homeMarker = null;
  let pathSource = null;
  let pathCoords = [];
  // Autopilot uptime last seen. A value that moves BACKWARDS means the vehicle
  // rebooted, which is the only thing that should discard a flown track — a
  // link drop must not throw away the flight in progress.
  let lastBootMs = null;
  let started = false;
  let firstFix = true;

  // Waypoint planning ("Punktabflug") state — operator-clicked route points,
  // their parallel DOM markers, the dashed plan-line source, and the
  // subscription set the gui registers against to track the count / FLY state.
  let waypoints = [];          // {lat, lon}[] in entry order
  let wpMarkers = [];          // maplibregl.Marker[] parallel to waypoints
  let wpRouteSource = null;    // GeoJSON source of the dashed plan polyline
  let waypointMode = false;    // crosshair + click/right-click/Esc active
  const waypointSubs = new Set();

  // Interpolation state for the vehicle marker. TARGET = latest telemetry;
  // DISPLAYED = the value currently shown, eased toward TARGET each frame.
  let vehTarget = null;
  let vehDisplay = null;
  let vehAnimator = null;
  // Track retention. The old 500-point cap held under a minute of flight at
  // 5-10 Hz, so a track quietly ate its own beginning mid-sortie. Points are
  // decimated by DISTANCE instead — a hovering aircraft adds nothing, a moving
  // one adds a point every few metres — which makes the cap an hours-long
  // budget rather than a stopwatch.
  const TRACK_MIN_MOVE_M = 2.0;   // metres before a new point is recorded
  const TRACK_MAX_POINTS = 20000; // ~40 km of track at the spacing above

  const POS_TAU = 0.22;     // seconds — ease-out time constant for lng/lat
  const HDG_TAU = 0.18;     // seconds — ease-out time constant for heading
  const POS_EPS = 1e-7;     // ~1 cm; below this we consider the marker settled
  const HDG_EPS = 0.05;     // degrees

  // Where the map opens before a GPS fix arrives: the operating site at
  // Neubiberg. [lng, lat] — MapLibre's order, the reverse of how coordinates
  // are usually written down.
  const DEFAULT_CENTER = [11.640969, 48.080217];
  // All tile traffic routes through the backend serve endpoint so the map works
  // fully offline once tiles are cached. Online, the backend fetches+caches
  // transparently, so the online experience is unchanged. The browser never
  // talks to the internet directly.
  // MapLibre GL JS v4.7.1 is vendored locally at src/vendor/maplibre-gl.min.js
  // (offline fix: the field laptop has no internet, so the CDN load failed and
  // left #map empty). Keep this in sync with src/index.html and the vendor file.

  // The source catalogue is HYDRATED from GET /api/tiles/sources, which reads
  // corvus/tile_sources.py — the single registry. The frontend used to carry a
  // hand-copied mirror of every id, label, maxzoom, and attribution, which had
  // to be updated in lockstep with the Python side and was the one place a new
  // source could ship uncredited. BOOTSTRAP is the only entry still stated
  // here, because the map paints its first frame before any fetch can resolve.
  const BOOTSTRAP_LAYER = "satellite";
  const BOOTSTRAP = {
    id: BOOTSTRAP_LAYER,
    label: "Satellite",
    provider: "esri",
    style: "satellite",
    maxzoom: 19,
    attribution: "\u00a9 Esri, Maxar, Earthstar Geographics",
  };

  // id -> source descriptor, and the provider grouping used by the layer
  // switcher. Both stay at their bootstrap value if the fetch fails, so a
  // backend hiccup degrades to "satellite only" rather than a blank map.
  let sources = { [BOOTSTRAP_LAYER]: BOOTSTRAP };
  let providers = [];

  /** Descriptor for *key*, falling back to the bootstrap entry so a raster
   *  source is never added without a maxzoom or an attribution string. */
  function specFor(key) {
    return sources[key] || BOOTSTRAP;
  }

  // Which base layer is showing. Module-level so the config-load path,
  // setBaseLayer, and the Settings map-service picker all agree on it
  // independently of the buildControls closure.
  let activeLayer = BOOTSTRAP_LAYER;
  let layersPopoverEl = null;
  // Handle returned by Corvus.ui.optionList — it owns the "which layer is
  // active" highlight, so setBaseLayer never has to walk the DOM for it.
  let layerPicker = null;
  // The little clear-track control. Only in the DOM while a track exists.
  let trackClearEl = null;

  // Downloaded-area overlay: the named regions from GET /api/tiles/regions,
  // drawn as outlined rectangles with a DOM label at each centre. Labels are
  // markers rather than a MapLibre symbol layer on purpose — a text layer
  // needs a glyphs URL, and the field laptop has no internet to fetch fonts
  // from (the whole reason these regions exist).
  let regions = [];
  let regionSource = null;
  let regionMarkers = [];
  let regionsVisible = true;

  function tileUrl(key) {
    return "/api/tiles/" + key + "/{z}/{x}/{y}.png";
  }

  /**
   * The vehicle marker.
   *
   * Drawn as one SVG rather than stacked divs so every part scales together
   * and the heading stays exactly aligned with the body. The layers, outward
   * in: a heading cone that says which way the nose points, a white ring that
   * separates the marker from ANY imagery underneath it (the previous marker
   * had no outline and disappeared over red roofs), the coloured body, and a
   * centre dot marking the actual reported position — the thing an operator is
   * really reading when they ask "where is it".
   */
  function buildVehicleMarker() {
    const el = document.createElement("div");
    el.className = "vehicle-marker";
    el.innerHTML =
      '<div class="v-halo"></div>' +
      '<svg class="v-body" viewBox="0 0 48 48" aria-hidden="true">' +
        // Heading cone — a tapered wedge, wider than the old flat triangle so
        // the direction reads at a glance while the map is moving.
        '<path class="v-cone" d="M24 1 L31.5 15.5 A16 16 0 0 0 16.5 15.5 Z"/>' +
        '<circle class="v-ring" cx="24" cy="24" r="10.5"/>' +
        '<circle class="v-fill" cx="24" cy="24" r="9"/>' +
        '<circle class="v-centre" cx="24" cy="24" r="2.4"/>' +
      "</svg>";
    return el;
  }

  /**
   * The home / launch point.
   *
   * A landing-pad mark rather than a house glyph: "H" in a ring is what the
   * symbol means to anyone who flies, and it stays legible at the size this
   * renders at, where a house silhouette turned to mush. Crosshair ticks mark
   * the exact coordinate, because unlike the vehicle this marker is a place
   * the aircraft has to come back to precisely.
   *
   * Deliberately quieter than the vehicle: smaller, no halo. Where the
   * aircraft IS must win over where it started.
   */
  function buildHomeMarker() {
    const el = document.createElement("div");
    el.className = "home-marker";
    el.innerHTML =
      '<svg class="h-body" viewBox="0 0 32 32" aria-hidden="true">' +
        '<g class="h-ticks">' +
          '<line x1="16" y1="0.5" x2="16" y2="4.5"/>' +
          '<line x1="16" y1="27.5" x2="16" y2="31.5"/>' +
          '<line x1="0.5" y1="16" x2="4.5" y2="16"/>' +
          '<line x1="27.5" y1="16" x2="31.5" y2="16"/>' +
        "</g>" +
        '<circle class="h-ring" cx="16" cy="16" r="10"/>' +
        '<circle class="h-fill" cx="16" cy="16" r="8.6"/>' +
        // "H" drawn as strokes, not text: no font dependency, and it stays
        // crisp at any zoom.
        '<g class="h-glyph">' +
          '<line x1="12.4" y1="11.6" x2="12.4" y2="20.4"/>' +
          '<line x1="19.6" y1="11.6" x2="19.6" y2="20.4"/>' +
          '<line x1="12.4" y1="16" x2="19.6" y2="16"/>' +
        "</g>" +
      "</svg>";
    return el;
  }

  /**
   * The flown track: three stacked lines rather than one.
   *
   * A single stroke over satellite imagery is legible on grass and invisible
   * over a red-tiled roof or a ploughed field. The casing (a dark, wider line
   * underneath) gives the track an edge against ANY background, the glow lifts
   * it off the map, and the core carries the colour. This is the same trick
   * road maps use, and it is why the track stays readable while the aircraft
   * flies over mixed terrain.
   *
   * Colours come from --track (themes.css) resolved at build time; MapLibre
   * takes literals, so repaintTrack() re-reads them when the theme changes.
   */
  function addPathLayer() {
    map.addSource("vehicle-path", {
      type: "geojson",
      data: { type: "Feature", geometry: { type: "LineString", coordinates: [] }, properties: {} },
    });
    map.addLayer({
      id: "path-glow",
      source: "vehicle-path",
      type: "line",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-width": 11, "line-blur": 8, "line-opacity": 0.3 },
    });
    map.addLayer({
      id: "path-casing",
      source: "vehicle-path",
      type: "line",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-width": 5.5, "line-opacity": 0.55 },
    });
    map.addLayer({
      id: "path-line",
      source: "vehicle-path",
      type: "line",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-width": 2.6, "line-opacity": 0.95 },
    });
    pathSource = map.getSource("vehicle-path");
    repaintTrack();
  }

  /**
   * Metres between two [lng, lat] points — equirectangular, which is exact
   * enough at the scale of consecutive telemetry samples and far cheaper than
   * haversine at 10 Hz.
   */
  function metresBetween(a, b) {
    const R = 6371000;
    const lat1 = (a[1] * Math.PI) / 180;
    const lat2 = (b[1] * Math.PI) / 180;
    const dLat = lat2 - lat1;
    const dLng = ((b[0] - a[0]) * Math.PI) / 180;
    const x = dLng * Math.cos((lat1 + lat2) / 2);
    return Math.sqrt(x * x + dLat * dLat) * R;
  }

  /**
   * Append *position* to the track if the aircraft has actually moved.
   * Returns true when the track changed.
   *
   * Distance decimation is what lets the track cover a whole sortie: a hover
   * contributes one point, not six hundred, so the cap is an hours-long budget
   * instead of a stopwatch.
   */
  function recordTrackPoint(position) {
    if (!position || position.length < 2) return false;
    const lng = position[0], lat = position[1];
    if (!isFinite(lng) || !isFinite(lat)) return false;
    // [0,0] is the state store's "no fix yet" default, not the Gulf of Guinea.
    if (lng === 0 && lat === 0) return false;

    const last = pathCoords[pathCoords.length - 1];
    if (last && metresBetween(last, [lng, lat]) < TRACK_MIN_MOVE_M) return false;

    pathCoords.push([lng, lat]);
    // shift-on-overflow rather than slice: drop ONE point when over cap instead
    // of re-allocating the whole array on every sample.
    if (pathCoords.length > TRACK_MAX_POINTS) pathCoords.shift();
    return true;
  }

  /**
   * Has the autopilot rebooted since the last sample?
   *
   * time_boot_ms restarts near zero on boot, so a value that moves backwards
   * is a new flight session. Deliberately NOT keyed on the link: a dropped
   * radio would otherwise erase a flight that is still in the air.
   */
  function checkForReboot(bootMs) {
    if (typeof bootMs !== "number" || bootMs <= 0) return false;
    const previous = lastBootMs;
    lastBootMs = bootMs;
    return previous !== null && bootMs < previous;
  }

  /** Discard the flown track and repaint. */
  function clearTrack() {
    pathCoords = [];
    if (pathSource) {
      pathSource.setData({
        type: "Feature", geometry: { type: "LineString", coordinates: [] }, properties: {},
      });
    }
    updateTrackControl();
    // The plan line starts at the vehicle, not at the track, so it is
    // unaffected — but it shares the source update cadence, so keep it honest.
    if (waypoints.length > 0) updateRoute();
  }

  /** Show the clear-track control only while there is a track to clear. */
  function updateTrackControl() {
    if (!trackClearEl) return;
    trackClearEl.hidden = pathCoords.length < 2;
  }

  /** Push the current theme's track colours onto the three track layers. */
  /* Keep the map's drawing buffer at the screen's real resolution whatever
     the interface scale is. MapLibre sizes it from the container's UNSCALED
     size, so `zoom` alone would leave the map soft at 150% (the same buffer
     stretched over more screen) and oversampled at 80% (a bigger buffer than
     the screen can show — the frame cost of a scale nobody chose for
     performance). Multiplying the pixel ratio by the scale cancels both
     exactly: buffer = container/scale x ratio*scale = the native size.

     Guarded on setPixelRatio because the offline vendored MapLibre is pinned
     and a future bump must not be able to break the map over a sharpness
     detail. */
  function applyScale() {
    if (!map) return;
    const scale = Corvus.scale ? Corvus.scale.get() : 1;
    if (typeof map.setPixelRatio === "function") {
      map.setPixelRatio((window.devicePixelRatio || 1) * scale);
    } else if (typeof map.resize === "function") {
      map.resize();
    }
  }

  function repaintTrack() {
    if (!map) return;
    const track = Corvus.ui.token("--track", "#E4322F");
    // The casing is the page background, not black: on the light theme a black
    // outline would be heavier than the track it is meant to support.
    const casing = Corvus.ui.token("--bg", "#0B0E12");
    const set = (layer, prop, value) => {
      if (map.getLayer(layer)) map.setPaintProperty(layer, prop, value);
    };
    set("path-glow", "line-color", track);
    set("path-casing", "line-color", casing);
    set("path-line", "line-color", track);
  }

  function addRegionLayer() {
    map.addSource("offline-regions", {
      type: "geojson",
      data: { type: "FeatureCollection", features: [] },
    });
    // Below the flight track and the plan route: knowing where your tiles are
    // must never obscure where the aircraft is.
    map.addLayer({
      id: "offline-regions-fill",
      source: "offline-regions",
      type: "fill",
      paint: { "fill-color": "#4CC9FF", "fill-opacity": 0.06 },
    });
    map.addLayer({
      id: "offline-regions-line",
      source: "offline-regions",
      type: "line",
      layout: { "line-cap": "square", "line-join": "miter" },
      paint: {
        "line-color": "#4CC9FF",
        "line-width": 1.4,
        "line-opacity": 0.85,
        "line-dasharray": [4, 3],
      },
    });
    regionSource = map.getSource("offline-regions");
  }

  /** Closed [lng,lat] ring for a {w,s,e,n} bounds (GeoJSON needs the first
   *  point repeated at the end). */
  function boundsRing(b) {
    return [[
      [b.w, b.s], [b.e, b.s], [b.e, b.n], [b.w, b.n], [b.w, b.s],
    ]];
  }

  function buildRegionLabel(region) {
    const el = document.createElement("div");
    el.className = "region-label" + (region.state === "running" ? " downloading" : "");
    const name = document.createElement("span");
    name.className = "region-label-name";
    name.textContent = region.name || "Region";
    el.appendChild(name);
    const meta = document.createElement("span");
    meta.className = "region-label-meta";
    meta.textContent = region.state === "running"
      ? "downloading\u2026"
      : `z${region.minzoom}\u2013${region.maxzoom}`;
    el.appendChild(meta);
    return el;
  }

  /**
   * Replace the drawn set of downloaded areas. Safe before the map has loaded
   * (the list is stored and drawn on "load"), and safe to call repeatedly —
   * every marker from the previous call is removed first, so a region that
   * was renamed or deleted cannot leave a stale label behind.
   */
  function setRegions(list) {
    regions = Array.isArray(list) ? list.slice() : [];
    if (!map || !started || !regionSource) return;

    regionSource.setData({
      type: "FeatureCollection",
      features: regionsVisible ? regions.map((r) => ({
        type: "Feature",
        geometry: { type: "Polygon", coordinates: boundsRing(r.bounds || {}) },
        properties: { id: r.id, name: r.name || "" },
      })) : [],
    });

    regionMarkers.forEach((m) => m.remove());
    regionMarkers = [];
    if (!regionsVisible) return;
    regions.forEach((r) => {
      const b = r.bounds || {};
      const centre = [((b.w + b.e) / 2), ((b.s + b.n) / 2)];
      if (!isFinite(centre[0]) || !isFinite(centre[1])) return;
      regionMarkers.push(
        new maplibregl.Marker({ element: buildRegionLabel(r), anchor: "center" })
          .setLngLat(centre).addTo(map));
    });
  }

  /** Fetch the named areas from the backend and draw them. Failure is silent:
   *  the overlay is informational, and a missing backend already shows up
   *  everywhere else. */
  function loadRegions() {
    return Corvus.telemetry.requestJson("/api/tiles/regions").then((data) => {
      setRegions((data && data.regions) || []);
      return regions;
    }).catch(() => regions);
  }

  /** Show/hide the overlay without discarding the list. Returns the new state. */
  function setRegionsVisible(on) {
    regionsVisible = !!on;
    setRegions(regions);
    return regionsVisible;
  }

  /** Frame a {w,s,e,n} bounds in the viewport, with room around it. */
  function fitBounds(b) {
    if (!map || !b) return;
    const w = Number(b.w), s = Number(b.s), e = Number(b.e), n = Number(b.n);
    if (![w, s, e, n].every(isFinite)) return;
    map.fitBounds([[w, s], [e, n]], { padding: 60, duration: 700 });
  }

  function addWaypointRouteLayer() {
    map.addSource("waypoints-route", {
      type: "geojson",
      data: { type: "Feature", geometry: { type: "LineString", coordinates: [] }, properties: {} },
    });
    map.addLayer({
      id: "waypoints-route",
      source: "waypoints-route",
      type: "line",
      layout: { "line-cap": "butt", "line-join": "round" },
      paint: {
        "line-color": "#F5C842",
        "line-width": 2,
        "line-opacity": 0.9,
        "line-dasharray": [3, 2],
      },
    });
    wpRouteSource = map.getSource("waypoints-route");
  }

  function initialStyle(key) {
    const spec = specFor(key);
    const sources = {
      base: {
        type: "raster",
        tiles: [tileUrl(key)],
        tileSize: 256,
        maxzoom: spec.maxzoom,
        attribution: spec.attribution,
      },
    };
    const layers = [{ id: "base", type: "raster", source: "base" }];
    // No `glyphs`: every base layer is a raster and there are no symbol/text
    // layers, so fonts are unused. MapLibre may log a harmless warning.
    return { version: 8, sources, layers };
  }

  /** Swap the base raster layer. Safe to call before the map has loaded (the
   *  choice is remembered and applied on "load") and idempotent otherwise. */
  function setBaseLayer(key) {
    if (!sources[key] && key !== BOOTSTRAP_LAYER) return;
    activeLayer = key;
    // Sync the switcher before the early-out, so a choice made from Settings
    // while the map is still loading is already reflected when it appears.
    if (layerPicker) layerPicker.setValue(key);
    if (!map || !started) return;
    const spec = specFor(key);
    if (map.getLayer("base")) map.removeLayer("base");
    if (map.getSource("base")) map.removeSource("base");
    map.addSource("base", {
      type: "raster",
      tiles: [tileUrl(key)],
      tileSize: 256,
      maxzoom: spec.maxzoom,
      attribution: spec.attribution,
    });
    // Insert base BELOW "path-glow" so the track/waypoints stay on top.
    map.addLayer({ id: "base", type: "raster", source: "base" }, "path-glow");
  }

  function buildControls(container, layersPopover) {
    layersPopoverEl = layersPopover;
    const items = [
      { id: "in", icon: "plus", title: "Zoom in" },
      { id: "out", icon: "minus", title: "Zoom out" },
      { id: "center", icon: "crosshair", title: "Center on vehicle" },
      { id: "divider" },
      { id: "layers", icon: "layers", title: "Map layers" },
      { id: "regions", icon: "frame", title: "Show downloaded areas", active: true },
      { id: "three", icon: "box", title: "3D mode" },
    ];
    items.forEach((it) => {
      if (it.id === "divider") {
        const d = document.createElement("div");
        d.className = "mc-divider";
        container.appendChild(d);
        return;
      }
      // Same factory as every other icon button in the app; .mc-btn only
      // supplies the map-chrome surface, not the button's construction.
      const b = Corvus.ui.iconButton(it.icon, {
        className: "mc-btn",
        title: it.title,
        size: 17,
        variant: it.active ? "active" : undefined,
      });
      b.dataset.act = it.id;
      container.appendChild(b);
    });

    renderLayersPopover();

    container.addEventListener("click", (e) => {
      const b = e.target.closest(".mc-btn");
      if (!b) return;
      e.preventDefault();
      e.stopPropagation();
      const act = b.dataset.act;
      if (act === "in") map.zoomIn();
      else if (act === "out") map.zoomOut();
      else if (act === "center") centerOnVehicle(true);
      else if (act === "layers") {
        const open = !layersPopover.hidden;
        layersPopover.hidden = open;
        b.classList.toggle("active", !open);
      } else if (act === "regions") {
        b.classList.toggle("active", setRegionsVisible(!regionsVisible));
      } else if (act === "three") {
        const on = map.getPitch() < 10;
        map.easeTo({ pitch: on ? 50 : 0, duration: 500 });
        b.classList.toggle("active", on);
      }
    });

    document.addEventListener("click", (e) => {
      if (!e.target.closest(".map-controls") && !e.target.closest(".layers-popover")) {
        layersPopover.hidden = true;
        const lb = container.querySelector('[data-act="layers"]');
        if (lb) lb.classList.remove("active");
      }
    });
  }

  /**
   * The clear-track control: a very small, quiet button in the map's bottom-left
   * corner. It only exists in the DOM while there IS a track, and fades up on
   * hover — an always-visible button for an action taken once a flight would be
   * more prominent than the thing it acts on.
   */
  function buildTrackControl(mapEl) {
    if (!mapEl) return;
    trackClearEl = Corvus.ui.iconButton("trash-2", {
      className: "track-clear",
      title: "Clear the flown track",
      ariaLabel: "Clear the flown track",
      size: 12,
      onClick: clearTrack,
    });
    trackClearEl.hidden = true;
    mapEl.appendChild(trackClearEl);
    Corvus.ui.refreshIcons();
  }

  /**
   * (Re)build the layer switcher from the hydrated source catalogue. Layers are
   * grouped under their service (Esri / OpenStreetMap / Google / Bing) so the
   * twelve entries stay scannable and the operator can see which service a
   * layer belongs to without opening Settings. Called once from buildControls
   * with just the bootstrap entry, and again after the catalogue loads.
   */
  function renderLayersPopover() {
    if (!layersPopoverEl) return;
    Corvus.ui.clear(layersPopoverEl);

    const head = document.createElement("h4");
    head.textContent = "Map layers";
    layersPopoverEl.appendChild(head);

    // Fall back to a single ungrouped group before the catalogue arrives.
    const groups = providers.length
      ? providers
      : [{ id: BOOTSTRAP.provider, label: "", sources: [BOOTSTRAP_LAYER] }];

    // One picker across all groups so exactly one layer is ever marked active;
    // the group headings are interleaved into the same element afterwards.
    const options = [];
    groups.forEach((g) => {
      (g.sources || []).forEach((id) => {
        const s = sources[id];
        if (s) options.push({ id, label: s.label || id, group: g.label });
      });
    });

    layerPicker = Corvus.ui.optionList({
      ariaLabel: "Map layer",
      value: activeLayer,
      options,
      onChange: (id) => {
        setBaseLayer(id);
        // Persist the choice so it survives restarts. Fire-and-forget: a
        // failed save (backend busy/offline) must never break the switch.
        Corvus.telemetry.postAction("/api/config", {
          map: { base_layer: id, provider: specFor(id).provider },
        }).catch(() => {});
      },
    });

    // Insert a heading before the first item of each group. Skipped when there
    // is only one group (no point labelling a single section).
    if (groups.length > 1) {
      const items = Array.from(layerPicker.el.children);
      let cursor = 0;
      groups.forEach((g) => {
        const count = (g.sources || []).filter((id) => sources[id]).length;
        if (!count) return;
        const h = document.createElement("div");
        h.className = "layer-group";
        h.textContent = g.label;
        layerPicker.el.insertBefore(h, items[cursor]);
        cursor += count;
      });
    }

    layersPopoverEl.appendChild(layerPicker.el);
  }

  /**
   * Load the source catalogue from the backend and rebuild the layer switcher.
   * Failure is non-fatal: the bootstrap entry keeps the map painting, so the
   * worst case is a switcher with one option rather than a broken map.
   */
  function loadSources() {
    return Corvus.telemetry.requestJson("/api/tiles/sources").then((data) => {
      const list = (data && data.sources) || [];
      if (!list.length) return;
      const next = {};
      list.forEach((s) => { next[s.id] = s; });
      sources = next;
      providers = (data && data.providers) || [];
      renderLayersPopover();
      // The active layer's attribution/maxzoom may have been the bootstrap
      // fallback until now; re-apply so MapLibre credits the real source.
      if (started && activeLayer !== BOOTSTRAP_LAYER) setBaseLayer(activeLayer);
    }).catch(() => {});
  }

  /** A [lng, lat] pair with a real fix behind it, or null. [0,0] is the state
   *  store's "nothing yet" default, not a position off West Africa. */
  function realFix(pos) {
    if (!pos || pos.length < 2) return null;
    const lng = Number(pos[0]), lat = Number(pos[1]);
    if (!isFinite(lng) || !isFinite(lat)) return null;
    if (lng === 0 && lat === 0) return null;
    return [lng, lat];
  }

  /**
   * Centre the map on the aircraft.
   *
   * Falls back to the home point when the vehicle has no fix — after a link
   * drop, home is still the most useful place to be looking, and it is where
   * the aircraft will come back to. When there is nothing to centre on at all
   * the operator is told why: the button used to return silently on a dropped
   * link, which is indistinguishable from a broken button. It also had no
   * [0,0] guard, so a connected vehicle that had not yet acquired a fix flew
   * the map to the Gulf of Guinea.
   */
  function centerOnVehicle(animate) {
    if (!map) return false;
    const s = Corvus.telemetry.getState() || {};
    // The latest reported position (TARGET), not the displayed one — the
    // operator asked for where the aircraft is, not where the marker has eased
    // to.
    const target = (s.connected && realFix(s.position)) || realFix(s.home);
    if (!target) {
      const why = !s.connected
        ? "No vehicle connected — nothing to centre on."
        : "Waiting for a GPS fix — no position to centre on yet.";
      window.dispatchEvent(new CustomEvent("corvus:notification",
        { detail: { level: "info", message: why } }));
      return false;
    }
    if (animate) map.easeTo({ center: target, duration: 600 });
    else map.setCenter(target);
    return true;
  }

  /** Push the latest telemetry into the marker's TARGET and (re)start the loop. */
  function setVehicleTarget(state) {
    const lng = state.position[0];
    const lat = state.position[1];
    const heading = state.heading || 0;
    if (!vehTarget) {
      vehTarget = { lng, lat, heading };
      vehDisplay = { lng, lat, heading };   // first sample snaps (preserves old behavior)
    } else {
      vehTarget.lng = lng;
      vehTarget.lat = lat;
      vehTarget.heading = heading;
    }
    if (Corvus.anim.reducedMotion()) {
      vehDisplay = { lng, lat, heading };
      renderVehicle();
    } else {
      Corvus.anim.wake();
    }
  }

  /** Apply the DISPLAYED position/heading to the marker element. */
  function renderVehicle() {
    if (!vehicleMarker || !vehDisplay) return;
    vehicleMarker.setLngLat([vehDisplay.lng, vehDisplay.lat]);
    // The whole SVG rotates, so the heading cone and the body stay locked
    // together — they used to be separate elements and could disagree.
    const body = vehicleMarker.getElement().querySelector(".v-body");
    if (body) body.style.transform = `rotate(${vehDisplay.heading}deg)`;
  }

  function updateVehicle(state) {
    if (!vehicleMarker || !state.connected) return;
    setVehicleTarget(state);

    if (firstFix && state.position && state.position[0] !== 0 && state.position[1] !== 0) {
      // Only center on a REAL GPS fix: state.connected reflects the MAVLink
      // heartbeat (not GPS), and the state store defaults position to [0,0]
      // until a fix arrives, so centering on the first connected sample
      // would ease to "null island" at zoom 16 before the real fix lands.
      firstFix = false;
      map.easeTo({ center: state.position, zoom: 16, duration: 1000 });
    }

    // A reboot — and only a reboot — discards the track. A link drop must not,
    // or a radio glitch would erase the flight so far.
    if (checkForReboot(state.boot_ms)) clearTrack();

    // Track recording stays on the TARGET (latest telemetry) so the track is
    // accurate; interpolation is purely a rendering concern.
    if (recordTrackPoint(state.position)) {
      if (pathSource) {
        pathSource.setData({
          type: "Feature",
          geometry: { type: "LineString", coordinates: pathCoords },
          properties: {},
        });
      }
      updateTrackControl();
      // The plan line's first segment starts at the vehicle, so it must track
      // the vehicle too. Only when a plan exists — avoids repainting the route
      // source on every telemetry sample when no waypoints are planned.
      if (waypoints.length > 0) updateRoute();
    }

    if (state.home && state.home[0] !== 0 && homeMarker) {
      homeMarker.setLngLat(state.home);
    }
  }

  // ---- waypoint planning ("Punktabflug") ----

  /** Build the numbered DOM element for a waypoint marker (1-based index). */
  function buildWpMarker(idx) {
    const el = document.createElement("div");
    el.className = "wp-marker";
    const num = document.createElement("span");
    num.className = "wp-marker-num";
    num.textContent = String(idx);
    el.appendChild(num);
    return el;
  }

  /** Append a waypoint at [lng, lat], add its marker, refresh the route. */
  function addWaypoint(lng, lat) {
    if (!map || !started) return;
    waypoints.push({ lat, lon: lng });
    const idx = waypoints.length;            // 1-based label
    const m = new maplibregl.Marker({ element: buildWpMarker(idx), anchor: "center" })
      .setLngLat([lng, lat]).addTo(map);
    wpMarkers.push(m);
    updateRoute();
    notifyWaypoints();
  }

  /** Remove the most recently added waypoint (right-click). Earlier labels
   *  keep their numbers — only the tail is popped, so no renumbering. */
  function removeLastWaypoint() {
    if (!waypoints.length) return;
    waypoints.pop();
    const m = wpMarkers.pop();
    if (m) m.remove();
    updateRoute();
    notifyWaypoints();
  }

  /** Remove every waypoint + the planned route; notify subscribers with []. */
  function clearWaypoints() {
    waypoints = [];
    wpMarkers.forEach((m) => m.remove());
    wpMarkers = [];
    updateRoute();
    notifyWaypoints();
  }

  /** Current vehicle [lng, lat] when telemetry reports a real GPS fix, else
   *  null. Rendering-only: never exposed as a flyable waypoint, so
   *  getWaypoints/notifyWaypoints and the gotopoints payload stay
   *  operator-only. Mirrors the defensive `Corvus.telemetry && ...getState()`
   *  style used elsewhere so a not-yet-loaded telemetry module degrades to []. */
  function vehicleLngLat() {
    const s = Corvus.telemetry && Corvus.telemetry.getState();
    if (!s || !s.connected || !s.position) return null;
    const lng = s.position[0], lat = s.position[1];
    if (!isFinite(lng) || !isFinite(lat)) return null;
    if (lng === 0 && lat === 0) return null;   // no GPS fix
    return [lng, lat];
  }

  /** Plan-route coordinates: the vehicle position (when a real fix is
   *  available) prepended to the operator waypoints, in [lng, lat] form, so
   *  the first segment (drone -> wp1) renders even with a single waypoint. A
   *  LineString needs >= 2 points, so a lone waypoint without a vehicle fix
   *  collapses to [] rather than feeding MapLibre a 1-point array. Pure: no
   *  source mutation, safe in Node (wpRouteSource is null there). */
  function computePlanCoords(wps) {
    const list = wps || waypoints;
    if (!list.length) return [];
    const wpCoords = list.map((w) => [w.lon, w.lat]);
    const veh = vehicleLngLat();
    if (veh) return [veh, ...wpCoords];
    return wpCoords.length >= 2 ? wpCoords : [];
  }

  /** Push the plan-route coordinates into the dashed route source. The line
   *  starts at the vehicle's current position (when connected) so the drone
   *  -> wp1 segment is visible and follows the vehicle as it moves. */
  function updateRoute() {
    if (!wpRouteSource) return;
    wpRouteSource.setData({
      type: "Feature",
      geometry: { type: "LineString", coordinates: computePlanCoords() },
      properties: {},
    });
  }

  /** Notify subscribers with a fresh copy of the current waypoint list. */
  function notifyWaypoints() {
    const snap = waypoints.map((w) => ({ lat: w.lat, lon: w.lon }));
    waypointSubs.forEach((cb) => {
      try { cb(snap); } catch (_) { /* one bad subscriber must not break others */ }
    });
  }

  /** Current waypoints as {lat, lon}[] (fresh copy, entry order). */
  function getWaypoints() {
    return waypoints.map((w) => ({ lat: w.lat, lon: w.lon }));
  }

  /** Register a callback fired whenever the waypoint set changes. */
  function onWaypointsUpdate(cb) {
    if (typeof cb === "function") waypointSubs.add(cb);
  }

  // ---- offline tile-source introspection (best-effort) ----
  // Any error (network, 404, parse) collapses to an empty result. Not called
  // on load; invoke lazily when source/cache info is needed.

  /** List of tile sources the backend reports, or [] on any error. */
  async function getSources() {
    try {
      const res = await fetch("/api/tiles/sources");
      if (!res.ok) return [];
      const data = await res.json();
      return (data && data.sources) || [];
    } catch (_) {
      return [];
    }
  }

  /** Cache stats for *sourceId* from /api/tiles/sources, or null on any error.
   *  Without a sourceId, returns the whole source list. */
  async function getCacheStats(sourceId) {
    try {
      const res = await fetch("/api/tiles/sources");
      if (!res.ok) return null;
      const data = await res.json();
      const list = (data && data.sources) || [];
      if (!sourceId) return list;
      return list.find((s) => s.id === sourceId) || null;
    } catch (_) {
      return null;
    }
  }

  // Planning-mode handlers. Attached/detached as a set by setWaypointMode so
  // nothing leaks globally when planning is off. Each guards `waypointMode`
  // defensively — they are only registered while in mode, but the guard makes
  // a teardown race harmless. MapLibre markers stop pointer propagation, so
  // these never fire for clicks on vehicle/home/waypoint markers.
  function onPlanningClick(e) {
    if (!waypointMode || !e || !e.lngLat) return;
    addWaypoint(e.lngLat.lng, e.lngLat.lat);
  }

  function onPlanningRightClick(e) {
    if (!waypointMode) return;
    if (e && e.preventDefault) e.preventDefault();   // suppress browser menu
    removeLastWaypoint();
  }

  function onPlanningKey(e) {
    if (!waypointMode) return;
    if (e.key === "Escape" || e.key === "Esc") {
      if (e.preventDefault) e.preventDefault();
      setWaypointMode(false);
    }
  }

  /**
   * Toggle planning mode. On: cursor → crosshair, left-click appends a
   * waypoint, right-click removes the last, Esc exits. Off: handlers detach
   * but existing waypoints stay (clearWaypoints removes them). Idempotent.
   */
  function setWaypointMode(enabled) {
    enabled = !!enabled;
    if (waypointMode === enabled) return;
    if (!map || !started) return;   // planning needs a loaded map
    waypointMode = enabled;
    if (enabled) {
      map.getCanvas().style.cursor = "crosshair";
      map.on("click", onPlanningClick);
      map.on("contextmenu", onPlanningRightClick);
      document.addEventListener("keydown", onPlanningKey);
    } else {
      map.getCanvas().style.cursor = "";
      map.off("click", onPlanningClick);
      map.off("contextmenu", onPlanningRightClick);
      document.removeEventListener("keydown", onPlanningKey);
    }
  }

  function init(mapEl, controlsEl, layersPopover) {
    map = new maplibregl.Map({
      container: mapEl,
      center: DEFAULT_CENTER,
      zoom: 13,
      pitch: 0,
      bearing: 0,
      style: initialStyle("satellite"),
      // Added explicitly below so it can be placed bottom-LEFT; the default
      // control lands bottom-right, under the control rail.
      attributionControl: false,
      dragRotate: true,
      keyboard: false,
    });
    // Attribution is a legal requirement, so it is always on — but it belongs in
    // the corner nothing else competes for. Bottom-right is where the control
    // rail and the HUD live, so it goes bottom-left, with the clear-track button
    // stacked above it (see .track-clear in main.css).
    map.addControl(new maplibregl.AttributionControl(), "bottom-left");

    // Controls are built NOW, not on "load". MapLibre fires "load" only once the
    // style AND its first tiles have resolved, so building the rail there left
    // the operator staring at a map with no zoom, layer or centre buttons for as
    // long as the first tile fetch took — seconds on a cold cache, and much
    // worse offline. Nothing in the rail needs a loaded style: the handlers act
    // on the map object (which exists from the constructor), and setBaseLayer
    // already defers its own work until `started`.
    buildControls(controlsEl, layersPopover);
    buildTrackControl(mapEl);

    map.on("load", () => {
      // Regions first: their layers must sit UNDER the track and plan route,
      // and MapLibre stacks in insertion order.
      addRegionLayer();
      addPathLayer();
      addWaypointRouteLayer();

      homeMarker = new maplibregl.Marker({ element: buildHomeMarker(), anchor: "center" })
        .setLngLat(DEFAULT_CENTER).addTo(map);
      vehicleMarker = new maplibregl.Marker({ element: buildVehicleMarker(), anchor: "center", rotationAlignment: "map" })
        .setLngLat(DEFAULT_CENTER).addTo(map);

      // Register the marker animator with the shared loop. It returns true
      // while still easing toward the target and false once settled, which is
      // what lets the loop self-cancel when the vehicle is idle.
      vehAnimator = {
        step: function (dt) {
          if (!vehDisplay || !vehTarget) return false;
          const nx = Corvus.anim.approach(vehDisplay.lng, vehTarget.lng, dt, POS_TAU);
          const ny = Corvus.anim.approach(vehDisplay.lat, vehTarget.lat, dt, POS_TAU);
          const nh = Corvus.anim.approachAngle(vehDisplay.heading, vehTarget.heading, dt, HDG_TAU);
          const done =
            Math.abs(nx - vehTarget.lng) < POS_EPS &&
            Math.abs(ny - vehTarget.lat) < POS_EPS &&
            Corvus.anim.settledAngle(nh, vehTarget.heading, HDG_EPS);
          vehDisplay.lng = done ? vehTarget.lng : nx;
          vehDisplay.lat = done ? vehTarget.lat : ny;
          vehDisplay.heading = done ? vehTarget.heading : nh;
          renderVehicle();
          return !done;
        },
      };
      Corvus.anim.add(vehAnimator);

      Corvus.telemetry.subscribe(updateVehicle);
      started = true;
      // MapLibre paint properties are literal colours, so a theme switch has
      // to push new ones — the same reason the Plotly charts subscribe.
      Corvus.ui.onThemeChange(repaintTrack);
      applyScale();
      window.addEventListener("corvus:scalechange", applyScale);
      // Draw whatever regions arrived while the style was still loading, then
      // refresh from the backend.
      setRegions(regions);
      loadRegions();
      window.dispatchEvent(new Event("corvus:mapready"));
    });

    // Hydrate the source catalogue, then apply the operator's saved base layer.
    // Order matters: the catalogue has to land first, or a saved Google/Bing
    // layer would be rejected as unknown. The map already rendered the
    // bootstrap layer synchronously above, so a slow or failed fetch just
    // leaves that default — the map is never blank (offline-safe). Errors are
    // swallowed: /api/config may not exist yet or the backend may be busy.
    loadSources().then(() => Corvus.telemetry.requestJson("/api/config")).then((res) => {
      const key = res && res.config && res.config.map && res.config.map.base_layer;
      if (!key || !sources[key] || key === activeLayer) return;
      activeLayer = key;
      if (started) setBaseLayer(key);
      else window.addEventListener("corvus:mapready", () => setBaseLayer(key), { once: true });
    }).catch(() => {});
  }

  return {
    init,
    centerOnVehicle,
    getMap: () => map,
    isReady: () => started,
    setWaypointMode,
    getWaypoints,
    clearWaypoints,
    onWaypointsUpdate,
    getSources,
    getCacheStats,
    // Settings' map-service picker drives the live map through this, so the
    // Home tab's layer switcher and the Appearance page can never disagree
    // about which base layer is showing.
    setBaseLayer,
    getBaseLayer: () => activeLayer,
    // Downloaded-area overlay, driven by the offline-map dialog.
    clearTrack,
    getTrack: () => pathCoords.map((c) => c.slice()),
    setRegions,
    loadRegions,
    setRegionsVisible,
    getRegions: () => regions.slice(),
    fitBounds,
    // test hook: the hydrated source catalogue (id -> descriptor). Before
    // /api/tiles/sources resolves this holds only the bootstrap entry, which
    // is exactly what a test asserting the offline fallback wants to see.
    // Read-only; mirrors the `_animators` hook convention on Corvus.anim.
    _sources: () => sources,
    _bootstrap: () => BOOTSTRAP,
    // test hooks: the pure track rules — distance decimation and the reboot
    // edge — assertable without a map.
    _recordTrackPoint: (pos) => recordTrackPoint(pos),
    _realFix: (pos) => realFix(pos),
    _checkForReboot: (ms) => checkForReboot(ms),
    _resetTrackState: () => { pathCoords = []; lastBootMs = null; },
    // test hook: pure coordinate computation for the plan route (vehicle
    // position prefix + operator waypoints, or []). Accepts an optional
    // waypoint list for testing in Node, where the internal `waypoints` array
    // cannot be populated without a map; omit it to use the live waypoints.
    // Never touches wpRouteSource (null in Node). Mirrors the _sources convention.
    _planRouteCoords: (wps) => computePlanCoords(wps),
  };
})();
