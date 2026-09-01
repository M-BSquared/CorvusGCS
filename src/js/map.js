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
  let started = false;
  let firstFix = true;

  // Interpolation state for the vehicle marker. TARGET = latest telemetry;
  // DISPLAYED = the value currently shown, eased toward TARGET each frame.
  let vehTarget = null;
  let vehDisplay = null;
  let vehAnimator = null;
  const POS_TAU = 0.22;     // seconds — ease-out time constant for lng/lat
  const HDG_TAU = 0.18;     // seconds — ease-out time constant for heading
  const POS_EPS = 1e-7;     // ~1 cm; below this we consider the marker settled
  const HDG_EPS = 0.05;     // degrees

  const DEFAULT_CENTER = [8.539, 47.378];
  const TILE = {
    satellite: {
      tiles: ["https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}"],
      maxzoom: 19,
    },
    streets: {
      tiles: ["https://server.arcgisonline.com/ArcGIS/rest/services/World_Street_Map/MapServer/tile/{z}/{y}/{x}"],
      maxzoom: 19,
    },
    hybrid: {
      tiles: ["https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}"],
      maxzoom: 19,
      labels: ["https://server.arcgisonline.com/ArcGIS/rest/services/Reference/World_Boundaries_and_Places/MapServer/tile/{z}/{y}/{x}"],
    },
  };

  function icon(name, size) {
    const i = document.createElement("i");
    i.setAttribute("data-lucide", name);
    i.style.width = size + "px";
    i.style.height = size + "px";
    return i;
  }

  function buildVehicleMarker() {
    const el = document.createElement("div");
    el.className = "vehicle-marker";
    el.innerHTML =
      '<div class="v-dir"></div>' +
      '<div class="v-body"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" ' +
      'stroke-linecap="round" stroke-linejoin="round"><path d="M12 2L4 20l8-4 8 4z"/></svg></div>';
    return el;
  }

  function buildHomeMarker() {
    const el = document.createElement("div");
    el.className = "home-marker";
    el.innerHTML = '<div class="h-body"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" ' +
      'stroke-linecap="round" stroke-linejoin="round"><path d="m3 9 9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>' +
      '<polyline points="9 22 9 12 15 12 15 22"/></svg></div>';
    return el;
  }

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
      paint: { "line-color": "#4CC9FF", "line-width": 8, "line-blur": 6, "line-opacity": 0.22 },
    });
    map.addLayer({
      id: "path-line",
      source: "vehicle-path",
      type: "line",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": "#4CC9FF", "line-width": 2.4, "line-opacity": 0.9 },
    });
    pathSource = map.getSource("vehicle-path");
  }

  function initialStyle(key) {
    const spec = TILE[key];
    const sources = {
      base: { type: "raster", tiles: spec.tiles, tileSize: 256, maxzoom: spec.maxzoom },
    };
    const layers = [{ id: "base", type: "raster", source: "base" }];
    if (spec.labels) {
      sources["base-labels-src"] = { type: "raster", tiles: spec.labels, tileSize: 256, maxzoom: spec.maxzoom };
      layers.push({ id: "base-labels", type: "raster", source: "base-labels-src" });
    }
    return { version: 8, sources, layers, glyphs: "https://demotiles.maplibre.org/font/{fontstack}/{range}.pbf" };
  }

  function setBaseLayer(key) {
    const spec = TILE[key];
    if (map.getLayer("base-labels")) map.removeLayer("base-labels");
    if (map.getSource("base-labels-src")) map.removeSource("base-labels-src");
    if (map.getLayer("base")) map.removeLayer("base");
    if (map.getSource("base")) map.removeSource("base");
    map.addSource("base", { type: "raster", tiles: spec.tiles, tileSize: 256, maxzoom: spec.maxzoom });
    map.addLayer({ id: "base", type: "raster", source: "base" }, "path-glow");
    if (spec.labels) {
      map.addSource("base-labels-src", { type: "raster", tiles: spec.labels, tileSize: 256, maxzoom: spec.maxzoom });
      map.addLayer({ id: "base-labels", type: "raster", source: "base-labels-src" }, "path-glow");
    }
  }

  function buildControls(container, layersPopover) {
    const items = [
      { id: "in", icon: "plus", title: "Zoom in" },
      { id: "out", icon: "minus", title: "Zoom out" },
      { id: "center", icon: "crosshair", title: "Center on vehicle" },
      { id: "divider" },
      { id: "layers", icon: "layers", title: "Map layers" },
      { id: "three", icon: "box", title: "3D mode" },
    ];
    items.forEach((it) => {
      if (it.id === "divider") {
        const d = document.createElement("div");
        d.className = "mc-divider";
        container.appendChild(d);
        return;
      }
      const b = document.createElement("button");
      b.className = "mc-btn";
      b.title = it.title;
      b.dataset.act = it.id;
      b.appendChild(icon(it.icon, 17));
      container.appendChild(b);
    });

    const layers = [
      { id: "satellite", label: "Satellite" },
      { id: "hybrid", label: "Hybrid" },
      { id: "streets", label: "Streets" },
    ];
    layersPopover.innerHTML = "<h4>Map layers</h4>";
    let active = "satellite";
    layers.forEach((l) => {
      const b = document.createElement("button");
      b.className = "layer-opt" + (l.id === active ? " active" : "");
      b.dataset.layer = l.id;
      b.innerHTML = '<span class="dot"></span>' + l.label;
      b.addEventListener("click", () => {
        active = l.id;
        setBaseLayer(l.id);
        layersPopover.querySelectorAll(".layer-opt").forEach((x) =>
          x.classList.toggle("active", x.dataset.layer === l.id));
      });
      layersPopover.appendChild(b);
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
        const open = !layersPopover.hidden;
        layersPopover.hidden = open;
        b.classList.toggle("active", !open);
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

  function centerOnVehicle(animate) {
    const s = Corvus.telemetry.getState();
    if (!s || !s.connected) return;
    // Center on the latest reported position (TARGET), not the displayed one —
    // the operator asked to center on the vehicle's actual position.
    if (animate) map.easeTo({ center: s.position, duration: 600 });
    else map.setCenter(s.position);
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
    const body = vehicleMarker.getElement().querySelector(".v-body");
    if (body) body.style.transform = `rotate(${vehDisplay.heading}deg)`;
  }

  function updateVehicle(state) {
    if (!vehicleMarker || !state.connected) return;
    setVehicleTarget(state);

    if (firstFix) {
      firstFix = false;
      map.easeTo({ center: state.position, zoom: 16, duration: 1000 });
    }

    // Path recording stays on the TARGET (latest telemetry) so the track is
    // accurate; interpolation is purely a rendering concern.
    const last = pathCoords[pathCoords.length - 1];
    if (!last || last[0] !== state.position[0] || last[1] !== state.position[1]) {
      pathCoords.push(state.position.slice());
      if (pathCoords.length > 500) pathCoords = pathCoords.slice(-500);
      if (pathSource) {
        pathSource.setData({
          type: "Feature",
          geometry: { type: "LineString", coordinates: pathCoords },
          properties: {},
        });
      }
    }

    if (state.home && state.home[0] !== 0 && homeMarker) {
      homeMarker.setLngLat(state.home);
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
      attributionControl: false,
      dragRotate: true,
      keyboard: false,
    });
    map.on("load", () => {
      addPathLayer();

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

      buildControls(controlsEl, layersPopover);
      Corvus.telemetry.subscribe(updateVehicle);
      started = true;
      window.dispatchEvent(new Event("corvus:mapready"));
    });
  }

  return { init, centerOnVehicle, getMap: () => map, isReady: () => started };
})();
