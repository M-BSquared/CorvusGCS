"use strict";

/**
 * The Credits dialog names everything Corvus ships or draws from, and the
 * partnership in the agreed wording. Elevation models live in their own
 * registry (`terrain` in GET /api/tiles/sources), and the dialog once read
 * only the base layers, so every elevation credit, Copernicus's required
 * notice among them, was missing.
 *
 * Run:
 *   node tests/test_frontend_credits.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};

function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", textContent: "", children: [], src: "", alt: "",
    parentNode: null,
  };
  e.appendChild = (c) => { c.parentNode = e; e.children.push(c); return c; };
  e.removeChild = (c) => {
    const i = e.children.indexOf(c);
    if (i === -1) throw new Error("removeChild: not a child");
    e.children.splice(i, 1);
    c.parentNode = null;
    return c;
  };
  return e;
}

global.document = {
  createElement: makeEl,
  createDocumentFragment: () => makeEl("#fragment"),
};

function allText(el) {
  return [el.textContent, el.alt].concat(el.children.map(allText)).join("\n");
}

let lastBody = null;
let tileResponse = null;
Corvus.ui = {
  label: (t) => { const e = makeEl("span"); e.textContent = t; return e; },
  empty: (t) => { const e = makeEl("div"); e.textContent = t; return e; },
  button: () => makeEl("button"),
  modal: (o) => { lastBody = o.body; return { open() {}, close() {} }; },
};
Corvus.telemetry = {
  requestJson: (url) => {
    if (url === "/api/version") return Promise.resolve({ version: "2000.01.01" });
    if (url === "/api/tiles/sources") {
      return tileResponse ? Promise.resolve(tileResponse) : Promise.reject(new Error("offline"));
    }
    return Promise.reject(new Error(`unexpected ${url}`));
  },
};

require("./../src/js/credits.js");

const flush = () => new Promise((r) => setTimeout(r, 0));

const COPERNICUS = "Elevation: © Mapterhorn (mapterhorn.com/attribution), Copernicus GLO-30 © DLR";
const SRTM = "Elevation: AWS Terrain Tiles: SRTM, USGS NED, and national datasets";

async function openWith(response) {
  tileResponse = response;
  lastBody = null;
  Corvus.credits.open();
  await flush();
  await flush();
  await flush();
  assert.ok(lastBody, "the dialog was not opened");
  return allText(lastBody);
}

(async () => {
  // --- elevation credits come from the terrain registry ------------------
  const text = await openWith({
    sources: [
      { id: "satellite", provider: "esri", attribution: "© Esri, Maxar, Earthstar Geographics" },
      { id: "hybrid", provider: "esri", attribution: "© Esri, Maxar, Earthstar Geographics" },
      { id: "osm", provider: "osm", attribution: "© OpenStreetMap contributors" },
    ],
    providers: [{ id: "esri", label: "Esri" }, { id: "osm", label: "OpenStreetMap" }],
    terrain: [
      { id: "terrain", label: "SRTM (AWS Terrain Tiles)", attribution: SRTM },
      { id: "copernicus", label: "Copernicus GLO-30", attribution: COPERNICUS },
    ],
  });
  assert.ok(text.includes("Elevation data"), "no elevation group");
  assert.ok(text.includes(COPERNICUS), "the Copernicus notice is missing");
  assert.ok(text.includes(SRTM), "the SRTM credit is missing");
  assert.ok(text.includes("Copernicus GLO-30"), "the elevation model is not named");
  assert.equal(text.split("© Esri, Maxar, Earthstar Geographics").length - 1, 1,
    "a shared map attribution is listed twice");
  assert.ok(!text.includes("Loading"), "a placeholder was left behind");
  assert.ok(text.includes("Corvus GCS 2000.01.01"), "version not taken from /api/version");

  // --- the partnership, in the agreed wording ---------------------------
  assert.ok(text.includes("with Universität der Bundeswehr München"));
  assert.ok(!/\bat (the )?Universität/.test(text), "the institution is credited with \"at\"");
  assert.ok(text.includes("Chair LRT 1.1 of Prof. Dr. Matthias Gerdts"), "the chair is not credited");

  // --- what the static lists must carry ----------------------------------
  for (const name of ["FFmpeg", "AppImage runtime", "GitHub", "PX4 GPS drivers", "pyulog"]) {
    assert.ok(text.includes(name), `${name} is not credited`);
  }

  // --- offline: both groups say so instead of inventing a credit ---------
  const offline = await openWith(null);
  assert.ok(offline.includes("Map attributions unavailable"));
  assert.ok(offline.includes("Elevation attributions unavailable"));

  // --- no dash used as punctuation in what the operator reads ------------
  const lists = [].concat(
    Corvus.credits.LIBRARIES, Corvus.credits.FONTS, Corvus.credits.RUNTIME,
    Corvus.credits.SERVICES, Corvus.credits.REFERENCES,
  );
  for (const e of lists) {
    for (const v of [e.name, e.license, e.copyright, e.role]) {
      if (!v) continue;
      assert.ok(!/[–—]| - /.test(v), `dash in credits text: ${v}`);
    }
  }
  for (const v of Object.values(Corvus.credits.PROJECT)) {
    assert.ok(!/[–—]| - /.test(v), `dash in credits text: ${v}`);
  }

  console.log("test_frontend_credits.js: all assertions passed");
})().catch((err) => {
  console.error(err);
  process.exit(1);
});
