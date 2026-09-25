"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupVideo — the Video sub-page of the Setup page.
 *
 * A list of cameras, each an address with an optional user and password, and
 * the ffmpeg that decodes the RTSP ones. A camera is one of two kinds:
 *
 *   RTSP    decoded by ffmpeg in the backend (udp://, srt:// and http://
 *           streams go the same way).
 *   WebRTC  a WHEP address on a media server (MediaMTX, go2rtc, Janus); the
 *           window plays it itself, and its state is the window's to report.
 *
 * Opening a camera puts it in a floating
 * window of its own (Corvus.videoWindows), the same frame the SSH Launcher's
 * terminals use, so it stays up over the map while the operator flies. The
 * camera button on the map opens every camera at once.
 *
 * Two views, never both: the LIST and the EDITOR for one camera, the same
 * shape as the SSH Launcher's shelf and for the same reason. Removing a
 * camera is in its editor, beside Save and Cancel, not beside its Open
 * button.
 *
 * The password is never sent to the browser. The editor leaves it out of the
 * save unless the operator typed one, and the backend then keeps the stored
 * one (see POST /api/video/streams).
 *
 * Backend contract:
 *   GET  /api/video/status           cameras with their state, and the decoder
 *   POST /api/video/streams          {id?, name, kind, url, username, password?, transport}
 *   POST /api/video/streams/remove   {id}
 *   POST /api/video/settings         {ffmpeg}
 *
 * Exposes render(container, navigateBack) -> destroy(). destroy() stops the
 * poll; the camera windows are the operator's and stay open.
 */
