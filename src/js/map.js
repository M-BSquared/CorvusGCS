"use strict";
window.Corvus = window.Corvus || {};

Corvus.map = (function () {
  let map = null;
  let vehicleMarker = null;
  let homeMarker = null;
  let pathSource = null;
  let pastSource = null;
  // The flight in progress. Every flight before it on this station is in
  // pastFlights, oldest first, until the operator clears the track.
  let pathCoords = [];
  let pastFlights = [];
  // Autopilot uptime last seen. A value that moves BACKWARDS means the vehicle
  // rebooted: a new flight session, never a link drop.
  let lastBootMs = null;
  // The armed flag of the last sample, null until one arrives. Disarmed to
  // armed is where a new flight begins.
  let lastArmed = null;
  // Which vehicle the track above belongs to ("<link>:<system id>", the
  // telemetry's vehicle_id), null until the first one is heard. When the
  // operator flies another vehicle, the track goes to Corvus.mapFleet with the
  // vehicle it belongs to and the new vehicle's track comes back from there.
  let trackOwner = null;
  // The flown vehicle's colour from the vehicle list, "" for the theme's own.
  let vehicleColor = "";
  let vehicleLabelEl = null;
  // Settings (ui.track_earlier_flights / ui.track_rainbow /
  // ui.track_clear_on_restart / ui.track_in_air), applied by app.js once the
  // config lands. Earlier flights get their own colour unless turned off, and
  // a colour each when the rainbow is on; the track outlives a restart unless
  // asked otherwise; in 3D it is drawn in the air too unless turned off.
  let trackEarlierFlights = true;
  let trackRainbow = false;
  let trackClearOnRestart = false;
  let trackInAir = true;
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
  // The pitch and zoom a move of OURS asked for and the camera has not reached
  // yet (null when there is nothing pending). MapLibre stops a running
  // animation whenever another move starts, so on a start with 3D saved on
  // the tilt, the first zoom onto the aircraft and a follow jump each cut the
  // one before short: 3D showed as on, the camera stayed flat at the zoom it
  // opened on. Every programmatic move carries what is still pending (see
  // cameraMove), an operator gesture drops it, and a moveend that lands short
  // asks once more.
  const cameraGoal = { pitch: null, zoom: null };
  let cameraGoalRetries = 0;
  const CAMERA_GOAL_RETRIES = 3;
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
  // Track recording keeps the SHAPE of the flight, not a point every so
  // many metres (see recordTrackPoint). A fixed spacing was coarse in a turn
  // and wasteful on a straight leg, and it left the line ending up to that
  // spacing behind the aircraft.
  //
  // A sample closer than this to the end of the line is noise, not movement:
  // a hover adds nothing.
  const TRACK_JITTER_M = 0.3;
  // The same two rules for height, in metres of height. Looser than across:
  // a baro wanders by more than a GNSS fix does, and a hover must not turn
  // that into vertices. A point's height counts as these share of the
  // horizontal limits (see trackZScale).
  const TRACK_JITTER_V_M = 1.0;
  const TRACK_TOLERANCE_V_M = 1.0;
  // How far the drawn line may stray from where the aircraft actually flew:
  // a quarter of the position's own reported uncertainty (gps_h_acc), within
  // these bounds. Finer than that adds points, not truth. A straight leg is
  // two points however long it is, a turn gets as many as its curve needs.
  // With RTK the line is within a decimetre of the flight; on an orbit of
  // 30 m that is about as many points as the old fixed 2 m spacing took,
  // on a wide one half of them.
  const TRACK_TOLERANCE_MIN_M = 0.1;
  const TRACK_TOLERANCE_MAX_M = 0.5;
  const TRACK_TOLERANCE_M = 0.25;   // when the receiver does not say
  // Samples held while one segment is open. Bounds the per-sample work; a
  // segment that outgrows it is closed where it is.
  const TRACK_WINDOW_MAX = 400;
  const TRACK_MAX_POINTS = 20000; // fixed vertices, all flights: hours of flying
  // Drawing the line is what costs, above all in 3D, where every update
  // re-renders it into each terrain tile's texture. It is redrawn only when
  // the change is visible, and at most this often.
  const TRACK_PAINT_MIN_PX = 1.5;
  // The recorded points are where the flight left a straight line, which on
  // a 30 m orbit is every 8 m or so: drawn as a polyline, a curve shows its
  // corners. So a curve is DRAWN as one (smoothTrack): a point every
  // TRACK_SMOOTH_STEP_M along a Hermite curve through the recorded points,
  // only where the track really curves. A segment longer than
  // TRACK_SMOOTH_MAX_SEG_M is a leg, and a turn sharper than
  // TRACK_SMOOTH_MAX_TURN a corner: both stay straight, as flown.
  const TRACK_SMOOTH_STEP_M = 1.5;
  const TRACK_SMOOTH_MAX_SEG_M = 20;
  const TRACK_SMOOTH_MAX_TURN = Math.PI / 3;
  const TRACK_PAINT_MS = 100;
  const TRACK_PAINT_MS_3D = 250;
  // The track survives a restart of the app in localStorage. History, not a
  // setting, so it is in settings-transfer.js's NOT_SETTINGS.
  const TRACK_KEY = "corvus.map.track";
  const TRACK_SAVE_DELAY_MS = 5000;
  let trackSaveTimer = null;

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
  // MapLibre GL JS v6.15.0 is vendored locally at src/vendor/maplibre-gl.mjs
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
  let contextCopyTimer = null;  // reverts the copy button's "copied" tick

  // Downloaded-area overlay: the named regions from GET /api/tiles/regions,
  // drawn as outlined rectangles with a DOM label at each centre. Labels are
  // markers rather than a MapLibre symbol layer on purpose — a text layer
  // needs a glyphs URL, and the field laptop has no internet to fetch fonts
  // from (the whole reason these regions exist).
  let regions = [];
  let regionSource = null;
  let regionMarkers = [];
  // Off until asked for: the rectangles are for deciding what to download,
  // not something to fly over.
  let regionsVisible = false;

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
    // Earlier flights first, so the flight in progress is drawn over them
    // where the two cross. Narrower and fainter as well as another colour:
    // the difference must hold where colour alone does not.
    map.addSource("vehicle-path-past", { type: "geojson", data: pastGeometry() });
    map.addLayer({
      id: "path-past-glow",
      source: "vehicle-path-past",
      type: "line",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": ["get", "color"], "line-width": 9, "line-blur": 7, "line-opacity": 0.18 },
    });
    map.addLayer({
      id: "path-past-casing",
      source: "vehicle-path-past",
      type: "line",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-width": 4.6, "line-opacity": 0.45 },
    });
    map.addLayer({
      id: "path-past-line",
      source: "vehicle-path-past",
      type: "line",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": ["get", "color"], "line-width": 2.2, "line-opacity": 0.85 },
    });
    pastSource = map.getSource("vehicle-path-past");
    map.addSource("vehicle-path", { type: "geojson", data: currentGeometry() });
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
   * Distance in metres from *p* to the segment *a*-*b*, in a flat frame
   * around *a*. Clamped to the segment, so flying back along a line counts as
   * leaving it. *zScale* weighs the height difference in (0, or no height on
   * one of the three, leaves it out).
   */
  function metresFromSegment(p, a, b, zScale) {
    const R = 6371000;
    const k = Math.cos((a[1] * Math.PI) / 180);
    const zs = zScale > 0 && a.length > 2 && b.length > 2 && p.length > 2 ? zScale : 0;
    const toXYZ = (c) => [((c[0] - a[0]) * Math.PI / 180) * k * R,
      ((c[1] - a[1]) * Math.PI / 180) * R, zs ? (c[2] - a[2]) * zs : 0];
    const [bx, by, bz] = toXYZ(b);
    const [px, py, pz] = toXYZ(p);
    const len2 = bx * bx + by * by + bz * bz;
    const t = len2 > 0 ? Math.max(0, Math.min(1, (px * bx + py * by + pz * bz) / len2)) : 0;
    return Math.hypot(px - t * bx, py - t * by, pz - t * bz);
  }

  /** Metres between two track points, height weighed in by *zScale*. */
  function metresMoved(a, b, zScale) {
    const flat = metresBetween(a, b);
    if (!(zScale > 0) || a.length < 3 || b.length < 3) return flat;
    return Math.hypot(flat, (b[2] - a[2]) * zScale);
  }

  // The samples since the last fixed vertex, and which track they belong to.
  // Every way the track is replaced (a new flight, a restore, another vehicle,
  // a clear) assigns a new pathCoords array, and the segment closes with it.
  let trackWindow = [];
  let trackWindowOf = null;

  /**
   * Extend the track with *position*. Returns true when the track changed.
   *
   * The last point of the line is the aircraft itself and follows every
   * sample, so the line ends where the aircraft is. It becomes a fixed vertex
   * only when the straight line from the vertex before it to the newest
   * sample would miss one of the samples in between by more than the
   * tolerance (trackTolerance, from *hAcc*): the drawn line is never further than that from where
   * the aircraft flew, with as few points as the shape allows. A hover adds
   * nothing (TRACK_JITTER_M), a straight leg two points, a turn as many as its
   * curve needs, which makes TRACK_MAX_POINTS hours rather than minutes.
   */
  /** The tolerance for a position whose horizontal accuracy is *hAcc* metres. */
  function trackTolerance(hAcc) {
    const acc = Number(hAcc);
    if (!(acc > 0) || !isFinite(acc)) return TRACK_TOLERANCE_M;
    return Math.min(TRACK_TOLERANCE_MAX_M, Math.max(TRACK_TOLERANCE_MIN_M, acc / 4));
  }

  function recordTrackPoint(position, hAcc, altitude) {
    if (!position || position.length < 2) return false;
    const lng = position[0], lat = position[1];
    if (!isFinite(lng) || !isFinite(lat)) return false;
    // [0,0] is the state store's "no fix yet" default, not the Gulf of Guinea.
    if (lng === 0 && lat === 0) return false;
    // The height above home rides along as a third coordinate, for the 3D
    // track. The line layers drape and ignore it.
    const alt = altitude === null || altitude === undefined ? NaN : Number(altitude);
    const p = isFinite(alt) ? [lng, lat, Math.round(alt * 10) / 10] : [lng, lat];
    if (trackWindowOf !== pathCoords) {
      trackWindow = [];
      trackWindowOf = pathCoords;
    }

    const tip = pathCoords[pathCoords.length - 1];
    if (tip && metresMoved(tip, p, TRACK_JITTER_M / TRACK_JITTER_V_M) < TRACK_JITTER_M) return false;

    if (!trackWindow.length) {
      pathCoords.push(p);   // the first point, or a new segment after a fixed vertex
    } else {
      const anchor = pathCoords[pathCoords.length - 2];
      const tolerance = trackTolerance(hAcc);
      const zScale = tolerance / TRACK_TOLERANCE_V_M;
      const strays = trackWindow.length >= TRACK_WINDOW_MAX
        || trackWindow.some((s) => metresFromSegment(s, anchor, p, zScale) > tolerance);
      if (strays) {
        // The tip so far becomes a fixed vertex, and a new segment opens at it.
        trackWindow = [];
        pathCoords.push(p);
        trimTrack();
      } else {
        pathCoords[pathCoords.length - 1] = p;
      }
    }
    if (pathCoords.length > 1) trackWindow.push(p);
    scheduleTrackSave();
    return true;
  }

  /** Over the cap, the OLDEST point goes: the first flight's before the
   *  current one's. shift() rather than slice, one point per new vertex. */
  function trimTrack() {
    if (trackPointCount() <= TRACK_MAX_POINTS) return;
    if (pastFlights.length) {
      pastFlights[0].shift();
      if (pastFlights[0].length < 2) pastFlights.shift();
      paintPast();
    } else {
      pathCoords.shift();
    }
  }

  function trackPointCount() {
    return pastFlights.reduce((n, f) => n + f.length, pathCoords.length);
  }

  /**
   * Close the flight in progress and start the next one.
   *
   * The closed flight moves to pastFlights, where it is drawn in the earlier
   * flights' colour. A single point is not a flight and is simply dropped.
   * Returns true when a flight was closed.
   */
  function startNewFlight() {
    const closed = pathCoords.length >= 2;
    if (closed) pastFlights.push(pathCoords);
    pathCoords = [];
    if (closed) {
      paintPast();
      paintCurrent();
      scheduleTrackSave();
    }
    return closed;
  }

  /**
   * Has a new flight begun with this sample? True on the disarmed to armed
   * edge only: the first sample after a start is a baseline, like the uptime
   * in checkForReboot, so an app opened mid-flight does not split the track.
   */
  function checkForTakeoff(armed) {
    if (typeof armed !== "boolean") return false;
    const previous = lastArmed;
    lastArmed = armed;
    return previous === false && armed;
  }

  function currentGeometry() {
    return {
      type: "Feature", geometry: { type: "LineString", coordinates: smoothTrack(pathCoords) }, properties: {},
    };
  }

  // The drawn points between two recorded ones, keyed by the first of them
  // and checked against its neighbours: only the tip's few segments change
  // between two paints, so the rest is not computed again.
  const smoothCache = new WeakMap();

  /** The track as drawn: *coords* with the curved stretches filled in. */
  function smoothTrack(coords) {
    if (coords.length < 3) return coords;
    const out = [coords[0]];
    for (let i = 0; i < coords.length - 1; i++) {
      const prev = i > 0 ? coords[i - 1] : null;
      const a = coords[i], b = coords[i + 1];
      const next = i + 2 < coords.length ? coords[i + 2] : null;
      let hit = smoothCache.get(a);
      if (!hit || hit.prev !== prev || hit.b !== b || hit.next !== next) {
        hit = { prev, b, next, pts: smoothSegment(prev, a, b, next) };
        smoothCache.set(a, hit);
      }
      for (let j = 0; j < hit.pts.length; j++) out.push(hit.pts[j]);
      out.push(b);
    }
    return out;
  }

  /**
   * The points strictly between *a* and *b* on a cubic Hermite curve whose
   * tangent at each end bisects the turn there. Empty for a straight
   * stretch, a long leg, or where both ends are corners. Worked in a flat
   * frame around *a* (longitude scaled by the latitude), which over a
   * segment of a few metres is exact enough; a height is interpolated
   * straight.
   */
  function smoothSegment(prev, a, b, next) {
    const k = Math.cos((a[1] * Math.PI) / 180);
    const xy = (c) => [(c[0] - a[0]) * k, c[1] - a[1]];
    const B = xy(b);
    const len = Math.hypot(B[0], B[1]);
    const lenM = metresBetween(a, b);
    if (!(len > 0) || lenM > TRACK_SMOOTH_MAX_SEG_M || lenM < 2 * TRACK_SMOOTH_STEP_M) return [];
    const chord = [B[0] / len, B[1] / len];
    const tangent = (from, to) => {
      if (!from || !to) return chord;
      const f = xy(from), t = xy(to);
      // The two segments meeting at this end: the one before and this one,
      // or this one and the one after.
      const u = from === prev ? [-f[0], -f[1]] : B;
      const v = from === prev ? B : [t[0] - B[0], t[1] - B[1]];
      const lu = Math.hypot(u[0], u[1]), lv = Math.hypot(v[0], v[1]);
      if (!(lu > 0) || !(lv > 0)) return chord;
      const cos = (u[0] * v[0] + u[1] * v[1]) / (lu * lv);
      if (Math.acos(Math.max(-1, Math.min(1, cos))) > TRACK_SMOOTH_MAX_TURN) return chord;
      const d = [u[0] / lu + v[0] / lv, u[1] / lu + v[1] / lv];
      const ld = Math.hypot(d[0], d[1]);
      return ld > 0 ? [d[0] / ld, d[1] / ld] : chord;
    };
    const m0 = tangent(prev, a);
    const m1 = tangent(a, next);
    const straight = (m) => Math.abs(m[0] * chord[1] - m[1] * chord[0]) < 1e-6;
    if (straight(m0) && straight(m1)) return [];
    const steps = Math.ceil(lenM / TRACK_SMOOTH_STEP_M);
    const heights = a.length > 2 && b.length > 2;
    const pts = [];
    for (let i = 1; i < steps; i++) {
      const t = i / steps, t2 = t * t, t3 = t2 * t;
      const h10 = (t3 - 2 * t2 + t) * len;
      const h01 = -2 * t3 + 3 * t2;
      const h11 = (t3 - t2) * len;
      const x = h10 * m0[0] + h01 * B[0] + h11 * m1[0];
      const y = h10 * m0[1] + h01 * B[1] + h11 * m1[1];
      const p = [a[0] + x / k, a[1] + y];
      if (heights) p.push(a[2] + (b[2] - a[2]) * t);
      pts.push(p);
    }
    return pts;
  }

  /**
   * The colour each earlier flight is drawn in, oldest first. One tone for
   * all of them, or a tone of its own each when the rainbow is on, so that
   * flights drawn over the same ground can still be told apart.
   */
  function pastColors() {
    const track = vehicleColor || Corvus.ui.token("--track", "#E4322F");
    if (!trackEarlierFlights) return pastFlights.map(() => track);
    const fleet = Corvus.fleet;
    if (trackRainbow && fleet && fleet.rainbowTone) return pastFlights.map((_f, i) => fleet.rainbowTone(i));
    const tone = vehicleColor && fleet
      ? fleet.tint(vehicleColor, 0.45) : Corvus.ui.token("--track-past", "#E88A7E");
    return pastFlights.map(() => tone);
  }

  function pastGeometry() {
    const colors = pastColors();
    return {
      type: "FeatureCollection",
      features: pastFlights.map((coordinates, i) => ({
        type: "Feature",
        geometry: { type: "LineString", coordinates: smoothTrack(coordinates) },
        properties: { color: colors[i] },
      })),
    };
  }

  // What was last handed to MapLibre, for requestTrackPaint.
  let trackPaintTimer = null;
  let trackPaintedAt = 0;
  let trackPaintedTip = null;
  let trackPaintedLength = 0;
  // An operator's drag or zoom is under way (movestart with an input event).
  let operatorMoving = false;

  function paintCurrent() {
    if (trackPaintTimer !== null) window.clearTimeout(trackPaintTimer);
    trackPaintTimer = null;
    trackPaintedAt = Date.now();
    trackPaintedLength = pathCoords.length;
    const tip = pathCoords[pathCoords.length - 1];
    trackPaintedTip = tip ? tip.slice() : null;
    if (pathSource) pathSource.setData(currentGeometry());
    requestTrack3DFrame();
  }

  /** Metres one screen pixel covers at *lat* in the current view. */
  function metresPerPixel(lat) {
    const zoom = map && typeof map.getZoom === "function" ? Number(map.getZoom()) : NaN;
    if (!isFinite(zoom)) return 0;
    return 40075016.686 * Math.cos((lat * Math.PI) / 180) / (512 * Math.pow(2, zoom));
  }

  /**
   * Redraw the flight in progress when the operator would see a difference.
   *
   * A new fixed vertex always counts. A tip that moved less than
   * TRACK_PAINT_MIN_PX on screen does not, which is most samples when zoomed
   * out. What does count is drawn at most every TRACK_PAINT_MS
   * (TRACK_PAINT_MS_3D in 3D), and not while the operator drags the map,
   * where a re-cut line costs the frames the drag needs; it catches up once
   * the map is let go.
   */
  function requestTrackPaint() {
    if (!pathSource) return;
    const tip = pathCoords[pathCoords.length - 1];
    // One new corner since the last draw is the tip that was drawn, fixed
    // where it stood: unseen unless it or the new tip is a visible distance
    // from what is on screen. More than one, or fewer points (the cap), is.
    const added = pathCoords.length - trackPaintedLength;
    const mpp = tip ? metresPerPixel(tip[1]) : 0;
    if (tip && trackPaintedTip && mpp > 0 && (added === 0 || added === 1)) {
      const px = (c) => metresBetween(trackPaintedTip, c) / mpp;
      const corner = added === 1 ? pathCoords[pathCoords.length - 2] : null;
      if (px(tip) < TRACK_PAINT_MIN_PX && (!corner || px(corner) < TRACK_PAINT_MIN_PX)) return;
    }
    if (trackPaintTimer !== null) return;
    const interval = threeD ? TRACK_PAINT_MS_3D : TRACK_PAINT_MS;
    const wait = Math.max(0, interval - (Date.now() - trackPaintedAt));
    trackPaintTimer = window.setTimeout(flushTrackPaint, wait);
  }

  function flushTrackPaint() {
    trackPaintTimer = null;
    if (operatorMoving) {
      trackPaintTimer = window.setTimeout(flushTrackPaint, TRACK_PAINT_MS_3D);
      return;
    }
    paintCurrent();
  }

  function paintPast() {
    if (pastSource) pastSource.setData(pastGeometry());
    track3dPalette = null;
    requestTrack3DFrame();
  }

  function fleetHasTracks() {
    return !!(Corvus.mapFleet && Corvus.mapFleet.hasTracks());
  }

  function trackStorage() {
    try { return window.localStorage || null; } catch (_e) { return null; }
  }

  /** Write the track a few seconds after it changed, not on every point. */
  function scheduleTrackSave() {
    if (trackSaveTimer !== null) return;
    trackSaveTimer = window.setTimeout(saveTrack, TRACK_SAVE_DELAY_MS);
  }

  function saveTrack() {
    if (trackSaveTimer !== null) window.clearTimeout(trackSaveTimer);
    trackSaveTimer = null;
    const storage = trackStorage();
    if (!storage) return;
    // Seven decimals is about a centimetre, below the finest tolerance, and
    // keeps a full track well inside the storage quota.
    const round = (flight) => flight.map((c) => (c.length > 2
      ? [+c[0].toFixed(7), +c[1].toFixed(7), +c[2].toFixed(1)]
      : [+c[0].toFixed(7), +c[1].toFixed(7)]));
    try {
      if (!pastFlights.length && pathCoords.length < 2) {
        storage.removeItem(TRACK_KEY);
      } else {
        storage.setItem(TRACK_KEY, JSON.stringify({
          v: 1, past: pastFlights.map(round), current: round(pathCoords),
          owner: trackOwner || undefined,
        }));
      }
    } catch (_e) { /* quota or storage off: the track still lives in memory */ }
  }

  /** The track the last session left, read back once at init. */
  function restoreTrack() {
    const storage = trackStorage();
    if (!storage) return;
    let saved = null;
    try { saved = JSON.parse(storage.getItem(TRACK_KEY) || "null"); } catch (_e) { return; }
    if (!saved || saved.v !== 1) return;
    const valid = (flight) => Array.isArray(flight) && flight.every((c) =>
      Array.isArray(c) && (c.length === 2 || (c.length === 3 && isFinite(c[2])))
      && isFinite(c[0]) && isFinite(c[1]));
    pastFlights = (Array.isArray(saved.past) ? saved.past : [])
      .filter((f) => valid(f) && f.length >= 2);
    pathCoords = valid(saved.current) ? saved.current : [];
    trackOwner = typeof saved.owner === "string" && saved.owner ? saved.owner : null;
  }

  /**
   * Is this sample about another vehicle than the track is? Then the track
   * goes with the vehicle it belongs to, and the new one's comes back.
   *
   * The first vehicle ever heard simply takes the track as it is, so a single
   * aircraft keeps the track it always had.
   */
  function noteVehicle(state) {
    const id = state && typeof state.vehicle_id === "string" ? state.vehicle_id : "";
    if (!id || id === trackOwner) return false;
    if (trackOwner === null) {
      trackOwner = id;
      applyVehicleColor();
      return false;
    }
    const fleetMap = Corvus.mapFleet;
    if (fleetMap) fleetMap.stashTrack(trackOwner, { current: pathCoords, past: pastFlights });
    const next = fleetMap ? fleetMap.takeTrack(id) : null;
    pathCoords = next ? next.current : [];
    pastFlights = next ? next.past : [];
    trackOwner = id;
    // The new vehicle's own uptime and armed state are the baseline, not the
    // last vehicle's: comparing the two would read as a reboot or a takeoff.
    lastBootMs = null;
    lastArmed = null;
    // Nor is the marker eased across from where the last one was: it is
    // hidden until this one reports a position, and its first one snaps.
    vehTarget = null;
    vehDisplay = null;
    const markerEl = vehicleMarker && vehicleMarker.getElement();
    if (markerEl) markerEl.hidden = true;
    paintCurrent();
    paintPast();
    saveTrack();
    updateTrackControl();
    applyVehicleColor();
    return true;
  }

  /** Paint the flown vehicle's marker and track in its colour from the list. */
  function applyVehicleColor() {
    const fleet = Corvus.fleet;
    const color = fleet && trackOwner ? fleet.colorOf(trackOwner) : "";
    const name = fleet && trackOwner ? fleet.nameOf(trackOwner) : "";
    const multi = !!(fleet && fleet.isMulti());
    const markerEl = vehicleMarker && vehicleMarker.getElement();
    if (markerEl && fleet) {
      if (!vehicleLabelEl) {
        vehicleLabelEl = document.createElement("span");
        vehicleLabelEl.className = "v-label";
        markerEl.appendChild(vehicleLabelEl);
      }
      vehicleLabelEl.textContent = name;
      vehicleLabelEl.hidden = !multi || !name;
    }
    if (fleet) {
      [markerEl, veh3dEl, veh3dShadowEl, veh3dLeadEl, veh3dAltEl].forEach((el) => {
        fleet.paintElement(el, color);
      });
    }
    if (color === vehicleColor) return;
    vehicleColor = color;
    repaintTrack();
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

  /** Discard the flown track, every flight of it, and repaint. */
  function clearTrack() {
    pathCoords = [];
    pastFlights = [];
    if (Corvus.mapFleet) Corvus.mapFleet.clearTracks();
    paintCurrent();
    paintPast();
    saveTrack();
    updateTrackControl();
    // The plan line starts at the vehicle, not at the track, so it is
    // unaffected — but it shares the source update cadence, so keep it honest.
    if (waypoints.length > 0) updateRoute();
  }

  /** Show the clear-track control only while there is a track to clear. */
  function updateTrackControl() {
    if (!trackClearEl) return;
    trackClearEl.hidden = pathCoords.length < 2 && pastFlights.length === 0 && !fleetHasTracks();
  }

  /**
   * Apply the four track settings. *startup* is true for the config read when
   * the app starts, the one moment "clear on restart" means discarding what
   * the last session left.
   */
  function setTrackOptions(opts, startup) {
    const o = opts || {};
    trackEarlierFlights = o.earlierFlights !== false;
    trackRainbow = o.rainbow === true;
    trackClearOnRestart = o.clearOnRestart === true;
    trackInAir = o.inAir !== false;
    if (startup && trackClearOnRestart) clearTrack();
    repaintTrack();
    requestTrack3DFrame();
  }

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

  /* Hold a map's container at one width while the right panel slides.

     MapLibre watches its container and answers every change with resize(),
     which reallocates the WebGL drawing buffer and redraws everything,
     terrain and buildings included. Left alone, the slide's width transition
     triggered that five or six times out of step with the frames, which was
     the stutter. Pinned, the container changes size once: at the start when
     the panel closes (the extra width is still hidden under the panel), at
     the end when it opens (the lost width is already under it).

     resize() keeps the centre, which would shift the picture by half the
     change; panning by that half keeps it anchored on the left edge, the way
     the panel uncovers and covers it. Following too: the aircraft stays where
     it is on screen, and the follow box moves the camera only once it leaves
     the box, so the map never jumps under the operator.

     Under a tilted camera or over terrain a pan is not a shift: it moves the
     camera, the foreground travels further than the horizon, and the picture
     shears. There the vanishing point is held on its pixel instead, through
     the padding, so the picture does not move at all. It is then off the
     middle by half the panel, which is harmless while the camera is still;
     the next programmatic move (following, centring, the 3D switch) takes it
     back as part of a motion the map is making anyway (panelRestPadding).
     Returns the unsubscribe. */
  const panelRest = new WeakMap();

  /** The padding a map held by holdThroughPanel goes back to on its next
   *  camera move, or null when it is already there. */
  function panelRestPadding(m) {
    return (m && panelRest.get(m)) || null;
  }

  function holdThroughPanel(m) {
    const panelApi = Corvus.panel;
    if (!m || !panelApi || typeof panelApi.onMotion !== "function") return () => {};
    const container = m.getContainer && m.getContainer();
    const canvas = m.getCanvas && m.getCanvas();
    if (!container || !canvas) return () => {};
    let phase = null;
    let lastWidth = 0;
    let settleTimer = null;

    const release = () => {
      container.style.width = "";
      container.style.right = "";
    };
    const tilted = () => {
      if (typeof m.getPadding !== "function" || typeof m.setPadding !== "function") return false;
      const pitch = typeof m.getPitch === "function" ? m.getPitch() : 0;
      const terrain = typeof m.getTerrain === "function" && m.getTerrain();
      return pitch >= 0.5 || !!terrain;
    };
    const samePadding = (a, b) => ["top", "bottom", "left", "right"]
      .every((side) => Math.abs((a[side] || 0) - (b[side] || 0)) < 0.5);
    const onMoveEnd = () => {
      const base = panelRest.get(m);
      if (base && !phase && samePadding(m.getPadding(), base)) panelRest.delete(m);
    };
    m.on("moveend", onMoveEnd);

    const onResize = () => {
      if (!phase) return;
      const width = canvas.clientWidth;
      const before = lastWidth;
      lastWidth = width;
      if (phase === "settling") phase = null;
      if (width === before) return;
      const base = panelRest.get(m);
      if (base) {
        // Keep the vanishing point on its pixel: a new width moves the middle
        // by half the change, a padding of the whole change moves it back.
        // Kept on one side only, so closing what opening added cancels out.
        // Still inside resize(), so the step is never painted.
        const padding = m.getPadding();
        const offset = (padding.right - base.right) - (padding.left - base.left) + (width - before);
        m.setPadding(Object.assign({}, padding, {
          right: base.right + Math.max(0, offset),
          left: base.left + Math.max(0, -offset),
        }));
        return;
      }
      // The centre is where the old middle of the picture was; put it back
      // at that pixel. On a flat map one pan of half the change does it. On
      // the globe and over terrain a pan is not a pure shift, so measure
      // where it landed and correct, a few times at most. All of it runs
      // inside resize(), before the redraw, so no step is ever painted.
      const anchor = m.getCenter && m.getCenter();
      const target = before / 2;
      let error = (width - before) / 2;
      for (let step = 0; step < 4 && Math.abs(error) >= 0.5; step++) {
        m.panBy([error, 0], { animate: false });
        if (!anchor || typeof m.project !== "function") break;
        error = m.project(anchor).x - target;
      }
    };
    m.on("resize", onResize);

    const off = panelApi.onMotion(({ phase: step, growth }) => {
      if (step === "start") {
        release();
        const natural = container.clientWidth;
        if (!natural) { phase = null; return; }
        if (settleTimer) { clearTimeout(settleTimer); settleTimer = null; }
        if (!phase) lastWidth = canvas.clientWidth;
        phase = "moving";
        if (!panelRest.has(m) && tilted()) panelRest.set(m, m.getPadding());
        container.style.width = `${natural + Math.max(0, growth || 0)}px`;
        container.style.right = "auto";
      } else if (step === "end" && phase) {
        release();
        // When the released width equals the pinned one no resize follows,
        // so the anchoring must not linger for an unrelated one later.
        phase = "settling";
        settleTimer = setTimeout(() => { phase = null; settleTimer = null; onMoveEnd(); }, 250);
      }
    });
    return () => {
      off();
      m.off("resize", onResize);
      m.off("moveend", onMoveEnd);
      if (settleTimer) clearTimeout(settleTimer);
      release();
      phase = null;
    };
  }

  /** Push the current theme's track colours onto the track layers. */
  function repaintTrack() {
    if (!map) return;
    track3dPalette = null;
    const track = vehicleColor || Corvus.ui.token("--track", "#E4322F");
    // The casing is the page background, not black: on the light theme a black
    // outline would be heavier than the track it is meant to support.
    const casing = Corvus.ui.token("--bg", "#0B0E12");
    const set = (layer, prop, value) => {
      if (map.getLayer(layer)) map.setPaintProperty(layer, prop, value);
    };
    set("path-glow", "line-color", track);
    set("path-casing", "line-color", casing);
    set("path-line", "line-color", track);
    set("path-past-casing", "line-color", casing);
    paintPast();
    if (Corvus.mapFleet && Corvus.mapFleet.repaint) Corvus.mapFleet.repaint();
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

  /** Two {w,s,e,n} boxes cover nearly the same ground: their overlap is at
   *  least `share` of the larger one. */
  function sameGround(a, b, share) {
    const area = (x) => Math.max(0, x.e - x.w) * Math.max(0, x.n - x.s);
    const overlap = area({
      w: Math.max(a.w, b.w), e: Math.min(a.e, b.e),
      s: Math.max(a.s, b.s), n: Math.min(a.n, b.n),
    });
    const larger = Math.max(area(a), area(b));
    if (larger === 0) return a.w === b.w && a.s === b.s && a.e === b.e && a.n === b.n;
    return overlap / larger >= share;
  }

  /**
   * The downloaded areas as they are drawn: one entry per piece of ground.
   * Downloading an area again (another source, a deeper zoom, a retry) used
   * to stack a second rectangle and a second label on the same spot, which
   * made both unreadable. Areas that cover nearly the same ground are merged
   * into one box with one label: the names once each, the zoom range of all
   * of them, and "downloading" while any of them still is. Pure, so both
   * maps draw the same thing.
   */
  function mergeRegions(list, skipSource) {
    const groups = [];
    (Array.isArray(list) ? list : []).forEach((r) => {
      if (!r || (skipSource && r.source === skipSource)) return;
      const b = r.bounds || {};
      const box = { w: Number(b.w), s: Number(b.s), e: Number(b.e), n: Number(b.n) };
      if (![box.w, box.s, box.e, box.n].every(isFinite)) return;
      const group = groups.find((g) => sameGround(g.bounds, box, 0.9));
      if (!group) {
        groups.push({
          id: r.id, ids: [r.id], names: [r.name || "Region"], bounds: box,
          minzoom: r.minzoom, maxzoom: r.maxzoom, state: r.state,
        });
        return;
      }
      group.ids.push(r.id);
      const name = r.name || "Region";
      if (group.names.indexOf(name) < 0) group.names.push(name);
      group.bounds = {
        w: Math.min(group.bounds.w, box.w), s: Math.min(group.bounds.s, box.s),
        e: Math.max(group.bounds.e, box.e), n: Math.max(group.bounds.n, box.n),
      };
      if (isFinite(r.minzoom)) {
        group.minzoom = isFinite(group.minzoom) ? Math.min(group.minzoom, r.minzoom) : r.minzoom;
      }
      if (isFinite(r.maxzoom)) {
        group.maxzoom = isFinite(group.maxzoom) ? Math.max(group.maxzoom, r.maxzoom) : r.maxzoom;
      }
      if (r.state === "running") group.state = "running";
    });
    return groups.map((g) => Object.assign(g, { name: g.names.join(", ") }));
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
    const drawn = mergeRegions(regions.filter((r) => !isTerrainSource(r && r.source)));

    regionSource.setData({
      type: "FeatureCollection",
      features: regionsVisible ? drawn.map((r) => ({
        type: "Feature",
        geometry: { type: "Polygon", coordinates: boundsRing(r.bounds) },
        properties: { id: r.id, name: r.name || "" },
      })) : [],
    });

    regionMarkers.forEach((m) => m.remove());
    regionMarkers = [];
    if (!regionsVisible) return;
    drawn.forEach((r) => {
      const b = r.bounds;
      const centre = [((b.w + b.e) / 2), ((b.s + b.n) / 2)];
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

  // ---- Overlays: what a plugin draws on this map --------------------------
  // In js/map-overlays.js; these are its handles.
  const {
    overlays, overlayIds, cleanOverlayCoords, drawOverlay,
    setOverlay, setPolygonOverlay, setTextOverlay,
    setButtonOverlay, setMarkerOverlay, removeOverlay, setOverlayVisible,
  } = Corvus.mapOverlays;

  /** Wait for one click on the map (a plugin placing something). Planning
   *  mode would take the same click for a waypoint, so it ends first. */
  function pickPoint(onPick, opts) {
    if (!map || !started) return null;
    setWaypointMode(false);
    stopMeasure();
    closeContextMenu();
    return Corvus.mapOverlays.pickPoint(onPick, opts);
  }

  /** Bring the Home map onto these [lng, lat] points, and stop following the
   *  aircraft so the view is not pulled straight back to it. */
  function fitCoords(coords) {
    const clean = cleanOverlayCoords(coords);
    if (!map || !clean.length) return false;
    let w = Infinity, s = Infinity, e = -Infinity, n = -Infinity;
    clean.forEach(([lng, lat]) => {
      w = Math.min(w, lng); e = Math.max(e, lng);
      s = Math.min(s, lat); n = Math.max(n, lat);
    });
    noteHomeAim();
    setFollow(false);
    // The operator asked for this view, so a zoom still pending from an
    // earlier move of ours (the first fix's) must not take it back on the
    // next moveend. A pending tilt is kept: framing does not flatten 3D.
    cameraGoal.zoom = null;
    // The Home page may have come forward a moment ago (api.map.fit brings it
    // there), and MapLibre only hears of its new size on the next frame. The
    // fit is worked out against the size the map is now, so measure first.
    const container = map.getContainer && map.getContainer();
    const canvas = map.getCanvas && map.getCanvas();
    if (container && canvas && typeof map.resize === "function"
        && (container.clientWidth !== canvas.clientWidth
            || container.clientHeight !== canvas.clientHeight)) {
      map.resize();
    }
    map.fitBounds([[w, s], [e, n]], {
      padding: 60,
      maxZoom: 18,
      // MapLibre turns the map north up on a fit unless told otherwise; the
      // operator's rotation is theirs.
      bearing: typeof map.getBearing === "function" ? map.getBearing() : 0,
      linear: true,
      duration: Corvus.anim.reducedMotion() ? 0 : 800,
    });
    return true;
  }

  function initialStyle(key) {
    const spec = specFor(key);
    // The coarse copy first, as in setBaseLayer, which explains why.
    const sources = {
      "ground-imagery": {
        type: "raster",
        tiles: [tileUrl(key)],
        tileSize: 256,
        maxzoom: Math.min(GROUND_IMAGERY_MAXZOOM, spec.maxzoom),
      },
      base: {
        type: "raster",
        tiles: [tileUrl(key)],
        tileSize: 256,
        maxzoom: spec.maxzoom,
        attribution: spec.attribution,
      },
    };
    const layers = [
      { id: "terrain-ground-imagery", type: "raster", source: "ground-imagery",
        layout: { visibility: "none" } },
      { id: "base", type: "raster", source: "base" },
    ];
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
    for (const id of ["base", "terrain-ground-imagery"]) {
      if (map.getLayer(id)) map.removeLayer(id);
    }
    for (const id of ["base", "ground-imagery"]) {
      if (map.getSource(id)) map.removeSource(id);
    }
    // The coarse copy goes in FIRST: MapLibre asks for tiles source by
    // source in the order they were added, so after a pan onto new ground
    // its handful of tiles are at the head of the queue rather than behind
    // the eighty the detail wants. Hidden on the flat map, where it asks for
    // nothing (see setGround).
    map.addSource("ground-imagery", {
      type: "raster",
      tiles: [tileUrl(key)],
      tileSize: 256,
      maxzoom: Math.min(GROUND_IMAGERY_MAXZOOM, spec.maxzoom),
    });
    map.addLayer({
      id: "terrain-ground-imagery", type: "raster", source: "ground-imagery",
      layout: { visibility: threeD ? "visible" : "none" },
    }, firstOverlayLayer());
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
    // A map service with its own elevation model brings it along.
    chooseTerrain();
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
    "terrain-hillshade",
    "offline-regions-fill", "offline-regions-line",
    "buildings-3d",
    "path-past-glow", "path-past-casing", "path-past-line",
    "path-glow", "path-casing", "path-line",
    "waypoints-route",
  ];

  function firstOverlayLayer() {
    const fixed = OVERLAY_LAYERS.find((id) => map.getLayer(id));
    if (fixed) return fixed;
    // A plugin's line, before the regions are in: still not under the imagery.
    for (const id of overlays.keys()) {
      const ids = overlayIds(id);
      if (map.getLayer(ids.fill)) return ids.fill;
      if (map.getLayer(ids.casing)) return ids.casing;
    }
    return undefined;
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
      { id: "regions", icon: "frame", title: "Show downloaded areas" },
      { id: "three", icon: "box", title: "3D mode" },
      { id: "measure", icon: "ruler", title: "Measure a distance (click two points)" },
      { id: "divider", group: "video" },
      { id: "video", icon: "video", title: "Camera windows (set up under Setup, Video)" },
    ];
    items.forEach((it) => {
      if (it.id === "divider") {
        const d = document.createElement("div");
        d.className = "mc-divider";
        if (it.group) d.dataset.group = it.group;
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
      } else if (act === "measure") {
        toggleMeasure();
      } else if (act === "video" && Corvus.videoWindows) {
        pressCameraButton();
      }
    });

    wireCameraButton(container);

    wireThreeDPanel(container.querySelector('[data-act="three"]'));

    if (Corvus.mapMeasure) {
      Corvus.mapMeasure.attach(map, (on) => {
        const b = container.querySelector('[data-act="measure"]');
        if (b) b.classList.toggle("active", on);
      });
    }
  }

  /** Measuring and planning both own the click, so turning one on ends the other. */
  function toggleMeasure() {
    if (!Corvus.mapMeasure) return;
    if (!Corvus.mapMeasure.isActive()) {
      setWaypointMode(false);
      Corvus.mapOverlays.cancelPick();
      closeContextMenu();
    }
    Corvus.mapMeasure.toggle();
  }

  function stopMeasure() {
    if (Corvus.mapMeasure && Corvus.mapMeasure.isActive()) Corvus.mapMeasure.setActive(false);
  }

  /** Show or hide the rail's ruler. Hiding it also ends a measurement. */
  function setMeasureTool(on) {
    const b = controlsEl && controlsEl.querySelector('[data-act="measure"]');
    if (!on) stopMeasure();
    if (b) b.hidden = !on;
  }

  /**
   * The rail's camera button, which only exists while there is a camera.
   *
   * With none set up it is hidden (with the divider above it) rather than a
   * button that only says where to add one. With one camera a press opens or
   * closes its window.
   *
   * With several, the press still opens or closes ONE window: the camera
   * picked last, so the common case (the same camera, flight after flight) is
   * one press. The list of all of them is the button's second gesture, the
   * same one the 3D button has: the pointer resting on it, keyboard focus, or
   * a long press or right-click. One row per camera, marked while its window
   * is open, and a last row that opens or closes all of them. The list stays
   * open, so several cameras can be picked in one go.
   *
   * It is lit while any camera window is open, whichever button or page
   * opened it.
   */
  function wireCameraButton(container) {
    const vw = Corvus.videoWindows;
    const btn = container.querySelector('[data-act="video"]');
    const divider = container.querySelector('.mc-divider[data-group="video"]');
    if (!btn) return;
    if (!vw || typeof vw.onCameras !== "function") {
      btn.hidden = true;
      if (divider) divider.hidden = true;
      return;
    }
    const show = (list) => {
      const any = list.length > 0;
      btn.hidden = !any;
      if (divider) divider.hidden = !any;
      btn.title = any ? `Camera: ${cameraLabel(lastCamera(list))}` : "";
      btn.setAttribute("aria-haspopup", list.length > 1 ? "menu" : "false");
      if (cameraMenuHandle && cameraMenuHandle.isOpen()) {
        if (list.length > 1) cameraMenuHandle.rebuild();
        else closeCameraList(false);
      }
    };
    show([]);
    vw.onCameras(show);
    vw.onChange((n) => {
      btn.classList.toggle("active", n > 0);
      if (cameraMenuHandle && cameraMenuHandle.isOpen()) cameraMenuHandle.rebuild();
    });
    wireCameraHover(btn);
    vw.loadCameras();
  }

  function cameraLabel(cam) {
    return String((cam && (cam.name || cam.address)) || "Camera");
  }

  // Which camera the press opens, per browser profile: it describes this
  // operator's habit, not the vehicle, so it is not worth a config key.
  const LAST_CAMERA_KEY = "corvus.map.lastCamera";
  // Only consulted when storage cannot be read (a private window).
  let lastCameraId = "";

  function readLastCamera() {
    try { return String(window.localStorage.getItem(LAST_CAMERA_KEY) || ""); }
    catch (_e) { return lastCameraId; }
  }

  function rememberCamera(cam) {
    if (!cam || !cam.id) return;
    lastCameraId = String(cam.id);
    try { window.localStorage.setItem(LAST_CAMERA_KEY, lastCameraId); } catch (_e) {}
    const b = controlsEl && controlsEl.querySelector('[data-act="video"]');
    if (b) b.title = `Camera: ${cameraLabel(cam)}`;
  }

  /** The camera picked last, or the first one when that one is gone. */
  function lastCamera(list) {
    const id = readLastCamera();
    return list.find((c) => c.id === id) || list[0] || null;
  }

  function pressCameraButton() {
    const vw = Corvus.videoWindows;
    const list = vw.cameras();
    if (list.length > 0) vw.toggle(lastCamera(list));
    else vw.toggleAll();
  }

  let cameraMenuHandle = null;
  let cameraHoverTimer = null;

  /** The list the camera button reveals when there is more than one camera. */
  function cameraMenu() {
    if (cameraMenuHandle) return cameraMenuHandle;
    cameraMenuHandle = Corvus.ui.menu({
      className: "layers-popover camera-popover",
      role: "menu",
      ariaLabel: "Cameras",
      side: "left",
      gap: railMenuGap,
      matchAnchorWidth: false,
      // Revealed by pointing, like the 3D panel: it must not take the keyboard.
      autofocus: false,
      render: renderCameraRows,
      onOpen: () => markCameraList(true),
      onClose: () => markCameraList(false),
    });
    return cameraMenuHandle;
  }

  function markCameraList(open) {
    const b = controlsEl && controlsEl.querySelector('[data-act="video"]');
    if (b) b.setAttribute("aria-expanded", open ? "true" : "false");
  }

  /** The 3D panel's hover, focus and long-press wiring, for the camera list. */
  function wireCameraHover(btn) {
    btn.addEventListener("mouseenter", () => scheduleCameraList(true));
    btn.addEventListener("mouseleave", () => scheduleCameraList(false));
    btn.addEventListener("focus", () => openCameraList());
    btn.addEventListener("blur", (e) => {
      const to = e && e.relatedTarget;
      if (to && cameraMenuHandle && withinNode(to, cameraMenuHandle.el)) return;
      closeCameraList(true);
    });
    btn.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      openCameraList();
    });
    const surface = cameraMenu().el;
    surface.addEventListener("mouseenter", () => scheduleCameraList(true));
    surface.addEventListener("mouseleave", () => scheduleCameraList(false));
  }

  function clearCameraHoverTimer() {
    if (cameraHoverTimer) window.clearTimeout(cameraHoverTimer);
    cameraHoverTimer = null;
  }

  function scheduleCameraList(open) {
    clearCameraHoverTimer();
    cameraHoverTimer = window.setTimeout(
      () => { cameraHoverTimer = null; if (open) openCameraList(); else closeCameraList(false); },
      open ? THREE_D_HOVER_OPEN_MS : THREE_D_HOVER_CLOSE_MS);
  }

  function openCameraList() {
    clearCameraHoverTimer();
    const btn = controlsEl && controlsEl.querySelector('[data-act="video"]');
    const vw = Corvus.videoWindows;
    if (!btn || btn.hidden || !vw || vw.cameras().length < 2) return;
    if (cameraMenu().isOpen()) return;
    cameraMenu().open({ el: btn });
  }

  function closeCameraList(refocus) {
    clearCameraHoverTimer();
    if (cameraMenuHandle) cameraMenuHandle.close(!!refocus);
  }

  function renderCameraRows(surface) {
    const vw = Corvus.videoWindows;
    const head = document.createElement("div");
    head.className = "ui-menu-head";
    head.textContent = "Cameras";
    surface.appendChild(head);
    const rows = vw.cameras().map((cam) => {
      const on = vw.has(cam.id);
      const row = Corvus.ui.menuItem({
        label: cameraLabel(cam),
        note: cam.name && cam.address ? cam.address : "",
        dot: true,
        active: on,
        role: "menuitemcheckbox",
        onSelect: () => { rememberCamera(cam); vw.toggle(cam); },
      });
      row.setAttribute("aria-checked", on ? "true" : "false");
      if (cam.address) row.title = String(cam.address);
      surface.appendChild(row);
      return row;
    });
    const anyOpen = vw.count() > 0;
    const all = Corvus.ui.menuItem({
      label: anyOpen ? "Close all" : "Open all",
      icon: anyOpen ? "x" : "layout-grid",
      onSelect: () => {
        if (anyOpen) vw.closeAll();
        else vw.cameras().forEach((cam) => vw.open(cam));
      },
    });
    surface.appendChild(all);
    rows.push(all);
    return rows;
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
    // So does a caption that changes (ARM is DISARM once armed) or a button
    // that comes and goes: the buttons share the longest word's caption size.
    // Only a real change refits, since the arm caption is rewritten on every
    // telemetry frame.
    if (typeof MutationObserver === "function") {
      const captions = () => Array.from(bar.children)
        .map((el) => (el.hidden ? "" : el.textContent.trim())).join("|");
      let seen = captions();
      new MutationObserver(() => {
        const now = captions();
        if (now === seen) return;
        seen = now;
        fit();
      }).observe(bar, {
        subtree: true, childList: true, characterData: true,
        attributes: true, attributeFilter: ["hidden"],
      });
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
   * Fill the open 3D panel: the buildings switch and the elevation model,
   * the two things the button itself cannot say.
   *
   * There is no "off" row. Off is what the button does — a mode that can be
   * reached two ways is a mode the operator has to work out which way they
   * reached, and the panel exists to answer a different question: WHICH 3D
   * the button will give them.
   *
   * The hint is the point of the panel, not decoration. Both modes draw the
   * terrain; what separates them is that one asks the backend for building
   * footprints, which the first time over an area is an Overpass round trip
   * per cell. On a field laptop over a radio link that is the whole
   * decision, and it is invisible unless it is written down.
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
      ariaLabel: "Buildings",
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
      label: "Buildings",
      control: threeDPicker.el,
      className: "field-switch",
      hint: "OpenStreetMap buildings, kept for offline use. Relief is "
        + "always shown. Own elevation profiles, GeoTIFF and surface "
        + "model (DOM) data can be added in the settings.",
    });
    surface.appendChild(field);

    const rows = renderTerrainRows(surface);
    // The rows the keyboard walks. A menu with nothing to walk does not open
    // at all (see Corvus.ui.menu), so the switch alone is load-bearing.
    return [threeDPicker.el, ...rows];
  }

  /**
   * The elevation model list in the 3D panel.
   *
   * "Automatic" first, with what it resolves to right now as its second line:
   * the rule (the map service's own model, else Copernicus) is only useful if
   * the operator can see where it landed. A keyed model is listed only while
   * its key is set, the same rule the layer switcher applies to keyed map
   * services: offered without its key it would only ever draw the fallback.
   */
  function renderTerrainRows(surface) {
    if (!terrainCatalogue.length) return [];
    const head = document.createElement("div");
    head.className = "ui-menu-head";
    head.textContent = "Elevation";
    surface.appendChild(head);

    const provider = activeLayer && sources[activeLayer]
      ? sources[activeLayer].provider : null;
    const auto = terrainFor(terrainCatalogue, provider, defaultTerrainId,
      TERRAIN_AUTO, preferredTerrainId);
    const options = [{
      id: TERRAIN_AUTO,
      label: auto ? `Automatic (${auto.label || auto.id})` : "Automatic",
    }];
    // The preferred model first: it is the one "Automatic" usually lands on.
    const usable = terrainCatalogue.filter(terrainUsable);
    usable.sort((a, b) => (b.id === preferredTerrainId) - (a.id === preferredTerrainId));
    usable.forEach((d) => {
      options.push({ id: d.id, label: d.label || d.id });
    });
    const picker = Corvus.ui.optionList({
      ariaLabel: "Elevation model",
      value: options.some((o) => o.id === terrainChoice) ? terrainChoice : TERRAIN_AUTO,
      options,
      onChange: (id) => {
        setTerrainSource(id);
        persistThreeD();
      },
    });
    surface.appendChild(picker.el);
    return Array.from(picker.el.children);
  }

  /**
   * Choose the elevation model: a DEM id, or "auto". Applied at once when 3D
   * is showing, remembered otherwise. Returns the choice that stands.
   */
  function setTerrainSource(id) {
    terrainChoice = (typeof id === "string" && id) ? id : TERRAIN_AUTO;
    chooseTerrain();
    return terrainChoice;
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
        ? "3D: terrain & buildings" : "3D: terrain")
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
      // The DEMs 3D mode can read heights from, including their encoding and
      // tile size — stated once in corvus/tile_sources.py and never mirrored
      // here. A backend that serves none leaves terrainSpec null, and 3D
      // degrades to a tilt over flat ground rather than failing.
      terrainCatalogue = (data && data.terrain) || [];
      defaultTerrainId = (data && data.default_terrain)
        || (terrainCatalogue[0] && terrainCatalogue[0].id) || null;
      preferredTerrainId = (data && data.preferred_terrain) || null;
      // 3D may already be on: the rail works from the first frame, and this
      // catalogue arrives over the network. Re-applying the mode is how a
      // press that landed before the DEM was known still gets its terrain,
      // instead of a tilt over flat ground for the rest of the session.
      if (!chooseTerrain() && threeD) set3D(true);
      // Which regions are elevation (and so not drawn) is only known now.
      setRegions(regions);
      if (layersMenuHandle) layersMenuHandle.rebuild();
      // The active layer's attribution/maxzoom may have been the bootstrap
      // fallback until now; re-apply so MapLibre credits the real source.
      if (started && activeLayer !== BOOTSTRAP_LAYER) setBaseLayer(activeLayer);
    }).catch(() => {});
  }

  /** Can this DEM load at all: keyless, or its key is set. */
  function terrainUsable(dem) {
    return !!dem && (!dem.token_required || dem.token_set);
  }

  /**
   * The DEM to draw the terrain from while *provider*'s map is showing.
   *
   * *choice* is the operator's pick from the 3D panel: a DEM id, or "auto"
   * (also what anything unknown means). A pick that cannot load (a keyed
   * model whose key was removed) falls back to the automatic rule rather
   * than to flat ground.
   *
   * Automatic: a map service with its own elevation model and a key for it
   * gets that model, since its imagery was made to sit on it. Otherwise the
   * preferred model (Copernicus, finer than SRTM in steep ground and with
   * national survey models where they exist), and failing that the free
   * default every other DEM falls back to on the backend. Pure, so the
   * choice is checkable without a map.
   */
  function terrainFor(dems, provider, defaultId, choice, preferredId) {
    const list = Array.isArray(dems) ? dems : [];
    const fallback = list.find((d) => d.id === defaultId) || list[0] || null;
    if (choice && choice !== TERRAIN_AUTO) {
      const picked = list.find((d) => d && d.id === choice);
      if (terrainUsable(picked)) return picked;
    }
    const own = list.find((d) => d && d !== fallback && d.provider === provider
      && terrainUsable(d));
    if (own) return own;
    const preferred = list.find((d) => d && d.id === preferredId);
    return terrainUsable(preferred) ? preferred : fallback;
  }

  /**
   * Point the terrain at the DEM the active map service calls for, and swap
   * it under a running 3D view when that changed. Returns true when it
   * re-applied 3D itself.
   */
  function chooseTerrain() {
    const provider = activeLayer && sources[activeLayer]
      ? sources[activeLayer].provider : null;
    const next = terrainFor(terrainCatalogue, provider, defaultTerrainId,
      terrainChoice, preferredTerrainId);
    if ((next && next.id) === (terrainSpec && terrainSpec.id)) {
      terrainSpec = next;   // same DEM, fresher descriptor (a key just set)
      return false;
    }
    // The old DEM's source, shading and binding go under its own id, before
    // the spec that names them changes.
    disableTerrain();
    terrainSpec = next;
    if (!threeD || !map) return false;
    set3D(true);
    return true;
  }

  /** Re-read the source catalogue: a key saved in Settings changes which
   *  layers can draw and which elevation model 3D reads from. */
  function refreshSources() {
    return loadSources();
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
    if (animate) cameraMove({ center: target, duration: 600 });
    else cameraMove({ center: target }, true);
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
  /** Easing options plus whatever pitch and zoom are still pending. */
  function withCameraGoal(opts) {
    const out = Object.assign({}, opts);
    if (cameraGoal.pitch !== null && out.pitch === undefined) out.pitch = cameraGoal.pitch;
    if (cameraGoal.zoom !== null && out.zoom === undefined) out.zoom = cameraGoal.zoom;
    return out;
  }

  /** Remember a pitch or zoom as pending, within what the map can reach, so
   *  an unreachable goal is never chased. */
  function setCameraGoal(goal) {
    if (!map) return;
    if (goal.pitch !== undefined) {
      const max = typeof map.getMaxPitch === "function" ? map.getMaxPitch() : 60;
      cameraGoal.pitch = Math.min(Number(goal.pitch), max);
    }
    if (goal.zoom !== undefined) {
      const max = typeof map.getMaxZoom === "function" ? map.getMaxZoom() : 22;
      cameraGoal.zoom = Math.min(Number(goal.zoom), max);
    }
    cameraGoalRetries = 0;
  }

  /** A programmatic camera move: easeTo, or jumpTo at duration 0, carrying
   *  the pending goals. Every move this module makes goes through here. */
  function cameraMove(opts, jump) {
    if (!map) return;
    const merged = withCameraGoal(opts);
    const rest = panelRestPadding(map);
    if (rest && merged.padding === undefined) merged.padding = rest;
    if (jump) {
      delete merged.duration;
      map.jumpTo(merged);
    } else {
      // Over terrain the ease leaves the camera's ground height alone.
      // Unfrozen, MapLibre re-reads the DEM pixel under the target every
      // frame and lands on it, which over a surface model is a roof or a
      // tree top: every follow pan ended with the view lurching. The height
      // is reconciled once the camera is still (syncTerrainElevation).
      if (terrainOn) merged.freezeElevation = true;
      map.easeTo(merged);
    }
  }

  /** On moveend: drop the goals the camera reached, and if one was cut short
   *  by a move that did not carry it, ask for it again (a few times at most,
   *  so a goal something keeps refusing is not chased forever). */
  function settleCameraGoal() {
    if (!map) return;
    if (cameraGoal.pitch !== null && Math.abs(map.getPitch() - cameraGoal.pitch) < 0.5) {
      cameraGoal.pitch = null;
    }
    if (cameraGoal.zoom !== null && Math.abs(map.getZoom() - cameraGoal.zoom) < 0.05) {
      cameraGoal.zoom = null;
    }
    if (cameraGoal.pitch === null && cameraGoal.zoom === null) {
      cameraGoalRetries = 0;
      return;
    }
    if (cameraGoalRetries >= CAMERA_GOAL_RETRIES) {
      cameraGoal.pitch = null;
      cameraGoal.zoom = null;
      return;
    }
    cameraGoalRetries += 1;
    // Out of the moveend handler: MapLibre can fire it from inside its own
    // frame, and starting a camera move there re-enters the frame ("already
    // running"). By the time this runs another move may have started, and
    // that one carries the goals itself.
    window.setTimeout(() => {
      if (!map || (cameraGoal.pitch === null && cameraGoal.zoom === null)) return;
      if (typeof map.isMoving === "function" && map.isMoving()) return;
      cameraMove({ duration: TILT_MS }, Corvus.anim.reducedMotion());
    }, 0);
  }

  /** MapLibre's camera transform, or null. Not public API: v5 kept it at
   *  map.transform, v6 moved it to map._camera.transform (where its own
   *  Marker reads it). Losing it silently costs a forced layout per sample
   *  in viewSize() and the far-side test on the globe, so both places are
   *  asked. */
  function transformOf(m) {
    if (!m) return null;
    return m.transform || (m._camera && m._camera.transform) || null;
  }

  /** The map's viewport in CSS pixels. Read from the transform, which is
   *  what project() measures against, rather than from the container: a
   *  clientWidth read here runs right after the other telemetry subscribers
   *  have written the DOM, and forced a full layout on every sample. */
  function viewSize() {
    const transform = transformOf(map);
    if (transform && transform.width > 0 && transform.height > 0) {
      return [transform.width, transform.height];
    }
    const container = map && map.getContainer();
    return container ? [container.clientWidth, container.clientHeight] : [0, 0];
  }

  function applyFollow() {
    if (!map || !followMode || followEasing) return;
    const target = vehTarget && realFix([vehTarget.lng, vehTarget.lat]);
    if (!target) return;
    const [width, height] = viewSize();
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
      if (shifted) cameraMove({ center: target, offset, duration: 0 });
      else cameraMove({ center: target }, true);
      return;
    }
    followEasing = true;
    cameraMove({ center: target, offset, duration: FOLLOW_EASE_MS });
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
    // No fix yet, and nothing drawn yet: there is nowhere to draw it.
    if (!vehTarget && !realFix(state.position)) return;
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
    setShown(el, !threeD);
    vehicleMarker.setLngLat([vehDisplay.lng, vehDisplay.lat]);
    // The whole SVG rotates, so the heading cone and the body stay locked
    // together — they used to be separate elements and could disagree.
    const body = vehicleMarker.getElement().querySelector(".v-body");
    setTransform(body, `rotate(${px(vehDisplay.heading)}deg)`);
    // The airborne aircraft is placed from the map's render matrix. With the
    // camera still, the last frame's matrix is the current one; otherwise
    // the map has to draw the frame that carries it.
    redrawVehicle3D();
  }

  /** Show the airborne aircraft where the latest sample puts it: from the
   *  last frame's matrix while the camera has not moved since, else by
   *  asking the map for a frame. */
  function redrawVehicle3D() {
    if (!threeD || !map) return;
    if (veh3dMatrixValid && !mapInMotion()) {
      try { renderVehicle3D(veh3dMatrix); return; } catch (_error) { /* a full frame below */ }
    }
    if (typeof map.triggerRepaint === "function") map.triggerRepaint();
  }

  /** Refresh the altitudes the 3D aircraft is drawn at. Per telemetry sample
   *  rather than per frame: the DEM lookup behind it is a terrain-mesh query,
   *  and nothing about it changes between two frames of the same sample. */
  function updateVehicleAltitude(state) {
    const agl = Number(state && state.altitude_agl);
    vehAltAgl = isFinite(agl) ? agl : null;
    vehOnGround = Number(state && state.landed_state) === LANDED_ON_GROUND;
    vehAltDraw = vehicleDrawAltitude(state);
    // Where the shadow goes. Same reason it lives here rather than in the
    // render hook: it is a terrain-mesh query, and the ground under the
    // aircraft moves at telemetry rate, not at frame rate.
    vehGroundDraw = terrainElevation(realFix(state && state.position));
    // The aircraft, its line and its readout are placed on frames the map
    // draws, and a change of height alone draws none: an aircraft that lands
    // without drifting sideways kept its leader line and its last height on
    // screen until something else moved the map.
    const key = `${vehAltAgl === null ? "" : vehAltAgl.toFixed(1)}|${vehOnGround}`;
    if (key !== vehAltKey) {
      vehAltKey = key;
      redrawVehicle3D();
    }
  }

  /** Does the aircraft get a leader line, a shadow and a height readout?
   *  Not on the ground, whatever the height says, and not within a metre of it. */
  function showsHeightLine(agl, onGround) {
    if (onGround) return false;
    return agl !== null && isFinite(agl) && agl > VEH3D_MIN_AGL_M;
  }

  /** Where the height readout goes: beside the leader line, halfway down it,
   *  and clear of the aircraft. On the line it covered the aircraft whenever
   *  the line was shorter than the readout is tall, which at 10 m and a
   *  normal zoom is always. Returns the readout's left edge and centre y. */
  function heightLabelPosition(air, base, scale) {
    const s = isFinite(scale) && scale > 0 ? scale : 1;
    return {
      x: air.x + VEH3D_HALF_WIDTH_PX * s + VEH3D_LABEL_GAP_PX,
      y: air.y + (base.y - air.y) * 0.5,
    };
  }

  function updateVehicle(state) {
    // Before the connected check: the sample that names a new vehicle is
    // often the first one after a disconnected one.
    noteVehicle(state);
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
      setCameraGoal({ zoom: 16 });
      cameraMove({ center: state.position, duration: 1000 });
    }

    updateTrack(state);
    updateHome(state);
  }

  /** One telemetry sample's part in the flown track: a reboot or a takeoff
   *  closes the flight in progress, and a real move extends it. */
  function updateTrack(state) {
    // A reboot is a new flight session: the track is discarded if Settings
    // asks for that, and otherwise the last flight joins the earlier ones. A
    // link drop is neither, or a radio glitch would split or erase a flight.
    if (checkForReboot(state.boot_ms)) {
      if (trackClearOnRestart) clearTrack();
      else startNewFlight();
    }
    // Arming after a landing starts the next flight, so the one before it
    // turns into the earlier flights' colour as this one takes off.
    if (checkForTakeoff(state.armed)) startNewFlight();

    // Track recording stays on the TARGET (latest telemetry) so the track is
    // accurate; interpolation is purely a rendering concern.
    if (recordTrackPoint(state.position, state.gps_h_acc, state.altitude_agl)) {
      requestTrackPaint();
      updateTrackControl();
      // The plan line's first segment starts at the vehicle, so it must track
      // the vehicle too. Only when a plan exists — avoids repainting the route
      // source on every telemetry sample when no waypoints are planned.
      if (waypoints.length > 0) updateRoute();
    }
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
      // The arrows and WASD fly the aircraft. A menu left open by a click on
      // the map must not keep them for its rows: it closes, and the key goes
      // on to the flight controls.
      dismissKeys: isFlightKey,
      render: renderContextRows,
      onClose: dropContextPin,
    });
    return contextMenuHandle;
  }

  /** Whether a keydown is one of the keys the on-screen flight controls own
   *  (see joystick.js KEY_DIRS), with no modifier that turns it into a
   *  shortcut. Shift stays allowed: it is the boost. */
  function isFlightKey(e) {
    if (!e || e.ctrlKey || e.metaKey || e.altKey) return false;
    const key = typeof e.key === "string" ? e.key : "";
    return /^Arrow(Up|Down|Left|Right)$/.test(key) || /^[wasd]$/i.test(key);
  }

  /** Fill the open menu: the coordinates it acts on, then one row per action. */
  function renderContextRows(surface) {
    const point = { lng: contextPoint.lng, lat: contextPoint.lat };

    const head = document.createElement("div");
    head.className = "ui-menu-head map-context-head";
    const pinIcon = Corvus.ui.icon("map-pin", "auto");
    pinIcon.classList.add("map-context-pin-icon");
    head.appendChild(pinIcon);
    const coords = document.createElement("span");
    coords.className = "map-context-coords";
    coords.textContent = formatLngLat(point);
    head.appendChild(coords);
    head.appendChild(buildCopyButton(coords.textContent));
    surface.appendChild(head);

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
        label: typeof action.label === "function" ? action.label(point)
          : (action.label == null ? action.id : action.label),
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

  /**
   * The button beside the coordinates that puts them on the clipboard. Both
   * icons are drawn up front and CSS shows one, so the tick needs no Lucide
   * pass of its own. It only turns into the tick once the copy has actually
   * landed: a refused clipboard leaves the copy icon, not a false "copied".
   */
  function buildCopyButton(text) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "map-context-copy";
    btn.title = "Copy coordinates";
    btn.setAttribute("aria-label", "Copy coordinates");
    const copyIcon = Corvus.ui.icon("copy", "auto");
    copyIcon.classList.add("map-context-copy-idle");
    const doneIcon = Corvus.ui.icon("check", "auto");
    doneIcon.classList.add("map-context-copy-done");
    btn.appendChild(copyIcon);
    btn.appendChild(doneIcon);
    btn.addEventListener("click", () => {
      copyToClipboard(text).then(() => {
        btn.classList.add("is-copied");
        btn.title = "Copied";
        btn.setAttribute("aria-label", "Coordinates copied");
        if (contextCopyTimer) clearTimeout(contextCopyTimer);
        contextCopyTimer = setTimeout(() => {
          contextCopyTimer = null;
          btn.classList.remove("is-copied");
          btn.title = "Copy coordinates";
          btn.setAttribute("aria-label", "Copy coordinates");
        }, 1800);
      }, (err) => console.warn("copy coordinates failed:", err));
    });
    return btn;
  }

  /** Resolve once *text* is on the clipboard. The asynchronous clipboard
   *  first; the copy command on a hidden field where an embedding refuses it. */
  function copyToClipboard(text) {
    const byCommand = () => new Promise((resolve, reject) => {
      const box = document.createElement("textarea");
      box.value = text;
      box.setAttribute("readonly", "");
      box.style.position = "fixed";
      box.style.left = "-9999px";
      document.body.appendChild(box);
      let ok = false;
      try { box.select(); ok = document.execCommand("copy"); } catch (_e) { ok = false; }
      document.body.removeChild(box);
      if (ok) resolve(); else reject(new Error("copy refused"));
    });
    try {
      if (navigator.clipboard && typeof navigator.clipboard.writeText === "function") {
        return navigator.clipboard.writeText(text).catch(byCommand);
      }
    } catch (_e) { /* fall through to the copy command */ }
    return byCommand();
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
    if (contextCopyTimer) { clearTimeout(contextCopyTimer); contextCopyTimer = null; }
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
  function measuring() {
    return !!Corvus.mapMeasure && Corvus.mapMeasure.isActive();
  }

  function onMapClick(e) {
    if (waypointMode || measuring() || Corvus.mapOverlays.isPicking() || !e || !e.lngLat) return;
    openContextMenu(e.lngLat);
  }

  /* Right-click opens the same menu, because that is the gesture most
     operators will try first. Outside planning mode there is no browser menu
     worth keeping over the canvas. */
  function onMapRightClick(e) {
    if (waypointMode || measuring() || Corvus.mapOverlays.isPicking() || !e || !e.lngLat) return;
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
      stopMeasure();
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
  // The camera's ground height (syncTerrainElevation), as shares of the
  // camera's distance from the centre: the spacing of the patch the median
  // is taken over, how far the height may be off before it is moved, and how
  // far off is jumped rather than glided...
  const CAMERA_GROUND_SPAN = 0.04;
  const CAMERA_GROUND_BAND = 0.015;
  const CAMERA_GROUND_SNAP = 0.3;
  // ...and the share of the camera's height above the ground it looks at that
  // the ground may rise by before the camera counts as inside it.
  const CAMERA_GROUND_BURIED = 0.5;
  // The glide: its time constant, the shortest step it assumes (the first
  // frame has no previous one), and how close counts as there.
  const CAMERA_GROUND_GLIDE_MS = 220;
  const CAMERA_GROUND_MIN_STEP_MS = 16;
  const CAMERA_GROUND_DONE_M = 0.5;
  const EARTH_CIRCUMFERENCE_M = 40075016.686;
  const METRES_PER_DEGREE = EARTH_CIRCUMFERENCE_M / 360;
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
  // How long the terrain waits for the tilt (or a follow pan) to finish
  // before it is attached anyway, and how often it looks (whenStill).
  const TERRAIN_ATTACH_WAIT_MS = 1500;
  const TERRAIN_ATTACH_POLL_MS = 50;
  // How long the terrain waits for its elevation tiles before it is attached
  // with whatever has arrived (enableTerrain), and how long the tilt waits for
  // the terrain. The second is short because it is the button's answer: tiles
  // our cache holds arrive well inside it, and over ground it does not hold
  // the tilt goes first and the relief follows.
  const TERRAIN_PRELOAD_MS = 3000;
  const TERRAIN_TILT_WAIT_MS = 150;
  // How long the swing between flat and tilted takes. Short enough that the
  // button answers at once, long enough to read as a move rather than a cut.
  const TILT_MS = 400;
  // The zoom of the ancestor MapLibre's terrain falls back to while a tile
  // loads (its own rule, see primeOverviewTile).
  const TERRAIN_OVERVIEW_ZOOM = 5;
  // See init: the browser, not MapLibre, should hold the queue of tiles.
  const MAX_PARALLEL_TILE_REQUESTS = 64;
  // A failed tile is asked for again this many times, this long after the
  // map went idle (onMapIdle). "source z/x/y" -> attempts so far.
  const TILE_RETRIES = 2;
  const TILE_RETRY_MS = 3000;
  const tileRetries = new Map();
  let tileRetryTimer = null;
  // Attempts the settle poll makes before giving up — a few seconds' worth.
  const TERRAIN_SYNC_TRIES = 20;
  // Where the globe hands over to the flat map. MapLibre's own globe fades
  // into Mercator across zoom 11 to 12, so switching at 12 happens while the
  // sphere is visually flat: the operator sees a continuous zoom, not a mode
  // change. The band is hysteresis for a zoom sitting on the threshold. The
  // terrain is not part of this handover; it stays on under both.
  const GLOBE_MAX_ZOOM = 12;
  const GLOBE_ZOOM_BAND = 0.5;
  // The shading laid over the terrain (see addHillshade). Relief draped with
  // imagery shows its shape only through the shadows photographed into it,
  // which a street or topo map does not have, and which satellite imagery
  // only has for the hour it was taken. A light hillshade gives every slope
  // the same readable side whatever is under it. Light enough that imagery
  // stays imagery: this is a reading aid, not a style.
  const HILLSHADE_EXAGGERATION = 0.35;
  const HILLSHADE_SHADOW = "rgba(10, 18, 30, 0.55)";
  const HILLSHADE_HIGHLIGHT = "rgba(255, 255, 255, 0.18)";
  const HILLSHADE_ACCENT = "rgba(10, 18, 30, 0.25)";
  // The slippy-grid zoom building cells are addressed on. Must match
  // corvus/buildings.CELL_ZOOM — the backend answers that zoom and nothing
  // else, because the cache is keyed by it.
  const BUILDING_CELL_Z = 15;
  // Below this the extrusions are sub-pixel slivers and a viewport covers
  // dozens of cells, so the fetching would cost far more than it shows.
  const BUILDING_MIN_ZOOM = 14;
  // Hard cap on cells requested for one view. A zoomed-out 3D view would
  // otherwise queue a hundred Overpass cells in one gesture. The backend
  // fetches them four to a query (a z14 block), so 36 cells is nine queries.
  const BUILDING_MAX_CELLS = 36;
  // In-memory cell budget. ~36 cells cover a view; 240 is several views' worth of
  // panning before the oldest is dropped and re-fetched from the backend
  // cache (which is on disk and keeps it).
  const BUILDING_CACHE_CELLS = 240;
  // Polygons the extrusion source may hold at once. A dense European centre
  // fills this from a handful of cells; past it the cost is a visibly slower
  // map, and what is being paid for is buildings near the horizon.
  const BUILDING_MAX_FEATURES = 20000;
  // Re-asking for a cell the backend has queued. The delay grows with the
  // attempt, so a slow upstream is waited out rather than hammered.
  // Capped, and many enough to outlast a queue of a view's blocks against a
  // slow Overpass (a minute or two). Cheap: each is a loopback request the
  // backend answers from memory, and the backend says "unavailable" rather
  // than "pending" when nothing is actually on its way.
  const BUILDING_RETRY_MS = 2500;
  const BUILDING_RETRY_MAX_MS = 8000;
  const BUILDING_MAX_RETRIES = 20;
  // See addBuildingLayer: chosen against map imagery, not against the theme.
  const BUILDING_COLOR = "#A7B0BD";
  // Under the imagery in 3D (setGround). A 3D tile whose imagery has not
  // arrived was drawn transparent, so the page showed through it: a white or
  // black hole in the ground after every pan onto new ground, until the
  // operator moved the map again. Now it is plain ground with the relief
  // shading on it. Chosen against map imagery, not against the theme, like
  // the buildings.
  const GROUND_COLOR = "#8B9081";
  // The coarse copy of the imagery under the 3D ground (setGround): a few
  // tiles cover the view, and the flat map has usually loaded them already.
  const GROUND_IMAGERY_MAXZOOM = 14;
  // Perspective scaling of the airborne marker, as a fraction of its 2D size.
  // Bounded at both ends: an aircraft near the horizon must still be findable,
  // and one close to the camera must not swallow the viewport.
  const VEH3D_SCALE_MIN = 0.55;
  const VEH3D_SCALE_MAX = 1.6;
  // Below this the aircraft is on the ground for drawing purposes: the leader
  // line and the shadow are noise under a marker that is already sitting on
  // the shadow.
  const VEH3D_MIN_AGL_M = 1.0;
  // MAV_LANDED_STATE_ON_GROUND, from EXTENDED_SYS_STATE.
  const LANDED_ON_GROUND = 1;
  // Room between the aircraft's edge and the height readout beside it.
  const VEH3D_LABEL_GAP_PX = 6;
  // Half the 2D marker's width at scale 1: the readout starts past it.
  const VEH3D_HALF_WIDTH_PX = 24;
  // The flown track in the air (renderTrack3D), beside the one on the
  // ground. A thread rather than a ribbon: the ground track is the one to
  // read, this one only says where in the air it was flown. Earlier flights
  // thinner and fainter, as on the ground.
  const TRACK3D_CORE_PX = 1.3;
  const TRACK3D_CASING_PX = 2.8;
  const TRACK3D_PAST_CORE_PX = 1.0;
  const TRACK3D_PAST_CASING_PX = 2.2;
  const TRACK3D_PAST_OPACITY = 0.7;
  const TRACK3D_CASING_OPACITY = 0.45;
  // How long the ground under an earlier flight's first point is trusted
  // before it is asked again: a terrain-mesh query, and the DEM fills in.
  const TRACK3D_GROUND_MS = 1000;

  let threeD = false;
  // Which 3D the operator asked for, remembered across off/on so pressing the
  // mode they were last in does not first hand them the other one.
  //
  //   "simple" — the camera tilt, the sky, the terrain relief and the globe
  //              at the far end of the zoom. Elevation tiles come through the
  //              same cache as the imagery and cost about as much, so they
  //              are not optional: a tilted map over flat ground is exactly
  //              the picture that makes an operator misjudge a ridge.
  //   "full"   — all of that plus extruded buildings, which the first time
  //              over an area are an Overpass round trip per cell.
  //
  // The split exists because the second one has a cost a radio link notices,
  // and an operator who only wants the ground should not pay it. The names
  // predate the terrain being in both and are kept because saved configs
  // carry them.
  const THREE_D_SIMPLE = "simple";
  const THREE_D_FULL = "full";
  const THREE_D_OFF = "off";
  // Terrain without buildings is where an operator starts. Buildings are a
  // choice they make, not one they discover after a field laptop has spent a
  // radio link on Overpass round trips they never asked for.
  let threeDDetail = THREE_D_SIMPLE;
  // The DEM descriptor from GET /api/tiles/sources ({id, provider, encoding,
  // tile_size, maxzoom, attribution}). Null until the catalogue lands — and
  // stays null on a backend that does not serve one, which is what makes 3D
  // degrade to a plain tilt rather than break. Which one of the catalogue it
  // is follows the map service showing (see chooseTerrain).
  let terrainSpec = null;
  let terrainCatalogue = [];
  let defaultTerrainId = null;
  let preferredTerrainId = null;
  // The operator's pick of elevation model in the 3D panel: a DEM id, or
  // "auto" for the rule in terrainFor. Saved as map.terrain_source.
  const TERRAIN_AUTO = "auto";
  let terrainChoice = TERRAIN_AUTO;
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
  // The tilt into 3D waiting for the terrain (tiltAfterTerrain), or null.
  let pendingTilt = null;
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
  // The centre an elevation probe is running for (seedCameraElevation).
  let terrainSeedKey = null;
  // The camera's ground height on its way to a new value (glideCameraGround).
  const groundGlide = { target: null, frame: null, last: 0 };
  // True while Corvus itself is writing that height, so the move it fires is
  // not taken for the operator's (see the moveend handler).
  let elevationWriting = false;
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
  let veh3dLeadShown = true; // what setLeadShown last set, built visible
  let veh3dAltEl = null;     // its height above ground, as a number
  // The flown track at its height, drawn on a canvas from the same matrix.
  // A canvas and not a line layer: MapLibre drapes every line onto the
  // terrain, which is the ground track's job and not this one's.
  let track3dEl = null;
  let track3dDrawn = false;
  let track3dPalette = null;
  // Mercator x, y and z per metre of each point, and the ground under each
  // earlier flight, both keyed by the arrays themselves so a replaced point
  // or flight is simply computed again.
  const track3dMercator = new WeakMap();
  const track3dGround = new WeakMap();
  let veh3dLayerAdded = false;
  // The render matrix of the last frame the map drew, and whether the camera
  // is still where that frame had it. While it is, the matrix still places
  // the aircraft and its track exactly, so a new sample moves them without a
  // whole map frame (terrain, tiles, sky): at 60 Hz that frame was most of
  // the CPU 3D mode cost with the camera standing still.
  const veh3dMatrix = new Float64Array(16);
  let veh3dMatrixValid = false;
  // The height to draw the aircraft at, in MapLibre's drawing frame (see
  // terrainElevation). Recomputed per telemetry sample rather than per frame —
  // the DEM lookup behind it is a terrain-mesh query, not an array read.
  let vehAltDraw = null;
  let vehAltAgl = null;
  // The vehicle says it is on the ground (EXTENDED_SYS_STATE). Outranks the
  // height, which on a real aircraft drifts a metre or more on the baro over
  // a flight and would otherwise leave the leader line standing after landing.
  let vehOnGround = false;
  // What the last 3D frame was drawn from, so a change asks for a new frame.
  let vehAltKey = "";
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

  /** Is *id* one of the elevation sources rather than a map layer? */
  function isTerrainSource(id) {
    return !!id && (terrainCatalogue.some((d) => d.id === id)
      || (!!terrainSpec && terrainSpec.id === id));
  }

  /** The raster-dem source id, or null when the backend serves no DEM. */
  function terrainSourceId() {
    return terrainSpec ? terrainSpec.id : null;
  }

  /**
   * Put the DEM into the style and switch terrain on. Idempotent, and a no-op
   * without a DEM descriptor — the tilt then still works, on flat ground.
   *
   * The tile size is the DEM's own (terrainTileSize), never MapLibre's
   * default: it assumes 512 for raster-dem, and the free terrarium tiles are
   * 256, so leaving it out halves every elevation and quietly flattens the
   * world.
   */
  /**
   * Bring terrain up, once its tiles are in, the camera has been placed above
   * the ground and the camera is holding still.
   *
   * The shading goes on first. It reads exactly the terrain's tiles (see
   * addHillshade), so it loads them while the map is still flat. Attached
   * before they were in, the terrain drew every tile still loading at sea
   * level: cliffs hundreds of metres deep beside the ones that had arrived,
   * and white where the camera saw past them, until the operator moved the
   * map. Attached once they are in, the terrain takes them from the browser's
   * cache and comes up whole. The wait is bounded (TERRAIN_PRELOAD_MS), so
   * ground no cache holds still gets its relief, in pieces.
   *
   * The tilt waits for this, for TERRAIN_TILT_WAIT_MS at most (see
   * tiltAfterTerrain): relief attached under a camera looking straight down
   * barely shows, and the tilt then reveals it. Past that wait the tilt goes
   * anyway and the terrain is attached once the camera is still (whenStill),
   * because writing the camera's height is a jumpTo, which would stop the
   * tilt part way. Attach and seed land in the same frame, so the ground
   * rises under the camera without the camera ever being inside it.
   */
  function enableTerrain() {
    const id = terrainSourceId();
    if (!map || !started || !id || terrainWanted) return false;
    terrainWanted = true;
    // Claimed before the probe below, so a second press cannot start a second
    // attach, and cancelled by a generation bump if 3D goes off meanwhile.
    const generation = ++terrainGeneration;
    addHillshade(id);
    const start = nowMs();
    // Already tilted (the operator changed the elevation model, or a map
    // service brought its own, with 3D on): every moment spent waiting is
    // the ground shown flat. The coarse fallback is enough to come up on.
    let tilted = false;
    try { tilted = map.getPitch() > 10; } catch (_error) { /* a flat camera */ }
    Promise.all([
      primeCameraElevation(),
      tilted ? null : whenSourceLoaded(hillshadeSourceId(id), generation, start),
      primeOverviewTile(id, start),
    ]).then(([height]) => {
      whenStill(generation, () => {
        attachTerrain(id, generation);
        // AFTER the attach, never before: setTerrain resets the camera's
        // centre elevation to zero as it binds — it re-derives that height
        // from the terrain it is attaching, which at that instant knows
        // nothing. A seed written first is therefore thrown away, and the
        // camera goes back underground on the very call that was meant to
        // save it.
        if (height !== null && terrainOn) writeCenterElevation(height);
        tiltNow();
      });
    });
    return true;
  }

  /**
   * Resolve once *id* has loaded the tiles of the view, or after
   * TERRAIN_PRELOAD_MS from *since* whatever it has. Never rejects: a source
   * that will not load is a reason to stop waiting, not to fail the mode.
   */
  function whenSourceLoaded(id, generation, since) {
    return new Promise((resolve) => {
      const look = () => {
        if (generation !== terrainGeneration || !terrainWanted || !map) { resolve(); return; }
        let done = true;
        try {
          done = typeof map.isSourceLoaded !== "function" || map.isSourceLoaded(id);
        } catch (_error) { /* the source is gone; nothing to wait for */ }
        if (done || nowMs() - since > TERRAIN_PRELOAD_MS) { resolve(); return; }
        window.setTimeout(look, TERRAIN_ATTACH_POLL_MS);
      };
      look();
    });
  }

  /**
   * Fetch the coarse elevation tile the terrain falls back to, so it is in
   * the browser's cache before the terrain asks for it.
   *
   * Under every tile it draws, MapLibre's terrain also asks for that tile's
   * ancestor at zoom 5 and uses it until the tile itself arrives. Without it
   * a tile still loading is drawn at sea level. It asks for that ancestor
   * last, behind every detailed tile, so over a slow link or a model our
   * side renders tile by tile it arrived last too, and the view broke into
   * cliffs and holes until it did. Resolves when the tile is in, failed, or
   * the preload wait is up; never rejects.
   */
  function primeOverviewTile(id, since) {
    if (!map || typeof window.fetch !== "function") return Promise.resolve();
    let centre;
    try { centre = map.getCenter(); } catch (_error) { return Promise.resolve(); }
    if (!centre || !isFinite(centre.lng) || !isFinite(centre.lat)) return Promise.resolve();
    const zoom = Math.min(TERRAIN_OVERVIEW_ZOOM, terrainSpec.maxzoom);
    const count = 1 << zoom;
    const lat = Math.max(-85.05112878, Math.min(85.05112878, centre.lat));
    const rad = (lat * Math.PI) / 180;
    const x = Math.max(0, Math.min(count - 1, Math.floor(((centre.lng + 180) / 360) * count)));
    const y = Math.max(0, Math.min(count - 1, Math.floor(
      ((1 - Math.log(Math.tan(rad) + 1 / Math.cos(rad)) / Math.PI) / 2) * count)));
    const url = tileUrl(id).replace("{z}", zoom).replace("{x}", x).replace("{y}", y);
    const left = Math.max(0, TERRAIN_PRELOAD_MS - (nowMs() - since));
    return new Promise((resolve) => {
      const timer = window.setTimeout(resolve, left);
      const done = () => { window.clearTimeout(timer); resolve(); };
      try {
        window.fetch(url).then((r) => r.blob()).then(done, done);
      } catch (_error) {
        done();
      }
    });
  }

  /**
   * Tilt into 3D once the terrain is attached, or after TERRAIN_TILT_WAIT_MS
   * if it is not by then. The button answers at once either way: the
   * shading appears on the press.
   */
  function tiltAfterTerrain(generation) {
    cancelPendingTilt();
    pendingTilt = window.setTimeout(() => {
      pendingTilt = null;
      if (threeD && generation === terrainGeneration) tiltTo(THREE_D_PITCH);
    }, TERRAIN_TILT_WAIT_MS);
  }

  /** The waiting tilt, now. A no-op when none is waiting. */
  function tiltNow() {
    if (pendingTilt === null) return;
    cancelPendingTilt();
    if (threeD) tiltTo(THREE_D_PITCH);
  }

  function cancelPendingTilt() {
    if (pendingTilt !== null) window.clearTimeout(pendingTilt);
    pendingTilt = null;
  }

  /**
   * Run *fn* once the camera is not being moved, or after a short wait at
   * most. Writing the camera's height is a jumpTo, and a jumpTo stops any
   * animation in flight: the tilt into 3D, or a follow pan. Bounded, because
   * a camera that never holds still (a follow pan per telemetry sample) must
   * not keep the terrain away, and dropped by a generation bump.
   */
  function whenStill(generation, fn, since) {
    if (generation !== terrainGeneration || !terrainWanted || !map) return;
    const start = since === undefined ? nowMs() : since;
    if (!mapInMotion() || nowMs() - start > TERRAIN_ATTACH_WAIT_MS) {
      fn();
      return;
    }
    window.setTimeout(() => whenStill(generation, fn, start), TERRAIN_ATTACH_POLL_MS);
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
        tileSize: terrainTileSize(),
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
    // From here the camera's height is held by syncTerrainElevation, never
    // by MapLibre's per-pixel clamp: see there for why.
    holdCameraGround(true);
    // Normally on since the press (enableTerrain), with its tiles loaded.
    if (!map.getLayer("terrain-hillshade")) addHillshade(id);
    // From here on, DEM tiles landing are the cue to reconcile the camera's
    // idea of the ground height with the real one — see syncTerrainElevation.
    terrainSyncAt = 0;
    map.on("data", onTerrainData);
    scheduleTerrainSync(true);
  }

  /** The DEM's pixel size. Not a default to lean on: MapLibre assumes 512
   *  for raster-dem, and the free tiles are 256, so a wrong size halves or
   *  doubles every height and quietly flattens (or stretches) the world. */
  function terrainTileSize() {
    const size = Number(terrainSpec && terrainSpec.tile_size);
    return size === 512 ? 512 : 256;
  }

  /** The hillshade's own raster-dem source id. */
  function hillshadeSourceId(id) {
    return id + "-shade";
  }

  /**
   * Shade the terrain, so its shape reads on any base layer.
   *
   * Its own source, because MapLibre warns when one source serves both, but
   * declared at twice the tile size so it asks for exactly the tiles the
   * terrain mesh asks for (MapLibre doubles a terrain source's size the same
   * way). Declared at the real size it asked for a second set one zoom
   * deeper: twice the elevation tiles on every switch and every pan, which
   * over a custom model is twice the rendering on our side, and the imagery
   * waited behind them for the browser's few connections, so the ground
   * stayed white until the operator moved the map. The slope is computed
   * from the image's own pixels and the tile's zoom, so the shading is as
   * strong at either size, only one zoom softer.
   *
   * Under every overlay (it is first in OVERLAY_LAYERS): the downloaded
   * areas, the track and the buildings must not be darkened by a slope.
   * Best effort: a MapLibre without hillshade still has the relief itself.
   */
  function addHillshade(id) {
    if (!map || !terrainSpec) return;
    const shade = hillshadeSourceId(id);
    try {
      if (map.getLayer("terrain-hillshade")) map.removeLayer("terrain-hillshade");
      if (map.getSource(shade)) map.removeSource(shade);
      map.addSource(shade, {
        type: "raster-dem",
        tiles: [tileUrl(id)],
        tileSize: terrainTileSize() * 2,
        maxzoom: terrainSpec.maxzoom,
        encoding: terrainSpec.encoding || "terrarium",
      });
      map.addLayer({
        id: "terrain-hillshade",
        type: "hillshade",
        source: shade,
        paint: {
          "hillshade-exaggeration": HILLSHADE_EXAGGERATION,
          "hillshade-shadow-color": HILLSHADE_SHADOW,
          "hillshade-highlight-color": HILLSHADE_HIGHLIGHT,
          "hillshade-accent-color": HILLSHADE_ACCENT,
          // Lit from the map's north-west whatever way the camera faces, the
          // way every printed map is: a light that turned with the bearing
          // would make a valley read as a ridge halfway through a turn.
          "hillshade-illumination-anchor": "map",
        },
      }, firstOverlayLayer());
    } catch (error) {
      console.warn("Corvus: terrain shading unavailable", error);
    }
  }

  function removeHillshade() {
    if (!map) return;
    const id = terrainSourceId();
    try {
      if (map.getLayer("terrain-hillshade")) map.removeLayer("terrain-hillshade");
      if (id && map.getSource(hillshadeSourceId(id))) map.removeSource(hillshadeSourceId(id));
    } catch (_error) { /* already gone */ }
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
   * The height is Corvus's, not MapLibre's, for as long as terrain is on (see
   * attachTerrain). MapLibre's own rule puts the camera's focus on the one
   * DEM pixel under the centre, every frame and at the end of every ease.
   * Over a surface model, which has the roofs and the tree tops in it, that
   * pixel jumps by ten or twenty metres whenever the centre crosses a hedge,
   * and following an aircraft moves the centre all the time: the whole view
   * pumped up and down with every follow pan. What is used here is the
   * median of a patch around the centre (cameraGround), changed only when it
   * has moved by a share of the camera's distance, and then glided to rather
   * than jumped to.
   *
   * Runs for as long as 3D is on, not once: DEM tiles refine as they load,
   * and every camera move ends somewhere new. Rate limited, and a no-op once
   * the two agree.
   */
  function syncTerrainElevation() {
    if (!map || !terrainOn) return false;
    if (typeof map.setCenterElevation !== "function") return true;   // nothing to do
    // Never mid-animation or mid-drag: the camera is being moved, and a
    // height written now would be a jolt under the operator's hand. The
    // moveend handler asks again once the map is still.
    if (mapInMotion()) return false;
    let centre;
    try { centre = map.getCenter(); } catch (_error) { return false; }
    if (!centre || !isFinite(centre.lng) || !isFinite(centre.lat)) return false;
    const distance = cameraDistanceM(centre.lat);
    // A literal zero under the centre is not an answer (readTerrainValue):
    // terrainElevation is null there, and so is the patch. Keeping it
    // unsettled costs a few retries at genuine sea level and then leaves 0
    // standing, which is the right answer there.
    const ground = cameraGround(centre, distance);
    if (ground === null) {
      // The DEM under the centre has not arrived, and after a long jump it
      // may never: see seedCameraElevation.
      seedCameraElevation(centre);
      return false;
    }
    terrainSeedKey = null;
    if (groundGlide.target !== null) {
      groundGlide.target = ground;   // already on its way; only the goal moved
      return true;
    }
    const current = Number(map.getCenterElevation());
    const off = isFinite(current) ? Math.abs(ground - current) : Infinity;
    if (off < cameraGroundBand(distance)) return true;
    // A camera inside the hill, or so far off that it might as well be, is
    // the blank map: gliding out of it would show the operator the inside of
    // a mountain for a moment. Everything else glides.
    if (cameraBuried(ground, current, distance) || off > CAMERA_GROUND_SNAP * distance) {
      return writeCenterElevation(ground);
    }
    glideCameraGround(ground);
    return true;
  }

  /** Is the camera being moved right now, by an animation or by a hand? */
  function mapInMotion() {
    if (typeof map.isEasing === "function" && map.isEasing()) return true;
    return typeof map.isMoving === "function" && map.isMoving();
  }

  /**
   * How far the camera is from the point it looks at, in metres.
   *
   * MapLibre's camera sits 1.5 viewport heights from the centre at its
   * default field of view; cameraToCenterDistance is that, in pixels, when
   * the transform is there to ask. Times the metres in one pixel at the
   * centre's latitude.
   */
  function cameraDistanceM(lat) {
    const transform = transformOf(map);
    const px = transform && isFinite(transform.cameraToCenterDistance)
      ? transform.cameraToCenterDistance : 1.5 * viewSize()[1];
    let zoom;
    try { zoom = map.getZoom(); } catch (_error) { zoom = NaN; }
    if (!isFinite(zoom) || !(px > 0)) return 0;
    const metresPerPixel = EARTH_CIRCUMFERENCE_M * Math.cos((lat * Math.PI) / 180)
      / (512 * Math.pow(2, zoom));
    return px * metresPerPixel;
  }

  /** Would the camera be in or close to the ground at *ground*? It sits
   *  distance x cos(pitch) above the height it looks at. */
  function cameraBuried(ground, current, distance) {
    if (!isFinite(current)) return true;
    let pitch;
    try { pitch = map.getPitch(); } catch (_error) { pitch = 0; }
    const rise = distance * Math.cos(((isFinite(pitch) ? pitch : 0) * Math.PI) / 180);
    return ground - current > CAMERA_GROUND_BURIED * rise;
  }

  /** How far the camera's ground may be off before it is moved: a share of
   *  the camera's distance, so a few metres close in and tens far out, where
   *  the same metres are a fraction of a pixel. */
  function cameraGroundBand(distance) {
    return Math.max(TERRAIN_SYNC_EPSILON_M, CAMERA_GROUND_BAND * distance);
  }

  /**
   * The ground height the camera should look at, around *centre*, or null
   * while the DEM under the centre itself is unknown.
   *
   * A median over a small patch rather than the one pixel under the centre.
   * Over a surface model the pixel is a roof one moment and a road the next;
   * the median is the level most of the patch is at, which moves when the
   * ground does and not when the centre crosses a building. The patch scales
   * with the camera's distance, so it stays about the same share of the view.
   */
  function cameraGround(centre, distance) {
    const middle = terrainElevation([centre.lng, centre.lat]);
    if (middle === null) return null;
    const step = CAMERA_GROUND_SPAN * distance;
    if (!(step > 0)) return middle;
    const dLat = step / METRES_PER_DEGREE;
    const dLng = dLat / Math.max(0.05, Math.cos((centre.lat * Math.PI) / 180));
    const heights = [middle];
    for (let i = -1; i <= 1; i++) {
      for (let j = -1; j <= 1; j++) {
        if (i === 0 && j === 0) continue;
        const h = terrainElevation([centre.lng + i * dLng, centre.lat + j * dLat]);
        if (h !== null) heights.push(h);
      }
    }
    return medianOf(heights);
  }

  function medianOf(values) {
    const sorted = values.slice().sort((a, b) => a - b);
    const mid = sorted.length >> 1;
    return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
  }

  /** Set the camera's ground height, without the move it fires being taken
   *  for a new view (see the moveend handler). */
  function writeCenterElevation(height) {
    elevationWriting = true;
    try {
      map.setCenterElevation(height);
      return true;
    } catch (_error) {
      return false;   // a camera mid-change; the next sync tries again
    } finally {
      elevationWriting = false;
    }
  }

  /**
   * Move the camera's ground height to *target* over a few frames.
   *
   * An exponential approach with a fixed time constant, so it takes as long
   * on a laptop drawing ten frames a second as on one drawing sixty. It gives
   * way to anything that moves the camera: the moveend that follows asks for
   * the height again, from where the camera then is.
   */
  function glideCameraGround(target) {
    groundGlide.target = target;
    if (groundGlide.frame !== null) return;
    groundGlide.last = nowMs();
    stepCameraGround();
  }

  function stepCameraGround() {
    groundGlide.frame = null;
    const target = groundGlide.target;
    if (!map || !terrainOn || target === null || mapInMotion()) {
      groundGlide.target = null;
      return;
    }
    const current = Number(map.getCenterElevation());
    const now = nowMs();
    const dt = Math.max(CAMERA_GROUND_MIN_STEP_MS, now - groundGlide.last);
    groundGlide.last = now;
    const left = target - current;
    const next = !isFinite(current) || Math.abs(left) <= CAMERA_GROUND_DONE_M
      ? target
      : current + left * (1 - Math.exp(-dt / CAMERA_GROUND_GLIDE_MS));
    if (!writeCenterElevation(next) || next === target) {
      groundGlide.target = null;
      return;
    }
    groundGlide.frame = window.requestAnimationFrame(stepCameraGround);
  }

  /** Stop a glide in progress. Idempotent. */
  function stopCameraGlide() {
    if (groundGlide.frame !== null) window.cancelAnimationFrame(groundGlide.frame);
    groundGlide.frame = null;
    groundGlide.target = null;
  }

  function nowMs() {
    return typeof performance !== "undefined" && performance.now ? performance.now() : Date.now();
  }

  /**
   * Lift the camera out of the ground after a long jump.
   *
   * Where the terrain under the centre is not loaded, there is no ground to
   * sync to. After a jump of a few kilometres into a valley, say from a
   * follow cut or a place search, the camera keeps the height of where it
   * was, which can be hundreds of metres underground. MapLibre then asks for
   * the elevation tiles IT can see, which are not the ones under the centre,
   * so the centre never loads and the map stays blank: the same deadlock
   * primeCameraElevation breaks when terrain is first attached.
   *
   * Broken the same way: the height is read straight from one elevation
   * tile. One probe per centre at a time.
   */
  function seedCameraElevation(centre) {
    if (!map || typeof map.setCenterElevation !== "function") return;
    const key = `${centre.lng.toFixed(4)},${centre.lat.toFixed(4)}`;
    if (terrainSeedKey === key) return;
    terrainSeedKey = key;
    const generation = terrainGeneration;
    demElevationAt([centre.lng, centre.lat]).then((height) => {
      if (generation !== terrainGeneration || !terrainOn) return;
      if (height === null || !isFinite(height)) return;
      let now;
      try { now = map.getCenter(); } catch (_error) { return; }
      // Only if the camera is still where the probe was for and still blind.
      if (!now || Math.abs(now.lng - centre.lng) > 1e-3 || Math.abs(now.lat - centre.lat) > 1e-3) return;
      if (terrainElevation([now.lng, now.lat]) !== null) return;
      // Not into a moving camera: the write would stop the tilt or the pan
      // part way. Forgotten instead, so the moveend at its end asks again.
      if (mapInMotion()) { terrainSeedKey = null; return; }
      if (!writeCenterElevation(height)) return;
      scheduleTerrainSync(true);
    });
  }

  /**
   * Who holds the camera's height: Corvus while terrain is on, MapLibre
   * otherwise (where it holds it at sea level, which is right on flat ground).
   */
  function holdCameraGround(own) {
    if (!map || typeof map.setCenterClampedToGround !== "function") return;
    try { map.setCenterClampedToGround(!own); } catch (_error) { /* an older MapLibre */ }
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
  /**
   * What the 3D ground shows while its imagery loads: the same imagery at
   * GROUND_IMAGERY_MAXZOOM, and plain ground under that.
   *
   * A tilted view asks for the finest imagery near the camera, eighty tiles
   * and more after a pan onto new ground, and MapLibre draws nothing coarser
   * in their place until they arrive. On the flat map that is seconds of a
   * few blank squares; on the 3D ground it was a hole the page showed
   * through, white or black, as big as the near half of the view. The coarse
   * copy is a handful of tiles, usually already cached from the flat map, so
   * it is there almost at once and the detail sharpens over it. Its URLs are
   * the base layer's own, so nothing is fetched twice. The source itself is
   * made with the base layer (setBaseLayer); this only shows or hides it.
   */
  function setGround(on) {
    if (!map) return;
    try {
      if (map.getLayer("terrain-ground-imagery") && typeof map.setLayoutProperty === "function") {
        map.setLayoutProperty("terrain-ground-imagery", "visibility", on ? "visible" : "none");
      }
      if (!on) {
        if (map.getLayer("terrain-ground")) map.removeLayer("terrain-ground");
        return;
      }
      if (map.getLayer("terrain-ground")) return;
      const below = ["terrain-ground-imagery", "base"].find((id) => map.getLayer(id));
      map.addLayer({
        id: "terrain-ground",
        type: "background",
        paint: { "background-color": GROUND_COLOR },
      }, below || firstOverlayLayer());
    } catch (_error) { /* no style yet; the "load" handler re-applies 3D */ }
  }

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
   * Should the globe be showing at *zoom*, given that it *currentlyGlobe*?
   *
   * Pure, and split out for the same reason followAction is: the rule is the
   * thing worth asserting, and it should not need a renderer to check. The
   * band is hysteresis — the threshold sits further away in whichever
   * direction would mean changing — so a zoom resting on the boundary does
   * not swap the projection back and forth on every wheel click.
   *
   * A zoom that is not a number keeps whatever is showing: a transform
   * mid-change is not a reason to rebuild the world.
   */
  function wantsGlobe(zoom, currentlyGlobe) {
    if (!isFinite(zoom)) return !!currentlyGlobe;
    return zoom < GLOBE_MAX_ZOOM + (currentlyGlobe ? GLOBE_ZOOM_BAND : -GLOBE_ZOOM_BAND);
  }

  /**
   * Choose the projection for the current zoom: the globe far out, the flat
   * map close in, and nothing for the operator to press in between.
   *
   * The TERRAIN is not part of this. It used to be: the globe once came with
   * the terrain switched off, on the belief that MapLibre could not draw
   * both, so every zoom below 12 showed the Alps as a photograph of the Alps
   * on a smooth ball. MapLibre 5.24 draws relief on the globe and keeps the
   * camera above it by itself (centerClampedToGround), checked against the
   * Zugspitze from zoom 3 to 16. Zoom 8 to 11, where a whole valley and its
   * ridges fit on screen, is exactly where relief is worth the most.
   *
   * The projection still changes, because the aircraft is drawn differently
   * on each (see renderVehicle3D): the flat map's render matrix takes
   * Mercator coordinates, the globe's does not.
   */
  function applyProjectionForZoom() {
    if (!map || !started || !threeD) return;
    let zoom;
    try { zoom = map.getZoom(); } catch (_error) { return; }
    const wantGlobe = wantsGlobe(zoom, globeOn);
    if (wantGlobe === globeOn) return;
    globeOn = wantGlobe;
    setGlobe(wantGlobe);
    scheduleTerrainSync(true);
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
    stopCameraGlide();
    cancelPendingTilt();
    terrainSeedKey = null;
    holdCameraGround(false);
    // Before the early return: the shading goes on at the press, so an attach
    // still waiting for its tiles has one too.
    removeHillshade();
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
    }, ["path-past-glow", "path-glow"].find((id) => map.getLayer(id)));
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
   * that rectangle to then keep the nearest few dozen means materialising a
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
        if (data && data.unavailable) {
          // Nothing is on its way (offline, or Overpass asked for a pause).
          // Release the claim and do not poll: the next change of view asks
          // again, which is when it can have become available.
          buildingCells.delete(key);
          buildingRetries.delete(key);
          return;
        }
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

  /**
   * Re-ask for a cell the backend is still fetching, a bounded number of
   * times, then let it go WITHOUT keeping the empty claim.
   *
   * Keeping the claim was the bug behind "buildings in some places, not
   * others": a view asks for a few dozen cells, two workers fetch them from
   * Overpass a few seconds each, and the cells at the back of that queue
   * outlasted the retries. Each then sat in the map as an empty answer for
   * the rest of the session, and panning back never asked again, although
   * the backend had long since fetched it. Released, the next change of view
   * picks it up from the backend's cache.
   */
  function retryBuildingCell(key) {
    const tries = (buildingRetries.get(key) || 0) + 1;
    if (tries > BUILDING_MAX_RETRIES) {
      releaseBuildingCell(key);
      return;
    }
    buildingRetries.set(key, tries);
    window.setTimeout(() => {
      // Only if 3D is still on and the cell is still worth having: an
      // operator who left the area (or left 3D) must not be paying for
      // requests about ground they are not looking at. Released, not
      // abandoned: the empty claim would otherwise stand for the rest of the
      // session, and panning back (or pressing 3D again) would never ask for
      // the cell the backend has meanwhile fetched.
      if (!threeD || !buildingSource || !viewBuildingCells().includes(key)) {
        releaseBuildingCell(key);
        return;
      }
      buildingCells.delete(key);
      fetchBuildingCell(key);
    }, Math.min(BUILDING_RETRY_MS * tries, BUILDING_RETRY_MAX_MS));
  }

  /** Forget a cell that is still only claimed, so the next view asks again.
   *  A cell holding buildings is kept. */
  function releaseBuildingCell(key) {
    buildingRetries.delete(key);
    const cell = buildingCells.get(key);
    if (cell && !cell.length) buildingCells.delete(key);
  }

  /**
   * Coalesce the repaints a burst of arriving cells would otherwise cause.
   *
   * A pan asks for a few dozen cells and they land within a second of
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

  /** Build the four elements that make up the airborne aircraft, and the
   *  canvas the track in the air is drawn on, under them. */
  function buildVehicle3D(mapEl) {
    if (!mapEl || veh3dEl) return;

    track3dEl = document.createElement("canvas");
    track3dEl.className = "track3d";
    track3dEl.setAttribute("aria-hidden", "true");
    mapEl.appendChild(track3dEl);

    veh3dLeadEl = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    veh3dLeadEl.setAttribute("class", "veh3d-lead");
    veh3dLeadEl.setAttribute("aria-hidden", "true");
    const line = document.createElementNS("http://www.w3.org/2000/svg", "line");
    veh3dLeadEl.appendChild(line);
    setLeadShown(false);
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

  /**
   * Show or hide the leader line. It is an SVG element, and SVG has no
   * `hidden` property: assigning one sets a plain field the CSS never sees,
   * which left the last line standing after a landing or a switch to 2D.
   */
  function setLeadShown(shown) {
    if (!veh3dLeadEl || shown === veh3dLeadShown) return;
    veh3dLeadShown = shown;
    if (shown) veh3dLeadEl.removeAttribute("hidden");
    else veh3dLeadEl.setAttribute("hidden", "");
  }

  // The aircraft is redrawn on every animation frame. A write that changes
  // nothing, or changes it by less than anyone can see, still makes Chromium
  // restyle and composite a new frame, and in the desktop app Qt composites
  // that frame again: with the aircraft flying, those frames were most of
  // what the app cost. So positions go out at a tenth of a pixel and angles
  // at a tenth of a degree, and only when they changed.
  function px(v) { return v.toFixed(1); }
  function setTransform(el, value) {
    if (el && el.style.transform !== value) el.style.transform = value;
  }
  function setShown(el, shown) {
    if (el && el.hidden === shown) el.hidden = !shown;
  }
  function setAttrIf(el, name, value) {
    if (el && el.getAttribute(name) !== value) el.setAttribute(name, value);
  }
  function setTextIf(el, value) {
    if (el && el.textContent !== value) el.textContent = value;
  }

  function hideVehicle3D() {
    veh3dScreen = null;
    veh3dGroundScreen = null;
    // Only where it changes something: this runs from the custom layer's
    // render hook, which fires on every frame the map draws — including every
    // frame of plain 2D, where all four are hidden already and four attribute
    // writes per frame is a cost with nothing to show for it.
    [veh3dEl, veh3dShadowEl, veh3dAltEl].forEach((el) => {
      if (el && !el.hidden) el.hidden = true;
    });
    setLeadShown(false);
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
    const transform = transformOf(map);
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
    const [width, height] = viewSize();
    if (!(width > 0 && height > 0)) { hideVehicle3D(); return; }

    const lng = vehDisplay.lng, lat = vehDisplay.lat;
    if (!realFix([lng, lat])) { hideVehicle3D(); return; }
    veh3dScreen = null;
    veh3dGroundScreen = null;

    // On the GLOBE the render matrix is the globe's own, and it takes points
    // on a sphere — feeding it the mercator coordinates every other
    // projection wants put the aircraft in the bottom-right corner of the
    // viewport while the map drew it in the middle. There is no altitude
    // worth showing at that zoom anyway: below zoom 12 a pixel is a hundred
    // metres of ground or more, so a drone's height above the terrain is a
    // pixel or two. So the globe gets the aircraft where MapLibre itself
    // says it is, on the terrain, and nothing else.
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
    // On the ground is on the ground: a baro that has drifted a metre over
    // the flight must not leave a landed aircraft hovering over its shadow.
    const airM = (vehAltDraw === null || (vehOnGround && groundKnown)) ? groundM
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
    setShown(veh3dEl, true);
    setTransform(veh3dEl,
      `translate(-50%, -50%) translate(${px(air.x)}px, ${px(air.y)}px) scale(${scale.toFixed(3)}) rotateX(${px(pitch)}deg)`);
    const body = veh3dEl.querySelector(".v-body");
    setTransform(body, `rotate(${px(vehDisplay.heading - bearing)}deg)`);

    // Below this the aircraft is effectively on its own shadow, and a leader
    // line of two pixels reads as a rendering fault.
    const airborne = showsHeightLine(vehAltAgl, vehOnGround) && !!base;
    if (!airborne) {
      setShown(veh3dShadowEl, false);
      setLeadShown(false);
      setShown(veh3dAltEl, false);
      return;
    }

    veh3dGroundScreen = { x: base.x, y: base.y };
    setShown(veh3dShadowEl, true);
    setTransform(veh3dShadowEl,
      `translate(-50%, -50%) translate(${px(base.x)}px, ${px(base.y)}px) scale(${scale.toFixed(3)}) rotateX(${px(pitch)}deg)`);

    setLeadShown(true);
    setAttrIf(veh3dLeadEl, "viewBox", `0 0 ${width} ${height}`);
    setAttrIf(veh3dLeadEl, "width", String(width));
    setAttrIf(veh3dLeadEl, "height", String(height));
    const line = veh3dLeadEl.firstChild;
    setAttrIf(line, "x1", px(base.x));
    setAttrIf(line, "y1", px(base.y));
    setAttrIf(line, "x2", px(air.x));
    setAttrIf(line, "y2", px(air.y));

    setShown(veh3dAltEl, true);
    setTextIf(veh3dAltEl, Corvus.units.formatLength(vehAltAgl));
    const label = heightLabelPosition(air, base, scale);
    setTransform(veh3dAltEl, `translate(0, -50%) translate(${px(label.x)}px, ${px(label.y)}px)`);
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
    setShown(veh3dEl, true);
    // No rotateX: the globe's camera is looking at a sphere, and laying the
    // marker into a ground plane that is not flat would only skew it.
    setTransform(veh3dEl, `translate(-50%, -50%) translate(${px(at.x)}px, ${px(at.y)}px)`);
    const body = veh3dEl.querySelector(".v-body");
    setTransform(body, `rotate(${px(vehDisplay.heading - map.getBearing())}deg)`);
    setShown(veh3dShadowEl, false);
    setLeadShown(false);
    setShown(veh3dAltEl, false);
  }

  /** Redraw the track in the air when it has something new to show: on its
   *  own while the camera is still (see veh3dMatrix), else with a map frame. */
  function requestTrack3DFrame() {
    if (!threeD || !map) return;
    if (veh3dMatrixValid && !mapInMotion()) {
      try { renderTrack3D(veh3dMatrix); return; } catch (_error) { /* a full frame below */ }
    }
    if (typeof map.triggerRepaint === "function") map.triggerRepaint();
  }

  /** The track's colours, read from the theme once rather than per frame. */
  function track3DColors() {
    if (!track3dPalette) {
      track3dPalette = {
        track: vehicleColor || Corvus.ui.token("--track", "#E4322F"),
        casing: Corvus.ui.token("--bg", "#0B0E12"),
        past: pastColors(),
      };
    }
    return track3dPalette;
  }

  /**
   * Where a flight's heights are measured from, in the frame the aircraft
   * is drawn in, or null when that is not known yet.
   *
   * The points carry their height above home. The flight in progress takes
   * the aircraft's own datum (vehicleDrawAltitude less that height), so its
   * tip meets the aircraft wherever the aircraft is drawn. An earlier flight
   * takes the ground under its first point, where it took off. Flat ground
   * is sea level, as for the aircraft.
   */
  function track3DBase(coords, current) {
    if (current && vehAltDraw !== null && vehAltAgl !== null) return vehAltDraw - vehAltAgl;
    if (!terrainOn) return 0;
    const now = Date.now();
    const known = track3dGround.get(coords);
    if (known && now - known.at < TRACK3D_GROUND_MS) return known.value;
    const value = terrainElevation(coords[0]);
    track3dGround.set(coords, { at: now, value });
    return value;
  }

  /** Mercator x, y and z per metre for one track point, computed once. */
  function track3DMercatorOf(c) {
    let m = track3dMercator.get(c);
    if (!m) {
      if (typeof maplibregl === "undefined") return null;
      const mc = maplibregl.MercatorCoordinate.fromLngLat({ lng: c[0], lat: c[1] }, 1);
      m = [mc.x, mc.y, mc.z];
      track3dMercator.set(c, m);
    }
    return m;
  }

  /**
   * Trace one flight at its height into *sink* (a canvas context, or
   * anything with moveTo and lineTo). Returns the number of points traced.
   *
   * The same projection as projectAltitude, unrolled: this runs for every
   * point on every frame. A point without a height, or behind the camera,
   * breaks the line rather than joining across it.
   */
  function traceTrack3D(sink, matrix, coords, base, width, height) {
    let open = false;
    let traced = 0;
    for (let i = 0; i < coords.length; i++) {
      const c = coords[i];
      const m = c.length > 2 && isFinite(c[2]) ? track3DMercatorOf(c) : null;
      if (!m) { open = false; continue; }
      const z = m[2] * (base + c[2]);
      const w = matrix[3] * m[0] + matrix[7] * m[1] + matrix[11] * z + matrix[15];
      if (!(w > 1e-6)) { open = false; continue; }
      const x = ((matrix[0] * m[0] + matrix[4] * m[1] + matrix[8] * z + matrix[12]) / w * 0.5 + 0.5) * width;
      const y = (0.5 - (matrix[1] * m[0] + matrix[5] * m[1] + matrix[9] * z + matrix[13]) / w * 0.5) * height;
      if (open) sink.lineTo(x, y);
      else { sink.moveTo(x, y); open = true; }
      traced++;
    }
    return traced;
  }

  /** The canvas sized to the map in device pixels, or null. */
  function track3DContext(width, height) {
    if (!track3dEl || typeof track3dEl.getContext !== "function") return null;
    const ratio = map && typeof map.getPixelRatio === "function"
      ? map.getPixelRatio() : (window.devicePixelRatio || 1);
    const w = Math.round(width * ratio), h = Math.round(height * ratio);
    if (track3dEl.width !== w || track3dEl.height !== h) {
      track3dEl.width = w;
      track3dEl.height = h;
      track3dEl.style.width = width + "px";
      track3dEl.style.height = height + "px";
    }
    const ctx = track3dEl.getContext("2d");
    if (ctx) ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    return ctx;
  }

  function clearTrack3D() {
    if (!track3dDrawn) return;
    track3dDrawn = false;
    const ctx = track3dEl && typeof track3dEl.getContext === "function" ? track3dEl.getContext("2d") : null;
    if (!ctx) return;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, track3dEl.width, track3dEl.height);
  }

  /**
   * Draw the flown track at the height it was flown, over the one on the
   * ground. Same hook and same matrix as the aircraft, so the two agree.
   * Not on the globe, for the reason renderVehicleOnGlobe gives.
   */
  function renderTrack3D(matrix) {
    if (!track3dEl) return;
    const show = threeD && trackInAir && !!matrix && !!map && !usingGlobe()
      && (pathCoords.length >= 2 || pastFlights.length > 0);
    const [width, height] = show ? viewSize() : [0, 0];
    if (!show || !(width > 0 && height > 0)) { clearTrack3D(); return; }
    const ctx = track3DContext(width, height);
    if (!ctx) return;
    ctx.clearRect(0, 0, width, height);
    track3dDrawn = true;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    const colors = track3DColors();
    const stroke = (coords, current, color) => {
      if (coords.length < 2) return;
      const base = track3DBase(coords, current);
      if (base === null) return;
      ctx.beginPath();
      if (traceTrack3D(ctx, matrix, smoothTrack(coords), base, width, height) < 2) return;
      ctx.globalAlpha = TRACK3D_CASING_OPACITY;
      ctx.strokeStyle = colors.casing;
      ctx.lineWidth = current ? TRACK3D_CASING_PX : TRACK3D_PAST_CASING_PX;
      ctx.stroke();
      ctx.globalAlpha = current ? 1 : TRACK3D_PAST_OPACITY;
      ctx.strokeStyle = color;
      ctx.lineWidth = current ? TRACK3D_CORE_PX : TRACK3D_PAST_CORE_PX;
      ctx.stroke();
    };
    pastFlights.forEach((coords, i) => stroke(coords, false, colors.past[i] || colors.track));
    stroke(pathCoords, true, colors.track);
    ctx.globalAlpha = 1;
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
          const matrix = renderMatrix(args);
          veh3dMatrixValid = !!matrix && matrix.length === 16;
          if (veh3dMatrixValid) veh3dMatrix.set(matrix);
          try { renderTrack3D(matrix); } catch (_error) { /* one bad frame */ }
          try { renderVehicle3D(matrix); } catch (_error) { /* one bad frame */ }
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
   *  not a swing into it. */
  function tiltTo(pitch) {
    setCameraGoal({ pitch });
    cameraMove({ duration: TILT_MS }, Corvus.anim.reducedMotion());
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
   * going to "simple" has to take the buildings back out, and going to
   * "full" has to build them. Both directions run
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
    let terrainStarting = false;
    if (on) {
      // Everything that touches the style waits for the style. The rail is
      // built before "load" (so the operator is never left without controls
      // during the first tile fetch), which means this can be pressed before
      // there is a style to add a source to; the "load" handler re-applies.
      if (started) {
        // Which projection the zoom calls for: the globe zoomed out to the
        // country, the flat map over the field. Through wantsGlobe and the
        // CURRENT state, so entering 3D at a zoom near the line does not
        // pick the one applyProjectionForZoom would take straight back.
        const wantGlobe = wantsGlobe(map.getZoom(), globeOn);
        if (wantGlobe !== globeOn) setGlobe(wantGlobe);
        globeOn = wantGlobe;
        // Terrain in both modes, on both projections. The tilt waits for it
        // a moment (see enableTerrain).
        terrainStarting = enableTerrain();
        if (full) {
          addBuildingLayer();
          refreshBuildings();
        } else {
          // Also the path from "full" to "simple" with 3D staying on, which
          // is why it removes rather than merely not adding.
          removeBuildingLayer();
        }
        setSky(true);
        setGround(true);
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
      if (terrainStarting) tiltAfterTerrain(terrainGeneration);
      else if (pendingTilt === null) tiltTo(THREE_D_PITCH);
    } else {
      hideVehicle3D();
      removeBuildingLayer();
      setSky(false);
      setGround(false);
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
      map: {
        three_d: threeDMode(), three_d_detail: threeDDetail,
        terrain_source: terrainChoice,
      },
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
    if (typeof map3d.terrain_source === "string" && map3d.terrain_source) {
      terrainChoice = map3d.terrain_source;
      chooseTerrain();
    }
    set3DDetail(map3d.three_d_detail || map3d.three_d);
    const mode = normaliseThreeDMode(map3d.three_d);
    if (mode === THREE_D_OFF || threeD) return;
    if (started) set3DMode(mode);
    else window.addEventListener("corvus:mapready", () => {
      if (!threeD) set3DMode(mode);
    }, { once: true });
  }

  /**
   * Ask again for the tiles in view that failed, twice at most each.
   *
   * MapLibre gives up on a tile after one failure, says nothing when the
   * answer was a 404 (which is how our backend answers an upstream that
   * timed out), and only asks again once the view moves. So a single slow
   * answer left a blurry or plain patch until the operator nudged the map.
   * Checked when the map goes idle, which is when every tile has either
   * loaded or failed: if all loaded this is one pass over the tiles in view
   * and nothing else. A tile that loads is forgotten (onTileData), so it gets
   * its retries again the next time. Offline the backend answers a miss at
   * once, so the retries cost two quick requests per missing tile.
   */
  function onMapIdle() {
    if (tileRetryTimer !== null || !retryableTiles().length) return;
    tileRetryTimer = window.setTimeout(() => {
      tileRetryTimer = null;
      const failed = retryableTiles();
      for (const { sourceId, z, x, y, drop } of failed) {
        const key = `${sourceId} ${z}/${x}/${y}`;
        if (tileRetries.size > 2000) tileRetries.clear();
        tileRetries.set(key, (tileRetries.get(key) || 0) + 1);
        // Dropped, so the next update asks for it as a new tile, the way a
        // pan does. Not map.refreshTiles: it marks a failed raster tile
        // "expired", which MapLibre takes for a tile with an image, and every
        // frame then threw on the missing texture and drew a blank map.
        try { drop(); } catch (_error) { /* gone with a switch of map or mode */ }
      }
      if (failed.length && typeof map._update === "function") map._update(false);
    }, TILE_RETRY_MS);
  }

  function retryableTiles() {
    return failedTilesInView().filter(({ sourceId, z, x, y }) =>
      !beyondSparseLevel(sourceId, z)
      && (tileRetries.get(`${sourceId} ${z}/${x}/${y}`) || 0) < TILE_RETRIES);
  }

  /** Is *z* above the level an elevation source like Copernicus covers
   *  everywhere? Above it a missing tile is no finer data there, not a
   *  failure: the ground uses the level above, and asking again only buys
   *  the same 404. */
  function beyondSparseLevel(sourceId, z) {
    const id = terrainSourceId();
    if (!id || (sourceId !== id && sourceId !== hillshadeSourceId(id))) return false;
    // null for every source without one, and Number(null) is 0.
    const sparse = terrainSpec.sparse_above;
    return typeof sparse === "number" && z > sparse;
  }

  /** The tiles in view that failed, as {sourceId, z, x, y, drop}. Not
   *  public API: MapLibre 6 keeps them in tileManagers[id]._inViewTiles and
   *  removes one with _removeTile. Anything else answers none, so an upgrade
   *  that moves them costs the retries, not the map. */
  function failedTilesInView() {
    const out = [];
    try {
      const managers = (map && map.style && map.style.tileManagers) || {};
      for (const [sourceId, manager] of Object.entries(managers)) {
        const tiles = manager && manager._inViewTiles && manager._inViewTiles.getAllTiles
          && typeof manager._removeTile === "function"
          ? manager._inViewTiles.getAllTiles() : [];
        for (const tile of tiles) {
          const c = tile && tile.state === "errored" && tile.tileID && tile.tileID.canonical;
          if (c) {
            out.push({ sourceId, z: c.z, x: c.x, y: c.y,
              drop: () => manager._removeTile(tile.tileID.key) });
          }
        }
      }
    } catch (_error) { /* a MapLibre that keeps its tiles elsewhere */ }
    return out;
  }

  function onTileData(event) {
    const c = event && event.tile && event.tile.tileID && event.tile.tileID.canonical;
    if (c && event.sourceId) tileRetries.delete(`${event.sourceId} ${c.z}/${c.x}/${c.y}`);
  }

  function init(mapEl, controlsEl) {
    // MapLibre keeps 16 tile requests in flight and queues the rest in order.
    // The browser opens only a few connections to our server anyway, so a
    // tile the browser could answer from its cache (ground seen before, the
    // coarse elevation the terrain falls back to) waited behind slow ones
    // instead, and the terrain broke into holes until it came. Handed to the
    // browser, a cached tile comes back at once; the server sees no more
    // requests at a time than before.
    if (typeof maplibregl.setMaxParallelImageRequests === "function") {
      maplibregl.setMaxParallelImageRequests(MAX_PARALLEL_TILE_REQUESTS);
    }
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
    Corvus.mapOverlays.attach(map);
    map.on("idle", onMapIdle);
    map.on("data", onTileData);
    // Attribution is a legal requirement, so it is always on — but it belongs in
    // the corner nothing else competes for. Bottom-right is where the control
    // rail and the HUD live, so it goes bottom-left, with the clear-track button
    // stacked above it (see .track-clear in map.css).
    addAttribution(map);
    holdThroughPanel(map);

    // Controls are built NOW, not on "load". MapLibre fires "load" only once the
    // style AND its first tiles have resolved, so building the rail there left
    // the operator staring at a map with no zoom, layer or centre buttons for as
    // long as the first tile fetch took — seconds on a cold cache, and much
    // worse offline. Nothing in the rail needs a loaded style: the handlers act
    // on the map object (which exists from the constructor), and setBaseLayer
    // already defers its own work until `started`.
    buildControls(controlsEl);
    restoreTrack();
    // Written on the way out as well, so the last few seconds of a flight are
    // not lost to the save delay when the app is closed straight after it.
    window.addEventListener("pagehide", saveTrack);
    buildTrackControl(mapEl);
    updateTrackControl();
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
      operatorMoving = true;
      // The operator has the camera now: nothing we asked for earlier is
      // taken back from them. A wheel only zooms, so only the zoom goes.
      cameraGoal.zoom = null;
      if (source.type !== "wheel") cameraGoal.pitch = null;
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
      operatorMoving = false;
      // Also after the camera's own ground height was set: if that stopped a
      // tilt or a pan part way, the goal it was carrying is asked for again.
      settleCameraGoal();
      // But nothing else. It is not a new view, and answering it would only
      // ask for the height that was just written.
      if (elevationWriting) return;
      // The camera landed somewhere new: it may have crossed the zoom where
      // the globe hands over to the flat map, and its ground height may be
      // stale.
      // Not every way the camera can move interpolates that height — a follow
      // jump and a plain setCenter do not — so reconcile it here as well as
      // on DEM tiles.
      applyProjectionForZoom();
      scheduleTerrainSync(true);
      // New ground came into view; ask the backend for its buildings. A no-op
      // outside 3D, and debounced, so a drag costs one pass over the grid.
      refreshBuildings();
    });

    // Anything that moves the camera or resizes the canvas leaves the last
    // frame's matrix stale until the frame it causes, which renews it.
    for (const type of ["move", "resize", "terrain", "projectiontransition"]) {
      map.on(type, () => { veh3dMatrixValid = false; });
    }

    map.on("load", () => {
      // Regions first: their layers must sit UNDER the track and plan route,
      // and MapLibre stacks in insertion order.
      addRegionLayer();
      addPathLayer();
      addWaypointRouteLayer();
      // The vehicles not being flown, under this one's track.
      if (Corvus.mapFleet) Corvus.mapFleet.attach(map);

      // Painted on the ground like a helipad: flat under a tilted camera, and
      // turning with the map.
      homeMarker = new maplibregl.Marker({
        element: buildHomeMarker(), anchor: "center", pitchAlignment: "map", rotationAlignment: "map",
      })
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
        // Stepped at the map rate, not on every display frame (see anim.js).
        map: true,
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
      if (Corvus.fleet) Corvus.fleet.subscribe(applyVehicleColor);
      started = true;
      Corvus.mapOverlays.markStarted();
      // Whatever a plugin drew while the style was still loading.
      overlays.forEach((_o, id) => drawOverlay(id));
      // MapLibre paint properties are literal colours, so a theme switch has
      // to push new ones — the same reason the Plotly charts subscribe.
      Corvus.ui.onThemeChange(repaintTrack);
      // An overlay's casing is the page background, like the track's.
      Corvus.ui.onThemeChange(() => overlays.forEach((_o, id) => drawOverlay(id)));
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
    // Shared with the mission planner's map, which sits beside the panel too.
    holdThroughPanel,
    panelRestPadding,
    setWaypointMode,
    // The flight bar's OPTIONAL narrowing rule: whether it answers to a share
    // of the map column (captions off early, as the window narrows) on top of
    // the fit it always answers to. Off unless Settings asks; app.js applies
    // the config's answer once that fetch lands.
    setFlightBarShrink,
    setMeasureTool,
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
    // The elevation model 3D reads from: a DEM id, or "auto".
    setTerrainSource,
    getTerrainSource: () => terrainChoice,
    getTerrainSpec: () => terrainSpec,
    hasTerrain: () => terrainOn,
    getWaypoints,
    clearWaypoints,
    onWaypointsUpdate,
    // Lines drawn on this map by plugins, always under the flown track. The
    // plugin api (js/plugins.js) namespaces the ids, so one plugin cannot
    // replace or remove another's.
    setOverlay,
    setPolygonOverlay,
    setTextOverlay,
    setButtonOverlay,
    setMarkerOverlay,
    pickPoint,
    removeOverlay,
    setOverlayVisible,
    hasOverlay: (id) => overlays.has(id),
    fitCoords,
    // test hook: the stored overlays, as plain data.
    _overlays: () => {
      const out = {};
      overlays.forEach((o, id) => { out[id] = Object.assign({}, o, { coords: o.coords.map((c) => c.slice()) }); });
      return out;
    },
    // Context menu: the map owns where and when it opens, app.js owns what the
    // rows do (they are flight commands, and those carry the topbar's attempt
    // tracking and the armed gating that live there).
    setContextActions,
    closeContextMenu,
    getSources,
    // Re-read the catalogue after a map key was saved in Settings.
    refreshSources,
    getCacheStats,
    // Settings' map-service picker drives the live map through this, so the
    // Home tab's layer switcher and the Appearance page can never disagree
    // about which base layer is showing.
    setBaseLayer,
    getBaseLayer: () => activeLayer,
    // The aircraft's mark, for the Mission planner's map: one drawing of the
    // aircraft in the app, so the planner's is recognisably the same one.
    buildVehicleMarker,
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
    // Which vehicle the track and the marker are drawn for; Corvus.mapFleet
    // draws every other one.
    trackOwner: () => trackOwner,
    updateTrackControl,
    getEarlierFlights: () => pastFlights.map((f) => f.map((c) => c.slice())),
    isTrackRainbow: () => trackRainbow && trackEarlierFlights,
    _pastGeometry: pastGeometry,
    // Settings' two track switches; app.js applies the config's answer with
    // startup = true once that fetch lands.
    setTrackOptions,
    setRegions,
    loadRegions,
    setRegionsVisible,
    mergeRegions,
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
    _showsHeightLine: (agl, onGround) => showsHeightLine(agl, onGround),
    _heightLabelPosition: (air, base, scale) => heightLabelPosition(air, base, scale),
    _projectAltitude: projectAltitude,
    // test hook: where MapLibre keeps its camera transform, in each version.
    _transformOf: (m) => transformOf(m),
    // test hook: one flight traced at its height, into any moveTo/lineTo sink.
    _traceTrack3D: (sink, matrix, coords, base, width, height) =>
      traceTrack3D(sink, matrix, coords, base, width, height),
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
    // test hook: the globe/flat-map handover rule, including its hysteresis.
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
    // test hook: one ground-height reconciliation, and whether it settled.
    // It is what kept writing the camera height four times a second.
    _syncTerrainElevation: () => syncTerrainElevation(),
    _setTerrainSpec: (spec) => {
      terrainSpec = spec;
      terrainCatalogue = spec ? [spec] : [];
      defaultTerrainId = spec ? spec.id : null;
    },
    // test hook: which DEM a map service gets, as a pure rule.
    _terrainFor: (dems, provider, defaultId, choice, preferredId) =>
      terrainFor(dems, provider, defaultId, choice, preferredId),
    _globeZooms: () => ({ max: GLOBE_MAX_ZOOM, band: GLOBE_ZOOM_BAND }),
    _buildingLimits: () => ({
      cellZoom: BUILDING_CELL_Z,
      maxCells: BUILDING_MAX_CELLS,
      maxFeatures: BUILDING_MAX_FEATURES,
      minZoom: BUILDING_MIN_ZOOM,
    }),
    // test hooks: the pure track rules — distance decimation and the reboot
    // edge — assertable without a map.
    _recordTrackPoint: (pos, hAcc, alt) => recordTrackPoint(pos, hAcc, alt),
    // test hook: the track as drawn, curves filled in.
    _smoothTrack: (coords) => smoothTrack(coords),
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
    _checkForTakeoff: (armed) => checkForTakeoff(armed),
    _noteVehicle: (state) => noteVehicle(state),
    _updateTrack: (state) => updateTrack(state),
    _saveTrack: () => saveTrack(),
    _restoreTrack: () => restoreTrack(),
    // test hook: push one telemetry sample through the whole marker path
    // (target, altitude, follow, track) without an SSE stream behind it.
    // Mirrors the _updateHome convention.
    _updateVehicle: (state) => updateVehicle(state),
    // test hook: the open menu's point, so the click -> menu path is assertable
    // without a layout engine.
    _contextPoint: () => (contextPoint ? { lng: contextPoint.lng, lat: contextPoint.lat } : null),
    _openContextMenu: (lngLat) => openContextMenu(lngLat),
    _trackInAir: () => trackInAir,
    _resetTrackState: () => {
      if (trackPaintTimer !== null) window.clearTimeout(trackPaintTimer);
      trackPaintTimer = null;
      pathCoords = [];
      pastFlights = [];
      lastBootMs = null;
      lastArmed = null;
      trackOwner = null;
      trackEarlierFlights = true;
      trackRainbow = false;
      trackClearOnRestart = false;
      trackInAir = true;
      if (trackSaveTimer !== null) window.clearTimeout(trackSaveTimer);
      trackSaveTimer = null;
    },
    // test hook: pure coordinate computation for the plan route (vehicle
    // position prefix + operator waypoints, or []). Accepts an optional
    // waypoint list for testing in Node, where the internal `waypoints` array
    // cannot be populated without a map; omit it to use the live waypoints.
    // Never touches wpRouteSource (null in Node). Mirrors the _sources convention.
    _planRouteCoords: (wps) => computePlanCoords(wps),
  };
})();
