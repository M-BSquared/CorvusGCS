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
 *                                   {name,label,size,cached}]}],cached,error,dir}
 *   POST /api/firmware/flash       {release,board} -> downloads then flashes
 *   POST /api/firmware/upload     raw .px4/.bin body
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
    const linkCard = S.el("div", "page-card");
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
    catalogBox.appendChild(Corvus.ui.field({
      label: "Release", control: releaseField,
    }));

    const boardFilter = Corvus.ui.input({
      placeholder: "Filter boards…", ariaLabel: "Filter the board list",
    });
    const boardField = Corvus.ui.select({
      ariaLabel: "Flight controller board",
      options: [{ value: "", label: "Select a release first" }],
      disabled: true,
    });
    const boardBox = S.el("div", "firmware-board");
    boardBox.appendChild(boardFilter);
    boardBox.appendChild(boardField);
    catalogBox.appendChild(Corvus.ui.field({
      label: "Board", control: boardBox,
      hint: "PX4 ships one image per flight-controller target — pick the one your board is.",
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
    fileInput.accept = ".px4,.bin";
    fileInput.className = "field-input firmware-file-input";
    fileInput.setAttribute("aria-label", "Select PX4 firmware file");
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
      eventSource: null,    // the /api/firmware/progress SSE
      unsub: null,          // telemetry subscription
      abort: null,          // AbortController for the in-flight upload
      file: null,           // the selected File
      source: "catalog",    // "catalog" (download) | "file" (local .px4/.bin)
      catalog: null,        // last /api/firmware/catalog payload
      releases: [],         // releases from the catalogue
      boards: [],           // boards of the selected release (unfiltered)
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
        : !!(releaseField.value && boardField.value);
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

    /** Fill the board <select> from the selected release, honouring the filter. */
    function renderBoards() {
      const needle = String(boardFilter.value || "").trim().toLowerCase();
      const matches = state.boards.filter((b) =>
        !needle || b.label.toLowerCase().includes(needle)
          || b.name.toLowerCase().includes(needle));
      Corvus.ui.setOptions(boardField, matches.map((b) => ({
        value: b.name,
        // Cached images flash with no network at all, so say which ones those
        // are — that is the difference between a 3-minute wait and none.
        label: b.label + (b.cached ? "  ·  downloaded" : "") ,
      })), preferredBoard());
      boardField.disabled = matches.length === 0;
      if (!matches.length) {
        Corvus.ui.setOptions(boardField, [{ value: "", label: "No board matches the filter" }]);
      }
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
      if (state.boardTouched && boardField.value) return boardField.value;
      if (state.detected && state.detected.name) return state.detected.name;
      return boardField.value;
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
          renderBoards();
          catalogNote.textContent = (data && data.error)
            || "No PX4 releases cached yet — connect once to download the list, "
               + "or use a local file.";
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
        Corvus.ui.setOptions(releaseField, state.releases.map((r) => ({
          value: r.tag,
          label: r.tag + (r.prerelease ? "  ·  pre-release" : ""),
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
      });
    }

    releaseField.addEventListener("change", () => selectRelease(releaseField.value));
    boardField.addEventListener("change", () => {
      // Once the operator picks, detection stops moving the selection under them.
      state.boardTouched = true;
      recomputeUploadGate();
    });
    boardFilter.addEventListener("input", renderBoards);

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
      const board = boardField.value;
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
        try { state.eventSource.close(); } catch (_e) {}
      }
      try {
        state.eventSource = new EventSource("/api/firmware/progress");
        state.eventSource.addEventListener("progress", (e) => {
          try {
            const d = JSON.parse(e.data);
            const pct = Math.max(0, Math.min(100, Math.round(d.percent || 0)));
            fill.style.width = pct + "%";
            label.textContent = pct + "% · " + (d.message || d.state || "");
            if (d.message) appendLog(d.message, d.state);
            // Drive Cancel visibility from the SSE state (the live source
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
          } catch (_err) { /* keep the SSE open on a malformed event */ }
        });
        state.eventSource.addEventListener("ping", () => {});
        state.eventSource.onerror = () => { /* keep the SSE open; backend closes it */ };
      } catch (_err) { /* no EventSource — the status refetch covers terminal states */ }
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
      if (state.eventSource) { try { state.eventSource.close(); } catch (_e) {} state.eventSource = null; }
      if (state.abort) { try { state.abort.abort(); } catch (_e) {} state.abort = null; }
    }

    return destroy;
  }

  return { render };
})();
