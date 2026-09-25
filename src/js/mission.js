"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.mission — the Mission planner page.

  A flight drawn before it is flown: a start point, waypoints, orbits and a
  landing on a map, with the whole route's altitude shown underneath as a
  profile the operator can drag each point's height on. Nothing here talks to
  the aircraft until Upload is pressed, and Upload and Fly are two buttons
  rather than one — a planned mission is reviewed and walked out to, unlike the
  Home tab's fly-to-points, which is one gesture from click to flight.

  Off by default; Settings > Appearance > Pages puts MISSION in the left rail
  (see Corvus.sidenav.setMissionEnabled).

  WHAT OWNS WHAT

  * `items` is the mission, in order, and the single source of truth. Every
    surface — the markers, the route line, the list, the profile — is DRAWN
    from it and never holds state of its own. That is what lets a drag on the
    chart and a drag on the map be the same edit.
  * The item shape mirrors corvus/mission.py exactly: the type names, the field
    names, and the bounds. The Python side is the validator and the only place
    a MAV_CMD is named, so this file carries no command numbers at all.
  * The page owns a SECOND MapLibre map. It is not the Home tab's — that one
    follows the aircraft and is the live picture — and the two never share
    state beyond the base layer the operator picked in Settings, which both
    read. teardown() is what stops that second map, its markers, the Plotly
    chart and the resize listener from outliving the page.
  * The TILES under both maps are one store. They are served from the backend's
    .mbtiles through /api/tiles/<source>/z/x/y.png, so an area downloaded from
    this page is an area the Home map has, with no second copy and nothing to
    keep in step. That is why the offline-download control is here at all:
    planning is when an operator discovers what imagery they are missing, and
    the fix belonged on the same screen. Corvus.tiles is handed this map while
    the page is up (see tileHost) and handed it back on teardown.

  ALTITUDES are metres above the HOME position, which is the frame PX4 flies a
  MAV_FRAME_GLOBAL_RELATIVE_ALT mission in and therefore the only frame the
  operator should ever be typing in. The terrain in the profile is converted
  into the same frame (ground minus home elevation), so the vertical distance
  between the plan line and the ground is the real clearance, readable with a
  ruler rather than with arithmetic.
