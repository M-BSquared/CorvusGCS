"use strict";

/**
 * The map service API keys dialog (Settings -> Map service -> API KEYS).
 *
 * A source-contract test, like tests/test_frontend_css_boundary.js and
 * tests/test_frontend_vendor_zoom.js: the dialog is built inside sidenav.js's
 * settings render and is not reachable from Node without standing up the whole
 * page, but what has to hold about it is a property of the source, not of a
 * particular render.
 *
 * What is pinned here is the credential handling, because every one of these
 * is invisible at runtime — the dialog looks and behaves identically whether
 * or not it is leaking the key:
 *
 *   1. the key goes to POST /api/tiles/token and nowhere else — never into a
 *      URL, never into localStorage, never onto an element's text;
 *   2. an empty field is "leave this one alone", never "clear it", so a key
 *      cannot be destroyed by tabbing past the box it lives in;
 *   3. the field is a password field with autocomplete off, so the browser
 *      does not offer to remember a credential Corvus deliberately never
 *      shows again;
 *   4. the sign-up address is printed, not linked — an <a href> goes nowhere
 *      in the QtWebEngine desktop build.
 *
 * Run:
 *   node tests/test_frontend_map_tokens.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const SRC = path.join(__dirname, "..", "src");
const sidenav = fs.readFileSync(path.join(SRC, "js", "sidenav.js"), "utf8");
const mainCss = fs.readFileSync(path.join(SRC, "css", "main.css"), "utf8");

// The dialog's own source, bounded at both ends, so a match elsewhere in this
// 1 800-line module cannot stand in for one inside it.
const start = sidenav.indexOf("function openMapTokenDialog(");
assert.ok(start > 0, "openMapTokenDialog must exist in sidenav.js");
const end = sidenav.indexOf("// --- Section B: SSH Connections", start);
assert.ok(end > start, "the marker that bounds the dialog moved; fix this test");
const dialog = sidenav.slice(start, end);

// ---------------------------------------------------------------------------
// 1. One door out, and it is a POST body
// ---------------------------------------------------------------------------
assert.ok(
  /fetch\("\/api\/tiles\/token"/.test(dialog),
  "the key is sent to POST /api/tiles/token");

// A key in a query string lands in the server log, the browser history and
// any referrer the page sends; it must only ever travel in the body.
assert.ok(
  !/\/api\/tiles\/token\?/.test(dialog),
  "the key must never be put in a query string");

// The frontend must not keep its own copy. The whole point of proxying tiles
// through the backend is that the browser never holds the credential.
assert.ok(
  !/localStorage/.test(dialog),
  "the dialog must not cache a key in browser storage");

// ---------------------------------------------------------------------------
// 2. An empty field never clears a saved key
// ---------------------------------------------------------------------------
assert.ok(
  /rows\.filter\(\(row\) => row\.value\(\)\)/.test(dialog),
  "SAVE must skip the rows the operator did not type into");
assert.ok(
  /postToken\(prov\.id, ""\)/.test(dialog),
  "REMOVE is the one path that sends an empty token");

// ---------------------------------------------------------------------------
// 3. The field is a credential field
// ---------------------------------------------------------------------------
const fieldBlock = dialog.slice(dialog.indexOf("Corvus.ui.input({"));
assert.ok(/type: "password"/.test(fieldBlock), "the key field is a password field");
assert.ok(/autocomplete: false/.test(fieldBlock),
  "the browser must not offer to remember a key Corvus never shows again");

// The saved key is never rendered back: the placeholder says one exists, the
// value stays empty.
assert.ok(
  /placeholder: prov\.token_set \?/.test(fieldBlock),
  "a saved key is reported by the placeholder, never by the value");
assert.ok(
  !/value:\s*prov\.token/.test(dialog),
  "a stored key must never be written into the field");

// ---------------------------------------------------------------------------
// 4. The sign-up address is text
// ---------------------------------------------------------------------------
assert.ok(!/<a\s/i.test(dialog), "no anchor: an external link goes nowhere in the .app");
assert.ok(/meta\.signup/.test(dialog), "the operator is told where to get a key");

// ---------------------------------------------------------------------------
// 5. A keyed service with no key says so, and is still offered
// ---------------------------------------------------------------------------
assert.ok(
  /if \(prov\.token_required && !prov\.token_set\) return "Needs an API key";/
    .test(sidenav),
  "a service that cannot draw says why on its own card");
assert.ok(
  !/providers\s*=\s*providers\.filter\(\(p\) => !p\.token_required/.test(sidenav),
  "a keyed service is reported, never hidden from the picker");

// The button only appears when this build actually has a keyed service, so it
// can never open an empty dialog.
assert.ok(
  /const keyed = providers\.filter\(\(p\) => p\.token_required\);\s*\n\s*if \(keyed\.length\)/
    .test(sidenav),
  "the API KEYS button is conditional on there being a keyed service");

// ---------------------------------------------------------------------------
// 6. Every class the dialog writes is styled
// ---------------------------------------------------------------------------
const written = new Set();
for (const m of dialog.matchAll(/className = "(settings-map-[a-z-]+)"/g)) {
  written.add(m[1]);
}
["settings-map-keys", "settings-map-keys-note"].forEach((c) => written.add(c));
for (const cls of written) {
  assert.ok(mainCss.includes("." + cls),
    `.${cls} is written by sidenav.js but styled nowhere`);
}

// Every CSS custom property the new rules use must actually be defined, or the
// rule silently does nothing.
const themes = fs.readFileSync(path.join(SRC, "css", "themes.css"), "utf8");
const block = mainCss.slice(mainCss.indexOf(".settings-map-keys {"));
const rules = block.slice(0, block.indexOf("\n\n/*", 1));
for (const m of rules.matchAll(/var\((--[a-z0-9-]+)\)/g)) {
  assert.ok(themes.includes(m[1] + ":") || mainCss.includes(m[1] + ":"),
    `${m[1]} is used by the map-keys rules but defined nowhere`);
}

console.log("map service API keys: all assertions passed");
