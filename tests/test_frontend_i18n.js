"use strict";

/**
 * Corvus.i18n: the interface language.
 *
 * The pure seams (the lookup, the {name} patterns, the parameter suffix, the
 * code matching), the DOM swap on a small stand-in for the document (text
 * nodes, attributes, what is skipped, how English comes back exactly), the
 * switching and its persistence, and the wiring: the first start setup offers
 * the language in its Look step and not on its first page, and Settings offers
 * a dropdown with a reload button and shows no folder path.
 *
 * Run:
 *   node tests/test_frontend_i18n.js
 */

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

global.window = global;
global.Corvus = {};
const events = [];
window.dispatchEvent = (e) => { events.push(e); return true; };
window.addEventListener = () => {};
global.CustomEvent = class CustomEvent {
  constructor(type, o = {}) { this.type = type; this.detail = o.detail; }
};

const store = {};
let storageThrows = false;
global.localStorage = {
  getItem: (k) => { if (storageThrows) throw new Error("denied"); return k in store ? store[k] : null; },
  setItem: (k, v) => { if (storageThrows) throw new Error("denied"); store[k] = String(v); },
  removeItem: (k) => { delete store[k]; },
};

// ---------------------------------------------------------------------------
// A small document.
// ---------------------------------------------------------------------------

function textNode(value) {
  return { nodeType: 3, nodeValue: value, parentNode: null };
}

function element(tag, attrs, kids) {
  const e = {
    nodeType: 1,
    tagName: tag.toUpperCase(),
    parentNode: null,
    childNodes: [],
    _attrs: Object.assign({}, attrs || {}),
    getAttribute(name) { return name in this._attrs ? this._attrs[name] : null; },
    setAttribute(name, value) { this._attrs[name] = String(value); },
    removeAttribute(name) { delete this._attrs[name]; },
    appendChild(child) { child.parentNode = this; this.childNodes.push(child); return child; },
    matches(selector) {
      const classes = String(this._attrs.class || "").split(/\s+/);
      return this._attrs.translate === "no" || classes.includes("notranslate") || classes.includes("xterm") ||
        (selector.indexOf("zzz") >= 0);
    },
    closest(selector) {
      for (let n = this; n && n.nodeType === 1; n = n.parentNode) if (n.matches(selector)) return n;
      return null;
    },
  };
  (kids || []).forEach((k) => e.appendChild(typeof k === "string" ? textNode(k) : k));
  return e;
}

function text(node) {
  return node.nodeType === 3 ? node.nodeValue : node.childNodes.map(text).join("");
}

const html = element("html", { lang: "en", "data-i18n": "pending" });
const body = element("body");
html.appendChild(body);
global.document = {
  documentElement: html,
  body,
  querySelectorAll: () => [],
};

const observers = [];
global.MutationObserver = class MutationObserver {
  constructor(cb) { this.cb = cb; this.active = false; observers.push(this); }
  observe() { this.active = true; }
  disconnect() { this.active = false; }
};

// ---------------------------------------------------------------------------
// The backend, as far as i18n.js asks it.
// ---------------------------------------------------------------------------

const LANGS = {
  en: { code: "en", name: "English", native_name: "English", source: "builtin", strings: {} },
  de: {
    code: "de", name: "German", native_name: "Deutsch", source: "bundled",
    strings: {
      "Settings": "Einstellungen",
      "Takeoff": "Abheben",
      "Settings of {name}": "Einstellungen von {name}",
      "More information about {x}": "Weitere Informationen zu {x}",
      "Roll {a}°": "Rollen {a}°",
      "Roll {a}°, Pitch {b}°": "Rollen {a}°, Nicken {b}°",
      "Vibration Monitor": "Vibrationsmonitor",
      "Action on RC loss": "Aktion bei RC-Verlust",
      "Blank": "",
      "{x}": "never",
    },
  },
  fr: {
    code: "fr", name: "French", native_name: "Français", source: "bundled",
    strings: { "Settings": "Réglages", "Takeoff": "Décollage" },
  },
};
const requests = [];
let holds = {};
let failStrings = false;
window.fetch = (url) => {
  requests.push(url);
  const reply = (body, ok = true, status = 200) => Promise.resolve({ ok, status, json: () => Promise.resolve(body) });
  if (url === "/api/languages") {
    return reply({
      languages: Object.values(LANGS).map((l) => ({ code: l.code, name: l.name, native_name: l.native_name, source: l.source, count: Object.keys(l.strings).length })),
      default: "en", user_dir: "/home/x/.corvus/languages",
    });
  }
  const m = /\/api\/languages\/strings\?code=([\w-]+)$/.exec(url);
  if (m) {
    if (failStrings) return reply({}, false, 500);
    const entry = LANGS[m[1]];
    if (!entry) return reply({ error: "no" }, false, 404);
    const answer = () => reply(entry);
    return holds[m[1]] ? holds[m[1]].then(answer) : answer();
  }
  return reply({}, false, 404);
};

