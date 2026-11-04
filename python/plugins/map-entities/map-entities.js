"use strict";
window.Corvus = window.Corvus || {};

/**
 * Map Entities: a tactical layer on the Home map.
 *
 * Things the operator places by hand, at an exact coordinate:
 *
 *   gate     two posts and the opening between them (a height above the
 *            takeoff point). The plugin watches the aircraft's track and says
 *            whether it flew through the opening, over it or under it.
 *   tree     a position, a crown radius and a height. Flagged when the
 *            aircraft enters the crown below the top.
 *   symbols  a person, a vehicle, an explosion, a radar, a camera and so on,
 *            each with an optional radius drawn as a light area (a radar's
 *            reach, a blast's danger zone). The area is a full circle or a
 *            sector: an angle and the direction it faces (0 is north).
 *            Entering one can be flagged.
 *
 * What is placed is saved through api.saveSettings (the plugin's own
 * config.json). What the aircraft did (passes, entries) is kept for the
 * session only; "Reset results" starts it again.
 *
 * Geometry is done in metres on a flat plane around each entity, which is far
 * closer than a hand placed coordinate is to the truth.
 */
Corvus.pluginMapEntities = (function () {
  const PLUGIN_ID = "map-entities";
  const MAX_ENTITIES = 500;
  const MAX_RESULTS = 50;
  const NAME_MAX = 60;
  const NOTE_MAX = 300;
  // A jump this long between two samples is a lost link or a new fix, not
  // flight: the segment between them proves nothing.
  const MAX_STEP_M = 500;
  const MIN_STEP_M = 0.3;
  const METRES_PER_DEGREE = Math.PI / 180 * 6378137;

  const PASS_COLOR = "#7BD389";
  const MISS_COLOR = "#FF9F43";
  const HIT_COLOR = "#FF5C5C";

  const COLORS = [
    { id: "lime", label: "Green", color: "#7BD389" },
    { id: "darkgreen", label: "Dark green", color: "#1E8E3E" },
    { id: "cyan", label: "Cyan", color: "#2BC4E4" },
    { id: "blue", label: "Blue", color: "#5B8DEF" },
    { id: "magenta", label: "Pink", color: "#C77DFF" },
    { id: "yellow", label: "Yellow", color: "#FFD60A" },
    { id: "orange", label: "Orange", color: "#FF9F43" },
    { id: "red", label: "Red", color: "#FF5C5C" },
    { id: "white", label: "White", color: "#FFFFFF" },
  ];
  const COLOR_RE = /^#[0-9A-Fa-f]{6}$/;

  // `icon` names a symbol of the map api (api.map.icons()).
  const KINDS = [
    { id: "gate", label: "Gate", shape: "gate", color: "#FFD60A", base: 0, height: 5, icon: "gate" },
    { id: "tree", label: "Tree", shape: "point", color: "#7BD389", radius: 3, height: 12, alert: true, icon: "tree" },
    { id: "radar", label: "Radar", shape: "point", color: "#5B8DEF", radius: 500, alert: true, icon: "radar" },
    { id: "camera", label: "Camera", shape: "point", color: "#2BC4E4", radius: 150, alert: true, icon: "camera" },
    { id: "antenna", label: "Antenna", shape: "point", color: "#C77DFF", radius: 300, icon: "antenna" },
    { id: "person", label: "Person", shape: "point", color: "#1E8E3E", radius: 0, icon: "person" },
    { id: "vehicle", label: "Vehicle", shape: "point", color: "#FFFFFF", radius: 0, icon: "vehicle" },
    { id: "building", label: "Building", shape: "point", color: "#FFFFFF", radius: 0, icon: "building" },
    { id: "explosion", label: "Explosion", shape: "point", color: "#FF5C5C", radius: 50, alert: true, icon: "explosion" },
    { id: "fire", label: "Fire", shape: "point", color: "#FF9F43", radius: 30, alert: true, icon: "fire" },
    { id: "hazard", label: "Hazard", shape: "point", color: "#FFD60A", radius: 0, icon: "hazard" },
    { id: "target", label: "Target", shape: "point", color: "#FF5C5C", radius: 0, icon: "target" },
    { id: "pad", label: "Landing pad", shape: "point", color: "#5B8DEF", radius: 0, icon: "pad" },
    { id: "flag", label: "Flag", shape: "point", color: "#2BC4E4", radius: 0, icon: "flag" },
  ];
  const KIND_BY_ID = {};
  KINDS.forEach((k) => { KIND_BY_ID[k.id] = k; });

  // ---- numbers and text -----------------------------------------------------

  function clamp(value, lo, hi) {
    return Math.min(Math.max(value, lo), hi);
  }

  function finite(value, fallback) {
    const n = typeof value === "string" && value.trim() === "" ? NaN : Number(value);
    return isFinite(n) ? n : fallback;
  }

  function validPoint(lng, lat) {
    return isFinite(lng) && isFinite(lat) && Math.abs(lat) <= 90 && Math.abs(lng) <= 180;
  }

  function plainText(value, max) {
    return String(value == null ? "" : value).replace(/\s+/g, " ").trim().slice(0, max);
  }

  /**
   * One angle in decimal degrees from what an operator typed: a decimal comma
   * is fine. Null for anything that is not a number inside +-limit.
   * @param {string|number} text
   * @param {number} limit 90 for a latitude, 180 for a longitude
   * @returns {?number}
   */
  function parseDegrees(text, limit) {
    const n = Number(String(text).trim().replace(",", "."));
    if (String(text).trim() === "" || !isFinite(n) || Math.abs(n) > limit) return null;
    return n;
  }

  /**
   * "48.1234, 11.5678" as pasted from a map: latitude first, then longitude,
   * separated by a comma, a semicolon or white space. After a semicolon the
   * numbers may use a decimal comma. Null unless there are exactly two.
   * @param {string} text
   * @returns {?{lat: number, lng: number}}
   */
  function parseCoordinatePair(text) {
    const raw = String(text == null ? "" : text).trim();
    if (!raw) return null;
    const parts = raw.indexOf(";") >= 0 ? raw.split(";") : raw.split(/[,\s]+/);
    const tokens = parts.map((p) => p.trim()).filter(Boolean);
    if (tokens.length !== 2) return null;
    const lat = parseDegrees(tokens[0], 90);
    const lng = parseDegrees(tokens[1], 180);
    return lat == null || lng == null ? null : { lat, lng };
  }

  function formatDegrees(value) {
    return Number(value).toFixed(7).replace(/0+$/, "").replace(/\.$/, ".0");
  }

  // ---- geometry (pure) ------------------------------------------------------

  /**
   * [lng, lat] as east/north metres around `origin` ([lng, lat]).
   * @returns {[number, number]}
   */
  function toLocal(origin, point) {
    return [
      (point[0] - origin[0]) * METRES_PER_DEGREE * Math.cos(origin[1] * Math.PI / 180),
      (point[1] - origin[1]) * METRES_PER_DEGREE,
    ];
  }

  /** Metres between two [lng, lat] points. */
  function distanceM(a, b) {
    const d = toLocal(a, b);
    return Math.hypot(d[0], d[1]);
  }

  /** Compass bearing from a to b in degrees, 0 to 360. */
  function bearingDeg(a, b) {
    const d = toLocal(a, b);
    return (Math.atan2(d[0], d[1]) * 180 / Math.PI + 360) % 360;
  }

  function cross(a, b) {
    return a[0] * b[1] - a[1] * b[0];
  }

  /**
   * Where the aircraft's step from `prev` to `cur` crosses a gate's line.
   *
   * `u` is how far along the gate from post A (0 to 1), `t` how far along the
   * step. A step that ends exactly on the line belongs to this step, not the
   * next, so one crossing is never counted twice. Null when the step does not
   * cross the line between the posts.
   *
   * @param {{lng: number, lat: number, lng2: number, lat2: number}} gate
   * @param {{lng: number, lat: number, alt: number}} prev
   * @param {{lng: number, lat: number, alt: number}} cur
   * @returns {?{u: number, t: number, offset: number, alt: number}}
   */
  function gateCrossing(gate, prev, cur) {
    const origin = [gate.lng, gate.lat];
    const r = toLocal(origin, [gate.lng2, gate.lat2]);
    const p = toLocal(origin, [prev.lng, prev.lat]);
    const q = toLocal(origin, [cur.lng, cur.lat]);
    const d = [q[0] - p[0], q[1] - p[1]];
    const denom = cross(r, d);
    if (Math.abs(denom) < 1e-9) return null;
    const u = cross(p, d) / denom;
    const t = cross(p, r) / denom;
    if (u < 0 || u > 1 || t <= 0 || t > 1) return null;
    const width = Math.hypot(r[0], r[1]);
    const a0 = finite(prev.alt, 0);
    const a1 = finite(cur.alt, 0);
    return { u, t, offset: (u - 0.5) * width, alt: a0 + t * (a1 - a0) };
  }

  /**
   * Whether a crossing went through the opening: the altitude lies between
   * the gate's base and base + height (metres above the takeoff point).
   * @returns {{ok: boolean, reason: string}} reason is "", "above" or "below"
   */
  function gateVerdict(gate, crossing) {
    const base = finite(gate.base, 0);
    const top = base + finite(gate.height, 0);
    if (crossing.alt > top) return { ok: false, reason: "above" };
    if (crossing.alt < base) return { ok: false, reason: "below" };
    return { ok: true, reason: "" };
  }

  /**
   * How close the step from `prev` to `cur` came to a zone's centre.
   * @param {{lng: number, lat: number, radius: number}} zone
   * @returns {{touched: boolean, dist: number, alt: number, inside: boolean}}
   *   touched: the step enters the circle somewhere; alt: altitude where it
   *   came closest; inside: the step's end is in the circle
   */
  function zoneContact(zone, prev, cur) {
    if (sectorAngle(zone) < 360) return sectorContact(zone, prev, cur);
    const origin = [zone.lng, zone.lat];
    const p = toLocal(origin, [prev.lng, prev.lat]);
    const q = toLocal(origin, [cur.lng, cur.lat]);
    const d = [q[0] - p[0], q[1] - p[1]];
    const len2 = d[0] * d[0] + d[1] * d[1];
    const t = len2 > 0 ? clamp(-(p[0] * d[0] + p[1] * d[1]) / len2, 0, 1) : 1;
    const dist = Math.hypot(p[0] + t * d[0], p[1] + t * d[1]);
    const a0 = finite(prev.alt, 0);
    const a1 = finite(cur.alt, 0);
    return {
      touched: dist <= zone.radius,
      dist,
      alt: a0 + t * (a1 - a0),
      inside: Math.hypot(q[0], q[1]) <= zone.radius,
    };
  }

  /** The angle of a zone's area in degrees: 360 for a full circle. A tree's
   *  crown is always round. */
  function sectorAngle(zone) {
    if (zone.kind === "tree") return 360;
    return clamp(finite(zone.angle, 360), 1, 360);
  }

  /**
   * Whether a point, in metres east and north of the zone's centre, lies in
   * the zone's area: inside the radius and, for a sector, within half its
   * angle either side of the direction it faces. The centre always counts.
   * @param {Object} zone {radius, angle, bearing}
   * @param {[number, number]} local
   * @returns {boolean}
   */
  function inSector(zone, local) {
    const dist = Math.hypot(local[0], local[1]);
    if (dist > zone.radius) return false;
    const angle = sectorAngle(zone);
    if (angle >= 360 || dist === 0) return true;
    const toPoint = (Math.atan2(local[0], local[1]) * 180 / Math.PI + 360) % 360;
    const off = Math.abs(((toPoint - finite(zone.bearing, 0) + 540) % 360) - 180);
    return off <= angle / 2;
  }

  /**
   * zoneContact for a sector. The step is walked in short pieces rather than
   * solved exactly: the edges of a sector make the exact answer a good deal
   * more code than the walk is worth at the speeds an aircraft flies.
   */
  function sectorContact(zone, prev, cur) {
    const origin = [zone.lng, zone.lat];
    const p = toLocal(origin, [prev.lng, prev.lat]);
    const q = toLocal(origin, [cur.lng, cur.lat]);
    const d = [q[0] - p[0], q[1] - p[1]];
    const len = Math.hypot(d[0], d[1]);
    const a0 = finite(prev.alt, 0);
    const a1 = finite(cur.alt, 0);
    const pace = Math.max(0.5, Math.min(zone.radius / 30, 5));
    const n = clamp(Math.ceil(len / pace), 1, 400);
    let touched = false;
    let alt = a1;
    let dist = Infinity;
    for (let i = 0; i <= n; i++) {
      const t = i / n;
      const at = [p[0] + t * d[0], p[1] + t * d[1]];
      dist = Math.min(dist, Math.hypot(at[0], at[1]));
      if (!touched && inSector(zone, at)) {
        touched = true;
        alt = a0 + t * (a1 - a0);
      }
    }
    return { touched, dist, alt, inside: inSector(zone, q) };
  }

  /**
   * The corners of a sector as a polygon ring: the centre, then the arc.
   * Not closed (the map closes it).
   * @param {{lng: number, lat: number, radius: number, angle: number, bearing: number}} zone
   * @returns {Array<[number, number]>}
   */
  function sectorRing(zone) {
    const angle = sectorAngle(zone);
    const start = finite(zone.bearing, 0) - angle / 2;
    const steps = Math.max(2, Math.ceil(angle / 5));
    const cos = Math.max(Math.cos(zone.lat * Math.PI / 180), 0.01);
    const ring = [[zone.lng, zone.lat]];
    for (let i = 0; i <= steps; i++) {
      const theta = (start + angle * i / steps) * Math.PI / 180;
      ring.push([
        zone.lng + zone.radius * Math.sin(theta) / (METRES_PER_DEGREE * cos),
        zone.lat + zone.radius * Math.cos(theta) / METRES_PER_DEGREE,
      ]);
    }
    return ring;
  }

  // ---- the entities ---------------------------------------------------------

  let idSeq = 0;
  function newId() {
    idSeq += 1;
    return "e" + Date.now().toString(36) + idSeq.toString(36) + Math.floor(Math.random() * 1296).toString(36);
  }

  function nextName(entities, kind) {
    let n = 0;
    entities.forEach((e) => {
      const m = e.kind === kind.id && new RegExp("^" + kind.label + " (\\d+)$").exec(e.name);
      if (m) n = Math.max(n, Number(m[1]));
    });
    return kind.label + " " + (n + 1);
  }

  /**
   * A new entity with its kind's defaults.
   * @param {string} kindId
   * @param {[number, number]} point [lng, lat]
   * @param {[number, number]} [second] a gate's second post
   * @param {Array} [existing] the entities so far, to number the name
   * @returns {?Object}
   */
  function makeEntity(kindId, point, second, existing) {
    const kind = KIND_BY_ID[kindId];
    if (!kind || !Array.isArray(point) || !validPoint(point[0], point[1])) return null;
    const e = {
      id: newId(),
      kind: kind.id,
      name: nextName(existing || [], kind),
      color: kind.color,
      lng: point[0],
      lat: point[1],
      note: "",
    };
    if (kind.shape === "gate") {
      if (!Array.isArray(second) || !validPoint(second[0], second[1])) return null;
      e.lng2 = second[0];
      e.lat2 = second[1];
      e.base = kind.base;
      e.height = kind.height;
    } else {
      e.radius = kind.radius || 0;
      e.alert = !!kind.alert;
      if (kind.id === "tree") e.height = kind.height;
      else { e.angle = 360; e.bearing = 0; }
    }
    return e;
  }

  /**
   * One entity as saved, made safe to use: unknown kinds and unusable
   * coordinates are dropped (null), numbers are held to sane ranges, text is
   * plain and short. The settings file is the operator's to edit by hand.
   * @param {*} raw
   * @returns {?Object}
   */
  function normalizeEntity(raw) {
    if (!raw || typeof raw !== "object") return null;
    const kind = KIND_BY_ID[raw.kind];
    if (!kind) return null;
    const lng = finite(raw.lng, NaN);
    const lat = finite(raw.lat, NaN);
    if (!validPoint(lng, lat)) return null;
    const id = typeof raw.id === "string" && /^[A-Za-z0-9_-]{1,24}$/.test(raw.id) ? raw.id : newId();
    const e = {
      id,
      kind: kind.id,
      name: plainText(raw.name, NAME_MAX) || kind.label,
      color: typeof raw.color === "string" && COLOR_RE.test(raw.color) ? raw.color : kind.color,
      lng,
      lat,
      note: plainText(raw.note, NOTE_MAX),
    };
    if (kind.shape === "gate") {
      const lng2 = finite(raw.lng2, NaN);
      const lat2 = finite(raw.lat2, NaN);
      if (!validPoint(lng2, lat2)) return null;
      e.lng2 = lng2;
      e.lat2 = lat2;
      e.base = clamp(finite(raw.base, kind.base), -1000, 10000);
      e.height = clamp(finite(raw.height, kind.height), 0, 10000);
    } else {
      e.radius = clamp(finite(raw.radius, kind.radius || 0), 0, 100000);
      e.alert = raw.alert == null ? !!kind.alert : !!raw.alert;
      if (kind.id === "tree") e.height = clamp(finite(raw.height, kind.height), 0, 1000);
      else {
        e.angle = clamp(finite(raw.angle, 360), 1, 360);
        e.bearing = ((finite(raw.bearing, 0) % 360) + 360) % 360;
      }
    }
    return e;
  }

  function normalizeList(list) {
    if (!Array.isArray(list)) return [];
    const seen = new Set();
    const out = [];
    list.slice(0, MAX_ENTITIES).forEach((raw) => {
      const e = normalizeEntity(raw);
      if (!e) return;
      while (seen.has(e.id)) e.id = newId();
      seen.add(e.id);
      out.push(e);
    });
    return out;
  }

  // ---- the session state ----------------------------------------------------

  const S = {
    api: null,
    entities: [],
    results: new Map(),    // id -> {passes, misses, entries, hits, inside, log}
    selected: null,
    placing: null,         // {kind, step, first, cancel}
    undo: null,            // {items: [{entity, index}], timer}
    prev: null,
    options: { labels: true, notify: true, armedOnly: true },
    ui: null,              // the open view's hooks, or null
    started: false,
  };

  function resultOf(id) {
    let r = S.results.get(id);
    if (!r) {
      r = { passes: 0, misses: 0, entries: 0, hits: 0, inside: false, log: [] };
      S.results.set(id, r);
    }
    return r;
  }

  function findEntity(id) {
    return S.entities.find((e) => e.id === id) || null;
  }

  function isGate(e) {
    return KIND_BY_ID[e.kind].shape === "gate";
  }

  function pushLog(r, line) {
    r.log.push(line);
    if (r.log.length > MAX_RESULTS) r.log.splice(0, r.log.length - MAX_RESULTS);
  }

  // ---- watching the aircraft ------------------------------------------------

  /**
   * One step of the aircraft against every entity. Updates the results and
   * returns what happened, for the caller to announce.
   *
   * @param {{lng: number, lat: number, alt: number}} prev
   * @param {{lng: number, lat: number, alt: number}} cur
   * @param {number} [now] clock for the log, ms
   * @returns {Array<{type: string, entity: Object, text: string, level: string}>}
   */
  function processStep(prev, cur, now) {
    const events = [];
    const stamp = now == null ? Date.now() : now;
    S.entities.forEach((e) => {
      const r = resultOf(e.id);
      if (isGate(e)) {
        const crossing = gateCrossing(e, prev, cur);
        if (!crossing) return;
        const verdict = gateVerdict(e, crossing);
        const entry = {
          t: stamp,
          ok: verdict.ok,
          reason: verdict.reason,
          alt: crossing.alt,
          offset: crossing.offset,
        };
        pushLog(r, entry);
        if (verdict.ok) {
          r.passes += 1;
          events.push({ type: "pass", entity: e, level: "info", text: e.name + ": flown through" });
        } else {
          r.misses += 1;
          events.push({
            type: "miss", entity: e, level: "warning",
            text: e.name + ": crossed " + (verdict.reason === "above" ? "above" : "below") + " the opening",
          });
        }
        return;
      }
      if (!(e.radius > 0)) return;
      const contact = zoneContact(e, prev, cur);
      const isTree = e.kind === "tree";
      const low = !isTree || contact.alt <= finite(e.height, 0);
      const touching = contact.touched && low;
      if (touching && !r.inside) {
        r.entries += 1;
        if (isTree) r.hits += 1;
        pushLog(r, { t: stamp, ok: !isTree, alt: contact.alt, offset: contact.dist, reason: "" });
        if (e.alert) {
          events.push({
            type: isTree ? "hit" : "enter", entity: e,
            level: isTree ? "warning" : "info",
            text: isTree ? e.name + ": aircraft is inside the crown, below the top" : e.name + ": aircraft entered the area",
          });
        }
      }
      r.inside = contact.inside && (!isTree || finite(cur.alt, 0) <= finite(e.height, 0));
    });
    return events;
  }

  /**
   * The aircraft's position out of a telemetry state, or null while there is
   * nothing to measure: not connected, no fix, or (when asked) not armed.
   * @returns {?{lng: number, lat: number, alt: number}}
   */
  function sampleOf(state, armedOnly) {
    const s = state || {};
    if (!s.connected || !Array.isArray(s.position)) return null;
    if (armedOnly && !s.armed) return null;
    const lng = Number(s.position[0]);
    const lat = Number(s.position[1]);
    if (!validPoint(lng, lat) || (lng === 0 && lat === 0)) return null;
    return { lng, lat, alt: finite(s.altitude_agl, 0) };
  }

  function announce(events) {
    if (!events.length) return;
    if (S.options.notify && S.api && typeof S.api.notification === "function") {
      events.forEach((ev) => S.api.notification(ev.level, ev.text));
    }
    events.forEach((ev) => drawEntity(ev.entity));
    refreshView();
  }

  function onState(state) {
    const cur = sampleOf(state, S.options.armedOnly);
    if (!cur) {
      S.prev = null;
      return;
    }
    const prev = S.prev;
    if (prev) {
      const step = distanceM([prev.lng, prev.lat], [cur.lng, cur.lat]);
      if (step > MAX_STEP_M) {
        S.prev = cur;
        return;
      }
      if (step < MIN_STEP_M && Math.abs(cur.alt - prev.alt) < MIN_STEP_M) return;
      announce(processStep(prev, cur));
    }
    S.prev = cur;
  }

  function resetResults() {
    S.results.clear();
    S.entities.forEach(drawEntity);
    refreshView();
  }

  // ---- drawing on the map ---------------------------------------------------

  function mapApi() {
    return S.api && S.api.map ? S.api.map : null;
  }

  function statusColor(e) {
    const r = S.results.get(e.id);
    if (!r) return e.color;
    if (isGate(e)) {
      if (r.passes > 0) return PASS_COLOR;
      return r.misses > 0 ? MISS_COLOR : e.color;
    }
    if (e.kind === "tree" && r.hits > 0) return HIT_COLOR;
    return r.inside ? MISS_COLOR : e.color;
  }

  const KEY_SUFFIXES = ["l", "a", "b", "m", "c"];

  function undrawEntity(id) {
    const m = mapApi();
    if (!m) return;
    KEY_SUFFIXES.forEach((s) => m.remove(id + "-" + s));
  }

  function moveHandler(e, which) {
    return (point) => {
      if (which === "b") { e.lng2 = point[0]; e.lat2 = point[1]; } else { e.lng = point[0]; e.lat = point[1]; }
      drawEntity(e);
      save();
      renderDetail();
    };
  }

  /** Draw one entity from what is stored. Takes away what its kind has no use for. */
  function drawEntity(e) {
    const m = mapApi();
    if (!m || !e) return;
    const kind = KIND_BY_ID[e.kind];
    const color = statusColor(e);
    const selected = S.selected === e.id;
    const label = S.options.labels ? e.name : "";
    const select = () => selectEntity(e.id);
    if (kind.shape === "gate") {
      m.remove(e.id + "-c");
      m.drawLine(e.id + "-l", [[e.lng, e.lat], [e.lng2, e.lat2]], { color, width: 5 });
      const post = (suffix, point, which) => m.drawMarker(e.id + "-" + suffix, point, {
        color, size: 18, selected, draggable: selected, onClick: select,
        title: e.name + (which === "a" ? ", post A" : ", post B"),
        onDrag: (p) => {
          const a = which === "a" ? p : [e.lng, e.lat];
          const b = which === "b" ? p : [e.lng2, e.lat2];
          m.drawLine(e.id + "-l", [a, b], { color, width: 5 });
        },
        onDragEnd: moveHandler(e, which),
      });
      post("a", [e.lng, e.lat], "a");
      post("b", [e.lng2, e.lat2], "b");
      m.drawMarker(e.id + "-m", [(e.lng + e.lng2) / 2, (e.lat + e.lat2) / 2], {
        icon: kind.icon, color, label, selected, onClick: select, title: e.name,
      });
      return;
    }
    ["l", "a", "b"].forEach((s) => m.remove(e.id + "-" + s));
    if (e.radius > 0) {
      const style = { color, fillOpacity: 0.14, width: 2.5, opacity: 1, dashed: e.kind !== "tree" };
      if (sectorAngle(e) < 360) m.drawPolygon(e.id + "-c", sectorRing(e), style);
      else m.drawCircle(e.id + "-c", [e.lng, e.lat], e.radius, style);
    } else {
      m.remove(e.id + "-c");
    }
    m.drawMarker(e.id + "-m", [e.lng, e.lat], {
      icon: kind.icon, color, label, selected, draggable: selected, onClick: select, title: e.name,
      size: e.kind === "tree" ? 24 : 28,
      onDragEnd: moveHandler(e, "a"),
    });
  }

  function drawAll() {
    S.entities.forEach(drawEntity);
  }

  function pointsOf(e) {
    if (isGate(e)) return [[e.lng, e.lat], [e.lng2, e.lat2]];
    const reach = Math.max(e.radius || 0, 20);
    const dLat = reach / METRES_PER_DEGREE;
    const dLng = reach / (METRES_PER_DEGREE * Math.max(Math.cos(e.lat * Math.PI / 180), 0.01));
    return [[e.lng - dLng, e.lat - dLat], [e.lng + dLng, e.lat + dLat]];
  }

  // ---- saving ---------------------------------------------------------------

  function save() {
    if (!S.api || typeof S.api.saveSettings !== "function") return;
    try {
      Promise.resolve(S.api.saveSettings({ entities: S.entities })).catch(() => {});
    } catch (_e) { /* a missing backend must not break placing */ }
  }

  // ---- editing --------------------------------------------------------------

  function selectEntity(id) {
    const before = S.selected;
    S.selected = id && findEntity(id) ? id : null;
    [before, S.selected].forEach((x) => { const e = x && findEntity(x); if (e) drawEntity(e); });
    renderList();
    renderDetail();
  }

  function addEntity(e) {
    if (!e || S.entities.length >= MAX_ENTITIES) return false;
    S.entities.push(e);
    S.selected = e.id;
    S.entities.forEach((x) => { if (x.id !== e.id) drawEntity(x); });
    drawEntity(e);
    save();
    refreshAll();
    return true;
  }

  const UNDO_MS = 8000;

  function clearUndo() {
    if (S.undo) clearTimeout(S.undo.timer);
    S.undo = null;
  }

  /** Keep what was just deleted for a few seconds, so a slip costs a click. */
  function rememberDeleted(items) {
    clearUndo();
    const timer = setTimeout(() => { S.undo = null; renderUndo(); }, UNDO_MS);
    S.undo = { items, timer };
    renderUndo();
  }

  function removeEntity(id) {
    const i = S.entities.findIndex((e) => e.id === id);
    if (i < 0) return false;
    const [entity] = S.entities.splice(i, 1);
    S.results.delete(id);
    undrawEntity(id);
    if (S.selected === id) S.selected = null;
    save();
    refreshAll();
    rememberDeleted([{ entity, index: i }]);
    return true;
  }

  function removeAll() {
    if (!S.entities.length) return;
    const items = S.entities.map((entity, index) => ({ entity, index }));
    S.entities.forEach((e) => undrawEntity(e.id));
    S.entities = [];
    S.results.clear();
    S.selected = null;
    save();
    refreshAll();
    rememberDeleted(items);
  }

  /** Put back what the last delete took, where it was. */
  function undoDelete() {
    const u = S.undo;
    if (!u) return false;
    clearUndo();
    u.items.slice().sort((a, b) => a.index - b.index).forEach((item) => {
      S.entities.splice(Math.min(item.index, S.entities.length), 0, item.entity);
      S.selected = item.entity.id;
    });
    S.entities.forEach(drawEntity);
    save();
    refreshAll();
    renderUndo();
    return true;
  }

  /** Apply a change to one entity and redraw it. */
  function updateEntity(e, patch) {
    Object.assign(e, patch);
    drawEntity(e);
    save();
    renderList();
  }

  // ---- placing --------------------------------------------------------------

  const PENDING_KEY = "pending";

  function clearPending() {
    const m = mapApi();
    if (m) m.remove(PENDING_KEY);
  }

  function stopPlacing() {
    const p = S.placing;
    S.placing = null;
    if (p && typeof p.cancel === "function") p.cancel();
    clearPending();
    renderHint();
    renderPalette();
  }

  function armPick() {
    const p = S.placing;
    const m = mapApi();
    if (!p || !m) return;
    const cancel = m.pickPoint(onPick, {
      onCancel: () => { S.placing = null; clearPending(); renderHint(); renderPalette(); },
    });
    if (!cancel) {
      S.placing = null;
      if (S.api && S.api.notification) S.api.notification("warning", "The map is not ready yet");
      renderHint();
      renderPalette();
      return;
    }
    p.cancel = cancel;
  }

  /**
   * Start placing a kind. A gate takes two clicks (its posts), anything else
   * one. It keeps going, so a row of trees is a row of clicks, until Escape,
   * a right click or Done.
   */
  function startPlacing(kindId) {
    if (!KIND_BY_ID[kindId]) return;
    stopPlacing();
    S.placing = { kind: kindId, step: 0, first: null, cancel: null };
    armPick();
    renderHint();
    renderPalette();
  }

  function onPick(point) {
    const p = S.placing;
    if (!p) return;
    const kind = KIND_BY_ID[p.kind];
    if (kind.shape === "gate" && p.step === 0) {
      p.first = point;
      p.step = 1;
      const m = mapApi();
      if (m) m.drawMarker(PENDING_KEY, point, { color: kind.color, size: 18 });
      armPick();
      renderHint();
      return;
    }
    const e = makeEntity(p.kind, p.first || point, p.first ? point : null, S.entities);
    clearPending();
    p.step = 0;
    p.first = null;
    if (e) addEntity(e);
    armPick();
    renderHint();
  }

  /** Face a sector towards the next click. */
  function aimOnMap(e) {
    stopPlacing();
    const m = mapApi();
    if (!m) return;
    m.pickPoint((point) => {
      updateEntity(e, { bearing: Math.round(bearingDeg([e.lng, e.lat], point)) % 360 });
      renderDetail();
    }, { onCancel: () => {} });
  }

  /** Move an entity's point (or a gate's post) to the next click. */
  function repositionOnMap(e, which) {
    stopPlacing();
    const m = mapApi();
    if (!m) return;
    m.pickPoint((point) => {
      const patch = which === "b" ? { lng2: point[0], lat2: point[1] } : { lng: point[0], lat: point[1] };
      updateEntity(e, patch);
      renderDetail();
    }, { onCancel: () => {} });
  }

  // ---- the view -------------------------------------------------------------

  function h(tag, cls, text) {
    const el = document.createElement(tag);
    if (cls) el.className = cls;
    if (text != null) el.textContent = text;
    return el;
  }

  function iconPaths(name) {
    const m = mapApi();
    const paths = m && typeof m.iconPaths === "function" ? m.iconPaths(name) : null;
    return paths || [];
  }

  function glyph(name, size) {
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "2");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("width", String(size || 16));
    svg.setAttribute("height", String(size || 16));
    iconPaths(name).forEach((d) => {
      const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
      path.setAttribute("d", d);
      svg.appendChild(path);
    });
    return svg;
  }

  /** A symbol on its own dark round ground, in the entity's colour: readable
   *  on the light theme and the dark one alike, like its mark on the map. */
  function badge(name, color, size) {
    const b = h("span", "ent-badge");
    b.style.color = color;
    b.appendChild(glyph(name, size || 14));
    return b;
  }

  function lucide(name, size) {
    const ui = window.Corvus && Corvus.ui;
    if (ui && typeof ui.icon === "function") return ui.icon(name, size || 14);
    return h("span");
  }

  function refreshIcons() {
    const ui = window.Corvus && Corvus.ui;
    if (ui && typeof ui.refreshIcons === "function") {
      try { ui.refreshIcons(); } catch (_e) { /* cosmetic */ }
    }
  }

  function iconButton(name, title, onClick, cls) {
    const b = h("button", "icon-btn" + (cls ? " " + cls : ""));
    b.type = "button";
    b.title = title;
    b.setAttribute("aria-label", title);
    b.appendChild(lucide(name, 14));
    b.addEventListener("click", (event) => {
      event.stopPropagation();
      onClick(event);
    });
    return b;
  }

  function field(label, control, wide) {
    const f = h("label", "field" + (wide ? " ent-wide" : ""));
    f.appendChild(h("span", "field-label", label));
    f.appendChild(control);
    return f;
  }

  function numberInput(value, spec, onCommit) {
    const input = h("input", "field-input field-input-mono");
    input.type = "number";
    input.step = String(spec.step || 1);
    if (spec.min != null) input.min = String(spec.min);
    if (spec.max != null) input.max = String(spec.max);
    input.value = String(Math.round(finite(value, 0) * 100) / 100);
    input.addEventListener("change", () => {
      const n = finite(input.value, NaN);
      if (!isFinite(n)) { input.value = String(value); return; }
      onCommit(clamp(n, spec.min == null ? -Infinity : spec.min, spec.max == null ? Infinity : spec.max));
    });
    return input;
  }

  function pointRow(e, which, caption) {
    const lngKey = which === "b" ? "lng2" : "lng";
    const latKey = which === "b" ? "lat2" : "lat";
    const wrap = h("div", "ent-point ent-wide");
    const lat = h("input", "field-input field-input-mono");
    const lng = h("input", "field-input field-input-mono");
    [lat, lng].forEach((i) => { i.type = "text"; i.inputMode = "decimal"; i.spellcheck = false; });
    lat.value = formatDegrees(e[latKey]);
    lng.value = formatDegrees(e[lngKey]);
    const commit = () => {
      const pair = parseCoordinatePair(lat.value);
      const nextLat = pair ? pair.lat : parseDegrees(lat.value, 90);
      const nextLng = pair ? pair.lng : parseDegrees(lng.value, 180);
      if (nextLat == null || nextLng == null) {
        lat.value = formatDegrees(e[latKey]);
        lng.value = formatDegrees(e[lngKey]);
        return;
      }
      updateEntity(e, { [latKey]: nextLat, [lngKey]: nextLng });
      lat.value = formatDegrees(nextLat);
      lng.value = formatDegrees(nextLng);
      renderDetail();
    };
    lat.addEventListener("change", commit);
    lng.addEventListener("change", commit);
    const head = h("div", "ent-point-head");
    head.appendChild(h("span", "field-label", caption));
    head.appendChild(iconButton("crosshair", "Place on the map", () => repositionOnMap(e, which)));
    wrap.appendChild(head);
    const pair = h("div", "ent-point-grid");
    pair.appendChild(field("Latitude", lat));
    pair.appendChild(field("Longitude", lng));
    wrap.appendChild(pair);
    return wrap;
  }

  function colorSelect(e) {
    const sel = h("select", "field-select");
    COLORS.forEach((c) => {
      const o = h("option", "", c.label);
      o.value = c.color;
      o.dataset.color = c.color;
      sel.appendChild(o);
    });
    if (!COLORS.some((c) => c.color.toLowerCase() === e.color.toLowerCase())) {
      const o = h("option", "", "Custom");
      o.value = e.color;
      o.dataset.color = e.color;
      sel.appendChild(o);
    }
    sel.value = e.color;
    sel.addEventListener("change", () => updateEntity(e, { color: sel.value }));
    return sel;
  }

  function stampText(t) {
    try { return new Date(t).toLocaleTimeString(); } catch (_e) { return ""; }
  }

  function metres(value) {
    return (Math.round(value * 10) / 10) + " m";
  }

  function logLine(e, entry) {
    const line = h("div", "ent-log-line" + (entry.ok ? "" : " is-bad"));
    line.appendChild(h("span", "ent-log-time", stampText(entry.t)));
    let what;
    if (isGate(e)) {
      what = (entry.ok ? "Through" : (entry.reason === "above" ? "Above" : "Below"))
        + ", " + metres(entry.alt) + " up, " + metres(Math.abs(entry.offset)) + " off centre";
    } else {
      what = "Entered, " + metres(entry.alt) + " up";
    }
    line.appendChild(h("span", "ent-log-text", what));
    return line;
  }

  function buildDetail(e) {
    const kind = KIND_BY_ID[e.kind];
    const box = h("div", "ent-detail");

    const head = h("div", "ent-detail-head");
    head.appendChild(badge(kind.icon, e.color, 16));
    const name = h("input", "field-input ent-detail-name");
    name.type = "text";
    name.maxLength = NAME_MAX;
    name.value = e.name;
    name.setAttribute("aria-label", "Name");
    name.addEventListener("change", () => updateEntity(e, { name: plainText(name.value, NAME_MAX) || kind.label }));
    head.appendChild(name);
    head.appendChild(iconButton("locate-fixed", "Show on the map", () => {
      const m = mapApi();
      if (m) m.fit(pointsOf(e));
    }));
    head.appendChild(iconButton("trash-2", "Delete", () => removeEntity(e.id), "ent-remove"));
    box.appendChild(head);

    const grid = h("div", "ent-grid");
    if (kind.shape === "gate") {
      grid.appendChild(pointRow(e, "a", "Post A"));
      grid.appendChild(pointRow(e, "b", "Post B"));
      grid.appendChild(field("Opening from (m above takeoff)",
        numberInput(e.base, { min: -1000, max: 10000, step: 0.5 }, (v) => updateEntity(e, { base: v }))));
      grid.appendChild(field("Opening height (m)",
        numberInput(e.height, { min: 0, max: 10000, step: 0.5 }, (v) => updateEntity(e, { height: v }))));
      const facts = h("div", "ent-facts ent-wide");
      const a = [e.lng, e.lat];
      const b = [e.lng2, e.lat2];
      [["Width", metres(distanceM(a, b))], ["Heading", Math.round(bearingDeg(a, b)) + "°"]].forEach((pair) => {
        const cell = h("div", "ent-cell");
        cell.appendChild(h("span", "ent-cell-label", pair[0]));
        cell.appendChild(h("span", "ent-cell-value", pair[1]));
        facts.appendChild(cell);
      });
      grid.appendChild(facts);
    } else {
      grid.appendChild(pointRow(e, "a", "Position"));
      grid.appendChild(field(e.kind === "tree" ? "Crown radius (m)" : "Radius (m, 0 for none)",
        numberInput(e.radius, { min: 0, max: 100000, step: e.radius >= 100 ? 10 : 1 }, (v) => updateEntity(e, { radius: v }))));
      if (e.kind === "tree") {
        grid.appendChild(field("Height (m)",
          numberInput(e.height, { min: 0, max: 1000, step: 0.5 }, (v) => updateEntity(e, { height: v }))));
      } else {
        grid.appendChild(field("Angle (degrees, 360 for a full circle)",
          numberInput(e.angle, { min: 1, max: 360, step: 5 }, (v) => { updateEntity(e, { angle: v }); renderDetail(); })));
        if (e.angle < 360) {
          const aim = h("div", "ent-aim");
          aim.appendChild(numberInput(e.bearing, { min: 0, max: 360, step: 5 }, (v) => updateEntity(e, { bearing: v % 360 })));
          aim.appendChild(iconButton("crosshair", "Aim on the map", () => aimOnMap(e)));
          grid.appendChild(field("Direction (degrees, 0 is north)", aim, true));
        }
      }
      const alert = h("label", "ent-check ent-wide");
      const box2 = h("input");
      box2.type = "checkbox";
      box2.checked = !!e.alert;
      box2.addEventListener("change", () => updateEntity(e, { alert: box2.checked }));
      alert.appendChild(box2);
      alert.appendChild(h("span", "", e.kind === "tree" ? "Warn when the aircraft is in the crown" : "Warn when the aircraft enters"));
      grid.appendChild(alert);
    }
    grid.appendChild(field("Colour", colorSelect(e)));
    const note = h("input", "field-input");
    note.type = "text";
    note.maxLength = NOTE_MAX;
    note.value = e.note || "";
    note.addEventListener("change", () => updateEntity(e, { note: plainText(note.value, NOTE_MAX) }));
    grid.appendChild(field("Note", note, true));
    box.appendChild(grid);

    const log = h("div", "ent-log");
    log.setAttribute("data-role", "log");
    box.appendChild(log);
    return box;
  }

  function renderLog() {
    const ui = S.ui;
    if (!ui || !ui.detail) return;
    const log = ui.detail.querySelector('[data-role="log"]');
    const e = S.selected && findEntity(S.selected);
    if (!log || !e) return;
    const r = S.results.get(e.id);
    log.textContent = "";
    if (!r || !r.log.length) return;
    log.appendChild(h("div", "ent-log-head", "Results"));
    r.log.slice().reverse().forEach((entry) => log.appendChild(logLine(e, entry)));
  }

  function renderDetail() {
    const ui = S.ui;
    if (!ui) return;
    ui.detail.textContent = "";
    const e = S.selected && findEntity(S.selected);
    ui.detail.hidden = !e;
    if (!e) return;
    ui.detail.appendChild(buildDetail(e));
    renderLog();
    refreshIcons();
  }

  function rowDetail(e) {
    const r = S.results.get(e.id);
    if (isGate(e)) {
      if (!r || (!r.passes && !r.misses)) return { text: metres(distanceM([e.lng, e.lat], [e.lng2, e.lat2])), level: "" };
      return r.passes > 0
        ? { text: r.passes + "× through", level: "good" }
        : { text: r.misses + "× missed", level: "warn" };
    }
    if (e.kind === "tree") {
      if (r && r.hits > 0) return { text: "hit ×" + r.hits, level: "bad" };
      return { text: metres(e.height) + " · r " + metres(e.radius), level: "" };
    }
    if (r && r.inside) return { text: "inside", level: "warn" };
    const angle = sectorAngle(e);
    return { text: e.radius > 0 ? "r " + metres(e.radius) + (angle < 360 ? " · " + Math.round(angle) + "°" : "") : "", level: "" };
  }

  function renderSummary() {
    const ui = S.ui;
    if (!ui) return;
    const gates = S.entities.filter(isGate);
    const passed = gates.filter((g) => (S.results.get(g.id) || {}).passes > 0).length;
    const trees = S.entities.filter((e) => e.kind === "tree");
    const hits = trees.filter((t) => (S.results.get(t.id) || {}).hits > 0).length;
    const inside = S.entities.filter((e) => !isGate(e) && (S.results.get(e.id) || {}).inside).length;
    const cells = [
      ["Entities", String(S.entities.length), ""],
      ["Gates", passed + " / " + gates.length, ""],
      ["Trees hit", String(hits), hits ? "bad" : ""],
      ["Inside area", String(inside), inside ? "warn" : ""],
    ];
    ui.summary.textContent = "";
    cells.forEach((c) => {
      const cell = h("div", "ent-cell");
      if (c[2]) cell.setAttribute("data-level", c[2]);
      cell.appendChild(h("span", "ent-cell-label", c[0]));
      cell.appendChild(h("span", "ent-cell-value", c[1]));
      ui.summary.appendChild(cell);
    });
  }

  function renderList() {
    const ui = S.ui;
    if (!ui) return;
    renderSummary();
    ui.list.textContent = "";
    ui.empty.hidden = S.entities.length > 0;
    S.entities.forEach((e) => {
      const kind = KIND_BY_ID[e.kind];
      const row = h("div", "ent-row" + (S.selected === e.id ? " is-selected" : ""));
      row.setAttribute("role", "button");
      row.tabIndex = 0;
      row.appendChild(badge(kind.icon, statusColor(e), 14));
      row.appendChild(h("span", "ent-row-label", e.name));
      const d = rowDetail(e);
      const value = h("span", "ent-row-value", d.text);
      if (d.level) value.setAttribute("data-level", d.level);
      row.appendChild(value);
      const tools = h("span", "ent-row-tools");
      tools.appendChild(iconButton("locate-fixed", "Show on the map", () => {
        const m = mapApi();
        if (m) m.fit(pointsOf(e));
      }));
      tools.appendChild(iconButton("trash-2", "Delete", () => removeEntity(e.id), "ent-remove"));
      row.appendChild(tools);
      row.addEventListener("click", () => selectEntity(e.id));
      row.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") { event.preventDefault(); selectEntity(e.id); }
      });
      ui.list.appendChild(row);
    });
    refreshIcons();
  }

  function renderUndo() {
    const ui = S.ui;
    if (!ui) return;
    const u = S.undo;
    ui.undo.hidden = !u;
    ui.undo.textContent = "";
    if (!u) return;
    const many = u.items.length > 1;
    ui.undo.appendChild(h("span", "ent-undo-text", many ? "Deleted all" : "Deleted"));
    if (!many) ui.undo.appendChild(h("strong", "ent-undo-name", u.items[0].entity.name));
    const button = h("button", "btn", "Undo");
    button.type = "button";
    button.setAttribute("data-variant", "secondary");
    button.setAttribute("data-size", "sm");
    button.addEventListener("click", undoDelete);
    ui.undo.appendChild(button);
  }

  function renderHint() {
    const ui = S.ui;
    if (!ui) return;
    const p = S.placing;
    ui.hint.hidden = !p;
    ui.hint.textContent = "";
    if (!p) return;
    const kind = KIND_BY_ID[p.kind];
    let text;
    if (kind.shape === "gate") {
      text = p.step === 0 ? "Click the map for post A of the gate." : "Click the map for post B of the gate.";
    } else {
      text = "Click the map to place: " + kind.label + ".";
    }
    ui.hint.appendChild(h("span", "ent-hint-text", text));
    const done = h("button", "btn", "Done");
    done.type = "button";
    done.setAttribute("data-variant", "secondary");
    done.setAttribute("data-size", "sm");
    done.addEventListener("click", stopPlacing);
    ui.hint.appendChild(done);
    ui.hint.appendChild(h("span", "ent-hint-sub", "Esc or right click also stops."));
  }

  function renderPalette() {
    const ui = S.ui;
    if (!ui) return;
    ui.palette.querySelectorAll("[data-kind]").forEach((b) => {
      b.classList.toggle("is-active", !!S.placing && S.placing.kind === b.getAttribute("data-kind"));
    });
  }

  /** The two things the plugin is for, side by side on a row of their own,
   *  then every other symbol in an even grid. */
  function buildPalette() {
    const wrap = h("div", "ent-palette-wrap");
    const main = h("div", "ent-palette-main");
    const rest = h("div", "ent-palette");
    wrap.appendChild(main);
    wrap.appendChild(rest);
    KINDS.forEach((kind) => {
      const isMain = kind.id === "gate" || kind.id === "tree";
      const b = h("button", "ent-kind" + (isMain ? " is-main" : ""));
      b.type = "button";
      b.setAttribute("data-kind", kind.id);
      b.title = "Place: " + kind.label;
      b.appendChild(badge(kind.icon, kind.color, isMain ? 18 : 15));
      b.appendChild(h("span", "ent-kind-label", kind.label));
      b.addEventListener("click", () => {
        if (S.placing && S.placing.kind === kind.id) stopPlacing();
        else startPlacing(kind.id);
      });
      (isMain ? main : rest).appendChild(b);
    });
    return wrap;
  }

  /** Redraw whatever of the view shows results. Cheap enough to call per event. */
  function refreshView() {
    renderList();
    renderLog();
  }

  /** The list changed under the editor (something was added, or taken away). */
  function refreshAll() {
    renderList();
    renderDetail();
  }

  function init(container, api) {
    S.api = api || S.api;
    if (!S.started) loadSaved();
    const root = h("div", "ent-root");

    const summary = h("div", "ent-summary");
    root.appendChild(summary);

    const addHead = h("div", "ent-list-head");
    addHead.appendChild(h("span", "", "Place"));
    root.appendChild(addHead);
    const palette = buildPalette();
    root.appendChild(palette);
    const hint = h("div", "ent-hint");
    hint.setAttribute("role", "status");
    hint.hidden = true;
    root.appendChild(hint);

    const listHead = h("div", "ent-list-head");
    listHead.appendChild(h("span", "", "On the map"));
    const tools = h("span", "ent-head-tools");
    tools.appendChild(iconButton("rotate-ccw", "Reset results", resetResults));
    tools.appendChild(iconButton("maximize", "Show all on the map", () => {
      const m = mapApi();
      if (!m || !S.entities.length) return;
      m.fit([].concat.apply([], S.entities.map(pointsOf)));
    }));
    tools.appendChild(iconButton("trash-2", "Delete all", removeAll, "ent-remove"));
    listHead.appendChild(tools);
    root.appendChild(listHead);

    const undo = h("div", "ent-undo");
    undo.setAttribute("role", "status");
    undo.hidden = true;
    root.appendChild(undo);

    const empty = h("div", "ent-empty", "Nothing placed yet. Pick a gate or a tree above, then click the map.");
    root.appendChild(empty);
    const list = h("div", "ent-list");
    root.appendChild(list);
    const detail = h("div", "ent-detail-wrap");
    detail.hidden = true;
    root.appendChild(detail);

    container.textContent = "";
    container.appendChild(root);
    S.ui = { root, summary, palette, hint, list, empty, detail, undo };
    renderUndo();
    renderList();
    renderDetail();
    renderHint();
    renderPalette();
    refreshIcons();
  }

  function destroy(container) {
    stopPlacing();
    clearUndo();
    S.ui = null;
    if (container) container.textContent = "";
  }

  // ---- start ----------------------------------------------------------------

  function readOptions(api) {
    const o = api && typeof api.getOptions === "function" ? api.getOptions() || {} : {};
    S.options = {
      labels: o.labels !== false,
      notify: o.notify !== false,
      armedOnly: o.armedOnly !== false,
    };
  }

  function loadSaved() {
    S.started = true;
    const settings = S.api && typeof S.api.getSettings === "function" ? S.api.getSettings() || {} : {};
    S.entities = normalizeList(settings.entities);
    readOptions(S.api);
    drawAll();
  }

  function start(api) {
    if (S.started) return;
    S.api = api;
    loadSaved();
    if (api && typeof api.subscribe === "function") api.subscribe(onState);
  }

  function optionsChanged(options, api) {
    S.api = api || S.api;
    readOptions({ getOptions: () => options });
    drawAll();
  }

  return {
    init, destroy, start, optionsChanged,
    KINDS, COLORS,
    parseDegrees, parseCoordinatePair, toLocal, distanceM, bearingDeg,
    gateCrossing, gateVerdict, zoneContact, inSector, sectorRing, sectorAngle,
    makeEntity, normalizeEntity, normalizeList, sampleOf,
    processStep, onState,
    _state: S,
    _addEntity: addEntity,
    _removeEntity: removeEntity,
    _removeAll: removeAll,
    _undoDelete: undoDelete,
    _startPlacing: startPlacing,
    _stopPlacing: stopPlacing,
    _select: selectEntity,
    _resetResults: resetResults,
  };
})();

if (window.Corvus && Corvus.plugins && typeof Corvus.plugins.register === "function") {
  Corvus.plugins.register("map-entities", {
    name: "Map Entities",
    icon: "map-pin",
    description: "Gates, trees, people and devices with a range, placed exactly on the Home map",
    tab: true,
    init: function (containerEl, api) { Corvus.pluginMapEntities.init(containerEl, api); },
    destroy: function (containerEl) { Corvus.pluginMapEntities.destroy(containerEl); },
    start: function (api) { Corvus.pluginMapEntities.start(api); },
    options: [
      { key: "labels", label: "Show names on the map", type: "toggle", default: true },
      { key: "notify", label: "Notify on gate passes and area entries", type: "toggle", default: true },
      { key: "armedOnly", label: "Only check while armed", type: "toggle", default: true },
    ],
    optionsChanged: function (options, api) { Corvus.pluginMapEntities.optionsChanged(options, api); },
  });
}
