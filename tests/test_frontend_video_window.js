"use strict";

/**
 * Floating camera windows (Corvus.videoWindows) and the Video setup page's
 * pure helpers (Corvus.setupVideo).
 *
 * The camera window is the SSH Launcher's terminal window with a picture in
 * it: the same frame from Corvus.floatWindows, in the same layer. So the first
 * half of this file asserts exactly that, down to the class names and the
 * gestures, and that a camera and a terminal stack as one set of windows.
 *
 * The second half is what fills it. For RTSP, the frame loop: one request at
 * a time, the frame number carried forward, the state on the pill when there
 * is no picture, and no request at all once the window is closed, because that
 * silence is what stops the camera's decoder on the backend. For WebRTC, the
 * peer connection: the offer through the backend, a session token and nothing
 * more in the window, "live" only once frames actually move, a reconnect when
 * they stop, and an explanation when the camera's codec is one this window
 * cannot play.
 *
 * The last part is the way out of the app's window: the pop-out button opens
 * the frame's content in a real window (window.open), the app keeps counting
 * it through its heartbeat even after a reload, and "put it back" and the
 * map's camera button both reach it there.
 *
 * Run:
 *   node tests/test_frontend_video_window.js
 */

const assert = require("node:assert/strict");

global.window = global;
global.Corvus = {};

let VIEW = { width: 1400, height: 900 };

const windowListeners = {};
window.addEventListener = (t, cb) => { (windowListeners[t] = windowListeners[t] || []).push(cb); };
window.removeEventListener = () => {};
global.CustomEvent = class CustomEvent {
  constructor(type, o = {}) { this.type = type; this.detail = o.detail; }
};
global.localStorage = { getItem: () => null, setItem: () => {}, removeItem: () => {} };

// ---------------------------------------------------------------------------
// DOM stub, the same shape as tests/test_frontend_term_window.js.
// ---------------------------------------------------------------------------
function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "",
    textContent: "",
    children: [],
    style: {},
    dataset: {},
    disabled: false,
    hidden: false,
    parentNode: null,
    _attrs: {},
    _listeners: {},
    _isEl: true,
  };
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) { const has = e.classList.contains(c); const next = force === undefined ? !has : !!force; if (next) e.classList.add(c); else e.classList.remove(c); return next; },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => { if (c.parentNode) c.parentNode.removeChild(c); c.parentNode = e; e.children.push(c); return c; };
  e.append = (...cs) => cs.forEach((c) => e.appendChild(c));
  e.insertBefore = (c, ref) => {
    if (c.parentNode) c.parentNode.removeChild(c);
    const i = ref ? e.children.indexOf(ref) : -1;
    c.parentNode = e;
    if (i < 0) e.children.push(c); else e.children.splice(i, 0, c);
    return c;
  };
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; return c; };
  Object.defineProperty(e, "firstChild", { get() { return e.children[0] || null; } });
  Object.defineProperty(e, "lastChild", { get() { return e.children[e.children.length - 1] || null; } });
  Object.defineProperty(e, "clientWidth", {
    get() { return e.classList.contains("term-layer") ? VIEW.width : (parseFloat(e.style.width) || 0); },
  });
  Object.defineProperty(e, "clientHeight", {
    get() { return e.classList.contains("term-layer") ? VIEW.height : (parseFloat(e.style.height) || 0); },
  });
  Object.defineProperty(e, "offsetWidth", { get() { return e.clientWidth; } });
  Object.defineProperty(e, "offsetHeight", { get() { return e.clientHeight; } });
  e.getBoundingClientRect = () => ({
    left: parseFloat(e.style.left) || 0, top: parseFloat(e.style.top) || 0,
    width: e.offsetWidth, height: e.offsetHeight,
  });
  e.contains = (node) => node === e || e.children.some((c) => c._isEl && c.contains(node));
  Object.defineProperty(e, "isConnected", {
    get() { let cur = e; while (cur) { if (cur === body) return true; cur = cur.parentNode; } return false; },
  });
  e.closest = (sel) => {
    const want = sel.replace(/^\./, "");
    let cur = e;
    while (cur) { if (cur.classList && cur.classList.contains(want)) return cur; cur = cur.parentNode; }
    return null;
  };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); if (k === "class") e.className = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.removeAttribute = (k) => { delete e._attrs[k]; };
  e.addEventListener = (type, cb) => { (e._listeners[type] = e._listeners[type] || []).push(cb); };
  e.removeEventListener = () => {};
  e.setPointerCapture = () => {};
  e.releasePointerCapture = () => {};
  e.querySelector = () => null;
  e.querySelectorAll = () => [];
  // A canvas: remembers what was drawn on it.
  e.draws = 0;
  e.getContext = () => ({ drawImage() { e.draws += 1; } });
  // A <video>: counts the frames a test says it decoded.
  e.frames = 0;
  e.videoWidth = 1280;
  e.videoHeight = 720;
  e.play = () => Promise.resolve();
  e.getVideoPlaybackQuality = () => ({ totalVideoFrames: e.frames });
  return e;
}

const body = makeEl("body");
global.document = {
  createElement: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  getElementById: () => null,
  querySelector: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  removeEventListener: () => {},
  body,
};

function fire(el, type, event) {
  (el._listeners[type] || []).slice().forEach((cb) => cb(Object.assign({
    type, button: 0, pointerId: 1, target: el, preventDefault() {},
  }, event)));
}

// ---------------------------------------------------------------------------
// The network: a queue of answers per frame request, and a record of requests.
// ---------------------------------------------------------------------------
const requests = [];
let answers = [];           // what the next frame requests get, in order
let pending = null;         // a request with no answer yet resolves here
let statusAnswer = { streams: [] };

function imageAnswer(seq) {
  return {
    ok: true, status: 200,
    headers: { get: (k) => ({ "Content-Type": "image/jpeg", "X-Video-Seq": String(seq) })[k] || null },
    blob: () => Promise.resolve({ size: 10 }),
  };
}
function jsonAnswer(data, status = 200) {
  return {
    ok: status < 400, status,
    headers: { get: (k) => (k === "Content-Type" ? "application/json" : null) },
    json: () => Promise.resolve(data),
  };
}

const rtcRequests = [];
let rtcAnswers = [];
global.fetch = (url, init) => {
  if (String(url).startsWith("/api/video/status")) {
    return Promise.resolve(jsonAnswer(statusAnswer));
  }
  if (String(url).startsWith("/api/video/webrtc/")) {
    rtcRequests.push({ url: String(url), body: JSON.parse(init.body), keepalive: !!init.keepalive });
    if (String(url).endsWith("/close")) return Promise.resolve(jsonAnswer({ ok: true, closed: true }));
    const next = rtcAnswers.length ? rtcAnswers.shift()
      : jsonAnswer({ ok: true, sdp: "v=0 answer", session: "tok-" + rtcRequests.length });
    return Promise.resolve(next);
  }
  const req = { url: String(url), init, aborted: false };
  requests.push(req);
  if (init && init.signal) init.signal.addEventListener("abort", () => { req.aborted = true; });
  if (answers.length) return Promise.resolve(answers.shift());
  return new Promise((resolve) => { pending = resolve; });
};
global.createImageBitmap = () => Promise.resolve({ width: 1280, height: 720, close() {} });