require(path.join(__dirname, "..", "src", "js", "i18n.js"));
const I = Corvus.i18n;
const tick = () => new Promise((r) => setImmediate(r));

// ---------------------------------------------------------------------------
// Pure
// ---------------------------------------------------------------------------

function testNormalizeFoldsWhitespace() {
  assert.equal(I.normalize("  Download\n      offline   map "), "Download offline map");
}

function testFillReplacesKnownNamesOnly() {
  assert.equal(I.fill("{a} of {b}", { a: 1, b: 2 }), "1 of 2");
  assert.equal(I.fill("{a} of {b}", { a: 1 }), "1 of {b}");
  assert.equal(I.fill("plain", null), "plain");
}

function testLookupExactAndPattern() {
  const t = I.compile(LANGS.de.strings);
  assert.equal(I.lookup(t, "Settings"), "Einstellungen");
  assert.equal(I.lookup(t, "Settings of Schwalby"), "Einstellungen von Schwalby");
  assert.equal(I.lookup(t, "Nothing here"), null);
}

function testAMatchedPartIsTranslatedToo() {
  const t = I.compile(LANGS.de.strings);
  assert.equal(I.lookup(t, "Settings of Vibration Monitor"), "Einstellungen von Vibrationsmonitor");
  assert.equal(I.lookup(t, "More information about Takeoff"), "Weitere Informationen zu Abheben");
}

function testAParameterSuffixIsKeptAndTheLabelTranslated() {
  const t = I.compile(LANGS.de.strings);
  assert.equal(I.lookup(t, "Action on RC loss (NAV_RCL_ACT)"), "Aktion bei RC-Verlust (NAV_RCL_ACT)");
  assert.equal(I.lookup(t, "Unknown label (NAV_RCL_ACT)"), null);
}

function testTheLongerPatternWins() {
  const t = I.compile(LANGS.de.strings);
  assert.equal(I.lookup(t, "Roll 90°"), "Rollen 90°");
  assert.equal(I.lookup(t, "Roll 90°, Pitch 180°"), "Rollen 90°, Nicken 180°");
}

function testABlankTranslationIsNotATranslation() {
  const t = I.compile(LANGS.de.strings);
  assert.equal(I.lookup(t, "Blank"), null);
}

function testAPatternWithNoTextOfItsOwnIsIgnored() {
  const t = I.compile(LANGS.de.strings);
  assert.equal(I.lookup(t, "anything at all"), null);
  assert.equal(t.patterns.length, 4);
}

function testAnEntryKeyIsMatchedWhateverItsWhitespace() {
  const t = I.compile({ "Download   offline\nmap": "Offline-Karte herunterladen" });
  assert.equal(I.lookup(t, I.normalize("Download offline map")), "Offline-Karte herunterladen");
}

function testMatchCode() {
  const codes = ["en", "de", "fr", "pt-BR"];
  assert.equal(I.matchCode("de", codes), "de");
  assert.equal(I.matchCode("DE", codes), "de");
  assert.equal(I.matchCode("de-AT", codes), "de");
  assert.equal(I.matchCode("pt-br", codes), "pt-BR");
  assert.equal(I.matchCode("pt", codes), "pt-BR");
  assert.equal(I.matchCode("es", codes), "");
  assert.equal(I.matchCode("", codes), "");
  assert.equal(I.matchCode(null, codes), "");
}

function testDetectTakesTheFirstInstalledPreference() {
  assert.equal(I.detect(["es-ES", "fr-CA", "de"], ["en", "de", "fr"]), "fr");
  assert.equal(I.detect(["es"], ["en", "de"]), "");
  assert.equal(I.detect([], ["en", "de"]), "");
  assert.equal(I.detect("de-DE", ["en", "de"]), "de");
}

function testFromConfigDefaultsToEnglish() {
  assert.equal(I.fromConfig({ language: "de" }), "de");
  assert.equal(I.fromConfig({}), "en");
  assert.equal(I.fromConfig(null), "en");
  assert.equal(I.fromConfig({ language: 5 }), "en");
}

