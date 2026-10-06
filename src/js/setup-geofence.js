"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.setupGeofence: the Geofence card on Safety & Sensors.

  The area is drawn the way the planner's Area pattern is: click a corner,
  click the next, Enter or a double click closes it, right click takes the
  last corner back, Escape gives up. Once it is closed every corner can be
  dragged, a right click on a corner removes it, and the small handle in the
  middle of an edge adds one there.

  Every finished edit is saved on this station at once (Corvus.geofence), so
  the Home map's outline follows the card. Putting it on the vehicle is its
  own press, because a fence that changes under a flying aircraft is not
  something that should happen as a side effect of tidying a corner.

  The action on leaving the area is a parameter, and arrives as a field of the
  Geofence section of GET /api/safety, built by the connected stack's schema.
  So are the writes that make a stored polygon act (ArduPilot ignores it until
  FENCE_TYPE and FENCE_ENABLE say otherwise): `enable_writes`, sent after a
  successful upload.

  card(opts) -> {el, destroy}. opts:
    section     the geofence section of /api/safety, or null when not connected
    connected   whether a vehicle answered
    armed()     whether the vehicle is armed now
    fieldGrid   (fields) -> element, the page's own parameter form
    register    (el, recheck) -> void, the page's armed gate
    writeParams (writes) -> Promise, the page's write chain
    reload      () -> void, re-read the page after writes
