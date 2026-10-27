"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.i18n: the interface language.

  The English text of the interface is the key. A language file (src/lang/*.json,
  or ~/.corvus/languages/*.json for one an operator adds) maps that text to its
  translation, and this module swaps the text on screen. The code that builds
  the interface keeps writing plain English: nothing in it carries a message id,
  and a text a language does not list simply stays English.

  How the swap works. Every text node and every title, placeholder, aria-label
  and alt of the page is looked up by its English wording (whitespace folded),
  and replaced when the language has an entry. A MutationObserver does the same
  for anything the page builds later, so a modal, a setup page or a plugin is
  translated without knowing about this module. The English original of each
  node is kept on the node, which is what makes a switch back to English, or on
  to a third language, exact instead of a guess.

  A key with {name} in it is a pattern: it matches any text in that place, and
  the same {name} in the translation puts the matched text back. That is how
  "Settings of Vibration" is translated by one entry, "Settings of {name}".

  English is the built-in default and costs nothing: with it no observer runs.

  Persistence is two-layered like the theme: localStorage so the first paint of
  the next start is already in the right language (see the inline script in
  index.html), and the backend config (`language`), which wins once it is read.
*/
Corvus.i18n = (function () {
  const KEY = "corvus.lang";
  const DEFAULT = "en";
  const ATTRS = ["title", "placeholder", "aria-label", "alt"];
  const LETTER = /[A-Za-zÀ-ɏ]/;
  const HOLE = /\{([A-Za-z_][A-Za-z0-9_]*)\}/g;
  const PARAM_SUFFIX = /^(.+) \(([A-Z][A-Z0-9_]{2,})\)$/;
  const SKIP_TAGS = { SCRIPT: 1, STYLE: 1, NOSCRIPT: 1, TEXTAREA: 1 };
  const SKIP_SELECTOR = '[translate="no"], .notranslate, .xterm';
  const CACHE_MAX = 4000;
  const MISSED_MAX = 6000;
  const TEXT_MAX = 400;
  const PENDING_MS = 2500;

  const BUILTIN = { code: DEFAULT, name: "English", native_name: "English", source: "builtin", count: 0 };

  let code = DEFAULT;
  let table = null;
  let generation = 0;
  let languages = [BUILTIN];
  let userDir = "";
  let observer = null;
  let collecting = false;
  let started = false;
  let listing = null;
  let seq = 0;
  const cache = new Map();
  const missed = new Map();

  /* ------------------------------- pure -------------------------------- */

  /** The lookup key of a piece of text: whitespace folded and trimmed. */
  function normalize(text) {
    return String(text).replace(/\s+/g, " ").trim();
  }

  /** *template* with each {name} replaced from *vars*; unknown names stay. */
  function fill(template, vars) {
    if (!vars) return template;
    return String(template).replace(HOLE, (whole, name) =>
      (Object.prototype.hasOwnProperty.call(vars, name) ? String(vars[name]) : whole));
  }

  /**
   * The lookup structure of a `strings` object: an exact map, and the keys
   * with {name} in them as patterns (longest first, so the most specific
   * wins). An empty translation is an untranslated entry and is left out.
   */
  function compile(strings) {
    const exact = new Map();
    const patterns = [];
    const values = new Set();
    Object.keys(strings || {}).forEach((rawKey) => {
      const value = strings[rawKey];
      if (typeof value !== "string" || !value.trim()) return;
      const key = normalize(rawKey);
      if (!key) return;
      values.add(normalize(value));
      const names = [];
      let source = "";
      let last = 0;
      let prefix = null;
      key.replace(HOLE, (whole, name, at) => {
        if (prefix === null) prefix = key.slice(0, at);
        source += escapeRe(key.slice(last, at)) + "([\\s\\S]+?)";
        names.push(name);
        last = at + whole.length;
        return whole;
      });
      if (!names.length) {
        exact.set(key, value);
        return;
      }
      const suffix = key.slice(last);
      if (!key.replace(HOLE, "").trim()) return;
      source += escapeRe(suffix);
      patterns.push({
        re: new RegExp("^" + source + "$"), names, value, prefix, suffix, weight: key.length,
      });
    });
    patterns.sort((a, b) => b.weight - a.weight);
    return { exact, patterns, values };
  }

  function escapeRe(text) {
    return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  }

  /**
   * The translation of the normalized *key* in *tbl*, or null.
   *
   * What a {name} matched is translated too, when the language has an entry
   * for it, so "More information about {label}" carries every label the page
   * has without listing each of them.
   *
   * A label that names the parameter it edits, "Action on RC loss
   * (NAV_RCL_ACT)", is translated by its label alone: the parameter is the
   * autopilot's own name and stays as it is, so a language does not list each
   * of them.
   */
  function lookup(tbl, key, depth) {
    if (!tbl || !key) return null;
    const hit = tbl.exact.get(key);
    if (hit !== undefined) return hit;
    const level = depth || 0;
    if (level > 2) return null;
    const named = PARAM_SUFFIX.exec(key);
    if (named) {
      const label = lookup(tbl, named[1], level + 1);
      return label === null ? null : label + " (" + named[2] + ")";
    }
    if (key.length > TEXT_MAX) return null;
    for (let i = 0; i < tbl.patterns.length; i++) {
      const p = tbl.patterns[i];
      if (p.prefix && !key.startsWith(p.prefix)) continue;
      if (p.suffix && !key.endsWith(p.suffix)) continue;
      const m = p.re.exec(key);
      if (!m) continue;
      const vars = {};
      p.names.forEach((name, n) => {
        const inner = lookup(tbl, normalize(m[n + 1]), level + 1);
        vars[name] = inner === null ? m[n + 1] : inner;
      });
      return fill(p.value, vars);
    }
    return null;
  }

  /**
   * The installed code that best answers *wanted*: the same code in any case,
   * else the same primary language ("de-AT" is served by "de"), else "".
   */
  function matchCode(wanted, codes) {
    const w = String(wanted || "").toLowerCase();
    if (!w) return "";
    const list = codes || [];
    const exact = list.find((c) => c.toLowerCase() === w);
    if (exact) return exact;
    const primary = w.split("-")[0];
    const close = list.find((c) => c.toLowerCase() === primary);
    if (close) return close;
    return list.find((c) => c.toLowerCase().split("-")[0] === primary) || "";
  }

  /** The first of the browser's *preferred* languages that is installed, or "". */
  function detect(preferred, codes) {
    const prefs = Array.isArray(preferred) ? preferred : (preferred ? [preferred] : []);
    for (let i = 0; i < prefs.length; i++) {
      const hit = matchCode(prefs[i], codes);
      if (hit) return hit;
    }
    return "";
  }

  /** The language a GET /api/config body asks for ("en" when it names none). */
  function fromConfig(cfg) {
    const v = cfg && cfg.language;
    return (typeof v === "string" && v) ? v : DEFAULT;
  }

  /* ----------------------------- translating --------------------------- */

  function translate(text) {
    const key = normalize(text);
    if (cache.has(key)) return cache.get(key);
    const out = lookup(table, key);
    if (cache.size >= CACHE_MAX) cache.clear();
    cache.set(key, out);
    if (out === null && key && missed.size < MISSED_MAX && !(table && table.values.has(key))) {
      missed.set(key, true);
    }
    return out;
  }

  /**
   * The text for *english* in the current language, with {name} filled from
   * *vars*. For strings that never reach the DOM as a text node: a confirm()
   * question, a file name. English comes back as given.
   */
  function t(english, vars) {
    const out = table ? translate(english) : null;
    return fill(out === null ? english : out, vars);
  }

  function active() { return !!table || collecting; }

  function skipped(node) {
    const el = node && (node.nodeType === 1 ? node : node.parentNode);
    if (!el || el.nodeType !== 1) return false;
    if (SKIP_TAGS[el.tagName]) return true;
    return typeof el.closest === "function" && !!el.closest(SKIP_SELECTOR);
  }

  function translateText(node) {
    let raw = node.nodeValue;
    if (!raw || !LETTER.test(raw)) return;
    const rec = node.__i18n;
    let stale = false;
    if (rec && raw === rec.out) {
      if (rec.gen === generation) return;
      // Translated under an earlier language while off the page (a closed
      // popover): the English it came from is the only honest source.
      raw = rec.src;
      stale = true;
    }
    const out = translate(raw);
    if (out === null || !table) {
      if (stale) node.nodeValue = raw;
      if (rec) node.__i18n = undefined;
      return;
    }
    const lead = raw.match(/^\s*/)[0];
    const trail = raw.match(/\s*$/)[0];
    const next = lead + out + trail;
    node.nodeValue = next;
    node.__i18n = { src: raw, out: next, gen: generation };
  }

  function translateAttr(el, name) {
    let raw = el.getAttribute(name);
    if (!raw || !LETTER.test(raw)) return;
    const recs = el.__i18nAttr || (el.__i18nAttr = {});
    const rec = recs[name];
    let stale = false;
    if (rec && raw === rec.out) {
      if (rec.gen === generation) return;
      raw = rec.src;
      stale = true;
    }
    const out = translate(raw);
    if (out === null || !table) {
      if (stale) el.setAttribute(name, raw);
      if (rec) recs[name] = undefined;
      return;
    }
    el.setAttribute(name, out);
    recs[name] = { src: raw, out, gen: generation };
  }

  /** Translate *root* and everything under it. */
  function translateTree(root) {
    if (!root) return;
    if (root.nodeType === 3) { translateText(root); return; }
    if (root.nodeType === 1) {
      if (SKIP_TAGS[root.tagName]) return;
      if (typeof root.matches === "function" && root.matches(SKIP_SELECTOR)) return;
      for (let i = 0; i < ATTRS.length; i++) translateAttr(root, ATTRS[i]);
    } else if (root.nodeType !== 9 && root.nodeType !== 11) {
      return;
    }
    const kids = root.childNodes || [];
    for (let i = 0; i < kids.length; i++) translateTree(kids[i]);
  }

  /** Put the English back under *root* wherever this module replaced it. */
  function restoreTree(root) {
    if (!root) return;
    if (root.nodeType === 3) {
      const rec = root.__i18n;
      if (rec && root.nodeValue === rec.out) root.nodeValue = rec.src;
      root.__i18n = undefined;
      return;
    }
    if (root.nodeType === 1 && root.__i18nAttr) {
      const recs = root.__i18nAttr;
      ATTRS.forEach((name) => {
        const rec = recs[name];
        if (rec && root.getAttribute(name) === rec.out) root.setAttribute(name, rec.src);
      });
      root.__i18nAttr = undefined;
    }
    const kids = root.childNodes || [];
    for (let i = 0; i < kids.length; i++) restoreTree(kids[i]);
  }

  /* ------------------------------ observer ------------------------------ */

  function root() {
    return (typeof document !== "undefined" && (document.body || document.documentElement)) || null;
  }

  function onMutations(records) {
    if (!active()) return;
    pause();
    try {
      for (let i = 0; i < records.length; i++) {
        const r = records[i];
        if (r.type === "childList") {
          if (!r.addedNodes.length || skipped(r.target)) continue;
          for (let n = 0; n < r.addedNodes.length; n++) translateTree(r.addedNodes[n]);
        } else if (r.type === "characterData") {
          if (!LETTER.test(r.target.nodeValue || "") || skipped(r.target)) continue;
          translateText(r.target);
        } else if (r.type === "attributes") {
          if (skipped(r.target)) continue;
          translateAttr(r.target, r.attributeName);
        }
      }
    } finally {
      resume();
    }
  }

  function startObserver() {
    const el = root();
    if (observer || !el || typeof MutationObserver !== "function") return;
    observer = new MutationObserver(onMutations);
  }

  function resume() {
    const el = root();
    if (!observer || !el) return;
    observer.observe(el, {
      childList: true, subtree: true, characterData: true,
      attributes: true, attributeFilter: ATTRS,
    });
  }

  /* Our own writes must not come back to us as mutations. */
  function pause() {
    if (observer) observer.disconnect();
  }

  function stopObserver() {
    if (observer) { observer.disconnect(); observer = null; }
  }

  /* ------------------------------- loading ------------------------------ */

  function getJson(url) {
    return window.fetch(url, { cache: "no-store" }).then((res) => {
      if (!res.ok) throw new Error(url + " answered " + res.status);
      return res.json();
    });
  }

  function readSaved() {
    try { return window.localStorage.getItem(KEY) || ""; } catch (_e) { return ""; }
  }

  function writeSaved(value) {
    try { window.localStorage.setItem(KEY, value); } catch (_e) { /* optional */ }
  }

  function endPending() {
    try { document.documentElement.removeAttribute("data-i18n"); } catch (_e) { /* optional */ }
  }

  function codes() {
    return languages.map((l) => l.code);
  }

  /**
   * Ask the backend which languages are installed. Without an answer the list
   * stays at English, which is always there.
   */
  function refresh() {
    listing = getJson("/api/languages").then((data) => {
      const list = (data && Array.isArray(data.languages)) ? data.languages : [];
      languages = list.some((l) => l.code === DEFAULT) ? list : [BUILTIN].concat(list);
      userDir = (data && data.user_dir) || userDir;
      return languages;
    }).catch(() => languages);
    return listing;
  }

  function apply(next, entry) {
    pause();
    if (table) restoreTree(root());
    code = next;
    table = (entry && entry.strings) ? compile(entry.strings) : null;
    cache.clear();
    generation += 1;
    try { document.documentElement.setAttribute("lang", code); } catch (_e) { /* optional */ }
    if (table || collecting) {
      startObserver();
      translateTree(root());
      resume();
    } else {
      stopObserver();
    }
    // A dropdown keeps a copy of its option's text, which the pass above has
    // just changed under it.
    if (typeof document !== "undefined" && document.querySelectorAll) {
      document.querySelectorAll("select").forEach((sel) => {
        if (sel.corvusSelect && typeof sel.corvusSelect.refresh === "function") sel.corvusSelect.refresh();
      });
    }
    writeSaved(code);
    endPending();
    try {
      window.dispatchEvent(new CustomEvent("corvus:languagechange", { detail: { language: code } }));
    } catch (_e) { /* optional */ }
    return code;
  }

  /**
   * Switch the interface to *wanted* (a code, or anything matchCode maps to
   * one). Resolves with the code now in use: an unknown or unreadable language
   * leaves the current one alone, and English is always available. The last
   * call wins when two overlap.
   */
  function use(wanted) {
    const mine = ++seq;
    // Asked before the list has arrived (the config answers about as fast as
    // the folder scan does), so it waits for it rather than finding no match.
    return (listing || refresh()).then(() => {
      const target = matchCode(wanted, codes()) || (String(wanted || "") === DEFAULT ? DEFAULT : "");
      if (mine !== seq) return code;
      if (!target) { endPending(); return code; }
      if (target === DEFAULT) return apply(DEFAULT, null);
      return getJson("/api/languages/strings?code=" + encodeURIComponent(target)).then((entry) => {
        if (mine !== seq) return code;
        return apply(target, entry);
      });
    }).catch(() => {
      if (mine === seq) endPending();
      return code;
    });
  }

  /**
   * Start-up: the languages installed, then the one last used on this machine.
   * The backend config overrides it a moment later (app.js), so a stale cache
   * costs one switch, never a wrong answer. Resolves once the page is settled.
   */
  function init() {
    if (started) return Promise.resolve(code);
    started = true;
    const saved = readSaved();
    const timer = window.setTimeout(endPending, PENDING_MS);
    return refresh().then(() => (saved && saved !== DEFAULT ? use(saved) : code)).then((c) => {
      window.clearTimeout(timer);
      endPending();
      return c;
    });
  }

  /**
   * The installed languages as dropdown options, each in its own name first
   * ("Deutsch (German)"), so somebody who lands in a language they cannot read
   * can still find theirs.
   */
  function options(forSetup) {
    return languages.filter((l) => !(forSetup && l.last)).map((l) => ({
      value: l.code,
      label: l.name !== l.native_name ? l.native_name + " (" + l.name + ")" : l.native_name,
    }));
  }

  /** Read the folder again, and the current language's file with it. */
  function reload() {
    return refresh().then(() => {
      const keep = code;
      if (keep === DEFAULT) return keep;
      if (!codes().includes(keep)) return use(DEFAULT);
      return use(keep);
    });
  }

  /* ----------------------------- for translators ----------------------- */

  /**
   * Start recording every text the page shows that the language does not
   * translate, even in English. For writing a language file: open the pages,
   * then call Corvus.i18n.template().
   */
  function collect(on) {
    collecting = on !== false;
    if (collecting) { pause(); startObserver(); translateTree(root()); resume(); }
    else if (!table) stopObserver();
    return collecting;
  }

  /**
   * Bring a subtree that sat off the page (a closed popover) into the current
   * language. The observer only sees what is attached while it runs, and not
   * at all in English, so such a node keeps the language it was last shown in.
   */
  function sync(node) {
    if (!node) return;
    if (!active()) { restoreTree(node); return; }
    pause();
    try { translateTree(node); } finally { if (active()) resume(); }
  }

  /**
   * Whether *el* already shows *english*, as written or as this module
   * translated it. Code that rewrites a label only when it changed has to ask
   * this rather than compare textContent: a translated label never equals its
   * English, so the comparison would rewrite it on every frame, and the
   * translation would land a moment later each time.
   */
  function shows(el, english) {
    if (!el) return false;
    const text = english == null ? "" : String(english);
    if (el.textContent === text) return true;
    const node = el.firstChild;
    if (!node || node.nodeType !== 3 || node.nextSibling) return false;
    const rec = node.__i18n;
    return !!rec && rec.gen === generation && rec.src === text && node.nodeValue === rec.out;
  }

  /** The English texts seen on screen that the current language lacks, sorted. */
  function missing() {
    return Array.from(missed.keys()).sort();
  }

  /** missing() as a language file's `strings`, each with a blank to fill in. */
  function template() {
    const out = {};
    missing().forEach((key) => { out[key] = ""; });
    return out;
  }

  return {
    init, use, reload, refresh, options, t, collect, missing, template, sync, shows,
    current: () => code,
    available: () => languages.slice(),
    userDir: () => userDir,
    fromConfig, detect, matchCode, DEFAULT,
    // Exported for the test suite.
    normalize, fill, compile, lookup, translateTree, restoreTree,
  };
})();

/* The same call under a name short enough to put around a literal. */
Corvus.t = function (english, vars) { return Corvus.i18n.t(english, vars); };