*/
Corvus.mission = (function () {
  // ---- Contract with corvus/mission.py --------------------------------
  // These bounds are the SAME numbers the backend validates against. They are
  // restated (not fetched) because the editor has to refuse an impossible
  // value while the operator is dragging, not after a round trip — and a test
  // pins the two together so they cannot drift.
  const ALT_MIN_M = -100;
  const ALT_MAX_M = 1000;
  const RADIUS_MIN_M = 1;
  const RADIUS_MAX_M = 10000;
  const HOLD_MAX_S = 3600;
  const TURNS_MAX = 100;
  const SPEED_MIN_MS = 0.5;
  const SPEED_MAX_MS = 100;
  const MAX_ITEMS = 255;
  const POINT_NAME_MAX = 40;

  // The altitude a freshly placed point gets. 30 m is a working height for
  // almost everything and is below the 120 m ceiling the takeoff button
  // enforces, so a plan drawn without thinking about altitude is still a legal
  // one; the profile is where it gets adjusted.
  const DEFAULT_ALT_M = 30;
  const DEFAULT_RADIUS_M = 50;
  // Above this, a plan is legal in most of Europe only with a permit. Not
  // refused — a survey aircraft may genuinely be up there — but said out loud.
  const CEILING_HINT_M = 120;
  // Ground clearance under which the profile calls a leg out. Not a refusal
  // either: the DEM is a 30 m grid and a real tree is not in it, which is
  // exactly why the number is a warning and not a gate.
  const CLEARANCE_WARN_M = 15;
  // Cruise speed used for the duration estimate when the plan pins none.
  const NOMINAL_SPEED_MS = 10;

  // Which way round an orbit is flown. Two things, not a range, so it is a
  // picker rather than a number field — and PX4 carries it as the SIGN of the
  // loiter radius, which corvus/mission.py applies on the way to the wire.
  const DIRECTIONS = [
    { value: 1, label: "Clockwise" },
    { value: -1, label: "Counter-clockwise" },
  ];

  // What cannot be part of a one-line name: control characters and the line
  // and paragraph separators. Mirrors _POINT_NAME_JUNK_RE in corvus/mission.py.
  const POINT_NAME_JUNK = /[\u0000-\u001f\u007f-\u009f\u2028\u2029]+/g;

  // One row per item type. `params` names the editable fields and their
  // bounds; the names are the contract with ITEM_SPECS in corvus/mission.py.
  // A point's own SPEED is not in here, for the same reason it is not in
  // ITEM_SPECS: it is not a parameter of the item but a command of its own on
  // the wire, and it is optional in a way a param with a default cannot be.
  const TYPES = {
    takeoff: {
      label: "Takeoff", icon: "plane-takeoff", position: true, color: "healthy",
      hint: "Takes off from the start point and climbs to this height before flying on.",
      params: { pitch: { label: "Climb pitch", unit: "°", min: 0, max: 45, step: 1, def: 0 } },
    },
    waypoint: {
      label: "Waypoint", icon: "map-pin", position: true, color: "nav",
      hint: "Fly to this point at this height.",
      params: {
        hold: { label: "Hold", unit: "s", min: 0, max: HOLD_MAX_S, step: 1, def: 0 },
        accept_radius: { label: "Accept radius", unit: "m", min: 0, max: 1000, step: 1, def: 0 },
      },
    },
    loiter_turns: {
      label: "Circle", icon: "rotate-cw", position: true, color: "warning",
      hint: "Orbit this point a set number of times, then continue.",
      hoverHint: "Hold over this point, then continue. A multirotor does not fly the orbit.",
      params: {
        turns: { label: "Turns", unit: "", min: 0, max: TURNS_MAX, step: 0.5, def: 1 },
        radius: { label: "Radius", unit: "m", min: RADIUS_MIN_M, max: RADIUS_MAX_M, step: 1, def: DEFAULT_RADIUS_M },
        direction: { label: "Direction", min: -1, max: 1, def: 1, choices: DIRECTIONS },
      },
    },
    loiter_time: {
      label: "Hold", icon: "timer", position: true, color: "warning",
      hint: "Circle this point for a set time, then continue.",
      hoverHint: "Hold position over this point for a set time, then continue.",
      params: {
        seconds: { label: "Time", unit: "s", min: 0, max: HOLD_MAX_S, step: 1, def: 30 },
        radius: { label: "Radius", unit: "m", min: RADIUS_MIN_M, max: RADIUS_MAX_M, step: 1, def: DEFAULT_RADIUS_M },
        direction: { label: "Direction", min: -1, max: 1, def: 1, choices: DIRECTIONS },
      },
    },
    land: {
      label: "Land", icon: "plane-landing", position: true, color: "accent",
      hint: "Descend and touch down here. Ends the mission.",
      params: {},
    },
    rtl: {
      label: "Return", icon: "house", position: false, color: "accent",
      hint: "Fly home and land there. Ends the mission.",
      params: {},
    },
  };

  // The tools on the map's own rail, top left. "select" is the resting state:
  // a map click with no tool armed selects nothing and changes nothing, which
  // is what makes panning around a finished plan safe.
  //
  // There is no TAKEOFF tool. The start and the takeoff were two tools for
  // one place, and which one to press first was a question the operator had
  // to be told the answer to. START places both: where the aircraft stands,
  // and the climb from there that is the mission's first item.
  const TOOLS = [
    { id: "select", icon: "mouse-pointer-2", label: "SELECT", title: "Select and move points" },
    { id: "home", icon: "plane-takeoff", label: "START", title: "Set the start: the aircraft takes off here" },
    { id: "divider" },
    { id: "waypoint", icon: "map-pin", label: "POINT", title: "Add a waypoint" },
    { id: "loiter_turns", icon: "rotate-cw", label: "CIRCLE", title: "Add a circle (orbit)" },
    { id: "loiter_time", icon: "timer", label: "HOLD", title: "Add a timed hold" },
    { id: "land", icon: "plane-landing", label: "LAND", title: "End the mission with a landing" },
    { id: "divider2" },
    { id: "rtl", icon: "house", label: "RETURN", title: "End the mission with a return to the start" },
  ];

  // The item types that end a mission. Nothing after one is flown, so a plan
  // has at most one and it is always last.
  const ENDS = { land: true, rtl: true };
  const END_NAMES = { land: "a landing", rtl: "a return" };
  // How close a takeoff has to stand to the start to BE the start's takeoff,
  // rather than a separate place the aircraft flies to before it climbs.
  const TIED_M = 0.5;

  // The plan's colour and the dark casing under it, both from css/themes.css
  // where they are defined once for every theme — see --plan there for why
  // they are not retuned per palette. Read as functions rather than captured
  // at load, because Corvus.ui.token needs a document to read from.
  const planColor = () => Corvus.ui.token("--plan", "#FFC21A");
  const planInk = () => Corvus.ui.token("--plan-ink", "rgba(0,0,0,0.7)");

  // How the route is drawn. The dashes are the Home tab's; the casing is what
  // makes them legible on imagery nobody chose — a bright amber alone reads
  // on a field and disappears on a white roof. Solid rather than dashed,
  // because a dasharray is measured in LINE WIDTHS: the same [3, 2] on a wider
  // casing would be a longer dash on a longer period, and the two would drift
  // out of phase along the route.
  const PLAN_DASH = [3, 2];
  const PLAN_LINE_W = 3.2;
  const PLAN_CASING_W = PLAN_LINE_W + 3.2;
  const ORBIT_LINE_W = 2.6;
  const ORBIT_CASING_W = ORBIT_LINE_W + 2.8;

  // The zoom band that weight is spread over. The widths above are what the
  // route is drawn at with the camera in close, over ground the operator can
  // see the edges of; pulled back far enough to hold a five-kilometre plan,
  // the same line is a hairline on a photograph. So it is quoted near and
  // widened far — what has to stay constant is how findable the route is, not
  // how many pixels it is.
  const WIDTH_ZOOM_NEAR = 17;
  const WIDTH_ZOOM_FAR = 9;
  const WIDTH_FAR_FACTOR = 1.75;
  // The same argument for the markers, which are DOM elements MapLibre draws
  // at a fixed pixel size whatever the camera does.
  const POINT_SCALE_FAR = 1.45;

  /** A line width as a MapLibre zoom expression: *width* close in, wider out. */
  function widthByZoom(width) {
    return ["interpolate", ["linear"], ["zoom"],
      WIDTH_ZOOM_FAR, Math.round(width * WIDTH_FAR_FACTOR * 100) / 100,
      WIDTH_ZOOM_NEAR, width];
  }

  /** How much bigger than its quoted size a marker is drawn at *zoom*.
   *  Published to CSS as a variable rather than applied as a transform:
   *  MapLibre writes its own transform onto every marker element INLINE, and a
   *  stylesheet cannot win against that — which is exactly why the selected
   *  state's `transform: scale()` never did anything. */
  function pointScaleFor(zoom) {
    const value = Number(zoom);
    if (!isFinite(value)) return 1;
    const held = Math.min(WIDTH_ZOOM_NEAR, Math.max(WIDTH_ZOOM_FAR, value));
    const out = (WIDTH_ZOOM_NEAR - held) / (WIDTH_ZOOM_NEAR - WIDTH_ZOOM_FAR);
    return Math.round((1 + out * (POINT_SCALE_FAR - 1)) * 1000) / 1000;
  }

  // The item types that orbit, and so have a radius to drag and a direction to
  // fly it in. Mirrors ORBIT_TYPES in corvus/mission.py.
  const ORBIT_TYPES = ["loiter_turns", "loiter_time"];

  /*
    WHO ACTUALLY FLIES A LOITER RADIUS

    "Loiter" in MAVLink does not mean stand still — it means stay at this
    point. An aircraft that cannot hover stays at a point by flying a circle
    around it, and MAV_CMD_NAV_LOITER_TIME / _TURNS carry that circle in
    param3: its size, and in its SIGN the direction. That is the whole reason a
    Hold has a radius at all.

    A multirotor holds the point itself. PX4's multicopter side flies a
    position hold, so the radius is a number the aircraft never uses — and the
    ring the planner drew around it was a promise about the flight path that
    the airframe does not keep. So on a multirotor the two fields and the ring
    are not shown.

    Only a POSITIVELY identified multirotor hides them. A fixed wing needs
    them, a VTOL may hold in either mode, and an unknown or absent vehicle is
    not an argument for hiding a field the plan carries — a plan drawn with
    nothing connected must still be flyable by the aeroplane it was drawn for.

    Nothing is dropped from the PLAN. The stored radius stays in the file and
    on the wire, so the same mission opened with a fixed wing connected has its
    circle back, unchanged. The airframe decides what is SHOWN, never what is
    saved. Types are the MAV_TYPE names MAV_TYPE_MAP in corvus/mavlink_bridge.py
    puts into the state store.
  */
  const HOVER_TYPES = [
    "QUADROTOR", "COAXIAL", "HELICOPTER", "HEXAROTOR", "OCTOROTOR", "TRIROTOR",
  ];

  /** Does the connected aircraft hold a loiter point rather than circle it? */
  function vehicleHovers() {
    const state = Corvus.telemetry && typeof Corvus.telemetry.getState === "function"
      ? Corvus.telemetry.getState() : null;
    if (!state || !state.connected) return false;
    return HOVER_TYPES.indexOf(String(state.vehicle_type || "")) !== -1;
  }

  /** Is *key* a field this airframe would actually fly? */
  function paramApplies(item, key) {
    if (key !== "radius" && key !== "direction") return true;
    return !(isOrbit(item) && hovers);
  }

  const EARTH_RADIUS_M = 6371008.8;
  const DEFAULT_CENTER = [11.640969, 48.080217];   // same fallback as the Home map
  // Only reached when there is no Home map to take a view from — see
  // openingView. Close enough to place a takeoff point on a building.
  const FALLBACK_ZOOM = 15;
  // How many points the terrain profile is sampled at. Enough to show a ridge
  // between two waypoints, few enough that one plan is a handful of tiles.
  const PROFILE_SAMPLES = 240;
  // Tiles one terrain sample pass may touch. Past this the zoom drops a level,
  // which costs resolution the DEM does not really have anyway.
  const TERRAIN_TILE_BUDGET = 48;

  // ---- page state ------------------------------------------------------
  let map = null;
  let mapEl = null;
  let profileEl = null;
  let listEl = null;
  let listHeadEl = null;
  let sideScrollEl = null;
  let detailEl = null;
  let summaryEl = null;
  let issuesEl = null;
  let toolsEl = null;
  let status = null;

  let items = [];          // the mission, in order
  let home = null;         // {lat, lon, elevation|null} — the planned start
  let selectedId = null;
  let tool = "select";
  let planName = "Mission";
  let planSpeed = null;    // m/s, or null for "whatever the airframe does"
  let nextId = 1;

  let markers = [];        // maplibregl.Marker[], parallel to positioned items
  let homeMarker = null;
  let radiusHandle = null; // the selected orbit's radius grip, or null
  let routeSource = null;
  let orbitSource = null;
  let regionSource = null;
  let regionMarkers = [];
  let regions = [];
  let regionsVisible = true;
  let mapReady = false;
  let activeLayerId = "satellite";
  let pendingLayer = null;
  let layerMenuHandle = null;
  let controlsEl = null;
  let tilesTriggerEl = null;
  let clearPlanEl = null;

  // Terrain profile: the sampled ground under the current route, plus the
  // decoded DEM tiles it was read from. The tile cache survives an edit (the
  // ground does not move) and is dropped with the page.
  let terrainTiles = new Map();
  let terrainSpec = null;
  let ground = null;       // {distances: number[], elevations: number[]} | null
  let groundToken = 0;     // guards a slow sample pass against a newer edit
  let homeElevation = null;

  // The built page, kept between visits — see render/suspend/resume below.
  // Null before the first entry and after a teardown; anything else means the
  // page is only put down, not gone.
  let pageEl = null;
  let mapWrapEl = null;

  let profileGeom = null;  // pixel geometry of the last profile draw
  let dragState = null;
  // The list row being carried to a new place in the plan, or null. Nothing
  // to do with dragState above, which is the profile chart's altitude drag:
  // the two can never be in flight at once and they share nothing.
  let rowDrag = null;
  let resizeHandler = null;
  let toolsObserver = null;
  let themeUnsub = null;
  let vehicleUnsub = null;
  // Cached rather than read per draw: this decides what the panel and the map
  // show, and telemetry arrives many times a second.
  let hovers = false;
  // True while a move the OPERATOR started is in flight, so only those are
  // reported back as the view this map was left in.
  let aiming = false;
  // True while the status line is the "your plan is somewhere else" notice,
  // so a pan back onto the plan can take it down again without clearing a
  // message somebody else put there.
  let offscreenNotice = false;
  // Place search (see "Place search" below) — a Corvus.mapSearch handle, not
  // the box's own state: that lives in the component both maps share.
  let searchBox = null;
  // True whenever the page is not the one on screen — suspended as well as
  // torn down. Every async continuation in this module checks it before
  // writing into the DOM or the map, and both exits set it for that reason.
  let destroyed = false;

  // What the vehicle is flying, as far as it is THIS plan. `synced` is the
  // backend's mission revision when this plan was put on (or read off) the
  // aircraft, with the plan as it was then. An edit, or another station's
  // upload, ends the claim: a leg is never highlighted on a route the
  // aircraft is not flying.
  let synced = null;          // {revision, key} | null
  let progress = null;        // {item, reached, state} | null
  let vehicleMission = null;  // the mission fields of the last telemetry frame
  let vehicleSig = "";        // ...serialised, so an unchanged frame costs nothing
  let vehicleEl = null;
  let legSource = null;

  // =====================================================================
  // Pure geometry — no DOM, no map. Exported as test hooks at the bottom.
  // =====================================================================

  /** Great-circle distance in metres between two {lat, lon}. */
  function distanceM(a, b) {
    if (!a || !b) return 0;
    const lat1 = a.lat * Math.PI / 180;
    const lat2 = b.lat * Math.PI / 180;
    const dLat = lat2 - lat1;
    const dLon = (b.lon - a.lon) * Math.PI / 180;
    const h = Math.sin(dLat / 2) ** 2 +
      Math.cos(lat1) * Math.cos(lat2) * Math.sin(dLon / 2) ** 2;
    return 2 * EARTH_RADIUS_M * Math.asin(Math.min(1, Math.sqrt(h)));
  }

  /** A point *fraction* of the way from a to b, linearly in lat/lon.
   *  Good enough for profile sampling — the legs are kilometres, not
   *  continents, and the error over one leg is well under a DEM cell. */
  function lerpPoint(a, b, fraction) {
    return {
      lat: a.lat + (b.lat - a.lat) * fraction,
      lon: a.lon + (b.lon - a.lon) * fraction,
    };
  }

  /**
   * The route as profile stations: every positioned item in order, preceded by
   * home when there is one, each carrying the cumulative ground distance to
   * it and the altitude the aircraft is meant to be at.
   *
   * Home is included because the climb-out is part of the flight: a profile
   * that starts at the first waypoint hides the takeoff entirely. Its
   * altitude is 0 — home is, by definition, the zero of this frame.
   *
   * An RTL is a station too, drawn back at home: "and then it comes back" is
   * the part of a plan an operator most wants to see the length of.
   */
  function stations(list, start) {
    const out = [];
    let cumulative = 0;
    let previous = null;

    if (start) {
      previous = { lat: start.lat, lon: start.lon };
      out.push({ id: "home", type: "home", lat: start.lat, lon: start.lon, alt: 0, distance: 0 });
    }
    (list || []).forEach((item) => {
      const spec = TYPES[item.type];
      if (!spec) return;
      const place = spec.position ? item : start;
      if (!place) return;                       // an RTL with no home to return to
      if (previous) cumulative += distanceM(previous, place);
      previous = { lat: place.lat, lon: place.lon };
      out.push({
        id: item.id,
        type: item.type,
        lat: place.lat,
        lon: place.lon,
        alt: spec.position ? Number(item.alt) || 0 : 0,
        distance: cumulative,
      });
    });
    return out;
  }

  /** Ground distance the plan covers, orbit circumferences included. */
  function routeLength(list, start) {
    const points = stations(list, start);
    let total = points.length ? points[points.length - 1].distance : 0;
    (list || []).forEach((item) => {
      if (item.type === "loiter_turns") {
        total += 2 * Math.PI * (Number(item.radius) || 0) * (Number(item.turns) || 0);
      }
    });
    return total;
  }

  /** The speed each item is reached at, given the plan's own *speed* as the
   *  starting one: a point that pins a speed changes it from there on, and
   *  every point after it inherits that until another one says otherwise.
   *  Mirrors how PX4 holds the last DO_CHANGE_SPEED it executed, which is
   *  what corvus/mission.py emits. Keyed by item id. */
  function speedByItem(list, speed) {
    const out = new Map();
    let current = Number(speed) > 0 ? Number(speed) : NOMINAL_SPEED_MS;
    (list || []).forEach((item) => {
      if (Number(item.speed) > 0) current = Number(item.speed);
      out.set(item.id, current);
    });
    return out;
  }

  /** The speed a point is reached at as the plan stands, or null when nothing
   *  ahead of it pins one and the airframe's own default is what flies. */
  function speedInto(item) {
    if (!item) return null;
    let current = planSpeed;
    for (let i = 0; i < items.length; i += 1) {
      if (items[i].speed != null) current = items[i].speed;
      if (items[i].id === item.id) return current;
    }
    return current;
  }

  /** Seconds the plan is likely to take, at *speed* (or the nominal cruise)
   *  and at whatever speeds single points pin along the way. Timed holds are
   *  added on top — they are flight time the distance does not account for. */
  function routeDuration(list, start, speed) {
    const points = stations(list, start);
    const speeds = speedByItem(list, speed);
    const byId = new Map();
    (list || []).forEach((item) => byId.set(item.id, item));
    // Leg by leg rather than length-over-speed: with a speed pinned partway
    // through, the two stop being the same number.
    let seconds = 0;
    for (let i = 1; i < points.length; i += 1) {
      const mps = speeds.get(points[i].id) || NOMINAL_SPEED_MS;
      seconds += (points[i].distance - points[i - 1].distance) / mps;
    }
    (list || []).forEach((item) => {
      if (item.type === "loiter_turns") {
        const mps = speeds.get(item.id) || NOMINAL_SPEED_MS;
        seconds += 2 * Math.PI * (Number(item.radius) || 0)
          * (Number(item.turns) || 0) / mps;
      }
      if (item.type === "loiter_time") seconds += Number(item.seconds) || 0;
      if (item.type === "waypoint") seconds += Number(item.hold) || 0;
    });
    return seconds;
  }

  /** A closed ring of [lng, lat] approximating a circle of *radius* metres. */
  function circleRing(centre, radius, segments) {
    const count = segments || 64;
    const latRad = centre.lat * Math.PI / 180;
    const dLat = (radius / EARTH_RADIUS_M) * 180 / Math.PI;
    const dLon = dLat / Math.max(0.01, Math.cos(latRad));
    const ring = [];
    for (let i = 0; i <= count; i += 1) {
      const angle = (i / count) * 2 * Math.PI;
      ring.push([
        centre.lon + dLon * Math.cos(angle),
        centre.lat + dLat * Math.sin(angle),
      ]);
    }
    return ring;
  }

  /** Clamp a number into [min, max], falling back to *fallback* for junk. */
  function clampNumber(value, min, max, fallback) {
    const n = Number(value);
    if (!isFinite(n)) return fallback;
    return Math.min(max, Math.max(min, n));
  }

  // Stations that are MEANT to be on the ground. Their clearance is zero by
  // design, so counting them would put "0 m" in the summary for every correct
  // plan and make the one number that matters unreadable.
  const GROUNDED = { home: true, land: true, rtl: true };

  /**
   * Ground clearance at every station, or null where it does not apply — the
   * terrain is unknown, or the station is a start, a landing or a return.
   *
   * Both sides are in metres above home, so this is a subtraction rather than
   * a datum conversion — which is the whole reason the profile is drawn in
   * that frame.
   */
  function clearances(points, groundProfile) {
    if (!groundProfile || !groundProfile.distances.length) return points.map(() => null);
    return points.map((station) => {
      if (GROUNDED[station.type]) return null;
      const under = interpolateAt(groundProfile, station.distance);
      return under == null ? null : station.alt - under;
    });
  }

  /** Linear read of a sampled profile at *distance* metres along it. */
  function interpolateAt(profile, distance) {
    const xs = profile.distances;
    const ys = profile.elevations;
    if (!xs.length) return null;
    if (distance <= xs[0]) return ys[0];
    if (distance >= xs[xs.length - 1]) return ys[ys.length - 1];
    let low = 0;
    let high = xs.length - 1;
    while (high - low > 1) {
      const mid = (low + high) >> 1;
      if (xs[mid] <= distance) low = mid; else high = mid;
    }
    const span = xs[high] - xs[low];
    if (!(span > 0)) return ys[low];
    return ys[low] + (ys[high] - ys[low]) * ((distance - xs[low]) / span);
  }

  // =====================================================================
  // The plan, as the backend sees it
  // =====================================================================

  /** The current plan in the shape corvus/mission.py validates. */
  function toPlan() {
    const plan = {
      version: 1,
      name: planName || "Mission",
      items: items.map((item) => {
        const spec = TYPES[item.type];
        const out = { type: item.type };
        if (spec.position) {
          out.lat = item.lat;
          out.lon = item.lon;
          out.alt = item.alt;
        }
        Object.keys(spec.params).forEach((key) => { out[key] = item[key]; });
        // Only when the point pins one. Sending `null` would be a value the
        // backend has to refuse, and sending 0 would be PX4's "no change"
        // dressed up as a speed.
        if (item.speed != null) out.speed = item.speed;
        if (item.name) out.name = item.name;
        return out;
      }),
    };
    if (home) {
      plan.home = { lat: home.lat, lon: home.lon };
      if (home.elevation != null) plan.home.elevation = home.elevation;
    }
    if (planSpeed != null) plan.speed = planSpeed;
    return plan;
  }

  /** Replace the whole plan from a validated backend payload. */
  function fromPlan(plan) {
    items = [];
    nextId = 1;
    selectedId = null;
    planName = (plan && plan.name) || "Mission";
    planSpeed = (plan && typeof plan.speed === "number") ? plan.speed : null;
    home = (plan && plan.home)
      ? { lat: plan.home.lat, lon: plan.home.lon,
          elevation: plan.home.elevation != null ? plan.home.elevation : null }
      : null;
    homeElevation = home && home.elevation != null ? home.elevation : null;
    ((plan && plan.items) || []).forEach((raw) => {
      const spec = TYPES[raw.type];
      if (!spec) return;
      const item = { id: nextId++, type: raw.type };
      if (spec.position) {
        item.lat = raw.lat;
        item.lon = raw.lon;
        item.alt = raw.type === "land" ? 0 : clampNumber(raw.alt, ALT_MIN_M, ALT_MAX_M, DEFAULT_ALT_M);
      }
      Object.keys(spec.params).forEach((key) => {
        const rule = spec.params[key];
        item[key] = clampNumber(raw[key], rule.min, rule.max, rule.def);
      });
      // Absent stays absent: a point with no speed of its own inherits the
      // one in force, and clamping a missing field to a minimum would invent
      // a 0.5 m/s crawl out of nothing.
      item.speed = raw.speed == null
        ? null : clampNumber(raw.speed, SPEED_MIN_MS, SPEED_MAX_MS, SPEED_MIN_MS);
      item.name = cleanPointName(raw.name);
      snapDirection(item);
      items.push(item);
    });
  }

  /** A point's name as corvus/mission.py's clean_point_name stores it, or ""
   *  for none. Counted in code points, as Python counts, so a name that ends
   *  in an emoji is cut in the same place on both sides. */
  function cleanPointName(raw) {
    if (typeof raw !== "string") return "";
    const text = raw.replace(POINT_NAME_JUNK, " ").replace(/\s+/g, " ").trim();
    return Array.from(text).slice(0, POINT_NAME_MAX).join("").trim();
  }

  /** What a point is called on screen: its own name, else its kind. */
  function pointName(item) {
    return item.name || TYPES[item.type].label;
  }

  function escapeMarkup(text) {
    return String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  }

  // =====================================================================
  // Editing
  // =====================================================================

  function selectedItem() {
    return items.find((item) => item.id === selectedId) || null;
  }

  /** The altitude a new point should get: the one before it, so a route drawn
   *  left to right stays level unless the operator says otherwise. */
  function suggestedAltitude(before) {
    const from = before == null ? items.length : before;
    for (let i = from - 1; i >= 0; i -= 1) {
      const spec = TYPES[items[i].type];
      if (spec.position && items[i].type !== "land") return items[i].alt;
    }
    return DEFAULT_ALT_M;
  }

  /** Where the mission ends: the index of its first landing or return, or -1. */
  function endIndex() {
    return items.findIndex((item) => ENDS[item.type]);
  }

  /** The takeoff the start point carries: the first item, when it is a
   *  takeoff standing on the start. Null for a plan with no start, a plan for
   *  an aircraft already in the air, or an older file whose takeoff is
   *  somewhere else. */
  function startTakeoff() {
    const first = items[0];
    if (!home || !first || first.type !== "takeoff") return null;
    return distanceM(home, first) <= TIED_M ? first : null;
  }

  /** Why the tool *id* cannot be used right now, or null when it can. A tool
   *  that would draw a plan nobody can fly is greyed with the reason on it,
   *  rather than letting the operator build one and warning afterwards. */
  function toolBlocked(id) {
    if (id === "select" || id === "home") return null;
    if (!home) return "Set the start first. The mission begins there.";
    if (ENDS[id]) {
      const end = endIndex();
      if (end >= 0) {
        return `The mission already ends with ${END_NAMES[items[end].type]}. `
          + "Remove it to end the mission differently.";
      }
    }
    return null;
  }

  /** Add one item. It goes in before the mission's ending when there is one,
   *  so a point drawn after the landing is still flown, and the landing stays
   *  the last thing the aircraft does. *at* overrides that. */
  function addItem(type, lngLat, at) {
    if (items.length >= MAX_ITEMS) {
      notify("warning", `A mission can hold ${MAX_ITEMS} items.`);
      return null;
    }
    const spec = TYPES[type];
    if (!spec) return null;
    let index = at;
    if (index == null) {
      const end = endIndex();
      index = end >= 0 ? end : items.length;
    }
    const item = { id: nextId++, type };
    if (spec.position) {
      if (!lngLat) return null;
      item.lat = lngLat.lat;
      item.lon = lngLat.lng != null ? lngLat.lng : lngLat.lon;
      item.alt = type === "land" ? 0 : suggestedAltitude(index);
    }
    Object.keys(spec.params).forEach((key) => { item[key] = spec.params[key].def; });
    // No speed of its own: a new point is flown at whatever the plan, or the
    // point before it, already set.
    item.speed = null;
    item.name = "";
    snapDirection(item);
    items.splice(index, 0, item);
    selectedId = item.id;
    return item;
  }

  /** Put the start at *lngLat*, and the takeoff with it. A takeoff already
   *  standing on the start moves along; a plan with no takeoff at all gets
   *  one as its first item. A takeoff somewhere else (an older file) is left
   *  where it was drawn, since a second one would be a climb in mid air. */
  function placeStart(lngLat) {
    const lat = lngLat.lat;
    const lon = lngLat.lng != null ? lngLat.lng : lngLat.lon;
    const tied = startTakeoff();
    home = { lat, lon, elevation: null };
    homeElevation = null;
    if (tied) {
      tied.lat = lat;
      tied.lon = lon;
      selectedId = tied.id;
      return tied;
    }
    if (items.some((item) => item.type === "takeoff")) return null;
    const takeoff = addItem("takeoff", { lat, lon }, 0);
    if (takeoff) takeoff.alt = DEFAULT_ALT_M;
    return takeoff;
  }

  /** Take the start away, and the takeoff that stands on it. */
  function removeStart() {
    const tied = startTakeoff();
    if (tied) removeItem(tied.id);
    home = null;
    homeElevation = null;
  }

  /** The slots a moved item may land in. The start's takeoff stays first and
   *  the ending stays last, so dragging a row can reorder the route but never
   *  put a point before the climb or after the touchdown. */
  function slotRange() {
    const last = items.length - 1;
    return {
      min: startTakeoff() ? 1 : 0,
      max: last >= 0 && ENDS[items[last].type] && endIndex() === last ? last - 1 : last,
    };
  }

  /** Is the item at *index* held in place (the start's takeoff, the ending)? */
  function pinned(index) {
    const range = slotRange();
    return index < range.min || index > range.max;
  }

  /** Direction is one of two things, not a range: a 0 from a hand-edited file
   *  would otherwise fly out as a radius of zero once the sign is applied.
   *  Mirrors the same snap in corvus/mission.py. */
  function snapDirection(item) {
    if (ORBIT_TYPES.indexOf(item.type) === -1) return item;
    item.direction = Number(item.direction) >= 0 ? 1 : -1;
    return item;
  }

  /** Is *item* one of the orbiting kinds — the ones with a ring on the map? */
  function isOrbit(item) {
    return !!item && ORBIT_TYPES.indexOf(item.type) !== -1;
  }

  function removeItem(id) {
    const index = items.findIndex((item) => item.id === id);
    if (index < 0) return;
    items.splice(index, 1);
    if (selectedId === id) {
      const next = items[Math.min(index, items.length - 1)];
      selectedId = next ? next.id : null;
    }
  }

  function moveItem(id, delta) {
    const index = items.findIndex((item) => item.id === id);
    if (index < 0) return;
    moveItemTo(id, index + delta);
  }

  /** Lift one item out of the list and drop it at *target*, an index in the
   *  list as it will be afterwards. A target off either end is refused rather
   *  than clamped: both callers already know how long the list is, and a
   *  silent clamp would turn a chevron at the end into a move. */
  function moveItemTo(id, target) {
    const index = items.findIndex((item) => item.id === id);
    if (index < 0 || target === index) return;
    if (target < 0 || target >= items.length) return;
    if (pinned(index) || pinned(target)) return;
    const [moved] = items.splice(index, 1);
    items.splice(target, 0, moved);
  }

  /** Set one item's altitude, honouring the shared bounds. Land is excluded:
   *  a touchdown is on the ground by definition, and the backend would refuse
   *  anything else anyway. */
  function setAltitude(id, metres) {
    const item = items.find((entry) => entry.id === id);
    if (!item || item.type === "land" || !TYPES[item.type].position) return null;
    item.alt = Math.round(clampNumber(metres, ALT_MIN_M, ALT_MAX_M, item.alt) * 10) / 10;
    return item.alt;
  }

  /** Say something in the sidebar's status line, or say nothing at all.
   *  Every backend call in this module resolves after an await, by which time
   *  the operator may have left the page and taken the line with them. */
  function say(text, kind) {
    if (destroyed || !status) return;
    // Whatever is on the line now, it is no longer the "your plan is over
    // there" notice — so the pan that would clear that notice must not clear
    // somebody else's message instead.
    offscreenNotice = false;
    if (text == null) status.hide();
    else status.show(text, kind);
  }

  function notify(level, message) {
    try {
      window.dispatchEvent(new CustomEvent("corvus:notification", { detail: { level, message } }));
    } catch (_error) { /* notifications are never worth failing an edit for */ }
  }

  // =====================================================================
  // Rendering — the page
  // =====================================================================

  /* Entering the page.

     The planner is built ONCE and then kept. Leaving it SUSPENDS the page —
     its listeners, its subscriptions, its hold on the offline-map dialog —
     and coming back re-attaches the very same element, with the same MapLibre
     map, the same loaded style, the same tiles in it and the same decoded DEM
     behind the altitude profile.

     It used to be built and destroyed on every visit, and that cost most of a
     second of blank map each time: a GL context, a style, a dozen tile
     requests and a re-sampled terrain, all to show ground the operator was
     already looking at on the Home tab. The planner is a page you flip to and
     from constantly while drawing a route, so that was paid over and over.

     What resume() has to do instead is catch up with everything that could
     have changed while the page was away — its size, the base layer, and
     where the operator has since aimed the other map. */
  function render(container) {
    if (pageEl && map) { resume(container); return; }
    // Not a resume. Anything an earlier, half-built page left behind goes
    // before a new one is put up, the same idempotent entry Corvus.setup has.
    teardown();
    build(container);
  }

  function build(container) {
    destroyed = false;

    const page = document.createElement("div");
    page.className = "mission-page";

    const mapWrap = document.createElement("div");
    mapWrapEl = mapWrap;
    mapWrap.className = "mission-map-wrap";
    mapEl = document.createElement("div");
    mapEl.className = "mission-map";
    mapWrap.appendChild(mapEl);

    // The bar and the line under it are one stack, so the hint follows the bar
    // wherever a narrow map pushes it — a fixed offset would have it sitting
    // over the second row of a wrapped bar.
    const topLeft = document.createElement("div");
    topLeft.className = "mission-topleft";
    toolsEl = document.createElement("div");
    toolsEl.className = "flight-actions glass mission-tools";
    const hint = document.createElement("div");
    hint.className = "mission-hint";
    hint.id = "missionHint";
    topLeft.append(toolsEl, hint);
    mapWrap.appendChild(topLeft);

    const controls = document.createElement("div");
    controls.className = "map-controls mission-map-controls";
    mapWrap.appendChild(controls);

    page.appendChild(mapWrap);
    page.appendChild(buildSidebar());

    const profileWrap = document.createElement("div");
    profileWrap.className = "mission-profile";
    const profileHead = document.createElement("div");
    profileHead.className = "mission-profile-head";
    const profileTitle = document.createElement("span");
    profileTitle.className = "mission-profile-title";
    profileTitle.textContent = "ALTITUDE PROFILE";
    const profileNote = document.createElement("span");
    profileNote.className = "mission-profile-note";
    profileNote.id = "missionProfileNote";
    profileNote.textContent = "Drag a point up or down to set its height.";
    profileHead.append(profileTitle, profileNote);
    profileEl = document.createElement("div");
    profileEl.className = "mission-profile-plot";
    profileWrap.append(profileHead, profileEl);
    page.appendChild(profileWrap);

    pageEl = page;
    container.appendChild(page);

    buildTools();
    buildMapControls(controls);
    buildCornerControls(mapWrap);
    buildSearch(mapWrap);
    initMap();
    wireProfileDragging();
    arm();

    loadLastPlan();
    refreshAll();
    Corvus.ui.refreshIcons();
  }

  /** Coming back to a page that was only put down.
   *
   *  Everything expensive is still here, so this is only the catching up: the
   *  container had no size while the page was off screen, the base layer is
   *  chosen for the whole app on the Home map, and the two maps are never
   *  maps of two different places. A jump to a view this map is already at
   *  costs nothing and loads no tile, which is the whole point of keeping it.
   */
  function resume(container) {
    container.appendChild(pageEl);
    destroyed = false;
    arm();
    map.resize();
    fitTools();
    resizeProfile();
    const layer = baseLayerId();
    if (layer !== activeLayerId) applyBaseLayer(layer);
    const opening = openingView();
    if (opening && Array.isArray(opening.center)) {
      map.jumpTo({
        center: opening.center,
        zoom: opening.zoom,
        bearing: opening.bearing || 0,
      });
    }
    // The plan is whatever the operator left on screen, unsaved edits and
    // all. loadLastPlan() is a first-entry thing: re-reading yesterday's file
    // here would throw away the route they walked away from for ten seconds.
    refreshAll();
    notePlanOffscreen();
    Corvus.ui.refreshIcons();
  }

  /** Wire the page to the things outside it: the window's size, the map
   *  column's size, the theme, the airframe and the keyboard. Called on every
   *  entry and undone by disarm() on every exit — a page nobody is looking at
   *  must not be redrawing itself behind the one that is. */
  function arm() {
    resizeHandler = () => {
      if (map) map.resize();
      fitTools();
      drawProfile();
    };
    window.addEventListener("resize", resizeHandler);
    // The map column changes width without the window doing anything — the
    // right panel opens, the left rail collapses — and the bar has to answer
    // to the box it is in rather than to the window it is in.
    if (typeof ResizeObserver === "function" && mapWrapEl) {
      toolsObserver = new ResizeObserver(() => fitTools());
      toolsObserver.observe(mapWrapEl);
    }
    // Plotly holds resolved colour strings, so a theme switch has to redraw
    // the chart — the same contract every other chart in the app is under.
    // A theme changed while the page was suspended is caught by the redraw
    // resume() does anyway.
    themeUnsub = Corvus.ui.onThemeChange(() => drawProfile());
    // The airframe decides whether a loiter radius is a thing that gets flown,
    // and it can arrive, change or go away while the page is open. Only a
    // CHANGE redraws: the state itself lands on every telemetry frame.
    hovers = vehicleHovers();
    if (Corvus.telemetry && typeof Corvus.telemetry.getState === "function") {
      onVehicleMission(Corvus.telemetry.getState());
    }
    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      vehicleUnsub = Corvus.telemetry.subscribe((frame) => {
        if (destroyed) return;
        onVehicleMission(frame);
        const next = vehicleHovers();
        if (next === hovers) return;
        hovers = next;
        refreshAll();
      });
    }
    // On the DOCUMENT so Escape leaves a placement tool from anywhere on the
    // page — which is also why it cannot stay on while another page is up.
    document.addEventListener("keydown", onKey, true);
  }

  /** arm()'s mirror. Shared by suspend and teardown. */
  function disarm() {
    if (resizeHandler) window.removeEventListener("resize", resizeHandler);
    resizeHandler = null;
    if (toolsObserver) { toolsObserver.disconnect(); toolsObserver = null; }
    if (typeof themeUnsub === "function") themeUnsub();
    themeUnsub = null;
    if (typeof vehicleUnsub === "function") vehicleUnsub();
    vehicleUnsub = null;
    document.removeEventListener("keydown", onKey, true);
  }

  /** Which base layer the app is on. Chosen once, on the Home map, for both
   *  of them. */
  function baseLayerId() {
    return (Corvus.map && typeof Corvus.map.getBaseLayer === "function")
      ? (Corvus.map.getBaseLayer() || "satellite") : "satellite";
  }

  /** Tell Plotly the chart has a size again: its own responsive handler had
   *  nothing to measure while the page was off screen. Before the redraw,
   *  never after — the geometry the altitude drag hit-tests against is read
   *  back out of the drawn chart. */
  function resizeProfile() {
    if (!profileEl || !window.Plotly || !window.Plotly.Plots) return;
    try { window.Plotly.Plots.resize(profileEl); } catch (_error) { /* never drawn */ }
  }

  /** Leaving the page.
   *
   *  The DOM, the map, the markers and the decoded terrain all stay; what goes
   *  is everything that would go on acting on a page the operator is not
   *  looking at, and everything transient that must not come back mid-gesture
   *  when they return. resume() puts it all back. */
  function suspend() {
    destroyed = true;
    disarm();
    if (layerMenuHandle) { layerMenuHandle.close(false); layerMenuHandle = null; }
    // Two gestures that the page is being taken out from under.
    endDrag();
    endRowDrag();
    // The box registers a capture listener on the DOCUMENT while it is open,
    // for the same reason the key handler in disarm() has to come off.
    if (searchBox && typeof searchBox.setOpen === "function") searchBox.setOpen(false);
    aiming = false;
    offscreenNotice = false;
    // A terrain sample that lands after this belongs to the page as it was.
    groundToken += 1;
    // The dialog frames and downloads whatever map is on screen, and that is
    // the Home map again.
    if (Corvus.tiles && typeof Corvus.tiles.useMap === "function") Corvus.tiles.useMap(null);
  }

  /** Destroying the page for good: the GL context, the chart, the markers and
   *  the cached terrain all go with it.
   *
   *  A nav-away is a suspend, not this. What is left for teardown is the page
   *  leaving the app altogether — Settings turning the planner off — and the
   *  rebuild after a half-built entry. It starts by suspending, so there is
   *  one list of what has to be let go of and not two. */
  function teardown() {
    suspend();
    pendingLayer = null;
    controlsEl = null;
    tilesTriggerEl = null;
    clearPlanEl = null;
    // The box owns its pending lookup, its pin and a capture listener on the
    // DOCUMENT; suspend only closes it, and a page that is not coming back
    // has no use for the rest.
    if (searchBox) { searchBox.destroy(); searchBox = null; }
    markers.forEach((marker) => marker.remove());
    markers = [];
    if (homeMarker) { homeMarker.remove(); homeMarker = null; }
    if (radiusHandle) { radiusHandle.remove(); radiusHandle = null; }
    regionMarkers.forEach((marker) => marker.remove());
    regionMarkers = [];
    if (profileEl && window.Plotly && typeof window.Plotly.purge === "function") {
      try { window.Plotly.purge(profileEl); } catch (_error) { /* never drawn */ }
    }
    if (map) {
      try { map.remove(); } catch (_error) { /* already gone */ }
      map = null;
    }
    mapReady = false;
    routeSource = null;
    orbitSource = null;
    legSource = null;
    vehicleEl = null;
    vehicleSig = "";
    regionSource = null;
    profileGeom = null;
    terrainTiles = new Map();
    ground = null;
    if (pageEl && pageEl.remove) pageEl.remove();
    pageEl = null;
    mapWrapEl = null;
    mapEl = profileEl = listEl = detailEl = summaryEl = issuesEl = toolsEl = null;
    listHeadEl = sideScrollEl = null;
    status = null;
  }

  // ---- the tool rail ---------------------------------------------------

  /* The tool bar, built as the Home tab's flight bar — .flight-actions with
     .fa-btn inside it, icon over caption, laid out across the top of the map.
     It was a vertical icon rail before, which made the planner's primary
     control set a different kind of object from the primary control set on
     the screen next to it, and left every tool unlabelled.

     The zoom rail on the right stays a rail, because that is what it is on the
     Home tab too: chrome for looking, against the edge, while what you ACT
     with runs across the top. */
  function buildTools() {
    Corvus.ui.clear(toolsEl);
    TOOLS.forEach((entry) => {
      if (entry.id.indexOf("divider") === 0) {
        const divider = document.createElement("div");
        divider.className = "fa-divider";
        toolsEl.appendChild(divider);
        return;
      }
      const button = document.createElement("button");
      button.type = "button";
      button.className = "fa-btn";
      button.title = entry.title;
      button.dataset.tool = entry.id;
      if (entry.id === tool) button.classList.add("active");
      button.appendChild(Corvus.ui.icon(entry.icon, 17));
      const caption = document.createElement("span");
      caption.textContent = entry.label;
      button.appendChild(caption);
      toolsEl.appendChild(button);
    });
    updateTools();
    fitTools();
    toolsEl.addEventListener("click", (event) => {
      const button = event.target.closest(".fa-btn");
      if (!button) return;
      event.preventDefault();
      // A greyed tool still answers a press, with the reason it is greyed:
      // a button that silently does nothing reads as a broken one.
      const blocked = toolBlocked(button.dataset.tool);
      if (blocked) {
        setHint(blocked);
        return;
      }
      // RTL names no place on the map, so arming a tool for it would be a mode
      // the operator could never leave by clicking. It appends immediately.
      if (button.dataset.tool === "rtl") {
        addItem("rtl", null);
        setTool("select");
        refreshAll();
        return;
      }
      setTool(button.dataset.tool);
    });
  }

  /** Grey the tools that cannot be used on the plan as it stands, with the
   *  reason as the tooltip, and mark START while the plan still needs one. */
  function updateTools() {
    if (!toolsEl) return;
    const titles = {};
    TOOLS.forEach((entry) => { titles[entry.id] = entry.title; });
    toolsEl.querySelectorAll(".fa-btn").forEach((button) => {
      const id = button.dataset.tool;
      const blocked = toolBlocked(id);
      button.classList.toggle("is-blocked", !!blocked);
      button.setAttribute("aria-disabled", blocked ? "true" : "false");
      button.title = blocked || titles[id] || "";
      button.classList.toggle("is-next", id === "home" && !home);
    });
  }

  /**
   * Keep the tool bar on ONE ROW, the way the Home flight bar is.
   *
   * The captions come off only when the row will not otherwise fit, and the
   * test is a MEASUREMENT of the box the bar actually has — not a viewport
   * media query, which was the old rule and asked the wrong question: the
   * bar's room is the map column, and that changes when the operator opens the
   * right-hand panel without the window moving a pixel.
   *
   * The class is cleared before measuring, because a compact bar measures
   * narrow and would otherwise never expand again once it had shrunk once.
   */
  function fitTools() {
    if (!toolsEl || !toolsEl.parentNode) return;
    // Corvus.ui.fitBar, which the Home flight bar is fitted by too: the two
    // bars are the same control set on two screens and they have to narrow
    // the same way, which they cannot be relied on to do from two copies of
    // the rule. The room is .mission-topleft's, not the bar's own — this bar
    // has no width cap of its own because its parent IS the cap.
    Corvus.ui.fitBar(toolsEl, toolsEl.parentNode.clientWidth);
  }

  function setTool(next) {
    tool = next || "select";
    if (toolBlocked(tool)) tool = "select";
    if (!toolsEl) return;
    toolsEl.querySelectorAll(".fa-btn").forEach((button) => {
      button.classList.toggle("active", button.dataset.tool === tool);
    });
    if (mapEl) mapEl.classList.toggle("is-placing", tool !== "select");
    updateHint();
  }

  function setHint(text) {
    const hint = document.getElementById("missionHint");
    if (hint) hint.textContent = text;
  }

  /** The line under the tool bar: what the next step is, in the order a
   *  mission is drawn in. Start, then the route, then how it ends. */
  function updateHint() {
    if (tool === "home") {
      setHint(home
        ? "Click the map to move the start. The aircraft takes off there."
        : "Click the map where the aircraft takes off.");
      return;
    }
    if (tool !== "select") {
      const end = endIndex();
      const before = end >= 0 && !ENDS[tool] ? ` before ${END_NAMES[items[end].type]}` : "";
      setHint(`Click the map to add a ${TYPES[tool].label.toLowerCase()}${before}. Esc to stop.`);
      return;
    }
    if (!home) {
      setHint("Press START and click where the aircraft takes off.");
    } else if (!items.some((item) => TYPES[item.type].position && item.type !== "takeoff")) {
      setHint("Now add points with POINT, CIRCLE or HOLD, then end with LAND or RETURN.");
    } else if (endIndex() < 0) {
      setHint("End the mission with LAND or RETURN. Drag a point to move it, right-click to delete it.");
    } else {
      setHint("Drag a point to move it, right-click to delete it. New points go in before the end.");
    }
  }

  /* The rail, which IS the Home map's rail: the same buttons, in the same
     order, at the same distance from the same edge. `regions` acts on the same
     store the Home one does — the tiles live in the backend's .mbtiles and are
     served to both maps through one proxy, so an area pulled down while
     drawing a mission is an area the Home map already has.

     The two controls that are NOT here are not missing: the offline download
     and the trash have their own places on the Home map, and they have the
     same places here. See buildCornerControls. */
  const MAP_CONTROLS = [
    { id: "in", icon: "plus", title: "Zoom in" },
    { id: "out", icon: "minus", title: "Zoom out" },
    { id: "fit", icon: "scan", title: "Fit the whole mission" },
    { id: "divider" },
    { id: "vehicle", icon: "crosshair", title: "Go to the aircraft" },
    { id: "layers", icon: "layers", title: "Map layer" },
    { id: "regions", icon: "frame", title: "Show downloaded areas", active: true },
  ];

  function buildMapControls(container) {
    controlsEl = container;
    const controls = MAP_CONTROLS;
    controls.forEach((entry) => {
      if (entry.id.indexOf("divider") === 0) {
        const divider = document.createElement("div");
        divider.className = "mc-divider";
        container.appendChild(divider);
        return;
      }
      const button = Corvus.ui.iconButton(entry.icon, {
        className: "mc-btn", title: entry.title, size: 17,
        variant: entry.active ? "active" : undefined,
      });
      button.dataset.act = entry.id;
      container.appendChild(button);
    });
    container.addEventListener("click", (event) => {
      const button = event.target.closest(".mc-btn");
      if (!button || !map) return;
      event.preventDefault();
      const act = button.dataset.act;
      if (act === "in") map.zoomIn();
      else if (act === "out") map.zoomOut();
      else if (act === "fit") fitMission();
      else if (act === "vehicle") goToVehicle();
      else if (act === "layers") openLayerMenu(button);
      else if (act === "regions") {
        button.classList.toggle("active", setRegionsVisible(!regionsVisible));
      }
    });
  }

  /* The two corner controls, in the corners the Home map puts them in.

     They are not in the rail on purpose, and not out of taste: an operator who
     has learned where the download button is on one map has learned it for
     both, and a control that moves between two screens of the same
     application is a control that has to be looked for twice.

     * DOWNLOAD — .tiles-trigger, top right, immediately left of the zoom rail.
       The same class as the Home map's, so it is the same glass square in the
       same place rather than something that resembles it.
     * CLEAR — .track-clear, bottom left above the attribution: small and faint
       until the pointer is on it, and absent entirely while there is nothing
       to throw away. On the Home map it clears the FLOWN track; here it clears
       the DRAWN route. Same gesture, same corner, same weight — the thing on
       the map that this page's work would be lost with. */
  function buildCornerControls(mapWrap) {
    tilesTriggerEl = Corvus.ui.iconButton("download", {
      className: "tiles-trigger",
      title: "Download offline map",
      ariaLabel: "Download offline map",
      size: 17,
      onClick: () => openTileDialog(),
    });
    mapWrap.appendChild(tilesTriggerEl);

    clearPlanEl = Corvus.ui.iconButton("trash-2", {
      className: "track-clear mission-plan-clear",
      title: "Delete the planned route",
      ariaLabel: "Delete the planned route",
      size: 12,
      onClick: () => confirmClear(),
    });
    clearPlanEl.hidden = true;
    mapWrap.appendChild(clearPlanEl);
    updateClearButton();
  }

  /* The offline-map dialog, pointed at THIS map.
     Corvus.tiles frames its download on whichever map it has been handed, and
     draws what comes back on every map that can draw it — so the rectangle
     appears here and on the Home map at the same moment, which is the truth:
     there is one cache underneath both. */
  function openTileDialog() {
    if (!Corvus.tiles || typeof Corvus.tiles.open !== "function") return;
    if (typeof Corvus.tiles.useMap === "function") Corvus.tiles.useMap(tileHost());
    // toggle, not open: the Home trigger is a press-to-close too, and a button
    // that only ever opens a dialog that is already open does nothing.
    if (typeof Corvus.tiles.isOpen === "function" && Corvus.tiles.isOpen()) Corvus.tiles.close();
    else Corvus.tiles.open(tilesTriggerEl);
  }

  /** What Corvus.tiles needs of a map: where it is looking, how to frame an
   *  area on it, what imagery it is showing, and where to draw the areas. */
  function tileHost() {
    return {
      getMap: () => map,
      getBaseLayer: () => activeLayerId,
      fitBounds: fitRegionBounds,
      setRegions: setRegions,
    };
  }

  /** The trash exists only while there is a route to delete — the same rule
   *  the Home map's clear-track button follows, and the reason neither is a
   *  permanently lit button for an action taken once. */
  function updateClearButton() {
    if (!clearPlanEl) return;
    clearPlanEl.hidden = !items.length && !home;
  }

  // ---- the sidebar -----------------------------------------------------

  function buildSidebar() {
    const side = document.createElement("aside");
    side.className = "mission-side";

    const nameInput = Corvus.ui.input({
      id: "missionName",
      value: planName,
      placeholder: "Mission",
      ariaLabel: "Mission name",
      autocomplete: false,
      onChange: (value) => { planName = value.trim() || "Mission"; },
    });
    /* Cruise speed. Empty is the ABSENCE of the setting, not zero: corvus/
       mission.py emits a DO_CHANGE_SPEED only when the plan pins one, and 0 is
       a real PX4 value meaning "no change". Without this field a speed could
       only ever arrive by opening a file that already carried one — and it
       still quietly drove the duration estimate. */
    const speedInput = Corvus.ui.input({
      id: "missionSpeed",
      type: "number",
      value: planSpeed == null ? "" : planSpeed,
      placeholder: "default",
      min: SPEED_MIN_MS, max: SPEED_MAX_MS, step: 0.5,
      mono: true,
      ariaLabel: "Cruise speed in metres per second",
      autocomplete: false,
      onChange: (value) => {
        const text = String(value).trim();
        planSpeed = text ? clampNumber(text, SPEED_MIN_MS, SPEED_MAX_MS, SPEED_MIN_MS) : null;
        speedInput.value = planSpeed == null ? "" : planSpeed;
        renderSummary();
        // The selected point's own speed box shows what it INHERITS as its
        // placeholder, and this is what it inherits.
        renderDetail();
      },
    });

    // Everything above the point editor scrolls as one. The list used to be
    // the only part that scrolled, squeezed between a fixed header block and
    // the editor, and showed three rows of a plan with thirty.
    sideScrollEl = document.createElement("div");
    sideScrollEl.className = "mission-side-scroll";
    side.appendChild(sideScrollEl);

    const planRow = document.createElement("div");
    planRow.className = "mission-plan-row";
    planRow.appendChild(Corvus.ui.field({ label: "Name", control: nameInput }));
    // "Start" because a point further down the plan may raise or lower it;
    // this is the speed the mission begins at.
    planRow.appendChild(Corvus.ui.field({ label: "Start speed (m/s)", control: speedInput }));
    sideScrollEl.appendChild(planRow);

    summaryEl = document.createElement("div");
    summaryEl.className = "mission-summary";
    sideScrollEl.appendChild(summaryEl);

    // The vehicle's side of it: which leg is being flown, or that the
    // aircraft holds a mission that is not the one on screen.
    vehicleEl = document.createElement("div");
    vehicleEl.className = "mission-vehicle";
    vehicleEl.hidden = true;
    sideScrollEl.appendChild(vehicleEl);

    // What is wrong with the plan, while it is being drawn rather than at the
    // moment of upload. These used to appear only in the confirm dialog, which
    // is the last place a route still worth changing gets read.
    issuesEl = document.createElement("div");
    issuesEl.className = "mission-issues";
    issuesEl.hidden = true;
    sideScrollEl.appendChild(issuesEl);

    listHeadEl = document.createElement("div");
    listHeadEl.className = "mission-list-head";
    const listTitle = document.createElement("span");
    listTitle.textContent = "ITEMS";
    listHeadEl.appendChild(listTitle);
    listHeadEl.appendChild(Corvus.ui.iconButton("trash-2", {
      title: "Clear the whole plan",
      ariaLabel: "Clear the whole plan",
      onClick: () => confirmClear(),
    }));
    sideScrollEl.appendChild(listHeadEl);

    listEl = document.createElement("div");
    listEl.className = "mission-list";
    sideScrollEl.appendChild(listEl);

    detailEl = document.createElement("div");
    detailEl.className = "mission-detail";
    side.appendChild(detailEl);

    status = Corvus.ui.message({ className: "mission-status" });
    side.appendChild(status.el);
    side.appendChild(buildActions());
    return side;
  }

  function buildActions() {
    const wrap = document.createElement("div");
    wrap.className = "mission-actions";

    const upload = Corvus.ui.button({
      variant: "primary", shape: "block", icon: "upload",
      label: "Upload to vehicle",
      onClick: () => uploadPlan(false, upload),
    });
    const fly = Corvus.ui.button({
      variant: "secondary", shape: "block", icon: "play",
      label: "Upload and fly",
      onClick: () => uploadPlan(true, fly),
    });
    wrap.append(upload, fly);

    const fileRow = document.createElement("div");
    fileRow.className = "mission-file-row";
    const read = Corvus.ui.button({
      variant: "secondary", size: "sm", icon: "download", label: "From vehicle",
      className: "mission-read", onClick: () => readFromVehicle(read),
    });
    read.title = "Read the mission the vehicle holds into the planner";
    fileRow.append(
      Corvus.ui.button({ variant: "secondary", size: "sm", icon: "save", label: "Save", onClick: () => savePlan() }),
      Corvus.ui.button({ variant: "secondary", size: "sm", icon: "folder-open", label: "Open", onClick: () => openPlanDialog() }),
      read,
      Corvus.ui.button({ variant: "ghost", size: "sm", icon: "file-plus", label: "New", onClick: () => confirmClear() }),
    );
    wrap.appendChild(fileRow);
    return wrap;
  }

  // =====================================================================
  // The map
  // =====================================================================

  function initMap() {
    const layer = baseLayerId();
    activeLayerId = layer;
    const opening = openingView();
    map = new maplibregl.Map({
      container: mapEl,
      center: opening.center,
      zoom: opening.zoom,
      bearing: opening.bearing || 0,
      style: rasterStyle(layer, layerSpec(layer)),
      attributionControl: false,
      keyboard: false,
    });
    // The same credit the Home map shows, on the same argument — map.js owns
    // it (see ATTRIBUTION_OPTIONS there). The fallback is not a second copy of
    // that argument, only the empty list that keeps MapLibre's default credit
    // out of a build where map.js did not load.
    if (Corvus.map && typeof Corvus.map.addAttribution === "function") {
      Corvus.map.addAttribution(map);
    } else {
      map.addControl(new maplibregl.AttributionControl(
        { compact: true, customAttribution: [] }), "bottom-left");
    }

    map.on("load", () => {
      if (destroyed) return;
      addMissionLayers();
      mapReady = true;
      if (pendingLayer) applyBaseLayer(pendingLayer.layerId, pendingLayer.spec);
      applyPointScale();
      setRegions(regions);
      loadRegions();
      drawMap();
      // NOT a fit any more. The map opens on the view the operator last
      // aimed (openingView), and framing the plan over the top of that would
      // undo it — which is how re-entering the planner used to throw away
      // where you had been looking, and how it came to be showing different
      // ground from the Home map. A plan that is nowhere on screen is said
      // out loud instead.
      notePlanOffscreen();
    });
    // A move the OPERATOR started is them choosing where this map looks, and
    // that choice is what the planner reopens on. Keyed on the move carrying
    // an originalEvent, exactly as the Home map's follow-break is: our own
    // easeTo/fitBounds and the rail's zoom buttons carry none, so a fit to
    // the plan never counts as an aim. A place the operator SEARCHED for
    // does — it is also programmatic, and so it is reported by the search
    // box's onGo rather than seen here.
    map.on("movestart", (event) => { aiming = !!(event && event.originalEvent); });
    map.on("moveend", () => {
      clearOffscreenNotice();
      if (!aiming) return;
      aiming = false;
      noteAim();
    });
    map.on("zoom", applyPointScale);
    map.on("click", onMapClick);
    // A right-click on EMPTY map removes the last thing placed — the undo of
    // the click that placed it, and the same gesture the Home tab's
    // fly-to-points planner uses. A right-click on a point deletes that point
    // instead and never reaches here (the marker stops it), because a gesture
    // aimed at something has to act on the thing it was aimed at.
    // "Last placed" is the newest item, not the last in the list: a point
    // drawn after the landing goes in before it, and undoing that point must
    // not take the landing away instead.
    map.on("contextmenu", (event) => {
      if (event && event.preventDefault) event.preventDefault();
      if (!items.length) return;
      const newest = items.reduce((best, item) => (item.id > best.id ? item : best));
      if (newest === startTakeoff()) removeStart();
      else removeItem(newest.id);
      refreshAll();
    });

    // The base layer the operator chose in Settings, once its catalogue has
    // landed. Best effort: a failed fetch leaves the bootstrap style, which is
    // a working attributed map — this page must open offline like every other.
    // The DEM the altitude profile reads its ground from. Best effort, and
    // only the terrain half: the base-layer catalogue is hydrated once by
    // map.js and read back through layerSpec(), so the descriptor this map
    // needs is re-applied below rather than fetched a second time.
    Corvus.telemetry.requestJson("/api/tiles/sources").then((data) => {
      if (destroyed || !map) return;
      terrainSpec = ((data && data.terrain) || [])[0] || null;
      sampleGround();
      // The catalogue has certainly landed by now, so the layer opened on the
      // bootstrap maxzoom and attribution can be re-applied with its real ones.
      applyBaseLayer(activeLayerId);
    }).catch(() => {});
  }

  function onKey(event) {
    if (destroyed || !map) return;
    // A dialog owns the keyboard while it is open. Its own handler is
    // registered on the document AFTER this one and in the same phase, so
    // without this guard Escape would disarm the tool behind it as well.
    if (document.querySelector(".modal-overlay")) return;
    // A row in the air owns Escape before any tool does: letting go of it is
    // the only way out of a drag that leaves the plan as it was.
    if (event.key === "Escape" && rowDrag) {
      event.preventDefault();
      onRowPointerCancel();
      return;
    }
    if (event.key === "Escape" && searchBox && searchBox.isOpen()) {
      // The search box owns Escape while it is open — the input's own handler
      // takes it when the caret is in the field, and this is the case where
      // the operator opened the box and then clicked the map.
      event.preventDefault();
      searchBox.setOpen(false);
      return;
    }
    if (event.key === "Escape" && tool !== "select") {
      event.preventDefault();
      setTool("select");
      return;
    }
    if (event.key === "Delete" || event.key === "Backspace") {
      if (selectedId == null || isTypingTarget(event.target)) return;
      // Backspace navigates back in some browsers when nothing has focus,
      // which with a map on screen looks exactly like the app crashing.
      event.preventDefault();
      removeItem(selectedId);
      refreshAll();
    }
  }

  /** Is the keyboard inside something that takes text? */
  function isTypingTarget(node) {
    const el = node && node.nodeType === 1 ? node : null;
    if (!el) return false;
    const tag = (el.tagName || "").toLowerCase();
    return tag === "input" || tag === "textarea" || tag === "select" ||
      el.isContentEditable === true;
  }

  /** Where this map opens.
   *
   *  The Home map owns that answer (see "The view the two maps share" in
   *  map.js): the planner opens on the view the operator last aimed, on
   *  either map, so the two screens are never maps of two different places.
   *  What is left here is only the fallback for when there is no Home map to
   *  ask — a test, or a build where map.js failed to load — and it is the old
   *  rule: the aircraft if it has a fix, else the operating site.
   */
  function openingView() {
    const shared = (Corvus.map && typeof Corvus.map.missionOpenView === "function")
      ? Corvus.map.missionOpenView() : null;
    if (shared && Array.isArray(shared.center)) return shared;
    const state = Corvus.telemetry && Corvus.telemetry.getState();
    const centre = (state && state.position && state.position[0] && state.position[1])
      ? [state.position[0], state.position[1]]
      : DEFAULT_CENTER;
    return { center: centre, zoom: FALLBACK_ZOOM, bearing: 0 };
  }

  /** Tell the Home map where the operator left this one, so leaving the page
   *  and coming back returns to it. Only ever called for a move the operator
   *  started — see the movestart handler in initMap. */
  function noteAim() {
    if (!map || !Corvus.map || typeof Corvus.map.noteMissionView !== "function") return;
    const centre = map.getCenter();
    if (!centre) return;
    Corvus.map.noteMissionView({
      center: [centre.lng, centre.lat],
      zoom: map.getZoom(),
      bearing: map.getBearing(),
    });
  }

  /** A one-raster style pointed at the backend tile proxy, so this map is as
   *  offline-capable as the Home one — the browser never reaches upstream. */
  /** One layer's descriptor, out of the catalogue map.js already fetched.
   *  There is exactly one of those in the app — the Home map hydrates it from
   *  /api/tiles/sources — and a second copy here would be a second thing to
   *  keep in step with corvus/tile_sources.py. */
  function layerSpec(layerId) {
    return (Corvus.map && typeof Corvus.map.layerSpec === "function")
      ? Corvus.map.layerSpec(layerId) : null;
  }

  function rasterStyle(layerId, spec) {
    return {
      version: 8,
      sources: {
        base: {
          type: "raster",
          tiles: [`/api/tiles/${layerId}/{z}/{x}/{y}.png`],
          tileSize: 256,
          maxzoom: (spec && spec.maxzoom) || 19,
          attribution: (spec && spec.attribution) || "",
        },
      },
      layers: [{ id: "base", type: "raster", source: "base" }],
    };
  }

  /* Swap the base raster. `spec` is optional — the layer switcher picks a
     layer by id alone, and the catalogue that describes it lives in map.js,
     so the maxzoom and the attribution are read back from there rather than
     carried through every call. */
  function applyBaseLayer(layerId, spec) {
    const descriptor = spec || layerSpec(layerId);
    activeLayerId = layerId;
    if (!map) return;
    // Before the style is up there is nothing to swap, and once("load") would
    // never fire if load had already happened — so the choice is remembered
    // and the load handler applies it.
    if (!mapReady) { pendingLayer = { layerId, spec: descriptor }; return; }
    pendingLayer = null;
    if (map.getLayer("base")) map.removeLayer("base");
    if (map.getSource("base")) map.removeSource("base");
    map.addSource("base", {
      type: "raster",
      tiles: [`/api/tiles/${layerId}/{z}/{x}/{y}.png`],
      tileSize: 256,
      maxzoom: (descriptor && descriptor.maxzoom) || 19,
      attribution: (descriptor && descriptor.attribution) || "",
    });
    // Under everything drawn ON it, so the plan AND the downloaded-area
    // rectangles survive a layer switch — the same insertion rule, and the
    // same bug it fixes on the Home map: re-inserting the imagery above the
    // region overlay made the downloaded areas vanish on exactly the action an
    // operator takes while deciding what still needs downloading.
    map.addLayer({ id: "base", type: "raster", source: "base" }, firstOverlayLayer());
  }

  /* Every overlay this map stacks on the imagery, bottom to top. The regions
     are first because knowing where your tiles are must never cover the route
     you are drawing. */
  const OVERLAY_LAYERS = [
    "mission-regions-fill", "mission-regions-line",
    "mission-orbit-fill", "mission-orbit-casing", "mission-orbit-line",
    "mission-route-casing", "mission-route-line",
  ];

  function firstOverlayLayer() {
    return OVERLAY_LAYERS.find((id) => map.getLayer(id));
  }

  /* The layer switcher, built by map.js for this rail.
     It used to be a second, thinner copy here: no service headings, no
     persistence, and a fresh menu on every press. Corvus.map.createLayerMenu
     is the one definition of what that control is, so the Home rail and this
     one are the same list with the same behaviour by construction rather than
     by two files agreeing. */
  function layerMenu() {
    if (layerMenuHandle) return layerMenuHandle;
    layerMenuHandle = Corvus.map.createLayerMenu({
      rail: () => controlsEl,
      active: () => activeLayerId,
      onSelect: (id) => {
        applyBaseLayer(id);
        // The base layer is ONE setting with two views of it. The switcher
        // writes it to the config, so the Home map would come back on the new
        // layer after a restart — telling it now is what stops the operator
        // walking back to HOME and finding the imagery they just changed.
        if (Corvus.map.getBaseLayer() !== id) Corvus.map.setBaseLayer(id);
      },
    });
    return layerMenuHandle;
  }

  function openLayerMenu(anchor) {
    if (layerMenu().isOpen()) layerMenu().close(false);
    else layerMenu().open({ el: anchor });
  }

  function addMissionLayers() {
    addRegionLayers();
    map.addSource("mission-orbit", { type: "geojson", data: emptyCollection() });
    map.addLayer({
      id: "mission-orbit-fill",
      source: "mission-orbit",
      type: "fill",
      paint: { "fill-color": planColor(), "fill-opacity": 0.24 },
    });
    map.addLayer({
      id: "mission-orbit-casing",
      source: "mission-orbit",
      type: "line",
      layout: { "line-join": "round" },
      paint: { "line-color": planInk(), "line-width": widthByZoom(ORBIT_CASING_W) },
    });
    map.addLayer({
      id: "mission-orbit-line",
      source: "mission-orbit",
      type: "line",
      layout: { "line-cap": "butt", "line-join": "round" },
      paint: {
        "line-color": planColor(),
        "line-width": widthByZoom(ORBIT_LINE_W),
        "line-dasharray": PLAN_DASH,
      },
    });

    map.addSource("mission-route", { type: "geojson", data: emptyLine() });
    map.addLayer({
      id: "mission-route-casing",
      source: "mission-route",
      type: "line",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": planInk(), "line-width": widthByZoom(PLAN_CASING_W) },
    });
    map.addLayer({
      id: "mission-route-line",
      source: "mission-route",
      type: "line",
      layout: { "line-cap": "butt", "line-join": "round" },
      paint: {
        "line-color": planColor(),
        "line-width": widthByZoom(PLAN_LINE_W),
        "line-dasharray": PLAN_DASH,
      },
    });
    routeSource = map.getSource("mission-route");
    orbitSource = map.getSource("mission-orbit");

    // The leg being flown, over the route, in the colour the app uses for
    // "going well". Empty unless the vehicle is flying THIS plan.
    map.addSource("mission-leg", { type: "geojson", data: emptyLine() });
    map.addLayer({
      id: "mission-leg-line",
      source: "mission-leg",
      type: "line",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: {
        "line-color": token("--healthy", "#45D483"),
        "line-width": widthByZoom(PLAN_LINE_W + 1),
      },
    });
    legSource = map.getSource("mission-leg");
    drawLeg();
  }

  /* What is already on the disk, drawn where it is.
     The same rectangles the Home map draws, in the same colour, from the same
     /api/tiles/regions list — a mission planned inside one of these boxes is a
     mission that can be flown with the laptop offline, and that is a thing to
     be able to SEE while drawing it rather than to find out in the field. */
  function addRegionLayers() {
    map.addSource("mission-regions", { type: "geojson", data: emptyCollection() });
    map.addLayer({
      id: "mission-regions-fill",
      source: "mission-regions",
      type: "fill",
      paint: { "fill-color": "#4CC9FF", "fill-opacity": 0.06 },
    });
    map.addLayer({
      id: "mission-regions-line",
      source: "mission-regions",
      type: "line",
      layout: { "line-cap": "square", "line-join": "miter" },
      paint: {
        "line-color": "#4CC9FF",
        "line-width": 1.4,
        "line-opacity": 0.85,
        "line-dasharray": [4, 3],
      },
    });
    regionSource = map.getSource("mission-regions");
  }

  /** Closed [lng,lat] ring for a {w,s,e,n} bounds. */
  function boundsRing(bounds) {
    const b = bounds || {};
    return [[[b.w, b.s], [b.e, b.s], [b.e, b.n], [b.w, b.n], [b.w, b.s]]];
  }

  function buildRegionLabel(region) {
    const element = document.createElement("div");
    element.className = "region-label" + (region.state === "running" ? " downloading" : "");
    const name = document.createElement("span");
    name.className = "region-label-name";
    name.textContent = region.name || "Region";
    const meta = document.createElement("span");
    meta.className = "region-label-meta";
    meta.textContent = region.state === "running"
      ? "downloading\u2026"
      : `z${region.minzoom} to ${region.maxzoom}`;
    element.append(name, meta);
    return element;
  }

  /** Replace the drawn set of downloaded areas. Safe before the style is up —
   *  the list is kept and drawn once it is — and safe to call repeatedly. */
  function setRegions(list) {
    regions = Array.isArray(list) ? list.slice() : [];
    if (!map || !mapReady || !regionSource) return;
    // The elevation download rides along with the imagery over the same
    // ground, so drawing it would claim the same area twice.
    const terrainId = terrainSpec && terrainSpec.id;
    const drawn = regions.filter((region) => !terrainId || region.source !== terrainId);

    regionSource.setData({
      type: "FeatureCollection",
      features: regionsVisible ? drawn.map((region) => ({
        type: "Feature",
        geometry: { type: "Polygon", coordinates: boundsRing(region.bounds) },
        properties: { id: region.id, name: region.name || "" },
      })) : [],
    });

    regionMarkers.forEach((marker) => marker.remove());
    regionMarkers = [];
    if (!regionsVisible) return;
    drawn.forEach((region) => {
      const b = region.bounds || {};
      const centre = [(b.w + b.e) / 2, (b.s + b.n) / 2];
      if (!isFinite(centre[0]) || !isFinite(centre[1])) return;
      regionMarkers.push(new maplibregl.Marker({
        element: buildRegionLabel(region), anchor: "center",
      }).setLngLat(centre).addTo(map));
    });
  }

  /** Show or hide the overlay without discarding the list. */
  function setRegionsVisible(on) {
    regionsVisible = !!on;
    setRegions(regions);
    return regionsVisible;
  }

  /** Fetch what is on the disk and draw it. Silent on failure: the overlay is
   *  informational, and a backend that is not answering shows up everywhere
   *  else on the page already. */
  function loadRegions() {
    Corvus.telemetry.requestJson("/api/tiles/regions").then((data) => {
      if (destroyed) return;
      setRegions((data && data.regions) || []);
    }).catch(() => {});
  }

  /** Frame a downloaded area's {w,s,e,n} — what a region row in the offline
   *  dialog does, on this map rather than the Home one. */
  function fitRegionBounds(bounds) {
    const b = bounds || {};
    const west = Number(b.w), south = Number(b.s), east = Number(b.e), north = Number(b.n);
    if (!map || ![west, south, east, north].every(isFinite)) return;
    map.fitBounds([[west, south], [east, north]], { padding: 60, duration: 700 });
  }

  function emptyCollection() { return { type: "FeatureCollection", features: [] }; }
  function emptyLine() {
    return { type: "Feature", geometry: { type: "LineString", coordinates: [] }, properties: {} };
  }

  function token(name, fallback) { return Corvus.ui.token(name, fallback); }

  function onMapClick(event) {
    if (!event || !event.lngLat) return;
    if (tool === "select") return;
    if (tool === "home") {
      placeStart(event.lngLat);
      setTool("select");
      refreshAll();
      return;
    }
    // The start is what every altitude in the plan is measured from and where
    // the climb begins, so nothing is placed before it. The tool bar already
    // says so; this is the guard for a tool armed before the start went away.
    const blocked = toolBlocked(tool);
    if (blocked) {
      setTool("select");
      setHint(blocked);
      return;
    }
    if (!addItem(tool, event.lngLat)) return;
    // A landing ends the mission: there is no second one to place.
    if (ENDS[tool]) setTool("select");
    refreshAll();
  }

  /** Tell the stylesheet how big the markers are at this zoom. One property on
   *  the map container; every marker inherits it. */
  function applyPointScale() {
    if (!map || !mapEl) return;
    mapEl.style.setProperty("--mission-point-scale", String(pointScaleFor(map.getZoom())));
  }

  function drawMap() {
    if (!map || !mapReady) return;
    drawMarkers();
    drawRoute();
    drawOrbits();
  }

  function drawMarkers() {
    markers.forEach((marker) => marker.remove());
    markers = [];

    if (home) {
      const element = buildHomeMarker(startTakeoff());
      if (homeMarker) homeMarker.remove();
      homeMarker = new maplibregl.Marker({ element, anchor: "center", draggable: true })
        .setLngLat([home.lon, home.lat]).addTo(map);
      homeMarker.on("dragend", () => {
        // placeStart carries the takeoff along: the two are one place.
        placeStart(homeMarker.getLngLat());
        window.setTimeout(refreshAll, 0);   // see the item markers below
      });
      // The start IS the takeoff on the map, so a click on it opens the
      // takeoff, which is where its climb height is set.
      element.addEventListener("click", (event) => {
        event.stopPropagation();
        const tied = startTakeoff();
        if (!tied) return;
        selectedId = tied.id;
        refreshSelection();
      });
      // The same gesture every other mark answers to. Recoverable: the START
      // tool puts both back.
      element.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        event.stopPropagation();
        removeStart();
        window.setTimeout(refreshAll, 0);
      });
    } else if (homeMarker) {
      homeMarker.remove();
      homeMarker = null;
    }

    let number = 0;
    const tied = startTakeoff();
    items.forEach((item) => {
      const spec = TYPES[item.type];
      if (!spec.position) return;
      number += 1;
      // Drawn by the start's own mark, which carries its number. A second
      // ring on the same spot would be two things to grab for one place.
      if (item === tied) return;
      const element = buildItemMarker(item, number);
      const marker = new maplibregl.Marker({ element, anchor: "center", draggable: true })
        .setLngLat([item.lon, item.lat]).addTo(map);
      marker.on("dragstart", () => { selectedId = item.id; refreshSelection(); });
      marker.on("drag", () => {
        const at = marker.getLngLat();
        item.lat = at.lat;
        item.lon = at.lng;
        drawRoute();
        drawOrbits();
        // The grip sits ON the ring, and the ring has just moved with its
        // centre. Left behind, it would size the orbit from where it used to
        // be the moment it was next touched.
        if (radiusHandle && item.id === selectedId && isOrbit(item)) {
          radiusHandle.setLngLat(radiusHandleAt(item));
        }
      });
      // Deferred by a frame: refreshAll rebuilds every marker, and removing
      // this one from inside its own dragend handler would pull the element
      // out from under MapLibre while it is still finishing the gesture.
      marker.on("dragend", () => window.setTimeout(refreshAll, 0));
      element.addEventListener("click", (event) => {
        event.stopPropagation();
        selectedId = item.id;
        refreshSelection();
      });
      /* Right-click deletes THIS point. It used to delete the last one placed
         wherever the pointer was, which is an undo wearing a delete's clothes:
         aimed at the third point of nine, it removed the ninth. The undo is
         still there — it is the right-click on empty map below — but a gesture
         made ON something now acts on that something.
         stopPropagation because MapLibre's own listeners sit on the container
         these marker elements live in, and the map handler would otherwise
         delete a second point in the same press. */
      element.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        event.stopPropagation();
        removeItem(item.id);
        // Deferred for the same reason dragend is: refreshAll rebuilds every
        // marker, and this one is still handling its own event.
        window.setTimeout(refreshAll, 0);
      });
      markers.push(marker);
    });
    drawRadiusHandle();
    Corvus.ui.refreshIcons();
  }

  /* Put the radius handle on the selected orbit's ring, or take it away.
     Dragging it sets the radius from where the pointer actually is — the
     distance from the centre to the handle IS the radius, so the number in the
     field and the circle on the map cannot disagree. */
  function drawRadiusHandle() {
    if (radiusHandle) { radiusHandle.remove(); radiusHandle = null; }
    const item = selectedItem();
    if (!isOrbit(item) || hovers || !map) return;

    const element = buildRadiusHandle();
    radiusHandle = new maplibregl.Marker({ element, anchor: "center", draggable: true })
      .setLngLat(radiusHandleAt(item)).addTo(map);
    radiusHandle.on("drag", () => {
      const at = radiusHandle.getLngLat();
      item.radius = Math.round(clampNumber(
        distanceM(item, { lat: at.lat, lon: at.lng }),
        RADIUS_MIN_M, RADIUS_MAX_M, item.radius));
      drawOrbits();
      showRadiusValue(item);
    });
    radiusHandle.on("dragend", () => {
      resetProfileNote();
      window.setTimeout(refreshAll, 0);
    });
  }

  /** Where the handle sits: due east of the centre, at the current radius.
   *  The ring's own first vertex, so the handle is always ON the line it
   *  resizes rather than near it. */
  function radiusHandleAt(item) {
    return circleRing(item, Number(item.radius) || DEFAULT_RADIUS_M, 4)[0];
  }

  /** The radius, while it is being dragged, in the profile's caption line —
   *  the same place a dragged altitude reports itself. */
  function showRadiusValue(item) {
    const note = document.getElementById("missionProfileNote");
    if (note) note.textContent = `${pointName(item)}: ${Math.round(item.radius)} m radius`;
  }

  /* The start point, drawn as the SAME landing-pad mark the Home tab puts on
     the aircraft's home: "H" in a ring with crosshair ticks on the exact
     coordinate. It is the same concept — where the flight begins — so it is
     the same drawing, down to the .home-marker class it shares with map.js.
     The planner's additions are that this one can be dragged, and that when
     it carries the takeoff it wears that point's ring and number, so the
     route reads 1, 2, 3 from the start instead of beginning at 2. */
  function buildHomeMarker(takeoff) {
    const element = document.createElement("div");
    element.className = "home-marker mission-home";
    element.title = "Start. The aircraft takes off here, and every altitude in the plan "
      + "is measured from here. Click to set the climb height, drag to move, right-click to remove";
    element.innerHTML =
      '<svg class="h-body" viewBox="0 0 32 32" aria-hidden="true">' +
        '<g class="h-ticks">' +
          '<line x1="16" y1="0.5" x2="16" y2="4.5"/>' +
          '<line x1="16" y1="27.5" x2="16" y2="31.5"/>' +
          '<line x1="0.5" y1="16" x2="4.5" y2="16"/>' +
          '<line x1="27.5" y1="16" x2="31.5" y2="16"/>' +
        "</g>" +
        '<circle class="h-ring" cx="16" cy="16" r="10"/>' +
        '<circle class="h-fill" cx="16" cy="16" r="8.6"/>' +
        '<g class="h-glyph">' +
          '<line x1="12.4" y1="11.6" x2="12.4" y2="20.4"/>' +
          '<line x1="19.6" y1="11.6" x2="19.6" y2="20.4"/>' +
          '<line x1="12.4" y1="16" x2="19.6" y2="16"/>' +
        "</g>" +
      "</svg>";
    if (takeoff) {
      element.classList.add("has-takeoff");
      element.dataset.id = String(takeoff.id);
      if (takeoff.id === selectedId) element.classList.add("is-selected");
      const number = document.createElement("span");
      number.className = "mission-home-num";
      number.textContent = String(positionNumber(items.indexOf(takeoff)));
      element.appendChild(number);
    }
    return element;
  }

  /* One planned point, as the Home tab's numbered amber ring — literally its
     .wp-marker, so the two planners share one definition rather than two that
     drift. A mission has six kinds of point where fly-to-points has one, and
     the difference is carried by a small glyph badge rather than by a second
     palette: one colour keeps the route reading as one object, and the number
     stays where the eye looks for it. */
  function buildItemMarker(item, number) {
    const spec = TYPES[item.type];
    const element = document.createElement("div");
    element.className = "wp-marker mission-point";
    element.dataset.kind = item.type;
    element.dataset.id = String(item.id);
    if (item.id === selectedId) element.classList.add("is-selected");
    element.title = markerTitle(item, number);
    const label = document.createElement("span");
    label.className = "wp-marker-num";
    label.textContent = String(number);
    element.appendChild(label);
    if (item.type !== "waypoint") {
      const badge = document.createElement("span");
      badge.className = "mission-point-badge";
      badge.appendChild(Corvus.ui.icon(spec.icon, "auto"));
      element.appendChild(badge);
    }
    setMarkerName(element, item.name);
    return element;
  }

  function markerTitle(item, number) {
    const spec = TYPES[item.type];
    const what = item.name ? `${item.name} (${spec.label})` : spec.label;
    return `${number}. ${what}: drag to move, right-click to delete`;
  }

  /* The name beside the ring, when the point has one. The number stays in the
     ring: it is the order the route is flown in, which a name does not say. */
  function setMarkerName(element, name) {
    let tag = element.querySelector(".mission-point-name");
    if (!name) {
      if (tag) tag.remove();
      return;
    }
    if (!tag) {
      tag = document.createElement("span");
      tag.className = "mission-point-name";
      element.appendChild(tag);
    }
    tag.textContent = name;
  }

  /** Show *item*'s name in its list row and on its marker while it is being
   *  typed. In place, because a rebuild of either would be a rebuild per
   *  keystroke, and the detail panel with the field in it is not touched. */
  function relabelItem(item) {
    if (listEl) {
      const label = listEl.querySelector(`.mission-row[data-id="${item.id}"] .mission-row-label`);
      if (label) {
        label.textContent = pointName(item);
        label.title = item.name ? TYPES[item.type].label : "";
      }
    }
    if (mapEl) {
      const element = mapEl.querySelector(`.mission-point[data-id="${item.id}"]`);
      if (element) {
        setMarkerName(element, item.name);
        element.title = markerTitle(item, positionNumber(items.indexOf(item)));
      }
    }
  }

  /* The grab handle on a selected orbit's ring: drag it to set the radius.
     Only the SELECTED orbit has one, because a plan with six circles in it
     would otherwise carry six handles nobody is reaching for — and because a
     handle sitting on a ring is indistinguishable from a waypoint until you
     know which circle it belongs to. */
  function buildRadiusHandle() {
    const element = document.createElement("div");
    element.className = "mission-radius-handle";
    element.title = "Drag to set the orbit radius";
    return element;
  }

  function drawRoute() {
    if (!routeSource) return;
    const points = stations(items, home);
    routeSource.setData({
      type: "Feature",
      properties: {},
      geometry: {
        type: "LineString",
        coordinates: points.length >= 2 ? points.map((p) => [p.lon, p.lat]) : [],
      },
    });
  }

  function drawOrbits() {
    if (!orbitSource) return;
    // A ring the aircraft will not fly is a drawing of a flight path that does
    // not happen. The point itself stays on the map; only the circle goes.
    const features = (hovers ? [] : items)
      .filter((item) => item.type === "loiter_turns" || item.type === "loiter_time")
      .map((item) => ({
        type: "Feature",
        properties: { id: item.id },
        geometry: {
          type: "Polygon",
          coordinates: [circleRing(item, Number(item.radius) || DEFAULT_RADIUS_M)],
        },
      }));
    orbitSource.setData({ type: "FeatureCollection", features });
  }

  /** The plan's bounding box as {w,s,e,n}, or null when nothing is drawn.

     An orbit is a circle, not a point: a box drawn around its CENTRE leaves
     the ring hanging off the edge of the view, which is the part of it the
     operator actually drew. So each station is widened by its own radius
     before the box is taken. */
  function planBounds() {
    const points = stations(items, home);
    if (!points.length) return null;
    const byId = {};
    items.forEach((item) => { byId[item.id] = item; });

    let west = Infinity, east = -Infinity, south = Infinity, north = -Infinity;
    points.forEach((station) => {
      const item = byId[station.id];
      const radius = (isOrbit(item) && !hovers) ? (Number(item.radius) || 0) : 0;
      const dLat = (radius / EARTH_RADIUS_M) * 180 / Math.PI;
      const dLon = dLat / Math.max(0.01, Math.cos(station.lat * Math.PI / 180));
      west = Math.min(west, station.lon - dLon);
      east = Math.max(east, station.lon + dLon);
      south = Math.min(south, station.lat - dLat);
      north = Math.max(north, station.lat + dLat);
    });
    if (![west, south, east, north].every(isFinite)) return null;
    return { w: west, s: south, e: east, n: north };
  }

  /** Do two {w,s,e,n} boxes share any ground at all? */
  function boxesOverlap(a, b) {
    if (!a || !b) return false;
    return a.w <= b.e && b.w <= a.e && a.s <= b.n && b.s <= a.n;
  }

  /* Frame the whole plan. */
  function fitMission() {
    if (!map) return;
    const box = planBounds();
    if (!box) return;
    // One point with no radius is a box of zero size, which fitBounds answers
    // with its maximum zoom. Centre on it instead.
    if (box.e - box.w < 1e-9 && box.n - box.s < 1e-9) {
      map.easeTo({ center: [box.w, box.s], zoom: 16, duration: 500 });
      return;
    }
    map.fitBounds([[box.w, box.s], [box.e, box.n]],
                  { padding: 80, duration: 600, maxZoom: 18 });
  }

  /** Say so when the plan is nowhere on screen — and say it, never fix it.

     This is the one case the shared view gets wrong on its own: a mission
     drawn at the last site, reopened at this one, sits entirely outside the
     view the map was handed. The old behaviour was to frame it, which is how
     the planner ended up showing somewhere else from the map next door
     — the exact thing the shared view exists to stop. So the camera is left
     alone and the sidebar points at the button that moves it, which keeps
     every camera move in this planner something the operator asked for. */
  function notePlanOffscreen() {
    if (!map || !mapReady) return;
    const plan = planBounds();
    if (!plan) return;
    let view = null;
    try {
      const bounds = map.getBounds();
      view = {
        w: bounds.getWest(), s: bounds.getSouth(),
        e: bounds.getEast(), n: bounds.getNorth(),
      };
    } catch (_error) { return; }   // no canvas yet; nothing to compare against
    if (boxesOverlap(plan, view)) return;
    say(`${planName} is outside this view. Use Fit to frame it.`, "warn");
    offscreenNotice = true;
  }

  /** Take the notice back down once the plan is on screen again, however it
   *  got there. A status line that still says "outside this view" while the
   *  route is in the middle of the map is worse than no line at all. */
  function clearOffscreenNotice() {
    if (!offscreenNotice || !map || !mapReady) return;
    const plan = planBounds();
    let view = null;
    try {
      const bounds = map.getBounds();
      view = {
        w: bounds.getWest(), s: bounds.getSouth(),
        e: bounds.getEast(), n: bounds.getNorth(),
      };
    } catch (_error) { return; }
    if (!plan || boxesOverlap(plan, view)) say(null);
  }

  function goToVehicle() {
    const state = Corvus.telemetry && Corvus.telemetry.getState();
    if (!state || !state.position || (!state.position[0] && !state.position[1])) {
      notify("warning", "The aircraft has not reported a position.");
      return;
    }
    map.easeTo({ center: [state.position[0], state.position[1]], zoom: 16, duration: 600 });
  }

  // =====================================================================
  // Place search
  //
  // Corvus.mapSearch (js/map-search.js), the same control the Home map
  // mounts. It was 450 lines here and nowhere else, which made "fly to this
  // place by name" a thing the planner could do and the map next door could
  // not — so it moved out whole, and what is left is the two answers this
  // page owes it: which map, and which downloaded areas.
  //
  // onGo is the one thing the two hosts answer differently, and only because
  // the Home map has a follow mode to break. Here a chosen place is simply
  // the operator aiming this map, which is what the planner reopens on: they
  // typed where they wanted to be looking, more deliberately than the wheel
  // nudge that already counts as an aim.
  // =====================================================================

  function buildSearch(mapWrap) {
    searchBox = Corvus.mapSearch.create({
      container: mapWrap,
      map: () => map,
      regions: () => regions,
      // On moveend, not now: the fly-to has only just been STARTED and the
      // camera is still over the ground the operator is leaving, so reading
      // the centre here would record the view they searched their way out of.
      onGo: () => { if (map) map.once("moveend", noteAim); },
    });
  }

  // =====================================================================
  // The sidebar contents
  // =====================================================================

  function refreshAll() {
    if (destroyed) return;
    // The name and the speed are the two controls that are not rebuilt from
    // state on every pass, so an Open (which replaces the whole plan) has to
    // write into them or the boxes keep claiming the old mission's values.
    // Never while one has focus: that would rewrite a number mid-keystroke.
    const nameInput = document.getElementById("missionName");
    if (nameInput && nameInput.value !== planName && document.activeElement !== nameInput) {
      nameInput.value = planName;
    }
    const speedInput = document.getElementById("missionSpeed");
    const speedText = planSpeed == null ? "" : String(planSpeed);
    if (speedInput && speedInput.value !== speedText && document.activeElement !== speedInput) {
      speedInput.value = speedText;
    }
    renderDetail();
    refreshPlan();
  }

  /** Everything a committed VALUE changes, except the panel it was typed into.
   *  Rebuilding that panel takes the focus out of the field the operator just
   *  tabbed into, and the field already holds the value that was stored. */
  function refreshPlan() {
    if (destroyed) return;
    drawMap();
    renderList();
    renderSummary();
    updateTools();
    updateHint();
    updateClearButton();
    sampleGround();
    drawProfile();
    // An edit may be what ends "this is the plan on the vehicle".
    applyProgress();
    Corvus.ui.refreshIcons();
  }

  /** The cheap half of refreshAll, for a change that only moved the highlight:
   *  no terrain resample, no route rebuild. */
  function refreshSelection() {
    if (destroyed) return;
    if (mapEl) {
      mapEl.querySelectorAll(".mission-point, .mission-home.has-takeoff").forEach((element) => {
        element.classList.toggle("is-selected", element.dataset.id === String(selectedId));
      });
    }
    // The radius grip belongs to whichever orbit is selected, so a change of
    // selection moves it or takes it off the map.
    drawRadiusHandle();
    renderList();
    markProgress();
    renderDetail();
    drawProfile();
    Corvus.ui.refreshIcons();
  }

  function renderList() {
    if (!listEl) return;
    // A rebuild takes the grabbed row out of the DOM from under the pointer.
    // Nothing is committed until the drop, so the drag is simply abandoned
    // and the list comes back in the order the plan is actually in.
    endRowDrag();
    Corvus.ui.clear(listEl);
    if (!items.length) {
      listEl.appendChild(Corvus.ui.empty(home
        ? "Nothing planned yet. Add points from the tool bar."
        : "Nothing planned yet. Press START and click where the aircraft takes off."));
      return;
    }
    let number = 0;
    items.forEach((item, at) => {
      const spec = TYPES[item.type];
      if (spec.position) number += 1;
      const row = document.createElement("div");
      row.className = "mission-row";
      row.dataset.kind = item.type;
      row.dataset.id = String(item.id);
      if (item.id === selectedId) row.classList.add("is-selected");
      row.tabIndex = 0;

      const index = document.createElement("span");
      index.className = "mission-row-index";
      index.textContent = spec.position ? String(number) : "•";
      // The one part of the row a finger can grab (see the reordering
      // section below), so it is the one part that says the row moves.
      index.title = "Drag to reorder";
      const icon = Corvus.ui.icon(spec.icon, 13);
      icon.classList.add("mission-row-icon");
      const label = document.createElement("span");
      label.className = "mission-row-label";
      // A named point is listed by its name; the icon beside it still says
      // what kind of point it is, and the tooltip says it in words.
      label.textContent = pointName(item);
      if (item.name) label.title = spec.label;
      const value = document.createElement("span");
      value.className = "mission-row-value";
      value.textContent = describeItem(item);
      row.append(index, icon, label, value);

      // The start's takeoff and the ending hold their places, so they have
      // no handle and no arrows, and nothing else can be moved past them.
      const held = pinned(at);
      if (held) {
        row.classList.add("is-pinned");
        index.title = at === 0 ? "The takeoff is always first" : "The mission always ends here";
      }
      const tools = document.createElement("span");
      tools.className = "mission-row-tools";
      tools.appendChild(Corvus.ui.iconButton("chevron-up", {
        title: "Move up", size: 12, disabled: held || pinned(at - 1),
        onClick: (event) => { event.stopPropagation(); moveItem(item.id, -1); refreshAll(); },
      }));
      tools.appendChild(Corvus.ui.iconButton("chevron-down", {
        title: "Move down", size: 12, disabled: held || pinned(at + 1),
        onClick: (event) => { event.stopPropagation(); moveItem(item.id, 1); refreshAll(); },
      }));
      tools.appendChild(Corvus.ui.iconButton("x", {
        title: "Remove", size: 12,
        onClick: (event) => { event.stopPropagation(); removeItem(item.id); refreshAll(); },
      }));
      row.appendChild(tools);

      row.addEventListener("click", () => { selectedId = item.id; refreshSelection(); });
      row.addEventListener("pointerdown", (event) => armRowDrag(event, item.id, row));
      row.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          selectedId = item.id;
          refreshSelection();
        }
      });
      listEl.appendChild(row);
    });
  }

  /* The row's one-line summary. Deliberately short: the row is a grid whose
     label column shares its width with this, and a circle that printed its
     turns, its radius AND its direction pushed "Circle" out to "Ci…". The
     rest of it is a click away in the detail panel below. */
  function describeItem(item) {
    const spec = TYPES[item.type];
    if (!spec.position) return "to start";
    if (item.type === "land") return "ground";
    if (item.type === "takeoff") return `climb to ${formatAlt(item.alt)} m`;
    const parts = [`${formatAlt(item.alt)} m`];
    if (isOrbit(item)) {
      const count = item.type === "loiter_turns"
        ? `${item.turns}×` : `${Math.round(item.seconds)} s`;
      // The radius and the turn arrow only when they mean something: on a
      // multirotor the row would otherwise quote a circle it does not fly.
      if (!hovers) {
        const turn = item.direction < 0 ? "↺" : "↻";
        parts.push(`${turn}${Math.round(item.radius)} m`);
      }
      parts.push(count);
    }
    if (item.type === "waypoint" && item.hold > 0) parts.push(`${Math.round(item.hold)} s`);
    // Only where the speed CHANGES. Printing the inherited one on every row
    // would put the same number down the whole list and hide the one row that
    // is actually different.
    if (item.speed != null) parts.push(`${formatAlt(item.speed)} m/s`);
    return parts.join(" · ");
  }

  function formatAlt(value) {
    const n = Number(value) || 0;
    return Number.isInteger(n) ? String(n) : n.toFixed(1);
  }

  // ---- dragging a row to a new place in the plan -----------------------

  /* The order of the list IS the order it is flown, and the only way to
     change it was the pair of chevrons on the row: one step per click, four
     clicks to move a waypoint past three others, and no sight of where it is
     going while you do it.

     Pointer events rather than HTML5 drag-and-drop. That API cannot be driven
     by a finger at all, its drag image is the browser's rather than the row's,
     and its drop target is whatever happens to be under the cursor rather
     than a position between two rows.

     Nothing reaches `items` until the pointer is let go. What moves during
     the drag is the DOM: the grabbed row is re-inserted between its
     neighbours as it passes them, and carried under the cursor by a
     transform — the one way of moving it that the layout the next slot is
     measured against does not see. So a rebuild of the list at any moment
     (an airframe change, an opened file) costs nothing but the gesture, and
     a cancel is a plain renderList(). */

  // How far the pointer travels before a press stops being the click that
  // selects the row and becomes a drag.
  const ROW_DRAG_SLOP_PX = 4;
  // The band at each end of the list that scrolls it while a drag is held
  // against it, and how fast: pixels of scroll per frame per pixel in.
  const ROW_EDGE_PX = 24;
  const ROW_EDGE_SPEED = 0.45;

  /**
   * Which slot a row dropped with its middle at *centre* lands in.
   *
   * *others* is every OTHER row, in layout order, as `{top, height}` boxes in
   * the same coordinates as *centre*; the answer is how many of them the
   * dropped row now belongs after, which is its index in the list it is being
   * put back into.
   *
   * Pure, and the half of the drag that can be wrong without anything
   * throwing: a row that lands one slot off is a plan flown in the wrong
   * order, and it looks exactly like a plan flown in the right one.
   */
  function dropSlot(others, centre) {
    let slot = 0;
    others.forEach((box) => { if (box.top + box.height / 2 < centre) slot += 1; });
    return slot;
  }

  /**
   * Scaled pixels per unscaled pixel — the interface-scale `zoom` on <body>.
   *
   * Everything a reorder does is measured in UNSCALED pixels, because the one
   * thing it writes is a transform and transforms are unscaled. Client rects
   * and pointer clientY are not: they come back multiplied by this, and a row
   * dragged with them undivided travelled half again as far as the pointer at
   * 150% and let go over the wrong neighbour. Corvus.ui owns the measurement
   * (see its uiScale); the guard is for a host that loaded the planner
   * without the component layer.
   */
  function pointerScale() {
    return (Corvus.ui && typeof Corvus.ui.uiScale === "function") ? Corvus.ui.uiScale() : 1;
  }

  /** An element's client rect, divided back into unscaled pixels. */
  function unscaledRect(el, k) {
    const r = el.getBoundingClientRect();
    return {
      top: r.top / k, bottom: r.bottom / k,
      left: r.left / k, right: r.right / k,
      width: r.width / k, height: r.height / k,
    };
  }

  /** The part of the list that is on screen, in unscaled pixels: the band a
   *  grabbed row is kept inside and whose ends scroll. On a wide screen the
   *  list has no height of its own, the sidebar around it scrolls and the
   *  ITEMS header stays pinned over the rows passing under it, so the band is
   *  the list cut to that box and below that header. On a narrow one the list
   *  scrolls itself and the band is simply its own box. */
  function listViewport(k) {
    const box = unscaledRect(listEl, k);
    let top = box.top;
    let bottom = box.bottom;
    if (sideScrollEl) {
      const view = unscaledRect(sideScrollEl, k);
      top = Math.max(top, view.top);
      bottom = Math.min(bottom, view.bottom);
    }
    if (listHeadEl) top = Math.max(top, unscaledRect(listHeadEl, k).bottom);
    return { top, bottom: Math.max(top, bottom) };
  }

  /** Scroll whichever box holds the list: the list itself where it has a
   *  height of its own, the sidebar where it does not. True if one moved. */
  function scrollListBy(step) {
    return [listEl, sideScrollEl].some((el) => {
      if (!el) return false;
      const was = el.scrollTop;
      el.scrollTop = was + step;
      return el.scrollTop !== was;
    });
  }

  /** A press on a row. Arms a drag without starting one: a press that never
   *  travels is a click, and the click is how a row gets selected. */
  function armRowDrag(event, id, row) {
    if (rowDrag) {
      // A second pointer on the list while a row is already in the air. The
      // first one owns the gesture: the DOM is mid-reorder and `items` is
      // not, so a second drag would read its drop slot off a list that no
      // longer says what the plan says.
      if (rowDrag.active) return;
      // An armed press whose pointer left the list before it travelled far
      // enough to be a drag. Nothing was touched, so it is simply replaced.
      endRowDrag();
    }
    if (items.length < 2) return;
    if (event.button != null && event.button !== 0) return;
    if (pinned(items.findIndex((item) => item.id === id))) return;
    // The per-row buttons are targets in their own right; a press on one is
    // a press on it, not a grab of the row underneath.
    if (closestClass(event.target, ".mission-row-tools")) return;
    // A finger dragged down the list scrolls it, so on a touch screen the
    // index cell is the only handle — it is the one part of the row the
    // stylesheet takes out of the browser's panning. Without that rule a plan
    // longer than the panel could be reordered but never read.
    if (event.pointerType === "touch" && !closestClass(event.target, ".mission-row-index")) return;
    // One scale for the whole gesture, taken here: every Y below is in the
    // unscaled pixels the transform is written in, never in the scaled ones
    // the pointer reports.
    const scale = pointerScale();
    rowDrag = {
      id, row,
      pointerId: event.pointerId,
      scale,
      startY: event.clientY / scale,
      clientY: event.clientY / scale,
      grab: 0,      // where in the row it was taken hold of
      shift: 0,     // the transform currently carrying it
      raf: 0,
      active: false,
    };
    listEl.addEventListener("pointermove", onRowPointerMove);
    listEl.addEventListener("pointerup", onRowPointerUp);
    listEl.addEventListener("pointercancel", onRowPointerCancel);
  }

  /** `Element.closest`, minus the assumption that there is an element. */
  function closestClass(node, selector) {
    const el = node && node.nodeType === 1 ? node : null;
    return !!(el && typeof el.closest === "function" && el.closest(selector));
  }

  function onRowPointerMove(event) {
    if (!rowDrag || event.pointerId !== rowDrag.pointerId) return;
    rowDrag.clientY = event.clientY / rowDrag.scale;
    if (!rowDrag.active) {
      if (Math.abs(rowDrag.clientY - rowDrag.startY) < ROW_DRAG_SLOP_PX) return;
      beginRowDrag();
    }
    // Otherwise the gesture is also a text selection of every row it crosses.
    event.preventDefault();
    placeRow();
  }

  /** The press has travelled: it is a drag. */
  function beginRowDrag() {
    const drag = rowDrag;
    drag.active = true;
    drag.grab = drag.startY - unscaledRect(drag.row, drag.scale).top;
    // Captured on the LIST rather than the row. The row is re-inserted
    // between its neighbours as the drag goes on, and captured only once the
    // press is known to be a drag: a capture taken at pointerdown would
    // retarget the click that a plain press ends in, and selecting a row by
    // clicking it would quietly stop working.
    try { listEl.setPointerCapture(drag.pointerId); } catch (_error) { /* older browsers */ }
    listEl.classList.add("is-reordering");
    drag.row.classList.add("is-dragging");
    drag.raf = window.requestAnimationFrame(rowEdgeScroll);
  }

  /** Put the grabbed row where the pointer has it, and let the rows it has
   *  passed close up behind it.
   *
   *  Measured off the live layout on every sample rather than from a geometry
   *  taken at the start: the list scrolls under the drag and the other rows
   *  move as the slot changes, so anything captured up front is wrong after
   *  the first of those. The row's own offset is re-derived the same way:
   *  its rect carries the transform, so the transform is added back to
   *  whatever is needed to reach the pointer rather than accumulated blind.
   */
  function placeRow() {
    const drag = rowDrag;
    const row = drag.row;
    const rows = Array.prototype.slice.call(listEl.children);
    const at = rows.indexOf(row);
    if (at < 0) { endRowDrag(); return; }
    const box = listViewport(drag.scale);
    const height = row.offsetHeight;
    // The row stays inside the panel however far past it the pointer goes:
    // going further is what scrolls the list, below.
    const wanted = clampNumber(drag.clientY - drag.grab, box.top, box.bottom - height, box.top);
    const others = [];
    rows.forEach((el) => {
      if (el === row) return;
      const rect = unscaledRect(el, drag.scale);
      others.push({ el, top: rect.top, height: rect.height });
    });
    const range = slotRange();
    const slot = clampNumber(dropSlot(others, wanted + height / 2), range.min, range.max, at);
    if (slot !== at) {
      listEl.insertBefore(row, others[slot] ? others[slot].el : null);
      renumberRows();
    }
    drag.shift += wanted - unscaledRect(row, drag.scale).top;
    row.style.transform = `translateY(${Math.round(drag.shift)}px)`;
  }

  /** Renumber the rows in place. The number is a row's position among the
   *  items that have one, so a row dragged past a waypoint has changed both
   *  — and the cheap way of saying so, renderList(), would take the grabbed
   *  row out from under the pointer. */
  function renumberRows() {
    let number = 0;
    Array.prototype.forEach.call(listEl.children, (row) => {
      const spec = TYPES[row.dataset.kind];
      const placed = !!(spec && spec.position);
      if (placed) number += 1;
      const cell = row.querySelector(".mission-row-index");
      if (cell) cell.textContent = placed ? String(number) : "•";
    });
  }

  /** Scroll the list while the drag is held against one of its ends. On a
   *  frame timer of its own rather than on pointermove, because a pointer
   *  parked in the band sends no more samples and the list has to keep
   *  coming to it. */
  function rowEdgeScroll() {
    if (!rowDrag || !rowDrag.active) return;
    const box = listViewport(rowDrag.scale);
    const above = (box.top + ROW_EDGE_PX) - rowDrag.clientY;
    const below = rowDrag.clientY - (box.bottom - ROW_EDGE_PX);
    let step = 0;
    if (above > 0) step = -above * ROW_EDGE_SPEED;
    else if (below > 0) step = below * ROW_EDGE_SPEED;
    // Only if it actually moved: at either end of the list it does not, and
    // replacing the row costs a layout each frame for nothing.
    if (step && scrollListBy(step)) placeRow();
    if (rowDrag) rowDrag.raf = window.requestAnimationFrame(rowEdgeScroll);
  }

  function onRowPointerUp(event) {
    if (!rowDrag || event.pointerId !== rowDrag.pointerId) return;
    const drag = rowDrag;
    const slot = drag.active
      ? Array.prototype.indexOf.call(listEl.children, drag.row) : -1;
    endRowDrag();
    // A press that never travelled. Nothing moved, and the row's own click
    // handler is about to select it as it always has.
    if (slot < 0) return;
    moveItemTo(drag.id, slot);
    // The item just moved is the one worth looking at, on the map and in the
    // panel under the list.
    selectedId = drag.id;
    refreshAll();
  }

  /** The drag is off: a cancelled pointer, or Escape. Nothing was committed,
   *  so the plan's own order is what comes back. */
  function onRowPointerCancel() {
    if (!rowDrag) return;
    endRowDrag();
    renderList();
    Corvus.ui.refreshIcons();
  }

  /** Take the drag apart, committing nothing. The drop, the cancel and every
   *  rebuild of the list come through here; only the drop goes on to move the
   *  item. */
  function endRowDrag() {
    const drag = rowDrag;
    if (!drag) return;
    rowDrag = null;
    if (drag.raf) window.cancelAnimationFrame(drag.raf);
    if (listEl) {
      try { listEl.releasePointerCapture(drag.pointerId); } catch (_error) { /* never captured */ }
      listEl.removeEventListener("pointermove", onRowPointerMove);
      listEl.removeEventListener("pointerup", onRowPointerUp);
      listEl.removeEventListener("pointercancel", onRowPointerCancel);
      listEl.classList.remove("is-reordering");
    }
    drag.row.classList.remove("is-dragging");
    drag.row.style.transform = "";
  }


  function renderDetail() {
    if (!detailEl) return;
    Corvus.ui.clear(detailEl);
    const item = selectedItem();
    if (!item) {
      detailEl.appendChild(Corvus.ui.empty("Select an item to edit it."));
      return;
    }
    const spec = TYPES[item.type];

    const head = document.createElement("div");
    head.className = "mission-detail-head";
    head.appendChild(Corvus.ui.icon(spec.icon, 14));
    const title = document.createElement("span");
    title.textContent = spec.label;
    head.appendChild(title);
    // Deleting one item had three gestures and no button: a row that has to be
    // hovered, a key with nothing on screen saying it works, and a right-click
    // on the map. This is the one an operator finds by looking.
    head.appendChild(Corvus.ui.iconButton("trash-2", {
      className: "mission-detail-remove",
      title: `Delete this ${spec.label.toLowerCase()}`,
      ariaLabel: `Delete this ${spec.label.toLowerCase()}`,
      size: 13,
      onClick: () => { removeItem(item.id); refreshAll(); },
    }));
    detailEl.appendChild(head);

    const note = Corvus.ui.empty(
      (hovers && spec.hoverHint) ? spec.hoverHint : spec.hint);
    note.className = "field-hint";
    detailEl.appendChild(note);

    detailEl.appendChild(nameField(item));

    if (spec.position) {
      const grid = document.createElement("div");
      grid.className = "mission-detail-grid";
      // Clamped here, not only announced by the input's min/max: a typed 500
      // passes an HTML `max` untouched, and the first thing that would have
      // said so is the 400 the backend answers an upload with.
      // The start's takeoff and the start are one place, so typing a new
      // position for one moves both.
      const moveTo = (lat, lon) => {
        if (item === startTakeoff()) placeStart({ lat, lon });
        else { item.lat = lat; item.lon = lon; }
        refreshPlan();
      };
      grid.appendChild(numberField({
        label: "Latitude", value: item.lat, step: 0.000001, min: -90, max: 90,
        onCommit: (value) => {
          moveTo(clampNumber(value, -90, 90, item.lat), item.lon);
          return item.lat;
        },
      }));
      grid.appendChild(numberField({
        label: "Longitude", value: item.lon, step: 0.000001, min: -180, max: 180,
        onCommit: (value) => {
          moveTo(item.lat, clampNumber(value, -180, 180, item.lon));
          return item.lon;
        },
      }));
      detailEl.appendChild(grid);

      if (item.type !== "land") {
        // Height and speed on one row: the speed box used to be the last field
        // in a panel capped at 42% of the sidebar, below the fold, where an
        // operator looking for a per-point speed concluded there was none.
        const flight = document.createElement("div");
        flight.className = "mission-detail-grid";
        flight.appendChild(numberField({
          label: item.type === "takeoff" ? "Climb to (above start)" : "Altitude above home", unit: "m",
          value: item.alt, step: 1, min: ALT_MIN_M, max: ALT_MAX_M,
          onCommit: (value) => {
            const stored = setAltitude(item.id, value);
            refreshPlan();
            return stored;
          },
        }));
        flight.appendChild(speedField(item));
        detailEl.appendChild(flight);
      } else {
        detailEl.appendChild(speedField(item));
      }
    } else {
      detailEl.appendChild(speedField(item));
    }

    // A radius and a direction this airframe will not fly are not shown as
    // fields the operator can set. Said once, under the item, rather than
    // leaving two dead controls to be filled in.
    if (isOrbit(item) && hovers) {
      const why = Corvus.ui.empty(
        "This aircraft holds the point rather than circling it, so the orbit "
        + "radius and direction are not flown. They stay in the plan for an "
        + "airframe that does fly them.");
      why.className = "field-hint mission-detail-note";
      detailEl.appendChild(why);
    }

    Object.keys(spec.params).forEach((key) => {
      if (!paramApplies(item, key)) return;
      const rule = spec.params[key];
      if (rule.choices) {
        detailEl.appendChild(choiceField(item, key, rule));
        return;
      }
      detailEl.appendChild(numberField({
        label: rule.label, unit: rule.unit,
        value: item[key], step: rule.step, min: rule.min, max: rule.max,
        onCommit: (value) => {
          item[key] = clampNumber(value, rule.min, rule.max, rule.def);
          refreshPlan();
          return item[key];
        },
      }));
    });
  }

  /* How fast the leg INTO this point is flown, and from here on until another
     point says otherwise — which is exactly what the DO_CHANGE_SPEED that
     corvus/mission.py puts in front of it does on the aircraft.

     Empty is the ABSENCE of a setting, not a zero: a 0 is a real PX4 value
     meaning "no change", and a point that pins nothing has to keep inheriting
     rather than commit the minimum. So this cannot be a numberField, whose
     empty box falls back to the last stored number. The placeholder is the
     speed the point is flown at as things stand, so an empty field still says
     what will happen. */
  /* The point's own name. Optional: empty means it is shown by its number and
     its kind, which is what the placeholder says. Shown in the list and on
     the map as it is typed; committed on change like every other field. The
     vehicle never sees it (MISSION_ITEM_INT has no field for one), so it
     lives in the plan and in the saved file. */
  function nameField(item) {
    const input = Corvus.ui.input({
      value: item.name || "",
      placeholder: TYPES[item.type].label,
      ariaLabel: "Name of this point",
      autocomplete: false,
      onInput: (value) => {
        item.name = cleanPointName(value);
        relabelItem(item);
      },
      onChange: (value) => {
        item.name = cleanPointName(value);
        input.value = item.name;
        relabelItem(item);
        // Not refreshPlan(): a rebuilt list on blur would swallow the click
        // on another row that caused the blur. Only the two places that
        // quote a point by name and are not relabelled in place.
        drawProfile();
        renderVehicle();
      },
    });
    input.maxLength = POINT_NAME_MAX;
    return Corvus.ui.field({ label: "Name", control: input });
  }

  function speedField(item) {
    const inherited = speedInto(item);
    const input = Corvus.ui.input({
      type: "number",
      value: item.speed == null ? "" : item.speed,
      placeholder: inherited == null ? "airframe" : String(inherited),
      min: SPEED_MIN_MS, max: SPEED_MAX_MS, step: 0.5,
      mono: true,
      ariaLabel: "Speed to this point in metres per second",
      autocomplete: false,
      onChange: (value) => {
        const text = String(value).trim();
        item.speed = text
          ? clampNumber(text, SPEED_MIN_MS, SPEED_MAX_MS, SPEED_MIN_MS) : null;
        input.value = item.speed == null ? "" : item.speed;
        // The points after this one inherit the change, so their rows and the
        // duration are both stale until this runs.
        refreshPlan();
        if (item.speed == null) {
          const back = speedInto(item);
          input.placeholder = back == null ? "airframe" : String(back);
        }
      },
    });
    return Corvus.ui.field({ label: "Speed (m/s)", control: input });
  }

  /** A field whose value is one of a short list rather than a number on a
   *  range — the orbit direction, today. */
  function choiceField(item, key, rule) {
    const select = Corvus.ui.select({
      ariaLabel: rule.label,
      value: String(item[key]),
      options: rule.choices.map((choice) => ({
        value: String(choice.value), label: choice.label,
      })),
      onChange: (value) => {
        item[key] = Number(value);
        snapDirection(item);
        refreshPlan();
      },
    });
    return Corvus.ui.field({ label: rule.label, control: select });
  }

  function numberField(opts) {
    // The last value that was actually STORED, which is what an emptied field
    // goes back to. opts.value is only the value the field opened with, and
    // the panel is deliberately not rebuilt on a commit.
    let last = opts.value;
    const input = Corvus.ui.input({
      type: "number",
      value: opts.value,
      min: opts.min, max: opts.max, step: opts.step,
      mono: true,
      ariaLabel: opts.label,
      autocomplete: false,
      // On commit rather than per keystroke: a half-typed "4" on the way to
      // "45" would otherwise move the aircraft's plan to 4 m and redraw
      // everything under the operator's cursor.
      onChange: (value) => {
        const text = String(value).trim();
        // An EMPTY box is not a zero, which is what Number("") would make it:
        // a cleared altitude used to commit 0 m and drop the leg to the
        // ground. Emptied or filled with junk, the field goes back to what the
        // item holds; clamped, it shows the number that was stored rather than
        // the one that was typed.
        const n = text === "" ? NaN : Number(text);
        if (!isFinite(n)) { input.value = last; return; }
        const stored = opts.onCommit(n);
        if (stored != null) { last = stored; input.value = stored; }
      },
    });
    return Corvus.ui.field({
      label: opts.unit ? `${opts.label} (${opts.unit})` : opts.label,
      control: input,
    });
  }

  function renderSummary() {
    if (!summaryEl) return;
    Corvus.ui.clear(summaryEl);
    const points = stations(items, home);
    const length = routeLength(items, home);
    const seconds = routeDuration(items, home, planSpeed);
    const gaps = clearances(points, ground).filter((value) => value != null);
    const highest = items.reduce(
      (best, item) => (TYPES[item.type].position ? Math.max(best, item.alt) : best), 0);

    summaryEl.appendChild(summaryCell("Items", String(items.length)));
    summaryEl.appendChild(summaryCell("Distance", formatDistance(length)));
    summaryEl.appendChild(summaryCell("Duration", formatDuration(seconds)));
    summaryEl.appendChild(summaryCell("Top", `${formatAlt(highest)} m`,
      highest > CEILING_HINT_M ? "warn" : ""));
    if (gaps.length) {
      const lowest = Math.min.apply(null, gaps);
      summaryEl.appendChild(summaryCell("Clearance", `${formatAlt(lowest)} m`,
        lowest < CLEARANCE_WARN_M ? "warn" : ""));
    }
    renderIssues();
  }

  /** The plan's warnings, under the summary. The same list the upload dialog
   *  shows, minus "the mission is empty" — an empty page does not need to be
   *  told it is empty. */
  function renderIssues() {
    if (!issuesEl) return;
    Corvus.ui.clear(issuesEl);
    const lines = problems().filter((line) => line !== "The mission is empty.");
    issuesEl.hidden = !lines.length;
    lines.forEach((line) => {
      const row = document.createElement("div");
      row.className = "mission-issue";
      row.appendChild(Corvus.ui.icon("triangle-alert", 12));
      const text = document.createElement("span");
      text.textContent = line;
      row.appendChild(text);
      issuesEl.appendChild(row);
    });
    // Called straight from the terrain pass and from the speed field too, so
    // it cannot lean on refreshPlan to draw its icons.
    if (lines.length) Corvus.ui.refreshIcons();
  }

  function summaryCell(label, value, level) {
    const cell = document.createElement("div");
    cell.className = "mission-summary-cell";
    if (level) cell.dataset.level = level;
    const caption = document.createElement("span");
    caption.className = "mission-summary-label";
    caption.textContent = label;
    const figure = document.createElement("span");
    figure.className = "mission-summary-value";
    figure.textContent = value;
    cell.append(caption, figure);
    return cell;
  }

  function formatDistance(metres) {
    if (!(metres > 0)) return "—";
    return metres >= 1000 ? `${(metres / 1000).toFixed(2)} km` : `${Math.round(metres)} m`;
  }

  function formatDuration(seconds) {
    if (!(seconds > 0)) return "—";
    const total = Math.round(seconds);
    const minutes = Math.floor(total / 60);
    return minutes ? `${minutes}:${String(total % 60).padStart(2, "0")}` : `${total} s`;
  }

  // =====================================================================
  // Terrain — the ground under the route
  // =====================================================================

  /**
   * Sample the DEM along the current route and redraw the profile when it
   * lands.
   *
   * Best effort by design: the elevation tiles come through the backend cache,
   * so a field laptop that cached them has a ground line and one that did not
   * has a plan drawn against a flat zero. Neither is an error state, and the
   * planner never waits on this.
   *
   * `groundToken` is the guard: an edit made while a pass is in flight makes
   * that pass irrelevant, and its result must not be drawn over the newer
   * route.
   */
  function sampleGround() {
    if (destroyed) return;
    // Taken BEFORE the early returns, not after them. A route that has just
    // become too short to profile still has a pass in the air for the route it
    // used to be, and that pass would find its own token current and draw
    // itself over the new plan.
    const pass = ++groundToken;
    if (!terrainSpec) return;
    const points = stations(items, home);
    if (points.length < 2) { ground = null; return; }
    const total = points[points.length - 1].distance;
    if (!(total > 0)) { ground = null; return; }

    const samples = [];
    for (let i = 0; i < PROFILE_SAMPLES; i += 1) {
      const distance = (i / (PROFILE_SAMPLES - 1)) * total;
      samples.push({ distance, point: pointAtDistance(points, distance) });
    }

    const zoom = terrainZoomFor(points, total);
    Promise.all(samples.map((sample) => elevationAt(sample.point, zoom)))
      .then((values) => {
        if (destroyed || pass !== groundToken) return;
        if (values.every((value) => value == null)) { ground = null; drawProfile(); return; }
        // The DEM is absolute; the profile is relative to home. Home's own
        // ground height is the datum, and the first station IS home whenever
        // the plan has one — so it is read from the same surface as the rest
        // rather than from a second source that could disagree with it.
        const base = values[0] != null ? values[0] : 0;
        homeElevation = base;
        // The DEM has just said how high the launch point is, so the plan
        // records it. A saved mission then carries its own datum, and opens on
        // a laptop with no elevation tiles still knowing what its altitudes
        // are measured from — corvus/mission.py already validates the field.
        if (home && values[0] != null) home.elevation = Math.round(base * 10) / 10;
        ground = {
          distances: samples.map((sample) => sample.distance),
          elevations: values.map((value) => (value == null ? 0 : value - base)),
        };
        renderSummary();
        drawProfile();
      })
      .catch(() => { /* no terrain is a drawable state */ });
  }

  /** The point *distance* metres along a station list. */
  function pointAtDistance(points, distance) {
    for (let i = 1; i < points.length; i += 1) {
      if (distance <= points[i].distance) {
        const span = points[i].distance - points[i - 1].distance;
        const fraction = span > 0 ? (distance - points[i - 1].distance) / span : 0;
        return lerpPoint(points[i - 1], points[i], fraction);
      }
    }
    const last = points[points.length - 1];
    return { lat: last.lat, lon: last.lon };
  }

  /** The highest DEM zoom whose tile count for this route stays inside the
   *  budget. A long route is sampled coarsely rather than not at all. */
  function terrainZoomFor(points, total) {
    const maxZoom = Math.min(14, (terrainSpec && terrainSpec.maxzoom) || 14);
    for (let zoom = maxZoom; zoom > 6; zoom -= 1) {
      let west = points[0].lon, east = points[0].lon;
      let south = points[0].lat, north = points[0].lat;
      points.forEach((p) => {
        west = Math.min(west, p.lon); east = Math.max(east, p.lon);
        south = Math.min(south, p.lat); north = Math.max(north, p.lat);
      });
      const x0 = lonToTile(west, zoom), x1 = lonToTile(east, zoom);
      const y0 = latToTile(north, zoom), y1 = latToTile(south, zoom);
      if ((x1 - x0 + 1) * (y1 - y0 + 1) <= TERRAIN_TILE_BUDGET) return zoom;
    }
    // A route long enough to defeat every zoom above still gets a profile;
    // `total` only decides how coarse it is allowed to be.
    return total > 200000 ? 7 : 8;
  }

  function lonToTile(lon, zoom) {
    return Math.floor(((lon + 180) / 360) * Math.pow(2, zoom));
  }

  function latToTile(lat, zoom) {
    const rad = lat * Math.PI / 180;
    return Math.floor(
      ((1 - Math.log(Math.tan(rad) + 1 / Math.cos(rad)) / Math.PI) / 2) * Math.pow(2, zoom));
  }

  /** Terrarium packing: the DEM's metres, hidden in an RGB pixel. */
  function decodeTerrarium(r, g, b) {
    return (r * 256 + g + b / 256) - 32768;
  }

  /** Ground elevation in metres at a point, or null when the tile is not
   *  available (offline with nothing cached, or outside the DEM's coverage). */
  function elevationAt(point, zoom) {
    const scale = Math.pow(2, zoom);
    const x = ((point.lon + 180) / 360) * scale;
    const rad = point.lat * Math.PI / 180;
    const y = ((1 - Math.log(Math.tan(rad) + 1 / Math.cos(rad)) / Math.PI) / 2) * scale;
    const tileX = Math.floor(x);
    const tileY = Math.floor(y);
    const pixelX = Math.min(255, Math.max(0, Math.floor((x - tileX) * 256)));
    const pixelY = Math.min(255, Math.max(0, Math.floor((y - tileY) * 256)));
    return loadTerrainTile(zoom, tileX, tileY).then((data) => {
      if (!data) return null;
      const offset = (pixelY * 256 + pixelX) * 4;
      return decodeTerrarium(data[offset], data[offset + 1], data[offset + 2]);
    });
  }

  // A decoded tile is 256 KB of pixel data, and a plan edited across several
  // zoom levels would otherwise accumulate every tile of every one of them.
  const TERRAIN_CACHE_MAX = 64;

  /** One decoded DEM tile, cached for the life of the page. The cache holds
   *  the promise rather than the result so two samples on the same tile —
   *  which is the common case — share a single fetch. */
  function loadTerrainTile(zoom, x, y) {
    const key = `${zoom}/${x}/${y}`;
    if (terrainTiles.has(key)) return terrainTiles.get(key);
    // Insertion-ordered, so the first key is the oldest tile.
    while (terrainTiles.size >= TERRAIN_CACHE_MAX) {
      terrainTiles.delete(terrainTiles.keys().next().value);
    }
    const pending = new Promise((resolve) => {
      const image = new Image();
      image.onload = () => {
        try {
          const canvas = document.createElement("canvas");
          canvas.width = 256;
          canvas.height = 256;
          const context = canvas.getContext("2d", { willReadFrequently: true });
          context.drawImage(image, 0, 0, 256, 256);
          resolve(context.getImageData(0, 0, 256, 256).data);
        } catch (_error) {
          resolve(null);
        }
      };
      image.onerror = () => resolve(null);
      image.src = `/api/tiles/${terrainSpec.id}/${zoom}/${x}/${y}.png`;
    });
    terrainTiles.set(key, pending);
    return pending;
  }

  // =====================================================================
  // The altitude profile
  // =====================================================================

  /**
   * Draw the profile: the ground, the planned flight path over it, a marker
   * per item and a highlight on the selected one.
   *
   * Everything is in metres above home, and the x-axis is ground distance
   * flown — so the slope of the plan line is the real climb gradient and the
   * gap to the ground is the real clearance. Both axes are fixed
   * (`fixedrange`) because this chart is an EDITOR: a drag on it moves a
   * waypoint's altitude, and a chart that also panned under the same gesture
   * would make that unusable — and it is what lets the pixel maths below stay
   * exact.
   */
  function drawProfile() {
    if (!profileEl || destroyed) return;
    // Plotly is 1.0 MB and is fetched on first use (js/lazy.js). Come back
    // through this same function once it is here; `destroyed` is re-checked
    // because the operator can leave the page while it loads, and this is the
    // one chart that can be asked to draw on every plan edit.
    if (!window.Plotly) {
      if (!Corvus.lazy || typeof Corvus.lazy.plotly !== "function") return;
      Corvus.lazy.plotly()
        .then(() => { if (!destroyed && profileEl) drawProfile(); })
        .catch((err) => console.error("altitude profile unavailable:", err));
      return;
    }
    const points = stations(items, home);
    const theme = Corvus.ui.plotlyTheme();
    const colors = Corvus.ui.chartColors();

    const planX = points.map((p) => p.distance / 1000);
    const planY = points.map((p) => p.alt);
    const range = profileRange(points, ground);

    const traces = [
      terrainFillTrace(range),
      terrainLineTrace(colors),
      {
        x: planX, y: planY,
        type: "scatter", mode: "lines+markers",
        line: { color: colors.accent, width: 2 },
        marker: {
          size: 9,
          color: points.map((p) => markerColor(p, colors)),
          line: { color: token("--bg", "#0B0E12"), width: 1.5 },
        },
        hovertemplate: "%{customdata}<extra></extra>",
        customdata: points.map((p, index) => hoverText(p, index)),
        name: "Plan",
      },
      highlightTrace(points, colors),
    ];

    const layout = Object.assign({}, theme, {
      margin: PROFILE_MARGIN,
      showlegend: false,
      hovermode: "closest",
      dragmode: false,
      xaxis: Object.assign({}, theme.xaxis, {
        title: "Distance (km)",
        fixedrange: true,
        rangemode: "tozero",
      }),
      yaxis: Object.assign({}, theme.yaxis, {
        title: "m above home",
        fixedrange: true,
        range: range,
      }),
    });

    window.Plotly.react(profileEl, traces, layout, {
      displayModeBar: false, responsive: true, showTips: false,
      // The chart is a control surface, so the cursor has to say so.
      scrollZoom: false,
    }).then(() => {
      if (destroyed) return;
      profileGeom = readGeometry(range, planX);
    }).catch(() => { /* a chart that will not draw must not break the page */ });
  }

  const PROFILE_MARGIN = { l: 52, r: 16, t: 10, b: 38 };

  /** The y-range the chart is drawn at: everything that has to be visible,
   *  plus headroom so a point dragged to the top is not against the frame. */
  function profileRange(points, groundProfile) {
    let low = 0;
    let high = DEFAULT_ALT_M;
    points.forEach((p) => { low = Math.min(low, p.alt); high = Math.max(high, p.alt); });
    if (groundProfile) {
      groundProfile.elevations.forEach((value) => {
        low = Math.min(low, value);
        high = Math.max(high, value);
      });
    }
    const pad = Math.max(12, (high - low) * 0.18);
    return [low - pad, high + pad];
  }

  function terrainFillTrace(range) {
    if (!ground) return { x: [], y: [], type: "scatter", mode: "lines", hoverinfo: "skip" };
    const xs = ground.distances.map((d) => d / 1000);
    const floor = range[0];
    return {
      x: xs.concat(xs.slice().reverse()),
      y: ground.elevations.concat(xs.map(() => floor)),
      type: "scatter",
      mode: "lines",
      fill: "toself",
      fillcolor: token("--surface-3", "#2A3038"),
      line: { width: 0 },
      hoverinfo: "skip",
      name: "Ground",
    };
  }

  function terrainLineTrace(colors) {
    if (!ground) return { x: [], y: [], type: "scatter", mode: "lines", hoverinfo: "skip" };
    return {
      x: ground.distances.map((d) => d / 1000),
      y: ground.elevations,
      type: "scatter",
      mode: "lines",
      line: { color: colors.warning, width: 1 },
      hovertemplate: "Ground %{y:.0f} m<extra></extra>",
      name: "Ground",
    };
  }

  function highlightTrace(points, colors) {
    const index = points.findIndex((p) => p.id === selectedId);
    if (index < 0) return { x: [], y: [], type: "scatter", mode: "markers", hoverinfo: "skip" };
    return {
      x: [points[index].distance / 1000],
      y: [points[index].alt],
      type: "scatter",
      mode: "markers",
      marker: {
        size: 17, color: "rgba(0,0,0,0)",
        line: { color: Corvus.ui.token("--text-1", "#12151A"), width: 2.5 },
      },
      hoverinfo: "skip",
      name: "Selected",
    };
  }

  function markerColor(station, colors) {
    if (station.type === "home") return colors.healthy;
    const spec = TYPES[station.type];
    if (!spec) return colors.nav;
    return colors[spec.color] || colors.nav;
  }

  function hoverText(station, index) {
    const item = station.type === "home" ? null : items.find((entry) => entry.id === station.id);
    // Escaped: Plotly reads hover text as a subset of HTML, and a name is
    // whatever the operator typed.
    const label = station.type === "home" ? "Start"
      : escapeMarkup(item ? pointName(item) : TYPES[station.type].label);
    const parts = [`${index === 0 ? "" : index + ". "}${label}`, `${formatAlt(station.alt)} m above home`];
    if (ground) {
      const under = interpolateAt(ground, station.distance);
      if (under != null) parts.push(`${formatAlt(station.alt - under)} m over ground`);
    }
    parts.push(formatDistance(station.distance) + " along");
    return parts.join("<br>");
  }

  // ---- dragging a point's altitude -------------------------------------

  /**
   * The chart's pixel geometry, so a mouse position can be turned into an
   * altitude and back.
   *
   * Plotly's own `_fullLayout._size` is used when it is there (it is the only
   * thing that knows the plot area after the auto-margin pass), with the
   * layout margins as a fallback — both axes are `fixedrange`, so the ranges
   * are exactly what was asked for and the arithmetic below is not a guess.
   */
  function readGeometry(range, planX) {
    const full = profileEl && profileEl._fullLayout;
    const size = (full && full._size) || null;
    const width = profileEl.clientWidth || 0;
    const height = profileEl.clientHeight || 0;
    const left = size ? size.l : PROFILE_MARGIN.l;
    const top = size ? size.t : PROFILE_MARGIN.t;
    const plotWidth = size ? size.w : Math.max(1, width - PROFILE_MARGIN.l - PROFILE_MARGIN.r);
    const plotHeight = size ? size.h : Math.max(1, height - PROFILE_MARGIN.t - PROFILE_MARGIN.b);
    const xAxis = (full && full.xaxis && full.xaxis.range) || [0, Math.max(0.001, planX[planX.length - 1] || 1)];
    return {
      left, top, plotWidth, plotHeight,
      xRange: xAxis.slice(),
      yRange: range.slice(),
    };
  }

  /** Data value -> pixel, inside the plot area. */
  function toPixel(geom, x, y) {
    const xSpan = geom.xRange[1] - geom.xRange[0] || 1;
    const ySpan = geom.yRange[1] - geom.yRange[0] || 1;
    return {
      px: geom.left + ((x - geom.xRange[0]) / xSpan) * geom.plotWidth,
      py: geom.top + ((geom.yRange[1] - y) / ySpan) * geom.plotHeight,
    };
  }

  /**
   * A pointer's position inside the plot, in the plot's own pixels.
   *
   * readGeometry() works in Plotly's layout space, which comes from
   * clientWidth/clientHeight and is therefore UNSCALED, while both the
   * element's rect and the pointer's clientX/Y arrive SCALED. Dividing both
   * by *k* is what puts a pointer and the station it is over in the same
   * space; without it every station sat 1.5x further right and lower than it
   * was drawn at 150%, the grab radius never reached one, and a height could
   * not be dragged at all.
   */
  function plotPoint(rect, clientX, clientY, k) {
    return { px: (clientX - rect.left) / k, py: (clientY - rect.top) / k };
  }

  /** Pixel -> altitude, clamped to what is currently drawable. Going higher
   *  than the frame is done by letting go and dragging again: the range grows
   *  with the data on every redraw. */
  function toAltitude(geom, py) {
    const ySpan = geom.yRange[1] - geom.yRange[0] || 1;
    const value = geom.yRange[1] - ((py - geom.top) / geom.plotHeight) * ySpan;
    return clampNumber(value, Math.max(ALT_MIN_M, geom.yRange[0]),
      Math.min(ALT_MAX_M, geom.yRange[1]), 0);
  }

  function wireProfileDragging() {
    profileEl.addEventListener("pointerdown", onProfilePointerDown);
  }

  function onProfilePointerDown(event) {
    if (!profileGeom || event.button !== 0) return;
    const at = plotPoint(profileEl.getBoundingClientRect(),
      event.clientX, event.clientY, pointerScale());
    const hit = hitTest(at.px, at.py);
    if (!hit) return;

    selectedId = hit.id;
    refreshSelection();
    // Nothing here has a height to set: home is the datum of the frame, a
    // landing ends on the ground, and a return names no place at all. A drag
    // started on one used to run to completion and report a height it had not
    // moved.
    const spec = TYPES[hit.type];
    if (hit.id === "home" || hit.type === "land" || !spec || !spec.position) return;

    event.preventDefault();
    dragState = { id: hit.id, pointerId: event.pointerId };
    try { profileEl.setPointerCapture(event.pointerId); } catch (_error) { /* older browsers */ }
    profileEl.classList.add("is-dragging");
    profileEl.addEventListener("pointermove", onProfilePointerMove);
    profileEl.addEventListener("pointerup", onProfilePointerUp);
    profileEl.addEventListener("pointercancel", onProfilePointerUp);
    showDragValue(hit.id);
  }

  /** The station nearest *(px, py)*, within a grab radius. Distance is
   *  measured in pixels rather than in data units, because that is how a
   *  pointer is aimed. */
  function hitTest(px, py) {
    const points = stations(items, home);
    let best = null;
    let bestGap = 18;
    points.forEach((station) => {
      const at = toPixel(profileGeom, station.distance / 1000, station.alt);
      const gap = Math.hypot(at.px - px, at.py - py);
      if (gap < bestGap) { bestGap = gap; best = station; }
    });
    return best;
  }

  function onProfilePointerMove(event) {
    if (!dragState || !profileGeom) return;
    const at = plotPoint(profileEl.getBoundingClientRect(),
      event.clientX, event.clientY, pointerScale());
    const altitude = toAltitude(profileGeom, at.py);
    if (setAltitude(dragState.id, altitude) == null) return;
    // Restyle rather than redraw: a full react() on every pointer sample makes
    // the drag lag behind the pointer, and nothing but the two altitude traces
    // changes while one is in flight.
    const points = stations(items, home);
    const index = points.findIndex((station) => station.id === dragState.id);
    window.Plotly.restyle(profileEl, { y: [points.map((p) => p.alt)] }, [2]);
    if (index >= 0) window.Plotly.restyle(profileEl, { y: [[points[index].alt]] }, [3]);
    showDragValue(dragState.id);
  }

  function onProfilePointerUp() {
    const id = dragState && dragState.id;
    endDrag();
    if (id == null) return;
    // Now the expensive half: the range may need to grow, the summary's
    // clearance may have changed, and the list row shows the new height.
    refreshAll();
  }

  function endDrag() {
    if (!dragState || !profileEl) { dragState = null; return; }
    try { profileEl.releasePointerCapture(dragState.pointerId); } catch (_error) { /* never captured */ }
    profileEl.classList.remove("is-dragging");
    profileEl.removeEventListener("pointermove", onProfilePointerMove);
    profileEl.removeEventListener("pointerup", onProfilePointerUp);
    profileEl.removeEventListener("pointercancel", onProfilePointerUp);
    dragState = null;
    resetProfileNote();
  }

  /** Put the profile's caption back to what it says when nothing is being
   *  dragged. Shared by the altitude drag and the radius grip, both of which
   *  borrow that line as their live readout. */
  function resetProfileNote() {
    const note = document.getElementById("missionProfileNote");
    if (note) note.textContent = "Drag a point up or down to set its height.";
  }

  /** What the height is, while it is being dragged. In the chart's own caption
   *  rather than a tooltip: the pointer is on the point, and a label under the
   *  cursor would be the one thing covering what is being aimed at. */
  function showDragValue(id) {
    const note = document.getElementById("missionProfileNote");
    const item = items.find((entry) => entry.id === id);
    if (!note || !item) return;
    const parts = [`${pointName(item)}: ${formatAlt(item.alt)} m above home`];
    if (ground) {
      const station = stations(items, home).find((entry) => entry.id === id);
      const under = station ? interpolateAt(ground, station.distance) : null;
      if (under != null) parts.push(`${formatAlt(item.alt - under)} m over ground`);
    }
    note.textContent = parts.join("  ·  ");
  }

  // =====================================================================
  // Vehicle and file actions
  // =====================================================================

  function problems() {
    const list = [];
    if (!items.length) list.push("The mission is empty.");
    if (items.length && !home) list.push("The mission has no start point.");

    /* Order is the one thing a mission has that a set of points does not, and
       PX4 flies it literally: it does not refuse a takeoff in the middle or an
       item after a landing, it simply does them in the order given. Said out
       loud rather than refused, because a plan uploaded to an aircraft that is
       ALREADY FLYING legitimately has no takeoff at all. */
    const takeoffs = items.filter((item) => item.type === "takeoff").length;
    if (takeoffs > 1) {
      list.push(`The plan has ${takeoffs} takeoffs; only the first one climbs.`);
    }
    if (takeoffs === 1 && items[0].type !== "takeoff") {
      list.push("The takeoff is not the first item. The aircraft flies to it before climbing.");
    }
    const ends = endIndex();
    if (ends < 0 && items.some((item) => TYPES[item.type].position && item.type !== "takeoff")) {
      list.push("The mission has no ending. Add a landing or a return, or the aircraft "
        + "stays at the last point when it is done.");
    }
    if (ends >= 0 && ends < items.length - 1) {
      const left = items.length - 1 - ends;
      list.push(
        `${left} item${left === 1 ? "" : "s"} after the `
        + `${TYPES[items[ends].type].label.toLowerCase()} will not be flown.`);
    }

    const highest = items.reduce(
      (best, item) => (TYPES[item.type].position ? Math.max(best, item.alt) : best), 0);
    if (highest > CEILING_HINT_M) {
      list.push(`The plan reaches ${formatAlt(highest)} m, above the ${CEILING_HINT_M} m open-category ceiling.`);
    }
    const gaps = clearances(stations(items, home), ground).filter((value) => value != null);
    if (gaps.length && Math.min.apply(null, gaps) < CLEARANCE_WARN_M) {
      list.push(`The route passes within ${formatAlt(Math.min.apply(null, gaps))} m of the ground.`);
    }
    return list;
  }

  function uploadPlan(andFly, button) {
    if (!items.length) {
      say("Nothing to upload. The mission is empty.", "warn");
      return;
    }
    const warnings = problems().filter((line) => line !== "The mission is empty.");
    const go = () => sendPlan(andFly, button);
    if (andFly || warnings.length) confirmUpload(andFly, warnings, go);
    else go();
  }

  /** The one confirmation in the planner, and it is deliberately not a toast:
   *  "upload and fly" arms and launches an aircraft, so it asks with the
   *  warnings in front of the operator rather than after the fact. */
  function confirmUpload(andFly, warnings, proceed) {
    const body = document.createElement("div");
    const lead = Corvus.ui.empty(andFly
      ? `${planName} will be uploaded, the vehicle switched to MISSION and armed. It will take off.`
      : `${planName} will be written to the vehicle. It will not fly until you start it.`);
    body.appendChild(lead);
    warnings.forEach((line) => {
      const warn = Corvus.ui.empty(line);
      warn.className = "mission-warning";
      body.appendChild(warn);
    });

    let dialog = null;
    const cancel = Corvus.ui.button({ variant: "secondary", label: "Cancel", onClick: () => dialog.close() });
    const confirm = Corvus.ui.button({
      variant: andFly ? "danger" : "primary",
      icon: andFly ? "play" : "upload",
      label: andFly ? "Upload and fly" : "Upload",
      onClick: () => { dialog.close(); proceed(); },
    });
    dialog = Corvus.ui.modal({
      title: andFly ? "Fly this mission?" : "Upload this mission?",
      body,
      actions: [cancel, confirm],
    });
    dialog.open();
    Corvus.ui.refreshIcons();
  }

  function sendPlan(andFly, button) {
    say(andFly ? "Uploading and starting…" : "Uploading…");
    Corvus.ui.setBusy(button, true);
    Corvus.telemetry.requestJson("/api/mission/upload", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ plan: toPlan(), start: !!andFly }),
    }).then((res) => {
      const count = (res && res.items) || 0;
      if (!destroyed) syncWith(res && res.revision);
      say(andFly
        ? `Mission started: ${count} items on the vehicle.`
        : `${count} items on the vehicle. Start it from the flight modes when ready.`, "ok");
    }).catch((error) => {
      say(error.message || "The vehicle refused the mission.", "err");
    }).finally(() => {
      if (!destroyed) Corvus.ui.setBusy(button, false);
    });
  }

  // =====================================================================
  // The mission on the vehicle
  // =====================================================================

  /** The plan as it goes on the wire, for "is this still what was synced". */
  function planKey() {
    const plan = toPlan();
    // Without the names: they never reach the vehicle, so renaming a point
    // mid-flight does not make the plan a different one from what is flown.
    const wire = plan.items.map((item) => {
      const copy = Object.assign({}, item);
      delete copy.name;
      return copy;
    });
    return JSON.stringify({ items: wire, speed: plan.speed == null ? null : plan.speed });
  }

  /** This plan is now what the vehicle holds, at the backend's `revision`. */
  function syncWith(revision) {
    synced = typeof revision === "number" ? { revision, key: planKey() } : null;
    applyProgress();
  }

  function numberOr(value, fallback) {
    return typeof value === "number" && isFinite(value) ? value : fallback;
  }

  /** A telemetry frame's mission fields; everything is redrawn only on a change. */
  function onVehicleMission(frame) {
    if (!frame) return;
    const next = {
      connected: !!frame.connected,
      state: typeof frame.mission_state === "string" ? frame.mission_state : "",
      total: numberOr(frame.mission_total, -1),
      known: !!frame.mission_known,
      revision: numberOr(frame.mission_revision, 0),
      item: numberOr(frame.mission_item, -1),
      reached: numberOr(frame.mission_reached_item, -1),
    };
    const sig = JSON.stringify(next);
    if (sig === vehicleSig) return;
    vehicleSig = sig;
    vehicleMission = next;
    applyProgress();
  }

  function isSynced() {
    const v = vehicleMission;
    return !!(v && synced && v.known && v.revision === synced.revision);
  }

  function currentProgress() {
    if (!isSynced() || planKey() !== synced.key) return null;
    const v = vehicleMission;
    return { item: v.item, reached: v.reached, state: v.state };
  }

  /** The plan item being flown to, or -1 when nothing is. */
  function currentIndex() {
    if (!progress) return -1;
    if (["not_started", "complete", "no_mission"].indexOf(progress.state) >= 0) return -1;
    return progress.item;
  }

  function applyProgress() {
    if (destroyed) return;
    progress = currentProgress();
    markProgress();
    drawLeg();
    renderVehicle();
  }

  /** The current point and the ones already flown, on the map and in the list. */
  function markProgress() {
    const current = currentIndex();
    let doneBefore = -1;
    if (progress) {
      if (progress.state === "complete") doneBefore = items.length;
      else if (progress.state !== "not_started") doneBefore = progress.item;
    }
    const classes = {};
    items.forEach((item, index) => {
      classes[String(item.id)] = { current: index === current, done: index < doneBefore };
    });
    [mapEl && mapEl.querySelectorAll(".mission-point"),
     listEl && listEl.querySelectorAll(".mission-row")].forEach((nodes) => {
      Array.prototype.forEach.call(nodes || [], (node) => {
        const mark = classes[node.dataset.id] || { current: false, done: false };
        node.classList.toggle("is-current", mark.current);
        node.classList.toggle("is-done", mark.done);
      });
    });
  }

  /** From the point before the current one (or home) to the current one. */
  function drawLeg() {
    if (!legSource) return;
    const index = currentIndex();
    let coordinates = [];
    const target = index >= 0 ? items[index] : null;
    if (target) {
      let from = null;
      for (let i = index - 1; i >= 0 && !from; i -= 1) {
        if (TYPES[items[i].type].position) from = items[i];
      }
      from = from || home;
      const to = TYPES[target.type].position ? target : home;
      if (from && to) coordinates = [[from.lon, from.lat], [to.lon, to.lat]];
    }
    legSource.setData({
      type: "Feature", properties: {},
      geometry: { type: "LineString", coordinates },
    });
  }

  /** The number the list and the map give the item at *index*: its place
   *  among the items that have a position. */
  function positionNumber(index) {
    let number = 0;
    for (let i = 0; i <= index && i < items.length; i += 1) {
      if (TYPES[items[i].type].position) number += 1;
    }
    return number;
  }

  /** "3. Waypoint" (or "3. Ridge" once it is named), numbered as the list
   *  numbers it. */
  function itemLabel(index) {
    const item = items[index];
    if (!item) return "";
    if (!TYPES[item.type].position) return pointName(item);
    return `${positionNumber(index)}. ${pointName(item)}`;
  }

  function renderVehicle() {
    if (!vehicleEl) return;
    Corvus.ui.clear(vehicleEl);
    const v = vehicleMission;
    let text = "";
    let level = "";
    let offerRead = false;
    if (!v || !v.connected) {
      text = "";
    } else if (progress) {
      level = "live";
      const index = currentIndex();
      if (progress.state === "complete") text = "The vehicle has flown this mission to the end.";
      else if (progress.state === "not_started") text = "This plan is on the vehicle, not started yet.";
      else if (progress.state === "paused") text = `Paused. Next: ${itemLabel(progress.item)}.`;
      else if (index >= 0 && !TYPES[items[index].type].position) text = "Returning to launch.";
      else if (index >= 0) text = `Flying to ${itemLabel(index)}.`;
      else text = "This plan is on the vehicle.";
    } else if (isSynced()) {
      level = "warn";
      text = "Changed since it was put on the vehicle. Upload to fly this version.";
    } else if (v.total > 0) {
      level = "warn";
      text = `The vehicle holds a mission of ${v.total} item${v.total === 1 ? "" : "s"} `
        + "that is not this plan.";
      offerRead = true;
    }
    vehicleEl.hidden = !text;
    if (!text) return;
    vehicleEl.dataset.level = level;
    const line = document.createElement("span");
    line.className = "mission-vehicle-text";
    line.textContent = text;
    vehicleEl.appendChild(line);
    if (offerRead) {
      const read = Corvus.ui.button({
        variant: "secondary", size: "sm", icon: "download", label: "Read it",
        onClick: () => readFromVehicle(read),
      });
      vehicleEl.appendChild(read);
      Corvus.ui.refreshIcons();
    }
  }

  /** Replace the plan on screen with the vehicle's, asking first if that loses one. */
  function readFromVehicle(button) {
    const go = () => fetchVehicleMission(button);
    if (!items.length) { go(); return; }
    let dialog = null;
    const cancel = Corvus.ui.button({
      variant: "secondary", label: "Keep this plan", onClick: () => dialog.close(),
    });
    const confirm = Corvus.ui.button({
      variant: "primary", icon: "download", label: "Replace it",
      onClick: () => { dialog.close(); go(); },
    });
    dialog = Corvus.ui.modal({
      title: "Read the mission from the vehicle?",
      body: `${planName} on screen is replaced by the mission the vehicle holds. `
        + "What is saved is not touched.",
      actions: [cancel, confirm],
    });
    dialog.open();
    Corvus.ui.refreshIcons();
  }

  function fetchVehicleMission(button) {
    say("Reading the mission from the vehicle…");
    if (button) Corvus.ui.setBusy(button, true);
    Corvus.telemetry.requestJson("/api/mission/download", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: "{}",
    }).then((res) => {
      if (destroyed) return;
      if (!res || !res.plan) {
        say((res && res.error) || "The vehicle's mission cannot be shown as a plan.", "err");
        return;
      }
      fromPlan(res.plan);
      refreshAll();
      syncWith(res.revision);
      if (items.length) fitMission();
      const skipped = res.skipped || [];
      const adjusted = res.adjusted || [];
      let text = res.count
        ? `Read ${res.count} item${res.count === 1 ? "" : "s"} from the vehicle.`
        : "The vehicle holds no mission.";
      if (skipped.length) {
        text += ` ${skipped.length} cannot be drawn here (${skipped[0].reason}`
          + (skipped.length > 1 ? ", and more" : "") + "). "
          + "Uploading this plan would take them off the vehicle.";
      }
      if (adjusted.length) {
        text += ` ${adjusted.length} value${adjusted.length === 1 ? " was" : "s were"} `
          + "changed to fit the planner.";
      }
      say(text, skipped.length || adjusted.length ? "warn" : "ok");
    }).catch((error) => {
      say(error.message || "Could not read the mission from the vehicle.", "err");
    }).finally(() => {
      if (!destroyed && button) Corvus.ui.setBusy(button, false);
    });
  }

  function savePlan() {
    if (!items.length) {
      say("Nothing to save. The mission is empty.", "warn");
      return;
    }
    say("Saving…");
    Corvus.telemetry.requestJson("/api/mission/plans/save", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: planName, plan: toPlan() }),
    }).then((res) => {
      rememberLastPlan((res && res.name) || planName);
      say(`Saved as ${(res && res.name) || planName}.`, "ok");
    }).catch((error) => {
      say(error.message || "Could not save the mission.", "err");
    });
  }

  function openPlanDialog() {
    Corvus.telemetry.requestJson("/api/mission/plans").then((data) => {
      if (destroyed) return;
      const plans = (data && data.plans) || [];
      const body = document.createElement("div");
      body.className = "mission-open-list";
      let dialog = null;

      if (!plans.length) {
        body.appendChild(Corvus.ui.empty(
          `No saved missions yet. They are kept in ${(data && data.dir) || "~/.corvus/missions"}.`));
      }
      plans.forEach((entry) => {
        const row = document.createElement("div");
        row.className = "mission-open-row";
        const label = document.createElement("button");
        label.type = "button";
        label.className = "mission-open-name";
        const title = document.createElement("span");
        title.textContent = entry.title || entry.name;
        const meta = document.createElement("span");
        meta.className = "mission-open-meta";
        meta.textContent = `${entry.items} items · ${formatDistance(entry.distance_m)}`;
        label.append(title, meta);
        label.addEventListener("click", () => { dialog.close(); loadPlan(entry.name); });
        row.appendChild(label);
        /* Two presses, and the second one is the one that deletes a file off
           the disk. Not a dialog: a second modal over this one would take
           Escape away from the list underneath it, since both close on the
           captured key and the outer one is registered first. A failure is
           reported rather than swallowed — a row that vanishes is the only
           thing an operator reads as "deleted". */
        let armed = false;
        let armedTimer = 0;
        const disarm = () => {
          armed = false;
          removeButton.classList.remove("is-armed");
          removeButton.title = `Delete ${entry.name}`;
        };
        const removeButton = Corvus.ui.iconButton("trash-2", {
          title: `Delete ${entry.name}`,
          ariaLabel: `Delete ${entry.name}`,
          onClick: () => {
            if (!armed) {
              armed = true;
              removeButton.classList.add("is-armed");
              removeButton.title = `Press again to delete ${entry.name}`;
              window.clearTimeout(armedTimer);
              armedTimer = window.setTimeout(disarm, 4000);
              return;
            }
            window.clearTimeout(armedTimer);
            disarm();
            Corvus.telemetry.requestJson("/api/mission/plans/remove", {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ name: entry.name }),
            }).then(() => {
              forgetLastPlan(entry.name);
              row.remove();
              if (!body.querySelector(".mission-open-row")) {
                body.appendChild(Corvus.ui.empty("No saved missions left."));
              }
            }).catch((error) => {
              say(error.message || `Could not delete ${entry.name}.`, "err");
            });
          },
        });
        row.appendChild(removeButton);
        body.appendChild(row);
      });

      dialog = Corvus.ui.modal({
        title: "Open a mission",
        body,
        actions: [Corvus.ui.button({ variant: "secondary", label: "Close", onClick: () => dialog.close() })],
      });
      dialog.open();
      Corvus.ui.refreshIcons();
    }).catch((error) => {
      say(error.message || "Could not list the saved missions.", "err");
    });
  }

  /** Open a saved plan. `quiet` is the page reopening the last mission by
   *  itself: an operator who deleted it, or moved to a machine without it,
   *  asked for none of this and must not be shown a failure they did not
   *  cause. An Open they clicked always reports. */
  function loadPlan(name, quiet) {
    Corvus.telemetry.requestJson("/api/mission/plans/load", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    }).then((res) => {
      if (destroyed || !res || !res.plan) return;
      fromPlan(res.plan);
      rememberLastPlan(res.name || name);
      refreshAll();
      // An Open the operator clicked is them asking for THAT mission, so it
      // is framed. The page reopening yesterday's plan by itself asked for
      // nothing and must not move a view the operator chose — it says where
      // the plan is instead.
      if (quiet) notePlanOffscreen();
      else fitMission();
      if (!quiet) say(`Opened ${res.name || name}.`, "ok");
    }).catch((error) => {
      if (!quiet) say(error.message || "Could not open that mission.", "err");
    });
  }

  /** Wipe the plan, asking first when there is something to lose. Both the
   *  list's trash and New land here: a route is twenty minutes of drawing, and
   *  neither button is one an operator means to press by accident. */
  function confirmClear() {
    if (!items.length && !home) { clearPlan(); return; }
    let dialog = null;
    const cancel = Corvus.ui.button({
      variant: "secondary", label: "Keep it", onClick: () => dialog.close(),
    });
    const confirm = Corvus.ui.button({
      variant: "danger", icon: "trash-2", label: "Clear the plan",
      onClick: () => { dialog.close(); clearPlan(); },
    });
    dialog = Corvus.ui.modal({
      title: "Clear this mission?",
      body: `${planName} holds ${items.length} item${items.length === 1 ? "" : "s"}. `
        + "Clearing it here does not touch what is already saved or on the vehicle.",
      actions: [cancel, confirm],
    });
    dialog.open();
    Corvus.ui.refreshIcons();
  }

  function clearPlan() {
    items = [];
    home = null;
    homeElevation = null;
    selectedId = null;
    planName = "Mission";
    planSpeed = null;
    ground = null;
    const nameInput = document.getElementById("missionName");
    if (nameInput) nameInput.value = planName;
    say(null);
    setTool("select");
    refreshAll();
  }

  /* Which plan the page opens on next time. localStorage rather than the
     backend config: it is a per-machine convenience, and a field laptop that
     cannot reach its own config file should still come back to the mission the
     operator was drawing on it. Failure is silent and means "start empty". */
  const LAST_PLAN_KEY = "corvus.mission.last";

  function rememberLastPlan(name) {
    try { localStorage.setItem(LAST_PLAN_KEY, String(name)); } catch (_error) {}
  }

  /** Stop reopening a plan that has just been deleted. */
  function forgetLastPlan(name) {
    try {
      if (localStorage.getItem(LAST_PLAN_KEY) === String(name)) {
        localStorage.removeItem(LAST_PLAN_KEY);
      }
    } catch (_error) {}
  }

  function loadLastPlan() {
    let name = "";
    try { name = localStorage.getItem(LAST_PLAN_KEY) || ""; } catch (_error) {}
    if (name) loadPlan(name, true);
  }

  return {
    render,
    // Leaving the page for another one. Keeps the map; see render() above for
    // why. teardown() is the destroyer, for the page leaving the app.
    suspend,
    teardown,
    // Read by Settings and by tests; never a second copy of the state.
    getPlan: toPlan,
    setPlan: (plan) => { fromPlan(plan); refreshAll(); },
    // test hooks: the pure geometry, assertable without a map or a browser.
    _distanceM: distanceM,
    // test hook: the box overlap the reopen rule turns on. Pure, and wrong in
    // ways nothing throws over. The search box's own pure halves moved out
    // with it and are asserted against Corvus.mapSearch.
    _boxesOverlap: boxesOverlap,
    _stations: stations,
    _routeLength: routeLength,
    _routeDuration: routeDuration,
    // test hook: the inheritance a pinned speed sets up. Pure, and wrong in
    // ways nothing throws over — the plan still uploads, at the wrong speed.
    _speedByItem: speedByItem,
    _circleRing: circleRing,
    _clearances: clearances,
    _interpolateAt: interpolateAt,
    _decodeTerrarium: decodeTerrarium,
    _profileRange: profileRange,
    _toPixel: toPixel,
    _toAltitude: toAltitude,
    // test hook: the pointer -> plot conversion. Pure, and the one place the
    // interface scale has to be divided back out of a drag.
    _plotPoint: plotPoint,
    _clampNumber: clampNumber,
    // test hook: which slot a dragged row lands in. Pure, and silent when
    // wrong — the plan still uploads, in the wrong order.
    _dropSlot: dropSlot,
    _pointScaleFor: pointScaleFor,
    _hoverTypes: HOVER_TYPES,
    _vehicleHovers: vehicleHovers,
    // test hooks: "is the vehicle flying THIS plan, and where". Wrong in the
    // worst way nothing throws over: a leg highlighted on a route the
    // aircraft is not flying.
    _syncWith: syncWith,
    _onVehicleMission: onVehicleMission,
    _progress: () => progress,
    _currentIndex: currentIndex,
    _widthByZoom: widthByZoom,
    _problems: problems,
    // test hooks: the order a plan is drawn in. Start first, the route, one
    // ending; wrong silently, as a landing with nothing before it.
    _placeStart: placeStart,
    _removeStart: removeStart,
    _addItem: addItem,
    _toolBlocked: toolBlocked,
    _moveItemTo: moveItemTo,
    // The live chart geometry and the grab test that reads it. Exported
    // because "the drag does nothing" is otherwise invisible from outside: it
    // is a silent early return in a pointer handler.
    // The page's own MapLibre map. Not the Home tab's — this one belongs to
    // the planner and dies with it.
    _map: () => map,
    _profileGeom: () => profileGeom,
    _hitTest: hitTest,
    _bounds: {
      ALT_MIN_M, ALT_MAX_M, RADIUS_MIN_M, RADIUS_MAX_M,
      HOLD_MAX_S, TURNS_MAX, SPEED_MIN_MS, SPEED_MAX_MS, MAX_ITEMS,
      POINT_NAME_MAX,
    },
    // test hooks: a point's name, cleaned as the backend cleans it, and the
    // plan as the vehicle sees it (which has no names in it).
    _cleanPointName: cleanPointName,
    _planKey: planKey,
    _types: TYPES,
    // The tool bar's contents. Every entry that is not a divider is rendered
    // as icon over caption, so one without a label is a blank button.
    _tools: TOOLS,
    // The zoom rail's contents. The offline download and the trash are NOT in
    // it, on either map — they have corners of their own.
    _mapControls: MAP_CONTROLS,
    // The downloaded-area overlay, so a test can drive it without a map.
    _setRegions: setRegions,
  };
})();