// WebRTC, faked: a peer connection that records what it was told, and a
// codec list a test can change.
const pcs = [];
let codecList = ["VP8", "VP9", "AV1"];
let failRemote = false;
class FakePC {
  constructor(cfg) {
    this.cfg = cfg;
    this.iceGatheringState = "complete";
    this.connectionState = "new";
    this.closed = false;
    this.transceivers = [];
    pcs.push(this);
  }
  addTransceiver(kind, init) { this.transceivers.push([kind, init]); }
  createOffer() { return Promise.resolve({ type: "offer", sdp: "v=0 offer" }); }
  setLocalDescription(d) { this.localDescription = d; return Promise.resolve(); }
  setRemoteDescription(d) {
    if (failRemote) return Promise.reject(new Error("no common codec"));
    this.remoteDescription = d;
    return Promise.resolve();
  }
  addEventListener() {}
  removeEventListener() {}
  close() { this.closed = true; }
}
global.RTCPeerConnection = FakePC;
global.RTCRtpReceiver = {
  getCapabilities: () => ({ codecs: codecList.concat(["rtx"]).map((m) => ({ mimeType: "video/" + m })) }),
};
global.MediaStream = class { constructor(tracks) { this.tracks = tracks; } };
// The frame watch runs once a second; the tests run it by hand instead.
const intervals = new Map();
let intervalId = 0;
global.setInterval = (fn) => { intervalId += 1; intervals.set(intervalId, fn); return intervalId; };
global.clearInterval = (id) => { intervals.delete(id); };
function tick() { Array.from(intervals.values()).forEach((fn) => fn()); }
let NOW = 1e12;
Date.now = () => NOW;

const toasts = [];

require("../src/js/ui.js");
Corvus.ui.toast = (o) => { toasts.push(o); return { el: makeEl("div"), close() {} }; };
// window.open, recorded: the pop-out button's only way out of the page.
const opened = [];
global.open = (url, name, features) => {
  const win = { url, name, features, closed: false, focused: 0, focus() { win.focused += 1; } };
  opened.push(win);
  return win;
};
global.screenX = 100; global.screenY = 50;
global.outerWidth = 1400; global.innerWidth = 1400;
global.outerHeight = 928; global.innerHeight = 900;

require("../src/js/float-window.js");
require("../src/js/popout.js");
require("../src/js/term-window.js");
require("../src/js/video-window.js");
require("../src/js/setup-video.js");
require("../src/js/popout-page.js");

const vw = Corvus.videoWindows;
const tw = Corvus.termWindows;
const shells = [];
Corvus.sshTerm = {
  available: () => true,
  ensure: () => Promise.resolve(true),
  create: (container, session) => {
    const h = { session, focused: 0, disposed: false,
      fit() {}, focus() { h.focused += 1; }, dispose() { h.disposed = true; } };
    shells.push(h);
    return h;
  },
};

function flush(n = 6) {
  let p = Promise.resolve();
  for (let i = 0; i < n; i += 1) p = p.then(() => new Promise((r) => setImmediate(r)));
  return p;
}

function layer() { return body.children.find((c) => c.classList.contains("term-layer")); }
function frames() { return layer() ? layer().children : []; }
function frameOf(label) {
  return frames().find((f) => f.getAttribute("aria-label") === label);
}
function partOf(frame, cls) {
  const walk = (el) => {
    for (const c of el.children) {
      if (c._isEl && c.classList.contains(cls)) return c;
      if (c._isEl) { const hit = walk(c); if (hit) return hit; }
    }
    return null;
  };
  return walk(frame);
}
function toolOf(frame, ariaLabel) {
  const walk = (el) => {
    for (const c of el.children) {
      if (c._isEl && c.getAttribute("aria-label") === ariaLabel) return c;
      if (c._isEl) { const hit = walk(c); if (hit) return hit; }
    }
    return null;
  };
  return walk(partOf(frame, "term-win-bar"));
}

async function reset() {
  vw.closeAll();
  tw.closeAll();
  Corvus.popouts.closeKind("terminal");
  opened.length = 0;
  if (pending) { pending(jsonAnswer({ state: "idle", seq: 0 })); pending = null; }
  await flush();
  requests.length = 0;
  answers = [];
  toasts.length = 0;
  statusAnswer = { streams: [] };
  rtcRequests.length = 0;
  rtcAnswers = [];
  pcs.length = 0;
  codecList = ["VP8", "VP9", "AV1"];
  failRemote = false;
  VIEW = { width: 1400, height: 900 };
}

const CAM = { id: "cam1", name: "Gimbal", address: "192.168.144.25:8554/main.264" };
const CAM2 = { id: "cam2", name: "Belly", address: "10.0.0.3/x" };

// ---------------------------------------------------------------------------
// The same window as a terminal
// ---------------------------------------------------------------------------

async function testTheCameraWindowIsTheTerminalWindowsFrame() {
  await reset();
  assert.equal(vw.open(CAM), true);
  const cam = frameOf("Camera: Gimbal");
  assert.ok(cam, "the window is a labelled dialog");
  tw.open({ name: "ssh-launcher/a", title: "Start mission", host: "10.0.0.7" });
  const term = frameOf("Terminal: Start mission");

  for (const cls of ["term-win", "term-win-bar", "term-win-icon", "term-win-info",
    "term-win-name", "term-win-host", "ssh-status", "term-win-tools", "term-win-body",
    "term-win-grip"]) {
    assert.ok(cam.classList.contains(cls) || partOf(cam, cls), `camera window has .${cls}`);
    assert.ok(term.classList.contains(cls) || partOf(term, cls), `terminal window has .${cls}`);
  }
  assert.equal(partOf(cam, "term-win-name").textContent, "Gimbal");
  assert.equal(partOf(cam, "term-win-host").textContent, CAM.address);
  assert.ok(toolOf(cam, "Maximize the camera"), "the same maximize button");
  assert.ok(toolOf(cam, "Close the camera window"), "and the same ×");
  assert.equal(layer().children.length, 2, "one layer holds both kinds");
}

async function testCameraAndTerminalStackAsOneSetOfWindows() {
  await reset();
  tw.open({ name: "ssh-launcher/a", title: "Start mission", host: "10.0.0.7" });
  vw.open(CAM);
  const term = frameOf("Terminal: Start mission");
  const cam = frameOf("Camera: Gimbal");
  assert.equal(layer().lastChild, cam, "the camera opened last and is in front");
  fire(term, "pointerdown", { target: partOf(term, "term-win-body") });
  assert.equal(layer().lastChild, term, "a press on the terminal brings it above the camera");
  const camRect = [cam.style.left, cam.style.top];
  const termRect = [term.style.left, term.style.top];
  assert.notDeepEqual(camRect, termRect, "the camera does not open exactly on the terminal");
}

async function testTheCameraWindowMovesAndMaximizesLikeATerminal() {
  await reset();
  vw.open(CAM);
  const cam = frameOf("Camera: Gimbal");
  const x0 = parseFloat(cam.style.left);
  const bar = partOf(cam, "term-win-bar");
  fire(cam, "pointerdown", { target: bar, clientX: 500, clientY: 300 });
  fire(cam, "pointermove", { clientX: 440, clientY: 300 });
  fire(cam, "pointerup", {});
  assert.equal(parseFloat(cam.style.left), x0 - 60, "dragged by its bar");

  fire(toolOf(cam, "Maximize the camera"), "click");
  assert.equal(cam.classList.contains("is-max"), true);
  fire(toolOf(cam, "Restore the camera"), "click");
  assert.equal(cam.classList.contains("is-max"), false);

  fire(partOf(cam, "video-win-body"), "dblclick");
  assert.equal(cam.classList.contains("is-max"), true, "a double-click on the picture maximizes too");
}

