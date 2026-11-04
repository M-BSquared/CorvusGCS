"use strict";

/**
 * Frontend test for src/js/map-search.js — the place search both maps mount.
 *
 * Everything asserted here is pure, and all of it is wrong in ways nothing
 * throws over. A place NAME that half-parses into two numbers flies the
 * operator somewhere in the Atlantic instead of saying "no network". A label
 * split at the wrong place hides the one word they typed behind forty
 * characters of postal address. A selection dropped when the network results
 * land moves the row Enter takes out from under their fingers, mid-keystroke.
 * None of it raises, none of it is visible in a screenshot, and all of it is
 * checkable without a browser.
 *
 * The coordinate assertions moved here from tests/test_frontend_mission.js
 * with the code: the box used to live inside the planner and now belongs to
 * both maps.
 *
 * Plain Node-runnable assertions (no browser, no test runner), same pattern as
 * the other tests/test_frontend_*.js files: stub the browser globals the
 * module touches at LOAD time, require the source, assert on the exposed
 * surface. create() is never called, so no DOM and no MapLibre are needed.
 *
 * Run:
 *   node tests/test_frontend_map_search.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

global.window = global;
global.Corvus = {};
global.document = {
  createElement: () => ({
    classList: { add() {}, remove() {}, toggle() {} },
    setAttribute() {}, addEventListener() {}, append() {}, appendChild() {},
    style: {}, dataset: {},
  }),
  addEventListener() {}, removeEventListener() {},
};

require("../src/js/map-search.js");

const search = Corvus.mapSearch;
assert.ok(search, "Corvus.mapSearch must be defined after requiring map-search.js");

// ---------------------------------------------------------------------------
// The offline half: a typed coordinate pair.
// ---------------------------------------------------------------------------
// This is the part of the place search that has to work on a laptop that has
// never seen the internet, which is the case the whole application is built
// for. It is also the part that is dangerous when it is merely *lenient*.

function testDecimalPairsAreReadLatitudeFirst() {
  assert.deepEqual(search._parseCoordinates("48.080217, 11.640969"),
                   { lat: 48.080217, lon: 11.640969 });
  assert.deepEqual(search._parseCoordinates("48.080217 11.640969"),
                   { lat: 48.080217, lon: 11.640969 });
  assert.deepEqual(search._parseCoordinates("  -33.8688 , 151.2093 "),
                   { lat: -33.8688, lon: 151.2093 });
}

function testGermanDecimalCommasAreRead() {
  // The keyboard this is typed on writes 48,08 — and only as the whole
  // string, so a comma stays a SEPARATOR in "48.08, 11.64".
  assert.deepEqual(search._parseCoordinates("48,080217 11,640969"),
                   { lat: 48.080217, lon: 11.640969 });
}

function testHemisphereLettersDecideWhichNumberIsWhich() {
  const expected = { lat: 48.5, lon: 11.25 };
  assert.deepEqual(search._parseCoordinates("48.5N 11.25E"), expected);
  assert.deepEqual(search._parseCoordinates("N48.5 E11.25"), expected);
  // Longitude first is still longitude when it says so.
  assert.deepEqual(search._parseCoordinates("11.25E, 48.5N"), expected);
  assert.deepEqual(search._parseCoordinates("33.8688S 151.2093W"),
                   { lat: -33.8688, lon: -151.2093 });
}

function testDegreesMinutesAndSecondsAreRead() {
  const dms = search._parseCoordinates("48°04'48.8\"N 11°38'27.5\"E");
  assert.ok(Math.abs(dms.lat - (48 + 4 / 60 + 48.8 / 3600)) < 1e-9);
  assert.ok(Math.abs(dms.lon - (11 + 38 / 60 + 27.5 / 3600)) < 1e-9);
  const dm = search._parseCoordinates("48 4.813 N, 11 38.458 E");
  assert.ok(Math.abs(dm.lat - (48 + 4.813 / 60)) < 1e-9);
  assert.ok(Math.abs(dm.lon - (11 + 38.458 / 60)) < 1e-9);
}

function testAFirstNumberThatCannotBeALatitudeIsReadAsTheLongitude() {
  // A copy-paste out of something that writes lon,lat. Reading 151 as a
  // latitude is not a near miss, it is a different planet.
  assert.deepEqual(search._parseCoordinates("151.2093, -33.8688"),
                   { lat: -33.8688, lon: 151.2093 });
  assert.deepEqual(search._parseCoordinates("91.0, 11.0"),
                   { lat: 11.0, lon: 91.0 });
  // Only when it is unambiguous: a letter says which is which, and a pair
  // where BOTH could be the latitude is left in the order it was typed.
  assert.equal(search._parseCoordinates("91.0N 11.0E"), null);
  assert.deepEqual(search._parseCoordinates("48.5, 11.25"),
                   { lat: 48.5, lon: 11.25 });
}

function testAPlaceNameIsNeverParsedAsAPosition() {
  const names = [
    "Manching", "Ingolstadt 5", "airfield 48", "48 north", "EDMS",
    "48.08", "", "   ", "1 2 3 4 5 6 7", "48.08, 11.64, 300",
  ];
  names.forEach((name) => {
    assert.equal(search._parseCoordinates(name), null,
      `"${name}" must not read as a coordinate pair`);
  });
}

function testOutOfRangeCoordinatesAreRefused() {
  // Neither number can be the latitude, so there is nothing to salvage.
  assert.equal(search._parseCoordinates("95, 96"), null);
  assert.equal(search._parseCoordinates("48.0, 181.0"), null);
  assert.equal(search._parseCoordinates("91.0N 11.0E"), null);
  assert.equal(search._parseCoordinates("48°70'00\"N 11°00'00\"E"), null);
}

// ---------------------------------------------------------------------------
// The row: what a result says before it is picked.
// ---------------------------------------------------------------------------
// Nominatim answers with a full postal address in one field. Drawn as one
// ellipsized run it puts the word the operator typed in front of everything
// they did not, and seven of those is a wall rather than a list.

function testAPlaceNameIsSplitFromItsAddress() {
  assert.deepEqual(
    search._splitLabel("Neubiberg, Landkreis München, Bayern, 85579, Deutschland"),
    { name: "Neubiberg", context: "Landkreis München, Bayern, 85579, Deutschland" },
  );
}

function testALabelWithNoAddressIsAllName() {
  // A downloaded area is named by the operator, and a coordinate pair is a
  // pair of numbers; neither has an address to put on the second line.
  assert.deepEqual(search._splitLabel("Manching"), { name: "Manching", context: "" });
  assert.deepEqual(search._splitLabel(""), { name: "", context: "" });
  assert.deepEqual(search._splitLabel(null), { name: "", context: "" });
}

function testWhatThisLaptopAnsweredIsMarkedAsItsOwn() {
  // The row is drawn from the label, and only a GEOCODED label is an address
  // with a place at the front of it. A coordinate pair is one thing that
  // happens to contain a comma: split like an address it showed the latitude
  // as the name and the longitude as its county, which is two numbers that
  // mean nothing apart.
  const regions = [{ name: "Manching", bounds: { w: 11.4, s: 48.6, e: 11.6, n: 48.8 } }];
  assert.equal(search._regionMatches(regions, "manching")[0].local, true,
    "a downloaded area is this laptop's own answer");
  const source = fs.readFileSync(
    path.join(__dirname, "..", "src", "js", "map-search.js"), "utf8");
  assert.match(source, /kind: "coordinates",\s*\n\s*local: true,/,
    "and so is a typed coordinate pair");
  assert.match(source, /row\.local \? \{ name: row\.label, context: "" \} : splitLabel\(row\.label\)/,
    "neither of them may be split at its first comma");
}

function testALabelThatOpensWithACommaKeepsItsWholeText() {
  // Nothing before the comma means there is no name to lift out of it, and a
  // row whose first line is empty is a row that says nothing.
  const odd = search._splitLabel(", Bayern, Deutschland");
  assert.equal(odd.name, ", Bayern, Deutschland");
  assert.equal(odd.context, "");
}

// ---------------------------------------------------------------------------
// The distance column.
// ---------------------------------------------------------------------------
// The reason it exists: a search for a common name answers with several of
// it, and which one is the site they are driving to is a question the names
// cannot settle and the distance can.

function testDistanceMatchesAKnownSeparation() {
  // One degree of latitude is ~111.2 km anywhere on the globe.
  const metres = search._distanceM({ lat: 48, lon: 11 }, { lat: 49, lon: 11 });
  assert.ok(Math.abs(metres - 111195) < 400, `one degree of latitude was ${metres} m`);
  assert.equal(search._distanceM({ lat: 48, lon: 11 }, { lat: 48, lon: 11 }), 0);
}

function testDistanceIsWrittenAtThePrecisionItIsWorth() {
  // Metres near to hand, one decimal of a kilometre in the middle, whole
  // kilometres far away — a column of six significant figures is noise.
  assert.equal(search._formatDistance(0), "0 m");
  assert.equal(search._formatDistance(342), "340 m");
  assert.equal(search._formatDistance(1420), "1.4 km");
  assert.equal(search._formatDistance(48200), "48 km");
  // Nothing to measure from is an empty column, never "0 m" — which would be
  // a claim that the place is where you are standing.
  assert.equal(search._formatDistance(NaN), "");
  assert.equal(search._formatDistance(Infinity), "");
}

// ---------------------------------------------------------------------------
// Downloaded areas, matched on this laptop.
// ---------------------------------------------------------------------------

function testADownloadedAreaIsFoundByNameAndFramedByItsBounds() {
  const regions = [
    { name: "Manching", bounds: { w: 11.4, s: 48.6, e: 11.6, n: 48.8 } },
    { name: "Ingolstadt", bounds: { w: 11.3, s: 48.7, e: 11.5, n: 48.9 } },
  ];
  const rows = search._regionMatches(regions, "manch");
  assert.equal(rows.length, 1);
  assert.equal(rows[0].label, "Manching");
  assert.equal(rows[0].kind, "downloaded area");
  // The row flies to the middle of the box and frames the box itself.
  assert.ok(Math.abs(rows[0].lat - 48.7) < 1e-9);
  assert.ok(Math.abs(rows[0].lon - 11.5) < 1e-9);
  assert.deepEqual(rows[0].bounds, { w: 11.4, s: 48.6, e: 11.6, n: 48.8 });
}

function testARegionWithoutUsableBoundsIsNotOffered() {
  // These come off the backend. A row with a NaN corner would fitBounds the
  // camera into nowhere.
  const regions = [
    { name: "Broken", bounds: { w: 11.4, s: 48.6, e: null, n: 48.8 } },
    { name: "Nameless", bounds: { w: 11.4, s: 48.6, e: 11.6, n: 48.8 } },
    { name: "Manching" },
  ];
  assert.deepEqual(search._regionMatches(regions, "broken"), []);
  assert.deepEqual(search._regionMatches(regions, "manching"), []);
  assert.equal(search._regionMatches(regions, "nameless").length, 1);
  assert.deepEqual(search._regionMatches(null, "anything"), []);
  assert.deepEqual(search._regionMatches([], "  "), []);
}

function testOneAreaIsOfferedOnceHoweverManyLayersItWasPulledFor() {
  // Tiles are downloaded per layer, so the same place is several rows in the
  // registry and must be one row in the list.
  const regions = [
    { name: "Manching", source: "satellite", bounds: { w: 11.4, s: 48.6, e: 11.6, n: 48.8 } },
    { name: "manching", source: "streets", bounds: { w: 11.4, s: 48.6, e: 11.6, n: 48.8 } },
  ];
  assert.equal(search._regionMatches(regions, "Manching").length, 1);
}

// ---------------------------------------------------------------------------
// The list is redrawn under the operator's fingers.
// ---------------------------------------------------------------------------
// It is drawn twice per search: once with what this laptop can answer alone,
// and again ~350 ms later when the network results land. Whoever pressed
// ArrowDown in that window had their selection thrown back to row 0 — and
// then Enter flew them to a different place than the one they were looking at.

function testTheSelectedRowSurvivesTheNetworkResultsArriving() {
  const local = [{ label: "48.08, 11.64" }, { label: "Manching (downloaded)" }];
  const withNetwork = local.concat([{ label: "Manching, Bavaria" }, { label: "Manching Air Base" }]);

  // The operator arrowed down to the second row before the network answered.
  assert.equal(search._keepSelection(local[1], withNetwork), 1);
  // And the row they are on is still the row they chose, by label.
  assert.equal(withNetwork[1].label, "Manching (downloaded)");
}

function testARowThatMovedIsStillTheRowTheyChose() {
  const before = [{ label: "a" }, { label: "b" }, { label: "c" }];
  const after = [{ label: "x" }, { label: "y" }, { label: "b" }];
  assert.equal(search._keepSelection(before[1], after), 2);
}

function testANewSearchStartsAtTheTop() {
  const rows = [{ label: "Ingolstadt" }, { label: "Ingolstadt Nord" }];
  // Nothing was selected, or what was selected is gone: first row, which is
  // what an operator who has not arrowed anywhere expects Enter to take.
  assert.equal(search._keepSelection(null, rows), 0);
  assert.equal(search._keepSelection({ label: "somewhere else" }, rows), 0);
}

function testAnEmptyListSelectsNothing() {
  assert.equal(search._keepSelection(null, []), -1);
  assert.equal(search._keepSelection({ label: "a" }, []), -1);
}

function testAJunkRowInTheListIsNotDereferenced() {
  // The rows come off the network; a null in the array must not throw
  // inside the one function that runs on every redraw.
  assert.equal(search._keepSelection({ label: "b" }, [null, { label: "b" }]), 1);
  assert.equal(search._keepSelection({ label: "z" }, [null, { label: "b" }]), 0);
}

// ---------------------------------------------------------------------------
// One box, both maps.
// ---------------------------------------------------------------------------
// The whole point of the module: this was 450 lines inside mission.js, so the
// Home map had no way to reach a place by name at all. A second copy over
// there would drift; a shared one cannot.

function read(file) {
  return fs.readFileSync(path.join(__dirname, "..", "src", file), "utf8");
}

function testBothMapsMountTheSameSearch() {
  assert.match(read("js/map.js"), /Corvus\.mapSearch\.create\(/,
    "the Home map must mount the shared search");
  assert.match(read("js/mission.js"), /Corvus\.mapSearch\.create\(/,
    "the Mission map must mount the shared search");
  assert.doesNotMatch(read("js/mission.js"), /function parseCoordinates/,
    "the planner must not keep a second copy of the coordinate parser");
}

function testTheSearchIsLoadedBeforeTheMapsThatMountIt() {
  // Every tag is `defer`, and deferred scripts execute in document order, so
  // "before" here is literally the order of the lines.
  const html = read("index.html");
  const searchAt = html.indexOf('src="js/map-search.js"');
  const mapAt = html.indexOf('src="js/map.js"');
  const missionAt = html.indexOf('src="js/mission.js"');
  assert.ok(searchAt > 0, "index.html must load js/map-search.js");
  assert.ok(searchAt < mapAt, "map-search.js must be loaded before map.js");
  assert.ok(searchAt < missionAt, "map-search.js must be loaded before mission.js");
}

function testTheBoxClosesOnAnOutsidePress() {
  const source = read("js/map-search.js");
  // Capture phase, like Corvus.ui.menu: a handler further down (the map's own
  // drag start) must not be able to swallow the press first.
  assert.match(source, /document\.addEventListener\("pointerdown", onOutside, true\)/);
  // A press inside the box — which includes the trigger button — is not an
  // outside press, or the trigger would close here and reopen on click.
  assert.match(source, /el\.contains\(target\)\) return/);
}

function testEveryDocumentListenerIsAlsoRemoved() {
  // The planner's map is built and torn down on every visit, so a listener
  // this box adds and never removes accumulates one live handler per visit,
  // each holding the closure and the map it captured.
  const source = read("js/map-search.js");
  const added = new Set();
  const removed = new Set();
  const pattern = /document\.(add|remove)EventListener\(\s*"([^"]+)"\s*,\s*([A-Za-z0-9_$]+)/g;
  let match;
  while ((match = pattern.exec(source)) !== null) {
    (match[1] === "add" ? added : removed).add(`${match[2]}:${match[3]}`);
  }
  assert.ok(added.size, "no document listeners found — did the pattern stop matching?");
  for (const key of added) {
    assert.ok(removed.has(key),
      `document listener ${key} is added but never removed; it leaks on teardown`);
  }
}

function testChoosingAPlaceIsReportedToWhicheverMapMountedTheBox() {
  // A fly-to is programmatic and carries no originalEvent, so neither map's
  // own movestart handler can see it. onGo is how it is said out loud — and
  // both hosts have something to do about it: the Home map stops following
  // the aircraft, and both record the view the operator asked for so the two
  // screens open on the same ground.
  assert.match(read("js/map-search.js"), /options\.onGo === "function"/);
  assert.match(read("js/map.js"), /onGo: \(\) => \{ setFollow\(false\); noteHomeAim\(\); \}/);
  assert.match(read("js/mission.js"), /onGo: \(\) => \{ if \(map\) map\.once\("moveend", noteAim\); \}/);
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

const tests = [
  testDecimalPairsAreReadLatitudeFirst,
  testGermanDecimalCommasAreRead,
  testHemisphereLettersDecideWhichNumberIsWhich,
  testDegreesMinutesAndSecondsAreRead,
  testAFirstNumberThatCannotBeALatitudeIsReadAsTheLongitude,
  testAPlaceNameIsNeverParsedAsAPosition,
  testOutOfRangeCoordinatesAreRefused,
  testAPlaceNameIsSplitFromItsAddress,
  testALabelWithNoAddressIsAllName,
  testALabelThatOpensWithACommaKeepsItsWholeText,
  testWhatThisLaptopAnsweredIsMarkedAsItsOwn,
  testDistanceMatchesAKnownSeparation,
  testDistanceIsWrittenAtThePrecisionItIsWorth,
  testADownloadedAreaIsFoundByNameAndFramedByItsBounds,
  testARegionWithoutUsableBoundsIsNotOffered,
  testOneAreaIsOfferedOnceHoweverManyLayersItWasPulledFor,
  testTheSelectedRowSurvivesTheNetworkResultsArriving,
  testARowThatMovedIsStillTheRowTheyChose,
  testANewSearchStartsAtTheTop,
  testAnEmptyListSelectsNothing,
  testAJunkRowInTheListIsNotDereferenced,
  testBothMapsMountTheSameSearch,
  testTheSearchIsLoadedBeforeTheMapsThatMountIt,
  testTheBoxClosesOnAnOutsidePress,
  testEveryDocumentListenerIsAlsoRemoved,
  testChoosingAPlaceIsReportedToWhicheverMapMountedTheBox,
];

let failures = 0;
tests.forEach((test) => {
  try {
    test();
    console.log(`ok - ${test.name}`);
  } catch (error) {
    failures += 1;
    console.error(`FAIL - ${test.name}\n  ${error.message}`);
  }
});
console.log(`\n${tests.length - failures}/${tests.length} passed`);
process.exit(failures ? 1 : 0);
