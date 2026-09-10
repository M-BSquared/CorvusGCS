"use strict";
window.Corvus = window.Corvus || {};

/**
 * SSH Launcher — a plugin for the TOOLS tab.
 *
 * The job the operator has: on the companion computer (or a ground server)
 * there are things that have to be started before a flight — the mission
 * script, the video pipeline, a log recorder — and doing each by hand means
 * opening the SSH tab, waiting for a shell, remembering a path, and typing a
 * command with the aircraft already on the pad. This is a shelf of buttons
 * for exactly those, one press each.
 *
 * Two ways to run one, per button:
 *
 *   TERMINAL (the default)  Opens an interactive SSH session of its own and
 *     types `cd -- <folder> && <command>` into it. The arrow beside the button
 *     opens that terminal, so the program's output is the operator's to read
 *     and Ctrl-C is theirs to press — this is a real shell, not a captured
 *     one. The session lives in the backend's SSH bridge for as long as Corvus
 *     runs, and closing it (DISCONNECT in the terminal) stops the program.
 *
 *   BACKGROUND  One-shot `POST /api/ssh/run` with nohup. Nothing to watch and
 *     nothing to stop from here, but the program outlives the connection, the
 *     request and Corvus itself — which is what a recorder that has to survive
 *     a laptop reboot needs.
 *
 * Either way the credentials come from a saved SSH connection, resolved on the
 * backend, so neither the browser nor this plugin ever holds a password: a
 * button stores the connection's NAME.
 *
 * The shelf is the plugin's state: a list of {id, label, connection, directory,
 * command, mode}, saved through api.saveSettings, so the buttons are there on
 * the next start. Nothing secret goes in there.
 *
 * Two views, never both: the SHELF (the buttons plus Add) and the EDITOR (one
 * button's fields). Editing is a mode rather than an expanding row because the
 * panel is narrow and a form beside a list of buttons in ~360px reads as
 * neither.
 *
 * This file is also the worked example the plugin folder's README points at:
 * a manifest, one script, one stylesheet, `Corvus.plugins.register` at the
 * bottom, and an init/destroy pair that leaves nothing behind — including the
 * liveness poll below.
 */
