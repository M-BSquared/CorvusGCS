"use strict";

/**
 * The two patches this repository carries against vendored libraries, and the
 * reason they have to stay.
 *
 * Both libraries do their own hit-testing, and both do it the same way: take
 * `event.clientX - element.getBoundingClientRect().left` and use it against a
 * geometry measured in the element's own layout pixels. Those two agree only
 * while nothing scales the element in between.
 *
 * This app scales everything: the interface-size control is a CSS `zoom` on
 * <body> (see --ui-scale in css/themes.css). A client rect comes back in the
 * window's pixels, everything below <body> is laid out in pixels the zoom
 * then multiplies, and an offset taken in the first and used in the second is
 * wrong by exactly the scale. Corvus.ui.uiScale() is that correction for the
 * app's own code; these two libraries needed telling as well.
 *
 * Plotly already makes this correction for CSS *transforms* — it walks the
 * ancestor chain, inverts what it finds and divides it back out. `zoom` is
 * not a transform, so the walk cannot see it. The patch divides the element's
 * currentCSSZoom out of the same matrix, which is why one edit fixes hover,
 * drag and box-zoom together: they all read that matrix.
 *
 * xterm has no such machinery; its one helper returning a pointer position
 * relative to the screen element is patched directly.
 *
 * Measured before the patches, at 150%: hovering a station on the mission
 * profile showed nothing at all, a box-zoom dragged over one region selected
 * another (12% and 30% of the axis span out), and pointing at column 11 of a
 * terminal selected column 16.
 *
 * A vendored file gets replaced wholesale on an upgrade and these patches go
 * with it, without a single test failing anywhere else. That is what this
 * file is for. If it fails after an upgrade, re-apply the patch — the exact
 * edit is printed with the failure — rather than deleting the test.
 *
 * Run:
 *   node tests/test_frontend_vendor_zoom.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const VENDOR = path.join(__dirname, "..", "src", "vendor");
const read = (f) => fs.readFileSync(path.join(VENDOR, f), "utf8");

const OPEN = "/*CORVUS-ZOOM-PATCH*/";
const CLOSE = "/*CORVUS-ZOOM-PATCH-END*/";

/** The patch body, with its markers, or null when it is gone. */
function extract(src) {
  const a = src.indexOf(OPEN);
  const b = src.indexOf(CLOSE);
  if (a < 0 || b < 0 || b < a) return null;
  return { patch: src.slice(a + OPEN.length, b), around: src.slice(Math.max(0, a - 420), b + 420) };
}

// ---------------------------------------------------------------------------
// plotly-basic.min.js — calcInverseTransform
// ---------------------------------------------------------------------------
const PLOTLY_FIX = `
Re-apply it like this. In src/vendor/plotly-basic.min.js find Plotly's
calcInverseTransform, which reads:

  function se(e){var t=e._fullLayout,r=e.getBoundingClientRect();
    if(!o.equalDomRects(r,t._lastBBox)){
      var n=t._invTransform=o.inverseTransformMatrix(o.getFullTransformMatrix(e));
      t._invScaleX=Math.sqrt(...),t._invScaleY=Math.sqrt(...),t._lastBBox=r}}

and insert, immediately after the line that assigns \`n\`:

  ${OPEN}var cvz=e.currentCSSZoom;if(typeof cvz==="number"&&isFinite(cvz)&&cvz>0&&cvz!==1)for(var cvi=0;cvi<n.length;cvi++)for(var cvj=0;cvj<3&&cvj<n[cvi].length;cvj++)n[cvi][cvj]/=cvz;${CLOSE}

The names are long on purpose so they cannot collide with the minifier's.
Only columns 0..2 are touched: column 3 is the translation, and the offset
handed to this matrix is already relative to the graph div. _invScaleX and
_invScaleY are derived from those same columns on the next two lines, which
is why one edit fixes hover and drag together.
`;