async function testOneWindowPerCamera() {
  await reset();
  vw.open(CAM);
  vw.open(CAM2);
  assert.equal(vw.count(), 2);
  vw.open(CAM);
  assert.equal(vw.count(), 2, "asking again raises, it does not duplicate");
  assert.equal(layer().lastChild, frameOf("Camera: Gimbal"));
  assert.equal(vw.open({ name: "no id" }), false);
}

// ---------------------------------------------------------------------------
// The frame loop
// ---------------------------------------------------------------------------

async function testFramesAreAskedForOneAtATimeAndNumbered() {
  await reset();
  answers = [
    jsonAnswer({ state: "starting", message: "Connecting to the camera.", seq: 0 }),
    imageAnswer(41),
    imageAnswer(42),
  ];
  vw.open(CAM);
  const cam = frameOf("Camera: Gimbal");
  const pill = partOf(cam, "ssh-status");
  await flush(20);

  assert.match(requests[0].url, /\/api\/video\/frame\?id=cam1&after=0$/);
  assert.match(requests[1].url, /after=0$/, "no picture yet, so still from the start");
  assert.match(requests[2].url, /after=41$/, "each request carries the last frame number");
  assert.match(requests[3].url, /after=42$/);
  assert.equal(requests.length, 4, "one request in flight, never a burst");
  assert.equal(partOf(cam, "video-win-canvas").draws, 2, "both frames were drawn");
  assert.equal(pill.lastChild.textContent, "LIVE");
  assert.equal(pill.classList.contains("connected"), true);
  assert.equal(partOf(cam, "video-win-msg").hidden, true, "nothing over a live picture");
}

async function testNoPictureShowsTheReasonAndDimsTheLastFrame() {
  await reset();
  answers = [
    imageAnswer(5),
    jsonAnswer({ state: "error", message: "Error opening input files: Connection refused", seq: 5 }),
  ];
  vw.open(CAM);
  await flush(20);
  const cam = frameOf("Camera: Gimbal");
  const pill = partOf(cam, "ssh-status");
  assert.equal(pill.lastChild.textContent, "OFFLINE");
  assert.equal(pill.classList.contains("connected"), false);
  assert.equal(pill.title, "Error opening input files: Connection refused", "the reason is on the pill");
  const msg = partOf(cam, "video-win-msg");
  assert.equal(msg.hidden, false);
  assert.match(msg.textContent, /Connection refused/);
  assert.equal(partOf(cam, "video-win-body").classList.contains("is-stale"), true,
    "a frozen frame must never look live");

  vw.showState(Corvus.floatWindows.get("video:cam1").player, { state: "starting" });
  assert.equal(pill.lastChild.textContent, "CONNECTING");
  assert.equal(pill.classList.contains("pending"), true);
}

async function testClosingTheWindowStopsAsking() {
  await reset();
  vw.open(CAM);
  await flush();
  assert.equal(requests.length, 1);
  vw.close(CAM.id);
  assert.equal(requests[0].aborted, true, "the request in flight is dropped");
  if (pending) { pending(imageAnswer(9)); pending = null; }
  await flush(20);
  assert.equal(requests.length, 1,
    "no request after close: that silence is what stops the decoder on the backend");
  assert.equal(frames().length, 0);
}

async function testACameraThatWasRemovedStopsTheLoop() {
  await reset();
  answers = [jsonAnswer({ error: "no such camera" }, 404)];
  vw.open(CAM);
  await flush(20);
  const cam = frameOf("Camera: Gimbal");
  assert.match(partOf(cam, "video-win-msg").textContent, /no longer set up/);
  assert.equal(requests.length, 1, "a camera that does not exist is not asked for again");
}

// ---------------------------------------------------------------------------
// The map's camera button
// ---------------------------------------------------------------------------

async function testTheCameraButtonOpensEveryCameraThenClosesThem() {
  await reset();
  const counts = [];
  const off = vw.onChange((n) => counts.push(n));
  statusAnswer = { available: true, streams: [CAM, CAM2] };
  assert.equal(await vw.toggleAll(), 2);
  assert.ok(frameOf("Camera: Gimbal") && frameOf("Camera: Belly"));
  assert.equal(await vw.toggleAll(), 0);
  assert.equal(vw.count(), 0);
  assert.equal(counts[counts.length - 1], 0, "the button hears that it is off again");
  assert.ok(counts.includes(2));
  off();
}

async function testTheCameraButtonSaysWhereToAddACamera() {
  await reset();
  statusAnswer = { available: true, streams: [] };
  assert.equal(await vw.toggleAll(), 0);
  assert.equal(toasts.length, 1);
  assert.match(toasts[0].message, /Setup, Video/);
  assert.doesNotMatch(toasts[0].message, /[–—]/, "no dashes in operator text");
}

async function testTheCameraListIsKeptForTheMapsButton() {
  await reset();
  const heard = [];
  const off = vw.onCameras((list) => heard.push(list.map((c) => c.id)));
  statusAnswer = { available: true, streams: [CAM, CAM2] };
  const list = await vw.loadCameras();
  assert.deepEqual(list.map((c) => c.id), [CAM.id, CAM2.id]);
  assert.deepEqual(Object.keys(list[0]).sort(), ["address", "id", "kind", "name"]);
  vw.setCameras({ streams: [CAM, CAM2] });
  assert.equal(heard.length, 1, "the same list again is not news");
  vw.setCameras({ streams: [CAM2] });
  assert.deepEqual(heard[heard.length - 1], [CAM2.id]);
  vw.setCameras(null);
  assert.deepEqual(vw.cameras().map((c) => c.id), [CAM2.id], "an unreadable answer keeps the last list");
  off();
}

async function testToggleOpensOneCameraThenClosesIt() {
  await reset();
  vw.toggle(CAM);
  assert.equal(vw.has(CAM.id), true);
  assert.ok(frameOf("Camera: Gimbal"));
  vw.toggle(CAM);
  assert.equal(vw.has(CAM.id), false);
  assert.equal(vw.count(), 0);
}

// ---------------------------------------------------------------------------
// The setup page's pure helpers
// ---------------------------------------------------------------------------

function testTheRowSaysWhatThePictureIsOrWhatWentWrong() {
  const sv = Corvus.setupVideo;
  assert.equal(sv.detailLine({ state: "live", width: 1280, height: 720, fps: 24.6 }), "1280 × 720, 25 fps");
  assert.equal(sv.detailLine({ state: "error", message: "401 Unauthorized", address: "h/x" }), "401 Unauthorized");
  assert.equal(sv.detailLine({ state: "idle", address: "10.0.0.2/main" }), "10.0.0.2/main");
}

function testThePasswordIsOnlySentWhenItWasTyped() {
  const sv = Corvus.setupVideo;
  const untouched = sv.saveBody({ id: "a", name: " Nose ", url: " rtsp://h/x ", username: "u",
    password: "", passwordTouched: false, transport: "udp" });
  assert.deepEqual(untouched,
    { id: "a", name: "Nose", kind: "rtsp", url: "rtsp://h/x", username: "u", transport: "udp" });
  assert.equal("password" in untouched, false, "an untouched field keeps the stored password");
  const typed = sv.saveBody({ url: "rtsp://h/x", password: "", passwordTouched: true });
  assert.equal(typed.password, "", "a field cleared on purpose clears it");
  assert.equal(typed.transport, "tcp");
  assert.equal("id" in typed, false, "a new camera has no id yet");
}

function testADraftWithoutAnAddressCannotBeSaved() {
  const sv = Corvus.setupVideo;
  assert.ok(sv.draftProblem({ url: "" }));
  assert.ok(sv.draftProblem({ url: "192.168.144.25:8554/main" }));
  assert.equal(sv.draftProblem({ url: "rtsp://192.168.144.25:8554/main" }), "");
}