// ---------------------------------------------------------------------------
// The DOM
// ---------------------------------------------------------------------------

async function testSwitchingLanguagesSwapsTextAndAttributesAndComesBack() {
  const button = element("button", { title: "Takeoff", "aria-label": "Settings", class: "fa-btn" }, [
    element("span", {}, ["  Settings  "]),
    element("span", {}, ["12.5"]),
    element("span", {}, ["Not translated"]),
  ]);
  const input = element("input", { placeholder: "Settings" });
  body.appendChild(button);
  body.appendChild(input);

  await I.refresh();
  assert.equal(await I.use("de"), "de");
  assert.equal(text(button.childNodes[0]), "  Einstellungen  ", "whitespace kept around the translation");
  assert.equal(text(button.childNodes[1]), "12.5");
  assert.equal(text(button.childNodes[2]), "Not translated");
  assert.equal(button.getAttribute("title"), "Abheben");
  assert.equal(button.getAttribute("aria-label"), "Einstellungen");
  assert.equal(input.getAttribute("placeholder"), "Einstellungen");
  assert.equal(html.getAttribute("lang"), "de");
  assert.equal(store["corvus.lang"], "de");

  assert.equal(await I.use("fr"), "fr");
  assert.equal(text(button.childNodes[0]), "  Réglages  ", "a second language starts from the English, not from the first");
  assert.equal(button.getAttribute("title"), "Décollage");

  assert.equal(await I.use("en"), "en");
  assert.equal(text(button.childNodes[0]), "  Settings  ");
  assert.equal(button.getAttribute("title"), "Takeoff");
  assert.equal(input.getAttribute("placeholder"), "Settings");
  assert.equal(html.getAttribute("lang"), "en");
  assert.equal(store["corvus.lang"], "en");
  body.childNodes.length = 0;
}

// A popover leaves the document when it closes, and the observer neither sees
// it then nor runs at all in English: sync() is how it catches up on opening.
async function testASubtreeThatWasOffThePageCatchesUpWithTheLanguage() {
  const sheet = element("div", {}, [element("p", {}, ["Settings"])]);
  await I.refresh();
  await I.use("de");
  body.appendChild(sheet);
  I.translateTree(sheet);
  assert.equal(text(sheet), "Einstellungen");
  body.childNodes.length = 0;
  sheet.parentNode = null;

  await I.use("fr");
  assert.equal(text(sheet), "Einstellungen", "off the page, nothing reaches it");
  body.appendChild(sheet);
  I.sync(sheet);
  assert.equal(text(sheet), "Réglages", "on opening it follows the language, not the one it was last shown in");
  body.childNodes.length = 0;
  sheet.parentNode = null;

  await I.use("de");
  await I.use("en");
  body.appendChild(sheet);
  I.sync(sheet);
  assert.equal(text(sheet), "Settings", "and English comes back exactly");
  body.childNodes.length = 0;
}

async function testWhatIsNotInterfaceIsLeftAlone() {
  const script = element("script", {}, ["Settings"]);
  const area = element("textarea", {}, ["Settings"]);
  const log = element("div", { translate: "no" }, [element("p", {}, ["Settings"])]);
  const term = element("div", { class: "xterm" }, [element("span", {}, ["Settings"])]);
  const plain = element("p", {}, ["Settings"]);
  [script, area, log, term, plain].forEach((e) => body.appendChild(e));
  await I.use("de");
  assert.equal(text(script), "Settings");
  assert.equal(text(area), "Settings");
  assert.equal(text(log), "Settings", "the vehicle's own console lines are never translated");
  assert.equal(text(term), "Settings");
  assert.equal(text(plain), "Einstellungen");
  await I.use("en");
  body.childNodes.length = 0;
}

async function testAnUnknownLanguageLeavesTheCurrentOneAlone() {
  await I.use("de");
  assert.equal(await I.use("xx"), "de");
  assert.equal(I.current(), "de");
  await I.use("en");
}

async function testAFailedLoadLeavesTheCurrentOneAlone() {
  await I.use("en");
  failStrings = true;
  assert.equal(await I.use("de"), "en");
  failStrings = false;
}

async function testTheLastRequestWins() {
  let release;
  holds.de = new Promise((r) => { release = r; });
  const slow = I.use("de");
  const fast = I.use("fr");
  assert.equal(await fast, "fr");
  release();
  await slow;
  holds = {};
  assert.equal(I.current(), "fr", "a slow answer for an older request must not undo a newer one");
  await I.use("en");
}

