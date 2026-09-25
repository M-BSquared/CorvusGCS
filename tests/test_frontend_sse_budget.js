"use strict";

/**
 * How many SSE connections this frontend can hold open at once.
 *
 * A browser caps concurrent HTTP/1.1 requests per origin at six. Corvus used
 * to be able to hold five server-sent-event streams open simultaneously —
 * telemetry, the MAVLink console, tile-download progress, firmware-flash
 * progress and an SSH shell — which left ONE connection for every map tile,
 * every fetch and every plugin asset. A viewport is dozens of tiles; they
 * queue behind each other one at a time, and the map crawls at exactly the
 * moment (pre-flight setup with a region downloading) the operator is busiest.
 * It degraded in the least diagnosable way there is: nothing errored,
 * everything was just slow.
 *
 * Four of those are now one. `js/events.js` opens a single `/api/events`
 * stream carrying the console, parameter, firmware and tile-progress topics,
 * each under its own event name. What is left is:
 *
 *   1. /api/telemetry   — the 50 Hz path, deliberately its own connection
 *   2. /api/events      — console + params + firmware + tiles
 *   3. /api/ssh/stream  — a terminal's output, only where its WebSocket
 *                         (/api/ssh/ws) could not open
 *
 * A terminal is a WebSocket now, which is not in the six: with one SSE stream
 * per terminal, and the pool shared by every window of the app, four
 * terminals left nothing for anything else. The stream stays as the fallback,
 * so it is still counted here. This file is what stops the budget drifting
 * back: a fourth endpoint, or a second construction site for an existing one,
 * fails here and has to be argued for.
 *
 * Run:
 *   node tests/test_frontend_sse_budget.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const JS_DIR = path.join(__dirname, "..", "src", "js");

/** Every `new EventSource(...)` in the frontend, as {file, url}. */
function constructionSites() {
  const sites = [];
  for (const name of fs.readdirSync(JS_DIR).filter((f) => f.endsWith(".js"))) {
    const source = fs.readFileSync(path.join(JS_DIR, name), "utf8");
    // The url may be a literal or built from one (events.js appends its topic
    // list), so the leading quoted path is what identifies the endpoint.
    const pattern = /new EventSource\(\s*(?:`|")(\/[^`"?]*)/g;
    let match;
    while ((match = pattern.exec(source)) !== null) {
      sites.push({ file: name, url: match[1] });
    }
  }
  return sites;
}

// The endpoints this frontend is allowed to open, and the one module that
// owns each. Adding a row means adding a connection to a budget of six.
const ALLOWED = {
  "/api/telemetry": "telemetry.js",
  "/api/events": "events.js",
  "/api/ssh/stream": "ssh-term.js",
};

// Topics that must travel on /api/events rather than as connections of their
// own. Each has a single-topic endpoint on the server still — they are a
// published API — but nothing in src/js may open one.
const MULTIPLEXED = [
  "/api/console/stream",
  "/api/params/progress",
  "/api/firmware/progress",
  "/api/tiles/progress",
];

function testNoUnexpectedStreamEndpoint() {
  const sites = constructionSites();
  assert.ok(sites.length, "no EventSource found — did the pattern stop matching?");
  for (const site of sites) {
    assert.ok(
      Object.prototype.hasOwnProperty.call(ALLOWED, site.url),
      `${site.file} opens ${site.url}, which is not in the SSE budget. `
      + "A browser allows six connections per origin in total; every stream "
      + "added here is one fewer for map tiles.",
    );
    assert.equal(site.file, ALLOWED[site.url],
      `${site.url} is opened by ${site.file}, but ${ALLOWED[site.url]} owns it`);
  }
}

function testEachEndpointHasExactlyOneConstructionSite() {
  // One site per endpoint is what makes reference-counting possible at all:
  // two `new EventSource` on the same url are two connections the moment both
  // paths are live, and nothing in the language stops that happening.
  const byUrl = new Map();
  for (const site of constructionSites()) {
    byUrl.set(site.url, (byUrl.get(site.url) || 0) + 1);
  }
  for (const [url, count] of byUrl) {
    assert.equal(count, 1,
      `${url} is constructed ${count} times; share one stream instead `
      + "(events.js is the worked example)");
  }
}

function testMultiplexedTopicsAreNotOpenedDirectly() {
  const opened = new Set(constructionSites().map((s) => s.url));
  for (const url of MULTIPLEXED) {
    assert.ok(!opened.has(url),
      `${url} is opened as its own EventSource. It is a topic on `
      + "/api/events — subscribe through Corvus.events instead, or the "
      + "connection budget this file exists to protect goes back to five.");
  }
}

function testTheBudgetIsStillWhatThisFileClaims() {
  // The number in the docstring above is load-bearing: it is the argument for
  // multiplexing. If it drifts, the argument drifts with it.
  assert.equal(Object.keys(ALLOWED).length, 3,
    "the SSE endpoint count changed; update this file's reasoning with it");
}

function testEveryMultiplexedTopicHasAConsumer() {
  // The other direction: a topic the server offers and nothing subscribes to
  // is dead weight on the stream, and a topic a page subscribes to under a
  // name the server does not know never fires. Both are silent.
  const events = fs.readFileSync(path.join(JS_DIR, "events.js"), "utf8");
  const subscribed = new Set();
  for (const name of fs.readdirSync(JS_DIR).filter((f) => f.endsWith(".js"))) {
    const source = fs.readFileSync(path.join(JS_DIR, name), "utf8");
    const pattern = /Corvus\.events\.subscribe\(\s*"([a-z]+)"/g;
    let match;
    while ((match = pattern.exec(source)) !== null) subscribed.add(match[1]);
  }
  for (const topic of ["console", "params", "firmware", "tiles"]) {
    assert.ok(subscribed.has(topic), `nothing subscribes to the "${topic}" topic`);
  }
  // "error" is events.js's own signal for a dropped stream, not a server topic.
  assert.match(events, /"error" is local to this module/,
    "events.js must keep the local-only 'error' topic out of the URL");
}

const tests = [
  testNoUnexpectedStreamEndpoint,
  testEachEndpointHasExactlyOneConstructionSite,
  testMultiplexedTopicsAreNotOpenedDirectly,
  testTheBudgetIsStillWhatThisFileClaims,
  testEveryMultiplexedTopicHasAConsumer,
];

let failed = 0;
for (const t of tests) {
  try { t(); console.log(`ok   - ${t.name}`); }
  catch (err) { failed++; console.error(`FAIL - ${t.name}: ${err && err.message}`); }
}
if (failed) {
  console.error(`\n${failed}/${tests.length} SSE budget test(s) FAILED`);
  process.exit(1);
}
console.log(`\nAll ${tests.length} SSE budget tests passed.`);
