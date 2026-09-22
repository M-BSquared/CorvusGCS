"use strict";

/**
 * The library/app boundary in CSS.
 *
 * src/js/ui.js is the one place that builds a control, and css/components.css
 * is meant to be the one place that styles it. That had drifted: 22 of the
 * classes ui.js writes (.page-card, .nav-item, .icon-btn, the whole .ui-toast
 * stack, .progress-bar) were defined only in main.css, and 15 more were split
 * across both files. Editing components.css to restyle a component then did
 * nothing, because the real rule was elsewhere.
 *
 * Worse, components.css loaded LAST, so a page override in main.css could only
 * win on specificity — and a tie went to the component. Three overrides tied
 * and lost silently: `.params-actions .btn`, `.btn.rc-detect` and
 * `.btn.rc-tx-chip-drop` each declare a padding that never applied, under a
 * comment explaining the effect it was supposed to have.
 *
 * Both are invisible at runtime — nothing throws, the app just does not look
 * the way the stylesheet says. So they are asserted here instead:
 *
 *   1. the load order is themes -> components -> main
 *   2. every class ui.js writes is defined in components.css
 *   3. main.css does not *define* a library class (a scoped override is fine)
 *
 * Plain Node-runnable assertions (no browser, no test runner), same pattern as
 * the other tests/test_frontend_*.js files.
 *
 * Run:
 *   node tests/test_frontend_css_boundary.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const SRC = path.join(__dirname, "..", "src");
const read = (p) => fs.readFileSync(path.join(SRC, p), "utf8");

const uiJs = read("js/ui.js");
const componentsCss = read("css/components.css");
const mainCss = read("css/main.css");
const indexHtml = read("index.html");

const stripComments = (css) => css.replace(/\/\*[\s\S]*?\*\//g, "");

// ---------------------------------------------------------------------------
// 1. Load order: tokens, then the library, then the app.
// ---------------------------------------------------------------------------
// Anything else and rule 3 below stops being enforceable by the cascade: an
// override in main.css would need to out-specify the component rather than
// merely come after it, and a tie would go the wrong way without saying so.
const sheetOrder = [...indexHtml.matchAll(/href="css\/([\w.-]+\.css)"/g)].map((m) => m[1]);
assert.deepEqual(
  sheetOrder,
  ["themes.css", "components.css", "main.css"],
  `index.html must load themes.css, then components.css, then main.css — got ${sheetOrder.join(", ")}`
);

// ---------------------------------------------------------------------------
// The classes ui.js writes.
// ---------------------------------------------------------------------------
// Every factory sets el.className to a literal (sometimes with a concatenated
// modifier, which is why each literal is split on whitespace). Classes that
// only ever arrive through a caller's `className` option are not ui.js's to
// own and are listed as exceptions below.
const libClasses = new Set();
for (const m of uiJs.matchAll(/className\s*=\s*"([^"]+)"/g)) {
  for (const cls of m[1].trim().split(/\s+/)) if (cls) libClasses.add(cls);
}
for (const m of uiJs.matchAll(/classList\.add\("([^"]+)"\)/g)) libClasses.add(m[1]);

assert.ok(libClasses.size > 20, `expected to find the lib's classes in ui.js, found ${libClasses.size}`);

// Classes a factory writes but deliberately does not style itself.
const NOT_OWNED = new Set([
  // A caller-supplied surface: iconButton's `className` option swaps .icon-btn
  // for .mc-btn (map controls) or .nav-item (left rail); those live with the
  // layout they belong to. Listed here only so the default is still checked.
  // (.nav-item itself IS owned — it is what navItem() writes unconditionally.)
  "glass",        // .glass is the shared material, defined in components.css anyway
  // fitBar() measures a bar the CALLER built — .flight-actions, on the Home
  // map and in the planner — and puts one of two narrowing states on it. What
  // those states do to a button is the map chrome's business and lives beside
  // .flight-actions in main.css; the library owns only the decision of which
  // one applies. Defining them here would mean components.css reaching into
  // .fa-btn, which is the boundary this file exists to defend, backwards.
  "is-tight",
  "is-compact",
]);

// ---------------------------------------------------------------------------
// 2. Every library class is defined in components.css.
// ---------------------------------------------------------------------------
const definedIn = (css, cls) =>
  new RegExp(`\\.${cls.replace(/[-/\\^$*+?.()|[\]{}]/g, "\\$&")}(?![\\w-])`).test(stripComments(css));

const undefinedInComponents = [...libClasses]
  .filter((c) => !NOT_OWNED.has(c))
  .filter((c) => !definedIn(componentsCss, c))
  .sort();

assert.deepEqual(
  undefinedInComponents,
  [],
  "ui.js writes these classes but components.css does not style them:\n  " +
    undefinedInComponents.join("\n  ")
);

// ---------------------------------------------------------------------------
// 3. main.css may override a library class, but may not define one.
// ---------------------------------------------------------------------------
// The line between the two is whether the selector is scoped. A bare
// `.page-card { ... }` at the top level is a definition and belongs in
// components.css. `.btn.rc-detect`, `.hud-actions .icon-btn` or a `.nav-item`
// inside a responsive @media block are overrides: each is conditional on
// something the app knows and the library does not.
//
// Top-level only, because a rule inside an at-rule is conditional by
// construction.
//
// The responsive rules are the carve-out that relies on it, and since the
// interface scale they now carry their condition in the selector instead: a
// media query measures the window and cannot see the `zoom` the scale puts
// on <body>, so the width breakpoints became `:where(:root[data-vw~="…"])`
// guards that Corvus.scale keeps in step with the effective viewport (see
// "RESPONSIVE" in main.css). That is the same kind of statement an @media
// block made — conditional on something the app knows and the library does
// not — written where the cascade can see it, so it earns the same carve-out
// and nothing else does: the guard has to be the leading compound.
const RESPONSIVE_GUARD = /^:where\(:root\[data-vw~="[\w-]+"\]\)\s/;

function topLevelSelectors(css) {
  const text = stripComments(css);
  const out = [];
  let depth = 0;
  let head = "";
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (ch === "{") {
      if (depth === 0) out.push(head.trim());
      depth += 1;
      head = "";
    } else if (ch === "}") {
      depth = Math.max(0, depth - 1);
      head = "";
    } else if (depth === 0) {
      head += ch;
    }
  }
  return out.filter((h) => h && !h.startsWith("@"));
}

// A selector "defines" a class when every class it names is a library class —
// i.e. nothing app-side narrows it. `.btn.rc-detect` names rc-detect too, so
// it is an override; `.icon-btn:hover` names only icon-btn, so it is not.
const definitions = [];
for (const head of topLevelSelectors(mainCss)) {
  for (const sel of head.split(",")) {
    const trimmed = sel.trim();
    if (RESPONSIVE_GUARD.test(trimmed)) continue;
    const named = [...trimmed.matchAll(/\.([\w-]+)/g)].map((m) => m[1]);
    if (!named.length) continue;
    if (named.every((c) => libClasses.has(c))) definitions.push(trimmed);
  }
}

assert.deepEqual(
  definitions,
  [],
  "main.css defines library classes that belong in components.css:\n  " +
    definitions.join("\n  ") +
    "\n(a scoped override like `.btn.rc-detect` or `.hud-actions .icon-btn` is fine;" +
    " a bare definition is not)"
);

// ---------------------------------------------------------------------------
// 4. The three overrides that used to lose their tie now win it.
// ---------------------------------------------------------------------------
// A regression guard with names on it: these are the rules whose padding was
// dead CSS under the old order. If someone moves components.css back after
// main.css, check 1 catches it — this one says what it costs.
for (const sel of [".params-actions .btn", ".btn.rc-detect", ".btn.rc-tx-chip-drop"]) {
  assert.ok(
    stripComments(mainCss).includes(sel),
    `${sel} should still be in main.css — it is one of the overrides the load order was fixed for`
  );
}

// ---------------------------------------------------------------------------
// 5. Only themes.css defines global tokens.
// ---------------------------------------------------------------------------
// The spacing and type scales were added there for the same reason this file
// exists: one home per kind of decision. The line is the same as in check 3 —
// scope. A custom property on :root is a design token and belongs in
// themes.css; one declared on a component selector (--js-size on .joystick-pad,
// --hud-w on .flight-overlay) is a local variable for that subtree, which is
// what custom properties are for. Top-level only, so a responsive
// `:root { --panel-w: 320px }` inside a media query stays an override.
function rootBlocks(css) {
  const text = stripComments(css);
  const out = [];
  let depth = 0;
  let head = "";
  let body = "";
  let capturing = false;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (ch === "{") {
      if (depth === 0) {
        capturing = /(^|,)\s*:root\s*$/.test(head.trim()) || head.trim() === ":root";
        body = "";
      }
      depth += 1;
    } else if (ch === "}") {
      depth = Math.max(0, depth - 1);
      if (depth === 0 && capturing) out.push(body);
      if (depth === 0) { capturing = false; head = ""; }
    } else if (depth === 0) {
      head += ch;
    } else if (depth === 1 && capturing) {
      body += ch;
    }
  }
  return out;
}

for (const [name, css] of [["components.css", componentsCss], ["main.css", mainCss]]) {
  const declared = rootBlocks(css).flatMap((body) =>
    [...body.matchAll(/(^|[;\s])(--[\w-]+)\s*:/g)].map((m) => m[2])
  );
  assert.deepEqual(
    [...new Set(declared)],
    [],
    `${name} must not define design tokens on :root — those live in themes.css ` +
      `(found ${[...new Set(declared)].join(", ")})`
  );
}

// And the scales themes.css added are actually reachable: a token nothing
// reads is a token that will drift out of step with the values in use.
for (const token of ["--space-8", "--text-sm"]) {
  assert.ok(
    read("css/themes.css").includes(`${token}:`),
    `themes.css should define ${token}`
  );
  assert.ok(
    componentsCss.includes(`var(${token})`),
    `${token} is defined but unused — components.css should consume it`
  );
}

console.log("test_frontend_css_boundary.js: all assertions passed");