Corvus.pluginSshLauncher = (function () {
  // Bounded so the shelf stays a shelf. Past this the panel scrolls and the
  // "one press" premise is gone; an operator with more of them wants a script
  // on the far end, not more buttons here.
  const MAX_BUTTONS = 12;

  // How often the shelf asks which of its sessions are still up. There is no
  // push for this — the SSH bridge has no event stream for session liveness —
  // and the answer is what decides whether a button's arrow leads anywhere, so
  // a stale dot would send the operator to a dead terminal. Slow enough to be
  // free, and only while the shelf is the visible view.
  const LIVE_POLL_MS = 4000;

  const MODE_TERMINAL = "terminal";
  const MODE_BACKGROUND = "background";

  /** A short, collision-free id for a new button. Never shown. */
  function newId() {
    return "b" + Date.now().toString(36) + Math.floor(Math.random() * 1e6).toString(36);
  }

  /**
   * The SSH session a button runs in. Keyed by the button's id, not its label:
   * renaming a button must not orphan the session its program is running in.
   * @param {Object} entry
   * @returns {string}
   */
  function sessionName(entry) {
    return "ssh-launcher/" + (entry && entry.id ? entry.id : "");
  }

  /**
   * Coerce one saved entry into a complete button, or null to drop it.
   * Everything is defensive: this reads a JSON file an operator may have
   * edited, and a half-written entry must cost that button and not the shelf.
   * @param {*} raw
   * @returns {Object|null}
   */
  function coerceButton(raw) {
    if (!raw || typeof raw !== "object") return null;
    const command = String(raw.command == null ? "" : raw.command).trim();
    if (!command) return null;              // a button with nothing to run is not one
    const directory = String(raw.directory == null ? "" : raw.directory).trim();
    const label = String(raw.label == null ? "" : raw.label).trim();
    return {
      id: (typeof raw.id === "string" && raw.id) ? raw.id : newId(),
      // An unnamed button still needs a face: the command is what the operator
      // would have typed anyway, so it is the honest default.
      label: label || command,
      connection: String(raw.connection == null ? "" : raw.connection).trim(),
      directory,
      command,
      mode: coerceMode(raw),
    };
  }

  /**
   * A button's run mode, including the migration from the `detach` boolean
   * this plugin used before it grew a terminal: `detach: true` meant "start it
   * with nohup and let it run", which is exactly BACKGROUND, and `false` meant
   * a foreground command whose output the operator wanted — that is TERMINAL
   * now, and a better answer to the same wish.
   * @param {Object} raw
   * @returns {string}
   */
  function coerceMode(raw) {
    if (raw.mode === MODE_BACKGROUND || raw.mode === MODE_TERMINAL) return raw.mode;
    if (typeof raw.detach === "boolean") return raw.detach ? MODE_BACKGROUND : MODE_TERMINAL;
    return MODE_TERMINAL;
  }

  /**
   * The button list from whatever is in the plugin's saved settings.
   *
   * Also migrates the single-command shape this plugin started with
   * ({connection, directory, command, detach} at the top level) into a one-entry
   * shelf, so an operator who configured it before does not lose their command.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} saved api.getSettings()
   * @returns {Object[]} complete buttons, capped at MAX_BUTTONS
   */
  function normalizeButtons(saved) {
    const s = saved || {};
    const source = Array.isArray(s.buttons) ? s.buttons : [s];
    const out = [];
    source.forEach((raw) => {
      const button = coerceButton(raw);
      if (button && out.length < MAX_BUTTONS) out.push(button);
    });
    return out;
  }

  /**
   * The shell line a button types into its terminal, or sends to /api/ssh/run.
   *
   * The directory is quoted for the REMOTE shell and `--` guards a folder whose
   * name begins with a dash. The command is passed through as the operator
   * typed it — quoting it would break every pipeline and argument in it, and
   * the SSH account's own permissions are the boundary here, exactly as they
   * are when they type it in the SSH tab themselves.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} entry {directory, command}
   * @returns {string}
   */
  function remoteLine(entry) {
    const e = entry || {};
    const command = String(e.command || "").trim();
    if (!command) return "";
    const directory = String(e.directory || "").trim();
    if (!directory) return command;
    // Single quotes with the POSIX escape for an embedded quote, because this
    // is read by the shell on the far end, not by the local one.
    return "cd -- '" + directory.replace(/'/g, "'\\''") + "' && " + command;
  }

  /**
   * Compose the human-readable preview of what a button will run.
   *
   * Shown, not sent: in background mode the backend composes and quotes the
   * real line, and in terminal mode remoteLine() does. Pure and exported so
   * the test suite can pin the preview without a DOM.
   *
   * @param {Object} cfg {connection, directory, command, mode}
   * @returns {string} the preview line, or "" when there is nothing to show
   */
  function previewLine(cfg) {
    const c = cfg || {};
    const command = String(c.command || "").trim();
    if (!command) return "";
    const directory = String(c.directory || "").trim();
    const body = coerceMode(c) === MODE_BACKGROUND ? `nohup ${command} &` : command;
    const remote = directory ? `cd ${directory} && ${body}` : body;
    const target = String(c.connection || "").trim();
    return target ? `ssh ${target} '${remote}'` : remote;
  }

  /**
   * One-line summary of a finished background run, for the status message.
   * @param {Object} res the POST /api/ssh/run response body
   * @returns {{text: string, kind: string}}
   */
  function resultSummary(res) {
    const r = res || {};
    if (!r.ok) {
      const detail = String(r.stderr || r.error || "").trim().split("\n")[0];
      return { text: detail || "The command failed.", kind: "err" };
    }
    // With nohup the stdout is the pid the remote shell echoed — the one piece
    // of proof that something actually started.
    const pid = String(r.stdout || "").trim();
    return { text: pid ? `Started in the background (pid ${pid}).` : "Started in the background.", kind: "ok" };
  }

  function init(containerEl, api) {
    const ui = Corvus.ui;
    containerEl.innerHTML = "";

    let buttons = normalizeButtons(
      (api && typeof api.getSettings === "function") ? api.getSettings() : {});
    let connections = [];        // saved SSH connections, filled by the fetch below
    let connectionsError = "";   // why the list is empty, when it is
    let editing = null;          // the button being edited, or null on the shelf
    let live = {};               // session name -> true, from /api/ssh/sessions
    let pollTimer = null;
    let cancelled = false;       // set by destroy(); gates every late callback

    const shelfEl = document.createElement("div");
    const editorEl = document.createElement("div");
    const status = ui.message({});
    const output = document.createElement("pre");
    output.className = "sshl-output";
    output.hidden = true;

    containerEl.appendChild(shelfEl);
    containerEl.appendChild(editorEl);
    containerEl.appendChild(status.el);
    containerEl.appendChild(output);

    // ---- persistence ------------------------------------------------------

    /** Save the shelf. Written with `replace` because the shelf IS this
     *  plugin's settings — a merge would leave the single-command keys this
     *  plugin used before it grew one lying in the config file forever.
     *  Best-effort: a failed write costs the buttons on the next start, never
     *  the run the operator is doing now. */
    function persist() {
      if (!api || typeof api.saveSettings !== "function") return;
      api.saveSettings({ buttons: buttons }, true).catch(() => {});
    }

    // ---- which sessions are up --------------------------------------------

    /**
     * Refresh the live-session map and repaint the shelf if it changed.
     * Never rejects: an unreachable backend means "nothing is running", which
     * is the safe reading — a disabled arrow, not a promise of a terminal.
     */
    function refreshLive() {
      return api.requestJson("/api/ssh/sessions").then((data) => {
        if (cancelled) return;
        const next = {};
        ((data && data.sessions) || []).forEach((s) => {
          if (s && s.connected) next[s.name] = true;
        });
        const changed = buttons.some((b) => !!live[sessionName(b)] !== !!next[sessionName(b)]);
        live = next;
        if (changed && editing === null) renderShelf();
      }).catch(() => {
        if (cancelled) return;
        if (Object.keys(live).length) { live = {}; if (editing === null) renderShelf(); }
      });
    }

    /** Poll only while the shelf is what the operator is looking at. */
    function startPolling() {
      stopPolling();
      pollTimer = setInterval(refreshLive, LIVE_POLL_MS);
    }
    function stopPolling() {
      if (pollTimer !== null) { clearInterval(pollTimer); pollTimer = null; }
    }

    // ---- the shelf --------------------------------------------------------

    function renderShelf() {
      editing = null;
      ui.clear(editorEl);
      ui.clear(shelfEl);

      const card = ui.card({});

      if (!buttons.length) {
        card.appendChild(ui.empty(
          "No launch buttons yet. Add one for each program you start before a flight."));
      } else {
        const list = document.createElement("div");
        list.className = "sshl-shelf";
        buttons.forEach((entry) => list.appendChild(shelfRow(entry)));
        card.appendChild(list);
      }

      const addBtn = ui.button({
        variant: "secondary",
        icon: "plus",
        label: "Add button",
        disabled: buttons.length >= MAX_BUTTONS,
        // A new button inherits the connection and folder the last one uses: a
        // shelf is almost always several programs on the same companion
        // computer, often in the same place.
        onClick: () => renderEditor({
          id: newId(),
          label: "",
          connection: (buttons.length ? buttons[buttons.length - 1].connection : "")
            || (connections.length ? connections[0].name : ""),
          directory: buttons.length ? buttons[buttons.length - 1].directory : "",
          command: "",
          mode: MODE_TERMINAL,
        }, true),
      });
      card.appendChild(ui.actions(addBtn));

      if (connectionsError) {
        const note = ui.empty(connectionsError);
        note.className = "sshl-note";
        card.appendChild(note);
      }

      shelfEl.appendChild(card);
      ui.refreshIcons();
      startPolling();
    }

    /** One row: the launch button, the arrow into its terminal, edit, remove. */
    function shelfRow(entry) {
      const row = document.createElement("div");
      row.className = "sshl-row";
      const running = !!live[sessionName(entry)];
      if (running) row.classList.add("sshl-running");

      const launchBtn = ui.button({
        variant: "primary",
        size: "sm",
        // Pressing a button whose program is already up restarts it (the
        // backend replaces a same-name session), so the icon says restart
        // rather than promising a second copy.
        icon: running ? "rotate-cw" : "play",
        label: entry.label,
        className: "sshl-launch",
        ariaLabel: running ? `Restart ${entry.label}` : `Launch ${entry.label}`,
        // The composed line as a tooltip: the row shows the label, and the
        // label is often nothing like the command it stands for.
        title: running
          ? `Restart — this stops what is running first.\n${previewLine(entry) || entry.command}`
          : (previewLine(entry) || entry.command),
        onClick: () => launch(entry, launchBtn),
      });

      const tools = document.createElement("div");
      tools.className = "sshl-row-tools";

      // A live dot beside the arrow, on the card's own surface rather than on
      // the filled launch button where a semantic green would fight the accent.
      // It is the one thing that says "this is up right now".
      if (running) {
        const dot = document.createElement("span");
        dot.className = "sshl-dot";
        dot.title = "Running";
        tools.appendChild(dot);
      }

      // The arrow into the terminal this button's program is running in. Only
      // terminal-mode buttons have one to go to, and only while the session is
      // actually up — an arrow that led to a dead terminal would be worse than
      // no arrow, so it says why it is off instead.
      if (entry.mode !== MODE_BACKGROUND) {
        const openBtn = ui.iconButton("chevron-right", {
          className: "icon-btn sshl-open",
          ariaLabel: `Open the terminal for ${entry.label}`,
          title: running
            ? "Open the terminal — watch it, or press Ctrl-C to stop it"
            : "Not running. Launch it to open its terminal.",
          disabled: !running,
          onClick: () => openTerminal(entry),
        });
        tools.appendChild(openBtn);
      }

      tools.appendChild(ui.iconButton("pencil", {
        ariaLabel: `Edit ${entry.label}`,
        title: "Edit",
        onClick: () => renderEditor(Object.assign({}, entry), false),
      }));
      tools.appendChild(ui.iconButton("trash-2", {
        ariaLabel: `Remove ${entry.label}`,
        title: running ? "Remove — this also stops what it is running" : "Remove",
        onClick: () => remove(entry, running),
      }));

      row.appendChild(launchBtn);
      row.appendChild(tools);
      return row;
    }

    // ---- the editor -------------------------------------------------------

    /**
     * Show the form for one button.
     * @param {Object} draft   a COPY of the entry — nothing is written back to
     *                         the shelf until Save, so Cancel really cancels.
     * @param {boolean} isNew  whether Save appends or replaces.
     */
    function renderEditor(draft, isNew) {
      editing = draft;
      stopPolling();
      status.hide();
      ui.clear(shelfEl);
      ui.clear(editorEl);

      const card = ui.card({});

      const labelInput = ui.input({
        value: draft.label,
        placeholder: "Start mission",
        ariaLabel: "Button label",
        autocomplete: false,
        onInput: (v) => { draft.label = v; },
      });
      card.appendChild(ui.field({
        label: "Button",
        control: labelInput,
        hint: "What the button says. Leave empty to use the command itself.",
      }));

      const connSel = ui.select({
        ariaLabel: "SSH connection",
        options: connectionOptions(),
        value: draft.connection,
        onChange: (v) => { draft.connection = v; paintPreview(); },
      });
      card.appendChild(ui.field({
        label: "Connection",
        control: connSel,
        hint: "One of the SSH connections saved in Settings. The password stays " +
              "on the backend — this plugin only ever names the connection.",
      }));
      // The select coerces an unknown value to its first option, so the draft
      // takes back whatever it actually settled on.
      draft.connection = connSel.value || "";

      const dirInput = ui.input({
        value: draft.directory,
        placeholder: "/home/pilot/mission",
        mono: true,
        ariaLabel: "Remote folder",
        autocomplete: false,
        spellcheck: false,
        onInput: (v) => { draft.directory = v.trim(); paintPreview(); },
      });
      card.appendChild(ui.field({
        label: "Folder",
        control: dirInput,
        hint: "Entered before the program starts. Leave empty to run in the " +
              "account's home directory.",
      }));

      const cmdInput = ui.input({
        value: draft.command,
        placeholder: "./start.sh",
        mono: true,
        ariaLabel: "Command to start",
        autocomplete: false,
        spellcheck: false,
        onInput: (v) => { draft.command = v.trim(); paintPreview(); },
      });
      card.appendChild(ui.field({
        label: "Program",
        control: cmdInput,
        hint: "Run by the remote login shell, so a pipeline or arguments work " +
              "as typed. It runs as the connection's own user and can do " +
              "whatever that account can do.",
      }));

      const terminalSwitch = ui.toggle({
        value: draft.mode !== MODE_BACKGROUND,
        ariaLabel: "Run in a terminal",
        onChange: (on) => { draft.mode = on ? MODE_TERMINAL : MODE_BACKGROUND; paintPreview(); },
      });
      card.appendChild(ui.field({
        label: "Run in a terminal",
        control: terminalSwitch.el,
        className: "field-switch",
        hint: "On: the program runs in its own SSH terminal, and the arrow " +
              "beside the button opens it — read its output there, and stop it " +
              "with Ctrl-C or DISCONNECT. The terminal lasts as long as Corvus " +
              "does. Off: the program is started with nohup and detached, so it " +
              "survives Corvus closing — but there is nothing to watch and " +
              "nothing to stop from here.",
      }));

      const preview = document.createElement("div");
      preview.className = "sshl-preview";
      card.appendChild(preview);

      const saveBtn = ui.button({
        variant: "primary",
        icon: "check",
        label: "Save",
        onClick: () => {
          const entry = coerceButton(draft);
          if (!entry) return;                      // guarded by paintPreview anyway
          if (isNew) buttons.push(entry);
          else buttons = buttons.map((b) => (b.id === entry.id ? entry : b));
          persist();
          renderShelf();
        },
      });
      const cancelBtn = ui.button({
        variant: "ghost",
        label: "Cancel",
        onClick: () => renderShelf(),
      });
      card.appendChild(ui.actions([saveBtn, cancelBtn]));

      function paintPreview() {
        const line = previewLine(draft);
        preview.textContent = line || "Name a program to see the command.";
        preview.classList.toggle("sshl-preview-empty", !line);
        // Nothing to save until there is a command AND somewhere to run it.
        saveBtn.disabled = !line || !draft.connection;
      }
      paintPreview();

      editorEl.appendChild(card);
      ui.refreshIcons();
    }

    /** Options for the connection <select>, including the empty states. */
    function connectionOptions() {
      if (!connections.length) {
        return [{ value: "", label: connectionsError || "No saved connections", disabled: true }];
      }
      return connections.map((c) => ({
        value: c.name,
        label: c.username ? `${c.name} — ${c.username}@${c.host}` : `${c.name} — ${c.host}`,
      }));
    }

    /** The saved connection a button points at, for the terminal's header. */
    function connectionFor(entry) {
      return connections.find((c) => c.name === entry.connection) || {};
    }

    // ---- running one --------------------------------------------------------

    function launch(entry, btn) {
      status.hide();
      output.hidden = true;
      output.textContent = "";
      ui.setBusy(btn, true);
      const done = () => { if (!cancelled) ui.setBusy(btn, false); };
      if (entry.mode === MODE_BACKGROUND) launchDetached(entry).finally(done);
      else launchInTerminal(entry).finally(done);
    }

    /**
     * TERMINAL mode: open a session of this button's own and type the line into
     * it. Connect first every time — the backend replaces a same-name session,
     * so pressing a button whose program has finished gives a fresh shell, and
     * pressing one that is still running restarts it rather than typing a
     * second command into a shell that is busy with the first.
     */
    function launchInTerminal(entry) {
      const session = sessionName(entry);
      return api.postJson("/api/ssh/connect", { name: session, from: entry.connection })
        .then((res) => {
          if (cancelled) return null;
          if (!(res && res.ok && res.connected)) {
            const detail = (res && res.error) || "Could not open the terminal";
            status.show(`${entry.label}: ${detail}`, "err");
            api.notification("warning", `SSH Launcher — ${entry.label}: ${detail}`);
            return null;
          }
          const line = remoteLine(entry);
          return api.postJson("/api/ssh/send", { name: session, data: line + "\n" })
            .then(() => {
              if (cancelled) return null;
              live[session] = true;
              status.show(`${entry.label}: running — the arrow opens its terminal.`, "ok");
              // The console is the app's shared record of what was commanded; a
              // program started on a companion computer belongs in it.
              api.console(`ssh-launcher: ${line}`, "success");
              renderShelf();
              return res;
            });
        })
        .catch((error) => {
          if (cancelled) return null;
          status.show(error.message || "Could not reach the backend", "err");
          return null;
        });
    }

    /** BACKGROUND mode: one-shot nohup, nothing left to watch. */
    function launchDetached(entry) {
      // postJson, not postAction: a command that fails puts the reason in
      // stderr, and that is the whole value of the response.
      return api.postJson("/api/ssh/run", {
        name: entry.connection,
        directory: entry.directory,
        command: entry.command,
        detach: true,
      }).then((res) => {
        if (cancelled) return;
        const summary = resultSummary(res);
        status.show(`${entry.label}: ${summary.text}`, summary.kind);
        const body = [res.stdout, res.stderr]
          .map((part) => String(part || "").trim())
          .filter(Boolean)
          .join("\n");
        // On success the only stdout is the pid, which the status line already
        // says; showing the box as well would be noise.
        if (body && !res.ok) {
          output.textContent = body;
          output.hidden = false;
        }
        api.console(`ssh-launcher: ${res.command || entry.command}`, res.ok ? "success" : "error");
        if (!res.ok) api.notification("warning", `SSH Launcher — ${entry.label}: ${summary.text}`);
      }).catch((error) => {
        if (cancelled) return;
        status.show(error.message || "Could not reach the backend", "err");
      });
    }

    /**
     * Remove a button, and with it the terminal it owns.
     *
     * The button is the only handle on its session — nothing else in the app
     * lists it — so leaving the session behind would leave a program running
     * that nobody can reach or stop until Corvus exits. Closing it is
     * therefore the right default, and because that stops a running program it
     * is the one action here that asks first.
     */
    function remove(entry, running) {
      if (running && !window.confirm(
        `Remove "${entry.label}"?\n\nIts terminal is open and will be closed, ` +
        `which stops what it is running.`)) return;
      buttons = buttons.filter((b) => b.id !== entry.id);
      persist();
      if (running) {
        const session = sessionName(entry);
        delete live[session];
        api.postJson("/api/ssh/disconnect", { name: session }).catch(() => {});
      }
      renderShelf();
    }

    /** Show the terminal a button's program is running in. */
    function openTerminal(entry) {
      const conn = connectionFor(entry);
      const shown = api.terminal({
        name: sessionName(entry),
        // The session key is an id; the header should read the button's name.
        title: entry.label,
        host: conn.host,
        port: conn.port,
        username: conn.username,
      });
      if (!shown) status.show("The SSH panel is not available.", "warn");
    }

    // ---- boot ---------------------------------------------------------------

    renderShelf();

    // The connection list decides whether a button can be added at all, so it
    // is fetched once and the view redrawn with it. A shelf that is already
    // configured stays pressable either way — the names are saved, and the
    // backend resolves them.
    api.requestJson("/api/ssh/connections").then((data) => {
      if (cancelled) return;
      connections = (data && Array.isArray(data.connections)) ? data.connections : [];
      connectionsError = connections.length ? "" : "Add an SSH connection in Settings first.";
      if (editing === null) renderShelf();
    }).catch(() => {
      if (cancelled) return;
      connections = [];
      connectionsError = "Could not read the saved SSH connections.";
      if (editing === null) renderShelf();
    });

    // Which sessions survived from a previous visit to this tab: the plugin is
    // torn down every time it is closed, but its sessions are not.
    refreshLive();

    // Everything destroy needs, parked on the container so teardown holds no
    // reference of its own (same shape as the Vibration Monitor's).
    containerEl._sshlDestroy = function () {
      cancelled = true;
      stopPolling();
      containerEl._sshlDestroy = null;
    };
  }

  function destroy(containerEl) {
    if (!containerEl) return;
    if (typeof containerEl._sshlDestroy === "function") containerEl._sshlDestroy();
  }

  return {
    init, destroy,
    previewLine, remoteLine, resultSummary, normalizeButtons, coerceButton,
    coerceMode, sessionName,
    MAX_BUTTONS, LIVE_POLL_MS, MODE_TERMINAL, MODE_BACKGROUND,
  };
})();

// Registered as this script runs. The grid re-renders on every register(), so
// the card appears whether the TOOLS tab is already open or not.
if (window.Corvus && Corvus.plugins && typeof Corvus.plugins.register === "function") {
  Corvus.plugins.register("ssh-launcher", {
    name: "SSH Launcher",
    icon: "rocket",
    description: "One-press buttons that start programs on a companion computer",
    init: function (containerEl, api) { Corvus.pluginSshLauncher.init(containerEl, api); },
    destroy: function (containerEl) { Corvus.pluginSshLauncher.destroy(containerEl); },
  });
}