async function testSwitchingTellsTheRestOfTheApp() {
  events.length = 0;
  await I.use("de");
  assert.deepEqual(events.map((e) => [e.type, e.detail.language]), [["corvus:languagechange", "de"]]);
  await I.use("en");
}

async function testAStorageThatRefusesDoesNotBreakSwitching() {
  storageThrows = true;
  assert.equal(await I.use("de"), "de");
  assert.equal(await I.use("en"), "en");
  storageThrows = false;
}

// ---------------------------------------------------------------------------
// The observer
// ---------------------------------------------------------------------------

function liveObserver() {
  return observers[observers.length - 1];
}

async function testWhatThePageBuildsLaterIsTranslatedToo() {
  await I.use("de");
  const obs = liveObserver();
  assert.ok(obs && obs.active, "an observer runs while a translation is in use");

  const later = element("div", { title: "Takeoff" }, [element("span", {}, ["Settings"])]);
  body.appendChild(later);
  obs.cb([{ type: "childList", target: body, addedNodes: [later], removedNodes: [] }]);
  assert.equal(text(later), "Einstellungen");
  assert.equal(later.getAttribute("title"), "Abheben");
  assert.ok(obs.active, "observing again after its own writes");

  const label = later.childNodes[0].childNodes[0];
  label.nodeValue = "Takeoff";
  obs.cb([{ type: "characterData", target: label }]);
  assert.equal(label.nodeValue, "Abheben", "a text the app rewrites is translated again");

  later.setAttribute("title", "Settings");
  obs.cb([{ type: "attributes", target: later, attributeName: "title" }]);
  assert.equal(later.getAttribute("title"), "Einstellungen");

  const log = element("div", { translate: "no" });
  body.appendChild(log);
  const line = element("p", {}, ["Settings"]);
  log.appendChild(line);
  obs.cb([{ type: "childList", target: log, addedNodes: [line], removedNodes: [] }]);
  assert.equal(text(line), "Settings", "a line added to the console is not translated");

  await I.use("en");
  assert.equal(text(later), "Takeoff", "the nodes built later come back to the English the app last wrote");
  assert.equal(later.getAttribute("title"), "Settings");
  assert.equal(obs.active, false, "English runs no observer");
  body.childNodes.length = 0;
}

async function testItsOwnOutputIsNotReportedAsMissing() {
  const probe = element("p", {}, ["Settings"]);
  body.appendChild(probe);
  await I.use("de");
  const before = I.missing();
  assert.ok(!before.includes("Einstellungen"));
  liveObserver().cb([{ type: "characterData", target: probe.childNodes[0] }]);
  assert.deepEqual(I.missing(), before);
  await I.use("en");
  body.childNodes.length = 0;
}

async function testMissingTextsAreCollectedForTranslators() {
  const page = element("div", {}, [element("p", {}, ["A brand new sentence."]), element("p", {}, ["42"])]);
  body.appendChild(page);
  await I.use("de");
  const missing = I.missing();
  assert.ok(missing.includes("A brand new sentence."));
  assert.ok(!missing.includes("42"), "numbers are not texts");
  const template = I.template();
  assert.equal(template["A brand new sentence."], "");
  await I.use("en");
  body.childNodes.length = 0;
}

// ---------------------------------------------------------------------------
// t()
// ---------------------------------------------------------------------------

async function testTranslatesAStringThatNeverReachesTheDom() {
  assert.equal(Corvus.t("Settings"), "Settings", "English as written");
  assert.equal(Corvus.t('Remove "{name}"?', { name: "A" }), 'Remove "A"?');
  await I.use("de");
  assert.equal(Corvus.t("Settings"), "Einstellungen");
  assert.equal(Corvus.t("Settings of {name}", { name: "x" }), "Einstellungen von x");
  assert.equal(Corvus.t("Never listed {n}", { n: 2 }), "Never listed 2");
  await I.use("en");
}

// ---------------------------------------------------------------------------
// Starting up
// ---------------------------------------------------------------------------

async function testStartUpUsesTheLanguageLastUsed() {
  await I.use("en");
  store["corvus.lang"] = "fr";
  html.setAttribute("data-i18n", "pending");
  const loaded = await I.init();
  assert.equal(loaded, "fr");
  assert.equal(html.getAttribute("data-i18n"), null, "the page is released once the text is in");
  await I.use("en");
}

// ---------------------------------------------------------------------------
// The wiring, read from the source
// ---------------------------------------------------------------------------

