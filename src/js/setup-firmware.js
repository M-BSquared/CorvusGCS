"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupFirmware — the Firmware sub-page of the Setup page.
 *
 * PX4 firmware flash over a DIRECT USB CONNECTION ONLY. The backend refuses to
 * flash over SiK radio / UDP / TCP: the gate is `can_flash` from
 * /api/firmware/status, which is true only when transport=="usb" AND the
 * vehicle is idle AND not armed. The frontend mirrors that gate (defensive:
 * even if the status snapshot is stale, the live telemetry armed flag still
 * disables Upload), and surfaces the reason flashing is blocked via two
 * banners: the USB-only gate banner and the armed banner.
 *
 * Backend contract (drives the UI; do not flash from the frontend's own logic):
 *   GET  /api/firmware/status     {state,transport,device,can_flash,armed,
 *                                  progress,message}
 *   GET  /api/firmware/catalog[?refresh=1]
 *                                  {releases:[{tag,name,prerelease,boards:[
 *                                   {name,label,size,cached,
 *                                    vendor,board,variant,title}]}],
 *                                   cached,error,dir}
 *                                  vendor/variant/title are derived from the
 *                                  `<vendor>_<board>_<variant>.px4` target
 *                                  name by the backend; the vendor is the
 *                                  left dropdown, the title is the right one,
 *                                  and the variant decides what the developer
 *                                  builds switch hides.
 *   POST /api/firmware/flash       {release,board} -> downloads then flashes
 *   POST /api/firmware/upload     raw .px4/.apj/.bin body
 *                                  (Content-Type: application/octet-stream),
 *                                  ?name=<filename> -> {ok,state} | {ok:false,error}
 *   GET  /api/firmware/progress    SSE: progress {state,percent,message} + ping
 *   POST /api/firmware/cancel      {ok} | {ok:false,error}
 *
 * Telemetry is pushed (SSE); the only one-shot fetches are the config-style
 * /api/firmware/* calls. The page re-fetches /api/firmware/status only when the
 * telemetry `connected`/`armed` signature changes (no refetch spam) and after
 * every terminal SSE event (authoritative state).
 *
 * Firmware selection: the operator picks a PX4 release and their board and the
 * backend downloads the matching image, caching it under `firmware_dir` so the
 * same flash works offline next time. The request names a release and a board,
 * never a URL — resolving the download target is the backend's job. The manual
 * file picker stays as the offline path and for custom builds.
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the view lifecycle: it calls destroy() on back / left-nav re-entry so
 * the firmware-progress SSE, the in-flight upload fetch (AbortController), and
 * the telemetry subscription are all released. No zombies, no leaked sockets.
 */
Corvus.setupFirmware = (function () {
  const S = Corvus.setupShared;

  /** Map the backend transport code to a human label for the Connection card. */
  function transportLabel(t) {
    switch (t) {
      case "usb": return "USB (direct)";
      case "sik": return "SiK Radio";
      case "udp": return "UDP";
      case "tcp": return "TCP";
      default:    return "Unknown";
    }
  }

  function render(container, navigateBack) {
    const page = S.el("div", "setup-page");
    // The back button only asks the orchestrator to navigate back; the
    // orchestrator's teardown() is the single place that calls destroy().
    page.appendChild(S.backButton(navigateBack));
    page.appendChild(S.pageHeader("Firmware Flash",
      "Flash PX4 firmware over a direct USB connection only"));

    // --- Connection card --------------------------------------------------
    const linkSection = S.el("div", "page-section");
    const linkCard = S.el("div", "page-card firmware-link-card");
    linkCard.appendChild(S.sectionTitle("Connection"));

    const transportRow = S.infoRow("Transport", "—");
    const deviceRow = S.infoRow("Device", "—");
    linkCard.appendChild(transportRow);
    linkCard.appendChild(deviceRow);

    // Gate banner: shown when flashing is not allowed. Reuses .params-banner
    // (warning style) so the operator immediately sees WHY the gate is closed.
    const gateBanner = S.el("div", "params-banner");
    gateBanner.hidden = true;
    linkCard.appendChild(gateBanner);

    // Armed banner: separate from the USB gate so the operator sees the exact
    // reason flashing is blocked (the backend's can_flash already encodes armed,
    // but the live telemetry armed flag is the authoritative real-time signal).
    const armedBanner = S.el("div", "params-banner");
    armedBanner.hidden = true;
    armedBanner.textContent = "Cannot flash while armed — disarm first.";
    linkCard.appendChild(armedBanner);

    linkSection.appendChild(linkCard);
    page.appendChild(linkSection);

    // --- Firmware card ----------------------------------------------------
    const fwSection = S.el("div", "page-section");
    const fwCard = S.el("div", "page-card firmware-card");
    fwCard.appendChild(S.sectionTitle("Firmware"));

    const note = S.el("div", "params-desc");
    note.textContent =
      "Pick a PX4 release and your board and Corvus downloads the image for you, " +
      "or select a local file. Flashing reboots the autopilot into its bootloader " +
      "and uploads over USB. Keep the USB cable connected.";
    fwCard.appendChild(note);

    // Source switch. Download is the default because it is the path that needs
    // no prior preparation; the file picker is what a custom build or a laptop
    // that has never been online needs.
    const sourceRow = S.el("div", "firmware-source");
    const sourceBtns = [
      { id: "catalog", label: "PX4 release", icon: "cloud-download" },
      { id: "file", label: "Local file", icon: "folder-open" },
    ].map((opt) => {
      const b = Corvus.ui.button({
        variant: "secondary", size: "sm", icon: opt.icon, label: opt.label,
        className: "firmware-source-btn",
        onClick: () => setSource(opt.id),
      });
      b.dataset.source = opt.id;
      sourceRow.appendChild(b);
      return b;
    });
    fwCard.appendChild(sourceRow);

    // --- Catalogue picker -------------------------------------------------
    const catalogBox = S.el("div", "firmware-catalog");

    const releaseField = Corvus.ui.select({
      ariaLabel: "PX4 release",
      options: [{ value: "", label: "Loading releases…" }],
      disabled: true,
    });
    // The release list is fetched once and then served from disk forever, so
    // without this nothing in the app ever asks GitHub again: a laptop that
    // was offline the first time it opened this page stayed empty, and a
    // machine that has the list never saw a new PX4 release. It is the only
    // control here that needs the network on purpose.
    const refreshBtn = Corvus.ui.button({
      variant: "secondary", size: "sm", icon: "refresh-cw",
      className: "firmware-refresh", label: "Refresh",
      onClick: () => loadCatalog(true),
    });
    const releaseRow = S.el("div", "firmware-release-row");
    releaseRow.appendChild(releaseField);
    releaseRow.appendChild(refreshBtn);
    catalogBox.appendChild(Corvus.ui.field({
      label: "Release", control: releaseRow,
    }));

    // Two dropdowns, not one list of 150 rows. A target name is
    // `<vendor>_<board>_<variant>`, and the operator knows the vendor half of
    // it before they open the page — it is printed on the board in their hand.
    // So they answer that first, and the second dropdown is then the handful
    // of boards that manufacturer makes rather than the whole release. Neither
    // list is a scroll, neither needs a search box, and the pair reads as the
    // one question it is: which board is this.
    const vendorField = Corvus.ui.select({
      className: "firmware-vendor-select",
      ariaLabel: "Board manufacturer",
      options: [{ value: "", label: "Loading manufacturers…" }],
      disabled: true,
    });
    const boardField = Corvus.ui.select({
      className: "firmware-board-select",
      ariaLabel: "Flight controller board",
      options: [{ value: "", label: "Pick a manufacturer first" }],
      disabled: true,
    });
    // Developer builds are off by default, not hidden: they are half the
    // targets and none of them are what an operator flashing their aircraft
    // wants, but someone who came for `_rover` has to be able to reach it.
    const variantBtn = Corvus.ui.button({
      variant: "secondary", size: "sm",
      className: "firmware-variant-btn", label: "Developer builds",
      onClick: () => {
        state.showVariants = !state.showVariants;
        variantBtn.classList.toggle("active", state.showVariants);
        variantBtn.setAttribute("aria-pressed", state.showVariants ? "true" : "false");
        renderBoards();
      },
    });
    variantBtn.setAttribute("aria-pressed", "false");
    const boardRow = S.el("div", "firmware-board-row");
    boardRow.appendChild(vendorField);
    boardRow.appendChild(boardField);
    boardRow.appendChild(variantBtn);

    // What a dropdown costs and this line buys back: a closed <select> shows
    // one label, so everything the old rows carried — the exact file, its
    // size, whether it is already downloaded, whether it is an autopilot at
    // all — lives here, under the pair, about the board actually selected.
    const boardDetail = S.el("div", "firmware-board-detail");
    boardDetail.hidden = true;
    const boardCount = S.el("div", "firmware-board-count");

    const boardBox = S.el("div", "firmware-board");
    boardBox.appendChild(boardRow);
    boardBox.appendChild(boardDetail);
    boardBox.appendChild(boardCount);
    catalogBox.appendChild(Corvus.ui.field({
      label: "Board", control: boardBox,
      hint: "PX4 ships one image per flight-controller target — pick your "
        + "manufacturer, then the board it made.",
    }));

    const detectedNote = S.el("div", "firmware-detected");
    detectedNote.hidden = true;
    catalogBox.appendChild(detectedNote);

    const catalogNote = S.el("div", "firmware-catalog-note");
    catalogBox.appendChild(catalogNote);
    fwCard.appendChild(catalogBox);

    // File row: native file input + filename display.
    const fileRow = S.el("div", "firmware-file-row");
    const fileInput = document.createElement("input");
    fileInput.type = "file";
    // .apj is ArduPilot's extension for the same container PX4 calls .px4 —
    // JSON, base64, zlib — so the uploader takes one without a change.
    fileInput.accept = ".px4,.apj,.bin";
    fileInput.className = "field-input firmware-file-input";
    fileInput.setAttribute("aria-label", "Select a firmware file");
    const filename = S.el("span", "firmware-filename", "No file selected");
    fileRow.appendChild(fileInput);
    fileRow.appendChild(filename);
    fwCard.appendChild(fileRow);

    // Action row: Upload (primary) + Cancel (secondary, only while flashing).
    const actionRow = S.el("div", "firmware-file-row");
    const uploadBtn = Corvus.ui.button({
      variant: "primary",
      className: "params-download-btn",
      icon: "upload-cloud",
      label: "Flash Firmware",
      disabled: true,
    });

    // Corvus.ui.button wraps the label in an unclassed <span> (the icon is an
    // <i>/<svg>), so this is the label node — captured once rather than
    // re-queried on every gate recompute.
    const uploadLabel = uploadBtn.querySelector("span");

    const cancelBtn = Corvus.ui.button({
      variant: "secondary",
      size: "sm",
      className: "firmware-cancel",
      icon: "x",
      label: "Cancel",
    });
    cancelBtn.hidden = true;

    actionRow.appendChild(uploadBtn);
    actionRow.appendChild(cancelBtn);
    fwCard.appendChild(actionRow);

    // Status line (last message) + progress bar + flash log.
    const status = S.el("div", "params-status");
    status.hidden = true;
    fwCard.appendChild(status);

    const barHost = S.el("div", "progress-host");
    const bar = S.el("div", "progress-bar");
    const fill = S.el("div", "progress-bar-fill");
    bar.appendChild(fill);
    barHost.appendChild(bar);
    const label = S.el("div", "progress-label", "");
    barHost.appendChild(label);
    fwCard.appendChild(barHost);

    const logTitle = S.el("div", "guidance-title");
    logTitle.appendChild(S.icon("info"));
    logTitle.appendChild(S.el("span", null, "Flash log"));
    fwCard.appendChild(logTitle);

    const logList = S.el("div", "guidance-list");
    logList.setAttribute("role", "log");
    logList.setAttribute("aria-live", "polite");
    fwCard.appendChild(logList);

    fwSection.appendChild(fwCard);
    page.appendChild(fwSection);

    container.appendChild(page);

    // --- State ------------------------------------------------------------
    const state = {
      status: null,         // last /api/firmware/status payload
      eventSource: null,    // unsubscribe from the shared "firmware" topic
      unsub: null,          // telemetry subscription
      abort: null,          // AbortController for the in-flight upload
      file: null,           // the selected File
      source: "catalog",    // "catalog" (download) | "file" (local .px4/.apj/.bin)
      catalog: null,        // last /api/firmware/catalog payload
      releases: [],         // releases from the catalogue
      boards: [],           // boards of the selected release (unfiltered)
      vendor: "",           // manufacturer shown in the left dropdown ("" = none)
      board: "",            // target name of the selected board ("" = none)
      showVariants: false,  // include PX4's non-default developer builds
      detected: null,       // {name,label,source} board the backend recognised
      boardTouched: false,  // true once the operator picked a board themselves
      // Telemetry signature for refetch gating (avoid /api/firmware/status spam).
      lastConnected: null,
      lastArmed: null,
    };

    /** Append a line to the flash log (capped ~50 like the guidance list). */
    function appendLog(message, level) {
      if (!message) return;
      const line = S.el("div", "guidance-line" + (level ? " " + level : ""));
      line.appendChild(S.el("span", "guidance-msg", String(message)));
      logList.appendChild(line);
      while (logList.children.length > 50) {
        logList.removeChild(logList.firstChild);
      }
      if (logList.scrollTop !== undefined) logList.scrollTop = logList.scrollHeight;
    }

    function notify(level, message) {
      window.dispatchEvent(new CustomEvent("corvus:notification",
        { detail: { level, message } }));
    }

    function isArmedFromTelemetry() {
      const t = Corvus.telemetry && Corvus.telemetry.getState();
      return !!(t && t.armed);
    }

    /**
     * Recompute the Upload button disabled state from the live gates. Upload
     * is enabled ONLY when the backend says can_flash AND the status state is
     * idle AND a file is selected AND the vehicle is not armed (telemetry is
     * the authoritative real-time armed signal; the status snapshot is a fallback).
     */
    function recomputeUploadGate() {
      const s = state.status || {};
      const armed = !!s.armed || isArmedFromTelemetry();
      const busy = s.state === "downloading" || s.state === "flashing";
      const canFlash = !!s.can_flash && !busy && !armed;
      const haveSource = state.source === "file"
        ? !!state.file
        : !!(releaseField.value && state.board);
      uploadBtn.disabled = !(canFlash && haveSource);
      // The button says what it will actually do — a catalogue flash downloads
      // first, and a button labelled "Flash" that spends two minutes fetching
      // reads as a hang.
      if (uploadLabel) {
        uploadLabel.textContent =
          state.source === "file" ? "Flash Firmware" : "Download & Flash";
      }
    }

    /** Switch between the catalogue picker and the local file picker. */
    function setSource(id) {
      state.source = id === "file" ? "file" : "catalog";
      sourceBtns.forEach((b) => {
        b.classList.toggle("active", b.dataset.source === state.source);
        b.setAttribute("aria-pressed", b.dataset.source === state.source ? "true" : "false");
      });
      catalogBox.hidden = state.source !== "catalog";
      fileRow.hidden = state.source !== "file";
      if (state.source === "catalog" && !state.catalog) loadCatalog(false);
      recomputeUploadGate();
    }

    function formatSize(bytes) {
      const kb = Number(bytes) / 1024;
      if (!isFinite(kb) || kb <= 0) return "";
      return kb >= 1024 ? (kb / 1024).toFixed(1) + " MB" : Math.round(kb) + " KB";
    }

    /** The boards worth offering: developer builds only on request, and
     *  whatever is selected right now stays reachable whatever else changes —
     *  a picker that silently drops what the Flash button is about to write to
     *  the aircraft is the one thing this control must never do. */
    function visibleBoards() {
      return state.boards.filter((b) => {
        if (b.name === state.board) return true;
        return state.showVariants || (b.variant || "default") === "default";
      }).sort(compareBoards);
    }

    /** A board with no vendor in its target name still has to live somewhere. */
    function vendorOf(b) {
      return (b && b.vendor) || "Other";
    }

    /** The manufacturers on offer, in the order compareBoards already puts
     *  their boards in: the detected board's vendor, then PX4, then the rest
     *  alphabetically. */
    function visibleVendors() {
      const seen = [];
      visibleBoards().forEach((b) => {
        const v = vendorOf(b);
        if (seen.indexOf(v) < 0) seen.push(v);
      });
      return seen;
    }

    function boardsOfVendor(vendor) {
      return visibleBoards().filter((b) => vendorOf(b) === vendor);
    }

    /** Display order. The backend sorts named targets to the front, which is
     *  right for a flat list and wrong for a grouped one. Here a vendor is one
     *  block: the detected board's vendor leads, then PX4's own reference
     *  boards, then the rest alphabetically. */
    function compareBoards(a, b) {
      const av = a.vendor || "", bv = b.vendor || "";
      if (av !== bv) return vendorRank(av) - vendorRank(bv) || av.localeCompare(bv);
      // Inside a vendor: autopilots before peripherals, the plain build
      // before its developer variants, then by name.
      if (!!a.peripheral !== !!b.peripheral) return a.peripheral ? 1 : -1;
      const ad = (a.variant || "default") === "default" ? 0 : 1;
      const bd = (b.variant || "default") === "default" ? 0 : 1;
      if (ad !== bd) return ad - bd;
      return String(a.title || a.name).localeCompare(String(b.title || b.name));
    }

    /** 0 = the vendor of the board we think is plugged in, 1 = PX4, 2 = rest. */
    function vendorRank(vendor) {
      const detected = state.detected
        && (state.boards.find((b) => b.name === state.detected.name) || {}).vendor;
      if (detected && vendor === detected) return 0;
      return vendor === "PX4" ? 1 : 2;
    }

    /** One option in the board dropdown. The two things that decide whether a
     *  target is the right one to flash — that it is a developer build, that
     *  it is not an autopilot at all — are said in the label itself, because a
     *  closed <select> shows the label and nothing else. */
    function boardOptionLabel(b) {
      let label = b.title || b.label || b.name;
      if ((b.variant || "default") !== "default") label += "  ·  " + b.variant;
      if (b.peripheral) label += "  ·  peripheral";
      if (b.cached) label += "  ·  downloaded";
      return label;
    }

    /** Fill the manufacturer dropdown, then the board one under it. */
    function renderVendors() {
      if (!state.boards.length) {
        Corvus.ui.setOptions(vendorField,
          [{ value: "", label: "Pick a release first" }], "");
        vendorField.disabled = true;
        state.vendor = "";
      } else {
        const vendors = visibleVendors();
        const wanted = vendors.indexOf(state.vendor) >= 0 ? state.vendor : "";
        Corvus.ui.setOptions(vendorField,
          [{ value: "", label: "Select manufacturer…" }].concat(vendors.map((v) => ({
            value: v, label: v + "  (" + boardsOfVendor(v).length + ")",
          }))), wanted);
        vendorField.disabled = false;
        state.vendor = vendorField.value;
      }
      renderBoardOptions();
    }

    /** Fill the board dropdown from the chosen manufacturer.
     *
     *  A <select> adopts its first option the moment it is filled, so the
     *  first option is a placeholder and never a board: the old single
     *  dropdown armed "Download & Flash" with whatever sorted first — on
     *  v1.17.0 the PX4 IO coprocessor image — invisibly, because a dropdown
     *  shows one row. Nothing here is flashable until somebody picks it. */
    function renderBoardOptions() {
      if (!state.vendor) {
        Corvus.ui.setOptions(boardField, [{
          value: "",
          label: state.boards.length
            ? "Pick a manufacturer first"
            : "No boards to choose from",
        }], "");
        boardField.disabled = true;
      } else {
        const list = boardsOfVendor(state.vendor);
        const wanted = list.some((b) => b.name === state.board) ? state.board : "";
        Corvus.ui.setOptions(boardField,
          [{ value: "", label: "Select board…" }].concat(list.map((b) => ({
            value: b.name, label: boardOptionLabel(b),
          }))), wanted);
        boardField.disabled = false;
      }
      selectBoard(boardField.value);
      renderBoardCount();
    }

    /** Rebuild both dropdowns from the selected release.
     *
     *  Only ever land on a board somebody chose: the operator's own pick, or
     *  the one detected on the USB port. Anything else and the release
     *  dropdown quietly re-aims the Flash button. */
    function renderBoards() {
      const wanted = preferredBoard();
      const match = wanted && visibleBoards().find((b) => b.name === wanted);
      state.board = match ? match.name : "";
      if (match) state.vendor = vendorOf(match);
      renderVendors();
    }

    /** What the selected image actually is: the exact file, its size, and the
     *  two warnings a label alone cannot carry. */
    function renderBoardDetail() {
      const b = state.board
        && state.boards.find((x) => x.name === state.board);
      if (!b) {
        boardDetail.hidden = true;
        Corvus.ui.clear(boardDetail);
        return;
      }
      boardDetail.hidden = false;
      Corvus.ui.clear(boardDetail);
      const meta = S.el("div", "firmware-board-meta");
      meta.appendChild(S.el("span", "firmware-board-target", b.name));
      const size = formatSize(b.size);
      if (size) meta.appendChild(S.el("span", "firmware-board-size", size));
      boardDetail.appendChild(meta);
      const chips = S.el("div", "firmware-board-chips");
      if ((b.variant || "default") !== "default") {
        chips.appendChild(S.el("span", "firmware-board-variant", b.variant));
      }
      // PX4 publishes IO, CAN-node and GNSS firmware in the same release. They
      // are legitimate downloads, but they are not the aircraft's autopilot,
      // and an unlabelled choice under a heading reading "Board" is how one
      // gets flashed onto one.
      if (b.peripheral) {
        chips.appendChild(S.el("span", "firmware-board-peripheral", "peripheral"));
      }
      // Cached images flash with no network at all, so say which ones those
      // are — that is the difference between a 3-minute wait and none.
      if (b.cached) chips.appendChild(S.el("span", "firmware-board-cached", "downloaded"));
      if (chips.children.length) boardDetail.appendChild(chips);
    }

    /** "150 targets from 24 manufacturers · 1 already downloaded" — the answer
     *  to "what can I get from here", which neither dropdown can show. */
    function renderBoardCount() {
      if (!state.boards.length) {
        boardCount.textContent = "";
        return;
      }
      const shown = visibleBoards().length;
      const vendors = visibleVendors().length;
      const downloaded = state.boards.filter((b) => b.cached).length;
      const parts = [(shown === state.boards.length
        ? state.boards.length + " targets"
        : shown + " of " + state.boards.length + " targets")
        + " from " + vendors + (vendors === 1 ? " manufacturer" : " manufacturers")];
      if (downloaded) parts.push(downloaded + " already downloaded");
      boardCount.textContent = parts.join(" \u00b7 ");
    }

    /** Adopt a board as the selection and tell the gate about it. */
    function selectBoard(name) {
      state.board = name || "";
      if (boardField.value !== state.board) boardField.value = state.board;
      renderBoardDetail();
      recomputeUploadGate();
    }

    /** Apply a release selection: swap the board list to that release's.
     *
     *  A build target keeps its name across releases, so a board the operator
     *  chose (or that was detected) survives a release change. */
    function selectRelease(tag) {
      const release = state.releases.find((r) => r.tag === tag) || state.releases[0];
      state.boards = (release && release.boards) || [];
      renderBoards();
    }

    /** Board the picker should land on: the operator's, else the detected one. */
    function preferredBoard() {
      if (state.boardTouched && state.board) return state.board;
      if (state.detected && state.detected.name) return state.detected.name;
      return state.board;
    }

    /** Show what the backend recognised on the USB port, and say how. */
    function renderDetected() {
      const d = state.detected;
      if (!d || !d.name) {
        detectedNote.hidden = true;
        detectedNote.textContent = "";
        return;
      }
      detectedNote.hidden = false;
      Corvus.ui.clear(detectedNote);
      const icon = S.icon("circle-check");
      detectedNote.appendChild(icon);
      detectedNote.appendChild(S.el("span", null,
        "Detected " + (d.label || d.name) + " on the USB port"
        + (d.source ? " (" + d.source + ")" : "")
        + ". Change it below if that is not your board."));
      S.refreshIcons();
    }

    /**
     * Load the firmware catalogue. `refresh` forces the backend to go to the
     * network; without it the backend answers from its own cache, so opening
     * this page offline is instant and silent.
     */
    function loadCatalog(refresh) {
      catalogNote.textContent = refresh ? "Checking for PX4 releases…" : "Loading releases…";
      catalogNote.classList.remove("err");
      Corvus.ui.setBusy(refreshBtn, true);
      const url = "/api/firmware/catalog" + (refresh ? "?refresh=1" : "");
      return Corvus.telemetry.requestJson(url).then((data) => {
        state.catalog = data || {};
        state.releases = (data && data.releases) || [];
        state.detected = (data && data.detected) || null;
        renderDetected();
        if (!state.releases.length) {
          Corvus.ui.setOptions(releaseField, [{ value: "", label: "No releases available" }]);
          releaseField.disabled = true;
          state.boards = [];
          state.board = "";
          state.vendor = "";
          renderBoards();
          // Say what to do about it. The list lives on GitHub and this is the
          // one page that fetches it, so "press Refresh once you have
          // internet" is the whole recovery — and the local-file path is still
          // open to a laptop that will never have any.
          catalogNote.textContent = ((data && data.error)
            ? data.error + " — "
            : "No PX4 releases downloaded yet — ")
            + "press Refresh once this machine is online, or flash a local "
            + ".px4 file instead.";
          catalogNote.classList.add("err");
          return;
        }
        releaseField.disabled = false;
        // Default to the newest STABLE release, decided BEFORE the options are
        // written: a select adopts its first option the moment it is filled, and
        // PX4's newest tag is usually a beta. A pre-release is a deliberate
        // choice, never the one an operator lands on by not choosing.
        const known = state.releases.some((r) => r.tag === releaseField.value);
        const stable = state.releases.find((r) => !r.prerelease) || state.releases[0];
        const wanted = known ? releaseField.value : stable.tag;
        // The label is the release's own name when it has one, not its tag:
        // PX4 tags read "v1.17.0", but an ArduPilot "tag" is a path
        // ("ardupilot:Copter/stable") that exists to route the flash request
        // and was never meant to be read by anyone.
        Corvus.ui.setOptions(releaseField, state.releases.map((r) => ({
          value: r.tag,
          label: (r.vendor === "ardupilot" ? (r.name || r.tag) : r.tag)
            + (r.prerelease ? "  ·  pre-release" : ""),
        })), wanted);
        selectRelease(releaseField.value);
        catalogNote.textContent = data.error
          ? data.error + " — showing the cached release list."
          : "Images are cached in " + (data.dir || "the firmware folder")
            + ", so a repeat flash needs no network.";
        catalogNote.classList.toggle("err", !!data.error);
      }).catch((err) => {
        catalogNote.textContent = (err && err.message) || "Could not load the release list";
        catalogNote.classList.add("err");
        recomputeUploadGate();
      }).finally(() => {
        Corvus.ui.setBusy(refreshBtn, false);
      });
    }

    releaseField.addEventListener("change", () => selectRelease(releaseField.value));
    // Changing manufacturer drops the board: the one selected belongs to the
    // vendor that is no longer shown, and a Flash button still pointed at it
    // is aimed at something the operator can no longer see.
    vendorField.addEventListener("change", () => {
      state.vendor = vendorField.value;
      state.board = "";
      renderBoardOptions();
    });
    boardField.addEventListener("change", () => {
      // Once the operator picks, detection stops moving the selection under them.
      state.boardTouched = true;
      selectBoard(boardField.value);
    });

    /** Replace the .page-row-value child of an infoRow element with new text. */
    function setRowValue(row, text) {
      const v = row.querySelector(".page-row-value");
      if (v) v.textContent = String(text);
    }

    /**
     * Apply a /api/firmware/status payload: transport/device rows, gate banner,
     * armed banner, button gates, progress bar, status line. Fires a terminal
     * notification on done/failed/cancelled.
     */
    function applyStatus(s) {
      state.status = s || {};
      const st = state.status;
      const transport = st.transport || "unknown";
      // OR the backend snapshot with the live telemetry flag: if EITHER source
      // says armed, treat as armed (defensive — never flash while armed).
      const armed = !!st.armed || isArmedFromTelemetry();
      const canFlash = !!st.can_flash;

      setRowValue(transportRow, transportLabel(transport));
      setRowValue(deviceRow, st.device || "—");

      // Gate banner: shown when not can_flash. Append the current transport so
      // the operator sees exactly why flashing is blocked right now.
      if (!canFlash) {
        gateBanner.hidden = false;
        gateBanner.textContent =
          "Firmware flash requires a direct USB connection to the flight controller. " +
          "SiK radio, UDP, and TCP cannot be used for flashing. " +
          "(Current transport: " + transportLabel(transport) + ".)";
      } else {
        gateBanner.hidden = true;
      }

      // Armed banner (live telemetry is the authoritative signal; applyStatus
      // re-runs on every telemetry frame via onTelemetry).
      armedBanner.hidden = !armed;

      // Progress + label from the status snapshot.
      const pct = Math.max(0, Math.min(100, Math.round(st.progress || 0)));
      fill.style.width = pct + "%";
      if (st.state === "flashing" || st.state === "downloading") {
        label.textContent = pct + "% · " + (st.message
          || (st.state === "downloading" ? "Downloading…" : "Flashing…"));
      } else if (st.message) {
        label.textContent = st.message;
      } else {
        label.textContent = "";
      }

      // Status line mirrors the backend message; err style on failure.
      if (st.message) {
        status.hidden = false;
        status.textContent = st.message;
        status.classList.toggle("err", st.state === "failed");
      } else {
        status.hidden = true;
        status.textContent = "";
        status.classList.remove("err");
      }

      // Cancel while the service is busy — a download is just as cancellable
      // as the flash it precedes, and it is the longer of the two.
      cancelBtn.hidden = st.state !== "flashing" && st.state !== "downloading";

      // Terminal states fire a notification (one per applyStatus call; the
      // notification dedupe layer collapses near-duplicate repeats).
      if (st.state === "done") {
        notify("info", st.message || "Firmware flash complete");
      } else if (st.state === "failed") {
        notify("critical", st.message || "Firmware flash failed");
      } else if (st.state === "cancelled") {
        notify("warning", st.message || "Firmware flash cancelled");
      }

      recomputeUploadGate();
    }

    /** Re-fetch /api/firmware/status and apply. */
    function refreshStatus() {
      if (!Corvus.telemetry || typeof Corvus.telemetry.requestJson !== "function") return;
      Corvus.telemetry.requestJson("/api/firmware/status")
        .then(applyStatus)
        .catch((err) => {
          // Keep the page responsive even if the status fetch fails — surface
          // the error in the status line and leave the gate closed.
          status.hidden = false;
          status.textContent = (err && err.message) || "Could not load firmware status";
          status.classList.add("err");
          uploadBtn.disabled = true;
        });
    }

    // --- Wire telemetry: refetch on connected/armed change, re-gate live -
    function onTelemetry(t) {
      if (!t) return;
      const connected = !!t.connected;
      const armed = !!t.armed;
      // Re-apply the armed banner + Upload gate immediately (the live armed
      // flag must not wait for a status refetch — the operator arms/disarms
      // and the UI must reflect it on the same telemetry frame).
      armedBanner.hidden = !armed;
      recomputeUploadGate();
      // Re-fetch /api/firmware/status only when the link/armed signature
      // changes (avoids spamming the endpoint on every telemetry frame — the
      // transport only changes when the connection changes).
      if (connected !== state.lastConnected || armed !== state.lastArmed) {
        state.lastConnected = connected;
        state.lastArmed = armed;
        refreshStatus();
      }
    }

    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      state.unsub = Corvus.telemetry.subscribe(onTelemetry);
    }

    // Initial status fetch — the gate is driven by the backend (the only place
    // that knows the transport). Telemetry's armed flag is overlaid live above.
    refreshStatus();
    // Catalogue from the backend's cache (no network unless the operator asks
    // for a refresh), then show the download picker.
    setSource("catalog");

    // --- File picker ------------------------------------------------------
    fileInput.addEventListener("change", () => {
      const f = fileInput.files && fileInput.files[0];
      state.file = f || null;
      filename.textContent = f ? f.name : "No file selected";
      recomputeUploadGate();
    });

    // --- Upload (raw binary POST, not postAction which JSON-encodes) --------
    async function uploadFirmware(file) {
      const url = "/api/firmware/upload" +
        (file.name ? "?name=" + encodeURIComponent(file.name) : "");
      state.abort = new AbortController();
      let res;
      try {
        res = await fetch(url, {
          method: "POST",
          headers: { "Content-Type": "application/octet-stream" },
          body: file,
          signal: state.abort.signal,
        });
      } catch (err) {
        if (err && err.name === "AbortError") throw new Error("Upload cancelled");
        throw new Error((err && err.message) || "Network request failed");
      } finally {
        state.abort = null;
      }
      let data;
      try { data = await res.json(); }
      catch (_e) { throw new Error("Invalid response from " + url); }
      if (!res.ok || !data.ok) {
        throw new Error(data.error || ("Upload rejected (" + res.status + ")"));
      }
      return data;
    }

    async function onUpload() {
      if (state.source === "file") return onUploadFile();
      return onFlashRelease();
    }

    async function onUploadFile() {
      if (!state.file) return;
      uploadBtn.disabled = true;
      status.hidden = false;
      status.textContent = "Uploading…";
      status.classList.remove("err");
      appendLog("Uploading " + state.file.name + "…", "info");
      try {
        await uploadFirmware(state.file);
        // Backend started flashing — show Cancel immediately and open the
        // progress SSE (the SSE is the live source for percent + messages).
        cancelBtn.hidden = false;
        status.textContent = "Flashing…";
        openProgressSse();
      } catch (err) {
        const msg = (err && err.message) || "Upload failed";
        status.textContent = msg;
        status.classList.add("err");
        appendLog(msg, "error");
        notify("critical", msg);
        // Re-enable Upload if the gate is still open (can_flash + not armed).
        recomputeUploadGate();
      }
    }

    /**
     * Download the selected release image and flash it. The request carries the
     * release tag and board name only; the backend resolves the download URL,
     * reuses a cached image when it has one, and re-checks the flash gate after
     * the download before touching the autopilot.
     */
    async function onFlashRelease() {
      const release = releaseField.value;
      const board = state.board;
      if (!release || !board) return;
      uploadBtn.disabled = true;
      status.hidden = false;
      status.classList.remove("err");
      status.textContent = "Downloading firmware…";
      appendLog("Downloading " + board + " (" + release + ")…", "info");
      try {
        await Corvus.telemetry.postAction("/api/firmware/flash", { release, board });
        cancelBtn.hidden = false;
        openProgressSse();
      } catch (err) {
        const msg = (err && err.message) || "Firmware download failed";
        status.textContent = msg;
        status.classList.add("err");
        appendLog(msg, "error");
        notify("critical", msg);
        recomputeUploadGate();
      }
    }

    function openProgressSse() {
      if (state.eventSource) {
        try { state.eventSource(); } catch (_e) {}
      }
      // The shared /api/events stream (js/events.js), not a connection of this
      // page's own — a browser allows six per origin and the map wants them
      // for tiles. Nothing else changes: the payload is the same object the
      // /api/firmware/progress endpoint sends, and a dropped stream is still
      // covered by the status refetch below.
      state.eventSource = Corvus.events.subscribe("firmware", (d) => {
        try {
          const pct = Math.max(0, Math.min(100, Math.round(d.percent || 0)));
          fill.style.width = pct + "%";
          label.textContent = pct + "% · " + (d.message || d.state || "");
          if (d.message) appendLog(d.message, d.state);
          // Drive Cancel visibility from the stream's state (the live source
          // during flashing) so the button tracks the backend precisely.
          cancelBtn.hidden = d.state !== "flashing" && d.state !== "downloading";
          // Terminal event: refetch the authoritative status so the gate +
          // banners reflect the result, and applyStatus fires the notification.
          if (d.state === "done" || d.state === "failed" || d.state === "cancelled") {
            // A finished download leaves a new cached image; re-read the
            // catalogue so the board list says so.
            if (d.state === "done" && state.source === "catalog") loadCatalog(false);
            refreshStatus();
          }
        } catch (_err) { /* keep the stream open on a malformed event */ }
      });
    }

    async function onCancel() {
      cancelBtn.disabled = true;
      try {
        await Corvus.telemetry.requestJson("/api/firmware/cancel", { method: "POST" });
        // The SSE will emit a terminal `cancelled` progress event on success;
        // no local UI update needed here.
      } catch (err) {
        appendLog((err && err.message) || "Cancel failed", "error");
      } finally {
        cancelBtn.disabled = false;
      }
    }

    uploadBtn.addEventListener("click", onUpload);
    cancelBtn.addEventListener("click", onCancel);

    // --- Teardown (idempotent + guarded) ----------------------------------
    let destroyed = false;
    function destroy() {
      if (destroyed) return;
      destroyed = true;
      if (state.unsub) { try { state.unsub(); } catch (_e) {} state.unsub = null; }
      if (state.eventSource) { try { state.eventSource(); } catch (_e) {} state.eventSource = null; }
      if (state.abort) { try { state.abort.abort(); } catch (_e) {} state.abort = null; }
    }

    return destroy;
  }

  return { render };
})();
