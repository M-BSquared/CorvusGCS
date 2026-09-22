"use strict";
/*
  One SSE connection for every stream that is not telemetry or SSH.

  A browser caps concurrent HTTP/1.1 requests per origin at six. This app used
  to open six separate EventSources — telemetry, the MAVLink console, parameter
  progress, tile-download progress, firmware progress and an SSH shell — and
  could hold five of them at once. That leaves ONE connection for every map
  tile, every fetch and every plugin asset. A viewport is dozens of tiles; they
  queue behind each other one at a time, and the map crawls at exactly the
  moment — pre-flight setup with a region downloading — the operator is
  busiest. Nothing errors. It is just slow, in the least diagnosable way there
  is.

  So console, params, firmware and tiles share this one `/api/events` stream,
  each arriving under its own event name. Worst case is now three connections
  (telemetry, this, SSH) instead of five, and three are left for everything
  else.

  Two streams deliberately stay outside it:

  * `/api/telemetry` — the 50 Hz path, open for the whole session, with its own
    version-keyed snapshot cache on the server. It is also the one stream whose
    stutter an operator would read as the aircraft misbehaving, and this module
    reconnects by design (see below). It must never be affected by a panel
    opening.
  * `/api/ssh/stream` — per session, high-volume, and it ends when its shell
    does.

  ## Why topics are sticky

  Changing the topic set means a new URL, and a new URL means closing this
  EventSource and opening another. That blip is invisible for a coalescing
  topic (params, tiles: the next event carries the current state) but it can
  drop a console line, and the calibration wizard reads the console.

  So a topic that has been asked for once stays in the URL for the life of the
  page, even after its last listener goes away. Dispatch is still
  reference-counted — nothing is delivered to a consumer that unsubscribed —
  but no reconnect happens when a panel closes. A session therefore reconnects
  a handful of times at most, as each topic is first used, and then holds one
  stream for good.

  The exception is `tiles`, which follows a single download job: a new job is a
  new `job` parameter and does force a reopen. Starting a region download is a
  deliberate, occasional act, and tiles coalesces, so that reconnect costs
  nothing but the round trip.
*/
window.Corvus = window.Corvus || {};
Corvus.events = (function () {
  /** topic -> Set of handlers currently listening. */
  const subs = new Map();
  /** Topics ever asked for. Sticky — see the note above. */
  const topics = new Set();
  let source = null;
  /** Bumped on every reopen so a late event from a closed stream is dropped. */
  let generation = 0;
  /** The download job the `tiles` topic follows, or null. */
  let jobId = null;

  function deliver(topic, data) {
    const handlers = subs.get(topic);
    if (!handlers) return;
    // Guarded per handler, exactly as telemetry.js guards its subscribers: one
    // panel throwing must cost only itself, never the other panels' events.
    handlers.forEach((fn) => {
      try { fn(data); } catch (err) {
        console.error(`${topic} event handler failed:`, err);
      }
    });
  }

  function streamUrl() {
    const list = Array.from(topics).join(",");
    const job = topics.has("tiles") && jobId
      ? `&job=${encodeURIComponent(jobId)}` : "";
    return `/api/events?topics=${encodeURIComponent(list)}${job}`;
  }

  function close() {
    if (!source) return;
    generation++;
    try { source.close(); } catch (_err) { /* already gone */ }
    source = null;
  }

  function open() {
    if (!topics.size) return;
    close();
    const gen = ++generation;
    try {
      source = new EventSource(streamUrl());
    } catch (_err) {
      // No EventSource at all. Every consumer of this module has its own
      // fallback (a poll, or a result endpoint), so this is quiet on purpose.
      source = null;
      return;
    }
    topics.forEach((topic) => {
      source.addEventListener(topic, (e) => {
        if (gen !== generation) return;
        let data;
        try { data = JSON.parse(e.data); } catch (_err) { return; }
        deliver(topic, data);
      });
    });
    // The server sends these when a topic has been quiet for 15 s, to keep the
    // connection from being reaped. Nothing to do with them.
    source.addEventListener("ping", () => {});
    // EventSource reconnects on its own. Consumers that need to know a gap
    // happened (the console announces one) subscribe to "error" below.
    source.onerror = () => {
      if (gen !== generation) return;
      deliver("error", { topics: Array.from(topics) });
    };
  }

  /**
   * Point the `tiles` topic at a download job. Passing null stops following.
   * Forces a reopen when it changes, because the job is part of the URL.
   * @param {string|null} id
   */
  function followJob(id) {
    const next = id || null;
    if (next === jobId) return;
    jobId = next;
    if (topics.has("tiles") && source) open();
  }

  /**
   * Listen to one topic: "console", "params", "firmware", "tiles" — or
   * "error", which fires when the underlying stream drops and is about to
   * reconnect.
   * @param {string} topic
   * @param {function(Object):void} handler
   * @returns {function():void} unsubscribe — idempotent.
   */
  function subscribe(topic, handler) {
    if (typeof handler !== "function") return function () {};
    if (!subs.has(topic)) subs.set(topic, new Set());
    subs.get(topic).add(handler);
    // "error" is local to this module; it is not a server topic and must not
    // go into the URL.
    if (topic !== "error" && !topics.has(topic)) {
      topics.add(topic);
      open();
    } else if (!source) {
      open();
    }
    let released = false;
    return function () {
      if (released) return;
      released = true;
      const handlers = subs.get(topic);
      if (handlers) handlers.delete(handler);
    };
  }

  /**
   * Close the stream and forget everything subscribed to it.
   *
   * Used by `pagehide`, so a reload leaves no socket and no handler holding a
   * reference into a document that is going away. A full stop rather than just
   * a close: a half-stopped module that still counts listeners for a dead page
   * is how a "why is this panel still updating?" bug starts.
   */
  function stop() {
    close();
    subs.clear();
    topics.clear();
    jobId = null;
  }

  /**
   * How many handlers a topic currently has.
   *
   * The question "did that page let go of its stream?" used to be answered by
   * looking at an EventSource's `closed`. There is no per-consumer socket any
   * more, so this is the honest form of it: zero here is what stops a torn-down
   * page being handed another event. Also the first thing to look at when a
   * panel keeps updating after it should have stopped.
   * @param {string} topic
   * @returns {number}
   */
  function listenerCount(topic) {
    const handlers = subs.get(topic);
    return handlers ? handlers.size : 0;
  }

  return { subscribe, followJob, stop, listenerCount };
})();