const SRC = path.join(__dirname, "..", "src");
const read = (...p) => fs.readFileSync(path.join(SRC, ...p), "utf8");

function between(source, from, to) {
  const a = source.indexOf(from);
  const b = source.indexOf(to, a + 1);
  assert.ok(a >= 0 && b > a, `${from} ... ${to} not found`);
  return source.slice(a, b);
}

function testThePagesLoadI18nBeforeUi() {
  for (const page of ["index.html", "popout.html"]) {
    const src = read(page);
    assert.match(src, /js\/i18n\.js/);
    assert.ok(src.indexOf("js/i18n.js") < src.indexOf("js/ui.js"), `${page}: i18n.js loads before ui.js`);
  }
}

function testThePageIsHeldBackUntilTheSavedLanguageIsIn() {
  const src = read("index.html");
  assert.match(src, /corvus\.lang/);
  assert.match(src, /data-i18n="pending"/);
  assert.match(src, /removeAttribute\("data-i18n"\)/);
}

function testTheVehiclesOwnConsoleIsNeverTranslated() {
  assert.match(read("index.html"), /id="consoleOutput" translate="no"/);
}

function testTheConfigDecidesTheLanguageAtStart() {
  const app = read("js", "app.js");
  assert.match(app, /Corvus\.i18n\.init\(\)/);
  assert.match(app, /Corvus\.i18n\.use\(Corvus\.i18n\.fromConfig\(cfg\)\)/);
}

function testTheFirstStartOffersTheLanguageInTheLookStepNotOnTheFirstPage() {
  const welcome = read("js", "welcome.js");
  const start = between(welcome, "function startPage()", "function stepper()");
  assert.ok(!/language/i.test(start), "the first page of the setup carries no language picker");
  const look = between(welcome, "look() {", "units() {");
  assert.match(look, /languageField\(\)/);
  assert.match(welcome, /language: choices\.language/);
}

