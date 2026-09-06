"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.credits — the attribution dialog behind Settings > About > Credits.

  Two halves, deliberately sourced differently:

  * The SHIPPED code — vendored libraries, fonts, Python runtime deps — is the
    static list below. Versions are stated because a credit without one cannot
    be checked against what is actually on disk, and the list carries a
    maintenance note for whoever bumps a vendored file next.

  * The MAP SERVICES are read live from GET /api/tiles/sources, whose
    attribution strings come from corvus/tile_sources.py. Those credits are
    legally required and change whenever a source is added, so mirroring them
    here would be exactly the desync the tile registry was consolidated to
    avoid. When the fetch fails, that block simply says so rather than
    inventing an attribution.
*/
Corvus.credits = (function () {

  /*
    MAINTENANCE: when a file under src/vendor/ is replaced, update the matching
    version here. The version strings are in the vendored bundles' own license
    headers (`head -c 600 src/vendor/<file>`), which is where these came from.
  */
  const LIBRARIES = [
    {
      name: "MapLibre GL JS",
      version: "4.7.1",
      license: "BSD 3-Clause",
      role: "Renders the map, its markers and the downloaded-area overlay.",
      url: "https://maplibre.org/",
    },
    {
      name: "Plotly.js (basic)",
      version: "2.35.2",
      license: "MIT",
      role: "Draws the autotune and vibration graphs.",
      url: "https://plotly.com/javascript/",
    },
    {
      name: "Lucide",
      version: "0.544.0",
      license: "ISC",
      role: "The icon set used throughout the interface.",
      url: "https://lucide.dev/",
    },
  ];

  const FONTS = [
    {
      name: "Inter",
      license: "SIL Open Font License 1.1",
      role: "Interface typeface.",
      url: "https://rsms.me/inter/",
    },
    {
      name: "JetBrains Mono",
      license: "SIL Open Font License 1.1",
      role: "Monospace typeface — telemetry values, console, coordinates.",
      url: "https://www.jetbrains.com/lp/mono/",
    },
  ];

  const RUNTIME = [
    {
      name: "Python",
      license: "PSF License",
      role: "The backend runs on it; the desktop build bundles a framework CPython.",
      url: "https://www.python.org/",
    },
    {
      name: "pymavlink",
      license: "LGPL v3",
      role: "MAVLink protocol: telemetry parsing and command encoding.",
      url: "https://github.com/ArduPilot/pymavlink",
    },
    {
      name: "paramiko",
      license: "LGPL 2.1",
      role: "SSH sessions to the companion computer.",
      url: "https://www.paramiko.org/",
    },
    {
      name: "pyserial",
      license: "BSD 3-Clause",
      role: "Serial-port enumeration and the radio link.",
      url: "https://github.com/pyserial/pyserial",
    },
    {
      name: "PyQt6 / PyQt6-WebEngine",
      license: "GPL v3 or commercial (Riverbank)",
      role: "The desktop shell hosting the interface.",
      url: "https://www.riverbankcomputing.com/software/pyqt/",
    },
    {
      name: "pytest",
      license: "MIT",
      role: "Test runner (development only — not shipped).",
      url: "https://pytest.org/",
    },
  ];

  const PROJECT = {
    author: "Maximilian Böck",
    // Always "with", never "at": the institution is a partner in this work,
    // not the place the software is attributed to.
    org: "with Universität der Bundeswehr München",
    note: "Corvus GCS is designed, built and maintained by Maximilian Böck. " +
          "Built for real field use with PX4 autonomous aircraft.",
    // This panel names the licence of every component it lists, so leaving the
    // product's own licence off it was the one gap. The file it points at ships
    // inside the bundle, so this is a claim the operator can actually check.
    license: "Sustainable Use License 1.0 — see LICENSE.md in the application "
             + "folder: internal business, non-commercial and personal use; "
             + "not for resale.",
  };

  /** One credit row: name + version, the license, and what it is used for. */
  function entryRow(e) {
    const row = document.createElement("div");
    row.className = "credits-entry";

    const head = document.createElement("div");
    head.className = "credits-entry-head";
    const name = document.createElement("span");
    name.className = "credits-entry-name";
    name.textContent = e.version ? `${e.name} ${e.version}` : e.name;
    head.appendChild(name);
    const lic = document.createElement("span");
    lic.className = "credits-entry-license";
    lic.textContent = e.license;
    head.appendChild(lic);
    row.appendChild(head);

    if (e.role) {
      const role = document.createElement("span");
      role.className = "credits-entry-role";
      role.textContent = e.role;
      row.appendChild(role);
    }
    if (e.url) {
      // Plain text, not a link: the field laptop has no internet, so a link
      // would be a dead end. The URL is still shown so it can be typed out
      // on a machine that does.
      const url = document.createElement("span");
      url.className = "credits-entry-url";
      url.textContent = e.url;
      row.appendChild(url);
    }
    return row;
  }

  function group(title, entries) {
    const sec = document.createElement("div");
    sec.className = "credits-group";
    sec.appendChild(Corvus.ui.label(title));
    entries.forEach((e) => sec.appendChild(entryRow(e)));
    return sec;
  }

  /**
   * Map-service credits, from the live tile registry. Attribution strings are
   * legally required and belong to corvus/tile_sources.py; this only groups
   * them by service and de-duplicates (Esri Satellite and Hybrid share one).
   */
  function mapServicesGroup() {
    const sec = document.createElement("div");
    sec.className = "credits-group";
    sec.appendChild(Corvus.ui.label("Map data"));
    const pending = Corvus.ui.empty("Loading map attributions…");
    sec.appendChild(pending);

    Corvus.telemetry.requestJson("/api/tiles/sources").then((data) => {
      const sources = (data && data.sources) || [];
      const providers = (data && data.providers) || [];
      if (!sources.length) throw new Error("no sources");
      sec.removeChild(pending);

      providers.forEach((p) => {
        // One service can serve several layers under the same credit line;
        // list each distinct attribution once.
        const seen = [];
        sources.filter((s) => s.provider === p.id).forEach((s) => {
          if (s.attribution && seen.indexOf(s.attribution) === -1) seen.push(s.attribution);
        });
        if (!seen.length) return;
        sec.appendChild(entryRow({
          name: p.label,
          license: seen.length === 1 ? "" : `${seen.length} sources`,
          role: seen.join(" · "),
        }));
      });
    }).catch(() => {
      pending.textContent = "Map attributions unavailable — the backend did not respond.";
    });

    return sec;
  }

  /** Build the dialog body. Version comes from GET /api/version, never a literal. */
  function body(version) {
    const frag = document.createDocumentFragment();

    const intro = document.createElement("div");
    intro.className = "credits-intro";
    const product = document.createElement("div");
    product.className = "credits-product";
    product.textContent = version ? `Corvus GCS ${version}` : "Corvus GCS";
    intro.appendChild(product);
    const by = document.createElement("div");
    by.className = "credits-author";
    by.textContent = `by ${PROJECT.author}`;
    intro.appendChild(by);
    const org = document.createElement("div");
    org.className = "credits-org";
    org.textContent = PROJECT.org;
    intro.appendChild(org);

    // The institution's own wordmark, shown as-is: an official logo is not
    // recolored to suit a theme, so the panel behind it is lightened on the
    // dark themes instead (see .credits-orgmark in components.css).
    const mark = document.createElement("img");
    mark.className = "credits-orgmark";
    mark.src = "assets/unibw_logo.png";
    mark.alt = PROJECT.org;
    intro.appendChild(mark);
    const note = document.createElement("div");
    note.className = "credits-note";
    note.textContent = PROJECT.note;
    intro.appendChild(note);
    const lic = document.createElement("div");
    lic.className = "credits-note credits-license";
    lic.textContent = PROJECT.license;
    intro.appendChild(lic);
    frag.appendChild(intro);

    frag.appendChild(group("Libraries", LIBRARIES));
    frag.appendChild(group("Typefaces", FONTS));
    frag.appendChild(group("Backend & runtime", RUNTIME));
    frag.appendChild(mapServicesGroup());

    const footer = document.createElement("div");
    footer.className = "credits-footer";
    footer.textContent =
      "Every library and font above is vendored under src/vendor/ and loaded " +
      "from disk — the interface requests nothing from the internet. Map tiles " +
      "are fetched by the backend and cached for offline use.";
    frag.appendChild(footer);

    return frag;
  }

  /** Open the credits dialog. */
  function open() {
    Corvus.telemetry.requestJson("/api/version")
      .catch(() => ({}))
      .then((v) => {
        const dialog = Corvus.ui.modal({
          title: "Credits",
          size: "lg",
          body: body(v && v.version),
          actions: Corvus.ui.button({
            variant: "secondary", label: "Close", onClick: () => dialog.close(),
          }),
        });
        dialog.open();
      });
  }

  return { open, LIBRARIES, FONTS, RUNTIME, PROJECT };
})();