// ---------------------------------------------------------------------------
// WebRTC
// ---------------------------------------------------------------------------

const RTC = { id: "rtc1", name: "Nose", address: "10.0.0.2:8889/cam/whep", kind: "webrtc" };

async function testAWebrtcCameraPlaysInAVideoAndHoldsOnlyAToken() {
  await reset();
  assert.equal(vw.open(RTC), true);
  await flush(20);
  const cam = frameOf("Camera: Nose");
  assert.ok(partOf(cam, "video-win-video"), "a <video>, not the JPEG canvas");
  assert.equal(partOf(cam, "video-win-canvas"), null);
  assert.equal(requests.length, 0, "no JPEG frames are asked for");

  const pc = pcs[0];
  assert.deepEqual(pc.cfg.iceServers, [], "no STUN: the field is offline");
  assert.deepEqual(pc.transceivers, [["video", { direction: "recvonly" }]], "video only, receive only");
  assert.deepEqual(rtcRequests[0], {
    url: "/api/video/webrtc/offer", body: { id: "rtc1", sdp: "v=0 offer" }, keepalive: false,
  }, "the offer goes through the backend, which holds the password");
  assert.deepEqual(pc.remoteDescription, { type: "answer", sdp: "v=0 answer" });

  const track = { kind: "video" };
  pc.ontrack({ track, streams: [] });
  const video = partOf(cam, "video-win-video");
  assert.deepEqual(video.srcObject.tracks, [track]);
  assert.equal(partOf(cam, "ssh-status").lastChild.textContent, "CONNECTING",
    "a connection is not a picture");

  video.frames = 25; NOW += 1000; tick();
  assert.equal(partOf(cam, "ssh-status").lastChild.textContent, "LIVE");
  assert.deepEqual(vw.stateOf("rtc1"), { state: "live", message: "", width: 1280, height: 720, fps: 0 });
  video.frames = 50; NOW += 1000; tick();
  assert.equal(vw.stateOf("rtc1").fps, 25);

  vw.close("rtc1");
  await flush();
  assert.equal(pc.closed, true);
  assert.deepEqual(rtcRequests[rtcRequests.length - 1].body, { session: "tok-1" },
    "closing the window ends the session on the camera's server");
  assert.equal(intervals.size, 0, "and the frame watch with it");
}

async function testAFrozenWebrtcPictureIsCalledOutAndReconnected() {
  await reset();
  vw.open(RTC);
  await flush(20);
  const cam = frameOf("Camera: Nose");
  const video = partOf(cam, "video-win-video");
  pcs[0].ontrack({ track: {}, streams: [] });
  video.frames = 10; NOW += 1000; tick();
  assert.equal(partOf(cam, "ssh-status").lastChild.textContent, "LIVE");

  NOW += 4000; tick();
  assert.equal(partOf(cam, "ssh-status").lastChild.textContent, "NO PICTURE",
    "a connection that stays up while nothing arrives is not live");
  assert.equal(partOf(cam, "video-win-body").classList.contains("is-stale"), true);

  NOW += 7000; tick();
  assert.equal(pcs[0].closed, true, "ten seconds without a frame: the connection is replaced");
  assert.equal(partOf(cam, "ssh-status").lastChild.textContent, "OFFLINE");
  assert.match(partOf(cam, "video-win-msg").textContent, /Reconnecting/);
  assert.ok(rtcRequests.some((r) => r.url.endsWith("/close") && r.body.session === "tok-1"));
  vw.close("rtc1");
}

async function testARefusedWebrtcCameraSaysWhyAndStopsRetryingWhenClosed() {
  await reset();
  rtcAnswers = [jsonAnswer({ ok: false, error: "The camera server refused the user or password." }, 502)];
  vw.open(RTC);
  await flush(20);
  const cam = frameOf("Camera: Nose");
  assert.equal(partOf(cam, "ssh-status").lastChild.textContent, "OFFLINE");
  assert.match(partOf(cam, "video-win-msg").textContent, /refused the user or password/);
  assert.doesNotMatch(partOf(cam, "video-win-msg").textContent, /H\.264/,
    "the codec hint is only for a codec failure");
  vw.close("rtc1");
  const before = pcs.length;
  await new Promise((r) => setTimeout(r, 1200));
  assert.equal(pcs.length, before, "a closed window does not reconnect");
}

async function testACodecFailureExplainsWhatThisWindowCanPlay() {
  await reset();
  failRemote = true;
  vw.open(RTC);
  await flush(20);
  const msg = partOf(frameOf("Camera: Nose"), "video-win-msg").textContent;
  assert.match(msg, /plays VP8, VP9 and AV1 over WebRTC, not H\.264/);
  assert.match(msg, /Use RTSP for an H\.264 camera/);
  assert.doesNotMatch(msg, /[\u2013\u2014]/, "no dashes in operator text");
  vw.close("rtc1");

  await reset();
  codecList = ["VP8", "H264"];
  failRemote = true;
  vw.open(RTC);
  await flush(20);
  assert.doesNotMatch(partOf(frameOf("Camera: Nose"), "video-win-msg").textContent, /not H\.264/,
    "an engine that has H.264 is not told it lacks it");
  vw.close("rtc1");
}

function testTheVideoPageReadsAWebrtcStateFromItsWindow() {
  const sv = Corvus.setupVideo;
  const cam = { id: "rtc1", kind: "webrtc", state: "idle", address: "h/whep" };
  assert.equal(sv.shownState(cam, () => null).state, "idle", "no window, not open");
  assert.equal(sv.shownState(cam, () => ({ state: "live", width: 640, height: 360, fps: 30 })).state, "live");
  const rtsp = { id: "c", kind: "rtsp", state: "error" };
  assert.equal(sv.shownState(rtsp, () => ({ state: "live" })), rtsp, "an RTSP camera is the backend's");
  assert.ok(sv.draftProblem({ kind: "webrtc", url: "rtsp://h/x" }));
  assert.equal(sv.draftProblem({ kind: "webrtc", url: "http://h:8889/cam/whep" }), "");
  assert.equal(sv.saveBody({ kind: "webrtc", url: "http://h/whep" }).kind, "webrtc");
}

// ---------------------------------------------------------------------------
// Out of the app: a window of its own
// ---------------------------------------------------------------------------

/** A second end of the pop-out channel: what another page would see and say. */
const peer = new BroadcastChannel("corvus-popouts");
peer.unref();
const heard = [];
peer.onmessage = (e) => heard.push(e.data);
function settle() { return new Promise((r) => setTimeout(r, 30)); }

async function testThePopOutButtonTakesTheCameraOutOfTheApp() {
  await reset();
  vw.open(CAM);
  const cam = frameOf("Camera: Gimbal");
  cam.getBoundingClientRect = () => ({ left: 300, top: 120, width: 560, height: 349 });
  fire(toolOf(cam, "Open the camera in its own window"), "click");

  assert.equal(opened.length, 1, "a real window, through window.open");
  const win = opened[0];
  assert.match(win.url, /^popout\.html\?/);
  const q = new URLSearchParams(win.url.split("?")[1]);
  assert.deepEqual([q.get("kind"), q.get("key"), q.get("id"), q.get("name")],
    ["video", "video:cam1", "cam1", "Gimbal"]);
  assert.equal(win.features, "popup=yes,width=560,height=349,left=400,top=198",
    "it opens exactly where the frame was, in screen coordinates");
  assert.equal(frameOf("Camera: Gimbal"), undefined, "the frame in the app is gone");
  assert.equal(vw.count(), 1, "but the camera still counts as open");
  assert.equal(vw.has("cam1"), true);

  assert.equal(vw.open(CAM), true);
  assert.equal(frameOf("Camera: Gimbal"), undefined, "asking again does not make a second one");
  assert.equal(win.focused, 1, "it brings the window out of the app forward");
}

