"use strict";

/**
 * The Settings page's pills (js/sidenav.js, SETTINGS_TABS).
 *
 * The page used to be one long column; it is now a row of pills, one per
 * topic, each with its own panel. What can go wrong in a regrouping like that
 * is quiet: a card that no pill builds any more simply disappears, and its
 * config key stops being applied on entry. So this asserts that every card
 * the page owns is still built by exactly one pill, that the pill row is a
 * real tab list, and that every heading it adds is translated.
 *
 * Run:
 *   node tests/test_frontend_settings_tabs.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const SRC = path.join(__dirname, "..", "src");
const read = (...p) => fs.readFileSync(path.join(SRC, ...p), "utf8");
const sidenav = read("js", "sidenav.js");
const css = require("./support/page_css.js").pageCss();

function between(source, from, to) {
  const a = source.indexOf(from);
  const b = source.indexOf(to, a + 1);
  assert.ok(a >= 0 && b > a, `${from} ... ${to} not found`);
  return source.slice(a, b);
}

const tabsBlock = between(sidenav, "const SETTINGS_TABS = [", "];");
const TAB_IDS = [...tabsBlock.matchAll(/\{ id: "([a-z]+)"/g)].map((m) => m[1]);
const TAB_LABELS = [...tabsBlock.matchAll(/label: "([^"]+)"/g)].map((m) => m[1]);
const renderers = between(sidenav, "const SETTINGS_TABS = [", "function renderSettingsTabs(");

function testThePillsAreTheTopicsInOrder() {
  assert.deepEqual(TAB_IDS, [
    "interface", "layout", "flight", "map", "controls", "connections", "data", "plugins", "about",
  ]);
  assert.equal(TAB_LABELS.length, TAB_IDS.length, "every pill has a label");
  assert.equal([...tabsBlock.matchAll(/icon: "/g)].length, TAB_IDS.length, "every pill has an icon");
}

// Every card the old Appearance section built, and where it lives now.
const HOME = {
  languageCard: "renderInterfaceTab", themeCard: "renderInterfaceTab",
  scaleCard: "renderInterfaceTab", unitsCard: "renderInterfaceTab",
  companyLogoCard: "renderInterfaceTab", appIconCard: "renderInterfaceTab",
  pagesCard: "renderLayoutTab", sidePanelCard: "renderLayoutTab",
  windowsCard: "renderLayoutTab", topBarCard: "renderFlightTab",
  notificationsCard: "renderLayoutTab", flightBarCard: "renderFlightTab",
  flightHudCard: "renderFlightTab", compassCard: "renderFlightTab",
  mapServiceCard: "renderMapTab", elevationDataCard: "renderMapTab",
  trackCard: "renderMapTab", mapToolsCard: "renderMapTab", mapMotionCard: "renderMapTab",
  controlsCard: "renderControlsTab",
};

function testEveryCardIsBuiltByExactlyOnePill() {
  for (const [card, tab] of Object.entries(HOME)) {
    const calls = renderers.split(card + "(").length - 1;
    assert.equal(calls, 1, `${card} is built once by the pills, not ${calls} times`);
    const body = between(renderers, `function ${tab}(`, "\n  }\n");
    assert.ok(body.includes(card + "("), `${card} belongs on ${tab}`);
  }
}

function testNoCardIsLeftWithoutACaller() {
  const defined = [...sidenav.matchAll(/function (\w+Card)\(/g)].map((m) => m[1]);
  assert.ok(defined.length >= Object.keys(HOME).length);
  for (const name of defined) {
    const calls = sidenav.split(name + "(").length - 1;
    assert.ok(calls >= 2, `${name} is defined but nothing builds it`);
  }
}

function testTheOtherSectionsMovedIntoPills() {
  const page = between(sidenav, "function renderSettingsPage(", "function renderPlaceholder(");
  assert.match(page, /renderSettingsTabs\(container, cfg, gen\)/);
  for (const fn of ["renderSSHSection", "renderVehiclesSection", "renderConnectionSection",
    "renderFilesSection", "renderAnalysisSection", "renderPluginsSection",
    "renderAppearanceSection"]) {
    assert.ok(!page.includes(fn + "("), `${fn} is built by its pill, not by the page`);
  }
  for (const fn of ["renderSSHSection", "renderVehiclesSection", "renderConnectionSection",
    "renderFilesSection", "renderAnalysisSection", "renderPluginsSection",
    "renderAboutSection", "settingsTransfer.section"]) {
    assert.ok(renderers.includes(fn), `${fn} is on a pill`);
  }
  assert.ok(!sidenav.includes("function renderAppearanceSection"), "the old single section is gone");
}

function testThePillRowIsATabList() {
  const fn = between(sidenav, "function renderSettingsTabs(", "\n  }\n\n");
  assert.match(fn, /setAttribute\("role", "tablist"\)/);
  assert.match(fn, /setAttribute\("role", "tab"\)/);
  assert.match(fn, /setAttribute\("role", "tabpanel"\)/);
  assert.match(fn, /"aria-selected"/);
  assert.match(fn, /ArrowRight/);
  // Built up front, hidden or not: cards apply the config as they are built.
  assert.match(fn, /t\.render\(panel, cfg, gen\)/);
  assert.match(fn, /p\.hidden = p\.dataset\.tab !== id/);
  // Not the side panel switches' ids (settingsTab_<id>).
  assert.match(fn, /"settingsPill_" \+ t\.id/);
  assert.match(css, /\.settings-tab\s*\{/);
  assert.match(css, /\.settings-tab\.active\s*\{/);
  assert.match(css, /\.settings-panel\[hidden\]\s*\{\s*display: none/);
}

function testTheUpdateDotIsRepeatedOnAbout() {
  const fn = between(sidenav, "function paintUpdateBadge(", "\n  }\n");
  assert.match(fn, /\.settings-tab\[data-tab="about"\]/);
  assert.match(css, /\.settings-tab\.has-update::after/);
}

function testEveryHeadingIsTranslated() {
  const groups = [...renderers.matchAll(/cardGroup\(container, "([^"]+)"/g)].map((m) => m[1]);
  const files = between(sidenav, "function renderFilesSection(", "\n  }\n");
  groups.push(...[...files.matchAll(/cardGroup\(container, "([^"]+)"/g)].map((m) => m[1]));
  assert.ok(groups.length >= 8);
  for (const code of ["de", "fr", "dl"]) {
    const strings = JSON.parse(read("lang", `${code}.json`)).strings;
    for (const text of [...TAB_LABELS, ...groups]) {
      assert.ok(strings[text], `${code}.json translates "${text}"`);
    }
  }
}

const tests = [
  testThePillsAreTheTopicsInOrder,
  testEveryCardIsBuiltByExactlyOnePill,
  testNoCardIsLeftWithoutACaller,
  testTheOtherSectionsMovedIntoPills,
  testThePillRowIsATabList,
  testTheUpdateDotIsRepeatedOnAbout,
  testEveryHeadingIsTranslated,
];
for (const t of tests) t();
console.log(`settings tabs: ${tests.length} passed`);
