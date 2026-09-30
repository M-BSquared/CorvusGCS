"use strict";

/**
 * The patch this repository carries against the vendored MapLibre's image
 * request queue, and the reason it has to stay.
 *
 * MapLibre 5.24's raster source builds its tile request like this:
 *
 *   tile.abortController = new AbortController;
 *   getImage(yield transformRequest(url), tile.abortController, ...)
 *
 * The arguments are evaluated in order, so `tile.abortController` is read
 * AFTER the request transform resolves. A tile aborted in between (a pan, a
 * zoom, 3D tilting the camera) has had its controller deleted by abortTile,
 * and the request is queued with none. The queue then reads
 * `item.abortController.signal` and throws. That throw lands inside whichever
 * getImage call happened to run the queue next, which rejects THAT call: an
 * unrelated tile, often an elevation tile, is marked errored and never asked
 * for again. On screen that is imagery with holes in it and terrain with
 * square pits where a DEM tile silently failed, plus a console full of
 * "Cannot read properties of undefined (reading 'signal')".
 *
 * The patch treats a queued request without a controller as what it is, an
 * aborted one, which the queue already knows how to skip.
 *
 * A vendored file gets replaced wholesale on an upgrade and this patch goes
 * with it. If this fails after an upgrade, first check whether the new
 * MapLibre still reads the controller after the await; if it does, re-apply
 * the edit printed below rather than deleting the test.
 *
 * Run:
 *   node tests/test_frontend_vendor_maplibre_queue.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const FILE = path.join(__dirname, "..", "src", "vendor", "maplibre-gl.min.js");
const src = fs.readFileSync(FILE, "utf8");

const OPEN = "/*CORVUS-QUEUE-PATCH*/";
const CLOSE = "/*CORVUS-QUEUE-PATCH-END*/";

const FIX = `
Re-apply it like this. In src/vendor/maplibre-gl.min.js find the image
request queue's processing loop, which reads:

  for(let t=o;t<e&&i.length>0;t++){const e=i.shift();e.abortController.signal.aborted?t--:s(e);}

and replace the condition so it reads:

  ${OPEN}(!e.abortController||e.abortController.signal.aborted)${CLOSE}?t--:s(e);
`;

let passed = 0;
function check(name, fn) { fn(); passed++; console.log("  ok  " + name); }

console.log("vendor/maplibre-gl.min.js — image queue patch");

check("the patch is in place", () => {
  const a = src.indexOf(OPEN);
  const b = src.indexOf(CLOSE);
  assert.ok(a >= 0 && b > a, "CORVUS-QUEUE-PATCH is missing." + FIX);
  const body = src.slice(a + OPEN.length, b);
  assert.equal(body, "(!e.abortController||e.abortController.signal.aborted)",
    "CORVUS-QUEUE-PATCH was changed." + FIX);
});

check("the unguarded read is gone", () => {
  assert.ok(!src.includes("const e=i.shift();e.abortController.signal.aborted"),
    "the queue still reads the controller unguarded." + FIX);
});

check("the patched loop still skips aborted requests and runs the rest", () => {
  // The loop body, lifted out and run against a queue holding one request of
  // each kind: the controller-less one must be skipped like an aborted one,
  // not thrown on, and the live one after it must still start.
  const a = src.indexOf(OPEN);
  const loopStart = src.lastIndexOf("for(let t=o;", a);
  const loopEnd = src.indexOf("}", src.indexOf(CLOSE)) + 1;
  const loop = src.slice(loopStart, loopEnd);
  const started = [];
  const run = new Function("o", "e", "i", "s", loop);
  run(0, 4, [
    { name: "aborted mid-transform" },
    { name: "aborted", abortController: { signal: { aborted: true } } },
    { name: "live", abortController: { signal: { aborted: false } } },
  ], (item) => started.push(item.name));
  assert.deepEqual(started, ["live"]);
});

console.log(`\n${passed} checks passed`);
