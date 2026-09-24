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

  // Follow mode: the map keeps the aircraft in view by itself.
  //
  // Not a hard lock on the centre — a camera welded to a 5-10 Hz telemetry
  // feed shivers, and every small correction the aircraft makes drags the
  // whole map with it. Instead the vehicle moves freely inside a dead-zone
  // rectangle in the middle of the viewport, and only crossing that edge pans
  // the map, which re-centres the aircraft and hands it a full box again.
  //
  // Dragging the map turns following OFF (the operator looking somewhere else
  // must not be yanked back), and the crosshair button turns it back on.
  let followMode = true;
  // True while OUR pan is in flight, so the samples arriving during it do not
  // each queue another one. Cleared on moveend.
  let followEasing = false;
  let controlsEl = null;
  // The place search, the same Corvus.mapSearch the planner mounts. This map
  // lives for the whole session, so the handle is only ever built once.
  let searchBox = null;

  // ---- The view the two maps share ------------------------------------
  //
  // There are two maps in this application and one piece of ground under
  // them. An operator who pans the Home map to tomorrow's site and then opens
  // the planner expects to be looking at that site — not at a second map with
  // its own idea of where "here" is. The planner opened on the aircraft, or
  // on a hardcoded fallback, or on whatever a reopened plan happened to fit,
  // which is three ways of showing a different place from the map next door.
  //
  // The rule, in one sentence: THE MISSION MAP OPENS ON THE VIEW YOU LAST
  // AIMED, on whichever of the two maps you aimed it.
  //
  // So the Home map records WHEN the operator last aimed it — a drag, a
  // wheel, the crosshair button, never a follow pan, because that is the
  // aircraft moving rather than the operator looking — and the Mission map
  // records where AND when. The more recent of the two is what the planner
  // opens on, which is what makes "move Home, then open Mission" land where
  // you were looking and "move Mission, leave, come back" land where you left
  // it.
  //
  // Nothing here ever moves the Home map. It stays alive and untouched behind
  // the planner, so returning to it shows the view it was left in — a planner
  // that wrote back into it would move the ground under the operator while
  // they were not looking at it.
  // A counter rather than a clock: two aims in the same millisecond are
  // ordinary (a wheel event lands inside a drag), a tie between them has no
  // right answer, and a laptop that syncs its clock mid-flight must not be
  // able to reorder them.
  let aimSeq = 0;
  let homeAimSeq = 0;       // operator-driven Home moves only
  let missionAimed = null;  // {center, zoom, bearing, seq} from the planner

  /** The Home map was just aimed BY THE OPERATOR. */
  function noteHomeAim() {
    homeAimSeq = ++aimSeq;
  }

  /** Where the operator left the Mission map, reported by mission.js after a
   *  move IT started. A programmatic fit, a fly-to or a search result is not
   *  an aim — the operator asked for a thing, not for a view. */
  function noteMissionView(view) {
    const v = view || {};
    const centre = Array.isArray(v.center) ? v.center.map(Number) : null;
    const zoom = Number(v.zoom);
    if (!centre || centre.length !== 2 || !centre.every(isFinite) || !isFinite(zoom)) return;
    const bearing = Number(v.bearing);
    missionAimed = {
      center: centre,
      zoom,
      bearing: isFinite(bearing) ? bearing : 0,
      seq: ++aimSeq,
    };
  }

  /** The camera the Mission map opens on, or null when there is no Home map
   *  to take one from (Node, or before init).
   *
   *  Pitch is deliberately not carried across: the planner draws a route on
   *  flat ground and a tilted plan is harder to place points on, whatever the
   *  Home map happens to be doing in 3D. */
  function missionOpenView() {
    if (missionAimed && missionAimed.seq > homeAimSeq) {
      return {
        center: missionAimed.center.slice(),
        zoom: missionAimed.zoom,
        bearing: missionAimed.bearing,
      };
    }
    if (!map) return null;
    const centre = map.getCenter();
    if (!centre || !isFinite(centre.lng) || !isFinite(centre.lat)) return null;
    return {
      center: [centre.lng, centre.lat],
      zoom: map.getZoom(),
      bearing: map.getBearing(),
    };
  }

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

  // Dead-zone rectangle, as a fraction of the map container. 40% leaves the
  // aircraft a generous middle to manoeuvre in while keeping it well clear of
  // the control rail and the flight HUD that overlay the map's edges.
  const FOLLOW_BOX_FRACTION = 0.4;
  // …but never smaller than this, or a tall narrow window would re-centre on
  // every twitch.
  const FOLLOW_BOX_MIN_PX = 140;
  // …and never so close to the edge that the aircraft cannot leave the box at
  // all, which would silently stop the map following.
  const FOLLOW_BOX_MAX_FRACTION = 0.8;
  const FOLLOW_EASE_MS = 700;
  // How far the camera may be aimed off the aircraft's ground point to bring
  // the AIRBORNE marker into the middle, as a fraction of the viewport. The
  // offset is perspective, so it is normally small; the clamp is for the case
  // it is not — a high aircraft at a close zoom can be a whole viewport above
  // its own shadow, and chasing that would put the ground the operator is
  // flying over off the bottom of the screen entirely. Past the clamp the
  // marker is allowed to sit high rather than the ground being thrown away.
  const FOLLOW_AIRBORNE_MAX_FRACTION = 0.35;
  // Beyond this many viewports from the centre the pan is a smear, not a
  // motion the eye can follow — cut instead. This is the reconnect case: the
  // aircraft was flown somewhere else while the link was down.
  const FOLLOW_JUMP_VIEWPORTS = 1;

  const POS_TAU = 0.22;     // seconds — ease-out time constant for lng/lat
  const HDG_TAU = 0.18;     // seconds — ease-out time constant for heading
  const POS_EPS = 1e-7;     // ~1 cm; below this we consider the marker settled
  const HDG_EPS = 0.05;     // degrees

  // Where the map opens before a GPS fix arrives: the operating site at
  // Neubiberg. [lng, lat] — MapLibre's order, the reverse of how coordinates
  // are usually written down.
  const DEFAULT_CENTER = [11.640969, 48.080217];

  /**
   * What the credit in the map's corner says, on both maps.
   *
   * Passing an options object at all is the point. MapLibre's default one
   * carries `customAttribution: '<a …>MapLibre</a>'`, so every map in the
   * application rendered the imagery credit, a pipe, and the word MapLibre —
   * a joined pair in which only the first half is a credit for anything ON
   * the map. The second is a credit for the renderer, and this application
   * already gives it properly: Settings > About > Credits lists MapLibre GL
   * JS by name, version, licence and URL (js/credits.js), which is where a
   * library belongs and where the BSD-3 notice actually has to live. Two
   * credits in the corner of a photograph, one of them redundant with a
   * dialog, is the caption reading worse for no gain.
   *
   * The imagery credit stays, on every layer, always: that is the one that is
   * a condition of the tiles being on screen at all, and it comes from each
   * source's own `attribution` (corvus/tile_sources.py). Nothing here can
   * suppress it, and nothing should try.
   *
   * `compact: true` is MapLibre's own default and is restated because it is
   * no longer inherited from the default object we stopped using.
   */
  const ATTRIBUTION_OPTIONS = { compact: true, customAttribution: [] };

  /**
   * Take the credit's open/closed state away from MapLibre, so it starts
   * folded and only a click unfolds it.
   *
   * MapLibre opens the credit by itself: once in onAdd when `compact` is set
   * explicitly, and again when a source's attribution text arrives a tick
   * later. Its own toggle is also spread over two writers, its click handler
   * and the native <summary> default, which is why collapsing "every open
   * that was not a click" kept closing real clicks too. Here the click is
   * handled once, in the capture phase, and both writers are stopped; any
   * other open is undone. A fold MapLibre makes on its own (it folds on map
   * drag) is accepted.
   *
   * @param {Element} attribEl the control's `.maplibregl-ctrl-attrib` <details>
   */
  function ownAttributionToggle(attribEl) {
    let shown = false;
    const apply = () => {
      if (attribEl.classList.contains("maplibregl-compact-show") !== shown) {
        attribEl.classList.toggle("maplibregl-compact-show", shown);
      }
      if (attribEl.hasAttribute("open") !== shown) {
        if (shown) attribEl.setAttribute("open", "");
        else attribEl.removeAttribute("open");
      }
    };
    attribEl.addEventListener("click", (e) => {
      const onButton = e.target && typeof e.target.closest === "function"
        && e.target.closest(".maplibregl-ctrl-attrib-button");
      // Folded, the whole disc is the button. Unfolded, only the icon is, so
      // the links in the credit still work.
      if (!onButton && shown) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      shown = !shown;
      apply();
    }, true);
    new MutationObserver(() => {
      if (shown && !attribEl.classList.contains("maplibregl-compact-show")) shown = false;
      apply();
    }).observe(attribEl, { attributes: true, attributeFilter: ["class", "open"] });
    apply();
  }

  /** The credit control, bottom-left, folded until clicked. Every map uses it. */
  function addAttribution(targetMap) {
    targetMap.addControl(new maplibregl.AttributionControl(ATTRIBUTION_OPTIONS), "bottom-left");
    const host = typeof targetMap.getContainer === "function" ? targetMap.getContainer() : null;
    const attribEl = host && host.querySelector(".maplibregl-ctrl-attrib");
    if (attribEl) ownAttributionToggle(attribEl);
  }
  // All tile traffic routes through the backend serve endpoint so the map works
  // fully offline once tiles are cached. Online, the backend fetches+caches
  // transparently, so the online experience is unchanged. The browser never
  // talks to the internet directly.
  // MapLibre GL JS v5.24.0 is vendored locally at src/vendor/maplibre-gl.min.js
  // (offline fix: the field laptop has no internet, so the CDN load failed and
  // left #map empty). Keep this in sync with src/index.html and the vendor file.
  //
  // v5 rather than v4 for the globe, and for two v4 bugs it retires: rendered
  // altitudes are measured from sea level instead of from the terrain under
  // the map centre, and the camera no longer has to be told the ground height
  // by hand (v4 left the zoom NaN doing it, which poisoned the transform).

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
  // How far the rail's popovers stand off the RAIL's own edge — not off the
  // button inside it, which is where the measurement has to start from. Both
  // the rail and the popovers are floating glass over the map, so the gap has
  // to read as a space between two objects rather than as a seam in one, and
  // no wider than that or they stop looking related at all.
  const MAP_RAIL_MENU_GAP = 8;
  // The layer switcher, as the app's dropdown: a Corvus.ui.menu opened from
  // the rail's layers button, built on first use.
  let layersMenuHandle = null;
  // The 3D panel and the switch inside it, one rail button down from the
  // layer switcher. It opens on hover rather than on press, because press is
  // already the mode's on/off — see wireThreeDPanel.
  let threeDMenuHandle = null;
  let threeDPicker = null;
  let threeDHoverTimer = null;
  // Long enough that a pointer travelling up the rail to the zoom buttons
  // does not leave a panel open behind it, short enough that a pointer that
  // stopped on the button is not left waiting.
  const THREE_D_HOVER_OPEN_MS = 220;
  // Longer, and for a different reason: there is a real gap between the rail
  // and the panel, and a surface that disappears while the pointer is
  // crossing it is a surface nobody can reach.
  const THREE_D_HOVER_CLOSE_MS = 320;
  // The little clear-track control. Only in the DOM while a track exists.
  let trackClearEl = null;

  // --- Map context menu (click a position, act on it) ---
  // The menu is anchored to a POINT, not to a corner, so it holds the lngLat
  // and re-projects on every map move rather than caching a pixel position:
  // panning under an open menu must slide it along with the ground it points
  // at, and a cached position would leave it pointing at a different place.
  //
  // The actions are injected by app.js (setContextActions) instead of being
  // written here, for the same reason the waypoint API is a contract: flight
  // commands carry the topbar's attempt tracking, the notification path and
  // the armed gating, all of which live in app.js. The map owns where and when
  // the menu appears; it does not own what the vehicle is asked to do.
  let contextActions = [];
  let contextMenuHandle = null; // the Corvus.ui.menu the actions are drawn in
  let contextPinEl = null;
  let contextPoint = null;      // {lng, lat} the open menu refers to, or null
  let contextHostEl = null;     // the map container the menu is positioned in

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
    // The sky is literal colours too, and it exists only while 3D is on.
    if (threeD) setSky(true);
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
      : `z${region.minzoom} to ${region.maxzoom}`;
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

    // Elevation rides along with an imagery download over the same ground, so
    // drawing it would put a second rectangle and a second label on top of
    // the first — the same area, claimed twice. The offline-map dialog still
    // lists it, which is where an operator manages what is on their disk.
    const drawn = regions.filter((r) => r.source !== terrainSourceId());

    regionSource.setData({
      type: "FeatureCollection",
      features: regionsVisible ? drawn.map((r) => ({
        type: "Feature",
        geometry: { type: "Polygon", coordinates: boundsRing(r.bounds || {}) },
        properties: { id: r.id, name: r.name || "" },
      })) : [],
    });

    regionMarkers.forEach((m) => m.remove());
    regionMarkers = [];
    if (!regionsVisible) return;
    drawn.forEach((r) => {
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
        // --plan, not a literal: the Mission planner draws its route in the
        // same amber, and the two are the same object seen on two screens.
        "line-color": Corvus.ui.token("--plan", "#FFC21A"),
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
    if (layersMenuHandle) layersMenuHandle.setLayerValue(key);
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
    // Insert base BELOW everything drawn ON it, so the track, the waypoints
    // and 3D mode's extruded buildings all stay on top. "path-glow" alone was
    // not enough once buildings existed: they are added while the bootstrap
    // base is showing, so a later re-insert before path-glow landed the
    // imagery on top of them and they vanished.
    map.addLayer({ id: "base", type: "raster", source: "base" }, firstOverlayLayer());
  }

  /** The lowest layer the base imagery must stay beneath, or undefined when
   *  none of them is in the style yet (before "load").
   *
   * EVERY overlay, in the order they are stacked, not just the track: the
   * offline-region rectangles are added FIRST (on "load", before the track),
   * so a list that started at "path-glow" re-inserted the imagery above them
   * and the downloaded areas disappeared the moment the operator switched
   * base layer — the one action most likely to be taken while deciding what
   * still needs downloading.
   */
  const OVERLAY_LAYERS = [
    "offline-regions-fill", "offline-regions-line",
    "buildings-3d",
    "path-glow", "path-casing", "path-line",
    "waypoints-route",
  ];

  function firstOverlayLayer() {
    return OVERLAY_LAYERS.find((id) => map.getLayer(id));
  }

  function buildControls(container) {
    controlsEl = container;
    const items = [
      { id: "in", icon: "plus", title: "Zoom in" },
      { id: "out", icon: "minus", title: "Zoom out" },
      { id: "center", icon: "crosshair", title: "Center on vehicle (resume following)",
        active: true },
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
        // The menu owns the button's own state through onOpen/onClose, so the
        // click only has to say which way it is going.
        if (layersMenu().isOpen()) layersMenu().close(false);
        else layersMenu().open({ el: b });
      } else if (act === "regions") {
        b.classList.toggle("active", setRegionsVisible(!regionsVisible));
      } else if (act === "three") {
        // Press is on/off, and nothing else. Which of the two 3D modes is
        // wanted is a setting, not a thing to cycle through on the way to the
        // one you meant — it lives in the panel that appears on hover.
        set3D(!threeD);
        persistThreeD();
      }
    });

    wireThreeDPanel(container.querySelector('[data-act="three"]'));
  }

  /**
   * The 3D button's second gesture: hover to reveal which 3D it will give you,
   * and change it there.
   *
   * Pressing is the common action and stays instant. Choosing between terrain
   * and the plain tilt is the rare one, and it is a SETTING — putting it on
   * the same press would mean walking through a mode you did not want on the
   * way to the one you did, in the middle of a flight.
   *
   * Three ways in, because hover alone is not a control:
   *
   *  * The pointer resting on the button. Delayed, so a pointer travelling
   *    across the rail to the zoom buttons does not leave a panel behind it.
   *  * Keyboard focus, which opens it WITHOUT taking the focus — the arrow
   *    keys reach into it from there (Corvus.ui.menu listens while open) and
   *    Escape closes it.
   *  * A long press or a right-click, which is the only one of the three a
   *    touch screen has. A field laptop with a touch display would otherwise
   *    have no way to reach this at all.
   *
   * Closing is delayed too, and cancelled by the pointer arriving on the
   * panel: the gap between the rail and the surface is real, and a panel that
   * vanishes while you are reaching for it is a panel you cannot use.
   */
  function wireThreeDPanel(btn) {
    if (!btn) return;
    btn.setAttribute("aria-haspopup", "true");
    btn.setAttribute("aria-expanded", "false");

    btn.addEventListener("mouseenter", () => scheduleThreeDPanel(true));
    btn.addEventListener("mouseleave", () => scheduleThreeDPanel(false));
    // Focus opens it; focus leaving for anywhere OUTSIDE the panel closes it,
    // so tabbing away tidies up while arrowing INTO it does not.
    btn.addEventListener("focus", () => openThreeDPanel());
    btn.addEventListener("blur", (e) => {
      const to = e && e.relatedTarget;
      if (to && threeDMenuHandle && withinNode(to, threeDMenuHandle.el)) return;
      closeThreeDPanel(true);
    });
    btn.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      openThreeDPanel();
    });

    // The same pair on the surface itself, so the pointer can cross the gap
    // and then stay as long as it likes.
    const surface = threeDMenu().el;
    surface.addEventListener("mouseenter", () => scheduleThreeDPanel(true));
    surface.addEventListener("mouseleave", () => scheduleThreeDPanel(false));
  }

  /** Is *node* inside *root*? Mirrors the same helper in ui.js, which is not
   *  exported — one `contains` with a null guard is cheaper than a new export. */
  function withinNode(node, root) {
    return !!(root && node && typeof root.contains === "function" && root.contains(node));
  }

  function clearThreeDHoverTimer() {
    if (threeDHoverTimer) window.clearTimeout(threeDHoverTimer);
    threeDHoverTimer = null;
  }

  /** Open or close the panel after the appropriate grace period. The pending
   *  timer is always replaced, so leaving and re-entering settles on the last
   *  thing the pointer actually did. */
  function scheduleThreeDPanel(open) {
    clearThreeDHoverTimer();
    threeDHoverTimer = window.setTimeout(
      () => { threeDHoverTimer = null; if (open) openThreeDPanel(); else closeThreeDPanel(false); },
      open ? THREE_D_HOVER_OPEN_MS : THREE_D_HOVER_CLOSE_MS);
  }

  function openThreeDPanel() {
    clearThreeDHoverTimer();
    const btn = controlsEl && controlsEl.querySelector('[data-act="three"]');
    if (!btn || threeDMenu().isOpen()) return;
    // autofocus:false — see Corvus.ui.menu. A panel the operator revealed by
    // pointing at a button must not move their keyboard.
    threeDMenu().open({ el: btn });
  }

  /** Close the panel. *refocus* puts the keyboard back on the button, which is
   *  right for Escape and wrong for the pointer simply moving away. */
  function closeThreeDPanel(refocus) {
    clearThreeDHoverTimer();
    if (threeDMenuHandle) threeDMenuHandle.close(!!refocus);
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

  /* OPTIONAL, and off unless Settings asks for it: how much of the map the
     flight bar may take before it gives up the button rhythm and then the
     captions. Half.

     What the bar does by DEFAULT is not this — it is the planner's rule, and
     the planner's rule is "fit": full-size buttons, full captions, until the
     row genuinely will not fit the room it has, and only then the two steps
     down. The two bars are the same control set at the same size under the
     same measurement; the planner's simply reaches the steps sooner because
     a 320px sidebar takes half of its window.

     This share is the switch on top of that — the same two steps, reached
     earlier, for an operator who would rather have the map than the words.
     It is not the bar's normal size and must never become it. */
  const FLIGHT_BAR_SHARE = 0.5;
  /* Whether that share is the rule at all. OFF until the config says
     otherwise, including before the config lands: the default is the full
     bar, so a boot that never hears from /api/config leaves the bar at the
     size it is supposed to be rather than at the size the switch would give
     it. */
  let flightBarShrink = false;
  /* The live fit, published for setFlightBarShrink. A no-op until the bar
     exists, so a Settings switch thrown at a half-built map does nothing
     rather than throwing. */
  let fitFlightBar = () => {};

  /**
   * Keep the flight bar inside the room the map column has.
   *
   * Two rules, one measurement. By default the room is .map-topleft's — the
   * box the bar sits in, which ends where the map's own controls begin. The
   * bar itself has no width cap, so it keeps its buttons at their own size and
   * simply steps down when the row will not fit that box; if even the icon
   * step will not, it floats over the chrome in one piece rather than coming
   * apart inside a cap. That is line for line the planner's rule (fitTools in
   * js/mission.js measures .mission-topleft the same way), and Corvus.ui.fitBar
   * is the planner's function, so the two bars behave alike rather than
   * agreeing by coincidence.
   *
   * With the optional shrink on, the room is instead a share of the MAP's
   * width (FLIGHT_BAR_SHARE). Either way it is a box OTHER than the bar that
   * is measured, because a bar that has already narrowed measures narrow and
   * would never widen again.
   *
   * Observed rather than listened for: the map column is resized by things the
   * window knows nothing about — the right-hand panel opening, the left rail
   * collapsing — and every one of them is a resize of this element.
   *
   * The window listener is NOT the ResizeObserver's fallback but its partner,
   * and the interface scale is why. A `zoom` change resizes this element in
   * its own pixels (a 990px map column becomes 510 at 150%) and fires no
   * ResizeObserver callback at all — the observer reports the box in the very
   * space the zoom just redefined, so nothing looks to it like a change. The
   * bar therefore kept its widest row in a column half the width and lay over
   * the map's search button. Corvus.scale dispatches a plain `resize` with
   * every change, so listening for both covers a box that moved and a box
   * whose pixels changed size under it.
   */
  function watchFlightBar(mapEl) {
    const bar = document.getElementById("flightActions");
    const host = mapEl && mapEl.parentNode;
    if (!bar) return;
    const fit = () => Corvus.ui.fitBar(bar, flightBarRoom(host, bar.parentNode));
    fitFlightBar = fit;
    fit();
    window.addEventListener("resize", fit);
    if (host && typeof ResizeObserver === "function") {
      new ResizeObserver(fit).observe(host);
    }
    // The captions are what the measurement is about, and a theme with wider
    // type changes them without anything being resized.
    if (Corvus.ui && typeof Corvus.ui.onThemeChange === "function") {
      Corvus.ui.onThemeChange(fit);
    }
  }

  /* The room to fit the bar into: .map-topleft's width, or a share of the map
     column when the optional shrink is on. Never the bar's own width — that is
     fitBar's fallback and it measures a bar that has already narrowed, which
     is why the bar has no cap of its own any more.

     A box that measures nothing (a hidden page, a map with no parent) returns
     null rather than 0: a room of zero would pin the bar at icons forever, and
     fitBar's fallback leaves a bar nobody can measure exactly as it is. */
  function flightBarRoom(host, room) {
    if (flightBarShrink) {
      const map = host ? host.clientWidth : 0;
      return map > 0 ? map * FLIGHT_BAR_SHARE : null;
    }
    const width = room ? room.clientWidth : 0;
    return width > 0 ? width : null;
  }

  /* Settings' switch. Applies immediately — the operator watches the bar while
     they throw it, which is the point of a switch about how the bar looks. */
  function setFlightBarShrink(on) {
    flightBarShrink = on !== false;
    fitFlightBar();
  }

  /**
   * The place search, in the rail's row, left of the offline-download
   * trigger — the same control in the same corner as the planner's, because
   * it is Corvus.mapSearch (js/map-search.js) and there is only one of it.
   *
   * It existed on the Mission map alone, which made "go to the next site by
   * name" something the operator could do while planning and not while
   * flying, on two screens showing the same ground. The offline half matters
   * more here than there, if anything: a coordinate pair out of a briefing is
   * how a field laptop with no network is told where to look.
   *
   * Mounted on the VIEW rather than on the map container, beside the
   * download trigger, so a press in the field is not also a press on the map.
   */
  function buildSearch(mapEl) {
    const host = mapEl && mapEl.parentNode;
    if (!host || !Corvus.mapSearch) return;
    searchBox = Corvus.mapSearch.create({
      container: host,
      map: () => map,
      regions: () => regions,
      // Searching for a place is the operator saying "look here instead",
      // which is exactly what dragging the map says — so it stops the
      // aircraft pulling the view back, and it is the view the planner opens
      // on. Neither follows from the fly-to itself: that is programmatic and
      // carries no originalEvent, so the movestart handler cannot see it.
      onGo: () => { setFollow(false); noteHomeAim(); },
    });
  }

  /**
   * The layer switcher, as the app's dropdown.
   *
   * It used to be a panel in index.html positioned with a top offset
   * hard-coded to where the layers button happened to sit in the rail — so it
   * drifted the moment the rail gained a button — and nothing but a click
   * somewhere else took it down. As a Corvus.ui.menu it hangs off the button
   * itself, dismisses on Escape like every other list the app opens, and
   * cannot be open at the same time as one of them.
   */
  function layersMenu() {
    if (layersMenuHandle) return layersMenuHandle;
    layersMenuHandle = createLayerMenu({
      rail: () => controlsEl,
      active: () => activeLayer,
      onSelect: setBaseLayer,
    });
    return layersMenuHandle;
  }

  /**
   * Build a layer switcher for a map rail — this one's, or another map's.
   *
   * The Mission planner has its own MapLibre map and its own rail, and an
   * operator switching layers there expects exactly what this rail does: the
   * same grouped list under the same service headings, the same surface, the
   * same dismissal, the same placement beside the rail, and the choice written
   * to the config so it survives a restart. That was a second, thinner copy
   * over there, and it behaved like none of those things.
   *
   * So the switcher is built here, once, and handed the only two things that
   * differ: which rail it hangs off, and what applies a layer to which map.
   * Everything the operator can see about it is the same by construction
   * rather than by two files agreeing.
   *
   * `spec.rail` is the rail element (or a function returning it, for a rail
   * that is rebuilt), `spec.active()` is the layer showing now, and
   * `spec.onSelect(id)` puts a layer on that caller's map. Persistence is NOT
   * the caller's: the base layer is the operator's map service, one setting
   * that both maps read at startup, so it is written from here.
   *
   * The returned handle is a Corvus.ui.menu with one addition: setLayerValue,
   * so a layer changed from somewhere else (Settings) is reflected in an open
   * list without the caller reaching into the picker.
   */
  function createLayerMenu(spec) {
    let picker = null;
    const railOf = () => (typeof spec.rail === "function" ? spec.rail() : spec.rail);

    /** The rail button reads as pressed while its menu is open. */
    function markButton(on) {
      const rail = railOf();
      const b = rail && rail.querySelector('[data-act="layers"]');
      if (!b) return;
      b.classList.toggle("active", !!on);
      b.setAttribute("aria-expanded", on ? "true" : "false");
    }

    function render(surface) {
      picker = null;
      const rows = renderLayerRows(surface, spec.active(), (id) => {
        spec.onSelect(id);
        // Persist the choice so it survives restarts. Fire-and-forget: a
        // failed save (backend busy/offline) must never break the switch.
        Corvus.telemetry.postAction("/api/config", {
          map: { base_layer: id, provider: specFor(id).provider },
        }).catch(() => {});
      }, (built) => { picker = built; });
      return rows;
    }

    const handle = Corvus.ui.menu({
      className: "layers-popover",
      // A group, not a menu: the rows inside are the radio group ui.optionList
      // builds, because exactly one layer is showing at a time.
      role: "group",
      ariaLabel: "Map layers",
      // Beside the rail rather than under it: the rail is against the right
      // edge of the map, and a list dropped below its layers button would
      // cover the buttons under it.
      side: "left",
      gap: () => railGapOf(railOf()),
      // The rail button is 34px wide; the layer names are not.
      matchAnchorWidth: false,
      render,
      onOpen: () => markButton(true),
      onClose: () => markButton(false),
    });
    handle.setLayerValue = (id) => { if (picker) picker.setValue(id); };
    return handle;
  }

  /**
   * The 3D switcher, as the app's dropdown.
   *
   * The same component and the same shape as the layer switcher above it,
   * because it is the same kind of question: one of a short list is showing,
   * and the operator is choosing which. It is a list rather than a cycle
   * through the three on a repeated press — a mode with a cost (elevation
   * tiles, building requests, a terrain mesh) should be picked deliberately
   * and should say what it costs, which is what the second line of each row
   * is for.
   */
  function threeDMenu() {
    if (threeDMenuHandle) return threeDMenuHandle;
    threeDMenuHandle = Corvus.ui.menu({
      className: "layers-popover three-d-popover",
      // A group, not a menu: what is inside is one setting, not a list of
      // actions to pick from.
      role: "group",
      ariaLabel: "3D mode",
      // Beside the rail, level with the cube button it belongs to — the same
      // placement the layer switcher above it uses, so the two rail popovers
      // behave alike. The gap is measured to the RAIL's edge rather than to
      // the button inside it; see railMenuGap.
      side: "left",
      gap: railMenuGap,
      matchAnchorWidth: false,
      // It opens on hover. Taking the keyboard because a pointer passed over
      // a button is not something the operator asked for.
      autofocus: false,
      render: renderThreeDRows,
      onOpen: () => markThreeButton(true),
      onClose: () => markThreeButton(false),
    });
    return threeDMenuHandle;
  }

  /**
   * Fill the open 3D panel: one switch, for the one thing the button itself
   * cannot say.
   *
   * There is no "off" row. Off is what the button does — a mode that can be
   * reached two ways is a mode the operator has to work out which way they
   * reached, and the panel exists to answer a different question: WHICH 3D
   * the button will give them.
   *
   * The hint is the point of the panel, not decoration. The two modes look
   * almost the same over flat ground; what actually separates them is that
   * one downloads elevation tiles and asks the backend for buildings and the
   * other touches the network not at all. On a field laptop over a radio link
   * that is the whole decision, and it is invisible unless it is written down.
   *
   * Built on every open, so a mode changed from anywhere else (a restored
   * config, a test) is simply already showing.
   */
  function renderThreeDRows(surface) {
    const head = document.createElement("div");
    head.className = "ui-menu-head";
    head.textContent = "3D mode";
    surface.appendChild(head);

    threeDPicker = Corvus.ui.toggle({
      value: threeDDetail === THREE_D_FULL,
      ariaLabel: "Terrain and buildings",
      onChange: (on) => {
        // Which 3D, never whether. The button is on/off and this is the
        // setting behind it, so flipping this while 3D is off records the
        // choice and leaves the map alone — the operator did not press the
        // button, and a map that tilts because a pointer wandered onto a
        // switch is a map that moved on its own.
        set3DDetail(on ? THREE_D_FULL : THREE_D_SIMPLE);
        persistThreeD();
      },
    });

    const field = Corvus.ui.field({
      label: "Terrain & buildings",
      control: threeDPicker.el,
      className: "field-switch",
      hint: "Elevation relief, extruded buildings and the globe. Off is the "
        + "camera tilt alone, over flat ground, and nothing downloaded "
        + "that a flat map does not already download.",
    });
    surface.appendChild(field);
    // The one row the keyboard walks. A menu with nothing to walk does not
    // open at all (see Corvus.ui.menu), so this is load-bearing.
    return [threeDPicker.el];
  }

  /**
   * The 3D rail button's state.
   *
   * Two different things, deliberately on two different attributes: .active
   * says the MODE is on, aria-expanded says the MENU is open. The layers
   * button next to it can put both on .active because its button has no state
   * of its own; this one does, and an operator watching the button go dark
   * when they closed the list without changing anything would be right to
   * read it as the mode having been turned off.
   */
  function markThreeButton(open) {
    const b = controlsEl && controlsEl.querySelector('[data-act="three"]');
    if (!b) return;
    b.classList.toggle("active", threeD);
    if (open !== undefined) b.setAttribute("aria-expanded", open ? "true" : "false");
    b.title = threeD
      ? (threeDDetail === THREE_D_FULL
        ? "3D: terrain & buildings" : "3D: simple")
      : "3D mode";
  }

  /**
   * The gap a rail popover opens with, measured from the RAIL rather than
   * from the button it hangs off.
   *
   * Corvus.ui.menu places a surface relative to its TRIGGER, and that trigger
   * is a 34 px button sitting inside the rail's own padding. Measuring from
   * the button spends most of the gap inside the rail, and what the operator
   * sees between the two glass edges is whatever is left — five pixels of a
   * twelve-pixel gap, which is why the surface looked stuck to the rail.
   * Adding the button's inset back puts the space where it shows.
   *
   * Read fresh on every placement (Corvus.ui.menu re-reads a function), so
   * any change to the rail's padding is simply accounted for rather than
   * mirrored here as a number.
   */
  function railMenuGap() {
    return railGapOf(controlsEl);
  }

  /** The same measurement for any rail, so a second map's rail opens its
   *  popovers at the same distance this one does.
   *
   *  Returned in UNSCALED pixels, because that is what Corvus.ui.menu places
   *  in. The two client rects are scaled, so their difference is divided back
   *  by the interface scale — an undivided inset is a gap that grows a second
   *  time with the interface and pushes the surface off its own rail. */
  function railGapOf(rail) {
    const btn = rail && rail.querySelector(".mc-btn");
    if (!rail || !btn || typeof rail.getBoundingClientRect !== "function") {
      return MAP_RAIL_MENU_GAP;
    }
    const k = (Corvus.ui && typeof Corvus.ui.uiScale === "function") ? Corvus.ui.uiScale() : 1;
    const inset = (btn.getBoundingClientRect().left - rail.getBoundingClientRect().left) / k;
    return MAP_RAIL_MENU_GAP + Math.max(0, inset);
  }

  /**
   * Fill the open switcher from the hydrated source catalogue. Layers are
   * grouped under their service (Esri / OpenStreetMap / Google / Bing) so the
   * twelve entries stay scannable and the operator can see which service a
   * layer belongs to without opening Settings. Built on every open, so a
   * catalogue that arrived since the last one is simply there.
   */
  function renderLayerRows(surface, active, onChange, onBuilt) {
    const head = document.createElement("div");
    head.className = "ui-menu-head";
    head.textContent = "Map layers";
    surface.appendChild(head);

    // Fall back to a single ungrouped group before the catalogue arrives. A
    // keyed service (MapTiler, Mapbox) with no saved key yet is left out here
    // rather than listed greyed out: those live tiles will not load without a
    // key, so offering them from the switcher just leads to a blank map. The
    // key is still asked for in Settings, where "Needs an API key" is the
    // right message to show.
    const groups = providers.length
      ? providers.filter((g) => !g.token_required || g.token_set)
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

    const picker = Corvus.ui.optionList({
      ariaLabel: "Map layer",
      value: active,
      options,
      onChange,
    });
    if (typeof onBuilt === "function") onBuilt(picker);

    // The rows, before the headings are interleaved between them: this is
    // what the keyboard walks, and a heading is not a stop on that walk.
    const items = Array.from(picker.el.children);

    // Insert a heading before the first item of each group. Skipped when there
    // is only one group (no point labelling a single section).
    if (groups.length > 1) {
      let cursor = 0;
      groups.forEach((g) => {
        const count = (g.sources || []).filter((id) => sources[id]).length;
        if (!count) return;
        const h = document.createElement("div");
        h.className = "layer-group";
        h.textContent = g.label;
        picker.el.insertBefore(h, items[cursor]);
        cursor += count;
      });
    }

    surface.appendChild(picker.el);
    return items;
  }

  /**
   * Load the source catalogue from the backend. Failure is non-fatal: the
   * bootstrap entry keeps the map painting, so the worst case is a switcher
   * with one option rather than a broken map. The switcher itself is built
   * from `sources` every time it opens, so there is nothing to re-render here
   * unless it is open right now.
   */
  function loadSources() {
    return Corvus.telemetry.requestJson("/api/tiles/sources").then((data) => {
      const list = (data && data.sources) || [];
      if (!list.length) return;
      const next = {};
      list.forEach((s) => { next[s.id] = s; });
      sources = next;
      providers = (data && data.providers) || [];
      // The DEM 3D mode reads heights from, including its encoding — stated
      // once in corvus/tile_sources.py and never mirrored here. A backend
      // that serves none leaves terrainSpec null, and 3D degrades to a tilt
      // over flat ground rather than failing.
      const dems = (data && data.terrain) || [];
      const wanted = (data && data.default_terrain) || (dems[0] && dems[0].id);
      terrainSpec = dems.find((d) => d.id === wanted) || dems[0] || null;
      // 3D may already be on: the rail works from the first frame, and this
      // catalogue arrives over the network. Re-applying the mode is how a
      // press that landed before the DEM was known still gets its terrain,
      // instead of a tilt over flat ground for the rest of the session.
      if (threeD) set3D(true);
      if (layersMenuHandle) layersMenuHandle.rebuild();
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
        ? "No vehicle connected. Nothing to centre on."
        : "Waiting for a GPS fix. No position to centre on yet.";
      window.dispatchEvent(new CustomEvent("corvus:notification",
        { detail: { level: "info", message: why } }));
      return false;
    }
    if (animate) map.easeTo({ center: target, duration: 600 });
    else map.setCenter(target);
    // Pressing the crosshair IS aiming the map, as much as dragging it is —
    // so the planner opens on the aircraft after it, rather than on wherever
    // the operator had been looking before.
    noteHomeAim();
    // Centring is also how the operator says "follow this again" — the button
    // that recovers the aircraft would otherwise hand back a view that stops
    // tracking it the moment it moves.
    setFollow(true);
    return true;
  }

  /**
   * Half the dead zone along one axis, in CSS pixels.
   *
   * Clamped at both ends: a floor so a small window does not re-centre on
   * every twitch, and a ceiling so the box can never fill the viewport (a box
   * the aircraft cannot leave is a map that never follows).
   */
  function followHalfExtent(size) {
    const box = Math.max(size * FOLLOW_BOX_FRACTION, FOLLOW_BOX_MIN_PX);
    return Math.min(box, size * FOLLOW_BOX_MAX_FRACTION) / 2;
  }

  /**
   * What the camera should do about a vehicle at (px, py) in a
   * width x height viewport: "hold", "ease" or "jump".
   *
   * Pure geometry — no map, no state — so the dead-zone rule is testable on
   * its own. An unprojectable point (behind the horizon in 3D mode, or a
   * degenerate container) holds: moving the map on a coordinate we do not
   * trust is worse than not moving it.
   */
  function followAction(px, py, width, height) {
    if (![px, py, width, height].every((n) => isFinite(n))) return "hold";
    if (width <= 0 || height <= 0) return "hold";
    const dx = Math.abs(px - width / 2);
    const dy = Math.abs(py - height / 2);
    if (dx > width * FOLLOW_JUMP_VIEWPORTS || dy > height * FOLLOW_JUMP_VIEWPORTS) {
      return "jump";
    }
    if (dx <= followHalfExtent(width) && dy <= followHalfExtent(height)) return "hold";
    return "ease";
  }

  /** Turn following on/off and light the crosshair button to match. */
  function setFollow(on) {
    followMode = !!on;
    if (!followMode) followEasing = false;
    if (controlsEl) {
      const btn = controlsEl.querySelector('[data-act="center"]');
      if (btn) btn.classList.toggle("active", followMode);
    }
    return followMode;
  }

  /**
   * Keep the aircraft in view. Called once per telemetry sample.
   *
   * Aimed at the TARGET (latest telemetry) rather than the eased marker
   * position, so by the time the pan lands the marker has caught up to it and
   * the two settle together instead of the camera chasing the marker.
   */
  function applyFollow() {
    if (!map || !followMode || followEasing) return;
    const target = vehTarget && realFix([vehTarget.lng, vehTarget.lat]);
    if (!target) return;
    const container = map.getContainer();
    const width = container ? container.clientWidth : 0;
    const height = container ? container.clientHeight : 0;
    // In 3D the aircraft is drawn at its altitude, which at 150 m and 60
    // degrees of pitch is a few hundred pixels above the ground point — far
    // enough that the marker can leave the top of the screen while the point
    // the dead zone is watching sits comfortably in the middle. So follow
    // what the operator is actually looking at.
    //
    // The camera is always aimed at the GROUND — a position on the map, which
    // MapLibre can place exactly — and the altitude is handled as a SCREEN
    // OFFSET instead: easeTo's own `offset` says where in the viewport the
    // aimed-at centre should land, so asking for the ground point to sit
    // exactly as far below the middle as the marker is drawn above it puts
    // the marker in the middle, by construction.
    //
    // This replaces un-projecting the marker's own screen position. That read
    // correctly at low altitude and failed at high: a marker above the
    // horizon un-projects to a point kilometres away or to nothing at all, so
    // the code fell back to the ground point, the marker stayed exactly as
    // far off the top of the screen as before, and the next sample asked for
    // the same useless pan — a follow mode that had quietly stopped
    // following, on exactly the flights where it matters most.
    const offset = followAirborneOffset(width, height);
    const shifted = offset[0] !== 0 || offset[1] !== 0;

    // The point the camera will actually deliver to the middle, which is what
    // the dead zone has to be measured against. The test and the pan MUST
    // mean the same point or following never settles: measure the marker,
    // move something else, and the marker is still outside the box it was
    // just judged by, so the next sample asks for the same pan, forever.
    //
    // Unclamped, this IS the marker (ground minus the full rise is where the
    // aircraft is drawn). Clamped — an aircraft a kilometre and a half above
    // the valley it is crossing, which is a normal thing to be after
    // launching off a ridge — it is the highest point the camera can reach
    // without throwing the ground off the bottom of the screen. The aircraft
    // then sits above the viewport and the map holds still, instead of
    // chasing something it can never catch.
    let point;
    if (threeD && shifted && veh3dGroundScreen) {
      point = { x: veh3dGroundScreen.x - offset[0], y: veh3dGroundScreen.y - offset[1] };
    } else if (threeD && veh3dScreen) {
      point = veh3dScreen;
    } else {
      try {
        point = map.project(target);
      } catch (_error) {
        return;   // a transform that is not ready yet is not an error worth logging
      }
    }
    if (!point) return;
    const action = followAction(point.x, point.y, width, height);
    if (action === "hold") return;

    // Reduced motion snaps, matching what the marker itself does; so does a
    // vehicle more than a viewport away, where a pan would be a smear rather
    // than a motion the eye can follow.
    if (action === "jump" || Corvus.anim.reducedMotion()) {
      // setCenter for the plain case, because that is what it means. Only a
      // shifted camera has to go through easeTo, which is the one call that
      // takes a screen offset; at zero duration it lands in the same frame.
      if (shifted) map.easeTo({ center: target, offset, duration: 0 });
      else map.setCenter(target);
      return;
    }
    followEasing = true;
    map.easeTo({ center: target, offset, duration: FOLLOW_EASE_MS });
  }

  /**
   * The screen offset that puts the AIRBORNE marker where the ground point
   * would otherwise go, as `[dx, dy]` for easeTo — or `[0, 0]` when there is
   * no airborne marker to correct for (2D, on the ground, on the globe, or a
   * frame where the shadow could not be placed).
   *
   * It is a difference between two points drawn in the SAME frame, so it
   * needs no model of the pitch, the zoom or the terrain: whatever MapLibre
   * did to separate the aircraft from its shadow is exactly what is undone
   * here. Clamped, because perspective can separate them by more than a
   * viewport and the ground is worth keeping on screen too.
   */
  function followAirborneOffset(width, height) {
    if (!threeD) return [0, 0];
    return airborneOffset(veh3dScreen, veh3dGroundScreen, width, height);
  }

  /** The offset rule itself, over two screen points. Pure, so the geometry is
   *  assertable without a renderer — the _followAction convention. */
  function airborneOffset(air, ground, width, height) {
    if (!air || !ground) return [0, 0];
    const dx = ground.x - air.x;
    const dy = ground.y - air.y;
    if (![dx, dy, width, height].every(isFinite)) return [0, 0];
    const clamp = (value, limit) => Math.max(-limit, Math.min(limit, value));
    return [
      clamp(dx, Math.max(0, width) * FOLLOW_AIRBORNE_MAX_FRACTION),
      clamp(dy, Math.max(0, height) * FOLLOW_AIRBORNE_MAX_FRACTION),
    ];
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
    // Same rule as the home marker: an aircraft is drawn where the aircraft
    // said it is, or not at all. The marker is created at DEFAULT_CENTER, so
    // without this a launched-but-unconnected Corvus shows an aircraft parked
    // on the map's opening view.
    const el = vehicleMarker.getElement();
    // Hidden in 3D, where the aircraft is drawn at its real altitude instead
    // (renderVehicle3D). Leaving it would park a second aircraft on the
    // ground directly under the flying one.
    if (el) el.hidden = threeD;
    vehicleMarker.setLngLat([vehDisplay.lng, vehDisplay.lat]);
    // The whole SVG rotates, so the heading cone and the body stay locked
    // together — they used to be separate elements and could disagree.
    const body = vehicleMarker.getElement().querySelector(".v-body");
    if (body) body.style.transform = `rotate(${vehDisplay.heading}deg)`;
    // The airborne aircraft is placed from the map's render matrix, which
    // only exists on a frame the map actually draws — so a vehicle that moves
    // while the camera is still has to ask for that frame.
    if (threeD && map && typeof map.triggerRepaint === "function") map.triggerRepaint();
  }

  /** Refresh the altitudes the 3D aircraft is drawn at. Per telemetry sample
   *  rather than per frame: the DEM lookup behind it is a terrain-mesh query,
   *  and nothing about it changes between two frames of the same sample. */
  function updateVehicleAltitude(state) {
    const agl = Number(state && state.altitude_agl);
    vehAltAgl = isFinite(agl) ? agl : null;
    vehAltDraw = vehicleDrawAltitude(state);
    // Where the shadow goes. Same reason it lives here rather than in the
    // render hook: it is a terrain-mesh query, and the ground under the
    // aircraft moves at telemetry rate, not at frame rate.
    vehGroundDraw = terrainElevation(realFix(state && state.position));
  }

  function updateVehicle(state) {
    if (!vehicleMarker || !state.connected) return;
    setVehicleTarget(state);
    updateVehicleAltitude(state);
    // After setVehicleTarget so the camera aims at THIS sample, not the last.
    // Skipped on the very first fix — the ease below places the map itself.
    if (!firstFix) applyFollow();

    if (firstFix && state.position && state.position[0] !== 0 && state.position[1] !== 0) {
      // Only center on a REAL GPS fix: state.connected reflects the MAVLink
      // heartbeat (not GPS), and the state store defaults position to [0,0]
      // until a fix arrives, so centering on the first connected sample
      // would ease to "null island" at zoom 16 before the real fix lands.
      firstFix = false;
      followEasing = true;   // cleared on moveend, like any other follow pan
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

    updateHome(state);
  }

  /**
   * Place — or hide — the home marker.
   *
   * Hidden until the aircraft has actually reported a home, and hidden again
   * the moment it stops: the marker used to be created at DEFAULT_CENTER and
   * shown from the first frame, so a freshly launched Corvus with nothing
   * connected drew a landing pad on a map position no aircraft had ever named,
   * and the update guard tested only the longitude, which meant a home that
   * was cleared left the last one on screen for good. "H" in a ring is a
   * promise about where the aircraft will come back to; it must be the
   * aircraft's answer or no marker at all.
   */
  function updateHome(state) {
    if (!homeMarker) return;
    const home = realFix(state && state.home);
    const el = homeMarker.getElement();
    if (!home) {
      if (el) el.hidden = true;
      return;
    }
    homeMarker.setLngLat(home);
    if (el) el.hidden = false;
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
      // An open search box owns Escape first, the same rule the planner is
      // under: the operator is closing the thing in front of them, not
      // disarming the tool behind it. Its own input takes the key while the
      // caret is in the field; this is the case where they opened the box and
      // then clicked the map.
      if (searchBox && searchBox.isOpen()) {
        if (e.preventDefault) e.preventDefault();
        searchBox.setOpen(false);
        return;
      }
      if (e.preventDefault) e.preventDefault();
      setWaypointMode(false);
    }
  }

  // ---- map context menu ---------------------------------------------------

  /**
   * Register the actions the context menu offers. Each is
   * `{id, label, icon, note?, enabled?(point), run(point)}` where `point` is
   * `{lng, lat}`; `enabled` is re-evaluated every time the menu opens, so a
   * row can reflect live vehicle state without the map subscribing to it.
   * Passing an empty list disables the menu entirely.
   */
  function setContextActions(actions) {
    contextActions = Array.isArray(actions) ? actions.slice() : [];
    if (!contextActions.length) closeContextMenu();
  }

  /** Degrees as the app writes coordinates everywhere else: 6 decimals is
   *  ~0.1 m, which is finer than anything a click can express. */
  function formatLngLat(point) {
    return `${point.lat.toFixed(6)}, ${point.lng.toFixed(6)}`;
  }

  /**
   * The menu surface, built once on first use. It is a Corvus.ui.menu — the
   * same component the flight bar's mode list and the layer switcher open —
   * so the rows, the glass, Escape, the outside press and the keyboard are
   * the app's, and what is left here is only what is particular to a menu
   * anchored to a place on the ground: the pin, the coordinates it acts on,
   * and following the point while the map pans.
   */
  function contextMenu() {
    if (contextMenuHandle) return contextMenuHandle;
    contextMenuHandle = Corvus.ui.menu({
      className: "map-context-menu",
      role: "menu",
      ariaLabel: "Map position actions",
      // Inside the map container, not on <body>: the menu has to scroll,
      // resize and pan with the ground it points at.
      host: () => contextHostEl,
      // The map does not scroll; it moves, and positionContextMenu() follows.
      closeOnScroll: false,
      render: renderContextRows,
      onClose: dropContextPin,
    });
    return contextMenuHandle;
  }

  /** Fill the open menu: the coordinates it acts on, then one row per action. */
  function renderContextRows(surface) {
    const point = { lng: contextPoint.lng, lat: contextPoint.lat };

    const coords = document.createElement("span");
    coords.className = "ui-menu-head map-context-coords";
    coords.textContent = formatLngLat(point);
    surface.appendChild(coords);

    return contextActions.map((action) => {
      const enabled = typeof action.enabled === "function"
        ? !!action.enabled(point) : true;
      // The note explains the row: why it cannot be used, or what it will do.
      // `note` may be a function so it can read live state at open time.
      const note = typeof action.note === "function"
        ? action.note(point, enabled) : action.note;
      const row = Corvus.ui.menuItem({
        className: "map-context-item",
        icon: action.icon,
        label: action.label == null ? action.id : action.label,
        note,
        disabled: !enabled,
        onSelect: () => {
          // Close first: the action is async and the operator has already made
          // the choice — leaving the menu up while a command flies would invite
          // a second click on a row that is now acting on a stale point.
          closeContextMenu();
          try { action.run(point); } catch (err) { console.error("map action failed:", err); }
        },
      });
      row.dataset.action = action.id || "";
      surface.appendChild(row);
      return row;
    });
  }

  /** Take the pin off the map and forget the point. Idempotent: it runs both
   *  from closeContextMenu() and from the menu closing itself (Escape, an
   *  outside press, another dropdown opening). */
  function dropContextPin() {
    if (contextPinEl && contextPinEl.parentNode) {
      contextPinEl.parentNode.removeChild(contextPinEl);
    }
    contextPinEl = null;
    contextPoint = null;
  }

  /** Close the menu and clear what it was about. Idempotent — every close path
   *  (Escape, outside click, an action, a new click, teardown) lands here. */
  function closeContextMenu() {
    if (contextMenuHandle) contextMenuHandle.close(false);
    dropContextPin();
  }

  /**
   * Keep the pin and the menu on their ground point. Both are positioned in
   * the container's own pixels (`map.project`), which is also what
   * `style.left` writes — so this needs no correction under the interface
   * scale, unlike anything reading a client rect. Called on every map move.
   */
  function positionContextMenu() {
    if (!contextMenuHandle || !contextMenuHandle.isOpen()) return;
    if (!contextPoint || !map || !contextHostEl) return;
    const at = map.project([contextPoint.lng, contextPoint.lat]);
    contextPinEl.style.left = `${at.x}px`;
    contextPinEl.style.top = `${at.y}px`;
    contextMenuHandle.place({ x: at.x, y: at.y });
  }

  /** Build and show the menu for *lngLat*. Replaces any menu already open. */
  function openContextMenu(lngLat) {
    // Closed before the new point is recorded: the menu clears contextPoint as
    // it goes, and doing this the other way round would drop the point the
    // rows are about to be built from.
    closeContextMenu();
    if (!map || !contextHostEl || !contextActions.length) return;
    contextPoint = { lng: lngLat.lng, lat: lngLat.lat };

    contextPinEl = document.createElement("div");
    contextPinEl.className = "map-context-pin";
    contextHostEl.appendChild(contextPinEl);

    const at = map.project([contextPoint.lng, contextPoint.lat]);
    contextPinEl.style.left = `${at.x}px`;
    contextPinEl.style.top = `${at.y}px`;
    // Opening focuses the first usable row, so the menu is operable from the
    // keyboard the moment it appears (and Escape has something to return from).
    if (!contextMenu().open({ x: at.x, y: at.y })) dropContextPin();
  }

  /* A left-click on the map opens the menu — MapLibre fires "click" only for a
     genuine click, never at the end of a pan, so dragging the map stays
     drag-only. Planning mode owns the click while it is on (a click there
     places a waypoint), so the menu stands down rather than competing. */
  function onMapClick(e) {
    if (waypointMode || !e || !e.lngLat) return;
    openContextMenu(e.lngLat);
  }

  /* Right-click opens the same menu, because that is the gesture most
     operators will try first. Outside planning mode there is no browser menu
     worth keeping over the canvas. */
  function onMapRightClick(e) {
    if (waypointMode || !e || !e.lngLat) return;
    if (e.preventDefault) e.preventDefault();
    openContextMenu(e.lngLat);
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
      // Planning takes the click over; a menu left open would be acting on a
      // point the operator can no longer see themselves choosing.
      closeContextMenu();
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

  // ---- 3D: terrain, buildings, and an aircraft drawn where it actually is ----
  //
  // 3D mode used to be a camera tilt and nothing else: the ground stayed a
  // flat plane, buildings did not exist, and the aircraft marker sat on that
  // plane whether it was at 5 m or 500 m. Tilting a flat map tells an operator
  // nothing they did not already know.
  //
  // What it is now, in three parts, each of which can fail on its own without
  // taking the other two down:
  //
  //  * TERRAIN — a raster-dem source (RGB-packed elevation tiles) routed
  //    through the same backend cache as the imagery, handed to MapLibre's
  //    setTerrain. The ground gets its real shape, and the imagery, the flown
  //    track and every marker drape over it.
  //  * BUILDINGS — OSM footprints with heights, extruded. Not Google's
  //    photorealistic meshes: those are a licensed 3D-Tiles product MapLibre
  //    cannot render (see corvus/buildings.py). Extruded footprints are the
  //    part of "show what is in the way" a key-less offline station can give.
  //  * THE AIRCRAFT — drawn at its actual altitude above that terrain, with a
  //    leader line down to a ground shadow, so height is something the
  //    operator SEES rather than reads off a number.
  //
  // Exaggeration is deliberately 1.0. Terrain is a clearance aid here, not
  // scenery: a hill drawn 1.5x too tall is a hill the operator misjudges.
  // MapLibre's own ceiling, and deliberately not raised. A more spectacular
  // camera angle is not worth arguing with the renderer's supported range in
  // an application whose map is the instrument.
  const THREE_D_PITCH = 60;
  const TERRAIN_EXAGGERATION = 1.0;
  // Ground-height reconciliation (syncTerrainElevation). Below the epsilon the
  // camera and the DEM already agree closely enough that a correction would be
  // invisible; the interval keeps a burst of arriving DEM tiles from
  // recomputing it on every one.
  const TERRAIN_SYNC_EPSILON_M = 2.0;
  const TERRAIN_SYNC_MIN_MS = 250;
  // The zoom the camera-seeding elevation probe reads. Coarse on purpose:
  // it only has to place a camera above the ground, the DEM query refines it
  // once terrain is live, and a z12 pixel (about 40 m of ground) comes from a
  // small tile that is very likely already cached.
  const TERRAIN_PROBE_ZOOM = 12;
  // A seed tile that never arrives must not hold the mode open.
  // Bounded tightly because the tilt waits behind it: a cached tile answers in
  // milliseconds, and a tile that is not coming must not hold the camera
  // still long enough for the operator to notice the button did nothing.
  const TERRAIN_PROBE_TIMEOUT_MS = 1500;
  // Attempts the settle poll makes before giving up — a few seconds' worth.
  const TERRAIN_SYNC_TRIES = 20;
  // Where the globe hands over to terrain. MapLibre's own globe is already
  // fading into Mercator across zoom 12-13, so switching at 12 happens while
  // the sphere is visually flat — the operator sees a continuous zoom, not a
  // mode change. The band is hysteresis for a zoom sitting on the threshold.
  const GLOBE_MAX_ZOOM = 12;
  const GLOBE_ZOOM_BAND = 0.5;
  // The slippy-grid zoom building cells are addressed on. Must match
  // corvus/buildings.CELL_ZOOM — the backend answers that zoom and nothing
  // else, because the cache is keyed by it.
  const BUILDING_CELL_Z = 15;
  // Below this the extrusions are sub-pixel slivers and a viewport covers
  // dozens of cells, so the fetching would cost far more than it shows.
  const BUILDING_MIN_ZOOM = 14;
  // Hard cap on cells requested for one view. A zoomed-out 3D view would
  // otherwise queue a hundred Overpass cells in one gesture.
  const BUILDING_MAX_CELLS = 24;
  // In-memory cell budget. ~24 cells cover a view; 240 is ten views' worth of
  // panning before the oldest is dropped and re-fetched from the backend
  // cache (which is on disk and keeps it).
  const BUILDING_CACHE_CELLS = 240;
  // Polygons the extrusion source may hold at once. A dense European centre
  // fills this from a handful of cells; past it the cost is a visibly slower
  // map, and what is being paid for is buildings near the horizon.
  const BUILDING_MAX_FEATURES = 12000;
  // Re-asking for a cell the backend has queued. The delay grows with the
  // attempt, so a slow upstream is waited out rather than hammered.
  const BUILDING_RETRY_MS = 2500;
  const BUILDING_MAX_RETRIES = 4;
  // See addBuildingLayer: chosen against map imagery, not against the theme.
  const BUILDING_COLOR = "#A7B0BD";
  // Perspective scaling of the airborne marker, as a fraction of its 2D size.
  // Bounded at both ends: an aircraft near the horizon must still be findable,
  // and one close to the camera must not swallow the viewport.
  const VEH3D_SCALE_MIN = 0.55;
  const VEH3D_SCALE_MAX = 1.6;
  // Below this the aircraft is on the ground for drawing purposes: the leader
  // line and the shadow are noise under a marker that is already sitting on
  // the shadow.
  const VEH3D_MIN_AGL_M = 1.0;

  let threeD = false;
  // Which 3D the operator asked for, remembered across off/on so pressing the
  // mode they were last in does not first hand them the other one.
  //
  //   "simple" — the camera tilt and the sky, over flat ground. No DEM, no
  //              Overpass, no globe: nothing is fetched that a 2D map does
  //              not already fetch, which is the whole point of it. The
  //              aircraft is still drawn at its height above that flat
  //              ground, because that costs nothing and is what the mode is
  //              being entered to see.
  //   "full"   — the tilt plus everything that makes the ground real:
  //              elevation relief, extruded buildings, and the globe at the
  //              far end of the zoom. It fetches elevation tiles and asks the
  //              backend for building footprints.
  //
  // The split exists because the second one has a cost — tiles over a radio
  // link, Overpass round trips, a terrain mesh on a field laptop's GPU — and
  // an operator who only wants to see the camera angle should not pay it.
  const THREE_D_SIMPLE = "simple";
  const THREE_D_FULL = "full";
  const THREE_D_OFF = "off";
  // Simple is where an operator starts. The expensive mode is a choice they
  // make, not one they discover after a field laptop has spent a radio link
  // on elevation tiles and Overpass round trips they never asked for.
  let threeDDetail = THREE_D_SIMPLE;
  // The DEM descriptor from GET /api/tiles/sources ({id, encoding, maxzoom,
  // attribution}). Null until the catalogue lands — and stays null on a
  // backend that does not serve one, which is what makes 3D degrade to a
  // plain tilt rather than break.
  let terrainSpec = null;
  // Whether the terrain is attached right now. terrainWanted is the mode's
  // INTENT, which is set the moment the operator asks and stays set across
  // the elevation probe that runs before the attach — the two differ for
  // those few milliseconds, and conflating them is how a second press starts
  // a second attach.
  let terrainOn = false;
  let terrainWanted = false;
  // Bumped by anything that invalidates an attach in flight, so a probe that
  // resolves late cannot switch terrain back on behind the operator.
  let terrainGeneration = 0;
  // Whether the globe projection is the one showing. Driven by zoom, not by a
  // button — see applyProjectionForZoom.
  let globeOn = false;
  // When the ground height under the camera was last reconciled with the DEM.
  // See syncTerrainElevation.
  let terrainSyncAt = 0;
  let terrainSyncTimer = null;
  // Attempts left in the current settle poll. Module-level so a DEM tile
  // landing mid-poll continues it rather than restarting it — see
  // scheduleTerrainSync.
  let terrainSyncTries = 0;
  let buildingSource = null;
  // "z/x/y" -> feature array. An empty array is a cell that is either genuinely
  // empty or still in flight; either way it must not be requested twice.
  const buildingCells = new Map();
  // "z/x/y" -> how many times we have re-asked for a cell the backend was
  // still fetching. Bounded, so a permanently unreachable upstream costs a
  // handful of requests and then nothing.
  const buildingRetries = new Map();
  let buildingsRefreshTimer = null;
  let buildingsPaintTimer = null;

  // The airborne aircraft is NOT a maplibregl.Marker: markers take a lngLat
  // and drape onto the terrain, which is exactly the thing an aircraft does
  // not do. These four elements are positioned by hand from the map's own
  // projection matrix (see renderVehicle3D).
  let veh3dEl = null;        // the aircraft, at altitude
  let veh3dShadowEl = null;  // where it is on the ground, directly below
  let veh3dLeadEl = null;    // the line joining the two
  let veh3dAltEl = null;     // its height above ground, as a number
  let veh3dLayerAdded = false;
  // The height to draw the aircraft at, in MapLibre's drawing frame (see
  // terrainElevation). Recomputed per telemetry sample rather than per frame —
  // the DEM lookup behind it is a terrain-mesh query, not an array read.
  let vehAltDraw = null;
  let vehAltAgl = null;
  // Terrain height under the aircraft, same frame, same reason.
  let vehGroundDraw = null;
  // Where the aircraft was last drawn on screen, in CSS pixels, or null. Follow
  // mode reads it: in 3D the marker is hundreds of pixels above the ground
  // point, so a dead zone measured on the ground point would let the aircraft
  // leave the top of the screen while its shadow sat comfortably in the middle.
  let veh3dScreen = null;
  // …and where its SHADOW was drawn, same frame, same units, or null when
  // there is no shadow (on the ground, on the globe, or over terrain whose
  // DEM has not arrived). Follow needs both: the difference between them is
  // exactly the screen offset the altitude introduced, and that offset is
  // what lets the camera be aimed at the marker without ever un-projecting a
  // point that may be above the horizon. See applyFollow.
  let veh3dGroundScreen = null;

  /**
   * The projection matrix out of a custom layer's render arguments.
   *
   * MapLibre 4 passed the matrix itself as the second argument; 5 passes an
   * object carrying it (plus the shader prelude a real WebGL layer would
   * need, which this one does not). Both shapes are accepted because the
   * vendored bundle is pinned and a version bump must not silently stop
   * drawing the aircraft — a wrong matrix is visible, a missing one is not.
   */
  function renderMatrix(args) {
    if (!args) return null;
    if (Array.isArray(args) || ArrayBuffer.isView(args)) return args;   // v4
    const data = args.defaultProjectionData;
    return (data && data.mainMatrix) || args.modelViewProjectionMatrix || null;
  }

  /** Multiply column-major 4x4 *m* by vec4 *v*. MapLibre hands out gl-matrix
   *  matrices, which are column-major: element (row r, column c) is m[c*4+r]. */
  function transformVec4(m, v) {
    const out = [0, 0, 0, 0];
    for (let r = 0; r < 4; r++) {
      out[r] = m[r] * v[0] + m[4 + r] * v[1] + m[8 + r] * v[2] + m[12 + r] * v[3];
    }
    return out;
  }

  /**
   * Screen position of a point *altitude* metres up, in MapLibre's own
   * drawing frame (see terrainElevation — that frame's zero is the terrain
   * under the map centre, not sea level).
   *
   * *matrix* is the one MapLibre hands a custom layer — its own mercator
   * projection, pitch, bearing, terrain and all — so this agrees with where
   * the map draws everything else by construction rather than by a formula
   * that has to be kept in step with it. Checked against map.project() for
   * points on the terrain: the two agree to a tenth of a pixel.
   *
   * Returns null for a point behind the camera (w <= 0), where the projection
   * is meaningless and the naive division would place it, mirrored, in front.
   */
  function projectAltitude(matrix, lng, lat, altitude, width, height) {
    if (!matrix || typeof maplibregl === "undefined") return null;
    const mc = maplibregl.MercatorCoordinate.fromLngLat(
      { lng, lat }, isFinite(altitude) ? altitude : 0);
    const clip = transformVec4(matrix, [mc.x, mc.y, mc.z, 1]);
    const w = clip[3];
    if (!(w > 1e-6)) return null;
    return {
      x: (clip[0] / w * 0.5 + 0.5) * width,
      y: (0.5 - clip[1] / w * 0.5) * height,
      w,
    };
  }

  /**
   * Terrain height under *lngLat*, in metres above sea level, or null.
   *
   * Everything about the aircraft's height is in this one datum — the DEM,
   * the autopilot's AMSL, and what MapLibre draws — which is what makes the
   * arithmetic in vehicleDrawAltitude as short as it is.
   *
   * MapLibre 4 was the awkward one: it drew relative to the terrain under the
   * map CENTRE and answered this query in that same shifted frame. v5 moved
   * both to sea level. Verified rather than assumed — projecting a point at
   * the height this returns lands on the same pixel map.project() reports,
   * and projecting it in the old frame does not.
   *
   * A literal 0 is NOT an answer. MapLibre returns 0 — not null — for a point
   * whose DEM tile is not loaded, which is every point outside the current
   * view and every point at all for the second or two after terrain is
   * attached. Taken at face value that is a confident "sea level", and it is
   * the difference between an aircraft drawn 150 m above an alpine valley and
   * one drawn 1 500 m inside the mountain: the home point is the usual
   * casualty, because after a few kilometres of flight its tile is no longer
   * loaded and homeGround silently becomes zero.
   *
   * The same rule syncTerrainElevation already applies to the camera, and for
   * the same reason. It costs nothing at genuine sea level: every caller's
   * fallback for "unknown" is 0 there anyway, so the answer is unchanged and
   * only its confidence is honest.
   */
  function terrainElevation(lngLat) {
    if (!map || !lngLat || !terrainOn) return null;
    if (typeof map.queryTerrainElevation !== "function") return null;
    let value = null;
    try {
      value = map.queryTerrainElevation({ lng: lngLat[0], lat: lngLat[1] });
    } catch (_error) {
      return null;   // terrain tiles not loaded here yet
    }
    return readTerrainValue(value);
  }

  /**
   * What one raw queryTerrainElevation answer MEANS, as a pure rule.
   *
   * Split out for the same reason wantsGlobe and followAction are: this is
   * the rule that decides whether an aircraft is drawn above a mountain or
   * inside it, and checking it should not need a GPU, a DEM or a map.
   */
  function readTerrainValue(value) {
    if (value == null || !isFinite(value)) return null;
    return value === 0 ? null : value;
  }

  /**
   * The height to draw the aircraft at, in metres above sea level.
   *
   * Preference order is an accuracy order, not a convenience one:
   *
   *  1. Terrain height under HOME plus the reported relative altitude. PX4
   *     measures relative_alt from the launch point, so anchoring it to the
   *     DEM at that same point puts the aircraft the right distance above the
   *     ground it took off from — the number the operator actually flies to,
   *     and the only one a hill under the aircraft cannot distort.
   *  2. The autopilot's own AMSL. Same datum as the DEM, so nothing has to be
   *     converted — but a bare GNSS altitude carries metres of error, and
   *     metres of error is a visible wobble.
   *  3. Terrain under the aircraft plus relative altitude, for a flight with
   *     no home fix. Wrong over a slope, but never by more than the slope,
   *     and it keeps the aircraft off the ground.
   *
   * With NO terrain at all — no DEM served, or 3D degraded to a plain tilt —
   * the whole preference order is beside the point: the ground MapLibre draws
   * is the sea-level plane, the shadow sits on it, and the leader line
   * between them is a measurement the operator reads as height above ground.
   * Drawing an AMSL there makes that line say 670 m for an aircraft 120 m up,
   * while the label beside it says 120. So on a flat world the aircraft is
   * drawn at its height above ground, which is the only reading that agrees
   * with the flat ground under it.
   */
  function vehicleDrawAltitude(state) {
    const agl = Number(state && state.altitude_agl);
    const amsl = Number(state && state.altitude_amsl);
    const hasAgl = isFinite(agl);
    if (!terrainOn) {
      if (hasAgl) return agl;
      return isFinite(amsl) ? amsl : 0;
    }
    const homeGround = terrainElevation(realFix(state && state.home));
    if (hasAgl && homeGround !== null) return homeGround + agl;
    if (isFinite(amsl) && amsl !== 0) return amsl;
    const ground = terrainElevation(realFix(state && state.position));
    return (ground === null ? 0 : ground) + (hasAgl ? agl : 0);
  }

  // ---- terrain ----

  /** The raster-dem source id, or null when the backend serves no DEM. */
  function terrainSourceId() {
    return terrainSpec ? terrainSpec.id : null;
  }

  /**
   * Put the DEM into the style and switch terrain on. Idempotent, and a no-op
   * without a DEM descriptor — the tilt then still works, on flat ground.
   *
   * tileSize 256 is not a default: MapLibre assumes 512 for raster-dem, and
   * the terrarium tiles are 256, so leaving it out halves every elevation and
   * quietly flattens the world.
   */
  /**
   * Bring terrain up, and call *onReady* once the camera has been placed.
   *
   * The callback exists because the order matters and is not obvious: the
   * camera's ground height has to be set BEFORE the tilt, not during it.
   * Writing the elevation into a running easeTo fights the animation, and
   * what the operator sees is the map decline to tilt at all. So the caller
   * hands its tilt over and it runs on the far side of the seed.
   */
  function enableTerrain(onReady) {
    const id = terrainSourceId();
    const ready = () => { if (typeof onReady === "function") onReady(); };
    if (!map || !started || !id || terrainWanted) return false;
    terrainWanted = true;
    // Claimed before the probe below, so a second press cannot start a second
    // attach, and cancelled by a generation bump if 3D goes off meanwhile.
    const generation = ++terrainGeneration;
    primeCameraElevation().then((height) => {
      if (generation !== terrainGeneration) return;   // switched off, or re-entered
      attachTerrain(id, generation);
      // AFTER the attach, never before: setTerrain resets the camera's centre
      // elevation to zero as it binds — it re-derives that height from the
      // terrain it is attaching, which at that instant knows nothing. A seed
      // written first is therefore thrown away, and the camera goes back
      // underground on the very call that was meant to save it.
      if (height !== null && terrainOn) {
        try { map.setCenterElevation(height); } catch (_error) { /* camera busy */ }
      }
      ready();
    });
    return true;
  }

  /**
   * Put the camera above the ground BEFORE the terrain arrives.
   *
   * This is the fix for the white map, and it is worth stating plainly
   * because the failure is so odd to look at. MapLibre's camera focuses on
   * sea level until it is told otherwise. Attach terrain over a valley floor
   * at 600 m and the camera is now six hundred metres underground: nothing is
   * in front of it, so nothing renders — and MapLibre will only report the
   * ground height once it can SEE the ground, which it cannot. The map sits
   * white and the only way out is to zoom until the camera clears the
   * mountain, which is a thing an operator has to discover rather than a
   * thing a ground station should do.
   *
   * So the height is read from the elevation tile directly, before terrain is
   * attached: one tile, one pixel, decoded here. That breaks the circle —
   * the camera starts above the ground, the ground is therefore visible, and
   * everything MapLibre does from there works the way it is supposed to.
   *
   * Resolves to null when the tile cannot be had (offline over ground that
   * was never downloaded). The camera then keeps its current elevation, which
   * is the behaviour this replaced — no worse, and no longer the common case.
   */
  function primeCameraElevation() {
    if (!map || typeof map.setCenterElevation !== "function") return Promise.resolve(null);
    let centre;
    try { centre = map.getCenter(); } catch (_error) { return Promise.resolve(null); }
    if (!centre || !isFinite(centre.lng) || !isFinite(centre.lat)) return Promise.resolve(null);
    return demElevationAt([centre.lng, centre.lat]);
  }

  /** Metres above sea level from one RGB-packed elevation pixel. */
  function decodeDemPixel(r, g, b) {
    const encoding = (terrainSpec && terrainSpec.encoding) || "terrarium";
    const height = encoding === "mapbox"
      ? -10000 + (r * 256 * 256 + g * 256 + b) * 0.1
      : r * 256 + g + b / 256 - 32768;
    return isFinite(height) ? height : null;
  }

  /**
   * Ground height under *lngLat*, read from a single elevation tile.
   *
   * Deliberately independent of MapLibre: this is what the camera is seeded
   * from, so it cannot be allowed to depend on the terrain being healthy —
   * that is the circle it exists to break. The tile comes from the same
   * backend cache as everything else, so it works offline exactly as far as
   * 3D does, and a miss resolves null rather than throwing.
   *
   * A coarse zoom on purpose. This only has to place a camera, which the DEM
   * query then refines to the metre once terrain is live; a z12 pixel is
   * about forty metres of ground, it is a small tile, and it is the one most
   * likely to be already cached.
   */
  function demElevationAt(lngLat) {
    const id = terrainSourceId();
    if (!id || typeof document === "undefined" || typeof Image === "undefined") {
      return Promise.resolve(null);
    }
    const zoom = Math.min(TERRAIN_PROBE_ZOOM, terrainSpec.maxzoom);
    const count = 1 << zoom;
    const lat = Math.max(-85.05112878, Math.min(85.05112878, lngLat[1]));
    const rad = (lat * Math.PI) / 180;
    const fx = ((lngLat[0] + 180) / 360) * count;
    const fy = ((1 - Math.log(Math.tan(rad) + 1 / Math.cos(rad)) / Math.PI) / 2) * count;
    if (!isFinite(fx) || !isFinite(fy)) return Promise.resolve(null);
    const x = Math.max(0, Math.min(count - 1, Math.floor(fx)));
    const y = Math.max(0, Math.min(count - 1, Math.floor(fy)));

    return new Promise((resolve) => {
      let settled = false;
      const finish = (value) => { if (!settled) { settled = true; resolve(value); } };
      // A tile that never resolves must not hold 3D mode open: the map is
      // still usable without the seed, only worse.
      const timer = window.setTimeout(() => finish(null), TERRAIN_PROBE_TIMEOUT_MS);
      const image = new Image();
      image.onload = () => {
        window.clearTimeout(timer);
        try {
          const size = image.naturalWidth || 256;
          const px = Math.max(0, Math.min(size - 1, Math.floor((fx - x) * size)));
          const py = Math.max(0, Math.min(size - 1, Math.floor((fy - y) * size)));
          const canvas = document.createElement("canvas");
          canvas.width = 1;
          canvas.height = 1;
          const ctx = canvas.getContext("2d", { willReadFrequently: true });
          ctx.drawImage(image, px, py, 1, 1, 0, 0, 1, 1);
          const data = ctx.getImageData(0, 0, 1, 1).data;
          finish(decodeDemPixel(data[0], data[1], data[2]));
        } catch (_error) {
          finish(null);   // a canvas we may not read from is not a reason to fail
        }
      };
      image.onerror = () => { window.clearTimeout(timer); finish(null); };
      image.src = `/api/tiles/${id}/${zoom}/${x}/${y}.png`;
    });
  }

  /**
   * Put the DEM source and the terrain into the style.
   *
   * A FRESH source every time, not a reused one. MapLibre leaves a raster-dem
   * source's tiles marked loaded across a setTerrain(null), so attaching
   * terrain to that same source hands back a terrain whose own DEM cache is
   * empty — and nothing refills it, because the tiles it would wait for have
   * already arrived once and fire no further events. queryTerrainElevation
   * then answers 0 forever: not null, which would read as "not yet", but a
   * confident wrong 0 that is indistinguishable from sea level.
   *
   * Rebuilding the source is what makes the second press of the button behave
   * like the first. It costs a loopback fetch against our own tile cache, not
   * a download, and it works offline for the same reason.
   */
  function attachTerrain(id, generation) {
    if (!map || !started || generation !== terrainGeneration || !terrainWanted) return;
    try {
      if (map.getSource(id)) map.removeSource(id);
      map.addSource(id, {
        type: "raster-dem",
        tiles: [tileUrl(id)],
        tileSize: 256,
        maxzoom: terrainSpec.maxzoom,
        encoding: terrainSpec.encoding || "terrarium",
        attribution: terrainSpec.attribution,
      });
      map.setTerrain({ source: id, exaggeration: TERRAIN_EXAGGERATION });
    } catch (error) {
      // A DEM that will not load is a flat 3D view, not a broken map.
      console.warn("Corvus: terrain unavailable", error);
      terrainWanted = false;
      return;
    }
    terrainOn = true;
    // From here on, DEM tiles landing are the cue to reconcile the camera's
    // idea of the ground height with the real one — see syncTerrainElevation.
    terrainSyncAt = 0;
    map.on("data", onTerrainData);
    scheduleTerrainSync(true);
  }

  /**
   * Tell the camera how high the ground under it really is.
   *
   * Without this 3D mode BLANKS THE MAP anywhere above sea level, which is
   * most places worth flying. MapLibre puts the camera's focus at zero until
   * it is told otherwise, so over a valley floor at 600 m the camera looks at
   * a point 600 m underground: the terrain is behind it, no tile computes as
   * visible, and the map renders empty while insisting everything has loaded.
   *
   * setCenterElevation is MapLibre 5's own public answer to this, and taking
   * it is most of why the map was moved to 5. The v4 equivalent had to be
   * done by hand through internals: transform.elevation is getter-only in 5,
   * and its recalculateZoom — MapLibre 4's own correction — preserved the
   * camera's ALTITUDE rather than its zoom, dropping the operator to street
   * level, and left the zoom NaN whenever the camera was below the new
   * ground, which poisoned getBounds, getZoom and every tile calculation.
   *
   * Runs for as long as 3D is on, not once: DEM tiles refine as they load,
   * and a camera moved by something that does not interpolate elevation can
   * drift. Rate limited, and a no-op once the two agree.
   */
  function syncTerrainElevation() {
    if (!map || !terrainOn) return false;
    if (typeof map.setCenterElevation !== "function") return true;   // nothing to do
    // Never mid-animation: MapLibre interpolates the elevation itself across
    // an ease, and a write here would fight its next frame.
    if (typeof map.isEasing === "function" && map.isEasing()) return false;
    let centre;
    try { centre = map.getCenter(); } catch (_error) { return false; }
    if (!centre || !isFinite(centre.lng) || !isFinite(centre.lat)) return false;
    const ground = terrainElevation([centre.lng, centre.lat]);
    if (ground === null) return false;   // the DEM under the centre has not arrived
    // A literal zero is not an answer yet. A terrain with no DEM loaded under
    // the centre reports 0 rather than null, which is exactly what the camera
    // already believes — so taking it at face value declares success, stops
    // asking, and leaves the aircraft's ground six hundred metres below the
    // mountain it is on. Keeping it unsettled costs a few retries at genuine
    // sea level and then leaves 0 standing, which is the right answer there.
    if (ground === 0) return false;
    const current = Number(map.getCenterElevation());
    if (isFinite(current) && Math.abs(ground - current) < TERRAIN_SYNC_EPSILON_M) return true;
    try {
      map.setCenterElevation(ground);
      return true;
    } catch (_error) {
      return false;   // a camera mid-change; the retry below tries again
    }
  }

  /**
   * Keep asking until the camera and the DEM agree, then stop.
   *
   * The events are not enough on their own. The ground height only becomes
   * knowable once the terrain MESH is built, which is strictly after the last
   * DEM tile's data event — so the final event can arrive a moment too early,
   * find nothing to read, and be the last one there is. The camera is then
   * left believing the ground is at sea level, which over a 600 m valley
   * floor is the whole blank-map failure again, reached by a race instead of
   * by a missing feature.
   *
   * A short bounded poll closes that window without pretending an event will
   * come. It stops on the first agreement, and it stops regardless after a
   * few seconds — a DEM that never loads (offline, uncached ground) must not
   * leave a timer running for the rest of the flight.
   *
   * Two things make it genuinely bounded, both of which the first version
   * only claimed:
   *
   *  * The try budget is refilled by a NEW CUE (terrain attached, camera
   *    moved) and not by a DEM tile landing. Panning over a city delivers
   *    tiles in a steady trickle, and a budget topped up by each one is a
   *    poll that never ends.
   *  * TERRAIN_SYNC_MIN_MS is a floor between attempts, not merely the gap
   *    between retries. Behind every attempt is a terrain-mesh query, and a
   *    burst of two dozen arriving tiles used to buy two dozen of them —
   *    which is what terrainSyncAt was written for and never read for.
   */
  function scheduleTerrainSync(restart) {
    if (restart) terrainSyncTries = 0;
    if (terrainSyncTimer) return;   // one is already pending; it will run
    const wait = Math.max(0, TERRAIN_SYNC_MIN_MS - (Date.now() - terrainSyncAt));
    if (wait > 0) {
      terrainSyncTimer = window.setTimeout(attemptTerrainSync, wait);
      return;
    }
    attemptTerrainSync();
  }

  function attemptTerrainSync() {
    terrainSyncTimer = null;
    if (!terrainOn) return;
    terrainSyncAt = Date.now();
    if (syncTerrainElevation()) return;
    if (++terrainSyncTries >= TERRAIN_SYNC_TRIES) return;
    terrainSyncTimer = window.setTimeout(attemptTerrainSync, TERRAIN_SYNC_MIN_MS);
  }

  /** Stop the settle poll. Idempotent. */
  function stopTerrainSync() {
    if (terrainSyncTimer) window.clearTimeout(terrainSyncTimer);
    terrainSyncTimer = null;
    terrainSyncTries = 0;
  }

  /** DEM tiles landing is the cue that the heights are worth asking for. */
  function onTerrainData(event) {
    if (!event || event.sourceId !== terrainSourceId()) return;
    if (event.sourceDataType === "metadata") return;
    scheduleTerrainSync();
    // The ground under the AIRCRAFT is read once per telemetry sample, not
    // per frame — so a DEM tile that lands between two samples would leave
    // the aircraft drawn against a ground height it no longer has to guess
    // at, which on entering 3D is every aircraft for the first second or two.
    // Only while that height is still unknown: once the tile under the
    // aircraft is in, telemetry keeps it current by itself and this costs
    // nothing for the rest of the flight.
    if (threeD && vehGroundDraw === null) refreshVehicleTerrain();
  }

  /** Re-read the terrain heights the airborne aircraft is drawn against, and
   *  ask for the frame that shows it. */
  function refreshVehicleTerrain() {
    if (!map || !threeD) return;
    updateVehicleAltitude(Corvus.telemetry.getState());
    if (typeof map.triggerRepaint === "function") map.triggerRepaint();
  }


  /**
   * Give the map a sky and an atmosphere, or take them away.
   *
   * At 60 degrees of pitch the horizon is on screen and the ground has to
   * stop against SOMETHING, or the map reads as half-drawn rather than as
   * distant. On the globe the same setting is what puts the blue rim around
   * the Earth.
   *
   * Colours come from the theme: this map sits inside an application wearing
   * one palette, and a photographic blue sky over the dark theme looks like a
   * screenshot of a different program. The CSS class is kept alongside as the
   * backstop for a MapLibre without setSky — it paints the container, which
   * is what shows above the horizon when nothing else does.
   */
  function setSky(on) {
    const container = map && map.getContainer();
    if (container) container.classList.toggle("three-d", !!on);
    if (!map || typeof map.setSky !== "function") return;
    try {
      map.setSky(on ? {
        "sky-color": Corvus.ui.token("--surface-3", "#1C232C"),
        "horizon-color": Corvus.ui.token("--surface-1", "#11161D"),
        "fog-color": Corvus.ui.token("--bg", "#0B0E12"),
        "sky-horizon-blend": 0.6,
        "horizon-fog-blend": 0.6,
        "fog-ground-blend": 0.05,
        "atmosphere-blend": ["interpolate", ["linear"], ["zoom"], 0, 0.8, 8, 0.4, 12, 0],
      // undefined, not null, to take it away: MapLibre validates the argument
      // against the style spec and null fails it ("sky: object expected, null
      // found"). The sky does come off either way — the validator logs rather
      // than throws — but leaving 3D printed a console error every time, and
      // an error the operator is meant to ignore is an error they will ignore
      // when it matters.
      } : undefined);
    } catch (_error) { /* no sky in this MapLibre: the CSS backstop stands */ }
  }

  /**
   * The globe, and the plain flat map.
   *
   * Feature-detected. On a MapLibre without a globe the map stays flat, which
   * is what it was before, rather than failing.
   */
  function setGlobe(on) {
    if (!map || typeof map.setProjection !== "function") return false;
    try {
      map.setProjection({ type: on ? "globe" : "mercator" });
      return true;
    } catch (_error) {
      return false;   // no globe in this MapLibre; the flat map still works
    }
  }

  /**
   * Choose the projection for the current zoom: globe far out, terrain close
   * in, and nothing for the operator to press in between.
   *
   * This is how Google Earth behaves, and here it is also a necessity.
   * MapLibre 5.24 answers queryTerrainElevation with 0 under the globe
   * projection, so terrain and globe cannot both be on: the camera would be
   * told the ground is at sea level, look at a point six hundred metres
   * underground, and render nothing. The split costs nothing real, because
   * the two are useful at opposite ends of the zoom range — from orbit a 600
   * m hill is well under a pixel, and a globe at street level is a flat map
   * with extra maths.
   *
   * The band is hysteresis: without it a zoom hovering on the threshold would
   * tear the terrain down and build it back up on every wheel click.
   */
  /**
   * Should the globe be showing at *zoom*, given that it *currentlyGlobe*?
   *
   * Pure, and split out for the same reason followAction is: the rule is the
   * thing worth asserting, and it should not need a renderer to check. The
   * band is hysteresis — the threshold sits further away in whichever
   * direction would mean changing — so a zoom resting on the boundary does
   * not tear the terrain down and build it back up on every wheel click.
   *
   * A zoom that is not a number keeps whatever is showing: a transform
   * mid-change is not a reason to rebuild the world.
   */
  function wantsGlobe(zoom, currentlyGlobe) {
    if (!isFinite(zoom)) return !!currentlyGlobe;
    return zoom < GLOBE_MAX_ZOOM + (currentlyGlobe ? GLOBE_ZOOM_BAND : -GLOBE_ZOOM_BAND);
  }

  function applyProjectionForZoom() {
    // Simple 3D has neither half of this handover: it is the camera angle and
    // nothing else, so the flat map stays flat all the way out.
    if (!map || !started || !threeD || threeDDetail !== THREE_D_FULL) return;
    let zoom;
    try { zoom = map.getZoom(); } catch (_error) { return; }
    const wantGlobe = wantsGlobe(zoom, globeOn);
    if (wantGlobe === globeOn) return;
    globeOn = wantGlobe;
    if (wantGlobe) {
      // Terrain first: leaving it on under the globe is the blank map above.
      disableTerrain();
      setGlobe(true);
    } else {
      setGlobe(false);
      // enableTerrain starts its own settle poll once it has attached; this
      // is for the case where terrain was already on and only the projection
      // changed under it.
      enableTerrain();
      scheduleTerrainSync(true);
    }
  }

  /**
   * Take terrain back out.
   *
   * The DEM SOURCE goes with it, deliberately. Leaving it in the style and
   * re-binding terrain to it on the next press is what made the second press
   * of the button behave differently from the first: MapLibre leaves a
   * raster-dem source's tiles marked loaded across a setTerrain(null), so the
   * new terrain waits for tiles that have already arrived, no further events
   * come, and queryTerrainElevation answers a confident 0 forever. See
   * attachTerrain, which rebuilds it. The cost is a loopback fetch against
   * our own tile cache — not a download, and it works offline.
   */
  function disableTerrain() {
    // The generation bump comes first and unconditionally: it cancels an
    // attach that is still waiting on its elevation probe, which is the one
    // way terrain could come back after being switched off.
    terrainGeneration++;
    terrainWanted = false;
    // Also unconditional: an attach that never completed can still have left
    // a settle poll running, and that poll outliving the mode is a timer
    // nobody is waiting for on a field laptop's battery.
    stopTerrainSync();
    if (!map || !terrainOn) return;
    map.off("data", onTerrainData);
    // Order matters: MapLibre refuses to remove a source the terrain is still
    // bound to, so the binding goes first.
    try { map.setTerrain(null); } catch (_error) { /* already gone */ }
    const id = terrainSourceId();
    try {
      if (id && map.getSource(id)) map.removeSource(id);
    } catch (_error) { /* something else is holding it; enableTerrain retries */ }
    terrainOn = false;
  }

  // ---- 3D buildings ----

  function addBuildingLayer() {
    if (!map || map.getSource("buildings")) return;
    map.addSource("buildings", {
      type: "geojson",
      data: { type: "FeatureCollection", features: [] },
    });
    // Under the track and the plan route, which are added first and therefore
    // stack above it: a building must never hide where the aircraft has been.
    map.addLayer({
      id: "buildings-3d",
      source: "buildings",
      type: "fill-extrusion",
      minzoom: BUILDING_MIN_ZOOM,
      paint: {
        // A literal mid-slate, not a theme token — the same argument the
        // vehicle marker's white ring makes: a building is read against
        // IMAGERY, not against the UI. A theme colour would be a pale block
        // on a pale street map and a dark block on dark satellite, i.e.
        // invisible in exactly the two cases it has to work in. A mid tone is
        // darker than bright roofs and lighter than shadowed ground.
        "fill-extrusion-color": BUILDING_COLOR,
        "fill-extrusion-height": ["get", "height"],
        "fill-extrusion-base": ["get", "min_height"],
        // Slightly translucent: an opaque block hides the ground the aircraft
        // has to land on, and the operator needs both. Not more than this, or
        // the extrusions stop reading as solid volumes.
        "fill-extrusion-opacity": 0.85,
        // Shades the lower part of each wall, which is the only cue that
        // these are volumes rather than flat polygons lying on a slope.
        "fill-extrusion-vertical-gradient": true,
      },
    }, map.getLayer("path-glow") ? "path-glow" : undefined);
    buildingSource = map.getSource("buildings");
  }

  function removeBuildingLayer() {
    // Timers first: a pending repaint or refresh would otherwise fire against
    // a source that is no longer in the style. Both check `threeD` too, but a
    // timer nobody is waiting for is work the field laptop should not do.
    if (buildingsRefreshTimer) window.clearTimeout(buildingsRefreshTimer);
    if (buildingsPaintTimer) window.clearTimeout(buildingsPaintTimer);
    buildingsRefreshTimer = null;
    buildingsPaintTimer = null;
    if (!map) return;
    if (map.getLayer("buildings-3d")) map.removeLayer("buildings-3d");
    if (map.getSource("buildings")) map.removeSource("buildings");
    buildingSource = null;
  }

  /** Slippy-grid column for *lon* at BUILDING_CELL_Z. */
  function cellX(lon) {
    const n = 1 << BUILDING_CELL_Z;
    return Math.max(0, Math.min(n - 1, Math.floor(((lon + 180) / 360) * n)));
  }

  /** Slippy-grid row for *lat* at BUILDING_CELL_Z. */
  function cellY(lat) {
    const n = 1 << BUILDING_CELL_Z;
    const clamped = Math.max(-85.05112878, Math.min(85.05112878, lat));
    const rad = (clamped * Math.PI) / 180;
    const frac = (1 - Math.log(Math.tan(rad) + 1 / Math.cos(rad)) / Math.PI) / 2;
    return Math.max(0, Math.min(n - 1, Math.floor(frac * n)));
  }

  /**
   * The cell keys covering the current view, capped.
   *
   * Pure-ish geometry over the map's own bounds, so a pitched view — whose
   * bounds MapLibre already widens to the visible ground — asks for the cells
   * that are actually on screen. The cap is a fetch budget, not a correctness
   * rule: beyond it the nearest cells are kept, because those are the ones
   * under the aircraft.
   */
  function viewBuildingCells() {
    if (!map) return [];
    let bounds, centre;
    try {
      bounds = map.getBounds();
      centre = map.getCenter();
    } catch (_error) {
      return [];   // a transform that is not ready yet asks for nothing
    }
    if (!bounds || !centre) return [];
    return buildingCellsIn(
      { w: bounds.getWest(), s: bounds.getSouth(), e: bounds.getEast(), n: bounds.getNorth() },
      [centre.lng, centre.lat],
    );
  }

  /**
   * The cell keys inside *box* ({w,s,e,n}), nearest to *centre* first, capped.
   *
   * Pure geometry, split out from viewBuildingCells for the same reason
   * followAction is: the rule is worth asserting on its own, and it has no
   * business needing a renderer to check.
   *
   * It grows OUTWARD from the centre rather than enumerating the box. A
   * pitched camera sees to the horizon and MapLibre's bounds say so: at 60
   * degrees the visible ground can be hundreds of cells across, so walking
   * that rectangle to then keep the nearest two dozen means materialising a
   * five-figure array on every pan, on the frame budget of a field laptop.
   * Square rings visit the cells that would have been kept, in the order they
   * would have been kept, and stop.
   *
   * Nearest-first is also load-bearing downstream: paintBuildings fills its
   * feature budget from the front of this list, so what a dense city drops is
   * the ground near the horizon rather than the ground under the aircraft.
   */
  function buildingCellsIn(box, centre) {
    if (!box || !centre) return [];
    if (![box.w, box.s, box.e, box.n, centre[0], centre[1]].every(isFinite)) return [];
    const x0 = cellX(box.w), x1 = cellX(box.e);
    const y0 = cellY(box.n), y1 = cellY(box.s);
    if (x1 < x0 || y1 < y0) return [];
    const cx = cellX(centre[0]), cy = cellY(centre[1]);
    const inView = (x, y) => x >= x0 && x <= x1 && y >= y0 && y <= y1;

    const cells = [];
    const reach = Math.max(x1 - x0, y1 - y0);
    for (let ring = 0; ring <= reach && cells.length < BUILDING_MAX_CELLS; ring++) {
      for (let dx = -ring; dx <= ring && cells.length < BUILDING_MAX_CELLS; dx++) {
        for (let dy = -ring; dy <= ring && cells.length < BUILDING_MAX_CELLS; dy++) {
          // Only the ring's edge; its interior was covered by earlier rings.
          if (ring > 0 && Math.abs(dx) !== ring && Math.abs(dy) !== ring) continue;
          const x = cx + dx, y = cy + dy;
          if (inView(x, y)) cells.push(`${BUILDING_CELL_Z}/${x}/${y}`);
        }
      }
    }
    return cells;
  }

  /**
   * Ask the backend for one cell. At most once per key at a time: the entry
   * is claimed with an empty array BEFORE the fetch, so the several move
   * events a single drag produces cannot each start the same request.
   *
   * The backend never blocks on the OSM upstream — it answers what it has and
   * says `pending` when it has queued the rest (see corvus/buildings.py). So
   * a pending cell is retried a few times, a few seconds apart, and then let
   * go. A failure is simply an empty cell: offline every cell fails, and a
   * map that re-asks on every pan is a map that stutters.
   */
  function fetchBuildingCell(key) {
    if (buildingCells.has(key)) return;
    buildingCells.set(key, []);
    Corvus.telemetry.requestJson(`/api/buildings/${key}.json`)
      .then((data) => {
        const features = (data && data.features) || [];
        const pending = !!(data && data.pending);
        buildingCells.set(key, features);
        // The budget is cleared by a FINAL answer only. A `pending` reply is
        // an HTTP success but not an answer, and clearing the count on one
        // hands every retry a fresh budget — with the upstream unreachable,
        // where every reply is pending forever, that is an unbounded retry
        // loop against a cell that is never coming. Measured: eight requests
        // per cell in twenty seconds instead of the five the cap describes.
        if (!pending) buildingRetries.delete(key);
        if (features.length) schedulePaintBuildings();
        if (pending) retryBuildingCell(key);
      })
      .catch(() => {
        // An empty cell is the right ANSWER offline, but it must not become a
        // permanent one: the claim above is what stops two requests racing,
        // and leaving it standing after a failure meant a single dropped
        // request (a radio coming back, a backend restarting, a laptop
        // waking) removed those buildings for the rest of the session — the
        // operator could pan away and back and never see them again. Dropping
        // the claim hands the cell to the same bounded retry a pending cell
        // gets, so it costs a handful of requests and then genuinely stops.
        buildingCells.delete(key);
        retryBuildingCell(key);
      });
  }

  /** Re-ask for a cell the backend is still fetching, up to a few times. */
  function retryBuildingCell(key) {
    const tries = (buildingRetries.get(key) || 0) + 1;
    if (tries > BUILDING_MAX_RETRIES) return;
    buildingRetries.set(key, tries);
    window.setTimeout(() => {
      // Only if 3D is still on and the cell is still worth having: an
      // operator who left the area (or left 3D) must not be paying for
      // requests about ground they are not looking at.
      if (!threeD || !buildingSource) return;
      if (!viewBuildingCells().includes(key)) return;
      buildingCells.delete(key);
      fetchBuildingCell(key);
    }, BUILDING_RETRY_MS * tries);
  }

  /**
   * Coalesce the repaints a burst of arriving cells would otherwise cause.
   *
   * A pan asks for up to two dozen cells and they land within a second of
   * each other; repainting per cell re-tiles a collection of thousands of
   * polygons that many times, for the same final picture. One frame's delay
   * turns that into one repaint.
   */
  function schedulePaintBuildings() {
    if (buildingsPaintTimer) return;
    buildingsPaintTimer = window.setTimeout(() => {
      buildingsPaintTimer = null;
      paintBuildings();
    }, 60);
  }

  /**
   * Push the visible cells' features into the extrusion source.
   *
   * Capped, and the cap is why viewBuildingCells returns its cells nearest
   * first: over a city centre the visible cells can hold more polygons than a
   * field laptop can extrude at a usable frame rate, and the ones worth
   * keeping are the ones near the aircraft, not the ones near the horizon.
   * Dropping the far cells costs detail where the operator is not looking.
   */
  function paintBuildings() {
    if (!buildingSource || !threeD) return;
    const features = [];
    for (const key of viewBuildingCells()) {
      const cell = buildingCells.get(key);
      if (!cell || !cell.length) continue;
      for (const feature of cell) {
        if (features.length >= BUILDING_MAX_FEATURES) break;
        features.push(feature);
      }
      if (features.length >= BUILDING_MAX_FEATURES) break;
    }
    buildingSource.setData({ type: "FeatureCollection", features });
  }

  /** Evict the oldest cells once the in-memory set outgrows its budget.
   *  Map iterates in insertion order, so the first keys are the oldest. */
  function trimBuildingCells() {
    while (buildingCells.size > BUILDING_CACHE_CELLS) {
      const oldest = buildingCells.keys().next();
      if (oldest.done) return;
      buildingCells.delete(oldest.value);
      buildingRetries.delete(oldest.value);
    }
  }

  /**
   * Fetch what the current view needs and repaint. Debounced, because a drag
   * ends in a burst of move events and each one would otherwise walk the grid.
   */
  function refreshBuildings() {
    if (!map || !threeD || !buildingSource) return;
    if (buildingsRefreshTimer) window.clearTimeout(buildingsRefreshTimer);
    buildingsRefreshTimer = window.setTimeout(() => {
      buildingsRefreshTimer = null;
      if (!threeD || !buildingSource) return;
      if (map.getZoom() < BUILDING_MIN_ZOOM) {
        buildingSource.setData({ type: "FeatureCollection", features: [] });
        return;
      }
      viewBuildingCells().forEach(fetchBuildingCell);
      trimBuildingCells();
      paintBuildings();
    }, 250);
  }

  // ---- the aircraft, at altitude ----

  /** Build the four elements that make up the airborne aircraft. */
  function buildVehicle3D(mapEl) {
    if (!mapEl || veh3dEl) return;

    veh3dLeadEl = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    veh3dLeadEl.setAttribute("class", "veh3d-lead");
    veh3dLeadEl.setAttribute("aria-hidden", "true");
    const line = document.createElementNS("http://www.w3.org/2000/svg", "line");
    veh3dLeadEl.appendChild(line);
    veh3dLeadEl.hidden = true;
    mapEl.appendChild(veh3dLeadEl);

    veh3dShadowEl = document.createElement("div");
    veh3dShadowEl.className = "veh3d-shadow";
    veh3dShadowEl.hidden = true;
    mapEl.appendChild(veh3dShadowEl);

    // The same markup as the 2D marker, so the aircraft the operator learned
    // to read on a flat map is the aircraft they read in the air.
    veh3dEl = buildVehicleMarker();
    veh3dEl.classList.add("veh3d");
    veh3dEl.hidden = true;
    mapEl.appendChild(veh3dEl);

    veh3dAltEl = document.createElement("div");
    veh3dAltEl.className = "veh3d-alt";
    veh3dAltEl.hidden = true;
    mapEl.appendChild(veh3dAltEl);
  }

  function hideVehicle3D() {
    veh3dScreen = null;
    veh3dGroundScreen = null;
    // Only where it changes something: this runs from the custom layer's
    // render hook, which fires on every frame the map draws — including every
    // frame of plain 2D, where all four are hidden already and four attribute
    // writes per frame is a cost with nothing to show for it.
    [veh3dEl, veh3dShadowEl, veh3dLeadEl, veh3dAltEl].forEach((el) => {
      if (el && !el.hidden) el.hidden = true;
    });
  }

  /**
   * Is the GLOBE the projection right now?
   *
   * `globeOn` is authoritative — the module is the only thing that sets a
   * projection — but MapLibre is asked as well, because the answer decides
   * whether the render matrix may be used at all and a wrong "no" puts the
   * aircraft in the corner of the screen (see renderVehicle3D).
   */
  function usingGlobe() {
    if (globeOn) return true;
    if (!map || typeof map.getProjection !== "function") return false;
    try {
      const projection = map.getProjection();
      return !!projection && projection.type === "globe";
    } catch (_error) {
      return false;
    }
  }

  /**
   * Is *lngLat* on the far side of the globe, behind the Earth?
   *
   * Only meaningful under the globe; on the flat map nothing is occluded.
   * Feature-detected against the transform because MapLibre exposes no public
   * form of this, and a bundle that does not have it simply draws the
   * aircraft — which is what every version did before the globe existed.
   */
  function occludedByGlobe(lngLat) {
    const transform = map && map.transform;
    if (!transform || typeof transform.isLocationOccluded !== "function") return false;
    try {
      return !!transform.isLocationOccluded({ lng: lngLat[0], lat: lngLat[1] });
    } catch (_error) {
      return false;
    }
  }

  /**
   * Place the airborne aircraft, its shadow, the line between them and the
   * height readout, from the map's own projection matrix.
   *
   * Called from a custom layer's render hook — the one documented way to get
   * that matrix — which also means it runs on exactly the frames the map
   * draws, so the aircraft can never lag the ground it is over.
   */
  function renderVehicle3D(matrix) {
    // Every way out of this function has to forget the last screen position:
    // follow mode reads it, and a point left over from the frame before the
    // map was resized (or from before the aircraft lost its fix) is a camera
    // that pans to somewhere the aircraft is not.
    if (!threeD || !veh3dEl || !vehDisplay || !map) { hideVehicle3D(); return; }
    const container = map.getContainer();
    const width = container ? container.clientWidth : 0;
    const height = container ? container.clientHeight : 0;
    if (!(width > 0 && height > 0)) { hideVehicle3D(); return; }

    const lng = vehDisplay.lng, lat = vehDisplay.lat;
    if (!realFix([lng, lat])) { hideVehicle3D(); return; }
    veh3dScreen = null;
    veh3dGroundScreen = null;

    // On the GLOBE the render matrix is the globe's own, and it takes points
    // on a sphere — feeding it the mercator coordinates every other
    // projection wants put the aircraft in the bottom-right corner of the
    // viewport while the map drew it in the middle. There is no altitude to
    // show at that zoom anyway: terrain is off (see applyProjectionForZoom)
    // and a kilometre of height is well under a pixel when a continent fits
    // on screen. So the globe gets the aircraft where MapLibre itself says it
    // is, and nothing else.
    if (usingGlobe()) { renderVehicleOnGlobe(lng, lat); return; }

    // vehGroundDraw, not a terrain query: this runs on every rendered frame,
    // and queryTerrainElevation walks the terrain mesh. The ground under an
    // aircraft changes at telemetry rate, not at 60 Hz, so it is computed
    // with the sample (updateVehicleAltitude) and read here.
    //
    // Unknown is not zero. With terrain attached but its tiles not yet in
    // (the first seconds of 3D, or ground that has just come into view), a
    // shadow drawn at sea level lands hundreds of pixels from the aircraft
    // and the leader line becomes a wire into the ground. With NO terrain the
    // drawing plane really is sea level, so there zero is the answer.
    const groundKnown = vehGroundDraw !== null || !terrainOn;
    const groundM = vehGroundDraw === null ? 0 : vehGroundDraw;
    // The floor keeps an aircraft whose reported altitude is below the DEM
    // from being drawn inside the hill. It applies only where the ground is
    // actually KNOWN: clamping to a zero that only means "no tile yet" would
    // haul an aircraft over a valley floor up to sea level.
    const airM = vehAltDraw === null ? groundM
      : (groundKnown ? Math.max(vehAltDraw, groundM) : vehAltDraw);
    const air = projectAltitude(matrix, lng, lat, airM, width, height);
    const base = groundKnown
      ? projectAltitude(matrix, lng, lat, groundM, width, height)
      : null;
    if (!air) { hideVehicle3D(); return; }

    // Perspective: an object's screen size falls off as 1/w, and w at the map
    // centre is the natural reference because that is where the 2D marker's
    // size was chosen. The exponent softens it — a literal 1/w makes the
    // aircraft lurch in size on every pitch change.
    const centre = map.getCenter();
    // Altitude 0 at the map centre IS the ground under the map centre: that
    // point is the drawing frame's own zero (see terrainElevation). So the
    // reference costs a matrix multiply, not a second mesh query.
    const ref = projectAltitude(matrix, centre.lng, centre.lat, 0, width, height);
    const ratio = (ref && ref.w > 1e-6) ? ref.w / air.w : 1;
    const scale = Math.max(VEH3D_SCALE_MIN,
      Math.min(VEH3D_SCALE_MAX, Math.pow(ratio, 0.7)));

    // Laid into the ground plane (rotateX by the camera pitch) rather than
    // left facing the screen: it is the same trick MapLibre's own
    // pitchAlignment:"map" uses, and it is what makes the heading cone point
    // somewhere in the WORLD instead of somewhere on the display.
    const pitch = map.getPitch();
    const bearing = map.getBearing();
    veh3dScreen = { x: air.x, y: air.y };
    veh3dEl.hidden = false;
    veh3dEl.style.transform =
      `translate(-50%, -50%) translate(${air.x}px, ${air.y}px) scale(${scale.toFixed(3)}) rotateX(${pitch}deg)`;
    const body = veh3dEl.querySelector(".v-body");
    if (body) body.style.transform = `rotate(${vehDisplay.heading - bearing}deg)`;

    // Below this the aircraft is effectively on its own shadow, and a leader
    // line of two pixels reads as a rendering fault.
    const airborne = vehAltAgl !== null && vehAltAgl > VEH3D_MIN_AGL_M && !!base;
    if (!airborne) {
      if (veh3dShadowEl) veh3dShadowEl.hidden = true;
      if (veh3dLeadEl) veh3dLeadEl.hidden = true;
      if (veh3dAltEl) veh3dAltEl.hidden = true;
      return;
    }

    veh3dGroundScreen = { x: base.x, y: base.y };
    veh3dShadowEl.hidden = false;
    veh3dShadowEl.style.transform =
      `translate(-50%, -50%) translate(${base.x}px, ${base.y}px) scale(${scale.toFixed(3)}) rotateX(${pitch}deg)`;

    veh3dLeadEl.hidden = false;
    veh3dLeadEl.setAttribute("viewBox", `0 0 ${width} ${height}`);
    veh3dLeadEl.setAttribute("width", String(width));
    veh3dLeadEl.setAttribute("height", String(height));
    const line = veh3dLeadEl.firstChild;
    line.setAttribute("x1", base.x.toFixed(1));
    line.setAttribute("y1", base.y.toFixed(1));
    line.setAttribute("x2", air.x.toFixed(1));
    line.setAttribute("y2", air.y.toFixed(1));

    veh3dAltEl.hidden = false;
    veh3dAltEl.textContent = `${Math.round(vehAltAgl)} m`;
    veh3dAltEl.style.transform =
      `translate(-50%, -50%) translate(${air.x}px, ${(air.y + (base.y - air.y) * 0.5).toFixed(1)}px)`;
  }

  /**
   * The aircraft on the globe: one marker, where MapLibre puts the ground.
   *
   * map.project() rather than the render matrix, because the globe's matrix
   * takes points on a sphere and the mercator coordinates the flat map needs
   * land somewhere else entirely on it. Height is dropped rather than
   * approximated: the globe only shows at the far end of the zoom range, and
   * there even a kilometre is a fraction of a pixel — a leader line drawn
   * from arithmetic nobody can check is worse than no leader line.
   *
   * Nothing is drawn for an aircraft on the far side of the Earth, where the
   * projection still answers with a screen point and the map draws ocean.
   */
  function renderVehicleOnGlobe(lng, lat) {
    if (occludedByGlobe([lng, lat])) { hideVehicle3D(); return; }
    let at;
    try { at = map.project([lng, lat]); } catch (_error) { hideVehicle3D(); return; }
    if (!at || !isFinite(at.x) || !isFinite(at.y)) { hideVehicle3D(); return; }

    veh3dScreen = { x: at.x, y: at.y };
    veh3dGroundScreen = { x: at.x, y: at.y };
    veh3dEl.hidden = false;
    // No rotateX: the globe's camera is looking at a sphere, and laying the
    // marker into a ground plane that is not flat would only skew it.
    veh3dEl.style.transform =
      `translate(-50%, -50%) translate(${at.x.toFixed(1)}px, ${at.y.toFixed(1)}px)`;
    const body = veh3dEl.querySelector(".v-body");
    if (body) body.style.transform = `rotate(${vehDisplay.heading - map.getBearing()}deg)`;
    if (veh3dShadowEl) veh3dShadowEl.hidden = true;
    if (veh3dLeadEl) veh3dLeadEl.hidden = true;
    if (veh3dAltEl) veh3dAltEl.hidden = true;
  }

  /**
   * Register the custom layer whose only job is to hand us the projection
   * matrix once per rendered frame.
   *
   * It draws nothing itself. A WebGL aircraft would need its own shaders,
   * its own theming and its own text; the DOM marker already exists, is
   * already themed, and is already what the operator recognises — all it was
   * ever missing was the third coordinate.
   */
  function addVehicle3DLayer() {
    if (!map || veh3dLayerAdded) return;
    try {
      map.addLayer({
        id: "vehicle-altitude",
        type: "custom",
        renderingMode: "3d",
        onAdd: () => {},
        render: (_gl, args) => {
          // An exception thrown out of a custom layer's render aborts
          // MapLibre's whole frame — the map would stop drawing because a
          // marker could not be placed. Swallow it and leave the aircraft
          // where it was; the next frame tries again.
          try { renderVehicle3D(renderMatrix(args)); } catch (_error) { /* one bad frame */ }
        },
      });
      veh3dLayerAdded = true;
    } catch (error) {
      console.warn("Corvus: 3D aircraft unavailable", error);
    }
  }

  // ---- the mode itself ----

  /**
   * Turn 3D mode on or off. Returns the resulting state.
   *
   * Each half is independent: a missing DEM still gets the tilt and the
   * buildings, a backend without buildings still gets the terrain, and either
   * way the aircraft is drawn at its altitude (above sea level if there is no
   * DEM to measure from). Nothing here can leave the map in a state the
   * operator cannot get out of by pressing the button again.
   */
  /** Tilt the camera, honouring the reduced-motion contract the rest of the
   *  app keeps: an operator who asked for no animation gets the new angle,
   *  not a six-hundred-millisecond swing into it. */
  function tiltTo(pitch) {
    if (Corvus.anim.reducedMotion()) map.jumpTo({ pitch });
    else map.easeTo({ pitch, duration: 600 });
  }

  /**
   * Normalise anything into one of the three modes.
   *
   * Pure, and forgiving on purpose: the value can come from a config file
   * written by a newer build, and a mode nobody recognises must leave the map
   * flat rather than half-built.
   */
  function normaliseThreeDMode(value) {
    if (value === THREE_D_SIMPLE || value === THREE_D_FULL) return value;
    return THREE_D_OFF;
  }

  /** The mode showing right now: "off", "simple" or "full". */
  function threeDMode() {
    return threeD ? threeDDetail : THREE_D_OFF;
  }

  /**
   * Ask for one of the three modes. Returns the one that resulted.
   *
   * Switching BETWEEN the two 3D modes is a real transition, not a no-op:
   * going to "simple" has to take the terrain, the buildings and the globe
   * back out, and going to "full" has to build them. Both directions run
   * through the same body, so there is one place that decides what a mode
   * consists of.
   */
  function set3DMode(mode) {
    const next = normaliseThreeDMode(mode);
    // Remembered even when the map is not ready to apply it, so the rail and
    // a restored config agree about which 3D the operator is in.
    if (next !== THREE_D_OFF) threeDDetail = next;
    applyThreeD(next !== THREE_D_OFF);
    return threeDMode();
  }

  /**
   * Choose WHICH 3D, without saying anything about whether it is on.
   *
   * The rail's button owns on/off; this is the setting behind it. Applied
   * immediately when 3D is already showing — the operator is looking at the
   * thing they just changed — and otherwise only remembered, so the next
   * press of the button hands them the one they picked.
   */
  function set3DDetail(detail) {
    const next = normaliseThreeDMode(detail);
    if (next === THREE_D_OFF) return threeDMode();   // not a detail
    threeDDetail = next;
    if (threeD) applyThreeD(true);
    else markThreeButton();
    return threeDMode();
  }

  /** The old boolean door onto the mode: on means whichever 3D was last
   *  chosen, which is the simple one until the operator says otherwise. */
  function set3D(on) {
    if (!map) return threeD;
    return applyThreeD(!!on);
  }

  function applyThreeD(on) {
    on = !!on;
    if (!map) return threeD;
    threeD = on;
    const full = threeDDetail === THREE_D_FULL;
    let tiltHandled = false;
    if (on) {
      // Everything that touches the style waits for the style. The rail is
      // built before "load" (so the operator is never left without controls
      // during the first tile fetch), which means this can be pressed before
      // there is a style to add a source to; the "load" handler re-applies.
      if (started) {
        if (full) {
          // Which of the two the zoom calls for. Entering 3D over the field
          // is terrain; entering it zoomed out to the country is the globe.
          //
          // Through wantsGlobe, and through the CURRENT state, for two
          // reasons. A bare threshold could enter on the globe at a zoom
          // applyProjectionForZoom would have taken straight back off it — a
          // mode change the operator watches happen for no reason they asked
          // for. And this runs again on an already-3D map (the style
          // finishing, the source catalogue landing, the other 3D mode being
          // chosen), where deciding "terrain" while the globe is still the
          // projection used to attach terrain UNDER the globe: the camera is
          // then told the ground is at sea level, looks at a point far
          // underground, and the map renders empty.
          const wantGlobe = wantsGlobe(map.getZoom(), globeOn);
          if (wantGlobe) {
            if (!globeOn) setGlobe(true);
            globeOn = true;
            // The two cannot both be on — see applyProjectionForZoom.
            disableTerrain();
          } else {
            if (globeOn) setGlobe(false);
            globeOn = false;
            // The tilt is handed to enableTerrain so it runs after the camera
            // has been placed above the ground — see there. When there is no
            // terrain to wait for, it happens immediately below instead.
            tiltHandled = enableTerrain(() => tiltTo(THREE_D_PITCH));
          }
          addBuildingLayer();
          refreshBuildings();
        } else {
          // Simple: the tilt and the sky, and nothing that costs a fetch.
          // Written as a teardown rather than as "do less", because this is
          // also the path from "full" to "simple" — the ground has to lose
          // its relief, the buildings have to go, and the globe has to hand
          // back to the flat map, all while 3D stays on.
          if (globeOn) setGlobe(false);
          globeOn = false;
          disableTerrain();
          removeBuildingLayer();
        }
        setSky(true);
        addVehicle3DLayer();
        // The aircraft's ground changed under it — from a DEM to the flat
        // plane or the other way — so the height it is drawn at has to be
        // re-read before the next frame rather than at the next telemetry
        // sample, which on a dropped link may never come.
        updateVehicleAltitude(Corvus.telemetry.getState());
        if (typeof map.triggerRepaint === "function") map.triggerRepaint();
      }
      if (vehicleMarker) {
        const el = vehicleMarker.getElement();
        if (el) el.hidden = true;
      }
      // The end of the tilt reconciles the ground height through the map's
      // own moveend handler — which matters when the DEM is already cached
      // and no further data event is coming. Deliberately NOT a
      // once("moveend") here: a 3D press cancelled before the tilt lands
      // leaves that listener registered forever, and an operator toggling the
      // button accumulates one per press.
      if (!tiltHandled) tiltTo(THREE_D_PITCH);
    } else {
      hideVehicle3D();
      removeBuildingLayer();
      setSky(false);
      setGlobe(false);
      globeOn = false;
      disableTerrain();
      // The 2D marker comes back only if the aircraft has actually reported a
      // position — the same rule renderVehicle applies, and the reason it is
      // not simply un-hidden here.
      renderVehicle();
      tiltTo(0);
    }
    markThreeButton();
    // The panel may be open over the button that was just pressed — the whole
    // point of a hover panel is that it is there while you use the control it
    // belongs to. setValue rather than rebuild: rebuilding would replace the
    // switch the pointer is on mid-press.
    if (threeDPicker && threeDMenuHandle && threeDMenuHandle.isOpen()) {
      threeDPicker.setValue(threeDDetail === THREE_D_FULL);
    }
    return threeD;
  }

  /**
   * Save the 3D state so the map comes back the way it was left.
   *
   * TWO keys, because there are two independent things here and one of them
   * survives the other being off. `three_d` is what the map opens in;
   * `three_d_detail` is where the panel's switch sits, which has to be
   * remembered even while 3D is off — an operator who prefers the cheap mode
   * and left the map flat must not be handed the expensive one on their next
   * press, and a single key that says "off" cannot remember anything.
   *
   * Fire-and-forget, like the layer choice: a failed save (backend busy,
   * offline) must never break the control the operator just used.
   */
  function persistThreeD() {
    if (!Corvus.telemetry || typeof Corvus.telemetry.postAction !== "function") return;
    Corvus.telemetry.postAction("/api/config", {
      map: { three_d: threeDMode(), three_d_detail: threeDDetail },
    }).catch(() => {});
  }

  /**
   * Put the map back into the 3D it was left in.
   *
   * The switch's position is restored always; the MODE only ever on the way
   * up. A config with neither key, or one from a newer build naming a mode
   * this one does not have, leaves the map flat — half-building a 3D nobody
   * asked for is worse than opening the way every version of this map has.
   *
   * `three_d_detail` falls back to `three_d` so a config written before the
   * two were separated still restores the right switch.
   *
   * A 3D press the operator made while this fetch was in flight wins — they
   * are looking at the map now, and a config written days ago is not an
   * argument against that.
   */
  function restoreThreeD(saved) {
    const map3d = saved || {};
    set3DDetail(map3d.three_d_detail || map3d.three_d);
    const mode = normaliseThreeDMode(map3d.three_d);
    if (mode === THREE_D_OFF || threeD) return;
    if (started) set3DMode(mode);
    else window.addEventListener("corvus:mapready", () => {
      if (!threeD) set3DMode(mode);
    }, { once: true });
  }

  function init(mapEl, controlsEl) {
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
    addAttribution(map);

    // Controls are built NOW, not on "load". MapLibre fires "load" only once the
    // style AND its first tiles have resolved, so building the rail there left
    // the operator staring at a map with no zoom, layer or centre buttons for as
    // long as the first tile fetch took — seconds on a cold cache, and much
    // worse offline. Nothing in the rail needs a loaded style: the handlers act
    // on the map object (which exists from the constructor), and setBaseLayer
    // already defers its own work until `started`.
    buildControls(controlsEl);
    buildTrackControl(mapEl);
    buildSearch(mapEl);
    watchFlightBar(mapEl);
    // Pure DOM, like the rail and the clear-track button, so it is built
    // before "load" for the same reason they are: nothing about it needs a
    // resolved style, and the elements stay hidden until 3D asks for them.
    buildVehicle3D(mapEl);

    // The context menu lives in the map container so it scrolls, resizes and
    // stacks with everything else on the map. Wired here rather than on "load"
    // for the same reason the rail is: nothing about it needs a loaded style,
    // and an operator who clicks during the first tile fetch should still get
    // a menu.
    contextHostEl = mapEl;
    map.on("click", onMapClick);
    map.on("contextmenu", onMapRightClick);
    // Re-project on every move so an open menu stays on its ground point while
    // the map pans, zooms, rotates or follows the vehicle.
    map.on("move", positionContextMenu);

    // A move the OPERATOR started is them saying "look here instead", so it
    // stops the aircraft dragging the view back. The crosshair button turns it
    // on again.
    //
    // Keyed on movestart carrying an originalEvent rather than on "dragstart":
    // our own easeTo/panBy and the rail's zoom buttons are programmatic and
    // carry none, so they can never trip this, while a drag — mouse or touch —
    // always does. The wheel is the one input deliberately let through, because
    // zooming is something you do WHILE watching the aircraft.
    map.on("movestart", (e) => {
      const source = e && e.originalEvent;
      if (!source) return;
      // Whatever it turns out to be, an input event on this map is the
      // operator aiming it, and that is what the planner opens on — a wheel
      // included, which is a change of view even though it is not a change of
      // subject and so does not stop the follow.
      noteHomeAim();
      if (source.type === "wheel") return;
      setFollow(false);
    });
    // Clears the in-flight guard whoever moved the map — a user drag that
    // interrupts a follow pan must not leave it stuck on.
    map.on("moveend", () => {
      followEasing = false;
      // The camera landed somewhere new: it may have crossed the zoom where
      // the globe hands over to terrain, and its ground height may be stale.
      // Not every way the camera can move interpolates that height — a follow
      // jump and a plain setCenter do not — so reconcile it here as well as
      // on DEM tiles.
      applyProjectionForZoom();
      scheduleTerrainSync(true);
      // New ground came into view; ask the backend for its buildings. A no-op
      // outside 3D, and debounced, so a drag costs one pass over the grid.
      refreshBuildings();
    });

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
      // Both markers start hidden: nothing has reported a position yet, and
      // DEFAULT_CENTER is where the map opens, not where anything is.
      const vehEl = vehicleMarker.getElement();
      if (vehEl) vehEl.hidden = true;
      updateHome(Corvus.telemetry.getState());

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
      // A 3D press that landed before the style did: apply it now that there
      // is something to apply it to. set3D is idempotent, so this is a no-op
      // in the usual case where the operator has not pressed it at all.
      if (threeD) set3D(true);
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
      const saved = (res && res.config && res.config.map) || {};
      restoreThreeD(saved);
      const key = saved.base_layer;
      if (!key || !sources[key] || key === activeLayer) return;
      activeLayer = key;
      if (started) setBaseLayer(key);
      else window.addEventListener("corvus:mapready", () => setBaseLayer(key), { once: true });
    }).catch(() => {});
  }

  return {
    init,
    centerOnVehicle,
    // Follow mode: on by default, dropped by a map drag, restored by
    // centerOnVehicle. Exposed so a future Settings switch (or a test) can
    // drive it without reaching into the module.
    setFollow,
    isFollowing: () => followMode,
    getMap: () => map,
    isReady: () => started,
    setWaypointMode,
    // The flight bar's OPTIONAL narrowing rule: whether it answers to a share
    // of the map column (captions off early, as the window narrows) on top of
    // the fit it always answers to. Off unless Settings asks; app.js applies
    // the config's answer once that fetch lands.
    setFlightBarShrink,
    // test hook: the rule as pure arithmetic — the room a map column of a
    // given width gives the bar (the second argument standing in for
    // .map-topleft), or null for "nothing measurable here".
    _flightBarRoom: (width, roomWidth) => flightBarRoom(
      { clientWidth: width },
      { clientWidth: roomWidth == null ? width : roomWidth },
    ),
    // 3D mode: terrain relief, extruded buildings, and the aircraft drawn at
    // the altitude it is actually flying. Exposed so Settings (or a test) can
    // drive it without reaching through the control rail.
    set3D,
    is3D: () => threeD,
    // The mode as one of "off" / "simple" / "full", and the way to set it.
    // Settings (or a test) drives the rail's 3D switcher through these.
    set3DMode,
    get3DMode: threeDMode,
    // Which 3D the button will give you, without turning it on.
    set3DDetail,
    hasTerrain: () => terrainOn,
    getWaypoints,
    clearWaypoints,
    onWaypointsUpdate,
    // Context menu: the map owns where and when it opens, app.js owns what the
    // rows do (they are flight commands, and those carry the topbar's attempt
    // tracking and the armed gating that live there).
    setContextActions,
    closeContextMenu,
    getSources,
    getCacheStats,
    // Settings' map-service picker drives the live map through this, so the
    // Home tab's layer switcher and the Appearance page can never disagree
    // about which base layer is showing.
    setBaseLayer,
    getBaseLayer: () => activeLayer,
    // The layer switcher, built for ANOTHER map's rail. The Mission planner
    // has its own map and its own rail and must offer the same list, the same
    // surface and the same persistence as this one — see createLayerMenu.
    createLayerMenu,
    // One layer's descriptor out of the hydrated catalogue — its label,
    // maxzoom and attribution. A second map building its own raster source
    // needs those, and the catalogue is fetched once, here.
    layerSpec: specFor,
    // The credit control, so every map says the same thing and folds and
    // opens it the same way rather than keeping its own copy of either.
    addAttribution,
    _ownAttributionToggle: ownAttributionToggle,
    // The shared view (see "The view the two maps share"): where the Mission
    // planner opens, and how it reports back where the operator left it.
    missionOpenView,
    noteMissionView,
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
    // test hook: the dead-zone rule as pure geometry — "hold" / "ease" /
    // "jump" for a vehicle at (px, py) in a width x height viewport.
    _followAction: followAction,
    _bootstrap: () => BOOTSTRAP,
    // test hooks: the shared-view rule as pure bookkeeping — which of the two
    // maps was aimed last — assertable without either map existing.
    _noteHomeAim: noteHomeAim,
    _sharedViewState: () => ({ homeAimSeq, missionAimed }),
    _resetSharedView: () => { aimSeq = 0; homeAimSeq = 0; missionAimed = null; },
    // test hooks: the pure 3D maths — a column-major 4x4 times a vec4, and
    // the altitude preference chain — assertable without WebGL or a DEM.
    _transformVec4: transformVec4,
    _vehicleDrawAltitude: (state) => vehicleDrawAltitude(state),
    _projectAltitude: projectAltitude,
    // test hook: the building-cell grid as pure geometry — which cells a
    // {w,s,e,n} view around a centre asks for, in which order, and how many.
    // Mirrors the _followAction convention.
    _buildingCellsIn: (box, centre) => buildingCellsIn(box, centre),
    // test hook: the mode-name rule. A config file from a newer build can
    // name a mode this one does not have, and the answer has to be "flat"
    // rather than "half-built".
    _normaliseThreeDMode: (value) => normaliseThreeDMode(value),
    // test hook: which 3D the operator last chose, remembered across off/on
    // so pressing 3D hands back the mode they were in, not the other one.
    _threeDDetail: () => threeDDetail,
    // test hook: the globe/terrain handover rule, including its hysteresis.
    _wantsGlobe: (zoom, currentlyGlobe) => wantsGlobe(zoom, currentlyGlobe),
    // test hook: what a raw terrain reading means. A literal 0 is MapLibre's
    // answer for a DEM tile that is not loaded, not a claim about sea level,
    // and reading it as one is what buried the aircraft in the mountain.
    _readTerrainValue: (value) => readTerrainValue(value),
    // test hook: the screen offset that aims the camera at the AIRBORNE
    // marker instead of at its shadow, clamped. Pure geometry over two
    // points, like _followAction.
    _airborneOffset: (air, ground, width, height) =>
      airborneOffset(air, ground, width, height),
    // test hook: the overlay stack the base imagery must stay underneath, in
    // the order the layers are added. A base layer re-inserted above the
    // first of these hides it — which is how switching map service used to
    // erase the downloaded-area rectangles.
    _overlayLayers: () => OVERLAY_LAYERS.slice(),
    // test hook: the elevation-tile pixel decoder that seeds the camera. It
    // is what keeps the map from going white over high ground, and it is
    // plain arithmetic, so it is assertable without a canvas.
    _decodeDemPixel: (r, g, b) => decodeDemPixel(r, g, b),
    _setTerrainSpec: (spec) => { terrainSpec = spec; },
    _globeZooms: () => ({ max: GLOBE_MAX_ZOOM, band: GLOBE_ZOOM_BAND }),
    _buildingLimits: () => ({
      cellZoom: BUILDING_CELL_Z,
      maxCells: BUILDING_MAX_CELLS,
      maxFeatures: BUILDING_MAX_FEATURES,
      minZoom: BUILDING_MIN_ZOOM,
    }),
    // test hooks: the pure track rules — distance decimation and the reboot
    // edge — assertable without a map.
    _recordTrackPoint: (pos) => recordTrackPoint(pos),
    _realFix: (pos) => realFix(pos),
    // test hook: the home-marker rule — a marker is drawn only for a home the
    // aircraft actually reported. Takes the marker so the rule is assertable
    // without MapLibre.
    _updateHome: (state, marker) => {
      const saved = homeMarker;
      if (marker) homeMarker = marker;
      try { updateHome(state); } finally { homeMarker = saved; }
    },
    _checkForReboot: (ms) => checkForReboot(ms),
    // test hook: push one telemetry sample through the whole marker path
    // (target, altitude, follow, track) without an SSE stream behind it.
    // Mirrors the _updateHome convention.
    _updateVehicle: (state) => updateVehicle(state),
    // test hook: the open menu's point, so the click -> menu path is assertable
    // without a layout engine.
    _contextPoint: () => (contextPoint ? { lng: contextPoint.lng, lat: contextPoint.lat } : null),
    _openContextMenu: (lngLat) => openContextMenu(lngLat),
    _resetTrackState: () => { pathCoords = []; lastBootMs = null; },
    // test hook: pure coordinate computation for the plan route (vehicle
    // position prefix + operator waypoints, or []). Accepts an optional
    // waypoint list for testing in Node, where the internal `waypoints` array
    // cannot be populated without a map; omit it to use the live waypoints.
    // Never touches wpRouteSource (null in Node). Mirrors the _sources convention.
    _planRouteCoords: (wps) => computePlanCoords(wps),
  };
})();
