"use strict";
window.Corvus = window.Corvus || {};

/**
 * Schwalby — a plugin for the TOOLS tab: one-press buttons that start programs,
 * each on this computer or on a companion computer over SSH.
 *
 * The job is the SSH Launcher's: the things that have to be started before a
 * flight (the mission script, the video pipeline, a log recorder), one press
 * each, with the aircraft already on the pad. Schwalby adds the other half of
 * the ground station: a program that runs on the laptop itself. Where a button
 * runs is picked per button with the Local / SSH segment in its settings:
 *
 *   LOCAL  This computer, as the operator, through /api/local/*. The backend
 *          only accepts that from this computer itself, whatever address the
 *          rest of the API is served on.
 *   SSH    A saved SSH connection, resolved on the backend, so neither the
 *          browser nor this plugin ever holds a password: a button stores the
 *          connection's NAME. "New connection…" at the end of the list types
 *          one in and saves it to the app's SSH connections first.
 *
 * Two ways to run one, per button, on either target:
 *
 *   TERMINAL (the default)  Types `cd -- <folder> && <command>` into a session
 *     of the button's own, opening one the first time: a login shell here, or
 *     an SSH shell there. Here the shell lives among the SSH sessions in the
 *     backend, so the terminal window, its pin and its disconnect button work
 *     on it exactly as on an SSH one. Pressing the button again types the line
 *     again into the SAME shell, because a key on a shelf pressed twice means
 *     "run it again", not "throw this session away". The window opens on the
 *     ARROW beside the button, never on the button: a launch is a launch.
 *
 *   BACKGROUND  One shot, nothing to watch. Over SSH it is `POST /api/ssh/run`
 *     with nohup, so the program outlives Corvus on the companion computer.
 *     Here it is `POST /api/local/run`, and it is stopped when Corvus closes:
 *     a ground station that leaves programs of its own running after it exits
 *     cannot be shut down cleanly between flights.
 *
 * The shelf is the plugin's state: a list of {id, label, target, connection,
 * directory, command, mode}, saved through api.saveSettings. Nothing secret
 * goes in there. Its sessions are "schwalby/<button id>", its own, so it never
 * presses the SSH Launcher's.
 *
 * Two views, never both: the SHELF (the buttons plus Add) and the EDITOR (one
 * button's fields). Removing a button is the last thing in its editor.
 *
 * Above both, the COMPANION indicator: the companion computer's name and
 * address with a status dot, pinged from this computer through
 * POST /api/local/ping at the interval set in its popup, and shown offline
 * once no reply has come back for the timeout set there. Pressing it opens
 * that popup. It is saved beside the buttons as {name, host, interval_s,
 * timeout_s} and probed only while the plugin is open.
 *
 * When a press does not run, the reason lands on the row that failed (a
 * warning triangle, the message in its popover) and on the notification board
 * (api.notification), for the operator who has already turned back to the
 * aircraft.
 *
 * Self-contained like every plugin: this script, its stylesheet
 * (schwalby.css, every class prefixed schw-), the Corvus.ui components and the
 * plugin api, nothing else.
 */
