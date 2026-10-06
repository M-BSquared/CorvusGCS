"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.mapOverlays: the shapes, labels and buttons plugins draw on the Home
  map (the map half of api.map in js/plugins.js).

  Moved out of js/map.js, which owns the map itself: it hands the instance
  over with attach() as soon as it exists and calls markStarted() once the
  style has loaded, then reaches the functions below through the handles it
  takes at load. Loaded before js/map.js.
*/
Corvus.mapOverlays = (function () {
  let map = null;
  let started = false;

  // ---- Overlays: what a plugin draws on this map --------------------------
  //
  // The map half of api.map in js/plugins.js. An overlay is kept here as data
  // first and drawn second, so a plugin that draws from its start hook, which
  // runs at boot and usually before the style has loaded, loses nothing: the
  // "load" handler draws whatever is waiting.
  //
  // Four kinds: a line, an area (a filled polygon with an outline), a text
  // label and a button. Lines and areas go UNDER the flown track. A reference shape is
  // what the aircraft is measured against, and where the two cross the
  // aircraft's own path is the one the operator has to be able to read. A
  // label is a DOM marker, for the reason the region labels give: a text
  // layer needs glyphs, and the field laptop has no internet to fetch them.
  // Markers sit over the canvas, so a label is the one overlay above the
  // track; it is small text, not something that hides a path. A button is a
  // DOM marker too, and the one overlay that takes the pointer.
  const overlays = new Map();   // id -> {kind, coords, color, ...}
  const overlayMarkers = new Map();   // id -> {marker, el} for a text overlay
  const OVERLAY_PREFIX = "overlay-";
  // The track's layers, lowest first: an overlay is inserted beneath the
  // first of them that is in the style.
  const TRACK_LAYERS = [
    "path-past-glow", "path-past-casing", "path-past-line",
    "path-glow", "path-casing", "path-line",
    "waypoints-route",
  ];
  const OVERLAY_DEFAULT_COLOR = "#2BC4E4";
  const OVERLAY_TEXT_MAX = 200;

  function overlayIds(id) {
    const source = OVERLAY_PREFIX + id;
    return { source, fill: source + "-fill", casing: source + "-casing", line: source + "-line" };
  }

  /** Keep the [lng, lat] pairs that are real numbers on the globe. Pure. */
  function cleanOverlayCoords(coords) {
    if (!Array.isArray(coords)) return [];
    const out = [];
    coords.forEach((c) => {
      if (!Array.isArray(c) || c.length < 2) return;
      const lng = Number(c[0]);
      const lat = Number(c[1]);
      if (!isFinite(lng) || !isFinite(lat)) return;
      if (Math.abs(lat) > 90 || Math.abs(lng) > 180) return;
      out.push([lng, lat]);
    });
    return out;
  }

  /** The style shared by every kind: colour, opacity, visibility. Pure. */
  function overlayStyle(opts) {
    const o = opts || {};
    const opacity = Number(o.opacity);
    return {
      color: typeof o.color === "string" && o.color ? o.color : OVERLAY_DEFAULT_COLOR,
      opacity: isFinite(opacity) ? Math.min(Math.max(opacity, 0), 1) : 0.95,
      visible: o.visible !== false,
    };
  }

  function overlayWidth(raw, fallback) {
    const width = Number(raw);
    return isFinite(width) && width > 0 ? Math.min(width, 12) : fallback;
  }

  /** Remove an overlay's layers, source and label, if they are there. */
  function undrawOverlay(id) {
    const label = overlayMarkers.get(id);
    if (label) {
      overlayMarkers.delete(id);
      try { label.marker.remove(); } catch (_e) { /* already gone */ }
    }
    if (!map) return;
    const ids = overlayIds(id);
    try {
      [ids.line, ids.casing, ids.fill].forEach((layer) => {
        if (map.getLayer(layer)) map.removeLayer(layer);
      });
      if (map.getSource(ids.source)) map.removeSource(ids.source);
    } catch (error) {
      console.warn("Corvus: could not remove overlay", id, error);
    }
  }

  /** The DOM label a text overlay is drawn as. textContent only: the text is
   *  a plugin's, and must never be read as markup. */
  function buildOverlayLabel(o) {
    const el = document.createElement("div");
    el.className = "map-overlay-label" + (o.dot ? " has-dot" : "");
    el.style.setProperty("--overlay-color", o.color);
    el.style.setProperty("--overlay-size", o.size + "px");
    el.style.opacity = String(o.opacity);
    if (o.dot) {
      const dot = document.createElement("span");
      dot.className = "map-overlay-label-dot";
      el.appendChild(dot);
    }
    const text = document.createElement("span");
    text.className = "map-overlay-label-text";
    text.textContent = o.text;
    el.appendChild(text);
    el.hidden = !o.visible;
    return el;
  }

  /** The DOM button a button overlay is drawn as. textContent only, and the
   *  click never reaches the map beneath it (no pan, no waypoint dropped). */
  function buildOverlayButton(o) {
    const el = document.createElement("button");
    el.type = "button";
    el.className = "map-overlay-button";
    el.style.setProperty("--overlay-color", o.color);
    el.style.setProperty("--overlay-size", o.size + "px");
    el.style.opacity = String(o.opacity);
    el.textContent = o.text;
    if (o.title) {
      el.title = o.title;
      el.setAttribute("aria-label", o.title);
    }
    el.disabled = !!o.disabled;
    const stop = (event) => { if (event && event.stopPropagation) event.stopPropagation(); };
    ["mousedown", "dblclick", "touchstart", "pointerdown"].forEach((type) => el.addEventListener(type, stop));
    el.addEventListener("click", (event) => {
      stop(event);
      if (typeof o.onClick !== "function") return;
      try { o.onClick(); } catch (error) { console.warn("Corvus: map button handler failed", error); }
    });
    el.hidden = !o.visible;
    return el;
  }

  /**
   * Put one overlay into the style as it is stored now. The layers are rebuilt
   * rather than patched: a change of colour, width or dash is rare, and a
   * rebuilt layer can never keep a dash array the new style does not have.
   */
  function drawOverlay(id) {
    if (!map || !started) return;
    const o = overlays.get(id);
    if (!o) { undrawOverlay(id); return; }
    if (o.kind === "text" || o.kind === "button") {
      undrawOverlay(id);
      try {
        const el = o.kind === "button" ? buildOverlayButton(o) : buildOverlayLabel(o);
        // With a dot the point is the dot's centre and the text runs off to
        // its right; without one the text is centred on the point.
        const marker = new maplibregl.Marker(o.kind === "text" && o.dot
          ? { element: el, anchor: "left", offset: [-5, 0] }
          : { element: el, anchor: "center" })
          .setLngLat(o.coords[0]).addTo(map);
        overlayMarkers.set(id, { marker, el });
      } catch (error) {
        console.warn("Corvus: could not draw overlay", id, error);
      }
      return;
    }
    const ids = overlayIds(id);
    const ring = o.kind === "polygon" ? o.coords.concat([o.coords[0]]) : null;
    const data = {
      type: "Feature",
      properties: {},
      geometry: ring
        ? { type: "Polygon", coordinates: [ring] }
        : { type: "LineString", coordinates: o.coords },
    };
    try {
      const source = map.getSource(ids.source);
      if (source) source.setData(data);
      else map.addSource(ids.source, { type: "geojson", data });
      [ids.line, ids.casing, ids.fill].forEach((layer) => {
        if (map.getLayer(layer)) map.removeLayer(layer);
      });
      const before = TRACK_LAYERS.find((layer) => map.getLayer(layer));
      const visibility = o.visible ? "visible" : "none";
      if (ring) {
        map.addLayer({
          id: ids.fill,
          source: ids.source,
          type: "fill",
          layout: { visibility },
          paint: { "fill-color": o.color, "fill-opacity": o.fillOpacity },
        }, before);
      }
      if (o.width > 0) {
        // A casing in the page background under the colour, the same trick
        // the track uses: a thin line alone disappears over imagery of its
        // own hue.
        map.addLayer({
          id: ids.casing,
          source: ids.source,
          type: "line",
          layout: { "line-cap": "round", "line-join": "round", visibility },
          paint: {
            "line-color": Corvus.ui.token("--bg", "#0B0E12"),
            "line-width": o.width + 2.4,
            "line-opacity": 0.45 * o.opacity,
          },
        }, before);
        const paint = { "line-color": o.color, "line-width": o.width, "line-opacity": o.opacity };
        if (o.dashed) paint["line-dasharray"] = [2, 1.5];
        map.addLayer({
          id: ids.line,
          source: ids.source,
          type: "line",
          layout: { "line-cap": o.dashed ? "butt" : "round", "line-join": "round", visibility },
          paint,
        }, before);
      }
    } catch (error) {
      console.warn("Corvus: could not draw overlay", id, error);
    }
  }

  /**
   * Draw (or redraw) a line overlay. `id` is the caller's key; drawing the
   * same id again replaces it, whatever kind it was. Returns false when there
   * is nothing to draw: fewer than two usable points.
   *
   * @param {string} id
   * @param {Array<[number, number]>} coords [lng, lat] pairs
   * @param {Object} [opts] {color, width, opacity, dashed, visible}
   * @returns {boolean}
   */
  function setOverlay(id, coords, opts) {
    if (typeof id !== "string" || !id) return false;
    const clean = cleanOverlayCoords(coords);
    if (clean.length < 2) return false;
    const o = opts || {};
    overlays.set(id, Object.assign({ kind: "line", coords: clean }, overlayStyle(o), {
      width: overlayWidth(o.width, 3),
      dashed: !!o.dashed,
    }));
    drawOverlay(id);
    return true;
  }

  /**
   * Draw (or redraw) an area: the polygon the points span, filled, with an
   * outline. The ring closes itself, so the first point need not be repeated
   * at the end (and is dropped when it is). Returns false for fewer than
   * three usable corners.
   *
   * @param {string} id
   * @param {Array<[number, number]>} coords [lng, lat] corners, in order
   * @param {Object} [opts] {color, fillOpacity, width, opacity, dashed,
   *                        visible}; width 0 draws no outline
   * @returns {boolean}
   */
  function setPolygonOverlay(id, coords, opts) {
    if (typeof id !== "string" || !id) return false;
    const clean = cleanOverlayCoords(coords);
    if (clean.length > 1) {
      const a = clean[0];
      const z = clean[clean.length - 1];
      if (a[0] === z[0] && a[1] === z[1]) clean.pop();
    }
    if (clean.length < 3) return false;
    const o = opts || {};
    const fill = Number(o.fillOpacity);
    const width = Number(o.width);
    overlays.set(id, Object.assign({ kind: "polygon", coords: clean }, overlayStyle(o), {
      fillOpacity: isFinite(fill) ? Math.min(Math.max(fill, 0), 1) : 0.25,
      width: width === 0 ? 0 : overlayWidth(o.width, 2),
      dashed: !!o.dashed,
    }));
    drawOverlay(id);
    return true;
  }

  /**
   * Draw (or redraw) a text label at one [lng, lat] point. Returns false for
   * an unusable point or empty text.
   *
   * @param {string} id
   * @param {[number, number]} point [lng, lat]
   * @param {string} text  shown as plain text, at most 200 characters
   * @param {Object} [opts] {color, size (px, 9 to 32), dot, opacity, visible}
   * @returns {boolean}
   */
  function setTextOverlay(id, point, text, opts) {
    if (typeof id !== "string" || !id) return false;
    const clean = cleanOverlayCoords([point]);
    const label = String(text == null ? "" : text).replace(/\s+/g, " ").trim()
      .slice(0, OVERLAY_TEXT_MAX);
    if (!clean.length || !label) return false;
    const o = opts || {};
    const size = Number(o.size);
    const style = overlayStyle(o);
    // Text is read, not seen through: fully opaque unless asked otherwise.
    if (o.opacity == null) style.opacity = 1;
    overlays.set(id, Object.assign({ kind: "text", coords: clean, text: label }, style, {
      size: isFinite(size) ? Math.min(Math.max(Math.round(size), 9), 32) : 12,
      dot: !!o.dot,
    }));
    drawOverlay(id);
    return true;
  }

  /**
   * Draw (or redraw) a clickable button at one [lng, lat] point. Returns
   * false for an unusable point or empty label.
   *
   * @param {string} id
   * @param {[number, number]} point [lng, lat]
   * @param {string} text  shown as plain text, at most 200 characters
   * @param {Object} [opts] {onClick, title, disabled, color, size (px, 9 to 32), opacity, visible}
   * @returns {boolean}
   */
  function setButtonOverlay(id, point, text, opts) {
    if (typeof id !== "string" || !id) return false;
    const clean = cleanOverlayCoords([point]);
    const label = String(text == null ? "" : text).replace(/\s+/g, " ").trim()
      .slice(0, OVERLAY_TEXT_MAX);
    if (!clean.length || !label) return false;
    const o = opts || {};
    const size = Number(o.size);
    const style = overlayStyle(o);
    if (o.opacity == null) style.opacity = 1;
    overlays.set(id, Object.assign({ kind: "button", coords: clean, text: label }, style, {
      size: isFinite(size) ? Math.min(Math.max(Math.round(size), 9), 32) : 13,
      title: typeof o.title === "string" ? o.title.slice(0, OVERLAY_TEXT_MAX) : "",
      disabled: !!o.disabled,
      onClick: typeof o.onClick === "function" ? o.onClick : null,
    }));
    drawOverlay(id);
    return true;
  }

  /** Remove an overlay. Returns whether there was one. */
  function removeOverlay(id) {
    const had = overlays.delete(id);
    undrawOverlay(id);
    return had;
  }

  /** Show or hide an overlay without forgetting it. */
  function setOverlayVisible(id, on) {
    const o = overlays.get(id);
    if (!o) return false;
    o.visible = !!on;
    const label = overlayMarkers.get(id);
    if (label) label.el.hidden = !o.visible;
    if (!map || !started) return true;
    const ids = overlayIds(id);
    [ids.fill, ids.casing, ids.line].forEach((layer) => {
      if (map.getLayer(layer)) map.setLayoutProperty(layer, "visibility", o.visible ? "visible" : "none");
    });
    return true;
  }

  return {
    attach(m) { map = m; },
    markStarted() { started = true; },
    overlays,
    overlayIds,
    cleanOverlayCoords,
    drawOverlay,
    setOverlay,
    setPolygonOverlay,
    setTextOverlay,
    setButtonOverlay,
    removeOverlay,
    setOverlayVisible,
  };
})();
