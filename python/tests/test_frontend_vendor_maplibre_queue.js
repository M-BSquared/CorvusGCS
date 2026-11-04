"use strict";

/**
 * The image request queue bug Corvus once patched in the vendored MapLibre,
 * and the upstream fix that retired the patch.
 *
 * MapLibre 5.24's raster source built its tile request like this:
 *
 *   tile.abortController = new AbortController;
 *   getImage(yield transformRequest(url), tile.abortController, ...)
 *
 * The arguments are evaluated in order, so `tile.abortController` was read
 * AFTER the request transform resolved. A tile aborted in between (a pan, a
 * zoom, 3D tilting the camera) had had its controller deleted by abortTile,
 * and the request was queued with none. The queue then read
 * `item.abortController.signal` and threw, inside whichever getImage call ran
 * the queue next: an unrelated tile, often an elevation tile, was marked
 * errored and never asked for again. Imagery with holes in it, terrain with
 * square pits, and "Cannot read properties of undefined (reading 'signal')".
 *
 * Corvus carried CORVUS-QUEUE-PATCH against that until MapLibre fixed it
 * upstream (6.1, maplibre-gl-js#8004): the raster and raster-dem sources now
 * hand the controller to transformAndGetImage, which holds it as a parameter
 * across the await and checks it for an abort before queueing. The queue loop
 * itself still reads the controller unguarded, so the fix lives entirely in
 * the call sites. This file checks they still look that way, so an upgrade
 * that brings the late read back fails here instead of in the field.
 *
 * Run:
 *   node tests/test_frontend_vendor_maplibre_queue.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const FILE = path.join(__dirname, "..", "src", "vendor", "maplibre-gl.mjs");
const src = fs.readFileSync(FILE, "utf8");

const HINT = `
If this fails after an upgrade, look at how the raster and raster-dem
sources' loadTile build their image request. When the tile's controller is
read only after an await (as an argument to getImage evaluated after
transformRequest resolves), the old crash is back: guard the image queue's
processing loop so a queued request without a controller is skipped like an
aborted one, mark the edit CORVUS-QUEUE-PATCH, and test it here.
`;

let passed = 0;
function check(name, fn) { fn(); passed++; console.log("  ok  " + name); }

console.log("vendor/maplibre-gl.mjs — image queue");

check("no CORVUS-QUEUE-PATCH is left behind in the upstream build", () => {
  assert.ok(!src.includes("CORVUS-QUEUE-PATCH"),
    "the vendored MapLibre carries a queue patch this test no longer checks.");
});

check("both tile sources hand their controller over before the await", () => {
  const sites = src.match(
    /\.abortController=new AbortController;try\{let \w+=await \w+\.transformAndGetImage\(this\.map\._requestManager,\w+,`Tile`,\w+\.abortController,/g,
  ) || [];
  assert.equal(sites.length, 2,
    "expected the raster and the raster-dem source to pass the tile's controller " +
    "straight into transformAndGetImage." + HINT);
});

check("transformAndGetImage keeps the controller it was given across the await", () => {
  const fn = /async function (\w+)\((\w+),(\w+),(\w+),(\w+),[^)]*\)\{let (\w+)=await \2\.transformRequest\(\3,\4\);return \w+\(\5\.signal\),\w+\.getImage\(\6,\5,/.exec(src);
  assert.ok(fn,
    "transformAndGetImage no longer reads: await transformRequest, check the " +
    "controller it was handed, then queue with that same controller." + HINT);
  assert.ok(src.includes(`.transformAndGetImage=${fn[1]},`),
    "the function checked above is not the exported transformAndGetImage." + HINT);
});

check("no source reads a tile's controller after awaiting the transform", () => {
  assert.ok(!/getImage\((?:yield|await) [^,]*transformRequest\([^)]*\),\w+\.abortController/.test(src),
    "a getImage call reads the tile's controller after the transform resolved." + HINT);
});

console.log(`\n${passed} checks passed`);
