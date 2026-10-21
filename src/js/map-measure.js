"use strict";

/*
  Corvus.mapMeasure: the distance tool on the Home map.

  The rail's ruler button turns it on. While it is on, a click puts down the
  first point, the line follows the pointer, and a second click fixes the end.
  The label on the line reads the ground distance and the heading from the
  first point to the second. A click after that starts a new measurement.

  The distance is the great circle over the ground, in the operator's units.
  It does not follow the terrain, so on a slope the path walked is longer.

  Escape or a right click clears the line; with nothing on the map they turn
  the tool off. The context menu and the planning clicks stand down while it is
  on (js/map.js asks isActive() first), because a click here is a measurement,
  not a place to act on.
*/
window.Corvus = window.Corvus || {};
Corvus.mapMeasure = (function () {

  const EARTH_R_M = 6371008.8;
  const SOURCE = "measure";
  const LAYERS = ["measure-casing", "measure-line", "measure-points"];

  let map = null;
  let active = false;
  let points = [];
  let cursor = null;
  let label = null;
  let labelText = null;
  let labelHeading = null;
  let onChange = null;

  // =====================================================================
  // Pure: no DOM, no map. Exported as test hooks at the bottom.
  // =====================================================================

  /** Great-circle distance in metres between two [lng, lat]. */
  function distanceM(a, b) {
    const lat1 = a[1] * Math.PI / 180;
    const lat2 = b[1] * Math.PI / 180;
    const dLat = lat2 - lat1;
    const dLng = (b[0] - a[0]) * Math.PI / 180;
    const h = Math.sin(dLat / 2) ** 2 +
      Math.cos(lat1) * Math.cos(lat2) * Math.sin(dLng / 2) ** 2;
    return 2 * EARTH_R_M * Math.asin(Math.min(1, Math.sqrt(h)));
  }

  /** Initial bearing from a to b in degrees clockwise from north, 0 to 359. */
  function bearingDeg(a, b) {
    const lat1 = a[1] * Math.PI / 180;
    const lat2 = b[1] * Math.PI / 180;
    const dLng = (b[0] - a[0]) * Math.PI / 180;
    const y = Math.sin(dLng) * Math.cos(lat2);
    const x = Math.cos(lat1) * Math.sin(lat2) - Math.sin(lat1) * Math.cos(lat2) * Math.cos(dLng);
    return (Math.atan2(y, x) * 180 / Math.PI + 360) % 360;
  }

  /** The point halfway along the great circle from a to b. */
  function midpoint(a, b) {
    const lat1 = a[1] * Math.PI / 180;
    const lng1 = a[0] * Math.PI / 180;
    const lat2 = b[1] * Math.PI / 180;
    const dLng = (b[0] - a[0]) * Math.PI / 180;
    const bx = Math.cos(lat2) * Math.cos(dLng);
    const by = Math.cos(lat2) * Math.sin(dLng);
    const lat = Math.atan2(Math.sin(lat1) + Math.sin(lat2),
      Math.sqrt((Math.cos(lat1) + bx) ** 2 + by ** 2));
    const lng = lng1 + Math.atan2(by, Math.cos(lat1) + bx);
    return [((lng * 180 / Math.PI + 540) % 360) - 180, lat * 180 / Math.PI];
  }

  /** A distance in the display units: a decimal below 100 m, whole numbers up
   *  to a kilometre, then the distance unit. */
  function formatDistance(m) {
    const units = Corvus.units;
    if (!units) return `${Math.round(m)} m`;
    if (m < 1000) return units.formatLength(m, { digits: m < 100 ? 1 : 0 });
    return units.formatDistance(m);
  }

  // =====================================================================
  // The line on the map
  // =====================================================================

  function accent() {
    return (Corvus.ui && Corvus.ui.token) ? Corvus.ui.token("--accent", "#4CC9FF") : "#4CC9FF";
  }

  function addLayers() {
    if (!map || map.getSource(SOURCE)) return;
    map.addSource(SOURCE, { type: "geojson", data: { type: "FeatureCollection", features: [] } });
    map.addLayer({
      id: "measure-casing",
      source: SOURCE,
      type: "line",
      filter: ["==", ["geometry-type"], "LineString"],
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": "#000", "line-opacity": 0.55, "line-width": 6 },
    });
    map.addLayer({
      id: "measure-line",
      source: SOURCE,
      type: "line",
      filter: ["==", ["geometry-type"], "LineString"],
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": accent(), "line-width": 3 },
    });
    map.addLayer({
      id: "measure-points",
      source: SOURCE,
      type: "circle",
      filter: ["==", ["geometry-type"], "Point"],
      paint: {
        "circle-radius": 5.5,
        "circle-color": "#fff",
        "circle-stroke-color": accent(),
        "circle-stroke-width": 2.5,
      },
    });
  }

  function removeLayers() {
    if (!map) return;
    try {
      LAYERS.forEach((id) => { if (map.getLayer(id)) map.removeLayer(id); });
      if (map.getSource(SOURCE)) map.removeSource(SOURCE);
    } catch (_e) { /* the map is going away */ }
  }

  function buildLabel() {
    const el = document.createElement("div");
    el.className = "map-measure-label";
    labelText = document.createElement("span");
    labelHeading = document.createElement("small");
    el.append(labelText, labelHeading);
    label = new maplibregl.Marker({ element: el, anchor: "bottom", offset: [0, -10] });
    return label;
  }

  function end() {
    return points.length === 2 ? points[1] : cursor;
  }

  /** Redraw the points, the line and the label from `points` and `cursor`. */
  function render() {
    const source = map && map.getSource(SOURCE);
    if (!source) return;
    const features = points.map((p) => ({
      type: "Feature", properties: {}, geometry: { type: "Point", coordinates: p },
    }));
    const to = points.length ? end() : null;
    if (points.length && to) {
      features.push({
        type: "Feature", properties: {},
        geometry: { type: "LineString", coordinates: [points[0], to] },
      });
    }
    source.setData({ type: "FeatureCollection", features });

    if (!points.length || !to) {
      if (label) label.remove();
      return;
    }
    const m = distanceM(points[0], to);
    if (!label) buildLabel();
    labelText.textContent = formatDistance(m);
    labelHeading.textContent = m > 0 ? `${Math.round(bearingDeg(points[0], to)) % 360}°` : "";
    label.setLngLat(midpoint(points[0], to));
    if (!label.getElement().isConnected) label.addTo(map);
  }

  function clear() {
    points = [];
    cursor = null;
    render();
  }

  // =====================================================================
  // Pointer and keyboard
  // =====================================================================

  function onClick(e) {
    if (!e || !e.lngLat) return;
    const p = [e.lngLat.lng, e.lngLat.lat];
    if (points.length === 1) {
      points.push(p);
      cursor = null;
    } else {
      points = [p];
      cursor = null;
    }
    render();
  }

  function onMove(e) {
    if (points.length !== 1 || !e || !e.lngLat) return;
    cursor = [e.lngLat.lng, e.lngLat.lat];
    render();
  }

  /** Give up the line first; with none on the map, leave the tool. */
  function back() {
    if (points.length) clear();
    else setActive(false);
  }

  function onContext(e) {
    if (e && e.preventDefault) e.preventDefault();
    back();
  }

  function onKey(e) {
    if (e && e.key === "Escape") back();
  }

  // =====================================================================
  // Public
  // =====================================================================

  function setActive(on) {
    on = !!on;
    if (!map || active === on) return active;
    if (on) {
      if (!map.getSource(SOURCE)) {
        if (!map.isStyleLoaded()) return false;
        addLayers();
      }
      active = true;
      map.getCanvas().style.cursor = "crosshair";
      map.on("click", onClick);
      map.on("mousemove", onMove);
      map.on("contextmenu", onContext);
      document.addEventListener("keydown", onKey);
    } else {
      active = false;
      map.getCanvas().style.cursor = "";
      map.off("click", onClick);
      map.off("mousemove", onMove);
      map.off("contextmenu", onContext);
      document.removeEventListener("keydown", onKey);
      clear();
    }
    if (typeof onChange === "function") onChange(active);
    return active;
  }

  /** Bind to the Home map. `change(active)` follows every on/off, whoever
   *  caused it, so the rail button can never disagree with the tool. */
  function attach(m, change) {
    map = m;
    onChange = change || null;
  }

  /** Tear everything down: the shutdown path and the tests. */
  function detach() {
    if (active) setActive(false);
    if (label) { label.remove(); label = null; }
    removeLayers();
    map = null;
    onChange = null;
  }

  return {
    attach,
    detach,
    setActive,
    toggle: () => setActive(!active),
    isActive: () => active,
    points: () => points.map((p) => p.slice()),
    _distanceM: distanceM,
    _bearingDeg: bearingDeg,
    _midpoint: midpoint,
    _formatDistance: formatDistance,
  };
})();
