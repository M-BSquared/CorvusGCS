"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.analysis — the Analysis page (left-nav "ANALYSIS").
 *
 * Two kinds of log, one folder:
 *
 *   ULog   PX4's own high-rate log, on the flight controller's SD card. Listed
 *          and pulled off the vehicle over the MAVLink LOG_* protocol.
 *   tlog   the MAVLink stream Corvus recorded on this laptop. Already local;
 *          listed here so one flight's evidence is findable in one place.
 *
 * The folder is picked once, in a single slim row at the top, and persisted to
 * the config — the point of the feature is that after a flight you press one
 * button, not that you re-answer "where to?" every time.
 *
 * Layout and lifecycle follow the Setup page. The landing view is live
 * telemetry, the download folder as one slim row, and two tiles; opening a tile
 * REPLACES the page rather than unfolding under it, so a log list is a place
 * you went to and Back is the way out — not a card that pushed everything else
 * off the screen.
 *
 * Downloads are **sequential**, and that is a protocol constraint, not a
 * simplification: MAVLink has one log session per vehicle, so two concurrent
 * downloads interleave their LOG_DATA and corrupt both files. Selecting five
 * logs queues five; the backend works through them and the page shows which
 * one is running and how many are left.
 *
 * Backend contract:
 *   GET  /api/logs/status    {state,message,percent,current,queued,completed,
 *                             logs,tlogs,dir,connected}
 *   POST /api/logs/refresh   ask the vehicle to enumerate its logs
 *   POST /api/logs/download  {ids:[...]} queue a sequential download
 *   GET  /api/logs/review?file=<name>
 *                            one ULog reduced to review plots + its messages
 *   POST /api/logs/erase     erase EVERY on-board log (no per-log delete exists)
 *   POST /api/logs/cancel
 *   POST /api/logs/dir       {dir} set + persist the download folder
 *
 * Exposes render(container) -> destroy(). The caller (sidenav) owns the
 * lifecycle: the poll timer and both telemetry subscriptions are released on
 * destroy.
 */
