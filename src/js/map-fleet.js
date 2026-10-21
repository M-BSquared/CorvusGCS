"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.mapFleet — the vehicles on the Home map that are not being flown.

  The vehicle being flown is map.js's: its marker eases at telemetry rate,
  rides in 3D, is followed by the camera and owns the main track. Every other
  vehicle the fleet list reports is drawn here, from the fleet stream: the
  same marker in its own colour with its name under it, eased toward each new
  position, and a track of its own in the same colour.

  The tracks change hands when the flown vehicle does. map.js gives the track
  of the vehicle it stops drawing to stashTrack(), and takes the track of the
  one it starts drawing with takeTrack(), so a vehicle's track follows the
  vehicle and not the role it plays. They survive a restart, like the main
  track, under their own key.

  A vehicle the operator removed (js/fleet.js) is forgotten here too: its
  marker and track go, and a track map.js hands over for it afterwards is
  dropped, until the vehicle is heard again.
*/
Corvus.mapFleet = (function () {
  const STORE_KEY = "corvus.map.fleetTracks";
  const SAVE_DELAY_MS = 5000;
  const MIN_MOVE_M = 2.0;
  const MAX_POINTS = 5000;      // per vehicle, all flights
  const MAX_VEHICLES = 16;      // tracks remembered
  const POS_TAU = 0.35;
  const HDG_TAU = 0.25;

  let map = null;
  let source = null;
  // id -> {current: [[lng,lat]...], past: [[[lng,lat]...]...], armed}
  let tracks = new Map();
  // id -> {marker, el, label, target, display}
  const markers = new Map();
  let saveTimer = null;
  let animator = null;
  let lastSnapshot = null;
  // What the drawn paths depend on besides the points: whose track map.js
  // draws itself, and each track's colour. See paintKey.
  let paintedKey = null;
  // Vehicles removed from the list: nothing of theirs is drawn or kept.
  const removed = new Set();

  function metres(a, b) {
    const R = 6371000;
    const lat1 = (a[1] * Math.PI) / 180;
    const lat2 = (b[1] * Math.PI) / 180;
    const x = ((b[0] - a[0]) * Math.PI / 180) * Math.cos((lat1 + lat2) / 2);
    const y = lat2 - lat1;
    return Math.sqrt(x * x + y * y) * R;
  }

  function validFlight(f) {
    return Array.isArray(f) && f.every((c) =>
      Array.isArray(c) && c.length === 2 && isFinite(c[0]) && isFinite(c[1]));
  }

  function storage() {
    try { return window.localStorage || null; } catch (_e) { return null; }
  }

  function restore() {
    const s = storage();
    if (!s) return;
    let saved = null;
    try { saved = JSON.parse(s.getItem(STORE_KEY) || "null"); } catch (_e) { return; }
    if (!saved || saved.v !== 1 || !saved.tracks || typeof saved.tracks !== "object") return;
    Object.keys(saved.tracks).slice(0, MAX_VEHICLES).forEach((id) => {
      const t = saved.tracks[id] || {};
      const current = validFlight(t.current) ? t.current : [];
      const past = (Array.isArray(t.past) ? t.past : []).filter((f) => validFlight(f) && f.length >= 2);
      if (current.length >= 2 || past.length) tracks.set(id, { current, past, armed: null });
    });
  }

  function scheduleSave() {
    if (saveTimer !== null) return;
    saveTimer = window.setTimeout(save, SAVE_DELAY_MS);
  }

  function save() {
    if (saveTimer !== null) window.clearTimeout(saveTimer);
    saveTimer = null;
    const s = storage();
    if (!s) return;
    const round = (f) => f.map((c) => [+c[0].toFixed(7), +c[1].toFixed(7)]);
    const out = {};
    tracks.forEach((t, id) => {
      if (t.current.length < 2 && !t.past.length) return;
      out[id] = { current: round(t.current), past: t.past.map(round) };
    });
    try {
      if (Object.keys(out).length) s.setItem(STORE_KEY, JSON.stringify({ v: 1, tracks: out }));
      else s.removeItem(STORE_KEY);
    } catch (_e) { /* quota: the tracks still live in memory */ }
  }

  function trackFor(id) {
    let t = tracks.get(id);
    if (!t) {
      if (tracks.size >= MAX_VEHICLES) {
        const oldest = tracks.keys().next().value;
        tracks.delete(oldest);
      }
      t = { current: [], past: [], armed: null };
      tracks.set(id, t);
    }
    return t;
  }

  function pointCount(t) {
    return t.past.reduce((n, f) => n + f.length, t.current.length);
  }

  /** Extend *id*'s track with one sample. Returns true when it changed. */
  function record(id, position, armed) {
    const t = trackFor(id);
    let changed = false;
    if (typeof armed === "boolean") {
      if (t.armed === false && armed && t.current.length >= 2) {
        t.past.push(t.current);
        t.current = [];
        changed = true;
      }
      t.armed = armed;
    }
    if (!Array.isArray(position) || position.length < 2) return changed;
    const p = [Number(position[0]), Number(position[1])];
    if (!isFinite(p[0]) || !isFinite(p[1]) || (p[0] === 0 && p[1] === 0)) return changed;
    const last = t.current[t.current.length - 1];
    if (last && metres(last, p) < MIN_MOVE_M) return changed;
    t.current.push(p);
    if (pointCount(t) > MAX_POINTS) {
      if (t.past.length) {
        t.past[0].shift();
        if (t.past[0].length < 2) t.past.shift();
      } else {
        t.current.shift();
      }
    }
    return true;
  }

  /** The vehicle map.js draws itself, so it is skipped here. */
  function ownerId() {
    return (Corvus.map && typeof Corvus.map.trackOwner === "function") ? Corvus.map.trackOwner() : null;
  }

  function trackColor(id) {
    const c = Corvus.fleet ? Corvus.fleet.colorOf(id) : "";
    return c || ((Corvus.ui && Corvus.ui.token) ? Corvus.ui.token("--track", "#E4322F") : "#E4322F");
  }

  function geometry() {
    const owner = ownerId();
    const features = [];
    tracks.forEach((t, id) => {
      if (id === owner) return;
      const color = trackColor(id);
      const plain = Corvus.fleet.tint(color, 0.45);
      const rainbow = Corvus.map && Corvus.map.isTrackRainbow && Corvus.map.isTrackRainbow();
      t.past.forEach((f, i) => {
        if (f.length >= 2) {
          features.push({ type: "Feature", properties: { color: rainbow ? Corvus.fleet.rainbowTone(i) : plain, past: 1 },
            geometry: { type: "LineString", coordinates: f } });
        }
      });
      if (t.current.length >= 2) {
        features.push({ type: "Feature", properties: { color, past: 0 },
          geometry: { type: "LineString", coordinates: t.current } });
      }
    });
    return { type: "FeatureCollection", features };
  }

  function paintKey() {
    const parts = [String(ownerId())];
    tracks.forEach((_t, id) => parts.push(id + "=" + trackColor(id)));
    return parts.join("|");
  }

  function paint() {
    paintedKey = paintKey();
    if (source) source.setData(geometry());
    if (Corvus.map && typeof Corvus.map.updateTrackControl === "function") {
      Corvus.map.updateTrackControl();
    }
  }

  function addLayers() {
    if (!map || source) return;
    map.addSource("fleet-paths", { type: "geojson", data: geometry() });
    // Under the flown vehicle's track, so where the two cross the aircraft
    // being flown is the one on top.
    const before = map.getLayer("path-past-glow") ? "path-past-glow" : undefined;
    const casing = (Corvus.ui && Corvus.ui.token) ? Corvus.ui.token("--bg", "#0B0E12") : "#0B0E12";
    map.addLayer({
      id: "fleet-path-casing", type: "line", source: "fleet-paths",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": casing, "line-width": ["case", ["==", ["get", "past"], 1], 4, 5],
        "line-opacity": 0.5 },
    }, before);
    map.addLayer({
      id: "fleet-path-line", type: "line", source: "fleet-paths",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": ["get", "color"], "line-width": ["case", ["==", ["get", "past"], 1], 2, 2.4],
        "line-opacity": ["case", ["==", ["get", "past"], 1], 0.8, 0.95] },
    }, before);
    source = map.getSource("fleet-paths");
  }

  function repaintTheme() {
    if (!map) return;
    const casing = Corvus.ui.token("--bg", "#0B0E12");
    if (map.getLayer("fleet-path-casing")) map.setPaintProperty("fleet-path-casing", "line-color", casing);
    paint();
    markers.forEach((m, id) => Corvus.fleet.paintElement(m.el, Corvus.fleet.colorOf(id)));
  }

  /* ---------------- markers ---------------- */

  function buildMarker(v) {
    const el = Corvus.map.buildVehicleMarker();
    el.classList.add("fleet-marker");
    el.title = v.name;
    const label = document.createElement("span");
    label.className = "v-label";
    el.appendChild(label);
    el.addEventListener("click", (e) => {
      e.stopPropagation();
      if (Corvus.fleet) Corvus.fleet.openDialog(v.id);
    });
    const marker = new maplibregl.Marker({ element: el, anchor: "center", rotationAlignment: "map" })
      .setLngLat(v.position).addTo(map);
    const pos = { lng: v.position[0], lat: v.position[1], heading: Number(v.heading) || 0 };
    return { marker, el, label, target: Object.assign({}, pos), display: Object.assign({}, pos) };
  }

  function renderMarker(m) {
    m.marker.setLngLat([m.display.lng, m.display.lat]);
    const body = m.el.querySelector(".v-body");
    if (body) body.style.transform = `rotate(${m.display.heading}deg)`;
  }

  function ensureAnimator() {
    if (animator || !Corvus.anim) return;
    animator = {
      step(dt) {
        let moving = false;
        markers.forEach((m) => {
          const a = Corvus.anim;
          const nx = a.approach(m.display.lng, m.target.lng, dt, POS_TAU);
          const ny = a.approach(m.display.lat, m.target.lat, dt, POS_TAU);
          const nh = a.approachAngle(m.display.heading, m.target.heading, dt, HDG_TAU);
          const done = Math.abs(nx - m.target.lng) < 1e-7 && Math.abs(ny - m.target.lat) < 1e-7 &&
            a.settledAngle(nh, m.target.heading, 0.05);
          m.display.lng = done ? m.target.lng : nx;
          m.display.lat = done ? m.target.lat : ny;
          m.display.heading = done ? m.target.heading : nh;
          renderMarker(m);
          if (!done) moving = true;
        });
        return moving;
      },
    };
    Corvus.anim.add(animator);
  }

  function syncMarkers(snapshot) {
    if (!map) return;
    const owner = ownerId();
    const multi = Corvus.fleet.isMulti();
    const keep = new Set();
    snapshot.vehicles.forEach((v) => {
      if (v.id === owner || !Array.isArray(v.position)) return;
      keep.add(v.id);
      let m = markers.get(v.id);
      if (!m) {
        m = buildMarker(v);
        markers.set(v.id, m);
      }
      m.target.lng = v.position[0];
      m.target.lat = v.position[1];
      if (typeof v.heading === "number") m.target.heading = v.heading;
      Corvus.fleet.paintElement(m.el, v.color);
      m.el.classList.toggle("is-stale", !!v.stale);
      const text = v.stale ? `${v.name} (lost)` : v.name;
      if (m.label.textContent !== text) m.label.textContent = text;
      m.label.hidden = !multi;
      m.el.title = text;
    });
    markers.forEach((m, id) => {
      if (!keep.has(id)) {
        m.marker.remove();
        markers.delete(id);
      }
    });
    if (markers.size) {
      ensureAnimator();
      if (Corvus.anim.reducedMotion()) {
        markers.forEach((m) => { Object.assign(m.display, m.target); renderMarker(m); });
      } else {
        Corvus.anim.wake();
      }
    }
  }

  function onSnapshot(snapshot) {
    lastSnapshot = snapshot;
    snapshot.vehicles.forEach((v) => removed.delete(v.id));
    const owner = ownerId();
    let changed = false;
    snapshot.vehicles.forEach((v) => {
      if (v.id === owner || v.stale) return;
      if (record(v.id, v.position, v.armed)) changed = true;
    });
    if (changed) scheduleSave();
    syncMarkers(snapshot);
    // Only when something drawn changed. Snapshots arrive several times a
    // second, and in 3D every setData re-renders the line into each terrain
    // tile's texture: unchanged paths were costing the panning frame rate.
    if (changed || paintKey() !== paintedKey) paint();
  }

  /** map.js hands over the track of the vehicle it stops drawing. */
  function stashTrack(id, track) {
    if (!id || !track) return;
    if (removed.has(id)) {
      tracks.delete(id);
      scheduleSave();
      paint();
      return;
    }
    const current = validFlight(track.current) ? track.current.slice() : [];
    const past = (Array.isArray(track.past) ? track.past : []).filter((f) => validFlight(f) && f.length >= 2);
    tracks.delete(id);
    if (current.length >= 2 || past.length) {
      trackFor(id);
      tracks.set(id, { current, past, armed: null });
    }
    scheduleSave();
    if (lastSnapshot) syncMarkers(lastSnapshot);
    paint();
  }

  /** map.js takes the track of the vehicle it starts drawing. */
  function takeTrack(id) {
    const t = tracks.get(id);
    tracks.delete(id);
    const m = markers.get(id);
    if (m) { m.marker.remove(); markers.delete(id); }
    scheduleSave();
    paint();
    if (!t) return null;
    return { current: t.current, past: t.past };
  }

  /** The operator removed *id*: drop its marker and its track. */
  function forget(id) {
    if (!id) return;
    removed.add(id);
    tracks.delete(id);
    const m = markers.get(id);
    if (m) { m.marker.remove(); markers.delete(id); }
    save();
    paint();
  }

  function clearTracks() {
    tracks = new Map();
    save();
    paint();
  }

  function hasTracks() {
    const owner = ownerId();
    for (const [id, t] of tracks) {
      if (id !== owner && (t.current.length >= 2 || t.past.length)) return true;
    }
    return false;
  }

  function attach(targetMap) {
    if (map) return;
    map = targetMap;
    addLayers();
    if (Corvus.ui && Corvus.ui.onThemeChange) Corvus.ui.onThemeChange(repaintTheme);
    window.addEventListener("pagehide", save);
    if (Corvus.fleet) Corvus.fleet.subscribe(onSnapshot);
  }

  restore();

  return {
    attach, stashTrack, takeTrack, clearTracks, hasTracks, forget, repaint: paint,
    // test hooks
    _record: record,
    _onSnapshot: (snapshot) => onSnapshot(snapshot),
    _tracks: () => tracks,
    _geometry: geometry,
    _save: save,
    _restore: () => { tracks = new Map(); restore(); },
  };
})();
