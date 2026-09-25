"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.videoWindows — floating camera views, one per camera.

  The frame is Corvus.floatWindows, the one the SSH Launcher's terminals use:
  same bar, same drag, same corner grip, same maximize and ×, same layer. A
  camera and a terminal on screen together are two of the same kind of window.

  What is inside depends on the camera's kind:

    rtsp    The backend decodes the stream with ffmpeg and this window fetches
            one JPEG at a time from GET /api/video/frame (see corvus/video.py
            for why this is not one endless stream). Asking again at once is
            what keeps the decoder running; closing the window stops asking,
            and the backend stops the decoder a few seconds later.
    webrtc  The camera's media server sends the video straight to a <video>
            in this window over WebRTC. The offer and answer pass through
            POST /api/video/webrtc/offer so the camera's password stays in the
            backend; this window holds only an opaque session token.

  Either way the state goes on the pill in the bar and over the picture, and
  the last picture stays up while the camera is reconnecting, dimmed, with the
  reason over it: a frozen frame that looks live is the one thing this window
  must never show. Both kinds reconnect by themselves, with a growing pause
  between attempts, for as long as the window is open.
*/
Corvus.videoWindows = (function () {
  const F = Corvus.floatWindows;
  const KIND = "video";
  // A 16:9 picture under the 34 px bar.
  const DEF_SIZE = { w: 560, h: 349 };
  // Pause after a failed attempt, doubled each time up to the maximum.
  const RETRY_MS = 1000;
  const MAX_RETRY_MS = 8000;
  // A frame request waits up to 2 s on the backend. One that has not
  // answered in this long is dead, not slow.
  const FRAME_TIMEOUT_MS = 12000;
  // WebRTC: how long to gather ICE candidates before sending the offer (there
  // is no STUN server offline, so this is the local candidates only), how
  // long the backend may take to reach the camera's server, and when no new
  // frame means "stalled" and then "reconnect".
  const RTC_GATHER_MS = 2500;
  const RTC_OFFER_TIMEOUT_MS = 20000;
  const RTC_STALL_MS = 3000;
  const RTC_RESTART_MS = 10000;
  const WATCH_MS = 1000;

  const listeners = new Set();
  // The cameras set up under Setup, Video, as last read from the backend.
  // null until the first read, so "none yet" and "not asked yet" differ.
  let known = null;
  const cameraListeners = new Set();

  // What the pill says for each state, and its colour.
  const PILL = {
    live: ["LIVE", "on"],
    starting: ["CONNECTING", "wait"],
    idle: ["CONNECTING", "wait"],
    stalled: ["NO PICTURE", "wait"],
    error: ["OFFLINE", ""],
    unavailable: ["OFFLINE", ""],
  };

  function keyOf(id) { return "video:" + id; }

  function sleep(ms) { return new Promise((resolve) => setTimeout(resolve, ms)); }

  function backoff(failures) {
    return Math.min(MAX_RETRY_MS, RETRY_MS * Math.pow(2, Math.max(0, failures - 1)));
  }

  /** A tab in the background has nobody watching it. */
  function hidden() {
    return typeof document !== "undefined" && document.hidden === true;
  }

  function emit() {
    listeners.forEach((fn) => { try { fn(count()); } catch (_e) {} });
  }

  /**
   * Put a state on a player: the pill, and the line over the picture.
   * Exported for the tests.
   * @param {Object} rec the player (see attach)
   * @param {Object} data {state, message}
   */
  function showState(rec, data) {
    const d = data || {};
    const state = PILL[d.state] ? d.state : "error";
    const [text, tone] = PILL[state];
    const message = String(d.message || "");
    if (typeof rec.setStatus === "function") rec.setStatus(text, tone, message);
    else F.setStatus(rec, text, tone, message);
    rec.state = state;
    rec.message = message;
    const live = state === "live";
    rec.bodyEl.classList.toggle("is-stale", !live && rec.hasFrame);
    rec.msgEl.hidden = live;
    rec.msgEl.textContent = live ? "" : (message || (tone === "wait"
      ? "Connecting to the camera." : "The camera is not sending."));
  }

  // ---- RTSP: JPEG frames from the backend ----------------------------------

  /** Decode one JPEG and draw it. Resolves once it is on screen. */
  function drawFrame(rec, blob) {
    const canvas = rec.canvasEl;
    const paint = (source, w, h) => {
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
      }
      const ctx = canvas.getContext && canvas.getContext("2d");
      if (ctx) ctx.drawImage(source, 0, 0);
      rec.width = w;
      rec.height = h;
    };
    // createImageBitmap decodes off the main thread and needs no object URL
    // to clean up afterwards; the <img> path is only for an engine without it.
    if (typeof window.createImageBitmap === "function") {
      return window.createImageBitmap(blob).then((bmp) => {
        if (!rec.stopped) paint(bmp, bmp.width, bmp.height);
        if (typeof bmp.close === "function") bmp.close();
      });
    }
    return new Promise((resolve) => {
      const url = URL.createObjectURL(blob);
      const img = new Image();
      const done = () => { URL.revokeObjectURL(url); resolve(); };
      img.onload = () => {
        if (!rec.stopped) paint(img, img.naturalWidth, img.naturalHeight);
        done();
      };
      img.onerror = done;
      img.src = url;
    });
  }

  function noteFrameRate(rec) {
    const now = Date.now();
    if (rec.frameAt) {
      const dt = (now - rec.frameAt) / 1000;
      if (dt > 0) rec.fps = rec.fps ? rec.fps * 0.9 + (1 / dt) * 0.1 : 1 / dt;
    }
    rec.frameAt = now;
  }

  /**
   * Ask for frames until the window closes. One request in flight, always:
   * the next is sent once the last frame is drawn, so a slow machine shows
   * fewer frames rather than falling further and further behind.
   *
   * A request that hangs is abandoned after FRAME_TIMEOUT_MS, repeated
   * failures back off, and a tab in the background stops asking, which lets
   * the backend stop the decoder until somebody looks again.
   */
  async function pump(rec) {
    let failures = 0;
    const retry = async (message) => {
      failures += 1;
      showState(rec, { state: "error", message });
      await sleep(backoff(failures));
    };
    while (!rec.stopped) {
      if (hidden()) {
        await sleep(500);
        continue;
      }
      const ac = typeof AbortController === "function" ? new AbortController() : null;
      rec.abort = ac;
      let timedOut = false;
      const timer = setTimeout(() => { timedOut = true; if (ac) ac.abort(); }, FRAME_TIMEOUT_MS);
      rec.frameTimer = timer;
      let res;
      let blob = null;
      let data = null;
      try {
        res = await fetch(
          `/api/video/frame?id=${encodeURIComponent(rec.streamId)}&after=${rec.after}`,
          { cache: "no-store", signal: ac ? ac.signal : undefined });
        const type = (res.headers && res.headers.get("Content-Type")) || "";
        if (res.ok && type.indexOf("image/") === 0) blob = await res.blob();
        else data = await res.json().catch(() => null);
      } catch (_e) {
        clearTimeout(timer);
        if (rec.stopped) return;
        await retry(timedOut ? "Corvus did not answer in time." : "Corvus is not answering.");
        continue;
      }
      clearTimeout(timer);
      if (rec.stopped) return;

      if (blob) {
        const seq = parseInt(res.headers.get("X-Video-Seq"), 10);
        if (isFinite(seq)) rec.after = seq;
        try { await drawFrame(rec, blob); } catch (_e) { continue; }
        if (rec.stopped) return;
        failures = 0;
        rec.hasFrame = true;
        noteFrameRate(rec);
        if (rec.state !== "live") showState(rec, { state: "live" });
        continue;
      }
      if (res.status === 404) {
        showState(rec, { state: "error", message: "This camera is no longer set up." });
        return;
      }
      if (!res.ok || !data) {
        await retry((data && data.error) || "Corvus could not read the camera.");
        continue;
      }
      showState(rec, data);
      // A state that came back after the backend's own wait is already paced;
      // one that came back at once (no decoder) is not.
      if (data.state === "unavailable") await sleep(MAX_RETRY_MS);
    }
  }

  // ---- WebRTC: straight from the camera's media server ---------------------

  /** Whether this engine can do WebRTC at all. */
  function webrtcAvailable() {
    return typeof window.RTCPeerConnection === "function";
  }

  /**
   * The video codecs this window can receive over WebRTC, e.g.
   * ["VP8", "VP9", "AV1"]. The desktop app's engine is built without the
   * proprietary codecs, and then H.264 is not among them: an H.264 camera
   * has to come in over RTSP there. Asked of the engine, never assumed.
   * @returns {string[]}
   */
  function webrtcCodecs() {
    try {
      const R = window.RTCRtpReceiver;
      const caps = R && typeof R.getCapabilities === "function" ? R.getCapabilities("video") : null;
      const names = ((caps && caps.codecs) || [])
        .map((c) => String(c.mimeType || "").split("/")[1] || "")
        .map((n) => n.toUpperCase())
        .filter((n) => n && ["RTX", "RED", "ULPFEC", "FLEXFEC-03"].indexOf(n) < 0);
      return Array.from(new Set(names));
    } catch (_e) {
      return [];
    }
  }

  /** Join words as "A, B and C". */
  function listWords(words) {
    if (words.length <= 1) return words.join("");
    return words.slice(0, -1).join(", ") + " and " + words[words.length - 1];
  }

  /** Add the one explanation a codec failure needs, when it is the likely one. */
  function withCodecHint(message) {
    const codecs = webrtcCodecs();
    if (!codecs.length || codecs.indexOf("H264") >= 0) return message;
    return `${message} This window plays ${listWords(codecs)} over WebRTC, not H.264. ` +
      "Use RTSP for an H.264 camera, or have the server send VP8 or VP9.";
  }

  function waitForIce(pc, ms) {
    if (pc.iceGatheringState === "complete") return Promise.resolve();
    return new Promise((resolve) => {
      let timer = null;
      const check = () => { if (pc.iceGatheringState === "complete") done(); };
      const done = () => {
        clearTimeout(timer);
        pc.removeEventListener("icegatheringstatechange", check);
        resolve();
      };
      timer = setTimeout(done, ms);
      pc.addEventListener("icegatheringstatechange", check);
    });
  }

  /** POST JSON; resolves {status, data} whatever the status, rejects on no answer. */
  function postJson(url, body, timeoutMs, keepalive) {
    const ac = typeof AbortController === "function" ? new AbortController() : null;
    const timer = timeoutMs ? setTimeout(() => { if (ac) ac.abort(); }, timeoutMs) : null;
    return fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal: ac ? ac.signal : undefined,
      keepalive: !!keepalive,
    }).then((res) => res.json().catch(() => null).then((data) => ({ status: res.status, data })))
      .finally(() => { if (timer) clearTimeout(timer); });
  }

  function closeSession(token, keepalive) {
    if (!token) return;
    postJson("/api/video/webrtc/close", { session: token }, 5000, keepalive).catch(() => {});
  }

  /** Drop the peer connection and its session. Late callbacks from it are ignored. */
  function rtcTeardown(rec) {
    if (rec.retryTimer) { clearTimeout(rec.retryTimer); rec.retryTimer = null; }
    rec.rtcGen += 1;
    const pc = rec.pc;
    rec.pc = null;
    if (pc) {
      pc.ontrack = null;
      pc.onconnectionstatechange = null;
      try { pc.close(); } catch (_e) {}
    }
    if (rec.session) closeSession(rec.session);
    rec.session = null;
  }

  /** Give up on this attempt, say why, and try again after a pause. */
  function rtcRetry(rec, gen, message) {
    if (rec.stopped || gen !== rec.rtcGen) return;
    rtcTeardown(rec);
    rec.rtcFailures += 1;
    const wait = backoff(rec.rtcFailures);
    showState(rec, { state: "error", message });
    rec.retryTimer = setTimeout(() => { rec.retryTimer = null; rtcConnect(rec); }, wait);
  }

  async function rtcConnect(rec) {
    rtcTeardown(rec);
    if (rec.stopped) return;
    const gen = ++rec.rtcGen;
    rec.rtcStartedAt = Date.now();
    rec.rtcFrameAt = 0;
    if (!webrtcAvailable()) {
      showState(rec, { state: "error", message: "WebRTC is not available in this window." });
      return;
    }
    showState(rec, { state: "starting", message: "Connecting to the camera." });

    let pc;
    try {
      // No STUN or TURN: the field is offline, and a camera on the same
      // network is reached by its local address.
      pc = new window.RTCPeerConnection({ iceServers: [], bundlePolicy: "max-bundle" });
    } catch (err) {
      rtcRetry(rec, gen, `WebRTC could not be started: ${(err && err.message) || err}`);
      return;
    }
    rec.pc = pc;
    try {
      // Video only. A camera's audio is not worth an autoplay prompt.
      pc.addTransceiver("video", { direction: "recvonly" });
      pc.ontrack = (e) => {
        if (gen !== rec.rtcGen || rec.stopped) return;
        const stream = (e.streams && e.streams[0]) || new window.MediaStream([e.track]);
        if (rec.videoEl.srcObject !== stream) {
          rec.videoEl.srcObject = stream;
          rec.rtcFrames = 0;
        }
        const playing = rec.videoEl.play && rec.videoEl.play();
        if (playing && typeof playing.catch === "function") playing.catch(() => {});
      };
      pc.onconnectionstatechange = () => {
        if (gen !== rec.rtcGen || rec.stopped) return;
        const s = pc.connectionState;
        if (s === "failed") rtcRetry(rec, gen, "The video connection failed. Reconnecting.");
        else if (s === "disconnected" && rec.state === "live") {
          showState(rec, { state: "stalled", message: "The video connection is interrupted." });
        }
      };
      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);
      await waitForIce(pc, RTC_GATHER_MS);
    } catch (err) {
      rtcRetry(rec, gen, `WebRTC could not be started: ${(err && err.message) || err}`);
      return;
    }
    if (gen !== rec.rtcGen || rec.stopped) return;

    let res;
    try {
      res = await postJson("/api/video/webrtc/offer",
        { id: rec.streamId, sdp: pc.localDescription.sdp }, RTC_OFFER_TIMEOUT_MS);
    } catch (_e) {
      rtcRetry(rec, gen, "Corvus is not answering.");
      return;
    }
    const data = res.data || {};
    if (gen !== rec.rtcGen || rec.stopped) {
      closeSession(data.session);
      return;
    }
    if (res.status === 404) {
      showState(rec, { state: "error", message: "This camera is no longer set up." });
      return;
    }
    if (!data.ok) {
      const error = data.error || "The camera server refused the connection.";
      rtcRetry(rec, gen, /codec/i.test(error) ? withCodecHint(error) : error);
      return;
    }
    rec.session = data.session;
    try {
      await pc.setRemoteDescription({ type: "answer", sdp: data.sdp });
    } catch (_e) {
      rtcRetry(rec, gen, withCodecHint("The camera server's answer could not be used."));
    }
  }

  function decodedFrames(video) {
    if (typeof video.getVideoPlaybackQuality === "function") {
      const q = video.getVideoPlaybackQuality();
      return (q && q.totalVideoFrames) || 0;
    }
    return Number(video.webkitDecodedFrameCount) || 0;
  }

  /**
   * Once a second: is the picture moving? A peer connection can stay
   * "connected" while the camera sends nothing, so the frames themselves are
   * the test, not the connection state.
   */
  function rtcWatch(rec) {
    if (rec.stopped || rec.retryTimer || !rec.pc) return;
    const now = Date.now();
    // A background tab may not decode at all; that is not the camera's fault.
    if (hidden()) { rec.rtcFrameAt = now; rec.rtcTickAt = now; return; }
    const v = rec.videoEl;
    const n = decodedFrames(v);
    if (n > rec.rtcFrames) {
      const dt = rec.rtcTickAt ? (now - rec.rtcTickAt) / 1000 : 0;
      if (dt > 0) rec.fps = (n - rec.rtcFrames) / dt;
      rec.rtcFrames = n;
      rec.rtcFrameAt = now;
      rec.hasFrame = true;
      rec.rtcFailures = 0;
      rec.width = v.videoWidth || rec.width || 0;
      rec.height = v.videoHeight || rec.height || 0;
      if (rec.state !== "live") showState(rec, { state: "live" });
    } else {
      const since = now - (rec.rtcFrameAt || rec.rtcStartedAt || now);
      if (rec.state === "live" && since > RTC_STALL_MS) {
        showState(rec, { state: "stalled", message: `No picture for ${Math.round(since / 1000)} s.` });
      }
      if (since > RTC_RESTART_MS) {
        rtcRetry(rec, rec.rtcGen, rec.hasFrame
          ? "No picture for 10 s. Reconnecting."
          : withCodecHint("No picture after 10 s. Check the address and the network."));
      }
    }
    rec.rtcTickAt = now;
  }

  // ---- the player: the picture inside a frame, or inside a pop-out -------------

  /** Every picture that is playing, in any frame of this page. */
  const players = new Set();

  /**
   * Put one camera's picture into `host` and start it. Used by the frames
   * here and by the pop-out page, so a camera looks and behaves the same in
   * both.
   *
   * @param {HTMLElement} host the element the picture fills
   * @param {Object} stream {id, name, address, kind}
   * @param {Function} setStatus (text, tone, message) for the pill
   * @returns {Object} the player: {stop(), snapshot(), ...its state}
   */
  function attach(host, stream, setStatus) {
    const s = stream || {};
    const webrtc = s.kind === "webrtc";
    const title = String(s.name || s.address || "Camera");
    const p = {
      bodyEl: host,
      setStatus,
      streamId: s.id,
      webrtc,
      after: 0,
      stopped: false,
      hasFrame: false,
      state: "",
      message: "",
      fps: 0,
      rtcGen: 0,
      rtcFailures: 0,
      rtcFrames: 0,
    };
    let media;
    if (webrtc) {
      media = document.createElement("video");
      media.className = "video-win-video video-win-media";
      // Muted and inline so it may start without a click: the window is
      // usually opened by one, but the picture arrives seconds later.
      media.muted = true;
      media.autoplay = true;
      media.playsInline = true;
      media.setAttribute("muted", "");
      media.setAttribute("playsinline", "");
      p.videoEl = media;
    } else {
      media = document.createElement("canvas");
      media.className = "video-win-canvas video-win-media";
      p.canvasEl = media;
    }
    media.setAttribute("aria-label", `Picture from ${title}`);
    const msg = document.createElement("div");
    msg.className = "video-win-msg";
    host.appendChild(media);
    host.appendChild(msg);
    p.msgEl = msg;
    showState(p, { state: "starting" });

    p.stop = function () {
      if (p.stopped) return;
      p.stopped = true;
      players.delete(p);
      if (p.abort) { try { p.abort.abort(); } catch (_e) {} }
      if (p.frameTimer) clearTimeout(p.frameTimer);
      if (p.watchTimer) clearInterval(p.watchTimer);
      if (p.webrtc) {
        rtcTeardown(p);
        if (p.videoEl) { try { p.videoEl.srcObject = null; } catch (_e) {} }
      }
    };
    p.snapshot = function () {
      return {
        state: p.state || "starting",
        message: p.message || "",
        width: p.width || 0,
        height: p.height || 0,
        fps: Math.round((p.fps || 0) * 10) / 10,
      };
    };

    players.add(p);
    if (webrtc) {
      rtcConnect(p);
      p.watchTimer = setInterval(() => rtcWatch(p), WATCH_MS);
    } else {
      pump(p);
    }
    return p;
  }

  // ---- the window ------------------------------------------------------------

  function popouts() {
    return (window.Corvus && Corvus.popouts) || null;
  }

  /**
   * Take a camera out of the app into a window of its own, opened where the
   * frame was. The frame closes once the new window exists; if the window
   * could not be opened (a browser that blocks pop-ups), the frame stays.
   */
  function popOut(stream, rec) {
    const po = popouts();
    if (!po) return false;
    const rect = rec.el.getBoundingClientRect();
    const ok = po.open(outSpec(stream), rect);
    if (ok) F.close(keyOf(stream.id));
    else notify("warning", "The camera window could not be opened. Allow pop-up windows for Corvus.");
    return ok;
  }

  /** What a camera's window out of the app is opened with. */
  function outSpec(stream) {
    return {
      key: keyOf(stream.id),
      kind: KIND,
      params: { id: stream.id, name: stream.name, address: stream.address, stream: stream.kind },
    };
  }

  /**
   * Open (or raise) the window for one camera. A camera already out in a
   * window of its own is brought to the front there instead.
   * @param {Object} stream {id, name, address, kind}
   * @param {Object} [at] {left, top, width, height} in the page's pixels:
   *        where a window coming back into the app was let go of
   * @returns {boolean} whether a window is now showing it
   */
  function open(stream, at) {
    const s = stream || {};
    if (!s.id || typeof s.id !== "string") return false;
    if (!window.Corvus || !Corvus.ui || !F || typeof document === "undefined") return false;

    const po = popouts();
    if (po && po.isOpen(keyOf(s.id))) {
      po.focus(keyOf(s.id));
      return true;
    }
    const existing = F.get(keyOf(s.id));
    if (existing) {
      F.raise(existing);
      return true;
    }

    // The desktop app's default: a window of its own at once, where the frame
    // would have opened. A window coming back into the app (`at`) is a frame.
    if (po && !at && po.opensOutside()
        && po.openNative(outSpec(s), F.placement(keyOf(s.id), DEF_SIZE))) {
      return true;
    }

    const title = String(s.name || s.address || "Camera");
    // The desktop app lets a frame be dragged out of its window; a browser
    // cannot, and gets a button that opens a pop-up instead. So does the
    // desktop app where it may not place its windows (Wayland).
    const native = !!(po && po.native());
    const rec = F.open({
      key: keyOf(s.id),
      kind: KIND,
      at,
      title,
      subtitle: String(s.address || ""),
      icon: "video",
      noun: "camera",
      ariaLabel: `Camera: ${title}`,
      className: "term-win--video",
      bodyClass: "video-win-body",
      status: PILL.starting[0],
      statusTone: PILL.starting[1],
      closeTitle: "Close the camera window",
      size: DEF_SIZE,
      onPopOut: po && (!native || !po.canPlace()) ? (r) => popOut(s, r) : null,
      onDragOut: po ? (r, size) => po.dragOut(outSpec(s), size, {
        hide: () => F.hideOut(r),
        gone: () => F.close(keyOf(s.id)),
      }) : null,
      onClose: (r) => {
        if (r.player) r.player.stop();
        emit();
      },
      mount: (r) => {
        r.player = attach(r.bodyEl, s, (text, tone, message) => F.setStatus(r, text, tone, message));
        // A double-click on the picture maximizes too: it is most of the
        // window, and the bar is a small target on a touch screen.
        r.bodyEl.addEventListener("dblclick", () => F.toggleMax(r));
      },
    });
    if (!rec) return false;
    emit();
    return true;
  }

  /** Close one camera's window, in the app or out of it. */
  function close(id) {
    const po = popouts();
    if (po && po.isOpen(keyOf(id))) po.close(keyOf(id));
    return F.close(keyOf(id));
  }

  /** Every camera window, gone, including the ones out in windows of their own. */
  function closeAll() {
    F.keys(KIND).forEach((key) => F.close(key));
    const po = popouts();
    if (po) po.closeKind(KIND);
  }

  /** @returns {boolean} whether this camera has a window open, in the app or out of it. */
  function has(id) {
    const po = popouts();
    return F.has(keyOf(id)) || !!(po && po.isOpen(keyOf(id)));
  }

  /** @returns {number} how many camera windows are open, in the app or out of it. */
  function count() {
    const po = popouts();
    return F.count(KIND) + (po ? po.count(KIND) : 0);
  }

  /**
   * What a camera window is showing, or null when none is open. The Video
   * page reads a WebRTC camera's state from here, because only the window
   * knows it; a popped-out window reports its state once a second.
   * @param {string} id
   * @returns {{state, message, width, height, fps}|null}
   */
  function stateOf(id) {
    const rec = F.get(keyOf(id));
    if (rec && rec.player) return rec.player.snapshot();
    const po = popouts();
    return po ? po.stateOf(keyOf(id)) : null;
  }

  /**
   * Called with the number of open camera windows whenever it changes.
   * @param {Function} fn
   * @returns {Function} unsubscribe
   */
  function onChange(fn) {
    if (typeof fn !== "function") return () => {};
    listeners.add(fn);
    return () => listeners.delete(fn);
  }

  /**
   * Record the camera list from a GET /api/video/status answer (or its
   * `streams`). Whoever reads the status passes it on, so the map's camera
   * button follows the Video page without polling on its own.
   * @param {Object|Array} data the status, or its streams
   */
  function setCameras(data) {
    const list = Array.isArray(data) ? data : ((data && Array.isArray(data.streams)) ? data.streams : null);
    if (!list) return;
    const next = list
      .filter((s) => s && typeof s.id === "string" && s.id)
      .map((s) => ({ id: s.id, name: s.name || "", address: s.address || "", kind: s.kind || "rtsp" }));
    if (known && JSON.stringify(known) === JSON.stringify(next)) return;
    known = next;
    cameraListeners.forEach((fn) => { try { fn(cameras()); } catch (_e) {} });
  }

  /** @returns {Object[]} the cameras set up, [{id, name, address, kind}] */
  function cameras() {
    return (known || []).map((c) => Object.assign({}, c));
  }

  function readStatus() {
    const t = Corvus.telemetry;
    const req = t && typeof t.requestJson === "function"
      ? t.requestJson("/api/video/status")
      : fetch("/api/video/status").then((r) => r.json());
    return Promise.resolve(req).then((data) => { setCameras(data); return data; });
  }

  /**
   * Read the camera list now. Resolves with it; a failed read keeps the last one.
   * @returns {Promise<Object[]>}
   */
  function loadCameras() {
    return readStatus().then(() => cameras(), () => cameras());
  }

  /**
   * Called with the camera list whenever a camera is added, renamed or removed.
   * @param {Function} fn
   * @returns {Function} unsubscribe
   */
  function onCameras(fn) {
    if (typeof fn !== "function") return () => {};
    cameraListeners.add(fn);
    return () => cameraListeners.delete(fn);
  }

  /** Open one camera's window, or close it when it is open. */
  function toggle(stream) {
    const s = stream || {};
    if (!s.id) return false;
    if (has(s.id)) {
      close(s.id);
      return false;
    }
    return open(s);
  }

  /**
   * The one-press camera button: close every camera window when any is
   * open, otherwise open one per configured camera. With none configured it
   * says where to add one rather than doing nothing.
   * @returns {Promise<number>} how many windows are open afterwards
   */
  function toggleAll() {
    if (count() > 0) {
      closeAll();
      return Promise.resolve(0);
    }
    return readStatus().then((data) => {
      const streams = (data && Array.isArray(data.streams)) ? data.streams : [];
      if (!streams.length) {
        notify("info", "No camera is set up yet. Add one under Setup, Video.");
        return 0;
      }
      const needsDecoder = streams.some((s) => s.kind !== "webrtc");
      if (data.available === false && data.reason && needsDecoder) notify("warning", data.reason);
      streams.forEach((s) => open(s));
      return count();
    }).catch(() => {
      notify("warning", "The camera list could not be read.");
      return count();
    });
  }

  function notify(level, message) {
    if (Corvus.ui && typeof Corvus.ui.toast === "function") {
      Corvus.ui.toast({ level, title: "Camera", message });
    }
  }

  // A popped-out camera asking to come back, and the set of popped-out
  // windows changing: both are news for whoever counts camera windows.
  if (popouts()) {
    popouts().onDock(KIND, (payload) => {
      if (payload && payload.stream) open(payload.stream, payload.at || null);
    });
    popouts().onChange(emit);
  }

  // A page being closed or reloaded ends its WebRTC sessions on the camera's
  // server now, rather than leaving them to its timeout. keepalive lets the
  // request outlive the page.
  if (typeof window.addEventListener === "function") {
    window.addEventListener("pagehide", () => {
      players.forEach((p) => { if (p.session) closeSession(p.session, true); });
    });
  }

  return {
    open, close, closeAll, has, count, stateOf, onChange, toggle, toggleAll, showState, attach,
    cameras, setCameras, loadCameras, onCameras,
    webrtcAvailable, webrtcCodecs, DEF_SIZE,
  };
})();