Corvus.analysis = (function () {
  const S = Corvus.setupShared;
  /** Status poll while a job runs. The log protocol has no push channel, and
   *  a dedicated SSE for a screen that is open for a minute is not worth one
   *  of the six connections this app can hold. */
  const POLL_BUSY_MS = 700;
  const POLL_IDLE_MS = 5000;

  /* A colour per flight mode, stable across plots and the timeline strip so
     the same band always means the same mode. Manual/Stabilized are warm
     (the pilot is flying), the automatic modes cool. */
  const MODE_COLORS = {
    "Manual": "#F5A742", "Stabilized": "#F5C842", "Acro": "#FF8A5B",
    "Altitude": "#7DDEFF", "Position": "#4CC9FF", "Position slow": "#63B4E8",
    "Offboard": "#B98CFF", "Mission": "#45D483", "Loiter": "#63E8A0",
    "Return": "#FF726E", "Takeoff": "#8CE0B4", "Land": "#8CD0E0",
    "Precision land": "#8CD0E0", "Descend": "#FFB36E", "Termination": "#FF514D",
    "Follow target": "#9ED483", "Orbit": "#C9A0FF", "VTOL takeoff": "#8CE0B4",
    "Rattitude": "#F5C842",
  };
  const MODE_FALLBACK = "#8A94A6";

  function modeColor(name) {
    return MODE_COLORS[name] || MODE_FALLBACK;
  }

  function formatSize(bytes) {
    const n = Number(bytes) || 0;
    if (n >= 1024 * 1024) return (n / (1024 * 1024)).toFixed(1) + " MB";
    if (n >= 1024) return Math.round(n / 1024) + " KB";
    return n + " B";
  }

  function formatUtc(seconds) {
    const n = Number(seconds) || 0;
    if (n <= 0) return "no timestamp";
    try {
      return new Date(n * 1000).toISOString().replace("T", " ").slice(0, 16) + " UTC";
    } catch (_e) {
      return "no timestamp";
    }
  }

  /* The nine values the Current Telemetry card shows, as label + formatter.
     A table rather than nine append calls, so the card can be repainted from a
     telemetry frame without restating the layout. */
  const TELEMETRY_ROWS = [
    ["Altitude AMSL", (s) => Math.round(s.altitude_amsl) + " m"],
    ["Altitude AGL", (s) => Math.round(s.altitude_agl) + " m"],
    ["Groundspeed", (s) => s.groundspeed.toFixed(1) + " m/s"],
    ["Vertical speed", (s) => s.vspeed.toFixed(1) + " m/s"],
    ["Heading", (s) => Math.round(s.heading) + "°"],
    ["Pitch", (s) => s.pitch.toFixed(1) + "°"],
    ["Roll", (s) => s.roll.toFixed(1) + "°"],
    ["Battery", (s) => s.battery_voltage.toFixed(1) + " V (" + s.battery_percent + "%)"],
    ["GPS Fix", (s) => s.gps_fix + " (" + s.gps_satellites + " sats)"],
  ];

  /**
   * Current Telemetry, live. It used to snapshot at render and then sit there —
   * a card labelled "current" showing whatever was true when the page opened.
   * Returns {el, destroy}.
   */
  function telemetryCard() {
    const card = Corvus.ui.card({});
    const values = [];
    TELEMETRY_ROWS.forEach(([label]) => {
      const r = Corvus.ui.row(label, "—");
      values.push(r.querySelector(".page-row-value"));
      card.appendChild(r);
    });
    const empty = Corvus.ui.empty("Vehicle not connected.");
    card.appendChild(empty);

    function paint(s) {
      const connected = !!(s && s.connected);
      empty.hidden = connected;
      TELEMETRY_ROWS.forEach(([, format], i) => {
        const target = values[i];
        if (!target || !target.parentNode) return;
        target.parentNode.hidden = !connected;
        if (!connected) return;
        let text = "—";
        try { text = format(s); } catch (_e) { text = "—"; }
        if (target.textContent !== text) target.textContent = text;
      });
    }

    paint(Corvus.telemetry.getState());
    const unsub = typeof Corvus.telemetry.subscribe === "function"
      ? Corvus.telemetry.subscribe(paint)
      : function () {};
    return {
      el: Corvus.ui.section({ title: "Current Telemetry", body: card }),
      destroy: unsub,
    };
  }

  function render(container) {
    const page = S.el("div", "logs-view");

    // The landing view: everything a sub-page replaces. Held as one element so
    // opening a tile is a swap, not a cascade of individual hides.
    const landing = S.el("div", "logs-landing");

    const telemetry = telemetryCard();
    landing.appendChild(telemetry.el);

    // --- Flight logs -------------------------------------------------------
    // The heading and its own underline separate the log tools from the
    // telemetry card above them; the folder row itself stays a slim setting
    // rather than a section, so it does not out-shout the logs it configures.
    landing.appendChild(S.sectionTitle("Flight logs"));

    const dirRow = S.el("div", "logs-dir-row");
    const dirIcon = S.el("span", "logs-dir-icon");
    dirIcon.appendChild(S.icon("folder"));
    dirRow.appendChild(dirIcon);
    dirRow.appendChild(S.el("span", "logs-dir-label", "Save to"));
    const dirInput = Corvus.ui.input({
      placeholder: "~/.corvus/flightlogs", ariaLabel: "Log download folder",
      className: "logs-dir-input", mono: true,
    });
    const dirBtn = Corvus.ui.button({
      variant: "secondary", size: "sm", label: "Save", onClick: () => saveDir(),
    });
    dirRow.appendChild(dirInput);
    dirRow.appendChild(dirBtn);
    landing.appendChild(dirRow);
    const dirNote = S.el("div", "logs-dir-note");
    landing.appendChild(dirNote);

    const tileGrid = S.el("div", "logs-tiles");
    landing.appendChild(tileGrid);
    page.appendChild(landing);

    // Sub-pages mount here, with the landing view hidden — a full view swap,
    // the same shape as the Setup page's tiles.
    const body = S.el("div", "logs-body");
    body.hidden = true;
    page.appendChild(body);
    container.appendChild(page);

    // --- State --------------------------------------------------------------
    const selected = new Set();
    let status = null;
    let dirTouched = false;
    let timer = null;
    let destroyed = false;
    let inFlight = false;
    let view = "tiles";        // "tiles" | "ulog" | "tlog" | "review"
    let reviewToken = 0;       // so a slow read cannot paint over a newer one
    let eraseModal = null;     // open confirm dialog, so destroy() can drop it

    // Elements of whichever view is mounted; rebuilt on every view swap and
    // repainted in place on every poll.
    let ui = null;

    function notify(level, message) {
      window.dispatchEvent(new CustomEvent("corvus:notification",
        { detail: { level, message } }));
    }

    function busy(s) {
      return !!s && (s.state === "listing" || s.state === "downloading"
        || s.state === "erasing");
    }

    function logs() { return (status && status.logs) || []; }
    function tlogs() { return (status && status.tlogs) || []; }
    function saved() { return (status && status.saved) || []; }

    /* ---------------- tile view ---------------- */

    function buildTiles() {
      Corvus.ui.clear(tileGrid);
      const grid = tileGrid;
      const ulogTile = Corvus.ui.tile({
        className: "logs-tile", icon: "hard-drive-download",
        title: "Vehicle logs (ULog)",
        desc: "PX4's own flight log, on the flight controller's SD card.",
        chevron: true, onClick: () => setView("ulog"),
      });
      ulogTile.dataset.view = "ulog";
      const tlogTile = Corvus.ui.tile({
        className: "logs-tile", icon: "file-clock",
        title: "Recorded tlogs",
        desc: "The MAVLink stream Corvus recorded on this laptop.",
        chevron: true, onClick: () => setView("tlog"),
      });
      tlogTile.dataset.view = "tlog";
      const reviewTile = Corvus.ui.tile({
        className: "logs-tile", icon: "activity",
        title: "Flight Review",
        desc: "Plot a downloaded ULog — motors, clipping, EKF, battery.",
        chevron: true, onClick: () => setView("review"),
      });
      reviewTile.dataset.view = "review";
      grid.appendChild(ulogTile);
      grid.appendChild(tlogTile);
      grid.appendChild(reviewTile);
      ui = { kind: "tiles", ulogTile, tlogTile, reviewTile };
      S.refreshIcons();
    }

    /** Tile subtitles carry the counts, so the landing view already answers
     *  "is there anything to fetch?" without opening either sub-page. */
    function paintTiles() {
      if (!ui || ui.kind !== "tiles") return;
      const s = status || {};
      const n = logs().length;
      const ulogDesc = ui.ulogTile.querySelector(".tile-desc");
      if (ulogDesc) {
        const have = logs().filter((l) => l.downloaded).length;
        ulogDesc.textContent = !s.connected
          ? "Connect to the vehicle to read its logs."
          : (n ? n + " log(s) on the vehicle"
                 + (have ? ", " + have + " already in the folder" : "")
                 + (busy(s) ? " — download running" : "")
               : "Not read yet — open to ask the vehicle.");
      }
      const tlogDesc = ui.tlogTile.querySelector(".tile-desc");
      if (tlogDesc) {
        const t = tlogs().length;
        tlogDesc.textContent = t
          ? t + " session(s) recorded on this laptop"
          : "Nothing recorded yet.";
      }
      const reviewDesc = ui.reviewTile && ui.reviewTile.querySelector(".tile-desc");
      if (reviewDesc) {
        const n = saved().length;
        reviewDesc.textContent = n
          ? n + " downloaded log(s) ready to review"
          : "Download a ULog first, then review it here.";
      }
    }

    /* ---------------- ULog sub-page ---------------- */

    function buildUlog() {
      Corvus.ui.clear(body);
      body.appendChild(S.backButton(() => setView("tiles"), "Analysis"));

      const card = S.el("div", "page-card logs-card");
      const head = S.el("div", "logs-head");
      head.appendChild(S.sectionTitle("Vehicle logs (ULog)"));
      const refreshBtn = Corvus.ui.button({
        variant: "secondary", size: "sm", icon: "refresh-cw",
        label: "Read from vehicle", onClick: () => refresh(),
      });
      head.appendChild(refreshBtn);
      card.appendChild(head);

      const linkBanner = S.el("div", "params-banner");
      linkBanner.hidden = true;
      linkBanner.textContent = "No link to the vehicle — connect to read its logs.";
      card.appendChild(linkBanner);

      const selectRow = S.el("div", "logs-select-row");
      const selectMissing = Corvus.ui.button({
        variant: "ghost", size: "sm", label: "Select missing",
        // The common case after a flight: fetch what is not already here.
        onClick: () => setAll("missing"),
      });
      const selectAll = Corvus.ui.button({
        variant: "ghost", size: "sm", label: "Select all", onClick: () => setAll(true),
      });
      const selectNone = Corvus.ui.button({
        variant: "ghost", size: "sm", label: "Clear", onClick: () => setAll(false),
      });
      const selectionCount = S.el("span", "logs-selection", "");
      selectRow.appendChild(selectMissing);
      selectRow.appendChild(selectAll);
      selectRow.appendChild(selectNone);
      selectRow.appendChild(selectionCount);
      card.appendChild(selectRow);

      const list = S.el("div", "logs-list");
      list.setAttribute("role", "group");
      list.setAttribute("aria-label", "Logs on the vehicle");
      card.appendChild(list);

      const actionRow = S.el("div", "logs-actions");
      const downloadBtn = Corvus.ui.button({
        variant: "primary", icon: "hard-drive-download",
        label: "Download selected", disabled: true, onClick: () => download(),
      });
      const cancelBtn = Corvus.ui.button({
        variant: "secondary", size: "sm", icon: "x", label: "Cancel",
        onClick: () => cancel(),
      });
      cancelBtn.hidden = true;
      // MAVLink has no per-log delete, so this is not a per-row bin icon: the
      // button says what the protocol actually does, and the confirm says what
      // would be lost.
      const eraseBtn = Corvus.ui.button({
        variant: "danger", size: "sm", icon: "trash-2",
        className: "logs-erase", label: "Erase all on vehicle",
        onClick: () => confirmErase(),
      });
      actionRow.appendChild(downloadBtn);
      actionRow.appendChild(cancelBtn);
      actionRow.appendChild(eraseBtn);
      card.appendChild(actionRow);

      const bar = S.el("div", "calib-progress");
      const barFill = S.el("div", "calib-progress-fill");
      bar.appendChild(barFill);
      bar.hidden = true;
      card.appendChild(bar);
      const statusLine = S.el("div", "logs-status");
      card.appendChild(statusLine);
      body.appendChild(card);

      ui = {
        kind: "ulog", list, linkBanner, refreshBtn, selectMissing, selectAll,
        selectNone, selectionCount, downloadBtn, cancelBtn, eraseBtn, bar,
        barFill, statusLine,
      };
      renderList();
      S.refreshIcons();
    }

    function renderList() {
      if (!ui || ui.kind !== "ulog") return;
      const list = ui.list;
      Corvus.ui.clear(list);
      const items = logs();
      if (!items.length) {
        list.appendChild(S.el("div", "guidance-empty",
          "No logs read yet — press “Read from vehicle”."));
        paintSelection();
        return;
      }
      items.forEach((log) => {
        const rowEl = S.el("label", "logs-row");
        rowEl.dataset.id = String(log.id);
        const box = document.createElement("input");
        box.type = "checkbox";
        box.className = "logs-check";
        box.checked = selected.has(log.id);
        box.setAttribute("aria-label", "Log " + log.id);
        box.addEventListener("change", () => {
          if (box.checked) selected.add(log.id);
          else selected.delete(log.id);
          paintSelection();
        });
        rowEl.appendChild(box);
        const rowBody = S.el("div", "logs-row-body");
        const titleRow = S.el("div", "logs-row-title-row");
        titleRow.appendChild(S.el("span", "logs-row-title", "Log " + log.id));
        // Already in the folder — the difference between a no-op and pulling
        // megabytes back over a telemetry link.
        if (log.downloaded) {
          titleRow.appendChild(S.el("span", "logs-row-tag", "in folder"));
        }
        rowBody.appendChild(titleRow);
        rowBody.appendChild(S.el("span", "logs-row-meta",
          formatUtc(log.utc) + "  ·  " + formatSize(log.size)
          + (log.downloaded && log.file ? "  ·  " + log.file : "")));
        rowEl.appendChild(rowBody);
        // A downloaded log is one click from being read. Without this the
        // operator has to go back, open Flight Review and find the same file
        // again in a list that names it by filename, not by log number.
        if (log.downloaded && log.file_name) {
          const open = Corvus.ui.button({
            variant: "ghost", size: "sm", icon: "arrow-right",
            className: "logs-row-review", label: "Review",
            onClick: (event) => {
              // The row is a <label> around the checkbox: without this the
              // shortcut would also toggle the selection it is leaving.
              if (event) { event.preventDefault(); event.stopPropagation(); }
              setView("review", log.file_name);
            },
          });
          open.title = "Open " + log.file_name + " in Flight Review";
          rowEl.appendChild(open);
        }
        rowEl.appendChild(S.el("span", "logs-row-mark"));
        list.appendChild(rowEl);
      });
      // The rows carry icons and are rebuilt on their own whenever the poll
      // changes the log list, not only from buildUlog.
      S.refreshIcons();
      paintSelection();
    }

    function paintSelection() {
      if (!ui || ui.kind !== "ulog") return;
      const n = selected.size;
      ui.selectionCount.textContent = n
        ? n + " selected — they download one after another"
        : "";
      gate();
    }

    function paintUlog() {
      if (!ui || ui.kind !== "ulog") return;
      const s = status || {};
      // Mark rows the backend has finished, so a long queue is readable at a
      // glance instead of only through the status line.
      const done = {};
      (s.completed || []).forEach((c) => { done[c.id] = c.ok ? "done" : "failed"; });
      Array.prototype.forEach.call(ui.list.children, (rowEl) => {
        if (rowEl.dataset.id === undefined) return;
        const id = Number(rowEl.dataset.id);
        if (s.current === id) rowEl.dataset.state = "active";
        else if (done[id]) rowEl.dataset.state = done[id];
        else rowEl.dataset.state = "";
      });
      const pct = Math.max(0, Math.min(100, Math.round(s.percent || 0)));
      ui.bar.hidden = !busy(s);
      ui.barFill.style.width = pct + "%";
      ui.statusLine.textContent = s.message || "";
      ui.statusLine.classList.toggle("err", s.state === "failed");
      gate();
    }

    /* ---------------- tlog sub-page ---------------- */

    function buildTlog() {
      Corvus.ui.clear(body);
      body.appendChild(S.backButton(() => setView("tiles"), "Analysis"));
      const card = S.el("div", "page-card logs-card");
      card.appendChild(S.sectionTitle("Recorded tlogs"));
      const desc = S.el("div", "params-desc");
      desc.textContent =
        "Every session Corvus recorded from the MAVLink stream. These are "
        + "already on disk — nothing to download.";
      card.appendChild(desc);
      const list = S.el("div", "logs-list");
      card.appendChild(list);
      body.appendChild(card);
      ui = { kind: "tlog", list };
      paintTlog();
      S.refreshIcons();
    }

    function paintTlog() {
      if (!ui || ui.kind !== "tlog") return;
      Corvus.ui.clear(ui.list);
      const items = tlogs();
      if (!items.length) {
        ui.list.appendChild(S.el("div", "guidance-empty", "No tlogs recorded yet."));
        return;
      }
      items.slice(0, 60).forEach((t) => {
        const rowEl = S.el("div", "logs-row logs-row-static");
        const rowBody = S.el("div", "logs-row-body");
        rowBody.appendChild(S.el("span", "logs-row-title", t.name));
        rowBody.appendChild(S.el("span", "logs-row-meta",
          formatSize(t.size) + "  ·  " + t.path));
        rowEl.appendChild(rowBody);
        ui.list.appendChild(rowEl);
      });
    }

    /* ---------------- Flight Review ---------------- */

    function buildReview(want) {
      Corvus.ui.clear(body);
      body.appendChild(S.backButton(() => setView("tiles"), "Analysis"));

      const pickCard = S.el("div", "page-card logs-card");
      pickCard.appendChild(S.sectionTitle("Flight Review"));
      const desc = S.el("div", "params-desc");
      desc.textContent =
        "Pick a downloaded ULog, or open one from anywhere on this computer. "
        + "Corvus reads it locally — nothing leaves this machine — and plots "
        + "the handful of things that decide whether a flight was healthy.";
      pickCard.appendChild(desc);

      const pickRow = S.el("div", "review-pick");
      const fileField = Corvus.ui.select({
        ariaLabel: "Downloaded ULog to review",
        options: [{ value: "", label: "No downloaded logs yet" }],
        disabled: true,
      });
      const openBtn = Corvus.ui.button({
        variant: "primary", icon: "activity", label: "Review",
        disabled: true, onClick: () => loadReview(fileField.value),
      });
      pickRow.appendChild(fileField);
      pickRow.appendChild(openBtn);
      pickCard.appendChild(pickRow);

      // The second way in: a log that was never downloaded through Corvus —
      // pulled off the card by hand, or sent over by whoever flew it. Quiet and
      // small on purpose: it is the exception, and the folder is the norm.
      //
      // Not a path field: the operator picks in their own file dialog and the
      // bytes are posted, so Corvus never gains the ability to read an
      // arbitrary path off this machine on request.
      const browseInput = document.createElement("input");
      browseInput.type = "file";
      browseInput.accept = ".ulg";
      browseInput.className = "review-file-input";
      browseInput.setAttribute("aria-label", "ULog file on this computer");
      const browseBtn = Corvus.ui.button({
        variant: "ghost", size: "sm", icon: "folder-open",
        className: "review-browse", label: "Open a .ulg from this computer",
        onClick: () => browseInput.click(),
      });
      browseInput.addEventListener("change", () => {
        const file = browseInput.files && browseInput.files[0];
        // Cleared so picking the same file twice fires a change event again —
        // after an error, retrying the same log is the obvious thing to try.
        browseInput.value = "";
        if (file) loadUploadedReview(file);
      });
      const browseRow = S.el("div", "review-browse-row");
      browseRow.appendChild(browseBtn);
      browseRow.appendChild(browseInput);
      pickCard.appendChild(browseRow);

      const pickNote = S.el("div", "logs-status");
      pickCard.appendChild(pickNote);
      body.appendChild(pickCard);

      const out = S.el("div", "review-out");
      body.appendChild(out);

      ui = { kind: "review", fileField, openBtn, browseBtn, pickNote, out,
             drawn: [], modes: [] };
      fillReviewFiles();
      S.refreshIcons();
      // Arrived from a log row: that log is the whole reason the view opened,
      // so read it rather than asking for it a second time. Checked against the
      // folder listing rather than against the <select>, because a file deleted
      // between the poll that drew the row and the click is a real case and the
      // honest answer is to say so, not to ask the backend about a missing file.
      if (want) {
        if (saved().some((f) => f.name === want)) {
          ui.fileField.value = want;
          loadReview(want);
        } else {
          ui.pickNote.classList.add("err");
          ui.pickNote.textContent = want + " is no longer in the folder.";
        }
      }
    }

    function fillReviewFiles() {
      if (!ui || ui.kind !== "review") return;
      const files = saved();
      if (!files.length) {
        Corvus.ui.setOptions(ui.fileField,
          [{ value: "", label: "No downloaded logs yet" }]);
        ui.fileField.disabled = true;
        ui.openBtn.disabled = true;
        return;
      }
      Corvus.ui.setOptions(ui.fileField, files.map((f) => ({
        value: f.name, label: f.name + "  ·  " + formatSize(f.size),
      })), ui.fileField.value);
      ui.fileField.disabled = false;
      ui.openBtn.disabled = false;
    }

    /** The downloaded-log picker is only usable when there is something in the
     *  folder. Opening a file from disk never is gated: it is the way in when
     *  the folder is empty. */
    function gateReviewPick() {
      if (!ui || ui.kind !== "review") return;
      const none = !saved().length;
      ui.fileField.disabled = none;
      ui.openBtn.disabled = none;
    }

    /** Release every Plotly graph this view drew. Plotly holds canvases and
     *  listeners per graph div; dropping the DOM alone leaks both. */
    function purgeReview() {
      // Every caller is abandoning what is on screen, so any read still in
      // flight is answering a question nobody is asking any more.
      reviewToken++;
      if (!ui || ui.kind !== "review") return;
      if (ui.observer) {
        try { ui.observer.disconnect(); } catch (_e) {}
        ui.observer = null;
        ui.pending = null;
      }
      if (!ui.drawn) return;
      if (typeof window !== "undefined" && window.Plotly) {
        ui.drawn.forEach((node) => {
          try { window.Plotly.purge(node); } catch (_e) {}
        });
      }
      ui.drawn = [];
    }

    /** One read, whichever way the log got here. `fetcher` is the only
     *  difference between a name in the download folder and a file the
     *  operator picked; everything around it — the busy state, the abandoned-
     *  read guard, the wait shown on the page — is the same job. */
    async function runReview(label, fetcher) {
      if (!ui || ui.kind !== "review") return;
      Corvus.ui.setBusy(ui.openBtn, true);   // also disables it
      Corvus.ui.setBusy(ui.browseBtn, true);
      ui.pickNote.classList.remove("err");
      ui.pickNote.textContent = "Reading " + label + "…";
      purgeReview();
      // Claimed after the purge, because the purge is what invalidates the
      // read this one replaces.
      const mine = ++reviewToken;
      Corvus.ui.clear(ui.out);
      // A large flight takes a moment to parse. Say so on the page rather than
      // leaving an empty one — arriving from a log row, the empty page is the
      // first thing the shortcut shows.
      ui.out.appendChild(S.el("div", "page-card review-loading",
        "Reading " + label + " — this takes a moment for a long flight."));
      try {
        const data = await fetcher();
        // A second pick while the first was still parsing: the answer that
        // arrives late is not the one the operator is now looking at.
        if (!ui || ui.kind !== "review" || mine !== reviewToken) return;
        ui.pickNote.textContent = "";
        Corvus.ui.clear(ui.out);
        renderReview(data);
      } catch (err) {
        if (!ui || ui.kind !== "review" || mine !== reviewToken) return;
        Corvus.ui.clear(ui.out);
        ui.pickNote.classList.add("err");
        ui.pickNote.textContent = (err && err.message) || "Could not read that log";
      } finally {
        if (ui && ui.kind === "review" && mine === reviewToken) {
          Corvus.ui.setBusy(ui.openBtn, false);
          Corvus.ui.setBusy(ui.browseBtn, false);
          // setBusy clears `disabled`, and the picker has nothing to pick from
          // when the folder is empty.
          gateReviewPick();
        }
      }
    }

    function loadReview(name) {
      if (!name) return Promise.resolve();
      return runReview(name, () => Corvus.telemetry.requestJson(
        "/api/logs/review?file=" + encodeURIComponent(name)));
    }

    /** A ULog from outside the download folder. The bytes are posted rather
     *  than the path, so the operator's own file dialog is the only thing that
     *  ever names a file on this machine. */
    function loadUploadedReview(file) {
      return runReview(file.name || "the selected log", async () => {
        const url = "/api/logs/review/upload"
          + (file.name ? "?name=" + encodeURIComponent(file.name) : "");
        let res;
        try {
          res = await fetch(url, {
            method: "POST",
            headers: { "Content-Type": "application/octet-stream" },
            body: file,
          });
        } catch (err) {
          throw new Error((err && err.message) || "Could not send that file");
        }
        let data;
        try { data = await res.json(); }
        catch (_e) { throw new Error("Invalid response from " + url); }
        if (!res.ok || !data.ok) {
          throw new Error(data.error || ("Rejected (" + res.status + ")"));
        }
        return data;
      });
    }

    function renderReview(data) {
      const out = ui.out;
      const summary = (data && data.summary) || {};
      // Held for drawPlot: the bands go behind every time plot, not just here.
      ui.modes = (data && data.modes) || [];

      const head = S.el("div", "page-card review-summary");
      head.appendChild(S.sectionTitle(summary.name || "Flight Review"));
      const facts = S.el("div", "review-facts");
      [
        ["Duration", summary.duration_s ? summary.duration_s + " s" : "—"],
        ["Airframe", summary.airframe || "—"],
        ["Firmware", (summary.sw || "").slice(0, 10) || "—"],
        ["Board", summary.hw || "—"],
        ["Dropouts", summary.dropouts
          ? summary.dropouts + " (" + summary.dropout_ms + " ms)" : "none"],
      ].forEach(([label, value]) => {
        const fact = S.el("div", "review-fact");
        fact.appendChild(S.el("span", "review-fact-label", label));
        fact.appendChild(S.el("span", "review-fact-value", String(value)));
        facts.appendChild(fact);
      });
      head.appendChild(facts);

      // The flight as a strip of modes. Every plot below carries the same
      // bands, so this is the key as much as it is a timeline.
      if (ui.modes.length) {
        const first = ui.modes[0].start;
        const last = ui.modes[ui.modes.length - 1].end;
        const span = Math.max(0.001, last - first);

        const strip = S.el("div", "review-modes");
        strip.setAttribute("role", "img");
        strip.setAttribute("aria-label",
          "Flight modes: " + ui.modes.map((m) =>
            m.mode + " from " + clock(m.start) + " to " + clock(m.end)).join(", "));
        ui.modes.forEach((m) => {
          const width = Math.max(0.001, m.end - m.start);
          const seg = S.el("div", "review-mode-seg");
          seg.style.flexGrow = String(width);
          seg.style.background = modeColor(m.mode);
          seg.title = m.mode + "  " + clock(m.start) + " – " + clock(m.end)
            + "  (" + width.toFixed(0) + " s)";
          // Name the span in place when it is wide enough to read; the key
          // below covers the slivers.
          if (width / span > 0.12) {
            seg.appendChild(S.el("span", "review-mode-seg-label", m.mode));
          }
          strip.appendChild(seg);
        });
        head.appendChild(strip);

        // A time ruler under the strip, so "which mode when" is answerable
        // from the overview rather than only from the timeline plot.
        const ruler = S.el("div", "review-mode-ruler");
        for (let i = 0; i <= 4; i++) {
          const tick = S.el("span", "review-mode-tick", clock(first + (span * i) / 4));
          ruler.appendChild(tick);
        }
        head.appendChild(ruler);

        const key = S.el("div", "review-mode-key");
        const totals = {};
        const seen = [];
        ui.modes.forEach((m) => {
          if (seen.indexOf(m.mode) < 0) seen.push(m.mode);
          totals[m.mode] = (totals[m.mode] || 0) + (m.end - m.start);
        });
        seen.forEach((name) => {
          const chip = S.el("span", "review-mode-chip");
          const dot = S.el("span", "review-mode-dot");
          dot.style.background = modeColor(name);
          chip.appendChild(dot);
          chip.appendChild(S.el("span", null, name));
          // How long each mode was actually flown — the summary the strip
          // alone cannot give when a mode appears more than once.
          chip.appendChild(S.el("span", "review-mode-time",
            totals[name].toFixed(0) + " s"));
          key.appendChild(chip);
        });
        head.appendChild(key);
      }

      // Findings before plots: the plots are the evidence, these are the two
      // sentences worth reading if you read nothing else.
      ((data && data.findings) || []).forEach((f) => {
        const row = S.el("div", "review-finding");
        row.dataset.level = f.level || "ok";
        row.appendChild(S.icon(f.level === "ok" ? "circle-check" : "triangle-alert"));
        row.appendChild(S.el("span", null, f.text));
        head.appendChild(row);
      });
      out.appendChild(head);

      const plots = (data && data.plots) || [];
      const groups = (data && data.groups) || [];

      // Jump chips. Two dozen plots is a scroll; this is the difference
      // between a report you skim and one you give up on.
      if (groups.length > 1) {
        const nav = S.el("div", "review-nav");
        groups.forEach((group) => {
          const chip = Corvus.ui.button({
            variant: "secondary", size: "sm", label: group,
            className: "review-nav-chip",
            onClick: () => {
              const target = out.querySelector("#review-group-" + slug(group));
              if (target && typeof target.scrollIntoView === "function") {
                target.scrollIntoView({ behavior: S.reducedMotion() ? "auto" : "smooth",
                                        block: "start" });
              }
            },
          });
          nav.appendChild(chip);
        });
        head.appendChild(nav);
      }

      groups.forEach((group) => {
        const inGroup = plots.filter((p) => p.group === group);
        if (!inGroup.length) return;
        const heading = S.el("div", "review-group");
        heading.id = "review-group-" + slug(group);
        heading.appendChild(S.el("span", "review-group-title", group));
        heading.appendChild(S.el("span", "review-group-count",
          inGroup.length + (inGroup.length === 1 ? " plot" : " plots")));
        out.appendChild(heading);
        inGroup.forEach((plot) => appendPlot(out, plot));
      });
      // Anything the backend grouped under a name it did not list still gets
      // drawn — a plot silently dropped is worse than one in the wrong place.
      plots.filter((p) => groups.indexOf(p.group) < 0).forEach((p) => appendPlot(out, p));

      appendMessages(out, (data && data.messages) || []);
      S.refreshIcons();
    }

    /** Seconds as m:ss — a 7-minute flight is unreadable in raw seconds. */
    function clock(seconds) {
      const total = Math.max(0, Math.round(Number(seconds) || 0));
      const minutes = Math.floor(total / 60);
      const rest = total % 60;
      return minutes + ":" + (rest < 10 ? "0" : "") + rest;
    }

    function slug(text) {
      return String(text).toLowerCase().replace(/[^a-z0-9]+/g, "-");
    }

    function appendPlot(out, plot) {
      const card = S.el("div", "page-card review-plot-card");
      card.appendChild(S.sectionTitle(plot.title));
      const host = S.el("div", "review-plot");
      // Equal-axis plots are given more height: the aspect ratio is fixed, so
      // height is the only way to make the track larger without distorting it.
      if (plot.equal) host.dataset.equal = "1";
      card.appendChild(host);
      if (plot.note) card.appendChild(S.el("div", "review-note", plot.note));
      out.appendChild(card);
      // Drawn when it comes into view. A full review is three dozen Plotly
      // graphs, and building them all up front stalls the page for seconds
      // before anything is readable — including the summary at the top, which
      // is the part most reviews never scroll past.
      if (!observe(host, plot)) drawPlot(host, plot);
    }

    /** Queue a plot for drawing on first scroll into view.
     *  Returns false where IntersectionObserver is unavailable, so the caller
     *  falls back to drawing immediately. */
    function observe(host, plot) {
      if (typeof window === "undefined" || typeof window.IntersectionObserver !== "function") {
        return false;
      }
      if (!ui.observer) {
        ui.pending = new Map();
        ui.observer = new window.IntersectionObserver((entries) => {
          entries.forEach((entry) => {
            if (!entry.isIntersecting) return;
            const queued = ui.pending && ui.pending.get(entry.target);
            if (!queued) return;
            ui.pending.delete(entry.target);
            ui.observer.unobserve(entry.target);
            drawPlot(entry.target, queued);
          });
        }, { rootMargin: "300px 0px" });
      }
      ui.pending.set(host, plot);
      ui.observer.observe(host);
      return true;
    }

    /** The aircraft's own commentary, with a severity filter. A log full of
     *  routine chatter hides the three lines that explain the flight. */
    function appendMessages(out, messages) {
      const card = S.el("div", "page-card logs-card");
      const head = S.el("div", "logs-head");
      head.appendChild(S.sectionTitle("Flight log messages"));
      const filter = Corvus.ui.select({
        ariaLabel: "Minimum message severity",
        options: [
          { value: "all", label: "All messages" },
          { value: "warning", label: "Warnings and worse" },
          { value: "error", label: "Errors only" },
        ],
      });
      head.appendChild(filter);
      card.appendChild(head);

      const list = S.el("div", "guidance-list review-messages");
      card.appendChild(list);

      const RANK = { emergency: 0, alert: 1, critical: 2, error: 3,
                     warning: 4, notice: 5, info: 6, debug: 7 };
      function paint() {
        Corvus.ui.clear(list);
        const limit = filter.value === "error" ? 3
          : (filter.value === "warning" ? 4 : 99);
        const shown = messages.filter((m) => (RANK[m.level] === undefined
          ? 99 : RANK[m.level]) <= limit);
        if (!shown.length) {
          list.appendChild(S.el("div", "guidance-empty", messages.length
            ? "Nothing at that severity."
            : "The aircraft logged no messages."));
          return;
        }
        shown.forEach((m) => {
          const line = S.el("div", "guidance-line " + (m.level || "info"));
          line.appendChild(S.el("span", "guidance-level", m.level || "info"));
          line.appendChild(S.el("span", "guidance-msg", m.text || ""));
          list.appendChild(line);
        });
      }
      filter.addEventListener("change", paint);
      paint();
      out.appendChild(card);
    }

    function drawPlot(host, plot) {
      if (typeof window === "undefined" || !window.Plotly) {
        host.appendChild(S.el("div", "guidance-empty", "Plotting unavailable."));
        return;
      }
      const palette = Corvus.ui.chartColors();
      const order = ["nav", "healthy", "warning", "critical", "accent"];
      const traces = plot.series.map((s, i) => {
        const color = palette[order[i % order.length]];
        const trace = {
          x: s.x, y: s.y, name: s.name, type: "scatter",
          mode: s.draw === "markers" ? "markers" : "lines",
        };
        if (s.draw === "markers") {
          // Setpoints are sparse and stepped; drawing them as a line implies
          // values between the points that were never commanded.
          trace.marker = { size: 5, color: color, symbol: "circle-open" };
        } else {
          trace.line = { width: 1.4, color: color };
          // A step, not a ramp: a mode change or a setpoint change is
          // instantaneous, and a slope across it is a drawing artefact.
          if (s.shape) trace.line.shape = s.shape;
        }
        return trace;
      });
      const layout = Object.assign({}, S.plotlyLayout(plot.unit || ""), {
        margin: { l: 52, r: 12, t: 6, b: 34 },
        showlegend: true,
        legend: { orientation: "h", y: -0.25, font: { size: 9 } },
      });
      // Not every plot is against time: the ground track is north over east.
      if (plot.xlabel) {
        layout.xaxis = Object.assign({}, layout.xaxis, { title: plot.xlabel });
      }
      // And where both axes are a distance, "m" on the y axis says nothing the
      // x axis has not already said.
      if (plot.ylabel) {
        layout.yaxis = Object.assign({}, layout.yaxis, { title: plot.ylabel });
      }
      // A track drawn on unequal axes is a track of a different shape.
      if (plot.equal) {
        layout.yaxis = Object.assign({}, layout.yaxis,
          { scaleanchor: "x", scaleratio: 1 });
      }
      // Shapes accumulate — assigning here rather than appending is how the
      // rejection line silently erased the mode bands on the one plot where
      // both of them matter.
      const shapes = [];
      // Flight-mode bands behind the traces — skipped on plots whose x axis is
      // not time, where a time span would be meaningless. The same oscillation
      // means different things in Position and in Manual, and this is what
      // lets the reader tell them apart without cross-referencing.
      const annotations = [];
      if (!plot.xlabel && ui.modes && ui.modes.length) {
        // The plotted extent, not the flight's: a plot whose topic started
        // late would otherwise place its labels off the drawn axis.
        let xMin = Infinity;
        let xMax = -Infinity;
        traces.forEach((t) => {
          if (!t.x || !t.x.length) return;
          if (t.x[0] < xMin) xMin = t.x[0];
          if (t.x[t.x.length - 1] > xMax) xMax = t.x[t.x.length - 1];
        });
        const span = xMax - xMin;
        ui.modes.forEach((m) => {
          shapes.push({
            type: "rect", xref: "x", yref: "paper",
            x0: m.start, x1: m.end, y0: 0, y1: 1,
            fillcolor: modeColor(m.mode), opacity: 0.16,
            line: { width: 0 }, layer: "below",
          });
          // Named in place, turned on its side, because a colour is only a
          // legend lookup: the reader should not have to scroll back to the
          // strip to find out what the band behind a spike was.
          if (!(span > 0)) return;
          const from = Math.max(m.start, xMin);
          const to = Math.min(m.end, xMax);
          // Too narrow to read: a rotated label in a sliver of a band lands on
          // its neighbours and makes both unreadable.
          if (to - from < span * 0.025) return;
          annotations.push({
            xref: "x", yref: "paper", x: (from + to) / 2, y: 0.985,
            text: m.mode, textangle: -90, showarrow: false,
            xanchor: "center", yanchor: "top",
            font: { size: 9, color: modeColor(m.mode) }, opacity: 0.85,
          });
        });
      }
      // The rejection line on the EKF plot is the whole point of that plot.
      if (plot.threshold != null) {
        shapes.push({
          type: "line", xref: "paper", x0: 0, x1: 1,
          y0: plot.threshold, y1: plot.threshold,
          line: { color: palette.critical, width: 1, dash: "dot" },
        });
      }
      if (shapes.length) layout.shapes = shapes;
      if (annotations.length) layout.annotations = annotations;
      try {
        window.Plotly.react(host, traces, layout, S.plotlyConfig(S.reducedMotion()));
        ui.drawn.push(host);
      } catch (err) {
        console.error("flight review plot failed:", err);
      }
    }

    /* ---------------- shared ---------------- */

    function setView(next, file) {
      view = next;
      const sub = next === "ulog" || next === "tlog" || next === "review";
      // The swap, not a fold-out: the landing view goes away entirely so the
      // sub-page owns the screen and Back is the only way out.
      landing.hidden = sub;
      body.hidden = !sub;
      purgeReview();
      if (next === "ulog") buildUlog();
      else if (next === "tlog") buildTlog();
      else if (next === "review") buildReview(file);
      else { Corvus.ui.clear(body); buildTiles(); }
      if (container && typeof container.scrollTop === "number") container.scrollTop = 0;
      apply(status);
    }

    function gate() {
      if (!ui || ui.kind !== "ulog") return;
      const s = status || {};
      const running = busy(s);
      ui.downloadBtn.disabled = running || inFlight || selected.size === 0 || !s.connected;
      ui.refreshBtn.disabled = running || inFlight || !s.connected;
      ui.selectMissing.disabled = running;
      ui.selectAll.disabled = running;
      ui.selectNone.disabled = running;
      ui.cancelBtn.hidden = !running;
      ui.eraseBtn.disabled = running || inFlight || !s.connected || !logs().length;
      ui.linkBanner.hidden = !!s.connected;
    }

    function apply(s) {
      if (s) status = s;
      if (!dirTouched && status && status.dir && dirInput.value !== status.dir) {
        dirInput.value = status.dir;
      }
      if (ui && ui.kind === "ulog") {
        // The list is rebuilt only when the set of logs actually changed;
        // repainting it on every poll would drop the checkboxes mid-selection.
        // The flag is part of the signature: a finished download changes it,
        // and without that the "in folder" tag would never appear.
        const signature = logs().map((l) => l.id + ":" + (l.downloaded ? 1 : 0)).join(",");
        if (signature !== ui.signature) {
          ui.signature = signature;
          renderList();
        }
        paintUlog();
      } else if (ui && ui.kind === "tlog") {
        paintTlog();
      } else if (ui && ui.kind === "review") {
        fillReviewFiles();
      } else {
        paintTiles();
      }
      schedule();
    }

    function poll() {
      // Guarded at the entry, not only at the response: refresh/download/cancel
      // all poll from a `finally`, so navigating away mid-request would
      // otherwise fire a status fetch at a page that no longer exists.
      if (destroyed) return Promise.resolve();
      return Corvus.telemetry.requestJson("/api/logs/status")
        .then((s) => { if (!destroyed) apply(s); })
        .catch(() => { if (!destroyed) schedule(); });
    }

    function schedule() {
      if (destroyed) return;
      if (timer) window.clearTimeout(timer);
      timer = window.setTimeout(poll, busy(status) ? POLL_BUSY_MS : POLL_IDLE_MS);
    }

    async function saveDir() {
      const value = String(dirInput.value || "").trim();
      if (!value) return;
      dirBtn.disabled = true;
      try {
        const res = await Corvus.telemetry.postAction("/api/logs/dir", { dir: value });
        dirTouched = false;
        dirNote.classList.remove("err");
        dirNote.textContent = res.warning
          ? res.warning
          : "Saved — logs go to " + (res.dir || value) + ".";
        poll();
      } catch (err) {
        dirNote.classList.add("err");
        dirNote.textContent = (err && err.message) || "Could not use that folder";
      } finally {
        dirBtn.disabled = false;
      }
    }

    async function refresh() {
      inFlight = true;
      gate();
      if (ui && ui.kind === "ulog") ui.statusLine.textContent = "Asking the vehicle for its logs…";
      try {
        await Corvus.telemetry.postAction("/api/logs/refresh", {});
      } catch (err) {
        const msg = (err && err.message) || "Could not list logs";
        if (ui && ui.kind === "ulog") {
          ui.statusLine.textContent = msg;
          ui.statusLine.classList.add("err");
        }
        notify("warning", msg);
      } finally {
        inFlight = false;
        poll();
      }
    }

    async function download() {
      const ids = Array.from(selected);
      if (!ids.length) return;
      inFlight = true;
      gate();
      try {
        await Corvus.telemetry.postAction("/api/logs/download", { ids });
      } catch (err) {
        const msg = (err && err.message) || "Download refused";
        if (ui && ui.kind === "ulog") {
          ui.statusLine.textContent = msg;
          ui.statusLine.classList.add("err");
        }
        notify("critical", msg);
      } finally {
        inFlight = false;
        poll();
      }
    }

    async function cancel() {
      if (!ui || ui.kind !== "ulog") return;
      ui.cancelBtn.disabled = true;
      try {
        await Corvus.telemetry.requestJson("/api/logs/cancel", { method: "POST" });
      } catch (_e) { /* the next poll reports the real state */ }
      finally {
        ui.cancelBtn.disabled = false;
        poll();
      }
    }

    /**
     * Gate the erase behind a dialog that states the two things the operator
     * needs: it takes ALL logs (the protocol offers nothing finer), and how
     * many of them are not yet in the download folder — which is the number
     * that turns an inconvenience into lost evidence.
     */
    function confirmErase() {
      if (eraseModal) return;
      const all = logs();
      const missing = all.filter((l) => !l.downloaded);

      const box = S.el("div", "motor-calib-warning");
      box.appendChild(S.el("div", "motor-calib-warning-line",
        "This erases ALL " + all.length + " log(s) from the flight controller. "
        + "MAVLink has no way to delete a single log."));
      box.appendChild(S.el("div", "motor-calib-warning-line",
        missing.length
          ? missing.length + " of them are NOT in your download folder yet and "
            + "cannot be recovered afterwards."
          : "All of them are already in your download folder."));
      box.appendChild(S.el("div", "motor-calib-warning-line",
        "The vehicle is asked for its log list again afterwards, so you will "
        + "see the result rather than have to trust it."));

      const cancelAction = Corvus.ui.button({
        variant: "secondary", label: "Keep the logs", onClick: closeEraseModal,
      });
      const confirmAction = Corvus.ui.button({
        variant: "danger",
        label: missing.length ? "Erase anyway" : "Erase all logs",
        onClick: () => { closeEraseModal(); erase(); },
      });
      eraseModal = Corvus.ui.modal({
        title: "Erase all logs on the vehicle",
        size: "sm", body: box, actions: [cancelAction, confirmAction],
        mount: page, onClose: () => { eraseModal = null; },
      });
      eraseModal.open();
    }

    function closeEraseModal() {
      if (!eraseModal) return;
      eraseModal.close();
      eraseModal = null;
    }

    async function erase() {
      inFlight = true;
      gate();
      selected.clear();
      try {
        await Corvus.telemetry.postAction("/api/logs/erase", {});
      } catch (err) {
        const msg = (err && err.message) || "Erase refused";
        if (ui && ui.kind === "ulog") {
          ui.statusLine.textContent = msg;
          ui.statusLine.classList.add("err");
        }
        notify("critical", msg);
      } finally {
        inFlight = false;
        poll();
      }
    }

    function setAll(mode) {
      selected.clear();
      if (mode === "missing") logs().filter((l) => !l.downloaded).forEach((l) => selected.add(l.id));
      else if (mode) logs().forEach((l) => selected.add(l.id));
      renderList();
    }

    dirInput.addEventListener("input", () => { dirTouched = true; });

    // The link state changes the gate and the tile subtitles, and it changes
    // without any log activity of its own.
    let unsub = null;
    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      unsub = Corvus.telemetry.subscribe((t) => {
        if (!t || !status) return;
        if (!!t.connected !== !!status.connected) poll();
      });
    }

    buildTiles();
    poll();

    return function destroy() {
      destroyed = true;
      if (timer) { try { window.clearTimeout(timer); } catch (_e) {} timer = null; }
      if (unsub) { try { unsub(); } catch (_e) {} unsub = null; }
      closeEraseModal();
      purgeReview();
      try { telemetry.destroy(); } catch (_e) {}
    };
  }

  return { render };
})();