async function testTheAppKnowsAPopOutByItsHeartbeatAndForgetsASilentOne() {
  await reset();
  peer.postMessage({ type: "state", key: "video:cam2", kind: "video",
    state: { state: "live", message: "", width: 1280, height: 720, fps: 25 } });
  await settle();
  assert.equal(vw.count(), 1, "a pop-out from before a reload is found again");
  assert.deepEqual(vw.stateOf("cam2"), { state: "live", message: "", width: 1280, height: 720, fps: 25 },
    "and the Video page can show what it shows");
  NOW += Corvus.popouts.STALE_MS + 1;
  assert.equal(vw.count(), 0, "one that stopped answering is gone");
  assert.equal(vw.stateOf("cam2"), null);
}

async function testPuttingItBackOpensTheFrameAgain() {
  await reset();
  peer.postMessage({ type: "state", key: "video:cam1", kind: "video", state: {} });
  await settle();
  peer.postMessage({ type: "dock", key: "video:cam1", kind: "video", payload: { stream: CAM } });
  await settle();
  assert.ok(frameOf("Camera: Gimbal"), "the camera is a frame in the app again");
  assert.equal(Corvus.popouts.count("video"), 0);
}

async function testTheCameraButtonClosesTheWindowsOutOfTheAppToo() {
  await reset();
  vw.open(CAM);
  fire(toolOf(frameOf("Camera: Gimbal"), "Open the camera in its own window"), "click");
  heard.length = 0;
  assert.equal(await vw.toggleAll(), 0);
  await settle();
  assert.deepEqual(heard.filter((m) => m.type === "close").map((m) => m.key), ["video:cam1"]);
  assert.equal(vw.count(), 0);
}

async function testATerminalGoesOutWithItsSessionAndIsRepairedThere() {
  await reset();
  const session = { name: "ssh-launcher/a", title: "Start mission", host: "10.0.0.7", port: 22, username: "pilot" };
  tw.open(session);
  const term = frameOf("Terminal: Start mission");
  term.getBoundingClientRect = () => ({ left: 0, top: 0, width: 640, height: 400 });
  fire(toolOf(term, "Open the terminal in its own window"), "click");
  const q = new URLSearchParams(opened[0].url.split("?")[1]);
  assert.deepEqual([q.get("kind"), q.get("name"), q.get("host"), q.get("username")],
    ["terminal", "ssh-launcher/a", "10.0.0.7", "pilot"]);
  assert.equal(tw.count(), 0, "the frame in the app is gone; the session is not touched");

  heard.length = 0;
  assert.equal(tw.open(session, { reattach: true, existingOnly: true }), true,
    "a relaunch repairs the terminal where it is");
  await settle();
  assert.deepEqual(heard.map((m) => m.type), ["reattach"]);
  assert.equal(tw.count(), 0, "and opens no second one in the app");
}

async function testThePopOutPageIsTheFrameWithoutTheAppAroundIt() {
  await reset();
  heard.length = 0;
  const root = makeEl("div");
  body.appendChild(root);
  let closed = 0;
  global.close = () => { closed += 1; };
  answers = [imageAnswer(7)];
  const page = Corvus.popoutPage.boot(root,
    "?kind=video&key=video%3Acam9&id=cam9&name=Nose&address=10.0.0.2%2Fmain&stream=rtsp&dock=1");
  await flush(20);
  const win = root.children[0];
  assert.ok(win.classList.contains("term-win") && win.classList.contains("popout-win"));
  assert.equal(partOf(win, "term-win-name").textContent, "Nose");
  assert.equal(partOf(win, "ssh-status").lastChild.textContent, "LIVE", "the same player, the same pill");
  assert.equal(partOf(win, "term-win-grip"), null, "the operating system sizes this window");
  await settle();
  assert.equal(heard[0].type, "state", "it tells the app it is there");
  assert.equal(heard[0].key, "video:cam9");

  const dock = toolOf(win, "Put the camera back into the Corvus window");
  heard.length = 0;
  fire(dock, "click");
  await settle();
  assert.deepEqual(heard.map((m) => m.type), ["dock"]);
  assert.deepEqual(heard[0].payload.stream,
    { id: "cam9", name: "Nose", address: "10.0.0.2/main", kind: "rtsp" });
  assert.equal(closed, 1, "and closes itself");
  assert.equal(page.player.stopped, true, "having stopped asking for frames");
  body.removeChild(root);
}

// ---------------------------------------------------------------------------
// The desktop app: dragged out past the edge, and back in
// ---------------------------------------------------------------------------

/** A stand-in for corvus/app.py's MainBridge, recording what it was asked. */
function nativeApp() {
  const calls = [];
  window.corvusNative = {
    detachWindow: (...a) => calls.push(["detach", ...a]),
    moveWindow: (...a) => calls.push(["move", ...a]),
    settleWindow: (...a) => calls.push(["settle", ...a]),
    closeWindow: (...a) => calls.push(["close", ...a]),
  };
  return calls;
}

async function testInTheDesktopAppAFrameIsDraggedOutOfTheApp() {
  await reset();
  const calls = nativeApp();
  vw.open(CAM);
  const cam = frameOf("Camera: Gimbal");
  assert.equal(toolOf(cam, "Open the camera in its own window"), null,
    "no pop-out button: the frame itself goes out");
  const bar = partOf(cam, "term-win-bar");
  const box = cam.getBoundingClientRect();
  const gx = box.left + 60;
  const gy = box.top + 16;
  fire(cam, "pointerdown", { target: bar, clientX: gx, clientY: gy });
  fire(cam, "pointermove", { clientX: gx + 50, clientY: gy });
  assert.equal(calls.length, 0, "inside the app it is an ordinary move");

  fire(cam, "pointermove", { clientX: 1450, clientY: 300 });
  assert.equal(calls[0][0], "detach", "the pointer left the app: a window of its own");
  const [, key, query, left, top, width, height] = calls[0];
  assert.equal(key, "video:cam1");
  assert.equal(new URLSearchParams(query).get("id"), "cam1");
  assert.deepEqual([left, top, width, height], [1390, 284, 560, 349],
    "where the frame would be, grabbed at the same spot");
  assert.equal(cam.classList.contains("is-out"), false,
    "the frame stays up until its window has drawn");

  fire(cam, "pointermove", { clientX: 1700, clientY: 500 });
  assert.deepEqual(calls[1], ["move", "video:cam1", 1640, 484], "and the window follows the pointer");

  peer.postMessage({ type: "state", key: "video:cam1", kind: "video", state: {} });
  await settle();
  assert.equal(cam.classList.contains("is-out"), true, "its window is showing: the frame steps aside");

  fire(cam, "pointerup", { type: "pointerup", clientX: 1700, clientY: 500 });
  assert.deepEqual(calls[2], ["settle", "video:cam1"], "let go outside: kept where it can be grabbed");
  assert.equal(frameOf("Camera: Gimbal"), undefined, "the frame in the app is gone");
  assert.equal(vw.count(), 1, "the camera is still open, out of the app");
  delete window.corvusNative;
}