{
  const src = read("plotly-basic.min.js");
  const found = extract(src);
  assert.ok(found,
    "src/vendor/plotly-basic.min.js has lost the interface-scale patch — every " +
    "chart in the app now answers the pointer in the wrong place above 100%.\n" + PLOTLY_FIX);

  const { patch, around } = found;
  assert.ok(around.includes("_invTransform=") && around.includes("inverseTransformMatrix"),
    "the patch is no longer inside calcInverseTransform — it corrects the matrix " +
    "that function builds, and is inert anywhere else.\n" + PLOTLY_FIX);
  assert.ok(around.includes("_invScaleX=") && around.includes("_invScaleY="),
    "the patch must sit BEFORE _invScaleX / _invScaleY are derived, or the drag " +
    "path keeps the uncorrected factor.\n" + PLOTLY_FIX);
  assert.ok(patch.includes("currentCSSZoom"),
    "the patch must read the element's own cumulative zoom, not the --ui-scale " +
    "token: a chart in a plugin may sit under a different one.\n" + PLOTLY_FIX);
  assert.ok(/\/=\s*cvz/.test(patch),
    "the correction divides by the zoom; multiplying doubles the error.\n" + PLOTLY_FIX);
  assert.ok(patch.includes("cvz!==1"),
    "at 100% the matrix must be left exactly as Plotly built it.\n" + PLOTLY_FIX);
  assert.ok(patch.includes("isFinite(cvz)") && patch.includes("cvz>0"),
    "a browser without currentCSSZoom reports undefined and must fall through to " +
    "Plotly's own behaviour rather than producing NaN coordinates.\n" + PLOTLY_FIX);
  assert.ok(/cvj<3/.test(patch),
    "only the linear columns (0..2) are scaled — column 3 is the translation, and " +
    "the offset handed in is already relative to the graph div.\n" + PLOTLY_FIX);

  // The arithmetic, lifted out and run, so the loop bounds are checked rather
  // than trusted.
  const correct = new Function("n", "e", patch + "return n;");
  {
    const m = [[1, 0, 0, 7], [0, 1, 0, 9], [0, 0, 1, 0], [0, 0, 0, 1]];
    const out = correct(m.map((r) => r.slice()), { currentCSSZoom: 1.5 });
    assert.equal(Math.round(out[0][0] * 1000) / 1000, 0.667, "x scales by 1/1.5");
    assert.equal(Math.round(out[1][1] * 1000) / 1000, 0.667, "y scales by 1/1.5");
    assert.equal(out[0][3], 7, "the translation column is not the pointer's scale");
    assert.equal(out[1][3], 9);
  }
  {
    const m = [[1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]];
    assert.deepEqual(correct(m.map((r) => r.slice()), { currentCSSZoom: 1 }), m,
      "at 100% nothing is touched");
    assert.deepEqual(correct(m.map((r) => r.slice()), {}), m,
      "a browser that reports no zoom keeps Plotly's own matrix");
    assert.deepEqual(correct(m.map((r) => r.slice()), { currentCSSZoom: 0 }), m,
      "and a nonsense zoom must not divide by zero");
    const small = correct(m.map((r) => r.slice()), { currentCSSZoom: 0.8 });
    assert.equal(Math.round(small[0][0] * 1000) / 1000, 1.25,
      "a shrunk interface is the same correction with the other sign");
  }
}

// ---------------------------------------------------------------------------
// xterm.js — getCoordsRelativeToElement
// ---------------------------------------------------------------------------
const XTERM_FIX = `
Re-apply it like this. In src/vendor/xterm.js find the helper behind
getCoordsRelativeToElement, which reads:

  function i(e,t,i){const s=i.getBoundingClientRect(),r=e.getComputedStyle(i),
    n=parseInt(r.getPropertyValue("padding-left")),
    o=parseInt(r.getPropertyValue("padding-top"));
    return[t.clientX-s.left-n,t.clientY-s.top-o]}

and replace its \`return\` with:

  ${OPEN}var cvz=i.currentCSSZoom;if(typeof cvz!=="number"||!isFinite(cvz)||cvz<=0)cvz=1;return[(t.clientX-s.left)/cvz-n,(t.clientY-s.top)/cvz-o];${CLOSE}

Only the rect-relative part is divided. The paddings come from
getComputedStyle, which already reports the element's own pixels, so dividing
them too would take the correction off twice.
`;

{
  const src = read("xterm.js");
  const found = extract(src);
  assert.ok(found,
    "src/vendor/xterm.js has lost the interface-scale patch — a selection in the " +
    "SSH or console terminal now grabs the wrong characters above 100%.\n" + XTERM_FIX);

  const { patch, around } = found;
  assert.ok(around.includes("getCoordsRelativeToElement"),
    "the patch is no longer beside getCoordsRelativeToElement, which is the one " +
    "place xterm turns a pointer into a cell.\n" + XTERM_FIX);
  assert.ok(patch.includes("currentCSSZoom"), "the patch must read the zoom.\n" + XTERM_FIX);
  assert.ok(/\(t\.clientX-s\.left\)\/cvz/.test(patch) && /\(t\.clientY-s\.top\)\/cvz/.test(patch),
    "both axes are corrected, and the division brackets the rect-relative part " +
    "only — the paddings are already in the element's own pixels.\n" + XTERM_FIX);
  assert.ok(/-n\]|-n,/.test(patch) && /-o\]/.test(patch),
    "the paddings must still be subtracted, and subtracted OUTSIDE the division.\n" + XTERM_FIX);

  // Run it, against a stub of the three arguments xterm passes.
  const coords = new Function("e", "t", "i", "s", "r", "n", "o", patch);
  const run = (zoom, clientX, clientY) => coords(
    null,                                  // e: the window (only used above)
    { clientX, clientY },                  // t: the event
    { currentCSSZoom: zoom },              // i: the screen element
    { left: 100, top: 50 },                // s: its client rect
    null, 4, 2                             // r unused here; n/o: the paddings
  );

  assert.deepEqual(run(1, 300, 250), [196, 198], "at 100%: 300-100-4, 250-50-2");
  assert.deepEqual(run(1.5, 400, 350), [196, 198],
    "at 150% the same CELL is three times as far from the edge in window " +
    "pixels, and must come back as the same cell");
  assert.deepEqual(run(undefined, 300, 250), [196, 198],
    "a browser that reports no zoom behaves exactly as xterm did");
  assert.deepEqual(run(0, 300, 250), [196, 198], "and a nonsense zoom does not divide by zero");
  assert.deepEqual(run(0.5, 200, 150), [196, 198], "a shrunk interface, the other way");
}

console.log("test_frontend_vendor_zoom.js: all assertions passed");
