"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.settingsMcp: the MCP server's part of Settings, Connections.

  On the page, one card: the server switch, who is connected, how many
  permissions are on, and two buttons into dialogs. Permissions holds one
  switch per kind of act with "ask me first" on the ones that change
  something, the fleet switch and the plugins that offer tools. Connect an
  AI client holds the address, the token and a block to paste into
  OpenCode, and is offered only while the server is on.

  Everything applies at once and persists in the backend (POST
  /api/mcp/settings), like the rest of the page. A refused write snaps the
  switch back. The token is shown only on this computer; GET /api/mcp sends
  it nowhere else.
*/
Corvus.settingsMcp = (function () {
  const MASK = "•".repeat(24);

  function post(url, body) {
    return Corvus.telemetry.requestJson(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body || {}),
    });
  }

  /** Pure: the OpenCode config block for an address and a token. */
  function opencodeConfig(url, token) {
    return JSON.stringify({
      $schema: "https://opencode.ai/config.json",
      mcp: {
        corvus: {
          type: "remote",
          url,
          enabled: true,
          oauth: false,
          // Above the time Corvus waits for the operator to allow a call.
          timeout: 60000,
          headers: { Authorization: `Bearer ${token}` },
        },
      },
    }, null, 2);
  }

  /** Pure: how many permissions are on, and how many of those ask first. */
  function permissionSummary(settings, catalog) {
    const s = settings || {};
    const perms = s.permissions || {};
    const confirm = s.confirm || {};
    const list = catalog || [];
    const on = list.filter((p) => perms[p.id]);
    const asking = on.filter((p) => p.writes && confirm[p.id]).length;
    const base = `${on.length} of ${list.length} permissions on`;
    return asking ? `${base}, ${asking} of them ask you first.` : `${base}.`;
  }

  /** Pure: one line about who is connected. */
  function clientsLine(status) {
    const s = status || {};
    if (!s.enabled) return "Off. Nothing can connect.";
    const live = (s.clients || []).filter((c) => c.seen_s <= 120);
    if (!live.length) return "On. No AI client connected yet.";
    return "Connected: " + live.map((c) => {
      const name = c.version ? `${c.name} ${c.version}` : c.name;
      return `${name} (${Math.round(c.seen_s)} s ago)`;
    }).join(", ");
  }

  function copyText(text, note) {
    const done = (ok) => {
      if (!note) return;
      note.textContent = ok ? "Copied to the clipboard." : "Clipboard unavailable. Select the text and copy it.";
      note.classList.toggle("err", !ok);
    };
    try {
      navigator.clipboard.writeText(text).then(() => done(true), () => done(false));
    } catch (_e) {
      done(false);
    }
  }

  function switchRow(label, hint, value, onChange, className) {
    const sw = Corvus.ui.toggle({ value, ariaLabel: label, onChange });
    const row = Corvus.ui.field({
      label, control: sw.el, hint,
      className: "field-switch" + (className ? " " + className : ""),
    });
    return { row, sw };
  }

  /**
   * The whole MCP part of the Connections pill.
   * @returns {HTMLElement}
   */
  function section() {
    const root = document.createElement("div");
    root.className = "mcp-settings";
    const loading = Corvus.ui.card({ body: "Reading the MCP settings…" });
    root.appendChild(Corvus.ui.section({ title: "MCP server", body: loading }));

    Corvus.telemetry.requestJson("/api/mcp").then((data) => {
      Corvus.ui.clear(root);
      build(root, data || {});
    }).catch((err) => {
      Corvus.ui.clear(root);
      root.appendChild(Corvus.ui.section({
        title: "MCP server",
        body: Corvus.ui.card({ body: `Could not read the MCP settings: ${(err && err.message) || err}` }),
      }));
    });
    return root;
  }

  function build(root, data) {
    let settings = data.settings || { permissions: {}, confirm: {}, plugins: {} };
    let token = data.token || "";
    const catalog = data.catalog || [];
    const save = (patch) => post("/api/mcp/settings", patch).then((res) => {
      if (!res || res.ok === false) throw new Error((res && res.error) || "Not saved.");
      settings = res.settings || settings;
      if (res.status && Corvus.mcp) Corvus.mcp.setStatus(res.status);
      paintServer();
      return res;
    });

    // The page holds the switch and the way into the two dialogs; the
    // permissions and the setup are read once and then left alone, so they
    // do not belong in the column of cards the operator scrolls through.
    const server = Corvus.ui.card({});
    server.appendChild(switchRow("MCP server",
      "Lets an AI client such as OpenCode read the vehicle and, as far as you allow " +
      "it, fly it. Only this computer can connect, and only with its token.",
      !!settings.enabled, (next) => save({ enabled: next }).then(() => {
        if (next && Corvus.mcp) Corvus.mcp.announce();
      })).row);
    const who = document.createElement("div");
    who.className = "mcp-clients";
    const summary = document.createElement("div");
    summary.className = "mcp-summary";
    server.append(who, summary);
    const permsBtn = Corvus.ui.button({
      label: "Permissions", icon: "shield", size: "sm",
      onClick: () => openPermissions(),
    });
    const connectBtn = Corvus.ui.button({
      label: "Connect an AI client", icon: "plug", size: "sm",
      onClick: () => openConnect(),
    });
    server.appendChild(Corvus.ui.actions([permsBtn, connectBtn]));
    root.appendChild(Corvus.ui.section({ title: "MCP server", body: server }));
    Corvus.ui.refreshIcons();

    function paintServer() {
      // Nothing can connect while it is off, so there is nothing to set up yet.
      connectBtn.hidden = !settings.enabled;
      summary.textContent = permissionSummary(settings, catalog);
    }
    paintServer();

    // ---- the permissions dialog ------------------------------------------
    function openPermissions() {
      const body = document.createElement("div");
      body.className = "mcp-dialog";
      body.appendChild(switchRow("Other vehicles of the fleet",
        "Off, the AI client acts only on the vehicle you fly. On, it can name any " +
        "vehicle Corvus hears.",
        !!settings.fleet, (next) => save({ fleet: next })).row);
      const intro = document.createElement("p");
      intro.className = "mcp-intro";
      intro.textContent = "A tool that is off is not even offered to the client. With Ask me " +
        "first, Corvus shows you each request and sends nothing until you allow it. " +
        `Direct control lasts at most ${data.control_max_s || 5} s per request; ` +
        "without a new one the vehicle holds.";
      body.appendChild(intro);
      const askRows = [];
      catalog.forEach((p) => {
        const main = switchRow(p.label, "", !!settings.permissions[p.id],
          (next) => save({ permissions: { [p.id]: next } }).then(paintAsk), "mcp-perm");
        main.row.appendChild(detail(p.hint, p.tools));
        body.appendChild(main.row);
        if (p.writes) {
          const ask = switchRow("Ask me first", "", !!settings.confirm[p.id],
            (next) => save({ confirm: { [p.id]: next } }), "mcp-ask");
          body.appendChild(ask.row);
          askRows.push({ id: p.id, row: ask.row });
        }
      });
      function paintAsk() {
        askRows.forEach((a) => { a.row.hidden = !settings.permissions[a.id]; });
      }
      paintAsk();

      const pluginTitle = document.createElement("p");
      pluginTitle.className = "mcp-dialog-title";
      pluginTitle.textContent = "Plugin tools";
      const pluginList = document.createElement("div");
      const pluginNote = document.createElement("p");
      pluginNote.className = "mcp-intro";
      pluginNote.textContent = "Plugin tools also need Plugin tools switched on above.";
      body.append(pluginTitle, pluginList, pluginNote);
      const paintPlugins = (list) => {
        Corvus.ui.clear(pluginList);
        if (!list || !list.length) {
          pluginList.appendChild(Corvus.ui.empty(
            "No plugin offers tools right now. A plugin's tools are listed here while " +
            "Corvus is open, the server is on, and the plugin allows them in its own " +
            "settings (Settings, Plugins)."));
          return;
        }
        list.forEach((p) => {
          const row = switchRow(p.name, "", settings.plugins[p.id] !== false,
            (next) => save({ plugins: { [p.id]: next } })).row;
          row.appendChild(detail("", p.tools));
          pluginList.appendChild(row);
        });
      };
      paintPlugins(data.plugins);
      // The plugins this window offers may have been announced a moment ago.
      const fresh = settings.enabled && Corvus.mcp ? Corvus.mcp.announce() : Promise.resolve();
      fresh.then(() => Corvus.telemetry.requestJson("/api/mcp"))
        .then((d) => { if (d && body.isConnected) paintPlugins(d.plugins); })
        .catch(() => {});

      const handle = Corvus.ui.modal({
        title: "MCP permissions", body, size: "lg",
        actions: [Corvus.ui.button({ label: "Done", variant: "primary", onClick: () => handle.close() })],
      });
      handle.open();
    }

    // ---- the connect dialog ----------------------------------------------
    function openConnect() {
      const body = document.createElement("div");
      body.className = "mcp-dialog";
      let shown = false;
      const note = document.createElement("div");
      note.className = "logs-status mcp-note";

      const urlInput = Corvus.ui.input({ value: data.url || "", mono: true, ariaLabel: "MCP address" });
      urlInput.readOnly = true;
      body.appendChild(Corvus.ui.field({
        label: "Address",
        control: withButtons(urlInput, [Corvus.ui.iconButton("copy", {
          title: "Copy the address", onClick: () => copyText(data.url || "", note),
        })]),
        hint: "The port is the one Corvus runs on. If Corvus has to move to another port, " +
              "the address changes with it.",
      }));

      const tokenInput = Corvus.ui.input({ value: MASK, mono: true, ariaLabel: "MCP token" });
      tokenInput.readOnly = true;
      const eye = Corvus.ui.iconButton("eye", {
        title: "Show the token", onClick: () => { shown = !shown; paintToken(); },
      });
      const tokenRow = Corvus.ui.field({
        label: "Token",
        control: withButtons(tokenInput, [
          eye,
          Corvus.ui.iconButton("copy", { title: "Copy the token", onClick: () => copyText(token, note) }),
          Corvus.ui.iconButton("refresh-cw", {
            title: "Make a new token. Clients holding the old one are refused from now on.",
            onClick: () => post("/api/mcp/token", {}).then((res) => {
              token = (res && res.token) || token;
              paintToken();
              note.textContent = "New token made. Give it to your clients again.";
              note.classList.remove("err");
            }).catch((err) => {
              note.textContent = (err && err.message) || "Could not make a new token.";
              note.classList.add("err");
            }),
          }),
        ]),
        hint: "Like a password for the MCP server. It is kept in its own file, never in a " +
              "settings export.",
      });
      body.appendChild(tokenRow);
      if (!data.local) {
        tokenRow.hidden = true;
        body.appendChild(Corvus.ui.empty(
          "The token is shown only on the computer Corvus runs on."));
      }

      const steps = document.createElement("ol");
      steps.className = "mcp-steps";
      [
        "Switch on what the client may do, under Permissions.",
        "Put this in opencode.json in your project folder, or in " +
          "~/.config/opencode/opencode.json for every project:",
      ].forEach((t) => {
        const li = document.createElement("li");
        li.textContent = t;
        steps.appendChild(li);
      });
      const code = document.createElement("pre");
      code.className = "mcp-code";
      code.setAttribute("translate", "no");
      const codeCopy = Corvus.ui.button({
        label: "Copy for OpenCode", icon: "copy", size: "sm",
        onClick: () => copyText(opencodeConfig(data.url || "", token), note),
      });
      const after = document.createElement("ol");
      after.className = "mcp-steps";
      after.start = 3;
      const li = document.createElement("li");
      li.textContent = "Start OpenCode in that folder and ask, for example, what the battery " +
        "says. Run /mcp in OpenCode to see that corvus is connected.";
      after.appendChild(li);
      const other = document.createElement("p");
      other.className = "mcp-intro";
      other.textContent = "Any other client that speaks MCP over HTTP (Streamable HTTP) " +
        "takes the same address with the header Authorization: Bearer and the token.";
      const limit = document.createElement("p");
      limit.className = "mcp-intro mcp-limit";
      limit.textContent = "These switches decide what the client may do through MCP. A " +
        "client that may also run commands on this computer could reach Corvus another " +
        "way, so allow those commands only where you trust it.";
      body.append(steps, code, Corvus.ui.actions([codeCopy]), after, other, limit, note);

      function paintToken() {
        tokenInput.value = shown ? token : MASK;
        eye.title = shown ? "Hide the token" : "Show the token";
        code.textContent = opencodeConfig(data.url || "", shown ? token : "<token>");
      }
      paintToken();

      const handle = Corvus.ui.modal({
        title: "Connect an AI client", body, size: "lg",
        actions: [Corvus.ui.button({ label: "Done", variant: "primary", onClick: () => handle.close() })],
      });
      handle.open();
    }

    // Kept current from the event stream for as long as the card is on the page.
    let seen = false;
    const off = Corvus.mcp ? Corvus.mcp.subscribe((status) => {
      if (root.isConnected) seen = true;
      else if (seen) { off(); return; }
      who.textContent = clientsLine(status || data.status);
    }) : null;
    if (!off) who.textContent = clientsLine(data.status);
  }

  /** A permission's hint and the tools it opens, under its switch. */
  function detail(hint, tools) {
    const extra = document.createElement("div");
    extra.className = "mcp-perm-detail";
    if (hint) {
      const h = document.createElement("span");
      h.textContent = hint;
      extra.appendChild(h);
    }
    if (tools && tools.length) {
      const t = document.createElement("span");
      t.className = "mcp-tools";
      t.setAttribute("translate", "no");
      t.textContent = tools.join(", ");
      extra.appendChild(t);
    }
    return extra;
  }

  function withButtons(inputEl, buttons) {
    const wrap = document.createElement("div");
    wrap.className = "mcp-inline";
    wrap.appendChild(inputEl);
    buttons.forEach((b) => wrap.appendChild(b));
    return wrap;
  }

  return { section, opencodeConfig, clientsLine, permissionSummary };
})();
