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
 *   TERMINAL (the default)  Types `cd -- <folder> && <command>` into an
 *     interactive SSH session of the button's own, opening one the first time.
 *     Pressing the button again types it again into the SAME shell — one run
 *     under the last, in one scrollback — because a key on a shelf pressed
 *     twice means "run it again", not "throw this session away and start
 *     over". Three buttons are three sessions, so three terminals can be open
 *     side by side.
 *
 *     The terminal window itself opens on the ARROW beside the button, never
 *     on the button: a launch is a launch, and an operator starting four
 *     programs on the pad does not want four windows thrown at them. The
 *     program's output is theirs to read there and Ctrl-C theirs to press —
 *     this is a real shell, not a captured one. Closing the window leaves the
 *     program running; the window's disconnect button, like Ctrl-C, stops it.
 *     The session lives in the backend's SSH bridge for as long as Corvus runs.
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
 * That name no longer has to exist first. The editor's connection list ends in
 * "New connection…", which opens the host/user/password form inline and, on
 * Save, POSTs it to /api/ssh/connections before the button that names it —
 * because a shelf is built on the pad as often as at a desk, and requiring a
 * trip to Settings first made the plugin useless exactly when it was most
 * wanted. The credentials land in the app's own SSH connection store (the same
 * one the SSH tab reads, so the connection is visible, reusable and removable
 * there); what this plugin saves is still only the name.
 *
 * The shelf is the plugin's state: a list of {id, label, connection, directory,
 * command, mode}, saved through api.saveSettings, so the buttons are there on
 * the next start. Nothing secret goes in there — a typed-in password is in the
 * form for as long as the editor is open and is never written to these
 * settings.
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

  // The connection <select>'s last entry: not a connection but a way to type
  // one in. A button used to require a connection saved in Settings first,
  // which made the shelf unusable exactly when it is most wanted — a new
  // companion computer on the pad, nothing configured yet.
  const NEW_CONNECTION = "__new__";

  /** The blank ad-hoc connection form. */
  function blankConnection() {
    return { name: "", host: "", port: "22", username: "", password: "", key_path: "" };
  }

  /**
   * The name an ad-hoc connection is saved under when the operator types none:
   * user@host, the way they would have written it on the command line anyway.
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
   * Why an ad-hoc connection cannot be saved yet, or "" when it can.
   *
   * The name check is the one that matters: saving under a name that is
   * already taken would REPLACE that connection's credentials — the launcher
   * would silently repoint an SSH card the operator relies on at another
   * machine. So a collision is refused here rather than resolved.
   *
   * Pure, and exported for the test suite.
   *
   * @param {Object} conn the connection form
   * @param {string[]} taken the names already saved
   * @returns {string} the reason, or "" when the form is good
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
   * Pure, and exported for the test suite — the shape of this request is the
   * whole contract between the plugin and the credential store.
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
      // Not trimmed: a password may legitimately begin or end with a space,
      // and this one is going straight to the backend's store.
      password: String(c.password == null ? "" : c.password),
      key_path: trim(c.key_path),
    };
  }

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

      // Not an error any more, just a hint: a button can carry a connection
      // that was never saved in Settings.
      if (connectionsError) {
        const note = ui.empty(connectionsError);
        // add, not assign: `className =` dropped the muted description styling
        // ui.empty() had just put there, and the hint came out as body text.
        note.classList.add("sshl-note");
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
        // Always "run it": pressing a button whose session is already open
        // types the line into that same shell rather than replacing it.
        icon: "play",
        label: entry.label,
        className: "sshl-launch",
        ariaLabel: running ? `Run ${entry.label} again` : `Launch ${entry.label}`,
        // The composed line as a tooltip: the row shows the label, and the
        // label is often nothing like the command it stands for.
        title: running
          ? `Run it again in the terminal it is already in.\n${previewLine(entry) || entry.command}`
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

      // The arrow back to this button's own terminal window. Launching opens
      // it; this is how it is fetched again after it was closed or buried.
      // Only terminal-mode buttons have one to go to, and only while the
      // session is actually up — an arrow that led to a dead terminal would be
      // worse than no arrow, so it says why it is off instead.
      if (entry.mode !== MODE_BACKGROUND) {
        const openBtn = ui.iconButton("chevron-right", {
          className: "icon-btn sshl-open",
          ariaLabel: `Open the terminal for ${entry.label}`,
          title: running
            ? "Open its terminal window — watch it, or press Ctrl-C to stop it"
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

      // The connection form the operator fills in when they pick "New
      // connection…". Held here rather than on the draft: it carries a
      // password, and the draft is what gets written to the config file.
      const newConn = blankConnection();
      let nameInput = null;

      const connSel = ui.select({
        ariaLabel: "SSH connection",
        options: connectionOptions(),
        // With nothing saved there is nothing to pick, so the editor opens on
        // the form instead of on an empty list.
        value: connections.length ? draft.connection : NEW_CONNECTION,
        onChange: () => { renderNewConnection(); paintPreview(); },
      });
      card.appendChild(ui.field({
        label: "Connection",
        control: connSel,
        hint: "One of the saved SSH connections, or a new one typed in below. " +
              "Either way the password is kept by the backend — this plugin " +
              "only ever names the connection.",
      }));
      if (connSel.value !== NEW_CONNECTION) draft.connection = connSel.value || "";

      const newConnEl = document.createElement("div");
      newConnEl.className = "sshl-newconn";
      card.appendChild(newConnEl);

      /** Whether the operator is typing a connection rather than picking one. */
      function pickedNew() { return connSel.value === NEW_CONNECTION; }

      /** The connection name this button will end up carrying. */
      function targetName() {
        if (!pickedNew()) return connSel.value || "";
        return String(newConn.name || "").trim() || derivedName(newConn);
      }

      /* The fields of an ad-hoc connection, in the order they are answered:
         where it is, then who logs in, then how — and the name it is filed
         under last, because it writes itself from the two above. */
      const NEW_FIELDS = [
        { key: "host", label: "Host", placeholder: "192.168.2.10", mono: true },
        { key: "port", label: "Port", type: "number", mono: true },
        { key: "username", label: "User", placeholder: "corvus" },
        { key: "password", label: "Password", type: "password", mono: true,
          placeholder: "optional — or a key file" },
        { key: "key_path", label: "Key file", mono: true,
          placeholder: "/home/you/.ssh/id_rsa" },
        { key: "name", label: "Save as", placeholder: "pilot@10.0.0.7",
          hint: "The name this connection is saved under. Leave it empty to " +
                "use user@host. It joins the SSH connections in Settings, so " +
                "the next button can simply pick it." },
      ];

      /* Built only while it is in use, and thrown away when a saved connection
         is picked instead: a password field that is merely hidden is still a
         password field in the page. */
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
          newConnEl.appendChild(ui.field({ label: f.label, control, hint: f.hint }));
        });
      }
      renderNewConnection();

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
        hint: "On: the program runs in an SSH session of this button's own, " +
              "and pressing the button again runs it again in that same " +
              "terminal. The arrow beside the button opens the window to watch " +
              "it in — read its output there, and stop it with Ctrl-C or the " +
              "window's disconnect button. The session lasts as long as Corvus " +
              "does. Off: the program is started with nohup and detached, so " +
              "it survives Corvus closing — but there is nothing to watch and " +
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
          if (pickedNew()) saveNewConnectionThenButton();
          else { draft.connection = connSel.value || ""; commit(); }
        },
      });

      /** Put the finished draft on the shelf and go back to it. */
      function commit() {
        const entry = coerceButton(draft);
        if (!entry) return;                        // guarded by paintPreview anyway
        if (isNew) buttons.push(entry);
        else buttons = buttons.map((b) => (b.id === entry.id ? entry : b));
        persist();
        renderShelf();
      }

      /**
       * Save the typed-in connection first, then the button that names it.
       *
       * In that order because a button carries a NAME: saving it against a
       * connection the backend does not have yet would leave a button that
       * cannot run. If the save fails, nothing is added and the form stays up
       * with the reason — the password is still in it, so the operator fixes a
       * typo rather than typing it all again.
       */
      function saveNewConnectionThenButton() {
        const body = newConnectionBody(newConn);
        ui.setBusy(saveBtn, true);
        api.postJson("/api/ssh/connections", body).then((res) => {
          if (cancelled) return;
          if (!(res && res.ok)) {
            ui.setBusy(saveBtn, false);
            status.show((res && res.error) || "The connection could not be saved.", "err");
            return;
          }
          // The endpoint answers with the redacted list, so the shelf's own
          // copy is up to date without a second request — and the next button
          // can pick this connection from the select.
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
      const cancelBtn = ui.button({
        variant: "ghost",
        label: "Cancel",
        onClick: () => renderShelf(),
      });
      card.appendChild(ui.actions([saveBtn, cancelBtn]));

      function paintPreview() {
        const target = targetName();
        const line = previewLine(Object.assign({}, draft, { connection: target }));
        preview.textContent = line || "Name a program to see the command.";
        preview.classList.toggle("sshl-preview-empty", !line);

        // The name writes itself from user and host until the operator types
        // one, so the placeholder has to keep up with what they are entering.
        if (nameInput) nameInput.placeholder = derivedName(newConn) || "pilot@10.0.0.7";

        // Nothing to save until there is a command AND somewhere to run it —
        // and, for a typed-in connection, enough of one to save.
        const connError = pickedNew()
          ? newConnectionError(newConn, connections.map((c) => c.name))
          : (target ? "" : "Pick a connection to run it on.");
        saveBtn.disabled = !line || !!connError;
        saveBtn.title = connError || (line ? "" : "Name a program first.");
        // A name that is already taken is the one refusal the operator cannot
        // see for themselves — everything else is a field they can look at.
        if (connError && connError.indexOf("already saved") >= 0) status.show(connError, "warn");
        else status.hide();
      }
      paintPreview();

      editorEl.appendChild(card);
      ui.refreshIcons();
    }

    /** Options for the connection <select>: the saved ones, then the way to
     *  type one that was never saved. The last entry is always there — a shelf
     *  is built on the pad as often as at a desk. */
    function connectionOptions() {
      const saved = connections.map((c) => {
        const address = c.username ? `${c.username}@${c.host}` : String(c.host || "");
        // A connection typed in here is named user@host by default, so spelling
        // the address out again would read "pilot@10.0.0.7 — pilot@10.0.0.7".
        return {
          value: c.name,
          label: (address && address !== c.name) ? `${c.name} — ${address}` : c.name,
        };
      });
      saved.push({ value: NEW_CONNECTION, label: "New connection…" });
      return saved;
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
     * TERMINAL mode: type the line into the button's OWN session, and open one
     * only when it has none.
     *
     * Send first, connect only if that fails. The button is a key on a shelf,
     * so pressing it twice means "run it again", and it runs again in the same
     * shell — the same window, one prompt under the last run, the scrollback
     * of the whole session intact. Connecting first every time (what this did)
     * replaced the session on every press: it killed the running program to
     * start it again, and every press cost a fresh shell and a fresh screen.
     *
     * The shell is the shell: if the last program is still in the foreground,
     * the line goes to ITS stdin rather than to a prompt, exactly as it would
     * for someone typing in that terminal. Ctrl-C first, or let it finish.
     *
     * The window is NOT opened here. A launch is a launch; the arrow beside
     * the button is the request to watch it.
     */
    function launchInTerminal(entry) {
      const session = sessionName(entry);
      const line = remoteLine(entry);
      return api.postJson("/api/ssh/send", { name: session, data: line + "\n" })
        .then((res) => {
          if (cancelled) return null;
          // ok:false is the backend saying it has no such live session — the
          // first press, or the shell has since ended.
          if (res && res.ok) return ran(entry, line, false);
          return connectThenSend(entry, line);
        })
        .catch((error) => {
          if (cancelled) return null;
          status.show(error.message || "Could not reach the backend", "err");
          return null;
        });
    }

    /** Open this button's session, then type the line into it. */
    function connectThenSend(entry, line) {
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
          return api.postJson("/api/ssh/send", { name: session, data: line + "\n" })
            .then(() => (cancelled ? null : ran(entry, line, true)));
        });
    }

    /**
     * Record a launch: mark the row live, say so, and log it.
     *
     * `reconnected` says a NEW shell is behind the session name. A terminal
     * window left open on the old one is quietly repointed at it — quietly
     * because the operator pressed a launch button, not the arrow: nothing is
     * opened, raised or focused, but a window that is already there must not
     * keep showing a shell that has stopped printing.
     */
    function ran(entry, line, reconnected) {
      const session = sessionName(entry);
      const was = !!live[session];
      live[session] = true;
      if (reconnected) openTerminal(entry, true, true);
      status.show(was && !reconnected
        ? `${entry.label}: sent again to its terminal — the arrow opens it.`
        : `${entry.label}: running — the arrow opens its terminal.`, "ok");
      // The console is the app's shared record of what was commanded; a
      // program started on a companion computer belongs in it.
      api.console(`ssh-launcher: ${line}`, "success");
      renderShelf();
      return true;
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
        `Remove "${entry.label}"?\n\nIts session will be closed, ` +
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

    /**
     * Show the terminal window a button's program is running in. Raises the
     * window when it is already open, rather than opening a second one.
     *
     * @param {Object} entry the button
     * @param {boolean} [reattach]    the session behind the name is a new shell
     * @param {boolean} [existingOnly] repair an open window, open none — a
     *        launch does this, the arrow does not
     */
    function openTerminal(entry, reattach, existingOnly) {
      const conn = connectionFor(entry);
      const shown = api.terminal({
        name: sessionName(entry),
        // The session key is an id; the header should read the button's name.
        title: entry.label,
        host: conn.host,
        port: conn.port,
        username: conn.username,
      }, { reattach: !!reattach, existingOnly: !!existingOnly });
      // "No window" is the normal answer for a launch, and only a failure when
      // the operator actually asked for one.
      if (!shown && !existingOnly) status.show("The terminal window could not be opened.", "warn");
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
      connectionsError = connections.length ? ""
        : "No saved SSH connections yet — add a button and choose " +
          "\u201CNew connection\u2026\u201D to enter one here.";
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
    coerceMode, sessionName, derivedName, newConnectionError, newConnectionBody,
    MAX_BUTTONS, LIVE_POLL_MS, MODE_TERMINAL, MODE_BACKGROUND, NEW_CONNECTION,
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
