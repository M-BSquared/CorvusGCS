"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.geofence: the area the vehicle may fly in.

  One copy of it for the whole app. The Geofence card on Safety & Sensors
  draws and uploads it (js/setup-geofence.js); this module keeps what the
  backend stored (GET /api/geofence), tells everyone who listens when it
  changes, and draws it on the Home map as a dashed outline when the operator
  asked for that.

  The outline is a polygon overlay on the Home map (Corvus.map), the same
  kind plugins draw, so it sits under the flown track like every reference
  shape. map.js draws an overlay that was set before its style loaded once it
  has, so this can draw at boot without waiting for the map.

  Backend contract:
    GET  /api/geofence          {polygon, show_on_map, on_vehicle, vehicle_has_fence}
    POST /api/geofence/save     {polygon?, show_on_map?} -> same shape
    POST /api/geofence/upload   puts the stored area on the vehicle
    POST /api/geofence/clear    removes the vehicle's fence, keeps the area
*/
Corvus.geofence = (function () {
  const OVERLAY_ID = "corvus-geofence";
  const EARTH_R = 6371008.8;

  let state = {
    loaded: false, polygon: [], show_on_map: true,
    on_vehicle: null, vehicle_has_fence: null, max_vertices: 64,
  };
  const listeners = new Set();

  // ---- Geometry, pure and exported for the tests ----------------------------

  /** Whether [lng, lat] lies inside the ring of [lng, lat] corners. */
  function contains(polygon, point) {
    if (!Array.isArray(polygon) || polygon.length < 3 || !point) return false;
    const x = point[0];
    const y = point[1];
    let inside = false;
    for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i, i += 1) {
      const xi = polygon[i][0];
      const yi = polygon[i][1];
      const xj = polygon[j][0];
      const yj = polygon[j][1];
      if ((yi > y) !== (yj > y) && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) inside = !inside;
    }
    return inside;
  }

  function orient(p, q, r) {
    return (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0]);
  }

  function crosses(a, b, c, d) {
    const d1 = orient(c, d, a);
    const d2 = orient(c, d, b);
    const d3 = orient(a, b, c);
    const d4 = orient(a, b, d);
    return d1 !== 0 && d2 !== 0 && d3 !== 0 && d4 !== 0
      && (d1 > 0) !== (d2 > 0) && (d3 > 0) !== (d4 > 0);
  }

  /** Whether two edges of the closed ring cross. The backend refuses such an
   *  area, and so does the vehicle. */
  function selfIntersects(polygon) {
    const n = Array.isArray(polygon) ? polygon.length : 0;
    for (let i = 0; i < n; i += 1) {
      for (let j = i + 1; j < n; j += 1) {
        if (j === i + 1 || (j + 1) % n === i) continue;
        if (crosses(polygon[i], polygon[(i + 1) % n], polygon[j], polygon[(j + 1) % n])) return true;
      }
    }
    return false;
  }

  /** The area of the ring in square metres, on a local flat projection. */
  function areaM2(polygon) {
    if (!Array.isArray(polygon) || polygon.length < 3) return 0;
    const lat0 = polygon.reduce((s, p) => s + p[1], 0) / polygon.length;
    const kx = (Math.PI / 180) * EARTH_R * Math.cos((lat0 * Math.PI) / 180);
    const ky = (Math.PI / 180) * EARTH_R;
    let sum = 0;
    for (let i = 0; i < polygon.length; i += 1) {
      const a = polygon[i];
      const b = polygon[(i + 1) % polygon.length];
      sum += (a[0] * kx) * (b[1] * ky) - (b[0] * kx) * (a[1] * ky);
    }
    return Math.abs(sum) / 2;
  }

  // ---- State -----------------------------------------------------------------

  function get() {
    return Object.assign({}, state, { polygon: state.polygon.map((p) => p.slice()) });
  }

  function onChange(fn) {
    listeners.add(fn);
    return () => listeners.delete(fn);
  }

  function adopt(data) {
    if (!data || typeof data !== "object") return;
    state = {
      loaded: true,
      polygon: Array.isArray(data.polygon) ? data.polygon.map((p) => [Number(p[0]), Number(p[1])]) : [],
      show_on_map: data.show_on_map !== false,
      on_vehicle: typeof data.on_vehicle === "boolean" ? data.on_vehicle : null,
      vehicle_has_fence: typeof data.vehicle_has_fence === "boolean" ? data.vehicle_has_fence : null,
      max_vertices: Number(data.max_vertices) > 0 ? Number(data.max_vertices) : state.max_vertices,
    };
    drawOnHome();
    listeners.forEach((fn) => {
      try { fn(get()); } catch (error) { console.error("geofence listener failed:", error); }
    });
  }

  function load() {
    return Corvus.telemetry.requestJson("/api/geofence").then((data) => { adopt(data); return get(); });
  }

  function post(url, payload) {
    return Corvus.telemetry.postAction(url, payload || {}).then((data) => { adopt(data); return get(); });
  }

  /** Keep a new area; [] removes it from this station. */
  function savePolygon(polygon) { return post("/api/geofence/save", { polygon }); }

  function setShowOnMap(on) { return post("/api/geofence/save", { show_on_map: !!on }); }

  function upload() { return post("/api/geofence/upload"); }

  function clearVehicle() { return post("/api/geofence/clear"); }

  // ---- The Home map ----------------------------------------------------------

  function drawOnHome() {
    const m = Corvus.map;
    if (!m || typeof m.setPolygonOverlay !== "function") return;
    if (state.show_on_map && state.polygon.length >= 3) {
      m.setPolygonOverlay(OVERLAY_ID, state.polygon, {
        color: Corvus.ui.token("--warning", "#F5A524"),
        fillOpacity: 0.06,
        width: 3,
        dashed: true,
      });
    } else if (typeof m.removeOverlay === "function") {
      m.removeOverlay(OVERLAY_ID);
    }
  }

  function start() {
    load().catch(() => {});
    // The outline is a literal colour in the map's paint, so a theme switch
    // redraws it with the new one.
    if (Corvus.ui && typeof Corvus.ui.onThemeChange === "function") Corvus.ui.onThemeChange(drawOnHome);
  }

  return {
    OVERLAY_ID,
    start, load, get, onChange,
    savePolygon, setShowOnMap, upload, clearVehicle,
    contains, selfIntersects, areaM2,
    _adopt: adopt,
  };
})();
