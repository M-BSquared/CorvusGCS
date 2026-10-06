"use strict";

/**
 * The page stylesheets, as index.html loads them: every css/*.css after
 * components.css, in order. main.css used to be the only one; it was cut into
 * several at its section boundaries, and a test that looks for a page rule
 * reads them all, the way the browser does.
 *
 * Lives under tests/support/ so tools/frontend_tests.js, which runs every
 * .js directly in tests/, does not run it as a test of its own.
 */

const fs = require("node:fs");
const path = require("node:path");

const SRC = path.join(__dirname, "..", "..", "src");

/** Every css/ sheet *html* links, in order. */
function sheetsIn(html) {
  return [...html.matchAll(/href="css\/([\w.-]+\.css)"/g)].map((m) => m[1]);
}

/** The page sheets of index.html: what follows components.css. */
function pageSheets() {
  const all = sheetsIn(fs.readFileSync(path.join(SRC, "index.html"), "utf8"));
  return all.slice(all.indexOf("components.css") + 1);
}

/** The page sheets' text, concatenated in load order. */
function pageCss() {
  return pageSheets()
    .map((name) => fs.readFileSync(path.join(SRC, "css", name), "utf8"))
    .join("\n");
}

module.exports = { sheetsIn, pageSheets, pageCss, SRC };
