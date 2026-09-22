"use strict";

/**
 * Corvus.lazy — the vendor bundles that are not needed to open the app.
 *
 * index.html used to load plotly-basic (1.0 MB) and xterm (~300 KB) as
 * blocking script tags. The app opens on the map; neither is used there. The
 * behaviour that matters is not "it eventually loads" but the three ways an
 * on-demand load goes wrong: loading twice, staying broken after one failed
 * fetch, and resolving into a screen the operator has already left. The
 * third is checked where it lives (tests/test_frontend_term_window.js); the
 * first two are here, along with the guarantee that the synchronous
 * "is it here?" question never triggers a download.
 *
 * Run:
 *   node tests/test_frontend_lazy.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

global.window = global;
global.Corvus = {};

// A document stub that records injected <script>s and lets a test decide
// whether each one "loads" or fails.
const injected = [];
const head = {
  appendChild(el) { injected.push(el); return el; },
};
global.document = {
  head,
  createElement() {
    const el = { src: "", async: true, _listeners: {} };
    el.addEventListener = (t, cb) => { (el._listeners[t] = el._listeners[t] || []).push(cb); };
    el.remove = () => { const i = injected.indexOf(el); if (i >= 0) injected.splice(i, 1); };
    return el;
  },
};

function fire(el, type) { (el._listeners[type] || []).slice().forEach((cb) => cb()); }
function flush() { return new Promise((r) => setTimeout(r, 0)); }

require("../src/js/lazy.js");
const lazy = Corvus.lazy;

function reset() {
  injected.length = 0;
  delete window.Plotly;
  delete window.Terminal;
  delete window.FitAddon;
}

// ---------------------------------------------------------------------------

async function testTwoCallersShareOneFetch() {
  reset();
  const first = lazy.script("vendor/a.js");
  const second = lazy.script("vendor/a.js");
  assert.equal(injected.length, 1, "three charts drawing in one frame is one fetch");
  fire(injected[0], "load");
  await first;
  await second;
  // And a later caller gets the settled promise without touching the DOM.
  await lazy.script("vendor/a.js");
  assert.equal(injected.length, 1);
}

async function testAFailedLoadIsRetryable() {
  reset();
  const first = lazy.script("vendor/b.js");
  fire(injected[0], "error");
  await assert.rejects(first, /could not load/);
  // Corvus runs offline by design. A fetch that failed because the page was
  // mid-reload must not leave the feature dead until the app restarts.
  const retry = lazy.script("vendor/b.js");
  assert.equal(injected.length, 1, "the failed script element was cleaned up and retried");
  fire(injected[0], "load");
  await retry;
}

async function testScriptsExecuteInTheOrderTheyWereAskedFor() {
  reset();
  lazy.script("vendor/c.js");
  // async=false is what keeps an addon executing after the bundle it extends
  // — xterm-addon-fit is useless before xterm.
  assert.equal(injected[0].async, false);
}

async function testAskingWhetherItIsHereNeverDownloads() {
  reset();
  assert.equal(lazy.plotlyReady(), false);
  assert.equal(lazy.terminalReady(), false);
  assert.equal(injected.length, 0,
    "a teardown asking 'is there anything to purge?' must not fetch 1 MB");
}

async function testPlotlyAlreadyPresentIsNotFetchedAgain() {
  reset();
  window.Plotly = { react() {} };
  assert.equal(lazy.plotlyReady(), true);
  const value = await lazy.plotly();
  assert.equal(value, window.Plotly);
  assert.equal(injected.length, 0);
}

async function testPlotlyIsFetchedOnFirstUse() {
  reset();
  const pending = lazy.plotly();
  assert.equal(injected.length, 1);
  assert.match(injected[0].src, /plotly-basic\.min\.js$/);
  window.Plotly = { react() {} };
  fire(injected[0], "load");
  assert.equal(await pending, window.Plotly);
}

async function testTheTerminalLoadsItsAddonAfterTheBundle() {
  reset();
  const pending = lazy.terminal();
  assert.equal(injected.length, 1, "the addon is not requested before the bundle");
  assert.match(injected[0].src, /xterm\.js$/);
  window.Terminal = function () {};
  fire(injected[0], "load");
  await flush();
  assert.equal(injected.length, 2, "the addon follows once the bundle is in");
  assert.match(injected[1].src, /xterm-addon-fit\.js$/);
  window.FitAddon = { FitAddon: function () {} };
  fire(injected[1], "load");
  assert.equal(await pending, window.Terminal);
  assert.equal(lazy.terminalReady(), true);
}

async function testTheHeavyBundlesAreNotInTheEagerScriptList() {
  const html = fs.readFileSync(
    path.join(__dirname, "..", "src", "index.html"), "utf8");
  // `defer` is on every one of them (see the comment above the tags): the
  // attribute is what makes thirty-six eager tags cheap, so it is matched
  // here rather than excluded.
  const eager = html.match(/<script src="[^"]+"(?: [a-z]+)*><\/script>/g) || [];
  const joined = eager.join("\n");
  assert.ok(eager.length, "no script tags found — did the pattern stop matching?");
  for (const tag of eager) {
    assert.match(tag, / defer>/,
      `${tag} is missing defer — it would block the parser mid-document`);
  }
  assert.doesNotMatch(joined, /plotly/,
    "plotly is 1.0 MB and belongs to three pages the app does not open on");
  assert.doesNotMatch(joined, /xterm/,
    "xterm is ~300 KB and belongs to the SSH terminal");
  // maplibre IS the first paint and lucide swaps the icons on every screen,
  // including the first, so both stay eager.
  assert.match(joined, /maplibre-gl\.min\.js/);
  assert.match(joined, /lucide\.min\.js/);
  // And the loader itself has to be defined before anything that asks for it.
  const lazyAt = joined.indexOf("js/lazy.js");
  assert.ok(lazyAt >= 0, "js/lazy.js is not loaded");
  assert.ok(lazyAt < joined.indexOf("js/analysis.js"), "lazy.js must come first");
}

const tests = [
  testTwoCallersShareOneFetch,
  testAFailedLoadIsRetryable,
  testScriptsExecuteInTheOrderTheyWereAskedFor,
  testAskingWhetherItIsHereNeverDownloads,
  testPlotlyAlreadyPresentIsNotFetchedAgain,
  testPlotlyIsFetchedOnFirstUse,
  testTheTerminalLoadsItsAddonAfterTheBundle,
  testTheHeavyBundlesAreNotInTheEagerScriptList,
];

(async () => {
  let failed = 0;
  for (const t of tests) {
    try { await t(); console.log(`ok   - ${t.name}`); }
    catch (err) { failed++; console.error(`FAIL - ${t.name}: ${err && err.message}`); }
  }
  if (failed) { console.error(`\n${failed}/${tests.length} lazy-loader test(s) FAILED`); process.exit(1); }
  console.log(`\nAll ${tests.length} lazy-loader tests passed.`);
})();