async function testLettingGoOverTheAppAgainKeepsTheFrameIn() {
  await reset();
  const calls = nativeApp();
  vw.open(CAM);
  const cam = frameOf("Camera: Gimbal");
  const bar = partOf(cam, "term-win-bar");
  const box = cam.getBoundingClientRect();
  fire(cam, "pointerdown", { target: bar, clientX: 900, clientY: 100 });
  fire(cam, "pointermove", { clientX: 1500, clientY: 100 });
  fire(cam, "pointermove", { clientX: 700, clientY: 300 });
  fire(cam, "pointerup", { type: "pointerup", clientX: 700, clientY: 300 });
  assert.deepEqual(calls[calls.length - 1], ["close", "video:cam1"], "the window out there is closed");
  assert.ok(frameOf("Camera: Gimbal"), "and the frame is still in the app");
  assert.equal(cam.classList.contains("is-out"), false);
  assert.deepEqual([parseFloat(cam.style.left), parseFloat(cam.style.top)],
    [700 - (900 - box.left), 300 - (100 - box.top)], "where it was let go of, grabbed at the same spot");
  assert.equal(vw.count(), 1);
  delete window.corvusNative;
}

async function testAWindowLetGoOfOverTheAppComesBackWhereItWasDropped() {
  await reset();
  peer.postMessage({ type: "state", key: "video:cam1", kind: "video", state: {} });
  await settle();
  peer.postMessage({ type: "dock", key: "video:cam1", kind: "video",
    payload: { stream: CAM, at: { left: 300, top: 200, width: 640, height: 389 } } });
  await settle();
  const cam = frameOf("Camera: Gimbal");
  assert.deepEqual(
    [cam.style.left, cam.style.top, cam.style.width, cam.style.height],
    ["300px", "200px", "640px", "389px"], "at the spot, in the size it had out there");
}

async function testTheWindowOutsideIsTheSameFrameAndMovesItself() {
  await reset();
  heard.length = 0;
  const calls = [];
  let closed = 0;
  window.corvusNative = {
    dragStart: () => calls.push(["start"]),
    dragTo: (dx, dy) => calls.push(["to", dx, dy]),
    resizeTo: (dw, dh) => calls.push(["size", dw, dh]),
    dragEnd: (x, y, cb) => {
      calls.push(["end", x, y]);
      cb(JSON.stringify({ inside: true, left: 120, top: 80, width: 560, height: 349 }));
    },
    toggleMaximize: (cb) => { calls.push(["max"]); cb(true); },
    raiseWindow: () => calls.push(["raise"]),
    closeWindow: () => { closed += 1; },
  };
  const root = makeEl("div");
  body.appendChild(root);
  answers = [imageAnswer(3)];
  Corvus.popoutPage.boot(root, "?kind=video&key=video%3Acam7&id=cam7&name=Nose&address=h%2Fx&stream=rtsp&dock=1");
  await flush(20);
  const win = root.children[0];
  assert.deepEqual(
    Array.from(partOf(win, "term-win-tools").children).map((b) => b.getAttribute("aria-label")),
    ["Put the camera back into the Corvus window", "Maximize the camera", "Close the camera window"],
    "the frame's own buttons, in the frame's order");
  assert.ok(partOf(win, "term-win-grip"), "and its grip");

  const bar = partOf(win, "term-win-bar");
  fire(win, "pointerdown", { target: bar, clientX: 60, clientY: 16, screenX: 1000, screenY: 400 });
  fire(win, "pointermove", { clientX: 60, clientY: 16, screenX: 1040, screenY: 430 });
  fire(win, "pointerup", { type: "pointerup", clientX: -300, clientY: 100, screenX: 1040, screenY: 430 });
  assert.deepEqual(calls.slice(0, 3), [["start"], ["to", 40, 30], ["end", -300, 100]],
    "the bar moves the window by the pointer's own deltas");
  await settle();
  assert.equal(heard[heard.length - 1].type, "dock", "let go over the app: back in");
  assert.deepEqual(heard[heard.length - 1].payload.at, { left: 120, top: 80, width: 560, height: 349 });
  assert.equal(closed, 1, "through the app's own close, not the browser's");

  fire(toolOf(win, "Maximize the camera"), "click");
  assert.deepEqual(calls[calls.length - 1], ["max"]);
  assert.equal(win.classList.contains("is-max"), true);
  body.removeChild(root);
  delete window.corvusNative;
}

// ---------------------------------------------------------------------------
// Settings, "Open inside the Corvus window": off by default
// ---------------------------------------------------------------------------

/** MainBridge with openWindow too: the desktop app as it is. */
function desktopApp() {
  const calls = nativeApp();
  window.corvusNative.openWindow = (...a) => calls.push(["open", ...a]);
  return calls;
}

async function testByDefaultACameraOpensAsAWindowOfItsOwnAtOnce() {
  await reset();
  const calls = desktopApp();
  Corvus.popouts.setInApp(false);
  const fresh = { id: "cam5", name: "Tail", address: "10.0.0.5/x" };
  assert.equal(vw.open(fresh), true);
  assert.equal(frameOf("Camera: Tail"), undefined, "no frame in the app first");
  assert.equal(calls.length, 1);
  const [what, key, query, left, top, width, height] = calls[0];
  assert.equal(what, "open");
  assert.equal(key, "video:cam5");
  const q = new URLSearchParams(query);
  assert.equal(q.get("id"), "cam5");
  assert.equal(q.get("dock"), null, "it opened on its own, so it stays a window of its own");
  assert.deepEqual([width, height], [560, 349], "the frame's own size");
  assert.ok(left >= 0 && top >= 0, "where the frame would have opened in the app");
  assert.equal(vw.count(), 1, "and it counts as open");

  assert.equal(vw.open(fresh), true);
  assert.equal(calls.length, 1, "asking again brings it forward, it does not open another");
  vw.close("cam5");
  delete window.corvusNative;
}

async function testWithTheSwitchOnItOpensInTheAppAndGoesOutByDragging() {
  await reset();
  const calls = desktopApp();
  Corvus.popouts.setInApp(true);
  vw.open(CAM);
  const cam = frameOf("Camera: Gimbal");
  assert.ok(cam, "a frame in the app, as before");
  assert.equal(calls.length, 0);
  const bar = partOf(cam, "term-win-bar");
  fire(cam, "pointerdown", { target: bar, clientX: 900, clientY: 100 });
  fire(cam, "pointermove", { clientX: 1500, clientY: 100 });
  assert.equal(calls[0][0], "detach");
  assert.equal(new URLSearchParams(calls[0][2]).get("dock"), "1",
    "dragged out of the app, so it may go back in");
  fire(cam, "pointerup", { type: "pointerup", clientX: 1500, clientY: 100 });
  Corvus.popouts.setInApp(false);
  delete window.corvusNative;
}

async function testATerminalAlsoOpensAsAWindowOfItsOwnByDefault() {
  await reset();
  const calls = desktopApp();
  Corvus.popouts.setInApp(false);
  const session = { name: "ssh-launcher/c", title: "Logger", host: "10.0.0.7", port: 22, username: "pilot" };
  assert.equal(tw.open(session, { existingOnly: true }), false,
    "a launch that only repairs opens nothing");
  assert.equal(calls.length, 0);
  assert.equal(tw.open(session), true);
  assert.equal(tw.count(), 0, "no frame in the app");
  assert.equal(calls[0][0], "open");
  assert.equal(calls[0][1], "term:ssh-launcher/c");
  delete window.corvusNative;
}

