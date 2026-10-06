"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.missionGeometry: the mission planner's pure geometry. Distances,
  stations, the altitude profile, durations and the pattern sweeps, with no
  DOM and no map. Moved out of js/mission.js, which takes these functions and
  constants at load and hands over its item TYPES table with useTypes().
  Loaded before js/mission.js.
*/
Corvus.missionGeometry = (function () {
  // The planner's item table (js/mission.js), set once at load.
  let TYPES = {};

  // Cruise speed used for the duration estimate when the plan pins none.
  const NOMINAL_SPEED_MS = 10;
  // A fixed wing's climb-out when the takeoff names no pitch: PX4's
  // FW_TKO_PITCH_MIN default on v1.16 to v1.18, and the low end of the 10 to
  // 15 degrees ArduPlane recommends. The run it needs to reach the takeoff
  // height is drawn from it, so a steeper real climb only ever adds room.
  const TAKEOFF_PITCH_DEG = 10;
  const EARTH_RADIUS_M = 6371008.8;
  const PATTERN_SPACING_M = { min: 5, max: 500, def: 30 };
  const PATTERN_WIDTH_M = { min: 10, max: 2000, def: 100 };
  const PATTERN_CIRCLE_MIN_M = 10;
  const PATTERN_CIRCLE_SEGMENTS = 48;
  const PATTERN_CIRCLE_POINTS = { min: 8, max: 48 };
  const PATTERN_MITER_LIMIT = 2.5;

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
      const station = {
        id: item.id,
        type: item.type,
        lat: place.lat,
        lon: place.lon,
        alt: spec.position ? Number(item.alt) || 0 : 0,
        distance: cumulative,
      };
      if (item.type === "land" && item.approach_alt != null) station.approach = Number(item.approach_alt);
      if (item.type === "takeoff" && Number(item.pitch) > 0) station.pitch = Number(item.pitch);
      out.push(station);
    });
    return out;
  }

  /**
   * The route as the altitude profile draws it, in one pass: the line
   * (kilometres along, metres up) and the marks, the points on it that are
   * drawn and can be grabbed. How a leg looks is the aircraft's, *kind* being
   * the plan's (see AIRCRAFT), and *ret* how the connected vehicle flies a
   * Return (GET /api/mission/return), or null.
   *
   * A multicopter climbs straight up and comes straight down; a fixed wing
   * does neither. Its takeoff is a climb-out along the first leg, which
   * reaches the takeoff height only after a run, and it glides into a
   * landing. A multicopter's landing has a corner above it where the descent
   * starts: the landing's counterpart of the takeoff's climb, set the same way
   * by dragging it. A Return is flown at the vehicle's own heights: a
   * multicopter lands at the end of it, a fixed wing circles there.
   */
  function profileShape(points, kind, ret) {
    const x = [];
    const y = [];
    const marks = [];
    const at = (metres, height) => { x.push(metres / 1000); y.push(height); };
    points.forEach((station, index) => {
      const before = index > 0 ? points[index - 1] : null;
      const after = index + 1 < points.length ? points[index + 1] : null;
      const mark = Object.assign({ number: index }, station);

      if (kind === "fixed_wing" && station.type === "takeoff" && before) {
        mark.distance = climbOutEnd(before, station, after);
        at(mark.distance, station.alt);
      } else if (kind === "multirotor" && station.type === "land" && before) {
        const from = station.approach != null ? station.approach : before.alt;
        at(station.distance, from);
        marks.push({ id: station.id, type: "descent", number: index, distance: station.distance, alt: from });
        at(station.distance, station.alt);
      } else if (station.type === "rtl" && before && (kind === "multirotor" || kind === "fixed_wing")) {
        const path = returnPath(before, station, kind, ret);
        path.forEach(([distance, height]) => at(distance, height));
        mark.alt = path[path.length - 1][1];
      } else {
        at(station.distance, station.alt);
      }
      marks.push(mark);
    });
    return { x, y, marks };
  }

  /** The line of the profile: see profileShape. */
  function profileLine(points, kind, ret) {
    const shape = profileShape(points, kind, ret);
    return { x: shape.x, y: shape.y };
  }

  /** The drawn and draggable points of the profile: see profileShape. */
  function profileMarks(points, kind, ret) {
    return profileShape(points, kind, ret).marks;
  }

  /** Where a fixed wing's climb-out reaches the takeoff height, in metres
   *  along the route. It climbs from where it stands along the first leg at
   *  the takeoff's pitch (or the airframe default) and can only get as far as
   *  the next point before the mission turns it there, so it stops at that. */
  function climbOutEnd(before, station, after) {
    const climb = takeoffClimb(before, station, after);
    if (!climb) return station.distance;
    return Math.max(station.distance, before.distance + Math.min(climb.run, climb.room));
  }

  /** A fixed wing's climb-out: {run, room, height, pitch}, the run it needs
   *  to reach the takeoff height and the room the first leg gives it, or null
   *  for a takeoff with nothing to climb. */
  function takeoffClimb(before, station, after) {
    const height = station.alt - before.alt;
    if (!(height > 0)) return null;
    const pitch = station.pitch > 0 ? station.pitch : TAKEOFF_PITCH_DEG;
    const run = height / Math.tan(pitch * Math.PI / 180);
    const room = after ? Math.max(0, after.distance - before.distance) : Infinity;
    return { run, room, height, pitch };
  }

  /** A Return as [metres along, metres up] after the point before it. With
   *  nothing read from the vehicle it is flown at the height it has, which is
   *  all the planner can know: the heights are the vehicle's, not the plan's. */
  function returnPath(before, station, kind, ret) {
    const known = !!(ret && ret.known);
    const pick = (...values) => values.find((value) => typeof value === "number" && isFinite(value));
    if (kind === "fixed_wing") {
      // Straight to the height it circles at. PX4 climbs first and circles
      // down, ArduPlane changes height on the way; both end up there.
      const circle = known ? pick(ret.arrive_at, ret.climb_to, before.alt) : before.alt;
      return [[station.distance, circle]];
    }
    const high = known && ret.climb_to != null ? Math.max(before.alt, ret.climb_to) : before.alt;
    const path = [];
    if (high > before.alt) path.push([before.distance, high]);
    path.push([station.distance, high]);
    if (known && ret.arrive_at != null && ret.arrive_at < high) path.push([station.distance, ret.arrive_at]);
    if (!(known && ret.hold)) path.push([station.distance, station.alt]);
    return path;
  }

  /** The glide into the first landing: {from, to, drop, run, degrees}, from
   *  the station before it down to it, or null when the plan has no landing
   *  or does not descend into one. A landing right under the point before it
   *  is 90 degrees, which is what it would be. */
  function landingApproach(points) {
    const index = points.findIndex((station) => station.type === "land");
    if (index < 1) return null;
    const from = points[index - 1];
    const to = points[index];
    const drop = from.alt - to.alt;
    if (!(drop > 0)) return null;
    const run = Math.max(0, to.distance - from.distance);
    const degrees = Math.atan2(drop, run) * 180 / Math.PI;
    return { from, to, drop, run, degrees };
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

  /** Metres east/north of *origin* and back, flat-earth. A pattern is a few
   *  hundred metres across, where that is a fraction of a metre off. */
  function localFrame(origin) {
    const k = EARTH_RADIUS_M * Math.PI / 180;
    const c = Math.max(0.01, Math.cos(origin.lat * Math.PI / 180));
    return {
      toXY: (p) => ({ x: (p.lon - origin.lon) * k * c, y: (p.lat - origin.lat) * k }),
      toLL: (q) => ({ lat: origin.lat + q.y / k, lon: origin.lon + q.x / (k * c) }),
    };
  }

  /** The back and forth sweep over a polygon of {x, y} metres: parallel passes
   *  *spacing* apart, along the polygon's longest edge, alternating direction.
   *  The spacing is shrunk a little so the passes divide the polygon evenly
   *  and the first and last run half a spacing inside the edge. */
  function sweepPolygon(ring, spacing) {
    if (!Array.isArray(ring) || ring.length < 3 || !(spacing > 0)) return [];
    let angle = 0;
    let longest = -1;
    ring.forEach((a, i) => {
      const b = ring[(i + 1) % ring.length];
      const len = Math.hypot(b.x - a.x, b.y - a.y);
      if (len > longest) { longest = len; angle = Math.atan2(b.y - a.y, b.x - a.x); }
    });
    const cos = Math.cos(angle);
    const sin = Math.sin(angle);
    const rot = ring.map((p) => ({ x: p.x * cos + p.y * sin, y: -p.x * sin + p.y * cos }));
    const lo = Math.min(...rot.map((p) => p.y));
    const hi = Math.max(...rot.map((p) => p.y));
    if (!(hi - lo > 0)) return [];
    const lines = Math.max(1, Math.ceil((hi - lo) / spacing));
    const step = (hi - lo) / lines;
    const out = [];
    for (let k = 0; k < lines; k += 1) {
      const y = lo + (k + 0.5) * step;
      const xs = [];
      rot.forEach((a, i) => {
        const b = rot[(i + 1) % rot.length];
        if ((a.y <= y && b.y > y) || (b.y <= y && a.y > y)) {
          xs.push(a.x + ((y - a.y) / (b.y - a.y)) * (b.x - a.x));
        }
      });
      xs.sort((u, v) => u - v);
      let runs = [];
      for (let i = 0; i + 1 < xs.length; i += 2) {
        if (xs[i + 1] - xs[i] > 0.01) runs.push([xs[i], xs[i + 1]]);
      }
      if (k % 2) runs = runs.reverse().map(([u, v]) => [v, u]);
      runs.forEach(([u, v]) => { out.push({ x: u, y }, { x: v, y }); });
    }
    return out.map((p) => ({ x: p.x * cos - p.y * sin, y: p.x * sin + p.y * cos }));
  }

  /** The sweep along a corridor: passes parallel to the centre line {x, y}[],
   *  spread across *width* metres, *spacing* apart, alternating direction. */
  function sweepCorridor(path, width, spacing) {
    const line = [];
    (path || []).forEach((p) => {
      const last = line[line.length - 1];
      if (!last || Math.hypot(p.x - last.x, p.y - last.y) > 0.5) line.push(p);
    });
    if (line.length < 2 || !(width > 0) || !(spacing > 0)) return [];
    const normals = [];
    for (let i = 0; i < line.length - 1; i += 1) {
      const dx = line[i + 1].x - line[i].x;
      const dy = line[i + 1].y - line[i].y;
      const len = Math.hypot(dx, dy);
      normals.push({ x: -dy / len, y: dx / len });
    }
    const vertex = line.map((p, i) => {
      const a = normals[Math.max(0, i - 1)];
      const b = normals[Math.min(normals.length - 1, i)];
      let nx = a.x + b.x;
      let ny = a.y + b.y;
      const len = Math.hypot(nx, ny);
      if (len < 1e-6) return { n: a, scale: 1 };
      nx /= len; ny /= len;
      const dot = nx * a.x + ny * a.y;
      return { n: { x: nx, y: ny }, scale: Math.min(PATTERN_MITER_LIMIT, 1 / Math.max(dot, 1e-6)) };
    });
    const lanes = Math.max(1, Math.ceil(width / spacing));
    const out = [];
    for (let j = 0; j < lanes; j += 1) {
      const d = -width / 2 + (j + 0.5) * (width / lanes);
      const pass = line.map((p, i) => ({
        x: p.x + vertex[i].n.x * d * vertex[i].scale,
        y: p.y + vertex[i].n.y * d * vertex[i].scale,
      }));
      if (j % 2) pass.reverse();
      out.push(...pass);
    }
    return out;
  }

  /** The waypoints that cover a pattern, as {lat, lon}[].
   *  *kind* is a PATTERNS id; *pts* what the operator clicked: centre and edge
   *  for a circle, the centre line for a corridor, the corners for an area. */
  function patternWaypoints(kind, pts, opts) {
    const o = opts || {};
    const spacing = clampNumber(o.spacing, PATTERN_SPACING_M.min, PATTERN_SPACING_M.max, PATTERN_SPACING_M.def);
    const width = clampNumber(o.width, PATTERN_WIDTH_M.min, PATTERN_WIDTH_M.max, PATTERN_WIDTH_M.def);
    if (!Array.isArray(pts) || !pts.length) return [];
    const frame = localFrame(pts[0]);
    const xy = pts.map(frame.toXY);
    let path = [];
    if (kind === "pattern_circle") {
      if (xy.length < 2) return [];
      const radius = Math.max(PATTERN_CIRCLE_MIN_M, Math.hypot(xy[1].x, xy[1].y));
      const start = Math.atan2(xy[1].y, xy[1].x);
      const count = Math.round(clampNumber(
        (2 * Math.PI * radius) / spacing, PATTERN_CIRCLE_POINTS.min, PATTERN_CIRCLE_POINTS.max,
        PATTERN_CIRCLE_POINTS.max));
      for (let i = 0; i <= count; i += 1) {
        const a = start - (i / count) * 2 * Math.PI;
        path.push({ x: radius * Math.cos(a), y: radius * Math.sin(a) });
      }
    } else if (kind === "pattern_corridor") {
      path = sweepCorridor(xy, width, spacing);
    } else if (kind === "pattern_area") {
      path = sweepPolygon(xy, spacing);
    }
    const out = [];
    path.forEach((p) => {
      const ll = frame.toLL(p);
      const last = out[out.length - 1];
      if (!last || distanceM(last, ll) > 0.5) out.push(ll);
    });
    return out;
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


  return {
    useTypes(types) { TYPES = types; },
    NOMINAL_SPEED_MS,
    TAKEOFF_PITCH_DEG,
    EARTH_RADIUS_M,
    PATTERN_SPACING_M,
    PATTERN_WIDTH_M,
    PATTERN_CIRCLE_MIN_M,
    PATTERN_CIRCLE_SEGMENTS,
    PATTERN_CIRCLE_POINTS,
    PATTERN_MITER_LIMIT,
    distanceM,
    lerpPoint,
    stations,
    profileShape,
    profileLine,
    profileMarks,
    climbOutEnd,
    takeoffClimb,
    returnPath,
    landingApproach,
    routeLength,
    speedByItem,
    routeDuration,
    circleRing,
    localFrame,
    sweepPolygon,
    sweepCorridor,
    patternWaypoints,
    clampNumber,
    GROUNDED,
    clearances,
    interpolateAt,
  };
})();
