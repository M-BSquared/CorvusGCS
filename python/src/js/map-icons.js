"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.mapIcons: the named symbols a plugin can put on the Home map with
  api.map.drawMarker("key", point, {icon: "tree"}).

  A symbol is a few SVG path strings on a 24 by 24 grid, drawn as strokes in
  the marker's colour. Path data only, so a name can never carry markup into
  the page. Loaded before js/map-overlays.js, which resolves the names; a
  plugin may still hand drawMarker its own path strings instead of a name.

  Adding a symbol: add a line to ICONS. The name is a lowercase word, the label
  is what an operator reads. Nothing else needs to change.
*/
Corvus.mapIcons = (function () {
  const NAME_RE = /^[a-z][a-z0-9-]{0,31}$/;

  const ICONS = [
    { name: "gate", label: "Gate",
      paths: ["M5 21V4", "M19 21V4", "M5 4h14", "M9 15h6"] },
    { name: "tree", label: "Tree",
      paths: ["M12 22v-7", "M12 3c-3 0-5.5 2.2-5.5 5 0 .6.1 1.1.3 1.6A4 4 0 0 0 7 17h10a4 4 0 0 0 .2-7.4c.2-.5.3-1 .3-1.6 0-2.8-2.5-5-5.5-5z"] },
    { name: "radar", label: "Radar",
      paths: ["M12 12l6-6", "M12 11a1 1 0 1 0 0 2 1 1 0 0 0 0-2z", "M7.8 16.2a6 6 0 1 1 8.4 0", "M4.9 19.1a10 10 0 1 1 14.2 0"] },
    { name: "camera", label: "Camera",
      paths: ["M4 8h3l2-3h6l2 3h3v11H4z", "M12 11a3 3 0 1 0 0 6 3 3 0 0 0 0-6z"] },
    { name: "antenna", label: "Antenna",
      paths: ["M12 12v9", "M8 21h8", "M12 8a1 1 0 1 0 0 2 1 1 0 0 0 0-2z", "M7.8 7.8a6 6 0 0 0 0 8.4", "M16.2 7.8a6 6 0 0 1 0 8.4"] },
    { name: "person", label: "Person",
      paths: ["M12 4a3 3 0 1 0 0 6 3 3 0 0 0 0-6z", "M5 21v-1a5 5 0 0 1 5-5h4a5 5 0 0 1 5 5v1"] },
    { name: "vehicle", label: "Vehicle",
      paths: ["M3 15v-4l2-5h14l2 5v4z", "M6 15v3", "M18 15v3", "M3 11h18"] },
    { name: "building", label: "Building",
      paths: ["M5 21V4h9v17", "M14 9h5v12", "M3 21h18", "M8 8h3", "M8 12h3", "M8 16h3"] },
    { name: "explosion", label: "Explosion",
      paths: ["M12 2l2 6 6-2-3 5.5 5 3.5-6 .5 1 6-5-4-5 4 1-6-6-.5 5-3.5L4 6l6 2z"] },
    { name: "fire", label: "Fire",
      paths: ["M8.5 14.5A2.5 2.5 0 0 0 11 12c0-1.38-.5-2-1-3-1.07-2.14-.22-4.05 2-6 .5 2.5 2 4.9 4 6.5 2 1.6 3 3.5 3 5.5a7 7 0 1 1-14 0c0-1.15.43-2.29 1-3a2.5 2.5 0 0 0 2.5 2.5z"] },
    { name: "hazard", label: "Hazard",
      paths: ["M12 3L2 20h20z", "M12 10v4", "M12 17v.5"] },
    { name: "target", label: "Target",
      paths: ["M12 4a8 8 0 1 0 0 16 8 8 0 0 0 0-16z", "M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z", "M12 1v4", "M12 19v4", "M1 12h4", "M19 12h4"] },
    { name: "pad", label: "Landing pad",
      paths: ["M4 4h16v16H4z", "M9 8v8", "M15 8v8", "M9 12h6"] },
    { name: "flag", label: "Flag",
      paths: ["M5 21V4", "M5 4h12l-2 4 2 4H5"] },
    { name: "pin", label: "Pin",
      paths: ["M12 21s-7-6.2-7-11a7 7 0 0 1 14 0c0 4.8-7 11-7 11z", "M12 7.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z"] },
    { name: "home", label: "Home",
      paths: ["M3 11l9-8 9 8", "M5 10v11h14V10", "M10 21v-6h4v6"] },
    { name: "drone", label: "Drone",
      paths: ["M12 10a2 2 0 1 0 0 4 2 2 0 0 0 0-4z", "M10.5 10.5L6 6", "M13.5 10.5L18 6", "M10.5 13.5L6 18", "M13.5 13.5L18 18", "M4 4.5h4", "M16 4.5h4", "M4 19.5h4", "M16 19.5h4"] },
    { name: "boat", label: "Boat",
      paths: ["M2 17h20l-3 4H5z", "M12 3v12", "M12 4l6 9h-6"] },
    { name: "medical", label: "Medical",
      paths: ["M10 3h4v7h7v4h-7v7h-4v-7H3v-4h7z"] },
    { name: "power", label: "Power",
      paths: ["M13 2L4 14h7l-1 8 9-12h-7z"] },
    { name: "water", label: "Water",
      paths: ["M12 3s6 6.5 6 11a6 6 0 0 1-12 0c0-4.5 6-11 6-11z"] },
  ];

  const BY_NAME = new Map();
  ICONS.forEach((icon) => {
    if (NAME_RE.test(icon.name)) BY_NAME.set(icon.name, icon);
  });

  /** The path strings of a named symbol (a copy), or null for an unknown name. */
  function get(name) {
    const icon = typeof name === "string" ? BY_NAME.get(name) : null;
    return icon ? icon.paths.slice() : null;
  }

  /** Every symbol as {name, label}, in the order they are defined. */
  function list() {
    return ICONS.filter((i) => BY_NAME.has(i.name)).map((i) => ({ name: i.name, label: i.label }));
  }

  return { get, has: (name) => typeof name === "string" && BY_NAME.has(name), list, NAME_RE };
})();
