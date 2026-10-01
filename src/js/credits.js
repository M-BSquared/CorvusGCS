"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.credits — the attribution dialog behind Settings > About > Credits.

  Two halves, deliberately sourced differently:

  * The SHIPPED code — vendored libraries, fonts, Python runtime deps, and the
    outside data services the backend talks to — is the static lists below. Versions are stated because a credit without one cannot
    be checked against what is actually on disk, and the list carries a
    maintenance note for whoever bumps a vendored file next.

  * The MAP SERVICES and ELEVATION MODELS are read live from GET /api/tiles/sources, whose
    attribution strings come from corvus/tile_sources.py. Those credits are
    legally required and change whenever a source is added, so mirroring them
    here would be exactly the desync the tile registry was consolidated to
    avoid. When the fetch fails, that block simply says so rather than
    inventing an attribution.
*/
Corvus.credits = (function () {

  /*
    MAINTENANCE: when a file under src/vendor/ is replaced, update the matching
    version here, and when a package is added to pyproject.toml (or one of its
    own dependencies changes), add or adjust its RUNTIME row. The version strings are in the vendored bundles' own license
    headers (`head -c 600 src/vendor/<file>`), which is where these came from.
  */
  const LIBRARIES = [
    {
      name: "MapLibre GL JS",
      version: "5.24.0",
      license: "BSD 3-Clause",
      copyright: "Copyright MapLibre contributors; portions Mapbox, Inc.",
      role: "Renders the map, its markers and the downloaded-area overlay. " +
            "Carries a small local patch (tile request queue).",
      url: "https://maplibre.org/",
    },
    {
      name: "Plotly.js (basic)",
      version: "2.35.2",
      license: "MIT",
      copyright: "Copyright 2012 to 2024 Plotly, Inc. Bundles d3 (BSD 3-Clause, Mike Bostock) and other MIT/BSD components.",
      role: "Draws the autotune and vibration graphs. Carries a small local patch (pointer position under interface scale).",
      url: "https://plotly.com/javascript/",
    },
    {
      name: "Lucide",
      version: "0.544.0",
      license: "ISC",
      copyright: "Copyright Lucide Contributors; portions Cole Bemis (Feather, MIT).",
      role: "The icon set used throughout the interface.",
      url: "https://lucide.dev/",
    },
    {
      name: "xterm.js",
      version: "5.5.0",
      license: "MIT",
      copyright: "Copyright The xterm.js authors; Christopher Jeffrey (term.js).",
      role: "The terminal emulator behind the SSH tab. Carries a small local patch (selection position under interface scale).",
      url: "https://xtermjs.org/",
    },
    {
      name: "xterm.js fit addon",
      license: "MIT",
      copyright: "Copyright The xterm.js authors.",
      role: "Sizes the terminal to its window.",
      url: "https://xtermjs.org/",
    },
  ];

  const FONTS = [
    {
      name: "Inter",
      license: "SIL Open Font License 1.1",
      copyright: "Copyright The Inter Project Authors (Rasmus Andersson).",
      role: "Interface typeface.",
      url: "https://rsms.me/inter/",
    },
    {
      name: "JetBrains Mono",
      license: "SIL Open Font License 1.1",
      copyright: "Copyright The JetBrains Mono Project Authors (JetBrains s.r.o.).",
      role: "Monospace typeface for telemetry values, console, coordinates.",
      url: "https://www.jetbrains.com/lp/mono/",
    },
  ];

  const RUNTIME = [
    {
      name: "Python",
      license: "PSF License",
      role: "The backend runs on it; the desktop build bundles its own CPython, " +
            "including the libraries its standard library is built with, such as " +
            "OpenSSL, SQLite, XZ Utils and zlib.",
      url: "https://www.python.org/",
    },
    {
      name: "pymavlink",
      license: "LGPL v3",
      role: "MAVLink protocol: telemetry parsing and command encoding. " +
            "Includes the MAVLink message definitions (MIT).",
      url: "https://github.com/ArduPilot/pymavlink",
    },
    {
      name: "MAVLink message definitions",
      license: "MIT",
      role: "The common and dialect message sets that pymavlink generates from.",
      url: "https://mavlink.io/",
    },
    {
      name: "lxml",
      license: "BSD 3-Clause",
      role: "XML parsing used by pymavlink to build its dialects. Includes libxml2 and libxslt (MIT).",
      url: "https://lxml.de/",
    },
    {
      name: "fastcrc",
      license: "MIT",
      role: "Checksums for MAVLink frames, used by pymavlink.",
      url: "https://github.com/overcat/fastcrc",
    },
    {
      name: "paramiko",
      license: "LGPL 2.1",
      role: "SSH sessions to the companion computer.",
      url: "https://www.paramiko.org/",
    },
    {
      name: "cryptography",
      license: "Apache 2.0 or BSD 3-Clause",
      role: "Ciphers and key handling under paramiko. Ships its own OpenSSL (Apache 2.0).",
      url: "https://cryptography.io/",
    },
    {
      name: "bcrypt",
      license: "Apache 2.0",
      role: "Key derivation for encrypted SSH keys, used by paramiko.",
      url: "https://github.com/pyca/bcrypt",
    },
    {
      name: "PyNaCl",
      license: "Apache 2.0",
      role: "Ed25519 keys for SSH, used by paramiko. Includes libsodium (ISC).",
      url: "https://github.com/pyca/pynacl",
    },
    {
      name: "invoke",
      license: "BSD 2-Clause",
      role: "Command execution helper required by paramiko.",
      url: "https://www.pyinvoke.org/",
    },
    {
      name: "cffi / pycparser",
      license: "MIT or MIT-0 / BSD 3-Clause",
      role: "Native bindings under cryptography, bcrypt and PyNaCl.",
      url: "https://cffi.readthedocs.io/",
    },
    {
      name: "pyserial",
      license: "BSD 3-Clause",
      role: "Serial-port enumeration and the radio link.",
      url: "https://github.com/pyserial/pyserial",
    },
    {
      name: "FFmpeg",
      license: "LGPL 2.1+ or GPL 2+, depending on the build",
      role: "Decodes RTSP and other camera streams for the video windows. " +
            "Not bundled: Corvus runs the ffmpeg installed on the computer.",
      url: "https://ffmpeg.org/",
    },
    {
      name: "PySide6 (Qt for Python)",
      license: "LGPL v3 or GPL v2/v3 (The Qt Company)",
      role: "The desktop shell hosting the interface. Used under the LGPL v3, " +
            "whose text ships in assets/licenses.",
      url: "https://pyside.org/",
    },
    {
      name: "Shiboken6",
      license: "LGPL v3 or GPL v2/v3 (The Qt Company)",
      role: "Binding layer between Python and Qt, required by PySide6.",
      url: "https://doc.qt.io/qtforpython-6/shiboken6/index.html",
    },
    {
      name: "Qt 6",
      license: "LGPL v3 or GPL v3 (The Qt Company)",
      role: "The windowing and widget toolkit under PySide6, shipped as shared libraries.",
      url: "https://www.qt.io/",
    },
    {
      name: "Qt WebEngine / Chromium",
      license: "LGPL v3 / GPL v3, with BSD 3-Clause and other open-source components",
      role: "The embedded browser that renders the interface. Includes FFmpeg (LGPL 2.1) " +
            "for video playback. The full component list is in Chromium's own credits.",
      url: "https://doc.qt.io/qt-6/qtwebengine-licensing.html",
    },
    {
      name: "PyInstaller",
      license: "GPL v2 with bootloader exception",
      role: "Packs the Windows build. Its bootloader is part of that build only.",
      url: "https://pyinstaller.org/",
    },
    {
      name: "AppImage runtime",
      license: "MIT",
      role: "The small starter at the front of the Linux AppImage that mounts and runs it, " +
            "added by appimagetool. Linux build only.",
      url: "https://appimage.org/",
    },
    {
      name: "Pillow",
      license: "MIT-CMU",
      role: "Cuts the Windows icon from the logo at build time (not shipped).",
      url: "https://python-pillow.github.io/",
    },
    {
      name: "pytest",
      license: "MIT",
      role: "Test runner (development only, not shipped).",
      url: "https://pytest.org/",
    },
    {
      name: "Ruff",
      license: "MIT",
      role: "Linter (development only, not shipped).",
      url: "https://docs.astral.sh/ruff/",
    },
  ];

  const SERVICES = [
    {
      name: "OpenStreetMap",
      license: "ODbL 1.0",
      role: "Building footprints and heights for the 3D map. © OpenStreetMap contributors.",
      url: "https://www.openstreetmap.org/copyright",
    },
    {
      name: "Overpass API",
      license: "Service of the OpenStreetMap community",
      role: "Serves the OpenStreetMap building data the backend requests and caches.",
      url: "https://overpass-api.de/",
    },
    {
      name: "Nominatim",
      license: "ODbL 1.0 (OpenStreetMap data)",
      role: "Place search on the map. Search text is sent to the public Nominatim service, so it needs a connection. © OpenStreetMap contributors.",
      url: "https://nominatim.org/",
    },
    {
      name: "PX4 Autopilot releases",
      license: "BSD 3-Clause",
      role: "Firmware images listed and downloaded from GitHub releases when you flash PX4. Nothing is bundled.",
      url: "https://px4.io/",
    },
    {
      name: "ArduPilot firmware server",
      license: "GPL v3",
      role: "Firmware images and board lists fetched when you flash ArduPilot. Nothing is bundled.",
      url: "https://firmware.ardupilot.org/",
    },
    {
      name: "GitHub",
      license: "Public API, no account",
      role: "The update check reads the list of published Corvus GCS releases. Nothing is " +
            "downloaded and nothing about the computer is sent. It can be switched off in Settings.",
      url: "https://github.com/M-BSquared/CorvusGCS/releases",
    },
  ];

  // Nothing here is shipped or imported. These are the sources a part of
  // Corvus follows closely enough that its byte values or its design come
  // from them, so they are credited even though no code of theirs runs.
  const REFERENCES = [
    {
      name: "PX4 Bootloader and px_uploader.py",
      license: "BSD 3-Clause",
      role: "The USB bootloader protocol and its CRC, which the firmware uploader speaks. " +
            "One board exception comes from ArduPilot's uploader.py.",
      url: "https://github.com/PX4/PX4-Bootloader",
    },
    {
      name: "PX4 GPS drivers",
      license: "BSD 3-Clause",
      role: "The u-blox configuration keys and survey-in steps behind the RTK base station setup.",
      url: "https://github.com/PX4/PX4-GPSDrivers",
    },
    {
      name: "PX4 Flight Review",
      license: "BSD 3-Clause",
      role: "The model for the Flight Review page, which is a smaller implementation of its own.",
      url: "https://github.com/PX4/flight_review",
    },
    {
      name: "pyulog",
      license: "BSD 3-Clause",
      role: "The reference the built-in ULog reader was checked against.",
      url: "https://github.com/PX4/pyulog",
    },
    {
      name: "QGroundControl",
      license: "Apache 2.0 or GPL v3",
      role: "The .params file format, the MAVLink shell framing and the second station " +
            "defaults that Corvus stays compatible with.",
      url: "https://qgroundcontrol.com/",
    },
  ];

  const PROJECT = {
    author: "Maximilian Böck",
    // Always "with", never "at": the institution is a partner in this work,
    // not the place the software is attributed to.
    org: "with Universität der Bundeswehr München",
    // Same sentence as the README and the website footer.
    chair: "In the context of the work at Chair LRT 1.1 of Prof. Dr. Matthias Gerdts.",
    note: "Corvus GCS is designed, built and maintained by Maximilian Böck. " +
          "Built for real field use with PX4 and ArduPilot aircraft.",
    // This panel names the licence of every component it lists, so leaving the
    // product's own licence off it was the one gap. The file it points at ships
    // inside the bundle, so this is a claim the operator can actually check.
    license: "Sustainable Use License 1.0. See LICENSE.md in the application "
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

    if (e.copyright) {
      const cp = document.createElement("span");
      cp.className = "credits-entry-role";
      cp.textContent = e.copyright;
      row.appendChild(cp);
    }
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

  function pendingGroup(title, text) {
    const sec = document.createElement("div");
    sec.className = "credits-group";
    sec.appendChild(Corvus.ui.label(title));
    const pending = Corvus.ui.empty(text);
    sec.appendChild(pending);
    return { sec, pending };
  }

  /**
   * Map-service and elevation credits, from the live tile registry.
   * Attribution strings are legally required and belong to
   * corvus/tile_sources.py; this only groups the map layers by service and
   * de-duplicates (Esri Satellite and Hybrid share one).
   *
   * Elevation models travel under their own `terrain` key, apart from the
   * base layers, so they need their own pass: reading `sources` alone left
   * every elevation credit off this dialog, Copernicus's required notice
   * among them.
   */
  function mapDataGroups() {
    const maps = pendingGroup("Map data", "Loading map attributions…");
    const dems = pendingGroup("Elevation data", "Loading elevation attributions…");

    Corvus.telemetry.requestJson("/api/tiles/sources").catch(() => null).then((data) => {
      const sources = (data && data.sources) || [];
      const providers = (data && data.providers) || [];
      const terrain = ((data && data.terrain) || []).filter((t) => t.attribution);

      if (sources.length) {
        maps.sec.removeChild(maps.pending);
        providers.forEach((p) => {
          // One service can serve several layers under the same credit line;
          // list each distinct attribution once.
          const seen = [];
          sources.filter((s) => s.provider === p.id).forEach((s) => {
            if (s.attribution && seen.indexOf(s.attribution) === -1) seen.push(s.attribution);
          });
          if (!seen.length) return;
          maps.sec.appendChild(entryRow({
            name: p.label,
            license: seen.length === 1 ? "" : `${seen.length} sources`,
            role: seen.join(" · "),
          }));
        });
      } else {
        maps.pending.textContent = "Map attributions unavailable. The backend did not respond.";
      }

      if (terrain.length) {
        dems.sec.removeChild(dems.pending);
        terrain.forEach((t) => {
          dems.sec.appendChild(entryRow({ name: t.label, license: "", role: t.attribution }));
        });
      } else {
        dems.pending.textContent = "Elevation attributions unavailable. The backend did not respond.";
      }
    });

    return [maps.sec, dems.sec];
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
    const chair = document.createElement("div");
    chair.className = "credits-org";
    chair.textContent = PROJECT.chair;
    intro.appendChild(chair);

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
    frag.appendChild(group("Data & services", SERVICES));
    mapDataGroups().forEach((sec) => frag.appendChild(sec));
    frag.appendChild(group("Reference implementations", REFERENCES));

    const footer = document.createElement("div");
    footer.className = "credits-footer";
    footer.textContent =
      "Every library and font above is vendored under src/vendor/ and loaded " +
      "from disk. The interface requests nothing from the internet. Map tiles " +
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

  return { open, LIBRARIES, FONTS, RUNTIME, SERVICES, REFERENCES, PROJECT };
})();
