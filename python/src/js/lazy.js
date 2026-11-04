"use strict";
/*
  Vendor bundles that are not needed to open the app.

  index.html used to load every vendored library up front, as blocking
  <script> tags: maplibre (1.0 MB), plotly-basic (1.0 MB), xterm plus its fit
  addon (~300 KB), lucide (368 KB). The app opens on the map. Plotly is used
  by Analysis, Tuning and the mission altitude profile; xterm is used by an
  SSH terminal. On a launch that never leaves the map — which is most of them
  — 1.3 MB was read off disk, pushed through the socket and parsed by
  Chromium to define globals nothing would call.

  So those two are fetched the first time something actually draws with them.
  Everything else stays where it was: maplibre is the first paint, and lucide
  swaps the icon placeholders on every screen including the first.

  The contract this has to honour is the awkward part, and it is why this is
  a module rather than four inline appendChild calls:

  * Once per bundle, however many callers ask at once. The promise is cached
    under its src, so three charts drawing in the same frame share one fetch.
  * A failed load is not cached. Corvus runs offline by design; a fetch that
    failed because the page was mid-reload must be retryable, or the feature
    is dead until the app restarts.
  * The caller may be gone by the time it resolves. Every call site here
    re-checks its own liveness flag (`destroyed`, `entry.dead`) after the
    await, exactly as it would after any other async gap.
  * `available()` stays synchronous and truthful. Code that only wants to
    clean up — Plotly.purge on teardown — must be able to ask "is it there?"
    without causing a 1 MB download on the way out.
*/
window.Corvus = window.Corvus || {};
Corvus.lazy = (function () {
  /** src -> Promise, resolved when that bundle's globals exist. */
  const pending = new Map();

  /**
   * Load one script once. Resolves when it has executed, rejects if it could
   * not be fetched — and forgets a rejection so the next caller retries.
   */
  function script(src) {
    if (pending.has(src)) return pending.get(src);
    const promise = new Promise((resolve, reject) => {
      if (typeof document === "undefined") {
        reject(new Error("no document"));
        return;
      }
      const el = document.createElement("script");
      el.src = src;
      // Order matters between a bundle and its addon (xterm, then the fit
      // addon): async=false keeps injected scripts executing in the order
      // they were added, which `defer` semantics give us for free.
      el.async = false;
      el.addEventListener("load", () => resolve());
      el.addEventListener("error", () => {
        pending.delete(src);
        el.remove();
        reject(new Error(`could not load ${src}`));
      });
      document.head.appendChild(el);
    });
    pending.set(src, promise);
    return promise;
  }

  /** Is Plotly here already? Synchronous, and never starts a load. */
  function plotlyReady() {
    return typeof window !== "undefined" && !!window.Plotly;
  }

  /** Plotly, fetched if this is the first chart of the session. */
  function plotly() {
    if (plotlyReady()) return Promise.resolve(window.Plotly);
    return script("vendor/plotly-basic.min.js").then(() => window.Plotly);
  }

  /** Is xterm (and its fit addon) here already? Synchronous. */
  function terminalReady() {
    return typeof window !== "undefined"
      && typeof window.Terminal === "function"
      && !!(window.FitAddon && window.FitAddon.FitAddon);
  }

  /** xterm + the fit addon, in that order — the addon needs the base. */
  function terminal() {
    if (terminalReady()) return Promise.resolve(window.Terminal);
    return script("vendor/xterm.js")
      .then(() => script("vendor/xterm-addon-fit.js"))
      .then(() => window.Terminal);
  }

  return { script, plotly, plotlyReady, terminal, terminalReady };
})();
