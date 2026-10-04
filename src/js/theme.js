"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.theme — the predefined color themes.

  A theme is a complete palette defined in css/themes.css under a
  `[data-theme="<id>"]` selector; selecting one is a single attribute write on
  <html>. There is deliberately no runtime color math here: the CSS is the
  source of truth for what a theme looks like, and this module only decides
  WHICH one is active. That is what replaced the v1 accent picker, where the
  only themeable value was --accent and every other color stayed dark.

  THEMES mirrors the blocks in css/themes.css and must stay in step with them
  (the ids are the contract). `swatch` is the preview shown in the picker:
  accent, panel surface, page background — the three colors that actually tell
  the themes apart at a glance.

  "light" is the default and the one theme with no block of its own —
  it is what bare `:root` carries in css/themes.css, so it is also what an
  unknown id degrades to. Changing DEFAULT changes only what a fresh install
  gets: an operator who has already picked a theme has it in localStorage and
  in the backend config, and both outrank this.

  Persistence is two-layered on purpose: localStorage so the choice can be
  applied before first paint (see the inline script in index.html) and the
  backend config so it survives a cache clear and follows the operator's
  profile. localStorage is written first and never blocks on the network.
*/
Corvus.theme = (function () {
  const KEY = "corvus.theme";
  const DEFAULT = "light";

  const THEMES = [
    { id: "light",        label: "Light",        desc: "Default",          swatch: ["#1B1F26", "#F1F3F6", "#FFFFFF"] },
    { id: "light-orange", label: "Light Orange", desc: "Corporate Orange", swatch: ["#C2540A", "#FFFFFF", "#F4F6F8"] },
    { id: "green",        label: "Green",        desc: "Dark",             swatch: ["#3DA876", "#171D25", "#0B0E12"] },
    { id: "blue",         label: "Blue",         desc: "Cool",             swatch: ["#3B9EFF", "#171D25", "#0B0E12"] },
    { id: "pink",         label: "Pink",         desc: "Magenta",          swatch: ["#F0509B", "#171D25", "#0B0E12"] },
    { id: "orange",       label: "Orange",       desc: "Amber",            swatch: ["#F58A2B", "#171D25", "#0B0E12"] },
  ];

  const IDS = THEMES.map((t) => t.id);

  /** True when *id* names a theme css/themes.css actually defines. */
  function isKnown(id) { return IDS.indexOf(id) !== -1; }

  /**
   * Apply *id* to the document and cache it. An unknown id (a config from a
   * newer build, a corrupted storage value) falls back to the default rather
   * than leaving the app on a half-applied palette. Returns the id applied.
   */
  function setTheme(id) {
    const v = isKnown(id) ? id : DEFAULT;
    try { document.documentElement.setAttribute("data-theme", v); } catch (_e) {}
    try { localStorage.setItem(KEY, v); } catch (_e) {}
    // Everything styled in CSS restyles itself the moment the attribute
    // changes. Plotly does not: it draws into its own surface from color
    // strings resolved at build time, so the charts have to be told. This is
    // the only reason a theme change is an event at all.
    try {
      window.dispatchEvent(new CustomEvent("corvus:themechange", { detail: { theme: v } }));
    } catch (_e) {}
    return v;
  }

  /** Apply the locally cached theme (called before the config fetch lands). */
  function applySaved() {
    let v = DEFAULT;
    try { v = localStorage.getItem(KEY) || DEFAULT; } catch (_e) {}
    return setTheme(v);
  }

  /**
   * Resolve the theme a backend config asks for. `theme.name` is what the
   * settings page writes today; `theme.accent` is the legacy v1 hex from the
   * old accent picker, kept readable so an existing ~/.corvus/config.json is
   * not a hard error — its nearest predefined theme is used when it matches
   * one, and the default otherwise. Returns null when the config names
   * nothing, so the caller can leave the cached theme alone.
   */
  function fromConfig(cfg) {
    const theme = cfg && cfg.theme;
    if (!theme) return null;
    if (isKnown(theme.name)) return theme.name;
    if (typeof theme.accent === "string") {
      const hex = theme.accent.trim().toLowerCase();
      const match = THEMES.find((t) => t.swatch[0].toLowerCase() === hex);
      return match ? match.id : DEFAULT;
    }
    return null;
  }

  return { setTheme, applySaved, isKnown, fromConfig, THEMES, IDS, DEFAULT };
})();