async function testAWindowThatOpenedOnItsOwnDoesNotGoBackIn() {
  await reset();
  heard.length = 0;
  const calls = [];
  window.corvusNative = {
    dragStart: () => calls.push(["start"]),
    dragTo: (dx, dy) => calls.push(["to", dx, dy]),
    resizeTo: () => {},
    dragEnd: (x, y, cb) => { calls.push(["end", x, y]); cb(JSON.stringify({ inside: true, left: 1, top: 1, width: 9, height: 9 })); },
    toggleMaximize: (cb) => cb(false),
    raiseWindow: () => {},
    closeWindow: () => calls.push(["close"]),
  };
  const root = makeEl("div");
  body.appendChild(root);
  answers = [imageAnswer(4)];
  Corvus.popoutPage.boot(root, "?kind=video&key=video%3Acam8&id=cam8&name=Belly&address=h%2Fy&stream=rtsp");
  await flush(20);
  const win = root.children[0];
  assert.deepEqual(
    Array.from(partOf(win, "term-win-tools").children).map((b) => b.getAttribute("aria-label")),
    ["Maximize the camera", "Close the camera window"], "no way back into the app on its bar");
  fire(win, "pointerdown", { target: partOf(win, "term-win-bar"), clientX: 60, clientY: 16, screenX: 500, screenY: 300 });
  fire(win, "pointermove", { clientX: 60, clientY: 16, screenX: 400, screenY: 300 });
  fire(win, "pointerup", { type: "pointerup", clientX: 60, clientY: 16, screenX: 400, screenY: 300 });
  await settle();
  assert.equal(heard.filter((m) => m.type === "dock").length, 0, "let go over the app, it stays a window");
  assert.equal(calls.filter((c) => c[0] === "close").length, 0);
  body.removeChild(root);
  delete window.corvusNative;
}

async function testThePinKeepsTheWindowAboveCorvus() {
  await reset();
  const calls = [];
  let onTop = true;       // pinned last time: the window comes back pinned
  window.corvusNative = {
    dragStart() {}, dragTo() {}, resizeTo() {}, dragEnd(x, y, cb) { cb("{}"); },
    toggleMaximize(cb) { cb(false); }, raiseWindow() {}, closeWindow() {},
    isPinned(cb) { cb(onTop); },
    setPinned(on, cb) { calls.push(on); onTop = on; cb(onTop); },
  };
  const root = makeEl("div");
  body.appendChild(root);
  answers = [imageAnswer(5)];
  Corvus.popoutPage.boot(root, "?kind=video&key=video%3Acam6&id=cam6&name=Mast&address=h%2Fz&stream=rtsp");
  await flush(20);
  const win = root.children[0];
  assert.deepEqual(
    Array.from(partOf(win, "term-win-tools").children).map((b) => b.getAttribute("aria-label")),
    ["Keep the camera above the Corvus window", "Maximize the camera", "Close the camera window"],
    "the pin sits beside maximize and close");
  const pin = toolOf(win, "Keep the camera above the Corvus window");
  assert.equal(pin.getAttribute("aria-pressed"), "true", "it shows the state the window really has");
  assert.equal(pin.classList.contains("active"), true);

  fire(pin, "click");
  assert.deepEqual(calls, [false], "a click lets other windows cover it again");
  assert.equal(pin.getAttribute("aria-pressed"), "false");
  assert.equal(pin.classList.contains("active"), false);
  assert.match(pin.title, /^Keep above the Corvus window/);

  fire(pin, "click");
  assert.deepEqual(calls, [false, true]);
  assert.equal(pin.classList.contains("active"), true);
  assert.doesNotMatch(pin.title, /[\u2013\u2014]/, "no dashes in operator text");
  body.removeChild(root);
  delete window.corvusNative;
}

async function testATerminalWindowKeepsItsOwnButtonsInTheAppsOrder() {
  await reset();
  window.corvusNative = {
    dragStart() {}, dragTo() {}, resizeTo() {}, dragEnd(x, y, cb) { cb("{}"); },
    toggleMaximize(cb) { cb(false); }, raiseWindow() {}, closeWindow() {},
    isPinned(cb) { cb(false); }, setPinned(on, cb) { cb(on); },
  };
  const root = makeEl("div");
  body.appendChild(root);
  Corvus.popoutPage.boot(root,
    "?kind=terminal&key=term%3Assh-launcher%2Fz&name=ssh-launcher%2Fz&title=Logger&host=10.0.0.7&dock=1");
  await flush(10);
  assert.deepEqual(
    Array.from(partOf(root.children[0], "term-win-tools").children).map((b) => b.getAttribute("aria-label")),
    ["Put the terminal back into the Corvus window", "Keep the terminal above the Corvus window",
     "Maximize the terminal", "Disconnect the session", "Close the terminal window"]);
  body.removeChild(root);
  delete window.corvusNative;
}

// ---------------------------------------------------------------------------
// The SSH Launcher's terminals: the same windows, and as reliable as before
// ---------------------------------------------------------------------------

async function testALaunchersTerminalOpensAsAWindowOfItsOwnThroughTheSharedCode() {
  await reset();
  const calls = desktopApp();
  Corvus.popouts.setInApp(false);
  // What plugins.js hands a plugin as api.terminal.
  const session = { name: "ssh-launcher/b1x", title: "Start mission", host: "10.0.0.7", port: 22, username: "pilot" };
  assert.equal(tw.open(session, { reattach: false, existingOnly: false }), true);
  assert.equal(calls[0][0], "open");
  assert.equal(calls[0][1], "term:ssh-launcher/b1x");
  const q = new URLSearchParams(calls[0][2]);
  assert.deepEqual([q.get("kind"), q.get("title"), q.get("username")], ["terminal", "Start mission", "pilot"]);
  assert.equal(q.get("password"), null, "no credential ever travels with a window");
  delete window.corvusNative;
}

async function testAKeyTheDesktopAppWouldRefuseStaysInTheAppRatherThanNowhere() {
  await reset();
  const calls = desktopApp();
  Corvus.popouts.setInApp(false);
  const odd = { name: "ssh-launcher/hand edited id!", title: "Odd", host: "10.0.0.7" };
  assert.equal(tw.open(odd), true);
  assert.equal(calls.filter((c) => c[0] === "open").length, 0, "not sent to a window that would never open");
  assert.ok(frameOf("Terminal: Odd"), "it opens in the app instead");
  delete window.corvusNative;
}

async function testANewShellReachesAWindowTheAppHasNotHeardFromLately() {
  await reset();
  heard.length = 0;
  const session = { name: "ssh-launcher/b2y", title: "Logger", host: "10.0.0.7" };
  // Nothing known about it: a covered window whose heartbeat lapsed.
  assert.equal(Corvus.popouts.isOpen("term:ssh-launcher/b2y"), false);
  assert.equal(tw.open(session, { reattach: true, existingOnly: true }), false,
    "a launch still opens nothing");
  await settle();
  assert.deepEqual(heard.map((m) => [m.type, m.key]), [["reattach", "term:ssh-launcher/b2y"]],
    "but the window, if it is there, is told about the new shell");
}

async function testARepairedTerminalOutsideDoesNotTakeTheKeyboard() {
  await reset();
  shells.length = 0;
  const root = makeEl("div");
  body.appendChild(root);
  Corvus.popoutPage.boot(root, "?kind=terminal&key=term%3Assh-launcher%2Fb3z&name=ssh-launcher%2Fb3z&title=Logger");
  await flush(10);
  assert.equal(shells.length, 1);
  assert.equal(shells[0].focused, 1, "opened by the operator: the keyboard goes in");
  peer.postMessage({ type: "reattach", key: "term:ssh-launcher/b3z" });
  await settle();
  assert.equal(shells.length, 2, "the new shell gets a new stream");
  assert.equal(shells[0].disposed, true, "the old one is dropped");
  assert.equal(shells[1].focused, 0, "and a repair does not take the keyboard");
  assert.equal(shells[1].session.name, "ssh-launcher/b3z");
  body.removeChild(root);
}