Corvus.pluginSchwalby = (function () {
  const PLUGIN_ID = "schwalby";

  // Bounded so the shelf stays a shelf; an operator with more of them wants a
  // script, not more buttons.
  const MAX_BUTTONS = 12;

  // How often the shelf asks which of its sessions are still up. There is no
  // push for this, and the answer is what decides whether a button's arrow
  // leads anywhere. Only while the shelf is the visible view.
  const LIVE_POLL_MS = 4000;

  const MODE_TERMINAL = "terminal";
  const MODE_BACKGROUND = "background";

  const TARGET_LOCAL = "local";
  const TARGET_SSH = "ssh";

  // The two answers to "where does it run", in the segment's order: Local
  // first, because it is what this plugin adds.
  const TARGETS = [
    { value: TARGET_LOCAL, label: "Local", icon: "laptop", title: "Run it on this computer" },
    { value: TARGET_SSH, label: "SSH", icon: "server", title: "Run it over an SSH connection" },
  ];

  // What a terminal on this computer calls its host, in the window header.
  const LOCAL_HOST = "this computer";

  // The connection <select>'s last entry: not a connection but a way to type
  // one in, for a companion computer nothing is configured for yet.
  const NEW_CONNECTION = "__new__";

  function targetInfo(value) {
    return TARGETS.find((t) => t.value === value) || TARGETS[0];
  }

  /** The blank ad-hoc connection form. */
  function blankConnection() {
    return { name: "", host: "", port: "22", username: "", password: "", key_path: "" };
  }

  /**
   * The name an ad-hoc connection is saved under when the operator types none:
   * user@host.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} conn the connection form
   * @returns {string} "" when there is not even a host yet
   */
  function derivedName(conn) {
    const c = conn || {};
    const host = String(c.host == null ? "" : c.host).trim();
    if (!host) return "";
    const user = String(c.username == null ? "" : c.username).trim();
    return user ? `${user}@${host}` : host;
  }

  /**
   * Why an ad-hoc connection cannot be saved yet, or "" when it can. A name
   * already taken is refused: saving under it would replace the credentials
   * of a connection the operator relies on.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} conn the connection form
   * @param {string[]} taken the names already saved
   * @returns {string}
   */
  function newConnectionError(conn, taken) {
    const c = conn || {};
    if (!String(c.host == null ? "" : c.host).trim()) return "Name the host to connect to.";
    const name = String(c.name == null ? "" : c.name).trim() || derivedName(c);
    const port = String(c.port == null ? "" : c.port).trim();
    if (port !== "") {
      const n = Number(port);
      if (!isFinite(n) || Math.floor(n) !== n || n < 1 || n > 65535) {
        return "The port has to be a number between 1 and 65535.";
      }
    }
    if ((taken || []).indexOf(name) >= 0) {
      return `"${name}" is already saved. Pick it above, or save this one as another name.`;
    }
    return "";
  }

  /**
   * The body of POST /api/ssh/connections for an ad-hoc connection.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} conn the connection form
   * @returns {Object}
   */
  function newConnectionBody(conn) {
    const c = conn || {};
    const trim = (v) => String(v == null ? "" : v).trim();
    return {
      name: trim(c.name) || derivedName(c),
      host: trim(c.host),
      port: parseInt(trim(c.port), 10) || 22,
      username: trim(c.username),
      // Not trimmed: a password may begin or end with a space.
      password: String(c.password == null ? "" : c.password),
      key_path: trim(c.key_path),
    };
  }

  /** A short, collision-free id for a new button. Never shown. */
  function newId() {
    return "b" + Date.now().toString(36) + Math.floor(Math.random() * 1e6).toString(36);
  }

  /**
   * The session a button runs in, keyed by its id so a rename does not orphan
   * it, and by this plugin so it is never the SSH Launcher's.
   * @param {Object} entry
   * @returns {string}
   */
  function sessionName(entry) {
    return PLUGIN_ID + "/" + (entry && entry.id ? entry.id : "");
  }

  /**
   * Where a saved button runs. An entry without a target that names a
   * connection is one copied over from the SSH Launcher, so it runs over SSH;
   * anything else runs here.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} raw
   * @returns {string}
   */
  function coerceTarget(raw) {
    const r = raw || {};
    if (r.target === TARGET_LOCAL || r.target === TARGET_SSH) return r.target;
    return String(r.connection == null ? "" : r.connection).trim() ? TARGET_SSH : TARGET_LOCAL;
  }

  /**
   * A button's run mode. `detach` is the SSH Launcher's older boolean, so an
   * entry copied from its settings keeps meaning what it meant there.
   * @param {Object} raw
   * @returns {string}
   */
  function coerceMode(raw) {
    const r = raw || {};
    if (r.mode === MODE_BACKGROUND || r.mode === MODE_TERMINAL) return r.mode;
    if (typeof r.detach === "boolean") return r.detach ? MODE_BACKGROUND : MODE_TERMINAL;
    return MODE_TERMINAL;
  }

  /**
   * Coerce one saved entry into a complete button, or null to drop it. This
   * reads a JSON file an operator may have edited, so a half-written entry
   * costs that button and not the shelf.
   *
   * Pure, and exported for the test suite.
   *
   * @param {*} raw
   * @returns {Object|null}
   */
  function coerceButton(raw) {
    if (!raw || typeof raw !== "object") return null;
    const command = String(raw.command == null ? "" : raw.command).trim();
    if (!command) return null;
    const label = String(raw.label == null ? "" : raw.label).trim();
    const target = coerceTarget(raw);
    return {
      id: (typeof raw.id === "string" && raw.id) ? raw.id : newId(),
      label: label || command,
      target,
      // A button here names no connection, and one left from when it ran
      // elsewhere would only mislead whoever reads the file.
      connection: target === TARGET_LOCAL ? ""
        : String(raw.connection == null ? "" : raw.connection).trim(),
      directory: String(raw.directory == null ? "" : raw.directory).trim(),
      command,
      mode: coerceMode(raw),
    };
  }

  /**
   * The button list from the plugin's saved settings.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} saved api.getSettings()
   * @returns {Object[]} complete buttons, capped at MAX_BUTTONS
   */
  function normalizeButtons(saved) {
    const s = saved || {};
    const out = [];
    (Array.isArray(s.buttons) ? s.buttons : []).forEach((raw) => {
      const button = coerceButton(raw);
      if (button && out.length < MAX_BUTTONS) out.push(button);
    });
    return out;
  }

  /**
   * A folder as a POSIX shell should read it: quoted, except for a leading
   * `~`, which only means "home" outside the quotes.
   * @param {string} directory
   * @returns {string}
   */
  function shellFolder(directory) {
    const quote = (text) => "'" + text.replace(/'/g, "'\\''") + "'";
    if (directory === "~") return "~";
    if (directory.indexOf("~/") === 0) {
      const rest = directory.slice(2);
      return rest ? "~/" + quote(rest) : "~/";
    }
    return quote(directory);
  }

  /**
   * The line a button types into its terminal. The folder is quoted and `--`
   * guards one that begins with a dash; the command is passed through as the
   * operator typed it, so pipelines and arguments work, and the account's own
   * permissions are the boundary, as when they type it themselves.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} entry {directory, command}
   * @returns {string}
   */
  function terminalLine(entry) {
    const e = entry || {};
    const command = String(e.command || "").trim();
    if (!command) return "";
    const directory = String(e.directory || "").trim();
    if (!directory) return command;
    return "cd -- " + shellFolder(directory) + " && " + command;
  }

  /**
   * The human-readable preview of what a button will run. Shown, not sent.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} cfg {target, connection, directory, command, mode}
   * @returns {string} "" when there is nothing to show
   */
  function previewLine(cfg) {
    const c = cfg || {};
    const command = String(c.command || "").trim();
    if (!command) return "";
    const directory = String(c.directory || "").trim();
    const local = coerceTarget(c) === TARGET_LOCAL;
    const background = coerceMode(c) === MODE_BACKGROUND;
    // Here a background program is Corvus' own child, not a nohup'd orphan.
    const body = background ? (local ? `${command} &` : `nohup ${command} &`) : command;
    const line = directory ? `cd ${directory} && ${body}` : body;
    if (local) return line;
    const target = String(c.connection || "").trim();
    return target ? `ssh ${target} '${line}'` : line;
  }

  /**
   * One line for a background run over SSH.
   * @param {Object} res the POST /api/ssh/run response body
   * @returns {{text: string, kind: string}}
   */
  function sshSummary(res) {
    const r = res || {};
    if (!r.ok) {
      const detail = String(r.stderr || r.error || "").trim().split("\n")[0];
      return { text: detail || "The command failed.", kind: "err" };
    }
    // With nohup the stdout is the pid the remote shell echoed.
    const pid = String(r.stdout || "").trim();
    return { text: pid ? `Started in the background (pid ${pid}).` : "Started in the background.", kind: "ok" };
  }

  /**
   * One line for a background run on this computer. The backend waits a
   * second before it answers, so a program that cannot start answers with its
   * exit code and its own words instead of a pid that is already gone.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} res the POST /api/local/run response body
   * @returns {{text: string, kind: string}}
   */
  function localSummary(res) {
    const r = res || {};
    const firstLine = (text) => String(text || "").trim().split("\n")[0].trim();
    if (r.exited) {
      if (r.ok) return { text: "Ran and finished.", kind: "ok" };
      return { text: firstLine(r.output) || `It ended at once with exit code ${r.code}.`, kind: "err" };
    }
    if (!r.ok) return { text: firstLine(r.error) || "The command failed.", kind: "err" };
    return {
      text: r.pid ? `Started in the background (pid ${r.pid}). It stops when Corvus closes.`
        : "Started in the background. It stops when Corvus closes.",
      kind: "ok",
    };
  }

  /**
   * The Local / SSH segment: a radiogroup of buttons, the arrow keys moving
   * the choice and only the chosen one a Tab stop.
   *
   * @param {Object} opts {value, options: [{value, label, icon, title}],
   *                       ariaLabel, onChange(next)}
   * @returns {{el: HTMLElement, getValue: Function}}
   */
  function segment(opts) {
    const ui = Corvus.ui;
    const options = opts.options;
    let value = options.some((o) => o.value === opts.value) ? opts.value : options[0].value;
    const el = document.createElement("div");
    el.className = "schw-seg";
    el.setAttribute("role", "radiogroup");
    if (opts.ariaLabel) el.setAttribute("aria-label", opts.ariaLabel);

    const buttons = options.map((opt, index) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "schw-seg-opt";
      b.setAttribute("role", "radio");
      b.dataset.value = opt.value;
      if (opt.title) b.title = opt.title;
      if (opt.icon) b.appendChild(ui.icon(opt.icon, 13));
      const text = document.createElement("span");
      text.textContent = opt.label;
      b.appendChild(text);
      b.addEventListener("click", () => choose(opt.value, false));
      b.addEventListener("keydown", (e) => {
        const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
        if (!step) return;
        e.preventDefault();
        choose(options[(index + step + options.length) % options.length].value, true);
      });
      el.appendChild(b);
      return b;
    });

    function paint() {
      buttons.forEach((b, i) => {
        const on = options[i].value === value;
        b.setAttribute("aria-checked", on ? "true" : "false");
        b.tabIndex = on ? 0 : -1;
      });
    }

    function choose(next, focus) {
      if (next === value) return;
      value = next;
      paint();
      const b = buttons[options.findIndex((o) => o.value === next)];
      if (focus && b && typeof b.focus === "function") b.focus();
      if (typeof opts.onChange === "function") opts.onChange(value);
    }

    paint();
    return { el, getValue: () => value };
  }

  // ---- the companion computer ---------------------------------------------

  // The slider stops, in seconds. Timeouts start above the shortest interval
  // because a timeout no longer than the interval calls every reply late.
  const INTERVAL_STEPS = [1, 2, 3, 5, 10];
  const TIMEOUT_STEPS = [3, 4, 6, 10, 15, 20, 30];
  const DEFAULT_INTERVAL_S = 2;
  const DEFAULT_TIMEOUT_S = 6;
  const MAX_COMPANION_NAME = 40;

  // The same rule the backend applies (corvus/net_probe.py), so Save is only
  // offered for a host the ping endpoint will take.
  const HOST_RE = /^[A-Za-z0-9:](?:[A-Za-z0-9._:%-]*[A-Za-z0-9])?$/;

  function nearestStep(steps, value, fallback) {
    const n = Number(value);
    if (!isFinite(n)) return fallback;
    return steps.reduce((best, s) => (Math.abs(s - n) < Math.abs(best - n) ? s : best), steps[0]);
  }

  /** The shortest timeout that is still longer than *interval*. */
  function timeoutFor(interval) {
    return TIMEOUT_STEPS.find((t) => t > interval) || TIMEOUT_STEPS[TIMEOUT_STEPS.length - 1];
  }

  /** The longest interval that is still shorter than *timeout*. */
  function intervalFor(timeout) {
    const fit = INTERVAL_STEPS.filter((i) => i < timeout);
    return fit.length ? fit[fit.length - 1] : INTERVAL_STEPS[0];
  }

  /**
   * Why a companion address cannot be pinged, or "" when it can.
   *
   * Pure, and exported for the test suite.
   *
   * @param {string} host
   * @returns {string}
   */
  function hostError(host) {
    const h = String(host == null ? "" : host).trim();
    if (!h) return "Enter an IP address or host name.";
    if (h.length > 253 || !HOST_RE.test(h)) return "That is not an IP address or host name.";
    return "";
  }

  /**
   * The saved companion computer as the indicator uses it, or null when none
   * is set. The intervals land on slider stops, and the timeout is kept longer
   * than the interval.
   *
   * Pure, and exported for the test suite.
   *
   * @param {*} raw
   * @returns {{name: string, host: string, interval_s: number, timeout_s: number}|null}
   */
  function normalizeCompanion(raw) {
    if (!raw || typeof raw !== "object") return null;
    const host = String(raw.host == null ? "" : raw.host).trim();
    if (hostError(host)) return null;
    const interval = nearestStep(INTERVAL_STEPS, raw.interval_s, DEFAULT_INTERVAL_S);
    let timeout = nearestStep(TIMEOUT_STEPS, raw.timeout_s, DEFAULT_TIMEOUT_S);
    if (timeout <= interval) timeout = timeoutFor(interval);
    return {
      name: String(raw.name == null ? "" : raw.name).trim().slice(0, MAX_COMPANION_NAME),
      host,
      interval_s: interval,
      timeout_s: timeout,
    };
  }

  /**
   * How long one probe may wait for its echo: the interval, within 1 to 5 s,
   * so a slow reply never holds the next probe back for long.
   *
   * Pure, and exported for the test suite.
   *
   * @param {number} intervalS
   * @returns {number} milliseconds
   */
  function probeWaitMs(intervalS) {
    return Math.min(5000, Math.max(1000, Number(intervalS) * 1000 || 2000));
  }

  /**
   * What the indicator shows. Online while the last reply is younger than the
   * timeout; before the first reply, checking until the timeout has passed;
   * offline after that. A probe that could not be made at all (no ping
   * program, the backend unreachable) is an error, not a verdict on the
   * companion, unless a recent reply already answers the question.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} s {host, startedAt, lastOkAt, error, now, timeoutMs}
   * @returns {string} "unset" | "checking" | "online" | "offline" | "error"
   */
  function companionState(s) {
    const o = s || {};
    if (!o.host) return "unset";
    const now = Number(o.now);
    const timeout = Number(o.timeoutMs);
    if (o.lastOkAt != null && now - o.lastOkAt <= timeout) return "online";
    if (o.error) return "error";
    if (o.lastOkAt == null && now - o.startedAt < timeout) return "checking";
    return "offline";
  }

  const COMPANION_LOOK = {
    unset: { level: "off", text: "Not set" },
    checking: { level: "warning", text: "Checking" },
    online: { level: "healthy", text: "Online" },
    offline: { level: "critical", text: "Offline" },
    error: { level: "warning", text: "Cannot ping" },
  };

  function init(containerEl, api) {
    const ui = Corvus.ui;
    containerEl.innerHTML = "";

    let buttons = normalizeButtons(
      (api && typeof api.getSettings === "function") ? api.getSettings() : {});
    let connections = [];         // saved SSH connections
    let connectionsError = "";    // why that list is empty, when it is
    let localStatus = null;       // GET /api/local/status
    let editing = null;           // the button being edited, or null on the shelf
    let repaintTarget = null;     // the open editor's target painter, for a late status
    let live = {};                // session name -> true, from /api/ssh/sessions
    const errors = Object.create(null);  // button id -> why its last press failed
    const closing = Object.create(null); // session name -> its disconnect in flight
    let pollTimer = null;
    let cancelled = false;        // set by destroy(); gates every late callback

    let companion = normalizeCompanion(
      (api && typeof api.getSettings === "function") ? (api.getSettings() || {}).companion : null);
    const probe = {
      gen: 0,               // bumped on every restart, so a late answer is dropped
      timer: null,          // the next probe
      paintTimer: null,     // the moment the indicator changes without an answer
      startedAt: 0,
      lastOkAt: null,
      rtt: null,
      error: "",
      shown: "",            // the state last painted, for the console line
    };
    let companionDialog = null;

    const companionEl = document.createElement("div");
    const shelfEl = document.createElement("div");
    const editorEl = document.createElement("div");
    const status = ui.message({});
    const output = document.createElement("pre");
    output.className = "schw-output";
    output.hidden = true;

    containerEl.appendChild(companionEl);
    containerEl.appendChild(status.el);
    containerEl.appendChild(shelfEl);
    containerEl.appendChild(editorEl);
    containerEl.appendChild(output);

    function isLocal(entry) { return !!entry && entry.target === TARGET_LOCAL; }

    /** Why a terminal on this computer cannot be had here, or "". */
    function localTerminalReason() {
      if (!localStatus || localStatus.terminal !== false) return "";
      return localStatus.reason || "A terminal on this computer is not available here.";
    }

    // ---- what went wrong, and for which button ------------------------------

    function setError(entry, text) {
      errors[entry.id] = String(text || "It failed.");
      if (editing === null) renderShelf();
    }

    function clearError(entry) {
      if (entry && entry.id) delete errors[entry.id];
    }

    /** The row gets the reason, and so does the operator who looked away. */
    function failed(entry, detail) {
      setError(entry, detail);
      api.notification("warning", `${entry.label}: ${detail}`);
    }

    // ---- persistence --------------------------------------------------------

    /** Save the shelf. Best-effort: a failed write costs the buttons on the
     *  next start, never the run the operator is doing now. */
    function persist() {
      if (!api || typeof api.saveSettings !== "function") return;
      const settings = { buttons: buttons };
      if (companion) settings.companion = companion;
      api.saveSettings(settings, true).catch(() => {});
    }

    // ---- the companion computer ---------------------------------------------

    function companionNow() {
      return companionState({
        host: companion ? companion.host : "",
        startedAt: probe.startedAt,
        lastOkAt: probe.lastOkAt,
        error: probe.error,
        now: Date.now(),
        timeoutMs: companion ? companion.timeout_s * 1000 : 0,
      });
    }

    /** The indicator at the top: one button that also opens the settings. */
    function paintCompanion() {
      if (probe.paintTimer !== null) { clearTimeout(probe.paintTimer); probe.paintTimer = null; }
      const state = companionNow();
      const look = COMPANION_LOOK[state];
      ui.clear(companionEl);

      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "schw-comp";
      btn.dataset.state = state;
      btn.addEventListener("click", () => openCompanionDialog());

      const iconEl = document.createElement("span");
      iconEl.className = "schw-comp-icon";
      iconEl.appendChild(ui.icon("cpu", 14));
      btn.appendChild(iconEl);

      const text = document.createElement("span");
      text.className = "schw-comp-text";
      const name = document.createElement("span");
      name.className = "schw-comp-name";
      const host = document.createElement("span");
      host.className = "schw-comp-host";
      if (companion) {
        name.textContent = companion.name || "Companion computer";
        host.textContent = companion.host;
      } else {
        name.textContent = "Companion computer";
        host.textContent = "Press to set";
      }
      text.append(name, host);
      btn.appendChild(text);

      const stateEl = document.createElement("span");
      stateEl.className = "schw-comp-state";
      stateEl.appendChild(ui.statusDot(look.level));
      const word = document.createElement("span");
      word.textContent = state === "online" && probe.rtt != null
        ? `${look.text} · ${formatRtt(probe.rtt)}`
        : look.text;
      stateEl.appendChild(word);
      btn.appendChild(stateEl);

      const label = companion
        ? `${companion.name || "Companion computer"} at ${companion.host}: ${look.text}`
        : "Companion computer: not set";
      btn.setAttribute("aria-label", `${label}. Change`);
      btn.title = state === "error" ? probe.error
        : companion ? `Pinged every ${companion.interval_s} s, offline after ${companion.timeout_s} s without a reply`
          : "Set the companion computer to watch";
      companionEl.appendChild(btn);
      ui.refreshIcons();

      noteTransition(state);

      // Online and checking both end on their own when nothing answers.
      let due = null;
      if (state === "online") due = probe.lastOkAt + companion.timeout_s * 1000;
      else if (state === "checking") due = probe.startedAt + companion.timeout_s * 1000;
      if (due !== null && !cancelled) {
        probe.paintTimer = setTimeout(paintCompanion, Math.max(0, due - Date.now()) + 50);
      }
    }

    function formatRtt(ms) {
      const n = Number(ms);
      if (!isFinite(n)) return "";
      return n < 10 ? `${n.toFixed(1)} ms` : `${Math.round(n)} ms`;
    }

    /** A line on the console when the companion comes up or goes away. */
    function noteTransition(state) {
      const was = probe.shown;
      probe.shown = state;
      if (!companion || was === state) return;
      const who = `${companion.name || "Companion computer"} (${companion.host})`;
      if (state === "online") api.console(`${PLUGIN_ID}: ${who} is online`, "success");
      else if (state === "offline") {
        api.console(`${PLUGIN_ID}: ${who} is offline, no reply for ${companion.timeout_s} s`, "error");
      }
    }

    function stopProbing() {
      probe.gen += 1;
      if (probe.timer !== null) { clearTimeout(probe.timer); probe.timer = null; }
      if (probe.paintTimer !== null) { clearTimeout(probe.paintTimer); probe.paintTimer = null; }
    }

    /** Start over: forget the old answers and probe the saved companion. */
    function startProbing() {
      stopProbing();
      probe.startedAt = Date.now();
      probe.lastOkAt = null;
      probe.rtt = null;
      probe.error = "";
      probe.shown = "";
      paintCompanion();
      if (!companion) return;
      const gen = probe.gen;
      const current = () => !cancelled && gen === probe.gen;
      const tick = () => {
        probe.timer = null;
        if (!current()) return;
        const began = Date.now();
        api.postJson("/api/local/ping", {
          host: companion.host,
          wait_ms: probeWaitMs(companion.interval_s),
        }).then((res) => {
          if (!current()) return;
          if (res && res.ok) {
            probe.error = "";
            if (res.reachable) {
              probe.lastOkAt = Date.now();
              probe.rtt = res.rtt_ms == null ? null : res.rtt_ms;
            }
          } else {
            probe.error = (res && res.error) || "The ping failed.";
          }
        }).catch((error) => {
          if (!current()) return;
          probe.error = (error && error.message) || "Could not reach the backend";
        }).then(() => {
          if (!current()) return;
          paintCompanion();
          const wait = Math.max(0, companion.interval_s * 1000 - (Date.now() - began));
          probe.timer = setTimeout(tick, wait);
        });
      };
      tick();
    }

    /** The popup: name, address, how often to ask and when to give up. */
    function openCompanionDialog() {
      if (companionDialog) return;
      const draft = companion ? Object.assign({}, companion) : {
        name: "", host: "", interval_s: DEFAULT_INTERVAL_S, timeout_s: DEFAULT_TIMEOUT_S,
      };
      const body = document.createElement("div");
      body.className = "schw-comp-form";

      body.appendChild(ui.field({
        label: "Name",
        control: ui.input({
          value: draft.name,
          placeholder: "Companion computer",
          ariaLabel: "Companion name",
          autocomplete: false,
          onInput: (v) => { draft.name = v; },
        }),
        hint: "Optional. Shown at the top of Schwalby.",
      }));

      const hostField = ui.field({
        label: "IP address",
        control: ui.input({
          value: draft.host,
          placeholder: "192.168.2.10",
          mono: true,
          ariaLabel: "Companion IP address",
          autocomplete: false,
          spellcheck: false,
          onInput: (v) => { draft.host = v.trim(); paintSave(); },
        }),
        hint: " ",
      });
      body.appendChild(hostField);

      const seconds = (list) => list.map((n) => ({ value: n, label: `${n} s` }));
      const intervalSlider = ui.slider({
        steps: seconds(INTERVAL_STEPS),
        value: draft.interval_s,
        ariaLabel: "Ping every",
        className: "schw-comp-slider",
        onInput: (v) => {
          draft.interval_s = v;
          if (draft.timeout_s <= v) {
            draft.timeout_s = timeoutFor(v);
            timeoutSlider.setValue(draft.timeout_s);
          }
        },
      });
      const timeoutSlider = ui.slider({
        steps: seconds(TIMEOUT_STEPS),
        value: draft.timeout_s,
        ariaLabel: "Offline after",
        className: "schw-comp-slider",
        onInput: (v) => {
          draft.timeout_s = v;
          if (draft.interval_s >= v) {
            draft.interval_s = intervalFor(v);
            intervalSlider.setValue(draft.interval_s);
          }
        },
      });
      body.appendChild(ui.field({
        label: "Ping every",
        className: "field-ruled",
        control: intervalSlider.el,
        info: "How often this computer sends the companion one ping.",
      }));
      body.appendChild(ui.field({
        label: "Offline after",
        control: timeoutSlider.el,
        info: "How long without a single reply before the companion is shown " +
              "as offline. Always longer than the ping interval, so one late " +
              "reply does not count as a lost companion.",
      }));

      const saveBtn = ui.button({
        variant: "primary",
        icon: "check",
        label: "Save",
        onClick: () => {
          const next = normalizeCompanion(draft);
          if (!next) return;
          companion = next;
          persist();
          dialog.close();
          startProbing();
        },
      });
      const clearBtn = ui.button({
        variant: "secondary",
        icon: "eraser",
        label: "Clear",
        title: "Forget the companion computer and stop pinging it",
        disabled: !companion,
        onClick: () => {
          companion = null;
          persist();
          dialog.close();
          startProbing();
        },
      });
      const exitBtn = ui.button({ variant: "ghost", label: "Exit", onClick: () => dialog.close() });

      function paintSave() {
        const problem = hostError(draft.host);
        saveBtn.disabled = !!problem;
        saveBtn.title = problem;
        // Only a wrong address is worth a line; an empty one is still being typed.
        setHint(hostField, draft.host ? problem : "");
      }
      paintSave();

      const dialog = ui.modal({
        title: "Companion computer",
        size: "sm",
        body,
        actions: [saveBtn, clearBtn, exitBtn],
        onClose: () => { companionDialog = null; },
      });
      companionDialog = dialog;
      dialog.open();
    }

    // ---- which sessions are up ----------------------------------------------

    /** Refresh the live-session map; never rejects. Unreachable means nothing
     *  is running, the safe reading. */
    function refreshLive() {
      return api.requestJson("/api/ssh/sessions").then((data) => {
        if (cancelled) return;
        const next = {};
        ((data && data.sessions) || []).forEach((sess) => {
          if (sess && sess.connected) next[sess.name] = true;
        });
        const changed = buttons.some((b) => !!live[sessionName(b)] !== !!next[sessionName(b)]);
        live = next;
        if (changed && editing === null) renderShelf();
      }).catch(() => {
        if (cancelled) return;
        if (Object.keys(live).length) { live = {}; if (editing === null) renderShelf(); }
      });
    }

    function startPolling() {
      stopPolling();
      pollTimer = setInterval(refreshLive, LIVE_POLL_MS);
    }
    function stopPolling() {
      if (pollTimer !== null) { clearInterval(pollTimer); pollTimer = null; }
    }

    // ---- the shelf ----------------------------------------------------------

    function renderShelf() {
      editing = null;
      repaintTarget = null;
      ui.clear(editorEl);
      ui.clear(shelfEl);

      const card = ui.card({});
      if (!buttons.length) {
        card.appendChild(ui.empty("No launch buttons yet. Add one for each program you start " +
                                  "before a flight, on this computer or on a companion computer."));
      } else {
        const list = document.createElement("div");
        list.className = "schw-shelf";
        buttons.forEach((entry) => list.appendChild(shelfRow(entry)));
        card.appendChild(list);
      }

      const last = buttons.length ? buttons[buttons.length - 1] : null;
      card.appendChild(ui.actions(ui.button({
        variant: "secondary",
        icon: "plus",
        label: "Add button",
        disabled: buttons.length >= MAX_BUTTONS,
        // A new button starts where the last one runs, in its folder: a shelf
        // is usually several programs on the same computer.
        onClick: () => renderEditor({
          id: newId(),
          label: "",
          target: last ? last.target : TARGET_LOCAL,
          connection: (last ? last.connection : "") || (connections.length ? connections[0].name : ""),
          directory: last ? last.directory : "",
          command: "",
          mode: MODE_TERMINAL,
        }, true),
      })));

      // Only worth saying once a button actually runs over SSH.
      if (connectionsError && buttons.some((b) => !isLocal(b))) {
        const note = ui.empty(connectionsError);
        note.classList.add("schw-note");
        card.appendChild(note);
      }

      shelfEl.appendChild(card);
      ui.refreshIcons();
      startPolling();
    }

    /** One row: the launch button, where it runs, why it last failed, the
     *  arrow into its terminal, and the way into its settings. */
    function shelfRow(entry) {
      const row = document.createElement("div");
      row.className = "schw-row";
      const running = !!live[sessionName(entry)];
      if (running) row.classList.add("schw-running");
      const failure = errors[entry.id];
      if (failure) row.classList.add("schw-failed");

      const launchBtn = ui.button({
        variant: "primary",
        size: "sm",
        icon: "play",
        label: entry.label,
        className: "schw-launch",
        ariaLabel: running ? `Run ${entry.label} again` : `Launch ${entry.label}`,
        title: running
          ? `Run it again in the terminal it is already in.\n${previewLine(entry) || entry.command}`
          : (previewLine(entry) || entry.command),
        onClick: () => launch(entry, launchBtn),
      });

      const tools = document.createElement("div");
      tools.className = "schw-row-tools";

      // Where it runs, in the segment's words.
      const where = document.createElement("span");
      where.className = "schw-where";
      where.textContent = targetInfo(entry.target).label;
      where.title = isLocal(entry) ? "Runs on this computer"
        : `Runs on ${entry.connection || "an SSH connection"} over SSH`;
      tools.appendChild(where);

      if (failure) {
        tools.appendChild(ui.infoHint({
          icon: "triangle-alert",
          size: 15,
          className: "schw-error",
          title: entry.label,
          text: failure,
          ariaLabel: `Why ${entry.label} did not run`,
        }));
      }

      if (running) {
        const dot = document.createElement("span");
        dot.className = "schw-dot";
        dot.title = "Running";
        tools.appendChild(dot);
      }

      // The arrow back to the button's own terminal, only while it is up.
      if (entry.mode !== MODE_BACKGROUND) {
        tools.appendChild(ui.iconButton("chevron-right", {
          className: "icon-btn schw-open",
          ariaLabel: `Open the terminal for ${entry.label}`,
          title: running
            ? "Open its terminal window to watch it, or press Ctrl-C to stop it"
            : "Not running. Launch it to open its terminal.",
          disabled: !running,
          onClick: () => openTerminal(entry),
        }));
      }

      tools.appendChild(ui.iconButton("pencil", {
        ariaLabel: `Edit ${entry.label}`,
        title: "Edit",
        onClick: () => renderEditor(Object.assign({}, entry), false),
      }));

      row.appendChild(launchBtn);
      row.appendChild(tools);
      return row;
    }

    // ---- the editor ---------------------------------------------------------

    // What the editor says about a field, per target.
    const HINTS = {
      local: {
        folderPlaceholder: "~/mission",
        folder: "Entered before the program starts. Leave empty to run in your " +
                "home folder.",
        program: "Run by your own login shell on this computer, so a pipeline " +
                 "or arguments work as typed. It runs as you and can do " +
                 "whatever your account can do.",
        terminal: "On: the program runs in a terminal of this button's own on " +
                  "this computer, and pressing the button again runs it again " +
                  "in that same terminal. The arrow beside the button opens the " +
                  "window to watch it in. Read its output there, and stop it " +
                  "with Ctrl-C or the window's disconnect button. Off: the " +
                  "program starts in the background, with nothing to watch. " +
                  "Either way it is stopped when Corvus closes.",
      },
      ssh: {
        folderPlaceholder: "/home/pilot/mission",
        folder: "Entered before the program starts. Leave empty to run in the " +
                "account's home directory.",
        program: "Run by the remote login shell, so a pipeline or arguments " +
                 "work as typed. It runs as the connection's own user and can " +
                 "do whatever that account can do.",
        terminal: "On: the program runs in an SSH session of this button's own, " +
                  "and pressing the button again runs it again in that same " +
                  "terminal. The arrow beside the button opens the window to " +
                  "watch it in. Read its output there, and stop it with Ctrl-C " +
                  "or the window's disconnect button. The session lasts as long " +
                  "as Corvus does. Off: the program is started with nohup and " +
                  "detached, so it survives Corvus closing, but there is nothing " +
                  "to watch and nothing to stop from here.",
      },
    };

    /** Rewrite the hint under a field built by ui.field; empty hides it. */
    function setHint(fieldEl, text) {
      const hint = fieldEl.querySelector(".field-hint");
      if (!hint) return;
      hint.textContent = text;
      hint.hidden = !text;
    }

    /** Rewrite what the hint icon beside a field's caption opens. */
    function setInfo(fieldEl, title, text) {
      const btn = fieldEl.querySelector(".ui-info");
      if (btn && btn.corvusPopover) btn.corvusPopover.setContent({ title, text });
    }

    /**
     * Show the form for one button.
     * @param {Object} draft   a COPY of the entry: nothing reaches the shelf
     *                         until Save, so Cancel really cancels.
     * @param {boolean} isNew  whether Save appends or replaces.
     */
    function renderEditor(draft, isNew) {
      editing = draft;
      stopPolling();
      status.hide();
      ui.clear(shelfEl);
      ui.clear(editorEl);
      draft.target = coerceTarget(draft);
      // The mode the operator chose. Where a terminal cannot be had it is
      // overridden while that is the target, and given back after.
      let wantedMode = draft.mode === MODE_BACKGROUND ? MODE_BACKGROUND : MODE_TERMINAL;

      const card = ui.card({});

      card.appendChild(ui.field({
        label: "Button",
        control: ui.input({
          value: draft.label,
          placeholder: "Start mission",
          ariaLabel: "Button label",
          autocomplete: false,
          onInput: (v) => { draft.label = v; },
        }),
        hint: "What the button says. Leave empty to use the command itself.",
      }));

      function isSsh() { return draft.target === TARGET_SSH; }

      const where = segment({
        ariaLabel: "Where it runs",
        value: draft.target,
        options: TARGETS,
        onChange: (next) => { draft.target = next; paintTarget(); paintPreview(); },
      });
      card.appendChild(ui.field({ label: "Runs on", control: where.el }));

      // Everything only an SSH button has, in one box a Local button hides.
      const sshEl = document.createElement("div");
      sshEl.className = "schw-ssh-fields";
      card.appendChild(sshEl);

      // The typed-in connection, held here rather than on the draft: it
      // carries a password, and the draft is what gets saved.
      const newConn = blankConnection();
      let nameInput = null;

      const connSel = ui.select({
        ariaLabel: "SSH connection",
        options: connectionOptions(),
        value: connections.length ? draft.connection : NEW_CONNECTION,
        onChange: () => { renderNewConnection(); paintPreview(); },
      });
      sshEl.appendChild(ui.field({
        label: "Connection",
        control: connSel,
        info: "One of the saved SSH connections, or a new one typed in below. " +
              "Either way the password is kept by the backend. This plugin " +
              "only ever names the connection.",
      }));

      const newConnEl = document.createElement("div");
      newConnEl.className = "schw-newconn";
      sshEl.appendChild(newConnEl);

      function pickedNew() { return isSsh() && connSel.value === NEW_CONNECTION; }

      /** The connection name this button will carry, "" for this computer. */
      function targetName() {
        if (!isSsh()) return "";
        if (!pickedNew()) return connSel.value || "";
        return String(newConn.name || "").trim() || derivedName(newConn);
      }

      const NEW_FIELDS = [
        { key: "host", label: "Host", placeholder: "192.168.2.10", mono: true },
        { key: "port", label: "Port", type: "number", mono: true },
        { key: "username", label: "User", placeholder: "corvus" },
        { key: "password", label: "Password", type: "password", mono: true,
          placeholder: "optional, or use a key file" },
        { key: "key_path", label: "Key file", mono: true,
          placeholder: "/home/you/.ssh/id_rsa" },
        { key: "name", label: "Save as", placeholder: "pilot@10.0.0.7",
          hint: "Leave it empty to use user@host.",
          info: "The name this connection is saved under. It joins the SSH " +
                "connections in Settings, so the next button can simply pick it." },
      ];

      /* Built only while in use: a password field that is merely hidden is
         still a password field in the page. */
      function renderNewConnection() {
        ui.clear(newConnEl);
        nameInput = null;
        newConnEl.hidden = !pickedNew();
        if (!pickedNew()) return;
        NEW_FIELDS.forEach((f) => {
          const control = ui.input({
            type: f.type || "text",
            value: newConn[f.key],
            placeholder: f.placeholder,
            mono: f.mono,
            ariaLabel: f.label,
            autocomplete: false,
            spellcheck: false,
            onInput: (v) => { newConn[f.key] = v; paintPreview(); },
          });
          if (f.key === "name") nameInput = control;
          newConnEl.appendChild(ui.field({ label: f.label, control, hint: f.hint, info: f.info }));
        });
      }

      const dirInput = ui.input({
        value: draft.directory,
        mono: true,
        ariaLabel: "Folder",
        autocomplete: false,
        spellcheck: false,
        onInput: (v) => { draft.directory = v.trim(); paintPreview(); },
      });
      const dirField = ui.field({ label: "Folder", control: dirInput, hint: " " });
      card.appendChild(dirField);

      const cmdInput = ui.input({
        value: draft.command,
        placeholder: "./start.sh",
        mono: true,
        ariaLabel: "Command to start",
        autocomplete: false,
        spellcheck: false,
        onInput: (v) => { draft.command = v.trim(); paintPreview(); },
      });
      const cmdField = ui.field({ label: "Program", control: cmdInput, info: " " });
      card.appendChild(cmdField);

      // Kept: the switch reads `disabled` from its options on every paint, so
      // a terminal that cannot be had here turns it off by flipping this.
      const switchOpts = {
        value: draft.mode !== MODE_BACKGROUND,
        ariaLabel: "Run in a terminal",
        onChange: (on) => {
          wantedMode = on ? MODE_TERMINAL : MODE_BACKGROUND;
          draft.mode = wantedMode;
          paintPreview();
        },
      };
      const terminalSwitch = ui.toggle(switchOpts);
      const termField = ui.field({
        label: "Run in a terminal",
        control: terminalSwitch.el,
        className: "field-switch",
        info: " ",
        hint: " ",
      });
      card.appendChild(termField);

      /** Fit the form to where the button runs. */
      function paintTarget() {
        const local = !isSsh();
        const words = local ? HINTS.local : HINTS.ssh;
        sshEl.hidden = local;
        renderNewConnection();
        dirInput.placeholder = words.folderPlaceholder;
        setHint(dirField, words.folder);
        setInfo(cmdField, "Program", words.program);
        const noTerminal = local ? localTerminalReason() : "";
        draft.mode = noTerminal ? MODE_BACKGROUND : wantedMode;
        switchOpts.disabled = !!noTerminal;
        terminalSwitch.setValue(draft.mode !== MODE_BACKGROUND);
        setInfo(termField, "Run in a terminal", words.terminal);
        // Why the switch is off stays under it: that is the state of this
        // computer, not an explanation to go looking for.
        setHint(termField, noTerminal);
      }
      paintTarget();
      repaintTarget = () => { paintTarget(); paintPreview(); };

      const preview = document.createElement("div");
      preview.className = "schw-preview";
      card.appendChild(preview);

      const saveBtn = ui.button({
        variant: "primary",
        icon: "check",
        label: "Save",
        onClick: () => {
          if (pickedNew()) saveNewConnectionThenButton();
          else {
            if (isSsh()) draft.connection = connSel.value || "";
            commit();
          }
        },
      });

      /** Put the finished draft on the shelf and go back to it. */
      function commit() {
        const entry = coerceButton(draft);
        if (!entry) return;
        const before = isNew ? null : buttons.find((b) => b.id === entry.id);
        if (before && !leaveSession(before, entry)) return;
        if (isNew) buttons.push(entry);
        else buttons = buttons.map((b) => (b.id === entry.id ? entry : b));
        persist();
        renderShelf();
      }

      /** Save the typed-in connection first, then the button that names it,
       *  so a button never names a connection the backend does not have. */
      function saveNewConnectionThenButton() {
        const body = newConnectionBody(newConn);
        ui.setBusy(saveBtn, true);
        api.postJson("/api/ssh/connections", body).then((res) => {
          if (cancelled) return;
          ui.setBusy(saveBtn, false);
          if (!(res && res.ok)) {
            status.show((res && res.error) || "The connection could not be saved.", "err");
            return;
          }
          if (Array.isArray(res.connections)) connections = res.connections;
          connectionsError = connections.length ? "" : connectionsError;
          draft.connection = body.name;
          commit();
        }).catch((error) => {
          if (cancelled) return;
          ui.setBusy(saveBtn, false);
          status.show(error.message || "Could not reach the backend", "err");
        });
      }

      const cancelBtn = ui.button({ variant: "ghost", label: "Cancel", onClick: () => renderShelf() });
      const deleteBtn = isNew ? null : ui.button({
        variant: "danger",
        icon: "trash-2",
        label: "Delete",
        title: "Remove this button",
        onClick: () => remove(draft, !!live[sessionName(draft)]),
      });
      card.appendChild(ui.actions([saveBtn, deleteBtn, cancelBtn]));

      function paintPreview() {
        const target = targetName();
        const line = previewLine(Object.assign({}, draft, { connection: target }));
        preview.textContent = line || "Name a program to see the command.";
        preview.classList.toggle("schw-preview-empty", !line);
        if (nameInput) nameInput.placeholder = derivedName(newConn) || "pilot@10.0.0.7";

        // Nothing to save until there is a command and somewhere to run it.
        // This computer is always somewhere to run it.
        let connError = "";
        if (pickedNew()) connError = newConnectionError(newConn, connections.map((c) => c.name));
        else if (isSsh() && !target) connError = "Pick a connection to run it on.";
        saveBtn.disabled = !line || !!connError;
        saveBtn.title = connError || (line ? "" : "Name a program first.");
        if (connError && connError.indexOf("already saved") >= 0) status.show(connError, "warn");
        else status.hide();
      }
      paintPreview();

      editorEl.appendChild(card);
      ui.refreshIcons();
    }

    /** The saved connections, then the way to type one in. */
    function connectionOptions() {
      const saved = connections.map((c) => {
        const address = c.username ? `${c.username}@${c.host}` : String(c.host || "");
        return {
          value: c.name,
          label: (address && address !== c.name) ? `${c.name} (${address})` : c.name,
        };
      });
      saved.push({ value: NEW_CONNECTION, label: "New connection…" });
      return saved;
    }

    function connectionFor(entry) {
      return connections.find((c) => c.name === entry.connection) || {};
    }

    // ---- running one --------------------------------------------------------

    /** The console line, naming where the program was started. */
    function consoleLine(entry, line) {
      const where = isLocal(entry) ? LOCAL_HOST : (entry.connection || "ssh");
      return `${PLUGIN_ID} (${where}): ${line}`;
    }

    function launch(entry, btn) {
      status.hide();
      output.hidden = true;
      output.textContent = "";
      clearError(entry);
      ui.setBusy(btn, true);
      const done = () => { if (!cancelled) ui.setBusy(btn, false); };
      if (entry.mode === MODE_BACKGROUND) launchDetached(entry).finally(done);
      else launchInTerminal(entry).finally(done);
    }

    /**
     * TERMINAL mode: type the line into the button's own session, and open one
     * only when it has none. Send first, connect only if that fails, so a
     * second press runs it again in the same shell instead of replacing it.
     * The window is not opened here; the arrow is the request to watch it.
     */
    function launchInTerminal(entry) {
      const session = sessionName(entry);
      const line = terminalLine(entry);
      const send = () => api.postJson("/api/ssh/send", { name: session, data: line + "\n" });
      // A session being closed because the button moved elsewhere must be gone
      // before the next line is sent, or the line lands in it.
      const pending = closing[session];
      return (pending ? pending.then(send) : send())
        .then((res) => {
          if (cancelled) return null;
          if (res && res.ok) return ran(entry, line, false);
          return connectThenSend(entry, line);
        })
        .catch((error) => {
          if (cancelled) return null;
          failed(entry, error.message || "Could not reach the backend");
          return null;
        });
    }

    /** Open this button's session (a shell here, or over SSH), then type. */
    function connectThenSend(entry, line) {
      const session = sessionName(entry);
      const opening = isLocal(entry)
        ? api.postJson("/api/local/connect", { name: session })
        : api.postJson("/api/ssh/connect", { name: session, from: entry.connection });
      return opening.then((res) => {
        if (cancelled) return null;
        if (!(res && res.ok && res.connected)) {
          failed(entry, (res && res.error) || "Could not open the terminal");
          return null;
        }
        return api.postJson("/api/ssh/send", { name: session, data: line + "\n" })
          .then(() => (cancelled ? null : ran(entry, line, true)));
      });
    }

    /** Record a launch. A window left open on a replaced shell is quietly
     *  repointed at the new one, without being opened or raised. */
    function ran(entry, line, reconnected) {
      const session = sessionName(entry);
      const was = !!live[session];
      live[session] = true;
      if (reconnected) openTerminal(entry, true, true);
      status.show(was && !reconnected
        ? `${entry.label}: sent again to its terminal. The arrow opens it.`
        : `${entry.label}: running. The arrow opens its terminal.`, "ok");
      api.console(consoleLine(entry, line), "success");
      renderShelf();
      return true;
    }

    /** BACKGROUND mode: one shot, here or over SSH. */
    function launchDetached(entry) {
      const local = isLocal(entry);
      const request = local
        ? api.postJson("/api/local/run", { directory: entry.directory, command: entry.command })
        : api.postJson("/api/ssh/run", {
          name: entry.connection,
          directory: entry.directory,
          command: entry.command,
          detach: true,
        });
      return request.then((res) => {
        if (cancelled) return;
        const r = res || {};
        const summary = local ? localSummary(r) : sshSummary(r);
        if (summary.kind === "err") failed(entry, summary.text);
        else status.show(`${entry.label}: ${summary.text}`, summary.kind);
        const body = [r.stdout, r.stderr, r.output]
          .map((part) => String(part || "").trim())
          .filter(Boolean)
          .join("\n");
        if (body && !r.ok) {
          output.textContent = body;
          output.hidden = false;
        }
        api.console(consoleLine(entry, r.command || entry.command), r.ok ? "success" : "error");
      }).catch((error) => {
        if (cancelled) return;
        failed(entry, error.message || "Could not reach the backend");
      });
    }

    /** Close a button's session, remembering it until the backend has. */
    function closeSession(entry) {
      const session = sessionName(entry);
      delete live[session];
      const done = api.postJson("/api/ssh/disconnect", { name: session })
        .catch(() => {})
        .then(() => { if (closing[session] === done) delete closing[session]; });
      closing[session] = done;
    }

    /**
     * Whether an edit may be saved, given the session the button has open.
     * Moved to another machine, its next press would still type into the old
     * shell (the send goes to the live session by name first); switched to the
     * background, nothing on the shelf would lead to it. Either way it has to
     * go, and because that stops a running program, this asks first.
     *
     * @returns {boolean} false when the operator would rather keep it
     */
    function leaveSession(before, after) {
      if (!live[sessionName(before)]) return true;
      const moved = before.target !== after.target
        || (after.target === TARGET_SSH && before.connection !== after.connection);
      const detached = before.mode !== MODE_BACKGROUND && after.mode === MODE_BACKGROUND;
      if (!moved && !detached) return true;
      if (!window.confirm(
        `"${before.label}" is still running in its terminal.\n\nSaving this ` +
        `closes that session, which stops what it is running.`)) return false;
      closeSession(before);
      return true;
    }

    /** Remove a button, and the session only it could reach. */
    function remove(entry, running) {
      if (running && !window.confirm(
        `Remove "${entry.label}"?\n\nIts session will be closed, ` +
        `which stops what it is running.`)) return;
      buttons = buttons.filter((b) => b.id !== entry.id);
      clearError(entry);
      persist();
      if (running) closeSession(entry);
      renderShelf();
    }

    /**
     * Show the terminal window a button's program runs in, raising it when it
     * is already open.
     * @param {Object} entry
     * @param {boolean} [reattach]     a new shell is behind the session name
     * @param {boolean} [existingOnly] repair an open window, open none
     */
    function openTerminal(entry, reattach, existingOnly) {
      let where;
      if (isLocal(entry)) {
        where = { host: LOCAL_HOST };
      } else {
        const conn = connectionFor(entry);
        where = { host: conn.host, port: conn.port, username: conn.username };
      }
      const shown = api.terminal(Object.assign({
        name: sessionName(entry),
        title: entry.label,
      }, where), { reattach: !!reattach, existingOnly: !!existingOnly });
      if (!shown && !existingOnly) failed(entry, "Its terminal window could not be opened.");
    }

    // ---- boot -----------------------------------------------------------------

    renderShelf();

    api.requestJson("/api/ssh/connections").then((data) => {
      if (cancelled) return;
      connections = (data && Array.isArray(data.connections)) ? data.connections : [];
      connectionsError = connections.length ? ""
        : "No saved SSH connections yet. Add a button, pick SSH and choose " +
          "“New connection…” to enter one here.";
      if (editing === null) renderShelf();
    }).catch(() => {
      if (cancelled) return;
      connections = [];
      connectionsError = "Could not read the saved SSH connections.";
      if (editing === null) renderShelf();
    });

    // Whether a terminal can be had on this computer. Unanswered, the switch
    // stays the operator's, and the backend says why when a button is pressed.
    api.requestJson("/api/local/status").then((data) => {
      if (cancelled) return;
      localStatus = data || null;
      if (repaintTarget) repaintTarget();
    }).catch(() => {});

    // Sessions survive the plugin being closed; find the ones still up.
    refreshLive();

    startProbing();

    containerEl._schwDestroy = function () {
      cancelled = true;
      stopPolling();
      stopProbing();
      if (companionDialog) companionDialog.close();
      containerEl._schwDestroy = null;
    };
  }

  function destroy(containerEl) {
    if (!containerEl) return;
    if (typeof containerEl._schwDestroy === "function") containerEl._schwDestroy();
  }

  return {
    init, destroy,
    previewLine, terminalLine, sshSummary, localSummary, normalizeButtons,
    coerceButton, coerceMode, coerceTarget, sessionName, derivedName,
    newConnectionError, newConnectionBody, segment,
    hostError, normalizeCompanion, companionState, probeWaitMs,
    MAX_BUTTONS, LIVE_POLL_MS, MODE_TERMINAL, MODE_BACKGROUND, NEW_CONNECTION,
    TARGET_LOCAL, TARGET_SSH, LOCAL_HOST,
    INTERVAL_STEPS, TIMEOUT_STEPS, DEFAULT_INTERVAL_S, DEFAULT_TIMEOUT_S,
  };
})();

// Registered as this script runs; the grid re-renders on every register().
if (window.Corvus && Corvus.plugins && typeof Corvus.plugins.register === "function") {
  Corvus.plugins.register("schwalby", {
    name: "Schwalby",
    icon: "bird",
    description: "One-press buttons that start programs on this computer or over SSH",
    init: function (containerEl, api) { Corvus.pluginSchwalby.init(containerEl, api); },
    destroy: function (containerEl) { Corvus.pluginSchwalby.destroy(containerEl); },
  });
}