*/
Corvus.setupGeofence = (function () {
  const SOURCE = "geofence";
  const DEFAULT_CENTER = [11.6, 48.1];
  const DEFAULT_ZOOM = 15;
  const MIN_CORNER_GAP_M = 0.5;

  function distanceM(a, b) {
    const R = 6371008.8;
    const rad = Math.PI / 180;
    const dLat = (b[1] - a[1]) * rad;
    const dLon = (b[0] - a[0]) * rad;
    const x = dLon * Math.cos(((a[1] + b[1]) / 2) * rad);
    return Math.sqrt(x * x + dLat * dLat) * R;
  }

  /** Drop corners that land on the one before: a double click places two. Pure. */
  function distinctCorners(pts) {
    const out = [];
    pts.forEach((p) => {
      const last = out[out.length - 1];
      if (!last || distanceM(last, p) > MIN_CORNER_GAP_M) out.push(p);
    });
    if (out.length > 1 && distanceM(out[0], out[out.length - 1]) <= MIN_CORNER_GAP_M) out.pop();
    return out;
  }

  /** Why *polygon* cannot be kept, or "". Pure. */
  function problemOf(polygon, maxVertices) {
    const G = Corvus.geofence;
    if (polygon.length < 3) return "An area needs at least 3 corners.";
    if (maxVertices && polygon.length > maxVertices) {
      return `An area may have at most ${maxVertices} corners.`;
    }
    if (G.selfIntersects(polygon)) return "The edges of the area cross each other.";
    return "";
  }

  function formatArea(m2) {
    if (m2 >= 1e6) return `${(m2 / 1e6).toFixed(2)} km²`;
    if (m2 >= 1e4) return `${(m2 / 1e4).toFixed(2)} ha`;
    return `${Math.round(m2)} m²`;
  }

  function card(opts) {
    const o = opts || {};
    const S = Corvus.setupShared;
    const ui = Corvus.ui;
    const G = Corvus.geofence;
    const section = o.section || null;

    let destroyed = false;
    let map = null;
    let mapReady = false;
    let fence = G.get();
    let poly = fence.polygon.map((p) => p.slice());
    let drawing = false;
    let draft = [];
    let cursor = null;
    let dragging = false;
    let busy = false;
    let corners = [];
    let mids = [];

    const el = S.el("div", "page-card safety-card geofence-card");
    el.dataset.section = "geofence";
    el.appendChild(S.sectionTitle((section && section.title) || "Geofence"));
    el.appendChild(S.el("div", "field-hint", (section && section.hint)
      || "Draw the area the vehicle may fly in. Leaving it triggers the action below."));

    const mapBox = S.el("div", "geofence-map");
    const mapHint = S.el("div", "geofence-map-hint");
    mapHint.hidden = true;
    mapBox.appendChild(mapHint);
    const expandBtn = ui.button({ variant: "secondary", size: "sm", icon: "maximize-2",
      label: "Large map", className: "geofence-expand", title: "Edit the area on a large map" });
    mapBox.appendChild(expandBtn);
    // Where the map sits in the card. While the large editor is open the map
    // moves into it and this slot says so, so the card keeps its height.
    const mapSlot = S.el("div", "geofence-map-slot");
    mapSlot.appendChild(mapBox);
    const slotNote = S.el("div", "geofence-map-away", "Editing on the large map.");
    slotNote.hidden = true;
    mapSlot.appendChild(slotNote);
    el.appendChild(mapSlot);

    // The large editor: the same map, nearly the whole window, with its own
    // bar so the operator never has to look back at the card while drawing.
    const editor = S.el("div", "geofence-editor-overlay");
    const editorPanel = S.el("div", "geofence-editor");
    const editorHead = S.el("div", "geofence-editor-head");
    const editorText = S.el("div", "geofence-editor-text");
    editorText.appendChild(S.el("div", "geofence-editor-title", "Geofence area"));
    const editorSummary = S.el("div", "geofence-editor-summary");
    editorText.appendChild(editorSummary);
    editorHead.appendChild(editorText);
    const editorDraw = ui.button({ variant: "secondary", size: "sm", icon: "pen-line", label: "Draw area" });
    const editorDelete = ui.button({ variant: "secondary", size: "sm", icon: "trash-2", label: "Delete area" });
    const editorDone = ui.button({ variant: "primary", size: "sm", icon: "check", label: "Done" });
    [editorDraw, editorDelete, editorDone].forEach((b) => editorHead.appendChild(b));
    editorPanel.appendChild(editorHead);
    const editorBody = S.el("div", "geofence-editor-body");
    editorPanel.appendChild(editorBody);
    editor.appendChild(editorPanel);
    let expanded = false;

    const summary = S.el("div", "geofence-summary");
    el.appendChild(summary);

    const row = S.el("div", "geofence-actions");
    const drawBtn = ui.button({ variant: "secondary", size: "sm", icon: "pen-line", label: "Draw area",
      className: "geofence-draw" });
    const deleteBtn = ui.button({ variant: "secondary", size: "sm", icon: "trash-2", label: "Delete area",
      className: "geofence-delete" });
    const uploadBtn = ui.button({ variant: "primary", size: "sm", icon: "upload", label: "Upload to vehicle",
      className: "geofence-upload" });
    const removeBtn = ui.button({ variant: "secondary", size: "sm", icon: "circle-x",
      label: "Remove from vehicle", className: "geofence-remove" });
    [drawBtn, deleteBtn, uploadBtn, removeBtn].forEach((b) => row.appendChild(b));
    el.appendChild(row);

    const status = S.el("div", "params-row-status geofence-status");
    el.appendChild(status);

    const warn = S.el("div", "field-hint geofence-warn", (section && section.inactive) || "");
    warn.hidden = !(section && section.inactive);
    el.appendChild(warn);

    const fields = (section && section.fields) || [];
    if (fields.length && typeof o.fieldGrid === "function") {
      el.appendChild(o.fieldGrid(fields));
    } else if (!o.connected) {
      el.appendChild(S.el("div", "field-hint",
        "Connect to a vehicle to choose what it does when it leaves the area."));
    }

    const showSw = ui.toggle({
      id: "geofenceShowHome",
      value: fence.show_on_map,
      ariaLabel: "Show on the Home map",
      onChange: (next) => G.setShowOnMap(next).catch((error) => {
        say("err", error.message || "Could not save the setting");
        throw error;
      }),
    });
    el.appendChild(ui.field({
      label: "Show on the Home map",
      control: showSw.el,
      className: "field-switch",
      hint: "Draws the area as a dashed outline on the Home map.",
    }));

    // ---- Buttons and text -----------------------------------------------------

    const armed = () => (typeof o.armed === "function" ? !!o.armed() : false);

    function say(cls, text) { S.setFieldStatus(status, cls, text); }

    function sync() {
      const has = poly.length >= 3;
      setLabel(drawBtn, drawing ? "Finish" : has ? "Redraw" : "Draw area");
      drawBtn.disabled = busy || (drawing && distinctCorners(draft).length < 3);
      deleteBtn.disabled = busy || drawing || !has;
      setLabel(editorDraw, drawing ? "Finish" : has ? "Redraw" : "Draw area");
      editorDraw.disabled = drawBtn.disabled;
      editorDelete.disabled = deleteBtn.disabled;
      editorDone.disabled = busy;
      expandBtn.hidden = expanded || !map;
      uploadBtn.disabled = busy || drawing || !has || !o.connected || armed();
      removeBtn.disabled = busy || drawing || !o.connected || armed() || fence.vehicle_has_fence === false;
      uploadBtn.title = !o.connected ? "Connect to a vehicle first"
        : armed() ? "Not while the vehicle is armed" : "";
      removeBtn.title = uploadBtn.title;

      if (drawing) {
        const n = distinctCorners(draft).length;
        summary.textContent = `Drawing: ${n} ${n === 1 ? "corner" : "corners"}.`;
      } else if (!has) {
        summary.textContent = "No area drawn.";
      } else {
        const where = !o.connected ? ""
          : fence.on_vehicle === true ? " On the vehicle."
          : fence.on_vehicle === false ? " Not on the vehicle in this form, upload it."
          : " Not uploaded over this link yet.";
        const vehicle = vehiclePosition();
        const outside = vehicle && !G.contains(poly, vehicle) ? " The vehicle is outside this area." : "";
        summary.textContent = `${poly.length} corners, ${formatArea(G.areaM2(poly))}.${where}${outside}`;
      }
      summary.classList.toggle("is-stale", !drawing && has && o.connected && fence.on_vehicle !== true);
      editorSummary.textContent = summary.textContent;
    }

    function setLabel(btn, text) {
      const span = Array.from(btn.children || []).find((c) => c.tagName === "SPAN");
      if (span) span.textContent = text;
    }

    function showHint(text) {
      mapHint.textContent = text || "";
      mapHint.hidden = !text;
    }

    [drawBtn, deleteBtn, uploadBtn, removeBtn].forEach((b) => {
      if (typeof o.register === "function") o.register(b, () => sync());
    });

    // Drawing happens on the large map: corners clicked onto a postcard are
    // corners in the wrong place.
    function onDraw() {
      if (drawing) { finishDrawing(); return; }
      openEditor();
      startDrawing();
    }
    drawBtn.addEventListener("click", onDraw);
    editorDraw.addEventListener("click", onDraw);
    editorDelete.addEventListener("click", () => deleteBtn.click());
    editorDone.addEventListener("click", () => closeEditor());
    expandBtn.addEventListener("click", () => openEditor());

    function openEditor() {
      if (expanded || !map || destroyed) return;
      expanded = true;
      editorBody.appendChild(mapBox);
      slotNote.hidden = false;
      document.body.appendChild(editor);
      S.refreshIcons();
      settle();
      sync();
    }

    function closeEditor() {
      if (!expanded) return;
      if (drawing) cancelDrawing();
      expanded = false;
      mapSlot.insertBefore(mapBox, slotNote);
      slotNote.hidden = true;
      if (editor.parentNode) editor.parentNode.removeChild(editor);
      settle();
      sync();
    }

    /** The map's container changed size: redraw at the new one and frame the area. */
    function settle() {
      window.requestAnimationFrame(() => {
        if (!map || destroyed) return;
        map.resize();
        fitArea(false);
      });
    }

    editor.addEventListener("click", (event) => {
      if (event.target === editor && !drawing) closeEditor();
    });

    deleteBtn.addEventListener("click", () => {
      if (!poly.length) return;
      keep([]).then(() => say("ok", fence.vehicle_has_fence
        ? "Area deleted here. The vehicle still holds its fence until you remove it."
        : "Area deleted."));
    });

    uploadBtn.addEventListener("click", () => {
      if (busy) return;
      busy = true;
      sync();
      say("pending", "Uploading the area…");
      G.upload().then(() => {
        const writes = (section && section.enable_writes) || [];
        if (!writes.length || typeof o.writeParams !== "function") return false;
        say("pending", "Switching the fence on…");
        return o.writeParams(writes).then(() => true);
      }).then((wrote) => {
        if (destroyed) return;
        say("ok", "The vehicle holds the area.");
        S.notify("success", "Geofence uploaded to the vehicle");
        if (wrote && typeof o.reload === "function") o.reload();
      }).catch((error) => {
        if (destroyed) return;
        say("err", (error && error.message) || "Upload failed");
        S.notify("error", `Geofence upload failed: ${(error && error.message) || "no answer"}`);
      }).finally(() => {
        busy = false;
        if (!destroyed) sync();
      });
    });

    removeBtn.addEventListener("click", () => {
      if (busy) return;
      busy = true;
      sync();
      say("pending", "Removing the fence from the vehicle…");
      G.clearVehicle().then(() => {
        if (!destroyed) say("ok", "The vehicle holds no fence now. The area is still kept here.");
      }).catch((error) => {
        if (!destroyed) say("err", (error && error.message) || "Could not remove the fence");
      }).finally(() => {
        busy = false;
        if (!destroyed) sync();
      });
    });

    /** Save *next* on this station; on refusal the last saved area comes back. */
    function keep(next) {
      busy = true;
      sync();
      return G.savePolygon(next).catch((error) => {
        if (!destroyed) say("err", (error && error.message) || "Could not save the area");
        poly = fence.polygon.map((p) => p.slice());
        redraw();
        throw error;
      }).finally(() => {
        busy = false;
        if (!destroyed) sync();
      });
    }

    const unsub = G.onChange((next) => {
      fence = next;
      if (!drawing && !dragging) {
        poly = next.polygon.map((p) => p.slice());
        redraw();
      }
      showSw.setValue(next.show_on_map);
      sync();
    });

    // ---- Drawing --------------------------------------------------------------

    function startDrawing() {
      if (!map) return;
      drawing = true;
      draft = [];
      cursor = null;
      say("", "");
      if (map.doubleClickZoom) map.doubleClickZoom.disable();
      mapBox.classList.add("is-drawing");
      showHint("Click to place corners. Enter or a double click closes the area, "
        + "right click undoes a corner, Escape cancels.");
      redraw();
      sync();
    }

    function stopDrawing() {
      drawing = false;
      draft = [];
      cursor = null;
      mapBox.classList.remove("is-drawing");
      if (map && map.doubleClickZoom) map.doubleClickZoom.enable();
      showHint("");
    }

    function finishDrawing() {
      const pts = distinctCorners(draft);
      const problem = problemOf(pts, fence.max_vertices);
      if (problem) {
        showHint(problem + " Keep clicking, or press Escape to cancel.");
        return;
      }
      stopDrawing();
      poly = pts;
      redraw();
      keep(pts).then(() => say("ok", o.connected
        ? "Area saved. Upload it to put it on the vehicle." : "Area saved."));
    }

    function cancelDrawing() {
      stopDrawing();
      redraw();
      sync();
    }

    function onKey(event) {
      if (destroyed) return;
      if (!drawing) {
        if (expanded && event.key === "Escape") { event.preventDefault(); closeEditor(); }
        return;
      }
      if (event.key === "Escape") { event.preventDefault(); cancelDrawing(); }
      else if (event.key === "Enter") { event.preventDefault(); finishDrawing(); }
      else if (event.key === "Backspace") { event.preventDefault(); undoCorner(); }
    }

    function undoCorner() {
      if (!draft.length) return;
      draft.pop();
      redraw();
      sync();
    }

    // ---- The map --------------------------------------------------------------

    function collection() {
      const features = [];
      const ring = drawing ? draft.concat(cursor ? [cursor] : []) : poly;
      if (ring.length >= 3) {
        features.push({ type: "Feature", properties: { kind: "area" },
          geometry: { type: "Polygon", coordinates: [ring.concat([ring[0]])] } });
      } else if (ring.length === 2) {
        features.push({ type: "Feature", properties: { kind: "area" },
          geometry: { type: "LineString", coordinates: ring } });
      }
      if (drawing) {
        draft.forEach((p) => features.push({ type: "Feature", properties: { kind: "corner" },
          geometry: { type: "Point", coordinates: p } }));
      }
      const vehicle = vehiclePosition();
      if (vehicle) {
        features.push({ type: "Feature", properties: { kind: "vehicle" },
          geometry: { type: "Point", coordinates: vehicle } });
      }
      return { type: "FeatureCollection", features };
    }

    function vehiclePosition() {
      const s = Corvus.telemetry && Corvus.telemetry.getState && Corvus.telemetry.getState();
      const p = s && s.position;
      return p && isFinite(p[0]) && isFinite(p[1]) && (p[0] || p[1]) ? [p[0], p[1]] : null;
    }

    function redraw() {
      if (!map || !mapReady) return;
      const source = map.getSource(SOURCE);
      if (source) source.setData(collection());
      drawHandles();
    }

    function drawHandles() {
      corners.forEach((m) => m.remove());
      mids.forEach((m) => m.remove());
      corners = [];
      mids = [];
      if (!map || drawing || poly.length < 3) return;
      poly.forEach((p, i) => {
        const node = document.createElement("div");
        node.className = "geofence-corner";
        node.title = "Drag to move. Right click removes the corner.";
        node.addEventListener("contextmenu", (event) => {
          event.preventDefault();
          event.stopPropagation();
          if (armed() || busy) return;
          if (poly.length <= 3) { say("err", "An area needs at least 3 corners."); return; }
          const next = poly.filter((_p, k) => k !== i);
          const problem = problemOf(next, fence.max_vertices);
          if (problem) { say("err", problem); return; }
          poly = next;
          redraw();
          keep(next).catch(() => {});
        });
        const marker = new maplibregl.Marker({ element: node, draggable: true })
          .setLngLat(p).addTo(map);
        marker.on("dragstart", () => { dragging = true; });
        marker.on("drag", () => {
          const at = marker.getLngLat();
          poly[i] = [at.lng, at.lat];
          const source = map.getSource(SOURCE);
          if (source) source.setData(collection());
        });
        marker.on("dragend", () => {
          dragging = false;
          const problem = problemOf(poly, fence.max_vertices);
          if (problem) {
            say("err", problem + " The corner went back.");
            poly = fence.polygon.map((q) => q.slice());
            redraw();
            return;
          }
          const next = poly.map((q) => q.slice());
          redraw();
          keep(next).then(() => say("ok", "Area saved.")).catch(() => {});
        });
        corners.push(marker);
      });
      if (poly.length >= (fence.max_vertices || 64)) return;
      poly.forEach((a, i) => {
        const b = poly[(i + 1) % poly.length];
        const node = document.createElement("div");
        node.className = "geofence-mid";
        node.title = "Click to add a corner here";
        node.addEventListener("click", (event) => {
          event.stopPropagation();
          if (armed() || busy) return;
          const next = poly.slice();
          next.splice(i + 1, 0, [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2]);
          poly = next;
          redraw();
          keep(next).catch(() => {});
        });
        mids.push(new maplibregl.Marker({ element: node })
          .setLngLat([(a[0] + b[0]) / 2, (a[1] + b[1]) / 2]).addTo(map));
      });
    }

    function openingView() {
      if (poly.length >= 3) return null;
      const vehicle = vehiclePosition();
      if (vehicle) return { center: vehicle, zoom: 16 };
      const shared = Corvus.map && typeof Corvus.map.missionOpenView === "function"
        ? Corvus.map.missionOpenView() : null;
      if (shared && Array.isArray(shared.center)) return { center: shared.center, zoom: shared.zoom };
      return { center: DEFAULT_CENTER, zoom: DEFAULT_ZOOM };
    }

    function fitArea(animate) {
      if (!map || poly.length < 3) return;
      const lons = poly.map((p) => p[0]);
      const lats = poly.map((p) => p[1]);
      map.fitBounds([[Math.min(...lons), Math.min(...lats)], [Math.max(...lons), Math.max(...lats)]],
        { padding: expanded ? 90 : 36, maxZoom: 18, duration: animate ? 400 : 0 });
    }

    function initMap() {
      if (destroyed || map || typeof maplibregl === "undefined") return;
      const layer = (Corvus.map && typeof Corvus.map.getBaseLayer === "function"
        && Corvus.map.getBaseLayer()) || "satellite";
      const spec = Corvus.map && typeof Corvus.map.layerSpec === "function"
        ? Corvus.map.layerSpec(layer) : null;
      const view = openingView() || { center: DEFAULT_CENTER, zoom: DEFAULT_ZOOM };
      try {
        map = new maplibregl.Map({
          container: mapBox,
          center: view.center,
          zoom: view.zoom,
          attributionControl: false,
          keyboard: false,
          style: {
            version: 8,
            sources: {
              base: {
                type: "raster",
                tiles: [`/api/tiles/${layer}/{z}/{x}/{y}.png`],
                tileSize: 256,
                maxzoom: (spec && spec.maxzoom) || 19,
                attribution: (spec && spec.attribution) || "",
              },
            },
            layers: [{ id: "base", type: "raster", source: "base" }],
          },
        });
      } catch (error) {
        console.warn("Corvus: the geofence map could not start", error);
        map = null;
        mapBox.classList.add("is-unavailable");
        showHint("The map could not start here.");
        return;
      }
      if (Corvus.map && typeof Corvus.map.addAttribution === "function") Corvus.map.addAttribution(map);
      if (typeof map.setPixelRatio === "function" && Corvus.scale) {
        map.setPixelRatio((window.devicePixelRatio || 1) * Corvus.scale.get());
      }
      map.addControl(new maplibregl.NavigationControl({ showCompass: false }), "top-right");
      map.on("load", () => {
        if (destroyed) return;
        const warning = ui.token("--warning", "#F5A524");
        map.addSource(SOURCE, { type: "geojson", data: collection() });
        map.addLayer({ id: "geofence-fill", type: "fill", source: SOURCE,
          filter: ["==", ["geometry-type"], "Polygon"],
          paint: { "fill-color": warning, "fill-opacity": 0.12 } });
        map.addLayer({ id: "geofence-line", type: "line", source: SOURCE,
          filter: ["!=", ["geometry-type"], "Point"],
          layout: { "line-join": "round" },
          paint: { "line-color": warning, "line-width": 2.5, "line-dasharray": [2, 1.5] } });
        map.addLayer({ id: "geofence-draft-corner", type: "circle", source: SOURCE,
          filter: ["==", ["get", "kind"], "corner"],
          paint: { "circle-radius": 5, "circle-color": warning,
            "circle-stroke-color": ui.token("--bg", "#0B0E12"), "circle-stroke-width": 1.5 } });
        map.addLayer({ id: "geofence-vehicle", type: "circle", source: SOURCE,
          filter: ["==", ["get", "kind"], "vehicle"],
          paint: { "circle-radius": 6, "circle-color": ui.token("--vehicle", "#2BC4E4"),
            "circle-stroke-color": "#FFFFFF", "circle-stroke-width": 2 } });
        mapReady = true;
        redraw();
        fitArea(false);
        sync();
      });
      map.on("click", (event) => {
        if (!drawing || !event || !event.lngLat) return;
        draft.push([event.lngLat.lng, event.lngLat.lat]);
        if (distinctCorners(draft).length >= 3) {
          showHint("Enter or a double click closes the area. Right click undoes a corner.");
        }
        redraw();
        sync();
      });
      map.on("dblclick", (event) => {
        if (!drawing) return;
        if (event && event.preventDefault) event.preventDefault();
        finishDrawing();
      });
      map.on("mousemove", (event) => {
        if (!drawing || !draft.length || !event || !event.lngLat) return;
        cursor = [event.lngLat.lng, event.lngLat.lat];
        const source = map.getSource(SOURCE);
        if (source) source.setData(collection());
      });
      map.on("contextmenu", (event) => {
        if (event && event.preventDefault) event.preventDefault();
        if (drawing) undoCorner();
      });
    }

    const unsubTelemetry = Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function"
      ? (() => {
        let last = 0;
        return Corvus.telemetry.subscribe(() => {
          const now = Date.now();
          if (destroyed || now - last < 1000) return;
          last = now;
          sync();
          const source = mapReady && map && map.getSource(SOURCE);
          if (source && !dragging) source.setData(collection());
        });
      })()
      : null;

    document.addEventListener("keydown", onKey);
    // The card is built before the page puts it in the document, and MapLibre
    // needs a container with a size.
    const startTimer = window.setTimeout(initMap, 0);
    sync();

    function destroy() {
      destroyed = true;
      window.clearTimeout(startTimer);
      if (editor.parentNode) editor.parentNode.removeChild(editor);
      document.removeEventListener("keydown", onKey);
      try { unsub(); } catch (_e) {}
      if (unsubTelemetry) { try { unsubTelemetry(); } catch (_e) {} }
      corners.forEach((m) => m.remove());
      mids.forEach((m) => m.remove());
      corners = [];
      mids = [];
      if (map) { try { map.remove(); } catch (_e) {} map = null; }
    }

    return { el, destroy };
  }

  return { card, distinctCorners, problemOf, formatArea };
})();