Corvus.setupVideo = (function () {
  const S = Corvus.setupShared;

  // The rows carry each camera's state; this is how fresh it is while the page
  // is open. The window itself is live, this only has to say which is which.
  const POLL_MS = 2000;

  const STATE_TEXT = {
    idle: "Not open",
    starting: "Connecting",
    live: "Live",
    stalled: "No picture",
    error: "Offline",
    unavailable: "No decoder",
  };
  const STATE_LEVEL = {
    idle: "off",
    starting: "warning",
    live: "healthy",
    stalled: "warning",
    error: "critical",
    unavailable: "critical",
  };

  /**
   * The line under a camera's name: what the picture is when it is live, what
   * went wrong when it is not, and the address otherwise.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} s one camera from GET /api/video/status
   * @returns {string}
   */
  function detailLine(s) {
    const c = s || {};
    if (c.state === "live" && c.width && c.height) {
      const fps = Number(c.fps) > 0 ? `, ${Math.round(Number(c.fps))} fps` : "";
      return `${c.width} × ${c.height}${fps}`;
    }
    if ((c.state === "error" || c.state === "stalled") && c.message) return String(c.message);
    return String(c.address || c.url || "");
  }

  /**
   * The body of POST /api/video/streams for one editor draft. The password is
   * only sent when the operator typed one, because the page never had the
   * stored one to send back.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} draft {id, name, url, username, password, passwordTouched, transport}
   * @returns {Object}
   */
  function saveBody(draft) {
    const d = draft || {};
    const body = {
      name: String(d.name || "").trim(),
      kind: d.kind === "webrtc" ? "webrtc" : "rtsp",
      url: String(d.url || "").trim(),
      username: String(d.username || "").trim(),
      transport: d.transport === "udp" ? "udp" : "tcp",
    };
    if (d.id) body.id = d.id;
    if (d.passwordTouched) body.password = String(d.password || "");
    return body;
  }

  /**
   * Why a draft cannot be saved yet, or "" when it can. The backend checks
   * the address properly; this only stops a save that cannot work.
   * @param {Object} draft
   * @returns {string}
   */
  function draftProblem(draft) {
    const url = String((draft && draft.url) || "").trim();
    if (!url) return "Enter the camera's address.";
    if (draft.kind === "webrtc") {
      if (!/^https?:\/\//i.test(url)) {
        return "A WebRTC camera is its WHEP address, for example http://192.168.144.25:8889/cam/whep.";
      }
      return "";
    }
    if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(url)) {
      return "The address starts with rtsp://, for example rtsp://192.168.144.25:8554/main.264.";
    }
    return "";
  }

  /**
   * What a row shows for one camera. An RTSP camera's state is the
   * backend's; a WebRTC camera's is only known to its window, so it is read
   * from there, and a WebRTC camera with no window open is simply not open.
   *
   * Pure apart from the window lookup it is handed; exported for the tests.
   *
   * @param {Object} s one camera from GET /api/video/status
   * @param {Function} stateOf Corvus.videoWindows.stateOf
   * @returns {Object}
   */
  function shownState(s, stateOf) {
    if (!s || s.kind !== "webrtc") return s;
    const local = typeof stateOf === "function" ? stateOf(s.id) : null;
    return Object.assign({}, s, local || { state: "idle", message: "", width: 0, height: 0, fps: 0 });
  }

  // What the editor says about the address, per kind.
  const KIND_TEXT = {
    rtsp: {
      placeholder: "rtsp://192.168.144.25:8554/main.264",
      hint: "The camera's RTSP address, from its manual or its web page. " +
            "udp://, srt:// and http:// streams work too. A user and " +
            "password written into the address are moved to the fields below.",
      password: "Kept by Corvus on this computer and never shown again.",
    },
    webrtc: {
      placeholder: "http://192.168.144.25:8889/cam/whep",
      hint: "The WHEP address of the stream on a media server such as MediaMTX " +
            "(http://host:8889/<stream>/whep), go2rtc or Janus. The video goes " +
            "straight from there to the window; Corvus only opens the connection.",
      password: "Kept by Corvus on this computer and never shown again. For a " +
                "token, leave User empty and put the token here.",
    },
  };

  function render(container, navigateBack) {
    const ui = Corvus.ui;
    const page = S.el("div", "setup-page video-page");
    page.appendChild(S.backButton(navigateBack));
    page.appendChild(S.pageHeader(
      "Video",
      "Cameras on the aircraft or the ground network, over RTSP or WebRTC, each in a window of its own",
    ));

    let status = null;
    let destroyed = false;
    let pollTimer = null;
    let editing = null;
    // null, not "": an empty camera list has the signature "", and it still
    // has to be drawn once to say that there are no cameras.
    let listSignature = null;
    const rows = new Map();

    // Two messages: whether video can work at all, at the top, and what the
    // last save did, with the cameras. The poll repaints the first and must
    // not wipe the second.
    const availability = ui.message({});
    page.appendChild(availability.el);
    const notice = ui.message({});

    // ------------------------------------------------------------------
    // Cameras
    // ------------------------------------------------------------------

    const camerasCard = S.el("div", "page-card video-cameras-card");
    camerasCard.appendChild(S.sectionTitle("Cameras"));
    camerasCard.appendChild(notice.el);
    const listEl = S.el("div", "video-list");
    camerasCard.appendChild(listEl);
    const addBtn = ui.button({
      variant: "secondary",
      icon: "plus",
      label: "Add camera",
      onClick: () => renderEditor({
        id: "", name: "", kind: "rtsp", url: "", username: "", password: "",
        passwordTouched: false, hasPassword: false, transport: "tcp",
      }, true),
    });
    const openAllBtn = ui.button({
      variant: "ghost",
      icon: "video",
      label: "Open all",
      title: "Open a window for every camera. The camera button on the map does the same.",
      onClick: () => ((status && status.streams) || []).forEach((s) => Corvus.videoWindows.open(s)),
    });
    camerasCard.appendChild(ui.actions([addBtn, openAllBtn]));
    page.appendChild(camerasCard);

    const editorEl = S.el("div", "video-editor");
    editorEl.hidden = true;
    page.appendChild(editorEl);

    // ------------------------------------------------------------------
    // Decoder
    // ------------------------------------------------------------------

    const decoderCard = S.el("div", "page-card video-decoder-card");
    decoderCard.appendChild(S.sectionTitle("Decoder"));
    const decoderIntro = S.el("div", "params-desc");
    decoderIntro.textContent =
      "RTSP cameras send H.264 or H.265, which Corvus hands to ffmpeg to " +
      "decode. It runs only while a camera window is open, and stops a few " +
      "seconds after the last one closes. WebRTC cameras need no ffmpeg: the " +
      "window decodes them itself.";
    decoderCard.appendChild(decoderIntro);
    const passwordWarning = S.el("div", "video-warning",
      "This ffmpeg is older than version 5, so camera passwords show in the " +
      "process list, where other accounts on this computer can read them. " +
      "Install ffmpeg 5 or later to keep them private.");
    passwordWarning.hidden = true;
    decoderCard.appendChild(passwordWarning);
    const codecs = (Corvus.videoWindows && Corvus.videoWindows.webrtcAvailable())
      ? Corvus.videoWindows.webrtcCodecs() : [];
    const rtcRow = S.infoRow("WebRTC plays",
      codecs.length ? codecs.join(", ") : "WebRTC is not available here", "webrtc-codecs");
    rtcRow.title = "The video codecs this window can receive over WebRTC. A WebRTC " +
      "camera has to send one of them.";
    const versionRow = S.infoRow("ffmpeg", "…", "ffmpeg-version");
    const pathRow = S.infoRow("Found at", "…", "ffmpeg-path");
    decoderCard.appendChild(versionRow);
    decoderCard.appendChild(pathRow);
    decoderCard.appendChild(rtcRow);
    const versionValue = versionRow.querySelector(".page-row-value");
    const pathValue = pathRow.querySelector(".page-row-value");
    pathValue.classList.add("video-mono");

    const ffmpegInput = ui.input({
      placeholder: "Found by itself",
      mono: true,
      ariaLabel: "Path to ffmpeg",
      autocomplete: false,
      spellcheck: false,
    });
    const ffmpegSave = ui.button({
      variant: "secondary",
      size: "sm",
      label: "Use this ffmpeg",
      onClick: () => saveDecoder(),
    });
    decoderCard.appendChild(ui.field({
      label: "ffmpeg path",
      control: ffmpegInput,
      hint: "Leave it empty to find ffmpeg by itself: on the PATH, in the usual " +
            "install folders, or from the imageio-ffmpeg package.",
    }));
    decoderCard.appendChild(ui.actions(ffmpegSave));
    page.appendChild(decoderCard);

    container.appendChild(page);

    // ------------------------------------------------------------------
    // Painting
    // ------------------------------------------------------------------

    function paint(data) {
      status = data || {};
      if (Corvus.videoWindows) Corvus.videoWindows.setCameras(status);
      const streams = Array.isArray(status.streams) ? status.streams : [];
      const needsDecoder = !streams.length || streams.some((s) => s.kind !== "webrtc");
      if (status.available === false && needsDecoder) {
        availability.show(status.reason || "Video is unavailable.", "warn");
      } else {
        availability.hide();
      }

      const ff = status.ffmpeg || {};
      versionValue.textContent = ff.path ? (ff.version || "found") : "not found";
      pathValue.textContent = ff.path || "—";
      if (document.activeElement !== ffmpegInput && !ffmpegInput.dataset.dirty) {
        ffmpegInput.value = ff.configured || "";
      }
      const oldDecoder = !!ff.path && ff.hides_password === false;
      passwordWarning.hidden = !oldDecoder;

      addBtn.disabled = streams.length >= (status.max_streams || 8);
      openAllBtn.hidden = streams.length < 2;

      const signature = streams.map((s) => [s.id, s.name, s.address, s.kind].join("\u0000")).join("\u0001");
      if (signature !== listSignature) {
        listSignature = signature;
        buildList(streams);
      }
      streams.forEach((s) => paintRow(s));
    }

    function buildList(streams) {
      rows.clear();
      ui.clear(listEl);
      if (!streams.length) {
        listEl.appendChild(ui.empty(
          "No cameras yet. Add the RTSP address of the camera on the aircraft " +
          "or on the ground network."));
        return;
      }
      streams.forEach((s) => listEl.appendChild(buildRow(s)));
      ui.refreshIcons();
    }

    function buildRow(s) {
      const row = S.el("div", "video-row");
      row.dataset.id = s.id;
      const dot = ui.statusDot("off");
      const text = S.el("div", "video-row-text");
      const nameLine = S.el("span", "video-row-name-line");
      const name = S.el("span", "video-row-name", s.name || s.address || "Camera");
      const kind = S.el("span", "video-row-kind", s.kind === "webrtc" ? "WebRTC" : "RTSP");
      nameLine.appendChild(name);
      nameLine.appendChild(kind);
      const detail = S.el("span", "video-row-detail", "");
      text.appendChild(nameLine);
      text.appendChild(detail);
      const state = S.el("span", "video-row-state", "");
      const openBtn = ui.button({
        variant: "primary",
        size: "sm",
        icon: "play",
        label: "Open",
        ariaLabel: `Open ${s.name || "the camera"}`,
        title: "Show the camera in a window of its own",
        onClick: () => {
          const current = ((status && status.streams) || []).find((c) => c.id === s.id) || s;
          Corvus.videoWindows.open(current);
        },
      });
      const editBtn = ui.iconButton("pencil", {
        ariaLabel: `Edit ${s.name || "the camera"}`,
        title: "Edit",
        onClick: () => {
          const current = ((status && status.streams) || []).find((c) => c.id === s.id) || s;
          renderEditor({
            id: current.id,
            name: current.name || "",
            kind: current.kind === "webrtc" ? "webrtc" : "rtsp",
            url: current.url || "",
            username: current.username || "",
            password: "",
            passwordTouched: false,
            hasPassword: !!current.has_password,
            transport: current.transport === "udp" ? "udp" : "tcp",
          }, false);
        },
      });
      row.appendChild(dot);
      row.appendChild(text);
      row.appendChild(state);
      row.appendChild(openBtn);
      row.appendChild(editBtn);
      rows.set(s.id, { dot, detail, state });
      return row;
    }

    function paintRow(camera) {
      const r = rows.get(camera.id);
      if (!r) return;
      const s = shownState(camera, Corvus.videoWindows && Corvus.videoWindows.stateOf);
      const st = STATE_TEXT[s.state] ? s.state : "idle";
      r.dot.setAttribute("data-level", STATE_LEVEL[st]);
      r.state.textContent = STATE_TEXT[st];
      r.state.dataset.state = st;
      const line = detailLine(s);
      if (r.detail.textContent !== line) r.detail.textContent = line;
      r.detail.title = line;
    }

    // ------------------------------------------------------------------
    // The editor
    // ------------------------------------------------------------------

    /**
     * Show the form for one camera. `draft` is a copy; nothing is saved until
     * Save, so Cancel really cancels.
     */
    function renderEditor(draft, isNew) {
      editing = draft;
      camerasCard.hidden = true;
      decoderCard.hidden = true;
      editorEl.hidden = false;
      ui.clear(editorEl);

      const card = S.el("div", "page-card video-editor-card");
      card.appendChild(S.sectionTitle(isNew ? "New camera" : "Camera"));

      const nameInput = ui.input({
        value: draft.name,
        placeholder: "Gimbal camera",
        ariaLabel: "Camera name",
        autocomplete: false,
        onInput: (v) => { draft.name = v; },
      });
      card.appendChild(ui.field({
        label: "Name",
        control: nameInput,
        hint: "What the window's title bar says. Leave it empty to use the address.",
      }));

      const kindSel = ui.select({
        ariaLabel: "Camera type",
        options: [
          { value: "rtsp", label: "RTSP (decoded by Corvus with ffmpeg)" },
          { value: "webrtc", label: "WebRTC (WHEP, played by the window)" },
        ],
        value: draft.kind,
        onChange: (v) => { draft.kind = v === "webrtc" ? "webrtc" : "rtsp"; paintKind(); check(); },
      });
      card.appendChild(ui.field({
        label: "Type",
        control: kindSel,
        hint: "RTSP is what most cameras send. WebRTC needs a media server in " +
              "between, and gives the lowest delay.",
      }));

      const urlInput = ui.input({
        value: draft.url,
        placeholder: KIND_TEXT.rtsp.placeholder,
        mono: true,
        ariaLabel: "Camera address",
        autocomplete: false,
        spellcheck: false,
        onInput: (v) => { draft.url = v; check(); },
      });
      const urlField = ui.field({ label: "Address", control: urlInput, hint: KIND_TEXT.rtsp.hint });
      card.appendChild(urlField);

      const userInput = ui.input({
        value: draft.username,
        placeholder: "optional",
        ariaLabel: "Camera user",
        autocomplete: false,
        spellcheck: false,
        onInput: (v) => { draft.username = v; },
      });
      card.appendChild(ui.field({ label: "User", control: userInput }));

      const passInput = ui.input({
        type: "password",
        placeholder: draft.hasPassword ? "Saved. Type to replace it." : "optional",
        mono: true,
        ariaLabel: "Camera password",
        autocomplete: false,
        onInput: (v) => { draft.password = v; draft.passwordTouched = true; },
      });
      const passField = ui.field({ label: "Password", control: passInput, hint: KIND_TEXT.rtsp.password });
      card.appendChild(passField);

      const transportSel = ui.select({
        ariaLabel: "RTSP transport",
        options: [
          { value: "tcp", label: "TCP (works through packet loss)" },
          { value: "udp", label: "UDP (lower delay on a clean link)" },
        ],
        value: draft.transport,
        onChange: (v) => { draft.transport = v; },
      });
      const transportField = ui.field({
        label: "Transport",
        control: transportSel,
        hint: "How the RTSP video travels. TCP unless the camera only does UDP.",
      });
      card.appendChild(transportField);

      /** The address hint, the example and the transport follow the type. */
      function paintKind() {
        const text = KIND_TEXT[draft.kind] || KIND_TEXT.rtsp;
        urlInput.placeholder = text.placeholder;
        const urlHint = urlField.querySelector && urlField.querySelector(".field-hint");
        if (urlHint) urlHint.textContent = text.hint;
        const passHint = passField.querySelector && passField.querySelector(".field-hint");
        if (passHint) passHint.textContent = text.password;
        transportField.hidden = draft.kind === "webrtc";
      }
      paintKind();

      const msg = ui.message({});
      card.appendChild(msg.el);

      const saveBtn = ui.button({
        variant: "primary",
        icon: "check",
        label: "Save",
        onClick: () => save(),
      });
      const deleteBtn = isNew ? null : ui.button({
        variant: "danger",
        icon: "trash-2",
        label: "Delete",
        title: "Remove this camera",
        onClick: () => remove(),
      });
      const cancelBtn = ui.button({
        variant: "ghost",
        label: "Cancel",
        onClick: () => closeEditor(),
      });
      card.appendChild(ui.actions([saveBtn, deleteBtn, cancelBtn]));
      editorEl.appendChild(card);

      function check() {
        const problem = draftProblem(draft);
        saveBtn.disabled = !!problem;
        saveBtn.title = problem;
      }
      check();

      async function save() {
        ui.setBusy(saveBtn, true);
        msg.hide();
        try {
          const res = await Corvus.telemetry.requestJson("/api/video/streams", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(saveBody(draft)),
          });
          if (destroyed) return;
          closeEditor();
          if (res && res.status) paint(res.status);
          const saved = ((res && res.status && res.status.streams) || [])
            .find((s) => s.id === res.id);
          // A camera just added is one the operator wants to see: that is the
          // test of whether the address is right.
          if (isNew && saved) Corvus.videoWindows.open(saved);
          if (res && res.warning) notice.show(res.warning, "warn");
          else notice.show(isNew ? "Camera added." : "Camera saved.", "ok");
        } catch (err) {
          if (destroyed) return;
          ui.setBusy(saveBtn, false);
          msg.show((err && err.message) || "The camera could not be saved.", "err");
        }
      }

      async function remove() {
        const label = draft.name || draft.url || "this camera";
        if (!window.confirm(`Remove "${label}"?`)) return;
        ui.setBusy(deleteBtn, true);
        try {
          const res = await Corvus.telemetry.requestJson("/api/video/streams/remove", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ id: draft.id }),
          });
          if (destroyed) return;
          Corvus.videoWindows.close(draft.id);
          closeEditor();
          if (res && res.status) paint(res.status);
          notice.show("Camera removed.", "ok");
        } catch (err) {
          if (destroyed) return;
          ui.setBusy(deleteBtn, false);
          msg.show((err && err.message) || "The camera could not be removed.", "err");
        }
      }

      ui.refreshIcons();
      nameInput.focus();
    }

    function closeEditor() {
      editing = null;
      ui.clear(editorEl);
      editorEl.hidden = true;
      camerasCard.hidden = false;
      decoderCard.hidden = false;
      notice.hide();
      refresh();
    }

    // ------------------------------------------------------------------
    // Decoder path
    // ------------------------------------------------------------------

    ffmpegInput.addEventListener("input", () => { ffmpegInput.dataset.dirty = "1"; });

    async function saveDecoder() {
      ui.setBusy(ffmpegSave, true);
      try {
        const res = await Corvus.telemetry.requestJson("/api/video/settings", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ ffmpeg: ffmpegInput.value.trim() }),
        });
        if (destroyed) return;
        delete ffmpegInput.dataset.dirty;
        if (res && res.status) paint(res.status);
        notice.show(ffmpegInput.value.trim()
          ? "Corvus decodes with that ffmpeg now."
          : "Corvus finds ffmpeg by itself now.", "ok");
      } catch (err) {
        if (destroyed) return;
        notice.show((err && err.message) || "The path could not be saved.", "err");
      } finally {
        if (!destroyed) ui.setBusy(ffmpegSave, false);
      }
    }

    // ------------------------------------------------------------------
    // Lifecycle
    // ------------------------------------------------------------------

    async function refresh() {
      if (editing) return;
      try {
        const data = await Corvus.telemetry.requestJson("/api/video/status");
        if (destroyed || editing) return;
        paint(data);
      } catch (_err) {
        if (destroyed) return;
        availability.show("Could not read the camera list.", "err");
      }
    }

    refresh();
    pollTimer = setInterval(refresh, POLL_MS);

    function destroy() {
      destroyed = true;
      if (pollTimer !== null) {
        clearInterval(pollTimer);
        pollTimer = null;
      }
    }

    S.refreshIcons();
    return destroy;
  }

  return { render, detailLine, saveBody, draftProblem, shownState, STATE_TEXT };
})();