// ---------------------------------------------------------------------------
// Wayland: the compositor places the windows, and there is no pin
// ---------------------------------------------------------------------------

async function testWhereWindowsCannotBePlacedTheFrameStaysInAndOffersItsWindow() {
  await reset();
  const calls = desktopApp();
  window.corvusNativeSupport = { place: false, pin: false };
  Corvus.popouts.setInApp(true);
  try {
    vw.open(CAM);
    const cam = frameOf("Camera: Gimbal");
    const bar = partOf(cam, "term-win-bar");
    fire(cam, "pointerdown", { target: bar, clientX: 900, clientY: 100 });
    fire(cam, "pointermove", { clientX: 1500, clientY: 100 });
    fire(cam, "pointerup", { type: "pointerup", clientX: 1500, clientY: 100 });
    assert.equal(calls.filter((c) => c[0] === "detach").length, 0,
      "a window that cannot follow the pointer is not started by dragging");
    assert.ok(frameOf("Camera: Gimbal"), "the frame stays in the app");
    const out = toolOf(cam, "Open the camera in its own window");
    assert.ok(out, "and offers the way out as a button");
    fire(out, "click");
    assert.equal(calls[calls.length - 1][0], "open", "a window of its own, the desktop app's");
    assert.equal(new URLSearchParams(calls[calls.length - 1][2]).get("dock"), "1");
    assert.equal(frameOf("Camera: Gimbal"), undefined);
  } finally {
    Corvus.popouts.setInApp(false);
    delete window.corvusNativeSupport;
    delete window.corvusNative;
  }
}

async function testWhereWindowsCannotBePlacedTheCompositorMovesThemAndThereIsNoPin() {
  await reset();
  const calls = [];
  window.corvusNativeSupport = { place: false, pin: false };
  window.corvusNative = {
    dragStart: () => calls.push(["start"]),
    dragTo: () => calls.push(["to"]),
    resizeTo: () => calls.push(["size"]),
    dragEnd: (x, y, cb) => { calls.push(["end"]); cb("{}"); },
    systemMove: () => calls.push(["system-move"]),
    systemResize: () => calls.push(["system-resize"]),
    toggleMaximize: (cb) => cb(true),
    setPinned: () => calls.push(["pin"]),
    isPinned: (cb) => cb(false),
    raiseWindow() {}, closeWindow() {},
  };
  const root = makeEl("div");
  body.appendChild(root);
  answers = [imageAnswer(6)];
  try {
    Corvus.popoutPage.boot(root, "?kind=video&key=video%3Acam9&id=cam9&name=Wing&address=h%2Fw&stream=rtsp");
    await flush(20);
    const win = root.children[0];
    assert.deepEqual(
      Array.from(partOf(win, "term-win-tools").children).map((b) => b.getAttribute("aria-label")),
      ["Maximize the camera", "Close the camera window"], "no pin where nothing can be kept on top");
    fire(win, "pointerdown", { target: partOf(win, "term-win-bar"), clientX: 60, clientY: 16, screenX: 5, screenY: 5 });
    fire(win, "pointermove", { clientX: 90, clientY: 16, screenX: 35, screenY: 5 });
    fire(win, "pointerup", { type: "pointerup", clientX: 90, clientY: 16, screenX: 35, screenY: 5 });
    fire(win, "pointerdown", { target: partOf(win, "term-win-grip"), clientX: 600, clientY: 300 });
    assert.deepEqual(calls, [["system-move"], ["system-resize"]],
      "the bar and the grip hand the gesture to the compositor, and nothing else is asked");
  } finally {
    body.removeChild(root);
    delete window.corvusNativeSupport;
    delete window.corvusNative;
  }
}

const tests = [
  testTheCameraWindowIsTheTerminalWindowsFrame,
  testCameraAndTerminalStackAsOneSetOfWindows,
  testTheCameraWindowMovesAndMaximizesLikeATerminal,
  testOneWindowPerCamera,
  testFramesAreAskedForOneAtATimeAndNumbered,
  testNoPictureShowsTheReasonAndDimsTheLastFrame,
  testClosingTheWindowStopsAsking,
  testACameraThatWasRemovedStopsTheLoop,
  testTheCameraButtonOpensEveryCameraThenClosesThem,
  testTheCameraButtonSaysWhereToAddACamera,
  testTheCameraListIsKeptForTheMapsButton,
  testToggleOpensOneCameraThenClosesIt,
  testTheRowSaysWhatThePictureIsOrWhatWentWrong,
  testThePasswordIsOnlySentWhenItWasTyped,
  testADraftWithoutAnAddressCannotBeSaved,
  testAWebrtcCameraPlaysInAVideoAndHoldsOnlyAToken,
  testAFrozenWebrtcPictureIsCalledOutAndReconnected,
  testARefusedWebrtcCameraSaysWhyAndStopsRetryingWhenClosed,
  testACodecFailureExplainsWhatThisWindowCanPlay,
  testTheVideoPageReadsAWebrtcStateFromItsWindow,
  testThePopOutButtonTakesTheCameraOutOfTheApp,
  testTheAppKnowsAPopOutByItsHeartbeatAndForgetsASilentOne,
  testPuttingItBackOpensTheFrameAgain,
  testTheCameraButtonClosesTheWindowsOutOfTheAppToo,
  testATerminalGoesOutWithItsSessionAndIsRepairedThere,
  testThePopOutPageIsTheFrameWithoutTheAppAroundIt,
  testInTheDesktopAppAFrameIsDraggedOutOfTheApp,
  testLettingGoOverTheAppAgainKeepsTheFrameIn,
  testAWindowLetGoOfOverTheAppComesBackWhereItWasDropped,
  testTheWindowOutsideIsTheSameFrameAndMovesItself,
  testByDefaultACameraOpensAsAWindowOfItsOwnAtOnce,
  testWithTheSwitchOnItOpensInTheAppAndGoesOutByDragging,
  testATerminalAlsoOpensAsAWindowOfItsOwnByDefault,
  testAWindowThatOpenedOnItsOwnDoesNotGoBackIn,
  testThePinKeepsTheWindowAboveCorvus,
  testATerminalWindowKeepsItsOwnButtonsInTheAppsOrder,
  testALaunchersTerminalOpensAsAWindowOfItsOwnThroughTheSharedCode,
  testAKeyTheDesktopAppWouldRefuseStaysInTheAppRatherThanNowhere,
  testANewShellReachesAWindowTheAppHasNotHeardFromLately,
  testARepairedTerminalOutsideDoesNotTakeTheKeyboard,
  testWhereWindowsCannotBePlacedTheFrameStaysInAndOffersItsWindow,
  testWhereWindowsCannotBePlacedTheCompositorMovesThemAndThereIsNoPin,
];

(async () => {
  let failed = 0;
  for (const t of tests) {
    try {
      await t();
      console.log(`  ok  ${t.name}`);
    } catch (err) {
      failed += 1;
      console.error(`  FAIL ${t.name}: ${err.message}`); if (process.env.STACK) console.error(err.stack);
    }
  }
  await reset();
  console.log(failed ? `${failed} failing` : `All ${tests.length} camera-window tests passed.`);
  process.exit(failed ? 1 : 0);
})();