function testTheFirstStartUsesADropdown() {
  const welcome = read("js", "welcome.js");
  const field = between(welcome, "function languageField()", "function go(n)");
  assert.match(field, /Corvus\.ui\.select\(/);
  assert.ok(!/optionCards/.test(field));
}

function testSettingsHasADropdownWithAReloadButtonAndNoPath() {
  const sidenav = read("js", "sidenav.js");
  const card = between(sidenav, "function languageCard(gen)", "// Color theme picker.");
  assert.match(card, /Corvus\.ui\.select\(/);
  assert.match(card, /Reload languages/);
  assert.ok(!/optionCards/.test(card), "a dropdown, so more languages fit");
  assert.ok(!/pathLine/.test(card), "the folder's path is not shown");
  assert.ok(!/Open language folder/.test(card), "no button to open the folder");
  assert.ok(!/folder-open/.test(card));
  assert.match(card, /settings-language-row/);
  const css = read("css", "main.css");
  assert.match(css, /\.settings-language-select/);
}

function testTheLanguageCardComesFirstInAppearance() {
  const sidenav = read("js", "sidenav.js");
  const section = between(sidenav, "function renderAppearanceSection", "// Which optional PAGES");
  assert.ok(section.indexOf("languageCard(") < section.indexOf("companyLogoCard("));
}

function testTheShippedLanguagesTranslateTheComposedTexts() {
  const load = (code) => I.compile(JSON.parse(fs.readFileSync(path.join(SRC, "lang", `${code}.json`), "utf8")).strings);
  const de = load("de");
  const fr = load("fr");
  const say = (tbl, text) => I.lookup(tbl, I.normalize(text));
  // Exact, parameter suffix, a pattern, and a pattern whose capture is itself translated.
  assert.equal(say(de, "Settings"), "Einstellungen");
  assert.equal(say(de, "Action on RC loss (NAV_RCL_ACT)"), "Aktion bei RC-Verlust (NAV_RCL_ACT)");
  assert.equal(say(de, "Settings of Vibration Monitor"), "Einstellungen von Vibrationsmonitor");
  assert.equal(say(de, "3 notifications"), "3 Meldungen");
  assert.equal(say(fr, "Roll 90°, Pitch 180°"), "Roulis 90°, tangage 180°");
  assert.equal(say(de, "Roll 90°, Pitch 180°, Yaw 270°"), "Rollen 90°, Nicken 180°, Gieren 270°");
  // The planner's sentences that name the vehicle, in both of its wordings.
  assert.match(say(de, "Click the map where the aircraft takes off."), /Fluggerät abhebt/);
  assert.match(say(de, "Click the map where the vehicle starts driving."), /Fahrzeug losfährt/);
  assert.match(say(fr, "Press START and click where the vehicle starts driving."), /le véhicule démarre/);
  assert.equal(say(de, "The plan reaches 120 m."), "Der Plan erreicht 120 m.");
  assert.equal(say(fr, "Step 2 of 4"), "Étape 2 sur 4");
  // The parameter info popover: its lines, and every family sentence in the editor.
  const dl = load("dl");
  const params = fs.readFileSync(path.join(SRC, "js", "setup-parameters.js"), "utf8");
  const families = [...params.matchAll(/^\s+\[\/\^[^\n]*?\/, "([^"\n]+)"\],$/gm)].map((m) => m[1]);
  assert.ok(families.length > 40, "the family table was found");
  const lines = ["The firmware gives no description for this parameter.", "Changing it needs a reboot.",
    "Default: 12 m/s", "Range: 0 to 20 m/s", "Unit: m/s", "Values: 0 RC only, 1 Joystick only", "Bits: 0 Gyro"];
  for (const [name, tbl] of [["de", de], ["fr", fr], ["dl", dl]]) {
    for (const text of lines.concat(families)) {
      assert.ok(say(tbl, text), `${name} translates: ${text}`);
    }
  }
  assert.equal(say(de, "Range: 0 to 20 m/s"), "Bereich: 0 bis 20 m/s");
  assert.equal(say(fr, "Range: 0 to 20 m/s"), "Plage : 0 à 20 m/s");
  assert.equal(say(de, "Default: 12 m/s"), "Standard: 12 m/s");
  // A dialog question keeps its line breaks.
  assert.equal(say(de, 'Delete the connection Drone?\n\nIts session is open and will be closed.'),
    "Die Verbindung Drone löschen?\n\nIhre Sitzung ist offen und wird geschlossen.");
}

function testTheLanguageFilesShip() {
  for (const code of ["en", "de", "fr"]) {
    const file = JSON.parse(fs.readFileSync(path.join(SRC, "lang", `${code}.json`), "utf8"));
    assert.equal(typeof file.meta.name, "string");
    assert.equal(typeof file.strings, "object");
  }
}

// ---------------------------------------------------------------------------

(async () => {
  const sync = [
    testNormalizeFoldsWhitespace, testFillReplacesKnownNamesOnly, testLookupExactAndPattern,
    testAMatchedPartIsTranslatedToo, testAParameterSuffixIsKeptAndTheLabelTranslated,
    testTheLongerPatternWins, testABlankTranslationIsNotATranslation,
    testAPatternWithNoTextOfItsOwnIsIgnored, testAnEntryKeyIsMatchedWhateverItsWhitespace,
    testMatchCode, testDetectTakesTheFirstInstalledPreference, testFromConfigDefaultsToEnglish,
  ];
  const asyncTests = [
    testSwitchingLanguagesSwapsTextAndAttributesAndComesBack, testASubtreeThatWasOffThePageCatchesUpWithTheLanguage,
    testWhatIsNotInterfaceIsLeftAlone,
    testAnUnknownLanguageLeavesTheCurrentOneAlone, testAFailedLoadLeavesTheCurrentOneAlone,
    testTheLastRequestWins, testSwitchingTellsTheRestOfTheApp,
    testAStorageThatRefusesDoesNotBreakSwitching, testWhatThePageBuildsLaterIsTranslatedToo,
    testItsOwnOutputIsNotReportedAsMissing, testMissingTextsAreCollectedForTranslators,
    testTranslatesAStringThatNeverReachesTheDom, testStartUpUsesTheLanguageLastUsed,
  ];
  const wiring = [
    testThePagesLoadI18nBeforeUi, testThePageIsHeldBackUntilTheSavedLanguageIsIn,
    testTheVehiclesOwnConsoleIsNeverTranslated, testTheConfigDecidesTheLanguageAtStart,
    testTheFirstStartOffersTheLanguageInTheLookStepNotOnTheFirstPage, testTheFirstStartUsesADropdown,
    testSettingsHasADropdownWithAReloadButtonAndNoPath, testTheLanguageCardComesFirstInAppearance,
    testTheLanguageFilesShip, testTheShippedLanguagesTranslateTheComposedTexts,
  ];
  let count = 0;
  for (const t of sync.concat(asyncTests, wiring)) {
    await t();
    count += 1;
    await tick();
  }
  console.log(`test_frontend_i18n: ${count} passed`);
})().catch((err) => {
  console.error(err);
  process.exit(1);
});
